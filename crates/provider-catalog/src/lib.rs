//! LLM provider plugin registry: trait definitions and built-in provider adapters.
//!
//! Abstracts over Anthropic, `OpenRouter`, `DeepSeek`, Ollama, and any
//! OpenAI-compatible endpoint through a uniform streaming provider trait.

pub mod anthropic;
pub mod catalog;
pub mod catalog_freshness;
pub mod config;
pub mod embedding;
pub mod entry;
pub mod error;
pub mod factory;
pub mod gemini;
pub mod hardware;
mod limits;
pub mod local_advice;
pub mod local_detect;
pub mod local_runtime;
pub mod model_metadata;
pub mod openai_compat;
pub mod provider;
pub mod selector;
pub mod setup;
mod util;

pub use anthropic::AnthropicProvider;
pub use catalog::Catalog;
pub use catalog_freshness::{
    AddedModel, CatalogDiff, ChangedModel, DEFAULT_STALE_AFTER_DAYS, DOCS_TABLE_BEGIN,
    DOCS_TABLE_END, FieldChange, IsoDate, LikelyRename, ProviderFreshness, RemovedModel,
    SnapshotModel, StalenessReport, ValidationReport, check_staleness, check_staleness_embedded,
    diff_provider, format_check_report, format_diff_report, load_catalog_entries_from_dir,
    parse_snapshot, render_docs_table, validate_catalog, verify_docs_table,
};
pub use config::{
    ConfigError, ConnectionReport, ProviderConfig, ProviderSettings, api_key_secret_name,
    config_path, test_connection,
};
pub use embedding::{
    EmbeddingInput, EmbeddingProvider, EmbeddingRequest, EmbeddingResponse, EmbeddingTask,
    EmbeddingTaskFormat,
};
pub use entry::{
    Auth, CapabilityOverrides, CompatFlags, ModelCapabilities, ModelInfo, ProviderEntry,
    ProviderKind,
};
pub use error::ProviderError;
pub use factory::{build_chat_provider, build_embedding_provider};
pub use gemini::GeminiProvider;
pub use hardware::{GpuInfo, HardwareInfo, probe_hardware};
pub use local_advice::{LocalAdvice, Recommendation, catalog_candidates, recommend};
pub use local_detect::{LocalServer, detect_local_servers};
pub use model_metadata::{
    CuratedModels, ModelKind, ModelMetadata, ProviderMetadata, embedded_metadata, model_metadata,
};
pub use openai_compat::{OpenAiCompatConfig, OpenAiCompatProvider};
pub use provider::{
    ChatChunk, ChatProvider, ChatRequest, FinishReason, Message, MessageRole, ModelRef, ToolCall,
    ToolChoice, Usage,
};
pub use selector::ModelSelector;
pub use setup::{ProviderChoice, SetupError, configure_provider, custom_entry, entry_for};
