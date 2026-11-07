//! The K0-K3 decision ladder inside the loop (ADR-0005).
//!
//! One [`Ladder`] per bot, built lazily on the bot's first turn.  Its
//! candidates (saved workflows, skills, the bot's visible tools) are rebuilt
//! at the start of a turn whenever they changed (workflow saved / deleted,
//! skill written, tools registered), its embedder and classifier come from the
//! providers and are dropped on a providers reload.
//!
//! Before K4 each top-level turn asks the ladder where it should go (capped by
//! [`LoopConfig::router_budget`](crate::LoopConfig::router_budget); a timeout
//! means `Agent`):
//!
//! * `Command` `/stop` cancels the run, `/new` starts a new chat;
//! * `Workflow` is replayed by `workflow-replay` through the loop's permission
//!   gate; a handover continues with K4 and the partial state is appended to
//!   the user message;
//! * `ToolHint` / `SkillHint` narrow the tool surface / reorder the skills
//!   index of K4;
//! * `Agent` is K4 unchanged.
//!
//! The decision is emitted as [`Event::RouteDecided`] and stored in the input
//! of the run's first step (the audit trail, also what `remember` reads).

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use agent_router::ladder::{
    BackendError, Candidate, CandidateKind, Classifier, Decision, Ladder, Level, Route, RouteCache,
    RouteContext,
};
use agent_router::{FIND_TOOL, GET_TOOL_DETAILS, USE_TOOL};
use async_trait::async_trait;
use futures::StreamExt;
use parking_lot::Mutex;
use protocol::event::{MessageDelta, RouteDecided};
use protocol::{BotId, ChatId, Event, RunId};
use provider_catalog::{ChatChunk, ChatRequest, Message, MessageRole, ModelRef};
use serde_json::{Value, json};
use storage::{Chat, Run, RunStep, now_ms};
use task_runtime::{ExecError, StepSpec, StepStatus};
use workflow_replay::{Handles, ReplayOptions, Replayer, WorkflowStore, summarize};

use crate::approval::LoopGate;
use crate::exec::TurnExecutor;
use crate::history::{status_of, step_output};
use crate::profile::BotProfile;
use crate::shared::{
    LoopError, Providers, RunCtx, STEP_COMMAND, STEP_TOOLS, STEP_USER, STEP_WORKFLOW, Shared, xe,
};

/// Lifetime of a remembered route in the intent cache.
const CACHE_TTL_SECS: i64 = 30 * 24 * 3600;

/// What the loop needs to replay workflows (see `AgentLoop::with_workflows`).
pub(crate) struct WorkflowWiring {
    pub store: WorkflowStore,
    pub handles: Handles,
    pub opts: ReplayOptions,
}

/// The ladder of one bot plus the fingerprint of the candidates it holds.
pub(crate) struct BotLadder {
    ladder: Arc<Ladder>,
    fingerprint: Mutex<Option<u64>>,
}

/// Ladders per bot and the workflow wiring.
#[derive(Default)]
pub(crate) struct Router {
    pub(crate) ladders: Mutex<HashMap<BotId, Arc<BotLadder>>>,
    pub workflows: OnceLock<WorkflowWiring>,
}

impl Router {
    /// Forget every ladder (providers reloaded: embedder / classifier changed).
    pub fn reset(&self) {
        self.ladders.lock().clear();
    }

    /// Forget one bot's ladder (its profile changed).
    pub fn reset_bot(&self, bot: BotId) {
        self.ladders.lock().remove(&bot);
    }

    /// A workflow was edited or deleted: drop the routes cached for it and
    /// rebuild the candidates at the next turn.
    pub fn workflow_changed(&self, name: &str) {
        for bl in self.ladders.lock().values() {
            let _ = bl.ladder.invalidate_workflow(name);
            *bl.fingerprint.lock() = None;
        }
    }
}

