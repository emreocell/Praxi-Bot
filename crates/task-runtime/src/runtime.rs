//! The durable run [`Runtime`].

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use parking_lot::Mutex;
use protocol::{BotId, ChatId, RunId, UserId};
use serde_json::json;
use storage::{Run, RunStep, Storage, StorageError, now_ms};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::state::{RunState, StepStatus, can_transition};
use crate::step::{StepExecutor, StepSpec};

/// Errors of the runtime.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    /// Storage failure.
    #[error(transparent)]
    Storage(#[from] StorageError),
    /// JSON failure.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// No such run.
    #[error("run {0} not found")]
    NotFound(RunId),
    /// The requested transition is not allowed from the current state.
    #[error("illegal transition {from} -> {to}")]
    IllegalTransition {
        /// Current state.
        from: RunState,
        /// Requested state.
        to: RunState,
    },
    /// The database holds a state string this build does not know.
    #[error("unknown stored state {0:?}")]
    UnknownState(String),
    /// The last step was interrupted and is not idempotent; replaying it could
    /// repeat a side effect.  Use [`Runtime::resume_skipping_interrupted`] to
    /// continue without replaying it.
    #[error("step {0} was interrupted and is not idempotent; refusing to replay")]
    NeedsDecision(Uuid),
}

/// Result alias.
pub type Result<T> = std::result::Result<T, RuntimeError>;

struct TokenGuard<'a> {
    rt: &'a Runtime,
    run_id: RunId,
}

impl Drop for TokenGuard<'_> {
    fn drop(&mut self) {
        self.rt.tokens.lock().remove(&self.run_id);
        self.rt.pausing.lock().remove(&self.run_id);
    }
}

/// Durable run runtime.  Storage calls are synchronous and short; they run
/// inline on the async task.
pub struct Runtime {
    storage: Arc<Storage>,
    executor: Arc<dyn StepExecutor>,
    tokens: Mutex<HashMap<RunId, CancellationToken>>,
    /// Live runs that must stop as `Paused` (not `Cancelled`), see
    /// [`Runtime::request_pause`].
    pausing: Mutex<HashSet<RunId>>,
}

impl Runtime {
    /// Create a runtime over `storage` using `executor` for steps.
    #[must_use]
    pub fn new(storage: Arc<Storage>, executor: Arc<dyn StepExecutor>) -> Self {
        Self {
            storage,
            executor,
            tokens: Mutex::new(HashMap::new()),
            pausing: Mutex::new(HashSet::new()),
        }
    }

    /// The underlying storage.
    #[must_use]
    pub fn storage(&self) -> &Arc<Storage> {
        &self.storage
    }

    /// Persist a new run in `Queued`.
    ///
    /// # Errors
    /// Fails on storage errors (e.g. unknown user/bot).
    pub fn create_run(
        &self,
        user_id: UserId,
        bot_id: BotId,
        chat_id: Option<ChatId>,
        kind: &str,
    ) -> Result<RunId> {
        self.create_child_run(user_id, bot_id, chat_id, kind, None, None)
    }

    /// Persist a new run in `Queued`, optionally as a child (subagent) of
    /// `parent` with its spawn parameters in `spec`.
    ///
    /// # Errors
    /// Fails on storage errors (e.g. unknown user/bot/parent).
    pub fn create_child_run(
        &self,
        user_id: UserId,
        bot_id: BotId,
        chat_id: Option<ChatId>,
        kind: &str,
        parent: Option<RunId>,
        spec: Option<serde_json::Value>,
    ) -> Result<RunId> {
        let now = now_ms();
        let run = Run {
            id: RunId::new(),
            user_id,
            bot_id,
            chat_id,
            kind: kind.to_owned(),
            state: RunState::Queued.as_str().to_owned(),
            error: None,
            created_at_ms: now,
            updated_at_ms: now,
            parent_run_id: parent,
            spec,
            computer_target: None,
            computer_node: None,
            computer_node_name: None,
            paused_at_ms: None,
            pause_reason: None,
        };
        self.storage.insert_run(&run)?;
        Ok(run.id)
    }

