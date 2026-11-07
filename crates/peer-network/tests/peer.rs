//! Integration tests: two (or three) in-process endpoints, no relay, loopback only.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use desktop_api::{
    Action, ActionResult, Capabilities, DesktopBackend, DesktopError, Image, Quality, Region,
    Snapshot, TreeQuality, UiNode, WindowInfo,
    snapshot::{AppSpec, Pid, Rect},
};
use iroh::{Endpoint, endpoint::presets};
use peer_network::proto::{
    ALPN_PAIR, ALPN_RPC, MAX_FRAME, PairRequest, PairResponse, RejectReason, Request, read_frame,
    write_frame,
};
use peer_network::{
    DesktopRpcHandler, HeartbeatConfig, LinkState, PROTOCOL_VERSION, PairTicket, PeerConfig,
    PeerError, PeerNetwork, RelayConfig, RemoteDesktop, Role, RpcHandler,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

// ── Fixtures ──────────────────────────────────────────────────────────────────

struct FakeBackend;

fn window() -> WindowInfo {
    WindowInfo {
        id: 7,
        title: "Fake Notepad".into(),
        pid: 4242,
        bounds: Rect {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
        },
        focused: true,
    }
}

#[async_trait]
impl DesktopBackend for FakeBackend {
    async fn snapshot(&self, scope: Option<u64>) -> Result<Snapshot, DesktopError> {
        Ok(Snapshot {
            window: window(),
            nodes: vec![UiNode::new(
                0,
                "textbox",
                format!("scope={scope:?}"),
                Some("hello".into()),
                None,
                vec![],
            )],
            generation: 3,
            quality: TreeQuality::Weak,
        })
    }

    async fn act(&self, action: Action) -> Result<ActionResult, DesktopError> {
        match action {
            Action::TypeText { text } if text == "boom" => Err(DesktopError::ElementNotFound {
                index: 9,
                generation: 3,
            }),
            _ => Ok(ActionResult {
                dispatched: true,
                verified: Some(true),
                new_snapshot: None,
            }),
        }
    }

    async fn screenshot(&self, _region: Region, _quality: Quality) -> Result<Image, DesktopError> {
        Ok(Image {
            data: vec![1, 2, 3, 255, 0],
            width: 2,
            height: 1,
            mime_type: "image/png".into(),
        })
    }

    async fn windows(&self) -> Result<Vec<WindowInfo>, DesktopError> {
        Ok(vec![window()])
    }

    async fn launch(&self, app: AppSpec) -> Result<Pid, DesktopError> {
        if app.executable == "nope" {
            return Err(DesktopError::Unsupported("no such app".into()));
        }
        Ok(99)
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            platform: "fake".into(),
            accessibility_tree: true,
            screenshot: true,
            app_launch: true,
            has_display: false,
        }
    }
}

fn loopback() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

async fn net(
    dir: &tempfile::TempDir,
    name: &str,
    role: Role,
    handler: Option<Arc<dyn RpcHandler>>,
) -> PeerNetwork {
    let mut cfg = PeerConfig::new(dir.path().join(name));
    cfg.relay = RelayConfig::Disabled;
    cfg.bind_addr = Some(loopback());
    cfg.local_role = role;
    cfg.display_name = name.into();
    cfg.capabilities = vec!["desktop".into()];
    PeerNetwork::start(cfg, handler).await.unwrap()
}

fn fake_handler() -> Arc<dyn RpcHandler> {
    Arc::new(DesktopRpcHandler::new(Arc::new(FakeBackend)))
}

/// A core and a node that are already paired.
async fn paired(dir: &tempfile::TempDir) -> (PeerNetwork, PeerNetwork) {
    let core = net(dir, "core", Role::Desktop, None).await;
    let node = net(dir, "node", Role::Node, Some(fake_handler())).await;
    let invite = core.create_invite(Role::Node).await;
    let ticket = PairTicket::parse(&invite.ticket).unwrap();
    node.pair(&ticket).await.unwrap();
    (core, node)
}

