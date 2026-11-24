//! Helpers (optional): embedding providers for the router, memory and knowledge
//! (`/v1/embeddings`). Owner-only writes; keys go to the secret store, never into
//! `embeddings.json` and never back out. See `docs/guide/embeddings.md`.

use std::collections::BTreeMap;

use axum::{
    Json,
    extract::{Path, State},
};
use embedding_hub::catalog::EmbeddingCatalog;
use embedding_hub::config::{EmbeddingConfig, ProviderConfig, ProviderType, Purpose};
use embedding_hub::limiter::{Limits, Meter, Usage};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{ApiError, AppState, error::ErrorBody, routes::ApiJson};

const FILE: &str = "embeddings.json";
const MAX_KEY_BYTES: usize = 4096;

/// A used / limit pair for the usage bars.
#[derive(Debug, Serialize, ToSchema)]
pub struct MeterDto {
    /// Used in the current window.
    pub used: u64,
    /// Provider limit, if known.
    pub limit: Option<u64>,
}

impl From<Meter> for MeterDto {
    fn from(m: Meter) -> Self {
        Self {
            used: m.used,
            limit: m.limit,
        }
    }
}

/// Usage bars: requests/min, tokens/min, requests/day.
#[derive(Debug, Serialize, ToSchema)]
pub struct UsageDto {
    /// Requests in the last minute.
    pub requests_per_minute: MeterDto,
    /// Tokens in the last minute.
    pub tokens_per_minute: MeterDto,
    /// Requests in the last 24 hours.
    pub requests_per_day: MeterDto,
}

impl From<Usage> for UsageDto {
    fn from(u: Usage) -> Self {
        Self {
            requests_per_minute: u.requests_per_minute.into(),
            tokens_per_minute: u.tokens_per_minute.into(),
            requests_per_day: u.requests_per_day.into(),
        }
    }
}

/// Provider limits (RPM / TPM / RPD).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
pub struct LimitsDto {
    /// Requests per minute.
    pub rpm: Option<u32>,
    /// Tokens per minute.
    pub tpm: Option<u32>,
    /// Requests per day.
    pub rpd: Option<u32>,
}

impl From<Limits> for LimitsDto {
    fn from(l: Limits) -> Self {
        Self {
            rpm: l.rpm,
            tpm: l.tpm,
            rpd: l.rpd,
        }
    }
}

impl From<LimitsDto> for Limits {
    fn from(l: LimitsDto) -> Self {
        Self {
            rpm: l.rpm,
            tpm: l.tpm,
            rpd: l.rpd,
        }
    }
}

/// One configured provider.
#[derive(Debug, Serialize, ToSchema)]
#[allow(clippy::struct_excessive_bools)]
pub struct EmbeddingProviderDto {
    /// Short id.
    pub id: String,
    /// `gemini`, `openai`, `compatible` or `local`.
    pub kind: String,
    /// Endpoint, if not the default.
    pub base_url: Option<String>,
    /// Model id.
    pub model: String,
    /// Vector size.
    pub dimensions: usize,
    /// Whether the `dimensions` parameter is sent.
    pub request_dimensions: bool,
    /// Whether an API key is stored (the key itself is never returned).
    pub has_key: bool,
    /// The owner agreed that text is sent to this provider.
    pub allow_remote: bool,
    /// Limits overridden by the owner.
    pub limits: Option<LimitsDto>,
    /// `ok`, `not_allowed`, `key_missing`, `cooling_down`, `key_rejected`,
    /// `rate_limited`, `offline`, `dimension_mismatch`, `rejected` or `unavailable`.
    pub state: String,
    /// Seconds until the provider is tried again.
    pub retry_in_secs: u64,
    /// Used / limit bars.
    pub usage: UsageDto,
    /// Whether text leaves this computer.
    pub remote: bool,
}

/// One model of the catalog.
#[derive(Debug, Serialize, ToSchema)]
pub struct CatalogModelDto {
    /// `gemini`, `openai` or `local`.
    pub provider: String,
    /// Model id.
    pub model: String,
    /// Plain-language name.
    pub label: String,
    /// Recommended vector size.
    pub dimensions: usize,
    /// Largest vector size.
    pub max_dimensions: usize,
    /// Longest input in tokens.
    pub max_input_tokens: usize,
    /// Free-tier limits.
    pub free_tier: Option<LimitsDto>,
    /// `YYYY-MM-DD` last reviewed.
    pub last_reviewed: String,
}

