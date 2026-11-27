//! Host-owned embedding dispatch. Tool arguments never select a provider or
//! grant egress. Per-bot admission lanes are released before network I/O;
//! revocation cancels the admitted generation and blocks stale publication.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

mod authority;
pub(crate) use authority::authorized;
mod local_host;
pub use authority::{
    EmbeddingApprovalChallenge, EmbeddingGrantTarget, grant_confirm, grant_request,
    revoke_bot_grants, revoke_grant,
};
pub use local_host::{HostLocalProfile, list_local_profiles, resolve_local_profile};

use async_trait::async_trait;
use builtin_tools::knowledge::{KnowledgeBackend, LocalKnowledgeBackend};
use memory_store::knowledge::embedding::{
    EmbeddingAuthorizer, EmbeddingConfig, EmbeddingProfile, EmbeddingProgress, EmbeddingRuntime,
    KnowledgeEmbeddingService,
};
use memory_store::knowledge::{KnowledgeStore, Scope, SearchMode, SearchRequest, SearchResponse};
use protocol::BotId;
use provider_catalog::{
    EmbeddingProvider, EmbeddingRequest, EmbeddingResponse, OpenAiCompatConfig, ProviderConfig,
    ProviderError,
};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, Notify, OwnedMutexGuard};
use tokio_util::sync::CancellationToken;
use tool_registry::ToolError;
use uuid::Uuid;

use crate::{ApiError, AppState, WeakAppState};

type Service = Arc<KnowledgeEmbeddingService>;
type Prepared = (Service, Activity);
type AuthorityLanes = parking_lot::Mutex<BTreeMap<(PathBuf, BotId), Arc<Lane>>>;
struct Lane {
    gate: Arc<Mutex<()>>,
    cancel: parking_lot::Mutex<CancellationToken>,
    active: AtomicUsize,
    changed: Notify,
}
struct Activity {
    lane: Arc<Lane>,
    cancel: CancellationToken,
}
impl Activity {
    fn begin(lane: Arc<Lane>) -> Self {
        lane.active.fetch_add(1, Ordering::SeqCst);
        let cancel = lane.cancel.lock().clone();
        Self { lane, cancel }
    }
}
impl Drop for Activity {
    fn drop(&mut self) {
        self.lane.active.fetch_sub(1, Ordering::SeqCst);
        self.lane.changed.notify_waiters();
    }
}

struct CachedService {
    bot: BotId,
    key: String,
    service: Service,
}

/// One admission lane per bot. It is never held during a provider request.
fn lane(state: &AppState, bot: BotId) -> Arc<Lane> {
    static LANES: OnceLock<AuthorityLanes> = OnceLock::new();
    let path = state.data_dir();
    let path = std::fs::canonicalize(&path).unwrap_or(path);
    let mut lanes = LANES.get_or_init(AuthorityLanes::default).lock();
    Arc::clone(lanes.entry((path, bot)).or_insert_with(|| {
        Arc::new(Lane {
            gate: Arc::new(Mutex::new(())),
            cancel: parking_lot::Mutex::new(state.shutdown_token().child_token()),
            active: AtomicUsize::new(0),
            changed: Notify::new(),
        })
    }))
}
pub(crate) fn authority_lock(state: &AppState, bot: BotId) -> Arc<Mutex<()>> {
    Arc::clone(&lane(state, bot).gate)
}
/// Cancel admitted work and wait for database/model users to release it.
/// Call while holding the bot authority lane, before deleting its files.
///
/// # Errors
/// A blocking worker did not release its handles within the bounded deadline.
pub async fn quiesce_bot(state: &AppState, bot: BotId) -> Result<(), ApiError> {
    invalidate_bot(state, bot);
    let lane = lane(state, bot);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let notified = lane.changed.notified();
            if lane.active.load(Ordering::SeqCst) == 0 {
                break;
            }
            notified.await;
        }
    })
    .await
    .map_err(|_| ApiError::conflict("knowledge worker is still releasing its handles"))
}

