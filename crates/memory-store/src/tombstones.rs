//! Tombstones (a correction must hold) and permanent erase.
//!
//! Forgetting a memory is a soft delete, so by itself the next extraction that
//! meets the same statement would simply store it again.  A tombstone keeps a
//! fingerprint of the forgotten statement (hash of the normalised words, a set
//! of word hashes and the embedding) so the pipeline can refuse it, without
//! keeping the text.  The only readable part is a short label for the owner's
//! "Forgotten items" list; it is dropped when the item is erased permanently.
//!
//! Permanent erase ([`MemoryStore::purge`]) removes the row, the FTS and vector
//! entries and the text inside journal snapshots, and keeps a text-free
//! tombstone so the statement is not learned again.

use protocol::{BotId, UserId};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::{MemoryError, Result};
use crate::store::{MemoryStore, get_in, journal_insert_flags, now_ms, unindex};
use crate::types::{JournalOp, Kind, Memory, Tombstone};

/// Embedding similarity at which a candidate counts as the same statement.
const NEAR_COSINE: f32 = 0.85;
/// Word-set overlap (Jaccard) that must hold as well: different facts about the
/// same topic ("lives in Paris" / "lives in Berlin") are not near-duplicates.
const NEAR_JACCARD: f64 = 0.7;
/// Characters of the owner-facing label.
const LABEL_CHARS: usize = 80;

fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Hash of the normalised statement (case, punctuation and spacing ignored).
pub(crate) fn fingerprint(text: &str) -> String {
    format!("{:x}", Sha256::digest(words(text).join(" ").as_bytes()))
}

/// Sorted, de-duplicated 8-byte hashes of the words.
fn word_hashes(text: &str) -> Vec<u8> {
    let mut hashes: Vec<[u8; 8]> = words(text)
        .iter()
        .map(|w| {
            let d = Sha256::digest(w.as_bytes());
            let mut h = [0u8; 8];
            h.copy_from_slice(&d[..8]);
            h
        })
        .collect();
    hashes.sort_unstable();
    hashes.dedup();
    hashes.concat()
}

fn jaccard(a: &[u8], b: &[u8]) -> f64 {
    let set = |x: &[u8]| -> std::collections::HashSet<[u8; 8]> {
        x.as_chunks::<8>().0.iter().copied().collect()
    };
    let (a, b) = (set(a), set(b));
    let inter = a.intersection(&b).count();
    let union = a.union(&b).count();
    if union == 0 {
        return 0.0;
    }
    #[allow(clippy::cast_precision_loss)]
    {
        inter as f64 / union as f64
    }
}

fn from_blob(b: &[u8]) -> Vec<f32> {
    b.as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect()
}

fn label_of(content: &str) -> String {
    let flat = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > LABEL_CHARS {
        let cut: String = flat.chars().take(LABEL_CHARS).collect();
        format!("{cut}…")
    } else {
        flat
    }
}

fn scope_key(bot: Option<BotId>) -> String {
    bot.map(|b| b.0.to_string()).unwrap_or_default()
}

/// Insert (replacing a same-statement tombstone of the same scope).
pub(crate) fn write(conn: &Connection, m: &Memory, emb: &[f32], erased: bool) -> Result<()> {
    let fp = fingerprint(&m.content);
    conn.execute(
        "DELETE FROM memory_tombstones WHERE user_id = ?1 AND IFNULL(bot_id, '') = ?2 \
         AND fingerprint = ?3",
        params![m.user_id.0.to_string(), scope_key(m.bot_id), fp],
    )?;
    conn.execute(
        "INSERT INTO memory_tombstones (id, user_id, bot_id, memory_id, kind, fingerprint, \
         tokens, embedding, label, erased, created_ms) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![
            Uuid::now_v7().to_string(),
            m.user_id.0.to_string(),
            m.bot_id.map(|b| b.0.to_string()),
            m.id.to_string(),
            m.kind.as_str(),
            fp,
            word_hashes(&m.content),
            crate::vec::to_blob(emb),
            (!erased).then(|| label_of(&m.content)),
            i64::from(erased),
            now_ms(),
        ],
    )?;
    Ok(())
}

