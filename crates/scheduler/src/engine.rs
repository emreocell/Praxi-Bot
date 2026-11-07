//! Background [`Scheduler`] engine: single Tokio task sleeping until the
//! nearest `next_run_at`, waking on store changes, detecting wall-clock jumps
//! via monotonic vs. wall time, applying [`MisfirePolicy`], persisting before
//! execution, and enforcing a per-schedule concurrency limit of 1.

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
use crate::model::{MisfirePolicy, Schedule, ScheduleSpec};
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
const MAX_IDLE_SLEEP_MS: u64 = 60_000;

/// Callback invoked by the [`Scheduler`] when a schedule or webhook fires.
#[async_trait]
pub trait ScheduleExecutor: Send + Sync + 'static {
    /// Execute a single triggered occurrence of `schedule`.
    async fn execute(&self, schedule: &Schedule) -> Result<()>;
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
        });

        let init_wall = inner.clock.wall_ms();
        let init_mono = inner.clock.mono_ms();

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
        self.inner.executor.execute(&fired).await
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
        spawn_executions(&self.inner, fired, 1, guard);
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
    pub fn run_now(&self, id: TaskId) -> Result<()> {
        let mut schedule = self
            .inner
            .store
            .get(id)?
            .ok_or_else(|| SchedulerError::NotFound(format!("schedule {id}")))?;
        let Some(guard) = try_acquire_in_flight(&self.inner, &schedule) else {
            return Err(SchedulerError::Busy(id.to_string()));
        };
        let now_ms = self.inner.clock.wall_ms();
        self.inner.store.touch_last_run(id, now_ms)?;
        schedule.last_run_at = Some(now_ms);
        spawn_executions(&self.inner, schedule, 1, guard);
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
            handle_misfire(inner, schedule, first_due_ms, now_wall_ms)
        } else {
            handle_on_time(inner, schedule, first_due_ms, now_wall_ms)
        };
        if let Err(e) = outcome {
            // One broken schedule (corrupt spec, DB hiccup) must not starve the
            // others nor spin the loop: park it as disabled and keep going.
            tracing::error!(schedule_id = %id, error = %e, "schedule failed; disabling it");
            let _ = inner.store.set_enabled(id, false, None);
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
            inner.store.advance_next_run(schedule.id, Some(next))?;
        }
    }
    Ok(())
}

fn handle_on_time(
    inner: &Arc<Inner>,
    mut schedule: Schedule,
    first_due_ms: i64,
    now_wall_ms: i64,
) -> Result<()> {
    let next_future = schedule.spec.next_after_due(first_due_ms, now_wall_ms)?;
    let Some(guard) = try_acquire_in_flight(inner, &schedule) else {
        inner.store.advance_next_run(schedule.id, next_future)?;
        return Ok(());
    };

    // Persist before executing so a restart never fires the same occurrence twice.
    inner
        .store
        .record_run_intent(schedule.id, now_wall_ms, next_future)?;
    schedule.last_run_at = Some(now_wall_ms);
    schedule.next_run_at = next_future;

    spawn_executions(inner, schedule, 1, guard);
    Ok(())
}

fn handle_misfire(
    inner: &Arc<Inner>,
    mut schedule: Schedule,
    first_due_ms: i64,
    now_wall_ms: i64,
) -> Result<()> {
    match schedule.misfire_policy {
        MisfirePolicy::Skip => {
            let next_future = schedule.spec.next_after_due(first_due_ms, now_wall_ms)?;
            inner.store.advance_next_run(schedule.id, next_future)?;
        }
        MisfirePolicy::RunOnce => {
            let next_future = schedule.spec.next_after_due(first_due_ms, now_wall_ms)?;
            let Some(guard) = try_acquire_in_flight(inner, &schedule) else {
                inner.store.advance_next_run(schedule.id, next_future)?;
                return Ok(());
            };
            inner
                .store
                .record_run_intent(schedule.id, now_wall_ms, next_future)?;
            schedule.last_run_at = Some(now_wall_ms);
            schedule.next_run_at = next_future;
            spawn_executions(inner, schedule, 1, guard);
        }
        MisfirePolicy::RunAll => {
            let (due_slots, next_future) = schedule
                .spec
                .collect_due_occurrences(first_due_ms, now_wall_ms)?;
            let count = due_slots.len();
            if count == 0 {
                inner.store.advance_next_run(schedule.id, next_future)?;
                return Ok(());
            }
            let Some(guard) = try_acquire_in_flight(inner, &schedule) else {
                inner.store.advance_next_run(schedule.id, next_future)?;
                return Ok(());
            };
            inner
                .store
                .record_run_intent(schedule.id, now_wall_ms, next_future)?;
            schedule.last_run_at = Some(now_wall_ms);
            schedule.next_run_at = next_future;
            spawn_executions(inner, schedule, count, guard);
        }
    }
    Ok(())
}

fn try_acquire_in_flight(inner: &Arc<Inner>, schedule: &Schedule) -> Option<InFlightGuard> {
    let mut in_flight = inner.in_flight.lock();
    if in_flight.contains(&schedule.id) {
        tracing::warn!(
            schedule_id = %schedule.id,
            name = %schedule.name,
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

fn spawn_executions(inner: &Arc<Inner>, schedule: Schedule, count: usize, guard: InFlightGuard) {
    let executor = Arc::clone(&inner.executor);
    tokio::spawn(async move {
        let _guard = guard;
        for _ in 0..count {
            if let Err(e) = executor.execute(&schedule).await {
                tracing::error!(
                    schedule_id = %schedule.id,
                    name = %schedule.name,
                    error = %e,
                    "scheduled execution failed"
                );
            }
        }
    });
}
