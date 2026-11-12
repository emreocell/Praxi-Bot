//! File tools (permission category `Fs`) with checkpoints before every change.
//!
//! `validate_path` runs before any stat or open. It rejects UNC and device
//! paths (including verbatim `\\?\UNC` and `\\.\` forms), refuses mapped
//! drives and reparse points that leave the local machine, and blocks
//! credential files unless the host granted that exact path. On Windows the
//! opened long path (8.3 names included) is checked before any byte is read.
//! Configured read roots are enforced on the lexical path and again after
//! symlink resolution. A non-empty root list that does not resolve denies
//! every call.

use std::cell::RefCell;
#[cfg(test)]
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

#[cfg(windows)]
#[path = "win_fs.rs"]
mod win_fs;

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
    /// Application data directory. When the directory exists this is the long
    /// path from `final_identity`, so an 8.3 `data_dir` still matches the
    /// normalized path the tools compare. Credential files under it (and
    /// well-known credential names anywhere) are refused.
    data_dir: PathBuf,
    /// When non-empty, every fs path must stay inside one of these directories.
    allowed_roots: Vec<PathBuf>,
    /// Credential paths the host explicitly granted. Does not permit UNC,
    /// device, or out-of-root access. Each protected path must be listed itself.
    grants: Vec<PathBuf>,
    /// The caller passed a non-empty root list and at least one root could
    /// not be resolved. Every `fs_*` call fails until this is false.
    roots_unresolved: bool,
    drives: DriveCache,
}

/// One file tool.
pub struct FsTool {
    spec: ToolSpec,
    op: Op,
    shared: Arc<Shared>,
}

/// Build all file tools; `data_dir` hosts checkpoints and spill files.
///
/// UNC paths, device paths, and credential files are refused. No root limit
/// is applied; use [`tools_bounded`] when the host has configured roots or grants.
#[must_use]
pub fn tools(data_dir: &Path) -> Vec<Arc<dyn Tool>> {
    tools_bounded(data_dir, &[], &[])
}

/// Like [`tools`], confined to `allowed_roots` when that slice is non-empty.
/// `grants` are absolute credential paths the host has explicitly allowed.
#[must_use]
pub(crate) fn tools_bounded(
    data_dir: &Path,
    allowed_roots: &[PathBuf],
    grants: &[PathBuf],
) -> Vec<Arc<dyn Tool>> {
    let drives = DriveCache::new();
    let (allowed_roots, roots_unresolved) = match prepare_roots(allowed_roots, &drives) {
        Ok(roots) => (roots, false),
        Err(()) => (Vec::new(), true),
    };
    let grants = prepare_grants(grants);
    let data_dir = canonical_data_dir(data_dir);
    let shared = Arc::new(Shared {
        checkpoints: CheckpointStore::new(&data_dir),
        spill_dir: data_dir.join("spill"),
        data_dir,
        allowed_roots,
        grants,
        roots_unresolved,
        drives,
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

const MAX_LINK_DEPTH: usize = 8;
const UNC_MSG: &str = "UNC paths are not allowed";
const DEVICE_MSG: &str = "device paths are not allowed";
const ROOT_MSG: &str = "path is outside the allowed roots";
const ROOT_UNRESOLVED_MSG: &str = "allowed root could not be resolved";
const GRANT_MSG: &str = "credential or control file requires an explicit grant";
const REPARSE_MSG: &str = "reparse points are not allowed";
const LINK_MSG: &str = "symlink chain is too deep";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    Local,
    Unc,
    Device,
}

/// How far [`validate_path`] follows links before the filesystem is changed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Follow {
    /// Read the file or directory. The final symlink is resolved.
    Content,
    /// Write bytes. The final symlink is resolved; a missing tail is allowed.
    Write,
    /// Rename or delete this directory entry. The final symlink is kept.
    Entry,
    /// Create or replace this directory entry. The final symlink is kept.
    Place,
}

fn denied(msg: &str) -> ToolError {
    ToolError::PermissionDenied(msg.to_string())
}

fn follows_final(follow: Follow) -> bool {
    matches!(follow, Follow::Content | Follow::Write)
}

fn allows_missing(follow: Follow) -> bool {
    matches!(follow, Follow::Write | Follow::Place)
}

#[derive(Clone, Copy)]
struct ResolveCtx<'a> {
    follow: Follow,
    roots: &'a [PathBuf],
    drives: &'a DriveCache,
    depth: usize,
    /// Checkpoint copies treat a cloud-filter placeholder as a normal file.
    /// `fs_read` leaves this false, so a placeholder is not hydrated on read.
    allow_cloud_placeholder: bool,
}

#[cfg(test)]
#[derive(Clone, Debug)]
struct DriveSnapshot {
    mapped: HashSet<char>,
    subst: HashMap<char, PathBuf>,
}

/// Drive letters are classified on each use. Nothing is cached: a mapping
/// created after the first file call must still be visible, and a failed
/// query must not become an empty allow-list.
struct DriveCache;

impl DriveCache {
    fn new() -> Self {
        Self
    }

    fn apply(&self, path: &Path) -> Result<PathBuf, ToolError> {
        let _ = self;
        confine_drive(path)
    }
}

/// String classification only. No filesystem access, so it cannot touch the network.
fn classify_path_str(raw: &str) -> Class {
    if raw.contains('\0') {
        return Class::Device;
    }
    let lower = raw.replace('/', "\\").to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix(r"\\") {
        return classify_double_slash(rest);
    }
    if let Some(rest) = lower.strip_prefix(r"\??\") {
        return classify_nt_prefix(rest);
    }
    if lower.starts_with(r"\device\") || lower == r"\device" {
        return if lower.contains("mup") || lower.contains("lanman") || lower.contains(r"\unc\") {
            Class::Unc
        } else {
            Class::Device
        };
    }
    Class::Local
}

fn classify_double_slash(rest: &str) -> Class {
    if let Some(after) = rest.strip_prefix(r"?\") {
        return classify_verbatim(after);
    }
    if rest.is_empty() || rest.starts_with(['?', '.']) {
        return Class::Device;
    }
    Class::Unc
}

fn classify_verbatim(after: &str) -> Class {
    if after == "unc" || after.starts_with(r"unc\") {
        return Class::Unc;
    }
    if is_drive_prefix(after) {
        return Class::Local;
    }
    Class::Device
}

fn classify_nt_prefix(rest: &str) -> Class {
    if rest == "unc" || rest.starts_with(r"unc\") {
        return Class::Unc;
    }
    if is_drive_prefix(rest) {
        return Class::Local;
    }
    if rest.contains(r"\unc\") || rest.contains("lanman") || rest.contains("mup") {
        return Class::Unc;
    }
    Class::Device
}

fn is_drive_prefix(rest: &str) -> bool {
    let b = rest.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

fn reject_syntax(raw: &str) -> Result<(), ToolError> {
    if raw.is_empty() || raw.contains('\0') {
        return Err(ToolError::InvalidInput(
            "path is empty or contains NUL".into(),
        ));
    }
    match classify_path_str(raw) {
        Class::Local => Ok(()),
        Class::Unc => Err(denied(UNC_MSG)),
        Class::Device => Err(denied(DEVICE_MSG)),
    }
}

/// `\\?\C:\...` and `\??\C:\...` become `C:\...`. Other strings are unchanged.
fn strip_local_prefix(raw: &str) -> String {
    let lower = raw.replace('/', "\\").to_ascii_lowercase();
    for prefix in [r"\\?\", r"\??\"] {
        if let Some(rest) = lower.strip_prefix(prefix)
            && is_drive_prefix(rest)
        {
            return raw.replace('/', "\\")[prefix.len()..].to_string();
        }
    }
    raw.to_string()
}

fn component_is_device(name: &str) -> bool {
    // DOS device names and `:` (alternate data streams) are a Windows
    // `CreateFileW` rule. The same byte strings are ordinary file names on
    // Unix and must not be refused there.
    #[cfg(not(windows))]
    {
        let _ = name;
        false
    }
    #[cfg(windows)]
    {
        if name.contains([':', '\0']) {
            return true;
        }
        let n = name.trim_end_matches([' ', '.']).to_ascii_uppercase();
        matches!(
            n.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
        ) || (n.len() == 4
            && (n.starts_with("COM") || n.starts_with("LPT"))
            && n.as_bytes()
                .get(3)
                .is_some_and(|b| (b'1'..=b'9').contains(b)))
    }
}

fn lexical_normalize(path: &Path) -> Result<PathBuf, ToolError> {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::Prefix(_) | Component::RootDir => out.push(c.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                }
            }
            Component::Normal(name) => {
                if component_is_device(&name.to_string_lossy()) {
                    return Err(denied(DEVICE_MSG));
                }
                out.push(name);
            }
        }
    }
    Ok(out)
}

#[cfg(any(windows, test))]
fn drive_letter(path: &Path) -> Option<char> {
    for c in path.components() {
        let Component::Prefix(prefix) = c else {
            continue;
        };
        let s = prefix.as_os_str().to_string_lossy();
        let bytes = s.as_bytes();
        for i in 0..bytes.len().saturating_sub(1) {
            if bytes[i].is_ascii_alphabetic() && bytes[i + 1] == b':' {
                return Some(char::from(bytes[i]).to_ascii_uppercase());
            }
        }
    }
    None
}

fn is_drive_relative(path: &Path) -> bool {
    path.components().any(|c| matches!(c, Component::Prefix(_))) && !path.is_absolute()
}

fn is_root_relative(path: &Path) -> bool {
    #[cfg(windows)]
    {
        path.is_absolute() && drive_letter(path).is_none()
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        false
    }
}

fn prefix_current_drive(path: &Path) -> Result<PathBuf, ToolError> {
    #[cfg(windows)]
    {
        let cwd =
            std::env::current_dir().map_err(|e| exec_err(format!("current directory: {e}")))?;
        let letter = drive_letter(&cwd).ok_or_else(|| denied(UNC_MSG))?;
        let mut out = PathBuf::from(format!("{letter}:\\"));
        for c in path.components() {
            if !matches!(c, Component::Prefix(_) | Component::RootDir) {
                out.push(c);
            }
        }
        Ok(out)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err(denied(UNC_MSG))
    }
}

fn lexical_absolute(raw: &str) -> Result<PathBuf, ToolError> {
    reject_syntax(raw)?;
    let stripped = strip_local_prefix(raw);
    reject_syntax(&stripped)?;
    let path = Path::new(&stripped);
    if is_drive_relative(path) {
        return Err(denied("drive-relative paths are not allowed"));
    }
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else if is_root_relative(path) {
        prefix_current_drive(path)?
    } else {
        let cwd =
            std::env::current_dir().map_err(|e| exec_err(format!("current directory: {e}")))?;
        reject_syntax(&cwd.to_string_lossy())?;
        cwd.join(path)
    };
    let norm = lexical_normalize(&abs)?;
    reject_syntax(&norm.to_string_lossy())?;
    Ok(norm)
}

fn split_anchor(path: &Path) -> Result<(PathBuf, Vec<OsString>), ToolError> {
    let mut anchor = PathBuf::new();
    let mut names = Vec::new();
    for c in path.components() {
        match c {
            Component::Prefix(_) | Component::RootDir => anchor.push(c.as_os_str()),
            Component::Normal(name) => names.push(name.to_os_string()),
            Component::CurDir => {}
            Component::ParentDir => return Err(denied(ROOT_MSG)),
        }
    }
    Ok((anchor, names))
}

fn join_link(parent: &Path, target: &Path, rest: &[OsString]) -> Result<PathBuf, ToolError> {
    let mut combined = if target.is_absolute() {
        target.to_path_buf()
    } else {
        parent.join(target)
    };
    for name in rest {
        if component_is_device(&name.to_string_lossy()) {
            return Err(denied(DEVICE_MSG));
        }
        combined.push(name);
    }
    lexical_normalize(&combined)
}

/// Resolve `raw` to a local path that is safe to stat and open.
///
/// Classification of UNC, device, and verbatim prefixes happens before any
/// filesystem call. Roots are checked on the lexical path before the walk, and
/// again on every symlink target before that target is stated.
fn validate_path(sh: &Shared, raw: &str, follow: Follow) -> Result<PathBuf, ToolError> {
    if sh.roots_unresolved {
        return Err(denied(ROOT_UNRESOLVED_MSG));
    }
    let lexical = lexical_absolute(raw)?;
    let ctx = ResolveCtx {
        follow,
        roots: &sh.allowed_roots,
        drives: &sh.drives,
        depth: 0,
        allow_cloud_placeholder: false,
    };
    let resolved = resolve_local(&lexical, &ctx)?;
    // The reparse-safe long path is the name Win32 will open. Check it before
    // any caller reads bytes. `resolved` may still be an 8.3 alias.
    let opened = final_identity(&resolved)?;
    if !sh.allowed_roots.is_empty() && !roots_contain(&sh.allowed_roots, &opened) {
        return Err(denied(ROOT_MSG));
    }
    if credential_blocked(sh, &lexical, &resolved) || credential_blocked(sh, &lexical, &opened) {
        return Err(denied(GRANT_MSG));
    }
    Ok(opened)
}

fn resolve_local(path: &Path, ctx: &ResolveCtx<'_>) -> Result<PathBuf, ToolError> {
    if ctx.depth > MAX_LINK_DEPTH {
        return Err(denied(LINK_MSG));
    }
    let path = lexical_normalize(path)?;
    reject_syntax(&path.to_string_lossy())?;
    let path = ctx.drives.apply(&path)?;
    reject_syntax(&path.to_string_lossy())?;
    if !ctx.roots.is_empty() && !roots_contain(ctx.roots, &path) {
        return Err(denied(ROOT_MSG));
    }
    let (mut acc, names) = split_anchor(&path)?;
    for (i, name) in names.iter().enumerate() {
        if component_is_device(&name.to_string_lossy()) {
            return Err(denied(DEVICE_MSG));
        }
        let next = acc.join(name);
        match fs::symlink_metadata(&next) {
            Ok(meta) if reparse_unsafe(&meta) => {
                if ctx.allow_cloud_placeholder && cloud_placeholder(&next, &meta) {
                    acc = next;
                } else {
                    return Err(denied(REPARSE_MSG));
                }
            }
            Ok(meta) if meta.file_type().is_symlink() => {
                return follow_link(&acc, &next, &names[i + 1..], ctx);
            }
            Ok(_) => acc = next,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && allows_missing(ctx.follow) => {
                return append_missing(&next, &names[i + 1..]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(exec_err(format!("{} does not exist", path.display())));
            }
            Err(e) => return Err(exec_err(format!("{}: {e}", next.display()))),
        }
    }
    Ok(acc)
}

fn append_missing(start: &Path, rest: &[OsString]) -> Result<PathBuf, ToolError> {
    let mut acc = start.to_path_buf();
    for name in rest {
        if component_is_device(&name.to_string_lossy()) {
            return Err(denied(DEVICE_MSG));
        }
        acc.push(name);
    }
    Ok(acc)
}

fn follow_link(
    parent: &Path,
    link: &Path,
    rest: &[OsString],
    ctx: &ResolveCtx<'_>,
) -> Result<PathBuf, ToolError> {
    let target = fs::read_link(link).map_err(|e| exec_err(format!("{}: {e}", link.display())))?;
    let raw = target.to_string_lossy();
    if dos_target_is_remote(&raw) {
        return Err(denied(UNC_MSG));
    }
    let stripped = strip_local_prefix(&raw);
    reject_syntax(&stripped)?;
    let combined = join_link(parent, Path::new(&stripped), rest)?;
    if rest.is_empty() && !follows_final(ctx.follow) {
        let verify = ResolveCtx {
            // Removing or preserving a link does not require its referent to
            // exist. Still classify its local target and every ancestor.
            follow: Follow::Write,
            depth: ctx.depth + 1,
            ..*ctx
        };
        let _verified = resolve_local(&combined, &verify)?;
        return Ok(link.to_path_buf());
    }
    let next = ResolveCtx {
        depth: ctx.depth + 1,
        ..*ctx
    };
    resolve_local(&combined, &next)
}

/// Cloud Files placeholder (`OneDrive`). `read_link` fails on this reparse tag.
/// Checkpoint walks treat the file as ordinary so deleting or moving its
/// parent folder is not aborted.
pub(crate) fn cloud_placeholder(path: &Path, meta: &fs::Metadata) -> bool {
    if !reparse_unsafe(meta) {
        return false;
    }
    #[cfg(windows)]
    {
        matches!(win_fs::reparse_tag(path), Ok(Some(tag)) if is_cloud_reparse_tag(tag))
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        false
    }
}

/// `IO_REPARSE_TAG_CLOUD` … `IO_REPARSE_TAG_CLOUD_F`, plus the older placeholder tag.
#[cfg(windows)]
fn is_cloud_reparse_tag(tag: u32) -> bool {
    const FILE_PLACEHOLDER: u32 = 0x8000_0015;
    tag == FILE_PLACEHOLDER || (tag & 0xFFFF_0FFF) == 0x9000_001A
}

fn reparse_unsafe(meta: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 && !meta.file_type().is_symlink()
    }
    #[cfg(not(windows))]
    {
        let _ = meta;
        false
    }
}

/// Resolve every configured root. An empty input stays empty (no limit).
/// Any root that cannot be resolved is an error: dropping it would leave an
/// empty list and turn the limit off.
fn prepare_roots(roots: &[PathBuf], drives: &DriveCache) -> Result<Vec<PathBuf>, ()> {
    if roots.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::with_capacity(roots.len());
    for root in roots {
        let abs = lexical_absolute(&root.to_string_lossy()).map_err(|_| ())?;
        let ctx = ResolveCtx {
            follow: Follow::Content,
            roots: &[],
            drives,
            depth: 0,
            allow_cloud_placeholder: false,
        };
        let resolved = resolve_local(&abs, &ctx).map_err(|_| ())?;
        let opened = final_identity(&resolved).map_err(|_| ())?;
        out.push(opened);
    }
    Ok(out)
}

