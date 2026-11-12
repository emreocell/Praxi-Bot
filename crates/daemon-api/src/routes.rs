//! Route handlers (`/v1`).

use std::collections::BTreeMap;

use agent_core::{
    ApprovalScope, BotProfile, Delivery, EffectivePolicy, Grant, GrantTarget, MessageMode,
    PolicyDecision,
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
    AcceptedResponse, AnswerQuestion, AnswerResponse, ApprovalDto, BotDto, BotInput, BotUsage,
    CatalogProviderDto, CategoryDecisionDto, ChatDto, ConfiguredProviderDto, ControlDecisionInput,
    ControlDecisionResponse, ControlStateDto, ControlStateInput, CreateChat, DecideApproval,
    Decision, DecisionResponse, DeliveryKind, DetectedProviderDto, EffectivePolicyDto, GrantDto,
    GrantTargetDto, Health, LatestCursor, MessageDto, Mode, ParseSchedule, ParsedSchedule,
    PolicyDto, PolicyInput, ProvidersDto, ReloadResponse, RememberResponse, RevokeGrant, RunDto,
    ScheduleChangeKind, ScheduleDto, ScheduleInput, Scope, SendMessage, SendResponse, UsageSummary,
    UsageTotals,
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

fn id(s: &str) -> Result<Uuid, ApiError> {
    s.parse().map_err(|_| ApiError::bad_request("malformed id"))
}

fn bot_dto(b: Bot) -> BotDto {
    BotDto {
        id: b.id.0,
        name: b.name,
        config: b.config,
        created_at_ms: b.created_at_ms,
        effective_policy: None,
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
        target: match &g.target {
            GrantTarget::Tool(name) => GrantTargetDto::Tool { name: name.clone() },
            GrantTarget::Category(c) => GrantTargetDto::Category {
                name: category_name(*c),
            },
        },
        granted_at_ms: g.granted_at_ms,
    }
}

fn grant_target(t: GrantTargetDto) -> Result<GrantTarget, ApiError> {
    match t {
        GrantTargetDto::Tool { name } if !name.trim().is_empty() => Ok(GrantTarget::Tool(name)),
        GrantTargetDto::Tool { .. } => Err(ApiError::bad_request("tool name must not be empty")),
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
                    },
                )
            })
            .collect(),
        grants: e.grants.iter().map(grant_dto).collect(),
    }
}

