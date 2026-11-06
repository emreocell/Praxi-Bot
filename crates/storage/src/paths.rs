//! Data directory resolution.

use std::ffi::OsString;
use std::path::PathBuf;

/// Environment variable overriding the data directory.
pub const DATA_DIR_ENV: &str = "BOT_DATA_DIR";

/// Resolve the data directory: `BOT_DATA_DIR` if set and non-empty, otherwise
/// the OS-specific application data directory.  Returns `None` only if the OS
/// provides no home directory.
#[must_use]
pub fn data_dir() -> Option<PathBuf> {
    resolve_data_dir(std::env::var_os(DATA_DIR_ENV))
}

/// Pure resolver behind [`data_dir`] (testable without touching the process env).
#[must_use]
pub fn resolve_data_dir(override_dir: Option<OsString>) -> Option<PathBuf> {
    match override_dir {
        Some(v) if !v.is_empty() => Some(PathBuf::from(v)),
        _ => directories::ProjectDirs::from("dev", "praxi", "praxi-bot")
            .map(|d| d.data_dir().to_path_buf()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_wins() {
        assert_eq!(
            resolve_data_dir(Some("/tmp/x".into())),
            Some(PathBuf::from("/tmp/x"))
        );
    }

    #[test]
    fn empty_override_falls_back_to_os_default() {
        let d = resolve_data_dir(Some(OsString::new()));
        assert_eq!(d, resolve_data_dir(None));
    }
}
