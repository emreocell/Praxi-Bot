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
    /// `ok` when ready for inference, or `setup_required` before provider setup.
    pub status: String,
    /// Daemon crate version.
    pub version: String,
    /// HTTP API compatibility version.
    pub api_version: String,
    /// Exact published release, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_tag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_id: Option<String>,
    /// Managed update capability and the result of the most recent attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_update: Option<crate::host::HostUpdateStatus>,
    /// Capabilities of this host; probes never launch a browser.
    pub host: HostInfo,
    /// Seconds since the daemon started.
    pub uptime_secs: u64,
    /// Background startup: `starting` while leadership is verified and components
    /// start, `ready` once finished, `degraded` when a component failed to start.
    #[serde(default = "default_ready")]
    pub startup: crate::host::StartupPhase,
    /// `true` when startup finished without a failure: this daemon's own automatic
    /// components are running, or it is standby/client by design and runs none.
    /// It does NOT mean channels are active; use `channels_active` for that.
    #[serde(default = "default_true")]
    pub components_ready: bool,
    /// `true` only when this daemon is the active leader and its automatic
    /// components (scheduler, channels) are running. `false` on standby.
    #[serde(default)]
    pub channels_active: bool,
    /// Why startup is `degraded`; fixed text, no secrets or paths.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub startup_reason: Option<String>,
    /// `true` when the daemon was started in safe mode: no tools, no schedules, no
    /// channels, no MCP servers or apps. Fixed for the life of the process.
    #[serde(default)]
    pub safe_mode: bool,
    /// The background loops (schedules, mission wake-ups, mission drivers).
    #[serde(default)]
    pub background: BackgroundHealth,
}

/// Whether the loops that make the daemon act on its own are running.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct BackgroundHealth {
    /// `ok`, or `degraded` when a loop crashed too often and was given up on (it stays down
    /// until the daemon restarts; the owner got a notice).
    pub status: String,
    /// The loops this daemon runs (none on a standby node or in safe mode).
    #[serde(default)]
    pub loops: Vec<BackgroundLoop>,
}

/// One supervised background loop.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BackgroundLoop {
    /// `scheduler`, `mission_wakes` or `mission_drivers`.
    pub name: String,
    /// Given up on after repeated crashes.
    pub degraded: bool,
    /// Crashes (and restarts) since the daemon started.
    pub restarts: u32,
}

fn default_ready() -> crate::host::StartupPhase {
    crate::host::StartupPhase::Ready
}
fn default_true() -> bool {
    true
}

/// Host capabilities, independent of provider readiness.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct HostInfo {
    pub os: String,
    pub arch: String,
    pub has_display: bool,
    pub has_browser: bool,
    /// Physical memory in MiB (1024 * 1024 bytes).
    pub total_ram_mb: u64,
    pub mode: crate::host::HostMode,
    pub state: crate::host::HostState,
    pub epoch: u64,
}

impl HostInfo {
    #[must_use]
    pub fn detect() -> Self {
        Self {
            mode: crate::host::HostMode::default(),
            state: crate::host::HostState::default(),
            epoch: 0,
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            has_display: browser_cdp::host::has_display(),
            has_browser: browser_cdp::find_browser_cached().is_ok(),
            total_ram_mb: browser_cdp::host::total_ram_bytes() / (1024 * 1024),
        }
    }
}

/// Bot `[runtime]` settings (ADR-0027 §7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RuntimeConfigDto {
    /// Configured computer target (`auto`, `pc`, `host`, `browser`, `off`, `node:<id>`, or
    /// `server:<server key>`: the bot stays here, its browser tools act on that paired server).
    pub computer: String,
}

/// Public bot representation.
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
    /// Bot revision: starts at 1; profile writes and skill assignment changes
    /// advance it in the write transaction. Send the loaded value back as
    /// `expected_version` to both PUT and PATCH.
    pub version: i64,
    /// The truth about what the bot may do: its stored policy merged with its
    /// persisted "always allow" grants.  Only present in `GET /v1/bots/{id}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_policy: Option<EffectivePolicyDto>,
    /// Parsed `[runtime]` computer target settings (`auto` when omitted in profile).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<RuntimeConfigDto>,
}

/// A bot's permission picture (`GET /v1/bots/{id}`).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EffectivePolicyDto {
    /// The profile's own policy as JSON; `null` = the workspace policy applies.
    pub policy: Option<Value>,
    /// What a human-initiated call of each category resolves to, grants
    /// included (`meta` is always allowed and omitted).
    pub categories: BTreeMap<String, CategoryDecisionDto>,
    /// The bot's autonomy mode, derived from its preset: `ask`, `balanced` or `full`.
    pub mode: String,
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
    /// Computers where a category grant allows this category. The decision
    /// stays `ask` for every computer not listed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
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
    /// One tool on one target: a site, a folder or a command prefix.
    Scoped {
        /// Tool name.
        tool: String,
        /// `origin`, `folder` or `command`.
        scope: String,
        /// The one target the rule covers (see `scope`).
        pattern: String,
    },
}

