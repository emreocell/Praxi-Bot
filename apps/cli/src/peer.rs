//! Daemon peer endpoint configuration and remote CLI client lifetime.
use crate::Result;
use peer_network::{ALPN_API, PeerConfig, PeerNetwork, Role};
use std::{path::Path, sync::Arc};

fn config(data: &Path, host: bool) -> Result<(PeerConfig, Option<bool>)> {
    let mut cfg = PeerConfig::new(data.join("peer"));
    cfg.local_role = if host { Role::Host } else { Role::Desktop };
    cfg.display_name = if host { "Praxi host" } else { "Praxi desktop" }.into();
    let path = data.join("config.toml");
    let config = match std::fs::read_to_string(path) {
        Ok(text) => text.parse::<toml::Value>()?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            toml::Value::Table(toml::map::Map::default())
        }
        Err(e) => return Err(e.into()),
    };
    let peer = config.get("peer");
    let enabled = peer
        .and_then(|p| p.get("enabled"))
        .map(|v| v.as_bool().ok_or("peer.enabled must be a boolean"))
        .transpose()?;
    if let Some(relay) = peer.and_then(|p| p.get("relay")) {
        cfg.relay = relay
            .as_str()
            .ok_or("peer.relay must be a string")?
            .parse()?;
    }
    Ok((cfg, enabled))
}

pub fn is_enabled(data: &Path, host: bool, force: bool) -> Result<bool> {
    let (_, enabled) = config(data, host)?;
    Ok(force || enabled.unwrap_or(host))
}

pub async fn start_server(
    data: &Path,
    host: bool,
    force: bool,
    state: daemon_api::AppState,
) -> Result<Option<PeerNetwork>> {
    let (mut cfg, enabled) = config(data, host)?;
    if !(force || enabled.unwrap_or(host)) {
        return Ok(None);
    }
    cfg.extra_protocols.push((
        ALPN_API.to_vec(),
        Arc::new(remote_api::ApiProtocol::new(state)),
    ));
    let net = PeerNetwork::start(cfg, None).await?;
    eprintln!("peer endpoint: {}", net.id());
    Ok(Some(net))
}

pub async fn start_client(
    data: &Path,
    endpoint: &str,
) -> Result<(PeerNetwork, remote_api::ApiProxy)> {
    let peer = peer_network::key_from_string(endpoint)?;
    let (cfg, _) = config(data, false)?;
    let net = PeerNetwork::start(cfg, None).await?;
    let proxy = match remote_api::ApiProxy::start(net.clone(), peer).await {
        Ok(proxy) => proxy,
        Err(e) => {
            net.shutdown().await;
            return Err(e.into());
        }
    };
    Ok((net, proxy))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn peer_defaults_and_config() {
        let dir = tempfile::tempdir().unwrap();
        let (cfg, enabled) = config(dir.path(), true).unwrap();
        assert_eq!(cfg.local_role, Role::Host);
        assert_eq!(cfg.data_dir, dir.path().join("peer"));
        assert_eq!(enabled, None);
        std::fs::write(
            dir.path().join("config.toml"),
            "[peer]\nenabled = true\nrelay = 'none'\n",
        )
        .unwrap();
        let (cfg, enabled) = config(dir.path(), false).unwrap();
        assert_eq!(cfg.relay, peer_network::RelayConfig::Disabled);
        assert_eq!(enabled, Some(true));
        assert_eq!(cfg.local_role, Role::Desktop);
    }
    #[tokio::test]
    async fn remote_status_uses_persisted_pairing_and_proxy() {
        let host_dir = tempfile::tempdir().unwrap();
        let client_dir = tempfile::tempdir().unwrap();
        for dir in [host_dir.path(), client_dir.path()] {
            std::fs::write(
                dir.join("config.toml"),
                "[peer]\nenabled=true\nrelay='none'\n",
            )
            .unwrap();
        }
        let storage = Arc::new(storage::Storage::open(host_dir.path()).unwrap());
        let agent = agent_core::AgentLoop::new(
            storage.clone(),
            tool_registry::ToolRegistry::new(),
            agent_core::Providers::single(Arc::new(daemon_api::fake::FakeProvider)),
            agent_core::LoopConfig::default(),
        );
        let state = daemon_api::AppState::new(
            agent,
            storage,
            daemon_api::DaemonConfig::new("host-only-token"),
        )
        .unwrap();
        let host = start_server(host_dir.path(), true, false, state)
            .await
            .unwrap()
            .unwrap();
        let (cfg, _) = config(client_dir.path(), false).unwrap();
        let pairing_client = PeerNetwork::start(cfg, None).await.unwrap();
        let invite = host.create_invite(Role::Desktop).await;
        pairing_client
            .pair(&peer_network::PairTicket::parse(&invite.ticket).unwrap())
            .await
            .unwrap();
        pairing_client.shutdown().await;
        let (net, proxy) = start_client(client_dir.path(), &host.id().to_string())
            .await
            .unwrap();
        let client = crate::client::Client::new(&proxy.base_url, proxy.token.clone());
        let mut output = Vec::new();
        crate::remote::status(&client, true, &mut output)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(value["health"]["status"], "ok");
        assert_eq!(value["pending_approvals"], 0);
        drop(proxy);
        net.shutdown().await;
        host.shutdown().await;
    }
}
