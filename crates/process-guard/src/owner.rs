//! Proof that the process behind `.daemon-endpoint.json` is this install's daemon.
//!
//! The endpoint file is plain data: any same-user process can replace it while
//! the real daemon holds the lock. So before any bearer token is sent to a
//! recorded endpoint, the recorded pid must (1) be the only owner of the
//! listening socket and (2) run this installation's `bot` image. Anything else
//! fails closed: treat it as "no daemon" and never send the token.
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::lifecycle::{Endpoint, discover};

/// Plain-language error for the user when the recorded endpoint is not ours.
pub const FOREIGN_PORT_MESSAGE: &str = "another program is using the Praxi port; refusing to send the API token. Close it or restart Praxi.";

/// OS facts the decision needs; a fake in tests, the kernel in production.
pub trait Probe {
    /// Full image path of `pid`; `None` when it cannot be opened (fail closed).
    fn image_path(&self, pid: u32) -> Option<PathBuf>;
    /// Every pid listening on loopback `port`.
    ///
    /// # Errors
    /// The owner table cannot be read.
    fn listener_pids(&self, port: u16) -> Result<BTreeSet<u32>, String>;
}

/// Why a recorded endpoint is not trusted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejection {
    /// The socket is owned by someone other than the recorded pid (or by several).
    Listener,
    /// The recorded pid does not run this install's `bot` binary.
    Image,
}

/// Canonical equality: resolves `\?\` prefixes, 8.3 names, `..` and case. A path
/// that cannot be resolved never matches.
#[must_use]
pub fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a
            .to_string_lossy()
            .eq_ignore_ascii_case(&b.to_string_lossy()),
        _ => false,
    }
}

/// Pure decision, see the module docs.
///
/// # Errors
/// [`Rejection`] when the endpoint must not receive a token.
pub fn verify_with(
    endpoint: Endpoint,
    expected: &[PathBuf],
    probe: &dyn Probe,
) -> Result<(), Rejection> {
    let pids = probe
        .listener_pids(endpoint.port)
        .map_err(|_| Rejection::Listener)?;
    if pids.is_empty() || pids.iter().any(|&p| p != endpoint.pid) {
        return Err(Rejection::Listener);
    }
    let image = probe.image_path(endpoint.pid).ok_or(Rejection::Image)?;
    if expected.iter().any(|e| same_file(e, &image)) {
        Ok(())
    } else {
        Err(Rejection::Image)
    }
}

/// [`discover`] plus the ownership proof. `Ok(None)`: no daemon holds the lock.
///
/// # Errors
/// A lock holder published an endpoint that is not one of `expected` (Windows;
/// other platforms keep their existing socket-owner check in the apps).
pub fn discover_trusted(data: &Path, expected: &[PathBuf]) -> Result<Option<Endpoint>, Rejection> {
    let Some(endpoint) = discover(data) else {
        return Ok(None);
    };
    verify(endpoint, expected).map(|()| Some(endpoint))
}

/// [`verify_with`] against the real OS.
///
/// # Errors
/// See [`verify_with`].
#[cfg(windows)]
pub fn verify(endpoint: Endpoint, expected: &[PathBuf]) -> Result<(), Rejection> {
    verify_with(endpoint, expected, &crate::windows::SystemProbe)
}

/// Non-Windows: the apps' own `/proc` / `lsof` socket-owner check applies.
///
/// # Errors
/// Never.
#[cfg(not(windows))]
pub fn verify(_endpoint: Endpoint, _expected: &[PathBuf]) -> Result<(), Rejection> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct Fake {
        images: HashMap<u32, PathBuf>,
        listeners: BTreeSet<u32>,
    }
    impl Probe for Fake {
        fn image_path(&self, pid: u32) -> Option<PathBuf> {
            self.images.get(&pid).cloned()
        }
        fn listener_pids(&self, _port: u16) -> Result<BTreeSet<u32>, String> {
            Ok(self.listeners.clone())
        }
    }

    fn endpoint(pid: u32) -> Endpoint {
        Endpoint {
            pid,
            port: 50000,
            requested_port: 47821,
        }
    }

    fn touch(dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, b"x").unwrap();
        p
    }

    #[test]
    fn only_the_recorded_pid_running_our_image_is_trusted() {
        let dir = tempfile::tempdir().unwrap();
        let ours = touch(dir.path(), "bot.exe");
        let evil = touch(dir.path(), "evil.exe");
        let expected = [ours.clone()];
        let fake = |listeners: &[u32], images: &[(u32, &PathBuf)]| Fake {
            images: images.iter().map(|(p, i)| (*p, (*i).clone())).collect(),
            listeners: listeners.iter().copied().collect(),
        };
        // pid and image match
        let ok = fake(&[7], &[(7, &ours)]);
        assert_eq!(verify_with(endpoint(7), &expected, &ok), Ok(()));
        // wrong image path
        let wrong_image = fake(&[7], &[(7, &evil)]);
        assert_eq!(
            verify_with(endpoint(7), &expected, &wrong_image),
            Err(Rejection::Image)
        );
        // pid cannot be opened
        let unopened = fake(&[7], &[]);
        assert_eq!(
            verify_with(endpoint(7), &expected, &unopened),
            Err(Rejection::Image)
        );
        // JSON replaced with a foreign pid: the real daemon (pid 7) owns the socket
        let replaced = fake(&[7], &[(7, &ours), (99, &ours)]);
        assert_eq!(
            verify_with(endpoint(99), &expected, &replaced),
            Err(Rejection::Listener)
        );
        // attacker pid recorded and listening, but not our binary
        let squatter = fake(&[99], &[(99, &evil)]);
        assert_eq!(
            verify_with(endpoint(99), &expected, &squatter),
            Err(Rejection::Image)
        );
        // nobody listens, or an extra listener shares the socket
        assert_eq!(
            verify_with(endpoint(7), &expected, &fake(&[], &[(7, &ours)])),
            Err(Rejection::Listener)
        );
        assert_eq!(
            verify_with(endpoint(7), &expected, &fake(&[7, 8], &[(7, &ours)])),
            Err(Rejection::Listener)
        );
    }

    #[test]
    fn path_comparison_is_canonical_not_textual() {
        let dir = tempfile::tempdir().unwrap();
        let file = touch(dir.path(), "bot.exe");
        let dotted = dir.path().join(".").join("bot.exe");
        assert!(same_file(&file, &dotted));
        assert!(!same_file(&file, &dir.path().join("missing.exe")));
    }
}
