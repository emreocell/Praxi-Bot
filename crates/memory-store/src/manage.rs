//! Management API (memory panel): list / edit / journal / export, plus lessons.

use protocol::{BotId, ChatId, UserId};
use rusqlite::params;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::error::{MemoryError, Result};
use crate::store::{COLS, MemoryStore, journal_decode, journal_row, now_ms, row_to_memory};
use crate::types::{JournalEntry, Kind, ListFilter, Memory, MemoryPatch, NewMemory, Trust};

impl MemoryStore {
    /// List a user's memories, newest first.
    ///
    /// # Errors
    /// SQLite failure.
    pub fn list(&self, user: UserId, filter: &ListFilter) -> Result<Vec<Memory>> {
        let conn = self.conn();
        let mut st = conn.prepare(&format!(
            "SELECT {COLS} FROM memories WHERE user_id = ?1 \
             AND (?2 IS NULL OR bot_id IS NULL OR bot_id = ?2) \
             AND (?3 IS NULL OR kind = ?3) AND (?4 = 1 OR deleted = 0) \
             ORDER BY created_ms DESC, rowid DESC"
        ))?;
        let rows = st.query_map(
            params![
                user.0.to_string(),
                filter.bot_id.map(|b| b.0.to_string()),
                filter.kind.map(Kind::as_str),
                i64::from(filter.include_deleted),
            ],
            row_to_memory,
        )?;
        Ok(rows
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(|(_, m)| m)
            .collect())
    }

    /// User edit of a memory's text; the edit is user-authored, so trust
    /// becomes `user`.  Journaled.
    ///
    /// # Errors
    /// `NotFound` / `Invalid` as for [`update`](Self::update).
    pub fn edit(&self, user: UserId, id: Uuid, content: &str) -> Result<Memory> {
        self.update(
            user,
            id,
            &MemoryPatch {
                content: Some(content.to_owned()),
                trust: Some(Trust::User),
                ..MemoryPatch::default()
            },
            "user edit",
        )
    }

    /// Journal entries of a user, newest first.
    ///
    /// # Errors
    /// SQLite failure.
    pub fn journal(&self, user: UserId, limit: usize) -> Result<Vec<JournalEntry>> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT id, memory_id, user_id, op, before_json, after_json, reason, ts_ms, undone \
             FROM memory_journal WHERE user_id = ?1 ORDER BY id DESC LIMIT ?2",
        )?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let raw = st
            .query_map(params![user.0.to_string(), limit], journal_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        raw.into_iter()
            .map(|r| journal_decode(r).map(|(_, e)| e))
            .collect()
    }

    /// Export everything the user has (memories incl. soft-deleted, journal)
    /// as JSON.
    ///
    /// # Errors
    /// SQLite or JSON failure.
    pub fn export_json(&self, user: UserId) -> Result<Value> {
        let memories = self.list(
            user,
            &ListFilter {
                include_deleted: true,
                ..ListFilter::default()
            },
        )?;
        let journal = self.journal(user, usize::MAX >> 1)?;
        Ok(json!({
            "version": 1,
            "exported_ms": now_ms(),
            "user_id": user,
            "memories": memories,
            "journal": journal,
        }))
    }

    /// Record a user correction as a bot-scoped `lesson` memory.
    ///
    /// # Errors
    /// `Invalid` if the text is empty or looks like a secret.
    pub fn record_correction(
        &self,
        user: UserId,
        bot: BotId,
        chat: ChatId,
        wrong: &str,
        corrected: &str,
    ) -> Result<Memory> {
        if wrong.trim().is_empty() || corrected.trim().is_empty() {
            return Err(MemoryError::Invalid("correction needs both texts".into()));
        }
        let mut new = NewMemory::new(
            user,
            Some(bot),
            Kind::Lesson,
            format!(
                "Correction: do not \"{}\"; instead \"{}\".",
                wrong.trim(),
                corrected.trim()
            ),
        );
        new.source_quote = Some(corrected.trim().to_owned());
        new.source_chat_id = Some(chat);
        new.importance = 0.8;
        self.add(&new, "user correction")
    }
}
