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
use std::path::{Path, PathBuf};

use mcp_bridge::paste::Parsed;
pub use mcp_bridge::paste::create_body as server_body;
use serde::Serialize;
use skills::hub::{HubPaths, SkillHub};
use tempfile::TempDir;

/// Largest config or instruction file read from another app.
pub const MAX_CONFIG_BYTES: u64 = 16 * 1024 * 1024;
/// Longest instruction file that is offered for a bot's instructions (a bot holds 8,000
/// characters in total, so this leaves room for what is already there).
pub const MAX_INSTRUCTION_CHARS: usize = 6_000;
/// Most markdown files read from one folder.
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

/// Read a text file bounded by [`MAX_CONFIG_BYTES`].
pub(crate) fn read_text(path: &Path) -> Result<String, &'static str> {
    let len = std::fs::metadata(path).map_err(|_| "cannot be read")?.len();
    if len > MAX_CONFIG_BYTES {
        return Err("is too large to be a settings file");
    }
    std::fs::read_to_string(path).map_err(|_| "cannot be read")
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
