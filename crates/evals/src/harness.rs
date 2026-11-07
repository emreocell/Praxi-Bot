//! The shared turn driver: builds an isolated storage + [`AgentLoop`], runs one
//! turn while a simulated user reacts to events, and collects a [`TurnTrace`].

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use agent_core::{
    AgentLoop, ApprovalScope, BotProfile, Guards, LoopConfig, MessageMode, ModelPrice, Providers,
};
use permission_engine::Policy;
use protocol::{BotId, ChatId, Event, ToolCallId, UserId};
use provider_catalog::ChatProvider;
use serde_json::{Value, json};
use storage::{Bot, Chat, Storage, User, now_ms};
use task_runtime::{RetryPolicy, RunState};
use tool_registry::ToolRegistry;

use crate::metrics::{Perception, perception_usage, sum_usage};
use crate::scenario::{Act, GuardOverrides, Reaction, Trigger};
use crate::{Error, Result};

/// The reason string the loop attaches to policy denials.
const POLICY_DENIAL_REASON: &str = "denied by policy";

/// Everything needed to run one turn.
pub struct TurnConfig {
    /// `provider/model` of the executor.
    pub model: String,
    /// System persona.
    pub persona: String,
    /// Extra bot-profile TOML appended after `[models]`.
    pub profile_extra: String,
    /// Global policy.
    pub policy: Policy,
    /// Guard overrides.
    pub guards: GuardOverrides,
    /// Price table keyed by `provider/model`.
    pub prices: HashMap<String, ModelPrice>,
    /// The chat provider.
    pub provider: Arc<dyn ChatProvider>,
    /// Registry holding the tools.
    pub registry: ToolRegistry,
    /// The user message.
    pub prompt: String,
    /// Simulated-user reactions.
    pub reactions: Vec<Reaction>,
    /// Wall-clock cap; the run is cancelled when it elapses.
    pub timeout: Duration,
    /// Model-step retry attempts.
    pub retry_attempts: u32,
}

/// The permission outcome of one tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallRecord {
    /// Loop-assigned call id.
    pub call_id: ToolCallId,
    /// Tool name.
    pub tool: String,
    /// `allowed`, `approved`, `denied_user` or `denied_policy`.
    pub decision: String,
    /// Error text when the call failed.
    pub error: Option<String>,
}

/// What happened during a turn.
pub struct TurnTrace {
    /// Final run state.
    pub state: RunState,
    /// Run error text.
    pub error: Option<String>,
    /// True if the wall-clock cap cancelled the run.
    pub timed_out: bool,
    /// All events in order.
    pub events: Vec<Event>,
    /// Tool calls with their permission outcomes.
    pub calls: Vec<CallRecord>,
    /// Last assistant message.
    pub final_text: String,
    /// Start time (epoch ms).
    pub started_ms: i64,
    /// Wall time.
    pub wall: Duration,
    /// Model calls.
    pub llm_calls: u32,
    /// Prompt tokens.
    pub input_tokens: u64,
    /// Completion tokens.
    pub output_tokens: u64,
    /// Cached tokens.
    pub cached_tokens: u64,
    /// Cost from the price table.
    pub cost_usd: Option<f64>,
    /// Requested calls per tool.
    pub tool_counts: BTreeMap<String, u32>,
    /// Perception ladder usage.
    pub perception: Perception,
    /// Text of every tool result.
    pub tool_results: Vec<String>,
    /// Plain-text trajectory (for the judge).
    pub trajectory: String,
    /// Run directory holder; keeps spill files alive until the trace drops.
    pub storage: Arc<Storage>,
    _dir: tempfile::TempDir,
}

impl TurnTrace {
    /// Directory of the isolated data dir (spill files live under it).
    #[must_use]
    pub fn data_dir(&self) -> &std::path::Path {
        self.storage.data_dir()
    }
}

