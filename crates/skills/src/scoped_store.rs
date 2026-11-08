//! Locked per-bot writes and explicit publication. Locks cover the comparison
//! and replacement together, including independently constructed indexes.

use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use crate::frontmatter;
use crate::hub::util::{copy_files, ensure_deletable, hash_files, refuse_reparse, walk};
use crate::index::{SKILL_FILE, load_dir_locked};
use crate::skill::{Skill, Source, parse_skill, validate_name};

const LOCK_FILE: &str = ".praxi-skills.lock";

/// The saved path and the version to use on the next update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrittenSkill {
    /// Atomically replaced `SKILL.md`.
    pub path: PathBuf,
    /// New version in its frontmatter.
    pub version: String,
}

pub(crate) fn check_plain_path(path: &Path) -> Result<(), String> {
    refuse_reparse(path).map_err(|e| e.to_string())
}

pub(crate) fn check_bot_path(root: &Path, id: &str) -> Result<(), String> {
    check_plain_path(root)?;
    check_plain_path(&root.join(id))?;
    check_plain_path(&root.join(id).join("skills"))
}

pub(crate) fn is_device_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
}

fn disk_name(name: &str) -> Result<(), String> {
    validate_name(name)?;
    if is_device_name(name) {
        return Err("skill name must not be a Windows device name".into());
    }
    Ok(())
}

fn ensure_dir(path: &Path) -> Result<(), String> {
    check_plain_path(path)?;
    fs::create_dir_all(path).map_err(|e| format!("create {}: {e}", path.display()))?;
    check_plain_path(path)
}

/// File-backed lock, released on drop even after an error or process exit.
/// The handle is read+write (never append-only) on Windows as well as Unix.
pub(crate) struct ScopeLock {
    _file: File,
}

