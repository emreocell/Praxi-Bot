//! A [`ChatProvider`] for any OpenAI-compatible `/chat/completions` endpoint
//! (`OpenRouter`, `DeepSeek`, Ollama, LM Studio, Gemini's compat endpoint, ...).
//!
//! Behaviour is driven by the [`ProviderEntry`]: capabilities decide streaming,
//! vision, tools and JSON mode; [`CompatFlags`](crate::entry::CompatFlags)
//! (documented on the type) adapt the wire format to endpoint quirks.
//!
//! Direct callers can retry transient HTTP failures before the response streams.
//! Ledger-aware callers use one request per attempt.

mod embedding;
mod request;
pub(crate) mod sse;
#[cfg(test)]
mod tests;

use std::time::Duration;

use async_trait::async_trait;
use futures::{StreamExt, stream};
use serde_json::Value;

use reqwest::header::HeaderMap;

use crate::util::{
    EventDecoder, Secret, build_client, classify_response, dump_failed_request, event_stream,
    retry_after, send_once, send_with_retry,
};
use crate::{
    entry::{Auth, ProviderEntry},
    error::ProviderError,
    limits::{response_json, response_text},
    provider::{CHAT_STREAM_IDLE, ChatProvider, ChatRequest, ChatStream},
};
use sse::{Assembler, Assembly};

/// Tunables for [`OpenAiCompatProvider`].
#[derive(Debug, Clone)]
pub struct OpenAiCompatConfig {
    /// Retries after the first attempt for 429/5xx responses (0 disables).
    pub max_retries: u32,
    /// Backoff before the first retry; doubles each attempt.
    pub base_delay: Duration,
    /// Upper bound for any single backoff. A `Retry-After` longer than this is
    /// not truncated: the request fails immediately instead.
    pub max_delay: Duration,
    /// Upper bound for the total time spent retrying one request: no retry is
    /// started when its backoff would push the elapsed time past this.
    pub max_total_retry: Duration,
    /// When set, the redacted body of a request that failed with HTTP 400 is
    /// written to `<debug_dir>/last-failed-request-<provider>.json`
    /// (conventionally `<data_dir>/debug`). Off by default.
    pub debug_dir: Option<std::path::PathBuf>,
    /// TCP/TLS connect timeout.
    pub connect_timeout: Duration,
    /// Idle timeout: maximum wait for response headers and between body
    /// chunks. Long streams are not cut off as long as data keeps flowing.
    pub timeout: Duration,
}

impl Default for OpenAiCompatConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(30),
            max_total_retry: Duration::from_secs(60),
            debug_dir: None,
            connect_timeout: Duration::from_secs(10),
            timeout: CHAT_STREAM_IDLE,
        }
    }
}

/// Chat provider for OpenAI-compatible endpoints.
#[derive(Debug, Clone)]
pub struct OpenAiCompatProvider {
    entry: ProviderEntry,
    config: OpenAiCompatConfig,
    client: reqwest::Client,
    key: Option<Secret>,
}

impl OpenAiCompatProvider {
    /// Create a provider, reading the API key from the environment variable
    /// named by `entry.auth`.
    ///
    /// # Errors
    ///
    /// [`ProviderError::Auth`] if the variable is unset or empty;
    /// [`ProviderError::Connection`] if the HTTP client cannot be built.
    pub fn new(entry: ProviderEntry, config: OpenAiCompatConfig) -> Result<Self, ProviderError> {
        let key = match &entry.auth {
            Auth::None => None,
            Auth::ApiKeyEnv(var) => match std::env::var(var) {
                Ok(k) if !k.trim().is_empty() => Some(k),
                _ => {
                    return Err(ProviderError::Auth(format!(
                        "environment variable {var} is not set"
                    )));
                }
            },
        };
        Self::with_api_key(entry, config, key)
    }

