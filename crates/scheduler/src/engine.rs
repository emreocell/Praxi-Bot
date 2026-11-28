//! Background [`Scheduler`] engine: single Tokio task sleeping until the
//! nearest `next_run_at`, waking on store changes, detecting wall-clock jumps
//! via monotonic vs. wall time, applying [`MisfirePolicy`], reserving durable
//! occurrence intents, and enforcing a per-schedule concurrency limit of 1.
//!
//! Recovery boundary: pending and interrupted dispatches survive in SQLite but
//! are deliberately NOT replayed automatically on startup. An explicit owner
//! retry goes through [`ScheduleExecutor::execute_attempt`], whose
//! `schedule_id:due_ms:attempt` key the executor stores durably in the run
//! record, so starting the same attempt twice returns the existing run, also
//! after a restart. A retry never starts a second run while the occurrence's
//! run is live, paused or waiting ([`ScheduleExecutor::attempt_run`]).
//! One-shot timers are consumed at reservation, but never disabled merely
//! because an intent was written.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use protocol::TaskId;
use tokio::sync::{Notify, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::clock::{Clock, SystemClock};
use crate::error::{Result, SchedulerError};
use crate::model::{AttemptRun, MisfirePolicy, Payload, Schedule, ScheduleSpec};
use crate::store::ScheduleStore;
use crate::webhook::{PendingReplay, ReplayGuard, WebhookMeta, prepare_webhook};

/// Discrepancy between wall-clock delta and monotonic delta (in milliseconds)
/// above which a wall-clock jump (OS sleep/resume, DST shift, manual clock
/// change) is declared.
pub const CLOCK_JUMP_THRESHOLD_MS: i64 = 2_000;

/// Grace window (in milliseconds) within which a normal timer wakeup is
/// considered on-time rather than a misfire.
pub const MISFIRE_GRACE_MS: i64 = 5_000;

/// Maximum sleep slice when no schedule has a nearer `next_run_at` so periodic
/// wall-clock checks remain bounded.
const MAX_IDLE_SLEEP_MS: u64 = 1_000;

/// Callback invoked by the [`Scheduler`] when a schedule or webhook fires.
#[async_trait]
pub trait ScheduleExecutor: Send + Sync + 'static {
    /// Execute a single triggered occurrence of `schedule`.
    async fn execute(&self, schedule: &Schedule) -> Result<()>;

    /// Execute a scheduled occurrence identified by `due_ms` (defaults to
    /// [`Self::execute`]).
    async fn execute_occurrence(&self, schedule: &Schedule, _due_ms: i64) -> Result<()> {
        self.execute(schedule).await
    }

    /// Execute attempt `attempt` of occurrence `due_ms`. The executor makes
    /// `schedule_id:due_ms:attempt` an idempotency key of the run it starts
    /// (defaults to [`Self::execute_occurrence`]).
    async fn execute_attempt(&self, schedule: &Schedule, due_ms: i64, _attempt: u32) -> Result<()> {
        self.execute_occurrence(schedule, due_ms).await
    }

    /// What exists under the run key of attempt `attempt` of occurrence
    /// `due_ms`. Read-only; the default knows no runs.
    fn attempt_run(&self, _schedule_id: TaskId, _due_ms: i64, _attempt: u32) -> AttemptRun {
        AttemptRun::None
    }

    /// Read-only host for a precheck; never creates an agent run.
    async fn precheck(
        &self,
        _schedule: &Schedule,
        _previous: Option<&str>,
    ) -> Result<crate::precheck::Outcome> {
        Err(SchedulerError::Execution(
            "precheck host unavailable".into(),
        ))
    }

    /// Bounded filesystem metadata or e-mail headers, through host permission gates.
    async fn poll_trigger(
        &self,
        _schedule: &Schedule,
        _trigger: &crate::Trigger,
    ) -> Result<Vec<crate::watch::Observation>> {
        Err(SchedulerError::Execution("trigger host unavailable".into()))
    }
}

struct InFlightGuard {
    inner: Arc<Inner>,
    schedule_id: TaskId,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.inner.in_flight.lock().remove(&self.schedule_id);
        self.inner.in_flight_notify.notify_waiters();
        self.inner.in_flight_notify.notify_one();
    }
}

struct Inner {
    store: Arc<ScheduleStore>,
    executor: Arc<dyn ScheduleExecutor>,
    clock: Arc<dyn Clock>,
    cancel: CancellationToken,
    in_flight: Mutex<HashSet<TaskId>>,
    in_flight_notify: Notify,
    skipped_by_schedule: Mutex<HashMap<TaskId, u64>>,
    total_skipped: AtomicU64,
    wall_jumps: AtomicU64,
    req_seq: AtomicU64,
    pass_tx: watch::Sender<u64>,
    replay_guard: ReplayGuard,
    watch_slots: Arc<tokio::sync::Semaphore>,
}

