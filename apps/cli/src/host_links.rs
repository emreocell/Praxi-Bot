//! This computer's links to paired always-on hosts (`/v1/hosts`).
//!
//! The local daemon is the app's iroh client: it pairs with a host by ticket
//! and opens a guarded loopback proxy ([`remote_api::ApiProxy`]) per host the
//! app selects. Nothing here talks to a host without a pairing, adds a listening
//! port, or bypasses the host's peer guard: the proxy dials as a Desktop peer.
//!
//! The peer endpoint starts lazily (first pair or connect), never at daemon
//! start, unless `serve --peer` already started one (then it is shared).
use std::{collections::HashMap, path::PathBuf, sync::Arc};

use builtin_tools::pc::{PcError, PcLink, ServerLinks};
use daemon_api::host_links::{
    HostLinkConnection, HostLinkError, HostLinkInfo, HostLinkState, HostLinks,
};
use parking_lot::Mutex;
use peer_network::{Device, PairTicket, PeerNetwork, Role};
use remote_api::{ApiProxy, ProxyState};

struct Open {
    proxy: ApiProxy,
    /// The channel was up at least once (separates "connecting" from "offline").
    seen_up: bool,
}

/// What lets this computer be a target for a paired host's bots (card 258-A).
pub struct PcTarget {
    pub access: Arc<crate::pc_target::AccessStore>,
    pub requests: Arc<daemon_api::pc_access::PcRequests>,
    /// `None` when this computer has no desktop to share.
    pub handler: Option<Arc<crate::pc_target::HostDesktopHandler>>,
}

/// Overwrite every string in `value` (zeroize) so a key does not linger.
fn scrub(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(s) => zeroize::Zeroize::zeroize(s),
        serde_json::Value::Array(a) => a.iter_mut().for_each(scrub),
        serde_json::Value::Object(o) => o.values_mut().for_each(scrub),
        _ => {}
    }
}

pub struct CliHostLinks {
    data: PathBuf,
    shared: Option<PeerNetwork>,
    started: tokio::sync::Mutex<Option<PeerNetwork>>,
    open: Mutex<HashMap<String, Open>>,
    pc: Option<PcTarget>,
    /// Keep-alive links so a host can reach this computer without the app selecting it.
    beats: Mutex<HashMap<String, tokio::task::JoinHandle<()>>>,
    /// Which servers this computer may work with, judged from their `/v1/health`.
    compatible: remote_api::Compatibility,
}

/// A short, single-line, path-free message for the UI.
fn safe(msg: impl std::fmt::Display) -> String {
    let text: String = msg
        .to_string()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(200)
        .collect();
    text.trim().to_owned()
}

#[derive(serde::Deserialize)]
struct PairingFile {
    #[serde(default)]
    devices: Vec<Device>,
}

impl CliHostLinks {
    pub fn new(data: PathBuf, shared: Option<PeerNetwork>) -> Self {
        Self {
            data,
            shared,
            started: tokio::sync::Mutex::new(None),
            open: Mutex::new(HashMap::new()),
            pc: None,
            beats: Mutex::new(HashMap::new()),
            compatible: crate::host_compat::compatibility(),
        }
    }

    /// Tests: judge servers by another rule than this build's release.
    #[cfg(test)]
    fn with_compatibility(mut self, compatible: remote_api::Compatibility) -> Self {
        self.compatible = compatible;
        self
    }

