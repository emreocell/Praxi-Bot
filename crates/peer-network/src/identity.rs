//! Persistent endpoint identity (one iroh secret key per install).

use std::path::Path;

use iroh::SecretKey;

use crate::error::{PeerError, Result};

fn io(e: &std::io::Error) -> PeerError {
    PeerError::Storage(e.to_string())
}

/// Load the secret key at `path`, creating (and restricting) it if missing.
///
/// The file holds the 32 raw key bytes as hex. On Unix it is created with mode
/// 0600; on Windows its ACL is reduced to the current user on a best-effort basis.
///
/// # Errors
///
/// Returns [`PeerError::Storage`] if the file cannot be read, written or parsed.
pub fn load_or_create_secret_key(path: &Path) -> Result<SecretKey> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse_key(text.trim()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| io(&e))?;
            }
            let key = SecretKey::generate();
            write_private(path, &to_hex(&key.to_bytes()))?;
            Ok(key)
        }
        Err(e) => Err(io(&e)),
    }
}

fn to_hex(bytes: &[u8]) -> String {
    data_encoding::HEXLOWER.encode(bytes)
}

fn parse_key(text: &str) -> Result<SecretKey> {
    let bytes = data_encoding::HEXLOWER
        .decode(text.as_bytes())
        .map_err(|e| PeerError::Storage(format!("identity key is not valid hex: {e}")))?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| PeerError::Storage("identity key must be 32 bytes".into()))?;
    Ok(SecretKey::from_bytes(&arr))
}

#[cfg(unix)]
fn write_private(path: &Path, contents: &str) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| io(&e))?;
    f.write_all(contents.as_bytes()).map_err(|e| io(&e))
}

#[cfg(not(unix))]
fn write_private(path: &Path, contents: &str) -> Result<()> {
    std::fs::write(path, contents).map_err(|e| io(&e))?;
    restrict_windows(path);
    Ok(())
}

/// Best effort: drop inherited ACL entries and grant only the current user.
#[cfg(not(unix))]
fn restrict_windows(path: &Path) {
    if let Ok(user) = std::env::var("USERNAME") {
        let _ = std::process::Command::new("icacls")
            .arg(path)
            .args(["/inheritance:r", "/grant:r"])
            .arg(format!("{user}:F"))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

/// Short fixed-length string form of a public key used in files and CLIs.
#[must_use]
pub fn key_to_string(key: &iroh::PublicKey) -> String {
    to_hex(key.as_bytes())
}

/// Parse the string form produced by [`key_to_string`].
///
/// # Errors
///
/// Returns [`PeerError::Protocol`] for malformed input.
pub fn key_from_string(s: &str) -> Result<iroh::PublicKey> {
    let bytes = data_encoding::HEXLOWER
        .decode(s.trim().to_ascii_lowercase().as_bytes())
        .map_err(|e| PeerError::Protocol(format!("bad public key: {e}")))?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| PeerError::Protocol("public key must be 32 bytes".into()))?;
    iroh::PublicKey::from_bytes(&arr)
        .map_err(|e| PeerError::Protocol(format!("bad public key: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_persists_across_loads() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub").join("identity.key");
        let a = load_or_create_secret_key(&p).unwrap();
        let b = load_or_create_secret_key(&p).unwrap();
        assert_eq!(a.public(), b.public());
    }

    #[test]
    fn corrupt_key_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("identity.key");
        std::fs::write(&p, "zz").unwrap();
        assert!(load_or_create_secret_key(&p).is_err());
    }

    #[test]
    fn public_key_string_round_trip() {
        let k = SecretKey::generate().public();
        assert_eq!(key_from_string(&key_to_string(&k)).unwrap(), k);
    }
}
