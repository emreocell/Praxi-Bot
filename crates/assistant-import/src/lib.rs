//! Find what other assistants left on a machine and plan what Praxi-Bot can take over.
//!
//! Used by `bot import detect|run` and by the desktop Import screen (through the daemon, so a
//! remote server imports from its own files). [`build_plan`] only reads; it writes nothing and
//! starts nothing. The plan lists, per item, whether it can be imported and why not.
//!
//! Sources: Claude Desktop and Claude Code (MCP servers, skills, agents, `CLAUDE.md`), Cursor and
//! VS Code (`mcp.json`), OpenAI Codex (`config.toml` servers, prompts, skills, `AGENTS.md`),
//! OpenClaw, Hermes, and a plain folder of skill / prompt markdown the owner points at.
//!
//! Rules that never bend:
//! - Everything read is untrusted text. Nothing is run and no server is contacted.
//! - MCP servers arrive **disabled**. A value in `env` / `headers` never stays in the config:
//!   the plan lists it as a secret slot (name only) and the caller stores it in the secret store,
//!   after the owner confirmed that one value. A value that is only a variable reference
//!   (`${env:TOKEN}`) is dropped, not copied. A command line or URL that itself holds what looks
//!   like a key is not imported.
//! - Skills (also those made from agent and prompt files) pass the skill scanner; only a
//!   low-risk one is imported. Instruction files go into a bot's instructions only when they fit,
//!   have no hidden characters and scan clean.
//! - API keys, channels and sign-ins are listed as skipped and never copied.
//!
//! The detection paths and the item mapping were adapted from the MIT-licensed `import-core`,
//! `claude-import`, `openclaw-import` and `hermes-import` crates of moltis
//! (<https://github.com/moltis-org/moltis>, Copyright (c) 2025 Fabien Penso); see
//! `THIRD_PARTY_NOTICES.md`.

#![allow(clippy::doc_markdown)] // product names stay plain

mod apps;
mod mcp;
mod skills_src;

use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};

use mcp_bridge::paste::Parsed;
pub use mcp_bridge::paste::create_body as server_body;
use serde::Serialize;
use skills::hub::{HubPaths, SkillHub};
use tempfile::TempDir;

/// Largest settings file (JSON / TOML) read from another app.
pub const MAX_CONFIG_BYTES: u64 = 1024 * 1024;
/// Largest markdown file (skill, agent, prompt, instruction file) read from another app.
pub const MAX_MARKDOWN_BYTES: u64 = 256 * 1024;
/// Most bytes one scan reads in all (the sizes of what it opens, counted before reading).
pub const MAX_TOTAL_BYTES: u64 = 16 * 1024 * 1024;
/// Most files and bytes a skill folder may hold to be looked at (the skill hub allows more).
pub const MAX_SKILL_FILES: usize = 200;
/// See [`MAX_SKILL_FILES`].
pub const MAX_SKILL_DIR_BYTES: u64 = 1024 * 1024;
/// Most directory entries looked at in one folder walk, whatever they are.
pub const MAX_DIR_ENTRIES: usize = 2_000;
/// Most servers taken from one settings file.
pub const MAX_SERVERS_PER_FILE: usize = 500;
/// Longest instruction file that is offered for a bot's instructions (a bot holds 8,000
/// characters in total, so this leaves room for what is already there).
pub const MAX_INSTRUCTION_CHARS: usize = 6_000;
/// Most markdown files (and skill folders) read from one folder.
pub const MAX_FOLDER_FILES: usize = 200;

/// Which app an item was found in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum App {
    /// Claude Desktop or Claude Code.
    Claude,
    /// Cursor.
    Cursor,
    /// VS Code.
    Vscode,
    /// OpenAI Codex CLI.
    Codex,
    /// OpenClaw.
    Openclaw,
    /// Hermes.
    Hermes,
    /// A folder the owner pointed at.
    Folder,
}

impl App {
    /// Every app, in the order the screen lists them.
    pub const ALL: [Self; 7] = [
        Self::Claude,
        Self::Cursor,
        Self::Vscode,
        Self::Codex,
        Self::Openclaw,
        Self::Hermes,
        Self::Folder,
    ];

    /// Display name.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::Cursor => "Cursor",
            Self::Vscode => "VS Code",
            Self::Codex => "Codex",
            Self::Openclaw => "OpenClaw",
            Self::Hermes => "Hermes",
            Self::Folder => "Folder",
        }
    }

    /// Stable key (`--from` value, id prefix).
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Cursor => "cursor",
            Self::Vscode => "vscode",
            Self::Codex => "codex",
            Self::Openclaw => "openclaw",
            Self::Hermes => "hermes",
            Self::Folder => "folder",
        }
    }

    /// The app with this key.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.key() == key)
    }
}

