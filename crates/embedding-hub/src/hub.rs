//! The hub: first usable provider of a purpose's chain wins.
//!
//! Privacy rules enforced here (not by callers):
//! * a remote provider is only called when the owner opted in
//!   (`allow_remote`) and the asking bot is not on the deny list;
//! * a batch that contains a private item never leaves the computer;
//! * every text is passed through the secret redactor before it is sent;
//! * the offline hash embedder is always the last resort.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use provider_catalog::embedding::{
    EmbeddingInput, EmbeddingRequest, EmbeddingTask, EmbeddingTaskFormat,
    MAX_EMBEDDING_BATCH_BYTES, MAX_EMBEDDING_INPUT_BYTES, MAX_EMBEDDING_INPUTS,
};
use provider_catalog::entry::{Auth, CompatFlags, ModelCapabilities, ProviderEntry, ProviderKind};
use provider_catalog::{EmbeddingProvider, ModelRef, OpenAiCompatConfig, ProviderError};
use serde::{Deserialize, Serialize};

use crate::catalog::EmbeddingCatalog;
use crate::config::{
    ConfigError, EmbeddingConfig, HASH_DIMENSIONS, ProviderConfig, ProviderType, Purpose,
    VectorSpace,
};
use crate::limiter::{Limits, RateLimiter, Refusal, Usage, estimate_tokens};

/// Document text or a search query (some models treat them differently).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Task {
    /// Text to be stored and found later.
    Document,
    /// A question looking for stored text.
    Query,
}

/// One text to embed.
#[derive(Debug, Clone)]
pub struct EmbedItem {
    /// The text.
    pub text: String,
    /// Marked private: never sent to a remote provider.
    pub private: bool,
}

impl EmbedItem {
    /// A normal (not private) text.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            private: false,
        }
    }
}

/// What happened to one provider during a call (also the health state).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Worked.
    Ok,
    /// Not allowed: no opt-in, bot denied, or private content.
    NotAllowed,
    /// No API key available.
    KeyMissing,
    /// Resting after an earlier failure.
    CoolingDown,
    /// Our own limiter says wait longer than this request may.
    WaitTooLong,
    /// The key was refused (401/403).
    KeyRejected,
    /// The provider said 429 or the quota is used up.
    RateLimited,
    /// Network problem or provider outage.
    Offline,
    /// The provider returned vectors of another size than configured.
    DimensionMismatch,
    /// The provider rejected the request.
    Rejected,
    /// Local model not installed in this build.
    Unavailable,
}

/// One step of the chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attempt {
    /// Provider id.
    pub provider: String,
    /// Result.
    pub outcome: Outcome,
}

/// Vectors plus the space they live in.
#[derive(Debug, Clone)]
pub struct Embedded {
    /// One vector per input, in order, L2-normalised.
    pub vectors: Vec<Vec<f32>>,
    /// The space of every vector.
    pub space: VectorSpace,
    /// Which providers were tried (for the honest "why not Gemini" message).
    pub attempts: Vec<Attempt>,
}

/// No provider could serve a pinned request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unserved {
    /// What happened to each provider.
    pub attempts: Vec<Attempt>,
}

/// A model that runs on this computer (registered by the daemon when the
/// `local-embeddings` build has its artifacts).
pub trait LocalEmbedder: Send + Sync {
    /// Vector size.
    fn dimensions(&self) -> usize;
    /// Model id for the vector space.
    fn model(&self) -> String;
    /// Embed a batch.
    ///
    /// # Errors
    /// Any failure of the local model.
    fn embed(&self, texts: &[String], task: Task) -> Result<Vec<Vec<f32>>, String>;
}

/// Removes secrets from a text before it leaves the computer.
pub type Redactor = Arc<dyn Fn(&str) -> String + Send + Sync>;
/// Looks up an API key by secret-store name (called while (re)configuring).
pub type KeyResolver<'a> = &'a dyn Fn(&str) -> Option<String>;

struct Health {
    cooldown_until: Option<Instant>,
    last: Option<Outcome>,
}

struct Backend {
    cfg: ProviderConfig,
    space: VectorSpace,
    format: EmbeddingTaskFormat,
    max_input_bytes: usize,
    remote: Option<Arc<dyn EmbeddingProvider>>,
    local: Option<Arc<dyn LocalEmbedder>>,
    key_missing: bool,
    limiter: Mutex<RateLimiter>,
    health: Mutex<Health>,
}

