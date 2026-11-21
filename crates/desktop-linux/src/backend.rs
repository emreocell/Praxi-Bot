//! Async [`DesktopBackend`] implementation for Linux (X11 / Xvfb + AT-SPI2).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use desktop_api::{
    Action, ActionResult, Capabilities, DesktopBackend, DesktopError, Image, LiveViewOptions,
    Quality, Region, Snapshot, WindowInfo,
    live_view::DesktopViewTarget,
    snapshot::{AppSpec, Pid},
};
use parking_lot::Mutex as SyncMutex;
use tokio::sync::Mutex;

use crate::{
    atspi_tree::AtspiWorker, live_view_x11::open_x11_live_view_target,
    pure::{self, detect_display_session},
    x11,
};

/// Screenshot and window listing should fail fast when the X server is wedged.
const X11_LIMIT: Duration = Duration::from_secs(8);
/// A large AT-SPI tree can be slow; still far under the agent's 120s tool timeout.
const TREE_LIMIT: Duration = Duration::from_secs(25);
/// Typing walks the keymap once, then one round trip per key.
const ACT_LIMIT: Duration = Duration::from_secs(20);

fn display_label() -> String {
    std::env::var("DISPLAY")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "(unset)".into())
}

async fn bounded<T>(
    op: &str,
    limit: Duration,
    fut: impl std::future::Future<Output = Result<T, DesktopError>>,
) -> Result<T, DesktopError> {
    match tokio::time::timeout(limit, fut).await {
        Ok(result) => result,
        Err(_) => Err(DesktopError::Accessibility(pure::op_timed_out(
            op,
            &display_label(),
            limit,
        ))),
    }
}

struct CachedLiveView {
    options: LiveViewOptions,
    target: Arc<DesktopViewTarget>,
}

/// Linux desktop backend using AT-SPI2 over D-Bus, `XTest` (`x11rb`) input, and
/// X11 `GetImage` (`XGetImage`) screenshot capture.
pub struct LinuxBackend {
    worker: Mutex<AtspiWorker>,
    live_view: SyncMutex<Option<CachedLiveView>>,
}

impl Default for LinuxBackend {
    fn default() -> Self {
        Self {
            worker: Mutex::new(AtspiWorker::default()),
            live_view: SyncMutex::new(None),
        }
    }
}

impl std::fmt::Debug for LinuxBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinuxBackend").finish_non_exhaustive()
    }
}

impl LinuxBackend {
    /// Create a new [`LinuxBackend`].
    ///
    /// # Errors
    ///
    /// Returns [`DesktopError::Unsupported`] if the current session is running
    /// under Wayland (Wayland is out of scope for v0).
    pub fn new() -> Result<Self, DesktopError> {
        let xdg = std::env::var("XDG_SESSION_TYPE").ok();
        let wayland = std::env::var("WAYLAND_DISPLAY").ok();
        // Reject Wayland sessions immediately with a clear explanation.
        if let Err(e @ DesktopError::Unsupported(_)) =
            detect_display_session(xdg.as_deref(), wayland.as_deref(), Some(":0"))
        {
            return Err(e);
        }
        Ok(Self {
            worker: Mutex::new(AtspiWorker::new()),
            live_view: SyncMutex::new(None),
        })
    }

    fn notify_live_view(&self) {
        if let Some(entry) = self.live_view.lock().as_ref() {
            entry.target.notify_desktop_action();
        }
    }

    /// Bring a window to the foreground on the active X11 display.
    ///
    /// # Errors
    ///
    /// Fails if the X11 server cannot be reached or the window ID is invalid.
    pub async fn activate_window(&self, window_id: u64) -> Result<(), DesktopError> {
        let res = bounded("activate_window", X11_LIMIT, async move {
            match tokio::task::spawn_blocking(move || x11::activate_window(window_id)).await {
                Ok(result) => result,
                Err(e) => Err(DesktopError::Internal(format!(
                    "activate_window task: {e}"
                ))),
            }
        })
        .await?;
        self.notify_live_view();
        Ok(res)
    }
}

#[async_trait]
impl DesktopBackend for LinuxBackend {
    async fn snapshot(&self, scope: Option<u64>) -> Result<Snapshot, DesktopError> {
        let mut guard = self.worker.lock().await;
        bounded("accessibility snapshot", TREE_LIMIT, guard.snapshot(scope)).await
    }

    async fn act(&self, action: Action) -> Result<ActionResult, DesktopError> {
        let res = bounded("desktop input", ACT_LIMIT, async {
            let mut guard = self.worker.lock().await;
            guard.act(&action).await
        })
        .await;
        if res.is_ok() {
            self.notify_live_view();
        }
        res
    }

    async fn screenshot(&self, region: Region, quality: Quality) -> Result<Image, DesktopError> {
        bounded("screenshot", X11_LIMIT, async move {
            match tokio::task::spawn_blocking(move || x11::screenshot(&region, quality)).await {
                Ok(result) => result,
                Err(e) => Err(DesktopError::Internal(format!("screenshot task: {e}"))),
            }
        })
        .await
    }

    async fn windows(&self) -> Result<Vec<WindowInfo>, DesktopError> {
        bounded("window list", X11_LIMIT, async {
            match tokio::task::spawn_blocking(x11::list_windows).await {
                Ok(result) => result,
                Err(e) => Err(DesktopError::Internal(format!("windows task: {e}"))),
            }
        })
        .await
    }

    async fn launch(&self, app: AppSpec) -> Result<Pid, DesktopError> {
        let res = bounded("launch", X11_LIMIT, async move {
            match tokio::task::spawn_blocking(move || x11::launch(&app)).await {
                Ok(result) => result,
                Err(e) => Err(DesktopError::Internal(format!("launch task: {e}"))),
            }
        })
        .await?;
        self.notify_live_view();
        Ok(res)
    }

    fn capabilities(&self) -> Capabilities {
        let has_display = x11::has_x11_display();
        Capabilities {
            platform: "linux".into(),
            accessibility_tree: has_display,
            screenshot: has_display,
            app_launch: has_display,
            has_display,
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
        let target = open_x11_live_view_target(options)?;
        if let Some(prev) = guard.take() {
            prev.target.shutdown();
        }
        *guard = Some(CachedLiveView {
            options: options.clone(),
            target: Arc::clone(&target),
        });
        Ok(target)
    }
}
