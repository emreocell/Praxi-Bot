//! Error type and its mapping to [`tool_registry::ToolError`].

use thiserror::Error;
use tool_registry::ToolError;

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
}

impl From<EmailError> for ToolError {
    fn from(e: EmailError) -> Self {
        match e {
            EmailError::Input(m) | EmailError::Config(m) => ToolError::InvalidInput(m),
            other => ToolError::Execution(other.to_string()),
        }
    }
}