fn build_config(c: &TurnConfig) -> LoopConfig {
    let mut guards = Guards::default();
    let g = &c.guards;
    if let Some(v) = g.max_steps {
        guards.max_steps = v;
    }
    if let Some(v) = g.repeat_warn {
        guards.repeat_warn = v;
    }
    if let Some(v) = g.repeat_stop {
        guards.repeat_stop = v;
    }
    if let Some(v) = g.max_tokens_per_turn {
        guards.max_tokens_per_turn = v;
    }
    if let Some(v) = g.spill_threshold_bytes {
        guards.spill_threshold_bytes = v;
    }
    if let Some(v) = g.spill_preview_bytes {
        guards.spill_preview_bytes = v;
    }
    if let Some(v) = g.tool_timeout_ms {
        guards.tool_timeout = Duration::from_millis(v);
    }
    if let Some(v) = g.model_timeout_ms {
        guards.model_timeout = Duration::from_millis(v);
    }
    LoopConfig {
        guards,
        global_policy: c.policy.clone(),
        model_retry: RetryPolicy {
            max_attempts: c.retry_attempts.max(1),
            initial_backoff_ms: 500,
            multiplier: 2,
            max_backoff_ms: 8_000,
        },
        prices: c.prices.clone(),
        ..LoopConfig::default()
    }
}

fn profile_toml(c: &TurnConfig) -> String {
    let persona = toml::Value::String(c.persona.clone());
    let model = toml::Value::String(c.model.clone());
    format!(
        "[bot]\nid = \"eval\"\nname = \"Eval\"\npersona = {persona}\n[models]\nexecutor = {model}\n{}",
        c.profile_extra
    )
}

fn seed(storage: &Storage) -> Result<(UserId, BotId, ChatId)> {
    let (user, bot, chat) = (UserId::new(), BotId::new(), ChatId::new());
    storage.insert_user(&User {
        id: user,
        name: "eval".into(),
        created_at_ms: now_ms(),
    })?;
    storage.insert_bot(&Bot {
        id: bot,
        user_id: user,
        name: "eval".into(),
        config: json!({}),
        created_at_ms: now_ms(),
    })?;
    storage.insert_chat(&Chat {
        id: chat,
        user_id: user,
        bot_id: bot,
        title: "eval".into(),
        created_at_ms: now_ms(),
    })?;
    Ok((user, bot, chat))
}

struct Reactor<'a> {
    lp: &'a AgentLoop,
    chat: ChatId,
    user: UserId,
    run: protocol::RunId,
    reactions: &'a [Reaction],
    fired: Vec<bool>,
}

impl Reactor<'_> {
    fn on_event(&mut self, ev: &Event, tool_of: &HashMap<ToolCallId, String>) {
        let (trigger, call_id, tool) = match ev {
            Event::RunStarted(_) => (Trigger::RunStarted, None, String::new()),
            Event::ApprovalNeeded(a) => (
                Trigger::ApprovalNeeded,
                Some(a.call_id),
                a.tool_name.clone(),
            ),
            Event::ToolCallRequested(r) => (
                Trigger::ToolCallRequested,
                Some(r.call_id),
                r.tool_name.clone(),
            ),
            Event::ToolCallCompleted(c) => (
                Trigger::ToolCallCompleted,
                Some(c.call_id),
                tool_of.get(&c.call_id).cloned().unwrap_or_default(),
            ),
            _ => return,
        };
        for (i, r) in self.reactions.iter().enumerate() {
            if r.on != trigger || (r.once && self.fired[i]) {
                continue;
            }
            if r.tool.as_ref().is_some_and(|t| *t != tool) {
                continue;
            }
            self.fired[i] = true;
            let (lp, chat, user, run) = (self.lp.clone(), self.chat, self.user, self.run);
            let (act, text, delay) = (r.act, r.text.clone(), r.delay_ms);
            let fire = move || {
                // Failures (already decided, run finished) are not harness errors.
                let _ = match (act, call_id) {
                    (Act::Approve, Some(id)) => lp.approve(id, ApprovalScope::Once, user),
                    (Act::ApproveChat, Some(id)) => lp.approve(id, ApprovalScope::ThisChat, user),
                    (Act::ApproveAlways, Some(id)) => lp.approve(id, ApprovalScope::Always, user),
                    (Act::Deny, Some(id)) => lp.deny(id, user, text),
                    (Act::Steer, _) => lp
                        .send_user_message(
                            chat,
                            text.as_deref().unwrap_or("please continue"),
                            MessageMode::Steer,
                        )
                        .map(|_| ()),
                    (Act::Cancel, _) => lp.cancel(run),
                    _ => Ok(()),
                };
            };
            if delay > 0 {
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                    fire();
                });
            } else {
                fire();
            }
        }
    }
}