/// The Gemini decision helper model of the catalog.
#[derive(Debug, Serialize, ToSchema)]
pub struct DecisionModelDto {
    /// Provider id.
    pub provider: String,
    /// Model id.
    pub model: String,
    /// Plain-language name.
    pub label: String,
    /// Free-tier limits.
    pub free_tier: Option<LimitsDto>,
    /// `YYYY-MM-DD` last reviewed.
    pub last_reviewed: String,
}

/// The dated catalog.
#[derive(Debug, Serialize, ToSchema)]
pub struct EmbeddingCatalogDto {
    /// `YYYY-MM-DD` last reviewed.
    pub last_reviewed: String,
    /// Caveat for the numbers.
    pub note: String,
    /// Embedding models.
    pub models: Vec<CatalogModelDto>,
    /// Decision models.
    pub decision_models: Vec<DecisionModelDto>,
}

/// `GET /v1/embeddings`.
#[derive(Debug, Serialize, ToSchema)]
pub struct EmbeddingsDto {
    /// Configured providers.
    pub providers: Vec<EmbeddingProviderDto>,
    /// Per purpose (`router`, `memory`, `knowledge`): provider ids in priority
    /// order. The offline embedder is always tried last.
    pub chains: BTreeMap<String, Vec<String>>,
    /// Bots that never use a remote provider.
    pub remote_denied_bots: Vec<String>,
    /// Known models and limits.
    pub catalog: EmbeddingCatalogDto,
    /// Chat providers whose key can be reused (`gemini`).
    pub reusable_keys: Vec<String>,
}

/// A provider as the owner edits it.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingProviderInput {
    /// Short id (`a-z`, `0-9`, `-`).
    pub id: String,
    /// `gemini`, `openai`, `compatible` or `local`.
    pub kind: String,
    /// Endpoint (required for `compatible`).
    #[serde(default)]
    pub base_url: Option<String>,
    /// Model id.
    pub model: String,
    /// Vector size.
    pub dimensions: usize,
    /// Send the `dimensions` parameter.
    #[serde(default = "yes")]
    pub request_dimensions: bool,
    /// The owner agrees that text is sent to this provider.
    #[serde(default)]
    pub allow_remote: bool,
    /// Replace the limits of the catalog.
    #[serde(default)]
    pub limits: Option<LimitsDto>,
    /// New API key (write-only; leave out to keep the stored key).
    #[serde(default)]
    pub api_key: Option<String>,
    /// Copy the key of a chat provider (`gemini`) instead of typing it.
    #[serde(default)]
    pub reuse_key_from: Option<String>,
    /// Remove the stored key.
    #[serde(default)]
    pub clear_key: bool,
}

fn yes() -> bool {
    true
}

/// `PUT /v1/embeddings`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingsInput {
    /// The complete provider list.
    pub providers: Vec<EmbeddingProviderInput>,
    /// Provider ids per purpose.
    #[serde(default)]
    pub chains: BTreeMap<String, Vec<String>>,
    /// Bots that never use a remote provider.
    #[serde(default)]
    pub remote_denied_bots: Vec<String>,
}

/// Result of `POST /v1/embeddings/{id}/test`.
#[derive(Debug, Serialize, ToSchema)]
pub struct EmbeddingTestDto {
    /// `ok` or the reason (see `state` of a provider).
    pub outcome: String,
}

fn key_name(id: &str) -> String {
    format!("embedding-{id}")
}

fn kind_of(s: &str) -> Result<ProviderType, ApiError> {
    Ok(match s {
        "gemini" => ProviderType::Gemini,
        "openai" => ProviderType::Openai,
        "compatible" => ProviderType::Compatible,
        "local" => ProviderType::Local,
        _ => return Err(ApiError::bad_request("unknown provider kind")),
    })
}

fn purpose_of(s: &str) -> Result<Purpose, ApiError> {
    Ok(match s {
        "router" => Purpose::Router,
        "memory" => Purpose::Memory,
        "knowledge" => Purpose::Knowledge,
        _ => return Err(ApiError::bad_request("unknown purpose")),
    })
}

fn purpose_name(p: Purpose) -> &'static str {
    match p {
        Purpose::Router => "router",
        Purpose::Memory => "memory",
        Purpose::Knowledge => "knowledge",
    }
}

fn outcome_name(o: embedding_hub::Outcome) -> String {
    serde_json::to_value(o)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "rejected".to_owned())
}

fn hub(s: &AppState) -> Result<std::sync::Arc<embedding_hub::EmbeddingHub>, ApiError> {
    s.agent()
        .providers()
        .embedding_hub()
        .ok_or_else(|| ApiError::unavailable("embeddings are not available on this host"))
}

