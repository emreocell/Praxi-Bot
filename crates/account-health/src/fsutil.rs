//! Owner-only files for the health report and state, and size-capped reads.

use std::io::{Read as _, Write as _};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// The report and state files are tiny; anything bigger is not ours.
pub(crate) const MAX_READ: u64 = 2 * 1024 * 1024;

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Read at most `cap` bytes; `None` when the file is missing, unreadable or larger.
pub(crate) fn read_capped(path: &Path, cap: u64) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > cap {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(cap + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= cap).then_some(bytes)
}

/// Create `dir` (and parents) owner-only; a directory an earlier build left open is tightened.
pub(crate) fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        if std::fs::metadata(dir)?.permissions().mode() & 0o077 != 0 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

/// Write `bytes` to `path` through a fresh, owner-only temp file in the same directory, then
/// rename. The temp name is unique per call, so two writers never share one; it is removed when
/// anything fails.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let tmp = dir.join(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        TMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let result = (|| {
        let mut f = options.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    // The rename reaches the disk only once the directory is flushed.
    #[cfg(unix)]
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn files_and_dir_are_owner_only_and_no_temp_is_left() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("account-health");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        create_private_dir(&dir).unwrap();
        let f = dir.join("report.json");
        write_atomic(&f, b"{}").unwrap();
        write_atomic(&f, b"{\"a\":1}").unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&f), 0o600);
        let left: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
        assert_eq!(left.len(), 1);
        assert_eq!(std::fs::read(&f).unwrap(), b"{\"a\":1}");
    }

    #[test]
    fn oversized_files_are_not_read() {
        let root = tempfile::tempdir().unwrap();
        let f = root.path().join("big");
        std::fs::write(&f, vec![b'x'; 11]).unwrap();
        assert!(read_capped(&f, 10).is_none());
        assert_eq!(read_capped(&f, 11).unwrap().len(), 11);
        assert!(read_capped(&root.path().join("none"), 10).is_none());
    }

    #[test]
    fn failed_write_leaves_no_temp() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("dir");
        std::fs::create_dir(&target).unwrap();
        // Renaming a file over a directory fails.
        assert!(write_atomic(&target, b"x").is_err());
        let names: Vec<_> = std::fs::read_dir(root.path()).unwrap().flatten().collect();
        assert_eq!(names.len(), 1);
    }
}