/// What an item is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// An MCP server.
    McpServer,
    /// A skill (a skill folder, or an agent / prompt file made into one).
    Skill,
    /// A `CLAUDE.md` / `AGENTS.md` that can go into a bot's instructions.
    Instructions,
    /// Something found but not importable here.
    Other,
}

impl Kind {
    fn key(self) -> &'static str {
        match self {
            Self::McpServer => "mcp",
            Self::Skill => "skill",
            Self::Instructions => "instructions",
            Self::Other => "other",
        }
    }
}

/// Machine-readable outcome of one item, so a screen can say it in the owner's language.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// Can be imported as it is.
    Ready,
    /// Can be imported; some values go to the secret store first.
    ReadyWithSecrets,
    /// The name is already used in Praxi-Bot.
    AlreadyHave,
    /// Another item of this list has the same name.
    Duplicate,
    /// An `npx` / `uvx` package without an exact version.
    Unpinned,
    /// The command is on the shell never-list.
    NeverList,
    /// A transport or setting Praxi-Bot does not support (SSE, for example).
    Unsupported,
    /// The entry is not a valid server or skill.
    Invalid,
    /// The file could not be read or parsed.
    Unreadable,
    /// The command line or URL itself holds what looks like a key.
    SecretInCommand,
    /// The command line or URL uses `${...}` variables Praxi-Bot does not fill in.
    Variables,
    /// The safety scan found something: read it first.
    ReviewFirst,
    /// Longer than a bot's instructions can hold.
    TooLong,
    /// Keys, sign-ins and channels are never copied.
    NeverCopied,
    /// Found, but there is no import for it.
    NotImported,
}

/// A value that moves to the secret store when the owner confirms it. Never the value itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SecretSlot {
    /// The secret's name in the store (`mcp/<server>/env/<KEY>`).
    pub name: String,
    /// The environment variable or header it belongs to.
    pub key: String,
    /// `env` or `header`.
    pub place: &'static str,
}

/// One line of the preview.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Item {
    /// Stable between a scan and the import (`<app>:<kind>:<name>`).
    pub id: String,
    /// Source app.
    pub app: App,
    /// Item kind.
    pub kind: Kind,
    /// Server or skill name (or a file name).
    pub name: String,
    /// The file it came from, home-relative (`~/.cursor/mcp.json`).
    pub source: String,
    /// `true` = can be imported.
    pub import: bool,
    /// `true` = ticked by default on the screen (instructions are not).
    pub default_on: bool,
    /// Outcome as a code.
    pub reason: Reason,
    /// Outcome as English text (CLI output; a screen should prefer `reason`).
    pub note: String,
    /// A safe one-line summary (never a value), or the folder of a skill to review.
    pub detail: Option<String>,
    /// Values that go to the secret store (names only).
    pub secrets: Vec<SecretSlot>,
    /// `env` / header names whose value was a variable reference and is not copied.
    pub dropped: Vec<String>,
}

impl Item {
    fn new(app: App, kind: Kind, name: &str, source: &str) -> Self {
        Self {
            id: format!("{}:{}:{name}", app.key(), kind.key()),
            app,
            kind,
            name: name.to_owned(),
            source: source.to_owned(),
            import: false,
            default_on: false,
            reason: Reason::NotImported,
            note: String::new(),
            detail: None,
            secrets: Vec::new(),
            dropped: Vec::new(),
        }
    }

    fn skipped(mut self, reason: Reason, note: impl Into<String>) -> Self {
        self.reason = reason;
        self.note = note.into();
        self
    }

    fn ready(mut self, reason: Reason, note: impl Into<String>) -> Self {
        self.import = true;
        self.default_on = true;
        self.reason = reason;
        self.note = note.into();
        self
    }
}

/// An MCP server to create (disabled) with the secrets to store first.
#[derive(Debug)]
pub struct ServerImport {
    /// The item this belongs to.
    pub item_id: String,
    /// The parsed config: secret references only. `secrets` holds the values.
    pub parsed: Parsed,
}

/// A skill folder to install.
#[derive(Debug)]
pub struct SkillImport {
    /// The item this belongs to.
    pub item_id: String,
    /// The folder with `SKILL.md` (a real folder of the other app, or a staged copy).
    pub dir: PathBuf,
}

