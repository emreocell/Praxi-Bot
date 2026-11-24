//! Inspectable personal memory management (ADR-0017 / card 162): list, search,
//! add, edit, forget/delete, undo via the journal, and preview what the recall
//! step will inject for a bot and query.

use axum::http::StatusCode;
use axum::{
    Json,
    extract::{Path as ApiPath, Query, State},
};
use memory_store::{
    Kind, ListFilter, Memory, MemoryError, MemoryMode, MemorySettings, MemoryStore, NewMemory,
    Trust, estimate_tokens, render_recall_block,
};
use protocol::{BotId, UserId};
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{ApiError, AppState, error::ErrorBody, routes::ApiJson};

/// Maximum UTF-8 byte length of a single user-authored or edited memory item.
pub const MAX_MEMORY_CONTENT_BYTES: usize = 8192;

/// Memory category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKindDto {
    Profile,
    Bot,
    Episodic,
    Procedural,
    Lesson,
}

impl From<MemoryKindDto> for Kind {
    fn from(value: MemoryKindDto) -> Self {
        match value {
            MemoryKindDto::Profile => Self::Profile,
            MemoryKindDto::Bot => Self::Bot,
            MemoryKindDto::Episodic => Self::Episodic,
            MemoryKindDto::Procedural => Self::Procedural,
            MemoryKindDto::Lesson => Self::Lesson,
        }
    }
}

impl From<Kind> for MemoryKindDto {
    fn from(value: Kind) -> Self {
        match value {
            Kind::Profile => Self::Profile,
            Kind::Bot => Self::Bot,
            Kind::Episodic => Self::Episodic,
            Kind::Procedural => Self::Procedural,
            Kind::Lesson => Self::Lesson,
        }
    }
}

/// Provenance trust of a stored memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemoryTrustDto {
    User,
    Untrusted,
}

impl From<Trust> for MemoryTrustDto {
    fn from(value: Trust) -> Self {
        match value {
            Trust::User => Self::User,
            Trust::Untrusted => Self::Untrusted,
        }
    }
}

/// One stored memory item and its active undo journal state.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MemoryItemDto {
    pub id: Uuid,
    /// `None` means shared across all bots of this owner.
    pub bot_id: Option<Uuid>,
    pub kind: MemoryKindDto,
    pub content: String,
    pub source_quote: Option<String>,
    pub source_chat_id: Option<Uuid>,
    pub source_message_id: Option<Uuid>,
    pub trust: MemoryTrustDto,
    pub importance: f32,
    pub created_ms: i64,
    pub last_used_ms: i64,
    pub valid_until_ms: Option<i64>,
    pub deleted: bool,
    /// Held for the owner's OK ("Needs your OK"): not used in answers until approved.
    pub pending: bool,
    /// Newest active journal entry id that can undo the last change to this memory.
    pub undo_journal_id: Option<i64>,
}

/// Query parameters for `GET /v1/memory`.
#[derive(Debug, Default, Deserialize)]
pub struct MemoryListQuery {
    /// Restrict to memories visible to this bot (shared + this bot's).
    pub bot: Option<Uuid>,
    /// When true with `bot`, return only memories scoped specifically to `bot` (exclude shared).
    #[serde(default)]
    pub bot_only: bool,
    /// Filter by category.
    pub kind: Option<MemoryKindDto>,
    /// Case-insensitive text filter over content and supporting quote.
    pub q: Option<String>,
    /// Include soft-deleted (forgotten) memories so they can be inspected or restored.
    #[serde(default)]
    pub include_deleted: bool,
}

/// Create a user-authored memory item (`POST /v1/memory`).
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateMemoryInput {
    /// `None` creates a shared memory across all bots; `Some(bot)` scopes to that bot.
    pub bot_id: Option<Uuid>,
    pub kind: MemoryKindDto,
    pub content: String,
}

/// Edit a memory item's text (`PATCH /v1/memory/{id}`).
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateMemoryInput {
    pub content: String,
}

/// Query parameters for `GET /v1/bots/{id}/memory/preview`.
#[derive(Debug, Deserialize)]
pub struct MemoryPreviewQuery {
    pub query: String,
}