/// A persisted "always allow" rule of a bot.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GrantDto {
    /// Computer scope of this grant, when it controls a desktop/browser.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub computer: Option<String>,
    /// What it covers.
    pub target: GrantTargetDto,
    /// When it was granted.
    pub granted_at_ms: i64,
    /// Legacy desktop/browser rule with no computer. It does not authorize
    /// anything; it is listed so the user can see that it stopped applying.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub inert: bool,
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
    /// Required for PUT: version of the profile being replaced. Missing is 428;
    /// stale is 409 with the current bot. Not used when creating a bot.
    #[serde(default)]
    pub expected_version: Option<i64>,
    /// Display name (defaults to the profile's `[bot].name`).
    #[serde(default)]
    pub name: Option<String>,
    /// Profile as a JSON object.
    #[serde(default)]
    pub config: Option<Value>,
    /// Profile as TOML text (the contents of a bot profile file).
    #[serde(default)]
    pub profile_toml: Option<String>,
    /// Optional `[runtime]` declaration. Must agree with `config` / `profile_toml`
    /// when both are supplied. Omission on update preserves the stored target.
    #[serde(default)]
    pub runtime: Option<RuntimeConfigDto>,
}

/// How one [`BotPatchOp`] changes the profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BotPatchKind {
    /// Set the value at `path` (missing parent objects are created).
    Set,
    /// Remove the key at `path` (no effect when it is absent).
    Unset,
}

/// One field-level change of a bot profile.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BotPatchOp {
    /// `set` or `unset`.
    pub op: BotPatchKind,
    /// RFC 6901 JSON Pointer into the profile (`config`), e.g. `/models/planner`,
    /// `/permissions/categories/fs/level`, `/runtime/computer`.  Object keys
    /// only: a path through an array or a scalar is refused. At most 32 segments
    /// and 4096 bytes; resulting profile depth at most 32.
    pub path: String,
    /// The new value (`set` only).
    #[serde(default)]
    pub value: Value,
}

/// `PATCH /v1/bots/{id}`: change only the listed fields of a bot, atomically.
/// Every field not named - including unknown keys - stays exactly as stored.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct BotPatchInput {
    /// The version the client loaded (`BotDto::version`).  A different current
    /// version refuses the whole patch with `409` and returns the current bot.
    pub expected_version: i64,
    /// New display name.
    #[serde(default)]
    pub name: Option<String>,
    /// Profile changes, applied in order (at most 128). Values/result use a
    /// conservative 1 MiB escaped JSON budget; excessive input is 400.
    #[serde(default)]
    pub ops: Vec<BotPatchOp>,
}

/// `409` body of `PATCH /v1/bots/{id}`: the usual error plus the bot as stored now.
#[derive(Debug, Serialize, ToSchema)]
pub struct BotConflictBody {
    /// `version_conflict`.
    pub error: crate::error::ErrorDetail,
    /// The current bot, with its current version.
    pub current: BotDto,
}

/// A chat thread.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ChatDto {
    /// Chat id.
    pub id: Uuid,
    /// Same as `id`, under the name clients that treat `POST /v1/chats` as
    /// "create chat" expect.
    pub chat_id: Uuid,
    /// Bot the chat talks to.
    pub bot_id: Uuid,
    /// Title.
    pub title: String,
    /// Creation time.
    pub created_at_ms: i64,
    /// Pinned chats sort first.
    #[serde(default)]
    pub pinned: bool,
    /// Archived chats are hidden from the default list.
    #[serde(default)]
    pub archived: bool,
}

/// `PATCH /v1/chats/{id}`: every field is optional, absent ones stay unchanged.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct UpdateChat {
    /// New title (1 to 120 characters after trimming).
    pub title: Option<String>,
    /// Pin or unpin.
    pub pinned: Option<bool>,
    /// Archive or restore.
    pub archived: Option<bool>,
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
    /// Bot that ran `run_id` (the author of an assistant message). It differs
    /// from the chat's bot when another bot answered an `@mention`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bot_id: Option<Uuid>,
    /// `user` / `assistant` / `system` / `tool`.
    pub role: String,
    /// Text.
    pub content: String,
    /// Creation time.
    pub created_at_ms: i64,
    /// Where the message came from (`owner` = this app, `telegram`, `peer`,
    /// `local_api`, `imported`); absent for older messages and most replies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Files attached to a user message.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<AttachmentDto>,
    /// Replaced by an edit or regenerated: kept as audit data, no longer part of
    /// the model context.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub superseded: bool,
    /// Only set on the messages of the chat's last turn: the run behind it used
    /// a tool that is not read-only, so redoing the turn could repeat an effect.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub used_write_tools: bool,
}

/// A file attached to a chat message (`POST /v1/chats/{id}/attachments`).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AttachmentDto {
    /// Attachment id.
    pub id: Uuid,
    /// Display name.
    pub name: String,
    /// Sniffed media type.
    pub mime_type: String,
    /// `image` (sent to the model as a picture), `document` or `text` (sent as text).
    pub kind: String,
    /// Original size in bytes.
    pub size: u64,
    /// Only the start of the text was given to the model.
    pub truncated: bool,
}

/// What a run cost, read from the usage ledger (`GET /v1/runs/{id}/usage`).
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RunUsageDto {
    /// The last completed executor response came from a configured loopback model.
    #[serde(default)]
    pub local_model: bool,
    /// `provider/model` the run's turn used (the last executor call); absent
    /// when no model call was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Prompt tokens of the run's own model calls (reported by the provider).
    pub prompt_tokens: u64,
    /// Completion tokens of the run's own model calls.
    pub completion_tokens: u64,
    /// Number of model calls of the run.
    pub model_calls: u64,
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
    /// Ids returned by `POST /v1/chats/{id}/attachments` for this chat (at most 6). The text
    /// may be empty when files are attached.
    #[serde(default)]
    pub attachment_ids: Vec<Uuid>,
}

