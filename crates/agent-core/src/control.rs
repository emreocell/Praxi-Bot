//! Desktop control bridge, loop side.
//!
//! The desktop and visible-browser tools call a
//! [`builtin_tools::DesktopControlGate`] before every mutating action.  The
//! loop installs an adapter that resolves the run behind the call and hands
//! the request to the [`ControlGate`] the embedder (the daemon) registered
//! with [`AgentLoop::set_control_gate`](crate::AgentLoop::set_control_gate).
//! The embedder decides (ask the desktop app, refuse while the human drives,
//! ...) and may publish a [`protocol::event::ControlRequested`] event on the run's
//! stream through the `publish` callback.  No gate registered = no-op.

use std::sync::{Arc, Weak};

use async_trait::async_trait;
use builtin_tools::{ControlRequest as ToolRequest, DesktopControlGate};
use protocol::event::{ControlRect, ControlRequested};
use protocol::{BotId, Event, RunId};
use uuid::Uuid;

use crate::shared::Shared;

/// One upcoming mutating desktop action, with the identity of its run.
#[derive(Debug, Clone, PartialEq)]
pub struct ControlRequest {
    /// Id the answer (`POST /v1/control/{request_id}`) refers to.
    pub request_id: Uuid,
    /// Run the action belongs to.
    pub run_id: RunId,
    /// Bot running it.
    pub bot_id: BotId,
    /// Target in global physical screen pixels, when known.
    pub rect: Option<ControlRect>,
    /// Short description of the target.
    pub label: String,
    /// `click`, `type`, `key`, `scroll`, `drag`, `set_value`, `launch`, ...
    pub action: String,
}

/// Decides whether a desktop action may go ahead (implemented by the daemon).
#[async_trait]
pub trait ControlGate: Send + Sync {
    /// Called before each mutating desktop action.  `publish` emits the
    /// request as a `control_requested` event on the run's stream; call it
    /// when a desktop app should be asked.
    ///
    /// # Errors
    /// A message for the model explaining why the action was refused.
    async fn before_action(
        &self,
        req: ControlRequest,
        publish: &(dyn Fn() + Send + Sync),
    ) -> Result<(), String>;
}

/// The [`DesktopControlGate`] the builtin tools get: resolves the run and
/// forwards to the registered [`ControlGate`].
pub(crate) struct LoopControl {
    pub sh: Weak<Shared>,
}

#[async_trait]
impl DesktopControlGate for LoopControl {
    async fn before_action(&self, req: &ToolRequest) -> Result<(), String> {
        let Some(sh) = self.sh.upgrade() else {
            return Ok(());
        };
        let Some(gate) = sh.control.read().clone() else {
            return Ok(());
        };
        let Some(ctx) = req
            .run_id
            .parse::<Uuid>()
            .ok()
            .and_then(|id| sh.runs.lock().get(&RunId(id)).cloned())
        else {
            // Not a live run (e.g. a tool used outside a turn): nothing to show.
            return Ok(());
        };
        let request = ControlRequest {
            request_id: Uuid::now_v7(),
            run_id: ctx.run_id,
            bot_id: ctx.bot_id,
            rect: req.rect,
            label: req.label.clone(),
            action: req.action.clone(),
        };
        let event = Event::ControlRequested(ControlRequested {
            request_id: request.request_id,
            run_id: request.run_id,
            bot_id: request.bot_id,
            rect: request.rect,
            label: request.label.clone(),
            action: request.action.clone(),
        });
        let sh2: Arc<Shared> = Arc::clone(&sh);
        let publish = move || sh2.emit(&ctx, event.clone());
        gate.before_action(request, &publish).await
    }
}
