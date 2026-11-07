//! State shared between the public [`AgentLoop`](crate::AgentLoop) handle and
//! the step executor.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};
use permission_engine::{PermissionResolver, Policy};
use protocol::event::TodoItem;
use protocol::{BotId, ChatId, Envelope, Event, InitiatorKind, RunId, ToolCallId, UserId};
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
}

/// How far an approval reaches (ADR-0012).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalScope {
    /// This call only.
    Once,
    /// This tool for the rest of the chat.
    ThisChat,
    /// This tool for this bot, persisted in the audit log.
    Always,
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

/// One event of the run stream.
#[derive(Debug, Clone)]
pub struct StreamItem {
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
}

pub(crate) struct Pending {
    pub tx: oneshot::Sender<Verdict>,
}

#[derive(Default)]
pub(crate) struct Grants {
    pub this_chat: HashSet<(ChatId, String)>,
    pub always: HashSet<(BotId, String)>,
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

    /// Append to the run's NDJSON log and broadcast.
    pub fn emit(&self, ctx: &RunCtx, event: Event) {
        let envelope = {
            let mut st = ctx.state.lock();
            let env = Envelope::new(st.seq, event);
            st.seq += 1;
            if st.writer.is_none() {
                st.writer = EventLogWriter::open(self.storage.data_dir(), ctx.run_id).ok();
            }
            if let Some(w) = st.writer.as_mut() {
                // A failing log must not kill the turn; the DB stays the source of truth.
                let _ = w.append(&env);
            }
            env
        };
        let _ = self.events.send(StreamItem {
            run_id: ctx.run_id,
            envelope,
        });
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

fn platform_ok(platforms: &[Platform]) -> bool {
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
