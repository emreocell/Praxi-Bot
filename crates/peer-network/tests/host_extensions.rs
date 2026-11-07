//! Hermetic host-extension tests: direct loopback endpoints, relay disabled.
use async_trait::async_trait;
use iroh::{
    Endpoint,
    endpoint::{Connection, ConnectionError, presets},
};
use peer_network::proto::{PairRequest, PairResponse, RejectReason, read_frame, write_frame};
use peer_network::{
    ALPN_API, ALPN_PAIR, Device, HeartbeatConfig, LeaderInfo, LeaderState, PROTOCOL_VERSION,
    PairTicket, PeerConfig, PeerError, PeerNetwork, ProtocolHandler, RelayConfig, Role,
};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::sync::mpsc;

fn config(dir: &tempfile::TempDir, name: &str, role: Role) -> PeerConfig {
    let mut cfg = PeerConfig::new(dir.path().join(name));
    cfg.relay = RelayConfig::Disabled;
    cfg.bind_addr = Some(SocketAddr::from(([127, 0, 0, 1], 0)));
    cfg.local_role = role;
    cfg
}

async fn raw_endpoint() -> Endpoint {
    Endpoint::builder(presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .bind_addr(SocketAddr::from(([127, 0, 0, 1], 0)))
        .unwrap()
        .bind()
        .await
        .unwrap()
}

struct ExtraHandler(mpsc::UnboundedSender<(Connection, Device)>);
#[async_trait]
impl ProtocolHandler for ExtraHandler {
    async fn handle(&self, conn: Connection, device: Device) {
        self.0.send((conn, device)).unwrap();
    }
}

async fn extra_pair(
    dir: &tempfile::TempDir,
) -> (
    PeerNetwork,
    PeerNetwork,
    mpsc::UnboundedReceiver<(Connection, Device)>,
) {
    let (tx, rx) = mpsc::unbounded_channel();
    let mut cfg = config(dir, "host", Role::Host);
    cfg.extra_protocols
        .push((ALPN_API.to_vec(), Arc::new(ExtraHandler(tx))));
    let host = PeerNetwork::start(cfg, None).await.unwrap();
    let desktop = PeerNetwork::start(config(dir, "desktop", Role::Desktop), None)
        .await
        .unwrap();
    let invite = host.create_invite(Role::Desktop).await;
    let device = desktop
        .pair(&PairTicket::parse(&invite.ticket).unwrap())
        .await
        .unwrap();
    assert_eq!(device.role, Role::Host);
    (host, desktop, rx)
}

async fn closed(conn: &Connection, code: u64) {
    let err = tokio::time::timeout(Duration::from_secs(5), conn.closed())
        .await
        .unwrap();
    assert!(
        matches!(err, ConnectionError::ApplicationClosed(c) if c.error_code.into_inner() == code)
    );
}

#[tokio::test]
async fn extra_protocol_is_paired_only_and_revoke_closes_inbound() {
    let dir = tempfile::tempdir().unwrap();
    let (host, desktop, mut calls) = extra_pair(&dir).await;
    let stranger = raw_endpoint().await;
    assert!(matches!(
        host.connect(&stranger.id(), ALPN_API).await,
        Err(PeerError::UnknownDevice(_))
    ));
    let conn = stranger.connect(host.addr(), ALPN_API).await.unwrap();
    closed(&conn, 403).await;
    assert!(calls.try_recv().is_err());
    let conn = desktop.connect(&host.id(), ALPN_API).await.unwrap();
    let (server, device) = tokio::time::timeout(Duration::from_secs(5), calls.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(device.id, peer_network::key_to_string(&desktop.id()));
    assert_eq!(device.role, Role::Desktop);
    assert_eq!(server.remote_id(), desktop.id());
    host.revoke(&desktop.id()).unwrap();
    closed(&conn, 410).await;
    stranger.close().await;
    desktop.shutdown().await;
    host.shutdown().await;
}

#[tokio::test]
async fn revoke_closes_outbound_extra_protocol() {
    let dir = tempfile::tempdir().unwrap();
    let (host, desktop, mut calls) = extra_pair(&dir).await;
    let conn = desktop.connect(&host.id(), ALPN_API).await.unwrap();
    let (server, _) = tokio::time::timeout(Duration::from_secs(5), calls.recv())
        .await
        .unwrap()
        .unwrap();
    desktop.revoke(&host.id()).unwrap();
    closed(&server, 410).await;
    assert!(conn.close_reason().is_some());
    assert!(matches!(
        desktop.connect(&host.id(), ALPN_API).await,
        Err(PeerError::UnknownDevice(_))
    ));
    desktop.shutdown().await;
    host.shutdown().await;
}

#[tokio::test]
async fn leader_heartbeats_update_and_clear_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let (core, node, _calls) = extra_pair(&dir).await;
    let mut events = core.subscribe();
    let task = node.spawn_heartbeat(
        core.id(),
        HeartbeatConfig {
            interval: Duration::from_millis(30),
            ..HeartbeatConfig::default()
        },
        Arc::new(|_| {}),
    );
    for leader in [
        Some(LeaderInfo {
            epoch: 42,
            state: LeaderState::Active,
        }),
        Some(LeaderInfo {
            epoch: 43,
            state: LeaderState::Standby,
        }),
        Some(LeaderInfo {
            epoch: 44,
            state: LeaderState::Fresh,
        }),
        None,
    ] {
        node.set_leader_info(leader);
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let event = events.recv().await.unwrap();
                if event.event.kind == "heartbeat" && event.leader == leader {
                    assert_eq!(event.event.leader, leader);
                    assert_eq!(core.peer_leader(&node.id()), leader);
                    break;
                }
            }
        })
        .await
        .unwrap();
    }
    node.set_leader_info(Some(LeaderInfo {
        epoch: 50,
        state: LeaderState::Active,
    }));
    tokio::time::timeout(Duration::from_secs(5), async {
        while core.peer_leader(&node.id()).is_none() {
            let _ = events.recv().await.unwrap();
        }
    })
    .await
    .unwrap();
    core.revoke(&node.id()).unwrap();
    assert_eq!(core.peer_leader(&node.id()), None);
    task.abort();
    node.shutdown().await;
    core.shutdown().await;
}

