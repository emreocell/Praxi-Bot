//! Bot profile: the subset of ADR-0011's TOML the loop needs.

use std::collections::BTreeMap;
use std::path::Path;

use cli_provider::{ApprovalLevel, EngineMode};
use globset::{Glob, GlobSet, GlobSetBuilder};
use permission_engine::{Policy, Rule};
use protocol::PermissionCategory;
use serde::{Deserialize, Deserializer, Serialize};

/// Errors while loading a profile.
#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    /// TOML syntax / shape error.
    #[error("profile toml: {0}")]
    Toml(String),
    /// Semantically invalid profile (unknown preset, bad glob...).
    #[error("invalid profile: {0}")]
    Invalid(String),
    /// File could not be read.
    #[error("profile io: {0}")]
    Io(#[from] std::io::Error),
}

/// Models per role (ADR-0008 / ADR-0011).
#[derive(Debug, Clone, Deserialize)]
pub struct ModelsConfig {
    /// Planner model (`provider/model`); informational in v0.
    pub planner: Option<String>,
    /// The model that runs the loop (`provider/model`).
    pub executor: String,
    /// Optional HTTP model for simple, clean first turns. Unset disables tiering.
    #[serde(default)]
    pub light: Option<String>,
    /// Optional K3 router classifier model.
    pub classifier: Option<String>,
    /// Ordered alternatives after a retryable error or missing capability.
    #[serde(default)]
    pub fallback: Vec<String>,
    /// Use only the curated direct set for models that struggle with discovery.
    #[serde(default)]
    pub direct_tools_only: bool,
}

/// `[permissions]` table: a preset and/or per-category rules.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PermissionsConfig {
    /// `read-only`, `approve-destructive`, `read-and-draft` or `full`.
    pub preset: Option<String>,
    /// Per-category rules layered over the preset.
    #[serde(default)]
    pub categories: BTreeMap<PermissionCategory, Rule>,
    /// This bot may send data off the machine after a run has read untrusted
    /// or private content, without an extra prompt. Default `false`. A tool
    /// grant does not turn this on. Not a permission level: [`BotProfile::policy`]
    /// ignores it.
    #[serde(default)]
    pub allow_egress_after_reads: bool,
}

impl PermissionsConfig {
    /// The bot's autonomy mode, derived from the preset (see
    /// [`permission_engine::AutonomyMode::from_preset`]).
    #[must_use]
    pub fn mode(&self) -> permission_engine::AutonomyMode {
        permission_engine::AutonomyMode::from_preset(self.preset.as_deref())
    }
}

/// `[tools]` table: allow / exclude globs over tool names.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ToolFilter {
    /// Empty = every tool.
    #[serde(default)]
    pub allow: Vec<String>,
    /// Always removed, even if allowed.
    #[serde(default)]
    pub exclude: Vec<String>,
}

/// `[router]` table: the K0-K3 decision ladder (ADR-0005) of this bot.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RouterSettings {
    /// Run the ladder before the main model (default `true`).
    pub enabled: bool,
    /// Use the K3 classifier model (`models.classifier`); defaults to `true`
    /// when a classifier model is configured.
    pub k3: Option<bool>,
}

impl Default for RouterSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            k3: None,
        }
    }
}

/// `[files]` table: what the built-in file tools may reach.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct FilesConfig {
    /// `computer` or `folder`; unset = the loop's default (This computer, but
    /// nothing on an always-on host until its owner chooses).
    #[serde(default)]
    pub mode: Option<FileAccessMode>,
    /// Folders for `folder` mode. Empty in that mode denies every file call.
    #[serde(default)]
    pub folders: Vec<String>,
}

/// Where the file tools may go.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileAccessMode {
    /// "This computer": anywhere the user can, protected locations excepted.
    #[default]
    Computer,
    /// "Folder only": `folders` and nothing else.
    Folder,
}

impl FilesConfig {
    /// The scope the file tools enforce; `unset_default` applies when the
    /// owner never chose a mode.
    #[must_use]
    pub fn scope(&self, unset_default: &builtin_tools::FsScope) -> builtin_tools::FsScope {
        match self.mode {
            None => unset_default.clone(),
            Some(FileAccessMode::Computer) => builtin_tools::FsScope::Computer,
            Some(FileAccessMode::Folder) => builtin_tools::FsScope::Folders(
                self.folders.iter().map(std::path::PathBuf::from).collect(),
            ),
        }
    }
}

