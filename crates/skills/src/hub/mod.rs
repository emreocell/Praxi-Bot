//! Skill hub: install, update, uninstall and track third-party skills.
//!
//! Skills can come from a local folder, a git repository (shallow clone via
//! the `git` CLI, pinned to a commit) or an `https://` index JSON. Nothing is
//! installed blindly:
//!
//! 1. [`SkillHub::preview`] stages the skill in a temp directory, validates
//!    `SKILL.md` and runs the static scan ([`scan`]), returning a [`Preview`]
//!    with a [`RiskReport`]. The scan is advisory defense in depth. It is not
//!    the security boundary: a clean report does not by itself authorize a
//!    remote install.
//! 2. [`SkillHub::install`] copies the staged tree only when [`InstallOptions::accept`]
//!    is the fingerprint of that exact preview. Git URLs and index URLs always
//!    require it. A local folder requires it unless the risk is [`RiskLevel::Low`]
//!    and [`InstallOptions::trust_local`] is set. Update is the same call with
//!    the preview from [`SkillHub::preview_update`] and `replace: true`.
//! 3. A lockfile records name, source, commit, content hash and install time
//!    so [`SkillHub::restore`] / [`SkillHub::sync`] can reproduce installs and
//!    detect tampering. Usage counters live in a separate file.
//!
//! All operations are blocking (git, curl, file I/O); call them from
//! `spawn_blocking` in async code.
//!
//! # Wiring (follow-up, not done in this crate)
//! ```ignore
//! let hub = skills::hub::SkillHub::new(skills::hub::HubPaths::from_data_dir(&data_dir));
//! let preview = hub.preview(&InstallSource::Git { url, rev: None, subdir: None })?;
//! show(preview.report.render());                      // CLI / UI approval dialog
//! hub.install(preview, &InstallOptions { accept: Some(fp), replace: false, trust_local: false })?;
//! skill_index.reload();                               // pick up the new skill
//! // in the skill_read tool: hub.record_use(&name)?;  // usage counters
//! ```
//! CLI: `praxi skill install|update|uninstall|list|sync`; UI: a skills
//! settings page listing [`SkillHub::list`] with source tag, risk, usage.

pub mod lock;
pub mod scan;
pub mod source;
pub(crate) mod util;

use std::fs;
use std::path::{Path, PathBuf};

use parking_lot::Mutex;
use tempfile::TempDir;

pub use lock::{GitPin, LockEntry, LockFile, SourceSpec, UsageStats};
pub use scan::{Finding, FindingKind, RiskLevel, RiskReport, Severity, scan_dir};
pub use source::{
    CurlFetcher, Fetcher, GitBackend, GitCli, IndexEntry, InstallSource, MAX_INDEX_BYTES,
};

use crate::index::SKILL_FILE;
use crate::skill::{self, validate_name};
use util::{
    copy_files, ensure_deletable, hash_files, io_err, now_rfc3339, remove_dir_inside, walk,
};

/// Everything that can go wrong in the hub.
#[derive(Debug, thiserror::Error)]
pub enum HubError {
    /// File system problem.
    #[error("{0}")]
    Io(String),
    /// `SKILL.md` or the skill layout is invalid.
    #[error("invalid skill: {0}")]
    InvalidSkill(String),
    /// Symlinks, special files or similar.
    #[error("unsafe skill: {0}")]
    Unsafe(String),
    /// Too many files, too many bytes or too deep.
    #[error("skill too large: {0}")]
    TooLarge(String),
    /// Bad source (URL, revision, index).
    #[error("source error: {0}")]
    Source(String),
    /// A git command failed.
    #[error("git error: {0}")]
    Git(String),
    /// Lockfile problem.
    #[error("lockfile error: {0}")]
    Lock(String),
    /// The risk report has to be accepted first; pass `fingerprint` as
    /// [`InstallOptions::accept`] after showing `report` to the user.
    #[error("risk report not accepted (fingerprint {fingerprint})\n{report}")]
    NotAccepted {
        /// Fingerprint to pass back once the user accepted.
        fingerprint: String,
        /// Rendered report.
        report: String,
    },
    /// A skill with this name already exists.
    #[error("skill `{0}` already exists")]
    AlreadyInstalled(String),
    /// No such skill in the lockfile.
    #[error("skill `{0}` is not installed through the hub")]
    NotInstalled(String),
    /// Content differs from what was expected (index `sha256` or lockfile).
    #[error("content hash mismatch: expected {expected}, got {actual}")]
    HashMismatch {
        /// Expected fingerprint.
        expected: String,
        /// Actual fingerprint.
        actual: String,
    },
}

