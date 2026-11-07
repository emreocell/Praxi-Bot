//! The public [`AgentLoop`] handle.

use std::collections::HashMap;
use std::sync::Arc;

use futures::future::BoxFuture;
use parking_lot::{Mutex, RwLock};
use permission_engine::PermissionResolver;
use protocol::event::{RunError, RunFinished, RunStarted};
use protocol::{BotId, ChatId, Event, InitiatorKind, RunId, ToolCallId, UserId};
use storage::{Message as DbMessage, Storage, now_ms};
use task_runtime::{RunState, Runtime, RuntimeError, StepStatus};
use tokio::sync::broadcast;
use tool_registry::ToolRegistry;
use uuid::Uuid;

use crate::AUDIT_GRANT_ALWAYS;
use crate::config::LoopConfig;
use crate::exec::TurnExecutor;
use crate::history::{status_of, step_output};
use crate::profile::BotProfile;
use crate::shared::{
    ApprovalScope, Delivery, Grants, LoopError, MessageMode, ModelOut, Pending, Providers,
    STEP_MODEL, Shared, StreamItem, Verdict, initiator_of, kind_for,
};
use crate::todo::TodoTool;

type Result<T> = std::result::Result<T, LoopError>;

#[derive(Clone, Copy)]
enum Start {
    Fresh,
    Resume,
}

/// Runs bot turns: model -> tool calls -> permission -> execute -> repeat.
///
/// Cheap to clone (shared handles).  Spawning methods must be called inside a
/// Tokio runtime.
#[derive(Clone)]
pub struct AgentLoop {
    sh: Arc<Shared>,
    rt: Arc<Runtime>,
}

impl AgentLoop {
    /// Build a loop.  Registers the built-in `plan.todo_write` tool into `tools`
    /// (unless a tool with that name already exists) and reloads persisted
    /// "always allow" grants from the audit log.
    #[must_use]
    pub fn new(
        storage: Arc<Storage>,
        tools: ToolRegistry,
        providers: Providers,
        cfg: LoopConfig,
    ) -> Self {
        let (events, _) = broadcast::channel(4096);
        let sh = Arc::new(Shared {
            storage: Arc::clone(&storage),
            tools,
            providers,
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
        });
        sh.tools
            .register(Arc::new(TodoTool::new(Arc::downgrade(&sh))));
        let rt = Arc::new(Runtime::new(
            storage,
            Arc::new(TurnExecutor {
                sh: Arc::clone(&sh),
            }),
        ));
        Self { sh, rt }
    }

    /// Register the profile used by `bot_id` (otherwise the bot row's JSON
    /// `config` is parsed as a profile).
    pub fn add_profile(&self, bot_id: BotId, profile: BotProfile) {
        self.sh.profiles.write().insert(bot_id, Arc::new(profile));
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
            let run_id = self.rt.create_run(
                chat.user_id,
                chat.bot_id,
                Some(chat_id),
                &kind_for(initiator),
            )?;
            self.insert_user_message(chat.user_id, chat_id, run_id, user_message)?;
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

    fn take_pending(&self, call_id: ToolCallId) -> Result<Pending> {
        self.sh
            .pending
            .lock()
            .remove(&call_id)
            .ok_or(LoopError::NoPendingApproval(call_id))
    }

    /// Cancel a run (in-flight step becomes `Interrupted`).
    ///
    /// # Errors
    /// Fails if the run is terminal or storage fails.
    pub fn cancel(&self, run_id: RunId) -> Result<()> {
        Ok(self.rt.cancel(run_id)?)
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
        if let Some(chat) = run.chat_id {
            let mut active = self.sh.active.lock();
            if active.contains_key(&chat) {
                return Err(LoopError::Busy(chat));
            }
            active.insert(chat, run_id);
        }
        self.spawn(run_id, Start::Resume);
        Ok(())
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

    fn spawn(&self, run_id: RunId, start: Start) {
        let handle = tokio::spawn(self.clone().drive(run_id, start));
        self.sh.tasks.lock().insert(run_id, handle);
    }

    // Boxed so the spawn / `conclude` / next-turn recursion has a nameable type.
    fn drive(self, run_id: RunId, start: Start) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            let res = match start {
                Start::Fresh => {
                    if let Some(ctx) = self.ctx_of(run_id) {
                        self.sh.emit(
                            &ctx,
                            Event::RunStarted(RunStarted {
                                run_id,
                                chat_id: ctx.chat_id,
                                bot_id: ctx.bot_id,
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

    fn ctx_of(&self, run_id: RunId) -> Option<Arc<crate::shared::RunCtx>> {
        let run = self.sh.storage.get_run(run_id).ok().flatten()?;
        self.sh.ctx(&run).ok()
    }

    /// Persist the final answer, emit the closing events and start any queued turn.
    fn conclude(&self, run_id: RunId, res: &std::result::Result<RunState, RuntimeError>) {
        let run = self.sh.storage.get_run(run_id).ok().flatten();
        let ctx = self.ctx_of(run_id);
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
                content: text,
                created_at_ms: now_ms(),
            });
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

    fn final_text(&self, run_id: RunId) -> Option<String> {
        let steps = self.sh.storage.list_steps(run_id).ok()?;
        let last = steps.last()?;
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
            if e.action != AUDIT_GRANT_ALWAYS {
                continue;
            }
            let bot = e.detail["bot_id"]
                .as_str()
                .and_then(|s| s.parse::<Uuid>().ok())
                .map(BotId);
            if let (Some(bot), Some(tool)) = (bot, e.detail["tool"].as_str()) {
                grants.always.insert((bot, tool.to_owned()));
            }
        }
    }
    grants
}
