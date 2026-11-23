//! Error types for the provider catalog and chat provider trait.

use std::time::Duration;

use thiserror::Error;

/// What went wrong, independent of which provider or transport supplied it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    Auth,
    Quota,
    RateLimit,
    Overloaded,
    ContextTooLong,
    ContentFiltered,
    Network,
    Timeout,
    Protocol,
    InvalidRequest,
    Unsupported,
    MalformedToolCall,
    Cancelled,
    Configuration,
}

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

    /// Account or subscription quota is exhausted.
    #[error("the model provider's usage quota is exhausted")]
    Quota,

    /// The request exceeds the model's context window.
    #[error("context limit exceeded: this conversation is too long for the model")]
    ContextTooLong,

    /// The provider failed on its side (HTTP 5xx).
    #[error("upstream error ({status}): {message}")]
    Upstream { status: u16, message: String },

    /// A retryable response with an explicit server-requested delay.
    #[error("{source}")]
    RetryAfter {
        #[source]
        source: Box<Self>,
        delay: Duration,
    },

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

    /// The connection closed before the stream's terminal event.
    #[error("the model response ended early: {0}")]
    Interrupted(&'static str),

    /// A streaming response could not be decoded.
    #[error("stream error: {0}")]
    Stream(String),

    /// A whole CLI turn failed. It may have taken actions before failing.
    #[error("{message}")]
    CliTurn {
        class: FailureClass,
        message: String,
    },
}

impl ProviderError {
    /// Classify failures for retry decisions without parsing displayed errors.
    #[must_use]
    pub fn class(&self) -> FailureClass {
        match self {
            Self::Auth(_) => FailureClass::Auth,
            Self::Quota => FailureClass::Quota,
            Self::RateLimited { .. } | Self::Api { status: 429, .. } => FailureClass::RateLimit,
            Self::Upstream { .. }
            | Self::Api {
                status: 500..=599, ..
            } => FailureClass::Overloaded,
            Self::ContextTooLong => FailureClass::ContextTooLong,
            Self::ContentFilter(_) => FailureClass::ContentFiltered,
            Self::Connection(_) | Self::Interrupted(_) => FailureClass::Network,
            Self::Timeout | Self::Api { status: 408, .. } => FailureClass::Timeout,
            Self::Stream(_) => FailureClass::Protocol,
            Self::InvalidRequest(_) | Self::Api { .. } => FailureClass::InvalidRequest,
            Self::Unsupported { .. } => FailureClass::Unsupported,
            Self::MalformedToolCall(_) => FailureClass::MalformedToolCall,
            Self::CliSession | Self::CatalogParse { .. } | Self::NoProvider(_) => {
                FailureClass::Configuration
            }
            Self::RetryAfter { source, .. } => source.class(),
            Self::CliTurn { class, .. } => *class,
        }
    }

    #[must_use]
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::RateLimited { retry_after } => *retry_after,
            Self::RetryAfter { delay, .. } => Some(*delay),
            _ => None,
        }
    }

    /// Whether repeating an attempted request is allowed. A whole CLI turn
    /// cannot be replayed automatically because tools may already have run.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        !matches!(self, Self::CliTurn { .. })
            && matches!(
                self.class(),
                FailureClass::RateLimit
                    | FailureClass::Overloaded
                    | FailureClass::Timeout
                    | FailureClass::Network
                    | FailureClass::MalformedToolCall
            )
    }

    /// A different model can also resolve a capability mismatch.
    #[must_use]
    pub fn can_fallback(&self) -> bool {
        self.is_retryable() || matches!(self, Self::Unsupported { .. })
    }
}
