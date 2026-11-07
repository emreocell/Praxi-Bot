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
}
