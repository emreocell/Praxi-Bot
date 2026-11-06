//! Error types for tool execution and registration.

use thiserror::Error;

/// Errors that can occur when calling a tool.
#[derive(Debug, Error)]
pub enum ToolError {
    /// The tool's input did not match its declared schema.
    #[error("invalid input: {0}")]
    InvalidInput(String),

    /// The tool call was denied by the permission engine.
    #[error("permission denied: {0}")]
    PermissionDenied(String),

    /// A recoverable error occurred during execution; the agent may retry.
    #[error("execution error: {0}")]
    Execution(String),

    /// An unrecoverable internal error.
    #[error("internal error: {0}")]
    Internal(#[from] Box<dyn std::error::Error + Send + Sync>),
}
