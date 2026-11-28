//! OS-agnostic Desktop trait and shared types for accessibility, input, and screenshots.
//!
//! All three OS backends implement this trait so the agent loop can drive any
//! desktop without knowing which operating system is running.

pub mod action;
pub mod backend;
pub mod covered;
pub mod cursor;
pub mod diff;
pub mod error;
pub mod grid;
pub mod guard;
pub mod guarded;
pub mod live_view;
pub mod render;
pub mod snapshot;
pub mod window;

pub use action::{Action, ActionResult, ClickMethod, CoordinateSpace, ElementOp, Target};
pub use backend::{Capabilities, DesktopBackend};
pub use cursor::{CursorOverlay, CursorSurface};
pub use diff::{NodeChange, NodeDiff, diff_nodes};
pub use error::DesktopError;
pub use grid::Grid;
pub use guard::{ActGuard, InputTarget, ProcessIdentity, Refusal};
pub use guarded::{GuardFailure, InputProbe, InputSink};
pub use live_view::{CaptureScope, LiveViewOptions};
pub use render::{render_node, render_snapshot};
pub use snapshot::{
    Fingerprint, Image, Quality, Region, Snapshot, TreeQuality, UiNode, WindowInfo,
};
pub use window::WindowOp;
