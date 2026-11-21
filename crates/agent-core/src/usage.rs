//! Purpose-labelled model calls. Only identifiers and numeric usage enter this ledger.
//!
//! A durable intent precedes every invocation; the guard writes its outcome even
//! when a timeout or cancellation drops the future. An unfinished intent remains
//! countable after a process crash. Router scopes are task-local (never bot-global).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::sync::Arc;
use std::time::Instant;

use agent_router::ladder::{BackendError, EmbedTask, Embedder};
use async_trait::async_trait;
use parking_lot::Mutex;
use protocol::event::RunError;
use protocol::{Event, RunId, UserId};
use provider_catalog::Usage;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use storage::AuditEntry;
use uuid::Uuid;

use crate::shared::{RunCtx, STEP_TOOLS, Shared};

/// The reason for a model invocation, independent of provider or model identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallPurpose {
    /// Main turn reasoning and tool selection.
    Executor,
    /// The final, tools-disabled response after a loop guard fires.
    GuardClose,
    /// K3 routing classifier.
    Router,
    /// K2 embedding request (tokens are not reported by that backend).
    Embedding,
    /// Background memory extraction or consolidation.
    Memory,
    /// A model invocation of a child run.
    Subagent,
    /// One attempt by the optional decision pool.
    Decider,
}

impl CallPurpose {
    /// Stable wire/audit key.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Executor => "executor",
            Self::GuardClose => "guard_close",
            Self::Router => "router",
            Self::Embedding => "embedding",
            Self::Memory => "memory",
            Self::Subagent => "subagent",
            Self::Decider => "decider",
        }
    }
}

/// A started invocation, optionally completed with reported token usage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageCall {
    /// Opaque invocation id, shared by intent and outcome rows.
    pub call_id: Uuid,
    /// Purpose, using the same stable keys as run summaries and evals.
    pub purpose: CallPurpose,
    /// Provider/model reference, passed through the production redactor.
    pub model: String,
    /// Durable step index (also permits mixed pre-ledger / resumed histories).
    pub step_n: u32,
    /// `None` when the provider did not report usage or the call was interrupted.
    #[serde(default)]
    pub tokens: Option<Usage>,
    /// Price estimate captured when this invocation completed.
    #[serde(default)]
    pub cost_usd: Option<f64>,
}

/// Reconcile durable call intents and outcomes without double counting. Old
/// audit rows without a call id are ignored; callers can fall back to the
/// historical model-step representation for those runs.
#[must_use]
pub fn calls_from_audit<'a>(
    entries: impl IntoIterator<Item = &'a AuditEntry>,
) -> BTreeMap<RunId, Vec<UsageCall>> {
    let mut calls = BTreeMap::new();
    for row in entries {
        if !matches!(row.action.as_str(), "llm.call_started" | "usage")
            || row.detail["ledger_version"] != 1
        {
            continue;
        }
        if let Some(run) = row.run_id
            && let Ok(call) = serde_json::from_value::<UsageCall>(row.detail.clone())
        {
            let key = (run, call.call_id);
            if row.action == "usage" {
                calls.insert(key, call);
            } else {
                calls.entry(key).or_insert(call);
            }
        }
    }
    let mut by_run: BTreeMap<RunId, Vec<UsageCall>> = BTreeMap::new();
    for ((run, _), call) in calls {
        by_run.entry(run).or_default().push(call);
    }
    by_run
}

#[derive(Default)]
struct ActivityState {
    pending: usize,
    finished: bool,
}

/// Tracks run/background lifetimes, not the token totals (the audit is durable).
#[derive(Default)]
pub(crate) struct Ledger(Mutex<HashMap<RunId, ActivityState>>);

/// Keeps a run and its ancestors open for accounting until work actually ends.
pub(crate) struct Activity {
    sh: Arc<Shared>,
    runs: Vec<RunId>,
}

impl Activity {
    pub(crate) fn new(sh: &Arc<Shared>, run_id: RunId) -> Self {
        let mut runs = Vec::new();
        let mut next = Some(run_id);
        let mut user = None;
        while let Some(id) = next {
            let Ok(Some(run)) = sh.storage.get_run(id) else {
                break;
            };
            if runs.contains(&id) || user.is_some_and(|u| u != run.user_id) {
                break;
            }
            user = Some(run.user_id);
            runs.push(id);
            next = run.parent_run_id;
        }
        let mut state = sh.usage.0.lock();
        for id in &runs {
            state.entry(*id).or_default().pending += 1;
        }
        drop(state);
        Self {
            sh: Arc::clone(sh),
            runs,
        }
    }
}

