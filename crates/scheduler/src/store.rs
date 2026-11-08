//! SQLite persistence for schedules and triggers, following `storage` crate
//! patterns (WAL, `PRAGMA user_version` forward-only migrations, internal
//! `Mutex<Connection>`).

use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use protocol::{BotId, ChatId, TaskId};
use rusqlite::types::Type;
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use tokio::sync::Notify;
use uuid::Uuid;

use crate::error::{Result, SchedulerError};
use crate::model::{MisfirePolicy, Payload, Schedule, ScheduleSpec, Trigger};

/// Embedded forward-only migrations; schema version = index + 1.
const MIGRATIONS: &[(&str, &str)] = &[
    (
        "0001_schedules",
        include_str!("../migrations/0001_schedules.sql"),
    ),
    (
        "0002_schedule_consistency",
        include_str!("../migrations/0002_schedule_consistency.sql"),
    ),
];

const SCHEDULE_SELECT: &str = "SELECT id, bot_id, chat_id, name, spec_json, payload_json, \
    enabled, created_at_ms, last_run_at_ms, next_run_at_ms, misfire_policy, revision FROM schedules";

/// Type alias for [`ScheduleStore`].
pub type Store = ScheduleStore;

/// Synchronous SQLite repository for schedules and triggers, with an async
/// change notification channel so the [`crate::Scheduler`] wakes immediately on
/// edits.
pub struct ScheduleStore {
    conn: Mutex<Connection>,
    notify: Notify,
}

impl std::fmt::Debug for ScheduleStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScheduleStore").finish_non_exhaustive()
    }
}

