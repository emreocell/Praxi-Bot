//! Desktop tools (permission category `Desktop`) over any [`DesktopBackend`].
//!
//! * Every mutating tool returns the refreshed snapshot.
//! * Keyboard tools need an element: `desktop_type` focuses the element first,
//!   there is no blind typing into an unknown focus.
//! * When the accessibility tree is `Weak`, the state also lists OCR text boxes
//!   with set-of-marks numbers; `index: "mark:<n>"` clicks a mark by coordinates.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
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
use tracing::Instrument as _;
use vision::{BoundingBox, OcrEngine};

use protocol::event::ControlRect;

use crate::control::{ActionPermit, ControlRequest, DesktopControlGate};
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
const LAUNCH_POLL: Duration = Duration::from_millis(100);

/// Per-run states kept at once; the oldest run is evicted beyond this.
const MAX_RUNS: usize = 16;

/// Roles `interactive_only` keeps (mirrors the backends' interactive set).
const INTERACTIVE_ROLES: &[&str] = &[
    "button",
    "checkbox",
    "combobox",
    "edit",
    "hyperlink",
    "listitem",
    "menuitem",
    "radiobutton",
    "tabitem",
    "treeitem",
    "slider",
    "spinner",
    "splitbutton",
    "document",
    "dataitem",
    "headeritem",
    "scrollbar",
    "thumb",
];

const STALE_HINT: &str = " (the index is stale; call desktop_get_state and use the new indexes)";
const MISSING_GEN_MSG: &str = "missing required parameter 'gen'; call desktop_get_state first and pass gen from that observation";

/// Desktop state of one run. Runs must never see each other's indexes.
// NOTE(concurrency): While `State` (`window_id`, `generation`, `targets`, `marks`) is tracked per `run_id`
// in `RunStates`, the underlying `DesktopBackend` (`desktop-windows` `uia.rs` and `desktop-linux` `atspi_tree.rs`)
// currently maintains a single global generation and retained element tree. If another run takes
// a snapshot, any subsequent action using this run's older generation fails closed (stale generation error).
// See tasks/060-desktop-per-run-trees.md for the planned follow-up (per-run retained trees or
// single-flight desktop actions).
#[derive(Default)]
struct State {
    observation_epoch: u64,
    window_id: Option<u64>,
    generation: Option<u64>,
    /// Mark number to absolute screen centre.
    marks: BTreeMap<u32, (f64, f64)>,
    /// Element index to its label and on-screen bounds (for the control gate).
    targets: BTreeMap<u32, (String, Option<ControlRect>)>,
}

/// A small capped map of per-run states (LRU eviction: touch on every use).
#[derive(Default)]
struct RunStates {
    order: VecDeque<String>,
    map: HashMap<String, State>,
}

impl RunStates {
    fn touch(&mut self, run_id: &str) {
        if self.map.contains_key(run_id) {
            if let Some(pos) = self.order.iter().position(|r| r == run_id) {
                self.order.remove(pos);
            }
            self.order.push_back(run_id.to_owned());
        }
    }

    fn get(&mut self, run_id: &str) -> Option<&State> {
        self.touch(run_id);
        if let Some(s) = self.map.get(run_id) {
            return Some(s);
        }
        self.order.back().and_then(|last| self.map.get(last))
    }

    fn entry(&mut self, run_id: &str) -> &mut State {
        if self.map.contains_key(run_id) {
            self.touch(run_id);
        } else {
            while self.order.len() >= MAX_RUNS {
                if let Some(old) = self.order.pop_front() {
                    self.map.remove(&old);
                } else {
                    break;
                }
            }
            self.order.push_back(run_id.to_owned());
        }
        self.map.entry(run_id.to_owned()).or_default()
    }
}

/// Observation filter of `desktop_get_state`.
#[derive(Default)]
struct ObsFilter {
    interactive_only: bool,
    /// Lower-cased substring.
    query: Option<String>,
}

impl ObsFilter {
    fn is_active(&self) -> bool {
        self.interactive_only || self.query.is_some()
    }

    fn keeps(&self, n: &desktop_api::UiNode) -> bool {
        if self.interactive_only
            && !(INTERACTIVE_ROLES.contains(&n.role.as_str())
                || n.states.iter().any(|s| s == "focused"))
        {
            return false;
        }
        match &self.query {
            Some(q) => {
                n.name.to_lowercase().contains(q)
                    || n.role.to_lowercase().contains(q)
                    || n.value
                        .as_deref()
                        .is_some_and(|v| v.to_lowercase().contains(q))
            }
            None => true,
        }
    }
}

struct Shared {
    backend: Arc<dyn DesktopBackend>,
    ocr: Option<SharedOcr>,
    state: Mutex<RunStates>,
    data_dir: PathBuf,
    settings: DesktopSettings,
    control: Option<Arc<dyn DesktopControlGate>>,
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

fn rect_of(r: desktop_api::snapshot::Rect) -> ControlRect {
    ControlRect {
        x: r.x,
        y: r.y,
        width: r.width,
        height: r.height,
    }
}

/// A small box around a screen point (marks and raw coordinates have no size).
fn point_rect(x: f64, y: f64) -> ControlRect {
    ControlRect {
        x: x - 16.0,
        y: y - 16.0,
        width: 32.0,
        height: 32.0,
    }
}

fn union(a: Option<ControlRect>, b: Option<ControlRect>) -> Option<ControlRect> {
    match (a, b) {
        (Some(a), Some(b)) => {
            let (x0, y0) = (a.x.min(b.x), a.y.min(b.y));
            let (x1, y1) = (
                (a.x + a.width).max(b.x + b.width),
                (a.y + a.height).max(b.y + b.height),
            );
            Some(ControlRect {
                x: x0,
                y: y0,
                width: x1 - x0,
                height: y1 - y0,
            })
        }
        (a, b) => a.or(b),
    }
}

fn derr(e: &DesktopError) -> ToolError {
    let hint = match e {
        DesktopError::ElementNotFound { .. } => STALE_HINT,
        DesktopError::Accessibility(m) if m.contains("fingerprint mismatch") => STALE_HINT,
        _ => "",
    };
    exec_err(format!("{e}{hint}"))
}

impl Shared {
    /// Resolve a reference against the caller's run state. `expect_gen` is required
    /// and must match the run's current generation (otherwise: stale hint).
    fn target(
        &self,
        run_id: &str,
        elem_ref: ElementRef,
        expect_gen: Option<u64>,
    ) -> Result<Target, ToolError> {
        let Some(expected_gen) = expect_gen else {
            return Err(ToolError::InvalidInput(MISSING_GEN_MSG.into()));
        };
        let mut states = self.state.lock();
        let st = states.get(run_id);
        let generation = st
            .and_then(|s| s.generation)
            .ok_or_else(|| exec_err("no snapshot yet; call desktop_get_state first"))?;
        match elem_ref {
            ElementRef::Index(index) => {
                if expected_gen != generation {
                    return Err(exec_err(format!(
                        "index {index} is from generation {expected_gen}, but the current generation is {generation}{STALE_HINT}"
                    )));
                }
                if !st.is_some_and(|s| s.targets.contains_key(&index)) {
                    return Err(exec_err(format!(
                        "index {index} was not shown in the observation (omitted by filter or does not exist); call desktop_get_state{STALE_HINT}"
                    )));
                }
                Ok(Target::ByIndex { index, generation })
            }
            ElementRef::Mark(mark_idx) => {
                if expected_gen != generation {
                    return Err(exec_err(format!(
                        "mark:{mark_idx} is from generation {expected_gen}, but the current generation is {generation}{STALE_HINT}"
                    )));
                }
                let (mark_x, mark_y) = st
                    .and_then(|s| s.marks.get(&mark_idx).copied())
                    .ok_or_else(|| {
                        exec_err(format!(
                            "mark:{mark_idx} is unknown; marks exist only after desktop_get_state on a window with a weak tree"
                        ))
                    })?;
                Ok(Target::ByCoordinate {
                    x: mark_x,
                    y: mark_y,
                    space: CoordinateSpace::Absolute,
                })
            }
        }
    }

