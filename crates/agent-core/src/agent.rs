//! The public [`AgentLoop`] handle.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

use agent_router::{ToolSurface, ToolVisibility};
use builtin_tools::BuiltinConfig;
use futures::future::BoxFuture;
use parking_lot::{Mutex, RwLock};
use permission_engine::{PermissionResolver, Policy};
use protocol::event::{RunError, RunFinished, RunStarted};
use protocol::{
    BotId, ChatId, Event, InitiatorKind, PermissionCategory, RunId, TaskId, ToolCallId, UserId,
};
use storage::{Message as DbMessage, Storage, now_ms};
use task_runtime::{RunState, Runtime, RuntimeError, StepStatus};
use tokio::sync::broadcast;
use tool_registry::ToolRegistry;
use uuid::Uuid;

use crate::approval::{LoopGate, grant_detail, grant_target};
use crate::config::{LoopConfig, ModelPrice};
use crate::control::{ControlGate, LoopControl};
use crate::exec::TurnExecutor;
use crate::history::{status_of, step_output};
use crate::profile::BotProfile;
use crate::shared::{
    ApprovalScope, CategoryDecision, Delivery, EffectivePolicy, Grant, GrantTarget, Grants,
    LoopError, MessageMode, ModelOut, Pending, Providers, STEP_MODEL, STEP_TOOLS, ScheduleWork,
    Shared, StreamItem, Verdict, initiator_of, kind_for, kind_for_schedule,
};
use crate::{AUDIT_GRANT_ALWAYS, AUDIT_GRANT_REVOKED};

type Result<T> = std::result::Result<T, LoopError>;

#[derive(Clone, Copy)]
pub(crate) enum Start {
    Fresh,
    Resume,
}

/// Runs bot turns: model -> tool calls -> permission -> execute -> repeat.
///
/// Cheap to clone (shared handles).  Spawning methods must be called inside a
/// Tokio runtime.
#[derive(Clone)]
pub struct AgentLoop {
    pub(crate) sh: Arc<Shared>,
    pub(crate) rt: Arc<Runtime>,
}

impl AgentLoop {
    /// Build a loop and reload the persisted "always allow" grant rules.
    /// Use [`Self::with_builtin_tools`] to register the built-in tool set.
    #[must_use]
    pub fn new(
        storage: Arc<Storage>,
        tools: ToolRegistry,
        providers: Providers,
        cfg: LoopConfig,
    ) -> Self {
        let (events, _) = broadcast::channel(4096);
        let first_gseq = storage.last_gseq().unwrap_or(0);
        // The discovery meta-tools only reveal what the calling bot may see.
        let sh = Arc::new_cyclic(|weak: &Weak<Shared>| {
            let weak = weak.clone();
            let visibility: ToolVisibility = Arc::new(move |ctx, spec| {
                weak.upgrade()
                    .is_some_and(|sh| sh.tool_visible_to_run(&ctx.run_id, spec))
            });
            Shared {
                storage: Arc::clone(&storage),
                surface: ToolSurface::with_visibility(
                    tools.clone(),
                    cfg.discovery_threshold,
                    visibility,
                ),
                tools,
                providers: RwLock::new(providers),
                global_policy: RwLock::new(cfg.global_policy.clone()),
                control: RwLock::new(None),
                resolver: PermissionResolver::new(),
                cfg,
                profiles: RwLock::new(HashMap::new()),
                events,
                runs: Mutex::new(HashMap::new()),
                pending: Mutex::new(HashMap::new()),
                grants: Mutex::new(load_grants(&storage)),
                active: Mutex::new(HashMap::new()),
                queued: Mutex::new(HashMap::new()),
                tasks: Mutex::new(HashMap::new()),
                bg: Mutex::new(Vec::new()),
                gseq: AtomicU64::new(first_gseq),
                emit_gate: Mutex::new(()),
                shutting_down: AtomicBool::new(false),
                rt: OnceLock::new(),
                spawn_gate: Mutex::new(()),
                skills: OnceLock::new(),
                skill_hints: RwLock::new(None),
                router: crate::router::Router::default(),
                directives: Mutex::new(HashMap::new()),
                approval_paused: Mutex::new(HashSet::new()),
            }
        });
        let rt = Arc::new(Runtime::new(
            storage,
            Arc::new(TurnExecutor {
                sh: Arc::clone(&sh),
            }),
        ));
        let _ = sh.rt.set(Arc::downgrade(&rt));
        Self { sh, rt }
    }

    pub(crate) fn from_parts(sh: Arc<Shared>, rt: Arc<Runtime>) -> Self {
        Self { sh, rt }
    }

