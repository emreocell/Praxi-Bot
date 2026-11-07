//! Errors raised while importing a spec or building tools.

use thiserror::Error;

/// Import / configuration failures. Call-time failures use
/// [`tool_registry::ToolError`].
#[derive(Debug, Error)]
pub enum OpenApiError {
    /// The spec text is larger than [`crate::ImportOptions::max_spec_bytes`].
    #[error("spec is {size} bytes, limit is {limit}")]
    SpecTooLarge {
        /// Actual size.
        size: usize,
        /// Configured limit.
        limit: usize,
    },
    /// The text is neither valid JSON nor valid YAML.
    #[error("spec is not valid JSON or YAML: {0}")]
    Parse(String),
    /// The document is not an `OpenAPI` 3.0.x / 3.1.x spec.
    #[error("unsupported or missing `openapi` version: {0}")]
    UnsupportedVersion(String),
    /// No usable base URL (`servers` missing or relative and no override).
    #[error("no usable server URL: {0}")]
    NoServer(String),
    /// More operations matched than allowed.
    #[error(
        "{count} operations selected, limit is {limit}; narrow the import with tag or operationId filters"
    )]
    TooManyOperations {
        /// Operations that passed the filters.
        count: usize,
        /// Configured limit.
        limit: usize,
    },
    /// The server host is a private / loopback address and that is not allowed.
    #[error("host `{0}` is a private or loopback address; set allow_private_hosts to permit it")]
    PrivateHost(String),
    /// Invalid configuration (e.g. a malformed secret name).
    #[error("invalid configuration: {0}")]
    Config(String),
}
