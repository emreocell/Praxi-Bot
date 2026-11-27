//! Client-stream event types.
//!
//! The agent core emits these events over the SSE / WebSocket stream.
//! Clients consume them to build live UI.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ids::{BotId, ChatId, RunId, TaskId, ToolCallId, UserId};

// ── Run lifecycle ─────────────────────────────────────────────────────────────

/// A run has started.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RunStarted {
    pub run_id: RunId,
    pub chat_id: ChatId,
    pub bot_id: BotId,
    /// The schedule that started this run (scheduled runs only; additive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule_id: Option<TaskId>,
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
    /// Structured facts the tool supplies for the approval card (for example
    /// a schedule's prompt and next firing times), when it has any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

/// What let a tool call run: who or which rule answered the question that would
/// otherwise have been a card. A closed set of daemon facts; nothing a page, a
/// mail or a tool result says can reach it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GrantedBy {
    /// The owner answered an approval card for this very call.
    OwnerCard,
    /// The Full level let the call through without a card, in a run the owner
    /// started.
    FullAccess,
    /// The Full level let the call through in a run started by a schedule, rule,
    /// webhook or mail the owner set up.
    OwnerSchedule,
    /// The Full level let the call through in an unattended session of the
    /// owner's mission.
    OwnerMission,
    /// The Balanced level's lane let the call through.
    BalancedLevel,
    /// An owner-created standing grant covered exactly this call.
    StandingGrant,
    /// A choice the owner made earlier on a card ("this chat", "always",
    /// "for this task") covered the call; `rule_id` names it.
    RememberedChoice,
    /// The owner's own per-site or per-app allow rule covered the call.
    SiteRule,
}

/// The permission level the bot was on when the call was decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AutonomyLevel {
    Ask,
    Balanced,
    Full,
}

/// A pending tool call was allowed to run: the owner approved its card, or an
/// automatic rule did the owner's part (see [`GrantedBy`]).
///
/// The three provenance fields are additive. An event recorded before they
/// existed has none of them and always meant an answered card, but it does not
/// say so: readers show it as an approval of unknown provenance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ToolCallApproved {
    pub run_id: RunId,
    pub call_id: ToolCallId,
    /// The owner who answered the card; for an automatic grant, the user the
    /// run belongs to (nobody answered).
    pub approved_by: UserId,
    /// What granted the call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub granted_by: Option<GrantedBy>,
    /// The bot's permission level when the call was decided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<AutonomyLevel>,
    /// The remembered rule that covered the call, as `<scope>:<name>` with scope
    /// `chat`, `always`, `category` or `task` and `name` a tool or category name.
    /// Never a path, address, command or other text of the call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_id: Option<String>,
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
    /// Truncated (at most 2 KB), redacted start of the tool's output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_preview: Option<String>,
    /// Time the tool itself ran, in milliseconds (excludes approval waits).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
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

// ── Secret requests ───────────────────────────────────────────────────────────

/// What a [`SecretRequested`] card asks the owner to type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SecretFields {
    /// One secret value (an API key, a token, a password on its own).
    Secret,
    /// A username and a password, stored as `<name>/username` and `<name>/password`.
    Login,
}

/// The model asked the owner for a credential (`secret_request`); the run waits
/// until the owner submits it on the local desktop or cancels. The event carries
/// what the card shows and NEVER a value: the owner's app posts the value
/// straight to the daemon (`POST /v1/secret-requests/{call_id}`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SecretRequested {
    pub run_id: RunId,
    /// Id to submit or cancel with.
    pub call_id: ToolCallId,
    /// The `fill/` entry name the value will be stored under (without the prefix).
    pub name: String,
    /// Why the bot asks. Plain text written by the model: untrusted, never markup.
    pub purpose: String,
    /// The exact browser origin the owner authorizes. Missing in old journal events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    pub fields: SecretFields,
    /// An entry with this name already exists and the bot asked to replace it.
    pub replaces_existing: bool,
    /// The run has already read untrusted content (a page, a mail, a download):
    /// the card must warn that the request may have been planted there.
    pub untrusted_content: bool,
    /// Seconds until the request expires on its own.
    pub expires_in_secs: u64,
    /// A mission asked: no run waits for the answer. The item that needs it is parked as
    /// blocked and the session moved on; the owner's answer releases that item. The card
    /// stays open after the tool call has returned.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub parked: bool,
}

