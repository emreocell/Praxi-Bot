//! The six git tools.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tool_registry::{
    ContentClass, ContentPart, Effect, PermissionCategory, Platform, Tool, ToolContext, ToolError,
    ToolOutput, ToolSpec,
};

use crate::DevToolsConfig;
use crate::filter::{filter_diff, filter_status};
use crate::git::GitRunner;
use crate::guard::{Workspace, check_branch_chars, check_paths, check_rev};
use crate::policy::{GitPolicy, MutationScope};

const DEFAULT_MAX_BYTES: usize = 200 * 1024;
const MAX_MAX_BYTES: usize = 4 * 1024 * 1024;
const MAX_LOG_COUNT: u32 = 200;
const MAX_MESSAGE_BYTES: usize = 16 * 1024;
/// Most bytes of a file listing read before a commit refuses to go on.
const MAX_LISTING_BYTES: usize = 4 * 1024 * 1024;
/// Most changed files a commit sizes one by one for the write budget.
const MAX_SIZED_FILES: usize = 100_000;

#[derive(Clone, Copy)]
enum Kind {
    Status,
    Diff,
    Log,
    Branch,
    Commit,
    Show,
}

struct GitTool {
    spec: ToolSpec,
    kind: Kind,
    ws: Workspace,
    git: GitRunner,
    policy: Option<Arc<dyn GitPolicy>>,
}

/// A repository the call may work in: the folder git runs in and the places git reported.
struct Repo {
    /// Where git runs (`repo`); pathspecs are relative to it.
    dir: PathBuf,
    /// The work tree's top level: diff headers and status entries are relative to it.
    top: PathBuf,
    git_dir: PathBuf,
    common_dir: PathBuf,
}

/// `rel` under `base` with the `.` parts dropped (`rel` was checked: no `..`, no root).
fn join_rel(base: &Path, rel: &str) -> PathBuf {
    let mut out = base.to_path_buf();
    for c in Path::new(rel).components() {
        if let Component::Normal(part) = c {
            out.push(part);
        }
    }
    out
}

/// Without the `\\?\` prefix `canonicalize` adds on Windows, so path rules see a normal path.
fn plain(p: PathBuf) -> PathBuf {
    let stripped = {
        let text = p.to_string_lossy();
        text.strip_prefix(r"\\?\")
            .filter(|rest| rest.as_bytes().get(1) == Some(&b':'))
            .map(PathBuf::from)
    };
    stripped.unwrap_or(p)
}

fn parse<T: DeserializeOwned>(input: Value) -> Result<T, ToolError> {
    serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))
}

fn spec(name: &str, description: &str, schema: Value, effect: Effect) -> ToolSpec {
    ToolSpec {
        name: name.to_string(),
        description: description.to_string(),
        input_schema: schema,
        effect,
        category: PermissionCategory::Fs,
        platforms: vec![Platform::All],
        content: if name == "git_commit" {
            ContentClass::private_read()
        } else {
            // status/diff/log/show/branch read the repo, which can hold anything.
            ContentClass::untrusted_private()
        },
    }
}

