//! Error types for the Telegram channel crate.

use secret_store::SecretError;
use thiserror::Error;

/// Convenience result alias for [`TelegramError`].
pub type Result<T> = std::result::Result<T, TelegramError>;

/// Errors produced by the Telegram Bot API client, pairing store, or daemon bridge.
#[derive(Debug, Error)]
pub enum TelegramError {
    /// Network or HTTP transport failure talking to Telegram or the daemon.
    #[error("http error: {0}")]
    Http(String),

    /// Telegram Bot API returned `ok: false`.
    #[error("telegram api error ({error_code:?}): {description}")]
    TelegramApi {
        /// Telegram numerical error code (e.g. `400`, `401`, `429`).
        error_code: Option<i64>,
        /// Human-readable description returned by Telegram.
        description: String,
    },

    /// Telegram answered `429 Too Many Requests`.
    #[error("telegram rate limited, retry after {retry_after_secs}s")]
    RateLimited {
        /// Seconds Telegram asked us to wait.
        retry_after_secs: u64,
    },

    /// Daemon API error or unexpected response.
    #[error("daemon api error: {0}")]
    Daemon(String),

    /// Secret store error (e.g. missing bot token).
    #[error("secret error: {0}")]
    Secret(#[from] SecretError),

    /// Persistent pairing/state file I/O or serialisation error.
    #[error("storage error: {0}")]
    Storage(String),

    /// Invalid configuration or input.
    #[error("config error: {0}")]
    Config(String),
}
