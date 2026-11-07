//! Encrypted file backend for headless machines without a keychain.
//!
//! The whole secret map is serialised to JSON and sealed with
//! `ChaCha20-Poly1305`; the key is derived from the passphrase with Argon2id
//! (a fresh random salt and nonce on every write). KDF cost parameters are kept
//! in the file header so they can be raised later without breaking old files.

use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
};

use argon2::{Algorithm, Argon2, Params, Version};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use chacha20poly1305::{
    ChaCha20Poly1305, Key, Nonce,
    aead::{Aead, KeyInit, OsRng, rand_core::RngCore},
};
use serde::{Deserialize, Serialize};

use crate::{
    error::SecretError,
    secret::{SecretStore, SecretValue, validate_name},
};

const FORMAT_VERSION: u32 = 1;

/// Argon2id cost parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KdfParams {
    /// Memory in KiB.
    pub m_cost: u32,
    /// Iterations.
    pub t_cost: u32,
    /// Lanes.
    pub p_cost: u32,
}

impl Default for KdfParams {
    /// OWASP-recommended Argon2id minimum (19 MiB, 2 iterations, 1 lane).
    fn default() -> Self {
        Self {
            m_cost: 19 * 1024,
            t_cost: 2,
            p_cost: 1,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct FileFormat {
    version: u32,
    kdf: KdfParams,
    salt: String,
    nonce: String,
    ciphertext: String,
}

/// [`SecretStore`] backed by a passphrase-encrypted file.
pub struct EncryptedFileStore {
    path: PathBuf,
    passphrase: String,
    kdf: KdfParams,
}

impl std::fmt::Debug for EncryptedFileStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EncryptedFileStore")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl EncryptedFileStore {
    /// Open (lazily) the store at `path`; the file is created on first write.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>, passphrase: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            passphrase: passphrase.into(),
            kdf: KdfParams::default(),
        }
    }

    /// Use custom KDF parameters for newly written files (tests use cheap ones).
    #[must_use]
    pub fn with_kdf(mut self, kdf: KdfParams) -> Self {
        self.kdf = kdf;
        self
    }

    /// Path of the backing file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Verify the passphrase against an existing file (no-op if absent).
    ///
    /// # Errors
    /// [`SecretError::WrongPassphrase`] or I/O / format errors.
    pub fn verify(&self) -> Result<(), SecretError> {
        self.load().map(|_| ())
    }

    fn derive_key(&self, salt: &[u8], kdf: KdfParams) -> Result<[u8; 32], SecretError> {
        let params = Params::new(kdf.m_cost, kdf.t_cost, kdf.p_cost, Some(32))
            .map_err(|e| SecretError::Corrupt(format!("kdf params: {e}")))?;
        let mut key = [0u8; 32];
        Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
            .hash_password_into(self.passphrase.as_bytes(), salt, &mut key)
            .map_err(|e| SecretError::Corrupt(format!("kdf: {e}")))?;
        Ok(key)
    }

