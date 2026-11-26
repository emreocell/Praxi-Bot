//! Persistent installation ownership and runtime leadership control.
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use axum::{
    Extension, Json,
    extract::{Path as Param, State},
    http::StatusCode,
};
use parking_lot::Mutex;
use peer_network::{LeaderInfo, LeaderState, PeerNetwork, Role};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{ApiError, AppState, WeakAppState, routes::ApiJson};

/// How long startup waits for a paired desktop or host heartbeat.
const PEER_CHECK_TIMEOUT: Duration = Duration::from_secs(10);

/// Epochs stay representable in TOML and bounded even for compromised peers.
/// At the ceiling, explicit force reuses the ceiling; equal owners still stop both.
pub const EPOCH_CEILING: u64 = 1_000_000_000;
pub const MAX_PEER_EPOCH_ADVANCE: u64 = 4096;
/// Managed installer paths. The socket and the status file are owned by systemd/root.
pub const UPDATE_SOCKET: &str = "/run/praxi-update.sock";
pub const UPDATE_STATUS: &str = "/var/lib/praxi-updates/status.json";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HostMode {
    #[default]
    Standalone,
    Host,
    Client,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HostState {
    #[default]
    Active,
    Standby,
    Fresh,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct HostLimits {
    pub max_computer_runs: Option<usize>,
    pub browser_idle_seconds: Option<u64>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct HostConfig {
    pub mode: HostMode,
    pub state: HostState,
    pub epoch: u64,
    pub leader: Option<String>,
    pub limits: HostLimits,
}

impl HostConfig {
    /// Missing files preserve the pre-host standalone behavior. Invalid files fail closed.
    /// # Errors
    /// Returns I/O, TOML parsing or ownership validation failures.
    pub fn load(data: &Path) -> std::io::Result<Self> {
        let text = match std::fs::read_to_string(data.join("host.toml")) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e),
        };
        let config: Self = toml::from_str(&text).map_err(std::io::Error::other)?;
        config.validate()?;
        Ok(config)
    }

    /// Validate state combinations, leader keys and resource limits.
    /// # Errors
    /// Returns an error for an invalid key, state or zero computer-run limit.
    pub fn validate(&self) -> std::io::Result<()> {
        if self.limits.max_computer_runs == Some(0) {
            return Err(std::io::Error::other("max_computer_runs must be positive"));
        }
        if let Some(key) = &self.leader {
            peer_network::key_from_string(key).map_err(std::io::Error::other)?;
        }
        if self.mode == HostMode::Client && self.state != HostState::Standby {
            return Err(std::io::Error::other("client mode requires standby state"));
        }
        if self.state == HostState::Fresh && self.mode != HostMode::Host {
            return Err(std::io::Error::other("fresh state requires host mode"));
        }
        Ok(())
    }

    /// Persist via a synced temporary file and an atomic replacement on the same volume.
    /// # Errors
    /// Returns validation, serialization, write, sync or atomic replacement failures.
    pub fn save(&self, data: &Path) -> std::io::Result<()> {
        use std::io::Write;
        self.validate()?;
        std::fs::create_dir_all(data)?;
        let mut file = tempfile::NamedTempFile::new_in(data)?;
        // Ownership updates must preserve settings written by other versions/modules.
        let path = data.join("host.toml");
        let mut root: toml::Table = match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).map_err(std::io::Error::other)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => toml::Table::new(),
            Err(e) => return Err(e),
        };
        let typed = toml::Table::try_from(self).map_err(std::io::Error::other)?;
        for key in ["mode", "state", "epoch", "leader"] {
            root.remove(key);
            if let Some(value) = typed.get(key) {
                root.insert(key.into(), value.clone());
            }
        }
        let limits = root
            .entry("limits")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        let limits = limits
            .as_table_mut()
            .ok_or_else(|| std::io::Error::other("limits must be a table"))?;
        for key in ["max_computer_runs", "browser_idle_seconds"] {
            limits.remove(key);
            if let Some(value) = typed.get("limits").and_then(|v| v.get(key)) {
                limits.insert(key.into(), value.clone());
            }
        }
        file.write_all(
            toml::to_string_pretty(&root)
                .map_err(std::io::Error::other)?
                .as_bytes(),
        )?;
        file.as_file().sync_all()?;
        file.persist(data.join("host.toml")).map_err(|e| e.error)?;
        #[cfg(unix)]
        std::fs::File::open(data)?.sync_all()?;
        Ok(())
    }

    #[must_use]
    pub fn runs_components(&self) -> bool {
        self.mode != HostMode::Client && self.state != HostState::Standby
    }

    fn wire(&self) -> LeaderInfo {
        LeaderInfo {
            epoch: self.epoch,
            state: match self.state {
                HostState::Active => LeaderState::Active,
                HostState::Standby => LeaderState::Standby,
                HostState::Fresh => LeaderState::Fresh,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct HostConflict {
    pub reason: String,
    pub peer: Option<String>,
    pub epoch: u64,
    /// Human-readable notification accompanying the SSE event.
    pub notification: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum HostEvent {
    HostStateChanged(HostConfig),
    HostConflict(HostConflict),
}

/// How far background startup of the automatic components has got.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum StartupPhase {
    /// Leadership verification and component startup are still running.
    Starting,
    /// Startup finished (components run, or this installation is standby).
    Ready,
    /// Startup finished but a component could not start.
    Degraded,
}

/// Why startup is `degraded`. Fixed vocabulary: the texts hold no paths or secrets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StartupReason {
    /// The schedule store could not be opened.
    StoreUnavailable,
    /// The daemon was shutting down while components started.
    DaemonStopped,
    /// Another component failed to start.
    ComponentFailed,
    /// The background startup task panicked.
    TaskPanicked,
    /// Leadership verification or `prepare` did not finish in time.
    TimedOut,
}

impl StartupReason {
    pub(crate) fn text(self) -> &'static str {
        match self {
            Self::StoreUnavailable => {
                "the schedule store could not be opened; the scheduler is not running"
            }
            Self::DaemonStopped => "the daemon was stopping while components started",
            Self::ComponentFailed => "a component could not start; see the daemon log",
            Self::TaskPanicked => "background startup failed unexpectedly; see the daemon log",
            Self::TimedOut => {
                "background startup did not finish in time; leadership could not be verified"
            }
        }
    }
    pub(crate) fn from_error(error: &ApiError) -> Self {
        match error.code_str().as_str() {
            "unavailable" => Self::DaemonStopped,
            "internal" => Self::StoreUnavailable,
            _ => Self::ComponentFailed,
        }
    }
}

/// Shared handle for handover, API actions and conflict detection. Transitions are serialized.
#[derive(Clone)]
pub struct LeaderControl(Arc<LeaderInner>);
struct LeaderInner {
    data: PathBuf,
    config: Mutex<HostConfig>,
    transition: tokio::sync::Mutex<()>,
    startup: tokio::sync::Mutex<()>,
    verifying: AtomicBool,
    phase: Mutex<(StartupPhase, Option<String>)>,
    state: WeakAppState,
    network: Mutex<Option<PeerNetwork>>,
    invites: Mutex<std::collections::VecDeque<std::time::Instant>>,
    admin_calls: Mutex<std::collections::VecDeque<std::time::Instant>>,
    events: tokio::sync::broadcast::Sender<HostEvent>,
    peer_check_timeout: Mutex<Duration>,
}

impl LeaderControl {
    pub(crate) fn new(data: PathBuf, config: HostConfig, state: WeakAppState) -> Self {
        Self(Arc::new(LeaderInner {
            data,
            config: Mutex::new(config),
            transition: tokio::sync::Mutex::new(()),
            startup: tokio::sync::Mutex::new(()),
            verifying: AtomicBool::new(false),
            phase: Mutex::new((StartupPhase::Ready, None)),
            state,
            network: Mutex::new(None),
            invites: Mutex::default(),
            admin_calls: Mutex::default(),
            events: tokio::sync::broadcast::channel(64).0,
            peer_check_timeout: Mutex::new(PEER_CHECK_TIMEOUT),
        }))
    }
    #[must_use]
    pub fn config(&self) -> HostConfig {
        self.0.config.lock().clone()
    }
    /// Subscribe to live ownership and conflict notifications.
    #[must_use]
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<HostEvent> {
        self.0.events.subscribe()
    }
    /// Record how background startup stands. `reason` must hold no secrets or paths.
    pub(crate) fn set_startup(&self, phase: StartupPhase, reason: Option<&str>) {
        *self.0.phase.lock() = (phase, reason.map(str::to_owned));
    }
    /// Mark startup degraded for a fixed reason.
    pub(crate) fn set_degraded(&self, reason: StartupReason) {
        self.set_startup(StartupPhase::Degraded, Some(reason.text()));
    }
    /// A later successful component start (for example `host activate`) ends `degraded`.
    pub(crate) fn clear_degraded(&self) {
        let mut phase = self.0.phase.lock();
        if phase.0 == StartupPhase::Degraded {
            *phase = (StartupPhase::Ready, None);
        }
    }
    /// The startup phase and, when degraded, a fixed human-readable reason.
    pub(crate) fn startup(&self) -> (StartupPhase, Option<String>) {
        self.0.phase.lock().clone()
    }
    /// Whether background startup (peer check, component start) is still running.
    pub(crate) fn is_starting(&self) -> bool {
        self.0.phase.lock().0 == StartupPhase::Starting
    }
    /// The attached endpoint, if peer networking is enabled.
    #[must_use]
    pub fn network(&self) -> Option<PeerNetwork> {
        self.0.network.lock().clone()
    }

    fn admit_invite(&self) -> Result<(), ApiError> {
        let mut invites = self.0.invites.lock();
        let now = std::time::Instant::now();
        while invites
            .front()
            .is_some_and(|time| now.duration_since(*time) >= Duration::from_secs(60))
        {
            invites.pop_front();
        }
        if invites.len() >= 5 {
            return Err(ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "invite_rate_limit",
                "at most five host invites per minute",
            ));
        }
        invites.push_back(now);
        Ok(())
    }

    /// Admission for one host-administration call: a sliding minute window
    /// shared by every administrator, so a stuck client cannot hammer the
    /// secret store or `providers.toml`.
    pub(crate) fn admit_admin_call(&self) -> Result<(), ApiError> {
        let mut calls = self.0.admin_calls.lock();
        let now = std::time::Instant::now();
        while calls
            .front()
            .is_some_and(|time| now.duration_since(*time) >= Duration::from_secs(60))
        {
            calls.pop_front();
        }
        if calls.len() >= crate::host_admin::MAX_ADMIN_CALLS_PER_MINUTE {
            return Err(ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "admin_rate_limit",
                "too many administration calls; wait a minute",
            ));
        }
        calls.push_back(now);
        Ok(())
    }

    /// Tests: forget the calls counted so far, so a long matrix of administrator
    /// requests is judged by what the guard decides, not by the minute budget.
    #[cfg(test)]
    pub(crate) fn reset_admin_budget(&self) {
        self.0.admin_calls.lock().clear();
    }

    /// Whether the paired device `key` currently holds host administration.
    /// Read from the pairing store on every call: a revoke or a withdrawn
    /// grant takes effect on the very next request of a live connection.
    #[must_use]
    pub(crate) fn peer_is_admin(&self, key: &str) -> bool {
        let Some(net) = self.network() else {
            return false;
        };
        peer_network::key_from_string(key)
            .ok()
            .and_then(|k| net.device(&k))
            .is_some_and(|d| d.admin && d.role == Role::Desktop)
    }

    /// Attach the already-running peer endpoint and publish our ownership state.
    pub fn set_network(&self, net: PeerNetwork) {
        let config = self.0.config.lock();
        self.0.verifying.store(
            config.state == HostState::Active
                && net
                    .devices()
                    .iter()
                    .any(|d| matches!(d.role, Role::Desktop | Role::Host)),
            Ordering::SeqCst,
        );
        net.set_leader_info(Some(config.wire()));
        *self.0.network.lock() = Some(net);
    }

    /// Shorten the startup peer check. Production uses [`PEER_CHECK_TIMEOUT`].
    #[cfg(test)]
    pub(crate) fn set_peer_check_timeout(&self, timeout: Duration) {
        *self.0.peer_check_timeout.lock() = timeout;
    }

    /// Release the endpoint during daemon shutdown (its API handler owns router state).
    pub fn detach_network(&self) {
        if let Some(net) = self.0.network.lock().take() {
            net.set_leader_info(None);
        }
    }

    async fn apply(
        &self,
        next: HostConfig,
        actor: &str,
        action: &str,
    ) -> Result<HostConfig, ApiError> {
        let state = self
            .0
            .state
            .upgrade()
            .ok_or_else(|| ApiError::unavailable("daemon stopped"))?;
        if state.shutdown_token().is_cancelled() {
            return Err(ApiError::unavailable("daemon stopped"));
        }
        let previous = self.config();
        let audit = self.audit(
            actor,
            action,
            &serde_json::json!({"previous": previous, "next": next, "phase": "requested"}),
        );
        if previous.state == HostState::Fresh && next.state == HostState::Fresh {
            // Learning a leader does not activate this empty handover target.
            // In particular, component startup/rollback must never consume fresh.
            audit?;
            next.save(&self.0.data).map_err(ApiError::internal)?;
            *self.0.config.lock() = next.clone();
            if let Some(net) = self.network() {
                net.set_leader_info(Some(next.wire()));
            }
            let _ = self
                .0
                .events
                .send(HostEvent::HostStateChanged(next.clone()));
            return Ok(next);
        }
        if next.runs_components() {
            audit?;
            // Keep a durable idle checkpoint until startup and the final save
            // both succeed. Even a failed rollback cannot leave active on disk.
            let mut idle = next.clone();
            idle.state = HostState::Standby;
            idle.save(&self.0.data).map_err(ApiError::internal)?;
            *self.0.config.lock() = next.clone();
            let started = state
                .start_leader_components()
                .and_then(|()| next.save(&self.0.data).map_err(ApiError::internal));
            if let Err(error) = started {
                *self.0.config.lock() = idle.clone();
                if let Some(net) = self.network() {
                    net.set_leader_info(Some(idle.wire()));
                }
                state.stop_components().await;
                let _ = self.0.events.send(HostEvent::HostStateChanged(idle));
                return Err(error);
            }
            self.clear_degraded();
            if let Some(net) = self.network() {
                net.set_leader_info(Some(next.wire()));
            }
            let _ = self
                .0
                .events
                .send(HostEvent::HostStateChanged(next.clone()));
        } else {
            // Close runtime admission even if persistence fails. Write ownership
            // before awaiting shutdown so cancellation cannot leave active on disk.
            *self.0.config.lock() = next.clone();
            if let Some(net) = self.network() {
                net.set_leader_info(Some(next.wire()));
            }
            let saved = next.save(&self.0.data);
            state.stop_components().await;
            let _ = self
                .0
                .events
                .send(HostEvent::HostStateChanged(next.clone()));
            saved.map_err(ApiError::internal)?;
            audit?;
        }
        Ok(next)
    }

    fn audit(&self, actor: &str, action: &str, detail: &serde_json::Value) -> Result<(), ApiError> {
        let state = self
            .0
            .state
            .upgrade()
            .ok_or_else(|| ApiError::unavailable("daemon stopped"))?;
        let config = self.config();
        let next = detail
            .get("next")
            .cloned()
            .unwrap_or_else(|| serde_json::json!(config));
        state
            .storage()
            .append_audit(
                state.user_id(),
                None,
                actor,
                action,
                &serde_json::json!({
                    "previous_state": config.state, "previous_epoch": config.epoch,
                    "new_state": next["state"], "new_epoch": next["epoch"], "detail": detail,
                }),
            )
            .map(|_| ())
            .map_err(ApiError::internal)
    }

    /// Linearize a synchronous automatic-run admission with ownership changes.
    pub(crate) fn with_active<T>(&self, action: impl FnOnce() -> T) -> Result<T, ApiError> {
        let config = self.0.config.lock();
        if self.0.verifying.load(Ordering::SeqCst) || !config.runs_components() {
            return Err(ApiError::conflict("this installation is standby"));
        }
        let result = action();
        drop(config);
        Ok(result)
    }

    pub(crate) fn components_allowed(&self) -> bool {
        !self.0.verifying.load(Ordering::SeqCst) && self.config().runs_components()
    }

    /// Start after a bounded check of paired daemon heartbeats.
    /// Unreachable peers cannot establish ownership; after the deadline the
    /// persisted owner starts, with the residual partition risk documented.
    /// A failed audit or save during a demotion is logged. Startup continues
    /// in that demoted state and does not open components.
    /// # Errors
    /// Returns an unavailable daemon or errors opening the schedule store.
    pub async fn start(&self) -> Result<(), ApiError> {
        let _startup = self.0.startup.lock().await;
        if self.0.verifying.load(Ordering::SeqCst) {
            let timeout = *self.0.peer_check_timeout.lock();
            let deadline = tokio::time::Instant::now() + timeout;
            loop {
                let mut complete = true;
                if let Some(net) = self.network() {
                    for device in net
                        .devices()
                        .iter()
                        .filter(|d| matches!(d.role, Role::Desktop | Role::Host))
                    {
                        if let Ok(key) = peer_network::key_from_string(&device.id) {
                            if let Some(info) = net.peer_leader(&key) {
                                // Apply the verdict ourselves before opening admission;
                                // the watcher may not yet have consumed its event.
                                // Persistence can fail after the safe demotion is
                                // already in memory. Do not abort startup for that.
                                if let Err(error) = self.observe(&device.id, info).await {
                                    support_logs::tracing::warn!(target: "praxi_support", reason = "component_warning",
                                        "host leadership: startup peer check failed; continuing in the current ownership state: {error}"
                                    );
                                }
                            } else {
                                complete = false;
                            }
                        }
                    }
                }
                if complete
                    || self.config().state != HostState::Active
                    || tokio::time::Instant::now() >= deadline
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        }
        let _transition = self.0.transition.lock().await;
        let state = self
            .0
            .state
            .upgrade()
            .ok_or_else(|| ApiError::unavailable("daemon stopped"))?;
        self.0.verifying.store(false, Ordering::SeqCst);
        // Opening the scheduler store and reading channel credentials can block.
        // Hand off the HTTP work even if a CPU quota leaves Tokio one worker.
        if tokio::runtime::Handle::current().runtime_flavor()
            == tokio::runtime::RuntimeFlavor::MultiThread
        {
            tokio::task::block_in_place(|| state.start_leader_components())
        } else {
            state.start_leader_components()
        }
    }

    /// Commit an explicitly authorized handover state after its transfer protocol completes.
    /// # Errors
    /// Rejects invalid state, decreasing epochs, persistence failures and startup errors.
    pub async fn transition_to(&self, next: HostConfig) -> Result<HostConfig, ApiError> {
        let _transition = self.0.transition.lock().await;
        next.validate()
            .map_err(|e| ApiError::bad_request(e.to_string()))?;
        if next.epoch < self.config().epoch {
            return Err(ApiError::conflict(
                "handover cannot decrease the ownership epoch",
            ));
        }
        self.0.verifying.store(false, Ordering::SeqCst);
        self.apply(next, "local", "host.handover").await
    }

    /// Stop automatic components and persist standby ownership.
    /// # Errors
    /// Returns persistence errors or an unavailable daemon. Components still stop on write failure.
    pub async fn standby(&self) -> Result<HostConfig, ApiError> {
        self.standby_as("local").await
    }

    async fn standby_as(&self, actor: &str) -> Result<HostConfig, ApiError> {
        let _transition = self.0.transition.lock().await;
        let mut next = self.config();
        next.state = HostState::Standby;
        self.apply(next, actor, "host.standby").await
    }

    /// Explicitly take ownership with an epoch above all known epochs.
    /// # Errors
    /// Rejects unforced takeover of a known leader, exhausted epochs and persistence/startup failures.
    pub async fn activate(&self, force: bool) -> Result<HostConfig, ApiError> {
        self.activate_as(force, "local").await
    }

    async fn activate_as(&self, force: bool, actor: &str) -> Result<HostConfig, ApiError> {
        let _transition = self.0.transition.lock().await;
        let mut next = self.config();
        let mut highest = next.epoch;
        let mut known = next.leader.is_some();
        if let Some(net) = self.network() {
            for device in net
                .devices()
                .iter()
                .filter(|d| matches!(d.role, Role::Desktop | Role::Host))
            {
                if let Ok(key) = peer_network::key_from_string(&device.id)
                    && let Some(info) = net.peer_leader(&key)
                {
                    if !acceptable_peer_epoch(next.epoch, info.epoch) {
                        continue;
                    }
                    highest = highest.max(info.epoch);
                    known |= info.state == LeaderState::Active && info.epoch >= next.epoch;
                }
            }
        }
        if known && !force {
            return Err(ApiError::conflict(
                "a known leader (possibly a stale record) may still be active; use force to run here from the last local data; schedules may run twice",
            ));
        }
        if highest >= EPOCH_CEILING && !force {
            return Err(ApiError::conflict(
                "leader epoch ceiling reached; explicit force required",
            ));
        }
        if next.state == HostState::Active && !force {
            self.audit(
                actor,
                "host.activate",
                &serde_json::json!({"next":next,"phase":"requested","no_change":true}),
            )?;
            // A Telegram 409 leaves the channel in conflict without giving up
            // ownership. Restart that channel; do not bump the epoch.
            self.restart_conflicted_channels();
            return Ok(next);
        }
        next.epoch = highest.saturating_add(1).min(EPOCH_CEILING);
        self.0.verifying.store(false, Ordering::SeqCst);
        next.state = HostState::Active;
        next.leader = None;
        if next.mode == HostMode::Client {
            next.mode = HostMode::Standalone;
        }
        self.apply(next, actor, "host.activate").await
    }

    fn restart_conflicted_channels(&self) {
        let Some(state) = self.0.state.upgrade() else {
            return;
        };
        if !self.components_allowed() {
            return;
        }
        for channel in state.channels() {
            if channel.info().status.as_deref() == Some("conflict") {
                channel.start();
            }
        }
    }

    /// Apply authenticated daemon heartbeat metadata. There is no automatic takeover.
    /// # Errors
    /// Returns persistence errors while demoting or an unavailable daemon.
    pub async fn observe(&self, peer: &str, info: LeaderInfo) -> Result<(), ApiError> {
        let _transition = self.0.transition.lock().await;
        let mut next = self.config();
        if !acceptable_peer_epoch(next.epoch, info.epoch)
            || info.state != LeaderState::Active
            || info.epoch < next.epoch
        {
            return Ok(());
        }
        let conflict = info.epoch == next.epoch && next.state == HostState::Active;
        if info.epoch > next.epoch || conflict {
            if next.state != HostState::Fresh {
                next.state = HostState::Standby;
            }
            next.epoch = info.epoch;
            next.leader = Some(peer.into());
            let result = self
                .apply(next, &format!("peer:{peer}"), "host.observe")
                .await;
            if conflict {
                self.emit_conflict("equal_epoch", Some(peer.into()));
            }
            result?;
        }
        Ok(())
    }

    /// Apply a conflict notification only for two active, equal-epoch owners.
    /// The sender's state comes from the authenticated heartbeat cache. The
    /// conflict frame echoes the receiver's own metadata and is not trusted.
    /// # Errors
    /// Returns ownership persistence or component shutdown errors.
    pub async fn observe_conflict(&self, peer: &str) -> Result<(), ApiError> {
        let _transition = self.0.transition.lock().await;
        let Some(net) = self.network() else {
            return Ok(());
        };
        let Ok(key) = peer_network::key_from_string(peer) else {
            return Ok(());
        };
        if !net
            .device(&key)
            .is_some_and(|device| matches!(device.role, Role::Desktop | Role::Host))
        {
            return Ok(());
        }
        let Some(info) = net.peer_leader(&key) else {
            return Ok(());
        };
        let mut local = self.config();
        if local.state != HostState::Active
            || info.state != LeaderState::Active
            || info.epoch != local.epoch
            || !acceptable_peer_epoch(local.epoch, info.epoch)
        {
            return Ok(());
        }
        local.state = HostState::Standby;
        local.leader = Some(peer.into());
        self.apply(local, &format!("peer:{peer}"), "host.conflict")
            .await?;
        self.emit_conflict("equal_epoch", Some(peer.into()));
        Ok(())
    }

    pub fn emit_conflict(&self, reason: &str, peer: Option<String>) {
        let event = HostConflict { reason: reason.into(), peer, epoch: self.config().epoch, notification: "Another installation is running this bot. Choose the active installation before resuming.".into() };
        let _ = self.0.events.send(HostEvent::HostConflict(event));
    }

    /// Notify after a terminal Telegram conflict; only the channel stops.
    pub fn telegram_conflict(&self) {
        self.emit_conflict("telegram_409", None);
    }
}

