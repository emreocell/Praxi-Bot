//! Loop configuration: guards, pricing, global policy.

use std::collections::HashMap;
use std::time::Duration;

use std::sync::Arc;

use memory_store::MemoryStore;
use parking_lot::RwLock;
use permission_engine::Policy;
use secret_store::{Redactor, SecretStore};
use storage::EventJournal;
use task_runtime::RetryPolicy;

/// Profile-visible, non-namespaced tool count above which the model sees the
/// discovery tools instead of every tool, unless
/// [`LoopConfig::discovery_threshold`] says otherwise.  Namespaced (`mcp:`)
/// tools never count.  Same value as the router's own default.
///
/// The built-in set (with `spawn_subagent`, the skill and the workflow tools)
/// is just over the router's own default of 25; the model should see these
/// directly and only switch to `find_tool` / `use_tool` once plugins or MCP
/// servers add many more.
pub const DEFAULT_DIRECT_TOOL_LIMIT: usize = agent_router::DEFAULT_DISCOVERY_THRESHOLD;

/// Default [`LoopConfig::schedule_approval_timeout`]: 30 minutes.
pub const DEFAULT_SCHEDULE_APPROVAL_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Per-turn safety limits.
#[derive(Debug, Clone)]
pub struct Guards {
    /// Maximum model calls in one turn.
    pub max_steps: u32,
    /// Timeout for a single tool call.
    pub tool_timeout: Duration,
    /// Timeout for one model call including all its provider retries.
    pub model_timeout: Duration,
    /// Identical consecutive tool calls at which a warning is injected.
    pub repeat_warn: u32,
    /// Identical consecutive tool calls at which the turn is stopped.
    pub repeat_stop: u32,
    /// Token budget (prompt + completion, summed over the turn's model calls).
    pub max_tokens_per_turn: u64,
    /// Tool output above this many bytes is spilled to a file.
    pub spill_threshold_bytes: usize,
    /// Bytes of a spilled output kept inline as a preview.
    pub spill_preview_bytes: usize,
}

impl Default for Guards {
    fn default() -> Self {
        Self {
            max_steps: 40,
            tool_timeout: Duration::from_secs(120),
            model_timeout: Duration::from_secs(300),
            repeat_warn: 3,
            repeat_stop: 5,
            max_tokens_per_turn: 2_000_000,
            spill_threshold_bytes: 16 * 1024,
            spill_preview_bytes: 2 * 1024,
        }
    }
}

/// USD price per million tokens for a model.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelPrice {
    /// Uncached prompt tokens.
    pub input_per_mtok: f64,
    /// Completion tokens.
    pub output_per_mtok: f64,
    /// Cached prompt tokens (falls back to `input_per_mtok`).
    pub cached_per_mtok: Option<f64>,
}

impl ModelPrice {
    /// Cost in USD for the given token counts.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn cost(&self, prompt: u64, completion: u64, cached: Option<u64>) -> f64 {
        let cached = cached.unwrap_or(0).min(prompt);
        let fresh = prompt - cached;
        let cached_price = self.cached_per_mtok.unwrap_or(self.input_per_mtok);
        (fresh as f64 * self.input_per_mtok
            + cached as f64 * cached_price
            + completion as f64 * self.output_per_mtok)
            / 1_000_000.0
    }
}

/// Memory hook configuration (ADR-0017): recall before a turn, extraction after.
#[derive(Debug, Clone)]
pub struct MemoryConfig {
    /// The store.
    pub store: Arc<MemoryStore>,
    /// `memory.enabled`; defaults to `true` when a store is given.
    pub enabled: bool,
    /// Token budget of the recall block prepended to the user message.
    pub recall_budget_tokens: usize,
}

impl MemoryConfig {
    /// Enabled hook over `store` with an 800-token recall budget.
    #[must_use]
    pub fn new(store: Arc<MemoryStore>) -> Self {
        Self {
            store,
            enabled: true,
            recall_budget_tokens: 800,
        }
    }
}

/// Refresh callback of a [`SharedRedactor`] (re-reads the secret sources).
pub type RedactorRefreshFn = Arc<dyn Fn(&SharedRedactor) + Send + Sync>;

#[derive(Default)]
struct RedactorInner {
    redactor: RwLock<Redactor>,
    /// Values registered by hand (`register`); they survive a refresh.
    pinned: parking_lot::Mutex<Vec<String>>,
    refresher: RwLock<Option<RedactorRefreshFn>>,
}

