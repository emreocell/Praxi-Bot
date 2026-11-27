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

/// An element-level accessibility action that is not a click: a UIA control
/// pattern on Windows, an AT-SPI action or state on Linux. None of these move
/// the pointer or type, so they work on a window that is covered or behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ElementOp {
    /// Run the element's default action (press a button, follow a link).
    Invoke,
    /// Flip a check box or toggle, or set it: `state: Some(on)` does nothing
    /// when the element already is in that state.
    Toggle { state: Option<bool> },
    /// Expand a tree item, combo box or menu.
    Expand,
    /// Collapse a tree item, combo box or menu.
    Collapse,
    /// Select a list, tab, tree or radio item.
    Select,
    /// Bring an element that is scrolled out of view into view.
    ScrollIntoView,
}

impl ElementOp {
    /// A short name for cards, logs and results.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Invoke => "invoke",
            Self::Toggle { state: None } => "toggle",
            Self::Toggle { state: Some(true) } => "switch on",
            Self::Toggle { state: Some(false) } => "switch off",
            Self::Expand => "expand",
            Self::Collapse => "collapse",
            Self::Select => "select",
            Self::ScrollIntoView => "scroll into view",
        }
    }
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
    /// An accessibility action on one element (never a coordinate).
    Element { target: Target, op: ElementOp },
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
