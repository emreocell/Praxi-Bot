//! State shared between the public [`AgentLoop`](crate::AgentLoop) handle and
//! the step executor.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use agent_router::{SurfaceMode, ToolSurface};
use parking_lot::{Mutex, RwLock};
use permission_engine::{Decision, PermissionResolver, Policy};
use protocol::event::TodoItem;
use protocol::{
    BotId, ChatId, Envelope, Event, InitiatorKind, PermissionCategory, RunId, ToolCallId, UserId,
};
use provider_catalog::{ChatProvider, ModelRef};
use serde::{Deserialize, Serialize};
use storage::{EventLogWriter, Run, Storage, read_event_log};
use task_runtime::ExecError;
use tokio::sync::{broadcast, oneshot};
use tokio::task::JoinHandle;
use tool_registry::{Platform, ToolRegistry, ToolSpec};
use uuid::Uuid;

use crate::config::LoopConfig;
use crate::profile::BotProfile;

/// Errors of the public loop API.
#[derive(Debug, thiserror::Error)]
pub enum LoopError {
    /// Storage failure.
    #[error(transparent)]
    Storage(#[from] storage::StorageError),
    /// Runtime failure.
    #[error(transparent)]
    Runtime(#[from] task_runtime::RuntimeError),
    /// A referenced chat / bot / run does not exist.
    #[error("not found: {0}")]
    NotFound(String),
    /// The bot has no usable profile.
    #[error("bot profile: {0}")]
    Profile(String),
    /// The chat already has a running turn; use `send_user_message`.
    #[error("chat {0} already has an active turn")]
    Busy(ChatId),
    /// No approval is waiting under that call id (already decided, or lost in a restart).
    #[error("no pending approval for call {0}")]
    NoPendingApproval(ToolCallId),
    /// [`AgentLoop::shutdown`](crate::AgentLoop::shutdown) was called: no new
    /// turns are accepted.
    #[error("the agent loop is shutting down")]
    ShuttingDown,
}

/// How far an approval reaches (ADR-0012).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalScope {
    /// This call only.
    Once,
    /// This tool for the rest of the chat.
    ThisChat,
    /// This tool for this bot: a persisted grant rule (see
    /// [`AgentLoop::list_grants`](crate::AgentLoop::list_grants)).
    Always,
    /// The tool's whole permission category for this bot: a persisted grant
    /// rule.  Absolute floors (money, never-list) still win.
    AlwaysCategory,
}

/// What a persisted "always allow" grant covers.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum GrantTarget {
    /// One tool, by name.
    Tool(String),
    /// Every tool of a permission category.
    Category(PermissionCategory),
}

/// A persisted "always allow" rule of a bot (ADR-0012), created by approving
/// with [`ApprovalScope::Always`] / [`ApprovalScope::AlwaysCategory`] and
/// removable with [`AgentLoop::revoke_grant`](crate::AgentLoop::revoke_grant).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    /// Bot the rule belongs to.
    pub bot_id: BotId,
    /// What it allows.
    pub target: GrantTarget,
    /// When it was granted (ms since the Unix epoch).
    pub granted_at_ms: i64,
}

/// What to do with a user message arriving while a turn is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageMode {
    /// Inject before the next model call of the running turn.
    Steer,
    /// Run as a new turn after the current one finishes.
    Queue,
}

/// Outcome of [`AgentLoop::send_user_message`](crate::AgentLoop::send_user_message).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// No turn was running; a new turn started.
    NewTurn(RunId),
    /// Injected into the running turn.
    Steered(RunId),
    /// Queued behind the running turn.
    Queued,
}

/// The permission picture of one bot: its stored policy merged with its
/// persisted grants (what actually happens for a human-initiated call).
#[derive(Debug, Clone)]
pub struct EffectivePolicy {
    /// The bot profile's own policy; `None` = the workspace policy applies.
    pub policy: Option<Policy>,
    /// Effective decision per permission category (`Meta` is always allowed and
    /// omitted).
    pub categories: BTreeMap<PermissionCategory, CategoryDecision>,
    /// The persisted "always allow" rules (tool-level ones are *not* reflected
    /// in `categories`: they only widen one tool).
    pub grants: Vec<Grant>,
}

/// Effective decision for one category.
#[derive(Debug, Clone, PartialEq)]
pub struct CategoryDecision {
    /// What a call of this category resolves to (initiator: human).
    pub decision: Decision,
    /// `true` when `decision` is `Allow` only because of a category grant (the
    /// policy alone would ask).
    pub via_grant: bool,
}

