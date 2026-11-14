//! Subagents: the `spawn_subagent` tool and the child runs behind it.
//!
//! A subagent is a *child run* of the same bot (`runs.parent_run_id`).  It
//! starts from a fresh history (its task plus a short parent-provided
//! context), sees the intersection of the parent's tools and the requested
//! ones, has its own step / token guards, and answers to the parent's
//! permissions, grants and approval flow.  The parent's tool step waits for
//! the child and receives its final text.
//!
//! The call itself is executed by the loop (not by the registered tool body),
//! because it needs the run it belongs to and the model call id; the tool
//! registered here only carries the spec the model sees.
//!
//! Crash recovery: children are ordinary runs and come back `Paused`.  The
//! parent's `spawn_subagent` step is idempotent, so resuming the parent
//! replays it; the replay finds the already persisted children by their
//! spawn key (`<tool step number>:<model call id>`), resumes the paused ones
//! and waits for them instead of spawning new ones.

use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use protocol::event::{SubagentFinished, SubagentStarted};
use protocol::{Event, InitiatorKind, RunId, ToolCallId};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use storage::Run;
use task_runtime::{RunState, Runtime};
use tool_registry::{
    Effect, PermissionCategory, Platform, Tool, ToolContext, ToolError, ToolOutput, ToolSpec,
};

use crate::agent::{AgentLoop, Start};
use crate::exec::TurnExecutor;
use crate::history::{step_output, truncate_utf8};
use crate::profile::BotProfile;
use crate::shared::{ModelOut, RunCtx, STEP_MODEL, Shared, kind_for};

/// Name of the tool.
pub(crate) const SPAWN_SUBAGENT: &str = "spawn_subagent";
/// A run at this depth may not spawn (top-level = 0, child = 1, grandchild = 2).
pub(crate) const MAX_DEPTH: u32 = 2;
/// Children one run may have running at once.
pub(crate) const MAX_CHILDREN: usize = 3;
/// `max_steps` of a child that does not ask for another value.
pub(crate) const DEFAULT_MAX_STEPS: u32 = 15;
/// Largest child answer handed to the parent, in bytes.
pub(crate) const RESULT_CAP_BYTES: usize = 8 * 1024;
/// Largest `summary` of a `subagent_finished` event, in bytes.
const EVENT_SUMMARY_BYTES: usize = 2 * 1024;
/// Largest parent-provided context, in bytes.
const CONTEXT_CAP_BYTES: usize = 2 * 1024;
const POLL: Duration = Duration::from_millis(20);

/// Spawn parameters of a child run, persisted in `runs.spec_json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SubSpec {
    pub task: String,
    #[serde(default)]
    pub context: String,
    /// Tool names / globs the child is limited to (`None` = the parent's tools).
    #[serde(default)]
    pub tools: Option<Vec<String>>,
    /// `executor` (default), `planner` or `classifier`.
    #[serde(default)]
    pub model_role: Option<String>,
    pub max_steps: u32,
    /// Token budget of the child's whole run.
    pub token_budget: u64,
    /// 1 for a child of a top-level run, 2 for a grandchild.
    pub depth: u32,
    /// Idempotency key of the spawning tool call.
    pub spawn_key: String,
}

impl SubSpec {
    /// The model the child runs on: the bot's model for the requested role.
    pub fn model_for(&self, parent: &BotProfile) -> Result<String, String> {
        let m = &parent.models;
        match self.model_role.as_deref().unwrap_or("executor") {
            "executor" => Ok(m.executor.clone()),
            "planner" => Ok(m.planner.clone().unwrap_or_else(|| m.executor.clone())),
            "classifier" => Ok(m.classifier.clone().unwrap_or_else(|| m.executor.clone())),
            other => Err(format!(
                "unknown model_role {other:?} (use executor, planner or classifier)"
            )),
        }
    }

    /// The child's first (and only initial) user message.
    pub fn first_message(&self) -> String {
        if self.context.trim().is_empty() {
            self.task.clone()
        } else {
            format!(
                "{}\n\nContext from the parent agent:\n{}",
                self.task, self.context
            )
        }
    }
}

