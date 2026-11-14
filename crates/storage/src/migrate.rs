//! Forward-only versioned migration runner with embedded SQL.

use rusqlite::Connection;

use crate::error::{Result, StorageError};

/// Embedded migrations, ordered; version = index + 1.
const MIGRATIONS: &[(&str, &str)] = &[
    ("0001_init", include_str!("../migrations/0001_init.sql")),
    (
        "0002_event_index",
        include_str!("../migrations/0002_event_index.sql"),
    ),
    (
        "0003_subagents",
        include_str!("../migrations/0003_subagents.sql"),
    ),
    (
        "0004_computer_target",
        include_str!("../migrations/0004_computer_target.sql"),
    ),
    (
        "0005_computer_identity",
        include_str!("../migrations/0005_computer_identity.sql"),
    ),
];

/// Highest schema version this build supports.
#[must_use]
pub fn latest_version() -> u32 {
    u32::try_from(MIGRATIONS.len()).unwrap_or(u32::MAX)
}

/// Current schema version of the database (0 for an empty DB).
///
/// # Errors
/// Returns an error on SQLite failure.
pub fn current_version(conn: &Connection) -> Result<u32> {
    Ok(conn.query_row("PRAGMA user_version", [], |r| r.get(0))?)
}

/// Apply all pending migrations, each in its own transaction.  Returns the
/// number applied.
///
/// # Errors
/// Fails on SQL errors or if the DB is newer than this build.
pub fn migrate(conn: &mut Connection) -> Result<u32> {
    let current = current_version(conn)?;
    let latest = latest_version();
    if current > latest {
        return Err(StorageError::SchemaTooNew {
            found: current,
            supported: latest,
        });
    }
    let mut applied = 0;
    for (i, (_name, sql)) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let version = i + 1;
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.execute_batch(&format!("PRAGMA user_version = {version}"))?;
        tx.commit()?;
        applied += 1;
    }
    Ok(applied)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tables(conn: &Connection) -> Vec<String> {
        let mut st = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
            .unwrap();
        st.query_map([], |r| r.get(0))
            .unwrap()
            .map(std::result::Result::unwrap)
            .collect()
    }

    #[test]
    fn migrates_empty_db_and_is_idempotent() {
        let mut conn = Connection::open_in_memory().unwrap();
        assert_eq!(current_version(&conn).unwrap(), 0);
        assert_eq!(migrate(&mut conn).unwrap(), latest_version());
        assert_eq!(current_version(&conn).unwrap(), latest_version());
        assert_eq!(migrate(&mut conn).unwrap(), 0);
        assert_eq!(
            tables(&conn),
            [
                "approvals",
                "audit_log",
                "bots",
                "chats",
                "event_index",
                "messages",
                "run_steps",
                "runs",
                "users"
            ]
        );
    }

    #[test]
    fn pre_058_paused_run_survives_upgrade_with_null_target_and_pause_clock() {
        let mut conn = Connection::open_in_memory().unwrap();
        for (_, sql) in &MIGRATIONS[..3] {
            conn.execute_batch(sql).unwrap();
        }
        conn.execute_batch(
            "PRAGMA user_version = 3;
            INSERT INTO users (id,name,created_at_ms) VALUES ('user','u',100);
            INSERT INTO bots (id,user_id,name,created_at_ms) VALUES ('bot','user','b',100);
            INSERT INTO runs (id,user_id,bot_id,kind,state,created_at_ms,updated_at_ms)
            VALUES ('run','user','bot','turn','paused',100,200);",
        )
        .unwrap();
        migrate(&mut conn).unwrap();
        let row: (String, Option<String>, Option<String>, i64) = conn
            .query_row(
                "SELECT state,computer_target,computer_node,paused_at_ms FROM runs WHERE id='run'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(row, ("paused".into(), None, None, 200));
    }

    #[test]
    fn rejects_newer_schema() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA user_version = 999").unwrap();
        assert!(matches!(
            migrate(&mut conn),
            Err(StorageError::SchemaTooNew { found: 999, .. })
        ));
    }
}
