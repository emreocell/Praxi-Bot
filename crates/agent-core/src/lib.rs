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
pub mod attachments;
pub mod bot_limits;
mod browser_url;
mod cli;
mod compaction;
mod config;
mod control;
pub mod control_note;
pub mod decider;
mod discovery_prompt;
mod exec;
mod history;
mod knowledge;
mod ledger;
mod memory;
mod mention;
mod profile;
mod router;
mod schedule_precheck;
mod scoped_grant;
mod shared;
pub mod spend;
mod subagent;
mod taint;
mod tiering;
mod tool_guidance;
pub mod usage;

#[cfg(test)]
mod approval_expiry_tests;
#[cfg(test)]
mod bot_limit_tests;
#[cfg(test)]
mod browser_mcp_tests;
#[cfg(test)]
mod control_grant_tests;
#[cfg(test)]
mod egress_tests;
#[cfg(test)]
mod mode_tests;
#[cfg(test)]
mod pc_scope_tests;
#[cfg(test)]
mod router_tests;
#[cfg(test)]
mod scoped_tests;
#[cfg(test)]
mod server_computer_tests;
#[cfg(test)]
mod spend_tests;
#[cfg(test)]
mod subagent_tests;
#[cfg(test)]
mod tests;

pub use agent::AgentLoop;
pub use computer_target::{
    ComputerLabel, ComputerNode, ComputerProvider, ComputerTarget, Resolution, RuntimeConfig,
    WAIT_FOR_PC_REASON, resolve as resolve_computer_target,
};
pub use config::{
    DEFAULT_APPROVAL_TIMEOUT, DEFAULT_DIRECT_TOOL_LIMIT, DEFAULT_MAX_SECRET_REQUESTS_PER_RUN,
    DEFAULT_SCHEDULE_APPROVAL_TIMEOUT, DEFAULT_SECRET_REQUEST_TIMEOUT, DEFAULT_WAIT_FOR_PC_TIMEOUT,
    Guards, JournalSlot, LoopConfig, MemoryConfig, ModelPrice, RedactorRefreshFn, SharedRedactor,
    secret_variants,
};
pub use control::{ControlGate, ControlRequest};
pub use knowledge::{
    HeuristicKnowledgeDecider, KnowledgeDecider, knowledge_auto_retrieval_available,
};
pub use permission_engine::Decision as PolicyDecision;
pub use profile::{
    BotProfile, EngineConsent, EngineSettings, FileAccessMode, FilesConfig, ModelsConfig,
    PermissionsConfig, ProfileError, RouterSettings, ToolFilter,
};
pub use scoped_grant::{Scope, ScopeKind};
pub use shared::{
    ApprovalScope, CategoryDecision, Delivery, EffectivePolicy, Grant, GrantTarget, LoopError,
    MessageMode, Providers, ScheduleWork, SecretRequestInfo, SkillHintFn, SkillHints, StreamItem,
    build_providers, initiator_of, occurrence_kind, schedule_id_of,
};
pub use subagent::MAX_DEPTH as MAX_SUBAGENT_DEPTH;
pub use usage::CallPurpose;

/// Audit-log action recording an "always allow" grant rule for a bot.
pub(crate) const AUDIT_GRANT_ALWAYS: &str = "grant.always";
/// Audit-log action recording the removal of such a rule.
pub(crate) const AUDIT_GRANT_REVOKED: &str = "grant.revoked";
/// Audit-log action recording that a legacy or orphaned computer grant is inert.
pub(crate) const AUDIT_GRANT_INERT: &str = "grant.inert";
