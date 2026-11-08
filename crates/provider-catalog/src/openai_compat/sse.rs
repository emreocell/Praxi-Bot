//! SSE decoding and assembly of `OpenAI` chat-completion chunks into [`ChatChunk`]s.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::{
    error::ProviderError,
    limits::{
        MAX_SSE_EVENT_BYTES, MAX_SSE_LINE_BYTES, MAX_SSE_TOTAL_BYTES, ToolArgumentBudget,
        checked_size,
    },
    provider::{ChatChunk, FinishReason, ToolCall, Usage},
    util::{ThoughtFilter, finish_error},
};

/// Incremental server-sent-events decoder. Yields the `data` payload of each
/// complete event; comments (`: keepalive`) and other fields are ignored.
#[derive(Debug, Default)]
pub(crate) struct SseDecoder {
    buf: Vec<u8>,
    data: String,
    has_data: bool,
    event_bytes: usize,
    total_bytes: usize,
}

impl SseDecoder {
    /// Consume only through the next event. The caller can stop at a terminal
    /// payload without parsing or buffering any following data in the same read.
    pub(crate) fn next_event(
        &mut self,
        bytes: &mut &[u8],
    ) -> Result<Option<String>, ProviderError> {
        while !bytes.is_empty() {
            let newline = bytes.iter().position(|&byte| byte == b'\n');
            let count = newline.map_or(bytes.len(), |pos| pos + 1);
            checked_size(self.buf.len(), count, MAX_SSE_LINE_BYTES, "SSE line")?;
            self.event_bytes =
                checked_size(self.event_bytes, count, MAX_SSE_EVENT_BYTES, "SSE event")?;
            self.total_bytes =
                checked_size(self.total_bytes, count, MAX_SSE_TOTAL_BYTES, "SSE stream")?;
            self.buf.extend_from_slice(&bytes[..count]);
            *bytes = &bytes[count..];
            if newline.is_some()
                && let Some(event) = self.line()?
            {
                return Ok(Some(event));
            }
        }
        Ok(None)
    }

    fn take_event(&mut self) -> Option<String> {
        if std::mem::take(&mut self.has_data) {
            Some(std::mem::take(&mut self.data))
        } else {
            None
        }
    }

    fn line(&mut self) -> Result<Option<String>, ProviderError> {
        let raw = String::from_utf8_lossy(&self.buf);
        let line = raw.trim_end_matches(['\n', '\r']);
        if line.is_empty() {
            self.buf.clear();
            self.event_bytes = 0;
            return Ok(self.take_event());
        }
        if let Some(rest) = line.strip_prefix("data:") {
            let rest = rest.strip_prefix(' ').unwrap_or(rest);
            // Lossy UTF-8 decoding can expand the raw bytes; also budget the
            // decoded payload and the newline inserted between data fields.
            checked_size(
                self.data.len(),
                rest.len() + usize::from(self.has_data),
                MAX_SSE_EVENT_BYTES,
                "SSE event data",
            )?;
            if self.has_data {
                self.data.push('\n');
            }
            self.data.push_str(rest);
            self.has_data = true;
        }
        self.buf.clear();
        Ok(None)
    }

    /// Flush a trailing event that was not terminated by a blank line.
    pub(crate) fn finish(&mut self) -> Result<Option<String>, ProviderError> {
        if !self.buf.is_empty()
            && let Some(event) = self.line()?
        {
            return Ok(Some(event));
        }
        Ok(self.take_event())
    }
}

#[derive(Debug, Default)]
struct PartialCall {
    id: String,
    name: String,
    arguments: String,
    /// The call's `extra_content` object (Google thought signature), echoed back on replay.
    extra: Option<Value>,
}

/// Accumulates deltas (index-based tool calls, usage, finish reason).
#[derive(Debug, Default)]
pub(super) struct Assembler {
    calls: BTreeMap<u64, PartialCall>,
    arguments: ToolArgumentBudget,
    finish: Option<FinishReason>,
    usage: Option<Usage>,
    thoughts: ThoughtFilter,
    /// Set once `[DONE]` or a finish reason has been seen.
    pub(super) finished: bool,
}

/// A finish reason is metadata; only an explicit `[DONE]` ends transport reads.
pub(super) enum Assembly {
    Chunks(Vec<ChatChunk>),
    Done,
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
    /// Slot for a streamed tool-call delta. Servers that omit `index` (some
    /// compat layers) are resolved by `id`: a new id opens a new call, a
    /// delta without id continues the most recent one.
    fn call_index(&self, c: &Value, pos: usize) -> Result<u64, ProviderError> {
        if let Some(i) = c.get("index").and_then(Value::as_u64) {
            return Ok(i);
        }
        match c.get("id").and_then(Value::as_str) {
            Some(id) if !id.is_empty() => {
                if let Some((index, _)) = self.calls.iter().find(|(_, call)| call.id == id) {
                    return Ok(*index);
                }
                // Only allocate a new slot for a new ID. A continuation of the last
                // valid slot (including u64::MAX) does not need to increment it.
                self.calls.keys().next_back().map_or(Ok(0), |index| {
                    index
                        .checked_add(1)
                        .ok_or_else(|| ProviderError::Stream("tool-call index exhausted".into()))
                })
            }
            _ => Ok(self.calls.keys().next_back().copied().unwrap_or(pos as u64)),
        }
    }

