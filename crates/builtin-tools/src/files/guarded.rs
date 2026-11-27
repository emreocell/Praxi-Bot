//! The document tools (`doc-tools`) and developer tools (`dev-tools`) behind the path rules
//! of the `fs_*` tools.
//!
//! Both crates validate paths on their own terms and know nothing of a bot's file access
//! scope. Here every path an input names goes through the same checks as `fs_read` /
//! `fs_write` (the bot's folders, credential and daemon-data denylists, symlink and UNC
//! handling, other bots' workspaces), and the validated absolute path replaces the one the
//! model wrote before the inner tool sees it. An output the model left out (`doc_convert`'s
//! default file, `doc_preview`'s default folder) is named and checked like one it wrote.
//! Tools that change a file also take an undo checkpoint first, check the workspace write
//! budget against the real size of what they write, and mark the file in the provenance
//! ledger when the run holds untrusted content or the file was made from an untrusted
//! input, exactly as `fs_write` does.
//!
//! The git tools run inside the repository's own folder: the bot's folders when it has
//! any, otherwise (with "This computer" access) the repository's top level, which must
//! pass the same folder vetting as a workspace (never the home folder or a folder
//! holding credentials). `git` is spawned directly, never through the shell. A
//! [`RepoPolicy`] hands the git tools the rest of the `fs_*` rules: the repository's top
//! level, git directory and common directory are vetted like any path, files `fs_read`
//! refuses are left out of diffs, `git_show` and `git_status` and cannot be committed, and a
//! commit or a branch change holds the same lock, budget, checkpoint and provenance rules
//! as a file write. `process_list` shows every program on the computer, so it is only
//! offered to bots with access to "This computer".

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tool_registry::{ContentPart, Tool, ToolContext, ToolError, ToolOutput, ToolSpec};

use super::{
    Follow, ROOT_UNRESOLVED_MSG, Shared, blocking, check_daemon_write, check_write_budget,
    credential_blocked, denied, file_mutations, validate_mutation, validate_path, with_base_dir,
    workspace_blocked, workspace_refusal,
};

/// How a tool uses a path field of its input.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Access {
    /// An existing file that is only read.
    Read,
    /// A file that is created (or replaced when the tool allows it).
    Create,
    /// An existing file that is rewritten in place.
    Edit,
    /// A folder new files are put in. No checkpoint: it would copy the whole folder.
    Folder,
}

type Field = (&'static str, Access);

fn fields_of(tool: &str) -> &'static [Field] {
    use Access::{Create, Edit, Folder, Read};
    match tool {
        "docx_write" | "xlsx_write" => &[("path", Create)],
        "xlsx_append_rows" | "xlsx_update_cells" | "docx_append" => &[("path", Edit)],
        "csv_transform" | "doc_convert" => &[("path", Read), ("output_path", Create)],
        "doc_preview" => &[("path", Read), ("output_dir", Folder)],
        // pdf_extract, docx_read, xlsx_read, pptx_read, csv_read, json_query
        _ => &[("path", Read)],
    }
}

/// The output the tool picks itself when the input names none, as the inner tool would: next
/// to the (already validated) source. `None` when the tool has no such default or the input
/// is too odd for one (the inner tool then reports its own error).
fn default_output(
    tool: &str,
    key: &str,
    object: &serde_json::Map<String, Value>,
    source: &Path,
) -> Option<String> {
    let source = source.to_string_lossy();
    match (tool, key) {
        ("doc_convert", "output_path") => {
            let to = object.get("to")?.as_str()?.trim().trim_start_matches('.');
            (!to.is_empty() && to.len() <= 8 && to.chars().all(|c| c.is_ascii_alphanumeric()))
                .then(|| doc_tools::default_convert_output(&source, to))
        }
        ("doc_preview", "output_dir") => {
            Some(doc_tools::default_preview_dir(&source)).filter(|d| !d.is_empty())
        }
        _ => None,
    }
}

