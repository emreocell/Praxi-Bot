//! Persistent installation ownership and runtime leadership control.
use std::{
    path::{Path, PathBuf},
    sync::Arc,
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

pub const INSTALL_URL: &str =
    "https://github.com/emreocell/Praxi-Bot/releases/latest/download/install-linux.sh";
/// Epochs stay representable in TOML and bounded even for compromised peers.
/// At the ceiling, explicit force reuses the ceiling; equal owners still stop both.
pub const EPOCH_CEILING: u64 = 1_000_000_000;
pub const MAX_PEER_EPOCH_ADVANCE: u64 = 4096;
pub const UPDATE_UNIT: &str = "praxi-update.service";

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
        let limits = root.entry("limits").or_insert_with(|| toml::Value::Table(toml::Table::new()));
        let limits = limits.as_table_mut().ok_or_else(|| std::io::Error::other("limits must be a table"))?;
        for key in ["max_computer_runs", "browser_idle_seconds"] {
            limits.remove(key);
            if let Some(value) = typed.get("limits").and_then(|v| v.get(key)) {
                limits.insert(key.into(), value.clone());
            }
        }
        file.write_all(toml::to_string_pretty(&root).map_err(std::io::Error::other)?.as_bytes())?;
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

/// Shared handle for handover, API actions and conflict detection. Transitions are serialized.
#[derive(Clone)]
pub struct LeaderControl(Arc<LeaderInner>);
struct LeaderInner {
    data: PathBuf,
    config: Mutex<HostConfig>,
    transition: tokio::sync::Mutex<()>,
    state: WeakAppState,
    network: Mutex<Option<PeerNetwork>>,
    invites: Mutex<std::collections::VecDeque<std::time::Instant>>,
    events: tokio::sync::broadcast::Sender<HostEvent>,
}

impl LeaderControl {
    pub(crate) fn new(data: PathBuf, config: HostConfig, state: WeakAppState) -> Self {
        Self(Arc::new(LeaderInner {
            data,
            config: Mutex::new(config),
            transition: tokio::sync::Mutex::new(()),
            state,
            network: Mutex::new(None),
            invites: Mutex::default(),
            events: tokio::sync::broadcast::channel(64).0,
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

    /// Attach the already-running peer endpoint and publish our ownership state.
    pub fn set_network(&self, net: PeerNetwork) {
        let config = self.0.config.lock();
        net.set_leader_info(Some(config.wire()));
        *self.0.network.lock() = Some(net);
    }

    /// Release the endpoint during daemon shutdown (its API handler owns router state).
    pub fn detach_network(&self) {
        if let Some(net) = self.0.network.lock().take() {
            net.set_leader_info(None);
        }
    }

    async fn apply(&self, next: HostConfig) -> Result<HostConfig, ApiError> {
        let state = self
            .0
            .state
            .upgrade()
            .ok_or_else(|| ApiError::unavailable("daemon stopped"))?;
        if state.shutdown_token().is_cancelled() {
            return Err(ApiError::unavailable("daemon stopped"));
        }
        if next.runs_components() {
            next.save(&self.0.data).map_err(ApiError::internal)?;
            *self.0.config.lock() = next.clone();
            if let Some(net) = self.network() {
                net.set_leader_info(Some(next.wire()));
            }
            if let Err(error) = state.start_leader_components() {
                let mut idle = next.clone();
                idle.state = HostState::Standby;
                *self.0.config.lock() = idle.clone();
                if let Some(net) = self.network() {
                    net.set_leader_info(Some(idle.wire()));
                }
                state.stop_components().await;
                let saved = idle.save(&self.0.data);
                let _ = self.0.events.send(HostEvent::HostStateChanged(idle));
                saved.map_err(ApiError::internal)?;
                return Err(error);
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
        }
        Ok(next)
    }

    /// Linearize a synchronous automatic-run admission with ownership changes.
    pub(crate) fn with_active<T>(&self, action: impl FnOnce() -> T) -> Result<T, ApiError> {
        let config = self.0.config.lock();
        if !config.runs_components() {
            return Err(ApiError::conflict("this installation is standby"));
        }
        let result = action();
        drop(config);
        Ok(result)
    }

    /// Start the components permitted by current ownership without changing the epoch.
    /// # Errors
    /// Returns an unavailable daemon or errors opening the schedule store.
    pub async fn start(&self) -> Result<(), ApiError> {
        let _transition = self.0.transition.lock().await;
        let state = self
            .0
            .state
            .upgrade()
            .ok_or_else(|| ApiError::unavailable("daemon stopped"))?;
        state.start_leader_components()
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
        self.apply(next).await
    }

    /// Stop automatic components and persist standby ownership.
    /// # Errors
    /// Returns persistence errors or an unavailable daemon. Components still stop on write failure.
    pub async fn standby(&self) -> Result<HostConfig, ApiError> {
        let _transition = self.0.transition.lock().await;
        let mut next = self.config();
        next.state = HostState::Standby;
        self.apply(next).await
    }

    /// Explicitly take ownership with an epoch above all known epochs.
    /// # Errors
    /// Rejects unforced takeover of a known leader, exhausted epochs and persistence/startup failures.
    pub async fn activate(&self, force: bool) -> Result<HostConfig, ApiError> {
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
                    if !acceptable_peer_epoch(next.epoch, info.epoch) { continue; }
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
            return Err(ApiError::conflict("leader epoch ceiling reached; explicit force required"));
        }
        next.epoch = highest.saturating_add(1).min(EPOCH_CEILING);
        next.state = HostState::Active;
        next.leader = None;
        if next.mode == HostMode::Client {
            next.mode = HostMode::Standalone;
        }
        self.apply(next).await
    }

    /// Apply authenticated daemon heartbeat metadata. There is no automatic takeover.
    /// # Errors
    /// Returns persistence errors while demoting or an unavailable daemon.
    pub async fn observe(&self, peer: &str, info: LeaderInfo) -> Result<(), ApiError> {
        let _transition = self.0.transition.lock().await;
        let mut next = self.config();
        if !acceptable_peer_epoch(next.epoch, info.epoch) || info.state != LeaderState::Active || info.epoch < next.epoch {
            return Ok(());
        }
        let conflict = info.epoch == next.epoch && next.state == HostState::Active;
        if info.epoch > next.epoch || conflict {
            if next.state != HostState::Fresh {
                next.state = HostState::Standby;
            }
            next.epoch = info.epoch;
            next.leader = Some(peer.into());
            let result = self.apply(next).await;
            if conflict {
                self.emit_conflict("equal_epoch", Some(peer.into()));
            }
            result?;
        }
        Ok(())
    }

    /// Apply a conflict notification only for two active, equal-epoch owners.
    /// The caller must authenticate the sender as a Desktop or Host first.
    /// # Errors
    /// Returns ownership persistence or component shutdown errors.
    pub async fn observe_conflict(&self, peer: &str, info: LeaderInfo) -> Result<(), ApiError> {
        let _transition = self.0.transition.lock().await;
        let mut next = self.config();
        if next.state != HostState::Active || info.state != LeaderState::Active
            || info.epoch != next.epoch || !acceptable_peer_epoch(next.epoch, info.epoch) {
            return Ok(());
        }
        next.state = HostState::Standby;
        next.leader = Some(peer.into());
        self.apply(next).await?;
        self.emit_conflict("equal_epoch", Some(peer.into()));
        Ok(())
    }

    pub fn emit_conflict(&self, reason: &str, peer: Option<String>) {
        let event = HostConflict { reason: reason.into(), peer, epoch: self.config().epoch, notification: "Another installation is running this bot. Choose the active installation before resuming.".into() };
        let _ = self.0.events.send(HostEvent::HostConflict(event));
    }

    /// Notify after a terminal Telegram conflict; only the channel stops.
    /// # Errors
    /// Kept fallible for callers; no ownership persistence is performed.
    pub async fn telegram_conflict(&self) -> Result<(), ApiError> {
        self.emit_conflict("telegram_409", None);
        Ok(())
    }
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
    ApiJson(input): ApiJson<HostInviteInput>,
) -> Result<Json<HostInvite>, ApiError> {
    if peer.as_ref().is_some_and(|p| p.role != crate::PeerRole::Desktop)
        && !matches!(input.role, InviteRole::Phone)
    {
        return Err(ApiError::forbidden("only a local user or desktop may invite privileged devices"));
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
    s.leader().admit_invite()?;
    let invite = net
        .create_invite_with_ttl(input.role.peer_role(), Duration::from_secs(ttl))
        .await;
    let qr = qrcode::QrCode::new(invite.ticket.as_bytes()).map_err(ApiError::internal)?;
    let command = (matches!(input.role, InviteRole::Host)
        && s.leader().config().mode != HostMode::Host)
        .then(|| {
            format!(
                "curl -fsSL {INSTALL_URL} | sudo sh -s -- --join '{}'",
                invite.ticket
            )
        });
    Ok(Json(HostInvite {
        ticket: invite.ticket,
        code: invite.code,
        expires_at_ms: storage::now_ms()
            .saturating_add(i64::try_from(ttl * 1000).unwrap_or(i64::MAX)),
        qr_svg: qr.render::<qrcode::render::svg::Color>().build(),
        install_command: command,
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
    if peer.as_ref().is_some_and(|p| p.role == crate::PeerRole::Phone)
        && net.device(&key).is_some_and(|d| d.role != Role::Phone)
    {
        return Err(ApiError::forbidden("phones may revoke only phone devices"));
    }
    if !net.revoke(&key).map_err(ApiError::internal)? {
        return Err(ApiError::not_found("device"));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, operation_id = "host_standby", path = "/v1/host/standby", tag = "host", security(("bearer" = [])), responses((status = 200, body = HostConfig)))]
pub(crate) async fn standby(State(s): State<AppState>) -> Result<Json<HostConfig>, ApiError> {
    s.leader().standby().await.map(Json)
}

#[utoipa::path(post, operation_id = "host_activate", path = "/v1/host/activate", tag = "host", security(("bearer" = [])), request_body = ActivateInput, responses((status = 200, body = HostConfig), (status = 409, body = crate::error::ErrorBody)))]
pub(crate) async fn activate(
    State(s): State<AppState>,
    ApiJson(input): ApiJson<ActivateInput>,
) -> Result<Json<HostConfig>, ApiError> {
    s.leader().activate(input.force).await.map(Json)
}

#[utoipa::path(post, operation_id = "host_update", path = "/v1/host/update", tag = "host", security(("bearer" = [])), responses((status = 202), (status = 409, body = crate::error::ErrorBody)))]
pub(crate) async fn update(State(s): State<AppState>) -> Result<StatusCode, ApiError> {
    if std::env::consts::OS != "linux" || s.leader().config().mode != HostMode::Host {
        return Err(ApiError::conflict(
            "updates require Linux host mode and the praxi-update.service unit",
        ));
    }
    // Fixed executable and arguments only. The installer must grant exactly this unit.
    let result = tokio::process::Command::new("/usr/bin/sudo")
        .args(["-n", "/usr/bin/systemctl", "start", UPDATE_UNIT])
        .output()
        .await
        .map_err(|_| ApiError::conflict("host updater unavailable; install /usr/bin/sudo and the praxi-update.service sudoers rule"))?;
    if !result.status.success() {
        return Err(ApiError::conflict(
            "could not start praxi-update.service; install its narrowly scoped sudoers rule or run the host updater locally",
        ));
    }
    Ok(StatusCode::ACCEPTED)
}
