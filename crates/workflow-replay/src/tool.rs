//! Agent tools: `workflow_run` and `workflow_list`.
//!
//! `workflow_run` is registered like a builtin tool (`Write` effect, category
//! `Script`: it is a composite that drives desktop, browser and tools). It does
//! not widen anything: every inner step still passes through the [`ToolGate`]
//! it was built with (the agent loop's approval flow), exactly like a script.
//!
//! Registering (the registry must be the one `Handles::tools` points to, so
//! `tool` / `shell` steps can find their tools):
//!
//! ```ignore
//! let agent = agent.with_gated_tools(|gate| {
//!     workflow_replay::tools(store, handles, gate, ReplayOptions::default())
//! });
//! ```

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;

use async_trait::async_trait;
use builtin_tools::ToolGate;
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{Value, json};
use tool_registry::{
    Effect, PermissionCategory, Platform, Tool, ToolContext, ToolError, ToolOutput, ToolSpec,
};

use crate::error::WorkflowError;
use crate::replay::{
    Handles, Handover, HandoverReason, MAX_WORKFLOW_DEPTH, ReplayOptions, ReplayReport, Replayer,
};
use crate::store::WorkflowStore;

/// Name of the run tool.
pub const RUN_TOOL_NAME: &str = "workflow_run";
/// Name of the list tool.
pub const LIST_TOOL_NAME: &str = "workflow_list";

fn spec(
    name: &str,
    description: &str,
    schema: Value,
    effect: Effect,
    category: PermissionCategory,
) -> ToolSpec {
    ToolSpec {
        name: name.to_owned(),
        description: description.to_owned(),
        input_schema: schema,
        effect,
        category,
        platforms: vec![Platform::All],
    }
}

/// Both tools, ready to register.
#[must_use]
pub fn tools(
    store: WorkflowStore,
    handles: Handles,
    gate: Arc<dyn ToolGate>,
    opts: ReplayOptions,
) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(WorkflowRun {
            spec: spec(
                RUN_TOOL_NAME,
                "Replay a saved workflow (a recorded, parameterised sequence of desktop/browser/tool steps) without calling a model. If the screen differs from the recording it stops and reports the step to continue from. Use workflow_list to see saved workflows. Example: {\"name\":\"notepad_note\",\"params\":{\"text\":\"hello\"}}",
                json!({"type":"object","properties":{
                    "name":{"type":"string","description":"Workflow name from workflow_list."},
                    "params":{"type":"object","description":"Parameter values by name; missing ones use the workflow's defaults."}
                },"required":["name"]}),
                Effect::Write,
                PermissionCategory::Script,
            ),
            store: store.clone(),
            handles,
            gate,
            opts,
            active: Arc::default(),
        }),
        Arc::new(WorkflowList {
            spec: spec(
                LIST_TOOL_NAME,
                "List saved workflows with their parameters. Example: {}",
                json!({"type":"object","properties":{}}),
                Effect::Read,
                PermissionCategory::Meta,
            ),
            store,
        }),
    ]
}

struct WorkflowList {
    spec: ToolSpec,
    store: WorkflowStore,
}

#[async_trait]
impl Tool for WorkflowList {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, _ctx: &ToolContext, _input: Value) -> Result<ToolOutput, ToolError> {
        let list = self
            .store
            .list()
            .map_err(|e| ToolError::Execution(e.to_string()))?;
        if list.is_empty() {
            return Ok(ToolOutput::text("no saved workflows"));
        }
        let mut out = String::new();
        for e in list {
            let _ = writeln!(
                out,
                "{} - {} ({} steps; params: {})",
                e.name,
                e.description,
                e.steps,
                if e.params.is_empty() {
                    "none".to_owned()
                } else {
                    e.params.join(", ")
                }
            );
        }
        Ok(ToolOutput::text(out))
    }
}

#[derive(Deserialize)]
struct RunInput {
    name: String,
    #[serde(default)]
    params: serde_json::Map<String, Value>,
}

