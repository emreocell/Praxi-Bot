//! SQLite persistence for missions: the mission row, its item ledger, notes,
//! sessions and the timeline. Own database file, WAL, forward-only
//! `PRAGMA user_version` migrations, an internal `Mutex<Connection>`.
//!
//! Every write that moves a mission or an item appends a `mission_events` row
//! in the same transaction, so the timeline never disagrees with the ledger.
//! Time is passed in (`now_ms`) so the supervisor's injected clock drives it.

use std::path::Path;
use std::time::Duration;

use parking_lot::{Mutex, MutexGuard};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use serde_json::{Value, json};

use crate::error::{MissionError, Result};
use crate::model::{
    Budgets, Digest, Item, ItemStatus, MAX_ITEMS, MAX_ITEMS_PER_CALL, MAX_KEY_CHARS,
    MAX_NOTE_CHARS, MAX_NOTES, MAX_PAYLOAD_BYTES, MAX_TEXT_CHARS, MAX_TITLE_CHARS, Mission,
    MissionState, Note, Progress, can_transition, clip,
};

const MIGRATIONS: &[(&str, &str)] = &[(
    "0001_missions",
    include_str!("../migrations/0001_missions.sql"),
)];

/// A mission to create.
#[derive(Debug, Clone)]
pub struct NewMission {
    /// Owner.
    pub user_id: String,
    /// Lead bot.
    pub bot_id: String,
    /// The mission's chat.
    pub chat_id: String,
    /// Short label.
    pub title: String,
    /// The owner's words.
    pub goal_text: String,
    /// "At least N items done" criterion.
    pub target_done: Option<u32>,
    /// Limits.
    pub budgets: Budgets,
    /// Items the owner already knows (may be empty; the bot plans the rest).
    pub items: Vec<NewItem>,
}

/// An item to add to the ledger.
#[derive(Debug, Clone)]
pub struct NewItem {
    /// Dedupe key.
    pub key: String,
    /// Free label.
    pub kind: String,
    /// One line of what to do.
    pub title: String,
    /// Small structured facts.
    pub payload: Option<Value>,
}

/// A change to one item.
#[derive(Debug, Clone, Default)]
pub struct ItemUpdate {
    /// New status.
    pub status: Option<ItemStatus>,
    /// Evidence / what was achieved.
    pub result: Option<String>,
    /// Failure text.
    pub error: Option<String>,
    /// Why it is parked.
    pub blocked_reason: Option<String>,
}

/// What `upsert_items` did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UpsertReport {
    /// New items.
    pub added: u32,
    /// Keys that were already in the ledger (left unchanged).
    pub existing: u32,
    /// Items refused because the ledger is full.
    pub dropped: u32,
}

/// How a session ended, as the supervisor records it.
#[derive(Debug, Clone)]
pub struct SessionEndRecord {
    /// `finished`, `guard_stop`, `failed`, `cancelled`, `paused`, `interrupted`.
    pub outcome: String,
    /// Model calls in the session.
    pub model_steps: u32,
    /// Spend in USD.
    pub cost_usd: f64,
}

/// One row of the timeline.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Event {
    /// Monotonic id (use as the `after` cursor).
    pub id: i64,
    /// When.
    pub at_ms: i64,
    /// `created`, `state`, `item`, `session_started`, `session_ended`, `digest`, ...
    pub kind: String,
    /// Item the event is about.
    pub item_key: Option<String>,
    /// Details (never secrets; ids, counts and short reasons).
    pub data: Value,
}

/// The mission database.
pub struct MissionStore {
    conn: Mutex<Connection>,
}

impl std::fmt::Debug for MissionStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MissionStore").finish_non_exhaustive()
    }
}

const MISSION_COLS: &str = "id, user_id, bot_id, chat_id, title, goal_text, target_done, \
    budgets_json, state, state_reason, sessions, model_steps, cost_micro_usd, stalled_sessions, \
    failed_sessions, ledger_version, questions_asked, lease_run_id, digest_json, created_at_ms, \
    started_at_ms, updated_at_ms, finished_at_ms";

const ITEM_COLS: &str = "key, kind, title, status, attempts, payload_json, result, last_error, \
    blocked_reason, owner_answer, updated_at_ms";

#[allow(clippy::cast_precision_loss)] // micro-USD fits an f64 mantissa for any realistic spend
fn usd(micro: i64) -> f64 {
    micro as f64 / 1_000_000.0
}

#[allow(clippy::cast_possible_truncation)] // clamped to a non-negative, finite, bounded value
fn micro(usd: f64) -> i64 {
    if usd.is_finite() && usd > 0.0 {
        (usd * 1_000_000.0).round().min(9.0e15) as i64
    } else {
        0
    }
}

fn conv_err<E: std::error::Error + Send + Sync + 'static>(idx: usize, e: E) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(idx, rusqlite::types::Type::Text, Box::new(e))
}

fn mission_of(row: &Row<'_>) -> rusqlite::Result<Mission> {
    let budgets: String = row.get(7)?;
    let state: String = row.get(8)?;
    let digest: Option<String> = row.get(18)?;
    Ok(Mission {
        id: row.get(0)?,
        user_id: row.get(1)?,
        bot_id: row.get(2)?,
        chat_id: row.get(3)?,
        title: row.get(4)?,
        goal_text: row.get(5)?,
        target_done: row.get::<_, Option<u32>>(6)?,
        budgets: serde_json::from_str(&budgets).map_err(|e| conv_err(7, e))?,
        state: MissionState::parse(&state)
            .ok_or_else(|| conv_err(8, std::io::Error::other(format!("state {state:?}"))))?,
        state_reason: row.get(9)?,
        sessions: row.get(10)?,
        model_steps: row.get(11)?,
        cost_usd: usd(row.get(12)?),
        stalled_sessions: row.get(13)?,
        failed_sessions: row.get(14)?,
        ledger_version: row.get(15)?,
        questions_asked: row.get(16)?,
        lease_run_id: row.get(17)?,
        digest: digest
            .map(|d| serde_json::from_str(&d).map_err(|e| conv_err(18, e)))
            .transpose()?,
        created_at_ms: row.get(19)?,
        started_at_ms: row.get(20)?,
        updated_at_ms: row.get(21)?,
        finished_at_ms: row.get(22)?,
    })
}

