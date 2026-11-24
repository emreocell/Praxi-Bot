//! "Basic (word match)" versus "Smart (meaning)" recall.
//!
//! The shipped embedder ([`crate::HashEmbedder`]) hashes words into buckets, so
//! the vector half of recall only finds memories that share words with the
//! query: honest name "Basic".  "Smart" swaps in a real embedding model.  Its
//! vectors have another dimension, so the vector table is rebuilt and every
//! live memory is embedded again, in batches, with a progress state.  Recall
//! stays lexical (words only) until the new index is complete, and any failure
//! falls back to Basic instead of leaving a half-built index.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::embed::{Embedder, HashEmbedder, normalize};
use crate::error::{MemoryError, Result};
use crate::store::{MemoryStore, meta_get, meta_set};
use crate::vec::to_blob;

pub(crate) const KEY_MODE: &str = "embed_mode";
pub(crate) const KEY_LABEL: &str = "embed_label";
pub(crate) const KEY_STATE: &str = "embed_state";
pub(crate) const KEY_DONE: &str = "embed_done";
pub(crate) const KEY_TOTAL: &str = "embed_total";
pub(crate) const KEY_ERROR: &str = "embed_error";
pub(crate) const KEY_DIRTY: &str = "vec_dirty";
pub(crate) const KEY_SOURCE: &str = "embed_source";

/// Dimension of the built-in Basic embedder.
pub const BASIC_DIM: usize = 384;
const BATCH: usize = 32;

/// How recall measures similarity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EmbedMode {
    /// Word match: no model, nothing leaves the computer.
    Basic,
    /// Meaning: an embedding model chosen by the owner.
    Smart,
}

/// Where the vector index stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EmbedState {
    /// Index complete and in use.
    Ready,
    /// Memories are being embedded again; recall uses words only meanwhile.
    Rebuilding,
    /// A Smart index is waiting for its embedder (e.g. after a restart).
    Waiting,
    /// The last switch failed; the store fell back to Basic.
    Failed,
}

/// Snapshot for the settings screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingStatus {
    /// Selected mode.
    pub mode: EmbedMode,
    /// Human label of the model ("Basic", or the Smart model name).
    pub label: String,
    /// Index state.
    pub state: EmbedState,
    /// Memories embedded so far while rebuilding.
    pub done: usize,
    /// Memories to embed in this rebuild.
    pub total: usize,
    /// Short reason of the last failed switch.
    pub error: Option<String>,
}

fn usize_meta(conn: &Connection, key: &str) -> Result<usize> {
    Ok(meta_get(conn, key)?
        .and_then(|v| v.parse().ok())
        .unwrap_or(0))
}

impl MemoryStore {
    /// Current mode, label and rebuild progress.
    ///
    /// # Errors
    /// SQLite failure.
    pub fn embedding_status(&self) -> Result<EmbeddingStatus> {
        let conn = self.conn();
        let mode = match meta_get(&conn, KEY_MODE)?.as_deref() {
            Some("smart") => EmbedMode::Smart,
            _ => EmbedMode::Basic,
        };
        let state = match meta_get(&conn, KEY_STATE)?.as_deref() {
            Some("rebuilding") => EmbedState::Rebuilding,
            Some("failed") => EmbedState::Failed,
            _ if !self.vec_ready.load(Ordering::Acquire) => EmbedState::Waiting,
            _ => EmbedState::Ready,
        };
        Ok(EmbeddingStatus {
            mode,
            label: meta_get(&conn, KEY_LABEL)?.unwrap_or_else(|| "Basic".to_owned()),
            state,
            done: usize_meta(&conn, KEY_DONE)?,
            total: usize_meta(&conn, KEY_TOTAL)?,
            error: meta_get(&conn, KEY_ERROR)?.filter(|e| !e.is_empty()),
        })
    }

