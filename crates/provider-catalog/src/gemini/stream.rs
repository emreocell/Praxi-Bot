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
    provider::{ChatChunk, FinishReason, ToolCall, Usage},
    util::EventDecoder,
};

#[derive(Debug, Default)]
pub(super) struct Decoder {
    calls: Vec<ToolCall>,
    usage: Option<Usage>,
    finish: Option<FinishReason>,
    finished: bool,
}

fn parse_finish(s: &str) -> FinishReason {
    match s {
        "STOP" => FinishReason::Stop,
        "MAX_TOKENS" => FinishReason::Length,
        "SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII" | "IMAGE_SAFETY"
        | "LANGUAGE" => FinishReason::ContentFilter,
        other => FinishReason::Other(other.to_owned()),
    }
}

fn parse_usage(u: &Value) -> Usage {
    let get = |k: &str| u.get(k).and_then(Value::as_u64);
    Usage {
        prompt_tokens: get("promptTokenCount").unwrap_or(0),
        // Thinking tokens are billed as output.
        completion_tokens: get("candidatesTokenCount").unwrap_or(0)
            + get("thoughtsTokenCount").unwrap_or(0),
        cached_tokens: get("cachedContentTokenCount"),
        cache_write_tokens: None,
    }
}

fn unique_id(index: usize) -> u64 {
    let mut h = std::hash::RandomState::new().build_hasher();
    h.write_usize(index);
    h.finish()
}

/// Gemini's `retryDelay` ("30s", "1.5s") from error details.
pub(super) fn retry_delay(body: &str) -> Option<Duration> {
    let v: Value = serde_json::from_str(body).ok()?;
    let details = v.pointer("/error/details")?.as_array()?;
    let s = details
        .iter()
        .find_map(|d| d.get("retryDelay").and_then(Value::as_str))?;
    let secs: f64 = s.strip_suffix('s')?.parse().ok()?;
    (secs.is_finite() && secs >= 0.0).then(|| Duration::from_secs_f64(secs))
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
            self.usage = Some(parse_usage(u));
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
                    });
                } else if let Some(t) = p.get("text").and_then(Value::as_str)
                    && !t.is_empty()
                    && p.get("thought").and_then(Value::as_bool) != Some(true)
                {
                    out.push(ChatChunk::TextDelta(t.to_owned()));
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
        let mut out: Vec<ChatChunk> = std::mem::take(&mut self.calls)
            .into_iter()
            .map(ChatChunk::ToolCall)
            .collect();
        let has_calls = !out.is_empty();
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
