//! Desktop tools (permission category `Desktop`) over any [`DesktopBackend`].
//!
//! * Every mutating tool returns the refreshed snapshot.
//! * Keyboard tools need an element: `desktop_type` focuses the element first,
//!   there is no blind typing into an unknown focus.
//! * When the accessibility tree is `Weak`, the state also lists OCR text boxes
//!   with set-of-marks numbers; `index: "mark:<n>"` clicks a mark by coordinates.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use desktop_api::snapshot::AppSpec;
use desktop_api::{
    Action, ClickMethod, CoordinateSpace, DesktopBackend, DesktopError, Quality, Region, Snapshot,
    Target, TreeQuality, render_snapshot,
};
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{Value, json};
use tool_registry::{
    Effect, PermissionCategory, Tool, ToolContext, ToolError, ToolOutput, ToolSpec,
};
use vision::{BoundingBox, OcrEngine};

use crate::util::{exec_err, finish_text, parse, save_file, spec};

/// An OCR engine usable from tools.
pub type SharedOcr = Arc<dyn OcrEngine + Send + Sync>;

/// Timing knobs for the desktop tools.
#[derive(Debug, Clone, Copy)]
pub struct DesktopSettings {
    /// Wait after an action before the refreshed snapshot is taken.
    pub action_settle: Duration,
    /// Wait after launching an app before windows are listed.
    pub launch_settle: Duration,
}

impl Default for DesktopSettings {
    fn default() -> Self {
        Self {
            action_settle: Duration::from_millis(300),
            launch_settle: Duration::from_millis(1500),
        }
    }
}

const MAX_MARKS: usize = 60;
const MAX_WINDOWS_SHOWN: usize = 40;

#[derive(Default)]
struct State {
    window_id: Option<u64>,
    generation: Option<u64>,
    /// Mark number to absolute screen centre.
    marks: BTreeMap<u32, (f64, f64)>,
}

struct Shared {
    backend: Arc<dyn DesktopBackend>,
    ocr: Option<SharedOcr>,
    state: Mutex<State>,
    data_dir: PathBuf,
    settings: DesktopSettings,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    ListApps,
    Launch,
    GetState,
    Click,
    Type,
    SetValue,
    Key,
    Scroll,
    Drag,
}

/// One desktop tool.
pub struct DesktopTool {
    spec: ToolSpec,
    op: Op,
    shared: Arc<Shared>,
}

/// An element reference as given by the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ElementRef {
    Index(u32),
    Mark(u32),
}

fn parse_ref(v: &Value) -> Result<ElementRef, ToolError> {
    let bad = || {
        ToolError::InvalidInput(
            "index must be a number from desktop_get_state or \"mark:<n>\"".into(),
        )
    };
    match v {
        Value::Number(n) => n
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .map(ElementRef::Index)
            .ok_or_else(bad),
        Value::String(s) => {
            let s = s.trim();
            if let Some(m) = s.strip_prefix("mark:") {
                m.trim().parse().map(ElementRef::Mark).map_err(|_| bad())
            } else {
                s.parse().map(ElementRef::Index).map_err(|_| bad())
            }
        }
        _ => Err(bad()),
    }
}

fn derr(e: &DesktopError) -> ToolError {
    let hint = match e {
        DesktopError::ElementNotFound { .. } => {
            " (the index is stale; call desktop_get_state and use the new indexes)"
        }
        _ => "",
    };
    exec_err(format!("{e}{hint}"))
}

impl Shared {
    fn target(&self, r: ElementRef) -> Result<Target, ToolError> {
        let st = self.state.lock();
        match r {
            ElementRef::Index(index) => {
                let generation = st
                    .generation
                    .ok_or_else(|| exec_err("no snapshot yet; call desktop_get_state first"))?;
                Ok(Target::ByIndex { index, generation })
            }
            ElementRef::Mark(n) => {
                let (x, y) = st.marks.get(&n).copied().ok_or_else(|| {
                    exec_err(format!(
                        "mark:{n} is unknown; marks exist only after desktop_get_state on a window with a weak tree"
                    ))
                })?;
                Ok(Target::ByCoordinate {
                    x,
                    y,
                    space: CoordinateSpace::Absolute,
                })
            }
        }
    }