/// One recalled memory and its boosted fusion score in a recall preview.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MemoryPreviewHitDto {
    pub memory: MemoryItemDto,
    pub score: f64,
}

/// Exact recall preview for a bot and query (`GET /v1/bots/{id}/memory/preview`).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MemoryPreviewDto {
    pub bot_id: Uuid,
    pub query: String,
    pub budget_tokens: usize,
    pub estimated_tokens: usize,
    /// The exact delimited data block `with_recall` prepends to the user message.
    pub rendered_block: String,
    pub hits: Vec<MemoryPreviewHitDto>,
}

/// A statement the owner forgot; extraction does not learn it again until the
/// owner removes this entry. Holds no text once erased permanently.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ForgottenItemDto {
    pub id: Uuid,
    /// `None` means the statement was forgotten for all bots.
    pub bot_id: Option<Uuid>,
    pub kind: MemoryKindDto,
    /// Short preview; `None` when the item was erased permanently.
    pub label: Option<String>,
    /// Erased permanently: the text is gone everywhere.
    pub erased: bool,
    pub created_ms: i64,
}

/// Query parameters for `GET /v1/memory/forgotten`.
#[derive(Debug, Default, Deserialize)]
pub struct ForgottenListQuery {
    /// Restrict to items that apply to this bot (shared + this bot's).
    pub bot: Option<Uuid>,
}

fn map_err(error: MemoryError) -> ApiError {
    match error {
        MemoryError::NotFound(msg) => ApiError::not_found(msg),
        MemoryError::Invalid(msg) => ApiError::conflict(msg),
        _ => ApiError::internal("memory operation failed"),
    }
}

fn validate_user_content(s: &AppState, raw: &str) -> Result<String, ApiError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ApiError::bad_request("memory content cannot be empty"));
    }
    if trimmed.len() > MAX_MEMORY_CONTENT_BYTES {
        return Err(ApiError::bad_request(format!(
            "memory content exceeds {MAX_MEMORY_CONTENT_BYTES} byte limit"
        )));
    }
    s.agent().refresh_external_redactor();
    if memory_store::contains_secret(trimmed)
        || s.agent().redact_external_text(trimmed.to_owned()) != trimmed
    {
        return Err(ApiError::bad_request("memory content contains a secret"));
    }
    Ok(trimmed.to_owned())
}

fn item_dto(store: &MemoryStore, user: UserId, m: Memory) -> Result<MemoryItemDto, ApiError> {
    let undo_journal_id = store.latest_journal_id(user, m.id).map_err(map_err)?;
    Ok(MemoryItemDto {
        id: m.id,
        bot_id: m.bot_id.map(|b| b.0),
        kind: m.kind.into(),
        content: m.content,
        source_quote: m.source_quote,
        source_chat_id: m.source_chat_id.map(|c| c.0),
        source_message_id: m.source_message_id,
        trust: m.trust.into(),
        importance: m.importance,
        created_ms: m.created_ms,
        last_used_ms: m.last_used_ms,
        valid_until_ms: m.valid_until_ms,
        deleted: m.deleted,
        pending: m.pending,
        undo_journal_id,
    })
}

