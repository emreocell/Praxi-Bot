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
    ActionResult, Browser, LaunchOptions, Page, ScreenshotOptions, ScreenshotTarget,
    SnapshotOptions, WaitCondition,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tool_registry::{
    Effect, PermissionCategory, Tool, ToolContext, ToolError, ToolOutput, ToolSpec,
};

use crate::control::{ControlRequest, DesktopControlGate};
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
    /// Whether the browser window is visible on the user's desktop (then its
    /// mutating actions go through the desktop control gate).  Headless and
    /// remote browsers are not.
    fn on_desktop(&self) -> bool {
        false
    }
    /// Navigate to `url` (starting the browser if needed); returns the page snapshot.
    async fn open(&self, url: &str, profile: Option<&str>) -> Result<String, String>;
    /// Current page snapshot with element refs.
    async fn snapshot(&self, interactive_only: bool) -> Result<String, String>;
    /// Click the element `r`.
    async fn click(&self, r: &str) -> Result<String, String>;
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
        }
    }
}

struct Session {
    browser: Browser,
    page: Page,
    profile: Option<String>,
}

/// [`BrowserDriver`] over Chrome/Edge via `browser-cdp`, launched lazily.
pub struct CdpDriver {
    settings: BrowserSettings,
    session: Mutex<Option<Session>>,
}

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

fn render_action(r: &ActionResult) -> Result<String, String> {
    if !r.ok {
        return Err(r.error.clone().unwrap_or_else(|| "action failed".into()));
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
    if let Some(dl) = &r.dialog {
        let _ = write!(
            out,
            "\n~ dialog({}) \"{}\" {}",
            dl.kind,
            dl.message.replace('\n', " "),
            if dl.accepted { "accepted" } else { "dismissed" }
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
            session: Mutex::new(None),
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

    async fn ensure<'a>(
        &self,
        guard: &'a mut Option<Session>,
        profile: Option<&str>,
    ) -> Result<&'a mut Session, String> {
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
            let opts = LaunchOptions {
                executable: self.settings.executable.clone(),
                headless: self.settings.headless,
                user_data_dir: self.profile_path(profile)?,
                ..LaunchOptions::default()
            };
            let browser = Browser::launch(opts).await.map_err(s)?;
            let page = browser.first_page().await.map_err(s)?;
            *guard = Some(Session {
                browser,
                page,
                profile: profile.map(str::to_string),
            });
        }
        Ok(guard.as_mut().expect("session just ensured"))
    }

    /// Close the browser (the next call launches a new one).
    pub async fn shutdown(&self) {
        if let Some(sess) = self.session.lock().await.take() {
            let _ = sess.browser.close().await;
        }
    }
}

#[async_trait]
impl BrowserDriver for CdpDriver {
    fn on_desktop(&self) -> bool {
        !self.settings.headless
    }

    async fn open(&self, url: &str, profile: Option<&str>) -> Result<String, String> {
        let mut g = self.session.lock().await;
        let sess = self.ensure(&mut g, profile).await?;
        // One snapshot, taken with the caller's options (no baseline, no second capture).
        let r = sess
            .page
            .navigate_snapshot(url, &snap_opts(false))
            .await
            .map_err(s)?;
        if !r.ok {
            return Err(r.error.unwrap_or_else(|| "navigation failed".into()));
        }
        Ok(r.snapshot.render())
    }

    async fn snapshot(&self, interactive_only: bool) -> Result<String, String> {
        let mut g = self.session.lock().await;
        let sess = self.ensure(&mut g, None).await?;
        let snap = sess
            .page
            .snapshot(&snap_opts(interactive_only))
            .await
            .map_err(s)?;
        Ok(snap.render())
    }

    async fn click(&self, r: &str) -> Result<String, String> {
        let mut g = self.session.lock().await;
        let sess = self.ensure(&mut g, None).await?;
        render_action(&sess.page.click(r).await.map_err(s)?)
    }

