//! Protect credential holders before bootstrap/worker threads start. This is
//! defence in depth, NOT OS isolation of arbitrary same-user programs: writable
//! configuration, UI automation, pre-existing handles and code injection remain.

use std::io;
use std::path::Path;
use std::sync::OnceLock;

#[cfg(windows)]
mod windows;

/// Optional compatibility policy in the daemon data directory.
pub const POLICY_FILE: &str = "isolation.toml";

/// Hardening is enabled unless explicitly disabled for compatibility. No tool
/// input or environment variable can weaken it for a running process.
#[derive(Clone, Copy, Debug, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Policy {
    pub memory_guard: bool,
    pub linux_no_new_privs: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            memory_guard: true,
            linux_no_new_privs: true,
        }
    }
}

impl Policy {
    /// Load policy; malformed/unreadable policy never silently disables guards.
    ///
    /// # Errors
    /// I/O or invalid TOML/unknown fields.
    pub fn load(data_dir: &Path) -> io::Result<Self> {
        match std::fs::read_to_string(data_dir.join(POLICY_FILE)) {
            Ok(text) => {
                toml::from_str(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }
}

/// Observed startup state of this process, not a claim about external daemons.
#[derive(Clone, Copy, Debug, Default)]
pub struct Status {
    pub memory_guard: bool,
    pub no_new_privs: bool,
}

static STATUS: OnceLock<Status> = OnceLock::new();

#[must_use]
pub fn status() -> Status {
    STATUS.get().copied().unwrap_or_default()
}

/// Call before secrets are read and before worker threads are created. Linux
/// no-new-privs then propagates to workers and their shell/MCP/CLI descendants.
/// Windows restricts new opens of this credential-holder process object.
///
/// # Errors
/// Any requested guard cannot be applied. Startup must fail closed.
pub fn protect(policy: Policy) -> io::Result<Status> {
    if let Some(status) = STATUS.get() {
        return Ok(*status);
    }
    let mut observed = Status::default();
    if policy.memory_guard {
        #[cfg(windows)]
        windows::protect()?;
        #[cfg(target_os = "linux")]
        rustix::process::set_dumpable_behavior(rustix::process::DumpableBehavior::NotDumpable)?;
        observed.memory_guard = cfg!(any(windows, target_os = "linux"));
    }
    #[cfg(target_os = "linux")]
    if policy.linux_no_new_privs {
        rustix::thread::set_no_new_privs(true)?;
        observed.no_new_privs = rustix::thread::no_new_privs()?;
        if !observed.no_new_privs {
            return Err(io::Error::other("no-new-privs did not apply"));
        }
    }
    let _ = STATUS.set(observed);
    Ok(observed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_policy_fails_closed_and_defaults_enable_hardening() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Policy::load(dir.path()).unwrap().memory_guard);
        std::fs::write(dir.path().join(POLICY_FILE), "memory_guadr = false").unwrap();
        assert!(Policy::load(dir.path()).is_err());
        std::fs::write(dir.path().join(POLICY_FILE), "linux_no_new_privs = false").unwrap();
        let policy = Policy::load(dir.path()).unwrap();
        assert!(policy.memory_guard);
        assert!(!policy.linux_no_new_privs);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_fixture() {
        if std::env::var("PRAXI_GUARD_FIXTURE").as_deref() != Ok("yes") {
            return;
        }
        let observed = protect(Policy::default()).unwrap();
        assert!(observed.memory_guard && observed.no_new_privs);
        let dir = tempfile::tempdir().unwrap();
        let output = std::process::Command::new("sh")
            .args(["-c", "test ! -r /proc/$1/mem && grep -q 'NoNewPrivs:.*1' /proc/self/status && echo normal-write > result", "probe"])
            .arg(std::process::id().to_string()).current_dir(dir.path()).output().unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("result")).unwrap(),
            "normal-write\n"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_child_inherits_no_new_privs_and_cannot_read_parent_proc_memory() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::linux_fixture", "--nocapture"])
            .env("PRAXI_GUARD_FIXTURE", "yes")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}