    /// Whether ordinary traffic may flow to server `id`, from its link's first verdict (the link
    /// is opened when there is none yet). `Ok(false)`: it runs a release this computer may not
    /// work with.
    async fn may_work_with(&self, id: &str) -> Result<bool, HostLinkError> {
        let existing = self
            .open
            .lock()
            .get(id)
            .map(|link| link.proxy.state.clone());
        let mut state = if let Some(state) = existing {
            state
        } else {
            self.connect(id).await?;
            self.open
                .lock()
                .get(id)
                .map(|link| link.proxy.state.clone())
                .ok_or(HostLinkError::NotFound)?
        };
        let seen = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            state.wait_for(|s| matches!(s, ProxyState::Connected | ProxyState::Incompatible(_))),
        )
        .await
        .map_err(|_| HostLinkError::Failed("the host did not answer".into()))?
        .map_err(|_| HostLinkError::Failed("the host link closed".into()))?
        .clone();
        Ok(matches!(seen, ProxyState::Connected))
    }

    #[must_use]
    pub fn with_pc_target(mut self, pc: PcTarget) -> Self {
        self.pc = Some(pc);
        self
    }

    /// Open the events link to a host so it can call this computer.
    async fn ensure_beat(&self, id: &str) -> Result<(), HostLinkError> {
        let device = self
            .devices()
            .into_iter()
            .find(|d| d.id == id && d.role == Role::Host)
            .ok_or(HostLinkError::NotFound)?;
        let key = device.public_key().map_err(|_| HostLinkError::NotFound)?;
        if self.beats.lock().contains_key(id) {
            return Ok(());
        }
        let net = self.network().await?;
        let beat = net.spawn_heartbeat(
            key,
            peer_network::HeartbeatConfig::default(),
            Arc::new(|_| {}),
        );
        if let Some(old) = self.beats.lock().insert(id.to_owned(), beat) {
            old.abort();
        }
        Ok(())
    }

    /// After a restart: reconnect to every host the owner lets use this computer.
    pub async fn restore_access(&self) {
        let Some(pc) = &self.pc else { return };
        for id in pc.access.enabled_ids() {
            let _ = self.ensure_beat(&id).await;
        }
    }
    async fn network(&self) -> Result<PeerNetwork, HostLinkError> {
        if let Some(net) = &self.shared {
            return Ok(net.clone());
        }
        let mut slot = self.started.lock().await;
        if let Some(net) = slot.as_ref() {
            return Ok(net.clone());
        }
        let (cfg, _) =
            crate::peer::config(&self.data, false).map_err(|e| HostLinkError::Failed(safe(e)))?;
        let handler = self
            .pc
            .as_ref()
            .and_then(|pc| pc.handler.clone())
            .map(|h| h as Arc<dyn peer_network::RpcHandler>);
        let net = PeerNetwork::start(cfg, handler)
            .await
            .map_err(|e| HostLinkError::Failed(safe(e)))?;
        *slot = Some(net.clone());
        Ok(net)
    }

    /// The paired always-on server with key `id`, if there is one.
    pub(crate) fn host_device(&self, id: &str) -> Option<Device> {
        self.devices()
            .into_iter()
            .find(|d| d.id == id && d.role == Role::Host)
    }

    /// One RPC to a paired server as the paired desktop (gap 2 of card 272).
    /// The server decides from ITS pairing file whether this computer is its
    /// administrator; nothing is claimed here.
    pub(crate) async fn call_host(
        &self,
        id: &str,
        method: &str,
        params: serde_json::Value,
        timeout: std::time::Duration,
    ) -> Result<serde_json::Value, PcError> {
        let device = self.host_device(id).ok_or_else(not_paired)?;
        let key = device.public_key().map_err(|_| not_paired())?;
        // Work only goes to a server that runs this release.
        if !self.may_work_with(id).await.map_err(|_| offline())? {
            return Err(incompatible());
        }
        let net = self.network().await.map_err(|_| offline())?;
        let client = net.rpc_client(&key).map_err(|_| not_paired())?;
        client
            .call_with_timeout(method, params, timeout)
            .await
            .map_err(server_error)
    }

    /// Paired devices without starting the endpoint: the running one if there
    /// is one, else the pairing file.
    fn devices(&self) -> Vec<Device> {
        if let Some(net) = &self.shared {
            return net.devices();
        }
        if let Some(net) = self.started.try_lock().ok().and_then(|s| s.clone()) {
            return net.devices();
        }
        std::fs::read_to_string(self.data.join("peer").join("devices.json"))
            .ok()
            .and_then(|text| serde_json::from_str::<PairingFile>(&text).ok())
            .map(|f| f.devices)
            .unwrap_or_default()
    }

    fn info(&self, device: &Device) -> HostLinkInfo {
        let sw = self
            .pc
            .as_ref()
            .map(|pc| pc.access.switches(&device.id))
            .unwrap_or_default();
        let mut open = self.open.lock();
        let (state, detail) = match open.get_mut(&device.id) {
            None => (HostLinkState::Idle, None),
            Some(link) => match link.proxy.state.borrow().clone() {
                // The channel is up: the app reads the release and says what is wrong.
                ProxyState::Connected | ProxyState::Incompatible(_) => {
                    link.seen_up = true;
                    (HostLinkState::Connected, None)
                }
                ProxyState::Reconnecting if link.seen_up => (
                    HostLinkState::Offline,
                    Some("the host dropped the connection; retrying".into()),
                ),
                ProxyState::Reconnecting => (HostLinkState::Connecting, None),
                ProxyState::Unreachable(why) => {
                    let lower = why.to_ascii_lowercase();
                    if lower.contains("revoked") || lower.contains("not paired") {
                        (
                            HostLinkState::Unreachable,
                            Some("the host no longer accepts this computer; pair again".into()),
                        )
                    } else if link.seen_up {
                        (
                            HostLinkState::Offline,
                            Some("the host did not answer; retrying".into()),
                        )
                    } else {
                        (
                            HostLinkState::Connecting,
                            Some("waiting for the host to answer".into()),
                        )
                    }
                }
            },
        };
        HostLinkInfo {
            id: device.id.clone(),
            name: device.name.clone(),
            state,
            detail,
            computer_access: sw.screen,
            computer_access_always: sw.screen && sw.always_read,
            computer_browser: sw.browser,
            computer_commands: sw.commands,
            computer_files: sw.files,
        }
    }
}