impl ScheduleStore {
    /// Validate and migrate an existing staged database without starting a scheduler.
    /// Returns the resulting schema version. Never creates a missing database.
    ///
    /// # Errors
    /// Fails on corruption, an incompatible schema, or migration errors.
    pub fn validate_and_migrate(path: impl AsRef<Path>) -> Result<u32> {
        let mut conn =
            Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let integrity: String = conn.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
        if integrity != "ok" {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
                Some(integrity),
            )
            .into());
        }
        migrate(&mut conn)?;
        // user_version alone does not prove that the required schema exists.
        conn.prepare(SCHEDULE_SELECT)?;
        conn.prepare("SELECT schedule_id, trigger_json, created_at_ms FROM schedule_triggers")?;
        conn.prepare("SELECT schedule_id, due_ms, revision, run_id, state FROM occurrences")?;
        Ok(conn.query_row("PRAGMA user_version", [], |row| row.get(0))?)
    }

    /// Open (creating if needed) `<data_dir>/schedules.db`, enable WAL +
    /// foreign keys, and apply pending migrations.
    ///
    /// # Errors
    /// Fails on filesystem, SQLite, or migration errors.
    pub fn open(data_dir: impl AsRef<Path>) -> Result<Self> {
        let dir = data_dir.as_ref();
        std::fs::create_dir_all(dir)?;
        let conn = Connection::open(dir.join("schedules.db"))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Self::init(conn)
    }

    /// Open `<storage.data_dir()>/schedules.db` alongside an existing
    /// [`storage::Storage`] instance.
    ///
    /// # Errors
    /// Fails on filesystem, SQLite, or migration errors.
    pub fn from_storage(storage: &storage::Storage) -> Result<Self> {
        Self::open(storage.data_dir())
    }

    /// Open an in-memory SQLite database (for tests).
    ///
    /// # Errors
    /// Fails on SQLite or migration errors.
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(Duration::from_secs(5))?;
        migrate(&mut conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            notify: Notify::new(),
        })
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Wake the scheduler loop (called automatically on schedule mutations).
    pub fn notify(&self) {
        self.notify.notify_one();
    }

    /// Future that resolves when the store changes or [`Self::notify`] is called.
    pub fn notified(&self) -> tokio::sync::futures::Notified<'_> {
        self.notify.notified()
    }

    /// Insert a new schedule. Validates `schedule.spec` before persisting.
    ///
    /// # Errors
    /// Fails if `schedule.spec` is invalid or on SQLite/JSON errors.
    pub fn insert(&self, schedule: &Schedule) -> Result<()> {
        schedule.spec.validate()?;
        let spec_json = serde_json::to_string(&schedule.spec)?;
        let payload_json = serde_json::to_string(&schedule.payload)?;
        self.conn().execute(
            "INSERT INTO schedules (id, bot_id, chat_id, name, spec_json, payload_json, \
             enabled, created_at_ms, last_run_at_ms, next_run_at_ms, misfire_policy) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                schedule.id.to_string(),
                schedule.bot_id.to_string(),
                schedule.chat_id.map(|c| c.to_string()),
                schedule.name,
                spec_json,
                payload_json,
                i64::from(schedule.enabled),
                schedule.created_at,
                schedule.last_run_at,
                schedule.next_run_at,
                schedule.misfire_policy.as_str(),
            ],
        )?;
        self.notify();
        Ok(())
    }

    /// Insert a new schedule, or replace only an existing row's definition.
    /// Runtime cursors and the original bot/creation identity are preserved on
    /// conflict. Definition changes invalidate unstarted catch-up occurrences.
    ///
    /// # Errors
    /// Fails if `schedule.spec` is invalid or on SQLite/JSON errors.
    pub fn upsert(&self, schedule: &Schedule) -> Result<()> {
        schedule.spec.validate()?;
        let spec_json = serde_json::to_string(&schedule.spec)?;
        let payload_json = serde_json::to_string(&schedule.payload)?;
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO schedules (id, bot_id, chat_id, name, spec_json, payload_json, \
             enabled, created_at_ms, last_run_at_ms, next_run_at_ms, misfire_policy) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) \
             ON CONFLICT(id) DO UPDATE SET \
             chat_id = excluded.chat_id, name = excluded.name, \
             spec_json = excluded.spec_json, payload_json = excluded.payload_json, \
             enabled = excluded.enabled, misfire_policy = excluded.misfire_policy, \
             revision = schedules.revision + 1",
            params![
                schedule.id.to_string(),
                schedule.bot_id.to_string(),
                schedule.chat_id.map(|c| c.to_string()),
                schedule.name,
                spec_json,
                payload_json,
                i64::from(schedule.enabled),
                schedule.created_at,
                schedule.last_run_at,
                schedule.next_run_at,
                schedule.misfire_policy.as_str(),
            ],
        )?;
        cancel_pending(&tx, schedule.id)?;
        tx.commit()?;
        drop(conn);
        self.notify();
        Ok(())
    }

    /// Fetch a schedule by ID.
    ///
    /// # Errors
    /// Fails on SQLite or deserialisation errors.
    pub fn get(&self, id: TaskId) -> Result<Option<Schedule>> {
        Ok(self
            .conn()
            .query_row(
                &format!("{SCHEDULE_SELECT} WHERE id = ?1"),
                [id.to_string()],
                schedule_row,
            )
            .optional()?)
    }

    /// List all schedules ordered by `created_at_ms, id`.
    ///
    /// # Errors
    /// Fails on SQLite or deserialisation errors.
    pub fn list(&self) -> Result<Vec<Schedule>> {
        let conn = self.conn();
        let mut st = conn.prepare(&format!("{SCHEDULE_SELECT} ORDER BY created_at_ms, id"))?;
        let rows = st.query_map([], schedule_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// List all schedules belonging to `bot_id`.
    ///
    /// # Errors
    /// Fails on SQLite or deserialisation errors.
    pub fn list_by_bot(&self, bot_id: BotId) -> Result<Vec<Schedule>> {
        let conn = self.conn();
        let mut st = conn.prepare(&format!(
            "{SCHEDULE_SELECT} WHERE bot_id = ?1 ORDER BY created_at_ms, id"
        ))?;
        let rows = st.query_map([bot_id.to_string()], schedule_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// List all enabled schedules whose `next_run_at` is `<= now_ms`, ordered
    /// by `next_run_at_ms ASC, id ASC`.
    ///
    /// # Errors
    /// Fails on SQLite or deserialisation errors.
    pub fn list_due(&self, now_ms: i64) -> Result<Vec<Schedule>> {
        let conn = self.conn();
        let mut st = conn.prepare(&format!(
            "{SCHEDULE_SELECT} WHERE enabled = 1 AND next_run_at_ms IS NOT NULL \
             AND next_run_at_ms <= ?1 ORDER BY next_run_at_ms ASC, id ASC"
        ))?;
        let rows = st.query_map([now_ms], schedule_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Earliest `next_run_at` across all enabled schedules.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn nearest_next_run_at(&self) -> Result<Option<i64>> {
        let min_ts: Option<i64> = self.conn().query_row(
            "SELECT MIN(next_run_at_ms) FROM schedules \
             WHERE enabled = 1 AND next_run_at_ms IS NOT NULL",
            [],
            |r| r.get(0),
        )?;
        Ok(min_ts)
    }

    /// Replace a definition only; never write `last_run_at` or `next_run_at`.
    /// Returns `false` when the row was deleted or its definition revision
    /// changed since the caller read it. Unstarted occurrences are cancelled.
    ///
    /// # Errors
    /// Fails if the specification is invalid or on SQLite/JSON errors.
    pub fn update_definition(&self, schedule: &Schedule) -> Result<bool> {
        schedule.spec.validate()?;
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = update_definition_in(&tx, schedule)?;
        tx.commit()?;
        drop(conn);
        if changed {
            self.notify();
        }
        Ok(changed)
    }

    /// Compatibility entry point for definition-only updates.
    /// See [`Self::update_definition`]; runtime state is never accepted from a
    /// potentially stale caller snapshot.
    ///
    /// # Errors
    /// As [`Self::update_definition`].
    pub fn update(&self, schedule: &Schedule) -> Result<bool> {
        self.update_definition(schedule)
    }

    /// Set an edited definition's timer only while its revision still matches.
    /// This is a low-level primitive: for a read/compute/write edit, use
    /// [`Self::update_definition_and_rearm`] so timer progress cannot race the
    /// calculation. This method never changes `last_run_at`.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn set_next_run_after_edit(
        &self,
        id: TaskId,
        expected_revision: i64,
        next: Option<i64>,
    ) -> Result<bool> {
        let changed = set_next_run_after_edit_in(&self.conn(), id, expected_revision, next)?;
        if changed {
            self.notify();
        }
        Ok(changed)
    }

    /// Apply a definition and re-arm a changed or re-enabled timer in one
    /// transaction, using the CURRENT row rather than the caller's run state.
    /// `enabled: None` preserves the current enabled state. An unchanged,
    /// already-consumed one-shot stays consumed even when its cursor is null.
    /// Returns the fresh row and its previous enabled state, or `None` if it
    /// was deleted. The internal revision is not exposed through the API DTO.
    ///
    /// # Errors
    /// Fails on invalid specifications, a changed bot identity, or SQLite/JSON errors.
    pub fn update_definition_and_rearm(
        &self,
        definition: &Schedule,
        enabled: Option<bool>,
        now_ms: i64,
    ) -> Result<Option<(Schedule, bool)>> {
        definition.spec.validate()?;
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(current) = get_schedule(&tx, definition.id)? else {
            return Ok(None);
        };
        if current.bot_id != definition.bot_id {
            return Err(SchedulerError::InvalidSpec(
                "the bot of a schedule cannot be changed".into(),
            ));
        }
        let next_revision = current.revision.checked_add(1).ok_or_else(|| {
            SchedulerError::InvalidSpec("schedule definition revision is exhausted".into())
        })?;
        let was_enabled = current.enabled;
        let enabled = enabled.unwrap_or(was_enabled);
        let next = rearm_if_spec_changed(&current, &definition.spec, enabled, now_ms)?;
        let mut edited = definition.clone();
        edited.enabled = enabled;
        edited.revision = current.revision;
        update_definition_in(&tx, &edited)?;
        set_next_run_after_edit_in(&tx, current.id, next_revision, next)?;
        let fresh = get_schedule(&tx, current.id)?;
        tx.commit()?;
        drop(conn);
        self.notify();
        Ok(fresh.map(|schedule| (schedule, was_enabled)))
    }

    /// Enable or disable a schedule and update its timer. This is a definition
    /// change: bump its revision and cancel unstarted catch-up occurrences.
    /// Already-dispatched work is allowed to finish.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn set_enabled(&self, id: TaskId, enabled: bool, next_run_at: Option<i64>) -> Result<bool> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "UPDATE schedules SET enabled = ?2, next_run_at_ms = ?3, \
             revision = revision + 1 WHERE id = ?1",
            params![id.to_string(), i64::from(enabled), next_run_at],
        )?;
        if changed > 0 {
            cancel_pending(&tx, id)?;
        }
        tx.commit()?;
        drop(conn);
        if changed > 0 {
            self.notify();
        }
        Ok(changed > 0)
    }

    /// Persist legacy timer progress. This does not make execution durable:
    /// a crash before the executor creates a run can still lose the firing.
    /// The timer engine uses transactional occurrence reservations instead.
    ///
    /// Does not emit a change notification (called by the engine loop itself).
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn record_run_intent(
        &self,
        id: TaskId,
        last_run_at: i64,
        next_run_at: Option<i64>,
    ) -> Result<bool> {
        let changed = self.conn().execute(
            "UPDATE schedules SET last_run_at_ms = ?2, next_run_at_ms = ?3 WHERE id = ?1",
            params![id.to_string(), last_run_at, next_run_at],
        )?;
        Ok(changed > 0)
    }

    /// Update only `last_run_at` (webhook firings).
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn touch_last_run(&self, id: TaskId, last_run_at: i64) -> Result<bool> {
        let changed = self.conn().execute(
            "UPDATE schedules SET last_run_at_ms = ?2 WHERE id = ?1",
            params![id.to_string(), last_run_at],
        )?;
        Ok(changed > 0)
    }

    /// Advance `next_run_at` without modifying `last_run_at` (used when
    /// skipping occurrences due to [`MisfirePolicy::Skip`] or the per-schedule
    /// concurrency limit).
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn advance_next_run(&self, id: TaskId, next_run_at: Option<i64>) -> Result<bool> {
        let changed = self.conn().execute(
            "UPDATE schedules SET next_run_at_ms = ?2 WHERE id = ?1",
            params![id.to_string(), next_run_at],
        )?;
        Ok(changed > 0)
    }

    /// Advance only the timer snapshot the engine actually inspected. An
    /// intervening definition edit, disable, delete, or firing wins the race.
    pub(crate) fn advance_if_current(
        &self,
        schedule: &Schedule,
        next_run_at: Option<i64>,
    ) -> Result<bool> {
        Ok(self.conn().execute(
            "UPDATE schedules SET next_run_at_ms = ?4 WHERE id = ?1 \
             AND enabled = 1 AND revision = ?2 AND next_run_at_ms IS ?3",
            params![
                schedule.id.to_string(),
                schedule.revision,
                schedule.next_run_at,
                next_run_at,
            ],
        )? > 0)
    }

    /// Park a broken timer only if it has not been edited since inspection.
    pub(crate) fn disable_if_current(&self, schedule: &Schedule) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if tx.execute(
            "UPDATE schedules SET enabled = 0, next_run_at_ms = NULL, \
             revision = revision + 1 WHERE id = ?1 AND revision = ?2",
            params![schedule.id.to_string(), schedule.revision],
        )? > 0
        {
            cancel_pending(&tx, schedule.id)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Atomically persist the occurrence identities and consume the inspected
    /// timer window. No execution or successful start is implied by a pending
    /// intent. A stale snapshot cannot move the cursor or create claims.
    pub(crate) fn reserve_occurrences(
        &self,
        schedule: &Schedule,
        due_slots: &[i64],
        next_run_at: Option<i64>,
    ) -> Result<bool> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "UPDATE schedules SET next_run_at_ms = ?4 WHERE id = ?1 \
             AND enabled = 1 AND revision = ?2 AND next_run_at_ms IS ?3",
            params![
                schedule.id.to_string(),
                schedule.revision,
                schedule.next_run_at,
                next_run_at,
            ],
        )?;
        if changed == 0 {
            return Ok(false);
        }
        for due_ms in due_slots {
            tx.execute(
                "INSERT INTO occurrences (schedule_id, due_ms, revision, run_id, state) \
                 VALUES (?1, ?2, ?3, NULL, 'pending') \
                 ON CONFLICT(schedule_id, due_ms) DO NOTHING",
                params![schedule.id.to_string(), due_ms, schedule.revision],
            )?;
        }
        tx.commit()?;
        Ok(true)
    }

    /// The per-occurrence linearization point: check the current definition
    /// and atomically claim one pending intent before entering the executor.
    /// A claim is `dispatched`, never `started`: the legacy executor neither
    /// accepts an idempotency key nor returns a durable run id (task 136).
    pub(crate) fn claim_occurrence(
        &self,
        id: TaskId,
        revision: i64,
        due_ms: i64,
        now_ms: i64,
    ) -> Result<Option<Schedule>> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(mut current) = get_schedule(&tx, id)? else {
            return Ok(None);
        };
        if !current.enabled || current.revision != revision {
            return Ok(None);
        }
        let changed = tx.execute(
            "UPDATE occurrences SET state = 'dispatched' WHERE schedule_id = ?1 \
             AND due_ms = ?2 AND revision = ?3 AND state = 'pending'",
            params![id.to_string(), due_ms, revision],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        tx.execute(
            "UPDATE schedules SET last_run_at_ms = ?2 WHERE id = ?1",
            params![id.to_string(), now_ms],
        )?;
        current.last_run_at = Some(now_ms);
        tx.commit()?;
        Ok(Some(current))
    }

    /// Record only the legacy executor's outcome; no fabricated run id or
    /// `started` acknowledgment. An interrupted dispatch stays uncertain and
    /// is NOT retried until task 136 supplies durable start idempotency.
    pub(crate) fn finish_occurrence(&self, id: TaskId, due_ms: i64, succeeded: bool) -> Result<()> {
        self.conn().execute(
            "UPDATE occurrences SET state = ?3 WHERE schedule_id = ?1 \
             AND due_ms = ?2 AND state = 'dispatched'",
            params![
                id.to_string(),
                due_ms,
                if succeeded { "succeeded" } else { "failed" },
            ],
        )?;
        Ok(())
    }

    pub(crate) fn unresolved_occurrence_count(&self) -> Result<u64> {
        Ok(self.conn().query_row(
            "SELECT COUNT(*) FROM occurrences WHERE state IN ('pending', 'dispatched')",
            [],
            |row| row.get(0),
        )?)
    }

    /// Delete a schedule by ID. Returns `false` if it did not exist.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn delete(&self, id: TaskId) -> Result<bool> {
        let changed = self
            .conn()
            .execute("DELETE FROM schedules WHERE id = ?1", [id.to_string()])?;
        if changed > 0 {
            self.notify();
        }
        Ok(changed > 0)
    }

    /// Attach or replace a [`Trigger`] (e.g. [`Trigger::Webhook`]) for `schedule_id`.
    ///
    /// # Errors
    /// Fails on SQLite/JSON errors or if `schedule_id` does not exist.
    pub fn set_trigger(&self, schedule_id: TaskId, trigger: &Trigger) -> Result<()> {
        let Trigger::Webhook { secret } = trigger;
        if secret.is_empty() {
            return Err(SchedulerError::InvalidSpec(
                "webhook secret must not be empty".into(),
            ));
        }
        let trigger_json = serde_json::to_string(trigger)?;
        self.conn().execute(
            "INSERT INTO schedule_triggers (schedule_id, trigger_json, created_at_ms) \
             VALUES (?1, ?2, ?3) \
             ON CONFLICT(schedule_id) DO UPDATE SET trigger_json = excluded.trigger_json",
            params![schedule_id.to_string(), trigger_json, storage::now_ms()],
        )?;
        Ok(())
    }

    /// Fetch the [`Trigger`] registered for `schedule_id`, if any.
    ///
    /// # Errors
    /// Fails on SQLite or deserialisation errors.
    pub fn get_trigger(&self, schedule_id: TaskId) -> Result<Option<Trigger>> {
        let raw: Option<String> = self
            .conn()
            .query_row(
                "SELECT trigger_json FROM schedule_triggers WHERE schedule_id = ?1",
                [schedule_id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        raw.map(|s| serde_json::from_str(&s).map_err(SchedulerError::from))
            .transpose()
    }

    /// Remove any [`Trigger`] registered for `schedule_id`.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn remove_trigger(&self, schedule_id: TaskId) -> Result<bool> {
        let changed = self.conn().execute(
            "DELETE FROM schedule_triggers WHERE schedule_id = ?1",
            [schedule_id.to_string()],
        )?;
        Ok(changed > 0)
    }
}

fn get_schedule(conn: &Connection, id: TaskId) -> Result<Option<Schedule>> {
    Ok(conn
        .query_row(
            &format!("{SCHEDULE_SELECT} WHERE id = ?1"),
            [id.to_string()],
            schedule_row,
        )
        .optional()?)
}

fn cancel_pending(conn: &Connection, id: TaskId) -> Result<()> {
    conn.execute(
        "DELETE FROM occurrences WHERE schedule_id = ?1 AND state = 'pending'",
        [id.to_string()],
    )?;
    Ok(())
}

fn update_definition_in(conn: &Connection, schedule: &Schedule) -> Result<bool> {
    let changed = conn.execute(
        "UPDATE schedules SET chat_id = ?2, name = ?3, spec_json = ?4, payload_json = ?5, \
         enabled = ?6, misfire_policy = ?7, revision = revision + 1 \
         WHERE id = ?1 AND revision = ?8",
        params![
            schedule.id.to_string(),
            schedule.chat_id.map(|c| c.to_string()),
            schedule.name,
            serde_json::to_string(&schedule.spec)?,
            serde_json::to_string(&schedule.payload)?,
            i64::from(schedule.enabled),
            schedule.misfire_policy.as_str(),
            schedule.revision,
        ],
    )?;
    if changed > 0 {
        cancel_pending(conn, schedule.id)?;
    }
    Ok(changed > 0)
}

fn set_next_run_after_edit_in(
    conn: &Connection,
    id: TaskId,
    expected_revision: i64,
    next: Option<i64>,
) -> Result<bool> {
    Ok(conn.execute(
        "UPDATE schedules SET next_run_at_ms = ?3 WHERE id = ?1 AND revision = ?2",
        params![id.to_string(), expected_revision, next],
    )? > 0)
}

fn rearm_if_spec_changed(
    current: &Schedule,
    new_spec: &ScheduleSpec,
    enabled: bool,
    now_ms: i64,
) -> Result<Option<i64>> {
    if !enabled {
        Ok(None)
    } else if new_spec != &current.spec || !current.enabled {
        match new_spec {
            ScheduleSpec::At { unix_ms } => Ok(Some(*unix_ms)),
            other => other.next_after(now_ms),
        }
    } else {
        Ok(current.next_run_at)
    }
}

fn conv_err<E: std::error::Error + Send + Sync + 'static>(idx: usize, e: E) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(idx, Type::Text, Box::new(e))
}