    fn load(&self) -> Result<BTreeMap<String, String>, SecretError> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(e) => return Err(e.into()),
        };
        let f: FileFormat =
            serde_json::from_str(&text).map_err(|e| SecretError::Corrupt(e.to_string()))?;
        if f.version != FORMAT_VERSION {
            return Err(SecretError::Corrupt(format!(
                "unsupported version {}",
                f.version
            )));
        }
        let dec = |s: &str| {
            B64.decode(s)
                .map_err(|e| SecretError::Corrupt(format!("base64: {e}")))
        };
        let salt = dec(&f.salt)?;
        let nonce = dec(&f.nonce)?;
        let ct = dec(&f.ciphertext)?;
        if nonce.len() != 12 {
            return Err(SecretError::Corrupt("bad nonce length".into()));
        }
        let key = self.derive_key(&salt, f.kdf)?;
        let plain = ChaCha20Poly1305::new(Key::from_slice(&key))
            .decrypt(Nonce::from_slice(&nonce), ct.as_ref())
            .map_err(|_| SecretError::WrongPassphrase)?;
        serde_json::from_slice(&plain).map_err(|e| SecretError::Corrupt(e.to_string()))
    }

    fn save(&self, map: &BTreeMap<String, String>) -> Result<(), SecretError> {
        let mut salt = [0u8; 16];
        OsRng.fill_bytes(&mut salt);
        let mut nonce = [0u8; 12];
        OsRng.fill_bytes(&mut nonce);
        let key = self.derive_key(&salt, self.kdf)?;
        let plain = serde_json::to_vec(map).map_err(|e| SecretError::Corrupt(e.to_string()))?;
        let ct = ChaCha20Poly1305::new(Key::from_slice(&key))
            .encrypt(Nonce::from_slice(&nonce), plain.as_ref())
            .map_err(|_| SecretError::Corrupt("encryption failed".into()))?;
        let file = FileFormat {
            version: FORMAT_VERSION,
            kdf: self.kdf,
            salt: B64.encode(salt),
            nonce: B64.encode(nonce),
            ciphertext: B64.encode(ct),
        };
        let json =
            serde_json::to_vec_pretty(&file).map_err(|e| SecretError::Corrupt(e.to_string()))?;
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("tmp");
        {
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            let mut f = opts.open(&tmp)?;
            f.write_all(&json)?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

impl SecretStore for EncryptedFileStore {
    fn get(&self, name: &str) -> Result<Option<SecretValue>, SecretError> {
        Ok(self.load()?.remove(name).map(SecretValue::new))
    }

    fn set(&self, name: &str, value: &str) -> Result<(), SecretError> {
        validate_name(name)?;
        let mut map = self.load()?;
        map.insert(name.to_string(), value.to_string());
        self.save(&map)
    }

    fn delete(&self, name: &str) -> Result<bool, SecretError> {
        let mut map = self.load()?;
        let existed = map.remove(name).is_some();
        if existed {
            self.save(&map)?;
        }
        Ok(existed)
    }

    fn list_names(&self) -> Result<Vec<String>, SecretError> {
        Ok(self.load()?.into_keys().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHEAP: KdfParams = KdfParams {
        m_cost: 8,
        t_cost: 1,
        p_cost: 1,
    };

    fn store(dir: &Path, pass: &str) -> EncryptedFileStore {
        EncryptedFileStore::new(dir.join("secrets.enc"), pass).with_kdf(CHEAP)
    }

    #[test]
    fn round_trip() {
        let d = tempfile::tempdir().unwrap();
        let s = store(d.path(), "pw");
        assert_eq!(s.list_names().unwrap(), Vec::<String>::new());
        s.set("a", "alpha").unwrap();
        s.set("b", "beta").unwrap();
        // A fresh handle with the same passphrase sees the data.
        let s2 = store(d.path(), "pw");
        assert_eq!(s2.get("a").unwrap().unwrap().expose(), "alpha");
        assert_eq!(s2.list_names().unwrap(), ["a", "b"]);
        assert!(s2.delete("a").unwrap());
        assert!(s2.get("a").unwrap().is_none());
    }

    #[test]
    fn file_does_not_contain_plaintext() {
        let d = tempfile::tempdir().unwrap();
        let s = store(d.path(), "pw");
        s.set("name", "super-secret-value").unwrap();
        let raw = std::fs::read_to_string(s.path()).unwrap();
        assert!(!raw.contains("super-secret-value"));
    }

    #[test]
    fn wrong_passphrase_fails() {
        let d = tempfile::tempdir().unwrap();
        store(d.path(), "right").set("a", "1").unwrap();
        let bad = store(d.path(), "wrong");
        assert!(matches!(bad.get("a"), Err(SecretError::WrongPassphrase)));
        assert!(matches!(bad.verify(), Err(SecretError::WrongPassphrase)));
        assert!(matches!(
            bad.set("b", "2"),
            Err(SecretError::WrongPassphrase)
        ));
    }

    #[test]
    fn corrupt_file_is_reported() {
        let d = tempfile::tempdir().unwrap();
        let s = store(d.path(), "pw");
        std::fs::write(s.path(), "not json").unwrap();
        assert!(matches!(s.get("a"), Err(SecretError::Corrupt(_))));
    }
}