fn prepare_grants(grants: &[PathBuf]) -> Vec<PathBuf> {
    grants
        .iter()
        .filter_map(|g| lexical_absolute(&g.to_string_lossy()).ok())
        .collect()
}

const CREDENTIAL_NAMES: &[&str] = &[
    "api-token",
    "praxi.env",
    "praxi-node.env",
    "secrets.enc",
    "identity.key",
];

/// Hard link to a credential file under another name (`notes.txt` → `.env`)
/// is not detected. These tools cannot create a hard link; a second directory
/// entry for the same file is a known limitation of the name denylist.
fn name_is_credential(name: &str) -> bool {
    let n = name.trim_end_matches([' ', '.']).to_ascii_lowercase();
    n == ".env" || n.starts_with(".env.") || CREDENTIAL_NAMES.contains(&n.as_str())
}

fn is_protected(path: &Path, data_dir: &Path) -> bool {
    let named = path.components().any(|c| match c {
        Component::Normal(name) => name_is_credential(&name.to_string_lossy()),
        Component::Prefix(_) | Component::RootDir | Component::CurDir | Component::ParentDir => {
            false
        }
    });
    named || under_data_sensitive(path, data_dir)
}

fn under_data_sensitive(path: &Path, data_dir: &Path) -> bool {
    let path = widen_path(path).unwrap_or_else(|_| path.to_path_buf());
    let data_dir = widen_path(data_dir).unwrap_or_else(|_| data_dir.to_path_buf());
    let p = norm_parts(&path);
    let d = norm_parts(&data_dir);
    if p.len() <= d.len() || p[..d.len()] != d[..] {
        return false;
    }
    // Windows resolves a trailing dot or space and `:stream` / `::$DATA` to the
    // same file, so names are compared canonically (on every platform: a
    // lexical match is cheap and conservative).
    let rel: Vec<String> = p[d.len()..]
        .iter()
        .map(|part| canonical_name(part))
        .collect();
    let Some(base) = rel.last().map(String::as_str) else {
        return false;
    };
    // Directly under the data dir only: these names are ordinary elsewhere.
    base == "devices.json"
        || base == "peer-node"
        || rel.iter().any(|s| s == "vault" || s == "secrets")
        || rel
            .first()
            .is_some_and(|first| DAEMON_STATE_DIRS.contains(&first.as_str()))
        || (rel.len() == 1 && daemon_state_name(base))
}

/// Compare-form of one path component: no `:stream` suffix, no trailing dots or
/// spaces, lower case.
fn canonical_name(part: &str) -> String {
    let base = part.split(':').next().unwrap_or(part);
    base.trim_end_matches([' ', '.']).to_ascii_lowercase()
}

/// Directories under the data dir whose contents grant authority or are the
/// daemon's own records, so the bot's file tools never touch them:
/// `peer` (pairing keys and device list), `bots` and `profiles` (bot profiles
/// and their policies), `runs` (the event logs the UI and replay trust).
/// Deliberately NOT protected, with the reason: `skills` and `workflows`
/// (the bot authors them on purpose), `checkpoints` (the file tools' own undo
/// store), `files`, `spill` and `screenshots` (bot output), logs (no authority).
const DAEMON_STATE_DIRS: [&str; 4] = ["peer", "bots", "profiles", "runs"];

/// The daemon's own state files directly under the data dir, each one a file
/// that grants authority or forges history if tampered with:
/// - `bot.db`, `schedules.db`, `memory.db` (+ `-wal`/`-shm`/`-journal`):
///   chats, approvals, audit, schedules that run unattended, memory.
/// - `control-latch.json` (the takeover latch and its release journal) and
///   `policy.json` (the global permission policy).
/// - `host.toml` (leader/epoch), `config.toml` (peer networking), `mcp.toml`
///   (servers the daemon launches), `providers.toml` (where model traffic
///   goes), `web.toml` (web tool limits).
/// - `telegram_state.json` and `telegram-inbox.jsonl` (which chats may command
///   the bot, and what they sent).
/// - the atomic-write temp files (`.tmp*`) of any of them.
///
/// Deliberately NOT listed: `skills.lock.json` / `skills-usage.json` (counters
/// and a lockfile for bot-writable skills; no authority) and logs.
fn daemon_state_name(base: &str) -> bool {
    const DATABASES: [&str; 3] = ["bot.db", "schedules.db", "memory.db"];
    const FILES: [&str; 9] = [
        "control-latch.json",
        "policy.json",
        "host.toml",
        "config.toml",
        "mcp.toml",
        "providers.toml",
        "web.toml",
        "telegram_state.json",
        "telegram-inbox.jsonl",
    ];
    DATABASES.iter().any(|s| {
        base == *s
            || base
                .strip_prefix(s)
                .is_some_and(|rest| matches!(rest, "-wal" | "-shm" | "-journal"))
    }) || FILES.iter().any(|s| {
        base == *s
            || base
                .strip_prefix(s)
                .is_some_and(|rest| matches!(rest, ".tmp" | ".bak"))
    }) || base.starts_with(".tmp")
}

/// A protected path is readable only when that path itself is granted.
/// A grant of the lexical path does not authorize a different resolved path,
/// and a grant of the resolved path does not authorize a different lexical one.
/// The 8.3 name and the long name of one file are the same path.
fn credential_blocked(sh: &Shared, lexical: &Path, resolved: &Path) -> bool {
    protected_ungranted(lexical, &sh.data_dir, &sh.grants)
        || protected_ungranted(resolved, &sh.data_dir, &sh.grants)
}

fn protected_ungranted(path: &Path, data_dir: &Path, grants: &[PathBuf]) -> bool {
    is_protected(path, data_dir) && !grants.iter().any(|g| grant_matches(g, path))
}

/// Exact path, or the same file under its 8.3 and long names.
///
/// A failed widen does not count as a match: an alias that cannot be verified
/// stays denied.
fn grant_matches(grant: &Path, path: &Path) -> bool {
    if same_path(grant, path) {
        return true;
    }
    match (widen_path(grant), widen_path(path)) {
        (Ok(a), Ok(b)) => same_path(&a, &b),
        _ => false,
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    norm_parts(a) == norm_parts(b)
}

fn roots_contain(roots: &[PathBuf], path: &Path) -> bool {
    // An 8.3 alias that cannot be widened is not inside. Matching the short
    // text against a long root would deny a real child; matching it without
    // widening would also let a different short name through.
    let Ok(path) = widen_path(path) else {
        return false;
    };
    let p = norm_parts(&path);
    roots.iter().any(|root| {
        let Ok(root) = widen_path(root) else {
            return false;
        };
        let r = norm_parts(&root);
        !r.is_empty() && p.len() >= r.len() && p[..r.len()] == r[..]
    })
}

/// Long form of `path`, without following a symlink or junction.
///
/// Roots and the opened identity come from `GetFinalPathNameByHandleW`, which
/// expands `RUNNER~1` to `runneradmin`. The caller's path is often still the
/// 8.3 temp directory, so containment and grants compare one form. The walk
/// stops at a reparse point; the link target is classified by the caller.
#[cfg_attr(not(windows), allow(clippy::unnecessary_wraps))]
fn widen_path(path: &Path) -> Result<PathBuf, ToolError> {
    #[cfg(not(windows))]
    {
        Ok(path.to_path_buf())
    }
    #[cfg(windows)]
    {
        // UNC and device paths are rejected before any directory is opened.
        reject_syntax(&path.to_string_lossy())?;
        let (anchor, names) = split_anchor(path)?;
        let mut acc = anchor;
        for (i, name) in names.iter().enumerate() {
            if component_is_device(&name.to_string_lossy()) {
                return Err(denied(DEVICE_MSG));
            }
            let next = acc.join(name);
            match fs::symlink_metadata(&next) {
                Ok(meta) if reparse_unsafe(&meta) || meta.file_type().is_symlink() => {
                    return append_missing(&next, &names[i + 1..]);
                }
                Ok(_) => acc = final_identity(&next)?,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    return append_missing(&next, &names[i + 1..]);
                }
                Err(_) => return Err(denied("path could not be verified")),
            }
        }
        Ok(acc)
    }
}

fn fold_part(s: &str) -> String {
    #[cfg(windows)]
    {
        s.to_ascii_lowercase()
    }
    #[cfg(not(windows))]
    {
        s.to_string()
    }
}

fn norm_parts(path: &Path) -> Vec<String> {
    path.components()
        .map(|c| fold_part(&c.as_os_str().to_string_lossy()))
        .collect()
}

fn dos_target_is_remote(target: &str) -> bool {
    let t = target.replace('/', "\\").to_ascii_lowercase();
    t.contains("lanmanredirector")
        || t.contains(r"\unc\")
        || t.contains(r"\mup")
        || t.starts_with(r"\\")
}

#[cfg(test)]
fn parse_mapped_drives(output: &str) -> HashSet<char> {
    let mut set = HashSet::new();
    for line in output.lines() {
        let line = line.trim();
        let lower = line.to_ascii_lowercase();
        let Some(idx) = lower.rfind(r"\network\") else {
            continue;
        };
        let after = &line[idx + r"\network\".len()..];
        let token = after.split(['\\', '/']).next().unwrap_or("");
        let mut chars = token.chars();
        let Some(c) = chars.next() else {
            continue;
        };
        if chars.next().is_none() && c.is_ascii_alphabetic() {
            set.insert(c.to_ascii_uppercase());
        }
    }
    set
}

#[cfg(test)]
fn parse_subst_listing(output: &str) -> HashMap<char, String> {
    let mut map = HashMap::new();
    for line in output.lines() {
        let Some((left, right)) = line.split_once("=>") else {
            continue;
        };
        let Some(c) = left.trim().chars().next() else {
            continue;
        };
        if !c.is_ascii_alphabetic() {
            continue;
        }
        let target = right.trim();
        if target.is_empty() {
            continue;
        }
        map.insert(c.to_ascii_uppercase(), target.to_string());
    }
    map
}

#[cfg(any(windows, test))]
fn local_subst_target(target: &str) -> Option<PathBuf> {
    if dos_target_is_remote(target) || classify_path_str(target) != Class::Local {
        return None;
    }
    let stripped = strip_local_prefix(target);
    if dos_target_is_remote(&stripped) || classify_path_str(&stripped) != Class::Local {
        return None;
    }
    let path = PathBuf::from(&stripped);
    if drive_letter(&path).is_none() && stripped.starts_with(['\\', '/']) {
        return None;
    }
    Some(path)
}

#[cfg(test)]
fn snapshot_from(reg_out: &str, subst_out: &str) -> DriveSnapshot {
    let mut mapped = parse_mapped_drives(reg_out);
    let mut subst = HashMap::new();
    for (letter, target) in parse_subst_listing(subst_out) {
        if let Some(path) = local_subst_target(&target) {
            subst.insert(letter, path);
        } else {
            mapped.insert(letter);
        }
    }
    DriveSnapshot { mapped, subst }
}

#[cfg(windows)]
const DRIVE_UNKNOWN: u32 = 0;
#[cfg(windows)]
const DRIVE_REMOTE: u32 = 4;

/// Classify one letter from a DOS-device query and a `GetDriveTypeW` code.
/// `Failed` and `DRIVE_UNKNOWN` deny the path. A remote target denies it.
/// A local `subst` (`\??\C:\...`) is rewritten; a fixed volume is kept.
#[cfg(windows)]
fn drive_decision(query: win_fs::DosQuery, drive_type: u32) -> Result<Option<PathBuf>, ToolError> {
    match query {
        win_fs::DosQuery::Failed => Err(denied("drive mapping could not be verified")),
        win_fs::DosQuery::Target(target) => {
            if dos_target_is_remote(&target) || drive_type == DRIVE_REMOTE {
                return Err(denied("mapped network drives are not allowed"));
            }
            if drive_type == DRIVE_UNKNOWN {
                return Err(denied("drive mapping could not be verified"));
            }
            Ok(local_subst_target(&target))
        }
        win_fs::DosQuery::Missing => {
            if drive_type == DRIVE_REMOTE {
                return Err(denied("mapped network drives are not allowed"));
            }
            if drive_type == DRIVE_UNKNOWN {
                return Err(denied("drive mapping could not be verified"));
            }
            Ok(None)
        }
    }
}

#[cfg(windows)]
fn rewrite_drive_live(path: &Path, depth: u8) -> Result<PathBuf, ToolError> {
    if depth > 4 {
        return Err(denied("drive substitution is too deep"));
    }
    let Some(letter) = drive_letter(path) else {
        return Ok(path.to_path_buf());
    };
    let query = win_fs::query_dos_device(&format!("{letter}:"));
    let kind = win_fs::drive_type(&format!(r"{letter}:\"));
    let Some(target) = drive_decision(query, kind)? else {
        return Ok(path.to_path_buf());
    };
    let rewritten = lexical_normalize(&target.join(path_after_drive(path)))?;
    reject_syntax(&rewritten.to_string_lossy())?;
    rewrite_drive_live(&rewritten, depth + 1)
}

#[cfg_attr(not(windows), allow(clippy::unnecessary_wraps))]
fn confine_drive(path: &Path) -> Result<PathBuf, ToolError> {
    #[cfg(windows)]
    {
        rewrite_drive_live(path, 0)
    }
    #[cfg(not(windows))]
    {
        Ok(path.to_path_buf())
    }
}

#[cfg(windows)]
fn path_after_drive(path: &Path) -> PathBuf {
    let mut rest = PathBuf::new();
    for c in path.components() {
        if !matches!(c, Component::Prefix(_) | Component::RootDir) {
            rest.push(c);
        }
    }
    rest
}

/// Long path of an existing file, or the caller's path when it does not exist yet.
///
/// On Windows the name comes from `GetFinalPathNameByHandleW` on a reparse-safe
/// handle, so `ENV~1` becomes `.env` before the denylist runs. File bytes are
/// not read. A failure to name an existing path is denied (fail closed).
#[cfg_attr(not(windows), allow(clippy::unnecessary_wraps))]
fn final_identity(path: &Path) -> Result<PathBuf, ToolError> {
    #[cfg(windows)]
    {
        match win_fs::final_path_nofollow(path) {
            Ok(opened) => normalize_final(&opened),
            Err(e) if e.kind() == io::ErrorKind::NotFound => final_missing(path),
            Err(_) => Err(denied("path could not be verified")),
        }
    }
    #[cfg(not(windows))]
    {
        Ok(path.to_path_buf())
    }
}

#[cfg(windows)]
fn final_missing(path: &Path) -> Result<PathBuf, ToolError> {
    let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) else {
        return Ok(path.to_path_buf());
    };
    let Some(name) = path.file_name() else {
        return Ok(path.to_path_buf());
    };
    match win_fs::final_path_nofollow(parent) {
        Ok(parent) => normalize_final(&parent.join(name)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(path.to_path_buf()),
        Err(_) => Err(denied("path could not be verified")),
    }
}

#[cfg(windows)]
fn normalize_final(path: &Path) -> Result<PathBuf, ToolError> {
    let raw = path.to_string_lossy();
    let stripped = strip_local_prefix(&raw);
    reject_syntax(&stripped)?;
    lexical_normalize(Path::new(&stripped))
}

/// Long path of `data_dir` when the directory exists.
///
/// Callers compare this prefix to paths that `GetFinalPathNameByHandleW` has
/// already expanded. A lexical 8.3 `data_dir` would not match that long form,
/// so `devices.json`, `peer-node`, `vault/`, and `secrets/` would look ordinary.
fn canonical_data_dir(data_dir: &Path) -> PathBuf {
    let lexical =
        lexical_absolute(&data_dir.to_string_lossy()).unwrap_or_else(|_| data_dir.to_path_buf());
    // Widen the existing ancestor as well as an existing final directory.
    // The data directory can be created only after tools are constructed.
    let widened = widen_path(&lexical).unwrap_or(lexical);
    // `widen_path` stops at a reparse point, so a data directory that is
    // itself a junction keeps its (possibly 8.3) final name. Name that
    // junction object, not its target.
    final_identity(&widened).unwrap_or(widened)
}

/// Identity of a path already observed with `symlink_metadata`.
///
/// `NotFound` and every other failure are denied. A failed lookup is not
/// evidence that the path is an absent local file.
#[cfg_attr(not(windows), allow(clippy::unnecessary_wraps))]
fn identity_existing(path: &Path) -> Result<PathBuf, ToolError> {
    #[cfg(windows)]
    {
        match win_fs::final_path_nofollow(path) {
            Ok(opened) => normalize_final(&opened),
            Err(_) => Err(denied("path could not be verified")),
        }
    }
    #[cfg(not(windows))]
    {
        Ok(path.to_path_buf())
    }
}

/// Name `path` after its ancestors have been walked.
///
/// The parent must already exist. A parent open that fails, including
/// `NotFound`, is a deny — the lexical path is not treated as safe to write.
#[cfg_attr(not(windows), allow(clippy::unnecessary_wraps))]
fn identity_for_write(path: &Path) -> Result<PathBuf, ToolError> {
    #[cfg(windows)]
    {
        match win_fs::final_path_nofollow(path) {
            Ok(opened) => normalize_final(&opened),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let parent = path
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .ok_or_else(|| denied("path could not be verified"))?;
                let name = path
                    .file_name()
                    .ok_or_else(|| denied("path could not be verified"))?;
                match win_fs::final_path_nofollow(parent) {
                    Ok(parent) => normalize_final(&parent.join(name)),
                    Err(_) => Err(denied("path could not be verified")),
                }
            }
            Err(_) => Err(denied("path could not be verified")),
        }
    }
    #[cfg(not(windows))]
    {
        Ok(path.to_path_buf())
    }
}

/// Policy passed into checkpoint copies. Checkpoints run without `Shared`, so
/// the file tool installs this for the duration of one call on this thread.
#[derive(Clone)]
struct CpGate {
    data_dir: PathBuf,
    roots: Vec<PathBuf>,
    grants: Vec<PathBuf>,
    roots_unresolved: bool,
}

thread_local! {
    static CP_GATE: RefCell<Option<CpGate>> = const { RefCell::new(None) };
}

struct ClearGate;

impl Drop for ClearGate {
    fn drop(&mut self) {
        CP_GATE.with(|slot| *slot.borrow_mut() = None);
    }
}

fn with_checkpoint_gate<T>(sh: &Shared, f: impl FnOnce() -> T) -> T {
    CP_GATE.with(|slot| {
        *slot.borrow_mut() = Some(CpGate {
            data_dir: sh.data_dir.clone(),
            roots: sh.allowed_roots.clone(),
            grants: sh.grants.clone(),
            roots_unresolved: sh.roots_unresolved,
        });
    });
    let _clear = ClearGate;
    f()
}

fn current_gate() -> Option<CpGate> {
    CP_GATE.with(|slot| slot.borrow().clone())
}

fn tool_to_io(err: ToolError) -> io::Error {
    match err {
        ToolError::PermissionDenied(m) => io::Error::new(io::ErrorKind::PermissionDenied, m),
        other => io::Error::other(other.to_string()),
    }
}

fn io_to_tool(err: &io::Error, context: &str) -> ToolError {
    if err.kind() == io::ErrorKind::PermissionDenied {
        ToolError::PermissionDenied(err.to_string())
    } else {
        exec_err(format!("{context}: {err}"))
    }
}

struct CpPolicy {
    data_dir: PathBuf,
    roots: Vec<PathBuf>,
    grants: Vec<PathBuf>,
    roots_unresolved: bool,
}

fn checkpoint_policy() -> CpPolicy {
    current_gate().map_or_else(
        || CpPolicy {
            data_dir: PathBuf::from(r"\\.\no-data-dir"),
            roots: Vec::new(),
            grants: Vec::new(),
            roots_unresolved: false,
        },
        |g| CpPolicy {
            data_dir: g.data_dir,
            roots: g.roots,
            grants: g.grants,
            roots_unresolved: g.roots_unresolved,
        },
    )
}

fn deny_io(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, msg)
}

/// Component walk used by checkpoints. Ancestors are classified with
/// `symlink_metadata` and `read_link` before any `CreateFile` of the full path.
fn walk_checkpoint(path: &Path, policy: &CpPolicy) -> Result<(PathBuf, PathBuf), ToolError> {
    if policy.roots_unresolved {
        return Err(denied(ROOT_UNRESOLVED_MSG));
    }
    let raw = path.to_string_lossy();
    reject_syntax(&raw)?;
    let lexical = lexical_absolute(&raw)?;
    let lexical = confine_drive(&lexical)?;
    // Blobs under `<data_dir>/checkpoints` are the tool's own copies. The
    // caller's roots still apply to every user path and link target.
    let root_limit: &[PathBuf] = if in_checkpoint_store(&lexical, &policy.data_dir) {
        &[]
    } else {
        &policy.roots
    };
    let drives = DriveCache::new();
    let ctx = ResolveCtx {
        follow: Follow::Place,
        roots: root_limit,
        drives: &drives,
        depth: 0,
        allow_cloud_placeholder: true,
    };
    let resolved = resolve_local(&lexical, &ctx)?;
    if !root_limit.is_empty() && !roots_contain(root_limit, &resolved) {
        return Err(denied(ROOT_MSG));
    }
    Ok((lexical, resolved))
}

fn in_checkpoint_store(path: &Path, data_dir: &Path) -> bool {
    let Ok(path) = widen_path(path) else {
        return false;
    };
    let Ok(data_dir) = widen_path(data_dir) else {
        return false;
    };
    let store = data_dir.join("checkpoints");
    let p = norm_parts(&path);
    let s = norm_parts(&store);
    !s.is_empty() && p.len() >= s.len() && p[..s.len()] == s[..]
}

fn protected_paths(paths: &[&Path], policy: &CpPolicy) -> bool {
    paths
        .iter()
        .any(|p| protected_ungranted(p, &policy.data_dir, &policy.grants))
}

/// What a checkpoint may do with one path after the ancestor walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CheckpointNode {
    /// The path is a local, non-credential location. Callers may copy it.
    Proceed,
    /// The path is a protected credential. Descendants are skipped: not copied,
    /// not followed, and not deleted. The top-level path still fails the tool.
    SkipProtected,
}

/// Classify `path` before a checkpoint reads it.
///
/// The walk matches `resolve_local`: each component is stated with
/// `symlink_metadata`, link text is classified, and roots apply to the
/// resolved path. `CreateFile` (`final_identity`) runs only after that walk,
/// and only for a path the walk found. A failed identity lookup is denied.
/// A path the walk found missing is not opened and is not treated as a
/// destination that is safe to create.
pub(crate) fn vet_checkpoint_node(path: &Path) -> io::Result<CheckpointNode> {
    let policy = checkpoint_policy();
    let (lexical, resolved) = walk_checkpoint(path, &policy).map_err(tool_to_io)?;
    match fs::symlink_metadata(&resolved) {
        Ok(meta) if cloud_placeholder(&resolved, &meta) => {
            // A cloud placeholder is a file. `read_link` fails on its reparse
            // tag and must not abort the parent checkpoint.
            finish_cloud_node(&policy, &lexical, &resolved, identity_existing(&resolved))
        }
        Ok(meta) if meta.file_type().is_symlink() || reparse_unsafe(&meta) => {
            let target = fs::read_link(&resolved)?;
            classify_link_target(&resolved, &target, &policy).map_err(tool_to_io)?;
            let opened = identity_existing(&resolved).map_err(tool_to_io)?;
            Ok(finish_node(&policy, &[&lexical, &resolved, &opened]))
        }
        Ok(_) => {
            let opened = identity_existing(&resolved).map_err(tool_to_io)?;
            if !in_checkpoint_store(&lexical, &policy.data_dir)
                && !policy.roots.is_empty()
                && !roots_contain(&policy.roots, &opened)
            {
                return Err(deny_io(ROOT_MSG));
            }
            Ok(finish_node(&policy, &[&lexical, &resolved, &opened]))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            // Absence was established by the component walk, not by a
            // `CreateFile` that followed an ancestor junction and returned
            // `NotFound`. Do not open the path.
            Ok(finish_node(&policy, &[&lexical, &resolved]))
        }
        Err(e) => Err(e),
    }
}

fn finish_cloud_node(
    policy: &CpPolicy,
    lexical: &Path,
    resolved: &Path,
    identity: Result<PathBuf, ToolError>,
) -> io::Result<CheckpointNode> {
    let opened = identity.map_err(tool_to_io)?;
    if !in_checkpoint_store(lexical, &policy.data_dir)
        && !policy.roots.is_empty()
        && !roots_contain(&policy.roots, &opened)
    {
        return Err(deny_io(ROOT_MSG));
    }
    Ok(finish_node(policy, &[lexical, resolved, &opened]))
}

fn finish_node(policy: &CpPolicy, paths: &[&Path]) -> CheckpointNode {
    if protected_paths(paths, policy) {
        CheckpointNode::SkipProtected
    } else {
        CheckpointNode::Proceed
    }
}

/// Classify `path` before a checkpoint reads or records it.
///
/// A protected path is an error here. Directory copies use
/// [`vet_checkpoint_node`] so a protected descendant can be skipped instead.
pub(crate) fn vet_checkpoint_path(path: &Path) -> io::Result<()> {
    match vet_checkpoint_node(path)? {
        CheckpointNode::Proceed => Ok(()),
        CheckpointNode::SkipProtected => Err(deny_io(GRANT_MSG)),
    }
}

/// Classify a restore destination before `remove`, `fs::copy`, or `make_link`.
///
/// The ancestor walk runs first. An existing parent must be a plain directory
/// and is named with `GetFinalPathNameByHandleW`. A missing parent is allowed
/// only when the nearest existing ancestor is a plain directory inside the
/// vetted root, so restore can recreate that directory. A junction, a failed
/// identity lookup, or a path outside the roots is still a deny.
pub(crate) fn vet_checkpoint_write(path: &Path) -> io::Result<()> {
    let policy = checkpoint_policy();
    let (lexical, resolved) = walk_checkpoint(path, &policy).map_err(tool_to_io)?;
    let parent = resolved
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| deny_io("path could not be verified"))?;
    let opened = match fs::symlink_metadata(parent) {
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() && !reparse_unsafe(&meta) => {
            identity_for_write(&resolved).map_err(tool_to_io)?
        }
        Ok(_) => return Err(deny_io("path could not be verified")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let anchor = plain_existing_ancestor(parent)?;
            let opened_anchor = identity_existing(&anchor).map_err(tool_to_io)?;
            let tail = resolved
                .strip_prefix(&anchor)
                .map_err(|_| deny_io("path could not be verified"))?;
            if !tail.components().all(|c| matches!(c, Component::Normal(_))) {
                return Err(deny_io("path could not be verified"));
            }
            opened_anchor.join(tail)
        }
        Err(e) => return Err(e),
    };
    if !policy.roots.is_empty() && !roots_contain(&policy.roots, &opened) {
        return Err(deny_io(ROOT_MSG));
    }
    if protected_paths(&[&lexical, &resolved, &opened], &policy) {
        return Err(deny_io(GRANT_MSG));
    }
    Ok(())
}

