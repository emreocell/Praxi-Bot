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

/// One reconciled invocation together with its audit timestamp (`ts_ms`).
#[derive(Debug, Clone)]
pub struct RecordedCall {
    /// Invocation details from the ledger.
    pub call: UsageCall,
    /// Audit timestamp (ms since Unix epoch): the outcome row's timestamp when
    /// completed, otherwise the intent row's timestamp.
    pub ts_ms: i64,
}

/// Reconcile durable call intents and outcomes without double counting, keeping
/// each invocation's audit timestamp so callers can bucket calls by day.
/// Old audit rows without a ledger version are ignored.
#[must_use]
pub fn calls_with_timestamps_from_audit<'a>(
    entries: impl IntoIterator<Item = &'a AuditEntry>,
) -> BTreeMap<RunId, Vec<RecordedCall>> {
    let mut calls: BTreeMap<(RunId, Uuid), RecordedCall> = BTreeMap::new();
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
            let recorded = RecordedCall {
                call,
                ts_ms: row.ts_ms,
            };
            if row.action == "usage" {
                calls.insert(key, recorded);
            } else {
                calls.entry(key).or_insert(recorded);
            }
        }
    }
    let mut by_run: BTreeMap<RunId, Vec<RecordedCall>> = BTreeMap::new();
    for ((run, _), recorded) in calls {
        by_run.entry(run).or_default().push(recorded);
    }
    by_run
}

