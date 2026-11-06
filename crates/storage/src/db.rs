//! The [`Storage`] repository.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use protocol::{BotId, ChatId, RunId, UserId};
use rusqlite::types::Type;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde_json::Value;
use uuid::Uuid;

use crate::error::{Result, StorageError};
use crate::migrate;
use crate::records::{Approval, AuditEntry, Bot, Chat, Message, Run, RunStep, User};

/// Current Unix time in milliseconds.
#[must_use]
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
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

fn json_at(row: &Row<'_>, idx: usize) -> rusqlite::Result<Value> {
    let s: String = row.get(idx)?;
    serde_json::from_str(&s).map_err(|e| conv_err(idx, e))
}

fn opt_json_at(row: &Row<'_>, idx: usize) -> rusqlite::Result<Option<Value>> {
    let s: Option<String> = row.get(idx)?;
    s.map(|s| serde_json::from_str(&s).map_err(|e| conv_err(idx, e)))
        .transpose()
}

fn to_u32(row: &Row<'_>, idx: usize) -> rusqlite::Result<u32> {
    let v: i64 = row.get(idx)?;
    u32::try_from(v).map_err(|e| conv_err(idx, e))
}

fn opt_json_text(v: Option<&Value>) -> Result<Option<String>> {
    Ok(v.map(serde_json::to_string).transpose()?)
}

/// Synchronous SQLite repository.  Cheap to share behind an `Arc`; calls
/// serialise on an internal mutex.
#[derive(Debug)]
pub struct Storage {
    conn: Mutex<Connection>,
    data_dir: PathBuf,
}

impl Storage {
    /// Open (creating if needed) `<data_dir>/bot.db`, enable WAL + foreign
    /// keys and run pending migrations.
    ///
    /// # Errors
    /// Fails on filesystem, SQLite or migration errors.
    pub fn open(data_dir: impl Into<PathBuf>) -> Result<Self> {
        let data_dir = data_dir.into();
        std::fs::create_dir_all(&data_dir)?;
        let conn = Connection::open(data_dir.join("bot.db"))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Self::init(conn, data_dir)
    }

    /// In-memory database (tests); `data_dir` is still used for event logs.
    ///
    /// # Errors
    /// Fails on SQLite or migration errors.
    pub fn open_in_memory(data_dir: impl Into<PathBuf>) -> Result<Self> {
        Self::init(Connection::open_in_memory()?, data_dir.into())
    }