/// Validated arguments of one `spawn_subagent` call.
struct SpawnArgs {
    task: String,
    context: String,
    tools: Option<Vec<String>>,
    model_role: Option<String>,
    max_steps: u32,
}

impl SpawnArgs {
    fn parse(input: &Value, max_steps_cap: u32) -> Result<Self, String> {
        let task = input
            .get("task")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .ok_or("`task` (non-empty string) is required")?
            .to_owned();
        let tools = match input.get("tools") {
            None | Some(Value::Null) => None,
            Some(Value::Array(a)) => Some(
                a.iter()
                    .map(|v| {
                        v.as_str()
                            .map(str::to_owned)
                            .ok_or("`tools` must be an array of tool names")
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            Some(_) => return Err("`tools` must be an array of tool names".into()),
        };
        let model_role = match input.get("model_role") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => Some(s.clone()),
            Some(_) => return Err("`model_role` must be a string".into()),
        };
        let max_steps = match input.get("max_steps") {
            None | Some(Value::Null) => DEFAULT_MAX_STEPS,
            Some(v) => v
                .as_u64()
                .filter(|n| *n >= 1)
                .map(|n| u32::try_from(n).unwrap_or(u32::MAX))
                .ok_or("`max_steps` must be a positive integer")?,
        }
        .min(max_steps_cap.max(1));
        let context = input
            .get("context")
            .and_then(Value::as_str)
            .map(|c| truncate_utf8(c, CONTEXT_CAP_BYTES).to_owned())
            .unwrap_or_default();
        Ok(Self {
            task,
            context,
            tools,
            model_role,
            max_steps,
        })
    }
}

struct SpawnTool {
    spec: ToolSpec,
}

/// The `spawn_subagent` tool (spec only; the loop executes the call).
pub(crate) fn tool() -> Arc<dyn Tool> {
    Arc::new(SpawnTool {
        spec: ToolSpec {
            name: SPAWN_SUBAGENT.to_owned(),
            description: "Delegate a self-contained task to a subagent: a fresh run of this bot \
                with its own history and step budget. Use it for side investigations or to work \
                on independent parts in parallel (up to 3 at once, call several in one turn). \
                The subagent only knows the task and the short context you give, so be specific. \
                Returns its final answer. Example: {\"task\":\"Find the three largest files in \
                the project\",\"tools\":[\"fs.*\"],\"max_steps\":10}"
                .to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "task": {"type": "string", "description": "What the subagent must do and return."},
                    "context": {"type": "string", "description": "Short facts it needs (at most 2 KB)."},
                    "tools": {"type": "array", "items": {"type": "string"},
                        "description": "Limit the subagent to these tools (names or globs). Default: all of yours."},
                    "model_role": {"type": "string", "enum": ["executor", "planner", "classifier"],
                        "description": "Which of the bot's models to use. Default: executor."},
                    "max_steps": {"type": "integer", "minimum": 1,
                        "description": "Model-call limit for the subagent (default 15)."}
                },
                "required": ["task"]
            }),
            effect: Effect::Read,
            category: PermissionCategory::Meta,
            platforms: vec![Platform::All],
        },
    })
}

#[async_trait]
impl Tool for SpawnTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, _ctx: &ToolContext, _input: Value) -> Result<ToolOutput, ToolError> {
        Err(ToolError::Execution(
            "spawn_subagent can only be run by the agent loop".into(),
        ))
    }
}

fn spawn_key_of(run: &Run) -> Option<String> {
    run.spec
        .as_ref()
        .and_then(|s| s["spawn_key"].as_str())
        .map(str::to_owned)
}

fn is_live_state(run: &Run) -> bool {
    matches!(
        RunState::parse(&run.state),
        Some(RunState::Queued | RunState::Running | RunState::WaitingApproval)
    )
}

impl TurnExecutor {
    /// Run one `spawn_subagent` call: create (or, after a restart, adopt) the
    /// child, wait for it and return its answer as the tool result.
    pub(crate) async fn run_subagent(
        &self,
        run: &Run,
        ctx: &RunCtx,
        call: ToolCallId,
        model_call_id: &str,
        input: Value,
    ) -> (String, bool, Option<u64>) {
        let started = Instant::now();
        let (text, is_error) = match self
            .spawn_and_wait(run, ctx, call, model_call_id, &input)
            .await
        {
            Ok(r) => r,
            Err(e) => (format!("Error: {e}"), true),
        };
        let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        (text, is_error, Some(ms))
    }

