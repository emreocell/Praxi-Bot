//! The desktop control gate: a hook the desktop and (visible) browser tools
//! call **before every mutating action** (click, type, key, scroll, drag,
//! set value, launch).
//!
//! The tools run inside the daemon process while the on-screen overlay lives
//! in the desktop app, so the daemon implements [`DesktopControlGate`]: it
//! asks the app to highlight the target first and refuses the action while the
//! human has the wheel.  Without a gate (CLI only) nothing changes.

use async_trait::async_trait;
use protocol::event::ControlRect;

/// One upcoming mutating action.
#[derive(Debug, Clone, PartialEq)]
pub struct ControlRequest {
    /// Run the tool call belongs to (as given in the `ToolContext`).
    pub run_id: String,
    /// Target of the action in global physical screen pixels, when known.
    pub rect: Option<ControlRect>,
    /// Short description of the target for the overlay label.
    pub label: String,
    /// `click`, `type`, `key`, `scroll`, `drag`, `set_value`, `launch`, ...
    pub action: String,
}

/// Decides whether a desktop action may go ahead.
#[async_trait]
pub trait DesktopControlGate: Send + Sync {
    /// Called before the action runs; may wait (the overlay highlight).
    ///
    /// # Errors
    /// A message for the model explaining why the action was refused.
    async fn before_action(&self, req: &ControlRequest) -> Result<(), String>;
}
