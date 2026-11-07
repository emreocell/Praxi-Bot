//! The peer network: endpoint lifecycle, accept loop, pairing, events, revoke.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use iroh::endpoint::{Connection, Incoming, presets};
use iroh::{Endpoint, EndpointAddr, PublicKey, RelayMode, RelayUrl};
use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio::sync::{Semaphore, broadcast};
use tokio::task::JoinHandle;

use crate::error::{PeerError, Result};
use crate::identity::{key_to_string, load_or_create_secret_key};
use crate::invite::{INVITE_TTL, Invite, InviteBook, MAX_INVITE_TTL, PairTicket, RedeemError};
use crate::proto::{
    ALPN_EVENTS, ALPN_PAIR, ALPN_RPC, CLOSE_NOT_PAIRED, CLOSE_REVOKED, CLOSE_SHUTDOWN, Event,
    LeaderInfo, MAX_FRAME, MAX_PAIR_FRAME, PROTOCOL_VERSION, PairRequest, PairResponse,
    RejectReason, read_frame, write_frame,
};
use crate::rpc::{DEFAULT_RPC_TIMEOUT, RpcClient, RpcHandler, map_conn_err, serve_connection};
use crate::store::{Device, DeviceStore, Role, now_secs};

/// Which relay servers to use for NAT traversal and fallback.
///
/// Terms of use of the public n0 relays and DNS discovery (rate limits,
/// availability, acceptable use) are **to verify** before shipping; see
/// <https://docs.iroh.computer> and the n0 relay documentation.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RelayConfig {
    /// The public n0 relays plus n0 DNS/pkarr address lookup.
    #[default]
    Default,
    /// A self-hosted relay; no external address lookup is used.
    Custom(RelayUrl),
    /// No relay and no address lookup: direct IP connectivity only
    /// (LAN, tests).
    Disabled,
}

impl std::str::FromStr for RelayConfig {
    type Err = PeerError;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "default" => Ok(Self::Default),
            "none" | "disabled" => Ok(Self::Disabled),
            url => url
                .parse::<RelayUrl>()
                .map(Self::Custom)
                .map_err(|e| PeerError::Protocol(format!("bad relay (default|none|<url>): {e}"))),
        }
    }
}

/// A protocol served only after the remote key has been paired.
#[async_trait]
pub trait ProtocolHandler: Send + Sync {
    /// Serve a connection from the authenticated paired device.
    /// Return when finished; revoke closes the connection while this runs.
    async fn handle(&self, conn: Connection, device: Device);
}

impl std::fmt::Debug for dyn ProtocolHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProtocolHandler")
    }
}

/// Configuration of a [`PeerNetwork`].
#[derive(Debug, Clone)]
pub struct PeerConfig {
    /// Directory holding `identity.key` and `devices.json`.
    pub data_dir: PathBuf,
    /// Relay selection.
    pub relay: RelayConfig,
    /// Bind the UDP socket to this address instead of all interfaces.
    pub bind_addr: Option<SocketAddr>,
    /// Role of this side, announced when pairing.
    pub local_role: Role,
    /// Display name of this side, announced when pairing.
    pub display_name: String,
    /// Capabilities this side offers (e.g. `desktop`, `browser`).
    pub capabilities: Vec<String>,
    /// Per-request RPC timeout (client and handler side).
    pub rpc_timeout: Duration,
    /// Additional paired-only ALPN handlers. Built-in ALPNs may not be replaced.
    pub extra_protocols: Vec<(Vec<u8>, Arc<dyn ProtocolHandler>)>,
}

impl PeerConfig {
    /// Defaults for a core (`desktop` role) storing its files in `data_dir`.
    #[must_use]
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            relay: RelayConfig::Default,
            bind_addr: None,
            local_role: Role::Desktop,
            display_name: "core".into(),
            capabilities: Vec::new(),
            rpc_timeout: DEFAULT_RPC_TIMEOUT,
            extra_protocols: Vec::new(),
        }
    }
}

/// An event received from a peer on `bot/events/1`.
#[derive(Debug, Clone)]
pub struct PeerEvent {
    /// The paired peer that sent it (or that just paired, for `paired`).
    pub peer: PublicKey,
    /// The event itself.
    pub event: Event,
    /// Leader metadata from this event, if advertised.
    pub leader: Option<LeaderInfo>,
}

/// State of the heartbeat link to a core (see [`PeerNetwork::spawn_heartbeat`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkState {
    Connected,
    Disconnected(String),
    /// The core no longer recognises this key; retrying stopped.
    Revoked(String),
}

/// Heartbeat timing.
#[derive(Debug, Clone, Copy)]
pub struct HeartbeatConfig {
    pub interval: Duration,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
    /// Give up a single connection attempt after this long.
    pub connect_timeout: Duration,
}

impl Default for HeartbeatConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(15),
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(30),
            connect_timeout: Duration::from_secs(10),
        }
    }
}

