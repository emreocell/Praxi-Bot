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
    sync::{Arc, Mutex},
};

use argon2::{Algorithm, Argon2, Params, Version};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use chacha20poly1305::{
    ChaCha20Poly1305, Key, Nonce,
    aead::{Aead, KeyInit, OsRng, rand_core::RngCore},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    error::SecretError,
    secret::{SecretStore, SecretValue, validate_name},
    transaction::{self, TransactionGuard, parent_directory},
    wipe::{self, DerivedKey, SecretBytes, SecretString},
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
#[derive(Clone)]
pub struct EncryptedFileStore {
    path: PathBuf,
    passphrase: SecretString,
    kdf: KdfParams,
    transaction: Arc<Mutex<()>>,
    #[cfg(test)]
    fail_before_rename: Arc<std::sync::atomic::AtomicBool>,
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
            passphrase: SecretString::new(passphrase.into()),
            kdf: KdfParams::default(),
            transaction: Arc::new(Mutex::new(())),
            #[cfg(test)]
            fail_before_rename: Arc::default(),
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
        let _transaction = self.lock()?;
        self.load().map(|_| ())
    }

    fn lock(&self) -> Result<TransactionGuard<'_>, SecretError> {
        let mut sidecar = self.path.as_os_str().to_os_string();
        sidecar.push(".lock");
        transaction::acquire(&self.transaction, Path::new(&sidecar))
    }

    fn derive_key(&self, salt: &[u8], kdf: KdfParams) -> Result<DerivedKey, SecretError> {
        let params = Params::new(kdf.m_cost, kdf.t_cost, kdf.p_cost, Some(32))
            .map_err(|e| SecretError::Corrupt(format!("kdf params: {e}")))?;
        let mut key = DerivedKey::new();
        Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
            .hash_password_into(self.passphrase.as_bytes(), salt, key.as_bytes_mut())
            .map_err(|e| SecretError::Corrupt(format!("kdf: {e}")))?;
        Ok(key)
    }

    fn load(&self) -> Result<SecretMap, SecretError> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(SecretMap::empty()),
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
        let plain = SecretBytes::new(
            ChaCha20Poly1305::new(Key::from_slice(key.as_bytes()))
                .decrypt(Nonce::from_slice(&nonce), ct.as_ref())
                .map_err(|_| SecretError::WrongPassphrase)?,
        );
        let map = serde_json::from_slice(plain.as_ref())
            .map_err(|e| SecretError::Corrupt(e.to_string()))?;
        Ok(SecretMap(map))
    }

    fn save(&self, map: &SecretMap) -> Result<(), SecretError> {
        let mut salt = [0u8; 16];
        OsRng.fill_bytes(&mut salt);
        let mut nonce = [0u8; 12];
        OsRng.fill_bytes(&mut nonce);
        // Wipe the key and the plaintext JSON before the sealed document is written.
        let ct = {
            let key = self.derive_key(&salt, self.kdf)?;
            let plain = SecretBytes::new(map.to_json()?);
            ChaCha20Poly1305::new(Key::from_slice(key.as_bytes()))
                .encrypt(Nonce::from_slice(&nonce), plain.as_ref())
                .map_err(|_| SecretError::Corrupt("encryption failed".into()))?
        };
        let file = FileFormat {
            version: FORMAT_VERSION,
            kdf: self.kdf,
            salt: B64.encode(salt),
            nonce: B64.encode(nonce),
            ciphertext: B64.encode(ct),
        };
        let json =
            serde_json::to_vec_pretty(&file).map_err(|e| SecretError::Corrupt(e.to_string()))?;
        let mut pending = PendingWrite::new(&self.path)?;
        pending.write_and_sync(&json)?;
        #[cfg(test)]
        if self
            .fail_before_rename
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(std::io::Error::other("injected failure before rename").into());
        }
        pending.commit(&self.path)?;
        Ok(())
    }
}

/// Owns only a newly created file; error paths never remove another writer's
/// file. Close the handle before cleanup so this also works on Windows.
struct PendingWrite {
    path: PathBuf,
    file: Option<std::fs::File>,
}

