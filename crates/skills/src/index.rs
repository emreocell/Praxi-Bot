//! Skill loading, precedence, the prompt index and file access.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use parking_lot::RwLock;

use crate::bundled::BUNDLED;
use crate::skill::{Skill, SkillContext, Source, parse_skill, validate_name};

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

#[derive(Default)]
struct State {
    skills: BTreeMap<String, Skill>,
    issues: Vec<SkillIssue>,
}

struct Shared {
    paths: SkillPaths,
    state: RwLock<State>,
}

/// The set of loaded skills. Cheap to clone; clones share state, so a
/// `skill_write` is visible to every holder.
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
    let Ok(entries) = fs::read_dir(dir) else {
        return; // a missing directory is normal
    };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
    entries.sort_by_key(fs::DirEntry::file_name);
    for e in entries {
        let path = e.path();
        if !path.is_dir() {
            continue;
        }
        let issue = |message: String| SkillIssue {
            source,
            path: path.display().to_string(),
            message,
        };
        let text = match fs::read_to_string(path.join(SKILL_FILE)) {
            Ok(t) => t,
            Err(err) => {
                issues.push(issue(format!("cannot read {SKILL_FILE}: {err}")));
                continue;
            }
        };
        match parse_skill(&text, source, Some(path.clone())) {
            Ok(s) => out.push(s),
            Err(m) => issues.push(issue(m)),
        }
    }
}

impl SkillIndex {
    /// Load bundled, user and bot skills. Later sources override earlier ones by name.
    #[must_use]
    pub fn load(paths: SkillPaths) -> Self {
        let index = Self {
            inner: Arc::new(Shared {
                paths,
                state: RwLock::default(),
            }),
        };
        index.reload();
        index
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
        let Some(dir) = self.get(name).and_then(|s| s.dir) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        collect_files(&dir, &dir, 0, &mut out);
        out.retain(|f| f != SKILL_FILE);
        out.sort();
        out
    }

    /// Validate `content` and save it as `<bot skills dir>/<name>/SKILL.md`, then reload.
    ///
    /// # Errors
    /// No bot directory, invalid name/content, name mismatch, or I/O failure.
    pub fn write(&self, name: &str, content: &str) -> Result<PathBuf, String> {
        validate_name(name)?;
        let dir = self
            .write_dir()
            .ok_or("no per-bot skills directory is configured")?
            .join(name);
        let skill = parse_skill(content, Source::Bot, Some(dir.clone()))?;
        if skill.name != name {
            return Err(format!(
                "frontmatter name `{}` does not match requested name `{name}`",
                skill.name
            ));
        }
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let file = dir.join(SKILL_FILE);
        fs::write(&file, content).map_err(|e| e.to_string())?;
        self.reload();
        Ok(file)
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
