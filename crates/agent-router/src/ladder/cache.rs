//! K1: intent cache. Normalised-text hash -> [`Route`] in SQLite.
//!
//! The table is owned by this crate (`router_intent_cache`), so no storage
//! migration is needed. Entries carry a TTL and, for workflow routes, the
//! workflow's version fingerprint so that editing a workflow invalidates them.

use std::path::Path;

use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};

use super::{Route, RouterError};

/// FNV-1a 64-bit, hex encoded (stable across runs and platforms).
#[must_use]
pub fn text_hash(normalized: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in normalized.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// A cache hit.
#[derive(Debug, Clone, PartialEq)]
pub struct CachedRoute {
    /// The remembered route.
    pub route: Route,
    /// Workflow version fingerprint recorded when the entry was stored.
    pub workflow_version: Option<String>,
}

/// SQLite-backed intent cache.
#[derive(Debug)]
pub struct RouteCache {
    conn: Mutex<Connection>,
    ttl_secs: i64,
}

impl RouteCache {
    /// Open (creating if needed) a cache file.
    ///
    /// # Errors
    /// SQLite failures.
    pub fn open(path: impl AsRef<Path>, ttl_secs: i64) -> Result<Self, RouterError> {
        Self::init(Connection::open(path)?, ttl_secs)
    }

    /// A private in-memory cache (tests, ephemeral bots).
    ///
    /// # Errors
    /// SQLite failures.
    pub fn in_memory(ttl_secs: i64) -> Result<Self, RouterError> {
        Self::init(Connection::open_in_memory()?, ttl_secs)
    }

    fn init(conn: Connection, ttl_secs: i64) -> Result<Self, RouterError> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS router_intent_cache (
                hash TEXT PRIMARY KEY,
                text TEXT NOT NULL,
                route_json TEXT NOT NULL,
                workflow TEXT,
                workflow_version TEXT,
                created_at INTEGER NOT NULL,
                expires_at INTEGER NOT NULL,
                hits INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS router_intent_cache_workflow
                ON router_intent_cache(workflow);",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
            ttl_secs,
        })
    }

    /// Store (or replace) the route for `normalized` text.
    ///
    /// # Errors
    /// SQLite / serialisation failures.
    pub fn put(
        &self,
        normalized: &str,
        route: &Route,
        workflow_version: Option<&str>,
        now: i64,
    ) -> Result<(), RouterError> {
        let workflow = match route {
            Route::Workflow { name, .. } => Some(name.as_str()),
            _ => None,
        };
        self.conn.lock().execute(
            "INSERT OR REPLACE INTO router_intent_cache
             (hash, text, route_json, workflow, workflow_version, created_at, expires_at, hits)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0)",
            params![
                text_hash(normalized),
                normalized,
                serde_json::to_string(route)?,
                workflow,
                workflow_version,
                now,
                now.saturating_add(self.ttl_secs),
            ],
        )?;
        Ok(())
    }

    /// Look up `normalized`. Expired entries are deleted and reported as a
    /// miss; a hash collision (different stored text) is a miss too.
    ///
    /// # Errors
    /// SQLite / deserialisation failures.
    pub fn get(&self, normalized: &str, now: i64) -> Result<Option<CachedRoute>, RouterError> {
        let hash = text_hash(normalized);
        let conn = self.conn.lock();
        let row: Option<(String, String, Option<String>, i64)> = conn
            .query_row(
                "SELECT text, route_json, workflow_version, expires_at
                 FROM router_intent_cache WHERE hash = ?1",
                params![hash],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let Some((text, json, workflow_version, expires_at)) = row else {
            return Ok(None);
        };
        if expires_at <= now {
            conn.execute(
                "DELETE FROM router_intent_cache WHERE hash = ?1",
                params![hash],
            )?;
            return Ok(None);
        }
        if text != normalized {
            return Ok(None);
        }
        conn.execute(
            "UPDATE router_intent_cache SET hits = hits + 1 WHERE hash = ?1",
            params![hash],
        )?;
        Ok(Some(CachedRoute {
            route: serde_json::from_str(&json)?,
            workflow_version,
        }))
    }

    /// Delete the entry for `normalized` (used when a hit turns out stale).
    ///
    /// # Errors
    /// SQLite failures.
    pub fn remove(&self, normalized: &str) -> Result<(), RouterError> {
        self.conn.lock().execute(
            "DELETE FROM router_intent_cache WHERE hash = ?1",
            params![text_hash(normalized)],
        )?;
        Ok(())
    }

    /// Delete every entry that routes to workflow `name`; returns the count.
    ///
    /// # Errors
    /// SQLite failures.
    pub fn invalidate_workflow(&self, name: &str) -> Result<usize, RouterError> {
        Ok(self.conn.lock().execute(
            "DELETE FROM router_intent_cache WHERE workflow = ?1",
            params![name],
        )?)
    }

    /// Delete expired entries; returns the count.
    ///
    /// # Errors
    /// SQLite failures.
    pub fn purge_expired(&self, now: i64) -> Result<usize, RouterError> {
        Ok(self.conn.lock().execute(
            "DELETE FROM router_intent_cache WHERE expires_at <= ?1",
            params![now],
        )?)
    }

    /// Number of stored entries.
    ///
    /// # Errors
    /// SQLite failures.
    pub fn len(&self) -> Result<usize, RouterError> {
        let n: i64 =
            self.conn
                .lock()
                .query_row("SELECT COUNT(*) FROM router_intent_cache", [], |r| r.get(0))?;
        Ok(usize::try_from(n).unwrap_or(0))
    }

    /// `true` if the cache holds no entries.
    ///
    /// # Errors
    /// SQLite failures.
    pub fn is_empty(&self) -> Result<bool, RouterError> {
        Ok(self.len()? == 0)
    }
}
