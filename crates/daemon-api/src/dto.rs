//! Request / response bodies of the `/v1` routes.  Ids are UUID strings;
//! timestamps are Unix epoch milliseconds.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

/// `GET /v1/health`.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct Health {
    /// Always `ok`.
    pub status: String,
    /// Daemon crate version.
    pub version: String,
    /// Seconds since the daemon started.
    pub uptime_secs: u64,
}

/// A bot.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct BotDto {
    /// Bot id.
    pub id: Uuid,
    /// Display name.
    pub name: String,
    /// Bot profile (ADR-0011) as JSON.
    pub config: Value,
    /// Creation time.
    pub created_at_ms: i64,
    /// The truth about what the bot may do: its stored policy merged with its
    /// persisted "always allow" grants.  Only present in `GET /v1/bots/{id}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_policy: Option<EffectivePolicyDto>,
}

/// A bot's permission picture (`GET /v1/bots/{id}`).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EffectivePolicyDto {
    /// The profile's own policy as JSON; `null` = the workspace policy applies.
    pub policy: Option<Value>,
    /// What a human-initiated call of each category resolves to, grants
    /// included (`meta` is always allowed and omitted).
    pub categories: BTreeMap<String, CategoryDecisionDto>,
    /// The persisted "always allow" rules.  Tool-level grants only widen that
    /// one tool and are not reflected in `categories`.
    pub grants: Vec<GrantDto>,
}

/// Effective decision of one permission category.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CategoryDecisionDto {
    /// `allow`, `ask` or `deny`.
    pub decision: String,
    /// `true` when `allow` holds only because of a category grant.
    pub via_grant: bool,
}

/// What an "always allow" grant covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GrantTargetDto {
    /// One tool, by name.
    Tool {
        /// Tool name.
        name: String,
    },
    /// Every tool of a permission category.
    Category {
        /// `fs`, `shell`, `desktop`, `browser`, `accounts`, `messaging`,
        /// `money`, `system`, `remote` or `script`.
        name: String,
    },
}

/// A persisted "always allow" rule of a bot.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GrantDto {
    /// What it covers.
    pub target: GrantTargetDto,
    /// When it was granted.
    pub granted_at_ms: i64,
}

/// `DELETE /v1/bots/{id}/grants`.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RevokeGrant {
    /// The rule to remove.
    pub target: GrantTargetDto,
}

/// `POST /v1/questions/{call_id}`.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AnswerQuestion {
    /// The user's answer (free text, or one of the offered options).
    pub answer: String,
}

/// Response of an answered question.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AnswerResponse {
    /// Tool call id of the question card.
    pub call_id: Uuid,
}

/// Create (`POST /v1/bots`) or replace (`PUT /v1/bots/{id}`) a bot.  Exactly
/// one of `config` / `profile_toml` is required.
#[derive(Debug, Default, Serialize, Deserialize, ToSchema)]
pub struct BotInput {
    /// Display name (defaults to the profile's `[bot].name`).
    #[serde(default)]
    pub name: Option<String>,
    /// Profile as a JSON object.
    #[serde(default)]
    pub config: Option<Value>,
    /// Profile as TOML text (the contents of a bot profile file).
    #[serde(default)]
    pub profile_toml: Option<String>,
}

/// A chat thread.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ChatDto {
    /// Chat id.
    pub id: Uuid,
    /// Bot the chat talks to.
    pub bot_id: Uuid,
    /// Title.
    pub title: String,
    /// Creation time.
    pub created_at_ms: i64,
}

/// `POST /v1/chats`.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CreateChat {
    /// Bot to talk to.
    pub bot_id: Uuid,
    /// Title (default `New chat`).
    #[serde(default)]
    pub title: Option<String>,
}

/// A stored chat message.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct MessageDto {
    /// Message id.
    pub id: Uuid,
    /// Chat it belongs to.
    pub chat_id: Uuid,
    /// Run that produced / carried it.
    pub run_id: Option<Uuid>,
    /// `user` / `assistant` / `system` / `tool`.
    pub role: String,
    /// Text.
    pub content: String,
    /// Creation time.
    pub created_at_ms: i64,
}

