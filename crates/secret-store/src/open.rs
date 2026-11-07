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

/// How many times the keychain probe is attempted in `Auto` mode before the
/// keychain is considered unavailable.
const AUTO_PROBE_ATTEMPTS: u32 = 3;

/// Probe the keychain up to `attempts` times with a short backoff, so a
/// transient failure (locked keychain, Secret Service still starting) does not
/// count as "unavailable".
fn probe_with_retry(store: &KeyringStore, attempts: u32) -> Result<(), SecretError> {
    let mut last = None;
    for attempt in 0..attempts.max(1) {
        match store.probe() {
            Ok(()) => return Ok(()),
            Err(e) => last = Some(e),
        }
        if attempt + 1 < attempts {
            std::thread::sleep(std::time::Duration::from_millis(
                250 * u64::from(attempt + 1),
            ));
        }
    }
    Err(last.unwrap_or_else(|| SecretError::Unavailable("keychain probe failed".into())))
}

/// Decide the `Auto` backend. An existing encrypted file wins. Otherwise the
/// keychain is used when its probe succeeds. If the probe keeps failing, we do
/// **not** silently create `secrets.enc` (that would make the fallback sticky
/// and orphan keychain secrets): the file is chosen only when the user opted in
/// by providing a passphrase through the environment, else an error is returned.
fn auto_uses_file(
    file_exists: bool,
    passphrase_opted_in: bool,
    probe: &dyn Fn() -> Result<(), SecretError>,
) -> Result<bool, SecretError> {
    if file_exists {
        return Ok(true);
    }
    match probe() {
        Ok(()) => Ok(false),
        Err(_) if passphrase_opted_in => Ok(true),
        Err(e) => Err(SecretError::Unavailable(format!(
            "OS keychain unavailable ({e}); not falling back to an encrypted file 
             automatically. Retry later, or choose the file backend explicitly 
             (backend = \"file\" or set {PASSPHRASE_ENV})"
        ))),
    }
}

/// When the encrypted file is used while the keychain also holds secrets,
/// warn: those keychain entries are not read.
fn warn_if_keychain_has_entries() {
    if let Ok(names) = KeyringStore::new(KEYRING_SERVICE).list_names()
        && !names.is_empty()
    {
        eprintln!(
            "warning: using {SECRETS_FILE_NAME} but the OS keychain also holds {} secret(s) that will not be read",
            names.len()
        );
    }
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
        BackendChoice::Auto => {
            let file_exists = path.exists();
            let opted_in = std::env::var(PASSPHRASE_ENV).is_ok_and(|p| !p.is_empty());
            let use_file = auto_uses_file(file_exists, opted_in, &|| {
                probe_with_retry(&KeyringStore::new(KEYRING_SERVICE), AUTO_PROBE_ATTEMPTS)
            })?;
            if use_file && file_exists {
                warn_if_keychain_has_entries();
            }
            use_file
        }
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
    fn auto_existing_file_wins_without_probing() {
        let r = auto_uses_file(true, false, &|| panic!("must not probe"));
        assert!(r.unwrap());
    }

    #[test]
    fn auto_probe_ok_uses_keyring() {
        assert!(!auto_uses_file(false, false, &|| Ok(())).unwrap());
    }

    #[test]
    fn auto_probe_failure_does_not_silently_pick_file() {
        let fail = || Err(SecretError::Unavailable("locked".into()));
        let r = auto_uses_file(false, false, &fail);
        assert!(matches!(r, Err(SecretError::Unavailable(_))));
    }

    #[test]
    fn auto_probe_failure_with_passphrase_opt_in_uses_file() {
        let fail = || Err(SecretError::Unavailable("locked".into()));
        assert!(auto_uses_file(false, true, &fail).unwrap());
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
