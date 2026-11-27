//! The rolling summary of the older part of a long chat (one row per chat).
//!
//! The summarised messages are never touched: they stay in `messages`, in the search index and
//! in exports. The row only says which prefix of the chat the model context replaces by a
//! condensed note (see `agent_core::summary`), and what that prefix carried (untrusted or
//! private content), so the note inherits it.

use protocol::{ChatId, RunId, UserId};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use crate::db::Storage;
use crate::error::Result;
use crate::records::Message;

/// A chat's rolling summary.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatSummary {
    /// The chat.
    pub chat_id: ChatId,
    /// Owner.
    pub user_id: UserId,
    /// The summary text, as the runtime renders it.
    pub content: String,
    /// Creation time of the newest summarised message.
    pub covered_until_ms: i64,
    /// Id of the newest summarised message; with `covered_until_ms` it is the cursor: the
    /// context holds the messages that sort after it.
    pub covered_until_id: Uuid,
    /// Messages summarised so far, over all rounds.
    pub covered_messages: u32,
    /// How many rounds folded into `content`.
    pub rounds: u32,
    /// Creation time of the oldest summarised message.
    pub first_message_ms: i64,
    /// A summarised message came from a context with untrusted content (or was untrusted).
    pub exposure_untrusted: bool,
    /// A summarised message came from a context with private content.
    pub exposure_private: bool,
    /// Source labels of the summarised messages.
    pub exposure_sources: Vec<String>,
    /// The model that wrote the last round (`provider/model`).
    pub model: String,
    /// First round.
    pub created_at_ms: i64,
    /// Last round.
    pub updated_at_ms: i64,
}

const COLUMNS: &str = "chat_id, user_id, content, covered_until_ms, covered_until_id, covered_messages, rounds, first_message_ms, exposure_untrusted, exposure_private, exposure_sources, model, created_at_ms, updated_at_ms";

fn conv(i: usize, e: impl std::error::Error + Send + Sync + 'static) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, Box::new(e))
}

fn summary_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ChatSummary> {
    let uuid = |i: usize| Uuid::parse_str(&r.get::<_, String>(i)?).map_err(|e| conv(i, e));
    Ok(ChatSummary {
        chat_id: ChatId(uuid(0)?),
        user_id: UserId(uuid(1)?),
        content: r.get(2)?,
        covered_until_ms: r.get(3)?,
        covered_until_id: uuid(4)?,
        covered_messages: u32::try_from(r.get::<_, i64>(5)?).unwrap_or(u32::MAX),
        rounds: u32::try_from(r.get::<_, i64>(6)?).unwrap_or(u32::MAX),
        first_message_ms: r.get(7)?,
        exposure_untrusted: r.get::<_, i64>(8)? != 0,
        exposure_private: r.get::<_, i64>(9)? != 0,
        exposure_sources: serde_json::from_str(&r.get::<_, String>(10)?).unwrap_or_default(),
        model: r.get(11)?,
        created_at_ms: r.get(12)?,
        updated_at_ms: r.get(13)?,
    })
}

/// Where a chat's messages stand: the cursor of a summary, `(created_at_ms, id)`.
pub type Cursor = (i64, Uuid);

impl ChatSummary {
    /// The cursor after which messages are not summarised.
    #[must_use]
    pub fn cursor(&self) -> Cursor {
        (self.covered_until_ms, self.covered_until_id)
    }
}

