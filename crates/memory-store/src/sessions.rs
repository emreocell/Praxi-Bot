//! Cross-session message search (FTS over past chat messages).

use protocol::{ChatId, UserId};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::Result;
use crate::recall::fts_query;
use crate::store::MemoryStore;

/// One hit of [`MemoryStore::search_sessions`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionHit {
    /// Message id in `bot.db`.
    pub message_id: Uuid,
    /// Chat it belongs to.
    pub chat_id: ChatId,
    /// `user` / `assistant`.
    pub role: String,
    /// Message text (truncated to 500 chars).
    pub content: String,
    /// Message time (ms).
    pub created_ms: i64,
    /// Higher is better (negated BM25).
    pub score: f64,
}

impl MemoryStore {
    /// Index one message for [`search_sessions`](Self::search_sessions).
    /// Only `user` and `assistant` messages are indexed; re-indexing is a no-op.
    /// Returns true if the message was newly indexed.
    ///
    /// # Errors
    /// SQLite failure.
    pub fn index_message(&self, m: &storage::Message) -> Result<bool> {
        if m.role != "user" && m.role != "assistant" {
            return Ok(false);
        }
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let n = tx.execute(
            "INSERT OR IGNORE INTO session_msgs (message_id, user_id, chat_id, role, content, created_ms) \
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                m.id.to_string(),
                m.user_id.0.to_string(),
                m.chat_id.0.to_string(),
                m.role,
                m.content,
                m.created_at_ms
            ],
        )?;
        if n == 1 {
            let rowid = tx.last_insert_rowid();
            tx.execute(
                "INSERT INTO session_fts(rowid, content) VALUES (?1, ?2)",
                params![rowid, m.content],
            )?;
        }
        tx.commit()?;
        Ok(n == 1)
    }

    /// Index all not-yet-indexed messages of a chat from `bot.db`.  Returns
    /// the number newly indexed.
    ///
    /// # Errors
    /// Storage or SQLite failure.
    pub fn index_chat(&self, storage: &storage::Storage, chat: ChatId) -> Result<usize> {
        let mut n = 0;
        for m in storage.list_messages(chat)? {
            if self.index_message(&m)? {
                n += 1;
            }
        }
        Ok(n)
    }

    /// Full-text search over the user's past chat messages (all chats),
    /// best match first.
    ///
    /// # Errors
    /// SQLite failure.
    pub fn search_sessions(
        &self,
        user: UserId,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SessionHit>> {
        let Some(q) = fts_query(query) else {
            return Ok(Vec::new());
        };
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT s.message_id, s.chat_id, s.role, s.content, s.created_ms, bm25(session_fts) \
             FROM session_fts JOIN session_msgs s ON s.rowid = session_fts.rowid \
             WHERE session_fts MATCH ?1 AND s.user_id = ?2 ORDER BY bm25(session_fts) LIMIT ?3",
        )?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = st.query_map(params![q, user.0.to_string(), limit], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, f64>(5)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (mid, cid, role, content, created_ms, bm) = row?;
            let (Ok(message_id), Ok(chat)) = (Uuid::parse_str(&mid), Uuid::parse_str(&cid)) else {
                continue;
            };
            out.push(SessionHit {
                message_id,
                chat_id: ChatId(chat),
                role,
                content: content.chars().take(500).collect(),
                created_ms,
                score: -bm,
            });
        }
        Ok(out)
    }
}
