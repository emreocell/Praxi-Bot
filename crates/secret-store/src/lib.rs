//! Secret storage for Praxi-Bot.
//!
//! - [`SecretStore`]: `get` / `set` / `delete` / `list_names`.
//! - Backends: [`KeyringStore`] (OS keychain), [`EncryptedFileStore`]
//!   (`ChaCha20-Poly1305` + Argon2id, for headless servers) and [`MemoryStore`]
//!   (in-process fake for tests).
//! - [`SecretRef`]: a *name only* handle. Config files and the agent loop hold
//!   refs; [`resolve`] turns one into a [`SecretValue`] and must only be called
//!   at the provider / MCP boundary (ADR-0012).
//! - [`detect_secrets`]: precision-first detection of a pasted credential in a
//!   chat message (see `docs/design/secret-detection.md`).
//! - [`scan_text`]: gitleaks-derived rule scan (keyword prefilter, lazy regexes)
//!   returning [`Finding`]s (rule id and span, never the value), for warnings
//!   before a secret is sent and for leak tests.
//! - [`Redactor`]: masks known secret values and common key shapes in any
//!   string (tool output, logs, events).

mod detect;
mod error;
mod file;
mod fill;
mod keyring_store;
mod memory;
mod open;
#[cfg(windows)]
mod publish;
mod redact;
mod rules;
mod scan;
mod secret;
mod transaction;
mod wipe;

pub use detect::{Confidence, SecretCandidate, SecretKind, detect_secrets, placeholder};
pub use error::SecretError;
pub use file::{EncryptedFileStore, KdfParams};
pub use fill::{
    FillError, FillGrant, FillSecretInfo, FillSecrets, GRANT_KIND_BROWSER_FILL,
    MAX_FILL_VALUE_BYTES, ValueHook, canonical_origin, metadata_path, sanitize_label,
    validate_fill_name,
};
pub use keyring_store::KeyringStore;
pub use memory::MemoryStore;
pub use open::{
    BackendChoice, BackendKind, KEYRING_SERVICE, PASSPHRASE_ENV, SECRETS_FILE_NAME, open_store,
    secrets_file_path,
};
pub use redact::{REDACTED, Redactor, secret_variants};
pub use scan::{Finding, scan_text};
pub use secret::{
    FILL_PREFIX, RESERVED_PREFIX, SecretRef, SecretStore, SecretValue, is_fill_name,
    is_host_credential, is_managed_oauth_name, is_reserved_name, refuse_fill_namespace, resolve,
    validate_name,
};
