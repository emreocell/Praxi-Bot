//! Permission resolution, human-approval gating, and audit trail.
//!
//! Evaluates policy rules for every tool call, suspends execution pending
//! user approval when required, and writes a durable audit log entry.
