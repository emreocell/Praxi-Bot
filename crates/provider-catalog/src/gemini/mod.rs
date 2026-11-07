//! A [`ChatProvider`] for the Google Gemini API
//! (`models/{model}:streamGenerateContent?alt=sse`).
//!
//! - System messages become `systemInstruction`.
//! - Tools become one `functionDeclarations` group; JSON schemas are converted
//!   to Gemini's `OpenAPI` subset (see [`schema`] for the keywords stripped).
//! - `functionCall` / `functionResponse` parts map to tool calls / results.
//!   Gemini calls have no usable id, so one is synthesised
//!   (`gm<16 hex>[x<thoughtSignature>]`); the signature that Gemini 3 requires
//!   on replayed calls travels inside the id.
//! - Images are sent as `inlineData` (base64 `data:` URLs only).
//! - Usage: `prompt_tokens` = `promptTokenCount`, `completion_tokens` =
//!   candidates + thinking tokens, `cached_tokens` = `cachedContentTokenCount`.
//!   Implicit caching is automatic server-side; explicit `cachedContents`
//!   resources are not managed here.
//! - Thought summaries (`thought: true` parts, `<thought>` spans) are never
//!   emitted as text.
//! - Abnormal finish reasons are errors, not empty stops: `SAFETY` and friends
//!   -> `ContentFilter`, `MALFORMED_FUNCTION_CALL` -> `MalformedToolCall`
//!   (the request is re-sent once), anything unknown -> `Stream`.
//! - The API key is sent in the `x-goog-api-key` header, not the URL.
//! - Errors: 401/403 and 400 `API_KEY_INVALID` -> `Auth`, other 400 ->
//!   `InvalidRequest`, 429 -> `RateLimited` (from `Retry-After` or the
//!   `RetryInfo` detail), 5xx -> `Upstream`; 429/`Upstream` are retried.

mod request;
mod schema;
mod stream;
#[cfg(test)]
mod tests;

use async_trait::async_trait;
use futures::stream::BoxStream;
use reqwest::header::HeaderMap;
use serde_json::Value;

use crate::{
    entry::{Auth, ProviderEntry},
    error::ProviderError,
    openai_compat::OpenAiCompatConfig,
    provider::{ChatChunk, ChatProvider, ChatRequest},
    util::{
        ChunkStream, Classify, Reopen, Secret, build_client, dump_failed_request, error_message,
        event_stream, key_from_env, map_reqwest_error, model_name, retry_after,
        retry_malformed_once, send_with_retry,
    },
};

/// Chat provider for the Google Gemini API.
#[derive(Debug, Clone)]
pub struct GeminiProvider {
    entry: ProviderEntry,
    config: OpenAiCompatConfig,
    client: reqwest::Client,
    key: Option<Secret>,
}

impl GeminiProvider {
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
        })
    }

    fn base(&self) -> &str {
        self.entry.base_url.trim_end_matches('/')
    }

    /// POST the request and return the decoded event stream.
    async fn open(&self, url: &str, body: &Value) -> Result<ChunkStream, ProviderError> {
        let make = || self.authed(self.client.post(url).json(body));
        match send_with_retry(&self.config, &make, classify as Classify).await {
            Ok(resp) => Ok(event_stream(resp, stream::Decoder::default())),
            Err(e) => {
                dump_failed_request(&self.config, &self.entry.id, body, &e);
                Err(e)
            }
        }
    }

    fn authed(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.key {
            Some(k) => rb.header("x-goog-api-key", &k.0),
            None => rb,
        }
    }
}

/// Classify a failed response (also used for in-stream `error` objects).
pub(crate) fn classify(status: u16, headers: &HeaderMap, body: &str) -> ProviderError {
    let message = error_message(body);
    let parsed: Option<Value> = serde_json::from_str(body).ok();
    // An invalid key is reported as 400 INVALID_ARGUMENT + reason API_KEY_INVALID.
    let bad_key = parsed.as_ref().is_some_and(|v| {
        v.pointer("/error/details")
            .and_then(Value::as_array)
            .is_some_and(|d| {
                d.iter()
                    .any(|e| e.get("reason").and_then(Value::as_str) == Some("API_KEY_INVALID"))
            })
    }) || message.contains("API key not valid");
    match status {
        401 | 403 => ProviderError::Auth(message),
        400 if bad_key => ProviderError::Auth(message),
        400 | 413 | 422 => ProviderError::InvalidRequest(message),
        429 => ProviderError::RateLimited {
            retry_after: retry_after(headers).or_else(|| stream::retry_delay(body)),
        },
        s @ 500..=599 => ProviderError::Upstream { status: s, message },
        s => ProviderError::Api { status: s, message },
    }
}

#[async_trait]
impl ChatProvider for GeminiProvider {
    fn id(&self) -> &str {
        &self.entry.id
    }

    async fn stream(
        &self,
        req: ChatRequest,
    ) -> Result<BoxStream<'static, Result<ChatChunk, ProviderError>>, ProviderError> {
        let model = model_name(&self.entry.id, &req);
        let model = model.strip_prefix("models/").unwrap_or(model);
        let body = request::build_body(&self.entry, model, &req)?;
        let url = format!(
            "{}/models/{model}:streamGenerateContent?alt=sse",
            self.base()
        );
        let first = self.open(&url, &body).await?;
        let this = self.clone();
        let reopen: Reopen = Box::new(move || {
            let (this, url, body) = (this.clone(), url.clone(), body.clone());
            Box::pin(async move { this.open(&url, &body).await })
        });
        Ok(retry_malformed_once(first, reopen))
    }

    async fn list_models(&self) -> Result<Vec<String>, ProviderError> {
        let url = self
            .entry
            .models_endpoint
            .clone()
            .unwrap_or_else(|| format!("{}/models?pageSize=1000", self.base()));
        let make = || self.authed(self.client.get(&url));
        let resp = send_with_retry(&self.config, &make, classify as Classify).await?;
        let v: Value = resp.json().await.map_err(map_reqwest_error)?;
        let items = v
            .get("models")
            .and_then(Value::as_array)
            .ok_or_else(|| ProviderError::Stream("model list has no `models` array".into()))?;
        Ok(items
            .iter()
            .filter(|m| {
                m.get("supportedGenerationMethods")
                    .and_then(Value::as_array)
                    .is_none_or(|ms| ms.iter().any(|x| x.as_str() == Some("generateContent")))
            })
            .filter_map(|m| m.get("name").and_then(Value::as_str))
            .map(|n| n.strip_prefix("models/").unwrap_or(n).to_owned())
            .collect())
    }
}
