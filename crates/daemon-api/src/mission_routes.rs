//! Missions (`/v1/missions`): long-running jobs that the bot works through alone.
//!
//! The ledger, the brief and the supervisor live in the `missions` crate and the session
//! runner in `mission_host`. This module is the HTTP surface: list and read a mission with its
//! progress (done / blocked / left), start one, pause, resume or stop it, and answer all the
//! blocked items in one request. Everything that starts or steers unattended work is
//! owner-only (see `owner.rs`).

use std::collections::HashSet;

use axum::http::HeaderMap;
use axum::{
    Json,
    extract::{Path as AxumPath, Query, Request, State},
    middleware::Next,
    response::{IntoResponse, Response},
};
use missions::{Budgets, ItemStatus, Mission, MissionError, MissionState, NewMission};
use protocol::{BotId, ChatId, RunId};
use serde::{Deserialize, Serialize};
use serde_json::json;
use storage::{Chat, Message, now_ms};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::{ApiError, AppState, error::ErrorBody, routes::ApiJson};

/// Longest goal the owner may give, in characters.
const MAX_GOAL_CHARS: usize = 4_000;

/// Counts of a mission's ledger items. `done`, `blocked` and `left` are the three headline
/// numbers and do not overlap: `left` is what the bot still has to work on (`todo + doing`).
#[derive(Debug, Serialize, ToSchema)]
pub struct MissionProgressDto {
    /// All items.
    pub total: u32,
    /// Finished successfully.
    pub done: u32,
    /// Parked for the owner (CAPTCHA, 2FA, a question).
    pub blocked: u32,
    /// Parked on a wake: a timer, a mail or a webhook (`mission_wait`).
    pub waiting: u32,
    /// Not started.
    pub todo: u32,
    /// Being worked on right now.
    pub doing: u32,
    /// Given up (the site refuses, needs payment, ...).
    pub failed: u32,
    /// Left out on purpose.
    pub skipped: u32,
    /// Still for the bot to work on: `todo + doing` (blocked and waiting items are counted in
    /// `blocked` and `waiting`).
    pub left: u32,
}

/// The limits the supervisor enforces before every session.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[allow(clippy::struct_field_names)] // `max_*` is the wire name of every limit
pub struct MissionBudgetsDto {
    /// Most sessions (agent runs) over the mission's life.
    pub max_sessions: u32,
    /// Most model calls over all sessions.
    pub max_model_steps: u32,
    /// Wall clock from start to deadline, in minutes.
    pub max_minutes: u64,
    /// Model spend cap in USD; `null` = no mission cap (bot limits still apply).
    pub max_cost_usd: Option<f64>,
    /// Sessions in a row without progress before the mission blocks.
    pub max_stalled_sessions: u32,
    /// Failed sessions in a row before the mission blocks.
    pub max_failed_sessions: u32,
}

impl From<&Budgets> for MissionBudgetsDto {
    fn from(b: &Budgets) -> Self {
        Self {
            max_sessions: b.max_sessions,
            max_model_steps: b.max_model_steps,
            max_minutes: b.max_minutes,
            max_cost_usd: b.max_cost_usd,
            max_stalled_sessions: b.max_stalled_sessions,
            max_failed_sessions: b.max_failed_sessions,
        }
    }
}

impl From<MissionBudgetsDto> for Budgets {
    fn from(b: MissionBudgetsDto) -> Self {
        Self {
            max_sessions: b.max_sessions,
            max_model_steps: b.max_model_steps,
            max_minutes: b.max_minutes,
            max_cost_usd: b.max_cost_usd,
            max_stalled_sessions: b.max_stalled_sessions,
            max_failed_sessions: b.max_failed_sessions,
        }
    }
}

