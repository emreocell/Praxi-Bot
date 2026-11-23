//! Standalone experiment. No daemon tool calls this crate. See the design note
//! for the Windows runtime/profile exceptions to workspace confinement.

use std::{io, path::Path};

use serde::{Deserialize, Serialize};

#[cfg(all(windows, feature = "windows-sandbox"))]
mod native;

/// Both this flag and the Cargo feature must be enabled. No unrestricted fallback.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SandboxConfig {
    /// Explicit execution opt-in, false by default.
    pub enabled: bool,
    /// Grant Internet client capability. Does not grant loopback exemptions.
    pub allow_network: bool,
    /// Aggregate committed memory limit across the job's processes.
    pub memory_limit_bytes: usize,
    /// Maximum time for the shell; all descendants are killed when it finishes.
    pub timeout_ms: u32,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            allow_network: false,
            memory_limit_bytes: 256 * 1024 * 1024,
            timeout_ms: 30_000,
        }
    }
}

/// Run one `cmd.exe /d /s /c` command in a newly created local workspace.
///
/// The workspace must not exist. Its parent must exist and be trusted. Results
/// written there remain for the caller to inspect and remove. Only the minimal
/// Windows environment allowlist is supplied; handles are not inherited.
/// This is not a permission or approval decision.
///
/// # Errors
/// Disabled feature/configuration, unsupported platform, invalid limits/path,
/// setup/cleanup failure, or timeout. A nonzero shell exit is returned as-is.
pub fn run_command(config: &SandboxConfig, workspace: &Path, command: &str) -> io::Result<u32> {
    if !config.enabled {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Windows sandbox is disabled. Enable it explicitly before running a command.",
        ));
    }
    if config.memory_limit_bytes < 16 * 1024 * 1024
        || config.timeout_ms == 0
        || config.timeout_ms > 300_000
        || command.is_empty()
        || command.contains('\0')
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Use a nonempty command, at least 16 MiB of memory, and a timeout of 1–300000 ms.",
        ));
    }
    #[cfg(all(windows, feature = "windows-sandbox"))]
    {
        native::run(config, workspace, command)
    }
    #[cfg(not(all(windows, feature = "windows-sandbox")))]
    {
        let _ = workspace;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "This runner requires Windows and the windows-sandbox Cargo feature. No command was started.",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_cannot_start_a_command() {
        let config: SandboxConfig = serde_json::from_str("{}").unwrap();
        assert!(!config.enabled && !config.allow_network);
        let result = run_command(&config, Path::new("unused"), "exit 0");
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn invalid_limits_are_rejected_before_setup() {
        let config = SandboxConfig {
            enabled: true,
            timeout_ms: u32::MAX,
            ..SandboxConfig::default()
        };
        assert_eq!(
            run_command(&config, Path::new("unused"), "exit 0")
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
