//! Shareable bot profiles (issue #228): a versioned, allow-listed file that
//! carries what a bot *is* (name, persona, model preference, tool selection,
//! permission level and category rules, skill references, schedule ideas) and
//! nothing about the machine or the owner.
//!
//! Security rules this module enforces:
//!
//! * **Export is an allow-list.** The file is built field by field from the
//!   stored profile; unknown keys of the stored profile never travel. Secrets,
//!   API keys, vault entries, "always allow" grants, the egress waiver, memory,
//!   chats, knowledge sources, trusted folders and other machine paths, CLI
//!   engine settings, `runtime` targets, app (`mcp:*`) tool bindings, paired
//!   devices and the avatar are not part of the schema at all.
//! * **Import is strict and inert.** Size cap, schema `format` + `format_version`,
//!   `deny_unknown_fields` everywhere, per-field caps and character checks.
//!   Parsing never reads a path or fetches a URL; every string stays data.
//! * **A ceiling, not a request.** The importer picks the highest permission
//!   level the new bot may have (default: read-only). The file's preset and
//!   every category / initiator rule are clamped to it; the preview lists what
//!   was lowered. The egress waiver and standing grants cannot be expressed in
//!   the file, so they can never be imported. Unknown tools are dropped with a
//!   note. Skills are references only (never installed, never trusted here) and
//!   schedules are shown as ideas, never created.

use std::collections::BTreeMap;

use globset::Glob;
use permission_engine::{Level, Rule};
use protocol::{InitiatorKind, PermissionCategory};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;

/// Value of the `format` field.
pub const FORMAT: &str = "praxi-bot-profile";
/// Schema version written by this build and the only one it reads.
pub const FORMAT_VERSION: u32 = 1;
/// Largest file accepted (bytes of the text).
pub const MAX_FILE_BYTES: usize = 64 * 1024;
const MAX_PERSONA_CHARS: usize = 16_000;
const MAX_NAME_CHARS: usize = 80;
const MAX_TOOL_PATTERNS: usize = 128;
const MAX_TOOL_PATTERN_CHARS: usize = 96;
const MAX_SKILLS: usize = 64;
const MAX_SCHEDULES: usize = 16;
const MAX_MODELS: usize = 8;

/// Tools every bot keeps so an emptied allow-list never widens to "every tool".
const SAFE_FALLBACK_TOOLS: [&str; 2] = ["ask_user", "todo_write"];

// ------------------------------------------------------------------ the file

/// The shareable profile file (`*.praxi-bot.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SharedProfile {
    /// Always `praxi-bot-profile`.
    pub format: String,
    /// Schema version (`1`).
    pub format_version: u32,
    /// Template metadata (gallery files only; never written by an export).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<TemplateMeta>,
    /// Identity and instructions.
    pub bot: SharedBot,
    /// Model preference (a hint; the importer picks what is available).
    #[serde(default)]
    pub models: SharedModels,
    /// Permission level and category rules (a request, clamped on import).
    #[serde(default)]
    pub permissions: SharedPermissions,
    /// Tool selection.
    #[serde(default)]
    pub tools: SharedTools,
    /// Skills the bot expects (references; never installed or trusted by import).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<SharedSkill>,
    /// Scheduled-task ideas (never created by import).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub schedules: Vec<ScheduleIdea>,
}

/// Gallery metadata: id and per-language names and descriptions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TemplateMeta {
    /// Stable template id (`researcher`, ...).
    pub id: String,
    /// Display name per language code (`en`, `tr`, `de`).
    pub names: BTreeMap<String, String>,
    /// One-sentence description per language code.
    pub descriptions: BTreeMap<String, String>,
}

/// `bot` section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SharedBot {
    /// Display name.
    pub name: String,
    /// Persona / instructions.
    #[serde(default)]
    pub persona: String,
    /// Preferred language code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