/// A mission with its headline numbers.
#[derive(Debug, Serialize, ToSchema)]
pub struct MissionDto {
    /// Mission id.
    pub id: Uuid,
    /// The lead bot.
    pub bot_id: Uuid,
    /// The mission's chat (its sessions run there).
    pub chat_id: Uuid,
    /// Short label.
    pub title: String,
    /// The owner's words, verbatim.
    pub goal: String,
    /// `running`, `waiting` (every open item waits for a timer, a mail or a webhook), `paused`,
    /// `blocked`, `done`, `failed` or `cancelled`.
    pub state: String,
    /// Why it is paused, blocked or failed.
    pub state_reason: Option<String>,
    /// Done / blocked / left counts.
    pub progress: MissionProgressDto,
    /// Success criterion: at least this many items done; `null` = every item finished.
    pub target_done: Option<u32>,
    /// Sessions started so far.
    pub sessions: u32,
    /// Model calls over all sessions.
    pub model_steps: u32,
    /// Spend over all sessions (USD, for models with a known price).
    pub cost_usd: f64,
    /// A session is working right now.
    pub session_running: bool,
    /// Limits.
    pub budgets: MissionBudgetsDto,
    /// Start (ms since epoch).
    pub started_at_ms: i64,
    /// Last change (ms since epoch).
    pub updated_at_ms: i64,
    /// End of a finished mission (ms since epoch).
    pub finished_at_ms: Option<i64>,
}

/// One ledger item.
#[derive(Debug, Serialize, ToSchema)]
pub struct MissionItemDto {
    /// Dedupe key (a domain, an address).
    pub key: String,
    /// Label such as `site`, `lead`, `task`.
    pub kind: String,
    /// What to do, one line.
    pub title: String,
    /// `todo`, `doing`, `done`, `blocked`, `waiting`, `failed` or `skipped`.
    pub status: String,
    /// How often a session started on it.
    pub attempts: u32,
    /// What was achieved (evidence), when done.
    pub result: Option<String>,
    /// Last failure.
    pub last_error: Option<String>,
    /// What the owner has to do or answer (blocked items).
    pub blocked_reason: Option<String>,
    /// The step needs the owner live (a 2FA / verification code): a code is never taken as an
    /// answer, never stored and never shown to the bot. Skip the item or finish it yourself.
    pub needs_live_owner: bool,
    /// What the owner answered for a blocked item.
    pub owner_answer: Option<String>,
    /// What a `waiting` item waits for (usually one entry; the first to happen wakes it).
    pub waiting_for: Vec<MissionWaitDto>,
    /// Last change (ms since epoch).
    pub updated_at_ms: i64,
}

/// What a `waiting` item waits for. The mail filter and the webhook name are the bot's own
/// words or the owner's, shown as plain text.
#[derive(Debug, Serialize, ToSchema)]
pub struct MissionWaitDto {
    /// `after` (a timer), `mail` (in the bot's own inbox) or `webhook`.
    pub kind: String,
    /// Mail: the sender's domain.
    pub from_domain: Option<String>,
    /// Mail: text the subject contains.
    pub subject_contains: Option<String>,
    /// Mail: text the subject or body contains.
    pub keyword: Option<String>,
    /// Webhook: its name.
    pub webhook_name: Option<String>,
    /// A timer: when it fires. A mail or webhook wait: when it gives up and the item is
    /// parked for the owner (ms since epoch).
    pub due_at_ms: i64,
    /// Why the bot waits.
    pub reason: String,
}

/// A line of the owner digest.
#[derive(Debug, Serialize, ToSchema)]
pub struct MissionDigestItemDto {
    /// Item key.
    pub key: String,
    /// Item title.
    pub title: String,
    /// Reason, error or result.
    pub text: Option<String>,
}

/// What the owner reads when a mission ends or blocks (built from the ledger by code).
#[derive(Debug, Serialize, ToSchema)]
pub struct MissionDigestDto {
    /// When it was written (ms since epoch).
    pub at_ms: i64,
    /// `done`, `blocked`, `failed` or `cancelled`.
    pub outcome: String,
    /// Why.
    pub reason: Option<String>,
    /// Headline counts.
    pub progress: MissionProgressDto,
    /// Items that need the owner.
    pub blocked: Vec<MissionDigestItemDto>,
    /// Items given up.
    pub failed: Vec<MissionDigestItemDto>,
    /// A few finished items.
    pub done_sample: Vec<MissionDigestItemDto>,
    /// Decisions the bot recorded.
    pub decisions: Vec<String>,
}