// ── Pairing ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn pair_then_rpc_and_remote_desktop() {
    let dir = tempfile::tempdir().unwrap();
    let (core, node) = paired(&dir).await;

    // Both sides stored each other with the right role.
    let on_core = core.device(&node.id()).unwrap();
    assert_eq!(on_core.role, Role::Node);
    assert_eq!(on_core.name, "node");
    assert_eq!(on_core.capabilities, vec!["desktop".to_string()]);
    let on_node = node.device(&core.id()).unwrap();
    assert_eq!(on_node.role, Role::Desktop);

    // node.info / node.ping over the raw client.
    let client = core.rpc_client(&node.id()).unwrap();
    let info = client.call("node.info", Value::Null).await.unwrap();
    assert_eq!(info["os"], std::env::consts::OS);
    assert_eq!(info["has_display"], false);
    assert_eq!(info["protocol_version"], PROTOCOL_VERSION);
    let pong = client.call("node.ping", Value::Null).await.unwrap();
    assert_eq!(pong["pong"], true);

    // desktop.* through RemoteDesktop as a DesktopBackend.
    let remote = RemoteDesktop::connect(&core, &node.id()).await.unwrap();
    assert_eq!(remote.node_info().version, env!("CARGO_PKG_VERSION"));
    assert_eq!(remote.capabilities().platform, "fake");
    let snap = remote.snapshot(Some(5)).await.unwrap();
    assert_eq!(snap.window.title, "Fake Notepad");
    assert_eq!(snap.nodes[0].name, "scope=Some(5)");
    assert_eq!(snap.generation, 3);
    let wins = remote.windows().await.unwrap();
    assert_eq!(wins.len(), 1);
    let img = remote
        .screenshot(Region::FullScreen, Quality::Low)
        .await
        .unwrap();
    assert_eq!(img.data, vec![1, 2, 3, 255, 0]);
    assert_eq!((img.width, img.height), (2, 1));
    assert_eq!(
        remote
            .launch(AppSpec {
                executable: "x".into(),
                args: vec![]
            })
            .await
            .unwrap(),
        99
    );
    let r = remote
        .act(Action::KeyPress {
            keys: vec!["Return".into()],
        })
        .await
        .unwrap();
    assert!(r.dispatched);

    // Errors keep their kind across the wire.
    let err = remote
        .act(Action::TypeText {
            text: "boom".into(),
        })
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        DesktopError::ElementNotFound {
            index: 9,
            generation: 3
        }
    ));
    let err = remote
        .launch(AppSpec {
            executable: "nope".into(),
            args: vec![],
        })
        .await
        .unwrap_err();
    assert!(matches!(err, DesktopError::Unsupported(_)));

    // The device list survives a restart.
    drop(core);
}

#[tokio::test]
async fn device_list_persists_across_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (core, node) = paired(&dir).await;
    let node_id = node.id();
    core.shutdown().await;
    drop(core);
    let core = net(&dir, "core", Role::Desktop, None).await;
    assert_eq!(core.device(&node_id).unwrap().role, Role::Node);
    // Identity is stable across restarts too.
    let again = net(&dir, "node", Role::Node, None).await;
    assert_eq!(again.id(), node_id);
}

#[tokio::test]
async fn expired_code_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let core = net(&dir, "core", Role::Desktop, None).await;
    let node = net(&dir, "node", Role::Node, None).await;
    let invite = core
        .create_invite_with_ttl(Role::Node, Duration::ZERO)
        .await;
    let ticket = PairTicket::parse(&invite.ticket).unwrap();
    let err = node.pair(&ticket).await.unwrap_err();
    assert!(
        matches!(&err, PeerError::PairingRejected(m) if m.contains("expired")),
        "{err}"
    );
    assert_eq!(core.devices().len(), 0);
    assert_eq!(node.devices().len(), 0);
}

