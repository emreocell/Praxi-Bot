//! Error types for the permission engine.

use thiserror::Error;

/// Errors returned by policy construction or resolution.
#[derive(Debug, Error)]
pub enum PolicyError {
    /// A glob pattern in the never-list or a limit field was invalid.
    #[error("invalid pattern: {0}")]
    InvalidPattern(String),
}
