//! The only way the engine talks to a server.
//!
//! Remote commands are an enum of constant strings: there is no API that takes
//! caller text as a command. Parameters travel on stdin as validated
//! `key=value` lines that `provision.sh` re-validates on the server.
use std::time::Duration;

use crate::known_hosts::ScannedKey;

/// Files the app uploads into the per-user staging directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageFile {
    Provision,
    Installer,
    SetupHeadless,
    /// The capability pack catalog (`packs.txt`).
    Packs,
    /// The web scraping add-on's program, pinned requirements and service unit (`packs/scrape/`).
    ScrapeService,
    ScrapeLock,
    ScrapeUnit,
}

/// `provision.sh` subcommands (fixed set).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sub {
    Preflight,
    InstallGh,
    Install,
    Snapshot,
    Status,
    Invite,
    Devices,
    Screen,
    /// Take screen view + input back from a device (rollback of a pairing).
    ScreenRevoke,
    Revoke,
    Uninstall,
    Purge,
    /// Capability packs: read-only facts, dry-run plan (no root), install and remove (root).
    PacksFacts,
    PacksPlan,
    PackInstall,
    PackRemove,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteCmd {
    /// Prints `praxi-ok`; proves login and a working POSIX `sh`.
    Probe,
    Stage(StageFile),
    Provision(Sub),
    /// Remove the staging directory.
    Cleanup,
}

// Everything runs inside `sh -c '...'` so the user's login shell (bash, zsh,
// fish, csh) never parses our syntax. Only double quotes appear in the body.
macro_rules! stage_cmd {
    ($file:literal) => {
        concat!(
            "sh -c 'umask 077; mkdir -p \"$HOME/.praxi-prov\" && cat > \"$HOME/.praxi-prov/",
            $file,
            "\"'"
        )
    };
}
// Unprivileged helper call.
macro_rules! user_cmd {
    ($sub:literal) => {
        concat!("sh -c 'bash \"$HOME/.praxi-prov/provision.sh\" ", $sub, "'")
    };
}
// Root (or `sudo -n`) call: the staging path is expanded by the inner sh
// before sudo runs, so root never sees the user's environment.
macro_rules! root_cmd {
    ($sub:literal) => {
        concat!(
            "sh -c 'S=; [ \"$(id -u)\" -eq 0 ] || S=\"sudo -n\"; $S bash \"$HOME/.praxi-prov/provision.sh\" ",
            $sub,
            "'"
        )
    };
}

impl RemoteCmd {
    /// The exact command string handed to ssh. Always a constant.
    pub fn command_line(self) -> &'static str {
        match self {
            Self::Probe => "sh -c 'echo praxi-ok'",
            Self::Cleanup => "sh -c 'rm -rf \"$HOME/.praxi-prov\"'",
            Self::Stage(StageFile::Provision) => stage_cmd!("provision.sh"),
            Self::Stage(StageFile::Installer) => stage_cmd!("install-linux.sh"),
            Self::Stage(StageFile::SetupHeadless) => stage_cmd!("setup-headless.sh"),
            Self::Stage(StageFile::Packs) => stage_cmd!("packs.txt"),
            Self::Stage(StageFile::ScrapeService) => stage_cmd!("praxi_scrape.py"),
            Self::Stage(StageFile::ScrapeLock) => stage_cmd!("scrape-requirements.lock"),
            Self::Stage(StageFile::ScrapeUnit) => stage_cmd!("praxi-scrape.service"),
            Self::Provision(Sub::Preflight) => user_cmd!("preflight"),
            Self::Provision(Sub::Status) => user_cmd!("status"),
            Self::Provision(Sub::InstallGh) => root_cmd!("install-gh"),
            Self::Provision(Sub::Install) => root_cmd!("install"),
            Self::Provision(Sub::Snapshot) => root_cmd!("snapshot"),
            Self::Provision(Sub::Invite) => root_cmd!("invite"),
            Self::Provision(Sub::Devices) => root_cmd!("devices"),
            Self::Provision(Sub::Screen) => root_cmd!("screen"),
            Self::Provision(Sub::ScreenRevoke) => root_cmd!("screen-revoke"),
            Self::Provision(Sub::Revoke) => root_cmd!("revoke"),
            Self::Provision(Sub::Uninstall) => root_cmd!("uninstall"),
            Self::Provision(Sub::Purge) => root_cmd!("purge"),
            Self::Provision(Sub::PacksFacts) => user_cmd!("packs-facts"),
            Self::Provision(Sub::PacksPlan) => user_cmd!("packs-plan"),
            Self::Provision(Sub::PackInstall) => root_cmd!("pack-install"),
            Self::Provision(Sub::PackRemove) => root_cmd!("pack-remove"),
        }
    }
}

