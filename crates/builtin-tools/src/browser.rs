//! Browser tools (permission category `Browser`; `browser_eval` is `Script`).
//!
//! The tools talk to a [`BrowserDriver`]. [`CdpDriver`] is the real
//! implementation over `browser-cdp`: one browser session per bot, launched
//! lazily on first use. The profile follows the runtime setting in
//! [`BrowserSettings`] (isolated temporary profile by default).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use browser_cdp::{
    ActionResult, Browser, DialogAction, DialogOptions, LaunchOptions, Page, ScreenshotOptions,
    ScreenshotTarget, SnapshotOptions, WaitCondition,
};
use secret_store::{FillSecrets, sanitize_label};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tool_registry::{
    Effect, PermissionCategory, StandingGrant, Tool, ToolContext, ToolError, ToolOutput, ToolSpec,
};

use crate::control::{ActionPermit, ControlRequest, DesktopControlGate};
use crate::fill_secret::{Source, names_a_secret, refused, scrub, source};
use crate::util::{exec_err, finish_text, parse, save_file, spec};

pub use browser_cdp::FieldTarget;

/// Why a driver cannot type a secret.
const NO_SECRET_FILL: &str = "this browser backend cannot tell which page frame holds a field, so secrets are not typed with it";

/// What `browser_wait` waits for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wait {
    /// Page text contains this string.
    Text(String),
    /// A CSS selector matches.
    Selector(String),
    /// No network activity for a short while.
    Idle,
}

/// Abstraction over a controllable browser. Every mutating call returns a
/// text view of what changed.
#[async_trait]
pub trait BrowserDriver: Send + Sync {
    /// Whether a local overlay should highlight mutations. All browser modes
    /// obey the computer-control admission barrier.
    fn on_desktop(&self) -> bool {
        false
    }
    /// Select a separate session for a different launch mode. Never mutates a
    /// shared driver's mode or closes another run's session.
    fn for_headless(&self, _headless: bool) -> Option<Arc<dyn BrowserDriver>> {
        None
    }
    /// Put the network guard of a served browser on this driver: every
    /// request through a filtering proxy that resolves and classifies the
    /// address itself (loopback, link-local/metadata never; private ranges
    /// only with the owner's allowance), popups blocked, and every new
    /// target paused until its requests are checked. Used for browsers a
    /// paired computer drives; the local managed-browser tools do not set
    /// one. Takes effect for sessions launched afterwards. Default: ignored.
    fn set_served_network(&self, _net: browser_cdp::ServedNetwork) {}
    /// Close every browser this driver owns (bot deleted, daemon stopping).
    async fn close_all(&self) {}
    /// Navigate to `url` (starting the browser if needed); returns the page snapshot.
    async fn open(&self, url: &str, profile: Option<&str>) -> Result<String, String>;
    /// Navigate to `url` with dialog options.
    async fn open_opts(
        &self,
        url: &str,
        profile: Option<&str>,
        dialog: Option<&str>,
        prompt_text: Option<&str>,
    ) -> Result<String, String> {
        let _ = (dialog, prompt_text);
        self.open(url, profile).await
    }
    /// Current page snapshot with element refs.
    async fn snapshot(&self, interactive_only: bool) -> Result<String, String>;
    /// Click the element `r`.
    async fn click(&self, r: &str) -> Result<String, String>;
    /// Click the element `r` with dialog options.
    async fn click_opts(
        &self,
        r: &str,
        dialog: Option<&str>,
        prompt_text: Option<&str>,
    ) -> Result<String, String> {
        let _ = (dialog, prompt_text);
        self.click(r).await
    }
    /// Replace the text of input `r`.
    async fn fill(&self, r: &str, text: &str) -> Result<String, String>;
    /// Where field `r` lives right now (label, role, and the origin of the
    /// FRAME that holds it, as the browser reports it). Backends that cannot
    /// say refuse, so no secret is ever typed through them.
    async fn field_target(&self, r: &str) -> Result<FieldTarget, String> {
        let _ = r;
        Err(NO_SECRET_FILL.into())
    }
    /// The element that has keyboard focus now. Backends that cannot say
    /// refuse (callers treat that as "may be a secret").
    async fn focused_target(&self) -> Result<FieldTarget, String> {
        Err(NO_SECRET_FILL.into())
    }
    /// Type `text` (an owner-stored secret) into field `r`, but only while the
    /// field's frame origin, re-read from the browser immediately before the
    /// text is inserted, satisfies `accept`. Never put `text` in the result.
    async fn fill_secret(
        &self,
        r: &str,
        text: &str,
        accept: &(dyn Fn(Option<String>) -> bool + Send + Sync),
    ) -> Result<String, String> {
        let _ = (r, text, accept);
        Err(NO_SECRET_FILL.into())
    }
    /// Choose option `value` in select `r`.
    async fn select(&self, r: &str, value: &str) -> Result<String, String>;
    /// Press a key or chord such as `Enter` or `Control+a`.
    async fn press(&self, key: &str) -> Result<String, String>;
    /// Scroll the page, or element `r`.
    async fn scroll(&self, r: Option<&str>, dx: f64, dy: f64) -> Result<String, String>;
    /// Wait for a condition.
    async fn wait(&self, cond: Wait, timeout: Option<Duration>) -> Result<String, String>;
    /// Go back in history.
    async fn back(&self) -> Result<String, String>;
    /// Go back in history with dialog options.
    async fn back_opts(&self, dialog: Option<&str>) -> Result<String, String> {
        let _ = dialog;
        self.back().await
    }
    /// List tabs; with `switch_to` make that tab (by listed number) current.
    async fn tabs(&self, switch_to: Option<usize>) -> Result<String, String>;
    /// Screenshot bytes and file extension.
    async fn screenshot(
        &self,
        full_page: bool,
        element: Option<&str>,
    ) -> Result<(Vec<u8>, &'static str), String>;
    /// Evaluate JavaScript in the page.
    async fn eval(&self, js: &str) -> Result<Value, String>;
    /// Browser-reported origin of the receiving ref, or the focused frame for a key press.
    async fn input_origin(&self, r: Option<&str>) -> Option<String> {
        self.field_target(r?).await.ok()?.frame_origin
    }
    /// URL of the top-level page of the current session as the BROWSER reports it (target
    /// info, not page script). `None`: no session, or the driver cannot tell.
    async fn current_url(&self) -> Option<String> {
        None
    }
    /// Whether the user is watching this browser right now (a live view is
    /// attached). `None`: the driver cannot tell, so nothing is claimed.
    fn live_view_attached(&self) -> Option<bool> {
        None
    }
}

/// The last line of a browser tool result: whether the user can see the page.
/// The model must not tell the user a page "is open" on their screen unless
/// this says so.
#[must_use]
pub fn view_line(attached: bool) -> &'static str {
    if attached {
        "[view: visible to the user in the live view]"
    } else {
        "[view: NOT visible to the user (no live view attached); this is the assistant's own browser, do not say they can see it]"
    }
}

/// `scheme://host:port` of an http(s) URL: lower case, no trailing dot, default port explicit.
#[must_use]
pub fn origin_of(url: &str) -> Option<String> {
    let url = url::Url::parse(url).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let host = url.host_str()?.trim_end_matches('.').to_ascii_lowercase();
    Some(format!(
        "{}://{host}:{}",
        url.scheme(),
        url.port_or_known_default()?
    ))
}

/// Runtime browser settings.
#[derive(Debug, Clone)]
pub struct BrowserSettings {
    /// Browser binary; discovered automatically when `None`.
    pub executable: Option<PathBuf>,
    /// Run without a window.
    pub headless: bool,
    /// Default profile directory. `None` means an isolated temporary profile.
    pub profile_dir: Option<PathBuf>,
    /// Where named profiles (`browser_open` `profile`) live.
    pub profiles_root: PathBuf,
    /// Real bot work/active takeover pins the managed process; watch never does.
    pub idle_timeout: Duration,
}

impl BrowserSettings {
    /// Isolated, headless defaults with named profiles under `<data_dir>/browsers`.
    #[must_use]
    pub fn isolated(data_dir: &Path) -> Self {
        Self {
            executable: None,
            headless: true,
            profile_dir: None,
            profiles_root: data_dir.join("browsers"),
            idle_timeout: Duration::from_secs(120),
        }
    }
}

struct Session {
    browser: Browser,
    page: Page,
    /// Own tab per subagent run (ADR 0025): `(lane, page)`, least recently used
    /// first. The top-level run keeps `page`; a lane's page is swapped into
    /// `page` for the length of one driver call (see [`with_session!`]).
    lanes: Vec<(String, Page)>,
    profile: Option<String>,
    headless: bool,
    last_work: tokio::time::Instant,
}

/// The whole browser start (spawn, debugging endpoint, first tab) gets this long
/// per attempt. Without it a stuck start plus the one relaunch outlasted the
/// tool timeout and the run showed only a spinner.
const LAUNCH_DEADLINE: Duration = Duration::from_secs(40);

/// How long a browser call waits for another call that holds the session.
const SESSION_WAIT: Duration = Duration::from_secs(45);

fn launch_deadline_message(headless: bool) -> String {
    let where_ = if headless {
        "headless"
    } else {
        "on this computer's desktop"
    };
    format!(
        "the browser did not start within {}s ({where_}). Do not retry browser_open in a loop;          run `bot doctor` on this computer (it names a missing display, XAUTHORITY or browser)          and tell the user what it reports.",
        LAUNCH_DEADLINE.as_secs()
    )
}

/// Tabs kept for subagent runs at once; the least recently used one is closed
/// when a new lane needs room.
const MAX_LANES: usize = 6;

impl Session {
    /// Make sure `lane` has its own tab (opened blank on first use).
    async fn ensure_lane(&mut self, lane: &str) -> Result<(), String> {
        if let Some(i) = self.lanes.iter().position(|(l, _)| l == lane) {
            let entry = self.lanes.remove(i);
            self.lanes.push(entry);
            return Ok(());
        }
        if self.lanes.len() >= MAX_LANES {
            let (_, oldest) = self.lanes.remove(0);
            let _ = oldest.close().await;
        }
        let page = self.browser.new_page("about:blank").await.map_err(s)?;
        self.lanes.push((lane.to_owned(), page));
        Ok(())
    }

    /// Exchange the shared page with `lane`'s page. Calling it twice restores
    /// the original arrangement, so a driver call that runs between the two
    /// swaps sees the lane's tab as `self.page`.
    fn swap_lane(&mut self, lane: &str) {
        if let Some((_, page)) = self.lanes.iter_mut().find(|(l, _)| l == lane) {
            std::mem::swap(&mut self.page, page);
        }
    }
}

/// [`BrowserDriver`] over Chrome/Edge via `browser-cdp`, launched lazily.
type BrowserPeers =
    Arc<parking_lot::Mutex<std::collections::HashMap<String, std::sync::Weak<CdpDriver>>>>;

pub struct CdpDriver {
    settings: BrowserSettings,
    alternate_mode: parking_lot::Mutex<Option<Arc<CdpDriver>>>,
    session: Arc<Mutex<Option<Session>>>,
    takeover_pin: Arc<std::sync::atomic::AtomicBool>,
    /// True while a bot run that used the computer is still active.
    activity: Arc<std::sync::RwLock<Option<ActivityProbe>>>,
    reaper_started: std::sync::atomic::AtomicBool,
    peers: Option<(String, BrowserPeers)>,
    /// Live-view slot of this bot's browser: idle until a session launches,
    /// then bound to its page. Capture stays lazy (viewer-driven).
    view: Option<Arc<browser_cdp::BrowserViewTarget>>,
    /// Network guard of served sessions (`None`: unrestricted).
    served: parking_lot::RwLock<Option<browser_cdp::ServedNetwork>>,
}

/// Reports whether real bot work is going on (see [`CdpDriver::set_activity_probe`]).
pub type ActivityProbe = Arc<dyn Fn() -> bool + Send + Sync>;

fn snap_opts(interactive_only: bool) -> SnapshotOptions {
    SnapshotOptions {
        interactive_only,
        compact: true,
        max_depth: None,
    }
}

fn s<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

/// Sanitize untrusted dialog message: strip control chars, cap at 300 chars with ellipsis, and escape quotes.
pub(crate) fn sanitize_dialog_message(msg: &str) -> String {
    const MAX_CHARS: usize = 300;
    let stripped: String = msg
        .replace(['\r', '\n', '\t'], " ")
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    let mut out = if stripped.chars().count() > MAX_CHARS {
        let mut truncated: String = stripped.chars().take(MAX_CHARS).collect();
        truncated.push_str("...");
        truncated
    } else {
        stripped
    };
    out = out.replace('"', "\\\"");
    out
}

fn render_action(r: &ActionResult) -> Result<String, String> {
    if !r.ok {
        let mut err = r.error.clone().unwrap_or_else(|| "action failed".into());
        for dl in &r.dialogs {
            let clean = sanitize_dialog_message(&dl.message);
            let status = if dl.accepted { "accepted" } else { "dismissed" };
            let _ = write!(
                err,
                "\n~ dialog({}) {status} page content: \"{clean}\"",
                dl.kind
            );
        }
        if let Some(n) = &r.settle_note {
            let _ = write!(err, "\n~ {n}");
        }
        return Err(err);
    }
    let d = &r.new_snapshot_diff;
    let body = if d.url_changed.is_some()
        || (!d.is_empty() && d.render().len() >= r.snapshot.to_text().len())
    {
        // A navigation, or a diff no shorter than the page: show the page itself.
        r.snapshot.render()
    } else {
        format!(
            "# {} ({})\n{}",
            r.snapshot.title,
            r.snapshot.url,
            d.render()
        )
    };
    let mut out = body;
    for dl in &r.dialogs {
        let clean = sanitize_dialog_message(&dl.message);
        let status = if dl.accepted { "accepted" } else { "dismissed" };
        let _ = write!(
            out,
            "\n~ dialog({}) {status} page content: \"{clean}\"",
            dl.kind
        );
    }
    if let Some(n) = &r.settle_note {
        let _ = write!(out, "\n~ {n}");
    }
    Ok(out)
}

impl CdpDriver {
    /// Create a driver; nothing is launched until the first call.
    #[must_use]
    pub fn new(settings: BrowserSettings) -> Self {
        Self {
            settings,
            alternate_mode: parking_lot::Mutex::new(None),
            peers: None,
            session: Arc::new(Mutex::new(None)),
            takeover_pin: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            activity: Arc::new(std::sync::RwLock::new(None)),
            reaper_started: std::sync::atomic::AtomicBool::new(false),
            view: None,
            served: parking_lot::RwLock::new(None),
        }
    }

    fn desired_headless(&self) -> bool {
        self.settings.headless
    }

    fn profile_path(&self, profile: Option<&str>) -> Result<Option<PathBuf>, String> {
        match profile {
            None => Ok(self.settings.profile_dir.clone()),
            Some("isolated") => Ok(None),
            Some(name) => {
                if name.is_empty()
                    || !name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                {
                    return Err("profile must be letters, digits, - or _".into());
                }
                Ok(Some(self.settings.profiles_root.join(name)))
            }
        }
    }

    fn session_alive(sess: &Session) -> bool {
        !sess.browser.client().is_closed()
    }

    async fn launch_session(&self, profile: Option<&str>) -> Result<Session, String> {
        // Admission has already reserved this bot's run slot. Reclaim other
        // inactive profiles before spawning another Chromium on a small host.
        if let Some((id, peers)) = &self.peers {
            let others: Vec<_> = peers
                .lock()
                .iter()
                .filter(|(key, _)| *key != id)
                .filter_map(|(_, weak)| weak.upgrade())
                .collect();
            for other in others {
                let active = other
                    .activity
                    .read()
                    .ok()
                    .and_then(|p| p.clone())
                    .is_some_and(|p| p());
                if !active && !other.takeover_pin.load(std::sync::atomic::Ordering::SeqCst) {
                    let idle = other.session.try_lock().ok().and_then(|mut s| {
                        let active = other
                            .activity
                            .read()
                            .ok()
                            .and_then(|p| p.clone())
                            .is_some_and(|p| p());
                        if active || other.takeover_pin.load(std::sync::atomic::Ordering::SeqCst) {
                            None
                        } else {
                            s.take()
                        }
                    });
                    if let Some(session) = idle {
                        close_session(other.view.as_deref(), session).await;
                    }
                }
            }
        }
        let headless = self.desired_headless();
        let opts = LaunchOptions {
            executable: self.settings.executable.clone(),
            headless,
            user_data_dir: self.profile_path(profile)?,
            served: self.served.read().clone(),
            ..LaunchOptions::default()
        };
        let started = async {
            let browser = Browser::launch(opts).await.map_err(s)?;
            let page = browser.first_page().await.map_err(s)?;
            Ok::<_, String>((browser, page))
        };
        let (browser, page) = tokio::time::timeout(LAUNCH_DEADLINE, started)
            .await
            .map_err(|_| launch_deadline_message(headless))??;
        Ok(Session {
            browser,
            page,
            profile: Some(
                profile
                    .unwrap_or(if self.settings.profile_dir.is_some() {
                        "<default>"
                    } else {
                        "isolated"
                    })
                    .to_owned(),
            ),
            headless,
            last_work: tokio::time::Instant::now(),
            lanes: Vec::new(),
        })
    }