/// When the owner consented to the risky engine settings of this bot.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EngineConsent {
    /// The owner agreed that a CLI runs this bot's file and shell actions.
    pub native_tools_at_ms: Option<i64>,
    /// The owner agreed to "Full access" (no approval prompts) for this bot.
    pub full_access_at_ms: Option<i64>,
}

/// `[engine]` table: who runs this bot's file and shell actions. Only the local
/// owner can change it (`PUT /v1/bots/{id}/engine`); bot create / update refuse it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EngineSettings {
    pub mode: EngineMode,
    pub approval_level: ApprovalLevel,
    /// Absolute folder chosen by the owner; required in `native_tools` mode.
    pub workspace: Option<String>,
    pub consent: EngineConsent,
}

#[derive(Deserialize)]
struct BotSection {
    id: String,
    name: String,
    #[serde(default)]
    persona: String,
    language: Option<String>,
}

#[derive(Deserialize)]
struct Raw {
    bot: BotSection,
    models: ModelsConfig,
    #[serde(default)]
    permissions: PermissionsConfig,
    #[serde(default)]
    tools: ToolFilter,
    #[serde(default)]
    router: RouterSettings,
    #[serde(default)]
    runtime: computer_target::RuntimeConfig,
    #[serde(default)]
    files: FilesConfig,
    #[serde(default)]
    engine: EngineSettings,
}

/// A loaded bot profile.
#[derive(Debug, Clone)]
pub struct BotProfile {
    /// Profile slug.
    pub id: String,
    /// Display name.
    pub name: String,
    /// System instruction / persona.
    pub persona: String,
    /// Preferred language code.
    pub language: Option<String>,
    /// Models per role.
    pub models: ModelsConfig,
    /// Permission policy configuration.
    pub permissions: PermissionsConfig,
    /// Tool allow / exclude lists.
    pub tools: ToolFilter,
    /// Decision-ladder settings.
    pub router: RouterSettings,
    /// Runtime configuration (`[runtime]`, including `computer` target).
    pub runtime: computer_target::RuntimeConfig,
    /// File access mode (`[files]`).
    pub files: FilesConfig,
    /// Engine mode, approval level and workspace of a CLI-backed bot.
    pub engine: EngineSettings,
    allow: GlobSet,
    exclude: GlobSet,
    /// Extra allow-lists a tool must also match (subagent narrowing).
    restrict: Vec<GlobSet>,
    hide_desktop: bool,
    hide_browser: bool,
    /// An unset file mode denies files on this daemon (see
    /// [`Self::with_unset_files_denied`]). Fails closed until the run context
    /// says otherwise; only the `pc_fs_*` visibility reads it.
    unset_files_denied: bool,
}

fn globs(patterns: &[String]) -> Result<GlobSet, String> {
    let mut b = GlobSetBuilder::new();
    for p in patterns {
        b.add(Glob::new(p).map_err(|e| format!("bad glob {p:?}: {e}"))?);
    }
    b.build().map_err(|e| e.to_string())
}

fn preset(name: &str) -> Option<Policy> {
    match name.replace('_', "-").as_str() {
        "read-only" => Some(Policy::read_only()),
        "approve-destructive" => Some(Policy::approve_destructive()),
        "read-and-draft" => Some(Policy::read_and_draft()),
        "full" => Some(Policy::full()),
        _ => None,
    }
}

impl TryFrom<Raw> for BotProfile {
    type Error = String;

    fn try_from(r: Raw) -> Result<Self, String> {
        if let Some(p) = &r.permissions.preset
            && preset(p).is_none()
        {
            return Err(format!("unknown permission preset {p:?}"));
        }
        validate_profile_models(&r.models)?;
        Ok(Self {
            allow: globs(&r.tools.allow)?,
            exclude: globs(&r.tools.exclude)?,
            restrict: Vec::new(),
            hide_desktop: false,
            hide_browser: false,
            unset_files_denied: true,
            id: r.bot.id,
            name: r.bot.name,
            persona: r.bot.persona,
            language: r.bot.language,
            models: r.models,
            permissions: r.permissions,
            tools: r.tools,
            router: r.router,
            runtime: r.runtime,
            files: r.files,
            engine: r.engine,
        })
    }
}

