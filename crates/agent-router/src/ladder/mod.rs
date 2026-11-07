//! Decision ladder K0-K3 (ADR-0005): decide, cheaply and before the main LLM
//! (K4), whether a request can be served faster.
//!
//! | Level | Mechanism | Module |
//! |---|---|---|
//! | K0 | slash commands, user regex rules | [`rules`] |
//! | K1 | intent cache (SQLite, TTL, workflow-version invalidation) | [`cache`] |
//! | K2 | BM25 + embeddings over workflows / skills / tools | [`semantic`] |
//! | K3 | fast classifier model, strict JSON | [`classifier`] |
//!
//! The ladder only *routes*; it never executes anything. Every level is
//! fail-open: errors and timeouts end in [`Route::Agent`] (K4). A workflow is
//! routed only at confidence >= [`LadderConfig::workflow_threshold`] (0.9) and
//! only if all its required parameters are known; otherwise the result is a
//! hint that narrows K4's surface.
//!
//! # Wiring (follow-up for `agent-core`)
//! 1. Build one [`Ladder`] per bot (`Ladder::builder()`), feed it
//!    [`Candidate`]s from the workflow store, skills and tool registry via
//!    [`Ladder::set_candidates`] (+ [`Ladder::refresh_embeddings`]) whenever
//!    those sets change.
//! 2. Before each K4 turn call [`Ladder::decide`]. Execute `Command` /
//!    `Workflow` through the normal permission gate; pass `ToolHint` /
//!    `SkillHint` to the tool surface; `Agent` runs unchanged.
//! 3. After a successful run the user explicitly accepted, call
//!    [`Ladder::remember_route`]; call [`Ladder::invalidate_workflow`] when a
//!    workflow is edited or deleted.
//! 4. Log each [`Decision`] (level, confidence, latency, reason) for the
//!    "why did it do that" panel and export [`Ladder::metrics`].

pub mod cache;
pub mod classifier;
pub mod gemini;
mod normalize;
pub mod rules;
pub mod semantic;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

pub use cache::{CachedRoute, RouteCache};
pub use classifier::{Classifier, Verdict};
pub use gemini::{GeminiClassifier, GeminiEmbedder};
pub use normalize::normalize;
pub use rules::{RuleAction, RuleSet, UserRule};
pub use semantic::{
    Candidate, CandidateKind, CandidateParam, Corpus, EmbedTask, Embedder, NullEmbedder, Scored,
    Scoring,
};

use classifier::{VerdictLimits, build_prompt, interpret};

/// Errors of the ladder's setup and cache APIs (routing itself never fails).
#[derive(Debug, thiserror::Error)]
pub enum RouterError {
    /// SQLite failure.
    #[error("intent cache: {0}")]
    Cache(#[from] rusqlite::Error),
    /// (De)serialisation failure.
    #[error("serialisation: {0}")]
    Serde(#[from] serde_json::Error),
    /// A user rule's regex did not compile.
    #[error("invalid rule {0}")]
    InvalidRule(String),
    /// `remember_route` refused the request.
    #[error("cannot remember route: {0}")]
    Rejected(String),
}

/// A failed call to an embedding / classifier backend. Never contains secrets.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub struct BackendError(String);

impl BackendError {
    /// New error with a message.
    #[must_use]
    pub fn new(msg: impl Into<String>) -> Self {
        Self(msg.into())
    }
}

/// A built-in command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Command {
    /// Command name (`stop`, `new`, or a user-rule command).
    pub name: String,
    /// Arguments.
    pub args: Vec<String>,
}

/// Where the request should go.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Route {
    /// Run a command.
    Command(Command),
    /// Replay a saved workflow (still behind the permission gate).
    Workflow {
        /// Workflow name.
        name: String,
        /// Parameters.
        params: BTreeMap<String, String>,
        /// Router confidence.
        confidence: f64,
    },
    /// Narrow the tool surface K4 sees.
    ToolHint {
        /// Likely tools.
        tools: Vec<String>,
    },
    /// Suggest skills.
    SkillHint {
        /// Likely skills.
        skills: Vec<String>,
    },
    /// Go to K4 unchanged.
    Agent,
}

/// The ladder level that produced a decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Level {
    /// Rules / commands.
    K0,
    /// Intent cache.
    K1,
    /// BM25 + embeddings.
    K2,
    /// Classifier.
    K3,
    /// Main LLM (nothing cheaper applied).
    K4,
}

