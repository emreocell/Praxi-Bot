//! Local daemon API: one typed HTTP API (`/v1`) plus one SSE event stream in
//! front of the [`agent_core::AgentLoop`].
//!
//! The crate is a library so the desktop app can embed it; `bot serve` just
//! builds an [`AppState`] and calls [`serve`].
//!
//! Security model (ADR-0009 / ADR-0013): the caller binds the listener to
//! `127.0.0.1`; on top of that every request must carry a loopback `Host`
//! (DNS-rebinding protection), a browser `Origin` must be one of the
//! configured desktop-app origins, and everything except `GET /v1/health`
//! needs `Authorization: Bearer <per-install token>` (see [`auth`]).  The one
//! exception is `GET /v1/events`, which also accepts `?token=<token>` for
//! browser `EventSource` clients (Host / Origin still apply).

pub mod auth;
pub mod control;
pub mod dto;
pub mod email;
mod email_routes;
pub mod error;
#[cfg(any(test, feature = "fake-provider"))]
pub mod fake;
pub mod host;
pub mod live_view;
pub mod mcp;
mod mcp_routes;
pub mod openapi;
pub mod peer;
pub mod policy;
pub use peer::{PeerIdentity, PeerRole, peer_router};
mod routes;
mod schedule;
mod schedule_tools;
mod sse;
pub mod web;
mod web_routes;

#[cfg(test)]
mod schedule_tools_tests;

#[cfg(test)]
mod tests;

use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use agent_core::{AgentLoop, BotProfile, Providers};
use axum::Router;
use axum::routing::{get, post, put};
use protocol::{BotId, ChatId, RunId, UserId};
use provider_catalog::Catalog;
use scheduler::{ScheduleStore, Scheduler};
use storage::{Bot, Chat, Run, Storage, User, now_ms};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

/// Connection tests (`/v1/email/*/test`) that may run at the same time.
const EMAIL_TESTS_AT_ONCE: usize = 2;

pub use auth::{TOKEN_FILE_NAME, TokenInfo, load_or_create_token, read_token, token_path};
pub use control::{ControlConfig, ControlHub};
pub use email::{EmailControl, EmailControlError};
pub use error::ApiError;
pub use mcp::{McpControl, McpControlError};
pub use openapi::openapi_json;
pub use policy::{PolicyLoadError, load_policy, validate_policy};
pub use schedule_tools::{ScheduleLimits, load_schedule_limits};
pub use web::{WebControl, WebControlError};

/// Default TCP port of `bot serve`.
pub const DEFAULT_PORT: u16 = 47821;

/// HTTP API compatibility version advertised by health and host status.
pub const API_VERSION: &str = "1.0";

/// Origins of the desktop app (Tauri, per platform) accepted by default.
pub const DEFAULT_ALLOWED_ORIGINS: &[&str] = &[
    "tauri://localhost",
    "http://tauri.localhost",
    "https://tauri.localhost",
];

/// Daemon settings.
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// Bearer token clients must present.
    pub token: String,
    /// Browser origins that may call the API (exact match).
    pub allowed_origins: Vec<String>,
    /// How long [`serve`] lets running tool steps finish on shutdown before
    /// interrupting them (see [`agent_core::AgentLoop::shutdown`]).
    pub shutdown_grace: Duration,
    /// Timing of the desktop control bridge (see [`control`]).
    pub control: ControlConfig,
    /// Most concurrent `GET /v1/events` streams (each holds replay buffers);
    /// further requests get `503` until one closes.
    pub max_sse_subscribers: usize,
}

/// Default [`DaemonConfig::max_sse_subscribers`].
pub const DEFAULT_MAX_SSE_SUBSCRIBERS: usize = 16;

/// Default [`DaemonConfig::shutdown_grace`].
pub const DEFAULT_SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

impl DaemonConfig {
    /// Config with the default desktop origins.
    #[must_use]
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
            allowed_origins: DEFAULT_ALLOWED_ORIGINS
                .iter()
                .map(ToString::to_string)
                .collect(),
            shutdown_grace: DEFAULT_SHUTDOWN_GRACE,
            control: ControlConfig::default(),
            max_sse_subscribers: DEFAULT_MAX_SSE_SUBSCRIBERS,
        }
    }
}