fn validate_profile_models(models: &ModelsConfig) -> Result<(), String> {
    if let Some(light) = &models.light {
        let Some((provider, model)) = light.split_once('/') else {
            return Err("light model must be an HTTP provider/model reference".into());
        };
        if provider.trim().is_empty()
            || model.trim().is_empty()
            || light.trim() != light
            || provider.starts_with("cli:")
        {
            return Err("light model must be an HTTP provider/model reference".into());
        }
    }
    for role in [models.planner.as_deref(), models.classifier.as_deref()]
        .into_iter()
        .flatten()
    {
        if role.starts_with("cli:") {
            return Err(format!(
                "planner/classifier models must be HTTP providers, got {role:?}"
            ));
        }
    }
    for model_ref in
        std::iter::once(models.executor.as_str()).chain(models.fallback.iter().map(String::as_str))
    {
        if model_ref.starts_with("cli:") {
            let parsed = provider_catalog::ModelRef(model_ref.to_owned());
            if let Some(suffix) = parsed
                .provider()
                .map(|_| parsed.model())
                .filter(|m| !m.is_empty() && *m != "default")
            {
                crate::cli::validate_cli_model(suffix)?;
            }
        }
    }
    Ok(())
}

impl<'de> Deserialize<'de> for BotProfile {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = Raw::deserialize(d)?;
        Self::try_from(raw).map_err(serde::de::Error::custom)
    }
}

impl BotProfile {
    /// Parse a profile from TOML text.
    ///
    /// # Errors
    /// Fails on syntax errors, missing required fields or an invalid preset / glob.
    pub fn from_toml_str(s: &str) -> Result<Self, ProfileError> {
        let raw: Raw = toml::from_str(s).map_err(|e| ProfileError::Toml(e.to_string()))?;
        Self::try_from(raw).map_err(ProfileError::Invalid)
    }

    /// Load a profile from a TOML file.
    ///
    /// # Errors
    /// Fails on I/O errors or as [`Self::from_toml_str`].
    pub fn from_path(path: &Path) -> Result<Self, ProfileError> {
        Self::from_toml_str(&std::fs::read_to_string(path)?)
    }

    /// The K3 classifier model when the ladder should use it: `router.k3`
    /// (default on) and a configured `models.classifier`.
    #[must_use]
    pub fn k3_model(&self) -> Option<&str> {
        self.models
            .classifier
            .as_deref()
            .filter(|_| self.router.k3.unwrap_or(true))
    }

    /// Whether `tool_name` passes the allow / exclude lists and resolved
    /// computer availability.
    #[must_use]
    pub fn tool_visible(&self, tool_name: &str) -> bool {
        if let Some(inner) = tool_name.strip_prefix(builtin_tools::pc::PC_PREFIX) {
            return self.pc_tool_visible(tool_name, inner);
        }
        // Shell commands and scripts cannot be confined to folders, so a
        // "Folder only" bot does not get them (honest sandbox, not a pretence).
        (self.files.mode != Some(FileAccessMode::Folder)
            || !(tool_name.starts_with("shell_") || tool_name.starts_with("script_")))
            && (!self.hide_desktop || !tool_name.starts_with("desktop_"))
            && (!self.hide_browser || !tool_name.starts_with("browser_"))
            && (self.tools.allow.is_empty() || self.allow.is_match(tool_name))
            && !self.exclude.is_match(tool_name)
            && self.restrict.iter().all(|g| g.is_match(tool_name))
    }

