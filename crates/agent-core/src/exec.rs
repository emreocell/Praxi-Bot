//! The [`StepExecutor`] that turns the durable runtime into an agent loop.
//!
//! A turn is a sequence of persisted steps (see `shared::STEP_*`):
//! `user_message` -> `model` -> `tools`* -> `model` -> ... -> done.  The
//! conversation is rebuilt from those rows on every model call, so a restart
//! needs no in-memory state.

use std::fmt::Write as _;
use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use permission_engine::{ActionRequest, Decision, Policy};
use protocol::event::{
    ApprovalNeeded, MessageDelta, ToolCallApproved, ToolCallCompleted, ToolCallDenied,
    ToolCallRequested,
};
use protocol::{Event, RunId, ToolCallId};
use provider_catalog::{ChatChunk, ChatRequest, ModelRef};
use serde_json::{Value, json};
use storage::{Approval, Run, RunStep, now_ms};
use task_runtime::{ExecError, RunState, StepExecutor, StepSpec, StepStatus, can_transition};
use tokio_util::sync::CancellationToken;
use tool_registry::{ContentPart, Effect, Tool, ToolContext, ToolOutput};
use uuid::Uuid;

use crate::history::{build_messages, expose, status_of, step_output, tool_defs, truncate_utf8};
use crate::shared::{
    ApprovalScope, CallRec, ModelOut, Pending, ResultRec, RunCtx, STEP_MODEL, STEP_TOOLS,
    STEP_USER, Shared, Verdict, xe,
};
use crate::todo::TOOL_NAME as TODO_TOOL;

pub(crate) struct TurnExecutor {
    pub sh: Arc<Shared>,
}

enum Slot {
    Done { text: String, is_error: bool },
    Run { tool: Arc<dyn Tool>, input: Value },
}

struct PendingGuard {
    sh: Arc<Shared>,
    id: ToolCallId,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.sh.pending.lock().remove(&self.id);
    }
}

fn call_key(name: &str, args: &str) -> (String, Value) {
    let v = serde_json::from_str(args).unwrap_or_else(|_| Value::String(args.to_owned()));
    (name.to_owned(), v)
}

fn user_step(text: &str) -> StepSpec {
    StepSpec {
        name: STEP_USER.to_owned(),
        input: json!({ "text": text }),
        idempotent: true,
        retry: None,
    }
}

#[async_trait]
impl StepExecutor for TurnExecutor {
    async fn next_step(
        &self,
        run: &Run,
        history: &[RunStep],
    ) -> Result<Option<StepSpec>, ExecError> {
        self.plan_next(run, history)
    }

    async fn execute(
        &self,
        run: &Run,
        step: &StepSpec,
        _cancel: &CancellationToken,
    ) -> Result<Value, ExecError> {
        match step.name.as_str() {
            STEP_USER => Ok(json!({ "text": step.input["text"] })),
            STEP_MODEL => self.exec_model(run).await,
            STEP_TOOLS => self.exec_tools(run, step).await,
            other => Err(xe(format!("unknown step kind {other:?}"))),
        }
    }
}

impl TurnExecutor {
    // ---- planning ----

    fn plan_next(&self, run: &Run, history: &[RunStep]) -> Result<Option<StepSpec>, ExecError> {
        let ctx = self.sh.ctx(run)?;
        if history.is_empty() {
            let first = self
                .sh
                .storage
                .list_messages(ctx.chat_id)
                .map_err(xe)?
                .into_iter()
                .find(|m| m.run_id == Some(run.id) && m.role == "user")
                .ok_or_else(|| xe("turn has no user message"))?;
            return Ok(Some(user_step(&first.content)));
        }
        if let Some(spec) = self.pending_tools(&ctx, history)? {
            return Ok(Some(spec));
        }

        let finished = history.last().is_some_and(|s| {
            s.name == STEP_MODEL
                && status_of(s) == Some(StepStatus::Succeeded)
                && step_output::<ModelOut>(s).is_some_and(|m| m.tool_calls.is_empty())
        });
        {
            let mut st = ctx.state.lock();
            if let Some(text) = st.steer.pop_front() {
                return Ok(Some(user_step(&text)));
            }
            if finished {
                st.closed = true;
                return Ok(None);
            }
        }

        let g = &self.sh.cfg.guards;
        let model_steps = history.iter().filter(|s| s.name == STEP_MODEL).count();
        if model_steps >= g.max_steps as usize {
            return Err(xe(format!(
                "step limit reached: {} model calls in one turn",
                g.max_steps
            )));
        }
        let tokens: u64 = history
            .iter()
            .filter(|s| s.name == STEP_MODEL)
            .filter_map(step_output::<ModelOut>)
            .filter_map(|m| m.usage)
            .map(|u| u.prompt_tokens + u.completion_tokens)
            .sum();
        if tokens >= g.max_tokens_per_turn {
            return Err(xe(format!(
                "token budget exhausted: {tokens} >= {}",
                g.max_tokens_per_turn
            )));
        }
        Ok(Some(StepSpec {
            name: STEP_MODEL.to_owned(),
            input: json!({ "model": ctx.profile.models.executor, "n": model_steps }),
            idempotent: true,
            retry: Some(self.sh.cfg.model_retry),
        }))
    }