// ── Handover to the owner ─────────────────────────────────────────────────────

/// What the owner is asked to do on a [`HandoverRequested`] card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HandoverKind {
    /// The page shows a CAPTCHA the bot could not (or should not) solve: the
    /// owner takes control of the browser, solves it and presses "Done".
    Captcha,
    /// The page wants a one-time code (2FA). The owner types it into the card;
    /// the daemon types it into the page. It is never stored and never reaches
    /// the model, the journal or the logs.
    Code,
}

/// The bot needs the owner for a step it must not do itself (`browser_handover`);
/// the run waits until the owner answers on the local desktop or the request
/// expires. The event carries what the card shows and NEVER a code: the owner's
/// app posts it straight to the daemon (`POST /v1/handovers/{call_id}/code`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HandoverRequested {
    pub run_id: RunId,
    /// Id to answer with.
    pub call_id: ToolCallId,
    pub kind: HandoverKind,
    /// Origin the browser reports for the page (`https://example.com:443`), read
    /// by the daemon, never taken from page text.
    pub site: String,
    /// The run has already read untrusted content (a page): the card warns that a
    /// fake prompt on a page can ask for a code.
    pub untrusted_content: bool,
    /// Seconds until the request expires on its own.
    pub expires_in_secs: u64,
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

// ── Decision ladder ───────────────────────────────────────────────────────────

/// The K0-K3 ladder decided how a turn starts (ADR-0005), before the main
/// model (K4) runs.  Emitted once per top-level turn of a bot whose router is
/// enabled.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RouteDecided {
    pub run_id: RunId,
    /// Level that decided: `K0` (rules / commands), `K1` (intent cache), `K2`
    /// (BM25 + embeddings), `K3` (classifier) or `K4` (nothing cheaper applied).
    pub level: String,
    /// The route, tagged by `kind`: `command` (`name`, `args`), `workflow`
    /// (`name`, `params`, `confidence`), `tool_hint` (`tools`), `skill_hint`
    /// (`skills`) or `agent`.
    pub route: Value,
    /// Confidence in `0..=1`.
    pub confidence: f64,
    /// Wall time of the whole decision.
    pub latency_ms: u64,
    /// Why (for the "why did it do that" panel).
    pub reason: String,
    /// Optional first-turn model tier (`light` or `standard`). Absent when disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
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

// ── Desktop control bridge ────────────────────────────────────────────────────

/// A rectangle in global physical screen pixels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ControlRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// A desktop action is about to run: the daemon asks the desktop app (if one
/// is connected) to show the on-screen overlay first.  The app answers with
/// `POST /v1/control/{request_id}`; without an answer the daemon decides on its
/// own after a timeout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ControlRequested {
    /// Id to answer with.
    pub request_id: uuid::Uuid,
    pub run_id: RunId,
    pub bot_id: BotId,
    /// Target of the action (global physical screen pixels), when known.
    pub rect: Option<ControlRect>,
    /// Short description of the target, e.g. `Save button`.
    pub label: String,
    /// `click`, `type`, `key`, `scroll`, `drag`, `set_value`, `launch`, ...
    pub action: String,
}

// ── Subagents ─────────────────────────────────────────────────────────────────

/// The agent spawned a subagent (a child run of the same bot).  Emitted on the
/// parent run's stream; the child's own events carry `parent_run_id` in their
/// envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SubagentStarted {
    /// The child run.
    pub run_id: RunId,
    pub parent_run_id: RunId,
    /// What the child was asked to do.
    pub task: String,
    /// The parent's `spawn_subagent` tool call that created the child.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<ToolCallId>,
    /// Set when the child is ANOTHER bot asked through `ask_bot`: the
    /// hand-off card shows who asked whom. Daemon-supplied, never model text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub helper: Option<SubagentHelper>,
}