fn item_of(row: &Row<'_>) -> rusqlite::Result<Item> {
    let status: String = row.get(3)?;
    let payload: Option<String> = row.get(5)?;
    Ok(Item {
        key: row.get(0)?,
        kind: row.get(1)?,
        title: row.get(2)?,
        status: ItemStatus::parse(&status)
            .ok_or_else(|| conv_err(3, std::io::Error::other(format!("status {status:?}"))))?,
        attempts: row.get(4)?,
        payload: payload
            .map(|p| serde_json::from_str(&p).map_err(|e| conv_err(5, e)))
            .transpose()?,
        result: row.get(6)?,
        last_error: row.get(7)?,
        blocked_reason: row.get(8)?,
        owner_answer: row.get(9)?,
        updated_at_ms: row.get(10)?,
    })
}

/// Whether the bot (not the owner) may move an item from `from` to `to`.
/// Finished items stay finished; only the owner reopens them.
fn model_item_transition_ok(from: ItemStatus, to: ItemStatus) -> bool {
    use ItemStatus::{Blocked, Doing, Done, Failed, Skipped, Todo};
    if from == to {
        return true;
    }
    match from {
        Done | Failed | Skipped => false,
        Todo => matches!(to, Doing | Done | Blocked | Failed | Skipped),
        Doing => matches!(to, Todo | Done | Blocked | Failed | Skipped),
        Blocked => matches!(to, Doing | Done | Failed | Skipped),
    }
}

fn check_text(what: &str, s: &str, max: usize) -> Result<String> {
    let s = s.trim();
    if s.is_empty() {
        return Err(MissionError::Invalid(format!("{what} is empty")));
    }
    if s.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
        return Err(MissionError::Invalid(format!(
            "{what} contains control characters"
        )));
    }
    Ok(clip(s, max))
}

fn event(
    tx: &Transaction<'_>,
    id: &str,
    now: i64,
    kind: &str,
    item: Option<&str>,
    data: &Value,
) -> Result<()> {
    tx.execute(
        "INSERT INTO mission_events (mission_id, at_ms, kind, item_key, data_json) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, now, kind, item, data.to_string()],
    )?;
    Ok(())
}