/// Instruction text for a bot.
#[derive(Debug)]
pub struct InstructionsImport {
    /// The item this belongs to.
    pub item_id: String,
    /// The text (bounded, scanned, without hidden characters).
    pub text: String,
}

/// The preview plus what an import needs to write it.
#[derive(Default)]
pub struct Plan {
    /// Preview lines.
    pub items: Vec<Item>,
    /// Servers that can be created.
    pub servers: Vec<ServerImport>,
    /// Skills that can be installed.
    pub skills: Vec<SkillImport>,
    /// Instruction files that can go into a bot.
    pub instructions: Vec<InstructionsImport>,
    /// Staged skill folders made from agent and prompt files; they live as long as the plan.
    staging: Vec<TempDir>,
}

impl std::fmt::Debug for Plan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Plan")
            .field("items", &self.items.len())
            .field("servers", &self.servers.len())
            .field("skills", &self.skills.len())
            .field("instructions", &self.instructions.len())
            .finish_non_exhaustive()
    }
}

impl Plan {
    /// The preview line with this id.
    #[must_use]
    pub fn item(&self, id: &str) -> Option<&Item> {
        self.items.iter().find(|i| i.id == id)
    }
}

/// Where other apps keep their files.
#[derive(Clone, Debug)]
pub struct Roots {
    /// The user's home directory.
    pub home: PathBuf,
    /// `%APPDATA%` on Windows (Claude Desktop and VS Code live under it).
    pub appdata: Option<PathBuf>,
    /// `HERMES_HOME`, when set.
    pub hermes_home: Option<PathBuf>,
    /// `OPENCLAW_HOME`, when set.
    pub openclaw_home: Option<PathBuf>,
    /// `CODEX_HOME`, when set.
    pub codex_home: Option<PathBuf>,
}

impl Roots {
    /// Only `home`; everything else at its default place under it.
    #[must_use]
    pub fn at_home(home: impl Into<PathBuf>) -> Self {
        Self {
            home: home.into(),
            appdata: None,
            hermes_home: None,
            openclaw_home: None,
            codex_home: None,
        }
    }

    /// The real locations of this machine (of the server, when this runs in its daemon).
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let dir = |v: &str| {
            std::env::var_os(v)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
        };
        let home = dir("USERPROFILE").or_else(|| dir("HOME"))?;
        Some(Self {
            home,
            appdata: dir("APPDATA"),
            hermes_home: dir("HERMES_HOME"),
            openclaw_home: dir("OPENCLAW_HOME"),
            codex_home: dir("CODEX_HOME"),
        })
    }

    pub(crate) fn codex(&self) -> PathBuf {
        self.codex_home
            .clone()
            .unwrap_or_else(|| self.home.join(".codex"))
    }

    /// `path` as `~/...` when it is under the home folder, with `/` separators.
    pub(crate) fn show(&self, path: &Path) -> String {
        let text = |p: &Path| p.to_string_lossy().replace('\\', "/");
        match path.strip_prefix(&self.home) {
            Ok(rest) => format!("~/{}", text(rest)),
            Err(_) => text(path),
        }
    }
}

/// What to scan.
#[derive(Clone, Debug, Default)]
pub struct ScanOptions {
    /// Only this app (`App::key`); every known app when `None`. A folder is scanned only when
    /// `folder` is set.
    pub only: Option<String>,
    /// A folder of skill / prompt markdown the owner chose.
    pub folder: Option<PathBuf>,
}

/// State while the plan is built.
pub(crate) struct Ctx<'a> {
    pub roots: &'a Roots,
    pub hub: SkillHub,
    pub existing_skills: BTreeSet<String>,
    pub taken_servers: &'a BTreeSet<String>,
    pub plan: Plan,
    /// Bytes opened so far, against [`MAX_TOTAL_BYTES`].
    pub bytes_read: u64,
    /// While a chosen folder is scanned: its canonical path. Everything read must stay inside.
    pub jail: Option<PathBuf>,
}

impl Ctx<'_> {
    pub(crate) fn push(&mut self, item: Item) {
        self.plan.items.push(item);
    }

    pub(crate) fn other(&mut self, app: App, name: &str, source: &str, reason: Reason, note: &str) {
        let item = Item::new(app, Kind::Other, name, source).skipped(reason, note);
        self.push(item);
    }
}

/// Whether `meta` is a link (a symlink, or a reparse point on Windows).
pub(crate) fn is_link(meta: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        meta.file_type().is_symlink() || meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}