/// Whether no search or embedding work of this bot is running right now.
/// Unlike [`quiesce_bot`] this cancels nothing: background jobs use it to wait
/// their turn instead of failing a run that is in progress.
pub(crate) fn bot_is_idle(state: &AppState, bot: BotId) -> bool {
    lane(state, bot).active.load(Ordering::SeqCst) == 0
}

/// Pretend a knowledge search of `bot` is running; returns its cancel token.
#[cfg(test)]
pub(crate) fn test_begin_activity(
    state: &AppState,
    bot: BotId,
) -> (impl Drop + use<>, CancellationToken) {
    let activity = Activity::begin(lane(state, bot));
    let cancel = activity.cancel.clone();
    (activity, cancel)
}

#[cfg(test)]
pub(crate) async fn test_guard() -> OwnedMutexGuard<()> {
    static LOCK: OnceLock<Arc<Mutex<()>>> = OnceLock::new();
    Arc::clone(LOCK.get_or_init(|| Arc::new(Mutex::new(()))))
        .lock_owned()
        .await
}

pub(crate) struct KnowledgeRuntime {
    state: WeakAppState,
    local: LocalKnowledgeBackend,
    services: Arc<parking_lot::Mutex<VecDeque<CachedService>>>,
}

pub(crate) fn install(state: &AppState) {
    let runtime = Arc::new(KnowledgeRuntime {
        state: state.downgrade(),
        local: LocalKnowledgeBackend::new(state.storage_arc()),
        services: Arc::default(),
    });
    if state.0.knowledge_runtime.set(Arc::clone(&runtime)).is_ok() {
        let tools = state.agent().tool_registry();
        tools.unregister(builtin_tools::knowledge::KNOWLEDGE_SEARCH);
        tools.register(builtin_tools::knowledge::tool(runtime.clone()));
        tools.unregister(builtin_tools::knowledge::DOCUMENTS_SEARCH);
        tools.register(builtin_tools::knowledge::documents_tool(runtime));
        tools.unregister(builtin_tools::knowledge_brain::KNOWLEDGE_BRAIN_WRITE);
        tools.register(builtin_tools::knowledge_brain::tool(state.storage_arc()));
    }
}

/// Invalidate cached and in-flight work after settings or source authority changes.
pub(crate) fn invalidate_bot(state: &AppState, bot: BotId) {
    let lane = lane(state, bot);
    let mut cancel = lane.cancel.lock();
    cancel.cancel();
    *cancel = state.shutdown_token().child_token();
    drop(cancel);
    evict_bot(state, bot);
}
fn evict_bot(state: &AppState, bot: BotId) {
    if let Some(runtime) = state.0.knowledge_runtime.get() {
        runtime.services.lock().retain(|entry| entry.bot != bot);
    }
}
/// Drop the current cached/admitted embedding generation.
pub fn forget_bot(state: &AppState, bot: BotId) {
    invalidate_bot(state, bot);
}