/// What `/v1/providers` needs from the embedder: the catalog, and a way to
/// rebuild the providers from the configuration (which needs the secret
/// store, so only the embedder can do it).
pub struct ProviderHost {
    /// Built-in catalog entries.
    pub catalog: Catalog,
    /// Rebuild the loop's providers from `providers.toml` and the secret store.
    pub reload: Box<dyn Fn() -> Result<Providers, String> + Send + Sync>,
}

/// A messaging channel the embedder runs next to the daemon (see
/// [`AppState::add_channel`]); backs `/v1/channels`.
#[async_trait::async_trait]
pub trait ChannelControl: Send + Sync {
    /// Start polling when this installation becomes active.
    fn start(&self) {}
    /// Stop polling before relinquishing leadership.
    async fn stop(&self) {}
    /// Current state of the channel.
    fn info(&self) -> dto::ChannelDto;
    /// A fresh one-time pairing code.
    ///
    /// # Errors
    /// The reason the channel cannot pair (e.g. it is not running).
    fn create_pair_code(&self) -> Result<dto::PairCodeDto, String>;
}

struct Inner {
    /// `schedule_changed` notifications for the daemon-wide event stream.
    schedule_events: tokio::sync::broadcast::Sender<dto::ScheduleChanged>,
    /// Channels registered by the embedder.
    channels: parking_lot::Mutex<Vec<Arc<dyn ChannelControl>>>,
    agent: AgentLoop,
    storage: Arc<Storage>,
    user_id: UserId,
    cfg: DaemonConfig,
    started: Instant,
    sse_gaps_total: Arc<std::sync::atomic::AtomicU64>,
    /// Open `GET /v1/events` streams.
    sse_subscribers: Arc<std::sync::atomic::AtomicUsize>,
    /// `(run, from_gseq)` of gaps already audited (bounded; see `sse`).
    sse_audited_gaps: Arc<parking_lot::Mutex<std::collections::HashSet<(uuid::Uuid, u64)>>>,
    shutdown: CancellationToken,
    hub: Arc<ControlHub>,
    live_view: Arc<live_view::LiveView>,
    refresh_rx: parking_lot::Mutex<Option<tokio::sync::watch::Receiver<u64>>>,
    provider_host: OnceLock<ProviderHost>,
    /// MCP server management (see [`AppState::set_mcp`]).
    mcp: OnceLock<Arc<dyn McpControl>>,
    /// E-mail account management (see [`AppState::set_email`]).
    email: OnceLock<Arc<dyn EmailControl>>,
    /// Bounds concurrent "test connection" calls (see `email_routes`).
    email_tests: Arc<tokio::sync::Semaphore>,
    /// Web search settings management (see [`AppState::set_web`]).
    web: OnceLock<Arc<dyn WebControl>>,
    /// Bounds concurrent web search probes (see `web_routes`).
    web_tests: Arc<tokio::sync::Semaphore>,
    /// Write-only secret sink of `POST /v1/secrets/{name}`.
    secrets: OnceLock<Arc<dyn secret_store::SecretStore>>,
    /// `schedules.db`, opened on first use.
    schedules: parking_lot::Mutex<Option<Arc<ScheduleStore>>>,
    /// Serialises `PUT /v1/policy` (read-modify-write of memory + `policy.json`).
    policy_lock: parking_lot::Mutex<()>,
    /// Serialises `providers.toml` read-modify-write operations (`POST /v1/providers/cli/enable`).
    providers_lock: parking_lot::Mutex<()>,
    /// Why `policy.json` could not be loaded (locked deny-all mode), if so.
    policy_load_error: parking_lot::Mutex<Option<String>>,
    /// CLI provider detector (`GET /v1/providers/detected`).
    detector: parking_lot::RwLock<Arc<cli_provider::Detector>>,
    /// The running scheduler (see [`AppState::start_scheduler`]).
    scheduler: parking_lot::Mutex<Option<Scheduler>>,
    leader: OnceLock<host::LeaderControl>,
    #[cfg(test)]
    scheduler_clock: parking_lot::Mutex<Option<Arc<dyn scheduler::Clock>>>,
    /// Shutdown steps in the order they ran (tests only).
    #[cfg(test)]
    order: parking_lot::Mutex<Vec<&'static str>>,
}