struct State {
    config: EmbeddingConfig,
    backends: HashMap<String, Arc<Backend>>,
}

/// One provider's row in Settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderStatus {
    /// Provider id.
    pub id: String,
    /// Last known state.
    pub state: Outcome,
    /// Seconds until it is tried again (0 = now).
    pub retry_in_secs: u64,
    /// Used / limit bars.
    pub usage: Usage,
    /// Whether text goes to another company.
    pub remote: bool,
}

/// Provider-agnostic embeddings. Cheap to share (`Arc`).
pub struct EmbeddingHub {
    state: RwLock<Arc<State>>,
    catalog: EmbeddingCatalog,
    redact: RwLock<Option<Redactor>>,
    locals: Mutex<HashMap<String, Arc<dyn LocalEmbedder>>>,
}

fn entry_for(cfg: &ProviderConfig) -> ProviderEntry {
    let kind = match cfg.kind {
        ProviderType::Gemini => ProviderKind::Gemini,
        _ => ProviderKind::OpenAiCompatible,
    };
    let base_url = cfg.base_url.clone().unwrap_or_else(|| match cfg.kind {
        ProviderType::Gemini => "https://generativelanguage.googleapis.com/v1beta".to_owned(),
        _ => "https://api.openai.com/v1".to_owned(),
    });
    ProviderEntry {
        id: cfg.id.clone(),
        kind,
        base_url,
        auth: Auth::None,
        capabilities: ModelCapabilities {
            tools: false,
            vision: false,
            streaming: false,
            json_mode: false,
        },
        compat_flags: CompatFlags::default(),
        models_endpoint: None,
        models: Vec::new(),
    }
}

fn format_for(cfg: &ProviderConfig) -> EmbeddingTaskFormat {
    match cfg.kind {
        ProviderType::Gemini if cfg.model.contains("embedding-2") => EmbeddingTaskFormat::Gemini2,
        ProviderType::Gemini => EmbeddingTaskFormat::Gemini001,
        ProviderType::Local => EmbeddingTaskFormat::E5,
        _ => EmbeddingTaskFormat::Plain,
    }
}

fn classify(e: &ProviderError) -> (Outcome, Duration) {
    match e {
        ProviderError::Auth(_) => (Outcome::KeyRejected, Duration::from_secs(600)),
        ProviderError::RateLimited { retry_after } => (
            Outcome::RateLimited,
            retry_after.unwrap_or(Duration::from_secs(60)),
        ),
        ProviderError::RetryAfter { delay, .. } => (Outcome::RateLimited, *delay),
        ProviderError::Quota => (Outcome::RateLimited, Duration::from_secs(3600)),
        ProviderError::Connection(_)
        | ProviderError::Timeout
        | ProviderError::Upstream { .. }
        | ProviderError::Interrupted(_)
        | ProviderError::Stream(_) => (Outcome::Offline, Duration::from_secs(30)),
        ProviderError::InvalidRequest(m) if m.contains("dimension mismatch") => {
            (Outcome::DimensionMismatch, Duration::from_secs(600))
        }
        _ => (Outcome::Rejected, Duration::from_secs(60)),
    }
}

fn cut(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn normalise(v: &mut [f32]) {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        for x in v {
            *x /= n;
        }
    }
}

fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// The offline fallback: feature hashing, no model, no network (lexical
/// similarity only).
#[must_use]
pub fn hash_embed(text: &str) -> Vec<f32> {
    let mut v = vec![0.0f32; HASH_DIMENSIONS];
    for tok in text
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        let h = fnv1a(tok);
        #[allow(clippy::cast_possible_truncation)]
        let idx = (h % HASH_DIMENSIONS as u64) as usize;
        v[idx] += if (h >> 63) == 0 { 1.0 } else { -1.0 };
    }
    normalise(&mut v);
    v
}

impl EmbeddingHub {
    /// Build a hub. `keys` resolves secret names, `redact` masks secrets.
    ///
    /// # Errors
    /// The configuration is invalid.
    pub fn new(
        config: EmbeddingConfig,
        keys: KeyResolver<'_>,
        redact: Option<Redactor>,
    ) -> Result<Self, ConfigError> {
        let hub = Self {
            state: RwLock::new(Arc::new(State {
                config: EmbeddingConfig::default(),
                backends: HashMap::new(),
            })),
            catalog: EmbeddingCatalog::builtin(),
            redact: RwLock::new(redact),
            locals: Mutex::new(HashMap::new()),
        };
        hub.reconfigure(config, keys)?;
        Ok(hub)
    }

