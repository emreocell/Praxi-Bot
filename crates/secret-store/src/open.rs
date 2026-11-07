//! Automatic backend selection.

use std::{
    path::{Path, PathBuf},
    str::FromStr,
};

use crate::{
    error::SecretError, file::EncryptedFileStore, keyring_store::KeyringStore, secret::SecretStore,
};

/// Environment variable carrying the passphrase of the encrypted file.
pub const PASSPHRASE_ENV: &str = "BOT_SECRETS_PASSPHRASE";
/// File name of the encrypted store inside the data directory.
pub const SECRETS_FILE_NAME: &str = "secrets.enc";
/// Keychain service name used by the real application.
pub const KEYRING_SERVICE: &str = "praxi-bot";

/// Which backend to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackendChoice {
    /// Existing encrypted file wins, else the OS keychain if it works, else the file.
    #[default]
    Auto,
    /// Force the OS keychain.
    Keyring,
    /// Force the encrypted file.
    File,
}

impl FromStr for BackendChoice {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "keyring" | "keychain" => Ok(Self::Keyring),
            "file" => Ok(Self::File),
            other => Err(format!("unknown backend {other:?} (auto|keyring|file)")),
        }
    }
}

/// The backend that was actually opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    /// OS keychain.
    Keyring,
    /// Encrypted file.
    File,
}

/// Location of the encrypted file for a data directory.
#[must_use]
pub fn secrets_file_path(data_dir: &Path) -> PathBuf {
    data_dir.join(SECRETS_FILE_NAME)
}

/// Open a secret store.
///
/// For the file backend the passphrase comes from `BOT_SECRETS_PASSPHRASE`, or
/// else from `prompt(creating)` (`creating` is true when no file exists yet).
/// An existing file is verified so a wrong passphrase fails immediately.
///
/// # Errors
/// Keychain unavailable when forced, missing / wrong passphrase, I/O errors.
pub fn open_store(
    choice: BackendChoice,
    data_dir: &Path,
    prompt: &dyn Fn(bool) -> Option<String>,
) -> Result<(Box<dyn SecretStore>, BackendKind), SecretError> {
    let path = secrets_file_path(data_dir);
    let use_file = match choice {
        BackendChoice::File => true,
        BackendChoice::Keyring => false,
        BackendChoice::Auto => path.exists() || KeyringStore::new(KEYRING_SERVICE).probe().is_err(),
    };
    if use_file {
        let creating = !path.exists();
        let pass = match std::env::var(PASSPHRASE_ENV) {
            Ok(p) if !p.is_empty() => p,
            _ => prompt(creating)
                .filter(|p| !p.is_empty())
                .ok_or(SecretError::NoPassphrase)?,
        };
        let store = EncryptedFileStore::new(path, pass);
        store.verify()?;
        Ok((Box::new(store), BackendKind::File))
    } else {
        let store = KeyringStore::new(KEYRING_SERVICE);
        if choice == BackendChoice::Keyring {
            store.probe()?;
        }
        Ok((Box::new(store), BackendKind::Keyring))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_choice() {
        assert_eq!(
            "AUTO".parse::<BackendChoice>().unwrap(),
            BackendChoice::Auto
        );
        assert_eq!(
            "file".parse::<BackendChoice>().unwrap(),
            BackendChoice::File
        );
        assert_eq!(
            "keyring".parse::<BackendChoice>().unwrap(),
            BackendChoice::Keyring
        );
        assert!("x".parse::<BackendChoice>().is_err());
    }

    #[test]
    fn forced_file_uses_prompt_and_rejects_missing_passphrase() {
        // Env var is process-global; only exercise the prompt path when unset.
        if std::env::var(PASSPHRASE_ENV).is_ok() {
            return;
        }
        let d = tempfile::tempdir().unwrap();
        let none = open_store(BackendChoice::File, d.path(), &|_| None);
        assert!(matches!(none, Err(SecretError::NoPassphrase)));
        let (s, kind) = open_store(BackendChoice::File, d.path(), &|creating| {
            assert!(creating);
            Some("pw".into())
        })
        .unwrap();
        assert_eq!(kind, BackendKind::File);
        assert_eq!(s.list_names().unwrap(), Vec::<String>::new());
    }
}
