//! `MemoryStore`: connection, schema, CRUD and journaling.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use protocol::{BotId, ChatId, UserId};
use rusqlite::{Connection, OptionalExtension, Row, params};
use uuid::Uuid;

use crate::embed::{Embedder, normalize};
use crate::error::{MemoryError, Result};
use crate::recall::RecallConfig;
use crate::secrets::contains_secret;
use crate::types::{JournalEntry, JournalOp, Kind, Memory, MemoryPatch, NewMemory, Trust};
use crate::vec::{register, to_blob};

/// Embedded migrations; version = index + 1.
const MIGRATIONS: &[(&str, &str)] =
    &[("0001_memory", include_str!("../migrations/0001_memory.sql"))];

pub(crate) const COLS: &str = "rowid, id, user_id, bot_id, kind, content, source_quote, \
    source_chat_id, source_message_id, trust, importance, created_ms, last_used_ms, \
    valid_until_ms, deleted";

/// Current time in Unix milliseconds.
#[must_use]
pub(crate) fn now_ms() -> i64 {
    storage::now_ms()
}

/// Hybrid memory database.  Cheap to share behind an `Arc`; calls serialise on
/// an internal mutex.
pub struct MemoryStore {
    conn: Mutex<Connection>,
    pub(crate) embedder: Arc<dyn Embedder>,
    pub(crate) config: RecallConfig,
}

impl std::fmt::Debug for MemoryStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryStore").finish_non_exhaustive()
    }
}

fn conv_err(msg: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, msg.into())
}

fn uuid_col(s: &str) -> rusqlite::Result<Uuid> {
    Uuid::parse_str(s).map_err(|e| conv_err(e.to_string()))
}

pub(crate) fn row_to_memory(r: &Row<'_>) -> rusqlite::Result<(i64, Memory)> {
    let opt = |i: usize| -> rusqlite::Result<Option<Uuid>> {
        r.get::<_, Option<String>>(i)?
            .as_deref()
            .map(uuid_col)
            .transpose()
    };
    let kind: String = r.get(4)?;
    let trust: String = r.get(9)?;
    #[allow(clippy::cast_possible_truncation)]
    let importance = r.get::<_, f64>(10)? as f32;
    Ok((
        r.get(0)?,
        Memory {
            id: uuid_col(&r.get::<_, String>(1)?)?,
            user_id: UserId(uuid_col(&r.get::<_, String>(2)?)?),
            bot_id: opt(3)?.map(BotId),
            kind: Kind::parse(&kind).ok_or_else(|| conv_err(format!("kind {kind}")))?,
            content: r.get(5)?,
            source_quote: r.get(6)?,
            source_chat_id: opt(7)?.map(ChatId),
            source_message_id: opt(8)?,
            trust: Trust::parse(&trust).ok_or_else(|| conv_err(format!("trust {trust}")))?,
            importance,
            created_ms: r.get(11)?,
            last_used_ms: r.get(12)?,
            valid_until_ms: r.get(13)?,
            deleted: r.get::<_, i64>(14)? != 0,
        },
    ))
}

impl MemoryStore {
    /// Validate and migrate an existing staged database without loading an embedder.
    /// Returns the resulting schema version and preserves the stored vector dimension.
    /// Never creates a missing database.
    ///
    /// # Errors
    /// Fails on corruption, an incompatible schema, or migration errors.
    pub fn validate_and_migrate(path: impl AsRef<Path>) -> Result<u32> {
        register();
        let mut conn =
            Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let integrity: String = conn.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
        if integrity != "ok" {
            return Err(MemoryError::Invalid(format!(
                "database integrity check: {integrity}"
            )));
        }
        migrate(&mut conn)?;
        conn.prepare(&format!("SELECT {COLS} FROM memories"))?;
        conn.prepare("SELECT key, value FROM meta")?;
        conn.prepare("SELECT id, memory_id, user_id, op, before_json, after_json, reason, ts_ms, undone FROM memory_journal")?;
        conn.prepare("SELECT rowid, message_id, user_id, chat_id, role, content, created_ms FROM session_msgs")?;
        conn.prepare("SELECT rowid, content FROM memories_fts")?;
        conn.prepare("SELECT rowid, content FROM session_fts")?;
        Ok(conn.query_row("PRAGMA user_version", [], |row| row.get(0))?)
    }

    /// Open (creating if needed) `<data_dir>/memory.db`.
    ///
    /// # Errors
    /// Fails on filesystem/SQLite/migration errors or an embedding-dimension mismatch.
    pub fn open(data_dir: impl AsRef<Path>, embedder: Arc<dyn Embedder>) -> Result<Self> {
        std::fs::create_dir_all(data_dir.as_ref())?;
        register();
        let conn = Connection::open(data_dir.as_ref().join("memory.db"))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Self::init(conn, embedder)
    }

