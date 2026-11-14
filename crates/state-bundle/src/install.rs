use std::{
    fs,
    path::{Path, PathBuf},
};

use permission_engine::Policy;
use rusqlite::{Connection, OpenFlags};
use secret_store::SecretStore;
use zeroize::Zeroizing;

use crate::{
    ImportError, ImportOptions, Manifest, Result, invalid, now,
    policy::POLICY_FILE,
    transfer::{INCLUDED_DIRECTORIES, INCLUDED_FILES, Secrets},
};

pub(crate) struct Staging(pub PathBuf);
impl Staging {
    pub(crate) fn create(data: &Path) -> Result<Self> {
        let path = data.join("import-staging");
        let builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = builder;
            builder.mode(0o700);
            builder.create(&path)?;
        }
        // Fail if staging already exists, including symlinks; never adopt it.
        #[cfg(not(unix))]
        builder.create(&path)?;
        Ok(Self(path))
    }
}
impl Drop for Staging {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(invalid("symlink in target state")),
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

pub(crate) fn check_target(data: &Path, replace: bool) -> Result<()> {
    if !exists(&data.join("bot.db"))? || replace {
        return Ok(());
    }
    let conn = Connection::open_with_flags(data.join("bot.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    // A read-only handle avoids migrating or creating anything in the target.
    for table in ["bots", "chats"] {
        let present: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            [table],
            |r| r.get(0),
        )?;
        if present
            && conn.query_row(&format!("SELECT EXISTS(SELECT 1 FROM {table})"), [], |r| {
                r.get::<_, bool>(0)
            })?
        {
            return Err(ImportError::TargetNotEmpty);
        }
    }
    Ok(())
}

fn database_error(db: &str, error: impl std::fmt::Display) -> ImportError {
    ImportError::InvalidDatabase {
        db: db.into(),
        reason: error.to_string(),
    }
}

fn incompatible_schema(db: &str, found: u32, supported: u32) -> ImportError {
    ImportError::IncompatibleSchema {
        db: db.into(),
        found,
        supported,
    }
}

/// Chat history is delivered to the model; only user and assistant messages and
/// the daemon's own well-formed control note are. Any other imported row (a
/// system claim, an unknown role, the control-note marker with other text)
/// becomes the inert `imported_note` role that is never sent to the model, and
/// is never turned into a user message, which would carry the owner's
/// authority. Nothing is deleted.
fn neutralise_system_messages(conn: &Connection) -> rusqlite::Result<()> {
    let rows: Vec<(String, String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT id, role, content FROM messages WHERE role NOT IN ('user', 'assistant', 'tool', ?1)",
        )?;
        stmt.query_map([agent_core::control_note::IMPORTED_ROLE], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<rusqlite::Result<_>>()?
    };
    for (id, role, content) in rows {
        if role == agent_core::control_note::ROLE
            && agent_core::control_note::is_well_formed(&content)
        {
            continue;
        }
        conn.execute(
            "UPDATE messages SET role = ?2 WHERE id = ?1",
            rusqlite::params![id, agent_core::control_note::IMPORTED_ROLE],
        )?;
    }
    Ok(())
}

/// Every run must be reachable from a root within the runtime's depth limit.
/// A bounded SQLite traversal rejects cycles without recursing through imported
/// data on the Rust stack (the runtime reconstructs parent contexts recursively).
fn validate_run_ancestry(conn: &Connection) -> Result<()> {
    let invalid: bool = conn
        .query_row(
            "WITH RECURSIVE ancestry(id, depth) AS (\
                SELECT id, 0 FROM runs WHERE parent_run_id IS NULL \
                UNION ALL \
                SELECT child.id, parent.depth + 1 FROM runs AS child \
                JOIN ancestry AS parent ON child.parent_run_id = parent.id \
                WHERE parent.depth < ?1\
             ) SELECT EXISTS(SELECT 1 FROM runs WHERE NOT EXISTS(\
                SELECT 1 FROM ancestry WHERE ancestry.id = runs.id\
             ))",
            [agent_core::MAX_SUBAGENT_DEPTH],
            |row| row.get(0),
        )
        .map_err(|error| database_error("bot.db", error))?;
    if invalid {
        return Err(database_error("bot.db", "cyclic or excessive run ancestry"));
    }
    Ok(())
}

fn prepare_database(path: &Path, db: &str, manifest: &Manifest) -> Result<()> {
    let open = || {
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)
            .map_err(|error| database_error(db, error))
    };
    let conn = open()?;
    let found =
        storage::migrate::current_version(&conn).map_err(|error| database_error(db, error))?;
    if db == "bot.db" && found != manifest.schema_version
        || manifest
            .db_schemas
            .as_ref()
            .is_some_and(|schemas| schemas.get(db) != Some(&found))
    {
        return Err(database_error(db, "database and manifest schema disagree"));
    }
    match db {
        "bot.db" => {
            drop(conn);
            storage::Storage::validate_and_migrate(path).map_err(|error| match error {
                storage::StorageError::SchemaTooNew { found, supported } => {
                    incompatible_schema(db, found, supported)
                }
                other => database_error(db, other),
            })?;
            let conn = open()?;
            validate_run_ancestry(&conn)?;
            neutralise_system_messages(&conn).map_err(|error| database_error(db, error))?;
            drop(conn);
        }
        "schedules.db" => {
            drop(conn);
            scheduler::ScheduleStore::validate_and_migrate(path).map_err(|error| match error {
                scheduler::SchedulerError::SchemaTooNew { found, supported } => {
                    incompatible_schema(db, found, supported)
                }
                other => database_error(db, other),
            })?;
        }
        "memory.db" => {
            drop(conn);
            memory_store::MemoryStore::validate_and_migrate(path).map_err(|error| match error {
                memory_store::MemoryError::SchemaTooNew { found, supported } => {
                    incompatible_schema(db, found, supported)
                }
                other => database_error(db, other),
            })?;
        }
        _ => return Err(database_error(db, "unsupported database")),
    }
    // Reopen after the owning store has closed all handles and registered any
    // extensions it needs. Each staged database must be a standalone file.
    let conn = open()?;
    let integrity: String = conn
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .map_err(|error| database_error(db, error))?;
    if integrity != "ok" {
        return Err(database_error(db, integrity));
    }
    let busy: u32 = conn
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
        .map_err(|error| database_error(db, error))?;
    if busy != 0 {
        return Err(database_error(db, "WAL checkpoint is busy"));
    }
    let mode: String = conn
        .query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))
        .map_err(|error| database_error(db, error))?;
    if mode != "delete" {
        return Err(database_error(db, "cannot make database self-contained"));
    }
    drop(conn);
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .and_then(|file| file.sync_all())
        .map_err(|error| database_error(db, error))?;
    Ok(())
}

