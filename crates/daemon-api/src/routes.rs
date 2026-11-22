//! Route handlers (`/v1`).

use agent_core::{
    ApprovalScope, BotProfile, Delivery, EffectivePolicy, Grant, GrantTarget, MessageMode,
    PolicyDecision, Scope as RuleScope, ScopeKind,
};
use axum::extract::{FromRequest, Path, Query, Request, State};
use axum::http::StatusCode;
use axum::{Json, extract::rejection::JsonRejection};
use permission_engine::{Level, Policy, Rule};
use protocol::{BotId, ChatId, Event, PermissionCategory, RunId, TaskId, ToolCallId};
use provider_catalog::Usage;
use scheduler::{MisfirePolicy, Payload, Schedule, ScheduleSpec, Trigger};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use storage::{Bot, Chat, now_ms};
use task_runtime::RunState;
use utoipa::IntoParams;
use uuid::Uuid;

use crate::AppState;
use crate::dto::{
    AcceptedResponse, AnswerQuestion, AnswerResponse, ApprovalDto, BotConflictBody, BotDto,
    BotInput, BotPatchInput, BotPatchKind, BotPatchOp, CatalogProviderDto, CategoryDecisionDto,
    ChatDto, ConfiguredProviderDto, ControlDecisionInput, ControlDecisionResponse, ControlStateDto,
    ControlStateInput, CreateChat, DecideApproval, Decision, DecisionResponse, DeliveryKind,
    DetectedProviderDto, EffectivePolicyDto, EnableCliProviderInput, GrantDto, GrantTargetDto,
    Health, LatestCursor, MessageDto, Mode, ParseSchedule, ParsedSchedule, PolicyDto, PolicyInput,
    PreviewSchedule, ProvidersDto, ReloadResponse, RememberResponse, ResendMessage, ResendResponse,
    RevokeGrant, RunDto, RunUsageDto, RuntimeConfigDto, ScheduleChangeKind, ScheduleDto,
    ScheduleHistoryEntryDto, ScheduleInput, Scope, SendMessage, SendResponse, UpdateChat,
};
use crate::error::{ApiError, ErrorBody};

/// JSON body extractor whose rejections use the API error format.
pub(crate) struct ApiJson<T>(pub T);

impl<S: Send + Sync, T: DeserializeOwned> FromRequest<S> for ApiJson<T> {
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, ApiError> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(v)) => Ok(Self(v)),
            Err(e @ JsonRejection::MissingJsonContentType(_)) => Err(ApiError::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "unsupported_media_type",
                e.body_text(),
            )),
            Err(e) => Err(ApiError::bad_request(e.body_text())),
        }
    }
}

pub(crate) fn id(s: &str) -> Result<Uuid, ApiError> {
    s.parse().map_err(|_| ApiError::bad_request("malformed id"))
}

pub(crate) fn bot_dto(b: Bot, version: i64) -> BotDto {
    let runtime = serde_json::from_value::<BotProfile>(b.config.clone())
        .ok()
        .map(|p| RuntimeConfigDto {
            computer: p.runtime.computer.as_str().to_owned(),
        });
    BotDto {
        id: b.id.0,
        name: b.name,
        config: b.config,
        created_at_ms: b.created_at_ms,
        version,
        effective_policy: None,
        runtime,
    }
}

fn category_name(c: PermissionCategory) -> String {
    serde_json::to_value(c)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn grant_dto(g: &Grant) -> GrantDto {
    GrantDto {
        computer: g.computer.clone(),
        target: match &g.target {
            GrantTarget::Tool(name) => GrantTargetDto::Tool { name: name.clone() },
            GrantTarget::Category(c) => GrantTargetDto::Category {
                name: category_name(*c),
            },
            GrantTarget::Scoped { tool, scope } => GrantTargetDto::Scoped {
                tool: tool.clone(),
                scope: scope.kind.as_str().to_owned(),
                pattern: scope.pattern.clone(),
            },
        },
        granted_at_ms: g.granted_at_ms,
        inert: g.inert,
    }
}

fn grant_target(t: GrantTargetDto) -> Result<GrantTarget, ApiError> {
    match t {
        GrantTargetDto::Tool { name } if !name.trim().is_empty() => Ok(GrantTarget::Tool(name)),
        GrantTargetDto::Tool { .. } => Err(ApiError::bad_request("tool name must not be empty")),
        GrantTargetDto::Scoped {
            tool,
            scope,
            pattern,
        } => match ScopeKind::parse(&scope) {
            Some(kind) if !tool.trim().is_empty() && !pattern.is_empty() => {
                Ok(GrantTarget::Scoped {
                    tool,
                    scope: RuleScope { kind, pattern },
                })
            }
            _ => Err(ApiError::bad_request("invalid scoped rule")),
        },
        GrantTargetDto::Category { name } => {
            match serde_json::from_value::<PermissionCategory>(Value::String(name.clone())) {
                // `meta` is always allowed; a grant on it would mean nothing.
                Ok(c) if c != PermissionCategory::Meta => Ok(GrantTarget::Category(c)),
                _ => Err(ApiError::bad_request(format!(
                    "unknown permission category {name:?}"
                ))),
            }
        }
    }
}

fn effective_policy_dto(e: EffectivePolicy) -> EffectivePolicyDto {
    EffectivePolicyDto {
        policy: e.policy.and_then(|p| serde_json::to_value(p).ok()),
        categories: e
            .categories
            .into_iter()
            .map(|(c, d)| {
                let decision = match d.decision {
                    PolicyDecision::Allow => "allow",
                    PolicyDecision::Ask { .. } => "ask",
                    PolicyDecision::Deny => "deny",
                };
                (
                    category_name(c),
                    CategoryDecisionDto {
                        decision: decision.to_owned(),
                        via_grant: d.via_grant,
                        scopes: d.scopes.clone(),
                    },
                )
            })
            .collect(),
        mode: e.mode.as_str().to_owned(),
        grants: e.grants.iter().map(grant_dto).collect(),
    }
}

fn chat_dto(c: Chat, flags: storage::ChatFlags) -> ChatDto {
    ChatDto {
        pinned: flags.pinned,
        archived: flags.archived,
        id: c.id.0,
        chat_id: c.id.0,
        bot_id: c.bot_id.0,
        title: c.title,
        created_at_ms: c.created_at_ms,
    }
}

fn run_dto(s: &AppState, r: storage::Run) -> RunDto {
    let can_remember = s.agent().can_remember(r.id);
    run_dto_with(r, can_remember)
}

fn run_dto_with(r: storage::Run, can_remember: bool) -> RunDto {
    let schedule_id = agent_core::schedule_id_of(&r.kind).map(|t| t.0);
    RunDto {
        id: r.id.0,
        bot_id: r.bot_id.0,
        chat_id: r.chat_id.map(|c| c.0),
        kind: r.kind,
        state: r.state,
        error: r.error,
        created_at_ms: r.created_at_ms,
        updated_at_ms: r.updated_at_ms,
        parent_run_id: r.parent_run_id.map(|p| p.0),
        can_remember,
        schedule_id,
        computer_target: r.computer_target,
        computer_node: r.computer_node,
        computer_node_name: r.computer_node_name,
        paused_at_ms: r.paused_at_ms,
        pause_reason: r.pause_reason,
    }
}

/// Approvals of a run, derived from its event log (the approval row's id is
/// not the tool call id, which only the `approval_needed` event carries).
/// An approval with no decision event in a run that is no longer waiting was
/// lost to a restart and reads `expired`.
fn run_approvals(s: &AppState, run: &storage::Run) -> Result<Vec<ApprovalDto>, ApiError> {
    let log =
        storage::read_event_log(s.storage().data_dir(), run.id).map_err(ApiError::internal)?;
    let waiting = run.state == RunState::WaitingApproval.as_str();
    let ms = |t: u64| i64::try_from(t).unwrap_or(i64::MAX);
    let mut out: Vec<ApprovalDto> = Vec::new();
    for env in log {
        match env.event {
            Event::ApprovalNeeded(n) => out.push(ApprovalDto {
                call_id: n.call_id.0,
                run_id: run.id.0,
                tool_name: n.tool_name,
                summary: n.summary,
                input: Some(n.input),
                details: n.details,
                decision: (!waiting).then(|| "expired".to_owned()),
                requested_at_ms: ms(env.ts_ms),
                decided_at_ms: None,
            }),
            Event::ToolCallApproved(d) => decide(&mut out, d.call_id, "approved", ms(env.ts_ms)),
            Event::ToolCallDenied(d) => decide(&mut out, d.call_id, "denied", ms(env.ts_ms)),
            _ => {}
        }
    }
    // A card is pending only while its wait is parked in the loop. One that is not (its tool timed
    // out, the wait was dropped, the daemon restarted) has expired, whatever the run's state says;
    // several cards of one run (parallel permission requests) can be pending at once.
    if waiting {
        for a in &mut out {
            if a.decision.is_none() && !s.agent().is_pending(ToolCallId(a.call_id)) {
                a.decision = Some("expired".to_owned());
            }
        }
    }
    Ok(out)
}

fn decide(list: &mut [ApprovalDto], call: ToolCallId, what: &str, at_ms: i64) {
    if let Some(a) = list.iter_mut().find(|a| a.call_id == call.0) {
        a.decision = Some(what.to_owned());
        a.decided_at_ms = Some(at_ms);
    }
}

fn engine_in_profile() -> ApiError {
    ApiError::new(
        StatusCode::FORBIDDEN,
        "engine_owner_only",
        "engine settings can only be changed by the local owner through the engine endpoint",
    )
}

/// Validate a bot definition; returns `(name, config json)`.
pub(crate) fn resolve_bot(input: BotInput) -> Result<(String, Value), ApiError> {
    let mut config = match (input.config, input.profile_toml) {
        (Some(c), None) => c,
        (None, Some(t)) => toml::from_str::<Value>(&t)
            .map_err(|e| ApiError::bad_request(format!("profile toml: {e}")))?,
        _ => {
            return Err(ApiError::bad_request(
                "exactly one of `config` or `profile_toml` is required",
            ));
        }
    };
    if let Some(rt) = input.runtime {
        let Some(obj) = config.as_object_mut() else {
            return Err(ApiError::bad_request("bot config must be a JSON object"));
        };
        if let Some(existing) = obj.get("runtime") {
            let declared: agent_core::RuntimeConfig = serde_json::from_value(existing.clone())
                .map_err(|e| ApiError::bad_request(format!("runtime: {e}")))?;
            let requested: agent_core::ComputerTarget =
                rt.computer.parse().map_err(|e: String| {
                    ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "bad_profile", e)
                })?;
            if declared.computer != requested {
                return Err(ApiError::bad_request(
                    "conflicting runtime computer targets",
                ));
            }
        }
        obj.insert(
            "runtime".to_owned(),
            serde_json::json!({ "computer": rt.computer }),
        );
    }
    let profile: BotProfile = serde_json::from_value(config.clone()).map_err(|e| {
        ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "bad_profile",
            format!("invalid bot profile: {e}"),
        )
    })?;
    let name = input.name.unwrap_or(profile.name);
    if name.trim().is_empty() {
        return Err(ApiError::bad_request("bot name must not be empty"));
    }
    Ok((name, config))
}

// ---------------------------------------------------------------- health

/// Liveness probe (no auth; still Host / Origin checked).
#[utoipa::path(get, path = "/v1/health", tag = "meta",
    responses((status = 200, body = Health)))]
pub(crate) async fn health(State(s): State<AppState>) -> Json<Health> {
    let (startup, startup_reason) = s.leader().startup();
    Json(Health {
        startup,
        components_ready: startup == crate::host::StartupPhase::Ready,
        channels_active: startup == crate::host::StartupPhase::Ready
            && s.leader().components_allowed()
            && !s.safe_mode(),
        startup_reason,
        safe_mode: s.safe_mode(),
        status: if s.agent().providers().setup_required() {
            "setup_required"
        } else {
            "ok"
        }
        .into(),
        version: env!("CARGO_PKG_VERSION").into(),
        api_version: crate::API_VERSION.into(),
        host: {
            let mut host = crate::dto::HostInfo::detect();
            let config = s.leader().config();
            host.mode = config.mode;
            host.state = config.state;
            host.epoch = config.epoch;
            host
        },
        uptime_secs: s.uptime_secs(),
    })
}

// ------------------------------------------------------------------ bots

/// List bots.
#[utoipa::path(get, path = "/v1/bots", tag = "bots", security(("bearer" = [])),
    responses((status = 200, body = Vec<BotDto>), (status = 401, body = ErrorBody)))]
pub(crate) async fn list_bots(State(s): State<AppState>) -> Result<Json<Vec<BotDto>>, ApiError> {
    Ok(Json(
        s.storage()
            .list_bots_versioned(s.user_id())?
            .into_iter()
            .map(|(bot, version)| bot_dto(bot, version))
            .collect(),
    ))
}

/// Create a bot from a JSON profile or TOML text.
#[utoipa::path(post, path = "/v1/bots", tag = "bots", security(("bearer" = [])),
    request_body = BotInput,
    responses((status = 201, body = BotDto), (status = 400, body = ErrorBody),
              (status = 422, body = ErrorBody)))]
pub(crate) async fn create_bot(
    State(s): State<AppState>,
    peer: Option<axum::Extension<crate::PeerIdentity>>,
    headers: axum::http::HeaderMap,
    ApiJson(input): ApiJson<BotInput>,
) -> Result<(StatusCode, Json<BotDto>), ApiError> {
    let (name, mut config) = resolve_bot(input)?;
    if config.get("engine").is_some() {
        return Err(engine_in_profile());
    }
    crate::peer::protect_knowledge_config(peer.as_ref().map(|peer| &peer.0), None, &mut config)?;
    // The owner's desktop with host administration acts as the owner here.
    let owner = s.bot_authority_peer(peer.as_ref().map(|peer| &peer.0));
    crate::peer::protect_execution_config(
        owner,
        s.leader().config().mode == crate::host::HostMode::Host,
        None,
        &mut config,
    )?;
    crate::file_access::guard_files_config(&s, owner, None, &mut config)?;
    // Permissions, tools, file access or a CLI engine in a new profile are
    // authority: the owner key, not the general token (card 262).
    if peer.is_none() && crate::owner::execution_changed(None, &config) {
        s.require_owner(&headers)?;
    }
    if config.get("knowledge").is_some() {
        return Err(ApiError::bad_request(
            "configure knowledge through the bot knowledge settings endpoint",
        ));
    }
    let bot = Bot {
        id: BotId::new(),
        user_id: s.user_id(),
        name,
        config,
        created_at_ms: now_ms(),
    };
    // New bots implicitly select all shared skills. Serialize that assignment
    // with owner deletion's in-use check, just like explicit assignment PUT.
    let dto = tokio::task::spawn_blocking(move || {
        let _skill_guard = s.0.skill_mutations.lock();
        s.storage().insert_bot(&bot)?;
        s.refresh_profile(&bot);
        Ok::<_, ApiError>(bot_dto(bot, 1))
    })
    .await
    .map_err(|_| ApiError::internal("bot worker failed"))??;
    Ok((StatusCode::CREATED, Json(dto)))
}