    fn default_method(r: ElementRef) -> ClickMethod {
        match r {
            ElementRef::Index(_) => ClickMethod::Accessibility,
            ElementRef::Mark(_) => ClickMethod::Global,
        }
    }

    fn scope(&self) -> Option<u64> {
        self.state.lock().window_id
    }

    async fn act(&self, action: Action) -> Result<String, ToolError> {
        let res = self.backend.act(action).await.map_err(|e| derr(&e))?;
        let mut head = format!("dispatched: {}", res.dispatched);
        if let Some(v) = res.verified {
            let _ = write!(head, ", verified: {v}");
        }
        Ok(head)
    }

    /// Take a fresh snapshot and render it (with marks when the tree is weak).
    async fn refresh(&self, scope: Option<u64>) -> Result<String, ToolError> {
        let snap = self.backend.snapshot(scope).await.map_err(|e| derr(&e))?;
        {
            let mut st = self.state.lock();
            st.window_id = Some(snap.window.id);
            st.generation = Some(snap.generation);
            st.marks.clear();
        }
        let mut text = format!("window_id: {}\n", snap.window.id);
        text.push_str(&render_snapshot(&snap));
        if snap.quality == TreeQuality::Weak {
            text.push_str(&self.marks_section(&snap).await);
        }
        Ok(text)
    }

    async fn marks_section(&self, snap: &Snapshot) -> String {
        let Some(ocr) = self.ocr.clone() else {
            return "\n(accessibility tree is weak and no OCR engine is configured)\n".into();
        };
        if !self.backend.capabilities().screenshot {
            return "\n(accessibility tree is weak; screenshots are unavailable)\n".into();
        }
        match self.build_marks(snap, ocr).await {
            Ok(s) => s,
            Err(e) => format!("\n(accessibility tree is weak; OCR failed: {e})\n"),
        }
    }

    async fn build_marks(&self, snap: &Snapshot, ocr: SharedOcr) -> Result<String, String> {
        let bounds = snap.window.bounds;
        let image = self
            .backend
            .screenshot(Region::Rect(bounds), Quality::Lossless)
            .await
            .map_err(|e| e.to_string())?;
        let rgb = image::load_from_memory(&image.data)
            .map_err(|e| e.to_string())?
            .to_rgb8();
        let (w, h) = rgb.dimensions();
        let (rgb, boxes) = tokio::task::spawn_blocking(move || {
            let mut boxes = ocr.recognize(&rgb);
            boxes.retain(|b| b.bbox.clamp_to(w, h).is_some());
            boxes.truncate(MAX_MARKS);
            (rgb, boxes)
        })
        .await
        .map_err(|e| e.to_string())?;
        if boxes.is_empty() {
            return Ok("\n(accessibility tree is weak; OCR found no text)\n".into());
        }
        let regions: Vec<BoundingBox> = boxes.iter().map(|b| b.bbox).collect();
        let marked = vision::annotate_marks(&rgb, &regions);
        let mut out = String::from("\nOCR marks (use index \"mark:<n>\"):\n");
        {
            let mut st = self.state.lock();
            for (label, bb) in &marked.marks {
                let (cx, cy) = bb.center();
                let ax = bounds.x + f64::from(cx);
                let ay = bounds.y + f64::from(cy);
                st.marks.insert(*label, (ax, ay));
                let text = usize::try_from(*label)
                    .ok()
                    .and_then(|l| l.checked_sub(1))
                    .and_then(|i| boxes.get(i))
                    .map_or("", |b| b.text.as_str());
                let _ = writeln!(
                    out,
                    "mark:{label} \"{}\" ({:.0},{:.0})",
                    text.chars().take(80).collect::<String>().replace('"', "'"),
                    ax,
                    ay
                );
            }
        }
        if let Ok(png) = vision::encode_png(&marked.image)
            && let Ok(path) = save_file(&self.data_dir.join("screenshots"), "png", &png.bytes)
        {
            let _ = writeln!(out, "marked screenshot: {path}");
        }
        Ok(out)
    }