/// Locations the hub reads and writes.
#[derive(Debug, Clone)]
pub struct HubPaths {
    /// Directory skills are installed into (the user layer, `<data_dir>/skills`).
    pub skills_dir: PathBuf,
    /// Lockfile path.
    pub lock_file: PathBuf,
    /// Usage counters path.
    pub usage_file: PathBuf,
}

impl HubPaths {
    /// Standard layout: `<data_dir>/skills`, `skills.lock.json`, `skills-usage.json`.
    #[must_use]
    pub fn from_data_dir(data_dir: &Path) -> Self {
        Self {
            skills_dir: data_dir.join("skills"),
            lock_file: data_dir.join("skills.lock.json"),
            usage_file: data_dir.join("skills-usage.json"),
        }
    }
}

/// Options for [`SkillHub::install`] and for an update (`replace: true` on the
/// preview returned by [`SkillHub::preview_update`]).
#[derive(Debug, Clone, Default)]
pub struct InstallOptions {
    /// Fingerprint of the preview the caller is accepting.
    ///
    /// Must equal [`RiskReport::fingerprint`] exactly. Required for every git
    /// URL and every index URL, whatever the scanner reported, and for a local
    /// folder unless the risk is [`RiskLevel::Low`] and [`Self::trust_local`]
    /// is set. A token for any other fingerprint is refused.
    pub accept: Option<String>,
    /// Overwrite an existing hub-installed skill of the same name (update).
    /// Skills not installed through the hub are never overwritten.
    pub replace: bool,
    /// Install a low-risk local folder without [`Self::accept`].
    ///
    /// The caller is stating that it already trusts this folder. This flag does
    /// not apply to git or index sources, and it does not apply when the scan
    /// risk is above [`RiskLevel::Low`]. Defaults to `false`.
    pub trust_local: bool,
}

/// A staged, scanned skill waiting for acceptance. Dropping it discards the
/// staging directory.
#[derive(Debug)]
pub struct Preview {
    /// Skill name.
    pub name: String,
    /// Skill version.
    pub version: String,
    /// Skill description.
    pub description: String,
    /// What was requested.
    pub source: SourceSpec,
    /// Pinned git revision (git and index sources).
    pub pin: Option<GitPin>,
    /// The scan result to show to the user.
    pub report: RiskReport,
    _staging: TempDir,
    root: PathBuf,
}

/// A hub-installed skill with its status.
#[derive(Debug, Clone)]
pub struct InstalledSkill {
    /// Lockfile record.
    pub entry: LockEntry,
    /// Provenance tag, e.g. `git:https://host/repo`.
    pub source_tag: String,
    /// Usage counters.
    pub usage: UsageStats,
    /// False when the skill directory is missing (see [`SkillHub::sync`]).
    pub present: bool,
    /// True when files on disk differ from the locked hash.
    pub modified: bool,
}

/// The skill hub. See the module docs.
pub struct SkillHub {
    paths: HubPaths,
    fetcher: Box<dyn Fetcher>,
    git: Box<dyn GitBackend>,
    guard: Mutex<()>,
}

impl std::fmt::Debug for SkillHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SkillHub")
            .field("paths", &self.paths)
            .finish_non_exhaustive()
    }
}

impl SkillHub {
    /// Hub using the `git` and `curl` command-line tools.
    #[must_use]
    pub fn new(paths: HubPaths) -> Self {
        Self::with_backends(paths, Box::new(CurlFetcher), Box::new(GitCli))
    }