/// `models` section: `provider/model` references.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(default, deny_unknown_fields)]
pub struct SharedModels {
    /// Preferred main model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executor: Option<String>,
    /// Planner model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub planner: Option<String>,
    /// Router classifier model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub classifier: Option<String>,
    /// Light model for simple first turns.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub light: Option<String>,
    /// Fallback models.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fallback: Vec<String>,
}

/// Permission level names, least to most permissive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum SharePreset {
    /// Everything denied.
    ReadOnly,
    /// Everything asks.
    ReadAndDraft,
    /// Reads free, changes ask (the app default).
    ApproveDestructive,
    /// No approval prompts.
    Full,
}

impl SharePreset {
    /// The profile `preset` string.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::ReadAndDraft => "read-and-draft",
            Self::ApproveDestructive => "approve-destructive",
            Self::Full => "full",
        }
    }

    fn level(self) -> ShareLevel {
        match self {
            Self::ReadOnly => ShareLevel::Deny,
            Self::ReadAndDraft | Self::ApproveDestructive => ShareLevel::Ask,
            Self::Full => ShareLevel::Allow,
        }
    }
}

/// A category level (`allow-with-limits` is not shareable: it resolves to ask).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ShareLevel {
    /// Always denied.
    Deny,
    /// Asks every time.
    Ask,
    /// Allowed.
    Allow,
}

impl ShareLevel {
    fn to_engine(self) -> Level {
        match self {
            Self::Deny => Level::Deny,
            Self::Ask => Level::Ask,
            Self::Allow => Level::Allow,
        }
    }

    fn from_engine(l: &Level) -> Self {
        match l {
            Level::Deny => Self::Deny,
            Level::Ask | Level::AllowWithLimits(_) => Self::Ask,
            Level::Allow => Self::Allow,
        }
    }
}

/// A per-category rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SharedRule {
    /// Level for every initiator without an override.
    pub level: ShareLevel,
    /// Per-initiator overrides (`human`, `schedule`, `routine`, ...).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[schema(value_type = BTreeMap<String, ShareLevel>)]
    pub by_initiator: BTreeMap<InitiatorKind, ShareLevel>,
}

/// `permissions` section.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(default, deny_unknown_fields)]
pub struct SharedPermissions {
    /// Requested level (unset = the app default, approve-destructive).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset: Option<SharePreset>,
    /// Category rules layered over the preset.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    #[schema(value_type = BTreeMap<String, SharedRule>)]
    pub categories: BTreeMap<PermissionCategory, SharedRule>,
}

/// `tools` section: allow / exclude globs over tool names.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default, deny_unknown_fields)]
pub struct SharedTools {
    /// Empty = every tool (still bounded by the permission level).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<String>,
    /// Always removed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
}

/// A skill reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SharedSkill {
    /// Skill name (a plain identifier, never a path).
    pub name: String,
    /// Version the author used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// A scheduled-task idea: text only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ScheduleIdea {
    /// Short title.
    pub title: String,
    /// What the bot should do.
    pub prompt: String,
    /// When, in plain words ("every weekday at 8").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
}

// ------------------------------------------------------------------ validation

fn bad(msg: impl Into<String>) -> String {
    msg.into()
}

fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}')
}

fn clean_text(field: &str, s: &str, max_chars: usize, multiline: bool) -> Result<(), String> {
    if s.chars().count() > max_chars {
        return Err(bad(format!(
            "{field} is longer than {max_chars} characters"
        )));
    }
    for c in s.chars() {
        let ok_ws = c == '\t' || (multiline && (c == '\n' || c == '\r'));
        if (c.is_control() && !ok_ws) || is_bidi_control(c) {
            return Err(bad(format!("{field} contains a control character")));
        }
    }
    if memory_store::contains_secret(s) {
        return Err(bad(format!(
            "{field} looks like it contains a secret (key, token or password)"
        )));
    }
    Ok(())
}

fn ident_ok(s: &str, extra: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || extra.contains(c))
}

