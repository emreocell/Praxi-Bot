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
    /// While a pause, a human takeover or a local override is in force:
    /// `Some(message)`. A permit cached by a running script is re-checked
    /// against this on every use; it never outlives the latch.
    fn latch_refusal(&self) -> Option<String> {
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
    /// Shared by every clone of this permit (a script's later calls inherit
    /// it): cleared when a fresh observation was really made.
    reobserve: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// The observation epoch this admission (or its last re-validation) saw.
    epoch: std::sync::Arc<std::sync::atomic::AtomicU64>,
}
#[derive(Default)]
struct PermitInner {
    completion: std::sync::Mutex<Option<Box<dyn FnOnce() + Send + Sync>>>,
}
impl ActionPermit {
    #[must_use]
    pub fn new(reobserve: bool, completion: impl FnOnce() + Send + Sync + 'static) -> Self {
        Self {
            epoch: std::sync::Arc::default(),
            reobserve: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(reobserve)),
            inner: std::sync::Arc::new(PermitInner {
                completion: std::sync::Mutex::new(Some(Box::new(completion))),
            }),
        }
    }
    #[must_use]
    pub fn needs_observation(&self) -> bool {
        self.reobserve.load(std::sync::atomic::Ordering::SeqCst)
    }
    /// A fresh observation was made for this admission: later actions under
    /// the same permit (the rest of a script) are ordinary again.
    pub fn observed(&self) {
        self.reobserve
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }
    /// The observation epoch this permit was last validated against.
    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.epoch.load(std::sync::atomic::Ordering::SeqCst)
    }
    /// Record the epoch this permit was validated against.
    pub fn set_epoch(&self, epoch: u64) {
        self.epoch.store(epoch, std::sync::atomic::Ordering::SeqCst);
    }
    /// The world changed since this permit was admitted: the next action must
    /// start from a fresh observation.
    pub fn require_observation(&self) {
        self.reobserve
            .store(true, std::sync::atomic::Ordering::SeqCst);
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
///
/// A cached permit is never trusted blindly: on every reuse it is re-validated
/// against the gate, an O(1) read of the hub's shared state. While a pause, a
/// human takeover or a local override is in force the reuse fails closed with
/// the gate's message (it must not wait: the held permit would block the
/// takeover's drain); if the observation epoch moved since the permit was last
/// validated, the next action is made to start from a fresh observation.
pub(crate) async fn admit_or_inherit<F, Fut>(
    gate: &dyn DesktopControlGate,
    admit: F,
) -> Result<ActionPermit, String>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<ActionPermit, String>>,
{
    let Ok(cell) = SCRIPT_ACTION.try_with(Clone::clone) else {
        return admit().await;
    };
    if let Some(permit) = cell.get() {
        if let Some(message) = gate.latch_refusal() {
            return Err(message);
        }
        let now = gate.observation_epoch();
        if permit.epoch() != now {
            permit.require_observation();
            permit.set_epoch(now);
        }
        return Ok(permit.clone());
    }
    // Read before admitting: an epoch change during the admission only makes
    // the permit re-observe once more (the safe direction).
    let epoch = gate.observation_epoch();
    cell.get_or_try_init(|| async {
        let permit = admit().await?;
        permit.set_epoch(epoch);
        Ok(permit)
    })
    .await
    .cloned()
}
pub(crate) async fn scope_action<F: std::future::Future>(
    action: ScriptAction,
    future: F,
) -> F::Output {
    SCRIPT_ACTION.scope(action, future).await
}
