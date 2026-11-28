//! Parked live hand-overs: a CAPTCHA a mission session left for the owner on the live page.
//!
//! One row per tool call (`call_id`). The row, not a timer in memory, is what keeps the item
//! parked: it outlives the session that asked and a restart, the supervisor's wake pass expires
//! it, and the owner's answer claims it. Claim, finish, restore and expiry are each one
//! transaction, so two of them never both act on the same row.
//!
//! The item of a row is `blocked` with `live_only` set; only the owner (through these functions
//! or an owner edit of the item) moves it on.

use rusqlite::{OptionalExtension, Row, Transaction, params};
use serde_json::json;

use super::{MissionStore, bump, check_text, event, transition_in};
use crate::error::Result;
use crate::model::{ItemStatus, MAX_TEXT_CHARS, MissionState};

/// A claim older than this is treated as abandoned (the answer that took it died): expiry may
/// take the row again. Checking a page takes seconds.
pub const CLAIM_STALE_MS: i64 = 10 * 60 * 1000;

const COLS: &str = "call_id, mission_id, item_key, bot_id, run_id, kind, expires_at_ms";

/// One parked hand-over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParkedHandover {
    /// The `browser_handover` tool call the owner's card answers.
    pub call_id: String,
    /// Mission.
    pub mission_id: String,
    /// The item parked as blocked (`live_only`).
    pub item: String,
    /// The bot whose browser holds the page.
    pub bot_id: String,
    /// The session run that asked.
    pub run_id: String,
    /// `captcha`.
    pub kind: String,
    /// When the supervisor gives it up.
    pub expires_at_ms: i64,
}

fn parked_of(row: &Row<'_>) -> rusqlite::Result<ParkedHandover> {
    Ok(ParkedHandover {
        call_id: row.get(0)?,
        mission_id: row.get(1)?,
        item: row.get(2)?,
        bot_id: row.get(3)?,
        run_id: row.get(4)?,
        kind: row.get(5)?,
        expires_at_ms: row.get(6)?,
    })
}

/// What [`MissionStore::park_live_handover`] needs.
#[derive(Debug, Clone)]
pub struct NewHandover<'a> {
    /// Tool call id.
    pub call_id: &'a str,
    /// Bot id.
    pub bot_id: &'a str,
    /// Session run id.
    pub run_id: &'a str,
    /// `captcha`.
    pub kind: &'a str,
    /// Blocked reason the owner reads.
    pub reason: &'a str,
    /// When it expires.
    pub expires_at_ms: i64,
}

/// What [`MissionStore::park_live_handover`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Park {
    /// The item with this key is parked and the row written.
    Parked(String),
    /// No single item is `doing`: nothing was changed.
    NoSoleItem,
    /// The mission already has `cap` parked hand-overs waiting: nothing was changed.
    OverCap,
}

/// What [`MissionStore::unclaim_live_handover`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unclaim {
    /// The row waits again for the next answer.
    Restored,
    /// It ran out while it was claimed: the item failed and the row is gone.
    Expired(ParkedHandover),
    /// Nothing claimed under that id.
    Gone,
}

fn row_in(tx: &Transaction<'_>, call_id: &str) -> Result<Option<ParkedHandover>> {
    Ok(tx
        .query_row(
            &format!("SELECT {COLS} FROM mission_handovers WHERE call_id = ?1"),
            [call_id],
            parked_of,
        )
        .optional()?)
}

/// Fail the item of `h` (when it is still parked live) and drop the row.
fn fail_in(
    tx: &Transaction<'_>,
    h: &ParkedHandover,
    error: &str,
    by: &str,
    now: i64,
) -> Result<()> {
    tx.execute(
        "DELETE FROM mission_handovers WHERE call_id = ?1",
        [&h.call_id],
    )?;
    let error = check_text("error", error, MAX_TEXT_CHARS)?;
    let n = tx.execute(
        "UPDATE mission_items SET status = 'failed', last_error = ?3, blocked_reason = NULL, live_only = 0, updated_at_ms = ?4 \
         WHERE mission_id = ?1 AND key = ?2 AND status = 'blocked' AND live_only = 1",
        params![h.mission_id, h.item, error, now],
    )?;
    if n > 0 {
        event(
            tx,
            &h.mission_id,
            now,
            "item",
            Some(&h.item),
            &json!({"from": "blocked", "to": "failed", "by": by}),
        )?;
        bump(tx, &h.mission_id, now)?;
    }
    Ok(())
}

