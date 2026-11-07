//! File tools (permission category `Fs`) with checkpoints before every change.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use globset::Glob;
use serde::Deserialize;
use serde_json::{Value, json};
use tool_registry::{
    Effect, PermissionCategory, Tool, ToolContext, ToolError, ToolOutput, ToolSpec,
};

use crate::checkpoint::CheckpointStore;
use crate::util::{exec_err, finish_text, parse, spec};

const MAX_LIST: usize = 500;
const MAX_MATCHES: usize = 200;
const MAX_SEARCH_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// Largest file `fs_read` (without a range) and `fs_edit` load into memory.
const MAX_TEXT_BYTES: u64 = 16 * 1024 * 1024;
const SKIP_DIRS: [&str; 4] = [".git", "node_modules", "target", ".hg"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Read,
    List,
    Search,
    Write,
    Edit,
    Move,
    Delete,
    CheckpointList,
    CheckpointRestore,
}

struct Shared {
    checkpoints: CheckpointStore,
    spill_dir: PathBuf,
}

/// One file tool.
pub struct FsTool {
    spec: ToolSpec,
    op: Op,
    shared: Arc<Shared>,
}

/// Build all file tools; `data_dir` hosts checkpoints and spill files.
#[must_use]
pub fn tools(data_dir: &Path) -> Vec<Arc<dyn Tool>> {
    let shared = Arc::new(Shared {
        checkpoints: CheckpointStore::new(data_dir),
        spill_dir: data_dir.join("spill"),
    });
    let fs = PermissionCategory::Fs;
    let defs: Vec<(Op, ToolSpec)> = vec![
        (
            Op::Read,
            spec(
                "fs_read",
                "Read a text file. Optional range picks lines (1-based). Example: {\"path\":\"a.txt\",\"range\":{\"start\":1,\"end\":40}}",
                json!({"type":"object","properties":{
                    "path":{"type":"string","description":"File path."},
                    "range":{"type":"object","description":"Line range, inclusive.","properties":{
                        "start":{"type":"integer","minimum":1},
                        "end":{"type":"integer","minimum":1}}}
                },"required":["path"]}),
                Effect::Read,
                fs,
            ),
        ),
        (
            Op::List,
            spec(
                "fs_list",
                "List a directory. With glob, walk recursively and match. Example: {\"path\":\".\",\"glob\":\"**/*.rs\"}",
                json!({"type":"object","properties":{
                    "path":{"type":"string","description":"Directory path."},
                    "glob":{"type":"string","description":"Glob such as *.txt or src/**/*.rs."}
                },"required":["path"]}),
                Effect::Read,
                fs,
            ),
        ),
        (
            Op::Search,
            spec(
                "fs_search",
                "Find text in files under a path (case-insensitive). Example: {\"query\":\"TODO\",\"path\":\"src\"}",
                json!({"type":"object","properties":{
                    "query":{"type":"string","description":"Text to find."},
                    "path":{"type":"string","description":"File or directory to search."},
                    "glob":{"type":"string","description":"Only files matching this glob."}
                },"required":["query","path"]}),
                Effect::Read,
                fs,
            ),
        ),
        (
            Op::Write,
            spec(
                "fs_write",
                "Create or overwrite a file with content. A checkpoint is saved first. Example: {\"path\":\"a.txt\",\"content\":\"hi\"}",
                json!({"type":"object","properties":{
                    "path":{"type":"string","description":"File path."},
                    "content":{"type":"string","description":"Full new file content."}
                },"required":["path","content"]}),
                Effect::Write,
                fs,
            ),
        ),
        (
            Op::Edit,
            spec(
                "fs_edit",
                "Replace exact text in a file. old must match once unless replace_all. Example: {\"path\":\"a.txt\",\"old\":\"foo\",\"new\":\"bar\"}",
                json!({"type":"object","properties":{
                    "path":{"type":"string","description":"File path."},
                    "old":{"type":"string","description":"Exact text to replace (non-empty)."},
                    "new":{"type":"string","description":"Replacement text."},
                    "replace_all":{"type":"boolean","description":"Replace every match."}
                },"required":["path","old","new"]}),
                Effect::Write,
                fs,
            ),
        ),
        (
            Op::Move,
            spec(
                "fs_move",
                "Move or rename a file or directory. Fails if the target exists unless overwrite. Example: {\"from\":\"a.txt\",\"to\":\"b.txt\"}",
                json!({"type":"object","properties":{
                    "from":{"type":"string","description":"Existing path."},
                    "to":{"type":"string","description":"New path."},
                    "overwrite":{"type":"boolean","description":"Replace the target if it exists."}
                },"required":["from","to"]}),
                Effect::Write,
                fs,
            ),
        ),
        (
            Op::Delete,
            spec(
                "fs_delete",
                "Delete a file or directory (directories need recursive). A checkpoint is saved first. Example: {\"path\":\"old.txt\"}",
                json!({"type":"object","properties":{
                    "path":{"type":"string","description":"Path to delete."},
                    "recursive":{"type":"boolean","description":"Required to delete a directory."}
                },"required":["path"]}),
                Effect::Destructive,
                fs,
            ),
        ),
        (
            Op::CheckpointList,
            spec(
                "checkpoint_list",
                "List file checkpoints of a run (default: current run). Example: {}",
                json!({"type":"object","properties":{
                    "run":{"type":"string","description":"Run id; default is the current run."}
                }}),
                Effect::Read,
                fs,
            ),
        ),
        (
            Op::CheckpointRestore,
            spec(
                "checkpoint_restore",
                "Restore files to a checkpoint (from checkpoint_list). Example: {\"id\":\"cp-0192...\"}",
                json!({"type":"object","properties":{
                    "id":{"type":"string","description":"Checkpoint id."}
                },"required":["id"]}),
                Effect::Write,
                fs,
            ),
        ),
    ];
    defs.into_iter()
        .map(|(op, spec)| {
            Arc::new(FsTool {
                spec,
                op,
                shared: shared.clone(),
            }) as Arc<dyn Tool>
        })
        .collect()
}

