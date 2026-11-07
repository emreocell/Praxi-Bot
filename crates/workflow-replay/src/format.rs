//! The workflow file format (TOML or JSON, versioned).
//!
//! ```toml
//! version = 1
//! name = "notepad_note"
//! description = "Open Notepad and type a note"
//!
//! [[params]]
//! name = "text"
//! type = "string"            # string | number | bool
//! default = "hello"          # no default = required
//! description = "Text to type"
//!
//! [[steps]]
//! kind = "launch"
//! executable = "notepad.exe"
//!
//! [[steps]]
//! kind = "desktop_action"
//! app = "notepad.exe"
//! window_title_pattern = "*Notepad"   # `*` and `?` wildcards, case-insensitive
//! action = "type"                      # click | type | set_value | key
//! value = "{text}"
//! [steps.target]
//! role = "edit"
//! name = "Text editor"
//! fingerprint = "9f2c0a41d3b7e855"     # recorded identity (hex)
//! ```
//!
//! Step kinds: `desktop_action`, `browser_action`, `navigate`, `launch`,
//! `wait_for`, `shell`, `tool`, `agent`. Only `agent` may involve a model, and
//! the replayer never calls one itself: it hands over at that step.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tool_registry::{Effect, PermissionCategory};

use crate::error::WorkflowError;
use crate::template::{self, Vars, is_ident};

/// Current format version.
pub const FORMAT_VERSION: u32 = 1;

/// Declared type of a parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParamType {
    /// Free text.
    #[default]
    String,
    /// A number (integer or decimal).
    Number,
    /// `true` / `false`.
    Bool,
}

/// One workflow parameter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Param {
    /// Identifier used as `{name}` in templates.
    pub name: String,
    /// Declared type.
    #[serde(rename = "type", default)]
    pub ty: ParamType,
    /// Value used when the caller gives none; without it the parameter is required.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// Human description.
    #[serde(default)]
    pub description: String,
}

/// Extra hint for finding an unnamed or renamed desktop element.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PositionHint {
    /// Recorded centre, screen x.
    pub x: f64,
    /// Recorded centre, screen y.
    pub y: f64,
    /// 0-based rank among the snapshot's nodes of the same role.
    pub role_ordinal: usize,
    /// How many nodes of that role the snapshot had.
    pub role_count: usize,
}

/// Where a desktop element is, in order of trust.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DesktopLocator {
    /// Accessibility role.
    pub role: String,
    /// Accessible name (may be empty).
    #[serde(default)]
    pub name: String,
    /// Recorded identity: hex of `Fingerprint::compute(role, name)`.
    pub fingerprint: String,
    /// Automation id (reserved: used once the backend exposes it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automation_id: Option<String>,
    /// Class name (reserved, like `automation_id`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class_name: Option<String>,
    /// Which of several identical role+name elements (0-based); recorded only
    /// when the recording screen had more than one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nth: Option<usize>,
    /// Position hint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<PositionHint>,
}

/// Where a browser element is, in order of trust.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrowserLocator {
    /// Accessibility role; empty to skip the role+name strategy.
    #[serde(default)]
    pub role: String,
    /// Accessible name / visible text.
    #[serde(default)]
    pub name: String,
    /// Recorded identity: hex of `Fingerprint::compute(role, name)`.
    pub fingerprint: String,
    /// `aria-label` text (matches the accessible name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aria_label: Option<String>,
    /// Last resort CSS selector; must match exactly one element.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub css: Option<String>,
    /// Which of several identical role+name elements (0-based).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nth: Option<usize>,
}

/// What a desktop step does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopActionKind {
    /// Click the target.
    Click,
    /// Focus the target, then type `value`.
    Type,
    /// Set the target's value to `value`.
    SetValue,
    /// Press `value` (`ctrl+s`), after focusing the target when there is one.
    Key,
}

/// What a browser step does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserActionKind {
    /// Click the target.
    Click,
    /// Replace the target's text with `value`.
    Fill,
    /// Choose option `value` in the target.
    Select,
    /// Press key `value`.
    Press,
    /// History back.
    Back,
}

/// A desktop step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DesktopAction {
    /// Application (informational).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// Window the step works in; exactly one visible window must match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_title_pattern: Option<String>,
    /// Element to act on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<DesktopLocator>,
    /// The action.
    pub action: DesktopActionKind,
    /// Value template (text, value or key chord).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// Click delivery (`accessibility`, `app_post`, `global`); default accessibility.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
}

