//! Error type.

use thiserror::Error;

/// Errors from secret stores.
#[derive(Debug, Error)]
pub enum SecretError {
    /// The secret name is empty or contains forbidden characters.
    #[error("invalid secret name {0:?} (use letters, digits, '.', '_', '-', ':', '/')")]
    InvalidName(String),
    /// A referenced secret does not exist.
    #[error("secret {0:?} not found")]
    NotFound(String),
    /// The OS keychain reported an error.
    #[error("keychain error: {0}")]
    Keyring(String),
    /// The OS keychain is not usable here (headless server, unsupported OS).
    #[error("OS keychain unavailable: {0}")]
    Unavailable(String),
    /// The passphrase did not decrypt the secrets file.
    #[error("wrong passphrase (or the secrets file is corrupt)")]
    WrongPassphrase,
    /// No passphrase could be obtained for the encrypted file.
    #[error("no passphrase: set BOT_SECRETS_PASSPHRASE or run interactively")]
    NoPassphrase,
    /// The secrets file has an unknown format.
    #[error("corrupt secrets file: {0}")]
    Corrupt(String),
    /// Filesystem error.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
