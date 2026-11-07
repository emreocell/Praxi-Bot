//! Hybrid retrieval: BM25 + vector search fused with RRF, then boosted.

use std::collections::HashMap;
use std::hash::Hash;

use protocol::{BotId, UserId};
use rusqlite::{Connection, params};

use crate::error::Result;
use crate::store::{COLS, MemoryStore, now_ms, row_to_memory};
use crate::types::Memory;
use crate::vec::to_blob;

/// Retrieval tuning.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RecallConfig {
    /// RRF constant `k` (classic value: 60).
    pub rrf_k: f64,
    /// Candidates taken from each retriever.
    pub candidates: usize,
    /// Vector hits below this cosine similarity are ignored.
    pub min_cosine: f32,
    /// Importance boost: score *= 1 + weight * importance.
    pub importance_weight: f64,
    /// Recency half-life (days) of the `last_used`/`created` boost.
    pub recency_half_life_days: f64,
}

impl Default for RecallConfig {
    fn default() -> Self {
        Self {
            rrf_k: 60.0,
            candidates: 50,
            min_cosine: 0.25,
            importance_weight: 0.5,
            recency_half_life_days: 30.0,
        }
    }
}

/// Rough token estimate (~4 chars per token).
#[must_use]
pub fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

/// Reciprocal-rank fusion: each list is best-first; returns `(item, score)`
/// best-first with `score = sum 1/(k + rank)` (rank starts at 1).  Ties keep
/// first-seen order.
#[must_use]
pub fn rrf_fuse<T: Copy + Eq + Hash>(lists: &[Vec<T>], k: f64) -> Vec<(T, f64)> {
    let mut order: Vec<T> = Vec::new();
    let mut score: HashMap<T, f64> = HashMap::new();
    for list in lists {
        for (i, item) in list.iter().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let s = 1.0 / (k + (i + 1) as f64);
            let e = score.entry(*item).or_insert_with(|| {
                order.push(*item);
                0.0
            });
            *e += s;
        }
    }
    let mut out: Vec<(T, f64)> = order.into_iter().map(|t| (t, score[&t])).collect();
    out.sort_by(|a, b| b.1.total_cmp(&a.1));
    out
}

/// FTS5 MATCH expression: OR of the quoted alphanumeric tokens.
pub(crate) fn fts_query(text: &str) -> Option<String> {
    let toks: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| format!("\"{}\"", w.to_lowercase()))
        .collect();
    (!toks.is_empty()).then(|| toks.join(" OR "))
}

fn visible(m: &Memory, user: UserId, bot: BotId, now: i64) -> bool {
    m.user_id == user
        && !m.deleted
        && m.bot_id.is_none_or(|b| b == bot)
        && m.valid_until_ms.is_none_or(|v| v > now)
}

struct Fused {
    memory: Memory,
    rrf: f64,
}

