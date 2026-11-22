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

mod attachment_routes;
pub mod auth;
mod bot_engine;
mod bot_tools;
pub mod changes;
mod chat_history;
mod cli_engines;
mod cli_login;
pub mod components;
pub mod composio;
mod composio_routes;
pub mod control;
pub mod doctor;
mod doctor_routes;
pub mod dto;
pub mod email;
mod email_routes;
pub mod error;
#[cfg(any(test, feature = "fake-provider"))]
pub mod fake;
mod file_access;
mod fill_routes;
pub mod host;
pub mod host_admin;
pub mod host_copy;
pub mod host_import;
pub mod host_links;
pub mod host_setup;
mod knowledge;
pub mod knowledge_runtime;
mod knowledge_settings;
pub mod live_view;
mod local_models;
pub mod mcp;
mod mcp_routes;
pub mod mcp_server;
pub mod memory;
pub mod openapi;
pub mod owner;
pub mod pc_access;
pub mod peer;
pub mod policy;
pub mod profile_share;
mod profile_share_routes;
mod provider_models;
mod secret_request_routes;
pub mod trusted_folders;
pub use peer::{PeerIdentity, PeerRole, peer_router};
mod routes;
mod schedule;
mod schedule_tools;
pub mod skill_hub;
pub mod skill_management;
pub mod skills;
mod spend_routes;
mod sse;
mod stop;
pub mod terminal;
pub mod usage;
pub mod voice;
mod voice_routes;
pub mod web;
mod web_routes;
mod workflows;

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

