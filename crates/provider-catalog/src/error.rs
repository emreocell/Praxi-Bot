//! Error types for the provider catalog and chat provider trait.

use std::time::Duration;

use thiserror::Error;

/// Errors from catalog loading or provider operations.
#[derive(Debug, Error)]
pub enum ProviderError {
    /// A TOML catalog file could not be parsed.
    #[error("catalog parse error in {path}: {source}")]
    CatalogParse {
        path: String,
        #[source]
        source: toml::de::Error,
    },

    /// No provider matched the selection criteria.
    #[error("no provider available for model ref: {0}")]
    NoProvider(String),

    /// The provider returned a non-retryable error response (other 4xx).
    #[error("provider error ({status}): {message}")]
    Api { status: u16, message: String },

    /// Credentials were missing or rejected (HTTP 401/403, or the API key
    /// environment variable is not set).
    #[error("authentication failed: {0}")]
    Auth(String),

    /// The provider rate-limited the request (HTTP 429).
    #[error("rate limited (retry after {retry_after:?})")]
    RateLimited {
        /// Value of the `Retry-After` header, when present and parseable.
        retry_after: Option<Duration>,
    },

    /// The provider failed on its side (HTTP 5xx).
    #[error("upstream error ({status}): {message}")]
    Upstream { status: u16, message: String },

    /// The request timed out.
    #[error("request timed out")]
    Timeout,

    /// The request cannot be expressed for this provider (for example images
    /// sent to a model without `vision`).
    #[error("invalid request: {0}")]
    InvalidRequest(String),

    /// The connection to the provider failed.
    #[error("connection error: {0}")]
    Connection(String),

    /// A streaming response could not be decoded.
    #[error("stream error: {0}")]
    Stream(String),
}
