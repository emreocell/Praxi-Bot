//! A narrow loopback facade for the stdio MCP server. Scoped bearers have no
//! access to ordinary daemon endpoints, approvals, secrets or configuration.
//! The secret store holds both the credential and its grant; neither travels
//! in a state bundle. No full owner credential is given to the stdio process.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Extension, Path, Query, State};
use axum::http::{Method, StatusCode};
use protocol::{BotId, ChatId, RunId};
use secret_store::{SecretStore, SecretValue};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use storage::now_ms;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::error::{ApiError, ErrorBody};
use crate::{AppState, PeerIdentity};

const PREFIX: &str = "praxi-mcp.";
const META_PREFIX: &str = "mcp-server/meta/";
const TOKEN_PREFIX: &str = "mcp-server/token/";
const MAX_TOKENS: usize = 128;
const MAX_TTL: u64 = 30 * 24 * 60 * 60;
const MAX_INPUT: usize = 32 * 1024;
const MAX_RESULT: usize = 64 * 1024;
/// The MCP client and daemon use the same approval wait ceiling.
pub const MCP_APPROVAL_WAIT: Duration = Duration::from_secs(10 * 60);

/// Independent MCP capabilities. Neither permits owner management endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
pub enum McpScope {
    #[serde(rename = "mcp:tasks")]
    Tasks,
    #[serde(rename = "mcp:computer")]
    Computer,
}

/// Nonsecret grant, returned to the owner after storing a credential.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct McpTokenGrant {
    pub id: Uuid,
    pub bots: Vec<Uuid>,
    pub scopes: BTreeSet<McpScope>,
    pub expires_at_ms: i64,
}

impl McpTokenGrant {
    fn valid(&self) -> bool {
        !self.bots.is_empty()
            && self.bots.len() <= 16
            && !self.scopes.is_empty()
            && self.expires_at_ms > now_ms()
    }
}

/// Owner request: create a credential in the secret store. Only its id is
/// returned; `bot mcp serve` reads that one scoped credential by id.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateMcpToken {
    pub bots: Vec<Uuid>,
    pub scopes: BTreeSet<McpScope>,
    #[serde(default = "default_ttl")]
    pub ttl_seconds: u64,
}

const fn default_ttl() -> u64 {
    MAX_TTL
}

/// Transport-authenticated scoped identity. HTTP input cannot construct this.
#[derive(Clone)]
pub(crate) struct ScopedPrincipal(pub McpTokenGrant);

/// Per-daemon admission state; persisted run kinds enforce concurrency across
/// restarts. The lock also serializes token mutation and task creation.
#[derive(Default)]
pub(crate) struct McpServerBook {
    calls: HashMap<Uuid, VecDeque<Instant>>,
}

impl McpServerBook {
    fn admit(&mut self, id: Uuid) -> Result<(), ApiError> {
        let now = Instant::now();
        self.calls.retain(|_, calls| {
            while calls
                .front()
                .is_some_and(|at| now.duration_since(*at) >= Duration::from_secs(60))
            {
                calls.pop_front();
            }
            !calls.is_empty()
        });
        let calls = self.calls.entry(id).or_default();
        if calls.len() >= 30 {
            return Err(rate_error("MCP token allows 30 tool calls per minute"));
        }
        calls.push_back(now);
        Ok(())
    }
}

fn rate_error(text: &'static str) -> ApiError {
    ApiError::new(StatusCode::TOO_MANY_REQUESTS, "mcp_rate_limit", text)
}

fn meta_name(id: Uuid) -> String {
    format!("{META_PREFIX}{id}")
}
fn token_name(id: Uuid) -> String {
    format!("{TOKEN_PREFIX}{id}")
}

fn load_grant(store: &dyn SecretStore, id: Uuid) -> Option<McpTokenGrant> {
    let raw = store.get(&meta_name(id)).ok()??;
    if raw.expose().len() > 4096 {
        return None;
    }
    let grant: McpTokenGrant = serde_json::from_str(raw.expose()).ok()?;
    (grant.id == id && grant.valid()).then_some(grant)
}