pub(crate) struct Inner {
    endpoint: Endpoint,
    config: PeerConfig,
    store: DeviceStore,
    invites: InviteBook,
    handler: Option<Arc<dyn RpcHandler>>,
    conns: Mutex<Vec<(PublicKey, Connection)>>,
    last_seen: Mutex<Vec<(PublicKey, Instant)>>,
    events: broadcast::Sender<PeerEvent>,
    leader: Mutex<Option<LeaderInfo>>,
    peer_leaders: Mutex<HashMap<PublicKey, LeaderInfo>>,
    pair_slots: Semaphore,
    pair_limits: Mutex<HashMap<PublicKey, PairBucket>>,
}

impl Inner {
    /// Dial a paired peer on `alpn` using its stored address.
    pub(crate) async fn dial(&self, peer: &PublicKey, alpn: &[u8]) -> Result<Connection> {
        let dev = self
            .store
            .get(peer)
            .ok_or_else(|| PeerError::UnknownDevice(key_to_string(peer)))?;
        let addr = dev.addr.unwrap_or_else(|| EndpointAddr::new(*peer));
        let conn = self
            .endpoint
            .connect(addr, alpn)
            .await
            .map_err(|e| connect_err(&e))?;
        self.track(peer, &conn)?;
        Ok(conn)
    }

    // Use the same lock as revoke, including a second paired-key check after dial.
    fn track(&self, peer: &PublicKey, conn: &Connection) -> Result<Device> {
        let mut conns = self.conns.lock();
        let Some(device) = self.store.get(peer) else {
            conn.close(CLOSE_NOT_PAIRED.into(), b"not paired");
            return Err(PeerError::UnknownDevice(key_to_string(peer)));
        };
        conns.retain(|(_, c)| c.close_reason().is_none());
        conns.push((*peer, conn.clone()));
        Ok(device)
    }
}

fn connect_err(e: &impl std::fmt::Display) -> PeerError {
    let s = e.to_string();
    if s.to_ascii_lowercase().contains("alpn") {
        PeerError::Connection(format!(
            "{s} (the peer does not speak this protocol version; update the older side)"
        ))
    } else {
        PeerError::Connection(s)
    }
}

