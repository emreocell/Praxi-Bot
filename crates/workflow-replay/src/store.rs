//! Saved workflows: `<data_dir>/workflows/<name>.toml` plus `index.json`.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use parking_lot::{Mutex, MutexGuard};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{StoreError, WorkflowError};
use crate::format::{Workflow, valid_name};

const INDEX_FILE: &str = "index.json";
const INDEX_VERSION: u32 = 1;
const DIRTY_FILE: &str = ".index-dirty";

/// How a workflow may be written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveMode {
    /// Refuse to replace an existing directory entry, including a symlink.
    CreateNew,
    /// Replace a workflow, optionally comparing its current store revision.
    Overwrite {
        /// Expected `Workflow::revision` (not its format `version`). Zero means
        /// the workflow must not exist; `None` retains legacy overwrite behavior.
        expected_version: Option<u64>,
    },
}

/// Errors from an explicitly controlled save. Kept separate from the existing
/// `StoreError` so existing store callers and error matches remain compatible.
#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    /// A create-only save would overwrite an existing entry.
    #[error("workflow already exists: {0}")]
    AlreadyExists(String),
    /// Another writer changed the workflow since it was read.
    #[error("workflow {name} revision conflict (current revision {current_version})")]
    Conflict {
        /// Workflow name.
        name: String,
        /// Current store revision, or zero when the workflow is absent.
        current_version: u64,
    },
    /// Incrementing the revision would exceed TOML's signed integer range.
    #[error("workflow revision exhausted: {0}")]
    RevisionExhausted(String),
    /// Validation, serialization, or disk error.
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl From<SaveError> for StoreError {
    fn from(error: SaveError) -> Self {
        match error {
            SaveError::Store(error) => error,
            error => Self::Io(std::io::Error::other(error)),
        }
    }
}

/// One line of the index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexEntry {
    /// Workflow name.
    pub name: String,
    /// Description.
    pub description: String,
    /// Number of steps.
    pub steps: usize,
    /// Parameter names.
    pub params: Vec<String>,
    /// File modification time, Unix milliseconds.
    pub updated_at_ms: i64,
}

#[derive(Serialize, Deserialize)]
struct Index {
    #[serde(default)]
    version: u32,
    workflows: Vec<IndexEntry>,
}

/// File-backed workflow store.
#[derive(Debug, Clone)]
pub struct WorkflowStore {
    dir: PathBuf,
    transaction: Arc<Mutex<()>>,
    #[cfg(test)]
    fail_before_index: Arc<std::sync::atomic::AtomicBool>,
}

impl WorkflowStore {
    /// A store under `<data_dir>/workflows` (created on first save).
    #[must_use]
    pub fn new(data_dir: &Path) -> Self {
        Self {
            dir: data_dir.join("workflows"),
            transaction: Arc::default(),
            #[cfg(test)]
            fail_before_index: Arc::default(),
        }
    }