    /// Hub with custom network/git backends (tests, proxies).
    #[must_use]
    pub fn with_backends(
        paths: HubPaths,
        fetcher: Box<dyn Fetcher>,
        git: Box<dyn GitBackend>,
    ) -> Self {
        Self {
            paths,
            fetcher,
            git,
            guard: Mutex::new(()),
        }
    }

    /// Paths in use.
    #[must_use]
    pub fn paths(&self) -> &HubPaths {
        &self.paths
    }

    /// Fetch/copy `source` into a staging directory, validate it and scan it.
    ///
    /// # Errors
    /// Source, git, validation or scan-limit failures.
    pub fn preview(&self, source: &InstallSource) -> Result<Preview, HubError> {
        let staging = TempDir::new().map_err(|e| HubError::Io(format!("temp dir: {e}")))?;
        let (root, spec, pin, expect) = self.stage(source, staging.path())?;

        let skill_md = root.join(SKILL_FILE);
        let text = fs::read_to_string(&skill_md)
            .map_err(|e| HubError::InvalidSkill(format!("cannot read {SKILL_FILE}: {e}")))?;
        let skill =
            skill::parse_skill(&text, skill::Source::User, None).map_err(HubError::InvalidSkill)?;
        let report = scan_dir(&root)?;
        if let Some(expected) = expect
            && !expected.eq_ignore_ascii_case(&report.fingerprint)
        {
            return Err(HubError::HashMismatch {
                expected,
                actual: report.fingerprint,
            });
        }
        Ok(Preview {
            name: skill.name,
            version: skill.version,
            description: skill.description,
            source: spec,
            pin,
            report,
            _staging: staging,
            root,
        })
    }

    /// Install a previewed skill.
    ///
    /// Git and index previews always require [`InstallOptions::accept`] to be
    /// this preview's fingerprint. A local folder requires the same token
    /// unless its risk is [`RiskLevel::Low`] and [`InstallOptions::trust_local`]
    /// is set. Passing `replace: true` is the update path and uses the same
    /// rule.
    ///
    /// # Errors
    /// [`HubError::NotAccepted`] when acceptance is required and `opts.accept`
    /// is missing or is not this preview's fingerprint;
    /// [`HubError::AlreadyInstalled`]; I/O and lockfile failures.
    pub fn install(&self, preview: &Preview, opts: &InstallOptions) -> Result<LockEntry, HubError> {
        if acceptance_required(preview, opts)
            && opts.accept.as_deref() != Some(preview.report.fingerprint.as_str())
        {
            return Err(HubError::NotAccepted {
                fingerprint: preview.report.fingerprint.clone(),
                report: preview.report.render(),
            });
        }
        self.commit(preview, opts.replace)
    }

    /// Re-fetch the skill from its recorded source. Returns `None` when the
    /// content is unchanged, otherwise a [`Preview`] to pass to
    /// [`SkillHub::install`] with `replace: true`. That install still requires
    /// an accept token for the new fingerprint when the source is a git URL
    /// or an index URL, and for a local folder that is not a trusted low-risk
    /// tree.
    ///
    /// # Errors
    /// [`HubError::NotInstalled`], plus everything [`SkillHub::preview`] can return.
    pub fn preview_update(&self, name: &str) -> Result<Option<Preview>, HubError> {
        validate_name(name).map_err(HubError::InvalidSkill)?;
        let entry = self.entry(name)?;
        let preview = self.preview(&install_source(&entry.source))?;
        if preview.name != entry.name {
            return Err(HubError::InvalidSkill(format!(
                "upstream skill is now named `{}`, not `{}`",
                preview.name, entry.name
            )));
        }
        if preview.report.fingerprint == entry.hash {
            return Ok(None);
        }
        Ok(Some(preview))
    }

