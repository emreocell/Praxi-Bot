//! Optional semantic retrieval. Lexical search never depends on this service.
//!
//! Profiles name an immutable vector space. Source grants authorize document
//! egress only; query egress has a separate host-controlled grant. Provider
//! adapters perform one request while this service owns durable reservations,
//! retry state, cancellation, caches and revision-checked publication.

mod index;
mod local;
mod vector;

#[cfg(feature = "local-embeddings")]
pub use local::LocalOnnxEmbedder;
pub use local::{LocalEmbeddingArtifacts, PinnedEmbeddingArtifact};

/// Whether this build includes the explicit-artifact CPU inference adapter.
#[must_use]
pub const fn local_embeddings_available() -> bool {
    cfg!(all(
        feature = "local-embeddings",
        any(target_os = "linux", windows)
    ))
}

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use provider_catalog::{EmbeddingProvider, EmbeddingTaskFormat};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub use super::embedding_runtime::{EmbeddingBudget, EmbeddingRuntime, RequestUnit};
use super::{
    KnowledgeStore, RemoteEmbeddingConsent, SearchHit, SearchMode, SearchRequest, SearchResponse,
};
use crate::{MemoryError, Result, Trust};

/// Full semantic identity of an index generation, without API keys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingProfile {
    pub provider_id: String,
    pub model: String,
    /// An optional pinned vendor/model artifact revision. Changing an alias
    /// without a new revision cannot be detected if the vendor does not report it.
    pub model_revision: Option<String>,
    /// Canonical configured base URL, or `local-onnx:<artifact SHA-256>`.
    pub endpoint_identity: String,
    pub dimensions: usize,
    /// Disable for compatible endpoints that always return a fixed dimension.
    pub request_dimensions: bool,
    pub format: EmbeddingTaskFormat,
}

impl EmbeddingProfile {
    /// Fingerprint the provider, destination, model, dimensions and task recipe.
    ///
    /// # Errors
    /// Invalid identity, URL, dimension, or serialization failure.
    pub fn fingerprint(&self) -> Result<String> {
        self.validate()?;
        let bytes = serde_json::to_vec(&("knowledge-embedding-v1", "l2-f64-v1", self))?;
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }

    fn validate(&self) -> Result<()> {
        for (value, max) in [(&self.provider_id, 128), (&self.model, 256)] {
            if value.trim().is_empty() || value.len() > max || value.chars().any(char::is_control) {
                return Err(invalid("invalid embedding provider or model"));
            }
        }
        if self.model_revision.as_ref().is_some_and(|revision| {
            revision.is_empty() || revision.len() > 256 || revision.chars().any(char::is_control)
        }) || !(1..=provider_catalog::embedding::MAX_EMBEDDING_DIMENSIONS)
            .contains(&self.dimensions)
        {
            return Err(invalid("invalid embedding revision or dimensions"));
        }
        if let Some(digest) = self.endpoint_identity.strip_prefix("local-onnx:") {
            if !is_digest(digest) || self.format != EmbeddingTaskFormat::E5 {
                return Err(invalid("invalid local embedding artifact identity"));
            }
        } else {
            let canonical = provider_catalog::embedding::endpoint_identity(&self.endpoint_identity)
                .map_err(|_| invalid("invalid embedding endpoint identity"))?;
            if canonical != self.endpoint_identity {
                return Err(invalid("embedding endpoint identity must be canonical"));
            }
        }
        Ok(())
    }
}

const fn default_batch() -> usize {
    16
}
const fn default_scan() -> usize {
    20_000
}
const fn default_cache() -> usize {
    20_000
}
const fn default_query_timeout() -> u64 {
    500
}
const fn default_index_timeout() -> u64 {
    30_000
}
const fn default_attempts() -> u32 {
    5
}

/// Portable preferences only. Native file paths and egress authority are
/// supplied separately by the trusted host, never deserialized from bot JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingConfig {
    pub profile: EmbeddingProfile,
    /// An ID in the trusted host registry, never a model or shared-library path.
    #[serde(default)]
    pub local_profile_id: Option<String>,
    /// Shared across provider configurations using the same account/project.
    /// Defaults at the host to provider + `SecretRef` identity, never a secret value.
    pub account_group: String,
    #[serde(default)]
    pub budget: EmbeddingBudget,
    #[serde(skip)]
    pub query_consent: Option<RemoteEmbeddingConsent>,
    #[serde(default = "default_batch")]
    pub max_batch_inputs: usize,
    #[serde(default = "default_scan")]
    pub max_scan_vectors: usize,
    #[serde(default = "default_cache")]
    pub max_cache_entries: usize,
    #[serde(default = "default_query_timeout")]
    pub query_timeout_ms: u64,
    #[serde(default = "default_index_timeout")]
    pub index_timeout_ms: u64,
    #[serde(default = "default_attempts")]
    pub max_attempts: u32,
}