    /// Current state of a run.
    ///
    /// # Errors
    /// Fails if the run is missing or storage fails.
    pub fn state(&self, run_id: RunId) -> Result<RunState> {
        let run = self.require_run(run_id)?;
        RunState::parse(&run.state).ok_or(RuntimeError::UnknownState(run.state))
    }

    /// Start a `Queued` run and drive it until it reaches a non-`Running`
    /// state.  Spawn the returned future to run in the background.
    ///
    /// # Errors
    /// Fails if the run is not `Queued` or on storage errors.
    pub async fn start(&self, run_id: RunId) -> Result<RunState> {
        if self.state(run_id)? != RunState::Queued {
            return Err(RuntimeError::IllegalTransition {
                from: self.state(run_id)?,
                to: RunState::Running,
            });
        }
        self.transition(run_id, RunState::Running, None)?;
        self.drive(run_id, None).await
    }

    /// Move a `Queued` run into `Paused` with an optional durable
    /// `pause_reason` (for example `waiting_for_pc`) before executing any step.
    ///
    /// # Errors
    /// Fails if the run is not `Queued` or on storage errors.
    pub fn pause_queued(&self, run_id: RunId, reason: Option<&str>) -> Result<RunState> {
        let state = self.state(run_id)?;
        if state != RunState::Queued {
            return Err(RuntimeError::IllegalTransition {
                from: state,
                to: RunState::Paused,
            });
        }
        self.transition(run_id, RunState::Running, None)?;
        self.transition(run_id, RunState::Paused, None)?;
        self.storage.set_run_pause_reason(run_id, reason)?;
        Ok(RunState::Paused)
    }

    /// Transition an idle `Paused` run to `Failed` with an error message
    /// (for example after waiting-for-PC times out).
    ///
    /// # Errors
    /// Fails if the run is not `Paused` or on storage errors.
    pub fn fail_paused(&self, run_id: RunId, error: &str) -> Result<RunState> {
        let state = self.state(run_id)?;
        if state != RunState::Paused {
            return Err(RuntimeError::IllegalTransition {
                from: state,
                to: RunState::Failed,
            });
        }
        self.transition(run_id, RunState::Failed, Some(error))
    }

    /// Explicitly resume a `Paused` run.  An interrupted idempotent step is
    /// replayed (new attempt); an interrupted non-idempotent step is **never**
    /// replayed: this returns [`RuntimeError::NeedsDecision`] and leaves the
    /// run `Paused`.
    ///
    /// # Errors
    /// Fails if the run is not `Paused`, needs a decision, or on storage errors.
    pub async fn resume(&self, run_id: RunId) -> Result<RunState> {
        self.resume_inner(run_id, false).await
    }

    /// Resume a `Paused` run without replaying the interrupted step (it stays
    /// `Interrupted` in history; the executor decides what to do next).
    ///
    /// # Errors
    /// Fails if the run is not `Paused` or on storage errors.
    pub async fn resume_skipping_interrupted(&self, run_id: RunId) -> Result<RunState> {
        self.resume_inner(run_id, true).await
    }

    async fn resume_inner(&self, run_id: RunId, skip: bool) -> Result<RunState> {
        let state = self.state(run_id)?;
        if state != RunState::Paused {
            return Err(RuntimeError::IllegalTransition {
                from: state,
                to: RunState::Running,
            });
        }
        let last = self.storage.list_steps(run_id)?.pop();
        let pending = match last {
            Some(s) if !skip && StepStatus::parse(&s.status) == Some(StepStatus::Interrupted) => {
                if s.idempotent {
                    Some(s)
                } else {
                    return Err(RuntimeError::NeedsDecision(s.id));
                }
            }
            _ => None,
        };
        self.transition(run_id, RunState::Running, None)?;
        self.drive(run_id, pending).await
    }

