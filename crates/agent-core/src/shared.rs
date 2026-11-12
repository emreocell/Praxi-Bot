//! State shared between the public [`AgentLoop`](crate::AgentLoop) handle and
//! the step executor.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, Weak};

use agent_router::ToolSurface;
use parking_lot::{Mutex, RwLock};
use permission_engine::{Decision, PermissionResolver, Policy};
use protocol::event::TodoItem;
use protocol::{
    BotId, ChatId, Envelope, Event, InitiatorKind, PermissionCategory, RunId, TaskId, ToolCallId,
    UserId,
};
use provider_catalog::{ChatProvider, ModelRef};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use storage::{EventJournal, JournalRecord, Run, Storage, read_event_log};
use task_runtime::{ExecError, Runtime};
use tokio::sync::{broadcast, oneshot};
use tokio::task::JoinHandle;
use tool_registry::{Platform, ToolRegistry, ToolSpec};
use uuid::Uuid;

use crate::config::{LoopConfig, ModelPrice, SharedRedactor};
use crate::profile::BotProfile;
use crate::subagent::SubSpec;

/// Errors of the public loop API.
#[derive(Debug, thiserror::Error)]
pub enum LoopError {
    /// Configure a provider before starting inference.
    #[error("setup required; run `bot setup` or `bot providers add`")]
    SetupRequired,
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
    /// A previous drive task for this run has not released its handle yet.
    /// Resume would detach that task or drop the new one; retry once it finishes.
    #[error("run {0} is still finishing; retry resume")]
    StillFinishing(RunId),
    /// This approval can only be given once (no "always" / "this chat").
    #[error("approval for call {0} can only be given once, not as a standing grant")]
    OnceOnly(ToolCallId),
    /// No approval is waiting under that call id (already decided, or lost in a restart).
    #[error("no pending approval for call {0}")]
    NoPendingApproval(ToolCallId),
    /// [`AgentLoop::shutdown`](crate::AgentLoop::shutdown) was called: no new
    /// turns are accepted.
    #[error("the agent loop is shutting down")]
    ShuttingDown,
    /// The decision ladder refused the request (router disabled, run not
    /// eligible for `remember`, ...).
    #[error("router: {0}")]
    Router(String),
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
    /// The message was the `/stop` command: the running turn was cancelled.
    Stopped(RunId),
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
    default: Option<Arc<dyn ChatProvider>>,
    by_id: HashMap<String, Arc<dyn ChatProvider>>,
    /// Catalog prices keyed by the full model reference (`provider/model`).
    prices: HashMap<String, ModelPrice>,
    /// Embedding backend of the K2 router (`None` = BM25 only).
    embedder: Option<Arc<dyn agent_router::ladder::Embedder>>,
    /// Masks secrets in every request sent through [`Self::pick`]ed providers.
    redactor: Option<SharedRedactor>,
}

impl Providers {
    /// No configured providers. Setup APIs remain available; inference is rejected.
    #[must_use]
    pub fn unconfigured() -> Self {
        Self {
            default: None,
            by_id: HashMap::new(),
            prices: HashMap::new(),
            embedder: None,
            redactor: None,
        }
    }

    /// Whether inference needs provider setup.
    #[must_use]
    pub fn setup_required(&self) -> bool {
        self.default.is_none()
    }

    /// A set whose only (and default) provider is `p`.
    #[must_use]
    pub fn single(p: Arc<dyn ChatProvider>) -> Self {
        let mut by_id = HashMap::new();
        by_id.insert(p.id().to_owned(), Arc::clone(&p));
        Self {
            default: Some(p),
            by_id,
            prices: HashMap::new(),
            embedder: None,
            redactor: None,
        }
    }

    /// Mask secrets in every request (system prompt, history, memory recall,
    /// tool results) before it reaches a provider.
    #[must_use]
    pub(crate) fn with_redactor(mut self, redactor: Option<SharedRedactor>) -> Self {
        self.redactor = redactor;
        self
    }

    /// Set the embedding backend of the K2 router (for example a
    /// `GeminiEmbedder` when a Gemini key is configured).  Part of the
    /// providers so a reload refreshes it together with them.
    #[must_use]
    pub fn with_embedder(mut self, embedder: Arc<dyn agent_router::ladder::Embedder>) -> Self {
        self.embedder = Some(embedder);
        self
    }