/// Request only reserved scoped credential records through the store API,
/// never the owner `api-token` file. Without `id`, one active matching grant is
/// required, avoiding an implicit choice of a wider authority.
///
/// # Errors
/// Missing, expired, ambiguous or insufficient grants; secret-store failures.
pub fn client_credential(
    store: &dyn SecretStore,
    bot: Uuid,
    id: Option<Uuid>,
    scopes: &BTreeSet<McpScope>,
) -> Result<SecretValue, &'static str> {
    let candidates = if let Some(id) = id {
        vec![id]
    } else {
        store
            .list_names()
            .map_err(|_| "cannot list scoped credentials")?
            .iter()
            .filter_map(|name| name.strip_prefix(META_PREFIX)?.parse().ok())
            .collect()
    };
    let mut eligible = candidates
        .into_iter()
        .filter_map(|id| load_grant(store, id))
        .filter(|g| g.bots.contains(&bot) && scopes.is_subset(&g.scopes));
    let grant = eligible
        .next()
        .ok_or("no matching scoped token; run bot token create first")?;
    if eligible.next().is_some() {
        return Err("multiple matching scoped tokens; specify --token-id");
    }
    store
        .get(&token_name(grant.id))
        .map_err(|_| "cannot read scoped credential")?
        .ok_or("scoped credential is missing")
}

pub(crate) fn looks_scoped(token: &str) -> bool {
    token.starts_with(PREFIX)
}

/// Only these canonical endpoints accept a scoped token. Encoding aliases,
/// redundant/trailing slashes and query credentials never enlarge authority.
pub(crate) fn scoped_path_allowed(method: &Method, path: &str) -> bool {
    (*method == Method::GET && path == "/v1/mcp-server/tools")
        || (*method == Method::POST && path == "/v1/mcp-server/call")
}

pub(crate) fn authenticate(state: &AppState, token: &str) -> Option<ScopedPrincipal> {
    let (id, entropy) = token.strip_prefix(PREFIX)?.split_once('.')?;
    if entropy.len() != 64 || !entropy.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let id: Uuid = id.parse().ok()?;
    let store = state.secret_store()?;
    let grant = load_grant(store.as_ref(), id)?;
    let expected = store.get(&token_name(id)).ok()??;
    let a = expected.expose().as_bytes();
    let b = token.as_bytes();
    (a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0)
        .then_some(ScopedPrincipal(grant))
}

fn owner_only(
    peer: Option<&Extension<PeerIdentity>>,
    scoped: Option<&Extension<ScopedPrincipal>>,
) -> Result<(), ApiError> {
    if peer.is_some() || scoped.is_some() {
        Err(ApiError::forbidden(
            "only the local owner manages MCP tokens",
        ))
    } else {
        Ok(())
    }
}

fn store(state: &AppState) -> Result<Arc<dyn SecretStore>, ApiError> {
    state
        .secret_store()
        .ok_or_else(|| ApiError::unavailable("secret store is unavailable"))
}

fn audit(
    state: &AppState,
    operation: &'static str,
    token: Uuid,
    bot: Option<Uuid>,
) -> Result<(), ApiError> {
    // Payload contains identifiers and fixed operation labels only, never
    // tokens, task text, tool arguments or a client-selected operation name.
    state
        .storage()
        .append_audit(
            state.user_id(),
            None,
            "local",
            operation,
            &json!({"credential_id":token,"bot_id":bot}),
        )
        .map(|_| ())
        .map_err(|_| ApiError::internal("MCP action audit could not be persisted"))
}

