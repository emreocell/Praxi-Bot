//! The wait and wake tables of the mission store: register a wait, fire or time it out, record
//! the wake, park a mission that has nothing runnable and release it when something is.
//!
//! Every function that changes an item, a wait or the mission state runs in one transaction and
//! writes its timeline event there, like the rest of the store.

use rusqlite::{OptionalExtension, Row, Transaction, TransactionBehavior, params};
use serde_json::{Value, json};

use super::{MissionStore, bump, event, item_in, transition_in};
use crate::error::{MissionError, Result};
use crate::model::{
    ItemStatus, MAX_ACTIVE_WAITS, MAX_TEXT_CHARS, MAX_WAITS_PER_ITEM, MissionState, clip,
};
use crate::wake::{
    MAX_REASON_CHARS, NewWait, Wait, WaitSpec, WaitState, Wake, WakeReason, clean_external,
};

const WAIT_COLS: &str = "id, mission_id, item_key, spec_json, reason, state, created_at_ms, \
    due_at_ms, next_poll_at_ms, polls, last_error, ended_at_ms";

fn wait_of(row: &Row<'_>) -> rusqlite::Result<Wait> {
    let spec: String = row.get(3)?;
    let state: String = row.get(5)?;
    Ok(Wait {
        id: row.get(0)?,
        mission_id: row.get(1)?,
        item_key: row.get(2)?,
        spec: serde_json::from_str(&spec).map_err(|e| super::conv_err(3, e))?,
        reason: row.get(4)?,
        state: WaitState::parse(&state)
            .ok_or_else(|| super::conv_err(5, std::io::Error::other(format!("state {state:?}"))))?,
        created_at_ms: row.get(6)?,
        due_at_ms: row.get(7)?,
        next_poll_at_ms: row.get(8)?,
        polls: row.get(9)?,
        last_error: row.get(10)?,
        ended_at_ms: row.get(11)?,
    })
}

fn wake_of(row: &Row<'_>) -> rusqlite::Result<Wake> {
    let reason: String = row.get(4)?;
    let payload: Option<String> = row.get(5)?;
    Ok(Wake {
        id: row.get(0)?,
        mission_id: row.get(1)?,
        wait_id: row.get(2)?,
        item_key: row.get(3)?,
        reason: WakeReason::parse(&reason).ok_or_else(|| {
            super::conv_err(4, std::io::Error::other(format!("reason {reason:?}")))
        })?,
        payload: payload
            .map(|p| serde_json::from_str(&p).map_err(|e| super::conv_err(5, e)))
            .transpose()?,
        created_at_ms: row.get(6)?,
        wait_reason: row.get(7)?,
        site: row
            .get::<_, Option<String>>(8)?
            .and_then(|spec| serde_json::from_str::<WaitSpec>(&spec).ok())
            .and_then(|spec| match spec {
                WaitSpec::Mail { filter } => filter.site,
                _ => None,
            }),
    })
}

const WAKE_COLS: &str = "w.id, w.mission_id, w.wait_id, w.item_key, w.reason, w.payload_json, w.created_at_ms, \
    (SELECT x.reason FROM mission_waits x WHERE x.id = w.wait_id), \
    (SELECT x.spec_json FROM mission_waits x WHERE x.id = w.wait_id)";

/// The wait ends because the item left it: nobody watches it any more.
pub(super) fn cancel_item_waits(tx: &Transaction<'_>, id: &str, key: &str, now: i64) -> Result<()> {
    tx.execute(
        "UPDATE mission_waits SET state = 'cancelled', ended_at_ms = ?3 \
         WHERE mission_id = ?1 AND item_key = ?2 AND state = 'waiting'",
        params![id, key, now],
    )?;
    Ok(())
}

/// The item the wakes are about is settled or parked again: they are not delivered any more.
pub(super) fn consume_item_wakes(
    tx: &Transaction<'_>,
    id: &str,
    key: &str,
    now: i64,
) -> Result<()> {
    tx.execute(
        "UPDATE mission_wakes SET consumed_at_ms = ?3 \
         WHERE mission_id = ?1 AND item_key = ?2 AND consumed_at_ms IS NULL",
        params![id, key, now],
    )?;
    Ok(())
}

/// The mission ended: every wait it still had is cancelled and its wakes are dropped.
pub(super) fn cancel_all_waits(tx: &Transaction<'_>, id: &str, now: i64) -> Result<()> {
    tx.execute(
        "UPDATE mission_waits SET state = 'cancelled', ended_at_ms = ?2 \
         WHERE mission_id = ?1 AND state = 'waiting'",
        params![id, now],
    )?;
    tx.execute(
        "UPDATE mission_wakes SET consumed_at_ms = ?2 WHERE mission_id = ?1 AND consumed_at_ms IS NULL",
        params![id, now],
    )?;
    Ok(())
}

