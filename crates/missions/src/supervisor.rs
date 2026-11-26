//! The supervisor: starts the next session of a mission until it is done.
//!
//! A session is an ordinary agent run (the [`SessionRunner`] starts it) whose
//! first message is the code-built brief. When it ends the supervisor reads the
//! **ledger**, not the model's last words, to decide what happens next:
//! continue with a fresh session, finish, or block for the owner. Budgets are
//! checked here, in code, before every session; the model cannot raise them.
//!
//! The step limit of the agent loop is a *session* end here, not a failure:
//! that is how a mission gets past the 40-model-call wall of one run.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::brief::{BRIEF_ITEMS, BriefInput, build_brief};
use crate::digest;
use crate::error::{MissionError, Result};
use crate::model::{ItemStatus, Mission, MissionState};
use crate::store::{MissionStore, SessionEndRecord};

/// Attempts an item gets (sessions that left it `doing`) before it is given up.
pub const MAX_ITEM_ATTEMPTS: u32 = 3;

/// Source of "now".
pub trait Clock: Send + Sync {
    /// Unix time in milliseconds.
    fn now_ms(&self) -> i64;
}

/// The wall clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
    }
}

/// What a session needs to start.
#[derive(Debug, Clone)]
pub struct SessionRequest {
    /// The mission.
    pub mission_id: String,
    /// The lead bot.
    pub bot_id: String,
    /// The mission's chat.
    pub chat_id: String,
    /// The brief: the session's first message.
    pub brief: String,
    /// 1-based number of this session.
    pub session_no: u32,
}

/// How a session's run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEnd {
    /// The model stopped by itself.
    Finished,
    /// A safety limit (steps, tokens) closed the run. A normal session end.
    GuardStop,
    /// The run failed (provider error, crash of the run).
    Failed(String),
    /// Someone stopped the run, or the daemon is shutting down.
    Cancelled,
    /// The run is paused, e.g. an approval nobody answered in time.
    Paused,
}

impl SessionEnd {
    fn name(&self) -> &'static str {
        match self {
            Self::Finished => "finished",
            Self::GuardStop => "guard_stop",
            Self::Failed(_) => "failed",
            Self::Cancelled => "cancelled",
            Self::Paused => "paused",
        }
    }
}

/// The result of one session.
#[derive(Debug, Clone)]
pub struct SessionResult {
    /// How it ended.
    pub end: SessionEnd,
    /// Model calls it made.
    pub model_steps: u32,
    /// Spend in USD, when known.
    pub cost_usd: f64,
}

/// Starts agent runs for the supervisor. The daemon implements it over the
/// agent loop; tests implement it with a scripted model.
#[async_trait]
pub trait SessionRunner: Send + Sync {
    /// `Err(reason)` when the bot may not run a mission on its own right now
    /// (not at the Full level, bot missing).
    ///
    /// # Errors
    /// The reason, shown to the owner.
    fn bot_ready(&self, bot_id: &str) -> std::result::Result<(), String>;

    /// Model calls one session may make (the loop's step guard), for the brief.
    fn session_steps(&self) -> u32 {
        40
    }

    /// Run one session to the end. `on_started` is called with the run id as
    /// soon as the run exists, so the lease survives in the database.
    async fn run_session(
        &self,
        req: SessionRequest,
        on_started: &(dyn for<'a> Fn(&'a str) + Send + Sync),
    ) -> SessionResult;

    /// Stop a run (the owner paused or stopped the mission).
    fn cancel_run(&self, run_id: &str);

    /// Post the digest where the owner reads it (the mission's chat).
    async fn announce(&self, _mission: &Mission, _text: &str) {}
}

/// Tunables of the supervisor.
#[derive(Debug, Clone)]
pub struct SupervisorConfig {
    /// Pause between two sessions of the same mission.
    pub session_gap: Duration,
    /// How often the loop looks for runnable missions when nobody wakes it.
    pub poll: Duration,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            session_gap: Duration::from_secs(2),
            poll: Duration::from_secs(15),
        }
    }
}

/// What one step of the supervisor did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// A session ran and the mission still has work: call again.
    Continue,
    /// The mission is not running any more (or was not to begin with).
    Stopped(MissionState),
}

/// Callback told that a mission changed (state, ledger, digest).
pub type ChangeHook = Arc<dyn Fn(&str) + Send + Sync>;