/// Create a bot-bound token in the secret store (local owner only).
#[utoipa::path(post, path = "/v1/mcp-server/tokens", tag = "mcp-server", security(("bearer" = [])),
    request_body = CreateMcpToken,
    responses((status = 201, body = McpTokenGrant), (status = 403, body = ErrorBody),
              (status = 422, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub(crate) async fn create_token(
    State(state): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    scoped: Option<Extension<ScopedPrincipal>>,
    input: Result<Json<CreateMcpToken>, axum::extract::rejection::JsonRejection>,
) -> Result<(StatusCode, Json<McpTokenGrant>), ApiError> {
    owner_only(peer.as_ref(), scoped.as_ref())?;
    let Json(mut input) = input.map_err(|_| ApiError::bad_request("invalid MCP token request"))?;
    if input.bots.is_empty()
        || input.bots.len() > 16
        || input.scopes.is_empty()
        || !(60..=MAX_TTL).contains(&input.ttl_seconds)
    {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_mcp_grant",
            "grant needs 1–16 bots, a scope and a TTL of 60 seconds to 30 days",
        ));
    }
    input.bots.sort_unstable();
    input.bots.dedup();
    for id in &input.bots {
        state.bot_of_user(BotId(*id))?;
    }
    let store = store(&state)?;
    let _book = state.0.mcp_server.lock();
    let count = store
        .list_names()
        .map_err(|_| ApiError::internal("cannot list MCP tokens"))?
        .iter()
        .filter(|n| n.starts_with(META_PREFIX))
        .count();
    if count >= MAX_TOKENS {
        return Err(ApiError::conflict(
            "revoke an old MCP token before creating another",
        ));
    }
    let id = Uuid::now_v7();
    let grant = McpTokenGrant {
        id,
        bots: input.bots,
        scopes: input.scopes,
        expires_at_ms: now_ms()
            .saturating_add(i64::try_from(input.ttl_seconds * 1000).unwrap_or(i64::MAX)),
    };
    let mut random = [0u8; 32];
    getrandom::fill(&mut random)
        .map_err(|_| ApiError::internal("credential generation unavailable"))?;
    let mut entropy = String::with_capacity(64);
    for b in random {
        let _ = write!(entropy, "{b:02x}");
    }
    let token = SecretValue::new(format!("{PREFIX}{id}.{entropy}"));
    audit(&state, "mcp.token_created", id, None)?;
    store
        .set(&token_name(id), token.expose())
        .map_err(|_| ApiError::internal("cannot store MCP token"))?;
    let json =
        serde_json::to_string(&grant).map_err(|_| ApiError::internal("cannot encode MCP grant"))?;
    if store.set(&meta_name(id), &json).is_err() {
        let _ = store.delete(&token_name(id));
        return Err(ApiError::internal("cannot store MCP grant"));
    }
    state.agent().refresh_external_redactor();
    Ok((StatusCode::CREATED, Json(grant)))
}

/// Revoke a token and cancel its outstanding tasks (local owner only).
#[utoipa::path(delete, path = "/v1/mcp-server/tokens/{id}", tag = "mcp-server", security(("bearer" = [])),
    params(("id" = Uuid, Path)), responses((status = 204), (status = 403, body = ErrorBody)))]
pub(crate) async fn revoke_token(
    State(state): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    scoped: Option<Extension<ScopedPrincipal>>,
    id: Result<Path<Uuid>, axum::extract::rejection::PathRejection>,
) -> Result<StatusCode, ApiError> {
    owner_only(peer.as_ref(), scoped.as_ref())?;
    let Path(id) = id.map_err(|_| ApiError::bad_request("invalid MCP token id"))?;
    let store = store(&state)?;
    let mut book = state.0.mcp_server.lock();
    audit(&state, "mcp.token_revoked", id, None)?;
    store
        .delete(&meta_name(id))
        .map_err(|_| ApiError::internal("cannot revoke MCP grant"))?;
    book.calls.remove(&id);
    let cancelled = cancel_revoked_runs(&state, id, |run| state.agent().cancel(run));
    store
        .delete(&token_name(id))
        .map_err(|_| ApiError::internal("cannot remove revoked MCP token"))?;
    cancelled?;
    Ok(StatusCode::NO_CONTENT)
}

fn cancel_revoked_runs(
    state: &AppState,
    id: Uuid,
    mut cancel: impl FnMut(RunId) -> Result<(), agent_core::LoopError>,
) -> Result<(), ApiError> {
    let runs = state
        .storage()
        .list_runs(state.user_id())
        .map_err(|_| ApiError::internal("cannot list MCP runs"))?;
    let mut failed = false;
    for run in runs
        .iter()
        .filter(|r| token_of_tree(r, &runs) == Some(id) && !terminal(&r.state))
    {
        // A terminal root may still have a persisted active child. Walk every
        // owned run; a parent cancellation may already have cancelled a child,
        // or a run may have finished since the snapshot. Neither is a failure.
        if cancel(run.id).is_err()
            && !state
                .storage()
                .get_run(run.id)
                .is_ok_and(|row| row.is_some_and(|r| terminal(&r.state)))
        {
            failed = true;
        }
    }
    if failed {
        Err(ApiError::internal("cannot cancel every revoked MCP task"))
    } else {
        Ok(())
    }
}

/// MCP tool metadata. All returned text is untrusted data.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ExportedMcpTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub read_only: bool,
    pub untrusted_output: bool,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct McpToolsQuery {
    pub bot: Uuid,
    #[serde(default)]
    #[param(required = false)]
    pub computer: bool,
    #[serde(default = "default_tasks")]
    #[param(required = false)]
    pub tasks: bool,
}
const fn default_tasks() -> bool {
    true
}