/// A mission with its ledger, blocked items and digest.
#[derive(Debug, Serialize, ToSchema)]
pub struct MissionDetailDto {
    /// The mission.
    pub mission: MissionDto,
    /// Every ledger item, in ledger order.
    pub items: Vec<MissionItemDto>,
    /// The items that wait for the owner: answer them all with one `POST .../answers`.
    pub blocked: Vec<MissionItemDto>,
    /// The digest, once the mission has ended or blocked.
    pub digest: Option<MissionDigestDto>,
}

/// `GET /v1/missions` filter.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListMissionsQuery {
    /// Only this bot's missions.
    pub bot_id: Option<Uuid>,
    /// `active` = running, paused or blocked.
    pub state: Option<String>,
}

/// `POST /v1/missions`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateMissionInput {
    /// The lead bot; it has to be on the Full level.
    pub bot_id: Uuid,
    /// The task in the owner's words.
    pub goal: String,
    /// Short label; defaults to the start of the goal.
    #[serde(default)]
    pub title: Option<String>,
    /// Success criterion: at least this many items done. Default: every item finished.
    #[serde(default)]
    pub target_done: Option<u32>,
    /// Limits; defaults apply to what is left out.
    #[serde(default)]
    pub budgets: Option<MissionBudgetsDto>,
}

/// One answer to a blocked item.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MissionAnswerInput {
    /// The blocked item's key.
    pub key: String,
    /// What the owner says (the bot reads it in the next session). Empty with `skip`.
    #[serde(default)]
    pub answer: String,
    /// Leave this item out instead of retrying it.
    #[serde(default)]
    pub skip: bool,
}

/// `POST /v1/missions/{id}/answers`: all blocked items at once.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MissionAnswersInput {
    /// The answers; items not named stay blocked.
    pub answers: Vec<MissionAnswerInput>,
}

/// How many items an answer request released.
#[derive(Debug, Serialize, ToSchema)]
pub struct MissionAnswersDto {
    /// Items that go back to the bot.
    pub released: u32,
    /// Items the owner chose to skip.
    pub skipped: u32,
}

fn uuid_of(s: &str) -> Uuid {
    Uuid::parse_str(s).unwrap_or_default()
}

fn progress_dto(p: missions::Progress) -> MissionProgressDto {
    MissionProgressDto {
        total: p.total,
        done: p.done,
        blocked: p.blocked,
        waiting: p.waiting,
        todo: p.todo,
        doing: p.doing,
        failed: p.failed,
        skipped: p.skipped,
        left: p.open(),
    }
}

fn store_err(e: MissionError) -> ApiError {
    match e {
        MissionError::NotFound(w) => ApiError::not_found(w),
        MissionError::Invalid(m) => ApiError::bad_request(m),
        MissionError::IllegalTransition { .. } | MissionError::Busy(_) => {
            ApiError::conflict(e.to_string())
        }
        other => ApiError::internal(other),
    }
}

fn mission_dto(s: &AppState, m: &Mission) -> Result<MissionDto, ApiError> {
    let store = s.mission_store()?;
    Ok(MissionDto {
        id: uuid_of(&m.id),
        bot_id: uuid_of(&m.bot_id),
        chat_id: uuid_of(&m.chat_id),
        title: m.title.clone(),
        goal: m.goal_text.clone(),
        state: m.state.as_str().to_owned(),
        state_reason: m.state_reason.clone(),
        progress: progress_dto(store.progress(&m.id).map_err(store_err)?),
        target_done: m.target_done,
        sessions: m.sessions,
        model_steps: m.model_steps,
        cost_usd: m.cost_usd,
        session_running: m.lease_run_id.is_some(),
        budgets: MissionBudgetsDto::from(&m.budgets),
        started_at_ms: m.started_at_ms,
        updated_at_ms: m.updated_at_ms,
        finished_at_ms: m.finished_at_ms,
    })
}

