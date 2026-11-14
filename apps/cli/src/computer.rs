//! Per-daemon [`ComputerProvider`] implementation wired into [`serve`](crate::serve).
//!
//! Bridges:
//! * `local` desktop (`local_desktop` from 054 Linux Xvfb or Windows),
//! * `pc` / `node:<id>` paired remote desktops over [`PeerNetwork`] via cached,
//!   on-demand [`RemoteDesktop`] clients, and
//! * [`PeerEvent`] monitoring to refresh connected PCs and auto-resume runs
//!   paused with `waiting_for_pc`.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use agent_core::{AgentLoop, ComputerProvider};
use desktop_api::DesktopBackend;
use parking_lot::RwLock;
use peer_network::{Device, PeerNetwork, RemoteDesktop, Role, key_from_string};
use tokio::sync::broadcast::error::RecvError;

const MONITOR_POLL_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone)]
struct CachedRemote {
    id: String,
    name: String,
    role: Role,
    backend: Arc<dyn DesktopBackend>,
}

/// Runtime computer availability exposed to [`AgentLoop`] by the daemon.
pub struct CliComputerProvider {
    host_mode: bool,
    local_desktop: Option<Arc<dyn DesktopBackend>>,
    has_display: bool,
    browser_available: bool,
    net: Option<PeerNetwork>,
    remotes: RwLock<HashMap<String, CachedRemote>>,
    active_pc_id: RwLock<Option<String>>,
    refresh_gate: tokio::sync::Mutex<()>,
}

impl CliComputerProvider {
    /// Build a provider for `serve` using detected local capabilities and the
    /// optional active [`PeerNetwork`].
    #[must_use]
    pub fn new(
        host_mode: bool,
        local_desktop: Option<Arc<dyn DesktopBackend>>,
        net: Option<PeerNetwork>,
    ) -> Self {
        let has_display = local_desktop.is_some() || browser_cdp::host::has_display();
        let browser_available = browser_cdp::find_browser().is_ok();
        Self {
            host_mode,
            local_desktop,
            has_display,
            browser_available,
            net,
            remotes: RwLock::new(HashMap::new()),
            active_pc_id: RwLock::new(None),
            refresh_gate: tokio::sync::Mutex::new(()),
        }
    }

    /// Override detected display/browser flags (for hermetic tests).
    #[cfg(test)]
    #[must_use]
    pub fn with_capabilities(mut self, has_display: bool, browser_available: bool) -> Self {
        self.has_display = has_display;
        self.browser_available = browser_available;
        self
    }

    /// Reconcile cached [`RemoteDesktop`] sessions against currently connected
    /// paired peers, connecting new ones on demand and pruning offline/revoked
    /// peers.
    pub async fn refresh_all(&self) {
        let Some(net) = &self.net else {
            return;
        };
        let _guard = self.refresh_gate.lock().await;
        let devices = net.devices();
        let mut online_candidates = Vec::new();
        for d in devices {
            let Ok(pk) = d.public_key() else {
                continue;
            };
            if is_pc_candidate(&d) && net.is_connected(&pk) {
                online_candidates.push((d, pk));
            }
        }

        // Drop any cached remotes whose peer disconnected or was unpaired.
        {
            let mut remotes = self.remotes.write();
            remotes.retain(|id, _| online_candidates.iter().any(|(d, _)| &d.id == id));
        }

        // Connect on demand for newly connected PC peers.
        for (device, pk) in &online_candidates {
            if self.remotes.read().contains_key(&device.id) {
                continue;
            }
            if let Ok(remote) = RemoteDesktop::connect(net, pk).await {
                let backend: Arc<dyn DesktopBackend> = Arc::new(remote);
                self.remotes.write().insert(
                    device.id.clone(),
                    CachedRemote {
                        id: device.id.clone(),
                        name: device.name.clone(),
                        role: device.role,
                        backend,
                    },
                );
            }
        }

        self.select_active_pc();
    }

    fn select_active_pc(&self) {
        let remotes = self.remotes.read();
        let current_still_valid = self
            .active_pc_id
            .read()
            .as_ref()
            .is_some_and(|id| remotes.contains_key(id));
        if current_still_valid {
            return;
        }
        // Prefer Role::Desktop over Role::Node when both are online, and break ties
        // deterministically by id.
        let best = remotes
            .values()
            .min_by_key(|r| (u8::from(r.role != Role::Desktop), r.id.clone()))
            .map(|r| r.id.clone());
        *self.active_pc_id.write() = best;
    }