impl Drop for Activity {
    fn drop(&mut self) {
        let mut state = self.sh.usage.0.lock();
        for id in &self.runs {
            if let Some(s) = state.get_mut(id) {
                s.pending = s.pending.saturating_sub(1);
            }
        }
        flush_summaries(&self.sh, &mut state);
    }
}

pub(crate) fn finish(sh: &Shared, run: RunId) {
    // Cancellation may only signal a live drive task. Its in-flight call and
    // background accounting must finish before a summary becomes eligible.
    if !sh
        .storage
        .get_run(run)
        .ok()
        .flatten()
        .and_then(|r| task_runtime::RunState::parse(&r.state))
        .is_some_and(task_runtime::RunState::is_terminal)
    {
        return;
    }
    let mut state = sh.usage.0.lock();
    state.entry(run).or_default().finished = true;
    flush_summaries(sh, &mut state);
}

#[derive(Default, Serialize)]
struct Totals {
    calls: u64,
    prompt_tokens: u64,
    completion_tokens: u64,
    cached_tokens: u64,
    cost_usd: Option<f64>,
}

fn flush_summaries(sh: &Shared, state: &mut HashMap<RunId, ActivityState>) {
    let ready: Vec<_> = state
        .iter()
        .filter_map(|(id, s)| (s.finished && s.pending == 0).then_some(*id))
        .collect();
    for id in ready {
        if write_summary(sh, id).is_ok() {
            state.remove(&id);
        }
    }
}

fn write_summary(sh: &Shared, id: RunId) -> storage::Result<()> {
    let Some(run) = sh.storage.get_run(id)? else {
        return Ok(());
    };
    let rows = sh.storage.list_audit(run.user_id)?;
    if rows
        .iter()
        .any(|a| a.run_id == Some(id) && a.action == "run.usage_summary")
    {
        return Ok(());
    }
    let mut included = HashSet::from([id]);
    let mut queue = vec![id];
    while let Some(parent) = queue.pop() {
        for child in sh.storage.list_child_runs(parent)? {
            if child.user_id == run.user_id && included.insert(child.id) {
                queue.push(child.id);
            }
        }
    }
    let mut totals: BTreeMap<CallPurpose, Totals> = BTreeMap::new();
    for (run_id, calls) in calls_from_audit(&rows) {
        if !included.contains(&run_id) {
            continue;
        }
        for call in calls {
            let purpose = if run_id == id {
                call.purpose
            } else {
                CallPurpose::Subagent
            };
            let t = totals.entry(purpose).or_default();
            t.calls = t.calls.saturating_add(1);
            if let Some(u) = call.tokens {
                t.prompt_tokens = t.prompt_tokens.saturating_add(u.prompt_tokens);
                t.completion_tokens = t.completion_tokens.saturating_add(u.completion_tokens);
                t.cached_tokens = t.cached_tokens.saturating_add(u.cached_tokens.unwrap_or(0));
            }
            if let Some(cost) = call.cost_usd {
                *t.cost_usd.get_or_insert(0.0) += cost;
            }
        }
    }
    sh.audit_usage_record(run.user_id, id, "run.usage_summary", &json!(totals))?;
    Ok(())
}

impl Shared {
    /// Production redaction for every string; no provider response/error text
    /// or prompt is accepted by the call guard in the first place.
    fn audit_usage_record(
        &self,
        user: UserId,
        run: RunId,
        action: &str,
        detail: &Value,
    ) -> storage::Result<()> {
        let mut detail = detail.clone();
        if let Some(redactor) = &self.cfg.redactor {
            crate::shared::redact_value(redactor, &mut detail);
        }
        self.storage
            .append_audit(user, Some(run), "bot", action, &detail)?;
        Ok(())
    }
}