fn wait_dto(w: &missions::Wait) -> MissionWaitDto {
    let (from_domain, subject_contains, keyword, webhook_name) = match &w.spec {
        missions::WaitSpec::After => (None, None, None, None),
        missions::WaitSpec::Mail { filter } => (
            filter.from_domain.clone(),
            filter.subject_contains.clone(),
            filter.keyword.clone(),
            None,
        ),
        missions::WaitSpec::Webhook { name, .. } => (None, None, None, Some(name.clone())),
        missions::WaitSpec::Every { .. } | missions::WaitSpec::Child { .. } => {
            (None, None, None, None)
        }
    };
    MissionWaitDto {
        kind: w.spec.kind().to_owned(),
        from_domain,
        subject_contains,
        keyword,
        webhook_name,
        due_at_ms: w.due_at_ms,
        reason: w.reason.clone(),
    }
}

fn item_dto(i: &missions::Item, waits: &[missions::Wait]) -> MissionItemDto {
    MissionItemDto {
        key: i.key.clone(),
        kind: i.kind.clone(),
        title: i.title.clone(),
        status: i.status.as_str().to_owned(),
        attempts: i.attempts,
        result: i.result.clone(),
        last_error: i.last_error.clone(),
        blocked_reason: i.blocked_reason.clone(),
        needs_live_owner: i.live_only,
        owner_answer: i.owner_answer.clone(),
        waiting_for: waits
            .iter()
            .filter(|w| w.item_key == i.key)
            .map(wait_dto)
            .collect(),
        updated_at_ms: i.updated_at_ms,
    }
}

fn digest_items(v: &[missions::DigestItem]) -> Vec<MissionDigestItemDto> {
    v.iter()
        .map(|d| MissionDigestItemDto {
            key: d.key.clone(),
            title: d.title.clone(),
            text: d.text.clone(),
        })
        .collect()
}

fn detail(s: &AppState, m: &Mission) -> Result<MissionDetailDto, ApiError> {
    let store = s.mission_store()?;
    let items = store.items(&m.id, None).map_err(store_err)?;
    let waits = store.active_waits(&m.id).map_err(store_err)?;
    Ok(MissionDetailDto {
        mission: mission_dto(s, m)?,
        blocked: items
            .iter()
            .filter(|i| i.status == ItemStatus::Blocked)
            .map(|i| item_dto(i, &waits))
            .collect(),
        items: items.iter().map(|i| item_dto(i, &waits)).collect(),
        digest: m.digest.as_ref().map(|d| MissionDigestDto {
            at_ms: d.at_ms,
            outcome: d.outcome.clone(),
            reason: d.reason.clone(),
            progress: progress_dto(d.progress),
            blocked: digest_items(&d.blocked),
            failed: digest_items(&d.failed),
            done_sample: digest_items(&d.done_sample),
            decisions: d.decisions.clone(),
        }),
    })
}

/// A mission of the local user, or `404`.
fn mission_of_user(s: &AppState, id: Uuid) -> Result<Mission, ApiError> {
    s.mission_store()?
        .get(&id.to_string())
        .map_err(store_err)?
        .filter(|m| m.user_id == s.user_id().to_string())
        .ok_or_else(|| ApiError::not_found(format!("mission {id}")))
}

fn audit(s: &AppState, action: &str, id: &str, extra: &serde_json::Value) {
    // Ids and counts only, never the goal text.
    let _ = s.storage().append_audit(
        s.user_id(),
        None,
        "user",
        action,
        &json!({"mission_id": id, "info": extra}),
    );
}

/// The user's missions, newest first.
#[utoipa::path(get, path = "/v1/missions", tag = "missions", security(("bearer" = [])),
    params(ListMissionsQuery),
    responses((status = 200, body = Vec<MissionDto>), (status = 503, body = ErrorBody)))]