/// Reconcile durable call intents and outcomes without double counting. Old
/// audit rows without a call id are ignored; callers can fall back to the
/// historical model-step representation for those runs.
#[must_use]
pub fn calls_from_audit<'a>(
    entries: impl IntoIterator<Item = &'a AuditEntry>,
) -> BTreeMap<RunId, Vec<UsageCall>> {
    calls_with_timestamps_from_audit(entries)
        .into_iter()
        .map(|(run, calls)| (run, calls.into_iter().map(|r| r.call).collect()))
        .collect()
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

/// Per-bot limits (`bot_limits.rs`): refuse a call that would break a hard
/// limit, notify at 80% / 100%. Returns the reservation held while the call is
/// in flight plus the estimates to record in the ledger row (so a call whose
/// provider reports no usage still counts).
///
/// Fails closed: an unreadable limit or ledger blocks the call.
fn check_bot_limits(
    sh: &Arc<Shared>,
    ctx: &RunCtx,
    model: &str,
    prompt_hint: Option<u64>,
) -> Result<Option<(crate::bot_limits::Reservation, f64, u64, u64)>, String> {
    use crate::bot_limits::{
        BOT_LIMIT_REACHED, Period, PeriodLimit, Reservation, crossed_alerts, estimate_tokens,
        find_breach, load_limits, period_usage,
    };
    let unavailable = |what: &str| {
        format!(
            "[{BOT_LIMIT_REACHED} period=unknown metric=unknown resets_ms=0] Bot spending limits \
             could not be checked ({what}), so the model was not called."
        )
    };
    let limits = load_limits(&sh.storage, ctx.bot_id).map_err(|e| unavailable(&e))?;
    if !limits.any() {
        return Ok(None);
    }
    let now = storage::now_ms();
    let price = sh.price_for(model);
    let prompt_seen = sh
        .storage
        .max_run_prompt_tokens(ctx.user_id, ctx.run_id)
        .ok()
        .flatten();
    let (prompt_est, completion_est) = estimate_tokens(prompt_seen, prompt_hint, sh.cfg.max_tokens);
    let est_tokens = prompt_est.saturating_add(completion_est);
    let est_usd = price.map(|p| p.cost(prompt_est, completion_est, None));

    // Held across check and reservation so parallel calls cannot all pass.
    let mut map = sh.spend.bot.reserved.lock();
    let mut usage = Vec::new();
    for p in Period::ALL {
        if *limits.period(p) == PeriodLimit::default() {
            continue;
        }
        let u = period_usage(&sh.storage, ctx.user_id, ctx.bot_id, p, now)
            .map_err(|_| unavailable("usage ledger unavailable"))?;
        usage.push((p, u));
    }
    let reserved = map.get(&ctx.bot_id).copied().unwrap_or_default();
    if let Some(b) = find_breach(&limits, &usage, reserved, est_usd, est_tokens) {
        drop(map);
        let _ = sh.audit_usage_record(
            ctx.user_id,
            ctx.run_id,
            "bot_limit.blocked",
            &json!({
                "bot_id": ctx.bot_id.to_string(), "period": b.period.as_str(),
                "metric": b.metric.as_str(), "limit": b.limit, "used": b.used,
                "model": model, "resets_ms": b.resets_ms,
            }),
        );
        return Err(breach_message(&b));
    }
    let entry = map.entry(ctx.bot_id).or_default();
    entry.usd += est_usd.unwrap_or(0.0);
    entry.tokens = entry.tokens.saturating_add(est_tokens);
    entry.calls += 1;
    drop(map);
    for a in crossed_alerts(&limits, &usage) {
        bot_alert(sh, ctx, &a);
    }
    Ok(Some((
        Reservation {
            bot: ctx.bot_id,
            usd: est_usd.unwrap_or(0.0),
            tokens: est_tokens,
        },
        est_usd.unwrap_or(0.0),
        est_tokens,
        prompt_est,
    )))
}

/// The stable, plain-language error of a broken hard limit.
fn breach_message(b: &crate::bot_limits::Breach) -> String {
    use crate::bot_limits::{BOT_LIMIT_REACHED, Metric};
    let (used, limit) = if b.metric == Metric::Usd {
        (format!("${:.2}", b.used), format!("${:.2}", b.limit))
    } else {
        (
            format!("{:.0} tokens", b.used),
            format!("{:.0} tokens", b.limit),
        )
    };
    format!(
        "[{BOT_LIMIT_REACHED} period={} metric={} resets_ms={}] This bot's {} limit of \
         {limit} is used up or would be exceeded by the next call ({used} used or in \
         flight). The model was not called or was stopped. Raise the limit in the bot's \
         settings, wait until it resets, or stop here.",
        b.period.as_str(),
        b.metric.as_str(),
        b.resets_ms,
        b.period.as_str(),
    )
}

/// Issue one bot-limit notification at most once per period, bot, measure,
/// level and limit value (raising the limit re-arms it). The in-memory set is
/// the fast path; the durable marker is the `bot_limit.alert` audit row, so a
/// restart does not repeat the notification.
fn bot_alert(sh: &Arc<Shared>, ctx: &RunCtx, a: &crate::bot_limits::Alert) {
    use crate::bot_limits::{BOT_LIMIT_WARNING, Metric};
    let key = format!(
        "{}|{}|{}|{}|{}|{}",
        ctx.bot_id,
        a.period.as_str(),
        a.metric.as_str(),
        a.level,
        a.base,
        a.start_ms
    );
    if !sh.spend.bot.alerted.lock().insert(key.clone()) {
        return;
    }
    let durable = sh
        .storage
        .list_audit_action_since(ctx.user_id, "bot_limit.alert", a.start_ms)
        .is_ok_and(|rows| rows.iter().any(|r| r.detail["key"].as_str() == Some(&key)));
    if durable {
        return;
    }
    let _ = sh.audit_usage_record(
        ctx.user_id,
        ctx.run_id,
        "bot_limit.alert",
        &json!({
            "key": key, "bot_id": ctx.bot_id.to_string(), "period": a.period.as_str(),
            "metric": a.metric.as_str(), "level": a.level, "limit": a.base,
        }),
    );
    let amount = if a.metric == Metric::Usd {
        format!("${:.2}", a.base)
    } else {
        format!("{:.0} tokens", a.base)
    };
    sh.emit(
        ctx,
        Event::Error(RunError {
            run_id: ctx.run_id,
            message: format!(
                "[{BOT_LIMIT_WARNING} period={} metric={} level={}] This bot used {}% of its {} \
                 limit of {amount}.",
                a.period.as_str(),
                a.metric.as_str(),
                a.level,
                a.level,
                a.period.as_str(),
            ),
            recoverable: true,
        }),
    );
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
    /// Per-bot limit estimate held while the call is in flight.
    bot_reservation: Option<crate::bot_limits::Reservation>,
    bot: protocol::BotId,
    /// Prompt tokens assumed at admission (0 when no limit applies).
    prompt_est: u64,
    /// Streamed bytes at which the hard limits are checked next.
    next_check_bytes: usize,
}

/// Streamed text is re-checked against the hard limits every this many bytes.
const STREAM_CHECK_BYTES: usize = 1024;

impl CallGuard {
    pub(crate) fn start(
        sh: &Arc<Shared>,
        ctx: &RunCtx,
        model: &str,
        purpose: CallPurpose,
    ) -> Result<Self, String> {
        Self::start_with_prompt(sh, ctx, model, purpose, None)
    }

    /// Like [`Self::start`], with an estimate of the request's prompt tokens so
    /// a large first prompt is counted at admission, not only after usage.
    pub(crate) fn start_with_prompt(
        sh: &Arc<Shared>,
        ctx: &RunCtx,
        model: &str,
        purpose: CallPurpose,
        prompt_hint: Option<u64>,
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
        let mut detail = detail;
        let mut prompt_est = 0;
        let bot_reservation =
            check_bot_limits(sh, ctx, model, prompt_hint)?.map(|(r, usd, tokens, prompt)| {
                // What the call counts as when the provider reports no usage.
                detail["limit_estimate_usd"] = json!(usd);
                detail["limit_estimate_tokens"] = json!(tokens);
                prompt_est = prompt;
                r
            });
        let reserved_usd = match check_spend_limit(sh, ctx, model) {
            Ok(v) => v,
            Err(e) => {
                sh.spend.bot.release(bot_reservation);
                return Err(e);
            }
        };
        if sh
            .audit_usage_record(ctx.user_id, ctx.run_id, "llm.call_started", &detail)
            .is_err()
        {
            release_reservation(sh, reserved_usd);
            sh.spend.bot.release(bot_reservation);
            return Err("usage ledger unavailable".to_owned());
        }
        Ok(Self {
            sh: Arc::clone(sh),
            run: ctx.run_id,
            user: ctx.user_id,
            detail,
            started: Instant::now(),
            reserved_usd,
            bot_reservation,
            bot: ctx.bot_id,
            prompt_est,
            next_check_bytes: STREAM_CHECK_BYTES,
        })
    }

    /// Called while a reply streams in (`streamed_bytes` so far): stop the call
    /// when what it has produced, on top of the recorded and in-flight usage of
    /// the bot, would break a hard limit. The unreported call is then counted
    /// by this larger estimate instead of the admission one.
    ///
    /// # Errors
    /// The typed limit message; the caller must stop reading the stream.
    pub(crate) fn stream_progress(&mut self, streamed_bytes: usize) -> Result<(), String> {
        use crate::bot_limits::{
            BotReserved, Period, PeriodLimit, find_breach, load_limits, period_usage,
        };
        let Some(mut own) = self.bot_reservation else {
            return Ok(());
        };
        if streamed_bytes < self.next_check_bytes {
            return Ok(());
        }
        self.next_check_bytes = streamed_bytes + STREAM_CHECK_BYTES;
        let fail = |what: &str| {
            format!(
                "[{} period=unknown metric=unknown resets_ms=0] Bot spending limits could not \
                 be checked ({what}), so the reply was stopped.",
                crate::bot_limits::BOT_LIMIT_REACHED
            )
        };
        let sh = &self.sh;
        let limits = load_limits(&sh.storage, self.bot).map_err(|e| fail(&e))?;
        let now = storage::now_ms();
        // About 3 bytes per token: more tokens than most text, so we err towards stopping.
        let completion = (streamed_bytes as u64).div_ceil(3);
        let actual_tokens = self.prompt_est.saturating_add(completion);
        let actual_usd = self.detail["model"]
            .as_str()
            .and_then(|m| sh.price_for(m))
            .map(|p| p.cost(self.prompt_est, completion, None));
        let others = {
            let map = sh.spend.bot.reserved.lock();
            let all = map.get(&self.bot).copied().unwrap_or_default();
            BotReserved {
                usd: (all.usd - own.usd).max(0.0),
                tokens: all.tokens.saturating_sub(own.tokens),
                calls: all.calls.saturating_sub(1),
            }
        };
        let mut usage = Vec::new();
        for p in Period::ALL {
            if *limits.period(p) == PeriodLimit::default() {
                continue;
            }
            let u = period_usage(&sh.storage, self.user, self.bot, p, now)
                .map_err(|_| fail("usage ledger unavailable"))?;
            usage.push((p, u));
        }
        let breach = find_breach(&limits, &usage, others, actual_usd, actual_tokens);
        // The call outgrew its admission reservation: hold the growth in the
        // shared in-flight total at once, so other chats, subagents and
        // scheduled runs of this bot are admitted against the real size.
        let grow_tokens = actual_tokens.saturating_sub(own.tokens);
        let grow_usd = actual_usd.map_or(0.0, |u| (u - own.usd).max(0.0));
        if grow_tokens > 0 || grow_usd > 0.0 {
            if let Some(e) = sh.spend.bot.reserved.lock().get_mut(&self.bot) {
                e.tokens = e.tokens.saturating_add(grow_tokens);
                e.usd += grow_usd;
            }
            own.tokens = own.tokens.saturating_add(grow_tokens);
            own.usd += grow_usd;
            self.bot_reservation = Some(own);
        }
        if actual_tokens > self.detail["limit_estimate_tokens"].as_u64().unwrap_or(0) {
            self.detail["limit_estimate_tokens"] = json!(actual_tokens);
            if let Some(usd) = actual_usd {
                self.detail["limit_estimate_usd"] = json!(usd);
            }
        }
        let Some(b) = breach else { return Ok(()) };
        let _ = sh.audit_usage_record(
            self.user,
            self.run,
            "bot_limit.blocked",
            &json!({
                "bot_id": self.bot.to_string(), "period": b.period.as_str(),
                "metric": b.metric.as_str(), "limit": b.limit, "used": b.used,
                "model": self.detail["model"], "resets_ms": b.resets_ms, "streaming": true,
            }),
        );
        Err(breach_message(&b))
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
        self.sh.spend.bot.release(self.bot_reservation);
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

    #[test]
    fn calls_with_timestamps_keeps_outcome_ts_and_falls_back_to_intent_ts() {
        let run = RunId::new();
        let completed_id = Uuid::now_v7();
        let crashed_id = Uuid::now_v7();
        let intent = AuditEntry {
            id: 1,
            user_id: UserId::new(),
            run_id: Some(run),
            actor: "bot".into(),
            action: "llm.call_started".into(),
            ts_ms: 1_000,
            detail: json!({"ledger_version": 1, "call_id": completed_id,
                "model": "fake/m", "purpose": "executor", "step_n": 1, "tokens": null}),
        };
        let mut outcome = intent.clone();
        outcome.id = 2;
        outcome.action = "usage".into();
        outcome.ts_ms = 2_500;
        outcome.detail["tokens"] = json!(Usage {
            prompt_tokens: 7,
            ..Usage::default()
        });
        let mut crashed = intent.clone();
        crashed.id = 3;
        crashed.ts_ms = 3_000;
        crashed.detail["call_id"] = json!(crashed_id);

        let by_run = calls_with_timestamps_from_audit([&outcome, &intent, &crashed]);
        let calls = &by_run[&run];
        assert_eq!(calls.len(), 2);
        let done = calls
            .iter()
            .find(|c| c.call.call_id == completed_id)
            .unwrap();
        assert_eq!(done.ts_ms, 2_500);
        assert_eq!(done.call.tokens.unwrap().prompt_tokens, 7);
        let lost = calls.iter().find(|c| c.call.call_id == crashed_id).unwrap();
        assert_eq!(lost.ts_ms, 3_000);
        assert!(lost.call.tokens.is_none());
    }
}