struct WorkflowRun {
    spec: ToolSpec,
    store: WorkflowStore,
    handles: Handles,
    gate: Arc<dyn ToolGate>,
    opts: ReplayOptions,
    /// Workflows being replayed per run (outermost first). Keyed by run id
    /// rather than a task-local: a workflow can reach itself again through
    /// `script_run`, whose engine runs on another thread.
    active: Arc<Mutex<HashMap<String, Vec<String>>>>,
}

/// Pops a workflow off the run's active stack on drop.
struct ActiveGuard {
    active: Arc<Mutex<HashMap<String, Vec<String>>>>,
    run_id: String,
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        let mut map = self.active.lock();
        if let Some(stack) = map.get_mut(&self.run_id) {
            stack.pop();
            if stack.is_empty() {
                map.remove(&self.run_id);
            }
        }
    }
}

impl WorkflowRun {
    fn enter(&self, run_id: &str, name: &str) -> Result<ActiveGuard, WorkflowError> {
        let mut map = self.active.lock();
        let stack = map.entry(run_id.to_owned()).or_default();
        let err = if stack.iter().any(|n| n == name) {
            Some(format!(
                "workflow `{name}` would call itself (active: {})",
                stack.join(" -> ")
            ))
        } else if stack.len() >= MAX_WORKFLOW_DEPTH {
            Some(format!(
                "workflows nested deeper than {MAX_WORKFLOW_DEPTH} levels"
            ))
        } else {
            None
        };
        if let Some(e) = err {
            if stack.is_empty() {
                map.remove(run_id);
            }
            return Err(WorkflowError::Invalid(e));
        }
        stack.push(name.to_owned());
        Ok(ActiveGuard {
            active: self.active.clone(),
            run_id: run_id.to_owned(),
        })
    }
}

#[async_trait]
impl Tool for WorkflowRun {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolContext, input: Value) -> Result<ToolOutput, ToolError> {
        let i: RunInput =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let wf = self
            .store
            .get(&i.name)
            .map_err(|e| ToolError::Execution(e.to_string()))?;
        let _guard = self
            .enter(&ctx.run_id, &wf.name)
            .map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let mut opts = self.opts.clone();
        opts.run_id.clone_from(&ctx.run_id);
        opts.bot_id.clone_from(&ctx.bot_id);
        let report = Replayer::new(self.handles.clone(), opts)
            .replay(&wf, &i.params, self.gate.as_ref())
            .await
            .map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        if let Some(h) = report.handover()
            && let HandoverReason::PermissionDenied { detail } = &h.reason
        {
            return Err(ToolError::PermissionDenied(format!(
                "workflow `{}` stopped at step {}: {detail}",
                report.workflow, h.step_index
            )));
        }
        Ok(ToolOutput::text(summarize(&report)))
    }
}

/// Text the agent reads after a replay.
#[must_use]
pub fn summarize(r: &ReplayReport) -> String {
    match r.handover() {
        None => format!(
            "Workflow `{}` completed: {}/{} steps, {} model calls, {} ms.",
            r.workflow, r.steps_replayed, r.steps_total, r.llm_calls, r.wall_time_ms
        ),
        Some(h) => summarize_handover(r, h),
    }
}

fn summarize_handover(r: &ReplayReport, h: &Handover) -> String {
    let mut out = format!(
        "Workflow `{}` HANDOVER at step {} ({}): {}\n{} of {} steps were done before it stopped ({} ms, {} model calls).\n",
        r.workflow,
        h.step_index,
        h.reason.as_str(),
        h.reason.detail(),
        h.partial_state.completed,
        r.steps_total,
        r.wall_time_ms,
        r.llm_calls,
    );
    if let Some(o) = &h.partial_state.last_output {
        let _ = writeln!(out, "Last step output: {o}");
    }
    out.push_str(
        "Steps not yet done (continue from the first one yourself; check the screen first):\n",
    );
    for (k, s) in h.partial_state.remaining.iter().enumerate() {
        let _ = writeln!(out, "  {}. {s}", h.step_index + k);
    }
    out
}