fn validate_model_ref(field: &str, m: &str) -> Result<(), String> {
    let ok = ident_ok(m, "._:/@+-", 128)
        && m.split_once('/')
            .is_some_and(|(p, rest)| !p.is_empty() && !rest.is_empty());
    if ok {
        Ok(())
    } else {
        Err(bad(format!("{field} must look like provider/model")))
    }
}

fn validate_pattern(field: &str, p: &str) -> Result<(), String> {
    if !ident_ok(p, "_*?:.-/", MAX_TOOL_PATTERN_CHARS) || p.contains("..") {
        return Err(bad(format!("{field}: bad tool pattern {p:?}")));
    }
    Glob::new(p).map_err(|e| bad(format!("{field}: bad tool pattern {p:?}: {e}")))?;
    Ok(())
}

impl SharedProfile {
    /// Check every field. Called by [`parse`] and by [`export`].
    ///
    /// # Errors
    /// A short message naming the first offending field.
    pub fn validate(&self) -> Result<(), String> {
        if self.format != FORMAT {
            return Err(bad("this is not a Praxi-Bot profile file"));
        }
        if self.format_version != FORMAT_VERSION {
            return Err(bad(format!(
                "profile format version {} is not supported (this app reads version {FORMAT_VERSION})",
                self.format_version
            )));
        }
        let name = self.bot.name.trim();
        if name.is_empty() {
            return Err(bad("bot.name must not be empty"));
        }
        clean_text("bot.name", name, MAX_NAME_CHARS, false)?;
        clean_text("bot.persona", &self.bot.persona, MAX_PERSONA_CHARS, true)?;
        if let Some(l) = &self.bot.language
            && !ident_ok(l, "-", 16)
        {
            return Err(bad("bot.language must be a language code such as en"));
        }
        let m = &self.models;
        for (f, v) in [
            ("models.executor", &m.executor),
            ("models.planner", &m.planner),
            ("models.classifier", &m.classifier),
            ("models.light", &m.light),
        ] {
            if let Some(v) = v {
                validate_model_ref(f, v)?;
            }
        }
        if m.fallback.len() > MAX_MODELS {
            return Err(bad("models.fallback has too many entries"));
        }
        for v in &m.fallback {
            validate_model_ref("models.fallback", v)?;
        }
        if self.tools.allow.len() > MAX_TOOL_PATTERNS
            || self.tools.exclude.len() > MAX_TOOL_PATTERNS
        {
            return Err(bad("tools lists have too many entries"));
        }
        for p in &self.tools.allow {
            validate_pattern("tools.allow", p)?;
        }
        for p in &self.tools.exclude {
            validate_pattern("tools.exclude", p)?;
        }
        if self.skills.len() > MAX_SKILLS {
            return Err(bad("too many skills"));
        }
        for s in &self.skills {
            // A skill name is an identifier, never a path (`../x`, `a/b`, `.hidden`).
            if !ident_ok(&s.name, "._-", 64) || s.name.starts_with('.') || s.name.contains("..") {
                return Err(bad(format!("bad skill name {:?}", s.name)));
            }
            if let Some(v) = &s.version
                && !ident_ok(v, "._+-", 32)
            {
                return Err(bad("bad skill version"));
            }
        }
        if self.schedules.len() > MAX_SCHEDULES {
            return Err(bad("too many schedule ideas"));
        }
        for s in &self.schedules {
            if s.title.trim().is_empty() || s.prompt.trim().is_empty() {
                return Err(bad("a schedule idea needs a title and a prompt"));
            }
            clean_text("schedules.title", &s.title, 80, false)?;
            clean_text("schedules.prompt", &s.prompt, 2_000, true)?;
            if let Some(w) = &s.when {
                clean_text("schedules.when", w, 120, false)?;
            }
        }
        if let Some(meta) = &self.meta {
            if !ident_ok(&meta.id, "-", 40) {
                return Err(bad("meta.id must be a short slug"));
            }
            for (field, map, max) in [
                ("meta.names", &meta.names, 80),
                ("meta.descriptions", &meta.descriptions, 400),
            ] {
                if map.len() > 8 {
                    return Err(bad(format!("{field} has too many languages")));
                }
                for (k, v) in map {
                    if !ident_ok(k, "-", 8) {
                        return Err(bad(format!("{field}: bad language code")));
                    }
                    clean_text(field, v, max, false)?;
                }
            }
        }
        Ok(())
    }
}

