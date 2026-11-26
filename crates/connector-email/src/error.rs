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
    #[error("the provider rejected the request{}{}", .code.as_deref().map_or(String::new(), |c| format!(" ({c})")), .detail.as_deref().map_or(String::new(), |d| format!(": {d}")))]
    Rejected {
        /// Sanitised machine code, `[a-z0-9_]{1,64}`.
        code: Option<String>,
        /// For a `400`/`422` validation answer: the provider's error `name` and
        /// the first `errors[]` entries (`path: message`), sanitised and capped,
        /// so the model and the owner can see what was refused.
        detail: Option<String>,
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

impl EmailError {
    /// One plain sentence on what to do about this failure, for the model and the
    /// owner; `None` when the message already says it. Never contains server text.
    #[must_use]
    pub fn fix_hint(&self) -> Option<&'static str> {
        Some(match self {
            Self::Credential(_) => {
                "no password or API key is stored for this account; the owner must enter it again in Settings > E-mail"
            }
            Self::Tls(_) => {
                "the secure connection failed; the owner should check the server name and port (IMAP is usually 993) in Settings > E-mail"
            }
            Self::Timeout(_) => {
                "the mail server did not answer in time; try again once, then tell the owner"
            }
            Self::Imap(m) if m.contains("login failed") => {
                "the server refused the login: the password may be wrong or expired; the owner can run \"Test connection\" in Settings > E-mail"
            }
            Self::Imap(m) if m.starts_with("examine") || m.starts_with("select") => {
                "that folder could not be opened; call email_list_folders and use a name from it"
            }
            Self::Imap(m) if m.starts_with("connect") || m.contains("greeting") => {
                "the mail server could not be reached; the owner should check the host, the port and the network of this computer"
            }
            Self::Provider(ProviderError::Auth | ProviderError::Forbidden) => {
                "the mail provider refused the API key; the owner must replace it in Settings > E-mail"
            }
            Self::Provider(ProviderError::Quota) => {
                "the provider plan limit was reached; the owner must free space or upgrade the plan"
            }
            _ => return None,
        })
    }
}

impl From<EmailError> for ToolError {
    fn from(e: EmailError) -> Self {
        match e {
            EmailError::Input(m) | EmailError::Config(m) => ToolError::InvalidInput(m),
            other => match other.fix_hint() {
                Some(hint) => ToolError::Execution(format!("{other}. Fix: {hint}")),
                None => ToolError::Execution(other.to_string()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ProviderError;

    /// The desktop app turns these exact sentences into plain words (`emailProviderKind` in the
    /// UI): changing one here without changing it there makes a refusal look like "host silent".
    #[test]
    fn provider_error_sentences_are_pinned_for_the_desktop() {
        assert_eq!(
            ProviderError::Auth.to_string(),
            "the provider rejected the API key"
        );
        assert_eq!(
            ProviderError::Forbidden.to_string(),
            "the API key is not allowed to do this"
        );
        assert_eq!(
            ProviderError::Quota.to_string(),
            "a limit of the provider plan was reached"
        );
        assert_eq!(
            ProviderError::RateLimited {
                retry_after_secs: Some(7)
            }
            .to_string(),
            "rate limited by the provider, retry after 7s"
        );
        assert_eq!(
            ProviderError::Transient.to_string(),
            "the provider is temporarily unavailable"
        );
        assert_eq!(
            ProviderError::Rejected {
                code: Some("already_exists".into()),
                detail: None,
            }
            .to_string(),
            "the provider rejected the request (already_exists)"
        );
    }
}
