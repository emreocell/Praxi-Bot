//! A [`CursorSurface`] that owns one worker thread and one [`OverlayWindow`].
//!
//! OS windows belong to the thread that made them (a Win32 window needs its creator to pump
//! messages, an X11 connection is simplest on one thread), so the platform code only supplies
//! the window; this module owns the thread, coalesces a burst of moves into the newest one and
//! times the click ripple. Dropping the surface hides and destroys the window.

use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use parking_lot::Mutex;

use super::CursorSurface;
use super::geometry::window_origin;
use super::sprite::{Bitmap, RIPPLE_FRAME_MS, RIPPLE_FRAMES, canvas_size, compose, render_arrow};

/// The platform window. Created and used on the surface's worker thread only.
pub trait OverlayWindow {
    /// Show the window (if hidden) with its top-left corner at `origin` (virtual-screen physical
    /// pixels) and `bitmap` as its pixels. It must never take focus, never receive input and
    /// stay above other windows.
    ///
    /// # Errors
    /// A short reason; the surface keeps running and tries again on the next call.
    fn place(&mut self, origin: (i32, i32), bitmap: &Bitmap) -> Result<(), String>;

    /// Hide the window.
    ///
    /// # Errors
    /// A short reason.
    fn hide(&mut self) -> Result<(), String>;

    /// Handle the platform's pending window messages (called a few times a second).
    fn pump(&mut self) {}
}

#[derive(Clone, Copy)]
enum Cmd {
    Move(f64, f64),
    Ripple(f64, f64),
    Hide,
}

/// See the module docs.
pub struct ThreadedSurface {
    tx: Mutex<Option<mpsc::Sender<Cmd>>>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    last_error: Arc<Mutex<Option<String>>>,
}

impl ThreadedSurface {
    /// Start the worker. `make` runs on the new thread and builds the window; a failure is
    /// returned here. `scale` is the DPI scale of the cursor image.
    ///
    /// # Errors
    /// The window could not be created, or the thread could not start.
    pub fn spawn<F>(name: &str, scale: f64, make: F) -> Result<Self, String>
    where
        F: FnOnce() -> Result<Box<dyn OverlayWindow>, String> + Send + 'static,
    {
        let (tx, rx) = mpsc::channel::<Cmd>();
        let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<(), String>>(1);
        let last_error: Arc<Mutex<Option<String>>> = Arc::default();
        let errors = Arc::clone(&last_error);
        let thread = std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || match make() {
                Ok(window) => {
                    let _ = ready_tx.send(Ok(()));
                    run(window, &rx, scale, &errors);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            })
            .map_err(|e| format!("could not start the cursor overlay thread: {e}"))?;
        ready_rx
            .recv()
            .map_err(|_| "the cursor overlay thread stopped while starting".to_owned())??;
        Ok(Self {
            tx: Mutex::new(Some(tx)),
            thread: Mutex::new(Some(thread)),
            last_error,
        })
    }

    /// The last error the window reported, if any.
    #[must_use]
    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().clone()
    }

    fn send(&self, cmd: Cmd) -> Result<(), String> {
        match self.tx.lock().as_ref() {
            Some(tx) => tx
                .send(cmd)
                .map_err(|_| "the cursor overlay thread is gone".to_owned()),
            None => Err("the cursor overlay is closed".to_owned()),
        }
    }
}

impl CursorSurface for ThreadedSurface {
    fn move_to(&self, x: f64, y: f64) -> Result<(), String> {
        self.send(Cmd::Move(x, y))
    }

    fn ripple(&self, x: f64, y: f64) -> Result<(), String> {
        self.send(Cmd::Ripple(x, y))
    }

    fn hide(&self) -> Result<(), String> {
        self.send(Cmd::Hide)
    }
}

impl Drop for ThreadedSurface {
    fn drop(&mut self) {
        // Dropping the sender ends the worker, which hides the window on its way out.
        self.tx.lock().take();
        if let Some(t) = self.thread.lock().take() {
            let _ = t.join();
        }
    }
}

struct Worker {
    window: Box<dyn OverlayWindow>,
    arrow: Bitmap,
    scale: f64,
    canvas: u32,
    pos: Option<(f64, f64)>,
    ripple: Option<usize>,
    errors: Arc<Mutex<Option<String>>>,
}

impl Worker {
    fn note(&self, r: Result<(), String>) {
        *self.errors.lock() = r.err();
    }

    fn redraw(&mut self) {
        let Some((x, y)) = self.pos else { return };
        let bmp = compose(&self.arrow, self.scale, self.ripple);
        let origin = window_origin(x, y, self.canvas);
        let r = self.window.place(origin, &bmp);
        self.note(r);
    }