/// Get one bot, including its `effective_policy` (stored policy merged with
/// the persisted grants).
#[utoipa::path(get, path = "/v1/bots/{id}", tag = "bots", security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "Bot id")),
    responses((status = 200, body = BotDto), (status = 404, body = ErrorBody)))]
pub(crate) async fn get_bot(
    State(s): State<AppState>,
    Path(bid): Path<String>,
) -> Result<Json<BotDto>, ApiError> {
    let (bot, version) = s
        .storage()
        .get_bot_versioned(BotId(id(&bid)?))?
        .filter(|(bot, _)| bot.user_id == s.user_id())
        .ok_or_else(|| ApiError::not_found("bot"))?;
    let effective = s.agent().effective_policy(bot.id).ok();
    let mut dto = bot_dto(bot, version);
    dto.effective_policy = effective.map(effective_policy_dto);
    Ok(Json(dto))
}

/// The bot's persisted "always allow" grants (created by approving with scope
/// `always` / `always_category`).
#[utoipa::path(get, path = "/v1/bots/{id}/grants", tag = "bots", security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "Bot id")),
    responses((status = 200, body = Vec<GrantDto>), (status = 404, body = ErrorBody)))]
pub(crate) async fn list_grants(
    State(s): State<AppState>,
    Path(bid): Path<String>,
) -> Result<Json<Vec<GrantDto>>, ApiError> {
    let bot = s.bot_of_user(BotId(id(&bid)?))?;
    Ok(Json(
        s.agent()
            .list_grants(bot.id)
            .iter()
            .map(grant_dto)
            .collect(),
    ))
}

/// Revoke one grant (audited); the next matching call asks again.
#[utoipa::path(delete, path = "/v1/bots/{id}/grants", tag = "bots", security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "Bot id")),
    request_body = RevokeGrant,
    responses((status = 204), (status = 400, body = ErrorBody),
              (status = 404, body = ErrorBody, description = "no such bot or grant")))]
pub(crate) async fn revoke_grant(
    State(s): State<AppState>,
    Path(bid): Path<String>,
    ApiJson(input): ApiJson<RevokeGrant>,
) -> Result<StatusCode, ApiError> {
    let bot = s.bot_of_user(BotId(id(&bid)?))?;
    let target = grant_target(input.target)?;
    if s.agent().revoke_grant(bot.id, &target)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("grant"))
    }
}

/// Replace a bot profile using its loaded `expected_version` (required; missing is 428).
#[utoipa::path(put, path = "/v1/bots/{id}", tag = "bots", security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "Bot id")),
    request_body = BotInput,
    responses((status = 200, body = BotDto), (status = 404, body = ErrorBody),
              (status = 428, body = ErrorBody), (status = 409, body = BotConflictBody),
              (status = 422, body = ErrorBody)))]
pub(crate) async fn update_bot(
    State(s): State<AppState>,
    Path(bid): Path<String>,
    peer: Option<axum::Extension<crate::PeerIdentity>>,
    headers: axum::http::HeaderMap,
    ApiJson(input): ApiJson<BotInput>,
) -> Result<Json<BotDto>, BotPatchError> {
    let bot = s.bot_of_user(BotId(id(&bid)?))?;
    let expected = input.expected_version.ok_or_else(|| ApiError::new(
        StatusCode::PRECONDITION_REQUIRED, "precondition_required",
        "PUT requires expected_version from the profile being replaced; reload and retry explicitly",
    ))?;
    let _knowledge_guard = crate::knowledge_runtime::authority_lock(&s, bot.id)
        .lock_owned()
        .await;
    let bot = s.bot_of_user(bot.id)?;
    let preserve_runtime = input.runtime.is_none();
    let (name, mut config) = resolve_bot(input)?;
    crate::peer::protect_knowledge_config(
        peer.as_ref().map(|peer| &peer.0),
        bot.config.get("knowledge"),
        &mut config,
    )?;
    let owner = s.bot_authority_peer(peer.as_ref().map(|peer| &peer.0));
    crate::peer::protect_execution_config(
        owner,
        s.leader().config().mode == crate::host::HostMode::Host,
        Some(&bot.config),
        &mut config,
    )?;
    crate::file_access::guard_files_config(&s, owner, bot.config.get("files"), &mut config)?;
    if peer.is_none() && crate::owner::execution_changed(Some(&bot.config), &config) {
        s.require_owner(&headers)?;
    }
    // Engine settings (who runs files and shell) change only through
    // `PUT /v1/bots/{id}/engine`: an edit keeps the stored table, and a profile
    // that carries a different one is refused.
    match (config.get("engine"), bot.config.get("engine")) {
        (Some(sent), stored) if Some(sent) != stored => return Err(engine_in_profile().into()),
        _ => {}
    }
    {
        let obj = config
            .as_object_mut()
            .ok_or_else(|| ApiError::bad_request("bot config must be an object"))?;
        match bot.config.get("engine") {
            Some(engine) => {
                obj.insert("engine".into(), engine.clone());
            }
            None => {
                obj.remove("engine");
            }
        }
    }
    let after: BotProfile = serde_json::from_value(config.clone()).map_err(|e| {
        ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "bad_profile",
            e.to_string(),
        )
    })?;
    if after.engine.mode == cli_provider::EngineMode::NativeTools
        && !after.models.executor.starts_with("cli:")
    {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "bad_engine",
            "this bot runs a CLI with its own tools; switch it back to Praxi-governed before choosing a non-CLI model",
        )
        .into());
    }
    if config.get("knowledge").is_some()
        && bot.config.pointer("/knowledge/embedding") != config.pointer("/knowledge/embedding")
    {
        // Even a stale profile cannot revive authority after an owner submits
        // a changed embedding profile. Dedicated settings own the saved value.
        crate::knowledge_runtime::revoke_bot_grants(&s, bot.id)?;
    }
    // Generic profile saves may contain stale snapshots. Only the dedicated
    // knowledge endpoint may update retrieval configuration or query grants.
    if let Some(current) = bot.config.get("knowledge") {
        config["knowledge"] = current.clone();
    } else if let Some(object) = config.as_object_mut() {
        object.remove("knowledge");
    }
    let outcome = s.storage().patch_bot(bot.id, expected, |_, stored| {
        if preserve_runtime
            && config.get("runtime").is_none()
            && let Some(runtime) = stored.get("runtime")
        {
            config
                .as_object_mut()
                .ok_or_else(|| "bad_profile:bot config must be an object".to_owned())?
                .insert("runtime".into(), runtime.clone());
        }
        Ok((name, config))
    })?;
    profile_write_response(&s, outcome)
}

/// Why a patch cannot be applied.
pub(crate) enum BotPatchError {
    Api(ApiError),
    /// The stored version is not `expected_version`.
    Stale(Box<BotDto>),
}

impl From<ApiError> for BotPatchError {
    fn from(e: ApiError) -> Self {
        Self::Api(e)
    }
}

impl From<storage::StorageError> for BotPatchError {
    fn from(e: storage::StorageError) -> Self {
        Self::Api(e.into())
    }
}

impl axum::response::IntoResponse for BotPatchError {
    fn into_response(self) -> axum::response::Response {
        match self {
            Self::Api(e) => e.into_response(),
            Self::Stale(current) => (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": {
                        "code": "version_conflict",
                        "message": "the bot changed since `expected_version`",
                    },
                    "current": *current,
                })),
            )
                .into_response(),
        }
    }
}

const MAX_PROFILE_DEPTH: usize = 32;
const MAX_PROFILE_BYTES: usize = 1024 * 1024;
const MAX_PATCH_OPS: usize = 128;

/// Iterative bound on the resulting JSON, including keys and escaped strings.
/// Checked before cloning/applying values and after each operation.
fn check_profile_bounds(value: &Value, depth: usize) -> Result<(), String> {
    let mut pending = vec![(value, depth)];
    let mut bytes = 0usize;
    while let Some((node, depth)) = pending.pop() {
        if depth > MAX_PROFILE_DEPTH {
            return Err("profile exceeds maximum depth (32)".into());
        }
        bytes = bytes.saturating_add(16);
        match node {
            Value::String(text) => bytes = bytes.saturating_add(text.len().saturating_mul(6)),
            Value::Object(map) => {
                for (key, child) in map {
                    bytes = bytes.saturating_add(key.len().saturating_mul(6));
                    pending.push((child, depth + 1));
                }
            }
            Value::Array(array) => pending.extend(array.iter().map(|child| (child, depth + 1))),
            _ => {}
        }
        if bytes > MAX_PROFILE_BYTES || pending.len() > MAX_PROFILE_BYTES / 16 {
            return Err("profile exceeds maximum size (1 MiB escaped JSON budget)".into());
        }
    }
    Ok(())
}

/// Split a JSON Pointer into object keys (RFC 6901 escapes).
fn pointer_keys(path: &str) -> Result<Vec<String>, String> {
    if path.len() > 4096 || path.bytes().filter(|b| *b == b'/').count() > MAX_PROFILE_DEPTH {
        return Err("patch path exceeds maximum length (4096) or depth (32)".into());
    }
    let rest = path
        .strip_prefix('/')
        .ok_or_else(|| format!("path `{path}` must start with `/`"))?;
    let keys: Vec<String> = rest
        .split('/')
        .map(|k| k.replace("~1", "/").replace("~0", "~"))
        .collect();
    if keys.iter().any(String::is_empty) {
        return Err(format!("path `{path}` has an empty segment"));
    }
    Ok(keys)
}

/// Set (`value` is `Some`) or remove (`None`) the key path `keys` below `node`.
fn patch_at(
    node: &mut Value,
    keys: &[String],
    value: Option<&Value>,
    path: &str,
) -> Result<(), String> {
    let Some((first, rest)) = keys.split_first() else {
        return Ok(());
    };
    let obj = node
        .as_object_mut()
        .ok_or_else(|| format!("path `{path}` crosses a non-object"))?;
    if rest.is_empty() {
        match value {
            Some(v) => {
                obj.insert(first.clone(), v.clone());
            }
            None => {
                obj.remove(first);
            }
        }
        return Ok(());
    }
    match (obj.get_mut(first), value) {
        (Some(child), _) => patch_at(child, rest, value, path),
        (None, Some(_)) => {
            let mut child = Value::Object(serde_json::Map::new());
            patch_at(&mut child, rest, value, path)?;
            obj.insert(first.clone(), child);
            Ok(())
        }
        (None, None) => Ok(()),
    }
}

/// Apply field-level ops to a profile.  Only the named keys change.
fn apply_patch_ops(config: &mut Value, ops: &[BotPatchOp]) -> Result<(), String> {
    check_profile_bounds(config, 0)?;
    for op in ops {
        let keys = pointer_keys(&op.path)?;
        let value = (op.op == BotPatchKind::Set).then_some(&op.value);
        patch_at(config, &keys, value, &op.path)?;
        check_profile_bounds(config, 0)?;
    }
    Ok(())
}

/// Change only the listed fields of a bot, atomically, if `expected_version` is current.
#[utoipa::path(patch, path = "/v1/bots/{id}", tag = "bots", security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "Bot id")),
    request_body = BotPatchInput,
    responses((status = 200, body = BotDto), (status = 404, body = ErrorBody),
              (status = 409, body = BotConflictBody,
               description = "the bot changed since `expected_version`; `current` is its state now"),
              (status = 400, body = ErrorBody, description = "patch exceeds depth, size or operation limits"),
              (status = 422, body = ErrorBody)))]
pub(crate) async fn patch_bot(
    State(s): State<AppState>,
    Path(bid): Path<String>,
    peer: Option<axum::Extension<crate::PeerIdentity>>,
    headers: axum::http::HeaderMap,
    ApiJson(input): ApiJson<BotPatchInput>,
) -> Result<Json<BotDto>, BotPatchError> {
    let bot = s.bot_of_user(BotId(id(&bid)?))?;
    let _knowledge_guard = crate::knowledge_runtime::authority_lock(&s, bot.id)
        .lock_owned()
        .await;
    let bot = s.bot_of_user(bot.id)?;
    let new_name = input.name.map(|n| n.trim().to_owned());
    if new_name.as_deref() == Some("") {
        return Err(ApiError::bad_request("bot name must not be empty").into());
    }
    let ops = input.ops;
    if ops.len() > MAX_PATCH_OPS {
        return Err(ApiError::bad_request("patch exceeds maximum operation count (128)").into());
    }
    for op in &ops {
        if op.path.len() > 4096
            || op.path.bytes().filter(|b| *b == b'/').count() > MAX_PROFILE_DEPTH
        {
            return Err(ApiError::bad_request(
                "patch path exceeds maximum length (4096) or depth (32)",
            )
            .into());
        }
        let depth = op.path.bytes().filter(|b| *b == b'/').count();
        check_profile_bounds(&op.value, depth).map_err(ApiError::bad_request)?;
    }
    {
        // Dry run on the profile read under the authority lock. Generic profile
        // edits never change knowledge settings (the dedicated endpoint owns them).
        let mut next = bot.config.clone();
        if apply_patch_ops(&mut next, &ops).is_ok() {
            if peer.is_none() && crate::owner::execution_changed(Some(&bot.config), &next) {
                s.require_owner(&headers)?;
            }
            crate::peer::protect_knowledge_config(
                peer.as_ref().map(|peer| &peer.0),
                bot.config.get("knowledge"),
                &mut next,
            )?;
            if next.get("knowledge") != bot.config.get("knowledge") {
                return Err(ApiError::bad_request(
                    "configure knowledge through the bot knowledge settings endpoint",
                )
                .into());
            }
        }
    }
    let host_mode = s.leader().config().mode == crate::host::HostMode::Host;
    let owner_key_given = s.has_owner(&headers);
    let outcome = s
        .storage()
        .patch_bot(bot.id, input.expected_version, |name, config| {
            let mut next = config.clone();
            apply_patch_ops(&mut next, &ops).map_err(|e| format!("bad_patch:{e}"))?;
            // The profile may have changed since the dry run above.
            if peer.is_none()
                && !owner_key_given
                && crate::owner::execution_changed(Some(config), &next)
            {
                return Err(
                    "forbidden:this edit changes what the bot may do and needs the owner key"
                        .to_owned(),
                );
            }
            // The FINAL candidate is checked against the stored profile inside
            // the atomic write, with the same guards as create and PUT.
            let refused = |e: ApiError| {
                if e.status() == StatusCode::FORBIDDEN {
                    format!("forbidden:{e}")
                } else {
                    format!("bad_patch:{e}")
                }
            };
            let peer_id = peer.as_ref().map(|p| &p.0);
            crate::peer::protect_knowledge_config(peer_id, config.get("knowledge"), &mut next)
                .map_err(refused)?;
            crate::peer::protect_execution_config(
                s.bot_authority_peer(peer_id),
                host_mode,
                Some(config),
                &mut next,
            )
            .map_err(refused)?;
            crate::file_access::guard_files_config(
                &s,
                s.bot_authority_peer(peer_id),
                config.get("files"),
                &mut next,
            )
            .map_err(refused)?;
            // Engine settings (who runs files and shell, and the consent
            // behind them) change only through `PUT /v1/bots/{id}/engine`.
            if next.get("engine") != config.get("engine") {
                return Err(
                    "bad_patch:the engine settings change only through the engine endpoint"
                        .to_owned(),
                );
            }
            serde_json::from_value::<BotProfile>(next.clone())
                .map_err(|e| format!("bad_profile:invalid bot profile: {e}"))?;
            Ok((new_name.unwrap_or_else(|| name.to_owned()), next))
        })?;
    profile_write_response(&s, outcome)
}

