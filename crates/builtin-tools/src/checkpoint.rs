//! Edit checkpoints: copies of files taken before a destructive file tool runs.
//!
//! Layout: `<root>/<run>/<checkpoint id>/manifest.json` plus `files/<n>` holding
//! the backed up content of each path. Restoring puts every recorded path back
//! to its state at checkpoint time (paths that did not exist are removed) and
//! first takes a checkpoint of the current state so a restore can be undone.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// One path recorded in a checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointEntry {
    /// Absolute path that was backed up.
    pub path: String,
    /// Whether the path existed when the checkpoint was taken.
    pub existed: bool,
    /// Whether the path was a directory.
    pub is_dir: bool,
    /// Backup location relative to the checkpoint directory.
    pub backup: Option<String>,
}

/// Manifest describing one checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Checkpoint id (`cp-...`).
    pub id: String,
    /// Run that created it.
    pub run_id: String,
    /// Creation time in unix milliseconds.
    pub created_ms: u64,
    /// Tool/operation that triggered it (e.g. `fs_write`).
    pub op: String,
    /// Recorded paths.
    pub entries: Vec<CheckpointEntry>,
}

/// Checkpoint storage rooted at `<data_dir>/checkpoints`.
#[derive(Debug, Clone)]
pub struct CheckpointStore {
    root: PathBuf,
}

fn sanitize(s: &str) -> String {
    let t: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if t.is_empty() { "no-run".into() } else { t }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn copy_recursive(from: &Path, to: &Path) -> io::Result<()> {
    if from.is_dir() {
        fs::create_dir_all(to)?;
        for e in fs::read_dir(from)? {
            let e = e?;
            copy_recursive(&e.path(), &to.join(e.file_name()))?;
        }
        Ok(())
    } else {
        if let Some(p) = to.parent() {
            fs::create_dir_all(p)?;
        }
        fs::copy(from, to).map(|_| ())
    }
}

fn remove_any(p: &Path) -> io::Result<()> {
    match fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => fs::remove_dir_all(p),
        Ok(_) => fs::remove_file(p),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

impl CheckpointStore {
    /// Create a store under `<data_dir>/checkpoints`.
    #[must_use]
    pub fn new(data_dir: &Path) -> Self {
        Self {
            root: data_dir.join("checkpoints"),
        }
    }

    /// Back up `paths` (those that exist) and write a manifest. Returns the id.
    ///
    /// # Errors
    /// I/O errors while copying or writing the manifest.
    pub fn create(&self, run_id: &str, op: &str, paths: &[&Path]) -> io::Result<String> {
        let id = format!("cp-{}", uuid::Uuid::now_v7().simple());
        let dir = self.root.join(sanitize(run_id)).join(&id);
        fs::create_dir_all(dir.join("files"))?;
        let mut entries = Vec::new();
        for (n, p) in paths.iter().enumerate() {
            let abs = std::path::absolute(p)?;
            let meta = fs::symlink_metadata(&abs).ok();
            let existed = meta.is_some();
            let is_dir = meta.as_ref().is_some_and(fs::Metadata::is_dir);
            let backup = if existed {
                let rel = format!("files/{n}");
                copy_recursive(&abs, &dir.join(&rel))?;
                Some(rel)
            } else {
                None
            };
            entries.push(CheckpointEntry {
                path: abs.display().to_string(),
                existed,
                is_dir,
                backup,
            });
        }
        let cp = Checkpoint {
            id: id.clone(),
            run_id: run_id.to_string(),
            created_ms: now_ms(),
            op: op.to_string(),
            entries,
        };
        let json = serde_json::to_vec_pretty(&cp).map_err(io::Error::other)?;
        fs::write(dir.join("manifest.json"), json)?;
        Ok(id)
    }

    /// List the checkpoints of a run, oldest first.
    ///
    /// # Errors
    /// I/O errors other than a missing directory.
    pub fn list(&self, run_id: &str) -> io::Result<Vec<Checkpoint>> {
        let dir = self.root.join(sanitize(run_id));
        let mut out = Vec::new();
        let rd = match fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e),
        };
        for e in rd {
            let m = e?.path().join("manifest.json");
            if let Ok(bytes) = fs::read(&m)
                && let Ok(cp) = serde_json::from_slice::<Checkpoint>(&bytes)
            {
                out.push(cp);
            }
        }
        out.sort_by(|a, b| a.created_ms.cmp(&b.created_ms).then(a.id.cmp(&b.id)));
        Ok(out)
    }

    fn find(&self, id: &str) -> io::Result<(PathBuf, Checkpoint)> {
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid checkpoint id",
            ));
        }
        let runs = match fs::read_dir(&self.root) {
            Ok(r) => r,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("checkpoint {id} not found"),
                ));
            }
            Err(e) => return Err(e),
        };
        for run in runs {
            let dir = run?.path().join(id);
            if let Ok(bytes) = fs::read(dir.join("manifest.json")) {
                let cp = serde_json::from_slice::<Checkpoint>(&bytes).map_err(io::Error::other)?;
                return Ok((dir, cp));
            }
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("checkpoint {id} not found"),
        ))
    }

    /// Restore a checkpoint. Returns the restored paths and the id of the
    /// safety checkpoint taken of the state that was overwritten.
    ///
    /// # Errors
    /// Unknown id or I/O errors.
    pub fn restore(&self, id: &str) -> io::Result<(Vec<String>, String)> {
        let (dir, cp) = self.find(id)?;
        let current: Vec<&Path> = cp.entries.iter().map(|e| Path::new(&e.path)).collect();
        let safety = self.create(&cp.run_id, &format!("pre-restore:{id}"), &current)?;
        let mut restored = Vec::new();
        for e in &cp.entries {
            let target = Path::new(&e.path);
            remove_any(target)?;
            if let Some(rel) = &e.backup {
                copy_recursive(&dir.join(rel), target)?;
            }
            restored.push(e.path.clone());
        }
        Ok((restored, safety))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_file_and_created_file() {
        let data = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(data.path());
        let existing = work.path().join("a.txt");
        let created = work.path().join("b.txt");
        fs::write(&existing, "original").unwrap();

        let id = store
            .create("run1", "fs_write", &[&existing, &created])
            .unwrap();
        fs::write(&existing, "changed").unwrap();
        fs::write(&created, "new").unwrap();

        let list = store.list("run1").unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, id);

        let (restored, safety) = store.restore(&id).unwrap();
        assert_eq!(restored.len(), 2);
        assert_eq!(fs::read_to_string(&existing).unwrap(), "original");
        assert!(!created.exists());
        // The restore itself is undoable.
        store.restore(&safety).unwrap();
        assert_eq!(fs::read_to_string(&existing).unwrap(), "changed");
        assert_eq!(fs::read_to_string(&created).unwrap(), "new");
    }

    #[test]
    fn directories_are_backed_up_recursively() {
        let data = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(data.path());
        let d = work.path().join("d");
        fs::create_dir_all(d.join("sub")).unwrap();
        fs::write(d.join("sub").join("f"), "x").unwrap();
        let id = store.create("r", "fs_delete", &[&d]).unwrap();
        fs::remove_dir_all(&d).unwrap();
        store.restore(&id).unwrap();
        assert_eq!(fs::read_to_string(d.join("sub").join("f")).unwrap(), "x");
    }

    #[test]
    fn bad_ids_are_rejected() {
        let data = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(data.path());
        assert!(store.restore("../x").is_err());
        assert!(store.restore("").is_err());
    }
}