    async fn spawn_and_wait(
        &self,
        run: &Run,
        ctx: &RunCtx,
        call: ToolCallId,
        model_call_id: &str,
        input: &Value,
    ) -> Result<(String, bool), String> {
        let sh = &self.sh;
        let rt: Arc<Runtime> = sh
            .rt
            .get()
            .and_then(Weak::upgrade)
            .ok_or("subagents are unavailable")?;
        let lp = AgentLoop::from_parts(Arc::clone(sh), Arc::clone(&rt));
        if ctx.depth() >= MAX_DEPTH {
            return Err(format!(
                "subagent depth limit reached: a subagent at depth {MAX_DEPTH} cannot spawn \
                 another; do the work yourself"
            ));
        }
        let args = SpawnArgs::parse(input, sh.cfg.guards.max_steps)?;
        // Validate role and tool globs before anything is persisted.
        let probe = SubSpec {
            task: args.task.clone(),
            context: args.context.clone(),
            tools: args.tools.clone(),
            model_role: args.model_role.clone(),
            max_steps: args.max_steps,
            token_budget: 0,
            depth: ctx.depth() + 1,
            spawn_key: String::new(),
        };
        let model = probe.model_for(&ctx.profile)?;
        ctx.profile.narrowed(probe.tools.as_deref(), Some(&model))?;

        // Same step number on a replay of this tool step, so the key is stable.
        let step_no = sh
            .storage
            .list_steps(run.id)
            .map_err(|e| e.to_string())?
            .len();
        let key = format!("{step_no}:{model_call_id}");
        let (child, adopted) = self.create_or_find_child(run, ctx, &rt, probe, key)?;

        let child_run = sh
            .storage
            .get_run(child)
            .map_err(|e| e.to_string())?
            .ok_or("subagent run vanished")?;
        let task = child_run
            .spec
            .as_ref()
            .and_then(|s| s["task"].as_str())
            .unwrap_or(&args.task)
            .to_owned();
        sh.emit(
            ctx,
            Event::SubagentStarted(SubagentStarted {
                run_id: child,
                parent_run_id: run.id,
                task,
                call_id: Some(call),
            }),
        );
        let live = rt.active_runs().contains(&child);
        match RunState::parse(&child_run.state) {
            Some(RunState::Paused) if !live => {
                // A resume racing with another resumer is fine: both then wait.
                if lp.resume(child).is_ok() {
                    // Its drive task has not run yet: until it does, the child
                    // still reads `Paused` and not live.
                    for _ in 0..100 {
                        let st = rt.state(child).map_err(|e| e.to_string())?;
                        if st != RunState::Paused || rt.active_runs().contains(&child) {
                            break;
                        }
                        tokio::time::sleep(POLL).await;
                    }
                }
            }
            Some(RunState::Queued) if adopted && !live => {
                lp.spawn(child, Start::Fresh);
            }
            _ => {}
        }
        if !adopted {
            lp.spawn(child, Start::Fresh);
        }
        let state = self.wait_child(&rt, run.id, child).await?;
        let (text, is_error) = self.child_result(child, state);
        if adopted && state.is_terminal() {
            // No `conclude` ran for a child that finished before the restart.
            let ev = finished_event(child, run.id, state.as_str(), &text, Some(call));
            sh.emit(ctx, ev);
        }
        Ok((text, is_error))
    }

