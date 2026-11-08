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

use reqwest::header::HeaderMap;

use crate::util::{
    Classify, Secret, dump_failed_request, error_message, map_reqwest_error, retry_after,
    send_with_retry,
};
use crate::{
    entry::{Auth, ProviderEntry},
    error::ProviderError,
    limits::{response_json, response_text},
    provider::{ChatChunk, ChatProvider, ChatRequest},
};
use sse::{Assembler, Assembly, SseDecoder};

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

    /// Send with retry on 429/5xx. Returns the first successful response.
    async fn send(
        &self,
        url: &str,
        body: Option<&Value>,
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
        let result = send_with_retry(&self.config, &make, classify as Classify).await;
        if let (Err(e), Some(b)) = (&result, body) {
            dump_failed_request(&self.config, &self.entry.id, b, e);
        }
        result
    }
}

fn classify(status: u16, headers: &HeaderMap, body: &str) -> ProviderError {
    let message = error_message(body);
    match status {
        401 | 403 => ProviderError::Auth(message),
        429 => ProviderError::RateLimited {
            retry_after: retry_after(headers),
        },
        s @ 500..=599 => ProviderError::Upstream { status: s, message },
        // Ollama (`"<model> does not support tools"`) and LM Studio
        // (`"The model does not support tools."`, code `model_not_support_tools`)
        // both refuse tool calls with HTTP 400. Any other 400 stays `Api`.
        400 if message.contains("does not support tools") => ProviderError::Unsupported {
            capability: "tools".into(),
        },
        s => ProviderError::Api { status: s, message },
    }
}

struct StreamState<B> {
    bytes: BoxStream<'static, Result<B, ProviderError>>,
    sse: SseDecoder,
    asm: Assembler,
    pending: VecDeque<ChatChunk>,
    eof: bool,
    done: bool,
}

impl<B: Send + 'static> StreamState<B> {
    /// Absorb a payload and flush once when the assembler signals `[DONE]`.
    fn absorb(&mut self, payload: &str) -> Result<(), ProviderError> {
        match self.asm.feed(payload)? {
            Assembly::Chunks(chunks) => self.pending.extend(chunks),
            Assembly::Done => {
                self.pending.extend(self.asm.finish()?);
                self.done = true;
                // Release the response immediately, even if the consumer pauses
                // while draining the final tool calls / usage / Done chunks.
                self.bytes = stream::empty().boxed();
            }
        }
        Ok(())
    }
}

async fn next_chunk<B: AsRef<[u8]> + Send + 'static>(
    mut st: StreamState<B>,
) -> Result<Option<(ChatChunk, StreamState<B>)>, ProviderError> {
    loop {
        if let Some(c) = st.pending.pop_front() {
            return Ok(Some((c, st)));
        }
        if st.done {
            return Ok(None);
        }
        if st.eof {
            if let Some(tail) = st.sse.finish()? {
                st.absorb(&tail)?;
            }
            if !st.done {
                st.pending.extend(st.asm.finish()?);
                st.done = true;
            }
            continue;
        }
        match st.bytes.next().await {
            Some(Ok(b)) => {
                let mut input = b.as_ref();
                while let Some(payload) = st.sse.next_event(&mut input)? {
                    st.absorb(&payload)?;
                    if st.done {
                        break;
                    }
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
        let model = self.model_name(&req);
        let streaming = self.entry.caps_for(model).streaming;
        let body = request::build_body(&self.entry, model, &req, streaming)?;
        let url = format!(
            "{}/chat/completions",
            self.entry.base_url.trim_end_matches('/')
        );
        let resp = self.send(&url, Some(&body)).await?;

        if !streaming {
            let text = response_text(resp).await?;
            let mut asm = Assembler::default();
            let mut chunks = match asm.feed(&text)? {
                Assembly::Chunks(chunks) => chunks,
                Assembly::Done => Vec::new(),
            };
            asm.finished = true;
            chunks.extend(asm.finish()?);
            return Ok(stream::iter(chunks.into_iter().map(Ok)).boxed());
        }

        let bytes = resp
            .bytes_stream()
            .map(|r| r.map_err(map_reqwest_error))
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