impl MemoryStore {
    fn gather(&self, user: UserId, bot: BotId, query: &str, limit: usize) -> Result<Vec<Fused>> {
        let emb = self.embed_one(query)?;
        let now = now_ms();
        let conn = self.conn();
        let cands = i64::try_from(self.config.candidates.max(limit)).unwrap_or(i64::MAX);

        let mut lexical: Vec<i64> = Vec::new();
        if let Some(q) = fts_query(query) {
            let mut st = conn.prepare(
                "SELECT m.rowid FROM memories_fts JOIN memories m ON m.rowid = memories_fts.rowid \
                 WHERE memories_fts MATCH ?1 AND m.user_id = ?2 \
                 AND (m.bot_id IS NULL OR m.bot_id = ?3) AND m.deleted = 0 \
                 AND (m.valid_until_ms IS NULL OR m.valid_until_ms > ?4) \
                 ORDER BY bm25(memories_fts) LIMIT ?5",
            )?;
            lexical = st
                .query_map(
                    params![q, user.0.to_string(), bot.0.to_string(), now, cands],
                    |r| r.get(0),
                )?
                .collect::<rusqlite::Result<_>>()?;
        }

        let mut semantic: Vec<i64> = Vec::new();
        {
            // The vec table cannot be filtered by scope: over-fetch, then filter.
            let mut st = conn.prepare(
                "SELECT rowid, distance FROM memories_vec WHERE embedding MATCH ?1 AND k = ?2 \
                 ORDER BY distance",
            )?;
            let hits: Vec<(i64, f64)> = st
                .query_map(params![to_blob(&emb), cands * 4], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?
                .collect::<rusqlite::Result<_>>()?;
            for (rowid, dist) in hits {
                #[allow(clippy::cast_possible_truncation)]
                let cos = 1.0 - (dist * dist / 2.0) as f32;
                if cos < self.config.min_cosine {
                    continue;
                }
                if let Some(m) = by_rowid(&conn, rowid)?
                    && visible(&m, user, bot, now)
                {
                    semantic.push(rowid);
                }
                if semantic.len() >= self.config.candidates {
                    break;
                }
            }
        }

        let fused = rrf_fuse(&[lexical, semantic], self.config.rrf_k);
        let mut out = Vec::new();
        for (rowid, rrf) in fused.into_iter().take(limit.max(self.config.candidates)) {
            if let Some(memory) = by_rowid(&conn, rowid)? {
                out.push(Fused { memory, rrf });
            }
        }
        Ok(out)
    }

    /// Existing memories similar to `text` (hybrid, RRF order, no boost, does
    /// not touch `last_used`).  Used for consolidation.
    ///
    /// # Errors
    /// Embedder or SQLite failure.
    pub fn find_similar(
        &self,
        user: UserId,
        bot: BotId,
        text: &str,
        limit: usize,
    ) -> Result<Vec<Memory>> {
        Ok(self
            .gather(user, bot, text, limit)?
            .into_iter()
            .take(limit)
            .map(|f| f.memory)
            .collect())
    }

    /// Memories relevant to `query` for `user` as seen by `bot` (shared +
    /// bot-scoped), best first, cut to `budget_tokens`.
    ///
    /// # Errors
    /// Embedder or SQLite failure.
    pub fn recall(
        &self,
        user: UserId,
        bot: BotId,
        query: &str,
        budget_tokens: usize,
    ) -> Result<Vec<Memory>> {
        let now = now_ms();
        let cfg = self.config;
        let mut scored: Vec<(f64, Memory)> = self
            .gather(user, bot, query, self.config.candidates)?
            .into_iter()
            .map(|f| {
                let m = f.memory;
                #[allow(clippy::cast_precision_loss)]
                let age_days =
                    (now - m.last_used_ms.max(m.created_ms)).max(0) as f64 / 86_400_000.0;
                let recency = 0.5f64.powf(age_days / cfg.recency_half_life_days.max(0.001));
                let boost = (1.0 + cfg.importance_weight * f64::from(m.importance.clamp(0.0, 1.0)))
                    * (0.75 + 0.25 * recency);
                (f.rrf * boost, m)
            })
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0));

        let mut spent = 0usize;
        let mut out = Vec::new();
        for (_, m) in scored {
            let cost = estimate_tokens(&m.content) + 4;
            if spent + cost > budget_tokens {
                break;
            }
            spent += cost;
            out.push(m);
        }
        if !out.is_empty() {
            let conn = self.conn();
            for m in &mut out {
                conn.execute(
                    "UPDATE memories SET last_used_ms = ?2 WHERE id = ?1",
                    params![m.id.to_string(), now],
                )?;
                m.last_used_ms = now;
            }
        }
        Ok(out)
    }
}

fn by_rowid(conn: &Connection, rowid: i64) -> Result<Option<Memory>> {
    use rusqlite::OptionalExtension;
    Ok(conn
        .query_row(
            &format!("SELECT {COLS} FROM memories WHERE rowid = ?1"),
            [rowid],
            row_to_memory,
        )
        .optional()?
        .map(|(_, m)| m))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rrf_orders_by_combined_rank() {
        // 2 is first in A, second in B; 1 is first in B only; 3 only in A (3rd).
        let fused = rrf_fuse(&[vec![2, 5, 3], vec![1, 2]], 60.0);
        let ids: Vec<i32> = fused.iter().map(|x| x.0).collect();
        assert_eq!(ids[0], 2);
        assert_eq!(ids, [2, 1, 5, 3]);
        assert!(fused[0].1 > fused[1].1);
    }

    #[test]
    fn fts_query_quotes_tokens() {
        assert_eq!(
            fts_query("Hello, wor\"ld!").unwrap(),
            "\"hello\" OR \"wor\" OR \"ld\""
        );
        assert!(fts_query("?!").is_none());
    }
}
