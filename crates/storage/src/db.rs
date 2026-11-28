//! The [`Storage`] repository.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use protocol::{BotId, ChatId, RunId, UserId};
use rusqlite::types::Type;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde_json::Value;
use uuid::Uuid;

use crate::error::{Result, StorageError};
use crate::migrate;
use crate::records::{
    Approval, AuditEntry, Bot, BotPatch, BotSkillAssignment, BotWorkspaceRecord, Chat, ChatFlags,
    EventIndexRow, Message, Run, RunStep, SkillAssignmentMode, User, WorkspaceSettingsRecord,
};

/// How many pre-upgrade database copies are kept under `<data_dir>/backups`.
const KEPT_UPGRADE_BACKUPS: usize = 3;

/// Before an upgrade changes the schema of an existing database, write a
/// consistent copy (`VACUUM INTO`, safe with WAL) to
/// `<data_dir>/backups/bot-v<old>-<unix ms>.db` and keep the newest few, so a
/// failed or unwanted upgrade can be rolled back by hand. A fresh or
/// already-current database is not copied. A newer-than-supported database is
/// left for the migrator to reject.
fn backup_before_migration(conn: &Connection, data_dir: &Path) -> Result<()> {
    let current = migrate::current_version(conn)?;
    if current == 0 || current >= migrate::latest_version() {
        return Ok(());
    }
    let dir = data_dir.join("backups");
    std::fs::create_dir_all(&dir)?;
    let target = dir.join(format!("bot-v{current}-{}.db", now_ms()));
    conn.execute("VACUUM INTO ?1", [target.to_string_lossy().as_ref()])?;
    let mut copies: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("bot-v"))
        })
        .collect();
    copies.sort_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
    while copies.len() > KEPT_UPGRADE_BACKUPS {
        let _ = std::fs::remove_file(copies.remove(0));
    }
    Ok(())
}

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

// Dotless-i folding supplements unicode61. Map snippets back to the original
// spelling using character offsets (the supplemental mappings are 1:1).
fn restore_search_snippet(content: &str, snippet: &str) -> String {
    let core = snippet.trim_matches('…');
    let folded = content.replace('ı', "i").replace('İ', "I");
    let Some(start) = folded.find(core) else {
        return snippet.to_owned();
    };
    let skip = folded[..start].chars().count();
    let count = core.chars().count();
    let text: String = content.chars().skip(skip).take(count).collect();
    format!(
        "{}{}{}",
        if snippet.starts_with('…') {
            "…"
        } else {
            ""
        },
        text,
        if snippet.ends_with('…') { "…" } else { "" }
    )
}

/// Optional narrowing for [`Storage::search_messages`]. Every field is applied in SQL
/// before pagination, so pages never come back short because of a later filter.
#[derive(Debug, Clone, Copy, Default)]
pub struct SearchFilter {
    /// Only chats of this bot.
    pub bot: Option<BotId>,
    /// Only this chat.
    pub chat: Option<ChatId>,
    /// Only messages at or after this time (ms since epoch).
    pub from_ms: Option<i64>,
    /// Only messages before this time (ms since epoch, exclusive).
    pub to_ms: Option<i64>,
    /// Only messages that carry an attachment.
    pub has_file: bool,
    /// Only messages that contain an http(s) link.
    pub has_link: bool,
}

/// Synchronous SQLite repository.  Cheap to share behind an `Arc`; calls
/// serialise on an internal mutex.
#[derive(Debug)]
pub struct Storage {
    conn: Mutex<Connection>,
    data_dir: PathBuf,
    /// Bots being deleted: no new chat, run or webhook is inserted for them
    /// ([`Storage::retire_bot`]).
    retired: Mutex<std::collections::HashSet<BotId>>,
}

