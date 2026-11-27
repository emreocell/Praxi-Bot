//! OS-agnostic Desktop trait and shared types for accessibility, input, and screenshots.
//!
//! All three OS backends implement this trait so the agent loop can drive any
//! desktop without knowing which operating system is running.

pub mod action;
pub mod backend;
pub mod covered;
pub mod cursor;
pub mod error;
pub mod grid;
pub mod guard;
pub mod guarded;
pub mod live_view;
pub mod render;
pub mod snapshot;

pub use action::{Action, ActionResult, ClickMethod, CoordinateSpace, Target};
pub use backend::{Capabilities, DesktopBackend};
pub use cursor::{CursorOverlay, CursorSurface};
pub use error::DesktopError;
pub use grid::Grid;
pub use guard::{ActGuard, InputTarget, ProcessIdentity, Refusal};
pub use guarded::{GuardFailure, InputProbe, InputSink};
pub use live_view::{CaptureScope, LiveViewOptions};
pub use render::{render_node, render_snapshot};
pub use snapshot::{
    Fingerprint, Image, Quality, Region, Snapshot, TreeQuality, UiNode, WindowInfo,
};