/// A browser step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrowserAction {
    /// The page must match this URL pattern (`*`, `?`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_pattern: Option<String>,
    /// Element to act on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<BrowserLocator>,
    /// The action.
    pub action: BrowserActionKind,
    /// Value template.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

/// Open a URL in the browser.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Navigate {
    /// URL template.
    pub url: String,
    /// Browser profile name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
}

/// Launch a desktop application.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Launch {
    /// Program (template).
    pub executable: String,
    /// Arguments (templates).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
}

/// Wait until something is true; set exactly one condition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WaitFor {
    /// A visible window title matches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_title_pattern: Option<String>,
    /// Browser page contains this text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Browser CSS selector matches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    /// Browser network is idle.
    #[serde(default)]
    pub idle: bool,
    /// Give up after this many seconds (default 15).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}

/// A shell command (runs through `shell_run`, still permission gated).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Shell {
    /// Command template.
    pub command: String,
    /// `powershell`, `cmd`, `bash` or `sh`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    /// Working directory template.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

/// Any registered tool with a templated input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolStep {
    /// Tool name.
    pub tool: String,
    /// Input; every string may contain `{param}`.
    pub input: Value,
}

/// An explicit LLM sub-task; the replayer stops here and hands over.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentStep {
    /// Instruction template for the agent.
    pub instruction: String,
}

/// One workflow step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Step {
    /// Act on a desktop element.
    DesktopAction(DesktopAction),
    /// Act on a browser element.
    BrowserAction(BrowserAction),
    /// Open a URL.
    Navigate(Navigate),
    /// Launch an application.
    Launch(Launch),
    /// Wait for a condition.
    WaitFor(WaitFor),
    /// Run a shell command.
    Shell(Shell),
    /// Call a tool.
    Tool(ToolStep),
    /// Hand a sub-task to the agent.
    Agent(AgentStep),
}

impl Step {
    /// The `kind` string of this step.
    #[must_use]
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::DesktopAction(_) => "desktop_action",
            Self::BrowserAction(_) => "browser_action",
            Self::Navigate(_) => "navigate",
            Self::Launch(_) => "launch",
            Self::WaitFor(_) => "wait_for",
            Self::Shell(_) => "shell",
            Self::Tool(_) => "tool",
            Self::Agent(_) => "agent",
        }
    }

    /// One-line human description (templates are shown unrendered).
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::DesktopAction(d) => {
                let target = d
                    .target
                    .as_ref()
                    .map_or(String::new(), |t| format!(" {} {:?}", t.role, t.name));
                format!(
                    "desktop {}{target}{}",
                    snake(&d.action),
                    d.value
                        .as_ref()
                        .map_or(String::new(), |v| format!(" value={v:?}"))
                )
            }
            Self::BrowserAction(b) => {
                let target = b
                    .target
                    .as_ref()
                    .map_or(String::new(), |t| format!(" {} {:?}", t.role, t.name));
                format!(
                    "browser {}{target}{}",
                    snake(&b.action),
                    b.value
                        .as_ref()
                        .map_or(String::new(), |v| format!(" value={v:?}"))
                )
            }
            Self::Navigate(n) => format!("navigate to {}", n.url),
            Self::Launch(l) => format!("launch {}", l.executable),
            Self::WaitFor(w) => format!(
                "wait for {}",
                w.window_title_pattern
                    .as_deref()
                    .map(|p| format!("window {p:?}"))
                    .or_else(|| w.text.as_deref().map(|t| format!("text {t:?}")))
                    .or_else(|| w.selector.as_deref().map(|s| format!("selector {s:?}")))
                    .unwrap_or_else(|| "network idle".into())
            ),
            Self::Shell(s) => format!("shell: {}", s.command),
            Self::Tool(t) => format!("tool {}", t.tool),
            Self::Agent(a) => format!("agent: {}", a.instruction),
        }
    }

    /// Templates this step contains.
    fn templates(&self) -> Result<Vec<String>, WorkflowError> {
        let mut refs = Vec::new();
        let mut add = |s: &str| -> Result<(), WorkflowError> {
            refs.extend(template::referenced(s)?);
            Ok(())
        };
        match self {
            Self::DesktopAction(d) => {
                if let Some(v) = &d.value {
                    add(v)?;
                }
            }
            Self::BrowserAction(b) => {
                if let Some(v) = &b.value {
                    add(v)?;
                }
            }
            Self::Navigate(n) => add(&n.url)?,
            Self::Launch(l) => {
                add(&l.executable)?;
                for a in &l.args {
                    add(a)?;
                }
            }
            Self::WaitFor(_) => {}
            Self::Shell(s) => {
                add(&s.command)?;
                if let Some(c) = &s.cwd {
                    add(c)?;
                }
            }
            Self::Tool(t) => template::referenced_in_value(&t.input, &mut refs)?,
            Self::Agent(a) => add(&a.instruction)?,
        }
        Ok(refs)
    }

    /// Permission categories and the highest effect this step needs.
    #[must_use]
    pub fn permission(&self) -> Option<(PermissionCategory, Effect)> {
        match self {
            Self::DesktopAction(_) | Self::Launch(_) => {
                Some((PermissionCategory::Desktop, Effect::Write))
            }
            Self::BrowserAction(_) | Self::Navigate(_) => {
                Some((PermissionCategory::Browser, Effect::Write))
            }
            Self::WaitFor(w) => Some(if w.window_title_pattern.is_some() {
                (PermissionCategory::Desktop, Effect::Read)
            } else {
                (PermissionCategory::Browser, Effect::Read)
            }),
            Self::Shell(_) => Some((PermissionCategory::Shell, Effect::Destructive)),
            Self::Tool(_) | Self::Agent(_) => None,
        }
    }
}

