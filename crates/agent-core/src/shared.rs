//! State shared between the public [`AgentLoop`](crate::AgentLoop) handle and
//! the step executor.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, Weak};

use agent_router::ToolSurface;
use agent_router::ladder::{GeminiEmbedder, gemini};
use computer_target::{
    ComputerLabel, ComputerProvider, ComputerTarget, Resolution, WAIT_FOR_PC_REASON,
    resolve as resolve_computer,
};
use desktop_api::DesktopBackend;
use parking_lot::{Mutex, RwLock};
use permission_engine::{Decision, PermissionResolver, Policy};
use protocol::event::TodoItem;
use protocol::{
    BotId, ChatId, Envelope, Event, InitiatorKind, PermissionCategory, RunId, TaskId, ToolCallId,
    UserId,
};
use provider_catalog::config::CliSettings;
use provider_catalog::{
    Auth, Catalog, ChatProvider, ModelInfo, ModelRef, OpenAiCompatConfig, ProviderConfig,
    ProviderEntry, ProviderKind, ProviderSettings, config_path,
};
use secret_store::SecretStore;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use storage::{EventJournal, JournalRecord, Run, Storage, read_event_log};
use task_runtime::{ExecError, RunState, Runtime};
use tokio::sync::{broadcast, oneshot};
use tokio::task::JoinHandle;
use tool_registry::{Platform, ToolRegistry, ToolSpec};
use uuid::Uuid;

use crate::config::{LoopConfig, ModelPrice, SharedRedactor};
use crate::profile::BotProfile;
use crate::subagent::SubSpec;
use crate::taint::{self, ContentTaint};

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
    /// The message the client named is no longer the chat's last user message.
    #[error("that message is not the last message of the chat")]
    NotLastMessage,
    /// The last turn cannot be edited or regenerated (foreign source, still
    /// running, or not a plain user/assistant exchange).
    #[error("this turn cannot be edited or regenerated: {0}")]
    NotResendable(&'static str),
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
    /// Typing, keys, value setting and dragging into the one program the card
    /// names, for the rest of this run only (card 243-1). Never persisted, not
    /// inherited by sub-agents. Accepted only on a card for which the daemon
    /// resolved an eligible control target.
    RunControl,
    /// This one tool (same MCP server and tool) for the rest of this run only:
    /// never persisted, not inherited by sub-agents, never another tool. It
    /// covers the egress gate and a plain policy ask, never an always-ask
    /// tool, a floor, a program start after untrusted content, or an external
    /// caller. It stops covering as soon as the run has read something it had
    /// not read when the grant was given (private data after a web page): the
    /// next call asks again. Accepted only on a card that offered it.
    RunTool,
}

/// What a persisted "always allow" grant covers.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum GrantTarget {
    /// One tool, by name.
    Tool(String),
    /// Every tool of a permission category.
    Category(PermissionCategory),
    /// One tool on one target (a site, a folder, a command prefix): approval
    /// memory that never covers another target.
    Scoped {
        /// The tool.
        tool: String,
        /// The one target the rule covers.
        scope: crate::scoped_grant::Scope,
    },
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
    /// Concrete computer scope for desktop/browser grants.
    pub computer: Option<String>,
    /// Legacy desktop/browser rule with no computer. It authorizes nothing and
    /// stays listed so the user can see that the old rule no longer applies.
    pub inert: bool,
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
    /// The bot's autonomy mode (derived from its preset).
    pub mode: permission_engine::AutonomyMode,
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
    /// Computers where a category grant allows this category. Empty when the
    /// decision does not depend on a computer. A non-empty list does not allow
    /// the category on any other computer.
    pub scopes: Vec<String>,
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
    /// Explicit default provider id (may refer to an HTTP provider or a `cli:*` engine).
    default_id: Option<String>,
    by_id: HashMap<String, Arc<dyn ChatProvider>>,
    /// Enabled runnable `cli:*` provider ids (executed via `cli-provider`, not `ChatProvider`).
    cli_ids: Vec<String>,
    /// Catalog prices keyed by the full model reference (`provider/model`).
    prices: HashMap<String, ModelPrice>,
    /// Embedding backend of the K2 router (`None` = BM25 only).
    embedder: Option<Arc<dyn agent_router::ladder::Embedder>>,
    decider: Option<Arc<crate::decider::TurnDecider>>,
    /// Masks secrets in every request sent through [`Self::pick`]ed providers.
    redactor: Option<SharedRedactor>,
}

impl Providers {
    /// No configured providers. Setup APIs remain available; inference is rejected.
    #[must_use]
    pub fn unconfigured() -> Self {
        Self {
            default: None,
            default_id: None,
            by_id: HashMap::new(),
            cli_ids: Vec::new(),
            prices: HashMap::new(),
            embedder: None,
            decider: None,
            redactor: None,
        }
    }

    /// Whether inference needs provider setup.
    #[must_use]
    pub fn setup_required(&self) -> bool {
        self.default.is_none() && self.cli_ids.is_empty()
    }

