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
use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params};
use serde_json::{Value, json};

use crate::error::{MissionError, Result};
use crate::model::{
    Budgets, Digest, Item, ItemStatus, MAX_ITEM_ATTEMPTS, MAX_ITEM_CUTS, MAX_ITEMS,
    MAX_ITEMS_PER_CALL, MAX_KEY_CHARS, MAX_NOTE_CHARS, MAX_NOTES, MAX_PAYLOAD_BYTES,
    MAX_TEXT_CHARS, MAX_TITLE_CHARS, Mission, MissionState, Note, Progress, can_transition, clip,
    looks_like_one_time_code, mentions_code,
};

/// Prefix of the lease a session holds from [`MissionStore::begin_session`] until its run
/// exists (the run id replaces it): the mission is taken while the session waits for the chat.
pub const SESSION_LEASE_PREFIX: &str = "session:";

const MIGRATIONS: &[(&str, &str)] = &[
    (
        "0001_missions",
        include_str!("../migrations/0001_missions.sql"),
    ),
    (
        "0002_item_cuts",
        include_str!("../migrations/0002_item_cuts.sql"),
    ),
    ("0003_wakes", include_str!("../migrations/0003_wakes.sql")),
    (
        "0004_sent_chars",
        include_str!("../migrations/0004_sent_chars.sql"),
    ),
    (
        "0005_handovers",
        include_str!("../migrations/0005_handovers.sql"),
    ),
];

mod handovers;
mod waits;

pub use handovers::{CLAIM_STALE_MS, NewHandover, Park, ParkedHandover, Unclaim};

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
    /// Parked for a step only the owner can do live (see [`Item::live_only`]); only the
    /// runtime sets it, the model's `mission_item` tool never does.
    pub live_only: Option<bool>,
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
    blocked_reason, owner_answer, updated_at_ms, live_only";

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
        live_only: row.get::<_, i64>(11)? != 0,
    })
}

/// Whether the bot (not the owner) may move an item from `from` to `to`.
/// Finished items stay finished; only the owner reopens them.
fn model_item_transition_ok(from: ItemStatus, to: ItemStatus) -> bool {
    use ItemStatus::{Blocked, Doing, Done, Failed, Skipped, Todo, Waiting};
    if from == to {
        return true;
    }
    match from {
        Done | Failed | Skipped => false,
        Todo => matches!(to, Doing | Done | Blocked | Failed | Skipped),
        Doing => matches!(to, Todo | Done | Blocked | Failed | Skipped),
        Blocked => matches!(to, Doing | Done | Failed | Skipped),
        // `mission_wait` parks an item; the bot may take it up again by itself (its wait ends).
        Waiting => matches!(to, Todo | Doing | Done | Blocked | Failed | Skipped),
    }
}