    async fn ensure<'a>(
        &self,
        guard: &'a mut Option<Session>,
        profile: Option<&str>,
        allow_launch: bool,
    ) -> Result<&'a mut Session, String> {
        self.start_idle_reaper();
        let desired_headless = self.desired_headless();
        // A crashed, closed, or mode-mismatched (headless vs headed) browser
        // must not stay cached across runs.
        if guard
            .as_ref()
            .is_some_and(|sess| !Self::session_alive(sess) || sess.headless != desired_headless)
            && let Some(old) = guard.take()
        {
            close_session(self.view.as_deref(), old).await;
        }
        if let Some(existing) = guard.as_ref() {
            if let Some(p) = profile
                && existing.profile.as_deref().unwrap_or("isolated") != p
            {
                return Err(format!(
                    "the browser is already running with profile {:?}; one browser session per bot",
                    existing.profile.as_deref().unwrap_or("isolated")
                ));
            }
        } else {
            if !allow_launch {
                return Err("browser_idle; call browser_open before reading the browser".into());
            }
            *guard = Some(self.launch_session(profile).await?);
            // Watching starts nothing: binding only tells the idle slot which
            // page to serve. The slot follows the driver that actually runs
            // (headless or headed); if it is still held by an older browser of
            // this bot, that one is retired to idle first.
            if let (Some(view), Some(session)) = (&self.view, guard.as_ref())
                && view.attach_page(&session.page).is_err()
            {
                let _ = view.detach_to_idle().await;
                let _ = view.attach_page(&session.page);
            }
        }
        let session = guard.as_mut().expect("session just ensured");
        session.last_work = tokio::time::Instant::now();
        Ok(session)
    }

    /// Adapter hook: watching alone must never call this or `ensure`.
    pub fn set_takeover_pin(&self, active: bool) {
        self.takeover_pin
            .store(active, std::sync::atomic::Ordering::SeqCst);
    }

    /// While `probe` returns true (a bot run that used the computer is
    /// active) the idle reaper keeps the browser and restarts its idle clock;
    /// watching never counts as activity.
    pub fn set_activity_probe(&self, probe: ActivityProbe) {
        if let Ok(mut slot) = self.activity.write() {
            *slot = Some(probe);
        }
    }

    fn start_idle_reaper(&self) {
        if self
            .reaper_started
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return;
        }
        let weak_session = Arc::downgrade(&self.session);
        let view = self.view.clone();
        let pin = Arc::clone(&self.takeover_pin);
        let activity = Arc::clone(&self.activity);
        let timeout = self.settings.idle_timeout;
        tokio::spawn(async move {
            let mut state = ReapState::default();
            loop {
                tokio::time::sleep(
                    timeout
                        .min(Duration::from_secs(5))
                        .max(Duration::from_millis(1)),
                )
                .await;
                let Some(session) = weak_session.upgrade() else {
                    break;
                };
                reap_tick(
                    &session,
                    view.as_deref(),
                    timeout,
                    &pin,
                    &activity,
                    &mut state,
                )
                .await;
            }
        });
    }

    /// Close the browser (the next call launches a new one).
    pub async fn shutdown(&self) {
        let session = self.session.lock().await.take();
        if let Some(sess) = session {
            close_session(self.view.as_deref(), sess).await;
        }
        let alternate = self.alternate_mode.lock().clone();
        if let Some(alt) = alternate {
            // One level only: the alternate driver has no alternate of its own.
            let session = alt.session.lock().await.take();
            if let Some(sess) = session {
                close_session(alt.view.as_deref(), sess).await;
            }
        }
    }

    /// One extra launch after a dead session or a terminal transport error.
    const RELAUNCH_LIMIT: u32 = 1;
}

/// What the idle reaper remembers between ticks.
#[derive(Default)]
struct ReapState {
    /// The previous tick saw a bot run using the computer.
    was_active: bool,
}