    /// Reinstall `name` exactly as locked (pinned commit, verified hash). The
    /// previously accepted content is trusted, so no acceptance is needed.
    ///
    /// # Errors
    /// [`HubError::HashMismatch`] when the source no longer yields the locked
    /// content, plus fetch/IO failures.
    pub fn restore(&self, name: &str) -> Result<LockEntry, HubError> {
        validate_name(name).map_err(HubError::InvalidSkill)?;
        let entry = self.entry(name)?;
        let source = match (&entry.pin, &entry.source) {
            (Some(pin), _) => InstallSource::Git {
                url: pin.url.clone(),
                rev: Some(pin.commit.clone()),
                subdir: pin.subdir.clone(),
            },
            (None, spec) => install_source(spec),
        };
        let mut preview = self.preview(&source)?;
        if preview.report.fingerprint != entry.hash {
            return Err(HubError::HashMismatch {
                expected: entry.hash,
                actual: preview.report.fingerprint,
            });
        }
        preview.source = entry.source.clone();
        self.commit(&preview, true)
    }

    /// Restore every locked skill whose directory is missing (fresh machine
    /// or deleted folder). Returns one result per restored skill.
    ///
    /// # Errors
    /// Only lockfile read failures; per-skill failures are in the result.
    #[allow(clippy::type_complexity)]
    pub fn sync(&self) -> Result<Vec<(String, Result<LockEntry, HubError>)>, HubError> {
        let lock = LockFile::load(&self.paths.lock_file)?;
        Ok(lock
            .skills
            .keys()
            .filter(|n| validate_name(n).is_ok() && !self.paths.skills_dir.join(n).exists())
            .map(|n| (n.clone(), self.restore(n)))
            .collect())
    }

    /// Remove a hub-installed skill, its lock entry and its usage counters.
    ///
    /// # Errors
    /// [`HubError::NotInstalled`] for skills the hub did not install.
    pub fn uninstall(&self, name: &str) -> Result<(), HubError> {
        validate_name(name).map_err(HubError::InvalidSkill)?;
        let _g = self.guard.lock();
        let dir = self.paths.skills_dir.join(name);
        ensure_deletable(&self.paths.skills_dir, &dir)?;
        let mut lock = LockFile::load(&self.paths.lock_file)?;
        if lock.skills.remove(name).is_none() {
            return Err(HubError::NotInstalled(name.to_string()));
        }
        remove_dir_inside(&self.paths.skills_dir, &dir)?;
        lock.save(&self.paths.lock_file)?;
        let mut usage = lock::load_usage(&self.paths.usage_file);
        if usage.remove(name).is_some() {
            lock::save_usage(&self.paths.usage_file, &usage)?;
        }
        Ok(())
    }

    /// All hub-installed skills with source tag, usage and drift status.
    ///
    /// # Errors
    /// Lockfile read failures.
    pub fn list(&self) -> Result<Vec<InstalledSkill>, HubError> {
        let lock = LockFile::load(&self.paths.lock_file)?;
        let usage = lock::load_usage(&self.paths.usage_file);
        Ok(lock
            .skills
            .into_values()
            .map(|entry| {
                let dir = validate_name(&entry.name)
                    .ok()
                    .map(|()| self.paths.skills_dir.join(&entry.name));
                let present = dir.as_ref().is_some_and(|path| path.is_dir());
                let modified = match &dir {
                    Some(path) if present => walk(path)
                        .and_then(|f| hash_files(&f))
                        .map_or(true, |h| h != entry.hash),
                    _ => false,
                };
                InstalledSkill {
                    source_tag: entry.source.tag(),
                    usage: usage.get(&entry.name).cloned().unwrap_or_default(),
                    entry,
                    present,
                    modified,
                }
            })
            .collect())
    }

    /// Count one use of skill `name` (any skill, hub-installed or not).
    ///
    /// # Errors
    /// Invalid names and I/O failures.
    pub fn record_use(&self, name: &str) -> Result<UsageStats, HubError> {
        validate_name(name).map_err(HubError::InvalidSkill)?;
        let _g = self.guard.lock();
        let mut usage = lock::load_usage(&self.paths.usage_file);
        let stats = usage.entry(name.to_string()).or_default();
        stats.uses += 1;
        stats.last_used_at = Some(now_rfc3339());
        let out = stats.clone();
        lock::save_usage(&self.paths.usage_file, &usage)?;
        Ok(out)
    }

    /// Usage counters of `name` (zeroes when never used).
    #[must_use]
    pub fn usage(&self, name: &str) -> UsageStats {
        lock::load_usage(&self.paths.usage_file)
            .remove(name)
            .unwrap_or_default()
    }

