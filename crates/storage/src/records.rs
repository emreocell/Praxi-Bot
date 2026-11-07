//! Row types of the storage layer.  Timestamps are Unix epoch milliseconds.
//!
//! `state` / `status` columns are plain strings here; `task-runtime` owns the
//! typed enums and their legal transitions.

use protocol::{BotId, ChatId, RunId, UserId};
use serde_json::Value;
use uuid::Uuid;

/// A human user.
#[derive(Debug, Clone, PartialEq)]
pub struct User {
    /// Primary key.
    pub id: UserId,
    /// Display name.
    pub name: String,
    /// Creation time.
    pub created_at_ms: i64,
}

/// A bot profile owned by a user.
#[derive(Debug, Clone, PartialEq)]
pub struct Bot {
    /// Primary key.
    pub id: BotId,
    /// Owner.
    pub user_id: UserId,
    /// Display name.
    pub name: String,
    /// Free-form configuration.
    pub config: Value,
    /// Creation time.
    pub created_at_ms: i64,
}

/// A chat thread.
#[derive(Debug, Clone, PartialEq)]
pub struct Chat {
    /// Primary key.
    pub id: ChatId,
    /// Owner.
    pub user_id: UserId,
    /// Bot this chat talks to.
    pub bot_id: BotId,
    /// Title.
    pub title: String,
    /// Creation time.
    pub created_at_ms: i64,
}

/// A chat message.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    /// Primary key (`UUIDv7`).
    pub id: Uuid,
    /// Owner.
    pub user_id: UserId,
    /// Chat it belongs to.
    pub chat_id: ChatId,
    /// Run that produced it, if any.
    pub run_id: Option<RunId>,
    /// `user` / `assistant` / `system` / `tool`.
    pub role: String,
    /// Text content.
    pub content: String,
    /// Creation time.
    pub created_at_ms: i64,
}

/// A durable run (one bot turn or scheduled job).
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    /// Primary key.
    pub id: RunId,
    /// Owner.
    pub user_id: UserId,
    /// Bot executing the run.
    pub bot_id: BotId,
    /// Chat the run belongs to, if any.
    pub chat_id: Option<ChatId>,
    /// Free-form kind, e.g. `turn` or `scheduled`.
    pub kind: String,
    /// Serialised run state.
    pub state: String,
    /// Terminal error message, if failed.
    pub error: Option<String>,
    /// Creation time.
    pub created_at_ms: i64,
    /// Last state change time.
    pub updated_at_ms: i64,
    /// The run that spawned this one (a subagent), if any.
    pub parent_run_id: Option<RunId>,
    /// Spawn parameters of a subagent run (task, limits, ...); `None` for
    /// top-level runs.
    pub spec: Option<Value>,
}

/// One persisted step of a run.
#[derive(Debug, Clone, PartialEq)]
pub struct RunStep {
    /// Primary key (`UUIDv7`).
    pub id: Uuid,
    /// Parent run.
    pub run_id: RunId,
    /// Owner.
    pub user_id: UserId,
    /// Zero-based position within the run.
    pub idx: u32,
    /// Step name.
    pub name: String,
    /// Step input (the intent).
    pub input: Value,
    /// Whether the step is safe to replay.
    pub idempotent: bool,
    /// Serialised retry policy.
    pub retry: Option<Value>,
    /// Serialised step status.
    pub status: String,
    /// Current attempt number (1-based).
    pub attempt: u32,
    /// Step result.
    pub output: Option<Value>,
    /// Error text of the last failed attempt.
    pub error: Option<String>,
    /// Intent persisted at.
    pub started_at_ms: i64,
    /// Result persisted at.
    pub finished_at_ms: Option<i64>,
}

/// A user approval request.
#[derive(Debug, Clone, PartialEq)]
pub struct Approval {
    /// Primary key (`UUIDv7`).
    pub id: Uuid,
    /// Owner.
    pub user_id: UserId,
    /// Run awaiting the approval.
    pub run_id: RunId,
    /// Step the approval concerns.
    pub step_id: Option<Uuid>,
    /// Human-readable summary of what is being approved.
    pub summary: String,
    /// `None` while pending, else e.g. `approved` / `denied`.
    pub decision: Option<String>,
    /// Request time.
    pub requested_at_ms: i64,
    /// Decision time.
    pub decided_at_ms: Option<i64>,
}

/// An append-only audit record.
#[derive(Debug, Clone, PartialEq)]
pub struct AuditEntry {
    /// Auto-increment key.
    pub id: i64,
    /// Owner.
    pub user_id: UserId,
    /// Related run, if any.
    pub run_id: Option<RunId>,
    /// Who acted (`user`, `bot`, `system`, ...).
    pub actor: String,
    /// What happened.
    pub action: String,
    /// Structured details.
    pub detail: Value,
    /// Time.
    pub ts_ms: i64,
}

/// One row of the daemon-wide event order: global sequence number `gseq` is
/// the event `seq` of run `run_id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventIndexRow {
    /// Daemon-wide, monotonically increasing sequence number (starts at 1).
    pub gseq: u64,
    /// Run the event belongs to.
    pub run_id: RunId,
    /// The envelope's run-local `seq`.
    pub seq: u64,
}
