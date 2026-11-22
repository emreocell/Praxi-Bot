//! Linux OS backend: AT-SPI2 accessibility, `XTest` (`x11rb`) input, and
//! `XGetImage` screenshot capture.
//!
//! [`LinuxBackend`] implements [`desktop_api::DesktopBackend`] on an X11
//! session (physical display or headless `Xvfb`). Wayland sessions are
//! detected and rejected with a clear [`desktop_api::DesktopError::Unsupported`]
//! error in v0. On non-Linux platforms this crate exposes the platform-independent
//! helpers in [`pure`].

pub mod helper;
pub mod live_view_x11;
pub mod pure;

/// Bounded X11 handshake. Linux-only: the Windows build has no X server.
///
/// # Errors
///
/// Returns a string that names the display and the cause (unset `DISPLAY`,
/// Wayland, timeout, or the X11 error text).
#[cfg(target_os = "linux")]
pub fn probe_x_server(timeout: std::time::Duration) -> Result<(), String> {
    x11::probe_x_server(timeout)
}

#[cfg(target_os = "linux")]
mod atspi_tree;
#[cfg(target_os = "linux")]
mod backend;
#[cfg(target_os = "linux")]
mod x11;

#[cfg(target_os = "linux")]
pub use backend::LinuxBackend;
