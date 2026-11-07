//! Plumbing shared by the provider adapters: HTTP client, retry with backoff,
//! SSE stream driving, and small request-conversion helpers.

use std::{
    collections::VecDeque,
    fmt,
    hash::{BuildHasher, Hasher},
    time::Duration,
};

use futures::{StreamExt, stream, stream::BoxStream};
use reqwest::header::{HeaderMap, RETRY_AFTER};
use serde_json::{Map, Value};

use crate::{
    error::ProviderError,
    openai_compat::{OpenAiCompatConfig, sse::SseDecoder},
    provider::{ChatChunk, ChatRequest},
};

/// API key that never appears in `Debug` output.
#[derive(Clone)]
pub(crate) struct Secret(pub(crate) String);

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// Read an API key from an environment variable.
pub(crate) fn key_from_env(var: &str) -> Result<String, ProviderError> {
    match std::env::var(var) {
        Ok(k) if !k.trim().is_empty() => Ok(k),
        _ => Err(ProviderError::Auth(format!(
            "environment variable {var} is not set"
        ))),
    }
}

pub(crate) fn build_client(config: &OpenAiCompatConfig) -> Result<reqwest::Client, ProviderError> {
    reqwest::Client::builder()
        .connect_timeout(config.connect_timeout)
        .read_timeout(config.timeout)
        .build()
        .map_err(|e| ProviderError::Connection(e.to_string()))
}

pub(crate) fn map_reqwest_error(e: reqwest::Error) -> ProviderError {
    if e.is_timeout() {
        ProviderError::Timeout
    } else {
        // Strip the URL: it may carry credentials in unusual setups.
        ProviderError::Connection(e.without_url().to_string())
    }
}

/// `Retry-After` header in whole seconds.
pub(crate) fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get(RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
}

/// Jittered exponential backoff, honouring `Retry-After`, capped at `max_delay`.
pub(crate) fn backoff(
    config: &OpenAiCompatConfig,
    attempt: u32,
    retry_after: Option<Duration>,
) -> Duration {
    let max = config.max_delay;
    if let Some(ra) = retry_after {
        return ra.min(max);
    }
    let exp = config
        .base_delay
        .saturating_mul(2u32.saturating_pow(attempt))
        .min(max);
    // Equal jitter: uniformly in [exp/2, exp].
    let rnd = std::hash::RandomState::new().build_hasher().finish();
    let frac = f64::from(u32::try_from(rnd >> 40).unwrap_or(0)) / f64::from(1u32 << 24);
    exp.mul_f64(0.5 + 0.5 * frac)
}

/// Turns a failed response (status, headers, raw body) into a [`ProviderError`].
pub(crate) type Classify = fn(u16, &HeaderMap, &str) -> ProviderError;

/// Send a request, retrying 429/5xx (rate limit / upstream) with backoff.
/// Returns the first successful response.
pub(crate) async fn send_with_retry(
    config: &OpenAiCompatConfig,
    make: &(dyn Fn() -> reqwest::RequestBuilder + Sync),
    classify: Classify,
) -> Result<reqwest::Response, ProviderError> {
    let mut attempt = 0;
    loop {
        let resp = make().send().await.map_err(map_reqwest_error)?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let headers = resp.headers().clone();
        let body = resp.text().await.unwrap_or_default();
        let err = classify(status.as_u16(), &headers, &body);
        let retry = match &err {
            ProviderError::RateLimited { retry_after } => Some(*retry_after),
            ProviderError::Upstream { .. } => Some(None),
            _ => None,
        };
        let Some(retry_after) = retry else {
            return Err(err);
        };
        if attempt >= config.max_retries {
            return Err(err);
        }
        tokio::time::sleep(backoff(config, attempt, retry_after)).await;
        attempt += 1;
    }
}

/// Incremental decoder fed with SSE `data:` payloads.
pub(crate) trait EventDecoder: Send + 'static {
    /// Absorb one payload.
    fn feed(&mut self, payload: &str) -> Result<Vec<ChatChunk>, ProviderError>;
    /// Called at end of stream; emits the final chunks or errors if truncated.
    fn finish(&mut self) -> Result<Vec<ChatChunk>, ProviderError>;
}

struct StreamState<D> {
    bytes: BoxStream<'static, Result<Vec<u8>, ProviderError>>,
    sse: SseDecoder,
    dec: D,
    pending: VecDeque<ChatChunk>,
    eof: bool,
    done: bool,
}

