//! Plumbing shared by the provider adapters: HTTP client, retry with backoff,
//! SSE stream driving, and small request-conversion helpers.

use std::{
    collections::{HashMap, VecDeque},
    fmt,
    hash::{BuildHasher, Hasher},
    time::{Duration, SystemTime},
};

use futures::{StreamExt, stream, stream::BoxStream};
use reqwest::header::{HeaderMap, RETRY_AFTER};
use serde_json::{Map, Value};

use crate::{
    error::ProviderError,
    limits::error_body,
    openai_compat::{OpenAiCompatConfig, sse::SseDecoder},
    provider::{ChatChunk, ChatRequest, ChatStream, FinishReason, Usage},
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
        .retry(reqwest::retry::never())
        .build()
        .map_err(|e| ProviderError::Connection(e.to_string()))
}

pub(crate) fn map_reqwest_error(e: reqwest::Error) -> ProviderError {
    if e.is_timeout() {
        ProviderError::Timeout
    } else if e.is_builder() {
        ProviderError::InvalidRequest("the model provider request could not be built".into())
    } else {
        // Strip the URL: it may carry credentials in unusual setups.
        ProviderError::Connection(e.without_url().to_string())
    }
}

/// `Retry-After` accepts either delay-seconds or an HTTP date.
pub(crate) fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let value = headers.get(RETRY_AFTER)?.to_str().ok()?.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let deadline = httpdate::parse_http_date(value).ok()?;
    Some(
        deadline
            .duration_since(SystemTime::now())
            .unwrap_or_default(),
    )
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
    jittered_backoff(exp)
}

/// Equal jitter between half of an exponential delay and its capped value.
#[must_use]
pub fn jittered_backoff(delay: Duration) -> Duration {
    let rnd = std::hash::RandomState::new().build_hasher().finish();
    let frac = f64::from(u32::try_from(rnd >> 40).unwrap_or(0)) / f64::from(1u32 << 24);
    delay.mul_f64(0.5 + 0.5 * frac)
}

/// Turns a failed response (status, headers, raw body) into a [`ProviderError`].
pub(crate) type Classify = fn(u16, &HeaderMap, &str) -> ProviderError;

pub(crate) async fn send_once(
    request: reqwest::RequestBuilder,
    classify: Classify,
) -> Result<reqwest::Response, ProviderError> {
    let resp = request.send().await.map_err(map_reqwest_error)?;
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let headers = resp.headers().clone();
    let body = error_body(resp).await.unwrap_or_default();
    Err(classify(status.as_u16(), &headers, &body))
}