/// A routing decision with its explanation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Decision {
    /// The route.
    pub route: Route,
    /// Level that decided.
    pub level: Level,
    /// Confidence in `0..=1`.
    pub confidence: f64,
    /// Wall time of the whole decision.
    pub latency_ms: u64,
    /// Why (for the explanation panel / eval logs).
    pub reason: String,
}

/// Input of [`Ladder::decide`].
#[derive(Debug, Clone)]
pub struct RouteContext {
    /// The user's text.
    pub text: String,
}

impl RouteContext {
    /// Context for `text`.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}

/// Thresholds and budgets.
#[derive(Debug, Clone)]
pub struct LadderConfig {
    /// Minimum confidence to route a workflow (K2 and K3). ADR-0005: 0.9.
    pub workflow_threshold: f64,
    /// Minimum K2 score to emit a tool/skill hint without asking K3.
    pub hint_threshold: f64,
    /// Minimum K3 confidence for a tool/skill hint.
    pub k3_hint_threshold: f64,
    /// K2 candidates below this score are not shown to K3.
    pub candidate_floor: f64,
    /// Max candidates shown to K3.
    pub top_k: usize,
    /// Max tools/skills in a hint.
    pub hint_max: usize,
    /// A workflow must beat the runner-up by this much at K2.
    pub margin: f64,
    /// If K2 finds nothing, show K3 the whole corpus when it has at most this many items.
    pub small_corpus: usize,
    /// K3 timeout (card: 1.5 s).
    pub k3_timeout: Duration,
    /// K3 output budget.
    pub k3_max_tokens: u32,
    /// Query-embedding timeout (K2 degrades to BM25 only).
    pub embed_timeout: Duration,
    /// K1 entry lifetime.
    pub cache_ttl: Duration,
    /// Hybrid score tunables.
    pub scoring: Scoring,
}

impl Default for LadderConfig {
    fn default() -> Self {
        Self {
            workflow_threshold: 0.9,
            hint_threshold: 0.7,
            k3_hint_threshold: 0.5,
            candidate_floor: 0.1,
            top_k: 5,
            hint_max: 3,
            margin: 0.05,
            small_corpus: 12,
            k3_timeout: Duration::from_millis(1500),
            k3_max_tokens: 192,
            embed_timeout: Duration::from_millis(500),
            cache_ttl: Duration::from_secs(7 * 24 * 3600),
            scoring: Scoring::default(),
        }
    }
}

#[derive(Debug, Default)]
struct Counters {
    levels: [AtomicU64; 5],
    k1_stale: AtomicU64,
    cache_errors: AtomicU64,
    k2_embed_errors: AtomicU64,
    k3_calls: AtomicU64,
    k3_timeouts: AtomicU64,
    k3_errors: AtomicU64,
    k3_invalid: AtomicU64,
}

/// Point-in-time copy of the counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetricsSnapshot {
    /// Decisions per level `[K0, K1, K2, K3, K4]`.
    pub decided: [u64; 5],
    /// K1 hits discarded because the workflow changed or vanished.
    pub k1_stale: u64,
    /// SQLite errors (treated as misses).
    pub cache_errors: u64,
    /// Query-embedding failures / timeouts.
    pub k2_embed_errors: u64,
    /// Classifier calls started.
    pub k3_calls: u64,
    /// Classifier timeouts.
    pub k3_timeouts: u64,
    /// Classifier transport errors.
    pub k3_errors: u64,
    /// Classifier answers rejected by validation.
    pub k3_invalid: u64,
}

type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

struct Outcome {
    route: Route,
    level: Level,
    confidence: f64,
    reason: String,
}

impl Outcome {
    fn new(route: Route, level: Level, confidence: f64, reason: impl Into<String>) -> Self {
        Self {
            route,
            level,
            confidence,
            reason: reason.into(),
        }
    }

    fn agent(reason: impl Into<String>) -> Self {
        Self::new(Route::Agent, Level::K4, 0.0, reason)
    }
}

/// The K0-K3 router.
pub struct Ladder {
    config: LadderConfig,
    rules: RuleSet,
    cache: Option<RouteCache>,
    corpus: RwLock<Arc<Corpus>>,
    embedder: Arc<dyn Embedder>,
    classifier: Option<Arc<dyn Classifier>>,
    clock: Clock,
    counters: Counters,
}

impl std::fmt::Debug for Ladder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ladder")
            .field("config", &self.config)
            .field("candidates", &self.corpus.read().len())
            .field("has_classifier", &self.classifier.is_some())
            .finish_non_exhaustive()
    }
}

