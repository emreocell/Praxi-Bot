//! The bot's own pointer on the real desktop.
//!
//! Computer-use apps draw a pointer of their own so the owner can see where the bot is about
//! to click, separate from the owner's cursor. [`CursorOverlay`] is the lifecycle of that
//! pointer, independent of the OS: it appears when the bot starts acting, glides from point to
//! point, ripples on a click, hides after a quiet spell and at once when the bot stops or the
//! owner takes control, and does nothing at all while the owner's switch
//! ([`live_view::bot_cursor::enabled`]) is off. The pixels are drawn by a platform
//! [`CursorSurface`] (a click-through, always-on-top, never-focused window), which is only a
//! thin shell over the pure parts in [`geometry`], [`path`] and [`sprite`].
//!
//! Every position is also published to [`live_view::bot_cursor`] so the live viewer can draw the
//! same pointer, whether or not a native surface exists. The real OS cursor is never touched
//! here.

pub mod geometry;
pub mod path;
pub mod sprite;
pub mod threaded;

use std::sync::Arc;
use std::time::Duration;

use live_view::TargetKind;
use live_view::bot_cursor;
use parking_lot::Mutex;

pub use threaded::{OverlayWindow, ThreadedSurface};

/// Draws the bot pointer. Coordinates are physical pixels in virtual-screen space. Calls must
/// be cheap and non-blocking: the window is updated on the surface's own thread.
pub trait CursorSurface: Send + Sync {
    /// Show the pointer (if hidden) at `(x, y)`.
    ///
    /// # Errors
    /// A short reason; the controller keeps going.
    fn move_to(&self, x: f64, y: f64) -> Result<(), String>;

    /// Play a click ripple around `(x, y)`.
    ///
    /// # Errors
    /// A short reason.
    fn ripple(&self, x: f64, y: f64) -> Result<(), String>;

    /// Hide the pointer.
    ///
    /// # Errors
    /// A short reason.
    fn hide(&self) -> Result<(), String>;
}

/// A surface that draws nothing: for hosts where another layer (the desktop app's own overlay)
/// draws the pointer, so only the published positions remain.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullSurface;

impl CursorSurface for NullSurface {
    fn move_to(&self, _: f64, _: f64) -> Result<(), String> {
        Ok(())
    }
    fn ripple(&self, _: f64, _: f64) -> Result<(), String> {
        Ok(())
    }
    fn hide(&self) -> Result<(), String> {
        Ok(())
    }
}

/// Milliseconds since some fixed point; injectable so idle hiding is testable.
pub trait Clock: Send + Sync {
    /// Now, in milliseconds.
    fn now_ms(&self) -> u64;
}

/// The real clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        use std::sync::OnceLock;
        use std::time::Instant;
        static START: OnceLock<Instant> = OnceLock::new();
        u64::try_from(START.get_or_init(Instant::now).elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

/// Timing of the overlay.
#[derive(Debug, Clone, Copy)]
pub struct CursorConfig {
    /// Quiet time after the last action before the pointer hides itself.
    pub idle: Duration,
    /// Pause between the steps of a glide; zero in tests.
    pub step: Duration,
    /// Most steps in one glide.
    pub max_steps: usize,
}

impl Default for CursorConfig {
    fn default() -> Self {
        Self {
            idle: Duration::from_secs(4),
            step: Duration::from_millis(14),
            max_steps: 16,
        }
    }
}

/// What the bot does at the point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gesture {
    /// Only move there (hover, scroll, type target, the start of a drag).
    Move,
    /// Move there and click: a ripple.
    Click,
}

#[derive(Default)]
struct Inner {
    pos: Option<(f64, f64)>,
    visible: bool,
    last_ms: u64,
    /// Bumped by every release, so a glide that was running when the owner took over stops.
    epoch: u64,
}

/// The lifecycle of the bot pointer over one [`CursorSurface`].
pub struct CursorOverlay {
    surface: Arc<dyn CursorSurface>,
    clock: Arc<dyn Clock>,
    cfg: CursorConfig,
    inner: Mutex<Inner>,
    errors: Mutex<Option<String>>,
}

