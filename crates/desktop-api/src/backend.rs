//! The `DesktopBackend` trait.

use async_trait::async_trait;

use crate::{
    action::{Action, ActionResult},
    error::DesktopError,
    guard::{ActGuard, InputTarget, Refusal},
    snapshot::{AppSpec, Image, Pid, Quality, Region, Snapshot, WindowInfo},
    window::WindowOp,
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

    /// Bring a top-level window to the foreground. A remote approval can bind
    /// the operation to the observed process identity.
    async fn focus(&self, _window_id: u64, _guard: Option<ActGuard>) -> Result<(), DesktopError> {
        Err(DesktopError::Unsupported(
            "window focus is unavailable".into(),
        ))
    }

    /// Minimise, maximise, restore, move, resize or close one top-level
    /// window. `guard` binds the call to the program an approval was shown
    /// for (the same identity [`Self::focus_target`] reports); a backend that
    /// receives one must refuse when the window now belongs to another program.
    ///
    /// # Errors
    /// [`DesktopError::Unsupported`] by default; the window is gone, the OS
    /// refused, or the guard did not match.
    async fn window_op(
        &self,
        _window_id: u64,
        _op: WindowOp,
        _guard: Option<ActGuard>,
    ) -> Result<(), DesktopError> {
        Err(DesktopError::Unsupported(
            "window management is unavailable on this computer".into(),
        ))
    }

    /// The text on the clipboard. `Ok(None)` when it holds no text (empty, an
    /// image, files) or the program that put it there asked for it to be kept
    /// out of clipboard tools (a password manager).
    ///
    /// # Errors
    /// [`DesktopError::Unsupported`] by default, or the clipboard could not
    /// be opened.
    async fn clipboard_text(&self) -> Result<Option<String>, DesktopError> {
        Err(DesktopError::Unsupported(
            "the clipboard is unavailable on this computer".into(),
        ))
    }

    /// Replace the clipboard content with `text`.
    ///
    /// # Errors
    /// [`DesktopError::Unsupported`] by default, or the clipboard could not
    /// be opened.
    async fn set_clipboard_text(&self, _text: &str) -> Result<(), DesktopError> {
        Err(DesktopError::Unsupported(
            "the clipboard is unavailable on this computer".into(),
        ))
    }

    /// Identify a specific window for a focus approval.
    async fn focus_target(&self, _window_id: u64) -> Result<Option<InputTarget>, DesktopError> {
        Ok(None)
    }

    /// Launch an application and return the new process ID.
    async fn launch(&self, app: AppSpec) -> Result<Pid, DesktopError>;

    /// The window and program that keyboard input would reach right now, with
    /// its process identity, elevation and class.
    ///
    /// `Ok(None)` (the default) means this backend cannot bind input to one
    /// program, so a run-scoped control grant is never offered or applied.
    ///
    /// # Errors
    /// The OS query failed; callers treat that as "no identity".
    async fn input_target(&self) -> Result<Option<InputTarget>, DesktopError> {
        Ok(None)
    }

    /// [`Self::act`], but the backend first checks `guard` against the window
    /// that would receive the input, as close to the OS send as it can, and
    /// does not send when the check fails. Replay and the desktop tools both
    /// pass through here.
    ///
    /// The default refuses: a backend that cannot check must not claim to.
    ///
    /// # Errors
    /// [`DesktopError::GuardRefused`] when nothing was sent because of the
    /// guard, otherwise as [`Self::act`].
    async fn act_guarded(
        &self,
        _action: Action,
        _guard: ActGuard,
    ) -> Result<ActionResult, DesktopError> {
        Err(DesktopError::GuardRefused(
            "not sent: this computer cannot check the target window before sending input".into(),
        ))
    }

    /// Dispatch an individually approved action with live receiver checks.
    /// Backends may support additional actions here without broadening the
    /// action set covered by a task-wide grant.
    async fn act_approved(
        &self,
        action: Action,
        guard: ActGuard,
    ) -> Result<ActionResult, DesktopError> {
        self.act_guarded(action, guard).await
    }

    /// Whether `keys` open a launcher or switch windows (Win/Super, Ctrl+Esc,
    /// Alt+Tab, ...), judged with this backend's own key parser. The default
    /// refuses every chord.
    fn key_chord_refusal(&self, _keys: &[String]) -> Option<Refusal> {
        Some(Refusal::Chord)
    }

    /// The top-level window under the screen point `(x, y)` (physical pixels), as this backend
    /// numbers windows, or `Ok(None)` when it cannot tell (the default: the check is skipped and
    /// the action goes ahead as before). Used to refuse a pointer action that would land on a
    /// different window than the one that was observed.
    ///
    /// # Errors
    /// The OS query failed; callers treat that as "cannot tell".
    async fn window_at(&self, _x: f64, _y: f64) -> Result<Option<u64>, DesktopError> {
        Ok(None)
    }

    /// Whether this backend drives the screen of the machine it runs on (not a paired computer
    /// reached over a link). Only then are the bot cursor's positions meaningful for this
    /// daemon's own live view. Defaults to `false`.
    fn is_local_screen(&self) -> bool {
        false
    }

    /// A surface for the bot's own pointer: a click-through, always-on-top, never-focused
    /// window that shows where the bot is about to click. `None` (the default) when this backend
    /// cannot draw one (no display, an unsupported platform, a remote computer).
    fn cursor_surface(&self) -> Option<std::sync::Arc<dyn crate::cursor::CursorSurface>> {
        None
    }

    /// Report what this backend supports.
    fn capabilities(&self) -> Capabilities;

    /// A VERIFIED identity of the program that owns `window` (for example a
    /// code-signing publisher plus the executable's hash), as an opaque string
    /// the owner can allow-list. `None` means this backend cannot verify it
    /// (the default: a window title or pid is not an identity). Owner-stored
    /// secrets are only ever typed into a program whose verified identity is on
    /// the secret's allow-list, so a backend that returns `None` can never
    /// receive one.
    async fn program_identity(&self, window: &WindowInfo) -> Option<String> {
        let _ = window;
        None
    }

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
