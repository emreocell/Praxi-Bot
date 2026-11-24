//! Shared desktop/CLI discovery. The OS lock is the authority, never a PID file.
use std::fs::{File, OpenOptions};
use std::io;
use std::net::{Ipv4Addr, TcpListener};
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Also held by backup restore. Never unlink this file: that would split the lock.
pub const LOCK_FILE: &str = ".daemon.lock";
const ENDPOINT_FILE: &str = ".daemon-endpoint.json";

/// Non-secret discovery information; always verify the socket owner before authentication.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Endpoint {
    /// PID that must own the listener.
    pub pid: u32,
    /// Actual loopback port, including an OS-selected fallback.
    pub port: u16,
    /// Requested port, so CLI clients using `BOT_PORT` can follow its fallback too.
    pub requested_port: u16,
}

impl Endpoint {
    /// Loopback URL (no file-controlled host).
    #[must_use]
    pub fn url(self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Publish after bind, while holding the daemon lock.
    /// # Errors
    /// The data directory cannot be written.
    pub fn publish(self, data: &Path) -> io::Result<()> {
        let tmp = data.join(format!("{ENDPOINT_FILE}.tmp"));
        std::fs::write(&tmp, serde_json::to_vec(&self)?)?;
        protect_file(&tmp)?;
        std::fs::rename(tmp, data.join(ENDPOINT_FILE))
    }
}

/// Owner-only access for lock/discovery files (Windows DACL; a no-op elsewhere,
/// where the data directory's own mode applies).
#[cfg_attr(not(windows), allow(clippy::unnecessary_wraps))]
fn protect_file(path: &Path) -> io::Result<()> {
    #[cfg(windows)]
    return crate::windows::owner_only_file(path);
    #[cfg(not(windows))]
    {
        let _ = path;
        Ok(())
    }
}

/// Open the lock file WITHOUT delete sharing on Windows, so no process can unlink
/// or rename it while anyone holds it (a replaced lock file would split the lock).
/// # Errors
/// The file cannot be opened.
pub fn open_lock_file(data: &Path, create: bool) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(create)
        .truncate(false);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0x1 | 0x2); // FILE_SHARE_READ | FILE_SHARE_WRITE
    }
    options.open(data.join(LOCK_FILE))
}

/// Acquire exclusive ownership before opening databases, reading secrets or spawning children.
/// # Errors
/// Another daemon/restore owns the directory, or the directory is unwritable.
pub fn acquire(data: &Path) -> io::Result<File> {
    std::fs::create_dir_all(data)?;
    let file = open_lock_file(data, true)?;
    protect_file(&data.join(LOCK_FILE))?;
    file.try_lock().map_err(|e| {
        io::Error::other(format!(
            "Praxi is already using this data folder, or a restore is in progress ({e})"
        ))
    })?;
    // An unlocked file after a crash is harmless; discard stale discovery before startup.
    match std::fs::remove_file(data.join(ENDPOINT_FILE)) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    Ok(file)
}

/// Discovery only while a daemon/restore actually holds the lock. A stale PID never wins.
#[must_use]
pub fn discover(data: &Path) -> Option<Endpoint> {
    let file = open_lock_file(data, false).ok()?;
    match file.try_lock() {
        Err(std::fs::TryLockError::WouldBlock) => {}
        Ok(()) | Err(std::fs::TryLockError::Error(_)) => return None,
    }
    let endpoint: Endpoint =
        serde_json::from_slice(&std::fs::read(data.join(ENDPOINT_FILE)).ok()?).ok()?;
    (endpoint.pid != 0 && endpoint.port != 0).then_some(endpoint)
}

/// Reserve the socket, keeping it open across startup so there is no probe/bind race.
/// # Errors
/// Neither the requested nor a free loopback port can be opened.
pub fn bind(port: u16) -> io::Result<TcpListener> {
    select_port(port, |p| TcpListener::bind((Ipv4Addr::LOCALHOST, p)))
}

fn select_port<T>(port: u16, mut bind: impl FnMut(u16) -> io::Result<T>) -> io::Result<T> {
    match bind(port) {
        Err(e) if port != 0 && matches!(e.kind(), io::ErrorKind::AddrInUse | io::ErrorKind::PermissionDenied) => bind(0),
        result => result,
    }.map_err(|e| io::Error::new(e.kind(), format!(
        "Praxi could not open a local connection on port {port} or a free port. Close other Praxi windows and try again ({e})"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_excludes_second_start_and_stale_files_do_not_block_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let first = acquire(dir.path()).unwrap();
        let endpoint = Endpoint {
            pid: 42,
            port: 12345,
            requested_port: 47821,
        };
        endpoint.publish(dir.path()).unwrap();
        assert!(acquire(dir.path()).is_err());
        assert_eq!(discover(dir.path()), Some(endpoint));
        drop(first); // The kernel does this on a crash, too.
        assert_eq!(discover(dir.path()), None);
        let _next = acquire(dir.path()).unwrap();
        assert_eq!(discover(dir.path()), None);
    }

    #[test]
    fn busy_port_uses_os_selection_and_other_errors_are_not_hidden() {
        let mut attempts = Vec::new();
        let result = select_port(47821, |p| {
            attempts.push(p);
            if p == 0 {
                Ok(53123)
            } else {
                Err(io::ErrorKind::AddrInUse.into())
            }
        })
        .unwrap();
        assert_eq!(attempts, [47821, 0]);
        assert_eq!(result, 53123);
        assert!(select_port(47821, |_| Err::<(), _>(io::ErrorKind::OutOfMemory.into())).is_err());
    }
}
