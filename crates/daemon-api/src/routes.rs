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
use protocol::{BotId, ChatId, Event, PermissionCategory, RunId, ToolCallId};
use provider_catalog::Usage;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use storage::{Bot, Chat, now_ms};
use task_runtime::RunState;
use utoipa::IntoParams;
use uuid::Uuid;

use crate::AppState;
use crate::dto::{
    AnswerQuestion, AnswerResponse, ApprovalDto, BotDto, BotInput, BotUsage, CatalogProviderDto,
    CategoryDecisionDto, ChatDto, ConfiguredProviderDto, ControlDecisionInput,
    ControlDecisionResponse, ControlStateDto, ControlStateInput, CreateChat, DecideApproval,
    Decision, DecisionResponse, DeliveryKind, EffectivePolicyDto, GrantDto, GrantTargetDto, Health,
    MessageDto, Mode, PolicyDto, PolicyInput, ProvidersDto, ReloadResponse, RevokeGrant, RunDto,
    Scope, SendMessage, SendResponse, UsageSummary, UsageTotals,
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
                    PolicyDecision::Ask => "ask",
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
        bot_id: c.bot_id.0,
        title: c.title,
        created_at_ms: c.created_at_ms,
    }
}

fn run_dto(r: storage::Run) -> RunDto {
    RunDto {
        id: r.id.0,
        bot_id: r.bot_id.0,
        chat_id: r.chat_id.map(|c| c.0),
        kind: r.kind,
        state: r.state,
        error: r.error,
        created_at_ms: r.created_at_ms,
        updated_at_ms: r.updated_at_ms,
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
        status: "ok".into(),
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
    Ok(Json(
        s.storage()
            .list_runs(s.user_id())?
            .into_iter()
            .map(run_dto)
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
    Ok(Json(run_dto(s.run_of_user(RunId(id(&rid)?))?)))
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
    Ok(Json(run_dto(s.run_of_user(run.id)?)))
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
    Ok((StatusCode::ACCEPTED, Json(run_dto(s.run_of_user(run.id)?))))
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

fn control_state_dto(s: &AppState) -> ControlStateDto {
    let reg = s.hub().registration();
    ControlStateDto {
        mode: reg.map(|(m, _)| m),
        expires_in_ms: reg.map_or(0, |(_, left)| {
            u64::try_from(left.as_millis()).unwrap_or(u64::MAX)
        }),
        app_connected: s.hub().app_connected(),
    }
}

/// The daemon's view of the desktop app's controller registration.
#[utoipa::path(get, path = "/v1/control/state", tag = "control", security(("bearer" = [])),
    responses((status = 200, body = ControlStateDto)))]
pub(crate) async fn get_control_state(State(s): State<AppState>) -> Json<ControlStateDto> {
    Json(control_state_dto(&s))
}

/// Register the desktop app as controller and report its mode (heartbeat: send
/// it on every state change and at least every 5 s; it expires after 15 s).
/// While the mode is `human_driving` or `paused`, desktop actions of bots are
/// refused immediately.
#[utoipa::path(put, path = "/v1/control/state", tag = "control", security(("bearer" = [])),
    request_body = ControlStateInput,
    responses((status = 200, body = ControlStateDto), (status = 400, body = ErrorBody)))]
pub(crate) async fn put_control_state(
    State(s): State<AppState>,
    ApiJson(input): ApiJson<ControlStateInput>,
) -> Json<ControlStateDto> {
    s.hub().put_state(input.mode);
    Json(control_state_dto(&s))
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

/// File the global policy is persisted in (`<data dir>/policy.json`).
pub(crate) const POLICY_FILE: &str = "policy.json";

/// The persisted global policy, if one was saved and still parses.
pub(crate) fn load_policy(data_dir: &std::path::Path) -> Option<Policy> {
    let text = std::fs::read_to_string(data_dir.join(POLICY_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

fn save_policy(data_dir: &std::path::Path, policy: &Policy) -> std::io::Result<()> {
    let path = data_dir.join(POLICY_FILE);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(policy)?)?;
    std::fs::rename(&tmp, &path)
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
    Json(policy_dto(&s.agent().global_policy()))
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
    Ok(Json(policy_dto(&policy)))
}

// ------------------------------------------------------------- providers

fn kind_name(k: &provider_catalog::ProviderKind) -> String {
    use provider_catalog::ProviderKind;
    match k {
        ProviderKind::OpenAiCompatible => "openai_compatible".to_owned(),
        ProviderKind::Anthropic => "anthropic".to_owned(),
        ProviderKind::Gemini => "gemini".to_owned(),
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
    let active = live.ids();
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
    catalog.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(Json(ProvidersDto {
        default_provider: Some(live.default_id().to_owned()),
        active,
        configured,
        catalog,
    }))
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
        default_provider: Some(providers.default_id().to_owned()),
        active: providers.ids(),
    };
    s.agent().set_providers(providers);
    Ok(Json(out))
}