/// The serde (`snake_case`) name of a unit enum value.
fn snake<T: Serialize>(v: &T) -> String {
    serde_json::to_value(v)
        .ok()
        .and_then(|x| x.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// A post-condition checked after the last step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SuccessCheck {
    /// A visible window title matches.
    WindowTitle {
        /// Title pattern.
        pattern: String,
    },
    /// The window's UI tree contains this text in a name or value.
    DesktopText {
        /// Window to read (default: focused).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        window_title_pattern: Option<String>,
        /// Text template.
        contains: String,
    },
    /// The browser snapshot contains this text.
    BrowserText {
        /// Text template.
        contains: String,
    },
}

/// A recorded, parameterised, replayable task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Workflow {
    /// Format version.
    pub version: u32,
    /// Identifier (`a-z0-9_-`).
    pub name: String,
    /// What it does.
    #[serde(default)]
    pub description: String,
    /// Declared parameters.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<Param>,
    /// Steps, in order.
    #[serde(default)]
    pub steps: Vec<Step>,
    /// Checked after the last step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub success_check: Option<SuccessCheck>,
}

/// Whether `name` is a legal workflow name.
#[must_use]
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

impl Workflow {
    /// Empty workflow with the current version.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            version: FORMAT_VERSION,
            name: name.into(),
            description: String::new(),
            params: Vec::new(),
            steps: Vec::new(),
            success_check: None,
        }
    }

    /// Parse TOML and validate.
    ///
    /// # Errors
    /// Parse or validation failure.
    pub fn from_toml(s: &str) -> Result<Self, WorkflowError> {
        let w: Self = toml::from_str(s).map_err(|e| WorkflowError::Format(e.to_string()))?;
        w.validate()?;
        Ok(w)
    }

    /// Serialise to TOML.
    ///
    /// # Errors
    /// A value TOML cannot hold (e.g. JSON `null` in a tool input).
    pub fn to_toml(&self) -> Result<String, WorkflowError> {
        toml::to_string_pretty(self).map_err(|e| WorkflowError::Format(e.to_string()))
    }

    /// Parse JSON and validate.
    ///
    /// # Errors
    /// Parse or validation failure.
    pub fn from_json(s: &str) -> Result<Self, WorkflowError> {
        let w: Self = serde_json::from_str(s).map_err(|e| WorkflowError::Format(e.to_string()))?;
        w.validate()?;
        Ok(w)
    }

    /// Serialise to JSON.
    ///
    /// # Errors
    /// Never in practice.
    pub fn to_json(&self) -> Result<String, WorkflowError> {
        serde_json::to_string_pretty(self).map_err(|e| WorkflowError::Format(e.to_string()))
    }

    /// Check the structural rules.
    ///
    /// # Errors
    /// The first rule that fails.
    pub fn validate(&self) -> Result<(), WorkflowError> {
        let bad = |m: String| Err(WorkflowError::Invalid(m));
        if self.version == 0 || self.version > FORMAT_VERSION {
            return bad(format!(
                "unsupported version {} (this build reads up to {FORMAT_VERSION})",
                self.version
            ));
        }
        if !valid_name(&self.name) {
            return bad(format!("bad name {:?}", self.name));
        }
        let mut declared = BTreeSet::new();
        for p in &self.params {
            if !is_ident(&p.name) {
                return bad(format!("bad parameter name {:?}", p.name));
            }
            if !declared.insert(p.name.as_str()) {
                return bad(format!("duplicate parameter {:?}", p.name));
            }
            if let Some(d) = &p.default {
                check_type(&p.name, p.ty, d)?;
            }
        }
        let mut refs: Vec<String> = Vec::new();
        for (i, s) in self.steps.iter().enumerate() {
            validate_step(i, s)?;
            refs.extend(s.templates()?);
        }
        match &self.success_check {
            Some(
                SuccessCheck::DesktopText { contains, .. } | SuccessCheck::BrowserText { contains },
            ) => {
                refs.extend(template::referenced(contains)?);
            }
            Some(SuccessCheck::WindowTitle { .. }) | None => {}
        }
        for r in refs {
            if !declared.contains(r.as_str()) {
                return bad(format!("template uses undeclared parameter `{r}`"));
            }
        }
        Ok(())
    }

    /// Permission categories the steps need (for review before saving/running).
    #[must_use]
    pub fn categories(&self) -> BTreeSet<PermissionCategory> {
        self.steps
            .iter()
            .filter_map(|s| s.permission().map(|(c, _)| c))
            .collect()
    }

    /// Declared parameter types by name.
    #[must_use]
    pub fn param_types(&self) -> BTreeMap<String, ParamType> {
        self.params.iter().map(|p| (p.name.clone(), p.ty)).collect()
    }

    /// Merge caller values over defaults and type-check them.
    ///
    /// # Errors
    /// Unknown parameter, missing required parameter or wrong type.
    pub fn bind_params(
        &self,
        supplied: &serde_json::Map<String, Value>,
    ) -> Result<Vars, WorkflowError> {
        let err = |m: String| Err(WorkflowError::Params(m));
        for k in supplied.keys() {
            if !self.params.iter().any(|p| p.name == *k) {
                return err(format!("unknown parameter `{k}`"));
            }
        }
        let mut out = Vars::new();
        for p in &self.params {
            let raw = match supplied.get(&p.name) {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Number(n)) => n.to_string(),
                Some(Value::Bool(b)) => b.to_string(),
                Some(other) => {
                    return err(format!(
                        "parameter `{}` must be a scalar, got {other}",
                        p.name
                    ));
                }
                None => match &p.default {
                    Some(d) => d.clone(),
                    None => return err(format!("missing required parameter `{}`", p.name)),
                },
            };
            check_type(&p.name, p.ty, &raw)?;
            out.insert(p.name.clone(), raw);
        }
        Ok(out)
    }
}

