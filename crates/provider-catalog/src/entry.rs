//! Provider catalog entry types (deserialised from TOML files).

use serde::{Deserialize, Serialize};

/// How the provider authenticates API calls.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Auth {
    /// Read the API key from the named environment variable.
    ApiKeyEnv(String),
    /// No authentication required (e.g. a local Ollama instance).
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

/// Provider-specific compatibility flags for handling API deviations.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompatFlags {
    /// Rewrite tool call format to match the provider's dialect.
    pub tool_call_format: Option<String>,
    /// Strip unsupported fields before sending the request.
    pub strip_system_role: bool,
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
}