    /// Install the secret redactor. Without one, remote providers are never
    /// called (fail closed).
    pub fn set_redactor(&self, redact: Option<Redactor>) {
        *self
            .redact
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = redact;
    }

    /// The compiled-in catalog.
    #[must_use]
    pub fn catalog(&self) -> &EmbeddingCatalog {
        &self.catalog
    }

    /// The active configuration.
    #[must_use]
    pub fn config(&self) -> EmbeddingConfig {
        self.snapshot().config.clone()
    }

    fn snapshot(&self) -> Arc<State> {
        self.state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Register the local model under provider id `id`, then call
    /// [`Self::reconfigure`] (or [`Self::refresh`]) to pick it up.
    pub fn register_local(&self, id: &str, local: Arc<dyn LocalEmbedder>) {
        self.locals
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id.to_owned(), local);
    }

    /// Re-read keys and local models with the current configuration.
    pub fn refresh(&self, keys: KeyResolver<'_>) {
        let _ = self.reconfigure(self.config(), keys);
    }

    /// Replace the configuration. Usage counters of unchanged providers
    /// survive; everything else starts fresh.
    ///
    /// # Errors
    /// The configuration is invalid; the old one stays active.
    pub fn reconfigure(
        &self,
        config: EmbeddingConfig,
        keys: KeyResolver<'_>,
    ) -> Result<(), ConfigError> {
        config.validate(&self.catalog)?;
        let old = self.snapshot();
        let locals = self
            .locals
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let mut backends = HashMap::new();
        for cfg in &config.providers {
            let limits = cfg.limits.unwrap_or_else(|| {
                self.catalog
                    .find(cfg.kind.catalog_name(), &cfg.model)
                    .and_then(|m| m.free_tier)
                    .unwrap_or_default()
            });
            let key = cfg.key_ref.as_deref().and_then(keys);
            let local = if cfg.kind == ProviderType::Local {
                locals.get(&cfg.id).cloned()
            } else {
                None
            };
            let remote: Option<Arc<dyn EmbeddingProvider>> = if cfg.kind.is_remote() {
                provider_catalog::build_embedding_provider(
                    &entry_for(cfg),
                    OpenAiCompatConfig {
                        allow_local_network: cfg.allow_local_network,
                        ..OpenAiCompatConfig::default()
                    },
                    key.clone(),
                )
                .ok()
                .map(Arc::from)
            } else {
                None
            };
            let keep = old
                .backends
                .get(&cfg.id)
                .filter(|b| b.cfg.model == cfg.model && b.limiter_limits() == limits);
            let limiter = keep.map_or_else(
                || Mutex::new(RateLimiter::new(limits)),
                |b| {
                    let mut l = b
                        .limiter
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    Mutex::new(std::mem::replace(&mut *l, RateLimiter::new(limits)))
                },
            );
            let max_input_bytes = self
                .catalog
                .find(cfg.kind.catalog_name(), &cfg.model)
                .map_or(MAX_EMBEDDING_INPUT_BYTES, |m| {
                    (m.max_input_tokens * 3).min(MAX_EMBEDDING_INPUT_BYTES)
                });
            backends.insert(
                cfg.id.clone(),
                Arc::new(Backend {
                    space: VectorSpace {
                        provider: cfg.id.clone(),
                        model: cfg.model.clone(),
                        dimensions: local.as_ref().map_or(cfg.dimensions, |l| l.dimensions()),
                    },
                    format: format_for(cfg),
                    max_input_bytes,
                    key_missing: cfg.kind.is_remote()
                        && key.is_none()
                        && cfg.kind != ProviderType::Compatible,
                    remote,
                    local,
                    limiter,
                    health: Mutex::new(Health {
                        cooldown_until: None,
                        last: None,
                    }),
                    cfg: cfg.clone(),
                }),
            );
        }
        *self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Arc::new(State { config, backends });
        Ok(())
    }

