//! LLM provider plugin registry: trait definitions and built-in provider adapters.
//!
//! Abstracts over Anthropic, `OpenRouter`, `DeepSeek`, Ollama, and any
//! OpenAI-compatible endpoint through a uniform streaming provider trait.

pub mod catalog;
pub mod entry;
pub mod error;
pub mod provider;
pub mod selector;

pub use catalog::Catalog;
pub use entry::{Auth, CompatFlags, ModelCapabilities, ProviderEntry, ProviderKind};
pub use error::ProviderError;
pub use provider::{ChatChunk, ChatProvider, ChatRequest, Message, MessageRole, ModelRef};
pub use selector::ModelSelector;