    /// Label and on-screen bounds of the element a mutating action targets.
    fn describe(&self, run_id: &str, elem_ref: ElementRef) -> (String, Option<ControlRect>) {
        let mut states = self.state.lock();
        let st = states.get(run_id);
        match elem_ref {
            ElementRef::Index(idx) => st
                .and_then(|s| s.targets.get(&idx).cloned())
                .unwrap_or_else(|| (format!("element {idx}"), None)),
            ElementRef::Mark(mark_idx) => (
                format!("mark {mark_idx}"),
                st.and_then(|s| s.marks.get(&mark_idx))
                    .map(|(mx, my)| point_rect(*mx, *my)),
            ),
        }
    }

    /// Ask the control gate (if any) whether this mutating action may run.
    async fn gate(
        &self,
        run_id: &str,
        action: &str,
        label: String,
        rect: Option<ControlRect>,
    ) -> Result<ActionPermit, ToolError> {
        async {
            if let Some(permit) = crate::control::inherited_action() {
                return Ok(permit);
            }
            let Some(gate) = &self.control else {
                return Ok(ActionPermit::default());
            };
            gate.admit_action(&ControlRequest {
                run_id: run_id.to_owned(),
                rect,
                label,
                action: action.to_owned(),
            })
            .await
            .map_err(exec_err)
        }
        .instrument(tracing::info_span!("desktop.gate", run_id, action))
        .await
    }

    fn default_method(r: ElementRef) -> ClickMethod {
        match r {
            ElementRef::Index(_) => ClickMethod::Accessibility,
            ElementRef::Mark(_) => ClickMethod::Global,
        }
    }

    fn scope(&self, run_id: &str) -> Option<u64> {
        self.state.lock().get(run_id).and_then(|s| s.window_id)
    }

    async fn act(&self, action: Action, permit: &ActionPermit) -> Result<String, ToolError> {
        let backend = Arc::clone(&self.backend);
        let res = permit
            .protect(async move { backend.act(action).await })
            .await
            .map_err(exec_err)?
            .map_err(|e| derr(&e))?;
        let mut head = format!("dispatched: {}", res.dispatched);
        if let Some(v) = res.verified {
            let _ = write!(head, ", verified: {v}");
        }
        Ok(head)
    }

    /// Take a fresh snapshot and render it (with marks when the tree is weak).
    async fn refresh(
        &self,
        run_id: &str,
        scope: Option<u64>,
        filter: &ObsFilter,
    ) -> Result<String, ToolError> {
        async {
            let observation_epoch=self.control.as_ref().map_or(0,|gate|gate.observation_epoch());
            let snap = self.backend.snapshot(scope).await.map_err(|e| derr(&e))?;
            if self.control.as_ref().map_or(0,|gate|gate.observation_epoch()) != observation_epoch {
                self.state.lock().map.remove(run_id);
                return Err(exec_err("human intervention changed the display during observation; obtain a fresh observation"));
            }
            let rendered_nodes: Vec<_> = if filter.is_active() {
                snap.nodes
                    .iter()
                    .filter(|n| filter.keeps(n))
                    .cloned()
                    .collect()
            } else {
                snap.nodes.clone()
            };
            let omitted = snap.nodes.len() - rendered_nodes.len();
            {
                let mut states = self.state.lock();
                let st = states.entry(run_id);
                st.window_id = Some(snap.window.id);
                st.generation = Some(snap.generation);
                st.observation_epoch = observation_epoch;
                st.marks.clear();
                st.targets = rendered_nodes
                    .iter()
                    .map(|n| {
                        let label = if n.name.is_empty() {
                            n.role.clone()
                        } else {
                            format!(
                                "{} \"{}\"",
                                n.role,
                                n.name
                                    .chars()
                                    .take(60)
                                    .collect::<String>()
                                    .replace('"', "'")
                            )
                        };
                        (n.index, (label, n.bounds.map(rect_of)))
                    })
                    .collect();
            }
            let mut text = format!("window_id: {}\n", snap.window.id);
            if filter.is_active() {
                let view = Snapshot {
                    window: snap.window.clone(),
                    nodes: rendered_nodes,
                    generation: snap.generation,
                    quality: snap.quality,
                };
                text.push_str(&render_snapshot(&view));
                if omitted > 0 {
                    let _ = writeln!(
                        text,
                        "({omitted} nodes omitted; use interactive_only/query)"
                    );
                }
            } else {
                text.push_str(&render_snapshot(&snap));
            }
            if snap.quality == TreeQuality::Weak {
                text.push_str(&self.marks_section(run_id, &snap).await);
            }
            Ok(text)
        }
        .instrument(tracing::info_span!("desktop.snapshot", run_id, ?scope))
        .await
    }

    async fn marks_section(&self, run_id: &str, snap: &Snapshot) -> String {
        let Some(ocr) = self.ocr.clone() else {
            return "\n(accessibility tree is weak and no OCR engine is configured)\n".into();
        };
        if !self.backend.capabilities().screenshot {
            return "\n(accessibility tree is weak; screenshots are unavailable)\n".into();
        }
        match self.build_marks(run_id, snap, ocr).await {
            Ok(s) => s,
            Err(e) => format!("\n(accessibility tree is weak; OCR failed: {e})\n"),
        }
    }

