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

// ── Plan / todo ───────────────────────────────────────────────────────────────

/// Progress of one todo item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Done,
}

/// One entry of the model-maintained todo list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TodoItem {
    pub text: String,
    pub status: TodoStatus,
}

/// The model replaced its todo list (plan tool); carries the full new list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TodoUpdated {
    pub run_id: RunId,
    pub items: Vec<TodoItem>,
}

// ── Questions ─────────────────────────────────────────────────────────────────

/// The model asked the user a question (`ask_user`); the run waits until the
/// question is answered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct QuestionAsked {
    pub run_id: RunId,
    /// Id to answer with.
    pub call_id: ToolCallId,
    pub question: String,
    /// Suggested answers (may be empty).
    pub options: Vec<String>,
}

// ── Model fallback ────────────────────────────────────────────────────────────

/// The loop switched to a fallback model after the previous one kept failing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ModelFallback {
    pub run_id: RunId,
    /// Model that failed.
    pub from: String,
    /// Model tried next.
    pub to: String,
    /// The error that caused the switch.
    pub reason: String,
}

// ── Workflow replay ───────────────────────────────────────────────────────────

/// A recorded workflow step is about to run (deterministic replay, no model).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WorkflowStepStarted {
    /// Run that started the replay, if it was started from a run.
    pub run_id: Option<RunId>,
    pub workflow: String,
    pub step_index: u32,
    /// Step kind, e.g. `desktop_action`.
    pub kind: String,
}

/// A workflow step finished successfully.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WorkflowStepCompleted {
    pub run_id: Option<RunId>,
    pub workflow: String,
    pub step_index: u32,
    /// Locator strategy that found the target, when the step had one.
    pub resolved_via: Option<String>,
}

/// A workflow step could not run (drift, ambiguity, failure or denial); the
/// replay stopped and control goes back to the agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WorkflowDrift {
    pub run_id: Option<RunId>,
    pub workflow: String,
    pub step_index: u32,
    pub reason: String,
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
    TodoUpdated(TodoUpdated),
    QuestionAsked(QuestionAsked),
    ModelFallback(ModelFallback),
    WorkflowStepStarted(WorkflowStepStarted),
    WorkflowStepCompleted(WorkflowStepCompleted),
    WorkflowDrift(WorkflowDrift),
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