    async fn settle_and_refresh(&self, prefix: String) -> Result<String, ToolError> {
        if !self.settings.action_settle.is_zero() {
            tokio::time::sleep(self.settings.action_settle).await;
        }
        let scope = self.scope();
        let state = self.refresh(scope).await?;
        Ok(format!("{prefix}\n{state}"))
    }
}

/// Build the desktop tools.
#[must_use]
pub fn tools(
    backend: Arc<dyn DesktopBackend>,
    ocr: Option<SharedOcr>,
    data_dir: &Path,
    settings: DesktopSettings,
) -> Vec<Arc<dyn Tool>> {
    let shared = Arc::new(Shared {
        backend,
        ocr,
        state: Mutex::new(State::default()),
        data_dir: data_dir.to_path_buf(),
        settings,
    });
    let d = PermissionCategory::Desktop;
    let index_prop = json!({
        "description": "Element index from desktop_get_state, or \"mark:<n>\" for an OCR mark.",
        "oneOf": [{"type":"integer","minimum":0},{"type":"string","pattern":"^(mark:)?[0-9]+$"}]
    });
    let method_prop = json!({
        "type":"string",
        "enum":["accessibility","app_post","global"],
        "description":"How to deliver the input. Default accessibility for indexes; global (moves the real cursor) for marks."
    });
    let defs: Vec<(Op, ToolSpec)> = vec![
        (
            Op::ListApps,
            spec(
                "desktop_list_apps",
                "List visible windows (open apps, programs) with id, pid and title. Example: {}",
                json!({"type":"object","properties":{}}),
                Effect::Read,
                d,
            ),
        ),
        (
            Op::Launch,
            spec(
                "desktop_launch",
                "Start, open or run an application or program (launch an app, open a file with it) and list windows. Example: {\"executable\":\"notepad.exe\"}",
                json!({"type":"object","properties":{
                    "executable":{"type":"string","description":"Program name or path."},
                    "args":{"type":"array","items":{"type":"string"},"description":"Arguments."}
                },"required":["executable"]}),
                Effect::Write,
                d,
            ),
        ),
        (
            Op::GetState,
            spec(
                "desktop_get_state",
                "Read the UI tree of a window as numbered elements. Use these numbers as index in other desktop tools. Example: {\"title\":\"Notepad\"}",
                json!({"type":"object","properties":{
                    "window_id":{"type":"integer","description":"Window id from desktop_list_apps."},
                    "title":{"type":"string","description":"Part of the window title."},
                    "screenshot":{"type":"boolean","description":"Also save a screenshot and return its file path."}
                }}),
                Effect::Read,
                d,
            ),
        ),
        (
            Op::Click,
            spec(
                "desktop_click",
                "Click a button, link or other element (mouse click). Returns the refreshed UI tree. Example: {\"index\":12}",
                json!({"type":"object","properties":{"index":index_prop,"method":method_prop},"required":["index"]}),
                Effect::Write,
                d,
            ),
        ),
        (
            Op::Type,
            spec(
                "desktop_type",
                "Click an element to focus it, then type text with the keyboard (enter text). Returns the refreshed UI tree. Example: {\"index\":5,\"text\":\"hello\"}",
                json!({"type":"object","properties":{
                    "index":index_prop,
                    "text":{"type":"string","description":"Text to type."}
                },"required":["index","text"]}),
                Effect::Write,
                d,
            ),
        ),
        (
            Op::SetValue,
            spec(
                "desktop_set_value",
                "Replace the value of a text field or combo box. Example: {\"index\":5,\"value\":\"hello\"}",
                json!({"type":"object","properties":{
                    "index":index_prop,
                    "value":{"type":"string","description":"New value."}
                },"required":["index","value"]}),
                Effect::Write,
                d,
            ),
        ),
        (
            Op::Key,
            spec(
                "desktop_key",
                "Press a keyboard key or shortcut chord (Enter, Tab, Ctrl+S, Ctrl+Shift+N), optionally after focusing an element. Example: {\"keys\":[\"ctrl\",\"s\"]}",
                json!({"type":"object","properties":{
                    "keys":{"description":"Key names, e.g. [\"ctrl\",\"s\"], [\"Return\"] or \"ctrl+s\".","oneOf":[
                        {"type":"array","items":{"type":"string"},"minItems":1},{"type":"string"}]},
                    "index":index_prop
                },"required":["keys"]}),
                Effect::Write,
                d,
            ),
        ),
        (
            Op::Scroll,
            spec(
                "desktop_scroll",
                "Scroll an element (positive dy = down). Example: {\"index\":3,\"dy\":5}",
                json!({"type":"object","properties":{
                    "index":index_prop,
                    "x":{"type":"number","description":"Screen x (when no index)."},
                    "y":{"type":"number","description":"Screen y (when no index)."},
                    "dx":{"type":"number","description":"Horizontal amount."},
                    "dy":{"type":"number","description":"Vertical amount."}
                }}),
                Effect::Read,
                d,
            ),
        ),
        (
            Op::Drag,
            spec(
                "desktop_drag",
                "Drag from one element to another (moves the real cursor). Example: {\"from\":3,\"to\":9}",
                json!({"type":"object","properties":{
                    "from":index_prop,
                    "to":index_prop,
                    "method":method_prop
                },"required":["from","to"]}),
                Effect::Write,
                d,
            ),
        ),
    ];
    defs.into_iter()
        .map(|(op, spec)| {
            Arc::new(DesktopTool {
                spec,
                op,
                shared: shared.clone(),
            }) as Arc<dyn Tool>
        })
        .collect()
}