#[async_trait::async_trait]
impl HostLinks for CliHostLinks {
    fn list(&self) -> Vec<HostLinkInfo> {
        self.devices()
            .iter()
            .filter(|d| d.role == Role::Host)
            .map(|d| self.info(d))
            .collect()
    }

    async fn pair(&self, ticket: &str) -> Result<HostLinkInfo, HostLinkError> {
        let ticket = PairTicket::parse(ticket).map_err(|e| match e {
            peer_network::PeerError::VersionMismatch { .. } => HostLinkError::ProtocolMismatch,
            _ => HostLinkError::Invalid("that is not a pairing ticket".into()),
        })?;
        if ticket.role != Role::Desktop {
            return Err(HostLinkError::Invalid(
                "this ticket is for another role; on the host run `bot host invite --role desktop`"
                    .into(),
            ));
        }
        let net = self.network().await?;
        let device = net.pair(&ticket).await.map_err(|e| match e {
            peer_network::PeerError::VersionMismatch { .. } => HostLinkError::ProtocolMismatch,
            _ => HostLinkError::Failed(safe(e)),
        })?;
        if device.role != Role::Host {
            return Err(HostLinkError::Invalid(
                "paired, but that device is not an always-on host (start it with `bot serve --host`)"
                    .into(),
            ));
        }
        Ok(self.info(&device))
    }

    async fn connect(&self, id: &str) -> Result<HostLinkConnection, HostLinkError> {
        let device = self
            .devices()
            .into_iter()
            .find(|d| d.id == id && d.role == Role::Host)
            .ok_or(HostLinkError::NotFound)?;
        let key = device.public_key().map_err(|_| HostLinkError::NotFound)?;
        // Read the open link under the lock, release it, THEN ask `info` (it
        // takes the same non-recursive lock).
        let existing = self
            .open
            .lock()
            .get(id)
            .map(|link| (link.proxy.base_url.clone(), link.proxy.token.clone()));
        if let Some((base_url, token)) = existing {
            return Ok(HostLinkConnection {
                id: device.id.clone(),
                name: device.name.clone(),
                base_url,
                token,
                state: self.info(&device).state,
            });
        }
        let net = self.network().await?;
        let proxy = ApiProxy::start_checked(net, key, Arc::clone(&self.compatible))
            .await
            .map_err(|e| HostLinkError::Failed(safe(e)))?;
        let connection = HostLinkConnection {
            id: device.id.clone(),
            name: device.name.clone(),
            base_url: proxy.base_url.clone(),
            token: proxy.token.clone(),
            state: HostLinkState::Connecting,
        };
        // A concurrent connect may have won: keep the first, drop ours.
        let mut open = self.open.lock();
        if let Some(link) = open.get(id) {
            return Ok(HostLinkConnection {
                base_url: link.proxy.base_url.clone(),
                token: link.proxy.token.clone(),
                ..connection
            });
        }
        open.insert(
            id.to_owned(),
            Open {
                proxy,
                seen_up: false,
            },
        );
        Ok(connection)
    }