#[utoipa::path(
    get,
    path = "/v1/memory",
    tag = "memory",
    security(("bearer" = [])),
    params(
        ("bot" = Option<Uuid>, Query, description = "Filter to memories visible to this bot"),
        ("bot_only" = Option<bool>, Query, description = "Exclude shared memories when filtering by bot"),
        ("kind" = Option<MemoryKindDto>, Query, description = "Filter by memory category"),
        ("q" = Option<String>, Query, description = "Case-insensitive search text"),
        ("include_deleted" = Option<bool>, Query, description = "Include forgotten (soft-deleted) items")
    ),
    responses(
        (status = 200, body = Vec<MemoryItemDto>),
        (status = 404, body = ErrorBody)
    )
)]
pub(crate) async fn list_memories(
    State(s): State<AppState>,
    Query(query): Query<MemoryListQuery>,
) -> Result<Json<Vec<MemoryItemDto>>, ApiError> {
    let bot_id = match query.bot {
        Some(id) => Some(s.bot_of_user(BotId(id))?.id),
        None => None,
    };
    let store = s.memory_store()?;
    let user = s.user_id();
    let needle = query
        .q
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .map(str::to_lowercase);
    let filter = ListFilter {
        bot_id,
        kind: query.kind.map(Kind::from),
        include_deleted: query.include_deleted,
    };
    let items = store.list(user, &filter).map_err(map_err)?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        if query.bot_only && bot_id.is_some() && item.bot_id != bot_id {
            continue;
        }
        if let Some(ref q) = needle {
            let matches_content = item.content.to_lowercase().contains(q);
            let matches_quote = item
                .source_quote
                .as_ref()
                .is_some_and(|quote| quote.to_lowercase().contains(q));
            if !matches_content && !matches_quote {
                continue;
            }
        }
        out.push(item_dto(&store, user, item)?);
    }
    Ok(Json(out))
}

#[utoipa::path(
    post,
    path = "/v1/memory",
    tag = "memory",
    security(("bearer" = [])),
    request_body = CreateMemoryInput,
    responses(
        (status = 201, body = MemoryItemDto),
        (status = 400, body = ErrorBody),
        (status = 404, body = ErrorBody)
    )
)]
pub(crate) async fn create_memory(
    State(s): State<AppState>,
    ApiJson(input): ApiJson<CreateMemoryInput>,
) -> Result<(StatusCode, Json<MemoryItemDto>), ApiError> {
    let bot_id = match input.bot_id {
        Some(id) => Some(s.bot_of_user(BotId(id))?.id),
        None => None,
    };
    let content = validate_user_content(&s, &input.content)?;
    let user = s.user_id();
    let kind = Kind::from(input.kind);
    s.storage().append_audit(
        user,
        None,
        "user",
        "memory.create",
        &json!({
            "bot_id": bot_id.map(|b| b.0),
            "kind": kind.as_str(),
        }),
    )?;
    let store = s.memory_store()?;
    let created = store
        .add(&NewMemory::new(user, bot_id, kind, content), "user add")
        .map_err(map_err)?;
    Ok((StatusCode::CREATED, Json(item_dto(&store, user, created)?)))
}

#[utoipa::path(
    patch,
    path = "/v1/memory/{id}",
    tag = "memory",
    security(("bearer" = [])),
    params(("id" = Uuid, Path)),
    request_body = UpdateMemoryInput,
    responses(
        (status = 200, body = MemoryItemDto),
        (status = 400, body = ErrorBody),
        (status = 404, body = ErrorBody)
    )
)]
pub(crate) async fn update_memory(
    State(s): State<AppState>,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(input): ApiJson<UpdateMemoryInput>,
) -> Result<Json<MemoryItemDto>, ApiError> {
    let content = validate_user_content(&s, &input.content)?;
    let user = s.user_id();
    s.storage().append_audit(
        user,
        None,
        "user",
        "memory.update",
        &json!({ "memory_id": id }),
    )?;
    let store = s.memory_store()?;
    let updated = store.edit(user, id, &content).map_err(map_err)?;
    Ok(Json(item_dto(&store, user, updated)?))
}