    fn peer_connected(&self, id: &str) -> bool {
        let Some(net) = &self.net else {
            return true;
        };
        let Ok(pk) = key_from_string(id) else {
            return false;
        };
        net.is_connected(&pk)
    }
}

impl ComputerProvider for CliComputerProvider {
    fn local(&self) -> Option<Arc<dyn DesktopBackend>> {
        self.local_desktop.clone()
    }

    fn pc(&self) -> Option<Arc<dyn DesktopBackend>> {
        let active = self.active_pc_id.read().clone()?;
        let cached = self.remotes.read().get(&active).cloned()?;
        if !self.peer_connected(&cached.id) {
            return None;
        }
        Some(cached.backend)
    }

    fn is_host(&self) -> bool {
        self.host_mode
    }

    fn browser_available(&self) -> bool {
        self.browser_available
    }

    fn has_display(&self) -> bool {
        self.has_display
    }

    fn node(&self, id: &str) -> Option<Arc<dyn DesktopBackend>> {
        let wanted_key = key_from_string(id).ok();
        let cached = {
            let remotes = self.remotes.read();
            if let Some(exact) = remotes.get(id) {
                Some(exact.clone())
            } else {
                remotes
                    .values()
                    .find(|r| {
                        r.id.eq_ignore_ascii_case(id)
                            || r.name == id
                            || wanted_key
                                .as_ref()
                                .is_some_and(|k| key_from_string(&r.id).ok().as_ref() == Some(k))
                    })
                    .cloned()
            }
        }?;
        if !self.peer_connected(&cached.id) {
            return None;
        }
        Some(cached.backend)
    }

    fn prepare(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async {
            self.refresh_all().await;
        })
    }
}

fn is_pc_candidate(d: &Device) -> bool {
    let has_desktop_cap = d.capabilities.iter().any(|c| c == "desktop");
    match d.role {
        // Companion desktop app pairs with Role::Desktop; allow empty legacy caps too.
        Role::Desktop => d.capabilities.is_empty() || has_desktop_cap,
        // Headless/agent node (`bot-node`) uses Role::Node and advertises `"desktop"`.
        Role::Node => has_desktop_cap,
        Role::Phone | Role::Host => false,
    }
}

