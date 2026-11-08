//! Deterministic replay: no model, every step permission-gated, locator and
//! fingerprint verified before each action, hand over to the agent on any
//! deviation.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use builtin_tools::{
    BrowserDriver, ControlRequest, DesktopControlGate, GateRequest, ToolGate, Wait, raw_command_for,
};
use desktop_api::snapshot::AppSpec;
use desktop_api::{Action, ClickMethod, DesktopBackend, Snapshot, Target};
use protocol::event::{WorkflowDrift, WorkflowStepCompleted, WorkflowStepStarted};
use protocol::{Event, RunId};
use serde::Serialize;
use serde_json::{Value, json};
use tool_registry::{
    ContentPart, Effect, PermissionCategory, ToolContext, ToolError, ToolRegistry,
};

use crate::error::{ReplayError, WorkflowError};
use crate::format::{
    BrowserAction, BrowserActionKind, DesktopAction, DesktopActionKind, Step, SuccessCheck,
    WaitFor, Workflow,
};
use crate::locator::{
    Drift, Found, Strategy, glob_match, parse_browser_snapshot, resolve_browser, resolve_desktop,
};
use crate::template::{self, Vars};

/// Maximum nesting of workflows started from other workflows.
pub const MAX_WORKFLOW_DEPTH: usize = 4;

tokio::task_local! {
    /// Names of the workflows currently being replayed on this task chain.
    static WORKFLOW_STACK: Vec<String>;
}

/// Workflows currently being replayed (outermost first) on this task.
pub(crate) fn active_workflows() -> Vec<String> {
    WORKFLOW_STACK.try_with(Clone::clone).unwrap_or_default()
}

/// Receives replay events.
pub type EventSink = Arc<dyn Fn(Event) + Send + Sync>;

/// The things a replay acts on.
#[derive(Clone)]
pub struct Handles {
    /// Desktop backend (needed by desktop steps).
    pub desktop: Option<Arc<dyn DesktopBackend>>,
    /// Browser driver (needed by browser steps).
    pub browser: Option<Arc<dyn BrowserDriver>>,
    /// Registry for `tool` and `shell` steps.
    pub tools: ToolRegistry,
}

/// A control gate that allows everything (tests, headless runs with no human
/// at the wheel). Must be chosen explicitly: a missing gate refuses.
pub struct AllowAllControl;

#[async_trait::async_trait]
impl DesktopControlGate for AllowAllControl {
    async fn before_action(&self, _req: &ControlRequest) -> Result<(), String> {
        Ok(())
    }
}

/// Replay settings.
#[derive(Clone)]
pub struct ReplayOptions {
    /// Run the replay belongs to (given to the gate; events carry it when it parses).
    pub run_id: String,
    /// Event receiver.
    pub events: Option<EventSink>,
    /// Treat a changed fingerprint on any strategy as drift.
    pub strict_fingerprint: bool,
    /// How long an element may take to appear before it counts as drift.
    pub resolve_timeout: Duration,
    /// Poll interval while waiting.
    pub poll_interval: Duration,
    /// Upper bound for one step.
    pub step_timeout: Duration,
    /// Pause after launching an application.
    pub launch_settle: Duration,
    /// Pause after a desktop / browser action.
    pub action_settle: Duration,
    /// Bot that started the replay, forwarded to tool steps.
    pub bot_id: Option<String>,
    /// Desktop control gate, asked before every native / browser mutation
    /// (the user may have taken the wheel). `None` fails closed: acting steps
    /// are refused. Use [`AllowAllControl`] only where no human is at the wheel.
    pub control: Option<Arc<dyn DesktopControlGate>>,
}

impl Default for ReplayOptions {
    fn default() -> Self {
        Self {
            run_id: String::new(),
            events: None,
            strict_fingerprint: false,
            resolve_timeout: Duration::from_secs(3),
            poll_interval: Duration::from_millis(250),
            step_timeout: Duration::from_secs(60),
            launch_settle: Duration::from_millis(1500),
            action_settle: Duration::from_millis(300),
            bot_id: None,
            control: None,
        }
    }
}

impl ReplayOptions {
    /// No pauses or retries (tests with fake backends).
    #[must_use]
    pub fn instant() -> Self {
        Self {
            resolve_timeout: Duration::ZERO,
            poll_interval: Duration::from_millis(1),
            launch_settle: Duration::ZERO,
            action_settle: Duration::ZERO,
            control: Some(Arc::new(AllowAllControl)),
            ..Self::default()
        }
    }
}

