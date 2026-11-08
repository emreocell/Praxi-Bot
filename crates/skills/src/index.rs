//! Skill loading, precedence, the prompt index and file access.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use parking_lot::RwLock;

use crate::bundled::BUNDLED;
use crate::scoped_store::{self, ScopeLock};
use crate::skill::{Skill, SkillContext, Source, parse_skill};

/// Byte budget of [`SkillIndex::prompt_block`].
pub const PROMPT_BUDGET_BYTES: usize = 1536;
/// Largest extra file `skill_read` returns.
pub const MAX_FILE_BYTES: u64 = 256 * 1024;
/// File name of the skill definition.
pub const SKILL_FILE: &str = "SKILL.md";

/// Room kept for the "... N more" trailer line.
const TRAILER_ROOM: usize = 48;

/// A problem found while loading a skill (never fatal).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillIssue {
    /// Layer the file was in.
    pub source: Source,
    /// Path (or `bundled:<dir>`) of the offending skill.
    pub path: String,
    /// What is wrong.
    pub message: String,
}

/// Directories to load skills from, besides the bundled ones.
#[derive(Debug, Clone, Default)]
pub struct SkillPaths {
    /// `<data_dir>/skills/`.
    pub user: Option<PathBuf>,
    /// `<data_dir>/bots/<id>/skills/`; also where `skill_write` saves.
    pub bot: Option<PathBuf>,
}

impl SkillPaths {
    /// Standard layout under `data_dir`, optionally for one bot.
    #[must_use]
    pub fn from_data_dir(data_dir: &Path, bot_id: Option<&str>) -> Self {
        Self {
            user: Some(data_dir.join("skills")),
            bot: bot_id.map(|id| data_dir.join("bots").join(id).join("skills")),
        }
    }
}

/// The three skill scopes. `None` for `bundled` uses the embedded skills only;
/// an optional on-disk bundled root is also read-only.
#[derive(Debug, Clone, Default)]
pub struct SkillRoots {
    /// Additional shipped skills, never a write destination.
    pub bundled: Option<PathBuf>,
    /// Shared user skills (`<data_dir>/skills/`), including hub installs.
    pub shared: Option<PathBuf>,
    /// Parent of per-bot directories (`<data_dir>/bots/`).
    pub bots_dir: Option<PathBuf>,
}

impl SkillRoots {
    /// The standard layout. Existing shared skills are left in place.
    #[must_use]
    pub fn from_data_dir(data_dir: &Path) -> Self {
        Self {
            bundled: None,
            shared: Some(data_dir.join("skills")),
            bots_dir: Some(data_dir.join("bots")),
        }
    }

    /// Resolve one caller without granting a bot-less caller a write root.
    ///
    /// # Errors
    /// An invalid bot id or a symlink/reparse point in its scope path.
    pub fn resolve(&self, bot_id: Option<&str>) -> Result<SkillPaths, String> {
        let bot = match bot_id {
            Some(id) => {
                validate_bot_id(id)?;
                self.bots_dir
                    .as_ref()
                    .map(|root| -> Result<PathBuf, String> {
                        scoped_store::check_bot_path(root, id)?;
                        Ok(root.join(id).join("skills"))
                    })
                    .transpose()?
            }
            None => None,
        };
        Ok(SkillPaths {
            user: self.shared.clone(),
            bot,
        })
    }
}

/// A bot id is a single safe segment, using the workflow-name grammar.
/// Windows device names are rejected on every platform as well.
///
/// # Errors
/// Empty/overlong ids, anything outside `[a-z0-9_-]`, or a device name.
pub fn validate_bot_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-'))
        || scoped_store::is_device_name(id)
    {
        return Err("bot_id must be a safe path segment: 1-64 a-z, 0-9, '_' or '-' characters (no Windows device names)".into());
    }
    Ok(())
}

#[derive(Default)]
struct State {
    skills: BTreeMap<String, Skill>,
    issues: Vec<SkillIssue>,
}

struct Shared {
    paths: SkillPaths,
    roots: SkillRoots,
    state: RwLock<State>,
}

/// The set of loaded skills. Cheap to clone; clones share state, so
/// writes through this bound index are visible to its clones. Tool calls resolve
/// a fresh view for the calling bot; the root index exposes shared/bundled only.
#[derive(Clone)]
pub struct SkillIndex {
    inner: Arc<Shared>,
}

impl std::fmt::Debug for SkillIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SkillIndex")
            .field("skills", &self.inner.state.read().skills.len())
            .finish_non_exhaustive()
    }
}

