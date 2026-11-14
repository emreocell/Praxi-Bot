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
use builtin_tools::{ActionPermit, ControlRequest as ToolRequest, DesktopControlGate};
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
    /// Same admission authority for headless mutations without local highlighting.
    async fn admit_background_action(
        self: Arc<Self>,
        req: ControlRequest,
    ) -> Result<ActionPermit, String> {
        self.admit_action(req, &|| {}).await
    }
    fn observation_epoch(&self) -> u64 {
        0
    }
    /// A message while observation tools must be refused (the human drives).
    fn observation_refused(&self) -> Option<String> {
        None
    }
    /// While a pause, a human takeover or a local override is in force.
    fn latch_refusal(&self) -> Option<String> {
        None
    }
    fn is_paused(&self) -> bool {
        false
    }
    /// Close the admission/action race; the permit survives until backend completion.
    async fn admit_action(
        self: Arc<Self>,
        req: ControlRequest,
        publish: &(dyn Fn() + Send + Sync),
    ) -> Result<ActionPermit, String> {
        self.before_action(req, publish).await?;
        Ok(ActionPermit::default())
    }

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

const NOT_CONSULTABLE: &str = "This desktop action was refused because the run it belongs to is not active, so the control state (pause / human driving) cannot be checked. Do not retry it; ask the user how to continue.";

/// The [`DesktopControlGate`] the builtin tools get: resolves the run and
/// forwards to the registered [`ControlGate`].
pub(crate) struct LoopControl {
    pub sh: Weak<Shared>,
}

#[async_trait]
impl DesktopControlGate for LoopControl {
    fn observation_epoch(&self) -> u64 {
        self.sh
            .upgrade()
            .and_then(|sh| sh.control.read().clone())
            .map_or(0, |gate| gate.observation_epoch())
    }

    fn computer_in_use(&self) -> bool {
        // A run that took a computer slot holds it until it ends.
        self.sh.upgrade().is_some_and(|sh| {
            sh.computer_slots.available_permits()
                < sh.cfg
                    .max_computer_runs
                    .clamp(1, tokio::sync::Semaphore::MAX_PERMITS)
        })
    }

    fn computer_in_use_for_bot(&self, bot_id: &str) -> bool {
        self.sh.upgrade().is_some_and(|sh| {
            sh.runs
                .lock()
                .values()
                .any(|ctx| ctx.bot_id.to_string() == bot_id && ctx.computer.get().is_some())
        })
    }

    fn observation_refused(&self) -> Option<String> {
        self.sh
            .upgrade()
            .and_then(|sh| sh.control.read().clone())
            .and_then(|gate| gate.observation_refused())
    }

    fn latch_refusal(&self) -> Option<String> {
        self.sh
            .upgrade()
            .and_then(|sh| sh.control.read().clone())
            .and_then(|gate| gate.latch_refusal())
    }

    async fn before_action(&self, req: &ToolRequest) -> Result<(), String> {
        self.admit_action(req).await.map(drop)
    }
    async fn admit_action(&self, req: &ToolRequest) -> Result<ActionPermit, String> {
        self.admit(req, true).await
    }
    async fn admit_background_action(&self, req: &ToolRequest) -> Result<ActionPermit, String> {
        self.admit(req, false).await
    }
}
impl LoopControl {
    async fn admit(&self, req: &ToolRequest, highlight: bool) -> Result<ActionPermit, String> {
        let Some(sh) = self.sh.upgrade() else {
            // Fail closed: if the loop is gone the latch cannot be consulted.
            return Err(NOT_CONSULTABLE.to_owned());
        };
        let gate = sh.control.read().clone();
        let ctx = req
            .run_id
            .parse::<Uuid>()
            .ok()
            .and_then(|id| sh.runs.lock().get(&RunId(id)).cloned());
        // Not a live run (a tool used outside a turn, or a stale id): the
        // request cannot be attributed to a run and so cannot be checked
        // against the control latch.  Fail closed.
        let Some(ctx) = ctx else {
            return Err(NOT_CONSULTABLE.to_owned());
        };
        acquire_computer_slot(&sh, &ctx).await?;
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
        let computer = Arc::clone(&ctx.computer);
        let sh2: Arc<Shared> = Arc::clone(&sh);
        let publish = move || sh2.emit(&ctx, event.clone());
        let permit = if let Some(gate) = gate {
            if highlight {
                gate.admit_action(request, &publish).await?
            } else {
                gate.admit_background_action(request).await?
            }
        } else {
            ActionPermit::default()
        };
        let validator = permit.validator();
        let epoch = permit.epoch();
        let wrapped = ActionPermit::new(permit.needs_observation(), move || {
            let _computer = computer;
            drop(permit);
        })
        .with_validator(validator);
        wrapped.set_epoch(epoch);
        Ok(wrapped)
    }
}

