//! "Smart (meaning)" memory recall: the embedding model the owner already set
//! up for a bot's knowledge, used for the owner's memories as well.
//!
//! * `PUT /v1/memory/embedding` (owner only) picks the bot whose knowledge
//!   embedding settings to reuse.  A model that runs on this computer sends
//!   nothing out; a remote provider needs `consent_send_text = true` because
//!   every memory text (secret-redacted first) is sent to it.
//! * The switch runs in the background: the vector table is rebuilt for the new
//!   dimension, memories are embedded in batches (progress is in
//!   `GET /v1/memory/embedding`), recall uses words only until it is done, and
//!   any failure falls back to Basic.
//! * Provider errors are replaced by fixed short messages: a provider body can
//!   carry URLs or tokens and must never reach the owner's screen or the DB.

use std::sync::Arc;
use std::time::Duration;

use axum::{Json, extract::State};
use memory_store::{EmbedMode, EmbedState, Embedder, EmbeddingStatus, MemoryError};
use protocol::BotId;
use provider_catalog::embedding::{EmbeddingInput, EmbeddingRequest, EmbeddingTask};
use provider_catalog::{EmbeddingProvider, ModelRef};
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::knowledge_settings::embedding_config;
use crate::{ApiError, AppState, error::ErrorBody, routes::ApiJson};

const CALL_TIMEOUT: Duration = Duration::from_secs(20);
const BATCH: usize = 16;
/// A re-embed sends every live memory once; refuse absurd stores.
const MAX_REEMBED: usize = 20_000;

/// Sync [`Embedder`] over an async [`EmbeddingProvider`].
pub(crate) struct ProviderEmbedder {
    provider: Arc<dyn EmbeddingProvider>,
    config: memory_store::knowledge::embedding::EmbeddingProfile,
    handle: tokio::runtime::Handle,
}

impl ProviderEmbedder {
    fn call(&self, texts: &[&str], task: EmbeddingTask) -> memory_store::Result<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(texts.len());
        for chunk in texts.chunks(BATCH) {
            let request = EmbeddingRequest {
                model: ModelRef(self.config.model.clone()),
                inputs: chunk
                    .iter()
                    .map(|t| EmbeddingInput {
                        text: (*t).to_owned(),
                        title: None,
                    })
                    .collect(),
                dimensions: self.config.dimensions,
                request_dimensions: self.config.request_dimensions,
                task,
                format: self.config.format,
            };
            let provider = Arc::clone(&self.provider);
            let handle = self.handle.clone();
            // The caller may sit on a runtime worker; a plain thread keeps
            // `block_on` legal and bounded by the timeout.
            let answer = std::thread::scope(|scope| {
                scope
                    .spawn(move || {
                        handle.block_on(async {
                            tokio::time::timeout(CALL_TIMEOUT, provider.embed(request)).await
                        })
                    })
                    .join()
            });
            match answer {
                Ok(Ok(Ok(response))) => out.extend(response.vectors),
                _ => {
                    return Err(MemoryError::Embed(
                        "the embedding model did not answer".into(),
                    ));
                }
            }
        }
        Ok(out)
    }
}

impl Embedder for ProviderEmbedder {
    fn dim(&self) -> usize {
        self.config.dimensions
    }

    fn embed(&self, texts: &[&str]) -> memory_store::Result<Vec<Vec<f32>>> {
        self.call(texts, EmbeddingTask::Document)
    }

    fn embed_queries(&self, texts: &[&str]) -> memory_store::Result<Vec<Vec<f32>>> {
        self.call(texts, EmbeddingTask::Query)
    }
}

/// What the settings screen shows about recall quality.
#[derive(Debug, Serialize, ToSchema)]
pub struct MemoryEmbeddingDto {
    /// `basic` (word match) or `smart` (meaning).
    pub mode: String,
    /// `ready`, `rebuilding`, `waiting` or `failed`.
    pub state: String,
    /// Model name in Smart mode; `Basic` otherwise.
    pub label: String,
    /// Memories embedded so far (while rebuilding).
    pub done: usize,
    /// Memories to embed in this rebuild.
    pub total: usize,
    /// Short reason when the last switch failed and recall fell back to Basic.
    pub error: Option<String>,
    /// Bots that have a knowledge embedding model Smart mode could reuse.
    pub sources: Vec<MemoryEmbeddingSourceDto>,
}

