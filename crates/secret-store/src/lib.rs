//! Secret storage for Praxi-Bot.
//!
//! - [`SecretStore`]: `get` / `set` / `delete` / `list_names`.
//! - Backends: [`KeyringStore`] (OS keychain), [`EncryptedFileStore`]
//!   (`ChaCha20-Poly1305` + Argon2id, for headless servers) and [`MemoryStore`]
//!   (in-process fake for tests).
//! - [`SecretRef`]: a *name only* handle. Config files and the agent loop hold
//!   refs; [`resolve`] turns one into a [`SecretValue`] and must only be called
//!   at the provider / MCP boundary (ADR-0012).
//! - [`Redactor`]: masks known secret values and common key shapes in any
//!   string (tool output, logs, events).

mod error;
mod file;
mod keyring_store;
mod memory;
mod open;
mod redact;
mod secret;

pub use error::SecretError;
pub use file::{EncryptedFileStore, KdfParams};
pub use keyring_store::KeyringStore;
pub use memory::MemoryStore;
pub use open::{
    BackendChoice, BackendKind, KEYRING_SERVICE, PASSPHRASE_ENV, SECRETS_FILE_NAME, open_store,
    secrets_file_path,
};
pub use redact::{REDACTED, Redactor};
pub use secret::{SecretRef, SecretStore, SecretValue, resolve, validate_name};
