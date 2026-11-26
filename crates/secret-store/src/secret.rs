//! Core types: [`SecretStore`], [`SecretRef`], [`SecretValue`].

use std::fmt;

use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::error::SecretError;
use crate::wipe::wipe_string;

/// A secret value. `Debug` never prints the content.
///
/// The bytes are wiped when the value is dropped.
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

impl Zeroize for SecretValue {
    fn zeroize(&mut self) {
        wipe_string(&mut self.0);
    }
}

impl Drop for SecretValue {
    fn drop(&mut self) {
        self.zeroize();
    }
}

impl ZeroizeOnDrop for SecretValue {}

/// Whether `name` is an OAuth record the MCP bridge manages itself
/// (`mcp/<server>/oauth`, `mcp/<server>/oauth-client`, ...): access and
/// refresh tokens and issued client secrets.  Such records are read and
/// written only by the bridge's credential store; no configuration field may
/// reference them ([`SecretRef::new`] and [`resolve`] refuse), so they can
/// never be sent as a header, put into a child's environment or used as a
/// provider key.  Compared case-insensitively (some keychains fold case).
#[must_use]
pub fn is_managed_oauth_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let mut parts = lower.split('/');
    if parts.next() != Some("mcp") {
        return false;
    }
    // Exactly `mcp/<server>/oauth` or `mcp/<server>/oauth-<part>`: names
    // below a server (`mcp/<server>/env/OAUTH_TOKEN`) stay ordinary secrets.
    let rest: Vec<&str> = parts.collect();
    matches!(rest.as_slice(), [server, last]
        if !server.is_empty() && (*last == "oauth" || last.starts_with("oauth-")))
}

/// A reference to a secret by name. Safe to persist and to show to the agent.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SecretRef(String);

impl TryFrom<String> for SecretRef {
    type Error = SecretError;
    fn try_from(name: String) -> Result<Self, Self::Error> {
        // Deserialization refuses only native-integration names; managed MCP
        // OAuth names are refused again in `resolve`, as before.
        validate_name(&name)?;
        if is_reserved_name(&name) {
            return Err(SecretError::InvalidName(name));
        }
        if is_host_credential(&name) {
            return Err(SecretError::InvalidName("reserved host credential".into()));
        }
        Ok(Self(name))
    }
}

impl From<SecretRef> for String {
    fn from(r: SecretRef) -> Self {
        r.0
    }
}

impl SecretRef {
    /// Create a reference after validating the name.
    ///
    /// # Errors
    /// [`SecretError::InvalidName`] for empty or malformed names, and for
    /// managed OAuth records ([`is_managed_oauth_name`]).
    pub fn new(name: impl Into<String>) -> Result<Self, SecretError> {
        let name = name.into();
        validate_name(&name)?;
        refuse_fill_namespace(&name)?;
        if is_reserved_name(&name) || is_managed_oauth_name(&name) {
            return Err(SecretError::InvalidName(name));
        }
        if is_host_credential(&name) {
            return Err(SecretError::InvalidName("reserved host credential".into()));
        }
        Ok(Self(name))
    }

    /// The secret name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.0
    }
}

/// Host credentials cannot be selected as provider/connector secrets or
/// exported in a portable state bundle. The host accesses them by exact name.
#[must_use]
pub fn is_host_credential(name: &str) -> bool {
    // Windows credential targets are case-insensitive. Reserve every ASCII
    // case spelling on all backends so aliases cannot cross this boundary.
    name.get(.."mcp-server/".len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("mcp-server/"))
}

impl fmt::Display for SecretRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Prefix of secrets owned by a native integration (the Composio API key).
/// No configuration, tool, bot, peer or state export may reference or read
/// them: [`SecretRef::new`] refuses these names, the secrets route refuses to
/// write them and state bundles skip them.  Only the owning crate talks to the
/// store with the literal name.
pub const RESERVED_PREFIX: &str = "composio/";

/// Whether `name` is reserved for a native integration (case-insensitive, and
/// not fooled by leading slashes).
#[must_use]
pub fn is_reserved_name(name: &str) -> bool {
    let n = name.trim_start_matches('/').to_ascii_lowercase();
    n.starts_with(RESERVED_PREFIX)
        || n == "composio"
        || n.starts_with(WEBHOOK_PREFIX)
        || n == "webhook"
}