fn active_waits_in(tx: &Transaction<'_>, id: &str) -> Result<Vec<Wait>> {
    let mut stmt = tx.prepare(&format!(
        "SELECT {WAIT_COLS} FROM mission_waits WHERE mission_id = ?1 AND state = 'waiting' ORDER BY id"
    ))?;
    Ok(stmt
        .query_map([id], wait_of)?
        .collect::<rusqlite::Result<_>>()?)
}

fn wait_in(tx: &Transaction<'_>, wait_id: i64) -> Result<Wait> {
    tx.query_row(
        &format!("SELECT {WAIT_COLS} FROM mission_waits WHERE id = ?1"),
        [wait_id],
        wait_of,
    )
    .optional()?
    .ok_or_else(|| MissionError::NotFound(format!("wait {wait_id}")))
}

/// The state of a mission, read inside a transaction.
fn state_in(tx: &Transaction<'_>, id: &str) -> Result<MissionState> {
    let state: String = tx
        .query_row("SELECT state FROM missions WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .optional()?
        .ok_or_else(|| MissionError::NotFound(id.to_owned()))?;
    MissionState::parse(&state)
        .ok_or_else(|| MissionError::Invalid(format!("stored state {state:?} is unknown")))
}

/// Missions whose waits the wake loop looks at: `running`, `waiting` (paused and blocked missions
/// are left alone until the owner moves them).
const WATCHED: &str = "m.state IN ('running', 'waiting')";

impl MissionStore {
    /// Park an item on a wait: the item becomes `waiting` and the bot's session can end or move
    /// on. A woken item does not lose an attempt for having been parked.
    ///
    /// # Errors
    /// Unknown mission or item, an item that is not `todo`, `doing` or `waiting`, too many waits,
    /// an empty reason, SQLite failure.
    pub fn register_wait(&self, id: &str, new: &NewWait, now: i64) -> Result<Wait> {
        let reason = super::check_text("reason", &new.reason, MAX_REASON_CHARS)?;
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let item = item_in(&tx, id, new.item_key.trim())?;
        if !matches!(
            item.status,
            ItemStatus::Todo | ItemStatus::Doing | ItemStatus::Waiting
        ) {
            return Err(MissionError::Invalid(format!(
                "item {:?} is {}: only an item that is still to do can wait",
                item.key, item.status
            )));
        }
        let active = active_waits_in(&tx, id)?;
        if active.len() >= MAX_ACTIVE_WAITS {
            return Err(MissionError::Invalid(format!(
                "this mission already has {MAX_ACTIVE_WAITS} waits registered; let some of them end first"
            )));
        }
        if active.iter().filter(|w| w.item_key == item.key).count() >= MAX_WAITS_PER_ITEM {
            return Err(MissionError::Invalid(format!(
                "item {:?} already has {MAX_WAITS_PER_ITEM} waits",
                item.key
            )));
        }
        let kind = new.spec.kind();
        tx.execute(
            "INSERT INTO mission_waits (mission_id, item_key, kind, spec_json, reason, state, created_at_ms, due_at_ms, next_poll_at_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, 'waiting', ?6, ?7, ?6)",
            params![
                id,
                item.key,
                kind,
                serde_json::to_string(&new.spec)?,
                reason,
                now,
                new.due_at_ms
            ],
        )?;
        let wait_id = tx.last_insert_rowid();
        if item.status != ItemStatus::Waiting {
            tx.execute(
                "UPDATE mission_items SET status = 'waiting', \
                 attempts = CASE WHEN status = 'doing' AND attempts > 0 THEN attempts - 1 ELSE attempts END, \
                 updated_at_ms = ?3 WHERE mission_id = ?1 AND key = ?2",
                params![id, item.key, now],
            )?;
            event(
                &tx,
                id,
                now,
                "item",
                Some(&item.key),
                &json!({"from": item.status.as_str(), "to": "waiting", "wait": kind}),
            )?;
            // The wake that brought the item back has done its job; the next one is delivered
            // when this wait ends.
            consume_item_wakes(&tx, id, &item.key, now)?;
            // Parking an item settles it for now: the session made progress.
            bump(&tx, id, now)?;
        }
        event(
            &tx,
            id,
            now,
            "wait",
            Some(&item.key),
            &json!({"wait": wait_id, "kind": kind, "due_at_ms": new.due_at_ms}),
        )?;
        let wait = wait_in(&tx, wait_id)?;
        tx.commit()?;
        Ok(wait)
    }

    /// The waits of a mission that are still being watched, oldest first.
    ///
    /// # Errors
    /// SQLite failure.
    pub fn active_waits(&self, id: &str) -> Result<Vec<Wait>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        active_waits_in(&tx, id)
    }

    /// The earliest moment something is due in a mission: a timer, or a mail or webhook wait
    /// giving up.
    ///
    /// # Errors
    /// SQLite failure.
    pub fn next_due(&self, id: &str) -> Result<Option<i64>> {
        Ok(self.conn().query_row(
            "SELECT MIN(due_at_ms) FROM mission_waits WHERE mission_id = ?1 AND state = 'waiting'",
            [id],
            |r| r.get(0),
        )?)
    }

    /// Timers that are due, in watched missions.
    ///
    /// # Errors
    /// SQLite failure.
    pub fn due_timers(&self, now: i64) -> Result<Vec<Wait>> {
        self.waits_where(
            "w.kind = 'after' AND w.due_at_ms <= ?1",
            now,
            "w.due_at_ms, w.id",
            i64::MAX,
        )
    }

    /// Mail and webhook waits whose time ran out, in watched missions.
    ///
    /// # Errors
    /// SQLite failure.
    pub fn expired_waits(&self, now: i64) -> Result<Vec<Wait>> {
        self.waits_where(
            "w.kind IN ('mail', 'webhook') AND w.due_at_ms <= ?1",
            now,
            "w.due_at_ms, w.id",
            i64::MAX,
        )
    }

    /// Mail waits whose next look is due, with the bot to look as. At most `limit`, the one that
    /// waited longest first.
    ///
    /// # Errors
    /// SQLite failure.
    pub fn mail_waits_to_poll(&self, now: i64, limit: usize) -> Result<Vec<(Wait, String)>> {
        let waits = self.waits_where(
            "w.kind = 'mail' AND w.next_poll_at_ms <= ?1 AND w.due_at_ms > ?1",
            now,
            "w.next_poll_at_ms, w.id",
            i64::try_from(limit).unwrap_or(i64::MAX),
        )?;
        let conn = self.conn();
        waits
            .into_iter()
            .map(|w| {
                let bot: String = conn.query_row(
                    "SELECT bot_id FROM missions WHERE id = ?1",
                    [&w.mission_id],
                    |r| r.get(0),
                )?;
                Ok((w, bot))
            })
            .collect()
    }

    fn waits_where(&self, cond: &str, now: i64, order: &str, limit: i64) -> Result<Vec<Wait>> {
        let cols: String = WAIT_COLS
            .split(", ")
            .map(|c| format!("w.{c}"))
            .collect::<Vec<_>>()
            .join(", ");
        let db = self.conn();
        let mut stmt = db.prepare(&format!(
            "SELECT {cols} FROM mission_waits w JOIN missions m ON m.id = w.mission_id \
             WHERE w.state = 'waiting' AND {WATCHED} AND {cond} ORDER BY {order} LIMIT ?2"
        ))?;
        Ok(stmt
            .query_map(params![now, limit], wait_of)?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// A look into the mailbox found nothing: the next one is not before `next_at_ms`. `error`
    /// is the reason a failed look gave (the wait goes on).
    ///
    /// # Errors
    /// SQLite failure.
    pub fn note_poll(&self, wait_id: i64, next_at_ms: i64, error: Option<&str>) -> Result<()> {
        self.conn().execute(
            "UPDATE mission_waits SET polls = polls + 1, next_poll_at_ms = ?2, last_error = ?3 \
             WHERE id = ?1 AND state = 'waiting'",
            params![
                wait_id,
                next_at_ms,
                error.map(|e| clean_external(e, MAX_TEXT_CHARS))
            ],
        )?;
        Ok(())
    }

    /// The wait happened: the item goes back to `todo`, the item's other waits end and a wake is
    /// recorded (once per wait). `payload` is what came from the outside; it is cleaned and
    /// capped by the caller. Returns `None` when the wait was no longer waiting.
    ///
    /// # Errors
    /// SQLite failure.
    pub fn fire_wait(
        &self,
        wait_id: i64,
        reason: WakeReason,
        payload: Option<&Value>,
        now: i64,
    ) -> Result<Option<Wake>> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let w = wait_in(&tx, wait_id)?;
        if w.state != WaitState::Waiting {
            return Ok(None);
        }
        tx.execute(
            "UPDATE mission_waits SET state = 'fired', ended_at_ms = ?2 WHERE id = ?1",
            params![wait_id, now],
        )?;
        let item = item_in(&tx, &w.mission_id, &w.item_key)?;
        if item.status == ItemStatus::Waiting {
            tx.execute(
                "UPDATE mission_items SET status = 'todo', updated_at_ms = ?3 WHERE mission_id = ?1 AND key = ?2",
                params![w.mission_id, w.item_key, now],
            )?;
            cancel_item_waits(&tx, &w.mission_id, &w.item_key, now)?;
            event(
                &tx,
                &w.mission_id,
                now,
                "item",
                Some(&w.item_key),
                &json!({"from": "waiting", "to": "todo", "by": "wake"}),
            )?;
        }
        let payload_json = payload.map(Value::to_string);
        tx.execute(
            "INSERT INTO mission_wakes (mission_id, wait_id, item_key, reason, payload_json, dedupe_key, created_at_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                w.mission_id,
                wait_id,
                w.item_key,
                reason.as_str(),
                payload_json,
                format!("wait:{wait_id}"),
                now
            ],
        )?;
        let wake_id = tx.last_insert_rowid();
        event(
            &tx,
            &w.mission_id,
            now,
            "wake",
            Some(&w.item_key),
            &json!({"wait": wait_id, "reason": reason.as_str()}),
        )?;
        bump(&tx, &w.mission_id, now)?;
        let wake = tx.query_row(
            &format!("SELECT {WAKE_COLS} FROM mission_wakes w WHERE w.id = ?1"),
            [wake_id],
            wake_of,
        )?;
        tx.commit()?;
        Ok(Some(wake))
    }

    /// The wait ran out. The item is blocked for the owner (with what was missing), unless
    /// another wait of the item is still running. Returns whether the wait ended now.
    ///
    /// # Errors
    /// SQLite failure.
    pub fn time_out_wait(&self, wait_id: i64, now: i64) -> Result<bool> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let w = wait_in(&tx, wait_id)?;
        if w.state != WaitState::Waiting {
            return Ok(false);
        }
        tx.execute(
            "UPDATE mission_waits SET state = 'timed_out', ended_at_ms = ?2 WHERE id = ?1",
            params![wait_id, now],
        )?;
        event(
            &tx,
            &w.mission_id,
            now,
            "wait_timed_out",
            Some(&w.item_key),
            &json!({"wait": wait_id, "kind": w.spec.kind()}),
        )?;
        let item = item_in(&tx, &w.mission_id, &w.item_key)?;
        let others: i64 = tx.query_row(
            "SELECT COUNT(*) FROM mission_waits WHERE mission_id = ?1 AND item_key = ?2 AND state = 'waiting'",
            params![w.mission_id, w.item_key],
            |r| r.get(0),
        )?;
        if item.status == ItemStatus::Waiting && others == 0 {
            let reason = clip(
                &format!(
                    "nothing came from {} in time; {}",
                    w.spec.describe(),
                    w.reason
                ),
                MAX_TEXT_CHARS,
            );
            tx.execute(
                "UPDATE mission_items SET status = 'blocked', blocked_reason = ?3, updated_at_ms = ?4 \
                 WHERE mission_id = ?1 AND key = ?2",
                params![w.mission_id, w.item_key, reason, now],
            )?;
            consume_item_wakes(&tx, &w.mission_id, &w.item_key, now)?;
            event(
                &tx,
                &w.mission_id,
                now,
                "item",
                Some(&w.item_key),
                &json!({"from": "waiting", "to": "blocked", "reason": reason, "by": "timeout"}),
            )?;
            bump(&tx, &w.mission_id, now)?;
        }
        tx.commit()?;
        Ok(true)
    }

    /// A webhook the owner created was called: every wait on it, in a watched mission of `bot_id`,
    /// fires with the (cleaned) body. Returns the missions that were woken.
    ///
    /// # Errors
    /// SQLite failure.
    pub fn deliver_webhook(
        &self,
        bot_id: &str,
        trigger_id: &str,
        body: &str,
        now: i64,
    ) -> Result<Vec<String>> {
        let waits: Vec<Wait> = {
            let conn = self.conn();
            let cols: String = WAIT_COLS
                .split(", ")
                .map(|c| format!("w.{c}"))
                .collect::<Vec<_>>()
                .join(", ");
            let mut stmt = conn.prepare(&format!(
                "SELECT {cols} FROM mission_waits w JOIN missions m ON m.id = w.mission_id \
                 WHERE w.state = 'waiting' AND w.kind = 'webhook' AND m.bot_id = ?1 AND {WATCHED} ORDER BY w.id"
            ))?;
            stmt.query_map([bot_id], wait_of)?
                .collect::<rusqlite::Result<_>>()?
        };
        let mut woken: Vec<String> = Vec::new();
        for w in waits {
            let WaitSpec::Webhook {
                trigger_id: t,
                name,
            } = &w.spec
            else {
                continue;
            };
            if t != trigger_id {
                continue;
            }
            let payload = json!({
                "webhook": clip(name, 80),
                "body": clean_external(body, crate::model::MAX_WAKE_PAYLOAD_CHARS),
            });
            if self
                .fire_wait(w.id, WakeReason::Webhook, Some(&payload), now)?
                .is_some()
                && !woken.contains(&w.mission_id)
            {
                woken.push(w.mission_id.clone());
            }
        }
        Ok(woken)
    }

    /// Recorded wakes whose item is still to do, oldest first (the brief delivers them).
    ///
    /// # Errors
    /// SQLite failure.
    pub fn pending_wakes(&self, id: &str, limit: usize) -> Result<Vec<Wake>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {WAKE_COLS} FROM mission_wakes w JOIN mission_items i \
             ON i.mission_id = w.mission_id AND i.key = w.item_key \
             WHERE w.mission_id = ?1 AND w.consumed_at_ms IS NULL AND i.status IN ('todo', 'doing') \
             ORDER BY w.id LIMIT ?2"
        ))?;
        Ok(stmt
            .query_map(
                params![id, i64::try_from(limit).unwrap_or(i64::MAX)],
                wake_of,
            )?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// An item the owner (or a crash) left `waiting` with no wait watching it goes back to `todo`:
    /// nothing would ever wake it. Returns how many.
    ///
    /// # Errors
    /// SQLite failure.
    pub fn requeue_orphan_waiting(&self, now: i64) -> Result<u32> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let orphans: Vec<(String, String)> = {
            let mut stmt = tx.prepare(
                "SELECT i.mission_id, i.key FROM mission_items i JOIN missions m ON m.id = i.mission_id \
                 WHERE i.status = 'waiting' AND m.state IN ('running', 'waiting') AND NOT EXISTS \
                 (SELECT 1 FROM mission_waits w WHERE w.mission_id = i.mission_id AND w.item_key = i.key AND w.state = 'waiting')",
            )?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?
        };
        for (mission, key) in &orphans {
            tx.execute(
                "UPDATE mission_items SET status = 'todo', updated_at_ms = ?3 WHERE mission_id = ?1 AND key = ?2",
                params![mission, key, now],
            )?;
            event(
                &tx,
                mission,
                now,
                "item",
                Some(key),
                &json!({"from": "waiting", "to": "todo", "by": "supervisor"}),
            )?;
            bump(&tx, mission, now)?;
        }
        tx.commit()?;
        Ok(u32::try_from(orphans.len()).unwrap_or(u32::MAX))
    }

    /// `running` -> `waiting`, but only when nothing is runnable and an item waits: checked and
    /// changed in one transaction, so a wake that fires at the same moment is not missed.
    /// Returns whether the mission is `waiting` now.
    ///
    /// # Errors
    /// SQLite failure.
    pub fn park_if_idle(&self, id: &str, now: i64) -> Result<bool> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        match state_in(&tx, id)? {
            MissionState::Waiting => return Ok(true),
            MissionState::Running => {}
            _ => return Ok(false),
        }
        let (open, waiting): (i64, i64) = tx.query_row(
            "SELECT COALESCE(SUM(status IN ('todo', 'doing')), 0), COALESCE(SUM(status = 'waiting'), 0) \
             FROM mission_items WHERE mission_id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if open > 0 || waiting == 0 {
            return Ok(false);
        }
        transition_in(
            &tx,
            id,
            MissionState::Waiting,
            Some("waiting for a wake"),
            now,
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// Waiting missions that have something to do again, or nothing left to wait for.
    ///
    /// # Errors
    /// SQLite failure.
    pub fn waiting_to_release(&self) -> Result<Vec<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT m.id FROM missions m WHERE m.state = 'waiting' AND (\
               EXISTS (SELECT 1 FROM mission_items i WHERE i.mission_id = m.id AND i.status IN ('todo', 'doing')) \
               OR NOT EXISTS (SELECT 1 FROM mission_items i WHERE i.mission_id = m.id AND i.status = 'waiting')) \
             ORDER BY m.created_at_ms, m.id",
        )?;
        Ok(stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// `waiting` -> `running`. A mission that has runnable items is released at most `per_hour`
    /// times an hour (a wake storm stops here: the wakes stay recorded and the next window
    /// delivers them); a mission with nothing left to wait for is released at once so the
    /// supervisor can settle it. Returns whether it was released.
    ///
    /// # Errors
    /// SQLite failure.
    pub fn release_waiting(&self, id: &str, per_hour: u32, now: i64) -> Result<bool> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if state_in(&tx, id)? != MissionState::Waiting {
            return Ok(false);
        }
        let (open, waiting): (i64, i64) = tx.query_row(
            "SELECT COALESCE(SUM(status IN ('todo', 'doing')), 0), COALESCE(SUM(status = 'waiting'), 0) \
             FROM mission_items WHERE mission_id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if open == 0 && waiting > 0 {
            return Ok(false);
        }
        if open > 0 {
            let recent: i64 = tx.query_row(
                "SELECT COUNT(*) FROM mission_events WHERE mission_id = ?1 AND kind = 'woken' AND at_ms > ?2",
                params![id, now - 3_600_000],
                |r| r.get(0),
            )?;
            if recent >= i64::from(per_hour) {
                return Ok(false);
            }
            event(&tx, id, now, "woken", None, &json!({}))?;
        }
        transition_in(&tx, id, MissionState::Running, Some("woken"), now)?;
        tx.commit()?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Budgets;
    use crate::store::{ItemUpdate, NewItem, NewMission};
    use crate::wake::MailFilter;

    fn store_with(items: &[&str]) -> (MissionStore, String) {
        let s = MissionStore::open_in_memory().unwrap();
        let m = s
            .create(
                &NewMission {
                    user_id: "u".into(),
                    bot_id: "bot".into(),
                    chat_id: "c".into(),
                    title: String::new(),
                    goal_text: "goal".into(),
                    target_done: None,
                    budgets: Budgets::default(),
                    items: items
                        .iter()
                        .map(|k| NewItem {
                            key: (*k).into(),
                            kind: "site".into(),
                            title: format!("Sign up on {k}"),
                            payload: None,
                        })
                        .collect(),
                },
                1_000,
            )
            .unwrap();
        (s, m.id)
    }

    fn mail_wait(item: &str, due: i64) -> NewWait {
        NewWait {
            item_key: item.into(),
            spec: WaitSpec::Mail {
                filter: MailFilter::new(Some("a.com"), None, None, None).unwrap(),
            },
            reason: "verification mail".into(),
            due_at_ms: due,
        }
    }

    fn timer(item: &str, due: i64) -> NewWait {
        NewWait {
            item_key: item.into(),
            spec: WaitSpec::After,
            reason: "check later".into(),
            due_at_ms: due,
        }
    }

    #[test]
    fn registering_parks_the_item_and_counts_as_progress() {
        let (s, id) = store_with(&["a.com", "b.com"]);
        let v0 = s.get(&id).unwrap().unwrap().ledger_version;
        let w = s
            .register_wait(&id, &mail_wait("a.com", 9_000), 2_000)
            .unwrap();
        assert_eq!(w.state, WaitState::Waiting);
        let p = s.progress(&id).unwrap();
        assert_eq!((p.waiting, p.todo), (1, 1));
        assert!(s.get(&id).unwrap().unwrap().ledger_version > v0);
        assert_eq!(s.next_due(&id).unwrap(), Some(9_000));
        assert_eq!(s.active_waits(&id).unwrap().len(), 1);
    }

    #[test]
    fn only_an_unfinished_item_can_wait_and_the_counts_are_capped() {
        let (s, id) = store_with(&["a.com"]);
        assert!(
            s.register_wait(&id, &timer("nope.com", 5_000), 2_000)
                .is_err()
        );
        for _ in 0..MAX_WAITS_PER_ITEM {
            s.register_wait(&id, &timer("a.com", 5_000), 2_000).unwrap();
        }
        let err = s
            .register_wait(&id, &timer("a.com", 5_000), 2_000)
            .unwrap_err();
        assert!(err.to_string().contains("already has"), "{err}");
        s.update_item(
            &id,
            "a.com",
            &ItemUpdate {
                status: Some(ItemStatus::Done),
                ..ItemUpdate::default()
            },
            false,
            3_000,
        )
        .unwrap();
        assert!(
            s.active_waits(&id).unwrap().is_empty(),
            "the wait ends with the item"
        );
        assert!(s.register_wait(&id, &timer("a.com", 5_000), 4_000).is_err());
    }

    #[test]
    fn a_mission_holds_a_bounded_number_of_waits() {
        let keys: Vec<String> = (0..=MAX_ACTIVE_WAITS)
            .map(|i| format!("s{i}.com"))
            .collect();
        let refs: Vec<&str> = keys.iter().map(String::as_str).collect();
        let (s, id) = store_with(&refs);
        for k in &keys[..MAX_ACTIVE_WAITS] {
            s.register_wait(&id, &timer(k, 5_000), 2_000).unwrap();
        }
        let err = s
            .register_wait(&id, &timer(&keys[MAX_ACTIVE_WAITS], 5_000), 2_000)
            .unwrap_err();
        assert!(err.to_string().contains("already has"), "{err}");
    }

    #[test]
    fn the_model_cannot_park_an_item_without_a_wait() {
        let (s, id) = store_with(&["a.com"]);
        let err = s
            .update_item(
                &id,
                "a.com",
                &ItemUpdate {
                    status: Some(ItemStatus::Waiting),
                    ..ItemUpdate::default()
                },
                false,
                2_000,
            )
            .unwrap_err();
        assert!(err.to_string().contains("mission_wait"), "{err}");
    }

    #[test]
    fn firing_turns_the_item_back_to_todo_records_one_wake_and_ends_the_siblings() {
        let (s, id) = store_with(&["a.com"]);
        let w1 = s
            .register_wait(&id, &mail_wait("a.com", 9_000), 2_000)
            .unwrap();
        let w2 = s.register_wait(&id, &timer("a.com", 6_000), 2_000).unwrap();
        let wake = s
            .fire_wait(w1.id, WakeReason::Mail, Some(&json!({"x": 1})), 3_000)
            .unwrap()
            .unwrap();
        assert_eq!(wake.item_key, "a.com");
        assert_eq!(s.progress(&id).unwrap().todo, 1);
        assert!(
            s.active_waits(&id).unwrap().is_empty(),
            "{w2:?} ended with the item's wake"
        );
        assert!(
            s.fire_wait(w1.id, WakeReason::Mail, None, 3_100)
                .unwrap()
                .is_none(),
            "a wait wakes once"
        );
        assert_eq!(s.pending_wakes(&id, 10).unwrap().len(), 1);
        // Settling the item delivers the wake no more.
        s.update_item(
            &id,
            "a.com",
            &ItemUpdate {
                status: Some(ItemStatus::Done),
                ..ItemUpdate::default()
            },
            false,
            4_000,
        )
        .unwrap();
        assert_eq!(s.pending_wakes(&id, 10).unwrap().len(), 0);
    }

    #[test]
    fn a_timeout_blocks_the_item_unless_another_wait_still_runs() {
        let (s, id) = store_with(&["a.com", "b.com"]);
        let only = s
            .register_wait(&id, &mail_wait("a.com", 5_000), 2_000)
            .unwrap();
        let first = s
            .register_wait(&id, &mail_wait("b.com", 5_000), 2_000)
            .unwrap();
        s.register_wait(&id, &timer("b.com", 9_000), 2_000).unwrap();
        assert!(s.time_out_wait(only.id, 5_000).unwrap());
        assert!(!s.time_out_wait(only.id, 5_001).unwrap(), "ends once");
        let a = &s.items(&id, None).unwrap()[0];
        assert_eq!(a.status, ItemStatus::Blocked);
        assert!(
            a.blocked_reason
                .as_deref()
                .unwrap()
                .contains("a mail from a.com")
        );
        assert!(s.time_out_wait(first.id, 5_000).unwrap());
        assert_eq!(
            s.items(&id, None).unwrap()[1].status,
            ItemStatus::Waiting,
            "the timer of b.com still runs"
        );
    }

    #[test]
    fn webhooks_wake_only_waits_on_that_trigger_of_that_bot() {
        let (s, id) = store_with(&["a.com", "b.com"]);
        let spec = |t: &str| WaitSpec::Webhook {
            trigger_id: t.into(),
            name: "Form".into(),
        };
        for (k, t) in [("a.com", "hook-1"), ("b.com", "hook-2")] {
            s.register_wait(
                &id,
                &NewWait {
                    item_key: k.into(),
                    spec: spec(t),
                    reason: "form".into(),
                    due_at_ms: 9_000,
                },
                2_000,
            )
            .unwrap();
        }
        assert_eq!(
            s.deliver_webhook("other-bot", "hook-1", "x", 3_000)
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            s.deliver_webhook("bot", "hook-3", "x", 3_000)
                .unwrap()
                .len(),
            0
        );
        let woken = s
            .deliver_webhook("bot", "hook-1", "hello\u{0}world", 3_000)
            .unwrap();
        assert_eq!(woken, vec![id.clone()]);
        let wake = &s.pending_wakes(&id, 10).unwrap()[0];
        assert_eq!(wake.reason, WakeReason::Webhook);
        assert_eq!(wake.payload.as_ref().unwrap()["body"], "hello world");
        assert_eq!(s.active_waits(&id).unwrap().len(), 1);
    }

    #[test]
    fn a_mission_parks_only_when_nothing_is_runnable_and_something_waits() {
        let (s, id) = store_with(&["a.com", "b.com"]);
        assert!(!s.park_if_idle(&id, 2_000).unwrap(), "nothing waits");
        s.register_wait(&id, &timer("a.com", 5_000), 2_000).unwrap();
        assert!(!s.park_if_idle(&id, 2_000).unwrap(), "b.com is still to do");
        s.update_item(
            &id,
            "b.com",
            &ItemUpdate {
                status: Some(ItemStatus::Done),
                ..ItemUpdate::default()
            },
            false,
            2_100,
        )
        .unwrap();
        assert!(s.park_if_idle(&id, 2_200).unwrap());
        assert_eq!(s.get(&id).unwrap().unwrap().state, MissionState::Waiting);
        assert_eq!(s.waiting_to_release().unwrap(), Vec::<String>::new());
    }

    #[test]
    fn release_is_rate_limited_per_hour_only_when_there_is_work() {
        let (s, id) = store_with(&["a.com"]);
        let mut now = 10_000;
        for round in 0..3u32 {
            let w = s
                .register_wait(&id, &timer("a.com", now + 10), now)
                .unwrap();
            assert!(s.park_if_idle(&id, now).unwrap());
            s.fire_wait(w.id, WakeReason::Timer, None, now + 10)
                .unwrap();
            now += 20;
            assert_eq!(s.waiting_to_release().unwrap(), vec![id.clone()]);
            let released = s.release_waiting(&id, 2, now).unwrap();
            assert_eq!(released, round < 2, "round {round}");
            if !released {
                break;
            }
            // The session takes the item up again and parks it.
            s.update_item(
                &id,
                "a.com",
                &ItemUpdate {
                    status: Some(ItemStatus::Doing),
                    ..ItemUpdate::default()
                },
                false,
                now,
            )
            .unwrap();
        }
        // An hour later the window has reopened.
        assert!(s.release_waiting(&id, 2, now + 3_600_001).unwrap());
    }

    #[test]
    fn ending_a_mission_cancels_its_waits() {
        let (s, id) = store_with(&["a.com"]);
        s.register_wait(&id, &timer("a.com", 5_000), 2_000).unwrap();
        s.set_state(&id, MissionState::Cancelled, Some("stop"), 3_000)
            .unwrap();
        assert_eq!(s.active_waits(&id).unwrap().len(), 0);
        assert_eq!(s.due_timers(10_000).unwrap().len(), 0);
    }

    #[test]
    fn an_orphaned_waiting_item_goes_back_to_todo() {
        let (s, id) = store_with(&["a.com"]);
        let w = s.register_wait(&id, &timer("a.com", 5_000), 2_000).unwrap();
        s.conn()
            .execute(
                "UPDATE mission_waits SET state = 'cancelled' WHERE id = ?1",
                [w.id],
            )
            .unwrap();
        assert_eq!(s.requeue_orphan_waiting(3_000).unwrap(), 1);
        assert_eq!(s.progress(&id).unwrap().todo, 1);
    }

    #[test]
    fn due_queries_see_only_watched_missions() {
        let (s, id) = store_with(&["a.com"]);
        s.register_wait(&id, &timer("a.com", 5_000), 2_000).unwrap();
        assert_eq!(s.due_timers(4_999).unwrap().len(), 0);
        assert_eq!(s.due_timers(5_000).unwrap().len(), 1);
        s.set_state(&id, MissionState::Paused, Some("pause"), 3_000)
            .unwrap();
        assert!(
            s.due_timers(9_000).unwrap().is_empty(),
            "a paused mission's waits are left alone"
        );
        s.set_state(&id, MissionState::Running, None, 9_500)
            .unwrap();
        assert_eq!(s.due_timers(9_500).unwrap().len(), 1);
    }
}
