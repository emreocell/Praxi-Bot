//! Saved workflows: `<data_dir>/workflows/<name>.toml` plus `index.json`.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use crate::error::StoreError;
use crate::format::{Workflow, valid_name};

const INDEX_FILE: &str = "index.json";

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
    workflows: Vec<IndexEntry>,
}

/// File-backed workflow store.
#[derive(Debug, Clone)]
pub struct WorkflowStore {
    dir: PathBuf,
}

impl WorkflowStore {
    /// A store under `<data_dir>/workflows` (created on first save).
    #[must_use]
    pub fn new(data_dir: &Path) -> Self {
        Self {
            dir: data_dir.join("workflows"),
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

    /// Validate and save (overwrites a workflow of the same name).
    ///
    /// # Errors
    /// Invalid workflow or name, or a disk error.
    pub fn save(&self, wf: &Workflow) -> Result<(), StoreError> {
        wf.validate()?;
        let path = self.path(&wf.name)?;
        let text = wf.to_toml()?;
        fs::create_dir_all(&self.dir)?;
        let tmp = path.with_extension("toml.tmp");
        fs::write(&tmp, text)?;
        fs::rename(&tmp, &path)?;
        self.rebuild_index().map(|_| ())
    }

    /// Load by name.
    ///
    /// # Errors
    /// Unknown name, bad file or disk error.
    pub fn get(&self, name: &str) -> Result<Workflow, StoreError> {
        let path = self.path(name)?;
        match fs::read_to_string(&path) {
            Ok(t) => Ok(Workflow::from_toml(&t)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(StoreError::NotFound(name.to_owned()))
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Delete by name; `false` when it did not exist.
    ///
    /// # Errors
    /// Bad name or disk error.
    pub fn delete(&self, name: &str) -> Result<bool, StoreError> {
        let path = self.path(name)?;
        match fs::remove_file(&path) {
            Ok(()) => {
                self.rebuild_index()?;
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// Index entries sorted by name (rebuilt from the files when the index is
    /// missing or unreadable).
    ///
    /// # Errors
    /// Disk error.
    pub fn list(&self) -> Result<Vec<IndexEntry>, StoreError> {
        if let Ok(text) = fs::read_to_string(self.dir.join(INDEX_FILE))
            && let Ok(idx) = serde_json::from_str::<Index>(&text)
        {
            return Ok(idx.workflows);
        }
        self.rebuild_index()
    }

    /// Rescan the directory and rewrite the index. Unparseable files are skipped.
    ///
    /// # Errors
    /// Disk error.
    pub fn rebuild_index(&self) -> Result<Vec<IndexEntry>, StoreError> {
        let mut entries = Vec::new();
        let rd = match fs::read_dir(&self.dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(entries),
            Err(e) => return Err(e.into()),
        };
        for item in rd.flatten() {
            let path = item.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            let Ok(wf) = Workflow::from_toml(&text) else {
                continue;
            };
            if path.file_stem().and_then(|s| s.to_str()) != Some(wf.name.as_str()) {
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
            workflows: entries.clone(),
        })
        .map_err(|e| StoreError::Index(e.to_string()))?;
        fs::write(self.dir.join(INDEX_FILE), text)?;
        Ok(entries)
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
}