#[derive(Deserialize)]
struct Range {
    start: Option<usize>,
    end: Option<usize>,
}

#[derive(Deserialize)]
struct ReadIn {
    path: String,
    range: Option<Range>,
}

#[derive(Deserialize)]
struct ListIn {
    path: String,
    glob: Option<String>,
}

#[derive(Deserialize)]
struct SearchIn {
    query: String,
    path: String,
    glob: Option<String>,
}

#[derive(Deserialize)]
struct WriteIn {
    path: String,
    content: String,
}

#[derive(Deserialize)]
struct EditIn {
    path: String,
    old: String,
    new: String,
    #[serde(default)]
    replace_all: bool,
}

#[derive(Deserialize)]
struct MoveIn {
    from: String,
    to: String,
    #[serde(default)]
    overwrite: bool,
}

#[derive(Deserialize)]
struct DeleteIn {
    path: String,
    #[serde(default)]
    recursive: bool,
}

#[derive(Deserialize)]
struct CpListIn {
    run: Option<String>,
}

#[derive(Deserialize)]
struct CpRestoreIn {
    id: String,
}

fn read_text(path: &Path) -> Result<String, ToolError> {
    use std::io::Read as _;
    let too_big = || {
        exec_err(format!(
            "{} is larger than {} MiB; read a line range instead",
            path.display(),
            MAX_TEXT_BYTES / (1024 * 1024)
        ))
    };
    let f = fs::File::open(path).map_err(|e| exec_err(format!("read {}: {e}", path.display())))?;
    if f.metadata().is_ok_and(|m| m.len() > MAX_TEXT_BYTES) {
        return Err(too_big());
    }
    // Bound the read even if the file grows (or reports no size, e.g. a pipe).
    let mut bytes = Vec::new();
    f.take(MAX_TEXT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| exec_err(format!("read {}: {e}", path.display())))?;
    if bytes.len() as u64 > MAX_TEXT_BYTES {
        return Err(too_big());
    }
    if bytes.iter().take(8192).any(|b| *b == 0) {
        return Err(exec_err(format!("{} is a binary file", path.display())));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Stream a file once, keeping only lines `start..=end` (1-based) in memory.
/// Returns the kept lines and the total line count.
fn read_line_range(
    path: &Path,
    start: usize,
    end: usize,
) -> Result<(Vec<String>, usize), ToolError> {
    use std::io::{BufRead as _, Read as _};
    let f = fs::File::open(path).map_err(|e| exec_err(format!("read {}: {e}", path.display())))?;
    let mut r = std::io::BufReader::new(f);
    let mut head = vec![0u8; 8192];
    let n = {
        let buf = r
            .fill_buf()
            .map_err(|e| exec_err(format!("read {}: {e}", path.display())))?;
        let n = buf.len().min(head.len());
        head[..n].copy_from_slice(&buf[..n]);
        n
    };
    if head[..n].contains(&0) {
        return Err(exec_err(format!("{} is a binary file", path.display())));
    }
    let too_big = |what: &str| {
        exec_err(format!(
            "{} {what} exceeds {} MiB; request a narrower line range",
            path.display(),
            MAX_TEXT_BYTES / (1024 * 1024)
        ))
    };
    let mut kept = Vec::new();
    let mut kept_bytes = 0u64;
    let mut total = 0usize;
    let mut line = Vec::new();
    loop {
        line.clear();
        // Cap a single line so a newline-free huge file cannot exhaust memory.
        // A line that hits the cap is an error: splitting it would silently
        // corrupt the line numbers.
        let got = r
            .by_ref()
            .take(MAX_TEXT_BYTES + 1)
            .read_until(b'\n', &mut line)
            .map_err(|e| exec_err(format!("read {}: {e}", path.display())))?;
        if got == 0 {
            break;
        }
        if got as u64 > MAX_TEXT_BYTES {
            return Err(too_big("line"));
        }
        total += 1;
        if total >= start && total <= end {
            // Strip exactly one line terminator (`\n` or `\r\n`), like `str::lines`.
            if line.last() == Some(&b'\n') {
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
            }
            // Total cap: an open or wide range must not load a huge file.
            kept_bytes += line.len() as u64 + 1;
            if kept_bytes > MAX_TEXT_BYTES {
                return Err(too_big("range"));
            }
            kept.push(String::from_utf8_lossy(&line).into_owned());
        }
    }
    Ok((kept, total))
}

fn do_read(i: &ReadIn) -> Result<String, ToolError> {
    let Some(r) = &i.range else {
        return read_text(Path::new(&i.path));
    };
    let start = r.start.unwrap_or(1).max(1);
    let end = r.end.unwrap_or(usize::MAX);
    let (kept, total) = read_line_range(Path::new(&i.path), start, end)?;
    let end = end.min(total);
    if start > end {
        return Err(exec_err(format!(
            "range starts after the end; file has {total} lines"
        )));
    }
    let mut out = format!("# {} lines {start}-{end} of {total}\n", i.path);
    out.push_str(&kept.join("\n"));
    Ok(out)
}

fn matcher(glob: Option<&str>) -> Result<Option<(globset::GlobMatcher, bool)>, ToolError> {
    glob.map(|g| {
        let m = Glob::new(g)
            .map_err(|e| ToolError::InvalidInput(format!("bad glob: {e}")))?
            .compile_matcher();
        Ok((m, !g.contains('/')))
    })
    .transpose()
}

/// Walk `root` recursively (depth-limited), calling `f(path, is_dir)`.
/// Returning `false` stops the walk.
fn walk(root: &Path, depth: usize, f: &mut dyn FnMut(&Path, bool) -> bool) -> bool {
    let Ok(rd) = fs::read_dir(root) else {
        return true;
    };
    let mut entries: Vec<_> = rd.filter_map(Result::ok).collect();
    entries.sort_by_key(fs::DirEntry::file_name);
    for e in entries {
        let p = e.path();
        let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
        if is_dir && SKIP_DIRS.iter().any(|s| e.file_name() == *s) {
            continue;
        }
        if !f(&p, is_dir) {
            return false;
        }
        if is_dir && depth > 0 && !walk(&p, depth - 1, f) {
            return false;
        }
    }
    true
}

fn glob_hit(m: &(globset::GlobMatcher, bool), root: &Path, p: &Path) -> bool {
    let (matcher, by_name) = m;
    if *by_name {
        p.file_name().is_some_and(|n| matcher.is_match(n))
    } else {
        let rel = p.strip_prefix(root).unwrap_or(p);
        matcher.is_match(rel)
    }
}

fn do_list(i: &ListIn) -> Result<String, ToolError> {
    let root = Path::new(&i.path);
    if !root.is_dir() {
        return Err(exec_err(format!("{} is not a directory", i.path)));
    }
    let m = matcher(i.glob.as_deref())?;
    let mut lines = Vec::new();
    let mut truncated = false;
    let mut push = |p: &Path, is_dir: bool| -> bool {
        if lines.len() >= MAX_LIST {
            truncated = true;
            return false;
        }
        let rel = p.strip_prefix(root).unwrap_or(p).display().to_string();
        if is_dir {
            lines.push(format!("d {rel}/"));
        } else {
            let size = fs::metadata(p).map_or(0, |m| m.len());
            lines.push(format!("f {rel} ({size} bytes)"));
        }
        true
    };
    match &m {
        None => {
            walk(root, 0, &mut |p, d| push(p, d));
        }
        Some(m) => {
            walk(root, 10, &mut |p, d| {
                if glob_hit(m, root, p) {
                    push(p, d)
                } else {
                    true
                }
            });
        }
    }
    let mut out = lines.join("\n");
    if out.is_empty() {
        out = "(no entries)".into();
    }
    if truncated {
        let _ = write!(out, "\n…[first {MAX_LIST} entries only]");
    }
    Ok(out)
}

fn search_file(path: &Path, q: &str, root: &Path, out: &mut Vec<String>) {
    let Ok(meta) = fs::metadata(path) else { return };
    if meta.len() > MAX_SEARCH_FILE_BYTES {
        return;
    }
    let Ok(bytes) = fs::read(path) else { return };
    if bytes.iter().take(8192).any(|b| *b == 0) {
        return;
    }
    let Ok(text) = String::from_utf8(bytes) else {
        return;
    };
    let rel = path
        .strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string();
    for (n, line) in text.lines().enumerate() {
        if out.len() >= MAX_MATCHES {
            return;
        }
        if line.to_lowercase().contains(q) {
            let shown: String = line.trim().chars().take(200).collect();
            out.push(format!("{rel}:{}: {shown}", n + 1));
        }
    }
}

fn do_search(i: &SearchIn) -> Result<String, ToolError> {
    if i.query.is_empty() {
        return Err(ToolError::InvalidInput("query is empty".into()));
    }
    let q = i.query.to_lowercase();
    let root = Path::new(&i.path);
    let m = matcher(i.glob.as_deref())?;
    let mut out = Vec::new();
    if root.is_file() {
        let parent = root.parent().unwrap_or(root);
        search_file(root, &q, parent, &mut out);
    } else if root.is_dir() {
        walk(root, 12, &mut |p, is_dir| {
            if !is_dir && m.as_ref().is_none_or(|m| glob_hit(m, root, p)) {
                search_file(p, &q, root, &mut out);
            }
            out.len() < MAX_MATCHES
        });
    } else {
        return Err(exec_err(format!("{} does not exist", i.path)));
    }
    if out.is_empty() {
        return Ok("no matches".into());
    }
    let mut s = out.join("\n");
    if out.len() >= MAX_MATCHES {
        let _ = write!(s, "\n…[first {MAX_MATCHES} matches only]");
    }
    Ok(s)
}

impl Shared {
    fn checkpoint(&self, run: &str, op: &str, paths: &[&Path]) -> Result<String, ToolError> {
        self.checkpoints
            .create(run, op, paths)
            .map_err(|e| exec_err(format!("checkpoint failed, nothing changed: {e}")))
    }
}

fn do_write(sh: &Shared, run: &str, i: &WriteIn) -> Result<String, ToolError> {
    let p = Path::new(&i.path);
    let cp = sh.checkpoint(run, "fs_write", &[p])?;
    if let Some(parent) = p.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(|e| exec_err(format!("create dirs: {e}")))?;
    }
    fs::write(p, i.content.as_bytes()).map_err(|e| exec_err(format!("write {}: {e}", i.path)))?;
    Ok(format!(
        "wrote {} bytes to {} (checkpoint {cp})",
        i.content.len(),
        i.path
    ))
}

fn do_edit(sh: &Shared, run: &str, i: &EditIn) -> Result<String, ToolError> {
    if i.old.is_empty() {
        return Err(ToolError::InvalidInput("old must not be empty".into()));
    }
    let p = Path::new(&i.path);
    let text = read_text(p)?;
    let n = text.matches(&i.old).count();
    if n == 0 {
        return Err(exec_err("old text not found; read the file again"));
    }
    if n > 1 && !i.replace_all {
        return Err(exec_err(format!(
            "old text matches {n} times; add more context or set replace_all"
        )));
    }
    let cp = sh.checkpoint(run, "fs_edit", &[p])?;
    let updated = text.replace(&i.old, &i.new);
    fs::write(p, updated).map_err(|e| exec_err(format!("write {}: {e}", i.path)))?;
    Ok(format!(
        "replaced {n} occurrence(s) in {} (checkpoint {cp})",
        i.path
    ))
}

fn do_move(sh: &Shared, run: &str, i: &MoveIn) -> Result<String, ToolError> {
    let (from, to) = (Path::new(&i.from), Path::new(&i.to));
    if fs::symlink_metadata(from).is_err() {
        return Err(exec_err(format!("{} does not exist", i.from)));
    }
    if to.exists() && !i.overwrite {
        return Err(exec_err(format!(
            "{} already exists; set overwrite to replace it",
            i.to
        )));
    }
    let cp = sh.checkpoint(run, "fs_move", &[from, to])?;
    if let Some(parent) = to.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(|e| exec_err(format!("create dirs: {e}")))?;
    }
    if to.exists() {
        let r = if to.is_dir() {
            fs::remove_dir_all(to)
        } else {
            fs::remove_file(to)
        };
        r.map_err(|e| exec_err(format!("remove target: {e}")))?;
    }
    fs::rename(from, to).map_err(|e| exec_err(format!("move failed: {e}")))?;
    Ok(format!("moved {} to {} (checkpoint {cp})", i.from, i.to))
}

