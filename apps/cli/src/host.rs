//! Host management and ownership heartbeat lifecycle.
use std::{collections::HashMap, io::Write, path::Path, sync::Arc, time::Duration};

use clap::Subcommand;
use peer_network::{HeartbeatConfig, LeaderState, PairTicket, PeerNetwork, Role};
use reqwest::Method;
use serde_json::json;

use crate::{Result, client::Client};

#[derive(Debug, Subcommand)]
pub enum HostCmd {
    /// Pair this fresh host with a desktop before starting the service.
    Join {
        ticket: String,
    },
    /// Create a single-use invite and print its QR and confirmation code.
    Invite {
        #[arg(long, value_parser = ["phone", "desktop", "node"])]
        role: String,
    },
    Status,
    Devices,
    Revoke {
        key: String,
    },
    Standby,
    Activate {
        #[arg(long)]
        force: bool,
    },
}

pub async fn join(data: &Path, ticket: &str) -> Result<()> {
    let ticket = PairTicket::parse(ticket)?;
    if ticket.role != Role::Host {
        return Err("host join requires an invite for role host".into());
    }
    let mut host = daemon_api::host::HostConfig::load(data)?;
    if (data.join("host.toml").exists()
        && (host.mode != daemon_api::host::HostMode::Host
            || host.state != daemon_api::host::HostState::Fresh))
        || (!data.join("host.toml").exists() && data.join("bot.db").exists())
    {
        return Err(
            "join requires a fresh installation; use explicit handover for existing data".into(),
        );
    }
    let (cfg, _) = crate::peer::config(data, true)?;
    let net = PeerNetwork::start(cfg, None).await?;
    let result = net.pair(&ticket).await;
    if let Ok(device) = &result
        && device.role != Role::Desktop
    {
        let _ = net.revoke(&ticket.addr.id);
    }
    net.shutdown().await;
    let device = result?;
    if device.role != Role::Desktop {
        return Err("host invites must originate from a desktop".into());
    }
    host.mode = daemon_api::host::HostMode::Host;
    host.state = daemon_api::host::HostState::Fresh;
    host.leader = None;
    host.save(data)?;
    println!("Paired desktop {}. Start bot serve --host.", device.id);
    Ok(())
}

pub async fn run(
    client: &Client,
    action: &HostCmd,
    json_output: bool,
    out: &mut dyn Write,
) -> Result<()> {
    let (method, path, body) = match action {
        HostCmd::Status | HostCmd::Devices => (Method::GET, "/v1/host".into(), None),
        HostCmd::Invite { role } => (
            Method::POST,
            "/v1/host/invite".into(),
            Some(json!({"role": role})),
        ),
        HostCmd::Revoke { key } => {
            let key = peer_network::key_from_string(key)?;
            (Method::DELETE, format!("/v1/host/devices/{key}"), None)
        }
        HostCmd::Standby => (Method::POST, "/v1/host/standby".into(), None),
        HostCmd::Activate { force } => (
            Method::POST,
            "/v1/host/activate".into(),
            Some(json!({"force": force})),
        ),
        HostCmd::Join { .. } => return Err("join is local only".into()),
    };
    if matches!(action, HostCmd::Activate { force: true }) {
        eprintln!(
            "Forced activation uses this installation's last local data; newer changes on the other leader may be missing."
        );
    }
    let mut response = client.request(method, &path, body).await?;
    if matches!(action, HostCmd::Devices) {
        response = response["peers"].clone();
    }
    if !json_output && matches!(action, HostCmd::Invite { .. }) {
        let invite: daemon_api::host::HostInvite = serde_json::from_value(response)?;
        let qr = qrcode::QrCode::new(invite.ticket.as_bytes())?;
        writeln!(
            out,
            "{}",
            qr.render::<qrcode::render::unicode::Dense1x2>().build()
        )?;
        writeln!(out, "Code: {}\n{}", invite.code, invite.ticket)?;
    } else {
        writeln!(out, "{}", serde_json::to_string_pretty(&response)?)?;
    }
    Ok(())
}

