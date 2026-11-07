//! SSE decoding and assembly of `OpenAI` chat-completion chunks into [`ChatChunk`]s.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::{
    error::ProviderError,
    provider::{ChatChunk, FinishReason, ToolCall, Usage},
};

/// Incremental server-sent-events decoder. Yields the `data` payload of each
/// complete event; comments (`: keepalive`) and other fields are ignored.
#[derive(Debug, Default)]
pub(crate) struct SseDecoder {
    buf: Vec<u8>,
    data: Vec<String>,
}

impl SseDecoder {
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(bytes);
        let mut events = Vec::new();
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let raw: Vec<u8> = self.buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&raw);
            let line = line.trim_end_matches(['\n', '\r']);
            if line.is_empty() {
                if !self.data.is_empty() {
                    events.push(std::mem::take(&mut self.data).join("\n"));
                }
            } else if let Some(rest) = line.strip_prefix("data:") {
                self.data
                    .push(rest.strip_prefix(' ').unwrap_or(rest).to_owned());
            }
        }
        events
    }

    /// Flush a trailing event that was not terminated by a blank line.
    pub(crate) fn finish(&mut self) -> Option<String> {
        if !self.buf.is_empty() {
            let rest = std::mem::take(&mut self.buf);
            let line = String::from_utf8_lossy(&rest);
            if let Some(d) = line.trim_end_matches(['\n', '\r']).strip_prefix("data:") {
                self.data.push(d.strip_prefix(' ').unwrap_or(d).to_owned());
            }
        }
        if self.data.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.data).join("\n"))
        }
    }
}

#[derive(Debug, Default)]
struct PartialCall {
    id: String,
    name: String,
    arguments: String,
}

/// Accumulates deltas (index-based tool calls, usage, finish reason).
#[derive(Debug, Default)]
pub(super) struct Assembler {
    calls: BTreeMap<u64, PartialCall>,
    finish: Option<FinishReason>,
    usage: Option<Usage>,
    /// Set once `[DONE]` or a finish reason has been seen.
    pub(super) finished: bool,
}

fn parse_finish(s: &str) -> FinishReason {
    match s {
        "stop" => FinishReason::Stop,
        "length" => FinishReason::Length,
        "tool_calls" | "function_call" => FinishReason::ToolCalls,
        "content_filter" => FinishReason::ContentFilter,
        other => FinishReason::Other(other.to_owned()),
    }
}

fn parse_usage(u: &Value) -> Option<Usage> {
    if !u.is_object() {
        return None;
    }
    let get = |k: &str| u.get(k).and_then(Value::as_u64);
    let cached = u
        .pointer("/prompt_tokens_details/cached_tokens")
        .and_then(Value::as_u64)
        // DeepSeek reports cache hits at the top level.
        .or_else(|| get("prompt_cache_hit_tokens"));
    Some(Usage {
        prompt_tokens: get("prompt_tokens").unwrap_or(0),
        completion_tokens: get("completion_tokens").unwrap_or(0),
        cached_tokens: cached,
        cache_write_tokens: None,
    })
}

fn error_message(v: &Value) -> Option<String> {
    let e = v.get("error")?;
    Some(
        e.get("message")
            .and_then(Value::as_str)
            .map_or_else(|| e.to_string(), str::to_owned),
    )
}

impl Assembler {
    /// Apply one JSON payload (a streamed chunk or a full non-stream response).
    pub(super) fn feed(&mut self, data: &str) -> Result<Vec<ChatChunk>, ProviderError> {
        let data = data.trim();
        if data == "[DONE]" {
            self.finished = true;
            return Ok(Vec::new());
        }
        let v: Value = serde_json::from_str(data)
            .map_err(|e| ProviderError::Stream(format!("invalid JSON chunk: {e}")))?;
        if let Some(msg) = error_message(&v) {
            return Err(ProviderError::Stream(format!("provider error: {msg}")));
        }
        let mut out = Vec::new();
        if let Some(u) = v.get("usage").and_then(parse_usage) {
            self.usage = Some(u);
        }
        let Some(choice) = v.pointer("/choices/0") else {
            return Ok(out);
        };
        // Streamed chunks use `delta`; non-streamed responses use `message`.
        let delta = choice.get("delta").or_else(|| choice.get("message"));
        if let Some(delta) = delta {
            if let Some(text) = delta.get("content").and_then(Value::as_str)
                && !text.is_empty()
            {
                out.push(ChatChunk::TextDelta(text.to_owned()));
            }
            if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for (pos, c) in calls.iter().enumerate() {
                    let idx = c.get("index").and_then(Value::as_u64).unwrap_or(pos as u64);
                    let p = self.calls.entry(idx).or_default();
                    if let Some(id) = c.get("id").and_then(Value::as_str)
                        && p.id.is_empty()
                    {
                        id.clone_into(&mut p.id);
                    }
                    if let Some(f) = c.get("function") {
                        if let Some(n) = f.get("name").and_then(Value::as_str)
                            && p.name.is_empty()
                        {
                            n.clone_into(&mut p.name);
                        }
                        if let Some(a) = f.get("arguments").and_then(Value::as_str) {
                            p.arguments.push_str(a);
                        }
                    }
                }
            }
        }
        if let Some(fr) = choice.get("finish_reason").and_then(Value::as_str) {
            self.finish = Some(parse_finish(fr));
            self.finished = true;
        }
        Ok(out)
    }

    /// Emit assembled tool calls, usage and `Done`. Errors if the stream ended
    /// without any terminator (truncated).
    pub(super) fn finish(&mut self) -> Result<Vec<ChatChunk>, ProviderError> {
        if !self.finished {
            return Err(ProviderError::Stream(
                "stream ended before a finish reason or [DONE]".into(),
            ));
        }
        let mut out = Vec::new();
        for (idx, p) in std::mem::take(&mut self.calls) {
            out.push(ChatChunk::ToolCall(ToolCall {
                id: if p.id.is_empty() {
                    format!("call_{idx}")
                } else {
                    p.id
                },
                name: p.name,
                arguments: p.arguments,
            }));
        }
        if let Some(u) = self.usage.take() {
            out.push(ChatChunk::Usage(u));
        }
        let reason = self.finish.take().unwrap_or_else(|| {
            // Servers that omit finish_reason: infer from the calls we emitted.
            if out.iter().any(|c| matches!(c, ChatChunk::ToolCall(_))) {
                FinishReason::ToolCalls
            } else {
                FinishReason::Stop
            }
        });
        out.push(ChatChunk::Done {
            finish_reason: reason,
        });
        Ok(out)
    }
}
