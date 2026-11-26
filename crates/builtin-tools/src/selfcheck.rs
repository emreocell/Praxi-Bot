//! `tool_self_check`: run one harmless call per tool and say which work.
//!
//! A tool can be registered and still fail the moment it is used: a mail account
//! without a password, a data folder the daemon's user cannot write, a search
//! provider that is not reachable, a missing shell. The check makes one fixed,
//! read-only sample call for every tool that has one, and reports `ok`,
//! `failed` (with the error and a plain fix hint) or `skipped` (with the
//! reason), plus a few host facts (data folder, home folder, shell).
//!
//! What it never does: call a tool that changes anything, pass model-written
//! input to a tool (the samples are fixed here), or return content (only status
//! words and the tool's own error text, shortened). Tools that need a browser,
//! a desktop or an owner decision are reported as skipped; `bot doctor` covers
//! those.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tool_registry::{
    ContentPart, Effect, PermissionCategory, Tool, ToolContext, ToolError, ToolOutput,
    ToolRegistry, ToolSpec,
};

use crate::util::{parse, spec};

/// Name of the tool.
pub const TOOL_SELF_CHECK: &str = "tool_self_check";

/// Longest time one sample call may take.
const CALL_TIMEOUT: Duration = Duration::from_secs(20);
/// Longest error text kept per tool.
const MAX_DETAIL_CHARS: usize = 300;