fn has_key(s: &AppState, cfg: &ProviderConfig) -> bool {
    cfg.key_ref.as_deref().is_some_and(|name| {
        s.secret_store()
            .is_some_and(|st| st.get(name).ok().flatten().is_some())
    })
}

fn limits_dto(l: Limits) -> Option<LimitsDto> {
    (l != Limits::default()).then(|| l.into())
}

fn dto(s: &AppState) -> Result<EmbeddingsDto, ApiError> {
    let hub = hub(s)?;
    let config = hub.config();
    let status = hub.status();
    let providers = config
        .providers
        .iter()
        .map(|p| {
            let st = status.iter().find(|x| x.id == p.id);
            EmbeddingProviderDto {
                id: p.id.clone(),
                kind: p.kind.catalog_name().to_owned(),
                base_url: p.base_url.clone(),
                model: p.model.clone(),
                dimensions: p.dimensions,
                request_dimensions: p.request_dimensions,
                has_key: has_key(s, p),
                allow_remote: p.allow_remote,
                limits: p.limits.and_then(limits_dto),
                state: st.map_or_else(|| "ok".to_owned(), |x| outcome_name(x.state)),
                retry_in_secs: st.map_or(0, |x| x.retry_in_secs),
                usage: st.map_or_else(|| empty_usage().into(), |x| x.usage.into()),
                remote: p.kind.is_remote(),
            }
        })
        .collect();
    let catalog = hub.catalog();
    Ok(EmbeddingsDto {
        providers,
        chains: config
            .chains
            .iter()
            .map(|(p, c)| (purpose_name(*p).to_owned(), c.clone()))
            .collect(),
        remote_denied_bots: config.remote_denied_bots.iter().cloned().collect(),
        catalog: catalog_dto(catalog),
        reusable_keys: reusable_keys(s),
    })
}

/// Zero usage for a provider the hub has no counters for yet.
fn empty_usage() -> Usage {
    let m = Meter {
        used: 0,
        limit: None,
    };
    Usage {
        requests_per_minute: m,
        tokens_per_minute: m,
        requests_per_day: m,
    }
}

fn catalog_dto(c: &EmbeddingCatalog) -> EmbeddingCatalogDto {
    EmbeddingCatalogDto {
        last_reviewed: c.last_reviewed.clone(),
        note: c.note.clone(),
        models: c
            .embedding_models
            .iter()
            .map(|m| CatalogModelDto {
                provider: m.provider.clone(),
                model: m.model.clone(),
                label: m.label.clone(),
                dimensions: m.dimensions,
                max_dimensions: m.max_dimensions,
                max_input_tokens: m.max_input_tokens,
                free_tier: m.free_tier.map(Into::into),
                last_reviewed: m.last_reviewed.clone(),
            })
            .collect(),
        decision_models: c
            .decision_models
            .iter()
            .map(|m| DecisionModelDto {
                provider: m.provider.clone(),
                model: m.model.clone(),
                label: m.label.clone(),
                free_tier: m.free_tier.map(Into::into),
                last_reviewed: m.last_reviewed.clone(),
            })
            .collect(),
    }
}

/// The chat provider `gemini` (when it has a stored key) can lend its key.
fn reusable_keys(s: &AppState) -> Vec<String> {
    let Ok(cfg) =
        provider_catalog::ProviderConfig::load(&provider_catalog::config_path(&s.data_dir()))
    else {
        return Vec::new();
    };
    cfg.providers
        .iter()
        .filter(|p| p.enabled && p.id == "gemini" && p.key.is_some())
        .map(|p| p.id.clone())
        .collect()
}

fn reused_key(s: &AppState, from: &str) -> Result<String, ApiError> {
    if from != "gemini" {
        return Err(ApiError::bad_request(
            "only the Gemini chat key can be reused",
        ));
    }
    let cfg = provider_catalog::ProviderConfig::load(&provider_catalog::config_path(&s.data_dir()))
        .map_err(|_| ApiError::bad_request("no Gemini key is set up"))?;
    let r = cfg
        .providers
        .iter()
        .find(|p| p.enabled && p.id == "gemini")
        .and_then(|p| p.key.clone())
        .ok_or_else(|| ApiError::bad_request("no Gemini key is set up"))?;
    let store = s
        .secret_store()
        .ok_or_else(|| ApiError::unavailable("this daemon has no secret store"))?;
    let key = secret_store::resolve(store.as_ref(), &r)
        .map_err(|_| ApiError::bad_request("the Gemini key could not be read"))?;
    Ok(key.expose().to_string())
}