/// Lift the tombstone of exactly this statement (an explicit re-add).
pub(crate) fn clear_matching(
    conn: &Connection,
    user: UserId,
    bot: Option<BotId>,
    content: &str,
) -> Result<()> {
    conn.execute(
        "DELETE FROM memory_tombstones WHERE user_id = ?1 AND IFNULL(bot_id, '') = ?2 \
         AND fingerprint = ?3",
        params![user.0.to_string(), scope_key(bot), fingerprint(content)],
    )?;
    Ok(())
}

/// Lift the tombstone made when `memory_id` was forgotten (undo of the delete).
pub(crate) fn remove_for_memory(conn: &Connection, memory_id: Uuid) -> Result<()> {
    conn.execute(
        "DELETE FROM memory_tombstones WHERE memory_id = ?1",
        [memory_id.to_string()],
    )?;
    Ok(())
}

type RawTombstone = (String, Option<String>, String, Option<String>, i64, i64);

fn tombstone_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<RawTombstone> {
    Ok((
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
    ))
}

impl MemoryStore {
    /// Whether `content` was forgotten by `user` (in a scope that covers
    /// `bot`) or is a near-duplicate of something forgotten.  The extraction
    /// pipeline refuses such candidates.
    ///
    /// # Errors
    /// Embedder or SQLite failure.
    pub fn is_forgotten(&self, user: UserId, bot: BotId, content: &str) -> Result<bool> {
        let emb = self.embed_one(content.trim())?;
        let fp = fingerprint(content);
        let tokens = word_hashes(content);
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT fingerprint, tokens, embedding FROM memory_tombstones \
             WHERE user_id = ?1 AND (bot_id IS NULL OR bot_id = ?2)",
        )?;
        let rows = st.query_map(params![user.0.to_string(), bot.0.to_string()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Vec<u8>>(1)?,
                r.get::<_, Vec<u8>>(2)?,
            ))
        })?;
        for row in rows {
            let (stored_fp, stored_tokens, stored_emb) = row?;
            if stored_fp == fp {
                return Ok(true);
            }
            let stored_emb = from_blob(&stored_emb);
            if stored_emb.len() == emb.len() {
                let cos: f32 = stored_emb.iter().zip(&emb).map(|(a, b)| a * b).sum();
                if cos >= NEAR_COSINE && jaccard(&stored_tokens, &tokens) >= NEAR_JACCARD {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// The statements `user` forgot, newest first.  With `bot`, only those that
    /// apply to that bot (shared ones and the bot's own).
    ///
    /// # Errors
    /// SQLite failure.
    pub fn list_tombstones(&self, user: UserId, bot: Option<BotId>) -> Result<Vec<Tombstone>> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT id, bot_id, kind, label, erased, created_ms FROM memory_tombstones \
             WHERE user_id = ?1 AND (?2 IS NULL OR bot_id IS NULL OR bot_id = ?2) \
             ORDER BY created_ms DESC, rowid DESC",
        )?;
        let raw = st
            .query_map(
                params![user.0.to_string(), bot.map(|b| b.0.to_string())],
                tombstone_row,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        raw.into_iter()
            .map(|(id, bot_id, kind, label, erased, created_ms)| {
                let bad = |what: &str| MemoryError::Invalid(format!("corrupt tombstone {what}"));
                Ok(Tombstone {
                    id: Uuid::parse_str(&id).map_err(|_| bad("id"))?,
                    bot_id: bot_id
                        .map(|b| Uuid::parse_str(&b).map(BotId))
                        .transpose()
                        .map_err(|_| bad("bot"))?,
                    kind: Kind::parse(&kind).ok_or_else(|| bad("kind"))?,
                    label,
                    erased: erased != 0,
                    created_ms,
                })
            })
            .collect()
    }

    /// Let the statement be learned again (the owner changed their mind).
    ///
    /// # Errors
    /// `NotFound` for an unknown or foreign tombstone.
    pub fn remove_tombstone(&self, user: UserId, id: Uuid) -> Result<()> {
        let n = self.conn().execute(
            "DELETE FROM memory_tombstones WHERE id = ?1 AND user_id = ?2",
            params![id.to_string(), user.0.to_string()],
        )?;
        if n == 0 {
            return Err(MemoryError::NotFound(format!("forgotten item {id}")));
        }
        Ok(())
    }

    /// Erase a memory permanently (live or already forgotten): the row, its
    /// search entries and the text inside every journal snapshot are removed;
    /// the journal keeps only operation, time and reason.  A text-free
    /// tombstone stays so the statement is not learned again.
    ///
    /// Backups made earlier still contain the text.
    ///
    /// # Errors
    /// `NotFound` if missing or not owned.
    pub fn purge(&self, user: UserId, id: Uuid, reason: &str) -> Result<()> {
        {
            let mut conn = self.conn();
            let tx = conn.transaction()?;
            let (rowid, memory) = get_in(&tx, id)?
                .filter(|(_, m)| m.user_id == user)
                .ok_or_else(|| MemoryError::NotFound(format!("memory {id}")))?;
            let emb = self.embed_one(&memory.content)?;
            write(&tx, &memory, &emb, true)?;
            unindex(&tx, rowid)?;
            tx.execute(
                "UPDATE memory_journal SET before_json = NULL, after_json = NULL, redacted = 1, \
                 undone = 1 WHERE memory_id = ?1 AND user_id = ?2",
                params![id.to_string(), user.0.to_string()],
            )?;
            tx.execute("DELETE FROM memories WHERE id = ?1", [id.to_string()])?;
            journal_insert_flags(
                &tx,
                &memory,
                JournalOp::Purge,
                None,
                None,
                reason,
                true,
                true,
            )?;
            // Merge the search segments so the text is not kept in old ones.
            tx.execute(
                "INSERT INTO memories_fts(memories_fts) VALUES ('optimize')",
                [],
            )?;
            tx.commit()?;
        }
        self.checkpoint();
        Ok(())
    }

    /// How many memories only `bot` knows (what [`Self::erase_bot`] would remove).
    ///
    /// # Errors
    /// SQLite failure.
    pub fn count_bot(&self, user: UserId, bot: BotId) -> Result<usize> {
        let n: i64 = self.conn().query_row(
            "SELECT COUNT(*) FROM memories WHERE user_id = ?1 AND bot_id = ?2 AND deleted = 0",
            params![user.0.to_string(), bot.0.to_string()],
            |r| r.get(0),
        )?;
        Ok(usize::try_from(n).unwrap_or(0))
    }

    /// Drop everything a deleted bot left in memory: its own memories (with
    /// their journal) and tombstones.  Memories shared across the user's bots
    /// stay: they belong to the user.
    ///
    /// # Errors
    /// SQLite failure.
    pub fn erase_bot(&self, user: UserId, bot: BotId) -> Result<usize> {
        let erased = {
            let mut conn = self.conn();
            let tx = conn.transaction()?;
            let rows: Vec<(i64, String)> = {
                let mut st = tx
                    .prepare("SELECT rowid, id FROM memories WHERE user_id = ?1 AND bot_id = ?2")?;
                st.query_map(params![user.0.to_string(), bot.0.to_string()], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?
                .collect::<rusqlite::Result<_>>()?
            };
            for (rowid, mid) in &rows {
                unindex(&tx, *rowid)?;
                tx.execute("DELETE FROM memory_journal WHERE memory_id = ?1", [mid])?;
                tx.execute("DELETE FROM memories WHERE id = ?1", [mid])?;
            }
            tx.execute(
                "DELETE FROM memory_tombstones WHERE user_id = ?1 AND bot_id = ?2",
                params![user.0.to_string(), bot.0.to_string()],
            )?;
            tx.execute(
                "INSERT INTO memories_fts(memories_fts) VALUES ('optimize')",
                [],
            )?;
            tx.commit()?;
            rows.len()
        };
        self.checkpoint();
        Ok(erased)
    }

    /// Best effort: fold the write-ahead log into the database file so erased
    /// pages are overwritten (a no-op for in-memory databases).
    pub(crate) fn checkpoint(&self) {
        let conn = self.conn();
        let _ = conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
            .optional();
    }
}
