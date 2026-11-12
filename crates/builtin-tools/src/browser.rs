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
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tool_registry::{
    Effect, PermissionCategory, Tool, ToolContext, ToolError, ToolOutput, ToolSpec,
};

use crate::control::{ActionPermit, ControlRequest, DesktopControlGate};
use crate::util::{exec_err, finish_text, parse, save_file, spec};

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
    /// Isolated, headless defaults with named profiles under `<data_dir>/browser-profiles`.
    #[must_use]
    pub fn isolated(data_dir: &Path) -> Self {
        Self {
            executable: None,
            headless: true,
            profile_dir: None,
            profiles_root: data_dir.join("browser-profiles"),
            idle_timeout: Duration::from_secs(120),
        }
    }
}

struct Session {
    browser: Browser,
    page: Page,
    profile: Option<String>,
    last_work: tokio::time::Instant,
}

/// [`BrowserDriver`] over Chrome/Edge via `browser-cdp`, launched lazily.
pub struct CdpDriver {
    settings: BrowserSettings,
    session: Arc<Mutex<Option<Session>>>,
    takeover_pin: Arc<std::sync::atomic::AtomicBool>,
    /// True while a bot run that used the computer is still active.
    activity: Arc<std::sync::RwLock<Option<ActivityProbe>>>,
    reaper_started: std::sync::atomic::AtomicBool,
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
            session: Arc::new(Mutex::new(None)),
            takeover_pin: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            activity: Arc::new(std::sync::RwLock::new(None)),
            reaper_started: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn profile_path(&self, profile: Option<&str>) -> Result<Option<PathBuf>, String> {
        match profile {
            None | Some("isolated") => Ok(self.settings.profile_dir.clone()),
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
        let opts = LaunchOptions {
            executable: self.settings.executable.clone(),
            headless: self.settings.headless,
            user_data_dir: self.profile_path(profile)?,
            ..LaunchOptions::default()
        };
        let browser = Browser::launch(opts).await.map_err(s)?;
        let page = browser.first_page().await.map_err(s)?;
        Ok(Session {
            browser,
            page,
            profile: profile.map(str::to_string),
            last_work: tokio::time::Instant::now(),
        })
    }

    async fn ensure<'a>(
        &self,
        guard: &'a mut Option<Session>,
        profile: Option<&str>,
        allow_launch: bool,
    ) -> Result<&'a mut Session, String> {
        self.start_idle_reaper();
        // A crashed or closed browser must not stay cached until daemon restart.
        if guard
            .as_ref()
            .is_some_and(|sess| !Self::session_alive(sess))
        {
            *guard = None;
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
                reap_tick(&session, timeout, &pin, &activity, &mut state).await;
            }
        });
    }

    /// Close the browser (the next call launches a new one).
    pub async fn shutdown(&self) {
        if let Some(sess) = self.session.lock().await.take() {
            let _ = sess.browser.close().await;
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
        let _ = s.browser.close().await;
    }
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
            let mut guard = $self.session.lock().await;
            let step = {
                let $sess = $self.ensure(&mut guard, $profile, $allow_launch).await?;
                $body
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
        !self.settings.headless
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
            sess.page.click_with(r, &dialog_opts).await
        })?;
        render_action(&acted)
    }

    async fn click(&self, r: &str) -> Result<String, String> {
        self.click_opts(r, None, None).await
    }

    async fn fill(&self, r: &str, text: &str) -> Result<String, String> {
        let acted = with_session!(self, None, |sess| { sess.page.fill(r, text).await })?;
        render_action(&acted)
    }

    async fn select(&self, r: &str, value: &str) -> Result<String, String> {
        let acted = with_session!(self, None, |sess| {
            sess.page.select_option(r, value).await
        })?;
        render_action(&acted)
    }

    async fn press(&self, key: &str) -> Result<String, String> {
        let acted = with_session!(self, None, |sess| { sess.page.press_key(key).await })?;
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
                let pages = sess.browser.pages().await?;
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
    data_dir: PathBuf,
    control: Option<Arc<dyn DesktopControlGate>>,
    observed_epoch: std::sync::atomic::AtomicU64,
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
    let shared = Arc::new(Shared {
        driver,
        data_dir: data_dir.to_path_buf(),
        control,
        observed_epoch: std::sync::atomic::AtomicU64::new(0),
    });
    let b = PermissionCategory::Browser;
    let ref_prop =
        json!({"type":"string","description":"Element ref from browser_snapshot, like e12."});
    let defs: Vec<(Op, ToolSpec)> = vec![
        (
            Op::Open,
            spec(
                "browser_open",
                "Open a URL in the browser and return the page snapshot. Example: {\"url\":\"https://example.com\"}",
                json!({"type":"object","properties":{
                    "url":{"type":"string","description":"Full URL."},
                    "profile":{"type":"string","description":"Profile name; default is the isolated profile. Only honoured when the browser starts."},
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
                "Read the page as lines like [e12] button \"Sign in\". Use the refs in other browser tools. Example: {\"interactive_only\":true}",
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
                "Click an element. Returns what changed. Example: {\"ref\":\"e12\"}",
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
                "Replace the text of an input. Example: {\"ref\":\"e5\",\"text\":\"hello\"}",
                json!({"type":"object","properties":{
                    "ref":ref_prop,
                    "text":{"type":"string","description":"New text."}
                },"required":["ref","text"]}),
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
                "List open tabs (current is marked *), or switch with switch_to. Example: {\"switch_to\":2}",
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
                "Run JavaScript in the page and return the JSON result. Last resort. Example: {\"js\":\"document.title\"}",
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
    text: String,
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
        if observing
            && let (Some(gate), Some(epoch)) = (gate, epoch)
            && (gate.observation_epoch() != epoch || gate.observation_refused().is_some())
        {
            return Err(exec_err(crate::control::OBSERVATION_DISCARDED));
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
                permit
                    .protect(async move { d.fill(&i.r#ref, &i.text).await })
                    .await
                    .map_err(exec_err)?
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

#[async_trait]
impl Tool for BrowserTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolContext, input: Value) -> Result<ToolOutput, ToolError> {
        let text = self.run(&ctx.run_id, input).await?;
        Ok(finish_text(ctx, &self.shared.data_dir.join("spill"), text))
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
        /// Runs inside every backend call (a human takeover landing mid-call).
        pub hook: PMutex<Option<FakeHook>>,
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
            last_work: tokio::time::Instant::now(),
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
            last_work: tokio::time::Instant::now(),
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
            last_work: tokio::time::Instant::now(),
        });
        tokio::time::pause();
        let timeout = Duration::from_millis(40);
        let mut state = ReapState::default();
        driver.set_takeover_pin(true);
        tokio::time::advance(Duration::from_secs(1)).await;
        reap_tick(
            &driver.session,
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
            last_work: tokio::time::Instant::now(),
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
}