    /// The K2 embedding backend, if any.
    #[must_use]
    pub fn embedder(&self) -> Option<Arc<dyn agent_router::ladder::Embedder>> {
        self.embedder.clone()
    }

    /// Record the catalog prices of `provider_id`'s models (those with a known
    /// input and output price), keyed `provider_id/model`.  They price the
    /// usage records unless [`LoopConfig::prices`] overrides a model.
    #[must_use]
    pub fn with_catalog_prices(
        mut self,
        provider_id: &str,
        models: &[provider_catalog::ModelInfo],
    ) -> Self {
        for m in models {
            if let (Some(input), Some(output)) = (m.input_per_mtok, m.output_per_mtok) {
                self.prices.insert(
                    format!("{provider_id}/{}", m.id),
                    ModelPrice {
                        input_per_mtok: input,
                        output_per_mtok: output,
                        cached_per_mtok: m.cached_per_mtok,
                    },
                );
            }
        }
        self
    }

    /// Catalog price of a model (full `provider/model` reference).
    #[must_use]
    pub fn price_for(&self, model: &str) -> Option<ModelPrice> {
        self.prices.get(model).copied()
    }

    /// Add a provider, addressable by its `id()` (the `provider/` prefix of a model ref).
    #[must_use]
    pub fn with(mut self, p: Arc<dyn ChatProvider>) -> Self {
        if self.default.is_none() {
            self.default = Some(Arc::clone(&p));
        }
        self.by_id.insert(p.id().to_owned(), p);
        self
    }

    /// Id of the default provider (used for models without a `provider/` prefix),
    /// or an empty string when setup is required.
    #[must_use]
    pub fn default_id(&self) -> &str {
        self.default.as_ref().map_or("", |p| p.id())
    }

    /// Ids of all providers, sorted.
    #[must_use]
    pub fn ids(&self) -> Vec<String> {
        let mut v: Vec<String> = self.by_id.keys().cloned().collect();
        v.sort();
        v
    }

    /// A provider by id.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<Arc<dyn ChatProvider>> {
        self.by_id.get(id).cloned()
    }

    pub(crate) fn pick(
        &self,
        model: &ModelRef,
    ) -> Result<Arc<dyn ChatProvider>, provider_catalog::ProviderError> {
        let p = model
            .provider()
            .and_then(|p| self.by_id.get(p))
            .or(self.default.as_ref())
            .cloned()
            .ok_or_else(|| {
                provider_catalog::ProviderError::NoProvider(
                    "setup required; run `bot setup` or `bot providers add`".into(),
                )
            })?;
        Ok(match &self.redactor {
            Some(r) => Arc::new(RedactingProvider {
                inner: p,
                redactor: r.clone(),
            }),
            None => p,
        })
    }
}

/// Provider wrapper that masks secrets in the whole request (every message
/// part and tool-call argument) just before it leaves the process.
struct RedactingProvider {
    inner: Arc<dyn ChatProvider>,
    redactor: SharedRedactor,
}

#[async_trait::async_trait]
impl ChatProvider for RedactingProvider {
    fn id(&self) -> &str {
        self.inner.id()
    }

    async fn stream(
        &self,
        mut req: provider_catalog::ChatRequest,
    ) -> Result<
        futures::stream::BoxStream<
            'static,
            Result<provider_catalog::ChatChunk, provider_catalog::ProviderError>,
        >,
        provider_catalog::ProviderError,
    > {
        redact_request(&self.redactor, &mut req);
        self.inner.stream(req).await
    }

    async fn list_models(&self) -> Result<Vec<String>, provider_catalog::ProviderError> {
        self.inner.list_models().await
    }
}

/// Mask secrets in every message and tool-call argument of `req`.
pub(crate) fn redact_request(r: &SharedRedactor, req: &mut provider_catalog::ChatRequest) {
    for m in &mut req.messages {
        redact_value(r, &mut m.content);
        for c in &mut m.tool_calls {
            if r.contains_secret(&c.arguments) {
                c.arguments = r.redact(&c.arguments);
            }
        }
    }
}

pub(crate) enum Verdict {
    Approved { scope: ApprovalScope, by: UserId },
    Denied { by: UserId, reason: Option<String> },
    Answered { answer: String },
}