/// FNV-1a, enough to notice that the candidate set changed.
fn fnv(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// K3 classifier backed by any [`ChatProvider`](provider_catalog::ChatProvider)
/// of the registry (the bot profile's `classifier` model role).
struct ProviderClassifier {
    providers: Providers,
    model: String,
}

#[async_trait]
impl Classifier for ProviderClassifier {
    async fn classify(&self, prompt: &str, max_tokens: u32) -> Result<String, BackendError> {
        let model = ModelRef(self.model.clone());
        let provider = self.providers.pick(&model);
        let mut req = ChatRequest::new(model, vec![Message::text(MessageRole::User, prompt)]);
        req.max_tokens = Some(max_tokens);
        req.temperature = Some(0.0);
        req.json_mode = true;
        let mut stream = provider
            .stream(req)
            .await
            .map_err(|e| BackendError::new(e.to_string()))?;
        let mut out = String::new();
        while let Some(chunk) = stream.next().await {
            match chunk.map_err(|e| BackendError::new(e.to_string()))? {
                ChatChunk::TextDelta(t) => out.push_str(&t),
                ChatChunk::Done { .. } => break,
                _ => {}
            }
        }
        Ok(out)
    }
}

/// What the router asked of K4 for this turn.
#[derive(Debug, Default, Clone)]
pub(crate) struct Hints {
    pub tools: Vec<String>,
    pub skills: Vec<String>,
}

/// The decision stored in a run's first step, if any.
pub(crate) fn stored_decision(steps: &[RunStep]) -> Option<Decision> {
    let first = steps.first().filter(|s| s.name == STEP_USER)?;
    serde_json::from_value(first.input.get("route")?.clone()).ok()
}

/// Tool / skill hints of the turn, from the stored decision.
pub(crate) fn route_hints(steps: &[RunStep]) -> Hints {
    match stored_decision(steps).map(|d| d.route) {
        Some(Route::ToolHint { tools }) => Hints {
            tools,
            skills: Vec::new(),
        },
        Some(Route::SkillHint { skills }) => Hints {
            tools: Vec::new(),
            skills,
        },
        _ => Hints::default(),
    }
}

/// `run_id` field of an event, when it has one.
pub(crate) fn event_run_id(ev: &Event) -> Option<RunId> {
    let v = serde_json::to_value(ev).ok()?;
    v.get("run_id")?.as_str()?.parse().ok().map(RunId)
}

/// What to do next after the routed first step(s).
pub(crate) enum RoutePlan {
    /// Not a routed turn, or nothing to add: plan as usual (K4).
    Continue,
    /// Run this step (workflow replay / command).
    Step(StepSpec),
    /// The route served the request completely.
    Done,
}

/// Note appended to the user message when the replay was cut short.
const INTERRUPTED_NOTE: &str = "\n\n[router: a saved workflow was started for this request but \
was interrupted before it finished; some of its steps may have run. Check the current state \
before continuing.]";

/// Text appended to the user message after a workflow handover (or `None`).
pub(crate) fn workflow_note(step: &RunStep) -> Option<String> {
    if status_of(step) != Some(StepStatus::Succeeded) {
        return Some(INTERRUPTED_NOTE.to_owned());
    }
    let out = step.output.as_ref()?;
    if out["completed"].as_bool().unwrap_or(false) {
        return None;
    }
    out["note"].as_str().map(|n| format!("\n\n{n}"))
}

/// Final assistant text of a run the router served itself.
pub(crate) fn routed_final_text(last: &RunStep) -> Option<String> {
    if !matches!(last.name.as_str(), STEP_WORKFLOW | STEP_COMMAND)
        || status_of(last) != Some(StepStatus::Succeeded)
    {
        return None;
    }
    last.output
        .as_ref()?
        .get("final_text")?
        .as_str()
        .map(str::to_owned)
}

fn route_json(decision: &Decision) -> Value {
    serde_json::to_value(&decision.route).unwrap_or(Value::Null)
}

fn level_name(l: Level) -> &'static str {
    match l {
        Level::K0 => "K0",
        Level::K1 => "K1",
        Level::K2 => "K2",
        Level::K3 => "K3",
        Level::K4 => "K4",
    }
}