    fn is_read(&self, ctx: &RunCtx, name: &str) -> bool {
        ctx.profile.tool_visible(name)
            && self
                .sh
                .tools
                .get(name)
                .is_some_and(|t| t.spec().effect == Effect::Read)
    }

    /// Next batch of tool calls still owed an answer for the last model step.
    fn pending_tools(
        &self,
        ctx: &RunCtx,
        history: &[RunStep],
    ) -> Result<Option<StepSpec>, ExecError> {
        let Some(mi) = history.iter().rposition(|s| {
            s.name == STEP_MODEL
                && status_of(s) == Some(StepStatus::Succeeded)
                && s.output.is_some()
        }) else {
            return Ok(None);
        };
        let Some(mo) = step_output::<ModelOut>(&history[mi]) else {
            return Ok(None);
        };
        let tool_steps = |range: &[RunStep]| -> Vec<CallRec> {
            range
                .iter()
                .filter(|s| s.name == STEP_TOOLS)
                .flat_map(|s| {
                    serde_json::from_value::<Vec<CallRec>>(s.input["calls"].clone())
                        .unwrap_or_default()
                })
                .collect()
        };
        let covered = tool_steps(&history[mi + 1..]).len();
        if covered >= mo.tool_calls.len() {
            return Ok(None);
        }
        let remaining = &mo.tool_calls[covered..];

        let mut seq: Vec<(String, Value)> = tool_steps(history)
            .iter()
            .map(|c| call_key(&c.name, &c.arguments))
            .collect();
        let group_all_reads = self.is_read(ctx, &remaining[0].name);
        let take = if group_all_reads {
            remaining
                .iter()
                .take_while(|c| self.is_read(ctx, &c.name))
                .count()
        } else {
            1
        };

        let g = &self.sh.cfg.guards;
        let mut calls = Vec::with_capacity(take);
        for c in &remaining[..take] {
            let key = call_key(&c.name, &c.arguments);
            let streak = 1 + seq.iter().rev().take_while(|k| **k == key).count();
            let repeat = u32::try_from(streak).unwrap_or(u32::MAX);
            if repeat >= g.repeat_stop {
                return Err(xe(format!(
                    "repeat detected: tool {} called {repeat} times in a row with identical arguments",
                    c.name
                )));
            }
            seq.push(key);
            calls.push(CallRec {
                id: c.id.clone(),
                name: c.name.clone(),
                arguments: c.arguments.clone(),
                repeat,
            });
        }
        Ok(Some(StepSpec {
            name: STEP_TOOLS.to_owned(),
            input: json!({ "calls": calls }),
            idempotent: group_all_reads,
            retry: None,
        }))
    }

    // ---- model step ----