    fn entry(&self, name: &str) -> Result<LockEntry, HubError> {
        LockFile::load(&self.paths.lock_file)?
            .skills
            .remove(name)
            .ok_or_else(|| HubError::NotInstalled(name.to_string()))
    }

    /// Materialise `source` below `staging`. Returns the skill root, the
    /// recorded spec, the git pin and an expected fingerprint (index only).
    #[allow(clippy::type_complexity)]
    fn stage(
        &self,
        source: &InstallSource,
        staging: &Path,
    ) -> Result<(PathBuf, SourceSpec, Option<GitPin>, Option<String>), HubError> {
        match source {
            InstallSource::Local(path) => {
                if !path.is_dir() {
                    return Err(HubError::Source(format!(
                        "`{}` is not a directory",
                        path.display()
                    )));
                }
                let root = staging.join("skill");
                fs::create_dir_all(&root).map_err(|e| io_err("create dir", &root, &e))?;
                copy_files(&walk(path)?, &root)?;
                let spec = SourceSpec::Local {
                    path: path.to_string_lossy().into_owned(),
                };
                Ok((root, spec, None, None))
            }
            InstallSource::Git { url, rev, subdir } => {
                let (root, pin) =
                    self.clone_repo(url, rev.as_deref(), subdir.as_deref(), staging)?;
                let spec = SourceSpec::Git {
                    url: url.clone(),
                    rev: rev.clone(),
                    subdir: subdir.clone(),
                };
                Ok((root, spec, Some(pin), None))
            }
            InstallSource::Index { url, name } => {
                if !url.starts_with("https://") {
                    return Err(HubError::Source("index URL must use https://".into()));
                }
                let index = source::parse_index(&self.fetcher.get(url)?)?;
                let entry = index
                    .skills
                    .into_iter()
                    .find(|e| e.name == *name)
                    .ok_or_else(|| {
                        HubError::Source(format!("index has no skill named `{name}`"))
                    })?;
                let want = entry.commit.clone().ok_or_else(|| {
                    HubError::Source(format!("index entry `{name}` is missing a commit pin"))
                })?;
                source::validate_commit_pin(&want)?;
                let (root, pin) = self.clone_repo(
                    &entry.git,
                    Some(want.as_str()),
                    entry.subdir.as_deref(),
                    staging,
                )?;
                if !pin.commit.eq_ignore_ascii_case(&want) {
                    return Err(HubError::Source(format!(
                        "index commit `{want}` resolved to `{}`",
                        pin.commit
                    )));
                }
                let spec = SourceSpec::Index {
                    index_url: url.clone(),
                    name: name.clone(),
                };
                Ok((root, spec, Some(pin), entry.sha256))
            }
        }
    }

    fn clone_repo(
        &self,
        url: &str,
        rev: Option<&str>,
        subdir: Option<&str>,
        staging: &Path,
    ) -> Result<(PathBuf, GitPin), HubError> {
        source::validate_git_url(url)?;
        if let Some(r) = rev {
            source::validate_rev(r)?;
        }
        if let Some(s) = subdir {
            source::validate_subdir(s)?;
        }
        let repo = staging.join("repo");
        fs::create_dir_all(&repo).map_err(|e| io_err("create dir", &repo, &e))?;
        let commit = self.git.checkout(url, rev, &repo)?;
        let root = subdir.map_or_else(|| repo.clone(), |s| repo.join(s));
        if !root.is_dir() {
            return Err(HubError::InvalidSkill(format!(
                "subdirectory `{}` not found in repository",
                subdir.unwrap_or("")
            )));
        }
        Ok((
            root,
            GitPin {
                url: url.to_string(),
                commit,
                subdir: subdir.map(str::to_string),
            },
        ))
    }