/// One idle-reaper tick, separated from its timer so tests can drive it with a
/// paused clock. The browser is kept while a takeover pins it or a bot run that
/// used the computer is active; the idle clock restarts at the tick that sees
/// the run end (never earlier, so the browser cannot be closed the instant a
/// run finishes), and the probe is checked again under the session lock so an
/// action admitted just before the close is not racing it.
async fn reap_tick(
    session: &Mutex<Option<Session>>,
    view: Option<&browser_cdp::BrowserViewTarget>,
    timeout: Duration,
    pin: &std::sync::atomic::AtomicBool,
    activity: &std::sync::RwLock<Option<ActivityProbe>>,
    state: &mut ReapState,
) {
    let run_active = || {
        activity
            .read()
            .ok()
            .and_then(|probe| probe.clone())
            .is_some_and(|probe| probe())
    };
    if pin.load(std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let active = run_active();
    if active || state.was_active {
        if let Ok(mut slot) = session.try_lock()
            && let Some(s) = slot.as_mut()
        {
            s.last_work = tokio::time::Instant::now();
        }
        state.was_active = active;
        return;
    }
    if let Ok(mut slot) = session.try_lock()
        && slot
            .as_ref()
            .is_some_and(|s| s.last_work.elapsed() >= timeout)
        // An action admitted since the check above holds the computer slot
        // before it takes this lock, so it is seen here.
        && !run_active()
        && let Some(s) = slot.take()
    {
        close_session(view, s).await;
    }
}

/// Close a browser and return its live-view slot to idle (only when the slot
/// still points at this browser's page), so the panel says "browser idle"
/// instead of a generic failure, and notifies listeners.
async fn close_session(view: Option<&browser_cdp::BrowserViewTarget>, session: Session) {
    if let Some(view) = view {
        let _ = view.detach_if_bound(&session.page).await;
    }
    let _ = session.browser.close().await;
}

tokio::task_local! {
    // Carried across control-gate waits, then checked with the session lock held.
    static EXPECTED_PAGE_ORIGIN: Option<String>;
}

tokio::task_local! {
    // The subagent run whose browser tab this call uses (`None`: the shared tab).
    static BROWSER_LANE: Option<String>;
}

fn current_lane() -> Option<String> {
    BROWSER_LANE.try_with(Clone::clone).ok().flatten()
}

fn expected_input_origin() -> Option<String> {
    EXPECTED_PAGE_ORIGIN.try_with(Clone::clone).ok().flatten()
}

/// Run `$body` against the cached session. A closed browser is replaced inside
/// `ensure`. A terminal transport error evicts the session and retries once.
macro_rules! with_session {
    ($self:expr, $profile:expr, |$sess:ident| $body:block) => {
        with_session!($self, $profile, true, |$sess| $body)
    };
    ($self:expr, $profile:expr, $allow_launch:expr, |$sess:ident| $body:block) => {{
        let mut outcome = None;
        for attempt in 0..=CdpDriver::RELAUNCH_LIMIT {
            let mut guard = tokio::time::timeout(SESSION_WAIT, $self.session.lock())
                .await
                .map_err(|_| {
                    format!(
                        "another browser call has held the browser for over {}s; try again shortly",
                        SESSION_WAIT.as_secs()
                    )
                })?;
            let step = {
                let $sess = $self.ensure(&mut guard, $profile, $allow_launch).await?;
                match current_lane() {
                    Some(lane) => {
                        $sess.ensure_lane(&lane).await?;
                        $sess.swap_lane(&lane);
                        let outcome = $body;
                        $sess.swap_lane(&lane);
                        outcome
                    }
                    None => $body,
                }
            };
            if let Some(session) = guard.as_mut() {
                session.last_work = tokio::time::Instant::now();
            }
            match step {
                Ok(value) => {
                    outcome = Some(value);
                    break;
                }
                Err(err) if err.is_fatal() && attempt < CdpDriver::RELAUNCH_LIMIT => {
                    *guard = None;
                }
                Err(err) => return Err(s(err)),
            }
        }
        outcome.ok_or_else(|| "browser session died and the relaunch failed".to_string())
    }};
}

#[async_trait]
impl BrowserDriver for CdpDriver {
    fn on_desktop(&self) -> bool {
        !self.desired_headless() && browser_cdp::host::has_display()
    }

    fn live_view_attached(&self) -> Option<bool> {
        // A screencast runs only while a viewer is subscribed.
        self.view
            .as_ref()
            .map(|view| view.status().active_profile.is_some())
    }

    fn set_served_network(&self, net: browser_cdp::ServedNetwork) {
        *self.served.write() = Some(net);
    }

    fn for_headless(&self, headless: bool) -> Option<Arc<dyn BrowserDriver>> {
        if headless == self.settings.headless {
            return None;
        }
        let mut alternate = self.alternate_mode.lock();
        Some(
            alternate
                .get_or_insert_with(|| {
                    let mut settings = self.settings.clone();
                    settings.headless = headless;
                    let mode = if headless { "headless" } else { "headed" };
                    settings.profile_dir = settings.profile_dir.map(|p| p.join(mode));
                    settings.profiles_root = settings.profiles_root.join(mode);
                    // `settings` is already this bot's profile. The caller must
                    // not wrap this driver in the managed-browser pool again:
                    // headed and headless keep separate directories and cookies.
                    let mut driver = CdpDriver::new(settings);
                    driver.takeover_pin = Arc::clone(&self.takeover_pin);
                    driver.activity = Arc::clone(&self.activity);
                    // One live-view slot per bot, whichever mode runs: the
                    // alternate driver binds it when it launches (and retires
                    // the other mode's page from it).
                    driver.view.clone_from(&self.view);
                    (*driver.served.write()).clone_from(&self.served.read());
                    Arc::new(driver)
                })
                .clone(),
        )
    }

    async fn close_all(&self) {
        self.shutdown().await;
    }

    async fn open_opts(
        &self,
        url: &str,
        profile: Option<&str>,
        dialog: Option<&str>,
        prompt_text: Option<&str>,
    ) -> Result<String, String> {
        let dialog_opts = DialogOptions {
            action: DialogAction::from_str_opt(dialog),
            prompt_text: prompt_text.map(str::to_string),
        };
        let snap = snap_opts(false);
        let r = with_session!(self, profile, |sess| {
            sess.page
                .navigate_snapshot_with(url, &snap, &dialog_opts)
                .await
        })?;
        render_action(&r)
    }

    async fn open(&self, url: &str, profile: Option<&str>) -> Result<String, String> {
        self.open_opts(url, profile, None, None).await
    }

    async fn snapshot(&self, interactive_only: bool) -> Result<String, String> {
        let opts = snap_opts(interactive_only);
        let snap = with_session!(self, None, false, |sess| {
            sess.page.snapshot(&opts).await
        })?;
        Ok(snap.render())
    }

    async fn click_opts(
        &self,
        r: &str,
        dialog: Option<&str>,
        prompt_text: Option<&str>,
    ) -> Result<String, String> {
        let dialog_opts = DialogOptions {
            action: DialogAction::from_str_opt(dialog),
            prompt_text: prompt_text.map(str::to_string),
        };
        let acted = with_session!(self, None, |sess| {
            sess.page
                .click_with_origin(r, &dialog_opts, expected_input_origin().as_deref())
                .await
        })?;
        render_action(&acted)
    }

    async fn click(&self, r: &str) -> Result<String, String> {
        self.click_opts(r, None, None).await
    }

    async fn fill(&self, r: &str, text: &str) -> Result<String, String> {
        let acted = with_session!(self, None, |sess| {
            sess.page
                .fill_with_origin(r, text, expected_input_origin().as_deref())
                .await
        })?;
        render_action(&acted)
    }

    async fn input_origin(&self, r: Option<&str>) -> Option<String> {
        let result: Result<Option<String>, String> = async {
            with_session!(self, None, false, |sess| {
                sess.page.input_origin(r).await
            })
        }
        .await;
        result.ok().flatten()
    }

    async fn field_target(&self, r: &str) -> Result<FieldTarget, String> {
        with_session!(self, None, false, |sess| {
            sess.page.field_target(r).await
        })
    }

    async fn focused_target(&self) -> Result<FieldTarget, String> {
        with_session!(self, None, false, |sess| {
            sess.page.focused_target().await
        })
    }

    async fn fill_secret(
        &self,
        r: &str,
        text: &str,
        accept: &(dyn Fn(Option<String>) -> bool + Send + Sync),
    ) -> Result<String, String> {
        let acted = with_session!(self, None, false, |sess| {
            sess.page.fill_secret(r, text, accept).await
        })?;
        render_action(&acted)
    }

    async fn select(&self, r: &str, value: &str) -> Result<String, String> {
        let acted = with_session!(self, None, |sess| {
            sess.page
                .select_option_with_origin(r, value, expected_input_origin().as_deref())
                .await
        })?;
        render_action(&acted)
    }

    async fn press(&self, key: &str) -> Result<String, String> {
        let acted = with_session!(self, None, |sess| {
            sess.page
                .press_key_with_origin(key, expected_input_origin().as_deref())
                .await
        })?;
        render_action(&acted)
    }

    async fn scroll(&self, r: Option<&str>, dx: f64, dy: f64) -> Result<String, String> {
        let acted = with_session!(self, None, |sess| { sess.page.scroll(r, dx, dy).await })?;
        render_action(&acted)
    }

    async fn wait(&self, cond: Wait, timeout: Option<Duration>) -> Result<String, String> {
        let c = match cond {
            Wait::Text(t) => WaitCondition::Text(t),
            Wait::Selector(sel) => WaitCondition::Selector(sel),
            Wait::Idle => WaitCondition::NetworkIdle,
        };
        let acted = with_session!(self, None, false, |sess| {
            sess.page.wait_for(&c, timeout).await
        })?;
        render_action(&acted)
    }

    async fn back_opts(&self, dialog: Option<&str>) -> Result<String, String> {
        let dialog_opts = DialogOptions {
            action: DialogAction::from_str_opt(dialog),
            prompt_text: None,
        };
        let acted = with_session!(self, None, |sess| {
            sess.page.back_with(&dialog_opts).await
        })?;
        render_action(&acted)
    }

    async fn back(&self) -> Result<String, String> {
        self.back_opts(None).await
    }

    async fn tabs(&self, switch_to: Option<usize>) -> Result<String, String> {
        with_session!(self, None, switch_to.is_some(), |sess| {
            let listed: Result<String, browser_cdp::Error> = async {
                let mut pages = sess.browser.pages().await?;
                // A run sees only its own tabs: a subagent just its tab, the
                // top-level run everything except the subagents' tabs.
                if current_lane().is_some() {
                    let own = sess.page.target_id().to_string();
                    pages.retain(|t| t.target_id == own);
                } else {
                    let hidden: Vec<String> = sess
                        .lanes
                        .iter()
                        .map(|(_, p)| p.target_id().to_string())
                        .collect();
                    pages.retain(|t| !hidden.contains(&t.target_id));
                }
                if let Some(n) = switch_to {
                    let t = n
                        .checked_sub(1)
                        .and_then(|i| pages.get(i))
                        .ok_or_else(|| browser_cdp::Error::Json(format!("no tab number {n}")))?;
                    sess.page = sess.browser.attach(&t.target_id).await?;
                    sess.page.bring_to_front().await?;
                }
                let current = sess.page.target_id().to_string();
                Ok(pages
                    .iter()
                    .enumerate()
                    .map(|(i, t)| {
                        format!(
                            "{}{} \"{}\" {}",
                            i + 1,
                            if t.target_id == current { "*" } else { "" },
                            t.title.replace('"', "'"),
                            t.url
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"))
            }
            .await;
            listed
        })
        .map_err(|err| err.strip_prefix("json error: ").unwrap_or(&err).to_string())
    }

    async fn screenshot(
        &self,
        full_page: bool,
        element: Option<&str>,
    ) -> Result<(Vec<u8>, &'static str), String> {
        let target = match (element, full_page) {
            (Some(r), _) => ScreenshotTarget::Element(r.to_string()),
            (None, true) => ScreenshotTarget::FullPage,
            (None, false) => ScreenshotTarget::Viewport,
        };
        let bytes = with_session!(self, None, false, |sess| {
            sess.page
                .screenshot(&ScreenshotOptions {
                    target: target.clone(),
                    jpeg_quality: Some(80),
                })
                .await
        })?;
        Ok((bytes, "jpg"))
    }

    async fn eval(&self, js: &str) -> Result<Value, String> {
        with_session!(self, None, |sess| { sess.page.evaluate(js).await })
    }

    async fn current_url(&self) -> Option<String> {
        // Never starts a browser just to ask.
        let listed: Result<Option<String>, String> = async {
            with_session!(self, None, false, |sess| {
                let current = sess.page.target_id().to_string();
                sess.browser.pages().await.map(|pages| {
                    pages
                        .into_iter()
                        .find(|t| t.target_id == current)
                        .map(|t| t.url)
                })
            })
        }
        .await;
        listed.ok().flatten()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Open,
    Snapshot,
    Click,
    Fill,
    Select,
    Press,
    Scroll,
    Wait,
    Back,
    Tabs,
    Screenshot,
    Eval,
}

struct Shared {
    driver: Arc<dyn BrowserDriver>,
    managed: Option<ManagedBrowsers>,
    data_dir: PathBuf,
    control: Option<Arc<dyn DesktopControlGate>>,
    observed_epoch: Arc<std::sync::atomic::AtomicU64>,
    /// Owner-stored secrets the fill tool may type; `None` refuses `secret`.
    fill_secrets: Option<Arc<FillSecrets>>,
}

/// The tool context supplies the bot identity, never the model's input.
type BotDrivers = Arc<parking_lot::Mutex<std::collections::HashMap<String, Arc<Shared>>>>;

struct ManagedBrowsers {
    settings: BrowserSettings,
    peers: BrowserPeers,
    bots: BotDrivers,
    /// Where each bot's idle browser view target is registered.
    views: Option<Arc<live_view::TargetRegistry>>,
}

impl Shared {
    fn for_bot(self: &Arc<Self>, bot: Option<&str>) -> Result<Arc<Self>, ToolError> {
        let Some(managed) = &self.managed else {
            return Ok(Arc::clone(self));
        };
        let id = bot.ok_or_else(|| exec_err("managed browser requires a bot identity"))?;
        if id.is_empty()
            || !id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(exec_err("invalid bot identity for browser profile"));
        }
        let mut bots = managed.bots.lock();
        Ok(Arc::clone(bots.entry(id.into()).or_insert_with(|| {
            let mut settings = managed.settings.clone();
            let root = settings.profiles_root.join(id);
            settings.profile_dir = Some(root.clone());
            settings.profiles_root = root.join("profiles");
            let mut cdp = CdpDriver::new(settings);
            cdp.peers = Some((id.into(), Arc::clone(&managed.peers)));
            if let Some(views) = &managed.views {
                let pin = Arc::clone(&cdp.takeover_pin);
                // Weak: the registry holds the target, the target must not hold the registry.
                let registry = Arc::downgrade(views);
                let target =
                    browser_cdp::BrowserViewTarget::new_idle(browser_cdp::BrowserViewConfig {
                        bot_id: uuid::Uuid::parse_str(id).ok(),
                        takeover_pin: Some(Arc::new(move |on| {
                            pin.store(on, std::sync::atomic::Ordering::SeqCst);
                        })),
                        on_change: Some(Arc::new(move || {
                            if let Some(registry) = registry.upgrade() {
                                registry.notify_changed();
                            }
                        })),
                        ..browser_cdp::BrowserViewConfig::default()
                    });
                views.register(target.clone());
                cdp.view = Some(target);
            }
            if let Some(gate) = self.control.clone() {
                let bot_id = id.to_owned();
                cdp.set_activity_probe(Arc::new(move || gate.computer_in_use_for_bot(&bot_id)));
            }
            let cdp = Arc::new(cdp);
            managed.peers.lock().insert(id.into(), Arc::downgrade(&cdp));
            Arc::new(Shared {
                driver: cdp,
                managed: None,
                data_dir: self.data_dir.clone(),
                control: self.control.clone(),
                observed_epoch: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                fill_secrets: self.fill_secrets.clone(),
            })
        })))
    }
}

/// Lazily construct a driver per bot, with persistent profiles in `<data>/browsers/<bot-id>`.
/// Explicit `profile: "isolated"` still selects a temporary, disposable profile.
#[must_use]
pub fn managed_tools(
    settings: BrowserSettings,
    data_dir: &Path,
    control: Option<Arc<dyn DesktopControlGate>>,
    views: Option<Arc<live_view::TargetRegistry>>,
) -> Vec<Arc<dyn Tool>> {
    managed_tools_with_secrets(settings, data_dir, control, views, None)
}

/// [`managed_tools`] whose `browser_fill` may type the owner's fill secrets.
#[must_use]
pub fn managed_tools_with_secrets(
    settings: BrowserSettings,
    data_dir: &Path,
    control: Option<Arc<dyn DesktopControlGate>>,
    views: Option<Arc<live_view::TargetRegistry>>,
    fill_secrets: Option<Arc<FillSecrets>>,
) -> Vec<Arc<dyn Tool>> {
    let driver = Arc::new(CdpDriver::new(settings.clone()));
    let peers = BrowserPeers::default();
    let bots = BotDrivers::default();
    if let Some(registry) = &views {
        wire_retirement(registry, &bots, &peers);
    }
    tools_with_pool(
        driver,
        data_dir,
        control,
        Some(ManagedBrowsers {
            settings,
            peers,
            bots,
            views,
        }),
        fill_secrets,
    )
}

/// A deleted bot leaves no driver, browser or selectable screen behind.
fn wire_retirement(registry: &live_view::TargetRegistry, bots: &BotDrivers, peers: &BrowserPeers) {
    let (bots, peers) = (Arc::clone(bots), Arc::clone(peers));
    registry.on_bot_retired(move |bot| retire_bot(&bots, &peers, &bot.to_string()));
}

/// Forget a deleted bot's driver and close its browser (in the background).
fn retire_bot(bots: &BotDrivers, peers: &BrowserPeers, id: &str) {
    let removed = bots.lock().remove(id);
    peers.lock().remove(id);
    if let Some(shared) = removed {
        let driver = Arc::clone(&shared.driver);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move { driver.close_all().await });
        }
    }
}

/// One browser tool.
pub struct BrowserTool {
    spec: ToolSpec,
    op: Op,
    shared: Arc<Shared>,
}

/// Build the browser tools over `driver`.
#[must_use]
pub fn tools(
    driver: Arc<dyn BrowserDriver>,
    data_dir: &Path,
    control: Option<Arc<dyn DesktopControlGate>>,
) -> Vec<Arc<dyn Tool>> {
    tools_with_pool(driver, data_dir, control, None, None)
}

/// [`tools`] whose `browser_fill` may type the owner's fill secrets.
#[must_use]
pub fn tools_with_secrets(
    driver: Arc<dyn BrowserDriver>,
    data_dir: &Path,
    control: Option<Arc<dyn DesktopControlGate>>,
    fill_secrets: Option<Arc<FillSecrets>>,
) -> Vec<Arc<dyn Tool>> {
    tools_with_pool(driver, data_dir, control, None, fill_secrets)
}

fn tools_with_pool(
    driver: Arc<dyn BrowserDriver>,
    data_dir: &Path,
    control: Option<Arc<dyn DesktopControlGate>>,
    managed: Option<ManagedBrowsers>,
    fill_secrets: Option<Arc<FillSecrets>>,
) -> Vec<Arc<dyn Tool>> {
    let shared = Arc::new(Shared {
        driver,
        managed,
        data_dir: data_dir.to_path_buf(),
        control,
        observed_epoch: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        fill_secrets,
    });
    let b = PermissionCategory::Browser;
    let ref_prop =
        json!({"type":"string","description":"Element ref from browser_snapshot, like e12."});
    let defs: Vec<(Op, ToolSpec)> = vec![
        (
            Op::Open,
            spec(
                "browser_open",
                "Open a URL in the assistant's own managed browser and return the page snapshot. Use this only when a real page is needed (logging in, clicking, forms, pages that need scripts, or the user wants to watch); to read or look something up use web_search and web_extract first, they are faster and need no browser. The result ends with a [view: ...] line saying whether the user can see the page. This is not the user's browser; to see what the user has open, use desktop_list_apps. Example: {\"url\":\"https://example.com\"}",
                json!({"type":"object","properties":{
                    "url":{"type":"string","description":"Full URL."},
                    "profile":{"type":"string","description":"Profile name, or 'isolated' for a disposable profile. Omit to use the configured per-bot default. Only honoured when the browser starts."},
                    "dialog":{"type":"string","enum":["accept","dismiss"],"description":"How to handle any dialog (e.g. beforeunload) during navigation. Default is dismiss (alerts are still accepted)."},
                    "prompt_text":{"type":"string","description":"Optional text to enter when accepting a prompt dialog."}
                },"required":["url"]}),
                Effect::Write,
                b,
            ),
        ),
        (
            Op::Snapshot,
            spec(
                "browser_snapshot",
                "Read the managed browser's page as lines like [e12] button \"Sign in\". about:blank or no page here says nothing about the user's own browser or screen (use desktop_list_apps for that). Use the refs in other browser tools. Example: {\"interactive_only\":true}",
                json!({"type":"object","properties":{
                    "interactive_only":{"type":"boolean","description":"Only list clickable/typable elements."}
                }}),
                Effect::Read,
                b,
            ),
        ),
        (
            Op::Click,
            spec(
                "browser_click",
                "Click an element. Returns what changed, so there is no need to snapshot afterwards. Example: {\"ref\":\"e12\"}",
                json!({"type":"object","properties":{
                    "ref":ref_prop,
                    "dialog":{"type":"string","enum":["accept","dismiss"],"description":"How to handle any dialog (e.g. confirm, prompt, beforeunload) during click. Default is dismiss (alerts are still accepted)."},
                    "prompt_text":{"type":"string","description":"Optional text to enter when accepting a prompt dialog."}
                },"required":["ref"]}),
                Effect::Write,
                b,
            ),
        ),
        (
            Op::Fill,
            spec(
                "browser_fill",
                "Replace the text of an input. Give exactly one of `text` (what you type) or `secret` (the name of a secret the owner stored, from secrets_list: it is typed for you and you never see the value). Example: {\"ref\":\"e5\",\"text\":\"hello\"} or {\"ref\":\"e5\",\"secret\":\"login/github\"}",
                json!({"type":"object","properties":{
                    "ref":ref_prop,
                    "text":{"type":"string","description":"New text. Not with `secret`."},
                    "secret":{"type":"string","description":"Name of an owner-stored secret to type instead of `text`."}
                },"required":["ref"]}),
                Effect::Write,
                b,
            ),
        ),
        (
            Op::Select,
            spec(
                "browser_select",
                "Choose an option in a dropdown. Example: {\"ref\":\"e7\",\"value\":\"blue\"}",
                json!({"type":"object","properties":{
                    "ref":ref_prop,
                    "value":{"type":"string","description":"Option value or label."}
                },"required":["ref","value"]}),
                Effect::Write,
                b,
            ),
        ),
        (
            Op::Press,
            spec(
                "browser_press",
                "Press a key or chord. Example: {\"key\":\"Enter\"}",
                json!({"type":"object","properties":{
                    "key":{"type":"string","description":"Key such as Enter, Tab, Escape, Control+a."}
                },"required":["key"]}),
                Effect::Write,
                b,
            ),
        ),
        (
            Op::Scroll,
            spec(
                "browser_scroll",
                "Scroll the page (positive dy = down) or an element. Example: {\"dy\":600}",
                json!({"type":"object","properties":{
                    "ref":ref_prop,
                    "dx":{"type":"number"},
                    "dy":{"type":"number"}
                }}),
                Effect::Read,
                b,
            ),
        ),
        (
            Op::Wait,
            spec(
                "browser_wait",
                "Wait for text, a CSS selector, or network idle. Give exactly one. Example: {\"text\":\"Welcome\"}",
                json!({"type":"object","properties":{
                    "text":{"type":"string"},
                    "selector":{"type":"string"},
                    "idle":{"type":"boolean"},
                    "timeout_secs":{"type":"integer","minimum":1,"maximum":120}
                }}),
                Effect::Read,
                b,
            ),
        ),
        (
            Op::Back,
            spec(
                "browser_back",
                "Go back one page. Example: {}",
                json!({"type":"object","properties":{
                    "dialog":{"type":"string","enum":["accept","dismiss"],"description":"How to handle any dialog (e.g. beforeunload) during back navigation. Default is dismiss."}
                }}),
                Effect::Write,
                b,
            ),
        ),
        (
            Op::Tabs,
            spec(
                "browser_tabs",
                "List open tabs of the bot's managed browser (current is marked *), or switch with switch_to. Not the user's tabs: empty or about:blank here says nothing about their browser. Example: {\"switch_to\":2}",
                json!({"type":"object","properties":{
                    "switch_to":{"type":"integer","minimum":1,"description":"Tab number from the list."}
                }}),
                Effect::Write,
                b,
            ),
        ),
        (
            Op::Screenshot,
            spec(
                "browser_screenshot",
                "Save a screenshot and return the file path. Example: {\"full_page\":true}",
                json!({"type":"object","properties":{
                    "full_page":{"type":"boolean"},
                    "ref":ref_prop
                }}),
                Effect::Read,
                b,
            ),
        ),
        (
            Op::Eval,
            spec(
                "browser_eval",
                "Run JavaScript in the page and return the JSON result. Last resort, not for reading a page (use browser_snapshot or web_extract). Example: {\"js\":\"document.title\"}",
                json!({"type":"object","properties":{
                    "js":{"type":"string","description":"A JavaScript expression."}
                },"required":["js"]}),
                Effect::Write,
                PermissionCategory::Script,
            ),
        ),
    ];
    defs.into_iter()
        .map(|(op, spec)| {
            Arc::new(BrowserTool {
                spec,
                op,
                shared: shared.clone(),
            }) as Arc<dyn Tool>
        })
        .collect()
}

#[derive(Deserialize)]
struct OpenIn {
    url: String,
    profile: Option<String>,
    dialog: Option<String>,
    prompt_text: Option<String>,
}

#[derive(Deserialize)]
struct SnapIn {
    #[serde(default)]
    interactive_only: bool,
}

#[derive(Deserialize)]
struct RefIn {
    r#ref: String,
    dialog: Option<String>,
    prompt_text: Option<String>,
}

#[derive(Deserialize)]
struct BackIn {
    dialog: Option<String>,
}

#[derive(Deserialize)]
struct FillIn {
    r#ref: String,
}

#[derive(Deserialize)]
struct SelectIn {
    r#ref: String,
    value: String,
}

#[derive(Deserialize)]
struct PressIn {
    key: String,
}

#[derive(Deserialize)]
struct ScrollIn {
    r#ref: Option<String>,
    #[serde(default)]
    dx: f64,
    #[serde(default)]
    dy: f64,
}

#[derive(Deserialize)]
struct WaitIn {
    text: Option<String>,
    selector: Option<String>,
    #[serde(default)]
    idle: bool,
    timeout_secs: Option<u64>,
}

#[derive(Deserialize)]
struct TabsIn {
    switch_to: Option<usize>,
}

#[derive(Deserialize)]
struct ShotIn {
    #[serde(default)]
    full_page: bool,
    r#ref: Option<String>,
}

#[derive(Deserialize)]
struct EvalIn {
    js: String,
}

/// Only `http`, `https` and `about:blank` may be opened. `file:`, `javascript:`,
/// `data:`, `chrome:` and friends would bypass the Fs and Script permission
/// categories (and `view-source:`/`blob:` can wrap them).
fn check_open_url(url: &str) -> Result<(), ToolError> {
    // Browsers drop ASCII tab/newline anywhere in a URL and trim control
    // characters and spaces at the ends, so a `java<newline>script:` URL is still a scheme.
    let cleaned: String = url
        .trim_matches(|c: char| c.is_ascii_control() || c == ' ')
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .collect();
    let lower = cleaned.to_ascii_lowercase();
    let ok = lower == "about:blank"
        || ["http://", "https://"]
            .iter()
            .any(|p| lower.starts_with(p) && lower.len() > p.len());
    if ok {
        Ok(())
    } else {
        Err(ToolError::InvalidInput(
            "browser_open only accepts full http:// or https:// URLs (or about:blank)".into(),
        ))
    }
}

/// Type the owner's fill secret `name` into field `r`.
///
/// The live frame origin of the receiving field is resolved and compared with
/// the secret's allow-list FIRST; a field that is not on the list is refused
/// before the value is read. The value is read from the store only now, kept in
/// memory, handed to the driver (which re-checks the frame origin immediately
/// before inserting) and wiped on drop. Nothing returned contains it.
///
/// # Errors
/// The field cannot be located, its frame origin is not on the secret's
/// allow-list, the secret has no value, or the browser refuses; the message
/// never contains the value.
pub async fn fill_with_secret(
    driver: &dyn BrowserDriver,
    secrets: &FillSecrets,
    r: &str,
    name: &str,
) -> Result<String, String> {
    let target = driver.field_target(r).await?;
    secrets
        .check_browser_in(
            name,
            target.frame_origin.as_deref(),
            target.top_origin.as_deref(),
        )
        .map_err(|e| e.to_string())?;
    let value = secrets.resolve_value(name).map_err(|e| e.to_string())?;
    let top = target.top_origin.clone();
    let accept = |origin: Option<String>| {
        secrets
            .check_browser_in(name, origin.as_deref(), top.as_deref())
            .is_ok()
    };
    let result = driver.fill_secret(r, value.expose(), &accept).await;
    let shown = match result {
        Ok(text) => Ok(scrub(&text, &value)),
        Err(e) => Err(scrub(&e, &value)),
    };
    drop(value);
    shown
}

impl BrowserTool {
    fn fill_secrets(&self) -> Result<Arc<FillSecrets>, ToolError> {
        self.shared
            .fill_secrets
            .clone()
            .ok_or_else(|| exec_err("no secrets are configured on this daemon"))
    }

    /// The per-bot (and per-run mode) shared state a call runs on. The same
    /// selection serves the call and its approval checks, so what is checked
    /// is what will receive the text.
    fn select_shared(&self, ctx: &ToolContext) -> Result<Arc<Shared>, ToolError> {
        // A bot whose own computer is a paired server browses THERE: the driver
        // is a link to that server's browser, never this machine's browser and
        // never a profile pool of this daemon. No owner secrets (`fill_secrets`)
        // go over the link.
        if let Some(driver) = self
            .shared
            .control
            .as_ref()
            .and_then(|gate| gate.remote_browser_for_run(&ctx.run_id))
        {
            return Ok(Arc::new(Shared {
                driver,
                managed: None,
                data_dir: self.shared.data_dir.clone(),
                control: self.shared.control.clone(),
                observed_epoch: self.shared.observed_epoch.clone(),
                fill_secrets: None,
            }));
        }
        let mut shared = self.shared.for_bot(ctx.bot_id.as_deref())?;
        if let Some(headless) = self
            .shared
            .control
            .as_ref()
            .and_then(|gate| gate.browser_headless_for_run(&ctx.run_id))
            && let Some(driver) = shared.driver.for_headless(headless)
        {
            // The alternate driver is already bot-scoped. Wrapping it in
            // `managed` would attach the other mode's profile pool.
            shared = Arc::new(Shared {
                driver,
                managed: None,
                data_dir: shared.data_dir.clone(),
                control: shared.control.clone(),
                observed_epoch: shared.observed_epoch.clone(),
                fill_secrets: shared.fill_secrets.clone(),
            });
        }
        Ok(shared)
    }

    /// Facts about the live receiving field of a `secret` fill, or the refusal.
    /// Returns the secret's name, the field and the standing grant that
    /// matches it exactly, if any.
    async fn secret_fill_facts(
        &self,
        ctx: &ToolContext,
        input: &Value,
    ) -> Result<Option<(String, FieldTarget, Option<String>)>, ToolError> {
        if self.op != Op::Fill {
            return Ok(None);
        }
        let Source::Secret(name) = source(input, "text")? else {
            return Ok(None);
        };
        let i: FillIn = parse(input.clone())?;
        let secrets = self.fill_secrets()?;
        let shared = self.select_shared(ctx)?;
        let target = shared
            .driver
            .field_target(&i.r#ref)
            .await
            .map_err(exec_err)?;
        // Refuse here, BEFORE any approval is offered, when the live frame is
        // not on this secret's allow-list.
        let grant = secrets
            .check_browser_in(
                &name,
                target.frame_origin.as_deref(),
                target.top_origin.as_deref(),
            )
            .map_err(|e| refused(&e))?;
        Ok(Some((name, target, grant)))
    }
}

impl BrowserTool {
    /// Ask the control gate before a mutating action, when the browser is a
    /// visible window on the user's desktop.
    async fn gate(
        &self,
        run_id: &str,
        action: &str,
        label: String,
    ) -> Result<ActionPermit, ToolError> {
        let Some(gate) = self.shared.control.as_ref() else {
            return Ok(ActionPermit::default());
        };
        let request = ControlRequest {
            run_id: run_id.to_owned(),
            rect: None,
            label,
            action: action.to_owned(),
        };
        crate::control::admit_or_inherit(gate.as_ref(), || async {
            if self.shared.driver.on_desktop() {
                gate.admit_action(&request).await
            } else {
                gate.admit_background_action(&request).await
            }
        })
        .await
        .map_err(exec_err)
    }

    /// A fresh observation is never taken while a pause, a human takeover or a
    /// local override is in force: the model must not read the page then.
    fn refuse_observation_while_latched(&self) -> Result<(), ToolError> {
        if let Some(gate) = &self.shared.control
            && let Some(message) = gate.latch_refusal().or_else(|| gate.observation_refused())
        {
            return Err(exec_err(message));
        }
        Ok(())
    }

    /// Observation tools discard their result if control changed while they
    /// ran (a screenshot, a wait or a tab list that straddles a takeover).
    async fn run(&self, run_id: &str, input: Value) -> Result<String, ToolError> {
        let observing = match self.op {
            Op::Snapshot | Op::Wait | Op::Screenshot => true,
            Op::Tabs => input.get("switch_to").is_none_or(Value::is_null),
            _ => false,
        };
        let gate = self.shared.control.clone();
        let epoch = gate.as_ref().map(|g| g.observation_epoch());
        let text = self.run_inner(run_id, input).await?;
        if let (Some(gate), Some(epoch)) = (gate, epoch) {
            if observing
                && (gate.observation_epoch() != epoch || gate.observation_refused().is_some())
            {
                return Err(exec_err(crate::control::OBSERVATION_DISCARDED));
            }
            // An action's own result carries a fresh observation (the refresh
            // after it, or the re-observation that replaced it): withheld if
            // the human drives by now. The epoch is not compared here: an
            // action legitimately waits through a pause and its release.
            if !observing && gate.observation_refused().is_some() {
                return Ok(crate::control::OBSERVATION_WITHHELD.to_owned());
            }
        }
        Ok(text)
    }

    async fn run_inner(&self, run_id: &str, input: Value) -> Result<String, ToolError> {
        let observing = match self.op {
            Op::Snapshot | Op::Wait | Op::Screenshot => true,
            Op::Tabs => input.get("switch_to").is_none_or(Value::is_null),
            _ => false,
        };
        if observing
            && let Some(message) = self
                .shared
                .control
                .as_ref()
                .and_then(|g| g.observation_refused())
        {
            return Err(exec_err(message));
        }
        let d = Arc::clone(&self.shared.driver);
        let text = match self.op {
            Op::Open => {
                let i: OpenIn = parse(input)?;
                check_open_url(&i.url)?;
                let permit = self.gate(run_id, "open", i.url.clone()).await?;
                if permit.needs_observation()
                    || self
                        .shared
                        .observed_epoch
                        .load(std::sync::atomic::Ordering::SeqCst)
                        != self
                            .shared
                            .control
                            .as_ref()
                            .map_or(0, |g| g.observation_epoch())
                {
                    self.shared.observed_epoch.store(
                        self.shared
                            .control
                            .as_ref()
                            .map_or(0, |g| g.observation_epoch()),
                        std::sync::atomic::Ordering::SeqCst,
                    );
                    self.refuse_observation_while_latched()?;
                    let fresh = d.snapshot(false).await.map_err(exec_err)?;
                    permit.observed();
                    return Ok(format!(
                        "Human intervention completed; previous action discarded. Fresh observation:\n{fresh}"
                    ));
                }
                permit
                    .protect(async move {
                        d.open_opts(
                            &i.url,
                            i.profile.as_deref(),
                            i.dialog.as_deref(),
                            i.prompt_text.as_deref(),
                        )
                        .await
                    })
                    .await
                    .map_err(exec_err)?
            }
            Op::Snapshot => {
                let i: SnapIn = parse(input)?;
                let epoch = self
                    .shared
                    .control
                    .as_ref()
                    .map_or(0, |gate| gate.observation_epoch());
                let snapshot = d.snapshot(i.interactive_only).await;
                if self
                    .shared
                    .control
                    .as_ref()
                    .map_or(0, |gate| gate.observation_epoch())
                    != epoch
                {
                    return Err(exec_err(
                        "human intervention changed the page during observation; obtain a fresh observation",
                    ));
                }
                if snapshot.is_ok() {
                    self.shared
                        .observed_epoch
                        .store(epoch, std::sync::atomic::Ordering::SeqCst);
                }
                snapshot
            }
            Op::Click => {
                let i: RefIn = parse(input)?;
                let permit = self.gate(run_id, "click", i.r#ref.clone()).await?;
                if permit.needs_observation()
                    || self
                        .shared
                        .observed_epoch
                        .load(std::sync::atomic::Ordering::SeqCst)
                        != self
                            .shared
                            .control
                            .as_ref()
                            .map_or(0, |g| g.observation_epoch())
                {
                    self.shared.observed_epoch.store(
                        self.shared
                            .control
                            .as_ref()
                            .map_or(0, |g| g.observation_epoch()),
                        std::sync::atomic::Ordering::SeqCst,
                    );
                    self.refuse_observation_while_latched()?;
                    let fresh = d.snapshot(false).await.map_err(exec_err)?;
                    permit.observed();
                    return Ok(format!(
                        "Human intervention completed; previous action discarded. Fresh observation:\n{fresh}"
                    ));
                }
                permit
                    .protect(async move {
                        d.click_opts(&i.r#ref, i.dialog.as_deref(), i.prompt_text.as_deref())
                            .await
                    })
                    .await
                    .map_err(exec_err)?
            }
            Op::Fill => {
                let what = source(&input, "text")?;
                let i: FillIn = parse(input)?;
                let permit = self.gate(run_id, "type", i.r#ref.clone()).await?;
                if permit.needs_observation()
                    || self
                        .shared
                        .observed_epoch
                        .load(std::sync::atomic::Ordering::SeqCst)
                        != self
                            .shared
                            .control
                            .as_ref()
                            .map_or(0, |g| g.observation_epoch())
                {
                    self.shared.observed_epoch.store(
                        self.shared
                            .control
                            .as_ref()
                            .map_or(0, |g| g.observation_epoch()),
                        std::sync::atomic::Ordering::SeqCst,
                    );
                    self.refuse_observation_while_latched()?;
                    let fresh = d.snapshot(false).await.map_err(exec_err)?;
                    permit.observed();
                    return Ok(format!(
                        "Human intervention completed; previous action discarded. Fresh observation:\n{fresh}"
                    ));
                }
                match what {
                    Source::Text(text) => permit
                        .protect(async move { d.fill(&i.r#ref, &text).await })
                        .await
                        .map_err(exec_err)?,
                    Source::Secret(name) => {
                        let secrets = self.fill_secrets()?;
                        permit
                            .protect(async move {
                                fill_with_secret(d.as_ref(), &secrets, &i.r#ref, &name).await
                            })
                            .await
                            .map_err(exec_err)?
                    }
                }
            }
            Op::Select => {
                let i: SelectIn = parse(input)?;
                let permit = self.gate(run_id, "click", i.r#ref.clone()).await?;
                if permit.needs_observation()
                    || self
                        .shared
                        .observed_epoch
                        .load(std::sync::atomic::Ordering::SeqCst)
                        != self
                            .shared
                            .control
                            .as_ref()
                            .map_or(0, |g| g.observation_epoch())
                {
                    self.shared.observed_epoch.store(
                        self.shared
                            .control
                            .as_ref()
                            .map_or(0, |g| g.observation_epoch()),
                        std::sync::atomic::Ordering::SeqCst,
                    );
                    self.refuse_observation_while_latched()?;
                    let fresh = d.snapshot(false).await.map_err(exec_err)?;
                    permit.observed();
                    return Ok(format!(
                        "Human intervention completed; previous action discarded. Fresh observation:\n{fresh}"
                    ));
                }
                permit
                    .protect(async move { d.select(&i.r#ref, &i.value).await })
                    .await
                    .map_err(exec_err)?
            }
            Op::Press => {
                let i: PressIn = parse(input)?;
                let permit = self.gate(run_id, "key", i.key.clone()).await?;
                if permit.needs_observation()
                    || self
                        .shared
                        .observed_epoch
                        .load(std::sync::atomic::Ordering::SeqCst)
                        != self
                            .shared
                            .control
                            .as_ref()
                            .map_or(0, |g| g.observation_epoch())
                {
                    self.shared.observed_epoch.store(
                        self.shared
                            .control
                            .as_ref()
                            .map_or(0, |g| g.observation_epoch()),
                        std::sync::atomic::Ordering::SeqCst,
                    );
                    self.refuse_observation_while_latched()?;
                    let fresh = d.snapshot(false).await.map_err(exec_err)?;
                    permit.observed();
                    return Ok(format!(
                        "Human intervention completed; previous action discarded. Fresh observation:\n{fresh}"
                    ));
                }
                permit
                    .protect(async move { d.press(&i.key).await })
                    .await
                    .map_err(exec_err)?
            }
            Op::Scroll => {
                let i: ScrollIn = parse(input)?;
                let permit = self
                    .gate(run_id, "scroll", i.r#ref.clone().unwrap_or_default())
                    .await?;
                if permit.needs_observation()
                    || self
                        .shared
                        .observed_epoch
                        .load(std::sync::atomic::Ordering::SeqCst)
                        != self
                            .shared
                            .control
                            .as_ref()
                            .map_or(0, |g| g.observation_epoch())
                {
                    self.shared.observed_epoch.store(
                        self.shared
                            .control
                            .as_ref()
                            .map_or(0, |g| g.observation_epoch()),
                        std::sync::atomic::Ordering::SeqCst,
                    );
                    self.refuse_observation_while_latched()?;
                    let fresh = d.snapshot(false).await.map_err(exec_err)?;
                    permit.observed();
                    return Ok(format!(
                        "Human intervention completed; previous action discarded. Fresh observation:\n{fresh}"
                    ));
                }
                permit
                    .protect(async move { d.scroll(i.r#ref.as_deref(), i.dx, i.dy).await })
                    .await
                    .map_err(exec_err)?
            }
            Op::Wait => {
                let i: WaitIn = parse(input)?;
                let given = usize::from(i.text.is_some())
                    + usize::from(i.selector.is_some())
                    + usize::from(i.idle);
                if given != 1 {
                    return Err(ToolError::InvalidInput(
                        "give exactly one of text, selector, idle".into(),
                    ));
                }
                let cond = if let Some(t) = i.text {
                    Wait::Text(t)
                } else if let Some(sel) = i.selector {
                    Wait::Selector(sel)
                } else {
                    Wait::Idle
                };
                d.wait(cond, i.timeout_secs.map(Duration::from_secs)).await
            }
            Op::Back => {
                let i: BackIn = parse(input)?;
                let permit = self.gate(run_id, "key", "back".to_owned()).await?;
                if permit.needs_observation()
                    || self
                        .shared
                        .observed_epoch
                        .load(std::sync::atomic::Ordering::SeqCst)
                        != self
                            .shared
                            .control
                            .as_ref()
                            .map_or(0, |g| g.observation_epoch())
                {
                    self.shared.observed_epoch.store(
                        self.shared
                            .control
                            .as_ref()
                            .map_or(0, |g| g.observation_epoch()),
                        std::sync::atomic::Ordering::SeqCst,
                    );
                    self.refuse_observation_while_latched()?;
                    let fresh = d.snapshot(false).await.map_err(exec_err)?;
                    permit.observed();
                    return Ok(format!(
                        "Human intervention completed; previous action discarded. Fresh observation:\n{fresh}"
                    ));
                }
                permit
                    .protect(async move { d.back_opts(i.dialog.as_deref()).await })
                    .await
                    .map_err(exec_err)?
            }
            Op::Tabs => {
                let i: TabsIn = parse(input)?;
                // The tool is `Effect::Write` so a default Ask policy cannot
                // auto-approve tab activation via `read_tools_skip_ask`.
                // Listing shares that spec. The control gate still runs only
                // for activation: a bare list does not take over the desktop.
                if let Some(n) = i.switch_to {
                    let permit = self.gate(run_id, "tab", format!("tab {n}")).await?;
                    if permit.needs_observation()
                        || self
                            .shared
                            .observed_epoch
                            .load(std::sync::atomic::Ordering::SeqCst)
                            != self
                                .shared
                                .control
                                .as_ref()
                                .map_or(0, |g| g.observation_epoch())
                    {
                        self.shared.observed_epoch.store(
                            self.shared
                                .control
                                .as_ref()
                                .map_or(0, |g| g.observation_epoch()),
                            std::sync::atomic::Ordering::SeqCst,
                        );
                        self.refuse_observation_while_latched()?;
                        let fresh = d.snapshot(false).await.map_err(exec_err)?;
                        permit.observed();
                        return Ok(format!(
                            "Human intervention completed; previous action discarded. Fresh observation:\n{fresh}"
                        ));
                    }
                    permit
                        .protect(async move { d.tabs(i.switch_to).await })
                        .await
                        .map_err(exec_err)?
                } else {
                    d.tabs(None).await
                }
            }
            Op::Screenshot => {
                let i: ShotIn = parse(input)?;
                let (bytes, ext) = d
                    .screenshot(i.full_page, i.r#ref.as_deref())
                    .await
                    .map_err(exec_err)?;
                let path = save_file(&self.shared.data_dir.join("screenshots"), ext, &bytes)?;
                Ok(format!("screenshot: {path}"))
            }
            Op::Eval => {
                let i: EvalIn = parse(input)?;
                let label: String = i.js.trim().chars().take(80).collect();
                let permit = self.gate(run_id, "eval", label).await?;
                if permit.needs_observation()
                    || self
                        .shared
                        .observed_epoch
                        .load(std::sync::atomic::Ordering::SeqCst)
                        != self
                            .shared
                            .control
                            .as_ref()
                            .map_or(0, |g| g.observation_epoch())
                {
                    self.shared.observed_epoch.store(
                        self.shared
                            .control
                            .as_ref()
                            .map_or(0, |g| g.observation_epoch()),
                        std::sync::atomic::Ordering::SeqCst,
                    );
                    self.refuse_observation_while_latched()?;
                    let fresh = d.snapshot(false).await.map_err(exec_err)?;
                    permit.observed();
                    return Ok(format!(
                        "Human intervention completed; previous action discarded. Fresh observation:\n{fresh}"
                    ));
                }
                permit
                    .protect(async move { d.eval(&i.js).await })
                    .await
                    .map_err(exec_err)?
                    .map(|v| v.to_string())
            }
        };
        text.map_err(exec_err)
    }
}

impl BrowserTool {
    async fn current_origin_in_lane(&self, ctx: &ToolContext, input: &Value) -> Option<String> {
        // Only the actions that act on the page that is open now (not open/eval/tabs/back).
        if !matches!(self.op, Op::Click | Op::Fill | Op::Select | Op::Press) {
            return None;
        }
        let shared = self.select_shared(ctx).ok()?;
        shared
            .driver
            .input_origin(if self.op == Op::Press {
                None
            } else {
                input.get("ref")?.as_str()
            })
            .await
            .and_then(|o| origin_of(&o))
    }

    async fn standing_grant_in_lane(
        &self,
        ctx: &ToolContext,
        input: &Value,
    ) -> Option<StandingGrant> {
        let (name, target, grant) = self.secret_fill_facts(ctx, input).await.ok().flatten()?;
        let id = grant?;
        let origin = target.frame_origin?;
        Some(StandingGrant {
            id,
            label: format!(
                "secret {} on {}",
                sanitize_label(&name, 80),
                sanitize_label(&origin, 80)
            ),
        })
    }

    async fn approval_details_in_lane(
        &self,
        ctx: &ToolContext,
        input: &Value,
    ) -> Result<Option<Value>, ToolError> {
        let Some((name, target, grant)) = self.secret_fill_facts(ctx, input).await? else {
            return Ok(None);
        };
        let frame = target.frame_origin.clone().unwrap_or_default();
        let top = target.top_origin.clone();
        let differs = top.as_deref() != Some(frame.as_str());
        Ok(Some(json!({
            "kind": "secret_fill",
            "tool": "browser_fill",
            "secret": sanitize_label(&name, 80),
            "field": sanitize_label(&target.name, 80),
            "field_role": sanitize_label(&target.role, 40),
            "frame_origin": frame,
            "top_origin": top,
            "frame_differs_from_top": differs,
            "has_standing_grant": grant.is_some(),
            "note": if differs {
                "The field is inside an embedded frame whose origin differs from the page's: the secret goes to the frame origin shown."
            } else {
                "The secret value is typed by Praxi; the assistant never sees it."
            },
        })))
    }

    async fn call_in_lane(&self, ctx: &ToolContext, input: Value) -> Result<ToolOutput, ToolError> {
        if self
            .shared
            .control
            .as_ref()
            .is_some_and(|g| !g.browser_available_for_run(&ctx.run_id))
        {
            return Err(exec_err(
                "browser is unavailable for this run's computer target",
            ));
        }
        let shared = self.select_shared(ctx)?;
        // Recheck the receiving frame after approval. The CDP driver repeats this
        // check with the session lock held immediately before sending input.
        if let Some(expected) = &ctx.expected_origin
            && matches!(self.op, Op::Click | Op::Fill | Op::Select | Op::Press)
        {
            let now = self.current_origin(ctx, &input).await;
            if now.as_deref() != Some(expected.as_str()) {
                return Err(exec_err(format!(
                    "the receiving frame is no longer on {expected} (it is on {}): look at the page again, then repeat the action so it can be approved for the new site",
                    now.as_deref().unwrap_or("an unknown page")
                )));
            }
        }
        let selected = BrowserTool {
            spec: self.spec.clone(),
            op: self.op,
            shared,
        };
        let text = EXPECTED_PAGE_ORIGIN
            .scope(
                ctx.expected_origin.clone(),
                selected.run(&ctx.run_id, input),
            )
            .await?;
        let view = selected.shared.driver.live_view_attached();
        let mut out = finish_text(ctx, &self.shared.data_dir.join("spill"), text);
        // Honest "seen by user" signal, after any truncation so it is never cut.
        if let Some(attached) = view
            && let Some(tool_registry::ContentPart::Text { text }) = out.content.first_mut()
        {
            text.push('\n');
            text.push_str(view_line(attached));
        }
        Ok(out)
    }
}

#[async_trait]
impl Tool for BrowserTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn egress_extra(&self, _ctx: &ToolContext, input: &Value) -> Option<String> {
        if !matches!(self.op, Op::Open) {
            return None;
        }
        let named = input
            .get("profile")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty());
        let line = match named {
            Some("isolated") => "profile: isolated (temporary, not logged in)".to_owned(),
            Some(name) => format!("profile: {name} (named profile, may be logged in)"),
            None if self.shared.managed.is_some() => {
                "profile: default (persistent per-bot profile, may be logged in)".to_owned()
            }
            None => "profile: isolated (temporary, not logged in)".to_owned(),
        };
        Some(line)
    }

    /// Typing an owner-stored secret is confirmed on every call, however
    /// permissive the policy, preset, grants or read shortcuts are.
    fn always_ask_for(&self, input: &Value) -> bool {
        self.op == Op::Fill && names_a_secret(input)
    }

    async fn current_origin(&self, ctx: &ToolContext, input: &Value) -> Option<String> {
        BROWSER_LANE
            .scope(ctx.lane.clone(), self.current_origin_in_lane(ctx, input))
            .await
    }

    async fn standing_grant(&self, ctx: &ToolContext, input: &Value) -> Option<StandingGrant> {
        BROWSER_LANE
            .scope(ctx.lane.clone(), self.standing_grant_in_lane(ctx, input))
            .await
    }

    async fn approval_details(
        &self,
        ctx: &ToolContext,
        input: &Value,
    ) -> Result<Option<Value>, ToolError> {
        BROWSER_LANE
            .scope(ctx.lane.clone(), self.approval_details_in_lane(ctx, input))
            .await
    }

    async fn call(&self, ctx: &ToolContext, input: Value) -> Result<ToolOutput, ToolError> {
        BROWSER_LANE
            .scope(ctx.lane.clone(), self.call_in_lane(ctx, input))
            .await
    }
}

#[cfg(test)]
pub(crate) mod fake {
    use super::*;
    use parking_lot::Mutex as PMutex;

    /// A callback a test runs inside a backend call.
    pub type FakeHook = Arc<dyn Fn(&str) + Send + Sync>;

    /// Records calls as `name:args` strings and returns canned text.
    #[derive(Default)]
    pub struct FakeBrowser {
        pub calls: PMutex<Vec<String>>,
        /// When set, mutating actions go through the desktop control gate.
        pub on_desktop: bool,
        /// The URL `current_url` reports (`None`: unknown).
        pub url: PMutex<Option<String>>,
        /// Runs inside every backend call (a human takeover landing mid-call).
        pub hook: PMutex<Option<FakeHook>>,
        /// Where each ref lives (for `field_target`); an unknown ref errors.
        pub targets: PMutex<std::collections::HashMap<String, FieldTarget>>,
        /// The origin the browser reports at insertion time when it differs
        /// from the target's (a navigation after the card was shown).
        #[allow(clippy::option_option)]
        // outer: "override set", inner: the reported origin
        pub origin_at_insert: PMutex<Option<Option<String>>>,
        /// What actually reached the page through `fill_secret`.
        pub typed_secrets: PMutex<Vec<String>>,
        /// The raw (pre-scrub) text `fill_secret` produced.
        pub raw_secret_output: PMutex<String>,
        /// Make the page echo the typed value in the action result.
        pub echo_value: bool,
        /// What `live_view_attached` reports (`None`: cannot tell).
        pub watched: PMutex<Option<bool>>,
    }

    impl FakeBrowser {
        fn rec(&self, s: &str) -> Result<String, String> {
            self.calls.lock().push(s.to_string());
            let hook = self.hook.lock().clone();
            if let Some(hook) = hook {
                hook(s);
            }
            if s.contains("e404") {
                Err("unknown ref e404".into())
            } else {
                Ok(format!("ok {s}"))
            }
        }
    }

    #[async_trait]
    impl BrowserDriver for FakeBrowser {
        fn live_view_attached(&self) -> Option<bool> {
            *self.watched.lock()
        }

        async fn input_origin(&self, r: Option<&str>) -> Option<String> {
            self.targets
                .lock()
                .get(r.unwrap_or("focused"))
                .and_then(|t| t.frame_origin.clone())
        }

        async fn current_url(&self) -> Option<String> {
            self.url.lock().clone()
        }

        fn on_desktop(&self) -> bool {
            self.on_desktop
        }

        async fn open_opts(
            &self,
            url: &str,
            profile: Option<&str>,
            dialog: Option<&str>,
            prompt_text: Option<&str>,
        ) -> Result<String, String> {
            let mut s = format!("open:{url}:{}", profile.unwrap_or("-"));
            if let Some(d) = dialog {
                let _ = write!(s, ":{d}");
            }
            if let Some(p) = prompt_text {
                let _ = write!(s, ":{p}");
            }
            self.rec(&s)
        }
        async fn open(&self, url: &str, profile: Option<&str>) -> Result<String, String> {
            self.open_opts(url, profile, None, None).await
        }
        async fn snapshot(&self, i: bool) -> Result<String, String> {
            self.rec(&format!("snapshot:{i}"))
        }
        async fn click_opts(
            &self,
            r: &str,
            dialog: Option<&str>,
            prompt_text: Option<&str>,
        ) -> Result<String, String> {
            let mut s = format!("click:{r}");
            if let Some(d) = dialog {
                let _ = write!(s, ":{d}");
            }
            if let Some(p) = prompt_text {
                let _ = write!(s, ":{p}");
            }
            self.rec(&s)
        }
        async fn click(&self, r: &str) -> Result<String, String> {
            self.click_opts(r, None, None).await
        }
        async fn fill(&self, r: &str, t: &str) -> Result<String, String> {
            self.rec(&format!("fill:{r}:{t}"))
        }
        async fn field_target(&self, r: &str) -> Result<FieldTarget, String> {
            self.rec(&format!("field_target:{r}"))?;
            self.targets
                .lock()
                .get(r)
                .cloned()
                .ok_or_else(|| format!("unknown ref {r}"))
        }
        async fn fill_secret(
            &self,
            r: &str,
            text: &str,
            accept: &(dyn Fn(Option<String>) -> bool + Send + Sync),
        ) -> Result<String, String> {
            // The call log never carries the value.
            self.rec(&format!("fill_secret:{r}"))?;
            let live = match self.origin_at_insert.lock().clone() {
                Some(o) => o,
                None => self
                    .targets
                    .lock()
                    .get(r)
                    .and_then(|t| t.frame_origin.clone()),
            };
            if !accept(live) {
                return Err(
                    "refused: the frame is not an origin the owner allowed; nothing was typed"
                        .into(),
                );
            }
            self.typed_secrets.lock().push(text.to_owned());
            let out = if self.echo_value {
                format!("filled {r}; the field now shows value={text}")
            } else {
                format!("filled {r}")
            };
            self.raw_secret_output.lock().clone_from(&out);
            Ok(out)
        }
        async fn select(&self, r: &str, v: &str) -> Result<String, String> {
            self.rec(&format!("select:{r}:{v}"))
        }
        async fn press(&self, k: &str) -> Result<String, String> {
            self.rec(&format!("press:{k}"))
        }
        async fn scroll(&self, r: Option<&str>, dx: f64, dy: f64) -> Result<String, String> {
            self.rec(&format!("scroll:{}:{dx}:{dy}", r.unwrap_or("-")))
        }
        async fn wait(&self, c: Wait, _t: Option<Duration>) -> Result<String, String> {
            self.rec(&format!("wait:{c:?}"))
        }
        async fn back_opts(&self, dialog: Option<&str>) -> Result<String, String> {
            let mut s = "back".to_string();
            if let Some(d) = dialog {
                let _ = write!(s, ":{d}");
            }
            self.rec(&s)
        }
        async fn back(&self) -> Result<String, String> {
            self.back_opts(None).await
        }
        async fn tabs(&self, s: Option<usize>) -> Result<String, String> {
            self.rec(&format!("tabs:{s:?}"))
        }
        async fn screenshot(
            &self,
            full: bool,
            _e: Option<&str>,
        ) -> Result<(Vec<u8>, &'static str), String> {
            let _ = self.rec(&format!("screenshot:{full}"));
            Ok((vec![0xFF, 0xD8, 0xFF], "jpg"))
        }
        async fn eval(&self, js: &str) -> Result<Value, String> {
            self.calls.lock().push(format!("eval:{js}"));
            Ok(json!({"title": "Example"}))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeBrowser;
    use super::*;
    use tool_registry::ContentPart;

    #[test]
    fn a_stuck_browser_start_gets_a_bounded_actionable_message() {
        assert!(LAUNCH_DEADLINE < Duration::from_secs(60));
        let m = launch_deadline_message(false);
        assert!(m.contains("bot doctor") && m.contains("40s"), "{m}");
    }

    fn build() -> (tempfile::TempDir, Arc<FakeBrowser>, Vec<Arc<dyn Tool>>) {
        let d = tempfile::tempdir().unwrap();
        let fake = Arc::new(FakeBrowser::default());
        let t = tools(fake.clone(), d.path(), None);
        (d, fake, t)
    }

    fn get(t: &[Arc<dyn Tool>], name: &str) -> Arc<dyn Tool> {
        t.iter().find(|x| x.spec().name == name).unwrap().clone()
    }

    fn text(o: &ToolOutput) -> String {
        match &o.content[0] {
            ContentPart::Text { text } => text.clone(),
            _ => panic!(),
        }
    }

    fn snap(url: &str, lines: &[(&str, &str)]) -> browser_cdp::Snapshot {
        browser_cdp::Snapshot {
            url: url.into(),
            title: "T".into(),
            nodes: lines
                .iter()
                .map(|(r, name)| browser_cdp::SnapNode {
                    ref_id: Some((*r).into()),
                    role: "button".into(),
                    name: (*name).into(),
                    depth: 0,
                    value: None,
                    states: Vec::new(),
                })
                .collect(),
        }
    }

    fn result(before: &browser_cdp::Snapshot, after: browser_cdp::Snapshot) -> ActionResult {
        ActionResult {
            ok: true,
            error: None,
            new_snapshot_diff: browser_cdp::diff(before, &after),
            snapshot: after,
            dialogs: Vec::new(),
            settle_note: None,
        }
    }

    #[test]
    fn navigating_click_renders_full_snapshot_without_minus_lines() {
        let before = snap("http://a/", &[("e1", "Old one"), ("e2", "Old two")]);
        let after = snap(
            "http://a/next",
            &[("e1", "New one"), ("e2", "New two"), ("e3", "New three")],
        );
        let out = render_action(&result(&before, after)).unwrap();
        assert!(!out.lines().any(|l| l.starts_with("- ")), "{out}");
        assert!(!out.lines().any(|l| l.starts_with("+ ")), "{out}");
        for r in ["[e1]", "[e2]", "[e3]"] {
            assert!(out.contains(r), "{out}");
        }
        assert!(out.starts_with("# T (http://a/next)"));
    }

    #[test]
    fn small_in_page_change_keeps_the_diff() {
        let names: Vec<(String, String)> = (0..20)
            .map(|i| (format!("e{i}"), format!("Item {i}")))
            .collect();
        let lines: Vec<(&str, &str)> = names
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let before = snap("http://a/", &lines);
        let mut changed = lines.clone();
        changed[3] = ("e3", "Changed");
        let after = snap("http://a/", &changed);
        let want = format!(
            "# T (http://a/)\n{}",
            browser_cdp::diff(&before, &after).render()
        );
        let out = render_action(&result(&before, after)).unwrap();
        assert_eq!(out, want);
        assert!(out.contains("- ") && out.contains("+ "));
    }

    #[test]
    fn dialog_and_settle_note_are_appended() {
        let a = snap("http://a/", &[("e1", "Go")]);
        let mut r = result(&a, a.clone());
        let long_msg = format!("hello\x00\x07world\n{}", "a".repeat(350));
        r.dialogs = vec![
            browser_cdp::DialogInfo {
                kind: "confirm".into(),
                message: "sure?".into(),
                accepted: false,
            },
            browser_cdp::DialogInfo {
                kind: "alert".into(),
                message: long_msg,
                accepted: true,
            },
        ];
        r.settle_note = Some("network still busy: 2 requests".into());
        let out = render_action(&r).unwrap();
        assert!(
            out.contains("\n~ dialog(confirm) dismissed page content: \"sure?\""),
            "{out}"
        );
        let alert_line = out.lines().find(|l| l.contains("dialog(alert)")).unwrap();
        assert!(alert_line.contains("accepted page content: \"helloworld "));
        assert!(alert_line.ends_with("...\""));
        assert!(!alert_line.contains('\x00') && !alert_line.contains('\x07'));
        assert!(out.ends_with("\n~ network still busy: 2 requests"), "{out}");
    }

    #[tokio::test]
    async fn browser_open_rejects_non_web_schemes() {
        let (_d, fake, t) = build();
        let ctx = ToolContext::default();
        for bad in [
            "file:///C:/Users/x/.ssh/id_rsa",
            "javascript:alert(1)",
            " JaVaScRiPt:alert(1)",
            "java\nscript:alert(1)",
            "data:text/html,<script>1</script>",
            "chrome://settings",
            "view-source:https://example.com",
            "blob:https://example.com/x",
            "example.com",
            "https://",
            "",
        ] {
            let err = get(&t, "browser_open")
                .call(&ctx, json!({"url": bad}))
                .await
                .unwrap_err();
            assert!(matches!(err, ToolError::InvalidInput(_)), "{bad}: {err}");
        }
        assert!(fake.calls.lock().is_empty());
        for good in [
            "http://localhost:3000/a",
            "HTTPS://example.com",
            "about:blank",
        ] {
            get(&t, "browser_open")
                .call(&ctx, json!({"url": good}))
                .await
                .unwrap();
        }
        assert_eq!(fake.calls.lock().len(), 3);
    }

    #[tokio::test]
    async fn page_grants_follow_receiving_and_focused_frames_not_the_top_url() {
        let (_dir, fake, tools) = build();
        *fake.url.lock() = Some("https://owner.test/".into());
        let target = |origin: Option<&str>| FieldTarget {
            name: "embedded input".into(),
            role: "textbox".into(),
            frame_origin: origin.map(str::to_owned),
            top_origin: Some("https://owner.test".into()),
            sensitive: false,
        };
        fake.targets
            .lock()
            .insert("e1".into(), target(Some("https://attacker.test")));
        fake.targets
            .lock()
            .insert("focused".into(), target(Some("https://attacker.test")));
        let ctx = ToolContext {
            expected_origin: Some("https://owner.test:443".into()),
            ..ToolContext::default()
        };
        let fill = get(&tools, "browser_fill");
        let input = json!({"ref":"e1", "text":"private document"});
        assert_eq!(
            fill.current_origin(&ctx, &input).await.as_deref(),
            Some("https://attacker.test:443")
        );
        assert!(fill.call(&ctx, input.clone()).await.is_err());
        let press = get(&tools, "browser_press");
        assert_eq!(
            press
                .current_origin(&ctx, &json!({"key":"Enter"}))
                .await
                .as_deref(),
            Some("https://attacker.test:443")
        );
        assert!(press.call(&ctx, json!({"key":"Enter"})).await.is_err());
        // A sandboxed/opaque receiver never supplies a reusable origin.
        fake.targets.lock().insert("e1".into(), target(None));
        assert!(fill.current_origin(&ctx, &input).await.is_none());
        assert!(fill.call(&ctx, input.clone()).await.is_err());
        assert!(
            fake.calls.lock().is_empty(),
            "no rejected input reached the driver"
        );
        fake.targets
            .lock()
            .insert("e1".into(), target(Some("https://owner.test")));
        assert!(fill.call(&ctx, input).await.is_ok());
    }

    #[tokio::test]
    async fn tools_forward_arguments_to_the_driver() {
        let (_d, fake, t) = build();
        let ctx = ToolContext::default();
        get(&t, "browser_open")
            .call(
                &ctx,
                json!({"url": "https://example.com", "profile": "work"}),
            )
            .await
            .unwrap();
        get(&t, "browser_click")
            .call(&ctx, json!({"ref": "e1"}))
            .await
            .unwrap();
        get(&t, "browser_fill")
            .call(&ctx, json!({"ref": "e2", "text": "hi"}))
            .await
            .unwrap();
        get(&t, "browser_wait")
            .call(&ctx, json!({"text": "Welcome"}))
            .await
            .unwrap();
        get(&t, "browser_scroll")
            .call(&ctx, json!({"dy": 100}))
            .await
            .unwrap();
        let calls = fake.calls.lock().clone();
        assert_eq!(
            calls,
            vec![
                "open:https://example.com:work",
                "click:e1",
                "fill:e2:hi",
                "wait:Text(\"Welcome\")",
                "scroll:-:0:100"
            ]
        );
    }

    #[tokio::test]
    async fn results_say_whether_the_user_can_see_the_page() {
        let (_d, fake, t) = build();
        let ctx = ToolContext::default();
        let open = get(&t, "browser_open");
        let call = || open.call(&ctx, json!({"url": "https://example.com"}));
        // A driver that cannot tell claims nothing.
        assert!(!text(&call().await.unwrap()).contains("[view:"));
        *fake.watched.lock() = Some(false);
        let hidden = text(&call().await.unwrap());
        assert!(hidden.ends_with(view_line(false)), "{hidden}");
        assert!(hidden.contains("NOT visible to the user"), "{hidden}");
        *fake.watched.lock() = Some(true);
        let seen = text(&call().await.unwrap());
        assert!(seen.ends_with("[view: visible to the user in the live view]"));
    }

    #[tokio::test]
    async fn tools_forward_dialog_options_to_the_driver() {
        let (_d, fake, t) = build();
        let ctx = ToolContext::default();
        get(&t, "browser_click")
            .call(
                &ctx,
                json!({"ref": "e1", "dialog": "accept", "prompt_text": "hello"}),
            )
            .await
            .unwrap();
        get(&t, "browser_open")
            .call(
                &ctx,
                json!({"url": "https://example.com", "dialog": "dismiss"}),
            )
            .await
            .unwrap();
        get(&t, "browser_back")
            .call(&ctx, json!({"dialog": "accept"}))
            .await
            .unwrap();
        let calls = fake.calls.lock().clone();
        assert_eq!(
            calls,
            vec![
                "click:e1:accept:hello",
                "open:https://example.com:-:dismiss",
                "back:accept",
            ]
        );
    }

    #[tokio::test]
    async fn driver_errors_become_execution_errors() {
        let (_d, _f, t) = build();
        let err = get(&t, "browser_click")
            .call(&ToolContext::default(), json!({"ref": "e404"}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Execution(m) if m.contains("unknown ref")));
    }

    #[tokio::test]
    async fn wait_needs_exactly_one_condition() {
        let (_d, _f, t) = build();
        let w = get(&t, "browser_wait");
        let ctx = ToolContext::default();
        assert!(w.call(&ctx, json!({})).await.is_err());
        assert!(
            w.call(&ctx, json!({"text": "a", "idle": true}))
                .await
                .is_err()
        );
        assert!(w.call(&ctx, json!({"idle": true})).await.is_ok());
    }

    #[tokio::test]
    async fn screenshot_saves_a_file_and_eval_returns_json() {
        let (_d, _f, t) = build();
        let ctx = ToolContext::default();
        let out = get(&t, "browser_screenshot")
            .call(&ctx, json!({}))
            .await
            .unwrap();
        let s = text(&out);
        let path = s.strip_prefix("screenshot: ").unwrap();
        assert!(Path::new(path).exists());
        let out = get(&t, "browser_eval")
            .call(&ctx, json!({"js": "document.title"}))
            .await
            .unwrap();
        assert_eq!(text(&out), "{\"title\":\"Example\"}");
    }

    #[test]
    fn eval_is_in_the_script_category() {
        let (_d, _f, t) = build();
        assert_eq!(
            get(&t, "browser_eval").spec().category,
            PermissionCategory::Script
        );
        assert_eq!(
            get(&t, "browser_click").spec().category,
            PermissionCategory::Browser
        );
    }

    /// 244-V: a managed bot browser gets an idle live-view target the moment the
    /// bot is first selected; nothing is launched and nothing is captured.
    #[test]
    fn managed_bot_browser_registers_an_idle_view_target_without_launching() {
        let dir = tempfile::tempdir().unwrap();
        let settings = BrowserSettings::isolated(dir.path());
        let registry = Arc::new(live_view::TargetRegistry::default());
        let shared = Arc::new(Shared {
            driver: Arc::new(CdpDriver::new(settings.clone())),
            managed: Some(ManagedBrowsers {
                settings,
                peers: BrowserPeers::default(),
                bots: BotDrivers::default(),
                views: Some(Arc::clone(&registry)),
            }),
            data_dir: dir.path().into(),
            control: None,
            fill_secrets: None,
            observed_epoch: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        });
        assert!(
            registry.targets().is_empty(),
            "nothing before a bot uses the browser"
        );
        let bot = "0194a1c2-0000-7000-8000-000000000001";
        let _ = shared.for_bot(Some(bot)).unwrap();
        let _ = shared.for_bot(Some(bot)).unwrap();
        let targets = registry.targets();
        assert_eq!(targets.len(), 1, "one slot per bot, not per call");
        assert_eq!(targets[0].kind, live_view::TargetKind::Browser);
        assert_eq!(targets[0].bot_id, uuid::Uuid::parse_str(bot).ok());
        assert_eq!(
            targets[0].unavailable_reason,
            Some(live_view::Reason::BrowserIdle)
        );
        // Idle means no Chromium and no profile directory.
        assert!(!dir.path().join("browsers").exists());
        // A second bot gets its own slot.
        let _ = shared
            .for_bot(Some("0194a1c2-0000-7000-8000-000000000002"))
            .unwrap();
        assert_eq!(registry.targets().len(), 2);
    }

    #[test]
    fn managed_profiles_are_per_bot_and_isolated_stays_temporary() {
        let dir = tempfile::tempdir().unwrap();
        let settings = BrowserSettings::isolated(dir.path());
        let shared = Arc::new(Shared {
            driver: Arc::new(CdpDriver::new(settings.clone())),
            managed: Some(ManagedBrowsers {
                settings,
                peers: BrowserPeers::default(),
                bots: BotDrivers::default(),
                views: None,
            }),
            data_dir: dir.path().into(),
            control: None,
            observed_epoch: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            fill_secrets: None,
        });
        let a = shared.for_bot(Some("bot-a")).unwrap();
        let b = shared.for_bot(Some("bot-b")).unwrap();
        let peers = &shared.managed.as_ref().unwrap().peers;
        let a_driver = peers.lock().get("bot-a").unwrap().upgrade().unwrap();
        let b_driver = peers.lock().get("bot-b").unwrap().upgrade().unwrap();
        assert_eq!(
            a_driver.settings.profile_dir,
            Some(dir.path().join("browsers/bot-a"))
        );
        assert_eq!(
            b_driver.settings.profile_dir,
            Some(dir.path().join("browsers/bot-b"))
        );
        assert_eq!(a_driver.profile_path(Some("isolated")).unwrap(), None);

        assert!(!Arc::ptr_eq(&a, &b));
        assert!(Arc::ptr_eq(&a, &shared.for_bot(Some("bot-a")).unwrap()));
        assert!(shared.for_bot(None).is_err());
        assert!(shared.for_bot(Some("../escape")).is_err());
        assert!(
            !dir.path().join("browsers").exists(),
            "selection never launches Chromium"
        );
        let drv = CdpDriver::new(BrowserSettings {
            profile_dir: Some(dir.path().join("browsers/bot-a")),
            ..BrowserSettings::isolated(dir.path())
        });
        assert_eq!(
            drv.profile_path(None).unwrap(),
            Some(dir.path().join("browsers/bot-a"))
        );
        assert_eq!(drv.profile_path(Some("isolated")).unwrap(), None);
    }

    #[test]
    fn profile_paths_follow_settings() {
        let d = tempfile::tempdir().unwrap();
        let drv = CdpDriver::new(BrowserSettings::isolated(d.path()));
        assert_eq!(drv.profile_path(None).unwrap(), None);
        assert_eq!(drv.profile_path(Some("isolated")).unwrap(), None);
        assert!(
            drv.profile_path(Some("work"))
                .unwrap()
                .unwrap()
                .ends_with("work")
        );
        assert!(drv.profile_path(Some("../evil")).is_err());
        let drv = CdpDriver::new(BrowserSettings {
            profile_dir: Some(d.path().join("p")),
            ..BrowserSettings::isolated(d.path())
        });
        assert_eq!(drv.profile_path(None).unwrap(), Some(d.path().join("p")));
    }

    #[test]
    fn tab_activation_spec_effect_is_write() {
        let (_d, _f, t) = build();
        // Activation is `browser_tabs` with `switch_to`. Its effect is what
        // `read_tools_skip_ask` consults; the control gate does not run when
        // the overlay app is closed.
        assert_eq!(
            get(&t, "browser_tabs").spec().effect,
            Effect::Write,
            "tab activation must be Write so a default Ask is not auto-approved"
        );
    }

    #[tokio::test]
    async fn remote_pc_run_cannot_call_host_browser_even_directly() {
        struct NoBrowser;
        #[async_trait]
        impl DesktopControlGate for NoBrowser {
            fn browser_available_for_run(&self, _: &str) -> bool {
                false
            }
            async fn before_action(&self, _: &ControlRequest) -> Result<(), String> {
                Ok(())
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let fake = Arc::new(FakeBrowser::default());
        let tools = tools(fake.clone(), dir.path(), Some(Arc::new(NoBrowser)));
        for (name, input) in [
            ("browser_open", json!({"url":"https://example.test"})),
            ("browser_snapshot", json!({})),
        ] {
            let err = get(&tools, name)
                .call(&ToolContext::default(), input)
                .await
                .unwrap_err();
            assert!(err.to_string().contains("computer target"));
        }
        assert!(fake.calls.lock().is_empty());
    }

    struct RefuseGate;

    #[async_trait]
    impl DesktopControlGate for RefuseGate {
        async fn before_action(&self, _req: &ControlRequest) -> Result<(), String> {
            Err("the user has taken control".into())
        }
    }

    #[tokio::test]
    async fn refusing_control_gate_blocks_eval_and_tab_switch() {
        let d = tempfile::tempdir().unwrap();
        let fake = Arc::new(FakeBrowser {
            on_desktop: true,
            ..FakeBrowser::default()
        });
        let t = tools(fake.clone(), d.path(), Some(Arc::new(RefuseGate)));
        let ctx = ToolContext::default();
        let err = get(&t, "browser_eval")
            .call(&ctx, json!({"js": "document.title"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("taken control"), "{err}");
        let err = get(&t, "browser_tabs")
            .call(&ctx, json!({"switch_to": 2}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("taken control"), "{err}");
        assert!(
            fake.calls.lock().is_empty(),
            "refused actions reached the browser: {:?}",
            fake.calls.lock()
        );
        get(&t, "browser_tabs").call(&ctx, json!({})).await.unwrap();
        assert_eq!(fake.calls.lock().as_slice(), ["tabs:None"]);
    }

    struct HumanDrives;

    #[async_trait]
    impl DesktopControlGate for HumanDrives {
        fn observation_refused(&self) -> Option<String> {
            Some("the user is controlling (observation paused)".into())
        }
        async fn before_action(&self, _req: &ControlRequest) -> Result<(), String> {
            Err("the user has taken control".into())
        }
    }

    #[tokio::test]
    async fn browser_observation_is_refused_while_the_human_drives() {
        let dir = tempfile::tempdir().unwrap();
        let fake = Arc::new(FakeBrowser::default());
        let t = tools(fake.clone(), dir.path(), Some(Arc::new(HumanDrives)));
        for (name, input) in [
            ("browser_snapshot", json!({})),
            ("browser_screenshot", json!({})),
            ("browser_tabs", json!({})),
            ("browser_wait", json!({"idle":true})),
        ] {
            let err = get(&t, name)
                .call(&ToolContext::default(), input)
                .await
                .unwrap_err();
            assert!(
                err.to_string().contains("observation paused"),
                "{name}: {err}"
            );
        }
        assert!(fake.calls.lock().is_empty(), "{:?}", fake.calls.lock());
    }

    #[tokio::test]
    async fn headless_mutations_all_obey_the_same_control_gate() {
        let dir = tempfile::tempdir().unwrap();
        let fake = Arc::new(FakeBrowser::default());
        let tools = tools(fake.clone(), dir.path(), Some(Arc::new(RefuseGate)));
        for (name, input) in [
            ("browser_open", json!({"url":"https://example.invalid"})),
            ("browser_click", json!({"ref":"e1"})),
            ("browser_fill", json!({"ref":"e1","text":"test"})),
            ("browser_select", json!({"ref":"e1","value":"v"})),
            ("browser_press", json!({"key":"Enter"})),
            ("browser_scroll", json!({"dy":1})),
            ("browser_back", json!({})),
            ("browser_tabs", json!({"switch_to":2})),
            ("browser_eval", json!({"js":"1"})),
        ] {
            assert!(
                get(&tools, name)
                    .call(&ToolContext::default(), input)
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("taken control")
            );
        }
        assert!(fake.calls.lock().is_empty());
    }

    fn ws_accept(key: &str) -> String {
        const GUID: &[u8] = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
        let mut data = key.as_bytes().to_vec();
        data.extend_from_slice(GUID);
        b64(&sha1(&data))
    }

    fn b64(data: &[u8]) -> String {
        const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in data.chunks(3) {
            let mut n = u32::from(chunk[0]) << 16;
            if chunk.len() > 1 {
                n |= u32::from(chunk[1]) << 8;
            }
            if chunk.len() > 2 {
                n |= u32::from(chunk[2]);
            }
            out.push(TABLE[((n >> 18) & 0x3f) as usize] as char);
            out.push(TABLE[((n >> 12) & 0x3f) as usize] as char);
            if chunk.len() > 1 {
                out.push(TABLE[((n >> 6) & 0x3f) as usize] as char);
            } else {
                out.push('=');
            }
            if chunk.len() > 2 {
                out.push(TABLE[(n & 0x3f) as usize] as char);
            } else {
                out.push('=');
            }
        }
        out
    }

    #[allow(clippy::many_single_char_names, clippy::needless_range_loop)]
    fn sha1(message: &[u8]) -> [u8; 20] {
        let mut h0: u32 = 0x6745_2301;
        let mut h1: u32 = 0xEFCD_AB89;
        let mut h2: u32 = 0x98BA_DCFE;
        let mut h3: u32 = 0x1032_5476;
        let mut h4: u32 = 0xC3D2_E1F0;
        let bit_len = u64::try_from(message.len())
            .unwrap_or(u64::MAX)
            .saturating_mul(8);
        let mut data = message.to_vec();
        data.push(0x80);
        while (data.len() % 64) != 56 {
            data.push(0);
        }
        data.extend_from_slice(&bit_len.to_be_bytes());
        for block in data.chunks(64) {
            let mut w = [0u32; 80];
            for i in 0..16 {
                w[i] = u32::from_be_bytes(block[i * 4..i * 4 + 4].try_into().expect("4 bytes"));
            }
            for i in 16..80 {
                w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
            }
            let (mut a, mut b, mut c, mut d, mut e) = (h0, h1, h2, h3, h4);
            for i in 0..80 {
                let (f, k) = match i {
                    0..=19 => ((b & c) | ((!b) & d), 0x5A82_7999),
                    20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                    40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                    _ => (b ^ c ^ d, 0xCA62_C1D6),
                };
                let temp = a
                    .rotate_left(5)
                    .wrapping_add(f)
                    .wrapping_add(e)
                    .wrapping_add(k)
                    .wrapping_add(w[i]);
                e = d;
                d = c;
                c = b.rotate_left(30);
                b = a;
                a = temp;
            }
            h0 = h0.wrapping_add(a);
            h1 = h1.wrapping_add(b);
            h2 = h2.wrapping_add(c);
            h3 = h3.wrapping_add(d);
            h4 = h4.wrapping_add(e);
        }
        let mut out = [0u8; 20];
        out[0..4].copy_from_slice(&h0.to_be_bytes());
        out[4..8].copy_from_slice(&h1.to_be_bytes());
        out[8..12].copy_from_slice(&h2.to_be_bytes());
        out[12..16].copy_from_slice(&h3.to_be_bytes());
        out[16..20].copy_from_slice(&h4.to_be_bytes());
        out
    }

    struct FakeCdp {
        url: String,
        shutdown: Arc<std::sync::atomic::AtomicBool>,
        killer: Arc<std::sync::Mutex<Option<std::net::TcpStream>>>,
    }

    impl FakeCdp {
        fn start() -> Self {
            use std::io::{Read, Write};
            use std::net::{Shutdown, TcpListener};
            use std::sync::atomic::AtomicBool;
            use std::sync::{Arc, Mutex};

            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let shutdown = Arc::new(AtomicBool::new(false));
            let killer = Arc::new(Mutex::new(None));
            let flag = Arc::clone(&shutdown);
            let slot = Arc::clone(&killer);
            std::thread::spawn(move || {
                let Ok((mut sock, _)) = listener.accept() else {
                    return;
                };
                let _ = sock.set_read_timeout(Some(Duration::from_millis(200)));
                if let Ok(clone) = sock.try_clone() {
                    *slot.lock().expect("killer lock") = Some(clone);
                }
                let mut buf = Vec::new();
                let mut tmp = [0u8; 2048];
                loop {
                    if flag.load(std::sync::atomic::Ordering::SeqCst) {
                        return;
                    }
                    match sock.read(&mut tmp) {
                        Ok(0) => return,
                        Ok(n) => {
                            buf.extend_from_slice(&tmp[..n]);
                            if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                                break;
                            }
                        }
                        Err(e)
                            if e.kind() == std::io::ErrorKind::TimedOut
                                || e.kind() == std::io::ErrorKind::WouldBlock => {}
                        Err(_) => return,
                    }
                }
                let header = String::from_utf8_lossy(&buf);
                let key = header
                    .lines()
                    .find_map(|line| {
                        line.split_once(':')
                            .filter(|(n, _)| n.eq_ignore_ascii_case("Sec-WebSocket-Key"))
                            .map(|(_, v)| v.trim().to_string())
                    })
                    .unwrap_or_default();
                let accept = ws_accept(&key);
                let response = format!(
                    "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
                );
                if sock.write_all(response.as_bytes()).is_err() {
                    return;
                }
                let _ = sock.flush();
                let header_end = buf
                    .windows(4)
                    .position(|w| w == b"\r\n\r\n")
                    .map_or(buf.len(), |i| i + 4);
                buf.drain(..header_end);
                while !flag.load(std::sync::atomic::Ordering::SeqCst) {
                    match read_ws_frame(&mut sock, &mut buf, &flag) {
                        Ok(Some((opcode, payload))) => {
                            if opcode == 0x8 {
                                return;
                            }
                            if opcode == 0x9 {
                                let _ = write_ws_frame(&mut sock, 0xA, &payload);
                                continue;
                            }
                            if opcode != 0x1 {
                                continue;
                            }
                            let Ok(text) = std::str::from_utf8(&payload) else {
                                continue;
                            };
                            let Ok(req) = serde_json::from_str::<Value>(text) else {
                                continue;
                            };
                            let reply = cdp_reply(&req);
                            let _ = write_ws_frame(&mut sock, 0x1, reply.to_string().as_bytes());
                        }
                        Ok(None) | Err(_) => return,
                    }
                }
                let _ = sock.shutdown(Shutdown::Both);
            });
            Self {
                url: format!("ws://127.0.0.1:{port}/devtools/browser/fake"),
                shutdown,
                killer,
            }
        }

        fn kill(&self) {
            self.shutdown
                .store(true, std::sync::atomic::Ordering::SeqCst);
            if let Some(sock) = self.killer.lock().expect("killer lock").take() {
                let _ = sock.shutdown(std::net::Shutdown::Both);
            }
        }
    }

    fn cdp_reply(req: &Value) -> Value {
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let method = req.get("method").and_then(Value::as_str).unwrap_or("");
        let result = match method {
            "Target.getTargets" => json!({
                "targetInfos": [{
                    "targetId": "t1",
                    "type": "page",
                    "title": "T",
                    "url": "about:blank"
                }]
            }),
            "Target.attachToTarget" => json!({"sessionId": "s1"}),
            "Target.createTarget" => {
                static NEXT: std::sync::atomic::AtomicUsize =
                    std::sync::atomic::AtomicUsize::new(1);
                let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                json!({"targetId": format!("lane{n}")})
            }
            _ => json!({}),
        };
        json!({"id": id, "result": result})
    }

    fn write_ws_frame(
        sock: &mut std::net::TcpStream,
        opcode: u8,
        payload: &[u8],
    ) -> std::io::Result<()> {
        use std::io::Write;
        let mut hdr = vec![0x80 | opcode];
        let n = payload.len();
        if n < 126 {
            hdr.push(u8::try_from(n).unwrap_or(125));
        } else if u16::try_from(n).is_ok() {
            hdr.push(126);
            hdr.extend_from_slice(&u16::try_from(n).unwrap_or(u16::MAX).to_be_bytes());
        } else {
            hdr.push(127);
            hdr.extend_from_slice(&u64::try_from(n).unwrap_or(u64::MAX).to_be_bytes());
        }
        sock.write_all(&hdr)?;
        sock.write_all(payload)?;
        sock.flush()
    }

    fn read_ws_frame(
        sock: &mut std::net::TcpStream,
        buf: &mut Vec<u8>,
        shutdown: &std::sync::atomic::AtomicBool,
    ) -> std::io::Result<Option<(u8, Vec<u8>)>> {
        loop {
            if buf.len() >= 2 {
                break;
            }
            if !read_some(sock, buf, shutdown)? {
                return Ok(None);
            }
        }
        let opcode = buf[0] & 0x0f;
        let masked = buf[1] & 0x80 != 0;
        let mut len = usize::from(buf[1] & 0x7f);
        let mut header = 2;
        if len == 126 {
            while buf.len() < 4 {
                if !read_some(sock, buf, shutdown)? {
                    return Ok(None);
                }
            }
            len = usize::from(u16::from_be_bytes([buf[2], buf[3]]));
            header = 4;
        } else if len == 127 {
            while buf.len() < 10 {
                if !read_some(sock, buf, shutdown)? {
                    return Ok(None);
                }
            }
            let raw = u64::from_be_bytes(buf[2..10].try_into().unwrap_or([0; 8]));
            len = usize::try_from(raw).unwrap_or(0);
            header = 10;
        }
        let mask_len = usize::from(masked) * 4;
        let total = header + mask_len + len;
        while buf.len() < total {
            if !read_some(sock, buf, shutdown)? {
                return Ok(None);
            }
        }
        let mask = if masked {
            let mut m = [0u8; 4];
            m.copy_from_slice(&buf[header..header + 4]);
            Some(m)
        } else {
            None
        };
        let start = header + mask_len;
        let mut payload = buf[start..total].to_vec();
        if let Some(m) = mask {
            for (i, b) in payload.iter_mut().enumerate() {
                *b ^= m[i % 4];
            }
        }
        buf.drain(..total);
        Ok(Some((opcode, payload)))
    }

    fn read_some(
        sock: &mut std::net::TcpStream,
        buf: &mut Vec<u8>,
        shutdown: &std::sync::atomic::AtomicBool,
    ) -> std::io::Result<bool> {
        use std::io::Read;
        if shutdown.load(std::sync::atomic::Ordering::SeqCst) {
            return Ok(false);
        }
        let mut tmp = [0u8; 4096];
        match sock.read(&mut tmp) {
            Ok(0) => Ok(false),
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                Ok(true)
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::TimedOut
                    || e.kind() == std::io::ErrorKind::WouldBlock =>
            {
                Ok(true)
            }
            Err(e) => Err(e),
        }
    }

    fn write_fake_launcher(dir: &Path, log: &Path) -> PathBuf {
        let log_text = log.display().to_string();
        assert!(!log_text.contains('"') && !log_text.contains('\''));
        #[cfg(windows)]
        {
            let path = dir.join("fake-browser.cmd");
            std::fs::write(
                &path,
                format!("@echo off\r\necho launched>>\"{log_text}\"\r\n"),
            )
            .unwrap();
            path
        }
        #[cfg(not(windows))]
        {
            use std::os::unix::fs::PermissionsExt;
            let path = dir.join("fake-browser");
            std::fs::write(&path, format!("#!/bin/sh\necho launched >> '{log_text}'\n")).unwrap();
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).unwrap();
            path
        }
    }

    #[tokio::test]
    async fn switching_bots_reclaims_inactive_browser_but_keeps_active_work_and_takeover() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().join("browsers/bot-a");
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::write(profile.join("login-fixture"), "keep").unwrap();
        let a = Arc::new(CdpDriver::new(BrowserSettings {
            profile_dir: Some(profile.clone()),
            ..BrowserSettings::isolated(dir.path())
        }));
        let server = FakeCdp::start();
        let browser = Browser::connect(&server.url).await.unwrap();
        let page = browser.first_page().await.unwrap();
        *a.session.lock().await = Some(Session {
            browser,
            page,
            profile: None,
            headless: true,
            last_work: tokio::time::Instant::now(),
            lanes: Vec::new(),
        });
        let active = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let probe = Arc::clone(&active);
        a.set_activity_probe(Arc::new(move || {
            probe.load(std::sync::atomic::Ordering::SeqCst)
        }));
        let mut b = CdpDriver::new(BrowserSettings {
            executable: Some(write_fake_launcher(
                dir.path(),
                &dir.path().join("launches"),
            )),
            ..BrowserSettings::isolated(dir.path())
        });
        let peers = BrowserPeers::default();
        peers.lock().insert("bot-a".into(), Arc::downgrade(&a));
        b.peers = Some(("bot-b".into(), peers));
        assert!(b.launch_session(None).await.is_err());
        assert!(
            a.session.lock().await.is_some(),
            "active bot keeps its browser"
        );
        active.store(false, std::sync::atomic::Ordering::SeqCst);
        a.set_takeover_pin(true);
        assert!(b.launch_session(None).await.is_err());
        assert!(
            a.session.lock().await.is_some(),
            "takeover keeps its browser"
        );
        a.set_takeover_pin(false);
        assert!(b.launch_session(None).await.is_err());
        assert!(
            a.session.lock().await.is_none(),
            "idle browser was reclaimed before launch"
        );
        assert!(
            profile.join("login-fixture").exists(),
            "persistent login profiles survive close"
        );
        server.kill();
    }

    /// ADR 0025: each subagent run works in its own tab; the top-level run keeps
    /// the shared one, and a lane keeps its tab across calls.
    #[tokio::test]
    async fn subagent_runs_keep_their_own_tab_apart_from_the_shared_one() {
        async fn tab(driver: &CdpDriver, lane: Option<&str>) -> String {
            BROWSER_LANE
                .scope(lane.map(str::to_owned), async {
                    with_session!(driver, None, false, |sess| {
                        Ok::<String, browser_cdp::Error>(sess.page.target_id().to_string())
                    })
                })
                .await
                .unwrap()
        }
        let server = FakeCdp::start();
        let dir = tempfile::tempdir().unwrap();
        let driver = CdpDriver::new(BrowserSettings::isolated(dir.path()));
        *driver.session.lock().await = Some(session_for(&server).await);
        let shared = tab(&driver, None).await;
        let a = tab(&driver, Some("run-a")).await;
        let b = tab(&driver, Some("run-b")).await;
        assert_eq!(shared, "t1");
        assert_ne!(a, shared);
        assert_ne!(b, shared);
        assert_ne!(a, b);
        assert_eq!(tab(&driver, Some("run-a")).await, a, "a lane keeps its tab");
        assert_eq!(
            tab(&driver, None).await,
            shared,
            "the shared tab is untouched"
        );
        // A subagent lists only its own tab; the top-level run does not list the lanes' tabs.
        let listed_in_lane = BROWSER_LANE
            .scope(Some("run-a".to_owned()), driver.tabs(None))
            .await
            .unwrap();
        assert!(!listed_in_lane.contains("about:blank"), "{listed_in_lane}");
        server.kill();
    }

    #[tokio::test]
    async fn read_only_browser_calls_cannot_implicitly_launch_outside_admission() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("launches.txt");
        let launcher = write_fake_launcher(dir.path(), &log);
        let driver = CdpDriver::new(BrowserSettings {
            executable: Some(launcher),
            ..BrowserSettings::isolated(dir.path())
        });
        assert!(
            driver
                .snapshot(false)
                .await
                .unwrap_err()
                .contains("browser_idle")
        );
        assert!(
            driver
                .tabs(None)
                .await
                .unwrap_err()
                .contains("browser_idle")
        );
        assert!(
            driver
                .screenshot(false, None)
                .await
                .unwrap_err()
                .contains("browser_idle")
        );
        assert!(!log.exists());
    }

    #[tokio::test]
    async fn watch_does_not_launch_and_takeover_pin_blocks_idle_close() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("launches.txt");
        let launcher = write_fake_launcher(dir.path(), &log);
        let driver = CdpDriver::new(BrowserSettings {
            executable: Some(launcher),
            idle_timeout: Duration::from_millis(40),
            ..BrowserSettings::isolated(dir.path())
        });
        // A registry/watch has no reason to call the bot-only `ensure` path.
        assert!(driver.session.lock().await.is_none());
        assert!(!log.exists());
        let server = FakeCdp::start();
        let browser = Browser::connect(&server.url).await.unwrap();
        let page = browser.first_page().await.unwrap();
        *driver.session.lock().await = Some(Session {
            browser,
            page,
            profile: None,
            headless: true,
            last_work: tokio::time::Instant::now(),
            lanes: Vec::new(),
        });
        driver.set_takeover_pin(true);
        driver.start_idle_reaper();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(driver.session.lock().await.is_some());
        driver.set_takeover_pin(false);
        for _ in 0..100 {
            if driver.session.lock().await.is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(driver.session.lock().await.is_none());
        assert!(!log.exists(), "watch/idle must not launch a new process");
        server.kill();
    }

    /// Deterministic: the tick is driven by hand on a frozen clock (no timers,
    /// no sleeps), so a slow CI runner cannot change the outcome.
    #[tokio::test]
    async fn an_active_bot_run_keeps_the_browser_open_between_steps_then_idle_close_resumes() {
        async fn tick(driver: &CdpDriver, timeout: Duration, state: &mut ReapState) {
            reap_tick(
                &driver.session,
                None,
                timeout,
                &driver.takeover_pin,
                &driver.activity,
                state,
            )
            .await;
        }
        let dir = tempfile::tempdir().unwrap();
        let driver = CdpDriver::new(BrowserSettings {
            idle_timeout: Duration::from_millis(40),
            ..BrowserSettings::isolated(dir.path())
        });
        let server = FakeCdp::start();
        let browser = Browser::connect(&server.url).await.unwrap();
        let page = browser.first_page().await.unwrap();
        *driver.session.lock().await = Some(Session {
            browser,
            page,
            profile: None,
            headless: true,
            last_work: tokio::time::Instant::now(),
            lanes: Vec::new(),
        });
        // Connected over real I/O; from here the clock is frozen and advanced by hand.
        tokio::time::pause();
        let run_active = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let probe = Arc::clone(&run_active);
        driver.set_activity_probe(Arc::new(move || {
            probe.load(std::sync::atomic::Ordering::SeqCst)
        }));
        let timeout = Duration::from_millis(40);
        let mut state = ReapState::default();
        // The run is active far longer than the idle timeout (waiting for the
        // model or an approval between steps): the browser stays.
        for _ in 0..10 {
            tokio::time::advance(Duration::from_millis(100)).await;
            tick(&driver, timeout, &mut state).await;
            assert!(
                driver.session.lock().await.is_some(),
                "idle close must not take the browser from an active run"
            );
        }
        // The run ends. The idle clock starts at the tick that sees it end:
        // not closed on that tick, nor just before a full timeout after it.
        // (The run ends 45 ms after the last tick that saw it active: longer
        // than the idle timeout, but the run was active until just now.)
        tokio::time::advance(Duration::from_millis(45)).await;
        run_active.store(false, std::sync::atomic::Ordering::SeqCst);
        tick(&driver, timeout, &mut state).await;
        assert!(
            driver.session.lock().await.is_some(),
            "not closed on the tick that sees the run end"
        );
        tokio::time::advance(Duration::from_millis(39)).await;
        tick(&driver, timeout, &mut state).await;
        assert!(driver.session.lock().await.is_some(), "39 ms < 40 ms idle");
        tokio::time::advance(Duration::from_millis(2)).await;
        tick(&driver, timeout, &mut state).await;
        assert!(driver.session.lock().await.is_none(), "idle close resumed");
        server.kill();
    }

    #[tokio::test]
    async fn a_takeover_pin_and_a_run_that_starts_just_before_the_close_keep_the_browser() {
        let dir = tempfile::tempdir().unwrap();
        let driver = CdpDriver::new(BrowserSettings {
            idle_timeout: Duration::from_millis(40),
            ..BrowserSettings::isolated(dir.path())
        });
        let server = FakeCdp::start();
        let browser = Browser::connect(&server.url).await.unwrap();
        let page = browser.first_page().await.unwrap();
        *driver.session.lock().await = Some(Session {
            browser,
            page,
            profile: None,
            headless: true,
            last_work: tokio::time::Instant::now(),
            lanes: Vec::new(),
        });
        tokio::time::pause();
        let timeout = Duration::from_millis(40);
        let mut state = ReapState::default();
        driver.set_takeover_pin(true);
        tokio::time::advance(Duration::from_secs(1)).await;
        reap_tick(
            &driver.session,
            None,
            timeout,
            &driver.takeover_pin,
            &driver.activity,
            &mut state,
        )
        .await;
        assert!(driver.session.lock().await.is_some(), "pinned");
        driver.set_takeover_pin(false);
        // A run admits an action between the first probe and the close: the
        // probe is read again under the session lock.
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = Arc::clone(&calls);
        driver.set_activity_probe(Arc::new(move || {
            // False for the first check, true from then on.
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= 1
        }));
        reap_tick(
            &driver.session,
            None,
            timeout,
            &driver.takeover_pin,
            &driver.activity,
            &mut state,
        )
        .await;
        assert!(
            driver.session.lock().await.is_some(),
            "a run that became active before the close keeps the browser"
        );
        server.kill();
    }

    #[tokio::test]
    async fn killed_browser_fake_launcher_relaunches_on_next_open() {
        assert_eq!(
            ws_accept("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("launches.txt");
        let launcher = write_fake_launcher(dir.path(), &log);
        let server = FakeCdp::start();
        let browser = Browser::connect(&server.url)
            .await
            .expect("connect fake cdp");
        let page = browser.first_page().await.expect("attach fake page");
        server.kill();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while !browser.client().is_closed() && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let drv = Arc::new(CdpDriver::new(BrowserSettings {
            executable: Some(launcher),
            ..BrowserSettings::isolated(dir.path())
        }));
        *drv.session.lock().await = Some(Session {
            browser,
            page,
            profile: None,
            headless: true,
            last_work: tokio::time::Instant::now(),
            lanes: Vec::new(),
        });
        let tools = tools(drv, dir.path(), None);
        let err = get(&tools, "browser_open")
            .call(
                &ToolContext::default(),
                json!({"url": "https://example.com"}),
            )
            .await
            .expect_err("fake launcher is not a browser");
        let written = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(
            written.contains("launched"),
            "dead session was reused instead of relaunching the fake browser: {err}"
        );
    }

    #[tokio::test]
    async fn browser_modes_keep_concurrent_runs_in_separate_sessions() {
        struct HeadlessControl;

        #[async_trait]
        impl DesktopControlGate for HeadlessControl {
            async fn before_action(&self, _req: &ControlRequest) -> Result<(), String> {
                Ok(())
            }

            fn browser_headless_for_run(&self, run_id: &str) -> Option<bool> {
                match run_id {
                    "run-host" => Some(false),
                    "run-browser" => Some(true),
                    _ => None,
                }
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("launches.txt");
        let launcher = write_fake_launcher(dir.path(), &log);
        let server = FakeCdp::start();
        let browser = Browser::connect(&server.url)
            .await
            .expect("connect fake cdp");
        let page = browser.first_page().await.expect("attach fake page");

        let drv = Arc::new(CdpDriver::new(BrowserSettings {
            executable: Some(launcher),
            headless: true,
            ..BrowserSettings::isolated(dir.path())
        }));
        *drv.session.lock().await = Some(Session {
            browser,
            page,
            profile: None,
            headless: true,
            last_work: tokio::time::Instant::now(),
            lanes: Vec::new(),
        });
        let tools = tools(
            Arc::clone(&drv) as Arc<dyn BrowserDriver>,
            dir.path(),
            Some(Arc::new(HeadlessControl)),
        );
        let open = get(&tools, "browser_open");

        let host_ctx = ToolContext {
            run_id: "run-host".into(),
            ..ToolContext::default()
        };
        let _ = open
            .call(&host_ctx, json!({"url": "https://example.com"}))
            .await
            .expect_err("fake launcher exits without CDP");
        assert!(drv.desired_headless());
        assert!(
            drv.session.lock().await.is_some(),
            "other run's session must survive mode selection"
        );
        let alternate = drv.alternate_mode.lock().clone().unwrap();
        assert!(!alternate.desired_headless());
        assert!(!Arc::ptr_eq(&drv, &alternate));
        assert!(Arc::ptr_eq(&drv.takeover_pin, &alternate.takeover_pin));
        assert!(Arc::ptr_eq(&drv.activity, &alternate.activity));
        let written = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(written.contains("launched"));

        // Returning to headless reuses the original session and mode.
        assert!(drv.for_headless(true).is_none());
        assert!(drv.session.lock().await.is_some());
        server.kill();
    }

    /// A fake browser: writes `DevToolsActivePort` for the profile the driver is
    /// about to use, pointing at the in-process fake CDP server, then idles.
    fn write_cdp_launcher(dir: &Path, profile: &Path, port: u16) -> PathBuf {
        let file = profile.join("DevToolsActivePort").display().to_string();
        assert!(!file.contains('"') && !file.contains('\''));
        #[cfg(windows)]
        {
            let path = dir.join("fake-cdp-browser.cmd");
            std::fs::write(
                &path,
                format!(
                    "@echo off\r\n(echo {port}& echo /devtools/browser/fake)> \"{file}\"\r\nping -n 8 127.0.0.1 >nul\r\n"
                ),
            )
            .unwrap();
            path
        }
        #[cfg(not(windows))]
        {
            use std::os::unix::fs::PermissionsExt;
            let path = dir.join("fake-cdp-browser");
            std::fs::write(
                &path,
                format!(
                    "#!/bin/sh\nprintf '%s\\n/devtools/browser/fake\\n' {port} > '{file}'\nsleep 8\n"
                ),
            )
            .unwrap();
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).unwrap();
            path
        }
    }

    async fn session_for(server: &FakeCdp) -> Session {
        let browser = Browser::connect(&server.url).await.unwrap();
        let page = browser.first_page().await.unwrap();
        Session {
            browser,
            page,
            profile: None,
            headless: true,
            last_work: tokio::time::Instant::now(),
            lanes: Vec::new(),
        }
    }

    fn drain(rx: &mut tokio::sync::broadcast::Receiver<()>) -> usize {
        let mut n = 0;
        while rx.try_recv().is_ok() {
            n += 1;
        }
        n
    }

    /// 244-V #2/#3: a host-desktop run (headed) launches through the alternate
    /// driver. The bot's ONE registered slot must bind to that browser, and the
    /// registry must announce it (register + attach) with no control event.
    #[tokio::test]
    async fn a_headed_host_run_binds_the_bots_registered_view_slot_and_announces_it() {
        struct HostGate;
        #[async_trait]
        impl DesktopControlGate for HostGate {
            async fn before_action(&self, _req: &ControlRequest) -> Result<(), String> {
                Ok(())
            }
            fn browser_headless_for_run(&self, _run_id: &str) -> Option<bool> {
                Some(false)
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let bot = "0194a1c2-0000-7000-8000-0000000000a1";
        let server = FakeCdp::start();
        let port: u16 = server
            .url
            .rsplit(':')
            .next()
            .unwrap()
            .split('/')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        // The headed driver of a managed bot uses `<browsers>/<bot>/headed`.
        let profile = dir.path().join("browsers").join(bot).join("headed");
        let launcher = write_cdp_launcher(dir.path(), &profile, port);
        let registry = Arc::new(live_view::TargetRegistry::default());
        let mut changes = registry.subscribe_changes();
        let tools = managed_tools(
            BrowserSettings {
                executable: Some(launcher),
                headless: true,
                ..BrowserSettings::isolated(dir.path())
            },
            dir.path(),
            Some(Arc::new(HostGate)),
            Some(Arc::clone(&registry)),
        );
        let ctx = ToolContext {
            run_id: "run-host".into(),
            bot_id: Some(bot.into()),
            ..ToolContext::default()
        };
        // The navigation itself may fail against the fake server; the launch and
        // the binding happen before it.
        let _ = get(&tools, "browser_open")
            .call(&ctx, json!({"url": "https://example.com"}))
            .await;
        let targets = registry.targets();
        assert_eq!(targets.len(), 1, "one slot per bot, headless or headed");
        assert_eq!(
            targets[0].unavailable_reason, None,
            "the slot is bound to the browser that actually runs, not left idle"
        );
        assert_eq!(targets[0].bot_id, uuid::Uuid::parse_str(bot).ok());
        assert!(
            drain(&mut changes) >= 2,
            "registering the slot and binding it are both announced"
        );
        server.kill();
    }

    /// 244-V #6: a normal close (idle reclamation, shutdown) returns the slot to
    /// the idle state, announces it, and a relaunch binds it again.
    #[tokio::test]
    async fn closing_the_browser_returns_the_slot_to_idle_and_a_relaunch_rebinds_it() {
        use live_view::{Reason, ViewTarget};
        let dir = tempfile::tempdir().unwrap();
        let changes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&changes);
        let view = browser_cdp::BrowserViewTarget::new_idle(browser_cdp::BrowserViewConfig {
            on_change: Some(Arc::new(move || {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            })),
            ..browser_cdp::BrowserViewConfig::default()
        });
        let mut driver = CdpDriver::new(BrowserSettings::isolated(dir.path()));
        driver.view = Some(view.clone());
        for round in 0..2 {
            let server = FakeCdp::start();
            let session = session_for(&server).await;
            view.attach_page(&session.page).unwrap();
            *driver.session.lock().await = Some(session);
            assert_eq!(view.info().unavailable_reason, None, "round {round}: bound");
            let before = changes.load(std::sync::atomic::Ordering::SeqCst);
            // Idle reclamation: the reaper closes it.
            reap_tick(
                &driver.session,
                driver.view.as_deref(),
                Duration::ZERO,
                &driver.takeover_pin,
                &driver.activity,
                &mut ReapState::default(),
            )
            .await;
            assert!(driver.session.lock().await.is_none());
            assert_eq!(
                view.info().unavailable_reason,
                Some(Reason::BrowserIdle),
                "round {round}: idle, not a generic failure"
            );
            assert!(
                changes.load(std::sync::atomic::Ordering::SeqCst) > before,
                "round {round}: the return to idle is announced"
            );
            server.kill();
        }
        // Shutdown takes the same path.
        let server = FakeCdp::start();
        let session = session_for(&server).await;
        view.attach_page(&session.page).unwrap();
        *driver.session.lock().await = Some(session);
        driver.shutdown().await;
        assert_eq!(view.info().unavailable_reason, Some(Reason::BrowserIdle));
        server.kill();
    }

    /// 244-V #6: create-use-delete cycles leave nothing behind: the registry, the
    /// driver map and the peer map stay empty, and the deleted bot's browser closes.
    #[tokio::test]
    async fn deleting_bots_removes_their_slots_drivers_and_browsers_without_leaking() {
        let dir = tempfile::tempdir().unwrap();
        let settings = BrowserSettings::isolated(dir.path());
        let registry = Arc::new(live_view::TargetRegistry::default());
        let bots = BotDrivers::default();
        let peers = BrowserPeers::default();
        wire_retirement(&registry, &bots, &peers);
        let shared = Arc::new(Shared {
            driver: Arc::new(CdpDriver::new(settings.clone())),
            managed: Some(ManagedBrowsers {
                settings,
                peers: Arc::clone(&peers),
                bots: Arc::clone(&bots),
                views: Some(Arc::clone(&registry)),
            }),
            data_dir: dir.path().into(),
            control: None,
            fill_secrets: None,
            observed_epoch: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        });
        for n in 0..20u128 {
            let id = uuid::Uuid::from_u128(0x0194_a1c2_0000_7000_8000_0000_0000_0000 + n);
            let _bot_shared = shared.for_bot(Some(&id.to_string())).unwrap();
            assert_eq!(registry.targets().len(), 1);
            // The bot used its browser.
            let server = FakeCdp::start();
            let browser = Browser::connect(&server.url).await.unwrap();
            let page = browser.first_page().await.unwrap();
            let cdp = peers
                .lock()
                .get(&id.to_string())
                .and_then(std::sync::Weak::upgrade)
                .expect("the bot's driver");
            *cdp.session.lock().await = Some(Session {
                browser,
                page,
                profile: None,
                headless: true,
                last_work: tokio::time::Instant::now(),
                lanes: Vec::new(),
            });
            // Delete the bot.
            assert_eq!(registry.retire_bot(id), 1);
            for _ in 0..200 {
                if cdp.session.lock().await.is_none() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert!(cdp.session.lock().await.is_none(), "its browser was closed");
            drop(cdp);
            assert!(registry.targets().is_empty(), "slot removed");
            assert!(bots.lock().is_empty(), "driver slot removed");
            assert!(peers.lock().is_empty(), "peer entry removed");
            server.kill();
        }
    }
}
