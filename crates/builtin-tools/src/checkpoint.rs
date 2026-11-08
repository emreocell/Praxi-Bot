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

/// A symlink or junction met while backing up. Links are never followed (that
/// would copy whatever they point at, possibly forever); their target is
/// recorded and the link is recreated on restore.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LinkRecord {
    /// Backup-relative path the link occupies (`files/<n>/...`).
    rel: String,
    /// Link target as read from the filesystem.
    target: String,
    /// Whether it points at a directory (matters for Windows).
    dir: bool,
}

const LINKS_FILE: &str = "links.json";

/// Largest link target that may be copied when the link itself cannot be
/// recreated (same bound as `fs_read`).
const MAX_LINK_COPY_BYTES: u64 = 16 * 1024 * 1024;

fn is_link(m: &fs::Metadata) -> bool {
    // On Windows `is_symlink` is true for symlinks and junctions alike.
    m.file_type().is_symlink()
}

/// Copy `from` to `to` without following links. Links are appended to `links`
/// with `rel` as their backup-relative path; no placeholder is written to `to`.
fn copy_recursive(
    from: &Path,
    to: &Path,
    rel: &str,
    links: &mut Vec<LinkRecord>,
) -> io::Result<()> {
    let meta = fs::symlink_metadata(from)?;
    if is_link(&meta) {
        let target = fs::read_link(from)?;
        let dir = fs::metadata(from).is_ok_and(|m| m.is_dir());
        links.push(LinkRecord {
            rel: rel.to_owned(),
            target: target.display().to_string(),
            dir,
        });
        Ok(())
    } else if meta.is_dir() {
        fs::create_dir_all(to)?;
        for e in fs::read_dir(from)? {
            let e = e?;
            let name = e.file_name();
            copy_recursive(
                &e.path(),
                &to.join(&name),
                &format!("{rel}/{}", name.to_string_lossy()),
                links,
            )?;
        }
        Ok(())
    } else {
        if let Some(p) = to.parent() {
            fs::create_dir_all(p)?;
        }
        fs::copy(from, to).map(|_| ())
    }
}

#[cfg(unix)]
fn make_link(target: &Path, at: &Path, _dir: bool) -> io::Result<()> {
    std::os::unix::fs::symlink(target, at)
}

#[cfg(windows)]
fn make_link(target: &Path, at: &Path, dir: bool) -> io::Result<()> {
    if !dir {
        return std::os::windows::fs::symlink_file(target, at);
    }
    // Directory symlinks need a privilege most users lack; junctions (what
    // pnpm creates) do not, so fall back to one.
    std::os::windows::fs::symlink_dir(target, at).or_else(|e| {
        let t = target.display().to_string();
        let t = t.strip_prefix(r"\\?\").unwrap_or(&t).to_owned();
        // `mklink` is a cmd.exe builtin, so both paths pass through cmd's
        // parser. Both come from the workspace (attacker-influenced): refuse
        // anything cmd would interpret rather than trust argument quoting.
        if cmd_unsafe(&t) || cmd_unsafe(&at.display().to_string()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "path contains shell metacharacters; refusing to create a junction",
            ));
        }
        let ok = std::process::Command::new("cmd")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(at)
            .arg(&t)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if ok { Ok(()) } else { Err(e) }
    })
}

/// True when `s` holds a character that cmd.exe treats specially (command
/// separators, redirection, `%VAR%` / `!VAR!` expansion, quotes, control
/// characters).
#[cfg_attr(not(windows), allow(dead_code))]
fn cmd_unsafe(s: &str) -> bool {
    s.chars()
        .any(|c| c.is_control() || matches!(c, '&' | '|' | '^' | '<' | '>' | '%' | '"' | '!'))
}