/// Handle to the running scheduler engine.
///
/// Cloning [`Scheduler`] shares the same underlying background task. Dropping
/// the last handle does not abruptly abort in-flight work unless
/// [`Scheduler::stop`] or [`Scheduler::shutdown`] is called, or the Tokio
/// runtime shuts down.
#[derive(Clone)]
pub struct Scheduler {
    inner: Arc<Inner>,
    task: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl std::fmt::Debug for Scheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scheduler")
            .field("total_skipped", &self.total_skipped_runs())
            .field("wall_jumps", &self.wall_clock_jumps_detected())
            .finish_non_exhaustive()
    }
}

impl Scheduler {
    /// Start the scheduler engine over `store` and `executor` using the system
    /// clock ([`SystemClock`]).
    #[must_use]
    pub fn start(store: Arc<ScheduleStore>, executor: Arc<dyn ScheduleExecutor>) -> Self {
        Self::start_with_clock(store, executor, Arc::new(SystemClock::new()))
    }

    /// Start the scheduler engine with a custom [`Clock`] (e.g. [`crate::FakeClock`]).
    #[must_use]
    pub fn start_with_clock(
        store: Arc<ScheduleStore>,
        executor: Arc<dyn ScheduleExecutor>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let (pass_tx, _pass_rx) = watch::channel(0_u64);
        let inner = Arc::new(Inner {
            store,
            executor,
            clock,
            cancel: CancellationToken::new(),
            in_flight: Mutex::new(HashSet::new()),
            in_flight_notify: Notify::new(),
            skipped_by_schedule: Mutex::new(HashMap::new()),
            total_skipped: AtomicU64::new(0),
            wall_jumps: AtomicU64::new(0),
            req_seq: AtomicU64::new(0),
            pass_tx,
            replay_guard: ReplayGuard::default(),
            watch_slots: Arc::new(tokio::sync::Semaphore::new(4)),
        });

        let init_wall = inner.clock.wall_ms();
        let init_mono = inner.clock.mono_ms();

        // Nothing is dispatching yet, so every unresolved row is a leftover of
        // an earlier process. Reclassify it as visible history; no replay.
        match inner.store.recover_unresolved_occurrences() {
            Ok(0) => {}
            Ok(count) => tracing::warn!(
                count,
                "marked unresolved schedule occurrences missed/interrupted; automatic replay awaits durable idempotent starts"
            ),
            Err(error) => tracing::error!(%error, "failed to recover unresolved schedule intents"),
        }

        // A run that finished under its durable key before the process died is
        // done, not interrupted: the key outlives the occurrence row.
        match inner.store.interrupted_attempts() {
            Ok(rows) => {
                for (id, due, attempt) in rows {
                    if inner.executor.attempt_run(id, due, attempt) == AttemptRun::Succeeded
                        && let Err(error) = inner.store.resolve_interrupted_as_succeeded(id, due)
                    {
                        tracing::error!(%error, "failed to settle an interrupted occurrence");
                    }
                }
            }
            Err(error) => tracing::error!(%error, "failed to list interrupted occurrences"),
        }

        // Run the startup recovery/misfire pass synchronously so any schedule
        // already overdue at startup is handled deterministically before the
        // caller advances a fake clock.
        if let Err(e) = process_due(&inner, init_wall, false, true) {
            tracing::error!(error = %e, "scheduler startup pass failed");
        }

        let loop_inner = Arc::clone(&inner);
        let handle = tokio::spawn(async move {
            run_loop(loop_inner, init_wall, init_mono).await;
        });

        Self {
            inner,
            task: Arc::new(Mutex::new(Some(handle))),
        }
    }

    /// Underlying [`ScheduleStore`].
    #[must_use]
    pub fn store(&self) -> &Arc<ScheduleStore> {
        &self.inner.store
    }

    /// Verify a webhook's `HMAC-SHA256` signature and fire the schedule's
    /// payload with `body` available as `{input}`.
    ///
    /// # Errors
    /// Returns an error if the schedule or trigger is missing, disabled, the
    /// signature is invalid, or execution fails.
    pub async fn handle_webhook(&self, id: TaskId, body: &str, signature: &str) -> Result<()> {
        self.handle_webhook_with_meta(id, body, signature, &WebhookMeta::default())
            .await
    }

    /// As [`Self::handle_webhook`] with optional replay-protection metadata.
    ///
    /// # Errors
    /// As [`Self::handle_webhook`]; a replayed delivery id or stale timestamp
    /// yields [`SchedulerError::InvalidSignature`].
    pub async fn handle_webhook_with_meta(
        &self,
        id: TaskId,
        body: &str,
        signature: &str,
        meta: &WebhookMeta,
    ) -> Result<()> {
        let now_ms = self.inner.clock.wall_ms();
        let (fired, pending) = prepare_webhook(
            &self.inner.store,
            id,
            body,
            signature,
            now_ms,
            meta,
            Some(&self.inner.replay_guard),
        )?;
        // Same per-schedule concurrency limit (1) as timer-driven runs.
        let Some(_guard) = try_acquire_in_flight(&self.inner, &fired) else {
            return Err(SchedulerError::Busy(id.to_string()));
        };
        self.commit_replay(pending, now_ms)?;
        execute_checked(
            &self.inner.store,
            self.inner.executor.as_ref(),
            &fired,
            None,
            now_ms,
            false,
        )
        .await
        .map(|_| ())
    }

