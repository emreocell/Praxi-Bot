//! Turn a completed run's persisted tool steps into a [`Workflow`].
//!
//! * Only successful, mutating desktop / browser / tool calls are kept; reads
//!   (`desktop_get_state`, `browser_snapshot`, `fs_read`, ...) and meta tools
//!   are dropped, failed calls are dropped.
//! * Element indexes / refs the model used are turned into semantic locators by
//!   reading the UI tree text the model saw just before the call.
//! * Literal values that appear verbatim in the user's message become
//!   parameters (default = the recorded literal).
//! * The recorder returns an unsaved *proposal* for review. The caller can
//!   explicitly persist it afterward with [`Recording::save_new`].
//!
//! TODO(model-assisted parametrisation): only whole-value substring matches are
//! lifted; partial matches (`...?q=cats`), reworded values and dates need a
//! small model pass over the proposal.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Datelike, Utc};
use protocol::RunId;
use serde::Deserialize;
use serde_json::{Map, Value};
use storage::{RunStep, Storage};
use tool_registry::{Effect, PermissionCategory, ToolRegistry};

use crate::error::RecordError;
use crate::format::{
    BrowserAction, BrowserActionKind, BrowserLocator, DesktopAction, DesktopActionKind,
    DesktopLocator, Launch, Navigate, Param, ParamType, PositionHint, Shell, Step, ToolStep,
    Workflow,
};
use crate::locator::{fingerprint_hex, parse_browser_line, parse_browser_snapshot, read_quoted};
use crate::store::{SaveError, SaveMode, WorkflowStore};
use crate::template::escape_literal;

/// Step names the agent loop persists (agent-core `STEP_USER` / `STEP_TOOLS`).
const STEP_USER: &str = "user_message";
const STEP_TOOLS: &str = "tools";

/// Minimum length of a literal that may become a parameter.
const MIN_LIFT_CHARS: usize = 3;

/// Tools that never belong in a workflow, with or without a registry.
const SKIPPED: &[&str] = &["todo_write", "ask_user", "find_tool", "get_tool_details"];

/// Read-only built-ins (used when no registry is available to ask).
const KNOWN_READS: &[&str] = &[
    "desktop_list_apps",
    "desktop_get_state",
    "desktop_scroll",
    "browser_snapshot",
    "browser_scroll",
    "browser_wait",
    "browser_tabs",
    "browser_screenshot",
    "fs_read",
    "fs_list",
    "fs_search",
    "checkpoint_list",
];

/// Recording options.
#[derive(Debug, Clone, Default)]
pub struct RecordOptions {
    /// Workflow name (default derived from the run id).
    pub name: Option<String>,
    /// Description (default: the user's first message).
    pub description: Option<String>,
}

/// A mutating call that could not be turned into a step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DroppedStep {
    /// Tool name.
    pub tool: String,
    /// Why.
    pub reason: String,
}

/// The reviewable result of recording.
#[derive(Debug, Clone)]
pub struct Recording {
    /// The proposed workflow (not saved anywhere).
    pub workflow: Workflow,
    /// Mutating calls that were left out; the workflow is incomplete if non-empty.
    pub dropped: Vec<DroppedStep>,
    /// Review hints.
    pub warnings: Vec<String>,
}

impl Recording {
    /// Save a reviewed proposal without replacing any existing workflow.
    /// Returns its initial store revision.
    ///
    /// # Errors
    /// Name collision, invalid workflow, or a store error.
    pub fn save_new(&self, store: &WorkflowStore) -> Result<u64, SaveError> {
        store.save_with_mode(&self.workflow, SaveMode::CreateNew)
    }
}

fn default_recording_name(run_id: RunId) -> String {
    let id = run_id.into_inner();
    let timestamp = id
        .get_timestamp()
        .and_then(|timestamp| {
            let (seconds, nanos) = timestamp.to_unix();
            DateTime::<Utc>::from_timestamp(i64::try_from(seconds).ok()?, nanos)
        })
        .filter(|time| (0..=9999).contains(&time.year()))
        .map_or_else(
            || "unknown".into(),
            |time| time.format("%Y%m%d-%H%M").to_string(),
        );
    // Keep the complete identifier. A short random suffix can still collide,
    // even for two distinct UUIDv7 values created in the same millisecond.
    // The normal UTC form is only 50 characters (valid_name permits 64).
    format!("run-{timestamp}-{}", id.simple())
}

/// Records workflows from persisted runs.
#[derive(Default)]
pub struct Recorder {
    registry: Option<ToolRegistry>,
}

impl Recorder {
    /// A recorder that classifies tools with the built-in name table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Classify tools through `registry` (exact effects for every tool).
    #[must_use]
    pub fn with_registry(mut self, registry: ToolRegistry) -> Self {
        self.registry = Some(registry);
        self
    }