/// Why control goes back to the agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HandoverReason {
    /// The screen no longer matches the recording.
    Drift {
        /// What differed.
        detail: String,
    },
    /// More than one element matched.
    Ambiguous {
        /// What matched.
        detail: String,
    },
    /// The action itself failed.
    StepFailed {
        /// Error text.
        detail: String,
    },
    /// The permission gate refused the step (the user said no, or policy did).
    /// Do not retry it without asking.
    PermissionDenied {
        /// Denial text.
        detail: String,
    },
    /// An explicit `agent` step: the sub-task is for the model.
    AgentStep {
        /// Rendered instruction.
        instruction: String,
    },
    /// All steps ran but the success check failed.
    SuccessCheckFailed {
        /// What was expected.
        detail: String,
    },
    /// The step needs a backend that was not provided.
    MissingBackend {
        /// Which one.
        detail: String,
    },
}

impl HandoverReason {
    /// Short stable name.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Drift { .. } => "drift",
            Self::Ambiguous { .. } => "ambiguous",
            Self::StepFailed { .. } => "step_failed",
            Self::PermissionDenied { .. } => "permission_denied",
            Self::AgentStep { .. } => "agent_step",
            Self::SuccessCheckFailed { .. } => "success_check_failed",
            Self::MissingBackend { .. } => "missing_backend",
        }
    }

    /// Human text.
    #[must_use]
    pub fn detail(&self) -> &str {
        match self {
            Self::Drift { detail }
            | Self::Ambiguous { detail }
            | Self::StepFailed { detail }
            | Self::PermissionDenied { detail }
            | Self::SuccessCheckFailed { detail }
            | Self::MissingBackend { detail } => detail,
            Self::AgentStep { instruction } => instruction,
        }
    }
}

/// What the replay had done when it stopped.
#[derive(Debug, Clone, Serialize)]
pub struct PartialState {
    /// Steps fully executed.
    pub completed: usize,
    /// Parameter values in effect.
    pub params: BTreeMap<String, String>,
    /// Output text of the last executed step, truncated.
    pub last_output: Option<String>,
    /// Descriptions of the steps that did not run (the stopped one first).
    pub remaining: Vec<String>,
}

/// The replay stopped before the end.
#[derive(Debug, Clone, Serialize)]
pub struct Handover {
    /// Index of the step that did not run (`steps.len()` for a failed success check).
    pub step_index: usize,
    /// Why.
    pub reason: HandoverReason,
    /// State to continue from.
    pub partial_state: PartialState,
}

/// Result of one executed step.
#[derive(Debug, Clone, Serialize)]
pub struct StepReport {
    /// Step index.
    pub index: usize,
    /// Step kind.
    pub kind: String,
    /// Locator strategy that found the target.
    pub resolved_via: Option<String>,
    /// The element's fingerprint differed from the recording.
    pub fingerprint_changed: bool,
    /// Milliseconds the step took.
    pub duration_ms: u64,
}

/// How the replay ended.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ReplayOutcome {
    /// Every step ran and the success check (if any) passed.
    Completed,
    /// Stopped; continue with the agent.
    Handover(Handover),
}

/// Metrics and result of a replay.
#[derive(Debug, Clone, Serialize)]
pub struct ReplayReport {
    /// Workflow name.
    pub workflow: String,
    /// Outcome.
    pub outcome: ReplayOutcome,
    /// Steps executed.
    pub steps_replayed: usize,
    /// Steps in the workflow.
    pub steps_total: usize,
    /// Model calls made by the replayer. Always 0: the replayer cannot call a model.
    pub llm_calls: u32,
    /// Wall time in milliseconds.
    pub wall_time_ms: u64,
    /// Per-step details.
    pub steps: Vec<StepReport>,
}

impl ReplayReport {
    /// Whether the replay ran to the end successfully.
    #[must_use]
    pub fn is_completed(&self) -> bool {
        matches!(self.outcome, ReplayOutcome::Completed)
    }

    /// The handover, if the replay stopped.
    #[must_use]
    pub fn handover(&self) -> Option<&Handover> {
        match &self.outcome {
            ReplayOutcome::Handover(h) => Some(h),
            ReplayOutcome::Completed => None,
        }
    }
}

enum Stop {
    Drift(String),
    Ambiguous(String),
    Failed(String),
    Denied(String),
    Agent(String),
    Missing(String),
}

impl From<Stop> for HandoverReason {
    fn from(s: Stop) -> Self {
        match s {
            Stop::Drift(detail) => Self::Drift { detail },
            Stop::Ambiguous(detail) => Self::Ambiguous { detail },
            Stop::Failed(detail) => Self::StepFailed { detail },
            Stop::Denied(detail) => Self::PermissionDenied { detail },
            Stop::Agent(instruction) => Self::AgentStep { instruction },
            Stop::Missing(detail) => Self::MissingBackend { detail },
        }
    }
}