/// `POST /v1/chats/{id}/resend`: re-run the last turn (regenerate), or replace the
/// last user message and run it (edit & resend).
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ResendMessage {
    /// The user message the client believes is the chat's last one.
    pub message_id: Uuid,
    /// The edited text; absent regenerates with the original text.
    #[serde(default)]
    pub text: Option<String>,
}

/// The new run started by a resend.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ResendResponse {
    /// The run to follow.
    pub run_id: Uuid,
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
    /// The message was `/stop`: the running turn was cancelled (`run_id` is that turn).
    Stopped,
    /// The turn was waiting for an approval and the owner wrote instead of answering: the
    /// approval was denied, the turn was stopped (`run_id` is that turn) and the message starts
    /// the next turn at once.
    Redirected,
}

/// Response of a sent message.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct SendResponse {
    /// How it was delivered.
    pub delivery: DeliveryKind,
    /// The run to follow (`null` when queued: the turn has no run yet).
    pub run_id: Option<Uuid>,
    /// Only when `delivery` is `queued`: who the queued turn will start as
    /// (`human`, `schedule`, `channel`, ...). Fixed when the message is queued.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued_initiator: Option<String>,
}

/// Response of `POST /v1/runs/{id}/remember`.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RememberResponse {
    /// The route that was cached for the run's request text, tagged by `kind`
    /// (`workflow`, `tool_hint` or `skill_hint`).
    #[schema(value_type = Object)]
    pub route: serde_json::Value,
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
    /// The call's input, as the `approval_needed` event carried it (so a client that missed the
    /// event can still show the full card).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    /// The tool's card facts from the `approval_needed` event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
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
    /// Typing, keys, value setting and dragging into the one program the card
    /// names, for the rest of this run only. Accepted only on a card whose
    /// `details.control_grant.offered` is true; refused everywhere else.
    RunControl,
    /// "Allow for this task": the same MCP server and tool for the rest of this run only. Never
    /// persisted, never another tool; it stops covering once the run reads something new (private
    /// data after a web page). Accepted only on a card whose `details.run_grant` is true.
    RunTool,
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
    /// `queued` / `running` / `waiting_approval` / `paused` / `succeeded` / `failed` / `cancelled`.
    pub state: String,
    /// Failure message.
    pub error: Option<String>,
    /// Creation time.
    pub created_at_ms: i64,
    /// Last state change.
    pub updated_at_ms: i64,
    /// The run that spawned this one when it is a subagent run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<Uuid>,
    /// Whether `POST /v1/runs/{id}/remember` would accept this run (same
    /// checks; nothing is cached by asking).
    #[serde(default)]
    pub can_remember: bool,
    /// The schedule that started this run (`null` for runs the user started).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule_id: Option<Uuid>,
    /// Resolved computer target label fixed at run start (`pc`, `host`, `browser`, `off`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub computer_target: Option<String>,
    /// Stable key of the PC used by this run (`local` for the daemon desktop).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub computer_node: Option<String>,
    /// Display name captured at target binding.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub computer_node_name: Option<String>,
    /// Start of the current pause in Unix milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paused_at_ms: Option<i64>,
    /// Reason the run is currently `paused`: `waiting_for_pc`, `daemon_restart` (a restart interrupted it; Full-level bots resume by themselves, a run that was waiting for a person is never marked this way) or `restart_resume_limit` (it was resumed automatically as often as allowed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_reason: Option<String>,
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
    /// Cost in USD of the model calls whose price is known; `null` when no
    /// call has a known price.
    #[serde(default)]
    pub cost_usd: Option<f64>,
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

/// Usage of one provider/model reference.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ModelUsage {
    /// Model reference as recorded in the ledger (e.g. `openai/gpt-4o`, or `"unknown"`).
    pub model: String,
    /// Provider prefix before `/` (or `cli:<name>`), when present.
    #[serde(default)]
    pub provider: Option<String>,
    /// Totals across all calls to this model.
    #[serde(flatten)]
    pub totals: UsageTotals,
    /// Calls to this model that reported no token usage.
    #[serde(default)]
    pub calls_without_usage: u64,
    /// Calls to this model whose cost is unknown (missing price or missing token usage).
    #[serde(default)]
    pub calls_without_cost: u64,
}

/// Usage of one provider.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ProviderUsage {
    /// Provider identifier (prefix before `/`, `cli:<name>`, or `"unknown"` when none).
    pub provider: String,
    /// Totals across all calls to this provider.
    #[serde(flatten)]
    pub totals: UsageTotals,
    /// Calls to this provider that reported no token usage.
    #[serde(default)]
    pub calls_without_usage: u64,
    /// Calls to this provider whose cost is unknown (missing price or missing token usage).
    #[serde(default)]
    pub calls_without_cost: u64,
}

/// Usage bucketed to one local calendar day (`YYYY-MM-DD`).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DailyUsage {
    /// Calendar day in `time_zone` (`YYYY-MM-DD`).
    pub day: String,
    /// Totals for that day.
    #[serde(flatten)]
    pub totals: UsageTotals,
    /// Calls on that day that reported no token usage.
    #[serde(default)]
    pub calls_without_usage: u64,
    /// Calls on that day whose cost is unknown (missing price or missing token usage).
    #[serde(default)]
    pub calls_without_cost: u64,
}

