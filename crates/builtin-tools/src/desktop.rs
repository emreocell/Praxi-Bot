//! Desktop tools (permission category `Desktop`) over any [`DesktopBackend`].
//!
//! * Every mutating tool returns the refreshed snapshot.
//! * Keyboard tools need an element: `desktop_type` focuses the element first,
//!   there is no blind typing into an unknown focus.
//! * When the accessibility tree is `Weak`, the state also lists OCR text boxes
//!   with set-of-marks numbers; `index: "mark:<n>"` clicks a mark by coordinates.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use desktop_api::snapshot::AppSpec;
use desktop_api::{
    ActGuard, Action, ClickMethod, CoordinateSpace, DesktopBackend, DesktopError, Quality, Region,
    Snapshot, Target, TreeQuality, render_snapshot,
};
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{Value, json};
use tool_registry::{
    ControlIdentity, ControlKind, ControlTarget, Effect, PermissionCategory, Tool, ToolContext,
    ToolError, ToolOutput, ToolSpec,
};
use tracing::Instrument as _;
use vision::{BoundingBox, OcrEngine};

use protocol::event::ControlRect;

use crate::control::{ActionPermit, ControlRequest, DesktopControlGate};
use crate::fill_secret::{Source, names_a_secret, refused, scrub, source};
use crate::util::{exec_err, finish_text, parse, save_file, spec};
use secret_store::{FillSecrets, SecretValue, sanitize_label};

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
const NO_OBSERVATION_MSG: &str = "no observation for this run; call desktop_get_state first";

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
    /// Indexes of password fields in the last observation (`secure` state).
    secure: BTreeSet<u32>,
    /// Screen bounds of the window the last snapshot described.
    window_bounds: Option<desktop_api::snapshot::Rect>,
}

/// Longest edge of a screenshot saved for the model. Larger captures are
/// downscaled; the tool text tells the model how to map those pixels back.
const MODEL_SHOT_MAX_EDGE: u32 = 1280;

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
        self.map.get(run_id)
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
    default_backend: Option<Arc<dyn DesktopBackend>>,
    ocr: Option<SharedOcr>,
    state: Mutex<RunStates>,
    data_dir: PathBuf,
    settings: DesktopSettings,
    control: Option<Arc<dyn DesktopControlGate>>,
    /// Owner-stored secrets the type tools may type; `None` refuses `secret`.
    fill_secrets: Option<Arc<FillSecrets>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    ListApps,
    Launch,
    Focus,
    GetState,
    Wait,
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

/// How an image pixel maps onto the screen. `region_*` is the capture size
/// before any downscale (X11 pixels). `img_*` is the file the model opens.
fn screenshot_coordinate_line(
    origin_x: f64,
    origin_y: f64,
    region_w: f64,
    region_h: f64,
    img_w: u32,
    img_h: u32,
) -> String {
    let sx = if img_w == 0 {
        1.0
    } else {
        region_w / f64::from(img_w)
    };
    let sy = if img_h == 0 {
        1.0
    } else {
        region_h / f64::from(img_h)
    };
    if (sx - 1.0).abs() < 0.02 && (sy - 1.0).abs() < 0.02 {
        format!(
            "coordinates: image (0,0) is screen ({origin_x:.0},{origin_y:.0}); 1 image pixel = 1 screen pixel ({img_w}x{img_h}). Element bounds are screen pixels."
        )
    } else {
        format!(
            "coordinates: image (0,0) is screen ({origin_x:.0},{origin_y:.0}); multiply image x by {sx:.3} and image y by {sy:.3} to get screen pixels (capture {region_w:.0}x{region_h:.0}, image {img_w}x{img_h}). Element bounds are screen pixels."
        )
    }
}