/// Parse and validate a profile file. Never touches the file system or the
/// network: the text is the only input.
///
/// # Errors
/// A short message for the user: too large, not JSON, wrong format, newer
/// version, unknown field, or a field that fails its check.
pub fn parse(text: &str) -> Result<SharedProfile, String> {
    if text.len() > MAX_FILE_BYTES {
        return Err(bad(format!(
            "the profile file is larger than {} KiB",
            MAX_FILE_BYTES / 1024
        )));
    }
    let value: Value = serde_json::from_str(text.trim_start_matches('\u{feff}'))
        .map_err(|_| bad("the file is not valid JSON"))?;
    // Check the envelope first so a newer file gets a clear message instead of
    // an "unknown field" error.
    let Some(obj) = value.as_object() else {
        return Err(bad("this is not a Praxi-Bot profile file"));
    };
    if obj.get("format").and_then(Value::as_str) != Some(FORMAT) {
        return Err(bad("this is not a Praxi-Bot profile file"));
    }
    match obj.get("format_version").and_then(Value::as_u64) {
        Some(v) if v == u64::from(FORMAT_VERSION) => {}
        Some(v) => {
            return Err(bad(format!(
                "profile format version {v} is not supported (this app reads version {FORMAT_VERSION}); update the app"
            )));
        }
        None => return Err(bad("format_version is missing")),
    }
    let profile: SharedProfile =
        serde_json::from_value(value).map_err(|e| bad(format!("invalid profile: {e}")))?;
    profile.validate()?;
    Ok(profile)
}

// ------------------------------------------------------------------ export

fn s(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str).map(str::to_owned)
}

/// App (`mcp:*`) and similar bindings never travel.
fn is_binding_pattern(p: &str) -> bool {
    p.starts_with("mcp:") || p.starts_with("mcp_") || p.starts_with("composio")
}

fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Build the shareable profile of a stored bot profile (`config`) from the
/// allow-list. `skills` and `schedules` come from the caller (selection and
/// schedule store) and are optional.
///
/// # Errors
/// The persona or a schedule text looks like it holds a secret, or the result
/// fails validation.
pub fn export(
    name: &str,
    config: &Value,
    skills: Vec<SharedSkill>,
    schedules: Vec<ScheduleIdea>,
) -> Result<SharedProfile, String> {
    let bot = config.get("bot");
    let models = config.get("models");
    let model = |key: &str| s(models.and_then(|m| m.get(key))).filter(|m| !m.starts_with("cli:"));
    let mut categories = BTreeMap::new();
    if let Some(cats) = config
        .pointer("/permissions/categories")
        .and_then(Value::as_object)
    {
        for (k, v) in cats {
            let Ok(cat) = serde_json::from_value::<PermissionCategory>(json!(k)) else {
                continue;
            };
            let Ok(rule) = serde_json::from_value::<Rule>(v.clone()) else {
                continue;
            };
            categories.insert(
                cat,
                SharedRule {
                    level: ShareLevel::from_engine(&rule.level),
                    by_initiator: rule
                        .by_initiator
                        .iter()
                        .map(|(k, l)| (*k, ShareLevel::from_engine(l)))
                        .collect(),
                },
            );
        }
    }
    let preset = s(config.pointer("/permissions/preset"))
        .and_then(|p| serde_json::from_value::<SharePreset>(json!(p.replace('_', "-"))).ok());
    let keep = |v: Vec<String>| -> Vec<String> {
        v.into_iter().filter(|p| !is_binding_pattern(p)).collect()
    };
    let profile = SharedProfile {
        format: FORMAT.to_owned(),
        format_version: FORMAT_VERSION,
        meta: None,
        bot: SharedBot {
            name: name.trim().to_owned(),
            persona: s(bot.and_then(|b| b.get("persona"))).unwrap_or_default(),
            language: s(bot.and_then(|b| b.get("language"))),
        },
        models: SharedModels {
            executor: model("executor"),
            planner: model("planner"),
            classifier: model("classifier"),
            light: model("light"),
            fallback: strings(models.and_then(|m| m.get("fallback")))
                .into_iter()
                .filter(|m| !m.starts_with("cli:"))
                .collect(),
        },
        permissions: SharedPermissions { preset, categories },
        tools: SharedTools {
            allow: keep(strings(config.pointer("/tools/allow"))),
            exclude: strings(config.pointer("/tools/exclude")),
        },
        skills,
        schedules,
    };
    profile.validate()?;
    Ok(profile)
}