/// One run in the "most expensive runs" list of `GET /v1/usage`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TopRunUsage {
    /// Run id.
    pub run_id: Uuid,
    /// Bot that executed the run.
    pub bot_id: Uuid,
    /// Chat the run belongs to (lets the UI open it), when it has one.
    #[serde(default)]
    pub chat_id: Option<Uuid>,
    /// When the run started (Unix ms).
    pub created_at_ms: i64,
    /// Totals of the run; `cost_usd` is `null` when no call had a known price.
    #[serde(flatten)]
    pub totals: UsageTotals,
    /// Calls of this run that reported no token usage.
    #[serde(default)]
    pub calls_without_usage: u64,
    /// Calls of this run whose cost is unknown.
    #[serde(default)]
    pub calls_without_cost: u64,
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
    /// Per-purpose totals, including invocations that report no tokens.
    #[serde(default)]
    pub by_purpose: std::collections::BTreeMap<String, UsageTotals>,
    /// Per-model totals, including invocations that report no tokens.
    #[serde(default)]
    pub by_model: Vec<ModelUsage>,
    /// Per-provider totals, including invocations that report no tokens.
    #[serde(default)]
    pub by_provider: Vec<ProviderUsage>,
    /// Daily series bucketed in `time_zone` (chronological, bounded to at most 366 days).
    #[serde(default)]
    pub daily: Vec<DailyUsage>,
    /// The (at most five) most expensive runs in the range: known cost first,
    /// then runs of unknown price by tokens.
    #[serde(default)]
    pub top_runs: Vec<TopRunUsage>,
    /// Calls in the selected range that reported no token usage.
    #[serde(default)]
    pub calls_without_usage: u64,
    /// Calls in the selected range whose cost is unknown (missing price or missing token usage).
    #[serde(default)]
    pub calls_without_cost: u64,
    /// IANA time zone used to bucket `daily` (e.g. `Europe/Istanbul` or `UTC`).
    #[serde(default)]
    pub time_zone: String,
}

// ---------------------------------------------------------- control bridge

/// Who is driving the desktop, as the desktop app's state machine reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ControlMode {
    /// No bot run is using the desktop.
    Idle,
    /// A bot run is driving the desktop.
    BotDriving,
    /// The human took the wheel: desktop actions are refused at once.
    HumanDriving,
    /// The run is paused: desktop actions are refused at once.
    Paused,
}

/// `PUT /v1/control/state`: the desktop app registers as controller and
/// reports its mode.  Send it on every state change and at least every 5 s;
/// the registration expires after 15 s without a heartbeat.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ControlStateInput {
    /// The app's current mode.
    pub mode: ControlMode,
    /// With `idle` / `bot_driving`: the user's explicit resume, clears a
    /// latched pause.  Plain heartbeats (without it) never release.
    #[serde(default)]
    pub release: bool,
}

/// The daemon's view of the controller registration.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ControlStateDto {
    /// Last reported mode; `null` when no app is registered (never reported,
    /// or the heartbeat expired).
    pub mode: Option<ControlMode>,
    /// Milliseconds until the registration expires; `0` when not registered.
    pub expires_in_ms: u64,
    /// A desktop app is listening on the global event stream.
    pub app_connected: bool,
    /// `paused` / `human_driving` while latched: refuses bot actions until
    /// `POST /v1/control/release`, regardless of the heartbeat.  `null` when
    /// nothing is latched.
    pub latched: Option<ControlMode>,
    /// Owned takeover; restored latches require explicit recovery.
    #[serde(default)]
    pub takeover: Option<live_view::TakeoverInfo>,
    /// Idempotent release acknowledgement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<ReleaseResult>,
}

/// `GET /v1/events/latest`.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct LatestCursor {
    /// Highest daemon-wide event `gseq` (`0` = none yet).
    pub gseq: u64,
}

/// The app's answer to a `control_requested` event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ControlVerdict {
    /// Run the action.
    Proceed,
    /// Do not run it (the human stopped or took over).
    Refuse,
}

/// `POST /v1/control/{request_id}`.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ControlDecisionInput {
    /// Proceed or refuse.
    pub decision: ControlVerdict,
    /// Reason shown to the model on refuse.
    #[serde(default)]
    pub reason: Option<String>,
}

/// Response of `POST /v1/control/{request_id}`.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ControlDecisionResponse {
    /// The request answered.
    pub request_id: Uuid,
    /// The decision taken.
    pub decision: ControlVerdict,
}

// ------------------------------------------------------------------ policy

/// The global default permission policy (`GET` / `PUT /v1/policy`).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PolicyDto {
    /// Name of the preset the policy equals (`read-only`,
    /// `approve-destructive`, `read-and-draft`, `full`); `null` = custom.
    pub preset: Option<String>,
    /// Set when the persisted `policy.json` could not be loaded: the daemon is
    /// locked to deny-all until a new policy is saved with `PUT /v1/policy`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load_error: Option<String>,
    /// Level per category: `deny`, `ask`, `allow` or `allow_with_limits`.
    pub categories: BTreeMap<String, String>,
}