impl Storage {
    /// The rolling summary of a chat, if it has one.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn get_chat_summary(&self, chat: ChatId) -> Result<Option<ChatSummary>> {
        Ok(self
            .conn()
            .query_row(
                &format!("SELECT {COLUMNS} FROM chat_summaries WHERE chat_id = ?1"),
                [chat.to_string()],
                summary_row,
            )
            .optional()?)
    }

    /// Store a chat's summary (replaces the previous round).
    ///
    /// # Errors
    /// Fails on SQLite errors, including an unknown chat or user.
    pub fn put_chat_summary(&self, s: &ChatSummary) -> Result<()> {
        self.conn().execute(
            &format!(
                "INSERT INTO chat_summaries ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14) \
                 ON CONFLICT(chat_id) DO UPDATE SET content = excluded.content, \
                 covered_until_ms = excluded.covered_until_ms, covered_until_id = excluded.covered_until_id, \
                 covered_messages = excluded.covered_messages, rounds = excluded.rounds, \
                 first_message_ms = excluded.first_message_ms, exposure_untrusted = excluded.exposure_untrusted, \
                 exposure_private = excluded.exposure_private, exposure_sources = excluded.exposure_sources, \
                 model = excluded.model, updated_at_ms = excluded.updated_at_ms"
            ),
            params![
                s.chat_id.to_string(),
                s.user_id.to_string(),
                s.content,
                s.covered_until_ms,
                s.covered_until_id.to_string(),
                i64::from(s.covered_messages),
                i64::from(s.rounds),
                s.first_message_ms,
                i64::from(s.exposure_untrusted),
                i64::from(s.exposure_private),
                serde_json::to_string(&s.exposure_sources)?,
                s.model,
                s.created_at_ms,
                s.updated_at_ms,
            ],
        )?;
        Ok(())
    }

    /// Drop a chat's summary; the messages are untouched. True when there was one.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn delete_chat_summary(&self, chat: ChatId) -> Result<bool> {
        Ok(self.conn().execute(
            "DELETE FROM chat_summaries WHERE chat_id = ?1",
            [chat.to_string()],
        )? > 0)
    }

    /// Live (not superseded) user and assistant messages of `chat` after `after`, oldest first,
    /// at most `limit` of them. `after = None`: from the start of the chat.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn list_dialogue_after(
        &self,
        chat: ChatId,
        after: Option<Cursor>,
        limit: usize,
    ) -> Result<Vec<Message>> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT id, user_id, chat_id, run_id, role, content, created_at_ms, exposure_untrusted, exposure_private, exposure_sources \
             FROM messages WHERE chat_id = ?1 AND superseded = 0 AND role IN ('user', 'assistant') \
             AND (?2 IS NULL OR created_at_ms > ?2 OR (created_at_ms = ?2 AND id > ?3)) \
             ORDER BY created_at_ms, id LIMIT ?4",
        )?;
        let rows = st.query_map(
            params![
                chat.to_string(),
                after.map(|c| c.0),
                after.map(|c| c.1.to_string()),
                i64::try_from(limit).unwrap_or(i64::MAX),
            ],
            crate::db::message_row,
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// How many live user and assistant messages of `chat` sort after `after`, and their
    /// bytes. `exclude_run`'s own messages are left out, like in the context.
    ///
    /// # Errors
    /// Fails on SQLite errors.
    pub fn dialogue_size_after(
        &self,
        chat: ChatId,
        after: Option<Cursor>,
        exclude_run: Option<RunId>,
    ) -> Result<(usize, usize)> {
        let (n, bytes): (i64, i64) = self.conn().query_row(
            "SELECT count(*), coalesce(sum(length(CAST(content AS BLOB))), 0) FROM messages \
             WHERE chat_id = ?1 AND superseded = 0 AND role IN ('user', 'assistant') \
             AND (?2 IS NULL OR created_at_ms > ?2 OR (created_at_ms = ?2 AND id > ?3)) \
             AND (?4 IS NULL OR run_id IS NULL OR run_id <> ?4)",
            params![
                chat.to_string(),
                after.map(|c| c.0),
                after.map(|c| c.1.to_string()),
                exclude_run.map(|r| r.to_string()),
            ],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok((
            usize::try_from(n).unwrap_or(usize::MAX),
            usize::try_from(bytes).unwrap_or(usize::MAX),
        ))
    }
}
