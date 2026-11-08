//! Persistent endpoint identity (one iroh secret key per install).

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use iroh::SecretKey;

use crate::error::{PeerError, Result};

fn io(e: &std::io::Error) -> PeerError {
    PeerError::Storage(e.to_string())
}

/// Load the secret key at `path`, creating (and restricting) it if missing.
///
/// The file holds the 32 raw key bytes as hex. New files are private from their
/// creation: mode 0600 on Unix, a protected current-user-only DACL on Windows.
/// Existing files are verified by handle before reading; unsafe permissions,
/// another owner, links and non-regular files are rejected without rewriting.
///
/// # Errors
///
/// Returns [`PeerError::Storage`] if the file cannot be read, written or parsed.
pub fn load_or_create_secret_key(path: &Path) -> Result<SecretKey> {
    let file = match platform::open_existing(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| io(&e))?;
            }
            let key = SecretKey::generate();
            write_private(path, &to_hex(&key.to_bytes()))?;
            return Ok(key);
        }
        Err(e) => return Err(io(&e)),
    };
    platform::verify_private(&file)?;
    // A key is 64 ASCII hex characters, optionally surrounded by whitespace.
    // Do not read an arbitrary file into memory just to discover it is not a key.
    let mut text = String::new();
    file.take(129)
        .read_to_string(&mut text)
        .map_err(|e| io(&e))?;
    if text.len() > 128 {
        return Err(PeerError::Storage("identity key file is too large".into()));
    }
    parse_key(text.trim())
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

fn write_private(path: &Path, contents: &str) -> Result<()> {
    // Security setup is checked before any secret is written. Do not turn a
    // failed ACL operation into a successfully created plaintext key.
    let mut file = platform::create_private(path).map_err(|e| io(&e))?;
    platform::verify_private(&file)?;
    file.write_all(contents.as_bytes()).map_err(|e| io(&e))?;
    file.sync_all().map_err(|e| io(&e))
}

#[cfg(unix)]
mod platform {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    use super::{File, Path, PeerError, Result, io};

    pub(super) fn open_existing(path: &Path) -> std::io::Result<File> {
        // NONBLOCK also prevents a substituted FIFO from hanging before its
        // metadata can be checked. All validation is against this same handle.
        rustix::fs::open(
            path,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK,
            rustix::fs::Mode::empty(),
        )
        .map(File::from)
        .map_err(Into::into)
    }

    pub(super) fn create_private(path: &Path) -> std::io::Result<File> {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
    }

    pub(super) fn verify_private(file: &File) -> Result<()> {
        verify_owner_and_mode(file, rustix::process::geteuid().as_raw())
    }

    fn verify_owner_and_mode(file: &File, owner: u32) -> Result<()> {
        let metadata = file.metadata().map_err(|e| io(&e))?;
        let mode = metadata.mode() & 0o7777;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(PeerError::Storage(
                "identity key must be a regular file with one link".into(),
            ));
        }
        if metadata.uid() != owner {
            return Err(PeerError::Storage(
                "identity key is not owned by the current user".into(),
            ));
        }
        if mode != 0o600 && mode != 0o400 {
            return Err(PeerError::Storage(
                "identity key permissions must be 0600 or 0400".into(),
            ));
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::super::load_or_create_secret_key;
        use super::*;

        #[test]
        fn owner_mismatch_is_rejected_even_with_private_mode() {
            let dir = tempfile::tempdir().unwrap();
            let file = create_private(&dir.path().join("identity.key")).unwrap();
            let uid = file.metadata().unwrap().uid();
            assert!(verify_owner_and_mode(&file, uid.wrapping_add(1)).is_err());
            assert!(verify_owner_and_mode(&file, uid).is_ok());
        }

        #[test]
        fn fifo_is_rejected_without_blocking_on_a_writer() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("identity.key");
            rustix::fs::mkfifoat(
                rustix::fs::CWD,
                &path,
                rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
            )
            .unwrap();
            assert!(load_or_create_secret_key(&path).is_err());
        }
    }
}

// Win32 security APIs have no safe std equivalent. Keep all checked FFI and
// pointer lifetimes inside this module; no unsafe code is needed elsewhere.
#[cfg(windows)]
#[allow(unsafe_code)]
#[path = "identity_windows.rs"]
mod platform;

#[cfg(not(any(unix, windows)))]
mod platform {
    use super::{File, Path, PeerError, Result};

    pub(super) fn open_existing(_path: &Path) -> std::io::Result<File> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "private identity files are unsupported on this platform",
        ))
    }

    pub(super) fn create_private(path: &Path) -> std::io::Result<File> {
        open_existing(path)
    }

    pub(super) fn verify_private(_file: &File) -> Result<()> {
        Err(PeerError::Storage(
            "cannot verify identity permissions on this platform".into(),
        ))
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

    #[cfg(unix)]
    #[test]
    fn loose_permissions_are_rejected_without_rewriting_the_key() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.key");
        load_or_create_secret_key(&path).unwrap();
        let before = std::fs::read(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load_or_create_secret_key(&path).is_err());
        assert_eq!(std::fs::read(path).unwrap(), before);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_identity_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.key");
        load_or_create_secret_key(&target).unwrap();
        let path = dir.path().join("identity.key");
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(load_or_create_secret_key(&path).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn hardlinked_identity_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.key");
        load_or_create_secret_key(&target).unwrap();
        let path = dir.path().join("identity.key");
        std::fs::hard_link(&target, &path).unwrap();
        assert!(load_or_create_secret_key(&path).is_err());
    }

    #[test]
    fn identity_supports_non_ascii_paths() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Masaüstü").join("kimlik-Öcel.key");
        let first = load_or_create_secret_key(&path).unwrap();
        assert_eq!(
            load_or_create_secret_key(&path).unwrap().public(),
            first.public()
        );
    }
}