/// Builder for [`Ladder`].
pub struct LadderBuilder {
    config: LadderConfig,
    rules: RuleSet,
    cache: Option<RouteCache>,
    embedder: Arc<dyn Embedder>,
    classifier: Option<Arc<dyn Classifier>>,
    clock: Clock,
}

fn system_clock() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

impl LadderBuilder {
    /// Thresholds and budgets.
    #[must_use]
    pub fn config(mut self, config: LadderConfig) -> Self {
        self.config = config;
        self
    }

    /// User-defined K0 rules from the bot profile.
    ///
    /// # Errors
    /// [`RouterError::InvalidRule`].
    pub fn rules(mut self, rules: &[UserRule]) -> Result<Self, RouterError> {
        self.rules = RuleSet::new(rules)?;
        Ok(self)
    }

    /// Enable K1 with this cache.
    #[must_use]
    pub fn cache(mut self, cache: RouteCache) -> Self {
        self.cache = Some(cache);
        self
    }

    /// K2 embedder (default: [`NullEmbedder`]).
    #[must_use]
    pub fn embedder(mut self, embedder: Arc<dyn Embedder>) -> Self {
        self.embedder = embedder;
        self
    }

    /// Enable K3 with this classifier.
    #[must_use]
    pub fn classifier(mut self, classifier: Arc<dyn Classifier>) -> Self {
        self.classifier = Some(classifier);
        self
    }

    /// Replace the clock (unix seconds); for tests.
    #[must_use]
    pub fn clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// Finish.
    #[must_use]
    pub fn build(self) -> Ladder {
        Ladder {
            config: self.config,
            rules: self.rules,
            cache: self.cache,
            corpus: RwLock::new(Arc::new(Corpus::default())),
            embedder: self.embedder,
            classifier: self.classifier,
            clock: self.clock,
            counters: Counters::default(),
        }
    }
}

impl Ladder {
    /// Start building a ladder (K0 slash commands only until configured).
    #[must_use]
    pub fn builder() -> LadderBuilder {
        LadderBuilder {
            config: LadderConfig::default(),
            rules: RuleSet::default(),
            cache: None,
            embedder: Arc::new(NullEmbedder),
            classifier: None,
            clock: Arc::new(system_clock),
        }
    }

    /// The active configuration.
    #[must_use]
    pub fn config(&self) -> &LadderConfig {
        &self.config
    }

    /// Replace the routable items (workflows, skills, tools). Rebuilds BM25;
    /// embeddings are dropped until [`Self::refresh_embeddings`] runs.
    pub fn set_candidates(&self, candidates: Vec<Candidate>) {
        *self.corpus.write() = Arc::new(Corpus::new(candidates));
    }

    /// Embed the current candidates (one batched request). K2 falls back to
    /// BM25 only until this succeeds.
    ///
    /// # Errors
    /// The backend failure; the corpus is left without embeddings.
    pub async fn refresh_embeddings(&self) -> Result<(), BackendError> {
        if !self.embedder.is_enabled() {
            return Ok(());
        }
        let snapshot = self.corpus.read().clone();
        if snapshot.is_empty() {
            return Ok(());
        }
        let texts: Vec<String> = snapshot
            .candidates
            .iter()
            .map(Candidate::embed_text)
            .collect();
        let vectors = self.embedder.embed(&texts, EmbedTask::Document).await?;
        if vectors.len() != texts.len() {
            return Err(BackendError::new("embedding count mismatch"));
        }
        let mut guard = self.corpus.write();
        // Only attach if the candidate set did not change meanwhile.
        if Arc::ptr_eq(&guard, &snapshot) {
            *guard = Arc::new(snapshot.with_embeddings(vectors));
        }
        Ok(())
    }

    /// Counters since construction.
    #[must_use]
    pub fn metrics(&self) -> MetricsSnapshot {
        let c = &self.counters;
        let get = |a: &AtomicU64| a.load(Ordering::Relaxed);
        MetricsSnapshot {
            decided: std::array::from_fn(|i| get(&c.levels[i])),
            k1_stale: get(&c.k1_stale),
            cache_errors: get(&c.cache_errors),
            k2_embed_errors: get(&c.k2_embed_errors),
            k3_calls: get(&c.k3_calls),
            k3_timeouts: get(&c.k3_timeouts),
            k3_errors: get(&c.k3_errors),
            k3_invalid: get(&c.k3_invalid),
        }
    }

