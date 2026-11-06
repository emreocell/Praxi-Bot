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
    pub content: Value,
}

impl Message {
    /// Construct a simple text message.
    pub fn text(role: MessageRole, text: impl Into<String>) -> Self {
        Self {
            role,
            content: Value::String(text.into()),
        }
    }
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
    /// Tool definitions available in this request.
    pub tools: Vec<Value>,
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
    /// The stream has finished.
    Done,
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
}