    async fn set_computer_access(
        &self,
        id: &str,
        access: &daemon_api::pc_access::ComputerAccessInput,
    ) -> Result<(), HostLinkError> {
        let (enabled, always_read) = (access.enabled, access.always_read);
        let any = enabled || access.browser || access.commands || access.files;
        let Some(pc) = &self.pc else {
            return Err(HostLinkError::Invalid(
                "this computer cannot be used by a server".into(),
            ));
        };
        if pc.handler.is_none() && any {
            return Err(HostLinkError::Invalid(
                "this computer has no screen a server could use".into(),
            ));
        }
        if !self
            .devices()
            .iter()
            .any(|d| d.id == id && d.role == Role::Host)
        {
            return Err(HostLinkError::NotFound);
        }
        let before = pc.access.switches(id);
        let next = crate::pc_target::Switches {
            screen: enabled,
            always_read,
            browser: access.browser,
            commands: access.commands,
            files: access.files,
        };
        pc.access
            .set_switches(id, next)
            .map_err(|e| HostLinkError::Failed(safe(e)))?;
        if !any
            || (before.screen && !next.screen)
            || (before.browser && !next.browser)
            || (before.commands && !next.commands)
            || (before.files && !next.files)
        {
            // Whatever the host had parked is denied now, not at its timeout.
            pc.requests.cancel_host(id);
        }
        if any {
            self.ensure_beat(id).await?;
        } else if let Some(beat) = self.beats.lock().remove(id) {
            beat.abort();
        }
        Ok(())
    }

    fn computer_requests(&self) -> daemon_api::pc_access::PcRequestList {
        self.pc.as_ref().map_or(
            daemon_api::pc_access::PcRequestList {
                requests: Vec::new(),
                reading: Vec::new(),
            },
            |pc| pc.requests.snapshot(),
        )
    }

    fn decide_computer_request(&self, id: &str, allow: bool) -> bool {
        self.pc
            .as_ref()
            .is_some_and(|pc| pc.requests.decide(id, allow))
    }

    fn disconnect(&self, id: &str) -> bool {
        self.open.lock().remove(id).is_some()
    }

    async fn call(
        &self,
        id: &str,
        method: &str,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> Result<daemon_api::host_links::HostCall, HostLinkError> {
        // Only administration, bot and health paths of the host API.
        // `GET /v1/host` is the host's read-only status (its version for "Update available").
        let status_read = method.eq_ignore_ascii_case("GET") && path == "/v1/host";
        if !(status_read || path.starts_with("/v1/admin/") || path.starts_with("/v1/bots")) {
            return Err(HostLinkError::Invalid("path not allowed".into()));
        }
        // Secrets, bots and work only go to a server that runs this release (the proxy enforces
        // the same for every other caller); its status and the update request always pass.
        if !remote_api::works_while_incompatible(&method.to_ascii_uppercase(), path)
            && !self.may_work_with(id).await?
        {
            return Err(HostLinkError::Incompatible);
        }
        let conn = self.connect(id).await?;
        let method = reqwest::Method::from_bytes(method.as_bytes())
            .map_err(|_| HostLinkError::Invalid("bad method".into()))?;
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(40))
            .build()
            .map_err(|e| HostLinkError::Failed(safe(e)))?;
        let mut req = http
            .request(method, format!("{}{path}", conn.base_url))
            .bearer_auth(&conn.token);
        if let Some(mut body) = body {
            req = req.json(&body);
            // The request now owns its serialised copy; wipe ours (a key).
            scrub(&mut body);
        }
        let resp = req
            .send()
            .await
            .map_err(|_| HostLinkError::Failed("the host did not answer".into()))?;
        let status = resp.status().as_u16();
        let body = resp.json().await.unwrap_or(serde_json::Value::Null);
        Ok(daemon_api::host_links::HostCall { status, body })
    }

