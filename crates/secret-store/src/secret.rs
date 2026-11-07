//! Core types: [`SecretStore`], [`SecretRef`], [`SecretValue`].

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::SecretError;

/// A secret value. `Debug` never prints the content.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretValue(String);

impl SecretValue {
    /// Wrap a value.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Access the raw value. Call only at the provider / MCP boundary.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretValue(<redacted>)")
    }
}

/// A reference to a secret by name. Safe to persist and to show to the agent.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecretRef(String);

impl SecretRef {
    /// Create a reference after validating the name.
    ///
    /// # Errors
    /// [`SecretError::InvalidName`] for empty or malformed names.
    pub fn new(name: impl Into<String>) -> Result<Self, SecretError> {
        let name = name.into();
        validate_name(&name)?;
        Ok(Self(name))
    }

    /// The secret name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SecretRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Validate a secret name.
///
/// # Errors
/// [`SecretError::InvalidName`] if empty, longer than 128 bytes, starting with
/// the reserved `__` prefix, or containing characters outside `[A-Za-z0-9._:/-]`.
pub fn validate_name(name: &str) -> Result<(), SecretError> {
    let ok = !name.is_empty()
        && name.len() <= 128
        && !name.starts_with("__")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':' | '/'));
    if ok {
        Ok(())
    } else {
        Err(SecretError::InvalidName(name.to_string()))
    }
}

/// Key/value secret storage.
pub trait SecretStore: Send + Sync {
    /// Fetch a secret, `None` when absent.
    ///
    /// # Errors
    /// Backend failures.
    fn get(&self, name: &str) -> Result<Option<SecretValue>, SecretError>;

    /// Create or overwrite a secret.
    ///
    /// # Errors
    /// Invalid name or backend failures.
    fn set(&self, name: &str, value: &str) -> Result<(), SecretError>;

    /// Delete a secret; returns whether it existed.
    ///
    /// # Errors
    /// Backend failures.
    fn delete(&self, name: &str) -> Result<bool, SecretError>;

    /// Names of all stored secrets, sorted.
    ///
    /// # Errors
    /// Backend failures.
    fn list_names(&self) -> Result<Vec<String>, SecretError>;
}

/// Resolve a [`SecretRef`] to its value. Call only at the provider / MCP boundary.
///
/// # Errors
/// [`SecretError::NotFound`] if the secret does not exist.
pub fn resolve(store: &dyn SecretStore, r: &SecretRef) -> Result<SecretValue, SecretError> {
    store
        .get(r.name())?
        .ok_or_else(|| SecretError::NotFound(r.name().to_string()))
}