/// Refuse a call that would take month-to-date spend past the monthly limit;
/// warn once per month at 80%. No limit, or a model without a known price,
/// means no check (an unpriced call adds no known cost and is never guessed).
///
/// On success returns the estimate reserved for this call (USD); the caller
/// must release it with [`release_reservation`] when the call ends.
fn check_spend_limit(sh: &Arc<Shared>, ctx: &RunCtx, model: &str) -> Result<f64, String> {
    use crate::spend::{SPEND_LIMIT_REACHED, estimate_usd, month_bounds_ms, spend_status};
    let Some(limit) = *sh.spend.limit.read() else {
        return Ok(0.0);
    };
    let Some(price) = sh.price_for(model) else {
        return Ok(0.0);
    };
    // Held across check and reservation so parallel calls cannot all pass.
    let mut reserved = sh.spend.reserved.lock();
    // Fail closed: with a limit set, an unreadable ledger must not allow spending.
    let status = spend_status(&sh.storage, ctx.user_id, storage::now_ms())
        .map_err(|_| "usage ledger unavailable".to_owned())?;
    let estimate = estimate_usd(
        &sh.storage,
        ctx.user_id,
        ctx.run_id,
        price,
        sh.cfg.max_tokens,
    );
    if status.spent_usd + reserved.usd + estimate > limit {
        let _ = sh.audit_usage_record(
            ctx.user_id,
            ctx.run_id,
            "spend.limit_blocked",
            &json!({
                "limit_usd": limit, "spent_usd": status.spent_usd,
                "estimate_usd": estimate, "model": model,
            }),
        );
        let blocked_at = status.spent_usd + reserved.usd;
        drop(reserved);
        let hint = if limit <= 0.0 {
            " The saved limit could not be read, so spending is blocked until you set it again."
        } else {
            ""
        };
        return Err(format!(
            "[{SPEND_LIMIT_REACHED}] Monthly spending limit reached: ${blocked_at:.2} spent or \
             in flight of ${limit:.2} this month (the next call could exceed it). The model was \
             not called. Raise or remove the limit on the Usage page, then run again.{hint}",
        ));
    }
    reserved.add(estimate);
    drop(reserved);
    if status.spent_usd >= 0.8 * limit {
        let month = month_bounds_ms(storage::now_ms()).0;
        let first = warn_is_new(sh, ctx, month, limit);
        if first {
            let message = format!(
                "Monthly spending limit: 80% used (${:.2} of ${limit:.2}). New model calls stop \
                 at the limit; raise it on the Usage page.",
                status.spent_usd
            );
            let _ = sh.audit_usage_record(
                ctx.user_id,
                ctx.run_id,
                "spend.limit_warning",
                &json!({ "limit_usd": limit, "spent_usd": status.spent_usd }),
            );
            sh.emit(
                ctx,
                Event::Error(RunError {
                    run_id: ctx.run_id,
                    message,
                    recoverable: true,
                }),
            );
        }
    }
    Ok(estimate)
}

/// Whether the 80% warning for this month and limit has not been issued yet.
/// The in-memory marker is the fast path; the durable one is the
/// `spend.limit_warning` audit row, so a restart does not repeat the warning.
fn warn_is_new(sh: &Arc<Shared>, ctx: &RunCtx, month: i64, limit: f64) -> bool {
    let mut marker = sh.spend.warned_month.lock();
    if *marker == Some(month) {
        return false;
    }
    let already = sh
        .storage
        .list_audit_action_since(ctx.user_id, "spend.limit_warning", month)
        .is_ok_and(|rows| {
            rows.iter()
                .any(|r| r.detail["limit_usd"].as_f64() == Some(limit))
        });
    *marker = Some(month);
    !already
}

fn release_reservation(sh: &Shared, amount: f64) {
    sh.spend.reserved.lock().release(amount);
}

/// One actual provider invocation (not one model step containing retries).
pub(crate) struct CallGuard {
    sh: Arc<Shared>,
    run: RunId,
    user: UserId,
    detail: Value,
    started: Instant,
    /// Spend-limit estimate held while the call is in flight.
    reserved_usd: f64,
}

impl CallGuard {
    pub(crate) fn start(
        sh: &Arc<Shared>,
        ctx: &RunCtx,
        model: &str,
        purpose: CallPurpose,
    ) -> Result<Self, String> {
        let steps = sh
            .storage
            .list_steps(ctx.run_id)
            .map_err(|_| "usage ledger unavailable".to_owned())?;
        let purpose = if ctx.parent_run_id.is_some() {
            CallPurpose::Subagent
        } else {
            purpose
        };
        let detail = json!({
            "ledger_version": 1, "call_id": Uuid::now_v7(),
            "purpose": purpose, "model": model, "calls": 1,
            "step_n": steps.last().map_or(0, |s| s.idx),
            "tool_rounds": steps.iter().filter(|s| s.name == STEP_TOOLS).count(),
            "tokens": null, "prompt_tokens": null, "completion_tokens": null,
            "cached_tokens": null, "cost_usd": null,
        });
        let reserved_usd = check_spend_limit(sh, ctx, model)?;
        if sh
            .audit_usage_record(ctx.user_id, ctx.run_id, "llm.call_started", &detail)
            .is_err()
        {
            release_reservation(sh, reserved_usd);
            return Err("usage ledger unavailable".to_owned());
        }
        Ok(Self {
            sh: Arc::clone(sh),
            run: ctx.run_id,
            user: ctx.user_id,
            detail,
            started: Instant::now(),
            reserved_usd,
        })
    }