/// Shared handler state (cheap to clone).
#[derive(Clone)]
pub struct AppState(Arc<Inner>);

/// A non-owning handle for background tasks that must not keep the daemon alive.
#[derive(Clone)]
pub struct WeakAppState(std::sync::Weak<Inner>);

impl WeakAppState {
    /// The state, unless the daemon is gone.
    #[must_use]
    pub fn upgrade(&self) -> Option<AppState> {
        self.0.upgrade().map(AppState)
    }
}

impl AppState {
    /// See [`WeakAppState`].
    #[must_use]
    pub fn downgrade(&self) -> WeakAppState {
        WeakAppState(Arc::downgrade(&self.0))
    }
}

impl AppState {
    /// Build the state.  The (single) local user is the first stored user and
    /// is created when the database has none.
    ///
    /// # Errors
    /// Storage errors.
    pub fn new(
        agent: AgentLoop,
        storage: Arc<Storage>,
        cfg: DaemonConfig,
    ) -> Result<Self, storage::StorageError> {
        Self::with_view_registry(
            agent,
            storage,
            cfg,
            Arc::new(::live_view::TargetRegistry::default()),
        )
    }

    /// Assemble with the same target registry supplied to `BuiltinConfig`.
    /// # Errors
    /// Returns storage errors while loading or creating the local user.
    pub fn with_view_registry(
        agent: AgentLoop,
        storage: Arc<Storage>,
        cfg: DaemonConfig,
        registry: Arc<::live_view::TargetRegistry>,
    ) -> Result<Self, storage::StorageError> {
        let user_id = if let Some(u) = storage.list_users()?.into_iter().next() {
            u.id
        } else {
            let u = User {
                id: UserId::new(),
                name: "me".to_owned(),
                created_at_ms: now_ms(),
            };
            storage.insert_user(&u)?;
            u.id
        };
        let hub = Arc::new(ControlHub::with_latch_file(
            cfg.control,
            storage.data_dir().join("control-latch.json"),
        ));
        agent.set_control_gate(hub.clone());
        let live_view = Arc::new(live_view::LiveView::with_registry(registry));
        // A watch channel coalesces completion refreshes, including partial failures.
        let (refresh_tx, refresh_rx) = tokio::sync::watch::channel(0u64);
        hub.set_refresh_hook(Arc::new(move |seq| {
            refresh_tx.send_replace(seq);
        }));
        let policy_load_error = match policy::load_policy(storage.data_dir()) {
            Ok(Some(p)) => {
                agent.set_global_policy(p);
                None
            }
            Ok(None) => None,
            Err(e) => {
                // Fail closed (SEC-13): never fall back to the default policy
                // because the saved one is unusable.
                policy::preserve_corrupt(storage.data_dir());
                agent.set_global_policy(permission_engine::Policy::read_only());
                agent.set_policy_locked(true);
                let msg = format!("{e}; all tool actions are denied until a policy is saved");
                eprintln!("daemon-api: WARNING: {msg}");
                Some(msg)
            }
        };
        let host_config = host::HostConfig::load(storage.data_dir())?;
        let state = Self(Arc::new(Inner {
            schedule_events: tokio::sync::broadcast::channel(64).0,
            channels: parking_lot::Mutex::new(Vec::new()),
            agent,
            storage,
            user_id,
            cfg,
            started: Instant::now(),
            sse_gaps_total: Arc::default(),
            sse_subscribers: Arc::default(),
            sse_audited_gaps: Arc::default(),
            shutdown: CancellationToken::new(),
            hub,
            live_view,
            refresh_rx: parking_lot::Mutex::new(Some(refresh_rx)),
            provider_host: OnceLock::new(),
            mcp: OnceLock::new(),
            email: OnceLock::new(),
            email_tests: Arc::new(tokio::sync::Semaphore::new(EMAIL_TESTS_AT_ONCE)),
            web: OnceLock::new(),
            web_tests: Arc::new(tokio::sync::Semaphore::new(EMAIL_TESTS_AT_ONCE)),
            secrets: OnceLock::new(),
            schedules: parking_lot::Mutex::new(None),
            policy_lock: parking_lot::Mutex::new(()),
            providers_lock: parking_lot::Mutex::new(()),
            policy_load_error: parking_lot::Mutex::new(policy_load_error),
            detector: parking_lot::RwLock::new(Arc::new(cli_provider::Detector::new())),
            scheduler: parking_lot::Mutex::new(None),
            leader: OnceLock::new(),
            #[cfg(test)]
            scheduler_clock: parking_lot::Mutex::new(None),
            #[cfg(test)]
            order: parking_lot::Mutex::new(Vec::new()),
        }));
        let _ = state.0.leader.set(host::LeaderControl::new(
            state.data_dir(),
            host_config,
            state.downgrade(),
        ));
        Ok(state)
    }