    /// Like [`Self::handle_webhook`], but returns as soon as the request is
    /// authenticated and accepted: the execution runs in the background (it may
    /// take as long as an agent turn).  The same errors are returned up front,
    /// including [`SchedulerError::Busy`] while a run of `id` is in flight.
    ///
    /// # Errors
    /// As [`Self::handle_webhook`], except execution errors (those are logged).
    pub fn trigger_webhook(&self, id: TaskId, body: &str, signature: &str) -> Result<()> {
        self.trigger_webhook_with_meta(id, body, signature, &WebhookMeta::default())
    }

    /// As [`Self::trigger_webhook`] with optional replay-protection metadata.
    ///
    /// # Errors
    /// As [`Self::trigger_webhook`]; a replayed delivery id or stale timestamp
    /// yields [`SchedulerError::InvalidSignature`].
    pub fn trigger_webhook_with_meta(
        &self,
        id: TaskId,
        body: &str,
        signature: &str,
        meta: &WebhookMeta,
    ) -> Result<()> {
        let now_ms = self.inner.clock.wall_ms();
        let (fired, pending) = prepare_webhook(
            &self.inner.store,
            id,
            body,
            signature,
            now_ms,
            meta,
            Some(&self.inner.replay_guard),
        )?;
        let Some(guard) = try_acquire_in_flight(&self.inner, &fired) else {
            return Err(SchedulerError::Busy(id.to_string()));
        };
        self.commit_replay(pending, now_ms)?;
        spawn_direct_execution(&self.inner, fired, guard, false);
        Ok(())
    }

    /// Remember an accepted webhook request; a replay that raced past the
    /// early check is refused here (the in-flight slot is released on return).
    fn commit_replay(&self, pending: Option<PendingReplay>, now_ms: i64) -> Result<()> {
        match pending {
            Some(p) => {
                if self.inner.replay_guard.commit(p, now_ms) {
                    Ok(())
                } else {
                    Err(SchedulerError::InvalidSignature)
                }
            }
            None => Ok(()),
        }
    }

    /// Fire schedule `id` now (`run-now`), in the background, without touching
    /// its timer (`next_run_at`).  Works on disabled schedules too.
    ///
    /// # Errors
    /// [`SchedulerError::NotFound`] for an unknown id,
    /// [`SchedulerError::Busy`] while a run of `id` is in flight.
    pub fn run_now(&self, id: TaskId, bypass_standing_hours: bool) -> Result<()> {
        let mut schedule = self
            .inner
            .store
            .get(id)?
            .ok_or_else(|| SchedulerError::NotFound(format!("schedule {id}")))?;
        if self.inner.store.frozen_for(id)?.is_some() {
            return Err(SchedulerError::Frozen(id.to_string()));
        }
        let Some(guard) = try_acquire_in_flight(&self.inner, &schedule) else {
            return Err(SchedulerError::Busy(id.to_string()));
        };
        let now_ms = self.inner.clock.wall_ms();
        self.inner.store.touch_last_run(id, now_ms)?;
        schedule.last_run_at = Some(now_ms);
        spawn_direct_execution(&self.inner, schedule, guard, bypass_standing_hours);
        Ok(())
    }

    /// Explicitly retry a `failed`, `missed`, or `interrupted` occurrence
    /// (`schedule_id:due_ms`) in the background using the current schedule
    /// definition and without touching `next_run_at`. Atomically claims the
    /// occurrence in SQLite so concurrent or repeat retries never double-fire.
    ///
    /// # Errors
    /// [`SchedulerError::NotFound`] if the schedule or occurrence is missing,
    /// [`SchedulerError::Frozen`] if the schedule moved to a server,
    /// [`SchedulerError::Disabled`] if it is disabled,
    /// [`SchedulerError::RateLimited`] within 30 s of the previous retry,
    /// [`SchedulerError::Busy`] if a run of `id` is currently in flight,
    /// [`SchedulerError::Conflict`] if the occurrence is already pending,
    /// dispatched, or succeeded.
    pub fn retry_occurrence(&self, id: TaskId, due_ms: i64) -> Result<()> {
        let schedule = self
            .inner
            .store
            .get(id)?
            .ok_or_else(|| SchedulerError::NotFound(format!("schedule {id}")))?;
        if self.inner.store.frozen_for(id)?.is_some() {
            return Err(SchedulerError::Frozen(id.to_string()));
        }
        let Some(guard) = try_acquire_in_flight(&self.inner, &schedule) else {
            return Err(SchedulerError::Busy(id.to_string()));
        };
        let now_ms = self.inner.clock.wall_ms();
        let executor = Arc::clone(&self.inner.executor);
        let (claimed, attempt) =
            self.inner
                .store
                .claim_retry_occurrence(id, due_ms, now_ms, &|attempt| {
                    executor.attempt_run(id, due_ms, attempt)
                })?;
        spawn_retry_execution(&self.inner, claimed, due_ms, attempt, guard);
        Ok(())
    }

    /// Number of occurrences of `id` skipped due to the per-schedule
    /// concurrency limit of 1.
    #[must_use]
    pub fn skipped_runs(&self, id: TaskId) -> u64 {
        self.inner
            .skipped_by_schedule
            .lock()
            .get(&id)
            .copied()
            .unwrap_or(0)
    }