/// Store prefix of the HMAC secrets of incoming webhooks (`webhook/<id>`). Like
/// the Composio key, only the owning route talks to the store with the literal
/// name; no configuration, bot, peer or state export can reference or read it.
pub const WEBHOOK_PREFIX: &str = "webhook/";

/// Store prefix of the owner-only fill secrets (see [`crate::FillSecrets`]).
pub const FILL_PREFIX: &str = "fill/";

/// Whether `name` is in the fill namespace (ASCII case-insensitive, so a
/// case-folding backend cannot be used to step around the prefix).
#[must_use]
pub fn is_fill_name(name: &str) -> bool {
    name.get(..FILL_PREFIX.len())
        .is_some_and(|p| p.eq_ignore_ascii_case(FILL_PREFIX))
}

/// Configuration (providers, MCP, `OpenAPI`, e-mail, channels) can never name a
/// fill secret: only the fill tools, through [`crate::FillSecrets`], read them.
///
/// # Errors
/// [`SecretError::Reserved`] for a `fill/` name.
pub fn refuse_fill_namespace(name: &str) -> Result<(), SecretError> {
    if is_fill_name(name) {
        Err(SecretError::Reserved(name.to_string()))
    } else {
        Ok(())
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
/// A `SecretRef` can also come from deserialisation (which skips
/// [`SecretRef::new`]), so the fill namespace is refused here as well.
///
/// # Errors
/// [`SecretError::NotFound`] if the secret does not exist,
/// [`SecretError::Reserved`] for a fill secret.
pub fn resolve(store: &dyn SecretStore, r: &SecretRef) -> Result<SecretValue, SecretError> {
    refuse_fill_namespace(r.name())?;
    // A deserialized reference skipped `SecretRef::new`: check again here,
    // the one place every configured reference is resolved.
    if is_managed_oauth_name(r.name()) || is_reserved_name(r.name()) {
        return Err(SecretError::InvalidName(r.name().to_string()));
    }
    store
        .get(r.name())?
        .ok_or_else(|| SecretError::NotFound(r.name().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Portable Windows-like credential lookup, including case-variant enumeration.
    // No OS keychain is accessed by this fixture.
    #[derive(Default)]
    struct CaseInsensitiveStore(crate::MemoryStore);

    impl crate::SecretStore for CaseInsensitiveStore {
        fn get(
            &self,
            name: &str,
        ) -> std::result::Result<Option<crate::SecretValue>, crate::SecretError> {
            crate::SecretStore::get(&self.0, &name.to_ascii_lowercase())
        }
        fn set(&self, name: &str, value: &str) -> std::result::Result<(), crate::SecretError> {
            crate::SecretStore::set(&self.0, &name.to_ascii_lowercase(), value)
        }
        fn delete(&self, name: &str) -> std::result::Result<bool, crate::SecretError> {
            crate::SecretStore::delete(&self.0, &name.to_ascii_lowercase())
        }
        fn list_names(&self) -> std::result::Result<Vec<String>, crate::SecretError> {
            crate::SecretStore::list_names(&self.0).map(|names| {
                names
                    .into_iter()
                    .map(|name| name.to_ascii_uppercase())
                    .collect()
            })
        }
    }

    #[test]
    fn host_credentials_cannot_become_provider_or_connector_references() {
        let store = CaseInsensitiveStore::default();
        store.set("mcp-server/token/id", "host value").unwrap();
        store.set("mcp-server/meta/id", "host value").unwrap();
        for name in [
            "mcp-server/token/id",
            "mcp-server/meta/id",
            "MCP-SERVER/TOKEN/ID",
            "MCP-SERVER/META/ID",
            "McP-SeRvEr/ToKeN/Id",
            "McP-SeRvEr/MeTa/Id",
        ] {
            assert_eq!(store.get(name).unwrap().unwrap().expose(), "host value");
            assert!(SecretRef::new(name).is_err());
            assert!(serde_json::from_value::<SecretRef>(serde_json::json!(name)).is_err());
        }
        let ordinary: SecretRef = serde_json::from_str("\"provider/key\"").unwrap();
        assert_eq!(ordinary.name(), "provider/key");
        store.set("provider/key", "ordinary value").unwrap();
        assert_eq!(
            resolve(&store, &ordinary).unwrap().expose(),
            "ordinary value"
        );
    }
}

#[cfg(test)]
mod reserved_tests {
    use super::*;

    #[test]
    fn composio_names_cannot_become_references() {
        for n in [
            "composio/api-key",
            "Composio/API-KEY",
            "/composio/x",
            "composio",
        ] {
            assert!(is_reserved_name(n), "{n}");
            assert!(SecretRef::new(n).is_err(), "{n}");
            assert!(
                serde_json::from_str::<SecretRef>(&format!("\"{n}\"")).is_err(),
                "{n}"
            );
        }
        assert!(SecretRef::new("mcp/github-token").is_ok());
        assert!(serde_json::from_str::<SecretRef>("\"mcp/x\"").is_ok());
        assert!(!is_reserved_name("composiox/a"));
        for n in ["webhook/0190", "Webhook/X", "/webhook/x", "webhook"] {
            assert!(is_reserved_name(n), "{n}");
            assert!(SecretRef::new(n).is_err(), "{n}");
        }
        assert!(!is_reserved_name("webhooks/a"));
    }
}

#[cfg(test)]
mod managed_oauth_tests {
    use super::*;

    #[test]
    fn managed_oauth_records_can_never_be_referenced_or_resolved() {
        let store = crate::MemoryStore::new();
        store
            .set("mcp/victim/oauth", "{\"access_token\":\"t\"}")
            .unwrap();
        store.set("mcp/victim/oauth-client", "s").unwrap();
        store.set("mcp/victim/key", "plain").unwrap();
        for name in [
            "mcp/victim/oauth",
            "MCP/Victim/OAuth",
            "mcp/victim/oauth-client",
        ] {
            assert!(is_managed_oauth_name(name), "{name}");
            assert!(SecretRef::new(name).is_err(), "{name}");
            // A reference that came in through serde skips `new`.
            let smuggled: SecretRef = serde_json::from_value(serde_json::json!(name)).unwrap();
            assert!(resolve(&store, &smuggled).is_err(), "{name}");
        }
        for name in [
            "mcp/victim/key",
            "mcp_victim_oauth",
            "oauth",
            "mcp/oauth",
            "github/oauth",
            "mcp/victim/env/OAUTH_TOKEN",
            "mcp/victim/header/oauth",
            "mcp/victim/oauthish",
        ] {
            assert!(!is_managed_oauth_name(name), "{name}");
        }
        assert_eq!(
            resolve(&store, &SecretRef::new("mcp/victim/key").unwrap())
                .unwrap()
                .expose(),
            "plain"
        );
    }

    #[test]
    fn is_fill_name_checks_case_insensitive_prefix_and_boundaries() {
        let cases = [
            ("", false),
            ("fill", false),
            ("fill/", true),
            ("fill/github", true),
            ("FILL/GITHUB", true),
            ("FiLl/a/b", true),
            ("/fill/a", false),
            ("fill_other/a", false),
            ("ééééé", false),
            ("🔒🔑🗝️", false),
        ];
        for (name, expected) in cases {
            assert_eq!(is_fill_name(name), expected, "name={name:?}");
            assert_eq!(
                refuse_fill_namespace(name).is_err(),
                expected,
                "name={name:?}"
            );
        }
    }

    #[test]
    fn is_host_credential_checks_case_insensitive_prefix_and_utf8_boundaries() {
        let cases = [
            ("", false),
            ("mcp-server", false),
            ("mcp-server/", true),
            ("mcp-server/token/1", true),
            ("MCP-SERVER/TOKEN", true),
            ("McP-SeRvEr/x", true),
            ("/mcp-server/x", false),
            ("mcp-servers/x", false),
            ("éééééé", false),
            ("🔑🔑🔑🔑", false),
        ];
        for (name, expected) in cases {
            assert_eq!(is_host_credential(name), expected, "name={name:?}");
        }
    }
}