impl PendingWrite {
    fn new(destination: &Path) -> Result<Self, std::io::Error> {
        let directory = parent_directory(destination);
        std::fs::create_dir_all(directory)?;
        let mut name = destination
            .file_name()
            .ok_or_else(|| std::io::Error::other("secret store path has no file name"))?
            .to_os_string();
        name.push(format!(
            ".{}.{}.tmp",
            std::process::id(),
            Uuid::now_v7().simple()
        ));
        let path = directory.join(name);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&path)?;
        Ok(Self {
            path,
            file: Some(file),
        })
    }

    fn write_and_sync(&mut self, bytes: &[u8]) -> Result<(), std::io::Error> {
        let file = self.file.as_mut().expect("temporary file is still open");
        file.write_all(bytes)?;
        file.sync_all()
    }

    fn commit(mut self, destination: &Path) -> Result<(), std::io::Error> {
        // Close the handle first. ReplaceFileW opens the replacement itself.
        drop(self.file.take());
        #[cfg(windows)]
        crate::publish::publish(&self.path, destination)?;
        #[cfg(not(windows))]
        {
            std::fs::rename(&self.path, destination)?;
            std::fs::File::open(parent_directory(destination))?.sync_all()?;
        }
        Ok(())
    }
}

/// In-memory secret map. Names and values are wiped when the map is dropped.
struct SecretMap(BTreeMap<String, String>);

impl SecretMap {
    fn empty() -> Self {
        Self(BTreeMap::new())
    }

    fn insert(&mut self, name: String, value: String) {
        if let Some(mut old) = self.0.insert(name, value) {
            wipe::wipe_string(&mut old);
        }
    }

    fn remove(&mut self, name: &str) -> Option<String> {
        self.0.remove(name)
    }

    fn to_json(&self) -> Result<Vec<u8>, SecretError> {
        serde_json::to_vec(&self.0).map_err(|e| SecretError::Corrupt(e.to_string()))
    }

    fn into_names(mut self) -> Vec<String> {
        let map = std::mem::take(&mut self.0);
        let mut names = Vec::with_capacity(map.len());
        for (name, mut value) in map {
            wipe::wipe_string(&mut value);
            names.push(name);
        }
        names
    }
}

impl Drop for SecretMap {
    fn drop(&mut self) {
        for (mut name, mut value) in std::mem::take(&mut self.0) {
            wipe::wipe_string(&mut name);
            wipe::wipe_string(&mut value);
        }
    }
}

impl Drop for PendingWrite {
    fn drop(&mut self) {
        drop(self.file.take());
        let _ = std::fs::remove_file(&self.path);
    }
}

impl SecretStore for EncryptedFileStore {
    fn get(&self, name: &str) -> Result<Option<SecretValue>, SecretError> {
        let _transaction = self.lock()?;
        let mut map = self.load()?;
        Ok(map.remove(name).map(SecretValue::new))
    }

    fn set(&self, name: &str, value: &str) -> Result<(), SecretError> {
        validate_name(name)?;
        let _transaction = self.lock()?;
        let mut map = self.load()?;
        map.insert(name.to_string(), value.to_string());
        self.save(&map)
    }

    fn delete(&self, name: &str) -> Result<bool, SecretError> {
        let _transaction = self.lock()?;
        let mut map = self.load()?;
        let existed = map.remove(name).map(SecretValue::new).is_some();
        if existed {
            self.save(&map)?;
        }
        Ok(existed)
    }

