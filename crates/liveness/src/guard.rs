//! The loop guard: restart with backoff, give up to `degraded`, catch one bad pass.

use std::collections::BTreeMap;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::FutureExt as _;
use parking_lot::Mutex;
use tokio::task::{AbortHandle, JoinHandle};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// When a loop is restarted and when the guard gives up on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// Crashes in a row after which the loop stays down and is `degraded`.
    pub max_crashes: u32,
    /// Wait before the first restart; it doubles with every crash in a row.
    pub backoff_base: Duration,
    /// Longest wait before a restart.
    pub backoff_max: Duration,
    /// A crash this long after the previous one starts the count again: the loop was healthy
    /// in between.
    pub stable_after: Duration,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            max_crashes: 5,
            backoff_base: Duration::from_secs(1),
            backoff_max: Duration::from_secs(30),
            stable_after: Duration::from_secs(300),
        }
    }
}

/// Whether a loop is being kept alive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopState {
    /// Running (possibly after restarts).
    Running,
    /// Crashed too often: stopped for good until the daemon restarts.
    Degraded,
}

/// What the guard knows about one loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopStatus {
    /// The loop's name.
    pub name: &'static str,
    /// Running or given up on.
    pub state: LoopState,
    /// Crashes since the daemon started.
    pub restarts: u32,
    /// Crashes in the current streak (the count the limit applies to).
    pub crashes_in_row: u32,
    /// When it last crashed, Unix ms.
    pub last_crash_ms: Option<i64>,
}

/// What a [`LoopEvent`] reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    /// The loop crashed and is being restarted (reported once per streak).
    Restarted,
    /// The loop crashed too often and was given up on.
    GaveUp,
}

/// Something the owner should hear about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoopEvent {
    /// The loop's name.
    pub name: &'static str,
    /// What happened.
    pub kind: EventKind,
    /// Crashes in the streak.
    pub crashes: u32,
}

/// Receives the [`LoopEvent`]s. It runs on the crashing task: keep it quick.
pub type Notifier = Arc<dyn Fn(&LoopEvent) + Send + Sync>;

/// Why a supervised loop stopped. Fixed values on purpose: a panic message may carry a path or a
/// secret, so it is not kept, logged or shown anywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    /// The loop (or one pass of it) panicked.
    Panicked,
    /// The loop returned although nobody asked it to stop.
    Ended,
}

/// What to do after a crash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Carry on after this wait.
    Retry(Duration),
    /// Stop: the loop is degraded.
    GiveUp,
}

/// Where a test makes a loop panic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// Inside one guarded pass ([`Guard::guarded`]).
    Pass,
    /// At the start of the supervised task, so the guard has to restart the whole loop.
    Task,
}

struct Entry {
    state: LoopState,
    restarts: u32,
    in_row: u32,
    last_crash: Option<Instant>,
    last_crash_ms: Option<i64>,
}

struct Shared {
    policy: Policy,
    loops: Mutex<BTreeMap<&'static str, Entry>>,
    notifier: Mutex<Option<Notifier>>,
    faults: Mutex<Vec<(&'static str, Fault, u32)>>,
    has_faults: AtomicBool,
}

/// Keeps the background loops alive. Cloning shares the state.
#[derive(Clone)]
pub struct Guard(Arc<Shared>);

impl Default for Guard {
    fn default() -> Self {
        Self::new(Policy::default())
    }
}

impl std::fmt::Debug for Guard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Guard")
            .field("loops", &self.statuses())
            .finish_non_exhaustive()
    }
}

