//! Windows OS backend: UI Automation, `SendInput`, and screenshot capture.
//!
//! [`WindowsBackend`] implements [`desktop_api::DesktopBackend`] using UI
//! Automation COM APIs on a dedicated STA thread, with `SendInput` for
//! keyboard and mouse actions. On other platforms this crate only exposes the
//! platform-independent helpers in [`pure`].

pub mod pure;

#[cfg(windows)]
mod backend;
#[cfg(windows)]
mod capture;
#[cfg(windows)]
mod input;
#[cfg(windows)]
mod uia;
#[cfg(windows)]
mod winlist;

#[cfg(windows)]
pub use backend::WindowsBackend;