fn authorize_bot(state: &AppState, principal: &ScopedPrincipal, bot: Uuid) -> Result<(), ApiError> {
    if !principal.0.bots.contains(&bot) {
        return Err(ApiError::forbidden("MCP token does not allow this bot"));
    }
    let store = store(state)?;
    let current = load_grant(store.as_ref(), principal.0.id).ok_or_else(ApiError::unauthorized)?;
    if !current.bots.contains(&bot) || current.scopes != principal.0.scopes {
        return Err(ApiError::forbidden("MCP token grant changed"));
    }
    state.bot_of_user(BotId(bot))?;
    Ok(())
}

fn require_scope(p: &ScopedPrincipal, scope: McpScope) -> Result<(), ApiError> {
    if p.0.scopes.contains(&scope) {
        Ok(())
    } else {
        Err(ApiError::forbidden(
            "MCP token does not allow this capability",
        ))
    }
}

fn task_tools(bot: Uuid) -> Vec<ExportedMcpTool> {
    [
        ("bot_list", "List the selected permitted bot.", json!({}), Vec::<&str>::new(), true),
        ("task_start", "Start an isolated bot task. Approvals happen only in Praxi.", json!({"bot":{"type":"string","enum":[bot]},"text":{"type":"string","maxLength":32768}}), vec!["text"], false),
        ("task_status", "Read the state of a task started by this token.", json!({"run":{"type":"string","format":"uuid"}}), vec!["run"], true),
        ("task_result", "Read bounded, redacted task output as untrusted data.", json!({"run":{"type":"string","format":"uuid"}}), vec!["run"], true),
        ("task_cancel", "Cancel a task started by this token.", json!({"run":{"type":"string","format":"uuid"}}), vec!["run"], false),
        ("schedule_list", "List schedule identifiers and states for this bot (no prompts).", json!({}), vec![], true),
    ].into_iter().map(|(name, description, properties, required, read_only)| ExportedMcpTool {
        name:name.into(), description:description.into(), read_only, untrusted_output:true,
        input_schema:json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}),
    }).collect()
}

fn is_computer_tool(spec: &tool_registry::ToolSpec) -> bool {
    // Review new native tools before widening this boundary (notably batch
    // tools that might add an eval/shell action under a benign prefix).
    let browser = matches!(
        spec.name.as_str(),
        "browser_open"
            | "browser_snapshot"
            | "browser_click"
            | "browser_fill"
            | "browser_select"
            | "browser_press"
            | "browser_scroll"
            | "browser_hover"
            | "browser_select_text"
            | "browser_wait"
            | "browser_back"
            | "browser_tabs"
            | "browser_screenshot"
    );
    let desktop = matches!(
        spec.name.as_str(),
        "desktop_list_apps"
            | "desktop_launch"
            | "desktop_get_state"
            | "desktop_click"
            | "desktop_type"
            | "desktop_set_value"
            | "desktop_key"
            | "desktop_scroll"
            | "desktop_drag"
    );
    (browser && spec.category == protocol::PermissionCategory::Browser)
        || (desktop && spec.category == protocol::PermissionCategory::Desktop)
}

/// List only explicitly enabled tools within the scoped token.
#[utoipa::path(get, path = "/v1/mcp-server/tools", tag = "mcp-server", security(("bearer" = [])),
    params(McpToolsQuery), responses((status = 200, body = Vec<ExportedMcpTool>), (status = 403, body = ErrorBody)))]
