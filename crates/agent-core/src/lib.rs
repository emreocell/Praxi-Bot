//! Agent loop, session management, and core orchestration logic.
//!
//! [`AgentLoop::run_turn`] runs one bot turn on top of the durable
//! [`task_runtime::Runtime`]: model -> tool calls -> permission check ->
//! execute -> results back -> repeat.  Every model call and tool batch is a
//! persisted step (intent before, result after), the conversation is rebuilt
//! from those rows, and progress is streamed as [`protocol::Event`]s (also
//! appended to the run's NDJSON log).
//!
//! Reference ideas: goose (loop / tool dispatch), codex-rs (turn loop,
//! approvals), deepseek-harness (spill, guard) - patterns only, no code copied.

mod agent;
mod config;
mod exec;
mod history;
mod profile;
mod shared;
mod todo;

#[cfg(test)]
mod tests;

pub use agent::AgentLoop;
pub use config::{Guards, LoopConfig, ModelPrice};
pub use profile::{BotProfile, ModelsConfig, PermissionsConfig, ProfileError, ToolFilter};
pub use shared::{ApprovalScope, Delivery, LoopError, MessageMode, Providers, StreamItem};

/// Audit-log action recording an "always allow this tool for this bot" grant.
pub(crate) const AUDIT_GRANT_ALWAYS: &str = "grant.always";