    /// The `pc_*` family (the user's own computer) is opt-in per bot: hidden
    /// unless `[tools] allow` has an entry written for it (one starting with
    /// `pc_`; a bare `*` is not an opt-in), and every per-bot restriction of
    /// the same tool on the bot's own computer applies to it as well: the
    /// `exclude` list and the "Folder only" rule are matched against the name
    /// without the prefix too, and the files of the user's computer need the
    /// bot's file access to be "This computer" (a "Folder only" bot, or one
    /// whose unset mode is denied on this host, never reads them).
    fn pc_tool_visible(&self, name: &str, inner: &str) -> bool {
        // Only an allow entry written for the family (`pc_...`) can match a
        // `pc_*` name: a bare `*` or `fs_*` next to `pc_info` never widens it.
        let opted_in = self
            .tools
            .allow
            .iter()
            .filter(|g| g.starts_with(builtin_tools::pc::PC_PREFIX))
            .any(|g| globset::Glob::new(g).is_ok_and(|g| g.compile_matcher().is_match(name)));
        let folder_only = self.files.mode == Some(FileAccessMode::Folder);
        let shell_like = inner.starts_with("shell_") || inner.starts_with("script_");
        let files_ok = match self.files.mode {
            Some(FileAccessMode::Computer) => true,
            Some(FileAccessMode::Folder) => false,
            None => !self.unset_files_denied,
        };
        let excluded = self.exclude.is_match(name) || self.exclude.is_match(inner);
        let files_denied = inner.starts_with("fs_") && !files_ok;
        let folder_denied = folder_only && shell_like;
        opted_in
            && !folder_denied
            && !files_denied
            && !excluded
            // A narrowed (subagent) surface must name the `pc_` tool itself.
            && self.restrict.iter().all(|g| g.is_match(name))
    }

    /// [`Self::tool_visible`] and the category rule together, for a tool of
    /// this name and category. The `pc_*` family acts on the user's own
    /// computer, so what the RUN's computer has (a desktop, a browser) does not
    /// hide it; the profile's own allow/exclude lists still do.
    #[must_use]
    pub fn spec_visible(&self, name: &str, category: PermissionCategory) -> bool {
        self.tool_visible(name)
            && (name.starts_with(builtin_tools::pc::PC_PREFIX) || self.category_visible(category))
    }

    /// Copy of this profile that knows whether an UNSET file mode is denied
    /// on this daemon (an always-on host whose owner never chose one).
    #[must_use]
    pub fn with_unset_files_denied(&self, denied: bool) -> Self {
        let mut p = self.clone();
        p.unset_files_denied = denied;
        p
    }

    /// Whether a tool's permission category is permitted by the resolved
    /// computer target.
    #[must_use]
    pub fn category_visible(&self, category: PermissionCategory) -> bool {
        match category {
            PermissionCategory::Desktop => !self.hide_desktop,
            PermissionCategory::Browser => !self.hide_browser,
            _ => true,
        }
    }

    /// Copy of this profile narrowed to the desktop/browser availability
    /// resolved at run start.
    #[must_use]
    pub fn with_computer_access(&self, desktop: bool, browser: bool) -> Self {
        let mut p = self.clone();
        p.hide_desktop = !desktop;
        p.hide_browser = !browser;
        p
    }

    /// The profile a subagent runs with: the same bot (persona, permissions,
    /// fallbacks), a tool surface that is the **intersection** of this
    /// profile's visible tools and `tools` (`None` = no narrowing; `Some([])`
    /// = no tools), and `executor` as its model when given.
    ///
    /// # Errors
    /// A malformed tool glob.
    pub(crate) fn narrowed(
        &self,
        tools: Option<&[String]>,
        executor: Option<&str>,
    ) -> Result<Self, String> {
        let mut p = self.clone();
        if let Some(tools) = tools {
            // The model sees dotted names with `.` replaced by `__`.
            let patterns: Vec<String> = tools
                .iter()
                .flat_map(|t| [t.clone(), t.replace("__", ".")])
                .collect();
            p.restrict.push(globs(&patterns)?);
        }
        if let Some(m) = executor {
            m.clone_into(&mut p.models.executor);
        }
        Ok(p)
    }