/// Nearest existing plain directory at or above `start`.
///
/// A symlink, junction, or other reparse point stops the walk: missing
/// directories are not created through a link.
fn plain_existing_ancestor(start: &Path) -> io::Result<PathBuf> {
    let mut cur = start.to_path_buf();
    loop {
        match fs::symlink_metadata(&cur) {
            Ok(meta)
                if meta.is_dir() && !meta.file_type().is_symlink() && !reparse_unsafe(&meta) =>
            {
                return Ok(cur);
            }
            Ok(_) => return Err(deny_io("path could not be verified")),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                cur = cur
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .ok_or_else(|| deny_io("path could not be verified"))?
                    .to_path_buf();
            }
            Err(e) => return Err(e),
        }
    }
}

/// Components of a checkpoint `backup` or links.json `rel`.
///
/// Only plain [`Component::Normal`] names are accepted. Absolute paths, UNC
/// and other prefixes, a root, `.`, and `..` are rejected with no filesystem
/// call. `Path::join` would otherwise replace the base with a UNC string or
/// let `..` escape the store.
pub(crate) fn plain_store_components(rel: &str) -> io::Result<Vec<OsString>> {
    if rel.is_empty() || rel.contains('\0') {
        return Err(deny_io("checkpoint relative path is empty"));
    }
    reject_syntax(rel).map_err(tool_to_io)?;
    let path = Path::new(rel);
    if path.is_absolute() || path.has_root() {
        return Err(deny_io("checkpoint relative path escapes the store"));
    }
    // Split on both separators. On Unix `\` is not a separator, so
    // `..\outside` would otherwise be one Normal component.
    let mut parts = Vec::new();
    for seg in rel.split(['/', '\\']) {
        if seg.is_empty() || seg == "." || seg == ".." || component_is_device(seg) {
            return Err(deny_io("checkpoint relative path escapes the store"));
        }
        parts.push(OsString::from(seg));
    }
    if parts.is_empty() {
        return Err(deny_io("checkpoint relative path is empty"));
    }
    for c in path.components() {
        if !matches!(c, Component::Normal(_)) {
            return Err(deny_io("checkpoint relative path escapes the store"));
        }
    }
    Ok(parts)
}

fn push_store_name(out: &mut PathBuf, part: &OsString) -> io::Result<()> {
    let piece = Path::new(part);
    let mut comps = piece.components();
    match (comps.next(), comps.next()) {
        (Some(Component::Normal(name)), None) if !component_is_device(&name.to_string_lossy()) => {
            out.push(name);
            Ok(())
        }
        _ => Err(deny_io("checkpoint relative path escapes the store")),
    }
}

/// `base` plus the plain components of `rel`. Never calls `Path::join` on the raw string.
pub(crate) fn join_store_rel(base: &Path, rel: &str) -> io::Result<PathBuf> {
    let mut out = base.to_path_buf();
    for part in plain_store_components(rel)? {
        push_store_name(&mut out, &part)?;
    }
    Ok(out)
}

/// Path components of `rel` that sit strictly under `backup`, if `rel` is that prefix.
pub(crate) fn store_rel_suffix(backup: &str, rel: &str) -> io::Result<Option<Vec<OsString>>> {
    let backup_parts = plain_store_components(backup)?;
    let rel_parts = plain_store_components(rel)?;
    if rel_parts.len() < backup_parts.len() || rel_parts[..backup_parts.len()] != backup_parts[..] {
        return Ok(None);
    }
    Ok(Some(rel_parts[backup_parts.len()..].to_vec()))
}

/// `base` plus already-checked plain names.
pub(crate) fn join_store_parts(base: &Path, parts: &[OsString]) -> io::Result<PathBuf> {
    let mut out = base.to_path_buf();
    for part in parts {
        push_store_name(&mut out, part)?;
    }
    Ok(out)
}

/// Classify a link target before `make_link`. The target text is rejected
/// before the walk, so a UNC target is never created and never copied.
pub(crate) fn vet_link_target(at: &Path, target: &Path) -> io::Result<()> {
    let policy = checkpoint_policy();
    let joined = classify_link_target(at, target, &policy).map_err(tool_to_io)?;
    let _resolved = walk_checkpoint(&joined, &policy).map_err(tool_to_io)?;
    Ok(())
}

fn classify_link_target(
    link: &Path,
    target: &Path,
    policy: &CpPolicy,
) -> Result<PathBuf, ToolError> {
    let raw = target.to_string_lossy();
    if dos_target_is_remote(&raw) || classify_path_str(&raw) != Class::Local {
        return Err(denied(UNC_MSG));
    }
    let stripped = strip_local_prefix(&raw);
    if dos_target_is_remote(&stripped) || classify_path_str(&stripped) != Class::Local {
        return Err(denied(UNC_MSG));
    }
    let joined = if Path::new(&stripped).is_absolute() {
        PathBuf::from(&stripped)
    } else {
        link.parent()
            .unwrap_or_else(|| Path::new(""))
            .join(&stripped)
    };
    let joined = lexical_normalize(&joined)?;
    let joined = confine_drive(&joined)?;
    if !policy.roots.is_empty() && !roots_contain(&policy.roots, &joined) {
        return Err(denied(ROOT_MSG));
    }
    if protected_ungranted(&joined, &policy.data_dir, &policy.grants) {
        return Err(denied(GRANT_MSG));
    }
    Ok(joined)
}

fn entry_is_protected(path: &Path, data_dir: &Path, grants: &[PathBuf]) -> io::Result<bool> {
    if protected_ungranted(path, data_dir, grants) {
        return Ok(true);
    }
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
        Ok(_) => {}
    }
    let opened = identity_existing(path).map_err(tool_to_io)?;
    Ok(protected_ungranted(&opened, data_dir, grants))
}

fn is_link_meta(meta: &fs::Metadata) -> bool {
    meta.file_type().is_symlink() || reparse_unsafe(meta)
}

/// Delete `path`, leaving protected files and directories in place.
///
/// Symlinks and junctions are removed as entries. Their targets are not walked.
pub(crate) fn remove_tree_skip_protected(
    path: &Path,
    data_dir: &Path,
    grants: &[PathBuf],
) -> io::Result<()> {
    if entry_is_protected(path, data_dir, grants)? {
        return Ok(());
    }
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if is_link_meta(&meta) {
        return fs::remove_file(path).or_else(|_| fs::remove_dir(path));
    }
    if meta.is_dir() {
        for entry in fs::read_dir(path)? {
            remove_tree_skip_protected(&entry?.path(), data_dir, grants)?;
        }
        if fs::read_dir(path)?.next().is_none() {
            fs::remove_dir(path)?;
        }
        return Ok(());
    }
    fs::remove_file(path)
}

/// Authorize only generated adjacent temporary names for an already-vetted
/// destination. Exact credential grants are translated to those temporary
/// names for the duration of this checkpoint gate; no user-supplied path is
/// added and the original protection check always runs first.
pub(crate) fn vet_checkpoint_temporary(temporary: &Path, original: &Path) -> io::Result<()> {
    vet_checkpoint_write(original)?;
    let original = widen_path(original).map_err(tool_to_io)?;
    let original_parts = norm_parts(&original);
    CP_GATE.with(|slot| {
        if let Some(gate) = slot.borrow_mut().as_mut() {
            let translated: Vec<_> = gate
                .grants
                .iter()
                .filter_map(|grant| {
                    let grant = widen_path(grant).ok()?;
                    let parts = norm_parts(&grant);
                    if !parts.starts_with(&original_parts) {
                        return None;
                    }
                    let mut translated = temporary.to_path_buf();
                    for component in grant.components().skip(original_parts.len()) {
                        translated.push(component);
                    }
                    Some(translated)
                })
                .collect();
            gate.grants.extend(translated);
        }
    });
    vet_checkpoint_write(temporary)
}

/// Whether an existing directory contains entries a restore must leave in place.
pub(crate) fn checkpoint_has_protected_descendant(path: &Path) -> io::Result<bool> {
    let policy = checkpoint_policy();
    has_protected_descendant(path, &policy.data_dir, &policy.grants)
}

/// Protection lookup for one entry, without reading its content.
pub(crate) fn checkpoint_entry_is_protected(path: &Path) -> io::Result<bool> {
    let policy = checkpoint_policy();
    entry_is_protected(path, &policy.data_dir, &policy.grants)
}

/// [`remove_tree_skip_protected`] using the checkpoint gate installed for restore.
pub(crate) fn remove_checkpoint_target(path: &Path) -> io::Result<()> {
    let policy = checkpoint_policy();
    remove_tree_skip_protected(path, &policy.data_dir, &policy.grants)
}

