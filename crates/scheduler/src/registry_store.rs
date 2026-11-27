//! Atomic registry policy, queue and rate accounting in schedules.db.
#[allow(clippy::wildcard_imports)] // shares the store's private helpers
use super::*;
use crate::registry::{Limits, Rule, RuleState, TriggerSource};
use serde_json::Value;

pub(super) fn write_rule(conn: &Connection, id: TaskId, rule: &Rule) -> Result<()> {
    let hook = match rule.source {
        TriggerSource::Webhook { id } => Some(id.to_string()),
        _ => None,
    };
    conn.execute("INSERT INTO trigger_registry(schedule_id,config_json,webhook_id) VALUES(?1,?2,?3) ON CONFLICT(schedule_id) DO UPDATE SET config_json=excluded.config_json,webhook_id=excluded.webhook_id",
        params![id.to_string(), serde_json::to_string(rule)?, hook])?;
    Ok(())
}

fn read_rule(conn: &Connection, id: TaskId) -> Result<Option<(Rule, RuleState)>> {
    let raw: Option<(String, String)> = conn
        .query_row(
            "SELECT config_json,state_json FROM trigger_registry WHERE schedule_id=?1",
            [id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    raw.map(|(rule, state)| Ok((serde_json::from_str(&rule)?, serde_json::from_str(&state)?)))
        .transpose()
}
fn save_state(conn: &Connection, id: TaskId, state: &RuleState) -> Result<()> {
    conn.execute(
        "UPDATE trigger_registry SET state_json=?2 WHERE schedule_id=?1",
        params![id.to_string(), serde_json::to_string(state)?],
    )?;
    Ok(())
}
fn trim_hits(hits: &mut Vec<i64>, now: i64) {
    hits.retain(|t| now.saturating_sub(*t) < 86_400_000);
}
fn within(hits: &[i64], limits: &Limits, now: i64) -> bool {
    hits.len() < limits.per_day as usize
        && hits
            .iter()
            .filter(|t| now.saturating_sub(**t) < 3_600_000)
            .count()
            < limits.per_hour as usize
}

impl ScheduleStore {
    /// The owner rule and bounded runtime status for a schedule.
    ///
    /// # Errors
    /// Database or invalid persisted JSON.
    pub fn registry(&self, id: TaskId) -> Result<Option<(Rule, RuleState)>> {
        read_rule(&self.conn(), id)
    }

    /// Edit source and schedule in the same revision-checked transaction. Preserve
    /// rate counters; discard queued old-source events on definition edits.
    ///
    /// # Errors
    /// Invalid rule, source already assigned, frozen schedule, or database failure.
    pub fn update_registry(&self, schedule: &Schedule, rule: &Rule, now: i64) -> Result<bool> {
        rule.validate()
            .map_err(|e| SchedulerError::InvalidSpec(e.into()))?;
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if frozen_in(&tx, schedule.id)? {
            return Err(SchedulerError::Frozen(schedule.id.to_string()));
        }
        if edit_in(
            &tx,
            schedule,
            Some(schedule.enabled),
            now,
            Some(schedule.revision),
        )?
        .is_none()
        {
            return Ok(false);
        }
        tx.execute(
            "UPDATE schedules SET origin='user', author=?2 WHERE id=?1",
            params![schedule.id.to_string(), schedule.stored_author().as_str()],
        )?;
        write_trigger_in(&tx, schedule.id, rule.trigger().as_ref())?;
        let mut state = read_rule(&tx, schedule.id)?
            .map(|(_, s)| s)
            .unwrap_or_default();
        state.pending.clear();
        state.not_before_ms = 0;
        state.last_error = None;
        write_rule(&tx, schedule.id, rule)?;
        save_state(&tx, schedule.id, &state)?;
        tx.commit()?;
        drop(conn);
        self.notify();
        Ok(true)
    }

    /// An authenticated incoming webhook is queued here instead of starting its
    /// independent legacy run. A disabled linked rule remains linked.
    ///
    /// # Errors
    /// Queue full, disabled rule, or database failure.
    pub fn enqueue_webhook(
        &self,
        bot: BotId,
        hook: Uuid,
        input: Value,
        now: i64,
    ) -> Result<Option<TaskId>> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id: Option<String> = tx
            .query_row(
                "SELECT schedule_id FROM trigger_registry WHERE webhook_id=?1",
                [hook.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        let Some(id) = id else { return Ok(None) };
        let id =
            TaskId(Uuid::parse_str(&id).map_err(|e| SchedulerError::InvalidSpec(e.to_string()))?);
        let schedule =
            get_schedule(&tx, id)?.ok_or_else(|| SchedulerError::NotFound(id.to_string()))?;
        if schedule.bot_id != bot || !schedule.enabled || frozen_in(&tx, id)? {
            return Err(SchedulerError::InvalidSpec(
                "this trigger is turned off".into(),
            ));
        }
        let (_, mut state) =
            read_rule(&tx, id)?.ok_or_else(|| SchedulerError::NotFound(id.to_string()))?;
        if state.pending.is_empty() {
            state.not_before_ms = now.saturating_add(1000);
        }
        if !state.enqueue(input) {
            return Err(SchedulerError::InvalidSpec(
                "the trigger queue is full; try again later".into(),
            ));
        }
        save_state(&tx, id, &state)?;
        tx.commit()?;
        drop(conn);
        self.notify();
        Ok(Some(id))
    }

    /// Unified dispatch guard. Queues quiet/rate-limited events; one digest is
    /// reserved atomically with trigger and bot accounting. `None` drains only.
    /// Unmanaged schedules retain their existing behavior.
    ///
    /// # Errors
    /// Database or serialization error. Queue overflow pauses with a fixed reason.
    pub fn registry_dispatch(
        &self,
        schedule: &Schedule,
        event: Option<Value>,
        now: i64,
    ) -> Result<Option<String>> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some((rule, mut state)) = read_rule(&tx, schedule.id)? else {
            return Ok(None);
        };
        let Some(current) = get_schedule(&tx, schedule.id)? else {
            return Ok(None);
        };
        // Timer progress does not change the definition revision: a reserved
        // one-shot may enter this guard with its timer already exhausted.
        if current.revision != schedule.revision || frozen_in(&tx, schedule.id)? {
            return Ok(None);
        }
        let quiet = rule.quiet_hours.as_ref().filter(|q| q.contains(now));
        if let Some(event) = event {
            if quiet.is_some_and(|q| q.drop) {
                return Ok(None);
            }
            // Flatten a watcher batch: 20 mails stay one digest, not 20 runs.
            let events = match event {
                Value::Array(v) => v,
                other => vec![other],
            };
            for event in events {
                if !state.enqueue(event) {
                    state.last_error = Some("queue".into());
                    tx.execute(
                        "UPDATE schedules SET enabled=0,next_run_at_ms=NULL WHERE id=?1",
                        [schedule.id.to_string()],
                    )?;
                    save_state(&tx, schedule.id, &state)?;
                    tx.commit()?;
                    return Ok(None);
                }
            }
        }
        trim_hits(&mut state.fired, now);
        let mut bot_hits: Vec<i64> = tx
            .query_row(
                "SELECT fired_json FROM trigger_bot_rate WHERE bot_id=?1",
                [schedule.bot_id.to_string()],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .map(|s| serde_json::from_str(&s))
            .transpose()?
            .unwrap_or_default();
        trim_hits(&mut bot_hits, now);
        let mut bot_limits = rule.bot_limits.clone();
        {
            let mut stmt=tx.prepare("SELECT r.config_json FROM trigger_registry r JOIN schedules s ON s.id=r.schedule_id WHERE s.bot_id=?1")?;
            for raw in stmt.query_map([schedule.bot_id.to_string()], |r| r.get::<_, String>(0))? {
                let other: Rule = serde_json::from_str(&raw?)?;
                bot_limits.per_hour = bot_limits.per_hour.min(other.bot_limits.per_hour);
                bot_limits.per_day = bot_limits.per_day.min(other.bot_limits.per_day);
            }
        }
        let ready = now >= state.not_before_ms
            && !state.pending.is_empty()
            && quiet.is_none()
            && within(&state.fired, &rule.limits, now)
            && within(&bot_hits, &bot_limits, now);
        let result = if ready {
            let input = serde_json::to_string(&state.pending)?;
            state.pending.clear();
            state.fired.push(now);
            state.last_fired_at_ms = Some(now);
            bot_hits.push(now);
            tx.execute("INSERT INTO trigger_bot_rate(bot_id,fired_json) VALUES(?1,?2) ON CONFLICT(bot_id) DO UPDATE SET fired_json=excluded.fired_json",params![schedule.bot_id.to_string(),serde_json::to_string(&bot_hits)?])?;
            tx.execute(
                "UPDATE schedules SET last_run_at_ms=?2 WHERE id=?1",
                params![schedule.id.to_string(), now],
            )?;
            Some(input)
        } else {
            None
        };
        // A one-shot's timer is already exhausted. Keep it enabled while a
        // quiet/rate-delayed digest waits, and disable it after that digest.
        if matches!(schedule.spec, ScheduleSpec::At { .. }) && state.last_error.is_none() {
            tx.execute(
                "UPDATE schedules SET enabled=?2 WHERE id=?1",
                params![
                    schedule.id.to_string(),
                    i64::from(result.is_none() && !state.pending.is_empty())
                ],
            )?;
        }
        save_state(&tx, schedule.id, &state)?;
        tx.commit()?;
        Ok(result)
    }

    /// Pause an errored registry run without storing external error text.
    ///
    /// # Errors
    /// Database error.
    pub fn registry_failed(&self, schedule: &Schedule) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if get_schedule(&tx, schedule.id)?.is_some_and(|s| s.revision == schedule.revision)
            && let Some((_, mut state)) = read_rule(&tx, schedule.id)?
        {
            state.last_error = Some("run".into());
            save_state(&tx, schedule.id, &state)?;
            tx.execute(
                "UPDATE schedules SET enabled=0,next_run_at_ms=NULL WHERE id=?1",
                [schedule.id.to_string()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}
fn frozen_in(conn: &Connection, id: TaskId) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT frozen_for IS NOT NULL FROM schedules WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(true))
}
