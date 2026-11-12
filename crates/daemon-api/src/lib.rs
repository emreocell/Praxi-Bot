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

/// Default TCP port of `bot serve`.
pub const DEFAULT_PORT: u16 = 47821;

/// Origins of the desktop app (Tauri, per platform) accepted by default.
pub const DEFAULT_ALLOWED_ORIGINS: &[&str] = &[
    "tauri://localhost",
    "http://tauri.localhost",
    "https://tauri.localhost",
];

/// Daemon settings.
#[derive(Clone)]
pub struct DaemonConfig {
    /// Bearer token clients must present.
    pub token: String,
    /// Per-launch, memory-only management bearer. Never load this from the token file.
    pub management_token: Option<String>,
    /// The embedder verified its shell/MCP/CLI child environment wiring.
    pub children_scrubbed: bool,
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

impl std::fmt::Debug for DaemonConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonConfig")
            .field("management_enabled", &self.management_token.is_some())
            .field("allowed_origins", &self.allowed_origins)
            .finish_non_exhaustive()
    }
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
            management_token: None,
            children_scrubbed: false,
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
pub trait ChannelControl: Send + Sync {
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
    provider_host: OnceLock<ProviderHost>,
    /// MCP server management (see [`AppState::set_mcp`]).
    mcp: OnceLock<Arc<dyn McpControl>>,
    /// E-mail account management (see [`AppState::set_email`]).
    email: OnceLock<Arc<dyn EmailControl>>,
    /// Bounds concurrent "test connection" calls (see `email_routes`).
    email_tests: Arc<tokio::sync::Semaphore>,
    /// Write-only secret sink of `POST /v1/secrets/{name}`.
    secrets: OnceLock<Arc<dyn secret_store::SecretStore>>,
    /// `schedules.db`, opened on first use.
    schedules: parking_lot::Mutex<Option<Arc<ScheduleStore>>>,
    /// Serialises `PUT /v1/policy` (read-modify-write of memory + `policy.json`).
    policy_lock: parking_lot::Mutex<()>,
    /// Why `policy.json` could not be loaded (locked deny-all mode), if so.
    policy_load_error: parking_lot::Mutex<Option<String>>,
    /// CLI provider detector (`GET /v1/providers/detected`).
    detector: parking_lot::RwLock<Arc<cli_provider::Detector>>,
    /// The running scheduler (see [`AppState::start_scheduler`]).
    scheduler: OnceLock<Scheduler>,
    /// Shutdown steps in the order they ran (tests only).
    #[cfg(test)]
    order: parking_lot::Mutex<Vec<&'static str>>,
}