fn do_delete(sh: &Shared, run: &str, i: &DeleteIn) -> Result<String, ToolError> {
    let p = Path::new(&i.path);
    let meta = fs::symlink_metadata(p).map_err(|e| exec_err(format!("{}: {e}", i.path)))?;
    if meta.is_dir() && !i.recursive {
        return Err(exec_err(format!(
            "{} is a directory; set recursive to delete it",
            i.path
        )));
    }
    let cp = sh.checkpoint(run, "fs_delete", &[p])?;
    let r = if meta.is_dir() {
        fs::remove_dir_all(p)
    } else {
        fs::remove_file(p)
    };
    r.map_err(|e| exec_err(format!("delete {}: {e}", i.path)))?;
    Ok(format!("deleted {} (checkpoint {cp})", i.path))
}

fn do_cp_list(sh: &Shared, run: &str) -> Result<String, ToolError> {
    let list = sh
        .checkpoints
        .list(run)
        .map_err(|e| exec_err(format!("list checkpoints: {e}")))?;
    if list.is_empty() {
        return Ok("no checkpoints".into());
    }
    let mut out = String::new();
    for c in list {
        let paths: Vec<&str> = c.entries.iter().map(|e| e.path.as_str()).collect();
        let _ = writeln!(out, "{} {} {}", c.id, c.op, paths.join(", "));
    }
    Ok(out)
}