/// Recreate a link; when the platform refuses (Windows without Developer
/// Mode, dangling directory links, unsafe paths) fall back to copying the
/// target's content, and failing that skip it. Returns a warning if the link
/// could not be restored as a link.
fn restore_link(target: &Path, at: &Path, dir: bool) -> Option<String> {
    let err = match make_link(target, at, dir) {
        Ok(()) => return None,
        Err(e) => e,
    };
    // A relative link target is relative to the link's own directory.
    let resolved = if target.is_relative() {
        at.parent()
            .map_or_else(|| target.to_path_buf(), |p| p.join(target))
    } else {
        target.to_path_buf()
    };
    let (copied, skip_reason) = match fs::metadata(&resolved) {
        Ok(m) if m.is_file() && m.len() > MAX_LINK_COPY_BYTES => (
            false,
            Some(format!(
                "target is larger than {} MiB",
                MAX_LINK_COPY_BYTES / (1024 * 1024)
            )),
        ),
        Ok(m) if m.is_file() => (fs::copy(&resolved, at).is_ok(), None),
        Ok(m) if m.is_dir() => (false, Some("target is a directory (not copied)".to_owned())),
        _ => (false, None),
    };
    Some(if copied {
        format!(
            "link {} -> {} could not be recreated ({err}); restored a copy of its content instead",
            at.display(),
            target.display()
        )
    } else {
        let why = skip_reason.map_or_else(String::new, |r| format!(": {r}"));
        format!(
            "link {} -> {} could not be recreated ({err}); skipped{why}",
            at.display(),
            target.display()
        )
    })
}