    async fn forget(&self, id: &str) -> bool {
        let Some(device) = self
            .devices()
            .into_iter()
            .find(|d| d.id == id && d.role == Role::Host)
        else {
            return false;
        };
        self.open.lock().remove(id);
        if let Some(pc) = &self.pc {
            // Removing the server ends its use of this computer.
            let _ = pc.access.set(id, false, false);
            pc.requests.cancel_host(id);
        }
        if let Some(beat) = self.beats.lock().remove(id) {
            beat.abort();
        }
        let (Ok(key), Ok(net)) = (device.public_key(), self.network().await) else {
            return false;
        };
        net.revoke(&key).unwrap_or(false)
    }
}

fn not_paired() -> PcError {
    PcError::Message(
        "The server this bot uses as its computer is not paired with this computer any more, so its browser cannot be used. Tell the user to pair the server again or to change the bot's computer. Do not use another browser instead.".into(),
    )
}

fn incompatible() -> PcError {
    PcError::Message(
        "The server this bot uses as its computer runs a different Praxi release than this computer, so its browser cannot be used. Tell the user to update the server (or this app) so that both run the same release. Do not use another browser instead.".into(),
    )
}

fn offline() -> PcError {
    PcError::Message(
        "The server this bot uses as its computer is off or cannot be reached right now, so its browser cannot be used. Tell the user to check that the server is on, then try again. Do not use another browser instead.".into(),
    )
}

/// One line of text from the other side, safe to show: no control characters, bounded.
fn plain(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).take(300).collect()
}

/// What a failed call to the server means for the model, in words. The
/// server's own text is shown as data inside a sentence, never trusted.
fn server_error(e: peer_network::PeerError) -> PcError {
    match e {
        peer_network::PeerError::Rpc { message, .. } => PcError::Message(format!(
            "The server refused this browser action ({}). Tell the user; do not try another way around it.",
            plain(&message)
        )),
        peer_network::PeerError::Timeout => {
            PcError::Message("The server did not answer in time.".into())
        }
        peer_network::PeerError::Connection(_) => offline(),
        peer_network::PeerError::NotPaired(_) | peer_network::PeerError::UnknownDevice(_) => {
            not_paired()
        }
        other => PcError::Message(format!(
            "The server's browser did not complete this ({}).",
            plain(&other.to_string())
        )),
    }
}

/// The paired servers a bot of THIS computer may use as its own computer
/// (`[runtime].computer = "server:<key>"`): the bot, its model calls and its
/// approvals stay here, only browser calls cross the paired link.
pub struct CliServerLinks(pub Arc<CliHostLinks>);

struct HostLink {
    links: Arc<CliHostLinks>,
    id: String,
}

#[async_trait::async_trait]
impl PcLink for HostLink {
    async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
        timeout: std::time::Duration,
    ) -> Result<serde_json::Value, PcError> {
        self.links
            .call_host(&self.id, method, params, timeout)
            .await
    }

    fn desktop(
        &self,
        _caller: &builtin_tools::pc::PcCaller,
    ) -> Result<Arc<dyn desktop_api::DesktopBackend>, PcError> {
        Err(PcError::Message(
            "The server's screen is not used as a computer by this bot.".into(),
        ))
    }
}

impl ServerLinks for CliServerLinks {
    fn link(&self, id: &str) -> Option<(String, Arc<dyn PcLink>)> {
        let device = self.0.host_device(id)?;
        Some((
            device.name.clone(),
            Arc::new(HostLink {
                links: Arc::clone(&self.0),
                id: device.id,
            }),
        ))
    }
}