fn actor(peer: Option<&Extension<crate::PeerIdentity>>) -> String {
    peer.map_or_else(
        || "local".into(),
        |p| {
            format!(
                "{}:{}",
                match p.role {
                    crate::PeerRole::Phone => "phone",
                    crate::PeerRole::Desktop => "desktop",
                    crate::PeerRole::Host => "host",
                    crate::PeerRole::Node => "node",
                },
                p.key
            )
        },
    )
}

fn acceptable_peer_epoch(local: u64, remote: u64) -> bool {
    remote <= EPOCH_CEILING && remote <= local.saturating_add(MAX_PEER_EPOCH_ADVANCE)
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct HostPeer {
    pub key: String,
    pub name: String,
    pub role: String,
    pub online: bool,
    pub last_seen_ms: Option<i64>,
    /// The device holds host administration (may send keys and settings here).
    pub admin: bool,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct HostStatus {
    pub mode: HostMode,
    pub state: HostState,
    pub epoch: u64,
    pub leader: Option<String>,
    pub endpoint_id: Option<String>,
    pub relay_url: Option<String>,
    pub version: String,
    /// Release tag this server was built from (`v0.1.0-alpha.3`); absent for untagged builds.
    #[serde(default)]
    pub release_tag: Option<String>,
    /// Identifies this exact build (the release tag, else `<version>+g<hash>`).
    #[serde(default)]
    pub build_id: String,
    pub api_version: String,
    pub os: String,
    pub arch: String,
    pub has_display: bool,
    pub has_browser: bool,
    pub total_ram_mb: u64,
    pub peers: Vec<HostPeer>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum InviteRole {
    Desktop,
    Phone,
    Node,
    Host,
}
impl InviteRole {
    fn peer_role(&self) -> Role {
        match self {
            Self::Desktop => Role::Desktop,
            Self::Phone => Role::Phone,
            Self::Node => Role::Node,
            Self::Host => Role::Host,
        }
    }
}
#[derive(Debug, Deserialize, ToSchema)]
pub struct HostInviteInput {
    pub role: InviteRole,
    pub ttl_secs: Option<u64>,
    /// Pair a desktop WITH host administration (default: yes for a desktop
    /// invite created by the local owner; a paired peer can never set it).
    #[serde(default)]
    pub admin: Option<bool>,
}
/// `PUT /v1/host/devices/{key}/admin`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct HostAdminInput {
    pub enabled: bool,
}
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct HostInvite {
    pub ticket: String,
    pub code: String,
    pub expires_at_ms: i64,
    pub qr_svg: String,
    pub install_command: Option<String>,
}
#[derive(Debug, Deserialize, ToSchema)]
pub struct ActivateInput {
    pub force: bool,
}

#[utoipa::path(get, operation_id = "host_status", path = "/v1/host", tag = "host", security(("bearer" = [])), responses((status = 200, body = HostStatus)))]
pub(crate) async fn status(State(s): State<AppState>) -> Json<HostStatus> {
    let cfg = s.leader().config();
    let host = crate::dto::HostInfo::detect();
    let net = s.leader().network();
    let peers = net.as_ref().map_or_else(Vec::new, |net| {
        net.devices()
            .into_iter()
            .filter_map(|d| {
                let key = peer_network::key_from_string(&d.id).ok()?;
                Some(HostPeer {
                    key: d.id,
                    name: d.name,
                    role: d.role.to_string(),
                    admin: d.admin && d.role == Role::Desktop,
                    online: net.is_connected(&key),
                    last_seen_ms: net.last_seen(&key).map(|t| {
                        storage::now_ms().saturating_sub(
                            i64::try_from(t.elapsed().as_millis()).unwrap_or(i64::MAX),
                        )
                    }),
                })
            })
            .collect()
    });
    Json(HostStatus {
        mode: cfg.mode,
        state: cfg.state,
        epoch: cfg.epoch,
        leader: cfg.leader,
        relay_url: net
            .as_ref()
            .and_then(|n| n.addr().relay_urls().next().map(ToString::to_string)),
        endpoint_id: net.map(|n| n.id().to_string()),
        version: env!("CARGO_PKG_VERSION").into(),
        release_tag: crate::release_tag().map(str::to_owned),
        build_id: crate::build_id().to_owned(),
        api_version: crate::API_VERSION.into(),
        os: host.os,
        arch: host.arch,
        has_display: host.has_display,
        has_browser: host.has_browser,
        total_ram_mb: host.total_ram_mb,
        peers,
    })
}

#[utoipa::path(post, operation_id = "host_invite", path = "/v1/host/invite", tag = "host", security(("bearer" = [])), request_body = HostInviteInput, responses((status = 200, body = HostInvite)))]
pub(crate) async fn invite(
    State(s): State<AppState>,
    peer: Option<Extension<crate::PeerIdentity>>,
    headers: axum::http::HeaderMap,
    ApiJson(input): ApiJson<HostInviteInput>,
) -> Result<Json<HostInvite>, ApiError> {
    // A phone that can mint another phone invite leaves a foothold that
    // survives revocation of the stolen device. The ADR creates phone
    // invites from the desktop ("Telefon ekle"), not from a phone.
    if peer
        .as_ref()
        .is_some_and(|p| p.role != crate::PeerRole::Desktop)
    {
        let reason = "only a local user or desktop may invite devices";
        s.leader().audit(
            &actor(peer.as_ref()),
            "host.invite",
            &serde_json::json!({"role": input.role, "phase": "denied", "reason": reason}),
        )?;
        return Err(ApiError::forbidden(reason));
    }
    let net = s
        .leader()
        .network()
        .ok_or_else(|| ApiError::conflict("peer networking is disabled; start bot serve --peer"))?;
    let ttl = input
        .ttl_secs
        .unwrap_or(if matches!(input.role, InviteRole::Host) {
            900
        } else {
            120
        });
    if ttl == 0 || ttl > 1800 {
        return Err(ApiError::bad_request("ttl_secs must be between 1 and 1800"));
    }
    s.leader().audit(
        &actor(peer.as_ref()),
        "host.invite",
        &serde_json::json!({"role":input.role,"ttl_secs":ttl,"phase":"requested"}),
    )?;
    s.leader().admit_invite()?;
    // Host administration is granted at pairing only when the OWNER asks
    // for it on this installation (no paired peer, whatever its role).
    // It also needs the owner key: the general API token alone issues an
    // ordinary invite and is refused when it explicitly asks for administration.
    let owner = s.owner_ok(&headers);
    if input.admin == Some(true) && peer.is_none() && !owner {
        return Err(s.require_owner(&headers).unwrap_err());
    }
    let admin = peer.is_none()
        && owner
        && matches!(input.role, InviteRole::Desktop)
        && input.admin.unwrap_or(true);
    let invite = net
        .create_invite_with_options(input.role.peer_role(), Duration::from_secs(ttl), admin)
        .await;
    let qr = qrcode::QrCode::new(invite.ticket.as_bytes()).map_err(ApiError::internal)?;
    // The installer does not implement --join yet. Do not return a broken command.
    Ok(Json(HostInvite {
        ticket: invite.ticket,
        code: invite.code,
        expires_at_ms: storage::now_ms()
            .saturating_add(i64::try_from(ttl * 1000).unwrap_or(i64::MAX)),
        qr_svg: qr.render::<qrcode::render::svg::Color>().build(),
        install_command: None,
    }))
}

#[utoipa::path(delete, operation_id = "host_revoke", path = "/v1/host/devices/{key}", tag = "host", security(("bearer" = [])), params(("key" = String, Path)), responses((status = 204)))]
pub(crate) async fn revoke(
    State(s): State<AppState>,
    Param(key): Param<String>,
    peer: Option<Extension<crate::PeerIdentity>>,
) -> Result<StatusCode, ApiError> {
    let key =
        peer_network::key_from_string(&key).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let net = s
        .leader()
        .network()
        .ok_or_else(|| ApiError::conflict("peer networking is disabled"))?;
    if peer
        .as_ref()
        .is_some_and(|p| p.role == crate::PeerRole::Phone)
        && net.device(&key).is_some_and(|d| d.role != Role::Phone)
    {
        let reason = "phones may revoke only phone devices";
        s.leader().audit(
            &actor(peer.as_ref()),
            "host.revoke",
            &serde_json::json!({"target": key.to_string(), "phase": "denied", "reason": reason}),
        )?;
        return Err(ApiError::forbidden(reason));
    }
    s.leader().audit(
        &actor(peer.as_ref()),
        "host.revoke",
        &serde_json::json!({"target":key.to_string(),"phase":"requested"}),
    )?;
    if !net.revoke(&key).map_err(ApiError::internal)? {
        return Err(ApiError::not_found("device"));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// The owner of this installation (not a paired peer) grants or withdraws host
/// administration for a paired desktop. This is the one confirmation step for a
/// desktop that was paired before the capability existed. Audited.
#[utoipa::path(put, operation_id = "host_set_admin", path = "/v1/host/devices/{key}/admin", tag = "host", security(("bearer" = [])), params(("key" = String, Path)), request_body = HostAdminInput, responses((status = 204), (status = 403, body = crate::error::ErrorBody), (status = 404, body = crate::error::ErrorBody)))]
pub(crate) async fn set_admin(
    State(s): State<AppState>,
    Param(key): Param<String>,
    peer: Option<Extension<crate::PeerIdentity>>,
    headers: axum::http::HeaderMap,
    ApiJson(input): ApiJson<HostAdminInput>,
) -> Result<StatusCode, ApiError> {
    if peer.is_some() {
        return Err(ApiError::forbidden(
            "host administration is granted on the host itself",
        ));
    }
    s.require_owner(&headers)?;
    let key =
        peer_network::key_from_string(&key).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let net = s
        .leader()
        .network()
        .ok_or_else(|| ApiError::conflict("peer networking is disabled"))?;
    s.leader().audit(
        &actor(peer.as_ref()),
        "host.admin_grant",
        &serde_json::json!({"target":key.to_string(),"enabled":input.enabled,"phase":"requested"}),
    )?;
    match net.set_device_admin(&key, input.enabled) {
        Ok(true) => {
            if input.enabled {
                // A new administrator watches and drives the host's screen at once:
                // an earlier "off" for this device no longer applies.
                s.clear_screen_denial(&format!("peer:{}", peer_network::key_to_string(&key)));
            }
            Ok(StatusCode::NO_CONTENT)
        }
        Ok(false) => Err(ApiError::not_found("device")),
        Err(peer_network::PeerError::Protocol(m)) => Err(ApiError::bad_request(m)),
        Err(e) => Err(ApiError::internal(e)),
    }
}

#[utoipa::path(post, operation_id = "host_standby", path = "/v1/host/standby", tag = "host", security(("bearer" = [])), responses((status = 200, body = HostConfig)))]
pub(crate) async fn standby(
    State(s): State<AppState>,
    peer: Option<Extension<crate::PeerIdentity>>,
) -> Result<Json<HostConfig>, ApiError> {
    s.leader().standby_as(&actor(peer.as_ref())).await.map(Json)
}

#[utoipa::path(post, operation_id = "host_activate", path = "/v1/host/activate", tag = "host", security(("bearer" = [])), request_body = ActivateInput, responses((status = 200, body = HostConfig), (status = 409, body = crate::error::ErrorBody)))]
pub(crate) async fn activate(
    State(s): State<AppState>,
    peer: Option<Extension<crate::PeerIdentity>>,
    ApiJson(input): ApiJson<ActivateInput>,
) -> Result<Json<HostConfig>, ApiError> {
    s.leader()
        .activate_as(input.force, &actor(peer.as_ref()))
        .await
        .map(Json)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct HostUpdateStatus {
    pub state: HostUpdateState,
    pub tag: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HostUpdateState {
    #[default]
    Idle,
    Available,
    Running,
    Updated,
    Failed,
    RolledBack,
    RollbackFailed,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HostUpdateInput {
    pub tag: String,
}

pub(crate) fn update_status() -> Option<HostUpdateStatus> {
    if !cfg!(target_os = "linux") || !Path::new(UPDATE_SOCKET).exists() {
        return None;
    }
    let status = std::fs::read(UPDATE_STATUS)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    Some(status.unwrap_or_default())
}

#[utoipa::path(post, operation_id = "host_update", path = "/v1/host/update", tag = "host", security(("bearer" = [])), request_body = HostUpdateInput, responses((status = 202), (status = 409, body = crate::error::ErrorBody)))]
pub(crate) async fn update(
    State(s): State<AppState>,
    peer: Option<Extension<crate::PeerIdentity>>,
    input: Option<Json<HostUpdateInput>>,
) -> Result<StatusCode, ApiError> {
    s.leader().audit(
        &actor(peer.as_ref()),
        "host.update",
        &serde_json::json!({"phase":"requested"}),
    )?;
    if s.leader().config().mode != HostMode::Host || s.safe_mode() || update_status().is_none() {
        return Err(ApiError::conflict(
            "managed updates are unavailable; use the verified Linux host installer with --update",
        ));
    }
    let Some(Json(input)) = input else {
        return Err(ApiError::bad_request(
            "update requires the desktop release tag",
        ));
    };
    if input.tag.len() > 100
        || !input.tag.starts_with('v')
        || !input
            .tag
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+'))
    {
        return Err(ApiError::bad_request("use a published release tag"));
    }
    request_update(&input.tag).await?;
    Ok(StatusCode::ACCEPTED)
}

#[cfg(target_os = "linux")]
async fn request_update(tag: &str) -> Result<(), ApiError> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let accepted = tokio::time::timeout(Duration::from_secs(10), async {
        let mut socket = tokio::net::UnixStream::connect(UPDATE_SOCKET).await?;
        socket.write_all(tag.as_bytes()).await?;
        socket.write_all(b"\n").await?;
        socket.shutdown().await?;
        socket.read_u8().await
    })
    .await;
    if matches!(accepted, Ok(Ok(b'1'))) {
        return Ok(());
    }
    Err(ApiError::conflict(
        "the updater refused the request; check whether an update is already running or use the verified host installer",
    ))
}

#[cfg(not(target_os = "linux"))]
fn request_update(_: &str) -> std::future::Ready<Result<(), ApiError>> {
    std::future::ready(Err(ApiError::conflict(
        "managed updates require a Linux host",
    )))
}
