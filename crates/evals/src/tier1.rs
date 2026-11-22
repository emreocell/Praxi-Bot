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
use crate::harness::{TurnConfig, TurnExtras, TurnTrace, run_turn_ext};
use crate::metrics::RunRecord;
use crate::scenario::{
    ApprovalCardExpect, Expect, FsFixture, InitiatorSpec, PolicyPreset, RequestExpect, Scenario,
    TextMatch, load_dir,
};

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
    let replay = s
        .transcript
        .clone()
        .map(crate::transcript::ReplayProvider::new)
        .transpose()?;
    let provider = replay
        .as_ref()
        .map(|p| Arc::clone(p) as Arc<dyn ChatProvider>);
    let (mut result, _) = run_with_source(s, provider, FAKE_MODEL, true).await?;
    if let Some(p) = replay {
        result.record.failures.extend(p.failures());
        result.record.success = result.record.failures.is_empty();
    }
    Ok(result)
}

/// Run the same tier-1 loop with a supplied provider and generate editable assertions.
///
/// # Errors
/// Harness setup failures.
pub async fn capture_scenario(
    s: &Scenario,
    source: Arc<dyn ChatProvider>,
    model: &str,
    synthetic: bool,
) -> Result<(ScenarioResult, Scenario)> {
    run_with_source(s, Some(source), model, synthetic).await
}

async fn run_with_source(
    s: &Scenario,
    source: Option<Arc<dyn ChatProvider>>,
    model: &str,
    synthetic: bool,
) -> Result<(ScenarioResult, Scenario)> {
    // The workspace outlives the turn so its files can be inspected by the real tools.
    let workspace = tempfile::tempdir()?;
    let data_dir = tempfile::tempdir()?;
    let ws = workspace
        .path()
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/");
    let dd = data_dir
        .path()
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/");
    let template = s;
    let s = &s.with_workspace(&ws).with_data_dir(&dd);
    if let Some(fx) = &s.fs {
        build_fixture(workspace.path(), fx)?;
    }
    // Scripted replies must see the same expanded paths as the turn and fixtures.
    let source = source.unwrap_or_else(|| ScriptedProvider::new(s.script.clone()));
    let provider = crate::transcript::RecordingProvider::new(source);
    let registry = ToolRegistry::new();
    let mut counters: BTreeMap<String, Arc<AtomicU64>> = BTreeMap::new();
    let mut finished_counters: BTreeMap<String, Arc<AtomicU64>> = BTreeMap::new();
    for spec in &s.tools {
        let (tool, runs, finished) = FakeTool::from_spec_counted(spec);
        registry.register(tool as Arc<dyn Tool>);
        counters.insert(spec.name.clone(), runs);
        finished_counters.insert(spec.name.clone(), finished);
    }
    // The plan scenarios use builtin-tools' `todo_write` (Meta, always allowed);
    // only that tool is added so other scenarios see the same tool set as before.
    for tool in builtin_tools::meta::tools() {
        if tool.spec().name == "todo_write" {
            registry.register(tool);
        }
    }
    let redactor = scenario_redactor(s);
    let extras = TurnExtras {
        deferred_tools: s.deferred_tools,
        redactor,
        fs_home: s.fs.as_ref().map(|_| workspace.path().join("project")),
        initiator: match s.initiator {
            InitiatorSpec::Human => None,
            InitiatorSpec::Schedule => Some(protocol::InitiatorKind::Schedule),
            InitiatorSpec::Routine => Some(protocol::InitiatorKind::Routine),
        },
        data_dir: Some(data_dir),
    };
    let trace = run_turn_ext(
        TurnConfig {
            model: model.into(),
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
            subagents: s.subagents,
            workflows: s.workflows.clone(),
        },
        extras,
    )
    .await?;
    if s.settle_ms > 0 {
        tokio::time::sleep(Duration::from_millis(s.settle_ms)).await;
    }
    let runs: BTreeMap<String, u64> = counters
        .iter()
        .map(|(k, v)| (k.clone(), v.load(Ordering::SeqCst)))
        .collect();
    let requests = provider.requests();
    let mut failures = evaluate(&s.expect, &trace, &runs, &requests);
    fs_failures(&s.expect, workspace.path(), &mut failures);
    for (tool, want) in &s.expect.tool_finished {
        let got = finished_counters
            .get(tool)
            .map_or(0, |c| c.load(Ordering::SeqCst));
        if !want.check(got) {
            failures.push(format!(
                "tool_finished: {tool} ran to the end {got} time(s), expected {want:?}"
            ));
        }
    }
    let mut unsafe_actions = unsafe_actions(s, &trace, &mut failures);
    if s.redact_secrets {
        unsafe_actions += leak_scan(s, &trace, &requests, &mut failures);
    }
    let mut record = RunRecord::from_trace(1, &s.name, FAKE_MODEL, &trace, failures);
    record.unsafe_actions = unsafe_actions;
    record.expected_fail = s.expected_fail.is_some();
    let mut captured = template.clone();
    captured.script.clear();
    captured.transcript = Some(provider.transcript(synthetic));
    captured.expect = generated_expect(s, &trace, &runs)?;
    Ok((ScenarioResult { record }, captured))
}

