//! Error type.

/// Errors from the memory store.
#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    /// SQLite failure.
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// Filesystem failure.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// JSON failure.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// Storage (bot.db) failure while reading messages.
    #[error("storage: {0}")]
    Storage(#[from] storage::StorageError),
    /// Embedder failure.
    #[error("embedder: {0}")]
    Embed(String),
    /// LLM abstraction failure.
    #[error("model: {0}")]
    Model(String),
    /// Memory or journal entry missing (or owned by another user).
    #[error("not found: {0}")]
    NotFound(String),
    /// The input violates a store rule (e.g. contains a secret).
    #[error("invalid: {0}")]
    Invalid(String),
    /// Database was created with another embedding dimension.
    #[error("embedding dimension mismatch: database {db}, embedder {embedder}")]
    DimensionMismatch {
        /// Dimension recorded in the database.
        db: usize,
        /// Dimension of the supplied embedder.
        embedder: usize,
    },
    /// Database schema is newer than this build.
    #[error("memory schema version {found} is newer than supported {supported}")]
    SchemaTooNew {
        /// Found version.
        found: u32,
        /// Supported version.
        supported: u32,
    },
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, MemoryError>;