    /// Let `/v1/providers` list the catalog and reload the providers (call
    /// once, right after [`AppState::new`]; a second call is ignored).
    pub fn set_provider_host(&self, host: ProviderHost) {
        let _ = self.0.provider_host.set(host);
    }

    /// Enable `/v1/mcp/*` with the embedder MCP manager (call once, before
    /// [`serve`]; a second call is ignored).
    pub fn set_mcp(&self, mcp: Arc<dyn McpControl>) {
        let _ = self.0.mcp.set(mcp);
    }

    /// Enable `/v1/email/*` with the embedder e-mail manager (call once, before
    /// [`serve`]; a second call is ignored).
    pub fn set_email(&self, email: Arc<dyn EmailControl>) {
        let _ = self.0.email.set(email);
    }

    /// Enable `/v1/web/search*` with the embedder web-search manager (call once,
    /// before [`serve`]; a second call is ignored).
    pub fn set_web(&self, web: Arc<dyn WebControl>) {
        let _ = self.0.web.set(web);
    }

    /// Enable `POST /v1/secrets/{name}` (write-only) over `store` (call once;
    /// a second call is ignored).
    pub fn set_secret_store(&self, store: Arc<dyn secret_store::SecretStore>) {
        let _ = self.0.secrets.set(store);
    }

    pub(crate) fn mcp(&self) -> Option<Arc<dyn McpControl>> {
        self.0.mcp.get().cloned()
    }

    pub(crate) fn email(&self) -> Option<Arc<dyn EmailControl>> {
        self.0.email.get().cloned()
    }

    pub(crate) fn web(&self) -> Option<Arc<dyn WebControl>> {
        self.0.web.get().cloned()
    }

    /// A slot for one connection test, or `429`: tests dial out to hosts the
    /// caller names, so only a couple may run at once.
    pub(crate) fn email_test_permit(
        &self,
    ) -> Result<tokio::sync::OwnedSemaphorePermit, error::ApiError> {
        Arc::clone(&self.0.email_tests)
            .try_acquire_owned()
            .map_err(|_| {
                error::ApiError::new(
                    axum::http::StatusCode::TOO_MANY_REQUESTS,
                    "too_many_requests",
                    "too many connection tests are running; try again in a moment",
                )
            })
    }

    /// A slot for one live web search probe, or `429`.
    pub(crate) fn web_test_permit(
        &self,
    ) -> Result<tokio::sync::OwnedSemaphorePermit, error::ApiError> {
        Arc::clone(&self.0.web_tests)
            .try_acquire_owned()
            .map_err(|_| {
                error::ApiError::new(
                    axum::http::StatusCode::TOO_MANY_REQUESTS,
                    "too_many_requests",
                    "too many web search probes are running; try again in a moment",
                )
            })
    }

    pub(crate) fn secret_store(&self) -> Option<Arc<dyn secret_store::SecretStore>> {
        self.0.secrets.get().cloned()
    }

    /// Ownership handle for runtime transitions and peer conflict detection.
    /// # Panics
    /// Never for a constructed state: the handle is initialized before the state is returned.
    #[must_use]
    pub fn leader(&self) -> &host::LeaderControl {
        self.0.leader.get().expect("initialized with state")
    }

    /// Start automatic components when ownership permits it.
    /// # Errors
    /// Returns errors opening the schedule store.
    pub(crate) fn start_leader_components(&self) -> Result<(), ApiError> {
        if self.0.shutdown.is_cancelled() || !self.leader().config().runs_components() {
            return Ok(());
        }
        self.start_scheduler()?;
        for channel in self.0.channels.lock().iter() {
            channel.start();
        }
        Ok(())
    }

