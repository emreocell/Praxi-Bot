//! The six git tools.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tool_registry::{
    ContentClass, ContentPart, Effect, PermissionCategory, Platform, Tool, ToolContext, ToolError,
    ToolOutput, ToolSpec,
};

use crate::DevToolsConfig;
use crate::git::GitRunner;
use crate::guard::{Workspace, check_branch_chars, check_paths, check_rev};

const DEFAULT_MAX_BYTES: usize = 200 * 1024;
const MAX_MAX_BYTES: usize = 4 * 1024 * 1024;
const MAX_LOG_COUNT: u32 = 200;
const MAX_MESSAGE_BYTES: usize = 16 * 1024;

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

impl GitTool {
    /// Resolve and validate the repository: inside the workspace, and so is
    /// its top level. Returns the directory to run git in.
    async fn repo_dir(&self, repo: Option<&str>) -> Result<PathBuf, ToolError> {
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
        // (a `.git` file or core.worktree can point anywhere).
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
        }
        Ok(dir)
    }

    async fn status(&self, ctx: &ToolContext, i: StatusIn) -> Result<ToolOutput, ToolError> {
        let dir = self.repo_dir(i.repo.as_deref()).await?;
        let untracked = if i.include_untracked.unwrap_or(true) {
            "--untracked-files=normal"
        } else {
            "--untracked-files=no"
        };
        let cap = cap_for(ctx, None);
        let out = self
            .git
            .run(
                &dir,
                &[
                    s("status"),
                    s("--porcelain=v1"),
                    s("--branch"),
                    s(untracked),
                ],
                None,
                cap,
            )
            .await?;
        Ok(ToolOutput::text(capped_text(
            out.stdout,
            cap,
            out.truncated,
        )))
    }

    async fn diff(&self, ctx: &ToolContext, i: DiffIn) -> Result<ToolOutput, ToolError> {
        check_paths(&i.paths)?;
        if let Some(r) = &i.rev {
            check_rev(r)?;
            if i.staged == Some(true) {
                return Err(invalid("use either rev or staged, not both"));
            }
        }
        let dir = self.repo_dir(i.repo.as_deref()).await?;
        let mut args = vec![
            s("diff"),
            s("--no-color"),
            s("--no-ext-diff"),
            s("--no-textconv"),
        ];
        if i.staged == Some(true) {
            args.push(s("--cached"));
        }
        if i.stat == Some(true) {
            args.push(s("--stat"));
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
        let out = self.git.run(&dir, &args, None, cap).await?;
        let text = if out.stdout.is_empty() {
            "(no differences)".to_string()
        } else {
            capped_text(out.stdout, cap, out.truncated)
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
        let dir = self.repo_dir(i.repo.as_deref()).await?;
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
        let commits: Vec<Value> = out
            .stdout
            .lines()
            .filter_map(|l| {
                let mut f = l.split('\u{1f}');
                Some(json!({
                    "hash": f.next()?, "author": f.next()?, "email": f.next()?,
                    "date": f.next()?, "subject": f.next().unwrap_or("")
                }))
            })
            .collect();
        Ok(json_out(json!({"commits": commits})))
    }

    async fn branch(&self, i: BranchIn) -> Result<ToolOutput, ToolError> {
        match i.action {
            BranchAction::List => {
                let dir = self.repo_dir(i.repo.as_deref()).await?;
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
            BranchAction::Create | BranchAction::Switch => {
                let name = i
                    .name
                    .as_deref()
                    .ok_or_else(|| invalid("name is required for create and switch"))?;
                check_branch_chars(name)?;
                if i.action == BranchAction::Switch
                    && (i.start_point.is_some() || i.switch.is_some())
                {
                    return Err(invalid(
                        "start_point and switch only apply to action=create",
                    ));
                }
                if let Some(r) = &i.start_point {
                    check_rev(r)?;
                }
                let dir = self.repo_dir(i.repo.as_deref()).await?;
                self.git
                    .run(
                        &dir,
                        &[s("check-ref-format"), s("--branch"), s(name)],
                        None,
                        4096,
                    )
                    .await
                    .map_err(|_| invalid(format!("invalid branch name: {name:?}")))?;
                if i.action == BranchAction::Create {
                    let mut args = vec![s("branch"), s(name)];
                    if let Some(r) = i.start_point {
                        args.push(r);
                    }
                    self.git.run(&dir, &args, None, 4096).await?;
                    if i.switch == Some(true) {
                        self.git
                            .run(&dir, &[s("switch"), s(name)], None, 4096)
                            .await?;
                    }
                    let switched = i.switch == Some(true);
                    Ok(json_out(json!({"created": name, "switched": switched})))
                } else {
                    self.git
                        .run(&dir, &[s("switch"), s(name)], None, 4096)
                        .await?;
                    Ok(json_out(json!({"switched_to": name})))
                }
            }
        }
    }

    async fn commit(&self, i: CommitIn) -> Result<ToolOutput, ToolError> {
        if i.paths.is_empty() {
            return Err(invalid("paths must list at least one path to commit"));
        }
        check_paths(&i.paths)?;
        let message = i.message.trim();
        if message.is_empty() {
            return Err(invalid("message is empty"));
        }
        if message.len() > MAX_MESSAGE_BYTES || message.contains('\0') {
            return Err(invalid("message is too long or contains NUL"));
        }
        let dir = self.repo_dir(i.repo.as_deref()).await?;
        let mut add = vec![s("add"), s("--")];
        add.extend(i.paths.iter().cloned());
        self.git.run(&dir, &add, None, 64 * 1024).await?;
        let mut commit = vec![
            s("commit"),
            s("--no-verify"),
            s("--no-gpg-sign"),
            s("-F"),
            s("-"),
            s("--only"),
            s("--"),
        ];
        commit.extend(i.paths);
        self.git
            .run(&dir, &commit, Some(message), 64 * 1024)
            .await?;
        let head = self
            .git
            .run(
                &dir,
                &[s("log"), s("-1"), s("--format=%H%x1f%s")],
                None,
                64 * 1024,
            )
            .await?;
        let line = head.stdout.trim();
        let (hash, subject) = line.split_once('\u{1f}').unwrap_or((line, ""));
        Ok(json_out(json!({"commit": hash, "subject": subject})))
    }

    async fn show(&self, ctx: &ToolContext, i: ShowIn) -> Result<ToolOutput, ToolError> {
        check_paths(&i.paths)?;
        let rev = i.rev.unwrap_or_else(|| "HEAD".to_string());
        check_rev(&rev)?;
        let dir = self.repo_dir(i.repo.as_deref()).await?;
        let mut args = vec![
            s("show"),
            s("--no-color"),
            s("--no-ext-diff"),
            s("--no-textconv"),
        ];
        if i.stat == Some(true) {
            args.push(s("--stat"));
        }
        args.push(rev);
        args.push(s("--"));
        args.extend(i.paths);
        let cap = cap_for(ctx, i.max_bytes);
        let out = self.git.run(&dir, &args, None, cap).await?;
        Ok(ToolOutput::text(capped_text(
            out.stdout,
            cap,
            out.truncated,
        )))
    }
}

fn json_out(data: Value) -> ToolOutput {
    ToolOutput {
        content: vec![ContentPart::Json { data }],
        spill: None,
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
