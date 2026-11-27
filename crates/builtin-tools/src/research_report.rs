//! `web_research_report`: turns the model's cited text into a Markdown file in the bot's
//! workspace (`research/<date>-<title>-<id>.md`).
//!
//! The text is checked against the sources `web_research` really read (see
//! [`web_tools::research::assemble_report`]): a `[n]` that no source has is refused with the fix,
//! citations are renumbered in order of use and the `Sources` list is written from what was
//! read, never from the text. The file itself is written by the registered `fs_write` tool, so
//! the path rules, the bot's folders, the write quota, checkpoints and the untrusted-origin
//! mark of a run that read pages are the ones every other file write gets. This tool adds no
//! second way to write a file.
//!
//! A bot without a workspace folder gets the finished report back in the answer instead of a
//! file in some other place.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tool_registry::{
    Effect, PermissionCategory, Tool, ToolContext, ToolError, ToolOutput, ToolRegistry, ToolSpec,
};
use web_tools::research::{self, ReportError};

use crate::files::FileGate;
use crate::util::{exec_err, parse, spec};

/// Name of the report tool.
pub const WEB_RESEARCH_REPORT: &str = web_tools::REPORT_TOOL;

/// Longest report text accepted, in characters.
const MAX_TEXT_CHARS: usize = 60_000;
/// Longest title accepted, in characters.
const MAX_TITLE_CHARS: usize = 200;
/// Folder of the bot's workspace the reports go to.
const REPORT_DIR: &str = "research";

#[derive(Deserialize)]
struct In {
    research_id: String,
    title: String,
    text: String,
}

struct ReportTool {
    spec: ToolSpec,
    registry: ToolRegistry,
    gate: Arc<FileGate>,
    pack_dir: PathBuf,
}

/// Build the report tool. `registry` is where `fs_write` is looked up when the tool runs;
/// `pack_dir` is where `web_research` keeps its source lists (see
/// [`web_tools::research::pack_dir`]).
pub(crate) fn tool(
    registry: ToolRegistry,
    gate: Arc<FileGate>,
    pack_dir: PathBuf,
) -> Arc<dyn Tool> {
    Arc::new(ReportTool {
        spec: spec(
            WEB_RESEARCH_REPORT,
            "Save a researched answer as a Markdown report in the workspace (research/ folder) and get \
             its path. Call it after web_research with its research_id, a title and the text, citing \
             sources as [n] after each claim (only numbers web_research gave). Do not write a Sources \
             section: it is added from what was really read, and citations are renumbered. An unknown \
             number is refused with the valid ones; fix the text and call again.",
            json!({"type":"object","required":["research_id","title","text"],"properties":{
                "research_id":{"type":"string","description":"The research_id web_research returned"},
                "title":{"type":"string","description":"Report title (max 200 characters)"},
                "text":{"type":"string","description":"The report in Markdown, with [n] citations"}
            }}),
            Effect::Write,
            PermissionCategory::Fs,
        ),
        registry,
        gate,
        pack_dir,
    })
}

fn refused(e: &ReportError) -> ToolError {
    ToolError::InvalidInput(e.to_string())
}

#[async_trait]
impl Tool for ReportTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolContext, input: Value) -> Result<ToolOutput, ToolError> {
        let i: In = parse(input)?;
        if i.title.chars().count() > MAX_TITLE_CHARS || i.text.chars().count() > MAX_TEXT_CHARS {
            return Err(ToolError::InvalidInput(format!(
                "title is limited to {MAX_TITLE_CHARS} characters and text to {MAX_TEXT_CHARS}"
            )));
        }
        let pack = research::get_pack(Some(&self.pack_dir), i.research_id.trim()).ok_or_else(|| {
            ToolError::InvalidInput(
                "unknown research_id: it is the id web_research returned (r-...); if it is lost, run web_research again".into(),
            )
        })?;
        let date = research::today();
        let report =
            research::assemble_report(&pack, &i.title, &i.text, &date).map_err(|e| refused(&e))?;
        let summary = format!(
            "{} of {} sources read are cited.\nSummary: {}",
            report.cited, report.read, report.lead
        );

