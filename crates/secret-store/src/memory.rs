//! In-process fake backend (tests, dry runs).

use std::{collections::BTreeMap, sync::Mutex};

use crate::{
    error::SecretError,
    secret::{SecretStore, SecretValue, validate_name},
};

/// A [`SecretStore`] kept in memory only.
#[derive(Debug, Default)]
pub struct MemoryStore {
    map: Mutex<BTreeMap<String, String>>,
}

impl MemoryStore {
    /// Empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, String>> {
        self.map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl SecretStore for MemoryStore {
    fn get(&self, name: &str) -> Result<Option<SecretValue>, SecretError> {
        Ok(self.lock().get(name).cloned().map(SecretValue::new))
    }

    fn set(&self, name: &str, value: &str) -> Result<(), SecretError> {
        validate_name(name)?;
        self.lock().insert(name.to_string(), value.to_string());
        Ok(())
    }

    fn delete(&self, name: &str) -> Result<bool, SecretError> {
        Ok(self.lock().remove(name).is_some())
    }

    fn list_names(&self) -> Result<Vec<String>, SecretError> {
        Ok(self.lock().keys().cloned().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::{SecretRef, resolve};

    #[test]
    fn crud_and_resolve() {
        let s = MemoryStore::new();
        s.set("b", "2").unwrap();
        s.set("a", "1").unwrap();
        assert_eq!(s.list_names().unwrap(), ["a", "b"]);
        let r = SecretRef::new("a").unwrap();
        assert_eq!(resolve(&s, &r).unwrap().expose(), "1");
        assert!(s.delete("a").unwrap());
        assert!(!s.delete("a").unwrap());
        assert!(matches!(resolve(&s, &r), Err(SecretError::NotFound(_))));
    }

    #[test]
    fn rejects_bad_names_and_debug_is_redacted() {
        let s = MemoryStore::new();
        assert!(s.set("", "x").is_err());
        assert!(s.set("has space", "x").is_err());
        assert!(s.set("__index__", "x").is_err());
        assert_eq!(
            format!("{:?}", SecretValue::new("hunter2")),
            "SecretValue(<redacted>)"
        );
    }
}