impl Shared {
    /// The bot's ladder (built on first use); `None` when its router is off.
    pub(crate) fn ladder_for(&self, bot: BotId, profile: &BotProfile) -> Option<Arc<BotLadder>> {
        if !profile.router.enabled {
            return None;
        }
        if let Some(bl) = self.router.ladders.lock().get(&bot) {
            return Some(Arc::clone(bl));
        }
        let providers = self.providers.read().clone();
        let mut builder = Ladder::builder().config(self.cfg.ladder.clone());
        if let Some(e) = providers.embedder() {
            builder = builder.embedder(e);
        }
        if let Some(model) = profile.k3_model() {
            builder = builder.classifier(Arc::new(ProviderClassifier {
                providers,
                model: model.to_owned(),
            }));
        }
        let dir = self.storage.data_dir().join("router");
        if std::fs::create_dir_all(&dir).is_ok()
            && let Ok(cache) = RouteCache::open(dir.join(format!("{bot}.sqlite")), CACHE_TTL_SECS)
        {
            builder = builder.cache(cache);
        }
        let bl = Arc::new(BotLadder {
            ladder: Arc::new(builder.build()),
            fingerprint: Mutex::new(None),
        });
        Some(Arc::clone(
            self.router.ladders.lock().entry(bot).or_insert(bl),
        ))
    }

    /// Everything the ladder may route to, as the bot sees it.
    pub(crate) fn router_candidates(&self, profile: &BotProfile) -> Vec<Candidate> {
        let mut out = Vec::new();
        if let Some(w) = self.router.workflows.get()
            && let Ok(entries) = w.store.list()
        {
            for e in entries {
                let Ok(wf) = w.store.get(&e.name) else {
                    continue;
                };
                let version = wf.to_toml().map_or_else(
                    |_| e.updated_at_ms.to_string(),
                    |t| format!("{:016x}", fnv(t.as_bytes())),
                );
                let mut c = Candidate::new(CandidateKind::Workflow, &wf.name, &wf.description)
                    .with_version(&version);
                for p in &wf.params {
                    c = c.with_param(&p.name, p.default.is_none());
                }
                out.push(c);
            }
        }
        if let Some(index) = self.skills.get() {
            for s in index.list() {
                let version = format!("{}:{:016x}", s.version, fnv(s.body.as_bytes()));
                out.push(
                    Candidate::new(CandidateKind::Skill, &s.name, &s.description)
                        .with_version(&version),
                );
            }
        }
        for spec in self.visible_specs(profile) {
            if matches!(spec.name.as_str(), FIND_TOOL | GET_TOOL_DETAILS | USE_TOOL) {
                continue;
            }
            let mut c = Candidate::new(CandidateKind::Tool, &spec.name, &spec.description);
            let required: Vec<&str> = spec
                .input_schema
                .get("required")
                .and_then(serde_json::Value::as_array)
                .map(|r| r.iter().filter_map(serde_json::Value::as_str).collect())
                .unwrap_or_default();
            if let Some(props) = spec
                .input_schema
                .get("properties")
                .and_then(serde_json::Value::as_object)
            {
                for name in props.keys() {
                    c = c.with_param(name, required.contains(&name.as_str()));
                }
            }
            out.push(c);
        }
        out
    }

    /// Rebuild the ladder's candidates when they changed (embeddings follow in
    /// the background; K2 uses BM25 only until they are there).
    pub(crate) fn refresh_ladder(&self, bl: &Arc<BotLadder>, profile: &BotProfile, embed: bool) {
        let candidates = self.router_candidates(profile);
        let fp = fnv(&serde_json::to_vec(&candidates).unwrap_or_default());
        {
            let mut cur = bl.fingerprint.lock();
            if *cur == Some(fp) {
                return;
            }
            *cur = Some(fp);
        }
        let count = candidates.len();
        bl.ladder.set_candidates(candidates);
        if embed && let Ok(handle) = tokio::runtime::Handle::try_current() {
            let ladder = Arc::clone(&bl.ladder);
            let task = handle.spawn(async move {
                if let Err(e) = ladder.refresh_embeddings().await {
                    tracing::warn!(
                        candidates = count,
                        error = %e,
                        "K2 embedding refresh failed; routing falls back to BM25"
                    );
                }
            });
            let mut bg = self.bg.lock();
            bg.retain(|h| !h.is_finished());
            bg.push(task);
        }
    }

    /// `POST /v1/runs/{id}/remember`: cache the route a succeeded run took (or
    /// the workflow its model ran) for the same request text.
    pub(crate) fn remember_route(&self, run_id: RunId) -> Result<Route, LoopError> {
        self.remember_route_inner(run_id, true)
    }

    /// The checks of [`Self::remember_route`] without caching anything.
    pub(crate) fn can_remember(&self, run_id: RunId) -> bool {
        self.remember_route_inner(run_id, false).is_ok()
    }

