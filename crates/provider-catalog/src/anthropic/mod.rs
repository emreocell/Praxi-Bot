//! A [`ChatProvider`] for the Anthropic Messages API (`POST /v1/messages`,
//! SSE streaming).
//!
//! - System messages become the top-level `system` field (all system messages,
//!   wherever they appear in the history, are hoisted there).
//! - Tools are converted from the `OpenAI` function format to
//!   `{name, description, input_schema}`.
//! - Prompt caching: `cache_control: {"type":"ephemeral"}` is placed on the
//!   last system block, the last tool and the last content block of the last
//!   message (3 of the 4 allowed breakpoints). The next request of an agent
//!   loop then reads everything up to the previous request's end from cache.
//!   Disable with [`AnthropicProvider::with_prompt_caching`].
//!   [`Usage`](crate::provider::Usage) reports cache reads in `cached_tokens`
//!   and writes in `cache_write_tokens`; `prompt_tokens` is the total prompt
//!   (`input + cache_read + cache_creation`).
//! - Thinking blocks are consumed (never emitted as answer text); extended
//!   thinking is not requested because [`ChatRequest`](crate::provider::ChatRequest)
//!   has no field for it.
//! - Errors: 401/403 -> `Auth`, 400/413/422 -> `InvalidRequest`, 429 ->
//!   `RateLimited` (with `retry-after`), 529 / 5xx / `overloaded_error` ->
//!   `Upstream`. 429 and `Upstream` are retried with backoff before any body is
//!   streamed.
//!
//! Not supported: `json_mode` (the API has no JSON mode; the flag is ignored).
//! Sampling parameters are forwarded as given; newer models reject non-default
//! values with a 400 (`InvalidRequest`).

mod request;
mod stream;
#[cfg(test)]
mod tests;

use async_trait::async_trait;
use futures::stream::BoxStream;
use serde_json::Value;

use crate::{
    entry::{Auth, ProviderEntry},
    error::ProviderError,
    openai_compat::OpenAiCompatConfig,
    provider::{ChatChunk, ChatProvider, ChatRequest},
    util::{
        Classify, Secret, build_client, error_message, event_stream, key_from_env,
        map_reqwest_error, model_name, retry_after, send_with_retry,
    },
};

/// Value of the `anthropic-version` header.
pub const API_VERSION: &str = "2023-06-01";

/// Chat provider for the Anthropic Messages API.
#[derive(Debug, Clone)]
pub struct AnthropicProvider {
    entry: ProviderEntry,
    config: OpenAiCompatConfig,
    client: reqwest::Client,
    key: Option<Secret>,
    caching: bool,
}

impl AnthropicProvider {
    /// Create a provider, reading the key from the environment variable named
    /// by `entry.auth`.
    ///
    /// # Errors
    /// [`ProviderError::Auth`] if the variable is unset or empty.
    pub fn new(entry: ProviderEntry, config: OpenAiCompatConfig) -> Result<Self, ProviderError> {
        let key = match &entry.auth {
            Auth::None => None,
            Auth::ApiKeyEnv(var) => Some(key_from_env(var)?),
        };
        Self::with_api_key(entry, config, key)
    }

    /// Create a provider with an explicit key (or none), ignoring `entry.auth`.
    ///
    /// # Errors
    /// [`ProviderError::Connection`] if the HTTP client cannot be built.
    pub fn with_api_key(
        entry: ProviderEntry,
        config: OpenAiCompatConfig,
        key: Option<String>,
    ) -> Result<Self, ProviderError> {
        Ok(Self {
            client: build_client(&config)?,
            entry,
            config,
            key: key.map(Secret),
            caching: true,
        })
    }

    /// Enable or disable `cache_control` breakpoints (default: enabled).
    #[must_use]
    pub fn with_prompt_caching(mut self, enabled: bool) -> Self {
        self.caching = enabled;
        self
    }

    /// Base URL without a trailing slash or `/v1` suffix.
    fn base(&self) -> &str {
        let b = self.entry.base_url.trim_end_matches('/');
        b.strip_suffix("/v1").unwrap_or(b)
    }

    fn authed(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let rb = rb.header("anthropic-version", API_VERSION);
        match &self.key {
            Some(k) => rb.header("x-api-key", &k.0),
            None => rb,
        }
    }
}

/// Map an Anthropic error `type` to the HTTP status it corresponds to.
pub(crate) fn status_for_error_type(t: &str) -> u16 {
    match t {
        "invalid_request_error" => 400,
        "authentication_error" => 401,
        "permission_error" => 403,
        "not_found_error" => 404,
        "request_too_large" => 413,
        "rate_limit_error" => 429,
        "overloaded_error" => 529,
        _ => 500,
    }
}

/// Classify a failed response. Public to the module tree for stream errors.
pub(crate) fn classify(
    status: u16,
    headers: &reqwest::header::HeaderMap,
    body: &str,
) -> ProviderError {
    let message = error_message(body);
    let overloaded = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            v.pointer("/error/type")
                .and_then(Value::as_str)
                .map(|t| t == "overloaded_error")
        })
        .unwrap_or(false);
    match status {
        401 | 403 => ProviderError::Auth(message),
        400 | 413 | 422 => ProviderError::InvalidRequest(message),
        429 => ProviderError::RateLimited {
            retry_after: retry_after(headers),
        },
        529 => ProviderError::Upstream {
            status: 529,
            message,
        },
        _ if overloaded => ProviderError::Upstream {
            status: 529,
            message,
        },
        s @ 500..=599 => ProviderError::Upstream { status: s, message },
        s => ProviderError::Api { status: s, message },
    }
}

#[async_trait]
impl ChatProvider for AnthropicProvider {
    fn id(&self) -> &str {
        &self.entry.id
    }

    async fn stream(
        &self,
        req: ChatRequest,
    ) -> Result<BoxStream<'static, Result<ChatChunk, ProviderError>>, ProviderError> {
        let body = request::build_body(
            &self.entry,
            model_name(&self.entry.id, &req),
            &req,
            self.caching,
        )?;
        let url = format!("{}/v1/messages", self.base());
        let make = || self.authed(self.client.post(&url).json(&body));
        let resp = send_with_retry(&self.config, &make, classify as Classify).await?;
        Ok(event_stream(resp, stream::Decoder::default()))
    }

    async fn list_models(&self) -> Result<Vec<String>, ProviderError> {
        let url = self
            .entry
            .models_endpoint
            .clone()
            .unwrap_or_else(|| format!("{}/v1/models?limit=1000", self.base()));
        let make = || self.authed(self.client.get(&url));
        let resp = send_with_retry(&self.config, &make, classify as Classify).await?;
        let v: Value = resp.json().await.map_err(map_reqwest_error)?;
        let items = v
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| ProviderError::Stream("model list has no `data` array".into()))?;
        Ok(items
            .iter()
            .filter_map(|m| m.get("id").and_then(Value::as_str).map(str::to_owned))
            .collect())
    }
}
