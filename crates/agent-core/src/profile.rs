//! Bot profile: the subset of ADR-0011's TOML the loop needs.

use std::collections::BTreeMap;
use std::path::Path;

use globset::{Glob, GlobSet, GlobSetBuilder};
use permission_engine::{Policy, Rule};
use protocol::PermissionCategory;
use serde::{Deserialize, Deserializer};

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
    /// Router classifier model; informational in v0.
    pub classifier: Option<String>,
    /// Fallback models; informational in v0.
    #[serde(default)]
    pub fallback: Vec<String>,
}

/// `[permissions]` table: a preset and/or per-category rules.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PermissionsConfig {
    /// `read-only`, `approve-destructive`, `read-and-draft` or `full`.
    pub preset: Option<String>,
    /// Per-category rules layered over the preset.
    #[serde(default)]
    pub categories: BTreeMap<PermissionCategory, Rule>,
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
    allow: GlobSet,
    exclude: GlobSet,
    /// Extra allow-lists a tool must also match (subagent narrowing).
    restrict: Vec<GlobSet>,
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
        Ok(Self {
            allow: globs(&r.tools.allow)?,
            exclude: globs(&r.tools.exclude)?,
            restrict: Vec::new(),
            id: r.bot.id,
            name: r.bot.name,
            persona: r.bot.persona,
            language: r.bot.language,
            models: r.models,
            permissions: r.permissions,
            tools: r.tools,
            router: r.router,
        })
    }
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

    /// Whether `tool_name` passes the allow / exclude lists.
    #[must_use]
    pub fn tool_visible(&self, tool_name: &str) -> bool {
        (self.tools.allow.is_empty() || self.allow.is_match(tool_name))
            && !self.exclude.is_match(tool_name)
            && self.restrict.iter().all(|g| g.is_match(tool_name))
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
}
