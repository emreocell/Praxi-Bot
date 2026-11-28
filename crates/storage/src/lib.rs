//! SQLite and NDJSON event-log storage layer for messages, audit, and stream replay.
//!
//! [`Storage`] is a synchronous repository over SQLite (WAL, forward-only
//! migrations); wrap calls in `spawn_blocking` where blocking matters.  Per-run
//! NDJSON event logs live in [`event_log`].  `audit_log` is append-only both in
//! the API (no update/delete) and via schema triggers.

mod chat_summary;
pub mod db;
pub mod error;
pub mod event_log;
mod incoming_webhooks;
pub mod journal;
pub mod migrate;
pub mod paths;
pub mod records;

pub use chat_summary::{ChatSummary, Cursor, sorts_after};
pub use db::{SearchFilter, SkillAssignmentLookup, Storage, is_valid_skill_name, now_ms};
pub use error::{Result, StorageError};
pub use event_log::{EventLogWriter, event_log_path, read_event_log};
pub use incoming_webhooks::IncomingWebhook;
pub use journal::{EventJournal, FsJournalSink, JournalRecord, JournalSink};
pub use paths::{DATA_DIR_ENV, data_dir, resolve_data_dir, without_verbatim_prefix};
pub use records::{
    Approval, AuditEntry, Bot, BotPatch, BotSkillAssignment, BotWorkspaceRecord, Chat, ChatFlags,
    EventIndexRow, Message, Run, RunStep, SkillAssignmentMode, User, WorkspaceSettingsRecord,
};