    /// In-memory database (tests).
    ///
    /// # Errors
    /// Fails on SQLite/migration errors.
    pub fn open_in_memory(embedder: Arc<dyn Embedder>) -> Result<Self> {
        register();
        Self::init(Connection::open_in_memory()?, embedder)
    }

    fn init(mut conn: Connection, embedder: Arc<dyn Embedder>) -> Result<Self> {
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        migrate(&mut conn)?;
        let dim = embedder.dim();
        let stored: Option<String> = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'embedding_dim'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        match stored.and_then(|s| s.parse::<usize>().ok()) {
            Some(db) if db != dim => {
                return Err(MemoryError::DimensionMismatch { db, embedder: dim });
            }
            Some(_) => {}
            None => {
                conn.execute(
                    "INSERT INTO meta(key, value) VALUES ('embedding_dim', ?1)",
                    [dim.to_string()],
                )?;
            }
        }
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE IF NOT EXISTS memories_vec USING vec0(embedding float[{dim}])"
        ))?;
        Ok(Self {
            conn: Mutex::new(conn),
            embedder,
            config: RecallConfig::default(),
        })
    }

    /// Replace the retrieval tuning.
    #[must_use]
    pub fn with_recall_config(mut self, config: RecallConfig) -> Self {
        self.config = config;
        self
    }

    pub(crate) fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn embed_one(&self, text: &str) -> Result<Vec<f32>> {
        let mut out = self.embedder.embed(&[text])?;
        let mut v = out
            .pop()
            .ok_or_else(|| MemoryError::Embed("embedder returned no vector".into()))?;
        if v.len() != self.embedder.dim() {
            return Err(MemoryError::Embed(format!(
                "expected {} dims, got {}",
                self.embedder.dim(),
                v.len()
            )));
        }
        normalize(&mut v);
        Ok(v)
    }

    /// Insert a memory and journal it.
    ///
    /// # Errors
    /// `Invalid` for empty content or secrets; storage/embedder failures otherwise.
    pub fn add(&self, new: &NewMemory, reason: &str) -> Result<Memory> {
        validate_text(&new.content, new.source_quote.as_deref())?;
        let now = now_ms();
        let mem = Memory {
            id: Uuid::now_v7(),
            user_id: new.user_id,
            bot_id: new.bot_id,
            kind: new.kind,
            content: new.content.trim().to_owned(),
            source_quote: new.source_quote.clone(),
            source_chat_id: new.source_chat_id,
            source_message_id: new.source_message_id,
            trust: new.trust,
            importance: new.importance.clamp(0.0, 1.0),
            created_ms: now,
            last_used_ms: now,
            valid_until_ms: new.valid_until_ms,
            deleted: false,
        };
        let emb = self.embed_one(&mem.content)?;
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO memories (id, user_id, bot_id, kind, content, source_quote, \
             source_chat_id, source_message_id, trust, importance, created_ms, last_used_ms, \
             valid_until_ms, deleted) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,0)",
            params![
                mem.id.to_string(),
                mem.user_id.0.to_string(),
                mem.bot_id.map(|b| b.0.to_string()),
                mem.kind.as_str(),
                mem.content,
                mem.source_quote,
                mem.source_chat_id.map(|c| c.0.to_string()),
                mem.source_message_id.map(|u| u.to_string()),
                mem.trust.as_str(),
                f64::from(mem.importance),
                mem.created_ms,
                mem.last_used_ms,
                mem.valid_until_ms,
            ],
        )?;
        let rowid = tx.last_insert_rowid();
        index(&tx, rowid, &mem.content, &emb)?;
        journal_insert(&tx, &mem, JournalOp::Add, None, Some(&mem), reason)?;
        tx.commit()?;
        Ok(mem)
    }

    /// Fetch one memory (any user; internal use) including deleted rows.
    ///
    /// # Errors
    /// SQLite failure.
    pub fn get(&self, id: Uuid) -> Result<Option<Memory>> {
        let conn = self.conn();
        Ok(get_in(&conn, id)?.map(|(_, m)| m))
    }

    /// Update fields of a live memory owned by `user_id`; journals before/after.
    ///
    /// # Errors
    /// `NotFound` if missing/deleted/not owned; `Invalid` for secrets/empty content.
    pub fn update(
        &self,
        user_id: UserId,
        id: Uuid,
        patch: &MemoryPatch,
        reason: &str,
    ) -> Result<Memory> {
        if let Some(c) = &patch.content {
            validate_text(c, patch.source_quote.as_deref())?;
        } else if let Some(q) = &patch.source_quote {
            validate_text("x", Some(q))?;
        }
        let emb = patch
            .content
            .as_deref()
            .map(|c| self.embed_one(c.trim()))
            .transpose()?;
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let (rowid, before) = live_owned(&tx, user_id, id)?;
        let mut after = before.clone();
        if let Some(c) = &patch.content {
            c.trim().clone_into(&mut after.content);
        }
        if let Some(q) = &patch.source_quote {
            after.source_quote = Some(q.clone());
        }
        if let Some(c) = patch.source_chat_id {
            after.source_chat_id = Some(c);
        }
        if let Some(m) = patch.source_message_id {
            after.source_message_id = Some(m);
        }
        if let Some(t) = patch.trust {
            after.trust = t;
        }
        if let Some(i) = patch.importance {
            after.importance = i.clamp(0.0, 1.0);
        }
        if let Some(v) = patch.valid_until_ms {
            after.valid_until_ms = v;
        }
        write_snapshot(&tx, &after)?;
        if let Some(e) = emb {
            index(&tx, rowid, &after.content, &e)?;
        }
        journal_insert(
            &tx,
            &after,
            JournalOp::Update,
            Some(&before),
            Some(&after),
            reason,
        )?;
        tx.commit()?;
        Ok(after)
    }

    /// Soft-delete a live memory owned by `user_id`.
    ///
    /// # Errors
    /// `NotFound` if missing/already deleted/not owned.
    pub fn delete(&self, user_id: UserId, id: Uuid, reason: &str) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let (rowid, before) = live_owned(&tx, user_id, id)?;
        let mut after = before.clone();
        after.deleted = true;
        write_snapshot(&tx, &after)?;
        unindex(&tx, rowid)?;
        journal_insert(
            &tx,
            &after,
            JournalOp::Delete,
            Some(&before),
            Some(&after),
            reason,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Restore the pre-change state recorded by journal entry `journal_id`.
    /// Only the newest not-yet-undone entry of a memory can be undone.
    ///
    /// # Errors
    /// `NotFound` for unknown/foreign entries; `Invalid` if already undone or
    /// not the newest active entry for that memory.
    pub fn undo(&self, user_id: UserId, journal_id: i64) -> Result<Memory> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let entry = journal_get(&tx, journal_id)?
            .filter(|e| e.0 == user_id)
            .ok_or_else(|| MemoryError::NotFound(format!("journal entry {journal_id}")))?
            .1;
        if entry.undone {
            return Err(MemoryError::Invalid("journal entry already undone".into()));
        }
        let newest: i64 = tx.query_row(
            "SELECT COALESCE(MAX(id), 0) FROM memory_journal WHERE memory_id = ?1 AND undone = 0",
            [entry.memory_id.to_string()],
            |r| r.get(0),
        )?;
        if newest != journal_id {
            return Err(MemoryError::Invalid(
                "only the newest active journal entry of a memory can be undone".into(),
            ));
        }
        let (rowid, _) = get_in(&tx, entry.memory_id)?
            .ok_or_else(|| MemoryError::NotFound(entry.memory_id.to_string()))?;
        let target = match (entry.op, entry.before) {
            (JournalOp::Add, _) => {
                let mut m = entry
                    .after
                    .ok_or_else(|| MemoryError::Invalid("journal entry lacks snapshot".into()))?;
                m.deleted = true;
                m
            }
            (_, Some(b)) => b,
            (_, None) => return Err(MemoryError::Invalid("journal entry lacks snapshot".into())),
        };
        write_snapshot(&tx, &target)?;
        if target.deleted {
            unindex(&tx, rowid)?;
        } else {
            let emb = self.embed_one(&target.content)?;
            index(&tx, rowid, &target.content, &emb)?;
        }
        tx.execute(
            "UPDATE memory_journal SET undone = 1 WHERE id = ?1",
            [journal_id],
        )?;
        tx.commit()?;
        Ok(target)
    }
}