    fn init(mut conn: Connection, data_dir: PathBuf) -> Result<Self> {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        migrate::migrate(&mut conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            data_dir,
        })
    }

    /// Data directory (event logs live under `runs/`).
    #[must_use]
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    // ---- users ----

    /// Insert a user.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn insert_user(&self, u: &User) -> Result<()> {
        self.conn().execute(
            "INSERT INTO users (id, name, created_at_ms) VALUES (?1, ?2, ?3)",
            params![u.id.to_string(), u.name, u.created_at_ms],
        )?;
        Ok(())
    }

    /// Fetch a user.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn get_user(&self, id: UserId) -> Result<Option<User>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT id, name, created_at_ms FROM users WHERE id = ?1",
                [id.to_string()],
                user_row,
            )
            .optional()?)
    }

    /// List all users, oldest first.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn list_users(&self) -> Result<Vec<User>> {
        let conn = self.conn();
        let mut st = conn.prepare("SELECT id, name, created_at_ms FROM users ORDER BY id")?;
        Ok(st
            .query_map([], user_row)?
            .collect::<rusqlite::Result<_>>()?)
    }

    // ---- bots ----

    /// Insert a bot.
    ///
    /// # Errors
    /// Fails on SQLite/JSON errors.
    pub fn insert_bot(&self, b: &Bot) -> Result<()> {
        self.conn().execute(
            "INSERT INTO bots (id, user_id, name, config_json, created_at_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                b.id.to_string(),
                b.user_id.to_string(),
                b.name,
                serde_json::to_string(&b.config)?,
                b.created_at_ms
            ],
        )?;
        Ok(())
    }

    /// Fetch a bot.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn get_bot(&self, id: BotId) -> Result<Option<Bot>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT id, user_id, name, config_json, created_at_ms FROM bots WHERE id = ?1",
                [id.to_string()],
                bot_row,
            )
            .optional()?)
    }

    /// List a user's bots.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn list_bots(&self, user_id: UserId) -> Result<Vec<Bot>> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT id, user_id, name, config_json, created_at_ms FROM bots WHERE user_id = ?1 ORDER BY id",
        )?;
        Ok(st
            .query_map([user_id.to_string()], bot_row)?
            .collect::<rusqlite::Result<_>>()?)
    }

    // ---- chats ----

    /// Insert a chat.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn insert_chat(&self, c: &Chat) -> Result<()> {
        self.conn().execute(
            "INSERT INTO chats (id, user_id, bot_id, title, created_at_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                c.id.to_string(),
                c.user_id.to_string(),
                c.bot_id.to_string(),
                c.title,
                c.created_at_ms
            ],
        )?;
        Ok(())
    }

    /// Fetch a chat.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn get_chat(&self, id: ChatId) -> Result<Option<Chat>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT id, user_id, bot_id, title, created_at_ms FROM chats WHERE id = ?1",
                [id.to_string()],
                chat_row,
            )
            .optional()?)
    }

    /// List a user's chats.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn list_chats(&self, user_id: UserId) -> Result<Vec<Chat>> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT id, user_id, bot_id, title, created_at_ms FROM chats WHERE user_id = ?1 ORDER BY id",
        )?;
        Ok(st
            .query_map([user_id.to_string()], chat_row)?
            .collect::<rusqlite::Result<_>>()?)
    }

    // ---- messages ----

    /// Insert a message.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn insert_message(&self, m: &Message) -> Result<()> {
        self.conn().execute(
            "INSERT INTO messages (id, user_id, chat_id, run_id, role, content, created_at_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                m.id.to_string(),
                m.user_id.to_string(),
                m.chat_id.to_string(),
                m.run_id.map(|r| r.to_string()),
                m.role,
                m.content,
                m.created_at_ms
            ],
        )?;
        Ok(())
    }

    /// Fetch a message.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn get_message(&self, id: Uuid) -> Result<Option<Message>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT id, user_id, chat_id, run_id, role, content, created_at_ms FROM messages WHERE id = ?1",
                [id.to_string()],
                message_row,
            )
            .optional()?)
    }

    /// List a chat's messages in order.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn list_messages(&self, chat_id: ChatId) -> Result<Vec<Message>> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT id, user_id, chat_id, run_id, role, content, created_at_ms FROM messages WHERE chat_id = ?1 ORDER BY created_at_ms, id",
        )?;
        Ok(st
            .query_map([chat_id.to_string()], message_row)?
            .collect::<rusqlite::Result<_>>()?)
    }

    // ---- runs ----

    /// Insert a run.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn insert_run(&self, r: &Run) -> Result<()> {
        self.conn().execute(
            "INSERT INTO runs (id, user_id, bot_id, chat_id, kind, state, error, created_at_ms, updated_at_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                r.id.to_string(),
                r.user_id.to_string(),
                r.bot_id.to_string(),
                r.chat_id.map(|c| c.to_string()),
                r.kind,
                r.state,
                r.error,
                r.created_at_ms,
                r.updated_at_ms
            ],
        )?;
        Ok(())
    }

    /// Fetch a run.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn get_run(&self, id: RunId) -> Result<Option<Run>> {
        Ok(self
            .conn()
            .query_row(
                &format!("{RUN_SELECT} WHERE id = ?1"),
                [id.to_string()],
                run_row,
            )
            .optional()?)
    }

    /// List a user's runs.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn list_runs(&self, user_id: UserId) -> Result<Vec<Run>> {
        let conn = self.conn();
        let mut st = conn.prepare(&format!("{RUN_SELECT} WHERE user_id = ?1 ORDER BY id"))?;
        Ok(st
            .query_map([user_id.to_string()], run_row)?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// List runs (all users) currently in the given serialised state.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn list_runs_by_state(&self, state: &str) -> Result<Vec<Run>> {
        let conn = self.conn();
        let mut st = conn.prepare(&format!("{RUN_SELECT} WHERE state = ?1 ORDER BY id"))?;
        Ok(st
            .query_map([state], run_row)?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Set a run's state (and error text).  Transition legality is the
    /// caller's responsibility.
    ///
    /// # Errors
    /// Fails on SQLite errors or if the run does not exist.
    pub fn update_run_state(&self, id: RunId, state: &str, error: Option<&str>) -> Result<()> {
        let n = self.conn().execute(
            "UPDATE runs SET state = ?2, error = ?3, updated_at_ms = ?4 WHERE id = ?1",
            params![id.to_string(), state, error, now_ms()],
        )?;
        if n == 0 {
            return Err(StorageError::NotFound(format!("run {id}")));
        }
        Ok(())
    }

    // ---- run_steps ----

    /// Insert a step (the persisted intent).
    ///
    /// # Errors
    /// Fails on SQLite/JSON errors.
    pub fn insert_step(&self, s: &RunStep) -> Result<()> {
        self.conn().execute(
            "INSERT INTO run_steps (id, run_id, user_id, idx, name, input_json, idempotent, retry_json, status, attempt, output_json, error, started_at_ms, finished_at_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                s.id.to_string(),
                s.run_id.to_string(),
                s.user_id.to_string(),
                s.idx,
                s.name,
                serde_json::to_string(&s.input)?,
                s.idempotent,
                opt_json_text(s.retry.as_ref())?,
                s.status,
                s.attempt,
                opt_json_text(s.output.as_ref())?,
                s.error,
                s.started_at_ms,
                s.finished_at_ms
            ],
        )?;
        Ok(())
    }

    /// Fetch a step.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn get_step(&self, id: Uuid) -> Result<Option<RunStep>> {
        Ok(self
            .conn()
            .query_row(
                &format!("{STEP_SELECT} WHERE id = ?1"),
                [id.to_string()],
                step_row,
            )
            .optional()?)
    }

    /// List a run's steps in order.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn list_steps(&self, run_id: RunId) -> Result<Vec<RunStep>> {
        let conn = self.conn();
        let mut st = conn.prepare(&format!("{STEP_SELECT} WHERE run_id = ?1 ORDER BY idx"))?;
        Ok(st
            .query_map([run_id.to_string()], step_row)?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Update a step's status/attempt/result.  `finished_at_ms` is set to now
    /// when `finished` is true, else cleared.
    ///
    /// # Errors
    /// Fails on SQLite/JSON errors or if the step does not exist.
    pub fn update_step(
        &self,
        id: Uuid,
        status: &str,
        attempt: u32,
        output: Option<&Value>,
        error: Option<&str>,
        finished: bool,
    ) -> Result<()> {
        let n = self.conn().execute(
            "UPDATE run_steps SET status = ?2, attempt = ?3, output_json = ?4, error = ?5, finished_at_ms = ?6 WHERE id = ?1",
            params![
                id.to_string(),
                status,
                attempt,
                opt_json_text(output)?,
                error,
                finished.then(now_ms)
            ],
        )?;
        if n == 0 {
            return Err(StorageError::NotFound(format!("step {id}")));
        }
        Ok(())
    }

    /// Re-label every step of `run_id` whose status is `from` as `to`;
    /// returns the number changed.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn relabel_steps(&self, run_id: RunId, from: &str, to: &str) -> Result<usize> {
        Ok(self.conn().execute(
            "UPDATE run_steps SET status = ?3, finished_at_ms = ?4 WHERE run_id = ?1 AND status = ?2",
            params![run_id.to_string(), from, to, now_ms()],
        )?)
    }

    // ---- approvals ----

    /// Insert an approval request.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn insert_approval(&self, a: &Approval) -> Result<()> {
        self.conn().execute(
            "INSERT INTO approvals (id, user_id, run_id, step_id, summary, decision, requested_at_ms, decided_at_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                a.id.to_string(),
                a.user_id.to_string(),
                a.run_id.to_string(),
                a.step_id.map(|s| s.to_string()),
                a.summary,
                a.decision,
                a.requested_at_ms,
                a.decided_at_ms
            ],
        )?;
        Ok(())
    }

    /// Fetch an approval.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn get_approval(&self, id: Uuid) -> Result<Option<Approval>> {
        Ok(self
            .conn()
            .query_row(
                &format!("{APPROVAL_SELECT} WHERE id = ?1"),
                [id.to_string()],
                approval_row,
            )
            .optional()?)
    }

    /// List a run's approvals.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn list_approvals(&self, run_id: RunId) -> Result<Vec<Approval>> {
        let conn = self.conn();
        let mut st = conn.prepare(&format!("{APPROVAL_SELECT} WHERE run_id = ?1 ORDER BY id"))?;
        Ok(st
            .query_map([run_id.to_string()], approval_row)?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Record the decision on a pending approval.
    ///
    /// # Errors
    /// Fails on SQLite errors or if the approval does not exist / is decided.
    pub fn decide_approval(&self, id: Uuid, decision: &str) -> Result<()> {
        let n = self.conn().execute(
            "UPDATE approvals SET decision = ?2, decided_at_ms = ?3 WHERE id = ?1 AND decision IS NULL",
            params![id.to_string(), decision, now_ms()],
        )?;
        if n == 0 {
            return Err(StorageError::NotFound(format!("pending approval {id}")));
        }
        Ok(())
    }

    // ---- audit_log (append-only: no update/delete API) ----

    /// Append an audit entry; returns its id.
    ///
    /// # Errors
    /// Fails on SQLite/JSON errors.
    pub fn append_audit(
        &self,
        user_id: UserId,
        run_id: Option<RunId>,
        actor: &str,
        action: &str,
        detail: &Value,
    ) -> Result<i64> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO audit_log (user_id, run_id, actor, action, detail_json, ts_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                user_id.to_string(),
                run_id.map(|r| r.to_string()),
                actor,
                action,
                serde_json::to_string(detail)?,
                now_ms()
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// List a user's audit entries in insertion order.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn list_audit(&self, user_id: UserId) -> Result<Vec<AuditEntry>> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT id, user_id, run_id, actor, action, detail_json, ts_ms FROM audit_log WHERE user_id = ?1 ORDER BY id",
        )?;
        Ok(st
            .query_map([user_id.to_string()], audit_row)?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Run a raw statement (test hook to prove schema-level guarantees).
    #[cfg(test)]
    pub(crate) fn raw_execute(&self, sql: &str) -> rusqlite::Result<usize> {
        self.conn().execute(sql, [])
    }
}

const RUN_SELECT: &str = "SELECT id, user_id, bot_id, chat_id, kind, state, error, created_at_ms, updated_at_ms FROM runs";
const STEP_SELECT: &str = "SELECT id, run_id, user_id, idx, name, input_json, idempotent, retry_json, status, attempt, output_json, error, started_at_ms, finished_at_ms FROM run_steps";
const APPROVAL_SELECT: &str = "SELECT id, user_id, run_id, step_id, summary, decision, requested_at_ms, decided_at_ms FROM approvals";

fn user_row(r: &Row<'_>) -> rusqlite::Result<User> {
    Ok(User {
        id: UserId(uuid_at(r, 0)?),
        name: r.get(1)?,
        created_at_ms: r.get(2)?,
    })
}

fn bot_row(r: &Row<'_>) -> rusqlite::Result<Bot> {
    Ok(Bot {
        id: BotId(uuid_at(r, 0)?),
        user_id: UserId(uuid_at(r, 1)?),
        name: r.get(2)?,
        config: json_at(r, 3)?,
        created_at_ms: r.get(4)?,
    })
}

fn chat_row(r: &Row<'_>) -> rusqlite::Result<Chat> {
    Ok(Chat {
        id: ChatId(uuid_at(r, 0)?),
        user_id: UserId(uuid_at(r, 1)?),
        bot_id: BotId(uuid_at(r, 2)?),
        title: r.get(3)?,
        created_at_ms: r.get(4)?,
    })
}

fn message_row(r: &Row<'_>) -> rusqlite::Result<Message> {
    Ok(Message {
        id: uuid_at(r, 0)?,
        user_id: UserId(uuid_at(r, 1)?),
        chat_id: ChatId(uuid_at(r, 2)?),
        run_id: opt_uuid_at(r, 3)?.map(RunId),
        role: r.get(4)?,
        content: r.get(5)?,
        created_at_ms: r.get(6)?,
    })
}

fn run_row(r: &Row<'_>) -> rusqlite::Result<Run> {
    Ok(Run {
        id: RunId(uuid_at(r, 0)?),
        user_id: UserId(uuid_at(r, 1)?),
        bot_id: BotId(uuid_at(r, 2)?),
        chat_id: opt_uuid_at(r, 3)?.map(ChatId),
        kind: r.get(4)?,
        state: r.get(5)?,
        error: r.get(6)?,
        created_at_ms: r.get(7)?,
        updated_at_ms: r.get(8)?,
    })
}

fn step_row(r: &Row<'_>) -> rusqlite::Result<RunStep> {
    Ok(RunStep {
        id: uuid_at(r, 0)?,
        run_id: RunId(uuid_at(r, 1)?),
        user_id: UserId(uuid_at(r, 2)?),
        idx: to_u32(r, 3)?,
        name: r.get(4)?,
        input: json_at(r, 5)?,
        idempotent: r.get(6)?,
        retry: opt_json_at(r, 7)?,
        status: r.get(8)?,
        attempt: to_u32(r, 9)?,
        output: opt_json_at(r, 10)?,
        error: r.get(11)?,
        started_at_ms: r.get(12)?,
        finished_at_ms: r.get(13)?,
    })
}

fn approval_row(r: &Row<'_>) -> rusqlite::Result<Approval> {
    Ok(Approval {
        id: uuid_at(r, 0)?,
        user_id: UserId(uuid_at(r, 1)?),
        run_id: RunId(uuid_at(r, 2)?),
        step_id: opt_uuid_at(r, 3)?,
        summary: r.get(4)?,
        decision: r.get(5)?,
        requested_at_ms: r.get(6)?,
        decided_at_ms: r.get(7)?,
    })
}

fn audit_row(r: &Row<'_>) -> rusqlite::Result<AuditEntry> {
    Ok(AuditEntry {
        id: r.get(0)?,
        user_id: UserId(uuid_at(r, 1)?),
        run_id: opt_uuid_at(r, 2)?.map(RunId),
        actor: r.get(3)?,
        action: r.get(4)?,
        detail: json_at(r, 5)?,
        ts_ms: r.get(6)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Fx {
        st: Storage,
        user: User,
        bot: Bot,
        chat: Chat,
    }

    fn fx() -> Fx {
        let st = Storage::open_in_memory(std::env::temp_dir()).unwrap();
        let user = User {
            id: UserId::new(),
            name: "emre".into(),
            created_at_ms: 1,
        };
        st.insert_user(&user).unwrap();
        let bot = Bot {
            id: BotId::new(),
            user_id: user.id,
            name: "b".into(),
            config: json!({"model": "x"}),
            created_at_ms: 2,
        };
        st.insert_bot(&bot).unwrap();
        let chat = Chat {
            id: ChatId::new(),
            user_id: user.id,
            bot_id: bot.id,
            title: "t".into(),
            created_at_ms: 3,
        };
        st.insert_chat(&chat).unwrap();
        Fx {
            st,
            user,
            bot,
            chat,
        }
    }

    fn run(f: &Fx) -> Run {
        let r = Run {
            id: RunId::new(),
            user_id: f.user.id,
            bot_id: f.bot.id,
            chat_id: Some(f.chat.id),
            kind: "turn".into(),
            state: "queued".into(),
            error: None,
            created_at_ms: 4,
            updated_at_ms: 4,
        };
        f.st.insert_run(&r).unwrap();
        r
    }

    #[test]
    fn user_bot_chat_message_round_trip() {
        let f = fx();
        assert_eq!(f.st.get_user(f.user.id).unwrap().unwrap(), f.user);
        assert_eq!(f.st.list_users().unwrap(), vec![f.user.clone()]);
        assert_eq!(f.st.get_bot(f.bot.id).unwrap().unwrap(), f.bot);
        assert_eq!(f.st.list_bots(f.user.id).unwrap(), vec![f.bot.clone()]);
        assert_eq!(f.st.get_chat(f.chat.id).unwrap().unwrap(), f.chat);
        assert_eq!(f.st.list_chats(f.user.id).unwrap(), vec![f.chat.clone()]);
        let r = run(&f);
        let m = Message {
            id: Uuid::now_v7(),
            user_id: f.user.id,
            chat_id: f.chat.id,
            run_id: Some(r.id),
            role: "user".into(),
            content: "hi".into(),
            created_at_ms: 5,
        };
        f.st.insert_message(&m).unwrap();
        assert_eq!(f.st.get_message(m.id).unwrap().unwrap(), m);
        assert_eq!(f.st.list_messages(f.chat.id).unwrap(), vec![m]);
        assert!(f.st.get_user(UserId::new()).unwrap().is_none());
    }

    #[test]
    fn user_isolation_in_lists() {
        let f = fx();
        let other = User {
            id: UserId::new(),
            name: "o".into(),
            created_at_ms: 1,
        };
        f.st.insert_user(&other).unwrap();
        assert_eq!(f.st.list_bots(other.id).unwrap().len(), 0);
        assert_eq!(f.st.list_chats(other.id).unwrap().len(), 0);
        run(&f);
        assert_eq!(f.st.list_runs(other.id).unwrap().len(), 0);
    }

    #[test]
    fn foreign_keys_enforced() {
        let f = fx();
        let bad = Bot {
            id: BotId::new(),
            user_id: UserId::new(),
            name: "x".into(),
            config: json!({}),
            created_at_ms: 0,
        };
        assert!(f.st.insert_bot(&bad).is_err());
    }

    #[test]
    fn run_state_and_steps() {
        let f = fx();
        let r = run(&f);
        f.st.update_run_state(r.id, "failed", Some("boom")).unwrap();
        let got = f.st.get_run(r.id).unwrap().unwrap();
        assert_eq!(
            (got.state.as_str(), got.error.as_deref()),
            ("failed", Some("boom"))
        );
        assert_eq!(f.st.list_runs_by_state("failed").unwrap().len(), 1);
        assert_eq!(f.st.list_runs_by_state("queued").unwrap().len(), 0);
        assert!(matches!(
            f.st.update_run_state(RunId::new(), "x", None),
            Err(StorageError::NotFound(_))
        ));

        let s = RunStep {
            id: Uuid::now_v7(),
            run_id: r.id,
            user_id: f.user.id,
            idx: 0,
            name: "s".into(),
            input: json!({"a": 1}),
            idempotent: true,
            retry: Some(json!({"max_attempts": 3})),
            status: "intent".into(),
            attempt: 1,
            output: None,
            error: None,
            started_at_ms: 6,
            finished_at_ms: None,
        };
        f.st.insert_step(&s).unwrap();
        assert_eq!(f.st.get_step(s.id).unwrap().unwrap(), s);
        let dup = RunStep {
            id: Uuid::now_v7(),
            ..s.clone()
        };
        assert!(f.st.insert_step(&dup).is_err(), "(run_id, idx) is unique");
        f.st.update_step(s.id, "succeeded", 1, Some(&json!("ok")), None, true)
            .unwrap();
        let got = f.st.list_steps(r.id).unwrap().remove(0);
        assert_eq!(got.status, "succeeded");
        assert_eq!(got.output, Some(json!("ok")));
        assert!(got.finished_at_ms.is_some());
        assert_eq!(f.st.relabel_steps(r.id, "succeeded", "x").unwrap(), 1);
    }

    #[test]
    fn approvals_decide_once() {
        let f = fx();
        let r = run(&f);
        let a = Approval {
            id: Uuid::now_v7(),
            user_id: f.user.id,
            run_id: r.id,
            step_id: None,
            summary: "send mail".into(),
            decision: None,
            requested_at_ms: 7,
            decided_at_ms: None,
        };
        f.st.insert_approval(&a).unwrap();
        assert_eq!(f.st.get_approval(a.id).unwrap().unwrap(), a);
        f.st.decide_approval(a.id, "approved").unwrap();
        assert!(f.st.decide_approval(a.id, "denied").is_err());
        let got = f.st.list_approvals(r.id).unwrap().remove(0);
        assert_eq!(got.decision.as_deref(), Some("approved"));
        assert!(got.decided_at_ms.is_some());
    }

    #[test]
    fn audit_log_is_append_only() {
        let f = fx();
        let id =
            f.st.append_audit(f.user.id, None, "system", "boot", &json!({"k": 1}))
                .unwrap();
        let entries = f.st.list_audit(f.user.id).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, id);
        assert_eq!(entries[0].detail, json!({"k": 1}));
        assert!(
            f.st.raw_execute("UPDATE audit_log SET actor = 'x'")
                .is_err()
        );
        assert!(f.st.raw_execute("DELETE FROM audit_log").is_err());
        assert_eq!(f.st.list_audit(f.user.id).unwrap().len(), 1);
    }

    #[test]
    fn file_db_uses_wal_and_reopens() {
        let dir = tempfile::tempdir().unwrap();
        let user = User {
            id: UserId::new(),
            name: "u".into(),
            created_at_ms: 1,
        };
        {
            let st = Storage::open(dir.path()).unwrap();
            let mode: String = st
                .conn()
                .query_row("PRAGMA journal_mode", [], |r| r.get(0))
                .unwrap();
            assert_eq!(mode, "wal");
            st.insert_user(&user).unwrap();
        }
        let st = Storage::open(dir.path()).unwrap();
        assert_eq!(st.get_user(user.id).unwrap().unwrap(), user);
    }
}
