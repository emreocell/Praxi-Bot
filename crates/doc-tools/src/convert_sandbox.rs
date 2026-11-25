//! Network isolation for the converters, and the syntactic path guard.
//!
//! DESIGN: vetting a hostile document can never be complete, so the control that really
//! matters is that the converter has no network. On Linux, LibreOffice and `pdftoppm` run
//! inside a network-less namespace: bubblewrap (`bwrap --unshare-net ...`, with a read-only
//! root, no home folders and a private /tmp) when present, else `unshare -rn` (unprivileged
//! user namespace) or `unshare -n` (root). One probe at first use decides which one works.
//! When none does the tools FAIL CLOSED, unless the owner sets
//! `allow_unsandboxed_convert = true` (documented as unsafe for untrusted documents).
//!
//! Windows and macOS have no such sandbox in safe Rust. Conversion is OFF on Windows
//! unless the owner sets `convert_enabled_on_windows = true` (a converter there can leak
//! the daemon's NTLM hash over SMB: only enable it for trusted documents); the strict
//! document vetting applies either way. macOS is treated like "no sandbox".

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use tool_registry::ToolError;

use crate::DocToolsConfig;
use crate::common::{exec, invalid};

/// How a converter is wrapped.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) enum Isolation {
    Bwrap(PathBuf),
    /// `unshare` with its arguments (`-rn` or `-n`).
    Unshare(PathBuf, &'static str),
}

pub(crate) const UNAVAILABLE: &str = "network isolation unavailable on this host: install bubblewrap (bwrap) or enable unprivileged user namespaces; the owner can set allow_unsandboxed_convert = true, which is UNSAFE for untrusted documents";

static PROBE: OnceLock<Option<Isolation>> = OnceLock::new();

/// Which isolation works here (probed once per process).
pub(crate) fn detect() -> Option<Isolation> {
    PROBE.get_or_init(probe).clone()
}