    /// The model tier this call ran on, when tiering is active for the step.
    pub(crate) fn tier(&mut self, tier: Option<&str>) {
        if let Some(tier) = tier {
            self.detail["tier"] = json!(tier);
        }
    }

    pub(crate) fn usage(&mut self, usage: &Usage) {
        self.detail["tokens"] = json!(usage);
        self.detail["prompt_tokens"] = json!(usage.prompt_tokens);
        self.detail["completion_tokens"] = json!(usage.completion_tokens);
        self.detail["cached_tokens"] = json!(usage.cached_tokens);
        self.detail["cost_usd"] = json!(
            self.detail["model"]
                .as_str()
                .and_then(|model| self.sh.price_for(model))
                .map(|p| p.cost(
                    usage.prompt_tokens,
                    usage.completion_tokens,
                    usage.cached_tokens
                ))
        );
        if self.detail["model"]
            .as_str()
            .is_some_and(|m| m.starts_with("cli:"))
        {
            self.detail["cost_source"] = json!("subscription_estimate");
        }
    }
}

impl Drop for CallGuard {
    fn drop(&mut self) {
        self.detail["latency_ms"] =
            json!(u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX));
        let _ = self
            .sh
            .audit_usage_record(self.user, self.run, "usage", &self.detail);
        // Settled: the ledger row now carries the real cost (released after the
        // write, so the estimate is never missing from both).
        release_reservation(&self.sh, self.reserved_usd);
    }
}

#[derive(Clone)]
pub(crate) struct Scope {
    pub sh: Arc<Shared>,
    pub ctx: Arc<RunCtx>,
}

tokio::task_local! { static ROUTER_SCOPE: Scope; }

impl Scope {
    pub(crate) async fn run<F: Future>(self, future: F) -> F::Output {
        ROUTER_SCOPE.scope(self, future).await
    }
    pub(crate) fn current() -> Option<Self> {
        ROUTER_SCOPE.try_with(Clone::clone).ok()
    }
    pub(crate) fn call(model: &str, purpose: CallPurpose) -> Result<CallGuard, BackendError> {
        let scope = Self::current().ok_or_else(|| BackendError::new("missing usage context"))?;
        CallGuard::start(&scope.sh, &scope.ctx, model, purpose).map_err(BackendError::new)
    }
}

/// Attribute each embedding request, including candidate refreshes, to the run
/// that caused it. Gemini's transport batch limit is exposed by the trait so a
/// 250-document refresh records three actual requests, not one logical batch.
pub(crate) struct CountedEmbedder(pub Arc<dyn Embedder>);

#[async_trait]
impl Embedder for CountedEmbedder {
    fn is_enabled(&self) -> bool {
        self.0.is_enabled()
    }
    async fn embed(
        &self,
        texts: &[String],
        task: EmbedTask,
    ) -> Result<Vec<Vec<f32>>, BackendError> {
        let mut out = Vec::with_capacity(texts.len());
        for chunk in texts.chunks(self.0.max_batch_size().max(1)) {
            let _call = Scope::call(&self.0.model_name(), CallPurpose::Embedding)?;
            out.extend(self.0.embed(chunk, task).await?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ledger_reconciles_completed_and_crashed_intents_without_double_counting() {
        let run = RunId::new();
        let mut started = AuditEntry {
            id: 1,
            user_id: UserId::new(),
            run_id: Some(run),
            actor: "bot".into(),
            action: "llm.call_started".into(),
            ts_ms: 0,
            detail: json!({"ledger_version": 1, "call_id": Uuid::now_v7(),
                "model": "fake/m", "purpose": "executor", "step_n": 1, "tokens": null}),
        };
        let mut completed = started.clone();
        completed.id = 2;
        completed.action = "usage".into();
        completed.detail["tokens"] = json!(Usage {
            prompt_tokens: 10,
            ..Usage::default()
        });
        // Completed data wins even if the caller supplies rows out of order.
        let calls = calls_from_audit([&completed, &started]);
        assert_eq!(calls[&run].len(), 1);
        assert_eq!(calls[&run][0].tokens.unwrap().prompt_tokens, 10);
        // A second invocation crashed before it could emit an outcome.
        started.detail["call_id"] = json!(Uuid::now_v7());
        let calls = calls_from_audit([&completed, &started]);
        assert_eq!(calls[&run].len(), 2);
        assert_eq!(calls[&run].iter().filter(|c| c.tokens.is_none()).count(), 1);
        // Legacy rows are not silently mistaken for new call records.
        started
            .detail
            .as_object_mut()
            .unwrap()
            .remove("ledger_version");
        assert!(calls_from_audit([&started]).is_empty());
    }
}
