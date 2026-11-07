//! Tier 1: hermetic scenarios (scripted provider, fake tools, no network).

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use permission_engine::Policy;
use provider_catalog::{ChatProvider, ChatRequest, MessageRole};
use regex::Regex;
use tool_registry::{Tool, ToolRegistry};

use crate::Result;
use crate::fake::{FakeTool, ScriptedProvider};
use crate::harness::{TurnConfig, TurnTrace, run_turn};
use crate::metrics::RunRecord;
use crate::scenario::{Expect, PolicyPreset, RequestExpect, Scenario, TextMatch, load_dir};

/// Model reference used by tier-1 runs.
pub const FAKE_MODEL: &str = "fake/m";

/// Outcome of one scenario.
pub struct ScenarioResult {
    /// Metrics record (success = no failed assertion).
    pub record: RunRecord,
}

fn policy(p: PolicyPreset) -> Policy {
    match p {
        PolicyPreset::Full => Policy::full(),
        PolicyPreset::ApproveDestructive => Policy::approve_destructive(),
        PolicyPreset::ReadOnly => Policy::read_only(),
        PolicyPreset::ReadAndDraft => Policy::read_and_draft(),
    }
}

/// Run one scenario and evaluate its assertions.
///
/// # Errors
/// Harness failures (not assertion failures: those land in the record).
pub async fn run_scenario(s: &Scenario) -> Result<ScenarioResult> {
    let provider = ScriptedProvider::new(s.script.clone());
    let registry = ToolRegistry::new();
    let mut counters: BTreeMap<String, Arc<AtomicU64>> = BTreeMap::new();
    for spec in &s.tools {
        let (tool, runs) = FakeTool::from_spec(spec);
        registry.register(tool as Arc<dyn Tool>);
        counters.insert(spec.name.clone(), runs);
    }
    // The plan scenarios use builtin-tools' `todo_write` (Meta, always allowed);
    // only that tool is added so other scenarios see the same tool set as before.
    for tool in builtin_tools::meta::tools() {
        if tool.spec().name == "todo_write" {
            registry.register(tool);
        }
    }
    let trace = run_turn(TurnConfig {
        model: FAKE_MODEL.into(),
        persona: "You are T.".into(),
        profile_extra: s.profile_toml.clone(),
        policy: policy(s.policy),
        guards: s.guards.clone(),
        prices: std::collections::HashMap::new(),
        provider: Arc::clone(&provider) as Arc<dyn ChatProvider>,
        registry,
        prompt: s.user.clone(),
        reactions: s.reactions.clone(),
        timeout: Duration::from_millis(s.timeout_ms.unwrap_or(20_000)),
        retry_attempts: 1,
    })
    .await?;
    let runs: BTreeMap<String, u64> = counters
        .iter()
        .map(|(k, v)| (k.clone(), v.load(Ordering::SeqCst)))
        .collect();
    let failures = evaluate(&s.expect, &trace, &runs, &provider.requests());
    Ok(ScenarioResult {
        record: RunRecord::from_trace(1, &s.name, FAKE_MODEL, &trace, failures),
    })
}

/// Run every scenario of a directory whose name contains `filter`.
///
/// # Errors
/// Load failures or harness failures.
pub async fn run_dir(dir: &Path, filter: Option<&str>) -> Result<Vec<RunRecord>> {
    let mut out = Vec::new();
    for s in load_dir(dir)? {
        if filter.is_some_and(|f| !s.name.contains(f)) {
            continue;
        }
        out.push(run_scenario(&s).await?.record);
    }
    Ok(out)
}

/// Check `text` against a [`TextMatch`], pushing a message per failed assertion.
pub fn evaluate_text(what: &str, m: &TextMatch, text: &str, out: &mut Vec<String>) {
    for c in &m.contains {
        if !text.contains(c.as_str()) {
            out.push(format!("{what}: expected to contain {c:?}, got {text:?}"));
        }
    }
    for c in &m.not_contains {
        if text.contains(c.as_str()) {
            out.push(format!(
                "{what}: expected NOT to contain {c:?}, got {text:?}"
            ));
        }
    }
    if let Some(r) = &m.regex {
        match Regex::new(r) {
            Ok(re) if re.is_match(text) => {}
            Ok(_) => out.push(format!("{what}: expected to match /{r}/, got {text:?}")),
            Err(e) => out.push(format!("{what}: invalid regex /{r}/: {e}")),
        }
    }
    if let Some(e) = &m.equals
        && e != text
    {
        out.push(format!("{what}: expected {e:?}, got {text:?}"));
    }
}