async fn next_chunk<D: EventDecoder>(
    mut st: StreamState<D>,
) -> Result<Option<(ChatChunk, StreamState<D>)>, ProviderError> {
    loop {
        if let Some(c) = st.pending.pop_front() {
            return Ok(Some((c, st)));
        }
        if st.done {
            return Ok(None);
        }
        if st.eof {
            if let Some(tail) = st.sse.finish() {
                let chunks = st.dec.feed(&tail)?;
                st.pending.extend(chunks);
            }
            let chunks = st.dec.finish()?;
            st.pending.extend(chunks);
            st.done = true;
            continue;
        }
        match st.bytes.next().await {
            Some(Ok(b)) => {
                for payload in st.sse.push(&b) {
                    let chunks = st.dec.feed(&payload)?;
                    st.pending.extend(chunks);
                }
            }
            Some(Err(e)) => return Err(e),
            None => st.eof = true,
        }
    }
}

/// Stream a successful SSE response through `decoder`.
pub(crate) fn event_stream<D: EventDecoder>(
    resp: reqwest::Response,
    decoder: D,
) -> BoxStream<'static, Result<ChatChunk, ProviderError>> {
    let bytes = resp
        .bytes_stream()
        .map(|r| r.map(|b| b.to_vec()).map_err(map_reqwest_error))
        .boxed();
    let state = StreamState {
        bytes,
        sse: SseDecoder::default(),
        dec: decoder,
        pending: VecDeque::new(),
        eof: false,
        done: false,
    };
    stream::try_unfold(state, next_chunk).boxed()
}

/// Model name to send: strips the `<provider>/` prefix when it names us.
pub(crate) fn model_name<'a>(provider_id: &str, req: &'a ChatRequest) -> &'a str {
    if req.model.provider() == Some(provider_id) {
        req.model.model()
    } else {
        &req.model.0
    }
}

/// Plain text of a message content (string, or the text parts of a parts array).
pub(crate) fn content_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Whether a content parts array contains an image.
pub(crate) fn has_image(content: &Value) -> bool {
    content.as_array().is_some_and(|parts| {
        parts.iter().any(|p| {
            matches!(
                p.get("type").and_then(Value::as_str),
                Some("image_url" | "image")
            )
        })
    })
}

/// Split `data:<mime>;base64,<data>` into `(mime, data)`.
pub(crate) fn parse_data_url(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    let mime = meta.strip_suffix(";base64")?;
    Some((mime.to_owned(), data.to_owned()))
}

/// The URL of an OpenAI-style `image_url` part.
pub(crate) fn image_url(part: &Value) -> Option<&str> {
    let iu = part.get("image_url")?;
    iu.as_str()
        .or_else(|| iu.get("url").and_then(Value::as_str))
}

/// Name, description and parameter schema of an OpenAI-format tool.
pub(crate) fn tool_parts(tool: &Value) -> Option<(&str, Option<&str>, Option<&Value>)> {
    let f = tool.get("function").unwrap_or(tool);
    let name = f.get("name")?.as_str()?;
    Some((
        name,
        f.get("description").and_then(Value::as_str),
        f.get("parameters"),
    ))
}

/// Parse tool-call arguments into a JSON object (`{}` when empty or invalid).
pub(crate) fn parse_args(args: &str) -> Value {
    match serde_json::from_str::<Value>(args) {
        Ok(v @ Value::Object(_)) => v,
        _ => Value::Object(Map::new()),
    }
}

/// `error.message` (or `message`) of a JSON error body; the raw text otherwise.
/// Truncated to 500 characters.
pub(crate) fn error_message(body: &str) -> String {
    let msg = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            v.pointer("/error/message")
                .or_else(|| v.get("message"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| body.to_owned());
    msg.chars().take(500).collect()
}

/// Append `blocks` to the last message when it has the same role, else start a
/// new message (providers require alternating roles).
pub(crate) fn push_merged<R: PartialEq>(
    out: &mut Vec<(R, Vec<Value>)>,
    role: R,
    blocks: Vec<Value>,
) {
    if blocks.is_empty() {
        return;
    }
    match out.last_mut() {
        Some(last) if last.0 == role => last.1.extend(blocks),
        _ => out.push((role, blocks)),
    }
}