/// One bot whose knowledge embedding settings can be reused.
#[derive(Debug, Serialize, ToSchema)]
pub struct MemoryEmbeddingSourceDto {
    pub bot_id: Uuid,
    pub bot_name: String,
    pub model: String,
    pub provider_id: String,
    /// The model runs on this computer: nothing is sent anywhere.
    pub local: bool,
}

/// Switch recall mode (`PUT /v1/memory/embedding`).
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MemoryEmbeddingInput {
    /// `basic` or `smart`.
    pub mode: String,
    /// Smart: the bot whose knowledge embedding settings to reuse.
    #[serde(default)]
    pub from_bot: Option<Uuid>,
    /// Smart with a remote provider: the owner agrees that memory texts are sent to it.
    #[serde(default)]
    pub consent_send_text: bool,
}

fn status_dto(
    status: EmbeddingStatus,
    sources: Vec<MemoryEmbeddingSourceDto>,
) -> MemoryEmbeddingDto {
    MemoryEmbeddingDto {
        mode: match status.mode {
            EmbedMode::Basic => "basic",
            EmbedMode::Smart => "smart",
        }
        .to_owned(),
        state: match status.state {
            EmbedState::Ready => "ready",
            EmbedState::Rebuilding => "rebuilding",
            EmbedState::Waiting => "waiting",
            EmbedState::Failed => "failed",
        }
        .to_owned(),
        label: status.label,
        done: status.done,
        total: status.total,
        error: status.error,
        sources,
    }
}

fn sources(s: &AppState) -> Vec<MemoryEmbeddingSourceDto> {
    let Ok(bots) = s.storage().list_bots(s.user_id()) else {
        return Vec::new();
    };
    bots.into_iter()
        .filter_map(|bot| {
            let config = embedding_config(&bot).ok().flatten()?;
            Some(MemoryEmbeddingSourceDto {
                bot_id: bot.id.0,
                bot_name: bot.name,
                model: config.profile.model,
                provider_id: config.profile.provider_id,
                local: config.local_profile_id.is_some(),
            })
        })
        .collect()
}

#[utoipa::path(
    get,
    path = "/v1/memory/embedding",
    tag = "memory",
    security(("bearer" = [])),
    responses((status = 200, body = MemoryEmbeddingDto))
)]
pub(crate) async fn get_embedding(
    State(s): State<AppState>,
) -> Result<Json<MemoryEmbeddingDto>, ApiError> {
    let store = s.memory_store()?;
    let status = store
        .embedding_status()
        .map_err(|_| ApiError::internal("memory operation failed"))?;
    Ok(Json(status_dto(status, sources(&s))))
}