impl CursorOverlay {
    /// An overlay on `surface` with the real clock and default timing.
    #[must_use]
    pub fn new(surface: Arc<dyn CursorSurface>) -> Arc<Self> {
        Self::with(surface, Arc::new(SystemClock), CursorConfig::default())
    }

    /// An overlay with an explicit clock and timing.
    #[must_use]
    pub fn with(
        surface: Arc<dyn CursorSurface>,
        clock: Arc<dyn Clock>,
        cfg: CursorConfig,
    ) -> Arc<Self> {
        Arc::new(Self {
            surface,
            clock,
            cfg,
            inner: Mutex::new(Inner::default()),
            errors: Mutex::new(None),
        })
    }

    /// Whether the pointer is showing right now.
    #[must_use]
    pub fn is_visible(&self) -> bool {
        self.inner.lock().visible
    }

    /// Where the pointer is, if it is showing.
    #[must_use]
    pub fn position(&self) -> Option<(f64, f64)> {
        let i = self.inner.lock();
        i.visible.then_some(i.pos).flatten()
    }

    /// The last error a surface call returned (cleared by the next call that works).
    #[must_use]
    pub fn last_error(&self) -> Option<String> {
        self.errors.lock().clone()
    }

    fn note(&self, r: Result<(), String>) {
        *self.errors.lock() = r.err();
    }

    /// The bot acts at `to`: appear (first action after idle), glide there from the previous
    /// point, and ripple for a click. Returns once the pointer has arrived. A non-finite or
    /// negative-infinite point, or the owner's switch being off, draws nothing.
    pub async fn point(self: &Arc<Self>, to: (f64, f64), gesture: Gesture) {
        if !bot_cursor::enabled() {
            self.release();
            return;
        }
        if !to.0.is_finite() || !to.1.is_finite() {
            return;
        }
        let (from, epoch) = {
            let i = self.inner.lock();
            (
                i.visible
                    .then_some(i.pos)
                    .flatten()
                    .unwrap_or_else(|| path::entry_point(to)),
                i.epoch,
            )
        };
        let steps = path::glide_path(from, to, self.cfg.max_steps);
        let n = steps.len();
        for (k, (x, y)) in steps.into_iter().enumerate() {
            {
                let mut i = self.inner.lock();
                if i.epoch != epoch {
                    return; // released mid-glide: the owner took over
                }
                i.pos = Some((x, y));
                i.visible = true;
                i.last_ms = self.clock.now_ms();
            }
            self.note(self.surface.move_to(x, y));
            bot_cursor::publish_move(TargetKind::Desktop, x, y);
            if k + 1 < n && !self.cfg.step.is_zero() {
                tokio::time::sleep(self.cfg.step).await;
            }
        }
        if gesture == Gesture::Click {
            if self.inner.lock().epoch != epoch {
                return;
            }
            self.note(self.surface.ripple(to.0, to.1));
            bot_cursor::publish_click(TargetKind::Desktop, to.0, to.1);
        }
        self.schedule_idle_hide();
    }

    /// Hide after the idle time unless something moves the pointer first.
    fn schedule_idle_hide(self: &Arc<Self>) {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let me = Arc::clone(self);
        let idle = self.cfg.idle;
        handle.spawn(async move {
            tokio::time::sleep(idle + Duration::from_millis(20)).await;
            me.hide_if_idle();
        });
    }

    /// Hide the pointer if nothing has moved it for the idle time. Returns whether it hid.
    pub fn hide_if_idle(&self) -> bool {
        let idle_ms = u64::try_from(self.cfg.idle.as_millis()).unwrap_or(u64::MAX);
        {
            let i = self.inner.lock();
            if !i.visible || self.clock.now_ms().saturating_sub(i.last_ms) < idle_ms {
                return false;
            }
        }
        self.release();
        true
    }

    /// The bot stopped, the run ended, a hand-over began or the owner took control: hide the
    /// pointer now and forget where it was. Safe to call at any time, any number of times.
    pub fn release(&self) {
        let was_shown = {
            let mut i = self.inner.lock();
            let was = i.visible || i.pos.is_some();
            i.visible = false;
            i.pos = None;
            i.epoch = i.epoch.wrapping_add(1);
            was
        };
        if was_shown {
            self.note(self.surface.hide());
        }
        bot_cursor::clear(TargetKind::Desktop);
    }
}