fn wall_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// Aborts the task when dropped, so a supervisor that is itself aborted leaves no loop behind.
struct AbortOnDrop(AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl Guard {
    /// A guard with `policy`.
    #[must_use]
    pub fn new(policy: Policy) -> Self {
        Self(Arc::new(Shared {
            policy,
            loops: Mutex::new(BTreeMap::new()),
            notifier: Mutex::new(None),
            faults: Mutex::new(Vec::new()),
            has_faults: AtomicBool::new(false),
        }))
    }

    /// Where the [`LoopEvent`]s go (replaces the previous receiver).
    pub fn set_notifier(&self, notifier: Notifier) {
        *self.0.notifier.lock() = Some(notifier);
    }

    /// Every loop the guard has seen, by name.
    #[must_use]
    pub fn statuses(&self) -> Vec<LoopStatus> {
        self.0
            .loops
            .lock()
            .iter()
            .map(|(name, e)| LoopStatus {
                name,
                state: e.state,
                restarts: e.restarts,
                crashes_in_row: e.in_row,
                last_crash_ms: e.last_crash_ms,
            })
            .collect()
    }

    /// Whether any loop was given up on.
    #[must_use]
    pub fn is_degraded(&self) -> bool {
        self.0
            .loops
            .lock()
            .values()
            .any(|e| e.state == LoopState::Degraded)
    }

    /// Whether this loop has crashed too often and is stopped until the daemon restarts.
    #[must_use]
    pub fn is_given_up(&self, name: &'static str) -> bool {
        self.0
            .loops
            .lock()
            .get(name)
            .is_some_and(|e| e.state == LoopState::Degraded)
    }

    /// List `name` (as running) before it has crashed, so health shows it.
    pub fn register(&self, name: &'static str) {
        self.0.loops.lock().entry(name).or_insert(Entry {
            state: LoopState::Running,
            restarts: 0,
            in_row: 0,
            last_crash: None,
            last_crash_ms: None,
        });
    }

    /// Note a crash of `name`: log it (an allowlisted support-log event: the loop's name and
    /// counts, never the panic message), tell the notifier and say what to do next.
    pub fn record_crash(&self, name: &'static str, cause: Cause) -> Verdict {
        self.register(name);
        let (verdict, event) = {
            let mut loops = self.0.loops.lock();
            let policy = self.0.policy;
            let Some(e) = loops.get_mut(name) else {
                return Verdict::GiveUp;
            };
            let now = Instant::now();
            if e.last_crash
                .is_some_and(|last| now.duration_since(last) >= policy.stable_after)
            {
                e.in_row = 0;
            }
            e.in_row += 1;
            e.restarts = e.restarts.saturating_add(1);
            e.last_crash = Some(now);
            e.last_crash_ms = Some(wall_ms());
            if e.in_row >= policy.max_crashes {
                e.state = LoopState::Degraded;
                support_logs::tracing::error!(
                    target: "praxi_support",
                    reason = "loop_gave_up",
                    loop_name = name,
                    crashes = u64::from(e.in_row),
                    "background loop given up on"
                );
                (
                    Verdict::GiveUp,
                    Some(LoopEvent {
                        name,
                        kind: EventKind::GaveUp,
                        crashes: e.in_row,
                    }),
                )
            } else {
                let shift = (e.in_row - 1).min(16);
                let wait = policy
                    .backoff_base
                    .saturating_mul(1u32 << shift)
                    .min(policy.backoff_max);
                support_logs::tracing::error!(
                    target: "praxi_support",
                    reason = match cause {
                        Cause::Panicked => "loop_panicked",
                        Cause::Ended => "loop_ended",
                    },
                    loop_name = name,
                    crashes = u64::from(e.in_row),
                    restart_in_ms = u64::try_from(wait.as_millis()).unwrap_or(u64::MAX),
                    "background loop crashed, restarting"
                );
                (
                    Verdict::Retry(wait),
                    (e.in_row == 1).then_some(LoopEvent {
                        name,
                        kind: EventKind::Restarted,
                        crashes: e.in_row,
                    }),
                )
            }
        };
        if let Some(event) = event {
            let notifier = self.0.notifier.lock().clone();
            if let Some(notify) = notifier {
                notify(&event);
            }
        }
        verdict
    }

    /// Run one pass of a loop: a panic inside it is caught, counted as a crash of `name` and
    /// turned into the [`Verdict`] for the loop to follow (wait and go on, or stop).
    ///
    /// Only use it where the pass keeps no half-finished state that the next pass would trip
    /// over: whatever the pass held (locks, guards) is released while it unwinds.
    ///
    /// # Errors
    /// The pass panicked.
    pub async fn guarded<T>(
        &self,
        name: &'static str,
        pass: impl Future<Output = T>,
    ) -> Result<T, Verdict> {
        let run = async {
            self.fault(name, Fault::Pass);
            pass.await
        };
        match AssertUnwindSafe(run).catch_unwind().await {
            Ok(value) => Ok(value),
            Err(_payload) => Err(self.record_crash(name, Cause::Panicked)),
        }
    }

    /// Keep the loop that `make` builds running until `cancel` fires: every panic (or return
    /// before `cancel`) restarts it after a backoff, and too many in a row give up on it.
    /// `make` is called again for each restart, so the loop starts over from its own state.
    pub fn supervise<F, Fut>(
        &self,
        name: &'static str,
        cancel: CancellationToken,
        mut make: F,
    ) -> JoinHandle<()>
    where
        F: FnMut() -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.register(name);
        let guard = self.clone();
        tokio::spawn(async move {
            loop {
                let body = make();
                let g = guard.clone();
                let task = tokio::spawn(async move {
                    g.fault(name, Fault::Task);
                    body.await;
                });
                let _abort = AbortOnDrop(task.abort_handle());
                let ended = task.await;
                if cancel.is_cancelled() || guard.is_given_up(name) {
                    return;
                }
                let verdict = match ended {
                    Ok(()) if guard.is_given_up(name) => return,
                    Ok(()) => guard.record_crash(name, Cause::Ended),
                    Err(e) if e.is_panic() => guard.record_crash(name, Cause::Panicked),
                    // Aborted from outside: nobody wants it back.
                    Err(_) => return,
                };
                match verdict {
                    Verdict::GiveUp => return,
                    Verdict::Retry(wait) => {
                        tokio::select! {
                            () = cancel.cancelled() => return,
                            () = tokio::time::sleep(wait) => {}
                        }
                    }
                }
            }
        })
    }

    /// Make `name` panic `times` times at `at`. For tests of the recovery path.
    #[doc(hidden)]
    pub fn inject_panic(&self, name: &'static str, at: Fault, times: u32) {
        self.0.faults.lock().push((name, at, times));
        self.0.has_faults.store(true, Ordering::SeqCst);
    }

    fn fault(&self, name: &'static str, at: Fault) {
        if !self.0.has_faults.load(Ordering::SeqCst) {
            return;
        }
        let hit = {
            let mut faults = self.0.faults.lock();
            let hit = faults
                .iter_mut()
                .find(|(n, a, left)| *n == name && *a == at && *left > 0)
                .map(|f| f.2 -= 1)
                .is_some();
            faults.retain(|f| f.2 > 0);
            if faults.is_empty() {
                self.0.has_faults.store(false, Ordering::SeqCst);
            }
            hit
        };
        assert!(!hit, "injected panic in `{name}` ({at:?})");
    }
}

#[cfg(test)]
mod tests;