impl Storage {
    /// Validate and migrate an existing staged database without creating it.
    /// Checks the columns used by this repository and foreign-key consistency;
    /// a schema version and SQLite page integrity alone do not establish either.
    /// Normal [`Self::open`] behavior is unchanged.
    ///
    /// # Errors
    /// Fails on corruption, an incompatible schema, or migration errors.
    pub fn validate_and_migrate(path: impl AsRef<Path>) -> Result<u32> {
        let mut conn =
            Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let integrity: String = conn.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
        if integrity != "ok" {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
                Some(integrity),
            )
            .into());
        }
        migrate::migrate(&mut conn)?;
        for select in [
            "SELECT id, name, created_at_ms FROM users",
            "SELECT id, user_id, name, config_json, created_at_ms, version FROM bots",
            "SELECT id, user_id, bot_id, title, created_at_ms FROM chats",
            "SELECT id, user_id, chat_id, run_id, role, content, created_at_ms FROM messages",
            RUN_SELECT,
            STEP_SELECT,
            APPROVAL_SELECT,
            "SELECT id, user_id, run_id, actor, action, detail_json, ts_ms FROM audit_log",
            "SELECT gseq, run_id, seq FROM event_index",
            "SELECT bot_id, mode, names_json, updated_at_ms, version FROM bot_skill_assignment",
            "SELECT id, generation FROM bot_skill_assignment_state",
        ] {
            conn.prepare(select)?;
        }
        {
            let mut st = conn.prepare(
                "SELECT bot_id, mode, names_json, updated_at_ms, version FROM bot_skill_assignment",
            )?;
            let mut rows = st.query([])?;
            while let Some(row) = rows.next()? {
                let _ = skill_assignment_row(row)?;
            }
        }
        let version_column_valid: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('bots') WHERE name='version' AND upper(type)='INTEGER' AND \"notnull\"=1)",
            [], |r| r.get(0),
        )?;
        let malformed_version: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM bots WHERE typeof(version) != 'integer' OR version < 1)",
            [],
            |r| r.get(0),
        )?;
        if !version_column_valid || malformed_version {
            return Err(StorageError::Conflict(
                "invalid bots.version: required positive INTEGER NOT NULL".into(),
            ));
        }
        if conn.prepare("PRAGMA foreign_key_check")?.exists([])? {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY),
                Some("database contains invalid foreign-key references".into()),
            )
            .into());
        }
        migrate::current_version(&conn)
    }

    /// Open (creating if needed) `<data_dir>/bot.db`, enable WAL + foreign
    /// keys and run pending migrations.
    ///
    /// # Errors
    /// Fails on filesystem, SQLite or migration errors.
    pub fn open(data_dir: impl Into<PathBuf>) -> Result<Self> {
        let data_dir = data_dir.into();
        std::fs::create_dir_all(&data_dir)?;
        // Resolve host-selected relative paths and symlinked home directories
        // once, before any knowledge capability treats this root as trusted.
        let data_dir = crate::paths::without_verbatim_prefix(std::fs::canonicalize(data_dir)?);
        let conn = Connection::open(data_dir.join("bot.db"))?;
        backup_before_migration(&conn, &data_dir)?;
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
            retired: Mutex::default(),
        })
    }

    /// Data directory (event logs live under `runs/`).
    #[must_use]
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// The open database. Other modules in this crate use it; callers outside do not.
    pub(crate) fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn retired_bots(&self) -> std::sync::MutexGuard<'_, std::collections::HashSet<BotId>> {
        self.retired
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// A bot is being deleted: from now on no chat, run or webhook is inserted for it
    /// ([`StorageError::Conflict`]). Returns `false` when it already was. Undone by
    /// [`Storage::restore_bot`] when the delete does not happen; a deleted bot stays retired.
    pub fn retire_bot(&self, bot: BotId) -> bool {
        self.retired_bots().insert(bot)
    }

    /// The delete of a retired bot did not happen: it may start work again.
    pub fn restore_bot(&self, bot: BotId) {
        self.retired_bots().remove(&bot);
    }

    /// Whether the bot is being (or was) deleted by this process.
    #[must_use]
    pub fn is_retired(&self, bot: BotId) -> bool {
        self.retired_bots().contains(&bot)
    }

    fn refuse_retired(&self, bot: BotId) -> Result<()> {
        if self.is_retired(bot) {
            return Err(StorageError::Conflict("the bot is being deleted".into()));
        }
        Ok(())
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

    /// Replace a profile only when its revision still matches the caller's snapshot.
    ///
    /// # Errors
    /// Fails on SQLite/JSON errors.
    pub fn update_bot(
        &self,
        id: BotId,
        name: &str,
        config: &Value,
        expected_version: i64,
    ) -> Result<BotPatch> {
        self.patch_bot(id, expected_version, |_, _| {
            Ok((name.to_owned(), config.clone()))
        })
    }

    /// Read a profile and revision in one SQLite statement.
    ///
    /// # Errors
    /// Fails on SQLite/JSON errors.
    pub fn get_bot_versioned(&self, id: BotId) -> Result<Option<(Bot, i64)>> {
        Ok(self.conn().query_row(
            "SELECT id, user_id, name, config_json, created_at_ms, version FROM bots WHERE id = ?1",
            [id.to_string()], |r| Ok((bot_row(r)?, r.get(5)?)),
        ).optional()?)
    }

    /// Read a user's profiles and revisions in one SQLite statement.
    ///
    /// # Errors
    /// Fails on SQLite/JSON errors.
    pub fn list_bots_versioned(&self, user_id: UserId) -> Result<Vec<(Bot, i64)>> {
        let conn = self.conn();
        let mut st = conn.prepare("SELECT id, user_id, name, config_json, created_at_ms, version FROM bots WHERE user_id = ?1 ORDER BY id")?;
        Ok(st
            .query_map([user_id.to_string()], |r| Ok((bot_row(r)?, r.get(5)?)))?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// The profile version of a bot (bumped by every write), `None` if it does not exist.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn bot_version(&self, id: BotId) -> Result<Option<i64>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT version FROM bots WHERE id = ?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// The profile versions of all of a user's bots.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn bot_versions(&self, user_id: UserId) -> Result<std::collections::HashMap<BotId, i64>> {
        let conn = self.conn();
        let mut st = conn.prepare("SELECT id, version FROM bots WHERE user_id = ?1")?;
        let rows = st.query_map([user_id.to_string()], |r| {
            Ok((BotId(uuid_at(r, 0)?), r.get::<_, i64>(1)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Change a bot's name and profile atomically if its version is still
    /// `expected_version`.  `apply` receives the stored name and profile and
    /// returns the new ones (or a refusal); it runs inside the transaction, so
    /// no other write can land between the read and the write.  The version is
    /// bumped on success.
    ///
    /// # Errors
    /// Fails on SQLite/JSON errors.
    pub fn patch_bot<F>(&self, id: BotId, expected_version: i64, apply: F) -> Result<BotPatch>
    where
        F: FnOnce(&str, &Value) -> std::result::Result<(String, Value), String>,
    {
        self.write_bot(id, Some(expected_version), apply)
    }

    /// Atomically derive the next name/profile from the stored one and write it,
    /// advancing the bot revision, WITHOUT a version precondition. For writers
    /// that own one field (the dedicated knowledge settings) and must not
    /// overwrite the rest of a profile with a stale full copy.
    ///
    /// # Errors
    /// Fails on SQLite/JSON errors.
    pub fn modify_bot<F>(&self, id: BotId, apply: F) -> Result<BotPatch>
    where
        F: FnOnce(&str, &Value) -> std::result::Result<(String, Value), String>,
    {
        self.write_bot(id, None, apply)
    }

    fn write_bot<F>(&self, id: BotId, expected_version: Option<i64>, apply: F) -> Result<BotPatch>
    where
        F: FnOnce(&str, &Value) -> std::result::Result<(String, Value), String>,
    {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let stored = tx
            .query_row(
                "SELECT id, user_id, name, config_json, created_at_ms, version FROM bots WHERE id = ?1",
                [id.to_string()],
                |r| Ok((bot_row(r)?, r.get::<_, i64>(5)?)),
            )
            .optional()?;
        let Some((bot, version)) = stored else {
            return Ok(BotPatch::Missing);
        };
        if expected_version.is_some_and(|expected| expected != version) {
            return Ok(BotPatch::Stale { bot, version });
        }
        let (name, config) = match apply(&bot.name, &bot.config) {
            Ok(next) => next,
            Err(why) => return Ok(BotPatch::Rejected(why)),
        };
        tx.execute(
            "UPDATE bots SET name = ?2, config_json = ?3, version = version + 1 WHERE id = ?1",
            params![id.to_string(), name, serde_json::to_string(&config)?],
        )?;
        tx.commit()?;
        Ok(BotPatch::Applied {
            bot: Bot {
                name,
                config,
                ..bot
            },
            version: version + 1,
        })
    }

    /// Delete a bot and its skill assignment row in the same transaction.
    /// Returns `false` if it does not exist.
    ///
    /// # Errors
    /// Fails on SQLite errors, in particular a foreign-key violation while
    /// chats or runs still reference the bot.
    pub fn delete_bot(&self, id: BotId) -> Result<bool> {
        self.delete_bot_with_cleanup(id, || Ok(()))
    }

    /// Delete a bot and run bounded local cleanup before committing its removal.
    /// Foreign-key checks run first. A cleanup error keeps the owning row, and
    /// concurrent chat/run insertion cannot race the transaction. The closure
    /// must not reenter this storage connection or wait for asynchronous work.
    ///
    /// # Errors
    /// SQLite constraints, storage failure, or the supplied cleanup error.
    pub fn delete_bot_with_cleanup(
        &self,
        id: BotId,
        cleanup: impl FnOnce() -> Result<()>,
    ) -> Result<bool> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let bid = id.to_string();
        tx.execute("DELETE FROM bot_skill_assignment WHERE bot_id = ?1", [&bid])?;
        tx.execute("DELETE FROM bot_spend_limits WHERE bot_id = ?1", [&bid])?;
        // Workspace mappings also reserve folder names after deletion. The
        // files stay on disk until the owner chooses to remove them.
        let n = tx.execute("DELETE FROM bots WHERE id = ?1", [&bid])?;
        if n > 0 {
            cleanup()?;
        }
        tx.commit()?;
        Ok(n > 0)
    }

    // ---- bot_skill_assignment ----

    /// Fetch a bot's skill assignment row, or `None` when the bot uses the
    /// default (`all` shared and bundled skills).
    ///
    /// # Errors
    /// Fails on SQLite or malformed row errors.
    pub fn get_bot_skill_assignment(&self, bot_id: BotId) -> Result<Option<BotSkillAssignment>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT bot_id, mode, names_json, updated_at_ms, version FROM bot_skill_assignment WHERE bot_id = ?1",
                [bot_id.to_string()],
                skill_assignment_row,
            )
            .optional()?)
    }

    /// Inspect a bot's skill assignment row without failing on malformed row
    /// contents, so management surfaces can report and repair corrupt rows
    /// while runtime callers continue to fail closed.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn inspect_bot_skill_assignment(&self, bot_id: BotId) -> Result<SkillAssignmentLookup> {
        let conn = self.conn();
        let bid = bot_id.to_string();
        let res = conn
            .query_row(
                "SELECT bot_id, mode, names_json, updated_at_ms, version FROM bot_skill_assignment WHERE bot_id = ?1",
                [&bid],
                skill_assignment_row,
            )
            .optional();
        match res {
            Ok(row) => Ok(SkillAssignmentLookup::Ok(row)),
            Err(rusqlite::Error::FromSqlConversionFailure(_, _, err)) => {
                let raw_ver: Option<i64> = conn
                    .query_row(
                        "SELECT version FROM bot_skill_assignment WHERE bot_id = ?1",
                        [&bid],
                        |r| r.get(0),
                    )
                    .optional()?
                    .flatten();
                let version = raw_ver.and_then(|v| u64::try_from(v).ok()).unwrap_or(0);
                Ok(SkillAssignmentLookup::Corrupt {
                    version,
                    reason: err.to_string(),
                })
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Current DB-backed generation counter incremented on every insert, update,
    /// or delete on `bot_skill_assignment`.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn bot_skill_assignment_generation(&self) -> Result<u64> {
        let gen_val: i64 = self.conn().query_row(
            "SELECT generation FROM bot_skill_assignment_state WHERE id = 1",
            [],
            |r| r.get(0),
        )?;
        Ok(u64::try_from(gen_val).unwrap_or(0))
    }

    /// Set a bot's skill assignment. `SkillAssignmentMode::All` deletes any
    /// existing row (restoring the default).
    ///
    /// # Errors
    /// Fails on invalid skill names, too many names (> 200), SQLite/JSON
    /// errors, or a missing bot foreign key.
    pub fn set_bot_skill_assignment(
        &self,
        bot_id: BotId,
        mode: SkillAssignmentMode,
        names: &[String],
    ) -> Result<BotSkillAssignment> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let row = write_skill_assignment_tx(&tx, bot_id, mode, names, None)?;
        tx.commit()?;
        Ok(row)
    }

    /// Write a `bot.skills_assigned` audit entry and update the bot's skill
    /// assignment in a single transaction, ensuring the audit record is
    /// persisted before the change takes effect.
    ///
    /// # Errors
    /// Fails on invalid skill names, too many names (> 200), or SQLite/JSON
    /// errors.
    pub fn set_bot_skill_assignment_audited(
        &self,
        user_id: UserId,
        bot_id: BotId,
        mode: SkillAssignmentMode,
        names: &[String],
        actor: &str,
        detail: &Value,
    ) -> Result<BotSkillAssignment> {
        self.set_bot_skill_assignment_audited_versioned(
            user_id, bot_id, mode, names, actor, detail, None,
        )
    }

    /// Version-checked variant of [`Self::set_bot_skill_assignment_audited`].
    /// The audit diff is derived from the stored assignment inside the transaction;
    /// caller-supplied `bot_id`, `mode`, `added` and `removed` are replaced.
    ///
    /// # Errors
    /// Fails with [`StorageError::Conflict`] if `expected_version` is `Some`
    /// and does not match the stored row's version (`0` when no row exists).
    #[allow(clippy::too_many_arguments)]
    pub fn set_bot_skill_assignment_audited_versioned(
        &self,
        user_id: UserId,
        bot_id: BotId,
        mode: SkillAssignmentMode,
        names: &[String],
        actor: &str,
        detail: &Value,
        expected_version: Option<u64>,
    ) -> Result<BotSkillAssignment> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let previous = tx.query_row(
            "SELECT bot_id, mode, names_json, updated_at_ms, version FROM bot_skill_assignment WHERE bot_id = ?1",
            [bot_id.to_string()],
            skill_assignment_row,
        ).optional();
        let previous_names: BTreeSet<String> = match previous {
            Ok(Some(row)) if row.mode == SkillAssignmentMode::Selected => {
                row.names.into_iter().collect()
            }
            Ok(_) | Err(rusqlite::Error::FromSqlConversionFailure(..)) => BTreeSet::default(),
            Err(error) => return Err(error.into()),
        };
        let row = write_skill_assignment_tx(&tx, bot_id, mode, names, expected_version)?;
        let next_names: BTreeSet<String> = row.names.iter().cloned().collect();
        let added: Vec<_> = next_names.difference(&previous_names).collect();
        let removed: Vec<_> = previous_names.difference(&next_names).collect();
        let mut detail = detail.as_object().cloned().unwrap_or_default();
        detail.insert("bot_id".into(), serde_json::json!(bot_id.to_string()));
        detail.insert(
            "mode".into(),
            serde_json::json!(match mode {
                SkillAssignmentMode::All => "all",
                SkillAssignmentMode::Selected => "selected",
            }),
        );
        detail.insert("added".into(), serde_json::json!(added));
        detail.insert("removed".into(), serde_json::json!(removed));
        tx.execute(
            "INSERT INTO audit_log (user_id, run_id, actor, action, detail_json, ts_ms) VALUES (?1, NULL, ?2, ?3, ?4, ?5)",
            params![
                user_id.to_string(),
                actor,
                "bot.skills_assigned",
                serde_json::to_string(&detail)?,
                now_ms()
            ],
        )?;
        tx.commit()?;
        Ok(row)
    }

    /// Delete a bot's skill assignment row, returning `true` if one existed.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn delete_bot_skill_assignment(&self, bot_id: BotId) -> Result<bool> {
        let n = self.conn().execute(
            "DELETE FROM bot_skill_assignment WHERE bot_id = ?1",
            [bot_id.to_string()],
        )?;
        Ok(n > 0)
    }

    // ---- chats ----

    /// Insert a chat.
    ///
    /// # Errors
    /// Fails on SQLite errors; [`StorageError::Conflict`] while the bot is being deleted.
    pub fn insert_chat(&self, c: &Chat) -> Result<()> {
        self.refuse_retired(c.bot_id)?;
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

    /// Pinned/archived flags of every chat of a user, keyed by chat id.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn list_chat_flags(
        &self,
        user_id: UserId,
    ) -> Result<std::collections::HashMap<ChatId, ChatFlags>> {
        let conn = self.conn();
        let mut st = conn.prepare("SELECT id, pinned, archived FROM chats WHERE user_id = ?1")?;
        let rows = st.query_map([user_id.to_string()], |r| {
            Ok((
                ChatId(uuid_at(r, 0)?),
                ChatFlags {
                    pinned: r.get::<_, i64>(1)? != 0,
                    archived: r.get::<_, i64>(2)? != 0,
                },
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Flags of one chat (all false for an unknown chat).
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn chat_flags(&self, id: ChatId) -> Result<ChatFlags> {
        Ok(self
            .conn()
            .query_row(
                "SELECT pinned, archived FROM chats WHERE id = ?1",
                [id.to_string()],
                |r| {
                    Ok(ChatFlags {
                        pinned: r.get::<_, i64>(0)? != 0,
                        archived: r.get::<_, i64>(1)? != 0,
                    })
                },
            )
            .optional()?
            .unwrap_or_default())
    }

    /// Rename and/or flag a chat; `None` leaves a field unchanged.
    ///
    /// # Errors
    /// Fails on SQLite errors or when the chat does not exist.
    pub fn update_chat(
        &self,
        id: ChatId,
        title: Option<&str>,
        pinned: Option<bool>,
        archived: Option<bool>,
    ) -> Result<()> {
        let n = self.conn().execute(
            "UPDATE chats SET title = COALESCE(?2, title), pinned = COALESCE(?3, pinned), archived = COALESCE(?4, archived) WHERE id = ?1",
            params![id.to_string(), title, pinned.map(i64::from), archived.map(i64::from)],
        )?;
        if n == 0 {
            return Err(StorageError::NotFound(format!("chat {id}")));
        }
        Ok(())
    }

    /// Delete a chat and its messages. Its finished runs stay (audit trail) but lose the chat
    /// link. Refused with [`StorageError::Conflict`] while a run of the chat is still active.
    ///
    /// # Errors
    /// Fails on SQLite errors or the conflict above.
    pub fn delete_chat(&self, id: ChatId) -> Result<bool> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let cid = id.to_string();
        let active: i64 = tx.query_row(
            "SELECT COUNT(*) FROM runs WHERE chat_id = ?1 AND state NOT IN ('succeeded', 'failed', 'cancelled')",
            [&cid],
            |r| r.get(0),
        )?;
        if active > 0 {
            return Err(StorageError::Conflict("chat has an active run".into()));
        }
        tx.execute("DELETE FROM messages WHERE chat_id = ?1", [&cid])?;
        tx.execute("UPDATE runs SET chat_id = NULL WHERE chat_id = ?1", [&cid])?;
        let n = tx.execute("DELETE FROM chats WHERE id = ?1", [&cid])?;
        tx.commit()?;
        Ok(n > 0)
    }

    // ---- messages ----

    /// Insert a message.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn insert_message(&self, m: &Message) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO messages (id, user_id, chat_id, run_id, role, content, created_at_ms, exposure_untrusted, exposure_private, exposure_sources) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                m.id.to_string(),
                m.user_id.to_string(),
                m.chat_id.to_string(),
                m.run_id.map(|r| r.to_string()),
                m.role,
                m.content,
                m.created_at_ms,
                i64::from(m.exposure_untrusted),
                i64::from(m.exposure_private),
                serde_json::to_string(&m.exposure_sources)?,
            ],
        )?;
        // A row that sorts at or before a summary's cursor (an import, a backfill) would sit
        // inside the part the summary stands in for and never be shown: drop the summary, as
        // an edit does. A normal append sorts after the cursor and keeps it.
        tx.execute(
            "DELETE FROM chat_summaries WHERE chat_id = ?1 \
             AND (?2 < covered_until_ms OR (?2 = covered_until_ms AND ?3 <= covered_until_id))",
            params![m.chat_id.to_string(), m.created_at_ms, m.id.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Insert a chat and its messages, each with its recorded origin, atomically.
    ///
    /// # Errors
    /// Fails on SQLite errors, including a duplicate id or an unknown bot/chat/run.
    pub fn insert_chat_with_messages(
        &self,
        chat: &Chat,
        messages: &[(Message, &str)],
    ) -> Result<()> {
        self.refuse_retired(chat.bot_id)?;
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO chats (id, user_id, bot_id, title, created_at_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                chat.id.to_string(),
                chat.user_id.to_string(),
                chat.bot_id.to_string(),
                chat.title,
                chat.created_at_ms,
            ],
        )?;
        for (message, source) in messages {
            tx.execute(
                "INSERT INTO messages (id, user_id, chat_id, run_id, role, content, created_at_ms, exposure_untrusted, exposure_private, exposure_sources, source) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    message.id.to_string(),
                    message.user_id.to_string(),
                    message.chat_id.to_string(),
                    message.run_id.map(|r| r.to_string()),
                    message.role,
                    message.content,
                    message.created_at_ms,
                    i64::from(message.exposure_untrusted),
                    i64::from(message.exposure_private),
                    serde_json::to_string(&message.exposure_sources)?,
                    source,
                ],
            )?;
        }
        tx.commit()?;
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
                "SELECT id, user_id, chat_id, run_id, role, content, created_at_ms, exposure_untrusted, exposure_private, exposure_sources FROM messages WHERE id = ?1",
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
            "SELECT id, user_id, chat_id, run_id, role, content, created_at_ms, exposure_untrusted, exposure_private, exposure_sources FROM messages WHERE chat_id = ?1 AND superseded = 0 ORDER BY created_at_ms, id",
        )?;
        Ok(st
            .query_map([chat_id.to_string()], message_row)?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Ids of a chat's superseded messages (replaced by an edit or regenerated).
    /// They are kept as audit data; [`Self::list_messages`] and the history
    /// reads leave them out.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn superseded_message_ids(
        &self,
        chat_id: ChatId,
    ) -> Result<std::collections::HashSet<Uuid>> {
        let conn = self.conn();
        let mut st =
            conn.prepare("SELECT id FROM messages WHERE chat_id = ?1 AND superseded = 1")?;
        let ids = st
            .query_map([chat_id.to_string()], |r| uuid_at(r, 0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(ids)
    }

    /// A chat's messages including superseded ones, oldest first (for the UI).
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn list_messages_with_superseded(&self, chat_id: ChatId) -> Result<Vec<Message>> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT id, user_id, chat_id, run_id, role, content, created_at_ms, exposure_untrusted, exposure_private, exposure_sources FROM messages WHERE chat_id = ?1 ORDER BY created_at_ms, id",
        )?;
        Ok(st
            .query_map([chat_id.to_string()], message_row)?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Mark messages superseded (`true`) or restore them (`false`). Never deletes.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn set_messages_superseded(&self, ids: &[Uuid], superseded: bool) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        for id in ids {
            // Any edit, regenerate or restore in a chat drops its summary, wherever the message
            // sits: no cursor comparison can then misjudge which side of the summary it is on.
            // A new summary is written when the chat grows again.
            tx.execute(
                "DELETE FROM chat_summaries WHERE chat_id = (SELECT chat_id FROM messages WHERE id = ?1)",
                [id.to_string()],
            )?;
            tx.execute(
                "UPDATE messages SET superseded = ?2 WHERE id = ?1",
                params![id.to_string(), i64::from(superseded)],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Ranked phrase search, scoped before pagination. Queries are literal FTS phrases,
    /// never FTS syntax. Tool, private, superseded and oversized messages are excluded.
    /// Returns at most 100 bounded messages and plain snippets (no HTML).
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn search_messages(
        &self,
        user: UserId,
        filter: SearchFilter,
        query: &str,
        offset: u32,
        limit: u32,
    ) -> Result<Vec<(Message, String)>> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT m.id, m.user_id, m.chat_id, m.run_id, m.role, m.content, m.created_at_ms,
             m.exposure_untrusted, m.exposure_private, m.exposure_sources,
             snippet(chat_messages_fts, 0, '', '', '…', 24)
             FROM chat_messages_fts JOIN messages m ON m.rowid = chat_messages_fts.rowid
             JOIN chats c ON c.id = m.chat_id
             WHERE chat_messages_fts MATCH ?1 AND m.user_id = ?2 AND c.user_id = ?2
             AND (?3 IS NULL OR c.bot_id = ?3) AND (?4 IS NULL OR c.id = ?4)
             AND (?7 IS NULL OR m.created_at_ms >= ?7) AND (?8 IS NULL OR m.created_at_ms < ?8)
             AND (?9 = 0 OR instr(m.content, '[[praxi-attachments:') > 0)
             AND (?10 = 0 OR instr(m.content, 'http://') > 0 OR instr(m.content, 'https://') > 0)
             ORDER BY rank, m.created_at_ms DESC, m.id LIMIT ?5 OFFSET ?6",
        )?;
        let phrase = format!(
            "\"{}\"",
            query
                .replace('ı', "i")
                .replace('İ', "I")
                .replace('"', "\"\"")
        );
        Ok(st
            .query_map(
                params![
                    phrase,
                    user.to_string(),
                    filter.bot.map(|v| v.to_string()),
                    filter.chat.map(|v| v.to_string()),
                    limit.min(100),
                    offset.min(10_000),
                    filter.from_ms,
                    filter.to_ms,
                    i64::from(filter.has_file),
                    i64::from(filter.has_link)
                ],
                |r| {
                    let message = message_row(r)?;
                    let snippet =
                        restore_search_snippet(&message.content, &r.get::<_, String>(10)?);
                    Ok((message, snippet))
                },
            )?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Bounded metadata for a chat's tool steps; never returns inputs, outputs or errors.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn chat_step_summaries(
        &self,
        user: UserId,
        chat: ChatId,
    ) -> Result<Vec<(RunId, String, String, i64)>> {
        let conn = self.conn();
        let mut st = conn.prepare("SELECT s.run_id, CASE WHEN length(json_extract(call.value,'$.name')) > 120 THEN '[REDACTED]' ELSE json_extract(call.value,'$.name') END,
            CASE WHEN s.status IN ('pending','running','succeeded','failed','cancelled','skipped') THEN s.status ELSE 'unknown' END, s.started_at_ms
            FROM run_steps s JOIN runs r ON r.id = s.run_id JOIN json_each(s.input_json,'$.calls') call
            WHERE s.name = 'tools' AND json_type(call.value,'$.name') = 'text' AND r.user_id = ?1 AND s.user_id = ?1 AND r.chat_id = ?2
            ORDER BY s.started_at_ms, s.id LIMIT 2001")?;
        Ok(st
            .query_map(params![user.to_string(), chat.to_string()], |r| {
                Ok((RunId(uuid_at(r, 0)?), r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Bounded approval outcomes; summaries may contain sensitive tool arguments and
    /// are deliberately omitted. No tool arguments or arbitrary decision text is read.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn chat_approval_summaries(
        &self,
        user: UserId,
        chat: ChatId,
    ) -> Result<Vec<(RunId, String, i64)>> {
        let conn = self.conn();
        let mut st = conn.prepare("SELECT a.run_id,
            CASE WHEN a.decision IN ('approved','denied','expired','answered','declined') THEN a.decision
                 WHEN a.decision IS NULL THEN 'pending' ELSE 'decided' END,
            coalesce(a.decided_at_ms,a.requested_at_ms)
            FROM approvals a JOIN runs r ON r.id = a.run_id
            WHERE r.user_id = ?1 AND a.user_id = ?1 AND r.chat_id = ?2
            ORDER BY a.requested_at_ms, a.id LIMIT 2001")?;
        Ok(st
            .query_map(params![user.to_string(), chat.to_string()], |r| {
                Ok((RunId(uuid_at(r, 0)?), r.get(1)?, r.get(2)?))
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Read the newest contiguous suffix of a chat within both limits.
    ///
    /// Returns `(messages, omitted_older_messages)` in chronological order.
    /// `exclude_run` is filtered before applying the limits, so messages that
    /// the caller reconstructs from that run's steps do not consume the budget.
    /// A message that does not fit stops the scan; its text is never copied
    /// into Rust, and older rows are not substituted for it. Text is never
    /// truncated, including at UTF-8 boundaries. At most `max_messages + 1`
    /// rows are considered and returned content totals at most `max_bytes`.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn list_messages_bounded(
        &self,
        chat_id: ChatId,
        exclude_run: Option<RunId>,
        max_messages: usize,
        max_bytes: usize,
    ) -> Result<(Vec<Message>, bool)> {
        self.list_messages_bounded_after(chat_id, None, exclude_run, max_messages, max_bytes)
    }

    /// [`Self::list_messages_bounded`], limited to the messages that sort after `after` (the
    /// cursor of a chat summary; `None`: the whole chat). Older messages are neither returned
    /// nor counted as omitted: a summary stands in for them.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn list_messages_bounded_after(
        &self,
        chat_id: ChatId,
        after: Option<crate::Cursor>,
        exclude_run: Option<RunId>,
        max_messages: usize,
        max_bytes: usize,
    ) -> Result<(Vec<Message>, bool)> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT id, user_id, chat_id, run_id, role, content, created_at_ms, exposure_untrusted, exposure_private, exposure_sources, length(CAST(content AS BLOB)) \
             FROM messages WHERE chat_id = ?1 AND superseded = 0 AND (?2 IS NULL OR run_id IS NULL OR run_id <> ?2) \
             AND (?4 IS NULL OR created_at_ms > ?4 OR (created_at_ms = ?4 AND id > ?5)) \
             ORDER BY created_at_ms DESC, id DESC LIMIT ?3",
        )?;
        let limit = i64::try_from(max_messages.saturating_add(1)).unwrap_or(i64::MAX);
        let mut rows = st.query(params![
            chat_id.to_string(),
            exclude_run.map(|r| r.to_string()),
            limit,
            after.map(|c| c.0),
            after.map(|c| c.1.to_string()),
        ])?;
        let mut messages = Vec::new();
        let mut used = 0usize;
        let mut omitted = false;
        while let Some(row) = rows.next()? {
            let bytes = usize::try_from(row.get::<_, i64>(10)?).map_err(|e| conv_err(10, e))?;
            if messages.len() == max_messages || bytes > max_bytes.saturating_sub(used) {
                omitted = true;
                break;
            }
            messages.push(message_row(row)?);
            used += bytes;
        }
        messages.reverse();
        Ok((messages, omitted))
    }

    /// Exposure bits and source labels of every message of `chat` created at or
    /// before `until_ms`, other than those delivered into `exclude_run` and
    /// those superseded by an edit or regenerate (they are not in the context).
    /// Unbounded: unlike the model-context window, nothing scrolls out.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn chat_exposure_until(
        &self,
        chat_id: ChatId,
        exclude_run: RunId,
        until_ms: i64,
    ) -> Result<(bool, bool, Vec<String>)> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT exposure_untrusted, exposure_private, exposure_sources FROM messages WHERE chat_id = ?1 AND superseded = 0 AND created_at_ms <= ?3 AND (run_id IS NULL OR run_id <> ?2)",
        )?;
        let mut rows = st.query(params![
            chat_id.to_string(),
            exclude_run.to_string(),
            until_ms
        ])?;
        let (mut untrusted, mut private) = (false, false);
        let mut sources: Vec<String> = Vec::new();
        while let Some(r) = rows.next()? {
            untrusted |= r.get::<_, i64>(0)? != 0;
            private |= r.get::<_, i64>(1)? != 0;
            let labels: Vec<String> =
                serde_json::from_str(&r.get::<_, String>(2)?).unwrap_or_default();
            for l in labels {
                if !l.is_empty() && !sources.contains(&l) {
                    sources.push(l);
                }
            }
        }
        Ok((untrusted, private, sources))
    }

    // ---- runs ----

    /// Insert a run.
    ///
    /// # Errors
    /// Fails on SQLite errors; [`StorageError::Conflict`] while the bot is being deleted.
    pub fn insert_run(&self, r: &Run) -> Result<()> {
        self.refuse_retired(r.bot_id)?;
        self.conn().execute(
            "INSERT INTO runs (id, user_id, bot_id, chat_id, kind, state, error, created_at_ms, updated_at_ms, parent_run_id, spec_json, computer_target, pause_reason, computer_node, computer_node_name, paused_at_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                r.id.to_string(),
                r.user_id.to_string(),
                r.bot_id.to_string(),
                r.chat_id.map(|c| c.to_string()),
                r.kind,
                r.state,
                r.error,
                r.created_at_ms,
                r.updated_at_ms,
                r.parent_run_id.map(|p| p.to_string()),
                r.spec.as_ref().map(serde_json::to_string).transpose()?,
                r.computer_target,
                r.pause_reason,
                r.computer_node, r.computer_node_name, r.paused_at_ms,
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

    /// The oldest run of `user_id` whose `kind` is exactly `kind` (the durable
    /// idempotency key of a scheduled occurrence attempt).
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn find_run_by_kind(&self, user_id: UserId, kind: &str) -> Result<Option<Run>> {
        Ok(self
            .conn()
            .query_row(
                &format!("{RUN_SELECT} WHERE user_id = ?1 AND kind = ?2 ORDER BY id LIMIT 1"),
                params![user_id.to_string(), kind],
                run_row,
            )
            .optional()?)
    }

    /// The newest run of `chat` that finished successfully (`succeeded`).
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn latest_succeeded_run_for_chat(
        &self,
        user_id: UserId,
        chat: ChatId,
    ) -> Result<Option<Run>> {
        Ok(self
            .conn()
            .query_row(
                &format!(
                    "{RUN_SELECT} WHERE user_id = ?1 AND chat_id = ?2 AND state = 'succeeded' ORDER BY id DESC LIMIT 1"
                ),
                params![user_id.to_string(), chat.to_string()],
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

    /// Ids and states of the user's runs that are not terminal (`queued`,
    /// `running`, `waiting_approval`, `paused`): one statement, no spec
    /// payloads, so it is cheap on a long history and a consistent snapshot.
    /// The state list is a literal: `daemon-api` tests pin it against
    /// `RunState::ALL` minus the terminal states.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn list_open_run_states(&self, user_id: UserId) -> Result<Vec<(RunId, String)>> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT id, state FROM runs WHERE user_id = ?1 AND state IN ('queued', 'running', 'waiting_approval', 'paused') ORDER BY id",
        )?;
        Ok(st
            .query_map([user_id.to_string()], |r| {
                Ok((RunId(uuid_at(r, 0)?), r.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// The state of one run of `user_id` (`None`: no such run for that user).
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn run_state_of_user(&self, user_id: UserId, id: RunId) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT state FROM runs WHERE id = ?1 AND user_id = ?2",
                [id.to_string(), user_id.to_string()],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// The direct children (subagent runs) of a run, oldest first.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn list_child_runs(&self, parent: RunId) -> Result<Vec<Run>> {
        let conn = self.conn();
        let mut st = conn.prepare(&format!(
            "{RUN_SELECT} WHERE parent_run_id = ?1 ORDER BY id"
        ))?;
        Ok(st
            .query_map([parent.to_string()], run_row)?
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
    /// caller's responsibility. Transitions out of `paused` clear `pause_reason`.
    ///
    /// # Errors
    /// Fails on SQLite errors or if the run does not exist.
    pub fn update_run_state(&self, id: RunId, state: &str, error: Option<&str>) -> Result<()> {
        let n = self.conn().execute(
            "UPDATE runs SET state = ?2, error = ?3, updated_at_ms = ?4, paused_at_ms = CASE WHEN ?2 = 'paused' THEN COALESCE(paused_at_ms, ?4) ELSE NULL END, pause_reason = CASE WHEN ?2 = 'paused' THEN pause_reason ELSE NULL END WHERE id = ?1",
            params![id.to_string(), state, error, now_ms()],
        )?;
        if n == 0 {
            return Err(StorageError::NotFound(format!("run {id}")));
        }
        Ok(())
    }

    /// Record the resolved computer target label (`pc`, `host`, `browser`,
    /// `off`) fixed at run start.
    ///
    /// # Errors
    /// Fails on SQLite errors or if the run does not exist.
    pub fn set_run_computer_target(&self, id: RunId, target: Option<&str>) -> Result<()> {
        let n = self.conn().execute(
            "UPDATE runs SET computer_target = ?2, updated_at_ms = ?3 WHERE id = ?1",
            params![id.to_string(), target, now_ms()],
        )?;
        if n == 0 {
            return Err(StorageError::NotFound(format!("run {id}")));
        }
        Ok(())
    }

    /// Persist label and concrete target together before any tool can run.
    ///
    /// # Errors
    /// Fails on SQLite errors or if the run does not exist.
    pub fn bind_run_computer(
        &self,
        id: RunId,
        label: &str,
        node: Option<&str>,
        name: Option<&str>,
    ) -> Result<()> {
        let n = self.conn().execute(
            "UPDATE runs SET computer_target = ?2, computer_node = ?3, computer_node_name = ?4 WHERE id = ?1 AND (computer_target IS NULL OR computer_target = ?2) AND (computer_node IS NULL OR computer_node = ?3)",
            params![id.to_string(), label, node, name],
        )?;
        if n == 0 {
            return Err(StorageError::NotFound(format!("run {id}")));
        }
        Ok(())
    }

    /// Update the pause reason on a paused run (e.g. `waiting_for_pc`).
    ///
    /// # Errors
    /// Fails on SQLite errors or if the run does not exist.
    pub fn set_run_pause_reason(&self, id: RunId, reason: Option<&str>) -> Result<()> {
        let n = self.conn().execute(
            "UPDATE runs SET pause_reason = ?2, updated_at_ms = ?3 WHERE id = ?1",
            params![id.to_string(), reason, now_ms()],
        )?;
        if n == 0 {
            return Err(StorageError::NotFound(format!("run {id}")));
        }
        Ok(())
    }

    /// List all runs currently in `paused` state with the given `pause_reason`.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn list_paused_runs_with_reason(&self, reason: &str) -> Result<Vec<Run>> {
        let conn = self.conn();
        let mut st = conn.prepare(&format!(
            "{RUN_SELECT} WHERE state = 'paused' AND pause_reason = ?1 ORDER BY id"
        ))?;
        Ok(st
            .query_map([reason], run_row)?
            .collect::<rusqlite::Result<_>>()?)
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

    /// Recent tool executions owned by one user and bot, optionally one run.
    /// Row and aggregate JSON byte limits are checked before copying payloads
    /// into Rust. Oversized rows are omitted; callers must not infer permission
    /// from the absence of a record.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn knowledge_tool_steps(
        &self,
        user_id: UserId,
        bot_id: BotId,
        run_id: Option<RunId>,
        max_rows: usize,
        max_bytes: usize,
    ) -> Result<Vec<RunStep>> {
        let conn = self.conn();
        let select = STEP_SELECT.replacen(" FROM ", ", length(CAST(input_json AS BLOB)) + coalesce(length(CAST(output_json AS BLOB)), 0) + coalesce(length(CAST(retry_json AS BLOB)), 0) + coalesce(length(CAST(error AS BLOB)), 0) + length(CAST(status AS BLOB)) FROM ", 1);
        let mut statement = conn.prepare(&format!(
            "{select} WHERE name='tools' AND user_id=?1 AND run_id IN (SELECT id FROM runs WHERE user_id=?1 AND bot_id=?2) AND (?3 IS NULL OR run_id=?3) ORDER BY started_at_ms DESC,id DESC LIMIT ?4"
        ))?;
        let mut rows = statement.query(params![
            user_id.to_string(),
            bot_id.to_string(),
            run_id.map(|id| id.to_string()),
            i64::try_from(max_rows.min(200)).unwrap_or(200)
        ])?;
        let mut remaining = max_bytes.min(8 * 1024 * 1024);
        let mut found = Vec::new();
        while let Some(row) = rows.next()? {
            let bytes =
                usize::try_from(row.get::<_, i64>(14)?).map_err(|error| conv_err(14, error))?;
            if bytes > remaining {
                continue;
            }
            found.push(step_row(row)?);
            remaining -= bytes;
        }
        Ok(found)
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

    /// Treat the folder (normalised path) as the owner's own content.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn trust_folder(&self, path: &str) -> Result<()> {
        self.conn().execute(
            "INSERT OR IGNORE INTO trusted_folders (path, added_at_ms) VALUES (?1, ?2)",
            params![path, now_ms()],
        )?;
        Ok(())
    }

    /// Stop trusting the folder. Returns whether it was listed.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn untrust_folder(&self, path: &str) -> Result<bool> {
        Ok(self
            .conn()
            .execute("DELETE FROM trusted_folders WHERE path = ?1", params![path])?
            > 0)
    }

    /// The trusted folders, oldest first.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn trusted_folders(&self) -> Result<Vec<String>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT path FROM trusted_folders ORDER BY added_at_ms, path")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Whether `path` (normalised) is a trusted folder or lies inside one.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn is_trusted_path(&self, path: &str) -> Result<bool> {
        let conn = self.conn();
        let mut probe = Some(path);
        while let Some(p) = probe {
            let hit: Option<i64> = conn
                .query_row(
                    "SELECT 1 FROM trusted_folders WHERE path = ?1",
                    params![p],
                    |r| r.get(0),
                )
                .optional()?;
            if hit.is_some() {
                return Ok(true);
            }
            probe = p.rfind('/').filter(|i| *i > 0).map(|i| &p[..i]);
        }
        Ok(false)
    }

    /// Record authenticated message origin before the message can drive a tool call.
    /// # Errors
    /// Fails on SQLite errors.
    pub fn set_message_source(&self, id: Uuid, source: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE messages SET source = ?2 WHERE id = ?1",
            params![id.to_string(), source],
        )?;
        Ok(())
    }

    /// The recorded origin of a message (`owner`, `imported`, `imported_foreign:<name>`, ...).
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn message_source(&self, id: Uuid) -> Result<String> {
        Ok(self
            .conn()
            .query_row(
                "SELECT source FROM messages WHERE id = ?1",
                [id.to_string()],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .unwrap_or_default())
    }

    /// Recorded origins of every user message delivered into `run` (the one
    /// that started it and any steered in later).
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn run_user_message_sources(&self, run: RunId) -> Result<Vec<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT source FROM messages WHERE run_id = ?1 AND role = 'user' ORDER BY created_at_ms",
        )?;
        let rows = stmt.query_map(params![run.to_string()], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Texts of the user messages delivered into `run`, oldest first (the one that started it
    /// first).
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn run_user_message_texts(&self, run: RunId) -> Result<Vec<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT content FROM messages WHERE run_id = ?1 AND role = 'user' AND superseded = 0              ORDER BY created_at_ms, id",
        )?;
        let rows = stmt.query_map(params![run.to_string()], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// The first live user message delivered into `run` with where it came from: its text, its
    /// recorded source and whether the content it was written after was untrusted. `None`
    /// when the run has none.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn run_first_user_message(&self, run: RunId) -> Result<Option<(String, String, bool)>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT content, source, exposure_untrusted FROM messages \
                 WHERE run_id = ?1 AND role = 'user' AND superseded = 0 \
                 ORDER BY created_at_ms, id LIMIT 1",
                params![run.to_string()],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)? != 0,
                    ))
                },
            )
            .optional()?)
    }

    /// Texts of the user messages of `chat` that the owner typed in the desktop app.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn owner_message_texts(&self, chat: ChatId) -> Result<Vec<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT content FROM messages WHERE chat_id = ?1 AND role = 'user' AND source = 'owner' AND superseded = 0
             ORDER BY created_at_ms DESC LIMIT 200",
        )?;
        let rows = stmt.query_map(params![chat.to_string()], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Record that the file (or, with `tree`, everything under the directory) at the already
    /// normalised `path` has an untrusted origin. Idempotent; a tree mark is never downgraded.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn mark_untrusted_path(&self, path: &str, tree: bool, origin: &str) -> Result<()> {
        self.conn().execute(
            "INSERT INTO file_provenance (path, tree, origin, recorded_at_ms) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(path) DO UPDATE SET tree = MAX(tree, excluded.tree)",
            params![path, i64::from(tree), origin, now_ms()],
        )?;
        Ok(())
    }

    /// Recorded provenance spellings, including entries written before canonical identities.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn untrusted_paths(&self) -> Result<Vec<(String, bool)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT path, tree FROM file_provenance")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Whether `path` (normalised: `/` separators, no trailing slash) is marked, itself or through
    /// a marked ancestor directory.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn is_untrusted_path(&self, path: &str) -> Result<bool> {
        let conn = self.conn();
        let mut probe = Some(path);
        let mut first = true;
        while let Some(p) = probe {
            let hit: Option<i64> = conn
                .query_row(
                    "SELECT tree FROM file_provenance WHERE path = ?1",
                    params![p],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(tree) = hit
                && (first || tree == 1)
            {
                return Ok(true);
            }
            first = false;
            probe = p.rfind('/').filter(|i| *i > 0).map(|i| &p[..i]);
        }
        Ok(false)
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

    /// A user's audit entries inside an optional time window, newest first.
    ///
    /// # Errors
    /// Fails on a database error.
    pub fn list_audit_between(
        &self,
        user_id: UserId,
        since_ms: Option<i64>,
        until_ms: Option<i64>,
    ) -> Result<Vec<AuditEntry>> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT id, user_id, run_id, actor, action, detail_json, ts_ms FROM audit_log WHERE user_id = ?1 AND ts_ms >= ?2 AND ts_ms <= ?3 ORDER BY id DESC",
        )?;
        Ok(st
            .query_map(
                params![
                    user_id.to_string(),
                    since_ms.unwrap_or(i64::MIN),
                    until_ms.unwrap_or(i64::MAX)
                ],
                audit_row,
            )?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Integrity facts about the audit log: `(rows, first_id, last_id,
    /// protecting_triggers)`. There is no hash chain; ids come from
    /// `AUTOINCREMENT` and are never reused, so a row removed behind the
    /// append-only triggers shows up as a gap between `rows` and the id span.
    ///
    /// # Errors
    /// Fails on a database error.
    pub fn audit_integrity(&self) -> Result<(i64, Option<i64>, Option<i64>, i64)> {
        let conn = self.conn();
        let (rows, first, last) = conn.query_row(
            "SELECT COUNT(*), MIN(id), MAX(id) FROM audit_log",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let triggers = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'trigger' AND tbl_name = 'audit_log' AND name IN ('audit_log_no_update', 'audit_log_no_delete')",
            [],
            |r| r.get(0),
        )?;
        Ok((rows, first, last, triggers))
    }

    /// Audit rows of one action written at or after `since_ms` (avoids loading
    /// the whole log for per-call accounting).
    ///
    /// # Errors
    /// Fails on a database error.
    pub fn list_audit_action_since(
        &self,
        user_id: UserId,
        action: &str,
        since_ms: i64,
    ) -> Result<Vec<AuditEntry>> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT id, user_id, run_id, actor, action, detail_json, ts_ms FROM audit_log WHERE user_id = ?1 AND action = ?2 AND ts_ms >= ?3 ORDER BY id",
        )?;
        Ok(st
            .query_map(params![user_id.to_string(), action, since_ms], audit_row)?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Month-to-date usage ledger totals computed in SQL: the sum of recorded
    /// `cost_usd` (never negative per row) and the count of calls that reported
    /// tokens but have no price. Only `ledger_version` 1 rows count.
    ///
    /// # Errors
    /// Fails on a database error.
    #[allow(clippy::cast_sign_loss)]
    pub fn usage_totals_since(&self, user_id: UserId, since_ms: i64) -> Result<(f64, u64)> {
        Ok(self.conn().query_row(
            "SELECT COALESCE(SUM(CASE WHEN json_type(detail_json, '$.cost_usd') IN ('real','integer') THEN MAX(json_extract(detail_json, '$.cost_usd'), 0.0) END), 0.0), COALESCE(SUM(CASE WHEN COALESCE(json_type(detail_json, '$.cost_usd'), 'null') NOT IN ('real','integer') AND json_type(detail_json, '$.tokens') = 'object' THEN 1 END), 0) FROM audit_log WHERE user_id = ?1 AND action = 'usage' AND ts_ms >= ?2 AND json_extract(detail_json, '$.ledger_version') = 1",
            params![user_id.to_string(), since_ms],
            |row| Ok((row.get::<_, f64>(0)?, row.get::<_, i64>(1)?.max(0) as u64)),
        )?)
    }

    /// Per-bot usage since `since_ms` from the usage ledger: `(usd, tokens, unreported_calls)`.
    ///
    /// Counting rules (ADR-0034): a call with a recorded `cost_usd` adds that
    /// cost; a call whose tokens were never reported adds the conservative
    /// estimate stored at call start (`limit_estimate_usd` / `limit_estimate_tokens`),
    /// so missing provider data cannot make spending free. A call with tokens
    /// but no known price adds tokens only. Subagent runs belong to their bot.
    ///
    /// # Errors
    /// Fails on a database error.
    #[allow(clippy::cast_sign_loss)]
    pub fn bot_usage_since(
        &self,
        user_id: UserId,
        bot_id: BotId,
        since_ms: i64,
    ) -> Result<(f64, u64, u64)> {
        Ok(self.conn().query_row(
            "SELECT                COALESCE(SUM(CASE                  WHEN json_type(a.detail_json, '$.cost_usd') IN ('real','integer') THEN MAX(json_extract(a.detail_json, '$.cost_usd'), 0.0)                  WHEN json_type(a.detail_json, '$.tokens') = 'object' THEN 0.0                  ELSE COALESCE(MAX(json_extract(a.detail_json, '$.limit_estimate_usd'), 0.0), 0.0) END), 0.0),                COALESCE(SUM(CASE                  WHEN json_type(a.detail_json, '$.tokens') = 'object' THEN MAX(COALESCE(json_extract(a.detail_json, '$.prompt_tokens'), 0), 0) + MAX(COALESCE(json_extract(a.detail_json, '$.completion_tokens'), 0), 0)                  ELSE MAX(COALESCE(json_extract(a.detail_json, '$.limit_estimate_tokens'), 0), 0) END), 0),                COALESCE(SUM(CASE WHEN json_type(a.detail_json, '$.tokens') = 'object' THEN 0 ELSE 1 END), 0)              FROM audit_log a JOIN runs r ON r.id = a.run_id              WHERE a.user_id = ?1 AND (r.bot_id = ?2 OR json_extract(r.spec_json, '$.delegation.caller_bot_id') = ?2) AND a.action = 'usage' AND a.ts_ms >= ?3                AND json_extract(a.detail_json, '$.ledger_version') = 1",
            params![user_id.to_string(), bot_id.to_string(), since_ms],
            |row| {
                Ok((
                    row.get::<_, f64>(0)?,
                    row.get::<_, i64>(1)?.max(0) as u64,
                    row.get::<_, i64>(2)?.max(0) as u64,
                ))
            },
        )?)
    }

    /// The stored limits JSON of a bot; `None` when no row exists (defaults apply).
    ///
    /// # Errors
    /// Fails on a database error.
    pub fn get_bot_spend_limits(&self, bot_id: BotId) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT limits_json FROM bot_spend_limits WHERE bot_id = ?1",
                [bot_id.to_string()],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Replace a bot's limits JSON and write the audit entry in ONE transaction,
    /// so a change can never exist without its audit record.
    ///
    /// # Errors
    /// Fails on a database error (including an unknown bot).
    pub fn set_bot_spend_limits(
        &self,
        user_id: UserId,
        bot_id: BotId,
        limits_json: &str,
        audit_detail: &Value,
    ) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO bot_spend_limits (bot_id, user_id, limits_json, updated_at_ms) VALUES (?1, ?2, ?3, ?4)              ON CONFLICT(bot_id) DO UPDATE SET limits_json = excluded.limits_json, updated_at_ms = excluded.updated_at_ms",
            params![bot_id.to_string(), user_id.to_string(), limits_json, now_ms()],
        )?;
        tx.execute(
            "INSERT INTO audit_log (user_id, run_id, actor, action, detail_json, ts_ms) VALUES (?1, NULL, 'owner', 'bot.spend_limits_changed', ?2, ?3)",
            params![user_id.to_string(), serde_json::to_string(audit_detail)?, now_ms()],
        )?;
        tx.commit()?;
        Ok(())
    }

    // ---- bot_workspaces & workspace_settings ----

    /// Fetch a bot's persisted workspace mapping and limits, if created yet.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn get_bot_workspace(&self, bot_id: BotId) -> Result<Option<BotWorkspaceRecord>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT bot_id, user_id, folder_name, soft_limit_bytes, hard_limit_bytes, created_at_ms, updated_at_ms FROM bot_workspaces WHERE bot_id = ?1",
                [bot_id.to_string()],
                bot_workspace_row,
            )
            .optional()?)
    }

    /// List all persisted bot workspace records for `user_id`.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn list_bot_workspaces(&self, user_id: UserId) -> Result<Vec<BotWorkspaceRecord>> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT bot_id, user_id, folder_name, soft_limit_bytes, hard_limit_bytes, created_at_ms, updated_at_ms FROM bot_workspaces WHERE user_id = ?1 ORDER BY created_at_ms, bot_id",
        )?;
        let rows = st.query_map([user_id.to_string()], bot_workspace_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Return the existing workspace record for `bot_id`, or create one with a
    /// unique `folder_name` derived from `preferred_folder_name`.
    ///
    /// # Errors
    /// Fails on SQLite errors or when `bot_id` does not exist.
    pub fn ensure_bot_workspace(
        &self,
        user_id: UserId,
        bot_id: BotId,
        preferred_folder_name: &str,
    ) -> Result<BotWorkspaceRecord> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let record = ensure_bot_workspace_tx(&tx, user_id, bot_id, preferred_folder_name)?;
        tx.commit()?;
        Ok(record)
    }

    /// Update a bot's optional soft/hard workspace limits and record an audit
    /// entry in the same transaction.
    ///
    /// # Errors
    /// Fails on SQLite errors or when `bot_id` does not exist.
    pub fn set_bot_workspace_limits(
        &self,
        user_id: UserId,
        bot_id: BotId,
        preferred_folder_name: &str,
        soft_limit_bytes: Option<u64>,
        hard_limit_bytes: Option<u64>,
        audit_detail: &Value,
    ) -> Result<BotWorkspaceRecord> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut record = ensure_bot_workspace_tx(&tx, user_id, bot_id, preferred_folder_name)?;
        let now = now_ms();
        let soft = soft_limit_bytes.map(|v| i64::try_from(v).unwrap_or(i64::MAX));
        let hard = hard_limit_bytes.map(|v| i64::try_from(v).unwrap_or(i64::MAX));
        tx.execute(
            "UPDATE bot_workspaces SET soft_limit_bytes = ?2, hard_limit_bytes = ?3, updated_at_ms = ?4 WHERE bot_id = ?1",
            params![bot_id.to_string(), soft, hard, now],
        )?;
        tx.execute(
            "INSERT INTO audit_log (user_id, run_id, actor, action, detail_json, ts_ms) VALUES (?1, NULL, 'owner', 'bot.workspace_limits_changed', ?2, ?3)",
            params![user_id.to_string(), serde_json::to_string(audit_detail)?, now],
        )?;
        tx.commit()?;
        record.soft_limit_bytes = soft_limit_bytes;
        record.hard_limit_bytes = hard_limit_bytes;
        record.updated_at_ms = now;
        Ok(record)
    }

    /// Host disk warning thresholds and workspace cleanup settings for `user_id`.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn get_workspace_settings(&self, user_id: UserId) -> Result<WorkspaceSettingsRecord> {
        let row = self
            .conn()
            .query_row(
                "SELECT warn_percent_1, warn_percent_2, warn_percent_3, min_free_bytes, downloads_max_age_days, logs_max_age_days, updated_at_ms FROM workspace_settings WHERE user_id = ?1",
                [user_id.to_string()],
                |r| {
                    let p1: i64 = r.get(0)?;
                    let p2: i64 = r.get(1)?;
                    let p3: i64 = r.get(2)?;
                    let min_free: i64 = r.get(3)?;
                    let dl_days: Option<i64> = r.get(4)?;
                    let log_days: Option<i64> = r.get(5)?;
                    let updated: i64 = r.get(6)?;
                    Ok(WorkspaceSettingsRecord {
                        warn_percent_1: u8::try_from(p1).unwrap_or(80),
                        warn_percent_2: u8::try_from(p2).unwrap_or(90),
                        warn_percent_3: u8::try_from(p3).unwrap_or(95),
                        min_free_bytes: u64::try_from(min_free).unwrap_or(0),
                        downloads_max_age_days: dl_days.and_then(|d| u32::try_from(d).ok()),
                        logs_max_age_days: log_days.and_then(|d| u32::try_from(d).ok()),
                        updated_at_ms: updated,
                    })
                },
            )
            .optional()?;
        Ok(row.unwrap_or_default())
    }

    /// Save host disk warning thresholds and workspace cleanup policy with an
    /// audit record in the same transaction.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn set_workspace_settings(
        &self,
        user_id: UserId,
        settings: &WorkspaceSettingsRecord,
        audit_detail: &Value,
    ) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let now = now_ms();
        tx.execute(
            "INSERT INTO workspace_settings (user_id, warn_percent_1, warn_percent_2, warn_percent_3, min_free_bytes, downloads_max_age_days, logs_max_age_days, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(user_id) DO UPDATE SET
               warn_percent_1 = excluded.warn_percent_1,
               warn_percent_2 = excluded.warn_percent_2,
               warn_percent_3 = excluded.warn_percent_3,
               min_free_bytes = excluded.min_free_bytes,
               downloads_max_age_days = excluded.downloads_max_age_days,
               logs_max_age_days = excluded.logs_max_age_days,
               updated_at_ms = excluded.updated_at_ms",
            params![
                user_id.to_string(),
                i64::from(settings.warn_percent_1),
                i64::from(settings.warn_percent_2),
                i64::from(settings.warn_percent_3),
                i64::try_from(settings.min_free_bytes).unwrap_or(i64::MAX),
                settings.downloads_max_age_days.map(i64::from),
                settings.logs_max_age_days.map(i64::from),
                now,
            ],
        )?;
        tx.execute(
            "INSERT INTO audit_log (user_id, run_id, actor, action, detail_json, ts_ms) VALUES (?1, NULL, 'owner', 'workspace.settings_changed', ?2, ?3)",
            params![user_id.to_string(), serde_json::to_string(audit_detail)?, now],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Largest `prompt_tokens` any usage row of one run recorded.
    ///
    /// # Errors
    /// Fails on a database error.
    #[allow(clippy::cast_sign_loss)]
    pub fn max_run_prompt_tokens(&self, user_id: UserId, run_id: RunId) -> Result<Option<u64>> {
        let v: Option<i64> = self.conn().query_row(
            "SELECT MAX(json_extract(detail_json, '$.prompt_tokens')) FROM audit_log WHERE user_id = ?1 AND run_id = ?2 AND action = 'usage'",
            params![user_id.to_string(), run_id.to_string()],
            |row| row.get(0),
        )?;
        Ok(v.filter(|n| *n >= 0).map(|n| n as u64))
    }

    /// Whether one run has an audit fact, without loading other runs' details.
    ///
    /// # Errors
    /// Fails on a database error.
    pub fn has_run_audit_action(
        &self,
        user_id: UserId,
        run_id: RunId,
        action: &str,
    ) -> Result<bool> {
        Ok(self.conn().query_row(
            "SELECT EXISTS(SELECT 1 FROM audit_log WHERE user_id = ?1 AND run_id = ?2 AND action = ?3)",
            params![user_id.to_string(), run_id.to_string(), action],
            |row| row.get(0),
        )?)
    }

    /// The audit rows of one run, oldest first.
    ///
    /// # Errors
    /// Fails on a database error.
    pub fn list_audit_for_run(&self, user_id: UserId, run_id: RunId) -> Result<Vec<AuditEntry>> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT id, user_id, run_id, actor, action, detail_json, ts_ms FROM audit_log WHERE user_id = ?1 AND run_id = ?2 ORDER BY id",
        )?;
        Ok(st
            .query_map([user_id.to_string(), run_id.to_string()], audit_row)?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// The newest audit row of one action for a user (indexed by `id`, no full scan).
    ///
    /// # Errors
    /// Fails on a database error.
    pub fn last_audit_by_action(
        &self,
        user_id: UserId,
        action: &str,
    ) -> Result<Option<AuditEntry>> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT id, user_id, run_id, actor, action, detail_json, ts_ms FROM audit_log WHERE user_id = ?1 AND action = ?2 ORDER BY id DESC LIMIT 1",
        )?;
        Ok(st
            .query_map(params![user_id.to_string(), action], audit_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .next())
    }

    // ---- global event order ----

    /// Highest `gseq` handed out so far (`0` when no event was indexed yet).
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn last_gseq(&self) -> Result<u64> {
        let v: i64 =
            self.conn()
                .query_row("SELECT COALESCE(MAX(gseq), 0) FROM event_index", [], |r| {
                    r.get(0)
                })?;
        Ok(u64::try_from(v).unwrap_or(0))
    }

    /// Record that event `seq` of `run_id` has the daemon-wide number `gseq`.
    ///
    /// # Errors
    /// Fails on SQLite errors (including a reused `gseq`).
    pub fn index_event(&self, gseq: u64, run_id: RunId, seq: u64) -> Result<()> {
        self.conn().execute(
            "INSERT INTO event_index (gseq, run_id, seq) VALUES (?1, ?2, ?3)",
            params![
                i64::try_from(gseq).unwrap_or(i64::MAX),
                run_id.to_string(),
                i64::try_from(seq).unwrap_or(i64::MAX)
            ],
        )?;
        Ok(())
    }

    /// Up to `limit` index rows with `gseq > after`, in order.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn list_event_index(&self, after: u64, limit: u32) -> Result<Vec<EventIndexRow>> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT gseq, run_id, seq FROM event_index WHERE gseq > ?1 ORDER BY gseq LIMIT ?2",
        )?;
        let rows = st.query_map(
            params![i64::try_from(after).unwrap_or(i64::MAX), limit],
            |r| {
                let gseq: i64 = r.get(0)?;
                let seq: i64 = r.get(2)?;
                Ok(EventIndexRow {
                    gseq: u64::try_from(gseq).unwrap_or(0),
                    run_id: RunId(uuid_at(r, 1)?),
                    seq: u64::try_from(seq).unwrap_or(0),
                })
            },
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Run a raw statement (test hook to prove schema-level guarantees).
    #[cfg(test)]
    pub(crate) fn raw_execute(&self, sql: &str) -> rusqlite::Result<usize> {
        self.conn().execute(sql, [])
    }
}

const RUN_SELECT: &str = "SELECT id, user_id, bot_id, chat_id, kind, state, error, created_at_ms, updated_at_ms, parent_run_id, spec_json, computer_target, pause_reason, computer_node, computer_node_name, paused_at_ms FROM runs";
const STEP_SELECT: &str = "SELECT id, run_id, user_id, idx, name, input_json, idempotent, retry_json, status, attempt, output_json, error, started_at_ms, finished_at_ms FROM run_steps";
const APPROVAL_SELECT: &str = "SELECT id, user_id, run_id, step_id, summary, decision, requested_at_ms, decided_at_ms FROM approvals";

fn user_row(r: &Row<'_>) -> rusqlite::Result<User> {
    Ok(User {
        id: UserId(uuid_at(r, 0)?),
        name: r.get(1)?,
        created_at_ms: r.get(2)?,
    })
}

/// Outcome of inspecting a bot's skill assignment row for UI/management repair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillAssignmentLookup {
    /// Well-formed row, or `None` when the bot uses the default (`All`).
    Ok(Option<BotSkillAssignment>),
    /// Row exists in `bot_skill_assignment` but its mode or `names_json` is
    /// malformed; runtime enforcement fails closed while management surfaces can
    /// overwrite it.
    Corrupt {
        /// Stored row version (or `0` if unreadable).
        version: u64,
        /// Validation/parse error.
        reason: String,
    },
}