impl ScopeLock {
    pub(crate) fn acquire(root: &Path) -> Result<Self, String> {
        ensure_dir(root)?;
        let path = root.join(LOCK_FILE);
        check_plain_path(&path)?;
        if let Ok(meta) = fs::symlink_metadata(&path)
            && !meta.is_file()
        {
            return Err(format!(
                "skill scope lock {} is not a regular file",
                path.display()
            ));
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options
            .open(&path)
            .map_err(|e| format!("open skill scope lock {}: {e}", path.display()))?;
        fs4::FileExt::lock(&file)
            .map_err(|e| format!("lock skill scope {}: {e}", root.display()))?;
        Ok(Self { _file: file })
    }
}

fn check_version(version: &str) -> Result<(), String> {
    if version.is_empty()
        || version.len() > 64
        || !version.starts_with(|c: char| c.is_ascii_alphanumeric())
        || !version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'))
        || version.contains("..")
        || version.ends_with('.')
    {
        return Err("skill version must be a safe 1-64 character version (letters, digits, '.', '-', '_' or '+')".into());
    }
    Ok(())
}

fn next_version(version: &str) -> Result<String, String> {
    check_version(version)?;
    let (prefix, last) = version.rsplit_once('.').unwrap_or(("", version));
    let next = if last.bytes().all(|b| b.is_ascii_digit()) {
        let next = last
            .parse::<u64>()
            .ok()
            .and_then(|n| n.checked_add(1))
            .ok_or("skill version is exhausted")?;
        if prefix.is_empty() {
            next.to_string()
        } else {
            format!("{prefix}.{next}")
        }
    } else {
        format!("{version}.1")
    };
    check_version(&next)?;
    Ok(next)
}

fn with_version(content: &str, version: &str) -> Result<String, String> {
    let (front, body) = frontmatter::split(content)?;
    let mut out = format!("---\nversion: {version}\n");
    for line in front.lines() {
        let is_version = !line.starts_with([' ', '\t'])
            && line
                .split_once(':')
                .is_some_and(|(key, _)| key.trim() == "version");
        if !is_version {
            out.push_str(line);
            out.push('\n');
        }
    }
    out.push_str("---\n");
    out.push_str(&body);
    out.push('\n');
    Ok(out)
}

fn read_skill(path: &Path, name: &str, source: Source) -> Result<Skill, String> {
    check_plain_path(path)?;
    let file = path.join(SKILL_FILE);
    check_plain_path(&file)?;
    let text = fs::read_to_string(&file).map_err(|e| format!("read {}: {e}", file.display()))?;
    let skill = parse_skill(&text, source, Some(path.to_path_buf()))?;
    if skill.name != name {
        return Err(format!(
            "stored skill name `{}` does not match `{name}`",
            skill.name
        ));
    }
    Ok(skill)
}

pub(crate) fn write(
    root: &Path,
    name: &str,
    content: &str,
    expected_version: Option<&str>,
) -> Result<WrittenSkill, String> {
    disk_name(name)?;
    let dir = root.join(name);
    let incoming = parse_skill(content, Source::Bot, Some(dir.clone()))?;
    if incoming.name != name {
        return Err(format!(
            "frontmatter name `{}` does not match requested name `{name}`",
            incoming.name
        ));
    }
    let _guard = ScopeLock::acquire(root)?;
    check_plain_path(&dir)?;
    let file = dir.join(SKILL_FILE);
    check_plain_path(&file)?;
    let current = match fs::read_to_string(&file) {
        Ok(text) => Some(parse_skill(&text, Source::Bot, Some(dir.clone()))?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("read {}: {e}", file.display())),
    };
    if current.as_ref().is_some_and(|skill| skill.name != name) {
        return Err(format!("stored skill name does not match `{name}`"));
    }
    let actual = current.as_ref().map(|skill| skill.version.as_str());
    if actual != expected_version {
        return Err(format!(
            "skill `{name}` version conflict (expected {}, current {}); read it again with skill_read before writing",
            expected_version.unwrap_or("absent"),
            actual.unwrap_or("absent")
        ));
    }
    let version = if let Some(previous) = actual {
        next_version(previous)?
    } else {
        check_version(&incoming.version)?;
        incoming.version
    };
    let content = with_version(content, &version)?;
    // A longer version may push a formerly valid document over the size limit.
    parse_skill(&content, Source::Bot, Some(dir.clone()))?;
    ensure_dir(&dir)?;
    let mut staged = tempfile::Builder::new()
        .prefix(".praxi-write-")
        .tempfile_in(&dir)
        .map_err(|e| e.to_string())?;
    staged
        .write_all(content.as_bytes())
        .map_err(|e| e.to_string())?;
    staged.as_file().sync_all().map_err(|e| e.to_string())?;
    // NamedTempFile::persist atomically replaces an existing file on Windows
    // too; never remove the destination before attempting the replacement.
    staged
        .persist(&file)
        .map_err(|e| format!("replace {}: {e}", file.display()))?;
    Ok(WrittenSkill {
        path: file,
        version,
    })
}

struct MovedSkill {
    original: PathBuf,
    backup: TempDir,
}

fn rollback(moved: Vec<MovedSkill>, error: &str) -> String {
    let mut message = error.to_owned();
    for entry in moved.into_iter().rev() {
        let backup = entry.backup.path().join("skill");
        if let Err(e) = fs::rename(&backup, &entry.original) {
            // Keep the backup on rollback failure; TempDir must not delete the
            // only surviving copy, even if metadata is inaccessible.
            let _ = write!(
                message,
                "; rollback {} failed: {e} (backup {})",
                entry.original.display(),
                backup.display()
            );
            let _ = entry.backup.keep();
        }
    }
    message
}

fn published_dirs(shared: &Path, name: &str) -> Result<Vec<PathBuf>, String> {
    let prefix = format!("{name}@");
    let mut out = Vec::new();
    for entry in fs::read_dir(shared).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let filename = entry.file_name();
        let filename = filename.to_string_lossy();
        let Some(version) = filename.strip_prefix(&prefix) else {
            continue;
        };
        check_version(version)?;
        let path = entry.path();
        ensure_deletable(shared, &path).map_err(|e| e.to_string())?;
        let skill = read_skill(&path, name, Source::User)?;
        if skill.version != version {
            return Err(format!(
                "published directory {} has a mismatched version",
                path.display()
            ));
        }
        out.push(path);
    }
    out.sort();
    Ok(out)
}

pub(crate) fn publish(
    bot: &Path,
    shared: &Path,
    name: &str,
    replace: bool,
) -> Result<PathBuf, String> {
    publish_with(bot, shared, name, replace, |from, to| fs::rename(from, to))
}

