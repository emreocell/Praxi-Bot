//! Async [`DesktopBackend`] facade over the dedicated UIA thread.

use std::sync::{Arc, mpsc};

use async_trait::async_trait;
use desktop_api::{
    Action, ActionResult, Capabilities, DesktopBackend, DesktopError, Image, LiveViewOptions,
    Quality, Region, Snapshot, WindowInfo,
    live_view::DesktopViewTarget,
    snapshot::{AppSpec, Pid},
};
use futures::channel::oneshot;
use parking_lot::Mutex as SyncMutex;

use crate::{capture, live_view_win::open_windows_live_view_target, uia::Worker, winlist};

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
                let _ = otx.send(f(w));
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
        let res = self
            .run(move |_| winlist::activate(winlist::hwnd_from_id(window_id)))
            .await;
        self.notify_live_view();
        res
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

    async fn screenshot(&self, region: Region, quality: Quality) -> Result<Image, DesktopError> {
        self.run(move |_| capture::screenshot(&region, quality))
            .await
    }

    async fn windows(&self) -> Result<Vec<WindowInfo>, DesktopError> {
        self.run(|_| winlist::list_windows()).await
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
        *guard = Some(CachedLiveView {
            options: options.clone(),
            target: Arc::clone(&target),
        });
        Ok(target)
    }
}