fn load_dir(dir: &Path, source: Source, out: &mut Vec<Skill>, issues: &mut Vec<SkillIssue>) {
    if !dir.exists() {
        return;
    }
    let guard = if source == Source::Bundled {
        None
    } else {
        match ScopeLock::acquire(dir) {
            Ok(guard) => Some(guard),
            Err(message) => {
                issues.push(SkillIssue {
                    source,
                    path: dir.display().to_string(),
                    message,
                });
                return;
            }
        }
    };
    load_dir_locked(dir, source, out, issues);
    drop(guard);
}

/// The caller holds the scope lock (or this is the read-only bundled layer).
pub(crate) fn load_dir_locked(
    dir: &Path,
    source: Source,
    out: &mut Vec<Skill>,
    issues: &mut Vec<SkillIssue>,
) {
    let Ok(entries) = fs::read_dir(dir) else {
        return; // a missing directory is normal
    };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
    entries.sort_by_key(fs::DirEntry::file_name);
    let mut published = Vec::new();
    for e in entries {
        if e.file_name().to_string_lossy().starts_with('.') {
            continue; // transaction staging and lock files are not skills
        }
        let path = e.path();
        let issue = |message: String| SkillIssue {
            source,
            path: path.display().to_string(),
            message,
        };
        if let Err(message) = scoped_store::check_plain_path(&path) {
            issues.push(issue(message));
            continue;
        }
        if !path.is_dir() {
            continue;
        }
        let file = path.join(SKILL_FILE);
        if let Err(message) = scoped_store::check_plain_path(&file) {
            issues.push(issue(message));
            continue;
        }
        let text = match fs::read_to_string(&file) {
            Ok(t) => t,
            Err(err) => {
                issues.push(issue(format!("cannot read {SKILL_FILE}: {err}")));
                continue;
            }
        };
        match parse_skill(&text, source, Some(path.clone())) {
            Ok(s)
                if source == Source::User
                    && e.file_name().to_string_lossy() == format!("{}@{}", s.name, s.version) =>
            {
                published.push(s);
            }
            Ok(s) => out.push(s),
            Err(m) => issues.push(issue(m)),
        }
    }
    // An actual published version overrides legacy/hub directories by name.
    // Merely containing '@' does not make an arbitrary legacy folder published.
    out.extend(published);
}

impl SkillIndex {
    /// Load a fixed view. Existing embedders remain source compatible.
    /// Tool calls infer roots only from the standard `<data>/skills` or
    /// `<data>/bots/<id>/skills` layout. An arbitrary fixed directory never
    /// grants another bot access.
    #[must_use]
    pub fn load(paths: SkillPaths) -> Self {
        let bots_dir = match &paths.bot {
            Some(path) => path.parent().and_then(|bot| {
                let id = bot.file_name()?.to_str()?;
                let bots_dir = bot.parent()?;
                (path.file_name()? == "skills"
                    && bots_dir.file_name()? == "bots"
                    && validate_bot_id(id).is_ok())
                .then(|| bots_dir.to_path_buf())
            }),
            None => paths.user.as_ref().and_then(|shared| {
                (shared.file_name()? == "skills")
                    .then(|| shared.parent().map(|data_dir| data_dir.join("bots")))
                    .flatten()
            }),
        };
        let roots = SkillRoots {
            bundled: None,
            shared: paths.user.clone(),
            bots_dir,
        };
        Self::load_resolved(paths, roots)
    }

    /// Load a registry's shared/bundled view. Every tool call resolves a
    /// separate view with [`Self::for_bot`]. No bot directory is global.
    #[must_use]
    pub fn load_roots(roots: SkillRoots) -> Self {
        let paths = SkillPaths {
            user: roots.shared.clone(),
            bot: None,
        };
        Self::load_resolved(paths, roots)
    }

    fn load_resolved(paths: SkillPaths, roots: SkillRoots) -> Self {
        let index = Self {
            inner: Arc::new(Shared {
                paths,
                roots,
                state: RwLock::default(),
            }),
        };
        index.reload();
        index
    }

    /// Re-read the scopes visible to one caller. A missing id means only
    /// shared and bundled skills, never another bot's bound view.
    ///
    /// # Errors
    /// An invalid bot id or an unsafe bot scope path.
    pub fn for_bot(&self, bot_id: Option<&str>) -> Result<Self, String> {
        let paths = self.inner.roots.resolve(bot_id)?;
        Ok(Self::load_resolved(paths, self.inner.roots.clone()))
    }

