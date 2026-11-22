//! Permission resolution, human-approval gating, and audit trail.
//!
//! Evaluates policy rules for every tool call, suspends execution pending
//! user approval when required, and writes a durable audit log entry.

pub mod egress;
pub mod error;
pub mod host_info;
pub mod modes;
pub mod never_list;
pub mod policy;
pub mod resolver;

// Re-export shared types from the protocol crate.
pub use protocol::{InitiatorKind, PermissionCategory};

pub use egress::{ContentExposure, EgressReach, egress_must_ask, executes_must_ask};
pub use error::PolicyError;
pub use host_info::is_host_status_command;
pub use modes::{AllowReason, AutonomyMode, CallFacts, Impact, balanced_lane};
pub use never_list::{CliStores, NeverList, set_data_dir};
pub use policy::{Level, Limits, Policy, Rule};
pub use resolver::{ActionRequest, Decision, PermissionResolver};