    /// Copy the staged tree into the skills directory and write the lock.
    fn commit(&self, p: &Preview, replace: bool) -> Result<LockEntry, HubError> {
        validate_name(&p.name).map_err(HubError::InvalidSkill)?;
        let _g = self.guard.lock();
        // The staging dir is private, but verify nothing changed since the scan.
        let files = walk(&p.root)?;
        let actual = hash_files(&files)?;
        if actual != p.report.fingerprint {
            return Err(HubError::HashMismatch {
                expected: p.report.fingerprint.clone(),
                actual,
            });
        }

        let mut lock = LockFile::load(&self.paths.lock_file)?;
        let existing = lock.skills.get(&p.name).cloned();
        let skills_dir = &self.paths.skills_dir;
        let dest = skills_dir.join(&p.name);
        fs::create_dir_all(skills_dir).map_err(|e| io_err("create dir", skills_dir, &e))?;
        if fs::symlink_metadata(&dest).is_ok() {
            ensure_deletable(skills_dir, &dest)?;
            if existing.is_none() || !replace {
                return Err(HubError::AlreadyInstalled(p.name.clone()));
            }
        }

        let tmp = skills_dir.join(format!(".hub-new-{}", p.name));
        let old = skills_dir.join(format!(".hub-old-{}", p.name));
        for leftover in [&tmp, &old] {
            remove_dir_inside(skills_dir, leftover)?;
        }
        fs::create_dir_all(&tmp).map_err(|e| io_err("create dir", &tmp, &e))?;
        if let Err(e) = copy_files(&files, &tmp) {
            let _ = remove_dir_inside(skills_dir, &tmp);
            return Err(e);
        }
        let copied = walk(&tmp)?;
        let copied_hash = hash_files(&copied)?;
        if copied_hash != p.report.fingerprint {
            let _ = remove_dir_inside(skills_dir, &tmp);
            return Err(HubError::HashMismatch {
                expected: p.report.fingerprint.clone(),
                actual: copied_hash,
            });
        }

        let had_old = fs::symlink_metadata(&dest).is_ok();
        if had_old {
            ensure_deletable(skills_dir, &dest)?;
            fs::rename(&dest, &old).map_err(|e| io_err("rename", &dest, &e))?;
        }
        if let Err(e) = fs::rename(&tmp, &dest) {
            if had_old {
                let _ = fs::rename(&old, &dest);
            }
            let _ = remove_dir_inside(skills_dir, &tmp);
            return Err(io_err("rename", &dest, &e));
        }

        let now = now_rfc3339();
        let entry = LockEntry {
            name: p.name.clone(),
            version: p.version.clone(),
            source: p.source.clone(),
            commit: p.pin.as_ref().map(|g| g.commit.clone()),
            pin: p.pin.clone(),
            hash: p.report.fingerprint.clone(),
            risk: p.report.risk(),
            installed_at: existing
                .as_ref()
                .map_or_else(|| now.clone(), |e| e.installed_at.clone()),
            updated_at: existing.as_ref().map(|_| now),
        };
        lock.skills.insert(p.name.clone(), entry.clone());
        if let Err(e) = lock.save(&self.paths.lock_file) {
            let _ = remove_dir_inside(skills_dir, &dest);
            if had_old {
                let _ = fs::rename(&old, &dest);
            }
            return Err(e);
        }
        if had_old {
            let _ = remove_dir_inside(skills_dir, &old);
        }
        Ok(entry)
    }
}

/// Remote skills always need an accept token. A local folder needs one
/// unless the caller opts into trusting a low-risk tree.
fn acceptance_required(preview: &Preview, opts: &InstallOptions) -> bool {
    match &preview.source {
        SourceSpec::Git { .. } | SourceSpec::Index { .. } => true,
        SourceSpec::Local { .. } => !(opts.trust_local && preview.report.risk() == RiskLevel::Low),
    }
}

fn install_source(spec: &SourceSpec) -> InstallSource {
    match spec {
        SourceSpec::Local { path } => InstallSource::Local(PathBuf::from(path)),
        SourceSpec::Git { url, rev, subdir } => InstallSource::Git {
            url: url.clone(),
            rev: rev.clone(),
            subdir: subdir.clone(),
        },
        SourceSpec::Index { index_url, name } => InstallSource::Index {
            url: index_url.clone(),
            name: name.clone(),
        },
    }
}

#[cfg(test)]
mod tests;
