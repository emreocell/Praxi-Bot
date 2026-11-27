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
mod auto_continue;
pub mod bot_limits;
pub mod browser_profile;
mod browser_url;
mod cli;
mod compaction;
mod config;
mod control;
pub mod control_note;
pub mod decider;
mod delegation;
mod discovery;
mod discovery_prompt;
mod elicitation;
mod embedding;
mod exec;
mod full_autonomy;
mod grounding;
mod history;
mod knowledge;
mod ledger;
mod loops;
mod mail_prompt;
mod memory;
mod mention;
mod mission_start;
mod money;
pub mod page_guard;
mod profile;
mod router;
mod schedule_precheck;
mod scoped_grant;
mod shared;
mod shell_allow;
mod site_profiles;
pub mod spend;
mod subagent;
mod taint;
mod tiering;
mod tool_guidance;
pub mod usage;

#[cfg(test)]
mod account_register_tests;
#[cfg(test)]
mod approval_expiry_tests;
#[cfg(test)]
mod auto_continue_tests;
#[cfg(test)]
mod automation_author_tests;
#[cfg(test)]
mod bot_limit_tests;
#[cfg(test)]
mod browser_mcp_tests;
#[cfg(test)]
mod cancel_supervisor;
#[cfg(test)]
mod control_grant_tests;
#[cfg(test)]
mod delegation_tests;
#[cfg(test)]
mod egress_tests;
#[cfg(test)]
mod full_lane_tests;
#[cfg(test)]
mod full_mode_tests;
#[cfg(test)]
mod mail_tests;
#[cfg(test)]
mod mission_session_tests;
#[cfg(test)]
mod mission_start_tests;
#[cfg(test)]
mod mode_tests;
#[cfg(test)]
mod notes_tests;
#[cfg(test)]
mod pc_scope_tests;
#[cfg(test)]
mod router_tests;
#[cfg(test)]
mod scoped_tests;
#[cfg(test)]
mod server_computer_tests;
#[cfg(test)]
mod site_profile_tests;
#[cfg(test)]
mod spend_tests;
#[cfg(test)]
mod stop_approval_tests;
#[cfg(test)]
mod subagent_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod unattended_gate_tests;

pub use agent::{AgentLoop, SessionStats};
pub use computer_target::{
    ComputerLabel, ComputerNode, ComputerProvider, ComputerTarget, Resolution, RuntimeConfig,
    WAIT_FOR_PC_REASON, resolve as resolve_computer_target,
};
pub use config::{
    DEFAULT_APPROVAL_TIMEOUT, DEFAULT_DIRECT_TOOL_LIMIT, DEFAULT_HANDOVER_TIMEOUT,
    DEFAULT_MAX_AUTO_RESUMES, DEFAULT_MAX_HANDOVERS_PER_RUN, DEFAULT_MAX_SECRET_REQUESTS_PER_RUN,
    DEFAULT_SCHEDULE_APPROVAL_TIMEOUT, DEFAULT_SECRET_REQUEST_TIMEOUT,
    DEFAULT_TRANSIENT_OUTAGE_BUDGET, DEFAULT_WAIT_FOR_PC_TIMEOUT, FullLaneSwitch, Guards,
    JournalSlot, LoopConfig, MemoryConfig, ModelPrice, RedactorRefreshFn, SharedRedactor,
    full_access_lane_switch, secret_variants,
};
pub use control::{ControlGate, ControlRequest};
pub use elicitation::ElicitationAnswerError;
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
    ApprovalScope, CategoryDecision, Delivery, EffectivePolicy, ExternalSource, Grant, GrantTarget,
    HandoverInfo, LoopError, MessageMode, MissionTarget, Providers, QueuedMessage, ScheduleWork,
    SecretRequestInfo, SkillHintFn, SkillHints, StreamItem, WorkspaceWarningEvent, build_providers,
    embedding_key_resolver, initiator_of, occurrence_kind, schedule_id_of,
};
pub use site_profiles::ApprovalSubject;
pub use subagent::MAX_DEPTH as MAX_SUBAGENT_DEPTH;
pub use usage::CallPurpose;

/// Audit-log action recording an "always allow" grant rule for a bot.
pub(crate) const AUDIT_GRANT_ALWAYS: &str = "grant.always";
/// Audit-log action recording the removal of such a rule.
pub(crate) const AUDIT_GRANT_REVOKED: &str = "grant.revoked";
/// Audit-log action recording that a legacy or orphaned computer grant is inert.
pub(crate) const AUDIT_GRANT_INERT: &str = "grant.inert";
