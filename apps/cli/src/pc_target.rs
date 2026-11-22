//! "My computer is a target for my bots" (card 258-A): this computer's daemon
//! serves the paired host's desktop requests, under the owner's consent.
//!
//! Authority model, all enforced here before the shared desktop RPC runs
//! anything:
//! 1. Only the paired device with role `host` is served, and the pairing is
//!    looked up again (role, generation, switch) right before every dispatch,
//!    so unpairing or switching off stops what is already parked.
//! 2. Off by default, per host, by the owner (`PUT /v1/hosts/{id}/computer-access`,
//!    owner authority only). Serving also needs the owner's desktop app to be
//!    running (its heartbeat is the lease).
//! 3. Reads: the first look each session asks the owner, per server, BOT and
//!    family (the "always" switch covers only a run placed on this computer,
//!    never a named bot's `pc_*` tools); Praxi's own windows never appear in a read (listing, snapshot,
//!    screenshot); an observation is discarded when the human took control
//!    while it was captured; every read is audited (an audit failure blocks it)
//!    and shows a "looking" indicator.
//! 4. Input (`desktop.act`, `desktop.launch`) always waits for the owner on a
//!    card built HERE from the parsed request and this OS (the program and
//!    window that would receive it, the complete masked text, the keys), is
//!    bound to that program at dispatch (`act_guarded`) and is refused for
//!    Praxi's own windows, password fields and launcher key chords.
//! 5. Every decision is written to the local audit log.
//!
//! Nothing new listens: requests arrive over the existing paired iroh link.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use agent_core::ControlGate;
use async_trait::async_trait;
use builtin_tools::BrowserDriver;
use builtin_tools::pc::{
    CALLER_KEY, PcFamily, PcInfo, RPC_BROWSER, RPC_INFO, RPC_TOOL, SERVED_TOOLS,
};
use daemon_api::pc_access::{PcPreview, PcRequests};
use desktop_api::{
    ActGuard, Action, ActionResult, Capabilities, ClickMethod, CoordinateSpace, DesktopBackend,
    DesktopError, Image, InputTarget, Quality, Region, Snapshot, Target, WindowInfo,
    snapshot::{AppSpec, Pid, Rect},
};
use parking_lot::Mutex;
use peer_network::proto::{ERR_FORBIDDEN, ERR_INVALID_PARAMS, ERR_METHOD_NOT_FOUND, RpcError};
use peer_network::{DesktopRpcHandler, Device, Role, RpcHandler, desktop_error_to_rpc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tool_registry::{Tool, ToolContext};

/// How long the owner has to answer a card before it is denied. Must stay below
/// `peer_network::ACTION_RPC_TIMEOUT` (the transport deadline of such calls).
pub const ASK_TIMEOUT: Duration = Duration::from_secs(120);

/// Longest text / command a card will show completely. Anything longer is
/// refused instead of being cut.
const MAX_PAYLOAD_CHARS: usize = 1500;

fn yes() -> bool {
    true
}

/// What the owner allowed one host. A file written before the families existed
/// has no `screen` key and meant "screen and input on", so that defaults to on;
/// every other family is off until turned on.
// One flag per family the owner switches separately.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct HostAccess {
    /// Screen and input.
    #[serde(default = "yes")]
    screen: bool,
    /// The first look each session needs no card.
    #[serde(default)]
    always_read: bool,
    #[serde(default)]
    browser: bool,
    #[serde(default)]
    commands: bool,
    #[serde(default)]
    files: bool,
}

/// The owner's switches for one host (what `PUT .../computer-access` carries).
// One flag per family the owner switches separately.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Switches {
    pub screen: bool,
    pub always_read: bool,
    pub browser: bool,
    pub commands: bool,
    pub files: bool,
}

impl Switches {
    fn any(self) -> bool {
        self.screen || self.browser || self.commands || self.files
    }
}

#[derive(Default, Serialize, Deserialize)]
struct AccessFile {
    #[serde(default)]
    hosts: BTreeMap<String, HostAccess>,
}

/// Hosts whose bots may use this computer. Persisted next to the pairing file.
pub struct AccessStore {
    path: PathBuf,
    state: Mutex<AccessFile>,
    /// `(host, bot, family)` whose first look this session was allowed (memory
    /// only). The bot is `""` for a run placed on this computer that names
    /// none; an allowance for one bot never covers another.
    session_reads: Mutex<BTreeSet<(String, String, &'static str)>>,
    /// How many times a family was switched off for a host. An approval that
    /// was asked before the latest revocation never runs (a delivered "allow"
    /// cannot be taken back, so it is checked against this instead).
    revocations: Mutex<BTreeMap<(String, &'static str), u64>>,
}

impl AccessStore {
    #[must_use]
    pub fn open(data: &Path) -> Self {
        let path = data.join("peer").join("computer-access.json");
        let state = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str::<AccessFile>(&t).ok())
            .unwrap_or_default();
        Self {
            path,
            state: Mutex::new(state),
            session_reads: Mutex::new(BTreeSet::new()),
            revocations: Mutex::new(BTreeMap::new()),
        }
    }

    /// Revocation counter of one family of one host.
    #[must_use]
    pub fn family_gen(&self, host_id: &str, family: PcFamily) -> u64 {
        self.revocations
            .lock()
            .get(&(host_id.to_owned(), family.as_str()))
            .copied()
            .unwrap_or(0)
    }

    /// The family is on NOW and has not been switched off since `gen` was read.
    #[must_use]
    pub fn family_valid(&self, host_id: &str, family: PcFamily, gen_then: u64) -> bool {
        self.family_on(host_id, family) && self.family_gen(host_id, family) == gen_then
    }

    /// Any family is on for this host (the pairing is served at all).
    #[must_use]
    pub fn enabled(&self, host_id: &str) -> bool {
        self.state.lock().hosts.contains_key(host_id)
    }

    /// This family is on for this host.
    #[must_use]
    pub fn family_on(&self, host_id: &str, family: PcFamily) -> bool {
        self.state
            .lock()
            .hosts
            .get(host_id)
            .is_some_and(|h| match family {
                PcFamily::Screen => h.screen,
                PcFamily::Browser => h.browser,
                PcFamily::Commands => h.commands,
                PcFamily::Files => h.files,
            })
    }

    #[must_use]
    pub fn switches(&self, host_id: &str) -> Switches {
        self.state
            .lock()
            .hosts
            .get(host_id)
            .map_or_else(Switches::default, |h| Switches {
                screen: h.screen,
                always_read: h.always_read,
                browser: h.browser,
                commands: h.commands,
                files: h.files,
            })
    }

    #[must_use]
    pub fn always_read(&self, host_id: &str) -> bool {
        self.state
            .lock()
            .hosts
            .get(host_id)
            .is_some_and(|h| h.always_read)
    }

    #[must_use]
    pub fn enabled_ids(&self) -> Vec<String> {
        self.state.lock().hosts.keys().cloned().collect()
    }

    fn session_read_allowed(&self, host_id: &str, bot: &str, family: PcFamily) -> bool {
        self.session_reads
            .lock()
            .contains(&(host_id.to_owned(), bot.to_owned(), family.as_str()))
    }

    fn allow_session_read(&self, host_id: &str, bot: &str, family: PcFamily) {
        self.session_reads
            .lock()
            .insert((host_id.to_owned(), bot.to_owned(), family.as_str()));
    }

    /// Screen and input only (the other families keep their switches);
    /// `on = false` removes every switch of the host.
    ///
    /// # Errors
    /// The file could not be written; the in-memory switch is left unchanged.
    pub fn set(&self, host_id: &str, on: bool, always_read: bool) -> std::io::Result<()> {
        let mut current = self.switches(host_id);
        if on {
            current.screen = true;
            current.always_read = always_read;
        } else {
            current = Switches::default();
        }
        self.set_switches(host_id, current)
    }

    /// Replace every switch of one host; none on removes the host.
    ///
    /// # Errors
    /// The file could not be written; the in-memory switch is left unchanged.
    pub fn set_switches(&self, host_id: &str, sw: Switches) -> std::io::Result<()> {
        let mut state = self.state.lock();
        let mut next = AccessFile {
            hosts: state.hosts.clone(),
        };
        if sw.any() {
            next.hosts.insert(
                host_id.to_owned(),
                HostAccess {
                    screen: sw.screen,
                    always_read: sw.screen && sw.always_read,
                    browser: sw.browser,
                    commands: sw.commands,
                    files: sw.files,
                },
            );
        } else {
            next.hosts.remove(host_id);
        }
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&self.path, serde_json::to_vec(&next)?)?;
        let before = Self::state_families(&state, host_id);
        *state = next;
        // Switching a family off revokes what was asked before.
        let after = Self::state_families(&state, host_id);
        {
            let mut revoked = self.revocations.lock();
            for fam in before.iter().filter(|f| !after.contains(f)) {
                *revoked.entry((host_id.to_owned(), *fam)).or_insert(0) += 1;
            }
        }
        // Switching a family off ends the session allowance of it.
        let still = Self::state_families(&state, host_id);
        self.session_reads
            .lock()
            .retain(|(h, _, fam)| h != host_id || still.contains(fam));
        Ok(())
    }

    fn state_families(state: &AccessFile, host_id: &str) -> Vec<&'static str> {
        let Some(h) = state.hosts.get(host_id) else {
            return Vec::new();
        };
        let mut v = Vec::new();
        if h.screen {
            v.push("screen");
        }
        if h.browser {
            v.push("browser");
        }
        if h.commands {
            v.push("commands");
        }
        if h.files {
            v.push("files");
        }
        v
    }
}

#[derive(Deserialize)]
struct PairingFile {
    #[serde(default)]
    devices: Vec<Device>,
}

/// The pairing as it is on disk right now (revoking removes the entry).
pub(crate) fn paired_device(data: &Path, id: &str) -> Option<Device> {
    let text = std::fs::read_to_string(data.join("peer").join("devices.json")).ok()?;
    serde_json::from_str::<PairingFile>(&text)
        .ok()?
        .devices
        .into_iter()
        .find(|d| d.id == id)
}

// ── Praxi's own windows ───────────────────────────────────────────────────────

fn is_praxi_title(title: &str) -> bool {
    title.to_lowercase().contains("praxi")
}

/// A window of Praxi itself (the app, its approval cards, the overlay): this
/// daemon's own process, or a title naming Praxi. A title is only a heuristic;
/// it fails toward hiding more.
fn is_praxi_window(w: &WindowInfo, own_pid: u32) -> bool {
    w.pid == own_pid || is_praxi_title(&w.title)
}

fn is_praxi_target(t: &InputTarget, own_pid: u32) -> bool {
    t.identity.pid == own_pid
        || is_praxi_title(&t.title)
        || t.identity.program_stem().contains("praxi")
}

fn overlaps(a: &Rect, b: &Rect) -> bool {
    a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height
}

fn contains(r: &Rect, x: f64, y: f64) -> bool {
    x >= r.x && x <= r.x + r.width && y >= r.y && y <= r.y + r.height
}

// ── Read channels: a backend that never shows Praxi's own UI ─────────────────

/// Wraps the real backend for every READ the host can make. Input never goes
/// through it (the handler dispatches input itself, guarded).
struct ReadBackend {
    inner: Arc<dyn DesktopBackend>,
    gate: Arc<dyn ControlGate>,
    own_pid: u32,
}

impl ReadBackend {
    fn control_refusal(&self) -> Option<DesktopError> {
        self.gate
            .observation_refused()
            .map(DesktopError::Accessibility)
    }

    fn discarded() -> DesktopError {
        DesktopError::Accessibility(
            "the user took control of the screen while this was captured, so it was discarded"
                .into(),
        )
    }
}

#[async_trait]
impl DesktopBackend for ReadBackend {
    async fn snapshot(&self, scope: Option<u64>) -> Result<Snapshot, DesktopError> {
        if let Some(e) = self.control_refusal() {
            return Err(e);
        }
        let epoch = self.gate.observation_epoch();
        let snap = self.inner.snapshot(scope).await?;
        if self.gate.observation_epoch() != epoch || self.gate.observation_refused().is_some() {
            return Err(Self::discarded());
        }
        if is_praxi_window(&snap.window, self.own_pid) {
            return Err(DesktopError::Accessibility(
                "that window belongs to Praxi and is not shared".into(),
            ));
        }
        Ok(snap)
    }

    async fn act(&self, _action: Action) -> Result<ActionResult, DesktopError> {
        Err(DesktopError::GuardRefused(
            "input is not served on this channel".into(),
        ))
    }

    async fn screenshot(&self, region: Region, quality: Quality) -> Result<Image, DesktopError> {
        if let Some(e) = self.control_refusal() {
            return Err(e);
        }
        let windows = self.inner.windows().await?;
        let area = match &region {
            Region::FullScreen => None,
            Region::Window { window_id } => {
                let w = windows
                    .iter()
                    .find(|w| w.id == *window_id)
                    .ok_or_else(|| DesktopError::Screenshot("that window is not visible".into()))?;
                if is_praxi_window(w, self.own_pid) {
                    return Err(DesktopError::Screenshot(
                        "that window belongs to Praxi and is not shared".into(),
                    ));
                }
                Some(w.bounds)
            }
            Region::Rect(r) => Some(*r),
        };
        // Pixels of a Praxi window (a pending card, the key fields) must never
        // leave this computer: refuse when one lies in the captured area.
        let covered = windows
            .iter()
            .filter(|w| is_praxi_window(w, self.own_pid))
            .any(|w| area.as_ref().is_none_or(|a| overlaps(a, &w.bounds)));
        if covered {
            return Err(DesktopError::Screenshot(
                "a Praxi window is open in this area, so the screenshot was not taken; ask for a specific other window".into(),
            ));
        }
        let epoch = self.gate.observation_epoch();
        let image = self.inner.screenshot(region, quality).await?;
        if self.gate.observation_epoch() != epoch || self.gate.observation_refused().is_some() {
            return Err(Self::discarded());
        }
        Ok(image)
    }

    async fn windows(&self) -> Result<Vec<WindowInfo>, DesktopError> {
        if let Some(e) = self.control_refusal() {
            return Err(e);
        }
        let epoch = self.gate.observation_epoch();
        let all = self.inner.windows().await?;
        if self.gate.observation_epoch() != epoch {
            return Err(Self::discarded());
        }
        Ok(all
            .into_iter()
            .filter(|w| !is_praxi_window(w, self.own_pid))
            .collect())
    }