/// Result of a finished remote command. Output is capped and control
/// characters are already replaced: server text is untrusted.
#[derive(Debug, Clone, Default)]
pub struct Output {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
}

impl Output {
    pub fn ok(&self) -> bool {
        self.code == 0
    }
}

/// Why a transport call did not produce an [`Output`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    /// The key the server presents differs from the pinned one. Hard stop.
    #[error("the server's host key does not match the pinned key")]
    HostKeyChanged,
    #[error("the server rejected the login (key or user)")]
    Auth,
    #[error("OpenSSH refused an unprotected private key file")]
    KeyPermissions,
    #[error("the server only offered password authentication")]
    PasswordOnly,
    #[error("could not reach the server: {0}")]
    Network(String),
    #[error("the command timed out")]
    Timeout,
    /// The TCP connection worked but no host key could be read (for example
    /// the two sides share no key exchange method).
    #[error("connected, but could not read the server's host key: {0}")]
    NoHostKey(String),
    #[error("{0}")]
    Other(String),
}

pub trait Transport {
    /// The host key the server offers right now, read by `ssh` itself without
    /// offering any credential.
    fn scan_host_keys(&self) -> Result<Vec<ScannedKey>, TransportError>;

    /// Run one fixed remote command with `stdin`, output capped, bounded time.
    fn run(
        &self,
        cmd: RemoteCmd,
        stdin: &[u8],
        timeout: Duration,
    ) -> Result<Output, TransportError>;

    /// Upload one of the app's own scripts into the staging directory.
    fn upload(&self, file: StageFile, bytes: &[u8]) -> Result<(), TransportError> {
        let out = self.run(RemoteCmd::Stage(file), bytes, Duration::from_secs(60))?;
        if out.ok() {
            Ok(())
        } else {
            Err(TransportError::Other(format!(
                "upload failed (exit {}): {}",
                out.code,
                out.stderr.trim()
            )))
        }
    }
}

/// Replace control characters (including ESC, so ANSI sequences die) and
/// bound the length. Applied to all server text before it is shown or logged.
pub fn sanitize(text: &str, max: usize) -> String {
    let mut out = String::with_capacity(text.len().min(max));
    for c in text.chars() {
        if out.len() >= max {
            out.push_str("...");
            break;
        }
        match c {
            '\n' | '\t' => out.push(c),
            c if c.is_control() => out.push('?'),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_commands_are_constants_wrapped_in_one_single_quoted_sh_body() {
        use Sub::*;
        let all = [
            RemoteCmd::Probe,
            RemoteCmd::Cleanup,
            RemoteCmd::Stage(StageFile::Provision),
            RemoteCmd::Stage(StageFile::Installer),
            RemoteCmd::Stage(StageFile::SetupHeadless),
            RemoteCmd::Stage(StageFile::Packs),
            RemoteCmd::Stage(StageFile::ScrapeService),
            RemoteCmd::Stage(StageFile::ScrapeLock),
            RemoteCmd::Stage(StageFile::ScrapeUnit),
            RemoteCmd::Provision(PacksFacts),
            RemoteCmd::Provision(PacksPlan),
            RemoteCmd::Provision(PackInstall),
            RemoteCmd::Provision(PackRemove),
            RemoteCmd::Provision(Preflight),
            RemoteCmd::Provision(Install),
            RemoteCmd::Provision(Purge),
        ];
        for c in all {
            let line = c.command_line();
            assert!(
                line.starts_with("sh -c '") && line.ends_with('\''),
                "{line}"
            );
            assert_eq!(line.matches('\'').count(), 2, "{line}");
        }
    }

    #[test]
    fn sanitize_strips_escapes() {
        assert_eq!(sanitize("a\x1b[31mred\x07", 100), "a?[31mred?");
    }
}