    /// Total number of occurrences skipped across all schedules due to the
    /// concurrency limit of 1.
    #[must_use]
    pub fn total_skipped_runs(&self) -> u64 {
        self.inner.total_skipped.load(Ordering::SeqCst)
    }

    /// Number of wall-clock jumps detected by the engine loop.
    #[must_use]
    pub fn wall_clock_jumps_detected(&self) -> u64 {
        self.inner.wall_jumps.load(Ordering::SeqCst)
    }

    /// Whether `id` currently has an execution in flight.
    #[must_use]
    pub fn is_in_flight(&self, id: TaskId) -> bool {
        self.inner.in_flight.lock().contains(&id)
    }

    /// Wake the scheduler loop and wait until it completes a full scheduling
    /// pass (spawning any newly due executions and recording any skips).
    pub async fn tick(&self) {
        if self.inner.cancel.is_cancelled() {
            return;
        }
        let mut rx = self.inner.pass_tx.subscribe();
        let target = self.inner.req_seq.fetch_add(1, Ordering::SeqCst) + 1;
        self.inner.store.notify();
        while *rx.borrow_and_update() < target {
            if rx.changed().await.is_err() {
                break;
            }
        }
    }

    /// Wake the scheduler loop, wait for a full scheduling pass, and then wait
    /// until all spawned in-flight executions have completed.
    pub async fn settle(&self) {
        self.tick().await;
        loop {
            let notified = self.inner.in_flight_notify.notified();
            if self.inner.in_flight.lock().is_empty() {
                break;
            }
            notified.await;
        }
    }

    /// Signal the background scheduler loop to stop.
    pub fn stop(&self) {
        self.inner.cancel.cancel();
        self.inner.store.notify();
    }

    /// Signal the background scheduler loop to stop and await its termination.
    pub async fn shutdown(&self) {
        self.stop();
        let handle = self.task.lock().take();
        if let Some(h) = handle {
            let _ = h.await;
        }
    }
}

async fn run_loop(inner: Arc<Inner>, mut prev_wall_ms: i64, mut prev_mono_ms: i64) {
    loop {
        if inner.cancel.is_cancelled() {
            break;
        }

        let observed_req = inner.req_seq.load(Ordering::SeqCst);
        let now_wall_ms = inner.clock.wall_ms();
        let now_mono_ms = inner.clock.mono_ms();

        let delta_wall = now_wall_ms.saturating_sub(prev_wall_ms);
        let delta_mono = now_mono_ms.saturating_sub(prev_mono_ms);
        prev_wall_ms = now_wall_ms;
        prev_mono_ms = now_mono_ms;

        let clock_jumped = (delta_wall - delta_mono).abs() > CLOCK_JUMP_THRESHOLD_MS;
        if clock_jumped {
            inner.wall_jumps.fetch_add(1, Ordering::SeqCst);
            tracing::info!(
                delta_wall_ms = delta_wall,
                delta_mono_ms = delta_mono,
                "detected wall-clock jump; applying misfire policies"
            );
        }

        if clock_jumped
            && delta_wall < delta_mono
            && let Err(e) = reanchor_after_backward_jump(&inner, now_wall_ms)
        {
            tracing::error!(error = %e, "failed to re-anchor schedules after clock moved backwards");
        }

        if let Err(e) = process_due(&inner, now_wall_ms, clock_jumped, false) {
            tracing::error!(error = %e, "error processing due schedules");
        }

        if let Err(error) = poll_triggers(&inner, now_wall_ms) {
            let _ = error;
            tracing::error!("schedule trigger pass failed");
        }
        let _ = inner.pass_tx.send(observed_req);

        // If another tick was requested while we were running, loop immediately.
        if inner.req_seq.load(Ordering::SeqCst) > observed_req {
            continue;
        }

        let nearest = match inner.store.nearest_next_run_at() {
            Ok(n) => n,
            Err(e) => {
                tracing::error!(error = %e, "failed to query nearest_next_run_at");
                None
            }
        };

        let cur_wall = inner.clock.wall_ms();
        if let Some(target_ms) = nearest
            && target_ms <= cur_wall
        {
            continue;
        }

        // Never sleep longer than one slice: OS suspend can stop the monotonic
        // timer, so waking regularly lets us notice a wall-clock jump promptly.
        let wait_ms = nearest
            .map_or(MAX_IDLE_SLEEP_MS, |target_ms| {
                u64::try_from((target_ms - cur_wall).max(0)).unwrap_or(MAX_IDLE_SLEEP_MS)
            })
            .min(MAX_IDLE_SLEEP_MS);

        tokio::select! {
            biased;
            () = inner.cancel.cancelled() => break,
            () = inner.store.notified() => {},
            () = inner.clock.sleep(Duration::from_millis(wait_ms)) => {},
        }
    }
}