    async fn launch(&self, _app: AppSpec) -> Result<Pid, DesktopError> {
        Err(DesktopError::GuardRefused(
            "input is not served on this channel".into(),
        ))
    }

    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }
}

// ── The handler ──────────────────────────────────────────────────────────────

/// Writes one audit row; an error blocks what it was written for.
pub type AuditFn = Arc<dyn Fn(&str, Value) -> Result<(), String> + Send + Sync>;

/// Makes a NEW managed browser for one bot of one server (its own process
/// and profile: tabs, page state and cookies are never shared).
pub type BrowserFactory = Arc<dyn Fn() -> Arc<dyn BrowserDriver> + Send + Sync>;

/// `(server, bot)` a managed browser belongs to.
type BrowserKey = (String, String);

/// At most this many bots' browsers are open at once; the oldest is closed.
const MAX_BROWSERS: usize = 3;

/// The bot of the server a request is for (`bot` in the request).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Caller {
    pub(crate) id: String,
    pub(crate) name: String,
}

/// The calling bot of a request: `None` when the request names none (a run
/// placed on this computer); a malformed one is refused.
pub(crate) fn caller_of(params: &Value) -> Result<Option<Caller>, String> {
    let Some(v) = params.get(CALLER_KEY) else {
        return Ok(None);
    };
    let id = v
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| {
            !id.is_empty()
                && id.len() <= 64
                && id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })
        .ok_or("the request names its bot in a form this computer does not accept")?;
    let name = v
        .get("name")
        .and_then(Value::as_str)
        .filter(|n| !n.trim().is_empty())
        .unwrap_or(id);
    if name.chars().count() > 64
        || name.chars().any(char::is_control)
        || protocol::approval_text::hides_characters(name)
    {
        return Err("the bot's name cannot be shown completely, so the request was refused".into());
    }
    Ok(Some(Caller {
        id: id.to_owned(),
        name: name.trim().to_owned(),
    }))
}

fn bot_key(caller: Option<&Caller>) -> &str {
    caller.map_or("", |c| c.id.as_str())
}

/// Everything the handler needs from the daemon.
pub struct PcTargetDeps {
    pub raw: Arc<dyn DesktopBackend>,
    pub data: PathBuf,
    pub access: Arc<AccessStore>,
    pub requests: Arc<PcRequests>,
    pub gate: Arc<dyn ControlGate>,
    /// The owner's desktop app is running (its heartbeat is live).
    pub owner_ui_live: Arc<dyn Fn() -> bool + Send + Sync>,
    /// Write an audit row; a failure blocks reads.
    pub audit: AuditFn,
    /// Mask owner secrets in text shown on a card.
    pub redact: Arc<dyn Fn(&str) -> String + Send + Sync>,
    pub own_pid: u32,
    /// The real `shell_run` / `fs_*` tools ([`builtin_tools::pc::local_tools`]),
    /// reachable only through `pc.tool` behind the owner's switches.
    pub tools: Vec<Arc<dyn Tool>>,
    /// Makes this computer's managed browser, one per bot; `None` = the
    /// browser is not served.
    pub browser: Option<BrowserFactory>,
}

/// Where this computer's served browser may connect (review blocker 4): the
/// network policy of its requests. Loopback (the daemon's own API, admin UIs),
/// link-local and cloud-metadata addresses are never reachable; a private-range
/// host only after the person at this computer allowed exactly that host.
struct BrowserNet {
    access: Arc<AccessStore>,
    requests: Arc<PcRequests>,
    /// The server and the bot this browser belongs to (fixed at creation).
    peer: Device,
    bot: Caller,
    /// `(host, revocation generation)` the owner allowed THIS bot this session.
    allowed: Mutex<BTreeSet<(String, u64)>>,
    timeout: Duration,
}

impl BrowserNet {
    async fn check(&self, url: String) -> Result<(), String> {
        let host = match browser_cdp::classify_url(&url).await {
            browser_cdp::Verdict::Allow => return Ok(()),
            browser_cdp::Verdict::Block(why) => {
                return Err(format!("blocked: {why}"));
            }
            browser_cdp::Verdict::Private(host) => host,
        };
        self.private_host(host, url).await
    }

    /// A private-range host: only with this computer's own card, per bot.
    async fn private_host(&self, host: String, shown: String) -> Result<(), String> {
        let peer = &self.peer;
        let gen_then = self.access.family_gen(&peer.id, PcFamily::Browser);
        if !self
            .access
            .family_valid(&peer.id, PcFamily::Browser, gen_then)
        {
            return Err("blocked: the browser is not allowed on this computer".into());
        }
        let key = (host.clone(), gen_then);
        if self.allowed.lock().contains(&key) {
            return Ok(());
        }
        let ok = self
            .requests
            .ask_as(
                &peer.id,
                &peer.name,
                Some(&self.bot.name),
                PcPreview {
                    kind: "act",
                    action: "browser private host".into(),
                    target: format!("{host} (an address inside your own network)"),
                    payload: shown,
                },
                self.timeout,
            )
            .await;
        if ok
            && self
                .access
                .family_valid(&peer.id, PcFamily::Browser, gen_then)
        {
            self.allowed.lock().insert(key);
            Ok(())
        } else {
            Err("blocked: a private network address was not allowed".into())
        }
    }
}

/// The shared desktop RPC surface behind the owner's consent.
pub struct HostDesktopHandler {
    reads: DesktopRpcHandler,
    deps: PcTargetDeps,
    ask_timeout: Duration,
    /// One browser per `(server, bot)`, oldest first.
    browsers: Mutex<Vec<(BrowserKey, Arc<dyn BrowserDriver>)>>,
}

impl HostDesktopHandler {
    #[must_use]
    pub fn new(deps: PcTargetDeps) -> Self {
        let reads = DesktopRpcHandler::new(Arc::new(ReadBackend {
            inner: Arc::clone(&deps.raw),
            gate: Arc::clone(&deps.gate),
            own_pid: deps.own_pid,
        }));
        Self {
            reads,
            deps,
            ask_timeout: ASK_TIMEOUT,
            browsers: Mutex::new(Vec::new()),
        }
    }

    #[cfg(test)]
    fn with_ask_timeout(mut self, t: Duration) -> Self {
        self.ask_timeout = t;
        self
    }

    /// The browser of one bot of one server: its own instance (tabs, page
    /// state, cookies) with its own network policy and private-host
    /// allowances. Made on first use; the oldest is closed past the cap.
    fn browser_for(&self, peer: &Device, bot: &Caller) -> Option<Arc<dyn BrowserDriver>> {
        let make = self.deps.browser.as_ref()?;
        let key = (peer.id.clone(), bot.id.clone());
        let mut open = self.browsers.lock();
        if let Some((_, b)) = open.iter().find(|(k, _)| *k == key) {
            return Some(Arc::clone(b));
        }
        let driver = make();
        let net = Arc::new(BrowserNet {
            access: Arc::clone(&self.deps.access),
            requests: Arc::clone(&self.deps.requests),
            peer: peer.clone(),
            bot: bot.clone(),
            allowed: Mutex::new(BTreeSet::new()),
            timeout: self.ask_timeout,
        });
        let host_net = Arc::clone(&net);
        driver.set_served_network(browser_cdp::ServedNetwork {
            check: browser_cdp::DestinationCheck::new(move |url| {
                let net = Arc::clone(&net);
                async move { net.check(url).await }
            }),
            // The proxy resolved the name itself: a private-range host asks
            // the person at this computer (once per bot and host).
            private_host: browser_cdp::DestinationCheck::new(move |host: String| {
                let net = Arc::clone(&host_net);
                async move {
                    let shown = format!("the server's bot asks to connect to {host}");
                    net.private_host(host, shown).await
                }
            }),
        });
        open.push((key, Arc::clone(&driver)));
        while open.len() > MAX_BROWSERS {
            let (_, old) = open.remove(0);
            tokio::spawn(async move { old.close_all().await });
        }
        Some(driver)
    }

    /// Park a card for this request (the bot's name on it) and wait.
    async fn ask(&self, peer: &Device, caller: Option<&Caller>, preview: PcPreview) -> bool {
        self.deps
            .requests
            .ask_as(
                &peer.id,
                &peer.name,
                caller.map(|c| c.name.as_str()),
                preview,
                self.ask_timeout,
            )
            .await
    }

    fn audit(&self, action: &str, detail: Value) -> Result<(), String> {
        (self.deps.audit)(action, detail)
    }

    fn refuse(&self, peer: &Device, method: &str, why: &str) -> RpcError {
        let _ = self.audit(
            "pc_target.refused",
            serde_json::json!({"host": peer.id, "method": method, "why": why}),
        );
        RpcError::new(ERR_FORBIDDEN, why)
    }