    /// Build a per-bot prompt through the same resolver used by the tools.
    /// Embedders must pass the run's bot id; there is no ambient current bot.
    ///
    /// # Errors
    /// An invalid bot id or unsafe scope path.
    pub fn prompt_block_for_bot(
        &self,
        bot_id: Option<&str>,
        ctx: &SkillContext,
        suggested: &[String],
    ) -> Result<String, String> {
        Ok(self
            .for_bot(bot_id)?
            .prompt_block_suggesting(ctx, suggested))
    }

    /// Re-read all sources from disk.
    pub fn reload(&self) {
        let mut all = Vec::new();
        let mut issues = Vec::new();
        for (dir, text) in BUNDLED {
            match parse_skill(text, Source::Bundled, None) {
                Ok(s) => all.push(s),
                Err(message) => issues.push(SkillIssue {
                    source: Source::Bundled,
                    path: format!("bundled:{dir}"),
                    message,
                }),
            }
        }
        if let Some(d) = &self.inner.roots.bundled {
            load_dir(d, Source::Bundled, &mut all, &mut issues);
        }
        if let Some(d) = &self.inner.paths.user {
            load_dir(d, Source::User, &mut all, &mut issues);
        }
        if let Some(d) = &self.inner.paths.bot {
            load_dir(d, Source::Bot, &mut all, &mut issues);
        }
        let mut skills = BTreeMap::new();
        for s in all {
            skills.insert(s.name.clone(), s); // later source wins
        }
        *self.inner.state.write() = State { skills, issues };
    }

    fn check_bound_scope(&self) -> Result<(), String> {
        if let (Some(bot), Some(root)) = (&self.inner.paths.bot, &self.inner.roots.bots_dir) {
            let id = bot
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .ok_or("invalid bound bot skills path")?;
            validate_bot_id(id)?;
            if bot != &root.join(id).join("skills") {
                return Err("bound bot skills path does not match its root".into());
            }
            scoped_store::check_bot_path(root, id)?;
        }
        Ok(())
    }

    /// All skills, sorted by name.
    #[must_use]
    pub fn list(&self) -> Vec<Skill> {
        self.inner.state.read().skills.values().cloned().collect()
    }