/// Shrink a capture whose long edge exceeds [`MODEL_SHOT_MAX_EDGE`].
/// The returned width and height are the file the model will open.
fn shrink_screenshot_for_model(img: desktop_api::Image) -> desktop_api::Image {
    if img.width.max(img.height) <= MODEL_SHOT_MAX_EDGE || img.data.is_empty() {
        return img;
    }
    let Ok(decoded) = image::load_from_memory(&img.data) else {
        return img;
    };
    let rgb = decoded.to_rgb8();
    let down = vision::downscale_max_dimension(&rgb, MODEL_SHOT_MAX_EDGE);
    let Ok(encoded) = vision::encode_jpeg(&down.image, 70) else {
        return img;
    };
    let (width, height) = down.image.dimensions();
    desktop_api::Image {
        data: encoded.bytes,
        width,
        height,
        mime_type: encoded.mime_type.to_string(),
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

/// Why `executable` cannot be started on a desktop whose platform is `platform`,
/// when it plainly belongs to another operating system (a `/opt/...` path on
/// Windows, `chrome.exe` or `C:\...` on Linux/macOS). Decided before any
/// approval is raised: the user is never asked about a call that cannot work.
fn foreign_launch_refusal(platform: &str, executable: &str) -> Option<String> {
    let exe = executable.trim();
    let drive = {
        let b = exe.as_bytes();
        b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == 0x5c || b[2] == b'/')
    };
    let (os, other) = match platform {
        "windows" if exe.starts_with('/') => ("Windows", "Linux or macOS"),
        "linux" | "macos" if drive || exe.to_ascii_lowercase().ends_with(".exe") => (
            if platform == "linux" {
                "Linux"
            } else {
                "macOS"
            },
            "Windows",
        ),
        _ => return None,
    };
    Some(format!(
        "wrong computer: {exe:?} looks like a {other} program or path, but this desktop is a {os} computer. \
Nothing was started and the user was not asked. Use a program name or path that exists on {os}; to open a web page use browser_open when it is listed."
    ))
}

fn derr(e: &DesktopError) -> ToolError {
    let hint = match e {
        DesktopError::ElementNotFound { .. } => STALE_HINT,
        DesktopError::Accessibility(m) if m.contains("fingerprint mismatch") => STALE_HINT,
        _ => "",
    };
    exec_err(format!("{e}{hint}"))
}

/// A result that may have come from typing a secret: scrub the value (and its
/// encodings) out of the text and out of any error message.
fn finish_secret(
    result: Result<String, ToolError>,
    secret: Option<&SecretValue>,
) -> Result<String, ToolError> {
    let Some(v) = secret else {
        return result;
    };
    match result {
        Ok(t) => Ok(scrub(&t, v)),
        Err(ToolError::InvalidInput(m)) => Err(ToolError::InvalidInput(scrub(&m, v))),
        Err(ToolError::PermissionDenied(m)) => Err(ToolError::PermissionDenied(scrub(&m, v))),
        Err(ToolError::Execution(m)) => Err(ToolError::Execution(scrub(&m, v))),
        Err(other) => Err(ToolError::Execution(scrub(&other.to_string(), v))),
    }
}

/// What a secret fill needs to know about the window that receives it.
struct WindowFacts {
    title: String,
    identity: Option<String>,
}

impl Shared {
    fn fill_secrets(&self) -> Result<Arc<FillSecrets>, ToolError> {
        self.fill_secrets
            .clone()
            .ok_or_else(|| exec_err("no secrets are configured on this daemon"))
    }

    /// The window this run observed, as the backend lists it NOW, with the
    /// verified program identity the backend supplies (`None` when it cannot).
    async fn live_window(&self, run_id: &str) -> Result<WindowFacts, ToolError> {
        let scope = self.observed_scope(run_id)?;
        let backend = self.backend(run_id)?;
        let wins = backend.windows().await.map_err(|e| derr(&e))?;
        let win = wins.into_iter().find(|w| w.id == scope).ok_or_else(|| {
            exec_err("the window that holds the field is gone; call desktop_get_state")
        })?;
        let identity = backend.program_identity(&win).await;
        Ok(WindowFacts {
            title: win.title,
            identity,
        })
    }

    /// Refuse unless the receiving program's verified identity is on the
    /// secret's allow-list.
    async fn verify_window_program(
        &self,
        run_id: &str,
        name: &str,
    ) -> Result<WindowFacts, ToolError> {
        let secrets = self.fill_secrets()?;
        let facts = self.live_window(run_id).await?;
        secrets
            .check_desktop(name, facts.identity.as_deref())
            .map_err(|e| refused(&e))?;
        Ok(facts)
    }

    /// The secret's value, after the live program check.
    async fn secret_for_window(&self, run_id: &str, name: &str) -> Result<SecretValue, ToolError> {
        self.verify_window_program(run_id, name).await?;
        self.fill_secrets()?
            .resolve_value(name)
            .map_err(|e| refused(&e))
    }

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
        let st = states
            .get(run_id)
            .ok_or_else(|| exec_err(NO_OBSERVATION_MSG))?;
        let generation = st.generation.ok_or_else(|| exec_err(NO_OBSERVATION_MSG))?;
        match elem_ref {
            ElementRef::Index(index) => {
                if expected_gen != generation {
                    return Err(exec_err(format!(
                        "index {index} is from generation {expected_gen}, but the current generation is {generation}{STALE_HINT}"
                    )));
                }
                if !st.targets.contains_key(&index) {
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
                    .marks
                    .get(&mark_idx)
                    .copied()
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
    fn describe(
        &self,
        run_id: &str,
        elem_ref: ElementRef,
    ) -> Result<(String, Option<ControlRect>), ToolError> {
        let mut states = self.state.lock();
        let st = states
            .get(run_id)
            .filter(|s| s.generation.is_some())
            .ok_or_else(|| exec_err(NO_OBSERVATION_MSG))?;
        Ok(match elem_ref {
            ElementRef::Index(idx) => st
                .targets
                .get(&idx)
                .cloned()
                .unwrap_or_else(|| (format!("element {idx}"), None)),
            ElementRef::Mark(mark_idx) => (
                format!("mark {mark_idx}"),
                st.marks.get(&mark_idx).map(|(mx, my)| point_rect(*mx, *my)),
            ),
        })
    }

    fn locate(
        &self,
        run_id: &str,
        pointer: &PointerIn,
    ) -> Result<(Target, String, Option<ControlRect>), ToolError> {
        match (&pointer.index, pointer.x, pointer.y) {
            (Some(index), None, None) => {
                let index = parse_ref(index)?;
                let target = self.target(run_id, index, pointer.expect_gen)?;
                let (label, rect) = self.describe(run_id, index)?;
                Ok((target, label, rect))
            }
            (None, Some(x), Some(y)) => {
                let mut states = self.state.lock();
                let state = states
                    .get(run_id)
                    .filter(|s| s.generation.is_some())
                    .ok_or_else(|| exec_err(NO_OBSERVATION_MSG))?;
                let bounds = state
                    .window_bounds
                    .ok_or_else(|| exec_err(NO_OBSERVATION_MSG))?;
                if pointer.expect_gen.is_some() && pointer.expect_gen != state.generation {
                    return Err(exec_err(format!(
                        "point belongs to an older observation{STALE_HINT}"
                    )));
                }
                if !(x >= bounds.x
                    && x < bounds.x + bounds.width
                    && y >= bounds.y
                    && y < bounds.y + bounds.height)
                {
                    return Err(exec_err("point is outside the observed window"));
                }
                Ok((
                    Target::ByCoordinate {
                        x,
                        y,
                        space: CoordinateSpace::Absolute,
                    },
                    format!("screen point ({x:.0}, {y:.0})"),
                    Some(point_rect(x, y)),
                ))
            }
            _ => Err(ToolError::InvalidInput(
                "give one index or both x and y".into(),
            )),
        }
    }

    /// A fresh observation is never taken while a pause, a human takeover or a
    /// local override is in force: the model must not read the screen then.
    fn refuse_observation_while_latched(&self) -> Result<(), ToolError> {
        if let Some(gate) = &self.control
            && let Some(message) = gate.latch_refusal().or_else(|| gate.observation_refused())
        {
            return Err(exec_err(message));
        }
        Ok(())
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
            let Some(gate) = &self.control else {
                return Ok(ActionPermit::default());
            };
            let request = ControlRequest {
                run_id: run_id.to_owned(),
                rect,
                label,
                action: action.to_owned(),
            };
            crate::control::admit_or_inherit(gate.as_ref(), || gate.admit_action(&request))
                .await
                .map_err(exec_err)
        }
        .instrument(tracing::info_span!("desktop.gate", run_id, action))
        .await
    }

    /// An unobserved run has no preferred window: `snapshot(None)` observes the
    /// backend's foreground window, never the most recently observed other run.
    fn scope(&self, run_id: &str) -> Option<u64> {
        self.state
            .lock()
            .get(run_id)
            .filter(|s| s.generation.is_some())
            .and_then(|s| s.window_id)
    }

    fn observed_scope(&self, run_id: &str) -> Result<u64, ToolError> {
        self.scope(run_id)
            .ok_or_else(|| exec_err(NO_OBSERVATION_MSG))
    }

    fn backend(&self, run_id: &str) -> Result<Arc<dyn DesktopBackend>, ToolError> {
        match &self.control {
            Some(gate) => gate.desktop_for_run(run_id),
            None => self.default_backend.clone(),
        }
        .ok_or_else(|| exec_err("desktop backend is unavailable for this run"))
    }

    async fn act(
        &self,
        run_id: &str,
        action: Action,
        permit: &ActionPermit,
        guard: Option<&ActGuard>,
    ) -> Result<String, ToolError> {
        let backend = self.backend(run_id)?;
        let guard = guard.cloned();
        let res = permit
            .protect(async move {
                match guard {
                    // Covered by a run-scoped control grant: the backend checks
                    // the window right before the OS call and may refuse.
                    Some(g) => backend.act_guarded(action, g).await,
                    None => backend.act(action).await,
                }
            })
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
            let backend = self.backend(run_id)?;
            let observation_epoch=self.control.as_ref().map_or(0,|gate|gate.observation_epoch());
            let snap = backend.snapshot(scope).await.map_err(|e| derr(&e))?;
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
                st.window_bounds = Some(snap.window.bounds);
                st.generation = Some(snap.generation);
                st.observation_epoch = observation_epoch;
                st.marks.clear();
                st.secure = rendered_nodes
                    .iter()
                    .filter(|n| n.states.iter().any(|s| s == "secure"))
                    .map(|n| n.index)
                    .collect();
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
        let Ok(backend) = self.backend(run_id) else {
            return "\n(accessibility tree is weak; desktop backend is unavailable)\n".into();
        };
        if !backend.capabilities().screenshot {
            return "\n(accessibility tree is weak; screenshots are unavailable)\n".into();
        }
        match self.build_marks(backend, run_id, snap, ocr).await {
            Ok(s) => s,
            Err(e) => format!("\n(accessibility tree is weak; OCR failed: {e})\n"),
        }
    }

    async fn build_marks(
        &self,
        backend: Arc<dyn DesktopBackend>,
        run_id: &str,
        snap: &Snapshot,
        ocr: SharedOcr,
    ) -> Result<String, String> {
        let bounds = snap.window.bounds;
        let image = backend
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
        let scope = self.observed_scope(run_id)?;
        let state = self
            .refresh(run_id, Some(scope), &ObsFilter::default())
            .await?;
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
        run_id: &str,
        before: &HashSet<u64>,
        pid: u32,
        deadline: tokio::time::Instant,
    ) -> Result<Vec<desktop_api::WindowInfo>, ToolError> {
        let backend = self.backend(run_id)?;
        let matches_launch = |w: &desktop_api::WindowInfo| -> bool {
            !before.contains(&w.id) && !w.title.is_empty() && (pid == 0 || w.pid == pid)
        };
        async {
            loop {
                let wins = backend.windows().await.map_err(|e| derr(&e))?;
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

/// Build the desktop tools with a fixed default backend.
#[must_use]
pub fn tools(
    backend: Arc<dyn DesktopBackend>,
    ocr: Option<SharedOcr>,
    data_dir: &Path,
    settings: DesktopSettings,
    control: Option<Arc<dyn DesktopControlGate>>,
) -> Vec<Arc<dyn Tool>> {
    tools_dynamic(Some(backend), ocr, data_dir, settings, control)
}

/// Build the desktop tools with optional fallback backend and per-run routing
/// via [`DesktopControlGate::desktop_for_run`].
#[must_use]
pub fn tools_dynamic(
    default_backend: Option<Arc<dyn DesktopBackend>>,
    ocr: Option<SharedOcr>,
    data_dir: &Path,
    settings: DesktopSettings,
    control: Option<Arc<dyn DesktopControlGate>>,
) -> Vec<Arc<dyn Tool>> {
    tools_dynamic_with_secrets(default_backend, ocr, data_dir, settings, control, None)
}

/// [`tools_dynamic`] whose `desktop_type` / `desktop_set_value` may type the
/// owner's fill secrets (only into a program with a verified identity on the
/// secret's allow-list).
#[must_use]
pub fn tools_dynamic_with_secrets(
    default_backend: Option<Arc<dyn DesktopBackend>>,
    ocr: Option<SharedOcr>,
    data_dir: &Path,
    settings: DesktopSettings,
    control: Option<Arc<dyn DesktopControlGate>>,
    fill_secrets: Option<Arc<FillSecrets>>,
) -> Vec<Arc<dyn Tool>> {
    let shared = Arc::new(Shared {
        default_backend,
        ocr,
        state: Mutex::new(RunStates::default()),
        data_dir: data_dir.to_path_buf(),
        settings,
        control,
        fill_secrets,
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
        "description":"How to deliver the input. Default accessibility for indexes; global (moves the real cursor) for marks. `app_post` is not implemented on Windows or Linux yet, and the call fails."
    });
    let defs: Vec<(Op, ToolSpec)> = vec![
        (
            Op::ListApps,
            spec(
                "desktop_list_apps",
                "List visible windows (open apps, programs) with id, pid and title; the foreground one is marked focused. Cheapest way to see what the user is doing or watching now: a browser window title holds the page or video name. Use it for \"what is on my screen / what am I watching\" before anything else. Example: {}",
                json!({"type":"object","properties":{}}),
                Effect::Read,
                d,
            ),
        ),
        (
            Op::Launch,
            spec(
                "desktop_launch",
                "Launch an application or open a document or URI on the selected interactive desktop. Examples: {\"executable\":\"notepad.exe\"}, {\"executable\":\"ms-settings:display\"}, {\"executable\":\"appx:Microsoft.WindowsCalculator_8wekyb3d8bbwe!App\"}. For a packaged Windows app use appx: followed by its installed AppUserModelID, without args. Use desktop_focus for a window that is already open. Use browser tools for web pages.",
                json!({"type":"object","properties":{
                    "executable":{"type":"string","description":"Program name, file path or URI such as ms-settings:display."},
                    "args":{"type":"array","items":{"type":"string"},"description":"Arguments."}
                },"required":["executable"]}),
                Effect::Write,
                d,
            ),
        ),
        (
            Op::Focus,
            spec(
                "desktop_focus",
                "Bring an existing desktop window to the foreground and read its accessibility tree. Pass the window_id from desktop_list_apps. Example: {\"window_id\":3}",
                json!({"type":"object","properties":{"window_id":{"type":"integer"}},"required":["window_id"]}),
                Effect::Write,
                d,
            ),
        ),
        (
            Op::GetState,
            spec(
                "desktop_get_state",
                "Read one window's accessibility tree with element names, roles and screen bounds. Pass the window_id from desktop_list_apps, then use its element indexes and gen in desktop actions. Use interactive_only or query to shrink a large tree. Add screenshot only if accessibility does not expose the needed target. Example: {\"window_id\":3,\"interactive_only\":true}",
                json!({"type":"object","properties":{
                    "window_id":{"type":"integer","description":"Window id from desktop_list_apps."},
                    "title":{"type":"string","description":"Part of the window title."},
                    "screenshot":{"type":"boolean","description":"Also save a screenshot scoped to the observed window and return its file path."},
                    "interactive_only":{"type":"boolean","description":"Only list interactive elements (buttons, fields, links...) and the focused one."},
                    "query":{"type":"string","description":"Only list elements whose name, value or role contains this text (case-insensitive)."}
                }}),
                Effect::Read,
                d,
            ),
        ),
        (
            Op::Wait,
            spec(
                "desktop_wait",
                "Wait up to 10 seconds for a window by title or ID, or for an element name or role within it. Start with desktop_list_apps to establish screen access. Example: {\"title\":\"WhatsApp\",\"timeout_ms\":5000}",
                json!({"type":"object","properties":{
                    "window_id":{"type":"integer"},
                    "title":{"type":"string","description":"Part of the window title."},
                    "query":{"type":"string","description":"Part of the element name or role."},
                    "timeout_ms":{"type":"integer","minimum":100,"maximum":10000}
                }}),
                Effect::Read,
                d,
            ),
        ),
        (
            Op::Click,
            spec(
                "desktop_click",
                "Click by element index and gen. If the accessibility tree lacks the target, pass screen x and y within the observed window. Returns the refreshed UI tree. Example: {\"index\":12,\"gen\":3}",
                json!({"type":"object","properties":{
                    "index":index_prop,"gen":gen_prop,"method":method_prop,
                    "x":{"type":"number","description":"Physical screen x within the observed window."},
                    "y":{"type":"number","description":"Physical screen y within the observed window."}
                },"oneOf":[{"required":["index","gen"]},{"required":["x","y"]}]}),
                Effect::Write,
                d,
            ),
        ),
        (
            Op::Type,
            spec(
                "desktop_type",
                "Focus an input by index and gen, or by screen x and y within the observed window, then type text. An owner-stored secret requires an element index. Password fields are refused on Windows. Returns the refreshed UI tree. Example: {\"index\":5,\"gen\":3,\"text\":\"hello\"}",
                json!({"type":"object","properties":{
                    "index":index_prop,
                    "gen":gen_prop,
                    "x":{"type":"number","description":"Physical screen x within the observed window."},
                    "y":{"type":"number","description":"Physical screen y within the observed window."},
                    "text":{"type":"string","description":"Text to type. Not with `secret`."},
                    "secret":{"type":"string","description":"Name of an owner-stored secret to type instead of `text`."}
                },"oneOf":[{"required":["index","gen"]},{"required":["x","y","text"]}]}),
                Effect::Write,
                d,
            ),
        ),
        (
            Op::SetValue,
            spec(
                "desktop_set_value",
                "Replace the value of a text field or combo box. Give exactly one of `value` or `secret` (the name of a secret the owner stored, from secrets_list: it is set for you and you never see the value). Pass gen from the observation you read the index from. Example: {\"index\":5,\"gen\":3,\"value\":\"hello\"}",
                json!({"type":"object","properties":{
                    "index":index_prop,
                    "gen":gen_prop,
                    "value":{"type":"string","description":"New value. Not with `secret`."},
                    "secret":{"type":"string","description":"Name of an owner-stored secret to set instead of `value`."}
                },"required":["index","gen"]}),
                Effect::Write,
                d,
            ),
        ),
        (
            Op::Key,
            spec(
                "desktop_key",
                "Press a keyboard key or shortcut chord in the observed window. Optionally focus a target first with index and gen, or with screen x and y. Example: {\"keys\":[\"ctrl\",\"s\"]}",
                json!({"type":"object","properties":{
                    "keys":{"description":"Key names, e.g. [\"ctrl\",\"s\"], [\"Return\"] or \"ctrl+s\".","oneOf":[
                        {"type":"array","items":{"type":"string"},"minItems":1},{"type":"string"}]},
                    "index":index_prop,
                    "gen":gen_prop,
                    "x":{"type":"number","description":"Physical screen x within the observed window."},
                    "y":{"type":"number","description":"Physical screen y within the observed window."}
                },"required":["keys"]}),
                Effect::Write,
                d,
            ),
        ),
        (
            Op::Scroll,
            spec(
                "desktop_scroll",
                "Scroll by element index and gen, or by screen x and y within the observed window. Positive dy scrolls down. Example: {\"index\":3,\"gen\":2,\"dy\":5}",
                json!({"type":"object","properties":{
                    "index":index_prop,
                    "gen":gen_prop,
                    "x":{"type":"number","description":"Screen x (when no index)."},
                    "y":{"type":"number","description":"Screen y (when no index)."},
                    "dx":{"type":"number","description":"Horizontal amount."},
                    "dy":{"type":"number","description":"Vertical amount."}
                }}),
                Effect::Write,
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
struct FocusIn {
    window_id: u64,
}

#[derive(Deserialize)]
struct WaitIn {
    window_id: Option<u64>,
    title: Option<String>,
    query: Option<String>,
    timeout_ms: Option<u64>,
}

#[derive(Deserialize)]
struct ClickIn {
    #[serde(flatten)]
    pointer: PointerIn,
    method: Option<String>,
}

#[derive(Deserialize)]
struct PointerIn {
    index: Option<Value>,
    #[serde(rename = "gen")]
    expect_gen: Option<u64>,
    x: Option<f64>,
    y: Option<f64>,
}

#[derive(Deserialize)]
struct SetValueIn {
    index: Value,
    #[serde(rename = "gen")]
    expect_gen: Option<u64>,
}

#[derive(Deserialize)]
struct KeyIn {
    keys: Value,
    #[serde(flatten)]
    pointer: PointerIn,
}

#[derive(Deserialize)]
struct ScrollIn {
    #[serde(flatten)]
    pointer: PointerIn,
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

async fn finish_control_target(
    backend: &dyn DesktopBackend,
    kind: ControlKind,
    refusal: Option<String>,
    focus_matters: bool,
) -> Option<ControlTarget> {
    let mut t = backend.input_target().await.ok()??;
    if !focus_matters {
        t.password_focus = false;
    }
    let refusal = refusal.or_else(|| desktop_api::guard::judge(&t).map(|r| r.code().to_owned()));
    Some(ControlTarget {
        kind,
        identity: ControlIdentity {
            image_path: t.identity.image_path,
            pid: t.identity.pid,
            start_time: t.identity.start_time,
        },
        window_title: card_title(&t.title),
        refusal,
    })
}

/// The program a desktop input step would reach, for callers that send the
/// input themselves (workflow replay). `keys` is the chord of a key step;
/// `secure_element` says the step's element is a password field; a key step
/// without an element goes to the live focus (`focus_matters`). `None` when
/// the backend cannot name a program: no grant is offered.
pub async fn resolve_input_target(
    backend: &dyn DesktopBackend,
    kind: ControlKind,
    keys: Option<&[String]>,
    secure_element: bool,
    focus_matters: bool,
) -> Option<ControlTarget> {
    let mut refusal = None;
    if let Some(keys) = keys
        && let Some(r) = backend.key_chord_refusal(keys)
    {
        refusal = Some(r.code().to_owned());
    }
    if secure_element {
        refusal.get_or_insert_with(|| desktop_api::Refusal::PasswordField.code().to_owned());
    }
    finish_control_target(backend, kind, refusal, focus_matters).await
}

/// The backend guard for a program the loop named in `ToolContext::control_guard`.
fn act_guard_for(id: &ControlIdentity) -> ActGuard {
    ActGuard {
        expect: desktop_api::ProcessIdentity {
            image_path: id.image_path.clone(),
            pid: id.pid,
            start_time: id.start_time,
        },
    }
}

/// Window title for a card: no control characters, bounded.
fn card_title(title: &str) -> String {
    title
        .chars()
        .filter(|c| !c.is_control())
        .take(120)
        .collect()
}

impl DesktopTool {
    /// The kind of input this tool sends, for the three tools a run-scoped
    /// control grant can ever cover. Click, scroll and drag have no target:
    /// a drag always asks.
    fn control_kind(&self) -> Option<ControlKind> {
        match self.op {
            Op::Type => Some(ControlKind::Type),
            Op::SetValue => Some(ControlKind::SetValue),
            Op::Key => Some(ControlKind::Key),
            _ => None,
        }
    }

    /// Resolve, before the permission decision, which program this call would
    /// send input to. `None` when the backend cannot name a program (Linux,
    /// macOS, a remote computer): no grant is offered and the call behaves as
    /// without this feature. The chord, password-field and window-class
    /// verdicts are carried as `refusal`; the backend checks them again right
    /// before it sends.
    async fn resolve_control_target(&self, run_id: &str, input: &Value) -> Option<ControlTarget> {
        let kind = self.control_kind()?;
        let sh = &self.shared;
        let backend = sh.backend(run_id).ok()?;
        let secure_ref = |v: &Value| -> bool {
            match parse_ref(v) {
                Ok(ElementRef::Index(i)) => sh
                    .state
                    .lock()
                    .get(run_id)
                    .is_some_and(|st| st.secure.contains(&i)),
                _ => false,
            }
        };
        let mut refusal: Option<String> = None;
        let mut focus_matters = false;
        match self.op {
            Op::Key => {
                match input.get("keys").map(parse_keys) {
                    Some(Ok(keys)) => {
                        if let Some(r) = backend.key_chord_refusal(&keys) {
                            refusal = Some(r.code().to_owned());
                        }
                    }
                    _ => refusal = Some(desktop_api::Refusal::Chord.code().to_owned()),
                }
                match input.get("index") {
                    // A key press that first clicks an element is a click, and a
                    // task-wide permission never covers a click: it asks.
                    Some(idx) if !idx.is_null() => return None,
                    // No element: the keys go to whatever has the focus.
                    _ => focus_matters = true,
                }
            }
            Op::Type | Op::SetValue if input.get("index").is_some_and(secure_ref) => {
                refusal = Some(desktop_api::Refusal::PasswordField.code().to_owned());
            }
            _ => {}
        }
        finish_control_target(backend.as_ref(), kind, refusal, focus_matters).await
    }

    /// Observation tools discard their result if control changed while they
    /// ran: a screenshot, an OCR pass or a window list that straddles the start
    /// of a human takeover never reaches the model.
    async fn run(
        &self,
        run_id: &str,
        input: Value,
        guard: Option<ActGuard>,
    ) -> Result<String, ToolError> {
        let observing = matches!(self.op, Op::ListApps | Op::GetState | Op::Wait);
        let gate = self.shared.control.clone();
        let epoch = gate.as_ref().map(|g| g.observation_epoch());
        let text = self.run_inner(run_id, input, guard.as_ref()).await?;
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

    async fn run_inner(
        &self,
        run_id: &str,
        input: Value,
        guard: Option<&ActGuard>,
    ) -> Result<String, ToolError> {
        let sh = &self.shared;
        if matches!(self.op, Op::ListApps | Op::GetState | Op::Wait)
            && let Some(message) = sh.control.as_ref().and_then(|g| g.observation_refused())
        {
            return Err(exec_err(message));
        }
        match self.op {
            Op::ListApps => {
                let backend = sh.backend(run_id)?;
                let wins = backend
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
                if let Some(reason) = foreign_launch_refusal(
                    &sh.backend(run_id)?.capabilities().platform,
                    &i.executable,
                ) {
                    return Err(exec_err(reason));
                }
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
                    sh.refuse_observation_while_latched()?;
                    let fresh = sh.refresh(run_id, None, &ObsFilter::default()).await?;
                    permit.observed();
                    return Ok(format!(
                        "Human intervention completed; previous action discarded. Fresh observation:\n{fresh}"
                    ));
                }
                let backend = sh.backend(run_id)?;
                let before: HashSet<u64> = backend
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
                let wins = sh
                    .poll_launched_windows(run_id, &before, pid, deadline)
                    .await?;
                let mut out = format!("launched pid={pid}\nwindows:\n");
                for w in wins.iter().take(MAX_WINDOWS_SHOWN) {
                    let _ = writeln!(out, "{}", window_line(w));
                }
                Ok(out)
            }
            Op::Focus => {
                let FocusIn { window_id } = parse(input)?;
                let backend = sh.backend(run_id)?;
                let window = backend
                    .windows()
                    .await
                    .map_err(|e| derr(&e))?
                    .into_iter()
                    .find(|w| w.id == window_id)
                    .ok_or_else(|| exec_err("window not found; list apps again"))?;
                let permit = sh
                    .gate(run_id, "focus", window.title, Some(rect_of(window.bounds)))
                    .await?;
                if permit.needs_observation() {
                    return Err(exec_err(
                        "Control changed; list apps again before focusing a window",
                    ));
                }
                permit
                    .protect(async move { backend.focus(window_id, None).await })
                    .await
                    .map_err(exec_err)?
                    .map_err(|e| derr(&e))?;
                let tree = sh
                    .refresh(run_id, Some(window_id), &ObsFilter::default())
                    .await?;
                Ok(format!("focused window id={window_id}\n{tree}"))
            }
            Op::Wait => {
                let WaitIn {
                    window_id,
                    title,
                    query,
                    timeout_ms,
                } = parse(input)?;
                let title = title
                    .map(|s| s.trim().to_lowercase())
                    .filter(|s| !s.is_empty());
                let query = query
                    .map(|s| s.trim().to_lowercase())
                    .filter(|s| !s.is_empty());
                let window_id =
                    window_id.or_else(|| title.is_none().then(|| sh.scope(run_id)).flatten());
                if title.is_none() && window_id.is_none() {
                    return Err(ToolError::InvalidInput(
                        "supply a window_id or title".into(),
                    ));
                }
                let backend = sh.backend(run_id)?;
                let ms = timeout_ms.unwrap_or(3000).clamp(100, 10000);
                tokio::time::timeout(Duration::from_millis(ms), async {
                    loop {
                        sh.refuse_observation_while_latched()?;
                        let windows = backend.windows().await.map_err(|e| derr(&e))?;
                        let window = windows.iter().find(|w| {
                            window_id.is_none_or(|id| id == w.id)
                                && title.as_ref().is_none_or(|t| w.title.to_lowercase().contains(t))
                        });
                        if let Some(w) = window {
                            let Some(needle) = &query else { return Ok(window_line(w)); };
                            let tree = backend.snapshot(Some(w.id)).await.map_err(|e| derr(&e))?;
                            if tree.nodes.iter().any(|n| n.name.to_lowercase().contains(needle)
                                || n.role.to_lowercase().contains(needle)) {
                                return Ok(format!("matching element found in {}\nRead desktop_get_state for its current index and gen.", window_line(w)));
                            }
                        }
                        tokio::time::sleep(Duration::from_millis(200)).await;
                    }
                }).await.map_err(|_| exec_err(format!("window or element did not appear within {ms} ms")))?
            }
            Op::GetState => {
                let i: StateIn = parse(input)?;
                let scope = if let Some(id) = i.window_id {
                    Some(id)
                } else if let Some(t) = &i.title {
                    let needle = t.to_lowercase();
                    let backend = sh.backend(run_id)?;
                    let wins = backend.windows().await.map_err(|e| derr(&e))?;
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
                    let window_id = sh.observed_scope(run_id)?;
                    let region = Region::Window { window_id };
                    let backend = sh.backend(run_id)?;
                    let img = backend
                        .screenshot(region, Quality::Medium)
                        .await
                        .map_err(|e| derr(&e))?;
                    let (ox, oy) = {
                        let mut states = sh.state.lock();
                        states
                            .get(run_id)
                            .and_then(|st| st.window_bounds)
                            .map_or((0.0, 0.0), |bounds| (bounds.x, bounds.y))
                    };
                    let capture_w = f64::from(img.width);
                    let capture_h = f64::from(img.height);
                    let img = shrink_screenshot_for_model(img);
                    let ext = if img.mime_type == "image/png" {
                        "png"
                    } else {
                        "jpg"
                    };
                    let path = save_file(&sh.data_dir.join("screenshots"), ext, &img.data)?;
                    let _ = writeln!(text, "screenshot: {path} ({}x{})", img.width, img.height);
                    let note = screenshot_coordinate_line(
                        ox, oy, capture_w, capture_h, img.width, img.height,
                    );
                    let _ = writeln!(text, "{note}");
                }
                Ok(text)
            }
            Op::Click => {
                let i: ClickIn = parse(input)?;
                let (target, label, rect) = sh.locate(run_id, &i.pointer)?;
                let default_method = match target {
                    Target::ByIndex { .. } => ClickMethod::Accessibility,
                    Target::ByCoordinate { .. } => ClickMethod::Global,
                };
                let method = parse_method(i.method.as_deref(), default_method)?;
                let permit = sh.gate(run_id, "click", label, rect).await?;
                if permit.needs_observation()
                    || sh.state.lock().get(run_id).is_some_and(|s| {
                        s.observation_epoch
                            != sh.control.as_ref().map_or(0, |g| g.observation_epoch())
                    })
                {
                    sh.state.lock().map.remove(run_id);
                    sh.refuse_observation_while_latched()?;
                    let fresh = sh.refresh(run_id, None, &ObsFilter::default()).await?;
                    permit.observed();
                    return Ok(format!(
                        "Human intervention completed; previous action discarded. Fresh observation:\n{fresh}"
                    ));
                }
                let head = sh
                    .act(run_id, Action::Click { target, method }, &permit, None)
                    .await?;
                sh.settle_and_refresh(run_id, head).await
            }
            Op::Type => {
                let what = source(&input, "text")?;
                let pointer: PointerIn = parse(input)?;
                if matches!(&what, Source::Secret(_)) && pointer.index.is_none() {
                    return Err(ToolError::InvalidInput(
                        "secret input needs an element index".into(),
                    ));
                }
                let (target, label, rect) = sh.locate(run_id, &pointer)?;
                let permit = sh.gate(run_id, "type", label, rect).await?;
                if permit.needs_observation()
                    || sh.state.lock().get(run_id).is_some_and(|s| {
                        s.observation_epoch
                            != sh.control.as_ref().map_or(0, |g| g.observation_epoch())
                    })
                {
                    sh.state.lock().map.remove(run_id);
                    sh.refuse_observation_while_latched()?;
                    let fresh = sh.refresh(run_id, None, &ObsFilter::default()).await?;
                    permit.observed();
                    return Ok(format!(
                        "Human intervention completed; previous action discarded. Fresh observation:\n{fresh}"
                    ));
                }
                // A secret is resolved only now (after the approval and the
                // live program check), held in memory and wiped on drop.
                let secret = match &what {
                    Source::Text(_) => None,
                    Source::Secret(name) => Some(sh.secret_for_window(run_id, name).await?),
                };
                // Focus the element with a real click, then type into the focus.
                let typed = async {
                    sh.act(
                        run_id,
                        Action::Click {
                            target,
                            method: ClickMethod::Global,
                        },
                        &permit,
                        guard,
                    )
                    .await?;
                    let text = match (&what, &secret) {
                        (Source::Text(t), _) => t.clone(),
                        (Source::Secret(name), Some(v)) => {
                            // The program that owns the window is verified again
                            // right before the keys are sent.
                            sh.verify_window_program(run_id, name).await?;
                            v.expose().to_owned()
                        }
                        (Source::Secret(_), None) => String::new(),
                    };
                    let head = sh
                        .act(run_id, Action::TypeText { text }, &permit, guard)
                        .await?;
                    sh.settle_and_refresh(run_id, head).await
                }
                .await;
                finish_secret(typed, secret.as_ref())
            }
            Op::SetValue => {
                let what = source(&input, "value")?;
                let i: SetValueIn = parse(input)?;
                if i.expect_gen.is_none() {
                    return Err(ToolError::InvalidInput(MISSING_GEN_MSG.into()));
                }
                let r = parse_ref(&i.index)?;
                let target = sh.target(run_id, r, i.expect_gen)?;
                let (label, rect) = sh.describe(run_id, r)?;
                let permit = sh.gate(run_id, "set_value", label, rect).await?;
                if permit.needs_observation()
                    || sh.state.lock().get(run_id).is_some_and(|s| {
                        s.observation_epoch
                            != sh.control.as_ref().map_or(0, |g| g.observation_epoch())
                    })
                {
                    sh.state.lock().map.remove(run_id);
                    sh.refuse_observation_while_latched()?;
                    let fresh = sh.refresh(run_id, None, &ObsFilter::default()).await?;
                    permit.observed();
                    return Ok(format!(
                        "Human intervention completed; previous action discarded. Fresh observation:\n{fresh}"
                    ));
                }
                let secret = match &what {
                    Source::Text(_) => None,
                    Source::Secret(name) => Some(sh.secret_for_window(run_id, name).await?),
                };
                let set = async {
                    let value = match (&what, &secret) {
                        (Source::Text(t), _) => t.clone(),
                        (Source::Secret(_), Some(v)) => v.expose().to_owned(),
                        (Source::Secret(_), None) => String::new(),
                    };
                    let head = sh
                        .act(run_id, Action::SetValue { target, value }, &permit, guard)
                        .await?;
                    sh.settle_and_refresh(run_id, head).await
                }
                .await;
                finish_secret(set, secret.as_ref())
            }
            Op::Key => {
                let i: KeyIn = parse(input)?;
                let keys = parse_keys(&i.keys)?;
                // Even a shortcut without an element index needs an observation
                // owned by this run before it can affect the shared desktop.
                sh.observed_scope(run_id)?;
                let pointer = &i.pointer;
                let (focus_target, label, rect) =
                    if pointer.index.is_some() || pointer.x.is_some() || pointer.y.is_some() {
                        let (target, label, rect) = sh.locate(run_id, pointer)?;
                        (Some(target), format!("{} in {label}", keys.join("+")), rect)
                    } else {
                        (None, keys.join("+"), None)
                    };
                let permit = sh.gate(run_id, "key", label, rect).await?;
                if permit.needs_observation()
                    || sh.state.lock().get(run_id).is_some_and(|s| {
                        s.observation_epoch
                            != sh.control.as_ref().map_or(0, |g| g.observation_epoch())
                    })
                {
                    sh.state.lock().map.remove(run_id);
                    sh.refuse_observation_while_latched()?;
                    let fresh = sh.refresh(run_id, None, &ObsFilter::default()).await?;
                    permit.observed();
                    return Ok(format!(
                        "Human intervention completed; previous action discarded. Fresh observation:\n{fresh}"
                    ));
                }
                if let Some(target) = focus_target {
                    sh.act(
                        run_id,
                        Action::Click {
                            target,
                            method: ClickMethod::Global,
                        },
                        &permit,
                        guard,
                    )
                    .await?;
                }
                let head = sh
                    .act(run_id, Action::KeyPress { keys }, &permit, guard)
                    .await?;
                sh.settle_and_refresh(run_id, head).await
            }
            Op::Scroll => {
                let i: ScrollIn = parse(input)?;
                let (target, label, rect) = sh.locate(run_id, &i.pointer)?;
                let permit = sh.gate(run_id, "scroll", label, rect).await?;
                if permit.needs_observation()
                    || sh.state.lock().get(run_id).is_some_and(|s| {
                        s.observation_epoch
                            != sh.control.as_ref().map_or(0, |g| g.observation_epoch())
                    })
                {
                    sh.state.lock().map.remove(run_id);
                    sh.refuse_observation_while_latched()?;
                    let fresh = sh.refresh(run_id, None, &ObsFilter::default()).await?;
                    permit.observed();
                    return Ok(format!(
                        "Human intervention completed; previous action discarded. Fresh observation:\n{fresh}"
                    ));
                }
                let head = sh
                    .act(
                        run_id,
                        Action::Scroll {
                            target,
                            delta_x: i.dx,
                            delta_y: i.dy,
                        },
                        &permit,
                        None,
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
                let ((la, ra), (lb, rb)) = (sh.describe(run_id, a)?, sh.describe(run_id, b)?);
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
                    sh.refuse_observation_while_latched()?;
                    let fresh = sh.refresh(run_id, None, &ObsFilter::default()).await?;
                    permit.observed();
                    return Ok(format!(
                        "Human intervention completed; previous action discarded. Fresh observation:\n{fresh}"
                    ));
                }
                let head = sh
                    .act(run_id, Action::Drag { from, to, method }, &permit, None)
                    .await?;
                sh.settle_and_refresh(run_id, head).await
            }
        }
    }
}

impl DesktopTool {
    /// The text key of this tool's secret-capable parameter.
    fn text_key(&self) -> Option<&'static str> {
        match self.op {
            Op::Type => Some("text"),
            Op::SetValue => Some("value"),
            _ => None,
        }
    }
}

#[async_trait]
impl Tool for DesktopTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn control_target(&self, ctx: &ToolContext, input: &Value) -> Option<ControlTarget> {
        self.resolve_control_target(&ctx.run_id, input).await
    }

    /// Typing an owner-stored secret is confirmed on every call, however
    /// permissive the policy, preset, grants or read shortcuts are. A desktop
    /// program never has a standing grant (the default `standing_grant`).
    fn always_ask_for(&self, input: &Value) -> bool {
        self.text_key().is_some() && names_a_secret(input)
    }

    async fn approval_details(
        &self,
        ctx: &ToolContext,
        input: &Value,
    ) -> Result<Option<Value>, ToolError> {
        if matches!(self.op, Op::Launch) {
            // A program of another operating system (a Linux path on a Windows PC)
            // is refused here, BEFORE any approval card: the user is never asked
            // about a call that cannot work.
            if let Some(exe) = input.get("executable").and_then(Value::as_str)
                && let Ok(backend) = self.shared.backend(&ctx.run_id)
                && let Some(reason) = foreign_launch_refusal(&backend.capabilities().platform, exe)
            {
                return Err(ToolError::Execution(reason));
            }
            return Ok(None);
        }
        let Some(key) = self.text_key() else {
            return Ok(None);
        };
        let Source::Secret(name) = source(input, key)? else {
            return Ok(None);
        };
        let sh = &self.shared;
        let r = parse_ref(
            input
                .get("index")
                .ok_or_else(|| ToolError::InvalidInput("missing index".into()))?,
        )?;
        let expect_gen = input.get("gen").and_then(Value::as_u64);
        if expect_gen.is_none() {
            return Err(ToolError::InvalidInput(MISSING_GEN_MSG.into()));
        }
        sh.target(&ctx.run_id, r, expect_gen)?;
        let (label, _) = sh.describe(&ctx.run_id, r)?;
        // Refuse here, BEFORE any approval is offered, when the receiving
        // program is not one the owner allowed for this secret.
        let facts = sh.verify_window_program(&ctx.run_id, &name).await?;
        Ok(Some(json!({
            "kind": "secret_fill",
            "tool": self.spec.name,
            "secret": sanitize_label(&name, 80),
            "field": sanitize_label(&label, 80),
            "window": sanitize_label(&facts.title, 80),
            "program": sanitize_label(facts.identity.as_deref().unwrap_or_default(), 80),
            "has_standing_grant": false,
            "note": "The secret value is typed by Praxi; the assistant never sees it.",
        })))
    }

    async fn call(&self, ctx: &ToolContext, input: Value) -> Result<ToolOutput, ToolError> {
        // Only the four input tools take part in a control grant; any other
        // tool ignores a guard.
        let guard = if self.control_kind().is_some() {
            ctx.control_guard.as_ref().map(act_guard_for)
        } else {
            None
        };
        let text = self
            .run(&ctx.run_id, input, guard)
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

    /// A callback a test runs inside a backend call.
    pub type FakeHook = Arc<dyn Fn(&str) + Send + Sync>;

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
        /// Runs inside `screenshot` / `windows` (a takeover landing mid-capture).
        pub hook: Mutex<Option<FakeHook>>,
        /// The window and program input would reach now. `None` = this fake
        /// cannot name a program (like Linux today): no grant, no guard.
        pub input: Mutex<Option<desktop_api::InputTarget>>,
        /// Actions that passed through `act_guarded` (and then ran).
        pub guarded: Mutex<Vec<Action>>,
        /// The edit (index 3) is a password field in the snapshot.
        pub secure_edit: bool,
        /// The verified program identity this backend reports (`None` = it cannot verify).
        pub identity: Mutex<Option<String>>,
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
                hook: Mutex::new(None),
                input: Mutex::new(None),
                guarded: Mutex::new(Vec::new()),
                secure_edit: false,
                identity: Mutex::new(None),
            }
        }

        /// A program that input would reach, with a complete identity.
        pub fn notepad() -> desktop_api::InputTarget {
            desktop_api::InputTarget {
                window_id: 77,
                title: "Untitled - Notepad".into(),
                class: "Notepad".into(),
                identity: desktop_api::ProcessIdentity {
                    image_path: "C:\\Windows\\System32\\notepad.exe".into(),
                    pid: 4242,
                    start_time: 1_000,
                },
                elevated: Some(false),
                password_focus: false,
                owned: false,
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
            let hook = self.hook.lock().clone();
            if let Some(hook) = hook {
                hook("snapshot");
            }
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
                            if i == 3 && self.secure_edit {
                                vec!["secure".to_owned()]
                            } else {
                                vec![]
                            },
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
            let hook = self.hook.lock().clone();
            if let Some(hook) = hook {
                hook("act");
            }
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

        async fn input_target(&self) -> Result<Option<desktop_api::InputTarget>, DesktopError> {
            Ok(self.input.lock().clone())
        }

        async fn act_guarded(
            &self,
            action: Action,
            guard: desktop_api::ActGuard,
        ) -> Result<ActionResult, DesktopError> {
            // The same decision a real backend takes right before it sends.
            let now = self.input.lock().clone().ok_or_else(|| {
                DesktopError::GuardRefused(format!(
                    "not sent: {}",
                    desktop_api::Refusal::NoIdentity.code()
                ))
            })?;
            desktop_api::guard::check_guard(&guard, &now)
                .map_err(|r| DesktopError::GuardRefused(format!("not sent: {}", r.code())))?;
            self.guarded.lock().push(action.clone());
            self.act(action).await
        }

        fn key_chord_refusal(&self, keys: &[String]) -> Option<desktop_api::Refusal> {
            // The same allowlist the real backends use.
            (!desktop_api::guarded::chord_allowed(keys)).then_some(desktop_api::Refusal::Chord)
        }

        async fn screenshot(&self, _r: Region, _q: Quality) -> Result<Image, DesktopError> {
            let hook = self.hook.lock().clone();
            if let Some(hook) = hook {
                hook("screenshot");
            }
            Ok(Image {
                data: self.screenshot_png.clone(),
                width: 10,
                height: 10,
                mime_type: "image/png".into(),
            })
        }

        async fn windows(&self) -> Result<Vec<WindowInfo>, DesktopError> {
            let hook = self.hook.lock().clone();
            if let Some(hook) = hook {
                hook("windows");
            }
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

        async fn program_identity(&self, _window: &WindowInfo) -> Option<String> {
            self.identity.lock().clone()
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
    #[test]
    fn a_program_of_another_os_is_refused_before_any_card() {
        let linux_on_windows =
            foreign_launch_refusal("windows", "/opt/praxi-browser/chrome").unwrap();
        assert!(
            linux_on_windows.starts_with("wrong computer:")
                && linux_on_windows.contains("Windows computer")
        );
        assert!(
            foreign_launch_refusal("linux", "chrome.exe")
                .unwrap()
                .contains("Linux computer")
        );
        assert!(foreign_launch_refusal("linux", r"C:\Program Files\x.exe").is_some());
        assert!(foreign_launch_refusal("windows", "notepad.exe").is_none());
        assert!(foreign_launch_refusal("windows", "ms-settings:display").is_none());
        assert!(foreign_launch_refusal("linux", "/usr/bin/firefox").is_none());
    }

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
    async fn click_by_coordinates_stays_inside_the_observed_window() {
        let fake = Arc::new(FakeDesktop::new(false));
        let (_d, t) = build(fake.clone(), None);
        let ctx = ToolContext::default();
        let err = get(&t, "desktop_click")
            .call(&ctx, json!({"x": 150, "y": 250}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("desktop_get_state"));
        get(&t, "desktop_get_state")
            .call(&ctx, json!({}))
            .await
            .unwrap();
        let err = get(&t, "desktop_click")
            .call(&ctx, json!({"x": 50, "y": 250}))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("outside the observed window"),
            "{err}"
        );
        assert!(fake.actions.lock().is_empty());
        get(&t, "desktop_click")
            .call(&ctx, json!({"x": 150, "y": 250}))
            .await
            .unwrap();
        assert!(matches!(
            &fake.actions.lock()[0],
            Action::Click {
                target: Target::ByCoordinate { .. },
                method: ClickMethod::Global
            }
        ));
    }

    #[tokio::test]
    async fn wait_finds_a_window_by_title_and_times_out_otherwise() {
        let fake = Arc::new(FakeDesktop::new(false));
        let (_d, t) = build(fake, None);
        let ctx = ToolContext::default();
        let ok = get(&t, "desktop_wait")
            .call(&ctx, json!({"title": "notepad", "timeout_ms": 500}))
            .await
            .unwrap();
        assert!(text(&ok).contains("Fake Notepad"), "{}", text(&ok));
        let err = get(&t, "desktop_wait")
            .call(&ctx, json!({"title": "no such window", "timeout_ms": 300}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("did not appear"), "{err}");
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

    // ---- run-scoped control grant: target and guarded dispatch ----

    fn ctx_for(run: &str) -> ToolContext {
        ToolContext {
            run_id: run.into(),
            ..ToolContext::default()
        }
    }

    async fn observed(t: &[Arc<dyn Tool>], ctx: &ToolContext) {
        get(t, "desktop_get_state")
            .call(ctx, json!({}))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn control_target_names_one_program_with_its_full_identity() {
        let fake = Arc::new(FakeDesktop::new(false));
        *fake.input.lock() = Some(FakeDesktop::notepad());
        let (_d, t) = build(fake, None);
        let ctx = ctx_for("r1");
        observed(&t, &ctx).await;
        for (tool, input) in [
            ("desktop_type", json!({"index": 3, "gen": 1, "text": "x"})),
            (
                "desktop_set_value",
                json!({"index": 3, "gen": 1, "value": "x"}),
            ),
            ("desktop_key", json!({"keys": "ctrl+a"})),
        ] {
            let target = get(&t, tool)
                .control_target(&ctx, &input)
                .await
                .unwrap_or_else(|| panic!("{tool}"));
            assert!(target.eligible(), "{tool}: {target:?}");
            assert_eq!(target.program_name(), "notepad.exe");
            assert_eq!(target.identity.pid, 4242);
            assert_eq!(target.identity.start_time, 1_000);
            assert_eq!(target.window_title, "Untitled - Notepad");
        }
    }

    #[tokio::test]
    async fn no_process_identity_means_no_control_target() {
        let fake = Arc::new(FakeDesktop::new(false)); // `input` stays None
        let (_d, t) = build(fake, None);
        let ctx = ctx_for("r1");
        observed(&t, &ctx).await;
        for tool in [
            "desktop_type",
            "desktop_set_value",
            "desktop_key",
            "desktop_drag",
        ] {
            assert!(
                get(&t, tool)
                    .control_target(&ctx, &json!({"index": 3, "gen": 1, "keys": "a"}))
                    .await
                    .is_none(),
                "{tool}"
            );
        }
    }

    #[tokio::test]
    async fn only_the_four_input_tools_have_a_control_target() {
        let fake = Arc::new(FakeDesktop::new(false));
        *fake.input.lock() = Some(FakeDesktop::notepad());
        let (_d, t) = build(fake, None);
        let ctx = ctx_for("r1");
        observed(&t, &ctx).await;
        for tool in [
            "desktop_drag",
            "desktop_click",
            "desktop_scroll",
            "desktop_launch",
            "desktop_get_state",
            "desktop_list_apps",
        ] {
            assert!(
                get(&t, tool)
                    .control_target(&ctx, &json!({"index": 1, "gen": 1}))
                    .await
                    .is_none(),
                "{tool}"
            );
        }
    }

    #[tokio::test]
    async fn win_chords_and_every_spelling_of_them_are_refused_on_the_target() {
        let fake = Arc::new(FakeDesktop::new(false));
        *fake.input.lock() = Some(FakeDesktop::notepad());
        let (_d, t) = build(fake, None);
        let ctx = ctx_for("r1");
        observed(&t, &ctx).await;
        for keys in [
            "win+r",
            "super+r",
            "cmd+r",
            "meta",
            "Windows+e",
            "alt+space",
            "alt+f2",
            "alt+f4",
            "f4",
            "f1",
            "ctrl+n",
            "ctrl+esc",
            "alt+tab",
        ] {
            let target = get(&t, "desktop_key")
                .control_target(&ctx, &json!({"keys": keys}))
                .await
                .unwrap();
            assert_eq!(target.refusal.as_deref(), Some("chord"), "{keys}");
            assert!(!target.eligible(), "{keys}");
        }
        let ok = get(&t, "desktop_key")
            .control_target(&ctx, &json!({"keys": "ctrl+s"}))
            .await
            .unwrap();
        assert!(ok.eligible());
    }

    #[tokio::test]
    async fn terminals_elevated_programs_and_dialogs_are_refused_on_the_target() {
        for (path, class, elevated, want) in [
            (
                "C:\\Windows\\System32\\cmd.exe",
                "ConsoleWindowClass",
                Some(false),
                "terminal",
            ),
            (
                "C:\\Windows\\notepad.exe",
                "Notepad",
                Some(true),
                "elevated",
            ),
            ("C:\\Windows\\notepad.exe", "Notepad", None, "elevated"),
            ("C:\\Windows\\notepad.exe", "#32770", Some(false), "dialog"),
            (
                "C:\\x\\Code.exe",
                "Chrome_WidgetWin_1",
                Some(false),
                "command_app",
            ),
            (
                "C:\\x\\chrome.exe",
                "Chrome_WidgetWin_1",
                Some(false),
                "browser",
            ),
        ] {
            let fake = Arc::new(FakeDesktop::new(false));
            let mut input = FakeDesktop::notepad();
            input.identity.image_path = path.into();
            input.class = class.into();
            input.elevated = elevated;
            *fake.input.lock() = Some(input);
            let (_d, t) = build(fake, None);
            let ctx = ctx_for("r1");
            observed(&t, &ctx).await;
            let target = get(&t, "desktop_type")
                .control_target(&ctx, &json!({"index": 3, "gen": 1, "text": "x"}))
                .await
                .unwrap();
            assert_eq!(target.refusal.as_deref(), Some(want), "{path} {class}");
            assert!(!target.eligible());
        }
    }

    #[tokio::test]
    async fn password_field_in_a_granted_app_still_asks() {
        // By index: the snapshot marks the node secure.
        let mut f = FakeDesktop::new(false);
        f.secure_edit = true;
        let fake = Arc::new(f);
        *fake.input.lock() = Some(FakeDesktop::notepad());
        let (_d, t) = build(fake.clone(), None);
        let ctx = ctx_for("r1");
        observed(&t, &ctx).await;
        for (tool, input) in [
            ("desktop_type", json!({"index": 3, "gen": 1, "text": "x"})),
            (
                "desktop_set_value",
                json!({"index": 3, "gen": 1, "value": "x"}),
            ),
        ] {
            let target = get(&t, tool).control_target(&ctx, &input).await.unwrap();
            assert_eq!(target.refusal.as_deref(), Some("password_field"), "{tool}");
        }
        // A non-secure element of the same window is fine.
        let ok = get(&t, "desktop_type")
            .control_target(&ctx, &json!({"index": 1, "gen": 1, "text": "x"}))
            .await
            .unwrap();
        assert!(ok.eligible());

        // Without an index the keys go to the live focus.
        let fake2 = Arc::new(FakeDesktop::new(false));
        let mut input = FakeDesktop::notepad();
        input.password_focus = true;
        *fake2.input.lock() = Some(input);
        let (_d2, t2) = build(fake2, None);
        observed(&t2, &ctx).await;
        let focus = get(&t2, "desktop_key")
            .control_target(&ctx, &json!({"keys": "enter"}))
            .await
            .unwrap();
        assert_eq!(focus.refusal.as_deref(), Some("password_field"));
        // With an element index the key press first clicks: a click is never
        // covered by a task-wide permission, so there is no target at all.
        assert!(
            get(&t2, "desktop_key")
                .control_target(&ctx, &json!({"keys": "enter", "index": 1, "gen": 1}))
                .await
                .is_none()
        );
    }

    fn guard_for(target: &desktop_api::InputTarget) -> tool_registry::ControlIdentity {
        tool_registry::ControlIdentity {
            image_path: target.identity.image_path.clone(),
            pid: target.identity.pid,
            start_time: target.identity.start_time,
        }
    }

    #[tokio::test]
    async fn a_covered_call_goes_through_the_guarded_path_for_the_click_and_the_text() {
        let fake = Arc::new(FakeDesktop::new(false));
        let notepad = FakeDesktop::notepad();
        *fake.input.lock() = Some(notepad.clone());
        let (_d, t) = build(fake.clone(), None);
        let mut ctx = ctx_for("r1");
        observed(&t, &ctx).await;
        ctx.control_guard = Some(guard_for(&notepad));
        get(&t, "desktop_type")
            .call(&ctx, json!({"index": 3, "gen": 1, "text": "hello"}))
            .await
            .unwrap();
        get(&t, "desktop_key")
            .call(&ctx, json!({"keys": "ctrl+s"}))
            .await
            .unwrap();
        let guarded = fake.guarded.lock();
        assert!(
            matches!(
                &guarded[0],
                Action::Click {
                    method: ClickMethod::Global,
                    ..
                }
            ),
            "the pre-click is guarded too"
        );
        assert!(matches!(&guarded[1], Action::TypeText { text } if text == "hello"));
        assert!(matches!(&guarded[2], Action::KeyPress { .. }));
        assert_eq!(guarded.len(), 3);
    }

    #[tokio::test]
    async fn an_uncovered_call_never_takes_the_guarded_path() {
        let fake = Arc::new(FakeDesktop::new(false));
        *fake.input.lock() = Some(FakeDesktop::notepad());
        let (_d, t) = build(fake.clone(), None);
        let ctx = ctx_for("r1"); // no control_guard
        observed(&t, &ctx).await;
        get(&t, "desktop_type")
            .call(&ctx, json!({"index": 3, "gen": 1, "text": "hello"}))
            .await
            .unwrap();
        assert!(fake.guarded.lock().is_empty());
        assert_eq!(fake.actions.lock().len(), 2);
    }

    #[tokio::test]
    async fn typed_text_after_focus_moved_to_another_process_is_not_sent() {
        let fake = Arc::new(FakeDesktop::new(false));
        let notepad = FakeDesktop::notepad();
        *fake.input.lock() = Some(notepad.clone());
        let (_d, t) = build(fake.clone(), None);
        let mut ctx = ctx_for("r1");
        observed(&t, &ctx).await;
        ctx.control_guard = Some(guard_for(&notepad));

        // A terminal took the focus between the decision and the send.
        let mut other = notepad.clone();
        other.identity = desktop_api::ProcessIdentity {
            image_path: "C:\\Windows\\System32\\cmd.exe".into(),
            pid: 9,
            start_time: 5,
        };
        other.class = "ConsoleWindowClass".into();
        *fake.input.lock() = Some(other);
        for (tool, input) in [
            (
                "desktop_type",
                json!({"index": 3, "gen": 1, "text": "powershell -w hidden"}),
            ),
            ("desktop_key", json!({"keys": "Return"})),
        ] {
            let err = get(&t, tool)
                .call(&ctx, input)
                .await
                .unwrap_err()
                .to_string();
            assert!(err.contains("not sent"), "{tool}: {err}");
        }
        assert!(
            fake.actions.lock().is_empty(),
            "nothing reached the backend"
        );
        assert_eq!(fake.typed.lock().as_str(), "");
    }

    #[tokio::test]
    async fn pid_reuse_with_a_different_start_time_is_not_the_granted_program() {
        let fake = Arc::new(FakeDesktop::new(false));
        let notepad = FakeDesktop::notepad();
        *fake.input.lock() = Some(notepad.clone());
        let (_d, t) = build(fake.clone(), None);
        let mut ctx = ctx_for("r1");
        observed(&t, &ctx).await;
        ctx.control_guard = Some(guard_for(&notepad));
        let mut reused = notepad.clone();
        reused.identity.start_time += 1; // same pid and path, another process
        *fake.input.lock() = Some(reused);
        let err = get(&t, "desktop_key")
            .call(&ctx, json!({"keys": "a"}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("not sent"), "{err}");
        assert!(fake.actions.lock().is_empty());
    }

    #[tokio::test]
    async fn a_guard_on_a_backend_that_cannot_check_fails_closed() {
        // `input` is None: the fake refuses like the default trait method does.
        let fake = Arc::new(FakeDesktop::new(false));
        let (_d, t) = build(fake.clone(), None);
        let mut ctx = ctx_for("r1");
        observed(&t, &ctx).await;
        ctx.control_guard = Some(guard_for(&FakeDesktop::notepad()));
        let err = get(&t, "desktop_key")
            .call(&ctx, json!({"keys": "a"}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("not sent"), "{err}");
        assert!(fake.actions.lock().is_empty());
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
        assert!(
            s.contains("1 image pixel = 1 screen pixel"),
            "the model must see that this capture is not a scaled 1920x1080: {s}"
        );
    }

    #[test]
    fn screenshot_coordinate_line_scales_a_downscaled_capture() {
        let same = screenshot_coordinate_line(0.0, 0.0, 1280.0, 800.0, 1280, 800);
        assert!(same.contains("1 image pixel = 1 screen pixel"), "{same}");
        assert!(same.contains("1280x800"), "{same}");
        let scaled = screenshot_coordinate_line(100.0, 40.0, 1920.0, 1080.0, 1280, 720);
        assert!(scaled.contains("100"), "{scaled}");
        assert!(scaled.contains("1.500"), "{scaled}");
        assert!(scaled.contains("screen pixels"), "{scaled}");
    }

    struct Recorder {
        backend: Arc<dyn DesktopBackend>,
        seen: Mutex<Vec<ControlRequest>>,
        refuse: bool,
    }

    #[async_trait]
    impl DesktopControlGate for Recorder {
        fn desktop_for_run(&self, _: &str) -> Option<Arc<dyn DesktopBackend>> {
            Some(self.backend.clone())
        }
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
            backend: fake.clone(),
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
    async fn desktop_observation_is_refused_while_the_human_drives() {
        let fake = Arc::new(FakeDesktop::new(false));
        let d = tempfile::tempdir().unwrap();
        let t = tools(
            fake.clone(),
            None,
            d.path(),
            settings(),
            Some(Arc::new(HumanDrives)),
        );
        let ctx = ToolContext::default();
        for (name, input) in [
            ("desktop_get_state", json!({})),
            ("desktop_get_state", json!({"screenshot": true})),
            ("desktop_list_apps", json!({})),
        ] {
            let err = get(&t, name).call(&ctx, input).await.unwrap_err();
            assert!(
                err.to_string().contains("observation paused"),
                "{name}: {err}"
            );
        }
        assert!(fake.actions.lock().is_empty());
        // Without a refusing gate the same read works (plain pause keeps reads).
        let (_d2, free, _gate) = gated(Arc::new(FakeDesktop::new(false)), false);
        get(&free, "desktop_get_state")
            .call(&ctx, json!({}))
            .await
            .unwrap();
    }

    /// Latches on demand (a Stop / takeover landing mid-script).
    #[derive(Default)]
    struct StopGate {
        backend: Option<Arc<dyn DesktopBackend>>,
        stopped: std::sync::atomic::AtomicBool,
    }
    #[async_trait]
    impl DesktopControlGate for StopGate {
        fn desktop_for_run(&self, _: &str) -> Option<Arc<dyn DesktopBackend>> {
            self.backend.clone()
        }
        fn latch_refusal(&self) -> Option<String> {
            self.stopped
                .load(std::sync::atomic::Ordering::SeqCst)
                .then(|| "The user stopped the bot; do not retry.".to_owned())
        }
        async fn before_action(&self, _req: &ControlRequest) -> Result<(), String> {
            match self.latch_refusal() {
                Some(m) => Err(m),
                None => Ok(()),
            }
        }
    }

    /// The observation epoch and the "human drives" ban, flipped by a test.
    #[derive(Default)]
    struct ObsGate {
        backend: Option<Arc<dyn DesktopBackend>>,
        epoch: std::sync::atomic::AtomicU64,
        driving: std::sync::atomic::AtomicBool,
    }
    impl ObsGate {
        fn human_takes_over(&self) {
            self.epoch.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.driving
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
    #[async_trait]
    impl DesktopControlGate for ObsGate {
        fn desktop_for_run(&self, _: &str) -> Option<Arc<dyn DesktopBackend>> {
            self.backend.clone()
        }
        fn observation_epoch(&self) -> u64 {
            self.epoch.load(std::sync::atomic::Ordering::SeqCst)
        }
        fn observation_refused(&self) -> Option<String> {
            self.driving
                .load(std::sync::atomic::Ordering::SeqCst)
                .then(|| "the user is controlling (observation paused)".to_owned())
        }
        async fn before_action(&self, _req: &ControlRequest) -> Result<(), String> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn an_observation_that_straddles_a_takeover_never_reaches_the_model() {
        for (name, input, hook_on) in [
            (
                "desktop_get_state",
                json!({"screenshot": true}),
                "screenshot",
            ),
            ("desktop_list_apps", json!({}), "windows"),
        ] {
            let fake = Arc::new(FakeDesktop::new(false));
            let gate = Arc::new(ObsGate {
                backend: Some(fake.clone()),
                ..ObsGate::default()
            });
            let flip = Arc::clone(&gate);
            *fake.hook.lock() = Some(Arc::new(move |what| {
                if what == hook_on {
                    flip.human_takes_over();
                }
            }));
            let d = tempfile::tempdir().unwrap();
            let t = tools(fake, None, d.path(), settings(), Some(gate));
            let err = get(&t, name)
                .call(&ToolContext::default(), input)
                .await
                .unwrap_err();
            assert!(err.to_string().contains("discarded"), "{name}: {err}");
        }
    }

    #[tokio::test]
    async fn the_refresh_after_an_action_never_shows_a_screen_taken_during_a_takeover() {
        let fake = Arc::new(FakeDesktop::new(false));
        let gate = Arc::new(ObsGate {
            backend: Some(fake.clone()),
            ..ObsGate::default()
        });
        let d = tempfile::tempdir().unwrap();
        let t = tools(fake.clone(), None, d.path(), settings(), Some(gate.clone()));
        let ctx = ToolContext::default();
        get(&t, "desktop_get_state")
            .call(&ctx, json!({}))
            .await
            .unwrap();
        let flip = Arc::clone(&gate);
        *fake.hook.lock() = Some(Arc::new(move |what| {
            if what == "act" {
                flip.human_takes_over();
            }
        }));
        let out = get(&t, "desktop_click")
            .call(&ctx, json!({"index": 1, "gen": 1}))
            .await;
        let shown = match &out {
            Ok(o) => text(o).clone(),
            Err(e) => e.to_string(),
        };
        assert!(
            !shown.contains("gen="),
            "a screen captured while the user took over reached the model: {shown}"
        );
    }

    #[tokio::test]
    async fn a_script_cannot_launch_or_act_after_a_stop_with_a_cached_permit() {
        let fake = Arc::new(FakeDesktop::new(false));
        let d = tempfile::tempdir().unwrap();
        let gate = Arc::new(StopGate {
            backend: Some(fake.clone()),
            ..StopGate::default()
        });
        let t = tools(fake.clone(), None, d.path(), settings(), Some(gate.clone()));
        let ctx = ToolContext {
            run_id: "script-run".into(),
            ..ToolContext::default()
        };
        let action: crate::control::ScriptAction = Arc::default();
        // The script's first computer action admits and caches the permit.
        crate::control::scope_action(Arc::clone(&action), async {
            get(&t, "desktop_get_state")
                .call(&ctx, json!({}))
                .await
                .unwrap();
            get(&t, "desktop_click")
                .call(&ctx, json!({"index": 2, "gen": 1}))
                .await
                .unwrap();
        })
        .await;
        assert_eq!(fake.actions.lock().len(), 1);
        gate.stopped
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let actions = fake.actions.lock().len();
        for (name, input) in [
            ("desktop_launch", json!({"executable": "notepad.exe"})),
            ("desktop_click", json!({"index": 2, "gen": 2})),
            ("desktop_key", json!({"keys": "ctrl+s"})),
        ] {
            let err = crate::control::scope_action(Arc::clone(&action), async {
                get(&t, name).call(&ctx, input).await
            })
            .await
            .unwrap_err();
            assert!(err.to_string().contains("stopped the bot"), "{name}: {err}");
        }
        assert_eq!(fake.actions.lock().len(), actions, "nothing was dispatched");
        assert!(fake.launched_at.lock().is_none(), "nothing was launched");
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
        // C knows B's current generation and index, but has never observed them.
        // This must fail before consulting either the control gate or backend.
        let ctx_c = ToolContext {
            run_id: "run-c".into(),
            ..ToolContext::default()
        };
        let err = get(&tools, "desktop_click")
            .call(&ctx_c, json!({"index": 1, "gen": 2}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains(NO_OBSERVATION_MSG), "{err}");
        assert!(!err.to_string().contains("stale"), "{err}");
        assert!(fake.actions.lock().is_empty());
        assert!(gate.seen.lock().is_empty());
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
    }

    async fn assert_unobserved_run_cannot_act(lenient_gen: bool) {
        let mut backend = FakeDesktop::new(false);
        backend.lenient_gen = lenient_gen;
        let fake = Arc::new(backend);
        let (_d, tools, gate) = gated(fake.clone(), false);
        let ctx_a = ToolContext {
            run_id: "observed-a".into(),
            ..ToolContext::default()
        };
        let ctx_b = ToolContext {
            run_id: "observed-b".into(),
            ..ToolContext::default()
        };
        let ctx_c = ToolContext {
            run_id: "unobserved-c".into(),
            ..ToolContext::default()
        };
        let state = get(&tools, "desktop_get_state");
        state.call(&ctx_a, json!({"window_id": 1})).await.unwrap();
        state
            .call(&ctx_b, json!({"window_id": 2, "query": "w2_el3"}))
            .await
            .unwrap();
        for (name, input) in [
            ("desktop_click", json!({"index": 3, "gen": 2})),
            (
                "desktop_type",
                json!({"index": 3, "gen": 2, "text": "blocked"}),
            ),
            (
                "desktop_set_value",
                json!({"index": 3, "gen": 2, "value": "blocked"}),
            ),
            (
                "desktop_key",
                json!({"index": 3, "gen": 2, "keys": "enter"}),
            ),
            ("desktop_key", json!({"keys": "enter"})),
            ("desktop_scroll", json!({"index": 3, "gen": 2, "dy": 1})),
            ("desktop_scroll", json!({"x": 1, "y": 2, "dy": 1})),
            ("desktop_drag", json!({"from": 3, "to": 3, "gen": 2})),
            ("desktop_click", json!({"index": "mark:1", "gen": 2})),
        ] {
            let err = get(&tools, name).call(&ctx_c, input).await.unwrap_err();
            assert!(
                err.to_string().contains(NO_OBSERVATION_MSG),
                "{name}: {err}"
            );
            assert!(!err.to_string().contains("stale"), "{name}: {err}");
            assert!(fake.actions.lock().is_empty(), "{name} reached backend");
            assert!(gate.seen.lock().is_empty(), "{name} reached control gate");
            assert_eq!(fake.scopes.lock().len(), 2, "{name} took a snapshot");
        }
    }

    #[tokio::test]
    async fn unobserved_run_cannot_act_even_with_current_generation() {
        assert_unobserved_run_cannot_act(false).await;
    }

    #[tokio::test]
    async fn unobserved_run_cannot_act_with_lenient_generation_backend() {
        assert_unobserved_run_cannot_act(true).await;
    }

    #[tokio::test]
    async fn default_window_uses_only_own_observation_or_foreground() {
        let fake = Arc::new(FakeDesktop::new(false));
        let (_d, tools) = build(fake.clone(), None);
        let ctx_a = ToolContext {
            run_id: "a".into(),
            ..ToolContext::default()
        };
        let ctx_b = ToolContext {
            run_id: "b".into(),
            ..ToolContext::default()
        };
        let ctx_c = ToolContext {
            run_id: "c".into(),
            ..ToolContext::default()
        };
        let state = get(&tools, "desktop_get_state");
        state.call(&ctx_a, json!({"window_id": 1})).await.unwrap();
        state.call(&ctx_b, json!({"window_id": 2})).await.unwrap();
        let own = state.call(&ctx_a, json!({})).await.unwrap();
        assert!(text(&own).contains("window_id: 1"));
        let fresh = state.call(&ctx_c, json!({})).await.unwrap();
        assert!(text(&fresh).contains("window_id: 77"));
        assert_eq!(*fake.scopes.lock(), [Some(1), Some(2), Some(1), None]);
    }

    #[tokio::test]
    async fn launch_and_list_do_not_inherit_another_runs_observation() {
        let fake = Arc::new(FakeDesktop::new(false));
        let (_d, tools) = build(fake.clone(), None);
        let observed = ToolContext {
            run_id: "observed".into(),
            ..ToolContext::default()
        };
        let fresh = ToolContext {
            run_id: "fresh".into(),
            ..ToolContext::default()
        };
        get(&tools, "desktop_get_state")
            .call(&observed, json!({"window_id": 2}))
            .await
            .unwrap();
        get(&tools, "desktop_list_apps")
            .call(&fresh, json!({}))
            .await
            .unwrap();
        get(&tools, "desktop_launch")
            .call(&fresh, json!({"executable": "fake"}))
            .await
            .unwrap();
        let err = get(&tools, "desktop_click")
            .call(&fresh, json!({"index": 1, "gen": 1}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains(NO_OBSERVATION_MSG), "{err}");
        assert!(fake.actions.lock().is_empty());
        let out = get(&tools, "desktop_get_state")
            .call(&fresh, json!({}))
            .await
            .unwrap();
        assert!(text(&out).contains("window_id: 77"));
        assert_eq!(fake.scopes.lock().last(), Some(&None));
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
        let (_d, t) = build(fake.clone(), None);
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
            .call(
                &oldest,
                json!({"index": 1, "gen": u64::try_from(MAX_RUNS + 4).unwrap()}),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains(NO_OBSERVATION_MSG), "{err}");
        assert!(!err.to_string().contains("stale"), "{err}");
        assert!(fake.actions.lock().is_empty());
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

    #[tokio::test]
    async fn dynamic_tools_route_desktop_calls_per_run_id() {
        struct RunRouter {
            routes: std::collections::HashMap<String, Arc<dyn DesktopBackend>>,
        }

        #[async_trait]
        impl DesktopControlGate for RunRouter {
            async fn before_action(&self, _req: &ControlRequest) -> Result<(), String> {
                Ok(())
            }

            fn desktop_for_run(&self, run_id: &str) -> Option<Arc<dyn DesktopBackend>> {
                self.routes.get(run_id).cloned()
            }
        }

        let a = Arc::new(FakeDesktop::new(false));
        let b = Arc::new(FakeDesktop::new(false));
        let mut routes: std::collections::HashMap<String, Arc<dyn DesktopBackend>> =
            std::collections::HashMap::new();
        routes.insert("run-a".into(), a.clone());
        routes.insert("run-b".into(), b.clone());
        let gate = Arc::new(RunRouter { routes });

        let d = tempfile::tempdir().unwrap();
        let t = tools_dynamic(
            Some(Arc::new(FakeDesktop::new(false))),
            None,
            d.path(),
            settings(),
            Some(gate),
        );

        let ctx_a = ToolContext {
            run_id: "run-a".into(),
            ..ToolContext::default()
        };
        let ctx_b = ToolContext {
            run_id: "run-b".into(),
            ..ToolContext::default()
        };
        let ctx_none = ToolContext {
            run_id: "run-none".into(),
            ..ToolContext::default()
        };

        get(&t, "desktop_get_state")
            .call(&ctx_a, json!({}))
            .await
            .unwrap();
        get(&t, "desktop_click")
            .call(&ctx_a, json!({"index": 1, "gen": 1}))
            .await
            .unwrap();
        assert_eq!(a.actions.lock().len(), 1);
        assert!(b.actions.lock().is_empty());

        get(&t, "desktop_get_state")
            .call(&ctx_b, json!({}))
            .await
            .unwrap();
        get(&t, "desktop_click")
            .call(&ctx_b, json!({"index": 2, "gen": 1}))
            .await
            .unwrap();
        assert_eq!(a.actions.lock().len(), 1);
        assert_eq!(b.actions.lock().len(), 1);

        let err = get(&t, "desktop_get_state")
            .call(&ctx_none, json!({}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("unavailable"), "{err}");
    }

    #[test]
    fn desktop_click_schema_states_app_post_is_not_implemented_and_parser_keeps_it() {
        let fake = Arc::new(FakeDesktop::new(false));
        let (_d, t) = build(fake, None);
        let click = get(&t, "desktop_click");
        let spec = click.spec();
        let method = &spec.input_schema["properties"]["method"];

        let variants: Vec<&str> = method["enum"]
            .as_array()
            .expect("method enum")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(
            variants.contains(&"app_post"),
            "app_post must remain in method enum for workflow replay: {variants:?}"
        );

        let desc = method["description"].as_str().expect("method description");
        assert!(desc.contains("app_post"), "{desc}");
        assert!(desc.contains("not implemented"), "{desc}");
        assert!(desc.contains("Windows"), "{desc}");
        assert!(desc.contains("Linux"), "{desc}");
        assert!(desc.contains("fail"), "{desc}");

        assert_eq!(
            parse_method(Some("app_post"), ClickMethod::Accessibility).unwrap(),
            ClickMethod::AppPost
        );
        assert_eq!(
            parse_method(Some("app_post"), ClickMethod::Global).unwrap(),
            ClickMethod::AppPost
        );
    }
}
