//! Deleting a bot together with everything only it owns, and counting that first.
//!
//! The plain [`Storage::delete_bot_with_cleanup`] refuses a bot that still has chats or runs
//! (a foreign-key failure). This is the owner's explicit "delete it all": one transaction removes
//! the bot's chats, messages (and their search rows), runs, steps, approvals, event-order rows,
//! webhooks, skill assignment and spend limits, then the bot; the audit log only grows.

use protocol::{BotId, ChatId, RunId, UserId};
use rusqlite::{Transaction, params};
use serde_json::json;
use uuid::Uuid;

use crate::db::{Storage, now_ms};
use crate::error::{Result, StorageError};

/// The run states that are over; every other state is a run that is still alive.
const FINISHED: &str = "('succeeded', 'failed', 'cancelled')";

/// What a bot owns in the main database, as counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BotDataCounts {
    /// Chats of the bot.
    pub chats: u64,
    /// Messages in those chats.
    pub messages: u64,
    /// Runs of the bot (including subagent runs).
    pub runs: u64,
    /// Steps of those runs.
    pub run_steps: u64,
    /// Approvals asked in those runs.
    pub approvals: u64,
    /// Incoming webhooks bound to the bot.
    pub webhooks: u64,
    /// Runs that have not finished (running, queued, paused, waiting).
    pub active_runs: u64,
}

/// What [`Storage::delete_bot_cascade`] removed, and the ids a caller needs to clean up the
/// files and indexes that live outside the database.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BotCascade {
    /// Rows removed.
    pub removed: BotDataCounts,
    /// Runs that were removed (their event logs are files).
    pub run_ids: Vec<RunId>,
    /// Chats that were removed (their attachments are files).
    pub chat_ids: Vec<ChatId>,
}

fn count(tx: &Transaction<'_>, sql: &str, bid: &str) -> Result<u64> {
    let n: i64 = tx.query_row(sql, [bid], |r| r.get(0))?;
    Ok(u64::try_from(n).unwrap_or(0))
}

fn counts(tx: &Transaction<'_>, bid: &str) -> Result<BotDataCounts> {
    Ok(BotDataCounts {
        chats: count(tx, "SELECT COUNT(*) FROM chats WHERE bot_id = ?1", bid)?,
        messages: count(
            tx,
            "SELECT COUNT(*) FROM messages WHERE chat_id IN (SELECT id FROM chats WHERE bot_id = ?1)",
            bid,
        )?,
        runs: count(tx, "SELECT COUNT(*) FROM runs WHERE bot_id = ?1", bid)?,
        run_steps: count(
            tx,
            "SELECT COUNT(*) FROM run_steps WHERE run_id IN (SELECT id FROM runs WHERE bot_id = ?1)",
            bid,
        )?,
        approvals: count(
            tx,
            "SELECT COUNT(*) FROM approvals WHERE run_id IN (SELECT id FROM runs WHERE bot_id = ?1)",
            bid,
        )?,
        webhooks: count(
            tx,
            "SELECT COUNT(*) FROM incoming_webhooks WHERE bot_id = ?1",
            bid,
        )?,
        active_runs: count(
            tx,
            &format!("SELECT COUNT(*) FROM runs WHERE bot_id = ?1 AND state NOT IN {FINISHED}"),
            bid,
        )?,
    })
}

fn ids(tx: &Transaction<'_>, sql: &str, bid: &str) -> Result<Vec<Uuid>> {
    let mut stmt = tx.prepare(sql)?;
    let rows = stmt.query_map([bid], |r| r.get::<_, String>(0))?;
    let mut out = Vec::new();
    for row in rows {
        if let Ok(id) = Uuid::parse_str(&row?) {
            out.push(id);
        }
    }
    Ok(out)
}