/// The supervisor.
pub struct Supervisor {
    store: Arc<MissionStore>,
    runner: Arc<dyn SessionRunner>,
    clock: Arc<dyn Clock>,
    cfg: SupervisorConfig,
    shutdown: CancellationToken,
    driving: Mutex<HashSet<String>>,
    wake: Notify,
    on_change: Mutex<Option<ChangeHook>>,
}

impl std::fmt::Debug for Supervisor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Supervisor").finish_non_exhaustive()
    }
}

impl Supervisor {
    /// A supervisor over `store` that starts sessions through `runner`.
    #[must_use]
    pub fn new(
        store: Arc<MissionStore>,
        runner: Arc<dyn SessionRunner>,
        clock: Arc<dyn Clock>,
        cfg: SupervisorConfig,
        shutdown: CancellationToken,
    ) -> Self {
        Self {
            store,
            runner,
            clock,
            cfg,
            shutdown,
            driving: Mutex::new(HashSet::new()),
            wake: Notify::new(),
            on_change: Mutex::new(None),
        }
    }

    /// Be told when a mission changes.
    pub fn set_change_hook(&self, hook: ChangeHook) {
        *self.on_change.lock() = Some(hook);
    }

    /// The store.
    #[must_use]
    pub fn store(&self) -> &Arc<MissionStore> {
        &self.store
    }

    /// Ask the loop to look for runnable missions now.
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    fn changed(&self, id: &str) {
        if let Some(hook) = self.on_change.lock().clone() {
            hook(id);
        }
    }

    // ---- owner controls ----

    /// Pause a mission; an in-flight session is stopped.
    ///
    /// # Errors
    /// Unknown mission, illegal transition.
    pub fn pause(&self, id: &str) -> Result<Mission> {
        let m = self.store.set_state(
            id,
            MissionState::Paused,
            Some("paused by the owner"),
            self.clock.now_ms(),
        )?;
        if let Some(run) = &m.lease_run_id {
            self.runner.cancel_run(run);
        }
        self.changed(id);
        Ok(m)
    }

    /// Resume a paused or blocked mission.
    ///
    /// # Errors
    /// Unknown mission, illegal transition.
    pub fn resume(&self, id: &str) -> Result<Mission> {
        let m = self
            .store
            .set_state(id, MissionState::Running, None, self.clock.now_ms())?;
        self.changed(id);
        self.wake();
        Ok(m)
    }

    /// Stop a mission for good; an in-flight session is stopped and the owner gets the digest.
    ///
    /// # Errors
    /// Unknown mission, illegal transition.
    pub async fn cancel(&self, id: &str) -> Result<Mission> {
        let m = self.store.set_state(
            id,
            MissionState::Cancelled,
            Some("stopped by the owner"),
            self.clock.now_ms(),
        )?;
        if let Some(run) = &m.lease_run_id {
            self.runner.cancel_run(run);
        }
        self.conclude(id).await?;
        self.get(id)
    }

    /// The owner answered blocked items: release them and run the mission again.
    ///
    /// # Errors
    /// Unknown mission, SQLite failure.
    pub fn answer_blocked(&self, id: &str, answers: &[(String, String)]) -> Result<u32> {
        let n = self
            .store
            .answer_blocked(id, answers, self.clock.now_ms())?;
        if n > 0 {
            self.changed(id);
            self.wake();
        }
        Ok(n)
    }

    fn get(&self, id: &str) -> Result<Mission> {
        self.store
            .get(id)?
            .ok_or_else(|| MissionError::NotFound(id.to_owned()))
    }

    // ---- the loop ----