    /// The pairing, the role and the switch as they are NOW. Called right
    /// before anything is dispatched, not only when the request arrived.
    fn still_authorized(&self, peer: &Device) -> Result<(), &'static str> {
        let Some(now) = paired_device(&self.deps.data, &peer.id) else {
            return Err("this computer is no longer paired with that server");
        };
        if now.role != Role::Host || now.pairing_generation != peer.pairing_generation {
            return Err("this computer was paired again since this request started");
        }
        if !self.deps.access.enabled(&peer.id) {
            return Err("the owner turned bots' use of this computer off");
        }
        if !(self.deps.owner_ui_live)() {
            return Err("the Praxi app is not running on this computer, so it is not shared");
        }
        Ok(())
    }

    /// The first look at a family each session asks the owner (the screen
    /// unless the owner chose "always"; files and the browser always).
    async fn first_look(
        &self,
        peer: &Device,
        caller: Option<&Caller>,
        method: &str,
        family: PcFamily,
    ) -> Result<(), RpcError> {
        let bot = bot_key(caller);
        // "Always" is the switch of a run placed on this computer; a named
        // bot's first look asks for that bot.
        let skip = family == PcFamily::Screen
            && caller.is_none()
            && self.deps.access.always_read(&peer.id);
        if skip || self.deps.access.session_read_allowed(&peer.id, bot, family) {
            return Ok(());
        }
        let who = caller.map_or_else(
            || "this server's bots".to_owned(),
            |c| format!("the bot \"{}\" of this server", c.name),
        );
        let (target, payload) = match family {
            PcFamily::Screen => (
                "this computer's screen",
                format!(
                    "Allow {who} to look at this screen for this session? Praxi's own windows stay hidden."
                ),
            ),
            PcFamily::Files => (
                "this computer's files",
                format!(
                    "Allow {who} to read files on this computer for this session? Passwords, other programs' settings and Praxi's own data stay unreachable. Writing a file asks every time."
                ),
            ),
            PcFamily::Browser => (
                "this computer's browser",
                format!(
                    "Allow {who} to look at pages in its browser on this computer for this session? Opening a page or typing in one asks every time."
                ),
            ),
            PcFamily::Commands => return Ok(()),
        };
        let gen_then = self.deps.access.family_gen(&peer.id, family);
        let allowed = self
            .ask(
                peer,
                caller,
                PcPreview {
                    kind: "session_read",
                    action: "look".into(),
                    target: target.into(),
                    payload,
                },
            )
            .await;
        if !allowed {
            return Err(self.refuse(
                peer,
                method,
                "the person at this computer did not allow looking here",
            ));
        }
        if let Err(why) = self.still_authorized(peer) {
            return Err(self.refuse(peer, method, why));
        }
        // An allow that was delivered before the family was switched off must
        // not bring the session allowance back.
        if !self.deps.access.family_valid(&peer.id, family, gen_then) {
            return Err(self.refuse(
                peer,
                method,
                "the owner switched this part of the computer off while the question was open",
            ));
        }
        self.deps.access.allow_session_read(&peer.id, bot, family);
        Ok(())
    }

    fn audit_read(
        &self,
        peer: &Device,
        caller: Option<&Caller>,
        method: &str,
        detail: &Value,
    ) -> Result<(), RpcError> {
        let mut d = serde_json::json!({"host": peer.id, "bot": bot_key(caller), "method": method});
        if let (Some(o), Some(extra)) = (d.as_object_mut(), detail.as_object()) {
            o.extend(extra.clone());
        }
        self.audit("pc_target.read", d).map_err(|e| {
            RpcError::new(
                ERR_FORBIDDEN,
                format!("not shared: the local audit log could not be written ({e})"),
            )
        })
    }

    async fn read(
        &self,
        peer: &Device,
        caller: Option<&Caller>,
        method: &str,
        params: Value,
    ) -> Result<Value, RpcError> {
        self.family_gate(peer, method, PcFamily::Screen)?;
        self.first_look(peer, caller, method, PcFamily::Screen)
            .await?;
        self.audit_read(peer, caller, method, &serde_json::json!({}))?;
        self.deps.requests.note_read(&peer.id, &peer.name);
        self.reads.handle(peer, method, params).await
    }

    /// The owner's switch for one family, as it is now. The refusal says where
    /// to turn it on, in words the bot can pass on.
    fn family_gate(&self, peer: &Device, method: &str, family: PcFamily) -> Result<(), RpcError> {
        if self.deps.access.family_on(&peer.id, family) {
            return Ok(());
        }
        let why = match family {
            PcFamily::Screen => {
                "the owner has not allowed screen and input on this computer (Settings > Computer)"
            }
            PcFamily::Browser => {
                "the owner has not allowed the browser on this computer (Settings > Computer)"
            }
            PcFamily::Commands => {
                "the owner has not allowed commands on this computer (Settings > Computer)"
            }
            PcFamily::Files => {
                "the owner has not allowed files on this computer (Settings > Computer)"
            }
        };
        Err(self.refuse(peer, method, why))
    }

    // ── facts ────────────────────────────────────────────────────────────────

    fn info(&self, peer: &Device) -> Result<Value, RpcError> {
        let sw = self.deps.access.switches(&peer.id);
        let info = PcInfo {
            os: std::env::consts::OS.to_owned(),
            shell: if cfg!(windows) { "PowerShell" } else { "sh" }.to_owned(),
            home: builtin_tools::user_home().map(|p| p.display().to_string()),
            hostname: std::env::var("COMPUTERNAME")
                .or_else(|_| std::env::var("HOSTNAME"))
                .ok()
                .filter(|h| !h.is_empty()),
            screen: sw.screen,
            browser: sw.browser && self.deps.browser.is_some(),
            commands: sw.commands,
            files: sw.files,
        };
        serde_json::to_value(info).map_err(|e| RpcError::new(-32603, e.to_string()))
    }

    // ── commands and files ───────────────────────────────────────────────────

    fn mask_output(&self, mut out: tool_registry::ToolOutput) -> tool_registry::ToolOutput {
        for part in &mut out.content {
            if let tool_registry::ContentPart::Text { text } = part {
                *text = (self.deps.redact)(text);
            }
        }
        out.spill = None;
        out.exposure = None;
        out
    }

    async fn run_served(&self, tool: &Arc<dyn Tool>, input: Value) -> Result<Value, RpcError> {
        let ctx = ToolContext {
            run_id: uuid::Uuid::now_v7().to_string(),
            max_inline_bytes: 60_000,
            ..ToolContext::default()
        };
        match tool.call(&ctx, input).await {
            Ok(out) => serde_json::to_value(self.mask_output(out))
                .map_err(|e| RpcError::new(-32603, e.to_string())),
            Err(e) => Err(RpcError::new(
                ERR_FORBIDDEN,
                (self.deps.redact)(&e.to_string()),
            )),
        }
    }

    #[allow(clippy::too_many_lines)] // one flat dispatch per served tool
    async fn serve_tool(
        &self,
        peer: &Device,
        caller: &Caller,
        params: Value,
    ) -> Result<Value, RpcError> {
        #[derive(Deserialize)]
        struct P {
            name: String,
            #[serde(default)]
            input: Value,
        }
        let method = RPC_TOOL;
        let p: P = serde_json::from_value(params)
            .map_err(|e| RpcError::new(ERR_INVALID_PARAMS, format!("invalid params: {e}")))?;
        let Some(name) = SERVED_TOOLS.iter().find(|n| **n == p.name) else {
            return Err(self.refuse(peer, method, "that tool is not served by this computer"));
        };
        let family = PcFamily::of_tool(name).unwrap_or(PcFamily::Commands);
        self.family_gate(peer, method, family)?;
        let Some(tool) = self.deps.tools.iter().find(|t| t.spec().name == *name) else {
            return Err(self.refuse(peer, method, "that tool is not available on this computer"));
        };
        let mut input = p.input;
        match *name {
            "shell_run" => {
                let command = str_field(&input, "command")
                    .ok_or_else(|| RpcError::new(ERR_INVALID_PARAMS, "missing command"))?;
                // The folder the command runs in is bound BEFORE the owner is
                // asked: the real folder (links resolved), shown whole.
                let cwd_raw = str_field(&input, "cwd")
                    .or_else(|| builtin_tools::user_home().map(|h| h.display().to_string()));
                let Some(cwd_raw) = cwd_raw else {
                    return Err(self.refuse(
                        peer,
                        method,
                        "this computer has no home folder to start in",
                    ));
                };
                complete_text(&cwd_raw).map_err(|why| self.refuse(peer, method, &why))?;
                let cwd = canonical_target(&resolve_path(&cwd_raw))
                    .map_err(|why| self.refuse(peer, method, &why))?;
                if !cwd.is_dir() {
                    return Err(self.refuse(peer, method, "the working folder does not exist"));
                }
                let cwd_text = cwd.display().to_string();
                self.command_refusal(&command, Some(&cwd_text))
                    .map_err(|why| self.refuse(peer, method, &why))?;
                if let Some(why) = self.base_refusal(peer, method) {
                    return Err(self.refuse(peer, method, &why));
                }
                let shell = str_field(&input, "shell")
                    .unwrap_or_else(|| if cfg!(windows) { "powershell" } else { "sh" }.to_owned());
                let preview = PcPreview {
                    kind: "act",
                    action: "run command".into(),
                    target: format!("in {shell} on this computer, folder: {cwd_text}"),
                    payload: (self.deps.redact)(&command),
                };
                self.approve_and_run(peer, Some(caller), method, preview, PcFamily::Commands)
                    .await?;
                if let Some(o) = input.as_object_mut() {
                    // Run in exactly the folder on the card.
                    o.insert("cwd".into(), Value::String(cwd_text));
                }
            }
            "fs_write" => {
                let path = str_field(&input, "path")
                    .ok_or_else(|| RpcError::new(ERR_INVALID_PARAMS, "missing path"))?;
                let content = str_field(&input, "content")
                    .ok_or_else(|| RpcError::new(ERR_INVALID_PARAMS, "missing content"))?;
                complete_text(&path).map_err(|why| self.refuse(peer, method, &why))?;
                complete_text(&content).map_err(|why| self.refuse(peer, method, &why))?;
                // The file that would be written: real location, links
                // resolved (a new file: its nearest existing folder resolved),
                // checked and shown whole.
                let abs = canonical_target(&resolve_path(&path))
                    .map_err(|why| self.refuse(peer, method, &why))?;
                write_target_refusal(&abs, &self.deps.data)
                    .map_err(|why| self.refuse(peer, method, &why))?;
                if let Some(why) = self.base_refusal(peer, method) {
                    return Err(self.refuse(peer, method, &why));
                }
                let existing = std::fs::metadata(&abs).ok();
                let what = match existing {
                    Some(m) if m.is_file() => {
                        format!("replaces an existing file of {} bytes", m.len())
                    }
                    Some(_) => "not a file".to_owned(),
                    None => "creates a new file".to_owned(),
                };
                let preview = PcPreview {
                    kind: "act",
                    action: "write file".into(),
                    target: format!("{} ({what})", abs.display()),
                    payload: (self.deps.redact)(&content),
                };
                self.approve_and_run(peer, Some(caller), method, preview, PcFamily::Files)
                    .await?;
                if let Some(o) = input.as_object_mut() {
                    // The file written is exactly the one on the card.
                    o.insert("path".into(), Value::String(abs.display().to_string()));
                }
            }
            _ => {
                if let Some(path) = str_field(&input, "path") {
                    complete_text(&path).map_err(|why| self.refuse(peer, method, &why))?;
                }
                if let Some(why) = self.base_refusal(peer, method) {
                    return Err(self.refuse(peer, method, &why));
                }
                self.first_look(peer, Some(caller), method, PcFamily::Files)
                    .await?;
                self.audit_read(
                    peer,
                    Some(caller),
                    method,
                    &serde_json::json!({"tool": name, "path": str_field(&input, "path")}),
                )?;
            }
        }
        self.run_served(tool, input).await
    }

    /// Commands that are never run for a server's bot, and what a command may
    /// not name or start in.
    fn command_refusal(&self, command: &str, cwd: Option<&str>) -> Result<(), String> {
        complete_text(command)?;
        if let Some(cwd) = cwd {
            complete_text(cwd)?;
            let canon_data = std::fs::canonicalize(&self.deps.data)
                .map_or_else(|_| self.deps.data.clone(), strip_verbatim);
            if let Some(why) = protected_location(Path::new(cwd), &canon_data) {
                return Err(format!("a command may not run there: {why}"));
            }
        }
        if permission_engine::NeverList::default_list().matches(command) {
            return Err(
                "this command is on the list of commands that are never run for a server's bot on this computer"
                    .into(),
            );
        }
        let lower = command.to_lowercase().replace('\\', "/");
        let data = self
            .deps
            .data
            .to_string_lossy()
            .to_lowercase()
            .replace('\\', "/");
        if !data.is_empty() && lower.contains(&data) {
            return Err("Praxi's own data folder is never reachable".into());
        }
        Ok(())
    }

    // ── browser ──────────────────────────────────────────────────────────────

    #[allow(clippy::too_many_lines)] // one flat dispatch table
    async fn serve_browser(
        &self,
        peer: &Device,
        caller: &Caller,
        params: Value,
    ) -> Result<Value, RpcError> {
        let method = RPC_BROWSER;
        self.family_gate(peer, method, PcFamily::Browser)?;
        let Some(driver) = self.browser_for(peer, caller) else {
            return Err(self.refuse(peer, method, "this computer has no browser to use"));
        };
        let op = str_field(&params, "op")
            .ok_or_else(|| RpcError::new(ERR_INVALID_PARAMS, "missing op"))?;
        let arg = |k: &str| str_field(&params, k).unwrap_or_default();
        // What is on the card for an input.
        let write: Option<(String, String)> = match op.as_str() {
            "open" => {
                let url = arg("url");
                complete_text(&url).map_err(|why| self.refuse(peer, method, &why))?;
                let Some(parsed) = web_url(&url) else {
                    return Err(self.refuse(
                        peer,
                        method,
                        "only http and https pages can be opened",
                    ));
                };
                // Never-reachable destinations get no card at all.
                if let browser_cdp::Verdict::Block(why) = browser_cdp::classify_url(&url).await {
                    return Err(self.refuse(peer, method, &format!("not allowed: {why}")));
                }
                Some((
                    format!(
                        "opens {} in this computer's browser",
                        parsed.host_str().unwrap_or_default()
                    ),
                    url,
                ))
            }
            "click" => Some((
                format!(
                    "click in the open page: {}",
                    describe_element(&driver, &arg("ref")).await
                ),
                format!("click element {}", arg("ref")),
            )),
            "fill" => {
                let t = arg("text");
                complete_text(&t).map_err(|why| self.refuse(peer, method, &why))?;
                // Same rule as desktop typing: a password / one-time-code /
                // card field is never filled by a bot.
                // Fails closed: when the browser cannot say what the field is,
                // the fill is refused.
                match driver.field_target(&arg("ref")).await {
                    Ok(f) if !f.sensitive => {}
                    Ok(_) => {
                        return Err(self.refuse(peer, method, "the element is a password field"));
                    }
                    Err(_) => {
                        return Err(self.refuse(
                            peer,
                            method,
                            "cannot tell whether the element is a password field",
                        ));
                    }
                }
                Some((
                    format!(
                        "type into the open page: {}",
                        describe_element(&driver, &arg("ref")).await
                    ),
                    format!(
                        "type into element {}:\n{}",
                        arg("ref"),
                        (self.deps.redact)(&t)
                    ),
                ))
            }
            "select" => Some((
                format!(
                    "choose an option in the open page: {}",
                    describe_element(&driver, &arg("ref")).await
                ),
                format!("choose \"{}\" in element {}", arg("value"), arg("ref")),
            )),
            "press" => {
                let k = arg("key");
                complete_text(&k).map_err(|why| self.refuse(peer, method, &why))?;
                let focused = driver.focused_target().await;
                // A secret field must not be typed into key by key.
                if !is_navigation_key(&k) && focused.as_ref().map_or(true, |f| f.sensitive) {
                    return Err(self.refuse(
                        peer,
                        method,
                        "the focused element is, or may be, a password field; only Tab, Enter, Escape and arrow keys are allowed there",
                    ));
                }
                let on = match &focused {
                    Ok(t) => format!(
                        "focused element ({} the page calls \"{}\")",
                        plain_line(&t.role, 40),
                        plain_line(&t.name, 120)
                    ),
                    Err(_) => "an unknown focused element".to_owned(),
                };
                Some((
                    format!(
                        "press a key in the open page ({on}): {}",
                        page_of(&driver).await
                    ),
                    format!("press {k}"),
                ))
            }
            "back" => Some((
                format!("go back in the open page: {}", page_of(&driver).await),
                "go back".into(),
            )),
            "tabs" if params.get("switch_to").is_some_and(|v| !v.is_null()) => {
                let n = params["switch_to"].as_u64().unwrap_or(0);
                Some((
                    format!("switch the browser tab to: {}", tab_line(&driver, n).await),
                    format!("switch to tab {n}"),
                ))
            }
            "snapshot" | "screenshot" | "tabs" | "wait" | "scroll" | "current_url" => None,
            _ => return Err(self.refuse(peer, method, "that browser operation is not served")),
        };
        if let Some((target, payload)) = write {
            complete_text(&payload).map_err(|why| self.refuse(peer, method, &why))?;
            if let Some(why) = self.base_refusal(peer, method) {
                return Err(self.refuse(peer, method, &why));
            }
            self.approve_and_run(
                peer,
                Some(caller),
                method,
                PcPreview {
                    kind: "act",
                    action: format!("browser {op}"),
                    target,
                    payload,
                },
                PcFamily::Browser,
            )
            .await?;
        } else {
            // The human driving the screen is not watched: no observation while
            // the control latch is theirs.
            if let Some(msg) = self.deps.gate.observation_refused() {
                return Err(self.refuse(peer, method, &msg));
            }
            self.first_look(peer, Some(caller), method, PcFamily::Browser)
                .await?;
            self.audit_read(peer, Some(caller), method, &serde_json::json!({"op": op}))?;
        }
        // What the browser returns (a read, or the page a click led to) is
        // discarded if control changed hands while it was captured.
        let epoch = self.deps.gate.observation_epoch();
        let out = run_browser_op(&driver, &op, &params, &self.deps.redact).await;
        if self.deps.gate.observation_epoch() != epoch
            || self.deps.gate.observation_refused().is_some()
        {
            return Err(self.refuse(
                peer,
                method,
                "the user took control of the screen while this was captured, so it was discarded",
            ));
        }
        out
    }

    async fn act(
        &self,
        peer: &Device,
        caller: Option<&Caller>,
        params: Value,
    ) -> Result<Value, RpcError> {
        let action = parse_action(&params)?;
        let prepared = self.prepare(peer, "desktop.act", &action).await?;
        let preview = prepared.preview("act", action_name(&action));
        self.approve_and_run(peer, caller, "desktop.act", preview, PcFamily::Screen)
            .await?;
        let outcome = self
            .deps
            .raw
            .act_approved(
                action,
                ActGuard {
                    expect: prepared.target.identity.clone(),
                },
            )
            .await
            .map_err(|e| desktop_error_to_rpc(&e))?;
        serde_json::to_value(outcome).map_err(|e| RpcError::new(-32603, e.to_string()))
    }

    async fn launch(
        &self,
        peer: &Device,
        caller: Option<&Caller>,
        params: Value,
    ) -> Result<Value, RpcError> {
        #[derive(Deserialize)]
        struct P {
            app: AppSpec,
        }
        let p: P = serde_json::from_value(params)
            .map_err(|e| RpcError::new(ERR_INVALID_PARAMS, format!("invalid params: {e}")))?;
        let command = std::iter::once(p.app.executable.as_str())
            .chain(p.app.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ");
        complete_text(&command).map_err(|why| self.refuse(peer, "desktop.launch", &why))?;
        // The same hard refusals as a command: the never-list and Praxi's own
        // data folder, whichever switch the program is started under.
        self.command_refusal(&command, None)
            .map_err(|why| self.refuse(peer, "desktop.launch", &why))?;
        if let Some(why) = self.base_refusal(peer, "desktop.launch") {
            return Err(self.refuse(peer, "desktop.launch", &why));
        }
        let preview = PcPreview {
            kind: "act",
            action: "launch".into(),
            target: "starts a new program on this computer".into(),
            payload: (self.deps.redact)(&command),
        };
        self.approve_and_run(peer, caller, "desktop.launch", preview, PcFamily::Screen)
            .await?;
        let pid = self
            .deps
            .raw
            .launch(p.app)
            .await
            .map_err(|e| desktop_error_to_rpc(&e))?;
        Ok(serde_json::json!({ "pid": pid }))
    }

    async fn focus(
        &self,
        peer: &Device,
        caller: Option<&Caller>,
        params: Value,
    ) -> Result<Value, RpcError> {
        let method = "desktop.focus";
        if let Some(why) = self.base_refusal(peer, method) {
            return Err(self.refuse(peer, method, &why));
        }
        let window_id = params
            .get("window_id")
            .and_then(Value::as_u64)
            .ok_or_else(|| RpcError::new(ERR_INVALID_PARAMS, "window_id is required"))?;
        let target = self
            .deps
            .raw
            .focus_target(window_id)
            .await
            .map_err(|e| desktop_error_to_rpc(&e))?
            .filter(|t| t.identity.is_complete() && !is_praxi_target(t, self.deps.own_pid))
            .ok_or_else(|| self.refuse(peer, method, "this window is unavailable for focus"))?;
        let preview = PcPreview {
            kind: "act",
            action: "focus".into(),
            target: (self.deps.redact)(&plain_line(&target.title, 120)),
            payload: (self.deps.redact)(&target.identity.image_path),
        };
        self.approve_and_run(peer, caller, method, preview, PcFamily::Screen)
            .await?;
        self.deps
            .raw
            .focus(
                window_id,
                Some(ActGuard {
                    expect: target.identity,
                }),
            )
            .await
            .map_err(|e| desktop_error_to_rpc(&e))?;
        Ok(Value::Null)
    }

    /// Latch and lease: the same refusal for every kind of input.
    fn base_refusal(&self, peer: &Device, _method: &str) -> Option<String> {
        if let Some(msg) = self.deps.gate.latch_refusal() {
            return Some(msg);
        }
        self.still_authorized(peer).err().map(str::to_owned)
    }

    /// Park a card and wait; then re-check everything the answer depends on.
    async fn approve_and_run(
        &self,
        peer: &Device,
        caller: Option<&Caller>,
        method: &str,
        preview: PcPreview,
        family: PcFamily,
    ) -> Result<(), RpcError> {
        let gen_then = self.deps.access.family_gen(&peer.id, family);
        let control_epoch = self.deps.gate.observation_epoch();
        let allowed = self.ask(peer, caller, preview.clone()).await;
        if !allowed {
            return Err(self.refuse(
                peer,
                method,
                "the person at this computer did not approve this action",
            ));
        }
        // The switch, the pairing or the human's hands may have changed while
        // the card was open.
        if let Some(why) = self.base_refusal(peer, method) {
            return Err(self.refuse(peer, method, &why));
        }
        if self.deps.gate.observation_epoch() != control_epoch {
            return Err(self.refuse(
                peer,
                method,
                "control changed while approval was open; observe again",
            ));
        }
        // The family this action needs, not just "some family": a delivered
        // allow does not outlive the switch that was turned off meanwhile.
        if !self.deps.access.family_valid(&peer.id, family, gen_then) {
            return Err(self.refuse(
                peer,
                method,
                "the owner switched this part of the computer off while the question was open",
            ));
        }
        self.audit(
            "pc_target.act",
            serde_json::json!({
                "host": peer.id,
                "bot": bot_key(caller),
                "method": method,
                "action": preview.action,
                "target": preview.target,
                "payload_chars": preview.payload.chars().count(),
                "payload_sha256": sha256_hex(&preview.payload),
            }),
        )
        .map_err(|e| {
            RpcError::new(
                ERR_FORBIDDEN,
                format!("not done: the local audit log could not be written ({e})"),
            )
        })?;
        Ok(())
    }

    /// Validate the request and resolve, on this OS, what it would reach.
    async fn prepare(
        &self,
        peer: &Device,
        method: &str,
        action: &Action,
    ) -> Result<Prepared, RpcError> {
        if let Some(why) = self.base_refusal(peer, method) {
            return Err(self.refuse(peer, method, &why));
        }
        let payload = describe_action(action, &self.deps.redact)
            .map_err(|why| self.refuse(peer, method, &why))?;
        if let Action::KeyPress { keys } = action
            && let Some(r) = self.deps.raw.key_chord_refusal(keys)
        {
            return Err(self.refuse(peer, method, r.why()));
        }
        let target = match self.deps.raw.input_target().await {
            Ok(Some(t)) if t.identity.is_complete() => t,
            _ => {
                return Err(self.refuse(
                    peer,
                    method,
                    "could not identify the program that would receive this input",
                ));
            }
        };
        if is_praxi_target(&target, self.deps.own_pid) {
            return Err(self.refuse(
                peer,
                method,
                "input to Praxi's own windows is never allowed",
            ));
        }
        if target.password_focus && !matches!(action, Action::Click { .. } | Action::Scroll { .. })
        {
            return Err(self.refuse(peer, method, "the focused element is a password field"));
        }
        // Coordinates may land on a Praxi window that is not in front.
        if let Some((x, y)) = absolute_point(action) {
            let windows = self.deps.raw.windows().await.unwrap_or_default();
            if windows
                .iter()
                .any(|w| is_praxi_window(w, self.deps.own_pid) && contains(&w.bounds, x, y))
            {
                return Err(self.refuse(
                    peer,
                    method,
                    "that point is inside a Praxi window; input there is never allowed",
                ));
            }
        }
        Ok(Prepared {
            description: format!(
                "\"{}\" — {} (process {})",
                target.title, target.identity.image_path, target.identity.pid
            ),
            payload,
            target,
        })
    }
}