    /// A set whose only (and default) provider is `p`.
    #[must_use]
    pub fn single(p: Arc<dyn ChatProvider>) -> Self {
        let mut by_id = HashMap::new();
        let id = p.id().to_owned();
        by_id.insert(id.clone(), Arc::clone(&p));
        Self {
            default: Some(p),
            default_id: Some(id),
            by_id,
            cli_ids: Vec::new(),
            prices: HashMap::new(),
            embedder: None,
            decider: None,
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

    /// The advisory pool configured on this provider snapshot, if usable.
    #[must_use]
    pub fn decider(&self) -> Option<Arc<crate::decider::TurnDecider>> {
        self.decider.clone()
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
            if self.default_id.is_none() {
                self.default_id = Some(p.id().to_owned());
            }
            self.default = Some(Arc::clone(&p));
        }
        self.by_id.insert(p.id().to_owned(), p);
        self
    }

    /// Register a runnable `cli:*` engine id (`"cli:claude"`).
    #[must_use]
    pub fn with_cli(mut self, id: impl Into<String>) -> Self {
        let id = id.into();
        if !self.cli_ids.contains(&id) {
            self.cli_ids.push(id.clone());
        }
        if self.default_id.is_none() {
            self.default_id = Some(id);
        }
        self
    }

    /// Override the active default provider id (and HTTP default if `id` is HTTP).
    #[must_use]
    pub fn with_default_id(mut self, id: impl Into<String>) -> Self {
        let id = id.into();
        if let Some(p) = self.by_id.get(&id) {
            self.default = Some(Arc::clone(p));
        }
        self.default_id = Some(id);
        self
    }

    /// Id of the default provider (used for models without a `provider/` prefix),
    /// or an empty string when setup is required.
    #[must_use]
    pub fn default_id(&self) -> &str {
        self.default_id.as_deref().unwrap_or("")
    }

    /// Ids of all providers (HTTP + runnable CLI), sorted and deduplicated.
    #[must_use]
    pub fn ids(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .by_id
            .keys()
            .cloned()
            .chain(self.cli_ids.iter().cloned())
            .collect();
        v.sort();
        v.dedup();
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
        if model.0.starts_with("cli:") {
            return Err(provider_catalog::ProviderError::NoProvider(format!(
                "provider {:?} is CLI-only and cannot serve HTTP requests",
                model.0
            )));
        }
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

/// Build runtime [`Providers`] from `providers.toml`: every enabled HTTP
/// provider that can be constructed plus every enabled runnable `cli:*` engine,
/// keeping the effective default active.
///
/// # Errors
/// Returns an error if `providers.toml` cannot be read or parsed.
pub fn build_providers(
    data_dir: &Path,
    catalog: &Catalog,
    store: &dyn SecretStore,
    warn: &mut dyn FnMut(&str),
) -> Result<Providers, String> {
    let cfg = ProviderConfig::load(&config_path(data_dir)).map_err(|e| e.to_string())?;
    let default_id = cfg.effective_default().map(|d| d.id.clone());
    let mut built: Vec<(String, Arc<dyn ChatProvider>, Vec<ModelInfo>)> = Vec::new();
    let mut cli_built: Vec<String> = Vec::new();
    for s in cfg.providers.iter().filter(|p| p.enabled) {
        if let Some(CliSettings::Cli { engine, .. }) = &s.cli {
            if cli_provider::engine_spec(engine.as_str())
                .is_some_and(cli_provider::CliEngineSpec::can_enable)
            {
                cli_built.push(s.id.clone());
            } else {
                warn(&format!(
                    "provider {} skipped: CLI engine {} is not runnable",
                    s.id,
                    engine.as_str()
                ));
            }
            continue;
        }
        match build_one(s, catalog, store) {
            Ok((p, models)) => built.push((s.id.clone(), p, models)),
            Err(e) => warn(&format!("provider {} skipped: {e}", s.id)),
        }
    }
    if built.is_empty() && cli_built.is_empty() {
        warn("setup required; run `bot setup` or `bot providers add`");
    }
    let mut providers = if built.is_empty() {
        Providers::unconfigured()
    } else {
        let pos = built
            .iter()
            .position(|(id, _, _)| Some(id) == default_id.as_ref())
            .unwrap_or(0);
        // The catalog's prices go with the providers, so a reload refreshes them
        // too and `/v1/usage` can report `cost_usd`.
        let (id, first, models) = built.remove(pos);
        let mut p = Providers::single(first).with_catalog_prices(&id, &models);
        for (id, next, models) in built {
            p = p.with(next).with_catalog_prices(&id, &models);
        }
        p
    };
    for cli_id in cli_built {
        providers = providers.with_cli(cli_id);
    }
    if let Some(def) = default_id
        && providers.ids().iter().any(|id| id == &def)
    {
        providers = providers.with_default_id(def);
    }
    // K2 embeddings (decision ladder): Gemini when a Gemini key is configured.
    if !providers.setup_required()
        && let Some(key) = gemini_key(&cfg, catalog, store)
    {
        providers = providers.with_embedder(Arc::new(GeminiEmbedder::new(
            key,
            gemini::DEFAULT_EMBED_MODEL,
        )));
    }
    providers.decider = crate::decider::build(data_dir, &providers, &cfg, catalog, store, warn);
    Ok(providers)
}

/// The API key of the first enabled Gemini provider that has one (stored
/// secret or environment variable), for the router's embedder.
fn gemini_key(cfg: &ProviderConfig, catalog: &Catalog, store: &dyn SecretStore) -> Option<String> {
    cfg.providers.iter().filter(|p| p.enabled).find_map(|s| {
        let entry = s.custom.as_ref().or_else(|| catalog.get(&s.id))?;
        if entry.kind != ProviderKind::Gemini {
            return None;
        }
        provider_key(s, entry, store)
    })
}

pub(crate) fn provider_key(
    settings: &ProviderSettings,
    entry: &ProviderEntry,
    store: &dyn SecretStore,
) -> Option<String> {
    let key = match (&settings.key, &entry.auth) {
        (Some(r), _) => secret_store::resolve(store, r)
            .ok()
            .map(|k| k.expose().to_string()),
        (None, Auth::ApiKeyEnv(var)) => std::env::var(var).ok(),
        (None, Auth::None) => None,
    };
    key.filter(|k| !k.trim().is_empty())
}

fn build_one(
    s: &ProviderSettings,
    catalog: &Catalog,
    store: &dyn SecretStore,
) -> Result<(Arc<dyn ChatProvider>, Vec<ModelInfo>), String> {
    let entry: ProviderEntry = match &s.custom {
        Some(c) => c.clone(),
        None => catalog
            .get(&s.id)
            .cloned()
            .ok_or_else(|| format!("{:?} is not in the catalog", s.id))?,
    };
    let provider = Arc::from(
        s.build_provider(&entry, store, OpenAiCompatConfig::default())
            .map_err(|e| e.to_string())?,
    );
    Ok((provider, entry.models))
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
    let knowledge_budgets = crate::knowledge::request_budgets(req);
    for m in &mut req.messages {
        redact_value(r, &mut m.content);
        for c in &mut m.tool_calls {
            if r.contains_secret(&c.arguments) {
                c.arguments = r.redact(&c.arguments);
            }
        }
    }
    crate::knowledge::rebudget_request(req, &knowledge_budgets);
}

pub(crate) enum Verdict {
    Approved {
        scope: ApprovalScope,
        by: UserId,
    },
    Denied {
        by: UserId,
        reason: Option<String>,
    },
    Answered {
        answer: String,
    },
    /// The owner's submit route stored the value of a `secret_request` (never carries it).
    SecretStored,
}

/// What a parked `secret_request` asks for: names and flags only, never a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretRequestInfo {
    /// The run that asked.
    pub run_id: RunId,
    /// Entry name (after `fill/`).
    pub name: String,
    /// One value or username + password.
    pub fields: protocol::event::SecretFields,
    /// The bot asked to replace an existing entry; without it the submit route
    /// must refuse a name that exists.
    pub replace: bool,
}

// Independent offer flags of one card; folding them into an enum would hide which
// combinations the daemon allows.
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct Pending {
    pub tx: oneshot::Sender<Verdict>,
    /// The card carries tool details or the tool is always-ask: only a
    /// one-time approval may be given for it, on every channel.
    pub once_only: bool,
    /// The card offers [`ApprovalScope::RunControl`]: the daemon resolved an
    /// eligible control target for this call.
    pub grantable: bool,
    /// The card is a taint-gate card that offers "always for this chat": creating that grant
    /// takes the owner's authority (the API token alone is not enough).
    pub gate_grant: bool,
    /// The card offers [`ApprovalScope::RunTool`].
    pub run_tool: bool,
    /// A tool-wide standing grant ("always for this bot") is refused for this card: an MCP
    /// tool in a run that has read something private.
    pub always_refused: bool,
}

/// Browser actions that act on the page open right now: their allowance is bound to its origin.
pub(crate) fn is_page_action(name: &str) -> bool {
    matches!(
        name,
        "browser_fill" | "browser_press" | "browser_click" | "browser_select" | "browser_act"
    )
}

#[derive(Default)]
pub(crate) struct Grants {
    /// `(chat, bot, tool, computer)`: a grant given to one bot never covers another
    /// bot answering in the same chat (`@mention`).
    pub this_chat: HashSet<(ChatId, BotId, String, Option<String>)>,
    /// `(chat, bot, tool class, destination class)`: "always allow for this chat" given to a
    /// taint-gate card on a Full bot (see `Shared::gate_key`). Never covers a send, a payment,
    /// a never-list command or a secret fill: those are not grantable.
    pub gate_chat: HashSet<(ChatId, BotId, &'static str, String)>,
    /// `(chat, bot, tool, target, computer)`: "allow for this chat" on a tool that is
    /// remembered per target. Covers only that target (see `scoped_grant`).
    pub this_chat_scoped: HashSet<(
        ChatId,
        BotId,
        String,
        crate::scoped_grant::Scope,
        Option<String>,
    )>,
    /// Persisted rules, in creation order.
    pub always: Vec<Grant>,
}

impl Grants {
    pub fn covers(
        &self,
        bot: BotId,
        tool: &str,
        category: PermissionCategory,
        computer: Option<&str>,
    ) -> bool {
        self.always.iter().any(|g| {
            !g.inert
                && g.bot_id == bot
                && g.computer.as_deref() == computer
                && match &g.target {
                    GrantTarget::Tool(t) => t == tool,
                    GrantTarget::Category(c) => *c == category,
                    GrantTarget::Scoped { .. } => false,
                }
        })
    }

    /// A persisted rule for this tool whose target covers `call`.
    pub fn covers_scoped(
        &self,
        bot: BotId,
        tool: &str,
        call: &crate::scoped_grant::Scope,
        computer: Option<&str>,
    ) -> bool {
        self.always.iter().any(|g| {
            !g.inert
                && g.bot_id == bot
                && g.computer.as_deref() == computer
                && matches!(&g.target, GrantTarget::Scoped { tool: t, scope }
                    if t == tool && scope.covers(call))
        })
    }
}

pub(crate) struct CtxState {
    pub steer: VecDeque<String>,
    pub closed: bool,
    pub seq: u64,
    pub todos: Vec<TodoItem>,
    /// `secret_request` calls this run has made (asked or refused after the card tests).
    pub secret_requests: u32,
}

/// Per-run resolved computer placement fixed at run start.
#[derive(Clone)]
pub(crate) struct RunComputer {
    pub node: Option<computer_target::ComputerNode>,
    pub unavailable_reason: Option<String>,
    pub label: ComputerLabel,
    pub desktop: Option<Arc<dyn DesktopBackend>>,
    pub desktop_available: bool,
    pub browser: bool,
    pub browser_headless: Option<bool>,
    pub wait_for_pc: bool,
    /// User-visible note about this resolution, emitted once as a run event.
    pub notice: Option<String>,
    /// The paired server this run's bot uses as ITS computer (`server:<key>`):
    /// its browser tools act there over this link. `None`: the browser tools
    /// use this daemon's own browser.
    pub server_link: Option<Arc<dyn builtin_tools::pc::PcLink>>,
}

/// Why a bot with `computer = "server:<key>"` has no computer for this run.
const SERVER_COMPUTER_UNAVAILABLE: &str = "The server this bot uses as its computer is not paired with this computer (or the key is unknown), so it has no computer tools. Tell the user to pair the server again in the Praxi app or to change the bot's computer. Do not use another computer instead.";

struct FallbackComputerProvider {
    desktop: Option<Arc<dyn DesktopBackend>>,
    browser: bool,
}

impl ComputerProvider for FallbackComputerProvider {
    fn local(&self) -> Option<Arc<dyn DesktopBackend>> {
        self.desktop.clone()
    }

    fn pc(&self) -> Option<Arc<dyn DesktopBackend>> {
        None
    }

    fn is_host(&self) -> bool {
        false
    }

    fn browser_available(&self) -> bool {
        self.browser
    }

    fn local_identity_bound(&self) -> bool {
        false
    }
}

pub(crate) struct RunCtx {
    pub computer: Arc<tokio::sync::OnceCell<tokio::sync::OwnedSemaphorePermit>>,
    pub waiting_computer: Arc<AtomicBool>,
    pub resolved_computer: RunComputer,
    /// The user's own paired computer, the target of the `pc_*` tools (card 272).
    /// Not part of `resolved_computer`: a run is placed on ONE computer, the
    /// per-request target is a second identity with its own grant scope.
    pub mypc: Option<computer_target::ComputerNode>,
    /// `pc_*` read families (files, screen, browser) the owner approved in
    /// THIS run: a read of the user's computer asks once per run and family,
    /// never once for every run (review blocker 1).
    pub pc_reads: Mutex<std::collections::BTreeSet<&'static str>>,
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
    /// Untrusted and private content this run has already shown the model.
    /// Never cleared. Copied from the parent when a subagent starts, and
    /// rebuilt from stored steps when the process resumes a run.
    pub taint: Mutex<ContentTaint>,
    /// Run-scoped control grants (card 243-1). In memory, per run: gone when
    /// the run ends and not copied to a sub-agent's own context.
    pub control_grants: Mutex<Vec<ControlGrant>>,
    /// Run-scoped tool grants ("allow for this task", [`ApprovalScope::RunTool`]):
    /// the tool name and the run's exposure when the owner gave it. In memory,
    /// per run: gone when the run ends and not copied to a sub-agent.
    pub tool_grants: Mutex<Vec<RunToolGrant>>,
    /// Origin the browser page was on when the current page action was resolved (read from the
    /// browser itself; `None`: unknown, so the action stays once-only). Set by the loop right
    /// before the decision and handed to the tool as `expected_origin`.
    pub page_origin: Mutex<Option<String>>,
}

/// "Allow this tool for this task": one tool, this run, at most the exposure
/// the run had when the owner gave it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunToolGrant {
    /// Registry name (`mcp:<server>/<tool>`).
    pub tool: String,
    /// What the run had read when the grant was given.
    pub exposure: permission_engine::ContentExposure,
}

impl RunToolGrant {
    /// Same tool, and the run has read nothing of a kind it had not read when
    /// the grant was given.
    pub(crate) fn covers(&self, tool: &str, now: permission_engine::ContentExposure) -> bool {
        self.tool == tool
            && (!now.untrusted || self.exposure.untrusted)
            && (!now.private || self.exposure.private)
    }
}

/// "Let this run type into this program" on one computer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ControlGrant {
    /// `tool_grant_scope` of the computer it was given on.
    pub computer: String,
    /// The one program it covers.
    pub identity: tool_registry::ControlIdentity,
}

/// The control kind a built-in tool sends, for the three tools a run-scoped
/// grant can ever cover. `desktop_launch`, `shell_run`, `script_run` and every
/// other tool return `None`: nothing covers them.
pub(crate) fn control_kind_of(name: &str) -> Option<tool_registry::ControlKind> {
    use tool_registry::ControlKind as K;
    match name {
        "desktop_type" => Some(K::Type),
        "desktop_set_value" => Some(K::SetValue),
        "desktop_key" => Some(K::Key),
        _ => None,
    }
}

impl RunCtx {
    /// Own per-run state key for tools that keep any (the browser tab): a subagent
    /// run is its own lane, a top-level run shares the bot's default.
    pub(crate) fn lane(&self) -> Option<String> {
        self.parent_run_id.map(|_| self.run_id.to_string())
    }

