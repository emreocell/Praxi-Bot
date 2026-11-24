//! Switching the embedding model of a live memory database.
//!
//! The new vectors are built in a staging table while recall keeps using the
//! old ones (no downtime). Memories that change while the copy runs are
//! reconciled at the end, and only then are the tables swapped in one
//! transaction together with the stored vector size and space name. Any
//! failure drops the staging table and leaves the store exactly as it was.

use std::collections::HashMap;
use std::sync::Arc;

use rusqlite::params;

use crate::embed::{Embedder, normalize};
use crate::error::{MemoryError, Result};
use crate::store::MemoryStore;
use crate::vec::to_blob;

const CHUNK: usize = 32;

impl MemoryStore {
    /// Name of the vector space of the stored vectors (`provider/model@dim`),
    /// if one was recorded by [`Self::reembed`].
    ///
    /// # Errors
    /// SQLite failure.
    pub fn embedding_space(&self) -> Result<Option<String>> {
        use rusqlite::OptionalExtension;
        Ok(self
            .conn()
            .query_row(
                "SELECT value FROM meta WHERE key = 'embedding_space'",
                [],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Re-embed every memory with `new` and switch to it, calling
    /// `progress(done, total)` after each chunk. Returns the number of
    /// memories re-embedded.
    ///
    /// # Errors
    /// The embedder or the database failed; the old vectors stay in use.
    pub fn reembed(
        &self,
        new: &Arc<dyn Embedder>,
        space: &str,
        progress: &mut dyn FnMut(usize, usize),
    ) -> Result<usize> {
        let dim = new.dim();
        self.conn().execute_batch(&format!(
            "DROP TABLE IF EXISTS memories_vec_next; \
             CREATE VIRTUAL TABLE memories_vec_next USING vec0(embedding float[{dim}])"
        ))?;
        let result = self.reembed_staged(new, space, progress);
        if result.is_err() {
            let _ = self
                .conn()
                .execute_batch("DROP TABLE IF EXISTS memories_vec_next");
        }
        result
    }

    fn indexed(conn: &rusqlite::Connection) -> Result<Vec<(i64, String)>> {
        let mut stmt = conn.prepare(
            "SELECT m.rowid, m.content FROM memories m \
             WHERE m.rowid IN (SELECT rowid FROM memories_vec) ORDER BY m.rowid",
        )?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    fn embed_checked(new: &Arc<dyn Embedder>, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let mut vectors = new.embed(texts)?;
        if vectors.len() != texts.len() || vectors.iter().any(|v| v.len() != new.dim()) {
            return Err(MemoryError::Embed(
                "the new embedder returned vectors of the wrong size".into(),
            ));
        }
        for v in &mut vectors {
            normalize(v);
        }
        Ok(vectors)
    }

    fn reembed_staged(
        &self,
        new: &Arc<dyn Embedder>,
        space: &str,
        progress: &mut dyn FnMut(usize, usize),
    ) -> Result<usize> {
        let rows = Self::indexed(&self.conn())?;
        let total = rows.len();
        let mut embedded: HashMap<i64, String> = HashMap::with_capacity(total);
        for chunk in rows.chunks(CHUNK) {
            let texts: Vec<&str> = chunk.iter().map(|(_, c)| c.as_str()).collect();
            let vectors = Self::embed_checked(new, &texts)?;
            {
                let conn = self.conn();
                for ((rowid, content), v) in chunk.iter().zip(&vectors) {
                    conn.execute(
                        "INSERT INTO memories_vec_next(rowid, embedding) VALUES (?1, ?2)",
                        params![rowid, to_blob(v)],
                    )?;
                    embedded.insert(*rowid, content.clone());
                }
            }
            progress(embedded.len(), total);
        }
        // Reconcile what changed meanwhile and swap, under one lock.
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        for (rowid, content) in Self::indexed(&tx)? {
            if embedded.get(&rowid) == Some(&content) {
                continue;
            }
            let v = Self::embed_checked(new, &[content.as_str()])?;
            tx.execute("DELETE FROM memories_vec_next WHERE rowid = ?1", [rowid])?;
            tx.execute(
                "INSERT INTO memories_vec_next(rowid, embedding) VALUES (?1, ?2)",
                params![rowid, to_blob(&v[0])],
            )?;
        }
        tx.execute(
            "DELETE FROM memories_vec_next WHERE rowid NOT IN (SELECT rowid FROM memories_vec)",
            [],
        )?;
        // vec0 tables cannot be renamed (their shadow tables keep the old name),
        // so the staged vectors are copied into a fresh table of the new size.
        tx.execute_batch(&format!(
            "DROP TABLE memories_vec;              CREATE VIRTUAL TABLE memories_vec USING vec0(embedding float[{}]);              INSERT INTO memories_vec(rowid, embedding)                  SELECT rowid, embedding FROM memories_vec_next;              DROP TABLE memories_vec_next;",
            new.dim()
        ))?;
        tx.execute(
            "INSERT INTO meta(key, value) VALUES ('embedding_dim', ?1) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [new.dim().to_string()],
        )?;
        tx.execute(
            "INSERT INTO meta(key, value) VALUES ('embedding_space', ?1) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [space],
        )?;
        tx.commit()?;
        // Still holding the connection: no write can slip in with the old model.
        *self
            .embedder
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::clone(new);
        drop(conn);
        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::HashEmbedder;
    use crate::types::{Kind, NewMemory, Trust};
    use protocol::{BotId, UserId};

    struct Failing(usize);
    impl Embedder for Failing {
        fn dim(&self) -> usize {
            128
        }
        fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
            if texts.iter().any(|t| t.contains("explode")) {
                return Err(MemoryError::Embed("provider down".into()));
            }
            HashEmbedder::new(self.0).embed(texts)
        }
    }

    fn add(s: &MemoryStore, user: UserId, text: &str) {
        s.add(
            &NewMemory {
                user_id: user,
                bot_id: None,
                kind: Kind::Profile,
                content: text.into(),
                source_quote: None,
                source_chat_id: None,
                source_message_id: None,
                trust: Trust::User,
                importance: 0.5,
                valid_until_ms: None,
            },
            "test",
        )
        .unwrap();
    }

    #[test]
    fn switching_model_keeps_recall_and_a_failure_changes_nothing() {
        let s = MemoryStore::open_in_memory(Arc::new(HashEmbedder::new(64))).unwrap();
        let (user, bot) = (UserId::new(), BotId::new());
        add(&s, user, "I live in Berlin");
        add(&s, user, "My cat is called Mimi");

        // A failing switch leaves the old vectors and the old model in place.
        add(&s, user, "explode when embedded again");
        let err = s.reembed(
            &(Arc::new(Failing(128)) as Arc<dyn Embedder>),
            "x/fail@128",
            &mut |_, _| {},
        );
        assert!(err.is_err());
        assert_eq!(s.embedding_space().unwrap(), None);
        assert_eq!(s.embedder().dim(), 64);
        let hits = s.recall(user, bot, "Berlin", 500).unwrap();
        assert!(hits.iter().any(|m| m.content.contains("Berlin")));
        add(&s, user, "still works after the failure");

        // Remove the poison memory and switch for real, with progress.
        let poison = s
            .list(user, &crate::types::ListFilter::default())
            .unwrap()
            .into_iter()
            .find(|m| m.content.contains("explode"))
            .unwrap();
        s.delete(user, poison.id, "test").unwrap();
        let mut seen = Vec::new();
        let n = s
            .reembed(
                &(Arc::new(HashEmbedder::new(128)) as Arc<dyn Embedder>),
                "offline/hash@128",
                &mut |d, t| seen.push((d, t)),
            )
            .unwrap();
        assert_eq!(n, 3);
        assert_eq!(seen.last(), Some(&(3, 3)));
        assert_eq!(s.embedder().dim(), 128);
        assert_eq!(
            s.embedding_space().unwrap().as_deref(),
            Some("offline/hash@128")
        );
        let hits = s.recall(user, bot, "where is Berlin", 500).unwrap();
        assert!(hits.iter().any(|m| m.content.contains("Berlin")));
        add(&s, user, "new memory in the new space");
    }
}
