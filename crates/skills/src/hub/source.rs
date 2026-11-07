//! Where skills come from: local folders, git repositories, index documents.
//!
//! Network and git access sit behind the [`Fetcher`] and [`GitBackend`]
//! traits so the hub can be tested without either. The defaults shell out to
//! the `curl` and `git` command-line tools (no HTTP client dependency).

use std::path::{Component, Path};
use std::process::Command;

use serde::Deserialize;

use super::HubError;

/// Largest index document accepted.
pub const MAX_INDEX_BYTES: usize = 1024 * 1024;

/// What to install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallSource {
    /// A folder containing `SKILL.md`.
    Local(std::path::PathBuf),
    /// A git repository (shallow-cloned).
    Git {
        /// Clone URL (`https://`, `ssh://`, `git@host:path`, `file://` or a local path).
        url: String,
        /// Branch, tag or full commit hash to pin; `None` = default branch.
        rev: Option<String>,
        /// Skill folder inside the repository; `None` = repository root.
        subdir: Option<String>,
    },
    /// An entry of an index JSON served over `https://`.
    Index {
        /// URL of the index document.
        url: String,
        /// Skill name inside the index.
        name: String,
    },
}

/// Downloads small documents (index JSON).
pub trait Fetcher: Send + Sync {
    /// GET `url` and return the body.
    ///
    /// # Errors
    /// Any transport failure or body over [`MAX_INDEX_BYTES`].
    fn get(&self, url: &str) -> Result<Vec<u8>, HubError>;
}

/// Fetches with the `curl` CLI (`https` only, size and time limited).
#[derive(Debug, Default, Clone, Copy)]
pub struct CurlFetcher;

impl Fetcher for CurlFetcher {
    fn get(&self, url: &str) -> Result<Vec<u8>, HubError> {
        let out = Command::new("curl")
            .args([
                "--fail",
                "--silent",
                "--show-error",
                "--location",
                "--proto",
                "=https",
                "--proto-redir",
                "=https",
                "--max-time",
                "30",
                "--max-filesize",
            ])
            .arg(MAX_INDEX_BYTES.to_string())
            .arg("--")
            .arg(url)
            .output()
            .map_err(|e| HubError::Source(format!("cannot run curl: {e}")))?;
        if !out.status.success() {
            return Err(HubError::Source(format!(
                "curl failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        if out.stdout.len() > MAX_INDEX_BYTES {
            return Err(HubError::Source("index document is too large".into()));
        }
        Ok(out.stdout)
    }
}

/// Checks a git revision out into a directory.
pub trait GitBackend: Send + Sync {
    /// Shallow-fetch `rev` (or the default branch) of `url` into the empty
    /// directory `dest` and return the full commit hash that was checked out.
    ///
    /// # Errors
    /// Any git failure.
    fn checkout(&self, url: &str, rev: Option<&str>, dest: &Path) -> Result<String, HubError>;
}

/// Uses the `git` CLI: `init` + `fetch --depth 1` + `checkout`, which works
/// for branches, tags and full commit hashes alike.
#[derive(Debug, Default, Clone, Copy)]
pub struct GitCli;

fn git(dir: &Path, args: &[&str]) -> Result<String, HubError> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "protocol.ext.allow=never",
            "-c",
            "core.symlinks=false",
            "-c",
            "core.hooksPath=",
            "-c",
            "advice.detachedHead=false",
        ])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_HTTP_LOW_SPEED_LIMIT", "1000")
        .env("GIT_HTTP_LOW_SPEED_TIME", "30")
        .output()
        .map_err(|e| HubError::Git(format!("cannot run git: {e}")))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(HubError::Git(format!(
            "git {} failed: {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

impl GitBackend for GitCli {
    fn checkout(&self, url: &str, rev: Option<&str>, dest: &Path) -> Result<String, HubError> {
        validate_git_url(url)?;
        if let Some(r) = rev {
            validate_rev(r)?;
        }
        git(dest, &["init", "--quiet"])?;
        git(dest, &["remote", "add", "origin", url])?;
        git(
            dest,
            &[
                "fetch",
                "--quiet",
                "--depth",
                "1",
                "origin",
                rev.unwrap_or("HEAD"),
            ],
        )?;
        git(dest, &["checkout", "--quiet", "--detach", "FETCH_HEAD"])?;
        git(dest, &["rev-parse", "HEAD"])
    }
}

/// Reject git URLs that could be abused as options or exotic transports.
///
/// # Errors
/// [`HubError::Source`] describing the problem.
pub fn validate_git_url(url: &str) -> Result<(), HubError> {
    let bad = url.is_empty()
        || url.starts_with('-')
        || url.contains("::")
        || url.chars().any(|c| c.is_whitespace() || c.is_control());
    if bad {
        return Err(HubError::Source(format!("unsafe git url `{url}`")));
    }
    Ok(())
}

/// Reject revisions that are not plain ref names or hashes.
///
/// # Errors
/// [`HubError::Source`] describing the problem.
pub fn validate_rev(rev: &str) -> Result<(), HubError> {
    let ok = !rev.is_empty()
        && !rev.starts_with('-')
        && !rev.contains("..")
        && rev
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-'));
    if ok {
        Ok(())
    } else {
        Err(HubError::Source(format!("invalid git revision `{rev}`")))
    }
}

/// A `subdir` must be a plain relative path without `..`.
///
/// # Errors
/// [`HubError::Source`] describing the problem.
pub fn validate_subdir(subdir: &str) -> Result<(), HubError> {
    let p = Path::new(subdir);
    let ok = !subdir.is_empty()
        && !p.is_absolute()
        && p.components().all(|c| matches!(c, Component::Normal(_)));
    if ok {
        Ok(())
    } else {
        Err(HubError::Source(format!("invalid subdirectory `{subdir}`")))
    }
}

/// An index document: `{"version":1,"skills":[{...}]}`.
#[derive(Debug, Deserialize)]
pub struct IndexFile {
    /// Schema version (currently 1).
    #[serde(default)]
    pub version: u32,
    /// Listed skills.
    pub skills: Vec<IndexEntry>,
}

/// One skill listed in an index.
#[derive(Debug, Clone, Deserialize)]
pub struct IndexEntry {
    /// Skill name.
    pub name: String,
    /// Short description (display only).
    #[serde(default)]
    pub description: String,
    /// Git clone URL.
    pub git: String,
    /// Full commit hash to pin; strongly recommended.
    #[serde(default)]
    pub commit: Option<String>,
    /// Folder inside the repository.
    #[serde(default)]
    pub subdir: Option<String>,
    /// Expected content fingerprint (as printed by the scan report); verified
    /// after fetching when present.
    #[serde(default)]
    pub sha256: Option<String>,
}

/// Parse an index document.
///
/// # Errors
/// [`HubError::Source`] for malformed JSON or unsupported versions.
pub fn parse_index(bytes: &[u8]) -> Result<IndexFile, HubError> {
    if bytes.len() > MAX_INDEX_BYTES {
        return Err(HubError::Source("index document is too large".into()));
    }
    let idx: IndexFile = serde_json::from_slice(bytes)
        .map_err(|e| HubError::Source(format!("invalid index JSON: {e}")))?;
    if idx.version > 1 {
        return Err(HubError::Source(format!(
            "unsupported index version {}",
            idx.version
        )));
    }
    Ok(idx)
}