/// Build the git tools.
pub(crate) fn tools(cfg: &DevToolsConfig) -> Vec<Arc<dyn Tool>> {
    let repo = json!({"type":"string","description":"Repository directory (absolute, or relative to the first workspace root). Default: the first workspace root."});
    let paths = json!({"type":"array","items":{"type":"string"},"description":"Repository-relative paths (no '..', no absolute paths, must not start with '-')."});
    let rev = json!({"type":"string","description":"Revision such as HEAD~1, a branch, a tag, a hash or a range a..b."});
    let max_bytes = json!({"type":"integer","minimum":1,"maximum":MAX_MAX_BYTES,"description":"Output size cap in bytes (default 204800)."});
    let defs: Vec<(Kind, ToolSpec)> = vec![
        (
            Kind::Status,
            spec(
                "git_status",
                "Show branch and working tree status (porcelain v1 format: XY path). Example: {\"repo\":\"C:/work/app\"}",
                json!({"type":"object","additionalProperties":false,"properties":{
                    "repo":repo,
                    "include_untracked":{"type":"boolean","description":"Include untracked files (default true)."}
                }}),
                Effect::Read,
            ),
        ),
        (
            Kind::Diff,
            spec(
                "git_diff",
                "Show a diff of the working tree (or the index with staged=true, or against a revision). Size-capped. Example: {\"paths\":[\"src/main.rs\"],\"staged\":true}",
                json!({"type":"object","additionalProperties":false,"properties":{
                    "repo":repo,"staged":{"type":"boolean","description":"Diff the index against HEAD."},
                    "rev":rev,"paths":paths,
                    "stat":{"type":"boolean","description":"Only a diffstat."},
                    "context_lines":{"type":"integer","minimum":0,"maximum":100},
                    "max_bytes":max_bytes
                }}),
                Effect::Read,
            ),
        ),
        (
            Kind::Log,
            spec(
                "git_log",
                "List recent commits as JSON {hash, author, email, date, subject}. Example: {\"max_count\":10,\"paths\":[\"README.md\"]}",
                json!({"type":"object","additionalProperties":false,"properties":{
                    "repo":repo,"rev":rev,"paths":paths,
                    "max_count":{"type":"integer","minimum":1,"maximum":MAX_LOG_COUNT,"description":"Default 20."}
                }}),
                Effect::Read,
            ),
        ),
        (
            Kind::Branch,
            spec(
                "git_branch",
                "List, create or switch branches. action=list returns JSON; create makes a branch (optionally from start_point and switch=true); switch checks a branch out. Example: {\"action\":\"create\",\"name\":\"feat/x\",\"switch\":true}",
                json!({"type":"object","additionalProperties":false,"properties":{
                    "repo":repo,
                    "action":{"type":"string","enum":["list","create","switch"]},
                    "name":{"type":"string","description":"Branch name (create, switch)."},
                    "start_point":rev,
                    "switch":{"type":"boolean","description":"With create: check the new branch out."},
                    "remotes":{"type":"boolean","description":"With list: include remote-tracking branches."}
                },"required":["action"]}),
                Effect::Write,
            ),
        ),
        (
            Kind::Commit,
            spec(
                "git_commit",
                "Stage the given paths and commit only those paths with the message. Hooks are not run. Example: {\"paths\":[\"src/lib.rs\"],\"message\":\"Fix parser\"}",
                json!({"type":"object","additionalProperties":false,"properties":{
                    "repo":repo,"paths":paths,
                    "message":{"type":"string","description":"Commit message (non-empty)."}
                },"required":["paths","message"]}),
                Effect::Write,
            ),
        ),
        (
            Kind::Show,
            spec(
                "git_show",
                "Show a commit (message and diff). Size-capped. Example: {\"rev\":\"HEAD~1\",\"stat\":true}",
                json!({"type":"object","additionalProperties":false,"properties":{
                    "repo":repo,"rev":rev,"paths":paths,
                    "stat":{"type":"boolean","description":"Only a diffstat."},
                    "max_bytes":max_bytes
                }}),
                Effect::Read,
            ),
        ),
    ];
    let ws = Workspace::new(cfg.allowed_roots.clone());
    let git = GitRunner::new(cfg.git_binary.clone(), cfg.git_timeout);
    defs.into_iter()
        .map(|(kind, spec)| {
            Arc::new(GitTool {
                spec,
                kind,
                ws: ws.clone(),
                git: git.clone(),
                policy: cfg.policy.clone(),
            }) as Arc<dyn Tool>
        })
        .collect()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusIn {
    repo: Option<String>,
    include_untracked: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DiffIn {
    repo: Option<String>,
    staged: Option<bool>,
    rev: Option<String>,
    #[serde(default)]
    paths: Vec<String>,
    stat: Option<bool>,
    context_lines: Option<u32>,
    max_bytes: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LogIn {
    repo: Option<String>,
    rev: Option<String>,
    #[serde(default)]
    paths: Vec<String>,
    max_count: Option<u32>,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum BranchAction {
    List,
    Create,
    Switch,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BranchIn {
    repo: Option<String>,
    action: BranchAction,
    name: Option<String>,
    start_point: Option<String>,
    switch: Option<bool>,
    remotes: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommitIn {
    repo: Option<String>,
    paths: Vec<String>,
    message: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ShowIn {
    repo: Option<String>,
    rev: Option<String>,
    #[serde(default)]
    paths: Vec<String>,
    stat: Option<bool>,
    max_bytes: Option<usize>,
}

fn s(v: &str) -> String {
    v.to_string()
}

fn invalid(msg: impl Into<String>) -> ToolError {
    ToolError::InvalidInput(msg.into())
}

fn cap_for(ctx: &ToolContext, requested: Option<usize>) -> usize {
    requested
        .unwrap_or(DEFAULT_MAX_BYTES)
        .clamp(1, MAX_MAX_BYTES)
        .min(ctx.max_inline_bytes.max(1))
}

/// Cut `text` to `cap` bytes on a char boundary, appending a notice.
fn capped_text(mut text: String, cap: usize, hit_capture_limit: bool) -> String {
    let total = text.len();
    if total > cap {
        let mut i = cap;
        while i > 0 && !text.is_char_boundary(i) {
            i -= 1;
        }
        text.truncate(i);
        let _ = write!(
            text,
            "\n...[output truncated at {i} bytes of at least {total}; narrow with paths or raise max_bytes]"
        );
    } else if hit_capture_limit {
        text.push_str("\n...[output truncated; narrow with paths or raise max_bytes]");
    }
    text
}

/// The trimmed message of a commit call, once its paths and message pass the plain checks.
fn check_commit_input(i: &CommitIn) -> Result<&str, ToolError> {
    if i.paths.is_empty() {
        return Err(invalid("paths must list at least one path to commit"));
    }
    check_paths(&i.paths)?;
    if i.paths.iter().any(|p| {
        Path::new(p)
            .components()
            .all(|c| matches!(c, Component::CurDir))
    }) {
        return Err(invalid(
            "name the files or folders to commit; \".\" (the whole folder) is not accepted",
        ));
    }
    let message = i.message.trim();
    if message.is_empty() {
        return Err(invalid("message is empty"));
    }
    if message.len() > MAX_MESSAGE_BYTES || message.contains(' ') {
        return Err(invalid("message is too long or contains NUL"));
    }
    Ok(message)
}

/// `git switch` of `name`: only an existing local branch (no tracking branch is made up from a
/// remote), and never over an ignored file the work tree holds (a real, untracked `.env`).
fn switch_args(name: &str) -> Vec<String> {
    vec![
        s("switch"),
        s("--no-guess"),
        s("--no-overwrite-ignore"),
        s("--no-recurse-submodules"),
        s(name),
    ]
}

/// `(path, new object id)` of each entry of `git diff-tree -r -z --raw` output
/// (`:<modes> <old> <new> <status>\0<path>\0`); `None` when it does not parse.
fn parse_raw_z(text: &str) -> Option<Vec<(String, String)>> {
    let mut entries = Vec::new();
    let mut tokens = text.split('\0').filter(|t| !t.is_empty());
    while let Some(header) = tokens.next() {
        let fields: Vec<&str> = header.strip_prefix(':')?.split(' ').collect();
        let new_id = *fields.get(3)?;
        let path = tokens.next()?;
        entries.push((path.to_owned(), new_id.to_owned()));
    }
    Some(entries)
}

/// The refusal for a protected file that a commit would record (or did, and was taken back).
fn protected_in_commit(rel: &str, undone: bool) -> ToolError {
    let after = if undone {
        "the commit was taken back"
    } else {
        "nothing was committed"
    };
    ToolError::PermissionDenied(format!(
        "{rel} is a protected file (credentials or tool settings) and is not committed; {after}"
    ))
}

/// Largest index kept in memory to undo a failed commit.
const MAX_INDEX_BACKUP_BYTES: u64 = 64 * 1024 * 1024;

/// The bytes of the index file, `None` when there is none or it is too large to keep.
fn read_index(git_dir: &Path) -> Option<Vec<u8>> {
    let path = git_dir.join("index");
    let meta = std::fs::metadata(&path).ok()?;
    if !meta.is_file() || meta.len() > MAX_INDEX_BACKUP_BYTES {
        return None;
    }
    std::fs::read(path).ok()
}

/// Put `bytes` back as the index, the way git writes it (a lock file, then a rename). Does
/// nothing when another git process holds the lock.
fn restore_index(git_dir: &Path, bytes: &[u8]) {
    use std::io::Write as _;
    let lock = git_dir.join("index.lock");
    let Ok(mut file) = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock)
    else {
        return;
    };
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    if written.is_ok() && std::fs::rename(&lock, git_dir.join("index")).is_ok() {
        return;
    }
    let _ = std::fs::remove_file(&lock);
}

impl GitTool {
    /// Resolve and validate the repository: inside the workspace, and so is
    /// its top level. `mutates` is set for a commit or a branch change.
    async fn repo_dir(&self, repo: Option<&str>, mutates: bool) -> Result<Repo, ToolError> {
        let dir = self.ws.resolve_dir(repo)?;
        let top = self
            .git
            .run(
                &dir,
                &[
                    s("rev-parse"),
                    s("--show-toplevel"),
                    s("--absolute-git-dir"),
                    s("--git-common-dir"),
                ],
                None,
                8192,
            )
            .await
            .map_err(|e| match e {
                ToolError::Execution(m) => ToolError::Execution(format!(
                    "{} is not a usable git repository: {m}",
                    dir.display()
                )),
                other => other,
            })?;
        let mut lines = top.stdout.lines();
        let (Some(top), Some(git_dir), Some(common)) = (lines.next(), lines.next(), lines.next())
        else {
            return Err(ToolError::Execution(
                "unexpected output from git rev-parse".into(),
            ));
        };
        // Work tree, git dir and common dir must all be inside the allowed roots
        // (a `.git` file or core.worktree can point anywhere), and the embedder's own rules
        // (credential folders, other bots' workspaces) hold for each of them too.
        let mut places = Vec::with_capacity(3);
        for (what, p) in [
            ("repository root", top),
            ("git directory", git_dir),
            ("common git directory", common),
        ] {
            let p = PathBuf::from(p);
            let abs = if p.is_absolute() {
                p.clone()
            } else {
                dir.join(&p)
            };
            let canon = abs
                .canonicalize()
                .map_err(|e| ToolError::Execution(format!("cannot resolve {what}: {e}")))?;
            if !self.ws.contains_canonical(&canon) {
                return Err(ToolError::PermissionDenied(format!(
                    "{what} {} is outside the allowed workspace roots",
                    p.display()
                )));
            }
            let canon = plain(canon);
            if let Some(policy) = &self.policy {
                policy.check_dir(&canon, mutates).map_err(|why| {
                    ToolError::PermissionDenied(format!("git cannot use the {what}: {why}"))
                })?;
            }
            places.push(canon);
        }
        let mut places = places.into_iter();
        let (Some(top), Some(git_dir), Some(common_dir)) =
            (places.next(), places.next(), places.next())
        else {
            return Err(ToolError::Execution(
                "unexpected output from git rev-parse".into(),
            ));
        };
        Ok(Repo {
            dir,
            top,
            git_dir,
            common_dir,
        })
    }

    /// Is the repository-relative `rel` (as git prints it) hidden by the embedder's rules?
    fn hidden(&self, repo: &Repo, rel: &str) -> bool {
        self.policy
            .as_ref()
            .is_some_and(|p| p.is_protected(&join_rel(&repo.top, rel)))
    }

    /// Refuse a `paths` entry that names a protected file or folder, so the model is told why
    /// instead of getting an empty answer. `paths` are relative to the folder git runs in.
    fn vet_paths(&self, repo: &Repo, paths: &[String]) -> Result<(), ToolError> {
        let Some(policy) = &self.policy else {
            return Ok(());
        };
        for p in paths {
            if policy.is_protected(&join_rel(&repo.dir, p)) {
                return Err(ToolError::PermissionDenied(format!(
                    "{p} is a protected path (credentials or tool settings): git tools do not read or change it"
                )));
            }
        }
        Ok(())
    }

    /// Every endpoint of `rev` (a name, or `a..b` / `a...b`) must name a `kind` object
    /// (`commit` or `tree`), so a blob id cannot be used to print a file that the diff filter
    /// never sees a name for.
    async fn require_object(&self, dir: &Path, rev: &str, kind: &str) -> Result<(), ToolError> {
        for endpoint in rev.split("...").flat_map(|part| part.split("..")) {
            let endpoint = endpoint.trim_start_matches('^');
            if endpoint.is_empty() {
                continue;
            }
            self.git
                .run(
                    dir,
                    &[
                        s("rev-parse"),
                        s("--verify"),
                        s("--quiet"),
                        format!("{endpoint}^{{{kind}}}"),
                    ],
                    None,
                    4096,
                )
                .await
                .map_err(|_| invalid(format!("{endpoint:?} does not name a {kind}")))?;
        }
        Ok(())
    }

    /// Hand a change to the embedder (budget, undo checkpoint) before it runs.
    fn begin(
        &self,
        op: &'static str,
        repo: &Repo,
        control_files: &[PathBuf],
        added_bytes: u64,
    ) -> Result<Option<Box<dyn MutationScope>>, ToolError> {
        self.policy
            .as_ref()
            .map(|p| p.begin_mutation(op, &repo.top, control_files, added_bytes))
            .transpose()
            .map_err(ToolError::PermissionDenied)
    }

    /// Close what [`Self::begin`] opened; the checkpoint id, if any, goes into the result.
    fn end(
        scope: Option<Box<dyn MutationScope>>,
        ok: bool,
        work_tree: Option<&Path>,
        out: &mut Value,
    ) -> Result<(), ToolError> {
        let Some(scope) = scope else {
            return Ok(());
        };
        if let Some(checkpoint) = scope.finish(ok, work_tree).map_err(ToolError::Execution)?
            && ok
        {
            out["checkpoint"] = json!(checkpoint);
        }
        Ok(())
    }

    async fn status(&self, ctx: &ToolContext, i: StatusIn) -> Result<ToolOutput, ToolError> {
        let repo = self.repo_dir(i.repo.as_deref(), false).await?;
        let untracked = if i.include_untracked.unwrap_or(true) {
            "--untracked-files=normal"
        } else {
            "--untracked-files=no"
        };
        let cap = cap_for(ctx, None);
        let out = self
            .git
            .run(
                &repo.dir,
                &[
                    s("status"),
                    s("--porcelain=v1"),
                    s("--branch"),
                    // A submodule's own state needs its own git directory, which a bot can point
                    // anywhere: not looked at.
                    s("--ignore-submodules=all"),
                    s(untracked),
                ],
                None,
                cap,
            )
            .await?;
        let shown = filter_status(&out.stdout, &|rel| self.hidden(&repo, rel)).into_text();
        Ok(ToolOutput::text(capped_text(shown, cap, out.truncated)))
    }

    async fn diff(&self, ctx: &ToolContext, i: DiffIn) -> Result<ToolOutput, ToolError> {
        check_paths(&i.paths)?;
        if let Some(r) = &i.rev {
            check_rev(r)?;
            if i.staged == Some(true) {
                return Err(invalid("use either rev or staged, not both"));
            }
        }
        let repo = self.repo_dir(i.repo.as_deref(), false).await?;
        self.vet_paths(&repo, &i.paths)?;
        if let Some(r) = &i.rev {
            self.require_object(&repo.dir, r, "tree").await?;
        }
        let stat = i.stat == Some(true);
        let mut args = vec![
            s("diff"),
            s("--no-color"),
            s("--no-ext-diff"),
            s("--no-textconv"),
            s("--no-renames"),
            s("--src-prefix=a/"),
            s("--dst-prefix=b/"),
            s("--submodule=short"),
            s("--ignore-submodules=all"),
        ];
        if i.staged == Some(true) {
            args.push(s("--cached"));
        }
        if stat {
            // Whole names, so the filter can tell which file a line is about.
            args.push(s("--stat=4096,2048"));
        }
        if let Some(n) = i.context_lines {
            if n > 100 {
                return Err(invalid("context_lines must be <= 100"));
            }
            args.push(format!("--unified={n}"));
        }
        if let Some(r) = i.rev {
            args.push(r);
        }
        args.push(s("--"));
        args.extend(i.paths);
        let cap = cap_for(ctx, i.max_bytes);
        let out = self.git.run(&repo.dir, &args, None, cap).await?;
        let shown = filter_diff(&out.stdout, stat, &|rel| self.hidden(&repo, rel)).into_text();
        let text = if shown.is_empty() {
            "(no differences)".to_string()
        } else {
            capped_text(shown, cap, out.truncated)
        };
        Ok(ToolOutput::text(text))
    }

    async fn log(&self, ctx: &ToolContext, i: LogIn) -> Result<ToolOutput, ToolError> {
        check_paths(&i.paths)?;
        if let Some(r) = &i.rev {
            check_rev(r)?;
        }
        let count = i.max_count.unwrap_or(20);
        if count == 0 || count > MAX_LOG_COUNT {
            return Err(invalid(format!("max_count must be 1..={MAX_LOG_COUNT}")));
        }
        let repo = self.repo_dir(i.repo.as_deref(), false).await?;
        self.vet_paths(&repo, &i.paths)?;
        let dir = repo.dir;
        let mut args = vec![
            s("log"),
            s("--no-color"),
            format!("--max-count={count}"),
            s("--format=%H%x1f%an%x1f%ae%x1f%aI%x1f%s"),
        ];
        if let Some(r) = i.rev {
            args.push(r);
        }
        args.push(s("--"));
        args.extend(i.paths);
        let out = self
            .git
            .run(&dir, &args, None, cap_for(ctx, Some(MAX_MAX_BYTES)))
            .await;
        // An empty repository has no HEAD: report no commits rather than an error.
        let out = match out {
            Err(ToolError::Execution(m)) if m.contains("does not have any commits") => {
                return Ok(json_out(json!({"commits": []})));
            }
            other => other?,
        };
        // A cut-off output ends in a half record: leave it out and say so.
        let mut lines: Vec<&str> = out.stdout.lines().collect();
        if out.truncated {
            lines.pop();
        }
        let commits: Vec<Value> = lines
            .into_iter()
            .filter_map(|l| {
                let mut f = l.split('\u{1f}');
                Some(json!({
                    "hash": f.next()?, "author": f.next()?, "email": f.next()?,
                    "date": f.next()?, "subject": f.next().unwrap_or("")
                }))
            })
            .collect();
        Ok(json_out(
            json!({"commits": commits, "truncated": out.truncated}),
        ))
    }

    async fn branch(&self, i: BranchIn) -> Result<ToolOutput, ToolError> {
        match i.action {
            BranchAction::List => {
                let dir = self.repo_dir(i.repo.as_deref(), false).await?.dir;
                let mut args = vec![
                    s("for-each-ref"),
                    s(
                        "--format=%(HEAD)%09%(refname:short)%09%(upstream:short)%09%(objectname:short)",
                    ),
                    s("refs/heads"),
                ];
                if i.remotes == Some(true) {
                    args.push(s("refs/remotes"));
                }
                let out = self.git.run(&dir, &args, None, MAX_MAX_BYTES).await?;
                let mut current = Value::Null;
                let branches: Vec<Value> = out
                    .stdout
                    .lines()
                    .filter_map(|l| {
                        let mut f = l.split('\t');
                        let head = f.next()? == "*";
                        let name = f.next()?;
                        let upstream = f.next().filter(|u| !u.is_empty());
                        let commit = f.next().unwrap_or("");
                        if head {
                            current = json!(name);
                        }
                        Some(
                            json!({"name":name,"current":head,"upstream":upstream,"commit":commit}),
                        )
                    })
                    .collect();
                Ok(json_out(
                    json!({"current": current, "branches": branches, "truncated": out.truncated}),
                ))
            }
            BranchAction::Create | BranchAction::Switch => self.change_branch(i).await,
        }
    }

    /// `create` and `switch`: a change, so the embedder is told before and after.
    async fn change_branch(&self, i: BranchIn) -> Result<ToolOutput, ToolError> {
        let name = i
            .name
            .as_deref()
            .ok_or_else(|| invalid("name is required for create and switch"))?;
        check_branch_chars(name)?;
        if i.action == BranchAction::Switch && (i.start_point.is_some() || i.switch.is_some()) {
            return Err(invalid(
                "start_point and switch only apply to action=create",
            ));
        }
        if let Some(r) = &i.start_point {
            check_rev(r)?;
        }
        let repo = self.repo_dir(i.repo.as_deref(), true).await?;
        let dir = &repo.dir;
        self.git
            .run(
                dir,
                &[s("check-ref-format"), s("--branch"), s(name)],
                None,
                4096,
            )
            .await
            .map_err(|_| invalid(format!("invalid branch name: {name:?}")))?;
        // A switch rewrites the work tree: before anything runs, see which files it would write
        // or remove. A protected one (a branch that tracks `.env` or `.claude/settings.json`, or
        // lacks the `.env` the work tree has) stops it, and what it adds counts for the budget.
        // git itself refuses to overwrite changes that are not committed.
        let switches = i.action == BranchAction::Switch || i.switch == Some(true);
        let mut added = 0;
        if switches {
            let target = match (i.action, i.start_point.as_deref()) {
                (BranchAction::Switch, _) => Some(format!("refs/heads/{name}")),
                (_, Some(start)) => Some(start.to_owned()),
                // A new branch at HEAD changes no file.
                (_, None) => None,
            };
            if let Some(target) = target {
                let tree = self.tree_of(dir, &target).await.map_err(|_| {
                    invalid(format!(
                        "{target:?} does not name a commit in this repository (a switch needs an existing local branch; create one with action=create and a start_point)"
                    ))
                })?;
                added = self.check_switch(&repo, &tree).await?;
            }
        }
        // A switch moves HEAD and the index; they are what an undo checkpoint can bring back.
        let control: Vec<PathBuf> = if switches {
            vec![repo.git_dir.join("index"), repo.git_dir.join("HEAD")]
        } else {
            Vec::new()
        };
        let tree = switches.then_some(repo.top.as_path());
        let scope = self.begin("git_branch", &repo, &control, added)?;
        let result = async {
            if i.action == BranchAction::Create {
                let mut args = vec![s("branch"), s(name)];
                if let Some(r) = i.start_point.clone() {
                    args.push(r);
                }
                self.git.run(dir, &args, None, 4096).await?;
                if i.switch == Some(true) {
                    self.git.run(dir, &switch_args(name), None, 4096).await?;
                }
                Ok(json!({"created": name, "switched": i.switch == Some(true)}))
            } else {
                self.git.run(dir, &switch_args(name), None, 4096).await?;
                Ok(json!({"switched_to": name}))
            }
        }
        .await;
        let mut out = match result {
            Ok(out) => out,
            Err(e) => {
                // The work tree may have changed before git failed: close the books anyway.
                let _ = Self::end(scope, false, tree, &mut Value::Null);
                return Err(e);
            }
        };
        Self::end(scope, true, tree, &mut out)?;
        Ok(json_out(out))
    }

    /// The id of the tree `rev` names, or an error when it names no commit or tree.
    async fn tree_of(&self, dir: &Path, rev: &str) -> Result<String, ToolError> {
        let out = self
            .git
            .run(
                dir,
                &[
                    s("rev-parse"),
                    s("--verify"),
                    s("--quiet"),
                    format!("{rev}^{{tree}}"),
                ],
                None,
                4096,
            )
            .await?;
        let id = out.stdout.trim();
        if id.is_empty() || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(invalid(format!("{rev:?} names no tree")));
        }
        Ok(id.to_owned())
    }

    /// Refuse a switch to the tree `target` when it would write or remove a protected file;
    /// otherwise the bytes it adds, for the write budget. Fails closed: a listing that cannot be
    /// read in full, or is too long to size, stops the switch.
    async fn check_switch(&self, repo: &Repo, target: &str) -> Result<u64, ToolError> {
        let out = self
            .git
            .run(
                &repo.dir,
                &[
                    s("diff-tree"),
                    s("-r"),
                    s("--no-renames"),
                    s("--no-abbrev"),
                    s("-z"),
                    s("--raw"),
                    s("HEAD^{tree}"),
                    s(target),
                ],
                None,
                MAX_LISTING_BYTES,
            )
            .await?;
        if out.truncated {
            return Err(invalid("too many files differ between the branches"));
        }
        let Some(changes) = parse_raw_z(&out.stdout) else {
            return Err(ToolError::Execution(
                "unexpected output from git diff-tree".into(),
            ));
        };
        if changes.len() > MAX_SIZED_FILES {
            return Err(invalid("too many files differ between the branches"));
        }
        if let Some((path, _)) = changes.iter().find(|(path, _)| self.hidden(repo, path)) {
            return Err(ToolError::PermissionDenied(format!(
                "switching would write or remove {path}, a protected file (credentials or tool settings); nothing was changed"
            )));
        }
        let ids: Vec<&str> = changes
            .iter()
            .map(|(_, id)| id.as_str())
            .filter(|id| id.bytes().any(|b| b != b'0'))
            .collect();
        self.blob_bytes(repo, &ids).await
    }

    /// The sizes of the objects `ids` name, added up (an object this repository does not hold
    /// counts for nothing: a submodule's commit).
    async fn blob_bytes(&self, repo: &Repo, ids: &[&str]) -> Result<u64, ToolError> {
        if ids.is_empty() {
            return Ok(0);
        }
        let mut stdin = ids.join("\n");
        stdin.push('\n');
        let out = self
            .git
            .run(
                &repo.dir,
                &[s("cat-file"), s("--batch-check=%(objectsize)")],
                Some(&stdin),
                MAX_LISTING_BYTES,
            )
            .await?;
        Ok(out
            .stdout
            .lines()
            .filter_map(|l| l.trim().parse::<u64>().ok())
            .fold(0u64, u64::saturating_add))
    }

    /// The names (relative to the top level) staged under `paths`.
    async fn staged_names(&self, repo: &Repo, paths: &[String]) -> Result<Vec<String>, ToolError> {
        let mut args: Vec<String> = [
            "diff",
            "--cached",
            "--name-only",
            "-z",
            "--no-renames",
            "--",
        ]
        .iter()
        .map(|a| s(a))
        .collect();
        args.extend(paths.iter().cloned());
        self.names_of(repo, &args).await
    }

    /// The names (relative to the top level) the commit at HEAD recorded.
    async fn recorded_names(&self, repo: &Repo) -> Result<Vec<String>, ToolError> {
        let args: Vec<String> = [
            "diff-tree",
            "--no-commit-id",
            "--root",
            "-r",
            "--no-renames",
            "--name-only",
            "-z",
            "HEAD",
        ]
        .iter()
        .map(|a| s(a))
        .collect();
        self.names_of(repo, &args).await
    }

    async fn names_of(&self, repo: &Repo, args: &[String]) -> Result<Vec<String>, ToolError> {
        let out = self
            .git
            .run(&repo.dir, args, None, MAX_LISTING_BYTES)
            .await?;
        if out.truncated {
            return Err(invalid(
                "too many changed files under these paths; name fewer of them",
            ));
        }
        Ok(out
            .stdout
            .split('\0')
            .filter(|n| !n.is_empty())
            .map(str::to_owned)
            .collect())
    }

    /// Take back the commit at HEAD (its files stay staged): the one before it becomes HEAD
    /// again, or, for a first commit, the branch is deleted. Best effort.
    async fn undo_commit(&self, repo: &Repo) {
        let has_parent = self
            .git
            .run(
                &repo.dir,
                &[s("rev-parse"), s("--verify"), s("--quiet"), s("HEAD~1")],
                None,
                4096,
            )
            .await
            .is_ok();
        let args = if has_parent {
            vec![s("reset"), s("--soft"), s("--quiet"), s("HEAD~1")]
        } else {
            vec![s("update-ref"), s("-d"), s("HEAD")]
        };
        let _ = self.git.run(&repo.dir, &args, None, 4096).await;
    }

    /// The files a commit of `paths` would record: changed in the work tree, new, deleted or
    /// already staged, as paths relative to the top level.
    async fn commit_candidates(
        &self,
        repo: &Repo,
        paths: &[String],
    ) -> Result<BTreeSet<String>, ToolError> {
        let mut names = BTreeSet::new();
        for fixed in [
            &[
                "ls-files",
                "-z",
                "--full-name",
                "-m",
                "-d",
                "-o",
                "--exclude-standard",
                "--",
            ][..],
            &[
                "diff",
                "--cached",
                "--name-only",
                "-z",
                "--no-renames",
                "--",
            ][..],
        ] {
            let mut args: Vec<String> = fixed.iter().map(|a| s(a)).collect();
            args.extend(paths.iter().cloned());
            let out = self
                .git
                .run(&repo.dir, &args, None, MAX_LISTING_BYTES)
                .await?;
            if out.truncated {
                return Err(invalid(
                    "too many changed files under these paths; name fewer of them",
                ));
            }
            names.extend(
                out.stdout
                    .split('\0')
                    .filter(|n| !n.is_empty())
                    .map(str::to_owned),
            );
        }
        Ok(names)
    }

    /// The git files a commit rewrites: the index, HEAD and the branch HEAD points at.
    fn control_files(repo: &Repo) -> Vec<PathBuf> {
        let mut files = vec![repo.git_dir.join("index"), repo.git_dir.join("HEAD")];
        if let Ok(head) = std::fs::read_to_string(repo.git_dir.join("HEAD"))
            && let Some(name) = head.trim().strip_prefix("ref: ")
            && name.starts_with("refs/")
            && !name.split('/').any(|part| part.is_empty() || part == "..")
        {
            files.push(join_rel(&repo.common_dir, name));
        }
        files
    }

    async fn commit(&self, i: CommitIn) -> Result<ToolOutput, ToolError> {
        let message = check_commit_input(&i)?;
        let repo = self.repo_dir(i.repo.as_deref(), true).await?;
        self.vet_paths(&repo, &i.paths)?;
        // Look at what would be committed BEFORE anything is staged: a protected file inside a
        // folder (or already staged) stops the whole commit, nothing changes.
        let candidates = self.commit_candidates(&repo, &i.paths).await?;
        if candidates.len() > MAX_SIZED_FILES {
            return Err(invalid(
                "too many changed files under these paths; name fewer of them",
            ));
        }
        let mut added: u64 = 0;
        for rel in &candidates {
            if self.hidden(&repo, rel) {
                return Err(ToolError::PermissionDenied(format!(
                    "{rel} is a protected file (credentials or tool settings) and is not committed; nothing was staged"
                )));
            }
            if let Ok(meta) = std::fs::symlink_metadata(join_rel(&repo.top, rel))
                && meta.is_file()
            {
                added = added.saturating_add(meta.len());
            }
        }
        // The index as it is now, to put back when the commit does not happen.
        let index_backup = read_index(&repo.git_dir);
        let scope = self.begin("git_commit", &repo, &Self::control_files(&repo), added)?;
        let committed = AtomicBool::new(false);
        let run = async {
            let mut add = vec![s("add"), s("--")];
            add.extend(i.paths.iter().cloned());
            self.git.run(&repo.dir, &add, None, 64 * 1024).await?;
            // A file that appeared under a named folder after the look above (another tool call,
            // another process) is staged by now: look again before anything is recorded.
            if let Some(rel) = self
                .staged_names(&repo, &i.paths)
                .await?
                .iter()
                .find(|n| self.hidden(&repo, n))
            {
                return Err(protected_in_commit(rel, false));
            }
            let mut commit = vec![
                s("commit"),
                s("--no-verify"),
                s("--no-gpg-sign"),
                s("-F"),
                s("-"),
                s("--only"),
                s("--"),
            ];
            commit.extend(i.paths.iter().cloned());
            self.git
                .run(&repo.dir, &commit, Some(message), 64 * 1024)
                .await?;
            committed.store(true, Ordering::SeqCst);
            // What git recorded is what counts: take the commit back if a protected file is in it.
            if let Some(rel) = self
                .recorded_names(&repo)
                .await?
                .iter()
                .find(|n| self.hidden(&repo, n))
            {
                self.undo_commit(&repo).await;
                committed.store(false, Ordering::SeqCst);
                return Err(protected_in_commit(rel, true));
            }
            self.git
                .run(
                    &repo.dir,
                    &[s("log"), s("-1"), s("--format=%H%x1f%s")],
                    None,
                    64 * 1024,
                )
                .await
        }
        .await;
        let head = match run {
            Ok(head) => head,
            Err(e) => {
                if !committed.load(Ordering::SeqCst)
                    && let Some(bytes) = &index_backup
                {
                    // Nothing was committed: whatever `git add` staged is unstaged again.
                    restore_index(&repo.git_dir, bytes);
                }
                let _ = Self::end(scope, false, None, &mut Value::Null);
                return Err(e);
            }
        };
        let line = head.stdout.trim();
        let (hash, subject) = line.split_once('\u{1f}').unwrap_or((line, ""));
        let mut out = json!({"commit": hash, "subject": subject});
        Self::end(scope, true, None, &mut out)?;
        Ok(json_out(out))
    }

    async fn show(&self, ctx: &ToolContext, i: ShowIn) -> Result<ToolOutput, ToolError> {
        check_paths(&i.paths)?;
        let rev = i.rev.unwrap_or_else(|| "HEAD".to_string());
        check_rev(&rev)?;
        let repo = self.repo_dir(i.repo.as_deref(), false).await?;
        self.vet_paths(&repo, &i.paths)?;
        self.require_object(&repo.dir, &rev, "commit").await?;
        let stat = i.stat == Some(true);
        let mut args = vec![
            s("show"),
            s("--no-color"),
            s("--no-ext-diff"),
            s("--no-textconv"),
            s("--no-renames"),
            s("--src-prefix=a/"),
            s("--dst-prefix=b/"),
            s("--submodule=short"),
            s("--ignore-submodules=all"),
        ];
        if stat {
            args.push(s("--stat=4096,2048"));
        }
        args.push(rev);
        args.push(s("--"));
        args.extend(i.paths);
        let cap = cap_for(ctx, i.max_bytes);
        let out = self.git.run(&repo.dir, &args, None, cap).await?;
        let shown = filter_diff(&out.stdout, stat, &|rel| self.hidden(&repo, rel)).into_text();
        Ok(ToolOutput::text(capped_text(shown, cap, out.truncated)))
    }
}

fn json_out(data: Value) -> ToolOutput {
    ToolOutput {
        content: vec![ContentPart::Json { data }],
        spill: None,
        exposure: None,
    }
}

#[async_trait]
impl Tool for GitTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolContext, input: Value) -> Result<ToolOutput, ToolError> {
        match self.kind {
            Kind::Status => self.status(ctx, parse(input)?).await,
            Kind::Diff => self.diff(ctx, parse(input)?).await,
            Kind::Log => self.log(ctx, parse(input)?).await,
            Kind::Branch => self.branch(parse(input)?).await,
            Kind::Commit => self.commit(parse(input)?).await,
            Kind::Show => self.show(ctx, parse(input)?).await,
        }
    }
}