/// `PUT /v1/policy`.  Start from `preset` (or, without it, from the current
/// policy) and apply the `categories` overrides on top.
#[derive(Debug, Default, Serialize, Deserialize, ToSchema)]
pub struct PolicyInput {
    /// `read-only`, `approve-destructive`, `read-and-draft` or `full`.
    #[serde(default)]
    pub preset: Option<String>,
    /// Per-category level: `deny`, `ask` or `allow`.
    #[serde(default)]
    pub categories: BTreeMap<String, String>,
}

// --------------------------------------------------------------- providers

/// `GET /v1/providers`.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ProvidersDto {
    /// Id of the loop's default provider.
    pub default_provider: Option<String>,
    /// Ids of the providers the running loop can use.
    pub active: Vec<String>,
    /// Providers from the user's `providers.toml`.
    pub configured: Vec<ConfiguredProviderDto>,
    /// Entries of the built-in provider catalog.
    pub catalog: Vec<CatalogProviderDto>,
}

/// A configured provider (never carries a key).
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ConfiguredProviderDto {
    /// Provider id.
    pub id: String,
    /// May be used.
    pub enabled: bool,
    /// Is the configured default.
    pub is_default: bool,
    /// Chosen default model (without the provider prefix).
    pub default_model: Option<String>,
    /// A key is stored for it.
    pub has_key: bool,
    /// Not from the catalog: defined by the user.
    pub custom: bool,
    /// Loaded in the running loop (a restart or `reload` picks up changes).
    pub active: bool,
    /// Model ids in the same order as `model_details`. By default only current
    /// chat candidates are shown; `?chat_only=false` includes everything.
    pub models: Vec<String>,
    /// Presentation metadata: recommended models first by rank, then the rest
    /// by display name. Split on `recommended` for an "All models" expander.
    /// No model is added just because it appears in the curated table.
    #[serde(default)]
    pub model_details: Vec<ModelMetadataDto>,
    /// Why the model list could not be fetched.
    pub models_error: Option<String>,
    /// Coding CLIs only: where the list came from (`cli`, `local_cache` or `bundled`, the
    /// last-resort fallback the UI marks as such).
    #[serde(default)]
    pub models_source: Option<String>,
    /// Coding CLIs only: when the list was fetched (ms since the epoch; absent for `bundled`).
    #[serde(default)]
    pub models_fetched_at_ms: Option<i64>,
}

/// Presentation only: never changes execution capabilities or configured ids.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ModelMetadataDto {
    pub id: String,
    pub display_name: String,
    /// `chat`, `reasoning`, `vision_capable`, `embedding`, `image`, `audio`,
    /// `video`, or `other`. Kind describes primary use, not input modalities.
    pub kind: String,
    pub recommended: bool,
    pub recommended_rank: Option<u8>,
    pub deprecated: bool,
    /// A short annotation such as `preview` or a live `OpenRouter` `free` id.
    pub note: Option<String>,
}

impl From<provider_catalog::ModelMetadata> for ModelMetadataDto {
    fn from(model: provider_catalog::ModelMetadata) -> Self {
        Self {
            id: model.id,
            display_name: model.display_name,
            kind: model.kind.as_str().into(),
            recommended: model.recommended,
            recommended_rank: model.recommended_rank,
            deprecated: model.deprecated,
            note: model.note,
        }
    }
}

/// A catalog entry.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CatalogProviderDto {
    /// Catalog id.
    pub id: String,
    /// `openai_compatible`, `anthropic`, `gemini` or `other:<name>`.
    pub kind: String,
    /// Base URL.
    pub base_url: String,
    /// `api_key_env` or `none`.
    pub auth: String,
    /// Environment variable of the key, for `api_key_env`.
    pub key_env: Option<String>,
    /// Supports tool calls.
    pub tools: bool,
    /// Supports image input.
    pub vision: bool,
    /// Already in `providers.toml`.
    pub configured: bool,
}

/// `POST /v1/providers/reload`.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ReloadResponse {
    /// Id of the new default provider.
    pub default_provider: Option<String>,
    /// Ids now loaded.
    pub active: Vec<String>,
}

/// Authentication status reported by a CLI provider (`GET /v1/providers/detected`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CliAuthState {
    SignedIn,
    SignedOut,
    Unknown,
}

impl From<cli_provider::AuthState> for CliAuthState {
    fn from(s: cli_provider::AuthState) -> Self {
        match s {
            cli_provider::AuthState::SignedIn => Self::SignedIn,
            cli_provider::AuthState::SignedOut => Self::SignedOut,
            cli_provider::AuthState::Unknown => Self::Unknown,
        }
    }
}

/// A detected coding CLI engine (`GET /v1/providers/detected`).
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DetectedProviderDto {
    /// Engine id (`claude`, `codex`, `cursor`, `opencode`, `gemini`, `agy`).
    pub id: String,
    /// Display name.
    pub display_name: String,
    /// Whether the CLI binary was found on PATH and `--version` succeeded.
    pub installed: bool,
    /// CLI version (first line, trimmed, max 80 chars, or null).
    pub version: Option<String>,
    /// Authentication status (`signed_in`, `signed_out`, `unknown`).
    pub auth: CliAuthState,
    /// Account label (email or org name, max 160 chars, or null).
    pub account_label: Option<String>,
    /// Whether this engine can currently run turns.
    pub runnable: bool,
    /// Stable detail code (`not_on_path`, `timeout`, `auth_unparsed`, `codex_unconfinable`, etc., or null).
    pub detail: Option<String>,
    /// User-readable refusal when detected but not runnable (e.g. Codex `unified_exec`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Request body for `POST /v1/providers/cli/enable`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EnableCliProviderInput {
    /// Engine id (`claude` is currently runnable).
    pub id: String,
}