const MAX_SKILL_ASSIGNMENTS: usize = 200;
const MAX_SKILL_NAME_LEN: usize = 64;

/// Validate a skill name using the same grammar as `skills::validate_name`.
#[must_use]
pub fn is_valid_skill_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_SKILL_NAME_LEN
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'))
        && name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && !name.contains("..")
        && !name.ends_with('.')
}

fn normalize_skill_names(names: &[String]) -> std::result::Result<Vec<String>, String> {
    if names.len() > MAX_SKILL_ASSIGNMENTS {
        return Err(format!(
            "at most {MAX_SKILL_ASSIGNMENTS} assigned skills are allowed"
        ));
    }
    let mut set = std::collections::BTreeSet::new();
    for name in names {
        if !is_valid_skill_name(name) {
            return Err(format!("invalid skill name `{name}`"));
        }
        set.insert(name.clone());
    }
    Ok(set.into_iter().collect())
}

fn write_skill_assignment_tx(
    tx: &rusqlite::Transaction<'_>,
    bot_id: BotId,
    mode: SkillAssignmentMode,
    names: &[String],
    expected_version: Option<u64>,
) -> Result<BotSkillAssignment> {
    let bid = bot_id.to_string();
    if let Some(expected) = expected_version {
        let current_raw: Option<i64> = tx
            .query_row(
                "SELECT version FROM bot_skill_assignment WHERE bot_id = ?1",
                [&bid],
                |r| r.get(0),
            )
            .optional()?;
        let current = current_raw.and_then(|v| u64::try_from(v).ok()).unwrap_or(0);
        if current != expected {
            return Err(StorageError::Conflict(format!(
                "bot skill assignment changed (expected version {expected}, current {current})"
            )));
        }
    }
    let ts = now_ms();
    let clean = normalize_skill_names(names)
        .map_err(|msg| std::io::Error::new(std::io::ErrorKind::InvalidInput, msg))?;
    match mode {
        SkillAssignmentMode::All => {
            tx.execute("DELETE FROM bot_skill_assignment WHERE bot_id = ?1", [&bid])?;
            Ok(BotSkillAssignment {
                bot_id,
                mode,
                names: Vec::new(),
                updated_at_ms: ts,
                version: 0,
            })
        }
        SkillAssignmentMode::Selected => {
            let json = serde_json::to_string(&clean)?;
            let cur_gen: i64 = tx.query_row(
                "SELECT generation FROM bot_skill_assignment_state WHERE id = 1",
                [],
                |r| r.get(0),
            )?;
            let next_ver = cur_gen.saturating_add(1);
            tx.execute(
                "INSERT INTO bot_skill_assignment (bot_id, mode, names_json, updated_at_ms, version) \
                 VALUES (?1, 'selected', ?2, ?3, ?4) \
                 ON CONFLICT(bot_id) DO UPDATE SET \
                 mode = excluded.mode, names_json = excluded.names_json, updated_at_ms = excluded.updated_at_ms, version = excluded.version",
                params![bid, json, ts, next_ver],
            )?;
            Ok(BotSkillAssignment {
                bot_id,
                mode,
                names: clean,
                updated_at_ms: ts,
                version: u64::try_from(next_ver).unwrap_or(1),
            })
        }
    }
}