    pub(crate) async fn stop_components(&self) {
        self.stop_scheduler().await;
        let channels = self.0.channels.lock().clone();
        for channel in channels {
            channel.stop().await;
        }
    }

    /// Register a messaging channel for `/v1/channels` (call before [`serve`]).
    pub fn add_channel(&self, channel: Arc<dyn ChannelControl>) {
        self.0.channels.lock().push(channel);
    }

    /// Tell the daemon-wide event stream that a schedule was changed.
    pub(crate) fn publish_schedule_changed(&self, id: uuid::Uuid, kind: dto::ScheduleChangeKind) {
        // No subscriber is not an error.
        let _ = self
            .0
            .schedule_events
            .send(dto::ScheduleChanged::new(id, kind));
    }

    pub(crate) fn subscribe_schedule_events(
        &self,
    ) -> tokio::sync::broadcast::Receiver<dto::ScheduleChanged> {
        self.0.schedule_events.subscribe()
    }

    pub(crate) fn channels(&self) -> Vec<Arc<dyn ChannelControl>> {
        self.0.channels.lock().clone()
    }

    /// The schedule store (`<data dir>/schedules.db`), opened on first use.
    pub(crate) fn schedule_store(&self) -> Result<Arc<ScheduleStore>, scheduler::SchedulerError> {
        let mut g = self.0.schedules.lock();
        if let Some(s) = g.as_ref() {
            return Ok(Arc::clone(s));
        }
        let store = Arc::new(ScheduleStore::from_storage(self.storage())?);
        *g = Some(Arc::clone(&store));
        Ok(store)
    }

    /// The running scheduler, or `503` when the daemon did not start one.
    pub(crate) fn scheduler(&self) -> Result<Scheduler, ApiError> {
        if !self.leader().config().runs_components() {
            return Err(ApiError::conflict("this installation is standby"));
        }
        self.0
            .scheduler
            .lock()
            .clone()
            .ok_or_else(|| ApiError::unavailable("the scheduler is not running"))
    }

    /// Open the schedule store and start the scheduler with an executor that
    /// runs payloads through the agent loop (initiator `Schedule`).  [`serve`]
    /// calls this; a second call is a no-op.
    ///
    /// # Errors
    /// The schedule store cannot be opened.
    pub fn start_scheduler(&self) -> Result<(), scheduler::SchedulerError> {
        if !self.leader().config().runs_components() {
            return Ok(());
        }
        let mut scheduler = self.0.scheduler.lock();
        if !self.leader().config().runs_components() {
            return Ok(());
        }
        if scheduler.is_some() {
            return Ok(());
        }
        let store = self.schedule_store()?;
        let exec = Arc::new(schedule::DaemonExecutor {
            agent: self.0.agent.clone(),
            storage: self.storage_arc(),
            user_id: self.user_id(),
            leader: self.leader().clone(),
        });
        #[cfg(test)]
        let clock = self.0.scheduler_clock.lock().clone();
        #[cfg(not(test))]
        let clock: Option<Arc<dyn scheduler::Clock>> = None;
        *scheduler = Some(match clock {
            Some(clock) => Scheduler::start_with_clock(store, exec, clock),
            None => Scheduler::start(store, exec),
        });
        Ok(())
    }

    /// Register the `schedule_*` tools so a bot can manage its own schedules
    /// (scoped to the calling bot; create / update / resume always ask the
    /// user). Returns how many tools were registered. [`serve`]'s embedder
    /// calls this once after [`AppState::new`].
    ///
    /// # Errors
    /// The schedule store cannot be opened.
    pub fn enable_schedule_tools(
        &self,
        limits: ScheduleLimits,
    ) -> Result<usize, scheduler::SchedulerError> {
        let host = Arc::new(schedule_tools::DaemonScheduleHost::new(
            self.schedule_store()?,
            self.storage_arc(),
            self.user_id(),
            self.0.schedule_events.clone(),
            limits,
        ));
        let registry = self.0.agent.tool_registry();
        Ok(
            builtin_tools::schedule::tools(&(host as Arc<dyn builtin_tools::ScheduleHost>))
                .into_iter()
                .filter(|t| registry.register(Arc::clone(t)))
                .count(),
        )
    }