impl From<cli_provider::DetectedCli> for DetectedProviderDto {
    fn from(c: cli_provider::DetectedCli) -> Self {
        Self {
            id: c.id,
            display_name: c.display_name,
            installed: c.installed,
            version: c.version,
            auth: c.auth.into(),
            account_label: c.account_label,
            runnable: c.runnable,
            detail: c.detail,
            message: c.message,
        }
    }
}

/// A schedule (`/v1/schedules`).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ScheduleDto {
    /// Schedule id.
    pub id: Uuid,
    /// Bot that runs it.
    pub bot_id: Uuid,
    /// Chat the runs post to; without one a chat named after the schedule is
    /// used (and created on the first firing).
    pub chat_id: Option<Uuid>,
    /// Display name.
    pub name: String,
    /// When it fires: `{"cron": "0 9 * * 1-5 Europe/Istanbul"}`,
    /// `{"every": {"seconds": 600}}` or `{"at": {"unix_ms": 1767225600000}}`.
    pub spec: Value,
    /// What it does: `{"kind": "agent_prompt", "text": "..."}`,
    /// `{"kind": "workflow", "name": "...", "params": {}}` or
    /// `{"kind": "tool", "name": "...", "input": {}}`, or a standing order that speaks only
    /// when something needs you: `{"kind": "standing_order", "checklist": "...",
    /// "active_hours": {"start": "08:00", "end": "22:00", "timezone": "Europe/Istanbul"},
    /// "max_runs_per_day": 24}` (`active_hours` optional; each run ends `OK`, which creates
    /// no notice, or `NEEDS_OWNER(reason)`, which does).
    pub payload: Value,
    /// Whether the timer is armed.
    pub enabled: bool,
    /// Creation time.
    pub created_at_ms: i64,
    /// Last firing.
    pub last_run_at_ms: Option<i64>,
    /// Next firing (`null` for a disabled or finished schedule).
    pub next_run_at_ms: Option<i64>,
    /// `skip`, `run_once` or `run_all`: what happens to occurrences missed
    /// while the daemon was down.
    pub misfire_policy: String,
    /// With `run_once` / `run_all`: a missed occurrence older than this many
    /// seconds is not run late; it is listed as missed instead. `null` = no limit.
    #[serde(default)]
    pub catchup_window_secs: Option<u64>,
    /// A webhook trigger is configured (`POST /v1/webhooks/{id}`); the secret
    /// is never returned.
    pub has_webhook: bool,
    /// Non-webhook trigger definition; never includes webhook secrets.
    pub trigger: Option<Value>,
    /// Read-only deterministic condition, including full script source.
    pub precheck: Option<Value>,
    /// Firings skipped before an agent run was created.
    #[serde(default)]
    pub skipped_by_precheck: u64,
    /// Consecutive precheck/polling failures (ten pauses the schedule).
    #[serde(default)]
    pub consecutive_errors: u32,
    /// Who created it: `user`, or `bot` (a bot's schedule tools, after you
    /// approved it). A bot may change only its own `bot` schedules.
    pub origin: String,
    /// Who wrote the definition, as the daemon recorded it: `owner` (the local app or your
    /// administrator desktop), `bot` (its schedule tools) or `unknown` (older schedules,
    /// other paired devices, imports). Only an `owner` schedule runs without cards on a bot
    /// at the Full level; confirm an `unknown` one with `POST /v1/schedules/{id}/confirm`.
    pub author: String,
    /// Version of the definition (it grows with every change to it). `POST
    /// /v1/schedules/{id}/confirm` names the one you looked at.
    pub revision: i64,
    /// Occurrences that never ran because the daemon stopped (for example the
    /// computer was off or asleep). They are not replayed; shown as "missed".
    #[serde(default)]
    pub missed_occurrences: u32,
    /// Name of the server this schedule was moved to. The local copy is then disabled
    /// (not deleted) so nothing runs twice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub moved_to: Option<String>,
    /// Next up to 5 upcoming firing timestamps (Unix epoch ms).
    #[serde(default)]
    pub next_runs_ms: Vec<i64>,
}

/// Body of `POST /v1/schedules` and `PUT /v1/schedules/{id}`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ScheduleInput {
    /// Bot that runs it (cannot be changed afterwards).
    pub bot_id: Uuid,
    /// Chat the runs post to (default: a chat named after the schedule).
    #[serde(default)]
    pub chat_id: Option<Uuid>,
    /// Display name (must not be empty).
    pub name: String,
    /// See [`ScheduleDto::spec`].
    pub spec: Value,
    /// See [`ScheduleDto::payload`].
    pub payload: Value,
    /// Armed or not (default `true`).
    #[serde(default)]
    pub enabled: Option<bool>,
    /// `skip` (default), `run_once` or `run_all`.
    #[serde(default)]
    pub misfire_policy: Option<String>,
    /// Catch-up window in seconds (1 to 30 days); `0` removes it. Absent keeps
    /// the current value (none on create).
    #[serde(default)]
    pub catchup_window_secs: Option<u64>,
    /// HMAC secret of a webhook trigger.  Absent: unchanged (none on create);
    /// the empty string removes the trigger.
    #[serde(default)]
    pub webhook_secret: Option<String>,
    /// File-watch or email trigger. Absent keeps it; null removes it.
    #[serde(default, deserialize_with = "present_json")]
    pub trigger: Option<Value>,
    /// `HashChanged` or Script precheck. Absent keeps it; null removes it.
    #[serde(default, deserialize_with = "present_json")]
    pub precheck: Option<Value>,
}