fn has_protected_descendant(path: &Path, data_dir: &Path, grants: &[PathBuf]) -> io::Result<bool> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    if is_link_meta(&meta) || !meta.is_dir() {
        return Ok(false);
    }
    for entry in fs::read_dir(path)? {
        let child = entry?.path();
        if entry_is_protected(&child, data_dir, grants)?
            || has_protected_descendant(&child, data_dir, grants)?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn hidden_move_path(path: &Path, label: &str) -> io::Result<PathBuf> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("move target has no parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("move target has no name"))?;
    Ok(parent.join(crate::checkpoint::temp_name(".", name, label)))
}

fn cross_device(err: &io::Error) -> bool {
    #[cfg(windows)]
    const CROSS_DEVICE: i32 = 17; // ERROR_NOT_SAME_DEVICE
    #[cfg(not(windows))]
    const CROSS_DEVICE: i32 = 18; // EXDEV (supported Unix platforms)
    err.kind() == io::ErrorKind::CrossesDevices || err.raw_os_error() == Some(CROSS_DEVICE)
}

struct MoveTransaction<'a, F> {
    sh: &'a Shared,
    rename: F,
    overwrite: bool,
    renames: Vec<(PathBuf, PathBuf)>,
    discard: Vec<PathBuf>,
    stages: Vec<PathBuf>,
    created_dirs: Vec<PathBuf>,
    source_dirs: Vec<PathBuf>,
}

impl<'a, F: Fn(&Path, &Path) -> io::Result<()>> MoveTransaction<'a, F> {
    fn new(sh: &'a Shared, rename: F, overwrite: bool) -> Self {
        Self {
            sh,
            rename,
            overwrite,
            renames: Vec::new(),
            discard: Vec::new(),
            stages: Vec::new(),
            created_dirs: Vec::new(),
            source_dirs: Vec::new(),
        }
    }

    fn rename_recorded(&mut self, from: &Path, to: &Path) -> io::Result<()> {
        (self.rename)(from, to)?;
        self.renames.push((from.to_path_buf(), to.to_path_buf()));
        Ok(())
    }

    fn ensure_dir(&mut self, dir: &Path) -> io::Result<()> {
        let mut missing = Vec::new();
        let mut cursor = dir;
        loop {
            match fs::symlink_metadata(cursor) {
                Ok(meta) if meta.is_dir() && !is_link_meta(&meta) => break,
                Ok(_) => return Err(io::Error::other("move parent is not a plain directory")),
                Err(err) if err.kind() == io::ErrorKind::NotFound => {
                    missing.push(cursor.to_path_buf());
                    cursor = cursor
                        .parent()
                        .ok_or_else(|| io::Error::other("missing move parent"))?;
                }
                Err(err) => return Err(err),
            }
        }
        for path in missing.into_iter().rev() {
            fs::create_dir(&path)?;
            self.created_dirs.push(path);
        }
        Ok(())
    }

    fn save_destination(&mut self, to: &Path) -> io::Result<()> {
        match fs::symlink_metadata(to) {
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(err),
        }
        if !self.overwrite {
            // Enforced where the destination is displaced: it can appear
            // after the preflight, while the checkpoint is being taken.
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "{} already exists; set overwrite to replace it",
                    to.display()
                ),
            ));
        }
        if entry_is_protected(to, &self.sh.data_dir, &self.sh.grants)? {
            return Err(deny_io("destination contains a protected file"));
        }
        if has_protected_descendant(to, &self.sh.data_dir, &self.sh.grants)? {
            let mut children = fs::read_dir(to)?.collect::<io::Result<Vec<_>>>()?;
            children.sort_by_key(fs::DirEntry::file_name);
            for child in children {
                let path = child.path();
                if !entry_is_protected(&path, &self.sh.data_dir, &self.sh.grants)? {
                    self.save_destination(&path)?;
                }
            }
        } else {
            let hidden = hidden_move_path(to, "move-old")?;
            vet_checkpoint_temporary(&hidden, to)?;
            self.rename_recorded(to, &hidden)?;
            self.discard.push(hidden);
        }
        Ok(())
    }

    fn move_path(&mut self, from: &Path, to: &Path) -> io::Result<()> {
        if entry_is_protected(from, &self.sh.data_dir, &self.sh.grants)? {
            return Ok(());
        }
        let meta = fs::symlink_metadata(from)?;
        let protected_source = has_protected_descendant(from, &self.sh.data_dir, &self.sh.grants)?;
        let protected_target = has_protected_descendant(to, &self.sh.data_dir, &self.sh.grants)?;
        if protected_source || protected_target {
            if !meta.is_dir() || is_link_meta(&meta) {
                return Err(deny_io("destination still contains a protected file"));
            }
            if !is_dir_nofollow(to) {
                self.save_destination(to)?;
                self.ensure_dir(to)?;
            }
            // Overwrite removes old mutable names that the source will not
            // replace, while credentials remain at their original paths.
            let mut destinations = fs::read_dir(to)?.collect::<io::Result<Vec<_>>>()?;
            destinations.sort_by_key(fs::DirEntry::file_name);
            for entry in destinations {
                if fs::symlink_metadata(from.join(entry.file_name())).is_err()
                    && !entry_is_protected(&entry.path(), &self.sh.data_dir, &self.sh.grants)?
                {
                    self.save_destination(&entry.path())?;
                }
            }
            let mut entries = fs::read_dir(from)?.collect::<io::Result<Vec<_>>>()?;
            entries.sort_by_key(fs::DirEntry::file_name);
            for entry in entries {
                self.move_path(&entry.path(), &to.join(entry.file_name()))?;
            }
            self.source_dirs.push(from.to_path_buf());
            return Ok(());
        }
        let parent = to
            .parent()
            .ok_or_else(|| io::Error::other("move target has no parent"))?;
        self.ensure_dir(parent)?;
        self.save_destination(to)?;
        match self.rename_recorded(from, to) {
            Ok(()) => Ok(()),
            Err(err) if cross_device(&err) => {
                let stage = hidden_move_path(to, "move-stage")?;
                vet_checkpoint_temporary(&stage, to)?;
                self.stages.push(stage.clone());
                crate::checkpoint::copy_move_payload(from, &stage)?;
                // Preserve the complete source on its own filesystem until
                // destination publication succeeds. Never recursively delete
                // the only surviving copy as part of an uncommitted move.
                let source_old = hidden_move_path(from, "move-source")?;
                vet_checkpoint_temporary(&source_old, from)?;
                self.rename_recorded(from, &source_old)?;
                self.discard.push(source_old);
                self.rename_recorded(&stage, to)
            }
            Err(err) => Err(err),
        }
    }

    fn rollback(&mut self) -> Vec<String> {
        let mut problems = Vec::new();
        for (from, to) in self.renames.iter().rev() {
            // Empty replacement directories must disappear before an old
            // file/directory can be renamed back on Windows as well as Unix.
            for dir in self
                .created_dirs
                .iter()
                .rev()
                .filter(|dir| dir.starts_with(from))
            {
                let _ = fs::remove_dir(dir);
            }
            // An unrelated writer creating the old name must not be
            // overwritten by recovery; keep the saved bytes and report it.
            if fs::symlink_metadata(from).is_ok() {
                problems.push(format!(
                    "{} is occupied; previous bytes retained at {}",
                    from.display(),
                    to.display()
                ));
                continue;
            }
            // The injected operation hook does not affect recovery.
            if let Err(err) = fs::rename(to, from) {
                problems.push(format!(
                    "{} retained at {}: {err}",
                    from.display(),
                    to.display()
                ));
            }
        }
        for stage in &self.stages {
            let _ = crate::checkpoint::remove_staged(stage);
        }
        for dir in self.created_dirs.iter().rev() {
            let _ = fs::remove_dir(dir);
        }
        problems
    }

    fn finish(self) -> Vec<String> {
        let mut warnings = Vec::new();
        for path in self.discard {
            if let Err(err) = remove_checkpoint_target(&path) {
                warnings.push(format!(
                    "previous bytes retained at {}: {err}",
                    path.display()
                ));
            }
        }
        for dir in self.source_dirs {
            if fs::read_dir(&dir).is_ok_and(|mut entries| entries.next().is_none())
                && let Err(err) = fs::remove_dir(&dir)
            {
                warnings.push(format!(
                    "empty source directory retained at {}: {err}",
                    dir.display()
                ));
            }
        }
        warnings
    }
}

