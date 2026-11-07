//! SQLite and NDJSON event-log storage layer for messages, audit, and stream replay.
//!
//! [`Storage`] is a synchronous repository over SQLite (WAL, forward-only
//! migrations); wrap calls in `spawn_blocking` where blocking matters.  Per-run
//! NDJSON event logs live in [`event_log`].  `audit_log` is append-only both in
//! the API (no update/delete) and via schema triggers.

pub mod db;
pub mod error;
pub mod event_log;
pub mod migrate;
pub mod paths;
pub mod records;

pub use db::{Storage, now_ms};
pub use error::{Result, StorageError};
pub use event_log::{EventLogWriter, event_log_path, read_event_log};
pub use paths::{DATA_DIR_ENV, data_dir, resolve_data_dir};
pub use records::{Approval, AuditEntry, Bot, Chat, EventIndexRow, Message, Run, RunStep, User};