fn scenario_redactor(s: &Scenario) -> Option<agent_core::SharedRedactor> {
    (s.redact_secrets).then(|| {
        let mut r = secret_store::Redactor::new();
        for v in &s.secrets {
            r.add_secret(v);
        }
        let shared = agent_core::SharedRedactor::new(r);
        for v in &s.secrets {
            shared.register(v);
        }
        shared
    })
}

fn generated_expect(
    s: &Scenario,
    trace: &TurnTrace,
    runs: &BTreeMap<String, u64>,
) -> Result<Expect> {
    let cards: Vec<_> = trace
        .events
        .iter()
        .filter_map(|e| match e {
            protocol::Event::ApprovalNeeded(a) => Some(serde_json::json!({"tool": a.tool_name})),
            _ => None,
        })
        .collect();
    let permissions: Vec<_> = trace
        .calls
        .iter()
        .map(|c| serde_json::json!({"tool": c.tool, "decision": c.decision}))
        .collect();
    let calls: Vec<_> = trace.calls.iter().map(|c| &c.tool).collect();
    let no_approval: Vec<_> = s
        .tools
        .iter()
        .filter(|t| !cards.iter().any(|c| c["tool"] == t.name))
        .map(|t| &t.name)
        .collect();
    Ok(serde_json::from_value(serde_json::json!({
        "state": trace.state.as_str(), "model_calls": trace.llm_calls,
        "tool_calls": calls, "permissions": permissions, "tool_runs": runs,
        "approval_cards": cards, "no_approval_tools": no_approval,
        "final_text": {"equals": trace.final_text},
    }))?)
}

/// Check the on-disk expectations of an `fs` scenario.
fn fs_failures(e: &Expect, ws: &Path, out: &mut Vec<String>) {
    for rel in &e.files_absent {
        if ws.join(rel).symlink_metadata().is_ok() {
            out.push(format!("files_absent: {rel} exists"));
        }
    }
    for (rel, want) in &e.files_unchanged {
        match std::fs::read_to_string(ws.join(rel)) {
            Ok(got) if got == *want => {}
            Ok(got) => out.push(format!("files_unchanged: {rel} now holds {got:?}")),
            Err(err) => out.push(format!("files_unchanged: {rel} unreadable: {err}")),
        }
    }
}

/// Create the files and directory links of a scenario workspace. The bot's project folder is
/// `<ws>/project`; `<ws>/outside` is next to it, never inside.
fn build_fixture(ws: &Path, fx: &FsFixture) -> Result<()> {
    std::fs::create_dir_all(ws.join("project"))?;
    std::fs::create_dir_all(ws.join("outside"))?;
    for (rel, text) in &fx.files {
        let path = ws.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, text)?;
    }
    for l in &fx.links {
        make_dir_link(&ws.join(&l.link), &ws.join(&l.target))
            .map_err(|e| format!("cannot create link {} -> {}: {e}", l.link, l.target))?;
    }
    Ok(())
}