fn do_cp_restore(sh: &Shared, id: &str) -> Result<String, ToolError> {
    let (paths, safety, warnings) = sh
        .checkpoints
        .restore_with_warnings(id)
        .map_err(|e| exec_err(format!("restore failed: {e}")))?;
    let mut out = format!(
        "restored {} path(s): {} (previous state saved as {safety})",
        paths.len(),
        paths.join(", ")
    );
    for w in warnings {
        let _ = write!(out, "\nwarning: {w}");
    }
    Ok(out)
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, ToolError> + Send + 'static,
) -> Result<T, ToolError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| exec_err(format!("task failed: {e}")))?
}

#[async_trait]
impl Tool for FsTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolContext, input: Value) -> Result<ToolOutput, ToolError> {
        let sh = self.shared.clone();
        let run = ctx.run_id.clone();
        let text = match self.op {
            Op::Read => {
                let i: ReadIn = parse(input)?;
                blocking(move || do_read(&i)).await?
            }
            Op::List => {
                let i: ListIn = parse(input)?;
                blocking(move || do_list(&i)).await?
            }
            Op::Search => {
                let i: SearchIn = parse(input)?;
                blocking(move || do_search(&i)).await?
            }
            Op::Write => {
                let i: WriteIn = parse(input)?;
                blocking(move || do_write(&sh, &run, &i)).await?
            }
            Op::Edit => {
                let i: EditIn = parse(input)?;
                blocking(move || do_edit(&sh, &run, &i)).await?
            }
            Op::Move => {
                let i: MoveIn = parse(input)?;
                blocking(move || do_move(&sh, &run, &i)).await?
            }
            Op::Delete => {
                let i: DeleteIn = parse(input)?;
                blocking(move || do_delete(&sh, &run, &i)).await?
            }
            Op::CheckpointList => {
                let i: CpListIn = parse(input)?;
                let run = i.run.unwrap_or(run);
                blocking(move || do_cp_list(&sh, &run)).await?
            }
            Op::CheckpointRestore => {
                let i: CpRestoreIn = parse(input)?;
                blocking(move || do_cp_restore(&sh, &i.id)).await?
            }
        };
        Ok(finish_text(ctx, &self.shared.spill_dir, text))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::many_single_char_names)]
    use super::*;

    fn setup() -> (tempfile::TempDir, tempfile::TempDir, Vec<Arc<dyn Tool>>) {
        let data = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let t = tools(data.path());
        (data, work, t)
    }

    fn get(tools: &[Arc<dyn Tool>], name: &str) -> Arc<dyn Tool> {
        tools
            .iter()
            .find(|t| t.spec().name == name)
            .unwrap()
            .clone()
    }

    fn ctx() -> ToolContext {
        ToolContext {
            run_id: "run-1".into(),
            max_inline_bytes: 1 << 20,
        }
    }

    fn text(o: &ToolOutput) -> String {
        match &o.content[0] {
            tool_registry::ContentPart::Text { text } => text.clone(),
            other => panic!("not text: {other:?}"),
        }
    }

    fn p(dir: &tempfile::TempDir, name: &str) -> String {
        dir.path().join(name).display().to_string()
    }

    #[tokio::test]
    async fn write_edit_delete_checkpoint_round_trip() {
        let (_d, w, t) = setup();
        let file = p(&w, "a.txt");
        let c = ctx();
        get(&t, "fs_write")
            .call(&c, json!({"path": file, "content": "hello world"}))
            .await
            .unwrap();
        get(&t, "fs_edit")
            .call(&c, json!({"path": file, "old": "world", "new": "there"}))
            .await
            .unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "hello there");
        get(&t, "fs_delete")
            .call(&c, json!({"path": file}))
            .await
            .unwrap();
        assert!(!Path::new(&file).exists());

        let list = text(
            &get(&t, "checkpoint_list")
                .call(&c, json!({}))
                .await
                .unwrap(),
        );
        let ids: Vec<String> = list
            .lines()
            .map(|l| l.split(' ').next().unwrap().to_string())
            .collect();
        assert_eq!(ids.len(), 3);
        // Restore the checkpoint taken before the delete: content is "hello there".
        get(&t, "checkpoint_restore")
            .call(&c, json!({"id": ids[2]}))
            .await
            .unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "hello there");
        // Restore the checkpoint taken before the very first write: file did not exist.
        get(&t, "checkpoint_restore")
            .call(&c, json!({"id": ids[0]}))
            .await
            .unwrap();
        assert!(!Path::new(&file).exists());
    }

    #[tokio::test]
    async fn edit_requires_unique_match() {
        let (_d, w, t) = setup();
        let file = p(&w, "a.txt");
        fs::write(&file, "x x").unwrap();
        let c = ctx();
        let err = get(&t, "fs_edit")
            .call(&c, json!({"path": file, "old": "x", "new": "y"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("2 times"));
        get(&t, "fs_edit")
            .call(
                &c,
                json!({"path": file, "old": "x", "new": "y", "replace_all": true}),
            )
            .await
            .unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "y y");
        let err = get(&t, "fs_edit")
            .call(&c, json!({"path": file, "old": "zzz", "new": "y"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[tokio::test]
    async fn read_range_is_capped_in_total_and_keeps_line_numbers() {
        let (_d, w, t) = setup();
        let c = ctx();
        let file = p(&w, "big.txt");
        // ~17 MiB in many short lines: every line is small, the total is not.
        let line = "x".repeat(1023) + "\n";
        fs::write(&file, line.repeat(17 * 1024)).unwrap();
        let err = get(&t, "fs_read")
            .call(&c, json!({"path": file, "range": {"start": 1}}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("MiB"), "{err}");
        // A narrow range of the same file still works and counts lines right.
        let ok = text(
            &get(&t, "fs_read")
                .call(&c, json!({"path": file, "range": {"start": 5, "end": 6}}))
                .await
                .unwrap(),
        );
        assert!(ok.contains("lines 5-6 of 17408"), "{ok}");
    }

    #[tokio::test]
    async fn read_range_list_search_move() {
        let (_d, w, t) = setup();
        let c = ctx();
        let file = p(&w, "sub/n.txt");
        get(&t, "fs_write")
            .call(&c, json!({"path": file, "content": "one\nTwo\nthree\n"}))
            .await
            .unwrap();
        let r = text(
            &get(&t, "fs_read")
                .call(&c, json!({"path": file, "range": {"start": 2, "end": 2}}))
                .await
                .unwrap(),
        );
        assert!(r.ends_with("\nTwo"));
        let l = text(
            &get(&t, "fs_list")
                .call(&c, json!({"path": w.path(), "glob": "*.txt"}))
                .await
                .unwrap(),
        );
        assert!(l.contains("sub") && l.contains("n.txt"), "{l}");
        let s = text(
            &get(&t, "fs_search")
                .call(&c, json!({"query": "two", "path": w.path()}))
                .await
                .unwrap(),
        );
        assert!(s.contains(":2: Two"), "{s}");
        let dest = p(&w, "moved.txt");
        get(&t, "fs_move")
            .call(&c, json!({"from": file, "to": dest}))
            .await
            .unwrap();
        assert!(Path::new(&dest).exists() && !Path::new(&file).exists());
        let err = get(&t, "fs_move")
            .call(&c, json!({"from": dest, "to": dest}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("exists"));
    }

    #[tokio::test]
    async fn oversized_files_are_not_loaded_whole() {
        let (_d, w, t) = setup();
        let c = ctx();
        let file = p(&w, "big.log");
        let f = fs::File::create(&file).unwrap();
        drop(f);
        fs::write(&file, "x\n".repeat(16 * 1024 * 1024 / 2 + 10)).unwrap();
        let err = get(&t, "fs_read")
            .call(&c, json!({"path": file}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("larger than"), "{err}");
        let err = get(&t, "fs_edit")
            .call(&c, json!({"path": file, "old": "x", "new": "y"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("larger than"), "{err}");
        // A ranged read still works by streaming.
        let r = text(
            &get(&t, "fs_read")
                .call(&c, json!({"path": file, "range": {"start": 2, "end": 3}}))
                .await
                .unwrap(),
        );
        assert!(r.ends_with("\nx\nx"), "{r}");
    }

    #[tokio::test]
    async fn delete_directory_needs_recursive() {
        let (_d, w, t) = setup();
        let dir = p(&w, "dd");
        fs::create_dir_all(&dir).unwrap();
        let c = ctx();
        assert!(
            get(&t, "fs_delete")
                .call(&c, json!({"path": dir}))
                .await
                .is_err()
        );
        get(&t, "fs_delete")
            .call(&c, json!({"path": dir, "recursive": true}))
            .await
            .unwrap();
        assert!(!Path::new(&dir).exists());
    }

    #[tokio::test]
    async fn invalid_input_is_reported() {
        let (_d, _w, t) = setup();
        let err = get(&t, "fs_read")
            .call(&ctx(), json!({"nope": 1}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput(_)));
    }
}