        let dir = self.gate.workspace_dir(ctx.bot_id.as_deref(), REPORT_DIR);
        let writer = self.registry.get("fs_write");
        let (Some(dir), Some(writer)) = (dir, writer) else {
            return Ok(ToolOutput::text(format!(
                "Not saved: this bot has no workspace folder to write to, so the report is below. \
                 Give it to the owner in your answer, or save it with fs_write where the owner \
                 says.\n{summary}\n\n{}",
                report.markdown
            )));
        };
        let rel = research::report_path(&date, &i.title, &pack.id);
        let file = rel.rsplit('/').next().unwrap_or(&rel);
        let path = dir.join(file);
        let written = writer
            .call(
                ctx,
                json!({"path": path.display().to_string(), "content": report.markdown}),
            )
            .await
            .map_err(|e| match e {
                ToolError::Execution(m) => exec_err(format!("the report was not saved: {m}")),
                other => other,
            })?;
        let mut out = ToolOutput::text(format!(
            "Report saved: {} (in the workspace: {rel}).\n{summary}",
            path.display()
        ));
        // What the write recorded about the run (an untrusted-origin mark) holds for this result.
        out.exposure = written.exposure;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tool_registry::ContentPart;
    use web_tools::research::{Pack, PackSource, new_id, put_pack};

    use super::*;
    use crate::files::tools_scoped_with_gate;
    use crate::provenance::Provenance;

    #[derive(Default)]
    struct Ledger(parking_lot::Mutex<Vec<String>>);
    impl Provenance for Ledger {
        fn is_marked(&self, n: &str) -> bool {
            self.0.lock().iter().any(|p| p == n)
        }
        fn mark(&self, n: &str, _tree: bool, _origin: &str) {
            self.0.lock().push(n.to_owned());
        }
    }

    /// One bot with a workspace; "Folder only" scope, so nothing outside is reachable.
    struct Access(PathBuf);
    impl crate::FsAccessProvider for Access {
        fn scope(&self, _bot: Option<&str>) -> crate::FsScope {
            crate::FsScope::Folders(Vec::new())
        }
        fn workspaces_root(&self) -> Option<PathBuf> {
            Some(self.0.clone())
        }
        fn bot_workspace(&self, bot: Option<&str>) -> Option<PathBuf> {
            (bot == Some("bot-a")).then(|| self.0.join("bot-a"))
        }
    }

    struct Rig {
        _tmp: tempfile::TempDir,
        ws: PathBuf,
        ledger: Arc<Ledger>,
        tool: Arc<dyn Tool>,
    }

    fn rig(with_fs_write: bool) -> Rig {
        let tmp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(tmp.path()).unwrap();
        let (root, data) = (base.join("workspaces"), base.join("data"));
        std::fs::create_dir_all(root.join("bot-a")).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        let ledger = Arc::new(Ledger::default());
        let (fs_tools, gate) = tools_scoped_with_gate(
            &data,
            &[],
            &[],
            Some(Arc::new(Access(root.clone()))),
            Some(base.clone()),
            Some(ledger.clone()),
        );
        let registry = ToolRegistry::new();
        if with_fs_write {
            for t in fs_tools {
                registry.register(t);
            }
        }
        let tool = tool(
            registry,
            Arc::new(gate),
            research::pack_dir(&data.join("spill")),
        );
        Rig {
            ws: root.join("bot-a"),
            _tmp: tmp,
            ledger,
            tool,
        }
    }

    fn pack() -> Pack {
        let p = Pack {
            id: new_id(),
            question: "Heat pumps or gas boilers?".into(),
            sources: vec![
                PackSource {
                    n: 1,
                    url: "https://a.example/x".into(),
                    title: "Source A".into(),
                },
                PackSource {
                    n: 2,
                    url: "https://b.example/y".into(),
                    title: "Source B".into(),
                },
            ],
        };
        put_pack(None, &p);
        p
    }

    const TEXT: &str = "Heat pumps usually cost less to run than gas boilers in mild climates \
        because they move heat instead of making it.[2] The installation costs more up front and \
        grants differ by country, so the payback time varies a lot.[1][2]";

    fn ctx(bot: &str, untrusted: bool) -> ToolContext {
        ToolContext {
            run_id: "r1".into(),
            bot_id: Some(bot.into()),
            exposure_untrusted: untrusted,
            ..ToolContext::default()
        }
    }