    /// How many live (not forgotten) memories the user has.
    ///
    /// # Errors
    /// SQLite failure.
    pub fn count_live(&self, user: protocol::UserId) -> Result<usize> {
        let conn = self.conn();
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM memories WHERE user_id = ?1 AND deleted = 0",
            [user.0.to_string()],
            |r| r.get(0),
        )?;
        Ok(usize::try_from(n).unwrap_or(0))
    }

    /// The owner's saved choice of Smart model source (opaque JSON written by
    /// the daemon), so a restart can bring the model back.
    #[must_use]
    pub fn source_note(&self) -> Option<String> {
        let conn = self.conn();
        meta_get(&conn, KEY_SOURCE)
            .ok()
            .flatten()
            .filter(|v| !v.is_empty())
    }

    /// Save or clear the Smart model source note.
    pub fn set_source_note(&self, note: Option<&str>) {
        let conn = self.conn();
        let _ = meta_set(&conn, KEY_SOURCE, note.unwrap_or(""));
    }

    /// Whether recall currently uses the vector index.
    #[must_use]
    pub fn semantic_ready(&self) -> bool {
        self.vec_ready.load(Ordering::Acquire)
    }

    /// Switch to Smart recall with `embedder`.  Blocking and potentially long
    /// (run it off the async runtime): rebuilds the vector table for the new
    /// dimension and embeds every live memory again.  On any failure the store
    /// falls back to Basic and the error is returned.
    ///
    /// # Errors
    /// The embedder failed its probe or a batch; the store is back on Basic.
    pub fn enable_smart(&self, embedder: Arc<dyn Embedder>, label: &str) -> Result<()> {
        // Probe first: a model that cannot answer must not touch the index.
        probe(embedder.as_ref())?;
        let same = {
            let conn = self.conn();
            meta_get(&conn, KEY_MODE)?.as_deref() == Some("smart")
                && meta_get(&conn, KEY_LABEL)?.as_deref() == Some(label)
                && meta_get(&conn, KEY_STATE)?.as_deref().unwrap_or("ready") == "ready"
                && meta_get(&conn, KEY_DIRTY)?.as_deref() != Some("1")
                && self.vec_dim() == embedder.dim()
        };
        if same {
            // Restart: the index on disk is complete, only the embedder is back.
            self.set_embedder(embedder);
            self.vec_ready.store(true, Ordering::Release);
            return Ok(());
        }
        match self.rebuild(embedder, EmbedMode::Smart, label) {
            Ok(()) => Ok(()),
            Err(e) => {
                let _ = self.fall_back_to_basic(&e.to_string());
                Err(e)
            }
        }
    }

    /// Switch back to Basic (word match) and rebuild the index for it.
    ///
    /// # Errors
    /// SQLite failure.
    pub fn enable_basic(&self) -> Result<()> {
        self.rebuild(
            Arc::new(HashEmbedder::new(BASIC_DIM)),
            EmbedMode::Basic,
            "Basic",
        )
    }

    fn fall_back_to_basic(&self, why: &str) -> Result<()> {
        self.rebuild(
            Arc::new(HashEmbedder::new(BASIC_DIM)),
            EmbedMode::Basic,
            "Basic",
        )?;
        let conn = self.conn();
        meta_set(&conn, KEY_STATE, "failed")?;
        // Short reason for the owner; never the raw provider body.
        let short: String = why.chars().take(160).collect();
        meta_set(&conn, KEY_ERROR, &short)?;
        Ok(())
    }

    fn rebuild(&self, embedder: Arc<dyn Embedder>, mode: EmbedMode, label: &str) -> Result<()> {
        let dim = embedder.dim();
        let ids: Vec<(i64, String)> = {
            let mut conn = self.conn();
            let tx = conn.transaction()?;
            let ids: Vec<(i64, String)> = {
                let mut st = tx.prepare(
                    "SELECT rowid, content FROM memories WHERE deleted = 0 ORDER BY rowid",
                )?;
                st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<rusqlite::Result<_>>()?
            };
            tx.execute_batch(&format!(
                "DROP TABLE IF EXISTS memories_vec; \
                 CREATE VIRTUAL TABLE memories_vec USING vec0(embedding float[{dim}])"
            ))?;
            meta_set(&tx, "embedding_dim", &dim.to_string())?;
            meta_set(&tx, KEY_STATE, "rebuilding")?;
            meta_set(&tx, KEY_DONE, "0")?;
            meta_set(&tx, KEY_TOTAL, &ids.len().to_string())?;
            meta_set(&tx, KEY_ERROR, "")?;
            meta_set(&tx, KEY_DIRTY, "0")?;
            tx.commit()?;
            // New writes are embedded with the new model from here on.
            self.set_vec_dim(dim);
            self.set_embedder(Arc::clone(&embedder));
            self.vec_ready.store(false, Ordering::Release);
            ids
        };
        for (n, chunk) in ids.chunks(BATCH).enumerate() {
            let texts: Vec<&str> = chunk.iter().map(|(_, c)| c.as_str()).collect();
            let vectors = embedder.embed(&texts)?;
            if vectors.len() != chunk.len() {
                return Err(MemoryError::Embed(
                    "embedder returned too few vectors".into(),
                ));
            }
            let conn = self.conn();
            for ((rowid, content), mut v) in chunk.iter().zip(vectors) {
                if v.len() != dim {
                    return Err(MemoryError::Embed(format!(
                        "expected {dim} dims, got {}",
                        v.len()
                    )));
                }
                normalize(&mut v);
                // Skip rows deleted or re-worded since the snapshot: a changed
                // row is embedded by its own update path.
                let live: Option<String> = conn
                    .query_row(
                        "SELECT content FROM memories WHERE rowid = ?1 AND deleted = 0",
                        [rowid],
                        |r| r.get(0),
                    )
                    .ok();
                if live.as_deref() == Some(content.as_str()) {
                    conn.execute("DELETE FROM memories_vec WHERE rowid = ?1", [rowid])?;
                    conn.execute(
                        "INSERT INTO memories_vec(rowid, embedding) VALUES (?1, ?2)",
                        params![rowid, to_blob(&v)],
                    )?;
                }
            }
            meta_set(
                &conn,
                KEY_DONE,
                &((n + 1) * BATCH).min(ids.len()).to_string(),
            )?;
        }
        let conn = self.conn();
        meta_set(
            &conn,
            KEY_MODE,
            if mode == EmbedMode::Smart {
                "smart"
            } else {
                "basic"
            },
        )?;
        meta_set(&conn, KEY_LABEL, label)?;
        meta_set(&conn, KEY_STATE, "ready")?;
        meta_set(&conn, KEY_DONE, &ids.len().to_string())?;
        drop(conn);
        self.vec_ready.store(true, Ordering::Release);
        Ok(())
    }
}

fn probe(embedder: &dyn Embedder) -> Result<()> {
    let out = embedder.embed(&["probe"])?;
    match out.first() {
        Some(v) if v.len() == embedder.dim() && v.iter().all(|x| x.is_finite()) => Ok(()),
        _ => Err(MemoryError::Embed(
            "embedding model gave no usable answer".into(),
        )),
    }
}
