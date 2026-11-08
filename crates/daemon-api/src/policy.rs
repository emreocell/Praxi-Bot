//! Loading of the persisted global policy (`<data dir>/policy.json`).
//!
//! A missing file is the only case that means "use the default policy".  A
//! file that exists but cannot be read or parsed is an error the caller must
//! treat as fail-closed (SEC-13): [`AppState::new`](crate::AppState::new)
//! locks the daemon into the deny-all policy and reports the reason in
//! `GET /v1/policy` (`load_error`).

use std::path::Path;

use permission_engine::Policy;

/// File the global policy is persisted in (`<data dir>/policy.json`).
pub const POLICY_FILE: &str = "policy.json";

/// Why an existing `policy.json` could not be used.
#[derive(Debug)]
pub enum PolicyLoadError {
    /// The file exists but reading it failed.
    Io(std::io::Error),
    /// The file is not a valid policy.
    Parse(String),
}

impl std::fmt::Display for PolicyLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{POLICY_FILE} could not be read: {e}"),
            Self::Parse(e) => write!(f, "{POLICY_FILE} is not a valid policy: {e}"),
        }
    }
}

impl std::error::Error for PolicyLoadError {}

/// Load the persisted global policy.
///
/// `Ok(None)`: no file (first run), use the default.  Any other failure is an
/// `Err` and must never be treated as the default.
///
/// # Errors
/// An unreadable file or one that does not parse as a [`Policy`].
pub fn load_policy(data_dir: &Path) -> Result<Option<Policy>, PolicyLoadError> {
    let text = match std::fs::read_to_string(data_dir.join(POLICY_FILE)) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(PolicyLoadError::Io(e)),
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| PolicyLoadError::Parse(e.to_string()))
}

/// Validate a policy document without installing it.
///
/// # Errors
/// The text does not parse as a [`Policy`].
pub fn validate_policy(text: &str) -> Result<Policy, PolicyLoadError> {
    serde_json::from_str(text).map_err(|e| PolicyLoadError::Parse(e.to_string()))
}

/// Keep a copy of an unusable `policy.json` as `policy.json.corrupt` (the
/// original stays, so the next start is locked again instead of falling back
/// to the default).  Best effort.
pub(crate) fn preserve_corrupt(data_dir: &Path) {
    let _ = std::fs::copy(
        data_dir.join(POLICY_FILE),
        data_dir.join(format!("{POLICY_FILE}.corrupt")),
    );
}
