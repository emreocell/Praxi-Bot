//! Praxi's own `known_hosts` (never the user's). Trust on first use with an
//! explicit, caller-confirmed fingerprint; any later difference is a hard stop.
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD},
};
use sha2::{Digest, Sha256};

use crate::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedKey {
    pub key_type: String,
    pub key_b64: String,
}

impl ScannedKey {
    /// `SHA256:...` exactly as `ssh-keygen -l` prints it.
    pub fn fingerprint(&self) -> String {
        let blob = STANDARD.decode(&self.key_b64).unwrap_or_default();
        format!("SHA256:{}", STANDARD_NO_PAD.encode(Sha256::digest(blob)))
    }

    fn rank(&self) -> u8 {
        match self.key_type.as_str() {
            "ssh-ed25519" => 0,
            "ecdsa-sha2-nistp256" => 1,
            "rsa-sha2-512" | "rsa-sha2-256" | "ssh-rsa" => 2,
            _ => 3,
        }
    }
}

/// Parse `ssh-keyscan` stdout: `host keytype base64` lines; comments ignored.
pub fn parse_scan(text: &str) -> Vec<ScannedKey> {
    text.lines()
        .filter(|l| !l.starts_with('#'))
        .take(16)
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let (_host, kt, b64) = (f.next()?, f.next()?, f.next()?);
            let kt_ok = kt.len() < 64
                && kt
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '@' | '.' | '_'));
            let b64_ok = STANDARD
                .decode(b64)
                .is_ok_and(|b| !b.is_empty() && b.len() < 4096);
            (kt_ok && b64_ok).then(|| ScannedKey {
                key_type: kt.to_owned(),
                key_b64: b64.to_owned(),
            })
        })
        .collect()
}

