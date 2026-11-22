//! Picker metadata, deliberately separate from execution capabilities and prices.
//!
//! The curated table only annotates ids supplied by the live listing. It never
//! installs models, changes a bot's configured model, or becomes an allowlist.

use std::{collections::BTreeSet, sync::OnceLock};

use serde::{Deserialize, Serialize};

use crate::catalog_freshness::IsoDate;

/// Primary use of a model. Reasoning and vision-capable text models are chat
/// candidates; image/audio/video *generation* models are not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelKind {
    Chat,
    Reasoning,
    VisionCapable,
    Embedding,
    Image,
    Audio,
    Video,
    Other,
}

impl ModelKind {
    #[must_use]
    pub const fn is_chat(self) -> bool {
        matches!(self, Self::Chat | Self::Reasoning | Self::VisionCapable)
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Reasoning => "reasoning",
            Self::VisionCapable => "vision_capable",
            Self::Embedding => "embedding",
            Self::Image => "image",
            Self::Audio => "audio",
            Self::Video => "video",
            Self::Other => "other",
        }
    }
}

/// Secret-free presentation metadata. The id remains the exact wire id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelMetadata {
    pub id: String,
    pub display_name: String,
    pub kind: ModelKind,
    #[serde(default)]
    pub recommended: bool,
    #[serde(default)]
    pub recommended_rank: Option<u8>,
    #[serde(default)]
    pub deprecated: bool,
    #[serde(default)]
    pub note: Option<String>,
}

/// Curated presentation metadata for one provider (plus its aliases and sources).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderMetadata {
    pub id: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Optional per-provider review date (`YYYY-MM-DD`); defaults to the
    /// table-level `last_reviewed` date when omitted.
    #[serde(
        default,
        alias = "verified_at",
        skip_serializing_if = "Option::is_none"
    )]
    pub last_reviewed: Option<String>,
    pub sources: Vec<String>,
    pub models: Vec<ModelMetadata>,
}

/// The checked-in curated model metadata table (`model_metadata.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CuratedModels {
    /// ISO date (`YYYY-MM-DD`) when the table was last reviewed against
    /// official provider listings.
    #[serde(alias = "verified_at")]
    pub last_reviewed: String,
    pub providers: Vec<ProviderMetadata>,
}

impl CuratedModels {
    /// Parse a curated model metadata table from JSON.
    ///
    /// # Errors
    /// Returns a human-readable error if the JSON does not match the schema.
    pub fn from_json(source: &str) -> Result<Self, String> {
        serde_json::from_str(source).map_err(|e| format!("invalid model metadata JSON: {e}"))
    }

    /// Resolve a provider id or alias to its canonical entry.
    #[must_use]
    pub fn find_provider(&self, id_or_alias: &str) -> Option<&ProviderMetadata> {
        self.providers
            .iter()
            .find(|p| p.id == id_or_alias || p.aliases.iter().any(|a| a == id_or_alias))
    }

    /// Effective review date string (`YYYY-MM-DD`) for `provider`.
    #[must_use]
    pub fn effective_last_reviewed<'a>(&'a self, provider: &'a ProviderMetadata) -> &'a str {
        provider
            .last_reviewed
            .as_deref()
            .unwrap_or(&self.last_reviewed)
    }

    /// Collect every schema/semantic violation in this curated table.
    #[must_use]
    pub fn validation_errors(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if self.last_reviewed.trim().is_empty() {
            errors.push("last_reviewed date is required".into());
        } else if let Err(err) = IsoDate::parse(&self.last_reviewed) {
            errors.push(format!(
                "invalid last_reviewed date `{}`: {err}",
                self.last_reviewed
            ));
        }
        if self.providers.is_empty() {
            errors.push("at least one provider entry is required".into());
        }

        let mut providers = BTreeSet::new();
        for provider in &self.providers {
            validate_provider_metadata(provider, &mut providers, &mut errors);
        }
        errors
    }

    /// Validate this curated table, returning the first violation if any.
    ///
    /// # Errors
    /// Returns the first validation error message.
    pub fn validate(&self) -> Result<(), String> {
        match self.validation_errors().into_iter().next() {
            None => Ok(()),
            Some(err) => Err(err),
        }
    }

    fn describe(&self, provider: &str, id: &str) -> ModelMetadata {
        self.find_provider(provider)
            .and_then(|p| p.models.iter().find(|m| m.id == id))
            .cloned()
            .unwrap_or_else(|| inferred_metadata(id))
    }
}