fn profile_write_response(
    s: &AppState,
    outcome: storage::BotPatch,
) -> Result<Json<BotDto>, BotPatchError> {
    match outcome {
        storage::BotPatch::Applied { bot, version } => {
            crate::knowledge_runtime::invalidate_bot(s, bot.id);
            s.refresh_profile(&bot);
            Ok(Json(bot_dto(bot, version)))
        }
        storage::BotPatch::Stale { bot, version } => {
            Err(BotPatchError::Stale(Box::new(bot_dto(bot, version))))
        }
        storage::BotPatch::Missing => Err(ApiError::not_found("bot").into()),
        storage::BotPatch::Rejected(why) => {
            let (code, msg) = why.split_once(':').unwrap_or(("bad_patch", &why));
            if code == "forbidden" {
                return Err(ApiError::forbidden(msg.to_owned()).into());
            }
            let code = if code == "bad_profile" {
                "bad_profile"
            } else {
                "bad_patch"
            };
            Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, code, msg.to_owned()).into())
        }
    }
}

/// Drop what other stores keep about a deleted bot: its id in MCP servers'
/// `bots` lists and its bound e-mail accounts.
pub(crate) async fn forget_bot_links(s: &AppState, bot: &Bot) {
    // Its scheduled jobs go too: a job of a deleted bot could only fail on every
    // due time. Ids are never reused, so a failure here cannot give anyone access.
    if let Ok(store) = s.schedule_store()
        && let Err(e) = store.delete_by_bot(bot.id)
    {
        eprintln!("daemon-api: warning: could not remove schedules of deleted bot: {e:?}");
    }
    // The bot's id must not linger in any MCP server's `bots` list.  Ids are
    // never reused, so a failure here cannot widen access; it is retried by
    // the next edit of the list.
    if let Some(mcp) = s.mcp() {
        let _ = mcp.forget_bot(&bot.id.to_string()).await;
    }
    // Accounts bound to the bot are removed, never unbound: an unbound account
    // is usable by every bot, so unbinding would silently widen access.  Ids
    // are never reused, so an account that survives a failure here stays inert
    // (bound to a bot that no longer exists).
    if let Some(email) = s.email()
        && let Err(e) = email.forget_bot(&bot.id.to_string()).await
    {
        eprintln!("daemon-api: warning: could not remove e-mail accounts of deleted bot: {e:?}");
    }
}

/// Delete a bot that has no chats or runs, together with everything that hangs
/// off it (knowledge index and grants, browser slot, MCP and e-mail links).
/// Shared by the HTTP route and the bot-management tool.
pub(crate) async fn remove_bot(s: &AppState, bot: &Bot) -> Result<(), ApiError> {
    let permit = crate::knowledge::worker_permit(s.data_dir())?;
    let authority = crate::knowledge::authority_lock(s, bot.id)
        .lock_owned()
        .await;
    crate::knowledge_runtime::quiesce_bot(s, bot.id).await?;
    let state = s.clone();
    let removed = bot.clone();
    crate::knowledge::blocking_with_permit(permit, move || {
        let _authority = authority;
        state
            .storage()
            .delete_bot_with_cleanup(removed.id, || {
                crate::knowledge_runtime::revoke_bot_grants(&state, removed.id).map_err(|_| {
                    storage::StorageError::Io(std::io::Error::other(
                        "knowledge authority could not be revoked",
                    ))
                })?;
                crate::knowledge::remove_bot_index(&state, &removed)
                    .map_err(storage::StorageError::Io)
            })
            .map_err(|_| {
                ApiError::conflict(
                    "bot still has chats or runs, or its knowledge files could not be removed",
                )
            })?;
        Ok(())
    })
    .await?;
    // The bot's browser slot, driver and screen target go with it; nothing
    // selectable or resident stays behind for a bot that no longer exists.
    s.live_view().registry.retire_bot(bot.id.0);
    forget_bot_links(s, bot).await;
    Ok(())
}

/// Delete a bot that has no chats or runs.
#[utoipa::path(delete, path = "/v1/bots/{id}", tag = "bots", security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "Bot id")),
    responses((status = 204), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)))]
pub(crate) async fn delete_bot(
    State(s): State<AppState>,
    Path(bid): Path<String>,
) -> Result<StatusCode, ApiError> {
    let bot = s.bot_of_user(BotId(id(&bid)?))?;
    remove_bot(&s, &bot).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ----------------------------------------------------------------- chats

/// List chats.
#[utoipa::path(get, path = "/v1/chats", tag = "chats", security(("bearer" = [])),
    responses((status = 200, body = Vec<ChatDto>)))]
pub(crate) async fn list_chats(State(s): State<AppState>) -> Result<Json<Vec<ChatDto>>, ApiError> {
    let flags = s.storage().list_chat_flags(s.user_id())?;
    Ok(Json(
        s.storage()
            .list_chats(s.user_id())?
            .into_iter()
            .map(|c| {
                let f = flags.get(&c.id).copied().unwrap_or_default();
                chat_dto(c, f)
            })
            .collect(),
    ))
}

/// Create a chat with a bot.
#[utoipa::path(post, path = "/v1/chats", tag = "chats", security(("bearer" = [])),
    request_body = CreateChat,
    responses((status = 201, body = ChatDto), (status = 404, body = ErrorBody)))]
pub(crate) async fn create_chat(
    State(s): State<AppState>,
    ApiJson(input): ApiJson<CreateChat>,
) -> Result<(StatusCode, Json<ChatDto>), ApiError> {
    let bot = s.bot_of_user(BotId(input.bot_id))?;
    let chat = Chat {
        id: ChatId::new(),
        user_id: s.user_id(),
        bot_id: bot.id,
        title: input.title.unwrap_or_else(|| "New chat".to_owned()),
        created_at_ms: now_ms(),
    };
    s.storage().insert_chat(&chat)?;
    Ok((
        StatusCode::CREATED,
        Json(chat_dto(chat, storage::ChatFlags::default())),
    ))
}

/// Rename, pin or archive a chat.
#[utoipa::path(patch, path = "/v1/chats/{id}", tag = "chats", security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "Chat id")),
    request_body = UpdateChat,
    responses((status = 200, body = ChatDto), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub(crate) async fn update_chat(
    State(s): State<AppState>,
    Path(cid): Path<String>,
    ApiJson(input): ApiJson<UpdateChat>,
) -> Result<Json<ChatDto>, ApiError> {
    let chat = s.chat_of_user(ChatId(id(&cid)?))?;
    let title = match input.title.as_deref().map(str::trim) {
        Some(t) if t.is_empty() || t.chars().count() > 120 => {
            return Err(ApiError::bad_request("title must be 1 to 120 characters"));
        }
        other => other,
    };
    s.storage()
        .update_chat(chat.id, title, input.pinned, input.archived)?;
    let chat = s.chat_of_user(chat.id)?;
    let flags = s.storage().chat_flags(chat.id)?;
    Ok(Json(chat_dto(chat, flags)))
}

/// Delete a chat and its messages (its runs stay for the audit trail). Refused while a run of
/// the chat is active.
#[utoipa::path(delete, path = "/v1/chats/{id}", tag = "chats", security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "Chat id")),
    responses((status = 204), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)))]
pub(crate) async fn delete_chat(
    State(s): State<AppState>,
    Path(cid): Path<String>,
) -> Result<StatusCode, ApiError> {
    let chat = s.chat_of_user(ChatId(id(&cid)?))?;
    s.storage().delete_chat(chat.id).map_err(|e| match e {
        storage::StorageError::Conflict(_) => {
            ApiError::conflict("the chat has a run in progress: stop it first")
        }
        other => other.into(),
    })?;
    agent_core::attachments::remove_chat(&s.data_dir(), chat.id);
    Ok(StatusCode::NO_CONTENT)
}

/// List a chat's messages, oldest first.
#[utoipa::path(get, path = "/v1/chats/{id}/messages", tag = "chats", security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "Chat id")),
    responses((status = 200, body = Vec<MessageDto>), (status = 404, body = ErrorBody)))]
pub(crate) async fn list_messages(
    State(s): State<AppState>,
    Path(cid): Path<String>,
) -> Result<Json<Vec<MessageDto>>, ApiError> {
    let chat = s.chat_of_user(ChatId(id(&cid)?))?;
    let mut run_bots: std::collections::HashMap<RunId, Option<Uuid>> =
        std::collections::HashMap::new();
    let mut out = Vec::new();
    let superseded = s.storage().superseded_message_ids(chat.id)?;
    let all = s.storage().list_messages_with_superseded(chat.id)?;
    // Only the last turn (from the last live user message on) can be redone, so only
    // its runs are checked for tools that acted.
    let turn_start = all
        .iter()
        .rposition(|m| m.role == "user" && !superseded.contains(&m.id))
        .unwrap_or(all.len());
    let mut acted: std::collections::HashMap<RunId, bool> = std::collections::HashMap::new();
    for (idx, m) in all.into_iter().enumerate() {
        let used_write_tools = idx >= turn_start
            && !superseded.contains(&m.id)
            && match m.run_id {
                Some(run) => *acted
                    .entry(run)
                    .or_insert_with(|| s.agent().run_used_non_read_tools(run).unwrap_or(true)),
                None => false,
            };
        let bot_id = match m.run_id {
            Some(run) => *run_bots
                .entry(run)
                .or_insert_with(|| s.storage().get_run(run).ok().flatten().map(|r| r.bot_id.0)),
            None => None,
        };
        let source = (m.role == "user")
            .then(|| s.storage().message_source(m.id).ok())
            .flatten()
            .filter(|v| !v.is_empty());
        let (content, attachments) = if m.role == "user" {
            let (text, ids) = agent_core::attachments::split_marker(&m.content);
            let dtos = ids
                .into_iter()
                .filter_map(|a| {
                    agent_core::attachments::meta(s.storage().data_dir(), chat.id, a).ok()
                })
                .map(|a| crate::dto::AttachmentDto::from(&a))
                .collect::<Vec<_>>();
            (text.to_owned(), dtos)
        } else {
            (m.content, Vec::new())
        };
        out.push(MessageDto {
            id: m.id,
            chat_id: m.chat_id.0,
            run_id: m.run_id.map(|r| r.0),
            bot_id,
            role: m.role,
            created_at_ms: m.created_at_ms,
            source,
            content,
            attachments,
            superseded: superseded.contains(&m.id),
            used_write_tools,
        });
    }
    Ok(Json(out))
}

/// Regenerate the last answer, or edit the last user message and resend it.
///
/// Owner only. The chat must be idle, the message must still be the chat's last
/// user message, written in this app, and every run of that turn finished. The old
/// messages are marked superseded (kept, not in the model context); a new run
/// starts. Tools that already acted in the old run are not undone.
#[utoipa::path(post, path = "/v1/chats/{id}/resend", tag = "chats", security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "Chat id")),
    request_body = ResendMessage,
    responses((status = 202, body = ResendResponse), (status = 403, body = ErrorBody),
              (status = 404, body = ErrorBody),
              (status = 409, description = "Chat busy or message no longer last", body = ErrorBody),
              (status = 422, description = "Turn cannot be edited", body = ErrorBody)))]
pub(crate) async fn resend_last_turn(
    State(s): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(cid): Path<String>,
    ApiJson(input): ApiJson<ResendMessage>,
) -> Result<(StatusCode, Json<ResendResponse>), ApiError> {
    // Also guards the peer path, which skips the router-level owner gate.
    s.require_owner(&headers)?;
    let chat = s.chat_of_user(ChatId(id(&cid)?))?;
    if input
        .text
        .as_deref()
        .is_some_and(|t| t.trim().is_empty() && !has_attachments(&s, chat.id, input.message_id))
    {
        return Err(ApiError::bad_request("text must not be empty"));
    }
    let run = s
        .agent()
        .resend_last_turn(chat.id, input.message_id, input.text.as_deref())?;
    Ok((StatusCode::ACCEPTED, Json(ResendResponse { run_id: run.0 })))
}

fn has_attachments(s: &AppState, chat: ChatId, message: Uuid) -> bool {
    s.storage()
        .get_message(message)
        .ok()
        .flatten()
        .is_some_and(|m| {
            m.chat_id == chat
                && !agent_core::attachments::split_marker(&m.content)
                    .1
                    .is_empty()
        })
}

/// Send a user message.  Starts a turn when the chat is idle (returns its run
/// id); otherwise `mode` decides between steering and queueing.
#[utoipa::path(post, path = "/v1/chats/{id}/messages", tag = "chats", security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "Chat id")),
    request_body = SendMessage,
    responses((status = 202, body = SendResponse), (status = 404, body = ErrorBody),
              (status = 409, description = "Setup required or chat busy", body = ErrorBody),
              (status = 422, body = ErrorBody)))]