fn validate_text(content: &str, quote: Option<&str>) -> Result<()> {
    if content.trim().is_empty() {
        return Err(MemoryError::Invalid("empty content".into()));
    }
    if contains_secret(content) || quote.is_some_and(contains_secret) {
        return Err(MemoryError::Invalid("content looks like a secret".into()));
    }
    Ok(())
}

pub(crate) fn get_in(conn: &Connection, id: Uuid) -> rusqlite::Result<Option<(i64, Memory)>> {
    conn.query_row(
        &format!("SELECT {COLS} FROM memories WHERE id = ?1"),
        [id.to_string()],
        row_to_memory,
    )
    .optional()
}

fn live_owned(conn: &Connection, user_id: UserId, id: Uuid) -> Result<(i64, Memory)> {
    get_in(conn, id)?
        .filter(|(_, m)| m.user_id == user_id && !m.deleted)
        .ok_or_else(|| MemoryError::NotFound(format!("memory {id}")))
}

/// Overwrite all mutable columns from a snapshot.
fn write_snapshot(conn: &Connection, m: &Memory) -> Result<()> {
    conn.execute(
        "UPDATE memories SET bot_id=?2, kind=?3, content=?4, source_quote=?5, source_chat_id=?6, \
         source_message_id=?7, trust=?8, importance=?9, last_used_ms=?10, valid_until_ms=?11, \
         deleted=?12 WHERE id=?1",
        params![
            m.id.to_string(),
            m.bot_id.map(|b| b.0.to_string()),
            m.kind.as_str(),
            m.content,
            m.source_quote,
            m.source_chat_id.map(|c| c.0.to_string()),
            m.source_message_id.map(|u| u.to_string()),
            m.trust.as_str(),
            f64::from(m.importance),
            m.last_used_ms,
            m.valid_until_ms,
            i64::from(m.deleted),
        ],
    )?;
    Ok(())
}