fn validate_provider_metadata<'a>(
    provider: &'a ProviderMetadata,
    providers: &mut BTreeSet<&'a str>,
    errors: &mut Vec<String>,
) {
    if !safe_model_id(&provider.id) {
        errors.push(format!("invalid provider id `{}`", provider.id));
    } else if !providers.insert(provider.id.as_str()) {
        errors.push(format!("duplicate provider id `{}`", provider.id));
    }
    for alias in &provider.aliases {
        if alias == &provider.id {
            errors.push(format!(
                "provider `{}` lists itself as an alias",
                provider.id
            ));
        } else if !safe_model_id(alias) {
            errors.push(format!(
                "provider `{}` has invalid alias `{alias}`",
                provider.id
            ));
        } else if !providers.insert(alias.as_str()) {
            errors.push(format!(
                "duplicate provider id or alias `{alias}` on provider `{}`",
                provider.id
            ));
        }
    }
    if let Some(date) = &provider.last_reviewed
        && let Err(err) = IsoDate::parse(date)
    {
        errors.push(format!(
            "provider `{}` has invalid last_reviewed date `{date}`: {err}",
            provider.id
        ));
    }
    if provider.sources.is_empty() || provider.sources.iter().any(|s| !s.starts_with("https://")) {
        errors.push(format!(
            "provider `{}` requires at least one official https:// source URL",
            provider.id
        ));
    }

    let mut ids = BTreeSet::new();
    let mut display_names = BTreeSet::new();
    let mut ranks = BTreeSet::new();
    for model in &provider.models {
        validate_model_metadata(
            &provider.id,
            model,
            &mut ids,
            &mut display_names,
            &mut ranks,
            errors,
        );
    }
}

fn validate_model_metadata<'a>(
    provider_id: &str,
    model: &'a ModelMetadata,
    ids: &mut BTreeSet<&'a str>,
    display_names: &mut BTreeSet<String>,
    ranks: &mut BTreeSet<u8>,
    errors: &mut Vec<String>,
) {
    if !safe_model_id(&model.id) {
        errors.push(format!(
            "provider `{provider_id}` has invalid model id `{}`",
            model.id
        ));
    } else if !ids.insert(model.id.as_str()) {
        errors.push(format!(
            "provider `{provider_id}` has duplicate model id `{}`",
            model.id
        ));
    }

    let trimmed_name = model.display_name.trim();
    if trimmed_name.is_empty() || model.display_name.chars().any(char::is_control) {
        errors.push(format!(
            "provider `{provider_id}` model `{}` requires a printable display_name",
            model.id
        ));
    } else if !display_names.insert(trimmed_name.to_ascii_lowercase()) {
        errors.push(format!(
            "provider `{provider_id}` has duplicate display_name `{}`",
            model.display_name
        ));
    }

    if let Some(note) = &model.note {
        if note.trim().is_empty() || note.chars().any(char::is_control) {
            errors.push(format!(
                "provider `{provider_id}` model `{}` has blank or control-character note",
                model.id
            ));
        }
        if note == "free" {
            errors.push(format!(
                "provider `{provider_id}` model `{}`: free notes must originate from the live list",
                model.id
            ));
        }
        let lower_note = note.to_ascii_lowercase();
        if !model.deprecated
            && (lower_note.contains("retired") || lower_note.contains("deprecated"))
        {
            errors.push(format!(
                "provider `{provider_id}` model `{}` has note `{note}` but deprecated is false",
                model.id
            ));
        }
    } else if model.deprecated {
        errors.push(format!(
            "provider `{provider_id}` deprecated model `{}` must include a note (e.g. legacy or retired)",
            model.id
        ));
    }

    if model.recommended {
        if model.kind != ModelKind::Chat || model.deprecated {
            errors.push(format!(
                "provider `{provider_id}` model `{}`: only current chat models can be recommended",
                model.id
            ));
        }
        match model.recommended_rank {
            Some(rank @ 1..=4) => {
                if !ranks.insert(rank) {
                    errors.push(format!(
                        "provider `{provider_id}` has duplicate recommendation rank {rank}"
                    ));
                }
            }
            _ => errors.push(format!(
                "provider `{provider_id}` model `{}`: recommendation rank must be 1..4",
                model.id
            )),
        }
        if model.id.ends_with(":free") {
            errors.push(format!(
                "provider `{provider_id}` model `{}`: unstable :free model cannot be recommended",
                model.id
            ));
        }
    } else if model.recommended_rank.is_some() {
        errors.push(format!(
            "provider `{provider_id}` model `{}`: only recommendations carry a rank",
            model.id
        ));
    }
}