fn drift_stop(d: &Drift) -> Stop {
    match d {
        Drift::Ambiguous { .. } => Stop::Ambiguous(d.to_string()),
        _ => Stop::Drift(d.to_string()),
    }
}

#[derive(Default)]
struct Done {
    resolved_via: Option<Strategy>,
    fingerprint_changed: bool,
    output: Option<String>,
}

fn tpl(e: &crate::error::WorkflowError) -> Stop {
    Stop::Failed(e.to_string())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}…")
    }
}

/// Runs workflows.
pub struct Replayer {
    handles: Handles,
    opts: ReplayOptions,
}

impl Replayer {
    /// A replayer over `handles`.
    #[must_use]
    pub fn new(handles: Handles, opts: ReplayOptions) -> Self {
        Self { handles, opts }
    }

    fn emit(&self, e: Event) {
        if let Some(sink) = &self.opts.events {
            sink(e);
        }
    }

    fn run_uuid(&self) -> Option<RunId> {
        self.opts.run_id.parse::<uuid::Uuid>().ok().map(RunId)
    }

    /// Replay `wf` with `params` (merged over defaults). Every step goes
    /// through `gate` first.
    ///
    /// # Errors
    /// The workflow is invalid or the parameters do not fit; nothing ran.
    pub async fn replay(
        &self,
        wf: &Workflow,
        params: &serde_json::Map<String, Value>,
        gate: &dyn ToolGate,
    ) -> Result<ReplayReport, ReplayError> {
        let mut stack = active_workflows();
        if stack.iter().any(|n| n == &wf.name) {
            return Err(WorkflowError::Invalid(format!(
                "workflow `{}` would call itself (active: {})",
                wf.name,
                stack.join(" -> ")
            ))
            .into());
        }
        if stack.len() >= MAX_WORKFLOW_DEPTH {
            return Err(WorkflowError::Invalid(format!(
                "workflows nested deeper than {MAX_WORKFLOW_DEPTH} levels"
            ))
            .into());
        }
        stack.push(wf.name.clone());
        WORKFLOW_STACK
            .scope(stack, self.replay_inner(wf, params, gate))
            .await
    }

