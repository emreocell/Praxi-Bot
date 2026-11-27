//! Copies of the daemon's SQLite databases, taken while the service is stopped.
//!
//! Migrations only move forward: a release that opened the data directory leaves databases an older
//! release refuses (`SchemaTooNew`). Putting the old program files back is therefore not a rollback
//! unless the data goes back with them.
//!
//! This runs as root inside a directory the daemon's user owns, so no step may trust a path: that user
//! can swap any component for a link between a check and its use. Every operation goes through
//! directory descriptors (`fd_dir::Dir`): nothing is resolved twice, a link anywhere is an error, and
//! root never copies, writes, changes the owner of or removes anything outside the tree. The
//! service user's processes are stopped before any of this runs (see `proc_pin::quiet_user`).
//!
//! A restore is all or nothing as far as a failure can tell. It first stages every database beside
//! its target (a new name, content and owner set, `fsync`ed); live data is untouched until all of them
//! are staged. A marker file in the snapshot then commits the restore, and from there it only goes
//! forward: the commit step is idempotent, so a run that was killed half way is finished by
//! `restore` (or by `bot self-update --recover`) instead of leaving databases of two generations.
//!
//! **This is a rollback aid, not a backup.** It covers the SQLite databases in the data directory and
//! in `bots/<id>/` only, for the update that is running, and the next update replaces it. A power
//! loss in the few renames of the commit step is not covered; keep a backup (see the host guide).
use super::fd_dir::{self, Dir};
use crate::Result;
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs::{File, Metadata},
    io::{self, Read, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Component, Path, PathBuf},
};

/// The data directory of an installer-managed host (`BOT_DATA_DIR`).
pub(super) const DATA_DIR: &str = "/var/lib/praxi";
/// Root-only, under the updater's state directory. Kept until the next update replaces it.
pub(super) const SNAPSHOT_DIR: &str = "/var/lib/praxi-updates/db-snapshot";

const MANIFEST: &str = "manifest.json";
/// Exists in the snapshot from the moment every database is staged until the last one is in place.
/// It lists the staged files, so that a restore that ran twice can tell them from what the new release
/// left behind. Written under another name and renamed: it is absent or whole.
const COMMIT: &str = "restore-commit";
const COMMIT_NEW: &str = "restore-commit.new";
/// A staged database is `<name>.restore-new` beside its target; not a name `is_database` matches.
const STAGED: &str = ".restore-new";
const SUFFIXES: [&str; 3] = [".db", ".db-wal", ".db-shm"];
/// Left free on the disk after the copy, so the daemon can still write.
const FREE_MARGIN: u64 = 256 * 1024 * 1024;
/// The manifest lists a few databases; anything near this size is not it.
const MAX_MANIFEST: u64 = 16 * 1024 * 1024;

const PENDING: &str = "an earlier rollback of the data was interrupted and is not finished; run `sudo systemctl start praxi-update-check.service` to finish it before updating again";

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