    fn list_names(&self) -> Result<Vec<String>, SecretError> {
        let _transaction = self.lock()?;
        Ok(self.load()?.into_names())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wipe;

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

    #[test]
    fn failure_before_rename_preserves_old_bytes_and_cleans_unique_temp() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path(), "pw");
        store.set("old", "value").unwrap();
        let old_bytes = std::fs::read(store.path()).unwrap();
        store
            .fail_before_rename
            .store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(store.set("new", "must not commit").is_err());
        assert_eq!(std::fs::read(store.path()).unwrap(), old_bytes);
        assert_eq!(store.list_names().unwrap(), ["old"]);
        assert!(std::fs::read_dir(dir.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        }));
    }

    #[test]
    fn cloned_handles_share_transaction_mutex_and_support_turkish_paths() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir.path().join("Masaüstü/şifreler"), "pw");
        let clone = store.clone();
        assert!(Arc::ptr_eq(&store.transaction, &clone.transaction));
        store.set("one", "1").unwrap();
        clone.set("two", "2").unwrap();
        assert_eq!(store.list_names().unwrap(), ["one", "two"]);
    }

    #[cfg(windows)]
    #[test]
    fn windows_commit_replaces_existing_file_and_drop_deletes_only_the_temp() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path(), "pw");
        store.set("old", "1").unwrap();
        let created = std::fs::metadata(store.path()).unwrap().created().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(40));
        let marker = dir.path().join("clock");
        std::fs::write(&marker, b"t").unwrap();
        assert_ne!(
            std::fs::metadata(&marker).unwrap().created().unwrap(),
            created,
            "this filesystem cannot distinguish creation times"
        );
        std::fs::remove_file(&marker).unwrap();
        store.set("new", "2").unwrap();
        assert_eq!(
            std::fs::metadata(store.path()).unwrap().created().unwrap(),
            created,
            "replacing an existing secrets file must keep its creation time"
        );
        assert_eq!(store.get("new").unwrap().unwrap().expose(), "2");
        assert!(std::fs::read_dir(dir.path()).unwrap().all(|entry| {
            let name = entry.unwrap().file_name();
            let name = name.to_string_lossy();
            !name.ends_with(".tmp") && !name.contains(".bak")
        }));

        let sibling = dir.path().join("keep-me");
        std::fs::write(&sibling, b"untouched").unwrap();
        let pending = PendingWrite::new(store.path()).unwrap();
        let temp = pending.path.clone();
        assert!(temp.exists());
        assert_ne!(temp, store.path());
        drop(pending);
        assert!(!temp.exists());
        assert_eq!(std::fs::read(&sibling).unwrap(), b"untouched");
        assert_eq!(store.get("old").unwrap().unwrap().expose(), "1");
    }

    #[test]
    fn encryption_decryption_and_drop_zeroize_key_plaintext_and_secrets() {
        let _ = wipe::take_wipes();
        let dir = tempfile::tempdir().unwrap();
        let pass = "correct horse battery";
        let secret = "super-secret-value";
        let store = store(dir.path(), pass);
        store.set("name", secret).unwrap();
        let after_set = wipe::take_wipes();
        assert!(
            after_set.contains(&wipe::WipeKind::Key),
            "derived key was not wiped after encryption: {after_set:?}"
        );
        assert!(
            after_set.contains(&wipe::WipeKind::Plaintext),
            "plaintext JSON was not wiped after encryption: {after_set:?}"
        );
        assert!(
            after_set.contains(&wipe::WipeKind::Secret(secret.len())),
            "secret string was not wiped after encryption: {after_set:?}"
        );
        assert!(!after_set.contains(&wipe::WipeKind::Secret(pass.len())));
        let raw = std::fs::read_to_string(store.path()).unwrap();
        assert!(!raw.contains(secret));
        assert!(!raw.contains(pass));

        let value = store.get("name").unwrap().unwrap();
        let after_get = wipe::take_wipes();
        assert!(after_get.contains(&wipe::WipeKind::Key));
        assert!(after_get.contains(&wipe::WipeKind::Plaintext));
        assert_eq!(value.expose(), secret);
        assert!(!after_get.contains(&wipe::WipeKind::Secret(secret.len())));
        drop(value);
        assert!(wipe::take_wipes().contains(&wipe::WipeKind::Secret(secret.len())));
        drop(store);
        assert!(wipe::take_wipes().contains(&wipe::WipeKind::Secret(pass.len())));
    }

    #[cfg(unix)]
    #[test]
    fn pending_and_committed_files_are_private() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path(), "pw");
        let pending = PendingWrite::new(store.path()).unwrap();
        assert_eq!(
            std::fs::metadata(&pending.path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        drop(pending);
        store.set("name", "value").unwrap();
        assert_eq!(
            std::fs::metadata(store.path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(dir.path().join("secrets.enc.lock"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}
