//! Provider catalog entry types (deserialised from TOML files).

use serde::{Deserialize, Serialize};

/// How the provider authenticates API calls.
///
/// Serialised in `snake_case`; the `PascalCase` spellings used by the catalog TOML
/// files (`ApiKeyEnv`, `None`) are accepted as aliases.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Auth {
    /// Read the API key from the named environment variable.
    #[serde(alias = "ApiKeyEnv")]
    ApiKeyEnv(String),
    /// No authentication required (e.g. a local Ollama instance).
    #[serde(alias = "None")]
    None,
}

/// What the provider's model(s) support.
// Four independent binary capability flags; a bitset would obscure semantics.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelCapabilities {
    /// Tool / function calling.
    pub tools: bool,
    /// Vision / image input.
    pub vision: bool,
    /// Streaming (chunked) responses.
    pub streaming: bool,
    /// Requesting JSON output mode.
    pub json_mode: bool,
}

/// Per-model overrides of [`ModelCapabilities`].
///
/// Each field is optional: `None` keeps the value underneath (the provider
/// flags, or a catalog override when a user value is merged on top). No
/// provenance or date is recorded. Unknown models keep the provider flags
/// (`tools` stays `true` unless an override says otherwise).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CapabilityOverrides {
    /// Tool / function calling.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<bool>,
    /// Vision / image input.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vision: Option<bool>,
    /// Streaming (chunked) responses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub streaming: Option<bool>,
    /// Requesting JSON output mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json_mode: Option<bool>,
}

impl CapabilityOverrides {
    /// Apply the set fields onto `base`. Unset fields leave `base` unchanged.
    #[must_use]
    pub fn apply(&self, mut base: ModelCapabilities) -> ModelCapabilities {
        if let Some(v) = self.tools {
            base.tools = v;
        }
        if let Some(v) = self.vision {
            base.vision = v;
        }
        if let Some(v) = self.streaming {
            base.streaming = v;
        }
        if let Some(v) = self.json_mode {
            base.json_mode = v;
        }
        base
    }

    /// Field-wise merge. `self` wins wherever it is `Some`.
    #[must_use]
    pub fn merge_over(&self, catalog: &Self) -> Self {
        Self {
            tools: self.tools.or(catalog.tools),
            vision: self.vision.or(catalog.vision),
            streaming: self.streaming.or(catalog.streaming),
            json_mode: self.json_mode.or(catalog.json_mode),
        }
    }
}

/// Provider-specific compatibility flags for handling API deviations.
///
/// All fields default to "standard `OpenAI` behaviour", so a TOML table may set
/// only the ones it needs. Flags consumed by the OpenAI-compatible provider:
///
/// - `strip_system_role`: the endpoint rejects the `system` role. All system
///   messages are merged (text only) into the start of the first user message
///   (a user message is created if there is none).
/// - `system_as_user`: like `strip_system_role` but in place: each `system`
///   message is sent with role `user`, preserving position. Ignored when
///   `strip_system_role` is set.
/// - `no_parallel_tool_calls`: send `"parallel_tool_calls": false` whenever
///   tools are present (for endpoints that mishandle several calls per turn).
/// - `tools_require_strict`: mark every tool `"strict": true` and default the
///   top-level parameter schema to `"additionalProperties": false`.
/// - `max_tokens_field`: name of the JSON field carrying the token limit;
///   default `"max_tokens"`, newer `OpenAI` models want `"max_completion_tokens"`.
/// - `no_stream_usage`: do not send `stream_options.include_usage` (for
///   endpoints that reject unknown `stream_options`; usage is then only
///   reported if the server volunteers it).
/// - `tool_call_format`: reserved for dialect rewriting; currently ignored.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[allow(clippy::struct_excessive_bools)]
pub struct CompatFlags {
    pub tool_call_format: Option<String>,
    pub strip_system_role: bool,
    pub system_as_user: bool,
    pub no_parallel_tool_calls: bool,
    pub tools_require_strict: bool,
    pub max_tokens_field: Option<String>,
    pub no_stream_usage: bool,
}

/// The wire protocol / API family the provider speaks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// OpenAI-compatible chat completions endpoint.
    OpenAiCompatible,
    /// Anthropic Messages API.
    Anthropic,
    /// Google Gemini API.
    Gemini,
    /// Any other provider.
    Other(String),
}

/// A single provider catalog entry (one `.toml` file in `providers/catalog/`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderEntry {
    /// Unique identifier (e.g. `"openrouter"`).
    pub id: String,
    /// API family.
    pub kind: ProviderKind,
    /// Base URL for API calls.
    pub base_url: String,
    /// Authentication method.
    pub auth: Auth,
    /// Capability flags for the default model(s) of this provider.
    pub capabilities: ModelCapabilities,
    /// Provider-specific compatibility overrides.
    #[serde(default)]
    pub compat_flags: CompatFlags,
    /// URL to list available models (optional).
    pub models_endpoint: Option<String>,
    /// Known models with limits and prices (optional; may be empty).
    #[serde(default)]
    pub models: Vec<ModelInfo>,
}

impl ProviderEntry {
    /// Catalog data for a model id (without the provider prefix).
    #[must_use]
    pub fn model(&self, id: &str) -> Option<&ModelInfo> {
        self.models.iter().find(|m| m.id == id)
    }

    /// Capabilities for `model`: the provider flags, with that model's catalog
    /// overrides applied. A model with no override (or an unknown id) keeps
    /// the provider flags.
    #[must_use]
    pub fn caps_for(&self, model: &str) -> ModelCapabilities {
        match self.model(model).and_then(|m| m.capabilities.as_ref()) {
            Some(over) => over.apply(self.capabilities.clone()),
            None => self.capabilities.clone(),
        }
    }
}

/// A model known to the catalog, with limits and USD prices per million tokens.
///
/// Every field except `id` is optional: unknown values are left out rather than
/// guessed. `prices_as_of` (ISO date) says when the prices were last checked.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelInfo {
    /// Model id as sent to the API.
    pub id: String,
    /// Context window in tokens.
    pub context_window: Option<u32>,
    /// Maximum output tokens.
    pub max_output_tokens: Option<u32>,
    /// USD per million uncached input tokens.
    pub input_per_mtok: Option<f64>,
    /// USD per million output tokens (including thinking tokens).
    pub output_per_mtok: Option<f64>,
    /// USD per million cache-read tokens.
    pub cached_per_mtok: Option<f64>,
    /// USD per million cache-write tokens (Anthropic 5-minute TTL).
    pub cache_write_per_mtok: Option<f64>,
    /// ISO date (`YYYY-MM-DD`) the prices were checked.
    pub prices_as_of: Option<String>,
    /// Per-model capability overrides. Absent fields keep the provider flags.
    /// No provenance or date is stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<CapabilityOverrides>,
}

impl ModelInfo {
    /// Cost of one completion in USD, or `None` when input or output prices
    /// are unknown. Cache reads/writes without their own price are charged at
    /// the plain input price (an upper bound).
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn cost_usd(&self, usage: &crate::provider::Usage) -> Option<f64> {
        let input = self.input_per_mtok?;
        let output = self.output_per_mtok?;
        let read = usage.cached_tokens.unwrap_or(0);
        let write = usage.cache_write_tokens.unwrap_or(0);
        let plain = usage.prompt_tokens.saturating_sub(read + write);
        let total = plain as f64 * input
            + read as f64 * self.cached_per_mtok.unwrap_or(input)
            + write as f64 * self.cache_write_per_mtok.unwrap_or(input)
            + usage.completion_tokens as f64 * output;
        Some(total / 1_000_000.0)
    }
}