/// Pretty JSON text and a safe file name for an exported profile.
#[must_use]
pub fn to_file(profile: &SharedProfile) -> (String, String) {
    let slug: String = profile
        .bot
        .name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let slug = if slug.is_empty() {
        "bot".to_owned()
    } else {
        slug.chars().take(40).collect()
    };
    let text = serde_json::to_string_pretty(profile).unwrap_or_default();
    (format!("{slug}.praxi-bot.json"), text)
}

// ------------------------------------------------------------------ import plan

/// What the importer decided and the host knows.
#[derive(Debug, Clone)]
pub struct ImportOptions {
    /// Highest level the new bot may have.
    pub ceiling: SharePreset,
    /// Display name override.
    pub name: Option<String>,
    /// Chosen main model (`provider/model`); defaults to the file's if available.
    pub executor: Option<String>,
    /// The importer allows the whole computer for file tools (otherwise: no
    /// folders until chosen).
    pub computer_files: bool,
}

/// What this host has.
#[derive(Debug, Clone, Default)]
pub struct ImportContext {
    /// Names of the registered tools.
    pub known_tools: Vec<String>,
    /// Ids of the providers that are usable here.
    pub providers: Vec<String>,
    /// `(name, version)` of the skills installed here.
    pub installed_skills: Vec<(String, String)>,
}

/// One note in the preview. `code` is stable (the UI translates it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PreviewNote {
    /// `level_lowered`, `rule_lowered`, `tool_dropped_unknown`,
    /// `tool_dropped_binding`, `tools_all_unknown`, `model_cli_dropped`,
    /// `model_unavailable`, `model_dropped`, `skill_missing`,
    /// `schedules_not_created`, `files_none`.
    pub code: String,
    /// What it is about (tool pattern, category, model, skill), if anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
}

fn note(code: &str, subject: Option<&str>) -> PreviewNote {
    PreviewNote {
        code: code.to_owned(),
        subject: subject.map(str::to_owned),
    }
}

/// Requested versus granted level of one category.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CategoryPreview {
    /// Category name (`fs`, `shell`, ...).
    pub category: String,
    /// What the file asked for.
    pub requested: ShareLevel,
    /// What the bot will get.
    pub granted: ShareLevel,
}

/// A skill reference with its local state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SkillPreview {
    /// Skill name.
    pub name: String,
    /// Author's version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// A skill with this name is installed here (it keeps its own review state;
    /// import neither installs nor trusts anything).
    pub installed: bool,
}