/// Retry transient failures before a response body is streamed. A retry must
/// fit both the attempt limit and the wall-clock budget for scheduling retries.
pub(crate) async fn send_with_retry(
    config: &OpenAiCompatConfig,
    make: &(dyn Fn() -> reqwest::RequestBuilder + Sync),
    classify: Classify,
) -> Result<reqwest::Response, ProviderError> {
    let started = tokio::time::Instant::now();
    let mut attempt = 0;
    loop {
        let err = match send_once(make(), classify).await {
            Ok(resp) => return Ok(resp),
            Err(err) => err,
        };
        if attempt >= config.max_retries || !err.is_retryable() {
            return Err(err);
        }
        let retry_after = err.retry_after();
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

/// Normalize HTTP errors and structured error events across the wire dialects.
/// A status of zero means the event supplied no HTTP status.
pub(crate) fn classify_response(status: u16, delay: Option<Duration>, body: &str) -> ProviderError {
    let message = error_message(body);
    let value = serde_json::from_str::<Value>(body).unwrap_or(Value::Null);
    let error = value.get("error").unwrap_or(&value);
    let codes = ["code", "type", "status"].map(|key| error[key].as_str().unwrap_or_default());
    let has = |known: &[&str]| codes.iter().any(|code| known.contains(code));
    let lower = message.to_ascii_lowercase();
    let mentions = |known: &[&str]| known.iter().any(|text| lower.contains(text));
    let status = if status == 0 {
        error["code"]
            .as_u64()
            .and_then(|n| u16::try_from(n).ok())
            .unwrap_or(0)
    } else {
        status
    };
    let err = if status == 401
        || has(&["invalid_api_key", "authentication_error", "UNAUTHENTICATED"])
    {
        ProviderError::Auth(message)
    } else if has(&["content_filter", "content_policy_violation", "safety_error"]) {
        ProviderError::ContentFilter("the response was blocked by a content filter".into())
    } else if status == 403 || has(&["permission_error", "PERMISSION_DENIED"]) {
        ProviderError::Auth(message)
    } else if has(&[
        "insufficient_quota",
        "billing_hard_limit_reached",
        "billing_error",
    ]) || status == 402
    {
        ProviderError::Quota
    } else if has(&[
        "context_length_exceeded",
        "context_window_exceeded",
        "prompt_too_long",
        "exceed_context_size_error",
    ]) || (matches!(status, 0 | 400 | 413 | 422)
        && mentions(&[
            "context length",
            "context window",
            "context size",
            "prompt is too long",
            "prompt too long",
            "input token count exceeds",
            "input token count (",
        ]))
    {
        ProviderError::ContextTooLong
    } else if status == 429
        || has(&[
            "rate_limit_exceeded",
            "rate_limit_error",
            "RESOURCE_EXHAUSTED",
        ])
    {
        ProviderError::RateLimited { retry_after: delay }
    } else if matches!(status, 408 | 504) || has(&["timeout", "timeout_error", "DEADLINE_EXCEEDED"])
    {
        ProviderError::Timeout
    } else if matches!(status, 500..=599)
        || has(&[
            "overloaded_error",
            "overloaded",
            "model_overloaded",
            "server_error",
            "UNAVAILABLE",
            "INTERNAL",
        ])
        || (status == 0 && matches!(lower.as_str(), "overloaded" | "service unavailable"))
    {
        ProviderError::Upstream {
            status: if status >= 500 { status } else { 503 },
            message,
        }
    } else if status == 0
        && has(&[
            "INVALID_ARGUMENT",
            "FAILED_PRECONDITION",
            "invalid_request_error",
        ])
    {
        ProviderError::InvalidRequest(message)
    } else if status == 0 {
        ProviderError::Stream("the model provider returned an unrecognized error".into())
    } else {
        ProviderError::Api { status, message }
    };
    if let Some(delay) = delay
        && matches!(err, ProviderError::Timeout | ProviderError::Upstream { .. })
    {
        return ProviderError::RetryAfter {
            source: Box::new(err),
            delay,
        };
    }
    err
}

/// The error for a response whose finish reason is not a normal completion, or
/// `None` for `Stop` / `Length` / `ToolCalls`. Safety-type reasons become
/// [`ProviderError::ContentFilter`], `MALFORMED_FUNCTION_CALL` becomes
/// [`ProviderError::MalformedToolCall`], anything else unknown a
/// [`ProviderError::Stream`] (never a silent empty "stop").
pub(crate) fn finish_error(reason: &FinishReason) -> Option<ProviderError> {
    match reason {
        FinishReason::Stop | FinishReason::Length | FinishReason::ToolCalls => None,
        FinishReason::ContentFilter => Some(ProviderError::ContentFilter(
            "the response was blocked by a content filter".into(),
        )),
        FinishReason::Other(raw) => {
            let up = raw.to_ascii_uppercase();
            Some(if up == "MODEL_CONTEXT_WINDOW_EXCEEDED" {
                ProviderError::ContextTooLong
            } else if up.contains("MALFORMED") || up.contains("UNEXPECTED_TOOL_CALL") {
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
            })
        }
    }
}

/// Boxed chunk stream, as returned by [`crate::provider::ChatProvider::stream`].
pub(crate) type ChunkStream = ChatStream;

/// Boxed future that (re)opens a [`ChunkStream`].
pub(crate) type Reopen =
    Box<dyn Fn() -> futures::future::BoxFuture<'static, Result<ChunkStream, ProviderError>> + Send>;

/// Wrap `first` so that a [`ProviderError::MalformedToolCall`] raised before any
/// chunk was yielded is retried exactly once by calling `reopen`.
pub(crate) fn retry_malformed_once(
    first: ChunkStream,
    reopen: Reopen,
    delay: Duration,
) -> ChunkStream {
    struct St {
        cur: ChunkStream,
        reopen: Reopen,
        emitted: bool,
        retried: bool,
        delay: Duration,
    }
    let st = St {
        cur: first,
        reopen,
        emitted: false,
        retried: false,
        delay,
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
                    tokio::time::sleep(st.delay).await;
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
    /// An explicit final wire event permits closing the body immediately.
    fn terminal(&self) -> bool {
        false
    }
    /// Preserve the last available usage if the stream failed before finishing.
    fn take_usage(&mut self) -> Option<Usage>;
}

struct StreamState<D, B> {
    bytes: BoxStream<'static, Result<B, ProviderError>>,
    sse: SseDecoder,
    dec: D,
    pending: VecDeque<ChatChunk>,
    frame: Option<(B, usize)>,
    failure: Option<ProviderError>,
    idle: Duration,
    done: bool,
}

impl<D: EventDecoder, B: AsRef<[u8]> + Send + 'static> StreamState<D, B> {
    async fn read_chunks(&mut self) -> Result<(), ProviderError> {
        while self.pending.is_empty() {
            if self.dec.terminal() {
                return self.finish();
            }
            if let Some((bytes, offset)) = &mut self.frame {
                let mut input = &bytes.as_ref()[*offset..];
                let before = input.len();
                let event = self.sse.next_event(&mut input)?;
                *offset += before - input.len();
                if input.is_empty() {
                    self.frame = None;
                }
                if let Some(payload) = event {
                    self.pending.extend(self.dec.feed(&payload)?);
                }
                continue;
            }
            match self.bytes.next().await {
                Some(Ok(bytes)) => self.frame = Some((bytes, 0)),
                Some(Err(err)) => return Err(err),
                None => {
                    if let Some(tail) = self.sse.finish()? {
                        self.pending.extend(self.dec.feed(&tail)?);
                    }
                    return self.finish();
                }
            }
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<(), ProviderError> {
        self.pending.extend(self.dec.finish()?);
        self.close();
        Ok(())
    }

    fn close(&mut self) {
        self.done = true;
        self.frame = None;
        self.bytes = stream::empty().boxed();
    }

    async fn next(mut self) -> Option<(Result<ChatChunk, ProviderError>, Self)> {
        if self.pending.is_empty() && !self.done {
            let result = tokio::time::timeout(self.idle, self.read_chunks())
                .await
                .unwrap_or(Err(ProviderError::Timeout));
            if let Err(error) = result {
                self.close();
                self.pending
                    .extend(self.dec.take_usage().map(ChatChunk::Usage));
                self.failure = Some(error);
            }
        }
        let item = self
            .pending
            .pop_front()
            .map(Ok)
            .or_else(|| self.failure.take().map(Err))?;
        Some((item, self))
    }
}

/// Stream a successful SSE response through `decoder`.
pub(crate) fn event_stream<D: EventDecoder>(
    resp: reqwest::Response,
    decoder: D,
    idle: Duration,
) -> ChatStream {
    let bytes = resp
        .bytes_stream()
        .map(|r| r.map_err(map_reqwest_error))
        .boxed();
    event_stream_bytes(bytes, decoder, idle)
}

pub(crate) fn event_stream_bytes<D, B>(
    bytes: BoxStream<'static, Result<B, ProviderError>>,
    decoder: D,
    idle: Duration,
) -> ChatStream
where
    D: EventDecoder,
    B: AsRef<[u8]> + Send + 'static,
{
    let state = StreamState {
        bytes,
        sse: SseDecoder::default(),
        dec: decoder,
        pending: VecDeque::new(),
        frame: None,
        failure: None,
        idle,
        done: false,
    };
    stream::unfold(state, StreamState::next).boxed()
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

#[cfg(test)]
mod property_tests {
    use proptest::collection::vec;
    use proptest::prelude::*;
    use proptest::sample::select;
    use proptest::test_runner::RngSeed;
    use serde_json::json;

    use super::{ThoughtFilter, error_message, parse_args, parse_data_url};

    fn config() -> ProptestConfig {
        let mut config = ProptestConfig::default();
        if config.rng_seed == RngSeed::Random {
            config.rng_seed = RngSeed::Fixed(461);
        }
        config
    }

    fn model_stream() -> impl Strategy<Value = String> {
        const PIECES: &[&str] = &[
            "<thought>",
            "</thought>",
            "<tho",
            "ught>",
            "</th",
            "ought>",
            "<",
            ">",
            "\0",
            "\n",
            "İıŞş🦀",
        ];
        let seg = prop_oneof![
            3 => select(PIECES).prop_map(str::to_owned),
            2 => ".{0,24}",
        ];
        vec(seg, 0..=24).prop_map(|parts| parts.concat())
    }

    proptest! {
        #![proptest_config(config())]

        /// `ThoughtFilter` produces identical visible text regardless of how a stream is split across UTF-8 character boundaries.
        #[test]
        fn thought_filter_is_chunk_boundary_invariant(
            stream in model_stream(),
            step in 1..=7_usize,
        ) {
            let mut single = ThoughtFilter::default();
            let mut one_shot = single.push(&stream);
            one_shot.push_str(&single.finish());

            let chars: Vec<char> = stream.chars().collect();
            let mut chunked = ThoughtFilter::default();
            let mut incremental = String::new();
            for slice in chars.chunks(step) {
                let piece: String = slice.iter().collect();
                incremental.push_str(&chunked.push(&piece));
            }
            incremental.push_str(&chunked.finish());

            prop_assert_eq!(incremental, one_shot);
        }

        /// `parse_args` never panics on arbitrary text, always yields a JSON object, and preserves valid JSON objects.
        #[test]
        fn parse_args_never_panics_and_always_returns_an_object(
            raw in ".{0,256}",
            key in "[a-z_]{1,16}",
            val in ".{0,64}",
        ) {
            let parsed = parse_args(&raw);
            prop_assert!(parsed.is_object(), "{parsed:?}");

            let obj = json!({ key: val });
            prop_assert_eq!(parse_args(&obj.to_string()), obj);
        }

        /// `parse_data_url` never panics on arbitrary text and round-trips `data:<mime>;base64,<payload>`.
        #[test]
        fn parse_data_url_round_trips_and_never_panics(
            raw in ".{0,256}",
            mime in "[a-z]{2,12}/[a-z0-9.+-]{2,16}",
            payload in "[A-Za-z0-9+/=]{0,128}",
        ) {
            let _ = parse_data_url(&raw);
            let url = format!("data:{mime};base64,{payload}");
            prop_assert_eq!(parse_data_url(&url), Some((mime, payload)));
        }

        /// `error_message` never panics on arbitrary text and truncates its result to at most 500 characters.
        #[test]
        fn error_message_never_panics_and_caps_at_500_chars(raw in ".{0,1024}") {
            let msg = error_message(&raw);
            prop_assert!(msg.chars().count() <= 500);
            let wrapped = json!({"error": {"message": raw}}).to_string();
            let wrapped_msg = error_message(&wrapped);
            prop_assert!(wrapped_msg.chars().count() <= 500);
        }
    }
}