    fn remember_route_inner(&self, run_id: RunId, commit: bool) -> Result<Route, LoopError> {
        let run = self
            .storage
            .get_run(run_id)?
            .ok_or_else(|| LoopError::NotFound(format!("run {run_id}")))?;
        let refuse = |m: &str| Err(LoopError::Router(m.to_owned()));
        if run.parent_run_id.is_some() {
            return refuse("subagent runs cannot be remembered");
        }
        if run.state != task_runtime::RunState::Succeeded.as_str() {
            return refuse("only a succeeded run can be remembered");
        }
        let chat_id = run
            .chat_id
            .ok_or_else(|| LoopError::Router("the run has no chat".into()))?;
        // Cheap checks first: the profile is cached, and the router flag
        // needs no ladder (a read-only probe must not create one).
        let profile = self.profile_for(run.bot_id).map_err(LoopError::Profile)?;
        if !profile.router.enabled {
            return refuse("the router is disabled for this bot");
        }
        let text = self
            .storage
            .list_messages(chat_id)?
            .into_iter()
            .find(|m| m.run_id == Some(run_id) && m.role == "user")
            .map(|m| m.content)
            .ok_or_else(|| LoopError::Router("the run has no user message".into()))?;
        let steps = self.storage.list_steps(run_id)?;
        let route = match stored_decision(&steps) {
            Some(d)
                if matches!(
                    d.route,
                    Route::Workflow { .. } | Route::ToolHint { .. } | Route::SkillHint { .. }
                ) =>
            {
                d.route
            }
            decision => match workflow_run_by_model(&steps) {
                Some((name, params)) => Route::Workflow {
                    name,
                    params,
                    confidence: decision.map_or(1.0, |d| d.confidence.max(0.9)),
                },
                None => return refuse("the run followed no route and ran no workflow"),
            },
        };
        if commit {
            let bl = self
                .ladder_for(run.bot_id, &profile)
                .ok_or_else(|| LoopError::Router("the router is disabled for this bot".into()))?;
            self.refresh_ladder(&bl, &profile, false);
            bl.ladder
                .remember_route(&text, &route)
                .map_err(|e| LoopError::Router(e.to_string()))?;
        }
        Ok(route)
    }
}