    /// Remember that `text` should route to `route` next time. Call only
    /// after a successful run that the user explicitly accepted.
    ///
    /// # Errors
    /// [`RouterError::Rejected`] for `Agent`, a parameterised workflow route, an unknown workflow, or when no
    /// cache is configured; SQLite failures.
    pub fn remember_route(&self, text: &str, route: &Route) -> Result<(), RouterError> {
        let cache = self
            .cache
            .as_ref()
            .ok_or_else(|| RouterError::Rejected("no cache configured".into()))?;
        let norm = normalize(text);
        if norm.is_empty() {
            return Err(RouterError::Rejected("empty text".into()));
        }
        let version = match route {
            Route::Agent => {
                return Err(RouterError::Rejected("agent routes are not cached".into()));
            }
            Route::Workflow { name, params, .. } => {
                // The cache key is the normalised text, which many different
                // raw texts share ("a_b.txt" / "a-b.txt"); replaying remembered
                // params would target the wrong file. Only parameterless
                // workflow routes are cacheable.
                if !params.is_empty() {
                    return Err(RouterError::Rejected(
                        "workflow routes with parameters are not cached".into(),
                    ));
                }
                let corpus = self.corpus.read().clone();
                let cand = corpus
                    .find(CandidateKind::Workflow, name)
                    .ok_or_else(|| RouterError::Rejected(format!("unknown workflow {name:?}")))?;
                Some(cand.version.clone())
            }
            _ => None,
        };
        cache.put(&norm, route, version.as_deref(), (self.clock)())
    }

    /// Drop every cached route to workflow `name` (it was edited or deleted).
    ///
    /// # Errors
    /// SQLite failures.
    pub fn invalidate_workflow(&self, name: &str) -> Result<usize, RouterError> {
        self.cache
            .as_ref()
            .map_or(Ok(0), |c| c.invalidate_workflow(name))
    }

    /// Decide where `ctx` should go. Never fails: any problem is K4.
    pub async fn decide(&self, ctx: &RouteContext) -> Decision {
        let start = Instant::now();
        let out = self.run(ctx).await;
        let idx = match out.level {
            Level::K0 => 0,
            Level::K1 => 1,
            Level::K2 => 2,
            Level::K3 => 3,
            Level::K4 => 4,
        };
        self.counters.levels[idx].fetch_add(1, Ordering::Relaxed);
        Decision {
            route: out.route,
            level: out.level,
            confidence: out.confidence,
            latency_ms: u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
            reason: out.reason,
        }
    }

    async fn run(&self, ctx: &RouteContext) -> Outcome {
        let norm = normalize(&ctx.text);

        // K0
        if let Some(route) = rules::slash_command(&ctx.text) {
            return Outcome::new(route, Level::K0, 1.0, "slash command");
        }
        if let Some((route, reason)) = self.rules.matching(&ctx.text, &norm) {
            return Outcome::new(route, Level::K0, 1.0, reason);
        }
        if norm.is_empty() {
            return Outcome::agent("empty request");
        }

        let corpus = self.corpus.read().clone();

        // K1
        if let Some(hit) = self.k1(&norm, &corpus) {
            return hit;
        }

        // K2
        if corpus.is_empty() {
            return Outcome::agent("no routing candidates");
        }
        let ranked = self.k2_rank(ctx, &corpus).await;
        if let Some(out) = self.k2_decide(&corpus, &ranked) {
            return out;
        }

        // K3
        self.k3(ctx, &corpus, &ranked).await
    }

    fn k1(&self, norm: &str, corpus: &Corpus) -> Option<Outcome> {
        let cache = self.cache.as_ref()?;
        let hit = match cache.get(norm, (self.clock)()) {
            Ok(Some(hit)) => hit,
            Ok(None) => return None,
            Err(_) => {
                self.counters.cache_errors.fetch_add(1, Ordering::Relaxed);
                return None;
            }
        };
        if let Route::Workflow { name, params, .. } = &hit.route {
            // Entries written before parameterised routes were refused.
            if !params.is_empty() {
                self.counters.k1_stale.fetch_add(1, Ordering::Relaxed);
                if cache.remove(norm).is_err() {
                    self.counters.cache_errors.fetch_add(1, Ordering::Relaxed);
                }
                return None;
            }
            let current = corpus
                .find(CandidateKind::Workflow, name)
                .map(|c| c.version.as_str());
            if current.is_none() || current != hit.workflow_version.as_deref() {
                self.counters.k1_stale.fetch_add(1, Ordering::Relaxed);
                if cache.remove(norm).is_err() {
                    self.counters.cache_errors.fetch_add(1, Ordering::Relaxed);
                }
                return None;
            }
        }
        let confidence = match &hit.route {
            Route::Workflow { confidence, .. } => *confidence,
            _ => 1.0,
        };
        Some(Outcome::new(
            hit.route,
            Level::K1,
            confidence,
            "intent cache hit",
        ))
    }

