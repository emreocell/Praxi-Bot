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

    /// Memories held for the owner's review ("Needs your OK"), newest first.
    ///
    /// # Errors
    /// SQLite failure.
    pub fn list_pending(&self, user: UserId) -> Result<Vec<Memory>> {
        let conn = self.conn();
        let mut st = conn.prepare(&format!(
            "SELECT {COLS} FROM memories WHERE user_id = ?1 AND pending = 1 AND deleted = 0              ORDER BY created_ms DESC, rowid DESC"
        ))?;
        let rows = st
            .query_map(params![user.0.to_string()], row_to_memory)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows.into_iter().map(|(_, m)| m).collect())
    }

    /// The owner approves a held memory: from now on it can be recalled.  Its
    /// trust is unchanged, so untrusted-origin text is still shown as data.
    /// Journaled (undo puts it back on hold).
    ///
    /// # Errors
    /// `NotFound` if it is missing, forgotten, not the user's, or not held.
    pub fn approve(&self, user: UserId, id: Uuid) -> Result<Memory> {
        self.require_pending(user, id)?;
        self.update(
            user,
            id,
            &MemoryPatch {
                pending: Some(false),
                ..MemoryPatch::default()
            },
            "owner approved",
        )
    }

    /// The owner discards a held memory (it is forgotten, journaled).
    ///
    /// # Errors
    /// `NotFound` if it is missing, forgotten, not the user's, or not held.
    pub fn discard(&self, user: UserId, id: Uuid) -> Result<()> {
        self.require_pending(user, id)?;
        self.delete(user, id, "owner discarded")
    }

    fn require_pending(&self, user: UserId, id: Uuid) -> Result<()> {
        match self.get(id)? {
            Some(m) if m.user_id == user && !m.deleted && m.pending => Ok(()),
            _ => Err(MemoryError::NotFound(format!("held memory {id}"))),
        }
    }

    /// Journal entries of a user, newest first.
    ///
    /// # Errors
    /// SQLite failure.
    pub fn journal(&self, user: UserId, limit: usize) -> Result<Vec<JournalEntry>> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT id, memory_id, user_id, op, before_json, after_json, reason, ts_ms, undone, redacted \
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

    /// Export what the user has as JSON.  Forgotten (soft-deleted) and erased
    /// items are NOT included: a forgotten text must not travel in an export.
    /// Their journal entries are left out as well, because the snapshots in
    /// them carry the text.
    ///
    /// # Errors
    /// SQLite or JSON failure.
    pub fn export_json(&self, user: UserId) -> Result<Value> {
        self.export_json_with(user, false)
    }

    /// Like [`export_json`](Self::export_json); `include_deleted` adds the
    /// forgotten items and their journal (a full local backup, never a share).
    ///
    /// # Errors
    /// SQLite or JSON failure.
    pub fn export_json_with(&self, user: UserId, include_deleted: bool) -> Result<Value> {
        let memories = self.list(
            user,
            &ListFilter {
                include_deleted,
                ..ListFilter::default()
            },
        )?;
        let mut journal = self.journal(user, usize::MAX >> 1)?;
        if !include_deleted {
            let live: std::collections::HashSet<Uuid> = memories.iter().map(|m| m.id).collect();
            journal.retain(|e| live.contains(&e.memory_id));
        }
        Ok(json!({
            "version": 1,
            "exported_ms": now_ms(),
            "user_id": user,
            "include_deleted": include_deleted,
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

    /// Newest active (not yet undone) journal entry id for `memory_id`, if any.
    ///
    /// # Errors
    /// SQLite failure.
    pub fn latest_journal_id(&self, user: UserId, memory_id: Uuid) -> Result<Option<i64>> {
        let conn = self.conn();
        let id: Option<i64> = conn.query_row(
            "SELECT MAX(id) FROM memory_journal WHERE user_id = ?1 AND memory_id = ?2 AND undone = 0",
            params![user.0.to_string(), memory_id.to_string()],
            |r| r.get(0),
        )?;
        Ok(id)
    }

    /// Undo the newest active journal entry for `memory_id`.
    ///
    /// # Errors
    /// `NotFound` if the memory has no active journal entry; `Invalid` if undo fails.
    pub fn undo_memory(&self, user: UserId, memory_id: Uuid) -> Result<Memory> {
        let journal_id = self.latest_journal_id(user, memory_id)?.ok_or_else(|| {
            MemoryError::NotFound(format!("active journal entry for memory {memory_id}"))
        })?;
        self.undo(user, journal_id)
    }
}