#[cfg(target_os = "linux")]
fn probe() -> Option<Isolation> {
    use std::process::Stdio;
    let ok = |mut c: Command| {
        c.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    };
    // Absolute, fixed locations only: a `PATH` lookup would trust whatever is writable.
    for dir in ["/usr/bin", "/bin", "/usr/local/bin"] {
        let bwrap = Path::new(dir).join("bwrap");
        if bwrap.is_file() {
            let mut c = Command::new(&bwrap);
            c.args([
                "--unshare-net",
                "--ro-bind",
                "/",
                "/",
                "--dev",
                "/dev",
                "true",
            ]);
            if ok(c) {
                return Some(Isolation::Bwrap(bwrap));
            }
        }
    }
    for dir in ["/usr/bin", "/bin"] {
        let unshare = Path::new(dir).join("unshare");
        if unshare.is_file() {
            for flag in ["-rn", "-n"] {
                let mut c = Command::new(&unshare);
                c.args([flag, "true"]);
                if ok(c) {
                    return Some(Isolation::Unshare(unshare, flag));
                }
            }
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
fn probe() -> Option<Isolation> {
    None
}

/// The command prefix for `isolation`; the caller appends the converter and its arguments.
/// `work` is the only writable folder.
fn wrapped(isolation: &Isolation, exe: &Path, work: &Path) -> Command {
    match isolation {
        Isolation::Bwrap(bwrap) => {
            let mut c = Command::new(bwrap);
            c.args([
                "--unshare-net",
                "--unshare-pid",
                "--unshare-ipc",
                "--die-with-parent",
                "--ro-bind",
                "/",
                "/",
                "--dev",
                "/dev",
                "--proc",
                "/proc",
                "--tmpfs",
                "/tmp",
                "--tmpfs",
                "/home",
                "--tmpfs",
                "/root",
                "--bind",
            ])
            .arg(work)
            .arg(work)
            .arg("--chdir")
            .arg(work)
            .arg("--")
            .arg(exe);
            c
        }
        Isolation::Unshare(unshare, flag) => {
            let mut c = Command::new(unshare);
            c.arg(flag).arg("--").arg(exe);
            c
        }
    }
}

/// A command that runs `exe` with no network, or the plain command when the owner
/// explicitly allowed unsandboxed conversion. Fails closed otherwise.
pub(crate) fn command(cfg: &DocToolsConfig, exe: &Path, work: &Path) -> Result<Command, ToolError> {
    #[cfg(test)]
    if FORCE_NONE.with(std::cell::Cell::get) {
        return command_with(cfg, None, exe, work);
    }
    command_with(cfg, detect(), exe, work)
}

#[cfg(test)]
thread_local! {
    /// Tests of the converter plumbing run unsandboxed (their fakes live in /tmp); the
    /// sandbox tests switch the real probe back on for their own thread.
    static FORCE_NONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(crate) fn force_no_sandbox(on: bool) {
    FORCE_NONE.with(|c| c.set(on));
}

pub(crate) fn command_with(
    cfg: &DocToolsConfig,
    isolation: Option<Isolation>,
    exe: &Path,
    work: &Path,
) -> Result<Command, ToolError> {
    // Windows: the owner switch comes first; there is no sandbox to probe.
    if cfg!(windows) {
        return Ok(Command::new(exe));
    }
    match isolation {
        Some(i) => Ok(wrapped(&i, exe, work)),
        None if cfg.convert.allow_unsandboxed_convert => Ok(Command::new(exe)),
        None => Err(exec(UNAVAILABLE)),
    }
}

/// Windows is opt-in: refuse unless the owner enabled it.
pub(crate) fn platform_gate(cfg: &DocToolsConfig) -> Result<(), ToolError> {
    if cfg!(windows) && !cfg.convert.convert_enabled_on_windows {
        return Err(exec(
            "document conversion is switched off on Windows (there is no network sandbox here); the owner can set convert_enabled_on_windows = true for trusted documents only",
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------- path guard

/// Reject, by text alone (NO filesystem call, so nothing touches the network), any path
/// that could name a remote or device location. RULE: a UNC path (`\\host\share`,
/// `//host/share`), a device / extended path (`\\?\`, `\\.\`, `\??\`), an alternate data
/// stream or drive-relative form (a `:` anywhere but right after a drive letter) and
/// control characters. Stops the SMB authentication / NTLM leak that a `stat` on a UNC
/// path would trigger before any other guard runs.
pub(crate) fn unsafe_path_reason(path: &str, windows: bool) -> Option<&'static str> {
    let b = path.as_bytes();
    let slash = |c: u8| c == b'\\' || c == b'/';
    if path.trim().is_empty() {
        return Some("the path is empty");
    }
    if path.chars().any(char::is_control) {
        return Some("the path contains control characters");
    }
    if b.len() >= 2 && slash(b[0]) && slash(b[1]) {
        return Some("network (UNC) paths are not allowed");
    }
    let norm = path.replace('/', "\\");
    if norm.contains("\\\\?\\") || norm.contains("\\\\.\\") || norm.contains("\\??\\") {
        return Some("device paths are not allowed");
    }
    if windows {
        let drive = b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':';
        if drive && !(b.len() >= 3 && slash(b[2])) {
            return Some("give the full path including the folder after the drive");
        }
        let rest = if drive { &path[2..] } else { path };
        if rest.contains(':') {
            return Some("paths with ':' (streams, devices) are not allowed");
        }
        if !drive && slash(b[0]) {
            return Some("give a full local path starting with a drive letter");
        }
    }
    None
}

/// Apply [`unsafe_path_reason`] for this platform.
pub(crate) fn guard_path(path: &str) -> Result<(), ToolError> {
    match unsafe_path_reason(path, cfg!(windows)) {
        Some(why) => Err(invalid(format!("{why}: {path}"))),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_no_unc_device_or_stream_paths_by_text() {
        for bad in [
            r"\\attacker\share\a.docx",
            "//attacker/share/a.docx",
            r"\\?\UNC\attacker\share\a.docx",
            r"\\.\pipe\x.docx",
            r"C:\x\\?\a.docx",
            r"\??\UNC\a\b.docx",
            r"C:\docs\a.docx:evil",
            r"C:\docs\CON:a.docx",
            "C:a.docx",
            r"\rooted\a.docx",
            "",
        ] {
            assert!(unsafe_path_reason(bad, true).is_some(), "windows: {bad:?}");
        }
        // These are refused on every platform, before any filesystem call.
        for bad in [r"\\h\s\a.docx", "//h/s/a.docx", r"/x/\\?\y.docx"] {
            assert!(unsafe_path_reason(bad, false).is_some(), "unix: {bad:?}");
        }
        for ok in [r"C:\Users\me\a.docx", "C:/Users/me/a.docx", "offer.docx"] {
            assert!(unsafe_path_reason(ok, true).is_none(), "{ok}");
        }
        for ok in ["/home/me/a.docx", "/tmp/a:b.docx", "docs/a.docx"] {
            assert!(unsafe_path_reason(ok, false).is_none(), "{ok}");
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn rule_fail_closed_without_a_sandbox_unless_the_owner_allows_it() {
        let mut cfg = DocToolsConfig::default();
        let err = command_with(&cfg, None, Path::new("/bin/true"), Path::new("/tmp")).unwrap_err();
        assert!(
            err.to_string().contains("network isolation unavailable"),
            "{err}"
        );
        cfg.convert.allow_unsandboxed_convert = true;
        assert!(command_with(&cfg, None, Path::new("/bin/true"), Path::new("/tmp")).is_ok());
        // With a sandbox available it is used, whatever the flag says.
        let iso = Isolation::Unshare(PathBuf::from("/usr/bin/unshare"), "-rn");
        let c = command_with(&cfg, Some(iso), Path::new("/bin/true"), Path::new("/tmp")).unwrap();
        assert!(c.get_program().to_string_lossy().ends_with("unshare"));
    }

    #[cfg(windows)]
    #[test]
    fn rule_windows_conversion_is_off_unless_the_owner_enables_it() {
        let mut cfg = DocToolsConfig::default();
        let err = platform_gate(&cfg).unwrap_err();
        assert!(
            err.to_string().contains("convert_enabled_on_windows"),
            "{err}"
        );
        cfg.convert.convert_enabled_on_windows = true;
        assert!(platform_gate(&cfg).is_ok());
    }
}