/// (Re)index a memory in FTS and the vector table.
fn index(conn: &Connection, rowid: i64, content: &str, emb: &[f32]) -> Result<()> {
    unindex(conn, rowid)?;
    conn.execute(
        "INSERT INTO memories_fts(rowid, content) VALUES (?1, ?2)",
        params![rowid, content],
    )?;
    conn.execute(
        "INSERT INTO memories_vec(rowid, embedding) VALUES (?1, ?2)",
        params![rowid, to_blob(emb)],
    )?;
    Ok(())
}

fn unindex(conn: &Connection, rowid: i64) -> Result<()> {
    conn.execute("DELETE FROM memories_fts WHERE rowid = ?1", [rowid])?;
    conn.execute("DELETE FROM memories_vec WHERE rowid = ?1", [rowid])?;
    Ok(())
}

fn journal_insert(
    conn: &Connection,
    m: &Memory,
    op: JournalOp,
    before: Option<&Memory>,
    after: Option<&Memory>,
    reason: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO memory_journal (memory_id, user_id, op, before_json, after_json, reason, ts_ms) \
         VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![
            m.id.to_string(),
            m.user_id.0.to_string(),
            op.as_str(),
            before.map(serde_json::to_string).transpose()?,
            after.map(serde_json::to_string).transpose()?,
            reason,
            now_ms(),
        ],
    )?;
    Ok(())
}

/// Returns (owner, entry).
pub(crate) fn journal_get(conn: &Connection, id: i64) -> Result<Option<(UserId, JournalEntry)>> {
    let row = conn
        .query_row(
            "SELECT id, memory_id, user_id, op, before_json, after_json, reason, ts_ms, undone \
             FROM memory_journal WHERE id = ?1",
            [id],
            journal_row,
        )
        .optional()?;
    row.map(journal_decode).transpose()
}

type RawJournal = (
    i64,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    i64,
    i64,
);

pub(crate) fn journal_row(r: &Row<'_>) -> rusqlite::Result<RawJournal> {
    Ok((
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
        r.get(6)?,
        r.get(7)?,
        r.get(8)?,
    ))
}

pub(crate) fn journal_decode(r: RawJournal) -> Result<(UserId, JournalEntry)> {
    let (id, memory_id, user_id, op, before, after, reason, ts_ms, undone) = r;
    let bad = |what: &str| MemoryError::Invalid(format!("corrupt journal {what}"));
    Ok((
        UserId(Uuid::parse_str(&user_id).map_err(|_| bad("user"))?),
        JournalEntry {
            id,
            memory_id: Uuid::parse_str(&memory_id).map_err(|_| bad("memory id"))?,
            op: JournalOp::parse(&op).ok_or_else(|| bad("op"))?,
            before: before.map(|s| serde_json::from_str(&s)).transpose()?,
            after: after.map(|s| serde_json::from_str(&s)).transpose()?,
            reason,
            ts_ms,
            undone: undone != 0,
        },
    ))
}

fn migrate(conn: &mut Connection) -> Result<()> {
    let current: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let latest = u32::try_from(MIGRATIONS.len()).unwrap_or(u32::MAX);
    if current > latest {
        return Err(MemoryError::SchemaTooNew {
            found: current,
            supported: latest,
        });
    }
    for (i, (_name, sql)) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.execute_batch(&format!("PRAGMA user_version = {}", i + 1))?;
        tx.commit()?;
    }
    Ok(())
}
