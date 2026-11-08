//! Error type shared by all web primitives.

use crate::policy::PolicyViolation;

/// Everything that can go wrong in this crate.
#[derive(Debug, Clone, thiserror::Error)]
pub enum WebError {
    /// The URL policy refused the request.
    #[error(transparent)]
    Policy(#[from] PolicyViolation),
    /// The URL could not be parsed.
    #[error("invalid URL: {0}")]
    InvalidUrl(String),
    /// Transport-level failure.
    #[error("http error: {0}")]
    Http(String),
    /// The server answered with an error status.
    #[error("server returned HTTP {0}")]
    Status(u16),
    /// The request timed out.
    #[error("request timed out")]
    Timeout,
    /// The response media type is not accepted for this operation.
    #[error("unsupported content type `{0}`")]
    ContentType(String),
    /// Redirect limit exceeded.
    #[error("too many redirects (limit {0})")]
    TooManyRedirects(usize),
    /// Response could not be parsed.
    #[error("parse error: {0}")]
    Parse(String),
    /// Invalid or missing configuration (including secrets).
    #[error("configuration error: {0}")]
    Config(String),
    /// A search backend failed.
    #[error("search failed: {0}")]
    Search(String),
}
