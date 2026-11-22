//! Async [`DesktopBackend`] facade over the dedicated UIA thread.

use std::sync::{Arc, mpsc};

use async_trait::async_trait;
use desktop_api::{
    ActGuard, Action, ActionResult, Capabilities, DesktopBackend, DesktopError, Image, InputTarget,
    LiveViewOptions, Quality, Refusal, Region, Snapshot, WindowInfo,
    live_view::DesktopViewTarget,
    snapshot::{AppSpec, Pid},
};
use futures::channel::oneshot;
use parking_lot::Mutex as SyncMutex;

use crate::{
    capture, identity, live_view_win::open_windows_live_view_target, uia::Worker, winlist,
};

type Job = Box<dyn FnOnce(&mut Worker) + Send>;

struct CachedLiveView {
    options: LiveViewOptions,
    target: Arc<DesktopViewTarget>,
}

/// Windows desktop backend.
///
/// All UI Automation calls run on one dedicated STA thread that owns the COM
/// objects; async methods post a closure to it and await the reply.
pub struct WindowsBackend {
    tx: mpsc::Sender<Job>,
    live_view: SyncMutex<Option<CachedLiveView>>,
}

impl WindowsBackend {
    /// Spawn the UIA thread and initialise COM / `IUIAutomation` on it.
    ///
    /// Also switches the process to per-monitor DPI awareness so all
    /// coordinates are physical pixels.
    ///
    /// # Errors
    ///
    /// Returns an error if COM or UI Automation cannot be initialised.
    pub fn new() -> Result<Self, DesktopError> {
        let (tx, rx) = mpsc::channel::<Job>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), DesktopError>>();
        std::thread::Builder::new()
            .name("uia-sta".into())
            .spawn(move || {
                let mut worker = match Worker::new() {
                    Ok(w) => {
                        let _ = ready_tx.send(Ok(()));
                        w
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                // Ends when every sender (the backend) is dropped.
                while let Ok(job) = rx.recv() {
                    job(&mut worker);
                }
                worker.close();
            })
            .map_err(|e| DesktopError::Internal(format!("spawn UIA thread: {e}")))?;
        ready_rx
            .recv()
            .map_err(|_| DesktopError::Internal("UIA thread died during init".into()))??;
        Ok(Self {
            tx,
            live_view: SyncMutex::new(None),
        })
    }

    async fn run<T, F>(&self, f: F) -> Result<T, DesktopError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Worker) -> Result<T, DesktopError> + Send + 'static,
    {
        let (otx, orx) = oneshot::channel();
        self.tx
            .send(Box::new(move |w| {
                // A canceled request must not act after waiting in the STA queue.
                if !otx.is_canceled() {
                    let _ = otx.send(winlist::require_interactive_desktop().and_then(|()| f(w)));
                }
            }))
            .map_err(|_| DesktopError::Internal("UIA thread is gone".into()))?;
        orx.await
            .map_err(|_| DesktopError::Internal("UIA thread dropped the request".into()))?
    }

    fn notify_live_view(&self) {
        if let Some(entry) = self.live_view.lock().as_ref() {
            entry.target.notify_desktop_action();
        }
    }

    /// Bring a window to the foreground (uses the Alt-key workaround for the
    /// foreground lock).
    ///
    /// # Errors
    ///
    /// Fails if the window no longer exists or Windows refuses the switch.
    pub async fn activate_window(&self, window_id: u64) -> Result<(), DesktopError> {
        self.focus(window_id, None).await
    }
}

#[async_trait]
impl DesktopBackend for WindowsBackend {
    async fn snapshot(&self, scope: Option<u64>) -> Result<Snapshot, DesktopError> {
        self.run(move |w| w.snapshot(scope)).await
    }

    async fn act(&self, action: Action) -> Result<ActionResult, DesktopError> {
        let res = self.run(move |w| w.act(&action)).await;
        self.notify_live_view();
        res
    }

    async fn input_target(&self) -> Result<Option<InputTarget>, DesktopError> {
        self.run(|w| Ok(w.input_target())).await
    }

    async fn act_guarded(
        &self,
        action: Action,
        guard: ActGuard,
    ) -> Result<ActionResult, DesktopError> {
        let res = self.run(move |w| w.act_with(&action, Some(&guard))).await;
        self.notify_live_view();
        res
    }

    async fn act_approved(
        &self,
        action: Action,
        guard: ActGuard,
    ) -> Result<ActionResult, DesktopError> {
        let res = self.run(move |w| w.act_approved(&action, &guard)).await;
        self.notify_live_view();
        res
    }

    fn key_chord_refusal(&self, keys: &[String]) -> Option<Refusal> {
        (!desktop_api::guarded::chord_allowed(keys)).then_some(Refusal::Chord)
    }

    async fn screenshot(&self, region: Region, quality: Quality) -> Result<Image, DesktopError> {
        self.run(move |_| capture::screenshot(&region, quality))
            .await
    }

    async fn windows(&self) -> Result<Vec<WindowInfo>, DesktopError> {
        self.run(|_| winlist::list_windows()).await
    }

    async fn focus_target(&self, window_id: u64) -> Result<Option<InputTarget>, DesktopError> {
        self.run(move |_| Ok(identity::top_level_facts(winlist::hwnd_from_id(window_id))))
            .await
    }

    async fn focus(&self, window_id: u64, guard: Option<ActGuard>) -> Result<(), DesktopError> {
        let res = self
            .run(move |_| {
                let hwnd = winlist::hwnd_from_id(window_id);
                if let Some(guard) = guard
                    && !identity::top_level_facts(hwnd)
                        .is_some_and(|target| guard.expect.same_program(&target.identity))
                {
                    return Err(DesktopError::GuardRefused(
                        Refusal::ForegroundChanged.why().into(),
                    ));
                }
                winlist::activate(hwnd)
            })
            .await;
        self.notify_live_view();
        res
    }

    async fn launch(&self, app: AppSpec) -> Result<Pid, DesktopError> {
        let res = self.run(move |_| winlist::launch(&app)).await;
        self.notify_live_view();
        res
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            platform: "windows".into(),
            accessibility_tree: true,
            screenshot: true,
            app_launch: true,
            has_display: true,
        }
    }

    fn live_view_target(
        &self,
        options: &LiveViewOptions,
    ) -> Result<Arc<dyn ::live_view::ViewTarget>, ::live_view::Reason> {
        let mut guard = self.live_view.lock();
        if let Some(cached) = guard.as_ref()
            && &cached.options == options
        {
            return Ok(Arc::clone(&cached.target) as Arc<dyn ::live_view::ViewTarget>);
        }
        let target = open_windows_live_view_target(options)?;
        if let Some(prev) = guard.replace(CachedLiveView {
            options: options.clone(),
            target: Arc::clone(&target),
        }) {
            prev.target.shutdown();
        }
        Ok(target)
    }
}