    /// Cancel a run.  A live run is signalled and finishes as `Cancelled`
    /// (its in-flight step becomes `Interrupted`); an idle one is cancelled
    /// immediately.
    ///
    /// # Errors
    /// Fails if the run is already terminal or on storage errors.
    pub fn cancel(&self, run_id: RunId) -> Result<()> {
        if let Some(token) = self.tokens.lock().get(&run_id) {
            // An explicit cancel beats a pending pause request.
            self.pausing.lock().remove(&run_id);
            token.cancel();
            return Ok(());
        }
        self.transition(run_id, RunState::Cancelled, None)?;
        Ok(())
    }

    /// Runs currently driven by this runtime (a task is executing them).
    #[must_use]
    pub fn active_runs(&self) -> Vec<RunId> {
        self.tokens.lock().keys().copied().collect()
    }

    /// Ask a live run to stop as `Paused` (resumable), e.g. for a clean
    /// shutdown.  Returns `false` if the run is not live.
    ///
    /// * `interrupt = false`: the step in flight (if any) finishes, then the
    ///   run pauses before planning the next one.
    /// * `interrupt = true`: the step in flight is abandoned and marked
    ///   `Interrupted` (it is never replayed unless idempotent, see
    ///   [`Runtime::resume`]); the run pauses.
    ///
    /// `interrupt = true` may follow an earlier `false` request.
    pub fn request_pause(&self, run_id: RunId, interrupt: bool) -> bool {
        let tokens = self.tokens.lock();
        let Some(token) = tokens.get(&run_id) else {
            return false;
        };
        self.pausing.lock().insert(run_id);
        if interrupt {
            token.cancel();
        }
        true
    }

    /// Terminal-or-paused state a stopped live run moves to: `Paused` after
    /// [`Runtime::request_pause`], else `Cancelled`.
    fn stop_state(&self, run_id: RunId) -> RunState {
        if self.pausing.lock().contains(&run_id) {
            RunState::Paused
        } else {
            RunState::Cancelled
        }
    }

    /// Startup recovery: every run left `Running` gets its un-finished steps
    /// marked `Interrupted` and moves to `Paused`.  Nothing is replayed.
    /// Returns the recovered run ids.
    ///
    /// # Errors
    /// Fails on storage errors.
    pub fn recover(&self) -> Result<Vec<RunId>> {
        let mut recovered = Vec::new();
        for run in self
            .storage
            .list_runs_by_state(RunState::Running.as_str())?
        {
            let n = self.storage.relabel_steps(
                run.id,
                StepStatus::Intent.as_str(),
                StepStatus::Interrupted.as_str(),
            )?;
            self.transition(run.id, RunState::Paused, None)?;
            self.storage.append_audit(
                run.user_id,
                Some(run.id),
                "system",
                "run.recovered",
                &json!({ "interrupted_steps": n }),
            )?;
            recovered.push(run.id);
        }
        Ok(recovered)
    }

    // ---- internals ----

    fn require_run(&self, run_id: RunId) -> Result<Run> {
        self.storage
            .get_run(run_id)?
            .ok_or(RuntimeError::NotFound(run_id))
    }

    fn transition(&self, run_id: RunId, to: RunState, error: Option<&str>) -> Result<RunState> {
        let run = self.require_run(run_id)?;
        let from = RunState::parse(&run.state).ok_or(RuntimeError::UnknownState(run.state))?;
        if !can_transition(from, to) {
            return Err(RuntimeError::IllegalTransition { from, to });
        }
        self.storage.update_run_state(run_id, to.as_str(), error)?;
        Ok(to)
    }

    async fn drive(&self, run_id: RunId, pending: Option<RunStep>) -> Result<RunState> {
        let token = CancellationToken::new();
        self.tokens.lock().insert(run_id, token.clone());
        let _guard = TokenGuard { rt: self, run_id };
        self.drive_inner(run_id, &token, pending).await
    }