impl EmbeddingConfig {
    #[must_use]
    pub fn new(profile: EmbeddingProfile, account_group: impl Into<String>) -> Self {
        Self {
            profile,
            local_profile_id: None,
            account_group: account_group.into(),
            budget: EmbeddingBudget::default(),
            query_consent: None,
            max_batch_inputs: default_batch(),
            max_scan_vectors: default_scan(),
            max_cache_entries: default_cache(),
            query_timeout_ms: default_query_timeout(),
            index_timeout_ms: default_index_timeout(),
            max_attempts: default_attempts(),
        }
    }

    /// Validate host-owned configuration before starting a worker.
    ///
    /// # Errors
    /// Invalid profile, grants, budgets, or unsupported resource limits.
    pub fn validate(&self) -> Result<()> {
        let fingerprint = self.profile.fingerprint()?;
        if self.local_profile_id.as_ref().is_some_and(|id| {
            id.is_empty()
                || id.len() > 128
                || !id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
        }) {
            return Err(invalid("invalid installed local embedding profile ID"));
        }
        self.budget.validate()?;
        if self.account_group.trim().is_empty()
            || self.account_group.len() > 256
            || self.account_group.chars().any(char::is_control)
            || !(1..=provider_catalog::embedding::MAX_EMBEDDING_INPUTS)
                .contains(&self.max_batch_inputs)
            || !(1..=100_000).contains(&self.max_scan_vectors)
            || !(1..=100_000).contains(&self.max_cache_entries)
            || !(1..=120_000).contains(&self.query_timeout_ms)
            || !(1..=120_000).contains(&self.index_timeout_ms)
            || !(1..=10).contains(&self.max_attempts)
            || self.query_consent.as_ref().is_some_and(|grant| {
                grant.profile_fingerprint != fingerprint || grant.granted_at_ms < 0
            })
        {
            return Err(invalid(
                "invalid embedding limits, quota account, or query grant",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmbeddingStatus {
    Progress,
    Complete,
    AwaitingConsent,
    Deferred,
    Cancelled,
    Failed,
}

/// Bounded worker result. Failure codes never contain provider response bodies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingProgress {
    pub status: EmbeddingStatus,
    pub embedded_chunks: usize,
    pub cache_hits: usize,
    #[serde(default)]
    pub skipped_chunks: usize,
    pub remaining_chunks: usize,
    pub next_attempt_ms: Option<i64>,
    pub failure_code: Option<String>,
}

impl EmbeddingProgress {
    fn status(status: EmbeddingStatus) -> Self {
        Self {
            status,
            embedded_chunks: 0,
            cache_hits: 0,
            skipped_chunks: 0,
            remaining_chunks: 0,
            next_attempt_ms: None,
            failure_code: None,
        }
    }
}

struct QueryCacheEntry {
    key: String,
    expires_ms: i64,
    vector: Vec<f32>,
}

/// Data-only local reranking input. Returning text is deliberately unsupported.
#[derive(Debug, Clone)]
pub struct RerankCandidate {
    pub id: Uuid,
    pub text: String,
}

/// Optional local implementation. Remote reranking is not enabled by this hook
/// or by an embedding grant; it requires a separately designed egress policy.
#[async_trait]
pub trait LocalReranker: Send + Sync {
    /// Return an exact permutation of the supplied IDs, with best results first.
    async fn rerank(&self, query: &str, candidates: &[RerankCandidate]) -> Result<Vec<Uuid>>;
}

/// A host permission decision, checked at admission and publication. Implement
/// this with live permission-engine policy and owner-confirmed host grants.
/// Source/query consent JSON alone never substitutes for this authority.
pub trait EmbeddingAuthorizer: Send + Sync {
    fn allows_document(&self, source: Uuid, profile: &str) -> bool;
    fn allows_query(&self, profile: &str) -> bool;
}

/// One bot + one immutable profile. Rebuild this object when host settings change.
pub struct KnowledgeEmbeddingService {
    store: Arc<KnowledgeStore>,
    provider: Arc<dyn EmbeddingProvider>,
    runtime: Arc<EmbeddingRuntime>,
    config: EmbeddingConfig,
    fingerprint: String,
    /// At most 64 vectors, five minutes each; raw query text is not retained.
    query_cache: Mutex<VecDeque<QueryCacheEntry>>,
    reranker: Option<Arc<dyn LocalReranker>>,
    authorizer: Option<Arc<dyn EmbeddingAuthorizer>>,
}

impl KnowledgeEmbeddingService {
    /// Bind the configured identity to the actual provider before any IO.
    ///
    /// # Errors
    /// Invalid configuration or a provider/endpoint identity mismatch.
    pub fn new(
        store: Arc<KnowledgeStore>,
        provider: Arc<dyn EmbeddingProvider>,
        runtime: Arc<EmbeddingRuntime>,
        config: EmbeddingConfig,
    ) -> Result<Self> {
        config.validate()?;
        let endpoint = provider
            .endpoint_identity()
            .map_err(|_| invalid("embedding provider has no valid endpoint identity"))?;
        if provider.id() != config.profile.provider_id
            || endpoint != config.profile.endpoint_identity
            || provider.is_remote() == endpoint.starts_with("local-onnx:")
        {
            return Err(invalid(
                "embedding provider identity differs from the authorized profile",
            ));
        }
        let fingerprint = config.profile.fingerprint()?;
        index::validate_schema(&store.conn())?;
        index::activate_profile(&mut store.conn(), &fingerprint)?;
        Ok(Self {
            store,
            provider,
            runtime,
            config,
            fingerprint,
            query_cache: Mutex::new(VecDeque::new()),
            reranker: None,
            authorizer: None,
        })
    }

    #[must_use]
    pub fn profile_fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Used by the host's tool adapter to select the existing egress policy path.
    #[must_use]
    pub fn may_egress(&self) -> bool {
        self.provider.is_remote()
            && self
                .authorizer
                .as_ref()
                .is_some_and(|authority| authority.allows_query(&self.fingerprint))
            && self
                .config
                .query_consent
                .as_ref()
                .is_some_and(|grant| grant.profile_fingerprint == self.fingerprint)
    }

    /// Install the trusted host's live permission decision service. Remote
    /// embedding defaults to denied when no authorizer is installed.
    #[must_use]
    pub fn with_authorizer(mut self, authorizer: Arc<dyn EmbeddingAuthorizer>) -> Self {
        self.authorizer = Some(authorizer);
        self
    }

    fn document_authorized(&self, source: Uuid) -> bool {
        !self.provider.is_remote()
            || self
                .authorizer
                .as_ref()
                .is_some_and(|authority| authority.allows_document(source, &self.fingerprint))
    }

    /// Install an optional bounded local reranker. No remote grant is implied.
    #[must_use]
    pub fn with_local_reranker(mut self, reranker: Arc<dyn LocalReranker>) -> Self {
        self.reranker = Some(reranker);
        self
    }

    async fn pack_with_rerank(
        &self,
        mut hits: Vec<SearchHit>,
        request: &SearchRequest,
        mode: SearchMode,
        cancel: &CancellationToken,
    ) -> Result<SearchResponse> {
        if let Some(reranker) = &self.reranker {
            hits.truncate(256);
            let candidates = hits
                .iter()
                .map(|hit| RerankCandidate {
                    id: hit.id,
                    text: hit.content.chars().take(1600).collect(),
                })
                .collect::<Vec<_>>();
            let result = tokio::select! {
                biased;
                ()=cancel.cancelled()=>return Err(MemoryError::Embed("knowledge search cancelled".into())),
                result=tokio::time::timeout(Duration::from_millis(self.config.query_timeout_ms),reranker.rerank(&request.query,&candidates))=>result,
            };
            if let Ok(Ok(ids)) = result {
                let expected = candidates
                    .iter()
                    .map(|candidate| candidate.id)
                    .collect::<std::collections::HashSet<_>>();
                let actual = ids
                    .iter()
                    .copied()
                    .collect::<std::collections::HashSet<_>>();
                if ids.len() == hits.len() && ids.len() == actual.len() && actual == expected {
                    let mut by_id = hits
                        .into_iter()
                        .map(|hit| (hit.id, hit))
                        .collect::<std::collections::HashMap<_, _>>();
                    hits = ids.into_iter().filter_map(|id| by_id.remove(&id)).collect();
                }
            }
        }
        if cancel.is_cancelled() {
            return Err(MemoryError::Embed("knowledge search cancelled".into()));
        }
        // Reranking may have yielded while a source was disabled, deleted, or
        // reindexed. Refresh exact IDs and metadata; never return a stale body.
        let conn = self.store.conn();
        let mut statement=conn.prepare("SELECT c.id,c.source_id,c.document_id,s.label,d.locator,c.section,c.line_start,c.line_end,s.trust,c.body,s.kind,s.locator
            FROM chunks c JOIN sources s ON s.id=c.source_id JOIN documents d ON d.id=c.document_id WHERE c.id=?1 AND s.enabled=1")?;
        let mut current = Vec::with_capacity(hits.len());
        for hit in hits {
            if let Some(hit) = statement
                .query_row([hit.id.to_string()], |row| {
                    let mut hit = SearchHit {
                        id: super::store::uuid_at(row, 0)?,
                        source_id: super::store::uuid_at(row, 1)?,
                        document_id: super::store::uuid_at(row, 2)?,
                        source_label: row.get(3)?,
                        locator: row.get(4)?,
                        section: row.get(5)?,
                        line_start: super::store::integer_at(row, 6)?,
                        line_end: super::store::integer_at(row, 7)?,
                        path: None,
                        page: None,
                        citation: None,
                        source_trust: if row.get::<_, String>(8)? == "user" {
                            Trust::User
                        } else {
                            Trust::Untrusted
                        },
                        content: row.get(9)?,
                        score: hit.score,
                    };
                    hit.attach_citation(&row.get::<_, String>(10)?, &row.get::<_, String>(11)?);
                    Ok(hit)
                })
                .optional()?
            {
                current.push(hit);
            }
        }
        super::search::pack_search_results(current, request, mode)
    }
}

pub(super) fn migrate(conn: &rusqlite::Connection) -> Result<()> {
    let version: u32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version >= 2 {
        return Ok(());
    }
    conn.execute_batch(index::SCHEMA)?;
    conn.execute_batch(index::GRANT_TRIGGER)?;
    conn.pragma_update(None, "user_version", 2)?;
    Ok(())
}

pub(super) fn migrate_v3(conn: &rusqlite::Connection) -> Result<()> {
    let version: u32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version >= 3 {
        return Ok(());
    }
    conn.execute_batch(index::FAILURE_SCHEMA)?;
    conn.execute_batch("ALTER TABLE embedding_jobs ADD COLUMN batch_limit INTEGER NOT NULL DEFAULT 0 CHECK(batch_limit BETWEEN 0 AND 128); DROP TRIGGER knowledge_embedding_grant_changed;")?;
    conn.execute_batch(index::GRANT_TRIGGER)?;
    conn.execute(
        "INSERT INTO knowledge_meta(key,value) VALUES('embedding_authority_epoch',?1)",
        [Uuid::now_v7().to_string()],
    )?;
    conn.pragma_update(None, "user_version", 3)?;
    Ok(())
}

pub(super) fn validate_structure(conn: &rusqlite::Connection) -> Result<()> {
    index::validate_data(conn)
}

impl KnowledgeStore {
    /// Clear only provider-cooldown scheduling after explicit owner recovery.
    /// Durable request reservations and terminal failure attempts are unchanged.
    ///
    /// # Errors
    /// SQLite errors.
    pub fn reset_embedding_cooldown(&self, profile: &str) -> Result<()> {
        self.conn().execute("UPDATE embedding_jobs SET not_before_ms=0,failure_code=NULL WHERE profile=?1 AND failure_code='provider_rate_limit'",[profile])?;
        Ok(())
    }
}

fn invalid(message: &str) -> MemoryError {
    MemoryError::Invalid(message.into())
}

fn is_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn cache_key(task: provider_catalog::EmbeddingTask, text: &str, title: Option<&str>) -> String {
    // Hash exactly the model input recipe, not mtime, path, or only the text.
    let bytes = serde_json::to_vec(&("knowledge-content-v1", task, title, text))
        .expect("string tuple is serializable");
    format!("{:x}", Sha256::digest(bytes))
}

fn normalize(vector: Vec<f32>, dimensions: usize) -> Result<Vec<f32>> {
    if vector.len() != dimensions || vector.iter().any(|value| !value.is_finite()) {
        return Err(invalid("embedding vector dimension or values are invalid"));
    }
    let norm = vector
        .iter()
        .map(|&value| f64::from(value).powi(2))
        .sum::<f64>()
        .sqrt();
    if !norm.is_finite() || norm <= 0. {
        return Err(invalid("embedding vector has zero or invalid norm"));
    }
    Ok(vector
        .into_iter()
        .map(|value| {
            #[allow(clippy::cast_possible_truncation)]
            let normalized = (f64::from(value) / norm) as f32;
            normalized
        })
        .collect())
}

#[cfg(test)]
mod tests;