#[tokio::test]
async fn bogus_secret_flood_is_rate_limited_without_burning_invite() {
    let dir = tempfile::tempdir().unwrap();
    let core = PeerNetwork::start(config(&dir, "core", Role::Desktop), None)
        .await
        .unwrap();
    let node = PeerNetwork::start(config(&dir, "node", Role::Host), None)
        .await
        .unwrap();
    let invite = core
        .create_invite_with_ttl(Role::Host, Duration::from_secs(15 * 60))
        .await;
    assert_eq!(invite.ttl, Duration::from_secs(15 * 60));
    let ticket = PairTicket::parse(&invite.ticket).unwrap();
    let raw = raw_endpoint().await;
    for _ in 0..5 {
        let conn = raw.connect(core.addr(), ALPN_PAIR).await.unwrap();
        let (mut send, mut recv) = conn.open_bi().await.unwrap();
        let req = PairRequest {
            protocol_version: PROTOCOL_VERSION,
            secret: "bogus".into(),
            code: ticket.code.clone(),
            name: "attacker".into(),
            role: Role::Host,
            capabilities: vec![],
            addr: raw.addr(),
        };
        write_frame(&mut send, &serde_json::to_vec(&req).unwrap(), 65536)
            .await
            .unwrap();
        send.finish().unwrap();
        let bytes = tokio::time::timeout(Duration::from_secs(5), read_frame(&mut recv, 65536))
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(matches!(
            serde_json::from_slice::<PairResponse>(&bytes).unwrap(),
            PairResponse::Rejected {
                reason: RejectReason::InvalidInvite,
                ..
            }
        ));
        conn.close(0u32.into(), b"done");
    }
    let conn = raw.connect(core.addr(), ALPN_PAIR).await.unwrap();
    closed(&conn, 429).await;
    node.pair(&ticket).await.unwrap();
    assert_eq!(core.device(&node.id()).unwrap().role, Role::Host);
    let capped = core.create_invite_with_ttl(Role::Host, Duration::MAX).await;
    assert_eq!(capped.ttl, Duration::from_secs(30 * 60));
    raw.close().await;
    node.shutdown().await;
    core.shutdown().await;
}

#[tokio::test]
async fn reserved_duplicate_and_invalid_alpns_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    for alpns in [
        vec![ALPN_PAIR.to_vec()],
        vec![ALPN_API.to_vec(), ALPN_API.to_vec()],
        vec![vec![]],
        vec![vec![1; 256]],
    ] {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut cfg = config(&dir, "invalid", Role::Host);
        for alpn in alpns {
            cfg.extra_protocols
                .push((alpn, Arc::new(ExtraHandler(tx.clone()))));
        }
        assert!(matches!(
            PeerNetwork::start(cfg, None).await,
            Err(PeerError::Protocol(_))
        ));
    }
}