    async fn k2_rank(&self, ctx: &RouteContext, corpus: &Corpus) -> Vec<Scored> {
        let mut query_emb = None;
        if self.embedder.is_enabled() && corpus.embeddings.is_some() {
            let call = self
                .embedder
                .embed(std::slice::from_ref(&ctx.text), EmbedTask::Query);
            match tokio::time::timeout(self.config.embed_timeout, call).await {
                Ok(Ok(mut v)) if v.len() == 1 => query_emb = v.pop(),
                _ => {
                    self.counters
                        .k2_embed_errors
                        .fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        corpus.rank(&ctx.text, query_emb.as_deref(), self.config.scoring)
    }

    fn k2_decide(&self, corpus: &Corpus, ranked: &[Scored]) -> Option<Outcome> {
        let cfg = &self.config;
        let top = ranked.first()?;
        let cand = corpus.get(top.index)?;
        let detail = format!(
            "{} (bm25 {:.2}, embed {:.2}, score {:.2})",
            cand.name, top.lexical, top.semantic, top.score
        );
        match cand.kind {
            CandidateKind::Workflow => {
                if top.score < cfg.workflow_threshold {
                    return None;
                }
                if ranked
                    .get(1)
                    .is_some_and(|second| top.score - second.score < cfg.margin)
                {
                    return None;
                }
                if cand.required_params().next().is_some() {
                    return None; // K2 cannot extract parameters: let K3 do it.
                }
                Some(Outcome::new(
                    Route::Workflow {
                        name: cand.name.clone(),
                        params: BTreeMap::new(),
                        confidence: top.score,
                    },
                    Level::K2,
                    top.score,
                    format!("workflow {detail}"),
                ))
            }
            kind @ (CandidateKind::Tool | CandidateKind::Skill) => {
                if top.score < cfg.hint_threshold {
                    return None;
                }
                let names: Vec<String> = ranked
                    .iter()
                    .filter(|s| s.score >= cfg.hint_threshold)
                    .filter_map(|s| corpus.get(s.index))
                    .filter(|c| c.kind == kind)
                    .take(cfg.hint_max)
                    .map(|c| c.name.clone())
                    .collect();
                let route = if kind == CandidateKind::Tool {
                    Route::ToolHint { tools: names }
                } else {
                    Route::SkillHint { skills: names }
                };
                Some(Outcome::new(
                    route,
                    Level::K2,
                    top.score,
                    format!("hint {detail}"),
                ))
            }
        }
    }

    async fn k3(&self, ctx: &RouteContext, corpus: &Corpus, ranked: &[Scored]) -> Outcome {
        let cfg = &self.config;
        let Some(classifier) = &self.classifier else {
            return Outcome::agent("no K2 match, no classifier");
        };
        let mut shown: Vec<&Candidate> = ranked
            .iter()
            .filter(|s| s.score >= cfg.candidate_floor)
            .take(cfg.top_k)
            .filter_map(|s| corpus.get(s.index))
            .collect();
        if shown.is_empty() && corpus.len() <= cfg.small_corpus {
            shown = corpus.candidates.iter().collect();
        }
        if shown.is_empty() {
            return Outcome::agent("no candidates for classifier");
        }
        let prompt = build_prompt(&ctx.text, &shown);
        self.counters.k3_calls.fetch_add(1, Ordering::Relaxed);
        let call = classifier.classify(&prompt, cfg.k3_max_tokens);
        let raw = match tokio::time::timeout(cfg.k3_timeout, call).await {
            Err(_) => {
                self.counters.k3_timeouts.fetch_add(1, Ordering::Relaxed);
                return Outcome::agent("classifier timeout");
            }
            Ok(Err(e)) => {
                self.counters.k3_errors.fetch_add(1, Ordering::Relaxed);
                return Outcome::agent(format!("classifier error: {e}"));
            }
            Ok(Ok(raw)) => raw,
        };
        let limits = VerdictLimits {
            workflow_threshold: cfg.workflow_threshold,
            hint_threshold: cfg.k3_hint_threshold,
        };
        match interpret(&raw, &shown, limits) {
            Ok(Verdict::Routed(route, conf)) => Outcome::new(route, Level::K3, conf, "classifier"),
            Ok(Verdict::Agent(conf, why)) => Outcome::new(Route::Agent, Level::K3, conf, why),
            Err(why) => {
                self.counters.k3_invalid.fetch_add(1, Ordering::Relaxed);
                Outcome::agent(format!("classifier answer rejected: {why}"))
            }
        }
    }
}