impl Drop for CursorOverlay {
    fn drop(&mut self) {
        self.release();
    }
}

/// The tests of the crate share the process-wide switch and published position.
#[cfg(test)]
pub(crate) static TEST_GLOBAL: Mutex<()> = Mutex::new(());

#[cfg(test)]
#[allow(clippy::await_holding_lock)] // the guard serialises tests that share the global owner switch
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[derive(Debug, Clone, PartialEq)]
    enum Ev {
        Move(i64, i64),
        Ripple(i64, i64),
        Hide,
    }

    #[derive(Default)]
    struct Fake {
        log: Mutex<Vec<Ev>>,
        fail: bool,
    }
    #[allow(clippy::cast_possible_truncation)]
    impl CursorSurface for Fake {
        fn move_to(&self, x: f64, y: f64) -> Result<(), String> {
            self.log
                .lock()
                .push(Ev::Move(x.round() as i64, y.round() as i64));
            if self.fail {
                Err("surface gone".into())
            } else {
                Ok(())
            }
        }
        fn ripple(&self, x: f64, y: f64) -> Result<(), String> {
            self.log
                .lock()
                .push(Ev::Ripple(x.round() as i64, y.round() as i64));
            if self.fail {
                Err("surface gone".into())
            } else {
                Ok(())
            }
        }
        fn hide(&self) -> Result<(), String> {
            self.log.lock().push(Ev::Hide);
            if self.fail {
                Err("surface gone".into())
            } else {
                Ok(())
            }
        }
    }

    #[derive(Default)]
    struct FakeClock(AtomicU64);
    impl Clock for FakeClock {
        fn now_ms(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    fn rig(fail: bool) -> (Arc<CursorOverlay>, Arc<Fake>, Arc<FakeClock>) {
        let s = Arc::new(Fake {
            fail,
            ..Fake::default()
        });
        let c = Arc::new(FakeClock::default());
        let o = CursorOverlay::with(
            s.clone(),
            c.clone(),
            CursorConfig {
                idle: Duration::from_secs(3),
                step: Duration::ZERO,
                max_steps: 8,
            },
        );
        (o, s, c)
    }

    use super::TEST_GLOBAL as GLOBAL;

    #[tokio::test]
    async fn the_first_action_shows_the_pointer_gliding_in_and_a_click_ripples() {
        let _g = GLOBAL.lock();
        bot_cursor::set_enabled(true);
        let (o, s, _) = rig(false);
        assert!(!o.is_visible());
        o.point((400.0, 300.0), Gesture::Click).await;
        let log = s.log.lock().clone();
        assert!(log.len() >= 3, "{log:?}");
        assert!(
            log[..log.len() - 1]
                .iter()
                .all(|e| matches!(e, Ev::Move(..)))
        );
        assert_eq!(
            log[log.len() - 2],
            Ev::Move(400, 300),
            "arrives exactly on the point"
        );
        assert_eq!(log[log.len() - 1], Ev::Ripple(400, 300));
        assert!(o.is_visible());
        assert_eq!(o.position(), Some((400.0, 300.0)));
        let p = bot_cursor::latest(TargetKind::Desktop).unwrap();
        assert_eq!((p.x, p.y, p.click_seq), (400.0, 300.0, 1));
    }

    #[tokio::test]
    async fn the_next_action_glides_from_the_previous_point_not_from_the_corner() {
        let _g = GLOBAL.lock();
        bot_cursor::set_enabled(true);
        let (o, s, _) = rig(false);
        o.point((100.0, 100.0), Gesture::Move).await;
        s.log.lock().clear();
        o.point((130.0, 100.0), Gesture::Move).await;
        let log = s.log.lock().clone();
        let xs: Vec<i64> = log
            .iter()
            .filter_map(|e| {
                if let Ev::Move(x, _) = e {
                    Some(*x)
                } else {
                    None
                }
            })
            .collect();
        assert!(xs.iter().all(|x| (100..=130).contains(x)), "{xs:?}");
        assert_eq!(xs.last(), Some(&130));
        assert!(
            !log.iter().any(|e| matches!(e, Ev::Ripple(..))),
            "a move does not ripple"
        );
    }

    #[tokio::test]
    async fn the_pointer_hides_after_the_idle_time_but_not_before() {
        let _g = GLOBAL.lock();
        bot_cursor::set_enabled(true);
        let (o, s, clock) = rig(false);
        o.point((50.0, 50.0), Gesture::Move).await;
        clock.0.store(2_900, Ordering::SeqCst);
        assert!(!o.hide_if_idle());
        assert!(o.is_visible());
        clock.0.store(3_100, Ordering::SeqCst);
        assert!(o.hide_if_idle());
        assert!(!o.is_visible());
        assert_eq!(s.log.lock().last(), Some(&Ev::Hide));
        assert!(bot_cursor::latest(TargetKind::Desktop).is_none());
        assert!(!o.hide_if_idle(), "already hidden");
    }

    #[tokio::test]
    async fn release_hides_at_once_forgets_the_point_and_is_idempotent() {
        let _g = GLOBAL.lock();
        bot_cursor::set_enabled(true);
        let (o, s, _) = rig(false);
        o.release();
        assert!(s.log.lock().is_empty(), "nothing shown, nothing to hide");
        o.point((10.0, 10.0), Gesture::Click).await;
        o.release();
        o.release();
        let hides = s.log.lock().iter().filter(|e| **e == Ev::Hide).count();
        assert_eq!(hides, 1);
        assert!(o.position().is_none());
        // The next action starts from the entry point again (it reappears gliding in).
        s.log.lock().clear();
        o.point((500.0, 500.0), Gesture::Move).await;
        assert!(matches!(s.log.lock()[0], Ev::Move(x, _) if x < 500));
    }

    #[tokio::test]
    async fn an_owner_takeover_during_a_glide_stops_it() {
        let _g = GLOBAL.lock();
        bot_cursor::set_enabled(true);
        let s = Arc::new(Fake::default());
        let o = CursorOverlay::with(
            s.clone(),
            Arc::new(FakeClock::default()),
            CursorConfig {
                idle: Duration::from_secs(3),
                step: Duration::from_millis(20),
                max_steps: 16,
            },
        );
        let o2 = Arc::clone(&o);
        let task = tokio::spawn(async move { o2.point((900.0, 500.0), Gesture::Click).await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        o.release();
        task.await.unwrap();
        let log = s.log.lock().clone();
        assert!(!log.iter().any(|e| matches!(e, Ev::Ripple(..))), "{log:?}");
        assert_eq!(
            log.last(),
            Some(&Ev::Hide),
            "nothing drawn after the release: {log:?}"
        );
        assert!(!o.is_visible());
    }

    #[tokio::test]
    async fn a_switched_off_pointer_draws_and_publishes_nothing() {
        let _g = GLOBAL.lock();
        bot_cursor::set_enabled(false);
        let (o, s, _) = rig(false);
        o.point((10.0, 10.0), Gesture::Click).await;
        assert!(s.log.lock().is_empty());
        assert!(bot_cursor::latest(TargetKind::Desktop).is_none());
        assert!(!o.is_visible());
        bot_cursor::set_enabled(true);
    }

    #[tokio::test]
    async fn surface_errors_never_panic_and_are_reported() {
        let _g = GLOBAL.lock();
        bot_cursor::set_enabled(true);
        let (o, _, _) = rig(true);
        o.point((20.0, 20.0), Gesture::Click).await;
        assert_eq!(o.last_error().as_deref(), Some("surface gone"));
        o.release();
        o.point((f64::NAN, 1.0), Gesture::Click).await;
        assert!(o.position().is_none());
    }

    #[tokio::test]
    async fn the_null_surface_still_publishes_positions_for_the_live_viewer() {
        let _g = GLOBAL.lock();
        bot_cursor::set_enabled(true);
        let o = CursorOverlay::with(
            Arc::new(NullSurface),
            Arc::new(FakeClock::default()),
            CursorConfig {
                step: Duration::ZERO,
                ..CursorConfig::default()
            },
        );
        o.point((77.0, 88.0), Gesture::Move).await;
        let p = bot_cursor::latest(TargetKind::Desktop).unwrap();
        assert_eq!((p.x, p.y), (77.0, 88.0));
        o.release();
    }
}