#[tokio::test]
async fn invite_is_single_use_and_checks_secret() {
    let dir = tempfile::tempdir().unwrap();
    let core = net(&dir, "core", Role::Desktop, None).await;
    let a = net(&dir, "a", Role::Node, None).await;
    let b = net(&dir, "b", Role::Node, None).await;
    let invite = core.create_invite(Role::Node).await;
    let ticket = PairTicket::parse(&invite.ticket).unwrap();

    // Wrong secret.
    let mut bad = ticket.clone();
    bad.secret = "00".repeat(32);
    assert!(matches!(
        a.pair(&bad).await.unwrap_err(),
        PeerError::PairingRejected(_)
    ));
    // Wrong code.
    let mut bad = ticket.clone();
    bad.code = "x".into();
    assert!(a.pair(&bad).await.is_err());

    // The genuine ticket works once ...
    a.pair(&ticket).await.unwrap();
    // ... and never again.
    assert!(matches!(
        b.pair(&ticket).await.unwrap_err(),
        PeerError::PairingRejected(_)
    ));
    assert_eq!(core.devices().len(), 1);
}

#[tokio::test]
async fn pairing_version_mismatch_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let core = net(&dir, "core", Role::Desktop, None).await;
    let invite = core.create_invite(Role::Node).await;
    let ticket = PairTicket::parse(&invite.ticket).unwrap();

    let raw = Endpoint::builder(presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .bind_addr(loopback())
        .unwrap()
        .bind()
        .await
        .unwrap();
    let conn = raw.connect(ticket.addr.clone(), ALPN_PAIR).await.unwrap();
    let (mut send, mut recv) = conn.open_bi().await.unwrap();
    let req = PairRequest {
        protocol_version: 99,
        secret: ticket.secret,
        code: ticket.code,
        name: "future".into(),
        role: Role::Node,
        capabilities: vec![],
        addr: raw.addr(),
    };
    write_frame(&mut send, &serde_json::to_vec(&req).unwrap(), 65536)
        .await
        .unwrap();
    send.finish().unwrap();
    let bytes = read_frame(&mut recv, 65536).await.unwrap().unwrap();
    match serde_json::from_slice::<PairResponse>(&bytes).unwrap() {
        PairResponse::Rejected {
            reason, message, ..
        } => {
            assert_eq!(reason, RejectReason::VersionMismatch);
            assert!(message.contains("incompatible protocol version"));
        }
        PairResponse::Accepted { .. } => panic!("must not pair"),
    }
    assert_eq!(core.devices().len(), 0);
}

#[tokio::test]
async fn role_mismatch_is_rejected_early_by_joiner() {
    let dir = tempfile::tempdir().unwrap();
    let core = net(&dir, "core", Role::Desktop, None).await;
    let phone = net(&dir, "phone", Role::Phone, None).await;
    let invite = core.create_invite(Role::Node).await;
    let err = phone
        .pair(&PairTicket::parse(&invite.ticket).unwrap())
        .await
        .unwrap_err();
    assert!(matches!(err, PeerError::PairingRejected(_)));
}

// ── Access control ────────────────────────────────────────────────────────────