    /// Stop the scheduler (no new firings); running firings are left to the
    /// agent loop's shutdown.
    async fn stop_scheduler(&self) {
        let scheduler = self.0.scheduler.lock().take();
        if let Some(s) = scheduler {
            s.shutdown().await;
        }
        #[cfg(test)]
        self.0.order.lock().push("scheduler");
    }

    #[cfg(test)]
    pub(crate) fn shutdown_order(&self) -> Vec<&'static str> {
        self.0.order.lock().clone()
    }

    pub(crate) fn provider_host(&self) -> Option<&ProviderHost> {
        self.0.provider_host.get()
    }
    pub(crate) fn providers_lock(&self) -> parking_lot::MutexGuard<'_, ()> {
        self.0.providers_lock.lock()
    }
    pub(crate) fn detector(&self) -> Arc<cli_provider::Detector> {
        Arc::clone(&*self.0.detector.read())
    }

    #[cfg(test)]
    pub(crate) fn set_detector(&self, detector: Arc<cli_provider::Detector>) {
        *self.0.detector.write() = detector;
    }
    /// Existing capture/input instances are registered here by the embedder.
    #[must_use]
    pub fn live_view(&self) -> &Arc<live_view::LiveView> {
        if let Ok(runtime) = tokio::runtime::Handle::try_current()
            && let Some(mut receiver) = self.0.refresh_rx.lock().take()
        {
            let weak_state = self.downgrade();
            runtime.spawn(async move {
                // The daemon finishes owed interruption work on its own.
                loop {
                    tokio::time::sleep(live_view::OWED_WORK_INTERVAL).await;
                    let Some(state) = weak_state.upgrade() else {
                        break;
                    };
                    live_view::retry_owed_work(&state).await;
                }
            });
            let weak_live = Arc::downgrade(&self.0.live_view);
            runtime.spawn(async move {
                while receiver.changed().await.is_ok() {
                    let seq = *receiver.borrow_and_update();
                    let Some(live) = weak_live.upgrade() else {
                        break;
                    };
                    for target in live.capture_targets() {
                        if let Some(target) = live.registry.get(target) {
                            let _ = target.frame_source().request_refresh(seq).await;
                        }
                    }
                }
            });
        }
        &self.0.live_view
    }

    pub(crate) fn hub(&self) -> &Arc<ControlHub> {
        &self.0.hub
    }

    pub(crate) fn agent(&self) -> &AgentLoop {
        &self.0.agent
    }
    pub(crate) fn storage(&self) -> &Storage {
        &self.0.storage
    }
    pub(crate) fn storage_arc(&self) -> Arc<Storage> {
        Arc::clone(&self.0.storage)
    }
    pub(crate) fn user_id(&self) -> UserId {
        self.0.user_id
    }
    pub(crate) fn token(&self) -> &str {
        &self.0.cfg.token
    }
    pub(crate) fn origin_allowed(&self, origin: &str) -> bool {
        self.0.cfg.allowed_origins.iter().any(|o| o == origin)
    }
    pub(crate) fn policy_load_error(&self) -> Option<String> {
        self.0.policy_load_error.lock().clone()
    }
    pub(crate) fn set_policy_load_error(&self, e: Option<String>) {
        *self.0.policy_load_error.lock() = e;
    }
    pub(crate) fn policy_lock(&self) -> parking_lot::MutexGuard<'_, ()> {
        self.0.policy_lock.lock()
    }
    pub(crate) fn data_dir(&self) -> PathBuf {
        self.0.storage.data_dir().to_path_buf()
    }
    /// Daemon lifetime signal for embedder-owned background tasks.
    #[must_use]
    pub fn shutdown_token(&self) -> CancellationToken {
        self.0.shutdown.clone()
    }
    pub(crate) fn uptime_secs(&self) -> u64 {
        self.0.started.elapsed().as_secs()
    }

    pub(crate) fn bot_of_user(&self, id: BotId) -> Result<Bot, ApiError> {
        self.storage()
            .get_bot(id)?
            .filter(|b| b.user_id == self.user_id())
            .ok_or_else(|| ApiError::not_found(format!("bot {id}")))
    }
    pub(crate) fn chat_of_user(&self, id: ChatId) -> Result<Chat, ApiError> {
        self.storage()
            .get_chat(id)?
            .filter(|c| c.user_id == self.user_id())
            .ok_or_else(|| ApiError::not_found(format!("chat {id}")))
    }
    pub(crate) fn run_of_user(&self, id: RunId) -> Result<Run, ApiError> {
        self.storage()
            .get_run(id)?
            .filter(|r| r.user_id == self.user_id())
            .ok_or_else(|| ApiError::not_found(format!("run {id}")))
    }

    /// Re-register a bot's profile in the loop after create / update (the loop
    /// caches parsed profiles).
    pub(crate) fn refresh_profile(&self, bot: &Bot) {
        if let Ok(p) = serde_json::from_value::<BotProfile>(bot.config.clone()) {
            self.0.agent.add_profile(bot.id, p);
        }
    }

    /// Shut the loop down (see [`serve`]): live runs end `Paused`, then any
    /// run a crash left `Running` / `WaitingApproval` is checkpointed too.
    async fn checkpoint_runs(&self) -> Vec<RunId> {
        #[cfg(test)]
        self.0.order.lock().push("agent");
        let mut ids = self.0.agent.shutdown(self.0.cfg.shutdown_grace).await;
        let agent = self.0.agent.clone();
        let rest = tokio::task::spawn_blocking(move || agent.recover().unwrap_or_default())
            .await
            .unwrap_or_default();
        for r in rest {
            if !ids.contains(&r) {
                ids.push(r);
            }
        }
        ids
    }
}