pub(crate) async fn send_message(
    State(s): State<AppState>,
    headers: axum::http::HeaderMap,
    axum::Extension(actor): axum::Extension<crate::live_view::Actor>,
    Path(cid): Path<String>,
    ApiJson(input): ApiJson<SendMessage>,
) -> Result<(StatusCode, Json<SendResponse>), ApiError> {
    let chat = s.chat_of_user(ChatId(id(&cid)?))?;
    if input.text.trim().is_empty() && input.attachment_ids.is_empty() {
        return Err(ApiError::bad_request("text must not be empty"));
    }
    if input.attachment_ids.len() > agent_core::attachments::MAX_PER_MESSAGE {
        return Err(ApiError::bad_request(format!(
            "at most {} attachments per message",
            agent_core::attachments::MAX_PER_MESSAGE
        )));
    }
    for att in &input.attachment_ids {
        agent_core::attachments::meta(s.storage().data_dir(), chat.id, *att)?;
    }
    let text = agent_core::attachments::with_marker(&input.text, &input.attachment_ids);
    let mode = match input.mode {
        Mode::Steer => MessageMode::Steer,
        Mode::Queue => MessageMode::Queue,
    };
    // Source hints can only reduce authority; only the app's owner-key request is local UI.
    let hint = headers
        .get("x-praxi-message-source")
        .and_then(|v| v.to_str().ok());
    let source = if !actor.local {
        // The owner's admin desktop of this host is told apart from any other
        // paired device: only its messages may start work on the user's own
        // computer (`pc_*`).
        if actor.admin { "peer_admin" } else { "peer" }
    } else if hint == Some("telegram") {
        "telegram"
    } else if hint == Some("local_ui") && s.owner_ok(&headers) {
        "owner"
    } else {
        "local_api"
    };
    let (delivery, run_id) = match s
        .agent()
        .send_user_message_from(chat.id, &text, mode, source)?
    {
        Delivery::NewTurn(r) => (DeliveryKind::NewTurn, Some(r.0)),
        Delivery::Steered(r) => (DeliveryKind::Steered, Some(r.0)),
        Delivery::Queued => (DeliveryKind::Queued, None),
        Delivery::Stopped(r) => (DeliveryKind::Stopped, Some(r.0)),
    };
    Ok((
        StatusCode::ACCEPTED,
        Json(SendResponse { delivery, run_id }),
    ))
}

// ------------------------------------------------------------- approvals

/// Query of `GET /v1/approvals`.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ApprovalsQuery {
    /// Only this run's approvals (pending and decided).  Without it: pending
    /// approvals of all runs.
    pub run_id: Option<Uuid>,
}

/// List approvals.
#[utoipa::path(get, path = "/v1/approvals", tag = "approvals", security(("bearer" = [])),
    params(ApprovalsQuery),
    responses((status = 200, body = Vec<ApprovalDto>)))]
pub(crate) async fn list_approvals(
    State(s): State<AppState>,
    Query(q): Query<ApprovalsQuery>,
) -> Result<Json<Vec<ApprovalDto>>, ApiError> {
    let out = if let Some(run) = q.run_id {
        run_approvals(&s, &s.run_of_user(RunId(run))?)?
    } else {
        let mut v = Vec::new();
        for run in s
            .storage()
            .list_runs_by_state(RunState::WaitingApproval.as_str())?
        {
            if run.user_id == s.user_id() {
                v.extend(
                    run_approvals(&s, &run)?
                        .into_iter()
                        .filter(|a| a.decision.is_none()),
                );
            }
        }
        v
    };
    Ok(Json(out))
}

/// Approve or deny a pending tool call.
#[utoipa::path(post, path = "/v1/approvals/{call_id}", tag = "approvals",
    security(("bearer" = [])),
    params(("call_id" = Uuid, Path, description = "Tool call id from the approval_needed event")),
    request_body = DecideApproval,
    responses((status = 200, body = DecisionResponse), (status = 404, body = ErrorBody)))]
pub(crate) async fn decide_approval(
    State(s): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(call): Path<String>,
    ApiJson(input): ApiJson<DecideApproval>,
) -> Result<Json<DecisionResponse>, ApiError> {
    let call_uuid = id(&call)?;
    let call_id = ToolCallId(call_uuid);
    // "Always allow for this chat" on a taint-gate card is a standing grant: it takes the
    // owner's authority (`x-praxi-owner-key`), not just the API token or a paired peer.
    if matches!(input.decision, Decision::Approve)
        && !matches!(
            input.scope,
            Scope::Once | Scope::RunControl | Scope::RunTool
        )
        && s.agent().approval_is_gate_grant(call_id)
    {
        s.require_owner(&headers)?;
    }
    match input.decision {
        Decision::Approve => {
            let scope = match input.scope {
                Scope::Once => ApprovalScope::Once,
                Scope::ThisChat => ApprovalScope::ThisChat,
                Scope::Always => ApprovalScope::Always,
                Scope::AlwaysCategory => ApprovalScope::AlwaysCategory,
                Scope::RunControl => ApprovalScope::RunControl,
                Scope::RunTool => ApprovalScope::RunTool,
            };
            s.agent().approve(call_id, scope, s.user_id())?;
        }
        Decision::Deny => s.agent().deny(call_id, s.user_id(), input.reason)?,
    }
    Ok(Json(DecisionResponse {
        call_id: call_uuid,
        decision: input.decision,
    }))
}

/// Answer a pending question card (the `call_id` of a `question_asked` event).
#[utoipa::path(post, path = "/v1/questions/{call_id}", tag = "approvals",
    security(("bearer" = [])),
    params(("call_id" = Uuid, Path, description = "Tool call id from the question_asked event")),
    request_body = AnswerQuestion,
    responses((status = 200, body = AnswerResponse), (status = 404, body = ErrorBody)))]
pub(crate) async fn answer_question(
    State(s): State<AppState>,
    Path(call): Path<String>,
    ApiJson(input): ApiJson<AnswerQuestion>,
) -> Result<Json<AnswerResponse>, ApiError> {
    let call_uuid = id(&call)?;
    s.agent()
        .answer_question(ToolCallId(call_uuid), &input.answer)?;
    Ok(Json(AnswerResponse { call_id: call_uuid }))
}

// ------------------------------------------------------------------ runs

/// List runs, newest last.
#[utoipa::path(get, path = "/v1/runs", tag = "runs", security(("bearer" = [])),
    responses((status = 200, body = Vec<RunDto>)))]
pub(crate) async fn list_runs(State(s): State<AppState>) -> Result<Json<Vec<RunDto>>, ApiError> {
    // `can_remember` may open the per-bot router cache (a sqlite file), so a
    // read-only listing does not ask: it reports `false`; `GET /v1/runs/{id}`
    // has the real answer.
    Ok(Json(
        s.storage()
            .list_runs(s.user_id())?
            .into_iter()
            .map(|r| run_dto_with(r, false))
            .collect(),
    ))
}

/// Get a run.
#[utoipa::path(get, path = "/v1/runs/{id}", tag = "runs", security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "Run id")),
    responses((status = 200, body = RunDto), (status = 404, body = ErrorBody)))]
pub(crate) async fn get_run(
    State(s): State<AppState>,
    Path(rid): Path<String>,
) -> Result<Json<RunDto>, ApiError> {
    Ok(Json(run_dto(&s, s.run_of_user(RunId(id(&rid)?))?)))
}

/// Cancel a run.
#[utoipa::path(post, path = "/v1/runs/{id}/cancel", tag = "runs", security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "Run id")),
    responses((status = 200, body = RunDto), (status = 404, body = ErrorBody),
              (status = 409, body = ErrorBody)))]
pub(crate) async fn cancel_run(
    State(s): State<AppState>,
    Path(rid): Path<String>,
) -> Result<Json<RunDto>, ApiError> {
    let run = s.run_of_user(RunId(id(&rid)?))?;
    if s.hub().run_parked_on_latch(run.id) {
        s.agent().force_cancel(run.id)?;
    } else {
        s.agent().cancel(run.id)?;
    }
    Ok(Json(run_dto(&s, s.run_of_user(run.id)?)))
}

/// Remember the route a succeeded run took (or the workflow its model ran):
/// the same request text is then routed there directly next time (intent
/// cache, K1).
#[utoipa::path(post, path = "/v1/runs/{id}/remember", tag = "runs", security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "Run id")),
    responses((status = 200, body = RememberResponse), (status = 404, body = ErrorBody),
              (status = 409, body = ErrorBody, description = "the run is not eligible")))]
pub(crate) async fn remember_run(
    State(s): State<AppState>,
    Path(rid): Path<String>,
) -> Result<Json<RememberResponse>, ApiError> {
    let run = s.run_of_user(RunId(id(&rid)?))?;
    let route = s.agent().remember_route(run.id)?;
    Ok(Json(RememberResponse { route }))
}

/// Resume a paused run (e.g. after a daemon restart).
#[utoipa::path(post, path = "/v1/runs/{id}/resume", tag = "runs", security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "Run id")),
    responses((status = 202, body = RunDto), (status = 404, body = ErrorBody),
              (status = 409, body = ErrorBody)))]
pub(crate) async fn resume_run(
    State(s): State<AppState>,
    Path(rid): Path<String>,
) -> Result<(StatusCode, Json<RunDto>), ApiError> {
    let run = s.run_of_user(RunId(id(&rid)?))?;
    s.agent().resume(run.id)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(run_dto(&s, s.run_of_user(run.id)?)),
    ))
}

// ----------------------------------------------------------------- usage

/// Model and token usage of one run (read-only; feeds the chat's message info).
#[utoipa::path(get, path = "/v1/runs/{id}/usage", tag = "runs", security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "Run id")),
    responses((status = 200, body = RunUsageDto), (status = 404, body = ErrorBody)))]
pub(crate) async fn run_usage(
    State(s): State<AppState>,
    Path(rid): Path<String>,
) -> Result<Json<RunUsageDto>, ApiError> {
    let run = s.run_of_user(RunId(id(&rid)?))?;
    let st = s.storage();
    let mut out = RunUsageDto {
        model: None,
        prompt_tokens: 0,
        completion_tokens: 0,
        model_calls: 0,
    };
    let audit = st.list_audit_for_run(s.user_id(), run.id)?;
    let calls = agent_core::usage::calls_from_audit(&audit);
    let mut recorded_steps = std::collections::HashSet::new();
    for call in calls.get(&run.id).into_iter().flatten() {
        if !matches!(
            call.purpose,
            agent_core::CallPurpose::Executor | agent_core::CallPurpose::GuardClose
        ) {
            continue;
        }
        recorded_steps.insert(call.step_n);
        out.model = Some(call.model.clone());
        out.model_calls += 1;
        if let Some(u) = call.tokens {
            out.prompt_tokens = out.prompt_tokens.saturating_add(u.prompt_tokens);
            out.completion_tokens = out.completion_tokens.saturating_add(u.completion_tokens);
        }
    }
    // Pre-ledger runs: the model step carries the model and usage.
    for step in st.list_steps(run.id)? {
        if step.name != "model" || recorded_steps.contains(&step.idx) {
            continue;
        }
        let Some(output) = step.output.as_ref() else {
            continue;
        };
        if let Some(m) = output.get("model").and_then(Value::as_str) {
            out.model = Some(m.to_owned());
        }
        out.model_calls += 1;
        if let Some(u) = output
            .get("usage")
            .and_then(|u| serde_json::from_value::<Usage>(u.clone()).ok())
        {
            out.prompt_tokens = out.prompt_tokens.saturating_add(u.prompt_tokens);
            out.completion_tokens = out.completion_tokens.saturating_add(u.completion_tokens);
        }
    }
    Ok(Json(out))
}

// --------------------------------------------------------------- control

/// `viewer`: only the takeover owner sees the takeover and epoch ids; everyone
/// else gets who/what/state, never credentials for the input and release routes.
pub(crate) fn control_state_dto(
    s: &AppState,
    viewer: Option<&crate::live_view::Actor>,
) -> ControlStateDto {
    let reg = s.hub().registration();
    ControlStateDto {
        mode: reg.map(|(m, _)| m),
        expires_in_ms: reg.map_or(0, |(_, left)| {
            u64::try_from(left.as_millis()).unwrap_or(u64::MAX)
        }),
        app_connected: s.hub().app_connected(),
        latched: s.hub().latched(),
        takeover: s.hub().takeover().map(|t| {
            let mut info = t.info;
            if viewer.is_none_or(|v| v.key != t.actor) {
                info.owner = crate::control::display_label(&t.actor);
                info.id = uuid::Uuid::nil();
                info.session_id = uuid::Uuid::nil();
                info.control_epoch = 0;
                info.input_expires_in_ms = 0;
            }
            info
        }),
        release: None,
    }
}

/// Explicitly release a latched `paused` / `human_driving`: bots may use the
/// desktop again.  This is the only way (besides a daemon restart) to end a
/// pause; heartbeats and a lost app never do.  Idempotent.
#[utoipa::path(post, path = "/v1/control/release", tag = "control", security(("bearer" = [])),
    request_body = Option<live_view::ReleaseRequest>,
    responses((status = 200, body = ControlStateDto), (status = 403, body = ErrorBody), (status = 409, body = ErrorBody)))]
pub(crate) async fn release_control(
    State(s): State<AppState>,
    axum::Extension(actor): axum::Extension<crate::live_view::Actor>,
    body: axum::body::Bytes,
) -> Result<Json<ControlStateDto>, ApiError> {
    let mut dto;
    if body.is_empty() {
        crate::live_view::require_latch_authority(&s, &actor)?;
        s.hub()
            .try_release_as(actor.local)
            .map_err(ApiError::conflict)?;
        dto = control_state_dto(&s, Some(&actor));
    } else {
        let request: live_view::ReleaseRequest = serde_json::from_slice(&body)
            .map_err(|_| ApiError::bad_request("invalid release request"))?;
        let result = crate::dto::ReleaseResult {
            takeover_id: request.takeover_id,
            request_id: request.request_id,
            state: "released".into(),
        };
        crate::live_view::release(&s, &actor, request).await?;
        dto = control_state_dto(&s, Some(&actor));
        dto.release = Some(result);
    }
    Ok(Json(dto))
}

/// The newest daemon-wide event cursor (`gseq`, `0` = no event yet): what a
/// client stores as its resume point after loading a snapshot, so a later
/// `GET /v1/events` with `Last-Event-ID: <gseq>` replays exactly what came
/// after.
#[utoipa::path(get, path = "/v1/events/latest", tag = "events", security(("bearer" = [])),
    responses((status = 200, body = LatestCursor)))]
pub(crate) async fn latest_cursor(
    State(s): State<AppState>,
) -> Result<Json<LatestCursor>, ApiError> {
    let st = s.storage_arc();
    let gseq = tokio::task::spawn_blocking(move || st.last_gseq())
        .await
        .map_err(ApiError::internal)?
        .map_err(ApiError::internal)?;
    Ok(Json(LatestCursor { gseq }))
}

/// The daemon's view of the desktop app's controller registration.
#[utoipa::path(get, path = "/v1/control/state", tag = "control", security(("bearer" = [])),
    responses((status = 200, body = ControlStateDto)))]
pub(crate) async fn get_control_state(
    State(s): State<AppState>,
    axum::Extension(actor): axum::Extension<crate::live_view::Actor>,
) -> Json<ControlStateDto> {
    Json(control_state_dto(&s, Some(&actor)))
}

/// Register the desktop app as controller and report its mode (heartbeat: send
/// it on every state change and at least every 5 s; it expires after 15 s).
/// While the mode is `human_driving` or `paused`, desktop actions of bots are
/// refused immediately.  A latched pause is never released by a heartbeat
/// (`idle` included): send `release: true` with `idle` / `bot_driving` (the
/// user's resume), or use `POST /v1/control/release`.
#[utoipa::path(put, path = "/v1/control/state", tag = "control", security(("bearer" = [])),
    request_body = ControlStateInput,
    responses((status = 200, body = ControlStateDto), (status = 400, body = ErrorBody),
        (status = 403, body = ErrorBody), (status = 409, body = ErrorBody)))]