/// Put the item of `h` (when it is still parked live) back to `todo` with `note` for the next
/// session, drop the row, and let a mission that waits for the owner run again.
fn release_in(
    tx: &Transaction<'_>,
    h: &ParkedHandover,
    note: &str,
    by: &str,
    now: i64,
) -> Result<bool> {
    tx.execute(
        "DELETE FROM mission_handovers WHERE call_id = ?1",
        [&h.call_id],
    )?;
    let note = check_text("answer", note, MAX_TEXT_CHARS)?;
    let n = tx.execute(
        "UPDATE mission_items SET status = 'todo', owner_answer = ?3, blocked_reason = NULL, live_only = 0, attempts = 0, cuts = 0, updated_at_ms = ?4 \
         WHERE mission_id = ?1 AND key = ?2 AND status = 'blocked' AND live_only = 1",
        params![h.mission_id, h.item, note, now],
    )?;
    if n == 0 {
        return Ok(false);
    }
    event(
        tx,
        &h.mission_id,
        now,
        "item",
        Some(&h.item),
        &json!({"from": "blocked", "to": "todo", "by": by}),
    )?;
    bump(tx, &h.mission_id, now)?;
    let state: String = tx.query_row(
        "SELECT state FROM missions WHERE id = ?1",
        [&h.mission_id],
        |r| r.get(0),
    )?;
    if matches!(
        MissionState::parse(&state),
        Some(MissionState::Blocked | MissionState::Waiting)
    ) {
        transition_in(
            tx,
            &h.mission_id,
            MissionState::Running,
            Some("the owner finished on the page"),
            now,
        )?;
    }
    Ok(true)
}

impl MissionStore {
    /// A CAPTCHA stopped the item the session is on (the only one `doing`): park it as blocked
    /// `live_only` and record the hand-over, in one transaction. Refused without a change when
    /// the mission already has `cap` hand-overs waiting for the owner.
    ///
    /// # Errors
    /// Invalid reason, SQLite failure.
    pub fn park_live_handover(
        &self,
        id: &str,
        new: &NewHandover<'_>,
        cap: u32,
        now: i64,
    ) -> Result<Park> {
        let reason = check_text("blocked reason", new.reason, MAX_TEXT_CHARS)?;
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        // Only rows whose item still waits count: an item the owner settled by hand frees its slot.
        let waiting: u32 = tx.query_row(
            "SELECT COUNT(*) FROM mission_handovers h JOIN mission_items i \
             ON i.mission_id = h.mission_id AND i.key = h.item_key \
             WHERE h.mission_id = ?1 AND i.status = 'blocked' AND i.live_only = 1",
            [id],
            |r| r.get(0),
        )?;
        if waiting >= cap {
            return Ok(Park::OverCap);
        }
        let doing: Vec<String> = {
            let mut stmt =
                tx.prepare("SELECT key FROM mission_items WHERE mission_id = ?1 AND status = ?2")?;
            stmt.query_map(params![id, ItemStatus::Doing.as_str()], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        let [key] = doing.as_slice() else {
            return Ok(Park::NoSoleItem);
        };
        tx.execute(
            "UPDATE mission_items SET status = 'blocked', blocked_reason = ?3, live_only = 1, updated_at_ms = ?4 \
             WHERE mission_id = ?1 AND key = ?2",
            params![id, key, reason, now],
        )?;
        super::waits::consume_item_wakes(&tx, id, key, now)?;
        event(
            &tx,
            id,
            now,
            "item",
            Some(key),
            &json!({"from": "doing", "to": "blocked", "reason": reason, "by": "handover"}),
        )?;
        bump(&tx, id, now)?;
        tx.execute(
            "INSERT INTO mission_handovers (call_id, mission_id, item_key, bot_id, run_id, kind, created_at_ms, expires_at_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                new.call_id,
                id,
                key,
                new.bot_id,
                new.run_id,
                new.kind,
                now,
                new.expires_at_ms
            ],
        )?;
        tx.commit()?;
        Ok(Park::Parked(key.clone()))
    }

    /// The parked hand-over under `call_id`, claimed or not.
    ///
    /// # Errors
    /// SQLite failure.
    pub fn live_handover(&self, call_id: &str) -> Result<Option<ParkedHandover>> {
        let conn = self.conn();
        Ok(conn
            .query_row(
                &format!("SELECT {COLS} FROM mission_handovers WHERE call_id = ?1"),
                [call_id],
                parked_of,
            )
            .optional()?)
    }

    /// Take the hand-over for one owner answer: exactly one caller gets it, and only before it
    /// expires. Finish it with [`Self::finish_live_handover`] or give it back with
    /// [`Self::unclaim_live_handover`].
    ///
    /// # Errors
    /// SQLite failure.
    pub fn claim_live_handover(&self, call_id: &str, now: i64) -> Result<Option<ParkedHandover>> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let n = tx.execute(
            "UPDATE mission_handovers SET claimed_at_ms = ?2 \
             WHERE call_id = ?1 AND claimed_at_ms IS NULL AND expires_at_ms > ?2",
            params![call_id, now],
        )?;
        let row = if n > 0 { row_in(&tx, call_id)? } else { None };
        tx.commit()?;
        Ok(row)
    }