pub(crate) async fn list_missions(
    State(s): State<AppState>,
    Query(q): Query<ListMissionsQuery>,
) -> Result<Json<Vec<MissionDto>>, ApiError> {
    let list = s
        .mission_store()?
        .list(&s.user_id().to_string())
        .map_err(store_err)?;
    let active_only = q.state.as_deref() == Some("active");
    list.iter()
        .filter(|m| q.bot_id.is_none_or(|b| uuid_of(&m.bot_id) == b))
        .filter(|m| {
            !active_only
                || matches!(
                    m.state,
                    MissionState::Running
                        | MissionState::Waiting
                        | MissionState::Paused
                        | MissionState::Blocked
                )
        })
        .map(|m| mission_dto(&s, m))
        .collect::<Result<Vec<_>, _>>()
        .map(Json)
}

/// One mission with its ledger, blocked items and digest.
#[utoipa::path(get, path = "/v1/missions/{id}", tag = "missions", security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "Mission id")),
    responses((status = 200, body = MissionDetailDto), (status = 404, body = ErrorBody)))]
pub(crate) async fn get_mission(
    State(s): State<AppState>,
    AxumPath(id): AxumPath<Uuid>,
) -> Result<Json<MissionDetailDto>, ApiError> {
    let m = mission_of_user(&s, id)?;
    Ok(Json(detail(&s, &m)?))
}

/// The label of a mission goal typed in a bridge (`hint`: the `x-praxi-message-source` header).
/// This route is owner-only and the bridges hold the owner key, so a goal from the paired
/// Telegram or Discord user is that owner's own words on their channel: `telegram_owner` /
/// `discord_owner`, which the approval and mission gates treat alike. Anything else, including a
/// hint that already says `*_owner`, is the app's `owner`: a hint never names a label itself.
fn goal_source(hint: Option<&str>) -> &'static str {
    match hint {
        Some("telegram") => "telegram_owner",
        Some("discord") => "discord_owner",
        _ => "owner",
    }
}