    async fn exec_model(&self, run: &Run) -> Result<Value, ExecError> {
        let ctx = self.sh.ctx(run)?;
        let steps = self.sh.storage.list_steps(run.id).map_err(xe)?;
        let specs = self.sh.visible_specs(&ctx.profile);
        let messages = build_messages(&self.sh.storage, run, &steps, &ctx.profile).map_err(xe)?;
        let model = ModelRef(ctx.profile.models.executor.clone());
        let mut req = ChatRequest::new(model.clone(), messages);
        req.tools = tool_defs(&specs);
        req.max_tokens = self.sh.cfg.max_tokens;
        req.temperature = self.sh.cfg.temperature;
        let provider = self.sh.providers.pick(&model);
        let mut stream = provider.stream(req).await.map_err(xe)?;

        let mut out = ModelOut::default();
        while let Some(chunk) = stream.next().await {
            match chunk.map_err(xe)? {
                ChatChunk::TextDelta(t) => {
                    self.sh.emit(
                        &ctx,
                        Event::MessageDelta(MessageDelta {
                            run_id: run.id,
                            text: t.clone(),
                        }),
                    );
                    out.text.push_str(&t);
                }
                ChatChunk::ToolCall(mut tc) => {
                    if let Some(real) = specs.iter().find(|s| expose(&s.name) == tc.name) {
                        tc.name.clone_from(&real.name);
                    }
                    out.tool_calls.push(tc);
                }
                ChatChunk::Usage(u) => out.usage = Some(u),
                ChatChunk::Done { finish_reason } => {
                    out.finish = format!("{finish_reason:?}");
                    break;
                }
                ChatChunk::ToolCallDelta { .. } => {}
            }
        }
        if let Some(u) = out.usage {
            let cost = self
                .sh
                .cfg
                .prices
                .get(&model.0)
                .map(|p| p.cost(u.prompt_tokens, u.completion_tokens, u.cached_tokens));
            self.sh.audit(
                &ctx,
                "bot",
                "usage",
                &json!({
                    "model": model.0,
                    "prompt_tokens": u.prompt_tokens,
                    "completion_tokens": u.completion_tokens,
                    "cached_tokens": u.cached_tokens,
                    "cost_usd": cost,
                }),
            );
        }
        serde_json::to_value(out).map_err(xe)
    }

    // ---- tool step ----

    #[allow(clippy::too_many_lines)]
    async fn exec_tools(&self, run: &Run, step: &StepSpec) -> Result<Value, ExecError> {
        let ctx = self.sh.ctx(run)?;
        let calls: Vec<CallRec> =
            serde_json::from_value(step.input["calls"].clone()).map_err(xe)?;

        let mut slots = Vec::with_capacity(calls.len());
        let mut uuids = Vec::with_capacity(calls.len());
        for call in &calls {
            let call_uuid = ToolCallId::new();
            uuids.push(call_uuid);
            let parsed: Result<Value, String> = if call.arguments.trim().is_empty() {
                Ok(json!({}))
            } else {
                serde_json::from_str(&call.arguments)
                    .map_err(|e| format!("invalid JSON arguments: {e}"))
            };
            self.sh.emit(
                &ctx,
                Event::ToolCallRequested(ToolCallRequested {
                    run_id: run.id,
                    call_id: call_uuid,
                    tool_name: call.name.clone(),
                    input: parsed.clone().unwrap_or(Value::Null),
                }),
            );
            let err = |m: String| Slot::Done {
                text: format!("Error: {m}"),
                is_error: true,
            };
            let input = match parsed {
                Ok(v) => v,
                Err(m) => {
                    slots.push(err(m));
                    continue;
                }
            };
            let tool = ctx
                .profile
                .tool_visible(&call.name)
                .then(|| self.sh.tools.get(&call.name))
                .flatten();
            let Some(tool) = tool else {
                slots.push(err(format!("unknown tool {:?}", call.name)));
                continue;
            };
            let (effect, category) = {
                let s = tool.spec();
                (s.effect, s.category)
            };
            let decision = if call.name == TODO_TOOL {
                Decision::Allow
            } else {
                self.decide(&ctx, &call.name, effect, category, &input)
            };
            let slot = match decision {
                Decision::Allow => Slot::Run { tool, input },
                Decision::Deny => {
                    self.sh.emit(
                        &ctx,
                        Event::ToolCallDenied(ToolCallDenied {
                            run_id: run.id,
                            call_id: call_uuid,
                            denied_by: ctx.user_id,
                            reason: Some("denied by policy".into()),
                        }),
                    );
                    Slot::Done {
                        text: "Permission denied: policy does not allow this tool call. \
                               Do not retry it; choose another approach or tell the user."
                            .into(),
                        is_error: true,
                    }
                }
                Decision::Ask => match self
                    .request_approval(&ctx, call_uuid, &call.name, &input)
                    .await
                {
                    Ok(()) => Slot::Run { tool, input },
                    Err(reason) => Slot::Done {
                        text: format!(
                            "Permission denied by the user{}. Do not retry this call.",
                            reason.map_or_else(String::new, |r| format!(": {r}"))
                        ),
                        is_error: true,
                    },
                },
            };
            slots.push(slot);
        }

        // Reads batched by `pending_tools` run concurrently; everything else is a single call.
        let outcomes = futures::future::join_all(slots.into_iter().map(|slot| async move {
            match slot {
                Slot::Done { text, is_error } => (text, is_error),
                Slot::Run { tool, input } => self.run_tool(run, &tool, input).await,
            }
        }))
        .await;

        let g = &self.sh.cfg.guards;
        let mut results = Vec::with_capacity(calls.len());
        for ((call, call_uuid), (text, is_error)) in calls.iter().zip(uuids).zip(outcomes) {
            let (mut text, spill_path) = self.spill(run, call_uuid, text);
            if call.repeat >= g.repeat_warn {
                let _ = write!(
                    text,
                    "\n[warning: this exact call has now been made {} times in a row; \
                     change your approach or stop]",
                    call.repeat
                );
            }
            self.sh.emit(
                &ctx,
                Event::ToolCallCompleted(ToolCallCompleted {
                    run_id: run.id,
                    call_id: call_uuid,
                    error: is_error.then(|| truncate_utf8(&text, 200).to_owned()),
                }),
            );
            results.push(ResultRec {
                id: call.id.clone(),
                call_uuid: call_uuid.into_inner(),
                name: call.name.clone(),
                is_error,
                text,
                spill_path,
            });
        }
        Ok(json!({ "results": results }))
    }