impl Storage {
    /// Count what a bot owns in the main database (the numbers of the delete preview).
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn bot_data_counts(&self, bot: BotId) -> Result<BotDataCounts> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        counts(&tx, &bot.to_string())
    }

    /// Delete a bot and everything only it owns, atomically, whatever chats and runs it has.
    ///
    /// Refused with [`StorageError::Conflict`] while a run of the bot is still alive. Other bots'
    /// rows are never touched: a stray link from another bot's row to this bot's chat or run is
    /// cleared, not followed. The bot's workspace folder name stays reserved (so a new bot can
    /// not take over its files by taking its name) but its size limits go. An audit entry
    /// `bot.deleted` records the counts; earlier audit rows stay. `cleanup` runs after the rows
    /// are gone and before the commit, and is told what was removed; if it fails nothing is
    /// deleted. It must not reenter this storage connection.
    ///
    /// Returns `None` when the bot does not exist.
    ///
    /// # Errors
    /// Fails on SQLite errors, the conflict above, or the cleanup error.
    pub fn delete_bot_cascade(
        &self,
        user: UserId,
        bot: BotId,
        actor: &str,
        cleanup: impl FnOnce(&BotCascade) -> Result<()>,
    ) -> Result<Option<BotCascade>> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let bid = bot.to_string();
        let name: Option<String> = tx
            .query_row("SELECT name FROM bots WHERE id = ?1", [&bid], |r| r.get(0))
            .ok();
        let Some(name) = name else {
            return Ok(None);
        };
        let removed = counts(&tx, &bid)?;
        if removed.active_runs > 0 {
            return Err(StorageError::Conflict("bot has an active run".into()));
        }
        let run_ids = ids(&tx, "SELECT id FROM runs WHERE bot_id = ?1", &bid)?;
        let chat_ids = ids(&tx, "SELECT id FROM chats WHERE bot_id = ?1", &bid)?;

        let runs_of = "(SELECT id FROM runs WHERE bot_id = ?1)";
        let chats_of = "(SELECT id FROM chats WHERE bot_id = ?1)";
        // Links from rows that stay (another bot's) to rows that go are cleared first.
        tx.execute(
            &format!("UPDATE messages SET run_id = NULL WHERE run_id IN {runs_of} AND chat_id NOT IN {chats_of}"),
            [&bid],
        )?;
        tx.execute(
            &format!("UPDATE runs SET parent_run_id = NULL WHERE bot_id <> ?1 AND parent_run_id IN {runs_of}"),
            [&bid],
        )?;
        tx.execute(
            &format!("UPDATE runs SET chat_id = NULL WHERE bot_id <> ?1 AND chat_id IN {chats_of}"),
            [&bid],
        )?;
        tx.execute(
            &format!("DELETE FROM approvals WHERE run_id IN {runs_of}"),
            [&bid],
        )?;
        tx.execute(
            &format!("DELETE FROM run_steps WHERE run_id IN {runs_of}"),
            [&bid],
        )?;
        // The search rows go with the messages (trigger `chat_search_delete`).
        tx.execute(
            &format!("DELETE FROM messages WHERE chat_id IN {chats_of}"),
            [&bid],
        )?;
        tx.execute(
            &format!("DELETE FROM event_index WHERE run_id IN {runs_of}"),
            [&bid],
        )?;
        tx.execute(
            "UPDATE runs SET parent_run_id = NULL WHERE bot_id = ?1",
            [&bid],
        )?;
        tx.execute("DELETE FROM runs WHERE bot_id = ?1", [&bid])?;
        tx.execute("DELETE FROM chats WHERE bot_id = ?1", [&bid])?;
        tx.execute(
            "DELETE FROM incoming_webhook_seen WHERE webhook_id IN (SELECT id FROM incoming_webhooks WHERE bot_id = ?1)",
            [&bid],
        )?;
        tx.execute("DELETE FROM incoming_webhooks WHERE bot_id = ?1", [&bid])?;
        tx.execute("DELETE FROM bot_skill_assignment WHERE bot_id = ?1", [&bid])?;
        tx.execute("DELETE FROM bot_spend_limits WHERE bot_id = ?1", [&bid])?;
        tx.execute(
            "UPDATE bot_workspaces SET soft_limit_bytes = NULL, hard_limit_bytes = NULL, updated_at_ms = ?2 WHERE bot_id = ?1",
            params![&bid, now_ms()],
        )?;
        tx.execute("DELETE FROM bots WHERE id = ?1", [&bid])?;
        let detail = json!({
            "bot_id": bid,
            "name": name,
            "chats": removed.chats,
            "messages": removed.messages,
            "runs": removed.runs,
            "run_steps": removed.run_steps,
            "approvals": removed.approvals,
            "webhooks": removed.webhooks,
        });
        tx.execute(
            "INSERT INTO audit_log (user_id, run_id, actor, action, detail_json, ts_ms) VALUES (?1, NULL, ?2, 'bot.deleted', ?3, ?4)",
            params![user.to_string(), actor, serde_json::to_string(&detail)?, now_ms()],
        )?;
        let cascade = BotCascade {
            removed,
            run_ids: run_ids.into_iter().map(RunId).collect(),
            chat_ids: chat_ids.into_iter().map(ChatId).collect(),
        };
        cleanup(&cascade)?;
        tx.commit()?;
        Ok(Some(cascade))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::incoming_webhooks::IncomingWebhook;
    use crate::records::{Approval, Bot, Chat, Message, Run, RunStep, SkillAssignmentMode, User};

    struct Owned {
        bot: Bot,
        chat: Chat,
        run: Run,
    }

    /// One bot with a chat, two messages, a run with a step and an approval, an event-order row,
    /// a webhook, a skill assignment, spend limits and a workspace.
    fn populate(st: &Storage, user: UserId, name: &str, gseq: u64) -> Owned {
        let bot = Bot {
            id: BotId::new(),
            user_id: user,
            name: name.into(),
            config: json!({}),
            created_at_ms: 1,
        };
        st.insert_bot(&bot).unwrap();
        let chat = Chat {
            id: ChatId::new(),
            user_id: user,
            bot_id: bot.id,
            title: format!("{name} chat"),
            created_at_ms: 2,
        };
        st.insert_chat(&chat).unwrap();
        let run = Run {
            id: RunId::new(),
            user_id: user,
            bot_id: bot.id,
            chat_id: Some(chat.id),
            kind: "turn".into(),
            state: "succeeded".into(),
            error: None,
            created_at_ms: 3,
            updated_at_ms: 3,
            parent_run_id: None,
            spec: None,
            computer_target: None,
            computer_node: None,
            computer_node_name: None,
            paused_at_ms: None,
            pause_reason: None,
        };
        st.insert_run(&run).unwrap();
        for (role, text) in [("user", "quokka question"), ("assistant", "quokka answer")] {
            st.insert_message(&Message {
                id: Uuid::now_v7(),
                user_id: user,
                chat_id: chat.id,
                run_id: Some(run.id),
                role: role.into(),
                content: format!("{name} {text}"),
                created_at_ms: 4,
                exposure_untrusted: false,
                exposure_private: false,
                exposure_sources: Vec::new(),
            })
            .unwrap();
        }
        let step = RunStep {
            id: Uuid::now_v7(),
            run_id: run.id,
            user_id: user,
            idx: 0,
            name: "tool".into(),
            input: json!({}),
            idempotent: true,
            retry: None,
            status: "done".into(),
            attempt: 1,
            output: None,
            error: None,
            started_at_ms: 5,
            finished_at_ms: Some(6),
        };
        st.insert_step(&step).unwrap();
        st.insert_approval(&Approval {
            id: Uuid::now_v7(),
            user_id: user,
            run_id: run.id,
            step_id: Some(step.id),
            summary: "write a file".into(),
            decision: Some("approved".into()),
            requested_at_ms: 5,
            decided_at_ms: Some(6),
        })
        .unwrap();
        st.index_event(gseq, run.id, 1).unwrap();
        st.insert_incoming_webhook(&IncomingWebhook {
            id: Uuid::now_v7(),
            user_id: user,
            bot_id: bot.id,
            name: "hook".into(),
            instruction: "do it".into(),
            enabled: true,
            created_at_ms: 7,
            updated_at_ms: 7,
            last_fired_at_ms: None,
        })
        .unwrap();
        st.set_bot_skill_assignment(bot.id, SkillAssignmentMode::Selected, &["notes".into()])
            .unwrap();
        st.set_bot_spend_limits(user, bot.id, "{\"daily\":1}", &json!({}))
            .unwrap();
        st.set_bot_workspace_limits(user, bot.id, name, Some(10), Some(20), &json!({}))
            .unwrap();
        Owned { bot, chat, run }
    }

    /// Row count of every table, by name (the search table's shadow tables included).
    fn table_counts(st: &Storage) -> Vec<(String, i64)> {
        let conn = st.conn();
        let names: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        names
            .into_iter()
            .map(|n| {
                let c = conn
                    .query_row(&format!("SELECT COUNT(*) FROM \"{n}\""), [], |r| r.get(0))
                    .unwrap();
                (n, c)
            })
            .collect()
    }

    /// No row anywhere points at a row that is gone.
    fn assert_no_orphans(st: &Storage) {
        let dangling: i64 = st
            .conn()
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .query_map([], |_| Ok(()))
            .unwrap()
            .count()
            .try_into()
            .unwrap();
        assert_eq!(dangling, 0, "rows with a foreign key to a deleted row");
        for (table, column, parent) in [
            ("run_steps", "run_id", "runs"),
            ("approvals", "run_id", "runs"),
            ("messages", "chat_id", "chats"),
            ("chats", "bot_id", "bots"),
            ("runs", "bot_id", "bots"),
            ("event_index", "run_id", "runs"),
            ("incoming_webhooks", "bot_id", "bots"),
        ] {
            let orphans: i64 = st
                .conn()
                .query_row(
                    &format!(
                        "SELECT COUNT(*) FROM {table} WHERE {column} NOT IN (SELECT id FROM {parent})"
                    ),
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(orphans, 0, "{table}.{column} has orphans");
        }
    }

    fn setup() -> (Storage, UserId) {
        let st = Storage::open_in_memory(std::env::temp_dir()).unwrap();
        let user = User {
            id: UserId::new(),
            name: "owner".into(),
            created_at_ms: 1,
        };
        st.insert_user(&user).unwrap();
        (st, user.id)
    }

    #[test]
    fn deleting_a_bot_leaves_no_row_of_it_and_does_not_touch_another_bot() {
        let (st, user) = setup();
        let keep = populate(&st, user, "keeper", 1);
        let gone = populate(&st, user, "goner", 2);
        let _ = (&keep.chat, &keep.run);

        let counts_before = st.bot_data_counts(gone.bot.id).unwrap();
        assert_eq!(
            (
                counts_before.chats,
                counts_before.messages,
                counts_before.runs
            ),
            (1, 2, 1)
        );
        assert_eq!(
            (
                counts_before.run_steps,
                counts_before.approvals,
                counts_before.webhooks
            ),
            (1, 1, 1)
        );
        let keeper_counts = st.bot_data_counts(keep.bot.id).unwrap();
        let tables_before = table_counts(&st);

        let out = st
            .delete_bot_cascade(user, gone.bot.id, "owner", |_| Ok(()))
            .unwrap()
            .expect("the bot existed");
        assert_eq!(out.removed, counts_before);
        assert_eq!(out.run_ids, vec![gone.run.id]);
        assert_eq!(out.chat_ids, vec![gone.chat.id]);

        // Nothing of the deleted bot is left, in any table.
        assert!(st.get_bot(gone.bot.id).unwrap().is_none());
        assert_eq!(
            st.bot_data_counts(gone.bot.id).unwrap(),
            BotDataCounts::default()
        );
        assert!(st.get_bot_skill_assignment(gone.bot.id).unwrap().is_none());
        assert!(st.get_bot_spend_limits(gone.bot.id).unwrap().is_none());
        assert!(st.last_gseq().unwrap() >= 1);
        let index: Vec<_> = st.list_event_index(0, 10).unwrap();
        assert_eq!(index.len(), 1);
        assert_eq!(index[0].run_id, keep.run.id);
        // The search rows left with the messages: the shared word now finds only the keeper.
        let hits = st
            .search_messages(user, crate::SearchFilter::default(), "quokka", 0, 10)
            .unwrap();
        assert_eq!(
            hits.len(),
            2,
            "only the keeper's two messages stay searchable"
        );

        assert_no_orphans(&st);

        // Another bot's data is exactly what it was, table by table.
        assert_eq!(st.bot_data_counts(keep.bot.id).unwrap(), keeper_counts);
        let tables_after = table_counts(&st);
        for ((name, before), (_, after)) in tables_before.iter().zip(&tables_after) {
            let expected = match name.as_str() {
                "chats"
                | "runs"
                | "bots"
                | "run_steps"
                | "approvals"
                | "incoming_webhooks"
                | "bot_skill_assignment"
                | "bot_spend_limits"
                | "event_index" => before - 1,
                "messages" => before - 2,
                // The audit log only grows: the delete is recorded, nothing is removed.
                "audit_log" => before + 1,
                _ if name.starts_with("chat_messages_fts") => continue,
                _ => *before,
            };
            assert_eq!(*after, expected, "table {name}");
        }
        // The workspace folder stays reserved; its limits are gone.
        let ws = st.get_bot_workspace(gone.bot.id).unwrap().unwrap();
        assert_eq!((ws.soft_limit_bytes, ws.hard_limit_bytes), (None, None));
        assert_eq!(
            st.get_bot_workspace(keep.bot.id)
                .unwrap()
                .unwrap()
                .soft_limit_bytes,
            Some(10)
        );
        // The audit trail names the deletion and kept the older rows.
        let audit = st.list_audit(user).unwrap();
        let entry = audit.iter().find(|a| a.action == "bot.deleted").unwrap();
        assert_eq!(entry.detail["name"], "goner");
        assert_eq!(entry.detail["chats"], 1);
        assert!(audit.iter().any(|a| a.action == "bot.spend_limits_changed"));
    }

    #[test]
    fn a_running_run_refuses_the_delete_and_changes_nothing() {
        let (st, user) = setup();
        let o = populate(&st, user, "busy", 1);
        let mut live = o.run.clone();
        live.id = RunId::new();
        live.state = "running".into();
        st.insert_run(&live).unwrap();
        assert_eq!(st.bot_data_counts(o.bot.id).unwrap().active_runs, 1);
        let before = table_counts(&st);
        let err = st
            .delete_bot_cascade(user, o.bot.id, "owner", |_| Ok(()))
            .unwrap_err();
        assert!(matches!(err, StorageError::Conflict(_)));
        assert_eq!(table_counts(&st), before);
    }

    #[test]
    fn a_failing_cleanup_rolls_everything_back() {
        let (st, user) = setup();
        let o = populate(&st, user, "rollback", 1);
        let before = table_counts(&st);
        let err = st.delete_bot_cascade(user, o.bot.id, "owner", |_| {
            Err(StorageError::Io(std::io::Error::other("cleanup failed")))
        });
        assert!(err.is_err());
        assert_eq!(table_counts(&st), before);
        assert!(st.get_bot(o.bot.id).unwrap().is_some());
    }

    #[test]
    fn deleting_a_missing_bot_is_none() {
        let (st, user) = setup();
        assert!(
            st.delete_bot_cascade(user, BotId::new(), "owner", |_| Ok(()))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn links_from_another_bots_rows_are_cleared_not_followed() {
        let (st, user) = setup();
        let a = populate(&st, user, "a", 1);
        let b = populate(&st, user, "b", 2);
        // A run of `b` points at a chat and a parent run of `a`.
        let mut child = b.run.clone();
        child.id = RunId::new();
        child.chat_id = Some(a.chat.id);
        child.parent_run_id = Some(a.run.id);
        st.insert_run(&child).unwrap();
        st.delete_bot_cascade(user, a.bot.id, "owner", |_| Ok(()))
            .unwrap()
            .unwrap();
        let kept = st.get_run(child.id).unwrap().unwrap();
        assert_eq!((kept.chat_id, kept.parent_run_id), (None, None));
    }
}
