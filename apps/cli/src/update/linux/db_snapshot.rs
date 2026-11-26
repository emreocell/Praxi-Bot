//! Copies of the daemon's SQLite databases, taken while the service is stopped.
//!
//! Migrations only move forward: a release that opened the data directory leaves databases an older
//! release refuses (`SchemaTooNew`). Putting the old program files back is therefore not a rollback
//! unless the data goes back with them.
use crate::Result;
use std::{
    fs,
    io::Write,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

/// The data directory of an installer-managed host (`BOT_DATA_DIR`).
pub(super) const DATA_DIR: &str = "/var/lib/praxi";
/// Root-only, under the updater's state directory. Kept until the next update replaces it.
pub(super) const SNAPSHOT_DIR: &str = "/var/lib/praxi-updates/db-snapshot";

const MANIFEST: &str = "manifest.json";
const SUFFIXES: [&str; 3] = [".db", ".db-wal", ".db-shm"];
/// Left free on the disk after the copy, so the daemon can still write.
const FREE_MARGIN: u64 = 256 * 1024 * 1024;

#[derive(serde::Serialize, serde::Deserialize)]
struct Entry {
    /// Relative to the data directory.
    path: PathBuf,
    uid: u32,
    gid: u32,
    mode: u32,
    len: u64,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Manifest {
    files: Vec<Entry>,
}

fn is_database(name: &str) -> bool {
    SUFFIXES.iter().any(|suffix| name.ends_with(suffix))
}

/// Database files (and their WAL companions) directly in the data directory and in `bots/<id>/`
/// (knowledge stores). Links and anything that is not a regular file are refused, never followed.
fn database_files(data: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let mut directories = vec![PathBuf::new()];
    match fs::read_dir(data.join("bots")) {
        Ok(bots) => {
            for bot in bots {
                let bot = bot?;
                if bot.file_type()?.is_dir() {
                    directories.push(PathBuf::from("bots").join(bot.file_name()));
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    for directory in directories {
        let entries = match fs::read_dir(data.join(&directory)) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        for entry in entries {
            let entry = entry?;
            if !entry.file_name().to_str().is_some_and(is_database) {
                continue;
            }
            if !entry.file_type()?.is_file() {
                return Err(format!(
                    "{} is not a regular file; refusing to copy it",
                    directory.join(entry.file_name()).display()
                )
                .into());
            }
            found.push(directory.join(entry.file_name()));
        }
    }
    found.sort();
    Ok(found)
}

fn available_bytes(path: &Path) -> Result<u64> {
    let stat = rustix::fs::statvfs(path)?;
    Ok(stat.f_bavail.saturating_mul(stat.f_frsize))
}

/// Fails before anything is stopped when the copy would not fit on the disk.
pub(super) fn preflight(data: &Path, snapshot: &Path) -> Result<()> {
    let parent = snapshot.parent().ok_or("invalid snapshot path")?;
    fs::create_dir_all(parent)?;
    preflight_with(data, snapshot, available_bytes(parent)?)
}

fn preflight_with(data: &Path, snapshot: &Path, available: u64) -> Result<()> {
    let needed: u64 = database_files(data)?
        .iter()
        .map(|path| fs::metadata(data.join(path)).map(|meta| meta.len()))
        .sum::<std::io::Result<u64>>()?;
    // The previous snapshot is replaced, so its space counts as free.
    let reusable: u64 = fs::read_dir(snapshot)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| entry.metadata().ok())
                .map(|meta| meta.len())
                .sum()
        })
        .unwrap_or(0);
    if available.saturating_add(reusable) < needed.saturating_add(FREE_MARGIN) {
        return Err(format!(
            "not enough free disk space for a safety copy of the data ({} MiB); nothing was changed",
            needed / (1024 * 1024) + 1
        )
        .into());
    }
    Ok(())
}

/// Replace the snapshot by copies of the current databases. The service must be stopped: the files
/// are then consistent, and a clean stop has already folded the write-ahead logs into them.
pub(super) fn take(data: &Path, snapshot: &Path) -> Result<()> {
    let parent = snapshot.parent().ok_or("invalid snapshot path")?;
    let staging = tempfile::Builder::new()
        .prefix(".db-snapshot-")
        .tempdir_in(parent)?;
    fs::set_permissions(staging.path(), fs::Permissions::from_mode(0o700))?;
    let mut files = Vec::new();
    for path in database_files(data)? {
        let source = data.join(&path);
        let meta = fs::metadata(&source)?;
        let destination = staging.path().join(snapshot_name(&path));
        fs::copy(&source, &destination)?;
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o600))?;
        fs::File::open(&destination)?.sync_all()?;
        if fs::metadata(&destination)?.len() != meta.len() {
            return Err(format!("short copy of {}", path.display()).into());
        }
        files.push(Entry {
            path,
            uid: meta.uid(),
            gid: meta.gid(),
            mode: meta.mode() & 0o7777,
            len: meta.len(),
        });
    }
    // The manifest comes last: a snapshot without one is not complete and is never restored.
    let mut manifest = fs::File::create(staging.path().join(MANIFEST))?;
    manifest.write_all(&serde_json::to_vec(&Manifest { files })?)?;
    manifest.sync_all()?;
    fs::File::open(staging.path())?.sync_all()?;
    match fs::remove_dir_all(snapshot) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    fs::rename(staging.path(), snapshot)?;
    // The directory now belongs to the snapshot; do not remove it with the guard.
    let _ = staging.keep();
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

/// Name of a data file inside the snapshot directory (one flat level; the path is in the manifest).
fn snapshot_name(path: &Path) -> String {
    path.to_string_lossy().replace('/', "__")
}

/// Put the snapshot's databases back, with their owner and mode. A write-ahead log left by the new
/// release is removed first: applied to an older file it would corrupt it.
pub(super) fn restore(data: &Path, snapshot: &Path) -> Result<()> {
    let manifest: Manifest = serde_json::from_slice(
        &fs::read(snapshot.join(MANIFEST))
            .map_err(|e| format!("no complete database snapshot to restore ({e})"))?,
    )?;
    for entry in &manifest.files {
        let copy = snapshot.join(snapshot_name(&entry.path));
        if fs::metadata(&copy)?.len() != entry.len {
            return Err(format!("the snapshot of {} is damaged", entry.path.display()).into());
        }
    }
    for entry in &manifest.files {
        let target = data.join(&entry.path);
        let name = target
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("invalid database name")?;
        if let Some(base) = name.strip_suffix(".db") {
            for suffix in ["-wal", "-shm"] {
                match fs::remove_file(target.with_file_name(format!("{base}.db{suffix}"))) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }
    }
    for entry in &manifest.files {
        let target = data.join(&entry.path);
        let directory = target.parent().ok_or("invalid database path")?;
        // Copy beside the target, then rename: the old file is replaced as a whole or not at all.
        let staging = tempfile::Builder::new()
            .prefix(".restore-")
            .tempdir_in(directory)?;
        let copy = staging.path().join("db");
        fs::copy(snapshot.join(snapshot_name(&entry.path)), &copy)?;
        std::os::unix::fs::chown(&copy, Some(entry.uid), Some(entry.gid))?;
        fs::set_permissions(&copy, fs::Permissions::from_mode(entry.mode))?;
        fs::File::open(&copy)?.sync_all()?;
        fs::rename(&copy, &target)?;
        fs::File::open(directory)?.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("bots/b1")).unwrap();
        fs::write(dir.path().join("bot.db"), "chats v1").unwrap();
        fs::write(dir.path().join("memory.db"), "memory v1").unwrap();
        fs::write(dir.path().join("bots/b1/knowledge.db"), "knowledge v1").unwrap();
        // Not databases: never copied.
        fs::write(dir.path().join("praxi.env"), "x").unwrap();
        fs::create_dir_all(dir.path().join("backups")).unwrap();
        fs::write(dir.path().join("backups/bot-v1.db"), "old backup").unwrap();
        dir
    }

    #[test]
    fn finds_databases_and_their_logs_but_nothing_else() {
        let dir = data_dir();
        fs::write(dir.path().join("bot.db-wal"), "wal").unwrap();
        fs::write(dir.path().join("bot.db-shm"), "shm").unwrap();
        let found = database_files(dir.path()).unwrap();
        assert_eq!(
            found,
            [
                "bot.db",
                "bot.db-shm",
                "bot.db-wal",
                "bots/b1/knowledge.db",
                "memory.db"
            ]
            .map(PathBuf::from)
        );
    }

    #[test]
    fn restores_what_was_taken_and_drops_a_newer_write_ahead_log() {
        let dir = data_dir();
        let snapshots = tempfile::tempdir().unwrap();
        let snapshot = snapshots.path().join("snapshot");
        preflight(dir.path(), &snapshot).unwrap();
        take(dir.path(), &snapshot).unwrap();
        // The new release migrates: files change, a WAL appears, a new database is created.
        fs::write(dir.path().join("bot.db"), "chats v2 (migrated)").unwrap();
        fs::write(dir.path().join("bot.db-wal"), "wal of the new release").unwrap();
        fs::write(dir.path().join("bot.db-shm"), "shm").unwrap();
        fs::write(dir.path().join("bots/b1/knowledge.db"), "knowledge v2").unwrap();
        fs::remove_file(dir.path().join("memory.db")).unwrap();
        fs::write(dir.path().join("created-by-new.db"), "new").unwrap();
        restore(dir.path(), &snapshot).unwrap();
        for (file, text) in [
            ("bot.db", "chats v1"),
            ("memory.db", "memory v1"),
            ("bots/b1/knowledge.db", "knowledge v1"),
        ] {
            assert_eq!(fs::read_to_string(dir.path().join(file)).unwrap(), text);
        }
        assert!(!dir.path().join("bot.db-wal").exists());
        assert!(!dir.path().join("bot.db-shm").exists());
        // Files the snapshot knows nothing about are left alone.
        assert!(dir.path().join("created-by-new.db").exists());
        assert!(dir.path().join("backups/bot-v1.db").exists());
        // No temporary directories are left behind in the data directory.
        assert!(
            fs::read_dir(dir.path())
                .unwrap()
                .flatten()
                .all(|entry| !entry.file_name().to_string_lossy().starts_with(".restore-"))
        );
    }

    #[test]
    fn a_new_snapshot_replaces_the_old_one_and_keeps_owner_and_mode() {
        let dir = data_dir();
        fs::set_permissions(dir.path().join("bot.db"), fs::Permissions::from_mode(0o640)).unwrap();
        let snapshots = tempfile::tempdir().unwrap();
        let snapshot = snapshots.path().join("snapshot");
        take(dir.path(), &snapshot).unwrap();
        fs::write(dir.path().join("bot.db"), "chats v1.5").unwrap();
        take(dir.path(), &snapshot).unwrap();
        assert_eq!(
            fs::metadata(&snapshot).unwrap().permissions().mode() & 0o777,
            0o700
        );
        fs::write(dir.path().join("bot.db"), "broken").unwrap();
        restore(dir.path(), &snapshot).unwrap();
        let restored = fs::metadata(dir.path().join("bot.db")).unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("bot.db")).unwrap(),
            "chats v1.5"
        );
        assert_eq!(restored.mode() & 0o777, 0o640);
        // Only the snapshot itself remains next to its parent: no staging directories.
        assert_eq!(fs::read_dir(snapshots.path()).unwrap().count(), 1);
    }

    #[test]
    fn an_incomplete_or_damaged_snapshot_is_never_restored() {
        let dir = data_dir();
        let snapshots = tempfile::tempdir().unwrap();
        let snapshot = snapshots.path().join("snapshot");
        assert!(restore(dir.path(), &snapshot).is_err());
        take(dir.path(), &snapshot).unwrap();
        fs::write(dir.path().join("bot.db"), "newer").unwrap();
        fs::write(
            snapshot.join(snapshot_name(Path::new("bot.db"))),
            "truncated",
        )
        .unwrap();
        assert!(restore(dir.path(), &snapshot).is_err());
        assert_eq!(
            fs::read_to_string(dir.path().join("bot.db")).unwrap(),
            "newer"
        );
        fs::remove_file(snapshot.join(MANIFEST)).unwrap();
        assert!(restore(dir.path(), &snapshot).is_err());
    }

    #[test]
    fn a_linked_database_is_refused_rather_than_followed() {
        let dir = data_dir();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret.db"), "not ours").unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret.db"), dir.path().join("evil.db"))
            .unwrap();
        let snapshots = tempfile::tempdir().unwrap();
        let error = take(dir.path(), &snapshots.path().join("snapshot")).unwrap_err();
        assert!(error.to_string().contains("not a regular file"));
        assert!(!snapshots.path().join("snapshot").exists());
    }

    #[test]
    fn preflight_refuses_a_copy_that_cannot_fit() {
        let dir = data_dir();
        let snapshots = tempfile::tempdir().unwrap();
        let snapshot = snapshots.path().join("snapshot");
        // Room for the databases but not for the margin the daemon needs afterwards.
        let error = preflight_with(dir.path(), &snapshot, 1024 * 1024).unwrap_err();
        assert!(error.to_string().contains("not enough free disk space"));
        assert!(error.to_string().contains("nothing was changed"));
        preflight_with(dir.path(), &snapshot, FREE_MARGIN + 1024 * 1024).unwrap();
        // The previous snapshot is replaced by the new one, so its size counts as free.
        take(dir.path(), &snapshot).unwrap();
        assert!(fs::read_dir(&snapshot).unwrap().count() > 1);
        preflight_with(dir.path(), &snapshot, FREE_MARGIN).unwrap();
        // The real disk of the test machine has room for this tiny data directory.
        preflight(dir.path(), &snapshot).unwrap();
    }
}