    /// Settings rows: state and usage bars of every provider.
    #[must_use]
    pub fn status(&self) -> Vec<ProviderStatus> {
        let now = Instant::now();
        let st = self.snapshot();
        st.config
            .providers
            .iter()
            .filter_map(|p| st.backends.get(&p.id))
            .map(|b| {
                let h = b
                    .health
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let wait = h
                    .cooldown_until
                    .map_or(0, |u| u.saturating_duration_since(now).as_secs());
                let state = if b.cfg.kind == ProviderType::Local && b.local.is_none() {
                    Outcome::Unavailable
                } else if b.key_missing {
                    Outcome::KeyMissing
                } else if wait > 0 {
                    h.last.unwrap_or(Outcome::CoolingDown)
                } else {
                    h.last.unwrap_or(Outcome::Ok)
                };
                ProviderStatus {
                    id: b.cfg.id.clone(),
                    state,
                    retry_in_secs: wait,
                    usage: b
                        .limiter
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .usage(now),
                    remote: b.cfg.kind.is_remote(),
                }
            })
            .collect()
    }

    fn chain(st: &State, purpose: Purpose) -> Vec<Arc<Backend>> {
        st.config
            .chains
            .get(&purpose)
            .into_iter()
            .flatten()
            .filter_map(|id| st.backends.get(id).cloned())
            .collect()
    }

    /// The space the next call of `purpose` would use (ignoring private items
    /// and bot rules): the first provider that is allowed and not resting, else
    /// the offline hash. Stores compare this with their stored space.
    #[must_use]
    pub fn preferred_space(&self, purpose: Purpose) -> VectorSpace {
        let st = self.snapshot();
        let now = Instant::now();
        Self::chain(&st, purpose)
            .into_iter()
            .find(|b| b.usable(now))
            .map_or_else(VectorSpace::hash, |b| b.space.clone())
    }

    /// Embed `items` for `purpose` with the first usable provider.
    ///
    /// All vectors of one call come from one space. With `pin`, only a
    /// provider of exactly that space may answer.
    ///
    /// # Errors
    /// [`Unserved`] when `pin` is set and its provider cannot answer now.
    pub async fn embed(
        &self,
        purpose: Purpose,
        items: &[EmbedItem],
        task: Task,
        bot_id: Option<&str>,
        pin: Option<&VectorSpace>,
        max_wait: Duration,
    ) -> Result<Embedded, Unserved> {
        let st = self.snapshot();
        let allow_remote = !items.iter().any(|i| i.private)
            && !bot_id.is_some_and(|b| st.config.remote_denied_bots.contains(b));
        let mut attempts = Vec::new();
        for b in Self::chain(&st, purpose) {
            if pin.is_some_and(|p| *p != b.space) {
                continue;
            }
            let outcome = self
                .try_backend(&b, items, task, allow_remote, max_wait)
                .await;
            match outcome {
                Ok(vectors) => {
                    attempts.push(Attempt {
                        provider: b.cfg.id.clone(),
                        outcome: Outcome::Ok,
                    });
                    return Ok(Embedded {
                        vectors,
                        space: b.space.clone(),
                        attempts,
                    });
                }
                Err(o) => attempts.push(Attempt {
                    provider: b.cfg.id.clone(),
                    outcome: o,
                }),
            }
        }
        if pin.is_some_and(|p| *p != VectorSpace::hash()) {
            return Err(Unserved { attempts });
        }
        Ok(Embedded {
            vectors: items.iter().map(|i| hash_embed(&i.text)).collect(),
            space: VectorSpace::hash(),
            attempts,
        })
    }

    fn try_local(
        b: &Backend,
        local: &dyn LocalEmbedder,
        items: &[EmbedItem],
        task: Task,
    ) -> Result<Vec<Vec<f32>>, Outcome> {
        let texts: Vec<String> = items.iter().map(|i| i.text.clone()).collect();
        let mut v = local.embed(&texts, task).map_err(|_| Outcome::Rejected)?;
        if v.len() != items.len() || v.iter().any(|x| x.len() != b.space.dimensions) {
            return Err(Outcome::DimensionMismatch);
        }
        for x in &mut v {
            normalise(x);
        }
        Ok(v)
    }

