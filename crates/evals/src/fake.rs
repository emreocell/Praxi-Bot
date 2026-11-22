//! Scripted fake provider and fake tools for hermetic runs.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use parking_lot::Mutex;
use provider_catalog::{
    ChatChunk, ChatProvider, ChatRequest, FinishReason, ProviderError, ToolCall, Usage,
};
use serde_json::{Value, json};
use tool_registry::{
    Effect, PermissionCategory, Platform, Tool, ToolContext, ToolError, ToolOutput, ToolSpec,
};

use crate::scenario::{CategorySpec, ContentSpec, EffectSpec, FakeToolSpec, ScriptStep};

fn content_class(s: &FakeToolSpec) -> tool_registry::ContentClass {
    use tool_registry::ContentClass as C;
    let class = match s.content {
        ContentSpec::None => C::none(),
        ContentSpec::PrivateRead => C::private_read(),
        ContentSpec::Untrusted => C::untrusted(),
        ContentSpec::UntrustedPrivate => C::untrusted_private(),
        ContentSpec::Egress => C::egress(),
        ContentSpec::UntrustedEgress => C::untrusted_egress(),
        ContentSpec::All => C::all(),
    };
    if s.executes { class.executes() } else { class }
}

/// A provider that replays scripted replies and records every request.
pub struct ScriptedProvider {
    id: &'static str,
    replies: Mutex<VecDeque<ScriptStep>>,
    /// Keep answering with the last reply once the script is exhausted.
    sticky: Mutex<Option<ScriptStep>>,
    repeat_last: bool,
    requests: Mutex<Vec<ChatRequest>>,
}

impl ScriptedProvider {
    /// A provider that fails with "script exhausted" after the last reply.
    #[must_use]
    pub fn new(script: Vec<ScriptStep>) -> Arc<Self> {
        Arc::new(Self {
            id: "fake",
            replies: Mutex::new(script.into()),
            sticky: Mutex::new(None),
            repeat_last: false,
            requests: Mutex::new(Vec::new()),
        })
    }

    /// A provider that repeats the last reply forever (judge / structure runs).
    #[must_use]
    pub fn repeating(script: Vec<ScriptStep>) -> Arc<Self> {
        Arc::new(Self {
            id: "fake",
            replies: Mutex::new(script.into()),
            sticky: Mutex::new(None),
            repeat_last: true,
            requests: Mutex::new(Vec::new()),
        })
    }

    /// A one-reply text provider.
    #[must_use]
    pub fn text(text: &str) -> Arc<Self> {
        Self::repeating(vec![ScriptStep {
            text: Some(text.to_owned()),
            ..ScriptStep::default()
        }])
    }

    /// Every request received so far.
    #[must_use]
    pub fn requests(&self) -> Vec<ChatRequest> {
        self.requests.lock().clone()
    }

    fn next(&self) -> Option<ScriptStep> {
        let mut q = self.replies.lock();
        if let Some(s) = q.pop_front() {
            if self.repeat_last && q.is_empty() {
                *self.sticky.lock() = Some(s.clone());
            }
            return Some(s);
        }
        self.sticky.lock().clone()
    }
}

fn chunks(step: &ScriptStep) -> Vec<ChatChunk> {
    let mut v = Vec::new();
    if let Some(t) = &step.text {
        v.push(ChatChunk::TextDelta(t.clone()));
    }
    for c in &step.tool_calls {
        v.push(ChatChunk::ToolCall(ToolCall {
            id: c.id.clone(),
            // The model sees dotted names with `.` replaced.
            name: c.tool.replace('.', "__"),
            arguments: c.raw_args.clone().unwrap_or_else(|| {
                if c.args.is_null() {
                    "{}".into()
                } else {
                    c.args.to_string()
                }
            }),
            provider_extra: None,
        }));
    }
    let u = step.usage.map_or(
        Usage {
            prompt_tokens: 10,
            completion_tokens: 5,
            cached_tokens: None,
            cache_write_tokens: None,
        },
        |u| Usage {
            prompt_tokens: u.prompt,
            completion_tokens: u.completion,
            cached_tokens: u.cached,
            cache_write_tokens: None,
        },
    );
    v.push(ChatChunk::Usage(u));
    v.push(ChatChunk::Done {
        finish_reason: if step.tool_calls.is_empty() {
            FinishReason::Stop
        } else {
            FinishReason::ToolCalls
        },
    });
    v
}

#[async_trait]
impl ChatProvider for ScriptedProvider {
    fn id(&self) -> &str {
        self.id
    }