fn relocate_safely(
    sh: &Shared,
    from: &Path,
    to: &Path,
    rename: impl Fn(&Path, &Path) -> io::Result<()>,
    overwrite: bool,
) -> io::Result<Vec<String>> {
    with_checkpoint_gate(sh, || {
        let mut tx = MoveTransaction::new(sh, rename, overwrite);
        if let Err(err) = tx.move_path(from, to) {
            let rollback = tx.rollback();
            if rollback.is_empty() {
                return Err(err);
            }
            return Err(io::Error::new(
                err.kind(),
                format!("{err}; rollback incomplete: {}", rollback.join("; ")),
            ));
        }
        Ok(tx.finish())
    })
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

fn do_read(sh: &Shared, i: &ReadIn) -> Result<String, ToolError> {
    let path = validate_path(sh, &i.path, Follow::Content)?;
    let Some(r) = &i.range else {
        return read_text(&path);
    };
    let start = r.start.unwrap_or(1).max(1);
    let end = r.end.unwrap_or(usize::MAX);
    let (kept, total) = read_line_range(&path, start, end)?;
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
        let ft = e.file_type().ok();
        let is_link = ft.as_ref().is_some_and(fs::FileType::is_symlink);
        // Symlinks and other reparse points are not descended into: opening
        // them can follow a junction onto a UNC target.
        let is_dir = ft.as_ref().is_some_and(fs::FileType::is_dir)
            && !is_link
            && !fs::symlink_metadata(&p).is_ok_and(|m| reparse_unsafe(&m));
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

fn is_dir_nofollow(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink() && !reparse_unsafe(&m))
}

fn is_file_nofollow(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .is_ok_and(|m| m.is_file() && !m.file_type().is_symlink() && !reparse_unsafe(&m))
}

fn do_list(sh: &Shared, i: &ListIn) -> Result<String, ToolError> {
    let root = validate_path(sh, &i.path, Follow::Content)?;
    if !is_dir_nofollow(&root) {
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
        let rel = p.strip_prefix(&root).unwrap_or(p).display().to_string();
        let meta = fs::symlink_metadata(p).ok();
        let link = meta
            .as_ref()
            .is_some_and(|m| m.file_type().is_symlink() || reparse_unsafe(m));
        if link {
            lines.push(format!("l {rel}"));
        } else if is_dir {
            lines.push(format!("d {rel}/"));
        } else {
            let size = meta.map_or(0, |m| m.len());
            lines.push(format!("f {rel} ({size} bytes)"));
        }
        true
    };
    match &m {
        None => {
            walk(&root, 0, &mut |p, d| push(p, d));
        }
        Some(m) => {
            walk(&root, 10, &mut |p, d| {
                if glob_hit(m, &root, p) {
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

fn search_file(sh: &Shared, path: &Path, q: &str, root: &Path, out: &mut Vec<String>) {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return;
    };
    if meta.file_type().is_symlink() || reparse_unsafe(&meta) {
        return;
    }
    // The directory entry name can be the long name while the caller opened a
    // short path. Name the handle before reading bytes.
    let Ok(opened) = final_identity(path) else {
        return;
    };
    if credential_blocked(sh, path, &opened) {
        return;
    }
    if meta.len() > MAX_SEARCH_FILE_BYTES {
        return;
    }
    let Ok(bytes) = fs::read(&opened) else { return };
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

fn do_search(sh: &Shared, i: &SearchIn) -> Result<String, ToolError> {
    if i.query.is_empty() {
        return Err(ToolError::InvalidInput("query is empty".into()));
    }
    let q = i.query.to_lowercase();
    let root = validate_path(sh, &i.path, Follow::Content)?;
    let m = matcher(i.glob.as_deref())?;
    let mut out = Vec::new();
    if is_file_nofollow(&root) {
        let parent = root.parent().unwrap_or(&root);
        search_file(sh, &root, &q, parent, &mut out);
    } else if is_dir_nofollow(&root) {
        walk(&root, 12, &mut |p, is_dir| {
            if !is_dir && m.as_ref().is_none_or(|m| glob_hit(m, &root, p)) {
                search_file(sh, p, &q, &root, &mut out);
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
    fn checkpoint(
        &self,
        run: &str,
        op: &str,
        paths: &[(&Path, &Path)],
    ) -> Result<(String, String), ToolError> {
        with_checkpoint_gate(self, || {
            let id = self
                .checkpoints
                .create_for_tool(run, op, paths)
                .map_err(|err| io_to_tool(&err, "checkpoint failed, nothing changed"))?;
            let skipped = self
                .checkpoints
                .skipped(&id)
                .map_err(|err| io_to_tool(&err, "read checkpoint result, nothing changed"))?;
            if !skipped.is_empty() {
                // The undo checkpoint would be incomplete, so the operation
                // would destroy bytes that cannot be brought back.
                self.checkpoints.discard(&id);
                return Err(exec_err(format!(
                    "{op} was not run and nothing changed: {} item(s) cannot be restored if it proceeds because they could not be saved in an undo checkpoint: {}",
                    skipped.len(),
                    skipped.join("; ")
                )));
            }
            let mut note = String::new();
            let mut seen = Vec::new();
            for (_, effective) in paths {
                let mut dirs = Vec::new();
                if let Some(parent) = effective.parent() {
                    dirs.push(parent.to_path_buf());
                }
                if is_dir_nofollow(effective) {
                    dirs.push(effective.to_path_buf());
                }
                for dir in dirs {
                    if seen.contains(&dir) {
                        continue;
                    }
                    for line in crate::checkpoint::leftover_notes(&dir) {
                        let _ = write!(note, "\nwarning: {line}");
                    }
                    seen.push(dir);
                }
            }
            Ok((id, note))
        })
    }
}

fn do_write(sh: &Shared, run: &str, i: &WriteIn) -> Result<String, ToolError> {
    let p = validate_path(sh, &i.path, Follow::Write)?;
    let (cp, note) = sh.checkpoint(run, "fs_write", &[(Path::new(&i.path), &p)])?;
    if let Some(parent) = p.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(|e| exec_err(format!("create dirs: {e}")))?;
    }
    fs::write(&p, i.content.as_bytes()).map_err(|e| exec_err(format!("write {}: {e}", i.path)))?;
    Ok(format!(
        "wrote {} bytes to {} (checkpoint {cp}){note}",
        i.content.len(),
        i.path
    ))
}

fn do_edit(sh: &Shared, run: &str, i: &EditIn) -> Result<String, ToolError> {
    if i.old.is_empty() {
        return Err(ToolError::InvalidInput("old must not be empty".into()));
    }
    let p = validate_path(sh, &i.path, Follow::Content)?;
    let text = read_text(&p)?;
    let n = text.matches(&i.old).count();
    if n == 0 {
        return Err(exec_err("old text not found; read the file again"));
    }
    if n > 1 && !i.replace_all {
        return Err(exec_err(format!(
            "old text matches {n} times; add more context or set replace_all"
        )));
    }
    let (cp, note) = sh.checkpoint(run, "fs_edit", &[(Path::new(&i.path), &p)])?;
    let updated = text.replace(&i.old, &i.new);
    fs::write(&p, updated).map_err(|e| exec_err(format!("write {}: {e}", i.path)))?;
    Ok(format!(
        "replaced {n} occurrence(s) in {} (checkpoint {cp}){note}",
        i.path
    ))
}

fn move_error(err: &ToolError, cp: &str, recovery: &str) -> ToolError {
    let message = format!("{err} (checkpoint {cp})\n{recovery}");
    match err {
        ToolError::PermissionDenied(_) => ToolError::PermissionDenied(message),
        ToolError::InvalidInput(_) => ToolError::InvalidInput(message),
        _ => exec_err(message),
    }
}

fn do_move(sh: &Shared, run: &str, i: &MoveIn) -> Result<String, ToolError> {
    do_move_with(sh, run, i, |from, to| fs::rename(from, to))
}

fn do_move_with(
    sh: &Shared,
    run: &str,
    i: &MoveIn,
    rename: impl Fn(&Path, &Path) -> io::Result<()>,
) -> Result<String, ToolError> {
    let mut saved = None;
    let result = (|| {
        let from = validate_path(sh, &i.from, Follow::Entry)?;
        let to = validate_path(sh, &i.to, Follow::Place)?;
        // Cheap preflight first: a rejected move must not copy the
        // destination into a checkpoint or leave an empty receipt behind.
        if fs::symlink_metadata(&from).is_err() {
            return Err(exec_err(format!("{} does not exist", i.from)));
        }
        if fs::symlink_metadata(&to).is_ok() && !i.overwrite {
            return Err(exec_err(format!(
                "{} already exists; set overwrite to replace it",
                i.to
            )));
        }
        let (cp, note) = sh.checkpoint(
            run,
            "fs_move",
            &[(Path::new(&i.from), &from), (Path::new(&i.to), &to)],
        )?;
        saved = Some(cp.clone());
        let source = norm_parts(&from);
        let target = norm_parts(&to);
        if source == target
            || (is_dir_nofollow(&from) && target.starts_with(&source))
            || (is_dir_nofollow(&to) && source.starts_with(&target))
        {
            return Err(exec_err(
                "move source and destination are the same path or overlapping directories",
            ));
        }
        let warnings = relocate_safely(sh, &from, &to, rename, i.overwrite)
            .map_err(|err| io_to_tool(&err, "move failed"))?;
        let mut result = format!("moved {} to {} (checkpoint {cp}){note}", i.from, i.to);
        for warning in warnings {
            let _ = write!(result, "\nwarning: {warning}");
        }
        Ok(result)
    })();
    result.map_err(|err| {
        // No checkpoint means the move was rejected before anything was read
        // or changed; the error stands on its own.
        let Some(cp) = saved else {
            return err;
        };
        let recovery = if err.to_string().contains("rollback incomplete") {
            "restore with checkpoint_restore; retained backup paths are listed above"
        } else {
            "nothing was lost; restore with checkpoint_restore if needed"
        };
        move_error(&err, &cp, recovery)
    })
}

fn do_delete(sh: &Shared, run: &str, i: &DeleteIn) -> Result<String, ToolError> {
    let p = validate_path(sh, &i.path, Follow::Entry)?;
    let meta = fs::symlink_metadata(&p).map_err(|e| exec_err(format!("{}: {e}", i.path)))?;
    if meta.is_dir() && !meta.file_type().is_symlink() && !i.recursive {
        return Err(exec_err(format!(
            "{} is a directory; set recursive to delete it",
            i.path
        )));
    }
    let (cp, note) = sh.checkpoint(run, "fs_delete", &[(Path::new(&i.path), &p)])?;
    remove_tree_skip_protected(&p, &sh.data_dir, &sh.grants)
        .map_err(|e| io_to_tool(&e, "delete failed"))?;
    Ok(format!("deleted {} (checkpoint {cp}){note}", i.path))
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
        for skipped in &c.skipped {
            let _ = writeln!(out, "warning: checkpoint is partial: {skipped}");
        }
    }
    Ok(out)
}

fn do_cp_restore(sh: &Shared, id: &str) -> Result<String, ToolError> {
    let (paths, safety, warnings) = with_checkpoint_gate(sh, || {
        sh.checkpoints
            .restore_with_warnings(id)
            .map_err(|e| io_to_tool(&e, "restore failed"))
    })?;
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
        if self.shared.roots_unresolved {
            return Err(denied(ROOT_UNRESOLVED_MSG));
        }
        let sh = self.shared.clone();
        let run = ctx.run_id.clone();
        let text = match self.op {
            Op::Read => {
                let i: ReadIn = parse(input)?;
                blocking(move || do_read(&sh, &i)).await?
            }
            Op::List => {
                let i: ListIn = parse(input)?;
                blocking(move || do_list(&sh, &i)).await?
            }
            Op::Search => {
                let i: SearchIn = parse(input)?;
                blocking(move || do_search(&sh, &i)).await?
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
            bot_id: None,
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

    fn link_for_checkpoint_test(target: &Path, link: &Path, directory: bool) -> bool {
        #[cfg(unix)]
        let result = {
            let _ = directory;
            std::os::unix::fs::symlink(target, link)
        };
        #[cfg(windows)]
        let result = if directory {
            std::os::windows::fs::symlink_dir(target, link)
        } else {
            std::os::windows::fs::symlink_file(target, link)
        };
        match result {
            Ok(()) => true,
            Err(err)
                if cfg!(windows)
                    && (err.kind() == io::ErrorKind::PermissionDenied
                        || err.raw_os_error() == Some(1314)) =>
            {
                eprintln!(
                    "skipping symlink regression: Windows symlink privilege unavailable: {err}"
                );
                false
            }
            Err(err) => panic!("could not create local symlink fixture: {err}"),
        }
    }

    #[test]
    fn editing_through_file_symlink_preserves_input_and_restores_referent() {
        let data = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let sh = shared_with(data.path(), Vec::new());
        let source = work.path().join("actual.txt");
        let link = work.path().join("alias.txt");
        fs::write(&source, "original bytes").unwrap();
        if !link_for_checkpoint_test(&source, &link, false) {
            return;
        }
        #[cfg(unix)]
        let before_inode = {
            use std::os::unix::fs::MetadataExt;
            fs::symlink_metadata(&link).unwrap().ino()
        };
        do_edit(
            &sh,
            "r",
            &EditIn {
                path: link.display().to_string(),
                old: "original".into(),
                new: "changed".into(),
                replace_all: false,
            },
        )
        .unwrap();
        assert_eq!(fs::read_to_string(&source).unwrap(), "changed bytes");
        let cp = sh.checkpoints.list("r").unwrap().remove(0);
        assert!(
            cp.entries
                .iter()
                .any(|entry| entry.via_link_of.as_deref() == Some(link.to_string_lossy().as_ref()))
        );
        assert!(
            cp.entries.len() >= 2,
            "referent and link topology must both be captured"
        );
        do_cp_restore(&sh, &cp.id).unwrap();
        assert_eq!(fs::read_to_string(&source).unwrap(), "original bytes");
        assert_eq!(fs::read_link(&link).unwrap(), source);
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(
                fs::symlink_metadata(&link).unwrap().ino(),
                before_inode,
                "unchanged link was recreated"
            );
        }
        // If the link object was subsequently redirected, restoring this
        // checkpoint must restore its topology as well as the referent bytes.
        let unrelated = work.path().join("unrelated.txt");
        fs::write(&unrelated, "leave alone").unwrap();
        fs::remove_file(&link).unwrap();
        assert!(link_for_checkpoint_test(&unrelated, &link, false));
        fs::write(&source, "changed again").unwrap();
        do_cp_restore(&sh, &cp.id).unwrap();
        assert_eq!(fs::read_to_string(&source).unwrap(), "original bytes");
        assert_eq!(fs::read_link(&link).unwrap(), source);
        assert_eq!(fs::read_to_string(&unrelated).unwrap(), "leave alone");
    }

    #[test]
    fn write_through_symlinked_parent_and_dangling_file_is_undoable() {
        let data = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let sh = shared_with(data.path(), Vec::new());
        let real = work.path().join("real");
        let alias = work.path().join("alias");
        fs::create_dir(&real).unwrap();
        fs::write(real.join("file.txt"), "before").unwrap();
        if !link_for_checkpoint_test(&real, &alias, true) {
            return;
        }
        let input = alias.join("file.txt");
        do_write(
            &sh,
            "parent",
            &WriteIn {
                path: input.display().to_string(),
                content: "after".into(),
            },
        )
        .unwrap();
        let cp = sh.checkpoints.list("parent").unwrap().remove(0);
        assert!(cp.entries.iter().any(|entry| entry.via_link_of.as_deref() == Some(input.to_string_lossy().as_ref())));
        do_cp_restore(&sh, &cp.id).unwrap();
        assert_eq!(fs::read_to_string(real.join("file.txt")).unwrap(), "before");
        assert_eq!(fs::read_link(&alias).unwrap(), real);

        let missing = real.join("not-created.txt");
        let dangling = work.path().join("dangling.txt");
        if !link_for_checkpoint_test(&missing, &dangling, false) {
            return;
        }
        do_write(
            &sh,
            "dangling",
            &WriteIn {
                path: dangling.display().to_string(),
                content: "created".into(),
            },
        )
        .unwrap();
        assert_eq!(fs::read_to_string(&missing).unwrap(), "created");
        let cp = sh.checkpoints.list("dangling").unwrap().remove(0);
        do_cp_restore(&sh, &cp.id).unwrap();
        assert!(!missing.exists());
        assert_eq!(fs::read_link(&dangling).unwrap(), missing);
        do_delete(
            &sh,
            "unlink",
            &DeleteIn {
                path: dangling.display().to_string(),
                recursive: false,
            },
        )
        .unwrap();
        assert!(fs::symlink_metadata(&dangling).is_err());
        let cp = sh.checkpoints.list("unlink").unwrap().remove(0);
        do_cp_restore(&sh, &cp.id).unwrap();
        assert_eq!(fs::read_link(&dangling).unwrap(), missing);
        assert!(!missing.exists());
    }

    #[test]
    fn restore_keeps_protected_descendants_and_exact_credential_grants() {
        let data = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let dir = work.path().join("project");
        fs::create_dir(&dir).unwrap();
        let file = dir.join("ordinary.txt");
        let secret = dir.join(".env");
        fs::write(&file, "before").unwrap();
        fs::write(&secret, "PROTECTED_ORIGINAL").unwrap();
        let sh = shared_with(data.path(), Vec::new());
        let (id, _) = sh.checkpoint("r", "delete", &[(&dir, &dir)]).unwrap();
        fs::write(&file, "after").unwrap();
        do_cp_restore(&sh, &id).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "before");
        assert_eq!(fs::read_to_string(&secret).unwrap(), "PROTECTED_ORIGINAL");
        assert!(!tree_has(
            &data.path().join("checkpoints"),
            "PROTECTED_ORIGINAL"
        ));

        let granted = shared_with(data.path(), vec![secret.clone()]);
        do_write(
            &granted,
            "grant",
            &WriteIn {
                path: secret.display().to_string(),
                content: "changed".into(),
            },
        )
        .unwrap();
        let cp = granted.checkpoints.list("grant").unwrap().remove(0);
        do_cp_restore(&granted, &cp.id).unwrap();
        assert_eq!(fs::read_to_string(&secret).unwrap(), "PROTECTED_ORIGINAL");
    }

    fn move_input(from: &Path, to: &Path) -> MoveIn {
        MoveIn {
            from: from.display().to_string(),
            to: to.display().to_string(),
            overwrite: true,
        }
    }

    #[test]
    fn overlapping_moves_fail_with_checkpoint_and_intact_bytes() {
        let data = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let sh = shared_with(data.path(), Vec::new());
        let dir = work.path().join("tree");
        fs::create_dir_all(dir.join("sub")).unwrap();
        let file = dir.join("file.txt");
        fs::write(&file, "keep").unwrap();
        for (from, to) in [
            (&file, &file),
            (&dir, &dir.join("inside")),
            (&dir.join("sub"), &dir),
        ] {
            let err = do_move(&sh, "move", &move_input(from, to)).unwrap_err();
            assert!(err.to_string().contains("(checkpoint cp-"), "{err}");
            assert!(err.to_string().contains("nothing was lost"), "{err}");
            assert_eq!(fs::read_to_string(&file).unwrap(), "keep");
            assert!(dir.join("sub").is_dir());
        }
        assert_eq!(sh.checkpoints.list("move").unwrap().len(), 3);
    }

    #[test]
    fn failed_overwrite_move_restores_destination_and_reports_checkpoint() {
        let data = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let sh = shared_with(data.path(), Vec::new());
        let from = work.path().join("source.txt");
        let to = work.path().join("destination.txt");
        fs::write(&from, "source bytes").unwrap();
        fs::write(&to, "destination bytes").unwrap();
        let err = do_move_with(
            &sh,
            "move",
            &move_input(&from, &to),
            |source, destination| {
                if grant_matches(source, &from) && grant_matches(destination, &to) {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "read-only destination",
                    ));
                }
                fs::rename(source, destination)
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("(checkpoint cp-"), "{err}");
        assert!(err.to_string().contains("nothing was lost"), "{err}");
        assert_eq!(fs::read_to_string(&from).unwrap(), "source bytes");
        assert_eq!(fs::read_to_string(&to).unwrap(), "destination bytes");
        assert_eq!(fs::read_dir(work.path()).unwrap().count(), 2);
    }

    #[test]
    fn cross_volume_move_stages_and_rolls_back_failed_publication() {
        for fail_publish in [false, true] {
            let data = tempfile::tempdir().unwrap();
            let work = tempfile::tempdir().unwrap();
            let sh = shared_with(data.path(), Vec::new());
            let from = work.path().join("source.txt");
            let to = work.path().join("destination.txt");
            fs::write(&from, "source bytes").unwrap();
            fs::write(&to, "destination bytes").unwrap();
            let result = do_move_with(
                &sh,
                "move",
                &move_input(&from, &to),
                |source, destination| {
                    if grant_matches(source, &from) && grant_matches(destination, &to) {
                        return Err(io::Error::new(
                            io::ErrorKind::CrossesDevices,
                            "test cross-volume",
                        ));
                    }
                    if fail_publish
                        && source.file_name().is_some_and(|name| {
                            name.to_string_lossy().contains(".praxi-move-stage-")
                        })
                    {
                        return Err(io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "injected publication failure",
                        ));
                    }
                    fs::rename(source, destination)
                },
            );
            if fail_publish {
                let err = result.unwrap_err();
                assert!(err.to_string().contains("(checkpoint cp-"), "{err}");
                assert_eq!(fs::read_to_string(&from).unwrap(), "source bytes");
                assert_eq!(fs::read_to_string(&to).unwrap(), "destination bytes");
                assert_eq!(fs::read_dir(work.path()).unwrap().count(), 2);
            } else {
                result.unwrap();
                assert!(!from.exists());
                assert_eq!(fs::read_to_string(&to).unwrap(), "source bytes");
                assert_eq!(fs::read_dir(work.path()).unwrap().count(), 1);
            }
        }
    }

    #[test]
    fn protected_directory_move_rolls_back_previous_children_and_replaced_file() {
        let data = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let sh = shared_with(data.path(), Vec::new());
        let from = work.path().join("source");
        let to = work.path().join("destination");
        fs::create_dir(&from).unwrap();
        fs::write(from.join(".env"), "SECRET_STAYS").unwrap();
        fs::write(from.join("a.txt"), "first").unwrap();
        fs::write(from.join("b.txt"), "second").unwrap();
        fs::write(&to, "old destination file").unwrap();
        let err = do_move_with(
            &sh,
            "move",
            &move_input(&from, &to),
            |source, destination| {
                if source.file_name().is_some_and(|name| name == "b.txt") {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "second move failed",
                    ));
                }
                fs::rename(source, destination)
            },
        )
        .unwrap_err();
        assert!(!err.to_string().contains("rollback incomplete"), "{err}");
        assert_eq!(fs::read_to_string(&to).unwrap(), "old destination file");
        assert_eq!(fs::read_to_string(from.join("a.txt")).unwrap(), "first");
        assert_eq!(fs::read_to_string(from.join("b.txt")).unwrap(), "second");
        assert_eq!(
            fs::read_to_string(from.join(".env")).unwrap(),
            "SECRET_STAYS"
        );
        assert!(!tree_has(&data.path().join("checkpoints"), "SECRET_STAYS"));
        assert_eq!(fs::read_dir(work.path()).unwrap().count(), 2);
    }

    #[test]
    fn protected_destination_walk_never_revisits_its_own_backup_names() {
        let data = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let sh = shared_with(data.path(), Vec::new());
        let from = work.path().join("source");
        let to = work.path().join("destination");
        fs::create_dir(&from).unwrap();
        fs::create_dir_all(to.join("nested")).unwrap();
        fs::write(from.join("new.txt"), "new").unwrap();
        fs::write(to.join(".env"), "outer secret").unwrap();
        fs::write(to.join("nested/.env"), "inner secret").unwrap();
        // Enough entries to require several directory-iterator batches. The
        // journal creates adjacent names while clearing these old children.
        for index in 0..1200 {
            fs::write(to.join(format!("old-{index}")), "x").unwrap();
            fs::write(to.join("nested").join(format!("old-{index}")), "x").unwrap();
        }
        let renames = std::cell::Cell::new(0);
        do_move_with(
            &sh,
            "move",
            &move_input(&from, &to),
            |source, destination| {
                assert!(
                    !source
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .contains(".praxi-move-old-"),
                    "a generated backup was revisited as a source: {}",
                    source.display()
                );
                renames.set(renames.get() + 1);
                fs::rename(source, destination)
            },
        )
        .unwrap();
        assert_eq!(renames.get(), 2401);
        assert_eq!(fs::read_to_string(to.join("new.txt")).unwrap(), "new");
        assert_eq!(fs::read_to_string(to.join(".env")).unwrap(), "outer secret");
        assert_eq!(
            fs::read_to_string(to.join("nested/.env")).unwrap(),
            "inner secret"
        );
        assert_eq!(fs::read_dir(&to).unwrap().count(), 3);
        assert_eq!(fs::read_dir(to.join("nested")).unwrap().count(), 1);
    }

    #[test]
    fn partial_checkpoint_stops_delete_move_and_write_and_names_skipped_paths() {
        // Card 181 item 1: never run a destructive operation whose undo
        // checkpoint is known to be incomplete.
        let _reset = crate::checkpoint::test_hooks::Reset;
        let data = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let sh = shared_with(data.path(), Vec::new());
        let proj = work.path().join("proj");
        fs::create_dir(&proj).unwrap();
        for index in 0..20 {
            fs::write(proj.join(format!("file-{index}.txt")), "keep").unwrap();
        }
        fs::write(proj.join("offline.dat"), "cloud only").unwrap();
        crate::checkpoint::test_hooks::FORCE_SKIP
            .with(|slot| *slot.borrow_mut() = Some("offline".into()));

        let err = do_delete(
            &sh,
            "partial",
            &DeleteIn {
                path: proj.display().to_string(),
                recursive: true,
            },
        )
        .unwrap_err();
        let text = err.to_string();
        assert!(text.contains("offline.dat"), "{text}");
        assert!(text.contains("cannot be restored"), "{text}");
        assert_eq!(fs::read_dir(&proj).unwrap().count(), 21);

        let dest = work.path().join("dest");
        let err = do_move(&sh, "partial", &move_input(&proj, &dest)).unwrap_err();
        assert!(err.to_string().contains("offline.dat"), "{err}");
        assert!(!dest.exists());
        assert_eq!(fs::read_dir(&proj).unwrap().count(), 21);

        let err = do_write(
            &sh,
            "partial",
            &WriteIn {
                path: proj.join("offline.dat").display().to_string(),
                content: "new".into(),
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("offline.dat"), "{err}");
        assert_eq!(
            fs::read_to_string(proj.join("offline.dat")).unwrap(),
            "cloud only"
        );

        // Without the unreadable entry the same operations proceed.
        crate::checkpoint::test_hooks::FORCE_SKIP.with(|slot| *slot.borrow_mut() = None);
        do_delete(
            &sh,
            "partial",
            &DeleteIn {
                path: proj.display().to_string(),
                recursive: true,
            },
        )
        .unwrap();
        assert!(!proj.exists());
    }

    #[test]
    fn rejected_moves_create_no_checkpoint_and_keep_the_missing_source_message() {
        // Card 181 item 5.
        let data = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let sh = shared_with(data.path(), Vec::new());
        let from = work.path().join("source.txt");
        let to = work.path().join("destination.txt");
        fs::write(&from, "source").unwrap();
        fs::write(&to, "destination").unwrap();

        let err = do_move(
            &sh,
            "reject",
            &MoveIn {
                from: from.display().to_string(),
                to: to.display().to_string(),
                overwrite: false,
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
        assert_eq!(fs::read_to_string(&to).unwrap(), "destination");

        let missing = work.path().join("missing.txt");
        let err = do_move(
            &sh,
            "reject",
            &move_input(&missing, &work.path().join("elsewhere.txt")),
        )
        .unwrap_err();
        assert!(err.to_string().contains("does not exist"), "{err}");

        let secret = work.path().join(".env");
        let err = do_move(&sh, "reject", &move_input(&from, &secret)).unwrap_err();
        assert!(matches!(err, ToolError::PermissionDenied(_)), "{err}");
        assert_eq!(fs::read_to_string(&from).unwrap(), "source");

        assert!(
            sh.checkpoints.list("reject").unwrap().is_empty(),
            "a rejected move left a checkpoint: {:?}",
            sh.checkpoints.list("reject").unwrap()
        );
        assert!(!tree_has(&data.path().join("checkpoints"), "destination"));
    }

    #[test]
    fn interrupted_swap_leftovers_are_reported_by_the_next_destructive_tool() {
        // Card 181 item 3: a crash between the two renames leaves the data
        // beside a missing target. The next checkpoint must point at it.
        let data = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let sh = shared_with(data.path(), Vec::new());
        let restore_old = work
            .path()
            .join("report.txt.praxi-restore-old-0190aaaabbbbccccddddeeeeffff0011");
        let move_old = work
            .path()
            .join(".notes.txt.praxi-move-old-0190aaaabbbbccccddddeeeeffff0022");
        fs::write(&restore_old, "precious report").unwrap();
        fs::write(&move_old, "precious notes").unwrap();
        let other = work.path().join("other.txt");
        fs::write(&other, "x").unwrap();
        let out = do_write(
            &sh,
            "leftover",
            &WriteIn {
                path: other.display().to_string(),
                content: "y".into(),
            },
        )
        .unwrap();
        assert!(out.contains("praxi-restore-old-0190aaaa"), "{out}");
        assert!(out.contains("praxi-move-old-0190aaaa"), "{out}");
        assert!(out.contains("report.txt"), "{out}");
        assert!(out.contains("notes.txt"), "{out}");
        assert!(out.contains("rename"), "{out}");
        assert_eq!(fs::read_to_string(&restore_old).unwrap(), "precious report");
        assert_eq!(fs::read_to_string(&move_old).unwrap(), "precious notes");
    }

    #[test]
    fn cross_volume_move_keeps_a_nested_protected_link_object() {
        // Card 181 item 7: confirm the source tree is not removed together
        // with a link the checkpoint walk treats as protected.
        let data = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let sh = shared_with(data.path(), Vec::new());
        let from = work.path().join("source");
        let to = work.path().join("destination");
        fs::create_dir(&from).unwrap();
        fs::write(from.join("plain.txt"), "plain").unwrap();
        let referent = work.path().join("referent");
        fs::create_dir(&referent).unwrap();
        fs::write(referent.join("inside.txt"), "linked").unwrap();
        let link = from.join(".env");
        // A junction needs no privilege on Windows; a symlink does.
        #[cfg(windows)]
        let made = std::process::Command::new("cmd")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(&link)
            .arg(&referent)
            .output()
            .is_ok_and(|out| out.status.success());
        #[cfg(not(windows))]
        let made = link_for_checkpoint_test(&referent, &link, true);
        if !made {
            eprintln!("skipping link move regression: could not create a link");
            return;
        }
        let result = do_move_with(
            &sh,
            "link-move",
            &move_input(&from, &to),
            |source, destination| {
                if grant_matches(source, &from) && grant_matches(destination, &to) {
                    return Err(io::Error::new(
                        io::ErrorKind::CrossesDevices,
                        "test cross-volume",
                    ));
                }
                fs::rename(source, destination)
            },
        );
        let survives =
            fs::symlink_metadata(&link).is_ok() || fs::symlink_metadata(to.join(".env")).is_ok();
        assert!(
            survives,
            "protected link object vanished (move result: {result:?})"
        );
        assert_eq!(
            fs::read_to_string(referent.join("inside.txt")).unwrap(),
            "linked"
        );
    }

    #[cfg(windows)]
    #[test]
    fn junction_data_dir_keeps_its_long_final_name() {
        // Card 181 item 9: the data directory itself can be a junction whose
        // own name has an 8.3 alias.
        let base = tempfile::tempdir().unwrap();
        let real = base.path().join("real-data-target-directory");
        fs::create_dir(&real).unwrap();
        let junction = base.path().join("praxi-data-junction-directory");
        let made = std::process::Command::new("cmd")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(&junction)
            .arg(&real)
            .output()
            .is_ok_and(|out| out.status.success());
        if !made {
            eprintln!("skipping junction regression: mklink /J failed");
            return;
        }
        let long = normalize_final(&win_fs::final_path_nofollow(&junction).unwrap()).unwrap();
        let short = win_fs::short_path(&junction).unwrap();
        if !short.to_string_lossy().contains('~') {
            eprintln!("skipping 8.3 regression: volume does not generate short names");
            return;
        }
        assert!(
            same_path(&canonical_data_dir(&short), &long),
            "{} vs {}",
            canonical_data_dir(&short).display(),
            long.display()
        );
    }

    #[test]
    fn destination_created_after_preflight_is_not_displaced_without_overwrite() {
        // Review of 34e5879: `overwrite` must hold at the point of mutation,
        // not only in the preflight that runs before a slow checkpoint.
        let _reset = crate::checkpoint::test_hooks::Reset;
        let data = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let sh = shared_with(data.path(), Vec::new());
        let from = work.path().join("source.txt");
        let to = work.path().join("destination.txt");
        fs::write(&from, "source bytes").unwrap();
        let late = to.clone();
        crate::checkpoint::test_hooks::BEFORE_FILE_COPY.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move |_| {
                if !late.exists() {
                    fs::write(&late, "late arrival").unwrap();
                }
            }));
        });
        let err = do_move(
            &sh,
            "late",
            &MoveIn {
                from: from.display().to_string(),
                to: to.display().to_string(),
                overwrite: false,
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
        assert_eq!(fs::read_to_string(&from).unwrap(), "source bytes");
        assert_eq!(fs::read_to_string(&to).unwrap(), "late arrival");
        assert_eq!(fs::read_dir(work.path()).unwrap().count(), 2);
    }

    #[test]
    fn blocked_operation_leaves_no_checkpoint_behind() {
        let _reset = crate::checkpoint::test_hooks::Reset;
        let data = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let sh = shared_with(data.path(), Vec::new());
        let proj = work.path().join("proj");
        fs::create_dir(&proj).unwrap();
        fs::write(proj.join("keep.txt"), "keep").unwrap();
        fs::write(proj.join("offline.dat"), "cloud only").unwrap();
        crate::checkpoint::test_hooks::FORCE_SKIP
            .with(|slot| *slot.borrow_mut() = Some("offline".into()));
        for _ in 0..2 {
            do_delete(
                &sh,
                "blocked",
                &DeleteIn {
                    path: proj.display().to_string(),
                    recursive: true,
                },
            )
            .unwrap_err();
        }
        assert_eq!(sh.checkpoints.list("blocked").unwrap().len(), 0);
        let run_dir = data.path().join("checkpoints").join("blocked");
        let left = fs::read_dir(&run_dir).map_or(0, Iterator::count);
        assert_eq!(left, 0, "blocked operations left checkpoint directories");
        assert_eq!(fs::read_to_string(proj.join("keep.txt")).unwrap(), "keep");
    }

    #[test]
    fn protected_files_do_not_count_as_skipped_for_delete_and_move() {
        let data = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let sh = shared_with(data.path(), Vec::new());
        for (name, moving) in [("del", false), ("mov", true)] {
            let proj = work.path().join(name);
            fs::create_dir(&proj).unwrap();
            fs::write(proj.join(".env"), "SECRET").unwrap();
            fs::write(proj.join("a.txt"), "a").unwrap();
            fs::write(proj.join("b.txt"), "b").unwrap();
            if moving {
                let dest = work.path().join("moved");
                do_move(&sh, name, &move_input(&proj, &dest)).unwrap();
                assert_eq!(fs::read_to_string(dest.join("a.txt")).unwrap(), "a");
            } else {
                do_delete(
                    &sh,
                    name,
                    &DeleteIn {
                        path: proj.display().to_string(),
                        recursive: true,
                    },
                )
                .unwrap();
                assert!(!proj.join("a.txt").exists());
            }
            assert_eq!(fs::read_to_string(proj.join(".env")).unwrap(), "SECRET");
            let cps = sh.checkpoints.list(name).unwrap();
            assert_eq!(cps.len(), 1);
            assert!(cps[0].skipped.is_empty(), "{:?}", cps[0].skipped);
        }
    }

    #[test]
    fn cloud_identity_failure_is_denied_instead_of_using_lexical_name() {
        let work = tempfile::tempdir().unwrap();
        let policy = CpPolicy {
            data_dir: work.path().join("data"),
            roots: Vec::new(),
            grants: Vec::new(),
            roots_unresolved: false,
        };
        let alias = work.path().join("ENV~1");
        let err =
            finish_cloud_node(&policy, &alias, &alias, Err(denied("identity denied"))).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(err.to_string().contains("identity denied"));
        assert_eq!(
            finish_cloud_node(&policy, &alias, &alias, Ok(work.path().join(".env"))).unwrap(),
            CheckpointNode::SkipProtected
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn missing_data_directory_under_short_ancestor_is_normalized_before_first_use() {
        let work = tempfile::tempdir().unwrap();
        let parent = work.path().join("Long Praxi Data Directory Parent");
        fs::create_dir(&parent).unwrap();
        let long_parent = normalize_final(&fs::canonicalize(&parent).unwrap()).unwrap();
        let short_parent = win_fs::short_path(&parent).unwrap();
        if !short_parent.to_string_lossy().contains('~') {
            eprintln!("skipping 8.3 regression: volume does not generate short names");
            return;
        }
        let missing = short_parent.join("not-yet-created").join("nested");
        let expected = long_parent.join("not-yet-created").join("nested");
        assert!(same_path(&canonical_data_dir(&missing), &expected));
        let t = tools(&missing);
        fs::create_dir_all(expected.join("vault")).unwrap();
        for path in [
            expected.join("devices.json"),
            expected.join("vault/note.txt"),
        ] {
            fs::write(&path, "secret").unwrap();
            let err = get(&t, "fs_read")
                .call(&ctx(), json!({"path": path}))
                .await
                .unwrap_err();
            assert!(matches!(err, ToolError::PermissionDenied(_)), "{err}");
        }
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

    #[test]
    fn unc_and_device_prefixes_are_classified_without_io() {
        let remote = [
            r"\\server\share",
            r"\\server\share\file",
            "//server/share",
            "//server/share/file",
            r"\\?\UNC\server\share\file",
            r"\\?\unc\server\share",
            "//?/UNC/server/share",
            r"\\server",
            r"\??\UNC\server\share",
            r"\Device\Mup\server\share",
            r"\Device\LanmanRedirector\server\share",
        ];
        for p in remote {
            assert_eq!(classify_path_str(p), Class::Unc, "{p}");
        }
        let device = [
            r"\\.\PhysicalDrive0",
            r"\\.\C:",
            "//./COM1",
            r"\\?\GLOBALROOT\Device\Harddisk0\Partition0",
            r"\\?\Volume{12345678-1234-1234-1234-123456789abc}\",
            r"\\?\",
            r"\Device\HarddiskVolume1\",
            r"\??\PhysicalDrive0",
        ];
        for p in device {
            assert_eq!(classify_path_str(p), Class::Device, "{p}");
        }
        let local = [
            r"C:\Windows\notepad.exe",
            "C:/Windows/notepad.exe",
            r"\\?\C:\Windows\notepad.exe",
            "//?/C:/Windows/notepad.exe",
            r"\??\C:\Windows\notepad.exe",
            "foo/bar",
            "./foo",
            "/etc/hosts",
        ];
        for p in local {
            assert_eq!(classify_path_str(p), Class::Local, "{p}");
        }
    }

    #[test]
    fn dos_device_names_and_mapped_drive_targets_are_classified_without_io() {
        for n in [
            "NUL", "nul", "CON", "PRN", "AUX", "COM1", "LPT9", "NUL.", "NUL ", "NUL:", "CONIN$",
            "CONOUT$",
        ] {
            #[cfg(windows)]
            assert!(component_is_device(n), "{n}");
            #[cfg(not(windows))]
            assert!(!component_is_device(n), "{n} is a normal name off Windows");
        }
        for n in ["NULL", "COM", "COM0", "LPT0", "COM10", "file.txt", ".env"] {
            assert!(!component_is_device(n), "{n}");
        }
        assert!(dos_target_is_remote(r"\??\UNC\evil\share"));
        assert!(dos_target_is_remote(
            r"\Device\LanmanRedirector\;Z:0000000000000000\evil\share"
        ));
        assert!(dos_target_is_remote(r"\Device\Mup\evil\share"));
        assert!(dos_target_is_remote(r"\\evil\share"));
        assert!(!dos_target_is_remote(r"\??\C:\Users\me"));
        assert!(!dos_target_is_remote(r"\Device\HarddiskVolume3"));
        let snap = snapshot_from(
            "HKEY_CURRENT_USER\\Network\\Z\r\nHKEY_CURRENT_USER\\Network\\Y\r\n",
            "S:\\: => C:\\work\r\nT:\\: => \\\\evil\\share\r\n",
        );
        assert!(snap.mapped.contains(&'Z') && snap.mapped.contains(&'Y'));
        assert!(snap.mapped.contains(&'T'), "UNC subst is a mapped drive");
        assert!(!snap.mapped.contains(&'S'));
        assert_eq!(
            snap.subst.get(&'S').map(PathBuf::as_path),
            Some(Path::new(r"C:\work"))
        );
    }

    /// Directory symlink, or a junction on Windows (no extra privilege).
    fn link_dir(target: &Path, link: &Path) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link)
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            let out = std::process::Command::new("cmd")
                .args(["/c", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .creation_flags(CREATE_NO_WINDOW)
                .stdin(std::process::Stdio::null())
                .output()?;
            if out.status.success() {
                Ok(())
            } else {
                Err(std::io::Error::other(
                    String::from_utf8_lossy(&out.stderr).to_string(),
                ))
            }
        }
    }

    #[tokio::test]
    async fn unc_device_and_dos_names_are_refused_before_open() {
        let (_d, _w, t) = setup();
        let c = ctx();
        let common = [
            r"\\192.0.2.1\share\file",
            r"\\?\UNC\192.0.2.1\share\file",
            "//192.0.2.1/share/file",
            r"\\.\PhysicalDrive0",
            r"\\.\NUL",
        ];
        // `NUL` and `CON` are ordinary file names on Unix. On Windows they are
        // DOS devices and are still refused before any open.
        #[cfg(windows)]
        let dos_names: &[&str] = &["NUL", "CON"];
        #[cfg(not(windows))]
        let dos_names: &[&str] = &[];
        for path in common.into_iter().chain(dos_names.iter().copied()) {
            let started = std::time::Instant::now();
            let err = get(&t, "fs_read")
                .call(&c, json!({"path": path}))
                .await
                .unwrap_err();
            assert!(
                started.elapsed() < std::time::Duration::from_secs(2),
                "{path} took {:?}; a stat would have waited on the network",
                started.elapsed()
            );
            assert!(
                matches!(err, ToolError::PermissionDenied(_)),
                "{path}: {err}"
            );
            assert!(!err.to_string().contains("PRAXI"), "{path}");
        }
        for (name, input) in [
            ("fs_list", json!({"path": r"\\192.0.2.1\share"})),
            (
                "fs_search",
                json!({"query": "x", "path": r"\\192.0.2.1\share"}),
            ),
            (
                "fs_write",
                json!({"path": r"\\192.0.2.1\share\file", "content": "x"}),
            ),
        ] {
            let err = get(&t, name).call(&c, input).await.unwrap_err();
            assert!(
                matches!(err, ToolError::PermissionDenied(_)),
                "{name}: {err}"
            );
        }
    }

    #[tokio::test]
    async fn daemon_state_files_in_the_data_dir_are_protected_for_read_and_write() {
        let (d, w, t) = setup();
        let c = ctx();
        let secret = "PRAXI_SENTINEL_state_files";
        for name in [
            "control-latch.json",
            "bot.db",
            "bot.db-wal",
            "bot.db-shm",
            "bot.db-journal",
            "policy.json",
            "policy.json.tmp",
            ".tmpAbC123",
            "schedules.db",
            "schedules.db-wal",
            "memory.db",
            "memory.db-shm",
            "memory.db-journal",
            "host.toml",
            "config.toml",
            "mcp.toml",
            "providers.toml",
            "web.toml",
            "telegram_state.json",
            "telegram-inbox.jsonl",
        ] {
            let file = d.path().join(name);
            fs::write(&file, secret).unwrap();
            let err = get(&t, "fs_read")
                .call(&c, json!({"path": &file}))
                .await
                .unwrap_err();
            assert!(
                matches!(err, ToolError::PermissionDenied(_)),
                "read {name}: {err}"
            );
            assert!(!err.to_string().contains(secret), "{name}: {err}");
            let err = get(&t, "fs_write")
                .call(&c, json!({"path": &file, "content": "x"}))
                .await
                .unwrap_err();
            assert!(
                matches!(err, ToolError::PermissionDenied(_)),
                "write {name}: {err}"
            );
            assert_eq!(fs::read_to_string(&file).unwrap(), secret, "{name}");
        }
        // The same names elsewhere (a user's own project) are ordinary files.
        let project = w.path().join("policy.json");
        fs::write(&project, "{}").unwrap();
        assert!(
            get(&t, "fs_read")
                .call(&c, json!({"path": &project}))
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn daemon_state_directories_are_protected_but_bot_authored_ones_are_not() {
        let (d, _w, t) = setup();
        let c = ctx();
        for dir in ["peer", "bots", "profiles", "runs"] {
            let file = d.path().join(dir).join("x.toml");
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(&file, "secret").unwrap();
            let err = get(&t, "fs_read")
                .call(&c, json!({"path": &file}))
                .await
                .unwrap_err();
            assert!(
                matches!(err, ToolError::PermissionDenied(_)),
                "{dir}: {err}"
            );
            let err = get(&t, "fs_write")
                .call(&c, json!({"path": &file, "content": "x"}))
                .await
                .unwrap_err();
            assert!(
                matches!(err, ToolError::PermissionDenied(_)),
                "{dir}: {err}"
            );
        }
        // Bot-authored by design (skills, workflows) or bot output: ordinary.
        for dir in ["skills", "workflows", "screenshots", "files"] {
            let file = d.path().join(dir).join("note.txt");
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(&file, "ok").unwrap();
            assert!(
                get(&t, "fs_read")
                    .call(&c, json!({"path": &file}))
                    .await
                    .is_ok(),
                "{dir} must stay usable"
            );
        }
    }

    #[test]
    fn trailing_dots_spaces_and_streams_cannot_name_a_protected_file() {
        let d = tempfile::tempdir().unwrap();
        for name in [
            "control-latch.json.",
            "control-latch.json ",
            "control-latch.json. .",
            "CONTROL-LATCH.JSON",
            "bot.db::$DATA",
            "policy.json:stream",
            "schedules.db-wal.",
            "host.toml ",
        ] {
            assert!(
                is_protected(&d.path().join(name), d.path()),
                "{name:?} must resolve to a protected name"
            );
        }
        assert!(is_protected(
            &d.path().join("bots. ").join("a.toml"),
            d.path()
        ));
        assert!(!is_protected(&d.path().join("notes.txt."), d.path()));
    }

    #[tokio::test]
    async fn a_not_yet_existing_protected_name_with_a_trailing_dot_cannot_be_created() {
        let (d, _w, t) = setup();
        let c = ctx();
        for name in ["control-latch.json.", "policy.json ", "mcp.toml."] {
            let file = d.path().join(name);
            let err = get(&t, "fs_write")
                .call(&c, json!({"path": &file, "content": "forged"}))
                .await
                .unwrap_err();
            assert!(
                matches!(err, ToolError::PermissionDenied(_)),
                "{name:?}: {err}"
            );
            assert!(!d.path().join("control-latch.json").exists());
            assert!(!d.path().join("policy.json").exists());
            assert!(!d.path().join("mcp.toml").exists());
        }
    }

    #[tokio::test]
    async fn credential_paths_require_an_explicit_grant() {
        let (d, w, t) = setup();
        let c = ctx();
        let secret = "PRAXI_SENTINEL_c102";
        for name in [
            ".env",
            ".env.local",
            "api-token",
            "praxi.env",
            "praxi-node.env",
            "secrets.enc",
            "identity.key",
        ] {
            let file = w.path().join(name);
            fs::write(&file, secret).unwrap();
            let err = get(&t, "fs_read")
                .call(&c, json!({"path": &file}))
                .await
                .unwrap_err();
            assert!(
                matches!(err, ToolError::PermissionDenied(_)),
                "{name}: {err}"
            );
            assert!(!err.to_string().contains(secret), "{name}: {err}");
            assert_eq!(fs::read_to_string(&file).unwrap(), secret);
        }
        let fresh = w.path().join(".env");
        fs::remove_file(&fresh).unwrap();
        let err = get(&t, "fs_write")
            .call(&c, json!({"path": &fresh, "content": secret}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::PermissionDenied(_)), "{err}");
        assert!(!fresh.exists(), "refused write must not create .env");

        let vault = d.path().join("vault");
        fs::create_dir_all(&vault).unwrap();
        let note = vault.join("note.txt");
        fs::write(&note, secret).unwrap();
        let err = get(&t, "fs_read")
            .call(&c, json!({"path": &note}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::PermissionDenied(_)), "{err}");

        let inside = d.path().join("devices.json");
        fs::write(&inside, secret).unwrap();
        let err = get(&t, "fs_read")
            .call(&c, json!({"path": &inside}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::PermissionDenied(_)), "{err}");

        // A project file with the same name is not a daemon credential.
        let outside = w.path().join("devices.json");
        fs::write(&outside, "project-devices").unwrap();
        let ok = text(
            &get(&t, "fs_read")
                .call(&c, json!({"path": &outside}))
                .await
                .unwrap(),
        );
        assert!(ok.contains("project-devices"), "{ok}");

        fs::write(w.path().join("readme.txt"), "ordinary notes").unwrap();
        fs::write(w.path().join(".env"), secret).unwrap();
        let searched = text(
            &get(&t, "fs_search")
                .call(&c, json!({"query": secret, "path": w.path()}))
                .await
                .unwrap(),
        );
        assert!(
            !searched.contains(secret),
            "search must not return credential contents: {searched}"
        );

        let envf = w.path().join(".env");
        let granted = tools_bounded(d.path(), &[], std::slice::from_ref(&envf));
        let ok = text(
            &get(&granted, "fs_read")
                .call(&c, json!({"path": &envf}))
                .await
                .unwrap(),
        );
        assert!(ok.contains(secret), "{ok}");
        let token = w.path().join("api-token");
        let err = get(&granted, "fs_read")
            .call(&c, json!({"path": &token}))
            .await
            .unwrap_err();
        assert!(
            matches!(err, ToolError::PermissionDenied(_)),
            "a grant is exact: {err}"
        );
    }

    #[tokio::test]
    async fn allowed_roots_block_dotdot_and_symlink_escape() {
        let (d, w, _t) = setup();
        let root = w.path().join("root");
        let sub = root.join("sub");
        fs::create_dir_all(&sub).unwrap();
        fs::write(root.join("ok.txt"), "safe").unwrap();
        let outside = w.path().join("outside.txt");
        let secret = "PRAXI_OUTSIDE_c102";
        fs::write(&outside, secret).unwrap();
        let t = tools_bounded(d.path(), std::slice::from_ref(&root), &[]);
        let c = ctx();
        let ok = text(
            &get(&t, "fs_read")
                .call(&c, json!({"path": root.join("ok.txt")}))
                .await
                .unwrap(),
        );
        assert!(ok.contains("safe"), "{ok}");
        let via_dot = text(
            &get(&t, "fs_read")
                .call(&c, json!({"path": sub.join("..").join("ok.txt")}))
                .await
                .unwrap(),
        );
        assert!(via_dot.contains("safe"), "{via_dot}");

        for escaped in [outside.clone(), root.join("..").join("outside.txt")] {
            let err = get(&t, "fs_read")
                .call(&c, json!({"path": &escaped}))
                .await
                .unwrap_err();
            assert!(
                matches!(err, ToolError::PermissionDenied(_)),
                "{}: {err}",
                escaped.display()
            );
            assert!(!err.to_string().contains(secret), "{err}");
        }
        let err = get(&t, "fs_write")
            .call(
                &c,
                json!({"path": root.join("..").join("planted.txt"), "content": "x"}),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::PermissionDenied(_)), "{err}");
        assert!(!w.path().join("planted.txt").exists());

        let outside_dir = w.path().join("outside_dir");
        fs::create_dir_all(&outside_dir).unwrap();
        fs::write(outside_dir.join("secret.txt"), secret).unwrap();
        let leak = root.join("leak");
        link_dir(&outside_dir, &leak).expect("directory link");
        let err = get(&t, "fs_read")
            .call(&c, json!({"path": leak.join("secret.txt")}))
            .await
            .unwrap_err();
        assert!(
            matches!(err, ToolError::PermissionDenied(_)),
            "reparse escape: {err}"
        );
        assert!(!err.to_string().contains(secret), "{err}");
        let searched = text(
            &get(&t, "fs_search")
                .call(&c, json!({"query": secret, "path": &root}))
                .await
                .unwrap(),
        );
        assert!(
            !searched.contains(secret),
            "search followed a reparse point: {searched}"
        );
        let nested = root.join("nested");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("ok.txt"), "safe").unwrap();
        let alias = root.join("alias");
        link_dir(&nested, &alias).expect("in-root directory link");
        let ok = text(
            &get(&t, "fs_read")
                .call(&c, json!({"path": alias.join("ok.txt")}))
                .await
                .unwrap(),
        );
        assert!(ok.contains("safe"), "in-root reparse: {ok}");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_local_files_work_and_unc_is_not_opened() {
        let (d, w, t) = setup();
        let file = w.path().join("local.txt");
        fs::write(&file, "local-ok").unwrap();
        let c = ctx();
        let ok = text(
            &get(&t, "fs_read")
                .call(&c, json!({"path": &file}))
                .await
                .unwrap(),
        );
        assert!(ok.contains("local-ok"), "{ok}");
        for path in [
            r"\\192.0.2.1\share\file",
            r"\\?\UNC\192.0.2.1\share\a",
            r"\\.\PhysicalDrive0",
            "//192.0.2.1/share/a",
        ] {
            let started = std::time::Instant::now();
            let err = get(&t, "fs_read")
                .call(&c, json!({"path": path}))
                .await
                .unwrap_err();
            assert!(
                started.elapsed() < std::time::Duration::from_secs(2),
                "{path} took {:?}",
                started.elapsed()
            );
            assert!(
                matches!(err, ToolError::PermissionDenied(_)),
                "{path}: {err}"
            );
        }
        let _ = d;
    }

    fn shared_with(data: &Path, grants: Vec<PathBuf>) -> Shared {
        Shared {
            checkpoints: crate::checkpoint::CheckpointStore::new(data),
            spill_dir: data.join("spill"),
            data_dir: data.to_path_buf(),
            allowed_roots: Vec::new(),
            grants,
            roots_unresolved: false,
            drives: DriveCache::new(),
        }
    }

    fn tree_has(root: &Path, needle: &str) -> bool {
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(rd) = fs::read_dir(&dir) else {
                continue;
            };
            for e in rd.flatten() {
                let p = e.path();
                let Ok(meta) = fs::symlink_metadata(&p) else {
                    continue;
                };
                if meta.file_type().is_symlink() {
                    continue;
                }
                if meta.is_dir() {
                    stack.push(p);
                } else if fs::read(&p).is_ok_and(|b| String::from_utf8_lossy(&b).contains(needle)) {
                    return true;
                }
            }
        }
        false
    }

    /// Directory reparse point whose target is `\\192.0.2.1\share`.
    fn remote_link(link: &Path) {
        #[cfg(windows)]
        win_fs::mount_unc(link).expect("unc mount point");
        #[cfg(not(windows))]
        link_dir(Path::new(r"\\192.0.2.1\share"), link).expect("remote link");
    }

    fn plant_manifest(data: &Path, run: &str, path: &str) -> String {
        let id = format!("cp-{}", uuid::Uuid::now_v7().simple());
        let dir = data.join("checkpoints").join(run).join(&id);
        fs::create_dir_all(dir.join("files")).unwrap();
        let cp = crate::checkpoint::Checkpoint {
            id: id.clone(),
            run_id: run.into(),
            created_ms: 0,
            op: "forged".into(),
            entries: vec![crate::checkpoint::CheckpointEntry {
                path: path.into(),
                existed: true,
                is_dir: false,
                backup: Some("files/0".into()),
                size: None,
                sha256: None,
                via_link_of: None,
            }],
            skipped: Vec::new(),
            links_sha256: None,
        };
        fs::write(dir.join("manifest.json"), serde_json::to_vec(&cp).unwrap()).unwrap();
        id
    }

    #[test]
    fn grant_must_cover_each_protected_path() {
        let data = tempfile::tempdir().unwrap();
        let notes = data.path().join("notes.txt");
        let envf = data.path().join(".env");
        let token = data.path().join("api-token");
        let sh = shared_with(data.path(), vec![notes.clone()]);
        assert!(
            credential_blocked(&sh, &notes, &envf),
            "a grant of the lexical path must not authorize a resolved .env"
        );
        let sh_env = shared_with(data.path(), vec![envf.clone()]);
        assert!(
            credential_blocked(&sh_env, &envf, &token),
            "a grant of .env must not authorize api-token"
        );
        assert!(
            !credential_blocked(&sh_env, &notes, &envf),
            "a grant of the resolved .env covers that path when the lexical name is not protected"
        );
    }

    #[tokio::test]
    async fn unresolved_allowed_root_does_not_disable_the_limit() {
        let (d, w, _) = setup();
        let missing = w.path().join("missing-root");
        let t = tools_bounded(d.path(), std::slice::from_ref(&missing), &[]);
        let file = w.path().join("plain.txt");
        let marker = "visible-if-roots-dropped";
        fs::write(&file, marker).unwrap();
        let err = get(&t, "fs_read")
            .call(&ctx(), json!({"path": &file}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::PermissionDenied(_)), "{err}");
        assert!(!err.to_string().contains(marker), "{err}");

        let root = w.path().join("root");
        fs::create_dir_all(&root).unwrap();
        let inside = root.join("ok.txt");
        fs::write(&inside, "inside").unwrap();
        let ok_tools = tools_bounded(d.path(), std::slice::from_ref(&root), &[]);
        let ok = text(
            &get(&ok_tools, "fs_read")
                .call(&ctx(), json!({"path": &inside}))
                .await
                .unwrap(),
        );
        assert!(ok.contains("inside"), "{ok}");
        let mixed = tools_bounded(d.path(), &[root, missing], &[]);
        let err = get(&mixed, "fs_read")
            .call(&ctx(), json!({"path": &inside}))
            .await
            .unwrap_err();
        assert!(
            matches!(err, ToolError::PermissionDenied(_)),
            "one unresolved root must not leave the others in force: {err}"
        );
    }

    #[tokio::test]
    async fn checkpoint_paths_are_classified_before_copy() {
        let (d, w, t) = setup();
        let c = ctx();
        let secret = "PRAXI_SENTINEL_cp";
        let envf = w.path().join(".env");
        fs::write(&envf, secret).unwrap();
        let devices = d.path().join("devices.json");
        fs::write(&devices, secret).unwrap();
        let id_unc = plant_manifest(d.path(), "run-1", r"\\192.0.2.1\share\file");
        let started = std::time::Instant::now();
        let err = get(&t, "checkpoint_restore")
            .call(&c, json!({"id": id_unc}))
            .await
            .unwrap_err();
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        assert!(matches!(err, ToolError::PermissionDenied(_)), "{err}");

        for path in [envf.display().to_string(), devices.display().to_string()] {
            let id = plant_manifest(d.path(), "run-1", &path);
            let err = get(&t, "checkpoint_restore")
                .call(&c, json!({"id": id}))
                .await
                .unwrap_err();
            assert!(
                matches!(err, ToolError::PermissionDenied(_)),
                "{path}: {err}"
            );
        }
        assert_eq!(fs::read_to_string(&envf).unwrap(), secret);
        assert!(
            !tree_has(&d.path().join("checkpoints"), secret),
            "credential bytes were copied into a checkpoint blob"
        );

        let dir = w.path().join("tree");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("keep.txt"), "keep").unwrap();
        remote_link(&dir.join("junc"));
        let started = std::time::Instant::now();
        let err = get(&t, "fs_delete")
            .call(&c, json!({"path": &dir, "recursive": true}))
            .await
            .unwrap_err();
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        assert!(matches!(err, ToolError::PermissionDenied(_)), "{err}");
        assert!(
            dir.join("keep.txt").exists(),
            "delete ran after a remote link"
        );
    }

    fn plant_blob(data: &Path, run: &str, path: &str, blob: &str) -> String {
        let id = format!("cp-{}", uuid::Uuid::now_v7().simple());
        let dir = data.join("checkpoints").join(run).join(&id);
        fs::create_dir_all(dir.join("files")).unwrap();
        fs::write(dir.join("files").join("0"), blob).unwrap();
        let cp = crate::checkpoint::Checkpoint {
            id: id.clone(),
            run_id: run.into(),
            created_ms: 0,
            op: "forged".into(),
            entries: vec![crate::checkpoint::CheckpointEntry {
                path: path.into(),
                existed: true,
                is_dir: false,
                backup: Some("files/0".into()),
                size: None,
                sha256: None,
                via_link_of: None,
            }],
            skipped: Vec::new(),
            links_sha256: None,
        };
        fs::write(dir.join("manifest.json"), serde_json::to_vec(&cp).unwrap()).unwrap();
        id
    }

    #[tokio::test]
    async fn ancestor_junction_is_classified_before_restore_write() {
        let (d, w, t) = setup();
        let c = ctx();
        let secret = "PRAXI_SENTINEL_ancestor";
        let tree = w.path().join("tree");
        fs::create_dir_all(&tree).unwrap();
        remote_link(&tree.join("link"));
        let through = tree.join("link").join("file");
        let id = plant_blob(d.path(), "run-1", &through.display().to_string(), secret);
        let started = std::time::Instant::now();
        let err = get(&t, "checkpoint_restore")
            .call(&c, json!({"id": id}))
            .await
            .unwrap_err();
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "ancestor junction was opened: {:?}",
            started.elapsed()
        );
        assert!(matches!(err, ToolError::PermissionDenied(_)), "{err}");
        assert!(
            !tree_has(w.path(), secret),
            "restore wrote the blob through the ancestor junction"
        );

        let missing = w.path().join("no-such-parent").join("file.txt");
        let id = plant_blob(d.path(), "run-1", &missing.display().to_string(), secret);
        get(&t, "checkpoint_restore")
            .call(&c, json!({"id": id}))
            .await
            .unwrap();
        assert_eq!(
            fs::read_to_string(&missing).unwrap(),
            secret,
            "a deleted parent inside the vetted tree was not recreated"
        );

        let root = w.path().join("root");
        let proj = root.join("proj");
        fs::create_dir_all(&proj).unwrap();
        let outside = w.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret.txt"), "OUTSIDE").unwrap();
        let limited = tools_bounded(d.path(), std::slice::from_ref(&root), &[]);
        let id = format!("cp-{}", uuid::Uuid::now_v7().simple());
        let cp_dir = d.path().join("checkpoints").join("run-1").join(&id);
        fs::create_dir_all(cp_dir.join("files").join("0")).unwrap();
        fs::write(
            cp_dir.join("files").join("0").join("keep.txt"),
            "from-backup",
        )
        .unwrap();
        let cp = crate::checkpoint::Checkpoint {
            id: id.clone(),
            run_id: "run-1".into(),
            created_ms: 0,
            op: "forged".into(),
            entries: vec![crate::checkpoint::CheckpointEntry {
                path: proj.display().to_string(),
                existed: true,
                is_dir: true,
                backup: Some("files/0".into()),
                size: None,
                sha256: None,
                via_link_of: None,
            }],
            skipped: Vec::new(),
            links_sha256: None,
        };
        fs::write(
            cp_dir.join("manifest.json"),
            serde_json::to_vec(&cp).unwrap(),
        )
        .unwrap();
        let links = json!([{
            "rel": "files/0/junc",
            "target": outside.display().to_string(),
            "dir": true
        }]);
        fs::write(
            cp_dir.join("links.json"),
            serde_json::to_vec(&links).unwrap(),
        )
        .unwrap();
        let out = text(
            &get(&limited, "checkpoint_restore")
                .call(&c, json!({"id": id}))
                .await
                .unwrap(),
        );
        assert!(
            fs::read_to_string(proj.join("keep.txt"))
                .unwrap()
                .contains("from-backup"),
            "restore did not reach the link records: {out}"
        );
        assert!(
            !proj.join("junc").exists(),
            "junction was created before its target was classified: {out}"
        );
        assert!(outside.join("secret.txt").is_file());
    }

    #[tokio::test]
    async fn delete_and_move_leave_protected_descendants() {
        let (d, w, t) = setup();
        let c = ctx();
        let secret = "PRAXI_SENTINEL_keepenv";
        let dir = w.path().join("proj");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("keep.txt"), "keep").unwrap();
        fs::write(dir.join(".env"), secret).unwrap();
        get(&t, "fs_delete")
            .call(&c, json!({"path": &dir, "recursive": true}))
            .await
            .unwrap();
        assert!(!dir.join("keep.txt").exists(), "keep.txt was not deleted");
        assert_eq!(fs::read_to_string(dir.join(".env")).unwrap(), secret);
        assert!(
            !tree_has(&d.path().join("checkpoints"), secret),
            "credential bytes were copied into a checkpoint blob"
        );

        let env_only = w.path().join(".env");
        fs::write(&env_only, secret).unwrap();
        let err = get(&t, "fs_delete")
            .call(&c, json!({"path": &env_only}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::PermissionDenied(_)), "{err}");
        assert_eq!(fs::read_to_string(&env_only).unwrap(), secret);

        let src = w.path().join("proj-move");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("keep.txt"), "keep").unwrap();
        fs::write(src.join(".env"), secret).unwrap();
        let dest = w.path().join("proj-dest");
        get(&t, "fs_move")
            .call(&c, json!({"from": &src, "to": &dest}))
            .await
            .unwrap();
        assert_eq!(fs::read_to_string(dest.join("keep.txt")).unwrap(), "keep");
        assert!(!dest.join(".env").exists(), ".env moved with the directory");
        assert_eq!(fs::read_to_string(src.join(".env")).unwrap(), secret);
        assert!(
            !tree_has(&d.path().join("checkpoints"), secret),
            "move copied credential bytes into a checkpoint blob"
        );
    }

    fn plant_forged(data: &Path, path: &str, backup: &str, link_rel: Option<&str>) -> String {
        let id = format!("cp-{}", uuid::Uuid::now_v7().simple());
        let dir = data.join("checkpoints").join("run-1").join(&id);
        fs::create_dir_all(dir.join("files")).unwrap();
        fs::write(dir.join("files").join("0"), "blob-bytes").unwrap();
        let cp = crate::checkpoint::Checkpoint {
            id: id.clone(),
            run_id: "run-1".into(),
            created_ms: 0,
            op: "forged".into(),
            entries: vec![crate::checkpoint::CheckpointEntry {
                path: path.into(),
                existed: true,
                is_dir: false,
                backup: Some(backup.into()),
                size: None,
                sha256: None,
                via_link_of: None,
            }],
            skipped: Vec::new(),
            links_sha256: None,
        };
        fs::write(dir.join("manifest.json"), serde_json::to_vec(&cp).unwrap()).unwrap();
        if let Some(rel) = link_rel {
            let links = json!([{
                "rel": rel,
                "target": "kept-target",
                "dir": false
            }]);
            fs::write(dir.join("links.json"), serde_json::to_vec(&links).unwrap()).unwrap();
        }
        id
    }

    #[tokio::test]
    async fn forged_backup_and_link_rel_are_rejected_before_io() {
        let (d, w, t) = setup();
        let c = ctx();
        let file = w.path().join("keep.txt");
        fs::write(&file, "stay").unwrap();
        let outside = w.path().join("outside");
        let escape = w.path().join("praxi-escape");
        let hostile = [
            r"\\192.0.2.1\share\x",
            r"\\server\share",
            r"..\outside",
            r"files/0/../../praxi-escape",
        ];
        for backup in hostile {
            let id = plant_forged(d.path(), &file.display().to_string(), backup, None);
            let started = std::time::Instant::now();
            let err = get(&t, "checkpoint_restore")
                .call(&c, json!({"id": id}))
                .await
                .unwrap_err();
            assert!(
                started.elapsed() < std::time::Duration::from_secs(2),
                "{backup} took {:?}",
                started.elapsed()
            );
            assert!(
                matches!(err, ToolError::PermissionDenied(_)),
                "{backup}: {err}"
            );
            assert_eq!(fs::read_to_string(&file).unwrap(), "stay", "{backup}");
            assert!(!outside.exists(), "{backup}");
            assert!(!escape.exists(), "{backup}");
        }
        for rel in hostile {
            let id = plant_forged(d.path(), &file.display().to_string(), "files/0", Some(rel));
            let started = std::time::Instant::now();
            let err = get(&t, "checkpoint_restore")
                .call(&c, json!({"id": id}))
                .await
                .unwrap_err();
            assert!(
                started.elapsed() < std::time::Duration::from_secs(2),
                "link {rel} took {:?}",
                started.elapsed()
            );
            assert!(
                matches!(err, ToolError::PermissionDenied(_)),
                "link {rel}: {err}"
            );
            assert_eq!(fs::read_to_string(&file).unwrap(), "stay", "link {rel}");
            assert!(!outside.exists(), "link {rel}");
            assert!(!escape.exists(), "link {rel}");
        }
    }

    #[tokio::test]
    async fn restore_recreates_deleted_parent_inside_root() {
        let (d, w, _) = setup();
        let c = ctx();
        let root = w.path().join("root");
        let parent = root.join("sub");
        let file = parent.join("a.txt");
        fs::create_dir_all(&parent).unwrap();
        let body = "restored-body";
        let id = plant_blob(d.path(), "run-1", &file.display().to_string(), body);
        fs::remove_dir_all(&parent).unwrap();
        let t = tools_bounded(d.path(), std::slice::from_ref(&root), &[]);
        get(&t, "checkpoint_restore")
            .call(&c, json!({"id": id}))
            .await
            .unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), body);
        assert!(parent.is_dir());

        let outside = w.path().join("outside-parent");
        let outside_file = outside.join("a.txt");
        let id = plant_blob(
            d.path(),
            "run-1",
            &outside_file.display().to_string(),
            "nope",
        );
        let err = get(&t, "checkpoint_restore")
            .call(&c, json!({"id": id}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::PermissionDenied(_)), "{err}");
        assert!(
            !outside.exists(),
            "restore created a parent outside the vetted root"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn unsavable_cloud_placeholder_stops_directory_delete_and_move_before_any_change() {
        let (_d, w, t) = setup();
        let c = ctx();
        let dir = w.path().join("proj");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("keep.txt"), "keep").unwrap();
        let placeholder = dir.join("online-only.txt");
        fs::write(&placeholder, b"").unwrap();
        win_fs::mark_cloud_placeholder(&placeholder).expect("cloud reparse tag on a local file");
        assert!(
            fs::read_link(&placeholder).is_err(),
            "placeholder still answers read_link; this does not reproduce the regression"
        );
        let err = get(&t, "fs_read")
            .call(&c, json!({"path": &placeholder}))
            .await
            .unwrap_err();
        assert!(
            matches!(err, ToolError::PermissionDenied(_)),
            "read of a cloud placeholder was allowed: {err}"
        );
        // Card 181 item 1: a placeholder that cannot be saved in the undo
        // checkpoint stops the delete instead of leaving 1 file restorable
        // only by hand. Nothing is changed.
        let err = get(&t, "fs_delete")
            .call(&c, json!({"path": &dir, "recursive": true}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("online-only.txt"), "{err}");
        assert!(err.to_string().contains("cannot be restored"), "{err}");
        assert_eq!(fs::read_to_string(dir.join("keep.txt")).unwrap(), "keep");

        let src = w.path().join("proj-move");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("keep.txt"), "keep").unwrap();
        let moved_placeholder = src.join("online-only.txt");
        fs::write(&moved_placeholder, b"").unwrap();
        win_fs::mark_cloud_placeholder(&moved_placeholder).expect("cloud reparse tag");
        let dest = w.path().join("proj-dest");
        let err = get(&t, "fs_move")
            .call(&c, json!({"from": &src, "to": &dest}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("online-only.txt"), "{err}");
        assert!(!dest.exists());
        assert_eq!(fs::read_to_string(src.join("keep.txt")).unwrap(), "keep");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unix_dos_names_and_colons_are_ordinary_files() {
        let (_d, w, t) = setup();
        let c = ctx();
        for name in ["nul", "con", "aux", "prn", "com1", "lpt1", "a:b"] {
            let file = w.path().join(name);
            let body = format!("ok-{name}");
            fs::write(&file, &body).unwrap();
            let ok = text(
                &get(&t, "fs_read")
                    .call(&c, json!({"path": &file}))
                    .await
                    .unwrap(),
            );
            assert!(ok.contains(&body), "{name}: {ok}");
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn short_names_of_credential_files_are_refused() {
        let base = tempfile::tempdir().unwrap();
        let data = base.path().join("praxi-bot");
        fs::create_dir_all(&data).unwrap();
        let secret = "PRAXI_SENTINEL_83";
        fs::write(data.join("devices.json"), secret).unwrap();
        let envf = base.path().join(".env");
        fs::write(&envf, secret).unwrap();
        let t = tools(&data);
        let c = ctx();
        let short_env = win_fs::short_path(&envf).unwrap();
        assert_ne!(
            short_env
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_ascii_lowercase(),
            ".env",
            "this volume did not generate an 8.3 name for .env: {}",
            short_env.display()
        );
        let err = get(&t, "fs_read")
            .call(&c, json!({"path": &short_env}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::PermissionDenied(_)), "{err}");
        assert!(!err.to_string().contains(secret), "{err}");
        let err = get(&t, "fs_search")
            .call(&c, json!({"query": secret, "path": &short_env}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::PermissionDenied(_)), "{err}");
        let err = get(&t, "fs_write")
            .call(&c, json!({"path": &short_env, "content": "replaced"}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::PermissionDenied(_)), "{err}");
        assert_eq!(fs::read_to_string(&envf).unwrap(), secret);

        let short_data = win_fs::short_path(&data).unwrap();
        assert_ne!(
            short_data
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_ascii_lowercase(),
            "praxi-bot",
            "this volume did not generate an 8.3 name for praxi-bot: {}",
            short_data.display()
        );
        let via = short_data.join("devices.json");
        let err = get(&t, "fs_read")
            .call(&c, json!({"path": &via}))
            .await
            .unwrap_err();
        assert!(
            matches!(err, ToolError::PermissionDenied(_)),
            "{via:?}: {err}"
        );
        assert!(!err.to_string().contains(secret), "{err}");

        let note = base.path().join("notes.txt");
        fs::write(&note, "ordinary").unwrap();
        let ok = text(
            &get(&t, "fs_read")
                .call(&c, json!({"path": &note}))
                .await
                .unwrap(),
        );
        assert!(ok.contains("ordinary"), "{ok}");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn short_data_dir_still_protects_sensitive_files() {
        let base = tempfile::tempdir().unwrap();
        let data = base.path().join("praxi-bot");
        fs::create_dir_all(data.join("vault")).unwrap();
        fs::create_dir_all(data.join("secrets")).unwrap();
        let secret = "PRAXI_SENTINEL_datadir";
        fs::write(data.join("devices.json"), secret).unwrap();
        fs::write(data.join("peer-node"), secret).unwrap();
        fs::write(data.join("vault").join("note.txt"), secret).unwrap();
        fs::write(data.join("secrets").join("note.txt"), secret).unwrap();
        fs::write(data.join("notes.txt"), "ordinary").unwrap();
        let short = win_fs::short_path(&data).unwrap();
        assert_ne!(
            short
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_ascii_lowercase(),
            "praxi-bot",
            "this volume did not generate an 8.3 name for praxi-bot: {}",
            short.display()
        );
        let t = tools(&short);
        let c = ctx();
        for path in [
            data.join("devices.json"),
            data.join("peer-node"),
            data.join("vault").join("note.txt"),
            data.join("secrets").join("note.txt"),
            short.join("devices.json"),
            short.join("vault").join("note.txt"),
        ] {
            let err = get(&t, "fs_read")
                .call(&c, json!({"path": &path}))
                .await
                .unwrap_err();
            assert!(
                matches!(err, ToolError::PermissionDenied(_)),
                "{}: {err}",
                path.display()
            );
            assert!(!err.to_string().contains(secret), "{err}");
        }
        let ok = text(
            &get(&t, "fs_read")
                .call(&c, json!({"path": data.join("notes.txt")}))
                .await
                .unwrap(),
        );
        assert!(ok.contains("ordinary"), "{ok}");
    }

    /// A root or grant taken from an 8.3 temp path must still allow that path.
    ///
    /// `prepare_roots` stores the long identity. The caller path keeps the
    /// short temp prefix (`RUNNER~1`), and the two forms name one directory.
    #[cfg(windows)]
    #[tokio::test]
    async fn short_temp_root_and_grant_match_the_long_identity() {
        let base = tempfile::tempdir().unwrap();
        let long_work = base.path().join("project-workspace");
        fs::create_dir_all(&long_work).unwrap();
        let short_work = win_fs::short_path(&long_work).unwrap();
        assert_ne!(
            short_work
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_ascii_lowercase(),
            "project-workspace",
            "this volume did not generate an 8.3 name for project-workspace: {}",
            short_work.display()
        );
        let data = base.path().join("praxi-data");
        fs::create_dir_all(&data).unwrap();
        let root = short_work.join("root");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("ok.txt"), "safe").unwrap();
        let bounded = tools_bounded(&data, std::slice::from_ref(&root), &[]);
        let c = ctx();
        let ok = text(
            &get(&bounded, "fs_read")
                .call(&c, json!({"path": root.join("ok.txt")}))
                .await
                .unwrap(),
        );
        assert!(ok.contains("safe"), "{ok}");
        let outside = short_work.join("outside.txt");
        fs::write(&outside, "nope").unwrap();
        let err = get(&bounded, "fs_read")
            .call(&c, json!({"path": &outside}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::PermissionDenied(_)), "{err}");

        let secret = "PRAXI_SENTINEL_short_grant";
        let envf = short_work.join(".env");
        fs::write(&envf, secret).unwrap();
        let granted = tools_bounded(&data, &[], std::slice::from_ref(&envf));
        let ok = text(
            &get(&granted, "fs_read")
                .call(&c, json!({"path": &envf}))
                .await
                .unwrap(),
        );
        assert!(ok.contains(secret), "{ok}");
        let long_env = fs::canonicalize(&envf).unwrap();
        let ok = text(
            &get(&granted, "fs_read")
                .call(&c, json!({"path": &long_env}))
                .await
                .unwrap(),
        );
        assert!(ok.contains(secret), "long path of a short grant: {ok}");
    }

    #[cfg(windows)]
    struct RemoveDos(char, &'static str);

    #[cfg(windows)]
    impl Drop for RemoveDos {
        fn drop(&mut self) {
            let _ = win_fs::define_dos_device(self.0, self.1, true);
        }
    }

    #[cfg(windows)]
    struct RemoveSubst(char);

    #[cfg(windows)]
    impl Drop for RemoveSubst {
        fn drop(&mut self) {
            let _ = std::process::Command::new("subst")
                .args([format!("{}:", self.0), "/d".into()])
                .status();
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn live_remote_drive_letter_is_refused_without_network_io() {
        // Defines process-session-wide drive letters, one of them pointing at an
        // unreachable share; PowerShell started meanwhile stalls on it.
        let _serial = crate::test_serial::WINDOWS_SHELL_TESTS.lock().await;
        let failed = drive_decision(win_fs::DosQuery::Failed, DRIVE_UNKNOWN).unwrap_err();
        assert!(matches!(failed, ToolError::PermissionDenied(_)), "{failed}");
        let remote = drive_decision(
            win_fs::DosQuery::Target(r"\Device\Mup\192.0.2.1\share".into()),
            DRIVE_REMOTE,
        )
        .unwrap_err();
        assert!(matches!(remote, ToolError::PermissionDenied(_)), "{remote}");

        let letter = win_fs::unused_drive_letter().expect("an unused drive letter");
        let target = r"\Device\Mup\192.0.2.1\share";
        win_fs::define_dos_device(letter, target, false).unwrap();
        let guard = RemoveDos(letter, target);
        let (_d, w, t) = setup();
        let c = ctx();
        let path = format!(r"{letter}:\file.txt");
        let started = std::time::Instant::now();
        let err = get(&t, "fs_read")
            .call(&c, json!({"path": path}))
            .await
            .unwrap_err();
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        assert!(matches!(err, ToolError::PermissionDenied(_)), "{err}");

        drop(guard);
        let letter = win_fs::unused_drive_letter().expect("an unused drive letter");
        let subst_root = w.path().join("subst-root");
        fs::create_dir_all(&subst_root).unwrap();
        fs::write(subst_root.join("f.txt"), "subst-ok").unwrap();
        let status = std::process::Command::new("subst")
            .arg(format!("{letter}:"))
            .arg(&subst_root)
            .status()
            .unwrap();
        assert!(status.success(), "subst {letter}:");
        let _subst = RemoveSubst(letter);
        let ok = text(
            &get(&t, "fs_read")
                .call(&c, json!({"path": format!(r"{letter}:\f.txt")}))
                .await
                .unwrap(),
        );
        assert!(ok.contains("subst-ok"), "{ok}");
    }
}
