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
mod approval;
mod cli;
mod config;
mod control;
pub mod control_note;
mod exec;
mod history;
mod memory;
mod profile;
mod router;
mod shared;
mod subagent;

#[cfg(test)]
mod router_tests;
#[cfg(test)]
mod subagent_tests;
#[cfg(test)]
mod tests;

pub use agent::AgentLoop;
pub use config::{
    DEFAULT_DIRECT_TOOL_LIMIT, DEFAULT_SCHEDULE_APPROVAL_TIMEOUT, Guards, JournalSlot, LoopConfig,
    MemoryConfig, ModelPrice, RedactorRefreshFn, SharedRedactor, secret_variants,
};
pub use control::{ControlGate, ControlRequest};
pub use permission_engine::Decision as PolicyDecision;
pub use profile::{
    BotProfile, ModelsConfig, PermissionsConfig, ProfileError, RouterSettings, ToolFilter,
};
pub use shared::{
    ApprovalScope, CategoryDecision, Delivery, EffectivePolicy, Grant, GrantTarget, LoopError,
    MessageMode, Providers, ScheduleWork, SkillHintFn, SkillHints, StreamItem, build_providers,
    schedule_id_of,
};
pub use subagent::MAX_DEPTH as MAX_SUBAGENT_DEPTH;

/// Audit-log action recording an "always allow" grant rule for a bot.
pub(crate) const AUDIT_GRANT_ALWAYS: &str = "grant.always";
/// Audit-log action recording the removal of such a rule.
pub(crate) const AUDIT_GRANT_REVOKED: &str = "grant.revoked";