/// Start a mission (owner only): the bot works through the goal alone, session after session,
/// until it is done, a budget is used up or the owner stops it. The bot has to be on the Full
/// level. The goal is stored verbatim; the audit entry `mission.created` holds ids only.
#[utoipa::path(post, path = "/v1/missions", tag = "missions", security(("bearer" = [])),
    request_body = CreateMissionInput,
    responses((status = 200, body = MissionDetailDto), (status = 400, body = ErrorBody),
              (status = 404, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub(crate) async fn create_mission(
    State(s): State<AppState>,
    headers: HeaderMap,
    ApiJson(input): ApiJson<CreateMissionInput>,
) -> Result<Json<MissionDetailDto>, ApiError> {
    s.require_owner(&headers)?;
    let store = s.mission_store()?;
    let supervisor = s.mission_supervisor()?;
    let bot = s.bot_of_user(BotId(input.bot_id))?;
    if !s.agent().bot_is_full(bot.id) {
        return Err(ApiError::bad_request(
            "a mission runs without asking, so the bot has to be on the Full level first",
        ));
    }
    let goal = input.goal.trim();
    if goal.is_empty() || goal.chars().count() > MAX_GOAL_CHARS {
        return Err(ApiError::bad_request(format!(
            "the goal must be 1 to {MAX_GOAL_CHARS} characters"
        )));
    }
    let budgets = input.budgets.map_or_else(Budgets::default, Budgets::from);
    // The mission's own chat: its sessions run there and its digest is posted there.
    let title = input
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map_or_else(
            || missions::model::clip(goal, 60),
            |t| missions::model::clip(t, 80),
        );
    let chat = Chat {
        id: ChatId::new(),
        user_id: s.user_id(),
        bot_id: bot.id,
        title: format!("Mission: {title}"),
        created_at_ms: now_ms(),
    };
    s.storage().insert_chat(&chat)?;
    let goal_message = Uuid::now_v7();
    s.storage().insert_message(&Message {
        id: goal_message,
        user_id: s.user_id(),
        chat_id: chat.id,
        run_id: None,
        role: "user".to_owned(),
        content: goal.to_owned(),
        created_at_ms: now_ms(),
        exposure_untrusted: false,
        exposure_private: false,
        exposure_sources: Vec::new(),
    })?;
    // The goal is the one text of a mission that is the owner's own words (the route is
    // owner-only): named sites and typed URLs in it count for the approval code. The briefs the
    // supervisor writes later are tagged `mission_brief` and never do. A bridge says where the
    // words were typed (`x-praxi-message-source`, as for a chat message) and the goal is labelled
    // like that chat message would be: a hint can only lower the standing, never raise it.
    let source = goal_source(
        headers
            .get("x-praxi-message-source")
            .and_then(|v| v.to_str().ok()),
    );
    s.storage().set_message_source(goal_message, source)?;
    let m = store
        .create(
            &NewMission {
                user_id: s.user_id().to_string(),
                bot_id: bot.id.to_string(),
                chat_id: chat.id.to_string(),
                title,
                goal_text: goal.to_owned(),
                target_done: input.target_done.filter(|n| *n > 0),
                budgets,
                items: Vec::new(),
            },
            now_ms(),
        )
        .map_err(store_err)?;
    audit(
        &s,
        "mission.created",
        &m.id,
        &json!({"bot_id": bot.id.to_string()}),
    );
    supervisor.wake();
    Ok(Json(detail(&s, &m)?))
}

fn check_full_for_resume(s: &AppState, m: &Mission) -> Result<(), ApiError> {
    if s.agent().bot_is_full(BotId(uuid_of(&m.bot_id))) {
        Ok(())
    } else {
        Err(ApiError::bad_request(
            "the bot is no longer on the Full level; set it back before resuming",
        ))
    }
}

/// Resume a paused or blocked mission for the owner (the owner-notice "try again" goes through
/// here; the caller already checked the owner key).
pub(crate) fn resume_for_owner(s: &AppState, id: Uuid) -> Result<(), ApiError> {
    let m = mission_of_user(s, id)?;
    let sup = s.mission_supervisor()?;
    check_full_for_resume(s, &m)?;
    sup.resume(&m.id).map_err(store_err)?;
    audit(s, "mission.resume", &m.id, &json!({"via": "owner_notice"}));
    Ok(())
}

async fn control(
    s: &AppState,
    headers: &HeaderMap,
    id: Uuid,
    what: &'static str,
) -> Result<Json<MissionDetailDto>, ApiError> {
    s.require_owner(headers)?;
    let m = mission_of_user(s, id)?;
    let sup = s.mission_supervisor()?;
    let after = match what {
        "pause" => sup.pause(&m.id),
        "resume" => {
            check_full_for_resume(s, &m)?;
            sup.resume(&m.id)
        }
        _ => sup.cancel(&m.id).await,
    }
    .map_err(store_err)?;
    audit(s, &format!("mission.{what}"), &m.id, &json!({}));
    Ok(Json(detail(s, &after)?))
}

/// Pause a mission (owner only): a session in flight is stopped, no new one starts.
#[utoipa::path(post, path = "/v1/missions/{id}/pause", tag = "missions", security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "Mission id")),
    responses((status = 200, body = MissionDetailDto), (status = 404, body = ErrorBody),
              (status = 409, body = ErrorBody)))]
pub(crate) async fn pause_mission(
    State(s): State<AppState>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<Uuid>,
) -> Result<Json<MissionDetailDto>, ApiError> {
    control(&s, &headers, id, "pause").await
}

/// Resume a paused or blocked mission (owner only).
#[utoipa::path(post, path = "/v1/missions/{id}/resume", tag = "missions", security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "Mission id")),
    responses((status = 200, body = MissionDetailDto), (status = 404, body = ErrorBody),
              (status = 409, body = ErrorBody)))]
pub(crate) async fn resume_mission(
    State(s): State<AppState>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<Uuid>,
) -> Result<Json<MissionDetailDto>, ApiError> {
    control(&s, &headers, id, "resume").await
}