/// One event of the run stream.
#[derive(Debug, Clone)]
pub struct StreamItem {
    /// Daemon-wide sequence number (starts at 1, strictly increasing in
    /// broadcast order, persisted in the storage event index).
    pub gseq: u64,
    /// Run the event belongs to.
    pub run_id: RunId,
    /// The sequenced envelope (also appended to the run's NDJSON log).
    pub envelope: Envelope,
}

/// Chat providers by id, with a default.
#[derive(Clone)]
pub struct Providers {
    default: Arc<dyn ChatProvider>,
    by_id: HashMap<String, Arc<dyn ChatProvider>>,
}

impl Providers {
    /// A set whose only (and default) provider is `p`.
    #[must_use]
    pub fn single(p: Arc<dyn ChatProvider>) -> Self {
        let mut by_id = HashMap::new();
        by_id.insert(p.id().to_owned(), Arc::clone(&p));
        Self { default: p, by_id }
    }

    /// Add a provider, addressable by its `id()` (the `provider/` prefix of a model ref).
    #[must_use]
    pub fn with(mut self, p: Arc<dyn ChatProvider>) -> Self {
        self.by_id.insert(p.id().to_owned(), p);
        self
    }

    pub(crate) fn pick(&self, model: &ModelRef) -> Arc<dyn ChatProvider> {
        model
            .provider()
            .and_then(|p| self.by_id.get(p))
            .map_or_else(|| Arc::clone(&self.default), Arc::clone)
    }
}

pub(crate) enum Verdict {
    Approved { scope: ApprovalScope, by: UserId },
    Denied { by: UserId, reason: Option<String> },
    Answered { answer: String },
}

pub(crate) struct Pending {
    pub tx: oneshot::Sender<Verdict>,
}

#[derive(Default)]
pub(crate) struct Grants {
    pub this_chat: HashSet<(ChatId, String)>,
    /// Persisted rules, in creation order.
    pub always: Vec<Grant>,
}

impl Grants {
    pub fn covers(&self, bot: BotId, tool: &str, category: PermissionCategory) -> bool {
        self.always.iter().any(|g| {
            g.bot_id == bot
                && match &g.target {
                    GrantTarget::Tool(t) => t == tool,
                    GrantTarget::Category(c) => *c == category,
                }
        })
    }
}

pub(crate) struct CtxState {
    pub steer: VecDeque<String>,
    pub closed: bool,
    pub seq: u64,
    pub writer: Option<EventLogWriter>,
    pub todos: Vec<TodoItem>,
}

pub(crate) struct RunCtx {
    pub run_id: RunId,
    pub chat_id: ChatId,
    pub bot_id: BotId,
    pub user_id: UserId,
    pub initiator: InitiatorKind,
    pub profile: Arc<BotProfile>,
    pub bot_policy: Option<Policy>,
    pub state: Mutex<CtxState>,
}

pub(crate) struct Shared {
    pub storage: Arc<Storage>,
    pub tools: ToolRegistry,
    pub providers: Providers,
    pub resolver: PermissionResolver,
    pub cfg: LoopConfig,
    pub profiles: RwLock<HashMap<BotId, Arc<BotProfile>>>,
    pub events: broadcast::Sender<StreamItem>,
    pub runs: Mutex<HashMap<RunId, Arc<RunCtx>>>,
    pub pending: Mutex<HashMap<ToolCallId, Pending>>,
    pub grants: Mutex<Grants>,
    pub active: Mutex<HashMap<ChatId, RunId>>,
    pub queued: Mutex<HashMap<ChatId, VecDeque<String>>>,
    pub tasks: Mutex<HashMap<RunId, JoinHandle<()>>>,
    /// Background work (memory extraction).
    pub bg: Mutex<Vec<JoinHandle<()>>>,
    pub surface: ToolSurface,
    /// Last `gseq` handed out (see [`StreamItem::gseq`]).
    pub gseq: AtomicU64,
    /// Serialises `emit`, so `gseq` order equals broadcast order.
    pub emit_gate: Mutex<()>,
    /// Set by `AgentLoop::shutdown`: no new turns / resumes.
    pub shutting_down: AtomicBool,
}

pub(crate) fn xe(e: impl std::fmt::Display) -> ExecError {
    ExecError(e.to_string())
}

pub(crate) const KIND_PREFIX: &str = "turn";

pub(crate) fn kind_for(i: InitiatorKind) -> String {
    match i {
        InitiatorKind::Human => KIND_PREFIX.to_owned(),
        InitiatorKind::Routine => format!("{KIND_PREFIX}:routine"),
        InitiatorKind::Channel => format!("{KIND_PREFIX}:channel"),
        InitiatorKind::Subagent => format!("{KIND_PREFIX}:subagent"),
    }
}

