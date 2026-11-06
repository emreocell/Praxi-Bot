//! Error type for desktop operations.

use thiserror::Error;

/// Errors from OS desktop operations.
#[derive(Debug, Error)]
pub enum DesktopError {
    /// The target element could not be found (stale index, different generation).
    #[error("element not found: index={index} generation={generation}")]
    ElementNotFound { index: u32, generation: u64 },

    /// The accessibility API returned an unexpected response.
    #[error("accessibility error: {0}")]
    Accessibility(String),

    /// A screenshot could not be captured.
    #[error("screenshot error: {0}")]
    Screenshot(String),

    /// The requested operation is not supported on this platform.
    #[error("unsupported on this platform: {0}")]
    Unsupported(String),

    /// An unrecoverable internal error.
    #[error("internal error: {0}")]
    Internal(String),
}