pub(crate) fn prepare(staging: &Path, manifest: &Manifest, opts: &ImportOptions) -> Result<()> {
    const DATABASES: &[&str] = &["bot.db", "schedules.db", "memory.db"];
    if let Some(schemas) = &manifest.db_schemas {
        for db in schemas.keys() {
            if !DATABASES.contains(&db.as_str()) || !staging.join(db).is_file() {
                return Err(database_error(db, "manifest schema entry has no database"));
            }
        }
    }
    for db in DATABASES {
        let path = staging.join(db);
        if *db == "bot.db" || exists(&path)? {
            prepare_database(&path, db, manifest)?;
        }
    }
    if !matches!(opts.host_mode.as_str(), "standalone" | "host" | "client")
        || !matches!(opts.host_state.as_str(), "fresh" | "standby" | "active")
    {
        return Err(invalid("invalid destination host policy"));
    }
    let path = staging.join("host.toml");
    let mut host: toml::Table = if path.exists() {
        toml::from_str(&fs::read_to_string(&path)?).map_err(|_| invalid("invalid host.toml"))?
    } else {
        toml::Table::new()
    };
    host.insert("mode".into(), opts.host_mode.clone().into());
    host.insert("state".into(), opts.host_state.clone().into());
    host.insert(
        "epoch".into(),
        i64::try_from(manifest.epoch)
            .map_err(|_| invalid("epoch out of range"))?
            .into(),
    );
    host.remove("leader");
    fs::write(
        path,
        toml::to_string(&host).map_err(|_| invalid("invalid host config"))?,
    )?;
    Ok(())
}

fn roots() -> Vec<&'static str> {
    INCLUDED_DIRECTORIES
        .iter()
        .chain(INCLUDED_FILES)
        .copied()
        .filter(|root| *root != POLICY_FILE)
        .chain([
            "bot.db-wal",
            "bot.db-shm",
            "bot.db-journal",
            "schedules.db-wal",
            "schedules.db-shm",
            "schedules.db-journal",
            "memory.db-wal",
            "memory.db-shm",
            "memory.db-journal",
        ])
        // Keep the old policy until every old database and sidecar is gone.
        .chain([POLICY_FILE])
        .collect()
}

fn restore_secrets(
    store: &dyn SecretStore,
    old: &[(String, Option<Zeroizing<String>>)],
) -> Result<()> {
    for (name, value) in old.iter().rev() {
        match value {
            Some(value) => store.set(name, value)?,
            None => {
                store.delete(name)?;
            }
        }
    }
    Ok(())
}

