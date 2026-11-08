//! Error types for the provider catalog and chat provider trait.

use std::time::Duration;

use thiserror::Error;

/// Errors from catalog loading or provider operations.
#[derive(Debug, Error)]
pub enum ProviderError {
    /// CLI engines run whole turns instead of implementing `ChatProvider`.
    #[error("cli engines are sessions")]
    CliSession,
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

    /// The model's output was blocked by a safety / content filter
    /// (Gemini `SAFETY`, `RECITATION`, ..., `OpenAI` `content_filter`).
    #[error("blocked by content filter: {0}")]
    ContentFilter(String),

    /// The model produced a malformed tool call (Gemini
    /// `MALFORMED_FUNCTION_CALL`). Retrying the request once usually succeeds.
    #[error("malformed tool call: {0}")]
    MalformedToolCall(String),

    /// The request timed out.
    #[error("request timed out")]
    Timeout,

    /// The request cannot be expressed for this provider (for example a tool
    /// result whose call id is unknown, or an image URL the provider cannot
    /// accept). A missing capability is [`Self::Unsupported`], not this.
    #[error("invalid request: {0}")]
    InvalidRequest(String),

    /// The model cannot perform a requested capability (`tools`, `vision`,
    /// ...). The same model is not retried; the fallback chain tries the next
    /// model. A generic [`Self::InvalidRequest`] or an unmatched HTTP 400 stays
    /// a hard stop.
    #[error("model does not support {capability}")]
    Unsupported { capability: String },

    /// The connection to the provider failed.
    #[error("connection error: {0}")]
    Connection(String),

    /// A streaming response could not be decoded.
    #[error("stream error: {0}")]
    Stream(String),
}