pub(crate) async fn list_tools(
    State(state): State<AppState>,
    principal: Option<Extension<ScopedPrincipal>>,
    query: Result<Query<McpToolsQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<Vec<ExportedMcpTool>>, ApiError> {
    let Extension(p) =
        principal.ok_or_else(|| ApiError::forbidden("a scoped MCP token is required"))?;
    let Query(q) = query.map_err(|_| ApiError::bad_request("invalid MCP tool query"))?;
    authorize_bot(&state, &p, q.bot)?;
    audit(&state, "mcp.tools_listed", p.0.id, Some(q.bot))?;
    state.agent().refresh_external_redactor();
    let mut tools = Vec::new();
    if q.tasks {
        require_scope(&p, McpScope::Tasks)?;
        tools.extend(task_tools(q.bot));
    }
    if q.computer {
        require_scope(&p, McpScope::Computer)?;
        let bot = state.bot_of_user(BotId(q.bot))?;
        let profile: agent_core::BotProfile = serde_json::from_value(bot.config)
            .map_err(|_| ApiError::internal("bot profile is invalid"))?;
        for spec in state.agent().tool_registry().list() {
            if is_computer_tool(&spec)
                && state.agent().redact_external_text(spec.name.clone()) == spec.name
                && profile.tool_visible(&spec.name)
                && profile.category_visible(spec.category)
                && state
                    .agent()
                    .tool_registry()
                    .get(&spec.name)
                    .is_some_and(|t| t.visible_to(Some(&q.bot.to_string())))
            {
                tools.push(ExportedMcpTool {
                    name: spec.name.clone(),
                    description: state.agent().redact_external_text(spec.description.clone()),
                    input_schema: scrub_json(&state, &spec.input_schema),
                    read_only: spec.effect == tool_registry::Effect::Read,
                    untrusted_output: true,
                });
            }
        }
    }
    if serde_json::to_vec(&tools)
        .map_err(|_| ApiError::internal("cannot encode MCP tools"))?
        .len()
        > MAX_RESULT
    {
        return Err(ApiError::unavailable(
            "MCP tool catalog exceeds its output budget",
        ));
    }
    Ok(Json(tools))
}

/// One MCP operation. `bot` is bound by the stdio server's --bot setting.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct McpCallInput {
    pub bot: Uuid,
    pub name: String,
    #[serde(default = "empty_object")]
    pub arguments: Value,
}
fn empty_object() -> Value {
    json!({})
}

fn token_of_run(run: &storage::Run) -> Option<Uuid> {
    run.kind.strip_prefix("turn:external_agent:")?.parse().ok()
}

fn token_of_tree(run: &storage::Run, runs: &[storage::Run]) -> Option<Uuid> {
    let mut ancestor = run;
    // The runtime permits depth two. Bound malformed imported ancestry too.
    for _ in 0..16 {
        let Some(parent) = ancestor.parent_run_id else {
            return token_of_run(ancestor);
        };
        ancestor = runs
            .iter()
            .find(|r| r.id == parent && r.user_id == run.user_id && r.bot_id == run.bot_id)?;
    }
    None
}

fn terminal(state: &str) -> bool {
    matches!(state, "succeeded" | "failed" | "cancelled")
}

fn owned_run(
    state: &AppState,
    p: &ScopedPrincipal,
    bot: Uuid,
    args: &Value,
) -> Result<storage::Run, ApiError> {
    let id: Uuid = args["run"]
        .as_str()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| ApiError::bad_request("a valid run id is required"))?;
    let run = state
        .storage()
        .get_run(RunId(id))
        .map_err(|_| ApiError::internal("cannot read MCP task"))?
        .filter(|r| {
            r.user_id == state.user_id() && r.bot_id.0 == bot && token_of_run(r) == Some(p.0.id)
        })
        .ok_or_else(|| ApiError::forbidden("task is not owned by this MCP token and bot"))?;
    Ok(run)
}

fn start(
    state: &AppState,
    p: &ScopedPrincipal,
    bot: Uuid,
    work: agent_core::ScheduleWork,
) -> Result<RunId, ApiError> {
    let _book = state.0.mcp_server.lock();
    authorize_bot(state, p, bot)?;
    let runs = state
        .storage()
        .list_runs(state.user_id())
        .map_err(|_| ApiError::internal("cannot count MCP tasks"))?;
    let active = runs
        .iter()
        .filter(|r| token_of_tree(r, &runs) == Some(p.0.id) && !terminal(&r.state))
        .count();
    if active >= 2 {
        return Err(rate_error("MCP token allows two concurrent tasks"));
    }
    audit(state, "mcp.task_started", p.0.id, Some(bot))?;
    let chat = storage::Chat {
        id: ChatId::new(),
        user_id: state.user_id(),
        bot_id: BotId(bot),
        title: "External agent task".into(),
        created_at_ms: now_ms(),
    };
    state
        .storage()
        .insert_chat(&chat)
        .map_err(|_| ApiError::internal("cannot create MCP task chat"))?;
    state
        .agent()
        .run_external(chat.id, p.0.id, work)
        .map_err(|_| {
            ApiError::conflict("MCP task could not start; check the selected bot and daemon state")
        })
}

