//! Client-stream event types.
//!
//! The agent core emits these events over the SSE / WebSocket stream.
//! Clients consume them to build live UI.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ids::{BotId, ChatId, RunId, ToolCallId, UserId};

// ── Run lifecycle ─────────────────────────────────────────────────────────────

/// A run has started.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RunStarted {
    pub run_id: RunId,
    pub chat_id: ChatId,
    pub bot_id: BotId,
}

/// A run has completed (successfully or not).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RunFinished {
    pub run_id: RunId,
    /// `None` on success, an error message on failure.
    pub error: Option<String>,
}

// ── Message streaming ─────────────────────────────────────────────────────────

/// A partial text chunk from the assistant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MessageDelta {
    pub run_id: RunId,
    /// Incremental text to append to the current assistant message.
    pub text: String,
}

// ── Tool calls ────────────────────────────────────────────────────────────────

/// The agent has requested a tool call; waiting for approval or execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ToolCallRequested {
    pub run_id: RunId,
    pub call_id: ToolCallId,
    pub tool_name: String,
    pub input: Value,
}

/// A human-approval gate is waiting for the user to respond.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ApprovalNeeded {
    pub run_id: RunId,
    pub call_id: ToolCallId,
    pub tool_name: String,
    pub input: Value,
    /// A human-readable summary of what will happen.
    pub summary: String,
}

/// The user approved a pending tool call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ToolCallApproved {
    pub run_id: RunId,
    pub call_id: ToolCallId,
    pub approved_by: UserId,
}

/// The user denied a pending tool call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ToolCallDenied {
    pub run_id: RunId,
    pub call_id: ToolCallId,
    pub denied_by: UserId,
    pub reason: Option<String>,
}

/// A tool call has completed; output is available.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ToolCallCompleted {
    pub run_id: RunId,
    pub call_id: ToolCallId,
    /// `None` on success, an error message on failure.
    pub error: Option<String>,
}

// ── Errors ────────────────────────────────────────────────────────────────────

/// A non-fatal error occurred during the run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RunError {
    pub run_id: RunId,
    pub message: String,
    pub recoverable: bool,
}

// ── Top-level event enum ──────────────────────────────────────────────────────

/// All events that can appear in the client event stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    RunStarted(RunStarted),
    RunFinished(RunFinished),
    MessageDelta(MessageDelta),
    ToolCallRequested(ToolCallRequested),
    ApprovalNeeded(ApprovalNeeded),
    ToolCallApproved(ToolCallApproved),
    ToolCallDenied(ToolCallDenied),
    ToolCallCompleted(ToolCallCompleted),
    Error(RunError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_tag_round_trip() {
        let ev = Event::RunStarted(RunStarted {
            run_id: RunId::new(),
            chat_id: ChatId::new(),
            bot_id: BotId::new(),
        });
        let json = serde_json::to_string(&ev).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(ev, back);
    }

    #[test]
    fn event_type_field_matches_variant() {
        let ev = Event::MessageDelta(MessageDelta {
            run_id: RunId::new(),
            text: "hello".into(),
        });
        let v = serde_json::to_value(&ev).unwrap();
        assert_eq!(v["type"], "message_delta");
    }
}