fn configured_settings(state: &AppState, bot: BotId) -> Result<EmbeddingConfig, ApiError> {
    let bot = state.bot_of_user(bot)?;
    let value = bot
        .config
        .get("knowledge")
        .and_then(|value| value.get("embedding"))
        .filter(|value| !value.is_null())
        .ok_or_else(|| ApiError::conflict("no embedding provider is configured"))?;
    let mut config: EmbeddingConfig = serde_json::from_value(value.clone())
        .map_err(|_| ApiError::bad_request("invalid embedding settings"))?;
    // A portable config can choose only an installed host profile ID.
    if let Some(id) = &config.local_profile_id {
        config.profile = resolve_local_profile(state, id)?;
        config.account_group = "local-host".into();
    } else if config.profile.endpoint_identity.starts_with("local-onnx:") {
        return Err(ApiError::bad_request(
            "local embedding requires an installed host profile ID",
        ));
    } else {
        let _lock = state.providers_lock();
        let providers = ProviderConfig::load(&provider_catalog::config_path(&state.data_dir()))
            .map_err(|_| ApiError::unavailable("embedding providers unavailable"))?;
        let settings = providers
            .get(&config.profile.provider_id)
            .ok_or_else(|| ApiError::conflict("embedding provider is unavailable"))?;
        let entry = settings
            .custom
            .as_ref()
            .or_else(|| {
                state
                    .provider_host()?
                    .catalog
                    .get(&config.profile.provider_id)
            })
            .ok_or_else(|| ApiError::conflict("embedding provider is unavailable"))?;
        let identity = if let Some(key) = &settings.key {
            format!("secret:{}", key.name())
        } else {
            match &entry.auth {
                provider_catalog::Auth::ApiKeyEnv(name) => format!("env:{name}"),
                provider_catalog::Auth::None => {
                    format!("anonymous:{}", config.profile.endpoint_identity)
                }
            }
        };
        config.account_group = format!("host-account:{:x}", Sha256::digest(identity.as_bytes()));
    }
    config.query_consent = None;
    config
        .validate()
        .map_err(|_| ApiError::bad_request("invalid embedding settings"))?;
    Ok(config)
}
fn settings(state: &AppState, bot: BotId) -> Result<EmbeddingConfig, ApiError> {
    let mut config = configured_settings(state, bot)?;
    let fingerprint = config
        .profile
        .fingerprint()
        .map_err(|_| ApiError::bad_request("invalid embedding profile"))?;
    config.query_consent = authorized(state, bot, &EmbeddingGrantTarget::Queries, &fingerprint);
    Ok(config)
}

struct HostAuthority {
    state: WeakAppState,
    bot: BotId,
    cancel: CancellationToken,
}
impl EmbeddingAuthorizer for HostAuthority {
    fn allows_document(&self, source: Uuid, profile: &str) -> bool {
        self.allows(
            &EmbeddingGrantTarget::Documents { source_id: source },
            profile,
        )
    }
    fn allows_query(&self, profile: &str) -> bool {
        self.allows(&EmbeddingGrantTarget::Queries, profile)
    }
}
impl HostAuthority {
    fn allows(&self, target: &EmbeddingGrantTarget, profile: &str) -> bool {
        if self.cancel.is_cancelled() {
            return false;
        }
        let Some(state) = self.state.upgrade() else {
            return false;
        };
        let Ok(config) = configured_settings(&state, self.bot) else {
            return false;
        };
        config
            .profile
            .fingerprint()
            .is_ok_and(|current| current == profile)
            && authorized(&state, self.bot, target, profile).is_some()
    }
}

impl KnowledgeRuntime {
    async fn prepare(
        &self,
        state: AppState,
        bot: BotId,
        gate: OwnedMutexGuard<()>,
    ) -> Result<Prepared, ApiError> {
        let config = settings(&state, bot).inspect_err(|_| evict_bot(&state, bot))?;
        let activity = Activity::begin(lane(&state, bot));
        let bytes = serde_json::to_vec(&config)
            .map_err(|_| ApiError::bad_request("invalid embedding settings"))?;
        let key = format!("{:x}", Sha256::digest(bytes));
        if let Some(service) = self
            .services
            .lock()
            .iter()
            .find(|entry| entry.bot == bot && entry.key == key)
            .map(|entry| Arc::clone(&entry.service))
        {
            drop(gate);
            return Ok((service, activity));
        }
        let services = Arc::clone(&self.services);
        tokio::task::spawn_blocking(move || {
            // Release the previous model generation before loading another.
            evict_bot(&state, bot);
            let provider = provider(&state, &config)?;
            let store = Arc::new(
                KnowledgeStore::open(
                    state.data_dir(),
                    Scope {
                        user_id: state.user_id(),
                        bot_id: bot,
                    },
                )
                .map_err(|_| ApiError::unavailable("knowledge index unavailable"))?,
            );
            let ledger = Arc::new(
                EmbeddingRuntime::open(state.data_dir())
                    .map_err(|_| ApiError::unavailable("embedding budget ledger unavailable"))?,
            );
            let service = Arc::new(
                KnowledgeEmbeddingService::new(store, provider, ledger, config)
                    .map_err(|_| ApiError::bad_request("embedding profile is unavailable"))?
                    .with_authorizer(Arc::new(HostAuthority {
                        state: state.downgrade(),
                        bot,
                        cancel: activity.cancel.clone(),
                    })),
            );
            let mut cache = services.lock();
            while cache.len() >= 16 {
                cache.pop_front();
            }
            cache.push_back(CachedService {
                bot,
                key,
                service: Arc::clone(&service),
            });
            drop(gate);
            Ok((service, activity))
        })
        .await
        .map_err(|_| ApiError::unavailable("embedding worker unavailable"))?
    }
}

