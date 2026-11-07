//! The [`StepExecutor`] that turns the durable runtime into an agent loop.
//!
//! A turn is a sequence of persisted steps (see `shared::STEP_*`):
//! `user_message` -> `model` -> `tools`* -> `model` -> ... -> done.  The
//! conversation is rebuilt from those rows on every model call, so a restart
//! needs no in-memory state.

use std::fmt::Write as _;
use std::sync::Arc;

use async_trait::async_trait;
use builtin_tools::{QUESTION_CARD_KIND, TODO_LIST_KIND};
use futures::StreamExt;
use permission_engine::Decision;
use protocol::PermissionCategory;
use protocol::event::{
    MessageDelta, ModelFallback, TodoItem, TodoStatus, TodoUpdated, ToolCallCompleted,
    ToolCallDenied, ToolCallRequested,
};
use protocol::{Event, ToolCallId};
use provider_catalog::{ChatChunk, ChatRequest, ModelRef, ProviderError};
use serde_json::{Value, json};
use storage::{Run, RunStep};
use task_runtime::{ExecError, StepExecutor, StepSpec, StepStatus};
use tokio_util::sync::CancellationToken;
use tool_registry::{ContentPart, Tool, ToolContext, ToolOutput, ToolSpec};

use crate::history::{
    GUARD_NOTE, build_messages, expose, status_of, step_output, tool_defs, truncate_utf8,
};
use crate::router::{RoutePlan, route_hints};
use crate::shared::{
    CallRec, ModelOut, ResultRec, RunCtx, STEP_COMMAND, STEP_MODEL, STEP_TOOLS, STEP_USER,
    STEP_WORKFLOW, Shared, xe,
};

pub(crate) struct TurnExecutor {
    pub sh: Arc<Shared>,
}

enum Slot {
    Done {
        text: String,
        is_error: bool,
    },
    Run {
        tool: Arc<dyn Tool>,
        input: Value,
        call: ToolCallId,
        meta: bool,
    },
    /// A `spawn_subagent` call, executed by the loop itself.
    Spawn {
        input: Value,
        call: ToolCallId,
        model_call_id: String,
    },
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

/// The first step of a routed turn: the user message plus the ladder's
/// decision (the audit record; `remember` and the hints read it back).
fn routed_user_step(text: &str, decision: &agent_router::ladder::Decision) -> StepSpec {
    let mut spec = user_step(text);
    spec.input["route"] = serde_json::to_value(decision).unwrap_or(Value::Null);
    spec
}

#[async_trait]
impl StepExecutor for TurnExecutor {
    async fn next_step(
        &self,
        run: &Run,
        history: &[RunStep],
    ) -> Result<Option<StepSpec>, ExecError> {
        if history.is_empty()
            && let Some(spec) = self.first_step(run).await?
        {
            return Ok(Some(spec));
        }
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
            STEP_MODEL => self.exec_model(run, step).await,
            STEP_TOOLS => self.exec_tools(run, step).await,
            STEP_WORKFLOW => self.exec_workflow(run, step).await,
            STEP_COMMAND => self.exec_command(run, step),
            other => Err(xe(format!("unknown step kind {other:?}"))),
        }
    }
}

impl TurnExecutor {
    // ---- planning ----

    /// The first step of a top-level turn: the user message, after the
    /// decision ladder (K0-K3) had its say.  `None` for subagents (their first
    /// step comes from `plan_next`).
    async fn first_step(&self, run: &Run) -> Result<Option<StepSpec>, ExecError> {
        let ctx = self.sh.ctx(run)?;
        if ctx.sub.is_some() {
            return Ok(None);
        }
        let raw = self.first_user_text(&ctx, run)?;
        let Some(decision) = self.route_first(&ctx, &raw).await else {
            return Ok(Some(user_step(&self.with_recall(&ctx, &raw))));
        };
        if let agent_router::ladder::Route::Command(c) = &decision.route
            && c.name == "stop"
        {
            // `/stop`: the run that carries it is cancelled before it starts.
            self.stop_run(run);
        }
        Ok(Some(routed_user_step(
            &self.with_recall(&ctx, &raw),
            &decision,
        )))
    }