fn present_json<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(d).map(Some)
}

/// Body of the confirm calls (`POST /v1/schedules/{id}/confirm`,
/// `POST /v1/bots/{bot}/triggers/{trigger}/confirm`).
#[derive(Debug, Clone, Copy, Deserialize, ToSchema)]
pub struct ConfirmAuthorInput {
    /// The `revision` of the definition you looked at; the confirmation applies to that
    /// version only.
    pub revision: i64,
}

/// Body of `POST /v1/schedules/parse`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ParseSchedule {
    /// English or Turkish phrase, e.g. `every weekday at 9`, `her gün 09:00`.
    pub text: String,
    /// IANA timezone for calendar phrases (default UTC).
    #[serde(default)]
    pub tz: Option<String>,
}

/// Result of `POST /v1/schedules/parse` and `POST /v1/schedules/preview`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ParsedSchedule {
    /// The [`ScheduleDto::spec`] the phrase means.
    pub spec: Value,
    /// First firing from now.
    pub next_run_at_ms: Option<i64>,
    /// Next up to 5 (or requested `count`) upcoming firing timestamps (Unix epoch ms).
    #[serde(default)]
    pub next_runs_ms: Vec<i64>,
}

/// Body of `POST /v1/schedules/preview`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct PreviewSchedule {
    /// Schedule specification (`{"cron": ...}`, `{"every": ...}`, `{"at": ...}`).
    #[serde(default)]
    pub spec: Option<Value>,
    /// Natural-language phrase (used when `spec` is omitted).
    #[serde(default)]
    pub text: Option<String>,
    /// Optional IANA timezone applied to 5-field cron expressions without an embedded timezone.
    #[serde(default)]
    pub tz: Option<String>,
    /// Number of upcoming occurrences to compute (default 5, max 100).
    #[serde(default)]
    pub count: Option<usize>,
    /// Anchor timestamp in Unix ms (default: now).
    #[serde(default)]
    pub from_ms: Option<i64>,
}

/// One entry in a schedule's run history (`GET /v1/schedules/{id}/history`).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ScheduleHistoryEntryDto {
    /// Scheduled due timestamp (Unix ms), present for timer occurrences.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_ms: Option<i64>,
    /// Linked run id, if a run was created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<Uuid>,
    /// Chat id where the run posted, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<Uuid>,
    /// Outcome or status (`pending`, `dispatched`, `started`, `queued`, `running`,
    /// `waiting_approval`, `paused`, `succeeded`, `failed`, `cancelled`, `missed`, `interrupted`).
    pub status: String,
    /// When the run started (Unix ms), if it started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<i64>,
    /// When the run finished (Unix ms), if it reached a terminal run state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at_ms: Option<i64>,
    /// Failure message, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Whether this occurrence can be retried via `POST /v1/schedules/{id}/occurrences/{due_ms}/retry`.
    pub retryable: bool,
}

/// Answer of `POST /v1/schedules/{id}/run-now` and `POST /v1/webhooks/{id}`:
/// the run goes on in the background (follow it in the events, with the
/// `schedule_id` of its `run_started`).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AcceptedResponse {
    /// Always `accepted`.
    pub status: String,
    /// The schedule that fires.
    pub schedule_id: Uuid,
}

/// A messaging channel (e.g. Telegram) as seen by the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ChannelDto {
    /// Channel id (`telegram`).
    pub id: String,
    /// Credentials are stored.
    pub configured: bool,
    /// The user has not turned the channel off.
    pub enabled: bool,
    /// The channel is running inside this daemon.
    pub running: bool,
    /// Number of paired (allowed) remote users.
    pub paired_users: usize,
    /// Polling lifecycle, including a persistent Telegram conflict notification.
    pub status: Option<String>,
}

/// A one-time pairing code (`POST /v1/channels/telegram/pair-code`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PairCodeDto {
    /// The 6-digit code the user sends as `/pair <code>`.
    pub code: String,
    /// Seconds until it expires.
    pub expires_in_secs: u64,
}

/// What happened to a schedule (see [`ScheduleChanged`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleChangeKind {
    /// `POST /v1/schedules`.
    Created,
    /// `PUT /v1/schedules/{id}` (definition changed).
    Updated,
    /// `PUT /v1/schedules/{id}` flipped `enabled`.
    Toggled,
    /// `DELETE /v1/schedules/{id}`.
    Deleted,
}

/// Data of the `schedule_changed` SSE event of the daemon-wide stream
/// (`GET /v1/events` without `run_id`).  It is a notification only (refetch
/// the schedule): it has no SSE `id`, is not replayed with `Last-Event-ID`
/// and is not part of `protocol::Envelope`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ScheduleChanged {
    /// Always `schedule_changed`.
    #[serde(rename = "type")]
    pub kind_tag: String,
    /// The schedule.
    pub schedule_id: Uuid,
    /// What happened.
    pub kind: ScheduleChangeKind,
}

impl ScheduleChanged {
    /// The notification for `schedule_id`.
    #[must_use]
    pub fn new(schedule_id: Uuid, kind: ScheduleChangeKind) -> Self {
        Self {
            kind_tag: "schedule_changed".to_owned(),
            schedule_id,
            kind,
        }
    }
}

