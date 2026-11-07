//! OS keychain backend (Windows Credential Manager, macOS Keychain, Secret
//! Service). The keychain APIs cannot enumerate entries, so the names are kept
//! in a reserved index entry (`__index__`) that holds a JSON array.

use crate::{
    error::SecretError,
    secret::{SecretStore, SecretValue, validate_name},
};

/// Reserved entry holding the JSON list of stored names.
const INDEX_ENTRY: &str = "__index__";

/// [`SecretStore`] backed by the OS keychain under a service name.
#[derive(Debug, Clone)]
pub struct KeyringStore {
    service: String,
}

impl KeyringStore {
    /// A store using `service` as the keychain service name (e.g. `praxi-bot`).
    #[must_use]
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    /// Check that the keychain works by writing, reading and deleting a probe.
    ///
    /// # Errors
    /// [`SecretError::Unavailable`] if any step fails.
    pub fn probe(&self) -> Result<(), SecretError> {
        let name = "__probe__";
        let unavailable = |e: SecretError| SecretError::Unavailable(e.to_string());
        self.raw_set(name, "probe").map_err(unavailable)?;
        let got = self.raw_get(name).map_err(unavailable)?;
        let _ = self.raw_delete(name);
        if got.as_deref() == Some("probe") {
            Ok(())
        } else {
            Err(SecretError::Unavailable(
                "probe value did not round-trip".into(),
            ))
        }
    }

    fn read_index(&self) -> Result<Vec<String>, SecretError> {
        match self.raw_get(INDEX_ENTRY)? {
            None => Ok(Vec::new()),
            Some(s) => serde_json::from_str(&s)
                .map_err(|e| SecretError::Corrupt(format!("keychain index: {e}"))),
        }
    }

    fn write_index(&self, names: &[String]) -> Result<(), SecretError> {
        let json = serde_json::to_string(names).map_err(|e| SecretError::Corrupt(e.to_string()))?;
        self.raw_set(INDEX_ENTRY, &json)
    }
}

impl SecretStore for KeyringStore {
    fn get(&self, name: &str) -> Result<Option<SecretValue>, SecretError> {
        validate_name(name)?;
        Ok(self.raw_get(name)?.map(SecretValue::new))
    }

    fn set(&self, name: &str, value: &str) -> Result<(), SecretError> {
        validate_name(name)?;
        self.raw_set(name, value)?;
        let mut idx = self.read_index()?;
        if !idx.iter().any(|n| n == name) {
            idx.push(name.to_string());
            idx.sort();
            self.write_index(&idx)?;
        }
        Ok(())
    }

    fn delete(&self, name: &str) -> Result<bool, SecretError> {
        validate_name(name)?;
        let existed = self.raw_delete(name)?;
        let mut idx = self.read_index()?;
        let before = idx.len();
        idx.retain(|n| n != name);
        if idx.len() != before {
            self.write_index(&idx)?;
        }
        Ok(existed)
    }

    fn list_names(&self) -> Result<Vec<String>, SecretError> {
        self.read_index()
    }
}

#[cfg(any(windows, target_os = "macos", target_os = "linux"))]
impl KeyringStore {
    fn entry(&self, name: &str) -> Result<keyring::Entry, SecretError> {
        keyring::Entry::new(&self.service, name).map_err(|e| SecretError::Keyring(e.to_string()))
    }

    fn raw_get(&self, name: &str) -> Result<Option<String>, SecretError> {
        match self.entry(name)?.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(SecretError::Keyring(e.to_string())),
        }
    }

    fn raw_set(&self, name: &str, value: &str) -> Result<(), SecretError> {
        self.entry(name)?
            .set_password(value)
            .map_err(|e| SecretError::Keyring(e.to_string()))
    }

    fn raw_delete(&self, name: &str) -> Result<bool, SecretError> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
    }
}