struct Prepared {
    target: InputTarget,
    description: String,
    payload: String,
}

impl Prepared {
    fn preview(&self, kind: &'static str, action: &str) -> PcPreview {
        PcPreview {
            kind,
            action: action.to_owned(),
            target: self.description.clone(),
            payload: self.payload.clone(),
        }
    }
}

fn parse_action(params: &Value) -> Result<Action, RpcError> {
    let action = params
        .get("action")
        .cloned()
        .ok_or_else(|| RpcError::new(ERR_INVALID_PARAMS, "missing action"))?;
    serde_json::from_value(action)
        .map_err(|e| RpcError::new(ERR_INVALID_PARAMS, format!("invalid action: {e}")))
}

fn action_name(a: &Action) -> &'static str {
    match a {
        Action::Click { .. } => "click",
        Action::TypeText { .. } => "type",
        Action::SetValue { .. } => "set value",
        Action::Scroll { .. } => "scroll",
        Action::KeyPress { .. } => "key",
        Action::Drag { .. } => "drag",
    }
}

fn absolute_point(a: &Action) -> Option<(f64, f64)> {
    fn at(t: &Target) -> Option<(f64, f64)> {
        match t {
            Target::ByCoordinate {
                x,
                y,
                space: CoordinateSpace::Absolute,
            } => Some((*x, *y)),
            _ => None,
        }
    }
    match a {
        Action::Click { target, .. }
        | Action::SetValue { target, .. }
        | Action::Scroll { target, .. } => at(target),
        Action::Drag { from, to, .. } => at(from).or_else(|| at(to)),
        _ => None,
    }
}

fn sha256_hex(text: &str) -> String {
    use sha2::Digest as _;
    use std::fmt::Write as _;
    sha2::Sha256::digest(text.as_bytes())
        .iter()
        .fold(String::with_capacity(64), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
}

/// Page-supplied text on a card: one line, nothing invisible or
/// direction-changing, bounded.
fn plain_line(text: &str, max: usize) -> String {
    let clean: String = text
        .chars()
        .filter(|c| {
            !c.is_control()
                && !matches!(
                    *c,
                    '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}'
                )
        })
        .collect();
    if clean.chars().count() > max {
        let mut cut: String = clean.chars().take(max).collect();
        cut.push('…');
        cut
    } else {
        clean
    }
}

/// Keys that move or confirm without typing a character (no modifiers).
pub(crate) fn is_navigation_key(spec: &str) -> bool {
    matches!(
        spec.to_ascii_lowercase().as_str(),
        "tab"
            | "enter"
            | "return"
            | "escape"
            | "esc"
            | "arrowup"
            | "arrowdown"
            | "arrowleft"
            | "arrowright"
            | "up"
            | "down"
            | "left"
            | "right"
    )
}

/// The page the browser is on, for a card.
async fn page_of(driver: &Arc<dyn BrowserDriver>) -> String {
    driver
        .current_url()
        .await
        .map_or_else(|| "an unknown page".to_owned(), |u| plain_line(&u, 300))
}

/// The element `r` and its page, for a card: the role and the page's own
/// name for it (page text, marked as such).
async fn describe_element(driver: &Arc<dyn BrowserDriver>, r: &str) -> String {
    let page = page_of(driver).await;
    match driver.field_target(r).await {
        Ok(t) => format!(
            "element {} ({} the page calls \"{}\") on {page}",
            plain_line(r, 16),
            plain_line(&t.role, 40),
            plain_line(&t.name, 120)
        ),
        Err(_) => format!("element {} on {page}", plain_line(r, 16)),
    }
}

/// Tab `n` as the browser lists it (title and address), for a card.
async fn tab_line(driver: &Arc<dyn BrowserDriver>, n: u64) -> String {
    let listed = driver.tabs(None).await.unwrap_or_default();
    listed
        .lines()
        .find(|l| {
            l.strip_prefix(&n.to_string())
                .is_some_and(|rest| rest.starts_with(' ') || rest.starts_with('*'))
        })
        .map_or_else(|| format!("tab {n} (not listed)"), |l| plain_line(l, 400))
}

/// Text a card can show in full: not too long, nothing invisible.
pub(crate) fn complete_text(text: &str) -> Result<(), String> {
    if text.chars().count() > MAX_PAYLOAD_CHARS {
        return Err(format!(
            "the text is longer than {MAX_PAYLOAD_CHARS} characters, so it cannot be shown completely and was refused"
        ));
    }
    if protocol::approval_text::hides_characters(text) {
        return Err(
            "the text contains invisible or direction-changing characters, so it was refused"
                .into(),
        );
    }
    Ok(())
}

/// Run one browser operation on `driver` and shape the answer as the wire
/// carries it.
pub(crate) async fn run_browser_op(
    driver: &Arc<dyn BrowserDriver>,
    op: &str,
    params: &Value,
    redact: &Arc<dyn Fn(&str) -> String + Send + Sync>,
) -> Result<Value, RpcError> {
    let text = |r: Result<String, String>| -> Result<Value, RpcError> {
        r.map(|t| serde_json::json!({"text": redact(&t)}))
            .map_err(|e| RpcError::new(ERR_FORBIDDEN, redact(&e)))
    };
    let arg = |k: &str| str_field(params, k).unwrap_or_default();
    match op {
        "open" => text(driver.open(&arg("url"), None).await),
        "snapshot" => text(
            driver
                .snapshot(params["interactive_only"].as_bool().unwrap_or(false))
                .await,
        ),
        "click" => text(driver.click(&arg("ref")).await),
        "fill" => text(driver.fill(&arg("ref"), &arg("text")).await),
        "select" => text(driver.select(&arg("ref"), &arg("value")).await),
        "press" => text(driver.press(&arg("key")).await),
        "back" => text(driver.back().await),
        "scroll" => text(
            driver
                .scroll(
                    str_field(params, "ref").as_deref(),
                    params["dx"].as_f64().unwrap_or(0.0),
                    params["dy"].as_f64().unwrap_or(0.0),
                )
                .await,
        ),
        "tabs" => text(
            driver
                .tabs(
                    params["switch_to"]
                        .as_u64()
                        .and_then(|n| usize::try_from(n).ok()),
                )
                .await,
        ),
        "wait" => {
            let cond = if let Some(t) = str_field(&params["cond"], "text") {
                builtin_tools::Wait::Text(t)
            } else if let Some(sel) = str_field(&params["cond"], "selector") {
                builtin_tools::Wait::Selector(sel)
            } else {
                builtin_tools::Wait::Idle
            };
            let timeout = params["timeout_ms"]
                .as_u64()
                .map(|ms| Duration::from_millis(ms.min(60_000)));
            text(driver.wait(cond, timeout).await)
        }
        "screenshot" => {
            use base64::Engine as _;
            match driver
                .screenshot(
                    params["full_page"].as_bool().unwrap_or(false),
                    str_field(params, "element").as_deref(),
                )
                .await
            {
                Ok((bytes, ext)) => Ok(serde_json::json!({
                    "data_base64": base64::engine::general_purpose::STANDARD.encode(bytes),
                    "ext": ext,
                })),
                Err(e) => Err(RpcError::new(ERR_FORBIDDEN, redact(&e))),
            }
        }
        "current_url" => Ok(serde_json::json!({
            "url": driver.current_url().await.map(|u| redact(&u))
        })),
        _ => Err(RpcError::new(
            ERR_METHOD_NOT_FOUND,
            "unknown browser operation",
        )),
    }
}

/// Only http and https pages with a host are opened.
pub(crate) fn web_url(url: &str) -> Option<url::Url> {
    url::Url::parse(url)
        .ok()
        .filter(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some())
}

fn str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// Strip the verbatim prefix Windows adds to canonical paths (not for UNC).
fn strip_verbatim(p: PathBuf) -> PathBuf {
    let text = p.to_string_lossy().into_owned();
    if let Some(rest) = text.strip_prefix(r"\\?\")
        && !rest.starts_with("UNC\\")
    {
        return PathBuf::from(rest);
    }
    p
}

/// The real location a path names: links resolved. A path that does not exist
/// yet is resolved through its nearest existing folder. `..` is refused.
fn canonical_target(path: &Path) -> Result<PathBuf, String> {
    if path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err("a path with .. is not accepted; give the full path".into());
    }
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut cur = path;
    let base = loop {
        if let Ok(c) = std::fs::canonicalize(cur) {
            break c;
        }
        let (Some(name), Some(parent)) = (cur.file_name(), cur.parent()) else {
            return Err("that location cannot be resolved".into());
        };
        tail.push(name.to_owned());
        cur = parent;
    };
    let mut out = strip_verbatim(base);
    for name in tail.into_iter().rev() {
        out.push(name);
    }
    complete_text(&out.display().to_string())?;
    Ok(out)
}

const CREDENTIAL_DIRS: [&str; 13] = [
    ".claude", ".codex", ".gemini", ".cursor", ".config", ".local", ".ssh", ".aws", ".gnupg",
    ".azure", ".kube", ".docker", "gcloud",
];

/// Folders no server's bot may use as a location: Praxi's own data and the
/// credential / tool-configuration folders (SSH, cloud, GPG, container, CLI
/// settings), judged on the real path.
fn protected_location(path: &Path, canon_data: &Path) -> Option<&'static str> {
    if path.starts_with(canon_data) {
        return Some("that is Praxi's own data folder");
    }
    path.components()
        .any(|c| match c {
            std::path::Component::Normal(n) => {
                CREDENTIAL_DIRS.contains(&n.to_string_lossy().to_lowercase().as_str())
            }
            _ => false,
        })
        .then_some("that is a credential or tool settings folder")
}