#[async_trait]
impl KnowledgeBackend for KnowledgeRuntime {
    fn may_egress(&self, bot: BotId) -> bool {
        self.state
            .upgrade()
            .and_then(|state| settings(&state, bot).ok())
            .is_some_and(|config| {
                !config.profile.endpoint_identity.starts_with("local-onnx:")
                    && config.query_consent.is_some()
            })
    }
    fn has_enabled_sources(&self, bot: BotId) -> bool {
        self.local.has_enabled_sources(bot)
    }
    fn has_enabled_document_sources(&self, bot: BotId) -> bool {
        self.local.has_enabled_document_sources(bot)
    }

    async fn search(
        &self,
        bot: BotId,
        request: SearchRequest,
    ) -> Result<SearchResponse, ToolError> {
        self.search_inner(bot, request, None).await
    }
    async fn search_for_run(
        &self,
        bot: BotId,
        request: SearchRequest,
        run_id: &str,
    ) -> Result<SearchResponse, ToolError> {
        self.search_inner(bot, request, Some(run_id)).await
    }
}

impl KnowledgeRuntime {
    async fn search_inner(
        &self,
        bot: BotId,
        request: SearchRequest,
        run_id: Option<&str>,
    ) -> Result<SearchResponse, ToolError> {
        let state = self
            .state
            .upgrade()
            .ok_or_else(|| ToolError::Execution("knowledge host unavailable".into()))?;
        state
            .bot_of_user(bot)
            .map_err(|_| ToolError::Execution("knowledge bot unavailable".into()))?;
        let configured = state
            .bot_of_user(bot)
            .ok()
            .and_then(|bot| {
                bot.config
                    .get("knowledge")
                    .and_then(|value| value.get("embedding"))
                    .cloned()
            })
            .is_some_and(|value| !value.is_null());
        if configured {
            let gate = authority_lock(&state, bot).lock_owned().await;
            if let Ok((service, activity)) = self.prepare(state.clone(), bot, gate).await {
                // Cancellation stops dispatch/scan, but the detached bounded
                // worker owns Activity until its SQLite/model users finish.
                // A dropped HTTP/tool waiter must not let deletion race them.
                let cancel = activity.cancel.child_token();
                let _cancel_on_drop = cancel.clone().drop_guard();
                let search_state = state.clone();
                let search_request = request.clone();
                let run_id = run_id.map(str::to_owned);
                let result = tokio::spawn(async move {
                    let _activity = activity;
                    let query_guard = || {
                        run_id.as_deref().is_none_or(|run_id| {
                            let decision =
                                search_state.agent().embedding_egress_decision(bot, run_id);
                            decision != permission_engine::Decision::Deny
                                && decision.unsupported_limits().is_none()
                        })
                    };
                    service
                        .search_with_query_guard(&search_request, &cancel, &query_guard)
                        .await
                })
                .await;
                if let Ok(Ok(response)) = result {
                    return Ok(response);
                }
            }
        }
        let mut response = self.local.search(bot, request).await?;
        if configured {
            response.mode = SearchMode::LexicalDegraded;
        }
        Ok(response)
    }
}