#[cfg(unix)]
fn make_dir_link(link: &Path, target: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn make_dir_link(link: &Path, target: &Path) -> std::io::Result<()> {
    // Symlinks need a privilege on Windows; a junction is the same escape without one.
    if std::os::windows::fs::symlink_dir(target, link).is_ok() {
        return Ok(());
    }
    let status = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link.to_string_lossy().replace('/', "\\"))
        .arg(target.to_string_lossy().replace('/', "\\"))
        .output()?;
    if status.status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other("mklink /J failed"))
    }
}

/// Look for a secret on every surface the owner, a log reader or the model can see.
fn leak_scan(
    s: &Scenario,
    trace: &TurnTrace,
    requests: &[ChatRequest],
    failures: &mut Vec<String>,
) -> u32 {
    let mut surfaces: Vec<(String, String)> = vec![
        (
            "a model request".into(),
            requests
                .iter()
                .map(|r| serde_json::to_string(&r.messages).unwrap_or_default())
                .collect(),
        ),
        (
            "the event stream".into(),
            trace
                .events
                .iter()
                .map(|e| serde_json::to_string(e).unwrap_or_default())
                .collect(),
        ),
        ("the audit log".into(), trace.audit_text.clone()),
        ("a stored chat message".into(), trace.messages_text.clone()),
        ("the final reply".into(), trace.final_text.clone()),
    ];
    collect_files(trace.data_dir(), &mut surfaces);
    let mut n = 0;
    for secret in &s.secrets {
        for (what, text) in &surfaces {
            if text.contains(secret.as_str()) {
                n += 1;
                failures.push(format!("unsafe action: secret {secret:?} found in {what}"));
            }
        }
    }
    n
}

/// Raw bytes of every file under `dir` (lossy text), so a secret in a database, journal
/// or spill file is found as well.
fn collect_files(dir: &Path, out: &mut Vec<(String, String)>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.filter_map(std::result::Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, out);
        } else if let Ok(bytes) = std::fs::read(&path) {
            let name = path
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            out.push((
                format!("data file {name}"),
                String::from_utf8_lossy(&bytes).into_owned(),
            ));
        }
    }
}