    /// Apply one JSON payload (a streamed chunk or a full non-stream response).
    pub(super) fn feed(&mut self, data: &str) -> Result<Assembly, ProviderError> {
        let data = data.trim();
        if data == "[DONE]" {
            self.finished = true;
            return Ok(Assembly::Done);
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
            return Ok(Assembly::Chunks(out));
        };
        // Streamed chunks use `delta`; non-streamed responses use `message`.
        let delta = choice.get("delta").or_else(|| choice.get("message"));
        if let Some(delta) = delta {
            // Gemma marks reasoning parts with `extra_content.google.thought`.
            let is_thought = delta
                .pointer("/extra_content/google/thought")
                .and_then(Value::as_bool)
                == Some(true);
            if let Some(text) = delta.get("content").and_then(Value::as_str)
                && !text.is_empty()
                && !is_thought
            {
                let visible = self.thoughts.push(text);
                if !visible.is_empty() {
                    out.push(ChatChunk::TextDelta(visible));
                }
            }
            if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for (pos, c) in calls.iter().enumerate() {
                    let idx = self.call_index(c, pos)?;
                    let p = self.calls.entry(idx).or_default();
                    if let Some(id) = c.get("id").and_then(Value::as_str)
                        && p.id.is_empty()
                    {
                        id.clone_into(&mut p.id);
                    }
                    if let Some(x) = c.get("extra_content")
                        && x.is_object()
                        && p.extra.is_none()
                    {
                        p.extra = Some(x.clone());
                    }
                    if let Some(f) = c.get("function") {
                        if let Some(n) = f.get("name").and_then(Value::as_str)
                            && p.name.is_empty()
                        {
                            n.clone_into(&mut p.name);
                        }
                        if let Some(a) = f.get("arguments").and_then(Value::as_str) {
                            self.arguments.charge(p.arguments.len(), a.len())?;
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
        Ok(Assembly::Chunks(out))
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
        let tail = self.thoughts.finish();
        if !tail.is_empty() {
            out.push(ChatChunk::TextDelta(tail));
        }
        if let Some(e) = self.finish.as_ref().and_then(finish_error) {
            return Err(e);
        }
        for (idx, p) in std::mem::take(&mut self.calls) {
            out.push(ChatChunk::ToolCall(ToolCall {
                id: if p.id.is_empty() {
                    format!("call_{idx}")
                } else {
                    p.id
                },
                name: p.name,
                arguments: p.arguments,
                provider_extra: p.extra,
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::limits::MAX_TOOL_ARGUMENT_BYTES;

    #[test]
    fn oversized_line_is_rejected_before_copying() {
        let mut decoder = SseDecoder::default();
        let bytes = vec![b'x'; MAX_SSE_LINE_BYTES + 1];
        let error = decoder.next_event(&mut bytes.as_slice()).unwrap_err();
        assert!(matches!(error, ProviderError::Stream(message) if message.contains("SSE line")));
        assert_eq!(decoder.buf, Vec::<u8>::new());
    }

    #[test]
    fn multiline_event_budget_applies_across_chunks() {
        let mut decoder = SseDecoder::default();
        let line = format!("data: {}\n", "x".repeat(MAX_SSE_LINE_BYTES / 2));
        for _ in 0..3 {
            assert!(decoder.next_event(&mut line.as_bytes()).unwrap().is_none());
        }
        let error = decoder.next_event(&mut line.as_bytes()).unwrap_err();
        assert!(matches!(error, ProviderError::Stream(message) if message.contains("SSE event")));
        assert!(decoder.data.len() < MAX_SSE_EVENT_BYTES);
    }

    #[test]
    fn total_stream_budget_includes_ignored_comments_and_empty_events() {
        let mut decoder = SseDecoder::default();
        let bytes = format!(":{}\n\n", "x".repeat(64 * 1024));
        let chunks = MAX_SSE_TOTAL_BYTES / bytes.len();
        for _ in 0..chunks {
            assert!(decoder.next_event(&mut bytes.as_bytes()).unwrap().is_none());
        }
        let error = decoder.next_event(&mut bytes.as_bytes()).unwrap_err();
        assert!(matches!(error, ProviderError::Stream(message) if message.contains("SSE stream")));
        assert_eq!(decoder.data, "");
    }

    #[test]
    fn split_tool_arguments_are_bounded_before_append() {
        let mut assembler = Assembler::default();
        let payload = json!({"choices": [{"delta": {"tool_calls": [{
            "index": 0, "function": {"arguments": "x".repeat(MAX_TOOL_ARGUMENT_BYTES / 2)}
        }]}}]})
        .to_string();
        assembler.feed(&payload).unwrap();
        assembler.feed(&payload).unwrap();
        let error = assembler.feed(&payload).err().unwrap();
        assert!(
            matches!(error, ProviderError::Stream(message) if message.contains("tool arguments"))
        );
        assert_eq!(assembler.calls[&0].arguments.len(), MAX_TOOL_ARGUMENT_BYTES);
    }
}