/// Fixed, harmless sample calls: `(tool, input)`. Only tools with `Effect::Read`
/// are ever called, whatever this table says.
const SAMPLES: &[(&str, &str)] = &[
    ("time_now", "{}"),
    ("calculate", r#"{"expression":"1+1"}"#),
    ("fs_list", r#"{"path":"."}"#),
    ("notes_read", "{}"),
    ("schedule_list", "{}"),
    ("account_list", "{}"),
    ("secrets_list", "{}"),
    ("email_identities", "{}"),
    ("email_list_folders", "{}"),
    ("email_search", r#"{"limit":1}"#),
    ("web_search", r#"{"query":"praxi self check"}"#),
    ("knowledge_search", r#"{"query":"self check"}"#),
];

/// Tools that wait, ask a person or act on the world even though they are
/// marked read-only: never called by the check.
const NEVER_CALLED: &[&str] = &[
    TOOL_SELF_CHECK,
    "wait",
    "email_wait_for",
    "ask_user",
    "browser_wait",
    "desktop_wait",
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckIn {
    #[serde(default)]
    tool: Option<String>,
}

/// The `tool_self_check` tool.
pub struct SelfCheck {
    spec: ToolSpec,
    registry: ToolRegistry,
    data_dir: PathBuf,
}

/// Build `tool_self_check` over `registry` (the one the loop calls tools from).
#[must_use]
pub fn tool(registry: ToolRegistry, data_dir: PathBuf) -> Arc<dyn Tool> {
    Arc::new(SelfCheck {
        spec: spec(
            TOOL_SELF_CHECK,
            "Check which of your tools really work. Runs one harmless, read-only sample call per tool (nothing is changed or sent except a fixed search or a mailbox check) and returns, per tool, `ok`, `failed` with the error and a fix hint, or `skipped` with the reason, plus facts about this computer (data folder, home folder, shell). Run it when a tool failed in a way you cannot explain, before a long task, or when the owner asks 'which tools work?'. Tell the owner the failed ones with their fix hints in plain words. `tool` limits the check to one tool. Example: {} or {\"tool\":\"email_search\"}",
            json!({"type":"object","additionalProperties":false,"properties":{
                "tool":{"type":"string","description":"Check only this tool."}
            }}),
            Effect::Read,
            PermissionCategory::Meta,
        ),
        registry,
        data_dir,
    })
}

/// How one tool fared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The sample call worked.
    Ok,
    /// The sample call failed.
    Failed {
        /// The error, shortened.
        detail: String,
        /// What to do about it.
        hint: String,
    },
    /// Not called, with the reason.
    Skipped(String),
}

/// A plain sentence on what to do about `error`, from what the tool said.
#[must_use]
pub fn fix_hint(tool: &str, error: &str) -> String {
    let e = error.to_lowercase();
    let mail = tool.starts_with("email_");
    if mail && (e.contains("no mail account") || e.contains("no e-mail account")) {
        return "this bot has no mail identity: the owner adds one under Settings > E-mail (a mailbox of its own, bound to this bot)".into();
    }
    if mail && e.contains("unknown account") {
        return "the named mail account does not exist or is not this bot's: call email_identities and use a name from it".into();
    }
    if let Some((_, fix)) = error.split_once("Fix: ") {
        return fix.trim().to_owned();
    }
    if e.contains("permission denied") || e.contains("access is denied") || e.contains("read-only")
    {
        return "the daemon's user cannot use this folder or file: the owner should check who runs the daemon and the permissions of its data folder".into();
    }
    if e.contains("no such file") || e.contains("not found") || e.contains("cannot find") {
        return "a path or program this tool needs is missing on this computer: check the data folder and the installed programs (run `bot doctor`)".into();
    }
    if e.contains("timed out") || e.contains("timeout") || e.contains("unreachable") {
        return "the network or the service did not answer in time: check this computer's internet access, then try again".into();
    }
    if e.contains("api key") || e.contains("unauthorized") || e.contains("401") || e.contains("403")
    {
        return "the service refused the key: the owner should check or replace it in Settings"
            .into();
    }
    if e.contains("not configured") || e.contains("no search backend") || e.contains("not enabled")
    {
        return "this tool is not set up: the owner configures it in Settings (see the user guide for this tool)".into();
    }
    "read the error above; run `bot doctor` on the computer the daemon runs on for the full environment check".into()
}

fn shorten(text: &str) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    flat.chars().take(MAX_DETAIL_CHARS).collect()
}

/// The sample input for `name`, when there is one.
fn sample(name: &str) -> Option<Value> {
    SAMPLES
        .iter()
        .find(|(n, _)| *n == name)
        .and_then(|(_, input)| serde_json::from_str(input).ok())
}

/// Why `spec`'s tool is not called, if it is not.
fn skip_reason(spec: &ToolSpec) -> Option<String> {
    if NEVER_CALLED.contains(&spec.name.as_str()) {
        return Some("waits, asks a person or checks itself; not called".into());
    }
    if spec.effect != Effect::Read {
        return Some(
            "it changes something (a file, a page, a message), so it is never called by a check"
                .into(),
        );
    }
    if sample(&spec.name).is_none() {
        return Some("it has no safe sample call (it needs a page, a program or an input only you can choose); `bot doctor` covers browsers and desktops".into());
    }
    None
}

/// Run the check for the tools of `registry` (all, or only `only`).
pub async fn run(
    registry: &ToolRegistry,
    ctx: &ToolContext,
    only: Option<&str>,
) -> Vec<(String, Verdict)> {
    let mut specs = registry.list();
    specs.sort_by(|a, b| a.name.cmp(&b.name));
    let mut out = Vec::new();
    for spec in specs {
        if only.is_some_and(|o| o != spec.name) {
            continue;
        }
        let verdict = match (skip_reason(&spec), registry.get(&spec.name)) {
            (_, None) => Verdict::Skipped("the tool disappeared".into()),
            (Some(why), _) => Verdict::Skipped(why),
            (None, Some(tool)) if !tool.visible_to(ctx.bot_id.as_deref()) => Verdict::Skipped(
                "not offered to this bot (for mail tools: the bot has no mail identity)".into(),
            ),
            (None, Some(tool)) => {
                let input = sample(&spec.name).unwrap_or(Value::Null);
                match tokio::time::timeout(CALL_TIMEOUT, tool.call(ctx, input)).await {
                    Ok(Ok(_)) => Verdict::Ok,
                    Ok(Err(e)) => {
                        let detail = shorten(&e.to_string());
                        let hint = fix_hint(&spec.name, &detail);
                        Verdict::Failed { detail, hint }
                    }
                    Err(_) => Verdict::Failed {
                        detail: format!("no answer within {} seconds", CALL_TIMEOUT.as_secs()),
                        hint: fix_hint(&spec.name, "timed out"),
                    },
                }
            }
        };
        out.push((spec.name.clone(), verdict));
    }
    out
}

/// Facts about the computer that explain most "registered but fails" cases.
fn environment(data_dir: &std::path::Path) -> Vec<Value> {
    let mut facts = Vec::new();
    let mut fact = |name: &str, ok: bool, detail: String, hint: &str| {
        facts.push(json!({
            "check": name,
            "status": if ok { "ok" } else { "failed" },
            "detail": detail,
            "hint": if ok { Value::Null } else { Value::String(hint.to_owned()) },
        }));
    };
    let probe = data_dir.join(format!(".self-check-{}", std::process::id()));
    let writable = std::fs::write(&probe, b"ok").is_ok();
    let _ = std::fs::remove_file(&probe);
    fact(
        "data folder is writable",
        writable,
        data_dir.display().to_string(),
        "the user that runs the daemon cannot write its data folder: fix its owner or permissions (a daemon started by another user, or a read-only disk, is the usual cause)",
    );
    let home = crate::user_home();
    fact(
        "home folder",
        home.is_some(),
        home.as_ref()
            .map_or_else(|| "not found".to_owned(), |h| h.display().to_string()),
        "the daemon's user has no usable home folder (HOME is unset or not a directory), so file tools cannot reach the owner's files: start the daemon with HOME set",
    );
    let shell = if cfg!(windows) { "cmd" } else { "sh" };
    let found = std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths)
            .any(|dir| dir.join(shell).is_file() || dir.join(format!("{shell}.exe")).is_file())
    });
    fact(
        "shell on PATH",
        found,
        shell.to_owned(),
        "no shell is on the daemon's PATH, so shell_run cannot start: set PATH for the daemon (a systemd unit has a minimal PATH)",
    );
    facts
}

#[async_trait]
impl Tool for SelfCheck {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolContext, input: Value) -> Result<ToolOutput, ToolError> {
        let i: CheckIn = parse(input)?;
        let results = run(&self.registry, ctx, i.tool.as_deref()).await;
        if i.tool.is_some() && results.is_empty() {
            return Err(ToolError::InvalidInput(
                "no such tool (see tool_search)".into(),
            ));
        }
        let count = |f: fn(&Verdict) -> bool| results.iter().filter(|(_, v)| f(v)).count();
        let tools: Vec<Value> = results
            .iter()
            .map(|(name, v)| match v {
                Verdict::Ok => json!({"tool": name, "status": "ok"}),
                Verdict::Failed { detail, hint } => {
                    json!({"tool": name, "status": "failed", "error": detail, "fix_hint": hint})
                }
                Verdict::Skipped(why) => json!({"tool": name, "status": "skipped", "reason": why}),
            })
            .collect();
        let data = json!({
            "summary": {
                "ok": count(|v| matches!(v, Verdict::Ok)),
                "failed": count(|v| matches!(v, Verdict::Failed { .. })),
                "skipped": count(|v| matches!(v, Verdict::Skipped(_))),
            },
            "environment": environment(&self.data_dir),
            "tools": tools,
            "next_step": "Tell the owner which tools failed, with the fix hint of each, in plain words. Skipped tools were not tested; do not claim they work or fail.",
        });
        Ok(ToolOutput {
            content: vec![ContentPart::Json { data }],
            spill: None,
            exposure: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Probe {
        spec: ToolSpec,
        result: Result<(), &'static str>,
    }

    fn probe_tool(name: &str, effect: Effect, result: Result<(), &'static str>) -> Arc<dyn Tool> {
        Arc::new(Probe {
            spec: ToolSpec {
                name: name.into(),
                description: "probe".into(),
                input_schema: json!({"type":"object"}),
                effect,
                category: PermissionCategory::Meta,
                platforms: vec![tool_registry::Platform::All],
                content: tool_registry::ContentClass::none(),
            },
            result,
        })
    }

    #[async_trait]
    impl Tool for Probe {
        fn spec(&self) -> &ToolSpec {
            &self.spec
        }
        async fn call(&self, _: &ToolContext, _: Value) -> Result<ToolOutput, ToolError> {
            self.result
                .map(|()| ToolOutput::text("fine"))
                .map_err(|e| ToolError::Execution(e.into()))
        }
    }

    #[tokio::test]
    async fn only_read_tools_with_a_sample_are_called_and_failures_carry_a_hint() {
        let reg = ToolRegistry::new();
        assert!(reg.register(probe_tool("time_now", Effect::Read, Ok(()))));
        assert!(reg.register(probe_tool(
            "fs_list",
            Effect::Read,
            Err("Permission denied (os error 13)")
        )));
        assert!(reg.register(probe_tool(
            "email_search",
            Effect::Read,
            Err("imap: login failed. Fix: the server refused the login")
        )));
        // Has a sample but changes things: never called.
        assert!(reg.register(probe_tool("fs_write", Effect::Write, Err("must not run"))));
        // Read-only but no sample: skipped.
        assert!(reg.register(probe_tool(
            "browser_snapshot",
            Effect::Read,
            Err("must not run")
        )));
        let results = run(&reg, &ToolContext::default(), None).await;
        let get = |n: &str| {
            results
                .iter()
                .find(|(name, _)| name == n)
                .unwrap()
                .1
                .clone()
        };
        assert_eq!(get("time_now"), Verdict::Ok);
        let Verdict::Failed { hint, .. } = get("fs_list") else {
            panic!("fs_list should fail")
        };
        assert!(hint.contains("permissions"), "{hint}");
        let Verdict::Failed { hint, .. } = get("email_search") else {
            panic!("email_search should fail")
        };
        assert_eq!(hint, "the server refused the login");
        assert!(matches!(get("fs_write"), Verdict::Skipped(_)));
        assert!(matches!(get("browser_snapshot"), Verdict::Skipped(_)));
    }

    #[tokio::test]
    async fn the_tool_reports_a_summary_and_the_environment() {
        let dir = tempfile::tempdir().unwrap();
        let reg = ToolRegistry::new();
        assert!(reg.register(probe_tool("time_now", Effect::Read, Ok(()))));
        let t = tool(reg, dir.path().to_path_buf());
        let out = t.call(&ToolContext::default(), json!({})).await.unwrap();
        let ContentPart::Json { data } = &out.content[0] else {
            panic!("expected JSON")
        };
        assert_eq!(data["summary"]["ok"], 1);
        assert_eq!(data["summary"]["failed"], 0);
        let env = data["environment"].as_array().unwrap();
        assert_eq!(env[0]["check"], "data folder is writable");
        assert_eq!(env[0]["status"], "ok");
        // A single tool, and an unknown one.
        let one = t
            .call(&ToolContext::default(), json!({"tool":"time_now"}))
            .await
            .unwrap();
        assert!(matches!(&one.content[0], ContentPart::Json { .. }));
        let e = t
            .call(&ToolContext::default(), json!({"tool":"nope"}))
            .await
            .unwrap_err();
        assert!(matches!(e, ToolError::InvalidInput(_)));
    }

    #[test]
    fn a_data_folder_that_cannot_be_written_is_reported_with_a_hint() {
        let facts = environment(std::path::Path::new("/nonexistent-dir-for-self-check"));
        assert_eq!(facts[0]["status"], "failed");
        assert!(facts[0]["hint"].as_str().unwrap().contains("permissions"));
    }
}