    /// Whether a bot with tool filter `candidate` could never see a tool this
    /// profile cannot (the ceiling for bots that create bots). Conservative: a
    /// pattern it cannot prove narrower is refused, so a glob must be one of
    /// this profile's own allow patterns (or this profile must allow every
    /// tool), and this profile's exclude patterns must all be carried over.
    ///
    /// # Errors
    /// The first pattern or omission that could widen the tool surface.
    pub fn covers_tool_filter(&self, candidate: &ToolFilter) -> Result<(), String> {
        let literal = |p: &str| !p.contains(['*', '?', '[', ']', '{', '}']);
        if candidate.allow.is_empty() && !self.tools.allow.is_empty() {
            return Err(
                "an empty tool list means every tool, which is more than this bot has; name the tools".into(),
            );
        }
        let all_literal = !candidate.allow.is_empty() && candidate.allow.iter().all(|p| literal(p));
        if !all_literal
            && let Some(missing) = self
                .tools
                .exclude
                .iter()
                .find(|e| !candidate.exclude.contains(e))
        {
            return Err(format!(
                "the exclusion {missing:?} of this bot must stay in the new tool filter"
            ));
        }
        for p in &candidate.allow {
            let ok = if literal(p) {
                self.tool_visible(p)
            } else {
                self.tools.allow.is_empty() || self.tools.allow.contains(p)
            };
            if !ok {
                return Err(format!(
                    "tool {p:?} is not one this bot has itself (use exact tool names it can see)"
                ));
            }
        }
        Ok(())
    }