const PROFILES: [&str; 11] = [
    ".bashrc",
    ".bash_profile",
    ".bash_login",
    ".profile",
    ".zshrc",
    ".zprofile",
    ".zshenv",
    "config.fish",
    "profile.ps1",
    "microsoft.powershell_profile.ps1",
    "microsoft.vscode_profile.ps1",
];

/// Places a server's bot may never write on this computer, judged on the REAL
/// path: Praxi's own data, credential and tool-configuration folders (the
/// protected-path table), git internals, startup folders and shell profiles.
fn write_target_refusal(path: &Path, data: &Path) -> Result<(), String> {
    let canon_data =
        std::fs::canonicalize(data).map_or_else(|_| data.to_path_buf(), strip_verbatim);
    if let Some(why) = protected_location(path, &canon_data) {
        return Err(format!("{why}; it is never writable"));
    }
    let lower = path.to_string_lossy().to_lowercase().replace('\\', "/");
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if lower.split('/').any(|c| c == ".git") {
        return Err("git internals (hooks, config) are never writable".into());
    }
    if lower.contains("/start menu/programs/startup")
        || lower.contains("/.config/autostart")
        || lower.contains("/library/launchagents")
    {
        return Err("startup locations are never writable".into());
    }
    if PROFILES.contains(&name.as_str()) {
        return Err("shell start-up files are never writable".into());
    }
    Ok(())
}

/// An absolute path: a relative one is taken from the user's home folder.
fn resolve_path(raw: &str) -> PathBuf {
    let p = Path::new(raw);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        builtin_tools::user_home().unwrap_or_default().join(p)
    }
}

fn target_text(t: &Target) -> String {
    match t {
        Target::ByIndex { index, generation } => {
            format!("element #{index} of the last view (generation {generation})")
        }
        Target::ByCoordinate { x, y, space } => match space {
            CoordinateSpace::Absolute => format!("screen point ({x:.0}, {y:.0})"),
            CoordinateSpace::Normalized999 => {
                format!("point ({x:.0}, {y:.0}) of 999 in the window")
            }
        },
    }
}

/// The complete payload of an action as a person reads it; secrets masked.
fn describe_action(
    a: &Action,
    redact: &Arc<dyn Fn(&str) -> String + Send + Sync>,
) -> Result<String, String> {
    match a {
        Action::Click { target, method } => {
            let how = match method {
                ClickMethod::Accessibility => "accessibility press",
                ClickMethod::AppPost => "posted mouse click",
                ClickMethod::Global => "real mouse click",
            };
            Ok(format!("click on {} ({how})", target_text(target)))
        }
        Action::TypeText { text } => {
            complete_text(text)?;
            Ok(format!("type exactly:\n{}", redact(text)))
        }
        Action::SetValue { target, value } => {
            complete_text(value)?;
            Ok(format!(
                "set {} to:\n{}",
                target_text(target),
                redact(value)
            ))
        }
        Action::Scroll {
            target,
            delta_x,
            delta_y,
        } => Ok(format!(
            "scroll {} by ({delta_x:.0}, {delta_y:.0})",
            target_text(target)
        )),
        Action::KeyPress { keys } => {
            if keys.is_empty() || keys.len() > 8 || keys.iter().any(|k| k.chars().count() > 32) {
                return Err("the key list is empty or too long".into());
            }
            let joined = keys.join(" + ");
            complete_text(&joined)?;
            Ok(format!("press keys: {joined}"))
        }
        Action::Drag { from, to, .. } => Ok(format!(
            "drag from {} to {}",
            target_text(from),
            target_text(to)
        )),
    }
}