#[utoipa::path(
    delete,
    path = "/v1/memory/{id}",
    tag = "memory",
    security(("bearer" = [])),
    params(("id" = Uuid, Path)),
    responses(
        (status = 200, body = MemoryItemDto),
        (status = 404, body = ErrorBody)
    )
)]
pub(crate) async fn delete_memory(
    State(s): State<AppState>,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<MemoryItemDto>, ApiError> {
    let user = s.user_id();
    s.storage().append_audit(
        user,
        None,
        "user",
        "memory.delete",
        &json!({ "memory_id": id }),
    )?;
    let store = s.memory_store()?;
    store.delete(user, id, "user forget").map_err(map_err)?;
    let deleted = store
        .get(id)
        .map_err(map_err)?
        .filter(|m| m.user_id == user)
        .ok_or_else(|| ApiError::not_found(format!("memory {id}")))?;
    Ok(Json(item_dto(&store, user, deleted)?))
}

#[utoipa::path(
    post,
    path = "/v1/memory/{id}/erase",
    tag = "memory",
    security(("bearer" = [])),
    params(("id" = Uuid, Path)),
    responses(
        (status = 204),
        (status = 404, body = ErrorBody)
    )
)]
/// Erase a memory permanently (live or already forgotten): its text is removed
/// from the item, the search index and the undo journal. Backups made earlier
/// still contain it; restoring one can bring the item back.
pub(crate) async fn erase_memory(
    State(s): State<AppState>,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<StatusCode, ApiError> {
    let user = s.user_id();
    s.storage().append_audit(
        user,
        None,
        "user",
        "memory.erase",
        &json!({ "memory_id": id }),
    )?;
    let store = s.memory_store()?;
    store.purge(user, id, "owner erased").map_err(map_err)?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    get,
    path = "/v1/memory/forgotten",
    tag = "memory",
    security(("bearer" = [])),
    params(("bot" = Option<Uuid>, Query, description = "Only items that apply to this bot")),
    responses(
        (status = 200, body = Vec<ForgottenItemDto>),
        (status = 404, body = ErrorBody)
    )
)]
pub(crate) async fn list_forgotten(
    State(s): State<AppState>,
    Query(query): Query<ForgottenListQuery>,
) -> Result<Json<Vec<ForgottenItemDto>>, ApiError> {
    let bot_id = match query.bot {
        Some(id) => Some(s.bot_of_user(BotId(id))?.id),
        None => None,
    };
    let store = s.memory_store()?;
    let items = store
        .list_tombstones(s.user_id(), bot_id)
        .map_err(map_err)?;
    Ok(Json(
        items
            .into_iter()
            .map(|t| ForgottenItemDto {
                id: t.id,
                bot_id: t.bot_id.map(|b| b.0),
                kind: t.kind.into(),
                label: t.label,
                erased: t.erased,
                created_ms: t.created_ms,
            })
            .collect(),
    ))
}

#[utoipa::path(
    delete,
    path = "/v1/memory/forgotten/{id}",
    tag = "memory",
    security(("bearer" = [])),
    params(("id" = Uuid, Path)),
    responses(
        (status = 204),
        (status = 404, body = ErrorBody)
    )
)]
/// Let a forgotten statement be learned again.
pub(crate) async fn remove_forgotten(
    State(s): State<AppState>,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<StatusCode, ApiError> {
    let user = s.user_id();
    s.storage().append_audit(
        user,
        None,
        "user",
        "memory.forgotten.remove",
        &json!({ "forgotten_id": id }),
    )?;
    let store = s.memory_store()?;
    store.remove_tombstone(user, id).map_err(map_err)?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/v1/memory/{id}/undo",
    tag = "memory",
    security(("bearer" = [])),
    params(("id" = Uuid, Path)),
    responses(
        (status = 200, body = MemoryItemDto),
        (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody)
    )
)]
pub(crate) async fn undo_memory(
    State(s): State<AppState>,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<MemoryItemDto>, ApiError> {
    let user = s.user_id();
    s.storage().append_audit(
        user,
        None,
        "user",
        "memory.undo",
        &json!({ "memory_id": id }),
    )?;
    let store = s.memory_store()?;
    let restored = store.undo_memory(user, id).map_err(map_err)?;
    Ok(Json(item_dto(&store, user, restored)?))
}

#[utoipa::path(
    get,
    path = "/v1/bots/{id}/memory/preview",
    tag = "memory",
    security(("bearer" = [])),
    params(
        ("id" = Uuid, Path),
        ("query" = String, Query, description = "Query text to preview recall for")
    ),
    responses(
        (status = 200, body = MemoryPreviewDto),
        (status = 400, body = ErrorBody),
        (status = 404, body = ErrorBody)
    )
)]
pub(crate) async fn preview_bot_memory(
    State(s): State<AppState>,
    ApiPath(id): ApiPath<Uuid>,
    Query(q): Query<MemoryPreviewQuery>,
) -> Result<Json<MemoryPreviewDto>, ApiError> {
    let bot = s.bot_of_user(BotId(id))?;
    let query = q.query.trim();
    if query.len() > MAX_MEMORY_CONTENT_BYTES {
        return Err(ApiError::bad_request("preview query exceeds byte limit"));
    }
    let store = s.memory_store()?;
    let user = s.user_id();
    let settings = MemorySettings::from_bot_config(&bot.config);
    let budget_tokens = settings.budget(s.agent().memory_recall_budget_tokens());
    // A bot with memory switched off recalls nothing, so the preview shows nothing.
    let selected = if query.is_empty() || !settings.enabled() {
        Vec::new()
    } else {
        store
            .preview_recall(user, bot.id, query, budget_tokens)
            .map_err(map_err)?
    };
    let memories: Vec<Memory> = selected.iter().map(|(m, _)| m.clone()).collect();
    let rendered_block = render_recall_block(&memories);
    let estimated_tokens = estimate_tokens(&rendered_block);
    let mut hits = Vec::with_capacity(selected.len());
    for (m, score) in selected {
        hits.push(MemoryPreviewHitDto {
            memory: item_dto(&store, user, m)?,
            score,
        });
    }
    Ok(Json(MemoryPreviewDto {
        bot_id: bot.id.0,
        query: query.to_owned(),
        budget_tokens,
        estimated_tokens,
        rendered_block,
        hits,
    }))
}

