//! Deterministic provider for tests and `bot serve --fake-provider`.
//!
//! * `call:<tool> <json args>` -> the model requests that tool call;
//! * after a tool result -> "tool said: <result>";
//! * anything else -> echoes the message word by word.

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use provider_catalog::{
    ChatChunk, ChatProvider, ChatRequest, FinishReason, MessageRole, ProviderError, ToolCall, Usage,
};

/// The fake provider (id `fake`).
#[derive(Debug, Default, Clone, Copy)]
pub struct FakeProvider;

fn usage() -> ChatChunk {
    ChatChunk::Usage(Usage {
        prompt_tokens: 10,
        completion_tokens: 5,
        cached_tokens: None,
    })
}

fn words(prefix: &str, text: &str) -> Vec<ChatChunk> {
    let mut v: Vec<ChatChunk> = format!("{prefix}{text}")
        .split_inclusive(' ')
        .map(|w| ChatChunk::TextDelta(w.to_owned()))
        .collect();
    v.push(usage());
    v.push(ChatChunk::Done {
        finish_reason: FinishReason::Stop,
    });
    v
}

#[async_trait]
impl ChatProvider for FakeProvider {
    fn id(&self) -> &'static str {
        "fake"
    }

    async fn stream(
        &self,
        req: ChatRequest,
    ) -> Result<BoxStream<'static, Result<ChatChunk, ProviderError>>, ProviderError> {
        let last = req
            .messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, MessageRole::User | MessageRole::Tool));
        let text = last
            .and_then(|m| m.content.as_str())
            .unwrap_or_default()
            .to_owned();
        let chunks = match last.map(|m| &m.role) {
            Some(MessageRole::Tool) => words("tool said: ", &text),
            _ => match text.strip_prefix("call:") {
                Some(rest) => {
                    let (name, args) = rest.split_once(' ').unwrap_or((rest, "{}"));
                    vec![
                        ChatChunk::ToolCall(ToolCall {
                            id: "fake-call-1".to_owned(),
                            name: name.to_owned(),
                            arguments: args.to_owned(),
                        }),
                        usage(),
                        ChatChunk::Done {
                            finish_reason: FinishReason::ToolCalls,
                        },
                    ]
                }
                None => words("echo: ", &text),
            },
        };
        Ok(futures::stream::iter(chunks.into_iter().map(Ok)).boxed())
    }
}