    /// Create a provider with an explicit key (or none), ignoring `entry.auth`.
    ///
    /// # Errors
    ///
    /// [`ProviderError::Connection`] if the HTTP client cannot be built.
    pub fn with_api_key(
        entry: ProviderEntry,
        config: OpenAiCompatConfig,
        key: Option<String>,
    ) -> Result<Self, ProviderError> {
        let client = build_client(&config)?;
        Ok(Self {
            entry,
            config,
            client,
            key: key.map(Secret),
        })
    }

    fn model_name<'a>(&self, req: &'a ChatRequest) -> &'a str {
        if req.model.provider() == Some(self.entry.id.as_str()) {
            req.model.model()
        } else {
            &req.model.0
        }
    }

    async fn send(
        &self,
        url: &str,
        body: Option<&Value>,
        retry: bool,
    ) -> Result<reqwest::Response, ProviderError> {
        let make = || {
            let mut rb = match body {
                Some(b) => self.client.post(url).json(b),
                None => self.client.get(url),
            };
            if let Some(k) = &self.key {
                rb = rb.bearer_auth(&k.0);
            }
            rb
        };
        let result = if retry {
            send_with_retry(&self.config, &make, classify).await
        } else {
            send_once(make(), classify).await
        };
        if let (Err(e), Some(b)) = (&result, body) {
            dump_failed_request(&self.config, &self.entry.id, b, e);
        }
        result
    }

    async fn open(&self, req: ChatRequest, retry: bool) -> Result<ChatStream, ProviderError> {
        let model = self.model_name(&req);
        let streaming = self.entry.caps_for(model).streaming;
        let body = request::build_body(&self.entry, model, &req, streaming)?;
        let url = format!(
            "{}/chat/completions",
            self.entry.base_url.trim_end_matches('/')
        );
        let resp = self.send(&url, Some(&body), retry).await?;

        if !streaming {
            let text = response_text(resp).await?;
            let mut asm = Assembler::default();
            let mut chunks = match asm.feed(&text)? {
                Assembly::Chunks(chunks) => chunks,
                Assembly::Done => Vec::new(),
            };
            asm.finished = true;
            match asm.finish() {
                Ok(final_chunks) => chunks.extend(final_chunks),
                Err(error) => {
                    let Some(usage) = asm.take_usage() else {
                        return Err(error);
                    };
                    return Ok(
                        stream::iter([Ok(crate::ChatChunk::Usage(usage)), Err(error)]).boxed(),
                    );
                }
            }
            return Ok(stream::iter(chunks.into_iter().map(Ok)).boxed());
        }

        Ok(event_stream(
            resp,
            Assembler::default(),
            self.config.timeout,
        ))
    }
}

fn classify(status: u16, headers: &HeaderMap, body: &str) -> ProviderError {
    match classify_response(status, retry_after(headers), body) {
        // Ollama and LM Studio refuse tool calls with HTTP 400.
        ProviderError::Api {
            status: 400,
            message,
        } if message.contains("does not support tools") => ProviderError::Unsupported {
            capability: "tools".into(),
        },
        error => error,
    }
}

#[async_trait]
impl ChatProvider for OpenAiCompatProvider {
    fn id(&self) -> &str {
        &self.entry.id
    }

    async fn stream(&self, req: ChatRequest) -> Result<ChatStream, ProviderError> {
        self.open(req, true).await
    }

    async fn stream_once(&self, req: ChatRequest) -> Result<ChatStream, ProviderError> {
        self.open(req, false).await
    }

    async fn list_models(&self) -> Result<Vec<String>, ProviderError> {
        let Some(url) = &self.entry.models_endpoint else {
            return Ok(Vec::new());
        };
        let resp = self.send(url, None, true).await?;
        let v = response_json(resp).await?;
        let items = v
            .get("data")
            .or_else(|| v.get("models"))
            .and_then(Value::as_array)
            .ok_or_else(|| ProviderError::Stream("model list has no `data` array".into()))?;
        Ok(items
            .iter()
            .filter_map(|m| m.get("id").and_then(Value::as_str).map(str::to_owned))
            .collect())
    }
}