/// Whether the bot (not the owner) may move `cur` to `to`.
fn model_may_move(cur: &Item, to: ItemStatus) -> Result<()> {
    let key = &cur.key;
    // A step only the owner can do on the page (a CAPTCHA, a phone check, a code): the bot
    // cannot take the item up again by itself, the owner's answer or edit releases it.
    if cur.status == ItemStatus::Blocked && cur.live_only && to != cur.status {
        return Err(MissionError::Invalid(format!(
            "item {key:?} waits for the owner to finish a step on the site; only the owner can release it. Do not retry it: take the next item"
        )));
    }
    if !model_item_transition_ok(cur.status, to) {
        return Err(MissionError::Invalid(format!(
            "item {key:?} is {} and cannot become {to}; finished items stay finished",
            cur.status
        )));
    }
    Ok(())
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
        self.create_capped(new, now, None)
    }

    /// [`MissionStore::create`], refusing when the bot already has `max_open` open missions
    /// (`running`, `paused` or `blocked`). The count and the insert share one write
    /// transaction, so two callers racing for the last place cannot both get it.
    ///
    /// # Errors
    /// As [`MissionStore::create`]; [`MissionError::Invalid`] when the cap is reached.
    pub fn create_capped(
        &self,
        new: &NewMission,
        now: i64,
        max_open: Option<usize>,
    ) -> Result<Mission> {
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
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if let Some(max) = max_open {
            let open: i64 = tx.query_row(
                "SELECT COUNT(*) FROM missions WHERE user_id = ?1 AND bot_id = ?2 \
                 AND state IN ('running', 'paused', 'blocked')",
                params![new.user_id, new.bot_id],
                |r| r.get(0),
            )?;
            if usize::try_from(open).unwrap_or(usize::MAX) >= max {
                return Err(MissionError::Invalid(format!(
                    "this bot already has {max} missions open; the owner has to finish or stop one first"
                )));
            }
        }
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

    /// The mission whose chat is `chat_id` (its sessions and digest live there), if any.
    ///
    /// # Errors
    /// SQLite failure.
    pub fn mission_for_chat(&self, chat_id: &str) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT id FROM missions WHERE chat_id = ?1 LIMIT 1",
                [chat_id],
                |r| r.get(0),
            )
            .optional()?)
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

    /// Count `chars` characters sent to places the owner did not name against the mission's
    /// ceiling of `ceiling` (all sessions together). `true` and counted when the total stays at
    /// or below it; `false` and nothing counted otherwise, or when the mission does not exist.
    /// One statement, so two calls at once cannot both take the last of the budget.
    ///
    /// # Errors
    /// SQLite failure.
    pub fn take_send_budget(&self, id: &str, chars: u64, ceiling: u64) -> Result<bool> {
        let (Ok(chars), Ok(ceiling)) = (i64::try_from(chars), i64::try_from(ceiling)) else {
            return Ok(false);
        };
        let n = self.conn().execute(
            "UPDATE missions SET sent_chars = sent_chars + ?2 WHERE id = ?1 AND sent_chars + ?2 <= ?3",
            params![id, chars, ceiling],
        )?;
        Ok(n == 1)
    }

    /// Characters the mission's sessions have sent so far (see [`Self::take_send_budget`]).
    ///
    /// # Errors
    /// SQLite failure.
    pub fn sent_chars(&self, id: &str) -> Result<u64> {
        let n: Option<i64> = self
            .conn()
            .query_row("SELECT sent_chars FROM missions WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(u64::try_from(n.unwrap_or(0)).unwrap_or(0))
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
        if to == ItemStatus::Waiting && cur.status != ItemStatus::Waiting {
            return Err(MissionError::Invalid(
                "an item is parked for a wake only through mission_wait (a timer, a mail or a webhook)".into(),
            ));
        }
        if !by_owner {
            model_may_move(&cur, to)?;
        }
        if !by_owner
            && to == ItemStatus::Doing
            && cur.status != ItemStatus::Doing
            && cur.attempts >= MAX_ITEM_ATTEMPTS
        {
            return Err(MissionError::Invalid(format!(
                "item {key:?} was started {} times without a result; do not start it again: mark it failed (with the error) or blocked (with what the owner has to do)",
                cur.attempts
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
        // The flag lives only while the item stays parked.
        let live_only = to == ItemStatus::Blocked && upd.live_only.unwrap_or(cur.live_only);
        tx.execute(
            "UPDATE mission_items SET status = ?3, attempts = ?4, result = ?5, last_error = ?6, \
             blocked_reason = ?7, live_only = ?8, updated_at_ms = ?9 WHERE mission_id = ?1 AND key = ?2",
            params![
                id,
                key,
                to.as_str(),
                attempts,
                result,
                error,
                reason,
                i64::from(live_only),
                now
            ],
        )?;
        if to != cur.status {
            // The item left its wait: nobody watches it any more. A wake that was meant to
            // continue the item is delivered until it is settled or parked again.
            if cur.status == ItemStatus::Waiting {
                waits::cancel_item_waits(&tx, id, key, now)?;
            }
            if !matches!(to, ItemStatus::Todo | ItemStatus::Doing) {
                waits::consume_item_wakes(&tx, id, key, now)?;
            }
            // Only a move that settles an item counts as progress. Flipping an item between
            // `todo` and `doing` must not reset the stalled-session count.
            if by_owner
                || matches!(
                    to,
                    ItemStatus::Done
                        | ItemStatus::Failed
                        | ItemStatus::Skipped
                        | ItemStatus::Blocked
                )
            {
                bump(&tx, id, now)?;
            }
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
                Some(ItemStatus::Waiting) => p.waiting = n,
                Some(ItemStatus::Failed) => p.failed = n,
                Some(ItemStatus::Skipped) => p.skipped = n,
                None => {}
            }
        }
        Ok(p)
    }

    /// The owner answered blocked items: each named item goes back to `todo`
    /// carrying the answer (with a fresh attempt budget: the answer is new information),
    /// and a `blocked` mission runs again. Returns how many items were released.
    ///
    /// # Errors
    /// Unknown mission, SQLite failure.
    pub fn answer_blocked(&self, id: &str, answers: &[(String, String)], now: i64) -> Result<u32> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        // Everything is checked before anything changes: a refused answer changes nothing.
        let mut checked = Vec::with_capacity(answers.len());
        for (key, answer) in answers {
            let answer = check_text("answer", answer, MAX_TEXT_CHARS)?;
            let item = match item_in(&tx, id, key) {
                Ok(item) => Some(item),
                Err(MissionError::NotFound(_)) => None,
                Err(e) => return Err(e),
            };
            if let Some(item) = item.filter(|i| i.status == ItemStatus::Blocked) {
                if item.live_only {
                    return Err(MissionError::Invalid(format!(
                        "item {key:?} waits for a step you have to do yourself on the site: a code is never stored or shown to the bot. Finish that step there, then skip the item here"
                    )));
                }
                if item.blocked_reason.as_deref().is_some_and(mentions_code)
                    && looks_like_one_time_code(&answer)
                {
                    return Err(MissionError::Invalid(format!(
                        "item {key:?}: that looks like a one-time code. A code is never stored or shown to the bot: enter it on the site yourself, then skip the item here, or answer with something other than the code"
                    )));
                }
            }
            checked.push((key, answer));
        }
        let mut released = 0;
        for (key, answer) in checked {
            let n = tx.execute(
                "UPDATE mission_items SET status = 'todo', owner_answer = ?3, live_only = 0, attempts = 0, cuts = 0, updated_at_ms = ?4 \
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
            if matches!(
                MissionState::parse(&state),
                Some(MissionState::Blocked | MissionState::Waiting)
            ) {
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
    /// `live_only` marks a step the owner has to do live (a code): the item takes no
    /// typed answer (see [`Item::live_only`]).
    ///
    /// # Errors
    /// SQLite failure.
    pub fn block_sole_doing_item(
        &self,
        id: &str,
        reason: &str,
        live_only: bool,
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
                live_only: Some(live_only),
                ..ItemUpdate::default()
            },
            false,
            now,
        )?;
        Ok(Some(item.key.clone()))
    }

    /// End-of-session housekeeping: an item still `doing` goes back to `todo`. What it costs
    /// depends on how the session ended (see [`Leftover`]): a session the model ended itself
    /// with the item unfinished keeps the attempt it counted (and gives the item up after
    /// `max_attempts`); a session the step guard cut, or an interruption, refunds it.
    /// Returns `(requeued, gave_up)`.
    ///
    /// # Errors
    /// SQLite failure.
    pub fn requeue_stale_doing(
        &self,
        id: &str,
        max_attempts: u32,
        left: Leftover,
        now: i64,
    ) -> Result<(u32, u32)> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let counts = requeue_in(&tx, id, max_attempts, left, now)?;
        tx.commit()?;
        Ok(counts)
    }

    /// A CAPTCHA stopped the work on the site the bot is on and nobody can solve it (a mission
    /// has no live hand-over, and a typed answer does not pass a CAPTCHA): give the item being
    /// worked on up with the reason, instead of parking it for an owner who could only skip it.
    /// Returns its key; `None` when no single item is `doing`.
    ///
    /// # Errors
    /// SQLite failure.
    pub fn give_up_sole_doing_item(
        &self,
        id: &str,
        error: &str,
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
                status: Some(ItemStatus::Failed),
                error: Some(error.to_owned()),
                ..ItemUpdate::default()
            },
            false,
            now,
        )?;
        Ok(Some(item.key.clone()))
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

    /// Start a session: counts it, takes the mission's lease and returns the session row id.
    /// It only succeeds for a `running` mission that no session holds, in one transaction,
    /// so two supervisors can never run the same mission at once. The lease is a
    /// [`SESSION_LEASE_PREFIX`] placeholder until [`Self::set_lease`] records the run id;
    /// [`Self::end_session`] and [`Self::recover_interrupted`] release it. The session row is
    /// the durable record that a session was in flight (see [`Self::recover_interrupted`]).
    ///
    /// # Errors
    /// Unknown mission, [`MissionError::Busy`] when the mission is not `running` or already
    /// has a session, SQLite failure.
    pub fn begin_session(&self, id: &str, now: i64) -> Result<i64> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: i64 =
            tx.query_row("SELECT COUNT(*) FROM missions WHERE id = ?1", [id], |r| {
                r.get(0)
            })?;
        if exists == 0 {
            return Err(MissionError::NotFound(id.to_owned()));
        }
        tx.execute(
            "INSERT INTO mission_sessions (mission_id, started_at_ms) VALUES (?1, ?2)",
            params![id, now],
        )?;
        let sid = tx.last_insert_rowid();
        let n = tx.execute(
            "UPDATE missions SET sessions = sessions + 1, lease_run_id = ?3, updated_at_ms = ?2 \
             WHERE id = ?1 AND state = 'running' AND lease_run_id IS NULL",
            params![id, now, format!("{SESSION_LEASE_PREFIX}{sid}")],
        )?;
        if n == 0 {
            // Dropping the transaction rolls the session row back.
            return Err(MissionError::Busy(id.to_owned()));
        }
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
    /// replayed). Their missions go on with a fresh brief, and an item the
    /// crash left `doing` is requeued (or given up after [`MAX_ITEM_ATTEMPTS`]).
    /// Returns the run ids of the abandoned sessions, so the caller can cancel
    /// those paused runs (nobody must be able to resume one next to its
    /// replacement).
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
        let mut missions: Vec<&str> = Vec::new();
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
            if !missions.contains(&mission.as_str()) {
                missions.push(mission);
            }
        }
        for mission in missions {
            requeue_in(&tx, mission, MAX_ITEM_ATTEMPTS, Leftover::Interrupted, now)?;
        }
        tx.commit()?;
        Ok(open.into_iter().filter_map(|(_, _, run)| run).collect())
    }

    // ---- digest and timeline ----

    /// Store the owner digest unless this outcome already has one: the first of two racing
    /// callers (the owner's stop and the session winding down) writes it and gets `true`, the
    /// other gets `false` and must not announce it again. A mission that runs again forgets
    /// its digest (see [`MissionStore::set_state`]), so its next end gets a fresh one.
    ///
    /// # Errors
    /// Unknown mission, SQLite failure.
    pub fn set_digest_once(&self, id: &str, digest: &Digest, now: i64) -> Result<bool> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: Option<String> = tx
            .query_row(
                "SELECT digest_json FROM missions WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| MissionError::NotFound(id.to_owned()))?;
        let same_outcome = current
            .and_then(|d| serde_json::from_str::<Digest>(&d).ok())
            .is_some_and(|d| d.outcome == digest.outcome);
        if same_outcome {
            return Ok(false);
        }
        tx.execute(
            "UPDATE missions SET digest_json = ?2, updated_at_ms = ?3 WHERE id = ?1",
            params![id, serde_json::to_string(digest)?, now],
        )?;
        event(
            &tx,
            id,
            now,
            "digest",
            None,
            &json!({"outcome": digest.outcome}),
        )?;
        tx.commit()?;
        Ok(true)
    }

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

/// How a session ended while an item was still `doing`: what the leftover costs the item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Leftover {
    /// The model ended the session itself and left the item unfinished: the attempt counts.
    Abandoned,
    /// The step guard cut the session. That is how a mission gets past the 40-call wall of one
    /// run, so the attempt is refunded. A separate cap ([`MAX_ITEM_CUTS`]) still gives up an
    /// item that eats whole sessions without being settled.
    Cut,
    /// The session was cancelled, paused, crashed or cut off by a restart or a provider
    /// error: not the item's fault, the attempt is refunded. It counts as a cut all the same
    /// ([`MAX_ITEM_CUTS`]), so an item that keeps ending its sessions this way is given up.
    Interrupted,
}