/// The metadata of `path` itself when it is a real file or folder (no link) and, with a `jail`,
/// its canonical path is inside it.
pub(crate) fn real_entry(path: &Path, jail: Option<&Path>) -> Option<std::fs::Metadata> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if is_link(&meta) {
        return None;
    }
    if let Some(jail) = jail
        && !path.canonicalize().ok()?.starts_with(jail)
    {
        return None;
    }
    Some(meta)
}

impl Ctx<'_> {
    /// Read a text file of at most `limit` bytes. Only a regular file is read (a pipe would block),
    /// the size counts against [`MAX_TOTAL_BYTES`], and the read itself stops at `limit`, so a
    /// file that grows meanwhile cannot get past it. In a chosen folder a link is refused.
    pub(crate) fn read_capped(&mut self, path: &Path, limit: u64) -> Result<String, &'static str> {
        let meta = match &self.jail {
            Some(jail) => real_entry(path, Some(jail)).ok_or("cannot be read")?,
            None => std::fs::metadata(path).map_err(|_| "cannot be read")?,
        };
        if !meta.is_file() {
            return Err("is not a regular file");
        }
        if meta.len() > limit {
            return Err("is too large to import");
        }
        if !self.charge(meta.len()) {
            return Err("was not read: this scan reached its size limit");
        }
        let file = std::fs::File::open(path).map_err(|_| "cannot be read")?;
        let mut bytes = Vec::new();
        file.take(limit + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "cannot be read")?;
        if bytes.len() as u64 > limit {
            return Err("is too large to import");
        }
        String::from_utf8(bytes).map_err(|_| "is not text")
    }

    /// Count `bytes` against the scan's total; `false` when that is more than it may read.
    pub(crate) fn charge(&mut self, bytes: u64) -> bool {
        match self.bytes_read.checked_add(bytes) {
            Some(total) if total <= MAX_TOTAL_BYTES => {
                self.bytes_read = total;
                true
            }
            _ => false,
        }
    }
}

/// Why a folder the owner chose is not read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FolderError {
    /// Not a full path, too long, or holds a NUL.
    NotAPath,
    /// Missing, not a folder, or cannot be opened.
    Unreadable,
    /// `/proc`, `/sys`, `/dev` or a Windows device path.
    SystemFolder,
}

impl std::fmt::Display for FolderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotAPath => "the folder must be a full path on this machine",
            Self::Unreadable => "the folder cannot be read",
            Self::SystemFolder => "system folders (/proc, /sys, /dev, device paths) are not read",
        })
    }
}

/// The folder as everything below it is checked against: absolute, no `..`, no links in the
/// path (`canonicalize`), and not a system pseudo-filesystem. The folder is the owner's choice,
/// so it is not limited to the home folder.
///
/// # Errors
/// [`FolderError`] says which rule it broke.
pub fn resolve_folder(path: &Path) -> Result<PathBuf, FolderError> {
    let text = path.to_string_lossy();
    if !path.is_absolute() || text.len() > 4096 || text.contains('\0') {
        return Err(FolderError::NotAPath);
    }
    // Looked at before and after `canonicalize`: a link to /proc is caught by the second look,
    // and a path Windows would open as a device never gets as far as the disk.
    if is_system_path(path, cfg!(windows)) {
        return Err(FolderError::SystemFolder);
    }
    let real = path.canonicalize().map_err(|_| FolderError::Unreadable)?;
    if is_system_path(&real, cfg!(windows)) {
        return Err(FolderError::SystemFolder);
    }
    if !real.is_dir() {
        return Err(FolderError::Unreadable);
    }
    Ok(real)
}

/// A pseudo-filesystem of the OS. `windows` says to apply the Windows rules too (device
/// namespaces, `CON`, `NUL`, `COM1`, ...); on other systems those are ordinary names.
fn is_system_path(path: &Path, windows: bool) -> bool {
    let mut lower = path.to_string_lossy().to_ascii_lowercase();
    if windows {
        lower = lower.replace('\\', "/");
    }
    if ["/proc", "/sys", "/dev"]
        .iter()
        .any(|p| lower == *p || lower.starts_with(&format!("{p}/")))
    {
        return true;
    }
    if !windows {
        return false;
    }
    // `\\.\` is the device namespace, `\??\` the NT one; `\\?\GLOBALROOT`, `\\?\Device`, `\\?\pipe` reach it too.
    if lower.starts_with("//./") || lower.starts_with("/??/") {
        return true;
    }
    let verbatim = lower.strip_prefix("//?/").unwrap_or(&lower);
    if ["globalroot", "device", "pipe", "harddiskvolume"]
        .iter()
        .any(|p| verbatim == *p || verbatim.starts_with(&format!("{p}/")))
    {
        return true;
    }
    lower.split('/').any(|part| {
        let stem = part.split('.').next().unwrap_or_default().trim_end();
        matches!(stem, "con" | "prn" | "aux" | "nul" | "conin$" | "conout$")
            || ["com", "lpt"].iter().any(|p| {
                stem.strip_prefix(p).is_some_and(|n| {
                    n.len() == 1 && n.starts_with(|c: char| ('1'..='9').contains(&c))
                })
            })
    })
}