pub(crate) struct Pending {
    pub tx: oneshot::Sender<Verdict>,
    /// The card carries tool details or the tool is always-ask: only a
    /// one-time approval may be given for it, on every channel.
    pub once_only: bool,
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
    pub todos: Vec<TodoItem>,
}

pub(crate) struct RunCtx {
    pub computer: Arc<tokio::sync::OnceCell<tokio::sync::OwnedSemaphorePermit>>,
    pub waiting_computer: Arc<AtomicBool>,
    pub run_id: RunId,
    pub chat_id: ChatId,
    pub bot_id: BotId,
    pub user_id: UserId,
    pub initiator: InitiatorKind,
    /// The schedule that started the run (schedule-initiated runs only).
    pub schedule_id: Option<TaskId>,
    pub profile: Arc<BotProfile>,
    pub bot_policy: Option<Policy>,
    /// The run that spawned this one (subagent runs only).
    pub parent_run_id: Option<RunId>,
    /// Spawn parameters of a subagent run.
    pub sub: Option<SubSpec>,
    pub state: Mutex<CtxState>,
}

impl RunCtx {
    /// Subagent nesting level: 0 for a top-level run.
    pub fn depth(&self) -> u32 {
        self.sub.as_ref().map_or(0, |s| s.depth)
    }
}

/// Where the foreground browser domain / application come from, for skill
/// suggestions (see [`AgentLoop::set_skill_hints`](crate::AgentLoop::set_skill_hints)).
pub type SkillHintFn = Arc<dyn Fn() -> SkillHints + Send + Sync>;

/// What is in the foreground right now, when known.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkillHints {
    /// Host of the current browser page.
    pub domain: Option<String>,
    /// Foreground application name or executable.
    pub app: Option<String>,
}

pub(crate) struct Shared {
    pub storage: Arc<Storage>,
    pub tools: ToolRegistry,
    pub providers: RwLock<Providers>,
    /// Workspace-wide policy; replaceable at runtime (`PUT /v1/policy`).
    pub global_policy: RwLock<Policy>,
    /// Locked deny-all mode (unusable persisted policy): every decision except
    /// `Meta` is `Deny`, whatever a bot's own policy says.
    pub policy_locked: std::sync::atomic::AtomicBool,
    /// Desktop control bridge (see `control.rs`); `None` = no gate.
    pub control: RwLock<Option<Arc<dyn crate::control::ControlGate>>>,
    pub computer_slots: Arc<tokio::sync::Semaphore>,
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
    /// Ticket that keeps broadcast order equal to `gseq` order (see `emit`).
    pub send_gate: Mutex<()>,
    /// Set by `AgentLoop::shutdown`: no new turns / resumes.
    pub shutting_down: AtomicBool,
    /// The durable runtime (set once by `AgentLoop::new`; weak because the
    /// runtime owns the executor that owns this).
    pub rt: OnceLock<Weak<Runtime>>,
    /// Serialises the "count live children, then create one" step of
    /// `spawn_subagent`.
    pub spawn_gate: Mutex<()>,
    /// Skills index (`AgentLoop::with_skills`).
    pub skills: OnceLock<skills::SkillIndex>,
    /// Foreground browser / app source for skill suggestions.
    pub skill_hints: RwLock<Option<SkillHintFn>>,
    /// The K0-K3 decision ladders (one per bot) and the workflow wiring.
    pub router: crate::router::Router,
    /// What a schedule-started run does instead of asking the model, until its
    /// first step has been planned (afterwards it lives in the step's input).
    pub directives: Mutex<HashMap<RunId, ScheduleWork>>,
    /// Runs that paused because nobody answered an approval in time.
    pub approval_paused: Mutex<HashSet<RunId>>,
    /// How many approvals/questions each run has parked right now (parallel
    /// `ask_user` calls park several); the run is `WaitingApproval` while > 0.
    pub parked: Mutex<HashMap<RunId, usize>>,
    /// Serialises a global-policy install with the decision to execute an
    /// already-approved tool. Not held across `.await`.
    pub exec_gate: Mutex<()>,
    /// Test-only: the drive task waits here after `conclude` and before it
    /// reaps its own [`JoinHandle`].
    #[cfg(test)]
    pub task_reap_gate: Mutex<Option<Arc<tokio::sync::Barrier>>>,
    /// Test-only: set just before the reap-gate wait.
    #[cfg(test)]
    pub task_reap_entered: AtomicBool,
    /// Test-only: set after that reap attempt, so a test can observe it.
    #[cfg(test)]
    pub task_reap_done: AtomicBool,
    /// Test-only: invoked while `exec_gate` is held, immediately before the
    /// pre-tool deny recheck.
    #[cfg(test)]
    pub exec_commit_hook: Mutex<Option<ExecCommitHook>>,
}