fn process_due(
    inner: &Arc<Inner>,
    now_wall_ms: i64,
    clock_jumped: bool,
    is_startup: bool,
) -> Result<()> {
    let due_list = inner.store.list_due(now_wall_ms)?;
    for schedule in due_list {
        let Some(first_due_ms) = schedule.next_run_at else {
            continue;
        };
        if first_due_ms > now_wall_ms {
            continue;
        }

        let lateness_ms = now_wall_ms.saturating_sub(first_due_ms);
        let is_misfire = (is_startup && first_due_ms < now_wall_ms)
            || clock_jumped
            || lateness_ms > MISFIRE_GRACE_MS;

        let id = schedule.id;
        let outcome = if is_misfire {
            handle_misfire(inner, &schedule, first_due_ms, now_wall_ms)
        } else {
            handle_on_time(inner, &schedule, first_due_ms, now_wall_ms)
        };
        if let Err(e) = outcome {
            // One broken schedule (corrupt spec, DB hiccup) must not starve the
            // others nor spin the loop: park it as disabled and keep going.
            tracing::error!(schedule_id = %id, error = %e, "schedule failed; disabling it");
            let _ = inner.store.disable_if_current(&schedule);
        }
    }
    Ok(())
}

/// After the wall clock moved backwards, an occurrence persisted under the old
/// (later) clock could be far in the future. Pull recurring schedules back to
/// their next occurrence under the new clock so they are not silently late.
fn reanchor_after_backward_jump(inner: &Arc<Inner>, now_wall_ms: i64) -> Result<()> {
    for schedule in inner.store.list()? {
        if !schedule.enabled || matches!(schedule.spec, ScheduleSpec::At { .. }) {
            continue;
        }
        let Some(current) = schedule.next_run_at else {
            continue;
        };
        if let Some(next) = schedule.spec.next_after(now_wall_ms)?
            && next < current
        {
            inner.store.advance_if_current(&schedule, Some(next))?;
        }
    }
    Ok(())
}

fn handle_on_time(
    inner: &Arc<Inner>,
    schedule: &Schedule,
    first_due_ms: i64,
    now_wall_ms: i64,
) -> Result<()> {
    let next_future = schedule.spec.next_after_due(first_due_ms, now_wall_ms)?;
    reserve_and_spawn(inner, schedule, vec![first_due_ms], next_future)
}

fn handle_misfire(
    inner: &Arc<Inner>,
    schedule: &Schedule,
    first_due_ms: i64,
    now_wall_ms: i64,
) -> Result<()> {
    match schedule.misfire_policy {
        MisfirePolicy::Skip => {
            let next_future = schedule.spec.next_after_due(first_due_ms, now_wall_ms)?;
            inner.store.advance_if_current(schedule, next_future)?;
            Ok(())
        }
        MisfirePolicy::RunOnce if schedule.catchup_window_secs.is_none() => {
            handle_on_time(inner, schedule, first_due_ms, now_wall_ms)
        }
        MisfirePolicy::RunOnce | MisfirePolicy::RunAll => {
            let (all_slots, next_future) = schedule
                .spec
                .collect_due_occurrences(first_due_ms, now_wall_ms)?;
            // Slots older than the window are never run late: they are recorded
            // as `missed` so the owner sees them (and can run one by hand).
            let oldest_ok = schedule.catchup_window_secs.map(|w| {
                let w_ms = i64::try_from(w)
                    .unwrap_or(i64::MAX / 1000)
                    .saturating_mul(1000);
                now_wall_ms.saturating_sub(w_ms)
            });
            let (mut fresh, mut missed): (Vec<i64>, Vec<i64>) = all_slots
                .into_iter()
                .partition(|s| oldest_ok.is_none_or(|limit| *s >= limit));
            if schedule.misfire_policy == MisfirePolicy::RunOnce {
                // One catch-up run (the first in-window slot); the rest are skipped by policy.
                fresh.truncate(1);
            }
            missed.sort_unstable();
            reserve_and_spawn_with_missed(inner, schedule, fresh, &missed, next_future)
        }
    }
}

fn reserve_and_spawn(
    inner: &Arc<Inner>,
    schedule: &Schedule,
    due_slots: Vec<i64>,
    next_future: Option<i64>,
) -> Result<()> {
    reserve_and_spawn_with_missed(inner, schedule, due_slots, &[], next_future)
}

fn reserve_and_spawn_with_missed(
    inner: &Arc<Inner>,
    schedule: &Schedule,
    due_slots: Vec<i64>,
    missed_slots: &[i64],
    next_future: Option<i64>,
) -> Result<()> {
    if due_slots.is_empty() {
        if missed_slots.is_empty() {
            inner.store.advance_if_current(schedule, next_future)?;
        } else {
            inner.store.reserve_occurrences_with_missed(
                schedule,
                &[],
                missed_slots,
                next_future,
            )?;
        }
        return Ok(());
    }
    let Some(guard) = try_acquire_in_flight(inner, schedule) else {
        inner.store.advance_if_current(schedule, next_future)?;
        return Ok(());
    };
    if inner.store.reserve_occurrences_with_missed(
        schedule,
        &due_slots,
        missed_slots,
        next_future,
    )? {
        spawn_timer_executions(inner, schedule.id, schedule.revision, due_slots, guard);
    }
    Ok(())
}

