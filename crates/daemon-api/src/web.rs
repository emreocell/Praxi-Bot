//! Web search settings management (`/v1/web/search*`): DTOs and the
//! [`WebControl`] trait the embedder implements (`apps/cli/src/web_host.rs`).
//!
//! API keys are **write-only**: they enter through [`WebSearchConfigInput`], go
//! straight into the secret store, and no response or log line ever carries one
//! (responses only report `brave_key_set` / `tavily_key_set`).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Current web search configuration (`GET /v1/web/search`): non-secret fields
/// only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[allow(clippy::struct_excessive_bools)]
pub struct WebSearchConfigDto {
    /// Priority order of backend ids (`searxng`, `brave`, `tavily`,
    /// `duckduckgo`, `duckduckgo_lite`, `bing`).
    pub order: Vec<String>,
    /// Effective ordered list of enabled and usable backends that `web_search`
    /// will try.
    pub active_backends: Vec<String>,
    /// Configured `SearXNG` base URL; `null` if unset.
    pub searxng_url: Option<String>,
    /// Whether a Brave Search API key is stored (never returned).
    pub brave_key_set: bool,
    /// Whether a Tavily Search API key is stored (never returned).
    pub tavily_key_set: bool,
    /// Whether the `DuckDuckGo` HTML fallback is enabled.
    pub duckduckgo: bool,
    /// Whether the `DuckDuckGo` Lite fallback is enabled.
    pub duckduckgo_lite: bool,
    /// Whether the Bing HTML fallback is enabled.
    pub bing: bool,
}

/// Body of `PUT /v1/web/search`. Every field is optional: omitted fields keep
/// their stored values.
#[derive(Clone, Default, Deserialize, ToSchema)]
pub struct WebSearchConfigInput {
    /// Desired priority order of backend ids.
    #[serde(default)]
    pub order: Option<Vec<String>>,
    /// `SearXNG` base URL; empty string clears it.
    #[serde(default)]
    pub searxng_url: Option<String>,
    /// Brave Search API key (**write-only**: stored in secret store, never
    /// returned or logged).
    #[serde(default)]
    pub brave_key: Option<String>,
    /// Remove the stored Brave Search API key.
    #[serde(default)]
    pub clear_brave_key: Option<bool>,
    /// Tavily Search API key (**write-only**: stored in secret store, never
    /// returned or logged).
    #[serde(default)]
    pub tavily_key: Option<String>,
    /// Remove the stored Tavily Search API key.
    #[serde(default)]
    pub clear_tavily_key: Option<bool>,
    /// Enable or disable the `DuckDuckGo` HTML fallback.
    #[serde(default)]
    pub duckduckgo: Option<bool>,
    /// Enable or disable the `DuckDuckGo` Lite fallback.
    #[serde(default)]
    pub duckduckgo_lite: Option<bool>,
    /// Enable or disable the Bing HTML fallback.
    #[serde(default)]
    pub bing: Option<bool>,
}

impl std::fmt::Debug for WebSearchConfigInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebSearchConfigInput")
            .field("order", &self.order)
            .field("searxng_url", &self.searxng_url)
            .field("brave_key", &self.brave_key.as_ref().map(|_| "<redacted>"))
            .field("clear_brave_key", &self.clear_brave_key)
            .field(
                "tavily_key",
                &self.tavily_key.as_ref().map(|_| "<redacted>"),
            )
            .field("clear_tavily_key", &self.clear_tavily_key)
            .field("duckduckgo", &self.duckduckgo)
            .field("duckduckgo_lite", &self.duckduckgo_lite)
            .field("bing", &self.bing)
            .finish()
    }
}

/// Body of `POST /v1/web/search/test`.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct WebSearchTestInput {
    /// Sample query to run; defaults to `"rust programming"`.
    #[serde(default)]
    pub query: Option<String>,
    /// Optional backend id to probe directly; omit to test the active fallback
    /// chain.
    #[serde(default)]
    pub backend: Option<String>,
}

/// Result of a live web search probe (`POST /v1/web/search/test`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WebSearchTestReport {
    /// `true` when at least one backend answered with results.
    pub ok: bool,
    /// Query that was run.
    pub query: String,
    /// Backend that answered (`null` when all failed).
    pub backend: Option<String>,
    /// Number of search results returned.
    pub result_count: usize,
    /// Elapsed wall-clock time in milliseconds.
    pub elapsed_ms: u64,
    /// Error summary when the probe failed (`null` on success).
    pub error: Option<String>,
    /// Errors from earlier backends in the chain before the answering one.
    #[serde(default)]
    pub fallback_errors: Vec<String>,
}

/// Why a [`WebControl`] operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebControlError {
    /// Invalid configuration (`422`).
    Invalid(String),
    /// Internal / persistence / secret-store failure (`500`).
    Internal(String),
}

/// What `/v1/web/search*` needs from the embedder: persisting `<data dir>/web.toml`,
/// writing keys to the secret store, refreshing the registered `web_*` tools,
/// and running a live search probe.
#[async_trait]
pub trait WebControl: Send + Sync {
    /// Current configuration (non-secret fields only).
    fn get(&self) -> WebSearchConfigDto;

    /// Validate, persist (`web.toml` + secret store), and hot-reload the web
    /// tools for the next turn.
    ///
    /// # Errors
    /// [`WebControlError`].
    async fn update(
        &self,
        input: WebSearchConfigInput,
    ) -> Result<WebSearchConfigDto, WebControlError>;

    /// Run a live search probe against the current configuration.
    ///
    /// # Errors
    /// [`WebControlError::Invalid`] if the query or backend name is invalid.
    async fn test(&self, input: WebSearchTestInput)
    -> Result<WebSearchTestReport, WebControlError>;
}