    /// Record a succeeded run.
    ///
    /// # Errors
    /// Missing or unfinished run, storage failure, nothing replayable.
    pub fn from_run(
        &self,
        storage: &Storage,
        run_id: RunId,
        opts: &RecordOptions,
    ) -> Result<Recording, RecordError> {
        let run = storage
            .get_run(run_id)
            .map_err(|e| RecordError::Storage(e.to_string()))?
            .ok_or_else(|| RecordError::RunNotFound(run_id.to_string()))?;
        if run.state != "succeeded" {
            return Err(RecordError::NotSucceeded {
                run: run_id.to_string(),
                state: run.state,
            });
        }
        let steps = storage
            .list_steps(run_id)
            .map_err(|e| RecordError::Storage(e.to_string()))?;
        let mut opts = opts.clone();
        if opts.name.is_none() {
            opts.name = Some(default_recording_name(run_id));
        }
        self.from_steps(&steps, &opts)
    }

    /// Record from already loaded steps (pure; used by tests).
    ///
    /// # Errors
    /// Nothing replayable, or the proposal fails validation.
    pub fn from_steps(
        &self,
        steps: &[RunStep],
        opts: &RecordOptions,
    ) -> Result<Recording, RecordError> {
        let mut ordered: Vec<&RunStep> = steps.iter().collect();
        ordered.sort_by_key(|s| s.idx);

        let message: String = ordered
            .iter()
            .filter(|s| s.name == STEP_USER)
            .filter_map(|s| s.input["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");

        let mut cx = Cx {
            lifter: Lifter::new(&message),
            ..Cx::default()
        };
        for step in ordered.iter().filter(|s| s.name == STEP_TOOLS) {
            self.record_step(step, &mut cx);
        }
        if cx.steps.is_empty() {
            return Err(RecordError::Empty);
        }

        let first = ordered.first().ok_or(RecordError::Empty)?;
        let mut wf = Workflow::new(
            opts.name
                .clone()
                .unwrap_or_else(|| default_recording_name(first.run_id)),
        );
        wf.description = opts.description.clone().unwrap_or_else(|| {
            message
                .lines()
                .next()
                .unwrap_or_default()
                .chars()
                .take(200)
                .collect()
        });
        wf.steps = cx.steps;
        wf.params = cx.lifter.params;
        if !cx.dropped.is_empty() {
            cx.warnings.push(format!(
                "{} mutating call(s) could not be recorded; the workflow is incomplete",
                cx.dropped.len()
            ));
        }
        if wf.params.is_empty() {
            cx.warnings
                .push("no parameters were detected; review literal values".into());
        }
        wf.validate()?;
        Ok(Recording {
            workflow: wf,
            dropped: cx.dropped,
            warnings: cx.warnings,
        })
    }

    fn classify(&self, name: &str) -> Class {
        if SKIPPED.contains(&name) {
            return Class::Skip;
        }
        if let Some(reg) = &self.registry
            && let Some(tool) = reg.get(name).or_else(|| reg.get(&name.replace("__", ".")))
        {
            let spec = tool.spec();
            return if spec.category == PermissionCategory::Meta || spec.effect == Effect::Read {
                Class::Read
            } else {
                Class::Mutating
            };
        }
        if KNOWN_READS.contains(&name) {
            Class::Read
        } else {
            Class::Mutating
        }
    }

    fn record_step(&self, step: &RunStep, cx: &mut Cx) {
        #[derive(Deserialize)]
        struct Call {
            id: String,
            name: String,
            arguments: String,
        }
        #[derive(Deserialize)]
        struct Res {
            id: String,
            is_error: bool,
            text: String,
        }
        let calls: Vec<Call> =
            serde_json::from_value(step.input["calls"].clone()).unwrap_or_default();
        let results: Vec<Res> = step
            .output
            .as_ref()
            .and_then(|o| serde_json::from_value(o["results"].clone()).ok())
            .unwrap_or_default();

        for call in calls {
            let res = results.iter().find(|r| r.id == call.id);
            let args: Value = serde_json::from_str(&call.arguments).unwrap_or(Value::Null);
            // Peel `use_tool {name, input}`.
            let (name, input) = if call.name == "use_tool" {
                (
                    args["name"].as_str().unwrap_or("use_tool").to_owned(),
                    args.get("input")
                        .cloned()
                        .unwrap_or_else(|| Value::Object(Map::new())),
                )
            } else {
                (call.name.clone(), args)
            };

            // The UI text before this call decides what an index meant, so it is
            // consulted first; this call's own output only feeds later calls.
            let class = self.classify(&name);
            if class == Class::Mutating {
                match res {
                    None => cx.dropped.push(DroppedStep {
                        tool: name.clone(),
                        reason: "no result was recorded (interrupted)".into(),
                    }),
                    Some(r) if r.is_error => {} // a failed attempt is simply not part of the flow
                    // A batch becomes the single actions it ran, so the
                    // workflow format (and its replay) stays unchanged.
                    Some(r) if name == "browser_act" => {
                        for (tool, single) in expand_browser_act(&input, &r.text) {
                            match convert(tool, &single, cx) {
                                Conv::Step(s) => cx.steps.push(s),
                                Conv::Dropped(reason) => cx.dropped.push(DroppedStep {
                                    tool: format!("browser_act/{tool}"),
                                    reason,
                                }),
                            }
                        }
                    }
                    Some(_) => match convert(&name, &input, cx) {
                        Conv::Step(s) => cx.steps.push(s),
                        Conv::Dropped(reason) => cx.dropped.push(DroppedStep {
                            tool: name.clone(),
                            reason,
                        }),
                    },
                }
            }
            if let Some(r) = res
                && !r.is_error
            {
                cx.observe(&name, &r.text);
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Skip,
    Read,
    Mutating,
}

#[allow(clippy::large_enum_variant)]
enum Conv {
    Step(Step),
    Dropped(String),
}

// ── Context: what the model last saw ────────────────────────────────────────

#[derive(Debug, Clone)]
struct DesktopNodeRec {
    index: u32,
    role: String,
    name: String,
    center: Option<(f64, f64)>,
}

#[derive(Default)]
struct Cx {
    steps: Vec<Step>,
    dropped: Vec<DroppedStep>,
    warnings: Vec<String>,
    lifter: Lifter,
    /// Latest desktop tree: window title and nodes.
    window_title: Option<String>,
    desktop: Vec<DesktopNodeRec>,
    /// Browser refs seen so far, and every element line of the latest snapshot
    /// (for counting identical role+name pairs).
    refs: HashMap<String, (String, String)>,
    page_nodes: Vec<(String, String)>,
    page_url: Option<String>,
}

impl Cx {
    /// Feed a tool output into the picture of the screen.
    fn observe(&mut self, tool: &str, text: &str) {
        if tool.starts_with("desktop_") {
            self.observe_desktop(text);
        } else if tool.starts_with("browser_") {
            self.observe_browser(text);
        }
    }

    fn observe_desktop(&mut self, text: &str) {
        let mut title = None;
        let mut nodes = Vec::new();
        for line in text.lines() {
            if let Some(h) = line.strip_prefix("# window \"") {
                title = Some(read_quoted(h).0);
            } else if let Some(n) = parse_desktop_line(line) {
                nodes.push(n);
            }
        }
        // Only a full window tree replaces the view (launch / list output has none).
        if title.is_some() {
            self.window_title = title;
            self.desktop = nodes;
        }
    }

    fn observe_browser(&mut self, text: &str) {
        let page = parse_browser_snapshot(text);
        if let Some(u) = page.url {
            self.page_url = Some(u);
            // A full snapshot replaces the element list.
            self.page_nodes = page
                .nodes
                .iter()
                .map(|n| (n.role.clone(), n.name.clone()))
                .collect();
        }
        for n in page.nodes {
            self.refs.insert(n.ref_id, (n.role, n.name));
        }
        // Diff outputs mention new refs on `[eN]` lines without a header.
        for line in text.lines() {
            if let Some(n) = parse_browser_line(line) {
                self.refs.insert(n.ref_id, (n.role, n.name));
            }
        }
    }

    fn desktop_locator(&self, index: u32) -> Result<DesktopLocator, String> {
        let node = self
            .desktop
            .iter()
            .find(|n| n.index == index)
            .ok_or_else(|| {
                format!("element index {index} is not in the last UI tree the model saw")
            })?;
        let same: Vec<&DesktopNodeRec> = self
            .desktop
            .iter()
            .filter(|n| n.role == node.role && n.name == node.name)
            .collect();
        let nth = (same.len() > 1)
            .then(|| same.iter().position(|n| n.index == index))
            .flatten();
        let same_role: Vec<&DesktopNodeRec> = self
            .desktop
            .iter()
            .filter(|n| n.role == node.role)
            .collect();
        let position = node.center.map(|(x, y)| PositionHint {
            x,
            y,
            role_ordinal: same_role
                .iter()
                .position(|n| n.index == index)
                .unwrap_or_default(),
            role_count: same_role.len(),
        });
        Ok(DesktopLocator {
            role: node.role.clone(),
            name: node.name.clone(),
            fingerprint: fingerprint_hex(&node.role, &node.name),
            automation_id: None,
            class_name: None,
            nth,
            position,
        })
    }

    fn browser_locator(&self, r: &str) -> Result<BrowserLocator, String> {
        let (role, name) = self
            .refs
            .get(r)
            .ok_or_else(|| format!("element ref {r} was never listed in a snapshot"))?;
        let same: Vec<usize> = self
            .page_nodes
            .iter()
            .enumerate()
            .filter(|(_, (ro, na))| ro == role && na == name)
            .map(|(i, _)| i)
            .collect();
        // The ref's own position among identical elements needs the ref list in
        // page order; page_nodes is in page order and so is the ref numbering.
        let nth = if same.len() > 1 {
            self.ordinal_of_ref(r, role, name)
        } else {
            None
        };
        Ok(BrowserLocator {
            role: role.clone(),
            name: name.clone(),
            fingerprint: fingerprint_hex(role, name),
            aria_label: None,
            css: None,
            nth,
        })
    }

    fn ordinal_of_ref(&self, r: &str, role: &str, name: &str) -> Option<usize> {
        let mut same: Vec<(u64, &str)> = self
            .refs
            .iter()
            .filter(|(_, (ro, na))| ro == role && na == name)
            .filter_map(|(k, _)| k[1..].parse::<u64>().ok().map(|n| (n, k.as_str())))
            .collect();
        same.sort_unstable();
        same.iter().position(|(_, k)| *k == r)
    }

    /// Window pattern for the current window: `*<last segment>` for titles like
    /// `Untitled - Notepad` (the document part changes between runs).
    fn window_pattern(&self) -> Option<String> {
        let title = self.window_title.as_deref()?;
        Some(window_pattern_for(title))
    }

    fn url_pattern(&self) -> Option<String> {
        let url = self.page_url.as_deref()?;
        let (scheme, rest) = url.split_once("://")?;
        let host = rest.split(['/', '?', '#']).next()?;
        Some(format!("{scheme}://{host}/*"))
    }
}

fn window_pattern_for(title: &str) -> String {
    let esc = |s: &str| s.replace('*', "?");
    for sep in [", ", " - ", " \u{2014} ", " \u{2013} "] {
        if let Some(i) = title.rfind(sep) {
            return format!("*{}", esc(&title[i + sep.len()..]));
        }
    }
    esc(title)
}

fn parse_desktop_line(line: &str) -> Option<DesktopNodeRec> {
    let t = line.strip_prefix('[')?;
    let close = t.find(']')?;
    let index: u32 = t[..close].parse().ok()?;
    let rest = t[close + 1..].trim_start();
    let (role, after) = rest.split_once(' ').unwrap_or((rest, ""));
    if role.is_empty() {
        return None;
    }
    let (name, tail) = after.strip_prefix('"').map_or((String::new(), after), |q| {
        let (n, t) = read_quoted(q);
        (n, t.trim_start())
    });
    let center = tail.strip_prefix('(').and_then(|c| {
        let (xy, _) = c.split_once(')')?;
        let (x, y) = xy.split_once(',')?;
        Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
    });
    Some(DesktopNodeRec {
        index,
        role: role.to_owned(),
        name,
        center,
    })
}

// ── Parameter lifting ───────────────────────────────────────────────────────

#[derive(Default)]
struct Lifter {
    message: String,
    params: Vec<Param>,
    by_value: BTreeMap<String, String>,
}

impl Lifter {
    fn new(message: &str) -> Self {
        Self {
            message: message.to_owned(),
            ..Self::default()
        }
    }

    /// A template for `value`: `{param}` when it appears in the user message,
    /// otherwise the escaped literal.
    fn lift(&mut self, hint: &str, value: &str) -> String {
        if value.chars().count() < MIN_LIFT_CHARS || !self.message.contains(value) {
            return escape_literal(value);
        }
        if let Some(n) = self.by_value.get(value) {
            return format!("{{{n}}}");
        }
        let base: String = hint
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '_'
                }
            })
            .collect();
        let base = if base.is_empty() || base.starts_with(|c: char| c.is_ascii_digit()) {
            format!("p_{base}")
        } else {
            base
        };
        let mut name = base.clone();
        let mut n = 2;
        while self.params.iter().any(|p| p.name == name) {
            name = format!("{base}_{n}");
            n += 1;
        }
        self.params.push(Param {
            name: name.clone(),
            ty: ParamType::String,
            default: Some(value.to_owned()),
            description: format!("Recorded from the request ({hint})"),
        });
        self.by_value.insert(value.to_owned(), name.clone());
        format!("{{{name}}}")
    }

    fn lift_value(&mut self, hint: &str, v: &Value) -> Value {
        match v {
            Value::String(s) => Value::String(self.lift(hint, s)),
            Value::Array(a) => Value::Array(a.iter().map(|x| self.lift_value(hint, x)).collect()),
            Value::Object(o) => Value::Object(
                o.iter()
                    .filter(|(_, x)| !x.is_null())
                    .map(|(k, x)| (k.clone(), self.lift_value(k, x)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }
}

// ── Call conversion ─────────────────────────────────────────────────────────

fn str_of<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn desktop_index(v: &Value) -> Result<u32, String> {
    match v {
        Value::Number(n) => n
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| "bad element index".to_owned()),
        Value::String(s) if s.trim().starts_with("mark:") => Err(
            "OCR mark targets have no semantic locator (the element had no accessible name)".into(),
        ),
        Value::String(s) => s.trim().parse().map_err(|_| "bad element index".to_owned()),
        _ => Err("bad element index".into()),
    }
}

fn keys_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Array(a) => Some(
            a.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("+"),
        ),
        _ => None,
    }
}

/// The single-action calls a `browser_act` batch stands for: only the steps
/// that ran (`done: k/n` in its result), `scroll` and `wait` left out like
/// the read-only single tools.
fn expand_browser_act(input: &Value, result: &str) -> Vec<(&'static str, Value)> {
    let done = result
        .lines()
        .next()
        .and_then(|l| l.strip_prefix("done: "))
        .and_then(|l| l.split('/').next())
        .and_then(|k| k.trim().parse::<usize>().ok())
        .unwrap_or(0);
    input["steps"]
        .as_array()
        .into_iter()
        .flatten()
        .take(done)
        .filter_map(|st| {
            let tool = match st["action"].as_str()? {
                "click" => "browser_click",
                "fill" => "browser_fill",
                "select" => "browser_select",
                "press" => "browser_press",
                _ => return None,
            };
            let mut single = Map::new();
            for key in ["ref", "text", "value", "key"] {
                if let Some(v) = st.get(key).filter(|v| !v.is_null()) {
                    single.insert(key.to_owned(), v.clone());
                }
            }
            Some((tool, Value::Object(single)))
        })
        .collect()
}

fn convert(name: &str, input: &Value, cx: &mut Cx) -> Conv {
    macro_rules! tryc {
        ($e:expr) => {
            match $e {
                Ok(v) => v,
                Err(m) => return Conv::Dropped(m),
            }
        };
    }
    match name {
        "desktop_launch" => {
            let Some(exe) = str_of(input, "executable") else {
                return Conv::Dropped("missing executable".into());
            };
            let args = input["args"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(escape_literal)
                        .collect()
                })
                .unwrap_or_default();
            Conv::Step(Step::Launch(Launch {
                executable: escape_literal(exe),
                args,
            }))
        }
        "desktop_click" | "desktop_type" | "desktop_set_value" | "desktop_key" => {
            let target = match input.get("index") {
                Some(i) => Some(tryc!(desktop_index(i).and_then(|i| cx.desktop_locator(i)))),
                None => None,
            };
            // A secret step keeps only the reference to the owner's secret.
            let secret = str_of(input, "secret").map(str::to_owned);
            let (action, value, method) = match name {
                "desktop_click" => (
                    DesktopActionKind::Click,
                    None,
                    str_of(input, "method").map(str::to_owned),
                ),
                "desktop_type" => (
                    DesktopActionKind::Type,
                    if secret.is_some() {
                        None
                    } else {
                        str_of(input, "text").map(|t| cx.lifter.lift("text", t))
                    },
                    None,
                ),
                "desktop_set_value" => (
                    DesktopActionKind::SetValue,
                    if secret.is_some() {
                        None
                    } else {
                        str_of(input, "value").map(|t| cx.lifter.lift("value", t))
                    },
                    None,
                ),
                _ => (
                    DesktopActionKind::Key,
                    input
                        .get("keys")
                        .and_then(keys_string)
                        .map(|k| escape_literal(&k)),
                    None,
                ),
            };
            Conv::Step(Step::DesktopAction(DesktopAction {
                app: None,
                window_title_pattern: cx.window_pattern(),
                target,
                action,
                value,
                method,
                secret,
            }))
        }
        "desktop_drag" => Conv::Dropped("drag is not supported in workflows yet".into()),
        "browser_open" => {
            let Some(url) = str_of(input, "url") else {
                return Conv::Dropped("missing url".into());
            };
            Conv::Step(Step::Navigate(Navigate {
                url: cx.lifter.lift("url", url),
                profile: str_of(input, "profile").map(str::to_owned),
            }))
        }
        "browser_click" | "browser_fill" | "browser_select" | "browser_press" | "browser_back" => {
            let target = match str_of(input, "ref") {
                Some(r) => Some(tryc!(cx.browser_locator(r))),
                None => None,
            };
            let secret = str_of(input, "secret").map(str::to_owned);
            let (action, value) = match name {
                "browser_click" => (BrowserActionKind::Click, None),
                "browser_fill" => (
                    BrowserActionKind::Fill,
                    if secret.is_some() {
                        None
                    } else {
                        str_of(input, "text").map(|t| cx.lifter.lift("text", t))
                    },
                ),
                "browser_select" => (
                    BrowserActionKind::Select,
                    str_of(input, "value").map(|t| cx.lifter.lift("value", t)),
                ),
                "browser_press" => (
                    BrowserActionKind::Press,
                    str_of(input, "key").map(escape_literal),
                ),
                _ => (BrowserActionKind::Back, None),
            };
            Conv::Step(Step::BrowserAction(BrowserAction {
                url_pattern: cx.url_pattern(),
                target,
                action,
                value,
                // Only a fill step may carry one; validation rejects the rest.
                secret: if action == BrowserActionKind::Fill {
                    secret
                } else {
                    None
                },
            }))
        }
        "shell_run" => {
            let Some(cmd) = str_of(input, "command") else {
                return Conv::Dropped("missing command".into());
            };
            Conv::Step(Step::Shell(Shell {
                // Never lift the whole command into a `{command}` parameter: shell
                // steps quote parameter values as one word, so replay would run
                // `'git status'` as a single program name.
                command: escape_literal(cmd),
                shell: str_of(input, "shell").map(str::to_owned),
                cwd: str_of(input, "cwd").map(escape_literal),
            }))
        }
        other => {
            if !input.is_object() {
                return Conv::Dropped("tool input is not an object".into());
            }
            let lifted = cx.lifter.lift_value("value", input);
            Conv::Step(Step::Tool(ToolStep {
                tool: other.to_owned(),
                input: lifted,
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use uuid::Uuid;

    const DESKTOP_TREE: &str = "dispatched: true\n# window \"Untitled - Notepad\" pid=4 gen=2 quality=Good nodes=3\n[0] button \"Close\" (790,10)\n[1] edit \"Text editor\" (400,300) {focused}\n[2] button \"Close\" (700,10)\n";

    fn tool_step(idx: u32, calls: Value, results: Value) -> RunStep {
        RunStep {
            id: Uuid::now_v7(),
            run_id: RunId::new(),
            user_id: protocol::UserId::new(),
            idx,
            name: STEP_TOOLS.into(),
            input: json!({ "calls": calls }),
            idempotent: false,
            retry: None,
            status: "succeeded".into(),
            attempt: 1,
            output: Some(json!({ "results": results })),
            error: None,
            started_at_ms: 0,
            finished_at_ms: Some(0),
        }
    }

    fn user_step(text: &str) -> RunStep {
        let mut s = tool_step(0, json!([]), json!([]));
        s.name = STEP_USER.into();
        s.input = json!({ "text": text });
        s.output = None;
        s
    }

    fn call(id: &str, name: &str, args: &Value) -> Value {
        json!({"id": id, "name": name, "arguments": args.to_string(), "repeat": 1})
    }

    fn res(id: &str, name: &str, text: &str, is_error: bool) -> Value {
        json!({"id": id, "call_uuid": Uuid::now_v7(), "name": name, "is_error": is_error, "text": text, "spill_path": null})
    }

    #[test]
    fn keeps_mutations_drops_reads_and_failures_and_lifts_params() {
        let steps = vec![
            user_step("open notepad and type Hello World"),
            tool_step(
                1,
                json!([call(
                    "a",
                    "desktop_launch",
                    &json!({"executable": "notepad.exe"})
                )]),
                json!([res("a", "desktop_launch", "launched pid=4", false)]),
            ),
            tool_step(
                2,
                json!([call("b", "desktop_get_state", &json!({}))]),
                json!([res("b", "desktop_get_state", DESKTOP_TREE, false)]),
            ),
            tool_step(
                3,
                json!([call("c", "desktop_click", &json!({"index": 99}))]),
                json!([res("c", "desktop_click", "Error: stale", true)]),
            ),
            tool_step(
                4,
                json!([call(
                    "d",
                    "desktop_type",
                    &json!({"index": 1, "text": "Hello World"})
                )]),
                json!([res("d", "desktop_type", DESKTOP_TREE, false)]),
            ),
            tool_step(
                5,
                json!([call("e", "todo_write", &json!({"items": []}))]),
                json!([res("e", "todo_write", "ok", false)]),
            ),
        ];
        let rec = Recorder::new()
            .from_steps(
                &steps,
                &RecordOptions {
                    name: Some("note".into()),
                    description: None,
                },
            )
            .unwrap();
        let wf = rec.workflow;
        assert_eq!(wf.steps.len(), 2, "{:#?}", wf.steps);
        assert!(rec.dropped.is_empty());
        let Step::DesktopAction(d) = &wf.steps[1] else {
            panic!()
        };
        assert_eq!(d.value.as_deref(), Some("{text}"));
        assert_eq!(d.window_title_pattern.as_deref(), Some("*Notepad"));
        let t = d.target.as_ref().unwrap();
        assert_eq!((t.role.as_str(), t.name.as_str()), ("edit", "Text editor"));
        assert_eq!(wf.params[0].default.as_deref(), Some("Hello World"));
        // The launch executable is not a parameter even though "notepad" is in the message.
        let Step::Launch(l) = &wf.steps[0] else {
            panic!()
        };
        assert_eq!(l.executable, "notepad.exe");
    }

    #[test]
    fn shell_run_command_is_never_lifted_into_a_parameter() {
        let steps = vec![
            user_step("please run git status for me"),
            tool_step(
                1,
                json!([call("s", "shell_run", &json!({"command": "git status"}))]),
                json!([res("s", "shell_run", "clean", false)]),
            ),
        ];
        let rec = Recorder::new()
            .from_steps(&steps, &RecordOptions::default())
            .unwrap();
        let Step::Shell(sh) = &rec.workflow.steps[0] else {
            panic!("{:#?}", rec.workflow.steps)
        };
        assert_eq!(sh.command, "git status");
        assert!(rec.workflow.params.is_empty());
    }

    #[test]
    fn duplicate_names_get_nth_and_marks_are_reported_dropped() {
        let steps = vec![
            user_step("close it"),
            tool_step(
                1,
                json!([call("b", "desktop_get_state", &json!({}))]),
                json!([res("b", "desktop_get_state", DESKTOP_TREE, false)]),
            ),
            tool_step(
                2,
                json!([call("c", "desktop_click", &json!({"index": 2}))]),
                json!([res("c", "desktop_click", "dispatched: true", false)]),
            ),
            tool_step(
                3,
                json!([call("m", "desktop_click", &json!({"index": "mark:3"}))]),
                json!([res("m", "desktop_click", "dispatched: true", false)]),
            ),
        ];
        let rec = Recorder::new()
            .from_steps(&steps, &RecordOptions::default())
            .unwrap();
        let Step::DesktopAction(d) = &rec.workflow.steps[0] else {
            panic!()
        };
        assert_eq!(d.target.as_ref().unwrap().nth, Some(1));
        assert_eq!(rec.dropped.len(), 1);
        assert!(rec.warnings.iter().any(|w| w.contains("incomplete")));
    }

    /// Guard: a recorded secret fill keeps the reference and no value; the
    /// text of an ordinary fill is still lifted as before.
    #[test]
    fn a_recorded_secret_fill_keeps_only_the_reference() {
        let page = "# Login (https://github.com/login)\n  [e1] textbox \"Password\"\n";
        let steps = vec![
            user_step("log in to github"),
            tool_step(
                1,
                json!([call(
                    "o",
                    "browser_open",
                    &json!({"url": "https://github.com/login"})
                )]),
                json!([res("o", "browser_open", page, false)]),
            ),
            tool_step(
                2,
                json!([call(
                    "f",
                    "browser_fill",
                    &json!({"ref": "e1", "secret": "login/github"})
                )]),
                json!([res("f", "browser_fill", "filled e1", false)]),
            ),
        ];
        let rec = Recorder::new()
            .from_steps(&steps, &RecordOptions::default())
            .unwrap();
        let Step::BrowserAction(b) = &rec.workflow.steps[1] else {
            panic!("{:?}", rec.workflow.steps)
        };
        assert_eq!(b.secret.as_deref(), Some("login/github"));
        assert!(b.value.is_none());
        assert!(rec.workflow.params.iter().all(|p| p.name != "text"));
        let json = rec.workflow.to_json().unwrap();
        assert!(json.contains("login/github"));
        rec.workflow.validate().unwrap();
    }

    #[test]
    fn a_browser_act_batch_is_recorded_as_the_single_actions_it_ran() {
        let page = "# Form (https://shop.test/join)
  [e1] textbox \"Name\"
  [e2] textbox \"City\"
  [e3] combobox \"Country\"
  [e4] textbox \"Phone\"
  [e5] textbox \"Notes\"
  [e6] button \"Join\"
  [e7] button \"Later\"
";
        let batch = json!({"steps": [
            {"action": "fill", "ref": "e1", "text": "Ada Lovelace"},
            {"action": "fill", "ref": "e2", "text": "London"},
            {"action": "select", "ref": "e3", "value": "United Kingdom"},
            {"action": "scroll", "dy": 300},
            {"action": "fill", "ref": "e4", "text": "5550100"},
            {"action": "press", "key": "Tab"},
            {"action": "click", "ref": "e6"},
            {"action": "click", "ref": "e7"},
        ]});
        let record = |result: &str| {
            let steps = vec![
                user_step("join the shop"),
                tool_step(
                    1,
                    json!([call(
                        "o",
                        "browser_open",
                        &json!({"url": "https://shop.test/join"})
                    )]),
                    json!([res("o", "browser_open", page, false)]),
                ),
                tool_step(
                    2,
                    json!([call("a", "browser_act", &batch)]),
                    json!([res("a", "browser_act", result, false)]),
                ),
            ];
            Recorder::new()
                .from_steps(&steps, &RecordOptions::default())
                .unwrap()
                .workflow
        };
        let kinds = |wf: &Workflow| -> Vec<(BrowserActionKind, String)> {
            wf.steps
                .iter()
                .filter_map(|s| match s {
                    Step::BrowserAction(a) => Some((
                        a.action,
                        a.target
                            .as_ref()
                            .map(|t| t.name.clone())
                            .unwrap_or_default(),
                    )),
                    _ => None,
                })
                .collect()
        };
        // Stopped after step 7: the eighth step never ran and is not recorded;
        // the scroll is left out like `browser_scroll`.
        let wf = record(
            "done: 7/8
stopped: after step 7 (click e6): the page navigated
# Done (https://shop.test/ok)
",
        );
        assert_eq!(
            kinds(&wf),
            vec![
                (BrowserActionKind::Fill, "Name".to_owned()),
                (BrowserActionKind::Fill, "City".to_owned()),
                (BrowserActionKind::Select, "Country".to_owned()),
                (BrowserActionKind::Fill, "Phone".to_owned()),
                (BrowserActionKind::Press, String::new()),
                (BrowserActionKind::Click, "Join".to_owned()),
            ]
        );
        assert!(!wf.steps.iter().any(|s| matches!(s, Step::Tool(_))));
        // Nothing ran: nothing is recorded.
        assert!(
            kinds(&record(
                "done: 0/8
stopped: step 1"
            ))
            .is_empty()
        );
    }

    #[test]
    fn browser_refs_become_locators_and_urls_become_params() {
        let page =
            "# Shop (https://shop.test/cart)\n  [e1] textbox \"Search\"\n  [e2] button \"Go\"\n";
        let steps = vec![
            user_step("search the shop for red shoes"),
            tool_step(
                1,
                json!([call(
                    "o",
                    "browser_open",
                    &json!({"url": "https://shop.test/cart"})
                )]),
                json!([res("o", "browser_open", page, false)]),
            ),
            tool_step(
                2,
                json!([call(
                    "f",
                    "browser_fill",
                    &json!({"ref": "e1", "text": "red shoes"})
                )]),
                json!([res("f", "browser_fill", "filled", false)]),
            ),
            tool_step(
                3,
                json!([call("g", "browser_click", &json!({"ref": "e2"}))]),
                json!([res("g", "browser_click", "clicked", false)]),
            ),
        ];
        let rec = Recorder::new()
            .from_steps(&steps, &RecordOptions::default())
            .unwrap();
        let wf = rec.workflow;
        let Step::BrowserAction(f) = &wf.steps[1] else {
            panic!()
        };
        assert_eq!(f.url_pattern.as_deref(), Some("https://shop.test/*"));
        assert_eq!(f.value.as_deref(), Some("{text}"));
        assert_eq!(f.target.as_ref().unwrap().name, "Search");
        let Step::Navigate(n) = &wf.steps[0] else {
            panic!()
        };
        assert_eq!(n.url, "https://shop.test/cart");
    }

    #[test]
    fn literal_braces_in_typed_text_are_escaped() {
        let steps = vec![
            user_step("go"),
            tool_step(
                1,
                json!([call("b", "desktop_get_state", &json!({}))]),
                json!([res("b", "desktop_get_state", DESKTOP_TREE, false)]),
            ),
            tool_step(
                2,
                json!([call(
                    "d",
                    "desktop_type",
                    &json!({"index": 1, "text": "{\"a\":1}"})
                )]),
                json!([res("d", "desktop_type", DESKTOP_TREE, false)]),
            ),
        ];
        let rec = Recorder::new()
            .from_steps(&steps, &RecordOptions::default())
            .unwrap();
        let Step::DesktopAction(d) = &rec.workflow.steps[0] else {
            panic!()
        };
        assert_eq!(d.value.as_deref(), Some("{{\"a\":1}}"));
    }

    #[test]
    fn task_145_regression_distinct_run_ids_in_one_millisecond_have_distinct_names() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open_in_memory(dir.path()).unwrap();
        let user_id = protocol::UserId::new();
        let bot_id = protocol::BotId::new();
        storage
            .insert_user(&storage::User {
                id: user_id,
                name: "test".into(),
                created_at_ms: 0,
            })
            .unwrap();
        storage
            .insert_bot(&storage::Bot {
                id: bot_id,
                user_id,
                name: "test".into(),
                config: json!({}),
                created_at_ms: 0,
            })
            .unwrap();
        let mut names = Vec::new();
        // Same v7 millisecond and last 12 hex digits; truncating either end
        // would lose the difference between these two valid run identifiers.
        for id in [
            "00000000-0000-7000-8000-000000000001",
            "00000000-0000-7001-8000-000000000001",
        ] {
            let run_id = RunId(Uuid::parse_str(id).unwrap());
            storage
                .insert_run(&storage::Run {
                    id: run_id,
                    user_id,
                    bot_id,
                    chat_id: None,
                    kind: "turn".into(),
                    state: "succeeded".into(),
                    error: None,
                    created_at_ms: 0,
                    updated_at_ms: 0,
                    parent_run_id: None,
                    spec: None,
                    computer_target: None,
                    computer_node: None,
                    computer_node_name: None,
                    paused_at_ms: None,
                    pause_reason: None,
                })
                .unwrap();
            let mut step = tool_step(
                1,
                json!([call("a", "shell_run", &json!({"command": "echo ok"}))]),
                json!([res("a", "shell_run", "ok", false)]),
            );
            step.run_id = run_id;
            step.user_id = user_id;
            storage.insert_step(&step).unwrap();
            names.push(
                Recorder::new()
                    .from_run(&storage, run_id, &RecordOptions::default())
                    .unwrap()
                    .workflow
                    .name,
            );
        }
        assert_ne!(names[0], names[1]);
        assert!(names.iter().all(|name| crate::format::valid_name(name)));
    }

    #[test]
    fn default_name_has_utc_timestamp_and_supports_unusual_ids() {
        let id = Uuid::parse_str("00000000-0000-7000-8000-000000000001").unwrap();
        assert_eq!(
            default_recording_name(RunId(id)),
            "run-19700101-0000-00000000000070008000000000000001"
        );
        for id in [Uuid::nil(), Uuid::max(), id] {
            assert!(crate::format::valid_name(&default_recording_name(RunId(
                id
            ))));
        }
    }

    #[test]
    fn explicit_name_wins_and_reviewed_proposal_saves_create_only() {
        let steps = [tool_step(
            1,
            json!([call("a", "shell_run", &json!({"command": "echo ok"}))]),
            json!([res("a", "shell_run", "ok", false)]),
        )];
        let recording = Recorder::new()
            .from_steps(
                &steps,
                &RecordOptions {
                    name: Some("chosen".into()),
                    description: None,
                },
            )
            .unwrap();
        assert_eq!(recording.workflow.name, "chosen");
        let dir = tempfile::tempdir().unwrap();
        let store = WorkflowStore::new(dir.path());
        assert_eq!(recording.save_new(&store).unwrap(), 1);
        assert!(
            matches!(recording.save_new(&store), Err(SaveError::AlreadyExists(name)) if name == "chosen")
        );
    }
}
