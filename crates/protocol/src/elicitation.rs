//! Owner-visible, bounded MCP form metadata. It never contains submitted values.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{ChatId, RunId, ToolCallId};

/// Maximum number of fields in a form.
pub const MAX_FIELDS: usize = 12;
/// Maximum text input size in bytes (also enforced on the daemon).
pub const MAX_VALUE_BYTES: usize = 1024;
/// Form metadata and the lifetime of its one-shot owner decision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ElicitationRequested {
    pub run_id: RunId,
    pub call_id: ToolCallId,
    pub server: String,
    pub message: String,
    pub fields: Vec<ElicitationField>,
    pub expires_in_secs: u64,
}

/// A form has closed. Its answer is never part of the event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ElicitationClosed {
    pub run_id: RunId,
    pub call_id: ToolCallId,
}

/// A channel can see the existing chat link, even when it cannot read the form.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ElicitationLink {
    pub run_id: RunId,
    pub chat_id: ChatId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ElicitationField {
    pub name: String,
    pub label: String,
    pub description: Option<String>,
    pub required: bool,
    /// Suspected credential fields are displayed as blocked and can never be submitted.
    pub blocked: bool,
    pub kind: ElicitationFieldKind,
    pub min_length: Option<u32>,
    pub max_length: Option<u32>,
    pub format: Option<ElicitationFormat>,
    pub minimum: Option<f64>,
    pub maximum: Option<f64>,
    pub options: Vec<ElicitationOption>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ElicitationFieldKind {
    String,
    Number,
    Integer,
    Boolean,
    Enum,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ElicitationFormat {
    Email,
    Uri,
    Date,
    DateTime,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ElicitationOption {
    pub value: String,
    pub label: String,
}