    /// Recover after a restart, then run until `shutdown`: every `running`
    /// mission is driven by its own task, one session at a time.
    pub fn spawn(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            match this.store.recover_interrupted(this.clock.now_ms()) {
                Ok(runs) if runs.is_empty() => {}
                Ok(runs) => {
                    tracing::info!(
                        "missions: {} interrupted session(s) abandoned; missions continue from their ledger",
                        runs.len()
                    );
                    // The recovered runs sit paused: cancel them so nobody resumes one
                    // next to the session that replaces it.
                    for run in &runs {
                        this.runner.cancel_run(run);
                    }
                }
                Err(e) => tracing::warn!("missions: recovery failed: {e}"),
            }
            loop {
                match this.store.list_by_state(MissionState::Running) {
                    Ok(list) => {
                        for m in list {
                            if this.driving.lock().insert(m.id.clone()) {
                                let driver = Arc::clone(&this);
                                tokio::spawn(async move {
                                    if let Err(e) = driver.drive(&m.id).await {
                                        tracing::warn!("missions: {}: {e}", m.id);
                                    }
                                    driver.driving.lock().remove(&m.id);
                                    // A mission resumed while we were finishing is picked up now.
                                    driver.wake();
                                });
                            }
                        }
                    }
                    Err(e) => tracing::warn!("missions: listing failed: {e}"),
                }
                tokio::select! {
                    () = this.shutdown.cancelled() => break,
                    () = this.wake.notified() => {}
                    () = tokio::time::sleep(this.cfg.poll) => {}
                }
            }
        })
    }

    /// Run sessions of mission `id` until it is no longer `running`.
    ///
    /// # Errors
    /// Storage failures.
    pub async fn drive(&self, id: &str) -> Result<MissionState> {
        loop {
            match self.step(id).await? {
                Step::Continue => {
                    if self.shutdown.is_cancelled() {
                        return Ok(MissionState::Running);
                    }
                    tokio::select! {
                        () = self.shutdown.cancelled() => return Ok(MissionState::Running),
                        () = tokio::time::sleep(self.cfg.session_gap) => {}
                    }
                }
                Step::Stopped(state) => return Ok(state),
            }
        }
    }

    /// One supervisor step: budget checks, then (if the mission may go on) one session.
    ///
    /// # Errors
    /// Storage failures.
    pub async fn step(&self, id: &str) -> Result<Step> {
        let m = self.get(id)?;
        if m.state != MissionState::Running {
            return Ok(Step::Stopped(m.state));
        }
        let now = self.clock.now_ms();
        if let Some(reason) = budget_block(&m, now) {
            return self.block(id, &reason).await;
        }
        if let Err(reason) = self.runner.bot_ready(&m.bot_id) {
            return self.block(id, &reason).await;
        }
        // Everything may already be finished (the owner skipped the last items).
        if let Some(step) = self.finish_if_nothing_open(&m).await? {
            return Ok(step);
        }

        let brief = self.brief_for(&m)?;

        let session = self.store.begin_session(id, now)?;
        self.changed(id);
        let store = Arc::clone(&self.store);
        let mission_id = id.to_owned();
        let on_started = move |run_id: &str| {
            let _ = store.set_lease(&mission_id, Some(run_id));
            let _ = store.set_session_run(session, run_id);
        };
        let result = self
            .runner
            .run_session(
                SessionRequest {
                    mission_id: id.to_owned(),
                    bot_id: m.bot_id.clone(),
                    chat_id: m.chat_id.clone(),
                    brief,
                    session_no: m.sessions + 1,
                },
                &on_started,
            )
            .await;

        let now = self.clock.now_ms();
        // The daemon is going down: leave the mission running, the session
        // interrupted. It continues from its ledger after the restart.
        let outcome = if self.shutdown.is_cancelled() && result.end == SessionEnd::Cancelled {
            "interrupted"
        } else {
            result.end.name()
        };
        self.store.requeue_stale_doing(id, MAX_ITEM_ATTEMPTS, now)?;
        self.store.end_session(
            id,
            session,
            m.ledger_version,
            &SessionEndRecord {
                outcome: outcome.to_owned(),
                model_steps: result.model_steps,
                cost_usd: result.cost_usd,
            },
            now,
        )?;
        self.changed(id);
        if outcome == "interrupted" {
            return Ok(Step::Stopped(MissionState::Running));
        }

        self.after_session(id, &result.end).await
    }

    /// Decide what follows a session that ended normally, from the ledger.
    async fn after_session(&self, id: &str, end: &SessionEnd) -> Result<Step> {
        let m = self.get(id)?;
        if m.state != MissionState::Running {
            // Paused, stopped or completed by a tool call during the session.
            if m.state.is_terminal() || m.state == MissionState::Blocked {
                self.conclude(id).await?;
            }
            return Ok(Step::Stopped(m.state));
        }
        match end {
            SessionEnd::Cancelled => {
                self.store.set_state(
                    id,
                    MissionState::Paused,
                    Some("the session was stopped; resume when ready"),
                    self.clock.now_ms(),
                )?;
                self.changed(id);
                return Ok(Step::Stopped(MissionState::Paused));
            }
            SessionEnd::Paused => {
                return self
                    .block(id, "a session paused while waiting for an approval nobody answered; approve it or resume")
                    .await;
            }
            SessionEnd::Failed(e) if m.failed_sessions >= m.budgets.max_failed_sessions => {
                return self
                    .block(
                        id,
                        &format!(
                            "{} sessions in a row failed; last error: {e}",
                            m.failed_sessions
                        ),
                    )
                    .await;
            }
            _ => {}
        }
        if let Some(step) = self.finish_if_nothing_open(&m).await? {
            return Ok(step);
        }
        if m.stalled_sessions >= m.budgets.max_stalled_sessions {
            return self
                .block(
                    id,
                    &format!(
                        "{} sessions in a row made no progress on the ledger",
                        m.stalled_sessions
                    ),
                )
                .await;
        }
        Ok(Step::Continue)
    }

    /// The brief for the next session, built from the ledger.
    fn brief_for(&self, m: &Mission) -> Result<String> {
        let id = m.id.as_str();
        let items = self.store.items(id, None)?;
        let progress = self.store.progress(id)?;
        let open: Vec<_> = items
            .iter()
            .filter(|i| i.status.is_open())
            .take(BRIEF_ITEMS * 2)
            .cloned()
            .collect();
        let answered: Vec<_> = open
            .iter()
            .filter(|i| i.owner_answer.is_some())
            .cloned()
            .collect();
        let blocked: Vec<_> = items
            .iter()
            .filter(|i| i.status == ItemStatus::Blocked)
            .cloned()
            .collect();
        let pinned: Vec<_> = self
            .store
            .notes(id, false)?
            .into_iter()
            .filter(|n| n.pinned || n.kind == "decision" || n.kind == "warning")
            .collect();
        Ok(build_brief(&BriefInput {
            mission: m,
            progress,
            open_items: &open,
            answered: &answered,
            blocked_items: &blocked,
            notes: &pinned,
            session_steps: self.runner.session_steps(),
        }))
    }

    /// With no open item left: finish if the goal is met, hand the blocked
    /// items to the owner if any, otherwise let one more session plan or
    /// complete. `None` = there is still work.
    async fn finish_if_nothing_open(&self, m: &Mission) -> Result<Option<Step>> {
        let p = self.store.progress(&m.id)?;
        if p.total == 0 || p.open() > 0 {
            return Ok(None);
        }
        if p.criterion_met(m.target_done) {
            self.store.set_state(
                &m.id,
                MissionState::Done,
                Some("the goal is met"),
                self.clock.now_ms(),
            )?;
            self.conclude(&m.id).await?;
            return Ok(Some(Step::Stopped(MissionState::Done)));
        }
        if p.blocked > 0 {
            let reason = format!(
                "{} item(s) need you; everything else is finished",
                p.blocked
            );
            return Ok(Some(self.block(&m.id, &reason).await?));
        }
        Ok(None)
    }

    async fn block(&self, id: &str, reason: &str) -> Result<Step> {
        self.store
            .set_state(id, MissionState::Blocked, Some(reason), self.clock.now_ms())?;
        self.conclude(id).await?;
        Ok(Step::Stopped(MissionState::Blocked))
    }

    /// Write the owner digest for a mission that just ended or blocked, post
    /// it to the chat and tell listeners.
    ///
    /// # Errors
    /// Storage failures.
    pub async fn conclude(&self, id: &str) -> Result<()> {
        let m = self.get(id)?;
        let d = digest::build(&self.store, &m, m.state.as_str(), self.clock.now_ms())?;
        self.store.set_digest(id, &d, self.clock.now_ms())?;
        let m = self.get(id)?;
        self.changed(id);
        self.runner.announce(&m, &digest::render_text(&m, &d)).await;
        Ok(())
    }
}

/// The first budget the mission has used up, as a sentence for the owner.
#[must_use]
pub fn budget_block(m: &Mission, now_ms: i64) -> Option<String> {
    let b = &m.budgets;
    if m.sessions >= b.max_sessions {
        return Some(format!(
            "session budget used up: {} of {} sessions",
            m.sessions, b.max_sessions
        ));
    }
    if m.model_steps >= b.max_model_steps {
        return Some(format!(
            "model-call budget used up: {} of {} calls",
            m.model_steps, b.max_model_steps
        ));
    }
    let minutes = (now_ms.saturating_sub(m.started_at_ms)).max(0) / 60_000;
    if u64::try_from(minutes).unwrap_or(0) >= b.max_minutes {
        return Some(format!("deadline reached after {} minutes", b.max_minutes));
    }
    if let Some(cap) = b.max_cost_usd
        && m.cost_usd >= cap
    {
        return Some(format!(
            "spend cap reached: ${:.2} of ${cap:.2}",
            m.cost_usd
        ));
    }
    None
}

#[cfg(test)]
mod tests;