/// A running peer endpoint: accepts paired peers, pairs new ones, dials known ones.
#[derive(Clone)]
pub struct PeerNetwork {
    inner: Arc<Inner>,
    accept_task: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl PeerNetwork {
    /// Bind the endpoint (creating the persistent identity on first run) and
    /// start accepting connections.
    ///
    /// `handler` serves incoming RPC calls; pass `None` on a pure core that
    /// only dials out.
    ///
    /// # Errors
    ///
    /// Fails if the identity or device list cannot be read or the endpoint
    /// cannot bind.
    pub async fn start(config: PeerConfig, handler: Option<Arc<dyn RpcHandler>>) -> Result<Self> {
        let mut alpns = vec![ALPN_PAIR.to_vec(), ALPN_RPC.to_vec(), ALPN_EVENTS.to_vec()];
        for (alpn, _) in &config.extra_protocols {
            if alpn.is_empty() || alpn.len() > 255 || alpns.contains(alpn) {
                return Err(PeerError::Protocol(
                    "invalid or duplicate extra ALPN".into(),
                ));
            }
            alpns.push(alpn.clone());
        }
        let key = load_or_create_secret_key(&config.data_dir.join("identity.key"))?;
        let store = DeviceStore::open(config.data_dir.join("devices.json"))?;
        let builder = match &config.relay {
            RelayConfig::Default => Endpoint::builder(presets::N0),
            RelayConfig::Custom(url) => {
                Endpoint::builder(presets::Minimal).relay_mode(RelayMode::custom([url.clone()]))
            }
            RelayConfig::Disabled => {
                Endpoint::builder(presets::Minimal).relay_mode(RelayMode::Disabled)
            }
        };
        let mut builder = builder.secret_key(key).alpns(alpns);
        if let Some(addr) = config.bind_addr {
            builder = builder
                .bind_addr(addr)
                .map_err(|e| PeerError::Endpoint(e.to_string()))?;
        }
        let endpoint = builder
            .bind()
            .await
            .map_err(|e| PeerError::Endpoint(e.to_string()))?;
        let (events, _) = broadcast::channel(256);
        let inner = Arc::new(Inner {
            endpoint,
            config,
            store,
            invites: InviteBook::default(),
            handler,
            conns: Mutex::new(Vec::new()),
            last_seen: Mutex::new(Vec::new()),
            events,
            leader: Mutex::new(None),
            peer_leaders: Mutex::new(HashMap::new()),
            pair_slots: Semaphore::new(8),
            pair_limits: Mutex::new(HashMap::new()),
        });
        let task = tokio::spawn(accept_loop(inner.clone()));
        Ok(Self {
            inner,
            accept_task: Arc::new(Mutex::new(Some(task))),
        })
    }

    /// This install's public key.
    #[must_use]
    pub fn id(&self) -> PublicKey {
        self.inner.endpoint.id()
    }

    /// The current dialable address.
    #[must_use]
    pub fn addr(&self) -> EndpointAddr {
        self.inner.endpoint.addr()
    }

    /// All paired devices.
    #[must_use]
    pub fn devices(&self) -> Vec<Device> {
        self.inner.store.list()
    }

    /// A paired device by key.
    #[must_use]
    pub fn device(&self, key: &PublicKey) -> Option<Device> {
        self.inner.store.get(key)
    }

    /// Subscribe to events from peers (heartbeats, `paired`).
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<PeerEvent> {
        self.inner.events.subscribe()
    }

    /// Whether `peer` currently has an open events link to us.
    #[must_use]
    pub fn is_connected(&self, peer: &PublicKey) -> bool {
        self.inner
            .conns
            .lock()
            .iter()
            .any(|(id, c)| id == peer && c.alpn() == ALPN_EVENTS && c.close_reason().is_none())
    }

    /// When we last heard from `peer` on the events link.
    #[must_use]
    pub fn last_seen(&self, peer: &PublicKey) -> Option<Instant> {
        self.inner
            .last_seen
            .lock()
            .iter()
            .find(|(id, _)| id == peer)
            .map(|(_, t)| *t)
    }

    /// Set the metadata included in subsequent heartbeats; no policy is applied.
    pub fn set_leader_info(&self, leader: Option<LeaderInfo>) {
        *self.inner.leader.lock() = leader;
    }

    /// Latest leader metadata received from a paired peer's heartbeat.
    #[must_use]
    pub fn peer_leader(&self, peer: &PublicKey) -> Option<LeaderInfo> {
        self.inner.peer_leaders.lock().get(peer).copied()
    }

    /// Dial a paired peer on a protocol using its stored address.
    ///
    /// # Errors
    /// Returns `UnknownDevice` for unpaired peers, or a connection error.
    pub async fn connect(&self, peer: &PublicKey, alpn: &[u8]) -> Result<Connection> {
        self.inner.dial(peer, alpn).await
    }

    /// Create a one-time invite (valid 2 minutes, single use) for a joiner
    /// taking `role`.
    ///
    /// With a relay configured this first waits (up to 10 s) for the endpoint
    /// to be reachable so the ticket contains the relay URL.
    pub async fn create_invite(&self, role: Role) -> Invite {
        self.create_invite_with_ttl(role, INVITE_TTL).await
    }

    /// [`create_invite`](Self::create_invite) with a custom validity window,
    /// clamped to 30 minutes. The returned invite reports the effective TTL.
    ///
    /// # Panics
    ///
    /// Never in practice.
    pub async fn create_invite_with_ttl(&self, role: Role, ttl: Duration) -> Invite {
        let ttl = ttl.min(MAX_INVITE_TTL);
        if self.inner.config.relay != RelayConfig::Disabled {
            let _ =
                tokio::time::timeout(Duration::from_secs(10), self.inner.endpoint.online()).await;
        }
        let (secret, code) = self.inner.invites.create(role, ttl);
        let ticket = PairTicket {
            addr: self.addr(),
            secret,
            code: code.clone(),
            role,
        };
        Invite {
            ticket: ticket.encode(),
            code,
            role,
            ttl,
        }
    }

    /// Join a core: dial the ticket's address, present the secret and store
    /// the core as a paired device. Returns the stored core device.
    ///
    /// # Errors
    ///
    /// Fails if the ticket's role differs from this side's role, the core is
    /// unreachable or rejects the invite (expired, used, wrong code), or the
    /// protocol versions differ.
    pub async fn pair(&self, ticket: &PairTicket) -> Result<Device> {
        let cfg = &self.inner.config;
        if ticket.role != cfg.local_role {
            return Err(PeerError::PairingRejected(format!(
                "this invite is for role '{}' but this install is '{}'",
                ticket.role, cfg.local_role
            )));
        }
        let conn = self
            .inner
            .endpoint
            .connect(ticket.addr.clone(), ALPN_PAIR)
            .await
            .map_err(|e| connect_err(&e))?;
        if conn.remote_id() != ticket.addr.id {
            return Err(PeerError::Connection(
                "remote key does not match the ticket".into(),
            ));
        }
        let (mut send, mut recv) = conn
            .open_bi()
            .await
            .map_err(|e| PeerError::Connection(e.to_string()))?;
        let req = PairRequest {
            protocol_version: PROTOCOL_VERSION,
            secret: ticket.secret.clone(),
            code: ticket.code.clone(),
            name: cfg.display_name.clone(),
            role: cfg.local_role,
            capabilities: cfg.capabilities.clone(),
            addr: self.addr(),
        };
        let body = serde_json::to_vec(&req).map_err(|e| PeerError::Protocol(e.to_string()))?;
        write_frame(&mut send, &body, MAX_PAIR_FRAME).await?;
        send.finish()
            .map_err(|e| PeerError::Connection(e.to_string()))?;
        let bytes = tokio::time::timeout(
            Duration::from_secs(15),
            read_frame(&mut recv, MAX_PAIR_FRAME),
        )
        .await
        .map_err(|_| PeerError::Timeout)??
        .ok_or_else(|| PeerError::Connection("core closed without answering".into()))?;
        let resp: PairResponse = serde_json::from_slice(&bytes)
            .map_err(|e| PeerError::Protocol(format!("bad pairing response: {e}")))?;
        conn.close(CLOSE_SHUTDOWN.into(), b"paired");
        match resp {
            PairResponse::Rejected {
                reason,
                protocol_version,
                message,
            } => {
                if reason == RejectReason::VersionMismatch {
                    Err(PeerError::VersionMismatch {
                        local: PROTOCOL_VERSION,
                        remote: protocol_version,
                    })
                } else {
                    Err(PeerError::PairingRejected(message))
                }
            }
            PairResponse::Accepted {
                protocol_version,
                name,
                role,
                capabilities,
                addr,
            } => {
                if protocol_version != PROTOCOL_VERSION {
                    return Err(PeerError::VersionMismatch {
                        local: PROTOCOL_VERSION,
                        remote: protocol_version,
                    });
                }
                let id = ticket.addr.id;
                let addr = if addr.id == id {
                    addr
                } else {
                    ticket.addr.clone()
                };
                let device = Device {
                    id: key_to_string(&id),
                    name: clip(&name),
                    role,
                    capabilities,
                    addr: Some(addr),
                    paired_at: now_secs(),
                };
                self.inner.store.upsert(device.clone())?;
                Ok(device)
            }
        }
    }

    /// Revoke a device: commit the removal, then close its connections.
    ///
    /// # Errors
    ///
    /// Fails if the device list cannot be persisted. Sockets stay up until
    /// that commit has swapped the in-memory list.
    pub fn revoke(&self, peer: &PublicKey) -> Result<bool> {
        let mut conns = self.inner.conns.lock();
        let removed = self.inner.store.remove(peer)?;
        conns.retain(|(id, conn)| {
            if id == peer {
                conn.close(CLOSE_REVOKED.into(), b"revoked");
                false
            } else {
                true
            }
        });
        self.inner.peer_leaders.lock().remove(peer);
        Ok(removed)
    }

    /// An RPC client for a paired peer.
    ///
    /// # Errors
    ///
    /// [`PeerError::UnknownDevice`] if the peer is not paired.
    pub fn rpc_client(&self, peer: &PublicKey) -> Result<RpcClient> {
        if self.inner.store.get(peer).is_none() {
            return Err(PeerError::UnknownDevice(key_to_string(peer)));
        }
        Ok(RpcClient::new(
            self.inner.clone(),
            *peer,
            self.inner.config.rpc_timeout,
        ))
    }

    /// Keep an events link to `core` alive: connect, send heartbeats, and
    /// reconnect with exponential backoff when it drops. Stops by itself if
    /// the core revokes this key.
    #[must_use]
    pub fn spawn_heartbeat(
        &self,
        core: PublicKey,
        cfg: HeartbeatConfig,
        on_state: Arc<dyn Fn(LinkState) + Send + Sync>,
    ) -> JoinHandle<()> {
        let inner = self.inner.clone();
        tokio::spawn(async move { heartbeat_loop(inner, core, cfg, on_state).await })
    }

    /// Stop accepting, close all connections and the endpoint.
    pub async fn shutdown(&self) {
        self.inner.endpoint.close().await;
        if let Some(t) = self.accept_task.lock().take() {
            t.abort();
        }
    }
}

fn clip(name: &str) -> String {
    name.chars().filter(|c| !c.is_control()).take(64).collect()
}

// ── Accept side ───────────────────────────────────────────────────────────────

async fn accept_loop(inner: Arc<Inner>) {
    while let Some(incoming) = inner.endpoint.accept().await {
        let inner = inner.clone();
        tokio::spawn(async move {
            let _ = handle_incoming(inner, incoming).await;
        });
    }
}

async fn handle_incoming(inner: Arc<Inner>, incoming: Incoming) -> Result<()> {
    let mut accepting = incoming
        .accept()
        .map_err(|e| PeerError::Connection(e.to_string()))?;
    let alpn = accepting
        .alpn()
        .await
        .map_err(|e| PeerError::Connection(e.to_string()))?;
    let conn = accepting
        .await
        .map_err(|e| PeerError::Connection(e.to_string()))?;
    let remote = conn.remote_id();

    if alpn == ALPN_PAIR {
        let Ok(_permit) = inner.pair_slots.try_acquire() else {
            conn.close(429u32.into(), b"pairing busy");
            return Ok(());
        };
        if !allow_pair(&inner.pair_limits, remote) {
            conn.close(429u32.into(), b"pairing rate limit");
            return Ok(());
        }
        let result =
            tokio::time::timeout(Duration::from_secs(15), handle_pair(&inner, &conn, remote))
                .await
                .unwrap_or(Err(PeerError::Timeout));
        if result.is_err() {
            conn.close(400u32.into(), b"pairing failed");
        }
        return result;
    }
    // Everything else requires a paired key, checked right after the handshake.
    let Ok(device) = inner.track(&remote, &conn) else {
        conn.close(CLOSE_NOT_PAIRED.into(), b"not paired");
        let _ = tokio::time::timeout(Duration::from_secs(2), conn.closed()).await;
        return Ok(());
    };
    if alpn == ALPN_RPC {
        if let Some(h) = &inner.handler {
            serve_connection(conn.clone(), device, h.clone(), inner.config.rpc_timeout).await;
        } else {
            conn.close(404u32.into(), b"this install does not serve rpc");
        }
    } else if alpn == ALPN_EVENTS {
        serve_events(&inner, &conn, remote).await;
    } else if let Some((_, handler)) = inner
        .config
        .extra_protocols
        .iter()
        .find(|(a, _)| *a == alpn)
    {
        tokio::select! {
            () = handler.handle(conn.clone(), device) => {}
            _ = conn.closed() => {}
        }
        // A handler may return after handing the connection to another task.
        let _ = conn.closed().await;
    } else {
        conn.close(400u32.into(), b"unknown protocol");
    }
    let id = conn.stable_id();
    inner.conns.lock().retain(|(_, c)| c.stable_id() != id);
    Ok(())
}

async fn handle_pair(inner: &Arc<Inner>, conn: &Connection, remote: PublicKey) -> Result<()> {
    let (mut send, mut recv) = conn
        .accept_bi()
        .await
        .map_err(|e| PeerError::Connection(e.to_string()))?;
    let bytes = tokio::time::timeout(
        Duration::from_secs(15),
        read_frame(&mut recv, MAX_PAIR_FRAME),
    )
    .await
    .map_err(|_| PeerError::Timeout)??
    .ok_or_else(|| PeerError::Protocol("empty pairing request".into()))?;
    let response = match serde_json::from_slice::<PairRequest>(&bytes) {
        Ok(req) => decide_pairing(inner, remote, req),
        Err(e) => rejected(
            RejectReason::InvalidInvite,
            format!("malformed pairing request: {e}"),
        ),
    };
    let accepted = matches!(response, PairResponse::Accepted { .. });
    let body = serde_json::to_vec(&response).map_err(|e| PeerError::Protocol(e.to_string()))?;
    write_frame(&mut send, &body, MAX_PAIR_FRAME).await?;
    let _ = send.finish();
    // Let the joiner read the answer and close first.
    let _ = tokio::time::timeout(Duration::from_secs(5), conn.closed()).await;
    if accepted {
        let _ = inner.events.send(PeerEvent {
            peer: remote,
            leader: None,
            event: Event {
                kind: "paired".into(),
                ts_ms: now_secs() * 1000,
                data: Value::Null,
                leader: None,
            },
        });
    }
    Ok(())
}

fn rejected(reason: RejectReason, message: impl Into<String>) -> PairResponse {
    PairResponse::Rejected {
        reason,
        protocol_version: PROTOCOL_VERSION,
        message: message.into(),
    }
}

fn decide_pairing(inner: &Inner, remote: PublicKey, req: PairRequest) -> PairResponse {
    if req.protocol_version != PROTOCOL_VERSION {
        return rejected(
            RejectReason::VersionMismatch,
            format!(
                "incompatible protocol version: core v{PROTOCOL_VERSION}, joiner v{}",
                req.protocol_version
            ),
        );
    }
    match inner.invites.redeem(&req.secret, &req.code, req.role) {
        Ok(()) => {}
        Err(RedeemError::Expired) => {
            return rejected(
                RejectReason::Expired,
                "the invite has expired; create a new one",
            );
        }
        Err(RedeemError::RoleMismatch) => {
            return rejected(
                RejectReason::RoleMismatch,
                "the invite is for a different role",
            );
        }
        Err(RedeemError::Invalid) => {
            return rejected(
                RejectReason::InvalidInvite,
                "invalid or already used invite",
            );
        }
    }
    // The identity is the authenticated connection key, never a claimed one.
    let addr = if req.addr.id == remote {
        req.addr
    } else {
        EndpointAddr::new(remote)
    };
    let device = Device {
        id: key_to_string(&remote),
        name: clip(&req.name),
        role: req.role,
        capabilities: req.capabilities,
        addr: Some(addr),
        paired_at: now_secs(),
    };
    if let Err(e) = inner.store.upsert(device) {
        return rejected(
            RejectReason::Internal,
            format!("could not store device: {e}"),
        );
    }
    PairResponse::Accepted {
        protocol_version: PROTOCOL_VERSION,
        name: inner.config.display_name.clone(),
        role: inner.config.local_role,
        capabilities: inner.config.capabilities.clone(),
        addr: inner.endpoint.addr(),
    }
}

async fn serve_events(inner: &Arc<Inner>, conn: &Connection, remote: PublicKey) {
    while let Ok(mut recv) = conn.accept_uni().await {
        let inner = inner.clone();
        tokio::spawn(async move {
            while let Ok(Some(bytes)) = read_frame(&mut recv, MAX_FRAME / 32).await {
                let Ok(event) = serde_json::from_slice::<Event>(&bytes) else {
                    continue;
                };
                // Revoke and metadata updates share the registration lock.
                let conns = inner.conns.lock();
                if inner.store.get(&remote).is_none() {
                    return;
                }
                if event.kind == "heartbeat" {
                    let mut leaders = inner.peer_leaders.lock();
                    if let Some(leader) = event.leader {
                        leaders.insert(remote, leader);
                    } else {
                        leaders.remove(&remote);
                    }
                }
                note_seen(&inner, remote);
                let addr = (event.kind == "heartbeat")
                    .then(|| event.data.get("addr"))
                    .flatten()
                    .and_then(|a| serde_json::from_value::<EndpointAddr>(a.clone()).ok())
                    .filter(|a| a.id == remote);
                if let Some(addr) = addr {
                    let _ = inner.store.update_addr(&remote, addr);
                }
                let _ = inner.events.send(PeerEvent {
                    peer: remote,
                    leader: event.leader,
                    event,
                });
                drop(conns);
            }
        });
    }
}

fn note_seen(inner: &Inner, peer: PublicKey) {
    let mut guard = inner.last_seen.lock();
    match guard.iter_mut().find(|(id, _)| *id == peer) {
        Some(slot) => slot.1 = Instant::now(),
        None => guard.push((peer, Instant::now())),
    }
}

// ── Heartbeat link (node side) ────────────────────────────────────────────────

async fn heartbeat_loop(
    inner: Arc<Inner>,
    core: PublicKey,
    cfg: HeartbeatConfig,
    on_state: Arc<dyn Fn(LinkState) + Send + Sync>,
) {
    let mut backoff = cfg.initial_backoff;
    loop {
        let dialed = tokio::time::timeout(cfg.connect_timeout, inner.dial(&core, ALPN_EVENTS))
            .await
            .unwrap_or_else(|_| Err(PeerError::Timeout));
        match dialed {
            Ok(conn) => match run_link(&inner, &conn, cfg.interval, &on_state).await {
                LinkEnd::Revoked(reason) => {
                    on_state(LinkState::Revoked(reason));
                    return;
                }
                LinkEnd::Dropped(reason) => {
                    on_state(LinkState::Disconnected(reason));
                    backoff = cfg.initial_backoff;
                }
            },
            Err(PeerError::UnknownDevice(m)) => {
                on_state(LinkState::Revoked(format!("core {m} is no longer paired")));
                return;
            }
            Err(e) => on_state(LinkState::Disconnected(e.to_string())),
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(cfg.max_backoff);
    }
}

enum LinkEnd {
    Revoked(String),
    Dropped(String),
}

async fn run_link(
    inner: &Inner,
    conn: &Connection,
    interval: Duration,
    on_state: &Arc<dyn Fn(LinkState) + Send + Sync>,
) -> LinkEnd {
    let mut send = match conn.open_uni().await {
        Ok(s) => s,
        Err(e) => return link_end(conn, &e),
    };
    on_state(LinkState::Connected);
    let mut seq: u64 = 0;
    loop {
        seq += 1;
        let event = Event {
            leader: *inner.leader.lock(),
            kind: "heartbeat".into(),
            ts_ms: now_secs() * 1000,
            data: json!({ "seq": seq, "addr": inner.endpoint.addr() }),
        };
        let body = serde_json::to_vec(&event).unwrap_or_default();
        if let Err(e) = write_frame(&mut send, &body, MAX_FRAME / 32).await {
            return link_end(conn, &e);
        }
        tokio::select! {
            () = tokio::time::sleep(interval) => {}
            e = conn.closed() => return link_end(conn, &e),
        }
    }
}

fn link_end(conn: &Connection, e: &impl std::fmt::Display) -> LinkEnd {
    match map_conn_err(conn, e) {
        PeerError::NotPaired(m) => LinkEnd::Revoked(m),
        other => LinkEnd::Dropped(other.to_string()),
    }
}

const PAIR_LIMIT_KEYS: usize = 4096;

struct PairBucket {
    tokens: f64,
    updated: Instant,
}

fn allow_pair(limits: &Mutex<HashMap<PublicKey, PairBucket>>, remote: PublicKey) -> bool {
    let mut limits = limits.lock();
    // Sample under the lock. A timestamp taken before the lock can be older
    // than one another handshake already stored, and `duration_since` panics.
    let now = Instant::now();
    allow_pair_at(&mut limits, remote, now)
}

fn allow_pair_at(
    limits: &mut HashMap<PublicKey, PairBucket>,
    remote: PublicKey,
    now: Instant,
) -> bool {
    limits.retain(|_, bucket| {
        now.saturating_duration_since(bucket.updated) < Duration::from_secs(60)
    });
    if limits.len() >= PAIR_LIMIT_KEYS && !limits.contains_key(&remote) {
        let oldest = limits
            .iter()
            .min_by_key(|(_, bucket)| bucket.updated)
            .map(|(key, _)| *key);
        if let Some(oldest) = oldest {
            limits.remove(&oldest);
        }
    }
    let bucket = limits.entry(remote).or_insert(PairBucket {
        tokens: 5.0,
        updated: now,
    });
    let elapsed = now.saturating_duration_since(bucket.updated).as_secs_f64();
    bucket.tokens = (bucket.tokens + elapsed / 12.0).min(5.0);
    bucket.updated = now;
    if bucket.tokens < 1.0 {
        return false;
    }
    bucket.tokens -= 1.0;
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{ALPN_API, RpcError};

    use async_trait::async_trait;

    struct Park;

    #[async_trait]
    impl ProtocolHandler for Park {
        async fn handle(&self, conn: Connection, _device: Device) {
            conn.closed().await;
        }
    }

    struct NopRpc;

    #[async_trait]
    impl RpcHandler for NopRpc {
        async fn handle(
            &self,
            _peer: &Device,
            _method: &str,
            _params: Value,
        ) -> std::result::Result<Value, RpcError> {
            Ok(Value::Null)
        }
    }

    fn loopback(dir: &std::path::Path, name: &str) -> PeerConfig {
        let mut cfg = PeerConfig::new(dir.join(name));
        cfg.relay = RelayConfig::Disabled;
        cfg.bind_addr = Some(SocketAddr::from(([127, 0, 0, 1], 0)));
        cfg.local_role = Role::Desktop;
        cfg
    }

    async fn expect_not_paired(core: &PeerNetwork, joiner: &PeerNetwork, alpn: &[u8]) {
        let rejected = joiner
            .inner
            .endpoint
            .connect(core.addr(), alpn)
            .await
            .unwrap();
        let closed = tokio::time::timeout(Duration::from_secs(5), rejected.closed())
            .await
            .unwrap();
        assert!(
            matches!(closed, iroh::endpoint::ConnectionError::ApplicationClosed(ref c) if c.error_code.into_inner() == u64::from(CLOSE_NOT_PAIRED)),
            "{closed}"
        );
    }

    #[test]
    fn pairing_buckets_refill_and_isolate_keys() {
        let mut limits = HashMap::new();
        let a = iroh::SecretKey::generate().public();
        let b = iroh::SecretKey::generate().public();
        let now = Instant::now();
        for _ in 0..5 {
            assert!(allow_pair_at(&mut limits, a, now));
        }
        assert!(!allow_pair_at(&mut limits, a, now));
        assert!(allow_pair_at(&mut limits, b, now));
        assert!(!allow_pair_at(
            &mut limits,
            a,
            now + Duration::from_secs(11)
        ));
        assert!(allow_pair_at(&mut limits, a, now + Duration::from_secs(12)));
        assert!(!allow_pair_at(
            &mut limits,
            a,
            now + Duration::from_secs(12)
        ));
        for _ in 0..5 {
            assert!(allow_pair_at(&mut limits, a, now + Duration::from_secs(72)));
        }
        assert!(!allow_pair_at(
            &mut limits,
            a,
            now + Duration::from_secs(72)
        ));
    }

    #[test]
    fn pairing_bucket_storage_is_bounded() {
        let now = Instant::now();
        let mut limits = HashMap::new();
        for _ in 0..PAIR_LIMIT_KEYS {
            limits.insert(
                iroh::SecretKey::generate().public(),
                PairBucket {
                    tokens: 0.0,
                    updated: now,
                },
            );
        }
        let key = iroh::SecretKey::generate().public();
        assert!(allow_pair_at(&mut limits, key, now));
        assert_eq!(limits.len(), PAIR_LIMIT_KEYS);
        assert!(limits.contains_key(&key));
        assert!(allow_pair_at(
            &mut limits,
            key,
            now + Duration::from_secs(60)
        ));
        assert_eq!(limits.len(), 1);
        assert!(limits.contains_key(&key));
    }

    #[test]
    fn concurrent_pair_attempts_do_not_panic_when_a_bucket_is_newer() {
        let stale = Instant::now();
        let keys: Vec<_> = (0..32)
            .map(|_| iroh::SecretKey::generate().public())
            .collect();
        let mut skewed = HashMap::new();
        skewed.insert(
            keys[0],
            PairBucket {
                tokens: 5.0,
                updated: stale + Duration::from_secs(30),
            },
        );
        // A clock captured before the lock is behind the stored stamp.
        let _ = allow_pair_at(&mut skewed, keys[0], stale);

        let limits = Mutex::new(HashMap::new());
        limits.lock().insert(
            keys[0],
            PairBucket {
                tokens: 5.0,
                updated: Instant::now() + Duration::from_secs(30),
            },
        );
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    for _ in 0..40 {
                        for key in &keys {
                            let _ = allow_pair(&limits, *key);
                        }
                    }
                });
            }
        });
    }

    #[test]
    fn pairing_flood_evicts_oldest_bucket_when_table_is_full() {
        let now = Instant::now();
        let mut limits = HashMap::new();
        let mut keys = Vec::with_capacity(PAIR_LIMIT_KEYS);
        for i in 0..PAIR_LIMIT_KEYS {
            let key = iroh::SecretKey::generate().public();
            limits.insert(
                key,
                PairBucket {
                    tokens: 0.0,
                    updated: now + Duration::from_millis(u64::try_from(i).unwrap()),
                },
            );
            keys.push(key);
        }
        let oldest = keys[0];
        let pinned = keys[PAIR_LIMIT_KEYS - 1];
        let denied_at = now
            + Duration::from_millis(u64::try_from(PAIR_LIMIT_KEYS - 1).unwrap())
            + Duration::from_secs(11);
        // A denied retry still refreshes `updated`, which used to pin every
        // resident key so an unseen joiner was refused forever.
        assert!(!allow_pair_at(&mut limits, pinned, denied_at));
        assert_eq!(limits.len(), PAIR_LIMIT_KEYS);
        assert!(limits.contains_key(&oldest));
        assert_eq!(limits.get(&pinned).unwrap().updated, denied_at);

        let mut admitted = Vec::new();
        for n in 0..128u64 {
            let fresh = iroh::SecretKey::generate().public();
            let at = denied_at + Duration::from_millis(n + 1);
            assert!(
                allow_pair_at(&mut limits, fresh, at),
                "unseen key refused from a full pairing table"
            );
            assert_eq!(limits.len(), PAIR_LIMIT_KEYS);
            admitted.push(fresh);
        }
        assert!(!limits.contains_key(&oldest));
        assert!(limits.contains_key(&pinned));
        for key in &admitted {
            assert!(limits.contains_key(key));
        }
    }

    #[tokio::test]
    async fn pairing_semaphore_rejects_busy_and_stalled_handshakes_release_slots() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = PeerConfig::new(dir.path().to_path_buf());
        cfg.relay = RelayConfig::Disabled;
        cfg.bind_addr = Some(SocketAddr::from(([127, 0, 0, 1], 0)));
        let net = PeerNetwork::start(cfg, None).await.unwrap();
        let permits = net.inner.pair_slots.acquire_many(7).await.unwrap();
        let raw = Endpoint::builder(presets::Minimal)
            .relay_mode(RelayMode::Disabled)
            .bind_addr(SocketAddr::from(([127, 0, 0, 1], 0)))
            .unwrap()
            .bind()
            .await
            .unwrap();
        let stalled = raw.connect(net.addr(), ALPN_PAIR).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while net.inner.pair_slots.available_permits() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let busy = raw.connect(net.addr(), ALPN_PAIR).await.unwrap();
        let closed = tokio::time::timeout(Duration::from_secs(5), busy.closed())
            .await
            .unwrap();
        assert!(
            matches!(closed, iroh::endpoint::ConnectionError::ApplicationClosed(c) if c.error_code.into_inner() == 429)
        );
        let closed = tokio::time::timeout(Duration::from_secs(20), stalled.closed())
            .await
            .unwrap();
        assert!(
            matches!(closed, iroh::endpoint::ConnectionError::ApplicationClosed(c) if c.error_code.into_inner() == 400)
        );
        assert_eq!(net.inner.pair_slots.available_permits(), 1);
        drop(permits);
        assert_eq!(net.inner.pair_slots.available_permits(), 8);
        raw.close().await;
        net.shutdown().await;
    }

    #[tokio::test]
    async fn injected_persist_failure_keeps_memory_and_live_connections() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = loopback(dir.path(), "core");
        cfg.extra_protocols
            .push((ALPN_API.to_vec(), Arc::new(Park)));
        let core = PeerNetwork::start(cfg, Some(Arc::new(NopRpc)))
            .await
            .unwrap();
        let desktop = PeerNetwork::start(loopback(dir.path(), "desktop"), None)
            .await
            .unwrap();
        let invite = core.create_invite(Role::Desktop).await;
        desktop
            .pair(&PairTicket::parse(&invite.ticket).unwrap())
            .await
            .unwrap();

        let api = desktop
            .inner
            .endpoint
            .connect(core.addr(), ALPN_API)
            .await
            .unwrap();
        let rpc = desktop
            .inner
            .endpoint
            .connect(core.addr(), ALPN_RPC)
            .await
            .unwrap();
        let events = desktop
            .inner
            .endpoint
            .connect(core.addr(), ALPN_EVENTS)
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let tracked = {
                    let conns = core.inner.conns.lock();
                    conns
                        .iter()
                        .filter(|(id, conn)| *id == desktop.id() && conn.close_reason().is_none())
                        .count()
                };
                if tracked == 3 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        let path = core.inner.config.data_dir.join("devices.json");
        let before = std::fs::read(&path).unwrap();
        std::fs::create_dir(path.with_extension("json.tmp")).unwrap();
        let err = core.revoke(&desktop.id()).unwrap_err();
        assert!(matches!(err, PeerError::Storage(_)), "{err}");
        assert!(core.device(&desktop.id()).is_some());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(api.close_reason().is_none());
        assert!(rpc.close_reason().is_none());
        assert!(events.close_reason().is_none());
        assert_eq!(
            core.inner
                .conns
                .lock()
                .iter()
                .filter(|(id, conn)| *id == desktop.id() && conn.close_reason().is_none())
                .count(),
            3
        );

        let other = PeerNetwork::start(loopback(dir.path(), "other"), None)
            .await
            .unwrap();
        let invite = core.create_invite(Role::Desktop).await;
        let err = other
            .pair(&PairTicket::parse(&invite.ticket).unwrap())
            .await
            .unwrap_err();
        assert!(
            matches!(err, PeerError::PairingRejected(ref message) if message.contains("store")),
            "{err}"
        );
        assert!(core.device(&other.id()).is_none());
        assert_eq!(core.devices().len(), 1);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        expect_not_paired(&core, &other, ALPN_API).await;
        expect_not_paired(&core, &other, ALPN_RPC).await;
        expect_not_paired(&core, &other, ALPN_EVENTS).await;
        assert!(api.close_reason().is_none());
        assert!(rpc.close_reason().is_none());
        assert!(events.close_reason().is_none());

        other.shutdown().await;
        desktop.shutdown().await;
        core.shutdown().await;
    }
}
