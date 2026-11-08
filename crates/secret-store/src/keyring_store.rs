//! OS keychain backend (Windows Credential Manager, macOS Keychain, Secret
//! Service). The keychain APIs cannot enumerate entries, so the names are kept
//! in a reserved index entry (`__index__`) that holds a JSON array. A second
//! reserved entry (`__index__.bak`) records candidate names *before* a new value
//! is written and retains a deleted name until its value is gone. Recovery
//! probes both lists under the transaction lock; it never invents a listed
//! value and never forgets a value written by an interrupted transaction.
//!
//! The index remains authoritative: OS keyrings cannot enumerate arbitrary
//! entries. Values orphaned by older writers, external applications, or loss of
//! both index entries cannot be rediscovered without knowing their names.

use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use crate::{
    error::SecretError,
    secret::{SecretStore, SecretValue, validate_name},
    transaction::{self, TransactionGuard},
};

/// Reserved entry holding the JSON list of stored names.
const INDEX_ENTRY: &str = "__index__";
const INDEX_BACKUP_ENTRY: &str = "__index__.bak";

/// [`SecretStore`] backed by the OS keychain under a service name.
#[derive(Clone)]
pub struct KeyringStore {
    service: String,
    lock_path: Option<PathBuf>,
    transaction: Arc<Mutex<()>>,
    #[cfg(all(test, any(windows, target_os = "macos", target_os = "linux")))]
    mock: Option<Arc<tests::MockEntries>>,
}

impl std::fmt::Debug for KeyringStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyringStore")
            .field("service", &self.service)
            .field("lock_path", &self.lock_path)
            .finish_non_exhaustive()
    }
}

impl KeyringStore {
    /// A store using `service` as the keychain service name (e.g. `praxi-bot`).
    #[must_use]
    pub fn new(service: impl Into<String>) -> Self {
        let service = service.into();
        // A keyring service is shared even when callers use different data
        // directories. Use one service-wide lock in the OS app config directory.
        // FNV-1a is a stable filename encoding, not a security primitive;
        // collisions only serialize otherwise independent services.
        let hash = service
            .as_bytes()
            .iter()
            .fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
                (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3)
            });
        let lock_path = directories::ProjectDirs::from("dev", "praxi", "praxi-bot")
            .map(|dirs| dirs.config_dir().join(format!("keyring-{hash:016x}.lock")));
        Self {
            service,
            lock_path,
            transaction: Arc::new(Mutex::new(())),
            #[cfg(all(test, any(windows, target_os = "macos", target_os = "linux")))]
            mock: None,
        }
    }

    fn lock(&self) -> Result<TransactionGuard<'_>, SecretError> {
        let path = self.lock_path.as_ref().ok_or_else(|| {
            SecretError::Unavailable("cannot locate the keyring lock directory".into())
        })?;
        transaction::acquire(&self.transaction, path)
    }

    /// Check that the keychain works by writing, reading and deleting a probe.
    ///
    /// # Errors
    /// [`SecretError::Unavailable`] if any step fails.
    pub fn probe(&self) -> Result<(), SecretError> {
        let name = "__probe__";
        let unavailable = |e: SecretError| SecretError::Unavailable(e.to_string());
        let _transaction = self.lock().map_err(unavailable)?;
        self.raw_set(name, "probe").map_err(unavailable)?;
        let got = self.raw_get(name);
        let cleanup = self.raw_delete(name);
        let got = got.map_err(unavailable)?;
        cleanup.map_err(unavailable)?;
        if got.as_deref() == Some("probe") {
            Ok(())
        } else {
            Err(SecretError::Unavailable(
                "probe value did not round-trip".into(),
            ))
        }
    }

    fn read_index(&self, entry: &str) -> Result<Option<Vec<String>>, SecretError> {
        let Some(json) = self.raw_get(entry)? else {
            return Ok(None);
        };
        let names: Vec<String> = serde_json::from_str(&json)
            .map_err(|e| SecretError::Corrupt(format!("keychain index {entry}: {e}")))?;
        for name in &names {
            validate_name(name)
                .map_err(|_| SecretError::Corrupt(format!("invalid name in {entry}")))?;
        }
        Ok(Some(names))
    }

    fn write_index(&self, entry: &str, names: &[String]) -> Result<(), SecretError> {
        let json = serde_json::to_string(names).map_err(|e| SecretError::Corrupt(e.to_string()))?;
        self.raw_set(entry, &json)
    }

    /// Reconcile the authoritative index and its recovery copy, returning the
    /// sorted names whose values exist. Called lazily by [`Self::list_names`].
    ///
    /// Recovery can restore a value whose set/delete was interrupted before
    /// acknowledgement. It cannot enumerate values absent from both indexes.
    ///
    /// # Errors
    /// Lock, keychain, or index errors. A failed probe is never treated as a
    /// missing value, so a temporarily unavailable keychain cannot erase names.
    pub fn repair_index(&self) -> Result<Vec<String>, SecretError> {
        let _transaction = self.lock()?;
        self.repair_index_locked()
    }

    fn repair_index_locked(&self) -> Result<Vec<String>, SecretError> {
        let primary = self.read_index(INDEX_ENTRY);
        let backup = self.read_index(INDEX_BACKUP_ENTRY)?;
        let primary = match primary {
            Ok(names) => names,
            // The recovery copy also covers partial failure of the primary.
            Err(SecretError::Corrupt(_)) if backup.is_some() => None,
            Err(error) => return Err(error),
        };
        let candidates: BTreeSet<_> = primary
            .iter()
            .chain(backup.iter())
            .flatten()
            .cloned()
            .collect();
        let mut present = Vec::with_capacity(candidates.len());
        for name in candidates {
            if self.raw_get(&name)?.is_some() {
                present.push(name);
            }
        }
        if primary.is_none() && backup.is_none() {
            return Ok(present);
        }
        // Keep recovery information durable before changing the primary.
        if backup.as_ref() != Some(&present) {
            self.write_index(INDEX_BACKUP_ENTRY, &present)?;
        }
        if primary.as_ref() != Some(&present) {
            self.write_index(INDEX_ENTRY, &present)?;
        }
        Ok(present)
    }
}

