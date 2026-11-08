//! Locks protecting a complete read/modify/write, not just the final write.

use std::{
    fs::{File, OpenOptions},
    path::Path,
    sync::{Mutex, MutexGuard},
};

use crate::SecretError;

/// Drop the file (and its advisory lock) before releasing the thread mutex.
pub(crate) struct TransactionGuard<'a> {
    _file: File,
    _thread: MutexGuard<'a, ()>,
}

pub(crate) fn parent_directory(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

pub(crate) fn acquire<'a>(
    mutex: &'a Mutex<()>,
    path: &Path,
) -> Result<TransactionGuard<'a>, SecretError> {
    // No mutable in-memory state is protected by this mutex. After a panic,
    // the on-disk/keyring state is reloaded (and its index repaired) as usual.
    let thread = mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    std::fs::create_dir_all(parent_directory(path))?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    fs4::FileExt::lock(&file)?;
    // Never unlink the sidecar: another process may already have it open.
    Ok(TransactionGuard {
        _file: file,
        _thread: thread,
    })
}
