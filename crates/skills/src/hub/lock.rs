//! Lockfile (`skills.lock.json`) and usage counters (`skills-usage.json`).

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::HubError;
use super::scan::RiskLevel;
use super::util::{atomic_write, io_err};

/// Current lockfile schema version.
pub const LOCK_VERSION: u32 = 1;

/// Where a skill was requested from (what the user typed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SourceSpec {
    /// A folder on this machine.
    Local {
        /// Absolute or relative folder path as given.
        path: String,
    },
    /// A git repository.
    Git {
        /// Clone URL.
        url: String,
        /// Requested branch, tag or commit; `None` means the default branch.
        rev: Option<String>,
        /// Skill folder inside the repository; `None` means the root.
        subdir: Option<String>,
    },
    /// An entry of a published index JSON.
    Index {
        /// URL of the index document.
        index_url: String,
        /// Skill name inside the index.
        name: String,
    },
}

impl SourceSpec {
    /// Short provenance tag such as `git:https://host/repo`, `local:C:/x`.
    #[must_use]
    pub fn tag(&self) -> String {
        match self {
            SourceSpec::Local { path } => format!("local:{path}"),
            SourceSpec::Git { url, .. } => format!("git:{url}"),
            SourceSpec::Index { index_url, name } => format!("index:{name}@{index_url}"),
        }
    }
}

/// The exact git revision a skill was installed from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitPin {
    /// Clone URL.
    pub url: String,
    /// Full commit hash.
    pub commit: String,
    /// Skill folder inside the repository.
    pub subdir: Option<String>,
}

/// One installed skill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockEntry {
    /// Skill name (also its directory name).
    pub name: String,
    /// Skill version from its frontmatter.
    pub version: String,
    /// What the user asked for; used by update.
    pub source: SourceSpec,
    /// Pinned commit for git and index installs; used by restore.
    pub pin: Option<GitPin>,
    /// Commit hash (shortcut for `pin.commit`).
    pub commit: Option<String>,
    /// SHA-256 content hash of the installed tree.
    pub hash: String,
    /// Overall risk level at install time.
    pub risk: RiskLevel,
    /// RFC 3339 UTC install time.
    pub installed_at: String,
    /// RFC 3339 UTC time of the last update, if any.
    pub updated_at: Option<String>,
}

/// The lockfile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockFile {
    /// Schema version.
    pub version: u32,
    /// Entries by skill name.
    pub skills: BTreeMap<String, LockEntry>,
}

impl Default for LockFile {
    fn default() -> Self {
        Self {
            version: LOCK_VERSION,
            skills: BTreeMap::new(),
        }
    }
}

impl LockFile {
    /// Read the lockfile; a missing file is an empty lock.
    ///
    /// # Errors
    /// Fails on unreadable or malformed files and on newer schema versions.
    pub fn load(path: &Path) -> Result<Self, HubError> {
        match fs::read_to_string(path) {
            Ok(text) => {
                let lock: LockFile = serde_json::from_str(&text)
                    .map_err(|e| HubError::Lock(format!("{}: {e}", path.display())))?;
                if lock.version > LOCK_VERSION {
                    return Err(HubError::Lock(format!(
                        "lockfile version {} is newer than supported {LOCK_VERSION}",
                        lock.version
                    )));
                }
                Ok(lock)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(io_err("read", path, &e)),
        }
    }

    /// Write the lockfile atomically.
    ///
    /// # Errors
    /// Fails on I/O errors.
    pub fn save(&self, path: &Path) -> Result<(), HubError> {
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| HubError::Lock(format!("serialize: {e}")))?;
        atomic_write(path, text.as_bytes())
    }
}

/// Usage counters of one skill.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageStats {
    /// How many times the skill was loaded/used.
    pub uses: u64,
    /// RFC 3339 UTC time of the last use.
    pub last_used_at: Option<String>,
}

/// Usage counters by skill name.
pub type UsageMap = BTreeMap<String, UsageStats>;

/// Read the usage file; missing or corrupt files yield an empty map (counters
/// are advisory and must never block the hub).
pub fn load_usage(path: &Path) -> UsageMap {
    fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Write the usage file atomically.
pub fn save_usage(path: &Path, usage: &UsageMap) -> Result<(), HubError> {
    let text = serde_json::to_string_pretty(usage)
        .map_err(|e| HubError::Lock(format!("serialize: {e}")))?;
    atomic_write(path, text.as_bytes())
}