impl MissionStore {
    /// Open (creating and migrating) the database at `path`.
    ///
    /// # Errors
    /// SQLite or filesystem failures; a database from a newer build.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        if let Some(dir) = path.as_ref().parent()
            && !dir.as_os_str().is_empty()
        {
            std::fs::create_dir_all(dir)?;
        }
        Self::init(Connection::open(path)?)
    }

    /// An in-memory database (tests).
    ///
    /// # Errors
    /// SQLite failure.
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Self> {
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let current: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        let supported = u32::try_from(MIGRATIONS.len()).unwrap_or(u32::MAX);
        if current > supported {
            return Err(MissionError::SchemaTooNew {
                found: current,
                supported,
            });
        }
        for (i, (_, sql)) in MIGRATIONS.iter().enumerate().skip(current as usize) {
            let tx = conn.transaction()?;
            tx.execute_batch(sql)?;
            tx.pragma_update(
                None,
                "user_version",
                i64::try_from(i + 1).unwrap_or(i64::MAX),
            )?;
            tx.commit()?;
        }
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock()
    }

    // ---- missions ----

    /// Create a mission in the `running` state.
    ///
    /// # Errors
    /// Invalid budgets or goal; SQLite failure.
    pub fn create(&self, new: &NewMission, now: i64) -> Result<Mission> {
        new.budgets.validate().map_err(MissionError::Invalid)?;
        let goal = new.goal_text.trim();
        if goal.is_empty() {
            return Err(MissionError::Invalid("the goal is empty".into()));
        }
        let title = if new.title.trim().is_empty() {
            clip(goal, 60)
        } else {
            clip(&new.title, 80)
        };
        let id = uuid::Uuid::now_v7().to_string();
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO missions (id, user_id, bot_id, chat_id, title, goal_text, target_done, budgets_json, \
             state, created_at_ms, started_at_ms, updated_at_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'running', ?9, ?9, ?9)",
            params![
                id,
                new.user_id,
                new.bot_id,
                new.chat_id,
                title,
                goal,
                new.target_done,
                serde_json::to_string(&new.budgets)?,
                now
            ],
        )?;
        event(&tx, &id, now, "created", None, &json!({}))?;
        if !new.items.is_empty() {
            upsert_in(&tx, &id, &new.items, now)?;
        }
        tx.commit()?;
        drop(conn);
        self.get(&id)?.ok_or_else(|| MissionError::NotFound(id))
    }

    /// One mission.
    ///
    /// # Errors
    /// SQLite failure.
    pub fn get(&self, id: &str) -> Result<Option<Mission>> {
        Ok(self
            .conn()
            .query_row(
                &format!("SELECT {MISSION_COLS} FROM missions WHERE id = ?1"),
                [id],
                mission_of,
            )
            .optional()?)
    }

    /// A user's missions, newest first.
    ///
    /// # Errors
    /// SQLite failure.
    pub fn list(&self, user_id: &str) -> Result<Vec<Mission>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {MISSION_COLS} FROM missions WHERE user_id = ?1 ORDER BY created_at_ms DESC, id DESC"
        ))?;
        let rows = stmt.query_map([user_id], mission_of)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Every mission in `state`, oldest first (the supervisor's work list).
    ///
    /// # Errors
    /// SQLite failure.
    pub fn list_by_state(&self, state: MissionState) -> Result<Vec<Mission>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {MISSION_COLS} FROM missions WHERE state = ?1 ORDER BY created_at_ms, id"
        ))?;
        let rows = stmt.query_map([state.as_str()], mission_of)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Move a mission to `to` (checked by [`can_transition`]).
    ///
    /// # Errors
    /// Unknown mission, illegal transition, SQLite failure.
    pub fn set_state(
        &self,
        id: &str,
        to: MissionState,
        reason: Option<&str>,
        now: i64,
    ) -> Result<Mission> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        transition_in(&tx, id, to, reason, now)?;
        tx.commit()?;
        drop(conn);
        self.get(id)?
            .ok_or_else(|| MissionError::NotFound(id.to_owned()))
    }

    /// Replace the budgets (owner only; the caller checks that).
    ///
    /// # Errors
    /// Invalid budgets, unknown mission, SQLite failure.
    pub fn set_budgets(&self, id: &str, budgets: &Budgets, now: i64) -> Result<()> {
        budgets.validate().map_err(MissionError::Invalid)?;
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let n = tx.execute(
            "UPDATE missions SET budgets_json = ?2, updated_at_ms = ?3 WHERE id = ?1",
            params![id, serde_json::to_string(budgets)?, now],
        )?;
        if n == 0 {
            return Err(MissionError::NotFound(id.to_owned()));
        }
        event(&tx, id, now, "budgets", None, &json!({}))?;
        tx.commit()?;
        Ok(())
    }

    /// Record the run of the session in flight (`None` clears it).
    ///
    /// # Errors
    /// SQLite failure.
    pub fn set_lease(&self, id: &str, run_id: Option<&str>) -> Result<()> {
        self.conn().execute(
            "UPDATE missions SET lease_run_id = ?2 WHERE id = ?1",
            params![id, run_id],
        )?;
        Ok(())
    }

    /// The one batched owner question a mission allows. `true` the first
    /// time, `false` afterwards: later questions become blocked items.
    ///
    /// # Errors
    /// SQLite failure.
    pub fn take_question_slot(&self, id: &str) -> Result<bool> {
        let n = self.conn().execute(
            "UPDATE missions SET questions_asked = questions_asked + 1 WHERE id = ?1 AND questions_asked = 0",
            [id],
        )?;
        Ok(n == 1)
    }

    /// Whether `bot_id` has a mission that is running (its `mission_*` tools are visible then).
    ///
    /// # Errors
    /// SQLite failure.
    pub fn bot_has_running_mission(&self, bot_id: &str) -> Result<bool> {
        Ok(self.conn().query_row(
            "SELECT EXISTS(SELECT 1 FROM missions WHERE bot_id = ?1 AND state = 'running')",
            [bot_id],
            |r| r.get(0),
        )?)
    }

    // ---- items ----

    /// Add items; keys already in the ledger are left alone. The ledger holds at most [`MAX_ITEMS`].
    ///
    /// # Errors
    /// Invalid items (empty key/title, oversized payload), unknown mission, SQLite failure.
    pub fn upsert_items(&self, id: &str, items: &[NewItem], now: i64) -> Result<UpsertReport> {
        if items.len() > MAX_ITEMS_PER_CALL {
            return Err(MissionError::Invalid(format!(
                "plan at most {MAX_ITEMS_PER_CALL} items per call"
            )));
        }
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let report = upsert_in(&tx, id, items, now)?;
        tx.commit()?;
        Ok(report)
    }

    /// Change one item. `by_owner` lifts the "finished items stay finished" rule.
    ///
    /// # Errors
    /// Unknown item, an illegal status move, a missing blocked reason, SQLite failure.
    pub fn update_item(
        &self,
        id: &str,
        key: &str,
        upd: &ItemUpdate,
        by_owner: bool,
        now: i64,
    ) -> Result<Item> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let cur = item_in(&tx, id, key)?;
        let to = upd.status.unwrap_or(cur.status);
        if !by_owner && !model_item_transition_ok(cur.status, to) {
            return Err(MissionError::Invalid(format!(
                "item {key:?} is {} and cannot become {to}; finished items stay finished",
                cur.status
            )));
        }
        let reason = match (to, upd.blocked_reason.as_deref()) {
            (ItemStatus::Blocked, Some(r)) => {
                Some(check_text("blocked reason", r, MAX_TEXT_CHARS)?)
            }
            (ItemStatus::Blocked, None) => {
                return Err(MissionError::Invalid(
                    "a blocked item needs a reason the owner can act on (what is needed, in one sentence)".into(),
                ));
            }
            _ => None,
        };
        let result = upd
            .result
            .as_deref()
            .filter(|r| !r.trim().is_empty())
            .map(|r| check_text("result", r, MAX_TEXT_CHARS))
            .transpose()?
            .or(cur.result.clone());
        let error = upd
            .error
            .as_deref()
            .filter(|r| !r.trim().is_empty())
            .map(|r| check_text("error", r, MAX_TEXT_CHARS))
            .transpose()?
            .or(cur.last_error.clone());
        let attempts =
            cur.attempts + u32::from(to == ItemStatus::Doing && cur.status != ItemStatus::Doing);
        tx.execute(
            "UPDATE mission_items SET status = ?3, attempts = ?4, result = ?5, last_error = ?6, \
             blocked_reason = ?7, updated_at_ms = ?8 WHERE mission_id = ?1 AND key = ?2",
            params![id, key, to.as_str(), attempts, result, error, reason, now],
        )?;
        if to != cur.status {
            bump(&tx, id, now)?;
            event(
                &tx,
                id,
                now,
                "item",
                Some(key),
                &json!({"from": cur.status.as_str(), "to": to.as_str(), "reason": reason}),
            )?;
        }
        let item = item_in(&tx, id, key)?;
        tx.commit()?;
        Ok(item)
    }

    /// Items, in ledger order, optionally only one status.
    ///
    /// # Errors
    /// SQLite failure.
    pub fn items(&self, id: &str, status: Option<ItemStatus>) -> Result<Vec<Item>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {ITEM_COLS} FROM mission_items WHERE mission_id = ?1 AND (?2 IS NULL OR status = ?2) ORDER BY seq"
        ))?;
        let rows = stmt.query_map(params![id, status.map(ItemStatus::as_str)], item_of)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Headline counts.
    ///
    /// # Errors
    /// SQLite failure.
    pub fn progress(&self, id: &str) -> Result<Progress> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT status, COUNT(*) FROM mission_items WHERE mission_id = ?1 GROUP BY status",
        )?;
        let rows = stmt.query_map([id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, u32>(1)?)))?;
        let mut p = Progress::default();
        for row in rows {
            let (status, n) = row?;
            p.total += n;
            match ItemStatus::parse(&status) {
                Some(ItemStatus::Todo) => p.todo = n,
                Some(ItemStatus::Doing) => p.doing = n,
                Some(ItemStatus::Done) => p.done = n,
                Some(ItemStatus::Blocked) => p.blocked = n,
                Some(ItemStatus::Failed) => p.failed = n,
                Some(ItemStatus::Skipped) => p.skipped = n,
                None => {}
            }
        }
        Ok(p)
    }

    /// The owner answered blocked items: each named item goes back to `todo`
    /// carrying the answer, and a `blocked` mission runs again. Returns how
    /// many items were released.
    ///
    /// # Errors
    /// Unknown mission, SQLite failure.
    pub fn answer_blocked(&self, id: &str, answers: &[(String, String)], now: i64) -> Result<u32> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let mut released = 0;
        for (key, answer) in answers {
            let answer = check_text("answer", answer, MAX_TEXT_CHARS)?;
            let n = tx.execute(
                "UPDATE mission_items SET status = 'todo', owner_answer = ?3, updated_at_ms = ?4 \
                 WHERE mission_id = ?1 AND key = ?2 AND status = 'blocked'",
                params![id, key, answer, now],
            )?;
            if n > 0 {
                released += 1;
                event(
                    &tx,
                    id,
                    now,
                    "item",
                    Some(key),
                    &json!({"from": "blocked", "to": "todo", "by": "owner"}),
                )?;
            }
        }
        if released > 0 {
            bump(&tx, id, now)?;
            let state: String =
                tx.query_row("SELECT state FROM missions WHERE id = ?1", [id], |r| {
                    r.get(0)
                })?;
            if MissionState::parse(&state) == Some(MissionState::Blocked) {
                transition_in(
                    &tx,
                    id,
                    MissionState::Running,
                    Some("the owner answered"),
                    now,
                )?;
            }
        }
        tx.commit()?;
        Ok(released)
    }

    /// A CAPTCHA or 2FA stopped the work on the site the bot is on: park the item
    /// being worked on (the only one `doing`) as blocked, so the session moves on
    /// instead of waiting for the owner. Returns its key; `None` when no single
    /// item is `doing` (the caller then tells the model to park it itself).
    ///
    /// # Errors
    /// SQLite failure.
    pub fn block_sole_doing_item(
        &self,
        id: &str,
        reason: &str,
        now: i64,
    ) -> Result<Option<String>> {
        let doing = self.items(id, Some(ItemStatus::Doing))?;
        let [item] = doing.as_slice() else {
            return Ok(None);
        };
        self.update_item(
            id,
            &item.key,
            &ItemUpdate {
                status: Some(ItemStatus::Blocked),
                blocked_reason: Some(reason.to_owned()),
                ..ItemUpdate::default()
            },
            false,
            now,
        )?;
        Ok(Some(item.key.clone()))
    }

    /// End-of-session housekeeping: an item still `doing` goes back to `todo`
    /// (its attempt was already counted), or to `failed` after `max_attempts`.
    /// Returns `(requeued, gave_up)`.
    ///
    /// # Errors
    /// SQLite failure.
    pub fn requeue_stale_doing(&self, id: &str, max_attempts: u32, now: i64) -> Result<(u32, u32)> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let stale: Vec<(String, u32)> = {
            let mut stmt = tx.prepare(
                "SELECT key, attempts FROM mission_items WHERE mission_id = ?1 AND status = 'doing'",
            )?;
            stmt.query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?
        };
        let (mut requeued, mut gave_up) = (0, 0);
        for (key, attempts) in stale {
            let (to, err) = if attempts >= max_attempts {
                gave_up += 1;
                (
                    "failed",
                    Some(format!(
                        "gave up after {attempts} attempts without a result"
                    )),
                )
            } else {
                requeued += 1;
                ("todo", None)
            };
            tx.execute(
                "UPDATE mission_items SET status = ?3, last_error = COALESCE(?4, last_error), updated_at_ms = ?5 \
                 WHERE mission_id = ?1 AND key = ?2",
                params![id, key, to, err, now],
            )?;
            event(
                &tx,
                id,
                now,
                "item",
                Some(&key),
                &json!({"from": "doing", "to": to, "by": "supervisor"}),
            )?;
        }
        tx.commit()?;
        Ok((requeued, gave_up))
    }

    // ---- notes ----

    /// Write a note (`decision`, `learned`, `warning`). Pinned notes appear in every brief.
    ///
    /// # Errors
    /// Empty text, unknown kind, SQLite failure.
    pub fn add_note(&self, id: &str, kind: &str, text: &str, pinned: bool, now: i64) -> Result<()> {
        if !matches!(kind, "decision" | "learned" | "warning") {
            return Err(MissionError::Invalid(
                "note kind must be decision, learned or warning".into(),
            ));
        }
        let text = check_text("note", text, MAX_NOTE_CHARS)?;
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO mission_notes (mission_id, kind, text, pinned, at_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, kind, text, i64::from(pinned), now],
        )?;
        // Keep the table bounded: drop the oldest unpinned notes beyond the cap.
        tx.execute(
            "DELETE FROM mission_notes WHERE mission_id = ?1 AND pinned = 0 AND id NOT IN \
             (SELECT id FROM mission_notes WHERE mission_id = ?1 AND pinned = 0 ORDER BY id DESC LIMIT ?2)",
            params![id, i64::try_from(MAX_NOTES).unwrap_or(i64::MAX)],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Notes, oldest first; `pinned_only` for the brief.
    ///
    /// # Errors
    /// SQLite failure.
    pub fn notes(&self, id: &str, pinned_only: bool) -> Result<Vec<Note>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT kind, text, pinned, at_ms FROM mission_notes WHERE mission_id = ?1 AND (?2 = 0 OR pinned = 1) ORDER BY id",
        )?;
        let rows = stmt.query_map(params![id, i64::from(pinned_only)], |r| {
            Ok(Note {
                kind: r.get(0)?,
                text: r.get(1)?,
                pinned: r.get::<_, i64>(2)? != 0,
                at_ms: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ---- sessions ----

    /// Start a session: counts it and returns its row id. The session row is
    /// the durable record that a session was in flight (see [`Self::recover_interrupted`]).
    ///
    /// # Errors
    /// Unknown mission, SQLite failure.
    pub fn begin_session(&self, id: &str, now: i64) -> Result<i64> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let n = tx.execute(
            "UPDATE missions SET sessions = sessions + 1, updated_at_ms = ?2 WHERE id = ?1",
            params![id, now],
        )?;
        if n == 0 {
            return Err(MissionError::NotFound(id.to_owned()));
        }
        tx.execute(
            "INSERT INTO mission_sessions (mission_id, started_at_ms) VALUES (?1, ?2)",
            params![id, now],
        )?;
        let sid = tx.last_insert_rowid();
        event(
            &tx,
            id,
            now,
            "session_started",
            None,
            &json!({"session": sid}),
        )?;
        tx.commit()?;
        Ok(sid)
    }

    /// Attach the run to a session row.
    ///
    /// # Errors
    /// SQLite failure.
    pub fn set_session_run(&self, session: i64, run_id: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE mission_sessions SET run_id = ?2 WHERE id = ?1",
            params![session, run_id],
        )?;
        Ok(())
    }

    /// Close a session: adds its steps and spend to the mission and records
    /// whether the ledger moved (`ledger_version` differs from
    /// `version_at_start`). Returns whether it progressed.
    ///
    /// # Errors
    /// Unknown mission, SQLite failure.
    pub fn end_session(
        &self,
        id: &str,
        session: i64,
        version_at_start: i64,
        end: &SessionEndRecord,
        now: i64,
    ) -> Result<bool> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let version: i64 = tx
            .query_row(
                "SELECT ledger_version FROM missions WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| MissionError::NotFound(id.to_owned()))?;
        let progressed = version != version_at_start;
        let cost = micro(end.cost_usd);
        tx.execute(
            "UPDATE mission_sessions SET ended_at_ms = ?2, outcome = ?3, model_steps = ?4, cost_micro_usd = ?5, progressed = ?6 WHERE id = ?1",
            params![session, now, end.outcome, end.model_steps, cost, i64::from(progressed)],
        )?;
        let failed = end.outcome == "failed";
        tx.execute(
            "UPDATE missions SET model_steps = model_steps + ?2, cost_micro_usd = cost_micro_usd + ?3, \
             stalled_sessions = CASE WHEN ?4 THEN 0 ELSE stalled_sessions + 1 END, \
             failed_sessions = CASE WHEN ?5 THEN failed_sessions + 1 ELSE 0 END, \
             lease_run_id = NULL, updated_at_ms = ?6 WHERE id = ?1",
            params![id, end.model_steps, cost, progressed, failed, now],
        )?;
        event(
            &tx,
            id,
            now,
            "session_ended",
            None,
            &json!({"session": session, "outcome": end.outcome, "model_steps": end.model_steps, "progressed": progressed}),
        )?;
        tx.commit()?;
        Ok(progressed)
    }

    /// After a restart: sessions that never ended are `interrupted` (the run
    /// itself is already paused by the run recovery; it is abandoned, not
    /// resumed, because its model context is gone and side effects are never
    /// replayed). Their missions go on with a fresh brief. Returns the run ids
    /// of the abandoned sessions, so the caller can cancel those paused runs
    /// (nobody must be able to resume one next to its replacement).
    ///
    /// # Errors
    /// SQLite failure.
    pub fn recover_interrupted(&self, now: i64) -> Result<Vec<String>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let open: Vec<(i64, String, Option<String>)> = {
            let mut stmt = tx.prepare(
                "SELECT id, mission_id, run_id FROM mission_sessions WHERE ended_at_ms IS NULL",
            )?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<_>>()?
        };
        for (sid, mission, _) in &open {
            tx.execute(
                "UPDATE mission_sessions SET ended_at_ms = ?2, outcome = 'interrupted' WHERE id = ?1",
                params![sid, now],
            )?;
            tx.execute(
                "UPDATE missions SET lease_run_id = NULL WHERE id = ?1",
                [mission],
            )?;
            event(
                &tx,
                mission,
                now,
                "session_ended",
                None,
                &json!({"session": sid, "outcome": "interrupted"}),
            )?;
        }
        tx.commit()?;
        Ok(open.into_iter().filter_map(|(_, _, run)| run).collect())
    }

    // ---- digest and timeline ----

    /// Store the owner digest.
    ///
    /// # Errors
    /// Unknown mission, SQLite failure.
    pub fn set_digest(&self, id: &str, digest: &Digest, now: i64) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let n = tx.execute(
            "UPDATE missions SET digest_json = ?2, updated_at_ms = ?3 WHERE id = ?1",
            params![id, serde_json::to_string(digest)?, now],
        )?;
        if n == 0 {
            return Err(MissionError::NotFound(id.to_owned()));
        }
        event(
            &tx,
            id,
            now,
            "digest",
            None,
            &json!({"outcome": digest.outcome}),
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Timeline rows with `id > after`, oldest first, at most `limit`.
    ///
    /// # Errors
    /// SQLite failure.
    pub fn events(&self, id: &str, after: i64, limit: u32) -> Result<Vec<Event>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, at_ms, kind, item_key, data_json FROM mission_events WHERE mission_id = ?1 AND id > ?2 ORDER BY id LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![id, after, limit], |r| {
            let data: Option<String> = r.get(4)?;
            Ok(Event {
                id: r.get(0)?,
                at_ms: r.get(1)?,
                kind: r.get(2)?,
                item_key: r.get(3)?,
                data: data
                    .and_then(|d| serde_json::from_str(&d).ok())
                    .unwrap_or(Value::Null),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

fn bump(tx: &Transaction<'_>, id: &str, now: i64) -> Result<()> {
    tx.execute(
        "UPDATE missions SET ledger_version = ledger_version + 1, updated_at_ms = ?2 WHERE id = ?1",
        params![id, now],
    )?;
    Ok(())
}

fn item_in(tx: &Transaction<'_>, id: &str, key: &str) -> Result<Item> {
    tx.query_row(
        &format!("SELECT {ITEM_COLS} FROM mission_items WHERE mission_id = ?1 AND key = ?2"),
        params![id, key],
        item_of,
    )
    .optional()?
    .ok_or_else(|| MissionError::NotFound(format!("item {key:?}")))
}

fn transition_in(
    tx: &Transaction<'_>,
    id: &str,
    to: MissionState,
    reason: Option<&str>,
    now: i64,
) -> Result<()> {
    let state: String = tx
        .query_row("SELECT state FROM missions WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .optional()?
        .ok_or_else(|| MissionError::NotFound(id.to_owned()))?;
    let from = MissionState::parse(&state)
        .ok_or_else(|| MissionError::Invalid(format!("stored state {state:?} is unknown")))?;
    if from == to {
        return Ok(());
    }
    if !can_transition(from, to) {
        return Err(MissionError::IllegalTransition { from, to });
    }
    let reason = reason.map(|r| clip(r, MAX_TEXT_CHARS));
    tx.execute(
        "UPDATE missions SET state = ?2, state_reason = ?3, updated_at_ms = ?4, \
         finished_at_ms = CASE WHEN ?5 THEN ?4 ELSE NULL END, \
         stalled_sessions = CASE WHEN ?2 = 'running' THEN 0 ELSE stalled_sessions END, \
         failed_sessions = CASE WHEN ?2 = 'running' THEN 0 ELSE failed_sessions END \
         WHERE id = ?1",
        params![id, to.as_str(), reason, now, to.is_terminal()],
    )?;
    event(
        tx,
        id,
        now,
        "state",
        None,
        &json!({"from": from.as_str(), "to": to.as_str(), "reason": reason}),
    )?;
    Ok(())
}

fn upsert_in(tx: &Transaction<'_>, id: &str, items: &[NewItem], now: i64) -> Result<UpsertReport> {
    let exists: i64 = tx.query_row("SELECT COUNT(*) FROM missions WHERE id = ?1", [id], |r| {
        r.get(0)
    })?;
    if exists == 0 {
        return Err(MissionError::NotFound(id.to_owned()));
    }
    let (mut count, mut seq): (i64, i64) = tx.query_row(
        "SELECT COUNT(*), COALESCE(MAX(seq), 0) FROM mission_items WHERE mission_id = ?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let mut report = UpsertReport::default();
    for it in items {
        let key = check_text("item key", &it.key, MAX_KEY_CHARS)?;
        let title = check_text("item title", &it.title, MAX_TITLE_CHARS)?;
        let kind = check_text("item kind", &it.kind, 40)?;
        let payload = it
            .payload
            .as_ref()
            .filter(|p| !p.is_null())
            .map(|p| {
                let s = p.to_string();
                if s.len() > MAX_PAYLOAD_BYTES {
                    Err(MissionError::Invalid(format!(
                        "item {key:?}: payload is larger than {MAX_PAYLOAD_BYTES} bytes"
                    )))
                } else {
                    Ok(s)
                }
            })
            .transpose()?;
        let taken: i64 = tx.query_row(
            "SELECT COUNT(*) FROM mission_items WHERE mission_id = ?1 AND key = ?2",
            params![id, key],
            |r| r.get(0),
        )?;
        if taken > 0 {
            report.existing += 1;
            continue;
        }
        if usize::try_from(count).unwrap_or(usize::MAX) >= MAX_ITEMS {
            report.dropped += 1;
            continue;
        }
        seq += 1;
        count += 1;
        tx.execute(
            "INSERT INTO mission_items (mission_id, key, seq, kind, title, status, payload_json, updated_at_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, 'todo', ?6, ?7)",
            params![id, key, seq, kind, title, payload, now],
        )?;
        report.added += 1;
    }
    if report.added > 0 {
        bump(tx, id, now)?;
        event(tx, id, now, "plan", None, &json!({"added": report.added}))?;
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_mission(items: &[(&str, &str)]) -> NewMission {
        NewMission {
            user_id: "u".into(),
            bot_id: "b".into(),
            chat_id: "c".into(),
            title: String::new(),
            goal_text: "Register me on backlink sites".into(),
            target_done: None,
            budgets: Budgets::default(),
            items: items
                .iter()
                .map(|(k, t)| NewItem {
                    key: (*k).into(),
                    kind: "site".into(),
                    title: (*t).into(),
                    payload: None,
                })
                .collect(),
        }
    }

    fn store() -> MissionStore {
        MissionStore::open_in_memory().unwrap()
    }

    #[test]
    fn create_seeds_items_and_counts() {
        let s = store();
        let m = s
            .create(
                &new_mission(&[("a.com", "Sign up"), ("b.com", "Sign up")]),
                10,
            )
            .unwrap();
        assert_eq!(m.state, MissionState::Running);
        assert_eq!(m.title, "Register me on backlink sites");
        let p = s.progress(&m.id).unwrap();
        assert_eq!((p.total, p.todo), (2, 2));
        assert!(m.ledger_version > 0);
    }

    #[test]
    fn upsert_dedupes_by_key_and_caps_the_ledger() {
        let s = store();
        let m = s.create(&new_mission(&[("a.com", "x")]), 1).unwrap();
        let r = s
            .upsert_items(
                &m.id,
                &[
                    NewItem {
                        key: "a.com".into(),
                        kind: "site".into(),
                        title: "again".into(),
                        payload: None,
                    },
                    NewItem {
                        key: "b.com".into(),
                        kind: "site".into(),
                        title: "new".into(),
                        payload: None,
                    },
                ],
                2,
            )
            .unwrap();
        assert_eq!(
            r,
            UpsertReport {
                added: 1,
                existing: 1,
                dropped: 0
            }
        );
        let version = s.get(&m.id).unwrap().unwrap().ledger_version;
        let r = s
            .upsert_items(
                &m.id,
                &[NewItem {
                    key: "a.com".into(),
                    kind: "site".into(),
                    title: "x".into(),
                    payload: None,
                }],
                3,
            )
            .unwrap();
        assert_eq!(r.added, 0);
        assert_eq!(
            s.get(&m.id).unwrap().unwrap().ledger_version,
            version,
            "a no-op plan is not progress"
        );
    }

    #[test]
    fn oversized_payload_and_empty_key_are_rejected() {
        let s = store();
        let m = s.create(&new_mission(&[]), 1).unwrap();
        let big = NewItem {
            key: "k".into(),
            kind: "site".into(),
            title: "t".into(),
            payload: Some(json!({"x": "y".repeat(MAX_PAYLOAD_BYTES)})),
        };
        assert!(s.upsert_items(&m.id, &[big], 2).is_err());
        let empty = NewItem {
            key: "  ".into(),
            kind: "site".into(),
            title: "t".into(),
            payload: None,
        };
        assert!(s.upsert_items(&m.id, &[empty], 2).is_err());
    }

    #[test]
    fn blocked_needs_a_reason_and_finished_items_stay_finished() {
        let s = store();
        let m = s.create(&new_mission(&[("a.com", "x")]), 1).unwrap();
        let blocked = ItemUpdate {
            status: Some(ItemStatus::Blocked),
            ..ItemUpdate::default()
        };
        assert!(s.update_item(&m.id, "a.com", &blocked, false, 2).is_err());
        let blocked = ItemUpdate {
            status: Some(ItemStatus::Blocked),
            blocked_reason: Some("CAPTCHA on the sign-up form".into()),
            ..ItemUpdate::default()
        };
        let it = s.update_item(&m.id, "a.com", &blocked, false, 3).unwrap();
        assert_eq!(
            it.blocked_reason.as_deref(),
            Some("CAPTCHA on the sign-up form")
        );
        let done = ItemUpdate {
            status: Some(ItemStatus::Done),
            result: Some("account created".into()),
            ..ItemUpdate::default()
        };
        s.update_item(&m.id, "a.com", &done, false, 4).unwrap();
        let back = ItemUpdate {
            status: Some(ItemStatus::Todo),
            ..ItemUpdate::default()
        };
        assert!(s.update_item(&m.id, "a.com", &back, false, 5).is_err());
        assert!(
            s.update_item(&m.id, "a.com", &back, true, 6).is_ok(),
            "the owner may reopen"
        );
    }

    #[test]
    fn answering_blocked_items_releases_them_and_resumes_the_mission() {
        let s = store();
        let m = s.create(&new_mission(&[("a.com", "x")]), 1).unwrap();
        let blocked = ItemUpdate {
            status: Some(ItemStatus::Blocked),
            blocked_reason: Some("needs the owner's phone number".into()),
            ..ItemUpdate::default()
        };
        s.update_item(&m.id, "a.com", &blocked, false, 2).unwrap();
        s.set_state(&m.id, MissionState::Blocked, Some("1 item needs you"), 3)
            .unwrap();
        let n = s
            .answer_blocked(
                &m.id,
                &[
                    ("a.com".into(), "skip phone sites".into()),
                    ("zzz".into(), "n/a".into()),
                ],
                4,
            )
            .unwrap();
        assert_eq!(n, 1);
        let m = s.get(&m.id).unwrap().unwrap();
        assert_eq!(m.state, MissionState::Running);
        let it = &s.items(&m.id, None).unwrap()[0];
        assert_eq!(it.status, ItemStatus::Todo);
        assert_eq!(it.owner_answer.as_deref(), Some("skip phone sites"));
    }

    #[test]
    fn state_changes_follow_the_table_and_write_the_timeline() {
        let s = store();
        let m = s.create(&new_mission(&[]), 1).unwrap();
        s.set_state(&m.id, MissionState::Done, None, 2).unwrap();
        let err = s
            .set_state(&m.id, MissionState::Running, None, 3)
            .unwrap_err();
        assert!(matches!(err, MissionError::IllegalTransition { .. }));
        let m = s.get(&m.id).unwrap().unwrap();
        assert_eq!(m.finished_at_ms, Some(2));
        let kinds: Vec<_> = s
            .events(&m.id, 0, 50)
            .unwrap()
            .into_iter()
            .map(|e| e.kind)
            .collect();
        assert_eq!(kinds, ["created", "state"]);
    }

    #[test]
    fn sessions_accumulate_and_progress_is_the_ledger_version() {
        let s = store();
        let m = s.create(&new_mission(&[("a.com", "x")]), 1).unwrap();
        let v0 = s.get(&m.id).unwrap().unwrap().ledger_version;
        let sid = s.begin_session(&m.id, 2).unwrap();
        s.set_lease(&m.id, Some("run-1")).unwrap();
        let end = SessionEndRecord {
            outcome: "finished".into(),
            model_steps: 7,
            cost_usd: 0.25,
        };
        assert!(
            !s.end_session(&m.id, sid, v0, &end, 3).unwrap(),
            "no ledger write: stalled"
        );
        let m1 = s.get(&m.id).unwrap().unwrap();
        assert_eq!(
            (m1.sessions, m1.model_steps, m1.stalled_sessions),
            (1, 7, 1)
        );
        assert!((m1.cost_usd - 0.25).abs() < 1e-9);
        assert_eq!(m1.lease_run_id, None);

        let sid = s.begin_session(&m.id, 4).unwrap();
        s.update_item(
            &m.id,
            "a.com",
            &ItemUpdate {
                status: Some(ItemStatus::Done),
                ..ItemUpdate::default()
            },
            false,
            5,
        )
        .unwrap();
        assert!(
            s.end_session(&m.id, sid, m1.ledger_version, &end, 6)
                .unwrap()
        );
        assert_eq!(s.get(&m.id).unwrap().unwrap().stalled_sessions, 0);
    }

    #[test]
    fn failed_sessions_count_in_a_row() {
        let s = store();
        let m = s.create(&new_mission(&[]), 1).unwrap();
        let fail = SessionEndRecord {
            outcome: "failed".into(),
            model_steps: 0,
            cost_usd: 0.0,
        };
        for t in 0..2 {
            let sid = s.begin_session(&m.id, t).unwrap();
            s.end_session(&m.id, sid, 0, &fail, t).unwrap();
        }
        assert_eq!(s.get(&m.id).unwrap().unwrap().failed_sessions, 2);
        let ok = SessionEndRecord {
            outcome: "finished".into(),
            model_steps: 1,
            cost_usd: 0.0,
        };
        let sid = s.begin_session(&m.id, 9).unwrap();
        s.end_session(&m.id, sid, 0, &ok, 9).unwrap();
        assert_eq!(s.get(&m.id).unwrap().unwrap().failed_sessions, 0);
    }

    #[test]
    fn recover_interrupted_closes_open_sessions_and_clears_leases() {
        let s = store();
        let m = s.create(&new_mission(&[]), 1).unwrap();
        s.begin_session(&m.id, 2).unwrap();
        s.set_lease(&m.id, Some("run-x")).unwrap();
        s.set_session_run(1, "run-x").unwrap();
        assert_eq!(s.recover_interrupted(3).unwrap(), vec!["run-x".to_owned()]);
        assert_eq!(s.recover_interrupted(4).unwrap().len(), 0);
        assert_eq!(s.get(&m.id).unwrap().unwrap().lease_run_id, None);
    }

    #[test]
    fn stale_doing_items_are_requeued_then_given_up() {
        let s = store();
        let m = s.create(&new_mission(&[("a.com", "x")]), 1).unwrap();
        let doing = ItemUpdate {
            status: Some(ItemStatus::Doing),
            ..ItemUpdate::default()
        };
        s.update_item(&m.id, "a.com", &doing, false, 2).unwrap();
        assert_eq!(s.requeue_stale_doing(&m.id, 2, 3).unwrap(), (1, 0));
        s.update_item(&m.id, "a.com", &doing, false, 4).unwrap();
        assert_eq!(s.requeue_stale_doing(&m.id, 2, 5).unwrap(), (0, 1));
        let it = &s.items(&m.id, None).unwrap()[0];
        assert_eq!(it.status, ItemStatus::Failed);
        assert_eq!(it.attempts, 2);
    }

    #[test]
    fn only_one_question_batch_per_mission() {
        let s = store();
        let m = s.create(&new_mission(&[]), 1).unwrap();
        assert!(s.take_question_slot(&m.id).unwrap());
        assert!(!s.take_question_slot(&m.id).unwrap());
    }

    #[test]
    fn notes_are_bounded_and_validated() {
        let s = store();
        let m = s.create(&new_mission(&[]), 1).unwrap();
        assert!(s.add_note(&m.id, "gossip", "x", false, 1).is_err());
        for i in 0..(MAX_NOTES + 10) {
            s.add_note(&m.id, "learned", &format!("n{i}"), false, 1)
                .unwrap();
        }
        s.add_note(&m.id, "decision", "use bot@example.com", true, 2)
            .unwrap();
        assert_eq!(s.notes(&m.id, false).unwrap().len(), MAX_NOTES + 1);
        assert_eq!(s.notes(&m.id, true).unwrap().len(), 1);
    }

    #[test]
    fn reopening_the_file_keeps_everything() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missions.db");
        let id = {
            let s = MissionStore::open(&path).unwrap();
            s.create(&new_mission(&[("a.com", "x")]), 1).unwrap().id
        };
        let s = MissionStore::open(&path).unwrap();
        assert_eq!(s.progress(&id).unwrap().total, 1);
    }
}
