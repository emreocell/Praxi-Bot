//! The `DesktopBackend` trait.

use async_trait::async_trait;

use crate::{
    action::{Action, ActionResult},
    error::DesktopError,
    snapshot::{AppSpec, Image, Pid, Quality, Region, Snapshot, WindowInfo},
};

/// What the backend supports.
// Each field is a distinct, independent runtime capability; a bitfield or enum
// would obscure the semantics rather than improve them.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone)]
pub struct Capabilities {
    /// Platform name (e.g. `"windows"`, `"macos"`, `"linux"`).
    pub platform: String,
    /// Whether an accessibility tree can be extracted.
    pub accessibility_tree: bool,
    /// Whether screenshots are supported.
    pub screenshot: bool,
    /// Whether application launch is supported.
    pub app_launch: bool,
    /// Whether a display is available (false on headless servers without Xvfb).
    pub has_display: bool,
}

/// OS-agnostic interface for desktop automation.
///
/// All three OS backends (Windows, macOS, Linux) implement this trait.
/// The agent loop uses only this interface and is therefore OS-independent.
///
/// Every method is `async` to allow implementations to offload work to
/// dedicated OS threads (required on Windows for COM / UI Automation).
#[async_trait]
pub trait DesktopBackend: Send + Sync {
    /// Capture an accessibility tree snapshot of a window.
    ///
    /// `scope` identifies which window to snapshot.  `None` captures the
    /// focused window.
    async fn snapshot(&self, scope: Option<u64>) -> Result<Snapshot, DesktopError>;

    /// Dispatch an automation action and optionally return a fresh snapshot.
    async fn act(&self, action: Action) -> Result<ActionResult, DesktopError>;

    /// Capture a screenshot.
    async fn screenshot(&self, region: Region, quality: Quality) -> Result<Image, DesktopError>;

    /// List all visible windows.
    async fn windows(&self) -> Result<Vec<WindowInfo>, DesktopError>;

    /// Launch an application and return the new process ID.
    async fn launch(&self, app: AppSpec) -> Result<Pid, DesktopError>;

    /// Report what this backend supports.
    fn capabilities(&self) -> Capabilities;

    /// Return a live-view target adapter for watching and taking over this
    /// desktop backend's active host session, or return a typed capability /
    /// availability [`::live_view::Reason`].
    ///
    /// Default returns `Err(Reason::Unsupported)` so unsupported platforms,
    /// remote proxies, and existing test fakes remain source-compatible.
    ///
    /// # Errors
    ///
    /// Returns [`::live_view::Reason::Unsupported`] on unsupported platforms or
    /// [`::live_view::Reason::Unavailable`] if the active display session cannot
    /// be accessed.
    fn live_view_target(
        &self,
        _options: &crate::live_view::LiveViewOptions,
    ) -> Result<std::sync::Arc<dyn ::live_view::ViewTarget>, ::live_view::Reason> {
        Err(::live_view::Reason::Unsupported)
    }
}