/// The two bots of an `ask_bot` hand-off, for the chat card.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SubagentHelper {
    /// The bot that was asked (the child run executes as this bot).
    pub bot_id: String,
    /// Display name of the bot that was asked.
    pub bot_name: String,
    /// The bot that asked.
    pub caller_bot_id: String,
    /// Display name of the bot that asked.
    pub caller_name: String,
}

/// A subagent run ended.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SubagentFinished {
    /// The child run.
    pub run_id: RunId,
    pub parent_run_id: RunId,
    /// Final run state: `succeeded`, `failed`, `cancelled` or `paused`.
    pub state: String,
    /// The child's final text (at most 2 KB here; the parent's tool result
    /// carries up to 8 KB) or the failure message.
    pub summary: String,
    /// The parent's `spawn_subagent` tool call that created the child.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<ToolCallId>,
}

// ── Errors ────────────────────────────────────────────────────────────────────

/// A non-fatal error occurred during the run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RunError {
    pub run_id: RunId,
    pub message: String,
    pub recoverable: bool,
}

// ── File checkpoints ──────────────────────────────────────────────────────────

/// The user restored a file checkpoint of this run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CheckpointRestored {
    /// Restore attempt failed; inspect warnings and safety snapshots.
    #[serde(default)]
    pub partial: bool,
    pub run_id: RunId,
    /// Restored checkpoint id, or `"all"` when undoing the whole run.
    pub checkpoint_id: String,
    /// Safety snapshot created of the overwritten state before restoring.
    pub safety_checkpoint_id: String,
    /// Paths restored (in restore order).
    pub restored: Vec<String>,
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
    SecretRequested(SecretRequested),
    HandoverRequested(HandoverRequested),
    ModelFallback(ModelFallback),
    RouteDecided(RouteDecided),
    WorkflowStepStarted(WorkflowStepStarted),
    WorkflowStepCompleted(WorkflowStepCompleted),
    WorkflowDrift(WorkflowDrift),
    ControlRequested(ControlRequested),
    SubagentStarted(SubagentStarted),
    SubagentFinished(SubagentFinished),
    CheckpointRestored(CheckpointRestored),
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
            schedule_id: Some(TaskId::new()),
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

    #[test]
    fn approval_provenance_is_additive_and_old_event_round_trips_unchanged() {
        let old = serde_json::json!({
            "type": "tool_call_approved", "run_id": RunId::new(),
            "call_id": ToolCallId::new(), "approved_by": UserId::new(),
        });
        let event: Event = serde_json::from_value(old.clone()).unwrap();
        let Event::ToolCallApproved(a) = &event else {
            panic!("not an approval");
        };
        assert_eq!(
            (a.granted_by, a.level, a.rule_id.as_deref()),
            (None, None, None)
        );
        assert_eq!(serde_json::to_value(&event).unwrap(), old);
        let mut full = old;
        full["granted_by"] = serde_json::json!("remembered_choice");
        full["level"] = serde_json::json!("balanced");
        full["rule_id"] = serde_json::json!("always:shell_run");
        let event: Event = serde_json::from_value(full.clone()).unwrap();
        assert_eq!(serde_json::to_value(event).unwrap(), full);
    }

    #[test]
    fn route_tier_is_optional_and_old_event_round_trips_unchanged() {
        let old = serde_json::json!({
            "type": "route_decided", "run_id": RunId::new(), "level": "K4",
            "route": {"kind":"agent"}, "confidence": 0.0,
            "latency_ms": 0, "reason": "no route",
        });
        let event: Event = serde_json::from_value(old.clone()).unwrap();
        assert_eq!(serde_json::to_value(&event).unwrap(), old);
        let mut selected = old;
        selected["tier"] = serde_json::json!("light");
        let event: Event = serde_json::from_value(selected.clone()).unwrap();
        assert_eq!(serde_json::to_value(event).unwrap(), selected);
    }
}
