//! Linux OS backend: AT-SPI2 accessibility, `XTest` (`x11rb`) input, and
//! `XGetImage` screenshot capture.
//!
//! [`LinuxBackend`] implements [`desktop_api::DesktopBackend`] on an X11
//! session (physical display or headless `Xvfb`). Wayland sessions are
//! detected and rejected with a clear [`desktop_api::DesktopError::Unsupported`]
//! error in v0. On non-Linux platforms this crate exposes the platform-independent
//! helpers in [`pure`].

pub mod pure;

#[cfg(target_os = "linux")]
mod atspi_tree;
#[cfg(target_os = "linux")]
mod backend;
#[cfg(target_os = "linux")]
mod x11;

#[cfg(target_os = "linux")]
pub use backend::LinuxBackend;