    /// Register `spawn_subagent` (see the `subagent` module): the model can
    /// delegate tasks to child runs of its own bot.
    #[must_use]
    pub fn with_subagents(self) -> Self {
        let _ = self.sh.tools.register(crate::subagent::tool());
        self
    }

    /// Register the `skill_*` tools over `index` and put the skills index
    /// (suggested skills first, see [`skills::SkillIndex::prompt_block`]) into
    /// the system prompt of every model call.
    #[must_use]
    pub fn with_skills(self, index: skills::SkillIndex) -> Self {
        for tool in skills::tools(&index) {
            let _ = self.sh.tools.register(tool);
        }
        let _ = self.sh.skills.set(index);
        self
    }

    /// Tell the loop what is in the foreground (browser page host, application)
    /// so skills that match it are listed as suggested; without a source the
    /// turn's text is matched against skill keywords instead.
    pub fn set_skill_hints(&self, hints: crate::SkillHintFn) {
        *self.sh.skill_hints.write() = Some(hints);
    }

    /// Register the built-in tools (desktop, browser, files, shell, script,
    /// `ask_user`, `todo_write`) into the loop's registry.  The config's
    /// [`ToolGate`](builtin_tools::ToolGate) is replaced by the loop's own, so
    /// tool calls made from `script_run` go through the same resolver and
    /// interactive approval flow as direct calls.
    #[must_use]
    pub fn with_builtin_tools(self, mut cfg: BuiltinConfig) -> Self {
        cfg.gate = Arc::new(LoopGate {
            sh: Arc::downgrade(&self.sh),
        });
        cfg.control = Some(Arc::new(LoopControl {
            sh: Arc::downgrade(&self.sh),
        }));
        let _ = builtin_tools::register_builtin_tools(&self.sh.tools, cfg);
        self
    }

    /// Register extra tools that need the loop's permission gate (for example
    /// `workflow_run`, whose inner steps must pass the same approval flow as
    /// direct calls). `build` receives that gate.
    #[must_use]
    pub fn with_gated_tools<F>(self, build: F) -> Self
    where
        F: FnOnce(Arc<dyn builtin_tools::ToolGate>) -> Vec<Arc<dyn tool_registry::Tool>>,
    {
        let gate: Arc<dyn builtin_tools::ToolGate> = Arc::new(LoopGate {
            sh: Arc::downgrade(&self.sh),
        });
        for tool in build(gate) {
            let _ = self.sh.tools.register(tool);
        }
        self
    }

    /// Install the desktop control gate (the daemon registers its bridge to the
    /// desktop app here); see [`ControlGate`].
    pub fn set_control_gate(&self, gate: Arc<dyn ControlGate>) {
        *self.sh.control.write() = Some(gate);
    }