fn remove_any(p: &Path) -> io::Result<()> {
    match fs::symlink_metadata(p) {
        // A link is removed itself, never its target. A directory symlink or
        // junction on Windows needs `remove_dir`.
        Ok(m) if is_link(&m) => fs::remove_file(p).or_else(|_| fs::remove_dir(p)),
        Ok(m) if m.is_dir() => fs::remove_dir_all(p),
        Ok(_) => fs::remove_file(p),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

fn read_links(dir: &Path) -> Vec<LinkRecord> {
    fs::read(dir.join(LINKS_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
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
        let mut links = Vec::new();
        for (n, p) in paths.iter().enumerate() {
            let abs = std::path::absolute(p)?;
            let meta = fs::symlink_metadata(&abs).ok();
            let existed = meta.is_some();
            let is_dir = meta.as_ref().is_some_and(fs::Metadata::is_dir);
            let backup = if existed {
                let rel = format!("files/{n}");
                copy_recursive(&abs, &dir.join(&rel), &rel, &mut links)?;
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
        if !links.is_empty() {
            let lj = serde_json::to_vec_pretty(&links).map_err(io::Error::other)?;
            fs::write(dir.join(LINKS_FILE), lj)?;
        }
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
        self.restore_with_warnings(id).map(|(p, s, _)| (p, s))
    }

    /// Like [`Self::restore`], also returning warnings for links that could
    /// not be recreated (they are copied or skipped instead of aborting the
    /// restore halfway).
    ///
    /// # Errors
    /// Unknown id or I/O errors.
    pub fn restore_with_warnings(
        &self,
        id: &str,
    ) -> io::Result<(Vec<String>, String, Vec<String>)> {
        let (dir, cp) = self.find(id)?;
        let current: Vec<&Path> = cp.entries.iter().map(|e| Path::new(&e.path)).collect();
        let safety = self.create(&cp.run_id, &format!("pre-restore:{id}"), &current)?;
        let links = read_links(&dir);
        let mut restored = Vec::new();
        let mut warnings = Vec::new();
        for e in &cp.entries {
            let target = Path::new(&e.path);
            remove_any(target)?;
            if let Some(rel) = &e.backup {
                let mut ignored = Vec::new();
                let src = dir.join(rel);
                if fs::symlink_metadata(&src).is_ok() {
                    copy_recursive(&src, target, rel, &mut ignored)?;
                }
                // Recreate links recorded under this entry (including the
                // entry itself when it was a link).
                for l in &links {
                    let Some(rest) = l.rel.strip_prefix(rel.as_str()) else {
                        continue;
                    };
                    let at = if rest.is_empty() {
                        target.to_path_buf()
                    } else if let Some(r) = rest.strip_prefix('/') {
                        r.split('/').fold(target.to_path_buf(), |p, c| p.join(c))
                    } else {
                        continue;
                    };
                    if let Some(parent) = at.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    warnings.extend(restore_link(Path::new(&l.target), &at, l.dir));
                }
            }
            restored.push(e.path.clone());
        }
        Ok((restored, safety, warnings))
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
    fn links_are_not_followed_and_are_restored_as_links() {
        let data = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("huge.bin"), "payload").unwrap();
        let store = CheckpointStore::new(data.path());
        let d = work.path().join("d");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("f"), "x").unwrap();
        if make_link(outside.path(), &d.join("ext"), true).is_err() {
            eprintln!("skipping: cannot create links here");
            return;
        }
        // A self-referential link must not recurse.
        let _ = make_link(&d, &d.join("loop"), true);

        let id = store.create("r", "fs_delete", &[&d]).unwrap();
        // The link target was not copied into the backup.
        let backup = store.list("r").unwrap()[0].entries[0]
            .backup
            .clone()
            .unwrap();
        let (cp_dir, _) = store.find(&id).unwrap();
        assert!(!cp_dir.join(&backup).join("ext").exists());

        fs::remove_dir_all(&d).unwrap();
        assert!(outside.path().join("huge.bin").exists());
        store.restore(&id).unwrap();
        assert_eq!(fs::read_to_string(d.join("f")).unwrap(), "x");
        let m = fs::symlink_metadata(d.join("ext")).unwrap();
        assert!(m.file_type().is_symlink(), "link restored as a link");
        assert_eq!(
            fs::read_to_string(d.join("ext").join("huge.bin")).unwrap(),
            "payload"
        );
        // Restoring over it again removes only the link, not the target.
        store.restore(&id).unwrap();
        assert!(outside.path().join("huge.bin").exists());
    }

    #[test]
    fn cmd_metacharacters_are_detected() {
        for bad in [
            r"C:\tmp\a&calc",
            "a|b",
            "a^b",
            "a<b",
            "a>b",
            "%PATH%",
            "a\"b",
            "a!b",
            "a\nb",
        ] {
            assert!(cmd_unsafe(bad), "{bad}");
        }
        assert!(!cmd_unsafe(
            r"C:\Users\me\My Project\node_modules\.pnpm\a-b@1.0"
        ));
    }

    #[test]
    fn failed_link_recreation_does_not_abort_restore() {
        let data = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(data.path());
        let d = work.path().join("d");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("f"), "x").unwrap();
        // `d/bad` is a real non-empty directory in the backup, so it is restored
        // first and the link can then never be created at that path (no
        // symlink privilege or platform quirk involved), nor copied over it.
        fs::create_dir_all(d.join("bad")).unwrap();
        fs::write(d.join("bad").join("keep"), "k").unwrap();
        let id = store.create("r", "fs_delete", &[&d]).unwrap();
        let (cp_dir, _) = store.find(&id).unwrap();
        let backup = store.list("r").unwrap()[0].entries[0]
            .backup
            .clone()
            .unwrap();
        let links = vec![LinkRecord {
            rel: format!("{backup}/bad"),
            target: r"C:\tmp\a&calc\nope".into(),
            dir: true,
        }];
        fs::write(cp_dir.join(LINKS_FILE), serde_json::to_vec(&links).unwrap()).unwrap();
        fs::remove_dir_all(&d).unwrap();
        let (_, _, warnings) = store.restore_with_warnings(&id).unwrap();
        assert_eq!(fs::read_to_string(d.join("f")).unwrap(), "x");
        assert_eq!(fs::read_to_string(d.join("bad").join("keep")).unwrap(), "k");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("skipped"), "{warnings:?}");
    }

    #[test]
    fn link_fallback_copy_is_bounded_relative_and_skips_directories() {
        let w = tempfile::tempdir().unwrap();
        // `at` already exists, so creating the link fails and the fallback runs.
        let at = w.path().join("at");
        fs::write(&at, "old").unwrap();

        // Relative target resolves against the link's directory.
        fs::write(w.path().join("src.txt"), "new").unwrap();
        let msg = restore_link(Path::new("src.txt"), &at, false).unwrap();
        assert!(msg.contains("restored a copy"), "{msg}");
        assert_eq!(fs::read_to_string(&at).unwrap(), "new");

        // Oversized targets are skipped with a warning.
        let big = w.path().join("big.bin");
        fs::File::create(&big)
            .unwrap()
            .set_len(MAX_LINK_COPY_BYTES + 1)
            .unwrap();
        fs::write(&at, "old").unwrap();
        let msg = restore_link(&big, &at, false).unwrap();
        assert!(msg.contains("skipped") && msg.contains("larger"), "{msg}");
        assert_eq!(fs::read_to_string(&at).unwrap(), "old");

        // Directories are not copied.
        let dir = w.path().join("dir");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("f"), "x").unwrap();
        let msg = restore_link(&dir, &at, true).unwrap();
        assert!(
            msg.contains("skipped") && msg.contains("directory"),
            "{msg}"
        );
        assert!(!at.join("f").exists());
    }

    #[test]
    fn bad_ids_are_rejected() {
        let data = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(data.path());
        assert!(store.restore("../x").is_err());
        assert!(store.restore("").is_err());
    }
}