/// What the profile will be allowed to do, shown before the bot is created.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[allow(clippy::struct_excessive_bools)] // one flag per kind of reach, shown as a list
pub struct ImportPreview {
    /// Name the bot will get.
    pub name: String,
    /// Persona text, in full: it is the bot's instructions and comes from a stranger.
    pub persona: String,
    /// Preferred language.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Model the file prefers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_model: Option<String>,
    /// Main model the bot will use; `null` until one is chosen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Level the file asked for.
    pub requested_level: SharePreset,
    /// Level the bot will get (never above the importer's ceiling).
    pub granted_level: SharePreset,
    /// Every category, requested versus granted.
    pub categories: Vec<CategoryPreview>,
    /// The tool allow list stays empty, meaning every tool (within the level).
    pub all_tools: bool,
    /// Tool patterns the bot may use.
    pub tools_allow: Vec<String>,
    /// Tool patterns removed.
    pub tools_exclude: Vec<String>,
    /// The bot could run shell commands.
    pub can_run_shell: bool,
    /// The bot could read or change files.
    pub can_use_files: bool,
    /// The bot could control the desktop.
    pub can_control_desktop: bool,
    /// The bot could use a browser.
    pub can_use_browser: bool,
    /// The bot could run scripts.
    pub can_run_scripts: bool,
    /// The bot could act on the user's own computer (`pc_*`).
    pub can_use_own_computer: bool,
    /// Skills the profile expects.
    pub skills: Vec<SkillPreview>,
    /// Schedule ideas (text only; none is created).
    pub schedule_ideas: Vec<ScheduleIdea>,
    /// What was lowered, dropped or left out, and why.
    pub notes: Vec<PreviewNote>,
}

/// The validated result: a stored profile config and its preview.
#[derive(Debug, Clone)]
pub struct ImportPlan {
    /// Profile JSON for a new bot (`bot`, `models`, `permissions`, `tools`, `files`, `runtime`).
    pub config: Value,
    /// Name of the bot.
    pub name: String,
    /// `provider/model` or `None` while the importer still has to choose.
    pub executor: Option<String>,
    /// What to show the user.
    pub preview: ImportPreview,
}