/// One permit per root run, shared with its subagents and held until completion.
/// Observation calls and script delegates reserve the same slot as mutations.
pub(crate) async fn acquire_computer_slot(
    sh: &Shared,
    ctx: &crate::shared::RunCtx,
) -> Result<(), String> {
    let waiting = ComputerWait(Arc::clone(&ctx.waiting_computer));
    if ctx.computer.get().is_none() {
        ctx.waiting_computer
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
    ctx.computer
        .get_or_try_init(|| async {
            match Arc::clone(&sh.computer_slots).try_acquire_owned() {
                Ok(permit) => Ok(permit),
                Err(tokio::sync::TryAcquireError::Closed) => {
                    Err("computer admission closed".to_string())
                }
                Err(tokio::sync::TryAcquireError::NoPermits) => {
                    sh.emit(
                        ctx,
                        Event::MessageDelta(protocol::event::MessageDelta {
                            run_id: ctx.run_id,
                            text: "Waiting for computer.\n".into(),
                        }),
                    );
                    Arc::clone(&sh.computer_slots)
                        .acquire_owned()
                        .await
                        .map_err(|_| "computer admission closed".to_string())
                }
            }
        })
        .await?;
    drop(waiting);
    Ok(())
}

/// Tool timeouts count active time; a human pause remains a cancellable wait.
pub(crate) async fn pause_aware_timeout<F: std::future::Future>(
    budget: std::time::Duration,
    gate: Option<Arc<dyn ControlGate>>,
    waiting: Arc<std::sync::atomic::AtomicBool>,
    future: F,
) -> Result<F::Output, ()> {
    tokio::pin!(future);
    let mut left = budget;
    loop {
        let started = tokio::time::Instant::now();
        let paused = gate.as_ref().is_some_and(|g| g.is_paused())
            || waiting.load(std::sync::atomic::Ordering::SeqCst);
        let slice = if paused {
            std::time::Duration::from_millis(50)
        } else {
            left.min(std::time::Duration::from_millis(50))
        };
        tokio::select! {
            biased;
            out = &mut future => return Ok(out),
            () = tokio::time::sleep(slice) => {
                if !paused && !gate.as_ref().is_some_and(|g| g.is_paused()) && !waiting.load(std::sync::atomic::Ordering::SeqCst) { left = left.saturating_sub(started.elapsed()); if left.is_zero() { return Err(()); } }
            }
        }
    }
}

struct ComputerWait(Arc<std::sync::atomic::AtomicBool>);
impl Drop for ComputerWait {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Paused;
    #[async_trait]
    impl ControlGate for Paused {
        fn is_paused(&self) -> bool {
            true
        }
        async fn before_action(
            &self,
            _req: ControlRequest,
            _publish: &(dyn Fn() + Send + Sync),
        ) -> Result<(), String> {
            Ok(())
        }
    }
    #[tokio::test(start_paused = true)]
    async fn human_pause_does_not_consume_tool_budget_and_remains_cancellable() {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let waiting = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker = tokio::spawn(pause_aware_timeout(
            std::time::Duration::from_millis(10),
            Some(Arc::new(Paused)),
            waiting.clone(),
            async { rx.await.unwrap() },
        ));
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(!worker.is_finished());
        tx.send(42).unwrap();
        assert_eq!(worker.await.unwrap(), Ok(42));
        let worker = tokio::spawn(pause_aware_timeout(
            std::time::Duration::from_millis(10),
            Some(Arc::new(Paused)),
            waiting,
            std::future::pending::<()>(),
        ));
        worker.abort();
        assert!(worker.await.unwrap_err().is_cancelled());
    }
}
