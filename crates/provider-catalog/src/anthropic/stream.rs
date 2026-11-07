//! Decoding of Anthropic Messages SSE events into [`ChatChunk`]s.

use std::collections::BTreeMap;

use reqwest::header::HeaderMap;
use serde_json::Value;

use super::{classify, status_for_error_type};
use crate::{
    error::ProviderError,
    provider::{ChatChunk, FinishReason, ToolCall, Usage},
    util::EventDecoder,
};

/// A thinking block kept from the stream (not shown as answer text).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(super) struct ThinkingBlock {
    pub(super) text: String,
    pub(super) signature: String,
    /// Opaque payload of a `redacted_thinking` block.
    pub(super) redacted: Option<String>,
}

#[derive(Debug)]
enum Block {
    Text,
    Tool {
        id: String,
        name: String,
        args: String,
    },
    Thinking(ThinkingBlock),
    Other,
}

#[derive(Debug, Default)]
struct UsageAcc {
    seen: bool,
    input: u64,
    cache_read: Option<u64>,
    cache_write: Option<u64>,
    output: u64,
}

impl UsageAcc {
    /// Counts are cumulative: later events overwrite earlier ones.
    fn absorb(&mut self, u: &Value) {
        let get = |k: &str| u.get(k).and_then(Value::as_u64);
        self.seen = true;
        if let Some(n) = get("input_tokens") {
            self.input = n;
        }
        if let Some(n) = get("cache_read_input_tokens") {
            self.cache_read = Some(n);
        }
        if let Some(n) = get("cache_creation_input_tokens") {
            self.cache_write = Some(n);
        }
        if let Some(n) = get("output_tokens") {
            self.output = n;
        }
    }

    fn usage(&self) -> Usage {
        Usage {
            prompt_tokens: self.input
                + self.cache_read.unwrap_or(0)
                + self.cache_write.unwrap_or(0),
            completion_tokens: self.output,
            cached_tokens: self.cache_read,
            cache_write_tokens: self.cache_write,
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct Decoder {
    blocks: BTreeMap<u64, Block>,
    usage: UsageAcc,
    finish: Option<FinishReason>,
    finished: bool,
}

fn parse_finish(s: &str) -> FinishReason {
    match s {
        "end_turn" | "stop_sequence" => FinishReason::Stop,
        "max_tokens" | "model_context_window_exceeded" => FinishReason::Length,
        "tool_use" => FinishReason::ToolCalls,
        "refusal" => FinishReason::ContentFilter,
        other => FinishReason::Other(other.to_owned()),
    }
}

fn stream_error(v: &Value) -> ProviderError {
    let e = v.get("error").unwrap_or(v);
    let status = e
        .get("type")
        .and_then(Value::as_str)
        .map_or(500, status_for_error_type);
    classify(status, &HeaderMap::new(), &v.to_string())
}

impl Decoder {
    /// Thinking blocks seen so far, in stream order.
    #[cfg(test)]
    pub(super) fn thinking(&self) -> Vec<ThinkingBlock> {
        self.blocks
            .values()
            .filter_map(|b| match b {
                Block::Thinking(t) => Some(t.clone()),
                _ => None,
            })
            .collect()
    }

    fn block_start(&mut self, v: &Value, out: &mut Vec<ChatChunk>) {
        let idx = v.get("index").and_then(Value::as_u64).unwrap_or(0);
        let Some(cb) = v.get("content_block") else {
            return;
        };
        let s = |k: &str| {
            cb.get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let block = match cb.get("type").and_then(Value::as_str) {
            Some("text") => {
                let t = s("text");
                if !t.is_empty() {
                    out.push(ChatChunk::TextDelta(t));
                }
                Block::Text
            }
            Some("tool_use") => {
                let (id, name) = (s("id"), s("name"));
                out.push(ChatChunk::ToolCallDelta {
                    call_id: id.clone(),
                    name: Some(name.clone()),
                    input_delta: String::new(),
                });
                Block::Tool {
                    id,
                    name,
                    args: String::new(),
                }
            }
            Some("thinking") => Block::Thinking(ThinkingBlock {
                text: s("thinking"),
                ..ThinkingBlock::default()
            }),
            Some("redacted_thinking") => Block::Thinking(ThinkingBlock {
                redacted: Some(s("data")),
                ..ThinkingBlock::default()
            }),
            _ => Block::Other,
        };
        self.blocks.insert(idx, block);
    }

    fn block_delta(&mut self, v: &Value, out: &mut Vec<ChatChunk>) {
        let idx = v.get("index").and_then(Value::as_u64).unwrap_or(0);
        let Some(d) = v.get("delta") else { return };
        let str_of = |k: &str| d.get(k).and_then(Value::as_str).unwrap_or_default();
        match (
            d.get("type").and_then(Value::as_str),
            self.blocks.get_mut(&idx),
        ) {
            (Some("text_delta"), _) => {
                let t = str_of("text");
                if !t.is_empty() {
                    out.push(ChatChunk::TextDelta(t.to_owned()));
                }
            }
            (Some("input_json_delta"), Some(Block::Tool { id, args, .. })) => {
                let part = str_of("partial_json");
                args.push_str(part);
                out.push(ChatChunk::ToolCallDelta {
                    call_id: id.clone(),
                    name: None,
                    input_delta: part.to_owned(),
                });
            }
            (Some("thinking_delta"), Some(Block::Thinking(t))) => {
                t.text.push_str(str_of("thinking"));
            }
            (Some("signature_delta"), Some(Block::Thinking(t))) => {
                t.signature.push_str(str_of("signature"));
            }
            _ => {}
        }
    }
}

impl EventDecoder for Decoder {
    fn feed(&mut self, payload: &str) -> Result<Vec<ChatChunk>, ProviderError> {
        let v: Value = serde_json::from_str(payload.trim())
            .map_err(|e| ProviderError::Stream(format!("invalid JSON event: {e}")))?;
        let mut out = Vec::new();
        match v.get("type").and_then(Value::as_str) {
            Some("message_start") => {
                if let Some(u) = v.pointer("/message/usage") {
                    self.usage.absorb(u);
                }
            }
            Some("content_block_start") => self.block_start(&v, &mut out),
            Some("content_block_delta") => self.block_delta(&v, &mut out),
            Some("message_delta") => {
                if let Some(r) = v.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    self.finish = Some(parse_finish(r));
                }
                if let Some(u) = v.get("usage") {
                    self.usage.absorb(u);
                }
            }
            Some("message_stop") => self.finished = true,
            Some("error") => return Err(stream_error(&v)),
            // ping, content_block_stop, unknown future events.
            _ => {}
        }
        Ok(out)
    }

    fn finish(&mut self) -> Result<Vec<ChatChunk>, ProviderError> {
        if !self.finished {
            return Err(ProviderError::Stream(
                "stream ended before message_stop".into(),
            ));
        }
        let mut out = Vec::new();
        for block in std::mem::take(&mut self.blocks).into_values() {
            if let Block::Tool { id, name, args } = block {
                out.push(ChatChunk::ToolCall(ToolCall {
                    id,
                    name,
                    arguments: if args.trim().is_empty() {
                        "{}".into()
                    } else {
                        args
                    },
                    provider_extra: None,
                }));
            }
        }
        if self.usage.seen {
            out.push(ChatChunk::Usage(self.usage.usage()));
        }
        let reason = self.finish.take().unwrap_or_else(|| {
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