/// `GET /v1/memory/pending`: memories waiting for the owner's OK.
#[utoipa::path(
    get,
    path = "/v1/memory/pending",
    tag = "memory",
    security(("bearer" = [])),
    responses((status = 200, body = Vec<MemoryItemDto>))
)]
pub(crate) async fn list_pending(
    State(s): State<AppState>,
) -> Result<Json<Vec<MemoryItemDto>>, ApiError> {
    let store = s.memory_store()?;
    let user = s.user_id();
    let mut out = Vec::new();
    for m in store.list_pending(user).map_err(map_err)? {
        out.push(item_dto(&store, user, m)?);
    }
    Ok(Json(out))
}

#[utoipa::path(
    post,
    path = "/v1/memory/{id}/approve",
    tag = "memory",
    security(("bearer" = [])),
    params(("id" = Uuid, Path)),
    responses((status = 200, body = MemoryItemDto), (status = 404, body = ErrorBody))
)]
pub(crate) async fn approve_memory(
    State(s): State<AppState>,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<MemoryItemDto>, ApiError> {
    let user = s.user_id();
    s.storage().append_audit(
        user,
        None,
        "user",
        "memory.approve",
        &json!({ "memory_id": id }),
    )?;
    let store = s.memory_store()?;
    let approved = store.approve(user, id).map_err(map_err)?;
    Ok(Json(item_dto(&store, user, approved)?))
}

#[utoipa::path(
    post,
    path = "/v1/memory/{id}/discard",
    tag = "memory",
    security(("bearer" = [])),
    params(("id" = Uuid, Path)),
    responses((status = 200, body = MemoryItemDto), (status = 404, body = ErrorBody))
)]
pub(crate) async fn discard_memory(
    State(s): State<AppState>,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<MemoryItemDto>, ApiError> {
    let user = s.user_id();
    s.storage().append_audit(
        user,
        None,
        "user",
        "memory.discard",
        &json!({ "memory_id": id }),
    )?;
    let store = s.memory_store()?;
    store.discard(user, id).map_err(map_err)?;
    let gone = store
        .get(id)
        .map_err(map_err)?
        .filter(|m| m.user_id == user)
        .ok_or_else(|| ApiError::not_found(format!("memory {id}")))?;
    Ok(Json(item_dto(&store, user, gone)?))
}

/// A bot's memory settings as the settings screen shows them.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BotMemorySettingsDto {
    /// `on`, `off` or `ask`.
    pub mode: String,
    /// Recall budget in tokens (the default is 800).
    pub budget_tokens: usize,
    /// Slider lower bound.
    pub budget_min: usize,
    /// Slider upper bound.
    pub budget_max: usize,
    /// Share profile and episodic facts with the owner's other bots.
    pub share_with_other_bots: bool,
}