    async fn stream(
        &self,
        req: ChatRequest,
    ) -> Result<BoxStream<'static, Result<ChatChunk, ProviderError>>, ProviderError> {
        self.requests.lock().push(req);
        let Some(step) = self.next() else {
            return Err(ProviderError::InvalidRequest("script exhausted".into()));
        };
        if let Some(e) = &step.error {
            return Err(ProviderError::InvalidRequest(e.clone()));
        }
        if step.hang {
            return Ok(futures::stream::pending().boxed());
        }
        Ok(futures::stream::iter(chunks(&step).into_iter().map(Ok)).boxed())
    }
}

/// A fake tool with a fixed output that counts its executions.
pub struct FakeTool {
    spec: ToolSpec,
    always_ask: bool,
    runs: Arc<AtomicU64>,
    finished: Arc<AtomicU64>,
    output: String,
    delay: Duration,
    error: Option<String>,
}

impl FakeTool {
    /// Build a fake tool from its scenario spec; the returned counter tracks executions.
    #[must_use]
    pub fn from_spec(s: &FakeToolSpec) -> (Arc<Self>, Arc<AtomicU64>) {
        let (tool, runs, _) = Self::from_spec_counted(s);
        (tool, runs)
    }

    /// Like [`Self::from_spec`], plus a counter of executions that ran to the end.
    #[must_use]
    pub fn from_spec_counted(s: &FakeToolSpec) -> (Arc<Self>, Arc<AtomicU64>, Arc<AtomicU64>) {
        let runs = Arc::new(AtomicU64::new(0));
        let finished = Arc::new(AtomicU64::new(0));
        let output = s
            .output_bytes
            .map_or_else(|| s.output.clone(), |n| "x".repeat(n));
        let declared_effect = match s.effect {
            EffectSpec::Read => Effect::Read,
            EffectSpec::Write => Effect::Write,
            EffectSpec::Destructive => Effect::Destructive,
        };
        let (content, effect, always_ask) = if s.browser_mcp {
            browser_rules(&s.name)
        } else {
            (content_class(s), declared_effect, false)
        };
        let tool = Arc::new(Self {
            spec: ToolSpec {
                name: s.name.clone(),
                description: s
                    .description
                    .clone()
                    .unwrap_or_else(|| format!("fake tool {}", s.name)),
                input_schema: json!({"type": "object"}),
                effect,
                category: match s.category {
                    CategorySpec::Fs => PermissionCategory::Fs,
                    CategorySpec::Shell => PermissionCategory::Shell,
                    CategorySpec::Desktop => PermissionCategory::Desktop,
                    CategorySpec::Browser => PermissionCategory::Browser,
                    CategorySpec::Money => PermissionCategory::Money,
                    CategorySpec::Messaging => PermissionCategory::Messaging,
                    CategorySpec::Script => PermissionCategory::Script,
                    CategorySpec::Accounts => PermissionCategory::Accounts,
                    CategorySpec::System => PermissionCategory::System,
                    CategorySpec::Remote => PermissionCategory::Remote,
                    CategorySpec::Meta => PermissionCategory::Meta,
                },
                platforms: vec![Platform::All],

                content: if s.name.starts_with("mcp:") && s.description.is_some() {
                    content.with_external_metadata()
                } else {
                    content
                },
            },
            always_ask,
            runs: Arc::clone(&runs),
            finished: Arc::clone(&finished),
            output,
            delay: Duration::from_millis(s.delay_ms),
            error: s.error.clone(),
        });
        (tool, runs, finished)
    }
}

/// The class an owner-connected browser server's tool gets from the MCP
/// bridge (same rules), or the generic MCP class for a name they do not know.
fn browser_rules(name: &str) -> (tool_registry::ContentClass, Effect, bool) {
    use tool_registry::browser_mcp;
    let remote = name.rsplit('/').next().unwrap_or(name);
    browser_mcp::rules(browser_mcp::classify(remote), false, false).unwrap_or((
        tool_registry::ContentClass::declared(true, true, true),
        Effect::Destructive,
        false,
    ))
}

#[async_trait]
impl Tool for FakeTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn always_ask(&self) -> bool {
        self.always_ask
    }

    async fn approval_details(
        &self,
        _: &ToolContext,
        input: &Value,
    ) -> Result<Option<Value>, ToolError> {
        Ok(self
            .always_ask
            .then(|| json!({"kind": "mcp_call", "tool": self.spec.name, "arguments": input})))
    }

    async fn call(&self, _: &ToolContext, _: Value) -> Result<ToolOutput, ToolError> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        self.finished.fetch_add(1, Ordering::SeqCst);
        match &self.error {
            Some(e) => Err(ToolError::Execution(e.clone())),
            None => Ok(ToolOutput::text(self.output.clone())),
        }
    }
}