    async fn fill(&self, r: &str, text: &str) -> Result<String, String> {
        let mut g = self.session.lock().await;
        let sess = self.ensure(&mut g, None).await?;
        render_action(&sess.page.fill(r, text).await.map_err(s)?)
    }

    async fn select(&self, r: &str, value: &str) -> Result<String, String> {
        let mut g = self.session.lock().await;
        let sess = self.ensure(&mut g, None).await?;
        render_action(&sess.page.select_option(r, value).await.map_err(s)?)
    }

    async fn press(&self, key: &str) -> Result<String, String> {
        let mut g = self.session.lock().await;
        let sess = self.ensure(&mut g, None).await?;
        render_action(&sess.page.press_key(key).await.map_err(s)?)
    }

    async fn scroll(&self, r: Option<&str>, dx: f64, dy: f64) -> Result<String, String> {
        let mut g = self.session.lock().await;
        let sess = self.ensure(&mut g, None).await?;
        render_action(&sess.page.scroll(r, dx, dy).await.map_err(s)?)
    }

    async fn wait(&self, cond: Wait, timeout: Option<Duration>) -> Result<String, String> {
        let mut g = self.session.lock().await;
        let sess = self.ensure(&mut g, None).await?;
        let c = match cond {
            Wait::Text(t) => WaitCondition::Text(t),
            Wait::Selector(sel) => WaitCondition::Selector(sel),
            Wait::Idle => WaitCondition::NetworkIdle,
        };
        render_action(&sess.page.wait_for(&c, timeout).await.map_err(s)?)
    }

    async fn back(&self) -> Result<String, String> {
        let mut g = self.session.lock().await;
        let sess = self.ensure(&mut g, None).await?;
        render_action(&sess.page.back().await.map_err(s)?)
    }

