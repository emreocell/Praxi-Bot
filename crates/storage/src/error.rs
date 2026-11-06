//! Storage error type.

/// Errors produced by the storage layer.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// SQLite failure.
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// Filesystem failure.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// JSON (de)serialisation failure.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// A required row does not exist.
    #[error("not found: {0}")]
    NotFound(String),
    /// The database was written by a newer schema than this binary knows.
    #[error("database schema version {found} is newer than supported {supported}")]
    SchemaTooNew {
        /// Version found in the database.
        found: u32,
        /// Highest version this build supports.
        supported: u32,
    },
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, StorageError>;