fn try_acquire_in_flight(inner: &Arc<Inner>, schedule: &Schedule) -> Option<InFlightGuard> {
    let mut in_flight = inner.in_flight.lock();
    if in_flight.contains(&schedule.id) {
        tracing::warn!(
            schedule_id = %schedule.id,
            "skipping scheduled run: previous execution is still in flight (concurrency limit = 1)"
        );
        drop(in_flight);
        inner.total_skipped.fetch_add(1, Ordering::SeqCst);
        *inner
            .skipped_by_schedule
            .lock()
            .entry(schedule.id)
            .or_insert(0) += 1;
        return None;
    }
    in_flight.insert(schedule.id);
    Some(InFlightGuard {
        inner: Arc::clone(inner),
        schedule_id: schedule.id,
    })
}

fn spawn_direct_execution(
    inner: &Arc<Inner>,
    schedule: Schedule,
    guard: InFlightGuard,
    bypass_standing_hours: bool,
) {
    let inner = Arc::clone(inner);
    tokio::spawn(async move {
        let _guard = guard;
        if let Err(_e) = execute_checked(
            &inner.store,
            inner.executor.as_ref(),
            &schedule,
            None,
            inner.clock.wall_ms(),
            bypass_standing_hours,
        )
        .await
        {
            tracing::error!(
                schedule_id = %schedule.id,
                "scheduled execution failed"
            );
        }
    });
}

fn spawn_retry_execution(
    inner: &Arc<Inner>,
    schedule: Schedule,
    due_ms: i64,
    attempt: u32,
    guard: InFlightGuard,
) {
    let inner = Arc::clone(inner);
    tokio::spawn(async move {
        let _guard = guard;
        let id = schedule.id;
        let outcome = execute_checked(
            &inner.store,
            inner.executor.as_ref(),
            &schedule,
            Some((due_ms, attempt)),
            inner.clock.wall_ms(),
            false,
        )
        .await;
        if let Err(error) = &outcome {
            tracing::error!(schedule_id = %id, due_ms, %error, "retried execution failed");
        }
        // A retry that did not run (frozen, precheck said no, row deleted,
        // definition edited meanwhile) is not a success: record it as failed so
        // history stays honest and Retry stays available.
        let ran = matches!(outcome, Ok(Dispatch::Ran));
        if let Err(error) = inner.store.finish_occurrence(id, due_ms, ran) {
            tracing::error!(schedule_id = %id, due_ms, %error, "failed to record retried occurrence outcome");
        }
    });
}

fn spawn_timer_executions(
    inner: &Arc<Inner>,
    id: TaskId,
    revision: i64,
    due_slots: Vec<i64>,
    guard: InFlightGuard,
) {
    let inner = Arc::clone(inner);
    tokio::spawn(async move {
        let _guard = guard;
        for due_ms in due_slots {
            if inner.cancel.is_cancelled() {
                break;
            }
            // Never keep executing an old cloned payload after a delete,
            // disable or edit. The store rechecks this inside the claim
            // transaction too, closing the read-to-claim race.
            match inner.store.get(id) {
                Ok(Some(current)) if current.enabled && current.revision == revision => {}
                Ok(_) => {
                    tracing::info!(schedule_id = %id, "catch-up cancelled: schedule changed");
                    break;
                }
                Err(error) => {
                    tracing::error!(schedule_id = %id, %error, "catch-up cancelled: read failed");
                    break;
                }
            }
            let schedule = match inner.store.claim_occurrence(
                id,
                revision,
                due_ms,
                inner.clock.wall_ms(),
            ) {
                Ok(Some(schedule)) => schedule,
                Ok(None) => {
                    // A backward clock adjustment can revisit an identity
                    // that was already dispatched. Skip that identity but
                    // still inspect later slots; definition cancellation is
                    // rechecked at the top of every iteration.
                    tracing::info!(schedule_id = %id, due_ms, "occurrence already consumed or cancelled");
                    continue;
                }
                Err(error) => {
                    tracing::error!(schedule_id = %id, %error, "catch-up cancelled: claim failed");
                    break;
                }
            };
            let occurrence = format!("{id}:{due_ms}");
            let outcome = execute_checked(
                &inner.store,
                inner.executor.as_ref(),
                &schedule,
                Some((due_ms, 0)),
                inner.clock.wall_ms(),
                false,
            )
            .await;
            if let Err(error) = &outcome {
                tracing::error!(%occurrence, %error, "scheduled execution failed");
            }
            if let Err(error) = inner.store.finish_occurrence(id, due_ms, outcome.is_ok()) {
                tracing::error!(%occurrence, %error, "failed to record occurrence outcome");
                break;
            }
        }
    });
}

/// Whether [`execute_checked`] handed the occurrence to the executor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Dispatch {
    /// The executor ran it (its own result is the `Result`).
    Ran,
    /// Nothing ran: frozen, precheck said no, deleted, or edited meanwhile.
    Skipped,
}