/// A [`Redactor`] that can be replaced while the loop runs (secrets are added
/// and rotated after startup).  Clones share the same underlying redactor, so
/// the daemon keeps a handle to refresh what the loop already uses.
///
/// Every registered secret is also masked in its JSON-escaped, URL-encoded and
/// base64 (standard and URL-safe) forms.
#[derive(Clone, Default)]
pub struct SharedRedactor(Arc<RedactorInner>);

impl std::fmt::Debug for SharedRedactor {
    // Never print the registered secret values.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SharedRedactor(..)")
    }
}

impl From<Redactor> for SharedRedactor {
    fn from(r: Redactor) -> Self {
        Self::new(r)
    }
}

/// `value` plus its JSON-escaped, URL-encoded and base64 (standard and
/// URL-safe, at each of the three byte alignments) renderings.
#[must_use]
pub fn secret_variants(value: &str) -> Vec<String> {
    use base64::Engine as _;
    use base64::engine::general_purpose::{STANDARD_NO_PAD, URL_SAFE_NO_PAD};
    let mut out = vec![value.to_owned()];
    if let Ok(j) = serde_json::to_string(value) {
        out.push(j[1..j.len() - 1].to_owned());
    }
    let mut url = String::new();
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            url.push(char::from(b));
        } else {
            {
                use std::fmt::Write as _;
                let _ = write!(url, "%{b:02X}");
            }
        }
    }
    out.push(url);
    // Base64 of a substring depends on its alignment in the encoded text; the
    // stable middle of each of the 3 alignments is what appears in practice.
    for k in 0..3usize {
        let mut bytes = vec![0u8; k];
        bytes.extend_from_slice(value.as_bytes());
        let lead = [0, 2, 3][k];
        let trail = usize::from(bytes.len() % 3 != 0);
        for enc in [
            STANDARD_NO_PAD.encode(&bytes),
            URL_SAFE_NO_PAD.encode(&bytes),
        ] {
            if enc.len() > lead + trail {
                out.push(enc[lead..enc.len() - trail].to_owned());
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

fn add_with_variants(r: &mut Redactor, value: &str) {
    for v in secret_variants(value) {
        r.add_secret(&v);
    }
}

impl SharedRedactor {
    /// Share `r`.
    #[must_use]
    pub fn new(r: Redactor) -> Self {
        let s = Self::default();
        *s.0.redactor.write() = r;
        s
    }

    /// Replace the redactor atomically (pinned values are kept).
    pub fn replace(&self, mut r: Redactor) {
        for v in self.0.pinned.lock().iter() {
            add_with_variants(&mut r, v);
        }
        *self.0.redactor.write() = r;
    }

    /// Mask `value` (and its encoded forms) from now on, immediately and
    /// across later refreshes.
    pub fn register(&self, value: &str) {
        if value.len() < 6 {
            return;
        }
        {
            let mut pinned = self.0.pinned.lock();
            if !pinned.iter().any(|p| p == value) {
                pinned.push(value.to_owned());
            }
        }
        add_with_variants(&mut self.0.redactor.write(), value);
    }

    /// Rebuild from every value in `store` plus `extra` literal secrets (the
    /// daemon token, the vault passphrase, environment keys), each with its
    /// encoded variants.  On a backend failure the previous redactor stays in
    /// place (fail closed: nothing already known is dropped) and the error is
    /// returned.
    ///
    /// # Errors
    /// Backend failures while reading the store.
    pub fn refresh_from_store(
        &self,
        store: Option<&dyn SecretStore>,
        extra: &[String],
    ) -> Result<(), secret_store::SecretError> {
        let mut r = Redactor::new();
        if let Some(s) = store {
            for name in s.list_names()? {
                if let Some(v) = s.get(&name)? {
                    add_with_variants(&mut r, v.expose());
                }
            }
        }
        for e in extra {
            add_with_variants(&mut r, e);
        }
        self.replace(r);
        Ok(())
    }

    /// Install the callback [`Self::refresh_now`] runs.
    pub fn set_refresher(&self, f: RedactorRefreshFn) {
        *self.0.refresher.write() = Some(f);
    }

    /// Re-read the secret sources now (no-op without a refresher).
    pub fn refresh_now(&self) {
        let f = self.0.refresher.read().clone();
        if let Some(f) = f {
            f(self);
        }
    }

    /// Mask all known values and patterns in `text`.
    #[must_use]
    pub fn redact(&self, text: &str) -> String {
        self.0.redactor.read().redact(text)
    }

    /// Whether `text` contains anything that would be masked.
    #[must_use]
    pub fn contains_secret(&self, text: &str) -> bool {
        self.0.redactor.read().contains_secret(text)
    }
}

/// Lazily created (or injected) event journal of one loop.  Cloning yields a
/// fresh *empty* slot, so a cloned config never shares a writer with the loop
/// it was cloned from.
#[derive(Default)]
pub struct JournalSlot(std::sync::OnceLock<Arc<EventJournal>>);

impl Clone for JournalSlot {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl std::fmt::Debug for JournalSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("JournalSlot(..)")
    }
}

impl JournalSlot {
    /// A slot with `journal` preinstalled (custom sinks, tests).
    #[must_use]
    pub fn with_journal(journal: EventJournal) -> Self {
        let slot = Self::default();
        let _ = slot.0.set(Arc::new(journal));
        slot
    }

    pub(crate) fn get_or_init(&self, init: impl FnOnce() -> EventJournal) -> &Arc<EventJournal> {
        self.0.get_or_init(|| Arc::new(init()))
    }
}

/// Configuration of an [`AgentLoop`](crate::AgentLoop).
#[derive(Debug, Clone)]
pub struct LoopConfig {
    /// Guards.
    pub guards: Guards,
    /// Workspace-wide policy (ADR-0012); bots override per category.
    pub global_policy: Policy,
    /// Retry policy for model steps (they are idempotent).
    pub model_retry: RetryPolicy,
    /// Prices keyed by the full model reference (`provider/model`).
    pub prices: HashMap<String, ModelPrice>,
    /// Tools with `Effect::Read` skip an `Ask` that comes from the policy level
    /// (never from the absolute floors).
    pub read_tools_skip_ask: bool,
    /// Response token cap sent to the provider.
    pub max_tokens: Option<u32>,
    /// Sampling temperature.
    pub temperature: Option<f64>,
    /// Registry size above which the model sees the `find_tool` /
    /// `get_tool_details` / `use_tool` discovery tools instead of every tool
    /// (default [`DEFAULT_DIRECT_TOOL_LIMIT`]).
    pub discovery_threshold: usize,
    /// Thresholds and budgets of the K0-K3 decision ladder.
    pub ladder: agent_router::LadderConfig,
    /// Hard cap on the whole routing decision of a turn; when it is hit the
    /// turn goes to the main model (`Agent`, K4).
    pub router_budget: Duration,
    /// Masks secrets in tool outputs and emitted events when set.
    pub redactor: Option<SharedRedactor>,
    /// Memory hook; `None` = no memory.
    pub memory: Option<MemoryConfig>,
    /// How long a schedule-started run waits for an approval (or a question
    /// answer) before it pauses (default [`DEFAULT_SCHEDULE_APPROVAL_TIMEOUT`]).
    pub schedule_approval_timeout: Duration,
    /// Where events are journaled.  Empty by default: the loop then creates a
    /// background writer over its own storage on first use.
    pub journal: JournalSlot,
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            guards: Guards::default(),
            global_policy: Policy::approve_destructive(),
            model_retry: RetryPolicy {
                max_attempts: 3,
                initial_backoff_ms: 500,
                multiplier: 2,
                max_backoff_ms: 8_000,
            },
            prices: HashMap::new(),
            read_tools_skip_ask: true,
            max_tokens: None,
            temperature: None,
            discovery_threshold: DEFAULT_DIRECT_TOOL_LIMIT,
            ladder: agent_router::LadderConfig::default(),
            router_budget: Duration::from_secs(2),
            redactor: None,
            memory: None,
            schedule_approval_timeout: DEFAULT_SCHEDULE_APPROVAL_TIMEOUT,
            journal: JournalSlot::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_splits_cached_tokens() {
        let p = ModelPrice {
            input_per_mtok: 2.0,
            output_per_mtok: 10.0,
            cached_per_mtok: Some(0.5),
        };
        let c = p.cost(1_000_000, 100_000, Some(400_000));
        assert!((c - (1.2 + 0.2 + 1.0)).abs() < 1e-9);
    }
}