/// [`MissionStore::requeue_stale_doing`] inside a transaction. Giving an item up settles it,
/// so it counts as progress.
fn requeue_in(
    tx: &Transaction<'_>,
    id: &str,
    max_attempts: u32,
    left: Leftover,
    now: i64,
) -> Result<(u32, u32)> {
    let stale: Vec<(String, u32, u32)> = {
        let mut stmt = tx.prepare(
            "SELECT key, attempts, cuts FROM mission_items WHERE mission_id = ?1 AND status = 'doing'",
        )?;
        stmt.query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    let (mut requeued, mut gave_up) = (0, 0);
    for (key, attempts, cuts) in stale {
        let (attempts, cuts) = match left {
            Leftover::Abandoned => (attempts, cuts),
            // A cut, and an interruption too: not the item's fault, so the attempt is refunded,
            // but both count toward the cuts. Otherwise a session that dies the same way every
            // time (a crash on this item, a provider error, a pause loop) would be refunded for
            // ever and never reach the cap.
            Leftover::Cut | Leftover::Interrupted => (attempts.saturating_sub(1), cuts + 1),
        };
        let abandoned = left == Leftover::Abandoned;
        let (to, err) =
            if (abandoned && attempts >= max_attempts) || (!abandoned && cuts >= MAX_ITEM_CUTS) {
                gave_up += 1;
                let why = if abandoned {
                    format!("gave up after {attempts} attempts without a result")
                } else {
                    format!("gave up: {cuts} whole sessions were not enough to finish it")
                };
                ("failed", Some(why))
            } else {
                requeued += 1;
                ("todo", None)
            };
        tx.execute(
            "UPDATE mission_items SET status = ?3, attempts = ?4, cuts = ?5, \
             last_error = COALESCE(?6, last_error), updated_at_ms = ?7 \
             WHERE mission_id = ?1 AND key = ?2",
            params![id, key, to, attempts, cuts, err, now],
        )?;
        if to == "failed" {
            waits::consume_item_wakes(tx, id, &key, now)?;
        }
        event(
            tx,
            id,
            now,
            "item",
            Some(&key),
            &json!({"from": "doing", "to": to, "by": "supervisor"}),
        )?;
    }
    if gave_up > 0 {
        bump(tx, id, now)?;
    }
    Ok((requeued, gave_up))
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
         failed_sessions = CASE WHEN ?2 = 'running' THEN 0 ELSE failed_sessions END, \
         digest_json = CASE WHEN ?2 = 'running' THEN NULL ELSE digest_json END \
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
    if to.is_terminal() {
        waits::cancel_all_waits(tx, id, now)?;
    }
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
    fn an_answered_item_gets_a_fresh_attempt_budget() {
        let s = store();
        let m = s.create(&new_mission(&[("a.com", "x")]), 1).unwrap();
        let go = |to: ItemStatus, t: i64| {
            let upd = ItemUpdate {
                status: Some(to),
                blocked_reason: (to == ItemStatus::Blocked).then(|| "needs a phone number".into()),
                ..ItemUpdate::default()
            };
            s.update_item(&m.id, "a.com", &upd, false, t)
        };
        // Started and given back until the cap is reached, then parked for the owner.
        for t in 0..MAX_ITEM_ATTEMPTS {
            go(ItemStatus::Doing, i64::from(t) * 2 + 2).unwrap();
            go(ItemStatus::Todo, i64::from(t) * 2 + 3).unwrap();
        }
        assert!(go(ItemStatus::Doing, 20).is_err(), "the cap holds");
        go(ItemStatus::Blocked, 21).unwrap();
        s.answer_blocked(&m.id, &[("a.com".into(), "use +49 30 1234567".into())], 22)
            .unwrap();
        let it = &s.items(&m.id, None).unwrap()[0];
        assert_eq!((it.status, it.attempts), (ItemStatus::Todo, 0));
        go(ItemStatus::Doing, 23).expect("the owner's answer reopens the item for the bot");
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
        let abandoned = Leftover::Abandoned;
        assert_eq!(
            s.requeue_stale_doing(&m.id, 2, abandoned, 3).unwrap(),
            (1, 0)
        );
        s.update_item(&m.id, "a.com", &doing, false, 4).unwrap();
        assert_eq!(
            s.requeue_stale_doing(&m.id, 2, abandoned, 5).unwrap(),
            (0, 1)
        );
        let it = &s.items(&m.id, None).unwrap()[0];
        assert_eq!(it.status, ItemStatus::Failed);
        assert_eq!(it.attempts, 2);
    }

    #[test]
    fn a_cut_or_interrupted_session_refunds_the_attempt() {
        let s = store();
        let m = s.create(&new_mission(&[("a.com", "x")]), 1).unwrap();
        let doing = ItemUpdate {
            status: Some(ItemStatus::Doing),
            ..ItemUpdate::default()
        };
        // Cut: no attempt is burnt, however often, until the separate cut cap.
        for cut in 1..MAX_ITEM_CUTS {
            s.update_item(&m.id, "a.com", &doing, false, 2).unwrap();
            assert_eq!(
                s.requeue_stale_doing(&m.id, 1, Leftover::Cut, 3).unwrap(),
                (1, 0),
                "cut {cut}"
            );
            let it = &s.items(&m.id, None).unwrap()[0];
            assert_eq!((it.status, it.attempts), (ItemStatus::Todo, 0));
        }
        // An interruption refunds the attempt too, but counts toward the cuts.
        s.update_item(&m.id, "a.com", &doing, false, 4).unwrap();
        assert_eq!(
            s.requeue_stale_doing(&m.id, 1, Leftover::Interrupted, 5)
                .unwrap(),
            (0, 1),
            "the 4th whole session (3 cuts + 1 interruption) gives the item up"
        );
        let it = &s.items(&m.id, None).unwrap()[0];
        assert_eq!(it.status, ItemStatus::Failed);
        assert!(it.last_error.as_deref().unwrap().contains("whole sessions"));
    }

    #[test]
    fn repeated_interruptions_cannot_refund_for_ever() {
        let s = store();
        let m = s.create(&new_mission(&[("a.com", "x")]), 1).unwrap();
        let doing = ItemUpdate {
            status: Some(ItemStatus::Doing),
            ..ItemUpdate::default()
        };
        for n in 1..MAX_ITEM_CUTS {
            s.update_item(&m.id, "a.com", &doing, false, 2).unwrap();
            assert_eq!(
                s.requeue_stale_doing(&m.id, 3, Leftover::Interrupted, 3)
                    .unwrap(),
                (1, 0),
                "interruption {n}"
            );
            assert_eq!(s.items(&m.id, None).unwrap()[0].attempts, 0);
        }
        s.update_item(&m.id, "a.com", &doing, false, 4).unwrap();
        assert_eq!(
            s.requeue_stale_doing(&m.id, 3, Leftover::Interrupted, 5)
                .unwrap(),
            (0, 1)
        );
        assert_eq!(s.items(&m.id, None).unwrap()[0].status, ItemStatus::Failed);
    }

    #[test]
    fn the_open_mission_cap_is_enforced_inside_create() {
        let s = store();
        for i in 0..3 {
            let mut nm = new_mission(&[]);
            nm.title = format!("m{i}");
            s.create_capped(&nm, 1, Some(3)).unwrap();
        }
        let err = s.create_capped(&new_mission(&[]), 2, Some(3)).unwrap_err();
        assert!(err.to_string().contains("3 missions open"), "{err}");
        // Another bot has its own places; an uncapped create is unchanged.
        let mut other = new_mission(&[]);
        other.bot_id = "other".into();
        s.create_capped(&other, 3, Some(3)).unwrap();
        s.create(&new_mission(&[]), 4).unwrap();
        // A finished mission frees its place.
        let first = s
            .list("u")
            .unwrap()
            .into_iter()
            .find(|m| m.bot_id == "b")
            .unwrap();
        s.set_state(&first.id, MissionState::Cancelled, None, 5)
            .unwrap();
        s.create_capped(&new_mission(&[]), 6, Some(4)).unwrap();
    }

    #[test]
    fn racing_creates_cannot_pass_the_open_mission_cap() {
        let s = std::sync::Arc::new(store());
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let s = s.clone();
                std::thread::spawn(move || s.create_capped(&new_mission(&[]), 1, Some(3)).is_ok())
            })
            .collect();
        let made = handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .filter(|ok| *ok)
            .count();
        assert_eq!(made, 3);
        assert_eq!(s.list("u").unwrap().len(), 3);
    }

    #[test]
    fn the_owners_answer_gives_an_item_fresh_attempts_and_cuts() {
        let s = store();
        let m = s.create(&new_mission(&[("a.com", "x")]), 1).unwrap();
        let doing = ItemUpdate {
            status: Some(ItemStatus::Doing),
            ..ItemUpdate::default()
        };
        s.update_item(&m.id, "a.com", &doing, false, 2).unwrap();
        s.requeue_stale_doing(&m.id, 3, Leftover::Cut, 3).unwrap();
        s.update_item(&m.id, "a.com", &doing, false, 4).unwrap();
        s.update_item(
            &m.id,
            "a.com",
            &ItemUpdate {
                status: Some(ItemStatus::Blocked),
                blocked_reason: Some("which address?".into()),
                ..ItemUpdate::default()
            },
            false,
            5,
        )
        .unwrap();
        s.answer_blocked(&m.id, &[("a.com".into(), "use b@x.org".into())], 6)
            .unwrap();
        let cuts: u32 = s
            .conn()
            .query_row("SELECT cuts FROM mission_items", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cuts, 0);
    }

    fn status(
        s: &MissionStore,
        id: &str,
        key: &str,
        st: ItemStatus,
        by_owner: bool,
    ) -> Result<Item> {
        s.update_item(
            id,
            key,
            &ItemUpdate {
                status: Some(st),
                blocked_reason: (st == ItemStatus::Blocked).then(|| "needs a look".to_owned()),
                ..ItemUpdate::default()
            },
            by_owner,
            9,
        )
    }

    #[test]
    fn begin_session_is_a_compare_and_swap_on_the_lease() {
        let s = store();
        let m = s.create(&new_mission(&[]), 1).unwrap();
        let sid = s.begin_session(&m.id, 2).unwrap();
        let held = s.get(&m.id).unwrap().unwrap();
        assert_eq!(
            held.lease_run_id.as_deref(),
            Some(format!("{SESSION_LEASE_PREFIX}{sid}").as_str()),
            "the session holds the mission before its run exists"
        );
        assert!(matches!(
            s.begin_session(&m.id, 3),
            Err(MissionError::Busy(_))
        ));
        let m1 = s.get(&m.id).unwrap().unwrap();
        assert_eq!(m1.sessions, 1, "the refused session is not counted");
        let rows: i64 = s
            .conn()
            .query_row("SELECT COUNT(*) FROM mission_sessions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1, "and leaves no session row behind");
        // The run id replaces the placeholder; ending the session frees the mission.
        s.set_lease(&m.id, Some("run-1")).unwrap();
        assert!(matches!(
            s.begin_session(&m.id, 3),
            Err(MissionError::Busy(_))
        ));
        let end = SessionEndRecord {
            outcome: "finished".into(),
            model_steps: 1,
            cost_usd: 0.0,
        };
        s.end_session(&m.id, sid, 0, &end, 4).unwrap();
        assert!(s.begin_session(&m.id, 5).is_ok());
    }

    #[test]
    fn begin_session_needs_a_running_mission() {
        let s = store();
        let m = s.create(&new_mission(&[]), 1).unwrap();
        s.set_state(&m.id, MissionState::Paused, None, 2).unwrap();
        assert!(matches!(
            s.begin_session(&m.id, 3),
            Err(MissionError::Busy(_))
        ));
        assert!(matches!(
            s.begin_session("nope", 3),
            Err(MissionError::NotFound(_))
        ));
        assert_eq!(s.get(&m.id).unwrap().unwrap().sessions, 0);
    }

    #[test]
    fn a_mission_is_found_by_its_chat() {
        let s = store();
        let m = s.create(&new_mission(&[]), 1).unwrap();
        assert_eq!(s.mission_for_chat("c").unwrap(), Some(m.id));
        assert_eq!(s.mission_for_chat("other").unwrap(), None);
    }

    #[test]
    fn flipping_todo_and_doing_is_not_progress_and_attempts_are_capped() {
        let s = store();
        let m = s.create(&new_mission(&[("a.com", "x")]), 1).unwrap();
        let v0 = s.get(&m.id).unwrap().unwrap().ledger_version;
        for _ in 0..MAX_ITEM_ATTEMPTS {
            status(&s, &m.id, "a.com", ItemStatus::Doing, false).unwrap();
            status(&s, &m.id, "a.com", ItemStatus::Todo, false).unwrap();
        }
        assert_eq!(
            s.get(&m.id).unwrap().unwrap().ledger_version,
            v0,
            "starting and putting back is no progress"
        );
        let err = status(&s, &m.id, "a.com", ItemStatus::Doing, false).unwrap_err();
        assert!(
            err.to_string().contains("failed") && err.to_string().contains("blocked"),
            "{err}"
        );
        // The model can still settle it; the owner can always reopen it.
        let it = status(&s, &m.id, "a.com", ItemStatus::Failed, false).unwrap();
        assert_eq!(it.attempts, MAX_ITEM_ATTEMPTS);
        assert!(s.get(&m.id).unwrap().unwrap().ledger_version > v0);
        let it = status(&s, &m.id, "a.com", ItemStatus::Doing, true).unwrap();
        assert_eq!(it.status, ItemStatus::Doing);
    }

    #[test]
    fn settling_an_item_is_progress() {
        let s = store();
        let m = s
            .create(
                &new_mission(&[
                    ("a.com", "x"),
                    ("b.com", "x"),
                    ("c.com", "x"),
                    ("d.com", "x"),
                ]),
                1,
            )
            .unwrap();
        for (key, st) in [
            ("a.com", ItemStatus::Done),
            ("b.com", ItemStatus::Blocked),
            ("c.com", ItemStatus::Failed),
            ("d.com", ItemStatus::Skipped),
        ] {
            let before = s.get(&m.id).unwrap().unwrap().ledger_version;
            status(&s, &m.id, key, st, false).unwrap();
            assert!(
                s.get(&m.id).unwrap().unwrap().ledger_version > before,
                "{st}"
            );
        }
    }

    #[test]
    fn recover_requeues_items_a_crash_left_doing() {
        let s = store();
        let m = s
            .create(&new_mission(&[("a.com", "x"), ("b.com", "x")]), 1)
            .unwrap();
        // a.com was started for the third time, b.com for the first.
        for _ in 0..MAX_ITEM_ATTEMPTS {
            status(&s, &m.id, "a.com", ItemStatus::Doing, false).unwrap();
            if s.items(&m.id, None).unwrap()[0].attempts < MAX_ITEM_ATTEMPTS {
                status(&s, &m.id, "a.com", ItemStatus::Todo, false).unwrap();
            }
        }
        status(&s, &m.id, "b.com", ItemStatus::Doing, false).unwrap();
        s.begin_session(&m.id, 2).unwrap();
        s.recover_interrupted(3).unwrap();
        let items = s.items(&m.id, None).unwrap();
        // A crash is not the item's fault: the attempt is refunded.
        assert_eq!(items[0].status, ItemStatus::Todo, "attempt refunded");
        assert_eq!(items[0].attempts, MAX_ITEM_ATTEMPTS - 1);
        assert_eq!(items[1].status, ItemStatus::Todo, "worked again");
        // A mission without an open session is left alone.
        status(&s, &m.id, "b.com", ItemStatus::Doing, false).unwrap();
        s.recover_interrupted(4).unwrap();
        assert_eq!(s.items(&m.id, None).unwrap()[1].status, ItemStatus::Doing);
    }

    #[test]
    fn a_code_is_never_taken_as_an_answer() {
        let s = store();
        let m = s
            .create(
                &new_mission(&[("a.com", "x"), ("b.com", "x"), ("c.com", "x")]),
                1,
            )
            .unwrap();
        for key in ["a.com", "b.com", "c.com"] {
            status(&s, &m.id, key, ItemStatus::Doing, false).unwrap();
        }
        // a.com: parked by a code handover (live only); b.com: the model wrote a code reason;
        // c.com: an ordinary question.
        s.update_item(
            &m.id,
            "a.com",
            &ItemUpdate {
                status: Some(ItemStatus::Blocked),
                blocked_reason: Some("a 2FA code for a.com".into()),
                live_only: Some(true),
                ..ItemUpdate::default()
            },
            false,
            2,
        )
        .unwrap();
        s.update_item(
            &m.id,
            "b.com",
            &ItemUpdate {
                status: Some(ItemStatus::Blocked),
                blocked_reason: Some("needs the SMS verification code".into()),
                ..ItemUpdate::default()
            },
            false,
            2,
        )
        .unwrap();
        s.update_item(
            &m.id,
            "c.com",
            &ItemUpdate {
                status: Some(ItemStatus::Blocked),
                blocked_reason: Some("which phone number?".into()),
                ..ItemUpdate::default()
            },
            false,
            2,
        )
        .unwrap();
        assert!(s.items(&m.id, None).unwrap()[0].live_only);

        let live = s
            .answer_blocked(&m.id, &[("a.com".into(), "the second address".into())], 3)
            .unwrap_err();
        assert!(live.to_string().contains("never stored"), "{live}");
        let code = s
            .answer_blocked(&m.id, &[("b.com".into(), "code is 123456".into())], 3)
            .unwrap_err();
        assert!(code.to_string().contains("one-time code"), "{code}");

        // One refused answer changes nothing, not even the good ones next to it.
        let both = s.answer_blocked(
            &m.id,
            &[
                ("c.com".into(), "555-0100".into()),
                ("b.com".into(), "123 456".into()),
            ],
            3,
        );
        assert!(both.is_err());
        assert_eq!(s.progress(&m.id).unwrap().blocked, 3);

        // An answer that is not a code goes through, also to an item that mentions a code.
        assert_eq!(
            s.answer_blocked(
                &m.id,
                &[
                    ("c.com".into(), "123456".into()),
                    (
                        "b.com".into(),
                        "skip the SMS step, use the e-mail link".into()
                    ),
                ],
                4
            )
            .unwrap(),
            2
        );
        // The owner finishes the live step on the site and skips the item.
        let it = status(&s, &m.id, "a.com", ItemStatus::Skipped, true).unwrap();
        assert!(!it.live_only, "the flag does not outlive the parking");
    }

    #[test]
    fn only_one_question_batch_per_mission() {
        let s = store();
        let m = s.create(&new_mission(&[]), 1).unwrap();
        assert!(s.take_question_slot(&m.id).unwrap());
        assert!(!s.take_question_slot(&m.id).unwrap());
    }

    #[test]
    fn the_send_ceiling_is_one_total_for_the_mission_kept_in_its_row() {
        let s = store();
        let m = s.create(&new_mission(&[]), 1).unwrap();
        let other = s.create(&new_mission(&[]), 1).unwrap();
        assert_eq!(s.sent_chars(&m.id).unwrap(), 0);
        assert!(s.take_send_budget(&m.id, 600, 1000).unwrap());
        // Over the ceiling counts nothing; what is left can still be taken.
        assert!(!s.take_send_budget(&m.id, 401, 1000).unwrap());
        assert_eq!(s.sent_chars(&m.id).unwrap(), 600);
        assert!(s.take_send_budget(&m.id, 400, 1000).unwrap());
        assert!(!s.take_send_budget(&m.id, 1, 1000).unwrap());
        // Another mission and an unknown one are not touched by it.
        assert_eq!(s.sent_chars(&other.id).unwrap(), 0);
        assert!(!s.take_send_budget("no-such-mission", 1, 1000).unwrap());
        // Sessions do not reset it: it is a column of the mission, not of a session.
        let session = s.begin_session(&m.id, 5).unwrap();
        s.set_session_run(session, "run-2").unwrap();
        assert_eq!(s.sent_chars(&m.id).unwrap(), 1000);
    }

    #[test]
    fn the_send_total_survives_reopening_the_database() {
        let dir = std::env::temp_dir().join(format!("missions-sent-{}", uuid::Uuid::now_v7()));
        let path = dir.join("missions.db");
        let id = {
            let s = MissionStore::open(&path).unwrap();
            let m = s.create(&new_mission(&[]), 1).unwrap();
            assert!(s.take_send_budget(&m.id, 7, 10).unwrap());
            m.id
        };
        let s = MissionStore::open(&path).unwrap();
        assert_eq!(s.sent_chars(&id).unwrap(), 7);
        assert!(!s.take_send_budget(&id, 4, 10).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
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

    fn park(s: &MissionStore, id: &str, call: &str, expires: i64, cap: u32) -> Park {
        s.park_live_handover(
            id,
            &NewHandover {
                call_id: call,
                bot_id: "b",
                run_id: "00000000-0000-0000-0000-000000000001",
                kind: "captcha",
                reason: "needs you on this page: a CAPTCHA on https://a.com",
                expires_at_ms: expires,
            },
            cap,
            10,
        )
        .unwrap()
    }

    fn item(s: &MissionStore, id: &str, key: &str) -> Item {
        s.items(id, None)
            .unwrap()
            .into_iter()
            .find(|i| i.key == key)
            .unwrap()
    }

    #[test]
    fn a_parked_handover_is_claimed_once_and_only_the_owner_moves_its_item() {
        let s = store();
        let m = s
            .create(&new_mission(&[("a.com", "A"), ("b.com", "B")]), 1)
            .unwrap();
        assert_eq!(park(&s, &m.id, "call-1", 1000, 1), Park::NoSoleItem);
        status(&s, &m.id, "a.com", ItemStatus::Doing, false).unwrap();
        assert_eq!(
            park(&s, &m.id, "call-1", 1000, 1),
            Park::Parked("a.com".into())
        );
        let a = item(&s, &m.id, "a.com");
        assert_eq!(a.status, ItemStatus::Blocked);
        assert!(a.live_only);
        // The bot cannot take the item up again, finish it or give it up by itself.
        for to in [
            ItemStatus::Todo,
            ItemStatus::Doing,
            ItemStatus::Done,
            ItemStatus::Failed,
            ItemStatus::Skipped,
        ] {
            let e = status(&s, &m.id, "a.com", to, false).unwrap_err();
            assert!(e.to_string().contains("only the owner"), "{to}: {e}");
        }
        // One answer claims it; a second answer, or the claim after the expiry, gets nothing.
        assert!(s.claim_live_handover("call-1", 20).unwrap().is_some());
        assert!(s.claim_live_handover("call-1", 21).unwrap().is_none());
        // Expiry leaves a claimed row alone; the failed check puts it back.
        assert_eq!(s.expire_live_handovers(2000).unwrap().len(), 0);
        assert_eq!(
            s.unclaim_live_handover("call-1", 30).unwrap(),
            Unclaim::Restored
        );
        assert!(
            s.claim_live_handover("call-1", 1000).unwrap().is_none(),
            "expired"
        );
        // Solved: the item goes back to todo with the note, the row is gone.
        assert!(s.claim_live_handover("call-1", 40).unwrap().is_some());
        let row = s
            .finish_live_handover("call-1", true, "continue on that page", 50)
            .unwrap()
            .unwrap();
        assert_eq!(row.item, "a.com");
        let a = item(&s, &m.id, "a.com");
        assert_eq!(a.status, ItemStatus::Todo);
        assert!(!a.live_only);
        assert_eq!(a.owner_answer.as_deref(), Some("continue on that page"));
        assert!(s.live_handover("call-1").unwrap().is_none());
        assert!(
            s.finish_live_handover("call-1", true, "again", 60)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn a_handover_that_runs_out_fails_its_item_also_when_it_was_being_checked() {
        let s = store();
        let m = s
            .create(&new_mission(&[("a.com", "A"), ("b.com", "B")]), 1)
            .unwrap();
        status(&s, &m.id, "a.com", ItemStatus::Doing, false).unwrap();
        park(&s, &m.id, "call-a", 100, 2);
        status(&s, &m.id, "b.com", ItemStatus::Doing, false).unwrap();
        park(&s, &m.id, "call-b", 100, 2);
        // The supervisor's pass: unclaimed rows past their time fail their item.
        assert!(s.claim_live_handover("call-b", 50).unwrap().is_some());
        let expired = s.expire_live_handovers(100).unwrap();
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].call_id, "call-a");
        let a = item(&s, &m.id, "a.com");
        assert_eq!(a.status, ItemStatus::Failed);
        assert!(a.last_error.as_deref().unwrap().contains("in time"));
        // A check that failed after the time: the item fails then, it never stays blocked.
        let Unclaim::Expired(row) = s.unclaim_live_handover("call-b", 120).unwrap() else {
            panic!("expired");
        };
        assert_eq!(row.item, "b.com");
        assert_eq!(item(&s, &m.id, "b.com").status, ItemStatus::Failed);
        // A claim nobody finished is taken back by the expiry after a while.
        status(&s, &m.id, "a.com", ItemStatus::Todo, true).unwrap();
        status(&s, &m.id, "a.com", ItemStatus::Doing, false).unwrap();
        park(&s, &m.id, "call-c", i64::MAX, 2);
        assert!(s.claim_live_handover("call-c", 200).unwrap().is_some());
        assert_eq!(s.expire_live_handovers(201).unwrap().len(), 0);
        assert_eq!(
            s.expire_live_handovers(200 + CLAIM_STALE_MS).unwrap().len(),
            1
        );
    }

    #[test]
    fn a_mission_keeps_at_most_its_cap_of_pages_waiting_and_a_settled_item_frees_one() {
        let s = store();
        let m = s
            .create(&new_mission(&[("a.com", "A"), ("b.com", "B")]), 1)
            .unwrap();
        status(&s, &m.id, "a.com", ItemStatus::Doing, false).unwrap();
        park(&s, &m.id, "call-a", 1000, 1);
        status(&s, &m.id, "b.com", ItemStatus::Doing, false).unwrap();
        assert_eq!(park(&s, &m.id, "call-b", 1000, 1), Park::OverCap);
        assert_eq!(
            item(&s, &m.id, "b.com").status,
            ItemStatus::Doing,
            "untouched"
        );
        // The owner skips the waiting item by hand: its slot is free again.
        status(&s, &m.id, "a.com", ItemStatus::Skipped, true).unwrap();
        assert_eq!(
            park(&s, &m.id, "call-b", 1000, 1),
            Park::Parked("b.com".into())
        );
    }

    #[test]
    fn a_handover_whose_item_the_owner_settled_or_whose_mission_ended_lets_its_page_go() {
        let s = store();
        let m = s
            .create(
                &new_mission(&[("a.com", "A"), ("b.com", "B"), ("c.com", "C")]),
                1,
            )
            .unwrap();
        for (key, call) in [("a.com", "call-a"), ("b.com", "call-b")] {
            status(&s, &m.id, key, ItemStatus::Doing, false).unwrap();
            park(&s, &m.id, call, i64::MAX, 3);
        }
        assert_eq!(
            s.settle_orphan_live_handovers().unwrap(),
            vec![],
            "both wait"
        );
        // The owner skips one: its page is closed, the other still waits.
        status(&s, &m.id, "a.com", ItemStatus::Skipped, true).unwrap();
        let settled = s.settle_orphan_live_handovers().unwrap();
        assert_eq!(settled.len(), 1);
        assert_eq!(settled[0].0.call_id, "call-a");
        assert!(!settled[0].1, "a skipped item's page is closed");
        assert!(s.live_handover("call-a").unwrap().is_none());
        assert_eq!(item(&s, &m.id, "a.com").status, ItemStatus::Skipped);
        // The owner puts one back to todo by hand: the page stays open for the bot.
        status(&s, &m.id, "c.com", ItemStatus::Doing, false).unwrap();
        park(&s, &m.id, "call-c", i64::MAX, 3);
        status(&s, &m.id, "c.com", ItemStatus::Todo, true).unwrap();
        let settled = s.settle_orphan_live_handovers().unwrap();
        assert_eq!(settled.len(), 1);
        assert!(settled[0].1, "an open item gets its page back");
        // A claimed row is the answer's to finish, even when the mission ended meanwhile.
        assert!(s.claim_live_handover("call-b", 20).unwrap().is_some());
        s.set_state(&m.id, MissionState::Cancelled, None, 30)
            .unwrap();
        assert_eq!(s.settle_orphan_live_handovers().unwrap(), vec![]);
        assert_eq!(
            s.unclaim_live_handover("call-b", 40).unwrap(),
            Unclaim::Restored
        );
        let settled = s.settle_orphan_live_handovers().unwrap();
        assert_eq!(settled.len(), 1);
        assert!(!settled[0].1, "the mission is over: the page is closed");
    }

    #[test]
    fn after_a_restart_parked_handovers_are_settled_from_their_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db");
        let id = {
            let s = MissionStore::open(&path).unwrap();
            let m = s
                .create(&new_mission(&[("a.com", "A"), ("b.com", "B")]), 1)
                .unwrap();
            status(&s, &m.id, "a.com", ItemStatus::Doing, false).unwrap();
            park(&s, &m.id, "call-a", 100, 2);
            status(&s, &m.id, "b.com", ItemStatus::Doing, false).unwrap();
            park(&s, &m.id, "call-b", 10_000, 2);
            m.id
        };
        let s = MissionStore::open(&path).unwrap();
        assert!(
            s.live_handover("call-a").unwrap().is_some(),
            "the rows survive"
        );
        let settled = s.recover_live_handovers(500).unwrap();
        assert_eq!(settled.len(), 2);
        // Ran out while the daemon was down: failed. Still in time: the page is lost, so the
        // site is tried again by a later session.
        assert_eq!(item(&s, &id, "a.com").status, ItemStatus::Failed);
        let b = item(&s, &id, "b.com");
        assert_eq!(b.status, ItemStatus::Todo);
        assert!(b.owner_answer.as_deref().unwrap().contains("restarted"));
        assert!(s.live_handover("call-b").unwrap().is_none());
    }
}
