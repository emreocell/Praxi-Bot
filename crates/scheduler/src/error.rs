//! Error types for the `scheduler` crate.

use thiserror::Error;

/// Errors returned by schedule parsing, persistence, execution, and webhook triggers.
#[derive(Debug, Error)]
pub enum SchedulerError {
    /// SQLite error.
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// Underlying `storage` crate error.
    #[error("storage: {0}")]
    Storage(#[from] storage::StorageError),

    /// Filesystem I/O error.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    /// JSON serialisation / deserialisation error.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    /// Invalid schedule specification (bad cron expression, unknown timezone, zero interval).
    #[error("invalid schedule spec: {0}")]
    InvalidSpec(String),

    /// Requested schedule or trigger was not found.
    #[error("not found: {0}")]
    NotFound(String),

    /// Schedule is currently disabled.
    #[error("schedule {0} is disabled")]
    Disabled(String),

    /// Schedule already has a run in flight (per-schedule concurrency limit is 1).
    #[error("schedule {0} is already running")]
    Busy(String),

    /// Webhook HMAC-SHA256 signature verification failed.
    #[error("invalid webhook HMAC-SHA256 signature")]
    InvalidSignature,

    /// Database schema version is newer than this build supports.
    #[error("database schema version {found} is newer than supported version {supported}")]
    SchemaTooNew {
        /// Version found in `PRAGMA user_version`.
        found: u32,
        /// Highest version supported by this binary.
        supported: u32,
    },

    /// Schedule executor reported a failure.
    #[error("execution error: {0}")]
    Execution(String),
}

/// Result alias for `scheduler` operations.
pub type Result<T> = std::result::Result<T, SchedulerError>;
