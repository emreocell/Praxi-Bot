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
//!   comparing monotonic vs. wall time, applies [`MisfirePolicy`], persists
//!   state before executing so no occurrence fires twice across restarts, and
//!   enforces a per-schedule concurrency limit of 1.
//! - [`handle_webhook`] (and [`Scheduler::handle_webhook`]) verifies an
//!   `HMAC-SHA256` signature and executes the schedule's payload with the
//!   request body substituted for `{input}`.
//! - [`parse_natural_language`] converts common English and Turkish schedule
//!   phrases into a [`ScheduleSpec`] using a deterministic table-driven parser.

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
    MisfirePolicy, ParsedCron, Payload, RUN_ALL_CAP, Schedule, ScheduleId, SchedulePayload,
    ScheduleSpec, Spec, Trigger,
};
pub use nl::{parse_natural_language, parse_natural_language_with_tz, parse_nl};
pub use protocol::{BotId, ChatId, Initiator, InitiatorKind, TaskId};
pub use store::{ScheduleStore, Store};
pub use webhook::{
    REPLAY_WINDOW_MS, ReplayGuard, WebhookMeta, handle_webhook, sign_webhook_body,
    sign_webhook_hex, verify_webhook_signature,
};