    /// One skill by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Skill> {
        self.inner.state.read().skills.get(name).cloned()
    }

    /// Problems found during the last load.
    #[must_use]
    pub fn issues(&self) -> Vec<SkillIssue> {
        self.inner.state.read().issues.clone()
    }

    /// Directory `skill_write` saves into, if configured.
    #[must_use]
    pub fn write_dir(&self) -> Option<&Path> {
        self.inner.paths.bot.as_deref()
    }

    /// System-prompt block: name + description per skill, suggested skills
    /// first, at most [`PROMPT_BUDGET_BYTES`] bytes. Empty when no skills exist.
    #[must_use]
    pub fn prompt_block(&self, ctx: &SkillContext) -> String {
        self.prompt_block_suggesting(ctx, &[])
    }

    /// Like [`Self::prompt_block`], and the skills named in `suggested` (a
    /// router hint) are listed first as suggested too.
    #[must_use]
    pub fn prompt_block_suggesting(&self, ctx: &SkillContext, suggested: &[String]) -> String {
        let skills = self.list();
        if skills.is_empty() {
            return String::new();
        }
        let (mut ordered, rest): (Vec<_>, Vec<_>) = skills
            .iter()
            .partition(|s| s.matches(ctx) || suggested.contains(&s.name));
        let n_suggested = ordered.len();
        ordered.extend(rest);
        let mut out = String::from(
            "Skills (load one with skill_read when relevant; [suggested] = matches the current page/app):\n",
        );
        let total = ordered.len();
        let mut shown = 0;
        for (i, s) in ordered.iter().enumerate() {
            let tag = if i < n_suggested { "[suggested] " } else { "" };
            let line = format!("- {tag}{}: {}\n", s.name, s.description);
            let limit = if i + 1 == total {
                PROMPT_BUDGET_BYTES
            } else {
                PROMPT_BUDGET_BYTES - TRAILER_ROOM
            };
            if out.len() + line.len() > limit {
                break;
            }
            out.push_str(&line);
            shown += 1;
        }
        if shown < total {
            let _ = writeln!(out, "- ... {} more (use skill_list)", total - shown);
        }
        out
    }

    /// Read the `SKILL.md` body (`file == None`) or an extra file of the skill.
    ///
    /// # Errors
    /// Unknown skill, invalid or escaping path, missing/oversized/non-text file.
    pub fn read(&self, name: &str, file: Option<&str>) -> Result<String, String> {
        self.check_bound_scope()?;
        let skill = self
            .get(name)
            .ok_or_else(|| format!("unknown skill `{name}` (use skill_list)"))?;
        let Some(file) = file else {
            return Ok(skill.body);
        };
        let rel = safe_relative(file)?;
        if file.eq_ignore_ascii_case(SKILL_FILE) {
            return Ok(skill.body);
        }
        let dir = skill
            .dir
            .ok_or_else(|| format!("skill `{name}` is bundled and has no extra files"))?;
        let root = dir.canonicalize().map_err(|e| e.to_string())?;
        let full = root
            .join(&rel)
            .canonicalize()
            .map_err(|_| format!("file `{file}` not found in skill `{name}`"))?;
        if !full.starts_with(&root) {
            return Err("path escapes the skill directory".into());
        }
        let meta = fs::metadata(&full).map_err(|e| e.to_string())?;
        if !meta.is_file() {
            return Err(format!("`{file}` is not a file"));
        }
        if meta.len() > MAX_FILE_BYTES {
            return Err(format!("file is larger than {MAX_FILE_BYTES} bytes"));
        }
        fs::read_to_string(&full).map_err(|_| format!("`{file}` is not UTF-8 text"))
    }

    /// Extra files (relative, `/`-separated) of a skill, excluding `SKILL.md`.
    #[must_use]
    pub fn extra_files(&self, name: &str) -> Vec<String> {
        if self.check_bound_scope().is_err() {
            return Vec::new();
        }
        let Some(dir) = self.get(name).and_then(|s| s.dir) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        collect_files(&dir, &dir, 0, &mut out);
        out.retain(|f| f != SKILL_FILE);
        out.sort();
        out
    }

    /// Save against the per-bot version observed by this index, then reload.
    /// Prefer [`Self::write_versioned`] for tool/API calls, where the caller
    /// explicitly supplies the version it read.
    ///
    /// # Errors
    /// No bot root, invalid content, a version conflict, or an I/O error.
    pub fn write(&self, name: &str, content: &str) -> Result<PathBuf, String> {
        let expected = self.get(name).filter(|s| s.source == Source::Bot);
        self.write_versioned(name, content, expected.as_ref().map(|s| s.version.as_str()))
            .map(|written| written.path)
    }

    /// Compare the current per-bot version and atomically save a new version.
    /// `None` is create-only, even when a shared/bundled skill has that name.
    ///
    /// # Errors
    /// Missing bot root, malformed input, version conflict, or an I/O error.
    pub fn write_versioned(
        &self,
        name: &str,
        content: &str,
        expected_version: Option<&str>,
    ) -> Result<scoped_store::WrittenSkill, String> {
        self.check_bound_scope()?;
        let root = self
            .write_dir()
            .ok_or("skill_write requires a bot_id and a configured per-bot skills directory")?;
        let written = scoped_store::write(root, name, content, expected_version)?;
        self.reload();
        Ok(written)
    }

    /// Publish this bot's skill as `<shared>/<name>@<version>/`, preserving
    /// existing legacy/hub directories and their provenance. A different
    /// effective shared skill requires explicit `replace` authorization.
    ///
    /// # Errors
    /// Missing scopes/skill, unsafe paths, a replacement conflict, or I/O error.
    pub fn publish(&self, name: &str, replace: bool) -> Result<PathBuf, String> {
        self.check_bound_scope()?;
        let bot = self
            .write_dir()
            .ok_or("skill_publish requires a bot_id and a configured per-bot skills directory")?;
        let shared = self
            .inner
            .paths
            .user
            .as_deref()
            .ok_or("no shared skills directory is configured")?;
        let path = scoped_store::publish(bot, shared, name, replace)?;
        self.reload();
        Ok(path)
    }
}

fn collect_files(root: &Path, dir: &Path, depth: usize, out: &mut Vec<String>) {
    if depth > 4 || out.len() > 200 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.filter_map(Result::ok) {
        let p = e.path();
        if e.file_name().to_string_lossy().starts_with(".praxi-")
            || scoped_store::check_plain_path(&p).is_err()
        {
            continue;
        }
        if p.is_dir() {
            collect_files(root, &p, depth + 1, out);
        } else if let Ok(rel) = p.strip_prefix(root) {
            out.push(
                rel.components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/"),
            );
        }
    }
}

/// Accept only plain relative paths made of normal components.
fn safe_relative(file: &str) -> Result<PathBuf, String> {
    if file.is_empty() || file.contains('\0') {
        return Err("invalid file path".into());
    }
    let mut out = PathBuf::new();
    for c in Path::new(file).components() {
        match c {
            Component::Normal(n) => out.push(n),
            Component::CurDir => {}
            _ => return Err("file must be a relative path inside the skill directory".into()),
        }
    }
    if out.as_os_str().is_empty() {
        return Err("invalid file path".into());
    }
    Ok(out)
}