pub(crate) fn initiator_of(kind: &str) -> InitiatorKind {
    match kind.strip_prefix("turn:") {
        Some("routine") => InitiatorKind::Routine,
        Some("channel") => InitiatorKind::Channel,
        Some("subagent") => InitiatorKind::Subagent,
        _ => InitiatorKind::Human,
    }
}

impl Shared {
    /// Profile for a bot: explicit registration, else the bot row's JSON config.
    pub fn profile_for(&self, bot_id: BotId) -> Result<Arc<BotProfile>, String> {
        if let Some(p) = self.profiles.read().get(&bot_id) {
            return Ok(Arc::clone(p));
        }
        let bot = self
            .storage
            .get_bot(bot_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("bot {bot_id} not found"))?;
        let profile: BotProfile = serde_json::from_value(bot.config)
            .map_err(|e| format!("bot {bot_id} has no usable profile: {e}"))?;
        let profile = Arc::new(profile);
        self.profiles.write().insert(bot_id, Arc::clone(&profile));
        Ok(profile)
    }

    /// Per-run context, built on first use (also after a restart).
    pub fn ctx(&self, run: &Run) -> Result<Arc<RunCtx>, ExecError> {
        if let Some(c) = self.runs.lock().get(&run.id) {
            return Ok(Arc::clone(c));
        }
        let chat_id = run.chat_id.ok_or_else(|| xe("turn run has no chat"))?;
        let profile = self.profile_for(run.bot_id).map_err(xe)?;
        let seq = read_event_log(self.storage.data_dir(), run.id)
            .map_or(0, |v| v.last().map_or(0, |e| e.seq + 1));
        let ctx = Arc::new(RunCtx {
            run_id: run.id,
            chat_id,
            bot_id: run.bot_id,
            user_id: run.user_id,
            initiator: initiator_of(&run.kind),
            bot_policy: profile.policy(),
            profile,
            state: Mutex::new(CtxState {
                steer: VecDeque::new(),
                closed: false,
                seq,
                writer: None,
                todos: Vec::new(),
            }),
        });
        Ok(Arc::clone(self.runs.lock().entry(run.id).or_insert(ctx)))
    }

    /// Whether `spec` may be shown to / used by the bot of run `run_id` (the
    /// discovery meta-tools ask this; an unknown run sees nothing).
    pub fn tool_visible_to_run(&self, run_id: &str, spec: &ToolSpec) -> bool {
        let Ok(id) = run_id.parse::<Uuid>() else {
            return false;
        };
        self.runs
            .lock()
            .get(&RunId(id))
            .is_some_and(|c| c.profile.tool_visible(&spec.name) && platform_ok(&spec.platforms))
    }

    /// Append to the run's NDJSON log, index it under the next `gseq` and
    /// broadcast.
    pub fn emit(&self, ctx: &RunCtx, event: Event) {
        let event = self.redact_event(event);
        // Lock order: gate, then the run's state.
        let _gate = self.emit_gate.lock();
        let (envelope, gseq) = {
            let mut st = ctx.state.lock();
            let env = Envelope::new(st.seq, event);
            st.seq += 1;
            let gseq = self.gseq.fetch_add(1, Ordering::SeqCst) + 1;
            // Index before the log line: a crash in between loses nothing a
            // replay could expect (a missing log line is skipped).
            let _ = self.storage.index_event(gseq, ctx.run_id, env.seq);
            if st.writer.is_none() {
                st.writer = EventLogWriter::open(self.storage.data_dir(), ctx.run_id).ok();
            }
            if let Some(w) = st.writer.as_mut() {
                // A failing log must not kill the turn; the DB stays the source of truth.
                let _ = w.append(&env);
            }
            (env, gseq)
        };
        let _ = self.events.send(StreamItem {
            gseq,
            run_id: ctx.run_id,
            envelope,
        });
    }

    /// Mask secrets in a string when a redactor is configured.
    pub fn redact(&self, text: String) -> String {
        match &self.cfg.redactor {
            Some(r) if r.contains_secret(&text) => r.redact(&text),
            _ => text,
        }
    }

    /// Mask secrets in every string of an event (no-op without a redactor or
    /// when nothing matches).
    fn redact_event(&self, event: Event) -> Event {
        let Some(r) = &self.cfg.redactor else {
            return event;
        };
        let Ok(mut v) = serde_json::to_value(&event) else {
            return event;
        };
        if !redact_value(r, &mut v) {
            return event;
        }
        serde_json::from_value(v).unwrap_or(event)
    }

    /// Tool specs shown to the model: the discovery meta-tools above the
    /// threshold, otherwise every tool the bot may see (sorted by name).
    pub fn model_specs(&self, profile: &BotProfile) -> Vec<ToolSpec> {
        match self.surface.mode() {
            SurfaceMode::Discovery => self.surface.specs(),
            SurfaceMode::Direct => self.visible_specs(profile),
        }
    }