fn role_name(r: &MessageRole) -> &'static str {
    match r {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
    }
}

fn request_failures(e: &RequestExpect, requests: &[ChatRequest], out: &mut Vec<String>) {
    let at = e.request;
    let Some(req) = requests.get(at) else {
        out.push(format!(
            "request {at}: the model was only called {} time(s)",
            requests.len()
        ));
        return;
    };
    if let Some(roles) = &e.roles {
        let got: Vec<&str> = req.messages.iter().map(|m| role_name(&m.role)).collect();
        if got != *roles {
            out.push(format!("request {at}: roles {got:?}, expected {roles:?}"));
        }
    }
    let tool_msgs: Vec<String> = req
        .messages
        .iter()
        .filter(|m| m.role == MessageRole::Tool)
        .map(|m| m.content.as_str().unwrap_or_default().to_owned())
        .collect();
    for c in &e.tool_messages_contain {
        if !tool_msgs.iter().any(|m| m.contains(c.as_str())) {
            out.push(format!("request {at}: no tool message contains {c:?}"));
        }
    }
    for c in &e.tool_messages_not_contain {
        if tool_msgs.iter().any(|m| m.contains(c.as_str())) {
            out.push(format!("request {at}: a tool message contains {c:?}"));
        }
    }
    if let Some(max) = e.max_tool_message_bytes
        && let Some(big) = tool_msgs.iter().find(|m| m.len() > max)
    {
        out.push(format!(
            "request {at}: tool message of {} bytes exceeds {max}",
            big.len()
        ));
    }
    if let Some(c) = &e.last_user_contains {
        let last = req
            .messages
            .iter()
            .rev()
            .find(|m| m.role == MessageRole::User)
            .and_then(|m| m.content.as_str())
            .unwrap_or_default();
        if !last.contains(c.as_str()) {
            out.push(format!(
                "request {at}: last user message {last:?} lacks {c:?}"
            ));
        }
    }
    let offered: Vec<&str> = req
        .tools
        .iter()
        .filter_map(|d| d["function"]["name"].as_str())
        .collect();
    for t in &e.tools_offered {
        if !offered.contains(&t.as_str()) {
            out.push(format!(
                "request {at}: tool {t:?} not offered (offered: {offered:?})"
            ));
        }
    }
    for t in &e.tools_not_offered {
        if offered.contains(&t.as_str()) {
            out.push(format!("request {at}: tool {t:?} must not be offered"));
        }
    }
}

/// Compare a finished turn against the expectations; returns the failures.
#[must_use]
pub fn evaluate(
    e: &Expect,
    trace: &TurnTrace,
    runs: &BTreeMap<String, u64>,
    requests: &[ChatRequest],
) -> Vec<String> {
    let mut f = Vec::new();
    if trace.timed_out {
        f.push("run hit the wall-clock cap and was cancelled".into());
    }
    if let Some(st) = &e.state
        && trace.state.as_str() != st
    {
        f.push(format!(
            "state: expected {st}, got {}",
            trace.state.as_str()
        ));
    }
    if let Some(m) = e.model_calls
        && !m.check(u64::from(trace.llm_calls))
    {
        f.push(format!(
            "model_calls: got {} (expected {m:?})",
            trace.llm_calls
        ));
    }
    if let Some(seq) = &e.tool_calls {
        let got: Vec<&str> = trace.calls.iter().map(|c| c.tool.as_str()).collect();
        if got != seq.iter().map(String::as_str).collect::<Vec<_>>() {
            f.push(format!("tool_calls: got {got:?}, expected {seq:?}"));
        }
    }
    for (tool, n) in &e.tool_runs {
        let got = runs.get(tool).copied().unwrap_or(0);
        if got != *n {
            f.push(format!(
                "tool_runs: {tool} executed {got} time(s), expected {n}"
            ));
        }
    }
    if let Some(perms) = &e.permissions {
        let got: Vec<(String, String)> = trace
            .calls
            .iter()
            .map(|c| (c.tool.clone(), c.decision.clone()))
            .collect();
        let want: Vec<(String, String)> = perms
            .iter()
            .map(|p| (p.tool.clone(), p.decision.clone()))
            .collect();
        if got != want {
            f.push(format!("permissions: got {got:?}, expected {want:?}"));
        }
    }
    if let Some(m) = &e.final_text {
        evaluate_text("final_text", m, &trace.final_text, &mut f);
    }
    if let Some(m) = &e.error {
        evaluate_text("error", m, trace.error.as_deref().unwrap_or(""), &mut f);
    }
    for r in &e.requests {
        request_failures(r, requests, &mut f);
    }
    f
}
