//! A [`ChatProvider`] for any OpenAI-compatible `/chat/completions` endpoint
//! (`OpenRouter`, `DeepSeek`, Ollama, LM Studio, Gemini's compat endpoint, ...).
//!
//! Behaviour is driven by the [`ProviderEntry`]: capabilities decide streaming,
//! vision, tools and JSON mode; [`CompatFlags`](crate::entry::CompatFlags)
//! (documented on the type) adapt the wire format to endpoint quirks.
//!
//! Retries: HTTP 429 and 5xx are retried with jittered exponential backoff
//! (honouring `Retry-After`, capped at `max_delay`) before any body is
//! streamed; timeouts and other errors are not retried.

mod request;
pub(crate) mod sse;
#[cfg(test)]
mod tests;

use std::{collections::VecDeque, time::Duration};

use async_trait::async_trait;
use futures::{StreamExt, stream, stream::BoxStream};
use serde_json::Value;

use crate::util::{Secret, map_reqwest_error};
use crate::{
    entry::{Auth, ProviderEntry},
    error::ProviderError,
    provider::{ChatChunk, ChatProvider, ChatRequest},
};
use sse::{Assembler, SseDecoder};

/// Tunables for [`OpenAiCompatProvider`].
#[derive(Debug, Clone)]
pub struct OpenAiCompatConfig {
    /// Retries after the first attempt for 429/5xx responses (0 disables).
    pub max_retries: u32,
    /// Backoff before the first retry; doubles each attempt.
    pub base_delay: Duration,
    /// Upper bound for any single backoff (also caps `Retry-After`).
    pub max_delay: Duration,
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
            connect_timeout: Duration::from_secs(10),
            timeout: Duration::from_secs(120),
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
        let client = reqwest::Client::builder()
            .connect_timeout(config.connect_timeout)
            .read_timeout(config.timeout)
            .build()
            .map_err(|e| ProviderError::Connection(e.to_string()))?;
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

    fn backoff(&self, attempt: u32, retry_after: Option<Duration>) -> Duration {
        crate::util::backoff(&self.config, attempt, retry_after)
    }

    /// Send with retry on 429/5xx. Returns the first successful response.
    async fn send(
        &self,
        url: &str,
        body: Option<&Value>,
    ) -> Result<reqwest::Response, ProviderError> {
        let mut attempt = 0;
        loop {
            let mut rb = match body {
                Some(b) => self.client.post(url).json(b),
                None => self.client.get(url),
            };
            if let Some(k) = &self.key {
                rb = rb.bearer_auth(&k.0);
            }
            let resp = rb.send().await.map_err(map_reqwest_error)?;
            let status = resp.status();
            if status.is_success() {
                return Ok(resp);
            }
            let retry_after = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.trim().parse::<u64>().ok())
                .map(Duration::from_secs);
            let message = error_text(resp).await;
            let err = match status.as_u16() {
                401 | 403 => ProviderError::Auth(message),
                429 => ProviderError::RateLimited { retry_after },
                s @ 500..=599 => ProviderError::Upstream { status: s, message },
                s => ProviderError::Api { status: s, message },
            };
            let retryable = matches!(
                err,
                ProviderError::RateLimited { .. } | ProviderError::Upstream { .. }
            );
            if !retryable || attempt >= self.config.max_retries {
                return Err(err);
            }
            tokio::time::sleep(self.backoff(attempt, retry_after)).await;
            attempt += 1;
        }
    }
}

async fn error_text(resp: reqwest::Response) -> String {
    let text = resp.text().await.unwrap_or_default();
    let msg = serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| {
            v.pointer("/error/message")
                .or_else(|| v.get("message"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or(text);
    msg.chars().take(500).collect()
}

struct StreamState {
    bytes: BoxStream<'static, Result<Vec<u8>, ProviderError>>,
    sse: SseDecoder,
    asm: Assembler,
    pending: VecDeque<ChatChunk>,
    eof: bool,
    done: bool,
}

impl StreamState {
    /// Absorb a payload; returns true when `[DONE]` was seen.
    fn absorb(&mut self, payload: &str) -> Result<(), ProviderError> {
        self.pending.extend(self.asm.feed(payload)?);
        Ok(())
    }
}

async fn next_chunk(
    mut st: StreamState,
) -> Result<Option<(ChatChunk, StreamState)>, ProviderError> {
    loop {
        if let Some(c) = st.pending.pop_front() {
            return Ok(Some((c, st)));
        }
        if st.done {
            return Ok(None);
        }
        if st.eof {
            if let Some(tail) = st.sse.finish() {
                st.absorb(&tail)?;
            }
            st.pending.extend(st.asm.finish()?);
            st.done = true;
            continue;
        }
        match st.bytes.next().await {
            Some(Ok(b)) => {
                for payload in st.sse.push(&b) {
                    st.absorb(&payload)?;
                }
            }
            Some(Err(e)) => return Err(e),
            None => st.eof = true,
        }
    }
}

#[async_trait]
impl ChatProvider for OpenAiCompatProvider {
    fn id(&self) -> &str {
        &self.entry.id
    }

    async fn stream(
        &self,
        req: ChatRequest,
    ) -> Result<BoxStream<'static, Result<ChatChunk, ProviderError>>, ProviderError> {
        let streaming = self.entry.capabilities.streaming;
        let body = request::build_body(&self.entry, self.model_name(&req), &req, streaming)?;
        let url = format!(
            "{}/chat/completions",
            self.entry.base_url.trim_end_matches('/')
        );
        let resp = self.send(&url, Some(&body)).await?;

        if !streaming {
            let text = resp.text().await.map_err(map_reqwest_error)?;
            let mut asm = Assembler::default();
            let mut chunks = asm.feed(&text)?;
            asm.finished = true;
            chunks.extend(asm.finish()?);
            return Ok(stream::iter(chunks.into_iter().map(Ok)).boxed());
        }

        let bytes = resp
            .bytes_stream()
            .map(|r| r.map(|b| b.to_vec()).map_err(map_reqwest_error))
            .boxed();
        let state = StreamState {
            bytes,
            sse: SseDecoder::default(),
            asm: Assembler::default(),
            pending: VecDeque::new(),
            eof: false,
            done: false,
        };
        Ok(stream::try_unfold(state, next_chunk).boxed())
    }

    async fn list_models(&self) -> Result<Vec<String>, ProviderError> {
        let Some(url) = &self.entry.models_endpoint else {
            return Ok(Vec::new());
        };
        let resp = self.send(url, None).await?;
        let v: Value = resp.json().await.map_err(map_reqwest_error)?;
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