fn check_type(name: &str, ty: ParamType, v: &str) -> Result<(), WorkflowError> {
    let ok = match ty {
        ParamType::String => true,
        ParamType::Number => v.parse::<f64>().is_ok_and(f64::is_finite),
        ParamType::Bool => v == "true" || v == "false",
    };
    if ok {
        Ok(())
    } else {
        Err(WorkflowError::Params(format!(
            "parameter `{name}` must be {ty:?}, got {v:?}"
        )))
    }
}

fn validate_step(i: usize, s: &Step) -> Result<(), WorkflowError> {
    let bad = |m: &str| {
        Err(WorkflowError::Invalid(format!(
            "step {i} ({}): {m}",
            s.kind_name()
        )))
    };
    match s {
        Step::DesktopAction(d) => match d.action {
            DesktopActionKind::Click if d.target.is_none() => bad("click needs a target"),
            DesktopActionKind::Type | DesktopActionKind::SetValue
                if d.target.is_none() || d.value.is_none() =>
            {
                bad("needs a target and a value")
            }
            DesktopActionKind::Key if d.value.is_none() => bad("key needs a value"),
            _ => Ok(()),
        },
        Step::BrowserAction(b) => match b.action {
            BrowserActionKind::Click if b.target.is_none() => bad("click needs a target"),
            BrowserActionKind::Fill | BrowserActionKind::Select
                if b.target.is_none() || b.value.is_none() =>
            {
                bad("needs a target and a value")
            }
            BrowserActionKind::Press if b.value.is_none() => bad("press needs a value"),
            _ => Ok(()),
        },
        Step::WaitFor(w) => {
            let n = usize::from(w.window_title_pattern.is_some())
                + usize::from(w.text.is_some())
                + usize::from(w.selector.is_some())
                + usize::from(w.idle);
            if n == 1 {
                Ok(())
            } else {
                bad("set exactly one wait condition")
            }
        }
        Step::Tool(t) if !t.input.is_object() => bad("input must be an object"),
        Step::Navigate(_) | Step::Launch(_) | Step::Shell(_) | Step::Tool(_) | Step::Agent(_) => {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    pub(crate) fn sample() -> Workflow {
        let mut w = Workflow::new("notepad_note");
        w.description = "Open Notepad and type".into();
        w.params.push(Param {
            name: "text".into(),
            ty: ParamType::String,
            default: Some("hello".into()),
            description: "Text".into(),
        });
        w.steps.push(Step::Launch(Launch {
            executable: "notepad.exe".into(),
            args: vec![],
        }));
        w.steps.push(Step::DesktopAction(DesktopAction {
            app: Some("notepad.exe".into()),
            window_title_pattern: Some("*Notepad".into()),
            target: Some(DesktopLocator {
                role: "edit".into(),
                name: "Text editor".into(),
                fingerprint: "00ff".into(),
                automation_id: None,
                class_name: None,
                nth: None,
                position: Some(PositionHint {
                    x: 10.0,
                    y: 20.5,
                    role_ordinal: 0,
                    role_count: 1,
                }),
            }),
            action: DesktopActionKind::Type,
            value: Some("{text}".into()),
            method: None,
        }));
        w.steps.push(Step::Tool(ToolStep {
            tool: "fs_write".into(),
            input: json!({"path": "a.txt", "content": "{text}", "n": 3}),
        }));
        w.success_check = Some(SuccessCheck::WindowTitle {
            pattern: "*Notepad".into(),
        });
        w
    }

    #[test]
    fn toml_and_json_round_trip() {
        let w = sample();
        let t = w.to_toml().unwrap();
        assert!(t.contains("kind = \"desktop_action\""), "{t}");
        assert_eq!(Workflow::from_toml(&t).unwrap(), w);
        assert_eq!(Workflow::from_json(&w.to_json().unwrap()).unwrap(), w);
    }

    #[test]
    fn rejects_undeclared_params_and_future_versions() {
        let mut w = sample();
        w.steps.push(Step::Agent(AgentStep {
            instruction: "do {nope}".into(),
        }));
        assert!(w.validate().is_err());
        let mut w = sample();
        w.version = 99;
        assert!(w.validate().is_err());
    }

    #[test]
    fn bind_params_applies_defaults_and_types() {
        let mut w = sample();
        w.params.push(Param {
            name: "n".into(),
            ty: ParamType::Number,
            default: None,
            description: String::new(),
        });
        let none = serde_json::Map::new();
        assert!(w.bind_params(&none).is_err(), "n is required");
        let ok = json!({"n": 4}).as_object().unwrap().clone();
        let v = w.bind_params(&ok).unwrap();
        assert_eq!(v["text"], "hello");
        assert_eq!(v["n"], "4");
        let bad = json!({"n": "abc"}).as_object().unwrap().clone();
        assert!(w.bind_params(&bad).is_err());
        let unknown = json!({"n": 1, "zzz": 1}).as_object().unwrap().clone();
        assert!(w.bind_params(&unknown).is_err());
    }

    #[test]
    fn wait_for_needs_exactly_one_condition() {
        let mut w = Workflow::new("w");
        w.steps.push(Step::WaitFor(WaitFor {
            window_title_pattern: Some("a".into()),
            text: Some("b".into()),
            selector: None,
            idle: false,
            timeout_secs: None,
        }));
        assert!(w.validate().is_err());
    }
}
