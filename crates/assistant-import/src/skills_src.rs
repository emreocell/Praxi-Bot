//! Skills (folders, agent files, prompt files, loose markdown) and instruction files.

use std::path::{Path, PathBuf};

use skills::hub::{InstallSource, RiskLevel};
use skills::{MAX_DESCRIPTION_LEN, MAX_NAME_LEN, MAX_SKILL_BYTES, validate_name};

use crate::{
    App, Ctx, InstructionsImport, Item, Kind, MAX_FOLDER_FILES, MAX_INSTRUCTION_CHARS, Reason,
    SkillImport, read_text,
};

/// What a markdown file is made into.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum MdKind {
    /// A Claude Code agent (`name`, `description`, `tools`, `model` + prompt).
    Agent,
    /// A Codex custom prompt.
    Prompt,
    /// A loose markdown file in a chosen folder.
    Loose,
}

/// Skill folders under `dir`: `dir/<name>/SKILL.md`, and one level deeper
/// (`dir/<category>/<name>/SKILL.md`, how Hermes groups them).
fn skill_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // A link could point anywhere: only real folders are followed.
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        if path.join("SKILL.md").is_file() {
            found.push(path);
        } else if let Ok(inner) = std::fs::read_dir(&path) {
            for e in inner.flatten() {
                let p = e.path();
                if e.file_type().is_ok_and(|t| t.is_dir()) && p.join("SKILL.md").is_file() {
                    found.push(p);
                }
            }
        }
    }
    found.sort();
    found
}

/// Markdown files directly in `dir` and one folder level below (real files only).
fn markdown_files(dir: &Path, deep: bool) -> Vec<PathBuf> {
    fn is_md(p: &Path) -> bool {
        p.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"))
    }
    let mut found = Vec::new();
    let mut folders = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_file() && is_md(&path) {
            found.push(path);
        } else if deep && kind.is_dir() {
            let hidden = entry.file_name().to_string_lossy().starts_with('.');
            if !hidden && entry.file_name() != "node_modules" {
                folders.push(path);
            }
        }
    }
    folders.sort();
    for folder in folders {
        if let Ok(inner) = std::fs::read_dir(&folder) {
            for e in inner.flatten() {
                let p = e.path();
                if e.file_type().is_ok_and(|t| t.is_file()) && is_md(&p) {
                    found.push(p);
                }
            }
        }
    }
    found.sort();
    found.truncate(MAX_FOLDER_FILES);
    found
}

/// `name` / `description` of a leading `---` block, and the text after it.
fn split_front(text: &str) -> (Option<String>, Option<String>, String) {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = text.lines();
    if lines.next().map(str::trim_end) != Some("---") {
        return (None, None, text.trim().to_owned());
    }
    let (mut name, mut description) = (None, None);
    let mut rest = Vec::new();
    let mut closed = false;
    for line in lines {
        if closed {
            rest.push(line);
        } else if line.trim_end() == "---" {
            closed = true;
        } else if let Some((key, value)) = line.split_once(':') {
            let value = value.trim().trim_matches(['"', '\'']).trim();
            match key.trim() {
                "name" if !value.is_empty() => name = Some(value.to_owned()),
                "description" if !value.is_empty() => description = Some(value.to_owned()),
                _ => {}
            }
        }
    }
    if closed {
        (name, description, rest.join("\n").trim().to_owned())
    } else {
        // No closing line: not a frontmatter block, the file is plain text.
        (None, None, text.trim().to_owned())
    }
}

/// A skill name from free text: `a-z0-9._-`, at most 64 characters.
fn skill_name(raw: &str) -> Option<String> {
    let mut out = String::new();
    for c in raw.trim().chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_') {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let out: String = out
        .trim_matches(['-', '.'])
        .chars()
        .take(MAX_NAME_LEN)
        .collect();
    let out = out.trim_end_matches(['-', '.']).to_owned();
    validate_name(&out).ok().map(|()| out)
}

fn short(text: &str, max: usize) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    one_line.chars().take(max).collect()
}

/// Control or invisible characters (bidi marks, zero-width, tag characters).
pub(crate) fn has_hidden(text: &str) -> bool {
    text.chars().any(|c| {
        (c.is_control() && c != '\n' && c != '\t')
            || matches!(c as u32,
                0x00AD | 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x2064 | 0x2066..=0x2069
                | 0xFEFF | 0xE0000..=0xE007F)
    })
}