fn status(run: &storage::Run) -> Value {
    json!({"run":run.id,"bot":run.bot_id,"state":run.state,"initiator":"external_agent"})
}

fn result(state: &AppState, run: &storage::Run) -> Result<Value, ApiError> {
    let mut answer = status(run);
    answer["untrusted"] = json!(true);
    if !terminal(&run.state) {
        answer["ready"] = json!(false);
        return Ok(answer);
    }
    answer["ready"] = json!(true);
    answer["is_error"] = json!(run.state != "succeeded");
    if let Some(chat) = run.chat_id {
        let messages = state
            .storage()
            .list_messages(chat)
            .map_err(|_| ApiError::internal("cannot read MCP result"))?;
        if let Some(message) = messages
            .iter()
            .rev()
            .find(|m| m.run_id == Some(run.id) && m.role == "assistant")
        {
            answer["text"] = json!(message.content);
        }
    }
    if answer.get("text").is_none() {
        let steps = state
            .storage()
            .list_steps(run.id)
            .map_err(|_| ApiError::internal("cannot read MCP result"))?;
        let mut text = String::new();
        for entry in steps
            .iter()
            .filter(|s| s.name == "tools")
            .filter_map(|s| s.output.as_ref())
            .filter_map(|v| v.get("results"))
            .filter_map(Value::as_array)
            .flatten()
        {
            if entry["is_error"] == true {
                answer["is_error"] = json!(true);
            }
            if let Some(part) = entry["text"].as_str() {
                text.push_str(part);
                text.push('\n');
            }
            if text.len() > MAX_RESULT {
                break;
            }
        }
        if text.is_empty() && run.state != "succeeded" {
            text = "Task failed, was cancelled, or approval was refused.".into();
        }
        answer["text"] = json!(text);
    }
    Ok(bound_result(state, &answer))
}

fn scrub_json(state: &AppState, value: &Value) -> Value {
    match value {
        Value::String(s) => json!(state.agent().redact_external_text(s.clone())),
        Value::Array(items) => Value::Array(items.iter().map(|v| scrub_json(state, v)).collect()),
        Value::Object(items) => Value::Object(
            items
                .iter()
                .map(|(k, v)| {
                    (
                        state.agent().redact_external_text(k.clone()),
                        scrub_json(state, v),
                    )
                })
                .collect(),
        ),
        v => v.clone(),
    }
}

fn bound_result(state: &AppState, value: &Value) -> Value {
    state.agent().refresh_external_redactor();
    let value = scrub_json(state, value);
    let text = value.to_string();
    if text.len() <= MAX_RESULT {
        return value;
    }
    // JSON escaping can expand a character to six bytes; retain a conservative
    // prefix and bound the encoded envelope, not just the original string.
    let mut end = MAX_RESULT / 8;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    json!({"truncated":true,"untrusted":true,"is_error":value["is_error"],"text":&text[..end]})
}

/// A disconnected MCP call must not leave an approval that can execute later.
struct CancelComputerCall {
    agent: agent_core::AgentLoop,
    run: RunId,
}

impl Drop for CancelComputerCall {
    fn drop(&mut self) {
        let _ = self.agent.cancel(self.run);
    }
}

