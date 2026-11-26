//! Errors of the mission store and supervisor.

use crate::model::MissionState;

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, MissionError>;

/// What can go wrong.
#[derive(Debug, thiserror::Error)]
pub enum MissionError {
    /// SQLite failure.
    #[error("mission store: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// JSON column could not be read or written.
    #[error("mission store: {0}")]
    Json(#[from] serde_json::Error),
    /// Filesystem failure opening the database.
    #[error("mission store: {0}")]
    Io(#[from] std::io::Error),
    /// No such mission or item.
    #[error("not found: {0}")]
    NotFound(String),
    /// The input is not acceptable (a message safe to show to the model and the owner).
    #[error("{0}")]
    Invalid(String),
    /// The state machine does not allow this move.
    #[error("a {from} mission cannot become {to}")]
    IllegalTransition {
        /// Current state.
        from: MissionState,
        /// Requested state.
        to: MissionState,
    },
    /// Another supervisor holds the mission (a session is in flight, or it is not `running`).
    #[error("mission {0} is not free for a new session")]
    Busy(String),
    /// The database was written by a newer build.
    #[error("mission database schema {found} is newer than this build supports ({supported})")]
    SchemaTooNew {
        /// Version in the file.
        found: u32,
        /// Highest version this build knows.
        supported: u32,
    },
}