    async fn run_tool(&self, run: &Run, tool: &Arc<dyn Tool>, input: Value) -> (String, bool) {
        let g = &self.sh.cfg.guards;
        let tctx = ToolContext {
            run_id: run.id.to_string(),
            max_inline_bytes: g.spill_threshold_bytes,
        };
        match tokio::time::timeout(g.tool_timeout, tool.call(&tctx, input)).await {
            Err(_) => (
                format!("Error: tool timed out after {:?}", g.tool_timeout),
                true,
            ),
            Ok(Err(e)) => (format!("Error: {e}"), true),
            Ok(Ok(out)) => (render_output(&out), false),
        }
    }

    /// Spill oversized output to `<data_dir>/spill/<run>/<call>.txt`; the model
    /// keeps a preview and the path.
    fn spill(&self, run: &Run, call: ToolCallId, text: String) -> (String, Option<String>) {
        let g = &self.sh.cfg.guards;
        if text.len() <= g.spill_threshold_bytes {
            return (text, None);
        }
        let dir = self
            .sh
            .storage
            .data_dir()
            .join("spill")
            .join(run.id.to_string());
        let path = dir.join(format!("{call}.txt"));
        let preview = truncate_utf8(&text, g.spill_preview_bytes);
        let saved = std::fs::create_dir_all(&dir)
            .and_then(|()| std::fs::write(&path, &text))
            .is_ok();
        if saved {
            let shown = path.display().to_string();
            let msg = format!(
                "{preview}\n[output truncated: {} bytes total; full output saved to {shown}]",
                text.len()
            );
            (msg, Some(shown))
        } else {
            (
                format!("{preview}\n[output truncated: {} bytes total]", text.len()),
                None,
            )
        }
    }

    // ---- permissions ----

    fn decide(
        &self,
        ctx: &RunCtx,
        name: &str,
        effect: Effect,
        category: protocol::PermissionCategory,
        input: &Value,
    ) -> Decision {
        let req = ActionRequest {
            category,
            raw_command: input
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            initiator: ctx.initiator,
        };
        let r = &self.sh.resolver;
        let d = r.resolve(
            &self.sh.cfg.global_policy,
            ctx.bot_policy.as_ref(),
            None,
            &req,
        );
        if d != Decision::Ask {
            return d;
        }
        // An `Ask` forced by an absolute floor (money, never-list) survives
        // even a full policy; only a policy-level `Ask` can be waived.
        let floor = r.resolve(&Policy::full(), None, None, &req) != Decision::Allow;
        if floor {
            return d;
        }
        let granted = {
            let g = self.sh.grants.lock();
            g.this_chat.contains(&(ctx.chat_id, name.to_owned()))
                || g.always.contains(&(ctx.bot_id, name.to_owned()))
        };
        if granted || (self.sh.cfg.read_tools_skip_ask && effect == Effect::Read) {
            Decision::Allow
        } else {
            d
        }
    }