impl SecretStore for KeyringStore {
    fn get(&self, name: &str) -> Result<Option<SecretValue>, SecretError> {
        validate_name(name)?;
        let _transaction = self.lock()?;
        Ok(self.raw_get(name)?.map(SecretValue::new))
    }

    fn set(&self, name: &str, value: &str) -> Result<(), SecretError> {
        validate_name(name)?;
        let _transaction = self.lock()?;
        let mut idx = self.repair_index_locked()?;
        if !idx.iter().any(|n| n == name) {
            idx.push(name.to_string());
            idx.sort();
        }
        // Write-ahead names make a crash after the value write recoverable;
        // only the primary index advertises committed values.
        self.write_index(INDEX_BACKUP_ENTRY, &idx)?;
        self.raw_set(name, value)?;
        self.write_index(INDEX_ENTRY, &idx)
    }

    fn delete(&self, name: &str) -> Result<bool, SecretError> {
        validate_name(name)?;
        let _transaction = self.lock()?;
        let mut idx = self.repair_index_locked()?;
        if self.raw_get(name)?.is_none() {
            return Ok(false);
        }
        // Also protect a known name left unindexed by an older writer. If the
        // deletion fails, repair must still be able to find its existing value.
        if !idx.iter().any(|n| n == name) {
            idx.push(name.to_string());
            idx.sort();
        }
        self.write_index(INDEX_BACKUP_ENTRY, &idx)?;
        idx.retain(|n| n != name);
        self.write_index(INDEX_ENTRY, &idx)?;
        let existed = self.raw_delete(name)?;
        self.write_index(INDEX_BACKUP_ENTRY, &idx)?;
        Ok(existed)
    }

    fn list_names(&self) -> Result<Vec<String>, SecretError> {
        self.repair_index()
    }
}

#[cfg(any(windows, target_os = "macos", target_os = "linux"))]
impl KeyringStore {
    fn entry(&self, name: &str) -> Result<Arc<keyring::Entry>, SecretError> {
        #[cfg(test)]
        if let Some(mock) = &self.mock {
            return Ok(mock.entry(name));
        }
        keyring::Entry::new(&self.service, name)
            .map(Arc::new)
            .map_err(|e| SecretError::Keyring(e.to_string()))
    }

    fn raw_get(&self, name: &str) -> Result<Option<String>, SecretError> {
        match self.entry(name)?.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(SecretError::Keyring(e.to_string())),
        }
    }

    fn raw_set(&self, name: &str, value: &str) -> Result<(), SecretError> {
        #[cfg(test)]
        if let Some(mock) = &self.mock {
            mock.check_failure(name, false)?;
        }
        self.entry(name)?
            .set_password(value)
            .map_err(|e| SecretError::Keyring(e.to_string()))
    }

    fn raw_delete(&self, name: &str) -> Result<bool, SecretError> {
        #[cfg(test)]
        if let Some(mock) = &self.mock {
            mock.check_failure(name, true)?;
        }
        match self.entry(name)?.delete_credential() {
            Ok(()) => Ok(true),
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(e) => Err(SecretError::Keyring(e.to_string())),
        }
    }
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
impl KeyringStore {
    fn unsupported(&self) -> SecretError {
        SecretError::Unavailable(format!(
            "no OS keychain support on this platform (service {})",
            self.service
        ))
    }

    fn raw_get(&self, _name: &str) -> Result<Option<String>, SecretError> {
        Err(self.unsupported())
    }