/// The embedding setup with live state and usage (no secrets).
#[utoipa::path(get, path = "/v1/embeddings", tag = "embeddings", security(("bearer" = [])),
    responses((status = 200, body = EmbeddingsDto), (status = 503, body = ErrorBody)))]
pub(crate) async fn get_embeddings(
    State(s): State<AppState>,
) -> Result<Json<EmbeddingsDto>, ApiError> {
    Ok(Json(dto(&s)?))
}

fn resolver(s: &AppState) -> impl Fn(&str) -> Option<String> + '_ {
    move |name: &str| {
        let store = s.secret_store()?;
        agent_core::embedding_key_resolver(store.as_ref())(name)
    }
}

/// Save the embedding setup (owner only). Text is only ever sent to a provider
/// whose `allow_remote` the owner switched on.
#[utoipa::path(put, path = "/v1/embeddings", tag = "embeddings", security(("bearer" = [])),
    request_body = EmbeddingsInput,
    responses((status = 200, body = EmbeddingsDto), (status = 400, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub(crate) async fn put_embeddings(
    State(s): State<AppState>,
    ApiJson(input): ApiJson<EmbeddingsInput>,
) -> Result<Json<EmbeddingsDto>, ApiError> {
    let hub = hub(&s)?;
    let old = hub.config();
    let store = s.secret_store();
    let mut config = EmbeddingConfig {
        remote_denied_bots: input.remote_denied_bots.iter().cloned().collect(),
        ..EmbeddingConfig::default()
    };
    // Keys are written only after the whole request is known to be valid.
    let mut writes: Vec<(String, String)> = Vec::new();
    let mut clears: Vec<String> = Vec::new();
    for p in &input.providers {
        let kind = kind_of(&p.kind)?;
        let name = key_name(&p.id);
        let mut key_ref = old
            .provider(&p.id)
            .and_then(|o| o.key_ref.clone())
            .filter(|_| !p.clear_key);
        if p.clear_key {
            clears.push(name.clone());
        }
        let new_key = match (&p.api_key, &p.reuse_key_from) {
            (Some(k), None) if !k.trim().is_empty() => Some(k.trim().to_owned()),
            (None, Some(from)) => Some(reused_key(&s, from)?),
            (None | Some(_), None) => None,
            (Some(_), Some(_)) => {
                return Err(ApiError::bad_request(
                    "give either a key or a provider to reuse, not both",
                ));
            }
        };
        if let Some(k) = new_key {
            if k.len() > MAX_KEY_BYTES || k.chars().any(char::is_control) {
                return Err(ApiError::bad_request("the API key looks invalid"));
            }
            key_ref = Some(name.clone());
            writes.push((name, k));
        }
        config.providers.push(ProviderConfig {
            id: p.id.clone(),
            kind,
            base_url: p.base_url.clone().filter(|u| !u.trim().is_empty()),
            model: p.model.trim().to_owned(),
            dimensions: p.dimensions,
            request_dimensions: p.request_dimensions,
            key_ref,
            allow_remote: p.allow_remote && kind.is_remote(),
            limits: p.limits.map(Into::into),
        });
    }
    for (purpose, chain) in &input.chains {
        config.chains.insert(purpose_of(purpose)?, chain.clone());
    }
    config
        .validate(hub.catalog())
        .map_err(|e| ApiError::bad_request(e.0))?;
    if !writes.is_empty() || !clears.is_empty() {
        let st = store
            .as_ref()
            .ok_or_else(|| ApiError::unavailable("this daemon has no secret store"))?;
        for (name, key) in &writes {
            st.set(name, key)
                .map_err(|_| ApiError::internal("could not store the key"))?;
        }
        for name in &clears {
            let _ = st.delete(name);
        }
    }
    // Providers that were removed take their key with them.
    if let Some(st) = &store {
        for gone in old
            .providers
            .iter()
            .filter(|o| !config.providers.iter().any(|p| p.id == o.id))
        {
            let _ = st.delete(&key_name(&gone.id));
        }
    }
    // Let the redactor learn the new key before it can be sent anywhere.
    let _ = s.agent().redact_host_text(String::new());
    hub.reconfigure(config.clone(), &resolver(&s))
        .map_err(|e| ApiError::bad_request(e.0))?;
    config
        .save(&s.data_dir().join(FILE))
        .map_err(|e| ApiError::internal(e.0))?;
    Ok(Json(dto(&s)?))
}

/// Send one fixed test sentence to a provider (owner only).
#[utoipa::path(post, path = "/v1/embeddings/{id}/test", tag = "embeddings", security(("bearer" = [])),
    params(("id" = String, Path, description = "provider id")),
    responses((status = 200, body = EmbeddingTestDto), (status = 503, body = ErrorBody)))]
