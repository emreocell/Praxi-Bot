//! LLM provider plugin registry: trait definitions and built-in provider adapters.
//!
//! Abstracts over Anthropic, `OpenRouter`, `DeepSeek`, Ollama, and any
//! OpenAI-compatible endpoint through a uniform streaming provider trait.

pub mod catalog;
pub mod config;
pub mod entry;
pub mod error;
pub mod openai_compat;
pub mod provider;
pub mod selector;

pub use catalog::Catalog;
pub use config::{
    ConfigError, ConnectionReport, ProviderConfig, ProviderSettings, api_key_secret_name,
    config_path, test_connection,
};
pub use entry::{Auth, CompatFlags, ModelCapabilities, ProviderEntry, ProviderKind};
pub use error::ProviderError;
pub use openai_compat::{OpenAiCompatConfig, OpenAiCompatProvider};
pub use provider::{
    ChatChunk, ChatProvider, ChatRequest, FinishReason, Message, MessageRole, ModelRef, ToolCall,
    ToolChoice, Usage,
};
pub use selector::ModelSelector;