/// Access the embedded, validated curated model metadata table.
///
/// # Panics
/// Panics only if the compiled-in `model_metadata.json` fails to parse or
/// validate (guarded by unit tests).
#[must_use]
pub fn embedded_metadata() -> &'static CuratedModels {
    static CURATED: OnceLock<CuratedModels> = OnceLock::new();
    CURATED.get_or_init(|| {
        let table = CuratedModels::from_json(include_str!("model_metadata.json"))
            .expect("checked-in model metadata must parse");
        table
            .validate()
            .expect("checked-in model metadata must validate");
        table
    })
}

/// Annotate *only* live ids, deduplicate, filter and order recommendations
/// before other models. This is a presentation helper, never an execution gate.
/// `chat_only = false` also includes deprecated and non-chat models.
#[must_use]
pub fn model_metadata(provider: &str, ids: &[String], chat_only: bool) -> Vec<ModelMetadata> {
    let mut seen = BTreeSet::new();
    let mut models: Vec<_> = ids
        .iter()
        .filter(|id| safe_model_id(id) && seen.insert(id.as_str()))
        .map(|id| {
            let mut model = embedded_metadata().describe(provider, id);
            if provider == "openrouter" && id.ends_with(":free") {
                // This id came from the provider's current list, not the table.
                model.note = Some("free".into());
            }
            model
        })
        .filter(|m| !chat_only || (m.kind.is_chat() && !m.deprecated))
        .collect();
    models.sort_by(|a, b| {
        a.recommended_rank
            .unwrap_or(u8::MAX)
            .cmp(&b.recommended_rank.unwrap_or(u8::MAX))
            .then_with(|| a.display_name.cmp(&b.display_name))
            .then_with(|| a.id.cmp(&b.id))
    });
    models
}

pub(crate) fn safe_model_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && id.bytes().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b'/' | b':' | b'@' | b'+')
        })
}

pub(crate) fn inferred_metadata(id: &str) -> ModelMetadata {
    let lower = id.to_ascii_lowercase();
    ModelMetadata {
        id: id.into(),
        display_name: humanize(id),
        kind: infer_kind(&lower),
        recommended: false,
        recommended_rank: None,
        deprecated: false,
        note: lower.contains("preview").then(|| "preview".into()),
    }
}

fn infer_kind(id: &str) -> ModelKind {
    let groups: &[(ModelKind, &[&str])] = &[
        (
            ModelKind::Embedding,
            &["embed", "bge-", "e5-", "nomic-", "snowflake-arctic-embed"],
        ),
        (
            ModelKind::Video,
            &["video", "veo", "sora", "seedance", "gemini-omni"],
        ),
        (
            ModelKind::Audio,
            &[
                "tts",
                "audio",
                "music",
                "realtime",
                "real-time",
                "whisper",
                "transcri",
                "speech",
                "lyria",
                "-live",
            ],
        ),
        (
            ModelKind::Image,
            &[
                "image",
                "imagen",
                "dall-e",
                "dalle",
                "flux",
                "stable-diffusion",
                "nano-banana",
            ],
        ),
        (
            ModelKind::Other,
            &[
                "deep-research",
                "deep_research",
                "moderation",
                "rerank",
                "computer-use",
                "computer_use",
                "robotics",
                "aqa",
            ],
        ),
        (ModelKind::Reasoning, &["reasoning", "reasoner"]),
        (ModelKind::VisionCapable, &["vision", "-vl"]),
    ];
    groups
        .iter()
        .find(|(_, patterns)| patterns.iter().any(|p| id.contains(p)))
        .map_or(ModelKind::Chat, |(kind, _)| *kind)
}

fn humanize(id: &str) -> String {
    id.split(['-', '_', '/', ':'])
        .filter(|s| !s.is_empty())
        .map(|word| {
            if matches!(word, "gpt" | "tts" | "api" | "vl") {
                word.to_ascii_uppercase()
            } else if word == "openai" {
                "OpenAI".into()
            } else {
                let mut chars = word.chars();
                chars.next().map_or_else(String::new, |first| {
                    first.to_uppercase().chain(chars).collect()
                })
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests;
