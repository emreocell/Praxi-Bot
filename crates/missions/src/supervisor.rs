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

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::AtomicI64;
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio::sync::Notify;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::brief::{BRIEF_ITEMS, BriefInput, build_brief};
use crate::digest;
use crate::error::{MissionError, Result};
pub use crate::model::MAX_ITEM_ATTEMPTS;
use crate::model::{ItemStatus, Mission, MissionState};
use crate::store::{Leftover, MissionStore, SESSION_LEASE_PREFIX, SessionEndRecord};
use crate::wake::MailProbe;

/// How long the loop waits for the drivers of running sessions after `shutdown` before it
/// aborts them (their runs are cancelled the moment `shutdown` fires, so this is a backstop).
const DRIVER_JOIN_GRACE: Duration = Duration::from_secs(15);

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
    /// Fires when the session has to stop: the owner paused or stopped the mission, or the
    /// daemon is shutting down. A runner that is still waiting for the chat to be free (no
    /// run, no lease yet) must give up then.
    pub cancel: CancellationToken,
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
    ///
    /// `req.cancel` firing before the run exists: return
    /// `SessionResult { end: SessionEnd::Cancelled, model_steps: 0, cost_usd: 0.0 }` without
    /// starting a run. Firing after: cancel the run and return its result.
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
    /// How often the wake loop looks at timers, timeouts and mailboxes.
    pub wake_poll: Duration,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            session_gap: Duration::from_secs(2),
            poll: Duration::from_secs(15),
            wake_poll: Duration::from_secs(10),
        }
    }
}