fn parse_method(m: Option<&str>, default: ClickMethod) -> Result<ClickMethod, ToolError> {
    match m {
        None => Ok(default),
        Some("accessibility") => Ok(ClickMethod::Accessibility),
        Some("app_post") => Ok(ClickMethod::AppPost),
        Some("global") => Ok(ClickMethod::Global),
        Some(o) => Err(ToolError::InvalidInput(format!("unknown method {o:?}"))),
    }
}

#[derive(Deserialize)]
struct StateIn {
    window_id: Option<u64>,
    title: Option<String>,
    #[serde(default)]
    screenshot: bool,
}

#[derive(Deserialize)]
struct LaunchIn {
    executable: String,
    #[serde(default)]
    args: Vec<String>,
}

#[derive(Deserialize)]
struct ClickIn {
    index: Value,
    method: Option<String>,
}

#[derive(Deserialize)]
struct TypeIn {
    index: Value,
    text: String,
}

#[derive(Deserialize)]
struct SetValueIn {
    index: Value,
    value: String,
}

#[derive(Deserialize)]
struct KeyIn {
    keys: Value,
    index: Option<Value>,
}

#[derive(Deserialize)]
struct ScrollIn {
    index: Option<Value>,
    x: Option<f64>,
    y: Option<f64>,
    #[serde(default)]
    dx: f64,
    #[serde(default)]
    dy: f64,
}

#[derive(Deserialize)]
struct DragIn {
    from: Value,
    to: Value,
    method: Option<String>,
}

fn parse_keys(v: &Value) -> Result<Vec<String>, ToolError> {
    let keys: Vec<String> = match v {
        Value::String(s) => s
            .split('+')
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty())
            .collect(),
        Value::Array(a) => a
            .iter()
            .map(|k| {
                k.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| ToolError::InvalidInput("keys must be strings".into()))
            })
            .collect::<Result<_, _>>()?,
        _ => {
            return Err(ToolError::InvalidInput(
                "keys must be a list or string".into(),
            ));
        }
    };
    if keys.is_empty() {
        return Err(ToolError::InvalidInput("keys is empty".into()));
    }
    Ok(keys)
}

fn window_line(w: &desktop_api::WindowInfo) -> String {
    format!(
        "id={} pid={} {}\"{}\"",
        w.id,
        w.pid,
        if w.focused { "focused " } else { "" },
        w.title.replace('"', "'")
    )
}