/// Stop a mission for good (owner only): the live session is stopped and the owner gets the digest.
#[utoipa::path(post, path = "/v1/missions/{id}/cancel", tag = "missions", security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "Mission id")),
    responses((status = 200, body = MissionDetailDto), (status = 404, body = ErrorBody),
              (status = 409, body = ErrorBody)))]
pub(crate) async fn cancel_mission(
    State(s): State<AppState>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<Uuid>,
) -> Result<Json<MissionDetailDto>, ApiError> {
    control(&s, &headers, id, "cancel").await
}

/// Answer the blocked items of a mission in one go (owner only). Answered items go back to the
/// bot with the answer; skipped ones are left out; a blocked mission runs again. A one-time code
/// (2FA) is never an answer: it would be stored in the ledger and shown to the bot.
#[utoipa::path(post, path = "/v1/missions/{id}/answers", tag = "missions", security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "Mission id")),
    request_body = MissionAnswersInput,
    responses((status = 200, body = MissionAnswersDto), (status = 400, body = ErrorBody),
              (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)))]
pub(crate) async fn answer_mission(
    State(s): State<AppState>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<Uuid>,
    ApiJson(input): ApiJson<MissionAnswersInput>,
) -> Result<Json<MissionAnswersDto>, ApiError> {
    s.require_owner(&headers)?;
    apply_answers(&s, id, &input).map(Json)
}

/// The answers of `POST /v1/missions/{id}/answers`, for the callers that already checked the
/// owner (that route, and the owner-notice route a Telegram or Discord reply goes through).
pub(crate) fn apply_answers(
    s: &AppState,
    id: Uuid,
    input: &MissionAnswersInput,
) -> Result<MissionAnswersDto, ApiError> {
    let m = mission_of_user(s, id)?;
    // An ended mission is history: its ledger no longer changes.
    if m.state.is_terminal() {
        return Err(ApiError::conflict(format!(
            "this mission is {}: its ledger no longer changes",
            m.state
        )));
    }
    let sup = s.mission_supervisor()?;
    let store = s.mission_store()?;
    if input.answers.is_empty() || input.answers.len() > missions::model::MAX_ITEMS {
        return Err(ApiError::bad_request("send at least one answer"));
    }
    // Validate everything first so a bad request changes nothing.
    let blocked: std::collections::HashMap<String, missions::Item> = store
        .items(&m.id, Some(ItemStatus::Blocked))
        .map_err(store_err)?
        .into_iter()
        .map(|i| (i.key.clone(), i))
        .collect();
    for a in &input.answers {
        let Some(item) = blocked.get(&a.key) else {
            return Err(ApiError::bad_request(format!(
                "item {:?} is not blocked",
                a.key
            )));
        };
        if !a.skip && a.answer.trim().is_empty() {
            return Err(ApiError::bad_request(format!(
                "item {:?}: write an answer or choose to skip it",
                a.key
            )));
        }
        if !a.skip && item.live_only {
            return Err(ApiError::bad_request(format!(
                "item {:?} needs you live (a code): Praxi never stores one or shows it to the bot. Finish that step yourself on the site, then skip the item",
                a.key
            )));
        }
    }
    // An answer or a skip may start a blocked mission again, and a mission only runs unattended
    // on a bot that is still at the Full level.
    let resumes = matches!(m.state, MissionState::Blocked | MissionState::Waiting);
    if resumes && !s.agent().bot_is_full(BotId(uuid_of(&m.bot_id))) {
        return Err(ApiError::bad_request(
            "the bot is no longer on the Full level; set it back before answering",
        ));
    }
    let now = now_ms();
    let (skips, answers): (Vec<_>, Vec<_>) = input.answers.iter().partition(|a| a.skip);
    // The store checks all answers (a code is refused) and changes nothing when it refuses one,
    // so the answers go first and a refusal leaves the skips undone too.
    let to_answer: Vec<(String, String)> = answers
        .iter()
        .map(|a| (a.key.clone(), a.answer.clone()))
        .collect();
    let released = if to_answer.is_empty() {
        0
    } else {
        sup.answer_blocked(&m.id, &to_answer).map_err(store_err)?
    };
    let mut skipped = 0;
    for a in &skips {
        let upd = missions::ItemUpdate {
            status: Some(ItemStatus::Skipped),
            ..missions::ItemUpdate::default()
        };
        store
            .update_item(&m.id, &a.key, &upd, true, now)
            .map_err(store_err)?;
        skipped += 1;
    }
    // Skipping the last blocked item may finish a blocked mission: let the supervisor look.
    if skipped > 0 && released == 0 && resumes {
        sup.resume(&m.id).map_err(store_err)?;
    }
    audit(
        s,
        "mission.answers",
        &m.id,
        &json!({"released": released, "skipped": skipped}),
    );
    Ok(MissionAnswersDto { released, skipped })
}