    /// Names of the registered tools, sorted.
    #[must_use]
    pub fn tool_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.sh.tools.list().into_iter().map(|s| s.name).collect();
        names.sort();
        names
    }

    /// The loop's providers (cheap clone).
    #[must_use]
    pub fn providers(&self) -> Providers {
        self.sh.providers.read().clone()
    }

    /// Replace the providers (after `POST /v1/providers/reload`); runs already
    /// streaming keep the provider they started with.
    pub fn set_providers(&self, providers: Providers) {
        *self.sh.providers.write() = providers;
        // The K2 embedder and the K3 classifier come from the providers.
        self.sh.router.reset();
    }

    /// The workspace-wide default permission policy.
    #[must_use]
    pub fn global_policy(&self) -> Policy {
        self.sh.global_policy.read().clone()
    }

    /// Replace the workspace-wide default policy; takes effect for the next
    /// tool call of every run.
    pub fn set_global_policy(&self, policy: Policy) {
        *self.sh.global_policy.write() = policy;
    }

    /// USD price of `model` (full `provider/model` reference): the loop config's
    /// override, else the price the providers' catalog carries.
    #[must_use]
    pub fn price_for(&self, model: &str) -> Option<ModelPrice> {
        self.sh.price_for(model)
    }

    /// Register the profile used by `bot_id` (otherwise the bot row's JSON
    /// `config` is parsed as a profile).
    pub fn add_profile(&self, bot_id: BotId, profile: BotProfile) {
        self.sh.profiles.write().insert(bot_id, Arc::new(profile));
        self.sh.router.reset_bot(bot_id);
    }

    /// Wire saved workflows into the loop: registers `workflow_run` /
    /// `workflow_list` (their inner steps pass the loop's approval flow), lets
    /// the decision ladder route requests to workflows and replay them without
    /// a model call, and forwards the replay's events to the run's stream.
    /// `handles.tools` is replaced by the loop's own registry.
    #[must_use]
    pub fn with_workflows(
        self,
        store: workflow_replay::WorkflowStore,
        mut handles: workflow_replay::Handles,
    ) -> Self {
        handles.tools = self.sh.tools.clone();
        let weak = Arc::downgrade(&self.sh);
        let opts = workflow_replay::ReplayOptions {
            events: Some(Arc::new(move |ev: Event| {
                let Some(sh) = weak.upgrade() else { return };
                let Some(run_id) = crate::router::event_run_id(&ev) else {
                    return;
                };
                let ctx = sh.runs.lock().get(&run_id).cloned();
                if let Some(ctx) = ctx {
                    sh.emit(&ctx, ev);
                }
            })),
            ..workflow_replay::ReplayOptions::default()
        };
        let (s2, h2, o2) = (store.clone(), handles.clone(), opts.clone());
        let this = self.with_gated_tools(|gate| workflow_replay::tools(s2, h2, gate, o2));
        let _ = this.sh.router.workflows.set(crate::router::WorkflowWiring {
            store,
            handles,
            opts,
        });
        this
    }

    /// A saved workflow was edited or deleted: drop the routes cached for it
    /// (K1) and rebuild the router's candidates at the next turn.
    pub fn workflow_changed(&self, name: &str) {
        self.sh.router.workflow_changed(name);
    }

    /// Cache the route a succeeded run took (`POST /v1/runs/{id}/remember`):
    /// the same request text then goes straight there (K1).  A run whose model
    /// ran a saved workflow (`workflow_run`) remembers that workflow.  Returns
    /// the remembered route as JSON (tagged by `kind`).
    ///
    /// # Errors
    /// [`LoopError::NotFound`] for an unknown run, [`LoopError::Router`] when
    /// the run is not eligible (not succeeded, a subagent, no route or
    /// workflow, router disabled) or the cache refuses it.
    pub fn remember_route(&self, run_id: RunId) -> Result<serde_json::Value> {
        let route = self.sh.remember_route(run_id)?;
        Ok(serde_json::to_value(route).unwrap_or(serde_json::Value::Null))
    }

    /// Whether [`AgentLoop::remember_route`] would accept `run_id` (same
    /// checks, nothing is cached).
    #[must_use]
    pub fn can_remember(&self, run_id: RunId) -> bool {
        self.sh.can_remember(run_id)
    }

    /// Subscribe to the live event stream of all runs (events are also
    /// appended to each run's NDJSON log).
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<StreamItem> {
        self.sh.events.subscribe()
    }

    /// The durable runtime (state queries, etc.).
    #[must_use]
    pub fn runtime(&self) -> &Arc<Runtime> {
        &self.rt
    }

    /// Start a turn for a human message and return its run id immediately;
    /// the turn runs in the background.
    ///
    /// # Errors
    /// Fails if the chat / bot profile is missing, the chat already has an
    /// active turn ([`LoopError::Busy`]) or storage fails.
    pub fn run_turn(&self, chat_id: ChatId, user_message: &str) -> Result<RunId> {
        self.run_turn_as(chat_id, user_message, InitiatorKind::Human)
    }

    /// Like [`Self::run_turn`] with an explicit initiator kind (policy rules
    /// can depend on it, ADR-0012).
    ///
    /// # Errors
    /// As [`Self::run_turn`].
    pub fn run_turn_as(
        &self,
        chat_id: ChatId,
        user_message: &str,
        initiator: InitiatorKind,
    ) -> Result<RunId> {
        self.start_turn(chat_id, user_message, &kind_for(initiator), None)
    }

    /// Start a run on behalf of schedule `schedule_id` (initiator
    /// [`InitiatorKind::Schedule`]) and return its run id; the run goes on in
    /// the background.  `work` says what it does: a normal prompt turn, a saved
    /// workflow replayed through the loop's permission gate, or one tool call
    /// through the same gate.  Approvals apply as for any run; when nobody
    /// answers within [`LoopConfig::schedule_approval_timeout`] the run
    /// pauses (resumable with [`Self::resume`]).  `RunStarted` carries the
    /// `schedule_id`.
    ///
    /// # Errors
    /// As [`Self::run_turn`].
    pub fn run_scheduled(
        &self,
        chat_id: ChatId,
        schedule_id: TaskId,
        work: ScheduleWork,
    ) -> Result<RunId> {
        let text = match &work {
            ScheduleWork::Prompt(t) => t.clone(),
            ScheduleWork::Workflow { name, params } => {
                format!("Scheduled: run the saved workflow `{name}` with {params:?}.")
            }
            ScheduleWork::Tool { name, input } => {
                format!("Scheduled: call the tool `{name}` with {input}.")
            }
        };
        let directive = (!matches!(work, ScheduleWork::Prompt(_))).then_some(work);
        self.start_turn(chat_id, &text, &kind_for_schedule(schedule_id), directive)
    }

    fn start_turn(
        &self,
        chat_id: ChatId,
        user_message: &str,
        kind: &str,
        directive: Option<ScheduleWork>,
    ) -> Result<RunId> {
        if self.sh.shutting_down.load(Ordering::SeqCst) {
            return Err(LoopError::ShuttingDown);
        }
        let chat = self
            .sh
            .storage
            .get_chat(chat_id)?
            .ok_or_else(|| LoopError::NotFound(format!("chat {chat_id}")))?;
        self.sh
            .profile_for(chat.bot_id)
            .map_err(LoopError::Profile)?;
        let run_id = {
            let mut active = self.sh.active.lock();
            if active.contains_key(&chat_id) {
                return Err(LoopError::Busy(chat_id));
            }
            let run_id = self
                .rt
                .create_run(chat.user_id, chat.bot_id, Some(chat_id), kind)?;
            self.insert_user_message(chat.user_id, chat_id, run_id, user_message)?;
            if let Some(d) = directive {
                self.sh.directives.lock().insert(run_id, d);
            }
            active.insert(chat_id, run_id);
            run_id
        };
        self.spawn(run_id, Start::Fresh);
        Ok(run_id)
    }

    /// Route a user message: starts a turn when the chat is idle; otherwise
    /// `Steer` injects it before the running turn's next model call and
    /// `Queue` runs it as the next turn (07 #1).
    ///
    /// # Errors
    /// As [`Self::run_turn`] when a new turn has to start.
    pub fn send_user_message(
        &self,
        chat_id: ChatId,
        text: &str,
        mode: MessageMode,
    ) -> Result<Delivery> {
        if let Some(run_id) = self.stop_command_target(chat_id, text) {
            self.cancel(run_id)?;
            return Ok(Delivery::Stopped(run_id));
        }
        {
            let active = self.sh.active.lock();
            if let Some(&run_id) = active.get(&chat_id) {
                if mode == MessageMode::Steer {
                    let ctx = self.sh.runs.lock().get(&run_id).cloned();
                    if let Some(ctx) = ctx {
                        let mut st = ctx.state.lock();
                        if !st.closed {
                            st.steer.push_back(text.to_owned());
                            drop(st);
                            self.insert_user_message(ctx.user_id, chat_id, run_id, text)?;
                            return Ok(Delivery::Steered(run_id));
                        }
                    }
                }
                self.sh
                    .queued
                    .lock()
                    .entry(chat_id)
                    .or_default()
                    .push_back(text.to_owned());
                return Ok(Delivery::Queued);
            }
        }
        Ok(Delivery::NewTurn(self.run_turn(chat_id, text)?))
    }

    /// The running turn of `chat_id` when `text` is the `/stop` command and
    /// the bot's router is enabled (K0 is deterministic: no ladder needed).
    fn stop_command_target(&self, chat_id: ChatId, text: &str) -> Option<RunId> {
        let run_id = *self.sh.active.lock().get(&chat_id)?;
        let is_stop = matches!(
            agent_router::ladder::rules::slash_command(text),
            Some(agent_router::ladder::Route::Command(c)) if c.name == "stop"
        );
        if !is_stop {
            return None;
        }
        let chat = self.sh.storage.get_chat(chat_id).ok().flatten()?;
        let profile = self.sh.profile_for(chat.bot_id).ok()?;
        profile.router.enabled.then_some(run_id)
    }

    /// Approve a pending tool call (the id from `ApprovalNeeded`).
    ///
    /// # Errors
    /// [`LoopError::NoPendingApproval`] if it is not waiting (already decided
    /// or lost in a restart).
    pub fn approve(&self, call_id: ToolCallId, scope: ApprovalScope, by: UserId) -> Result<()> {
        self.take_pending(call_id)?
            .tx
            .send(Verdict::Approved { scope, by })
            .map_err(|_| LoopError::NoPendingApproval(call_id))
    }

    /// Deny a pending tool call; the model receives a denial message.
    ///
    /// # Errors
    /// As [`Self::approve`].
    pub fn deny(&self, call_id: ToolCallId, by: UserId, reason: Option<String>) -> Result<()> {
        self.take_pending(call_id)?
            .tx
            .send(Verdict::Denied { by, reason })
            .map_err(|_| LoopError::NoPendingApproval(call_id))
    }

    /// Answer a pending question card (the id from `QuestionAsked`).
    ///
    /// # Errors
    /// [`LoopError::NoPendingApproval`] if no question is waiting under that id.
    pub fn answer_question(&self, call_id: ToolCallId, answer: &str) -> Result<()> {
        self.take_pending(call_id)?
            .tx
            .send(Verdict::Answered {
                answer: answer.to_owned(),
            })
            .map_err(|_| LoopError::NoPendingApproval(call_id))
    }

    /// The persisted "always allow" rules of a bot.
    #[must_use]
    pub fn list_grants(&self, bot_id: BotId) -> Vec<Grant> {
        self.sh
            .grants
            .lock()
            .always
            .iter()
            .filter(|g| g.bot_id == bot_id)
            .cloned()
            .collect()
    }

    /// Remove one "always allow" rule (audited).  Returns whether it existed.
    ///
    /// # Errors
    /// Fails if the bot is unknown or the audit entry cannot be written.
    pub fn revoke_grant(&self, bot_id: BotId, target: &GrantTarget) -> Result<bool> {
        let bot = self
            .sh
            .storage
            .get_bot(bot_id)?
            .ok_or_else(|| LoopError::NotFound(format!("bot {bot_id}")))?;
        let removed = {
            let mut g = self.sh.grants.lock();
            let before = g.always.len();
            g.always
                .retain(|x| !(x.bot_id == bot_id && x.target == *target));
            g.always.len() != before
        };
        if removed {
            let probe = Grant {
                bot_id,
                target: target.clone(),
                granted_at_ms: 0,
            };
            self.sh.storage.append_audit(
                bot.user_id,
                None,
                "user",
                AUDIT_GRANT_REVOKED,
                &grant_detail(&probe),
            )?;
        }
        Ok(removed)
    }

    /// Whether a guard (step limit, token budget, repeat detector) ended the
    /// run through the graceful closing call.
    ///
    /// # Errors
    /// Fails on storage errors.
    pub fn stopped_by_guard(&self, run_id: RunId) -> Result<bool> {
        Ok(self
            .sh
            .storage
            .list_steps(run_id)?
            .iter()
            .filter(|s| s.name == STEP_MODEL)
            .filter_map(step_output::<ModelOut>)
            .any(|m| m.stopped_by_guard))
    }

    /// Wait for background work (memory extraction) started so far.
    pub async fn flush_background(&self) {
        let handles: Vec<_> = self.sh.bg.lock().drain(..).collect();
        for h in handles {
            let _ = h.await;
        }
    }

    fn take_pending(&self, call_id: ToolCallId) -> Result<Pending> {
        self.sh
            .pending
            .lock()
            .remove(&call_id)
            .ok_or(LoopError::NoPendingApproval(call_id))
    }

    /// Cancel a run (in-flight step becomes `Interrupted`); unfinished
    /// subagents of the run are cancelled too.
    ///
    /// # Errors
    /// Fails if the run is terminal or storage fails.
    pub fn cancel(&self, run_id: RunId) -> Result<()> {
        self.rt.cancel(run_id)?;
        // A cancelled parent takes its subagents with it.
        self.cancel_descendants(run_id);
        Ok(())
    }

    /// Startup recovery: every run left `Running` **or** `WaitingApproval` gets
    /// its unfinished steps marked `Interrupted` and becomes `Paused`;
    /// dangling approvals expire.  Nothing is replayed or resumed.
    ///
    /// # Errors
    /// Fails on storage errors.
    pub fn recover(&self) -> Result<Vec<RunId>> {
        let mut ids = self.rt.recover()?;
        let st = &self.sh.storage;
        for run in st.list_runs_by_state(RunState::WaitingApproval.as_str())? {
            st.relabel_steps(
                run.id,
                StepStatus::Intent.as_str(),
                StepStatus::Interrupted.as_str(),
            )?;
            for a in st.list_approvals(run.id)? {
                if a.decision.is_none() {
                    st.decide_approval(a.id, "expired")?;
                }
            }
            st.update_run_state(run.id, RunState::Paused.as_str(), None)?;
            st.append_audit(
                run.user_id,
                Some(run.id),
                "system",
                "run.recovered",
                &serde_json::json!({ "was": "waiting_approval" }),
            )?;
            ids.push(run.id);
        }
        Ok(ids)
    }

    /// Resume a `Paused` run in the background.  An interrupted model step is
    /// replayed; an interrupted tool call is **never** re-run (it may have had
    /// side effects): the model is told its outcome is unknown.
    ///
    /// # Errors
    /// Fails if the run does not exist, is not `Paused`, or its chat is busy.
    pub fn resume(&self, run_id: RunId) -> Result<()> {
        if self.sh.shutting_down.load(Ordering::SeqCst) {
            return Err(LoopError::ShuttingDown);
        }
        let run = self
            .sh
            .storage
            .get_run(run_id)?
            .ok_or_else(|| LoopError::NotFound(format!("run {run_id}")))?;
        let state = self.rt.state(run_id)?;
        if state != RunState::Paused {
            return Err(RuntimeError::IllegalTransition {
                from: state,
                to: RunState::Running,
            }
            .into());
        }
        // A subagent is not the chat's turn; its parent is.
        if run.parent_run_id.is_none()
            && let Some(chat) = run.chat_id
        {
            let mut active = self.sh.active.lock();
            if active.contains_key(&chat) {
                return Err(LoopError::Busy(chat));
            }
            active.insert(chat, run_id);
        }
        self.spawn(run_id, Start::Resume);
        Ok(())
    }

    /// Graceful shutdown (a restart must lose nothing).
    ///
    /// 1. Stops accepting new turns / resumes ([`LoopError::ShuttingDown`]).
    /// 2. Cancels in-flight model calls and runs parked on an approval or
    ///    question; their step is marked `Interrupted`.
    /// 3. Lets running *tool* steps finish for up to `grace`, then interrupts
    ///    what is left (an interrupted tool is never replayed on resume).
    /// 4. Every run that was live ends `Paused` (resumable after the next
    ///    start; dangling approvals expire), not `Cancelled`.
    /// 5. Flushes background memory tasks (bounded by `grace`, at least 1 s).
    ///
    /// Queued (not yet started) follow-up messages are in-memory only and are
    /// dropped.  Returns the runs that were paused.  Call
    /// [`Self::recover`] afterwards to checkpoint anything a crash left behind.
    pub async fn shutdown(&self, grace: Duration) -> Vec<RunId> {
        self.sh.shutting_down.store(true, Ordering::SeqCst);
        let deadline = tokio::time::Instant::now() + grace;
        // Hard stop for the unwinding of interrupted runs.
        let hard = deadline + Duration::from_secs(5);
        let mut seen: HashSet<RunId> = HashSet::new();
        loop {
            let active = self.rt.active_runs();
            if active.is_empty() {
                break;
            }
            let now = tokio::time::Instant::now();
            if now >= hard {
                break;
            }
            for run in active {
                seen.insert(run);
                // A finishing tool step is the only thing worth waiting for.
                let interrupt = now >= deadline || !self.tool_step_in_flight(run);
                self.rt.request_pause(run, interrupt);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // Let the drive tasks finish their bookkeeping (`conclude`).
        let handles: Vec<_> = self.sh.tasks.lock().drain().map(|(_, h)| h).collect();
        for h in handles {
            let left = hard.saturating_duration_since(tokio::time::Instant::now());
            let _ = tokio::time::timeout(left.max(Duration::from_millis(200)), h).await;
        }
        let mut paused = Vec::new();
        for run in seen {
            if self.rt.state(run).ok() != Some(RunState::Paused) {
                continue;
            }
            self.expire_approvals(run);
            if let Ok(Some(r)) = self.sh.storage.get_run(run) {
                let _ = self.sh.storage.append_audit(
                    r.user_id,
                    Some(run),
                    "system",
                    "run.paused_for_shutdown",
                    &serde_json::json!({}),
                );
            }
            paused.push(run);
        }
        let _ =
            tokio::time::timeout(grace.max(Duration::from_secs(1)), self.flush_background()).await;
        paused
    }

    /// Whether the run's newest step is a tool step that is still executing
    /// (and the run is not parked waiting for the user).
    fn tool_step_in_flight(&self, run: RunId) -> bool {
        let st = &self.sh.storage;
        if st
            .get_run(run)
            .ok()
            .flatten()
            .is_some_and(|r| r.state == RunState::WaitingApproval.as_str())
        {
            return false;
        }
        st.list_steps(run)
            .ok()
            .and_then(|s| s.last().cloned())
            .is_some_and(|s| s.name == STEP_TOOLS && s.status == StepStatus::Intent.as_str())
    }

    fn expire_approvals(&self, run: RunId) {
        let st = &self.sh.storage;
        for a in st.list_approvals(run).unwrap_or_default() {
            if a.decision.is_none() {
                let _ = st.decide_approval(a.id, "expired");
            }
        }
    }

    /// The bot's permission picture: its stored policy merged with its
    /// persisted grants, per category (what a human-initiated call resolves to).
    ///
    /// # Errors
    /// [`LoopError::NotFound`] / [`LoopError::Profile`] when the bot or its
    /// profile is unusable.
    pub fn effective_policy(&self, bot_id: BotId) -> Result<EffectivePolicy> {
        let profile = self.sh.profile_for(bot_id).map_err(|e| {
            if e.contains("not found") {
                LoopError::NotFound(format!("bot {bot_id}"))
            } else {
                LoopError::Profile(e)
            }
        })?;
        let policy = profile.policy();
        let categories: BTreeMap<PermissionCategory, CategoryDecision> = [
            PermissionCategory::Fs,
            PermissionCategory::Shell,
            PermissionCategory::Desktop,
            PermissionCategory::Browser,
            PermissionCategory::Accounts,
            PermissionCategory::Messaging,
            PermissionCategory::Money,
            PermissionCategory::System,
            PermissionCategory::Remote,
            PermissionCategory::Script,
        ]
        .into_iter()
        .map(|c| {
            let (decision, via_grant) = self.sh.effective_category(bot_id, policy.as_ref(), c);
            (
                c,
                CategoryDecision {
                    decision,
                    via_grant,
                },
            )
        })
        .collect();
        Ok(EffectivePolicy {
            policy,
            categories,
            grants: self.list_grants(bot_id),
        })
    }

    /// Wait until the background task of `run_id` (started by this handle) has
    /// finished and return the run state.
    ///
    /// # Errors
    /// Fails if the run is unknown or storage fails.
    pub async fn wait(&self, run_id: RunId) -> Result<RunState> {
        let handle = self.sh.tasks.lock().remove(&run_id);
        if let Some(h) = handle {
            let _ = h.await;
        }
        Ok(self.rt.state(run_id)?)
    }

    // ---- internals ----

    fn insert_user_message(
        &self,
        user_id: UserId,
        chat_id: ChatId,
        run_id: RunId,
        text: &str,
    ) -> Result<()> {
        self.sh.storage.insert_message(&DbMessage {
            id: Uuid::now_v7(),
            user_id,
            chat_id,
            run_id: Some(run_id),
            role: "user".to_owned(),
            content: text.to_owned(),
            created_at_ms: now_ms(),
        })?;
        Ok(())
    }

    pub(crate) fn spawn(&self, run_id: RunId, start: Start) {
        let handle = tokio::spawn(self.clone().drive(run_id, start));
        self.sh.tasks.lock().insert(run_id, handle);
    }

    // Boxed so the spawn / `conclude` / next-turn recursion has a nameable type.
    fn drive(self, run_id: RunId, start: Start) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            let res = match start {
                Start::Fresh => {
                    let is_child = self
                        .sh
                        .storage
                        .get_run(run_id)
                        .ok()
                        .flatten()
                        .is_some_and(|r| r.parent_run_id.is_some());
                    // A child announces itself with `subagent_started` instead.
                    if !is_child && let Some(ctx) = self.ctx_of(run_id) {
                        self.sh.emit(
                            &ctx,
                            Event::RunStarted(RunStarted {
                                run_id,
                                chat_id: ctx.chat_id,
                                bot_id: ctx.bot_id,
                                schedule_id: ctx.schedule_id,
                            }),
                        );
                    }
                    self.rt.start(run_id).await
                }
                Start::Resume => match self.rt.resume(run_id).await {
                    Err(RuntimeError::NeedsDecision(_)) => {
                        self.rt.resume_skipping_interrupted(run_id).await
                    }
                    other => other,
                },
            };
            self.conclude(run_id, &res);
        })
    }

    pub(crate) fn ctx_of(&self, run_id: RunId) -> Option<Arc<crate::shared::RunCtx>> {
        let run = self.sh.storage.get_run(run_id).ok().flatten()?;
        self.sh.ctx(&run).ok()
    }

    /// Persist the final answer, emit the closing events and start any queued turn.
    fn conclude(&self, run_id: RunId, res: &std::result::Result<RunState, RuntimeError>) {
        let run = self.sh.storage.get_run(run_id).ok().flatten();
        if let Some(parent) = run.as_ref().and_then(|r| r.parent_run_id) {
            self.conclude_child(run_id, parent, res);
            return;
        }
        let ctx = self.ctx_of(run_id);
        if matches!(res, Ok(RunState::Paused)) {
            // Paused by `shutdown`: no `run_finished` (the run is resumable and
            // a finished marker in its log would end later followers), no
            // queued follow-up turn.
            let approval_timeout = self.sh.approval_paused.lock().remove(&run_id);
            if let Some(ctx) = &ctx {
                let message = if approval_timeout {
                    "run paused: nobody answered the approval request in time; resume it with \
                     POST /v1/runs/{id}/resume"
                } else {
                    "run paused: the daemon is shutting down; resume it after the next start"
                };
                self.sh.emit(
                    ctx,
                    Event::Error(RunError {
                        run_id,
                        message: message.into(),
                        recoverable: true,
                    }),
                );
            }
            self.sh.runs.lock().remove(&run_id);
            if let Some(chat_id) = run.as_ref().and_then(|r| r.chat_id) {
                let mut active = self.sh.active.lock();
                if active.get(&chat_id) == Some(&run_id) {
                    active.remove(&chat_id);
                }
            }
            return;
        }
        let error: Option<String> = match res {
            Ok(RunState::Succeeded) => None,
            Ok(RunState::Failed) => Some(
                run.as_ref()
                    .and_then(|r| r.error.clone())
                    .unwrap_or_else(|| "failed".into()),
            ),
            Ok(RunState::Cancelled) => Some("cancelled".into()),
            Ok(other) => Some(format!("stopped in state {other}")),
            Err(e) => Some(e.to_string()),
        };
        if error.is_none()
            && let Some(run) = &run
            && let Some(chat_id) = run.chat_id
            && let Some(text) = self.final_text(run_id)
        {
            let _ = self.sh.storage.insert_message(&DbMessage {
                id: Uuid::now_v7(),
                user_id: run.user_id,
                chat_id,
                run_id: Some(run_id),
                role: "assistant".to_owned(),
                content: text.clone(),
                created_at_ms: now_ms(),
            });
            self.enqueue_memory(run, &text);
        }
        if let Some(ctx) = &ctx {
            if let Some(message) = &error {
                self.sh.emit(
                    ctx,
                    Event::Error(RunError {
                        run_id,
                        message: message.clone(),
                        recoverable: false,
                    }),
                );
            }
            self.sh
                .emit(ctx, Event::RunFinished(RunFinished { run_id, error }));
        }
        self.sh.runs.lock().remove(&run_id);

        let Some(run) = run else { return };
        let Some(chat_id) = run.chat_id else { return };
        let next = {
            let mut active = self.sh.active.lock();
            if active.get(&chat_id) == Some(&run_id) {
                active.remove(&chat_id);
            }
            self.sh
                .queued
                .lock()
                .get_mut(&chat_id)
                .and_then(std::collections::VecDeque::pop_front)
        };
        if let Some(text) = next {
            let _ = self.run_turn_as(chat_id, &text, initiator_of(&run.kind));
        }
    }

    /// Hand the finished exchange to the memory pipeline (background task).
    fn enqueue_memory(&self, run: &storage::Run, final_text: &str) {
        if !self.sh.cfg.memory.as_ref().is_some_and(|m| m.enabled) {
            return;
        }
        if let Some(ex) = crate::memory::Exchange::of(&self.sh, run, final_text) {
            let sh = Arc::clone(&self.sh);
            let handle = tokio::spawn(crate::memory::extract(sh, ex));
            let mut bg = self.sh.bg.lock();
            bg.retain(|h| !h.is_finished());
            bg.push(handle);
        }
    }

    fn final_text(&self, run_id: RunId) -> Option<String> {
        let steps = self.sh.storage.list_steps(run_id).ok()?;
        let last = steps.last()?;
        if let Some(text) = crate::router::routed_final_text(last) {
            return Some(text);
        }
        if last.name != STEP_MODEL || status_of(last) != Some(StepStatus::Succeeded) {
            return None;
        }
        step_output::<ModelOut>(last)
            .map(|m| m.text)
            .filter(|t| !t.is_empty())
    }
}

fn load_grants(storage: &Storage) -> Grants {
    let mut grants = Grants::default();
    for user in storage.list_users().unwrap_or_default() {
        for e in storage.list_audit(user.id).unwrap_or_default() {
            if e.action != AUDIT_GRANT_ALWAYS && e.action != AUDIT_GRANT_REVOKED {
                continue;
            }
            let bot = e.detail["bot_id"]
                .as_str()
                .and_then(|s| s.parse::<Uuid>().ok())
                .map(BotId);
            let (Some(bot_id), Some(target)) = (bot, grant_target(&e.detail)) else {
                continue;
            };
            grants
                .always
                .retain(|g| !(g.bot_id == bot_id && g.target == target));
            if e.action == AUDIT_GRANT_ALWAYS {
                grants.always.push(Grant {
                    bot_id,
                    target,
                    granted_at_ms: e.ts_ms,
                });
            }
        }
    }
    grants
}