    /// Resolve a model-requested call to the tool to run plus the spec and name
    /// permissions are evaluated against (for `use_tool`: the target tool).
    /// `None` = unknown / hidden tool.
    pub fn resolve_call(
        &self,
        profile: &BotProfile,
        name: &str,
        input: &serde_json::Value,
    ) -> Option<Resolved> {
        let tool = self.surface.resolve(name)?;
        let own = tool.spec().clone();
        let mut eff_input = input.clone();
        let (spec, eff_name) = if name == agent_router::USE_TOOL {
            let target = input.get("name").and_then(serde_json::Value::as_str);
            match (target, self.surface.effective_spec(name, input)) {
                (Some(t), Some(s)) => {
                    if !profile.tool_visible(t) || !platform_ok(&s.platforms) {
                        return None;
                    }
                    eff_input = input
                        .get("input")
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!({}));
                    (Some(s), t.to_owned())
                }
                // Malformed target: the meta-tool itself reports the error.
                _ => (None, name.to_owned()),
            }
        } else if self.surface.mode() == SurfaceMode::Discovery {
            // find_tool / get_tool_details: always available to the model.
            (Some(own), name.to_owned())
        } else {
            if !profile.tool_visible(name) {
                return None;
            }
            (Some(own), name.to_owned())
        };
        Some(Resolved {
            tool,
            spec,
            input: eff_input,
            name: eff_name,
        })
    }

    /// Tool specs the bot may see, sorted by name (stable prompts cache better).
    pub fn visible_specs(&self, profile: &BotProfile) -> Vec<ToolSpec> {
        let mut specs: Vec<ToolSpec> = self
            .tools
            .list()
            .into_iter()
            .filter(|s| profile.tool_visible(&s.name) && platform_ok(&s.platforms))
            .collect();
        specs.sort_by(|a, b| a.name.cmp(&b.name));
        specs
    }

    pub fn audit(&self, ctx: &RunCtx, actor: &str, action: &str, detail: &serde_json::Value) {
        let _ = self
            .storage
            .append_audit(ctx.user_id, Some(ctx.run_id), actor, action, detail);
    }
}

/// Redact all strings of `v` in place; `true` if anything changed.
fn redact_value(r: &secret_store::Redactor, v: &mut serde_json::Value) -> bool {
    match v {
        serde_json::Value::String(s) => {
            if r.contains_secret(s) {
                *s = r.redact(s);
                true
            } else {
                false
            }
        }
        serde_json::Value::Array(a) => a.iter_mut().fold(false, |c, x| redact_value(r, x) | c),
        serde_json::Value::Object(o) => o.values_mut().fold(false, |c, x| redact_value(r, x) | c),
        _ => false,
    }
}

/// Result of [`Shared::resolve_call`].
pub(crate) struct Resolved {
    pub tool: Arc<dyn tool_registry::Tool>,
    /// Spec to evaluate permissions against; `None` for a malformed `use_tool`
    /// (it executes nothing and just reports the error).
    pub spec: Option<ToolSpec>,
    /// Input permissions are evaluated against (`use_tool`: the target's input).
    pub input: serde_json::Value,
    /// Name permissions / grants are keyed by.
    pub name: String,
}

pub(crate) fn platform_ok(platforms: &[Platform]) -> bool {
    let here = if cfg!(windows) {
        Platform::Windows
    } else if cfg!(target_os = "macos") {
        Platform::MacOs
    } else {
        Platform::Linux
    };
    platforms.is_empty() || platforms.iter().any(|p| *p == Platform::All || *p == here)
}

/// Persisted model-step output.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct ModelOut {
    pub text: String,
    pub tool_calls: Vec<provider_catalog::ToolCall>,
    pub usage: Option<provider_catalog::Usage>,
    pub finish: String,
    /// Model that actually answered (differs from the planned one after a fallback).
    #[serde(default)]
    pub model: String,
    /// This was the tool-less closing call after a guard stopped the turn.
    #[serde(default)]
    pub stopped_by_guard: bool,
    /// Which guard stopped the turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard_reason: Option<String>,
}

/// One call inside a persisted tool step's input.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CallRec {
    pub id: String,
    pub name: String,
    pub arguments: String,
    pub repeat: u32,
}

/// One result inside a persisted tool step's output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ResultRec {
    pub id: String,
    pub call_uuid: Uuid,
    pub name: String,
    pub is_error: bool,
    pub text: String,
    pub spill_path: Option<String>,
}

pub(crate) const STEP_USER: &str = "user_message";
pub(crate) const STEP_MODEL: &str = "model";
pub(crate) const STEP_TOOLS: &str = "tools";