    async fn try_backend(
        &self,
        b: &Backend,
        items: &[EmbedItem],
        task: Task,
        allow_remote: bool,
        max_wait: Duration,
    ) -> Result<Vec<Vec<f32>>, Outcome> {
        if let Some(local) = &b.local {
            return Self::try_local(b, local.as_ref(), items, task);
        }
        let Some(remote) = &b.remote else {
            return Err(Outcome::Unavailable);
        };
        if !(allow_remote && b.cfg.allow_remote) {
            return Err(Outcome::NotAllowed);
        }
        if b.key_missing {
            return Err(Outcome::KeyMissing);
        }
        if let Some(until) = b
            .health
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .cooldown_until
            && until > Instant::now()
        {
            return Err(Outcome::CoolingDown);
        }
        // Secrets never leave; each text is also cut to the model's input size.
        let Some(redact) = self
            .redact
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
        else {
            return Err(Outcome::NotAllowed);
        };
        let texts: Vec<String> = items
            .iter()
            .map(|i| {
                let red = redact(&i.text);
                let t = cut(&red, b.max_input_bytes).trim().to_owned();
                if t.is_empty() {
                    "(empty)".to_owned()
                } else {
                    t
                }
            })
            .collect();
        let mut out = Vec::with_capacity(texts.len());
        let budget = b
            .limiter
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .token_budget();
        let mut start = 0;
        while start < texts.len() {
            let (mut end, mut tokens, mut bytes) = (start, 0u64, 0usize);
            while end < texts.len() && end - start < MAX_EMBEDDING_INPUTS {
                let t = estimate_tokens(&texts[end]);
                if end > start
                    && (budget.is_some_and(|bg| tokens + t > bg)
                        || bytes + texts[end].len() > MAX_EMBEDDING_BATCH_BYTES)
                {
                    break;
                }
                tokens += t;
                bytes += texts[end].len();
                end += 1;
            }
            self.wait_for_slot(b, tokens, max_wait).await?;
            let request = EmbeddingRequest {
                model: ModelRef(b.cfg.model.clone()),
                inputs: texts[start..end]
                    .iter()
                    .map(|t| EmbeddingInput {
                        text: t.clone(),
                        title: None,
                    })
                    .collect(),
                dimensions: b.cfg.dimensions,
                request_dimensions: b.cfg.request_dimensions,
                task: match task {
                    Task::Document => EmbeddingTask::Document,
                    Task::Query => EmbeddingTask::Query,
                },
                format: b.format,
            };
            match remote.embed(request).await {
                Ok(resp) => {
                    out.extend(resp.vectors.into_iter().map(|mut v| {
                        normalise(&mut v);
                        v
                    }));
                }
                Err(e) => {
                    let (outcome, rest) = classify(&e);
                    b.fail(outcome, rest);
                    return Err(outcome);
                }
            }
            start = end;
        }
        b.succeed();
        Ok(out)
    }

    async fn wait_for_slot(
        &self,
        b: &Backend,
        tokens: u64,
        max_wait: Duration,
    ) -> Result<(), Outcome> {
        let started = Instant::now();
        loop {
            let r = b
                .limiter
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .reserve(Instant::now(), tokens);
            match r {
                Ok(()) => return Ok(()),
                Err(Refusal::TooLarge) => return Err(Outcome::Rejected),
                Err(Refusal::Wait(d)) => {
                    if started.elapsed() + d > max_wait {
                        return Err(Outcome::WaitTooLong);
                    }
                    tokio::time::sleep(d).await;
                }
            }
        }
    }

    /// Owner pressed "Test connection": one fixed harmless sentence to one
    /// provider (no user text). Needs the provider's opt-in like any call.
    pub async fn test(&self, provider_id: &str) -> Outcome {
        let st = self.snapshot();
        let Some(b) = st.backends.get(provider_id).cloned() else {
            return Outcome::Unavailable;
        };
        let items = [EmbedItem::new(
            "Hello from Praxi. This is a connection test.",
        )];
        match self
            .try_backend(&b, &items, Task::Query, true, Duration::from_secs(5))
            .await
        {
            Ok(_) => Outcome::Ok,
            Err(o) => o,
        }
    }
}

impl Backend {
    fn limiter_limits(&self) -> Limits {
        self.limiter
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .limits()
    }

    fn usable(&self, now: Instant) -> bool {
        if self.local.is_some() {
            return true;
        }
        self.remote.is_some()
            && self.cfg.allow_remote
            && !self.key_missing
            && self
                .health
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .cooldown_until
                .is_none_or(|u| u <= now)
    }

    fn fail(&self, outcome: Outcome, rest: Duration) {
        let now = Instant::now();
        let mut h = self
            .health
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        h.cooldown_until = Some(now + rest);
        h.last = Some(outcome);
        if outcome == Outcome::RateLimited {
            self.limiter
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .block_for(now, rest);
        }
    }

    fn succeed(&self) {
        let mut h = self
            .health
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        h.cooldown_until = None;
        h.last = Some(Outcome::Ok);
    }
}