/// Whether a standing order may fire now: inside its active hours and under today's cap.
/// Read-only, so a gated firing leaves the check state, queued events and the count alone;
/// ask it before any of those is touched. Other payloads always may run.
fn standing_order_open(
    store: &ScheduleStore,
    schedule: &Schedule,
    now: i64,
    bypass_hours: bool,
) -> Result<bool> {
    let Payload::StandingOrder {
        active_hours,
        max_runs_per_day,
        ..
    } = &schedule.payload
    else {
        return Ok(true);
    };
    if let Some(hours) = active_hours
        && !bypass_hours
        && !hours.contains(now)?
    {
        return Ok(false);
    }
    let today = schedule.payload.day_key(now)?;
    Ok(store.standing_order_runs(schedule.id, &today)? < *max_runs_per_day)
}

/// [`standing_order_open`] for the poll loop, where one broken row must not stop the rest.
fn gate_open(inner: &Inner, schedule: &Schedule, now: i64) -> bool {
    standing_order_open(&inner.store, schedule, now, false).unwrap_or_else(|error| {
        tracing::error!(schedule_id = %schedule.id, %error, "standing order gate unreadable");
        false
    })
}

/// A firing a standing order's hours or cap turned away: nothing ran, and history says why
/// instead of reading as a success.
fn note_gated(store: &ScheduleStore, schedule: &Schedule, occurrence: Option<(i64, u32)>) {
    if let Some((due, _)) = occurrence
        && let Err(error) = store.note_standing_gated(schedule.id, due)
    {
        tracing::error!(schedule_id = %schedule.id, due, %error, "failed to note a gated firing");
    }
}

/// Start the executor for `fired`. A standing order takes its daily slot here, in the same
/// transaction that checks its revision, right before the executor is called; a start that
/// fails before anything began ([`SchedulerError::NotStarted`], e.g. no connection to the
/// model) gives the slot back.
async fn launch(
    store: &ScheduleStore,
    executor: &dyn ScheduleExecutor,
    fired: &Schedule,
    occurrence: Option<(i64, u32)>,
    now: i64,
) -> Result<Dispatch> {
    let slot = if let Payload::StandingOrder {
        max_runs_per_day, ..
    } = &fired.payload
    {
        let day = fired.payload.day_key(now)?;
        if !store.claim_standing_order_run(fired.id, fired.revision, &day, *max_runs_per_day)? {
            note_gated(store, fired, occurrence);
            return Ok(Dispatch::Skipped);
        }
        Some(day)
    } else {
        None
    };
    let outcome = match occurrence {
        Some((due, attempt)) => executor.execute_attempt(fired, due, attempt).await,
        None => executor.execute(fired).await,
    };
    if let (Err(SchedulerError::NotStarted(_)), Some(day)) = (&outcome, &slot)
        && let Err(error) = store.release_standing_order_run(fired.id, day)
    {
        tracing::error!(schedule_id = %fired.id, %error, "failed to give back a standing order slot");
    }
    outcome.map(|()| Dispatch::Ran)
}

/// Every ingress shares this no-run-before-precheck boundary. A stale definition
/// cannot finish its check and start work after an owner edit or deletion.
pub(crate) async fn execute_checked(
    store: &ScheduleStore,
    executor: &dyn ScheduleExecutor,
    schedule: &Schedule,
    occurrence: Option<(i64, u32)>,
    now: i64,
    bypass_standing_hours: bool,
) -> Result<Dispatch> {
    // A schedule that moved to a server never fires here, whichever ingress asked.
    if store.frozen_for(schedule.id)?.is_some() {
        return Ok(Dispatch::Skipped);
    }
    // Before the precheck (which advances its change hash) and the registry (which empties its
    // queue): a gated firing must lose nothing.
    if !standing_order_open(store, schedule, now, bypass_standing_hours)? {
        note_gated(store, schedule, occurrence);
        return Ok(Dispatch::Skipped);
    }
    let mut fired = schedule.clone();
    if fired.precheck.is_some() {
        let mut state = store.check_state(schedule)?;
        let outcome = tokio::time::timeout(
            crate::precheck::TIMEOUT,
            executor.precheck(schedule, state.last_hash.as_deref()),
        )
        .await;
        if let Ok(Ok(outcome)) = outcome {
            state.precheck_errors = 0;
            if let Some(hash) = outcome.hash {
                state.last_hash = Some(hash);
            }
            if !outcome.run {
                state.skipped_by_precheck = state.skipped_by_precheck.saturating_add(1);
            }
            if !store.save_check_state(schedule, &state)? {
                return Ok(Dispatch::Skipped);
            }
            if !outcome.run {
                return Ok(Dispatch::Skipped);
            }
            if let Some(input) = outcome.input {
                fired.external_input = Some(input);
            }
        } else {
            state.precheck_errors = state.precheck_errors.saturating_add(1);
            if !store.save_check_state(schedule, &state)? {
                return Ok(Dispatch::Skipped);
            }
            fired.precheck_failed = true;
        }
    }
    let Some(current) = store.get(schedule.id)? else {
        return Ok(Dispatch::Skipped);
    };
    if current.revision != schedule.revision {
        return Ok(Dispatch::Skipped);
    }
    if store.registry(schedule.id)?.is_some() {
        let event = fired.external_input.as_ref().map_or_else(
            || serde_json::json!({"received_at": now, "kind":"schedule"}),
            |s| serde_json::from_str(s).unwrap_or_else(|_| serde_json::Value::String(s.clone())),
        );
        let Some(input) = store.registry_dispatch(schedule, Some(event), now)? else {
            return Ok(Dispatch::Skipped);
        };
        fired.external_input = Some(input);
    }
    // The tenth failed check still fails open; the next firing is paused.
    let outcome = launch(store, executor, &fired, occurrence, now).await;
    if outcome.is_err() {
        store.registry_failed(schedule)?;
    }
    outcome
}