    /// Park the run in `WaitingApproval` until `approve` / `deny` is called.
    /// `Ok(())` = approved, `Err(reason)` = denied.
    async fn request_approval(
        &self,
        ctx: &RunCtx,
        call_id: ToolCallId,
        name: &str,
        input: &Value,
    ) -> Result<(), Option<String>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.sh.pending.lock().insert(call_id, Pending { tx });
        let _guard = PendingGuard {
            sh: Arc::clone(&self.sh),
            id: call_id,
        };
        let args = input.to_string();
        let summary = format!("{name} {}", truncate_utf8(&args, 300));
        let approval_id = Uuid::now_v7();
        let _ = self.sh.storage.insert_approval(&Approval {
            id: approval_id,
            user_id: ctx.user_id,
            run_id: ctx.run_id,
            step_id: None,
            summary: summary.clone(),
            decision: None,
            requested_at_ms: now_ms(),
            decided_at_ms: None,
        });
        self.set_state(ctx.run_id, RunState::WaitingApproval);
        self.sh.emit(
            ctx,
            Event::ApprovalNeeded(ApprovalNeeded {
                run_id: ctx.run_id,
                call_id,
                tool_name: name.to_owned(),
                input: input.clone(),
                summary,
            }),
        );
        let verdict = rx.await;
        self.set_state(ctx.run_id, RunState::Running);

        match verdict {
            Ok(Verdict::Approved { scope, by }) => {
                let _ = self.sh.storage.decide_approval(approval_id, "approved");
                match scope {
                    ApprovalScope::Once => {}
                    ApprovalScope::ThisChat => {
                        self.sh
                            .grants
                            .lock()
                            .this_chat
                            .insert((ctx.chat_id, name.to_owned()));
                    }
                    ApprovalScope::Always => {
                        self.sh
                            .grants
                            .lock()
                            .always
                            .insert((ctx.bot_id, name.to_owned()));
                        self.sh.audit(
                            ctx,
                            "user",
                            crate::AUDIT_GRANT_ALWAYS,
                            &json!({ "bot_id": ctx.bot_id.to_string(), "tool": name }),
                        );
                    }
                }
                self.sh.audit(
                    ctx,
                    "user",
                    "approval.approved",
                    &json!({ "tool": name, "scope": format!("{scope:?}") }),
                );
                self.sh.emit(
                    ctx,
                    Event::ToolCallApproved(ToolCallApproved {
                        run_id: ctx.run_id,
                        call_id,
                        approved_by: by,
                    }),
                );
                Ok(())
            }
            Ok(Verdict::Denied { by, reason }) => {
                let _ = self.sh.storage.decide_approval(approval_id, "denied");
                self.sh.audit(
                    ctx,
                    "user",
                    "approval.denied",
                    &json!({ "tool": name, "reason": reason }),
                );
                self.sh.emit(
                    ctx,
                    Event::ToolCallDenied(ToolCallDenied {
                        run_id: ctx.run_id,
                        call_id,
                        denied_by: by,
                        reason: reason.clone(),
                    }),
                );
                Err(reason)
            }
            Err(_) => Err(Some("approval request was dropped".into())),
        }
    }

    fn set_state(&self, run_id: RunId, to: RunState) {
        if let Ok(Some(run)) = self.sh.storage.get_run(run_id)
            && let Some(from) = RunState::parse(&run.state)
            && can_transition(from, to)
        {
            let _ = self.sh.storage.update_run_state(run_id, to.as_str(), None);
        }
    }
}

fn render_output(out: &ToolOutput) -> String {
    let mut parts: Vec<String> = out
        .content
        .iter()
        .map(|p| match p {
            ContentPart::Text { text } => text.clone(),
            ContentPart::Json { data } => data.to_string(),
            ContentPart::Binary { mime_type, base64 } => {
                format!(
                    "[binary {mime_type}, {} base64 chars omitted]",
                    base64.len()
                )
            }
        })
        .collect();
    if let Some(s) = &out.spill {
        parts.push(format!(
            "[full output ({} bytes) at {}]",
            s.size_bytes, s.path
        ));
    }
    parts.join("\n")
}