/// How `run_one_session` ended.
enum Ran {
    /// The session ran; the ledger decides what follows.
    Ended(SessionEnd),
    /// Nothing more to do for now.
    Stop(Step),
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
    /// The session in flight per mission: fired by pause, stop and shutdown. An entry also
    /// keeps two `step`s of one supervisor off the same mission.
    active: Mutex<HashMap<String, CancellationToken>>,
    wake: Notify,
    on_change: Mutex<Option<ChangeHook>>,
    /// Looks into the bot's own mailbox for the wake loop (none: mail waits only time out).
    mail: Mutex<Option<Arc<dyn MailProbe>>>,
    /// When the wake loop last pruned old waits and wakes (`i64::MIN`: never).
    last_prune_ms: AtomicI64,
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
            active: Mutex::new(HashMap::new()),
            wake: Notify::new(),
            on_change: Mutex::new(None),
            mail: Mutex::new(None),
            last_prune_ms: AtomicI64::new(i64::MIN),
        }
    }

    /// Let the wake loop look into mailboxes for `mail` waits.
    pub fn set_mail_probe(&self, probe: Arc<dyn MailProbe>) {
        *self.mail.lock() = Some(probe);
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
        self.stop_session(&m);
        self.changed(id);
        Ok(m)
    }

    /// Stop the session in flight, if any: the runner is told through the session's token
    /// (it may still be waiting for the chat) and the run, once it exists, is cancelled.
    fn stop_session(&self, m: &Mission) {
        if let Some(token) = self.active.lock().get(&m.id) {
            token.cancel();
        }
        if let Some(run) = m
            .lease_run_id
            .as_deref()
            .filter(|r| !r.starts_with(SESSION_LEASE_PREFIX))
        {
            self.runner.cancel_run(run);
        }
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
        self.stop_session(&m);
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
    /// mission is driven by its own task, one session at a time. The returned
    /// handle completes only after every driver is gone: the sessions in flight
    /// are cancelled the moment `shutdown` fires and joined here.
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
            // Timers, timeouts and mailboxes of waiting items, on their own task: a slow mailbox
            // never delays the start of a session.
            let wakes = {
                let wake_loop = Arc::clone(&this);
                tokio::spawn(async move { wake_loop.wake_loop().await })
            };
            let mut drivers: JoinSet<()> = JoinSet::new();
            while !this.shutdown.is_cancelled() {
                match this.store.list_by_state(MissionState::Running) {
                    Ok(list) => {
                        for m in list {
                            if this.driving.lock().insert(m.id.clone()) {
                                let driver = Arc::clone(&this);
                                drivers.spawn(async move {
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
                // Reap the drivers that ended (a panicked one is already logged by tokio).
                while drivers.try_join_next().is_some() {}
                tokio::select! {
                    biased;
                    () = this.shutdown.cancelled() => break,
                    () = this.wake.notified() => {}
                    () = tokio::time::sleep(this.cfg.poll) => {}
                }
            }
            let joined = async { while drivers.join_next().await.is_some() {} };
            if tokio::time::timeout(DRIVER_JOIN_GRACE, joined)
                .await
                .is_err()
            {
                tracing::warn!("missions: a session did not stop in time; aborting it");
                drivers.abort_all();
                while drivers.join_next().await.is_some() {}
                this.active.lock().clear();
            }
            let _ = wakes.await;
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
        // A waiting mission starts no session: the wake loop releases it when a wake fires.
        if m.state != MissionState::Running {
            return Ok(Step::Stopped(m.state));
        }
        // No new session once the daemon is going down (it would be cancelled at once and
        // still count against the session budget).
        if self.shutdown.is_cancelled() {
            return Ok(Step::Stopped(MissionState::Running));
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

        // The token is registered before the session row exists, so a pause or stop that
        // lands at any point from here on reaches the runner.
        let cancel = {
            let mut active = self.active.lock();
            if active.contains_key(id) {
                return Ok(Step::Stopped(MissionState::Running));
            }
            let token = self.shutdown.child_token();
            active.insert(id.to_owned(), token.clone());
            token
        };
        let ran = self.run_one_session(&m, brief, cancel).await;
        self.active.lock().remove(id);
        match ran? {
            Ran::Ended(end) => self.after_session(id, &end).await,
            Ran::Stop(step) => Ok(step),
        }
    }

    /// Begin, run and close one session of `m`.
    async fn run_one_session(
        &self,
        m: &Mission,
        brief: String,
        cancel: CancellationToken,
    ) -> Result<Ran> {
        let id = m.id.as_str();
        let session = match self.store.begin_session(id, self.clock.now_ms()) {
            Ok(session) => session,
            // Another supervisor holds the mission, or it was paused a moment ago.
            Err(MissionError::Busy(_)) => return Ok(Ran::Stop(Step::Stopped(self.get(id)?.state))),
            Err(e) => return Err(e),
        };
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
                    cancel,
                },
                &on_started,
            )
            .await;

        let now = self.clock.now_ms();
        // The daemon is going down: its shutdown cancels or pauses the run. Leave the mission
        // running and the session interrupted; it continues from its ledger after the restart.
        let interrupted = self.shutdown.is_cancelled()
            && matches!(result.end, SessionEnd::Cancelled | SessionEnd::Paused);
        let outcome = if interrupted {
            "interrupted"
        } else {
            result.end.name()
        };
        // The run of a paused session (read before `end_session` releases the lease).
        let paused_run = (!interrupted && result.end == SessionEnd::Paused)
            .then(|| self.get(id).ok().and_then(|m| m.lease_run_id))
            .flatten()
            .filter(|r| !r.starts_with(SESSION_LEASE_PREFIX));
        // What an item left `doing` costs depends on how the session ended: only a session the
        // model closed itself with the item unfinished burns the attempt. The step guard
        // cutting a session is the normal way a long item spans sessions.
        let left = match &result.end {
            _ if interrupted => Leftover::Interrupted,
            SessionEnd::Finished => Leftover::Abandoned,
            SessionEnd::GuardStop => Leftover::Cut,
            SessionEnd::Failed(_) | SessionEnd::Cancelled | SessionEnd::Paused => {
                Leftover::Interrupted
            }
        };
        self.store
            .requeue_stale_doing(id, MAX_ITEM_ATTEMPTS, left, now)?;
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
        if interrupted {
            return Ok(Ran::Stop(Step::Stopped(MissionState::Running)));
        }
        // A session paused by an unanswered approval is abandoned: the next session starts from
        // a fresh brief, so nobody must be able to resume the old run next to it.
        if let Some(run) = paused_run {
            self.runner.cancel_run(&run);
        }
        Ok(Ran::Ended(result.end))
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
                    .block(
                        id,
                        "a session stopped because an approval was not answered in time; resume the mission when you are ready to decide it",
                    )
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
        let waits = self.store.active_waits(id)?;
        let wakes = self.store.pending_wakes(id, 20)?;
        Ok(build_brief(&BriefInput {
            mission: m,
            progress,
            open_items: &open,
            answered: &answered,
            blocked_items: &blocked,
            notes: &pinned,
            session_steps: self.runner.session_steps(),
            wakes: &wakes,
            waits: &waits,
            now_ms: self.clock.now_ms(),
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
        if p.waiting > 0 {
            // Nothing to do until a wake fires: the mission waits instead of holding a session.
            // Checked and changed in one transaction, so a wake that fires now is not missed.
            if self.store.park_if_idle(&m.id, self.clock.now_ms())? {
                self.changed(&m.id);
                return Ok(Some(Step::Stopped(MissionState::Waiting)));
            }
            return Ok(None);
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
        // One digest per ending: the owner's stop and the session winding down both end up
        // here, and the first one to write it is the only one that announces it.
        if m.digest
            .as_ref()
            .is_some_and(|d| d.outcome == m.state.as_str())
        {
            return Ok(());
        }
        let d = digest::build(&self.store, &m, m.state.as_str(), self.clock.now_ms())?;
        if !self.store.set_digest_once(id, &d, self.clock.now_ms())? {
            return Ok(());
        }
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

mod wakes;
pub use wakes::WakePass;

impl crate::wake::ChildWakeSink for Supervisor {
    fn deliver(&self, child_run_id: &str, summary: &str) {
        if let Err(e) = self.deliver_child_finished(child_run_id, summary) {
            tracing::warn!("missions: child wake delivery failed: {e}");
        }
    }
}

#[cfg(test)]
mod tests;
