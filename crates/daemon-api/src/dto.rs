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
    /// Capabilities of this host; probes never launch a browser.
    pub host: HostInfo,
    /// Seconds since the daemon started.
    pub uptime_secs: u64,
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
}

impl HostInfo {
    #[must_use]
    pub fn detect() -> Self {
        Self {
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            has_display: browser_cdp::host::has_display(),
            has_browser: browser_cdp::find_browser().is_ok(),
            total_ram_mb: browser_cdp::host::total_ram_bytes() / (1024 * 1024),
        }
    }
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
    /// Same as `id`, under the name clients that treat `POST /v1/chats` as
    /// "create chat" expect.
    pub chat_id: Uuid,
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
    /// The message was `/stop`: the running turn was cancelled (`run_id` is that turn).
    Stopped,
}

/// Response of a sent message.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct SendResponse {
    /// How it was delivered.
    pub delivery: DeliveryKind,
    /// The run to follow (`null` when queued: the turn has no run yet).
    pub run_id: Option<Uuid>,
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
    /// Model ids the provider reports (empty without a listing).
    pub models: Vec<String>,
    /// Why the model list could not be fetched.
    pub models_error: Option<String>,
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
    /// Engine id (`claude`, `codex`, `cursor`, `opencode`, `gemini`, `chatgpt-plan`).
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
    /// `{"kind": "tool", "name": "...", "input": {}}`.
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
    /// A webhook trigger is configured (`POST /v1/webhooks/{id}`); the secret
    /// is never returned.
    pub has_webhook: bool,
    /// Who created it: `user`, or `bot` (a bot's schedule tools, after you
    /// approved it). A bot may change only its own `bot` schedules.
    pub origin: String,
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
    /// HMAC secret of a webhook trigger.  Absent: unchanged (none on create);
    /// the empty string removes the trigger.
    #[serde(default)]
    pub webhook_secret: Option<String>,
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

/// Result of `POST /v1/schedules/parse`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ParsedSchedule {
    /// The [`ScheduleDto::spec`] the phrase means.
    pub spec: Value,
    /// First firing from now.
    pub next_run_at_ms: Option<i64>,
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