    fn create_or_find_child(
        &self,
        run: &Run,
        ctx: &RunCtx,
        rt: &Runtime,
        mut spec: SubSpec,
        key: String,
    ) -> Result<(RunId, bool), String> {
        let sh = &self.sh;
        // No `.await` while the gate is held.
        let _gate = sh.spawn_gate.lock();
        let kids = sh
            .storage
            .list_child_runs(run.id)
            .map_err(|e| e.to_string())?;
        if let Some(existing) = kids
            .iter()
            .find(|k| spawn_key_of(k).as_deref() == Some(key.as_str()))
        {
            return Ok((existing.id, true));
        }
        let live = kids.iter().filter(|k| is_live_state(k)).count();
        if live >= MAX_CHILDREN {
            return Err(format!(
                "too many subagents running at once (max {MAX_CHILDREN}); wait for one to \
                 finish before spawning another"
            ));
        }
        spec.spawn_key = key;
        spec.token_budget = self.child_token_budget(run, ctx)?;
        let id = rt
            .create_child_run(
                ctx.user_id,
                ctx.bot_id,
                Some(ctx.chat_id),
                &kind_for(InitiatorKind::Subagent),
                Some(run.id),
                Some(serde_json::to_value(&spec).map_err(|e| e.to_string())?),
            )
            .map_err(|e| e.to_string())?;
        Ok((id, false))
    }

    /// A quarter of what the parent's own token budget still has left (its own
    /// model calls and its whole subtree's spend are charged against it).
    fn child_token_budget(&self, run: &Run, ctx: &RunCtx) -> Result<u64, String> {
        let budget = ctx
            .sub
            .as_ref()
            .map_or(self.sh.cfg.guards.max_tokens_per_turn, |s| s.token_budget);
        let own = model_tokens(
            &self
                .sh
                .storage
                .list_steps(run.id)
                .map_err(|e| e.to_string())?,
        );
        let used = own.saturating_add(descendant_tokens(&self.sh.storage, run.id));
        Ok((budget.saturating_sub(used) / 4).max(1))
    }

    /// Poll until the child is terminal (or paused and no longer driven), then
    /// give its `conclude` bookkeeping a moment to emit the closing events.
    async fn wait_child(
        &self,
        rt: &Runtime,
        parent: RunId,
        child: RunId,
    ) -> Result<RunState, String> {
        let state = loop {
            let st = rt.state(child).map_err(|e| e.to_string())?;
            if st.is_terminal() {
                break st;
            }
            if st == RunState::Paused && !rt.active_runs().contains(&child) {
                if self
                    .sh
                    .shutting_down
                    .load(std::sync::atomic::Ordering::SeqCst)
                {
                    // The child was paused for a shutdown: this step is
                    // unfinished, not answered.  Pause the parent too, which
                    // drops this wait and leaves the step `Interrupted`; the
                    // replay after the restart adopts the child again.
                    rt.request_pause(parent, true);
                } else {
                    break st;
                }
            }
            tokio::time::sleep(POLL).await;
        };
        for _ in 0..100 {
            if !self.sh.runs.lock().contains_key(&child) {
                break;
            }
            tokio::time::sleep(POLL).await;
        }
        Ok(state)
    }

    /// The tool result for a child that ended in `state`.
    fn child_result(&self, child: RunId, state: RunState) -> (String, bool) {
        let run = self.sh.storage.get_run(child).ok().flatten();
        match state {
            RunState::Succeeded => {
                let text = final_text_of(&self.sh, child).unwrap_or_default();
                let text = if text.trim().is_empty() {
                    "(the subagent finished without a text answer)".to_owned()
                } else {
                    cap(&text, RESULT_CAP_BYTES)
                };
                (text, false)
            }
            RunState::Failed => (
                format!(
                    "Error: the subagent failed: {}",
                    run.and_then(|r| r.error)
                        .unwrap_or_else(|| "unknown error".into())
                ),
                true,
            ),
            RunState::Cancelled => ("Error: the subagent was cancelled.".to_owned(), true),
            other => (
                format!("Error: the subagent stopped in state {other} before finishing."),
                true,
            ),
        }
    }
}

/// Tokens the `STEP_MODEL` steps of `steps` spent.
pub(crate) fn model_tokens(steps: &[storage::RunStep]) -> u64 {
    steps
        .iter()
        .filter(|s| s.name == STEP_MODEL)
        .filter_map(step_output::<ModelOut>)
        .filter_map(|m| m.usage)
        .fold(0_u64, |total, usage| {
            total
                .saturating_add(usage.prompt_tokens)
                .saturating_add(usage.completion_tokens)
        })
}