enum Backend {
    /// A document tool: the settings it starts from. The tool is built for each call, so its
    /// write checks (daemon data, the bot's quota) and the conversion workspace belong to the
    /// calling bot.
    Doc(Box<doc_tools::DocToolsConfig>),
    /// A git tool: the allowed roots belong to the calling bot, so it is built for each call.
    Git(&'static str),
    /// `process_list`: no paths involved.
    Plain(Arc<dyn Tool>),
}

/// One guarded tool: the inner tool's own spec, the path rules of the file tools in front.
pub(super) struct GuardedTool {
    spec: ToolSpec,
    shared: Arc<Shared>,
    backend: Backend,
}

/// The document tools, guarded.
pub(super) fn office_tools(shared: &Arc<Shared>) -> Vec<Arc<dyn Tool>> {
    let cfg = Box::new(doc_tools::DocToolsConfig::default());
    doc_tools::tools(&cfg)
        .into_iter()
        .map(|tool| {
            Arc::new(GuardedTool {
                spec: tool.spec().clone(),
                shared: Arc::clone(shared),
                backend: Backend::Doc(cfg.clone()),
            }) as Arc<dyn Tool>
        })
        .collect()
}

/// The developer tools (git and `process_list`), guarded.
pub(super) fn developer_tools(shared: &Arc<Shared>) -> Vec<Arc<dyn Tool>> {
    let placeholder = dev_tools::DevToolsConfig::new(Vec::new());
    dev_tools::tools(&placeholder)
        .into_iter()
        .map(|tool| {
            let spec = tool.spec().clone();
            let backend = match dev_tools::GIT_TOOL_NAMES
                .iter()
                .find(|n| **n == spec.name.as_str())
            {
                Some(name) => Backend::Git(name),
                None => Backend::Plain(tool),
            };
            Arc::new(GuardedTool {
                spec,
                shared: Arc::clone(shared),
                backend,
            }) as Arc<dyn Tool>
        })
        .collect()
}

/// A call's view of the shared state under the calling bot's file access scope.
fn scope_for(shared: &Arc<Shared>, ctx: &ToolContext) -> Result<Arc<Shared>, ToolError> {
    let sh = match &shared.access {
        Some(access) => {
            let scope = access.scope(ctx.bot_id.as_deref());
            Arc::new(shared.scoped(&scope, ctx.bot_id.as_deref()))
        }
        None => Arc::clone(shared),
    };
    if sh.roots_unresolved {
        return Err(denied(ROOT_UNRESOLVED_MSG));
    }
    Ok(sh)
}

/// What [`prepare`] decided before the inner tool runs.
struct Prepared {
    input: Value,
    /// Undo checkpoints taken for the files the tool will change.
    checkpoints: Vec<String>,
    /// Those files, for the provenance ledger.
    changed: Vec<PathBuf>,
    /// The part of `changed` that is new content made from the input files (not an edit in place).
    outputs: Vec<PathBuf>,
    /// The files the tool reads.
    inputs: Vec<PathBuf>,
}

/// Validate every path field of `input` (the output the tool would pick itself included),
/// take undo checkpoints for what will change and swap the validated absolute paths into the
/// input.
fn prepare(sh: &Shared, run: &str, tool: &str, mut input: Value) -> Result<Prepared, ToolError> {
    let mut prepared = Prepared {
        input: Value::Null,
        checkpoints: Vec::new(),
        changed: Vec::new(),
        outputs: Vec::new(),
        inputs: Vec::new(),
    };
    // A malformed input is the inner tool's to report; nothing was validated or changed.
    let Some(object) = input.as_object_mut() else {
        prepared.input = input;
        return Ok(prepared);
    };
    let mut resolved = Vec::new();
    let mut source: Option<PathBuf> = None;
    for (key, access) in fields_of(tool) {
        let raw = match object.get(*key).and_then(Value::as_str) {
            Some(raw) => raw.to_owned(),
            // Left out: the tool writes to its default place, which is judged like any other.
            None => match source
                .as_deref()
                .and_then(|src| default_output(tool, key, object, src))
            {
                Some(raw) => raw,
                None => continue,
            },
        };
        let follow = match access {
            Access::Read | Access::Edit => Follow::Content,
            Access::Create | Access::Folder => Follow::Write,
        };
        let path = validate_path(sh, &raw, follow)?;
        if *access != Access::Read {
            validate_mutation(sh, &raw, &path)?;
            // The size is not known before the tool runs; this catches a bot already over its
            // limit. The writer checks again with the real size just before it writes.
            check_write_budget(sh, &path, 0)?;
        } else if *key == "path" {
            source = Some(path.clone());
            prepared.inputs.push(path.clone());
        }
        resolved.push((*key, *access, raw, path));
    }
    let undo = |prepared: &mut Prepared| {
        for id in prepared.checkpoints.drain(..) {
            sh.checkpoints.discard(&id);
        }
    };
    for (key, access, raw, path) in resolved {
        if matches!(access, Access::Create | Access::Edit) {
            match sh.checkpoint(run, tool, &[(Path::new(&raw), &path)]) {
                Ok((id, _)) => prepared.checkpoints.push(id),
                Err(e) => {
                    undo(&mut prepared);
                    return Err(e);
                }
            }
        }
        if access != Access::Read {
            prepared.changed.push(path.clone());
        }
        if matches!(access, Access::Create | Access::Folder) {
            prepared.outputs.push(path.clone());
        }
        object.insert(
            key.to_owned(),
            Value::String(path.to_string_lossy().into_owned()),
        );
    }
    prepared.input = input;
    Ok(prepared)
}

/// Seal the undo checkpoints of a finished call and mark what an untrusted run wrote, or what
/// was made from an untrusted file.
fn finish(
    sh: &Shared,
    ctx: &ToolContext,
    prepared: &Prepared,
    result: Result<ToolOutput, ToolError>,
) -> Result<ToolOutput, ToolError> {
    let mut out = match result {
        Ok(out) => out,
        Err(e) => {
            // The tools write their result in one step at the end, so a failed call
            // changed nothing and its checkpoint protects nothing.
            for id in &prepared.checkpoints {
                sh.checkpoints.discard(id);
            }
            return Err(e);
        }
    };
    for id in &prepared.checkpoints {
        sh.record_checkpoint_after(id)?;
    }
    if let Some(ledger) = sh.provenance.as_deref() {
        let ledger_ref: Option<&dyn crate::provenance::Provenance> = Some(ledger);
        // A run that holds untrusted content marks everything it wrote; a clean run marks what
        // it made from an untrusted file (a converted download is still the download).
        let marked = if ctx.exposure_untrusted {
            &prepared.changed
        } else if prepared
            .inputs
            .iter()
            .any(|p| crate::provenance::is_untrusted(ledger_ref, p))
        {
            &prepared.outputs
        } else {
            &prepared.changed[..0]
        };
        for path in marked {
            crate::provenance::mark_path(
                ledger,
                path,
                path.is_dir(),
                "written by a tool in an untrusted run",
            );
        }
    }
    if let Some(id) = prepared.checkpoints.first()
        && let Some(ContentPart::Text { text }) = out.content.first_mut()
    {
        let _ = write!(text, "\n(checkpoint {id})");
    }
    Ok(out)
}

/// The write check handed to the document writers: the daemon boundary, then the bot's write
/// budget with the number of bytes the write really adds.
fn write_guard(sh: &Arc<Shared>) -> doc_tools::WriteGuard {
    let scope = std::panic::AssertUnwindSafe(Arc::clone(sh));
    doc_tools::WriteGuard::sized(move |path, added| {
        // Name the whole wrapper so the closure captures it, not the field.
        let scope = &scope;
        // Defense in depth: the writers refuse the daemon data directory and native CLI
        // configuration themselves, whatever path reaches them.
        check_daemon_write(&scope.0.data_dir, path).map_err(|e| e.to_string())?;
        check_write_budget(&scope.0, Path::new(path), added).map_err(|e| e.to_string())
    })
}

/// The read check handed to `doc_convert` / `doc_preview`: the same rule as `fs_read`.
fn read_guard(sh: &Arc<Shared>) -> doc_tools::WriteGuard {
    let scope = std::panic::AssertUnwindSafe(Arc::clone(sh));
    doc_tools::WriteGuard::new(move |path| {
        let scope = &scope;
        validate_path(&scope.0, path, Follow::Content)
            .map(|_| ())
            .map_err(|e| e.to_string())
    })
}

/// A commit, or a branch change that creates or switches (it rewrites the work tree).
fn git_mutates(tool: &str, input: &Value) -> bool {
    match tool {
        "git_commit" => true,
        "git_branch" => input.get("action").and_then(Value::as_str) != Some("list"),
        _ => false,
    }
}

/// Roots the git tools may use and the validated repository path to hand them.
fn git_target(sh: &Shared, mutates: bool, input: &mut Value) -> Result<Vec<PathBuf>, ToolError> {
    let Some(object) = input.as_object_mut() else {
        return Ok(Vec::new());
    };
    let wanted = match object.get("repo").and_then(Value::as_str) {
        Some(repo) => repo.to_owned(),
        None => sh
            .workdir
            .as_ref()
            .ok_or_else(|| ToolError::InvalidInput("repo is required".into()))?
            .to_string_lossy()
            .into_owned(),
    };
    let repo = validate_path(sh, &wanted, Follow::Content)?;
    if mutates {
        validate_mutation(sh, &wanted, &repo)?;
    }
    let roots = if sh.allowed_roots.is_empty() {
        // "This computer": the repository's top level must be a folder a bot may work in. It is
        // found by walking up, so it can be far above the folder the call named (a `.git` in
        // the shared workspaces folder): vet it like a path of its own, which also refuses
        // other bots' workspaces.
        let top = repo
            .ancestors()
            .find(|dir| dir.join(".git").exists())
            .unwrap_or(&repo)
            .to_path_buf();
        if let Some(why) = workspace_refusal(&top, &sh.data_dir) {
            return Err(denied(&format!("git tools cannot use this folder: {why}")));
        }
        let top_text = top.to_string_lossy().into_owned();
        let top = validate_path(sh, &top_text, Follow::Content)?;
        if mutates {
            validate_mutation(sh, &top_text, &top)?;
        }
        vec![top]
    } else {
        sh.allowed_roots.clone()
    };
    object.insert(
        "repo".to_owned(),
        Value::String(repo.to_string_lossy().into_owned()),
    );
    Ok(roots)
}

/// The `fs_*` rules, as the git tools see them for one call.
struct RepoPolicy {
    sh: Arc<Shared>,
    run: String,
    untrusted_run: bool,
}

impl std::fmt::Debug for RepoPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RepoPolicy")
    }
}