#[tokio::test]
async fn unpaired_key_cannot_use_rpc() {
    let dir = tempfile::tempdir().unwrap();
    let (_core, node) = paired(&dir).await;

    let stranger = Endpoint::builder(presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .bind_addr(loopback())
        .unwrap()
        .bind()
        .await
        .unwrap();
    let conn = stranger.connect(node.addr(), ALPN_RPC).await.unwrap();
    let outcome = async {
        let (mut send, mut recv) = conn.open_bi().await.ok()?;
        let req = Request {
            jsonrpc: "2.0".into(),
            id: json!(1),
            method: "desktop.snapshot".into(),
            params: Value::Null,
        };
        write_frame(&mut send, &serde_json::to_vec(&req).unwrap(), MAX_FRAME)
            .await
            .ok()?;
        send.finish().ok()?;
        read_frame(&mut recv, MAX_FRAME).await.ok().flatten()
    };
    let reply = tokio::time::timeout(Duration::from_secs(5), outcome)
        .await
        .expect("must be closed promptly");
    assert!(reply.is_none(), "unpaired key got a reply");
    let closed = tokio::time::timeout(Duration::from_secs(5), conn.closed())
        .await
        .unwrap();
    assert!(closed.to_string().contains("not paired"), "{closed}");
}

#[tokio::test]
async fn node_role_may_not_control() {
    let dir = tempfile::tempdir().unwrap();
    let a = net(&dir, "a", Role::Node, Some(fake_handler())).await;
    let b = net(&dir, "b", Role::Node, None).await;
    let invite = a.create_invite(Role::Node).await;
    b.pair(&PairTicket::parse(&invite.ticket).unwrap())
        .await
        .unwrap();
    let client = b.rpc_client(&a.id()).unwrap();
    // node.* is fine, desktop.* is forbidden for a peer with role node.
    client.call("node.ping", Value::Null).await.unwrap();
    let err = client
        .call("desktop.snapshot", Value::Null)
        .await
        .unwrap_err();
    assert!(matches!(err, PeerError::Rpc { code: -32003, .. }), "{err}");
}

#[tokio::test]
async fn unknown_method_and_bad_params() {
    let dir = tempfile::tempdir().unwrap();
    let (core, node) = paired(&dir).await;
    let client = core.rpc_client(&node.id()).unwrap();
    let err = client.call("nope.nothing", Value::Null).await.unwrap_err();
    assert!(matches!(err, PeerError::Rpc { code: -32601, .. }));
    let err = client
        .call("desktop.act", json!({ "action": "garbage" }))
        .await
        .unwrap_err();
    assert!(matches!(err, PeerError::Rpc { code: -32602, .. }));
}

#[tokio::test]
async fn revoke_closes_the_connection() {
    let dir = tempfile::tempdir().unwrap();
    let (core, node) = paired(&dir).await;

    // RPC link: the node revokes the core while the core is connected.
    let client = core.rpc_client(&node.id()).unwrap();
    client.call("node.ping", Value::Null).await.unwrap();
    // The server registers the connection just after the handshake; wait for it.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(node.revoke(&core.id()).unwrap());
    assert!(node.device(&core.id()).is_none());
    let err = tokio::time::timeout(
        Duration::from_secs(5),
        client.call("node.ping", Value::Null),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(matches!(err, PeerError::NotPaired(_)), "{err}");
    // New connections are refused as well.
    let fresh = core.rpc_client(&node.id()).unwrap();
    let err = fresh.call("node.ping", Value::Null).await.unwrap_err();
    assert!(matches!(err, PeerError::NotPaired(_)), "{err}");
}

#[tokio::test]
async fn heartbeat_link_and_revoke_from_core() {
    let dir = tempfile::tempdir().unwrap();
    let (core, node) = paired(&dir).await;
    let mut events = core.subscribe();

    let (tx, mut states) = tokio::sync::mpsc::unbounded_channel();
    let cfg = HeartbeatConfig {
        interval: Duration::from_millis(100),
        initial_backoff: Duration::from_millis(50),
        max_backoff: Duration::from_millis(200),
        connect_timeout: Duration::from_secs(1),
    };
    let task = node.spawn_heartbeat(
        core.id(),
        cfg,
        Arc::new(move |s| {
            let _ = tx.send(s);
        }),
    );
    assert_eq!(states.recv().await.unwrap(), LinkState::Connected);
    let ev = loop {
        let ev = tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .unwrap()
            .unwrap();
        if ev.event.kind == "heartbeat" {
            break ev;
        }
    };
    assert_eq!(ev.peer, node.id());
    assert_eq!(ev.event.kind, "heartbeat");
    assert!(core.is_connected(&node.id()));
    assert!(core.last_seen(&node.id()).is_some());

    // Revoking on the core drops the link and stops the node's retries.
    assert!(core.revoke(&node.id()).unwrap());
    let mut revoked = false;
    while let Ok(Some(s)) = tokio::time::timeout(Duration::from_secs(5), states.recv()).await {
        if matches!(s, LinkState::Revoked(_)) {
            revoked = true;
            break;
        }
    }
    assert!(revoked, "node should learn it was revoked");
    task.await.unwrap();
    assert!(!core.is_connected(&node.id()));
}

#[tokio::test]
async fn heartbeat_reconnects_after_core_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (core, node) = paired(&dir).await;
    let node_id = node.id();
    // Fixed port so the restarted core is reachable at the stored address.
    let addr = core.addr();
    let port = addr.ip_addrs().next().unwrap().port();
    core.shutdown().await;
    drop(core);

    let (tx, mut states) = tokio::sync::mpsc::unbounded_channel();
    let cfg = HeartbeatConfig {
        interval: Duration::from_millis(100),
        initial_backoff: Duration::from_millis(50),
        max_backoff: Duration::from_millis(200),
        connect_timeout: Duration::from_secs(1),
    };
    let core_id = node.devices()[0].public_key().unwrap();
    let task = node.spawn_heartbeat(
        core_id,
        cfg,
        Arc::new(move |s| {
            let _ = tx.send(s);
        }),
    );
    // Core is down: the link reports disconnects and keeps retrying.
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(20), states.recv())
            .await
            .unwrap()
            .unwrap(),
        LinkState::Disconnected(_)
    ));

    let mut cfg = PeerConfig::new(dir.path().join("core"));
    cfg.relay = RelayConfig::Disabled;
    cfg.bind_addr = Some(SocketAddr::from(([127, 0, 0, 1], port)));
    let core = PeerNetwork::start(cfg, None).await.unwrap();
    let mut got = false;
    while let Ok(Some(s)) = tokio::time::timeout(Duration::from_secs(20), states.recv()).await {
        if s == LinkState::Connected {
            got = true;
            break;
        }
    }
    assert!(got, "node should reconnect");
    assert!(core.device(&node_id).is_some());
    task.abort();
}

