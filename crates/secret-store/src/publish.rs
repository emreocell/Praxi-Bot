//! Windows publication of a fully synced temporary secret file.
//!
//! `MoveFileExW` without write-through can return before the directory update is
//! on disk. An existing destination is published with `ReplaceFileW`; a missing
//! one is created with `MoveFileExW`. The caller still deletes only the temp path.

use std::io;
use std::path::Path;

/// Publish `source` as `destination`.
///
/// When `destination` already exists this calls `ReplaceFileW` with
/// `REPLACEFILE_WRITE_THROUGH | REPLACEFILE_IGNORE_MERGE_SNAPSHOT`. Otherwise it
/// calls `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH`.
pub(crate) fn publish(source: &Path, destination: &Path) -> io::Result<()> {
    match std::fs::metadata(destination) {
        Ok(_) => win::replace_file(source, destination),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            win::move_file(source, destination)
        }
        Err(error) => Err(error),
    }
}

/// The only `unsafe` in this crate: `ReplaceFileW` and `MoveFileExW` have no
/// safe wrapper in `windows` 0.62. Paths stay in owned `HSTRING`s for the call.
#[allow(unsafe_code)]
mod win {
    use std::path::Path;

    use windows::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW, REPLACE_FILE_FLAGS,
        REPLACEFILE_IGNORE_MERGE_ERRORS, REPLACEFILE_WRITE_THROUGH, ReplaceFileW,
    };
    use windows::core::{HSTRING, PCWSTR};

    /// SDK name for the flag value `0x2`. The review text calls this
    /// `REPLACEFILE_IGNORE_MERGE_SNAPSHOT`; that identifier is not in the headers.
    const REPLACEFILE_IGNORE_MERGE_SNAPSHOT: REPLACE_FILE_FLAGS = REPLACEFILE_IGNORE_MERGE_ERRORS;

    pub(super) fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
        let destination = HSTRING::from(destination);
        let source = HSTRING::from(source);
        // SAFETY: both `HSTRING`s are NUL-terminated and live until this function
        // returns. A null backup name asks Windows not to leave a second copy.
        // `lpExclude` and `lpReserved` are reserved and must be null.
        unsafe {
            ReplaceFileW(
                &destination,
                &source,
                PCWSTR::null(),
                REPLACEFILE_WRITE_THROUGH | REPLACEFILE_IGNORE_MERGE_SNAPSHOT,
                None,
                None,
            )
        }
        .map_err(std::io::Error::from)
    }

    pub(super) fn move_file(source: &Path, destination: &Path) -> std::io::Result<()> {
        let source = HSTRING::from(source);
        let destination = HSTRING::from(destination);
        // SAFETY: both `HSTRING`s are NUL-terminated and live until this function
        // returns. Write-through keeps the call from succeeding before the move
        // is on disk. Replace-existing covers a file created after the metadata check.
        unsafe {
            MoveFileExW(
                &source,
                &destination,
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        }
        .map_err(std::io::Error::from)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::publish;

    #[test]
    fn windows_publish_moves_absent_file_and_replaces_existing_without_backup() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("secrets.enc");
        let source = dir.path().join("secrets.enc.1.tmp");
        std::fs::write(&source, b"first").unwrap();
        let moved_created = created(&source);
        publish(&source, &destination).unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), b"first");
        assert!(!source.exists());
        assert_eq!(created(&destination), moved_created);

        std::thread::sleep(std::time::Duration::from_millis(40));
        let replacement = dir.path().join("secrets.enc.2.tmp");
        std::fs::write(&replacement, b"second").unwrap();
        let kept = created(&destination);
        assert_ne!(
            created(&replacement),
            kept,
            "this filesystem cannot distinguish creation times"
        );
        publish(&replacement, &destination).unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), b"second");
        assert!(!replacement.exists());
        assert_eq!(
            created(&destination),
            kept,
            "ReplaceFileW keeps the replaced file's creation time"
        );
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("secrets.enc")]);
    }

    fn created(path: &Path) -> std::time::SystemTime {
        std::fs::metadata(path).unwrap().created().unwrap()
    }
}
