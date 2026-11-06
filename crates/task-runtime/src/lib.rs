//! Scheduled task execution and background job management for the core daemon.
//!
//! Durable runs: every step is persisted before (intent) and after (result)
//! execution, so a crash leaves an auditable trail.  [`Runtime::recover`]
//! pauses runs that were `Running` and marks in-flight steps `Interrupted`;
//! resuming is always explicit and never replays a side-effecting step.

mod runtime;
pub mod state;
pub mod step;

pub use runtime::{Runtime, RuntimeError};
pub use state::{RunState, StepStatus, can_transition};
pub use step::{ExecError, RetryPolicy, StepExecutor, StepSpec};