/// Subscribe to [`PeerNetwork`] events in the background, refreshing cached
/// remote PC connections and auto-resuming runs paused with `waiting_for_pc`.
pub fn spawn_peer_monitor(
    net: PeerNetwork,
    provider: Arc<CliComputerProvider>,
    agent: AgentLoop,
    weak_state: daemon_api::WeakAppState,
) {
    tokio::spawn(async move {
        let mut rx = net.subscribe();
        provider.refresh_all().await;
        let _ = agent.resume_waiting_for_pc().await;

        let mut poll = tokio::time::interval(MONITOR_POLL_INTERVAL);
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        poll.tick().await;

        loop {
            tokio::select! {
                ev = rx.recv() => {
                    match ev {
                        Ok(_) | Err(RecvError::Lagged(_)) => {
                            if weak_state.upgrade().is_none() {
                                break;
                            }
                            provider.refresh_all().await;
                            let _ = agent.resume_waiting_for_pc().await;
                        }
                        Err(RecvError::Closed) => break,
                    }
                }
                _ = poll.tick() => {
                    if weak_state.upgrade().is_none() {
                        break;
                    }
                    provider.refresh_all().await;
                    let _ = agent.resume_waiting_for_pc().await;
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use agent_core::{AgentLoop, BotProfile, LoopConfig, Providers};
    use async_trait::async_trait;
    use builtin_tools::{BuiltinConfig, ToolGate};
    use computer_target::{ComputerLabel, ComputerTarget, resolve};
    use daemon_api::{AppState, DaemonConfig, fake::FakeProvider};
    use desktop_api::{
        Action, ActionResult, Capabilities, DesktopError, Image, Quality, Region, Snapshot,
        TreeQuality, UiNode, WindowInfo,
        snapshot::{AppSpec, Pid, Rect},
    };
    use peer_network::{
        DesktopRpcHandler, HeartbeatConfig, PairTicket, PeerConfig, RelayConfig, key_from_string,
    };
    use permission_engine::Policy;
    use protocol::{BotId, ChatId, RunId, UserId};
    use storage::{Bot, Chat, Storage, User, now_ms};
    use tool_registry::ToolRegistry;

    use super::*;

    struct FakePcDesktop {
        label: &'static str,
        snapshots: AtomicUsize,
        clicks: AtomicUsize,
    }

    impl FakePcDesktop {
        fn new(label: &'static str) -> Arc<Self> {
            Arc::new(Self {
                label,
                snapshots: AtomicUsize::new(0),
                clicks: AtomicUsize::new(0),
            })
        }
    }

    #[async_trait]
    impl DesktopBackend for FakePcDesktop {
        async fn snapshot(&self, _scope: Option<u64>) -> Result<Snapshot, DesktopError> {
            self.snapshots.fetch_add(1, Ordering::SeqCst);
            let nodes = vec![UiNode::new(
                0,
                "button",
                format!("OK-{}", self.label),
                None,
                Some(Rect {
                    x: 10.0,
                    y: 10.0,
                    width: 60.0,
                    height: 24.0,
                }),
                vec![],
            )];
            let quality = TreeQuality::assess(&nodes);
            Ok(Snapshot {
                window: WindowInfo {
                    id: 1,
                    title: format!("PC-{}", self.label),
                    pid: 42,
                    bounds: Rect {
                        x: 0.0,
                        y: 0.0,
                        width: 800.0,
                        height: 600.0,
                    },
                    focused: true,
                },
                nodes,
                generation: 1,
                quality,
            })
        }

        async fn act(&self, action: Action) -> Result<ActionResult, DesktopError> {
            if matches!(action, Action::Click { .. }) {
                self.clicks.fetch_add(1, Ordering::SeqCst);
            }
            Ok(ActionResult {
                dispatched: true,
                verified: Some(true),
                new_snapshot: None,
            })
        }

        async fn screenshot(
            &self,
            _region: Region,
            _quality: Quality,
        ) -> Result<Image, DesktopError> {
            Ok(Image {
                data: vec![1, 2, 3],
                width: 1,
                height: 1,
                mime_type: "image/png".into(),
            })
        }

        async fn windows(&self) -> Result<Vec<WindowInfo>, DesktopError> {
            Ok(vec![])
        }

        async fn launch(&self, _app: AppSpec) -> Result<Pid, DesktopError> {
            Ok(100)
        }

        fn capabilities(&self) -> Capabilities {
            Capabilities {
                platform: "windows".into(),
                accessibility_tree: true,
                screenshot: true,
                app_launch: true,
                has_display: true,
            }
        }
    }

    struct ReplaceableGate;

    #[async_trait]
    impl ToolGate for ReplaceableGate {
        async fn authorize(
            &self,
            _req: &builtin_tools::GateRequest,
        ) -> std::result::Result<(), String> {
            Err("agent loop replaces this gate".into())
        }
    }

    fn loopback_peer_cfg(
        dir: &std::path::Path,
        role: Role,
        name: &str,
        caps: Vec<&str>,
    ) -> PeerConfig {
        let mut cfg = PeerConfig::new(dir.to_path_buf());
        cfg.relay = RelayConfig::Disabled;
        cfg.bind_addr = Some("127.0.0.1:0".parse().unwrap());
        cfg.local_role = role;
        cfg.display_name = name.into();
        cfg.capabilities = caps.into_iter().map(str::to_owned).collect();
        cfg
    }

    #[test]
    fn pc_candidate_filter_and_resolution_matrix() {
        let key = key_from_string(&"0".repeat(64));
        let make_device = |role: Role, caps: Vec<&str>| Device {
            id: key
                .as_ref()
                .map_or_else(|_| "0".repeat(64), ToString::to_string),
            name: "pc".into(),
            role,
            capabilities: caps.into_iter().map(str::to_owned).collect(),
            addr: None,
            paired_at: 1,
        };

        assert!(is_pc_candidate(&make_device(Role::Desktop, vec![])));
        assert!(is_pc_candidate(&make_device(
            Role::Desktop,
            vec!["desktop"]
        )));
        assert!(is_pc_candidate(&make_device(
            Role::Node,
            vec!["desktop", "browser"]
        )));
        assert!(!is_pc_candidate(&make_device(Role::Node, vec![])));
        assert!(!is_pc_candidate(&make_device(Role::Node, vec!["browser"])));
        assert!(!is_pc_candidate(&make_device(Role::Phone, vec!["desktop"])));
        assert!(!is_pc_candidate(&make_device(Role::Host, vec!["desktop"])));

        let vps_no_pc = CliComputerProvider::new(true, None, None).with_capabilities(false, true);
        let auto_res = resolve(&ComputerTarget::Auto, &vps_no_pc);
        assert_eq!(auto_res.label, ComputerLabel::Browser);
        assert!(auto_res.browser);
        assert!(!auto_res.wait_for_pc);

        let pc_res = resolve(&ComputerTarget::Pc, &vps_no_pc);
        assert!(pc_res.wait_for_pc);

        let local_app = CliComputerProvider::new(false, Some(FakePcDesktop::new("local")), None)
            .with_capabilities(true, false);
        let pc_local = resolve(&ComputerTarget::Pc, &local_app);
        assert_eq!(pc_local.label, ComputerLabel::Pc);
        assert!(pc_local.desktop.is_some());
        assert!(!pc_local.wait_for_pc);
    }

    fn setup_pc_host_harness(
        host_data: &std::path::Path,
        host_net: &PeerNetwork,
    ) -> (
        Arc<Storage>,
        ChatId,
        Arc<CliComputerProvider>,
        AgentLoop,
        AppState,
    ) {
        let storage = Arc::new(Storage::open(host_data).unwrap());
        let (user_id, bot_id, chat_id) = (UserId::new(), BotId::new(), ChatId::new());
        storage
            .insert_user(&User {
                id: user_id,
                name: "u".into(),
                created_at_ms: now_ms(),
            })
            .unwrap();
        let profile = BotProfile::from_toml_str(
            "[bot]\nid = \"pcbot\"\nname = \"PC\"\n[models]\nexecutor = \"fake/echo\"\n[runtime]\ncomputer = \"pc\"\n",
        )
        .unwrap();
        storage
            .insert_bot(&Bot {
                id: bot_id,
                user_id,
                name: "PC".into(),
                config: serde_json::json!({}),
                created_at_ms: now_ms(),
            })
            .unwrap();
        storage
            .insert_chat(&Chat {
                id: chat_id,
                user_id,
                bot_id,
                title: "c".into(),
                created_at_ms: now_ms(),
            })
            .unwrap();

        let provider = Arc::new(
            CliComputerProvider::new(true, None, Some(host_net.clone()))
                .with_capabilities(false, false),
        );
        let mut bcfg = BuiltinConfig::new(host_data.join("bt"), Arc::new(ReplaceableGate));
        bcfg.dynamic_desktop = true;
        let agent = AgentLoop::new(
            storage.clone(),
            ToolRegistry::new(),
            Providers::single(Arc::new(FakeProvider)),
            LoopConfig {
                global_policy: Policy::full(),
                ..LoopConfig::default()
            },
        )
        .with_builtin_tools(bcfg);
        agent.add_profile(bot_id, profile);
        agent.set_computer_provider(provider.clone());

        let state = AppState::new(
            agent.clone(),
            storage.clone(),
            DaemonConfig::new("host-only-token"),
        )
        .unwrap();
        spawn_peer_monitor(
            host_net.clone(),
            provider.clone(),
            agent.clone(),
            state.downgrade(),
        );
        (storage, chat_id, provider, agent, state)
    }

    async fn wait_for_run_state(
        storage: &Storage,
        run_id: RunId,
        target_state: &str,
        max_polls: usize,
    ) -> (Option<String>, Option<String>) {
        for _ in 0..max_polls {
            if let Some(r) = storage.get_run(run_id).unwrap()
                && r.state == target_state
            {
                return (r.computer_target, r.pause_reason);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("run {run_id:?} did not reach state {target_state:?}");
    }

    async fn start_pc_peer(
        host_net: &PeerNetwork,
        pc_peer_dir: &std::path::Path,
        label: &'static str,
        pair: bool,
    ) -> (Arc<FakePcDesktop>, PeerNetwork, tokio::task::JoinHandle<()>) {
        let backend = FakePcDesktop::new(label);
        let pc_net = PeerNetwork::start(
            loopback_peer_cfg(pc_peer_dir, Role::Node, "my-pc", vec!["desktop"]),
            Some(Arc::new(DesktopRpcHandler::new(backend.clone()))),
        )
        .await
        .unwrap();
        if pair {
            let invite = host_net.create_invite(Role::Node).await;
            pc_net
                .pair(&PairTicket::parse(&invite.ticket).unwrap())
                .await
                .unwrap();
        }
        let hb_cfg = HeartbeatConfig {
            interval: Duration::from_millis(80),
            initial_backoff: Duration::from_millis(20),
            max_backoff: Duration::from_millis(100),
            connect_timeout: Duration::from_secs(1),
        };
        let hb = pc_net.spawn_heartbeat(host_net.id(), hb_cfg, Arc::new(|_| {}));
        (backend, pc_net, hb)
    }

    async fn disconnect_pc_and_wait_drop(
        host_net: &PeerNetwork,
        pc_net: PeerNetwork,
        hb: tokio::task::JoinHandle<()>,
        provider: &CliComputerProvider,
    ) {
        let pc_id = pc_net.id();
        hb.abort();
        pc_net.shutdown().await;
        for _ in 0..200 {
            if !host_net.is_connected(&pc_id) {
                provider.refresh_all().await;
                if provider.pc().is_none() {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("provider did not drop RemoteDesktop after PC disconnected");
    }

    #[tokio::test]
    async fn waiting_for_pc_auto_resumes_and_reconnects_over_peer_network() {
        let dir = tempfile::tempdir().unwrap();
        let host_data = dir.path().join("host");
        let pc_peer_dir = dir.path().join("pc-node");
        std::fs::create_dir_all(&host_data).unwrap();
        std::fs::create_dir_all(&pc_peer_dir).unwrap();

        let host_net = PeerNetwork::start(
            loopback_peer_cfg(&host_data.join("peer"), Role::Host, "vps", vec![]),
            None,
        )
        .await
        .unwrap();
        let (storage, chat_id, provider, agent, state) =
            setup_pc_host_harness(&host_data, &host_net);

        let prompt = "call:desktop_get_state {}\ncall:desktop_click {\"index\":0,\"gen\":$gen}";

        // 1. No PC connected -> run enters durable waiting_for_pc pause.
        let run1 = agent.run_turn(chat_id, prompt).unwrap();
        let (target, pause_reason) = wait_for_run_state(&storage, run1, "paused", 200).await;
        assert_eq!(pause_reason.as_deref(), Some("waiting_for_pc"));
        assert_eq!(target.as_deref(), Some("pc"));

        // 2. Pair and connect a PC node serving DesktopRpcHandler -> monitor auto-resumes run1.
        let (pc_backend1, pc_net1, hb1) =
            start_pc_peer(&host_net, &pc_peer_dir, "first", true).await;
        let (target, pause_reason) = wait_for_run_state(&storage, run1, "succeeded", 400).await;
        assert_eq!(pause_reason, None);
        assert_eq!(target.as_deref(), Some("pc"));
        assert!(pc_backend1.snapshots.load(Ordering::SeqCst) >= 1);
        assert_eq!(pc_backend1.clicks.load(Ordering::SeqCst), 1);

        // 3. Disconnect PC -> provider drops remote desktop; reconnect -> refreshes RemoteDesktop.
        disconnect_pc_and_wait_drop(&host_net, pc_net1, hb1, &provider).await;
        let (pc_backend2, pc_net2, hb2) =
            start_pc_peer(&host_net, &pc_peer_dir, "second", false).await;

        let mut reconnected = false;
        for _ in 0..200 {
            if provider.pc().is_some() {
                reconnected = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(
            reconnected,
            "monitor should reconnect RemoteDesktop after PC restarts"
        );

        let run2 = agent.run_turn(chat_id, prompt).unwrap();
        let _ = wait_for_run_state(&storage, run2, "succeeded", 400).await;
        assert!(pc_backend2.snapshots.load(Ordering::SeqCst) >= 1);
        assert_eq!(pc_backend2.clicks.load(Ordering::SeqCst), 1);

        hb2.abort();
        drop(state);
        pc_net2.shutdown().await;
        host_net.shutdown().await;
    }
}
