//! Error type and its mapping to [`tool_registry::ToolError`].

use thiserror::Error;
use tool_registry::ToolError;

/// Coarse, non-leaky failure classes of an API-backed mailbox provider. The
/// provider's own error text is never copied into these (it is server-controlled
/// and may echo request data); only a sanitised machine code is kept.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProviderError {
    /// `401`: the API key is missing, unknown, revoked or expired.
    #[error("the provider rejected the API key")]
    Auth,
    /// `403`: the key is valid but may not do this.
    #[error("the API key is not allowed to do this")]
    Forbidden,
    /// A plan or resource limit was reached (`limit_exceeded`, `409`/`403` with
    /// a limit code).
    #[error("a limit of the provider plan was reached")]
    Quota,
    /// `429`: slow down. `retry_after_secs` is the provider's `Retry-After`.
    #[error("rate limited by the provider{}", .retry_after_secs.map_or(String::new(), |s| format!(", retry after {s}s")))]
    RateLimited {
        /// Seconds to wait, clamped to one hour.
        retry_after_secs: Option<u64>,
    },
    /// `5xx`, network trouble or a timeout: worth trying again later.
    #[error("the provider is temporarily unavailable")]
    Transient,
    /// `4xx` the request itself was wrong (validation, name taken, ...).
    /// `code` is the provider's `snake_case` error code when it was well formed.
    #[error("the provider rejected the request{}", .code.as_deref().map_or(String::new(), |c| format!(" ({c})")))]
    Rejected {
        /// Sanitised machine code, `[a-z0-9_]{1,64}`.
        code: Option<String>,
    },
    /// The response was larger than the cap or not what the API documents.
    #[error("the provider sent an unusable or oversized response")]
    BadResponse,
    /// A redirect was answered; redirects are never followed.
    #[error("the provider answered with a redirect, which is refused")]
    Redirect,
}

/// Errors from the e-mail connector. Messages never contain credentials.
#[derive(Debug, Error)]
pub enum EmailError {
    /// Bad account configuration.
    #[error("config: {0}")]
    Config(String),
    /// Bad caller input (query, address, folder name ...).
    #[error("invalid input: {0}")]
    Input(String),
    /// The credential could not be resolved from the secret store.
    #[error("credential: {0}")]
    Credential(String),
    /// TLS setup or handshake failure.
    #[error("tls: {0}")]
    Tls(String),
    /// IMAP failure.
    #[error("imap: {0}")]
    Imap(String),
    /// SMTP failure.
    #[error("smtp: {0}")]
    Smtp(String),
    /// A network operation timed out.
    #[error("timed out after {0}s")]
    Timeout(u64),
    /// The requested message or folder does not exist.
    #[error("not found: {0}")]
    NotFound(String),
    /// This mailbox (its provider) does not offer the operation.
    #[error("not supported by this mailbox: {0}")]
    Unsupported(String),
    /// An API-backed provider failed, in a coarse class.
    #[error("mailbox provider: {0}")]
    Provider(#[from] ProviderError),
}

impl EmailError {
    /// Shorthand for [`EmailError::Unsupported`].
    #[must_use]
    pub fn unsupported(what: &str) -> Self {
        Self::Unsupported(what.to_owned())
    }
}

impl From<EmailError> for ToolError {
    fn from(e: EmailError) -> Self {
        match e {
            EmailError::Input(m) | EmailError::Config(m) => ToolError::InvalidInput(m),
            other => ToolError::Execution(other.to_string()),
        }
    }
}