/// Shared handler state (cheap to clone).
#[derive(Clone)]
pub struct AppState(Arc<Inner>);

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
        Ok(Self(Arc::new(Inner {
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
            provider_host: OnceLock::new(),
            mcp: OnceLock::new(),
            email: OnceLock::new(),
            email_tests: Arc::new(tokio::sync::Semaphore::new(EMAIL_TESTS_AT_ONCE)),
            secrets: OnceLock::new(),
            schedules: parking_lot::Mutex::new(None),
            policy_lock: parking_lot::Mutex::new(()),
            policy_load_error: parking_lot::Mutex::new(policy_load_error),
            detector: parking_lot::RwLock::new(Arc::new(cli_provider::Detector::new())),
            scheduler: OnceLock::new(),
            #[cfg(test)]
            order: parking_lot::Mutex::new(Vec::new()),
        })))
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

    pub(crate) fn secret_store(&self) -> Option<Arc<dyn secret_store::SecretStore>> {
        self.0.secrets.get().cloned()
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
    pub(crate) fn scheduler(&self) -> Result<&Scheduler, ApiError> {
        self.0
            .scheduler
            .get()
            .ok_or_else(|| ApiError::unavailable("the scheduler is not running"))
    }

    /// Open the schedule store and start the scheduler with an executor that
    /// runs payloads through the agent loop (initiator `Schedule`).  [`serve`]
    /// calls this; a second call is a no-op.
    ///
    /// # Errors
    /// The schedule store cannot be opened.
    pub fn start_scheduler(&self) -> Result<(), scheduler::SchedulerError> {
        if self.0.scheduler.get().is_some() {
            return Ok(());
        }
        let store = self.schedule_store()?;
        let exec = Arc::new(schedule::DaemonExecutor {
            agent: self.0.agent.clone(),
            storage: self.storage_arc(),
            user_id: self.user_id(),
        });
        let _ = self.0.scheduler.set(Scheduler::start(store, exec));
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
        if let Some(s) = self.0.scheduler.get() {
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
    pub(crate) fn detector(&self) -> Arc<cli_provider::Detector> {
        Arc::clone(&*self.0.detector.read())
    }

    #[cfg(test)]
    pub(crate) fn set_detector(&self, detector: Arc<cli_provider::Detector>) {
        *self.0.detector.write() = detector;
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
    pub(crate) fn management_token(&self) -> Option<&str> {
        self.0.cfg.management_token.as_deref()
    }
    pub(crate) fn children_scrubbed(&self) -> bool {
        self.0.cfg.children_scrubbed
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
    pub(crate) fn shutdown_token(&self) -> CancellationToken {
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
            auth::guard,
        ))
        .with_state(state)
}

// This declaration constructs BOTH the router and its authority registry. Adding a
// method without a privilege is a compile error. Unknown routes fail closed.
macro_rules! classified_routes {
    ($($path:literal => [$($method:ident: $privilege:ident => $handler:path),+]
        $(; limit = $limit:expr)?),+ $(,)?) => {
        pub(crate) const ROUTE_PRIVILEGES: &[(&str, &str, auth::Privilege)] = &[
            $($(($path, stringify!($method), auth::Privilege::$privilege),)+)+
        ];
        fn api_routes() -> Router<AppState> {
            use email_routes as e;
            use mcp_routes as m;
            use routes as r;
            Router::new()$(.route($path,
                axum::routing::MethodRouter::new()
                    $(.on(axum::routing::MethodFilter::$method, $handler))+
                    $(.layer(axum::extract::DefaultBodyLimit::max($limit)))?
            ))+
        }
    };
}

classified_routes! {
    "/v1/health" => [GET: Public => r::health],
    "/v1/isolation" => [GET: Cli => r::isolation],
    "/v1/openapi.json" => [GET: Cli => openapi::serve_openapi],
    "/v1/bots" => [GET: Cli => r::list_bots, POST: Management => r::create_bot],
    "/v1/bots/{id}/grants" => [GET: Cli => r::list_grants, DELETE: Management => r::revoke_grant],
    "/v1/bots/{id}" => [GET: Cli => r::get_bot, PUT: Management => r::update_bot, DELETE: Management => r::delete_bot],
    "/v1/chats" => [GET: Cli => r::list_chats, POST: Cli => r::create_chat],
    "/v1/chats/{id}/messages" => [GET: Cli => r::list_messages, POST: Cli => r::send_message],
    "/v1/approvals" => [GET: Cli => r::list_approvals],
    "/v1/approvals/{call_id}" => [POST: Management => r::decide_approval],
    "/v1/questions/{call_id}" => [POST: Management => r::answer_question],
    "/v1/runs" => [GET: Cli => r::list_runs],
    "/v1/runs/{id}" => [GET: Cli => r::get_run],
    "/v1/runs/{id}/cancel" => [POST: Cli => r::cancel_run],
    "/v1/runs/{id}/resume" => [POST: Cli => r::resume_run],
    "/v1/runs/{id}/remember" => [POST: Management => r::remember_run],
    "/v1/usage" => [GET: Cli => r::usage],
    "/v1/control/state" => [GET: Management => r::get_control_state, PUT: Management => r::put_control_state],
    "/v1/control/release" => [POST: Management => r::release_control],
    "/v1/control/{request_id}" => [POST: Management => r::decide_control],
    "/v1/events/latest" => [GET: Cli => r::latest_cursor],
    "/v1/events/status" => [GET: Cli => sse::status],
    "/v1/policy" => [GET: Cli => r::get_policy, PUT: Management => r::put_policy],
    "/v1/providers" => [GET: Cli => r::list_providers],
    "/v1/providers/detected" => [GET: Management => r::detect_providers],
    "/v1/providers/reload" => [POST: Management => r::reload_providers],
    "/v1/schedules" => [GET: Cli => r::list_schedules, POST: Management => r::create_schedule],
    "/v1/schedules/parse" => [POST: Cli => r::parse_schedule],
    "/v1/schedules/{id}" => [GET: Cli => r::get_schedule, PUT: Management => r::update_schedule, DELETE: Management => r::delete_schedule],
    "/v1/schedules/{id}/run-now" => [POST: Management => r::run_schedule_now],
    "/v1/webhooks/{id}" => [POST: Public => r::webhook]; limit = r::WEBHOOK_MAX_BODY,
    "/v1/channels" => [GET: Cli => r::list_channels],
    "/v1/channels/telegram/pair-code" => [POST: Management => r::telegram_pair_code],
    "/v1/mcp/servers" => [GET: Cli => m::list_mcp_servers, POST: Management => m::create_mcp_server],
    "/v1/mcp/servers/{name}" => [PUT: Management => m::update_mcp_server, DELETE: Management => m::delete_mcp_server],
    "/v1/mcp/servers/{name}/reconnect" => [POST: Management => m::reconnect_mcp_server],
    "/v1/mcp/servers/{name}/tools" => [GET: Cli => m::list_mcp_tools],
    "/v1/mcp/catalog" => [GET: Cli => m::mcp_catalog],
    "/v1/email/accounts" => [GET: Management => e::list_email_accounts, POST: Management => e::create_email_account],
    "/v1/email/accounts/{name}" => [PUT: Management => e::update_email_account, DELETE: Management => e::delete_email_account],
    "/v1/email/accounts/{name}/test" => [POST: Management => e::test_email_account],
    "/v1/email/test" => [POST: Management => e::test_email_draft],
    "/v1/secrets/{*name}" => [POST: Management => m::set_secret],
    "/v1/events" => [GET: Cli => sse::events],
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
    state.start_scheduler().map_err(std::io::Error::other)?;
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