impl DesktopTool {
    async fn run(&self, input: Value) -> Result<String, ToolError> {
        let sh = &self.shared;
        match self.op {
            Op::ListApps => {
                let wins = sh.backend.windows().await.map_err(|e| derr(&e))?;
                if wins.is_empty() {
                    return Ok("no visible windows".into());
                }
                Ok(wins
                    .iter()
                    .take(MAX_WINDOWS_SHOWN)
                    .map(window_line)
                    .collect::<Vec<_>>()
                    .join("\n"))
            }
            Op::Launch => {
                let i: LaunchIn = parse(input)?;
                let pid = sh
                    .backend
                    .launch(AppSpec {
                        executable: i.executable,
                        args: i.args,
                    })
                    .await
                    .map_err(|e| derr(&e))?;
                if !sh.settings.launch_settle.is_zero() {
                    tokio::time::sleep(sh.settings.launch_settle).await;
                }
                let wins = sh.backend.windows().await.map_err(|e| derr(&e))?;
                let mut out = format!("launched pid={pid}\nwindows:\n");
                for w in wins.iter().take(MAX_WINDOWS_SHOWN) {
                    let _ = writeln!(out, "{}", window_line(w));
                }
                Ok(out)
            }
            Op::GetState => {
                let i: StateIn = parse(input)?;
                let scope = if let Some(id) = i.window_id {
                    Some(id)
                } else if let Some(t) = &i.title {
                    let needle = t.to_lowercase();
                    let wins = sh.backend.windows().await.map_err(|e| derr(&e))?;
                    let found = wins
                        .iter()
                        .find(|w| w.title.to_lowercase().contains(&needle))
                        .ok_or_else(|| {
                            exec_err(format!(
                                "no window title contains {t:?}; use desktop_list_apps"
                            ))
                        })?;
                    Some(found.id)
                } else {
                    sh.scope()
                };
                let mut text = sh.refresh(scope).await?;
                if i.screenshot {
                    let region =
                        scope.map_or(Region::FullScreen, |window_id| Region::Window { window_id });
                    let img = sh
                        .backend
                        .screenshot(region, Quality::Medium)
                        .await
                        .map_err(|e| derr(&e))?;
                    let ext = if img.mime_type == "image/png" {
                        "png"
                    } else {
                        "jpg"
                    };
                    let path = save_file(&sh.data_dir.join("screenshots"), ext, &img.data)?;
                    let _ = writeln!(text, "screenshot: {path} ({}x{})", img.width, img.height);
                }
                Ok(text)
            }
            Op::Click => {
                let i: ClickIn = parse(input)?;
                let r = parse_ref(&i.index)?;
                let method = parse_method(i.method.as_deref(), Shared::default_method(r))?;
                let target = sh.target(r)?;
                let head = sh.act(Action::Click { target, method }).await?;
                sh.settle_and_refresh(head).await
            }
            Op::Type => {
                let i: TypeIn = parse(input)?;
                let r = parse_ref(&i.index)?;
                let target = sh.target(r)?;
                // Focus the element with a real click, then type into the focus.
                sh.act(Action::Click {
                    target,
                    method: ClickMethod::Global,
                })
                .await?;
                let head = sh.act(Action::TypeText { text: i.text }).await?;
                sh.settle_and_refresh(head).await
            }
            Op::SetValue => {
                let i: SetValueIn = parse(input)?;
                let r = parse_ref(&i.index)?;
                let target = sh.target(r)?;
                let head = sh
                    .act(Action::SetValue {
                        target,
                        value: i.value,
                    })
                    .await?;
                sh.settle_and_refresh(head).await
            }
            Op::Key => {
                let i: KeyIn = parse(input)?;
                let keys = parse_keys(&i.keys)?;
                if let Some(idx) = &i.index {
                    let target = sh.target(parse_ref(idx)?)?;
                    sh.act(Action::Click {
                        target,
                        method: ClickMethod::Global,
                    })
                    .await?;
                }
                let head = sh.act(Action::KeyPress { keys }).await?;
                sh.settle_and_refresh(head).await
            }
            Op::Scroll => {
                let i: ScrollIn = parse(input)?;
                let target = match (&i.index, i.x, i.y) {
                    (Some(idx), _, _) => sh.target(parse_ref(idx)?)?,
                    (None, Some(x), Some(y)) => Target::ByCoordinate {
                        x,
                        y,
                        space: CoordinateSpace::Absolute,
                    },
                    _ => {
                        return Err(ToolError::InvalidInput(
                            "give index, or both x and y".into(),
                        ));
                    }
                };
                let head = sh
                    .act(Action::Scroll {
                        target,
                        delta_x: i.dx,
                        delta_y: i.dy,
                    })
                    .await?;
                sh.settle_and_refresh(head).await
            }
            Op::Drag => {
                let i: DragIn = parse(input)?;
                let (a, b) = (parse_ref(&i.from)?, parse_ref(&i.to)?);
                let method = parse_method(i.method.as_deref(), ClickMethod::Global)?;
                let (from, to) = (sh.target(a)?, sh.target(b)?);
                let head = sh.act(Action::Drag { from, to, method }).await?;
                sh.settle_and_refresh(head).await
            }
        }
    }
}