/// Execute one resumable source-embedding batch with the current owner settings.
///
/// # Errors
/// Unknown source/bot, stale revision, concurrent management, unavailable local
/// artifacts, or invalid settings. Provider response bodies are never returned.
pub async fn embedding_batch(
    state: &AppState,
    bot: BotId,
    source: Uuid,
    expected_revision: u64,
) -> Result<EmbeddingProgress, ApiError> {
    run_embedding_batch(state, bot, source, expected_revision, false).await
}

/// Explicit owner retry of a failed source. Automatic continuation must use
/// [`embedding_batch`] so it cannot repeatedly reset the bounded attempt count.
/// The durable account quota and provider cooldown are never reset here.
///
/// # Errors
/// The same ownership, revision and configuration errors as [`embedding_batch`].
pub async fn retry_embedding_batch(
    state: &AppState,
    bot: BotId,
    source: Uuid,
    expected_revision: u64,
) -> Result<EmbeddingProgress, ApiError> {
    run_embedding_batch(state, bot, source, expected_revision, true).await
}

async fn run_embedding_batch(
    state: &AppState,
    bot: BotId,
    source: Uuid,
    expected_revision: u64,
    reset_retry: bool,
) -> Result<EmbeddingProgress, ApiError> {
    state.bot_of_user(bot)?;
    let gate = authority_lock(state, bot).lock_owned().await;
    let runtime = state
        .0
        .knowledge_runtime
        .get()
        .ok_or_else(|| ApiError::unavailable("knowledge runtime unavailable"))?;
    let (service, activity) = runtime.prepare(state.clone(), bot, gate).await?;
    // Check the browser's revision before any egress; the embedding service
    // rechecks grants and chunk revisions before dispatch and publication.
    let store = KnowledgeStore::open(
        state.data_dir(),
        Scope {
            user_id: state.user_id(),
            bot_id: bot,
        },
    )
    .map_err(|_| ApiError::unavailable("knowledge index unavailable"))?;
    let current = store
        .source(source)
        .map_err(|_| ApiError::unavailable("knowledge index unavailable"))?
        .ok_or_else(|| ApiError::not_found("knowledge source"))?;
    if current.revision != expected_revision || !current.enabled {
        return Err(ApiError::conflict(
            "knowledge source changed or is disabled",
        ));
    }
    drop(store);
    if reset_retry {
        service
            .reset_source_retry(source)
            .map_err(|_| ApiError::unavailable("embedding retry state unavailable"))?;
    }
    let cancel = activity.cancel.child_token();
    let _cancel_on_drop = cancel.clone().drop_guard();
    tokio::spawn(async move {
        let _activity = activity;
        service.embed_source_batch(source, &cancel).await
    })
    .await
    .map_err(|_| ApiError::unavailable("embedding worker unavailable"))?
    .map_err(|_| ApiError::unavailable("embedding batch unavailable"))
}

pub(crate) fn provider(
    state: &AppState,
    config: &EmbeddingConfig,
) -> Result<Arc<dyn EmbeddingProvider>, ApiError> {
    if config.profile.endpoint_identity.starts_with("local-onnx:") {
        return local_provider(state, config);
    }
    // Fail closed on stale endpoint/provider settings, even for a cache miss.
    resolve_provider(state, &config.profile).map_err(|_| {
        ApiError::conflict("configured embedding provider changed or is unavailable")
    })?;
    Ok(Arc::new(ConfiguredProvider {
        state: state.downgrade(),
        profile: config.profile.clone(),
    }))
}

#[cfg(feature = "local-embeddings")]
fn local_provider(
    state: &AppState,
    config: &EmbeddingConfig,
) -> Result<Arc<dyn EmbeddingProvider>, ApiError> {
    let id = config
        .local_profile_id
        .as_deref()
        .ok_or_else(|| ApiError::bad_request("installed local profile is required"))?;
    let artifacts = local_host::artifacts(state, id)?;
    if artifacts
        .profile()
        .map_err(|_| ApiError::bad_request("invalid local profile"))?
        != config.profile
    {
        return Err(ApiError::conflict("host local embedding profile changed"));
    }
    let provider = memory_store::knowledge::embedding::LocalOnnxEmbedder::load(&artifacts)
        .map_err(|_| ApiError::bad_request("local embedding artifacts failed validation"))?;
    Ok(Arc::new(provider))
}