/// A staged database: the file that must be in `path` once the restore is done. A rename keeps the
/// inode, so this tells the staged file from a same-sized database of the release being rolled back.
#[derive(serde::Serialize, serde::Deserialize)]
struct Staged {
    path: PathBuf,
    inode: u64,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Committed {
    staged: Vec<Staged>,
}

fn is_database(name: &str) -> bool {
    SUFFIXES.iter().any(|suffix| name.ends_with(suffix))
}

/// An I/O error that names the path, and says so when a link is what got in the way.
fn failed(path: &Path, error: &io::Error) -> Box<dyn std::error::Error> {
    if fd_dir::is_refusal(error) {
        format!(
            "{} is a link or not an ordinary directory or file; refusing to follow it ({error})",
            path.display()
        )
        .into()
    } else {
        format!("{}: {error}", path.display()).into()
    }
}

fn split(snapshot: &Path) -> Result<(&Path, &OsStr)> {
    match (snapshot.parent(), snapshot.file_name()) {
        (Some(parent), Some(name)) => Ok((parent, name)),
        _ => Err("invalid snapshot path".into()),
    }
}

/// The directory that holds the snapshot. Root keeps the snapshot there, so it must belong to the
/// updater and be closed to everyone else.
fn private_directory(path: &Path) -> Result<Dir> {
    let dir = Dir::open(path).map_err(|e| failed(path, &e))?;
    let stat = dir.stat_self()?;
    if stat.st_uid != rustix::process::geteuid().as_raw() || stat.st_mode & 0o022 != 0 {
        return Err(format!(
            "{} must belong to the updater and not be writable by others (owner {}, mode {:o})",
            path.display(),
            stat.st_uid,
            stat.st_mode & 0o7777
        )
        .into());
    }
    Ok(dir)
}

fn open_data(data: &Path) -> Result<Dir> {
    Dir::open(data).map_err(|e| failed(data, &e))
}

/// The user that owns the data directory: the service user.
pub(super) fn data_owner(data: &Path) -> Result<u32> {
    Ok(open_data(data)?.stat_self()?.st_uid)
}

/// Database files (and their WAL companions) directly in the data directory and in `bots/<id>/`
/// (knowledge stores). Links and anything that is not a regular file are refused, never followed.
fn database_files(data: &Dir) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let mut directories = vec![PathBuf::new()];
    match data.sub(Path::new("bots")) {
        Ok(bots) => {
            for name in bots.names()? {
                let path = Path::new("bots").join(&name);
                match bots.stat(&name) {
                    Ok(stat) if fd_dir::is_dir(&stat) => directories.push(path),
                    // A bot's directory that leads somewhere else is not a store to copy or restore.
                    Ok(stat) if fd_dir::is_symlink(&stat) => {
                        return Err(
                            format!("{} is a link; refusing to follow it", path.display()).into(),
                        );
                    }
                    Ok(_) => {}
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(failed(&path, &e)),
                }
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(failed(Path::new("bots"), &e)),
    }
    for directory in directories {
        let handle = if directory.as_os_str().is_empty() {
            data.try_clone()?
        } else {
            match data.sub(&directory) {
                Ok(handle) => handle,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(failed(&directory, &e)),
            }
        };
        for name in handle.names()? {
            if !name.to_str().is_some_and(is_database) {
                continue;
            }
            let path = directory.join(&name);
            if !fd_dir::is_regular_file(&handle.stat(&name)?) {
                return Err(format!(
                    "{} is not a regular file; refusing to copy it",
                    path.display()
                )
                .into());
            }
            found.push(path);
        }
    }
    found.sort();
    Ok(found)
}

/// Open a database for reading and check what was opened (not what the path said a moment ago): a
/// regular file with a single name. A second name could be a hard link to a file the service user
/// must not read through a root-made copy.
fn open_database(data: &Dir, path: &Path) -> Result<(File, Metadata)> {
    let file = data.open_read(path).map_err(|e| failed(path, &e))?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(format!(
            "{} is not a regular file; refusing to copy it",
            path.display()
        )
        .into());
    }
    if meta.nlink() != 1 {
        return Err(format!(
            "{} has more than one hard link; refusing to copy it",
            path.display()
        )
        .into());
    }
    Ok((file, meta))
}

fn available_bytes(dir: &Dir) -> Result<u64> {
    let stat = rustix::fs::fstatvfs(dir)?;
    Ok(stat.f_bavail.saturating_mul(stat.f_frsize))
}

/// Fails before anything is stopped when the copy would not fit on the disk, or when an earlier
/// rollback is still waiting to be finished.
pub(super) fn preflight(data: &Path, snapshot: &Path) -> Result<()> {
    let (parent, _) = split(snapshot)?;
    std::fs::create_dir_all(parent)?;
    let available = available_bytes(&private_directory(parent)?)?;
    preflight_with(data, snapshot, available)
}

fn preflight_with(data: &Path, snapshot: &Path, available: u64) -> Result<()> {
    let (parent, name) = split(snapshot)?;
    let data = open_data(data)?;
    let mut needed = 0_u64;
    for path in database_files(&data)? {
        needed = needed.saturating_add(open_database(&data, &path)?.1.len());
    }
    // The previous snapshot is replaced, so its space counts as free.
    let mut reusable = 0_u64;
    if let Ok(existing) = Dir::open(parent).and_then(|dir| dir.sub(Path::new(name))) {
        if existing.exists(OsStr::new(COMMIT))? {
            return Err(PENDING.into());
        }
        for entry in existing.names()? {
            if let Ok(stat) = existing.stat(&entry) {
                reusable = reusable.saturating_add(u64::try_from(stat.st_size).unwrap_or(0));
            }
        }
    }
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
    let (parent_path, name) = split(snapshot)?;
    let parent = private_directory(parent_path)?;
    let data = open_data(data)?;
    if pending(&parent, name)? {
        return Err(PENDING.into());
    }
    let (staging_name, staging) = parent.mkdir_unique(".db-snapshot-", 0o700)?;
    if let Err(error) = copy_databases(&data, &staging) {
        drop(staging);
        let _ = parent.remove_tree(&staging_name);
        return Err(error);
    }
    drop(staging);
    parent.remove_tree(name)?;
    parent.rename(&staging_name, &parent, name)?;
    parent.sync()?;
    Ok(())
}

fn copy_databases(data: &Dir, staging: &Dir) -> Result<()> {
    copy_listed(data, staging, database_files(data)?)
}

/// Copy the listed databases into `staging`. The list is only a list of names: each one is opened
/// again, without following links, and what was opened is what is checked and copied.
fn copy_listed(data: &Dir, staging: &Dir, paths: Vec<PathBuf>) -> Result<()> {
    let mut files = Vec::new();
    for path in paths {
        let (source, before) = open_database(data, &path)?;
        let mut copy = staging.create_file(OsStr::new(&snapshot_name(&path)), 0o600)?;
        let copied = io::copy(&mut &source, &mut copy)?;
        copy.sync_all()?;
        if !unchanged(&before, &source.metadata()?, copied) {
            return Err(format!(
                "{} changed while it was copied; a process of the service user is still running",
                path.display()
            )
            .into());
        }
        files.push(Entry {
            path,
            uid: before.uid(),
            gid: before.gid(),
            mode: before.mode() & 0o7777,
            len: before.len(),
        });
    }
    // The manifest comes last: a snapshot without one is not complete and is never restored.
    let mut manifest = staging.create_file(OsStr::new(MANIFEST), 0o600)?;
    manifest.write_all(&serde_json::to_vec(&Manifest { files })?)?;
    manifest.sync_all()?;
    staging.sync()?;
    Ok(())
}

/// The service is stopped: a file that changes while it is copied means something still runs.
fn unchanged(before: &Metadata, after: &Metadata, copied: u64) -> bool {
    copied == before.len()
        && after.len() == before.len()
        && after.mtime() == before.mtime()
        && after.mtime_nsec() == before.mtime_nsec()
}

/// Name of a data file inside the snapshot directory (one flat level; the path is in the manifest).
fn snapshot_name(path: &Path) -> String {
    path.to_string_lossy().replace('/', "__")
}

/// `path` from a manifest as the directory it is in and its file name: plain names only. A manifest
/// is root's own file, but a path that climbs out of the data directory is refused all the same.
fn plain_entry(path: &Path) -> Result<(PathBuf, OsString)> {
    let mut names = Vec::new();
    for component in path.components() {
        let Component::Normal(name) = component else {
            return Err(format!("invalid database path {}", path.display()).into());
        };
        names.push(name);
    }
    let name = names
        .pop()
        .ok_or_else(|| format!("invalid database path {}", path.display()))?;
    Ok((names.iter().collect(), name.to_owned()))
}

fn staged_name(name: &OsStr) -> OsString {
    let mut staged = name.to_owned();
    staged.push(STAGED);
    staged
}

/// The directories the databases of a manifest are in, each opened once. Everything that follows
/// happens through these descriptors: swapping a directory for a link afterwards changes nothing.
struct Directories(BTreeMap<PathBuf, Dir>);

impl Directories {
    fn open(data: &Dir, manifest: &Manifest) -> Result<Self> {
        let mut opened = BTreeMap::new();
        for entry in &manifest.files {
            let (directory, _) = plain_entry(&entry.path)?;
            if opened.contains_key(&directory) {
                continue;
            }
            let handle = if directory.as_os_str().is_empty() {
                data.try_clone()?
            } else {
                data.sub(&directory).map_err(|e| failed(&directory, &e))?
            };
            opened.insert(directory, handle);
        }
        Ok(Self(opened))
    }