fn poll_triggers(inner: &Arc<Inner>, now: i64) -> Result<()> {
    for schedule in inner.store.list()? {
        if !schedule.enabled || inner.in_flight.lock().contains(&schedule.id) {
            continue;
        }
        if let Some((_, state)) = inner.store.registry(schedule.id)?
            && !state.pending.is_empty()
        {
            // Queued events wait for the hours and the cap; nothing is taken from the queue.
            if !gate_open(inner, &schedule, now) {
                continue;
            }
            let Some(guard) = try_acquire_in_flight(inner, &schedule) else {
                continue;
            };
            if let Some(input) = inner.store.registry_dispatch(&schedule, None, now)? {
                let mut fired = schedule.clone();
                fired.external_input = Some(input);
                let inner = Arc::clone(inner);
                tokio::spawn(async move {
                    let _guard = guard;
                    if inner
                        .store
                        .get(fired.id)
                        .ok()
                        .flatten()
                        .is_some_and(|s| s.revision == fired.revision)
                        && launch(&inner.store, inner.executor.as_ref(), &fired, None, now)
                            .await
                            .is_err()
                    {
                        let _ = inner.store.registry_failed(&fired);
                    }
                });
                continue;
            }
            drop(guard);
        }
        let Some(trigger) = inner.store.get_trigger(schedule.id)? else {
            continue;
        };
        if matches!(trigger, crate::Trigger::Webhook { .. }) {
            continue;
        }
        // Not polled while gated: what happens meanwhile is still there to see afterwards.
        if !gate_open(inner, &schedule, now) {
            continue;
        }
        let state = inner.store.check_state(&schedule)?;
        let mut watch = state.watch.clone().unwrap_or_default();
        // Do not occupy a poll slot or write SQLite on idle one-second wakes.
        if now < watch.next_poll_ms && watch.take_ready(&trigger, now).is_none() {
            continue;
        }
        let Ok(slot) = Arc::clone(&inner.watch_slots).try_acquire_owned() else {
            continue;
        };
        let Some(guard) = try_acquire_in_flight(inner, &schedule) else {
            continue;
        };
        let inner = Arc::clone(inner);
        tokio::spawn(async move {
            let _guard = guard;
            let result = poll_one(&inner, &schedule, &trigger, state, now).await;
            drop(slot);
            let result = match result {
                Ok(Some(fired)) => execute_checked(
                    &inner.store,
                    inner.executor.as_ref(),
                    &fired,
                    None,
                    now,
                    false,
                )
                .await
                .map(|_| ()),
                Ok(None) => Ok(()),
                Err(error) => Err(error),
            };
            if result.is_err() {
                tracing::error!(schedule_id=%schedule.id,"schedule source polling failed");
            }
        });
    }
    Ok(())
}

async fn poll_one(
    inner: &Arc<Inner>,
    schedule: &Schedule,
    trigger: &crate::Trigger,
    mut state: crate::precheck::CheckState,
    now: i64,
) -> Result<Option<Schedule>> {
    let mut watch = state.watch.take().unwrap_or_default();
    if now >= watch.next_poll_ms {
        let observed = tokio::select! {
            ()=inner.cancel.cancelled()=>return Ok(None),
            result=tokio::time::timeout(crate::precheck::TIMEOUT,inner.executor.poll_trigger(schedule,trigger))=>match result {
                Ok(Ok(entries))=>watch.observe(trigger,now,entries).is_ok(), _=>false,
            }
        };
        if observed {
            state.trigger_errors = 0;
        } else {
            state.trigger_errors = state.trigger_errors.saturating_add(1);
            watch.next_poll_ms = now.saturating_add(crate::watch::default_poll_ms(trigger));
        }
        // An owner rule sets its own cadence, also after a failed check: a source that is
        // down must not be asked again every few seconds.
        if let Some((rule, _)) = inner.store.registry(schedule.id)?
            && let Some(poll_seconds) = rule.source.poll_seconds()
        {
            watch.next_poll_ms = now.saturating_add(
                i64::try_from(poll_seconds)
                    .unwrap_or(86400)
                    .saturating_mul(1000),
            );
        }
    }
    let ready = if state.trigger_errors < 10 {
        watch.take_ready(trigger, now)
    } else {
        None
    };
    state.watch = Some(watch);
    if !inner.store.save_check_state(schedule, &state)? {
        return Ok(None);
    }
    if let Some(input) = ready {
        let mut fired = schedule.clone();
        fired.external_input = Some(input);
        if inner.store.registry(fired.id)?.is_none() {
            inner.store.touch_last_run(fired.id, now)?;
        }
        return Ok(Some(fired));
    }
    Ok(None)
}
