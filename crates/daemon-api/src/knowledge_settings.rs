//! Knowledge configuration authored by the authenticated user, never by a
//! retrieved file. Provider endpoints and quota groups are resolved server-side.

use axum::{
    Json,
    extract::{Path, State},
};
use memory_store::knowledge::{
    KnowledgeStore, Scope,
    embedding::{EmbeddingConfig, EmbeddingProfile, local_embeddings_available},
};
use protocol::BotId;
use provider_catalog::{EmbeddingTaskFormat, ProviderConfig, ProviderKind};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::knowledge_runtime::{EmbeddingApprovalChallenge, EmbeddingGrantTarget};
use crate::{ApiError, AppState, error::ErrorBody, routes::ApiJson};

#[derive(Debug, Serialize, ToSchema)]
pub struct KnowledgeSettingsDto {
    pub auto_retrieve: bool,
    pub auto_retrieval_available: bool,
    pub embedding: Option<KnowledgeEmbeddingDto>,
    pub local_embeddings_available: bool,
    pub local_profiles: Vec<KnowledgeLocalProfileDto>,
}

/// A host-installed local model. Native file locations are never API inputs.
#[derive(Debug, Serialize, ToSchema)]
pub struct KnowledgeLocalProfileDto {
    pub id: String,
    pub label: String,
    pub model: String,
    pub dimensions: usize,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct KnowledgeEmbeddingDto {
    pub provider_id: String,
    pub model: String,
    pub dimensions: usize,
    pub request_dimensions: bool,
    pub endpoint: String,
    pub profile_fingerprint: String,
    pub query_remote: bool,
    pub requests_per_minute: u32,
    pub requests_per_day: u32,
    pub local_profile_id: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeEmbeddingInput {
    pub provider_id: String,
    pub model: String,
    pub dimensions: usize,
    #[serde(default)]
    pub request_dimensions: bool,
    pub requests_per_minute: u32,
    pub requests_per_day: u32,
    /// Select an already installed host profile; cannot choose native files.
    pub local_profile_id: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeSettingsInput {
    pub auto_retrieve: bool,
    /// Null explicitly disables embeddings; lexical retrieval remains available.
    pub embedding: Option<KnowledgeEmbeddingInput>,
}

pub(crate) fn embedding_config(bot: &storage::Bot) -> Result<Option<EmbeddingConfig>, ApiError> {
    let value = &bot.config["knowledge"]["embedding"];
    if value.is_null() {
        return Ok(None);
    }
    let config: EmbeddingConfig = serde_json::from_value(value.clone())
        .map_err(|_| ApiError::conflict("invalid knowledge embedding settings; save them again"))?;
    config
        .validate()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    Ok(Some(config))
}

fn profile_fingerprint(config: &EmbeddingConfig) -> Result<String, ApiError> {
    config
        .profile
        .fingerprint()
        .map_err(|e| ApiError::bad_request(e.to_string()))
}

fn settings_dto(s: &AppState, bot: &storage::Bot) -> Result<KnowledgeSettingsDto, ApiError> {
    let embedding = embedding_config(bot)?
        .map(|config| -> Result<KnowledgeEmbeddingDto, ApiError> {
            let fingerprint = profile_fingerprint(&config)?;
            Ok(KnowledgeEmbeddingDto {
                provider_id: config.profile.provider_id,
                model: config.profile.model,
                dimensions: config.profile.dimensions,
                request_dimensions: config.profile.request_dimensions,
                endpoint: config.profile.endpoint_identity,
                query_remote: crate::knowledge_runtime::authorized(
                    s,
                    bot.id,
                    &EmbeddingGrantTarget::Queries,
                    &fingerprint,
                )
                .is_some(),
                profile_fingerprint: fingerprint,
                requests_per_minute: config.budget.requests_per_minute,
                requests_per_day: config.budget.requests_per_day,
                local_profile_id: config.local_profile_id,
            })
        })
        .transpose()?;
    Ok(KnowledgeSettingsDto {
        auto_retrieve: agent_core::knowledge_auto_retrieval_available()
            && bot.config["knowledge"]["auto_retrieve"]
                .as_bool()
                .unwrap_or(false),
        embedding,
        auto_retrieval_available: agent_core::knowledge_auto_retrieval_available(),
        local_embeddings_available: local_embeddings_available(),
        local_profiles: crate::knowledge_runtime::list_local_profiles(s)?
            .into_iter()
            .map(|entry| KnowledgeLocalProfileDto {
                id: entry.id,
                label: entry.label,
                model: entry.profile.model,
                dimensions: entry.profile.dimensions,
            })
            .collect(),
    })
}

fn configured_provider(
    s: &AppState,
    id: &str,
) -> Result<
    (
        provider_catalog::ProviderSettings,
        provider_catalog::ProviderEntry,
    ),
    ApiError,
> {
    let providers = ProviderConfig::load(&provider_catalog::config_path(&s.data_dir()))
        .map_err(|_| ApiError::internal("provider settings unavailable"))?;
    let settings = providers
        .get(id)
        .filter(|p| p.enabled && p.cli.is_none())
        .cloned()
        .ok_or_else(|| ApiError::bad_request("choose an enabled HTTP provider"))?;
    let entry = settings
        .custom
        .clone()
        .or_else(|| s.provider_host().and_then(|h| h.catalog.get(id)).cloned())
        .ok_or_else(|| ApiError::bad_request("provider is not in the configured catalog"))?;
    if !matches!(
        entry.kind,
        ProviderKind::Gemini | ProviderKind::OpenAiCompatible
    ) {
        return Err(ApiError::bad_request(
            "provider does not support embeddings",
        ));
    }
    Ok((settings, entry))
}

fn make_embedding(
    s: &AppState,
    input: KnowledgeEmbeddingInput,
) -> Result<EmbeddingConfig, ApiError> {
    if input.provider_id == "local-onnx" {
        let id = input
            .local_profile_id
            .ok_or_else(|| ApiError::bad_request("choose an installed local embedding profile"))?;
        let profile = crate::knowledge_runtime::resolve_local_profile(s, &id)?;
        let mut config = EmbeddingConfig::new(profile, "local-host");
        config.local_profile_id = Some(id);
        config
            .validate()
            .map_err(|e| ApiError::bad_request(e.to_string()))?;
        return Ok(config);
    }
    if input.local_profile_id.is_some() {
        return Err(ApiError::bad_request(
            "local profiles require the local-onnx provider",
        ));
    }
    let (settings, entry) = configured_provider(s, &input.provider_id)?;
    let endpoint = provider_catalog::embedding::endpoint_identity(&entry.base_url)
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    let format = if entry.kind == ProviderKind::Gemini {
        match input.model.as_str() {
            "gemini-embedding-001" => EmbeddingTaskFormat::Gemini001,
            "gemini-embedding-2" | "gemini-embedding-2-preview" => EmbeddingTaskFormat::Gemini2,
            _ => {
                return Err(ApiError::bad_request(
                    "unsupported Gemini embedding model; choose gemini-embedding-001, gemini-embedding-2 or gemini-embedding-2-preview",
                ));
            }
        }
    } else {
        EmbeddingTaskFormat::Plain
    };
    let profile = EmbeddingProfile {
        provider_id: settings.id.clone(),
        model: input.model,
        model_revision: None,
        endpoint_identity: endpoint,
        dimensions: input.dimensions,
        request_dimensions: input.request_dimensions,
        format,
    };
    // Aliases of one saved credential debit a single host-wide quota. Only a
    // name is hashed; secret bytes are never read here. Without a saved handle,
    // the provider identity is the available conservative local grouping key.
    let account = settings.key.as_ref().map_or_else(
        || format!("environment-provider:{}", settings.id),
        |reference| format!("secret-ref:{}", reference.name()),
    );
    let mut config =
        EmbeddingConfig::new(profile, format!("{:x}", Sha256::digest(account.as_bytes())));
    config.budget.requests_per_minute = input.requests_per_minute;
    config.budget.requests_per_day = input.requests_per_day;
    config
        .validate()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    Ok(config)
}

/// Display only the current configured provider/model/destination identity.
pub(crate) fn remote_fingerprint(s: &AppState, bot: Uuid) -> Result<String, ApiError> {
    let bot = s.bot_of_user(BotId(bot))?;
    let config = embedding_config(&bot)?
        .ok_or_else(|| ApiError::conflict("configure an embedding provider first"))?;
    if config.local_profile_id.is_some() {
        return Err(ApiError::bad_request(
            "local embeddings do not need a remote source grant",
        ));
    }
    let (_, entry) = configured_provider(s, &config.profile.provider_id)?;
    let endpoint = provider_catalog::embedding::endpoint_identity(&entry.base_url)
        .map_err(|_| ApiError::bad_request("invalid embedding endpoint"))?;
    if endpoint != config.profile.endpoint_identity {
        return Err(ApiError::conflict(
            "provider destination changed; save the embedding settings again",
        ));
    }
    profile_fingerprint(&config)
}

#[utoipa::path(get, path = "/v1/bots/{bot}/knowledge/settings", tag = "knowledge", security(("bearer" = [])), params(("bot" = Uuid, Path)), responses((status = 200, body = KnowledgeSettingsDto), (status = 404, body = ErrorBody)))]
pub(crate) async fn get_settings(
    State(s): State<AppState>,
    Path(bot): Path<Uuid>,
) -> Result<Json<KnowledgeSettingsDto>, ApiError> {
    crate::knowledge::read_blocking(move || {
        Ok(Json(settings_dto(&s, &s.bot_of_user(BotId(bot))?)?))
    })
    .await
}

#[utoipa::path(put, path = "/v1/bots/{bot}/knowledge/settings", tag = "knowledge", security(("bearer" = [])), params(("bot" = Uuid, Path)), request_body = KnowledgeSettingsInput, responses((status = 200, body = KnowledgeSettingsDto), (status = 400, body = ErrorBody)))]
pub(crate) async fn put_settings(
    State(s): State<AppState>,
    Path(bot): Path<Uuid>,
    ApiJson(input): ApiJson<KnowledgeSettingsInput>,
) -> Result<Json<KnowledgeSettingsDto>, ApiError> {
    s.bot_of_user(BotId(bot))?;
    if input.auto_retrieve && !agent_core::knowledge_auto_retrieval_available() {
        return Err(ApiError::conflict(
            "automatic retrieval is unavailable until native untrusted-read enforcement is installed",
        ));
    }
    let authority = crate::knowledge::try_authority(&s, BotId(bot))?;
    crate::knowledge_runtime::quiesce_bot(&s, BotId(bot)).await?;
    crate::knowledge::blocking(s.data_dir(), move || {
        let _authority = authority;
        let mut bot = s.bot_of_user(BotId(bot))?;
        let previous = embedding_config(&bot)
            .ok()
            .flatten()
            .as_ref()
            .map(profile_fingerprint)
            .transpose()?;
        let embedding = input
            .embedding
            .map(|settings| make_embedding(&s, settings))
            .transpose()?;
        let fingerprint = embedding.as_ref().map(profile_fingerprint).transpose()?;
        if embedding.is_some() {
            // Explicit configuration initializes the index identity used by
            // owner approval cards even before the first source is selected.
            KnowledgeStore::open(
                s.data_dir(),
                Scope {
                    user_id: bot.user_id,
                    bot_id: bot.id,
                },
            )
            .map_err(|_| ApiError::internal("knowledge index unavailable"))?;
        }
        if previous != fingerprint {
            crate::knowledge_runtime::revoke_bot_grants(&s, bot.id)?;
            // Old source metadata is inert without its host grant. Retain paid
            // vectors until a replacement profile generation completes.
        }
        let knowledge = json!({"auto_retrieve":input.auto_retrieve,"embedding":embedding});
        // Only the knowledge subtree is written, against the stored profile in
        // one transaction, so a concurrent profile edit is never overwritten
        // with this request's older copy; the bot revision still advances.
        match s.storage().modify_bot(bot.id, |name, stored| {
            let mut next = stored.clone();
            next["knowledge"] = knowledge.clone();
            Ok((name.to_owned(), next))
        })? {
            storage::BotPatch::Applied { bot: saved, .. } => bot = saved,
            _ => return Err(ApiError::not_found("bot")),
        }
        crate::knowledge_runtime::invalidate_bot(&s, bot.id);
        s.refresh_profile(&bot);
        Ok(Json(settings_dto(&s, &bot)?))
    })
    .await
}

/// Ask policy for an owner-reviewable destination and source snapshot. Merely
/// opening the card grants no authority and starts no provider request.
#[utoipa::path(post, path = "/v1/bots/{bot}/knowledge/embedding-approvals", tag = "knowledge", security(("bearer" = [])), params(("bot" = Uuid, Path)), request_body = EmbeddingGrantTarget, responses((status = 200, body = EmbeddingApprovalChallenge), (status = 403, body = ErrorBody), (status = 409, body = ErrorBody)))]
pub(crate) async fn request_embedding_approval(
    State(s): State<AppState>,
    Path(bot): Path<Uuid>,
    ApiJson(target): ApiJson<EmbeddingGrantTarget>,
) -> Result<Json<EmbeddingApprovalChallenge>, ApiError> {
    s.bot_of_user(BotId(bot))?;
    let authority = crate::knowledge::try_authority(&s, BotId(bot))?;
    crate::knowledge::blocking(s.data_dir(), move || {
        let _authority = authority;
        Ok(Json(crate::knowledge_runtime::grant_request(
            &s,
            BotId(bot),
            target,
        )?))
    })
    .await
}

/// Confirm exactly the server snapshot the local owner reviewed. A changed
/// profile/source, expired card or replay requires a fresh owner decision.
#[utoipa::path(post, path = "/v1/bots/{bot}/knowledge/embedding-approvals/{approval}/confirm", tag = "knowledge", security(("bearer" = [])), params(("bot" = Uuid, Path), ("approval" = Uuid, Path)), responses((status = 204), (status = 403, body = ErrorBody), (status = 409, body = ErrorBody)))]
pub(crate) async fn confirm_embedding_approval(
    State(s): State<AppState>,
    Path((bot, approval)): Path<(Uuid, Uuid)>,
) -> Result<axum::http::StatusCode, ApiError> {
    s.bot_of_user(BotId(bot))?;
    let authority = crate::knowledge::try_authority(&s, BotId(bot))?;
    crate::knowledge::blocking(s.data_dir(), move || {
        let _authority = authority;
        crate::knowledge_runtime::grant_confirm(&s, BotId(bot), approval)?;
        Ok(axum::http::StatusCode::NO_CONTENT)
    })
    .await
}

#[utoipa::path(delete, path = "/v1/bots/{bot}/knowledge/embedding-grants", tag = "knowledge", security(("bearer" = [])), params(("bot" = Uuid, Path)), request_body = EmbeddingGrantTarget, responses((status = 204), (status = 404, body = ErrorBody)))]
pub(crate) async fn revoke_embedding_grant(
    State(s): State<AppState>,
    Path(bot): Path<Uuid>,
    ApiJson(target): ApiJson<EmbeddingGrantTarget>,
) -> Result<axum::http::StatusCode, ApiError> {
    s.bot_of_user(BotId(bot))?;
    let authority = crate::knowledge::try_authority(&s, BotId(bot))?;
    crate::knowledge_runtime::quiesce_bot(&s, BotId(bot)).await?;
    crate::knowledge::blocking(s.data_dir(), move || {
        let _authority = authority;
        crate::knowledge_runtime::revoke_grant(&s, BotId(bot), target)?;
        Ok(axum::http::StatusCode::NO_CONTENT)
    })
    .await
}

/// Explicit owner recovery for a provider's shared retry delay; quota usage and
/// per-source failure records remain intact.
#[utoipa::path(post, path = "/v1/bots/{bot}/knowledge/embedding-cooldown/reset", tag = "knowledge", security(("bearer" = [])), params(("bot" = Uuid, Path)), responses((status = 204), (status = 409, body = ErrorBody)))]
pub(crate) async fn reset_embedding_cooldown(
    State(s): State<AppState>,
    Path(bot): Path<Uuid>,
) -> Result<axum::http::StatusCode, ApiError> {
    s.bot_of_user(BotId(bot))?;
    let authority = crate::knowledge::try_authority(&s, BotId(bot))?;
    crate::knowledge::blocking(s.data_dir(), move || {
        let _authority = authority;
        crate::knowledge_runtime::reset_embedding_cooldown(&s, BotId(bot))?;
        Ok(axum::http::StatusCode::NO_CONTENT)
    })
    .await
}