    /// The directory holding the files.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, name: &str) -> Result<PathBuf, StoreError> {
        if !valid_name(name) {
            return Err(StoreError::BadName(name.to_owned()));
        }
        Ok(self.dir.join(format!("{name}.toml")))
    }

    fn lock(&self) -> Result<StoreGuard<'_>, StoreError> {
        let thread = self.transaction.lock();
        fs::create_dir_all(&self.dir)?;
        let path = self.dir.join(".store.lock");
        if let Ok(metadata) = fs::symlink_metadata(&path)
            && !metadata.is_file()
        {
            return Err(std::io::Error::other("workflow lock is not a regular file").into());
        }
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(path)?;
        fs4::FileExt::lock(&file)?;
        Ok(StoreGuard {
            _file: file,
            _thread: thread,
        })
    }

    /// Validate and save (overwrites a workflow of the same name).
    ///
    /// # Errors
    /// Invalid workflow or name, or a disk error.
    pub fn save(&self, wf: &Workflow) -> Result<(), StoreError> {
        self.save_with_mode(
            wf,
            SaveMode::Overwrite {
                expected_version: None,
            },
        )
        .map(|_| ())
        .map_err(StoreError::from)
    }

    /// Validate and save under one thread/process transaction. Returns the
    /// committed store revision; the caller's workflow is not mutated.
    ///
    /// New workflows start at revision 1. Overwrites increment the revision
    /// read from disk, regardless of the caller's proposed revision.
    ///
    /// # Errors
    /// [`SaveError::AlreadyExists`] for a name collision, [`SaveError::Conflict`]
    /// for a stale revision, or a validation/disk error. An index failure after
    /// publication leaves a durable dirty marker; the next list/rebuild repairs
    /// the index rather than returning a stale cached result.
    pub fn save_with_mode(&self, wf: &Workflow, mode: SaveMode) -> Result<u64, SaveError> {
        wf.validate().map_err(StoreError::from)?;
        let path = self.path(&wf.name)?;
        let _transaction = self.lock()?;
        self.cleanup_temps()?;
        let current_version = match fs::symlink_metadata(&path) {
            Ok(_) if mode == SaveMode::CreateNew => {
                return Err(SaveError::AlreadyExists(wf.name.clone()));
            }
            Ok(_) => self.get_locked(&wf.name)?.revision,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => return Err(StoreError::Io(error).into()),
        };
        if let SaveMode::Overwrite {
            expected_version: Some(expected),
        } = mode
            && expected != current_version
        {
            return Err(SaveError::Conflict {
                name: wf.name.clone(),
                current_version,
            });
        }
        let revision = current_version
            .checked_add(1)
            .filter(|revision| i64::try_from(*revision).is_ok())
            .ok_or_else(|| SaveError::RevisionExhausted(wf.name.clone()))?;
        let mut saved = wf.clone();
        saved.revision = revision;
        let text = saved.to_toml().map_err(StoreError::from)?;
        let pending = PendingWrite::new(&path, text.as_bytes()).map_err(StoreError::from)?;
        self.mark_dirty()?;
        match pending.commit(&path, mode == SaveMode::CreateNew) {
            Err(error)
                if mode == SaveMode::CreateNew
                    && error.kind() == std::io::ErrorKind::AlreadyExists =>
            {
                return Err(SaveError::AlreadyExists(wf.name.clone()));
            }
            result => result.map_err(StoreError::from)?,
        }
        #[cfg(test)]
        if self
            .fail_before_index
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(StoreError::Io(std::io::Error::other("injected index failure")).into());
        }
        self.rebuild_index_locked()?;
        Ok(revision)
    }

    /// Load by name.
    ///
    /// # Errors
    /// Unknown name, bad file or disk error.
    pub fn get(&self, name: &str) -> Result<Workflow, StoreError> {
        self.path(name)?;
        let _transaction = self.lock()?;
        self.get_locked(name)
    }

    fn get_locked(&self, name: &str) -> Result<Workflow, StoreError> {
        let path = self.path(name)?;
        match fs::symlink_metadata(&path) {
            Ok(metadata) if !metadata.is_file() => {
                return Err(std::io::Error::other("workflow is not a regular file").into());
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(StoreError::NotFound(name.to_owned()));
            }
            Err(e) => return Err(e.into()),
        }
        let workflow = Workflow::from_toml(&fs::read_to_string(path)?)?;
        if workflow.name != name || workflow.revision == 0 {
            return Err(WorkflowError::Invalid("stored name or revision is invalid".into()).into());
        }
        Ok(workflow)
    }

    /// Delete by name; `false` when it did not exist.
    ///
    /// # Errors
    /// Bad name or disk error.
    pub fn delete(&self, name: &str) -> Result<bool, StoreError> {
        let path = self.path(name)?;
        let _transaction = self.lock()?;
        self.cleanup_temps()?;
        self.mark_dirty()?;
        match fs::remove_file(&path) {
            Ok(()) => {
                sync_directory(&self.dir)?;
                self.rebuild_index_locked()?;
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.rebuild_index_locked()?;
                Ok(false)
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Index entries sorted by name (rebuilt from the files when the index is
    /// missing or unreadable).
    ///
    /// # Errors
    /// Disk error.
    pub fn list(&self) -> Result<Vec<IndexEntry>, StoreError> {
        let _transaction = self.lock()?;
        self.cleanup_temps()?;
        let index_path = self.dir.join(INDEX_FILE);
        if !self.is_dirty()?
            && fs::symlink_metadata(&index_path).is_ok_and(|metadata| metadata.is_file())
            && let Ok(text) = fs::read_to_string(&index_path)
            && let Ok(idx) = serde_json::from_str::<Index>(&text)
            && idx.version == INDEX_VERSION
        {
            return Ok(idx.workflows);
        }
        self.rebuild_index_locked()
    }

    /// Rescan the directory and rewrite the index. Unparseable files are skipped.
    ///
    /// # Errors
    /// Disk error.
    pub fn rebuild_index(&self) -> Result<Vec<IndexEntry>, StoreError> {
        let _transaction = self.lock()?;
        self.rebuild_index_locked()
    }

    fn rebuild_index_locked(&self) -> Result<Vec<IndexEntry>, StoreError> {
        self.cleanup_temps()?;
        let mut entries = Vec::new();
        let rd = match fs::read_dir(&self.dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(entries),
            Err(e) => return Err(e.into()),
        };
        for item in rd {
            let item = item?;
            let path = item.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml")
                || !item.file_type()?.is_file()
            {
                continue;
            }
            let text = match fs::read_to_string(&path) {
                Ok(text) => text,
                Err(error) if error.kind() == std::io::ErrorKind::InvalidData => continue,
                Err(error) => return Err(error.into()),
            };
            let Ok(wf) = Workflow::from_toml(&text) else {
                continue;
            };
            if path.file_stem().and_then(|s| s.to_str()) != Some(wf.name.as_str())
                || wf.revision == 0
            {
                continue;
            }
            let updated_at_ms = item
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .and_then(|d| i64::try_from(d.as_millis()).ok())
                .unwrap_or_default();
            entries.push(IndexEntry {
                name: wf.name.clone(),
                description: wf.description.clone(),
                steps: wf.steps.len(),
                params: wf.params.iter().map(|p| p.name.clone()).collect(),
                updated_at_ms,
            });
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        fs::create_dir_all(&self.dir)?;
        let text = serde_json::to_string_pretty(&Index {
            version: INDEX_VERSION,
            workflows: entries.clone(),
        })
        .map_err(|e| StoreError::Index(e.to_string()))?;
        let path = self.dir.join(INDEX_FILE);
        PendingWrite::new(&path, text.as_bytes())?.commit(&path, false)?;
        self.clear_dirty()?;
        Ok(entries)
    }

    fn mark_dirty(&self) -> Result<(), StoreError> {
        let path = self.dir.join(DIRTY_FILE);
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(file) => file.sync_all()?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if !fs::symlink_metadata(path)?.is_file() {
                    return Err(std::io::Error::other("invalid workflow dirty marker").into());
                }
            }
            Err(error) => return Err(error.into()),
        }
        sync_directory(&self.dir)?;
        Ok(())
    }

    fn is_dirty(&self) -> Result<bool, StoreError> {
        match fs::symlink_metadata(self.dir.join(DIRTY_FILE)) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    fn clear_dirty(&self) -> Result<(), StoreError> {
        match fs::remove_file(self.dir.join(DIRTY_FILE)) {
            Ok(()) => sync_directory(&self.dir)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    /// Called only while holding the process lock; no cooperating writer can
    /// still own one of these files. Never follow symlinks or remove directories.
    fn cleanup_temps(&self) -> Result<(), StoreError> {
        for item in fs::read_dir(&self.dir)? {
            let item = item?;
            let file_name = item.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            if owned_temp(name) && !item.file_type()?.is_dir() {
                fs::remove_file(item.path())?;
            }
        }
        Ok(())
    }
}

struct StoreGuard<'a> {
    _file: fs::File,
    _thread: MutexGuard<'a, ()>,
}

fn owned_temp(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".tmp") else {
        return false;
    };
    if stem.strip_suffix(".toml").is_some_and(valid_name) {
        return true; // Previous versions' shared workflow temporary file.
    }
    let Some((base, id)) = stem.rsplit_once('.') else {
        return false;
    };
    Uuid::parse_str(id).is_ok()
        && (base == INDEX_FILE || base.strip_suffix(".toml").is_some_and(valid_name))
}

fn sync_directory(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    fs::File::open(dir)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

struct PendingWrite {
    path: PathBuf,
    file: Option<fs::File>,
}

impl PendingWrite {
    fn new(destination: &Path, bytes: &[u8]) -> std::io::Result<Self> {
        let mut name = destination
            .file_name()
            .ok_or_else(|| std::io::Error::other("missing file name"))?
            .to_os_string();
        name.push(format!(".{}.tmp", Uuid::now_v7().simple()));
        let path = destination.with_file_name(name);
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&path)?;
        let mut pending = Self {
            path,
            file: Some(file),
        };
        let file = pending.file.as_mut().expect("temporary file is open");
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(pending)
    }

    fn commit(mut self, destination: &Path, create_new: bool) -> std::io::Result<()> {
        drop(self.file.take());
        if create_new {
            // An atomic no-clobber publish, including against writers that do
            // not honor our advisory lock. Both paths are on the same volume.
            fs::hard_link(&self.path, destination)?;
            fs::remove_file(&self.path)?;
        } else {
            fs::rename(&self.path, destination)?;
        }
        sync_directory(destination.parent().unwrap_or_else(|| Path::new(".")))
    }
}

impl Drop for PendingWrite {
    fn drop(&mut self) {
        drop(self.file.take());
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::{Launch, Step};

    fn wf(name: &str) -> Workflow {
        let mut w = Workflow::new(name);
        w.description = "d".into();
        w.steps.push(Step::Launch(Launch {
            executable: "x".into(),
            args: vec![],
        }));
        w
    }

    #[test]
    fn save_get_list_delete() {
        let d = tempfile::tempdir().unwrap();
        let s = WorkflowStore::new(d.path());
        assert!(s.list().unwrap().is_empty());
        s.save(&wf("b-one")).unwrap();
        s.save(&wf("a_two")).unwrap();
        let names: Vec<_> = s.list().unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["a_two", "b-one"]);
        assert_eq!(s.get("a_two").unwrap(), wf("a_two"));
        assert!(s.delete("a_two").unwrap());
        assert!(!s.delete("a_two").unwrap());
        assert!(matches!(s.get("a_two"), Err(StoreError::NotFound(_))));
        assert_eq!(s.list().unwrap().len(), 1);
    }

    #[test]
    fn rejects_path_traversal_names_and_survives_a_lost_index() {
        let d = tempfile::tempdir().unwrap();
        let s = WorkflowStore::new(d.path());
        assert!(matches!(s.get("../x"), Err(StoreError::BadName(_))));
        assert!(s.save(&wf("UPPER")).is_err());
        s.save(&wf("ok")).unwrap();
        fs::remove_file(s.dir().join(INDEX_FILE)).unwrap();
        assert_eq!(s.list().unwrap().len(), 1);
        fs::write(s.dir().join(INDEX_FILE), "garbage").unwrap();
        assert_eq!(s.list().unwrap().len(), 1);
    }

    #[test]
    fn task_145_regression_index_writes_never_mutate_an_existing_inode() {
        let dir = tempfile::tempdir().unwrap();
        let store = WorkflowStore::new(dir.path());
        store.save(&wf("first")).unwrap();
        let snapshot = dir.path().join("old-index.json");
        fs::hard_link(store.dir().join(INDEX_FILE), &snapshot).unwrap();
        let before = fs::read(&snapshot).unwrap();
        store.save(&wf("second")).unwrap();
        assert_eq!(fs::read(snapshot).unwrap(), before);
        assert_eq!(store.list().unwrap().len(), 2);
    }

    fn assert_no_owned_temps(store: &WorkflowStore) {
        assert!(
            fs::read_dir(store.dir())
                .unwrap()
                .all(|entry| { !owned_temp(&entry.unwrap().file_name().to_string_lossy()) })
        );
    }

    #[test]
    fn create_new_preserves_existing_workflow_and_legacy_save_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let store = WorkflowStore::new(dir.path());
        let first = wf("same");
        assert_eq!(
            store.save_with_mode(&first, SaveMode::CreateNew).unwrap(),
            1
        );
        let before = fs::read(store.path("same").unwrap()).unwrap();
        let index = fs::read(store.dir().join(INDEX_FILE)).unwrap();
        let mut second = first.clone();
        second.description = "different workflow".into();
        second.revision = 99;
        assert!(matches!(store.save_with_mode(&second, SaveMode::CreateNew),
            Err(SaveError::AlreadyExists(name)) if name == "same"));
        assert_eq!(fs::read(store.path("same").unwrap()).unwrap(), before);
        assert_eq!(fs::read(store.dir().join(INDEX_FILE)).unwrap(), index);
        store.save(&second).unwrap();
        let saved = store.get("same").unwrap();
        assert_eq!(saved.revision, 2);
        assert_eq!(saved.version, crate::format::FORMAT_VERSION);
        assert_eq!(saved.description, second.description);
    }

    #[test]
    fn sixteen_writers_keep_all_workflows_and_a_complete_index() {
        let dir = tempfile::tempdir().unwrap();
        let store = WorkflowStore::new(dir.path());
        let start = std::sync::Barrier::new(16);
        std::thread::scope(|threads| {
            for i in 0..16 {
                let writer = if i % 2 == 0 {
                    store.clone()
                } else {
                    WorkflowStore::new(dir.path())
                };
                let start = &start;
                threads.spawn(move || {
                    start.wait();
                    writer
                        .save_with_mode(&wf(&format!("flow-{i:02}")), SaveMode::CreateNew)
                        .unwrap();
                });
            }
        });
        let names: Vec<_> = store
            .list()
            .unwrap()
            .into_iter()
            .map(|item| item.name)
            .collect();
        let expected: Vec<_> = (0..16).map(|i| format!("flow-{i:02}")).collect();
        assert_eq!(names, expected);
        for name in names {
            assert_eq!(store.get(&name).unwrap().revision, 1);
        }
        assert_no_owned_temps(&store);
    }

    #[test]
    fn compare_and_swap_allows_exactly_one_writer_per_revision() {
        let dir = tempfile::tempdir().unwrap();
        let store = WorkflowStore::new(dir.path());
        store
            .save_with_mode(&wf("shared"), SaveMode::CreateNew)
            .unwrap();
        for expected in 1..=3 {
            let start = std::sync::Barrier::new(16);
            let outcomes = std::thread::scope(|threads| {
                let mut handles = Vec::new();
                for i in 0..16 {
                    let writer = WorkflowStore::new(dir.path());
                    let start = &start;
                    handles.push(threads.spawn(move || {
                        let mut candidate = wf("shared");
                        candidate.description = format!("writer-{i}");
                        start.wait();
                        writer.save_with_mode(
                            &candidate,
                            SaveMode::Overwrite {
                                expected_version: Some(expected),
                            },
                        )
                    }));
                }
                handles
                    .into_iter()
                    .map(|handle| handle.join().unwrap())
                    .collect::<Vec<_>>()
            });
            assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
            for result in outcomes {
                match result {
                    Ok(revision) => assert_eq!(revision, expected + 1),
                    Err(SaveError::Conflict {
                        name,
                        current_version,
                    }) => {
                        assert_eq!(name, "shared");
                        assert_eq!(current_version, expected + 1);
                    }
                    other => panic!("unexpected save result: {other:?}"),
                }
            }
            assert_eq!(store.get("shared").unwrap().revision, expected + 1);
        }
        assert_no_owned_temps(&store);
    }

    #[test]
    fn legacy_workflow_without_revision_loads_as_one_and_migrates_on_save() {
        let dir = tempfile::tempdir().unwrap();
        let store = WorkflowStore::new(dir.path());
        fs::create_dir_all(store.dir()).unwrap();
        fs::write(
            store.path("old").unwrap(),
            "version = 1\nname = \"old\"\nsteps = []\n",
        )
        .unwrap();
        let loaded = store.get("old").unwrap();
        assert_eq!(loaded.revision, 1);
        assert_eq!(loaded.version, 1);
        assert_eq!(
            store
                .save_with_mode(
                    &loaded,
                    SaveMode::Overwrite {
                        expected_version: Some(1)
                    }
                )
                .unwrap(),
            2
        );
        assert_eq!(store.get("old").unwrap().revision, 2);
    }

    #[test]
    fn crashed_writer_temps_are_cleaned_without_removing_unrelated_files() {
        let dir = tempfile::tempdir().unwrap();
        let store = WorkflowStore::new(dir.path());
        store.save(&wf("kept")).unwrap();
        let owned = [
            "old.toml.tmp".to_string(),
            format!("abandoned.toml.{}.tmp", Uuid::now_v7().simple()),
            format!("index.json.{}.tmp", Uuid::now_v7().simple()),
        ];
        for name in &owned {
            fs::write(store.dir().join(name), "partial write").unwrap();
        }
        fs::write(store.dir().join("unrelated.tmp"), "leave this alone").unwrap();
        let names: Vec<_> = store
            .list()
            .unwrap()
            .into_iter()
            .map(|item| item.name)
            .collect();
        assert_eq!(names, ["kept"]);
        for name in owned {
            assert!(!store.dir().join(name).exists());
        }
        assert_eq!(
            fs::read(store.dir().join("unrelated.tmp")).unwrap(),
            b"leave this alone"
        );
    }

    #[test]
    fn interrupted_index_commit_is_repaired_before_cached_list_is_returned() {
        let dir = tempfile::tempdir().unwrap();
        let store = WorkflowStore::new(dir.path());
        store.save(&wf("first")).unwrap();
        store
            .fail_before_index
            .store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(
            store
                .save_with_mode(&wf("second"), SaveMode::CreateNew)
                .is_err()
        );
        assert!(store.dir().join(DIRTY_FILE).exists());
        let old_index: Index =
            serde_json::from_str(&fs::read_to_string(store.dir().join(INDEX_FILE)).unwrap())
                .unwrap();
        assert_eq!(old_index.workflows.len(), 1);
        drop(store);
        let reopened = WorkflowStore::new(dir.path());
        let names: Vec<_> = reopened
            .list()
            .unwrap()
            .into_iter()
            .map(|item| item.name)
            .collect();
        assert_eq!(names, ["first", "second"]);
        assert!(!reopened.dir().join(DIRTY_FILE).exists());
        assert_no_owned_temps(&reopened);
    }

    #[test]
    fn create_new_publish_does_not_clobber_an_uncooperative_writer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("race.toml");
        let pending = PendingWrite::new(&path, b"candidate").unwrap();
        fs::write(&path, b"incumbent").unwrap();
        assert_eq!(
            pending.commit(&path, true).unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(path).unwrap(), b"incumbent");
        assert!(fs::read_dir(dir.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        }));
    }

    #[test]
    fn revision_exhaustion_never_wraps_or_changes_the_workflow() {
        let dir = tempfile::tempdir().unwrap();
        let store = WorkflowStore::new(dir.path());
        fs::create_dir_all(store.dir()).unwrap();
        let mut exhausted = wf("full");
        exhausted.revision = u64::try_from(i64::MAX).unwrap();
        let before = exhausted.to_toml().unwrap();
        fs::write(store.path("full").unwrap(), &before).unwrap();
        assert!(matches!(
            store.save_with_mode(
                &exhausted,
                SaveMode::Overwrite {
                    expected_version: Some(exhausted.revision)
                }
            ),
            Err(SaveError::RevisionExhausted(_))
        ));
        assert_eq!(
            fs::read_to_string(store.path("full").unwrap()).unwrap(),
            before
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_not_followed_for_workflows_indexes_or_temp_cleanup() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let store = WorkflowStore::new(dir.path());
        fs::create_dir_all(store.dir()).unwrap();
        let outside = dir.path().join("outside");
        let original = wf("linked").to_toml().unwrap();
        fs::write(&outside, &original).unwrap();
        symlink(&outside, store.path("linked").unwrap()).unwrap();
        symlink(&outside, store.dir().join(INDEX_FILE)).unwrap();
        symlink(&outside, store.dir().join("abandoned.toml.tmp")).unwrap();
        assert!(store.get("linked").is_err());
        assert!(matches!(
            store.save_with_mode(&wf("linked"), SaveMode::CreateNew),
            Err(SaveError::AlreadyExists(_))
        ));
        assert!(store.save(&wf("linked")).is_err());
        assert!(store.list().unwrap().is_empty());
        assert!(!store.dir().join("abandoned.toml.tmp").exists());
        assert!(
            !fs::symlink_metadata(store.dir().join(INDEX_FILE))
                .unwrap()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(outside).unwrap(), original);
    }

    #[test]
    fn two_processes_keep_every_workflow_and_index_entry() {
        let dir = tempfile::tempdir().unwrap();
        let mut children = Vec::new();
        for id in 0..2 {
            children.push(
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "store::tests::workflow_process_writer",
                        "--ignored",
                    ])
                    .env("PRAXI_WORKFLOW_TEST_DIR", dir.path())
                    .env("PRAXI_WORKFLOW_TEST_WRITER", id.to_string())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .unwrap(),
            );
        }
        for child in children {
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "child failed: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let store = WorkflowStore::new(dir.path());
        assert_eq!(store.list().unwrap().len(), 20);
        assert_no_owned_temps(&store);
    }

    #[test]
    #[ignore = "helper invoked by two_processes_keep_every_workflow_and_index_entry"]
    fn workflow_process_writer() {
        let dir = std::env::var_os("PRAXI_WORKFLOW_TEST_DIR").unwrap();
        let id = std::env::var("PRAXI_WORKFLOW_TEST_WRITER").unwrap();
        let store = WorkflowStore::new(Path::new(&dir));
        for i in 0..10 {
            store
                .save_with_mode(&wf(&format!("process-{id}-{i}")), SaveMode::CreateNew)
                .unwrap();
        }
    }
}
