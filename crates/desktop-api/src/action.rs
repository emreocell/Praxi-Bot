//! Action types for desktop automation.

use serde::{Deserialize, Serialize};

use crate::snapshot::Snapshot;

/// How input is delivered to the target.
///
/// Methods are tried in order: Accessibility → `AppPost` → Global.
/// `Global` moves the real cursor and must be explicitly requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClickMethod {
    /// Use the platform accessibility API (UIA pattern / `AXPress`). Preferred.
    Accessibility,
    /// Post a located mouse event to the target process (no real cursor move).
    AppPost,
    /// Warp the real system cursor and click.  Named explicitly; never chosen by default.
    Global,
}

/// Coordinate space for coordinate-based targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoordinateSpace {
    /// Physical screen pixels.
    Absolute,
    /// Normalised 0–999 range over the target window.
    Normalized999,
}

/// How the target element is identified.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Target {
    /// Target by stable index + generation (from the most recent [`Snapshot`]).
    ByIndex {
        index: u32,
        /// Snapshot generation the index was taken from.
        generation: u64,
    },
    /// Target by screen-space or normalised coordinate.
    ByCoordinate {
        x: f64,
        y: f64,
        space: CoordinateSpace,
    },
}

/// A single desktop automation action.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    /// Click an element or coordinate.
    Click { target: Target, method: ClickMethod },
    /// Type text into the focused element.
    TypeText { text: String },
    /// Set the value of a form control (text field, combo box, etc.).
    SetValue { target: Target, value: String },
    /// Scroll within an element.
    Scroll {
        target: Target,
        delta_x: f64,
        delta_y: f64,
    },
    /// Press one or more keyboard keys (e.g. `["Return"]`, `["cmd", "c"]`).
    KeyPress { keys: Vec<String> },
    /// Drag from one target to another.
    Drag {
        from: Target,
        to: Target,
        method: ClickMethod,
    },
}

/// The result of dispatching a [`Action`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionResult {
    /// `true` if the action was successfully dispatched to the target.
    pub dispatched: bool,
    /// Whether the expected post-action change was verified.
    ///
    /// `None` = verification was not attempted.
    pub verified: Option<bool>,
    /// A fresh snapshot taken after the action (if requested).
    pub new_snapshot: Option<Snapshot>,
}