fn publish_with(
    bot: &Path,
    shared: &Path,
    name: &str,
    replace: bool,
    commit: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
) -> Result<PathBuf, String> {
    disk_name(name)?;
    ensure_dir(bot)?;
    ensure_dir(shared)?;
    let bot_canon = bot.canonicalize().map_err(|e| e.to_string())?;
    let shared_canon = shared.canonicalize().map_err(|e| e.to_string())?;
    if bot_canon.starts_with(&shared_canon) || shared_canon.starts_with(&bot_canon) {
        return Err("bot and shared skill scopes must be disjoint".into());
    }
    // Writers take bot before shared. Loading scopes never holds both locks.
    let _bot_guard = ScopeLock::acquire(bot)?;
    let own_dir = bot.join(name);
    ensure_deletable(bot, &own_dir).map_err(|e| e.to_string())?;
    let skill = read_skill(&own_dir, name, Source::Bot)?;
    check_version(&skill.version)?;
    let files = walk(&own_dir).map_err(|e| e.to_string())?;
    let source_hash = hash_files(&files).map_err(|e| e.to_string())?;
    let _shared_guard = ScopeLock::acquire(shared)?;
    let mut existing = Vec::new();
    let mut issues = Vec::new();
    load_dir_locked(shared, Source::User, &mut existing, &mut issues);
    if !issues.is_empty() {
        return Err(format!(
            "cannot publish into an incomplete shared index: {}",
            issues
                .iter()
                .map(|i| i.message.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }
    if let Some(previous) = existing.iter().rev().find(|skill| skill.name == name) {
        let previous_dir = previous
            .dir
            .as_deref()
            .ok_or("shared skill has no directory")?;
        let previous_files = walk(previous_dir).map_err(|e| e.to_string())?;
        let previous_hash = hash_files(&previous_files).map_err(|e| e.to_string())?;
        if previous_hash != source_hash && !replace {
            return Err(format!(
                "shared skill `{name}` already differs; skill_publish requires replace: true"
            ));
        }
    }
    let target = shared.join(format!("{name}@{}", skill.version));
    let previous = published_dirs(shared, name)?;
    let staged = tempfile::Builder::new()
        .prefix(".praxi-publish-")
        .tempdir_in(shared)
        .map_err(|e| e.to_string())?;
    copy_files(&files, staged.path()).map_err(|e| e.to_string())?;
    let staged_files = walk(staged.path()).map_err(|e| e.to_string())?;
    if hash_files(&staged_files).map_err(|e| e.to_string())? != source_hash {
        return Err("skill changed while being staged for publication; retry".into());
    }
    let mut moved = Vec::new();
    for original in previous {
        let backup = match tempfile::Builder::new()
            .prefix(".praxi-publish-old-")
            .tempdir_in(shared)
        {
            Ok(backup) => backup,
            Err(e) => {
                let message = rollback(moved, &format!("stage publication backup: {e}"));
                return Err(message);
            }
        };
        if let Err(e) = fs::rename(&original, backup.path().join("skill")) {
            let message = rollback(moved, &format!("back up {}: {e}", original.display()));
            return Err(message);
        }
        moved.push(MovedSkill { original, backup });
    }
    if let Err(e) = commit(staged.path(), &target) {
        let message = rollback(moved, &format!("publish {}: {e}", target.display()));
        return Err(message);
    }
    // Readers use the same scope lock, so they see the complete old or new
    // version. Legacy/hub folders are retained; only previous publications
    // are replaced. Windows never renames onto a nonempty destination.
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SkillIndex, SkillRoots};

    #[test]
    fn failed_publication_swap_rolls_back_the_previous_shared_version() {
        let tmp = tempfile::tempdir().unwrap();
        let root = SkillIndex::load_roots(SkillRoots::from_data_dir(tmp.path()));
        let bot = root.for_bot(Some("a")).unwrap();
        let text = "---\nname: x\ndescription: x\n---\noriginal\n";
        bot.write("x", text).unwrap();
        let original = bot.publish("x", false).unwrap();
        let before = fs::read(original.join(SKILL_FILE)).unwrap();
        bot.write("x", &text.replace("original", "new")).unwrap();
        let error = publish_with(
            bot.write_dir().unwrap(),
            &tmp.path().join("skills"),
            "x",
            true,
            |_, _| Err(std::io::ErrorKind::PermissionDenied.into()),
        )
        .unwrap_err();
        assert!(error.contains("publish"));
        assert_eq!(fs::read(original.join(SKILL_FILE)).unwrap(), before);
        assert_eq!(
            root.for_bot(None).unwrap().read("x", None).unwrap(),
            "original"
        );
        assert!(!tmp.path().join("skills/x@0.1.1").exists());
        assert!(
            fs::read_dir(tmp.path().join("skills"))
                .unwrap()
                .all(|entry| {
                    !entry
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".praxi-publish-")
                })
        );
    }
}