/// Build the preview. Reads files; writes nothing.
///
/// `taken_servers` are the lower-cased names of the MCP servers already configured.
#[must_use]
pub fn build_plan(
    roots: &Roots,
    data_dir: &Path,
    taken_servers: &BTreeSet<String>,
    options: &ScanOptions,
) -> Plan {
    let hub = SkillHub::new(HubPaths::from_data_dir(data_dir));
    let mut existing: BTreeSet<String> = hub
        .list()
        .map(|l| l.into_iter().map(|s| s.entry.name).collect())
        .unwrap_or_default();
    if let Ok(entries) = std::fs::read_dir(data_dir.join("skills")) {
        existing.extend(
            entries
                .flatten()
                .filter_map(|e| e.file_name().into_string().ok()),
        );
    }
    let mut ctx = Ctx {
        roots,
        hub,
        existing_skills: existing,
        taken_servers,
        plan: Plan::default(),
        bytes_read: 0,
        jail: None,
    };
    let want = |app: App| {
        options.only.as_deref().is_none_or(|o| o == app.key())
            // A folder is only read when the owner named one.
            && (app != App::Folder || options.folder.is_some())
    };
    if want(App::Claude) {
        ctx.claude();
    }
    if want(App::Cursor) {
        ctx.cursor();
    }
    if want(App::Vscode) {
        ctx.vscode();
    }
    if want(App::Codex) {
        ctx.codex();
    }
    if want(App::Openclaw) {
        ctx.openclaw();
    }
    if want(App::Hermes) {
        ctx.hermes();
    }
    if want(App::Folder)
        && let Some(folder) = &options.folder
    {
        ctx.folder(folder);
    }
    ctx.plan
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pseudo_filesystems_and_windows_devices_are_system_paths() {
        for p in [
            "/proc",
            "/proc/1/root",
            "/sys/kernel",
            "/dev",
            "/dev/shm",
            "/DEV/null",
        ] {
            assert!(is_system_path(Path::new(p), false), "{p}");
        }
        for p in [
            "/home/me/proc",
            "/process",
            "/devices",
            "/srv/dev",
            "/home/me/con",
            "/tmp/nul",
        ] {
            assert!(!is_system_path(Path::new(p), false), "{p}");
        }
        for p in [
            r"\\.\C:",
            r"\\.\pipe\x",
            r"\\?\GLOBALROOT\Device\HarddiskVolume1",
            r"\\?\Device\Null",
            r"\??\C:\x",
            r"C:\work\CON",
            r"C:\work\nul.md",
            r"C:\work\COM1",
            r"D:\LPT9.txt",
        ] {
            assert!(is_system_path(Path::new(p), true), "{p}");
        }
        for p in [
            r"C:\Users\me\skills",
            r"\\?\C:\Users\me",
            r"\\?\UNC\server\share",
            r"C:\work\COM10",
            r"C:\work\console",
            r"C:\work\devices",
        ] {
            assert!(!is_system_path(Path::new(p), true), "{p}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn real_entry_refuses_a_link_and_a_path_that_resolves_outside_the_jail() {
        let jail = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        std::fs::write(out.path().join("secret.md"), "x").unwrap();
        std::os::unix::fs::symlink(out.path(), jail.path().join("door")).unwrap();
        let jail = jail.path().canonicalize().unwrap();
        // lstat of the file sees a regular file, but one component of the path is a link.
        let through = jail.join("door/secret.md");
        assert!(real_entry(&through, None).is_some());
        assert!(real_entry(&through, Some(&jail)).is_none());
        assert!(
            real_entry(&jail.join("door"), None).is_none(),
            "a link itself"
        );
        std::fs::write(jail.join("own.md"), "x").unwrap();
        assert!(real_entry(&jail.join("own.md"), Some(&jail)).is_some());
        assert!(real_entry(&jail.join("own.md"), Some(&jail.join("elsewhere"))).is_none());
    }
}
