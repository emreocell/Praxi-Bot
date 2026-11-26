//! The document tools (`doc-tools`) and developer tools (`dev-tools`) behind the path rules
//! of the `fs_*` tools.
//!
//! Both crates validate paths on their own terms and know nothing of a bot's file access
//! scope. Here every path an input names goes through the same checks as `fs_read` /
//! `fs_write` (the bot's folders, credential and daemon-data denylists, symlink and UNC
//! handling, other bots' workspaces), and the validated absolute path replaces the one the
//! model wrote before the inner tool sees it. Tools that change a file also take an undo
//! checkpoint first, honour the workspace write budget and mark the file in the provenance
//! ledger when the run holds untrusted content, exactly as `fs_write` does.
//!
//! The git tools run inside the repository's own folder: the bot's folders when it has
//! any, otherwise (with "This computer" access) the repository's top level, which must
//! pass the same folder vetting as a workspace (never the home folder or a folder
//! holding credentials). `git` is spawned directly, never through the shell.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tool_registry::{ContentPart, Tool, ToolContext, ToolError, ToolOutput, ToolSpec};

use super::{
    Follow, ROOT_UNRESOLVED_MSG, Shared, blocking, check_daemon_write, check_write_budget, denied,
    file_mutations, validate_mutation, validate_path, with_base_dir, workspace_refusal,
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

enum Backend {
    /// A document tool with a fixed configuration.
    Doc(Arc<dyn Tool>),
    /// `doc_convert` / `doc_preview`: the workspace they write into and the input guard
    /// belong to the calling bot, so the tool is built for each call.
    Convert(&'static str, doc_tools::DocToolsConfig),
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
    let data_dir = shared.data_dir.clone();
    let cfg = doc_tools::DocToolsConfig {
        // Defense in depth: the writers refuse the daemon data directory and native CLI
        // configuration themselves, whatever path reaches them.
        write_guard: Some(doc_tools::WriteGuard::new(move |path| {
            check_daemon_write(&data_dir, path).map_err(|e| e.to_string())
        })),
        ..doc_tools::DocToolsConfig::default()
    };
    doc_tools::tools(&cfg)
        .into_iter()
        .map(|tool| {
            let spec = tool.spec().clone();
            let backend = match spec.name.as_str() {
                "doc_convert" => Backend::Convert("doc_convert", cfg.clone()),
                "doc_preview" => Backend::Convert("doc_preview", cfg.clone()),
                _ => Backend::Doc(tool),
            };
            Arc::new(GuardedTool {
                spec,
                shared: Arc::clone(shared),
                backend,
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
}

/// Validate every path field of `input`, take undo checkpoints for what will change and swap
/// the validated absolute paths into the input.
fn prepare(
    sh: &Shared,
    run: &str,
    tool: &str,
    fields: &[Field],
    mut input: Value,
) -> Result<Prepared, ToolError> {
    let mut prepared = Prepared {
        input: Value::Null,
        checkpoints: Vec::new(),
        changed: Vec::new(),
    };
    // A malformed input is the inner tool's to report; nothing was validated or changed.
    let Some(object) = input.as_object_mut() else {
        prepared.input = input;
        return Ok(prepared);
    };
    let mut resolved = Vec::new();
    for (key, access) in fields {
        let Some(raw) = object.get(*key).and_then(Value::as_str) else {
            continue;
        };
        let follow = match access {
            Access::Read | Access::Edit => Follow::Content,
            Access::Create | Access::Folder => Follow::Write,
        };
        let path = validate_path(sh, raw, follow)?;
        if *access != Access::Read {
            validate_mutation(sh, raw, &path)?;
            // The size is not known before the tool runs; this catches a bot already over its limit.
            check_write_budget(sh, &path, 0)?;
        }
        resolved.push((*key, *access, raw.to_owned(), path));
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
        object.insert(
            key.to_owned(),
            Value::String(path.to_string_lossy().into_owned()),
        );
    }
    prepared.input = input;
    Ok(prepared)
}

/// Seal the undo checkpoints of a finished call and mark what an untrusted run wrote.
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
    if ctx.exposure_untrusted
        && let Some(ledger) = sh.provenance.as_deref()
    {
        for path in &prepared.changed {
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

/// Roots the git tools may use and the validated repository path to hand them.
fn git_target(sh: &Shared, tool: &str, input: &mut Value) -> Result<Vec<PathBuf>, ToolError> {
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
    if matches!(tool, "git_commit" | "git_branch") {
        validate_mutation(sh, &wanted, &repo)?;
    }
    let roots = if sh.allowed_roots.is_empty() {
        // "This computer": the repository's top level must be a folder a bot may work in.
        let top = repo
            .ancestors()
            .find(|dir| dir.join(".git").exists())
            .unwrap_or(&repo)
            .to_path_buf();
        if let Some(why) = workspace_refusal(&top, &sh.data_dir) {
            return Err(denied(&format!("git tools cannot use this folder: {why}")));
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
            Backend::Plain(tool) => tool.call(&ctx, input).await,
            Backend::Doc(tool) => {
                let tool = Arc::clone(tool);
                blocking(move || {
                    // Held from the checkpoint to the sealed result, like the file tools do,
                    // so a host restore never sees a half-finished change.
                    let lock = file_mutations(&sh.data_dir);
                    let _mutation = lock.read();
                    with_base_dir(base, || {
                        let prepared = prepare(&sh, &ctx.run_id, &name, fields_of(&name), input)?;
                        let result = handle.block_on(tool.call(&ctx, prepared.input.clone()));
                        finish(&sh, &ctx, &prepared, result)
                    })
                })
                .await
            }
            Backend::Convert(tool_name, base_cfg) => {
                let (tool_name, mut cfg) = (*tool_name, base_cfg.clone());
                blocking(move || {
                    let lock = file_mutations(&sh.data_dir);
                    let _mutation = lock.read();
                    with_base_dir(base, || {
                        let prepared = prepare(&sh, &ctx.run_id, &name, fields_of(&name), input)?;
                        // Outputs stay in the bot's workspace (its first folder without one);
                        // without either the converters refuse to run.
                        cfg.convert.output_root = sh
                            .bot_workspace
                            .clone()
                            .or_else(|| sh.allowed_roots.first().cloned());
                        let scope = std::panic::AssertUnwindSafe(Arc::clone(&sh));
                        cfg.convert.read_guard = Some(doc_tools::WriteGuard::new(move |path| {
                            // Name the whole wrapper so the closure captures it, not the field.
                            let scope = &scope;
                            validate_path(&scope.0, path, Follow::Content)
                                .map(|_| ())
                                .map_err(|e| e.to_string())
                        }));
                        let tool = doc_tools::tools(&cfg)
                            .into_iter()
                            .find(|t| t.spec().name == tool_name)
                            .ok_or_else(|| ToolError::Execution("tool is not available".into()))?;
                        let result = handle.block_on(tool.call(&ctx, prepared.input.clone()));
                        finish(&sh, &ctx, &prepared, result)
                    })
                })
                .await
            }
            Backend::Git(tool_name) => {
                let tool_name = *tool_name;
                let (input, roots) = blocking({
                    let sh = Arc::clone(&sh);
                    move || {
                        with_base_dir(base, || {
                            let mut input = input;
                            let roots = git_target(&sh, tool_name, &mut input)?;
                            Ok((input, roots))
                        })
                    }
                })
                .await?;
                let cfg = dev_tools::DevToolsConfig::new(roots);
                let tool = dev_tools::tools(&cfg)
                    .into_iter()
                    .find(|t| t.spec().name == tool_name)
                    .ok_or_else(|| ToolError::Execution("tool is not available".into()))?;
                tool.call(&ctx, input).await
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
            git(repo, &["init", "-q"]);
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
        git(&hidden, &["init", "-q"]);
        let err = tool(&computer, "git_status")
            .call(&ctx(), json!({"repo": hidden}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::PermissionDenied(_)), "{err}");
        // The daemon's own data folder is never a repository a bot may commit to.
        git(data.path(), &["init", "-q"]);
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
}
