//! The `ChatProvider` trait and associated message types.

use async_trait::async_trait;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::ProviderError;

/// A fully-qualified reference to a model, optionally pinned to a provider.
///
/// Format: `"<provider>/<model>"` (e.g. `"openrouter/gpt-4o"`) or just
/// `"<model>"` when the provider is determined by the selector.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ModelRef(pub String);

impl ModelRef {
    /// Return the provider part (before the first `/`), if present.
    #[must_use]
    pub fn provider(&self) -> Option<&str> {
        self.0.split_once('/').map(|(p, _)| p)
    }

    /// Return the model name part (after the first `/`, or the whole string).
    #[must_use]
    pub fn model(&self) -> &str {
        self.0.split_once('/').map_or(self.0.as_str(), |(_, m)| m)
    }
}

impl std::fmt::Display for ModelRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Role of a message in a conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

/// A single message in a conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: MessageRole,
    /// Either a JSON string, or an array of OpenAI-style content parts
    /// (`{"type":"text","text":..}` / `{"type":"image_url","image_url":{"url":..}}`).
    /// `Null` is allowed for assistant messages that only carry tool calls.
    pub content: Value,
    /// Tool calls requested by an assistant message.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// For `Tool` messages: the id of the call this message answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    /// Construct a simple text message.
    pub fn text(role: MessageRole, text: impl Into<String>) -> Self {
        Self {
            role,
            content: Value::String(text.into()),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    /// Construct a tool-result message answering `tool_call_id`.
    pub fn tool_result(tool_call_id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            role: MessageRole::Tool,
            content: Value::String(text.into()),
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call_id.into()),
        }
    }
}

/// A complete tool call requested by the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    /// Provider-assigned call id (synthesised when the provider omits it).
    pub id: String,
    /// Tool name.
    pub name: String,
    /// Raw JSON arguments exactly as the model produced them (may be invalid
    /// JSON; validate before use).
    pub arguments: String,
}

/// How the model may choose among the offered tools.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolChoice {
    /// The model decides (default).
    Auto,
    /// The model must not call tools.
    None,
    /// The model must call some tool.
    Required,
    /// The model must call the named tool.
    Function(String),
}

/// Why generation stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    /// Any provider-specific value.
    Other(String),
}

/// Token accounting for a completion.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    /// Prompt tokens served from the provider's cache, when reported.
    pub cached_tokens: Option<u64>,
}

/// A chat completion request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequest {
    /// Target model (provider + model name).
    pub model: ModelRef,
    /// The conversation so far.
    pub messages: Vec<Message>,
    /// Maximum tokens to generate.
    pub max_tokens: Option<u32>,
    /// Sampling temperature (0.0–2.0).
    pub temperature: Option<f64>,
    /// Tool definitions available in this request, in `OpenAI` format:
    /// `{"type":"function","function":{"name","description","parameters"}}`.
    #[serde(default)]
    pub tools: Vec<Value>,
    /// Tool selection policy; `None` leaves it to the provider default.
    #[serde(default)]
    pub tool_choice: Option<ToolChoice>,
    /// Request JSON-object output (honoured only if the model has `json_mode`).
    #[serde(default)]
    pub json_mode: bool,
}

impl ChatRequest {
    /// A request with no tools, no limits and default sampling.
    #[must_use]
    pub fn new(model: ModelRef, messages: Vec<Message>) -> Self {
        Self {
            model,
            messages,
            max_tokens: None,
            temperature: None,
            tools: Vec::new(),
            tool_choice: None,
            json_mode: false,
        }
    }
}

/// A single streaming chunk from a chat completion.
#[derive(Debug, Clone)]
pub enum ChatChunk {
    /// A partial text delta.
    TextDelta(String),
    /// A tool-call delta (partial JSON for the input field).
    ToolCallDelta {
        call_id: String,
        name: Option<String>,
        input_delta: String,
    },
    /// A complete tool call, assembled from the provider's streamed deltas.
    /// Emitted after all text, in call-index order.
    ToolCall(ToolCall),
    /// Token usage, when the provider reports it (at most once, before `Done`).
    Usage(Usage),
    /// The stream has finished.
    Done { finish_reason: FinishReason },
}

/// A streaming LLM chat provider.
///
/// Implementations live in OS/provider-specific crates; this is the trait only.
#[async_trait]
pub trait ChatProvider: Send + Sync {
    /// Return the provider's catalog identifier (e.g. `"openrouter"`).
    fn id(&self) -> &str;

    /// Send a chat request and return a streaming response.
    async fn stream(
        &self,
        req: ChatRequest,
    ) -> Result<BoxStream<'static, Result<ChatChunk, ProviderError>>, ProviderError>;

    /// List the model ids the provider offers. Providers without a listing
    /// endpoint return an empty list.
    async fn list_models(&self) -> Result<Vec<String>, ProviderError> {
        Ok(Vec::new())
    }
}