fn skill_assignment_row(r: &Row<'_>) -> rusqlite::Result<BotSkillAssignment> {
    let bot_id = BotId(uuid_at(r, 0)?);
    let mode_str: String = r.get(1)?;
    let mode = match mode_str.as_str() {
        "all" => SkillAssignmentMode::All,
        "selected" => SkillAssignmentMode::Selected,
        other => {
            return Err(conv_err(
                1,
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("invalid skill assignment mode `{other}`"),
                ),
            ));
        }
    };
    let names_raw: String = r.get(2)?;
    let parsed: Vec<String> = serde_json::from_str(&names_raw).map_err(|e| conv_err(2, e))?;
    let names = normalize_skill_names(&parsed)
        .map_err(|msg| conv_err(2, std::io::Error::new(std::io::ErrorKind::InvalidData, msg)))?;
    let ver: i64 = r.get(4)?;
    Ok(BotSkillAssignment {
        bot_id,
        mode,
        names,
        updated_at_ms: r.get(3)?,
        version: u64::try_from(ver).unwrap_or(0),
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

pub(crate) fn message_row(r: &Row<'_>) -> rusqlite::Result<Message> {
    Ok(Message {
        id: uuid_at(r, 0)?,
        user_id: UserId(uuid_at(r, 1)?),
        chat_id: ChatId(uuid_at(r, 2)?),
        run_id: opt_uuid_at(r, 3)?.map(RunId),
        role: r.get(4)?,
        content: r.get(5)?,
        created_at_ms: r.get(6)?,
        exposure_untrusted: r.get::<_, i64>(7)? != 0,
        exposure_private: r.get::<_, i64>(8)? != 0,
        exposure_sources: serde_json::from_str(&r.get::<_, String>(9)?).unwrap_or_default(),
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
        parent_run_id: opt_uuid_at(r, 9)?.map(RunId),
        spec: opt_json_at(r, 10)?,
        computer_target: r.get(11)?,
        computer_node: r.get(13)?,
        computer_node_name: r.get(14)?,
        paused_at_ms: r.get(15)?,
        pause_reason: r.get(12)?,
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

fn bot_workspace_row(r: &Row<'_>) -> rusqlite::Result<BotWorkspaceRecord> {
    let soft: Option<i64> = r.get(3)?;
    let hard: Option<i64> = r.get(4)?;
    Ok(BotWorkspaceRecord {
        bot_id: BotId(uuid_at(r, 0)?),
        user_id: UserId(uuid_at(r, 1)?),
        folder_name: r.get(2)?,
        soft_limit_bytes: soft.and_then(|v| u64::try_from(v).ok()),
        hard_limit_bytes: hard.and_then(|v| u64::try_from(v).ok()),
        created_at_ms: r.get(5)?,
        updated_at_ms: r.get(6)?,
    })
}

/// Comparison key for workspace folder names: file systems answer to other
/// spellings of the same folder (case, Unicode normalisation, the Turkish dotted
/// and dotless i), so two names with the same key must never both be handed out.
/// NFKC, then upper and lower case (this also folds `ß`/`SS` and final sigma),
/// then NFC. Deliberately coarser than any one file system.
fn folder_fold_key(name: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    let nfkc: String = name.nfkc().collect();
    let folded = nfkc.to_uppercase().to_lowercase();
    folded.nfkc().collect()
}

fn ensure_bot_workspace_tx(
    tx: &rusqlite::Transaction<'_>,
    user_id: UserId,
    bot_id: BotId,
    preferred_folder_name: &str,
) -> Result<BotWorkspaceRecord> {
    if let Some(existing) = tx
        .query_row(
            "SELECT bot_id, user_id, folder_name, soft_limit_bytes, hard_limit_bytes, created_at_ms, updated_at_ms FROM bot_workspaces WHERE bot_id = ?1",
            [bot_id.to_string()],
            bot_workspace_row,
        )
        .optional()?
    {
        return Ok(existing);
    }
    let mut existing_folders: Vec<String> = {
        let mut st = tx.prepare("SELECT folder_name FROM bot_workspaces WHERE user_id = ?1")?;
        let rows = st.query_map([user_id.to_string()], |r| r.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    for s in &mut existing_folders {
        *s = folder_fold_key(s);
    }
    let base = if preferred_folder_name.is_empty() {
        "bot"
    } else {
        preferred_folder_name
    };
    let short_id = bot_id.0.simple().to_string();
    let short8 = &short_id[..8];
    let mut candidate = base.to_owned();
    if existing_folders.contains(&folder_fold_key(&candidate)) {
        candidate = format!("{base}-{short8}");
    }
    let mut suffix = 2u32;
    while existing_folders.contains(&folder_fold_key(&candidate)) {
        candidate = format!("{base}-{short8}-{suffix}");
        suffix += 1;
    }
    let now = now_ms();
    tx.execute(
        "INSERT INTO bot_workspaces (bot_id, user_id, folder_name, soft_limit_bytes, hard_limit_bytes, created_at_ms, updated_at_ms) VALUES (?1, ?2, ?3, NULL, NULL, ?4, ?4)",
        params![bot_id.to_string(), user_id.to_string(), candidate, now],
    )?;
    Ok(BotWorkspaceRecord {
        bot_id,
        user_id,
        folder_name: candidate,
        soft_limit_bytes: None,
        hard_limit_bytes: None,
        created_at_ms: now,
        updated_at_ms: now,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn upgrade_of_an_older_database_is_backed_up_first() {
        let dir = tempfile::tempdir().unwrap();
        {
            let conn = Connection::open(dir.path().join("bot.db")).unwrap();
            conn.execute_batch(
                "CREATE TABLE marker (x); INSERT INTO marker VALUES (1); PRAGMA user_version = 3;",
            )
            .unwrap();
        }
        // user_version 3 without a ledger bootstraps and then migrates.
        let _ = Storage::open(dir.path());
        let backups: Vec<_> = std::fs::read_dir(dir.path().join("backups"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(backups.len(), 1, "{backups:?}");
        assert!(backups[0].starts_with("bot-v3-"));
        let copy = Connection::open(dir.path().join("backups").join(&backups[0])).unwrap();
        let n: i64 = copy
            .query_row("SELECT x FROM marker", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        // Fresh database: no backups directory content added on reopen at latest.
        let fresh = tempfile::tempdir().unwrap();
        Storage::open(fresh.path()).unwrap();
        assert!(!fresh.path().join("backups").exists());
    }

    use super::*;
    use serde_json::json;

    #[test]
    fn data_dir_is_canonical_but_never_a_verbatim_path() {
        let dir = tempfile::tempdir().unwrap();
        let st = Storage::open(dir.path().join("nested").join("..")).unwrap();
        assert!(!st.data_dir().to_string_lossy().starts_with("\\\\?\\"));
        assert_eq!(
            std::fs::canonicalize(st.data_dir()).unwrap(),
            std::fs::canonicalize(dir.path()).unwrap()
        );
    }

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
            parent_run_id: None,
            spec: None,
            computer_target: None,
            computer_node: None,
            computer_node_name: None,
            paused_at_ms: None,
            pause_reason: None,
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
            exposure_untrusted: false,
            exposure_private: false,
            exposure_sources: Vec::new(),
        };
        f.st.insert_message(&m).unwrap();
        assert_eq!(f.st.get_message(m.id).unwrap().unwrap(), m);
        assert_eq!(f.st.list_messages(f.chat.id).unwrap(), vec![m]);
        assert!(f.st.get_user(UserId::new()).unwrap().is_none());
    }

    #[test]
    fn full_text_history_ranks_phrases_and_tracks_visibility_and_deletion() {
        let f = fx();
        let insert = |text: &str, private: bool, role: &str| {
            let m = Message {
                id: Uuid::now_v7(),
                user_id: f.user.id,
                chat_id: f.chat.id,
                run_id: None,
                role: role.into(),
                content: text.into(),
                created_at_ms: 1,
                exposure_untrusted: false,
                exposure_private: private,
                exposure_sources: vec![],
            };
            f.st.insert_message(&m).unwrap();
            m.id
        };
        let old = insert("İstanbul güzel şehir çalışma", false, "assistant");
        insert("old English phrase", false, "user");
        insert(
            &format!(
                "{} old English phrase {}",
                "padding ".repeat(300),
                "tail ".repeat(300)
            ),
            false,
            "assistant",
        );
        insert("old English phrase", true, "user");
        insert("old English phrase", false, "tool");
        let search = |q| {
            f.st.search_messages(f.user.id, SearchFilter::default(), q, 0, 100)
                .unwrap()
        };
        assert_eq!(search("istanbul guzel sehir")[0].0.id, old);
        assert_eq!(search("İstanbul güzel şehir").len(), 1);
        assert_eq!(search("calisma")[0].1, "İstanbul güzel şehir çalışma");
        let ranked = search("old English phrase");
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].0.content, "old English phrase");
        assert!(ranked[1].1.len() < 320);
        assert_eq!(
            f.st.search_messages(UserId::new(), SearchFilter::default(), "old", 0, 30)
                .unwrap(),
            vec![]
        );
        assert_eq!(
            f.st.search_messages(
                f.user.id,
                SearchFilter {
                    bot: Some(BotId::new()),
                    ..SearchFilter::default()
                },
                "old",
                0,
                30,
            )
            .unwrap(),
            vec![]
        );
        assert_eq!(
            f.st.search_messages(
                f.user.id,
                SearchFilter {
                    chat: Some(ChatId::new()),
                    ..SearchFilter::default()
                },
                "old",
                0,
                30,
            )
            .unwrap(),
            vec![]
        );
        f.st.set_messages_superseded(&[old], true).unwrap();
        assert_eq!(search("istanbul"), vec![]);
        f.st.set_messages_superseded(&[old], false).unwrap();
        assert_eq!(search("istanbul").len(), 1);
        f.st.delete_chat(f.chat.id).unwrap();
        assert_eq!(search("old"), vec![]);
    }

    fn message_at(f: &Fx, text: &str, at: i64) -> Message {
        Message {
            id: Uuid::now_v7(),
            user_id: f.user.id,
            chat_id: f.chat.id,
            run_id: None,
            role: "user".into(),
            content: text.into(),
            created_at_ms: at,
            exposure_untrusted: false,
            exposure_private: false,
            exposure_sources: vec![],
        }
    }

    #[test]
    fn history_search_filters_by_date_file_and_link_before_paging() {
        let f = fx();
        let plain = message_at(&f, "rapor taslağı", 1_000);
        let linked = message_at(&f, "rapor https://example.com/a", 2_000);
        let filed = message_at(
            &f,
            "rapor ekte

[[praxi-attachments:[{\"name\":\"a.pdf\"}]]]",
            3_000,
        );
        for m in [&plain, &linked, &filed] {
            f.st.insert_message(m).unwrap();
        }
        let ids = |filter: SearchFilter| {
            f.st.search_messages(f.user.id, filter, "RAPOR", 0, 30)
                .unwrap()
                .into_iter()
                .map(|(m, _)| m.id)
                .collect::<std::collections::HashSet<_>>()
        };
        assert_eq!(ids(SearchFilter::default()).len(), 3);
        let only = |m: &Message| std::collections::HashSet::from([m.id]);
        let with_link = SearchFilter {
            has_link: true,
            ..SearchFilter::default()
        };
        assert_eq!(ids(with_link), only(&linked));
        let with_file = SearchFilter {
            has_file: true,
            ..SearchFilter::default()
        };
        assert_eq!(ids(with_file), only(&filed));
        let window = SearchFilter {
            from_ms: Some(1_500),
            to_ms: Some(2_500),
            ..SearchFilter::default()
        };
        assert_eq!(ids(window), only(&linked));
    }

    #[test]
    fn history_search_stays_fast_with_fifty_thousand_messages() {
        let f = fx();
        {
            let mut conn = f.st.conn();
            let tx = conn.transaction().unwrap();
            let mut st = tx
                .prepare(
                    "INSERT INTO messages (id, user_id, chat_id, run_id, role, content, created_at_ms,
                     exposure_untrusted, exposure_private, exposure_sources) VALUES (?1, ?2, ?3, NULL, 'user', ?4, ?5, 0, 0, '[]')",
                )
                .unwrap();
            for i in 0..50_000_i64 {
                let text = if i == 49_999 {
                    "Yarın İstanbul'a uçuyorum".to_owned()
                } else {
                    format!("mesaj {i} günlük notlar toplantı özeti çalışma planı")
                };
                st.execute(params![
                    Uuid::now_v7().to_string(),
                    f.user.id.to_string(),
                    f.chat.id.to_string(),
                    text,
                    i
                ])
                .unwrap();
            }
            drop(st);
            tx.commit().unwrap();
        }
        let started = std::time::Instant::now();
        let hits =
            f.st.search_messages(f.user.id, SearchFilter::default(), "istanbul", 0, 30)
                .unwrap();
        let broad =
            f.st.search_messages(f.user.id, SearchFilter::default(), "toplanti", 0, 30)
                .unwrap();
        let elapsed = started.elapsed();
        assert_eq!(hits.len(), 1);
        assert_eq!(broad.len(), 30);
        assert!(elapsed.as_millis() < 1500, "search took {elapsed:?}");
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

    fn dialogue(f: &Fx, n: u128) -> Vec<Uuid> {
        (1..=n)
            .map(|i| {
                let id = Uuid::from_u128(i);
                f.st.insert_message(&Message {
                    id,
                    user_id: f.user.id,
                    chat_id: f.chat.id,
                    run_id: None,
                    role: if i % 2 == 1 { "user" } else { "assistant" }.into(),
                    content: format!("m{i}"),
                    // Two messages share a millisecond: the id breaks the tie.
                    created_at_ms: i64::try_from(i / 2).unwrap(),
                    exposure_untrusted: false,
                    exposure_private: false,
                    exposure_sources: Vec::new(),
                })
                .unwrap();
                id
            })
            .collect()
    }

    fn summary_of(f: &Fx, ids: &[Uuid], until: usize) -> crate::ChatSummary {
        let last = f.st.get_message(ids[until]).unwrap().unwrap();
        crate::ChatSummary {
            chat_id: f.chat.id,
            user_id: f.user.id,
            content: "earlier".into(),
            covered_until_ms: last.created_at_ms,
            covered_until_id: last.id,
            covered_messages: u32::try_from(until + 1).unwrap(),
            rounds: 1,
            first_message_ms: 0,
            exposure_untrusted: true,
            exposure_private: false,
            exposure_sources: vec!["page example.com".into()],
            model: "p/m".into(),
            created_at_ms: 10,
            updated_at_ms: 10,
        }
    }

    #[test]
    fn chat_summary_round_trips_and_a_round_replaces_the_row() {
        let f = fx();
        let ids = dialogue(&f, 6);
        assert_eq!(f.st.get_chat_summary(f.chat.id).unwrap(), None);
        let mut s = summary_of(&f, &ids, 2);
        f.st.put_chat_summary(&s).unwrap();
        assert_eq!(f.st.get_chat_summary(f.chat.id).unwrap(), Some(s.clone()));
        s = crate::ChatSummary {
            rounds: 2,
            content: "later".into(),
            ..summary_of(&f, &ids, 4)
        };
        f.st.put_chat_summary(&s).unwrap();
        assert_eq!(f.st.get_chat_summary(f.chat.id).unwrap(), Some(s));
        assert!(f.st.delete_chat_summary(f.chat.id).unwrap());
        assert!(!f.st.delete_chat_summary(f.chat.id).unwrap());
    }

    #[test]
    fn messages_after_the_cursor_are_listed_and_the_summarised_ones_stay_stored() {
        let f = fx();
        let ids = dialogue(&f, 6);
        // The cursor sits on m2, which shares its millisecond with m3: the id breaks the tie.
        let s = summary_of(&f, &ids, 1);
        let (tail, omitted) =
            f.st.list_messages_bounded_after(f.chat.id, Some(s.cursor()), None, 10, 100)
                .unwrap();
        let texts: Vec<_> = tail.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(texts, ["m3", "m4", "m5", "m6"]);
        assert!(!omitted, "older messages are replaced, not omitted");
        // Without a cursor the whole chat is there.
        assert_eq!(
            f.st.list_messages_bounded(f.chat.id, None, 10, 100)
                .unwrap()
                .0
                .len(),
            6
        );
        assert_eq!(f.st.list_messages(f.chat.id).unwrap().len(), 6);
        // The bound still applies after the cursor.
        let (two, omitted) =
            f.st.list_messages_bounded_after(f.chat.id, Some(s.cursor()), None, 2, 100)
                .unwrap();
        assert_eq!(two.len(), 2);
        assert!(omitted);
        assert_eq!(
            f.st.dialogue_size_after(f.chat.id, Some(s.cursor()), None)
                .unwrap(),
            (4, 8)
        );
        let after =
            f.st.list_dialogue_after(f.chat.id, Some(s.cursor()), 2)
                .unwrap();
        assert_eq!(after.len(), 2);
        assert_eq!(after[0].content, "m3");
    }

    #[test]
    fn any_supersede_or_restore_in_the_chat_drops_its_summary() {
        let f = fx();
        let ids = dialogue(&f, 6);
        // A message after the cursor, the one on it, one before it, a restore.
        for (range, superseded) in [(4..6, true), (2..3, true), (0..1, true), (4..6, false)] {
            f.st.put_chat_summary(&summary_of(&f, &ids, 2)).unwrap();
            f.st.set_messages_superseded(&ids[range], superseded)
                .unwrap();
            assert!(f.st.get_chat_summary(f.chat.id).unwrap().is_none());
        }
        // Another chat's edit leaves it alone.
        f.st.put_chat_summary(&summary_of(&f, &ids, 2)).unwrap();
        f.st.set_messages_superseded(&[Uuid::from_u128(999)], true)
            .unwrap();
        assert!(f.st.get_chat_summary(f.chat.id).unwrap().is_some());
    }

    #[test]
    fn the_cursor_orders_the_same_in_sql_and_in_rust() {
        let f = fx();
        // Random ids, many sharing a millisecond: SQL sorts the id as text, Rust as bytes.
        let scattered = |i: u64| {
            // splitmix64, twice: ids spread over every byte.
            let mix = |mut z: u64| {
                z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
                z ^ (z >> 31)
            };
            let seed = i.wrapping_mul(0x9e37_79b9_7f4a_7c15);
            Uuid::from_u128((u128::from(mix(seed)) << 64) | u128::from(mix(!seed)))
        };
        let mut msgs: Vec<(i64, Uuid)> = (0..200)
            .map(|i| {
                let id = scattered(u64::try_from(i).unwrap());
                f.st.insert_message(&Message {
                    id,
                    user_id: f.user.id,
                    chat_id: f.chat.id,
                    run_id: None,
                    role: "user".into(),
                    content: "x".into(),
                    created_at_ms: i / 50,
                    exposure_untrusted: false,
                    exposure_private: false,
                    exposure_sources: Vec::new(),
                })
                .unwrap();
                (i / 50, id)
            })
            .collect();
        msgs.sort();
        let listed: Vec<(i64, Uuid)> =
            f.st.list_dialogue_after(f.chat.id, None, 1000)
                .unwrap()
                .iter()
                .map(|m| (m.created_at_ms, m.id))
                .collect();
        assert_eq!(listed, msgs);
        for at in [0, 37, 49, 50, 120, 199] {
            let after: Vec<(i64, Uuid)> =
                f.st.list_dialogue_after(f.chat.id, Some(msgs[at]), 1000)
                    .unwrap()
                    .iter()
                    .map(|m| (m.created_at_ms, m.id))
                    .collect();
            assert_eq!(after, msgs[at + 1..]);
            assert!(after.iter().all(|m| crate::sorts_after(*m, msgs[at])));
            assert_eq!(
                f.st.dialogue_size_after(f.chat.id, Some(msgs[at]), None)
                    .unwrap()
                    .0,
                199 - at
            );
        }
    }

    #[test]
    fn deleting_a_chat_deletes_its_summary_and_a_summary_needs_a_chat() {
        let f = fx();
        let ids = dialogue(&f, 4);
        let s = summary_of(&f, &ids, 1);
        f.st.put_chat_summary(&s).unwrap();
        assert!(f.st.delete_chat(f.chat.id).unwrap());
        assert!(f.st.get_chat_summary(f.chat.id).unwrap().is_none());
        assert!(f.st.put_chat_summary(&s).is_err());
    }

    #[test]
    fn superseded_messages_stay_stored_but_leave_every_context_read() {
        let f = fx();
        let mut ids = Vec::new();
        for i in 0..3u32 {
            let id = Uuid::from_u128(u128::from(i) + 1);
            ids.push(id);
            f.st.insert_message(&Message {
                id,
                user_id: f.user.id,
                chat_id: f.chat.id,
                run_id: None,
                role: "user".into(),
                content: format!("m{i}"),
                created_at_ms: i64::from(i),
                exposure_untrusted: true,
                exposure_private: false,
                exposure_sources: Vec::new(),
            })
            .unwrap();
            f.st.set_message_source(id, "owner").unwrap();
        }
        f.st.set_messages_superseded(&ids[1..], true).unwrap();
        assert_eq!(f.st.list_messages(f.chat.id).unwrap().len(), 1);
        let (bounded, _) =
            f.st.list_messages_bounded(f.chat.id, None, 10, 100)
                .unwrap();
        assert_eq!(bounded.len(), 1);
        assert_eq!(f.st.owner_message_texts(f.chat.id).unwrap(), ["m0"]);
        assert_eq!(
            f.st.list_messages_with_superseded(f.chat.id).unwrap().len(),
            3
        );
        assert_eq!(f.st.superseded_message_ids(f.chat.id).unwrap().len(), 2);
        f.st.set_messages_superseded(&ids[1..], false).unwrap();
        assert_eq!(f.st.list_messages(f.chat.id).unwrap().len(), 3);
    }

    #[test]
    fn chat_exposure_until_sees_old_tainted_messages_the_bounded_window_drops() {
        let f = fx();
        let r = run(&f);
        for i in 0..300u32 {
            f.st.insert_message(&Message {
                id: Uuid::from_u128(u128::from(i) + 1),
                user_id: f.user.id,
                chat_id: f.chat.id,
                run_id: None,
                role: "user".into(),
                content: "m".into(),
                created_at_ms: i64::from(i),
                exposure_untrusted: i == 0,
                exposure_private: false,
                exposure_sources: if i == 0 {
                    vec!["web".into()]
                } else {
                    Vec::new()
                },
            })
            .unwrap();
        }
        let (window, omitted) =
            f.st.list_messages_bounded(f.chat.id, Some(r.id), 256, usize::MAX)
                .unwrap();
        assert!(omitted && !window.iter().any(|m| m.exposure_untrusted));
        let (untrusted, private, sources) =
            f.st.chat_exposure_until(f.chat.id, r.id, 1000).unwrap();
        assert!(untrusted && !private);
        assert_eq!(sources, vec!["web".to_string()]);
    }

    #[test]
    fn chat_exposure_until_ignores_superseded_messages() {
        let f = fx();
        let r = run(&f);
        let id = Uuid::from_u128(77);
        f.st.insert_message(&Message {
            id,
            user_id: f.user.id,
            chat_id: f.chat.id,
            run_id: None,
            role: "user".into(),
            content: "m".into(),
            created_at_ms: 1,
            exposure_untrusted: true,
            exposure_private: true,
            exposure_sources: vec!["web".into()],
        })
        .unwrap();
        assert!(f.st.chat_exposure_until(f.chat.id, r.id, 1000).unwrap().0);
        f.st.set_messages_superseded(&[id], true).unwrap();
        let (u, p, src) = f.st.chat_exposure_until(f.chat.id, r.id, 1000).unwrap();
        assert!(!u && !p && src.is_empty());
    }

    #[test]
    fn bounded_messages_keep_latest_rows_and_filter_run_before_limiting() {
        let f = fx();
        let excluded = run(&f);
        for i in 0..10u32 {
            f.st.insert_message(&Message {
                id: Uuid::from_u128(u128::from(i) + 1),
                user_id: f.user.id,
                chat_id: f.chat.id,
                run_id: (i >= 6).then_some(excluded.id),
                role: "user".into(),
                content: format!("message {i}"),
                // Equal timestamps exercise the deterministic id tie-break.
                created_at_ms: 10,
                exposure_untrusted: false,
                exposure_private: false,
                exposure_sources: Vec::new(),
            })
            .unwrap();
        }
        let (messages, omitted) =
            f.st.list_messages_bounded(f.chat.id, Some(excluded.id), 3, 100)
                .unwrap();
        assert!(omitted);
        assert_eq!(
            messages
                .iter()
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>(),
            ["message 3", "message 4", "message 5"]
        );
        let (all, omitted) =
            f.st.list_messages_bounded(f.chat.id, Some(excluded.id), 6, 100)
                .unwrap();
        assert_eq!(all.len(), 6);
        assert!(!omitted, "exact row boundary is not truncation");
        assert_eq!(
            f.st.list_messages_bounded(ChatId::new(), None, 3, 100)
                .unwrap(),
            (vec![], false)
        );
    }

    #[test]
    fn bounded_messages_count_utf8_bytes_without_splitting_text() {
        let f = fx();
        for i in 0..3u32 {
            f.st.insert_message(&Message {
                id: Uuid::from_u128(u128::from(i) + 1),
                user_id: f.user.id,
                chat_id: f.chat.id,
                run_id: None,
                role: "user".into(),
                content: "é🙂".into(),
                created_at_ms: i64::from(i),
                exposure_untrusted: false,
                exposure_private: false,
                exposure_sources: Vec::new(),
            })
            .unwrap();
        }
        let (messages, omitted) = f.st.list_messages_bounded(f.chat.id, None, 10, 12).unwrap();
        assert!(omitted);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages.iter().map(|m| m.content.len()).sum::<usize>(), 12);
        assert!(messages.iter().all(|m| m.content == "é🙂"));
        assert_eq!(
            f.st.list_messages_bounded(f.chat.id, None, 10, 5).unwrap(),
            (vec![], true)
        );
        assert_eq!(
            f.st.list_messages_bounded(f.chat.id, None, 0, 100).unwrap(),
            (vec![], true)
        );
        assert_eq!(
            f.st.list_messages_bounded(f.chat.id, None, 10, 0).unwrap(),
            (vec![], true)
        );
    }

    #[test]
    fn deleting_a_chat_keeps_its_finished_runs_and_refuses_active_ones() {
        let f = fx();
        let run_id = run(&f).id;
        f.st.update_chat(f.chat.id, Some("Renamed"), Some(true), None)
            .unwrap();
        assert_eq!(f.st.get_chat(f.chat.id).unwrap().unwrap().title, "Renamed");
        assert_eq!(
            f.st.chat_flags(f.chat.id).unwrap(),
            ChatFlags {
                pinned: true,
                archived: false
            }
        );
        // the fixture run is queued: active, so the delete is refused
        assert!(matches!(
            f.st.delete_chat(f.chat.id),
            Err(StorageError::Conflict(_))
        ));
        f.st.update_run_state(run_id, "succeeded", None).unwrap();
        assert!(f.st.delete_chat(f.chat.id).unwrap());
        assert!(f.st.get_chat(f.chat.id).unwrap().is_none());
        assert!(f.st.get_run(run_id).unwrap().unwrap().chat_id.is_none());
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
    fn patch_bot_is_atomic_and_versioned() {
        let f = fx();
        let v0 = f.st.bot_version(f.bot.id).unwrap().unwrap();
        // A plain replace bumps the version too, so a patch built on the old one is stale.
        assert!(matches!(
            f.st.update_bot(f.bot.id, "n", &json!({"a": 1}), v0)
                .unwrap(),
            BotPatch::Applied { .. }
        ));
        let v1 = f.st.bot_version(f.bot.id).unwrap().unwrap();
        assert_eq!(v1, v0 + 1);
        match f
            .st
            .patch_bot(f.bot.id, v0, |_, _| Ok(("x".into(), json!({}))))
            .unwrap()
        {
            BotPatch::Stale { bot, version } => {
                assert_eq!(version, v1);
                assert_eq!(bot.name, "n");
            }
            other => panic!("expected stale, got {other:?}"),
        }
        assert_eq!(f.st.get_bot(f.bot.id).unwrap().unwrap().name, "n");
        match f
            .st
            .patch_bot(f.bot.id, v1, |name, cfg| {
                let mut c = cfg.clone();
                c["b"] = json!(2);
                Ok((format!("{name}!"), c))
            })
            .unwrap()
        {
            BotPatch::Applied { bot, version } => {
                assert_eq!(version, v1 + 1);
                assert_eq!(
                    (bot.name.as_str(), bot.config),
                    ("n!", json!({"a": 1, "b": 2}))
                );
            }
            other => panic!("expected applied, got {other:?}"),
        }
        // A refusal writes nothing and keeps the version.
        assert_eq!(
            f.st.patch_bot(f.bot.id, v1 + 1, |_, _| Err("no".into()))
                .unwrap(),
            BotPatch::Rejected("no".into())
        );
        assert_eq!(f.st.bot_version(f.bot.id).unwrap(), Some(v1 + 1));
        assert_eq!(
            f.st.patch_bot(BotId::new(), 1, |_, _| Err("x".into()))
                .unwrap(),
            BotPatch::Missing
        );
        assert_eq!(f.st.bot_versions(f.user.id).unwrap()[&f.bot.id], v1 + 1);
    }

    #[test]
    fn review95_all_profile_and_skill_writers_bump_one_bot_revision_transactionally() {
        let f = fx();
        let bid = f.bot.id.to_string();
        let assert_revision = |expected| {
            assert_eq!(
                f.st.get_bot_versioned(f.bot.id).unwrap().unwrap().1,
                expected
            );
        };
        assert_revision(1);
        f.st.set_bot_skill_assignment(f.bot.id, SkillAssignmentMode::Selected, &["one".into()])
            .unwrap();
        assert_revision(2);
        f.st.set_bot_skill_assignment(f.bot.id, SkillAssignmentMode::Selected, &["two".into()])
            .unwrap();
        assert_revision(3);
        f.st.set_bot_skill_assignment(f.bot.id, SkillAssignmentMode::All, &[])
            .unwrap();
        assert_revision(4);
        f.st.set_bot_skill_assignment(f.bot.id, SkillAssignmentMode::Selected, &[])
            .unwrap();
        assert_revision(5);
        f.st.delete_bot_skill_assignment(f.bot.id).unwrap();
        assert_revision(6);
        assert!(!f.st.delete_bot_skill_assignment(f.bot.id).unwrap());
        assert_revision(6); // No changed assignment, no revision.
        {
            let mut conn = f.st.conn();
            let tx = conn.transaction().unwrap();
            tx.execute("UPDATE bots SET config_json='{}' WHERE id=?1", [&bid])
                .unwrap();
            assert_eq!(
                tx.query_row("SELECT version FROM bots WHERE id=?1", [&bid], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                7
            );
            // Rollback includes both the profile and its revision.
        }
        assert_revision(6);
        f.st.conn()
            .execute("UPDATE bots SET name='sql writer' WHERE id=?1", [&bid])
            .unwrap();
        assert_revision(7);
        assert!(matches!(
            f.st.update_bot(f.bot.id, "stale", &json!({}), 6).unwrap(),
            BotPatch::Stale { version: 7, .. }
        ));
        assert_revision(7);
        let list = f.st.list_bots_versioned(f.user.id).unwrap();
        assert_eq!(list[0].1, 7);
        assert_eq!(list[0].0.name, "sql writer");
    }

    #[test]
    fn review95_modify_bot_changes_one_field_of_the_stored_profile_and_bumps_the_revision() {
        let f = fx();
        f.st.update_bot(f.bot.id, "n", &json!({"a": 1, "b": 2}), 1)
            .unwrap();
        // A concurrent profile write lands before the dedicated writer runs.
        let out =
            f.st.modify_bot(f.bot.id, |name, stored| {
                let mut next = stored.clone();
                next["knowledge"] = json!({"auto_retrieve": true});
                Ok((name.to_owned(), next))
            })
            .unwrap();
        assert!(matches!(out, BotPatch::Applied { version: 3, .. }));
        let (bot, v) = f.st.get_bot_versioned(f.bot.id).unwrap().unwrap();
        assert_eq!(v, 3);
        assert_eq!(
            bot.config,
            json!({"a": 1, "b": 2, "knowledge": {"auto_retrieve": true}})
        );
    }

    #[test]
    fn update_and_delete_bot() {
        let f = fx();
        assert!(matches!(
            f.st.update_bot(f.bot.id, "renamed", &json!({"a": 1}), 1)
                .unwrap(),
            BotPatch::Applied { .. }
        ));
        let got = f.st.get_bot(f.bot.id).unwrap().unwrap();
        assert_eq!(
            (got.name.as_str(), got.config),
            ("renamed", json!({"a": 1}))
        );
        assert_eq!(
            f.st.update_bot(BotId::new(), "x", &json!({}), 1).unwrap(),
            BotPatch::Missing
        );
        // The fixture chat still references the bot.
        assert!(f.st.delete_bot(f.bot.id).is_err());
        let spare = Bot {
            id: BotId::new(),
            user_id: f.user.id,
            name: "spare".into(),
            config: json!({}),
            created_at_ms: 3,
        };
        f.st.insert_bot(&spare).unwrap();
        assert!(f.st.delete_bot(spare.id).unwrap());
        assert!(!f.st.delete_bot(spare.id).unwrap());
    }

    #[test]
    fn bot_cleanup_happens_after_constraints_and_before_commit() {
        let f = fx();
        assert!(
            f.st.delete_bot_with_cleanup(f.bot.id, || panic!(
                "referenced bot must never be cleaned"
            ))
            .is_err()
        );
        let empty = Bot {
            id: BotId::new(),
            user_id: f.user.id,
            name: "Empty".into(),
            config: json!({}),
            created_at_ms: 1,
        };
        f.st.insert_bot(&empty).unwrap();
        assert!(
            f.st.delete_bot_with_cleanup(empty.id, || Err(crate::StorageError::Io(
                std::io::Error::other("cleanup failed")
            )))
            .is_err()
        );
        assert!(f.st.get_bot(empty.id).unwrap().is_some());
        let mut cleaned = false;
        assert!(
            f.st.delete_bot_with_cleanup(empty.id, || {
                cleaned = true;
                Ok(())
            })
            .unwrap()
        );
        assert!(cleaned);
        assert!(f.st.get_bot(empty.id).unwrap().is_none());
    }

    #[test]
    fn child_runs_persist_parent_and_spec() {
        let f = fx();
        let parent = run(&f);
        let mut child = run(&f);
        // `run` already inserted a copy; build a second, linked one.
        child.id = RunId::new();
        child.parent_run_id = Some(parent.id);
        child.spec = Some(serde_json::json!({ "task": "t", "depth": 1 }));
        f.st.insert_run(&child).unwrap();
        assert_eq!(f.st.get_run(child.id).unwrap().unwrap(), child);
        assert_eq!(f.st.list_child_runs(parent.id).unwrap(), vec![child]);
        assert!(
            f.st.get_run(parent.id)
                .unwrap()
                .unwrap()
                .parent_run_id
                .is_none()
        );
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
    fn knowledge_steps_enforce_owner_row_and_aggregate_byte_bounds() {
        let f = fx();
        let run = run(&f);
        for index in 0..3 {
            f.st.insert_step(&RunStep {
                id: Uuid::now_v7(),
                run_id: run.id,
                user_id: f.user.id,
                idx: index,
                name: "tools".into(),
                input: json!({}),
                idempotent: true,
                retry: None,
                status: "succeeded".into(),
                attempt: 1,
                output: Some(json!(if index == 2 {
                    "x".repeat(1024)
                } else {
                    "small".into()
                })),
                error: None,
                started_at_ms: i64::from(index),
                finished_at_ms: Some(4),
            })
            .unwrap();
        }
        let found =
            f.st.knowledge_tool_steps(f.user.id, f.bot.id, Some(run.id), 3, 40)
                .unwrap();
        assert_eq!(
            found.len(),
            2,
            "oversized latest row must not consume the budget"
        );
        assert_eq!(
            f.st.knowledge_tool_steps(f.user.id, f.bot.id, Some(run.id), 3, 20)
                .unwrap()
                .len(),
            1
        );
        assert!(
            f.st.knowledge_tool_steps(f.user.id, f.bot.id, Some(run.id), 1, 40)
                .unwrap()
                .is_empty(),
            "row cap applies before filtering oversized payloads"
        );
        for (user, bot, run) in [
            (UserId::new(), f.bot.id, run.id),
            (f.user.id, BotId::new(), run.id),
            (f.user.id, f.bot.id, RunId::new()),
        ] {
            assert_eq!(
                f.st.knowledge_tool_steps(user, bot, Some(run), 3, 2048)
                    .unwrap()
                    .len(),
                0
            );
        }
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
    fn event_index_orders_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (RunId::new(), RunId::new());
        {
            let st = Storage::open(dir.path()).unwrap();
            assert_eq!(st.last_gseq().unwrap(), 0);
            st.index_event(1, a, 0).unwrap();
            st.index_event(2, b, 0).unwrap();
            st.index_event(3, a, 1).unwrap();
            assert!(st.index_event(3, b, 1).is_err(), "gseq is never reused");
        }
        let st = Storage::open(dir.path()).unwrap();
        assert_eq!(st.last_gseq().unwrap(), 3);
        let rows = st.list_event_index(1, 10).unwrap();
        let got: Vec<_> = rows.iter().map(|r| (r.gseq, r.run_id, r.seq)).collect();
        assert_eq!(got, [(2, b, 0), (3, a, 1)]);
        assert_eq!(st.list_event_index(0, 2).unwrap().len(), 2);
        assert_eq!(st.list_event_index(3, 10).unwrap(), vec![]);
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

    #[test]
    fn untrusted_file_marks_cover_the_file_or_a_whole_tree_and_survive_a_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let st = Storage::open(dir.path()).unwrap();
        st.mark_untrusted_path("c:/docs/page.txt", false, "write")
            .unwrap();
        st.mark_untrusted_path("c:/work/clone", true, "git clone")
            .unwrap();
        assert!(st.is_untrusted_path("c:/docs/page.txt").unwrap());
        assert!(!st.is_untrusted_path("c:/docs/own.txt").unwrap());
        assert!(
            !st.is_untrusted_path("c:/docs/page.txt/x").unwrap(),
            "a file mark is not a tree"
        );
        assert!(st.is_untrusted_path("c:/work/clone/src/readme.md").unwrap());
        assert!(!st.is_untrusted_path("c:/work/clone2/readme.md").unwrap());
        drop(st);
        let st = Storage::open(dir.path()).unwrap();
        assert!(st.is_untrusted_path("c:/work/clone/a/b").unwrap());
    }

    #[test]
    fn bot_skill_assignment_migration_and_crud() {
        let dir = tempfile::tempdir().unwrap();
        let user = User {
            id: UserId::new(),
            name: "u".into(),
            created_at_ms: 1,
        };
        let existing_bot = Bot {
            id: BotId::new(),
            user_id: user.id,
            name: "preexisting".into(),
            config: json!({}),
            created_at_ms: 2,
        };
        // Simulate a pre-existing v3 database with an existing bot.
        {
            let conn = Connection::open(dir.path().join("bot.db")).unwrap();
            conn.execute_batch(include_str!("../migrations/0001_init.sql"))
                .unwrap();
            conn.execute_batch(include_str!("../migrations/0002_event_index.sql"))
                .unwrap();
            conn.execute_batch(include_str!("../migrations/0003_subagents.sql"))
                .unwrap();
            conn.execute_batch("PRAGMA user_version = 3").unwrap();
            conn.execute(
                "INSERT INTO users (id, name, created_at_ms) VALUES (?1, ?2, ?3)",
                params![user.id.to_string(), user.name, user.created_at_ms],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO bots (id, user_id, name, config_json, created_at_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    existing_bot.id.to_string(),
                    existing_bot.user_id.to_string(),
                    existing_bot.name,
                    "{}",
                    existing_bot.created_at_ms
                ],
            )
            .unwrap();
        }
        let st = Storage::open(dir.path()).unwrap();
        assert_eq!(
            migrate::current_version(&st.conn()).unwrap(),
            migrate::latest_version()
        );
        // Existing bot has no row => defaults to all.
        assert_eq!(st.get_bot_skill_assignment(existing_bot.id).unwrap(), None);

        // Set selected (deduplicates and sorts).
        let saved = st
            .set_bot_skill_assignment_audited(
                user.id,
                existing_bot.id,
                SkillAssignmentMode::Selected,
                &["zeta".into(), "alpha".into(), "zeta".into()],
                "user",
                &json!({"bot_id": existing_bot.id.to_string(), "mode": "selected"}),
            )
            .unwrap();
        assert_eq!(saved.mode, SkillAssignmentMode::Selected);
        assert_eq!(saved.names, vec!["alpha", "zeta"]);
        assert_eq!(
            st.get_bot_skill_assignment(existing_bot.id)
                .unwrap()
                .unwrap()
                .names,
            vec!["alpha", "zeta"]
        );
        assert_eq!(st.list_audit(user.id).unwrap().len(), 1);

        // If the bot has a chat, delete_bot fails and rolls back the skill-assignment delete too.
        let chat = Chat {
            id: ChatId::new(),
            user_id: user.id,
            bot_id: existing_bot.id,
            title: "c".into(),
            created_at_ms: 3,
        };
        st.insert_chat(&chat).unwrap();
        assert!(st.delete_bot(existing_bot.id).is_err());
        assert!(
            st.get_bot_skill_assignment(existing_bot.id)
                .unwrap()
                .is_some()
        );

        // Setting mode = All deletes the row.
        st.set_bot_skill_assignment(existing_bot.id, SkillAssignmentMode::All, &[])
            .unwrap();
        assert_eq!(st.get_bot_skill_assignment(existing_bot.id).unwrap(), None);

        // Deleting a bot with a skill assignment removes the assignment row atomically.
        let spare = Bot {
            id: BotId::new(),
            user_id: user.id,
            name: "spare".into(),
            config: json!({}),
            created_at_ms: 4,
        };
        st.insert_bot(&spare).unwrap();
        st.set_bot_skill_assignment(spare.id, SkillAssignmentMode::Selected, &["alpha".into()])
            .unwrap();
        assert!(st.delete_bot(spare.id).unwrap());
        assert_eq!(st.get_bot_skill_assignment(spare.id).unwrap(), None);
        assert!(!st.delete_bot_skill_assignment(spare.id).unwrap());
    }

    #[test]
    fn bot_skill_assignment_generation_tracks_db_writes_and_version_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let st = Storage::open(dir.path()).unwrap();
        let user = User {
            id: UserId::new(),
            name: "u".into(),
            created_at_ms: 1,
        };
        let bot = Bot {
            id: BotId::new(),
            user_id: user.id,
            name: "b".into(),
            config: json!({}),
            created_at_ms: 2,
        };
        st.insert_user(&user).unwrap();
        st.insert_bot(&bot).unwrap();

        let g0 = st.bot_skill_assignment_generation().unwrap();
        assert_eq!(g0, 1);

        // First write from expected_version = 0 (no row) succeeds and bumps DB generation.
        let r1 = st
            .set_bot_skill_assignment_audited_versioned(
                user.id,
                bot.id,
                SkillAssignmentMode::Selected,
                &["alpha".into()],
                "user",
                &json!({}),
                Some(0),
            )
            .unwrap();
        let g1 = st.bot_skill_assignment_generation().unwrap();
        assert_eq!(r1.version, g1);
        assert!(g1 > g0);

        // Stale expected_version (0 instead of r1.version) returns Conflict without modifying row.
        let err = st
            .set_bot_skill_assignment_audited_versioned(
                user.id,
                bot.id,
                SkillAssignmentMode::Selected,
                &["beta".into()],
                "user",
                &json!({}),
                Some(0),
            )
            .unwrap_err();
        assert!(matches!(err, StorageError::Conflict(_)), "{err:?}");
        assert_eq!(
            st.get_bot_skill_assignment(bot.id).unwrap().unwrap().names,
            vec!["alpha"]
        );

        // External SQLite writer on the same DB file bumps generation via trigger.
        let external = Storage::open(dir.path()).unwrap();
        external
            .set_bot_skill_assignment(bot.id, SkillAssignmentMode::Selected, &["gamma".into()])
            .unwrap();
        let g2 = st.bot_skill_assignment_generation().unwrap();
        assert!(g2 > g1);

        // Corrupt row: get_bot_skill_assignment fails, inspect_bot_skill_assignment reports Corrupt
        // with raw version so management UI can overwrite and repair it.
        st.conn()
            .execute(
                "UPDATE bot_skill_assignment SET names_json = 'not-json' WHERE bot_id = ?1",
                [bot.id.to_string()],
            )
            .unwrap();
        assert!(st.get_bot_skill_assignment(bot.id).is_err());
        let inspected = st.inspect_bot_skill_assignment(bot.id).unwrap();
        match inspected {
            SkillAssignmentLookup::Corrupt { version, .. } => {
                assert_eq!(version, g2);
                let repaired = st
                    .set_bot_skill_assignment_audited_versioned(
                        user.id,
                        bot.id,
                        SkillAssignmentMode::All,
                        &[],
                        "user",
                        &json!({"repaired": true}),
                        Some(version),
                    )
                    .unwrap();
                assert_eq!(repaired.version, 0);
            }
            other @ SkillAssignmentLookup::Ok(_) => panic!("expected Corrupt, got {other:?}"),
        }
        assert_eq!(st.get_bot_skill_assignment(bot.id).unwrap(), None);
        assert!(st.bot_skill_assignment_generation().unwrap() > g2);
    }

    #[test]
    fn assignment_audit_diff_uses_current_database_state() {
        let dir = tempfile::tempdir().unwrap();
        let st = Storage::open(dir.path()).unwrap();
        let user = User {
            id: UserId::new(),
            name: "u".into(),
            created_at_ms: 1,
        };
        let bot = Bot {
            id: BotId::new(),
            user_id: user.id,
            name: "b".into(),
            config: json!({}),
            created_at_ms: 1,
        };
        st.insert_user(&user).unwrap();
        st.insert_bot(&bot).unwrap();
        st.set_bot_skill_assignment(bot.id, SkillAssignmentMode::Selected, &["stale".into()])
            .unwrap();
        let external = Storage::open(dir.path()).unwrap();
        external
            .set_bot_skill_assignment(
                bot.id,
                SkillAssignmentMode::Selected,
                &["current".into(), "keep".into()],
            )
            .unwrap();
        st.set_bot_skill_assignment_audited_versioned(
            user.id,
            bot.id,
            SkillAssignmentMode::Selected,
            &["next".into(), "keep".into(), "next".into()],
            "user",
            &json!({"added": ["stale"], "removed": [], "custom": true}),
            None,
        )
        .unwrap();
        let audit = st.list_audit(user.id).unwrap().pop().unwrap();
        assert_eq!(audit.detail["added"], json!(["next"]));
        assert_eq!(audit.detail["removed"], json!(["current"]));
        assert_eq!(audit.detail["custom"], true);
        st.set_bot_skill_assignment_audited_versioned(
            user.id,
            bot.id,
            SkillAssignmentMode::All,
            &[],
            "user",
            &json!({}),
            None,
        )
        .unwrap();
        let audit = st.list_audit(user.id).unwrap().pop().unwrap();
        assert_eq!(audit.detail["added"], json!([]));
        assert_eq!(audit.detail["removed"], json!(["keep", "next"]));
    }

    #[test]
    fn computer_target_and_pause_reason_persist_and_clear() {
        let dir = tempfile::tempdir().unwrap();
        let run_id;
        {
            let st = Storage::open(dir.path()).unwrap();
            let u = User {
                id: UserId::new(),
                name: "u".into(),
                created_at_ms: 1,
            };
            st.insert_user(&u).unwrap();
            let b = Bot {
                id: BotId::new(),
                user_id: u.id,
                name: "b".into(),
                config: json!({}),
                created_at_ms: 1,
            };
            st.insert_bot(&b).unwrap();
            let r = Run {
                id: RunId::new(),
                user_id: u.id,
                bot_id: b.id,
                chat_id: None,
                kind: "turn".into(),
                state: "queued".into(),
                error: None,
                created_at_ms: 2,
                updated_at_ms: 2,
                parent_run_id: None,
                spec: None,
                computer_target: None,
                computer_node: None,
                computer_node_name: None,
                paused_at_ms: None,
                pause_reason: None,
            };
            run_id = r.id;
            st.insert_run(&r).unwrap();
            st.bind_run_computer(run_id, "pc", Some("pc-A"), Some("My PC"))
                .unwrap();
            st.update_run_state(run_id, "paused", None).unwrap();
            st.set_run_pause_reason(run_id, Some("waiting_for_pc"))
                .unwrap();
            assert_eq!(
                st.list_paused_runs_with_reason("waiting_for_pc")
                    .unwrap()
                    .len(),
                1
            );
        }
        let st = Storage::open(dir.path()).unwrap();
        let got = st.get_run(run_id).unwrap().unwrap();
        assert_eq!(got.computer_target.as_deref(), Some("pc"));
        assert_eq!(got.pause_reason.as_deref(), Some("waiting_for_pc"));
        assert_eq!(got.computer_node.as_deref(), Some("pc-A"));
        assert_eq!(got.computer_node_name.as_deref(), Some("My PC"));
        assert!(
            st.bind_run_computer(run_id, "pc", Some("pc-B"), Some("Other PC"))
                .is_err()
        );
        assert_eq!(
            st.get_run(run_id)
                .unwrap()
                .unwrap()
                .computer_node
                .as_deref(),
            Some("pc-A")
        );
        assert!(got.paused_at_ms.is_some());
        st.conn()
            .execute(
                "UPDATE runs SET paused_at_ms = 123 WHERE id = ?1",
                [run_id.to_string()],
            )
            .unwrap();
        st.set_run_computer_target(run_id, Some("pc")).unwrap();
        st.set_run_pause_reason(run_id, Some("waiting_for_pc"))
            .unwrap();
        st.update_run_state(run_id, "paused", None).unwrap();
        assert_eq!(st.get_run(run_id).unwrap().unwrap().paused_at_ms, Some(123));
        st.update_run_state(run_id, "running", None).unwrap();
        let running = st.get_run(run_id).unwrap().unwrap();
        assert_eq!(running.pause_reason, None);
        assert_eq!(running.paused_at_ms, None);
    }

    #[test]
    fn workspace_folder_names_are_unique_across_case_and_unicode_spellings() {
        let f = fx();
        let mut folders = Vec::new();
        // Each pair is one folder on a case-insensitive or normalising file system.
        for name in [
            "Research Bot",
            "RESEARCH BOT",
            "\u{e7}a\u{11f}r\u{131}",
            "\u{c7}A\u{11e}RI",
            "caf\u{e9}",
            "cafe\u{301}",
            "Stra\u{df}e",
            "STRASSE",
        ] {
            let bot = Bot {
                id: BotId::new(),
                user_id: f.user.id,
                name: name.into(),
                config: json!({}),
                created_at_ms: 9,
            };
            f.st.insert_bot(&bot).unwrap();
            let rec = f.st.ensure_bot_workspace(f.user.id, bot.id, name).unwrap();
            folders.push(rec.folder_name);
        }
        let keys: std::collections::HashSet<_> =
            folders.iter().map(|n| folder_fold_key(n)).collect();
        assert_eq!(
            keys.len(),
            folders.len(),
            "folders share a key: {folders:?}"
        );
        assert_eq!(folders[0], "Research Bot");
        assert_ne!(folders[1], "RESEARCH BOT", "case twin kept its own name");
    }
}