pub(crate) async fn put_control_state(
    State(s): State<AppState>,
    axum::Extension(actor): axum::Extension<crate::live_view::Actor>,
    ApiJson(input): ApiJson<ControlStateInput>,
) -> Result<Json<ControlStateDto>, ApiError> {
    // The local app owns the controller registration and the latch. A paired
    // device may touch either only with the local user's explicit input grant:
    // without it, even reporting a mode or setting a first pause is refused.
    crate::live_view::require_latch_authority(&s, &actor)?;
    // The local app's Pause/Stop must work during anybody's takeover: it
    // overrides the takeover and keeps the pause. Paired peers cannot.
    let local_pause =
        actor.local && !input.release && input.mode == crate::dto::ControlMode::Paused;
    let mut takeover_seen = local_pause && s.hub().takeover().is_some();
    #[cfg(test)]
    if local_pause {
        let hook = s.live_view().put_state_hook.lock().clone();
        if let Some(hook) = hook {
            hook();
        }
    }
    // A takeover can start (or end) between the check and the write: the Stop
    // is retried against whatever the hub holds now, never lost to a 409.
    for _ in 0..3 {
        if takeover_seen || (local_pause && s.hub().takeover().is_some()) {
            takeover_seen = false;
            crate::live_view::override_local_pause(&s, &actor).await?;
            if s.hub().takeover().is_some() {
                return Ok(Json(control_state_dto(&s, Some(&actor))));
            }
            // The takeover ended (a peer's release finished) between the check
            // and the override: apply the pause the normal way, so it is not
            // lost and the registration is refreshed.
        }
        match s
            .hub()
            .try_apply_state_as(input.mode, input.release, actor.local)
        {
            Err(e) if local_pause && e == "owned takeover controls the latch" => {}
            Err(e) => {
                // A failed latch-file write is not a conflict: the pause is in
                // force in memory and the write is retried.
                return Err(if e == "control persistence failed" {
                    ApiError::unavailable(e)
                } else {
                    ApiError::conflict(e)
                });
            }
            Ok(()) => return Ok(Json(control_state_dto(&s, Some(&actor)))),
        }
    }
    Err(ApiError::conflict("owned takeover controls the latch"))
}

/// Answer a `control_requested` event: let the action `proceed` (after the
/// overlay highlight) or `refuse` it.
#[utoipa::path(post, path = "/v1/control/{request_id}", tag = "control",
    security(("bearer" = [])),
    params(("request_id" = Uuid, Path, description = "request_id of the control_requested event")),
    request_body = ControlDecisionInput,
    responses((status = 200, body = ControlDecisionResponse),
              (status = 404, body = ErrorBody, description = "unknown, answered or timed-out request")))]
pub(crate) async fn decide_control(
    State(s): State<AppState>,
    axum::Extension(actor): axum::Extension<crate::live_view::Actor>,
    Path(rid): Path<String>,
    ApiJson(input): ApiJson<ControlDecisionInput>,
) -> Result<Json<ControlDecisionResponse>, ApiError> {
    // The overlay question belongs to the local user on this screen: no paired
    // device may answer it (it would approve a bot action before the local
    // overlay decides).
    crate::live_view::require_local(&s, &actor, "control_decision")?;
    let request_id = id(&rid)?;
    if s.hub().decide(request_id, input.decision, input.reason) {
        Ok(Json(ControlDecisionResponse {
            request_id,
            decision: input.decision,
        }))
    } else {
        Err(ApiError::not_found("control request"))
    }
}

// ---------------------------------------------------------------- policy

use crate::policy::POLICY_FILE;

/// Counter that keeps the temp file names of concurrent saves apart.
static POLICY_TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn save_policy(data_dir: &std::path::Path, policy: &Policy) -> std::io::Result<()> {
    let path = data_dir.join(POLICY_FILE);
    // A unique temp name per call: even without the caller's lock two writers
    // never share (and then lose) one temp file.
    let n = POLICY_TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = path.with_extension(format!("json.{}.{n}.tmp", std::process::id()));
    let result = std::fs::write(&tmp, serde_json::to_vec_pretty(policy)?)
        .and_then(|()| std::fs::rename(&tmp, &path));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn named_preset(name: &str) -> Option<Policy> {
    match name.replace('_', "-").as_str() {
        "read-only" => Some(Policy::read_only()),
        "approve-destructive" => Some(Policy::approve_destructive()),
        "read-and-draft" => Some(Policy::read_and_draft()),
        "full" => Some(Policy::full()),
        _ => None,
    }
}

fn level_name(l: &Level) -> &'static str {
    match l {
        Level::Deny => "deny",
        Level::Ask => "ask",
        Level::Allow => "allow",
        Level::AllowWithLimits(_) => "allow_with_limits",
    }
}

fn policy_dto(p: &Policy) -> PolicyDto {
    let preset = ["read-only", "approve-destructive", "read-and-draft", "full"]
        .into_iter()
        .find(|n| named_preset(n).is_some_and(|q| q.categories == p.categories))
        .map(str::to_owned);
    PolicyDto {
        preset,
        load_error: None,
        categories: p
            .categories
            .iter()
            .map(|(c, r)| (category_name(*c), level_name(&r.level).to_owned()))
            .collect(),
    }
}

/// The global default permission policy (what bots without their own rule
/// for a category get).
#[utoipa::path(get, path = "/v1/policy", tag = "policy", security(("bearer" = [])),
    responses((status = 200, body = PolicyDto)))]
pub(crate) async fn get_policy(State(s): State<AppState>) -> Json<PolicyDto> {
    let mut dto = policy_dto(&s.agent().global_policy());
    dto.load_error = s.policy_load_error();
    Json(dto)
}

/// Replace the global default policy (persisted; effective for the next tool
/// call of every run).  Start from `preset`, or from the current policy
/// without it, then apply the `categories` overrides.
#[utoipa::path(put, path = "/v1/policy", tag = "policy", security(("bearer" = [])),
    request_body = PolicyInput,
    responses((status = 200, body = PolicyDto), (status = 400, body = ErrorBody)))]
pub(crate) async fn put_policy(
    State(s): State<AppState>,
    ApiJson(input): ApiJson<PolicyInput>,
) -> Result<Json<PolicyDto>, ApiError> {
    // Read-modify-write of the global policy and its file: one writer at a time
    // (no `.await` below, so the guard is never held across a suspension point).
    let _guard = s.policy_lock();
    let mut policy = match input.preset.as_deref() {
        Some(name) => named_preset(name)
            .ok_or_else(|| ApiError::bad_request(format!("unknown preset {name:?}")))?,
        None => s.agent().global_policy(),
    };
    for (name, level) in &input.categories {
        let cat = match serde_json::from_value::<PermissionCategory>(Value::String(name.clone())) {
            Ok(c) if c != PermissionCategory::Meta => c,
            _ => {
                return Err(ApiError::bad_request(format!(
                    "unknown permission category {name:?}"
                )));
            }
        };
        let level = match level.as_str() {
            "deny" => Level::Deny,
            "ask" => Level::Ask,
            "allow" => Level::Allow,
            other => {
                return Err(ApiError::bad_request(format!(
                    "level of {name:?} must be deny, ask or allow, not {other:?}"
                )));
            }
        };
        policy.categories.insert(cat, Rule::new(level));
    }
    save_policy(&s.data_dir(), &policy).map_err(ApiError::internal)?;
    s.agent().set_global_policy(policy.clone());
    // The user replaced the policy: the locked deny-all mode ends here.
    s.set_policy_load_error(None);
    s.agent().set_policy_locked(false);
    Ok(Json(policy_dto(&policy)))
}

// ------------------------------------------------------------- providers

fn kind_name(k: &provider_catalog::ProviderKind) -> String {
    use provider_catalog::ProviderKind;
    match k {
        ProviderKind::OpenAiCompatible => "openai_compatible".to_owned(),
        ProviderKind::Anthropic => "anthropic".to_owned(),
        ProviderKind::Gemini => "gemini".to_owned(),
        ProviderKind::Cli => "cli".to_owned(),
        ProviderKind::Other(o) => format!("other:{o}"),
    }
}

/// How long listing one provider's models may take.
const MODELS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(4);

async fn models_of(
    p: Option<std::sync::Arc<dyn provider_catalog::ChatProvider>>,
) -> (Vec<String>, Option<String>) {
    let Some(p) = p else {
        return (Vec::new(), None);
    };
    match tokio::time::timeout(MODELS_TIMEOUT, p.list_models()).await {
        Ok(Ok(m)) => (m, None),
        Ok(Err(e)) => (Vec::new(), Some(e.to_string())),
        Err(_) => (Vec::new(), Some("timed out".to_owned())),
    }
}

/// Configured providers, catalog entries and the models each configured
/// provider offers (fetched live, with a short timeout; failures are reported
/// per provider and never fail the route).
#[utoipa::path(get, path = "/v1/providers", tag = "providers", security(("bearer" = [])),
    params(crate::provider_models::ProvidersQuery),
    responses((status = 200, body = ProvidersDto)))]
#[allow(clippy::too_many_lines)] // one flat DTO mapping
pub(crate) async fn list_providers(
    State(s): State<AppState>,
    Query(query): Query<crate::provider_models::ProvidersQuery>,
) -> Result<Json<ProvidersDto>, ApiError> {
    let cfg = provider_catalog::ProviderConfig::load(&provider_catalog::config_path(&s.data_dir()))
        .map_err(|e| {
            ApiError::internal(s.agent().redact_host_text(format!("providers.toml: {e}")))
        })?;
    let live = s.agent().providers();
    let mut active = live.ids();
    active.extend(
        cfg.providers
            .iter()
            .filter(|p| p.enabled && p.cli.is_some())
            .map(|p| p.id.clone()),
    );
    active.sort();
    active.dedup();
    let default_id = cfg.effective_default().map(|d| d.id.clone());
    let lookups = cfg.providers.iter().map(|p| models_of(live.get(&p.id)));
    let models = futures::future::join_all(lookups).await;
    let configured = cfg
        .providers
        .iter()
        .zip(models)
        .map(|(p, (models, models_error))| {
            let mut labels = std::collections::HashMap::new();
            let (mut models_source, mut models_fetched_at_ms) = (None, None);
            let models = if p.cli.is_some() {
                // The CLI's own list (disk-cached); never waits for the CLI.
                let engine = p.id.strip_prefix("cli:").unwrap_or("");
                let list = s.cli_models().get_or_bundled(engine);
                crate::cli_engines::spawn_models_refresh(&s, engine);
                if cli_provider::models::can_list(engine) {
                    models_source = Some(list.source.as_str().to_owned());
                    models_fetched_at_ms = (list.fetched_at_ms > 0).then_some(list.fetched_at_ms);
                }
                labels = list
                    .models
                    .iter()
                    .filter_map(|m| m.label.clone().map(|l| (m.id.clone(), l)))
                    .collect();
                list.models.into_iter().map(|m| m.id).collect()
            } else {
                models
            };
            let model_details = crate::provider_models::for_picker(
                s.agent(),
                p,
                models,
                &labels,
                query.chat_only.unwrap_or(true),
            );
            ConfiguredProviderDto {
                id: p.id.clone(),
                enabled: p.enabled,
                is_default: default_id.as_deref() == Some(p.id.as_str()),
                default_model: p.default_model.clone(),
                has_key: p.key.is_some(),
                custom: p.custom.is_some(),
                active: active.contains(&p.id),
                models: model_details.iter().map(|m| m.id.clone()).collect(),
                model_details,
                models_error: cfg
                    .quarantined
                    .get(&p.id)
                    .cloned()
                    .or_else(|| models_error.map(|e| s.agent().redact_host_text(e))),
                models_source,
                models_fetched_at_ms,
            }
        })
        .collect();
    let mut catalog: Vec<CatalogProviderDto> = s
        .provider_host()
        .map(|h| {
            h.catalog
                .all()
                .map(|e| CatalogProviderDto {
                    id: e.id.clone(),
                    kind: kind_name(&e.kind),
                    base_url: e.base_url.clone(),
                    auth: match e.auth {
                        provider_catalog::Auth::ApiKeyEnv(_) => "api_key_env",
                        provider_catalog::Auth::None => "none",
                    }
                    .to_owned(),
                    key_env: match &e.auth {
                        provider_catalog::Auth::ApiKeyEnv(v) => Some(v.clone()),
                        provider_catalog::Auth::None => None,
                    },
                    tools: e.capabilities.tools,
                    vision: e.capabilities.vision,
                    configured: cfg.get(&e.id).is_some(),
                })
                .collect()
        })
        .unwrap_or_default();
    for p in cfg.providers.iter().filter(|p| p.cli.is_some()) {
        catalog.push(CatalogProviderDto {
            id: p.id.clone(),
            kind: "cli".into(),
            base_url: String::new(),
            auth: "none".into(),
            key_env: None,
            tools: true,
            vision: false,
            configured: true,
        });
    }
    catalog.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(Json(ProvidersDto {
        default_provider: (!live.setup_required()).then(|| live.default_id().to_owned()),
        active,
        configured,
        catalog,
    }))
}

/// Query parameters for `GET /v1/providers/detected`.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub(crate) struct DetectProvidersQuery {
    /// When set to 1, bypasses the in-process 60-second cache.
    #[param(inline)]
    pub refresh: Option<String>,
}

/// Detect installed, signed-in coding CLIs. Local owner only: the rows carry
/// the signed-in account label (an email or organisation) and install state.
#[utoipa::path(get, path = "/v1/providers/detected", tag = "providers", security(("bearer" = [])),
    params(DetectProvidersQuery),
    responses((status = 200, body = Vec<DetectedProviderDto>),
              (status = 403, body = ErrorBody, description = "caller is not the local owner")))]
pub(crate) async fn detect_providers(
    State(s): State<AppState>,
    axum::Extension(actor): axum::Extension<crate::live_view::Actor>,
    Query(query): Query<DetectProvidersQuery>,
) -> Result<Json<Vec<DetectedProviderDto>>, ApiError> {
    crate::cli_engines::local_owner(&actor)?;
    let refresh = query
        .refresh
        .as_deref()
        .is_some_and(|r| r == "1" || r.eq_ignore_ascii_case("true"));
    let detected = s.detector().detect(refresh).await;
    Ok(Json(detected.into_iter().map(Into::into).collect()))
}

/// Registry id to the provider-catalog engine kind (`None`: never a provider).
pub(crate) fn cli_engine_kind(id: &str) -> Option<provider_catalog::config::CliEngine> {
    use provider_catalog::config::CliEngine;
    Some(match id {
        "claude" => CliEngine::Claude,
        "codex" => CliEngine::Codex,
        "cursor" => CliEngine::Cursor,
        "opencode" => CliEngine::OpenCode,
        "gemini" => CliEngine::Gemini,
        "agy" => CliEngine::Agy,
        _ => return None,
    })
}

