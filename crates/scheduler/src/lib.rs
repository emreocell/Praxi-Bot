//! Scheduled tasks, misfire policies, HMAC-SHA256 webhook triggers, and
//! deterministic EN/TR natural-language schedule parsing (F2).
//!
//! # Overview
//! - [`Schedule`], [`ScheduleSpec`], [`Payload`], [`MisfirePolicy`], and
//!   [`Trigger`] define the persisted schedule model.
//! - [`ScheduleStore`] persists schedules and triggers in SQLite (`schedules.db`)
//!   using WAL mode and forward-only `PRAGMA user_version` migrations.
//! - [`Scheduler::start`] spawns a single Tokio background task that sleeps
//!   until the nearest `next_run_at`, wakes immediately on store changes,
//!   detects wall-clock jumps (sleep/resume, DST, manual clock changes) by
//!   comparing monotonic vs. wall time, applies [`MisfirePolicy`], atomically
//!   reserves occurrence intents with timer progress, and enforces a
//!   per-schedule concurrency limit of 1. Definitions have an internal revision
//!   so edits cannot overwrite timer progress or run stale catch-up payloads.
//!
//! - [`handle_webhook`] (and [`Scheduler::handle_webhook`]) verifies an
//!   `HMAC-SHA256` signature and executes the schedule's payload with the
//!   request body substituted for `{input}`.
//! - [`parse_natural_language`] converts common English and Turkish schedule
//!   phrases into a [`ScheduleSpec`] using a deterministic table-driven parser.
//!
//! # Durable dispatch boundary (task 136)
//! An occurrence is identified by `schedule_id:due_ms`. Pending/dispatched
//! intents survive restart, but the current [`ScheduleExecutor`] cannot
//! durably deduplicate a start or return its `run_id`. At startup the engine
//! therefore reclassifies unresolved intents as `missed` / `interrupted`
//! (visible as [`Schedule::missed_occurrences`], an interrupted one-shot is
//! disabled) without replaying them. Full no-loss/no-duplicate
//! recovery, `started(run_id)`, and disabling one-shots after a durable start
//! require the task-136 `AgentLoop::start_scheduled` contract. A reserved
//! one-shot currently has an exhausted timer, not a fabricated start record.

pub mod clock;
pub mod engine;
pub mod error;
pub mod model;
pub mod nl;
pub mod store;
pub mod webhook;

#[cfg(test)]
mod tests;

pub use clock::{Clock, FakeClock, SystemClock};
pub use engine::{CLOCK_JUMP_THRESHOLD_MS, MISFIRE_GRACE_MS, ScheduleExecutor, Scheduler};
pub use error::{Result, SchedulerError};
pub use model::{
    MisfirePolicy, ParsedCron, Payload, RUN_ALL_CAP, Schedule, ScheduleId, ScheduleOrigin,
    SchedulePayload, ScheduleSpec, Spec, Trigger,
};
pub use nl::{parse_natural_language, parse_natural_language_with_tz, parse_nl};
pub use protocol::{BotId, ChatId, Initiator, InitiatorKind, TaskId};
pub use store::{SCHEMA_VERSION, ScheduleStore, Store};
pub use webhook::{
    REPLAY_WINDOW_MS, ReplayGuard, WebhookMeta, handle_webhook, sign_webhook_body,
    sign_webhook_hex, verify_webhook_signature,
};