/// Each root rename is atomic; the transaction as a whole is rollback protected,
/// with no asynchronous cancellation points. See README for crash limitations.
///
/// The policy file present at the rename is checked again here. A concurrent
/// writer can replace or create it after the caller's last `check_destination`.
pub(crate) fn commit(
    data: &Path,
    staging: &Path,
    store: &dyn SecretStore,
    secrets: &Secrets,
    incoming_policy: &Policy,
    allow_policy_relaxation: bool,
) -> Result<Option<PathBuf>> {
    let roots = roots();
    let mut old_roots = Vec::new();
    for root in &roots {
        if exists(&data.join(root))? {
            old_roots.push(*root);
        }
    }
    let backup = backup_path(data)?;
    let old_secrets = secrets
        .keys()
        .map(|name| {
            Ok((
                name.clone(),
                store
                    .get(name)?
                    .map(|value| Zeroizing::new(value.expose().to_owned())),
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    fs::create_dir(&backup)?;
    let mut moved = Vec::new();
    let mut installed = Vec::new();
    let mut secret_attempts = 0;
    let result: Result<()> = (|| {
        // Capture originals before the first write; restore even a failed set,
        // since some backends can mutate before reporting an error.
        for (name, value) in secrets {
            secret_attempts += 1;
            store.set(name, value)?;
        }
        #[cfg(test)]
        BEFORE_ROOT_MOVES.with(|slot| {
            if let Some(hook) = slot.get() {
                hook(data);
            }
        });
        for root in &old_roots {
            // Captured below, after a writer may have replaced or created it.
            if *root == POLICY_FILE {
                continue;
            }
            fs::rename(data.join(root), backup.join(root))?;
            moved.push(*root);
        }
        if exists(&data.join(POLICY_FILE))? {
            fs::rename(data.join(POLICY_FILE), backup.join(POLICY_FILE))?;
            moved.push(POLICY_FILE);
        }
        // This is the ceiling the rename would overwrite, not the one observed
        // before secrets and the move. A missing file is the daemon default.
        crate::policy::check_destination(&backup, incoming_policy, allow_policy_relaxation)?;
        // Publish the incoming permission ceiling before any imported grants.
        // Reverse rollback removes databases before removing this policy, and
        // restores the old policy before restoring its databases.
        for root in std::iter::once(POLICY_FILE)
            .chain(roots.iter().copied().filter(|root| *root != POLICY_FILE))
        {
            if exists(&staging.join(root))? {
                fs::rename(staging.join(root), data.join(root))?;
                installed.push(root);
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        let rollback: Result<()> = (|| {
            for root in installed.iter().rev() {
                fs::rename(data.join(root), staging.join(root))?;
            }
            for root in moved.iter().rev() {
                fs::rename(backup.join(root), data.join(root))?;
            }
            restore_secrets(store, &old_secrets[..secret_attempts])?;
            fs::remove_dir(&backup)?;
            Ok(())
        })();
        return match rollback {
            Ok(()) => Err(error),
            Err(e) => Err(ImportError::Recovery(format!(
                "{e}; backup retained at {}",
                backup.display()
            ))),
        };
    }
    let backup = if moved.is_empty() {
        fs::remove_dir(&backup)?;
        None
    } else {
        Some(backup)
    };
    // Pruning is best effort after a committed swap: an unrelated backup's
    // permission error must not misreport the installed state as a failed import.
    let _ = prune(data);
    Ok(backup)
}

fn prune(data: &Path) -> Result<()> {
    let mut backups = Vec::new();
    for entry in fs::read_dir(data)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name();
        if let Some(stamp) = name
            .to_str()
            .and_then(|n| n.strip_prefix("backup-"))
            .and_then(|n| n.parse::<u64>().ok())
        {
            backups.push((stamp, entry.path()));
        }
    }
    backups.sort_by_key(|(stamp, _)| *stamp);
    let excess = backups.len().saturating_sub(3);
    for (_, path) in backups.into_iter().take(excess) {
        fs::remove_dir_all(path)?;
    }
    Ok(())
}

fn backup_path(data: &Path) -> Result<PathBuf> {
    let mut stamp = now();
    // Retention can leave gaps; do not reuse a pruned timestamp and immediately
    // prune the newest backup when several imports finish in the same second.
    for entry in fs::read_dir(data)? {
        let entry = entry?;
        if let Some(previous) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.strip_prefix("backup-"))
            .and_then(|name| name.parse::<u64>().ok())
        {
            stamp = stamp.max(
                previous
                    .checked_add(1)
                    .ok_or_else(|| invalid("backup timestamp overflow"))?,
            );
        }
    }
    let backup = loop {
        let candidate = data.join(format!("backup-{stamp}"));
        if fs::symlink_metadata(&candidate).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
        {
            break candidate;
        }
        stamp = stamp
            .checked_add(1)
            .ok_or_else(|| invalid("backup timestamp overflow"))?;
    };
    Ok(backup)
}

#[cfg(test)]
thread_local! {
    static BEFORE_ROOT_MOVES: std::cell::Cell<Option<fn(&Path)>> =
        const { std::cell::Cell::new(None) };
}

/// Test seam: runs after secret writes and before root renames, on the
/// `commit` caller thread. Production builds have no hook.
#[cfg(test)]
pub(crate) fn set_before_root_moves(hook: Option<fn(&Path)>) {
    BEFORE_ROOT_MOVES.with(|slot| slot.set(hook));
}