fn uuid_at(row: &Row<'_>, idx: usize) -> rusqlite::Result<Uuid> {
    let s: String = row.get(idx)?;
    Uuid::parse_str(&s).map_err(|e| conv_err(idx, e))
}

fn opt_uuid_at(row: &Row<'_>, idx: usize) -> rusqlite::Result<Option<Uuid>> {
    let s: Option<String> = row.get(idx)?;
    s.map(|s| Uuid::parse_str(&s).map_err(|e| conv_err(idx, e)))
        .transpose()
}

fn schedule_row(row: &Row<'_>) -> rusqlite::Result<Schedule> {
    let spec_str: String = row.get(4)?;
    let spec: ScheduleSpec = serde_json::from_str(&spec_str).map_err(|e| conv_err(4, e))?;
    let payload_str: String = row.get(5)?;
    let payload: Payload = serde_json::from_str(&payload_str).map_err(|e| conv_err(5, e))?;
    let enabled_int: i64 = row.get(6)?;
    let policy_str: String = row.get(10)?;
    let misfire_policy = MisfirePolicy::parse(&policy_str).ok_or_else(|| {
        conv_err(
            10,
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("unknown misfire_policy '{policy_str}'"),
            ),
        )
    })?;

    Ok(Schedule {
        id: TaskId(uuid_at(row, 0)?),
        bot_id: BotId(uuid_at(row, 1)?),
        chat_id: opt_uuid_at(row, 2)?.map(ChatId),
        name: row.get(3)?,
        spec,
        payload,
        enabled: enabled_int != 0,
        created_at: row.get(7)?,
        last_run_at: row.get(8)?,
        next_run_at: row.get(9)?,
        misfire_policy,
        revision: row.get(11)?,
    })
}

fn migrate(conn: &mut Connection) -> Result<u32> {
    let current: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let latest = u32::try_from(MIGRATIONS.len()).unwrap_or(u32::MAX);
    if current > latest {
        return Err(SchedulerError::SchemaTooNew {
            found: current,
            supported: latest,
        });
    }
    let mut applied = 0;
    for (i, (_name, sql)) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let version = i + 1;
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.execute_batch(&format!("PRAGMA user_version = {version}"))?;
        tx.commit()?;
        applied += 1;
    }
    Ok(applied)
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