#[utoipa::path(
    put,
    path = "/v1/memory/embedding",
    tag = "memory",
    security(("bearer" = [])),
    request_body = MemoryEmbeddingInput,
    responses(
        (status = 202, body = MemoryEmbeddingDto),
        (status = 400, body = ErrorBody),
        (status = 409, body = ErrorBody)
    )
)]
pub(crate) async fn put_embedding(
    State(s): State<AppState>,
    ApiJson(input): ApiJson<MemoryEmbeddingInput>,
) -> Result<(axum::http::StatusCode, Json<MemoryEmbeddingDto>), ApiError> {
    let store = s.memory_store()?;
    let user = s.user_id();
    if !s.begin_memory_switch() {
        return Err(ApiError::conflict(
            "memory is already being re-indexed; wait until it is done",
        ));
    }
    // Held until the background job is started; released on every early return.
    let mut guard = SwitchGuard(Some(s.clone()));
    let job: Box<dyn FnOnce() + Send> = match input.mode.as_str() {
        "basic" => {
            s.storage().append_audit(
                user,
                None,
                "user",
                "memory.embedding",
                &json!({ "mode": "basic" }),
            )?;
            let task_store = Arc::clone(&store);
            Box::new(move || {
                let _ = task_store.enable_basic();
                task_store.set_source_note(None);
            })
        }
        "smart" => {
            let from = input.from_bot.ok_or_else(|| {
                ApiError::bad_request("choose which bot's embedding model to use")
            })?;
            let bot = s.bot_of_user(BotId(from))?;
            let config = embedding_config(&bot)?.ok_or_else(|| {
                ApiError::conflict(
                    "that bot has no embedding model yet; set one up under its knowledge settings first",
                )
            })?;
            let local = config.local_profile_id.is_some();
            if !local && !input.consent_send_text {
                return Err(ApiError::bad_request(
                    "consent is required: your memories would be sent to the embedding provider",
                ));
            }
            if store
                .count_live(user)
                .map_err(|_| ApiError::internal("memory operation failed"))?
                > MAX_REEMBED
            {
                return Err(ApiError::conflict(
                    "too many memories to re-index at once; forget some first",
                ));
            }
            let provider = crate::knowledge_runtime::provider(&s, &config)?;
            let label = format!("{}/{}", config.profile.provider_id, config.profile.model);
            s.storage().append_audit(
                user,
                None,
                "user",
                "memory.embedding",
                &json!({ "mode": "smart", "model": label, "remote": !local }),
            )?;
            let embedder: Arc<dyn Embedder> = Arc::new(ProviderEmbedder {
                provider,
                config: config.profile.clone(),
                handle: tokio::runtime::Handle::current(),
            });
            let note = json!({ "bot": from, "consent": input.consent_send_text || local });
            let task_store = Arc::clone(&store);
            Box::new(move || {
                // Remembered so a restart can bring the Smart model back.
                task_store.set_source_note(Some(&note.to_string()));
                let _ = task_store.enable_smart(embedder, &label);
            })
        }
        _ => return Err(ApiError::bad_request("mode must be basic or smart")),
    };
    let done = guard.0.take();
    tokio::task::spawn_blocking(move || {
        job();
        if let Some(s) = done {
            s.end_memory_switch();
        }
    });
    let status = store
        .embedding_status()
        .map_err(|_| ApiError::internal("memory operation failed"))?;
    Ok((
        axum::http::StatusCode::ACCEPTED,
        Json(status_dto(status, sources(&s))),
    ))
}

/// Releases the "one switch at a time" guard on an early return.
struct SwitchGuard(Option<AppState>);

impl Drop for SwitchGuard {
    fn drop(&mut self) {
        if let Some(s) = self.0.take() {
            s.end_memory_switch();
        }
    }
}

/// After a restart: bring the Smart model back (the index on disk is kept) or,
/// if its bot, model or consent is gone, fall back to Basic.  Runs off the
/// async runtime; never blocks startup.
pub(crate) fn restore_smart(s: &AppState) {
    let Ok(store) = s.memory_store() else { return };
    let Ok(status) = store.embedding_status() else {
        return;
    };
    if status.mode != EmbedMode::Smart || status.state != EmbedState::Waiting {
        return;
    }
    let state = s.clone();
    let handle = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        if !state.begin_memory_switch() {
            return;
        }
        let restored = (|| {
            let note: serde_json::Value = serde_json::from_str(&store.source_note()?).ok()?;
            let bot_id = Uuid::parse_str(note["bot"].as_str()?).ok()?;
            let bot = state.bot_of_user(BotId(bot_id)).ok()?;
            let config = embedding_config(&bot).ok().flatten()?;
            let local = config.local_profile_id.is_some();
            if !local && note["consent"].as_bool() != Some(true) {
                return None;
            }
            let provider = crate::knowledge_runtime::provider(&state, &config).ok()?;
            let label = format!("{}/{}", config.profile.provider_id, config.profile.model);
            let embedder: Arc<dyn Embedder> = Arc::new(ProviderEmbedder {
                provider,
                config: config.profile,
                handle,
            });
            store.enable_smart(embedder, &label).ok()
        })();
        if restored.is_none() {
            // The saved source is gone: do not leave recall waiting forever.
            let _ = store.enable_basic();
        }
        state.end_memory_switch();
    });
}