/// Write the `cli:<id>` entry of `providers.toml`. The entry is also the memory of the
/// owner's choice: an engine with an entry (on or off) is never auto-enabled again.
pub(crate) fn persist_cli_enable(
    s: &AppState,
    spec: &cli_provider::CliEngineSpec,
    engine: provider_catalog::config::CliEngine,
    enabled: bool,
) -> Result<ConfiguredProviderDto, ApiError> {
    let provider_id = format!("cli:{}", spec.id);
    let _guard = s.providers_lock();
    let path = provider_catalog::config_path(&s.data_dir());
    let _file_lock = provider_catalog::ProviderConfig::lock(&path).map_err(|e| match e {
        provider_catalog::ConfigError::LockTimeout(_) => {
            ApiError::unavailable(format!("providers.lock: {e}"))
        }
        other => ApiError::internal(format!("providers.lock: {other}")),
    })?;
    let mut cfg = provider_catalog::ProviderConfig::load(&path)
        .map_err(|e| ApiError::internal(format!("providers.toml: {e}")))?;
    let existing = cfg.get(&provider_id).cloned();
    let mut settings = provider_catalog::ProviderSettings::new(&provider_id, now_ms());
    settings.enabled = enabled;
    if let Some(old) = existing {
        settings.default_model = old.default_model;
        settings.model_caps = old.model_caps;
    }
    settings.cli = Some(provider_catalog::config::CliSettings::Cli {
        engine,
        command: spec.command.to_owned(),
    });
    cfg.upsert(settings);
    cfg.save(&path)
        .map_err(|e| ApiError::internal(format!("providers.toml: {e}")))?;
    let saved = cfg
        .get(&provider_id)
        .cloned()
        .ok_or_else(|| ApiError::internal("enabled CLI provider missing after upsert"))?;
    let is_default = cfg.effective_default().is_some_and(|d| d.id == provider_id);
    s.storage()
        .append_audit(
            s.user_id(),
            None,
            "user",
            if enabled {
                "provider.cli_enable"
            } else {
                "provider.cli_disable"
            },
            &serde_json::json!({ "id": &provider_id, "engine": spec.id }),
        )
        .map_err(ApiError::internal)?;
    let list = s.cli_models().get_or_bundled(spec.id);
    let labels = list
        .models
        .iter()
        .filter_map(|m| m.label.clone().map(|l| (m.id.clone(), l)))
        .collect();
    let model_details = crate::provider_models::for_picker(
        s.agent(),
        &saved,
        list.models.iter().map(|m| m.id.clone()).collect(),
        &labels,
        true,
    );
    Ok(ConfiguredProviderDto {
        id: provider_id,
        enabled: saved.enabled,
        is_default,
        default_model: saved.default_model,
        has_key: false,
        custom: false,
        active: saved.enabled,
        models: model_details.iter().map(|m| m.id.clone()).collect(),
        model_details,
        models_error: None,
        models_source: cli_provider::models::can_list(spec.id)
            .then(|| list.source.as_str().to_owned()),
        models_fetched_at_ms: (cli_provider::models::can_list(spec.id) && list.fetched_at_ms > 0)
            .then_some(list.fetched_at_ms),
    })
}

pub(crate) async fn reload_from_host(s: &AppState) -> Result<ReloadResponse, ApiError> {
    let st = s.clone();
    let built =
        tokio::task::spawn_blocking(move || (st.provider_host().expect("checked above").reload)())
            .await
            .map_err(ApiError::internal)?;
    let providers =
        built.map_err(|m| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "reload_failed", m))?;
    let out = ReloadResponse {
        default_provider: (!providers.setup_required()).then(|| providers.default_id().to_owned()),
        active: providers.ids(),
    };
    s.agent().set_providers(providers);
    Ok(out)
}

/// Enable a detected coding CLI provider server-side (`providers.toml` + live reload).
#[utoipa::path(post, path = "/v1/providers/cli/enable", tag = "providers",
    security(("bearer" = [])),
    request_body = EnableCliProviderInput,
    responses((status = 200, body = ConfiguredProviderDto),
              (status = 400, body = ErrorBody, description = "engine is not runnable or unknown")))]
pub(crate) async fn enable_cli_provider(
    State(s): State<AppState>,
    axum::Extension(actor): axum::Extension<crate::live_view::Actor>,
    ApiJson(body): ApiJson<EnableCliProviderInput>,
) -> Result<Json<ConfiguredProviderDto>, ApiError> {
    crate::cli_engines::local_owner(&actor)?;
    let Some(spec) = cli_provider::engine_spec(&body.id).filter(|spec| spec.can_enable()) else {
        return Err(ApiError::bad_request(cli_provider::enable_refusal_message(
            &body.id,
        )));
    };
    let Some(engine) = cli_engine_kind(spec.id) else {
        return Err(ApiError::bad_request(cli_provider::enable_refusal_message(
            spec.id,
        )));
    };
    let st = s.clone();
    let dto = tokio::task::spawn_blocking(move || persist_cli_enable(&st, spec, engine, true))
        .await
        .map_err(ApiError::internal)??;
    if s.provider_host().is_some() {
        let _ = reload_from_host(&s).await?;
    }
    Ok(Json(dto))
}

/// Switch a coding CLI provider off and remember that: it is not turned on again
/// automatically when it is detected.
#[utoipa::path(post, path = "/v1/providers/cli/disable", tag = "providers",
    security(("bearer" = [])),
    request_body = EnableCliProviderInput,
    responses((status = 200, body = ConfiguredProviderDto),
              (status = 400, body = ErrorBody, description = "engine is unknown or never a provider")))]
pub(crate) async fn disable_cli_provider(
    State(s): State<AppState>,
    axum::Extension(actor): axum::Extension<crate::live_view::Actor>,
    ApiJson(body): ApiJson<EnableCliProviderInput>,
) -> Result<Json<ConfiguredProviderDto>, ApiError> {
    crate::cli_engines::local_owner(&actor)?;
    let Some((spec, engine)) = cli_provider::engine_spec(&body.id)
        .and_then(|spec| cli_engine_kind(spec.id).map(|engine| (spec, engine)))
    else {
        return Err(ApiError::bad_request(cli_provider::enable_refusal_message(
            &body.id,
        )));
    };
    let st = s.clone();
    let dto = tokio::task::spawn_blocking(move || persist_cli_enable(&st, spec, engine, false))
        .await
        .map_err(ApiError::internal)??;
    if s.provider_host().is_some() {
        let _ = reload_from_host(&s).await?;
    }
    Ok(Json(dto))
}

/// Rebuild the loop's providers from `providers.toml` and the secret store, so
/// a provider added with the CLI / app is usable without restarting the
/// daemon.  Runs already streaming keep their provider.
#[utoipa::path(post, path = "/v1/providers/reload", tag = "providers",
    security(("bearer" = [])),
    responses((status = 200, body = ReloadResponse),
              (status = 409, body = ErrorBody, description = "the daemon has no provider configuration"),
              (status = 422, body = ErrorBody, description = "no usable provider; the old ones stay")))]
pub(crate) async fn reload_providers(
    State(s): State<AppState>,
) -> Result<Json<ReloadResponse>, ApiError> {
    if s.provider_host().is_none() {
        return Err(ApiError::conflict(
            "this daemon was started without provider configuration (fake provider)",
        ));
    }
    Ok(Json(reload_from_host(&s).await?))
}

// -------------------------------------------------------------- schedules

/// Largest webhook body accepted (256 KB).
pub(crate) const WEBHOOK_MAX_BODY: usize = 256 * 1024;

fn bad_schedule(m: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "bad_schedule", m)
}

fn schedule_dto(sc: &Schedule, has_webhook: bool) -> ScheduleDto {
    ScheduleDto {
        id: sc.id.0,
        bot_id: sc.bot_id.0,
        chat_id: sc.chat_id.map(|c| c.0),
        name: sc.name.clone(),
        spec: serde_json::to_value(&sc.spec).unwrap_or(Value::Null),
        payload: serde_json::to_value(&sc.payload).unwrap_or(Value::Null),
        enabled: sc.enabled,
        created_at_ms: sc.created_at,
        last_run_at_ms: sc.last_run_at,
        next_run_at_ms: sc.next_run_at,
        misfire_policy: sc.misfire_policy.as_str().to_owned(),
        has_webhook,
        trigger: None,
        precheck: sc
            .precheck
            .as_ref()
            .map(|p| serde_json::to_value(p).unwrap_or(Value::Null)),
        skipped_by_precheck: 0,
        consecutive_errors: 0,
        origin: sc.origin.as_str().to_owned(),
        missed_occurrences: sc.missed_occurrences,
        moved_to: None,
        next_runs_ms: Vec::new(),
    }
}

fn dto_of(s: &AppState, sc: &Schedule) -> Result<ScheduleDto, ApiError> {
    let store = s.schedule_store()?;
    let trigger = store.get_trigger(sc.id)?;
    let has = matches!(trigger, Some(Trigger::Webhook { .. }));
    let mut dto = schedule_dto(sc, has);
    dto.moved_to = store.frozen_for(sc.id)?;
    if matches!(
        trigger,
        Some(Trigger::FileWatch { .. } | Trigger::Email { .. })
    ) {
        dto.next_run_at_ms = None;
    }
    dto.next_runs_ms = compute_next_runs(sc, dto.next_run_at_ms, dto.moved_to.is_some());
    dto.trigger = trigger
        .filter(|t| !matches!(t, Trigger::Webhook { .. }))
        .map(|t| serde_json::to_value(t).unwrap_or(Value::Null));
    let checks = store.check_state(sc)?;
    dto.skipped_by_precheck = checks.skipped_by_precheck;
    dto.consecutive_errors = checks.precheck_errors.max(checks.trigger_errors);
    if let Some(value) = &mut dto.trigger {
        redact_schedule_value(s, value);
    }
    if let Some(value) = &mut dto.precheck {
        redact_schedule_value(s, value);
    }
    Ok(dto)
}

fn compute_next_runs(sc: &Schedule, next_run_at: Option<i64>, is_moved: bool) -> Vec<i64> {
    if !sc.enabled || is_moved {
        return Vec::new();
    }
    let Some(first) = next_run_at else {
        return Vec::new();
    };
    if matches!(sc.spec, ScheduleSpec::At { .. }) {
        return vec![first];
    }
    let mut runs = vec![first];
    if let Ok(more) = sc.spec.next_occurrences(first, 4) {
        runs.extend(more);
    }
    runs
}

/// The schedule `id`, when its bot belongs to the local user.
fn schedule_of_user(s: &AppState, raw: &str) -> Result<Schedule, ApiError> {
    let sid = TaskId(id(raw)?);
    s.schedule_store()?
        .get(sid)?
        .filter(|sc| s.bot_of_user(sc.bot_id).is_ok())
        .ok_or_else(|| ApiError::not_found(format!("schedule {sid}")))
}

/// Validated pieces of a [`ScheduleInput`].
struct ScheduleParts {
    spec: ScheduleSpec,
    payload: Payload,
    policy: MisfirePolicy,
    chat_id: Option<ChatId>,
}

fn schedule_parts(s: &AppState, input: &ScheduleInput) -> Result<ScheduleParts, ApiError> {
    s.bot_of_user(BotId(input.bot_id))?;
    if input.name.trim().is_empty() {
        return Err(ApiError::bad_request("name must not be empty"));
    }
    let spec: ScheduleSpec = serde_json::from_value(input.spec.clone())
        .map_err(|e| bad_schedule(format!("invalid spec: {e}")))?;
    spec.validate()?;
    let payload: Payload = serde_json::from_value(input.payload.clone())
        .map_err(|e| bad_schedule(format!("invalid payload: {e}")))?;
    match &payload {
        Payload::AgentPrompt { text } if text.trim().is_empty() => {
            return Err(bad_schedule("prompt text must not be empty"));
        }
        Payload::Workflow { name, params } if name.trim().is_empty() => {
            let _ = params;
            return Err(bad_schedule("workflow name must not be empty"));
        }
        Payload::Tool { name, .. } if name.trim().is_empty() => {
            return Err(bad_schedule("tool name must not be empty"));
        }
        _ => {}
    }
    let policy = match input.misfire_policy.as_deref() {
        None => MisfirePolicy::default(),
        Some(p) => MisfirePolicy::parse(p)
            .ok_or_else(|| bad_schedule(format!("unknown misfire_policy {p:?}")))?,
    };
    let chat_id = match input.chat_id {
        None => None,
        Some(c) => {
            let chat = s.chat_of_user(ChatId(c))?;
            if chat.bot_id.0 != input.bot_id {
                return Err(bad_schedule("the chat belongs to another bot"));
            }
            Some(chat.id)
        }
    };
    Ok(ScheduleParts {
        spec,
        payload,
        policy,
        chat_id,
    })
}

fn apply_webhook(
    store: &scheduler::ScheduleStore,
    sid: TaskId,
    secret: Option<&str>,
) -> Result<(), ApiError> {
    match secret {
        None => {}
        Some("") => {
            store.remove_trigger(sid)?;
        }
        Some(sec) => store.set_trigger(
            sid,
            &Trigger::Webhook {
                secret: sec.to_owned(),
            },
        )?,
    }
    Ok(())
}

fn redact_schedule_value(s: &AppState, value: &mut Value) {
    match value {
        Value::String(text) => *text = s.0.agent.redact_host_text(text.clone()),
        Value::Array(a) => {
            for value in a {
                redact_schedule_value(s, value);
            }
        }
        Value::Object(o) => {
            let previous = std::mem::take(o);
            for (key, mut value) in previous {
                redact_schedule_value(s, &mut value);
                o.insert(s.0.agent.redact_host_text(key), value);
            }
        }
        _ => {}
    }
}

async fn schedule_extensions(
    s: &AppState,
    schedule: &mut Schedule,
    input: &ScheduleInput,
) -> Result<Option<Option<Trigger>>, ApiError> {
    if input.trigger.is_some() && input.webhook_secret.is_some() {
        return Err(bad_schedule("give trigger or webhook_secret, not both"));
    }
    for value in [&input.trigger, &input.precheck].into_iter().flatten() {
        let mut shown = value.clone();
        redact_schedule_value(s, &mut shown);
        if shown != *value {
            return Err(bad_schedule(
                "schedule extension contains protected secret text",
            ));
        }
    }
    if input.trigger.is_some() || input.precheck.is_some() || schedule.precheck.is_some() {
        for value in [
            serde_json::to_value(&schedule.payload).unwrap_or(Value::Null),
            Value::String(schedule.name.clone()),
        ] {
            let mut shown = value.clone();
            redact_schedule_value(s, &mut shown);
            if shown != value {
                return Err(bad_schedule(
                    "schedule definition contains protected secret text",
                ));
            }
        }
    }
    if let Some(value) = &input.precheck {
        schedule.precheck = if value.is_null() {
            None
        } else {
            let check: scheduler::Precheck = serde_json::from_value(value.clone())
                .map_err(|_| bad_schedule("invalid precheck"))?;
            crate::schedule::validate_precheck(&s.0.agent, schedule.bot_id, &check)
                .map_err(bad_schedule)?;
            Some(check)
        };
    }
    match &input.trigger {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(value) => {
            let trigger: Trigger = serde_json::from_value(value.clone())
                .map_err(|_| bad_schedule("invalid trigger"))?;
            if matches!(trigger, Trigger::Webhook { .. }) {
                return Err(bad_schedule(
                    "use the existing webhook configuration for webhooks",
                ));
            }
            crate::schedule::validate_trigger(
                &s.0.agent,
                &s.0.storage,
                schedule.bot_id,
                schedule.id,
                &trigger,
            )
            .await
            .map_err(bad_schedule)?;
            Ok(Some(Some(trigger)))
        }
    }
}