    fn raw_set(&self, _name: &str, _value: &str) -> Result<(), SecretError> {
        Err(self.unsupported())
    }

    fn raw_delete(&self, _name: &str) -> Result<bool, SecretError> {
        Err(self.unsupported())
    }
}

#[cfg(all(test, any(windows, target_os = "macos", target_os = "linux")))]
mod tests {
    use std::{collections::BTreeMap, path::Path};

    use super::*;

    /// Isolated keyring-rs mock credentials, without installing a process-global
    /// default builder or ever touching the real OS keychain.
    #[derive(Default)]
    pub(super) struct MockEntries {
        entries: Mutex<BTreeMap<String, Arc<keyring::Entry>>>,
        failure: Mutex<Option<Failure>>,
    }

    struct Failure {
        name: String,
        delete: bool,
        skip: usize,
    }

    impl MockEntries {
        pub(super) fn entry(&self, name: &str) -> Arc<keyring::Entry> {
            Arc::clone(
                self.entries
                    .lock()
                    .unwrap()
                    .entry(name.into())
                    .or_insert_with(|| {
                        Arc::new(keyring::Entry::new_with_credential(Box::<
                            keyring::mock::MockCredential,
                        >::default(
                        )))
                    }),
            )
        }

        fn fail(&self, name: &str, delete: bool, skip: usize) {
            *self.failure.lock().unwrap() = Some(Failure {
                name: name.into(),
                delete,
                skip,
            });
        }

        pub(super) fn check_failure(&self, name: &str, delete: bool) -> Result<(), SecretError> {
            let mut pending = self.failure.lock().unwrap();
            if let Some(failure) = pending.as_mut()
                && failure.name == name
                && failure.delete == delete
            {
                if failure.skip > 0 {
                    failure.skip -= 1;
                } else {
                    *pending = None;
                    return Err(SecretError::Keyring("injected write failure".into()));
                }
            }
            Ok(())
        }
    }

    fn mock_store(dir: &Path) -> KeyringStore {
        KeyringStore {
            service: "test".into(),
            lock_path: Some(dir.join("keyring.lock")),
            transaction: Arc::new(Mutex::new(())),
            mock: Some(Arc::default()),
        }
    }

    fn reopened(store: &KeyringStore) -> KeyringStore {
        KeyringStore {
            transaction: Arc::new(Mutex::new(())),
            ..store.clone()
        }
    }

    #[test]
    fn interrupted_set_after_value_write_is_repaired_on_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let store = mock_store(dir.path());
        store.set("before", "one").unwrap();
        store.mock.as_ref().unwrap().fail(INDEX_ENTRY, false, 0);
        assert!(store.set("after", "two").is_err());
        assert_eq!(store.read_index(INDEX_ENTRY).unwrap().unwrap(), ["before"]);
        assert_eq!(store.raw_get("after").unwrap().as_deref(), Some("two"));