/// Test-only hook invoked under `exec_gate` before the pre-tool deny recheck.
#[cfg(test)]
pub(crate) type ExecCommitHook = Arc<dyn Fn(&Shared) + Send + Sync>;

/// What a schedule-started run does (see
/// [`AgentLoop::run_scheduled`](crate::AgentLoop::run_scheduled)).
#[derive(Debug, Clone, PartialEq)]
pub enum ScheduleWork {
    /// A normal turn with this prompt.
    Prompt(String),
    /// Replay a saved workflow through the loop's permission gate.
    Workflow {
        /// Workflow name.
        name: String,
        /// Workflow parameters.
        params: BTreeMap<String, String>,
    },
    /// Call one registered tool through the loop's permission gate.
    Tool {
        /// Tool name.
        name: String,
        /// Tool input.
        input: Value,
    },
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
        InitiatorKind::Schedule => format!("{KIND_PREFIX}:schedule"),
    }
}

/// Run kind of a turn started by schedule `id` (the id survives a restart).
pub(crate) fn kind_for_schedule(id: TaskId) -> String {
    format!("{KIND_PREFIX}:schedule:{id}")
}

/// The schedule behind a run kind written by [`kind_for_schedule`].
pub fn schedule_id_of(kind: &str) -> Option<TaskId> {
    kind.strip_prefix("turn:schedule:")?
        .parse()
        .ok()
        .map(TaskId)
}

pub(crate) fn initiator_of(kind: &str) -> InitiatorKind {
    match kind.strip_prefix("turn:") {
        Some("routine") => InitiatorKind::Routine,
        Some("channel") => InitiatorKind::Channel,
        Some("subagent") => InitiatorKind::Subagent,
        Some(s) if s == "schedule" || s.starts_with("schedule:") => InitiatorKind::Schedule,
        _ => InitiatorKind::Human,
    }
}