/// List schedules.
#[utoipa::path(get, path = "/v1/schedules", tag = "schedules", security(("bearer" = [])),
    responses((status = 200, body = Vec<ScheduleDto>)))]
pub(crate) async fn list_schedules(
    State(s): State<AppState>,
) -> Result<Json<Vec<ScheduleDto>>, ApiError> {
    let mut out = Vec::new();
    for sc in s.schedule_store()?.list()? {
        if s.bot_of_user(sc.bot_id).is_ok() {
            out.push(dto_of(&s, &sc)?);
        }
    }
    Ok(Json(out))
}

/// Create a schedule (armed unless `enabled` is `false`).
#[utoipa::path(post, path = "/v1/schedules", tag = "schedules", security(("bearer" = [])),
    request_body = ScheduleInput,
    responses((status = 201, body = ScheduleDto), (status = 404, body = ErrorBody),
              (status = 422, body = ErrorBody)))]
pub(crate) async fn create_schedule(
    State(s): State<AppState>,
    peer: Option<axum::Extension<crate::peer::PeerIdentity>>,
    ApiJson(input): ApiJson<ScheduleInput>,
) -> Result<(StatusCode, Json<ScheduleDto>), ApiError> {
    if peer.is_some() && (input.trigger.is_some() || input.precheck.is_some()) {
        return Err(ApiError::forbidden(
            "schedule triggers and prechecks are local-owner only",
        ));
    }
    let p = schedule_parts(&s, &input)?;
    let mut sc = Schedule::new(
        BotId(input.bot_id),
        p.chat_id,
        input.name.trim(),
        p.spec,
        p.payload,
        p.policy,
        now_ms(),
    )?;
    if input.enabled == Some(false) {
        sc.enabled = false;
        sc.next_run_at = None;
    }
    let trigger = schedule_extensions(&s, &mut sc, &input).await?;
    let store = s.schedule_store()?;
    s.0.storage.append_audit(
        s.user_id(),
        None,
        "user",
        "schedule.create_intent",
        &serde_json::json!({"schedule_id":sc.id.to_string(),"bot_id":sc.bot_id.to_string()}),
    )?;
    store.insert_config(&sc, trigger.flatten().as_ref())?;
    if let Err(e) = apply_webhook(&store, sc.id, input.webhook_secret.as_deref()) {
        let _ = store.delete(sc.id);
        return Err(e);
    }
    s.publish_schedule_changed(sc.id.0, ScheduleChangeKind::Created);
    Ok((
        StatusCode::CREATED,
        Json(dto_of(&s, &store.get(sc.id)?.unwrap_or(sc))?),
    ))
}

/// A schedule moved from the owner's other computer (`host_import`): the same
/// validation as `POST /v1/schedules` (bot, spec, payload, chat, precheck, trigger),
/// but the schedule keeps its id and creation time, so a second import finds it
/// ("exists") and never duplicates it. Next-run times are recomputed on THIS
/// clock from the spec, whose time zone is part of the spec itself.
pub(crate) async fn import_schedule(
    s: &AppState,
    input: &ScheduleInput,
    id: TaskId,
    created_at_ms: i64,
    origin: scheduler::ScheduleOrigin,
) -> Result<Schedule, ApiError> {
    let p = schedule_parts(s, input)?;
    let mut sc = Schedule::new(
        BotId(input.bot_id),
        p.chat_id,
        input.name.trim(),
        p.spec,
        p.payload,
        p.policy,
        now_ms(),
    )?;
    sc.id = id;
    sc.created_at = created_at_ms.min(now_ms());
    sc.origin = origin;
    if input.enabled == Some(false) {
        sc.enabled = false;
        sc.next_run_at = None;
    }
    let trigger = schedule_extensions(s, &mut sc, input).await?;
    let store = s.schedule_store()?;
    s.0.storage.append_audit(
        s.user_id(),
        None,
        "user",
        "schedule.import_intent",
        &serde_json::json!({"schedule_id":sc.id.to_string(),"bot_id":sc.bot_id.to_string()}),
    )?;
    store.insert_config(&sc, trigger.flatten().as_ref())?;
    s.publish_schedule_changed(sc.id.0, ScheduleChangeKind::Created);
    Ok(store.get(sc.id)?.unwrap_or(sc))
}

/// Get one schedule.
#[utoipa::path(get, path = "/v1/schedules/{id}", tag = "schedules", security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "Schedule id")),
    responses((status = 200, body = ScheduleDto), (status = 404, body = ErrorBody)))]
pub(crate) async fn get_schedule(
    State(s): State<AppState>,
    Path(sid): Path<String>,
) -> Result<Json<ScheduleDto>, ApiError> {
    Ok(Json(dto_of(&s, &schedule_of_user(&s, &sid)?)?))
}

/// Replace a schedule's definition (its bot stays).  A changed or re-enabled
/// timer is re-armed from now; run state (`last_run_at_ms`) is kept.
#[utoipa::path(put, path = "/v1/schedules/{id}", tag = "schedules", security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "Schedule id")),
    request_body = ScheduleInput,
    responses((status = 200, body = ScheduleDto), (status = 404, body = ErrorBody),
              (status = 422, body = ErrorBody)))]
pub(crate) async fn update_schedule(
    State(s): State<AppState>,
    Path(sid): Path<String>,
    peer: Option<axum::Extension<crate::peer::PeerIdentity>>,
    ApiJson(input): ApiJson<ScheduleInput>,
) -> Result<Json<ScheduleDto>, ApiError> {
    let mut sc = schedule_of_user(&s, &sid)?;
    // A schedule that moved to a server is frozen here: no update, not even a paused one,
    // (see `ScheduleStore::freeze`); `POST /v1/schedules/{id}/reclaim` is the way back.
    if s.schedule_store()?.frozen_for(sc.id)?.is_some() {
        return Err(scheduler::SchedulerError::Frozen(sc.id.to_string()).into());
    }
    if peer.is_some()
        && (input.trigger.is_some()
            || input.precheck.is_some()
            || sc.precheck.is_some()
            || matches!(
                s.schedule_store()?.get_trigger(sc.id)?,
                Some(Trigger::FileWatch { .. } | Trigger::Email { .. })
            ))
    {
        return Err(ApiError::forbidden(
            "schedule triggers and prechecks are local-owner only",
        ));
    }
    if input.bot_id != sc.bot_id.0 {
        return Err(bad_schedule("the bot of a schedule cannot be changed"));
    }
    let p = schedule_parts(&s, &input)?;
    input.name.trim().clone_into(&mut sc.name);
    sc.chat_id = p.chat_id;
    sc.payload = p.payload;
    sc.misfire_policy = p.policy;
    sc.spec = p.spec;
    let trigger = schedule_extensions(&s, &mut sc, &input).await?;
    let store = s.schedule_store()?;
    s.0.storage.append_audit(
        s.user_id(),
        None,
        "user",
        "schedule.update_intent",
        &serde_json::json!({"schedule_id":sc.id.to_string(),"bot_id":sc.bot_id.to_string()}),
    )?;
    // User edits take ownership in the same revision-checked transaction.
    sc.origin = scheduler::ScheduleOrigin::User;
    // Run progress may have changed after schedule_of_user. The store reads
    // it again and computes any re-arm under the same write transaction.
    let (sc, was_enabled) = persist_schedule_edit(
        &store,
        &sc,
        input.enabled,
        now_ms(),
        trigger.as_ref().map(|t| t.as_ref()),
    )?;
    apply_webhook(&store, sc.id, input.webhook_secret.as_deref())?;
    s.publish_schedule_changed(
        sc.id.0,
        if sc.enabled == was_enabled {
            ScheduleChangeKind::Updated
        } else {
            ScheduleChangeKind::Toggled
        },
    );
    Ok(Json(dto_of(&s, &store.get(sc.id)?.unwrap_or(sc))?))
}

/// Persist the PUT definition without trusting the route's runtime snapshot.
/// Keeping this boundary separate also allows deterministic read/write race
/// tests without a process-global hook or a real clock/sleep.
#[allow(clippy::option_option)] // Omitted preserves, null clears, value replaces the trigger.
fn persist_schedule_edit(
    store: &scheduler::ScheduleStore,
    definition: &Schedule,
    enabled: Option<bool>,
    now: i64,
    trigger: Option<Option<&Trigger>>,
) -> Result<(Schedule, bool), ApiError> {
    store
        .update_config(definition, enabled, now, trigger)?
        .ok_or_else(|| ApiError::not_found(format!("schedule {}", definition.id)))
}

/// Delete a schedule (and its webhook trigger).  Runs it already started stay.
#[utoipa::path(delete, path = "/v1/schedules/{id}", tag = "schedules",
    security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "Schedule id")),
    responses((status = 204), (status = 404, body = ErrorBody)))]
pub(crate) async fn delete_schedule(
    State(s): State<AppState>,
    Path(sid): Path<String>,
) -> Result<StatusCode, ApiError> {
    let sc = schedule_of_user(&s, &sid)?;
    let store = s.schedule_store()?;
    s.0.storage.append_audit(
        s.user_id(),
        None,
        "user",
        "schedule.delete_intent",
        &serde_json::json!({"schedule_id":sc.id.to_string()}),
    )?;
    // Foreign-key cascade removes the trigger in the same transaction; never
    // briefly expose a deleted file/e-mail trigger as an ordinary timer.
    store.delete(sc.id)?;
    s.publish_schedule_changed(sc.id.0, ScheduleChangeKind::Deleted);
    Ok(StatusCode::NO_CONTENT)
}

/// Fire a schedule now, in the background; its timer is untouched.  The run is
/// a normal run (its `run_started` event carries the `schedule_id`).
#[utoipa::path(post, path = "/v1/schedules/{id}/run-now", tag = "schedules",
    security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "Schedule id")),
    responses((status = 202, body = AcceptedResponse), (status = 404, body = ErrorBody),
              (status = 409, body = ErrorBody, description = "a run of it is still in flight"),
              (status = 503, body = ErrorBody, description = "the scheduler is not running")))]
pub(crate) async fn run_schedule_now(
    State(s): State<AppState>,
    Path(sid): Path<String>,
) -> Result<(StatusCode, Json<AcceptedResponse>), ApiError> {
    let sc = schedule_of_user(&s, &sid)?;
    if s.schedule_store()?.frozen_for(sc.id)?.is_some() {
        return Err(scheduler::SchedulerError::Frozen(sc.id.to_string()).into());
    }
    let scheduler = s.scheduler()?;
    s.leader().with_active(|| scheduler.run_now(sc.id))??;
    Ok((
        StatusCode::ACCEPTED,
        Json(AcceptedResponse {
            status: "accepted".to_owned(),
            schedule_id: sc.id.0,
        }),
    ))
}

/// Turn an English / Turkish phrase into a schedule spec.
#[utoipa::path(post, path = "/v1/schedules/parse", tag = "schedules",
    security(("bearer" = [])),
    request_body = ParseSchedule,
    responses((status = 200, body = ParsedSchedule),
              (status = 422, body = ErrorBody, description = "the phrase is not understood")))]
pub(crate) async fn parse_schedule(
    ApiJson(input): ApiJson<ParseSchedule>,
) -> Result<Json<ParsedSchedule>, ApiError> {
    let now = now_ms();
    let tz = input.tz.as_deref().map(str::trim).filter(|t| !t.is_empty());
    let spec = scheduler::parse_natural_language_with_tz(&input.text, now, tz)
        .ok_or_else(|| bad_schedule(format!("cannot understand {:?}", input.text)))?;
    // A bad timezone only shows when the spec is validated.
    spec.validate()?;
    let next_runs_ms = preview_runs_of(&spec, now, 5)?;
    Ok(Json(ParsedSchedule {
        spec: serde_json::to_value(&spec).unwrap_or(Value::Null),
        next_run_at_ms: next_runs_ms.first().copied(),
        next_runs_ms,
    }))
}

fn preview_runs_of(
    spec: &ScheduleSpec,
    anchor_ms: i64,
    count: usize,
) -> Result<Vec<i64>, ApiError> {
    match spec {
        ScheduleSpec::At { unix_ms } => Ok(vec![*unix_ms]),
        other => Ok(other.next_occurrences(anchor_ms, count)?),
    }
}

fn apply_tz_to_spec(spec: ScheduleSpec, tz: Option<&str>) -> Result<ScheduleSpec, ApiError> {
    let Some(tz_name) = tz.map(str::trim).filter(|t| !t.is_empty()) else {
        spec.validate()?;
        return Ok(spec);
    };
    let resolved = match spec {
        ScheduleSpec::Cron(raw) => {
            let trimmed = raw.trim();
            if trimmed.split_whitespace().count() == 5 {
                ScheduleSpec::cron_tz(trimmed, tz_name)
            } else {
                ScheduleSpec::Cron(trimmed.to_owned())
            }
        }
        other => other,
    };
    resolved.validate()?;
    Ok(resolved)
}

/// Preview the next upcoming run timestamps for a schedule spec or phrase.
#[utoipa::path(post, path = "/v1/schedules/preview", tag = "schedules",
    security(("bearer" = [])),
    request_body = PreviewSchedule,
    responses((status = 200, body = ParsedSchedule),
              (status = 422, body = ErrorBody, description = "invalid schedule spec or phrase")))]
