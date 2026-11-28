//! Liveness of the daemon: its background loops and the process itself.
//!
//! * [`Guard`] supervises the loops that must never stop quietly (the scheduler, the mission
//!   wake loop, the mission drivers). A loop that panics is restarted after a backoff; after too
//!   many crashes in a row the guard gives up, marks the loop *degraded* and says so (health,
//!   doctor, an owner notice). A single bad pass is caught where it happens
//!   ([`Guard::guarded`]) so the loop does not even need a restart.
//! * [`notify`] speaks the systemd notify protocol, so a daemon that hangs is restarted by
//!   `WatchdogSec`.

mod guard;
pub mod notify;

pub use guard::{
    EventKind, Fault, Guard, LoopEvent, LoopState, LoopStatus, Notifier, Policy, Verdict,
};