/// Keep links to daemon peers, publish metadata and apply authenticated events.
pub fn watch_peers(state: &daemon_api::AppState, net: PeerNetwork) {
    watch_peers_with_config(state, net, HeartbeatConfig::default());
}

fn watch_peers_with_config(
    state: &daemon_api::AppState,
    net: PeerNetwork,
    heartbeat: HeartbeatConfig,
) {
    state.leader().set_network(net.clone());
    let weak = state.downgrade();
    let mut events = net.subscribe();
    let shutdown = state.shutdown_token();
    tokio::spawn(async move {
        let mut links = HashMap::new();
        let mut tick = tokio::time::interval(Duration::from_secs(2));
        loop {
            tokio::select! {
                () = shutdown.cancelled() => break,
                _ = tick.tick() => {
                    let devices = net.devices();
                    links.retain(|key, task: &mut tokio::task::JoinHandle<()>| {
                        let keep = devices.iter().any(|d| &d.id == key) && !task.is_finished();
                        if !keep { task.abort(); }
                        keep
                    });
                    for device in devices.iter().filter(|d| matches!(d.role, Role::Host | Role::Desktop)) {
                        if !links.contains_key(&device.id) && let Ok(key) = peer_network::key_from_string(&device.id) {
                            links.insert(device.id.clone(), net.spawn_heartbeat(key, heartbeat, Arc::new(|_| {})));
                        }
                    }
                }
                event = events.recv() => {
                    let event = match event {
                        Ok(event) => event,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    };
                    if !net.device(&event.peer).is_some_and(|d| matches!(d.role, Role::Host | Role::Desktop)) { continue; }
                    let Some(state) = weak.upgrade() else { break; };
                    if event.event.kind == "host_conflict" {
                        if event.leader.is_some_and(|i| i.epoch == state.leader().config().epoch) {
                            if let Err(e) = state.leader().standby().await { eprintln!("host standby: {e}"); }
                            state.leader().emit_conflict("equal_epoch", Some(event.peer.to_string()));
                        }
                        continue;
                    }
                    if event.event.kind != "heartbeat" { continue; }
                    if let Some(info) = event.leader {
                        let local = state.leader().config();
                        let conflict = local.state == daemon_api::host::HostState::Active && info.state == LeaderState::Active && info.epoch == local.epoch;
                        if let Err(e) = state.leader().observe(&event.peer.to_string(), info).await { eprintln!("host leadership: {e}"); }
                        if conflict {
                            // Explicitly notify the other side even after our advertised state
                            // became standby; otherwise it could miss our final active heartbeat.
                            let message = peer_network::proto::Event { leader: Some(info), kind: "host_conflict".into(), ts_ms: u64::try_from(storage::now_ms()).unwrap_or(0), data: json!({"reason":"equal_epoch"}) };
                            let _ = tokio::time::timeout(Duration::from_secs(2), async {
                                let conn = net.connect(&event.peer, peer_network::ALPN_EVENTS).await?;
                                let mut send = conn.open_uni().await.map_err(|e| peer_network::PeerError::Connection(e.to_string()))?;
                                peer_network::proto::write_frame(&mut send, &serde_json::to_vec(&message).expect("plain event"), peer_network::proto::MAX_FRAME).await?;
                                let _ = send.finish();
                                let _ = send.stopped().await;
                                Ok::<_, peer_network::PeerError>(())
                            }).await;
                        }
                    }
                }
            }
        }
        if let Some(state) = weak.upgrade() {
            state.leader().detach_network();
        }
        for (_, task) in links {
            task.abort();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use daemon_api::host::{HostConfig, HostState};
    use peer_network::{PeerConfig, RelayConfig};

    async fn daemon(data: &Path, epoch: u64) -> (daemon_api::AppState, PeerNetwork) {
        HostConfig {
            epoch,
            ..HostConfig::default()
        }
        .save(data)
        .unwrap();
        let storage = Arc::new(storage::Storage::open(data).unwrap());
        let agent = agent_core::AgentLoop::new(
            storage.clone(),
            tool_registry::ToolRegistry::new(),
            agent_core::Providers::single(Arc::new(daemon_api::fake::FakeProvider)),
            agent_core::LoopConfig::default(),
        );
        let state =
            daemon_api::AppState::new(agent, storage, daemon_api::DaemonConfig::new("test-token"))
                .unwrap();
        let mut config = PeerConfig::new(data.join("peer"));
        config.relay = RelayConfig::Disabled;
        config.bind_addr = Some("127.0.0.1:0".parse().unwrap());
        let net = PeerNetwork::start(config, None).await.unwrap();
        (state, net)
    }

    #[tokio::test]
    async fn authenticated_heartbeat_demotes_lower_epoch_and_equal_epoch_stops_both() {
        for epoch in [0, 3] {
            let a_dir = tempfile::tempdir().unwrap();
            let b_dir = tempfile::tempdir().unwrap();
            let (a, a_net) = daemon(a_dir.path(), 0).await;
            let (b, b_net) = daemon(b_dir.path(), epoch).await;
            let invite = a_net.create_invite(Role::Desktop).await;
            b_net
                .pair(&PairTicket::parse(&invite.ticket).unwrap())
                .await
                .unwrap();
            let heartbeat = HeartbeatConfig {
                interval: Duration::from_millis(50),
                initial_backoff: Duration::from_millis(10),
                max_backoff: Duration::from_millis(100),
                connect_timeout: Duration::from_secs(2),
            };
            let mut a_events = a.leader().subscribe();
            let mut b_events = b.leader().subscribe();
            watch_peers_with_config(&a, a_net.clone(), heartbeat);
            watch_peers_with_config(&b, b_net.clone(), heartbeat);
            tokio::time::timeout(Duration::from_secs(12), async {
                while a.leader().config().state != HostState::Standby
                    || (epoch == 0 && b.leader().config().state != HostState::Standby)
                {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("heartbeat ownership policy converges");
            if epoch == 0 {
                assert!(matches!(
                    a_events.recv().await.unwrap(),
                    daemon_api::host::HostEvent::HostStateChanged(_)
                ));
                assert!(matches!(
                    a_events.recv().await.unwrap(),
                    daemon_api::host::HostEvent::HostConflict(_)
                ));
                assert!(matches!(
                    b_events.recv().await.unwrap(),
                    daemon_api::host::HostEvent::HostStateChanged(_)
                ));
                assert!(matches!(
                    b_events.recv().await.unwrap(),
                    daemon_api::host::HostEvent::HostConflict(_)
                ));
            } else {
                assert_eq!(b.leader().config().state, HostState::Active);
            }
            a.shutdown_token().cancel();
            b.shutdown_token().cancel();
            a_net.shutdown().await;
            b_net.shutdown().await;
        }
    }

    #[tokio::test]
    async fn host_join_records_desktop_and_preserves_fresh_mode() {
        let desktop_dir = tempfile::tempdir().unwrap();
        let host_dir = tempfile::tempdir().unwrap();
        let (_, desktop) = daemon(desktop_dir.path(), 0).await;
        std::fs::write(
            host_dir.path().join("config.toml"),
            "[peer]\nrelay='none'\n",
        )
        .unwrap();
        let invite = desktop.create_invite(Role::Host).await;
        join(host_dir.path(), &invite.ticket).await.unwrap();
        let config = HostConfig::load(host_dir.path()).unwrap();
        assert_eq!(config.mode, daemon_api::host::HostMode::Host);
        assert_eq!(config.state, HostState::Fresh);
        let (peer_config, _) = crate::peer::config(host_dir.path(), true).unwrap();
        let net = PeerNetwork::start(peer_config, None).await.unwrap();
        assert_eq!(net.devices().len(), 1);
        assert_eq!(net.devices()[0].role, Role::Desktop);
        assert_eq!(desktop.devices()[0].role, Role::Host);
        net.shutdown().await;
        desktop.shutdown().await;
    }
}
