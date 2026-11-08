//! Decoding of Gemini `streamGenerateContent` SSE chunks into [`ChatChunk`]s.

use std::{
    hash::{BuildHasher, Hasher},
    time::Duration,
};

use reqwest::header::HeaderMap;
use serde_json::Value;

use super::{classify, request::make_call_id};
use crate::{
    error::ProviderError,
    limits::ToolArgumentBudget,
    provider::{ChatChunk, FinishReason, ToolCall, Usage},
    util::{EventDecoder, ThoughtFilter, finish_error},
};

#[derive(Debug, Default)]
pub(super) struct Decoder {
    calls: Vec<ToolCall>,
    arguments: ToolArgumentBudget,
    usage: Option<Usage>,
    finish: Option<FinishReason>,
    finished: bool,
    thoughts: ThoughtFilter,
}

fn parse_finish(s: &str) -> FinishReason {
    match s {
        "STOP" => FinishReason::Stop,
        "MAX_TOKENS" => FinishReason::Length,
        other => FinishReason::Other(other.to_owned()),
    }
}

fn parse_usage(u: &Value) -> Result<Usage, ProviderError> {
    let get = |k: &str| u.get(k).and_then(Value::as_u64);
    // Thinking tokens are billed as output; reject totals that cannot be represented.
    let completion_tokens = get("candidatesTokenCount")
        .unwrap_or(0)
        .checked_add(get("thoughtsTokenCount").unwrap_or(0))
        .ok_or_else(|| ProviderError::Stream("Gemini completion-token total overflow".into()))?;
    Ok(Usage {
        prompt_tokens: get("promptTokenCount").unwrap_or(0),
        completion_tokens,
        cached_tokens: get("cachedContentTokenCount"),
        cache_write_tokens: None,
    })
}

fn unique_id(index: usize) -> u64 {
    let mut h = std::hash::RandomState::new().build_hasher();
    h.write_usize(index);
    h.finish()
}

/// Upper bound for a server-supplied `retryDelay` (the retry loop gives up on
/// anything longer than its own `max_delay` anyway).
const MAX_RETRY_DELAY: Duration = Duration::from_secs(24 * 3600);

/// Gemini's `retryDelay` ("30s", "1.5s") from error details.
pub(super) fn retry_delay(body: &str) -> Option<Duration> {
    let v: Value = serde_json::from_str(body).ok()?;
    let details = v.pointer("/error/details")?.as_array()?;
    let s = details
        .iter()
        .find_map(|d| d.get("retryDelay").and_then(Value::as_str))?;
    let secs: f64 = s.strip_suffix('s')?.parse().ok()?;
    if !secs.is_finite() || secs < 0.0 {
        return None;
    }
    // `try_from_secs_f64` rejects overflow ("1e30s"); clamp to a sane ceiling.
    Some(Duration::try_from_secs_f64(secs).map_or(MAX_RETRY_DELAY, |d| d.min(MAX_RETRY_DELAY)))
}

impl EventDecoder for Decoder {
    fn feed(&mut self, payload: &str) -> Result<Vec<ChatChunk>, ProviderError> {
        let v: Value = serde_json::from_str(payload.trim())
            .map_err(|e| ProviderError::Stream(format!("invalid JSON chunk: {e}")))?;
        if let Some(code) = v.pointer("/error/code").and_then(Value::as_u64) {
            let status = u16::try_from(code).unwrap_or(500);
            return Err(classify(status, &HeaderMap::new(), payload));
        }
        if let Some(u) = v.get("usageMetadata") {
            self.usage = Some(parse_usage(u)?);
        }
        let mut out = Vec::new();
        // A blocked prompt has no candidates, only a block reason.
        if v.pointer("/promptFeedback/blockReason").is_some() {
            self.finish = Some(FinishReason::ContentFilter);
            self.finished = true;
        }
        let Some(cand) = v.pointer("/candidates/0") else {
            return Ok(out);
        };
        if let Some(parts) = cand.pointer("/content/parts").and_then(Value::as_array) {
            for p in parts {
                if let Some(fc) = p.get("functionCall") {
                    let name = fc
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    let args = fc
                        .get("args")
                        .map_or_else(|| "{}".to_owned(), Value::to_string);
                    self.arguments.charge(0, args.len())?;
                    let sig = p.get("thoughtSignature").and_then(Value::as_str);
                    let id = make_call_id(unique_id(self.calls.len()), sig);
                    out.push(ChatChunk::ToolCallDelta {
                        call_id: id.clone(),
                        name: Some(name.clone()),
                        input_delta: args.clone(),
                    });
                    self.calls.push(ToolCall {
                        id,
                        name,
                        arguments: args,
                        provider_extra: None,
                    });
                } else if let Some(t) = p.get("text").and_then(Value::as_str)
                    && !t.is_empty()
                    && p.get("thought").and_then(Value::as_bool) != Some(true)
                {
                    let visible = self.thoughts.push(t);
                    if !visible.is_empty() {
                        out.push(ChatChunk::TextDelta(visible));
                    }
                }
            }
        }
        if let Some(fr) = cand.get("finishReason").and_then(Value::as_str) {
            self.finish = Some(parse_finish(fr));
            self.finished = true;
        }
        Ok(out)
    }

    fn finish(&mut self) -> Result<Vec<ChatChunk>, ProviderError> {
        if !self.finished {
            return Err(ProviderError::Stream(
                "stream ended before a finish reason".into(),
            ));
        }
        if let Some(e) = self.finish.as_ref().and_then(finish_error) {
            return Err(e);
        }
        let mut out: Vec<ChatChunk> = Vec::new();
        let tail = self.thoughts.finish();
        if !tail.is_empty() {
            out.push(ChatChunk::TextDelta(tail));
        }
        out.extend(
            std::mem::take(&mut self.calls)
                .into_iter()
                .map(ChatChunk::ToolCall),
        );
        let has_calls = out.iter().any(|c| matches!(c, ChatChunk::ToolCall(_)));
        if let Some(u) = self.usage.take() {
            out.push(ChatChunk::Usage(u));
        }
        let reason = match self.finish.take() {
            // Gemini reports STOP even when it wants tools run.
            Some(FinishReason::Stop) | None if has_calls => FinishReason::ToolCalls,
            Some(r) => r,
            None => FinishReason::Stop,
        };
        out.push(ChatChunk::Done {
            finish_reason: reason,
        });
        Ok(out)
    }
}