    /// An answer failed (the check is still on the page): the hand-over waits again, unless it
    /// ran out meanwhile, in which case the item fails now (it never stays blocked without a
    /// row that expires it).
    ///
    /// # Errors
    /// SQLite failure.
    pub fn unclaim_live_handover(&self, call_id: &str, now: i64) -> Result<Unclaim> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let Some(h) = tx
            .query_row(
                &format!(
                    "SELECT {COLS} FROM mission_handovers WHERE call_id = ?1 AND claimed_at_ms IS NOT NULL"
                ),
                [call_id],
                parked_of,
            )
            .optional()?
        else {
            return Ok(Unclaim::Gone);
        };
        let out = if h.expires_at_ms <= now {
            fail_in(
                &tx,
                &h,
                "a CAPTCHA nobody finished on the page in time",
                "handover_timeout",
                now,
            )?;
            Unclaim::Expired(h)
        } else {
            tx.execute(
                "UPDATE mission_handovers SET claimed_at_ms = NULL WHERE call_id = ?1",
                [call_id],
            )?;
            Unclaim::Restored
        };
        tx.commit()?;
        Ok(out)
    }

    /// End a claimed hand-over: `solved` puts the item back to `todo` with `note` for the next
    /// session (and lets a mission waiting for the owner run again), otherwise the item fails
    /// with `note` as the error. Returns the row, `None` when nothing was claimed under the id.
    ///
    /// # Errors
    /// Invalid note, SQLite failure.
    pub fn finish_live_handover(
        &self,
        call_id: &str,
        solved: bool,
        note: &str,
        now: i64,
    ) -> Result<Option<ParkedHandover>> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let Some(h) = tx
            .query_row(
                &format!(
                    "SELECT {COLS} FROM mission_handovers WHERE call_id = ?1 AND claimed_at_ms IS NOT NULL"
                ),
                [call_id],
                parked_of,
            )
            .optional()?
        else {
            return Ok(None);
        };
        if solved {
            release_in(&tx, &h, note, "owner_live", now)?;
        } else {
            fail_in(&tx, &h, note, "owner_live", now)?;
        }
        tx.commit()?;
        Ok(Some(h))
    }

    /// The supervisor's expiry: every unclaimed hand-over past its time (and a claim abandoned
    /// for [`CLAIM_STALE_MS`]) fails its item and is removed. Returns what expired, so the
    /// caller lets go of the pages.
    ///
    /// # Errors
    /// SQLite failure.
    pub fn expire_live_handovers(&self, now: i64) -> Result<Vec<ParkedHandover>> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let due: Vec<ParkedHandover> = {
            let mut stmt = tx.prepare(&format!(
                "SELECT {COLS} FROM mission_handovers WHERE \
                 (claimed_at_ms IS NULL AND expires_at_ms <= ?1) OR claimed_at_ms <= ?2"
            ))?;
            stmt.query_map(params![now, now - CLAIM_STALE_MS], parked_of)?
                .collect::<rusqlite::Result<_>>()?
        };
        for h in &due {
            fail_in(
                &tx,
                h,
                "a CAPTCHA nobody finished on the page in time",
                "handover_timeout",
                now,
            )?;
        }
        tx.commit()?;
        Ok(due)
    }

    /// After a restart no browser page survives, so no parked hand-over can be answered any
    /// more: one that ran out fails, the others go back to `todo` (the next session opens the
    /// site again and, if the CAPTCHA is still there, parks a fresh hand-over). Returns every
    /// row it settled.
    ///
    /// # Errors
    /// SQLite failure.
    pub fn recover_live_handovers(&self, now: i64) -> Result<Vec<ParkedHandover>> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let all: Vec<ParkedHandover> = {
            let mut stmt = tx.prepare(&format!("SELECT {COLS} FROM mission_handovers"))?;
            stmt.query_map([], parked_of)?
                .collect::<rusqlite::Result<_>>()?
        };
        for h in &all {
            if h.expires_at_ms <= now {
                fail_in(
                    &tx,
                    h,
                    "a CAPTCHA nobody finished on the page in time",
                    "handover_timeout",
                    now,
                )?;
            } else {
                release_in(
                    &tx,
                    h,
                    "The page kept for the owner was lost when Praxi restarted. Open the site again; if the CAPTCHA is still there, hand it over again with browser_handover.",
                    "restart",
                    now,
                )?;
            }
        }
        tx.commit()?;
        Ok(all)
    }
}