#[async_trait]
impl Tool for DesktopTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolContext, input: Value) -> Result<ToolOutput, ToolError> {
        let text = self.run(input).await?;
        Ok(finish_text(ctx, &self.shared.data_dir.join("spill"), text))
    }
}

#[cfg(test)]
pub(crate) mod fake {
    //! A scripted in-memory desktop used by unit tests.

    use super::*;
    use desktop_api::snapshot::{Image, Pid, Rect, UiNode, WindowInfo};
    use desktop_api::{ActionResult, Capabilities};

    pub struct FakeDesktop {
        pub actions: Mutex<Vec<Action>>,
        pub generation: Mutex<u64>,
        pub weak: bool,
        pub screenshot_png: Vec<u8>,
        pub typed: Mutex<String>,
    }

    impl FakeDesktop {
        pub fn new(weak: bool) -> Self {
            Self {
                actions: Mutex::new(Vec::new()),
                generation: Mutex::new(0),
                weak,
                screenshot_png: Vec::new(),
                typed: Mutex::new(String::new()),
            }
        }
    }

    fn window() -> WindowInfo {
        WindowInfo {
            id: 77,
            title: "Fake Notepad".into(),
            pid: 4242,
            bounds: Rect {
                x: 100.0,
                y: 200.0,
                width: 400.0,
                height: 300.0,
            },
            focused: true,
        }
    }

    #[async_trait]
    impl DesktopBackend for FakeDesktop {
        async fn snapshot(&self, _scope: Option<u64>) -> Result<Snapshot, DesktopError> {
            let mut g = self.generation.lock();
            *g += 1;
            let value = self.typed.lock().clone();
            let nodes = if self.weak {
                vec![UiNode::new(0, "pane", "", None, None, vec![])]
            } else {
                (0..6)
                    .map(|i| {
                        UiNode::new(
                            i,
                            if i == 3 { "edit" } else { "button" },
                            format!("el{i}"),
                            (i == 3).then(|| value.clone()),
                            None,
                            vec![],
                        )
                    })
                    .collect()
            };
            let quality = TreeQuality::assess(&nodes);
            Ok(Snapshot {
                window: window(),
                nodes,
                generation: *g,
                quality,
            })
        }

        async fn act(&self, action: Action) -> Result<ActionResult, DesktopError> {
            let current = *self.generation.lock();
            let stale = |t: &Target| match t {
                Target::ByIndex { index, generation } if *generation != current => {
                    Some(DesktopError::ElementNotFound {
                        index: *index,
                        generation: *generation,
                    })
                }
                _ => None,
            };
            match &action {
                Action::Click { target, .. } | Action::SetValue { target, .. } => {
                    if let Some(e) = stale(target) {
                        return Err(e);
                    }
                }
                Action::TypeText { text } => self.typed.lock().push_str(text),
                _ => {}
            }
            if let Action::SetValue { value, .. } = &action {
                *self.typed.lock() = value.clone();
            }
            self.actions.lock().push(action);
            Ok(ActionResult {
                dispatched: true,
                verified: None,
                new_snapshot: None,
            })
        }

        async fn screenshot(&self, _r: Region, _q: Quality) -> Result<Image, DesktopError> {
            Ok(Image {
                data: self.screenshot_png.clone(),
                width: 10,
                height: 10,
                mime_type: "image/png".into(),
            })
        }

        async fn windows(&self) -> Result<Vec<WindowInfo>, DesktopError> {
            Ok(vec![window()])
        }

