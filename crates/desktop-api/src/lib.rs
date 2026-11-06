//! OS-agnostic Desktop trait and shared types for accessibility, input, and screenshots.
//!
//! All three OS backends implement this trait so the agent loop can drive any
//! desktop without knowing which operating system is running.

pub mod action;
pub mod backend;
pub mod error;
pub mod grid;
pub mod render;
pub mod snapshot;

pub use action::{Action, ActionResult, ClickMethod, CoordinateSpace, Target};
pub use backend::{Capabilities, DesktopBackend};
pub use error::DesktopError;
pub use grid::Grid;
pub use render::{render_node, render_snapshot};
pub use snapshot::{
    Fingerprint, Image, Quality, Region, Snapshot, TreeQuality, UiNode, WindowInfo,
};