    async fn tabs(&self, switch_to: Option<usize>) -> Result<String, String> {
        let mut g = self.session.lock().await;
        let sess = self.ensure(&mut g, None).await?;
        let pages = sess.browser.pages().await.map_err(s)?;
        if let Some(n) = switch_to {
            let t = n
                .checked_sub(1)
                .and_then(|i| pages.get(i))
                .ok_or_else(|| format!("no tab number {n}"))?;
            sess.page = sess.browser.attach(&t.target_id).await.map_err(s)?;
            sess.page.bring_to_front().await.map_err(s)?;
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

    async fn screenshot(
        &self,
        full_page: bool,
        element: Option<&str>,
    ) -> Result<(Vec<u8>, &'static str), String> {
        let mut g = self.session.lock().await;
        let sess = self.ensure(&mut g, None).await?;
        let target = match (element, full_page) {
            (Some(r), _) => ScreenshotTarget::Element(r.to_string()),
            (None, true) => ScreenshotTarget::FullPage,
            (None, false) => ScreenshotTarget::Viewport,
        };
        let bytes = sess
            .page
            .screenshot(&ScreenshotOptions {
                target,
                jpeg_quality: Some(80),
            })
            .await
            .map_err(s)?;
        Ok((bytes, "jpg"))
    }

    async fn eval(&self, js: &str) -> Result<Value, String> {
        let mut g = self.session.lock().await;
        let sess = self.ensure(&mut g, None).await?;
        sess.page.evaluate(js).await.map_err(s)
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
                    "profile":{"type":"string","description":"Profile name; default is the isolated profile. Only honoured when the browser starts."}
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
                json!({"type":"object","properties":{"ref":ref_prop},"required":["ref"]}),
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
                json!({"type":"object","properties":{}}),
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
                Effect::Read,
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
}

#[derive(Deserialize)]
struct SnapIn {
    #[serde(default)]
    interactive_only: bool,
}

#[derive(Deserialize)]
struct RefIn {
    r#ref: String,
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
    async fn gate(&self, run_id: &str, action: &str, label: String) -> Result<(), ToolError> {
        let Some(gate) = self
            .shared
            .control
            .as_ref()
            .filter(|_| self.shared.driver.on_desktop())
        else {
            return Ok(());
        };
        gate.before_action(&ControlRequest {
            run_id: run_id.to_owned(),
            rect: None,
            label,
            action: action.to_owned(),
        })
        .await
        .map_err(exec_err)
    }

    async fn run(&self, run_id: &str, input: Value) -> Result<String, ToolError> {
        let d = &self.shared.driver;
        let text = match self.op {
            Op::Open => {
                let i: OpenIn = parse(input)?;
                check_open_url(&i.url)?;
                self.gate(run_id, "open", i.url.clone()).await?;
                d.open(&i.url, i.profile.as_deref()).await
            }
            Op::Snapshot => {
                let i: SnapIn = parse(input)?;
                d.snapshot(i.interactive_only).await
            }
            Op::Click => {
                let i: RefIn = parse(input)?;
                self.gate(run_id, "click", i.r#ref.clone()).await?;
                d.click(&i.r#ref).await
            }
            Op::Fill => {
                let i: FillIn = parse(input)?;
                self.gate(run_id, "type", i.r#ref.clone()).await?;
                d.fill(&i.r#ref, &i.text).await
            }
            Op::Select => {
                let i: SelectIn = parse(input)?;
                self.gate(run_id, "click", i.r#ref.clone()).await?;
                d.select(&i.r#ref, &i.value).await
            }
            Op::Press => {
                let i: PressIn = parse(input)?;
                self.gate(run_id, "key", i.key.clone()).await?;
                d.press(&i.key).await
            }
            Op::Scroll => {
                let i: ScrollIn = parse(input)?;
                self.gate(run_id, "scroll", i.r#ref.clone().unwrap_or_default())
                    .await?;
                d.scroll(i.r#ref.as_deref(), i.dx, i.dy).await
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
                self.gate(run_id, "key", "back".to_owned()).await?;
                d.back().await
            }
            Op::Tabs => {
                let i: TabsIn = parse(input)?;
                d.tabs(i.switch_to).await
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
                d.eval(&i.js).await.map(|v| v.to_string())
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

    /// Records calls as `name:args` strings and returns canned text.
    #[derive(Default)]
    pub struct FakeBrowser {
        pub calls: PMutex<Vec<String>>,
    }

    impl FakeBrowser {
        fn rec(&self, s: &str) -> Result<String, String> {
            self.calls.lock().push(s.to_string());
            if s.contains("e404") {
                Err("unknown ref e404".into())
            } else {
                Ok(format!("ok {s}"))
            }
        }
    }

    #[async_trait]
    impl BrowserDriver for FakeBrowser {
        async fn open(&self, url: &str, profile: Option<&str>) -> Result<String, String> {
            self.rec(&format!("open:{url}:{}", profile.unwrap_or("-")))
        }
        async fn snapshot(&self, i: bool) -> Result<String, String> {
            self.rec(&format!("snapshot:{i}"))
        }
        async fn click(&self, r: &str) -> Result<String, String> {
            self.rec(&format!("click:{r}"))
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
        async fn back(&self) -> Result<String, String> {
            self.rec("back")
        }
        async fn tabs(&self, s: Option<usize>) -> Result<String, String> {
            self.rec(&format!("tabs:{s:?}"))
        }
        async fn screenshot(
            &self,
            full: bool,
            _e: Option<&str>,
        ) -> Result<(Vec<u8>, &'static str), String> {
            self.calls.lock().push(format!("screenshot:{full}"));
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
            dialog: None,
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
        r.dialog = Some(browser_cdp::DialogInfo {
            kind: "confirm".into(),
            message: "sure?".into(),
            accepted: false,
        });
        r.settle_note = Some("network still busy: 2 requests".into());
        let out = render_action(&r).unwrap();
        assert!(
            out.contains("\n~ dialog(confirm) \"sure?\" dismissed"),
            "{out}"
        );
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
}
