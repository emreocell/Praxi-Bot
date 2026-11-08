//! Plumbing shared by the provider adapters: HTTP client, retry with backoff,
//! SSE stream driving, and small request-conversion helpers.

use std::{
    collections::{HashMap, VecDeque},
    fmt,
    hash::{BuildHasher, Hasher},
    time::{Duration, Instant},
};

use futures::{StreamExt, stream, stream::BoxStream};
use reqwest::header::{HeaderMap, RETRY_AFTER};
use serde_json::{Map, Value};

use crate::{
    error::ProviderError,
    limits::error_body,
    openai_compat::{OpenAiCompatConfig, sse::SseDecoder},
    provider::{ChatChunk, ChatRequest, FinishReason},
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

/// Jittered exponential backoff, capped at `max_delay`. An explicit
/// `Retry-After` is returned unchanged (never truncated).
pub(crate) fn backoff(
    config: &OpenAiCompatConfig,
    attempt: u32,
    retry_after: Option<Duration>,
) -> Duration {
    let max = config.max_delay;
    // A server-requested wait is honoured as-is; callers that cannot afford it
    // (longer than `max_delay`) give up before sleeping, see `send_with_retry`.
    if let Some(ra) = retry_after {
        return ra;
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
/// Returns the first successful response. Retrying stops (returning the last
/// error) once `max_retries` is reached or the next backoff would push the
/// total time spent past `config.max_total_retry`.
pub(crate) async fn send_with_retry(
    config: &OpenAiCompatConfig,
    make: &(dyn Fn() -> reqwest::RequestBuilder + Sync),
    classify: Classify,
) -> Result<reqwest::Response, ProviderError> {
    let started = Instant::now();
    let mut attempt = 0;
    loop {
        let resp = make().send().await.map_err(map_reqwest_error)?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let headers = resp.headers().clone();
        let body = error_body(resp).await.unwrap_or_default();
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
        // A requested wait longer than we are willing to sleep can never be
        // honoured: retrying earlier is futile, so fail now.
        if retry_after.is_some_and(|ra| ra > config.max_delay) {
            return Err(err);
        }
        let delay = backoff(config, attempt, retry_after);
        if started.elapsed().saturating_add(delay) > config.max_total_retry {
            return Err(err);
        }
        tokio::time::sleep(delay).await;
        attempt += 1;
    }
}

/// The error for a response whose finish reason is not a normal completion, or
/// `None` for `Stop` / `Length` / `ToolCalls`. Safety-type reasons become
/// [`ProviderError::ContentFilter`], `MALFORMED_FUNCTION_CALL` becomes
/// [`ProviderError::MalformedToolCall`], anything else unknown a retryable
/// [`ProviderError::Stream`] (never a silent empty "stop").
pub(crate) fn finish_error(reason: &FinishReason) -> Option<ProviderError> {
    match reason {
        FinishReason::Stop | FinishReason::Length | FinishReason::ToolCalls => None,
        FinishReason::ContentFilter => Some(ProviderError::ContentFilter(
            "the response was blocked by a content filter".into(),
        )),
        FinishReason::Other(raw) => {
            let up = raw.to_ascii_uppercase();
            Some(
                if up.contains("MALFORMED") || up.contains("UNEXPECTED_TOOL_CALL") {
                    ProviderError::MalformedToolCall(format!("finish reason {raw}"))
                } else if [
                    "SAFETY",
                    "RECITATION",
                    "FILTER",
                    "BLOCK",
                    "PROHIBITED",
                    "SPII",
                    "LANGUAGE",
                ]
                .iter()
                .any(|k| up.contains(k))
                {
                    ProviderError::ContentFilter(format!("finish reason {raw}"))
                } else {
                    ProviderError::Stream(format!("unexpected finish reason {raw}"))
                },
            )
        }
    }
}

/// Boxed chunk stream, as returned by [`crate::provider::ChatProvider::stream`].
pub(crate) type ChunkStream = BoxStream<'static, Result<ChatChunk, ProviderError>>;

/// Boxed future that (re)opens a [`ChunkStream`].
pub(crate) type Reopen =
    Box<dyn Fn() -> futures::future::BoxFuture<'static, Result<ChunkStream, ProviderError>> + Send>;

/// Wrap `first` so that a [`ProviderError::MalformedToolCall`] raised before any
/// chunk was yielded is retried exactly once by calling `reopen`.
pub(crate) fn retry_malformed_once(first: ChunkStream, reopen: Reopen) -> ChunkStream {
    struct St {
        cur: ChunkStream,
        reopen: Reopen,
        emitted: bool,
        retried: bool,
    }
    let st = St {
        cur: first,
        reopen,
        emitted: false,
        retried: false,
    };
    stream::unfold(st, |mut st| async move {
        loop {
            match st.cur.next().await {
                Some(Ok(c)) => {
                    st.emitted = true;
                    return Some((Ok(c), st));
                }
                Some(Err(ProviderError::MalformedToolCall(_))) if !st.emitted && !st.retried => {
                    st.retried = true;
                    match (st.reopen)().await {
                        Ok(s) => st.cur = s,
                        Err(e) => {
                            st.cur = stream::empty().boxed();
                            return Some((Err(e), st));
                        }
                    }
                }
                Some(Err(e)) => {
                    st.cur = stream::empty().boxed();
                    return Some((Err(e), st));
                }
                None => return None,
            }
        }
    })
    .boxed()
}

/// Removes `<thought>...</thought>` spans from streamed text, including tags
/// split across deltas. Text inside an unterminated span is dropped.
#[derive(Debug, Default)]
pub(crate) struct ThoughtFilter {
    in_thought: bool,
    carry: String,
}

impl ThoughtFilter {
    const OPEN: &'static str = "<thought>";
    const CLOSE: &'static str = "</thought>";

    /// Length of the longest suffix of `s` that is a proper prefix of `tag`.
    fn partial_suffix(s: &str, tag: &str) -> usize {
        (1..tag.len().min(s.len() + 1))
            .rev()
            .find(|&n| s.is_char_boundary(s.len() - n) && tag.starts_with(&s[s.len() - n..]))
            .unwrap_or(0)
    }

    /// Feed a delta; returns the visible part.
    pub(crate) fn push(&mut self, text: &str) -> String {
        let mut buf = std::mem::take(&mut self.carry);
        buf.push_str(text);
        let mut out = String::new();
        loop {
            if self.in_thought {
                if let Some(i) = buf.find(Self::CLOSE) {
                    buf.drain(..i + Self::CLOSE.len());
                    self.in_thought = false;
                } else {
                    let keep = Self::partial_suffix(&buf, Self::CLOSE);
                    self.carry = buf.split_off(buf.len() - keep);
                    return out;
                }
            } else if let Some(i) = buf.find(Self::OPEN) {
                out.push_str(&buf[..i]);
                buf.drain(..i + Self::OPEN.len());
                self.in_thought = true;
            } else {
                let keep = Self::partial_suffix(&buf, Self::OPEN);
                self.carry = buf.split_off(buf.len() - keep);
                out.push_str(&buf);
                return out;
            }
        }
    }

    /// End of stream: releases a held-back partial tag prefix that turned out
    /// to be ordinary text.
    pub(crate) fn finish(&mut self) -> String {
        let rest = std::mem::take(&mut self.carry);
        if self.in_thought { String::new() } else { rest }
    }
}

/// Tool calls in `messages` whose arguments are not a JSON object, keyed by call
/// id, with the original argument text.
pub(crate) fn invalid_arg_calls(messages: &[crate::provider::Message]) -> HashMap<&str, &str> {
    messages
        .iter()
        .flat_map(|m| &m.tool_calls)
        .filter(|c| {
            !matches!(
                serde_json::from_str::<Value>(&c.arguments),
                Ok(Value::Object(_))
            )
        })
        .map(|c| (c.id.as_str(), c.arguments.as_str()))
        .collect()
}

/// Tool-result text with a note about the call's original, invalid arguments.
pub(crate) fn with_args_note(text: &str, original: &str) -> String {
    let shown: String = original.chars().take(1000).collect();
    format!(
        "[note: the arguments of this call were not valid JSON and were sent as {{}}; original: {shown}]
{text}"
    )
}

/// Replace large inline payloads (base64 / `data:` URLs) in a request body so a
/// debug dump stays small and shareable. The API key is never part of the body.
pub(crate) fn redact_body(v: &Value) -> Value {
    match v {
        Value::String(s) if s.len() > 512 => Value::String(format!("<{} bytes redacted>", s.len())),
        Value::Array(a) => Value::Array(a.iter().map(redact_body).collect()),
        Value::Object(o) => {
            Value::Object(o.iter().map(|(k, v)| (k.clone(), redact_body(v))).collect())
        }
        other => other.clone(),
    }
}

/// When `config.debug_dir` is set and `err` is an HTTP 400-class failure, write
/// the redacted request body and the error to
/// `<debug_dir>/last-failed-request-<provider>.json` (best effort).
pub(crate) fn dump_failed_request(
    config: &OpenAiCompatConfig,
    provider_id: &str,
    body: &Value,
    err: &ProviderError,
) {
    let Some(dir) = &config.debug_dir else { return };
    let status = match err {
        ProviderError::InvalidRequest(_) | ProviderError::Unsupported { .. } => 400,
        ProviderError::Api { status, .. } if (400..500).contains(status) => *status,
        _ => return,
    };
    let dump = serde_json::json!({
        "provider": provider_id,
        "status": status,
        "error": err.to_string(),
        "request": redact_body(body),
    });
    let safe: String = provider_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::write(
        dir.join(format!("last-failed-request-{safe}.json")),
        serde_json::to_vec_pretty(&dump).unwrap_or_default(),
    );
}

/// Validate tool-call arguments before replaying them to a provider: valid JSON
/// objects pass through; anything else is replaced by `{}` (the original text
/// stays in the matching tool-result message).
pub(crate) fn sanitize_args(args: &str) -> String {
    match serde_json::from_str::<Value>(args) {
        Ok(Value::Object(_)) => args.to_owned(),
        _ => "{}".to_owned(),
    }
}

/// Incremental decoder fed with SSE `data:` payloads.
pub(crate) trait EventDecoder: Send + 'static {
    /// Absorb one payload.
    fn feed(&mut self, payload: &str) -> Result<Vec<ChatChunk>, ProviderError>;
    /// Called at end of stream; emits the final chunks or errors if truncated.
    fn finish(&mut self) -> Result<Vec<ChatChunk>, ProviderError>;
}

struct StreamState<D, B> {
    bytes: BoxStream<'static, Result<B, ProviderError>>,
    sse: SseDecoder,
    dec: D,
    pending: VecDeque<ChatChunk>,
    eof: bool,
    done: bool,
}

async fn next_chunk<D: EventDecoder, B: AsRef<[u8]> + Send + 'static>(
    mut st: StreamState<D, B>,
) -> Result<Option<(ChatChunk, StreamState<D, B>)>, ProviderError> {
    loop {
        if let Some(c) = st.pending.pop_front() {
            return Ok(Some((c, st)));
        }
        if st.done {
            return Ok(None);
        }
        if st.eof {
            if let Some(tail) = st.sse.finish()? {
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
                let mut input = b.as_ref();
                while let Some(payload) = st.sse.next_event(&mut input)? {
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
        .map(|r| r.map_err(map_reqwest_error))
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

#[cfg(test)]
mod thought_tests {
    use super::ThoughtFilter;

    fn run(parts: &[&str]) -> String {
        let mut f = ThoughtFilter::default();
        let mut out: String = parts.iter().map(|p| f.push(p)).collect();
        out.push_str(&f.finish());
        out
    }

    #[test]
    fn strips_spans_and_split_tags() {
        assert_eq!(run(&["a<thought>x</thought>b"]), "ab");
        assert_eq!(run(&["a<tho", "ught>x</th", "ought>b"]), "ab");
        assert_eq!(run(&["no tags here"]), "no tags here");
        // A lone `<` or partial tag at the end is plain text.
        assert_eq!(run(&["1 < 2", " and <thou"]), "1 < 2 and <thou");
        // Unterminated thought is dropped.
        assert_eq!(run(&["ok<thought>never closed"]), "ok");
        assert_eq!(run(&["é<thought>é</thought>é"]), "éé");
    }
}