fn chat_dto(c: Chat) -> ChatDto {
    ChatDto {
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
                decision: (!waiting).then(|| "expired".to_owned()),
                requested_at_ms: ms(env.ts_ms),
                decided_at_ms: None,
            }),
            Event::ToolCallApproved(d) => decide(&mut out, d.call_id, "approved", ms(env.ts_ms)),
            Event::ToolCallDenied(d) => decide(&mut out, d.call_id, "denied", ms(env.ts_ms)),
            _ => {}
        }
    }
    // Only the newest approval of a waiting run can still be pending.
    if waiting {
        let n = out.len();
        for (i, a) in out.iter_mut().enumerate() {
            if a.decision.is_none() && i + 1 != n {
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

/// Validate a bot definition; returns `(name, config json)`.
fn resolve_bot(input: BotInput) -> Result<(String, Value), ApiError> {
    let config = match (input.config, input.profile_toml) {
        (Some(c), None) => c,
        (None, Some(t)) => toml::from_str::<Value>(&t)
            .map_err(|e| ApiError::bad_request(format!("profile toml: {e}")))?,
        _ => {
            return Err(ApiError::bad_request(
                "exactly one of `config` or `profile_toml` is required",
            ));
        }
    };
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
    Json(Health {
        status: if s.agent().providers().setup_required() {
            "setup_required"
        } else {
            "ok"
        }
        .into(),
        version: env!("CARGO_PKG_VERSION").into(),
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
            .list_bots(s.user_id())?
            .into_iter()
            .map(bot_dto)
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
    ApiJson(input): ApiJson<BotInput>,
) -> Result<(StatusCode, Json<BotDto>), ApiError> {
    let (name, config) = resolve_bot(input)?;
    let bot = Bot {
        id: BotId::new(),
        user_id: s.user_id(),
        name,
        config,
        created_at_ms: now_ms(),
    };
    s.storage().insert_bot(&bot)?;
    s.refresh_profile(&bot);
    Ok((StatusCode::CREATED, Json(bot_dto(bot))))
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
    let bot = s.bot_of_user(BotId(id(&bid)?))?;
    let effective = s.agent().effective_policy(bot.id).ok();
    let mut dto = bot_dto(bot);
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

/// Replace a bot's name and profile.
#[utoipa::path(put, path = "/v1/bots/{id}", tag = "bots", security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "Bot id")),
    request_body = BotInput,
    responses((status = 200, body = BotDto), (status = 404, body = ErrorBody),
              (status = 422, body = ErrorBody)))]
pub(crate) async fn update_bot(
    State(s): State<AppState>,
    Path(bid): Path<String>,
    ApiJson(input): ApiJson<BotInput>,
) -> Result<Json<BotDto>, ApiError> {
    let mut bot = s.bot_of_user(BotId(id(&bid)?))?;
    let (name, config) = resolve_bot(input)?;
    s.storage().update_bot(bot.id, &name, &config)?;
    bot.name = name;
    bot.config = config;
    s.refresh_profile(&bot);
    Ok(Json(bot_dto(bot)))
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
    s.storage()
        .delete_bot(bot.id)
        .map_err(|_| ApiError::conflict("bot still has chats or runs"))?;
    // The bot's id must not linger in any MCP server's `bots` list.  Ids are
    // never reused, so a failure here cannot widen access; it is retried by
    // the next edit of the list.
    if let Some(mcp) = s.mcp() {
        let _ = mcp.forget_bot(&bot.id.to_string()).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

// ----------------------------------------------------------------- chats

/// List chats.
#[utoipa::path(get, path = "/v1/chats", tag = "chats", security(("bearer" = [])),
    responses((status = 200, body = Vec<ChatDto>)))]
pub(crate) async fn list_chats(State(s): State<AppState>) -> Result<Json<Vec<ChatDto>>, ApiError> {
    Ok(Json(
        s.storage()
            .list_chats(s.user_id())?
            .into_iter()
            .map(chat_dto)
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
    Ok((StatusCode::CREATED, Json(chat_dto(chat))))
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
    Ok(Json(
        s.storage()
            .list_messages(chat.id)?
            .into_iter()
            .map(|m| MessageDto {
                id: m.id,
                chat_id: m.chat_id.0,
                run_id: m.run_id.map(|r| r.0),
                role: m.role,
                content: m.content,
                created_at_ms: m.created_at_ms,
            })
            .collect(),
    ))
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
    Path(cid): Path<String>,
    ApiJson(input): ApiJson<SendMessage>,
) -> Result<(StatusCode, Json<SendResponse>), ApiError> {
    let chat = s.chat_of_user(ChatId(id(&cid)?))?;
    if input.text.trim().is_empty() {
        return Err(ApiError::bad_request("text must not be empty"));
    }
    let mode = match input.mode {
        Mode::Steer => MessageMode::Steer,
        Mode::Queue => MessageMode::Queue,
    };
    let (delivery, run_id) = match s.agent().send_user_message(chat.id, &input.text, mode)? {
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
    Path(call): Path<String>,
    ApiJson(input): ApiJson<DecideApproval>,
) -> Result<Json<DecisionResponse>, ApiError> {
    let call_uuid = id(&call)?;
    let call_id = ToolCallId(call_uuid);
    match input.decision {
        Decision::Approve => {
            let scope = match input.scope {
                Scope::Once => ApprovalScope::Once,
                Scope::ThisChat => ApprovalScope::ThisChat,
                Scope::Always => ApprovalScope::Always,
                Scope::AlwaysCategory => ApprovalScope::AlwaysCategory,
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
    s.agent().cancel(run.id)?;
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

/// Query of `GET /v1/usage`.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct UsageQuery {
    /// Only runs created at or after this time.
    pub since_ms: Option<i64>,
}

fn add(t: &mut UsageTotals, u: &Usage, cost: Option<f64>) {
    if let Some(c) = cost {
        *t.cost_usd.get_or_insert(0.0) += c;
    }
    t.prompt_tokens += u.prompt_tokens;
    t.completion_tokens += u.completion_tokens;
    t.cached_tokens += u.cached_tokens.unwrap_or(0);
    t.model_calls += 1;
}

/// Token usage summed over the model steps of all runs.
#[utoipa::path(get, path = "/v1/usage", tag = "usage", security(("bearer" = [])),
    params(UsageQuery),
    responses((status = 200, body = UsageSummary)))]
pub(crate) async fn usage(
    State(s): State<AppState>,
    Query(q): Query<UsageQuery>,
) -> Result<Json<UsageSummary>, ApiError> {
    let st = s.storage();
    let mut total = UsageTotals::default();
    let mut by_bot: BTreeMap<Uuid, UsageTotals> = BTreeMap::new();
    let mut runs = 0u64;
    for run in st.list_runs(s.user_id())? {
        if q.since_ms.is_some_and(|t| run.created_at_ms < t) {
            continue;
        }
        runs += 1;
        for step in st.list_steps(run.id)? {
            let Some(u) = (step.name == "model")
                .then_some(step.output.as_ref())
                .flatten()
                .and_then(|o| o.get("usage"))
                .and_then(|u| serde_json::from_value::<Usage>(u.clone()).ok())
            else {
                continue;
            };
            let cost = step
                .output
                .as_ref()
                .and_then(|o| o.get("model"))
                .and_then(Value::as_str)
                .and_then(|m| s.agent().price_for(m))
                .map(|p| p.cost(u.prompt_tokens, u.completion_tokens, u.cached_tokens));
            add(&mut total, &u, cost);
            add(by_bot.entry(run.bot_id.0).or_default(), &u, cost);
        }
    }
    Ok(Json(UsageSummary {
        runs,
        total,
        by_bot: by_bot
            .into_iter()
            .map(|(bot_id, totals)| BotUsage { bot_id, totals })
            .collect(),
    }))
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
        s.hub().try_release().map_err(ApiError::conflict)?;
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
    responses((status = 200, body = ControlStateDto), (status = 400, body = ErrorBody)))]
pub(crate) async fn put_control_state(
    State(s): State<AppState>,
    axum::Extension(actor): axum::Extension<crate::live_view::Actor>,
    ApiJson(input): ApiJson<ControlStateInput>,
) -> Result<Json<ControlStateDto>, ApiError> {
    // A peer may report a mode or set a first pause, but releasing or changing
    // a latch it does not own needs the local user or an input grant.
    let changes_latch = input.release
        || (matches!(
            input.mode,
            crate::dto::ControlMode::Paused | crate::dto::ControlMode::HumanDriving
        ) && s.hub().latched().is_some_and(|l| l != input.mode));
    if changes_latch {
        crate::live_view::require_latch_authority(&s, &actor)?;
    }
    // The local app's Pause/Stop must work during anybody's takeover: it
    // overrides the takeover and keeps the pause. Paired peers cannot.
    if actor.local
        && !input.release
        && input.mode == crate::dto::ControlMode::Paused
        && s.hub().takeover().is_some()
    {
        crate::live_view::override_local_pause(&s, &actor).await?;
        return Ok(Json(control_state_dto(&s, Some(&actor))));
    }
    s.hub()
        .try_apply_state(input.mode, input.release)
        .map_err(ApiError::conflict)?;
    Ok(Json(control_state_dto(&s, Some(&actor))))
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
    Path(rid): Path<String>,
    ApiJson(input): ApiJson<ControlDecisionInput>,
) -> Result<Json<ControlDecisionResponse>, ApiError> {
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
    responses((status = 200, body = ProvidersDto)))]
pub(crate) async fn list_providers(
    State(s): State<AppState>,
) -> Result<Json<ProvidersDto>, ApiError> {
    let cfg = provider_catalog::ProviderConfig::load(&provider_catalog::config_path(&s.data_dir()))
        .map_err(|e| ApiError::internal(format!("providers.toml: {e}")))?;
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
        .map(|(p, (models, models_error))| ConfiguredProviderDto {
            id: p.id.clone(),
            enabled: p.enabled,
            is_default: default_id.as_deref() == Some(p.id.as_str()),
            default_model: p.default_model.clone(),
            has_key: p.key.is_some(),
            custom: p.custom.is_some(),
            active: active.contains(&p.id),
            models,
            models_error,
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

/// Detect installed, signed-in coding CLIs.
#[utoipa::path(get, path = "/v1/providers/detected", tag = "providers", security(("bearer" = [])),
    params(DetectProvidersQuery),
    responses((status = 200, body = Vec<DetectedProviderDto>)))]
pub(crate) async fn detect_providers(
    State(s): State<AppState>,
    Query(query): Query<DetectProvidersQuery>,
) -> Result<Json<Vec<DetectedProviderDto>>, ApiError> {
    let refresh = query
        .refresh
        .as_deref()
        .is_some_and(|r| r == "1" || r.eq_ignore_ascii_case("true"));
    let detected = s.detector().detect(refresh).await;
    Ok(Json(detected.into_iter().map(Into::into).collect()))
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
    Ok(Json(out))
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
    }
}

fn dto_of(s: &AppState, sc: &Schedule) -> Result<ScheduleDto, ApiError> {
    let has = s.schedule_store()?.get_trigger(sc.id)?.is_some();
    Ok(schedule_dto(sc, has))
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
    ApiJson(input): ApiJson<ScheduleInput>,
) -> Result<(StatusCode, Json<ScheduleDto>), ApiError> {
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
    let store = s.schedule_store()?;
    store.insert(&sc)?;
    if let Err(e) = apply_webhook(&store, sc.id, input.webhook_secret.as_deref()) {
        let _ = store.delete(sc.id);
        return Err(e);
    }
    s.publish_schedule_changed(sc.id.0, ScheduleChangeKind::Created);
    Ok((StatusCode::CREATED, Json(dto_of(&s, &sc)?)))
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
    ApiJson(input): ApiJson<ScheduleInput>,
) -> Result<Json<ScheduleDto>, ApiError> {
    let mut sc = schedule_of_user(&s, &sid)?;
    if input.bot_id != sc.bot_id.0 {
        return Err(bad_schedule("the bot of a schedule cannot be changed"));
    }
    let p = schedule_parts(&s, &input)?;
    input.name.trim().clone_into(&mut sc.name);
    sc.chat_id = p.chat_id;
    sc.payload = p.payload;
    sc.misfire_policy = p.policy;
    sc.spec = p.spec;
    let store = s.schedule_store()?;
    // Run progress may have changed after schedule_of_user. The store reads
    // it again and computes any re-arm under the same write transaction.
    let (sc, was_enabled) = persist_schedule_edit(&store, &sc, input.enabled, now_ms())?;
    apply_webhook(&store, sc.id, input.webhook_secret.as_deref())?;
    s.publish_schedule_changed(
        sc.id.0,
        if sc.enabled == was_enabled {
            ScheduleChangeKind::Updated
        } else {
            ScheduleChangeKind::Toggled
        },
    );
    Ok(Json(dto_of(&s, &sc)?))
}

/// Persist the PUT definition without trusting the route's runtime snapshot.
/// Keeping this boundary separate also allows deterministic read/write race
/// tests without a process-global hook or a real clock/sleep.
fn persist_schedule_edit(
    store: &scheduler::ScheduleStore,
    definition: &Schedule,
    enabled: Option<bool>,
    now: i64,
) -> Result<(Schedule, bool), ApiError> {
    store
        .update_definition_and_rearm(definition, enabled, now)?
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
    store.remove_trigger(sc.id)?;
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
    s.scheduler()?.run_now(sc.id)?;
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
    let next = match &spec {
        ScheduleSpec::At { unix_ms } => Some(*unix_ms),
        other => other.next_after(now)?,
    };
    Ok(Json(ParsedSchedule {
        spec: serde_json::to_value(&spec).unwrap_or(Value::Null),
        next_run_at_ms: next,
    }))
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
    s.scheduler()?
        .trigger_webhook_with_meta(sid, text, signature, &meta)?;
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
              (status = 409, body = ErrorBody, description = "the channel is not running")))]
pub(crate) async fn telegram_pair_code(
    State(s): State<AppState>,
) -> Result<Json<crate::dto::PairCodeDto>, ApiError> {
    let channel = s
        .channels()
        .into_iter()
        .find(|c| c.info().id == "telegram")
        .ok_or_else(|| ApiError::not_found("channel telegram"))?;
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
            let (fresh, _) = persist_schedule_edit(&store, &stale, None, 60_000).unwrap();
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
        let (fresh, _) = persist_schedule_edit(&store, &stale, None, 2_000).unwrap();
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
        let (fresh, was_enabled) = persist_schedule_edit(&store, &stale, None, 10).unwrap();
        assert!(!was_enabled);
        assert!(!fresh.enabled);
        assert_eq!(fresh.next_run_at, None);
        store.delete(stale.id).unwrap();
        assert!(persist_schedule_edit(&store, &stale, Some(true), 20).is_err());
        assert!(store.get(stale.id).unwrap().is_none());
    }
}