#[cfg(not(feature = "local-embeddings"))]
fn local_provider(
    _: &AppState,
    _: &EmbeddingConfig,
) -> Result<Arc<dyn EmbeddingProvider>, ApiError> {
    Err(ApiError::bad_request(
        "this build does not include local embeddings",
    ))
}

struct ConfiguredProvider {
    state: WeakAppState,
    profile: EmbeddingProfile,
}

fn resolve_provider(
    state: &AppState,
    profile: &EmbeddingProfile,
) -> Result<Box<dyn EmbeddingProvider>, ProviderError> {
    let _lock = state.providers_lock();
    let config = ProviderConfig::load(&provider_catalog::config_path(&state.data_dir()))
        .map_err(|_| invalid_provider())?;
    let settings = config
        .get(&profile.provider_id)
        .ok_or_else(invalid_provider)?;
    let entry = settings
        .custom
        .as_ref()
        .or_else(|| state.provider_host()?.catalog.get(&profile.provider_id))
        .ok_or_else(invalid_provider)?;
    if provider_catalog::embedding::endpoint_identity(&entry.base_url)? != profile.endpoint_identity
    {
        return Err(invalid_provider());
    }
    let secrets = state
        .secret_store()
        .unwrap_or_else(|| Arc::new(secret_store::MemoryStore::new()));
    settings.build_embedding_provider(entry, secrets.as_ref(), OpenAiCompatConfig::default())
}

fn invalid_provider() -> ProviderError {
    ProviderError::InvalidRequest("embedding provider settings changed or are unavailable".into())
}

#[async_trait]
impl EmbeddingProvider for ConfiguredProvider {
    fn id(&self) -> &str {
        &self.profile.provider_id
    }

    fn endpoint_identity(&self) -> Result<String, ProviderError> {
        Ok(self.profile.endpoint_identity.clone())
    }

    async fn embed(
        &self,
        mut request: EmbeddingRequest,
    ) -> Result<EmbeddingResponse, ProviderError> {
        let state = self.state.upgrade().ok_or_else(invalid_provider)?;
        // Resolve every attempt, so cached services never retain old credentials
        // or authorize a changed destination. The HTTP adapter makes one attempt.
        let provider = resolve_provider(&state, &self.profile)?;
        state.agent().refresh_external_redactor();
        for input in &mut request.inputs {
            input.text = state
                .agent()
                .redact_external_text(std::mem::take(&mut input.text));
            input.title = input
                .title
                .take()
                .map(|title| state.agent().redact_external_text(title));
        }
        provider.embed(request).await
    }
}

/// Explicit owner recovery from a faulty provider cooldown. Local quota is kept.
///
/// # Errors
/// Missing bot/provider or an unavailable quota ledger.
pub fn reset_embedding_cooldown(state: &AppState, bot: BotId) -> Result<(), ApiError> {
    let config = configured_settings(state, bot)?;
    EmbeddingRuntime::open(state.data_dir())
        .and_then(|runtime| runtime.reset_cooldown(&config.account_group))
        .map_err(|_| ApiError::unavailable("embedding cooldown unavailable"))?;
    for bot in state.storage().list_bots(state.user_id())? {
        let Ok(other) = configured_settings(state, bot.id) else {
            continue;
        };
        if other.account_group != config.account_group {
            continue;
        }
        let path = state
            .data_dir()
            .join("bots")
            .join(bot.id.to_string())
            .join("knowledge.db");
        if !path.exists() {
            continue;
        }
        KnowledgeStore::open(
            state.data_dir(),
            Scope {
                user_id: state.user_id(),
                bot_id: bot.id,
            },
        )
        .and_then(|store| store.reset_embedding_cooldown(&other.profile.fingerprint()?))
        .map_err(|_| ApiError::unavailable("embedding source cooldown unavailable"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