/// What to do when the chat already has a running turn.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Inject before the running turn's next model call.
    Steer,
    /// Run as the next turn after the current one (default).
    #[default]
    Queue,
}

/// `POST /v1/chats/{id}/messages`.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct SendMessage {
    /// Message text.
    pub text: String,
    /// Behaviour while a turn is running.
    #[serde(default)]
    pub mode: Mode,
}

/// How a sent message was delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryKind {
    /// A new turn (run) started.
    NewTurn,
    /// Injected into the running turn.
    Steered,
    /// Queued behind the running turn.
    Queued,
}

/// Response of a sent message.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct SendResponse {
    /// How it was delivered.
    pub delivery: DeliveryKind,
    /// The run to follow (`null` when queued: the turn has no run yet).
    pub run_id: Option<Uuid>,
}

/// A pending or decided tool-call approval.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ApprovalDto {
    /// Tool call id (what `POST /v1/approvals/{call_id}` takes).
    pub call_id: Uuid,
    /// Run awaiting it.
    pub run_id: Uuid,
    /// Tool the model wants to call.
    pub tool_name: String,
    /// Human-readable summary.
    pub summary: String,
    /// `null` while pending, else `approved` / `denied` / `expired`.
    pub decision: Option<String>,
    /// Request time.
    pub requested_at_ms: i64,
    /// Decision time.
    pub decided_at_ms: Option<i64>,
}

/// Decision verb.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Allow the call.
    Approve,
    /// Refuse the call.
    Deny,
}

/// How far an approval reaches.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// This call only (default).
    #[default]
    Once,
    /// This tool for the rest of the chat.
    ThisChat,
    /// This tool for this bot, persisted (a grant, see `/v1/bots/{id}/grants`).
    Always,
    /// The tool's whole permission category for this bot, persisted.  Money and
    /// never-list floors still ask.
    AlwaysCategory,
}

/// `POST /v1/approvals/{call_id}`.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct DecideApproval {
    /// Approve or deny.
    pub decision: Decision,
    /// Scope of an approval (ignored for deny).
    #[serde(default)]
    pub scope: Scope,
    /// Reason shown to the model on deny.
    #[serde(default)]
    pub reason: Option<String>,
}

/// Response of a decision.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct DecisionResponse {
    /// Tool call id.
    pub call_id: Uuid,
    /// The decision taken.
    pub decision: Decision,
}

/// A run.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RunDto {
    /// Run id.
    pub id: Uuid,
    /// Bot executing it.
    pub bot_id: Uuid,
    /// Chat, if any.
    pub chat_id: Option<Uuid>,
    /// `turn`, `turn:routine`, ...
    pub kind: String,
    /// `pending` / `running` / `waiting_approval` / `paused` / `succeeded` / `failed` / `cancelled`.
    pub state: String,
    /// Failure message.
    pub error: Option<String>,
    /// Creation time.
    pub created_at_ms: i64,
    /// Last state change.
    pub updated_at_ms: i64,
}

/// Token totals.
#[derive(Debug, Default, Clone, Serialize, Deserialize, ToSchema)]
pub struct UsageTotals {
    /// Prompt tokens.
    pub prompt_tokens: u64,
    /// Completion tokens.
    pub completion_tokens: u64,
    /// Cached prompt tokens (subset of `prompt_tokens`).
    pub cached_tokens: u64,
    /// Number of model calls.
    pub model_calls: u64,
}

/// Usage of one bot.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct BotUsage {
    /// Bot id.
    pub bot_id: Uuid,
    /// Its totals.
    #[serde(flatten)]
    pub totals: UsageTotals,
}

/// `GET /v1/usage`.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct UsageSummary {
    /// Runs counted.
    pub runs: u64,
    /// Totals over all counted runs.
    pub total: UsageTotals,
    /// Per bot.
    pub by_bot: Vec<BotUsage>,
}