    /// The bot's own permission policy; `None` when the profile defines none
    /// (the global policy then applies, ADR-0012).
    #[must_use]
    pub fn policy(&self) -> Option<Policy> {
        let mut policy = match self.permissions.preset.as_deref() {
            Some(p) => preset(p)?,
            None if self.permissions.categories.is_empty() => return None,
            None => Policy::default(),
        };
        for (cat, rule) in &self.permissions.categories {
            policy.categories.insert(*cat, rule.clone());
        }
        Some(policy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOML: &str = r#"
[bot]
id = "mira"
name = "Mira"
persona = "You are Mira."
language = "tr"

[models]
planner = "openrouter/big"
executor = "deepseek/fast"
fallback = ["local/x"]

[runtime]
computer = "local"

[permissions]
preset = "approve-destructive"

[tools]
allow = ["fs.*", "plan.*"]
exclude = ["fs.delete*"]
"#;

    #[test]
    fn runtime_computer_parses_defaults_values_aliases_and_rejects_unknown() {
        use computer_target::ComputerTarget;

        let minimal = r#"
[bot]
id = "b"
name = "B"
persona = "p"

[models]
executor = "fake/echo"
"#;
        let p = BotProfile::from_toml_str(minimal).unwrap();
        assert_eq!(p.runtime.computer, ComputerTarget::Auto);

        for (raw, expected) in [
            ("auto", ComputerTarget::Auto),
            ("pc", ComputerTarget::Pc),
            ("local", ComputerTarget::Pc),
            ("host", ComputerTarget::Host),
            ("browser", ComputerTarget::Browser),
            ("off", ComputerTarget::Off),
            ("node:pc-1", ComputerTarget::Node("node:pc-1".into())),
        ] {
            let src = format!("{minimal}\n[runtime]\ncomputer = \"{raw}\"\n");
            let parsed = BotProfile::from_toml_str(&src).unwrap();
            assert_eq!(parsed.runtime.computer, expected);
        }

        let bad = format!("{minimal}\n[runtime]\ncomputer = \"unknown_target\"\n");
        assert!(BotProfile::from_toml_str(&bad).is_err());

        // Existing fixture uses legacy alias `computer = "local"`.
        let legacy = BotProfile::from_toml_str(TOML).unwrap();
        assert_eq!(legacy.runtime.computer, ComputerTarget::Pc);
    }

    #[test]
    fn parses_and_filters_tools() {
        let p = BotProfile::from_toml_str(TOML).unwrap();
        assert_eq!(p.id, "mira");
        assert_eq!(p.models.executor, "deepseek/fast");
        assert!(p.tool_visible("fs.read_file"));
        assert!(p.tool_visible("plan.todo_write"));
        assert!(!p.tool_visible("fs.delete_file"));
        assert!(!p.tool_visible("shell.run"));
        assert!(p.policy().is_some());
    }

    #[test]
    fn empty_allow_means_all_and_no_policy_means_global() {
        let p = BotProfile::from_toml_str(
            "[bot]\nid = \"a\"\nname = \"A\"\n[models]\nexecutor = \"m\"\n",
        )
        .unwrap();
        assert!(p.tool_visible("anything"));
        assert!(p.policy().is_none());
    }

    #[test]
    fn rejects_unknown_preset_and_missing_executor() {
        let bad = TOML.replace("approve-destructive", "yolo");
        assert!(matches!(
            BotProfile::from_toml_str(&bad),
            Err(ProfileError::Invalid(_))
        ));
        assert!(BotProfile::from_toml_str("[bot]\nid = \"a\"\nname = \"A\"\n").is_err());
    }

    #[test]
    fn deserializes_from_json_config() {
        let v = serde_json::json!({
            "bot": {"id": "j", "name": "J"},
            "models": {"executor": "m"},
            "permissions": {"preset": "full"}
        });
        let p: BotProfile = serde_json::from_value(v).unwrap();
        assert!(p.policy().is_some());
    }

    #[test]
    fn rejects_invalid_cli_models_and_cli_planner_or_classifier() {
        let bad_exec = serde_json::json!({
            "bot": {"id": "j", "name": "J"},
            "models": {"executor": "cli:claude/--dangerously-skip-permissions"},
        });
        assert!(serde_json::from_value::<BotProfile>(bad_exec).is_err());

        let bad_planner = serde_json::json!({
            "bot": {"id": "j", "name": "J"},
            "models": {"executor": "cli:claude/default", "planner": "cli:claude/default"},
        });
        assert!(serde_json::from_value::<BotProfile>(bad_planner).is_err());

        let bad_classifier = serde_json::json!({
            "bot": {"id": "j", "name": "J"},
            "models": {"executor": "cli:claude/default", "classifier": "cli:claude/default"},
        });
        assert!(serde_json::from_value::<BotProfile>(bad_classifier).is_err());

        let ok = serde_json::json!({
            "bot": {"id": "j", "name": "J"},
            "models": {"executor": "cli:claude/claude-sonnet-4-5", "planner": "openai/gpt-4o-mini"},
        });
        assert!(serde_json::from_value::<BotProfile>(ok).is_ok());
    }

    fn filter(allow: &[&str], exclude: &[&str]) -> ToolFilter {
        ToolFilter {
            allow: allow.iter().map(|s| (*s).to_owned()).collect(),
            exclude: exclude.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    fn with_tools(allow: &[&str], exclude: &[&str]) -> BotProfile {
        let mut p = BotProfile::from_toml_str(
            "[bot]
id = \"a\"
name = \"A\"
[models]
executor = \"m\"
",
        )
        .unwrap();
        p.allow = globs(&allow.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>()).unwrap();
        p.exclude = globs(&exclude.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>()).unwrap();
        p.tools = filter(allow, exclude);
        p
    }

    #[test]
    fn tool_ceiling_refuses_anything_not_provably_narrower() {
        let caller = with_tools(&["fs_*", "web_search"], &["fs_delete"]);
        // Own pattern, a visible literal, and a narrower literal are fine.
        assert!(
            caller
                .covers_tool_filter(&filter(&["fs_*"], &["fs_delete"]))
                .is_ok()
        );
        assert!(
            caller
                .covers_tool_filter(&filter(&["web_search", "fs_read"], &[]))
                .is_ok()
        );
        // A tool the caller lacks, an excluded one, a wider glob, "everything".
        assert!(
            caller
                .covers_tool_filter(&filter(&["shell_run"], &[]))
                .is_err()
        );
        assert!(
            caller
                .covers_tool_filter(&filter(&["fs_delete"], &[]))
                .is_err()
        );
        assert!(caller.covers_tool_filter(&filter(&["*"], &[])).is_err());
        assert!(
            caller
                .covers_tool_filter(&filter(&["fs_r*"], &["fs_delete"]))
                .is_err()
        );
        assert!(
            caller
                .covers_tool_filter(&filter(&[], &["fs_delete"]))
                .is_err()
        );
        // Dropping the caller's exclusion while using its glob would widen.
        assert!(caller.covers_tool_filter(&filter(&["fs_*"], &[])).is_err());
        // A caller that sees everything may hand out globs, keeping its exclusions.
        let open = with_tools(&[], &["shell_run"]);
        assert!(
            open.covers_tool_filter(&filter(&["fs_*"], &["shell_run"]))
                .is_ok()
        );
        assert!(open.covers_tool_filter(&filter(&["fs_*"], &[])).is_err());
        assert!(
            open.covers_tool_filter(&filter(&["shell_run"], &[]))
                .is_err()
        );
    }
}
