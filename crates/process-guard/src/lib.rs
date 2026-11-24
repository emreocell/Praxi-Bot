//! OS-level protection of the daemon process against its own same-user
//! children (bot shell tools, MCP servers, coding CLIs).
//!
//! The daemon keeps its owner secret in memory. A child running as the same OS
//! user could otherwise read that memory (`ReadProcessMemory`, `ptrace`,
//! `/proc/<pid>/mem`) or duplicate the daemon's handles. [`protect`] removes
//! those routes where the OS offers a switch for it:
//!
//! * **Windows**: the process security descriptor is replaced by a protected
//!   DACL that grants the owner only query/terminate/synchronize (plus an
//!   `OWNER RIGHTS` entry so the owner cannot implicitly regain `WRITE_DAC`).
//! * **Linux**: deliberately **not enabled yet**, reported as
//!   [`Outcome::Skipped`]. `PR_SET_DUMPABLE=0` would make `/proc/<pid>`
//!   root-owned and unreadable for the same user, and the desktop app learns
//!   the daemon's listener by scanning `/proc/<pid>/fd` and comparing the uid of
//!   `/proc/<pid>`; it would refuse to connect to a healthy daemon. Enabling it
//!   needs the desktop's listener check moved to a pid+stdin handshake or
//!   `SO_PEERCRED` first (follow-up).
//! * **Other platforms**: no-op, reported as [`Outcome::NotSupported`].
//!
//! This is defence in depth, **not** a sandbox: it does not stop a child from
//! reading the daemon's files, talking to its API with a token it obtained
//! elsewhere, injecting code through other channels, or using handles that
//! were opened before the guard was applied. Failure is never fatal: the
//! result is recorded so `doctor` can show it.

use std::io;
use std::path::Path;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

pub mod lifecycle;
pub mod owner;

#[cfg(windows)]
pub mod supervision;

#[cfg(windows)]
mod windows;

#[cfg(unix)]
mod unix;

/// File in the data directory holding the last [`Report`] (read by `doctor`).
pub const REPORT_FILE: &str = "process-guard.json";

/// What happened when the guard was applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// The process is protected.
    Applied,
    /// This platform has no such switch; nothing was changed.
    NotSupported,
    /// Intentionally not applied (see `detail`); nothing was changed.
    Skipped,
    /// The OS refused; the process runs unprotected.
    Failed,
}

/// Result of [`protect`], serialisable for the doctor check.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    /// Outcome class.
    pub outcome: Outcome,
    /// One line, safe to show to the user (never contains secrets).
    pub detail: String,
    /// Process id the report belongs to (a stale file names a dead pid).
    pub pid: u32,
}

static REPORT: OnceLock<Report> = OnceLock::new();

/// Apply the protection to the current process (once; later calls return the
/// first result). Call as early as possible, before secrets are read. Never
/// panics and never fails startup: inspect [`Report::outcome`].
pub fn protect() -> Report {
    REPORT
        .get_or_init(|| {
            let (outcome, detail) = match apply() {
                #[cfg(windows)]
                Ok(Applied::Yes(detail)) => (Outcome::Applied, detail.to_owned()),
                #[cfg(target_os = "linux")]
                Ok(Applied::Skipped(reason)) => (Outcome::Skipped, reason.to_owned()),
                #[cfg(not(any(windows, target_os = "linux")))]
                Ok(Applied::Unsupported) => (
                    Outcome::NotSupported,
                    format!("not supported on {}", std::env::consts::OS),
                ),
                Err(e) => (Outcome::Failed, format!("could not apply: {e}")),
            };
            Report {
                outcome,
                detail,
                pid: std::process::id(),
            }
        })
        .clone()
}

enum Applied {
    #[cfg(windows)]
    Yes(&'static str),
    #[cfg(target_os = "linux")]
    Skipped(&'static str),
    #[cfg(not(any(windows, target_os = "linux")))]
    Unsupported,
}

#[allow(clippy::unnecessary_wraps)] // the windows arm returns errors
fn apply() -> io::Result<Applied> {
    #[cfg(windows)]
    {
        windows::protect()?;
        Ok(Applied::Yes(
            "process access restricted (no VM read/write, handle duplication or thread creation by other processes)",
        ))
    }
    #[cfg(target_os = "linux")]
    {
        // PR_SET_DUMPABLE=0 is intentionally NOT set: it makes /proc/<pid>
        // root-owned and unreadable for the same user, which breaks the desktop
        // app's listener identity check (apps/desktop listener_pids scans
        // /proc/<pid>/fd; is_same_user compares the uid of /proc/<pid>). Enable
        // it only after that check uses a pid+stdin handshake or SO_PEERCRED.
        Ok(Applied::Skipped(
            "not enabled on Linux yet: it would break the desktop app's listener check; tracked as follow-up",
        ))
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        Ok(Applied::Unsupported)
    }
}

/// Persist `report` in `data_dir` for `doctor`.
///
/// # Errors
/// The file cannot be written.
pub fn record(data_dir: &Path, report: &Report) -> io::Result<()> {
    let text = serde_json::to_string(report).map_err(io::Error::other)?;
    // Atomic: write a temp file, then rename over the report so `doctor` never
    // reads a torn file.
    let tmp = data_dir.join(format!("{REPORT_FILE}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, data_dir.join(REPORT_FILE)).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Last recorded report, `None` if the daemon never wrote one (or it is
/// unreadable).
#[must_use]
pub fn load(data_dir: &Path) -> Option<Report> {
    serde_json::from_slice(&std::fs::read(data_dir.join(REPORT_FILE)).ok()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_overwrites_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut report = Report {
            outcome: Outcome::Skipped,
            detail: "a".into(),
            pid: 1,
        };
        record(dir.path(), &report).unwrap();
        report.detail = "b".into();
        record(dir.path(), &report).unwrap();
        assert_eq!(load(dir.path()).unwrap().detail, "b");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn report_round_trips_through_the_data_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(dir.path()).is_none());
        let report = Report {
            outcome: Outcome::Failed,
            detail: "boom".into(),
            pid: 7,
        };
        record(dir.path(), &report).unwrap();
        assert_eq!(load(dir.path()), Some(report));
    }

    /// Run the named test in a fresh process so the guard never touches the
    /// test runner itself.
    #[cfg(any(windows, target_os = "linux"))]
    fn run_fixture(name: &str) {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", name, "--nocapture"])
            .env("PRAXI_GUARD_FIXTURE", "yes")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_guard_is_a_documented_noop_and_proc_stays_readable() {
        run_fixture("tests::linux_fixture");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_fixture() {
        if std::env::var("PRAXI_GUARD_FIXTURE").as_deref() != Ok("yes") {
            return;
        }
        let report = protect();
        assert_eq!(report.outcome, Outcome::Skipped, "{report:?}");
        assert!(report.detail.contains("not enabled on Linux yet"));
        // The desktop's listener check needs a same-user process to read
        // /proc/<daemon>/fd and to see /proc/<daemon> owned by the same user.
        let output = std::process::Command::new("sh")
            .args([
                "-c",
                "ls /proc/$1/fd >/dev/null && test -O /proc/$1",
                "probe",
            ])
            .arg(std::process::id().to_string())
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }

    #[cfg(windows)]
    #[test]
    fn windows_guard_denies_memory_access_and_children_still_run() {
        run_fixture("windows::tests::protection_fixture");
    }

    #[cfg(not(any(windows, target_os = "linux")))]
    #[test]
    fn unsupported_platform_reports_not_supported() {
        assert_eq!(protect().outcome, Outcome::NotSupported);
    }
}
