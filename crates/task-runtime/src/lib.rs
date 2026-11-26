//! Scheduled task execution and background job management for the core daemon.
//!
//! Durable runs: every step is persisted before (intent) and after (result)
//! execution, so a crash leaves an auditable trail.  [`Runtime::recover`]
//! pauses runs that were `Running` and marks in-flight steps `Interrupted`;
//! resuming never replays a side-effecting step.  Recovered runs carry the pause
//! reason [`RESTART_PAUSE_REASON`] so the caller can tell them from other pauses.

mod runtime;
pub mod state;
pub mod step;

pub use runtime::{RESTART_LIMIT_PAUSE_REASON, RESTART_PAUSE_REASON, Runtime, RuntimeError};
pub use state::{RunState, StepStatus, can_transition};
pub use step::{ExecError, RetryPolicy, StepExecutor, StepSpec};