    fn first_user_text(&self, ctx: &RunCtx, run: &Run) -> Result<String, ExecError> {
        self.sh
            .storage
            .list_messages(ctx.chat_id)
            .map_err(xe)?
            .into_iter()
            .find(|m| m.run_id == Some(run.id) && m.role == "user")
            .map(|m| m.content)
            .ok_or_else(|| xe("turn has no user message"))
    }

    fn plan_next(&self, run: &Run, history: &[RunStep]) -> Result<Option<StepSpec>, ExecError> {
        let ctx = self.sh.ctx(run)?;
        if history.is_empty() {
            if let Some(sub) = &ctx.sub {
                return Ok(Some(user_step(&sub.first_message())));
            }
            let first = self.first_user_text(&ctx, run)?;
            return Ok(Some(user_step(&self.with_recall(&ctx, &first))));
        }
        let steer_empty = ctx.state.lock().steer.is_empty();
        match Self::route_plan(history, steer_empty) {
            RoutePlan::Step(spec) => return Ok(Some(spec)),
            RoutePlan::Done => {
                ctx.state.lock().closed = true;
                return Ok(None);
            }
            RoutePlan::Continue => {}
        }
        let model_steps = history.iter().filter(|s| s.name == STEP_MODEL).count();
        match self.pending_tools(&ctx, history) {
            Ok(Some(spec)) => return Ok(Some(spec)),
            Ok(None) => {}
            Err(reason) => return Ok(Some(Self::guard_step(&ctx, model_steps, &reason))),
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
        let max_steps = ctx.sub.as_ref().map_or(g.max_steps, |s| s.max_steps);
        let max_tokens = ctx
            .sub
            .as_ref()
            .map_or(g.max_tokens_per_turn, |s| s.token_budget);
        if model_steps >= max_steps as usize {
            let reason = format!("step limit reached: {max_steps} model calls in one turn");
            return Ok(Some(Self::guard_step(&ctx, model_steps, &reason)));
        }
        let tokens: u64 = history
            .iter()
            .filter(|s| s.name == STEP_MODEL)
            .filter_map(step_output::<ModelOut>)
            .filter_map(|m| m.usage)
            .map(|u| u.prompt_tokens + u.completion_tokens)
            .sum();
        if tokens >= max_tokens {
            let reason = format!("token budget exhausted: {tokens} >= {max_tokens}");
            return Ok(Some(Self::guard_step(&ctx, model_steps, &reason)));
        }
        Ok(Some(StepSpec {
            name: STEP_MODEL.to_owned(),
            input: json!({ "model": ctx.profile.models.executor, "n": model_steps }),
            idempotent: true,
            // Retries (and fallbacks) happen inside `exec_model`, which knows
            // which provider errors are worth retrying.
            retry: None,
        }))
    }

    /// The closing, tool-less model call that replaces a hard failure when a
    /// guard stops the turn.
    fn guard_step(ctx: &RunCtx, model_steps: usize, reason: &str) -> StepSpec {
        StepSpec {
            name: STEP_MODEL.to_owned(),
            input: json!({
                "model": ctx.profile.models.executor,
                "n": model_steps,
                "final": true,
                "guard": reason,
            }),
            idempotent: true,
            retry: None,
        }
    }

    /// Prepend the recalled-memory block to the first user message (never to
    /// the system prompt, so the prompt cache stays valid).
    fn with_recall(&self, ctx: &RunCtx, text: &str) -> String {
        let Some(m) = self
            .sh
            .cfg
            .memory
            .as_ref()
            .filter(|m| m.enabled && ctx.parent_run_id.is_none())
        else {
            return text.to_owned();
        };
        match m
            .store
            .recall(ctx.user_id, ctx.bot_id, text, m.recall_budget_tokens)
        {
            Ok(mems) => memory_store::prepend_to_user_message(
                &memory_store::render_recall_block(&mems),
                text,
            ),
            Err(_) => text.to_owned(),
        }
    }

    fn is_read(&self, ctx: &RunCtx, hints: &[String], name: &str, args: &str) -> bool {
        let input: Value = serde_json::from_str(args).unwrap_or(Value::Null);
        self.sh
            .resolve_call(&ctx.profile, name, &input, hints)
            .and_then(|r| r.spec)
            .is_some_and(|s| s.effect == tool_registry::Effect::Read)
    }

    /// Next batch of tool calls still owed an answer for the last model step.
    fn pending_tools(&self, ctx: &RunCtx, history: &[RunStep]) -> Result<Option<StepSpec>, String> {
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
        let hints = route_hints(history).tools;
        let covered = tool_steps(&history[mi + 1..]).len();
        if covered >= mo.tool_calls.len() {
            return Ok(None);
        }
        let remaining = &mo.tool_calls[covered..];

        let mut seq: Vec<(String, Value)> = tool_steps(history)
            .iter()
            .map(|c| call_key(&c.name, &c.arguments))
            .collect();
        let group_all_reads =
            self.is_read(ctx, &hints, &remaining[0].name, &remaining[0].arguments);
        let take = if group_all_reads {
            remaining
                .iter()
                .take_while(|c| self.is_read(ctx, &hints, &c.name, &c.arguments))
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
                return Err(format!(
                    "repeat detected: tool {} called {repeat} times in a row with identical arguments",
                    c.name
                ));
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

    async fn exec_model(&self, run: &Run, step: &StepSpec) -> Result<Value, ExecError> {
        let ctx = self.sh.ctx(run)?;
        let steps = self.sh.storage.list_steps(run.id).map_err(xe)?;
        let guard_reason = step.input["final"]
            .as_bool()
            .unwrap_or(false)
            .then(|| step.input["guard"].as_str().unwrap_or("guard").to_owned());
        let hints = route_hints(&steps);
        let specs = if guard_reason.is_some() {
            Vec::new()
        } else {
            self.sh.model_specs(&ctx.profile, &hints.tools)
        };
        let note = guard_reason
            .as_deref()
            .map(|r| GUARD_NOTE.replace("{reason}", r));
        let skills = self.skills_block(&ctx, &steps, &hints.skills);
        let messages = build_messages(
            &self.sh.storage,
            run,
            &steps,
            &ctx.profile,
            note.as_deref(),
            skills.as_deref(),
        )
        .map_err(xe)?;
        let primary = ctx.profile.models.executor.clone();
        let mut req = ChatRequest::new(ModelRef(primary.clone()), messages);
        req.tools = tool_defs(&specs);
        req.max_tokens = self.sh.cfg.max_tokens;
        req.temperature = self.sh.cfg.temperature;

        let models: Vec<&String> = std::iter::once(&primary)
            .chain(ctx.profile.models.fallback.iter())
            .collect();
        let mut last_err: Option<ProviderError> = None;
        let mut result: Option<ModelOut> = None;
        for (i, name) in models.iter().enumerate() {
            if let Some(prev) = &last_err {
                self.sh.emit(
                    &ctx,
                    Event::ModelFallback(ModelFallback {
                        run_id: run.id,
                        from: models[i - 1].clone(),
                        to: (*name).clone(),
                        reason: prev.to_string(),
                    }),
                );
            }
            req.model = ModelRef((*name).clone());
            let timeout = self.sh.cfg.guards.model_timeout;
            let attempt =
                tokio::time::timeout(timeout, self.call_with_retries(&ctx, run, &req, &specs))
                    .await
                    .unwrap_or(Err(ProviderError::Timeout));
            match attempt {
                Ok(out) => {
                    result = Some(out);
                    break;
                }
                Err(e) => {
                    // Only a retryable failure of the primary model justifies
                    // switching; a broken request / credential would fail
                    // everywhere.
                    let hard = i == 0 && !is_retryable(&e);
                    last_err = Some(e);
                    if hard {
                        break;
                    }
                }
            }
        }
        let Some(mut out) = result else {
            return Err(xe(last_err.map_or_else(
                || "no model available".to_owned(),
                |e| e.to_string(),
            )));
        };
        if let Some(reason) = guard_reason {
            // Tools were disabled: ignore anything the model still tried to call.
            out.tool_calls.clear();
            out.stopped_by_guard = true;
            self.sh.audit(
                &ctx,
                "bot",
                "run.stopped_by_guard",
                &json!({ "reason": reason }),
            );
            out.guard_reason = Some(reason);
        }
        self.audit_usage(&ctx, &out);
        serde_json::to_value(out).map_err(xe)
    }

    /// Records a `usage` audit entry (tokens and estimated cost) for a model
    /// step that reported usage.
    fn audit_usage(&self, ctx: &RunCtx, out: &ModelOut) {
        if let Some(u) = &out.usage {
            let cost = self
                .sh
                .price_for(&out.model)
                .map(|p| p.cost(u.prompt_tokens, u.completion_tokens, u.cached_tokens));
            self.sh.audit(
                ctx,
                "bot",
                "usage",
                &json!({
                    "model": out.model,
                    "prompt_tokens": u.prompt_tokens,
                    "completion_tokens": u.completion_tokens,
                    "cached_tokens": u.cached_tokens,
                    "cost_usd": cost,
                }),
            );
        }
    }

    /// The skills index block for the system prompt: skills matching the
    /// foreground page / app (when the embedder tells us) or else the turn's
    /// latest user text are listed first.
    fn skills_block(
        &self,
        ctx: &RunCtx,
        steps: &[RunStep],
        suggested: &[String],
    ) -> Option<String> {
        let index = self.sh.skills.get()?;
        let hints = self
            .sh
            .skill_hints
            .read()
            .as_ref()
            .map(|f| f())
            .unwrap_or_default();
        let text = steps
            .iter()
            .rev()
            .find(|s| s.name == STEP_USER)
            .and_then(|s| s.input["text"].as_str())
            .map(str::to_owned)
            .or_else(|| ctx.sub.as_ref().map(|s| s.task.clone()));
        let block = index.prompt_block_suggesting(
            &skills::SkillContext {
                domain: hints.domain,
                app: hints.app,
                text,
            },
            suggested,
        );
        (!block.is_empty()).then_some(block)
    }

    /// One model, up to `model_retry.max_attempts` attempts; non-retryable
    /// errors (bad credentials, bad request, 4xx) fail immediately.
    async fn call_with_retries(
        &self,
        ctx: &RunCtx,
        run: &Run,
        req: &ChatRequest,
        specs: &[ToolSpec],
    ) -> Result<ModelOut, ProviderError> {
        let policy = self.sh.cfg.model_retry;
        let attempts = policy.max_attempts.max(1);
        let mut attempt = 1;
        loop {
            match self.stream_once(ctx, run, req.clone(), specs).await {
                Ok(out) => return Ok(out),
                Err(e) if attempt < attempts && is_retryable(&e) => {
                    tokio::time::sleep(policy.backoff(attempt)).await;
                    attempt += 1;
                }
                Err(e) => return Err(e),
            }
        }
    }

    async fn stream_once(
        &self,
        ctx: &RunCtx,
        run: &Run,
        req: ChatRequest,
        specs: &[ToolSpec],
    ) -> Result<ModelOut, ProviderError> {
        let model = req.model.clone();
        let provider = self.sh.providers.read().pick(&model);
        let mut stream = provider.stream(req).await?;
        let mut out = ModelOut {
            model: model.0,
            ..ModelOut::default()
        };
        while let Some(chunk) = stream.next().await {
            match chunk? {
                ChatChunk::TextDelta(t) => {
                    self.sh.emit(
                        ctx,
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
        Ok(out)
    }

    // ---- tool step ----

    #[allow(clippy::too_many_lines)]
    async fn exec_tools(&self, run: &Run, step: &StepSpec) -> Result<Value, ExecError> {
        let ctx = self.sh.ctx(run)?;
        let calls: Vec<CallRec> =
            serde_json::from_value(step.input["calls"].clone()).map_err(xe)?;
        let hints = route_hints(&self.sh.storage.list_steps(run.id).map_err(xe)?).tools;

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
            let Some(resolved) = self
                .sh
                .resolve_call(&ctx.profile, &call.name, &input, &hints)
            else {
                slots.push(err(format!("unknown tool {:?}", call.name)));
                continue;
            };
            let meta = resolved
                .spec
                .as_ref()
                .is_some_and(|s| s.category == PermissionCategory::Meta);
            let decision = match &resolved.spec {
                Some(spec) => self.sh.decide(
                    &ctx,
                    &resolved.name,
                    spec.effect,
                    spec.category,
                    &resolved.input,
                ),
                // Malformed `use_tool`: executes nothing, only reports the error.
                None => Decision::Allow,
            };
            let is_spawn = resolved.name == crate::subagent::SPAWN_SUBAGENT;
            let spawn_input = resolved.input.clone();
            let tool = resolved.tool;
            let slot = match decision {
                Decision::Allow => Slot::Run {
                    tool,
                    input,
                    call: call_uuid,
                    meta,
                },
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
                Decision::Ask => {
                    let category = resolved
                        .spec
                        .as_ref()
                        .map_or(PermissionCategory::Script, |s| s.category);
                    match self
                        .sh
                        .request_approval(
                            &ctx,
                            call_uuid,
                            &resolved.name,
                            category,
                            &resolved.input,
                        )
                        .await
                    {
                        Ok(()) => Slot::Run {
                            tool,
                            input,
                            call: call_uuid,
                            meta,
                        },
                        Err(reason) => Slot::Done {
                            text: format!(
                                "Permission denied by the user{}. Do not retry this call.",
                                reason.map_or_else(String::new, |r| format!(": {r}"))
                            ),
                            is_error: true,
                        },
                    }
                }
            };
            let slot = match slot {
                Slot::Run { call: c, .. } if is_spawn => Slot::Spawn {
                    input: spawn_input,
                    call: c,
                    model_call_id: call.id.clone(),
                },
                other => other,
            };
            slots.push(slot);
        }

        // Reads batched by `pending_tools` run concurrently; everything else is a single call.
        let ctx_ref: &RunCtx = &ctx;
        let outcomes = futures::future::join_all(slots.into_iter().map(|slot| async move {
            match slot {
                Slot::Done { text, is_error } => (text, is_error, None),
                Slot::Spawn {
                    input,
                    call,
                    model_call_id,
                } => {
                    self.run_subagent(run, ctx_ref, call, &model_call_id, input)
                        .await
                }
                Slot::Run {
                    tool,
                    input,
                    call,
                    meta,
                } => self.run_tool(run, ctx_ref, call, &tool, input, meta).await,
            }
        }))
        .await;

        let g = &self.sh.cfg.guards;
        let mut results = Vec::with_capacity(calls.len());
        for ((call, call_uuid), (text, is_error, duration_ms)) in
            calls.iter().zip(uuids).zip(outcomes)
        {
            let text = self.sh.redact(text);
            let output_preview = preview(&text);
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
                    output_preview: Some(output_preview),
                    duration_ms,
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

    async fn run_tool(
        &self,
        run: &Run,
        ctx: &RunCtx,
        call: ToolCallId,
        tool: &Arc<dyn Tool>,
        input: Value,
        meta: bool,
    ) -> (String, bool, Option<u64>) {
        let g = &self.sh.cfg.guards;
        let tctx = ToolContext {
            run_id: run.id.to_string(),
            max_inline_bytes: g.spill_threshold_bytes,
        };
        // Approval (if any) happened before this point, so the clock only
        // covers the tool itself.
        let started = std::time::Instant::now();
        let outcome = tokio::time::timeout(g.tool_timeout, tool.call(&tctx, input)).await;
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let (text, is_error) = match outcome {
            Err(_) => (
                format!("Error: tool timed out after {:?}", g.tool_timeout),
                true,
            ),
            Ok(Err(e)) => (format!("Error: {e}"), true),
            Ok(Ok(out)) if meta => self.meta_output(ctx, call, &out).await,
            Ok(Ok(out)) => (render_output(&out), false),
        };
        (text, is_error, Some(duration_ms))
    }

    /// Turn the structured output of a `Meta` tool into events: a todo list
    /// becomes `TodoUpdated`, a question card parks the run until answered.
    async fn meta_output(
        &self,
        ctx: &RunCtx,
        call: ToolCallId,
        out: &ToolOutput,
    ) -> (String, bool) {
        let json_part = out.content.iter().find_map(|p| match p {
            ContentPart::Json { data } => Some(data),
            _ => None,
        });
        let texts = || {
            out.content
                .iter()
                .filter_map(|p| match p {
                    ContentPart::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        match json_part.and_then(|d| d["kind"].as_str().map(|k| (k, d))) {
            Some((TODO_LIST_KIND, data)) => {
                let items: Vec<builtin_tools::TodoItem> =
                    serde_json::from_value(data["items"].clone()).unwrap_or_default();
                let items: Vec<TodoItem> = items
                    .into_iter()
                    .map(|t| TodoItem {
                        text: t.content,
                        status: match t.status {
                            builtin_tools::TodoStatus::Pending => TodoStatus::Pending,
                            builtin_tools::TodoStatus::InProgress => TodoStatus::InProgress,
                            builtin_tools::TodoStatus::Completed => TodoStatus::Done,
                        },
                    })
                    .collect();
                ctx.state.lock().todos.clone_from(&items);
                self.sh.emit(
                    ctx,
                    Event::TodoUpdated(TodoUpdated {
                        run_id: ctx.run_id,
                        items,
                    }),
                );
                (texts(), false)
            }
            Some((QUESTION_CARD_KIND, data)) => {
                let question = data["question"].as_str().unwrap_or_default();
                let options: Vec<String> =
                    serde_json::from_value(data["options"].clone()).unwrap_or_default();
                match self.sh.request_answer(ctx, call, question, options).await {
                    Ok(answer) => (format!("The user answered: {answer}"), false),
                    Err(reason) => (
                        format!(
                            "The user declined to answer{}.",
                            reason.map_or_else(String::new, |r| format!(": {r}"))
                        ),
                        true,
                    ),
                }
            }
            _ => (render_output(out), false),
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
}

/// Largest `output_preview` of a `tool_call_completed` event, in bytes.
pub(crate) const OUTPUT_PREVIEW_BYTES: usize = 2048;

/// The (already redacted) start of a tool output, at most
/// [`OUTPUT_PREVIEW_BYTES`] including the truncation marker.
fn preview(text: &str) -> String {
    const MARK: &str = "\n[truncated]";
    if text.len() <= OUTPUT_PREVIEW_BYTES {
        return text.to_owned();
    }
    format!(
        "{}{MARK}",
        truncate_utf8(text, OUTPUT_PREVIEW_BYTES - MARK.len())
    )
}

/// Whether a provider failure is worth retrying (transient) as opposed to a
/// credential / request problem that would fail again.
pub(crate) fn is_retryable(e: &ProviderError) -> bool {
    match e {
        ProviderError::RateLimited { .. }
        | ProviderError::Upstream { .. }
        | ProviderError::Timeout
        | ProviderError::Connection(_)
        | ProviderError::Stream(_) => true,
        ProviderError::Api { status, .. } => matches!(*status, 408 | 429) || *status >= 500,
        _ => false,
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