impl Ctx<'_> {
    /// Every `SKILL.md` folder under `dirs`.
    pub(crate) fn skill_folders(&mut self, app: App, dirs: &[PathBuf]) {
        for dir in dirs.iter().flat_map(|d| skill_dirs(d)) {
            let source = self.roots.show(&dir);
            self.skill_item(app, &source, &dir, true);
        }
    }

    /// Markdown files made into skills.
    pub(crate) fn md_skills(&mut self, app: App, dir: &Path, kind: MdKind) {
        for file in markdown_files(dir, kind == MdKind::Loose) {
            let stem = file
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_owned();
            // Instruction files of a folder are offered as instructions, not skills.
            let lower = stem.to_ascii_lowercase();
            if kind == MdKind::Loose
                && matches!(
                    lower.as_str(),
                    "readme" | "agents" | "claude" | "license" | "changelog"
                )
            {
                continue;
            }
            let source = self.roots.show(&file);
            let text = match read_text(&file) {
                Ok(t) => t,
                Err(why) => {
                    self.other(
                        app,
                        &stem,
                        &source,
                        Reason::Unreadable,
                        &format!("the file {why}"),
                    );
                    continue;
                }
            };
            let (name, description, body) = split_front(&text);
            let fallback = || {
                body.lines()
                    .map(|l| l.trim().trim_start_matches('#').trim())
                    .find(|l| !l.is_empty())
                    .map(|l| short(l, MAX_DESCRIPTION_LEN))
            };
            let description = description
                .map(|d| short(&d, MAX_DESCRIPTION_LEN))
                .or_else(fallback);
            let (Some(name), Some(description)) =
                (skill_name(name.as_deref().unwrap_or(&stem)), description)
            else {
                self.other(
                    app,
                    &stem,
                    &source,
                    Reason::Invalid,
                    "not usable as a skill: it needs a name and some text",
                );
                continue;
            };
            let note = match kind {
                MdKind::Agent => Some("the agent's tools and model settings are not copied"),
                MdKind::Prompt | MdKind::Loose => None,
            };
            self.synthesized_skill(app, &source, &name, &description, &body, note);
        }
    }

    /// Stage a skill folder made from `body` and plan it like any other skill.
    fn synthesized_skill(
        &mut self,
        app: App,
        source: &str,
        name: &str,
        description: &str,
        body: &str,
        note: Option<&str>,
    ) {
        if body.len() + 512 > MAX_SKILL_BYTES {
            self.other(
                app,
                name,
                source,
                Reason::Invalid,
                "too large to be a skill",
            );
            return;
        }
        let Ok(staging) = tempfile::tempdir() else {
            return;
        };
        let dir = staging.path().join(name);
        // JSON-quoted values are valid in the skill frontmatter and cannot break out of it.
        let front = format!(
            "---\nname: {}\ndescription: {}\n---\n",
            serde_json::Value::from(name),
            serde_json::Value::from(description)
        );
        let written = std::fs::create_dir_all(&dir)
            .and_then(|()| std::fs::write(dir.join("SKILL.md"), format!("{front}{body}\n")));
        if written.is_err() {
            return;
        }
        self.plan.staging.push(staging);
        let before = self.plan.items.len();
        self.skill_item(app, source, &dir, false);
        if let (Some(note), Some(item)) = (note, self.plan.items.get_mut(before))
            && item.import
        {
            item.note = format!("{}; {note}", item.note);
        }
    }

    /// Plan the skill in `dir` (a real folder, or a staged one when `real` is false).
    fn skill_item(&mut self, app: App, source: &str, dir: &Path, real: bool) {
        let folder = dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("skill")
            .to_owned();
        let preview = match self.hub.preview(&InstallSource::Local(dir.to_path_buf())) {
            Ok(p) => p,
            Err(e) => {
                let item = Item::new(app, Kind::Skill, &folder, source)
                    .skipped(Reason::Invalid, format!("not a usable skill: {e}"));
                self.push(item);
                return;
            }
        };
        let name = preview.name.clone();
        let item = Item::new(app, Kind::Skill, &name, source);
        // The same skill twice in one app is listed once.
        if self.plan.items.iter().any(|i| i.id == item.id) {
            return;
        }
        let item = if self
            .plan
            .items
            .iter()
            .any(|i| i.kind == Kind::Skill && i.name == name)
        {
            item.skipped(
                Reason::Duplicate,
                "another skill in this list has the same name",
            )
        } else if self.existing_skills.contains(&name) {
            item.skipped(
                Reason::AlreadyHave,
                "you already have a skill with this name",
            )
        } else {
            match preview.report.risk() {
                RiskLevel::Low => {
                    let mut item = item.ready(Reason::Ready, "scanned: no warnings");
                    item.detail = Some(short(&preview.description, MAX_DESCRIPTION_LEN));
                    self.plan.skills.push(SkillImport {
                        item_id: item.id.clone(),
                        dir: dir.to_path_buf(),
                    });
                    item
                }
                risk => {
                    let how = if real {
                        format!(
                            "read it first, then run `bot skills install \"{}\"`",
                            dir.display()
                        )
                    } else {
                        "read the file first".to_owned()
                    };
                    let mut item =
                        item.skipped(Reason::ReviewFirst, format!("scan risk {risk:?}: {how}"));
                    item.detail = real.then(|| self.roots.show(dir));
                    item
                }
            }
        };
        self.push(item);
    }

    /// `CLAUDE.md` / `AGENTS.md`: instructions for a bot, when they fit and are clean.
    pub(crate) fn instructions(&mut self, app: App, file: &Path, name: &str) {
        if !file.is_file() {
            return;
        }
        let source = self.roots.show(file);
        let item = Item::new(app, Kind::Instructions, name, &source);
        let text = match read_text(file) {
            Ok(t) => t.replace("\r\n", "\n").trim().to_owned(),
            Err(why) => {
                let item = item.skipped(Reason::Unreadable, format!("the file {why}"));
                self.push(item);
                return;
            }
        };
        if text.is_empty() {
            return;
        }
        if has_hidden(&text) {
            let item = item.skipped(
                Reason::Invalid,
                "contains control or invisible characters; nothing is added",
            );
            self.push(item);
            return;
        }
        let chars = text.chars().count();
        if chars > MAX_INSTRUCTION_CHARS {
            let skill = skill_name(&name.replace('.', "-"));
            let mut item = item.skipped(
                Reason::TooLong,
                format!(
                    "{chars} characters; a bot's instructions hold {MAX_INSTRUCTION_CHARS}. It is offered as a skill instead"
                ),
            );
            item.detail.clone_from(&skill);
            self.push(item);
            if let Some(skill) = skill {
                let description = format!("Notes imported from {name}");
                self.synthesized_skill(app, &source, &skill, &description, &text, None);
            }
            return;
        }
        // Scan the text like a skill: instructions are the most trusted place text can go.
        let risk = self.risk_of_text(&text);
        let item = match risk {
            Some(RiskLevel::Low) => {
                let mut item = item.ready(
                    Reason::Ready,
                    "added to the end of the instructions of the bot you choose",
                );
                // Instructions change how a bot behaves: never ticked without the owner reading them.
                item.default_on = false;
                item.detail = Some(short(&text, 240));
                self.plan.instructions.push(InstructionsImport {
                    item_id: item.id.clone(),
                    text,
                });
                item
            }
            Some(risk) => item.skipped(
                Reason::ReviewFirst,
                format!("scan risk {risk:?}: read the file first; nothing is added"),
            ),
            None => item.skipped(Reason::Invalid, "could not be scanned; nothing is added"),
        };
        self.push(item);
    }

    fn risk_of_text(&self, text: &str) -> Option<RiskLevel> {
        let staging = tempfile::tempdir().ok()?;
        let dir = staging.path().join("instructions");
        std::fs::create_dir_all(&dir).ok()?;
        let front = "---\nname: imported-instructions\ndescription: scan\n---\n";
        std::fs::write(dir.join("SKILL.md"), format!("{front}{text}\n")).ok()?;
        let preview = self.hub.preview(&InstallSource::Local(dir)).ok()?;
        Some(preview.report.risk())
    }
}
