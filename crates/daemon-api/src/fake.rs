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
        cache_write_tokens: None,
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
                            provider_extra: None,
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

/// A scripted in-memory desktop for demos and tests (`bot serve
/// --fake-provider`): one window "Fake App" with a row of buttons on screen;
/// actions are counted, never performed.
#[derive(Debug, Default)]
pub struct FakeDesktop {
    generation: std::sync::atomic::AtomicU64,
    /// Actions dispatched so far.
    pub actions: std::sync::atomic::AtomicUsize,
}

fn fake_window() -> desktop_api::WindowInfo {
    desktop_api::WindowInfo {
        id: 1,
        title: "Fake App".into(),
        pid: 4242,
        bounds: desktop_api::snapshot::Rect {
            x: 300.0,
            y: 200.0,
            width: 900.0,
            height: 500.0,
        },
        focused: true,
    }
}

#[async_trait]
impl desktop_api::DesktopBackend for FakeDesktop {
    async fn snapshot(
        &self,
        _scope: Option<u64>,
    ) -> Result<desktop_api::Snapshot, desktop_api::DesktopError> {
        use std::sync::atomic::Ordering;
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let nodes: Vec<desktop_api::snapshot::UiNode> = (0..6u32)
            .map(|i| {
                desktop_api::snapshot::UiNode::new(
                    i,
                    "button",
                    format!("Button {i}"),
                    None,
                    Some(desktop_api::snapshot::Rect {
                        x: 340.0 + f64::from(i) * 130.0,
                        y: 420.0,
                        width: 110.0,
                        height: 44.0,
                    }),
                    vec![],
                )
            })
            .collect();
        let quality = desktop_api::snapshot::TreeQuality::assess(&nodes);
        Ok(desktop_api::Snapshot {
            window: fake_window(),
            nodes,
            generation,
            quality,
        })
    }

    async fn act(
        &self,
        _action: desktop_api::Action,
    ) -> Result<desktop_api::ActionResult, desktop_api::DesktopError> {
        self.actions
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(desktop_api::ActionResult {
            dispatched: true,
            verified: None,
            new_snapshot: None,
        })
    }

    async fn screenshot(
        &self,
        _region: desktop_api::Region,
        _quality: desktop_api::Quality,
    ) -> Result<desktop_api::snapshot::Image, desktop_api::DesktopError> {
        Err(desktop_api::DesktopError::Unsupported(
            "the fake desktop has no screen".into(),
        ))
    }

    async fn windows(&self) -> Result<Vec<desktop_api::WindowInfo>, desktop_api::DesktopError> {
        Ok(vec![fake_window()])
    }

    async fn launch(
        &self,
        _app: desktop_api::snapshot::AppSpec,
    ) -> Result<desktop_api::snapshot::Pid, desktop_api::DesktopError> {
        Ok(4242)
    }

    fn capabilities(&self) -> desktop_api::Capabilities {
        desktop_api::Capabilities {
            platform: "fake".into(),
            accessibility_tree: true,
            screenshot: false,
            app_launch: true,
            has_display: true,
        }
    }
}