/// Run one turn to completion (or until the wall-clock cap) and collect its trace.
///
/// # Errors
/// Harness failures: storage setup, an invalid profile, or the loop refusing to start.
pub async fn run_turn(cfg: TurnConfig) -> Result<TurnTrace> {
    let dir = tempfile::tempdir()?;
    let storage = Arc::new(Storage::open(dir.path())?);
    let (user, bot, chat) = seed(&storage)?;
    let lp = AgentLoop::new(
        Arc::clone(&storage),
        cfg.registry.clone(),
        Providers::single(Arc::clone(&cfg.provider)),
        build_config(&cfg),
    );
    lp.add_profile(
        bot,
        BotProfile::from_toml_str(&profile_toml(&cfg))
            .map_err(|e| -> Error { e.to_string().into() })?,
    );

    let mut rx = lp.subscribe();
    let started_ms = now_ms();
    let t0 = Instant::now();
    let run = lp.run_turn(chat, &cfg.prompt)?;

    let mut events: Vec<Event> = Vec::new();
    let mut tool_of: HashMap<ToolCallId, String> = HashMap::new();
    let mut reactor = Reactor {
        lp: &lp,
        chat,
        user,
        run,
        reactions: &cfg.reactions,
        fired: vec![false; cfg.reactions.len()],
    };
    let mut timed_out = false;
    let deadline = tokio::time::sleep(cfg.timeout);
    tokio::pin!(deadline);
    let wait = lp.wait(run);
    tokio::pin!(wait);
    let state = loop {
        tokio::select! {
            item = rx.recv() => {
                match item {
                    Ok(i) if i.run_id == run => {
                        let ev = i.envelope.event;
                        if let Event::ToolCallRequested(r) = &ev {
                            tool_of.insert(r.call_id, r.tool_name.clone());
                        }
                        reactor.on_event(&ev, &tool_of);
                        events.push(ev);
                    }
                    // Other runs' events, lag and a closed channel are all ignorable here.
                    Ok(_) | Err(_) => {}
                }
            }
            s = &mut wait => break s?,
            () = &mut deadline, if !timed_out => {
                timed_out = true;
                let _ = lp.cancel(run);
            }
        }
    };
    while let Ok(i) = rx.try_recv() {
        if i.run_id == run {
            events.push(i.envelope.event);
        }
    }
    let wall = t0.elapsed();

    Ok(collect(
        &storage, user, chat, run, state, events, timed_out, wall, started_ms, &cfg.model, dir,
    ))
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn collect(
    storage: &Arc<Storage>,
    user: UserId,
    chat: ChatId,
    run: protocol::RunId,
    state: RunState,
    events: Vec<Event>,
    timed_out: bool,
    wall: Duration,
    started_ms: i64,
    _model: &str,
    dir: tempfile::TempDir,
) -> TurnTrace {
    let mut calls: Vec<CallRecord> = Vec::new();
    let mut tool_counts: BTreeMap<String, u32> = BTreeMap::new();
    for ev in &events {
        match ev {
            Event::ToolCallRequested(r) => {
                *tool_counts.entry(r.tool_name.clone()).or_default() += 1;
                calls.push(CallRecord {
                    call_id: r.call_id,
                    tool: r.tool_name.clone(),
                    decision: "allowed".into(),
                    error: None,
                });
            }
            Event::ApprovalNeeded(a) => set(&mut calls, a.call_id, |c| {
                "asked".clone_into(&mut c.decision);
            }),
            Event::ToolCallApproved(a) => {
                set(&mut calls, a.call_id, |c| {
                    "approved".clone_into(&mut c.decision);
                });
            }
            Event::ToolCallDenied(d) => set(&mut calls, d.call_id, |c| {
                let policy = d.reason.as_deref() == Some(POLICY_DENIAL_REASON);
                (if policy {
                    "denied_policy"
                } else {
                    "denied_user"
                })
                .clone_into(&mut c.decision);
            }),
            Event::ToolCallCompleted(done) => {
                set(&mut calls, done.call_id, |c| {
                    c.error.clone_from(&done.error);
                });
            }
            _ => {}
        }
    }

    let steps = storage.list_steps(run).unwrap_or_default();
    let mut llm_calls = 0u32;
    let mut tool_results: Vec<String> = Vec::new();
    let mut trajectory = String::new();
    for s in &steps {
        match s.name.as_str() {
            "model" => {
                llm_calls += 1;
                if let Some(o) = &s.output {
                    let text = o["text"].as_str().unwrap_or_default();
                    if !text.is_empty() {
                        let _ = writeln!(trajectory, "assistant: {}", clip(text, 800));
                    }
                    for c in o["tool_calls"].as_array().into_iter().flatten() {
                        let _ = writeln!(
                            trajectory,
                            "tool_call: {}({})",
                            c["name"].as_str().unwrap_or("?"),
                            clip(c["arguments"].as_str().unwrap_or(""), 300)
                        );
                    }
                }
            }
            "tools" => {
                for r in s
                    .output
                    .iter()
                    .flat_map(|o| o["results"].as_array().into_iter().flatten())
                {
                    let text = r["text"].as_str().unwrap_or_default();
                    let err = if r["is_error"].as_bool().unwrap_or(false) {
                        " (error)"
                    } else {
                        ""
                    };
                    let _ = writeln!(trajectory, "tool_result{err}: {}", clip(text, 600));
                    tool_results.push(text.to_owned());
                }
            }
            _ => {}
        }
    }

    let audit: Vec<Value> = storage
        .list_audit(user)
        .unwrap_or_default()
        .into_iter()
        .filter(|a| a.action == "usage" && a.run_id == Some(run))
        .map(|a| a.detail)
        .collect();
    let (input_tokens, output_tokens, cached_tokens, cost_usd) = sum_usage(&audit);

    let perception = perception_usage(
        calls.iter().map(|c| c.tool.as_str()),
        tool_results.iter().map(String::as_str),
    );
    let final_text = storage
        .list_messages(chat)
        .unwrap_or_default()
        .into_iter()
        .rev()
        .find(|m| m.role == "assistant")
        .map(|m| m.content)
        .unwrap_or_default();
    let error = storage.get_run(run).ok().flatten().and_then(|r| r.error);

    TurnTrace {
        state,
        error,
        timed_out,
        events,
        calls,
        final_text,
        started_ms,
        wall,
        llm_calls,
        input_tokens,
        output_tokens,
        cached_tokens,
        cost_usd,
        tool_counts,
        perception,
        tool_results,
        trajectory,
        storage: Arc::clone(storage),
        _dir: dir,
    }
}

fn set(calls: &mut [CallRecord], id: ToolCallId, f: impl FnOnce(&mut CallRecord)) {
    if let Some(c) = calls.iter_mut().rev().find(|c| c.call_id == id) {
        f(c);
    }
}

/// Truncate to at most `max` bytes on a char boundary.
#[must_use]
pub fn clip(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &s[..end])
}
