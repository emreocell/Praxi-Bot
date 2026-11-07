//! Error types.

use thiserror::Error;

/// A workflow is malformed or its parameters are wrong.
#[derive(Debug, Error)]
pub enum WorkflowError {
    /// The workflow file could not be parsed or serialised.
    #[error("workflow format: {0}")]
    Format(String),
    /// A semantic validation rule failed.
    #[error("invalid workflow: {0}")]
    Invalid(String),
    /// A `{param}` template is malformed or references an unbound parameter.
    #[error("template: {0}")]
    Template(String),
    /// Supplied parameters do not match the declaration.
    #[error("parameters: {0}")]
    Params(String),
}

/// Errors of the workflow store.
#[derive(Debug, Error)]
pub enum StoreError {
    /// No workflow with this name.
    #[error("workflow not found: {0}")]
    NotFound(String),
    /// The name is not a valid file-safe identifier.
    #[error("invalid workflow name {0:?} (use a-z, 0-9, `_`, `-`; at most 64 characters)")]
    BadName(String),
    /// Disk error.
    #[error("workflow store io: {0}")]
    Io(#[from] std::io::Error),
    /// Stored content is malformed.
    #[error(transparent)]
    Workflow(#[from] WorkflowError),
    /// Index file is malformed.
    #[error("workflow index: {0}")]
    Index(String),
}

/// Errors while recording a run.
#[derive(Debug, Error)]
pub enum RecordError {
    /// The run does not exist.
    #[error("run not found: {0}")]
    RunNotFound(String),
    /// Only completed (succeeded) runs are recorded.
    #[error("run {run} is `{state}`; only succeeded runs can be recorded")]
    NotSucceeded {
        /// Run id.
        run: String,
        /// Its state.
        state: String,
    },
    /// Storage failure.
    #[error("storage: {0}")]
    Storage(String),
    /// The run produced nothing replayable.
    #[error("the run has no replayable steps")]
    Empty,
    /// The resulting workflow failed validation.
    #[error(transparent)]
    Workflow(#[from] WorkflowError),
}

/// A replay could not even start.
#[derive(Debug, Error)]
pub enum ReplayError {
    /// The workflow or its parameters are invalid.
    #[error(transparent)]
    Workflow(#[from] WorkflowError),
}
