//! SQLite persistence for schedules and triggers, following `storage` crate
//! patterns (WAL, `PRAGMA user_version` forward-only migrations, internal
//! `Mutex<Connection>`).

use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use protocol::{BotId, ChatId, TaskId};
use rusqlite::types::Type;
use rusqlite::{Connection, OptionalExtension, Row, params};
use tokio::sync::Notify;
use uuid::Uuid;

use crate::error::{Result, SchedulerError};
use crate::model::{MisfirePolicy, Payload, Schedule, ScheduleSpec, Trigger};

/// Embedded forward-only migrations; schema version = index + 1.
const MIGRATIONS: &[(&str, &str)] = &[(
    "0001_schedules",
    include_str!("../migrations/0001_schedules.sql"),
)];

const SCHEDULE_SELECT: &str = "SELECT id, bot_id, chat_id, name, spec_json, payload_json, \
    enabled, created_at_ms, last_run_at_ms, next_run_at_ms, misfire_policy FROM schedules";

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

    /// Insert or replace a schedule and notify the scheduler loop.
    ///
    /// # Errors
    /// Fails if `schedule.spec` is invalid or on SQLite/JSON errors.
    pub fn upsert(&self, schedule: &Schedule) -> Result<()> {
        schedule.spec.validate()?;
        let spec_json = serde_json::to_string(&schedule.spec)?;
        let payload_json = serde_json::to_string(&schedule.payload)?;
        self.conn().execute(
            "INSERT INTO schedules (id, bot_id, chat_id, name, spec_json, payload_json, \
             enabled, created_at_ms, last_run_at_ms, next_run_at_ms, misfire_policy) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) \
             ON CONFLICT(id) DO UPDATE SET \
             bot_id = excluded.bot_id, \
             chat_id = excluded.chat_id, \
             name = excluded.name, \
             spec_json = excluded.spec_json, \
             payload_json = excluded.payload_json, \
             enabled = excluded.enabled, \
             created_at_ms = excluded.created_at_ms, \
             last_run_at_ms = excluded.last_run_at_ms, \
             next_run_at_ms = excluded.next_run_at_ms, \
             misfire_policy = excluded.misfire_policy",
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

    /// Update an existing schedule. Returns `false` if no schedule with
    /// `schedule.id` exists.
    ///
    /// # Errors
    /// Fails if `schedule.spec` is invalid or on SQLite/JSON errors.
    pub fn update(&self, schedule: &Schedule) -> Result<bool> {
        schedule.spec.validate()?;
        let spec_json = serde_json::to_string(&schedule.spec)?;
        let payload_json = serde_json::to_string(&schedule.payload)?;
        let changed = self.conn().execute(
            "UPDATE schedules SET bot_id = ?2, chat_id = ?3, name = ?4, spec_json = ?5, \
             payload_json = ?6, enabled = ?7, last_run_at_ms = ?8, next_run_at_ms = ?9, \
             misfire_policy = ?10 WHERE id = ?1",
            params![
                schedule.id.to_string(),
                schedule.bot_id.to_string(),
                schedule.chat_id.map(|c| c.to_string()),
                schedule.name,
                spec_json,
                payload_json,
                i64::from(schedule.enabled),
                schedule.last_run_at,
                schedule.next_run_at,
                schedule.misfire_policy.as_str(),
            ],
        )?;
        if changed > 0 {
            self.notify();
        }
        Ok(changed > 0)
    }

    /// Enable or disable a schedule and update its `next_run_at`.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn set_enabled(&self, id: TaskId, enabled: bool, next_run_at: Option<i64>) -> Result<bool> {
        let changed = self.conn().execute(
            "UPDATE schedules SET enabled = ?2, next_run_at_ms = ?3 WHERE id = ?1",
            params![id.to_string(), i64::from(enabled), next_run_at],
        )?;
        if changed > 0 {
            self.notify();
        }
        Ok(changed > 0)
    }

    /// Persist `last_run_at` and the new `next_run_at` **before** executing an
    /// occurrence so a crash or restart never fires the same occurrence twice.
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