fn category_name(c: PermissionCategory) -> String {
    serde_json::to_value(c)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

const ALL_CATEGORIES: [PermissionCategory; 10] = [
    PermissionCategory::Fs,
    PermissionCategory::Shell,
    PermissionCategory::Desktop,
    PermissionCategory::Browser,
    PermissionCategory::Accounts,
    PermissionCategory::Messaging,
    PermissionCategory::Money,
    PermissionCategory::System,
    PermissionCategory::Remote,
    PermissionCategory::Script,
];

fn matches_any(patterns: &[String], name: &str) -> bool {
    patterns
        .iter()
        .any(|p| Glob::new(p).is_ok_and(|g| g.compile_matcher().is_match(name)))
}

fn provider_of(model: &str) -> &str {
    model.split_once('/').map_or(model, |(p, _)| p)
}

/// Turn a validated profile into a new bot's config under the importer's
/// ceiling. Pure: nothing is read or written.
///
/// # Errors
/// The chosen model is malformed or a CLI engine reference.
#[allow(clippy::too_many_lines)] // one ordered pipeline: model, level, tools, files
pub fn plan_import(
    profile: &SharedProfile,
    opts: &ImportOptions,
    ctx: &ImportContext,
) -> Result<ImportPlan, String> {
    let mut notes = Vec::new();

    // ---- name
    let name = opts
        .name
        .as_deref()
        .unwrap_or(&profile.bot.name)
        .trim()
        .to_owned();
    if name.is_empty() {
        return Err(bad("the bot name must not be empty"));
    }
    clean_text("name", &name, MAX_NAME_CHARS, false)?;

    // ---- models: a preference, only providers that exist here are kept
    let usable = |m: &str| ctx.providers.iter().any(|p| p == provider_of(m));
    let requested_model = profile.models.executor.clone();
    let executor = if let Some(chosen) = &opts.executor {
        validate_model_ref("model", chosen)?;
        if chosen.starts_with("cli:") {
            return Err(bad(
                "a CLI engine cannot be chosen through a profile import",
            ));
        }
        Some(chosen.clone())
    } else {
        match &requested_model {
            Some(m) if m.starts_with("cli:") => {
                notes.push(note("model_cli_dropped", Some(m)));
                None
            }
            Some(m) if usable(m) => Some(m.clone()),
            Some(m) => {
                notes.push(note("model_unavailable", Some(m)));
                None
            }
            None => None,
        }
    };
    let mut models = serde_json::Map::new();
    if let Some(e) = &executor {
        models.insert("executor".into(), json!(e));
    }
    for (key, v) in [
        ("planner", &profile.models.planner),
        ("classifier", &profile.models.classifier),
        ("light", &profile.models.light),
    ] {
        if let Some(m) = v {
            if m.starts_with("cli:") {
                notes.push(note("model_cli_dropped", Some(m)));
            } else if usable(m) {
                models.insert(key.into(), json!(m));
            } else {
                notes.push(note("model_dropped", Some(m)));
            }
        }
    }
    let fallback: Vec<&String> = profile
        .models
        .fallback
        .iter()
        .filter(|m| {
            let keep = !m.starts_with("cli:") && usable(m);
            if !keep {
                notes.push(note("model_dropped", Some(m)));
            }
            keep
        })
        .collect();
    if !fallback.is_empty() {
        models.insert("fallback".into(), json!(fallback));
    }

    // ---- permission level: min(file, ceiling), rules clamped to it
    let requested_level = profile
        .permissions
        .preset
        .unwrap_or(SharePreset::ApproveDestructive);
    let granted_level = requested_level.min(opts.ceiling);
    if granted_level < requested_level {
        notes.push(note("level_lowered", Some(granted_level.as_str())));
    }
    let bound = granted_level.level();
    let requested_base = requested_level.level();
    let mut categories_out = serde_json::Map::new();
    let mut category_view = Vec::new();
    for cat in ALL_CATEGORIES {
        let rule = profile.permissions.categories.get(&cat);
        let requested = rule.map_or(requested_base, |r| r.level);
        let granted = requested.min(bound);
        if let Some(r) = rule {
            let mut clamped = Rule::new(granted.to_engine());
            let mut lowered = granted < requested;
            for (k, l) in &r.by_initiator {
                let g = (*l).min(bound);
                lowered |= g < *l;
                clamped.by_initiator.insert(*k, g.to_engine());
            }
            if lowered {
                notes.push(note("rule_lowered", Some(&category_name(cat))));
            }
            categories_out.insert(
                category_name(cat),
                serde_json::to_value(&clamped).map_err(|e| e.to_string())?,
            );
        }
        category_view.push(CategoryPreview {
            category: category_name(cat),
            requested,
            granted,
        });
    }
    let mut permissions = serde_json::Map::new();
    permissions.insert("preset".into(), json!(granted_level.as_str()));
    if !categories_out.is_empty() {
        permissions.insert("categories".into(), Value::Object(categories_out));
    }

    // ---- tools: unknown and binding patterns dropped; an emptied list never
    // widens to "every tool"
    let mut allow = Vec::new();
    for p in &profile.tools.allow {
        if is_binding_pattern(p) {
            notes.push(note("tool_dropped_binding", Some(p)));
        } else if ctx
            .known_tools
            .iter()
            .any(|t| Glob::new(p).is_ok_and(|g| g.compile_matcher().is_match(t)))
        {
            allow.push(p.clone());
        } else {
            notes.push(note("tool_dropped_unknown", Some(p)));
        }
    }
    if allow.is_empty() && !profile.tools.allow.is_empty() {
        allow = SAFE_FALLBACK_TOOLS
            .iter()
            .map(|t| (*t).to_owned())
            .collect();
        notes.push(note("tools_all_unknown", None));
    }
    let exclude: Vec<String> = profile.tools.exclude.clone();
    let mut tools = serde_json::Map::new();
    if !allow.is_empty() {
        tools.insert("allow".into(), json!(allow));
    }
    if !exclude.is_empty() {
        tools.insert("exclude".into(), json!(exclude));
    }

    // ---- files: nothing until the importer says so
    let files = if opts.computer_files {
        json!({ "mode": "computer" })
    } else {
        notes.push(note("files_none", None));
        json!({ "mode": "folder", "folders": [] })
    };

    // ---- reach summary over the tools this host has
    let level_of = |c: PermissionCategory| {
        category_view
            .iter()
            .find(|v| v.category == category_name(c))
            .map_or(ShareLevel::Deny, |v| v.granted)
    };
    let live = |prefix: &str, c: PermissionCategory| {
        level_of(c) != ShareLevel::Deny
            && ctx.known_tools.iter().any(|t| {
                t.starts_with(prefix)
                    && (allow.is_empty() || matches_any(&allow, t))
                    && !matches_any(&exclude, t)
            })
    };

    // ---- skills and schedules: references and text only
    let skills = profile
        .skills
        .iter()
        .map(|sk| {
            let installed = ctx.installed_skills.iter().any(|(n, _)| n == &sk.name);
            if !installed {
                notes.push(note("skill_missing", Some(&sk.name)));
            }
            SkillPreview {
                name: sk.name.clone(),
                version: sk.version.clone(),
                installed,
            }
        })
        .collect();
    if !profile.schedules.is_empty() {
        notes.push(note("schedules_not_created", None));
    }

    let slug: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let mut bot = serde_json::Map::new();
    bot.insert(
        "id".into(),
        json!(format!(
            "{}-{}",
            if slug.is_empty() {
                "bot"
            } else {
                &slug[..slug.len().min(32)]
            },
            short_suffix()
        )),
    );
    bot.insert("name".into(), json!(name));
    bot.insert("persona".into(), json!(profile.bot.persona));
    if let Some(l) = &profile.bot.language {
        bot.insert("language".into(), json!(l));
    }
    let mut config = serde_json::Map::new();
    config.insert("bot".into(), Value::Object(bot));
    config.insert("models".into(), Value::Object(models));
    config.insert("permissions".into(), Value::Object(permissions));
    if !tools.is_empty() {
        config.insert("tools".into(), Value::Object(tools));
    }
    config.insert("files".into(), files);
    config.insert(
        "runtime".into(),
        json!({ "computer": "local", "browser": "local" }),
    );

    let preview = ImportPreview {
        name: name.clone(),
        persona: profile.bot.persona.clone(),
        language: profile.bot.language.clone(),
        requested_model,
        model: executor.clone(),
        requested_level,
        granted_level,
        categories: category_view.clone(),
        all_tools: allow.is_empty(),
        tools_allow: allow.clone(),
        tools_exclude: exclude.clone(),
        // "Folder only" hides shell and script tools, so they need the
        // importer's explicit whole-computer choice.
        can_run_shell: opts.computer_files && live("shell_", PermissionCategory::Shell),
        can_use_files: opts.computer_files && live("fs_", PermissionCategory::Fs),
        can_control_desktop: live("desktop_", PermissionCategory::Desktop),
        can_use_browser: live("browser_", PermissionCategory::Browser),
        can_run_scripts: opts.computer_files && live("script_", PermissionCategory::Script),
        can_use_own_computer: opts.computer_files
            && allow.iter().any(|p| p.starts_with("pc_"))
            && level_of(PermissionCategory::Fs) != ShareLevel::Deny,
        skills,
        schedule_ideas: profile.schedules.clone(),
        notes,
    };
    Ok(ImportPlan {
        config: Value::Object(config),
        name,
        executor,
        preview,
    })
}

fn short_suffix() -> String {
    let mut b = [0u8; 3];
    let _ = getrandom::fill(&mut b);
    format!("{:02x}{:02x}{:02x}", b[0], b[1], b[2])
}

// ------------------------------------------------------------------ gallery

const TEMPLATE_FILES: [&str; 5] = [
    include_str!("../templates/researcher.json"),
    include_str!("../templates/inbox-triage.json"),
    include_str!("../templates/daily-briefing.json"),
    include_str!("../templates/coder.json"),
    include_str!("../templates/family-helper.json"),
];

/// The built-in gallery, parsed and validated.
///
/// # Panics
/// Never at run time: a test parses every file, so a broken template fails CI.
#[must_use]
pub fn templates() -> Vec<SharedProfile> {
    TEMPLATE_FILES
        .iter()
        .filter_map(|t| parse(t).ok())
        .filter(|p| p.meta.is_some())
        .collect()
}

#[cfg(test)]
#[path = "profile_share_tests.rs"]
mod tests;