/// The first successful `workflow_run` call of the model in a run.
fn workflow_run_by_model(steps: &[RunStep]) -> Option<(String, BTreeMap<String, String>)> {
    for step in steps.iter().filter(|s| s.name == STEP_TOOLS) {
        let calls = step.input["calls"].as_array()?;
        let results = step.output.as_ref().map(|o| &o["results"]);
        for call in calls {
            if call["name"] != workflow_replay::RUN_TOOL_NAME {
                continue;
            }
            let ok = results.and_then(Value::as_array).is_some_and(|rs| {
                rs.iter()
                    .any(|r| r["id"] == call["id"] && r["is_error"] == false)
            });
            if !ok {
                continue;
            }
            let args: Value = serde_json::from_str(call["arguments"].as_str()?).ok()?;
            let name = args["name"].as_str()?.to_owned();
            let params = args["params"]
                .as_object()
                .map(|m| {
                    m.iter()
                        .map(|(k, v)| {
                            (
                                k.clone(),
                                v.as_str().map_or_else(|| v.to_string(), str::to_owned),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            return Some((name, params));
        }
    }
    None
}

impl TurnExecutor {
    /// Decide how a fresh top-level turn starts and emit `route_decided`.
    /// `None` for subagents and bots with the router off.
    pub(crate) async fn route_first(&self, ctx: &RunCtx, text: &str) -> Option<Decision> {
        if ctx.sub.is_some() || ctx.parent_run_id.is_some() {
            return None;
        }
        let bl = self.sh.ladder_for(ctx.bot_id, &ctx.profile)?;
        self.sh.refresh_ladder(&bl, &ctx.profile, true);
        let budget: Duration = self.sh.cfg.router_budget;
        let decision =
            match tokio::time::timeout(budget, bl.ladder.decide(&RouteContext::new(text))).await {
                Ok(d) => d,
                Err(_) => Decision {
                    route: Route::Agent,
                    level: Level::K4,
                    confidence: 0.0,
                    latency_ms: u64::try_from(budget.as_millis()).unwrap_or(u64::MAX),
                    reason: format!("router budget of {budget:?} exceeded"),
                },
            };
        self.emit_route(ctx, &decision);
        Some(decision)
    }

    fn emit_route(&self, ctx: &RunCtx, decision: &Decision) {
        self.sh.emit(
            ctx,
            Event::RouteDecided(RouteDecided {
                run_id: ctx.run_id,
                level: level_name(decision.level).to_owned(),
                route: route_json(decision),
                confidence: decision.confidence,
                latency_ms: decision.latency_ms,
                reason: decision.reason.clone(),
            }),
        );
    }

    /// The first step of a schedule-started workflow / tool run: the schedule
    /// itself is the decision (no ladder, no model call).  A workflow is
    /// replayed by the usual routed step; a tool call is recorded as a
    /// `directive` in the user step and planned by `plan_next`.
    pub(crate) fn scheduled_first_step(
        &self,
        ctx: &RunCtx,
        raw: &str,
        work: crate::shared::ScheduleWork,
    ) -> Option<StepSpec> {
        use crate::shared::ScheduleWork;
        match work {
            ScheduleWork::Prompt(_) => None,
            ScheduleWork::Workflow { name, params } => {
                let decision = Decision {
                    route: Route::Workflow {
                        name,
                        params,
                        confidence: 1.0,
                    },
                    level: Level::K0,
                    confidence: 1.0,
                    latency_ms: 0,
                    reason: "started by a schedule".to_owned(),
                };
                self.emit_route(ctx, &decision);
                let mut spec = StepSpec {
                    name: STEP_USER.to_owned(),
                    input: json!({ "text": raw }),
                    idempotent: true,
                    retry: None,
                };
                spec.input["route"] = serde_json::to_value(&decision).unwrap_or(Value::Null);
                Some(spec)
            }
            ScheduleWork::Tool { name, input } => {
                let mut spec = StepSpec {
                    name: STEP_USER.to_owned(),
                    input: json!({ "text": raw }),
                    idempotent: true,
                    retry: None,
                };
                spec.input["directive"] = json!({ "kind": "tool", "name": name, "input": input });
                Some(spec)
            }
        }
    }

    /// The step a schedule-started tool run takes after its user step, or
    /// `Done` once the tool step exists (it is never run twice, even after an
    /// interruption).
    pub(crate) fn directive_plan(history: &[RunStep]) -> Option<RoutePlan> {
        let first = history.first().filter(|s| s.name == STEP_USER)?;
        let d = first.input.get("directive")?;
        if d["kind"] != "tool" {
            return None;
        }
        if history.len() > 1 {
            return Some(RoutePlan::Done);
        }
        if status_of(first) != Some(StepStatus::Succeeded) {
            return None;
        }
        let call = crate::shared::CallRec {
            id: "scheduled".to_owned(),
            name: d["name"].as_str().unwrap_or_default().to_owned(),
            arguments: d["input"].to_string(),
            repeat: 1,
        };
        Some(RoutePlan::Step(StepSpec {
            name: STEP_TOOLS.to_owned(),
            input: json!({ "calls": [call] }),
            idempotent: false,
            retry: None,
        }))
    }

    /// `Command` `/stop`: cancel the run that carries it.
    pub(crate) fn stop_run(&self, run: &Run) {
        if let Some(rt) = self.sh.rt.get().and_then(std::sync::Weak::upgrade) {
            let _ = rt.cancel(run.id);
        }
    }

    /// The step a routed turn runs after its user step.
    pub(crate) fn route_plan(history: &[RunStep], steer_empty: bool) -> RoutePlan {
        let Some(decision) = stored_decision(history) else {
            return RoutePlan::Continue;
        };
        let routed = history
            .iter()
            .find(|s| s.name == STEP_WORKFLOW || s.name == STEP_COMMAND);
        match (&decision.route, routed) {
            (Route::Workflow { name, params, .. }, None)
                if history.len() == 1 && status_of(&history[0]) == Some(StepStatus::Succeeded) =>
            {
                RoutePlan::Step(StepSpec {
                    name: STEP_WORKFLOW.to_owned(),
                    input: json!({ "name": name, "params": params, "level": level_name(decision.level) }),
                    // A replay acts on the world: never run it twice.
                    idempotent: false,
                    retry: None,
                })
            }
            (Route::Command(c), None)
                if c.name == "new"
                    && history.len() == 1
                    && status_of(&history[0]) == Some(StepStatus::Succeeded) =>
            {
                RoutePlan::Step(StepSpec {
                    name: STEP_COMMAND.to_owned(),
                    input: json!({ "name": c.name, "args": c.args }),
                    idempotent: false,
                    retry: None,
                })
            }
            (_, Some(step))
                if history.last().is_some_and(|l| std::ptr::eq(l, step))
                    && status_of(step) == Some(StepStatus::Succeeded)
                    && steer_empty =>
            {
                let done = step.name == STEP_COMMAND
                    || step_output::<Value>(step).is_some_and(|o| o["completed"] == true);
                if done {
                    RoutePlan::Done
                } else {
                    RoutePlan::Continue
                }
            }
            _ => RoutePlan::Continue,
        }
    }

    /// Replay the routed workflow through the loop's permission gate.  Never
    /// fails the run: a replay that cannot start or stops early hands over to
    /// K4 with a note.
    pub(crate) async fn exec_workflow(
        &self,
        run: &Run,
        step: &StepSpec,
    ) -> Result<Value, ExecError> {
        let ctx = self.sh.ctx(run)?;
        let name = step.input["name"].as_str().unwrap_or_default().to_owned();
        let level = step.input["level"].as_str().unwrap_or("K0").to_owned();
        let params: serde_json::Map<String, Value> = step.input["params"]
            .as_object()
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        let fail = |why: String| {
            json!({
                "completed": false,
                "workflow": name,
                "note": format!(
                    "[router: the saved workflow `{name}` matched this request ({level}) but \
                     could not be replayed: {why}. Do the task yourself.]"
                ),
            })
        };
        let Some(w) = self.sh.router.workflows.get() else {
            return Ok(fail("workflows are not available".into()));
        };
        let wf = match w.store.get(&name) {
            Ok(wf) => wf,
            Err(e) => return Ok(fail(e.to_string())),
        };
        let mut opts = w.opts.clone();
        opts.run_id = run.id.to_string();
        let gate = LoopGate {
            sh: Arc::downgrade(&self.sh),
        };
        let report = match Replayer::new(w.handles.clone(), opts)
            .replay(&wf, &params, &gate)
            .await
        {
            Ok(r) => r,
            Err(e) => return Ok(fail(e.to_string())),
        };
        let summary = summarize(&report);
        if report.is_completed() {
            self.sh.emit(
                &ctx,
                Event::MessageDelta(MessageDelta {
                    run_id: run.id,
                    text: summary.clone(),
                }),
            );
            return Ok(json!({
                "completed": true,
                "workflow": name,
                "final_text": summary,
                "steps_replayed": report.steps_replayed,
                "wall_time_ms": report.wall_time_ms,
            }));
        }
        Ok(json!({
            "completed": false,
            "workflow": name,
            "note": format!(
                "[router: the saved workflow `{name}` matched this request ({level}) and was \
                 replayed automatically, but it stopped early. Continue from there; do not \
                 redo the steps that were completed.]\n{summary}"
            ),
        }))
    }

    /// Run a built-in command step (`/new`).
    pub(crate) fn exec_command(&self, run: &Run, step: &StepSpec) -> Result<Value, ExecError> {
        let ctx = self.sh.ctx(run)?;
        let text = match step.input["name"].as_str() {
            Some("new") => {
                let chat = Chat {
                    id: ChatId::new(),
                    user_id: run.user_id,
                    bot_id: run.bot_id,
                    title: "New chat".to_owned(),
                    created_at_ms: now_ms(),
                };
                self.sh.storage.insert_chat(&chat).map_err(xe)?;
                self.sh.audit(
                    &ctx,
                    "bot",
                    "chat.new_from_command",
                    &json!({ "chat_id": chat.id.to_string() }),
                );
                format!("Started a new chat: {}", chat.id)
            }
            other => return Err(xe(format!("unknown command {other:?}"))),
        };
        self.sh.emit(
            &ctx,
            Event::MessageDelta(MessageDelta {
                run_id: run.id,
                text: text.clone(),
            }),
        );
        Ok(json!({ "final_text": text }))
    }
}