/// The `/v1` router, guarded by Host / Origin / bearer checks.
pub fn router(state: AppState) -> Router {
    api_routes()
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            live_view::audit_ingress,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::guard,
        ))
        .with_state(state)
}

#[allow(clippy::too_many_lines)] // The route table of the whole API.
fn api_routes() -> Router<AppState> {
    use email_routes as e;
    use live_view as v;
    use mcp_routes as m;
    use routes as r;
    use web_routes as w;
    Router::new()
        .route("/v1/host", get(host::status))
        .route("/v1/host/invite", post(host::invite))
        .route(
            "/v1/host/devices/{key}",
            axum::routing::delete(host::revoke),
        )
        .route("/v1/host/standby", post(host::standby))
        .route("/v1/host/activate", post(host::activate))
        .route("/v1/host/update", post(host::update))
        .route("/v1/health", get(r::health))
        .route("/v1/view/targets", get(v::targets))
        .route("/v1/view/sessions", post(v::create_session))
        .route(
            "/v1/view/sessions/{id}",
            axum::routing::patch(v::update_session).delete(v::delete_session),
        )
        .route("/v1/view/sessions/{id}/heartbeat", put(v::heartbeat))
        .route("/v1/view/sessions/{id}/frames", get(v::stream::frames))
        .route(
            "/v1/view/sessions/{id}/input",
            post(v::input::input).layer(axum::extract::DefaultBodyLimit::max(
                ::live_view::MAX_BATCH_BYTES,
            )),
        )
        .route("/v1/control/takeover", post(v::takeover))
        .route("/v1/control/override", post(v::local_override))
        .route("/v1/control/recover", post(v::local_recover))
        .route("/v1/view/privileges/{peer_key}", put(v::set_privileges))
        .route("/v1/openapi.json", get(openapi::serve_openapi))
        .route("/v1/bots", get(r::list_bots).post(r::create_bot))
        .route(
            "/v1/bots/{id}/grants",
            get(r::list_grants).delete(r::revoke_grant),
        )
        .route(
            "/v1/bots/{id}",
            get(r::get_bot).put(r::update_bot).delete(r::delete_bot),
        )
        .route("/v1/chats", get(r::list_chats).post(r::create_chat))
        .route(
            "/v1/chats/{id}/messages",
            get(r::list_messages).post(r::send_message),
        )
        .route("/v1/approvals", get(r::list_approvals))
        .route("/v1/approvals/{call_id}", post(r::decide_approval))
        .route("/v1/questions/{call_id}", post(r::answer_question))
        .route("/v1/runs", get(r::list_runs))
        .route("/v1/runs/{id}", get(r::get_run))
        .route("/v1/runs/{id}/cancel", post(r::cancel_run))
        .route("/v1/runs/{id}/resume", post(r::resume_run))
        .route("/v1/runs/{id}/remember", post(r::remember_run))
        .route("/v1/usage", get(r::usage))
        .route(
            "/v1/control/state",
            get(r::get_control_state).put(r::put_control_state),
        )
        .route("/v1/control/release", post(r::release_control))
        .route("/v1/control/{request_id}", post(r::decide_control))
        .route("/v1/events/latest", get(r::latest_cursor))
        .route("/v1/events/status", get(sse::status))
        .route("/v1/policy", get(r::get_policy).put(r::put_policy))
        .route("/v1/providers", get(r::list_providers))
        .route("/v1/providers/detected", get(r::detect_providers))
        .route("/v1/providers/cli/enable", post(r::enable_cli_provider))
        .route("/v1/providers/reload", post(r::reload_providers))
        .route(
            "/v1/schedules",
            get(r::list_schedules).post(r::create_schedule),
        )
        .route("/v1/schedules/parse", post(r::parse_schedule))
        .route(
            "/v1/schedules/{id}",
            get(r::get_schedule)
                .put(r::update_schedule)
                .delete(r::delete_schedule),
        )
        .route("/v1/schedules/{id}/run-now", post(r::run_schedule_now))
        .route(
            "/v1/webhooks/{id}",
            post(r::webhook).layer(axum::extract::DefaultBodyLimit::max(r::WEBHOOK_MAX_BODY)),
        )
        .route("/v1/channels", get(r::list_channels))
        .route(
            "/v1/channels/telegram/pair-code",
            post(r::telegram_pair_code),
        )
        .route(
            "/v1/mcp/servers",
            get(m::list_mcp_servers).post(m::create_mcp_server),
        )
        .route(
            "/v1/mcp/servers/{name}",
            put(m::update_mcp_server).delete(m::delete_mcp_server),
        )
        .route(
            "/v1/mcp/servers/{name}/reconnect",
            post(m::reconnect_mcp_server),
        )
        .route("/v1/mcp/servers/{name}/tools", get(m::list_mcp_tools))
        .route("/v1/mcp/catalog", get(m::mcp_catalog))
        .route(
            "/v1/email/accounts",
            get(e::list_email_accounts).post(e::create_email_account),
        )
        .route(
            "/v1/email/accounts/{name}",
            put(e::update_email_account).delete(e::delete_email_account),
        )
        .route(
            "/v1/email/accounts/{name}/test",
            post(e::test_email_account),
        )
        .route("/v1/email/test", post(e::test_email_draft))
        .route(
            "/v1/web/search",
            get(w::get_web_search).put(w::update_web_search),
        )
        .route("/v1/web/search/test", post(w::test_web_search))
        .route("/v1/secrets/{*name}", post(m::set_secret))
        .route("/v1/events", get(sse::events))
}