    fn apply(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Move(x, y) => {
                self.pos = Some((x, y));
                self.redraw();
            }
            Cmd::Ripple(x, y) => {
                self.pos = Some((x, y));
                self.ripple = Some(0);
                self.redraw();
            }
            Cmd::Hide => {
                self.pos = None;
                self.ripple = None;
                let r = self.window.hide();
                self.note(r);
            }
        }
    }

    fn tick(&mut self) {
        if let Some(f) = self.ripple {
            self.ripple = (f + 1 < RIPPLE_FRAMES).then_some(f + 1);
            self.redraw();
        }
    }
}

fn run(
    window: Box<dyn OverlayWindow>,
    rx: &mpsc::Receiver<Cmd>,
    scale: f64,
    errors: &Arc<Mutex<Option<String>>>,
) {
    let mut w = Worker {
        window,
        arrow: render_arrow(scale),
        scale,
        canvas: canvas_size(scale),
        pos: None,
        ripple: None,
        errors: Arc::clone(errors),
    };
    loop {
        let wait = if w.ripple.is_some() {
            Duration::from_millis(RIPPLE_FRAME_MS)
        } else {
            Duration::from_millis(120)
        };
        match rx.recv_timeout(wait) {
            Ok(first) => {
                // Only the newest of a burst of moves is drawn; a hide or a ripple is never skipped.
                let mut cmd = first;
                while let Ok(next) = rx.try_recv() {
                    if matches!(cmd, Cmd::Move(..)) || matches!(next, Cmd::Hide) {
                        cmd = next;
                    } else {
                        w.apply(cmd);
                        cmd = next;
                    }
                }
                w.apply(cmd);
            }
            Err(RecvTimeoutError::Timeout) => w.tick(),
            Err(RecvTimeoutError::Disconnected) => break,
        }
        w.window.pump();
    }
    let _ = w.window.hide();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[derive(Debug, Clone, PartialEq)]
    enum Ev {
        Place((i32, i32), u32),
        Hide,
    }

    struct Fake(Arc<Mutex<Vec<Ev>>>, bool);
    impl OverlayWindow for Fake {
        fn place(&mut self, origin: (i32, i32), bitmap: &Bitmap) -> Result<(), String> {
            self.0.lock().push(Ev::Place(origin, bitmap.width));
            if self.1 { Err("boom".into()) } else { Ok(()) }
        }
        fn hide(&mut self) -> Result<(), String> {
            self.0.lock().push(Ev::Hide);
            Ok(())
        }
    }

    fn surface(fail: bool) -> (ThreadedSurface, Arc<Mutex<Vec<Ev>>>) {
        let log: Arc<Mutex<Vec<Ev>>> = Arc::default();
        let l = Arc::clone(&log);
        let s = ThreadedSurface::spawn("test-cursor", 1.0, move || {
            Ok(Box::new(Fake(l, fail)) as Box<dyn OverlayWindow>)
        })
        .unwrap();
        (s, log)
    }

    fn wait_for(log: &Arc<Mutex<Vec<Ev>>>, ok: impl Fn(&[Ev]) -> bool) -> Vec<Ev> {
        let end = Instant::now() + Duration::from_secs(3);
        loop {
            let snap = log.lock().clone();
            if ok(&snap) || Instant::now() > end {
                return snap;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_move_places_the_window_centred_on_the_point_and_hide_hides_it() {
        let (s, log) = surface(false);
        s.move_to(100.0, 200.0).unwrap();
        wait_for(&log, |l| !l.is_empty());
        s.hide().unwrap();
        let ev = wait_for(&log, |l| l.contains(&Ev::Hide));
        assert_eq!(ev[0], Ev::Place((64, 164), 72));
        assert!(ev.contains(&Ev::Hide));
    }

    #[test]
    fn a_ripple_plays_all_its_frames_then_settles_on_the_arrow() {
        let (s, log) = surface(false);
        s.ripple(10.0, 10.0).unwrap();
        let ev = wait_for(&log, |l| l.len() > RIPPLE_FRAMES);
        assert_eq!(
            ev.len(),
            RIPPLE_FRAMES + 1,
            "frame 0..last, then the arrow alone: {ev:?}"
        );
        assert!(ev.iter().all(|e| matches!(e, Ev::Place((-26, -26), 72))));
    }

    #[test]
    fn window_errors_are_kept_and_never_stop_the_surface() {
        let (s, log) = surface(true);
        s.move_to(1.0, 1.0).unwrap();
        wait_for(&log, |l| !l.is_empty());
        assert_eq!(s.last_error().as_deref(), Some("boom"));
        s.move_to(2.0, 2.0).unwrap();
        assert!(wait_for(&log, |l| l.len() >= 2).len() >= 2);
    }

    #[test]
    fn dropping_the_surface_hides_the_window_and_a_failed_window_is_reported() {
        let (s, log) = surface(false);
        s.move_to(5.0, 5.0).unwrap();
        wait_for(&log, |l| !l.is_empty());
        drop(s);
        assert_eq!(log.lock().last(), Some(&Ev::Hide));
        let err = ThreadedSurface::spawn("x", 1.0, || Err("no display".into()))
            .err()
            .unwrap();
        assert_eq!(err, "no display");
    }
}