    async fn replay_inner(
        &self,
        wf: &Workflow,
        params: &serde_json::Map<String, Value>,
        gate: &dyn ToolGate,
    ) -> Result<ReplayReport, ReplayError> {
        wf.validate()?;
        let vars = wf.bind_params(params)?;
        let types = wf.param_types();
        let start = Instant::now();
        let mut reports: Vec<StepReport> = Vec::new();
        let mut last_output: Option<String> = None;

        for (i, step) in wf.steps.iter().enumerate() {
            let step_start = Instant::now();
            self.emit(Event::WorkflowStepStarted(WorkflowStepStarted {
                run_id: self.run_uuid(),
                workflow: wf.name.clone(),
                step_index: u32::try_from(i).unwrap_or(u32::MAX),
                kind: step.kind_name().to_owned(),
            }));
            let result = match tokio::time::timeout(
                self.opts.step_timeout,
                self.run_step(step, &vars, &types, gate),
            )
            .await
            {
                Ok(r) => r,
                Err(_) => Err(Stop::Failed(format!(
                    "step timed out after {:?}",
                    self.opts.step_timeout
                ))),
            };
            match result {
                Ok(done) => {
                    self.emit(Event::WorkflowStepCompleted(WorkflowStepCompleted {
                        run_id: self.run_uuid(),
                        workflow: wf.name.clone(),
                        step_index: u32::try_from(i).unwrap_or(u32::MAX),
                        resolved_via: done.resolved_via.map(|s| s.as_str().to_owned()),
                    }));
                    if done.output.is_some() {
                        last_output = done.output;
                    }
                    reports.push(StepReport {
                        index: i,
                        kind: step.kind_name().to_owned(),
                        resolved_via: done.resolved_via.map(|s| s.as_str().to_owned()),
                        fingerprint_changed: done.fingerprint_changed,
                        duration_ms: millis(step_start.elapsed()),
                    });
                }
                Err(stop) => {
                    let reason = HandoverReason::from(stop);
                    return Ok(self.stopped(wf, &vars, reports, last_output, i, reason, start));
                }
            }
        }

        if let Some(check) = &wf.success_check
            && let Err(detail) = self.check(check, &vars).await
        {
            let reason = HandoverReason::SuccessCheckFailed { detail };
            return Ok(self.stopped(
                wf,
                &vars,
                reports,
                last_output,
                wf.steps.len(),
                reason,
                start,
            ));
        }
        Ok(ReplayReport {
            workflow: wf.name.clone(),
            outcome: ReplayOutcome::Completed,
            steps_replayed: reports.len(),
            steps_total: wf.steps.len(),
            llm_calls: 0,
            wall_time_ms: millis(start.elapsed()),
            steps: reports,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn stopped(
        &self,
        wf: &Workflow,
        vars: &Vars,
        reports: Vec<StepReport>,
        last_output: Option<String>,
        step_index: usize,
        reason: HandoverReason,
        start: Instant,
    ) -> ReplayReport {
        self.emit(Event::WorkflowDrift(WorkflowDrift {
            run_id: self.run_uuid(),
            workflow: wf.name.clone(),
            step_index: u32::try_from(step_index).unwrap_or(u32::MAX),
            reason: format!("{}: {}", reason.as_str(), truncate(reason.detail(), 300)),
        }));
        ReplayReport {
            workflow: wf.name.clone(),
            steps_replayed: reports.len(),
            steps_total: wf.steps.len(),
            llm_calls: 0,
            wall_time_ms: millis(start.elapsed()),
            outcome: ReplayOutcome::Handover(Handover {
                step_index,
                reason,
                partial_state: PartialState {
                    completed: reports.len(),
                    params: vars.clone(),
                    last_output,
                    remaining: wf.steps[step_index.min(wf.steps.len())..]
                        .iter()
                        .map(Step::describe)
                        .collect(),
                },
            }),
            steps: reports,
        }
    }

    async fn authorize(
        &self,
        gate: &dyn ToolGate,
        tool: &str,
        category: PermissionCategory,
        effect: Effect,
        input: Value,
    ) -> Result<(), Stop> {
        let req = GateRequest {
            run_id: self.opts.run_id.clone(),
            tool: tool.to_owned(),
            category,
            effect,
            raw_command: raw_command_for(tool, &input),
            input,
        };
        gate.authorize(&req).await.map_err(Stop::Denied)
    }

    /// Ask the desktop control gate (if any) before a mutating action.
    async fn control_gate(&self, action: &str, label: String) -> Result<(), Stop> {
        let Some(gate) = &self.opts.control else {
            return Err(Stop::Denied(format!(
                "no desktop control gate configured: refusing {action}"
            )));
        };
        gate.before_action(&ControlRequest {
            run_id: self.opts.run_id.clone(),
            rect: None,
            label,
            action: action.to_owned(),
        })
        .await
        .map_err(Stop::Denied)
    }

    async fn run_step(
        &self,
        step: &Step,
        vars: &Vars,
        types: &BTreeMap<String, crate::format::ParamType>,
        gate: &dyn ToolGate,
    ) -> Result<Done, Stop> {
        match step {
            Step::DesktopAction(d) => self.desktop_action(d, vars, gate).await,
            Step::BrowserAction(b) => self.browser_action(b, vars, gate).await,
            Step::Navigate(n) => {
                let driver = self.browser()?;
                let url = template::render(&n.url, vars).map_err(|e| tpl(&e))?;
                self.authorize(
                    gate,
                    "browser_open",
                    PermissionCategory::Browser,
                    Effect::Write,
                    json!({"url": url, "profile": n.profile}),
                )
                .await?;
                self.control_gate("browser_open", url.clone()).await?;
                let out = driver
                    .open(&url, n.profile.as_deref())
                    .await
                    .map_err(Stop::Failed)?;
                Ok(Done {
                    output: Some(truncate(&out, 2000)),
                    ..Done::default()
                })
            }
            Step::Launch(l) => {
                let backend = self.desktop()?;
                let executable = template::render(&l.executable, vars).map_err(|e| tpl(&e))?;
                let args = l
                    .args
                    .iter()
                    .map(|a| template::render(a, vars))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| tpl(&e))?;
                self.authorize(
                    gate,
                    "desktop_launch",
                    PermissionCategory::Desktop,
                    Effect::Write,
                    json!({"executable": executable, "args": args}),
                )
                .await?;
                self.control_gate("launch", executable.clone()).await?;
                let pid = backend
                    .launch(AppSpec { executable, args })
                    .await
                    .map_err(|e| Stop::Failed(e.to_string()))?;
                sleep(self.opts.launch_settle).await;
                Ok(Done {
                    output: Some(format!("launched pid={pid}")),
                    ..Done::default()
                })
            }
            Step::WaitFor(w) => self.wait_for(w, gate).await,
            Step::Shell(s) => {
                let dialect = template::ShellDialect::from_name(s.shell.as_deref());
                let mut input = serde_json::Map::new();
                input.insert(
                    "command".into(),
                    Value::String(
                        template::render_shell(&s.command, vars, dialect).map_err(|e| tpl(&e))?,
                    ),
                );
                if let Some(sh) = &s.shell {
                    input.insert("shell".into(), Value::String(sh.clone()));
                }
                // `cwd` is a path handed to the OS, never parsed by a shell.
                if let Some(c) = &s.cwd {
                    input.insert(
                        "cwd".into(),
                        Value::String(template::render(c, vars).map_err(|e| tpl(&e))?),
                    );
                }
                self.call_tool("shell_run", Value::Object(input), gate)
                    .await
            }
            Step::Tool(t) => {
                let input = if t.tool == "shell_run"
                    && let Value::Object(obj) = &t.input
                    && let Some(Value::String(cmd)) = obj.get("command")
                {
                    // A command line: quote substituted values for the target shell.
                    let dialect =
                        template::ShellDialect::from_name(obj.get("shell").and_then(Value::as_str));
                    let mut rest = obj.clone();
                    rest.remove("command");
                    let Value::Object(mut rendered) =
                        template::render_value(&Value::Object(rest), vars, types)
                            .map_err(|e| tpl(&e))?
                    else {
                        unreachable!("object renders to object")
                    };
                    rendered.insert(
                        "command".into(),
                        Value::String(
                            template::render_shell(cmd, vars, dialect).map_err(|e| tpl(&e))?,
                        ),
                    );
                    Value::Object(rendered)
                } else {
                    template::render_value(&t.input, vars, types).map_err(|e| tpl(&e))?
                };
                self.call_tool(&t.tool, input, gate).await
            }
            Step::Agent(a) => Err(Stop::Agent(
                template::render(&a.instruction, vars).map_err(|e| tpl(&e))?,
            )),
        }
    }

    fn desktop(&self) -> Result<&Arc<dyn DesktopBackend>, Stop> {
        self.handles
            .desktop
            .as_ref()
            .ok_or_else(|| Stop::Missing("no desktop backend".into()))
    }

    fn browser(&self) -> Result<&Arc<dyn BrowserDriver>, Stop> {
        self.handles
            .browser
            .as_ref()
            .ok_or_else(|| Stop::Missing("no browser driver".into()))
    }

    async fn call_tool(&self, name: &str, input: Value, gate: &dyn ToolGate) -> Result<Done, Stop> {
        let tool = self
            .handles
            .tools
            .get(name)
            .or_else(|| self.handles.tools.get(&name.replace("__", ".")))
            .ok_or_else(|| Stop::Missing(format!("tool `{name}` is not registered")))?;
        let spec = tool.spec();
        // Gate on the resolved tool's real name (`mcp_srv__x` may alias
        // `mcp_srv.x`), so profile globs and "always" grants match.
        self.authorize(gate, &spec.name, spec.category, spec.effect, input.clone())
            .await?;
        let ctx = ToolContext {
            run_id: self.opts.run_id.clone(),
            bot_id: self.opts.bot_id.clone(),
            ..ToolContext::default()
        };
        match tool.call(&ctx, input).await {
            Ok(out) => {
                let text: String = out
                    .content
                    .iter()
                    .map(|p| match p {
                        ContentPart::Text { text } => text.clone(),
                        ContentPart::Json { data } => data.to_string(),
                        ContentPart::Binary { mime_type, .. } => format!("[{mime_type}]"),
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                Ok(Done {
                    output: Some(truncate(&text, 2000)),
                    ..Done::default()
                })
            }
            Err(ToolError::PermissionDenied(m)) => Err(Stop::Denied(m)),
            Err(e) => Err(Stop::Failed(e.to_string())),
        }
    }

    // ── desktop ─────────────────────────────────────────────────────────────

    /// The window the pattern selects: `Ok(None)` without a pattern (focused
    /// window), an error for zero or several matches.
    async fn find_window(
        &self,
        backend: &Arc<dyn DesktopBackend>,
        pattern: Option<&str>,
    ) -> Result<Option<u64>, Drift> {
        let Some(p) = pattern else { return Ok(None) };
        let wins = backend.windows().await.map_err(|_| Drift::NotFound {
            tried: vec!["window list"],
        })?;
        let hits: Vec<u64> = wins
            .iter()
            .filter(|w| glob_match(p, &w.title))
            .map(|w| w.id)
            .collect();
        match hits.as_slice() {
            [] => Err(Drift::NotFound {
                tried: vec!["window title pattern"],
            }),
            [one] => Ok(Some(*one)),
            many => Err(Drift::Ambiguous {
                strategy: Strategy::RoleNameExact,
                count: many.len(),
            }),
        }
    }

    async fn desktop_action(
        &self,
        d: &DesktopAction,
        vars: &Vars,
        gate: &dyn ToolGate,
    ) -> Result<Done, Stop> {
        let backend = self.desktop()?;
        let value = d
            .value
            .as_ref()
            .map(|v| template::render(v, vars))
            .transpose()
            .map_err(|e| tpl(&e))?;
        let deadline = Instant::now() + self.opts.resolve_timeout;
        let pattern = d.window_title_pattern.as_deref();

        // Resolve (with a short wait for the screen to catch up).
        let (snap, found): (Option<Snapshot>, Option<Found>) = loop {
            let attempt: Result<(Option<Snapshot>, Option<Found>), Drift> = async {
                let window = self.find_window(backend, pattern).await?;
                let Some(loc) = &d.target else {
                    return Ok((None, None));
                };
                let snap = backend
                    .snapshot(window)
                    .await
                    .map_err(|_| Drift::NotFound {
                        tried: vec!["snapshot"],
                    })?;
                let found = resolve_desktop(loc, &snap, self.opts.strict_fingerprint)?;
                Ok((Some(snap), Some(found)))
            }
            .await;
            match attempt {
                Ok(v) => break v,
                Err(Drift::NotFound { .. }) if Instant::now() < deadline => {
                    sleep(self.opts.poll_interval).await;
                }
                Err(drift) => return Err(drift_stop(&drift)),
            }
        };

        let target = match (&snap, &found) {
            (Some(s), Some(f)) => Some(Target::ByIndex {
                index: s.nodes[f.node].index,
                generation: s.generation,
            }),
            _ => None,
        };
        let index_json = match &target {
            Some(Target::ByIndex { index, .. }) => json!(index),
            _ => Value::Null,
        };
        let method = match d.method.as_deref() {
            None | Some("accessibility") => ClickMethod::Accessibility,
            Some("app_post") => ClickMethod::AppPost,
            Some("global") => ClickMethod::Global,
            Some(other) => return Err(Stop::Failed(format!("unknown click method `{other}`"))),
        };
        let (tool, input) = match d.action {
            DesktopActionKind::Click => ("desktop_click", json!({"index": index_json})),
            DesktopActionKind::Type => {
                ("desktop_type", json!({"index": index_json, "text": value}))
            }
            DesktopActionKind::SetValue => (
                "desktop_set_value",
                json!({"index": index_json, "value": value}),
            ),
            DesktopActionKind::Key => ("desktop_key", json!({"keys": value, "index": index_json})),
        };
        self.authorize(
            gate,
            tool,
            PermissionCategory::Desktop,
            Effect::Write,
            input,
        )
        .await?;
        self.control_gate(
            match d.action {
                DesktopActionKind::Click => "click",
                DesktopActionKind::Type => "type",
                DesktopActionKind::SetValue => "set_value",
                DesktopActionKind::Key => "key",
            },
            tool.to_owned(),
        )
        .await?;

        match d.action {
            DesktopActionKind::Click => {
                let t = target.ok_or_else(|| Stop::Failed("click without target".into()))?;
                dispatch(backend, Action::Click { target: t, method }).await?;
            }
            DesktopActionKind::Type => {
                let t = target.ok_or_else(|| Stop::Failed("type without target".into()))?;
                // Same as `desktop_type`: a real click focuses, then keys go to the focus.
                dispatch(
                    backend,
                    Action::Click {
                        target: t,
                        method: ClickMethod::Global,
                    },
                )
                .await?;
                dispatch(
                    backend,
                    Action::TypeText {
                        text: value.unwrap_or_default(),
                    },
                )
                .await?;
            }
            DesktopActionKind::SetValue => {
                let t = target.ok_or_else(|| Stop::Failed("set_value without target".into()))?;
                dispatch(
                    backend,
                    Action::SetValue {
                        target: t,
                        value: value.unwrap_or_default(),
                    },
                )
                .await?;
            }
            DesktopActionKind::Key => {
                if let Some(t) = target {
                    dispatch(
                        backend,
                        Action::Click {
                            target: t,
                            method: ClickMethod::Global,
                        },
                    )
                    .await?;
                }
                let keys: Vec<String> = value
                    .unwrap_or_default()
                    .split('+')
                    .map(|k| k.trim().to_owned())
                    .filter(|k| !k.is_empty())
                    .collect();
                if keys.is_empty() {
                    return Err(Stop::Failed("empty key chord".into()));
                }
                dispatch(backend, Action::KeyPress { keys }).await?;
            }
        }
        sleep(self.opts.action_settle).await;
        Ok(Done {
            resolved_via: found.as_ref().map(|f| f.strategy),
            fingerprint_changed: found.as_ref().is_some_and(|f| f.fingerprint_changed),
            output: None,
        })
    }

    // ── browser ─────────────────────────────────────────────────────────────

    async fn browser_action(
        &self,
        b: &BrowserAction,
        vars: &Vars,
        gate: &dyn ToolGate,
    ) -> Result<Done, Stop> {
        let driver = self.browser()?;
        let value = b
            .value
            .as_ref()
            .map(|v| template::render(v, vars))
            .transpose()
            .map_err(|e| tpl(&e))?;
        let deadline = Instant::now() + self.opts.resolve_timeout;

        let hit = loop {
            let attempt: Result<Hit, Drift> = async {
                if b.target.is_none() && b.url_pattern.is_none() {
                    return Ok(Hit::None);
                }
                let text = driver.snapshot(false).await.map_err(|_| Drift::NotFound {
                    tried: vec!["snapshot"],
                })?;
                let page = parse_browser_snapshot(&text);
                if let (Some(p), Some(url)) = (&b.url_pattern, &page.url)
                    && !glob_match(p, url)
                {
                    return Err(Drift::NotFound {
                        tried: vec!["page url does not match the recorded pattern"],
                    });
                }
                let Some(loc) = &b.target else {
                    return Ok(Hit::None);
                };
                match resolve_browser(loc, &page.nodes, self.opts.strict_fingerprint) {
                    Ok(f) => Ok(Hit::Ref(page.nodes[f.node].ref_id.clone(), f)),
                    Err(Drift::NotFound { tried }) => match &loc.css {
                        Some(css) => self
                            .css_unique(driver, css)
                            .await
                            .map(|()| Hit::Css(css.clone())),
                        None => Err(Drift::NotFound { tried }),
                    },
                    Err(e) => Err(e),
                }
            }
            .await;
            match attempt {
                Ok(h) => break h,
                Err(Drift::NotFound { .. }) if Instant::now() < deadline => {
                    sleep(self.opts.poll_interval).await;
                }
                Err(drift) => return Err(drift_stop(&drift)),
            }
        };

        let (via, changed) = match &hit {
            Hit::Ref(_, f) => (Some(f.strategy), f.fingerprint_changed),
            Hit::Css(_) => (Some(Strategy::Css), false),
            Hit::None => (None, false),
        };
        let r = match &hit {
            Hit::Ref(r, _) => Some(r.clone()),
            _ => None,
        };

        let out = if let Hit::Css(css) = &hit {
            let js = css_action_js(css, b.action, value.as_deref());
            self.authorize(
                gate,
                "browser_eval",
                PermissionCategory::Script,
                Effect::Write,
                json!({"js": js}),
            )
            .await?;
            self.control_gate("browser_eval", css.clone()).await?;
            driver
                .eval(&js)
                .await
                .map(|v| v.to_string())
                .map_err(Stop::Failed)?
        } else {
            let (tool, input) = match b.action {
                BrowserActionKind::Click => ("browser_click", json!({"ref": r})),
                BrowserActionKind::Fill => ("browser_fill", json!({"ref": r, "text": value})),
                BrowserActionKind::Select => ("browser_select", json!({"ref": r, "value": value})),
                BrowserActionKind::Press => ("browser_press", json!({"key": value})),
                BrowserActionKind::Back => ("browser_back", json!({})),
            };
            self.authorize(
                gate,
                tool,
                PermissionCategory::Browser,
                Effect::Write,
                input,
            )
            .await?;
            self.control_gate(tool, tool.to_owned()).await?;
            let need = || Stop::Failed("browser step has no target".into());
            match b.action {
                BrowserActionKind::Click => driver.click(&r.ok_or_else(need)?).await,
                BrowserActionKind::Fill => {
                    driver
                        .fill(&r.ok_or_else(need)?, value.as_deref().unwrap_or_default())
                        .await
                }
                BrowserActionKind::Select => {
                    driver
                        .select(&r.ok_or_else(need)?, value.as_deref().unwrap_or_default())
                        .await
                }
                BrowserActionKind::Press => {
                    driver.press(value.as_deref().unwrap_or_default()).await
                }
                BrowserActionKind::Back => driver.back().await,
            }
            .map_err(Stop::Failed)?
        };
        sleep(self.opts.action_settle).await;
        Ok(Done {
            resolved_via: via,
            fingerprint_changed: changed,
            output: Some(truncate(&out, 2000)),
        })
    }

    /// The css selector must match exactly one element.
    async fn css_unique(&self, driver: &Arc<dyn BrowserDriver>, css: &str) -> Result<(), Drift> {
        let js = format!(
            "document.querySelectorAll({}).length",
            serde_json::to_string(css).unwrap_or_default()
        );
        let n = driver
            .eval(&js)
            .await
            .ok()
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        match n {
            0 => Err(Drift::NotFound { tried: vec!["css"] }),
            1 => Ok(()),
            count => Err(Drift::Ambiguous {
                strategy: Strategy::Css,
                count: usize::try_from(count).unwrap_or(usize::MAX),
            }),
        }
    }

    // ── wait / success check ────────────────────────────────────────────────

    async fn wait_for(&self, w: &WaitFor, gate: &dyn ToolGate) -> Result<Done, Stop> {
        let timeout = Duration::from_secs(w.timeout_secs.unwrap_or(15));
        if let Some(p) = &w.window_title_pattern {
            let backend = self.desktop()?;
            self.authorize(
                gate,
                "desktop_list_apps",
                PermissionCategory::Desktop,
                Effect::Read,
                json!({}),
            )
            .await?;
            let deadline = Instant::now() + timeout;
            loop {
                let wins = backend
                    .windows()
                    .await
                    .map_err(|e| Stop::Failed(e.to_string()))?;
                if wins.iter().any(|x| glob_match(p, &x.title)) {
                    return Ok(Done::default());
                }
                if Instant::now() >= deadline {
                    return Err(Stop::Drift(format!("window {p:?} did not appear")));
                }
                sleep(self.opts.poll_interval).await;
            }
        }
        let driver = self.browser()?;
        let cond = if let Some(t) = &w.text {
            Wait::Text(t.clone())
        } else if let Some(s) = &w.selector {
            Wait::Selector(s.clone())
        } else {
            Wait::Idle
        };
        self.authorize(
            gate,
            "browser_wait",
            PermissionCategory::Browser,
            Effect::Read,
            json!({}),
        )
        .await?;
        driver
            .wait(cond, Some(timeout))
            .await
            .map(|_| Done::default())
            .map_err(|e| Stop::Drift(format!("wait condition not met: {e}")))
    }

    async fn check(&self, check: &SuccessCheck, vars: &Vars) -> Result<(), String> {
        match check {
            SuccessCheck::WindowTitle { pattern } => {
                let backend = self
                    .desktop()
                    .map_err(|_| "no desktop backend".to_owned())?;
                let wins = backend.windows().await.map_err(|e| e.to_string())?;
                if wins.iter().any(|w| glob_match(pattern, &w.title)) {
                    Ok(())
                } else {
                    Err(format!("no window matches {pattern:?}"))
                }
            }
            SuccessCheck::DesktopText {
                window_title_pattern,
                contains,
            } => {
                let backend = self
                    .desktop()
                    .map_err(|_| "no desktop backend".to_owned())?;
                let needle = template::render(contains, vars).map_err(|e| e.to_string())?;
                let window = self
                    .find_window(backend, window_title_pattern.as_deref())
                    .await
                    .map_err(|d| d.to_string())?;
                let snap = backend.snapshot(window).await.map_err(|e| e.to_string())?;
                let hit = snap.nodes.iter().any(|n| {
                    n.name.contains(&needle)
                        || n.value.as_deref().is_some_and(|v| v.contains(&needle))
                });
                if hit {
                    Ok(())
                } else {
                    Err(format!("window does not show {needle:?}"))
                }
            }
            SuccessCheck::BrowserText { contains } => {
                let driver = self.browser().map_err(|_| "no browser driver".to_owned())?;
                let needle = template::render(contains, vars).map_err(|e| e.to_string())?;
                let text = driver.snapshot(false).await?;
                if text.contains(&needle) {
                    Ok(())
                } else {
                    Err(format!("page does not show {needle:?}"))
                }
            }
        }
    }
}

async fn dispatch(backend: &Arc<dyn DesktopBackend>, action: Action) -> Result<(), Stop> {
    let r = backend
        .act(action)
        .await
        .map_err(|e| Stop::Failed(e.to_string()))?;
    if r.dispatched {
        Ok(())
    } else {
        Err(Stop::Failed("the action was not dispatched".into()))
    }
}

fn css_action_js(css: &str, action: BrowserActionKind, value: Option<&str>) -> String {
    let sel = serde_json::to_string(css).unwrap_or_default();
    let val = serde_json::to_string(value.unwrap_or_default()).unwrap_or_default();
    let body = match action {
        BrowserActionKind::Click => "e.click(); return 'clicked';".to_owned(),
        BrowserActionKind::Fill | BrowserActionKind::Select => format!(
            "e.value = {val}; e.dispatchEvent(new Event('input', {{bubbles:true}})); e.dispatchEvent(new Event('change', {{bubbles:true}})); return 'set';"
        ),
        BrowserActionKind::Press | BrowserActionKind::Back => "return 'unsupported';".to_owned(),
    };
    format!(
        "(() => {{ const e = document.querySelector({sel}); if (!e) return 'missing'; {body} }})()"
    )
}

async fn sleep(d: Duration) {
    if !d.is_zero() {
        tokio::time::sleep(d).await;
    }
}

fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

enum Hit {
    None,
    Ref(String, Found),
    Css(String),
}