    /// A run-scoped control grant covers this call: same computer, same
    /// running program (path, pid and start time), a covered kind, and a target
    /// the deny rules did not refuse.
    pub(crate) fn control_covers(
        &self,
        name: &str,
        category: PermissionCategory,
        target: &tool_registry::ControlTarget,
    ) -> bool {
        if control_kind_of(name) != Some(target.kind) || !target.eligible() {
            return false;
        }
        let Some(scope) = self.tool_grant_scope(name, category) else {
            return false;
        };
        self.control_grants.lock().iter().any(|g| {
            g.computer == scope
                && g.identity.pid == target.identity.pid
                && g.identity.start_time == target.identity.start_time
                && g.identity
                    .image_path
                    .eq_ignore_ascii_case(&target.identity.image_path)
        })
    }

    pub(crate) fn grant_scope(&self, category: PermissionCategory) -> Option<String> {
        self.tool_grant_scope("", category)
    }

    pub(crate) fn tool_grant_scope(
        &self,
        name: &str,
        category: PermissionCategory,
    ) -> Option<String> {
        // The `pc_*` family acts on the user's own computer, whatever the run is
        // placed on. Its scope is that computer's id only: a grant given for the
        // bot's own computer ("host:..", "browser:..", "pc:..", or none for shell
        // and files) never matches it, and a grant given here never matches
        // those. Nothing paired still has a scope of its own, so it cannot fall
        // back to an unscoped grant.
        if name.starts_with(builtin_tools::pc::PC_PREFIX) {
            return Some(format!(
                "mypc:{}",
                self.mypc.as_ref().map_or("none", |n| n.id.as_str())
            ));
        }
        let computer_tool = matches!(
            category,
            PermissionCategory::Desktop | PermissionCategory::Browser
        ) || name.starts_with("desktop_")
            || name.starts_with("browser_");
        if !computer_tool {
            return None;
        }
        // No node means no machine. Do not fall back to a shared `local` alias.
        let id = self.resolved_computer.node.as_ref()?.id.as_str();
        if id.is_empty() {
            return None;
        }
        // A local kind is only ever scoped by a real machine fingerprint.
        if matches!(
            self.resolved_computer.label,
            ComputerLabel::Host | ComputerLabel::Browser
        ) && !is_machine_fingerprint(id)
        {
            return None;
        }
        Some(format!("{}:{id}", self.resolved_computer.label.as_str()))
    }

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
    pub usage: crate::usage::Ledger,
    pub storage: Arc<Storage>,
    pub tools: ToolRegistry,
    /// Whether a bot that never chose a file access mode gets no file access
    /// (always-on host) instead of "This computer". Evaluated per call.
    pub fs_unset_denied: RwLock<Option<Arc<dyn Fn() -> bool + Send + Sync>>>,
    pub checkpoint_access: RwLock<(Vec<std::path::PathBuf>, Vec<std::path::PathBuf>)>,
    pub providers: RwLock<Providers>,
    /// Workspace-wide policy; replaceable at runtime (`PUT /v1/policy`).
    pub global_policy: RwLock<Policy>,
    /// Locked deny-all mode (unusable persisted policy): every decision except
    /// `Meta` is `Deny`, whatever a bot's own policy says.
    pub policy_locked: std::sync::atomic::AtomicBool,
    /// Monthly spending limit (default off); see `spend.rs`.
    pub spend: crate::spend::SpendState,
    /// Desktop control bridge (see `control.rs`); `None` = no gate.
    pub control: RwLock<Option<Arc<dyn crate::control::ControlGate>>>,
    /// Dynamic computer availability provider installed by the daemon (`serve`).
    pub computer_provider: RwLock<Option<Arc<dyn ComputerProvider>>>,
    /// Paired servers a bot may use as ITS computer while it runs here.
    pub server_links: RwLock<Option<Arc<dyn builtin_tools::pc::ServerLinks>>>,
    /// Static desktop backend passed to [`AgentLoop::with_builtin_tools`] (fallback when
    /// no [`ComputerProvider`] is installed).
    pub static_desktop: RwLock<Option<Arc<dyn DesktopBackend>>>,
    /// Pre-resolved per-run computer placement keyed by `run_id` until `ctx()` claims it.
    pub run_computers: Mutex<HashMap<RunId, RunComputer>>,
    /// Active timeout cancellation tokens for runs paused on `waiting_for_pc`.
    pub wait_pc_timers: Mutex<HashMap<RunId, tokio_util::sync::CancellationToken>>,
    pub computer_slots: Arc<tokio::sync::Semaphore>,
    pub resolver: PermissionResolver,
    pub cfg: LoopConfig,
    pub profiles: RwLock<HashMap<BotId, Arc<BotProfile>>>,
    pub events: broadcast::Sender<StreamItem>,
    pub runs: Mutex<HashMap<RunId, Arc<RunCtx>>>,
    pub pending: Mutex<HashMap<ToolCallId, Pending>>,
    /// `secret_request` calls waiting for the owner's form (see `approval.rs`).
    pub secret_requests: Mutex<HashMap<ToolCallId, SecretRequestInfo>>,
    pub grants: Mutex<Grants>,
    pub active: Mutex<HashMap<ChatId, RunId>>,
    pub queued: Mutex<HashMap<ChatId, VecDeque<(String, &'static str)>>>,
    /// Bots still to answer one user message that mentioned several bots
    /// (`@A @B`), with that message's text. Only the user's own text fills it.
    pub mention_chain: Mutex<HashMap<ChatId, VecDeque<(BotId, Uuid)>>>,
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
    /// Owner-stored fill secrets (from the builtin tool config); workflow
    /// replay types them through the same checks as the fill tools.
    pub fill_secrets: OnceLock<Arc<secret_store::FillSecrets>>,
    /// Foreground browser / app source for skill suggestions.
    pub skill_hints: RwLock<Option<SkillHintFn>>,
    /// The K0-K3 decision ladders (one per bot) and the workflow wiring.
    pub router: crate::router::Router,
    /// What a schedule-started run does instead of asking the model, until its
    /// first step has been planned (afterwards it lives in the step's input).
    pub directives: Mutex<HashMap<RunId, ScheduleWork>>,
    /// Runs that paused because nobody answered an approval in time.
    pub approval_paused: Mutex<HashSet<RunId>>,
    /// Runs left paused because this boot could not read the local identity.
    pub identity_paused: Mutex<HashSet<RunId>>,
    /// How many approvals/questions each run has parked right now (parallel
    /// `ask_user` calls park several); the run is `WaitingApproval` while > 0.
    pub parked: Mutex<HashMap<RunId, usize>>,
    /// Serialises a global-policy install with the decision to execute an
    /// already-approved tool. Not held across `.await`.
    pub exec_gate: Mutex<()>,
    /// Cancellation tokens of the native-tools engine processes running per bot,
    /// so the owner's kill switch can end them at once.
    pub native_runs: Mutex<HashMap<BotId, Vec<(uuid::Uuid, tokio_util::sync::CancellationToken)>>>,
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

fn auto_unreachable_notice(
    target: &ComputerTarget,
    provider: &dyn ComputerProvider,
    rc: &RunComputer,
) -> Option<String> {
    // A host-homed bot never "falls back" from the PC: its own computer is the default.
    if !matches!(target, ComputerTarget::Auto)
        || provider.is_host()
        || !provider.paired_pc_unreachable()
    {
        return None;
    }
    let using = match rc.label {
        ComputerLabel::Host => "host",
        ComputerLabel::Browser => "browser",
        ComputerLabel::Pc | ComputerLabel::Off | ComputerLabel::Server => return None,
    };
    Some(format!("auto: PC not reachable, using {using}"))
}

pub(crate) fn xe(e: impl std::fmt::Display) -> ExecError {
    ExecError(e.to_string())
}

/// Run start may wait this long on a cache-only prepare. Dialing belongs to
/// the background monitor, which has a longer budget.
pub(crate) const COMPUTER_PREPARE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Run event emitted when a stored host/browser run is resumed during a boot
/// whose local computer identity could not be read.
pub(crate) const COMPUTER_IDENTITY_UNBOUND_NOTICE: &str = "this computer's identity could not be verified at startup; the run stays paused and can be resumed after the next restart";

/// Run event for a NEW run started while the local identity is unavailable:
/// it runs without local computer tools and nothing machine-bound is stored.
pub(crate) const COMPUTER_IDENTITY_UNBOUND_FRESH_NOTICE: &str =
    "this computer's identity could not be read; computer tools are unavailable until restart";

/// Whether `id` is a real 64-hex machine fingerprint.
pub(crate) fn is_machine_fingerprint(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Run event emitted when [`COMPUTER_PREPARE_TIMEOUT`] fires.
pub(crate) const COMPUTER_PREPARE_TIMEOUT_NOTICE: &str =
    "computer prepare timed out; using the cached desktop list without dialing";

pub(crate) const KIND_PREFIX: &str = "turn";

pub(crate) fn kind_for(i: InitiatorKind) -> String {
    match i {
        InitiatorKind::Human => KIND_PREFIX.to_owned(),
        InitiatorKind::Routine => format!("{KIND_PREFIX}:routine"),
        InitiatorKind::Channel => format!("{KIND_PREFIX}:channel"),
        InitiatorKind::Subagent => format!("{KIND_PREFIX}:subagent"),
        InitiatorKind::Schedule => format!("{KIND_PREFIX}:schedule"),
        InitiatorKind::ExternalAgent => format!("{KIND_PREFIX}:external_agent"),
        InitiatorKind::ExternalSchedule => format!("{KIND_PREFIX}:external_schedule"),
    }
}

/// Run kind of a turn that answers the user message `msg` for a bot mentioned
/// in it. The id binds the run to that exact message (first or later bot).
pub(crate) fn kind_for_mention(msg: Uuid) -> String {
    format!("{KIND_PREFIX}:mention:{msg}")
}

/// The user message a mention run (see [`kind_for_mention`]) answers.
pub(crate) fn mention_message_id(kind: &str) -> Option<Uuid> {
    kind.strip_prefix("turn:mention:")?.parse().ok()
}

/// Run kind of a turn started by schedule `id` (the id survives a restart).
pub(crate) fn kind_for_schedule(id: TaskId) -> String {
    format!("{KIND_PREFIX}:schedule:{id}")
}

/// Durable idempotency key of one scheduled occurrence attempt, stored in the
/// run's `kind`: `turn:[external_]schedule:<id>:<due_ms>:<attempt>`. A repeated
/// start with the same key returns the existing run, also after a restart.
#[must_use]
pub fn occurrence_kind(external: bool, id: TaskId, due_ms: i64, attempt: u32) -> String {
    let prefix = if external {
        "external_schedule"
    } else {
        "schedule"
    };
    format!("{KIND_PREFIX}:{prefix}:{id}:{due_ms}:{attempt}")
}

/// Whether `kind` carries an occurrence idempotency key.
pub(crate) fn is_occurrence_kind(kind: &str) -> bool {
    kind.strip_prefix("turn:schedule:")
        .or_else(|| kind.strip_prefix("turn:external_schedule:"))
        .is_some_and(|rest| rest.split(':').count() == 3)
}

/// The schedule behind a run kind written by [`kind_for_schedule`] or [`occurrence_kind`].
pub fn schedule_id_of(kind: &str) -> Option<TaskId> {
    kind.strip_prefix("turn:schedule:")
        .or_else(|| kind.strip_prefix("turn:external_schedule:"))?
        .split(':')
        .next()?
        .parse()
        .ok()
        .map(TaskId)
}

/// Who started a run, from its `kind` string.
#[must_use]
pub fn initiator_of(kind: &str) -> InitiatorKind {
    match kind.strip_prefix("turn:") {
        Some("routine") => InitiatorKind::Routine,
        Some("channel") => InitiatorKind::Channel,
        Some("subagent") => InitiatorKind::Subagent,
        Some(s) if s == "external_schedule" || s.starts_with("external_schedule:") => {
            InitiatorKind::ExternalSchedule
        }
        Some(s) if s == "schedule" || s.starts_with("schedule:") => InitiatorKind::Schedule,
        Some(s) if s == "external_agent" || s.starts_with("external_agent:") => {
            InitiatorKind::ExternalAgent
        }
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

    /// The profile a top-level run uses: narrowed to the computer it resolved
    /// to, and told whether an unset file mode is denied here.
    fn run_profile(&self, raw: &BotProfile, rc: &RunComputer) -> BotProfile {
        let unset_denied = matches!(self.unset_file_scope(), builtin_tools::FsScope::Folders(_));
        raw.with_computer_access(rc.desktop_available, rc.browser)
            .with_unset_files_denied(unset_denied)
    }

    /// Scope of a bot that never chose a file access mode.
    pub fn unset_file_scope(&self) -> builtin_tools::FsScope {
        let denied = self.fs_unset_denied.read().as_ref().is_some_and(|f| f());
        if denied {
            builtin_tools::FsScope::Folders(Vec::new())
        } else {
            builtin_tools::FsScope::Computer
        }
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

    /// Warm the installed [`ComputerProvider`] before resolving a run.
    ///
    /// The short bound is only for this path. It must not dial: providers
    /// prepare from the cache the background monitor fills. Returns whether
    /// the bound fired.
    pub(crate) async fn prepare_computer_provider(&self, target: &ComputerTarget) -> bool {
        if !matches!(
            target,
            ComputerTarget::Auto | ComputerTarget::Pc | ComputerTarget::Node(_)
        ) {
            return false;
        }
        let provider = self.computer_provider.read().clone();
        let Some(provider) = provider else {
            return false;
        };
        if tokio::time::timeout(COMPUTER_PREPARE_TIMEOUT, provider.prepare())
            .await
            .is_err()
        {
            tracing::warn!(
                budget_secs = COMPUTER_PREPARE_TIMEOUT.as_secs(),
                "{COMPUTER_PREPARE_TIMEOUT_NOTICE}"
            );
            return true;
        }
        false
    }

    pub(crate) async fn prepare_run_computer(&self, profile: &BotProfile, run: &Run) -> bool {
        let target = match run.computer_target.as_deref() {
            Some("pc") => match run.computer_node.as_deref() {
                Some(id) if id == computer_target::LEGACY_LOCAL_COMPUTER_ID => ComputerTarget::Host,
                Some(id) => ComputerTarget::Node(id.into()),
                None => ComputerTarget::Pc,
            },
            Some(_) => return false,
            None => profile.runtime.computer.clone(),
        };
        self.prepare_computer_provider(&target).await
    }

    fn desktop_tools_registered(&self) -> bool {
        self.tools
            .list()
            .iter()
            .any(|s| s.category == PermissionCategory::Desktop || s.name.starts_with("desktop_"))
    }

    fn browser_tools_registered(&self) -> bool {
        self.tools
            .list()
            .iter()
            .any(|s| s.category == PermissionCategory::Browser || s.name.starts_with("browser_"))
    }

    /// Resolve a bot's computer target for a single run, honoring a previously
    /// persisted resolution label when resuming a fixed-resolution run.
    pub(crate) fn resolve_computer_for_run(
        &self,
        profile: &BotProfile,
        persisted: Option<&Run>,
    ) -> RunComputer {
        let rc = self.resolve_computer_for_run_inner(profile, persisted);
        let provider = self.computer_provider.read().clone();
        Self::without_unverified_local(rc, provider.as_deref())
    }

    /// Never let a local desktop/browser carry an identity that is not a real
    /// machine fingerprint. With no provider installed yet (fallback) the node
    /// is dropped (tools keep working, nothing machine-bound is stored); with a
    /// provider whose identity is unreadable the run gets no local computer.
    fn without_unverified_local(
        mut rc: RunComputer,
        provider: Option<&dyn ComputerProvider>,
    ) -> RunComputer {
        let local_id = provider.map(|p| p.local_node().id);
        let is_local = matches!(rc.label, ComputerLabel::Host | ComputerLabel::Browser)
            || rc
                .node
                .as_ref()
                .is_some_and(|n| local_id.as_deref() == Some(n.id.as_str()));
        if !is_local || rc.unavailable_reason.is_some() {
            return rc;
        }
        let Some(p) = provider else {
            rc.node = None;
            return rc;
        };
        let verified = p.local_identity_bound()
            && rc
                .node
                .as_ref()
                .is_none_or(|n| is_machine_fingerprint(&n.id));
        if verified {
            return rc;
        }
        let mut off = Self::offline_pc(None);
        off.label = ComputerLabel::Off;
        off.wait_for_pc = false;
        off.notice = Some(COMPUTER_IDENTITY_UNBOUND_FRESH_NOTICE.into());
        off
    }

    fn resolve_computer_for_run_inner(
        &self,
        profile: &BotProfile,
        persisted: Option<&Run>,
    ) -> RunComputer {
        let provider = self.computer_provider.read().clone();
        if let Some(reason) =
            persisted.and_then(|run| Self::persisted_computer_error(run, provider.as_deref()))
        {
            return Self::unavailable_computer(reason);
        }
        if let Some(run) = persisted
            && let Some(label) = run.computer_target.as_deref()
        {
            let Some(label) = ComputerLabel::parse(label) else {
                let mut rc = self.resolve_fallback_computer(profile);
                rc.desktop = None;
                rc.desktop_available = false;
                rc.browser = false;
                rc.unavailable_reason = Some("Unknown persisted computer target".into());
                return rc;
            };
            if label == ComputerLabel::Pc {
                // A never-bound initial wait may select its first PC. A previously
                // used PC with a missing key cannot safely be reconstructed.
                if run.computer_node.is_none()
                    && run.pause_reason.as_deref() == Some(WAIT_FOR_PC_REASON)
                {
                    return provider.as_deref().map_or_else(
                        || Self::offline_pc(None),
                        |p| {
                            Self::run_computer(
                                resolve_computer(&ComputerTarget::Pc, p),
                                p.has_display(),
                            )
                        },
                    );
                }
                let mut node = Some(computer_target::ComputerNode {
                    id: run
                        .computer_node
                        .clone()
                        .unwrap_or_else(|| "unresolved".into()),
                    name: run
                        .computer_node_name
                        .clone()
                        .unwrap_or_else(|| "Unknown PC".into()),
                });
                let desktop = provider.as_deref().and_then(|p| {
                    let local_id = p.local_node().id;
                    match run.computer_node.as_deref() {
                        // This daemon's own desktop. The legacy constant matches
                        // only when this provider still uses that constant.
                        Some(id) if id == local_id && !p.is_host() => p.local(),
                        Some(id) if id == local_id => None,
                        Some(id) => p.node_target(id).filter(|d| d.node.id == id).map(|d| {
                            node = Some(d.node);
                            d.backend
                        }),
                        None => None,
                    }
                });
                let mut rc = Self::offline_pc(node);
                rc.wait_for_pc = desktop.is_none();
                rc.desktop_available = desktop.is_some();
                rc.desktop = desktop;
                if rc.wait_for_pc && provider.as_deref().is_some_and(|p| !p.pc_network_enabled()) {
                    rc.wait_for_pc = false;
                    rc.unavailable_reason =
                        Some("PC desktop unavailable: peer networking is disabled".into());
                }
                let local = provider.as_ref().is_some_and(|p| {
                    run.computer_node.as_deref() == Some(p.local_node().id.as_str())
                });
                rc.browser = local
                    && provider
                        .as_deref()
                        .is_some_and(ComputerProvider::browser_available);
                rc.browser_headless = rc.browser.then_some(true);
                return rc;
            }
            if label == ComputerLabel::Server {
                return self.resume_server_computer(profile, run);
            }
            let target = match label {
                ComputerLabel::Host => ComputerTarget::Host,
                ComputerLabel::Browser => ComputerTarget::Browser,
                _ => ComputerTarget::Off,
            };
            return if let Some(p) = provider.as_deref() {
                Self::run_computer(resolve_computer(&target, p), p.has_display())
            } else {
                let mut fixed = profile.clone();
                fixed.runtime.computer = target;
                self.resolve_fallback_computer(&fixed)
            };
        }
        if let ComputerTarget::Server(_) = &profile.runtime.computer {
            return self.resolve_server_computer(&profile.runtime.computer);
        }
        if let Some(p) = provider.as_deref() {
            let mut rc = Self::run_computer(
                resolve_computer(&profile.runtime.computer, p),
                p.has_display(),
            );
            rc.notice = auto_unreachable_notice(&profile.runtime.computer, p, &rc);
            return rc;
        }
        self.resolve_fallback_computer(profile)
    }

    fn persisted_computer_error(
        run: &Run,
        provider: Option<&dyn ComputerProvider>,
    ) -> Option<&'static str> {
        if run.computer_target.is_none()
            || run.computer_node.as_deref().is_some_and(|id| {
                id == computer_target::LEGACY_LOCAL_COMPUTER_ID || id.ends_with(":local")
            })
        {
            return Some(
                "Cannot resume legacy run: original computer identity is unknown; start a new run",
            );
        }
        if matches!(run.computer_target.as_deref(), Some("host" | "browser"))
            && provider
                .is_some_and(|p| run.computer_node.as_deref() != Some(p.local_node().id.as_str()))
        {
            return Some(
                "Cannot resume: local computer identity changed or state was imported; start a new run",
            );
        }
        None
    }

    /// Why a child run may not take the placement its parent resolved:
    /// either row is unusable on its own (no stored target, a legacy local id,
    /// or a host/browser id that is not this machine), or the child's stored
    /// target or node differs from what the validated parent resolved.
    fn child_placement_error(
        child: &Run,
        parent: &Run,
        resolved: &RunComputer,
        provider: Option<&dyn ComputerProvider>,
    ) -> Option<String> {
        if let Some(reason) = Self::persisted_computer_error(child, provider)
            .or_else(|| Self::persisted_computer_error(parent, provider))
        {
            return Some(reason.to_owned());
        }
        let node = resolved.node.as_ref().map(|n| n.id.as_str());
        if child.computer_target.as_deref() != Some(resolved.label.as_str())
            || child.computer_node.as_deref() != node
        {
            return Some(
                "Cannot resume: this sub-agent's stored computer does not match its parent's; start a new run"
                    .to_owned(),
            );
        }
        None
    }

    /// A stored host/browser run that cannot be compared with this boot's local
    /// identity because the identity key was unreadable. The run stays paused
    /// (retryable on a later boot); it is neither failed nor rebound.
    pub(crate) fn computer_identity_unbound(
        run: &Run,
        provider: Option<&dyn ComputerProvider>,
    ) -> bool {
        Self::persisted_computer_error(run, None).is_none()
            && matches!(run.computer_target.as_deref(), Some("host" | "browser"))
            && provider.is_some_and(|p| !p.local_identity_bound())
    }

    /// A server run resumes only through the pairing as it is now, and only on
    /// the server the run was placed on.
    fn resume_server_computer(&self, profile: &BotProfile, run: &Run) -> RunComputer {
        let rc = self.resolve_server_computer(&profile.runtime.computer);
        let same = rc.node.as_ref().map(|n| n.id.as_str()) == run.computer_node.as_deref();
        if same || rc.unavailable_reason.is_some() {
            rc
        } else {
            Self::unavailable_computer(
                "Cannot resume: this run used another server as its computer; start a new run",
            )
        }
    }

    /// A bot whose own computer is a paired server: the bot, its model calls and
    /// its approvals stay on THIS daemon; its browser tools are a link to the
    /// server. Only the browser family is routed there: no shell, files or
    /// desktop tool is claimed for the server. No server link, an unknown key or
    /// an unpaired server leaves the run with NO computer: it never falls back
    /// to this daemon's browser.
    fn resolve_server_computer(&self, target: &ComputerTarget) -> RunComputer {
        let id = target.server_id().unwrap_or_default();
        let found = self
            .server_links
            .read()
            .clone()
            .and_then(|links| links.link(id));
        let Some((name, link)) = found else {
            return Self::unavailable_computer(SERVER_COMPUTER_UNAVAILABLE);
        };
        RunComputer {
            node: Some(computer_target::ComputerNode {
                id: id.to_owned(),
                name,
            }),
            unavailable_reason: None,
            label: ComputerLabel::Server,
            desktop: None,
            desktop_available: false,
            browser: true,
            browser_headless: None,
            wait_for_pc: false,
            notice: None,
            server_link: Some(link),
        }
    }

    fn unavailable_computer(reason: &str) -> RunComputer {
        let mut rc = Self::offline_pc(None);
        rc.wait_for_pc = false;
        rc.unavailable_reason = Some(reason.into());
        rc
    }

    fn offline_pc(node: Option<computer_target::ComputerNode>) -> RunComputer {
        RunComputer {
            node,
            unavailable_reason: None,
            label: ComputerLabel::Pc,
            desktop: None,
            desktop_available: false,
            browser: false,
            browser_headless: None,
            wait_for_pc: true,
            notice: None,
            server_link: None,
        }
    }

    fn run_computer(res: Resolution, has_display: bool) -> RunComputer {
        let browser_headless = res.browser.then(|| res.browser_headless(has_display));
        RunComputer {
            label: res.label,
            node: res.node,
            unavailable_reason: res.unavailable_reason,
            desktop_available: res.desktop.is_some(),
            desktop: res.desktop,
            browser: res.browser,
            browser_headless,
            wait_for_pc: res.wait_for_pc,
            notice: None,
            server_link: None,
        }
    }

    pub(crate) fn bind_computer(&self, run_id: RunId, rc: &RunComputer) -> storage::Result<()> {
        // Lowest common point: a local kind never persists a non-fingerprint id.
        if matches!(rc.label, ComputerLabel::Host | ComputerLabel::Browser)
            && rc
                .node
                .as_ref()
                .is_some_and(|n| !is_machine_fingerprint(&n.id))
        {
            return Err(storage::StorageError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "refusing to store a local computer id that is not a machine fingerprint",
            )));
        }
        self.storage.bind_run_computer(
            run_id,
            rc.label.as_str(),
            rc.node.as_ref().map(|n| n.id.as_str()),
            rc.node.as_ref().map(|n| n.name.as_str()),
        )
    }

    fn resolve_fallback_computer(&self, profile: &BotProfile) -> RunComputer {
        let static_desktop = self.static_desktop.read().clone();
        let desktop_tools = self.desktop_tools_registered();
        let browser_tools = self.browser_tools_registered();
        let fallback = FallbackComputerProvider {
            desktop: static_desktop.clone(),
            browser: browser_tools,
        };
        if matches!(
            profile.runtime.computer,
            ComputerTarget::Pc | ComputerTarget::Node(_)
        ) {
            return Self::offline_pc(profile.runtime.computer.node_id().map(|id| {
                computer_target::ComputerNode {
                    id: id.into(),
                    name: id.into(),
                }
            }));
        }
        let mut res = resolve_computer(&profile.runtime.computer, &fallback);
        let allow_manual_desktop = static_desktop.is_none()
            && desktop_tools
            && matches!(profile.runtime.computer, ComputerTarget::Auto);
        if allow_manual_desktop {
            res.label = match profile.runtime.computer {
                ComputerTarget::Host => ComputerLabel::Host,
                _ => ComputerLabel::Pc,
            };
            res.wait_for_pc = false;
        }
        let browser_headless = (res.label == ComputerLabel::Browser && res.browser).then_some(true);
        let desktop_available = res.desktop.is_some() || allow_manual_desktop;
        RunComputer {
            node: res.node,
            unavailable_reason: res.unavailable_reason,
            label: res.label,
            desktop: res.desktop,
            desktop_available,
            browser: res.browser,
            browser_headless,
            wait_for_pc: res.wait_for_pc,
            notice: None,
            server_link: None,
        }
    }

    /// Exposure a run starts with: the parent's (subagents), what its own
    /// history already holds, and the provenance of the chat messages that
    /// enter its context.
    pub(crate) fn initial_taint(&self, run: &Run, chat_id: ChatId) -> ContentTaint {
        let mut taint = ContentTaint::default();
        if let Some(parent_id) = run.parent_run_id {
            let inherited = self
                .runs
                .lock()
                .get(&parent_id)
                .map(|ctx| ctx.taint.lock().clone());
            if let Some(parent) = inherited {
                taint.inherit(&parent);
            }
        }
        absorb_run_history(&mut taint, self, run.id, 0);
        // A subagent does not see the chat. A top-level turn, including a
        // scheduled one, does: provenance rides on the messages that enter
        // the context. Dropped by the history bound means the bytes are not
        // in front of the model, so those bits are not inherited.
        if run.parent_run_id.is_none() {
            let prior = crate::history::chat_provenance(
                &self.storage,
                chat_id,
                Some(run.id),
                schedule_id_of(&run.kind),
            );
            taint.exposure.inherit(prior.exposure);
            for label in prior.sources {
                taint.note_label(&label);
            }
            // Text read out of an attached file is untrusted data like any tool read.
            if let Some(name) = crate::history::text_attachment_source(&self.storage, run) {
                taint.exposure.observe(true, false);
                taint.note_label(&format!("attached file {name}"));
            }
            // Another bot's reply (an `@mention` chat) reaches this run as
            // untrusted data whatever that bot read. Same single exposure
            // state as every other source: no separate flag for mentions.
            if crate::history::has_foreign_replies(&self.storage, run) {
                taint.exposure.observe(true, false);
                taint.note_label("another bot's reply");
            }
        }
        taint
    }

    /// The user's own paired computer, the target of the `pc_*` tools.
    fn mypc_node(&self) -> Option<computer_target::ComputerNode> {
        self.computer_provider
            .read()
            .as_ref()
            .and_then(|p| p.mypc_node())
    }

    /// Per-run context, built on first use (also after a restart).
    #[allow(clippy::too_many_lines)] // One constructor: every per-run field in one place.
    pub fn ctx(&self, run: &Run) -> Result<Arc<RunCtx>, ExecError> {
        if let Some(c) = self.runs.lock().get(&run.id) {
            return Ok(Arc::clone(c));
        }
        let chat_id = run.chat_id.ok_or_else(|| xe("turn run has no chat"))?;
        let seq = read_event_log(self.storage.data_dir(), run.id)
            .map_or(0, |v| v.last().map_or(0, |e| e.seq + 1));
        let (profile, bot_policy, initiator, sub, resolved_computer) = match run.parent_run_id {
            None => {
                let raw_profile = self.profile_for(run.bot_id).map_err(xe)?;
                let rc = self
                    .run_computers
                    .lock()
                    .remove(&run.id)
                    .unwrap_or_else(|| self.resolve_computer_for_run(&raw_profile, Some(run)));
                let profile = Arc::new(self.run_profile(&raw_profile, &rc));
                (
                    profile,
                    raw_profile.policy(),
                    initiator_of(&run.kind),
                    None,
                    rc,
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
                // A child inherits its parent's resolved computer, but its own
                // stored placement must agree with it. A resumed child with no
                // stored target, a legacy id or a different node fails closed
                // here, before anything runs, with or without startup
                // reconciliation.
                let provider = self.computer_provider.read().clone();
                if let Some(reason) = Self::child_placement_error(
                    run,
                    &parent,
                    &pctx.resolved_computer,
                    provider.as_deref(),
                ) {
                    return Err(xe(reason));
                }
                let spec: SubSpec =
                    serde_json::from_value(run.spec.clone().unwrap_or(Value::Null)).map_err(xe)?;
                let model = spec.model_for(&pctx.profile).map_err(xe)?;
                let profile = Arc::new(
                    pctx.profile
                        .narrowed(spec.tools.as_deref(), Some(&model))
                        .map_err(xe)?,
                );
                (
                    profile,
                    pctx.bot_policy.clone(),
                    pctx.initiator,
                    Some(spec),
                    pctx.resolved_computer.clone(),
                )
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
        let taint = self.initial_taint(run, chat_id);
        let steer = self.pending_steering(run)?;
        let mypc = self.mypc_node();
        let ctx = Arc::new(RunCtx {
            mypc,
            pc_reads: Mutex::new(std::collections::BTreeSet::new()),
            computer,
            waiting_computer: Arc::new(AtomicBool::new(false)),
            resolved_computer,
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
                steer,
                closed: false,
                seq,
                todos: Vec::new(),
                secret_requests: 0,
            }),
            taint: Mutex::new(taint),
            control_grants: Mutex::new(Vec::new()),
            tool_grants: Mutex::new(Vec::new()),
            page_origin: Mutex::new(None),
        });
        Ok(Arc::clone(self.runs.lock().entry(run.id).or_insert(ctx)))
    }

    /// Accepted steering is stored as a user message before it is queued.
    /// Every planned user step consumes one such row, even if its no-op
    /// execution was interrupted: history replays the durable step input.
    fn pending_steering(&self, run: &Run) -> Result<VecDeque<String>, ExecError> {
        if run.parent_run_id.is_some()
            || RunState::parse(&run.state).is_some_and(RunState::is_terminal)
        {
            return Ok(VecDeque::new());
        }
        let Some(chat_id) = run.chat_id else {
            return Ok(VecDeque::new());
        };
        let consumed = self
            .storage
            .list_steps(run.id)
            .map_err(xe)?
            .iter()
            .filter(|step| step.name == STEP_USER)
            .count()
            // The original task belongs to first_step, including when no
            // step intent was written before the restart.
            .max(1);
        Ok(self
            .storage
            .list_messages(chat_id)
            .map_err(xe)?
            .into_iter()
            .filter(|message| message.run_id == Some(run.id) && message.role == "user")
            .skip(consumed)
            .map(|message| message.content)
            .collect())
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
        !self.cfg.safe_mode
            && profile.spec_visible(&spec.name, spec.category)
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
        if self.cfg.safe_mode {
            return Vec::new();
        }
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
        // Safe mode: nothing resolves, so nothing runs (fail closed).
        if self.cfg.safe_mode {
            return None;
        }
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
            if !profile.spec_visible(name, own.category) || !platform_ok(&own.platforms) {
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

    /// Resolve against the definitions this request actually advertised, then
    /// recheck the live registry and the bot's visibility.
    pub(crate) fn resolve_offered(
        &self,
        ctx: &RunCtx,
        name: &str,
        input: &Value,
        hints: &[String],
        offered: Option<&[String]>,
    ) -> Option<Resolved> {
        let Some(offered) = offered else {
            return self.resolve_call(&ctx.profile, ctx.bot_id, name, input, hints);
        };
        if self.cfg.safe_mode || !offered.iter().any(|n| n == name) {
            return None;
        }
        let tool = if matches!(
            name,
            agent_router::TOOL_SEARCH | agent_router::TOOL_DESCRIBE
        ) {
            self.surface
                .deferred_tools()
                .iter()
                .find(|t| t.spec().name == name)
                .cloned()?
        } else {
            let tool = self.tools.get(name)?;
            if !self.spec_visible(&ctx.profile, ctx.bot_id, tool.spec()) {
                return None;
            }
            tool
        };
        Some(Resolved {
            spec: Some(tool.spec().clone()),
            tool,
            input: input.clone(),
            name: name.to_owned(),
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

    /// The discovery-mode prompt block for this bot, or `None` when the model
    /// was handed its tools directly (no `find_tool` among `specs`) or the bot
    /// may use none of the advertised families. Built from the permission-
    /// filtered registry, independent of per-turn router hints. Unchanged
    /// registry and policy inputs produce identical bytes across turns.
    pub(crate) fn discovery_block(&self, ctx: &RunCtx, specs: &[ToolSpec]) -> Option<String> {
        let deferred = specs.iter().any(|s| s.name == agent_router::TOOL_SEARCH);
        if !deferred && !specs.iter().any(|s| s.name == agent_router::FIND_TOOL) {
            return None;
        }
        let visible = self.visible_specs(&ctx.profile, ctx.bot_id);
        let own_screen = self.is_host_homed() && ctx.resolved_computer.label == ComputerLabel::Host;
        crate::discovery_prompt::block_for(
            visible
                .iter()
                .filter(|s| {
                    self.decide(ctx, &s.name, s.effect, s.category, &Value::Null) != Decision::Deny
                })
                .map(|s| s.name.as_str()),
            own_screen,
            deferred,
        )
    }

    /// The "choosing tools" prompt block: which tool family fits which job and
    /// that a claim needs the result of the call behind it. Built from the
    /// tools the bot may use (all of them in discovery mode, where `specs`
    /// holds only the meta tools), so it is stable across a run's model calls.
    pub(crate) fn tool_guidance_block(&self, ctx: &RunCtx, specs: &[ToolSpec]) -> Option<String> {
        let families = if specs.iter().any(|s| {
            matches!(
                s.name.as_str(),
                agent_router::FIND_TOOL | agent_router::TOOL_SEARCH
            )
        }) {
            let visible = self.visible_specs(&ctx.profile, ctx.bot_id);
            crate::tool_guidance::Families::of(
                visible
                    .iter()
                    .filter(|s| {
                        self.decide(ctx, &s.name, s.effect, s.category, &Value::Null)
                            != Decision::Deny
                    })
                    .map(|s| s.name.as_str()),
            )
        } else {
            crate::tool_guidance::Families::of(specs.iter().map(|s| s.name.as_str()))
        };
        crate::tool_guidance::block(families)
    }

    /// This daemon is an always-on host (the bots' own computer).
    pub(crate) fn is_host_homed(&self) -> bool {
        self.computer_provider
            .read()
            .as_ref()
            .is_some_and(|p| p.is_host())
    }

    /// The display of this computer's own desktop, only when its backend
    /// really runs (the run resolved to the host with a desktop) and the
    /// daemon's environment names one.
    fn real_display(&self, rc: &RunComputer) -> Option<String> {
        if rc.label != ComputerLabel::Host || rc.desktop.is_none() {
            return None;
        }
        let guard = self.computer_provider.read();
        let provider: &Arc<dyn ComputerProvider> = guard.as_ref()?;
        let name = provider.display_name()?;
        crate::discovery_prompt::clean_display(&name)
    }

    /// The per-run "where you run" block, only for a host-homed daemon: which
    /// machine the shell, files, desktop and browser tools act on, its OS and
    /// shell, and how the user's computer is addressed. Fixed for the run.
    pub(crate) fn place_block(&self, ctx: &RunCtx) -> Option<String> {
        if ctx.resolved_computer.label == ComputerLabel::Server {
            let name = ctx.resolved_computer.node.as_ref().map(|n| n.name.as_str());
            return Some(crate::discovery_prompt::server_place_block(
                name.unwrap_or_default(),
            ));
        }
        if !self.is_host_homed() {
            let rc = &ctx.resolved_computer;
            return Some(crate::discovery_prompt::local_browser_note(
                rc.label,
                rc.browser,
                rc.browser_headless,
                rc.unavailable_reason.is_some() || rc.wait_for_pc,
            ));
        }
        let rc = &ctx.resolved_computer;
        let platform = rc.desktop.as_ref().map(|d| d.capabilities().platform);
        let display = self.real_display(rc);
        crate::discovery_prompt::place_block(&crate::discovery_prompt::Place {
            label: rc.label,
            node_name: rc.node.as_ref().map(|n| n.name.as_str()),
            desktop_platform: platform.as_deref(),
            browser: rc.browser,
            display: display.as_deref(),
        })
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
            | Event::SecretRequested(_)
            | Event::ControlRequested(_)
            | Event::SubagentStarted(_)
            | Event::SubagentFinished(_)
            | Event::Error(_)
    )
}

/// Rebuild taint from stored steps of `run_id` and its subagents.
///
/// Used when a run context is created (resume, or a parent absorbing a child
/// whose in-memory context is already gone).
pub(crate) fn absorb_run_history(taint: &mut ContentTaint, sh: &Shared, run_id: RunId, depth: u32) {
    if depth > 8 {
        return;
    }
    let Ok(steps) = sh.storage.list_steps(run_id) else {
        return;
    };
    for step in &steps {
        taint::observe_history_step(taint, &step.name, &step.input, step.output.as_ref());
    }
    let Ok(kids) = sh.storage.list_child_runs(run_id) else {
        return;
    };
    for kid in kids {
        absorb_run_history(taint, sh, kid.id, depth + 1);
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
    /// Tool calls a CLI session ran inside this model step. The ordinary tool
    /// step is not written for those calls; the facts are.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub executed: Vec<crate::taint::ExecFact>,
    /// External tool descriptions and schemas that were part of this request.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metadata: Vec<crate::taint::ExecFact>,
    /// Canonical names actually requested from this model. Legacy rows omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offered_tools: Option<Vec<String>>,
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
    /// What this call did to the model's context. Absent only on rows written
    /// before the field existed; those rows are not classified from their text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fact: Option<crate::taint::ExecFact>,
    /// Light-tier steps only: host facts about whether the call ran and whether
    /// it was valid. A tool cannot label its own output clean.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier_executed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier_invalid: Option<bool>,
    /// How the call ended (ok, error, timed out, denied, ...). Absent on rows
    /// written before the field existed; the ledger classifies those itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<crate::ledger::Outcome>,
    /// A validated, successful run-local schema activation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activated_tool: Option<String>,
}

pub(crate) const STEP_USER: &str = "user_message";
pub(crate) const STEP_MODEL: &str = "model";
pub(crate) const STEP_TOOLS: &str = "tools";
/// A saved workflow replayed by the router (K0-K3), instead of a model call.
pub(crate) const STEP_WORKFLOW: &str = "workflow";
/// A built-in command run by the router (`/new`).
pub(crate) const STEP_COMMAND: &str = "command";
/// One system nudge after a model turn ended with no text after tool calls.
pub(crate) const STEP_NUDGE: &str = "nudge";