impl dev_tools::GitPolicy for RepoPolicy {
    fn check_dir(&self, dir: &Path, mutates: bool) -> Result<(), String> {
        let raw = dir.to_string_lossy().into_owned();
        let resolved = validate_path(&self.sh, &raw, Follow::Content).map_err(|e| e.to_string())?;
        if mutates {
            validate_mutation(&self.sh, &raw, &resolved).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    fn is_protected(&self, path: &Path) -> bool {
        credential_blocked(&self.sh, path, path) || workspace_blocked(&self.sh, path)
    }

    fn begin_mutation(
        &self,
        op: &'static str,
        repo: &Path,
        control_files: &[PathBuf],
        added_bytes: u64,
    ) -> Result<Box<dyn dev_tools::MutationScope>, String> {
        check_write_budget(&self.sh, repo, added_bytes).map_err(|e| e.to_string())?;
        let checkpoint = if control_files.is_empty() {
            None
        } else {
            let paths: Vec<(&Path, &Path)> = control_files
                .iter()
                .map(|p| (p.as_path(), p.as_path()))
                .collect();
            let (id, _note) = self
                .sh
                .checkpoint(&self.run, op, &paths)
                .map_err(|e| e.to_string())?;
            Some(id)
        };
        Ok(Box::new(RepoScope {
            sh: Arc::clone(&self.sh),
            checkpoint,
            untrusted_run: self.untrusted_run,
        }))
    }
}

/// A commit or branch change in progress: its undo checkpoint and the provenance of what it
/// rewrote.
struct RepoScope {
    sh: Arc<Shared>,
    checkpoint: Option<String>,
    untrusted_run: bool,
}

impl dev_tools::MutationScope for RepoScope {
    fn finish(
        self: Box<Self>,
        ok: bool,
        work_tree: Option<&Path>,
    ) -> Result<Option<String>, String> {
        // A switch in a run that holds untrusted content rewrote files from a repository the
        // run may not trust: they are marked like what `fs_write` would have written.
        if let (Some(tree), true, Some(ledger)) =
            (work_tree, self.untrusted_run, self.sh.provenance.as_deref())
        {
            crate::provenance::mark_path(
                ledger,
                tree,
                true,
                "files switched by git in an untrusted run",
            );
        }
        let Some(id) = &self.checkpoint else {
            return Ok(None);
        };
        if !ok {
            // git failed: nothing it was going to change was changed.
            self.sh.checkpoints.discard(id);
            return Ok(None);
        }
        self.sh
            .record_checkpoint_after(id)
            .map_err(|e| e.to_string())?;
        Ok(Some(id.clone()))
    }
}

#[async_trait]
impl Tool for GuardedTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolContext, input: Value) -> Result<ToolOutput, ToolError> {
        let sh = scope_for(&self.shared, ctx)?;
        let base = sh.workdir.clone();
        let ctx = ctx.clone();
        let name = self.spec.name.clone();
        let handle = tokio::runtime::Handle::current();
        match &self.backend {
            Backend::Plain(tool) => {
                // Every program on the computer, with the names other users chose: only for a bot
                // that has this computer, never one confined to folders.
                if !sh.allowed_roots.is_empty() {
                    return Err(denied(
                        "process_list shows every program on this computer, so it is only available to bots with access to \"This computer\"",
                    ));
                }
                tool.call(&ctx, input).await
            }
            Backend::Doc(base_cfg) => {
                let mut cfg = (**base_cfg).clone();
                blocking(move || {
                    // Held from the checkpoint to the sealed result, like the file tools do,
                    // so a host restore never sees a half-finished change.
                    let lock = file_mutations(&sh.data_dir);
                    let _mutation = lock.read();
                    with_base_dir(base, || {
                        let prepared = prepare(&sh, &ctx.run_id, &name, input)?;
                        cfg.write_guard = Some(write_guard(&sh));
                        // Converted outputs stay in the bot's workspace (its first folder without
                        // one); without either the converters refuse to run.
                        cfg.convert.output_root = sh
                            .bot_workspace
                            .clone()
                            .or_else(|| sh.allowed_roots.first().cloned());
                        cfg.convert.read_guard = Some(read_guard(&sh));
                        let tool = doc_tools::tools(&cfg)
                            .into_iter()
                            .find(|t| t.spec().name == name)
                            .ok_or_else(|| ToolError::Execution("tool is not available".into()))?;
                        let result = handle.block_on(tool.call(&ctx, prepared.input.clone()));
                        finish(&sh, &ctx, &prepared, result)
                    })
                })
                .await
            }
            Backend::Git(tool_name) => {
                let tool_name = *tool_name;
                let mutates = git_mutates(tool_name, &input);
                blocking(move || {
                    // A commit or branch change holds the lock the file tools hold while they
                    // change something, so a host restore never sees it half done.
                    let lock = file_mutations(&sh.data_dir);
                    let _mutation = mutates.then(|| lock.read());
                    with_base_dir(base, || {
                        let mut input = input;
                        let roots = git_target(&sh, mutates, &mut input)?;
                        let policy = Arc::new(RepoPolicy {
                            sh: Arc::clone(&sh),
                            run: ctx.run_id.clone(),
                            untrusted_run: ctx.exposure_untrusted,
                        });
                        let cfg = dev_tools::DevToolsConfig::new(roots).with_policy(policy);
                        let tool = dev_tools::tools(&cfg)
                            .into_iter()
                            .find(|t| t.spec().name == tool_name)
                            .ok_or_else(|| ToolError::Execution("tool is not available".into()))?;
                        handle.block_on(tool.call(&ctx, input))
                    })
                })
                .await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;

    use serde_json::json;
    use tool_registry::{Effect, PermissionCategory};

    use super::super::{OptionalTools, guarded_tools};
    use super::*;
    use crate::{FsAccessProvider, FsScope};

    struct Scope(FsScope);
    impl FsAccessProvider for Scope {
        fn scope(&self, _bot: Option<&str>) -> FsScope {
            self.0.clone()
        }
    }

    #[derive(Default)]
    struct Ledger(parking_lot::Mutex<Vec<String>>);
    impl crate::provenance::Provenance for Ledger {
        fn is_marked(&self, name: &str) -> bool {
            self.0.lock().iter().any(|m| m == name)
        }
        fn mark(&self, name: &str, _tree: bool, _origin: &str) {
            self.0.lock().push(name.to_owned());
        }
    }

    const ALL: OptionalTools = OptionalTools {
        documents: true,
        developer: true,
    };

    fn tools_for(
        data: &Path,
        scope: FsScope,
        home: &Path,
        ledger: Option<Arc<Ledger>>,
    ) -> Vec<Arc<dyn Tool>> {
        guarded_tools(
            data,
            &[],
            &[],
            Some(Arc::new(Scope(scope))),
            Some(home.to_path_buf()),
            ledger.map(|l| l as Arc<dyn crate::provenance::Provenance>),
            ALL,
        )
    }

    fn tool(tools: &[Arc<dyn Tool>], name: &str) -> Arc<dyn Tool> {
        tools
            .iter()
            .find(|t| t.spec().name == name)
            .unwrap_or_else(|| panic!("{name} missing"))
            .clone()
    }

    fn text(out: &ToolOutput) -> String {
        match &out.content[0] {
            ContentPart::Text { text } => text.clone(),
            other => panic!("unexpected {other:?}"),
        }
    }

    fn json_part(out: &ToolOutput) -> Value {
        match &out.content[1] {
            ContentPart::Json { data } => data.clone(),
            other => panic!("unexpected {other:?}"),
        }
    }

    fn ctx() -> ToolContext {
        ToolContext {
            run_id: "run-1".into(),
            bot_id: Some("bot-a".into()),
            ..ToolContext::default()
        }
    }

    #[test]
    fn every_document_and_developer_tool_is_offered_with_its_own_metadata() {
        let data = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let tools = tools_for(data.path(), FsScope::Computer, work.path(), None);
        let expected = [
            ("pdf_extract", Effect::Read, PermissionCategory::Fs),
            ("docx_read", Effect::Read, PermissionCategory::Fs),
            ("docx_write", Effect::Write, PermissionCategory::Fs),
            ("docx_append", Effect::Write, PermissionCategory::Fs),
            ("xlsx_read", Effect::Read, PermissionCategory::Fs),
            ("xlsx_write", Effect::Write, PermissionCategory::Fs),
            ("xlsx_append_rows", Effect::Write, PermissionCategory::Fs),
            ("xlsx_update_cells", Effect::Write, PermissionCategory::Fs),
            ("pptx_read", Effect::Read, PermissionCategory::Fs),
            ("csv_read", Effect::Read, PermissionCategory::Fs),
            ("csv_transform", Effect::Write, PermissionCategory::Fs),
            ("json_query", Effect::Read, PermissionCategory::Fs),
            ("doc_convert", Effect::Write, PermissionCategory::Fs),
            ("doc_preview", Effect::Write, PermissionCategory::Fs),
            ("git_status", Effect::Read, PermissionCategory::Fs),
            ("git_diff", Effect::Read, PermissionCategory::Fs),
            ("git_log", Effect::Read, PermissionCategory::Fs),
            ("git_show", Effect::Read, PermissionCategory::Fs),
            ("git_branch", Effect::Write, PermissionCategory::Fs),
            ("git_commit", Effect::Write, PermissionCategory::Fs),
            ("process_list", Effect::Read, PermissionCategory::System),
        ];
        assert_eq!(tools.len(), expected.len());
        for (name, effect, category) in expected {
            let spec = tool(&tools, name).spec().clone();
            assert!(spec.is_valid(), "{name}");
            assert_eq!((spec.effect, spec.category), (effect, category), "{name}");
            assert!(
                !spec.content.executes,
                "{name} starts no program of the model's choosing"
            );
        }
        // The tools that read documents or repositories are untrusted and private, the ones
        // that only write are not.
        assert!(tool(&tools, "xlsx_read").spec().content.reads_private);
        assert!(tool(&tools, "git_diff").spec().content.ingests_untrusted);
        assert!(
            !tool(&tools, "xlsx_append_rows")
                .spec()
                .content
                .reads_private
        );
    }

    #[test]
    fn the_families_are_switched_separately() {
        let data = tempfile::tempdir().unwrap();
        let names = |set| {
            guarded_tools(data.path(), &[], &[], None, None, None, set)
                .iter()
                .map(|t| t.spec().name.clone())
                .collect::<Vec<_>>()
        };
        let docs = names(OptionalTools {
            documents: true,
            developer: false,
        });
        assert!(
            docs.contains(&"docx_append".to_owned()) && !docs.contains(&"git_status".to_owned())
        );
        let dev = names(OptionalTools {
            documents: false,
            developer: true,
        });
        assert!(dev.contains(&"process_list".to_owned()) && !dev.contains(&"xlsx_read".to_owned()));
        assert_eq!(
            names(OptionalTools {
                documents: false,
                developer: false
            })
            .len(),
            0
        );
    }

    #[tokio::test]
    async fn folder_access_confines_document_tools_like_fs_read() {
        let data = tempfile::tempdir().unwrap();
        let inside = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let tools = tools_for(
            data.path(),
            FsScope::Folders(vec![inside.path().to_path_buf()]),
            inside.path(),
            None,
        );
        let ok = inside.path().join("data.csv");
        fs::write(&ok, "a,b\n1,2\n").unwrap();
        let no = outside.path().join("data.csv");
        fs::write(&no, "a,b\n9,9\n").unwrap();

        let out = tool(&tools, "csv_read")
            .call(&ctx(), json!({"path": ok}))
            .await
            .unwrap();
        assert_eq!(json_part(&out)["total_rows"], 1);
        // A relative path starts in the bot's folder.
        let out = tool(&tools, "csv_read")
            .call(&ctx(), json!({"path": "data.csv"}))
            .await
            .unwrap();
        assert_eq!(json_part(&out)["total_rows"], 1);
        let err = tool(&tools, "csv_read")
            .call(&ctx(), json!({"path": no}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::PermissionDenied(_)), "{err}");
        // The output of a transform is confined too, and nothing is written when it is refused.
        let target = outside.path().join("out.csv");
        let err = tool(&tools, "csv_transform")
            .call(
                &ctx(),
                json!({"path": ok, "ops": [], "output_path": target}),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::PermissionDenied(_)), "{err}");
        assert!(!target.exists());
        // A folder bot that names no folder at all has no file access.
        let none = tools_for(
            data.path(),
            FsScope::Folders(Vec::new()),
            inside.path(),
            None,
        );
        assert!(
            tool(&none, "csv_read")
                .call(&ctx(), json!({"path": ok}))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn writers_refuse_the_daemon_data_directory_and_credentials() {
        let data = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let tools = tools_for(data.path(), FsScope::Computer, work.path(), None);
        let blocks = json!([{"type": "paragraph", "text": "planted"}]);
        for target in [
            data.path().join("skills.docx"),
            work.path().join(".ssh/id_rsa.docx"),
        ] {
            let err = tool(&tools, "docx_write")
                .call(&ctx(), json!({"path": target, "blocks": blocks}))
                .await
                .unwrap_err();
            assert!(
                matches!(err, ToolError::PermissionDenied(_)),
                "{target:?}: {err}"
            );
            assert!(!target.exists(), "{target:?}");
        }
        // An ordinary place on the computer works, and the result can be read back.
        let target = work.path().join("note.docx");
        tool(&tools, "docx_write")
            .call(&ctx(), json!({"path": target, "blocks": blocks}))
            .await
            .unwrap();
        let out = tool(&tools, "docx_read")
            .call(&ctx(), json!({"path": target}))
            .await
            .unwrap();
        assert!(text(&out).contains("planted"));
    }

    #[tokio::test]
    async fn edit_tools_change_a_file_in_place_with_an_undo_checkpoint() {
        let data = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let ledger = Arc::new(Ledger::default());
        let tools = tools_for(
            data.path(),
            FsScope::Folders(vec![work.path().to_path_buf()]),
            work.path(),
            Some(ledger.clone()),
        );
        let call = |name: &'static str, input: Value| {
            let tools = tools.clone();
            async move { tool(&tools, name).call(&ctx(), input).await }
        };
        let book = work.path().join("book.xlsx");
        let doc = work.path().join("report.docx");
        call(
            "xlsx_write",
            json!({"path": book, "sheets": [
                {"name": "Data", "rows": [["item", "qty"], ["apple", 3]]},
                {"name": "Other", "rows": [["keep"]]}
            ]}),
        )
        .await
        .unwrap();
        call(
            "docx_write",
            json!({"path": doc, "blocks": [{"type": "heading", "text": "Title"}]}),
        )
        .await
        .unwrap();
        let before = fs::read(&book).unwrap();

        let out = call(
            "xlsx_append_rows",
            json!({"path": "book.xlsx", "sheet": "Data", "rows": [["pear", 5]]}),
        )
        .await
        .unwrap();
        assert!(text(&out).contains("(checkpoint "), "{}", text(&out));
        call(
            "xlsx_update_cells",
            json!({"path": book, "sheet": "Data", "cells": {"B2": 4}}),
        )
        .await
        .unwrap();
        call(
            "docx_append",
            json!({"path": doc, "blocks": [{"type": "paragraph", "text": "More"}]}),
        )
        .await
        .unwrap();

        let data_sheet = call("xlsx_read", json!({"path": book, "sheet": "Data"}))
            .await
            .unwrap();
        let cells = json_part(&data_sheet)["cells"].clone();
        assert_eq!(
            cells,
            json!([["item", "qty"], ["apple", 4.0], ["pear", 5.0]])
        );
        let other = call("xlsx_read", json!({"path": book, "sheet": "Other"}))
            .await
            .unwrap();
        assert_eq!(json_part(&other)["cells"], json!([["keep"]]));
        let read = call("docx_read", json!({"path": doc})).await.unwrap();
        let blocks = json_part(&read)["blocks"].clone();
        assert_eq!(blocks[0]["text"], "Title");
        assert_eq!(blocks[1]["text"], "More");
        assert_ne!(fs::read(&book).unwrap(), before);

        // A bad edit reports its error, leaves the file as it was and keeps no checkpoint.
        let same = fs::read(&book).unwrap();
        assert!(
            call(
                "xlsx_append_rows",
                json!({"path": book, "sheet": "Nope", "rows": [["x"]]})
            )
            .await
            .is_err()
        );
        assert_eq!(fs::read(&book).unwrap(), same);
        // Edits and writes of an untrusted run are marked in the provenance ledger.
        assert!(ledger.0.lock().is_empty());
        let mut untrusted = ctx();
        untrusted.exposure_untrusted = true;
        tool(&tools, "docx_append")
            .call(
                &untrusted,
                json!({"path": doc, "blocks": [{"type": "paragraph", "text": "web"}]}),
            )
            .await
            .unwrap();
        assert!(!ledger.0.lock().is_empty());
    }

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    #[tokio::test]
    async fn git_tools_stay_in_the_bots_folders_and_refuse_credential_folders() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let data = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        for repo in [work.path(), outside.path()] {
            init_repo(repo);
            fs::write(repo.join("a.txt"), "one\n").unwrap();
        }
        let folder_tools = tools_for(
            data.path(),
            FsScope::Folders(vec![work.path().to_path_buf()]),
            work.path(),
            None,
        );
        // No repo given: the bot's folder. A path outside it is refused before git runs.
        let out = tool(&folder_tools, "git_status")
            .call(&ctx(), json!({}))
            .await
            .unwrap();
        assert!(text(&out).contains("a.txt"), "{}", text(&out));
        let err = tool(&folder_tools, "git_status")
            .call(&ctx(), json!({"repo": outside.path()}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::PermissionDenied(_)), "{err}");
        tool(&folder_tools, "git_commit")
            .call(&ctx(), json!({"paths": ["a.txt"], "message": "first"}))
            .await
            .unwrap();
        let log = tool(&folder_tools, "git_log")
            .call(&ctx(), json!({}))
            .await
            .unwrap();
        let ContentPart::Json { data: commits } = &log.content[0] else {
            panic!("git_log returns JSON");
        };
        assert_eq!(commits["commits"][0]["subject"], "first");

        // "This computer": any repository the folder vetting accepts, but not one inside a
        // credential or tool settings folder.
        let computer = tools_for(data.path(), FsScope::Computer, work.path(), None);
        let out = tool(&computer, "git_status")
            .call(&ctx(), json!({"repo": outside.path()}))
            .await
            .unwrap();
        assert!(text(&out).contains("a.txt"), "{}", text(&out));
        let hidden = outside.path().join(".ssh").join("repo");
        fs::create_dir_all(&hidden).unwrap();
        init_repo(&hidden);
        let err = tool(&computer, "git_status")
            .call(&ctx(), json!({"repo": hidden}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::PermissionDenied(_)), "{err}");
        // The daemon's own data folder is never a repository a bot may commit to.
        init_repo(data.path());
        fs::write(data.path().join("x"), "x").unwrap();
        assert!(
            tool(&computer, "git_commit")
                .call(
                    &ctx(),
                    json!({"repo": data.path(), "paths": ["x"], "message": "m"})
                )
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn process_list_runs_and_conversion_fails_closed_without_a_workspace() {
        let data = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let tools = tools_for(data.path(), FsScope::Computer, work.path(), None);
        let out = tool(&tools, "process_list")
            .call(&ctx(), json!({"limit": 3}))
            .await
            .unwrap();
        assert_eq!(out.content.len(), 1, "one JSON part");
        let doc = work.path().join("a.docx");
        fs::write(&doc, b"PK not a real document").unwrap();
        // No bot workspace and no folder: the converters have nowhere to write and refuse.
        let err = tool(&tools, "doc_convert")
            .call(&ctx(), json!({"path": doc, "to": "pdf"}))
            .await
            .unwrap_err();
        assert!(
            matches!(err, ToolError::Execution(_) | ToolError::InvalidInput(_)),
            "{err}"
        );
        assert!(!work.path().join("a.pdf").exists());
    }

    /// A scope source with a workspace layout and a write quota, which remembers what it was
    /// asked to allow.
    struct Provider {
        scope: FsScope,
        workspaces_root: Option<PathBuf>,
        bot_workspace: Option<PathBuf>,
        /// A write adding more bytes than this is refused.
        quota: Option<u64>,
        writes: parking_lot::Mutex<Vec<(PathBuf, u64)>>,
    }

    impl Provider {
        fn new(scope: FsScope) -> Arc<Self> {
            Arc::new(Self {
                scope,
                workspaces_root: None,
                bot_workspace: None,
                quota: None,
                writes: parking_lot::Mutex::default(),
            })
        }

        fn writes(&self) -> Vec<(PathBuf, u64)> {
            self.writes.lock().clone()
        }
    }

    impl FsAccessProvider for Provider {
        fn scope(&self, _bot: Option<&str>) -> FsScope {
            self.scope.clone()
        }
        fn workspaces_root(&self) -> Option<PathBuf> {
            self.workspaces_root.clone()
        }
        fn bot_workspace(&self, _bot: Option<&str>) -> Option<PathBuf> {
            self.bot_workspace.clone()
        }
        fn check_write(&self, _bot: Option<&str>, target: &Path, added: u64) -> Result<(), String> {
            self.writes.lock().push((target.to_path_buf(), added));
            match self.quota {
                Some(q) if added > q => Err("the workspace is over its limit".into()),
                _ => Ok(()),
            }
        }
    }

    fn tools_with(
        data: &Path,
        provider: &Arc<Provider>,
        home: &Path,
        ledger: Option<Arc<Ledger>>,
    ) -> Vec<Arc<dyn Tool>> {
        guarded_tools(
            data,
            &[],
            &[],
            Some(provider.clone() as Arc<dyn FsAccessProvider>),
            Some(home.to_path_buf()),
            ledger.map(|l| l as Arc<dyn crate::provenance::Provenance>),
            ALL,
        )
    }

    fn has_git() -> bool {
        Command::new("git").arg("--version").output().is_ok()
    }

    /// A repository that can commit without a global git identity.
    fn init_repo(dir: &Path) {
        fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["config", "user.name", "t"]);
        git(dir, &["config", "user.email", "t@example.com"]);
        git(dir, &["config", "commit.gpgsign", "false"]);
    }

    const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

    #[tokio::test]
    async fn process_list_is_only_for_bots_with_this_computer_and_its_output_is_untrusted() {
        let data = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let folders = tools_for(
            data.path(),
            FsScope::Folders(vec![work.path().to_path_buf()]),
            work.path(),
            None,
        );
        let err = tool(&folders, "process_list")
            .call(&ctx(), json!({"limit": 1}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::PermissionDenied(_)), "{err}");
        let computer = tools_for(data.path(), FsScope::Computer, work.path(), None);
        assert!(
            tool(&computer, "process_list")
                .call(&ctx(), json!({"limit": 1}))
                .await
                .is_ok()
        );
        // Another user's program picks its own name: what the tool returns is untrusted text.
        let class = tool(&computer, "process_list").spec().content;
        assert!(class.ingests_untrusted && class.reads_private);
    }

    #[tokio::test]
    async fn default_outputs_are_vetted_and_budgeted_like_named_ones() {
        let data = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let provider = Provider::new(FsScope::Folders(vec![work.path().to_path_buf()]));
        let tools = tools_with(data.path(), &provider, work.path(), None);
        let doc = work.path().join("offer.docx");
        fs::write(&doc, b"PK not a real document").unwrap();
        // No output_path / output_dir: the tool would write next to the source. That place is
        // asked about before the converter starts (it then fails for its own reasons here).
        let _ = tool(&tools, "doc_convert")
            .call(&ctx(), json!({"path": doc, "to": "pdf"}))
            .await;
        let _ = tool(&tools, "doc_preview")
            .call(&ctx(), json!({"path": doc}))
            .await;
        let asked: Vec<PathBuf> = provider.writes().into_iter().map(|(p, _)| p).collect();
        assert!(
            asked
                .iter()
                .any(|p| p.file_name().is_some_and(|n| n == "offer.pdf")),
            "the default output file was not checked: {asked:?}"
        );
        assert!(
            asked
                .iter()
                .any(|p| p.file_name() == work.path().file_name()),
            "the default output folder was not checked: {asked:?}"
        );
        // A folder bot with its quota used up cannot make the default output either.
        let mut full = Provider::new(FsScope::Folders(vec![work.path().to_path_buf()]));
        Arc::get_mut(&mut full).unwrap().quota = Some(0);
        let sheet = work.path().join("in.csv");
        fs::write(&sheet, "a,b\n1,2\n").unwrap();
        let tools = tools_with(data.path(), &full, work.path(), None);
        let err = tool(&tools, "csv_transform")
            .call(
                &ctx(),
                json!({"path": sheet, "ops": [{"op": "limit", "n": 10}], "output_path": work.path().join("out.csv")}),
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("over its limit"),
            "the real size is checked before the write: {err}"
        );
        assert!(!work.path().join("out.csv").exists());
        assert!(
            full.writes()
                .iter()
                .any(|(p, n)| p.ends_with("out.csv") && *n > 0),
            "the quota check saw the real size: {:?}",
            full.writes()
        );
    }

    #[tokio::test]
    async fn an_output_made_from_an_untrusted_file_is_marked_even_in_a_clean_run() {
        /// A ledger that trusts one folder (the test folders sit in the temp dir, which is
        /// untrusted by rule) and remembers what was marked.
        struct Trusting {
            marks: parking_lot::Mutex<Vec<String>>,
            folder: String,
        }
        impl crate::provenance::Provenance for Trusting {
            fn is_marked(&self, name: &str) -> bool {
                self.marks.lock().iter().any(|m| m == name)
            }
            fn mark(&self, name: &str, _tree: bool, _origin: &str) {
                self.marks.lock().push(name.to_owned());
            }
            fn is_trusted_folder(&self, name: &str) -> bool {
                name.starts_with(&self.folder)
            }
        }
        let data = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let ledger = Arc::new(Trusting {
            marks: parking_lot::Mutex::default(),
            folder: crate::provenance::identity(work.path()).unwrap(),
        });
        let provider = Provider::new(FsScope::Folders(vec![work.path().to_path_buf()]));
        let tools = guarded_tools(
            data.path(),
            &[],
            &[],
            Some(provider.clone() as Arc<dyn FsAccessProvider>),
            Some(work.path().to_path_buf()),
            Some(ledger.clone() as Arc<dyn crate::provenance::Provenance>),
            ALL,
        );
        let fetched = work.path().join("fetched.csv");
        let own = work.path().join("own.csv");
        fs::write(
            &fetched, "a,b
1,2
",
        )
        .unwrap();
        fs::write(
            &own, "a,b
3,4
",
        )
        .unwrap();
        // The ledger knows `fetched.csv` came from outside.
        crate::provenance::mark_path(&*ledger, &fetched, false, "fetched");
        let before = ledger.marks.lock().len();
        let clean = ctx();
        assert!(!clean.exposure_untrusted);
        // A copy of the user's own file stays unmarked ...
        tool(&tools, "csv_transform")
            .call(
                &clean,
                json!({"path": own, "ops": [{"op": "limit", "n": 10}], "output_path": work.path().join("own-copy.csv")}),
            )
            .await
            .unwrap();
        assert_eq!(
            ledger.marks.lock().len(),
            before,
            "an own file is not marked"
        );
        // ... what is made from the fetched one is the fetched content, whatever the run holds.
        tool(&tools, "csv_transform")
            .call(
                &clean,
                json!({"path": fetched, "ops": [{"op": "limit", "n": 10}], "output_path": work.path().join("fetched-copy.csv")}),
            )
            .await
            .unwrap();
        assert!(
            ledger.marks.lock().iter().any(|m| Some(m)
                == crate::provenance::identity(&work.path().join("fetched-copy.csv")).as_ref()),
            "{:?}",
            ledger.marks.lock()
        );
    }

    #[tokio::test]
    async fn git_tools_leave_out_credential_files_and_refuse_blanket_commits() {
        if !has_git() {
            return;
        }
        let data = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        init_repo(work.path());
        fs::write(work.path().join("a.txt"), "one\n").unwrap();
        fs::write(work.path().join(".env"), "TOKEN=hunter2\n").unwrap();
        git(work.path(), &["add", "-f", "a.txt", ".env"]);
        git(work.path(), &["commit", "-q", "-m", "init"]);
        for scope in [
            FsScope::Folders(vec![work.path().to_path_buf()]),
            FsScope::Computer,
        ] {
            let tools = tools_for(data.path(), scope.clone(), work.path(), None);
            let call = |name: &'static str, input: Value| {
                let tools = tools.clone();
                async move { tool(&tools, name).call(&ctx(), input).await }
            };
            // The whole tree against the empty tree: the credential file is not in it.
            for input in [
                json!({"rev": EMPTY_TREE}),
                json!({"rev": EMPTY_TREE, "paths": ["."]}),
                json!({"rev": EMPTY_TREE, "stat": true}),
            ] {
                let out = call("git_diff", input).await.unwrap();
                assert!(!text(&out).contains("hunter2"), "{scope:?}: {}", text(&out));
                assert!(text(&out).contains("a.txt"), "{}", text(&out));
            }
            let out = call("git_show", json!({})).await.unwrap();
            assert!(!text(&out).contains("hunter2"), "{scope:?}: {}", text(&out));
            fs::write(work.path().join(".env"), "TOKEN=changed\n").unwrap();
            fs::create_dir_all(work.path().join(".ssh")).unwrap();
            fs::write(work.path().join(".ssh").join("id_rsa"), "KEY\n").unwrap();
            let out = call("git_status", json!({})).await.unwrap();
            assert!(
                !text(&out).contains(".env") && !text(&out).contains(".ssh"),
                "{scope:?}: {}",
                text(&out)
            );
            let out = call("git_diff", json!({})).await.unwrap();
            assert!(!text(&out).contains("changed"), "{scope:?}: {}", text(&out));
            // Naming a protected file is refused; so is a commit that could include one.
            let err = call("git_diff", json!({"paths": [".env"]}))
                .await
                .unwrap_err();
            assert!(matches!(err, ToolError::PermissionDenied(_)), "{err}");
            for paths in [json!(["."]), json!([".env"]), json!(["a.txt", ".ssh"])] {
                let err = call("git_commit", json!({"paths": paths, "message": "m"}))
                    .await
                    .unwrap_err();
                assert!(
                    matches!(
                        err,
                        ToolError::PermissionDenied(_) | ToolError::InvalidInput(_)
                    ),
                    "{scope:?} {paths}: {err}"
                );
            }
            let log = call("git_log", json!({})).await.unwrap();
            let ContentPart::Json { data: commits } = &log.content[0] else {
                panic!("git_log returns JSON");
            };
            assert_eq!(commits["commits"].as_array().unwrap().len(), 1, "{scope:?}");
            git(work.path(), &["checkout", "-q", "--", ".env"]);
            let _ = fs::remove_dir_all(work.path().join(".ssh"));
        }
    }

    #[tokio::test]
    async fn git_refuses_a_repository_that_reaches_other_bots_workspaces_or_credential_folders() {
        if !has_git() {
            return;
        }
        let data = tempfile::tempdir().unwrap();
        // A `.git` in the folder that holds every bot's workspace: a bot working in its own
        // folder must not climb into it and see the others.
        let root = tempfile::tempdir().unwrap();
        init_repo(root.path());
        let own = root.path().join("bot-a");
        let other = root.path().join("bot-b");
        fs::create_dir_all(&own).unwrap();
        fs::create_dir_all(&other).unwrap();
        fs::write(other.join("secret.txt"), "not yours\n").unwrap();
        let mut shared = Provider::new(FsScope::Computer);
        {
            let p = Arc::get_mut(&mut shared).unwrap();
            p.workspaces_root = Some(root.path().to_path_buf());
            p.bot_workspace = Some(own.clone());
        }
        let tools = tools_with(data.path(), &shared, root.path(), None);
        for input in [json!({}), json!({"repo": own})] {
            for name in ["git_status", "git_diff", "git_log"] {
                let err = tool(&tools, name)
                    .call(&ctx(), input.clone())
                    .await
                    .unwrap_err();
                assert!(
                    matches!(err, ToolError::PermissionDenied(_)),
                    "{name} {input}: {err}"
                );
            }
        }

        // "Folder only": a `.git` file or `core.worktree` that points into a credential folder
        // inside the allowed folder is not accepted just because it is inside it.
        let allowed = tempfile::tempdir().unwrap();
        let provider = Provider::new(FsScope::Folders(vec![allowed.path().to_path_buf()]));
        let tools = tools_with(data.path(), &provider, allowed.path(), None);
        let sep = allowed.path().join("sep");
        let gitdir = allowed.path().join(".ssh").join("sep.git");
        fs::create_dir_all(&sep).unwrap();
        fs::create_dir_all(gitdir.parent().unwrap()).unwrap();
        let out = Command::new("git")
            .args(["init", "-q", "--separate-git-dir"])
            .arg(&gitdir)
            .arg(&sep)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let err = tool(&tools, "git_status")
            .call(&ctx(), json!({"repo": sep}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::PermissionDenied(_)), "{err}");
        let repo = allowed.path().join("repo");
        init_repo(&repo);
        let tree = allowed.path().join(".ssh").join("tree");
        fs::create_dir_all(&tree).unwrap();
        git(&repo, &["config", "core.worktree", &tree.to_string_lossy()]);
        let err = tool(&tools, "git_status")
            .call(&ctx(), json!({"repo": repo}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::PermissionDenied(_)), "{err}");
    }

    #[tokio::test]
    async fn git_commit_and_branch_changes_take_the_write_path() {
        if !has_git() {
            return;
        }
        let data = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        init_repo(work.path());
        fs::write(work.path().join("a.txt"), "one\n").unwrap();
        git(work.path(), &["add", "a.txt"]);
        git(work.path(), &["commit", "-q", "-m", "first"]);
        let ledger = Arc::new(Ledger::default());
        let provider = Provider::new(FsScope::Folders(vec![work.path().to_path_buf()]));
        let tools = tools_with(data.path(), &provider, work.path(), Some(ledger.clone()));
        let files = super::super::tools_scoped(
            data.path(),
            &[],
            &[],
            Some(provider.clone() as Arc<dyn FsAccessProvider>),
            Some(work.path().to_path_buf()),
            None,
        );
        let head = |dir: &Path| {
            let out = Command::new("git")
                .args(["log", "-1", "--format=%s"])
                .current_dir(dir)
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
        };

        // A commit is a write: the budget is asked, and the change can be undone.
        fs::write(work.path().join("a.txt"), "two\n").unwrap();
        let out = tool(&tools, "git_commit")
            .call(&ctx(), json!({"paths": ["a.txt"], "message": "second"}))
            .await
            .unwrap();
        let checkpoint = json_part_at(&out, 0)["checkpoint"]
            .as_str()
            .expect("a commit reports its undo checkpoint")
            .to_owned();
        assert_eq!(head(work.path()), "second");
        assert!(
            provider.writes().iter().any(|(_, n)| *n > 0),
            "{:?}",
            provider.writes()
        );
        tool(&files, "checkpoint_restore")
            .call(&ctx(), json!({"id": checkpoint}))
            .await
            .unwrap();
        assert_eq!(
            head(work.path()),
            "first",
            "restoring the checkpoint undoes the commit"
        );

        // Over the quota: nothing is committed.
        let mut full = Provider::new(FsScope::Folders(vec![work.path().to_path_buf()]));
        Arc::get_mut(&mut full).unwrap().quota = Some(0);
        let tight = tools_with(data.path(), &full, work.path(), None);
        let err = tool(&tight, "git_commit")
            .call(&ctx(), json!({"paths": ["a.txt"], "message": "third"}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::PermissionDenied(_)), "{err}");
        assert_eq!(head(work.path()), "first");

        // A switch in a clean run leaves the ledger alone; in a run that holds untrusted
        // content the files it rewrote are marked.
        let marks = ledger.0.lock().len();
        tool(&tools, "git_branch")
            .call(
                &ctx(),
                json!({"action": "create", "name": "feat", "switch": true}),
            )
            .await
            .unwrap();
        assert_eq!(ledger.0.lock().len(), marks);
        let mut untrusted = ctx();
        untrusted.exposure_untrusted = true;
        tool(&tools, "git_branch")
            .call(&untrusted, json!({"action": "switch", "name": "main"}))
            .await
            .unwrap();
        assert!(ledger.0.lock().len() > marks, "{:?}", ledger.0.lock());
        // Listing is a read: no mark, no budget.
        let marks = ledger.0.lock().len();
        tool(&tools, "git_branch")
            .call(&untrusted, json!({"action": "list"}))
            .await
            .unwrap();
        assert_eq!(ledger.0.lock().len(), marks);
    }

    fn json_part_at(out: &ToolOutput, i: usize) -> Value {
        match &out.content[i] {
            ContentPart::Json { data } => data.clone(),
            other => panic!("unexpected {other:?}"),
        }
    }
}
