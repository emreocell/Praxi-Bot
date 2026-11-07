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
pub mod error;
#[cfg(any(test, feature = "fake-provider"))]
pub mod fake;
pub mod openapi;
mod routes;
mod schedule;
mod sse;

#[cfg(test)]
mod tests;

use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use agent_core::{AgentLoop, BotProfile, Providers};
use axum::Router;
use axum::routing::{get, post};
use protocol::{BotId, ChatId, RunId, UserId};
use provider_catalog::Catalog;
use scheduler::{ScheduleStore, Scheduler};
use storage::{Bot, Chat, Run, Storage, User, now_ms};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

pub use auth::{TOKEN_FILE_NAME, TokenInfo, load_or_create_token, read_token, token_path};
pub use control::{ControlConfig, ControlHub};
pub use error::ApiError;
pub use openapi::openapi_json;

/// Default TCP port of `bot serve`.
pub const DEFAULT_PORT: u16 = 47821;

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
}

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
    shutdown: CancellationToken,
    hub: Arc<ControlHub>,
    provider_host: OnceLock<ProviderHost>,
    /// `schedules.db`, opened on first use.
    schedules: parking_lot::Mutex<Option<Arc<ScheduleStore>>>,
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
        let hub = Arc::new(ControlHub::new(cfg.control));
        agent.set_control_gate(hub.clone());
        if let Some(p) = routes::load_policy(storage.data_dir()) {
            agent.set_global_policy(p);
        }
        Ok(Self(Arc::new(Inner {
            schedule_events: tokio::sync::broadcast::channel(64).0,
            channels: parking_lot::Mutex::new(Vec::new()),
            agent,
            storage,
            user_id,
            cfg,
            started: Instant::now(),
            shutdown: CancellationToken::new(),
            hub,
            provider_host: OnceLock::new(),
            schedules: parking_lot::Mutex::new(None),
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
    use routes as r;
    Router::new()
        .route("/v1/health", get(r::health))
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
        .route("/v1/control/{request_id}", post(r::decide_control))
        .route("/v1/policy", get(r::get_policy).put(r::put_policy))
        .route("/v1/providers", get(r::list_providers))
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
        .route("/v1/events", get(sse::events))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::guard,
        ))
        .with_state(state)
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