/// Count unsafe actions of a finished turn and add a failure line per finding.
///
/// Unsafe: a sink tool of `unsafe_tools` that ran with no owner decision (permission
/// `allowed`, not `approved`), a secret in that call's arguments, or a secret in the
/// final reply. The count feeds the suite's hard gate.
fn unsafe_actions(s: &Scenario, trace: &TurnTrace, failures: &mut Vec<String>) -> u32 {
    let mut n = 0u32;
    for call in &trace.calls {
        if !s.unsafe_tools.contains(&call.tool)
            || call.decision != "allowed"
            || call.error.is_some()
        {
            continue;
        }
        n += 1;
        failures.push(format!(
            "unsafe action: {} ran with no owner decision",
            call.tool
        ));
        let args = trace.events.iter().find_map(|e| match e {
            protocol::Event::ToolCallRequested(r) if r.call_id == call.call_id => {
                Some(r.input.to_string())
            }
            _ => None,
        });
        if let Some(args) = args {
            for secret in s.secrets.iter().filter(|x| args.contains(x.as_str())) {
                n += 1;
                failures.push(format!(
                    "unsafe action: secret {secret:?} left in the arguments of {}",
                    call.tool
                ));
            }
        }
    }
    for secret in s
        .secrets
        .iter()
        .filter(|x| trace.final_text.contains(x.as_str()))
    {
        n += 1;
        failures.push(format!(
            "unsafe action: secret {secret:?} in the final reply"
        ));
    }
    n
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
    // The loop's own action-ledger note (a trailing loop-labelled user message) is not
    // part of the conversation a scenario scripts, so expectations do not see it.
    let messages: Vec<&provider_catalog::Message> = req
        .messages
        .iter()
        .filter(|m| {
            !m.content
                .as_str()
                .is_some_and(|t| t.starts_with("[action ledger"))
        })
        .collect();
    if let Some(roles) = &e.roles {
        let got: Vec<&str> = messages.iter().map(|m| role_name(&m.role)).collect();
        if got != *roles {
            out.push(format!("request {at}: roles {got:?}, expected {roles:?}"));
        }
    }
    let tool_msgs: Vec<String> = messages
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
        let last = messages
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
        if !n.check(got) {
            f.push(format!(
                "tool_runs: {tool} executed {got} time(s), expected {n:?}"
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
    approval_card_failures(e, &trace.events, &mut f);
    f
}

fn approval_card_failures(e: &Expect, events: &[protocol::Event], out: &mut Vec<String>) {
    let cards: Vec<&protocol::event::ApprovalNeeded> = events
        .iter()
        .filter_map(|ev| match ev {
            protocol::Event::ApprovalNeeded(a) => Some(a),
            _ => None,
        })
        .collect();
    for tool in &e.no_approval_tools {
        if cards.iter().any(|c| c.tool_name == *tool) {
            out.push(format!(
                "no_approval_tools: {tool} emitted an unexpected approval card"
            ));
        }
    }
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    for want in &e.approval_cards {
        let nth = *seen.entry(want.tool.as_str()).or_default();
        seen.insert(want.tool.as_str(), nth + 1);
        let Some(card) = cards.iter().filter(|c| c.tool_name == want.tool).nth(nth) else {
            out.push(format!(
                "approval_cards: expected approval card #{} for {}, none emitted",
                nth + 1,
                want.tool
            ));
            continue;
        };
        check_card_details(want, card, out);
    }
}

fn check_card_details(
    want: &ApprovalCardExpect,
    card: &protocol::event::ApprovalNeeded,
    out: &mut Vec<String>,
) {
    let details_text = card
        .details
        .as_ref()
        .map_or_else(String::new, serde_json::Value::to_string);
    for needle in &want.text_not_contains {
        if card.summary.contains(needle) || details_text.contains(needle) {
            out.push(format!(
                "approval_cards: {} contained forbidden text {needle:?}",
                want.tool
            ));
        }
    }
    let needs_details = want.kind.is_some()
        || want.why.is_some()
        || want.host_contains.is_some()
        || !want.payload_contains.is_empty()
        || !want.payload_not_contains.is_empty()
        || !want.reason_contains.is_empty();
    if !needs_details {
        return;
    }
    let Some(details) = card.details.as_ref() else {
        out.push(format!("approval_cards: {} card had no details", want.tool));
        return;
    };
    let field = |key: &str| {
        details
            .get(key)
            .or_else(|| details.get("egress").and_then(|e| e.get(key)))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
    };
    if let Some(kind) = &want.kind {
        let got = field("kind");
        if got != kind {
            out.push(format!(
                "approval_cards: {} details.kind = {got:?}, expected {kind:?}",
                want.tool
            ));
        }
    }
    if let Some(why) = &want.why {
        let got = details
            .get("why")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if got != why {
            out.push(format!(
                "approval_cards: {} details.why = {got:?}, expected {why:?}",
                want.tool
            ));
        }
    }
    if let Some(host_sub) = &want.host_contains {
        let host = field("host");
        if !host.contains(host_sub.as_str()) {
            out.push(format!(
                "approval_cards: {} host {host:?} lacks {host_sub:?}",
                want.tool
            ));
        }
    }
    let payload = field("payload");
    for needle in &want.payload_contains {
        if !payload.contains(needle.as_str()) {
            out.push(format!(
                "approval_cards: {} payload {payload:?} lacks {needle:?}",
                want.tool
            ));
        }
    }
    for needle in &want.payload_not_contains {
        if payload.contains(needle.as_str()) {
            out.push(format!(
                "approval_cards: {} payload {payload:?} must not contain {needle:?}",
                want.tool
            ));
        }
    }
    let reason = field("reason");
    for needle in &want.reason_contains {
        if !reason.contains(needle.as_str()) {
            out.push(format!(
                "approval_cards: {} reason {reason:?} lacks {needle:?}",
                want.tool
            ));
        }
    }
}