pub(crate) async fn preview_schedule(
    ApiJson(input): ApiJson<PreviewSchedule>,
) -> Result<Json<ParsedSchedule>, ApiError> {
    let anchor = input.from_ms.unwrap_or_else(now_ms);
    let count = input.count.unwrap_or(5).clamp(1, 100);
    let tz = input.tz.as_deref().map(str::trim).filter(|t| !t.is_empty());
    let spec = if let Some(spec_val) = input.spec {
        let parsed: ScheduleSpec = serde_json::from_value(spec_val)
            .map_err(|e| bad_schedule(format!("invalid spec: {e}")))?;
        apply_tz_to_spec(parsed, tz)?
    } else if let Some(text) = input.text {
        let parsed = scheduler::parse_natural_language_with_tz(&text, anchor, tz)
            .ok_or_else(|| bad_schedule(format!("cannot understand {text:?}")))?;
        parsed.validate()?;
        parsed
    } else {
        return Err(bad_schedule("provide either spec or text"));
    };
    let next_runs_ms = match &spec {
        ScheduleSpec::At { unix_ms } => {
            if *unix_ms > anchor {
                vec![*unix_ms]
            } else {
                Vec::new()
            }
        }
        other => other.next_occurrences(anchor, count)?,
    };
    Ok(Json(ParsedSchedule {
        spec: serde_json::to_value(&spec).unwrap_or(Value::Null),
        next_run_at_ms: next_runs_ms.first().copied(),
        next_runs_ms,
    }))
}

/// Run and occurrence history for a schedule (newest first).
#[utoipa::path(get, path = "/v1/schedules/{id}/history", tag = "schedules",
    security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "Schedule id")),
    responses((status = 200, body = Vec<ScheduleHistoryEntryDto>),
              (status = 404, body = ErrorBody)))]
pub(crate) async fn list_schedule_history(
    State(s): State<AppState>,
    Path(sid): Path<String>,
) -> Result<Json<Vec<ScheduleHistoryEntryDto>>, ApiError> {
    let sc = schedule_of_user(&s, &sid)?;
    let occurrences = s.schedule_store()?.list_occurrences(sc.id, 100)?;
    let mut runs_by_id: std::collections::HashMap<Uuid, storage::Run> = s
        .storage()
        .list_runs(s.user_id())?
        .into_iter()
        .filter(|r| agent_core::schedule_id_of(&r.kind) == Some(sc.id) && r.parent_run_id.is_none())
        .map(|r| (r.id.0, r))
        .collect();

    let mut entries = Vec::with_capacity(occurrences.len() + runs_by_id.len());
    for occ in occurrences {
        let run = occ
            .run_id
            .as_deref()
            .and_then(|raw| Uuid::parse_str(raw).ok())
            .and_then(|uid| runs_by_id.remove(&uid));
        let run_id = run
            .as_ref()
            .map(|r| r.id.0)
            .or_else(|| occ.run_id.as_deref().and_then(|r| Uuid::parse_str(r).ok()));
        let live_run = run.as_ref().is_some_and(|r| {
            matches!(
                r.state.as_str(),
                "queued" | "running" | "waiting_approval" | "paused"
            )
        });
        let status = match occ.state {
            scheduler::OccurrenceState::Pending
            | scheduler::OccurrenceState::Dispatched
            | scheduler::OccurrenceState::Started => run
                .as_ref()
                .map_or_else(|| occ.state.as_str().to_owned(), |r| r.state.clone()),
            // An interrupted or failed row whose run is still alive (or paused
            // for approval) shows the run's real state, with the run link.
            _ if live_run => run
                .as_ref()
                .map_or_else(|| occ.state.as_str().to_owned(), |r| r.state.clone()),
            other => other.as_str().to_owned(),
        };
        let attempt_state = crate::schedule::attempt_run_of(
            s.storage(),
            s.user_id(),
            sc.id,
            occ.due_ms,
            occ.attempt,
        );
        // The durable run key can say "done" when the row never got the outcome
        // (a crash right after the side effect): show it as succeeded, not interrupted.
        let status = if matches!(
            occ.state,
            scheduler::OccurrenceState::Interrupted | scheduler::OccurrenceState::Missed
        ) && attempt_state == scheduler::AttemptRun::Succeeded
        {
            "succeeded".to_owned()
        } else {
            status
        };
        let retryable = occ.state.is_retryable()
            && sc.enabled
            && occ.retry_count < scheduler::MAX_OCCURRENCE_RETRIES
            && matches!(
                attempt_state,
                scheduler::AttemptRun::None | scheduler::AttemptRun::Failed
            );
        let started_at_ms = run.as_ref().map(|r| r.created_at_ms);
        let finished_at_ms = run.as_ref().and_then(|r| {
            matches!(r.state.as_str(), "succeeded" | "failed" | "cancelled")
                .then_some(r.updated_at_ms)
        });
        entries.push(ScheduleHistoryEntryDto {
            due_ms: Some(occ.due_ms),
            run_id,
            chat_id: run.as_ref().and_then(|r| r.chat_id.map(|c| c.0)),
            status,
            started_at_ms,
            finished_at_ms,
            error: run.and_then(|r| r.error),
            retryable,
        });
    }
    for (_, r) in runs_by_id {
        let finished_at_ms = matches!(r.state.as_str(), "succeeded" | "failed" | "cancelled")
            .then_some(r.updated_at_ms);
        entries.push(ScheduleHistoryEntryDto {
            due_ms: None,
            run_id: Some(r.id.0),
            chat_id: r.chat_id.map(|c| c.0),
            status: r.state,
            started_at_ms: Some(r.created_at_ms),
            finished_at_ms,
            error: r.error,
            retryable: false,
        });
    }
    entries.sort_by(|a, b| {
        let ak = a.started_at_ms.or(a.due_ms).unwrap_or(0);
        let bk = b.started_at_ms.or(b.due_ms).unwrap_or(0);
        bk.cmp(&ak).then_with(|| b.due_ms.cmp(&a.due_ms))
    });
    entries.truncate(100);
    Ok(Json(entries))
}

/// Explicitly retry a `failed`, `missed`, or `interrupted` occurrence of a schedule.
/// Creates a normal run under the same permissions and approvals as a scheduled run
/// without altering `next_run_at`. Idempotent: concurrent or repeat retries never double-fire.
#[utoipa::path(post, path = "/v1/schedules/{id}/occurrences/{due_ms}/retry", tag = "schedules",
    security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "Schedule id"),
           ("due_ms" = i64, Path, description = "Occurrence due timestamp in Unix ms")),
    responses((status = 202, body = AcceptedResponse),
              (status = 404, body = ErrorBody),
              (status = 409, body = ErrorBody, description = "occurrence already handled, its run is still active, the schedule is disabled, or the retry limit is used up"),
              (status = 429, body = ErrorBody, description = "retried again within 30 seconds"),
              (status = 503, body = ErrorBody, description = "the scheduler is not running")))]
pub(crate) async fn retry_schedule_occurrence(
    State(s): State<AppState>,
    Path((sid, due_ms)): Path<(String, i64)>,
) -> Result<(StatusCode, Json<AcceptedResponse>), ApiError> {
    let sc = schedule_of_user(&s, &sid)?;
    if s.schedule_store()?.frozen_for(sc.id)?.is_some() {
        return Err(scheduler::SchedulerError::Frozen(sc.id.to_string()).into());
    }
    let scheduler = s.scheduler()?;
    s.leader()
        .with_active(|| scheduler.retry_occurrence(sc.id, due_ms))??;
    s.0.storage.append_audit(
        s.user_id(),
        None,
        "user",
        "schedule.retry_occurrence",
        &serde_json::json!({"schedule_id": sc.id.to_string(), "due_ms": due_ms}),
    )?;
    Ok((
        StatusCode::ACCEPTED,
        Json(AcceptedResponse {
            status: "accepted".to_owned(),
            schedule_id: sc.id.0,
        }),
    ))
}

/// Fire a schedule from outside.  No bearer token: the body is authenticated
/// with `X-Signature-256: sha256=<hex HMAC-SHA256(secret, body)>` (the secret
/// is the schedule's `webhook_secret`).  Optional replay protection:
/// `X-Webhook-Timestamp` (Unix ms; then the HMAC must cover `"{timestamp}.{body}"`
/// and the timestamp must be within 5 minutes) and `X-Delivery-Id` /
/// `X-GitHub-Delivery` (informational).  A signed request is accepted once per
/// 5-minute window whether or not those headers are sent, so the same body fired
/// twice in that time needs a timestamp (fresh signature) or a different body.  Host / Origin checks still
/// apply and the body may be at most 256 KB.  The body replaces `{input}` in the payload.
/// `202` once accepted (the run goes on in the background).
#[utoipa::path(post, path = "/v1/webhooks/{id}", tag = "schedules",
    params(("id" = Uuid, Path, description = "Schedule id"),
        ("X-Signature-256" = String, Header, description = "`sha256=<hex>` HMAC-SHA256 of the raw body")),
    request_body(content = String, content_type = "text/plain"),
    responses((status = 202, body = AcceptedResponse),
              (status = 401, body = ErrorBody, description = "missing or wrong signature"),
              (status = 404, body = ErrorBody, description = "no such schedule or no webhook trigger"),
              (status = 409, body = ErrorBody, description = "disabled, or a run of it is in flight"),
              (status = 413, description = "body larger than 256 KB"),
              (status = 503, body = ErrorBody, description = "the scheduler is not running")))]
pub(crate) async fn webhook(
    State(s): State<AppState>,
    Path(sid): Path<String>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> Result<(StatusCode, Json<AcceptedResponse>), ApiError> {
    let sid = TaskId(id(&sid)?);
    let signature = headers
        .get("x-signature-256")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let text = std::str::from_utf8(&body)
        .map_err(|_| ApiError::bad_request("the body must be UTF-8 text"))?;
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|v| !v.is_empty())
    };
    let meta = scheduler::WebhookMeta {
        delivery_id: header("x-delivery-id")
            .or_else(|| header("x-github-delivery"))
            .map(str::to_owned),
        timestamp_ms: header("x-webhook-timestamp").and_then(|v| v.parse().ok()),
    };
    // A present but unparsable timestamp must not silently downgrade to body-only auth.
    if header("x-webhook-timestamp").is_some() && meta.timestamp_ms.is_none() {
        return Err(ApiError::bad_request(
            "X-Webhook-Timestamp must be Unix milliseconds",
        ));
    }
    let scheduler = s.scheduler()?;
    s.leader()
        .with_active(|| scheduler.trigger_webhook_with_meta(sid, text, signature, &meta))??;
    Ok((
        StatusCode::ACCEPTED,
        Json(AcceptedResponse {
            status: "accepted".to_owned(),
            schedule_id: sid.0,
        }),
    ))
}

// -------------------------------------------------------------- channels

/// Messaging channels hosted by the daemon (Telegram).
#[utoipa::path(get, path = "/v1/channels", tag = "channels", security(("bearer" = [])),
    responses((status = 200, body = Vec<crate::dto::ChannelDto>)))]
pub(crate) async fn list_channels(State(s): State<AppState>) -> Json<Vec<crate::dto::ChannelDto>> {
    Json(s.channels().iter().map(|c| c.info()).collect())
}

/// Create a one-time pairing code for the Telegram channel (valid 10 minutes;
/// the user sends `/pair <code>` to the bot).
#[utoipa::path(post, path = "/v1/channels/telegram/pair-code", tag = "channels",
    security(("bearer" = [])),
    responses((status = 200, body = crate::dto::PairCodeDto), (status = 404, body = ErrorBody),
              (status = 409, body = ErrorBody, description = "the channel is not running"),
              (status = 503, body = ErrorBody, description = "the daemon is still starting")))]
pub(crate) async fn telegram_pair_code(
    State(s): State<AppState>,
) -> Result<Json<crate::dto::PairCodeDto>, ApiError> {
    let channel = s
        .channels()
        .into_iter()
        .find(|c| c.info().id == "telegram")
        .ok_or_else(|| {
            if s.leader().is_starting() {
                ApiError::starting()
            } else {
                ApiError::not_found("channel telegram")
            }
        })?;
    if s.leader().is_starting() {
        return Err(ApiError::starting());
    }
    channel
        .create_pair_code()
        .map(Json)
        .map_err(ApiError::conflict)
}

#[cfg(test)]
mod schedule_update_tests {
    use std::sync::{Arc, Barrier};

    use super::*;

    fn schedule(spec: ScheduleSpec) -> Schedule {
        Schedule::new(
            BotId::new(),
            None,
            "before",
            spec,
            Payload::AgentPrompt {
                text: "before".into(),
            },
            MisfirePolicy::RunOnce,
            0,
        )
        .unwrap()
    }

    #[test]
    fn route_edit_preserves_firing_between_read_and_write() {
        let store = Arc::new(scheduler::ScheduleStore::open_in_memory().unwrap());
        let original = schedule(ScheduleSpec::Every { seconds: 60 });
        store.insert(&original).unwrap();
        // Exactly the snapshot read by update_schedule before validation.
        let mut stale = store.get(original.id).unwrap().unwrap();
        stale.name = "after".into();
        stale.payload = Payload::AgentPrompt {
            text: "after".into(),
        };
        let read = Barrier::new(2);
        let fired = Barrier::new(2);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                read.wait();
                store
                    .record_run_intent(original.id, 60_000, Some(120_000))
                    .unwrap();
                fired.wait();
            });
            read.wait();
            fired.wait();
            let (fresh, _) = persist_schedule_edit(&store, &stale, None, 60_000, None).unwrap();
            assert_eq!(fresh.name, "after");
            assert_eq!(fresh.payload, stale.payload);
            assert_eq!(fresh.last_run_at, Some(60_000));
            assert_eq!(fresh.next_run_at, Some(120_000));
            assert!(
                store.list_due(60_000).unwrap().is_empty(),
                "must not re-fire"
            );
            let dto = serde_json::to_value(schedule_dto(&fresh, false)).unwrap();
            assert!(dto.get("revision").is_none());
        });
    }

    #[test]
    fn route_title_edit_does_not_rearm_consumed_one_shot() {
        let store = scheduler::ScheduleStore::open_in_memory().unwrap();
        let mut stale = schedule(ScheduleSpec::At { unix_ms: 1_000 });
        store.insert(&stale).unwrap();
        store.record_run_intent(stale.id, 1_000, None).unwrap();
        stale.name = "renamed".into();
        let (fresh, _) = persist_schedule_edit(&store, &stale, None, 2_000, None).unwrap();
        assert_eq!(fresh.last_run_at, Some(1_000));
        assert_eq!(fresh.next_run_at, None);
        assert_eq!(store.list_due(2_000).unwrap(), vec![]);
    }

    #[test]
    fn route_edit_uses_current_enabled_state_and_does_not_resurrect_delete() {
        let store = scheduler::ScheduleStore::open_in_memory().unwrap();
        let stale = schedule(ScheduleSpec::Every { seconds: 60 });
        store.insert(&stale).unwrap();
        store.set_enabled(stale.id, false, None).unwrap();
        let (fresh, was_enabled) = persist_schedule_edit(&store, &stale, None, 10, None).unwrap();
        assert!(!was_enabled);
        assert!(!fresh.enabled);
        assert_eq!(fresh.next_run_at, None);
        store.delete(stale.id).unwrap();
        assert!(persist_schedule_edit(&store, &stale, Some(true), 20, None).is_err());
        assert!(store.get(stale.id).unwrap().is_none());
    }
}