// ── RPC mechanics ─────────────────────────────────────────────────────────────

struct Slow {
    dropped: Arc<AtomicBool>,
}

struct SetOnDrop(Arc<AtomicBool>);
impl Drop for SetOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[async_trait]
impl RpcHandler for Slow {
    async fn handle(
        &self,
        _peer: &peer_network::Device,
        method: &str,
        _params: Value,
    ) -> Result<Value, peer_network::proto::RpcError> {
        let _guard = SetOnDrop(self.dropped.clone());
        if method == "slow" {
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
        Ok(json!("done"))
    }
}

#[tokio::test]
async fn timeout_and_cancellation_abort_the_work() {
    let dir = tempfile::tempdir().unwrap();
    let core = net(&dir, "core", Role::Desktop, None).await;
    let dropped = Arc::new(AtomicBool::new(false));
    let node = net(
        &dir,
        "node",
        Role::Node,
        Some(Arc::new(Slow {
            dropped: dropped.clone(),
        })),
    )
    .await;
    let ticket = PairTicket::parse(&core.create_invite(Role::Node).await.ticket).unwrap();
    node.pair(&ticket).await.unwrap();

    // Timeout.
    let client = core
        .rpc_client(&node.id())
        .unwrap()
        .with_timeout(Duration::from_millis(300));
    let err = client.call("slow", Value::Null).await.unwrap_err();
    assert!(matches!(err, PeerError::Timeout), "{err}");
    wait_for(&dropped).await;

    // Cancellation.
    dropped.store(false, Ordering::SeqCst);
    let client = core.rpc_client(&node.id()).unwrap();
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        c2.cancel();
    });
    let err = client
        .call_cancellable("slow", Value::Null, &cancel)
        .await
        .unwrap_err();
    assert!(matches!(err, PeerError::Cancelled), "{err}");
    wait_for(&dropped).await;

    // The link still works afterwards.
    assert_eq!(
        client.call("fast", Value::Null).await.unwrap(),
        json!("done")
    );
}

async fn wait_for(flag: &AtomicBool) {
    for _ in 0..100 {
        if flag.load(Ordering::SeqCst) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("handler was not aborted");
}

#[tokio::test]
async fn oversize_request_is_refused_locally() {
    let dir = tempfile::tempdir().unwrap();
    let (core, node) = paired(&dir).await;
    let client = core.rpc_client(&node.id()).unwrap();
    let big = "x".repeat(MAX_FRAME + 1);
    let err = client
        .call("node.ping", json!({ "pad": big }))
        .await
        .unwrap_err();
    assert!(matches!(err, PeerError::Protocol(_)), "{err}");
    // Still usable.
    client.call("node.ping", Value::Null).await.unwrap();
}