/// Serve the API on `listener` until `shutdown` completes.
///
/// Graceful shutdown: new connections stop, open SSE streams are closed, the
/// scheduler stops (no new firings), and then
/// [`agent_core::AgentLoop::shutdown`] runs: no new turns, model streams
/// are cancelled, running tool steps get [`DaemonConfig::shutdown_grace`] to
/// finish, and every live run ends `Paused` (unfinished steps marked
/// interrupted, dangling approvals expired), resumable with
/// `POST /v1/runs/{id}/resume` after the next start.  Runs a crash left behind
/// are checkpointed as well.  The ids of the paused runs are returned.
///
/// The caller must bind `listener` to a loopback address.
///
/// # Errors
/// I/O errors of the server, or the schedule store cannot be opened.
pub async fn serve(
    listener: TcpListener,
    state: AppState,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<Vec<RunId>> {
    state
        .leader()
        .start()
        .await
        .map_err(std::io::Error::other)?;
    let cancel = state.shutdown_token();
    let served = axum::serve(listener, router(state.clone()))
        .with_graceful_shutdown(async move {
            shutdown.await;
            cancel.cancel();
        })
        .await;
    // The scheduler first: nothing may start a run while the loop shuts down.
    state.stop_scheduler().await;
    served?;
    Ok(state.checkpoint_runs().await)
}