/// Parse the throw-away known_hosts file `ssh` wrote on first contact
/// (`StrictHostKeyChecking=accept-new`, unhashed): only lines for this very
/// endpoint count (`host` on port 22, `[host]:port` otherwise).
pub fn parse_known_hosts_scan(text: &str, host: &str, port: u16) -> Vec<ScannedKey> {
    let name = entry_name(host, port);
    let lines: String = text
        .lines()
        .filter(|l| {
            l.split_whitespace()
                .next()
                .is_some_and(|hosts| hosts.split(',').any(|h| h == name))
        })
        .collect::<Vec<_>>()
        .join("\n");
    parse_scan(&lines)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostKeyState {
    /// Pinned earlier and the server still presents it.
    Trusted { fingerprint: String },
    /// Never seen: the caller must show `key.fingerprint()` and ask.
    Unknown { key: ScannedKey },
    /// Pinned, but the server presents something else. Hard stop.
    Mismatch { pinned: String, seen: String },
}

pub struct KnownHosts {
    path: PathBuf,
}

/// The name ssh uses in known_hosts for this endpoint.
pub fn entry_name(host: &str, port: u16) -> String {
    if port == 22 {
        host.to_owned()
    } else {
        format!("[{host}]:{port}")
    }
}

impl KnownHosts {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn lines(&self) -> Vec<String> {
        fs::read_to_string(&self.path)
            .map(|t| t.lines().map(str::to_owned).collect())
            .unwrap_or_default()
    }

    /// Key types already pinned for this endpoint (to ask the server for the
    /// same type, so a changed key shows up as a mismatch).
    pub fn pinned_types(&self, host: &str, port: u16) -> Vec<String> {
        let mut types: Vec<String> = self
            .pinned(&entry_name(host, port))
            .into_iter()
            .map(|k| k.key_type)
            .filter(|t| !t.is_empty())
            .collect();
        types.dedup();
        types
    }

    fn pinned(&self, name: &str) -> Vec<ScannedKey> {
        self.lines()
            .iter()
            .filter_map(|l| {
                let mut f = l.split_whitespace();
                let hosts = f.next()?;
                hosts.split(',').any(|h| h == name).then(|| ScannedKey {
                    key_type: f.next().unwrap_or_default().to_owned(),
                    key_b64: f.next().unwrap_or_default().to_owned(),
                })
            })
            .collect()
    }

    pub fn check(
        &self,
        host: &str,
        port: u16,
        scanned: &[ScannedKey],
    ) -> Result<HostKeyState, Error> {
        let pinned = self.pinned(&entry_name(host, port));
        if pinned.is_empty() {
            let key = scanned
                .iter()
                .min_by_key(|k| k.rank())
                .cloned()
                .ok_or_else(|| Error::Failed("the server offered no host key".into()))?;
            return Ok(HostKeyState::Unknown { key });
        }
        if let Some(hit) = pinned.iter().find(|p| scanned.contains(p)) {
            return Ok(HostKeyState::Trusted {
                fingerprint: hit.fingerprint(),
            });
        }
        Ok(HostKeyState::Mismatch {
            pinned: pinned[0].fingerprint(),
            seen: scanned
                .first()
                .map_or_else(|| "(no key offered)".into(), ScannedKey::fingerprint),
        })
    }

    /// Pin a key the user explicitly accepted.
    pub fn pin(&self, host: &str, port: u16, key: &ScannedKey) -> Result<(), Error> {
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(
            file,
            "{} {} {}",
            entry_name(host, port),
            key.key_type,
            key.key_b64
        )?;
        Ok(())
    }

    /// Forget every key for this endpoint (used by `remove`).
    pub fn forget(&self, host: &str, port: u16) -> Result<(), Error> {
        let name = entry_name(host, port);
        let keep: Vec<String> = self
            .lines()
            .into_iter()
            .filter(|l| {
                l.split_whitespace()
                    .next()
                    .is_none_or(|h| !h.split(',').any(|x| x == name))
            })
            .collect();
        if self.path.exists() {
            let mut text = keep.join("\n");
            if !keep.is_empty() {
                text.push('\n');
            }
            fs::write(&self.path, text)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ED: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl";

    #[test]
    fn the_scan_file_is_parsed_for_the_right_endpoint_only() {
        let text = format!(
            "203.0.113.10 ssh-ed25519 {ED}\n[203.0.113.10]:2222 ssh-ed25519 {ED}\nother.example ssh-ed25519 {ED}\n"
        );
        let default = parse_known_hosts_scan(&text, "203.0.113.10", 22);
        assert_eq!(default.len(), 1);
        assert!(default[0].fingerprint().starts_with("SHA256:"));
        let custom = parse_known_hosts_scan(&text, "203.0.113.10", 2222);
        assert_eq!(custom.len(), 1);
        assert_eq!(parse_known_hosts_scan(&text, "203.0.113.10", 2200), []);
        assert_eq!(parse_known_hosts_scan("", "h", 22), []);
    }

    #[test]
    fn tofu_then_trusted_then_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let kh = KnownHosts::new(dir.path().join("known_hosts"));
        let scan = parse_scan(&format!("# x:22 SSH-2.0\nexample.com ssh-ed25519 {ED}\n"));
        assert_eq!(scan.len(), 1);
        let HostKeyState::Unknown { key } = kh.check("example.com", 22, &scan).unwrap() else {
            panic!("first contact must be unknown")
        };
        assert!(key.fingerprint().starts_with("SHA256:"));
        kh.pin("example.com", 22, &key).unwrap();
        assert!(matches!(
            kh.check("example.com", 22, &scan).unwrap(),
            HostKeyState::Trusted { .. }
        ));
        // another port is a different endpoint
        assert!(matches!(
            kh.check("example.com", 2222, &scan).unwrap(),
            HostKeyState::Unknown { .. }
        ));
        let other = vec![ScannedKey {
            key_type: "ssh-ed25519".into(),
            key_b64: STANDARD.encode(b"someone else"),
        }];
        assert!(matches!(
            kh.check("example.com", 22, &other).unwrap(),
            HostKeyState::Mismatch { .. }
        ));
        kh.forget("example.com", 22).unwrap();
        assert!(matches!(
            kh.check("example.com", 22, &scan).unwrap(),
            HostKeyState::Unknown { .. }
        ));
    }
}