impl AppState {
    /// `chat` is the chat of one of the user's missions.
    pub(crate) fn is_mission_chat(&self, chat: ChatId) -> bool {
        let Some(store) = self.0.mission_store.get() else {
            return false;
        };
        // A ledger that cannot be read fails closed: nobody but the owner gets the chat.
        store
            .mission_for_chat(&chat.to_string())
            .map_or(true, |m| m.is_some())
    }

    /// The run is a mission session, or runs in a mission's chat (a subagent of a session).
    pub(crate) fn is_mission_run(&self, run: &storage::Run) -> bool {
        missions::mission_of_kind(&run.kind).is_some()
            || run.chat_id.is_some_and(|c| self.is_mission_chat(c))
    }

    /// [`Self::is_mission_run`] against a chat set read once (for lists).
    pub(crate) fn run_in_missions(run: &storage::Run, mission_chats: &HashSet<ChatId>) -> bool {
        missions::mission_of_kind(&run.kind).is_some()
            || run.chat_id.is_some_and(|c| mission_chats.contains(&c))
    }

    /// The chats of the user's missions, for lists that must not show them to a non-owner.
    pub(crate) fn mission_chat_ids(&self) -> HashSet<ChatId> {
        let Some(store) = self.0.mission_store.get() else {
            return HashSet::new();
        };
        store
            .list(&self.user_id().to_string())
            .unwrap_or_default()
            .iter()
            .filter_map(|m| Uuid::parse_str(&m.chat_id).ok().map(ChatId))
            .collect()
    }

    /// Whether a request for `path` reads, steers or ends a mission: any route under
    /// `/v1/chats/{id}` or `/v1/runs/{id}` whose chat is a mission's chat or whose run is one
    /// of its sessions. The path is decoded first, like the owner-route table does.
    fn path_targets_mission(&self, path: &str) -> bool {
        let decoded = percent_encoding::percent_decode_str(path).decode_utf8_lossy();
        let mut segs = decoded.split('/').filter(|p| !p.is_empty());
        if segs.next() != Some("v1") {
            return false;
        }
        match (segs.next(), segs.next().map(Uuid::parse_str)) {
            (Some("chats"), Some(Ok(chat))) => self.is_mission_chat(ChatId(chat)),
            (Some("runs"), Some(Ok(run))) => match self.storage().get_run(RunId(run)) {
                Ok(Some(r)) => self.is_mission_run(&r),
                Ok(None) => false,
                Err(_) => true,
            },
            _ => false,
        }
    }
}

/// A mission is the owner's: its chat (the goal, the briefs, the digest) and the runs of its
/// sessions can be read, steered, resumed, cancelled or deleted only with the owner key (the
/// local app, or the host's administrator desktop, which the peer guard shows the key). A holder
/// of only the API token, a paired phone or another desktop is refused whichever route it uses.
pub(crate) async fn mission_gate(State(s): State<AppState>, req: Request, next: Next) -> Response {
    if !s.owner_ok(req.headers()) && s.path_targets_mission(req.uri().path()) {
        return ApiError::new(
            axum::http::StatusCode::FORBIDDEN,
            crate::owner::OWNER_REQUIRED,
            "this chat or run belongs to a mission, which is the owner's: use the app, or run it on the host with its owner key",
        )
        .into_response();
    }
    next.run(req).await
}