/// Tokens spent by every descendant of `run_id` (children, grandchildren, ...),
/// finished ones included: a parent pays for what its subtree burned.
pub(crate) fn descendant_tokens(storage: &storage::Storage, run_id: RunId) -> u64 {
    let mut total = 0u64;
    let mut seen = std::collections::HashSet::new();
    let mut todo = vec![run_id];
    while let Some(id) = todo.pop() {
        for kid in storage.list_child_runs(id).unwrap_or_default() {
            if !seen.insert(kid.id) {
                continue;
            }
            total = total.saturating_add(model_tokens(
                &storage.list_steps(kid.id).unwrap_or_default(),
            ));
            todo.push(kid.id);
        }
    }
    total
}

/// Last model text of a run.
pub(crate) fn final_text_of(sh: &Shared, run_id: RunId) -> Option<String> {
    let steps = sh.storage.list_steps(run_id).ok()?;
    let last = steps.last()?;
    if last.name != STEP_MODEL {
        return None;
    }
    step_output::<ModelOut>(last)
        .map(|m| m.text)
        .filter(|t| !t.is_empty())
}

/// `text`, cut to `max` bytes with a marker when it was longer.
fn cap(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    format!("{}\n[truncated]", truncate_utf8(text, max))
}

pub(crate) fn finished_event(
    child: RunId,
    parent: RunId,
    state: &str,
    summary: &str,
    call: Option<ToolCallId>,
) -> Event {
    Event::SubagentFinished(SubagentFinished {
        run_id: child,
        parent_run_id: parent,
        state: state.to_owned(),
        summary: cap(summary, EVENT_SUMMARY_BYTES),
        call_id: call,
    })
}

impl AgentLoop {
    /// Closing bookkeeping of a subagent run: report to the parent's stream.
    /// A child never writes a chat message, starts a queued turn or emits
    /// `run_started` / `run_finished`.
    pub(crate) fn conclude_child(
        &self,
        run_id: RunId,
        parent: RunId,
        res: &Result<RunState, task_runtime::RuntimeError>,
    ) {
        let run = self.sh.storage.get_run(run_id).ok().flatten();
        let (state, summary) = match res {
            Ok(RunState::Succeeded) => (
                "succeeded",
                final_text_of(&self.sh, run_id).unwrap_or_default(),
            ),
            Ok(RunState::Paused) => ("paused", "paused: the run was interrupted".to_owned()),
            Ok(RunState::Cancelled) => ("cancelled", "cancelled".to_owned()),
            Ok(RunState::Failed) => (
                "failed",
                run.as_ref()
                    .and_then(|r| r.error.clone())
                    .unwrap_or_else(|| "failed".into()),
            ),
            Ok(other) => (other.as_str(), format!("stopped in state {other}")),
            Err(e) => ("failed", e.to_string()),
        };
        if let Some(ctx) = self.ctx_of(run_id)
            && state == "failed"
        {
            self.sh.emit(
                &ctx,
                Event::Error(protocol::event::RunError {
                    run_id,
                    message: summary.clone(),
                    recoverable: false,
                }),
            );
        }
        if let Some(pctx) = self.ctx_of(parent) {
            self.sh
                .emit(&pctx, finished_event(run_id, parent, state, &summary, None));
        }
        self.sh.runs.lock().remove(&run_id);
        // Reporting may have rebuilt the context of a parent that is gone.
        if !self.rt.active_runs().contains(&parent) {
            self.sh.runs.lock().remove(&parent);
        }
    }

    /// Cancel every unfinished descendant of `run_id`.  Live ones finish as
    /// `Cancelled` through their own drive task; idle (paused) ones are
    /// cancelled here and reported to the parent's stream.
    pub(crate) fn cancel_descendants(&self, run_id: RunId) {
        let Ok(kids) = self.sh.storage.list_child_runs(run_id) else {
            return;
        };
        for kid in kids {
            self.cancel_descendants(kid.id);
            let Some(state) = RunState::parse(&kid.state) else {
                continue;
            };
            if state.is_terminal() {
                continue;
            }
            let live = self.rt.active_runs().contains(&kid.id);
            if self.rt.cancel(kid.id).is_ok()
                && !live
                && let Some(pctx) = self.ctx_of(run_id)
            {
                self.sh.emit(
                    &pctx,
                    finished_event(kid.id, run_id, "cancelled", "cancelled", None),
                );
            }
        }
    }
}