impl Shared {
    /// USD price of `model` (full `provider/model` reference): the loop
    /// config's override, else the providers' catalog price.
    pub fn price_for(&self, model: &str) -> Option<ModelPrice> {
        self.cfg
            .prices
            .get(model)
            .copied()
            .or_else(|| self.providers.read().price_for(model))
    }

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
        let seq = read_event_log(self.storage.data_dir(), run.id)
            .map_or(0, |v| v.last().map_or(0, |e| e.seq + 1));
        let (profile, bot_policy, initiator, sub) = match run.parent_run_id {
            None => {
                let profile = self.profile_for(run.bot_id).map_err(xe)?;
                (
                    Arc::clone(&profile),
                    profile.policy(),
                    initiator_of(&run.kind),
                    None,
                )
            }
            Some(parent_id) => {
                // A subagent is the parent's bot: same policy, grants and
                // initiator; a narrowed tool surface and its own model.
                let parent = self
                    .storage
                    .get_run(parent_id)
                    .map_err(xe)?
                    .ok_or_else(|| xe(format!("parent run {parent_id} not found")))?;
                let pctx = self.ctx(&parent)?;
                let spec: SubSpec =
                    serde_json::from_value(run.spec.clone().unwrap_or(Value::Null)).map_err(xe)?;
                let model = spec.model_for(&pctx.profile).map_err(xe)?;
                let profile = Arc::new(
                    pctx.profile
                        .narrowed(spec.tools.as_deref(), Some(&model))
                        .map_err(xe)?,
                );
                (profile, pctx.bot_policy.clone(), pctx.initiator, Some(spec))
            }
        };
        let computer = run
            .parent_run_id
            .and_then(|parent| {
                self.runs
                    .lock()
                    .get(&parent)
                    .map(|ctx| Arc::clone(&ctx.computer))
            })
            .unwrap_or_else(|| Arc::new(tokio::sync::OnceCell::new()));
        let ctx = Arc::new(RunCtx {
            computer,
            waiting_computer: Arc::new(AtomicBool::new(false)),
            run_id: run.id,
            chat_id,
            bot_id: run.bot_id,
            user_id: run.user_id,
            initiator,
            schedule_id: if run.parent_run_id.is_none() {
                schedule_id_of(&run.kind)
            } else {
                None
            },
            bot_policy,
            profile,
            parent_run_id: run.parent_run_id,
            sub,
            state: Mutex::new(CtxState {
                steer: VecDeque::new(),
                closed: false,
                seq,
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
        let Some((profile, bot)) = self
            .runs
            .lock()
            .get(&RunId(id))
            .map(|c| (Arc::clone(&c.profile), c.bot_id))
        else {
            return false;
        };
        self.spec_visible(&profile, bot, spec)
    }

    /// Profile allow / exclude lists, platform and the tool's own per-bot
    /// visibility (an MCP server's `bots` list).  Call-time enforcement stays
    /// in the tool itself; this keeps hidden tools out of what the model sees.
    pub(crate) fn spec_visible(&self, profile: &BotProfile, bot: BotId, spec: &ToolSpec) -> bool {
        profile.tool_visible(&spec.name)
            && platform_ok(&spec.platforms)
            // Fail closed: a name the registry no longer knows is not visible.
            && self
                .tools
                .get(&spec.name)
                .is_some_and(|t| t.visible_to(Some(&bot.to_string())))
    }

    /// The loop's event journal (created over `storage` on first use).
    fn journal(&self) -> &Arc<EventJournal> {
        self.cfg
            .journal
            .get_or_init(|| EventJournal::fs(Arc::clone(&self.storage)))
    }

    /// Hand the event to the journal writer (NDJSON log + event index), take
    /// the next `gseq` and broadcast.  Nothing here waits for the disk, except
    /// that approval / run-state events are durable before `emit` returns (the
    /// wait happens after `emit_gate` is released).
    pub fn emit(&self, ctx: &RunCtx, event: Event) {
        if matches!(event, Event::RunStarted(_)) {
            self.refresh_redactor();
        }
        let event = self.redact_event(ctx.run_id, event);
        let barrier = needs_barrier(&event);
        let journal = self.journal();
        // Lock order: emit_gate, then the run's state, then (while still holding
        // emit_gate) the send ticket.  `emit_gate` fixes `gseq` order = journal
        // queue order; the send ticket keeps broadcast order equal to it while
        // letting the fsync wait of a barrier event happen *outside* emit_gate.
        let gate = self.emit_gate.lock();
        let (envelope, gseq) = {
            let mut st = ctx.state.lock();
            let env = Envelope::new(st.seq, event).with_parent(ctx.parent_run_id);
            st.seq += 1;
            let gseq = self.gseq.fetch_add(1, Ordering::SeqCst) + 1;
            journal.append(JournalRecord {
                gseq,
                run_id: ctx.run_id,
                envelope: env.clone(),
            });
            (env, gseq)
        };
        let ticket = self.send_gate.lock();
        drop(gate);
        if barrier {
            // Approval / run-state events are durable before anyone can see
            // them (so a UI answer or follower never outruns the log).
            journal.flush();
        }
        let _ = self.events.send(StreamItem {
            gseq,
            run_id: ctx.run_id,
            envelope,
        });
        drop(ticket);
    }

    /// Re-read the secret sources into the redactor (new secrets are masked
    /// from the next run / message on, not only after the poll interval).
    pub fn refresh_redactor(&self) {
        if let Some(r) = &self.cfg.redactor {
            r.refresh_now();
        }
    }

    /// Journal write/sync failures so far.
    pub fn journal_error_count(&self) -> u64 {
        self.journal().error_count()
    }

    /// Make everything queued for the journal durable.
    pub fn flush_journal(&self) {
        self.journal().flush();
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
    fn redact_event(&self, run_id: RunId, event: Event) -> Event {
        let Some(r) = &self.cfg.redactor else {
            return event;
        };
        let value = serde_json::to_value(&event);
        redact_event_checked(r, run_id, event, value)
    }

    /// Tool specs shown to the model: the discovery meta-tools above the
    /// threshold, otherwise every tool the bot may see (sorted by name).
    ///
    /// `hints` (a router `ToolHint`) narrows the turn to those tools plus the
    /// discovery meta-tools, so the model can still reach everything else.
    pub fn model_specs(&self, profile: &BotProfile, bot: BotId, hints: &[String]) -> Vec<ToolSpec> {
        if !hints.is_empty() {
            let narrowed = self
                .surface
                .hinted_specs(hints, &|s| self.spec_visible(profile, bot, s));
            // Only the meta-tools left: every hint was unknown or hidden.
            if narrowed.len() > 3 {
                return narrowed;
            }
        }
        // Only profile-visible, non-namespaced tools count against the
        // threshold; `mcp:` tools are reached via `use_tool` and never push
        // native tools behind it.
        self.surface
            .specs_for(&|s| self.spec_visible(profile, bot, s))
    }

    /// Resolve a model-requested call to the tool to run plus the spec and name
    /// permissions are evaluated against (for `use_tool`: the target tool).
    /// `None` = unknown / hidden tool.
    pub fn resolve_call(
        &self,
        profile: &BotProfile,
        bot: BotId,
        name: &str,
        input: &serde_json::Value,
        hints: &[String],
    ) -> Option<Resolved> {
        let tool = if hints.is_empty() {
            self.surface
                .resolve_with(name, &|s| self.spec_visible(profile, bot, s))?
        } else {
            self.surface
                .resolve_hinted(name, hints, &|s| self.spec_visible(profile, bot, s))?
        };
        let own = tool.spec().clone();
        let mut eff_input = input.clone();
        let (spec, eff_name) = if name == agent_router::USE_TOOL {
            let target = input.get("name").and_then(serde_json::Value::as_str);
            match (target, self.surface.effective_spec(name, input)) {
                (Some(t), Some(s)) => {
                    if !profile.tool_visible(t)
                        || !platform_ok(&s.platforms)
                        || !self.spec_visible(profile, bot, &s)
                    {
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
        } else if matches!(
            name,
            agent_router::FIND_TOOL | agent_router::GET_TOOL_DETAILS
        ) {
            // find_tool / get_tool_details: the surface only resolves them when
            // it exposed them, and then they are always available.
            (Some(own), name.to_owned())
        } else {
            // Same gate as the spec list: a hidden or wrong-platform tool must
            // not run just because the model named it.
            if !profile.tool_visible(name) || !platform_ok(&own.platforms) {
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
    pub fn visible_specs(&self, profile: &BotProfile, bot: BotId) -> Vec<ToolSpec> {
        let mut specs: Vec<ToolSpec> = self
            .tools
            .list()
            .into_iter()
            .filter(|s| self.spec_visible(profile, bot, s))
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

/// Redact `event` through its JSON `value`; fails CLOSED: when the event cannot
/// be transformed or rebuilt, a placeholder error event replaces it (the
/// original, possibly secret-bearing event is never returned).
pub(crate) fn redact_event_checked(
    r: &SharedRedactor,
    run_id: RunId,
    event: Event,
    value: serde_json::Result<serde_json::Value>,
) -> Event {
    let withheld = || {
        Event::Error(protocol::event::RunError {
            run_id,
            message: "[event withheld: redaction failed]".into(),
            recoverable: true,
        })
    };
    let Ok(mut v) = value else {
        return withheld();
    };
    if !redact_value(r, &mut v) {
        return event;
    }
    serde_json::from_value(v).unwrap_or_else(|_| withheld())
}

/// Redact all strings of `v` in place; `true` if anything changed.
pub(crate) fn redact_value(r: &SharedRedactor, v: &mut serde_json::Value) -> bool {
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

/// Events that must be on disk before `emit` returns: approvals, questions,
/// control hand-over and run state.  Everything else is journaled in batches.
fn needs_barrier(event: &Event) -> bool {
    matches!(
        event,
        Event::RunStarted(_)
            | Event::RunFinished(_)
            | Event::ApprovalNeeded(_)
            | Event::ToolCallApproved(_)
            | Event::ToolCallDenied(_)
            | Event::QuestionAsked(_)
            | Event::ControlRequested(_)
            | Event::SubagentStarted(_)
            | Event::SubagentFinished(_)
            | Event::Error(_)
    )
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
/// A saved workflow replayed by the router (K0-K3), instead of a model call.
pub(crate) const STEP_WORKFLOW: &str = "workflow";
/// A built-in command run by the router (`/new`).
pub(crate) const STEP_COMMAND: &str = "command";
