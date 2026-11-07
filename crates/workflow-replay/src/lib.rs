//! Workflow record and deterministic replay.
//!
//! A successful agent run becomes a [`Workflow`]: a parameterised list of
//! semantic steps. [`Replayer`] runs it later with zero model calls while the
//! screen still matches the recording, and returns a [`Handover`] the moment it
//! does not (renamed or ambiguous element, failed action, denied permission,
//! an explicit `agent` step, a failed success check).
//!
//! * [`format`]: the TOML/JSON workflow format and parameter binding.
//! * [`locator`]: multi-strategy element resolution plus fingerprint check.
//! * [`recorder`]: persisted run steps to a reviewable workflow proposal.
//! * [`replay`]: the permission-gated replayer, events and metrics.
//! * [`store`]: `workflows/*.toml` with an index.
//! * [`tool`]: the `workflow_run` / `workflow_list` agent tools.
//!
//! The ideas (semantic steps, multi-strategy locators, `{var}` variables,
//! fallback to the agent) are common to record-and-replay tools; the code is
//! original.

#![allow(clippy::too_many_lines)]
#![cfg_attr(
    test,
    allow(
        clippy::needless_pass_by_value,
        clippy::assert_is_empty,
        clippy::unnecessary_wraps
    )
)]

pub mod error;
pub mod format;
pub mod locator;
pub mod recorder;
pub mod replay;
pub mod store;
pub mod template;
pub mod tool;

#[cfg(test)]
mod testkit;
#[cfg(test)]
mod tests;

pub use error::{RecordError, ReplayError, StoreError, WorkflowError};
pub use format::{
    AgentStep, BrowserAction, BrowserActionKind, BrowserLocator, DesktopAction, DesktopActionKind,
    DesktopLocator, FORMAT_VERSION, Launch, Navigate, Param, ParamType, PositionHint, Shell, Step,
    SuccessCheck, ToolStep, WaitFor, Workflow,
};
pub use recorder::{DroppedStep, RecordOptions, Recorder, Recording};
pub use replay::{
    EventSink, Handles, Handover, HandoverReason, PartialState, ReplayOptions, ReplayOutcome,
    ReplayReport, Replayer, StepReport,
};
pub use store::{IndexEntry, WorkflowStore};
pub use tool::{LIST_TOOL_NAME, RUN_TOOL_NAME, summarize, tools};