/// Change a bot's memory settings (`PUT /v1/bots/{id}/memory/settings`).
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct BotMemorySettingsInput {
    /// `on`, `off` or `ask`.
    pub mode: String,
    /// Recall budget in tokens; `null` = the default.
    pub budget_tokens: Option<usize>,
    /// Share profile and episodic facts with the owner's other bots.
    pub share_with_other_bots: bool,
}

fn settings_dto(settings: &MemorySettings, default_budget: usize) -> BotMemorySettingsDto {
    BotMemorySettingsDto {
        mode: match settings.mode {
            MemoryMode::On => "on",
            MemoryMode::Off => "off",
            MemoryMode::Ask => "ask",
        }
        .to_owned(),
        budget_tokens: settings.budget(default_budget),
        budget_min: memory_store::MIN_BUDGET_TOKENS,
        budget_max: memory_store::MAX_BUDGET_TOKENS,
        share_with_other_bots: settings.share_with_other_bots,
    }
}

#[utoipa::path(
    get,
    path = "/v1/bots/{id}/memory/settings",
    tag = "memory",
    security(("bearer" = [])),
    params(("id" = Uuid, Path)),
    responses((status = 200, body = BotMemorySettingsDto), (status = 404, body = ErrorBody))
)]
pub(crate) async fn get_bot_memory_settings(
    State(s): State<AppState>,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<BotMemorySettingsDto>, ApiError> {
    let bot = s.bot_of_user(BotId(id))?;
    Ok(Json(settings_dto(
        &MemorySettings::from_bot_config(&bot.config),
        s.agent().memory_recall_budget_tokens(),
    )))
}

#[utoipa::path(
    put,
    path = "/v1/bots/{id}/memory/settings",
    tag = "memory",
    security(("bearer" = [])),
    params(("id" = Uuid, Path)),
    request_body = BotMemorySettingsInput,
    responses(
        (status = 200, body = BotMemorySettingsDto),
        (status = 400, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody)
    )
)]
pub(crate) async fn put_bot_memory_settings(
    State(s): State<AppState>,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(input): ApiJson<BotMemorySettingsInput>,
) -> Result<Json<BotMemorySettingsDto>, ApiError> {
    let bot = s.bot_of_user(BotId(id))?;
    let mode = match input.mode.as_str() {
        "on" => MemoryMode::On,
        "off" => MemoryMode::Off,
        "ask" => MemoryMode::Ask,
        _ => return Err(ApiError::bad_request("mode must be on, off or ask")),
    };
    let next = MemorySettings {
        mode,
        budget_tokens: input.budget_tokens,
        share_with_other_bots: input.share_with_other_bots,
    };
    next.validate().map_err(ApiError::bad_request)?;
    let (row, version) = s
        .storage()
        .get_bot_versioned(bot.id)?
        .ok_or_else(|| ApiError::not_found("bot"))?;
    let mut config = row.config.clone();
    let obj = config
        .as_object_mut()
        .ok_or_else(|| ApiError::bad_request("bot config must be an object"))?;
    obj.insert(
        "memory".into(),
        serde_json::to_value(&next).map_err(|_| ApiError::internal("memory settings"))?,
    );
    s.storage().append_audit(
        s.user_id(),
        None,
        "user",
        "memory.settings",
        &json!({
            "bot_id": bot.id.0,
            "mode": input.mode,
            "budget": input.budget_tokens,
            "share": input.share_with_other_bots,
        }),
    )?;
    s.storage()
        .update_bot(bot.id, &row.name, &config, version)
        .map_err(|_| ApiError::conflict("the bot changed meanwhile; try again"))?;
    // The next turn reads the new switch from the bot row anyway; dropping the
    // cached profile also removes (or restores) the memory tools in the tool list.
    s.agent().forget_profile(bot.id);
    Ok(Json(settings_dto(
        &next,
        s.agent().memory_recall_budget_tokens(),
    )))
}
