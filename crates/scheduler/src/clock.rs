//! Wall-clock and monotonic clock abstraction, plus [`FakeClock`] for
//! deterministic testing across DST transitions, sleep/resume wall-clock jumps,
//! and misfire windows.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio::sync::watch;

/// Provides both wall-clock time (Unix epoch milliseconds) and monotonic time
/// (milliseconds since an arbitrary epoch) so the scheduler can detect
/// wall-clock jumps (PC sleep/resume, manual clock adjustments) by comparing
/// `delta_wall` against `delta_mono`.
#[async_trait]
pub trait Clock: Send + Sync + 'static {
    /// Current wall-clock Unix timestamp in milliseconds.
    fn wall_ms(&self) -> i64;

    /// Current monotonic clock reading in milliseconds.
    fn mono_ms(&self) -> i64;

    /// Sleep for up to `duration` (or wake early on a synthetic clock change in
    /// [`FakeClock`]).
    async fn sleep(&self, duration: Duration);
}

/// Production clock backed by [`std::time::SystemTime`], [`Instant`], and
/// [`tokio::time::sleep`].
#[derive(Debug, Clone)]
pub struct SystemClock {
    origin: Instant,
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemClock {
    /// Construct a new [`SystemClock`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

#[async_trait]
impl Clock for SystemClock {
    fn wall_ms(&self) -> i64 {
        storage::now_ms()
    }

    fn mono_ms(&self) -> i64 {
        i64::try_from(self.origin.elapsed().as_millis()).unwrap_or(i64::MAX)
    }

    async fn sleep(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}

/// Deterministic clock for testing cron schedules, DST transitions, misfire
/// policies, and wall-clock jumps.
#[derive(Debug)]
pub struct FakeClock {
    state: Mutex<(i64, i64)>,
    tx: watch::Sender<(i64, i64)>,
}

impl FakeClock {
    /// Create a [`FakeClock`] starting at `initial_wall_ms` with `mono_ms = 0`.
    #[must_use]
    pub fn new(initial_wall_ms: i64) -> Self {
        let init = (initial_wall_ms, 0_i64);
        let (tx, _rx) = watch::channel(init);
        Self {
            state: Mutex::new(init),
            tx,
        }
    }

    /// Advance both wall-clock time and monotonic time by `delta_ms` (normal
    /// passage of time).
    pub fn advance_ms(&self, delta_ms: i64) {
        let next = {
            let mut g = self.state.lock();
            g.0 = g.0.saturating_add(delta_ms);
            g.1 = g.1.saturating_add(delta_ms);
            *g
        };
        self.tx.send_replace(next);
    }

    /// Advance both wall-clock time and monotonic time by `duration`.
    pub fn advance(&self, duration: Duration) {
        let delta_ms = i64::try_from(duration.as_millis()).unwrap_or(i64::MAX);
        self.advance_ms(delta_ms);
    }

    /// Shift the wall clock by `delta_ms` **without** advancing the monotonic
    /// clock (simulates OS sleep/hibernation where the monotonic timer pauses,
    /// or a manual system clock jump).
    pub fn jump_wall_ms(&self, delta_ms: i64) {
        let next = {
            let mut g = self.state.lock();
            g.0 = g.0.saturating_add(delta_ms);
            *g
        };
        self.tx.send_replace(next);
    }

    /// Set the wall clock to `new_wall_ms` **without** advancing the monotonic
    /// clock.
    pub fn set_wall_ms(&self, new_wall_ms: i64) {
        let next = {
            let mut g = self.state.lock();
            g.0 = new_wall_ms;
            *g
        };
        self.tx.send_replace(next);
    }
}

#[async_trait]
impl Clock for FakeClock {
    fn wall_ms(&self) -> i64 {
        self.state.lock().0
    }

    fn mono_ms(&self) -> i64 {
        self.state.lock().1
    }

    async fn sleep(&self, duration: Duration) {
        let wait_ms = i64::try_from(duration.as_millis()).unwrap_or(i64::MAX);
        if wait_ms <= 0 {
            return;
        }
        let mut rx = self.tx.subscribe();
        let (start_wall, start_mono) = *self.state.lock();
        rx.borrow_and_update();
        let target_mono = start_mono.saturating_add(wait_ms);
        let target_wall = start_wall.saturating_add(wait_ms);

        loop {
            let (cur_wall, cur_mono) = *self.state.lock();
            let wall_delta = cur_wall.saturating_sub(start_wall);
            let mono_delta = cur_mono.saturating_sub(start_mono);
            if cur_mono >= target_mono || cur_wall >= target_wall || wall_delta != mono_delta {
                return;
            }
            if rx.changed().await.is_err() {
                return;
            }
        }
    }
}