    async fn build_marks(
        &self,
        run_id: &str,
        snap: &Snapshot,
        ocr: SharedOcr,
    ) -> Result<String, String> {
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
            let mut states = self.state.lock();
            let st = states.entry(run_id);
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

    async fn settle_and_refresh(&self, run_id: &str, prefix: String) -> Result<String, ToolError> {
        async {
            if !self.settings.action_settle.is_zero() {
                tokio::time::sleep(self.settings.action_settle).await;
            }
        }
        .instrument(tracing::info_span!("desktop.settle", op = "action"))
        .await;
        let scope = self.scope(run_id);
        let state = self.refresh(run_id, scope, &ObsFilter::default()).await?;
        Ok(format!("{prefix}\n{state}"))
    }

    /// Poll `windows()` until a new top-level window appears (matching `pid`, or
    /// any new window when `pid == 0`). The first sample is immediate; later
    /// samples are about 100 ms apart. A non-empty title counts as the one
    /// stable sample that ends the wait. `deadline` is absolute: the caller
    /// starts it before `launch()` so `WaitForInputIdle` counts toward
    /// `launch_settle` and the worst case is not slower than the old fixed sleep.
    async fn poll_launched_windows(
        &self,
        before: &HashSet<u64>,
        pid: u32,
        deadline: tokio::time::Instant,
    ) -> Result<Vec<desktop_api::WindowInfo>, ToolError> {
        let matches_launch = |w: &desktop_api::WindowInfo| -> bool {
            !before.contains(&w.id) && !w.title.is_empty() && (pid == 0 || w.pid == pid)
        };
        async {
            loop {
                let wins = self.backend.windows().await.map_err(|e| derr(&e))?;
                let found: Vec<_> = wins.into_iter().filter(&matches_launch).collect();
                let now = tokio::time::Instant::now();
                if !found.is_empty() || now >= deadline {
                    return Ok(found);
                }
                let remaining = deadline.saturating_duration_since(now);
                if remaining.is_zero() {
                    return Ok(found);
                }
                tokio::time::sleep(LAUNCH_POLL.min(remaining)).await;
            }
        }
        .instrument(tracing::info_span!("desktop.settle", op = "launch"))
        .await
    }
}

/// Build the desktop tools.
#[must_use]
pub fn tools(
    backend: Arc<dyn DesktopBackend>,
    ocr: Option<SharedOcr>,
    data_dir: &Path,
    settings: DesktopSettings,
    control: Option<Arc<dyn DesktopControlGate>>,
) -> Vec<Arc<dyn Tool>> {
    let shared = Arc::new(Shared {
        backend,
        ocr,
        state: Mutex::new(RunStates::default()),
        data_dir: data_dir.to_path_buf(),
        settings,
        control,
    });
    let d = PermissionCategory::Desktop;
    let index_prop = json!({
        "description": "Element index from desktop_get_state, or \"mark:<n>\" for an OCR mark.",
        "oneOf": [{"type":"integer","minimum":0},{"type":"string","pattern":"^(mark:)?[0-9]+$"}]
    });
    let gen_prop = json!({
        "type":"integer","minimum":0,
        "description":"Generation (gen=N in the # window header) of the observation the index was read from. Pass gen from the observation you read the index from; a mismatch is refused without acting."
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
                "Start, open or run an application, program, document or URI (launch an app, open a file with it) and list windows. This is the way to open apps. Examples: {\"executable\":\"notepad.exe\"}, {\"executable\":\"calc.exe\"}, a folder in Explorer {\"executable\":\"explorer.exe\",\"args\":[\"C:\\Users\"]}. Windows Settings pages are URIs, not programs: {\"executable\":\"ms-settings:display\"} (also ms-settings:network, ms-settings:bluetooth, ms-settings:about); never open Explorer for them. Other URI examples: calculator:, ms-clock:. Do not use it for web pages (http/https): use the browser tools for those. Never start an app by typing its name into some window; always launch it with this tool.",
                json!({"type":"object","properties":{
                    "executable":{"type":"string","description":"Program name, file path or URI such as ms-settings:display."},
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
                "Read the UI tree of a window as numbered elements. Use these numbers as index in other desktop tools (pass gen from this observation). Large windows: use interactive_only or query to shrink the output; indexes stay the same. Example: {\"title\":\"Notepad\"}",
                json!({"type":"object","properties":{
                    "window_id":{"type":"integer","description":"Window id from desktop_list_apps."},
                    "title":{"type":"string","description":"Part of the window title."},
                    "screenshot":{"type":"boolean","description":"Also save a screenshot and return its file path."},
                    "interactive_only":{"type":"boolean","description":"Only list interactive elements (buttons, fields, links...) and the focused one."},
                    "query":{"type":"string","description":"Only list elements whose name, value or role contains this text (case-insensitive)."}
                }}),
                Effect::Read,
                d,
            ),
        ),
        (
            Op::Click,
            spec(
                "desktop_click",
                "Click a button, link or other element (mouse click). Returns the refreshed UI tree. Pass gen from the observation you read the index from. Example: {\"index\":12,\"gen\":3}",
                json!({"type":"object","properties":{"index":index_prop,"gen":gen_prop,"method":method_prop},"required":["index","gen"]}),
                Effect::Write,
                d,
            ),
        ),
        (
            Op::Type,
            spec(
                "desktop_type",
                "Click an element to focus it, then type text with the keyboard (enter text). Not for starting apps (use desktop_launch). Returns the refreshed UI tree. Pass gen from the observation you read the index from. Example: {\"index\":5,\"gen\":3,\"text\":\"hello\"}",
                json!({"type":"object","properties":{
                    "index":index_prop,
                    "gen":gen_prop,
                    "text":{"type":"string","description":"Text to type."}
                },"required":["index","text","gen"]}),
                Effect::Write,
                d,
            ),
        ),
        (
            Op::SetValue,
            spec(
                "desktop_set_value",
                "Replace the value of a text field or combo box. Pass gen from the observation you read the index from. Example: {\"index\":5,\"gen\":3,\"value\":\"hello\"}",
                json!({"type":"object","properties":{
                    "index":index_prop,
                    "gen":gen_prop,
                    "value":{"type":"string","description":"New value."}
                },"required":["index","value","gen"]}),
                Effect::Write,
                d,
            ),
        ),
        (
            Op::Key,
            spec(
                "desktop_key",
                "Press a keyboard key or shortcut chord (Enter, Tab, Ctrl+S, Ctrl+Shift+N), optionally after focusing an element (index: pass gen from the observation you read the index from). Example: {\"keys\":[\"ctrl\",\"s\"]}",
                json!({"type":"object","properties":{
                    "keys":{"description":"Key names, e.g. [\"ctrl\",\"s\"], [\"Return\"] or \"ctrl+s\".","oneOf":[
                        {"type":"array","items":{"type":"string"},"minItems":1},{"type":"string"}]},
                    "index":index_prop,
                    "gen":gen_prop
                },"required":["keys"]}),
                Effect::Write,
                d,
            ),
        ),
        (
            Op::Scroll,
            spec(
                "desktop_scroll",
                "Scroll an element (positive dy = down). With index, pass gen from the observation you read the index from. Example: {\"index\":3,\"dy\":5}",
                json!({"type":"object","properties":{
                    "index":index_prop,
                    "gen":gen_prop,
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
                "Drag from one element to another (moves the real cursor). Pass gen from the observation you read the indexes from. Example: {\"from\":3,\"to\":9,\"gen\":3}",
                json!({"type":"object","properties":{
                    "from":index_prop,
                    "to":index_prop,
                    "gen":gen_prop,
                    "method":method_prop
                },"required":["from","to","gen"]}),
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
    #[serde(default)]
    interactive_only: bool,
    query: Option<String>,
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
    #[serde(rename = "gen")]
    expect_gen: Option<u64>,
    method: Option<String>,
}

#[derive(Deserialize)]
struct TypeIn {
    index: Value,
    #[serde(rename = "gen")]
    expect_gen: Option<u64>,
    text: String,
}

#[derive(Deserialize)]
struct SetValueIn {
    index: Value,
    #[serde(rename = "gen")]
    expect_gen: Option<u64>,
    value: String,
}

#[derive(Deserialize)]
struct KeyIn {
    keys: Value,
    index: Option<Value>,
    #[serde(rename = "gen")]
    expect_gen: Option<u64>,
}

#[derive(Deserialize)]
struct ScrollIn {
    index: Option<Value>,
    #[serde(rename = "gen")]
    expect_gen: Option<u64>,
    x: Option<f64>,
    y: Option<f64>,
    #[serde(default)]
    dx: f64,
    #[serde(default)]
    dy: f64,
}

#[derive(Deserialize)]
struct DragIn {
    #[serde(rename = "gen")]
    expect_gen: Option<u64>,
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
    async fn run(&self, run_id: &str, input: Value) -> Result<String, ToolError> {
        let sh = &self.shared;
        match self.op {
            Op::ListApps => {
                let wins = sh
                    .backend
                    .windows()
                    .instrument(tracing::info_span!("desktop.act", op = "list_apps"))
                    .await
                    .map_err(|e| derr(&e))?;
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
                let permit = sh
                    .gate(run_id, "launch", i.executable.clone(), None)
                    .await?;
                if permit.needs_observation()
                    || sh.state.lock().get(run_id).is_some_and(|s| {
                        s.observation_epoch
                            != sh.control.as_ref().map_or(0, |g| g.observation_epoch())
                    })
                {
                    sh.state.lock().map.remove(run_id);
                    let fresh = sh.refresh(run_id, None, &ObsFilter::default()).await?;
                    return Ok(format!(
                        "Human intervention completed; previous action discarded. Fresh observation:\n{fresh}"
                    ));
                }
                let before: HashSet<u64> = sh
                    .backend
                    .windows()
                    .await
                    .map_err(|e| derr(&e))?
                    .into_iter()
                    .map(|w| w.id)
                    .collect();
                // Deadline starts before `launch` so `WaitForInputIdle` shares the
                // `launch_settle` budget. Zero settle makes the deadline "now":
                // one sample, no sleep.
                let deadline = tokio::time::Instant::now() + sh.settings.launch_settle;
                let backend = Arc::clone(&sh.backend);
                let pid = permit
                    .protect(async move {
                        backend
                            .launch(AppSpec {
                                executable: i.executable,
                                args: i.args,
                            })
                            .await
                    })
                    .await
                    .map_err(exec_err)?
                    .map_err(|e| derr(&e))?;
                let wins = sh.poll_launched_windows(&before, pid, deadline).await?;
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
                    sh.scope(run_id)
                };
                let filter = ObsFilter {
                    interactive_only: i.interactive_only,
                    query: i
                        .query
                        .as_deref()
                        .map(str::trim)
                        .filter(|q| !q.is_empty())
                        .map(str::to_lowercase),
                };
                let mut text = sh.refresh(run_id, scope, &filter).await?;
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
                if i.expect_gen.is_none() {
                    return Err(ToolError::InvalidInput(MISSING_GEN_MSG.into()));
                }
                let r = parse_ref(&i.index)?;
                let method = parse_method(i.method.as_deref(), Shared::default_method(r))?;
                let target = sh.target(run_id, r, i.expect_gen)?;
                let (label, rect) = sh.describe(run_id, r);
                let permit = sh.gate(run_id, "click", label, rect).await?;
                if permit.needs_observation()
                    || sh.state.lock().get(run_id).is_some_and(|s| {
                        s.observation_epoch
                            != sh.control.as_ref().map_or(0, |g| g.observation_epoch())
                    })
                {
                    sh.state.lock().map.remove(run_id);
                    let fresh = sh.refresh(run_id, None, &ObsFilter::default()).await?;
                    return Ok(format!(
                        "Human intervention completed; previous action discarded. Fresh observation:\n{fresh}"
                    ));
                }
                let head = sh.act(Action::Click { target, method }, &permit).await?;
                sh.settle_and_refresh(run_id, head).await
            }
            Op::Type => {
                let i: TypeIn = parse(input)?;
                if i.expect_gen.is_none() {
                    return Err(ToolError::InvalidInput(MISSING_GEN_MSG.into()));
                }
                let r = parse_ref(&i.index)?;
                let target = sh.target(run_id, r, i.expect_gen)?;
                let (label, rect) = sh.describe(run_id, r);
                let permit = sh.gate(run_id, "type", label, rect).await?;
                if permit.needs_observation()
                    || sh.state.lock().get(run_id).is_some_and(|s| {
                        s.observation_epoch
                            != sh.control.as_ref().map_or(0, |g| g.observation_epoch())
                    })
                {
                    sh.state.lock().map.remove(run_id);
                    let fresh = sh.refresh(run_id, None, &ObsFilter::default()).await?;
                    return Ok(format!(
                        "Human intervention completed; previous action discarded. Fresh observation:\n{fresh}"
                    ));
                }
                // Focus the element with a real click, then type into the focus.
                sh.act(
                    Action::Click {
                        target,
                        method: ClickMethod::Global,
                    },
                    &permit,
                )
                .await?;
                let head = sh.act(Action::TypeText { text: i.text }, &permit).await?;
                sh.settle_and_refresh(run_id, head).await
            }
            Op::SetValue => {
                let i: SetValueIn = parse(input)?;
                if i.expect_gen.is_none() {
                    return Err(ToolError::InvalidInput(MISSING_GEN_MSG.into()));
                }
                let r = parse_ref(&i.index)?;
                let target = sh.target(run_id, r, i.expect_gen)?;
                let (label, rect) = sh.describe(run_id, r);
                let permit = sh.gate(run_id, "set_value", label, rect).await?;
                if permit.needs_observation()
                    || sh.state.lock().get(run_id).is_some_and(|s| {
                        s.observation_epoch
                            != sh.control.as_ref().map_or(0, |g| g.observation_epoch())
                    })
                {
                    sh.state.lock().map.remove(run_id);
                    let fresh = sh.refresh(run_id, None, &ObsFilter::default()).await?;
                    return Ok(format!(
                        "Human intervention completed; previous action discarded. Fresh observation:\n{fresh}"
                    ));
                }
                let head = sh
                    .act(
                        Action::SetValue {
                            target,
                            value: i.value,
                        },
                        &permit,
                    )
                    .await?;
                sh.settle_and_refresh(run_id, head).await
            }
            Op::Key => {
                let i: KeyIn = parse(input)?;
                let keys = parse_keys(&i.keys)?;
                let focus = i.index.as_ref().map(parse_ref).transpose()?;
                if focus.is_some() && i.expect_gen.is_none() {
                    return Err(ToolError::InvalidInput(MISSING_GEN_MSG.into()));
                }
                let focus_target = focus
                    .map(|r| sh.target(run_id, r, i.expect_gen))
                    .transpose()?;
                let (label, rect) = match focus {
                    Some(r) => {
                        let (l, rect) = sh.describe(run_id, r);
                        (format!("{} in {l}", keys.join("+")), rect)
                    }
                    None => (keys.join("+"), None),
                };
                let permit = sh.gate(run_id, "key", label, rect).await?;
                if permit.needs_observation()
                    || sh.state.lock().get(run_id).is_some_and(|s| {
                        s.observation_epoch
                            != sh.control.as_ref().map_or(0, |g| g.observation_epoch())
                    })
                {
                    sh.state.lock().map.remove(run_id);
                    let fresh = sh.refresh(run_id, None, &ObsFilter::default()).await?;
                    return Ok(format!(
                        "Human intervention completed; previous action discarded. Fresh observation:\n{fresh}"
                    ));
                }
                if let Some(target) = focus_target {
                    sh.act(
                        Action::Click {
                            target,
                            method: ClickMethod::Global,
                        },
                        &permit,
                    )
                    .await?;
                }
                let head = sh.act(Action::KeyPress { keys }, &permit).await?;
                sh.settle_and_refresh(run_id, head).await
            }
            Op::Scroll => {
                let i: ScrollIn = parse(input)?;
                if i.index.is_some() && i.expect_gen.is_none() {
                    return Err(ToolError::InvalidInput(MISSING_GEN_MSG.into()));
                }
                let (label, rect) = match (&i.index, i.x, i.y) {
                    (Some(idx), _, _) => sh.describe(run_id, parse_ref(idx)?),
                    (None, Some(x), Some(y)) => ("point".to_owned(), Some(point_rect(x, y))),
                    _ => (String::new(), None),
                };
                let target = match (&i.index, i.x, i.y) {
                    (Some(idx), _, _) => sh.target(run_id, parse_ref(idx)?, i.expect_gen)?,
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
                let permit = sh.gate(run_id, "scroll", label, rect).await?;
                if permit.needs_observation()
                    || sh.state.lock().get(run_id).is_some_and(|s| {
                        s.observation_epoch
                            != sh.control.as_ref().map_or(0, |g| g.observation_epoch())
                    })
                {
                    sh.state.lock().map.remove(run_id);
                    let fresh = sh.refresh(run_id, None, &ObsFilter::default()).await?;
                    return Ok(format!(
                        "Human intervention completed; previous action discarded. Fresh observation:\n{fresh}"
                    ));
                }
                let head = sh
                    .act(
                        Action::Scroll {
                            target,
                            delta_x: i.dx,
                            delta_y: i.dy,
                        },
                        &permit,
                    )
                    .await?;
                sh.settle_and_refresh(run_id, head).await
            }
            Op::Drag => {
                let i: DragIn = parse(input)?;
                if i.expect_gen.is_none() {
                    return Err(ToolError::InvalidInput(MISSING_GEN_MSG.into()));
                }
                let (a, b) = (parse_ref(&i.from)?, parse_ref(&i.to)?);
                let method = parse_method(i.method.as_deref(), ClickMethod::Global)?;
                let (from, to) = (
                    sh.target(run_id, a, i.expect_gen)?,
                    sh.target(run_id, b, i.expect_gen)?,
                );
                let ((la, ra), (lb, rb)) = (sh.describe(run_id, a), sh.describe(run_id, b));
                let permit = sh
                    .gate(run_id, "drag", format!("{la} to {lb}"), union(ra, rb))
                    .await?;
                if permit.needs_observation()
                    || sh.state.lock().get(run_id).is_some_and(|s| {
                        s.observation_epoch
                            != sh.control.as_ref().map_or(0, |g| g.observation_epoch())
                    })
                {
                    sh.state.lock().map.remove(run_id);
                    let fresh = sh.refresh(run_id, None, &ObsFilter::default()).await?;
                    return Ok(format!(
                        "Human intervention completed; previous action discarded. Fresh observation:\n{fresh}"
                    ));
                }
                let head = sh.act(Action::Drag { from, to, method }, &permit).await?;
                sh.settle_and_refresh(run_id, head).await
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
        let text = self
            .run(&ctx.run_id, input)
            .instrument(tracing::info_span!(
                "desktop.tool",
                tool = %self.spec.name,
                run_id = %ctx.run_id
            ))
            .await?;
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
        /// Window scope of every snapshot taken.
        pub scopes: Mutex<Vec<Option<u64>>>,
        /// Accept actions whose generation is not the latest one.
        pub lenient_gen: bool,
        /// Extra rows inserted before the buttons (shifts later indexes).
        pub extra_rows: Mutex<u32>,
        /// Pid returned by `launch` (default 4242).
        pub launch_pid: Pid,
        /// Delay after `launch()` before the launched window (`id = 88`) appears.
        /// `None` means no new window appears after launch.
        pub launch_window_delay: Option<Duration>,
        /// Pid of the new window (`id = 88`) that appears after launch.
        pub launched_window_pid: Pid,
        /// Timestamp when `launch()` was called.
        pub launched_at: Mutex<Option<tokio::time::Instant>>,
    }

    impl FakeDesktop {
        pub fn new(weak: bool) -> Self {
            Self {
                actions: Mutex::new(Vec::new()),
                generation: Mutex::new(0),
                weak,
                screenshot_png: Vec::new(),
                typed: Mutex::new(String::new()),
                scopes: Mutex::new(Vec::new()),
                lenient_gen: false,
                extra_rows: Mutex::new(0),
                launch_pid: 4242,
                launch_window_delay: Some(Duration::ZERO),
                launched_window_pid: 4242,
                launched_at: Mutex::new(None),
            }
        }
    }

    fn window(id: u64) -> WindowInfo {
        WindowInfo {
            id,
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
        async fn snapshot(&self, scope: Option<u64>) -> Result<Snapshot, DesktopError> {
            self.scopes.lock().push(scope);
            let win = scope.unwrap_or(77);
            let mut g = self.generation.lock();
            *g += 1;
            let value = self.typed.lock().clone();
            let nodes = if self.weak {
                vec![UiNode::new(0, "pane", "", None, None, vec![])]
            } else {
                let extra = *self.extra_rows.lock();
                (0..6 + extra)
                    .map(|i| {
                        UiNode::new(
                            i,
                            if i == 3 { "edit" } else { "button" },
                            if win == 77 {
                                format!("el{i}")
                            } else {
                                format!("w{win}_el{i}")
                            },
                            (i == 3).then(|| value.clone()),
                            (i == 2).then_some(Rect {
                                x: 120.0,
                                y: 230.0,
                                width: 80.0,
                                height: 24.0,
                            }),
                            vec![],
                        )
                    })
                    .collect()
            };
            let quality = TreeQuality::assess(&nodes);
            Ok(Snapshot {
                window: window(win),
                nodes,
                generation: *g,
                quality,
            })
        }

        async fn act(&self, action: Action) -> Result<ActionResult, DesktopError> {
            let current = *self.generation.lock();
            let lenient = self.lenient_gen;
            let stale = |t: &Target| match t {
                Target::ByIndex { index, generation } if !lenient && *generation != current => {
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
            let mut wins = vec![window(77)];
            if let (Some(at), Some(delay)) = (*self.launched_at.lock(), self.launch_window_delay)
                && at.elapsed() >= delay
            {
                wins.push(WindowInfo {
                    id: 88,
                    title: "Launched App".into(),
                    pid: self.launched_window_pid,
                    bounds: Rect {
                        x: 150.0,
                        y: 250.0,
                        width: 400.0,
                        height: 300.0,
                    },
                    focused: true,
                });
            }
            Ok(wins)
        }

        async fn launch(&self, _app: AppSpec) -> Result<Pid, DesktopError> {
            *self.launched_at.lock() = Some(tokio::time::Instant::now());
            Ok(self.launch_pid)
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
        let t = tools(fake, ocr, d.path(), settings(), None);
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
            .call(&ctx, json!({"index": 1, "gen": 1}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("desktop_get_state"));

        get(&t, "desktop_get_state")
            .call(&ctx, json!({}))
            .await
            .unwrap();
        let out = get(&t, "desktop_click")
            .call(&ctx, json!({"index": 1, "gen": 1}))
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
            .call(&ctx, json!({"index": 3, "gen": 1, "text": "hello"}))
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
            .call(&ctx, json!({"index": "mark:2", "gen": 1}))
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
            .call(&ctx, json!({"index": "mark:9", "gen": 1}))
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
        let launch_text = text(&out);
        assert!(launch_text.contains("launched pid=4242"), "{launch_text}");
        assert!(
            launch_text.contains("id=88 pid=4242 focused \"Launched App\""),
            "launch output should list the new window: {launch_text}"
        );
        assert!(
            !launch_text.contains("id=77"),
            "launch output should not list pre-existing windows: {launch_text}"
        );
        let out = get(&t, "desktop_list_apps")
            .call(&ctx, json!({}))
            .await
            .unwrap();
        assert!(text(&out).contains("\"Fake Notepad\""));
    }

    #[tokio::test(start_paused = true)]
    async fn launch_returns_early_when_new_window_appears_at_200ms() {
        let mut fake = FakeDesktop::new(false);
        fake.launch_window_delay = Some(Duration::from_millis(200));
        let fake = Arc::new(fake);
        let d = tempfile::tempdir().unwrap();
        let t = tools(
            fake,
            None,
            d.path(),
            DesktopSettings {
                action_settle: Duration::ZERO,
                launch_settle: Duration::from_millis(1500),
            },
            None,
        );
        let start = tokio::time::Instant::now();
        let out = get(&t, "desktop_launch")
            .call(
                &ToolContext::default(),
                json!({"executable": "notepad.exe"}),
            )
            .await
            .unwrap();
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(180) && elapsed < Duration::from_millis(900),
            "expected early return around 200ms, got {elapsed:?}"
        );
        let s = text(&out);
        assert!(s.contains("id=88 pid=4242 focused \"Launched App\""), "{s}");
        assert!(!s.contains("id=77"), "only new windows listed: {s}");
    }

    #[tokio::test(start_paused = true)]
    async fn launch_returns_at_cap_when_no_new_window_appears() {
        let mut fake = FakeDesktop::new(false);
        fake.launch_window_delay = None;
        let fake = Arc::new(fake);
        let d = tempfile::tempdir().unwrap();
        let cap = Duration::from_millis(350);
        let t = tools(
            fake,
            None,
            d.path(),
            DesktopSettings {
                action_settle: Duration::ZERO,
                launch_settle: cap,
            },
            None,
        );
        let start = tokio::time::Instant::now();
        let out = get(&t, "desktop_launch")
            .call(
                &ToolContext::default(),
                json!({"executable": "notepad.exe"}),
            )
            .await
            .unwrap();
        let elapsed = start.elapsed();
        assert!(
            elapsed >= cap.saturating_sub(Duration::from_millis(50)) && elapsed <= cap,
            "expected wait until cap ({cap:?}) and not past it, got {elapsed:?}"
        );
        let s = text(&out);
        assert_eq!(s, "launched pid=4242\nwindows:\n");
    }

    #[tokio::test(start_paused = true)]
    async fn launch_returns_immediately_when_the_window_is_already_there() {
        let mut fake = FakeDesktop::new(false);
        fake.launch_window_delay = Some(Duration::ZERO);
        let fake = Arc::new(fake);
        let d = tempfile::tempdir().unwrap();
        let t = tools(
            fake,
            None,
            d.path(),
            DesktopSettings {
                action_settle: Duration::ZERO,
                launch_settle: Duration::from_millis(1500),
            },
            None,
        );
        let start = tokio::time::Instant::now();
        let out = get(&t, "desktop_launch")
            .call(
                &ToolContext::default(),
                json!({"executable": "notepad.exe"}),
            )
            .await
            .unwrap();
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(100),
            "window already present must not wait out launch_settle, got {elapsed:?}"
        );
        let s = text(&out);
        assert!(s.contains("id=88 pid=4242 focused \"Launched App\""), "{s}");
        assert!(!s.contains("id=77"), "{s}");
    }

    #[tokio::test(start_paused = true)]
    async fn launch_ignores_a_new_window_with_a_different_pid() {
        let mut fake = FakeDesktop::new(false);
        fake.launch_pid = 4242;
        fake.launched_window_pid = 9999;
        fake.launch_window_delay = Some(Duration::from_millis(50));
        let fake = Arc::new(fake);
        let d = tempfile::tempdir().unwrap();
        let cap = Duration::from_millis(300);
        let t = tools(
            fake,
            None,
            d.path(),
            DesktopSettings {
                action_settle: Duration::ZERO,
                launch_settle: cap,
            },
            None,
        );
        let start = tokio::time::Instant::now();
        let out = get(&t, "desktop_launch")
            .call(
                &ToolContext::default(),
                json!({"executable": "notepad.exe"}),
            )
            .await
            .unwrap();
        let elapsed = start.elapsed();
        assert!(
            elapsed >= cap.saturating_sub(Duration::from_millis(50)) && elapsed <= cap,
            "a different pid is not the launched window; waited {elapsed:?}, cap {cap:?}"
        );
        assert_eq!(text(&out), "launched pid=4242\nwindows:\n");
    }

    #[tokio::test(start_paused = true)]
    async fn launch_pid_zero_accepts_any_new_window() {
        let mut fake = FakeDesktop::new(false);
        fake.launch_pid = 0;
        fake.launched_window_pid = 9001;
        fake.launch_window_delay = Some(Duration::from_millis(150));
        let fake = Arc::new(fake);
        let d = tempfile::tempdir().unwrap();
        let t = tools(
            fake,
            None,
            d.path(),
            DesktopSettings {
                action_settle: Duration::ZERO,
                launch_settle: Duration::from_millis(1500),
            },
            None,
        );
        let start = tokio::time::Instant::now();
        let out = get(&t, "desktop_launch")
            .call(
                &ToolContext::default(),
                json!({"executable": "ms-settings:display"}),
            )
            .await
            .unwrap();
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(800),
            "expected early return for pid=0 launch, got {elapsed:?}"
        );
        let s = text(&out);
        assert!(s.contains("launched pid=0"), "{s}");
        assert!(s.contains("id=88 pid=9001 focused \"Launched App\""), "{s}");
        assert!(!s.contains("id=77"), "{s}");
    }

    #[tokio::test(start_paused = true)]
    async fn launch_with_zero_settings_skips_wait() {
        let mut fake = FakeDesktop::new(false);
        fake.launch_window_delay = None;
        let fake = Arc::new(fake);
        let (_d, t) = build(fake, None);
        let start = tokio::time::Instant::now();
        let out = get(&t, "desktop_launch")
            .call(
                &ToolContext::default(),
                json!({"executable": "notepad.exe"}),
            )
            .await
            .unwrap();
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(50),
            "zero settings must skip wait, got {elapsed:?}"
        );
        assert_eq!(text(&out), "launched pid=4242\nwindows:\n");
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

    struct Recorder {
        seen: Mutex<Vec<ControlRequest>>,
        refuse: bool,
    }

    #[async_trait]
    impl DesktopControlGate for Recorder {
        async fn before_action(&self, req: &ControlRequest) -> Result<(), String> {
            self.seen.lock().push(req.clone());
            if self.refuse {
                Err("the user has taken control".into())
            } else {
                Ok(())
            }
        }
    }

    fn gated(
        fake: Arc<FakeDesktop>,
        refuse: bool,
    ) -> (tempfile::TempDir, Vec<Arc<dyn Tool>>, Arc<Recorder>) {
        let d = tempfile::tempdir().unwrap();
        let gate = Arc::new(Recorder {
            seen: Mutex::new(Vec::new()),
            refuse,
        });
        let t = tools(fake, None, d.path(), settings(), Some(gate.clone()));
        (d, t, gate)
    }

    #[tokio::test]
    async fn mutating_actions_ask_the_gate_with_rect_label_and_run_id() {
        let fake = Arc::new(FakeDesktop::new(false));
        let (_d, t, gate) = gated(fake.clone(), false);
        let ctx = ToolContext {
            run_id: "run-1".into(),
            ..ToolContext::default()
        };
        get(&t, "desktop_get_state")
            .call(&ctx, json!({}))
            .await
            .unwrap();
        assert!(gate.seen.lock().is_empty(), "reads are not gated");
        get(&t, "desktop_click")
            .call(&ctx, json!({"index": 2, "gen": 1}))
            .await
            .unwrap();
        get(&t, "desktop_key")
            .call(&ctx, json!({"keys": "ctrl+s"}))
            .await
            .unwrap();
        get(&t, "desktop_launch")
            .call(&ctx, json!({"executable": "notepad.exe"}))
            .await
            .unwrap();
        let seen = gate.seen.lock().clone();
        assert_eq!(seen.len(), 3);
        assert_eq!(seen[0].run_id, "run-1");
        assert_eq!(seen[0].action, "click");
        assert_eq!(seen[0].label, "button \"el2\"");
        let r = seen[0].rect.unwrap();
        assert!((r.x - 120.0).abs() < 1e-9 && (r.width - 80.0).abs() < 1e-9);
        assert_eq!(
            (seen[1].action.as_str(), seen[1].label.as_str()),
            ("key", "ctrl+s")
        );
        assert_eq!(
            (seen[2].action.as_str(), seen[2].label.as_str()),
            ("launch", "notepad.exe")
        );
        assert_eq!(fake.actions.lock().len(), 2, "click + key were dispatched");
    }

    #[tokio::test]
    async fn a_refusing_gate_stops_the_action_before_it_is_dispatched() {
        let fake = Arc::new(FakeDesktop::new(false));
        let (_d, t, gate) = gated(fake.clone(), true);
        let ctx = ToolContext::default();
        get(&t, "desktop_get_state")
            .call(&ctx, json!({}))
            .await
            .unwrap();
        let err = get(&t, "desktop_click")
            .call(&ctx, json!({"index": 2, "gen": 1}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("taken control"), "{err}");
        let err = get(&t, "desktop_type")
            .call(&ctx, json!({"index": 3, "gen": 1, "text": "x"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("taken control"), "{err}");
        assert!(
            fake.actions.lock().is_empty(),
            "nothing reached the backend"
        );
        assert_eq!(gate.seen.lock().len(), 2);
    }

    #[tokio::test]
    async fn runs_do_not_see_each_others_windows_or_indexes() {
        let mut fake_desktop = FakeDesktop::new(false);
        fake_desktop.lenient_gen = true;
        let fake = Arc::new(fake_desktop);
        let (_d, tools, gate) = gated(fake.clone(), false);
        let ctx_a = ToolContext {
            run_id: "run-a".into(),
            ..ToolContext::default()
        };
        let ctx_b = ToolContext {
            run_id: "run-b".into(),
            ..ToolContext::default()
        };
        let st = get(&tools, "desktop_get_state");
        st.call(&ctx_a, json!({"window_id": 1})).await.unwrap();
        st.call(&ctx_b, json!({"window_id": 2})).await.unwrap();
        let out = get(&tools, "desktop_click")
            .call(&ctx_a, json!({"index": 1, "gen": 1}))
            .await
            .unwrap();
        let out_str = text(&out);
        assert!(
            out_str.contains("window_id: 1"),
            "refresh must stay on A's window: {out_str}"
        );
        assert_eq!(fake.scopes.lock().last().copied(), Some(Some(1)));
        // A used its own generation (1), not B's (2).
        {
            let actions = fake.actions.lock();
            assert!(matches!(
                &actions[0],
                Action::Click {
                    target: Target::ByIndex {
                        index: 1,
                        generation: 1
                    },
                    ..
                }
            ));
        }
        assert_eq!(gate.seen.lock()[0].label, "button \"w1_el1\"");
        // A run that never observed anything has no state.
        let ctx_c = ToolContext {
            run_id: "run-c".into(),
            ..ToolContext::default()
        };
        let err = get(&tools, "desktop_click")
            .call(&ctx_c, json!({"index": 1, "gen": 1}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("desktop_get_state"), "{err}");
    }

    #[tokio::test]
    async fn strict_backend_rejects_another_runs_newer_generation() {
        let fake = Arc::new(FakeDesktop::new(false));
        let (_d, t) = build(fake.clone(), None);
        let (a, b) = (
            ToolContext {
                run_id: "a".into(),
                ..ToolContext::default()
            },
            ToolContext {
                run_id: "b".into(),
                ..ToolContext::default()
            },
        );
        let st = get(&t, "desktop_get_state");
        st.call(&a, json!({"window_id": 1})).await.unwrap();
        st.call(&b, json!({"window_id": 2})).await.unwrap();
        let err = get(&t, "desktop_click")
            .call(&a, json!({"index": 1, "gen": 1}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("stale"), "{err}");
        assert!(fake.actions.lock().is_empty());
    }

    #[tokio::test]
    async fn old_gen_is_refused_without_dispatch_and_missing_gen_errors() {
        let fake = Arc::new(FakeDesktop::new(false));
        let (_d, t) = build(fake.clone(), None);
        let ctx = ToolContext::default();
        let st = get(&t, "desktop_get_state");
        st.call(&ctx, json!({})).await.unwrap();
        *fake.extra_rows.lock() = 1;
        st.call(&ctx, json!({})).await.unwrap();
        let err = get(&t, "desktop_click")
            .call(&ctx, json!({"index": 1, "gen": 1}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("stale"), "{err}");
        assert!(fake.actions.lock().is_empty(), "nothing dispatched");
        let err = get(&t, "desktop_key")
            .call(&ctx, json!({"keys": "enter", "index": 1, "gen": 1}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("stale"), "{err}");
        // Missing gen returns error telling the model to call desktop_get_state and pass gen.
        let err = get(&t, "desktop_click")
            .call(&ctx, json!({"index": 1}))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("desktop_get_state") && err.to_string().contains("gen"),
            "{err}"
        );
        let err = get(&t, "desktop_key")
            .call(&ctx, json!({"keys": "enter", "index": 1}))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("desktop_get_state") && err.to_string().contains("gen"),
            "{err}"
        );
        // Current gen works.
        get(&t, "desktop_click")
            .call(&ctx, json!({"index": 1, "gen": 2}))
            .await
            .unwrap();
        assert_eq!(fake.actions.lock().len(), 1);
    }

    #[test]
    fn fingerprint_mismatch_maps_to_the_stale_hint() {
        let e = DesktopError::Accessibility(
            "fingerprint mismatch for index 3: element changed since snapshot".into(),
        );
        assert!(derr(&e).to_string().contains("the index is stale"));
        let other = DesktopError::Accessibility("boom".into());
        assert!(!derr(&other).to_string().contains("stale"));
    }

    #[tokio::test]
    async fn run_state_map_is_capped() {
        let fake = Arc::new(FakeDesktop::new(false));
        let (_d, t) = build(fake, None);
        let st = get(&t, "desktop_get_state");
        for n in 0..(MAX_RUNS + 4) {
            let ctx = ToolContext {
                run_id: format!("r{n}"),
                ..ToolContext::default()
            };
            st.call(&ctx, json!({})).await.unwrap();
        }
        let oldest = ToolContext {
            run_id: "r0".into(),
            ..ToolContext::default()
        };
        let err = get(&t, "desktop_click")
            .call(&oldest, json!({"index": 1, "gen": 1}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("desktop_get_state"), "{err}");
        let newest = ToolContext {
            run_id: format!("r{}", MAX_RUNS + 3),
            ..ToolContext::default()
        };
        get(&t, "desktop_click")
            .call(
                &newest,
                json!({"index": 1, "gen": u64::try_from(MAX_RUNS + 4).unwrap()}),
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn active_run_is_not_evicted_from_state_map() {
        let mut f = FakeDesktop::new(false);
        f.lenient_gen = true;
        let fake = Arc::new(f);
        let (_d, t) = build(fake, None);
        let st = get(&t, "desktop_get_state");

        // Fill up to MAX_RUNS (r0..r15)
        for n in 0..MAX_RUNS {
            let ctx = ToolContext {
                run_id: format!("r{n}"),
                ..ToolContext::default()
            };
            st.call(&ctx, json!({})).await.unwrap();
        }

        // r0 is active: access it so it becomes MRU
        let r0 = ToolContext {
            run_id: "r0".into(),
            ..ToolContext::default()
        };
        get(&t, "desktop_click")
            .call(&r0, json!({"index": 1, "gen": 1}))
            .await
            .unwrap();

        // Add a new run r16: exceeds MAX_RUNS, so the LRU entry (r1) should be evicted, not r0.
        let r_new = ToolContext {
            run_id: format!("r{MAX_RUNS}"),
            ..ToolContext::default()
        };
        st.call(&r_new, json!({})).await.unwrap();

        // r1 was inactive and should be evicted
        let r1 = ToolContext {
            run_id: "r1".into(),
            ..ToolContext::default()
        };
        let err = get(&t, "desktop_click")
            .call(&r1, json!({"index": 1, "gen": 2}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("desktop_get_state"), "{err}");

        // r0 was active and must NOT have been evicted (refreshed at gen 17)
        let out = get(&t, "desktop_click")
            .call(
                &r0,
                json!({"index": 1, "gen": u64::try_from(MAX_RUNS + 1).unwrap()}),
            )
            .await
            .unwrap();
        assert!(text(&out).contains("dispatched: true"));
    }

    #[tokio::test]
    async fn filtered_snapshot_rejects_hidden_indexes_and_allows_shown_indexes() {
        let fake = Arc::new(FakeDesktop::new(false));
        let (_d, t) = build(fake.clone(), None);
        let ctx = ToolContext::default();
        let st = get(&t, "desktop_get_state");

        // Query for "edit": only node 3 matches, node 1 and 2 are omitted
        let out = text(&st.call(&ctx, json!({"query": "edit"})).await.unwrap());
        assert!(out.contains("[3] edit"));
        assert!(!out.contains("[1] button"));

        // Trying to click hidden index 1 is rejected before dispatch
        let err = get(&t, "desktop_click")
            .call(&ctx, json!({"index": 1, "gen": 1}))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("was not shown in the observation"),
            "{err}"
        );
        assert!(
            fake.actions.lock().is_empty(),
            "hidden target must not be dispatched"
        );

        // Clicking shown index 3 succeeds
        let out = get(&t, "desktop_click")
            .call(&ctx, json!({"index": 3, "gen": 1}))
            .await
            .unwrap();
        assert!(text(&out).contains("dispatched: true"));
        assert_eq!(fake.actions.lock().len(), 1);
    }

    #[tokio::test]
    async fn interactive_only_and_query_keep_original_indexes() {
        let fake = Arc::new(FakeDesktop::new(false));
        let (_d, t) = build(fake, None);
        let ctx = ToolContext::default();
        let st = get(&t, "desktop_get_state");
        let full = text(&st.call(&ctx, json!({})).await.unwrap());
        let plain = text(&st.call(&ctx, json!({"query": ""})).await.unwrap());
        // Same content except the generation in the header.
        assert_eq!(
            full.replace("gen=1", "gen=2"),
            plain,
            "an empty query is not a filter"
        );
        let q = text(&st.call(&ctx, json!({"query": "EL4"})).await.unwrap());
        assert!(q.contains("[4] button \"el4\""), "{q}");
        assert!(!q.contains("[1] button"), "{q}");
        assert!(
            q.contains("(5 nodes omitted; use interactive_only/query)"),
            "{q}"
        );
        let q = text(&st.call(&ctx, json!({"query": "edit"})).await.unwrap());
        assert!(q.contains("[3] edit"), "{q}");
        assert!(q.contains("(5 nodes omitted"), "{q}");
    }
}
