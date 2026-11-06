//! Permission resolution, human-approval gating, and audit trail.
//!
//! Evaluates policy rules for every tool call, suspends execution pending
//! user approval when required, and writes a durable audit log entry.

pub mod error;
pub mod never_list;
pub mod policy;
pub mod resolver;

// Re-export shared types from the protocol crate.
pub use protocol::{InitiatorKind, PermissionCategory};

pub use error::PolicyError;
pub use never_list::NeverList;
pub use policy::{Level, Limits, Policy, Rule};
pub use resolver::{ActionRequest, Decision, PermissionResolver};