async fn computer_call(
    state: &AppState,
    p: &ScopedPrincipal,
    input: &McpCallInput,
) -> Result<Value, ApiError> {
    require_scope(p, McpScope::Computer)?;
    let tool = state
        .agent()
        .tool_registry()
        .get(&input.name)
        .filter(|t| is_computer_tool(t.spec()))
        .ok_or_else(|| ApiError::forbidden("computer tool is not exported"))?;
    if !tool.visible_to(Some(&input.bot.to_string())) {
        return Err(ApiError::forbidden(
            "computer tool is unavailable to this bot",
        ));
    }
    let run_id = start(
        state,
        p,
        input.bot,
        agent_core::ScheduleWork::Tool {
            name: input.name.clone(),
            input: input.arguments.clone(),
        },
    )?;
    let _pending = CancelComputerCall {
        agent: state.agent().clone(),
        run: run_id,
    };
    let deadline = tokio::time::Instant::now() + MCP_APPROVAL_WAIT;
    loop {
        let run = state
            .storage()
            .get_run(run_id)
            .map_err(|_| ApiError::internal("cannot read MCP task"))?
            .ok_or_else(|| ApiError::not_found("MCP task"))?;
        if terminal(&run.state) {
            return result(state, &run);
        }
        if tokio::time::Instant::now() >= deadline || authorize_bot(state, p, input.bot).is_err() {
            let _ = state.agent().cancel(run_id);
            return Err(ApiError::new(
                StatusCode::REQUEST_TIMEOUT,
                "mcp_approval_timeout",
                "MCP computer call expired or its credential was revoked; the task was cancelled",
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Dispatch a scoped task/computer operation through the ordinary agent loop.
#[utoipa::path(post, path = "/v1/mcp-server/call", tag = "mcp-server", security(("bearer" = [])),
    request_body = McpCallInput,
    responses((status = 200, body = Value), (status = 403, body = ErrorBody),
              (status = 429, body = ErrorBody), (status = 408, body = ErrorBody)))]
pub(crate) async fn call_tool(
    State(state): State<AppState>,
    principal: Option<Extension<ScopedPrincipal>>,
    input: Result<Json<McpCallInput>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Extension(p) =
        principal.ok_or_else(|| ApiError::forbidden("a scoped MCP token is required"))?;
    let Json(input) = input.map_err(|_| ApiError::bad_request("invalid MCP tool request"))?;
    authorize_bot(&state, &p, input.bot)?;
    if input.name.len() > 128
        || !input.arguments.is_object()
        || input.arguments.to_string().len() > MAX_INPUT
    {
        return Err(ApiError::bad_request(
            "invalid or oversized MCP tool arguments",
        ));
    }
    if input
        .arguments
        .get("bot")
        .is_some_and(|id| id.as_str().and_then(|s| s.parse::<Uuid>().ok()) != Some(input.bot))
    {
        return Err(ApiError::forbidden(
            "MCP server is bound to the selected bot",
        ));
    }
    state.0.mcp_server.lock().admit(p.0.id)?;
    audit(&state, "mcp.tool_called", p.0.id, Some(input.bot))?;
    let value = match input.name.as_str() {
        "bot_list" | "task_start" | "task_status" | "task_result" | "task_cancel"
        | "schedule_list" => {
            require_scope(&p, McpScope::Tasks)?;
            task_call(&state, &p, &input)?
        }
        _ => computer_call(&state, &p, &input).await?,
    };
    Ok(Json(bound_result(&state, &value)))
}

fn task_call(
    state: &AppState,
    p: &ScopedPrincipal,
    input: &McpCallInput,
) -> Result<Value, ApiError> {
    match input.name.as_str() {
        "bot_list" => {
            let bot = state.bot_of_user(BotId(input.bot))?;
            Ok(json!({"bots":[{"id":bot.id,"name":bot.name}],"untrusted":true}))
        }
        "task_start" => {
            let text = input.arguments["text"]
                .as_str()
                .filter(|t| !t.trim().is_empty() && t.len() <= MAX_INPUT)
                .ok_or_else(|| {
                    ApiError::bad_request("task text must be nonempty and at most 32 KiB")
                })?;
            let run = start(
                state,
                p,
                input.bot,
                agent_core::ScheduleWork::Prompt(text.into()),
            )?;
            Ok(json!({"run":run,"initiator":"external_agent"}))
        }
        "task_status" => Ok(status(&owned_run(state, p, input.bot, &input.arguments)?)),
        "task_result" => result(state, &owned_run(state, p, input.bot, &input.arguments)?),
        "task_cancel" => {
            let run = owned_run(state, p, input.bot, &input.arguments)?;
            audit(state, "mcp.task_cancelled", p.0.id, Some(input.bot))?;
            state
                .agent()
                .cancel(run.id)
                .map_err(|_| ApiError::conflict("MCP task could not be cancelled"))?;
            Ok(json!({"run":run.id,"cancelled":true}))
        }
        "schedule_list" => {
            let schedules = state
                .schedule_store()?
                .list_by_bot(BotId(input.bot))
                .map_err(|_| ApiError::internal("cannot list schedules"))?;
            Ok(
                json!({"schedules":schedules.iter().map(|s| json!({"id":s.id,"name":s.name,"enabled":s.enabled})).collect::<Vec<_>>(),"untrusted":true}),
            )
        }
        _ => Err(ApiError::forbidden("MCP operation is not exported")),
    }
}

#[cfg(test)]
mod tests;