pub(crate) async fn test_embedding(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<EmbeddingTestDto>, ApiError> {
    let outcome = hub(&s)?.test(&id).await;
    Ok(Json(EmbeddingTestDto {
        outcome: outcome_name(outcome),
    }))
}

// ------------------------------------------------------------ decision helper

/// Model of the optional Gemini decision helper (catalog: `gemini-decision-1`).
const HELPER_MODEL: &str = "gemini/gemini-decision-1";
/// Requests per minute the helper plans with: 90% of the free tier's 200.
const HELPER_RPM: u32 = 180;

/// `GET /v1/decision-helper`.
#[derive(Debug, Serialize, ToSchema)]
pub struct DecisionHelperDto {
    /// A Gemini key is set up, so the helper can be switched on.
    pub available: bool,
    /// The helper is on.
    pub enabled: bool,
    /// `decider.toml` was written by hand; switching here would replace it.
    pub custom_file: bool,
    /// Model used.
    pub model: String,
    /// Free-tier limits of that model (dated catalog).
    pub free_tier: Option<LimitsDto>,
    /// `YYYY-MM-DD` the limits were last reviewed.
    pub last_reviewed: String,
}

/// `PUT /v1/decision-helper`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DecisionHelperInput {
    /// Switch the helper on or off.
    pub enabled: bool,
}

fn helper_file(s: &AppState) -> std::path::PathBuf {
    s.data_dir().join(agent_core::decider::FILE)
}

fn helper_dto(s: &AppState) -> DecisionHelperDto {
    let settings = agent_core::decider::Settings::load(&s.data_dir());
    let (enabled, custom_file) = match &settings {
        Ok(cfg) => {
            let ours = cfg.backends.len() == 1 && cfg.backends[0].model == HELPER_MODEL;
            (cfg.enabled && ours, helper_file(s).exists() && !ours)
        }
        Err(_) => (false, true),
    };
    let catalog = EmbeddingCatalog::builtin();
    let model = catalog
        .decision_models
        .iter()
        .find(|m| format!("{}/{}", m.provider, m.model) == HELPER_MODEL);
    DecisionHelperDto {
        available: !reusable_keys(s).is_empty(),
        enabled,
        custom_file,
        model: HELPER_MODEL.to_owned(),
        free_tier: model.and_then(|m| m.free_tier).map(Into::into),
        last_reviewed: model.map(|m| m.last_reviewed.clone()).unwrap_or_default(),
    }
}

/// The optional decision helper (Gemini Decisions) and its state.
#[utoipa::path(get, path = "/v1/decision-helper", tag = "embeddings", security(("bearer" = [])),
    responses((status = 200, body = DecisionHelperDto)))]
pub(crate) async fn get_decision_helper(
    State(s): State<AppState>,
) -> Result<Json<DecisionHelperDto>, ApiError> {
    Ok(Json(helper_dto(&s)))
}

/// Switch the decision helper on or off (owner only). It only ever gives
/// advice: a decision never grants a permission.
#[utoipa::path(put, path = "/v1/decision-helper", tag = "embeddings", security(("bearer" = [])),
    request_body = DecisionHelperInput,
    responses((status = 200, body = DecisionHelperDto), (status = 400, body = ErrorBody)))]
pub(crate) async fn put_decision_helper(
    State(s): State<AppState>,
    ApiJson(input): ApiJson<DecisionHelperInput>,
) -> Result<Json<DecisionHelperDto>, ApiError> {
    let current = helper_dto(&s);
    if current.custom_file {
        return Err(ApiError::bad_request(
            "decider.toml has settings you wrote by hand; edit that file instead",
        ));
    }
    if input.enabled && !current.available {
        return Err(ApiError::bad_request(
            "add a Gemini key first (Settings > Models); the helper needs it",
        ));
    }
    let text = format!(
        "# Written by Settings > Models > Helpers.\nenabled = {}\ntimeout_ms = 1500\nmax_calls_per_turn = 2\n\n[[backends]]\napi = \"gemini_decisions\"\nmodel = \"{HELPER_MODEL}\"\nrpm = {HELPER_RPM}\n",
        input.enabled
    );
    let path = helper_file(&s);
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text)
        .and_then(|()| std::fs::rename(&tmp, &path))
        .map_err(|e| ApiError::internal(format!("cannot write decider.toml: {e}")))?;
    if s.provider_host().is_some() {
        crate::routes::reload_from_host(&s).await?;
    }
    Ok(Json(helper_dto(&s)))
}
