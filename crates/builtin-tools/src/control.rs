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
    /// Same security barrier without a local overlay highlight.
    async fn admit_background_action(&self, req: &ControlRequest) -> Result<ActionPermit, String> {
        self.admit_action(req).await
    }
    fn observation_epoch(&self) -> u64 {
        0
    }
    /// True while a bot run that used the computer is still active; keeps the
    /// managed browser from idle-closing between that run's steps.
    fn computer_in_use(&self) -> bool {
        false
    }
    /// While the human drives, observation tools (snapshots, screenshots, OCR,
    /// tab and window listings) are refused too: the model must not read what
    /// the user types. `Some(message)` is returned to the model.
    fn observation_refused(&self) -> Option<String> {
        None
    }
    /// Lifetime admission: held through action, settling and observation, including failures.
    async fn admit_action(&self, req: &ControlRequest) -> Result<ActionPermit, String> {
        self.before_action(req).await?;
        Ok(ActionPermit::default())
    }

    /// Called before the action runs; may wait (the overlay highlight).
    ///
    /// # Errors
    /// A message for the model explaining why the action was refused.
    async fn before_action(&self, req: &ControlRequest) -> Result<(), String>;
}

/// Completion is reference counted so dispatched OS/CDP work retains admission
/// even if its awaiting tool future is canceled.
#[derive(Clone, Default)]
pub struct ActionPermit {
    inner: std::sync::Arc<PermitInner>,
    reobserve: bool,
}
#[derive(Default)]
struct PermitInner {
    completion: std::sync::Mutex<Option<Box<dyn FnOnce() + Send + Sync>>>,
}
impl ActionPermit {
    #[must_use]
    pub fn new(reobserve: bool, completion: impl FnOnce() + Send + Sync + 'static) -> Self {
        Self {
            reobserve,
            inner: std::sync::Arc::new(PermitInner {
                completion: std::sync::Mutex::new(Some(Box::new(completion))),
            }),
        }
    }
    #[must_use]
    pub fn needs_observation(&self) -> bool {
        self.reobserve
    }
    /// Once dispatched, actual backend completion owns a clone of this permit.
    /// # Errors
    /// Returns a fixed error if the backend task panics or the runtime shuts down.
    pub async fn protect<F>(&self, future: F) -> Result<F::Output, String>
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let hold = std::sync::Arc::clone(&self.inner);
        tokio::spawn(async move {
            let _hold = hold;
            future.await
        })
        .await
        .map_err(|_| "computer action worker failed".into())
    }
}
impl Drop for PermitInner {
    fn drop(&mut self) {
        if let Some(f) = self.completion.get_mut().expect("permit poisoned").take() {
            f();
        }
    }
}

/// The admission of one script run, taken lazily by its first computer call.
pub(crate) type ScriptAction = std::sync::Arc<tokio::sync::OnceCell<ActionPermit>>;

// A script that uses the computer is one admitted action: the first computer
// tool call admits, later ones inherit the permit, so takeover drains the
// running script rather than deadlocking its next call. A script that never
// touches the computer is never admitted (and takes no computer slot).
tokio::task_local! { static SCRIPT_ACTION: ScriptAction; }

/// Admit with `admit`, or inherit the enclosing script's admission (taking it
/// with `admit` the first time).
pub(crate) async fn admit_or_inherit<F, Fut>(admit: F) -> Result<ActionPermit, String>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<ActionPermit, String>>,
{
    match SCRIPT_ACTION.try_with(Clone::clone) {
        Ok(cell) => cell.get_or_try_init(admit).await.cloned(),
        Err(_) => admit().await,
    }
}
pub(crate) async fn scope_action<F: std::future::Future>(
    action: ScriptAction,
    future: F,
) -> F::Output {
    SCRIPT_ACTION.scope(action, future).await
}