    fn get(&self, directory: &Path) -> Result<&Dir> {
        self.0
            .get(directory)
            .ok_or_else(|| format!("{} is not open", directory.display()).into())
    }

    fn sync(&self) -> Result<()> {
        for directory in self.0.values() {
            directory.sync()?;
        }
        Ok(())
    }
}

fn read_manifest(snapshot: &Dir) -> Result<Manifest> {
    let incomplete =
        |e: &dyn std::fmt::Display| format!("no complete database snapshot to restore ({e})");
    let file = snapshot
        .open_read(Path::new(MANIFEST))
        .map_err(|e| incomplete(&e))?;
    if !file.metadata()?.is_file() {
        return Err(incomplete(&"the manifest is not a file").into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_MANIFEST + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_MANIFEST {
        return Err(incomplete(&"the manifest is too large").into());
    }
    Ok(serde_json::from_slice(&bytes)?)
}

/// Whether the snapshot named `name` in `parent` is a restore that has been committed and not yet
/// finished.
fn pending(parent: &Dir, name: &OsStr) -> Result<bool> {
    match parent.sub(Path::new(name)) {
        Ok(snapshot) => Ok(snapshot.exists(OsStr::new(COMMIT))?),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(failed(Path::new(name), &e)),
    }
}

/// Whether an earlier restore was committed but killed before it finished (`restore` finishes it).
pub(super) fn restore_pending(snapshot: &Path) -> Result<bool> {
    let (parent, name) = split(snapshot)?;
    match Dir::open(parent) {
        Ok(parent) => pending(&parent, name),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(failed(parent, &e)),
    }
}

/// One database of a restore, ready to be staged: its entry, where it goes and the snapshot's copy.
struct Staging<'a> {
    entry: &'a Entry,
    directory: &'a Dir,
    name: OsString,
    copy: File,
}

/// Stage every database beside its target, under a new name. Nothing live is touched: a failure here
/// (a damaged copy, a full disk) leaves the data exactly as it was, and the staged files are removed.
fn stage(snapshot: &Dir, manifest: &Manifest, directories: &Directories) -> Result<Vec<Staged>> {
    // Everything is checked before anything is written.
    let mut pending = Vec::new();
    for entry in &manifest.files {
        let (directory, name) = plain_entry(&entry.path)?;
        let copy = snapshot
            .open_read(Path::new(&snapshot_name(&entry.path)))
            .map_err(|e| failed(&entry.path, &e))?;
        let meta = copy.metadata()?;
        if !meta.is_file() || meta.len() != entry.len {
            return Err(format!("the snapshot of {} is damaged", entry.path.display()).into());
        }
        pending.push(Staging {
            entry,
            directory: directories.get(&directory)?,
            name,
            copy,
        });
    }
    let staged = || -> Result<Vec<Staged>> {
        let mut staged = Vec::new();
        for item in &pending {
            let temporary = staged_name(&item.name);
            // Left by a run that was interrupted: removed by name (a link is removed, not followed).
            item.directory.unlink_if_exists(&temporary)?;
            let mut file = item.directory.create_file(&temporary, 0o600)?;
            let copied = io::copy(&mut &item.copy, &mut file)?;
            // On the descriptor: the file is root's until this is done, and no path leads to it.
            std::os::unix::fs::fchown(&file, Some(item.entry.uid), Some(item.entry.gid))?;
            file.set_permissions(std::fs::Permissions::from_mode(item.entry.mode))?;
            file.sync_all()?;
            let meta = file.metadata()?;
            if copied != item.entry.len || meta.len() != item.entry.len {
                return Err(format!("short copy of {}", item.entry.path.display()).into());
            }
            staged.push(Staged {
                path: item.entry.path.clone(),
                inode: meta.ino(),
            });
        }
        directories.sync()?;
        Ok(staged)
    }();
    if staged.is_err() {
        for item in &pending {
            let _ = item.directory.unlink_if_exists(&staged_name(&item.name));
        }
    }
    staged
}

/// Commit the restore: from the moment this returns, it only goes forward. The marker is made whole
/// before it appears under its name.
fn mark_committed(snapshot: &Dir, committed: &Committed) -> Result<()> {
    snapshot.unlink_if_exists(OsStr::new(COMMIT_NEW))?;
    let mut marker = snapshot.create_file(OsStr::new(COMMIT_NEW), 0o600)?;
    marker.write_all(&serde_json::to_vec(committed)?)?;
    marker.sync_all()?;
    snapshot.rename(OsStr::new(COMMIT_NEW), snapshot, OsStr::new(COMMIT))?;
    snapshot.sync()?;
    Ok(())
}

fn read_committed(snapshot: &Dir) -> Result<Option<Committed>> {
    let file = match snapshot.open_read(Path::new(COMMIT)) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(failed(Path::new(COMMIT), &e)),
    };
    let mut bytes = Vec::new();
    file.take(MAX_MANIFEST + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_MANIFEST {
        return Err("the rollback marker is damaged".into());
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| format!("the rollback marker is damaged ({e})").into())
}

/// Replace the live databases by the staged ones. Idempotent, so that it can be run again after it
/// was killed: a name already moved is simply gone from the staged side.
fn commit(
    snapshot: &Dir,
    manifest: &Manifest,
    directories: &Directories,
    committed: &Committed,
) -> Result<()> {
    // A write-ahead log (or shared-memory file) left by the release being rolled back is removed
    // first: applied to an older database file it would corrupt it. A log the snapshot itself holds
    // is replaced below instead, so it is not removed here (and a repeated run never removes it).
    for entry in &manifest.files {
        let (directory, name) = plain_entry(&entry.path)?;
        let Some(base) = name.to_str().and_then(|n| n.strip_suffix(".db")) else {
            continue;
        };
        for suffix in ["-wal", "-shm"] {
            let companion = format!("{base}.db{suffix}");
            if !manifest
                .files
                .iter()
                .any(|other| other.path == directory.join(&companion))
            {
                directories
                    .get(&directory)?
                    .unlink_if_exists(OsStr::new(&companion))?;
            }
        }
    }
    for entry in &manifest.files {
        let (directory, name) = plain_entry(&entry.path)?;
        let handle = directories.get(&directory)?;
        match handle.rename(&staged_name(&name), handle, &name) {
            Ok(()) => {}
            // Moved by the run that was interrupted; verified below.
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(failed(&entry.path, &e)),
        }
    }
    directories.sync()?;
    // What is in place now must be the staged file itself, not a database of the new release that
    // happens to be as long (the staged file of an interrupted run is gone once it was renamed).
    for entry in &manifest.files {
        let (directory, name) = plain_entry(&entry.path)?;
        let expected = committed
            .staged
            .iter()
            .find(|staged| staged.path == entry.path)
            .map(|staged| staged.inode);
        let stat = directories.get(&directory)?.stat(&name);
        if !stat.is_ok_and(|stat| {
            fd_dir::is_regular_file(&stat)
                && u64::try_from(stat.st_size) == Ok(entry.len)
                && Some(stat.st_ino) == expected
        }) {
            return Err(format!("{} was not restored completely", entry.path.display()).into());
        }
    }
    snapshot.unlink_if_exists(OsStr::new(COMMIT))?;
    snapshot.sync()?;
    Ok(())
}

/// Put the snapshot's databases back, with their owner and mode: all of them or, when something
/// fails before the commit, none. A write-ahead log left by the new release is removed with them.
pub(super) fn restore(data: &Path, snapshot: &Path) -> Result<()> {
    let (parent_path, name) = split(snapshot)?;
    let parent = private_directory(parent_path)?;
    let snapshot = parent
        .sub(Path::new(name))
        .map_err(|e| format!("no complete database snapshot to restore ({e})"))?;
    let manifest = read_manifest(&snapshot)?;
    let data = open_data(data)?;
    let directories = Directories::open(&data, &manifest)?;
    let committed = begin(&snapshot, &manifest, &directories)?;
    commit(&snapshot, &manifest, &directories, &committed)
}

/// Stage everything and commit, unless an earlier run already did (then its marker is the plan).
fn begin(snapshot: &Dir, manifest: &Manifest, directories: &Directories) -> Result<Committed> {
    if let Some(committed) = read_committed(snapshot)? {
        return Ok(committed);
    }
    let committed = Committed {
        staged: stage(snapshot, manifest, directories)?,
    };
    // From here the restore only goes forward, also after a crash.
    mark_committed(snapshot, &committed)?;
    Ok(committed)
}

/// A restore that is killed right after its commit, for tests (here and in the updater): everything
/// is staged and marked, nothing live has been replaced yet.
#[cfg(test)]
pub(super) fn interrupt_after_commit(data: &Path, snapshot: &Path) -> Result<()> {
    let (parent_path, name) = split(snapshot)?;
    let snapshot = private_directory(parent_path)?.sub(Path::new(name))?;
    let manifest = read_manifest(&snapshot)?;
    let directories = Directories::open(&open_data(data)?, &manifest)?;
    begin(&snapshot, &manifest, &directories).map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;

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

    /// Where snapshots go: the updater's own state directory (a test machine's umask may be looser).
    fn private_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        dir
    }

    fn files(dir: &Path) -> Vec<PathBuf> {
        database_files(&Dir::open(dir).unwrap()).unwrap()
    }

    /// What the new release does to the data: migrates, adds a log, adds a database.
    fn migrate(dir: &Path) {
        fs::write(dir.join("bot.db"), "chats v2 (migrated)").unwrap();
        fs::write(dir.join("bot.db-wal"), "wal of the new release").unwrap();
        fs::write(dir.join("bot.db-shm"), "shm").unwrap();
        fs::write(dir.join("bots/b1/knowledge.db"), "knowledge v2").unwrap();
        fs::write(dir.join("memory.db"), "memory v2").unwrap();
        fs::write(dir.join("created-by-new.db"), "new").unwrap();
    }

    fn contents(dir: &Path) -> Vec<String> {
        ["bot.db", "memory.db", "bots/b1/knowledge.db"]
            .map(|file| fs::read_to_string(dir.join(file)).unwrap_or_default())
            .to_vec()
    }

    fn v1() -> Vec<String> {
        vec!["chats v1".into(), "memory v1".into(), "knowledge v1".into()]
    }

    fn v2() -> Vec<String> {
        vec![
            "chats v2 (migrated)".into(),
            "memory v2".into(),
            "knowledge v2".into(),
        ]
    }

    /// No staged file and no commit marker anywhere.
    fn assert_clean(data: &Path, snapshot: &Path) {
        for dir in [data.to_owned(), data.join("bots/b1")] {
            assert!(
                fs::read_dir(dir)
                    .unwrap()
                    .flatten()
                    .all(|entry| !entry.file_name().to_string_lossy().ends_with(STAGED)),
                "a staged file was left behind"
            );
        }
        assert!(!snapshot.join(COMMIT).exists());
    }

    #[test]
    fn finds_databases_and_their_logs_but_nothing_else() {
        let dir = data_dir();
        fs::write(dir.path().join("bot.db-wal"), "wal").unwrap();
        fs::write(dir.path().join("bot.db-shm"), "shm").unwrap();
        assert_eq!(
            files(dir.path()),
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
        let snapshots = private_dir();
        let snapshot = snapshots.path().join("snapshot");
        preflight(dir.path(), &snapshot).unwrap();
        take(dir.path(), &snapshot).unwrap();
        migrate(dir.path());
        restore(dir.path(), &snapshot).unwrap();
        assert_eq!(contents(dir.path()), v1());
        assert!(!dir.path().join("bot.db-wal").exists());
        assert!(!dir.path().join("bot.db-shm").exists());
        // Files the snapshot knows nothing about are left alone.
        assert!(dir.path().join("created-by-new.db").exists());
        assert!(dir.path().join("backups/bot-v1.db").exists());
        // No temporary names are left behind in the data directory, and the restore is finished.
        assert_clean(dir.path(), &snapshot);
        // The snapshot is kept, and restoring again is harmless.
        migrate(dir.path());
        restore(dir.path(), &snapshot).unwrap();
        assert_eq!(contents(dir.path()), v1());
    }

    #[test]
    fn a_new_snapshot_replaces_the_old_one_and_keeps_owner_and_mode() {
        let dir = data_dir();
        fs::set_permissions(dir.path().join("bot.db"), fs::Permissions::from_mode(0o640)).unwrap();
        let snapshots = private_dir();
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
        let snapshots = private_dir();
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
        symlink(outside.path().join("secret.db"), dir.path().join("evil.db")).unwrap();
        let snapshots = private_dir();
        let error = take(dir.path(), &snapshots.path().join("snapshot")).unwrap_err();
        assert!(error.to_string().contains("not a regular file"));
        assert!(!snapshots.path().join("snapshot").exists());
        // The staging directory was removed too.
        assert_eq!(fs::read_dir(snapshots.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_bot_directory_that_is_a_link_is_refused() {
        let dir = data_dir();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("other.db"), "not ours").unwrap();
        symlink(outside.path(), dir.path().join("bots/b2")).unwrap();
        let snapshots = private_dir();
        let error = take(dir.path(), &snapshots.path().join("snapshot")).unwrap_err();
        assert!(error.to_string().contains("is a link"), "{error}");
        // So is a `bots` that is itself a link.
        fs::remove_file(dir.path().join("bots/b2")).unwrap();
        fs::remove_dir_all(dir.path().join("bots")).unwrap();
        symlink(outside.path(), dir.path().join("bots")).unwrap();
        let error = take(dir.path(), &snapshots.path().join("snapshot")).unwrap_err();
        assert!(error.to_string().contains("refusing to follow"), "{error}");
        assert!(!snapshots.path().join("snapshot").exists());
    }

    #[test]
    fn a_database_swapped_for_a_link_after_the_listing_is_not_copied() {
        // The listing is made, then the service user swaps a file for a link to a root-only file.
        let dir = data_dir();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("shadow"), "root only secret").unwrap();
        let data = Dir::open(dir.path()).unwrap();
        let listed = database_files(&data).unwrap();
        fs::remove_file(dir.path().join("bots/b1/knowledge.db")).unwrap();
        symlink(
            outside.path().join("shadow"),
            dir.path().join("bots/b1/knowledge.db"),
        )
        .unwrap();
        let snapshots = private_dir();
        let staging = Dir::open(snapshots.path()).unwrap();
        let error = copy_listed(&data, &staging, listed)
            .unwrap_err()
            .to_string();
        assert!(error.contains("refusing to follow"), "{error}");
        for entry in fs::read_dir(snapshots.path()).unwrap().flatten() {
            let copy = fs::read_to_string(entry.path()).unwrap_or_default();
            assert!(
                !copy.contains("secret"),
                "{:?} holds the target",
                entry.path()
            );
        }
        // The same for a directory swapped after the listing.
        let dir = data_dir();
        let data = Dir::open(dir.path()).unwrap();
        let listed = database_files(&data).unwrap();
        fs::remove_dir_all(dir.path().join("bots/b1")).unwrap();
        fs::write(outside.path().join("knowledge.db"), "root only secret").unwrap();
        symlink(outside.path(), dir.path().join("bots/b1")).unwrap();
        let snapshots = private_dir();
        let staging = Dir::open(snapshots.path()).unwrap();
        assert!(copy_listed(&data, &staging, listed).is_err());
        assert!(
            fs::read_dir(snapshots.path())
                .unwrap()
                .flatten()
                .all(|entry| !fs::read_to_string(entry.path())
                    .unwrap_or_default()
                    .contains("secret"))
        );
    }

    #[test]
    fn a_database_that_changes_while_it_is_copied_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("bot.db");
        fs::write(&file, "chats v1").unwrap();
        let before = fs::metadata(&file).unwrap();
        assert!(unchanged(
            &before,
            &fs::metadata(&file).unwrap(),
            before.len()
        ));
        // Appended to (a process still runs), or the copy came out short.
        fs::write(&file, "chats v1 and more").unwrap();
        let after = fs::metadata(&file).unwrap();
        assert!(!unchanged(&before, &after, before.len()));
        assert!(!unchanged(&after, &after, after.len() - 1));
        // Same length, rewritten: the modification time still gives it away.
        let same_length = dir.path().join("memory.db");
        fs::write(&same_length, "memory v1").unwrap();
        let before = fs::metadata(&same_length).unwrap();
        fs::File::options()
            .write(true)
            .open(&same_length)
            .unwrap()
            .set_modified(before.modified().unwrap() + std::time::Duration::from_secs(5))
            .unwrap();
        assert!(!unchanged(
            &before,
            &fs::metadata(&same_length).unwrap(),
            before.len()
        ));
    }

    #[test]
    fn a_hard_linked_database_is_refused() {
        let dir = data_dir();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("shadow"), "root only secret").unwrap();
        fs::hard_link(outside.path().join("shadow"), dir.path().join("evil.db")).unwrap();
        let snapshots = private_dir();
        let error = take(dir.path(), &snapshots.path().join("snapshot"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("more than one hard link"), "{error}");
        assert!(preflight(dir.path(), &snapshots.path().join("snapshot")).is_err());
    }

    #[test]
    fn a_pipe_in_place_of_a_database_neither_blocks_nor_is_copied() {
        let dir = data_dir();
        rustix::fs::mknodat(
            rustix::fs::CWD,
            dir.path().join("pipe.db"),
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::from_raw_mode(0o600),
            0,
        )
        .unwrap();
        let snapshots = private_dir();
        let error = take(dir.path(), &snapshots.path().join("snapshot")).unwrap_err();
        assert!(error.to_string().contains("not a regular file"), "{error}");
    }

    #[test]
    fn a_directory_replaced_by_a_link_is_never_restored_through() {
        let dir = data_dir();
        let snapshots = private_dir();
        let snapshot = snapshots.path().join("snapshot");
        take(dir.path(), &snapshot).unwrap();
        // The release that failed (or anything running as the daemon's user) swaps a bot's
        // directory for a link to somewhere only root can write.
        let outside = tempfile::tempdir().unwrap();
        fs::remove_dir_all(dir.path().join("bots/b1")).unwrap();
        symlink(outside.path(), dir.path().join("bots/b1")).unwrap();
        fs::write(dir.path().join("bot.db"), "newer").unwrap();
        let error = restore(dir.path(), &snapshot).unwrap_err();
        assert!(error.to_string().contains("refusing to follow"), "{error}");
        // Nothing was written through the link, and nothing else was touched either.
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
        assert_eq!(
            fs::read_to_string(dir.path().join("bot.db")).unwrap(),
            "newer"
        );
        assert_clean(dir.path(), &snapshot);
        // The same holds when the `bots` directory itself became a link.
        fs::remove_file(dir.path().join("bots/b1")).unwrap_or_default();
        fs::remove_file(dir.path().join("bots")).unwrap_or_default();
        fs::remove_dir_all(dir.path().join("bots")).unwrap_or_default();
        symlink(outside.path(), dir.path().join("bots")).unwrap();
        assert!(restore(dir.path(), &snapshot).is_err());
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_directory_swapped_for_a_link_after_it_was_opened_is_not_restored_through() {
        // The race itself: the directories are opened and checked, then one is swapped for a link
        // before anything is written. The writes must stay in the directory that was opened.
        let dir = data_dir();
        let snapshots = private_dir();
        let snapshot_path = snapshots.path().join("snapshot");
        take(dir.path(), &snapshot_path).unwrap();
        migrate(dir.path());
        let data = Dir::open(dir.path()).unwrap();
        let snapshot = Dir::open(&snapshot_path).unwrap();
        let manifest = read_manifest(&snapshot).unwrap();
        let directories = Directories::open(&data, &manifest).unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::rename(dir.path().join("bots/b1"), dir.path().join("bots/moved")).unwrap();
        symlink(outside.path(), dir.path().join("bots/b1")).unwrap();
        let committed = begin(&snapshot, &manifest, &directories).unwrap();
        commit(&snapshot, &manifest, &directories, &committed).unwrap();
        assert_eq!(
            fs::read_dir(outside.path()).unwrap().count(),
            0,
            "root wrote through the link"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("bots/moved/knowledge.db")).unwrap(),
            "knowledge v1"
        );
    }

    #[test]
    fn a_staged_name_planted_as_a_link_is_replaced_not_written_through() {
        let dir = data_dir();
        let snapshots = private_dir();
        let snapshot = snapshots.path().join("snapshot");
        take(dir.path(), &snapshot).unwrap();
        migrate(dir.path());
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("victim"), "keep me").unwrap();
        symlink(
            outside.path().join("victim"),
            dir.path().join("memory.db.restore-new"),
        )
        .unwrap();
        symlink(
            outside.path().join("fresh"),
            dir.path().join("bot.db.restore-new"),
        )
        .unwrap();
        restore(dir.path(), &snapshot).unwrap();
        assert_eq!(contents(dir.path()), v1());
        assert_eq!(
            fs::read_to_string(outside.path().join("victim")).unwrap(),
            "keep me"
        );
        assert!(!outside.path().join("fresh").exists());
        assert_clean(dir.path(), &snapshot);
    }

    #[test]
    fn a_path_that_climbs_out_of_the_data_directory_is_refused() {
        for relative in ["../etc/shadow.db", "/etc/shadow.db", "bots/../../x.db", ""] {
            let error = plain_entry(Path::new(relative)).unwrap_err().to_string();
            assert!(
                error.contains("invalid database path"),
                "{relative}: {error}"
            );
        }
        assert_eq!(
            plain_entry(Path::new("bots/b1/knowledge.db")).unwrap(),
            (PathBuf::from("bots/b1"), OsString::from("knowledge.db"))
        );
        // A file directly in the data directory is in no directory at all.
        assert_eq!(
            plain_entry(Path::new("bot.db")).unwrap(),
            (PathBuf::new(), OsString::from("bot.db"))
        );
        // And a manifest cannot make a restore leave the data directory.
        let dir = data_dir();
        let snapshots = private_dir();
        let snapshot = snapshots.path().join("snapshot");
        take(dir.path(), &snapshot).unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(
            snapshot.join(MANIFEST),
            serde_json::to_vec(&Manifest {
                files: vec![Entry {
                    path: outside.path().join("x.db"),
                    uid: 0,
                    gid: 0,
                    mode: 0o600,
                    len: 1,
                }],
            })
            .unwrap(),
        )
        .unwrap();
        assert!(restore(dir.path(), &snapshot).is_err());
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_failure_while_staging_leaves_every_database_as_it_was() {
        // Atomicity before the commit: the first databases staged fine, a later one cannot be (here
        // a directory sits where its staged file goes). No live database may have changed.
        let dir = data_dir();
        let snapshots = private_dir();
        let snapshot = snapshots.path().join("snapshot");
        take(dir.path(), &snapshot).unwrap();
        migrate(dir.path());
        fs::create_dir(dir.path().join("memory.db.restore-new")).unwrap();
        assert!(restore(dir.path(), &snapshot).is_err());
        assert_eq!(contents(dir.path()), v2(), "a half restored mixture");
        assert!(
            dir.path().join("bot.db-wal").exists(),
            "the log was removed too early"
        );
        assert!(
            !snapshot.join(COMMIT).exists(),
            "committed without a complete staging"
        );
        for staged in ["bot.db.restore-new", "bots/b1/knowledge.db.restore-new"] {
            assert!(
                !dir.path().join(staged).exists(),
                "{staged} was left behind"
            );
        }
        // Once the obstacle is gone the same restore goes through.
        fs::remove_dir(dir.path().join("memory.db.restore-new")).unwrap();
        restore(dir.path(), &snapshot).unwrap();
        assert_eq!(contents(dir.path()), v1());
    }

    #[test]
    fn a_restore_killed_after_the_commit_is_finished_not_left_mixed() {
        let dir = data_dir();
        let snapshots = private_dir();
        let snapshot_path = snapshots.path().join("snapshot");
        take(dir.path(), &snapshot_path).unwrap();
        migrate(dir.path());
        // Everything staged and committed; the process dies after the first rename.
        interrupt_after_commit(dir.path(), &snapshot_path).unwrap();
        assert_eq!(
            contents(dir.path()),
            v2(),
            "staging must not touch live data"
        );
        fs::rename(
            dir.path().join("bot.db.restore-new"),
            dir.path().join("bot.db"),
        )
        .unwrap();
        // Two generations are on disk now.
        assert_eq!(contents(dir.path())[0], "chats v1");
        assert_eq!(contents(dir.path())[1], "memory v2");
        assert!(restore_pending(&snapshot_path).unwrap());
        // Nothing else may start from here (or take a new snapshot over the unfinished one).
        let error = take(dir.path(), &snapshot_path).unwrap_err();
        assert!(error.to_string().contains("interrupted"), "{error}");
        let error = preflight(dir.path(), &snapshot_path).unwrap_err();
        assert!(error.to_string().contains("interrupted"), "{error}");
        // The restore rolls forward from the marker instead of staging again.
        restore(dir.path(), &snapshot_path).unwrap();
        assert_eq!(contents(dir.path()), v1());
        assert!(!dir.path().join("bot.db-wal").exists());
        assert_clean(dir.path(), &snapshot_path);
        assert!(!restore_pending(&snapshot_path).unwrap());
        // And the snapshot can be replaced again.
        take(dir.path(), &snapshot_path).unwrap();
    }

    #[test]
    fn a_damaged_or_half_written_marker_is_never_taken_for_a_commit() {
        let dir = data_dir();
        let snapshots = private_dir();
        let snapshot_path = snapshots.path().join("snapshot");
        take(dir.path(), &snapshot_path).unwrap();
        migrate(dir.path());
        // A marker that is not whole (it is renamed into place only when it is) is refused, and
        // nothing is replaced on the strength of it.
        fs::write(snapshot_path.join(COMMIT), "{\"staged\":[").unwrap();
        let error = restore(dir.path(), &snapshot_path).unwrap_err();
        assert!(error.to_string().contains("marker is damaged"), "{error}");
        assert_eq!(contents(dir.path()), v2());
        // One that is merely not finished: a leftover temporary marker does not make a commit.
        fs::remove_file(snapshot_path.join(COMMIT)).unwrap();
        fs::write(snapshot_path.join(COMMIT_NEW), "{\"staged\":[").unwrap();
        assert!(!restore_pending(&snapshot_path).unwrap());
        restore(dir.path(), &snapshot_path).unwrap();
        assert_eq!(contents(dir.path()), v1());
    }

    #[test]
    fn finishing_a_commit_again_never_removes_a_log_the_snapshot_holds() {
        // A snapshot that holds a write-ahead log (a stop that did not fold it in) restores it, and
        // running the commit step twice must not delete it the second time.
        let dir = data_dir();
        fs::write(dir.path().join("bot.db-wal"), "wal v1").unwrap();
        let snapshots = private_dir();
        let snapshot_path = snapshots.path().join("snapshot");
        take(dir.path(), &snapshot_path).unwrap();
        migrate(dir.path());
        restore(dir.path(), &snapshot_path).unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("bot.db-wal")).unwrap(),
            "wal v1"
        );
        assert!(!dir.path().join("bot.db-shm").exists());
        let data = Dir::open(dir.path()).unwrap();
        let snapshot = Dir::open(&snapshot_path).unwrap();
        let manifest = read_manifest(&snapshot).unwrap();
        let directories = Directories::open(&data, &manifest).unwrap();
        // The second run of the commit step, as after a kill: the files in place are the staged ones.
        let committed = Committed {
            staged: manifest
                .files
                .iter()
                .map(|entry| Staged {
                    path: entry.path.clone(),
                    inode: fs::metadata(dir.path().join(&entry.path)).unwrap().ino(),
                })
                .collect(),
        };
        commit(&snapshot, &manifest, &directories, &committed).unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("bot.db-wal")).unwrap(),
            "wal v1"
        );
    }

    #[test]
    fn a_restore_that_lost_a_staged_file_is_reported_not_called_done() {
        // Committed, but a staged file vanished and its database is not what the snapshot holds.
        let dir = data_dir();
        let snapshots = private_dir();
        let snapshot_path = snapshots.path().join("snapshot");
        take(dir.path(), &snapshot_path).unwrap();
        migrate(dir.path());
        let data = Dir::open(dir.path()).unwrap();
        let snapshot = Dir::open(&snapshot_path).unwrap();
        let manifest = read_manifest(&snapshot).unwrap();
        let directories = Directories::open(&data, &manifest).unwrap();
        let committed = begin(&snapshot, &manifest, &directories).unwrap();
        // `memory v2` is as long as `memory v1`: only the identity of the file tells them apart.
        fs::remove_file(dir.path().join("memory.db.restore-new")).unwrap();
        let error = commit(&snapshot, &manifest, &directories, &committed).unwrap_err();
        assert!(
            error.to_string().contains("not restored completely"),
            "{error}"
        );
        // The marker stays: this restore is not finished.
        assert!(snapshot.exists(OsStr::new(COMMIT)).unwrap());
    }

    #[test]
    fn the_snapshot_directory_must_be_the_updaters_and_closed_to_others() {
        let dir = data_dir();
        let snapshots = private_dir();
        let snapshot = snapshots.path().join("snapshot");
        take(dir.path(), &snapshot).unwrap();
        fs::set_permissions(snapshots.path(), fs::Permissions::from_mode(0o777)).unwrap();
        for result in [
            take(dir.path(), &snapshot),
            restore(dir.path(), &snapshot),
            preflight(dir.path(), &snapshot),
        ] {
            let error = result.unwrap_err().to_string();
            assert!(error.contains("not be writable by others"), "{error}");
        }
        fs::set_permissions(snapshots.path(), fs::Permissions::from_mode(0o700)).unwrap();
        // And it is no link either.
        let real = tempfile::tempdir().unwrap();
        let link = snapshots.path().join("link");
        symlink(real.path(), &link).unwrap();
        let error = take(dir.path(), &link.join("snapshot")).unwrap_err();
        assert!(error.to_string().contains("refusing to follow"), "{error}");
        assert_eq!(fs::read_dir(real.path()).unwrap().count(), 0);
    }

    #[test]
    fn the_data_owner_is_read_from_the_directory_itself() {
        let dir = data_dir();
        assert_eq!(
            data_owner(dir.path()).unwrap(),
            rustix::process::geteuid().as_raw()
        );
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), dir.path().join("link")).unwrap();
        assert!(data_owner(&dir.path().join("link")).is_err());
    }

    #[test]
    fn preflight_refuses_a_copy_that_cannot_fit() {
        let dir = data_dir();
        let snapshots = private_dir();
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