    fn text(out: &ToolOutput) -> String {
        out.content
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    fn input(p: &Pack, body: &str) -> Value {
        json!({"research_id": p.id, "title": "Heat pumps vs boilers", "text": body})
    }

    fn files_in(dir: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .map(|rd| rd.filter_map(Result::ok).map(|e| e.path()).collect())
            .unwrap_or_default()
    }

    #[test]
    fn the_tool_is_a_workspace_write_that_follows_the_file_rules() {
        let r = rig(true);
        let spec = r.tool.spec();
        assert!(spec.is_valid());
        assert_eq!(spec.name, "web_research_report");
        assert_eq!(spec.effect, Effect::Write);
        assert_eq!(spec.category, PermissionCategory::Fs);
        let c = spec.content;
        assert!(!c.ingests_untrusted && !c.reads_private && !c.egress);
    }

    #[tokio::test]
    async fn a_checked_report_lands_in_the_workspace_with_a_source_list_from_the_pack() {
        let r = rig(true);
        let p = pack();
        let out = r
            .tool
            .call(&ctx("bot-a", false), input(&p, TEXT))
            .await
            .unwrap();
        let said = text(&out);
        let files = files_in(&r.ws.join("research"));
        assert_eq!(files.len(), 1, "{files:?}");
        let name = files[0].file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.ends_with(".md") && name.contains("heat-pumps-vs-boilers"),
            "{name}"
        );
        assert!(said.contains(&files[0].display().to_string()), "{said}");
        assert!(said.contains("2 of 2 sources read are cited"), "{said}");
        assert!(
            said.contains("Summary: Heat pumps usually cost less"),
            "{said}"
        );
        let md = std::fs::read_to_string(&files[0]).unwrap();
        assert!(md.starts_with("# Heat pumps vs boilers\n"), "{md}");
        // [2] was used first, so it is [1] in the file, and the list says which page it is.
        assert!(md.contains("in mild climates because they move heat instead of making it.[1]"));
        assert!(
            md.ends_with(
                "- [1] Source B — https://b.example/y\n- [2] Source A — https://a.example/x\n"
            ),
            "{md}"
        );
        // A run that read no page leaves no untrusted mark in the ledger. (The reply's own
        // exposure is not asserted: a test folder under the system temp dir is outside content
        // by rule.)
        assert!(r.ledger.0.lock().is_empty());
        let _ = out;
    }

    #[tokio::test]
    async fn a_run_that_read_pages_marks_the_report_as_outside_content() {
        let r = rig(true);
        let p = pack();
        let out = r
            .tool
            .call(&ctx("bot-a", true), input(&p, TEXT))
            .await
            .unwrap();
        let files = files_in(&r.ws.join("research"));
        assert_eq!(files.len(), 1);
        assert!(r.ledger.is_marked(&crate::provenance::normalize(&files[0])));
        // The reply says the same, so the next steps of the run are judged accordingly.
        assert!(out.exposure.as_ref().is_some_and(|e| e.ingests_untrusted));
    }

    #[tokio::test]
    async fn the_same_research_written_again_replaces_its_report() {
        let r = rig(true);
        let p = pack();
        let c = ctx("bot-a", false);
        r.tool.call(&c, input(&p, TEXT)).await.unwrap();
        r.tool
            .call(&c, input(&p, &format!("{TEXT} More.")))
            .await
            .unwrap();
        assert_eq!(files_in(&r.ws.join("research")).len(), 1);
    }

    #[tokio::test]
    async fn a_bad_text_or_id_is_refused_with_the_fix_and_writes_nothing() {
        let r = rig(true);
        let p = pack();
        let c = ctx("bot-a", false);
        let unknown = r
            .tool
            .call(&c, input(&p, &format!("{TEXT}[7]")))
            .await
            .unwrap_err();
        assert!(matches!(unknown, ToolError::InvalidInput(_)), "{unknown}");
        assert!(unknown.to_string().contains("[1] to [2]"), "{unknown}");

        let none = r
            .tool
            .call(&c, input(&p, &TEXT.replace(['[', ']'], "")))
            .await
            .unwrap_err();
        assert!(none.to_string().contains("cites no source"), "{none}");

        for bad in ["r-0123456789abcdef", "../../etc/passwd", ""] {
            let e = r
                .tool
                .call(&c, json!({"research_id": bad, "title": "T", "text": TEXT}))
                .await
                .unwrap_err();
            assert!(e.to_string().contains("unknown research_id"), "{bad}: {e}");
        }
        assert!(files_in(&r.ws.join("research")).is_empty());
    }

    #[tokio::test]
    async fn without_a_workspace_the_report_comes_back_in_the_answer_not_in_some_other_folder() {
        let r = rig(true);
        let p = pack();
        let out = r
            .tool
            .call(&ctx("nobody", false), input(&p, TEXT))
            .await
            .unwrap();
        let said = text(&out);
        assert!(
            said.starts_with("Not saved: this bot has no workspace folder"),
            "{said}"
        );
        assert!(said.contains("## Sources"), "{said}");
        assert!(files_in(&r.ws.join("research")).is_empty());
        assert!(!r.ws.parent().unwrap().join("nobody").exists());

        // Nor is there a second way to write when the file tool is missing.
        let r = rig(false);
        let said = text(
            &r.tool
                .call(&ctx("bot-a", false), input(&p, TEXT))
                .await
                .unwrap(),
        );
        assert!(said.starts_with("Not saved"), "{said}");
    }
}