#[async_trait]
impl RpcHandler for HostDesktopHandler {
    async fn handle(&self, peer: &Device, method: &str, params: Value) -> Result<Value, RpcError> {
        if method == "node.ping" {
            return self.reads.handle(peer, method, params).await;
        }
        if peer.role != Role::Host {
            return Err(self.refuse(
                peer,
                method,
                "only the server this computer is paired with may use it",
            ));
        }
        if !self.deps.access.enabled(&peer.id) {
            return Err(self.refuse(
                peer,
                method,
                "the owner has not allowed bots to use this computer",
            ));
        }
        if let Err(why) = self.still_authorized(peer) {
            return Err(self.refuse(peer, method, why));
        }
        let caller = caller_of(&params).map_err(|why| self.refuse(peer, method, &why))?;
        // The `pc_*` families always name the bot asking: every allowance and
        // card here is per bot.
        let named = |method: &str| -> Result<&Caller, RpcError> {
            caller.as_ref().ok_or_else(|| {
                self.refuse(
                    peer,
                    method,
                    "the server did not say which of its bots is asking, so this was refused",
                )
            })
        };
        match method {
            "node.info" | "desktop.capabilities" => self.reads.handle(peer, method, params).await,
            RPC_INFO => {
                named(method)?;
                self.info(peer)
            }
            RPC_TOOL => self.serve_tool(peer, named(method)?, params).await,
            RPC_BROWSER => self.serve_browser(peer, named(method)?, params).await,
            "desktop.snapshot" | "desktop.screenshot" | "desktop.windows" => {
                self.read(peer, caller.as_ref(), method, params).await
            }
            "desktop.act" => {
                self.family_gate(peer, method, PcFamily::Screen)?;
                self.act(peer, caller.as_ref(), params).await
            }
            "desktop.focus" => {
                self.family_gate(peer, method, PcFamily::Screen)?;
                self.focus(peer, caller.as_ref(), params).await
            }
            "desktop.launch" => {
                self.family_gate(peer, method, PcFamily::Screen)?;
                self.launch(peer, caller.as_ref(), params).await
            }
            other => Err(RpcError::new(
                ERR_METHOD_NOT_FOUND,
                format!("unknown method '{other}'"),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use desktop_api::ProcessIdentity;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    const OWN_PID: u32 = 4242;

    fn identity(pid: u32, path: &str) -> ProcessIdentity {
        ProcessIdentity {
            image_path: path.into(),
            pid,
            start_time: 1,
        }
    }

    fn window(id: u64, title: &str, pid: u32, x: f64) -> WindowInfo {
        WindowInfo {
            id,
            title: title.into(),
            pid,
            bounds: Rect {
                x,
                y: 0.0,
                width: 100.0,
                height: 100.0,
            },
            focused: false,
        }
    }

    struct Fake {
        acts: AtomicUsize,
        guarded_for: Mutex<Option<ProcessIdentity>>,
        shots: AtomicUsize,
        epoch: Arc<AtomicU64>,
        bump_in_capture: bool,
        windows: Vec<WindowInfo>,
        foreground: Mutex<InputTarget>,
    }

    impl Fake {
        fn new(epoch: Arc<AtomicU64>) -> Arc<Self> {
            Arc::new(Self {
                acts: AtomicUsize::new(0),
                guarded_for: Mutex::new(None),
                shots: AtomicUsize::new(0),
                epoch,
                bump_in_capture: false,
                windows: vec![
                    window(1, "Notes", 100, 0.0),
                    window(2, "Praxi-Bot", 200, 500.0),
                ],
                foreground: Mutex::new(InputTarget {
                    window_id: 1,
                    title: "Notes".into(),
                    class: "Notepad".into(),
                    identity: identity(100, "C:\\Windows\\notepad.exe"),
                    elevated: Some(false),
                    password_focus: false,
                    owned: false,
                }),
            })
        }
    }

    #[async_trait]
    impl DesktopBackend for Fake {
        async fn snapshot(&self, _: Option<u64>) -> Result<Snapshot, DesktopError> {
            Err(DesktopError::Unsupported("test".into()))
        }
        async fn act(&self, _: Action) -> Result<ActionResult, DesktopError> {
            self.acts.fetch_add(100, Ordering::SeqCst);
            Err(DesktopError::Unsupported(
                "unguarded act must not be used".into(),
            ))
        }
        async fn act_guarded(&self, _: Action, g: ActGuard) -> Result<ActionResult, DesktopError> {
            self.acts.fetch_add(1, Ordering::SeqCst);
            *self.guarded_for.lock() = Some(g.expect);
            Ok(ActionResult {
                dispatched: true,
                verified: None,
                new_snapshot: None,
            })
        }
        async fn screenshot(&self, _: Region, _: Quality) -> Result<Image, DesktopError> {
            self.shots.fetch_add(1, Ordering::SeqCst);
            if self.bump_in_capture {
                self.epoch.fetch_add(1, Ordering::SeqCst);
            }
            Ok(Image {
                data: vec![1],
                width: 1,
                height: 1,
                mime_type: "image/png".into(),
            })
        }
        async fn windows(&self) -> Result<Vec<WindowInfo>, DesktopError> {
            Ok(self.windows.clone())
        }
        async fn launch(&self, _: AppSpec) -> Result<Pid, DesktopError> {
            Err(DesktopError::Unsupported("test".into()))
        }
        async fn input_target(&self) -> Result<Option<InputTarget>, DesktopError> {
            Ok(Some(self.foreground.lock().clone()))
        }
        fn key_chord_refusal(&self, keys: &[String]) -> Option<desktop_api::Refusal> {
            keys.iter()
                .any(|k| k.eq_ignore_ascii_case("win"))
                .then_some(desktop_api::Refusal::Chord)
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities {
                platform: "test".into(),
                accessibility_tree: false,
                screenshot: true,
                app_launch: false,
                has_display: true,
            }
        }
    }

    struct Gate {
        epoch: Arc<AtomicU64>,
        /// The human is driving the screen.
        refused: Arc<std::sync::atomic::AtomicBool>,
    }
    #[async_trait]
    impl ControlGate for Gate {
        fn observation_epoch(&self) -> u64 {
            self.epoch.load(Ordering::SeqCst)
        }
        fn observation_refused(&self) -> Option<String> {
            self.refused
                .load(Ordering::SeqCst)
                .then(|| "the user is driving the screen".to_owned())
        }
        async fn before_action(
            &self,
            _: agent_core::ControlRequest,
            _: &(dyn Fn() + Send + Sync),
        ) -> Result<(), String> {
            Ok(())
        }
    }

    struct Rig {
        h: HostDesktopHandler,
        fake: Arc<Fake>,
        access: Arc<AccessStore>,
        requests: Arc<PcRequests>,
        dir: tempfile::TempDir,
        live: Arc<std::sync::atomic::AtomicBool>,
        audit_ok: Arc<std::sync::atomic::AtomicBool>,
        epoch: Arc<AtomicU64>,
        refused: Arc<std::sync::atomic::AtomicBool>,
        shell: Arc<Recorder>,
        #[allow(dead_code)]
        write: Arc<Recorder>,
        browser: Arc<FakeBrowser>,
        /// Browsers the factory made (one per bot).
        browsers_made: Arc<AtomicUsize>,
    }

    fn device(role: Role) -> Device {
        Device {
            id: "host1".into(),
            name: "Server".into(),
            role,
            capabilities: vec![],
            addr: None,
            paired_at: 1,
            pairing_generation: "g1".into(),
            admin: false,
        }
    }

    fn write_pairing(dir: &Path, role: &str, generation: &str) {
        std::fs::create_dir_all(dir.join("peer")).unwrap();
        std::fs::write(
            dir.join("peer").join("devices.json"),
            serde_json::json!({"version":1,"devices":[{
                "id":"host1","name":"Server","role":role,"capabilities":[],
                "addr":null,"paired_at":1,"pairing_generation":generation
            }]})
            .to_string(),
        )
        .unwrap();
    }

    fn rig(bump_in_capture: bool, always_read: bool) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        write_pairing(dir.path(), "host", "g1");
        let epoch = Arc::new(AtomicU64::new(0));
        let mut fake = Fake::new(epoch.clone());
        Arc::get_mut(&mut fake).unwrap().bump_in_capture = bump_in_capture;
        let access = Arc::new(AccessStore::open(dir.path()));
        access.set("host1", true, always_read).unwrap();
        let requests = Arc::new(PcRequests::new());
        let live = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let audit_ok = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let (l, a) = (live.clone(), audit_ok.clone());
        let refused = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let served = (Recorder::new("shell_run"), Recorder::new("fs_write"));
        let browser = Arc::new(FakeBrowser::default());
        let browsers_made = Arc::new(AtomicUsize::new(0));
        let (made, fake_browser) = (browsers_made.clone(), browser.clone());
        let factory: BrowserFactory = Arc::new(move || {
            made.fetch_add(1, Ordering::SeqCst);
            fake_browser.clone() as Arc<dyn BrowserDriver>
        });
        let h = HostDesktopHandler::new(PcTargetDeps {
            raw: fake.clone(),
            data: dir.path().to_path_buf(),
            access: access.clone(),
            requests: requests.clone(),
            gate: Arc::new(Gate {
                epoch: epoch.clone(),
                refused: refused.clone(),
            }),
            owner_ui_live: Arc::new(move || l.load(Ordering::SeqCst)),
            audit: Arc::new(move |_, _| {
                a.load(Ordering::SeqCst)
                    .then_some(())
                    .ok_or("disk full".into())
            }),
            redact: Arc::new(|t| t.replace("hunter2", "••••")),
            own_pid: OWN_PID,
            tools: vec![
                served.0.clone() as Arc<dyn Tool>,
                served.1.clone() as Arc<dyn Tool>,
                Recorder::new("fs_read") as Arc<dyn Tool>,
            ],
            browser: Some(factory),
        })
        .with_ask_timeout(Duration::from_secs(5));
        Rig {
            h,
            fake,
            access,
            requests,
            dir,
            live,
            audit_ok,
            epoch,
            refused,
            shell: served.0,
            write: served.1,
            browser,
            browsers_made,
        }
    }

    /// A served tool that records its inputs and answers with a secret in it.
    struct Recorder {
        spec: tool_registry::ToolSpec,
        inputs: Mutex<Vec<Value>>,
    }

    impl Recorder {
        fn new(name: &str) -> Arc<Self> {
            Arc::new(Self {
                spec: tool_registry::ToolSpec {
                    name: name.into(),
                    description: "test".into(),
                    input_schema: serde_json::json!({"type": "object"}),
                    effect: tool_registry::Effect::Write,
                    category: tool_registry::PermissionCategory::Shell,
                    platforms: vec![tool_registry::Platform::All],
                    content: tool_registry::ContentClass::all(),
                },
                inputs: Mutex::new(Vec::new()),
            })
        }
    }

    #[async_trait]
    impl Tool for Recorder {
        fn spec(&self) -> &tool_registry::ToolSpec {
            &self.spec
        }
        async fn call(
            &self,
            _: &ToolContext,
            input: Value,
        ) -> Result<tool_registry::ToolOutput, tool_registry::ToolError> {
            self.inputs.lock().push(input);
            Ok(tool_registry::ToolOutput::text("done, token hunter2"))
        }
    }

    #[derive(Default)]
    struct FakeBrowser {
        /// What `current_url` reports (`None`: the driver cannot tell).
        url: Mutex<Option<String>>,
        opened: Mutex<Vec<String>>,
        snapshots: std::sync::atomic::AtomicUsize,
        /// Bump this epoch while a snapshot is being captured.
        bump: Mutex<Option<Arc<AtomicU64>>>,
        /// Every field reports itself as a password field.
        secret_fields: std::sync::atomic::AtomicBool,
        /// The browser cannot say what a field is.
        target_unknown: std::sync::atomic::AtomicBool,
        check: Mutex<Option<browser_cdp::DestinationCheck>>,
        /// Every policy installed, in order (one per bot's browser).
        checks: Mutex<Vec<browser_cdp::DestinationCheck>>,
        /// The proxy's private-host decisions, in the same order.
        hosts: Mutex<Vec<browser_cdp::DestinationCheck>>,
    }

    #[async_trait]
    impl BrowserDriver for FakeBrowser {
        async fn current_url(&self) -> Option<String> {
            self.url.lock().clone()
        }
        async fn open(&self, url: &str, _: Option<&str>) -> Result<String, String> {
            self.opened.lock().push(url.to_owned());
            Ok(format!("opened {url}"))
        }
        fn set_served_network(&self, net: browser_cdp::ServedNetwork) {
            self.checks.lock().push(net.check.clone());
            self.hosts.lock().push(net.private_host);
            *self.check.lock() = Some(net.check);
        }
        async fn snapshot(&self, _: bool) -> Result<String, String> {
            self.snapshots.fetch_add(1, Ordering::SeqCst);
            if let Some(epoch) = self.bump.lock().as_ref() {
                epoch.fetch_add(1, Ordering::SeqCst);
            }
            Ok("page".into())
        }
        async fn click(&self, _: &str) -> Result<String, String> {
            Ok("clicked".into())
        }
        async fn fill(&self, _: &str, _: &str) -> Result<String, String> {
            Ok("filled".into())
        }
        async fn field_target(&self, _: &str) -> Result<browser_cdp::FieldTarget, String> {
            if self.target_unknown.load(Ordering::SeqCst) {
                return Err("cannot tell".into());
            }
            Ok(browser_cdp::FieldTarget {
                name: "Password".into(),
                role: "textbox".into(),
                frame_origin: None,
                top_origin: None,
                sensitive: self.secret_fields.load(Ordering::SeqCst),
            })
        }
        async fn focused_target(&self) -> Result<browser_cdp::FieldTarget, String> {
            self.field_target("").await
        }
        async fn select(&self, _: &str, _: &str) -> Result<String, String> {
            Ok("selected".into())
        }
        async fn press(&self, _: &str) -> Result<String, String> {
            Ok("pressed".into())
        }
        async fn scroll(&self, _: Option<&str>, _: f64, _: f64) -> Result<String, String> {
            Ok("scrolled".into())
        }
        async fn wait(
            &self,
            _: builtin_tools::Wait,
            _: Option<Duration>,
        ) -> Result<String, String> {
            Ok("waited".into())
        }
        async fn back(&self) -> Result<String, String> {
            Ok("back".into())
        }
        async fn tabs(&self, _: Option<usize>) -> Result<String, String> {
            Ok("tabs".into())
        }
        async fn screenshot(
            &self,
            _: bool,
            _: Option<&str>,
        ) -> Result<(Vec<u8>, &'static str), String> {
            Ok((vec![1], "png"))
        }
        async fn eval(&self, _: &str) -> Result<Value, String> {
            Err("no".into())
        }
    }

    fn all_families() -> Switches {
        Switches {
            screen: true,
            always_read: true,
            browser: true,
            commands: true,
            files: true,
        }
    }

    #[allow(clippy::needless_pass_by_value)]
    fn tool_params(name: &str, input: Value) -> Value {
        as_bot(serde_json::json!({ "name": name, "input": input }), "bot-a")
    }

    fn as_bot(mut v: Value, id: &str) -> Value {
        v[CALLER_KEY] = serde_json::json!({"id": id, "name": format!("Bot {id}")});
        v
    }

    /// Each family is its own owner switch: with only screen and input on, a
    /// command, a file and a browser call are refused at once (no card, nothing
    /// run), with a reason the bot can pass on; turning the family on serves it.
    #[tokio::test]
    async fn every_family_is_refused_while_its_own_switch_is_off() {
        let r = rig(false, true);
        let host = device(Role::Host);
        let calls: [(&str, Value); 3] = [
            (
                RPC_TOOL,
                tool_params("shell_run", serde_json::json!({"command": "dir"})),
            ),
            (
                RPC_TOOL,
                tool_params("fs_read", serde_json::json!({"path": "C:/x.txt"})),
            ),
            (
                RPC_BROWSER,
                serde_json::json!({"op": "open", "url": "https://example.com/", "bot": {"id": "bot-a", "name": "Ayla"}}),
            ),
        ];
        for (method, params) in &calls {
            let res = r.h.handle(&host, method, params.clone()).await;
            let err = res.unwrap_err();
            assert_eq!(err.code, ERR_FORBIDDEN, "{method}");
            assert!(
                err.message.contains("Settings > Computer"),
                "{}",
                err.message
            );
        }
        assert!(
            r.requests.list().is_empty(),
            "no card for a switched-off family"
        );
        assert!(r.shell.inputs.lock().is_empty() && r.browser.opened.lock().is_empty());
        // Commands on: still no files, and the screen switch does not stand in.
        r.access
            .set_switches(
                "host1",
                Switches {
                    commands: true,
                    ..Switches::default()
                },
            )
            .unwrap();
        let res = r.h.handle(&host, RPC_TOOL, calls[1].1.clone()).await;
        assert_eq!(res.unwrap_err().code, ERR_FORBIDDEN);
        let res = r.h.handle(&host, "desktop.windows", Value::Null).await;
        assert_eq!(res.unwrap_err().code, ERR_FORBIDDEN, "screen is off");
    }

    /// A command always asks on THIS computer with the complete masked text, the
    /// shell and the folder; the never-list, a protected folder, Praxi's own data
    /// folder and a text too long to show are refused without a card.
    #[tokio::test]
    async fn a_command_asks_with_the_full_card_and_dangerous_ones_never_do() {
        let r = rig(false, true);
        r.access.set_switches("host1", all_families()).unwrap();
        let host = device(Role::Host);
        let call = r.h.handle(
            &host,
            RPC_TOOL,
            tool_params(
                "shell_run",
                serde_json::json!({"command": "echo hunter2 && dir"}),
            ),
        );
        let (res, info) = tokio::join!(call, answer(&r.requests, true));
        let out = res.unwrap();
        assert_eq!(info.action, "run command");
        assert!(
            info.payload.contains("echo •••• && dir"),
            "{}",
            info.payload
        );
        assert!(!info.payload.contains("hunter2"));
        assert!(info.target.contains("on this computer"));
        // The result is masked too, and the tool ran once with the user's home as folder.
        assert!(!out.to_string().contains("hunter2"), "{out}");
        assert_eq!(r.shell.inputs.lock().len(), 1);
        // Refused with no card.
        let data = r.dir.path().display().to_string();
        let long = "x".repeat(MAX_PAYLOAD_CHARS + 1);
        let refused = [
            "rm -rf /".to_owned(),
            format!("type {data}/peer/identity.key"),
            long,
        ];
        for command in refused {
            let res =
                r.h.handle(
                    &host,
                    RPC_TOOL,
                    tool_params("shell_run", serde_json::json!({"command": command})),
                )
                .await;
            assert_eq!(res.unwrap_err().code, ERR_FORBIDDEN, "{command:.30}");
        }
        assert_eq!(r.shell.inputs.lock().len(), 1, "nothing else ran");
        assert!(r.requests.list().is_empty());
        // Only the served tools are reachable.
        let res =
            r.h.handle(
                &host,
                RPC_TOOL,
                tool_params("script_run", serde_json::json!({})),
            )
            .await;
        assert_eq!(res.unwrap_err().code, ERR_FORBIDDEN);
    }

    /// Files: the real tools are served, so Praxi's own data and credentials stay
    /// unreachable; a write asks every time and writes the path on the card.
    #[tokio::test]
    async fn files_respect_the_protected_places_and_writes_ask_each_time() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join("secret.txt"), "praxi state").unwrap();
        let open = dir.path().join("notes.txt");
        std::fs::write(&open, "hello").unwrap();
        let r = rig(false, true);
        // Serve the real tools over this data dir.
        let real = builtin_tools::pc::local_tools(&data);
        let h = HostDesktopHandler::new(PcTargetDeps {
            raw: r.fake.clone(),
            data: data.clone(),
            access: r.access.clone(),
            requests: r.requests.clone(),
            gate: Arc::new(Gate {
                epoch: Arc::new(AtomicU64::new(0)),
                refused: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            }),
            owner_ui_live: Arc::new(|| true),
            audit: Arc::new(|_, _| Ok(())),
            redact: Arc::new(str::to_owned),
            own_pid: OWN_PID,
            tools: real,
            browser: None,
        })
        .with_ask_timeout(Duration::from_secs(5));
        write_pairing(&data, "host", "g1");
        r.access.set_switches("host1", all_families()).unwrap();
        let host = device(Role::Host);
        // Praxi's own data is refused by the file tool itself; no card for a refusal.
        let secret = data.join("secret.txt").display().to_string();
        let first = h.handle(
            &host,
            RPC_TOOL,
            tool_params("fs_read", serde_json::json!({"path": secret})),
        );
        let (res, info) = tokio::join!(first, answer(&r.requests, true));
        assert_eq!(info.kind, "session_read", "the first look asks once");
        let err = res.unwrap_err();
        assert!(
            !err.message.contains("praxi state"),
            "Praxi's data is never returned"
        );
        // An ordinary file reads (the session allowance is now given).
        let listing = h
            .handle(
                &host,
                RPC_TOOL,
                tool_params(
                    "fs_read",
                    serde_json::json!({"path": open.display().to_string()}),
                ),
            )
            .await
            .unwrap();
        assert!(listing.to_string().contains("hello"), "{listing}");
        // A write asks with the resolved path; denying it writes nothing.
        let target = dir.path().join("out.txt");
        let call = h.handle(
            &host,
            RPC_TOOL,
            tool_params(
                "fs_write",
                serde_json::json!({"path": target.display().to_string(), "content": "x"}),
            ),
        );
        let (res, info) = tokio::join!(call, answer(&r.requests, false));
        assert_eq!(res.unwrap_err().code, ERR_FORBIDDEN);
        assert_eq!(info.action, "write file");
        assert!(info.target.contains("out.txt") && info.target.contains("creates a new file"));
        assert!(!target.exists());
    }

    /// The browser: opening a page asks with the whole URL; other schemes and
    /// page scripts are not served.
    #[tokio::test]
    async fn the_browser_asks_for_every_input_and_only_opens_web_pages() {
        let r = rig(false, true);
        r.access.set_switches("host1", all_families()).unwrap();
        let host = device(Role::Host);
        let call = r.h.handle(
            &host,
            RPC_BROWSER,
            serde_json::json!({"op": "open", "url": "https://example.com/a?b=1", "bot": {"id": "bot-a", "name": "Ayla"}}),
        );
        let (res, info) = tokio::join!(call, answer(&r.requests, true));
        assert!(res.is_ok(), "{res:?}");
        assert_eq!(info.payload, "https://example.com/a?b=1");
        assert!(info.target.contains("example.com"));
        assert_eq!(r.browser.opened.lock().len(), 1);
        for params in [
            serde_json::json!({"op": "open", "url": "file:///C:/Windows/win.ini", "bot": {"id": "bot-a", "name": "Ayla"}}),
            serde_json::json!({"op": "open", "url": "javascript:alert(1)", "bot": {"id": "bot-a", "name": "Ayla"}}),
            serde_json::json!({"op": "eval", "js": "1", "bot": {"id": "bot-a", "name": "Ayla"}}),
        ] {
            let res = r.h.handle(&host, RPC_BROWSER, params).await;
            assert_eq!(res.unwrap_err().code, ERR_FORBIDDEN);
        }
        assert_eq!(r.browser.opened.lock().len(), 1);
        // Reading the page asks once for the session, then not again.
        let snap = r.h.handle(
            &host,
            RPC_BROWSER,
            serde_json::json!({"op": "snapshot", "bot": {"id": "bot-a", "name": "Ayla"}}),
        );
        let (res, info) = tokio::join!(snap, answer(&r.requests, true));
        assert!(res.is_ok());
        assert_eq!(info.kind, "session_read");
    }

    /// Review note 3: a bot never fills a password field through the browser.
    #[tokio::test]
    async fn browser_fill_into_a_password_field_is_refused() {
        let r = rig(false, true);
        r.access.set_switches("host1", all_families()).unwrap();
        r.browser.secret_fields.store(true, Ordering::SeqCst);
        let res = r
            .h
            .handle(
                &device(Role::Host),
                RPC_BROWSER,
                serde_json::json!({"op": "fill", "ref": "e1", "text": "hunter2", "bot": {"id": "bot-a", "name": "Ayla"}}),
            )
            .await;
        assert_eq!(res.unwrap_err().code, ERR_FORBIDDEN);
    }

    /// Follow-up: an unknown field fails closed, and a focused password field
    /// takes no typed characters (navigation keys still work).
    #[tokio::test]
    async fn fill_and_press_fail_closed_around_secret_fields() {
        let r = rig(false, true);
        r.access.set_switches("host1", all_families()).unwrap();
        let host = device(Role::Host);
        let call = |params: serde_json::Value| r.h.handle(&host, RPC_BROWSER, params);
        let bot = serde_json::json!({"id": "bot-a", "name": "Ayla"});
        r.browser.target_unknown.store(true, Ordering::SeqCst);
        let res =
            call(serde_json::json!({"op": "fill", "ref": "e1", "text": "x", "bot": bot})).await;
        assert_eq!(res.unwrap_err().code, ERR_FORBIDDEN);
        r.browser.target_unknown.store(false, Ordering::SeqCst);
        r.browser.secret_fields.store(true, Ordering::SeqCst);
        let res = call(serde_json::json!({"op": "press", "key": "a", "bot": bot})).await;
        assert_eq!(res.unwrap_err().code, ERR_FORBIDDEN);
        assert!(is_navigation_key("Tab") && !is_navigation_key("Control+a"));
    }

    /// Blocker 1: the card shows the complete REAL destination; what cannot be
    /// shown whole, or is a protected place on its real path, is refused.
    #[tokio::test]
    async fn a_write_is_bound_to_its_complete_real_path() {
        let r = rig(false, true);
        r.access.set_switches("host1", all_families()).unwrap();
        let host = device(Role::Host);
        let outside = tempfile::tempdir().unwrap();
        let base = outside.path().join("work");
        std::fs::create_dir_all(&base).unwrap();
        // A long nested path (over the old 300-character cut) is shown whole.
        let deep = format!("{}/notes.txt", "nested-folder/".repeat(30));
        let target = base.join(&deep);
        let call = r.h.handle(
            &host,
            RPC_TOOL,
            tool_params(
                "fs_write",
                serde_json::json!({"path": target.display().to_string(), "content": "x"}),
            ),
        );
        let (res, info) = tokio::join!(call, answer(&r.requests, true));
        assert!(res.is_ok(), "{res:?}");
        assert!(info.target.chars().count() > 300);
        assert!(info.target.contains("notes.txt"), "tail is on the card");
        // The tool is handed exactly the path on the card.
        let sent = r.write.inputs.lock()[0]["path"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(info.target.starts_with(&sent), "{sent}");
        // Git internals, shell profiles and `..` are refused with no card, also
        // behind a long prefix.
        for bad in [
            format!("{}/{deep}/.git/hooks/post-checkout", base.display()),
            format!("{}/.bashrc", base.display()),
            format!("{}/../outside.txt", base.display()),
        ] {
            let res =
                r.h.handle(
                    &host,
                    RPC_TOOL,
                    tool_params("fs_write", serde_json::json!({"path": bad, "content": "x"})),
                )
                .await;
            assert_eq!(res.unwrap_err().code, ERR_FORBIDDEN, "{bad:.60}");
        }
        assert!(r.requests.list().is_empty());
        // A command runs in the real folder on the card, and a folder that
        // does not exist is refused.
        let call = r.h.handle(
            &host,
            RPC_TOOL,
            tool_params(
                "shell_run",
                serde_json::json!({"command": "dir", "cwd": base.display().to_string()}),
            ),
        );
        let (res, info) = tokio::join!(call, answer(&r.requests, true));
        assert!(res.is_ok());
        assert!(info.target.contains("work"), "{}", info.target);
        let missing = base.join("no-such-folder");
        let res =
            r.h.handle(
                &host,
                RPC_TOOL,
                tool_params(
                    "shell_run",
                    serde_json::json!({"command": "dir", "cwd": missing.display().to_string()}),
                ),
            )
            .await;
        assert_eq!(res.unwrap_err().code, ERR_FORBIDDEN);
    }

    /// Blocker 1 (links): a link out of the folder shows, and writes, the
    /// destination it really points to.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlink_shows_its_real_destination() {
        let r = rig(false, true);
        r.access.set_switches("host1", all_families()).unwrap();
        let host = device(Role::Host);
        let outside = tempfile::tempdir().unwrap();
        let real = outside.path().join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = outside.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let call = r.h.handle(
            &host,
            RPC_TOOL,
            tool_params(
                "fs_write",
                serde_json::json!({"path": link.join("a.txt").display().to_string(), "content": "x"}),
            ),
        );
        let (res, info) = tokio::join!(call, answer(&r.requests, true));
        assert!(res.is_ok());
        assert!(info.target.contains("/real/a.txt"), "{}", info.target);
    }

    /// Blocker 2: the browser is not watched while the human drives, and what
    /// was captured across a control transition is discarded.
    #[tokio::test]
    async fn browser_reads_respect_the_human_latch_and_discard_across_a_transition() {
        let r = rig(false, true);
        r.access.set_switches("host1", all_families()).unwrap();
        let host = device(Role::Host);
        // First look allowed.
        let snap = serde_json::json!({"op": "snapshot", "bot": {"id": "bot-a", "name": "Ayla"}});
        let call = r.h.handle(&host, RPC_BROWSER, snap.clone());
        let (res, _) = tokio::join!(call, answer(&r.requests, true));
        assert!(res.is_ok());
        assert_eq!(r.browser.snapshots.load(Ordering::SeqCst), 1);
        // The human takes the screen: nothing is read at all.
        r.refused.store(true, Ordering::SeqCst);
        for op in ["snapshot", "screenshot", "tabs", "current_url"] {
            let res =
                r.h.handle(
                    &host,
                    RPC_BROWSER,
                    serde_json::json!({"op": op, "bot": {"id": "bot-a", "name": "Ayla"}}),
                )
                .await;
            assert_eq!(res.unwrap_err().code, ERR_FORBIDDEN, "{op}");
        }
        assert_eq!(r.browser.snapshots.load(Ordering::SeqCst), 1);
        // Control changes hands while a read is captured: discarded.
        r.refused.store(false, Ordering::SeqCst);
        *r.browser.bump.lock() = Some(r.epoch.clone());
        let res = r.h.handle(&host, RPC_BROWSER, snap).await;
        assert_eq!(res.unwrap_err().code, ERR_FORBIDDEN);
    }

    /// The page address leaves the host through the same redactor as every
    /// other text result.
    #[tokio::test]
    async fn current_url_passes_through_the_redactor() {
        let browser = Arc::new(FakeBrowser::default());
        *browser.url.lock() = Some("https://example.test/?token=hunter2".into());
        let driver: Arc<dyn BrowserDriver> = browser;
        let redact: Arc<dyn Fn(&str) -> String + Send + Sync> =
            Arc::new(|t| t.replace("hunter2", "••••"));
        let out = run_browser_op(&driver, "current_url", &serde_json::json!({}), &redact)
            .await
            .unwrap();
        assert_eq!(out["url"], "https://example.test/?token=••••");
    }

    /// Blocker 3: an "allow" that was already delivered does not run once ITS
    /// family was switched off, even though another family is still on, and a
    /// delivered first-look allow does not restore a session allowance.
    #[tokio::test]
    async fn a_delivered_allow_does_not_outlive_its_families_switch() {
        let r = rig(false, true);
        r.access.set_switches("host1", all_families()).unwrap();
        let host = device(Role::Host);
        let call = r.h.handle(
            &host,
            RPC_TOOL,
            tool_params("shell_run", serde_json::json!({"command": "dir"})),
        );
        let change = async {
            let id = loop {
                if let Some(i) = r.requests.list().first() {
                    break i.id.clone();
                }
                tokio::task::yield_now().await;
            };
            // Commands off, everything else still on; the allow was delivered.
            let mut sw = all_families();
            sw.commands = false;
            r.access.set_switches("host1", sw).unwrap();
            r.requests.decide(&id, true);
        };
        let (res, ()) = tokio::join!(call, change);
        assert_eq!(res.unwrap_err().code, ERR_FORBIDDEN);
        assert!(r.shell.inputs.lock().is_empty(), "the command did not run");
        // The same for a first look: the browser family switched off while its
        // question is open must not get a session allowance back.
        let call = r.h.handle(
            &host,
            RPC_BROWSER,
            serde_json::json!({"op": "snapshot", "bot": {"id": "bot-a", "name": "Ayla"}}),
        );
        let change = async {
            let id = loop {
                if let Some(i) = r.requests.list().first() {
                    break i.id.clone();
                }
                tokio::task::yield_now().await;
            };
            let mut sw = all_families();
            sw.commands = false;
            sw.browser = false;
            r.access.set_switches("host1", sw).unwrap();
            r.requests.decide(&id, true);
        };
        let (res, ()) = tokio::join!(call, change);
        assert_eq!(res.unwrap_err().code, ERR_FORBIDDEN);
        assert_eq!(r.browser.snapshots.load(Ordering::SeqCst), 0);
        assert!(
            !r.access
                .session_read_allowed("host1", "bot-a", PcFamily::Browser)
        );
    }

    /// Blocker 4: the served browser's network policy: loopback (the daemon's
    /// own API), link-local/metadata never; a private host only with this
    /// computer's own card; nothing survives the browser switch being turned off.
    #[tokio::test]
    async fn the_served_browser_cannot_reach_internal_services() {
        let r = rig(false, true);
        r.access.set_switches("host1", all_families()).unwrap();
        let host = device(Role::Host);
        for url in [
            "http://127.0.0.1:8123/v1/status",
            "http://169.254.169.254/latest/meta-data/",
        ] {
            let res =
                r.h.handle(
                    &host,
                    RPC_BROWSER,
                    serde_json::json!({"op": "open", "url": url, "bot": {"id": "bot-a", "name": "Ayla"}}),
                )
                .await;
            assert_eq!(res.unwrap_err().code, ERR_FORBIDDEN, "{url}");
        }
        assert!(
            r.requests.list().is_empty(),
            "no card for a blocked address"
        );
        assert!(r.browser.opened.lock().is_empty());
        // Serve one public open so the request context exists, then use the
        // network policy the way the browser does for every request.
        let call = r.h.handle(
            &host,
            RPC_BROWSER,
            serde_json::json!({"op": "open", "url": "https://93.184.216.34/", "bot": {"id": "bot-a", "name": "Ayla"}}),
        );
        let (res, _) = tokio::join!(call, answer(&r.requests, true));
        assert!(res.is_ok());
        let check = r
            .browser
            .check
            .lock()
            .clone()
            .expect("a policy is installed");
        // A redirect or subresource to an internal address.
        for url in [
            "http://127.0.0.1:8123/",
            "http://[::1]/",
            "http://169.254.169.254/",
        ] {
            assert!(check.check(url.into()).await.is_err(), "{url}");
        }
        assert!(
            check
                .check("https://93.184.216.34/x.png".into())
                .await
                .is_ok()
        );
        // A private host asks on THIS computer; denied stays blocked.
        let denied = tokio::join!(
            check.check("http://10.0.0.5/admin".into()),
            answer(&r.requests, false)
        );
        assert!(denied.0.is_err());
        let allowed = tokio::join!(
            check.check("http://10.0.0.5/admin".into()),
            answer(&r.requests, true)
        );
        assert!(allowed.0.is_ok());
        assert!(
            check.check("http://10.0.0.5/other".into()).await.is_ok(),
            "same host, this session"
        );
        // Browser switched off: the earlier allowance is gone.
        let mut sw = all_families();
        sw.browser = false;
        r.access.set_switches("host1", sw).unwrap();
        assert!(check.check("http://10.0.0.5/admin".into()).await.is_err());
    }

    /// Review note 2: the audit row of an approved action records what ran
    /// (masked, bounded) and a digest of the whole card text.
    #[tokio::test]
    async fn the_audit_row_records_the_masked_command() {
        let rows: Arc<Mutex<Vec<(String, Value)>>> = Arc::default();
        let r = rig(false, true);
        let log = rows.clone();
        let h = HostDesktopHandler::new(PcTargetDeps {
            raw: r.fake.clone(),
            data: r.dir.path().to_path_buf(),
            access: r.access.clone(),
            requests: r.requests.clone(),
            gate: Arc::new(Gate {
                epoch: Arc::new(AtomicU64::new(0)),
                refused: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            }),
            owner_ui_live: Arc::new(|| true),
            audit: Arc::new(move |a, d| {
                log.lock().push((a.to_owned(), d));
                Ok(())
            }),
            redact: Arc::new(|t| t.replace("hunter2", "••••")),
            own_pid: OWN_PID,
            tools: vec![r.shell.clone() as Arc<dyn Tool>],
            browser: None,
        })
        .with_ask_timeout(Duration::from_secs(5));
        r.access.set_switches("host1", all_families()).unwrap();
        let host = device(Role::Host);
        let call = h.handle(
            &host,
            RPC_TOOL,
            tool_params(
                "shell_run",
                serde_json::json!({"command": "echo hunter2\nRemove-Item C:/work/old -Recurse"}),
            ),
        );
        let (res, _) = tokio::join!(call, answer(&r.requests, true));
        assert!(res.is_ok(), "{res:?}");
        let rows = rows.lock();
        let act = rows
            .iter()
            .find(|(a, _)| a == "pc_target.act")
            .map(|(_, d)| d.clone())
            .expect("an act row");
        // Only the length and a digest of the payload are kept, never its text.
        assert!(act.get("payload").is_none(), "{act}");
        assert!(!act.to_string().contains("Remove-Item"), "{act}");
        assert!(!act.to_string().contains("hunter2"), "{act}");
        assert!(act["payload_chars"].as_u64().unwrap() > 0, "{act}");
        assert_eq!(act["bot"], "bot-a");
        assert_eq!(act["payload_sha256"].as_str().unwrap().len(), 64);
    }

    /// Review note 5: launching a program is refused for what a command is
    /// refused for (the never-list, Praxi's data folder), with no card.
    #[tokio::test]
    async fn a_launch_honours_the_command_refusals() {
        let r = rig(false, true);
        let host = device(Role::Host);
        let data = r.dir.path().display().to_string();
        for (exe, args) in [
            ("cmd.exe", vec!["/c".to_owned(), "rm -rf /".to_owned()]),
            ("notepad.exe", vec![format!("{data}/peer/identity.key")]),
        ] {
            let params = serde_json::json!({"app": {"executable": exe, "args": args}});
            let res = r.h.handle(&host, "desktop.launch", params).await;
            assert_eq!(res.unwrap_err().code, ERR_FORBIDDEN, "{exe}");
        }
        assert!(r.requests.list().is_empty());
    }

    /// Switching one family off ends its session allowance only.
    #[test]
    fn switches_persist_per_family_and_old_files_mean_screen_on() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("peer")).unwrap();
        // A file written before the families existed.
        std::fs::write(
            dir.path().join("peer").join("computer-access.json"),
            r#"{"hosts":{"h":{"always_read":true}}}"#,
        )
        .unwrap();
        let s = AccessStore::open(dir.path());
        assert!(s.family_on("h", PcFamily::Screen) && s.always_read("h"));
        assert!(!s.family_on("h", PcFamily::Commands));
        s.set_switches(
            "h",
            Switches {
                commands: true,
                ..Switches::default()
            },
        )
        .unwrap();
        let again = AccessStore::open(dir.path());
        assert!(again.family_on("h", PcFamily::Commands));
        assert!(!again.family_on("h", PcFamily::Screen));
        again.set_switches("h", Switches::default()).unwrap();
        assert!(!AccessStore::open(dir.path()).enabled("h"));
    }

    async fn answer(requests: &PcRequests, allow: bool) -> daemon_api::pc_access::PcRequestInfo {
        let info = loop {
            if let Some(r) = requests.list().first() {
                break r.clone();
            }
            tokio::task::yield_now().await;
        };
        assert!(requests.decide(&info.id, allow));
        info
    }

    fn type_params(text: &str) -> Value {
        serde_json::json!({ "action": Action::TypeText { text: text.into() } })
    }

    /// Blocker 4: the card shows the COMPLETE masked payload and the program
    /// that would receive it, and what cannot be shown completely is refused.
    #[tokio::test]
    async fn card_shows_the_full_masked_payload_and_the_resolved_target() {
        let r = rig(false, true);
        let host = device(Role::Host);
        let long = format!("{}{}", "harmless ".repeat(40), "and then hunter2 DELETE");
        let call = r.h.handle(&host, "desktop.act", type_params(&long));
        let (res, info) = tokio::join!(call, answer(&r.requests, true));
        assert!(res.is_ok(), "{res:?}");
        assert!(
            info.payload.contains("and then •••• DELETE"),
            "{}",
            info.payload
        );
        assert!(!info.payload.contains("hunter2"));
        assert!(info.target.contains("notepad.exe") && info.target.contains("Notes"));
        // Too long to show completely, or hiding characters: refused, no card.
        let huge = "x".repeat(MAX_PAYLOAD_CHARS + 1);
        for text in [huge.as_str(), "ok\u{202e}evil"] {
            let res = r.h.handle(&host, "desktop.act", type_params(text)).await;
            assert_eq!(res.unwrap_err().code, ERR_FORBIDDEN);
        }
        assert!(r.requests.list().is_empty());
    }

    /// Blocker 5: approved input is bound to the program shown and never
    /// reaches Praxi's own windows by keyboard or by coordinates.
    #[tokio::test]
    async fn input_is_bound_to_the_shown_program_and_never_reaches_praxi() {
        let r = rig(false, true);
        let host = device(Role::Host);
        let call = r.h.handle(&host, "desktop.act", type_params("hello"));
        let (res, _) = tokio::join!(call, answer(&r.requests, true));
        assert!(res.is_ok());
        assert_eq!(r.fake.acts.load(Ordering::SeqCst), 1, "guarded send only");
        assert_eq!(
            r.fake.guarded_for.lock().as_ref().map(|i| i.pid),
            Some(100),
            "dispatch is bound to the program on the card"
        );
        // Foreground is Praxi: keys and typing are refused outright.
        r.fake.foreground.lock().title = "Praxi-Bot".into();
        let res = r.h.handle(&host, "desktop.act", type_params("x")).await;
        assert_eq!(res.unwrap_err().code, ERR_FORBIDDEN);
        r.fake.foreground.lock().title = "Notes".into();
        // A coordinate click inside the Praxi window (x 500..600) is refused.
        let click = |x: f64| {
            serde_json::json!({ "action": Action::Click {
                target: Target::ByCoordinate { x, y: 10.0, space: CoordinateSpace::Absolute },
                method: ClickMethod::Global } })
        };
        let res = r.h.handle(&host, "desktop.act", click(550.0)).await;
        assert_eq!(res.unwrap_err().code, ERR_FORBIDDEN);
        assert!(
            r.requests.list().is_empty(),
            "no card for a forbidden target"
        );
        assert_eq!(r.fake.acts.load(Ordering::SeqCst), 1);
    }

    /// Blocker 3: switching off (or unpairing) while a card is open stops the
    /// action even if the card is answered "allow".
    #[tokio::test]
    async fn a_parked_action_does_not_run_after_the_switch_or_pairing_changed() {
        for unpair in [false, true] {
            let r = rig(false, true);
            let host = device(Role::Host);
            let call = r.h.handle(&host, "desktop.act", type_params("hello"));
            let change = async {
                let id = loop {
                    if let Some(i) = r.requests.list().first() {
                        break i.id.clone();
                    }
                    tokio::task::yield_now().await;
                };
                if unpair {
                    std::fs::remove_file(r.dir.path().join("peer").join("devices.json")).unwrap();
                } else {
                    r.access.set("host1", false, false).unwrap();
                }
                // Even a late "allow" must not run it.
                r.requests.decide(&id, true);
            };
            let (res, ()) = tokio::join!(call, change);
            assert_eq!(res.unwrap_err().code, ERR_FORBIDDEN, "unpair={unpair}");
            assert_eq!(r.fake.acts.load(Ordering::SeqCst), 0, "unpair={unpair}");
        }
    }

    /// Blocker 2: reads hide Praxi's windows, are discarded across a control
    /// transition, need the owner's app and a working audit, and the first
    /// look each session asks.
    #[tokio::test]
    async fn reads_hide_praxi_respect_latch_lease_audit_and_ask_first() {
        // Praxi window is not listed, and a full-screen capture is refused
        // while it is open; a capture of another window is allowed.
        let r = rig(false, true);
        let host = device(Role::Host);
        let list =
            r.h.handle(&host, "desktop.windows", Value::Null)
                .await
                .unwrap();
        let titles: Vec<&str> = list
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|w| w["title"].as_str())
            .collect();
        assert_eq!(titles, vec!["Notes"]);
        let shot =
            |region: Region| serde_json::json!({"region": region, "quality": Quality::Medium});
        let full =
            r.h.handle(&host, "desktop.screenshot", shot(Region::FullScreen))
                .await;
        assert!(full.is_err(), "full screen overlaps the Praxi window");
        assert_eq!(r.fake.shots.load(Ordering::SeqCst), 0);
        let one =
            r.h.handle(
                &host,
                "desktop.screenshot",
                shot(Region::Window { window_id: 1 }),
            )
            .await;
        assert!(one.is_ok(), "{one:?}");
        assert_eq!(r.requests.reading(), vec!["Server".to_string()]);

        // The human took control during the capture: discarded.
        let r2 = rig(true, true);
        let res =
            r2.h.handle(
                &host,
                "desktop.screenshot",
                shot(Region::Window { window_id: 1 }),
            )
            .await;
        assert!(
            res.is_err(),
            "observation across a latch transition is discarded"
        );

        // Owner app not running / audit failing: nothing is served.
        r.live.store(false, Ordering::SeqCst);
        assert!(
            r.h.handle(&host, "desktop.windows", Value::Null)
                .await
                .is_err()
        );
        r.live.store(true, Ordering::SeqCst);
        r.audit_ok.store(false, Ordering::SeqCst);
        assert!(
            r.h.handle(&host, "desktop.windows", Value::Null)
                .await
                .is_err()
        );

        // Default: the first look asks one card; "always" skips it.
        let r3 = rig(false, false);
        let call = r3.h.handle(&host, "desktop.windows", Value::Null);
        let (res, info) = tokio::join!(call, answer(&r3.requests, true));
        assert!(res.is_ok());
        assert_eq!(info.kind, "session_read");
        assert!(
            r3.h.handle(&host, "desktop.windows", Value::Null)
                .await
                .is_ok(),
            "asked once per session"
        );
    }

    /// Switch off, other roles and unknown peers never reach the backend.
    #[tokio::test]
    async fn only_the_adopted_host_with_the_switch_on_is_served() {
        let r = rig(false, true);
        for role in [Role::Desktop, Role::Phone, Role::Node] {
            let res =
                r.h.handle(&device(role), "desktop.windows", Value::Null)
                    .await;
            assert_eq!(res.unwrap_err().code, ERR_FORBIDDEN, "{role:?}");
        }
        r.access.set("host1", false, false).unwrap();
        let res =
            r.h.handle(&device(Role::Host), "desktop.windows", Value::Null)
                .await;
        assert_eq!(res.unwrap_err().code, ERR_FORBIDDEN);
    }

    /// Review blocker 1: what the person at this computer allowed for one
    /// bot (a session's first look, the screen's "always") never covers
    /// another bot of the same server, and the card names the bot.
    #[tokio::test]
    async fn a_session_allowance_for_one_bot_does_not_cover_another() {
        let r = rig(false, true);
        r.access.set_switches("host1", all_families()).unwrap();
        let host = device(Role::Host);
        let read = |bot: &str| {
            as_bot(
                serde_json::json!({"name": "fs_read", "input": {"path": "C:/x.txt"}}),
                bot,
            )
        };
        // Bot A: first look asks (allowed), then reads without a card.
        let (res, info) = tokio::join!(
            r.h.handle(&host, RPC_TOOL, read("bot-a")),
            answer(&r.requests, true)
        );
        assert!(res.is_ok(), "{res:?}");
        assert_eq!(info.kind, "session_read");
        assert_eq!(info.bot_name.as_deref(), Some("Bot bot-a"));
        assert!(r.h.handle(&host, RPC_TOOL, read("bot-a")).await.is_ok());
        assert_eq!(r.write.inputs.lock().len() + r.shell.inputs.lock().len(), 0);
        // Bot B of the same server asks again; denied, nothing is read.
        let (res, info) = tokio::join!(
            r.h.handle(&host, RPC_TOOL, read("bot-b")),
            answer(&r.requests, false)
        );
        assert_eq!(res.unwrap_err().code, ERR_FORBIDDEN);
        assert_eq!(info.bot_name.as_deref(), Some("Bot bot-b"));
        // The screen's "always" switch (on in this rig) covers a run placed
        // here, never a named bot's first look.
        assert!(
            r.h.handle(&host, "desktop.windows", Value::Null)
                .await
                .is_ok()
        );
        let (res, info) = tokio::join!(
            r.h.handle(
                &host,
                "desktop.windows",
                as_bot(serde_json::json!({}), "bot-b")
            ),
            answer(&r.requests, false)
        );
        assert_eq!(res.unwrap_err().code, ERR_FORBIDDEN);
        assert_eq!(info.kind, "session_read");
        // A `pc.*` call that names no bot is refused without a card.
        let res =
            r.h.handle(
                &host,
                RPC_TOOL,
                serde_json::json!({"name": "fs_read", "input": {"path": "C:/x.txt"}}),
            )
            .await;
        assert_eq!(res.unwrap_err().code, ERR_FORBIDDEN);
        assert!(r.requests.list().is_empty());
    }

    /// Review blocker 1: each bot gets its own browser at this computer (tabs,
    /// page state, cookies) and its own private-host allowances.
    #[tokio::test]
    async fn each_bot_has_its_own_browser_and_private_host_allowance() {
        let r = rig(false, true);
        r.access.set_switches("host1", all_families()).unwrap();
        let host = device(Role::Host);
        let snap = |bot: &str| as_bot(serde_json::json!({"op": "snapshot"}), bot);
        let (res, _) = tokio::join!(
            r.h.handle(&host, RPC_BROWSER, snap("bot-a")),
            answer(&r.requests, true)
        );
        assert!(res.is_ok());
        let (res, _) = tokio::join!(
            r.h.handle(&host, RPC_BROWSER, snap("bot-b")),
            answer(&r.requests, true)
        );
        assert!(res.is_ok());
        assert!(r.h.handle(&host, RPC_BROWSER, snap("bot-a")).await.is_ok());
        assert_eq!(r.browsers_made.load(Ordering::SeqCst), 2, "one per bot");
        let (check_a, check_b) = {
            let c = r.browser.checks.lock();
            (c[0].clone(), c[1].clone())
        };
        let allowed = tokio::join!(
            check_a.check("http://10.0.0.5/admin".into()),
            answer(&r.requests, true)
        );
        assert!(allowed.0.is_ok());
        assert!(check_a.check("http://10.0.0.5/x".into()).await.is_ok());
        let (res, info) = tokio::join!(
            check_b.check("http://10.0.0.5/admin".into()),
            answer(&r.requests, false)
        );
        assert!(res.is_err(), "bot B did not inherit bot A's allowance");
        assert_eq!(info.bot_name.as_deref(), Some("Bot bot-b"));
    }
    /// Review blocker 2: the filtering proxy of a bot's browser asks the
    /// person at this computer about a private-range host it resolved (with
    /// the bot's name); loopback never gets that far.
    #[tokio::test]
    async fn the_proxy_asks_about_a_private_host_for_that_bot_only() {
        let r = rig(false, true);
        r.access.set_switches("host1", all_families()).unwrap();
        let host = device(Role::Host);
        let snap = as_bot(serde_json::json!({"op": "snapshot"}), "bot-a");
        let (res, _) = tokio::join!(
            r.h.handle(&host, RPC_BROWSER, snap),
            answer(&r.requests, true)
        );
        assert!(res.is_ok());
        let decide = r.browser.hosts.lock()[0].clone();
        let (res, info) = tokio::join!(decide.check("nas.home".into()), answer(&r.requests, true));
        assert!(res.is_ok());
        assert!(info.target.contains("nas.home"), "{}", info.target);
        assert_eq!(info.bot_name.as_deref(), Some("Bot bot-a"));
        assert!(decide.check("nas.home".into()).await.is_ok(), "asked once");
    }
}