    #[allow(clippy::too_many_lines)]
    async fn drive_inner(
        &self,
        run_id: RunId,
        token: &CancellationToken,
        mut pending: Option<RunStep>,
    ) -> Result<RunState> {
        let st = &self.storage;
        loop {
            if token.is_cancelled() || self.pausing.lock().contains(&run_id) {
                return self.transition(run_id, self.stop_state(run_id), None);
            }
            let run = self.require_run(run_id)?;
            let (step_id, spec, mut attempt) = if let Some(prev) = pending.take() {
                let attempt = prev.attempt + 1;
                st.update_step(
                    prev.id,
                    StepStatus::Intent.as_str(),
                    attempt,
                    None,
                    None,
                    false,
                )?;
                (prev.id, StepSpec::from_row(&prev), attempt)
            } else {
                let history = st.list_steps(run_id)?;
                match self.executor.next_step(&run, &history).await {
                    Ok(None) => return self.transition(run_id, RunState::Succeeded, None),
                    Err(e) => return self.transition(run_id, RunState::Failed, Some(&e.0)),
                    Ok(Some(spec)) => {
                        let row = RunStep {
                            id: Uuid::now_v7(),
                            run_id,
                            user_id: run.user_id,
                            idx: u32::try_from(history.len()).unwrap_or(u32::MAX),
                            name: spec.name.clone(),
                            input: spec.input.clone(),
                            idempotent: spec.idempotent,
                            retry: spec.retry.map(serde_json::to_value).transpose()?,
                            status: StepStatus::Intent.as_str().to_owned(),
                            attempt: 1,
                            output: None,
                            error: None,
                            started_at_ms: now_ms(),
                            finished_at_ms: None,
                        };
                        // Intent is durable before the step runs.
                        st.insert_step(&row)?;
                        (row.id, spec, 1)
                    }
                }
            };

            loop {
                let outcome = tokio::select! {
                    biased;
                    () = token.cancelled() => None,
                    r = self.executor.execute(&run, &spec, token) => Some(r),
                };
                match outcome {
                    None => {
                        st.update_step(
                            step_id,
                            StepStatus::Interrupted.as_str(),
                            attempt,
                            None,
                            None,
                            true,
                        )?;
                        return self.transition(run_id, self.stop_state(run_id), None);
                    }
                    Some(Ok(output)) => {
                        st.update_step(
                            step_id,
                            StepStatus::Succeeded.as_str(),
                            attempt,
                            Some(&output),
                            None,
                            true,
                        )?;
                        break;
                    }
                    Some(Err(e)) => {
                        st.update_step(
                            step_id,
                            StepStatus::Failed.as_str(),
                            attempt,
                            None,
                            Some(&e.0),
                            true,
                        )?;
                        let policy = spec
                            .retry
                            .filter(|p| spec.idempotent && attempt < p.max_attempts);
                        let Some(policy) = policy else {
                            let msg = format!("step '{}' failed: {}", spec.name, e.0);
                            return self.transition(run_id, RunState::Failed, Some(&msg));
                        };
                        tokio::select! {
                            biased;
                            () = token.cancelled() => {
                                return self.transition(run_id, self.stop_state(run_id), None);
                            }
                            () = tokio::time::sleep(policy.backoff(attempt)) => {}
                        }
                        attempt += 1;
                        st.update_step(
                            step_id,
                            StepStatus::Intent.as_str(),
                            attempt,
                            None,
                            None,
                            false,
                        )?;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;
    use std::time::Duration;

    use async_trait::async_trait;
    use serde_json::Value;

    use super::*;
    use crate::step::{ExecError, RetryPolicy};
    use storage::{Bot, User};

    #[derive(Clone, Copy, PartialEq)]
    enum Block {
        Pending,
        UntilCancel,
        Sleep(u64),
    }

    struct Fake {
        plan: Vec<StepSpec>,
        failures: Mutex<HashMap<String, u32>>,
        block_on: Option<(String, Block)>,
        executed: Mutex<Vec<String>>,
    }

    impl Fake {
        fn new(plan: Vec<StepSpec>) -> Self {
            Self {
                plan,
                failures: Mutex::new(HashMap::new()),
                block_on: None,
                executed: Mutex::new(Vec::new()),
            }
        }
        fn blocking(mut self, name: &str, mode: Block) -> Self {
            self.block_on = Some((name.into(), mode));
            self
        }
        fn failing(self, name: &str, times: u32) -> Self {
            self.failures.lock().insert(name.into(), times);
            self
        }
    }

    #[async_trait]
    impl StepExecutor for Fake {
        async fn next_step(
            &self,
            _: &Run,
            history: &[RunStep],
        ) -> std::result::Result<Option<StepSpec>, ExecError> {
            Ok(self.plan.get(history.len()).cloned())
        }
        async fn execute(
            &self,
            _: &Run,
            step: &StepSpec,
            cancel: &CancellationToken,
        ) -> std::result::Result<Value, ExecError> {
            self.executed.lock().push(step.name.clone());
            {
                let mut f = self.failures.lock();
                if let Some(n) = f.get_mut(&step.name)
                    && *n > 0
                {
                    *n -= 1;
                    return Err(ExecError("boom".into()));
                }
            }
            if let Some((name, mode)) = &self.block_on
                && *name == step.name
            {
                match mode {
                    Block::Pending => std::future::pending::<()>().await,
                    Block::UntilCancel => {
                        cancel.cancelled().await;
                        return Err(ExecError("cancelled".into()));
                    }
                    Block::Sleep(ms) => tokio::time::sleep(Duration::from_millis(*ms)).await,
                }
            }
            Ok(json!({ "done": step.name }))
        }
    }

    fn spec(name: &str, idempotent: bool) -> StepSpec {
        StepSpec {
            name: name.into(),
            input: json!({ "n": name }),
            idempotent,
            retry: idempotent.then_some(RetryPolicy {
                max_attempts: 3,
                initial_backoff_ms: 1,
                multiplier: 2,
                max_backoff_ms: 5,
            }),
        }
    }

    fn setup(dir: &Path) -> (Arc<Storage>, UserId, BotId) {
        let st = Arc::new(Storage::open(dir).unwrap());
        let user = UserId::new();
        let bot = BotId::new();
        if st.get_user(user).unwrap().is_none() {
            st.insert_user(&User {
                id: user,
                name: "u".into(),
                created_at_ms: 1,
            })
            .unwrap();
            st.insert_bot(&Bot {
                id: bot,
                user_id: user,
                name: "b".into(),
                config: json!({}),
                created_at_ms: 1,
            })
            .unwrap();
        }
        (st, user, bot)
    }

    fn statuses(st: &Storage, run: RunId) -> Vec<(String, String, u32)> {
        st.list_steps(run)
            .unwrap()
            .into_iter()
            .map(|s| (s.name, s.status, s.attempt))
            .collect()
    }

    async fn wait_for_intent(st: &Storage, run: RunId) {
        for _ in 0..500 {
            if st
                .list_steps(run)
                .unwrap()
                .iter()
                .any(|s| s.status == "intent")
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("step intent never persisted");
    }

    #[tokio::test]
    async fn happy_path_runs_all_steps() {
        let dir = tempfile::tempdir().unwrap();
        let (st, u, b) = setup(dir.path());
        let ex = Arc::new(Fake::new(vec![spec("a", false), spec("b", true)]));
        let rt = Runtime::new(st.clone(), ex);
        let run = rt.create_run(u, b, None, "turn").unwrap();
        assert_eq!(rt.start(run).await.unwrap(), RunState::Succeeded);
        assert_eq!(
            statuses(&st, run),
            vec![
                ("a".into(), "succeeded".into(), 1),
                ("b".into(), "succeeded".into(), 1)
            ]
        );
        assert!(rt.start(run).await.is_err(), "terminal run cannot restart");
    }

    #[tokio::test]
    async fn idempotent_step_retries_then_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let (st, u, b) = setup(dir.path());
        let ex = Arc::new(Fake::new(vec![spec("a", true)]).failing("a", 2));
        let rt = Runtime::new(st.clone(), ex.clone());
        let run = rt.create_run(u, b, None, "turn").unwrap();
        assert_eq!(rt.start(run).await.unwrap(), RunState::Succeeded);
        assert_eq!(
            statuses(&st, run),
            vec![("a".into(), "succeeded".into(), 3)]
        );
        assert_eq!(ex.executed.lock().len(), 3);
    }

    #[tokio::test]
    async fn idempotent_step_exhausts_retries() {
        let dir = tempfile::tempdir().unwrap();
        let (st, u, b) = setup(dir.path());
        let ex = Arc::new(Fake::new(vec![spec("a", true)]).failing("a", 99));
        let rt = Runtime::new(st.clone(), ex.clone());
        let run = rt.create_run(u, b, None, "turn").unwrap();
        assert_eq!(rt.start(run).await.unwrap(), RunState::Failed);
        assert_eq!(ex.executed.lock().len(), 3);
        assert!(
            st.get_run(run)
                .unwrap()
                .unwrap()
                .error
                .unwrap()
                .contains("boom")
        );
    }

    #[tokio::test]
    async fn non_idempotent_step_is_never_retried() {
        let dir = tempfile::tempdir().unwrap();
        let (st, u, b) = setup(dir.path());
        let mut s = spec("a", false);
        s.retry = Some(RetryPolicy {
            max_attempts: 5,
            initial_backoff_ms: 1,
            multiplier: 1,
            max_backoff_ms: 1,
        });
        let ex = Arc::new(Fake::new(vec![s]).failing("a", 99));
        let rt = Runtime::new(st.clone(), ex.clone());
        let run = rt.create_run(u, b, None, "turn").unwrap();
        assert_eq!(rt.start(run).await.unwrap(), RunState::Failed);
        assert_eq!(ex.executed.lock().len(), 1);
        assert_eq!(statuses(&st, run), vec![("a".into(), "failed".into(), 1)]);
    }

    /// Crash mid-run: drop the runtime while a step is in flight, reopen the DB.
    async fn crash_mid_step(dir: &Path, step: StepSpec) -> (RunId, UserId, BotId) {
        let (st, u, b) = setup(dir);
        let ex = Arc::new(Fake::new(vec![step.clone()]).blocking(&step.name, Block::Pending));
        let rt = Arc::new(Runtime::new(st.clone(), ex));
        let run = rt.create_run(u, b, None, "turn").unwrap();
        let handle = tokio::spawn({
            let rt = rt.clone();
            async move { rt.start(run).await }
        });
        wait_for_intent(&st, run).await;
        handle.abort();
        assert!(handle.await.unwrap_err().is_cancelled());
        assert_eq!(
            rt.state(run).unwrap(),
            RunState::Running,
            "crash left it Running"
        );
        (run, u, b)
    }

    #[tokio::test]
    async fn crash_then_recover_pauses_with_interrupted_step() {
        let dir = tempfile::tempdir().unwrap();
        let (run, u, _b) = crash_mid_step(dir.path(), spec("send_mail", false)).await;

        // "Restart": fresh storage handle + runtime.
        let st = Arc::new(Storage::open(dir.path()).unwrap());
        let ex = Arc::new(Fake::new(vec![spec("send_mail", false)]));
        let rt = Runtime::new(st.clone(), ex.clone());
        assert_eq!(rt.recover().unwrap(), vec![run]);
        assert_eq!(rt.state(run).unwrap(), RunState::Paused);
        assert_eq!(
            statuses(&st, run),
            vec![("send_mail".into(), "interrupted".into(), 1)]
        );
        assert_eq!(st.list_audit(u).unwrap()[0].action, "run.recovered");
        assert!(rt.recover().unwrap().is_empty(), "recover is idempotent");

        // Nothing was auto-replayed; explicit resume refuses to replay a side effect.
        assert!(ex.executed.lock().is_empty());
        assert!(matches!(
            rt.resume(run).await,
            Err(RuntimeError::NeedsDecision(_))
        ));
        assert_eq!(rt.state(run).unwrap(), RunState::Paused);
        assert!(ex.executed.lock().is_empty());

        // Explicit skip continues without replaying.
        assert_eq!(
            rt.resume_skipping_interrupted(run).await.unwrap(),
            RunState::Succeeded
        );
        assert!(ex.executed.lock().is_empty());
        assert_eq!(
            statuses(&st, run),
            vec![("send_mail".into(), "interrupted".into(), 1)]
        );
    }

    #[tokio::test]
    async fn resume_replays_interrupted_idempotent_step() {
        let dir = tempfile::tempdir().unwrap();
        let (run, _u, _b) = crash_mid_step(dir.path(), spec("read_file", true)).await;
        let st = Arc::new(Storage::open(dir.path()).unwrap());
        let ex = Arc::new(Fake::new(vec![spec("read_file", true)]));
        let rt = Runtime::new(st.clone(), ex.clone());
        rt.recover().unwrap();
        assert_eq!(rt.resume(run).await.unwrap(), RunState::Succeeded);
        assert_eq!(*ex.executed.lock(), vec!["read_file".to_string()]);
        assert_eq!(
            statuses(&st, run),
            vec![("read_file".into(), "succeeded".into(), 2)]
        );
    }

    #[tokio::test]
    async fn resume_requires_paused() {
        let dir = tempfile::tempdir().unwrap();
        let (st, u, b) = setup(dir.path());
        let rt = Runtime::new(st, Arc::new(Fake::new(vec![])));
        let run = rt.create_run(u, b, None, "turn").unwrap();
        assert!(matches!(
            rt.resume(run).await,
            Err(RuntimeError::IllegalTransition { .. })
        ));
        assert!(matches!(
            rt.resume(RunId::new()).await,
            Err(RuntimeError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn cancel_live_run_interrupts_step() {
        let dir = tempfile::tempdir().unwrap();
        let (st, u, b) = setup(dir.path());
        let ex = Arc::new(Fake::new(vec![spec("a", false)]).blocking("a", Block::UntilCancel));
        let rt = Arc::new(Runtime::new(st.clone(), ex));
        let run = rt.create_run(u, b, None, "turn").unwrap();
        let handle = tokio::spawn({
            let rt = rt.clone();
            async move { rt.start(run).await }
        });
        wait_for_intent(&st, run).await;
        rt.cancel(run).unwrap();
        assert_eq!(handle.await.unwrap().unwrap(), RunState::Cancelled);
        assert_eq!(
            statuses(&st, run),
            vec![("a".into(), "interrupted".into(), 1)]
        );
        assert!(rt.cancel(run).is_err(), "already terminal");
    }

    #[tokio::test]
    async fn pause_with_interrupt_abandons_the_step_and_pauses() {
        let dir = tempfile::tempdir().unwrap();
        let (st, u, b) = setup(dir.path());
        let ex = Arc::new(Fake::new(vec![spec("a", false)]).blocking("a", Block::Pending));
        let rt = Arc::new(Runtime::new(st.clone(), ex));
        let run = rt.create_run(u, b, None, "turn").unwrap();
        let handle = tokio::spawn({
            let rt = rt.clone();
            async move { rt.start(run).await }
        });
        wait_for_intent(&st, run).await;
        assert_eq!(rt.active_runs(), vec![run]);
        assert!(rt.request_pause(run, true));
        assert_eq!(handle.await.unwrap().unwrap(), RunState::Paused);
        assert_eq!(
            statuses(&st, run),
            vec![("a".into(), "interrupted".into(), 1)]
        );
        assert_eq!(rt.active_runs(), Vec::<RunId>::new());
        assert!(!rt.request_pause(run, true), "no longer live");
    }

    #[tokio::test]
    async fn pause_without_interrupt_lets_the_step_finish_then_stops() {
        let dir = tempfile::tempdir().unwrap();
        let (st, u, b) = setup(dir.path());
        let ex = Arc::new(
            Fake::new(vec![spec("a", false), spec("b", false)]).blocking("a", Block::Sleep(150)),
        );
        let rt = Arc::new(Runtime::new(st.clone(), ex.clone()));
        let run = rt.create_run(u, b, None, "turn").unwrap();
        let handle = tokio::spawn({
            let rt = rt.clone();
            async move { rt.start(run).await }
        });
        wait_for_intent(&st, run).await;
        assert!(rt.request_pause(run, false));
        assert_eq!(handle.await.unwrap().unwrap(), RunState::Paused);
        // "a" completed, "b" never started.
        assert_eq!(
            statuses(&st, run),
            vec![("a".into(), "succeeded".into(), 1)]
        );
        assert_eq!(*ex.executed.lock(), vec!["a".to_owned()]);
        // The paused run resumes and finishes.
        assert_eq!(rt.resume(run).await.unwrap(), RunState::Succeeded);
    }

    #[tokio::test]
    async fn cancel_beats_a_pending_pause() {
        let dir = tempfile::tempdir().unwrap();
        let (st, u, b) = setup(dir.path());
        let ex = Arc::new(Fake::new(vec![spec("a", false)]).blocking("a", Block::Pending));
        let rt = Arc::new(Runtime::new(st.clone(), ex));
        let run = rt.create_run(u, b, None, "turn").unwrap();
        let handle = tokio::spawn({
            let rt = rt.clone();
            async move { rt.start(run).await }
        });
        wait_for_intent(&st, run).await;
        assert!(rt.request_pause(run, false));
        rt.cancel(run).unwrap();
        assert_eq!(handle.await.unwrap().unwrap(), RunState::Cancelled);
    }

    #[tokio::test]
    async fn cancel_idle_runs() {
        let dir = tempfile::tempdir().unwrap();
        let (st, u, b) = setup(dir.path());
        let rt = Runtime::new(st, Arc::new(Fake::new(vec![])));
        let run = rt.create_run(u, b, None, "turn").unwrap();
        rt.cancel(run).unwrap();
        assert_eq!(rt.state(run).unwrap(), RunState::Cancelled);
    }

    #[tokio::test]
    async fn pause_queued_and_fail_paused_preserve_and_clear_reason() {
        let dir = tempfile::tempdir().unwrap();
        let (st, u, b) = setup(dir.path());
        let rt = Runtime::new(st.clone(), Arc::new(Fake::new(vec![spec("a", true)])));
        let run1 = rt.create_run(u, b, None, "turn").unwrap();
        assert_eq!(
            rt.pause_queued(run1, Some("waiting_for_pc")).unwrap(),
            RunState::Paused
        );
        let row = st.get_run(run1).unwrap().unwrap();
        assert_eq!(row.pause_reason.as_deref(), Some("waiting_for_pc"));
        assert_eq!(rt.resume(run1).await.unwrap(), RunState::Succeeded);
        assert_eq!(st.get_run(run1).unwrap().unwrap().pause_reason, None);

        let run2 = rt.create_run(u, b, None, "turn").unwrap();
        rt.pause_queued(run2, Some("waiting_for_pc")).unwrap();
        assert_eq!(
            rt.fail_paused(run2, "pc timeout").unwrap(),
            RunState::Failed
        );
        let row2 = st.get_run(run2).unwrap().unwrap();
        assert_eq!(row2.error.as_deref(), Some("pc timeout"));
        assert_eq!(row2.pause_reason, None);
    }
}