pub fn install(
    state: &daemon_api::AppState,
    data: PathBuf,
    shared: Option<PeerNetwork>,
    pc: PcTarget,
) -> Arc<CliHostLinks> {
    let links = Arc::new(CliHostLinks::new(data.clone(), shared).with_pc_target(pc));
    let restore = Arc::clone(&links);
    tokio::spawn(async move { restore.restore_access().await });
    state.set_host_links(links.clone());
    // The "Add server" wizard pairs through the same links.
    state.set_host_setup(Arc::new(crate::host_setup::CliHostSetup::new(
        data,
        Arc::clone(&links),
    )));
    links
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_devices(dir: &std::path::Path, roles: &[(&str, &str)]) {
        let devices: Vec<serde_json::Value> = roles
            .iter()
            .map(|(id, role)| {
                serde_json::json!({
                    "id": id, "name": format!("dev-{id}"), "role": role,
                    "capabilities": [], "addr": null, "paired_at": 1,
                    "pairing_generation": ""
                })
            })
            .collect();
        std::fs::create_dir_all(dir.join("peer")).unwrap();
        std::fs::write(
            dir.join("peer").join("devices.json"),
            serde_json::json!({"version":1,"devices":devices}).to_string(),
        )
        .unwrap();
    }

    fn host_api(data: &std::path::Path) -> Arc<dyn peer_network::ProtocolHandler> {
        let storage = Arc::new(storage::Storage::open(data).unwrap());
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
        Arc::new(remote_api::ApiProtocol::new(state))
    }

    /// A server that runs another release gets neither bot RPC nor copied bots and secrets from
    /// this computer, and the link still shows as connected so the app can say what to update.
    #[tokio::test]
    async fn a_server_of_another_release_gets_neither_rpc_nor_copies() {
        use builtin_tools::pc::RPC_BROWSER;
        use peer_network::{PeerConfig, RelayConfig};
        let host_dir = tempfile::tempdir().unwrap();
        let mut hc = PeerConfig::new(host_dir.path().join("peer"));
        hc.local_role = Role::Host;
        hc.relay = RelayConfig::Disabled;
        hc.bind_addr = Some("127.0.0.1:0".parse().unwrap());
        hc.extra_protocols
            .push((peer_network::ALPN_API.to_vec(), host_api(host_dir.path())));
        let host = PeerNetwork::start(hc, None).await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mut dc = PeerConfig::new(dir.path().join("peer"));
        dc.local_role = Role::Desktop;
        dc.relay = RelayConfig::Disabled;
        dc.bind_addr = Some("127.0.0.1:0".parse().unwrap());
        let net = PeerNetwork::start(dc, None).await.unwrap();
        let refusing: remote_api::Compatibility =
            Arc::new(|_| Err("the server runs another Praxi release".into()));
        let links = CliHostLinks::new(dir.path().to_path_buf(), Some(net.clone()))
            .with_compatibility(refusing);
        let invite = host.create_invite(Role::Desktop).await;
        let id = links.pair(&invite.ticket).await.unwrap().id;
        let timeout = std::time::Duration::from_secs(10);

        // Bot RPC (a bot using the server as its computer).
        let error = links
            .call_host(&id, RPC_BROWSER, serde_json::json!({}), timeout)
            .await
            .unwrap_err();
        assert!(
            matches!(&error, PcError::Message(text) if text.contains("different Praxi release")),
            "{error:?}"
        );
        // Copying bots and secrets (`host_copy`), reading bots, administration.
        for (method, path) in [
            ("GET", "/v1/bots"),
            ("post", "/v1/bots"),
            ("POST", "/v1/admin/import/bots"),
            ("PUT", "/v1/admin/providers/openai"),
        ] {
            let outcome = links
                .call(&id, method, path, Some(serde_json::json!({})))
                .await;
            assert!(
                matches!(outcome, Err(HostLinkError::Incompatible)),
                "{method} {path}"
            );
        }
        // Status and the update request still reach the server: that is how it gets fixed.
        for (method, path) in [("GET", "/v1/host"), ("POST", "/v1/admin/host-update")] {
            let outcome = links
                .call(
                    &id,
                    method,
                    path,
                    Some(serde_json::json!({"tag": "v0.1.0"})),
                )
                .await;
            assert!(outcome.is_ok(), "{method} {path}: {outcome:?}");
        }
        // The link is up, so the app can read the release and explain.
        let listed = links.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].state, HostLinkState::Connected);

        // The same link under a policy that accepts the server works as before.
        let accepting = CliHostLinks::new(dir.path().to_path_buf(), Some(net.clone()))
            .with_compatibility(Arc::new(|_| Ok(())));
        let bots = accepting.call(&id, "GET", "/v1/bots", None).await.unwrap();
        assert_eq!(bots.status, 200);
        drop((links, accepting));
        net.shutdown().await;
        host.shutdown().await;
    }

    /// A bot of this computer reaches a paired server's browser through the REAL
    /// link and the REAL host handler: the paired administrator desktop is served,
    /// a desktop the owner did not make administrator is refused with words, and
    /// an unknown server key has no link at all.
    #[tokio::test]
    async fn the_server_link_serves_only_the_paired_admin_desktop() {
        use builtin_tools::pc::{PcCaller, RPC_BROWSER};
        use peer_network::{PeerConfig, RelayConfig};
        let host_dir = tempfile::tempdir().unwrap();
        let browser = Arc::new(crate::server_target::tests::Browser::default());
        let b = Arc::clone(&browser);
        let handler = Arc::new(crate::server_target::ServerTargetHandler::new(
            crate::server_target::ServerTargetDeps {
                data: host_dir.path().to_path_buf(),
                audit: Arc::new(|_, _| Ok(())),
                redact: Arc::new(str::to_owned),
                browser: Some(Arc::new(move || {
                    Arc::clone(&b) as Arc<dyn builtin_tools::BrowserDriver>
                })),
            },
        ));
        let mut hc = PeerConfig::new(host_dir.path().join("peer"));
        hc.local_role = Role::Host;
        hc.relay = RelayConfig::Disabled;
        hc.bind_addr = Some("127.0.0.1:0".parse().unwrap());
        // A real host also answers `/v1/health`: this computer reads the release from it first.
        hc.extra_protocols
            .push((peer_network::ALPN_API.to_vec(), host_api(host_dir.path())));
        let host = PeerNetwork::start(hc, Some(handler)).await.unwrap();
        let desktop = |admin: bool| {
            let host = host.clone();
            async move {
                let dir = tempfile::tempdir().unwrap();
                let mut dc = PeerConfig::new(dir.path().join("peer"));
                dc.local_role = Role::Desktop;
                dc.relay = RelayConfig::Disabled;
                dc.bind_addr = Some("127.0.0.1:0".parse().unwrap());
                let net = PeerNetwork::start(dc, None).await.unwrap();
                let links = Arc::new(CliHostLinks::new(dir.path().to_path_buf(), Some(net)));
                let invite = host
                    .create_invite_with_options(
                        Role::Desktop,
                        std::time::Duration::from_secs(60),
                        admin,
                    )
                    .await;
                let info = links.pair(&invite.ticket).await.unwrap();
                (CliServerLinks(links), info.id, dir)
            }
        };
        let caller = PcCaller {
            id: "bot-1".into(),
            name: "Mausi".into(),
        };
        let params = |p: serde_json::Value| caller.attach(p);
        let open = serde_json::json!({"op": "open", "url": "https://93.184.216.34/"});
        let timeout = std::time::Duration::from_secs(20);

        let (admin_links, id, _d1) = desktop(true).await;
        let (_, link) = admin_links.link(&id).expect("the paired server has a link");
        let ok = link
            .call(RPC_BROWSER, params(open.clone()), timeout)
            .await
            .unwrap();
        assert!(
            ok.to_string().contains("opened https://93.184.216.34/"),
            "{ok}"
        );
        assert_eq!(browser.opened.lock().len(), 1);
        assert!(admin_links.link("not-a-paired-server").is_none());

        let (plain_links, id, _d2) = desktop(false).await;
        let (_, link) = plain_links.link(&id).unwrap();
        let err = link
            .call(RPC_BROWSER, params(open), timeout)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("administrator desktop"), "{err}");
        assert_eq!(
            browser.opened.lock().len(),
            1,
            "the refused desktop opened nothing"
        );
    }

    /// The REAL links against a real host endpoint: a plan, a copy and a second
    /// item all reach `connect` on an already-open proxy (this used to deadlock).
    #[tokio::test]
    async fn repeated_calls_on_an_open_connection_do_not_deadlock() {
        use peer_network::{ALPN_API, PeerConfig, RelayConfig};
        let host_dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(storage::Storage::open(host_dir.path()).unwrap());
        let agent = agent_core::AgentLoop::new(
            storage.clone(),
            tool_registry::ToolRegistry::new(),
            agent_core::Providers::single(Arc::new(daemon_api::fake::FakeProvider)),
            agent_core::LoopConfig::default(),
        );
        let state =
            daemon_api::AppState::new(agent, storage, daemon_api::DaemonConfig::new("t")).unwrap();
        let mut hc = PeerConfig::new(host_dir.path().join("peer"));
        hc.local_role = Role::Host;
        hc.relay = RelayConfig::Disabled;
        hc.bind_addr = Some("127.0.0.1:0".parse().unwrap());
        hc.extra_protocols.push((
            ALPN_API.to_vec(),
            Arc::new(remote_api::ApiProtocol::new(state)),
        ));
        let host = PeerNetwork::start(hc, None).await.unwrap();
        let desk_dir = tempfile::tempdir().unwrap();
        let mut dc = PeerConfig::new(desk_dir.path().join("peer"));
        dc.local_role = Role::Desktop;
        dc.relay = RelayConfig::Disabled;
        dc.bind_addr = Some("127.0.0.1:0".parse().unwrap());
        let desk = PeerNetwork::start(dc, None).await.unwrap();
        let links = CliHostLinks::new(desk_dir.path().to_path_buf(), Some(desk));
        let invite = host.create_invite(Role::Desktop).await;
        let info = links.pair(&invite.ticket).await.unwrap();
        for path in ["/v1/bots", "/v1/bots", "/v1/admin/state"] {
            let call = tokio::time::timeout(
                std::time::Duration::from_secs(20),
                links.call(&info.id, "GET", path, None),
            )
            .await
            .expect("call must not hang")
            .unwrap();
            assert!(call.status > 0);
        }
        host.shutdown().await;
    }

    #[test]
    fn list_shows_only_hosts_and_never_starts_the_network() {
        let dir = tempfile::tempdir().unwrap();
        write_devices(
            dir.path(),
            &[("aaaa", "host"), ("bbbb", "phone"), ("cccc", "desktop")],
        );
        let links = CliHostLinks::new(dir.path().to_path_buf(), None);
        let hosts = links.list();
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].id, "aaaa");
        assert_eq!(hosts[0].state, HostLinkState::Idle);
        assert!(links.started.try_lock().unwrap().is_none());
    }

    #[test]
    fn missing_pairing_file_is_an_empty_list() {
        let dir = tempfile::tempdir().unwrap();
        let links = CliHostLinks::new(dir.path().to_path_buf(), None);
        assert!(links.list().is_empty());
    }

    #[tokio::test]
    async fn connect_refuses_unknown_and_non_host_devices() {
        let dir = tempfile::tempdir().unwrap();
        write_devices(dir.path(), &[("bbbb", "phone")]);
        let links = CliHostLinks::new(dir.path().to_path_buf(), None);
        for id in ["bbbb", "zzzz"] {
            assert!(matches!(
                links.connect(id).await,
                Err(HostLinkError::NotFound)
            ));
        }
        assert!(links.started.lock().await.is_none());
    }

    #[tokio::test]
    async fn pairing_rejects_tickets_for_other_roles_before_any_network() {
        // Not a ticket at all.
        let dir = tempfile::tempdir().unwrap();
        let links = CliHostLinks::new(dir.path().to_path_buf(), None);
        assert!(matches!(
            links.pair("nonsense").await,
            Err(HostLinkError::Invalid(_))
        ));
        assert!(links.started.lock().await.is_none());
    }

    #[test]
    fn messages_are_single_line_and_bounded() {
        let long = format!("a\nb\x07{}", "x".repeat(500));
        let msg = safe(long);
        assert!(!msg.contains('\n') && !msg.contains('\x07'));
        assert!(msg.chars().count() <= 200);
    }
}