/// Body of `PUT /v1/view/privileges/{peer_key}` (local user only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ScreenPrivilegesInput {
    /// May watch the screen (`screen:view`).
    pub view: bool,
    /// May take over and drive input (`screen:input`); requires `view`.
    pub input: bool,
}

/// Data of the `control_state_changed` SSE event of the daemon-wide stream: a
/// screen takeover was acquired, became active, was interrupted, released or
/// recovered. A notification only (refetch `GET /v1/control/state`): no SSE
/// `id`, not replayed, not an envelope. It names who and which target, never
/// takeover ids, epochs, pixels, text or keys.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ControlStateChanged {
    /// Always `control_state_changed`.
    #[serde(rename = "type")]
    pub kind_tag: String,
    /// `acquiring`, `active`, `paused`, `released` or `recovered`.
    pub state: String,
    /// Displayable actor identity (`local-ui:...` or `peer:<key>`).
    pub owner: String,
    /// The target the takeover addressed.
    pub target_id: Uuid,
    /// Why the takeover stopped, if it was interrupted.
    pub reason: Option<live_view::Reason>,
}

impl ControlStateChanged {
    /// The notification for a takeover state change.
    #[must_use]
    pub fn new(
        state: &str,
        owner: &str,
        target_id: Uuid,
        reason: Option<live_view::Reason>,
    ) -> Self {
        Self {
            kind_tag: "control_state_changed".to_owned(),
            state: state.to_owned(),
            owner: owner.to_owned(),
            target_id,
            reason,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ReleaseResult {
    pub takeover_id: Uuid,
    pub request_id: Uuid,
    pub state: String,
}

/// Which runs a Stop targets (`POST /v1/runs/stop`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum StopScope {
    /// Every non-terminal run of the user that exists when the daemon first
    /// receives the `stop_id`, plus the listed ids. Local user only.
    AllExisting,
    /// Exactly the listed ids that exist and are cancellable.
    Listed,
}

/// Body of `POST /v1/runs/stop`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct StopRunsInput {
    /// One user decision to stop, minted by the caller. The daemon fixes the
    /// target set the first time it sees this id; a repeat never changes it.
    pub stop_id: Uuid,
    /// Runs the caller knew about (at most 1024). Only the first receipt reads them.
    #[serde(default)]
    pub run_ids: Vec<Uuid>,
    /// Which runs the Stop targets besides `run_ids`.
    pub scope: StopScope,
    /// Hard stop: besides firing the cancel token, abort the run's task so a
    /// run stuck in work that ignores the token still ends (its process
    /// trees and requests are dropped). Applies to this receipt and to repeats.
    #[serde(default)]
    pub force: bool,
}

/// What happened to one run of a Stop's target set.
///
/// `cancelled` is the only outcome that means the run has stopped. For a live
/// run the daemon can only fire the run's cancel token: `cancel_requested`
/// says it did so, and the run's effects (a tool call, a shell child, a
/// browser or desktop action) have ceased only once its `state` is terminal or
/// `run_finished` arrives. A human takeover is not touched by this route
/// (`/v1/control/override` handles it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum StopOutcome {
    /// The run is cancelled (terminal) because of this Stop.
    Cancelled,
    /// The cancel was issued but the run is not terminal yet. A repeat of the
    /// same `stop_id` re-issues it and updates this outcome once the run ended.
    CancelRequested,
    /// The run was already finished when the Stop reached the daemon (or ended
    /// by itself before the cancel took effect).
    AlreadyTerminal,
    /// The id is not a run of this user. Never cancelled later, even if a run
    /// with that id appears.
    NotFound,
    /// The cancel failed; a repeat of the same `stop_id` retries it.
    Failed,
}

/// One member of a Stop's target set.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct StopRunResult {
    /// Run id.
    pub run_id: Uuid,
    /// Cancel outcome.
    pub outcome: StopOutcome,
    /// Current run state, if the run exists.
    pub state: Option<String>,
    /// Why the cancel failed.
    pub error: Option<String>,
}

/// Response of `POST /v1/runs/stop`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct StopRunsResponse {
    /// Echo of the request's `stop_id`.
    pub stop_id: Uuid,
    /// `true` when this call fixed the target set, `false` for a repeat.
    pub first_seen: bool,
    /// `true` when this `stop_id` was seen before but its target set is no
    /// longer kept (expired or evicted): nothing was cancelled and `results`
    /// is empty. The original target set is unknown, so the daemon will not
    /// guess it. To retry, send a NEW `stop_id` with `scope: listed` and the
    /// run ids the client still holds as stop-held. `all_existing` with a new
    /// `stop_id` sweeps every run open now, including runs started after the
    /// original Stop: use it only for a new user decision. The client should
    /// tell the user that the Stop expired.
    #[serde(default)]
    pub expired: bool,
    /// `false` when the audit row of this call could not be written. Only the
    /// local user's Stop proceeds without it (peers fail with 500).
    #[serde(default = "audited_default")]
    pub audited: bool,
    /// Every member of the target set. A run that is not listed was not
    /// targeted (it was created after the first receipt). A repeat retries
    /// `failed` members and re-issues the cancel of `cancel_requested` ones;
    /// terminal and unknown members are never touched again.
    pub results: Vec<StopRunResult>,
}

fn audited_default() -> bool {
    true
}
