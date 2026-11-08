//! The `Tool` trait and associated output types.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{error::ToolError, spec::ToolSpec};

/// A reference to a spilled (large) tool output written to a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpillRef {
    /// Absolute path to the spill file on the node running the tool.
    pub path: String,
    /// MIME type of the spill file contents.
    pub mime_type: String,
    /// Byte size of the spill file.
    pub size_bytes: u64,
}

/// A single content part in a tool's output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    /// Plain text output.
    Text { text: String },
    /// Structured JSON data.
    Json { data: Value },
    /// A reference to binary data (e.g. an image).
    Binary { mime_type: String, base64: String },
}

/// The result of a successful tool call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutput {
    /// The content parts returned by the tool.
    ///
    /// For most tools this contains a single text or JSON part.
    pub content: Vec<ContentPart>,
    /// If the full output exceeded the in-memory budget, the remainder was
    /// written to a temporary file and a pointer is returned here.
    pub spill: Option<SpillRef>,
}

impl ToolOutput {
    /// Create a simple text output with no spill.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![ContentPart::Text { text: text.into() }],
            spill: None,
        }
    }
}

/// Context supplied to every tool call.
#[derive(Debug)]
pub struct ToolContext {
    /// The run that invoked the tool.
    pub run_id: String,
    /// Maximum number of bytes of output to retain in memory before spilling.
    pub max_inline_bytes: usize,
    /// Bot that invoked the tool. `None` when the caller is not a bot.
    ///
    /// MCP servers with a `bots` allowlist refuse a call whose id is missing
    /// or not listed. `None` here is not "every bot".
    pub bot_id: Option<String>,
}

impl Default for ToolContext {
    fn default() -> Self {
        Self {
            run_id: String::new(),
            max_inline_bytes: 1024 * 1024, // 1 MiB
            bot_id: None,
        }
    }
}

/// A callable tool.
///
/// Implementors describe themselves via [`Tool::spec`] and execute via
/// [`Tool::call`].  All implementations must be `Send + Sync` so they can be
/// shared across async tasks.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Return the static specification for this tool.
    fn spec(&self) -> &ToolSpec;

    /// Execute the tool with the given `input` and return its output.
    async fn call(&self, ctx: &ToolContext, input: Value) -> Result<ToolOutput, ToolError>;
}