        let reopened = reopened(&store);
        assert_eq!(reopened.list_names().unwrap(), ["after", "before"]);
        assert_eq!(reopened.get("after").unwrap().unwrap().expose(), "two");
        assert_eq!(
            reopened.read_index(INDEX_ENTRY).unwrap(),
            reopened.read_index(INDEX_BACKUP_ENTRY).unwrap()
        );
    }

    #[test]
    fn interrupted_set_before_value_write_does_not_list_a_missing_secret() {
        let dir = tempfile::tempdir().unwrap();
        let store = mock_store(dir.path());
        store.mock.as_ref().unwrap().fail("new", false, 0);
        assert!(store.set("new", "value").is_err());
        assert_eq!(
            store.read_index(INDEX_BACKUP_ENTRY).unwrap().unwrap(),
            ["new"]
        );
        assert_eq!(reopened(&store).list_names().unwrap(), Vec::<String>::new());
        assert!(store.get("new").unwrap().is_none());
    }

    #[test]
    fn interrupted_delete_keeps_the_existing_value_discoverable() {
        let dir = tempfile::tempdir().unwrap();
        let store = mock_store(dir.path());
        store.set("name", "value").unwrap();
        store.mock.as_ref().unwrap().fail("name", true, 0);
        assert!(store.delete("name").is_err());
        assert_eq!(
            store.read_index(INDEX_ENTRY).unwrap().unwrap(),
            Vec::<String>::new()
        );
        assert_eq!(store.raw_get("name").unwrap().as_deref(), Some("value"));
        let reopened = reopened(&store);
        assert_eq!(reopened.list_names().unwrap(), ["name"]);
        assert!(reopened.delete("name").unwrap());
        assert_eq!(reopened.list_names().unwrap(), Vec::<String>::new());
    }

    #[test]
    fn interrupted_delete_after_value_removal_does_not_resurrect_the_name() {
        let dir = tempfile::tempdir().unwrap();
        let store = mock_store(dir.path());
        store.set("name", "value").unwrap();
        store
            .mock
            .as_ref()
            .unwrap()
            .fail(INDEX_BACKUP_ENTRY, false, 1);
        assert!(store.delete("name").is_err());
        assert!(store.raw_get("name").unwrap().is_none());
        assert_eq!(
            store.read_index(INDEX_BACKUP_ENTRY).unwrap().unwrap(),
            ["name"]
        );
        assert_eq!(reopened(&store).list_names().unwrap(), Vec::<String>::new());
        assert_eq!(
            store.read_index(INDEX_BACKUP_ENTRY).unwrap().unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn repair_migrates_legacy_index_and_recovers_missing_or_corrupt_primary() {
        let dir = tempfile::tempdir().unwrap();
        let store = mock_store(dir.path());
        store.raw_set("legacy", "value").unwrap();
        store.raw_set(INDEX_ENTRY, r#"["legacy"]"#).unwrap();
        assert!(store.raw_get(INDEX_BACKUP_ENTRY).unwrap().is_none());
        assert_eq!(store.list_names().unwrap(), ["legacy"]);
        assert_eq!(
            store.read_index(INDEX_BACKUP_ENTRY).unwrap().unwrap(),
            ["legacy"]
        );
        store.raw_delete(INDEX_ENTRY).unwrap();
        assert_eq!(store.repair_index().unwrap(), ["legacy"]);
        store.raw_set(INDEX_ENTRY, "invalid json").unwrap();
        assert_eq!(store.repair_index().unwrap(), ["legacy"]);
    }

    #[test]
    fn keychain_read_failure_does_not_remove_names_from_either_index() {
        let dir = tempfile::tempdir().unwrap();
        let store = mock_store(dir.path());
        store.set("name", "value").unwrap();
        store
            .entry("name")
            .unwrap()
            .get_credential()
            .downcast_ref::<keyring::mock::MockCredential>()
            .unwrap()
            .set_error(keyring::Error::Invalid(
                "test".into(),
                "read failure".into(),
            ));
        assert!(store.list_names().is_err());
        assert_eq!(store.read_index(INDEX_ENTRY).unwrap().unwrap(), ["name"]);
        assert_eq!(
            store.read_index(INDEX_BACKUP_ENTRY).unwrap().unwrap(),
            ["name"]
        );
    }

    #[test]
    fn concurrent_keyring_handles_keep_names_and_values_consistent() {
        let dir = tempfile::tempdir().unwrap();
        let store = mock_store(dir.path());
        let start = std::sync::Barrier::new(16);
        std::thread::scope(|threads| {
            for thread in 0..16 {
                let handle = reopened(&store);
                let start = &start;
                threads.spawn(move || {
                    start.wait();
                    for i in 0..20 {
                        let name = format!("thread-{thread}-{i}");
                        handle.set(&name, &name).unwrap();
                        if i % 2 == 0 {
                            assert!(handle.delete(&name).unwrap());
                        }
                    }
                });
            }
        });
        assert_eq!(store.list_names().unwrap().len(), 160);
        for thread in 0..16 {
            for i in 0..20 {
                let name = format!("thread-{thread}-{i}");
                let value = store.get(&name).unwrap();
                assert_eq!(value.is_some(), i % 2 == 1);
                if let Some(value) = value {
                    assert_eq!(value.expose(), name);
                }
            }
        }
        assert_eq!(
            store.read_index(INDEX_ENTRY).unwrap(),
            store.read_index(INDEX_BACKUP_ENTRY).unwrap()
        );
    }

    #[test]
    fn clone_shares_mutex_and_service_lock_is_stable() {
        let original = KeyringStore::new("test service/with separators");
        let clone = original.clone();
        let independent = KeyringStore::new("test service/with separators");
        assert!(Arc::ptr_eq(&original.transaction, &clone.transaction));
        assert_eq!(original.lock_path, independent.lock_path);
    }

    /// Touches the real OS keychain under a unique service name and cleans up.
    #[test]
    #[ignore = "uses the real OS keychain"]
    fn real_keychain_round_trip() {
        let service = format!("praxi-bot-test-{}", std::process::id());
        let s = KeyringStore::new(service);
        s.probe().unwrap();
        s.set("t.one", "v1").unwrap();
        assert_eq!(s.get("t.one").unwrap().unwrap().expose(), "v1");
        assert_eq!(s.list_names().unwrap(), ["t.one"]);
        assert!(s.delete("t.one").unwrap());
        assert_eq!(s.list_names().unwrap(), Vec::<String>::new());
        let _ = s.raw_delete(INDEX_ENTRY);
        let _ = s.raw_delete(INDEX_BACKUP_ENTRY);
    }
}