pub use auth::{
    OWNER_KEY_FILE_NAME, TOKEN_FILE_NAME, TokenInfo, load_or_create_owner_key,
    load_or_create_token, read_owner_key, read_token, token_path,
};
pub use components::{ComponentControl, ComponentError};
pub use composio::{ComposioControl, ComposioControlError};
pub use control::{ControlConfig, ControlHub};
pub use doctor_routes::{DoctorHost, DoctorReport, RepairResult};
pub use email::{EmailControl, EmailControlError};
pub use error::ApiError;
pub use mcp::{McpControl, McpControlError};
pub use openapi::openapi_json;
pub use policy::{PolicyLoadError, load_policy, validate_policy};
pub use schedule_tools::{ScheduleLimits, load_schedule_limits};
pub use skills::SkillAssignments;
pub use voice::{VoiceControl, VoiceControlError};
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
    /// Safe mode, decided once at process start by whoever launches the daemon
    /// (`bot serve --safe`, `BOT_SAFE_MODE`): no scheduler and no channels start.
    /// No route can change it; leaving it means restarting the process.
    pub safe_mode: bool,
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
            safe_mode: false,
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
    host_links: OnceLock<Arc<dyn host_links::HostLinks>>,
    host_setup: OnceLock<Arc<dyn host_setup::HostSetup>>,
    knowledge_runtime: OnceLock<Arc<knowledge_runtime::KnowledgeRuntime>>,
    /// MCP server management (see [`AppState::set_mcp`]).
    mcp: OnceLock<Arc<dyn McpControl>>,
    mcp_server: parking_lot::Mutex<mcp_server::McpServerBook>,
    /// Composio app connections (see [`AppState::set_composio`]).
    composio: OnceLock<Arc<dyn ComposioControl>>,
    /// Missing-component installer (see [`AppState::set_components`]).
    components: OnceLock<Arc<dyn ComponentControl>>,
    /// E-mail account management (see [`AppState::set_email`]).
    email: OnceLock<Arc<dyn EmailControl>>,
    /// Bounds concurrent "test connection" calls (see `email_routes`).
    email_tests: Arc<tokio::sync::Semaphore>,
    /// Web search settings management (see [`AppState::set_web`]).
    web: OnceLock<Arc<dyn WebControl>>,
    /// Dictation (speech-to-text) management (see [`AppState::set_voice`]).
    voice: OnceLock<Arc<dyn VoiceControl>>,
    /// Skill catalog index (see [`AppState::set_skills`]).
    skills: OnceLock<::skills::SkillIndex>,
    /// Serialises owner catalog mutations with per-bot assignment writes.
    skill_mutations: parking_lot::Mutex<()>,
    /// Bounds concurrent web search probes (see `web_routes`).
    web_tests: Arc<tokio::sync::Semaphore>,
    /// Write-only secret sink of `POST /v1/secrets/{name}`.
    secrets: OnceLock<Arc<dyn secret_store::SecretStore>>,
    owner_key: OnceLock<String>,
    /// Owner-stored fill secrets (see [`AppState::set_fill_secrets`]).
    fill_secrets: OnceLock<Arc<secret_store::FillSecrets>>,
    /// `schedules.db`, opened on first use.
    schedules: parking_lot::Mutex<Option<Arc<ScheduleStore>>>,
    /// `memory.db`, opened on first use when the loop was not given a store.
    memory: parking_lot::Mutex<Option<Arc<memory_store::MemoryStore>>>,
    /// Serialises `PUT /v1/policy` (read-modify-write of memory + `policy.json`).
    policy_lock: parking_lot::Mutex<()>,
    /// Target sets of `POST /v1/runs/stop` (see `stop`).
    stops: parking_lot::Mutex<stop::StopBook>,
    #[cfg(test)]
    stop_seam: stop::Seam,
    restore_locks: parking_lot::Mutex<std::collections::HashSet<RunId>>,
    #[cfg(test)]
    restore_fail_at: parking_lot::Mutex<Option<usize>>,
    #[cfg(test)]
    restore_pause: parking_lot::Mutex<Option<changes::TestRestorePause>>,
    /// Serialises `providers.toml` read-modify-write operations (`POST /v1/providers/cli/enable`).
    providers_lock: parking_lot::Mutex<()>,
    /// Why `policy.json` could not be loaded (locked deny-all mode), if so.
    policy_load_error: parking_lot::Mutex<Option<String>>,
    /// CLI provider detector (`GET /v1/providers/detected`).
    detector: parking_lot::RwLock<Arc<cli_provider::Detector>>,
    cli_installs: cli_engines::InstallBook,
    terminals: terminal::Terminals,
    cli_logins: cli_login::LoginBook,
    /// Live model lists of the coding CLIs (disk-cached, refreshed in the background).
    cli_models: Arc<cli_provider::models::ModelCache>,
    /// The running scheduler (see [`AppState::start_scheduler`]).
    scheduler: parking_lot::Mutex<Option<Scheduler>>,
    leader: OnceLock<host::LeaderControl>,
    /// Embedder additions to `/v1/doctor` (see [`AppState::set_doctor_host`]).
    doctor_host: OnceLock<Arc<dyn DoctorHost>>,
    /// One diagnostics run at a time (`/v1/doctor*`).
    doctor_runs: Arc<tokio::sync::Semaphore>,
    /// Runs inside the `/v1/doctor` CLI detection future before detecting
    /// (tests: simulate synchronous file-system I/O that blocks).
    #[cfg(test)]
    pub(crate) doctor_detect_hook: parking_lot::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
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
    #[allow(clippy::too_many_lines)] // one ordered construction of the shared state
    pub fn with_view_registry(
        agent: AgentLoop,
        storage: Arc<Storage>,
        cfg: DaemonConfig,
        registry: Arc<::live_view::TargetRegistry>,
    ) -> Result<Self, storage::StorageError> {
        // `<data>/runtimes` (the CLIs Praxi installed) is a protected location for shell commands.
        permission_engine::set_data_dir(storage.data_dir());
        memory_store::knowledge::brain::cleanup_stale_staging(storage.data_dir())
            .map_err(std::io::Error::other)?;
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
        match spend_routes::load_limit(storage.data_dir()) {
            Ok(Some(limit)) => agent.set_spend_limit(Some(limit)),
            Ok(None) => {}
            Err(e) => {
                // Fail closed: a damaged limit file must not silently turn the
                // limit off. Paid calls stay blocked until the owner sets it again.
                eprintln!(
                    "daemon-api: WARNING: {e}; paid model calls are blocked until the monthly spending limit is set again on the Usage page"
                );
                agent.set_spend_limit(Some(spend_routes::FAIL_CLOSED_LIMIT_USD));
            }
        }
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
        // Praxi's own Node.js runtime (host installs) is found from here on.
        cli_provider::node_runtime::register_data_dir(storage.data_dir());
        let cli_models = cli_provider::models::ModelCache::load(Some(
            storage.data_dir().join("cache").join("cli-models.json"),
        ));
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
            host_links: OnceLock::new(),
            host_setup: OnceLock::new(),
            knowledge_runtime: OnceLock::new(),
            mcp: OnceLock::new(),
            mcp_server: parking_lot::Mutex::new(mcp_server::McpServerBook::default()),
            composio: OnceLock::new(),
            components: OnceLock::new(),
            email: OnceLock::new(),
            email_tests: Arc::new(tokio::sync::Semaphore::new(EMAIL_TESTS_AT_ONCE)),
            web: OnceLock::new(),
            voice: OnceLock::new(),
            skills: OnceLock::new(),
            skill_mutations: parking_lot::Mutex::new(()),
            web_tests: Arc::new(tokio::sync::Semaphore::new(EMAIL_TESTS_AT_ONCE)),
            secrets: OnceLock::new(),
            owner_key: OnceLock::new(),
            fill_secrets: OnceLock::new(),
            schedules: parking_lot::Mutex::new(None),
            memory: parking_lot::Mutex::new(None),
            policy_lock: parking_lot::Mutex::new(()),
            stops: parking_lot::Mutex::new(stop::StopBook::default()),
            #[cfg(test)]
            stop_seam: stop::Seam::default(),
            restore_locks: parking_lot::Mutex::new(std::collections::HashSet::new()),
            #[cfg(test)]
            restore_fail_at: parking_lot::Mutex::new(None),
            #[cfg(test)]
            restore_pause: parking_lot::Mutex::new(None),
            providers_lock: parking_lot::Mutex::new(()),
            policy_load_error: parking_lot::Mutex::new(policy_load_error),
            detector: parking_lot::RwLock::new(Arc::new(cli_provider::Detector::new())),
            cli_installs: cli_engines::InstallBook::default(),
            terminals: terminal::Terminals::default(),
            cli_logins: cli_login::LoginBook::default(),
            cli_models,
            scheduler: parking_lot::Mutex::new(None),
            leader: OnceLock::new(),
            doctor_host: OnceLock::new(),
            doctor_runs: Arc::new(tokio::sync::Semaphore::new(1)),
            #[cfg(test)]
            doctor_detect_hook: parking_lot::Mutex::new(None),
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
        state.install_unset_file_access();
        knowledge_runtime::install(&state);
        Ok(state)
    }

    /// An always-on host does not hand its account's files to bots nobody
    /// configured: an unset file access mode means no access there.
    fn install_unset_file_access(&self) {
        let weak = self.downgrade();
        self.agent().set_unset_file_access_denied(move || {
            weak.upgrade()
                .is_some_and(|s| s.leader().config().mode == host::HostMode::Host)
        });
    }

    /// Let `/v1/providers` list the catalog and reload the providers (call
    /// once, right after [`AppState::new`]; a second call is ignored).
    pub fn set_provider_host(&self, host: ProviderHost) {
        let _ = self.0.provider_host.set(host);
    }

    /// Enable `/v1/hosts/*` (this computer's links to paired hosts). Call
    /// once, before [`serve`]; a second call is ignored.
    pub fn set_host_links(&self, links: Arc<dyn host_links::HostLinks>) {
        let _ = self.0.host_links.set(links);
    }

    /// Enable `/v1/hosts/setup/*` (the desktop "Add server" wizard). Call
    /// once, before [`serve`]; a second call is ignored.
    pub fn set_host_setup(&self, setup: Arc<dyn host_setup::HostSetup>) {
        let _ = self.0.host_setup.set(setup);
    }

    pub(crate) fn host_setup(&self) -> Option<Arc<dyn host_setup::HostSetup>> {
        self.0.host_setup.get().cloned()
    }

    pub(crate) fn host_links(&self) -> Option<Arc<dyn host_links::HostLinks>> {
        self.0.host_links.get().cloned()
    }

    /// Enable `/v1/mcp/*` with the embedder MCP manager (call once, before
    /// [`serve`]; a second call is ignored).
    pub fn set_mcp(&self, mcp: Arc<dyn McpControl>) {
        let _ = self.0.mcp.set(mcp);
    }

    /// Enable `/v1/composio/*` with the embedder Composio service (call once,
    /// before serving).
    pub fn set_composio(&self, composio: Arc<dyn ComposioControl>) {
        let _ = self.0.composio.set(composio);
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

    /// Enable `/v1/skills` and `/v1/bots/{id}/skills` over `index` (call once,
    /// before [`serve`]; a second call is ignored).
    ///
    /// # Errors
    /// `index` has no per-bot assignment source attached
    /// ([`::skills::SkillIndex::with_assignments`]): such an index would show
    /// every bot every shared and bundled skill, so it is refused instead of
    /// being served. Callers must fail startup on this.
    pub fn set_skills(&self, index: ::skills::SkillIndex) -> Result<(), &'static str> {
        if !index.has_assignments() {
            return Err("skill index has no per-bot assignment source attached");
        }
        let _ = self.0.skills.set(index);
        Ok(())
    }

    pub(crate) fn skills_index(&self) -> Result<::skills::SkillIndex, ApiError> {
        self.0
            .skills
            .get()
            .cloned()
            .ok_or_else(|| ApiError::unavailable("skills are not available in this daemon"))
    }

    /// Enable `POST /v1/secrets/{name}` (write-only) over `store` (call once;
    /// a second call is ignored).
    pub fn set_secret_store(&self, store: Arc<dyn secret_store::SecretStore>) {
        let _ = self.0.secrets.set(store);
    }

    /// Add desktop / browser facts and repairs to `/v1/doctor` (call once;
    /// a second call is ignored).
    pub fn set_doctor_host(&self, host: Arc<dyn DoctorHost>) {
        let _ = self.0.doctor_host.set(host);
    }

    #[cfg(test)]
    pub(crate) fn set_doctor_detect_hook(&self, hook: Arc<dyn Fn() + Send + Sync>) {
        *self.0.doctor_detect_hook.lock() = Some(hook);
    }

    pub(crate) fn doctor_host(&self) -> Option<Arc<dyn DoctorHost>> {
        self.0.doctor_host.get().cloned()
    }

    /// The diagnostics slot, or `429` while another run is in progress.
    pub(crate) fn doctor_permit(
        &self,
    ) -> Result<tokio::sync::OwnedSemaphorePermit, error::ApiError> {
        Arc::clone(&self.0.doctor_runs)
            .try_acquire_owned()
            .map_err(|_| {
                error::ApiError::new(
                    axum::http::StatusCode::TOO_MANY_REQUESTS,
                    "too_many_requests",
                    "diagnostics are already running; try again in a moment",
                )
            })
    }

    pub(crate) fn mcp(&self) -> Option<Arc<dyn McpControl>> {
        self.0.mcp.get().cloned()
    }

    /// Enable `/v1/components/*` (call once, at startup).
    pub fn set_components(&self, c: Arc<dyn ComponentControl>) {
        let _ = self.0.components.set(c);
    }

    pub(crate) fn components(&self) -> Option<Arc<dyn ComponentControl>> {
        self.0.components.get().cloned()
    }

    pub(crate) fn composio(&self) -> Option<Arc<dyn ComposioControl>> {
        self.0.composio.get().cloned()
    }

    pub(crate) fn email(&self) -> Option<Arc<dyn EmailControl>> {
        self.0.email.get().cloned()
    }

    /// Enable `/v1/voice*` with the embedder dictation manager (call once, before [`serve`]).
    pub fn set_voice(&self, voice: Arc<dyn VoiceControl>) {
        let _ = self.0.voice.set(voice);
    }

    pub(crate) fn voice(&self) -> Option<Arc<dyn VoiceControl>> {
        self.0.voice.get().cloned()
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

    /// Enable `/v1/fill-secrets*` over `secrets` (call once; a second call is
    /// ignored). Local-owner only; peers are refused by the peer router.
    pub fn set_fill_secrets(&self, secrets: Arc<secret_store::FillSecrets>) {
        let _ = self.0.fill_secrets.set(secrets);
    }

    pub(crate) fn fill_secrets(&self) -> Option<Arc<secret_store::FillSecrets>> {
        self.0.fill_secrets.get().cloned()
    }

    pub(crate) fn secret_store(&self) -> Option<Arc<dyn secret_store::SecretStore>> {
        self.0.secrets.get().cloned()
    }

    /// Whether this process was started in safe mode (fixed for its lifetime).
    #[must_use]
    pub fn safe_mode(&self) -> bool {
        self.0.cfg.safe_mode
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
        // Safe mode pauses (never deletes) everything that acts by itself.
        if self.0.cfg.safe_mode
            || self.0.shutdown.is_cancelled()
            || !self.leader().components_allowed()
        {
            return Ok(());
        }
        self.start_scheduler()?;
        for channel in self.0.channels.lock().iter() {
            channel.start();
        }
        Ok(())
    }

    pub(crate) async fn stop_components(&self) {
        // Channels first, so no message arrives while the scheduler winds down.
        let channels = self.0.channels.lock().clone();
        for channel in channels {
            channel.stop().await;
        }
        self.stop_scheduler().await;
    }

    /// Register a messaging channel for `/v1/channels` before automatic
    /// components start.
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

    /// The personal memory store (`<data dir>/memory.db`), shared with the agent
    /// loop when configured or opened with the local hash embedder on first use.
    pub(crate) fn memory_store(&self) -> Result<Arc<memory_store::MemoryStore>, ApiError> {
        if let Some(store) = self.0.agent.memory_store() {
            return Ok(store);
        }
        let mut g = self.0.memory.lock();
        if let Some(s) = g.as_ref() {
            return Ok(Arc::clone(s));
        }
        let store = Arc::new(
            memory_store::MemoryStore::open(
                self.data_dir(),
                Arc::new(memory_store::HashEmbedder::new(384)),
            )
            .map_err(|_| ApiError::internal("memory store unavailable"))?,
        );
        *g = Some(Arc::clone(&store));
        Ok(store)
    }

    /// The running scheduler, or `503` when the daemon did not start one.
    pub(crate) fn scheduler(&self) -> Result<Scheduler, ApiError> {
        if self.leader().is_starting() {
            return Err(ApiError::starting());
        }
        if !self.leader().components_allowed() {
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
        if self.0.cfg.safe_mode || !self.leader().components_allowed() {
            return Ok(());
        }
        let mut scheduler = self.0.scheduler.lock();
        if !self.leader().components_allowed() {
            return Ok(());
        }
        if scheduler.is_some() {
            return Ok(());
        }
        let store = self.schedule_store()?;
        let exec = Arc::new(schedule::DaemonExecutor {
            agent: self.0.agent.clone(),
            storage: self.storage_arc(),
            store: Arc::clone(&store),
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
        let host = Arc::new(
            schedule_tools::DaemonScheduleHost::new(
                self.schedule_store()?,
                self.storage_arc(),
                self.user_id(),
                self.0.schedule_events.clone(),
                limits,
            )
            .with_agent(self.0.agent.clone()),
        );
        let registry = self.0.agent.tool_registry();
        Ok(
            builtin_tools::schedule::tools(&(host as Arc<dyn builtin_tools::ScheduleHost>))
                .into_iter()
                .filter(|t| registry.register(Arc::clone(t)))
                .count(),
        )
    }

    /// Register the `bot_*` tools so a bot can create and manage other bots
    /// (bounded by its own authority; create / update / delete always ask the
    /// user and are visible only to bots with `tools.manage_bots`). Returns
    /// how many tools were registered. [`serve`]'s embedder calls this once
    /// after [`AppState::new`].
    #[must_use]
    pub fn enable_bot_tools(&self) -> usize {
        let host = Arc::new(bot_tools::DaemonBotHost::new(self));
        let registry = self.0.agent.tool_registry();
        builtin_tools::bots::tools(&(host as Arc<dyn builtin_tools::BotHost>))
            .into_iter()
            .filter(|t| registry.register(Arc::clone(t)))
            .count()
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
    pub(crate) fn cli_models(&self) -> Arc<cli_provider::models::ModelCache> {
        Arc::clone(&self.0.cli_models)
    }
    pub(crate) fn detector(&self) -> Arc<cli_provider::Detector> {
        Arc::clone(&*self.0.detector.read())
    }

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

    /// The control hub (pause / human-driving latch), for embedders that guard
    /// actions of their own with it.
    #[must_use]
    pub fn control_hub(&self) -> Arc<ControlHub> {
        Arc::clone(&self.0.hub)
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
    /// A writer of secret-free audit rows for the embedder (the Composio tools
    /// and connect flow): `(action, detail)`, actor `composio`.
    pub fn audit_writer(
        &self,
    ) -> impl Fn(&str, &serde_json::Value) -> Result<(), String> + Send + Sync + 'static {
        self.audit_writer_as("composio")
    }

    /// [`audit_writer`](Self::audit_writer) under another actor name.
    pub fn audit_writer_as(
        &self,
        actor: &'static str,
    ) -> impl Fn(&str, &serde_json::Value) -> Result<(), String> + Send + Sync + 'static {
        let storage = Arc::clone(&self.0.storage);
        let user = self.0.user_id;
        move |action, detail| {
            storage
                .append_audit(user, None, actor, action, detail)
                .map(|_| ())
                .map_err(|e| e.to_string())
        }
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
    #[cfg(test)]
    pub(crate) fn stop_seam(&self) -> &stop::Seam {
        &self.0.stop_seam
    }
    pub(crate) fn stops(&self) -> &parking_lot::Mutex<stop::StopBook> {
        &self.0.stops
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
            owner::owner_gate,
        ))
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
    use composio_routes as c;
    use email_routes as e;
    use fill_routes as f;
    use live_view as v;
    use mcp_routes as m;
    use routes as r;
    use voice_routes as vo;
    use web_routes as w;
    Router::new()
        .merge(workflows::router())
        .route("/v1/mcp-server/tokens", post(mcp_server::create_token))
        .route(
            "/v1/mcp-server/tokens/{id}",
            axum::routing::delete(mcp_server::revoke_token),
        )
        .route("/v1/mcp-server/tools", get(mcp_server::list_tools))
        .route("/v1/mcp-server/call", post(mcp_server::call_tool))
        .route("/v1/host", get(host::status))
        .route("/v1/hosts", get(host_links::list))
        .route("/v1/hosts/pair", post(host_links::pair))
        .route("/v1/hosts/{id}", axum::routing::delete(host_links::forget))
        .route("/v1/hosts/{id}/terminal", post(terminal::host_open))
        .route(
            "/v1/hosts/{id}/terminal/{tid}",
            axum::routing::delete(terminal::host_close),
        )
        .route(
            "/v1/hosts/{id}/terminal/{tid}/output",
            get(terminal::host_output),
        )
        .route(
            "/v1/hosts/{id}/terminal/{tid}/input",
            post(terminal::host_input),
        )
        .route(
            "/v1/hosts/{id}/terminal/{tid}/resize",
            post(terminal::host_resize),
        )
        .route(
            "/v1/hosts/computer-requests",
            get(host_links::computer_requests),
        )
        .route(
            "/v1/hosts/computer-requests/{rid}",
            post(host_links::decide_computer_request),
        )
        .route(
            "/v1/hosts/{id}/computer-access",
            axum::routing::put(host_links::set_computer_access),
        )
        .route("/v1/hosts/home", get(host_copy::home))
        .route(
            "/v1/trusted-folders",
            get(trusted_folders::list)
                .post(trusted_folders::add)
                .delete(trusted_folders::remove),
        )
        .route(
            "/v1/hosts/{id}/copy-setup",
            get(host_copy::plan).post(host_copy::copy),
        )
        .route("/v1/hosts/setup", post(host_setup::start))
        .route("/v1/hosts/setup/ssh-keys", get(host_setup::ssh_keys))
        .route("/v1/hosts/setup/{job}", get(host_setup::get))
        .route("/v1/hosts/setup/{job}/host-key", post(host_setup::host_key))
        .route("/v1/hosts/setup/{job}/cancel", post(host_setup::cancel))
        .route(
            "/v1/hosts/{id}/connect",
            post(host_links::connect).delete(host_links::disconnect),
        )
        .route("/v1/host/invite", post(host::invite))
        .route(
            "/v1/host/devices/{key}",
            axum::routing::delete(host::revoke),
        )
        .route("/v1/host/devices/{key}/admin", put(host::set_admin))
        .route("/v1/host/standby", post(host::standby))
        .route("/v1/host/activate", post(host::activate))
        .route("/v1/host/update", post(host::update))
        .route("/v1/health", get(r::health))
        .route("/v1/view/targets", get(v::targets))
        .route("/v1/view/availability", get(v::availability))
        .route("/v1/peer/link", get(peer::link))
        .route("/v1/view/status", get(v::status))
        .route("/v1/view/targets/{id}/thumbnail", get(v::thumbnail))
        .route("/v1/view/sessions", post(v::create_session))
        .route(
            "/v1/view/sessions/{id}",
            axum::routing::patch(v::update_session).delete(v::delete_session),
        )
        .route("/v1/view/sessions/{id}/heartbeat", put(v::heartbeat))
        .route("/v1/view/sessions/{id}/ack", post(v::ack_frame))
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
            "/v1/profile-share/templates",
            get(profile_share_routes::profile_templates),
        )
        .route(
            "/v1/profile-share/export",
            post(profile_share_routes::export_profile),
        )
        .route(
            "/v1/profile-share/preview",
            post(profile_share_routes::preview_profile),
        )
        .route(
            "/v1/profile-share/import",
            post(profile_share_routes::import_profile),
        )
        .route(
            "/v1/bots/{bot}/files",
            get(file_access::get_files).put(file_access::put_files),
        )
        .route(
            "/v1/bots/{bot}/knowledge/sources",
            get(knowledge::list_sources)
                .post(knowledge::create_source)
                .layer(axum::extract::DefaultBodyLimit::max(
                    knowledge::MAX_SOURCE_REQUEST_BYTES,
                )),
        )
        .route(
            "/v1/bots/{bot}/knowledge/sources/{source}",
            put(knowledge::update_source).delete(knowledge::delete_source),
        )
        .route(
            "/v1/bots/{bot}/knowledge/sources/{source}/reindex",
            post(knowledge::reindex_source),
        )
        .route(
            "/v1/bots/{bot}/knowledge/sources/{source}/embed",
            post(knowledge::embed_source),
        )
        .route(
            "/v1/bots/{bot}/knowledge/settings",
            get(knowledge_settings::get_settings).put(knowledge_settings::put_settings),
        )
        .route(
            "/v1/bots/{bot}/knowledge/email-captures",
            get(knowledge::email_captures),
        )
        .route(
            "/v1/bots/{bot}/knowledge/embedding-approvals",
            post(knowledge_settings::request_embedding_approval),
        )
        .route(
            "/v1/bots/{bot}/knowledge/embedding-approvals/{approval}/confirm",
            post(knowledge_settings::confirm_embedding_approval),
        )
        .route(
            "/v1/bots/{bot}/knowledge/embedding-grants",
            axum::routing::delete(knowledge_settings::revoke_embedding_grant),
        )
        .route(
            "/v1/bots/{bot}/knowledge/embedding-cooldown/reset",
            post(knowledge_settings::reset_embedding_cooldown),
        )
        .route(
            "/v1/memory",
            get(memory::list_memories).post(memory::create_memory),
        )
        .route(
            "/v1/memory/{id}",
            axum::routing::patch(memory::update_memory).delete(memory::delete_memory),
        )
        .route("/v1/memory/{id}/undo", post(memory::undo_memory))
        .route(
            "/v1/bots/{id}/memory/preview",
            get(memory::preview_bot_memory),
        )
        .route(
            "/v1/bots/{id}/grants",
            get(r::list_grants).delete(r::revoke_grant),
        )
        .route("/v1/bots/{id}/engine", put(bot_engine::put_engine))
        .route("/v1/bots/{id}/engine/kill", post(bot_engine::kill_engine))
        .route(
            "/v1/bots/{id}/skills",
            get(skills::get_bot_skills).put(skills::put_bot_skills),
        )
        .route(
            "/v1/skills",
            get(skills::list_skills).post(skill_management::create),
        )
        .route(
            "/v1/skills/import",
            post(skill_management::import)
                .layer(axum::extract::DefaultBodyLimit::max(12 * 1024 * 1024)),
        )
        .route("/v1/skills/hub/preview", post(skill_hub::hub_preview))
        .route("/v1/skills/hub/install", post(skill_hub::hub_install))
        .route(
            "/v1/skills/{name}",
            axum::routing::put(skill_management::update).delete(skill_management::delete),
        )
        .route(
            "/v1/bots/{id}",
            get(r::get_bot)
                .put(r::update_bot)
                .patch(r::patch_bot)
                .delete(r::delete_bot),
        )
        .route("/v1/chats/search", get(chat_history::search))
        .route("/v1/chats/{id}/export", get(chat_history::export))
        .route("/v1/chats", get(r::list_chats).post(r::create_chat))
        .route(
            "/v1/chats/{id}",
            axum::routing::patch(r::update_chat).delete(r::delete_chat),
        )
        .route(
            "/v1/chats/{id}/messages",
            get(r::list_messages).post(r::send_message),
        )
        .route("/v1/chats/{id}/resend", post(r::resend_last_turn))
        .route(
            "/v1/chats/{id}/attachments",
            post(attachment_routes::upload).layer(axum::extract::DefaultBodyLimit::max(
                attachment_routes::MAX_BODY,
            )),
        )
        .route(
            "/v1/chats/{id}/attachments/{att}",
            get(attachment_routes::download),
        )
        .route("/v1/approvals", get(r::list_approvals))
        .route("/v1/approvals/{call_id}", post(r::decide_approval))
        .route("/v1/questions/{call_id}", post(r::answer_question))
        .route("/v1/runs", get(r::list_runs))
        .route(
            "/v1/runs/stop",
            post(stop::stop_runs).layer(axum::extract::DefaultBodyLimit::max(stop::MAX_BODY_BYTES)),
        )
        .route("/v1/runs/{id}", get(r::get_run))
        .route("/v1/runs/{id}/cancel", post(r::cancel_run))
        .route("/v1/runs/{id}/resume", post(r::resume_run))
        .route("/v1/runs/{id}/remember", post(r::remember_run))
        .route("/v1/runs/{id}/usage", get(r::run_usage))
        .route("/v1/runs/{id}/changes", get(changes::get_run_changes))
        .route(
            "/v1/runs/{id}/changes/{cp}/preview",
            get(changes::preview_restore),
        )
        .route(
            "/v1/runs/{id}/changes/{cp}/restore",
            post(changes::restore_changes),
        )
        .route("/v1/usage", get(usage::usage))
        .route("/v1/usage/export", get(usage::export_csv))
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
        .route("/v1/local-models", get(local_models::get_local_models))
        .route("/v1/providers/cli/enable", post(r::enable_cli_provider))
        .route("/v1/providers/cli/disable", post(r::disable_cli_provider))
        .route("/v1/providers/cli", get(cli_engines::list))
        .route("/v1/providers/cli/install", post(cli_engines::preview))
        .route(
            "/v1/providers/cli/{id}/models/refresh",
            post(cli_engines::refresh_models_route),
        )
        .route(
            "/v1/providers/cli/install/{approval_id}/decision",
            post(cli_engines::decide),
        )
        .route("/v1/providers/reload", post(r::reload_providers))
        .route("/v1/doctor", get(doctor_routes::get_doctor))
        .route("/v1/doctor/report", get(doctor_routes::report))
        .route("/v1/doctor/repair/{id}", post(doctor_routes::repair))
        .route(
            "/v1/schedules",
            get(r::list_schedules).post(r::create_schedule),
        )
        .route("/v1/schedules/parse", post(r::parse_schedule))
        .route("/v1/schedules/preview", post(r::preview_schedule))
        .route(
            "/v1/schedules/{id}",
            get(r::get_schedule)
                .put(r::update_schedule)
                .delete(r::delete_schedule),
        )
        .route("/v1/schedules/{id}/history", get(r::list_schedule_history))
        .route(
            "/v1/schedules/{id}/occurrences/{due_ms}/retry",
            post(r::retry_schedule_occurrence),
        )
        .route("/v1/schedules/{id}/run-now", post(r::run_schedule_now))
        .route("/v1/schedules/{id}/reclaim", post(host_import::reclaim))
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
        .route("/v1/mcp/servers/{name}/auth/start", post(m::start_mcp_auth))
        .route(
            "/v1/mcp/servers/{name}/auth/callback",
            post(m::complete_mcp_auth),
        )
        .route(
            "/v1/mcp/servers/{name}/auth/cancel",
            post(m::cancel_mcp_auth),
        )
        .route(
            "/v1/mcp/servers/{name}/auth/status",
            get(m::mcp_auth_status),
        )
        .route(
            "/v1/mcp/servers/{name}/auth/logout",
            post(m::logout_mcp_auth),
        )
        .route("/v1/mcp/catalog", get(m::mcp_catalog))
        .route("/v1/components", get(crate::components::list_components))
        .route("/v1/components/{id}", get(crate::components::get_component))
        .route(
            "/v1/components/{id}/install",
            post(crate::components::install_component),
        )
        .route("/v1/composio", get(c::composio_status))
        .route(
            "/v1/composio/key",
            put(c::composio_set_key).delete(c::composio_remove_key),
        )
        .route(
            "/v1/composio/login",
            get(c::composio_login_status).post(c::composio_login_start),
        )
        .route("/v1/composio/toolkits", get(c::composio_catalog))
        .route("/v1/composio/logos/{slug}", get(c::composio_logo))
        .route("/v1/composio/apps", get(c::composio_list_apps))
        .route(
            "/v1/composio/apps/{slug}",
            get(c::composio_get_app).delete(c::composio_disconnect),
        )
        .route(
            "/v1/composio/apps/{slug}/connect",
            post(c::composio_connect),
        )
        .route(
            "/v1/composio/apps/{slug}/enabled",
            put(c::composio_set_enabled),
        )
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
        .route(
            "/v1/email/accounts/{name}/messages",
            get(e::list_email_messages),
        )
        .route(
            "/v1/email/accounts/{name}/send-test",
            post(e::send_email_test),
        )
        .route("/v1/email/test", post(e::test_email_draft))
        .route(
            "/v1/web/search",
            get(w::get_web_search).put(w::update_web_search),
        )
        .route("/v1/web/search/test", post(w::test_web_search))
        .route(
            "/v1/spend-limit",
            get(spend_routes::get_spend_limit).put(spend_routes::put_spend_limit),
        )
        .route("/v1/voice", get(vo::get_voice).put(vo::update_voice))
        .route(
            "/v1/voice/transcribe",
            post(vo::transcribe).layer(axum::extract::DefaultBodyLimit::max(vo::MAX_BODY_BYTES)),
        )
        .route("/v1/secrets/{*name}", post(m::set_secret))
        .route(
            "/v1/fill-secrets",
            get(f::list_fill_secrets).post(f::create_fill_secret),
        )
        .route(
            "/v1/fill-secrets/{*name}",
            put(f::update_fill_secret).delete(f::delete_fill_secret),
        )
        .route(
            "/v1/fill-grants",
            get(f::list_fill_grants).post(f::create_fill_grant),
        )
        .route(
            "/v1/fill-grants/{id}",
            axum::routing::delete(f::revoke_fill_grant),
        )
        .route(
            "/v1/secret-requests/{call_id}",
            post(secret_request_routes::submit_secret_request),
        )
        .route(
            "/v1/secret-requests/{call_id}/cancel",
            post(secret_request_routes::cancel_secret_request),
        )
        .route("/v1/events", get(sse::events))
        .merge(host_admin::routes())
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
/// I/O errors of the server.
pub async fn serve(
    listener: TcpListener,
    state: AppState,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<Vec<RunId>> {
    serve_with_startup(listener, state, std::future::ready(()), shutdown).await
}

/// Serve the API while `prepare` registers any remaining channels and the
/// leader checks paired peers before opening automatic components.
/// The caller must bind `listener` to a loopback address.
///
/// # Errors
/// I/O errors of the server. Background startup failures are logged.
pub async fn serve_with_startup(
    listener: TcpListener,
    state: AppState,
    prepare: impl Future<Output = ()> + Send + 'static,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<Vec<RunId>> {
    serve_with_startup_timeout(listener, state, prepare, shutdown, STARTUP_TIMEOUT).await
}

/// Longest the background startup (`prepare`, peer check, component start) may run
/// before health reports `degraded`.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(300);

async fn serve_with_startup_timeout(
    listener: TcpListener,
    state: AppState,
    prepare: impl Future<Output = ()> + Send + 'static,
    shutdown: impl Future<Output = ()> + Send + 'static,
    startup_timeout: Duration,
) -> std::io::Result<Vec<RunId>> {
    let _cancel_on_drop = state.shutdown_token().drop_guard();
    state
        .leader()
        .set_startup(host::StartupPhase::Starting, None);
    let startup = {
        let leader = state.leader().clone();
        let cancel = state.shutdown_token();
        // The work runs in its own task so that a panic is caught at the join.
        let work = {
            let leader = leader.clone();
            tokio::spawn(async move {
                prepare.await;
                leader.start().await
            })
        };
        tokio::spawn(async move {
            let began = std::time::Instant::now();
            eprintln!("startup: leader components starting in background");
            let abort = work.abort_handle();
            tokio::select! {
                () = cancel.cancelled() => abort.abort(),
                result = tokio::time::timeout(startup_timeout, work) => {
                    let elapsed = began.elapsed().as_millis();
                    match result {
                        Err(_) => {
                            abort.abort();
                            leader.set_degraded(host::StartupReason::TimedOut);
                            eprintln!("warning: leader startup timed out ({elapsed} ms)");
                        }
                        Ok(Err(e)) => {
                            leader.set_degraded(host::StartupReason::TaskPanicked);
                            eprintln!("warning: leader startup task failed: {e}");
                        }
                        Ok(Ok(Ok(()))) if leader.components_allowed() => {
                            leader.set_startup(host::StartupPhase::Ready, None);
                            eprintln!("startup: leader components started in background (+{elapsed} ms)");
                        }
                        Ok(Ok(Ok(()))) => {
                            leader.set_startup(host::StartupPhase::Ready, None);
                            eprintln!("startup: leader components idle ({elapsed} ms)");
                        }
                        Ok(Ok(Err(e))) => {
                            // Fixed text: the error itself may carry a path.
                            leader.set_degraded(host::StartupReason::from_error(&e));
                            eprintln!("warning: leader components failed to start ({elapsed} ms): {e}");
                        }
                    }
                }
            }
        })
    };
    // Fake-provider/test embedders have no host provider configuration and
    // must never probe the real machine's credentialed CLIs as a side effect.
    if state.provider_host().is_some() {
        let detector = state.detector();
        let warm = state.clone();
        tokio::spawn(async move {
            let rows = detector.detect(false).await;
            for row in rows.iter().filter(|r| r.installed) {
                cli_engines::refresh_models_if_stale(&warm, &row.id).await;
            }
        });
    }
    let stopping = state.clone();
    let served = axum::serve(listener, router(state.clone()))
        .with_graceful_shutdown(async move {
            shutdown.await;
            stopping.shutdown_token().cancel();
            if let Err(e) = startup.await {
                eprintln!("warning: leader startup task failed: {e}");
            }
            stopping.stop_components().await;
        })
        .await;
    served?;
    Ok(state.checkpoint_runs().await)
}