        async fn launch(&self, _app: AppSpec) -> Result<Pid, DesktopError> {
            Ok(4242)
        }

        fn capabilities(&self) -> Capabilities {
            Capabilities {
                platform: "fake".into(),
                accessibility_tree: true,
                screenshot: true,
                app_launch: true,
                has_display: true,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeDesktop;
    use super::*;
    use tool_registry::ContentPart;

    fn text(o: &ToolOutput) -> String {
        match &o.content[0] {
            ContentPart::Text { text } => text.clone(),
            _ => panic!(),
        }
    }

    fn settings() -> DesktopSettings {
        DesktopSettings {
            action_settle: Duration::ZERO,
            launch_settle: Duration::ZERO,
        }
    }

    fn build(
        fake: Arc<FakeDesktop>,
        ocr: Option<SharedOcr>,
    ) -> (tempfile::TempDir, Vec<Arc<dyn Tool>>) {
        let d = tempfile::tempdir().unwrap();
        let t = tools(fake, ocr, d.path(), settings());
        (d, t)
    }

    fn get(t: &[Arc<dyn Tool>], name: &str) -> Arc<dyn Tool> {
        t.iter().find(|x| x.spec().name == name).unwrap().clone()
    }

    #[test]
    fn element_refs_parse() {
        assert_eq!(parse_ref(&json!(12)).unwrap(), ElementRef::Index(12));
        assert_eq!(parse_ref(&json!("7")).unwrap(), ElementRef::Index(7));
        assert_eq!(parse_ref(&json!("mark:3")).unwrap(), ElementRef::Mark(3));
        assert!(parse_ref(&json!("mark:x")).is_err());
        assert!(parse_ref(&json!(-1)).is_err());
        assert!(parse_ref(&json!(null)).is_err());
    }

    #[test]
    fn keys_parse() {
        assert_eq!(parse_keys(&json!("ctrl+s")).unwrap(), vec!["ctrl", "s"]);
        assert_eq!(parse_keys(&json!(["Return"])).unwrap(), vec!["Return"]);
        assert!(parse_keys(&json!([])).is_err());
    }

    #[tokio::test]
    async fn get_state_renders_indexed_tree() {
        let fake = Arc::new(FakeDesktop::new(false));
        let (_d, t) = build(fake, None);
        let out = get(&t, "desktop_get_state")
            .call(&ToolContext::default(), json!({"title": "notepad"}))
            .await
            .unwrap();
        let s = text(&out);
        assert!(s.contains("window_id: 77"), "{s}");
        assert!(s.contains("[3] edit \"el3\""), "{s}");
    }

    #[tokio::test]
    async fn click_needs_a_snapshot_then_uses_its_generation_and_returns_refresh() {
        let fake = Arc::new(FakeDesktop::new(false));
        let (_d, t) = build(fake.clone(), None);
        let ctx = ToolContext::default();
        let err = get(&t, "desktop_click")
            .call(&ctx, json!({"index": 1}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("desktop_get_state"));

        get(&t, "desktop_get_state")
            .call(&ctx, json!({}))
            .await
            .unwrap();
        let out = get(&t, "desktop_click")
            .call(&ctx, json!({"index": 1}))
            .await
            .unwrap();
        let s = text(&out);
        assert!(s.contains("dispatched: true"), "{s}");
        assert!(s.contains("gen=2"), "refreshed snapshot expected: {s}");
        let actions = fake.actions.lock();
        assert!(matches!(
            &actions[0],
            Action::Click {
                target: Target::ByIndex {
                    index: 1,
                    generation: 1
                },
                method: ClickMethod::Accessibility
            }
        ));
    }

    #[tokio::test]
    async fn type_focuses_by_index_then_types_and_state_shows_text() {
        let fake = Arc::new(FakeDesktop::new(false));
        let (_d, t) = build(fake.clone(), None);
        let ctx = ToolContext::default();
        get(&t, "desktop_get_state")
            .call(&ctx, json!({}))
            .await
            .unwrap();
        let out = get(&t, "desktop_type")
            .call(&ctx, json!({"index": 3, "text": "hello"}))
            .await
            .unwrap();
        assert!(text(&out).contains("value=\"hello\""), "{}", text(&out));
        let actions = fake.actions.lock();
        assert!(matches!(
            &actions[0],
            Action::Click {
                method: ClickMethod::Global,
                ..
            }
        ));
        assert!(matches!(&actions[1], Action::TypeText { text } if text == "hello"));
    }

    #[tokio::test]
    async fn typing_requires_an_index() {
        let fake = Arc::new(FakeDesktop::new(false));
        let (_d, t) = build(fake.clone(), None);
        let err = get(&t, "desktop_type")
            .call(&ToolContext::default(), json!({"text": "blind"}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput(_)));
        assert!(fake.actions.lock().is_empty());
    }

    struct FixedOcr;
    impl OcrEngine for FixedOcr {
        fn recognize(&self, _img: &vision::RgbImage) -> Vec<vision::TextBox> {
            vec![
                vision::TextBox::new("Save".into(), BoundingBox::new(10, 20, 40, 20), 0.9),
                vision::TextBox::new("Cancel".into(), BoundingBox::new(100, 20, 60, 20), 0.9),
            ]
        }
    }

    #[tokio::test]
    async fn weak_tree_gets_ocr_marks_clickable_by_mark() {
        let mut fake = FakeDesktop::new(true);
        let img = image::RgbImage::from_pixel(400, 300, image::Rgb([240, 240, 240]));
        fake.screenshot_png = vision::encode_png(&img).unwrap().bytes;
        let fake = Arc::new(fake);
        let (_d, t) = build(fake.clone(), Some(Arc::new(FixedOcr)));
        let ctx = ToolContext::default();
        let out = get(&t, "desktop_get_state")
            .call(&ctx, json!({}))
            .await
            .unwrap();
        let s = text(&out);
        assert!(s.contains("mark:1 \"Save\""), "{s}");
        assert!(s.contains("mark:2 \"Cancel\""), "{s}");
        // Window origin (100,200) + box centre (30,30).
        assert!(s.contains("(130,230)"), "{s}");

        get(&t, "desktop_click")
            .call(&ctx, json!({"index": "mark:2"}))
            .await
            .unwrap();
        let actions = fake.actions.lock();
        match &actions[0] {
            Action::Click {
                target: Target::ByCoordinate { x, y, space },
                method,
            } => {
                assert!((x - 230.0).abs() < 0.5 && (y - 230.0).abs() < 0.5);
                assert_eq!(*space, CoordinateSpace::Absolute);
                assert_eq!(*method, ClickMethod::Global);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn unknown_mark_is_an_error() {
        let fake = Arc::new(FakeDesktop::new(false));
        let (_d, t) = build(fake, None);
        let ctx = ToolContext::default();
        get(&t, "desktop_get_state")
            .call(&ctx, json!({}))
            .await
            .unwrap();
        let err = get(&t, "desktop_click")
            .call(&ctx, json!({"index": "mark:9"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("mark:9"));
    }

    #[tokio::test]
    async fn launch_and_list_apps() {
        let fake = Arc::new(FakeDesktop::new(false));
        let (_d, t) = build(fake, None);
        let ctx = ToolContext::default();
        let out = get(&t, "desktop_launch")
            .call(&ctx, json!({"executable": "notepad.exe"}))
            .await
            .unwrap();
        assert!(text(&out).contains("launched pid=4242"));
        let out = get(&t, "desktop_list_apps")
            .call(&ctx, json!({}))
            .await
            .unwrap();
        assert!(text(&out).contains("\"Fake Notepad\""));
    }

    #[tokio::test]
    async fn screenshot_flag_saves_a_file() {
        let fake = Arc::new(FakeDesktop::new(false));
        let (_d, t) = build(fake, None);
        let out = get(&t, "desktop_get_state")
            .call(&ToolContext::default(), json!({"screenshot": true}))
            .await
            .unwrap();
        let s = text(&out);
        let path = s
            .split("screenshot: ")
            .nth(1)
            .unwrap()
            .split(' ')
            .next()
            .unwrap();
        assert!(Path::new(path).exists(), "{s}");
    }
}
