//! Async [`DesktopBackend`] implementation for Linux (X11 / Xvfb + AT-SPI2).

use async_trait::async_trait;
use desktop_api::{
    Action, ActionResult, Capabilities, DesktopBackend, DesktopError, Image, Quality, Region,
    Snapshot, WindowInfo,
    snapshot::{AppSpec, Pid},
};
use tokio::sync::Mutex;

use crate::{atspi_tree::AtspiWorker, pure::detect_display_session, x11};

/// Linux desktop backend using AT-SPI2 over D-Bus, `XTest` (`x11rb`) input, and
/// X11 `GetImage` (`XGetImage`) screenshot capture.
#[derive(Debug, Default)]
pub struct LinuxBackend {
    worker: Mutex<AtspiWorker>,
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
        })
    }

    /// Bring a window to the foreground on the active X11 display.
    ///
    /// # Errors
    ///
    /// Fails if the X11 server cannot be reached or the window ID is invalid.
    pub async fn activate_window(&self, window_id: u64) -> Result<(), DesktopError> {
        tokio::task::spawn_blocking(move || x11::activate_window(window_id))
            .await
            .map_err(|e| DesktopError::Internal(format!("activate_window task: {e}")))?
    }
}

#[async_trait]
impl DesktopBackend for LinuxBackend {
    async fn snapshot(&self, scope: Option<u64>) -> Result<Snapshot, DesktopError> {
        let mut guard = self.worker.lock().await;
        guard.snapshot(scope).await
    }

    async fn act(&self, action: Action) -> Result<ActionResult, DesktopError> {
        let mut guard = self.worker.lock().await;
        guard.act(&action).await
    }

    async fn screenshot(&self, region: Region, quality: Quality) -> Result<Image, DesktopError> {
        tokio::task::spawn_blocking(move || x11::screenshot(&region, quality))
            .await
            .map_err(|e| DesktopError::Internal(format!("screenshot task: {e}")))?
    }

    async fn windows(&self) -> Result<Vec<WindowInfo>, DesktopError> {
        tokio::task::spawn_blocking(x11::list_windows)
            .await
            .map_err(|e| DesktopError::Internal(format!("windows task: {e}")))?
    }

    async fn launch(&self, app: AppSpec) -> Result<Pid, DesktopError> {
        tokio::task::spawn_blocking(move || x11::launch(&app))
            .await
            .map_err(|e| DesktopError::Internal(format!("launch task: {e}")))?
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
}
