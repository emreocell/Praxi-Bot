//! Aggregated token/cost usage (`/v1/usage`) and CSV export (`/v1/usage/export`).
//!
//! Builds on the durable LLM call ledger (`agent_core::usage`) with legacy
//! model-step fallback for pre-ledger runs. Aggregates never contain prompt
//! text, response text, tool arguments, error text, or secrets.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;

use agent_core::ModelPrice;
use axum::Json;
use axum::extract::{Query, State};
use axum::http::header;
use axum::response::IntoResponse;
use chrono::{Duration, NaiveDate, TimeZone};
use chrono_tz::Tz;
use protocol::RunId;
use provider_catalog::Usage;
use serde::Deserialize;
use serde_json::Value;
use storage::{AuditEntry, Run, RunStep, now_ms};
use utoipa::IntoParams;
use uuid::Uuid;

use crate::AppState;
use crate::dto::{
    BotUsage, DailyUsage, ModelUsage, ProviderUsage, TopRunUsage, UsageSummary, UsageTotals,
};
use crate::error::{ApiError, ErrorBody};

/// Maximum number of local calendar days returned in the daily trend series.
pub const MAX_USAGE_DAYS: u32 = 366;

/// Query of `GET /v1/usage` and `GET /v1/usage/export`.
#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct UsageQuery {
    /// Only runs created at or after this time (Unix ms).
    pub since_ms: Option<i64>,
    /// Only runs created at or before this time (Unix ms).
    pub until_ms: Option<i64>,
    /// Rolling window of local calendar days (`1..=366`, ending today in `tz`).
    pub days: Option<u32>,
    /// IANA time zone name used for daily bucketing (defaults to local time zone).
    pub tz: Option<String>,
}

#[derive(Default, Clone)]
struct Bucket {
    totals: UsageTotals,
    calls_without_usage: u64,
    calls_without_cost: u64,
}

impl Bucket {
    fn record(&mut self, tokens: Option<Usage>, cost: Option<f64>) {
        add_totals(&mut self.totals, tokens, cost);
        if tokens.is_none() {
            self.calls_without_usage = self.calls_without_usage.saturating_add(1);
        }
        if cost.is_none() {
            self.calls_without_cost = self.calls_without_cost.saturating_add(1);
        }
    }
}

fn add_totals(t: &mut UsageTotals, tokens: Option<Usage>, cost: Option<f64>) {
    if let Some(c) = cost {
        *t.cost_usd.get_or_insert(0.0) += c;
    }
    if let Some(u) = tokens {
        t.prompt_tokens = t.prompt_tokens.saturating_add(u.prompt_tokens);
        t.completion_tokens = t.completion_tokens.saturating_add(u.completion_tokens);
        t.cached_tokens = t.cached_tokens.saturating_add(u.cached_tokens.unwrap_or(0));
    }
    t.model_calls = t.model_calls.saturating_add(1);
}

/// Resolve an optional IANA time zone name, defaulting to the local system zone
/// (falling back to UTC when the OS zone cannot be determined).
///
/// # Errors
/// Returns `400 bad_request` when a non-empty `tz` string is not a valid IANA zone.
pub(crate) fn resolve_tz(tz: Option<&str>) -> Result<Tz, ApiError> {
    match tz.map(str::trim).filter(|s| !s.is_empty()) {
        Some(name) => name
            .parse::<Tz>()
            .map_err(|_| ApiError::bad_request(format!("unknown time zone: {name}"))),
        None => Ok(iana_time_zone::get_timezone()
            .ok()
            .and_then(|s| s.parse::<Tz>().ok())
            .unwrap_or(chrono_tz::UTC)),
    }
}

/// Earliest valid local instant of `date` in `tz` (Unix ms), safely stepping
/// past DST spring-forward midnight gaps.
#[must_use]
pub(crate) fn local_midnight_ms(date: NaiveDate, tz: Tz) -> i64 {
    (0..=3)
        .find_map(|h| {
            date.and_hms_opt(h, 0, 0)
                .and_then(|dt| tz.from_local_datetime(&dt).earliest())
        })
        .map_or(0, |dt| dt.timestamp_millis())
}

/// Local calendar date of `ts_ms` in `tz`.
#[must_use]
pub(crate) fn local_day(ts_ms: i64, tz: Tz) -> NaiveDate {
    tz.timestamp_millis_opt(ts_ms)
        .single()
        .or_else(|| tz.timestamp_millis_opt(0).single())
        .map_or(NaiveDate::MIN, |dt| dt.date_naive())
}

/// Provider prefix of a model reference (`openai/gpt-4o` -> `Some("openai")`,
/// `cli:claude/sonnet` -> `Some("cli:claude")`, `cli:claude` -> `Some("cli:claude")`).
#[must_use]
pub(crate) fn provider_of(model: &str) -> Option<String> {
    if let Some((provider, _)) = model.split_once('/') {
        let p = provider.trim();
        (!p.is_empty()).then(|| p.to_owned())
    } else if model.starts_with("cli:") {
        Some(model.to_owned())
    } else {
        None
    }
}

fn normalize_model(raw: &str, redact: &dyn Fn(&str) -> String) -> String {
    let cleaned = redact(raw);
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "unknown".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Formula-injection-safe RFC-4180 CSV cell encoding.
///
/// Cells that begin (optionally after leading spaces/tabs) with `=`, `+`, `-`,
/// `@`, `\t`, or `\r` are prefixed with `'` so spreadsheet applications treat
/// them strictly as literal text.
#[must_use]
pub fn csv_escape(cell: &str) -> String {
    let trimmed = cell.trim_start_matches([' ', '\t']);
    let needs_prefix = cell.starts_with(['=', '+', '-', '@', '\t', '\r'])
        || trimmed.starts_with(['=', '+', '-', '@']);
    let safe = if needs_prefix {
        format!("'{cell}")
    } else {
        cell.to_owned()
    };
    if safe.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", safe.replace('"', "\"\""))
    } else {
        safe
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct CsvKey {
    day: String,
    bot_label: String,
    bot_id: Uuid,
    model: String,
    purpose: String,
}

struct Aggregated {
    summary: UsageSummary,
    csv_rows: BTreeMap<CsvKey, Bucket>,
}

struct AggregateCtx<'a> {
    runs: &'a [Run],
    audit: &'a [AuditEntry],
    bots: &'a HashMap<Uuid, String>,
    steps_for: &'a dyn Fn(RunId) -> storage::Result<Vec<RunStep>>,
    price_for: &'a dyn Fn(&str) -> Option<ModelPrice>,
    redact: &'a dyn Fn(&str) -> String,
    now_ms: i64,
}

#[derive(Default)]
struct Accumulator {
    runs: u64,
    total: Bucket,
    by_bot: BTreeMap<Uuid, UsageTotals>,
    by_purpose: BTreeMap<String, UsageTotals>,
    by_model: BTreeMap<String, Bucket>,
    by_provider: BTreeMap<String, Bucket>,
    by_day: BTreeMap<NaiveDate, Bucket>,
    by_run: BTreeMap<Uuid, (Bucket, TopRunUsage)>,
    csv_rows: BTreeMap<CsvKey, Bucket>,
}

/// Number of runs in the "most expensive runs" list.
const TOP_RUNS: usize = 5;

impl Accumulator {
    #[allow(clippy::too_many_arguments)]
    fn record(
        &mut self,
        ctx: &AggregateCtx<'_>,
        tz: Tz,
        run: &Run,
        bot_label: &str,
        raw_model: &str,
        purpose: &str,
        tokens: Option<Usage>,
        ts_ms: i64,
    ) {
        let bot_id = run.bot_id.0;
        let model = normalize_model(raw_model, ctx.redact);
        let cost = tokens.and_then(|u| {
            (ctx.price_for)(raw_model)
                .map(|p| p.cost(u.prompt_tokens, u.completion_tokens, u.cached_tokens))
        });
        let day = local_day(ts_ms, tz);
        let day_str = day.format("%Y-%m-%d").to_string();

        self.total.record(tokens, cost);
        add_totals(self.by_bot.entry(bot_id).or_default(), tokens, cost);
        add_totals(
            self.by_purpose.entry(purpose.to_owned()).or_default(),
            tokens,
            cost,
        );
        self.by_model
            .entry(model.clone())
            .or_default()
            .record(tokens, cost);
        let provider = provider_of(&model).unwrap_or_else(|| "unknown".to_owned());
        self.by_provider
            .entry(provider)
            .or_default()
            .record(tokens, cost);
        self.by_day.entry(day).or_default().record(tokens, cost);
        let entry = self.by_run.entry(run.id.0).or_insert_with(|| {
            (
                Bucket::default(),
                TopRunUsage {
                    run_id: run.id.0,
                    bot_id,
                    chat_id: run.chat_id.map(|c| c.0),
                    created_at_ms: run.created_at_ms,
                    totals: UsageTotals::default(),
                    calls_without_usage: 0,
                    calls_without_cost: 0,
                },
            )
        });
        entry.0.record(tokens, cost);
        self.csv_rows
            .entry(CsvKey {
                day: day_str,
                bot_label: bot_label.to_owned(),
                bot_id,
                model,
                purpose: purpose.to_owned(),
            })
            .or_default()
            .record(tokens, cost);
    }

    fn fold_run(
        &mut self,
        ctx: &AggregateCtx<'_>,
        tz: Tz,
        run: &Run,
        ledger: &BTreeMap<RunId, Vec<agent_core::usage::RecordedCall>>,
    ) -> Result<(), ApiError> {
        self.runs = self.runs.saturating_add(1);
        let bot_id = run.bot_id.0;
        let bot_label = ctx
            .bots
            .get(&bot_id)
            .map_or_else(|| bot_id.to_string(), |name| (ctx.redact)(name));

        let mut recorded_steps = HashSet::new();
        if let Some(calls) = ledger.get(&run.id) {
            for rec in calls {
                if matches!(
                    rec.call.purpose,
                    agent_core::CallPurpose::Executor
                        | agent_core::CallPurpose::GuardClose
                        | agent_core::CallPurpose::Subagent
                ) {
                    recorded_steps.insert(rec.call.step_n);
                }
                let ts_ms = if rec.ts_ms > 0 {
                    rec.ts_ms
                } else {
                    run.created_at_ms
                };
                self.record(
                    ctx,
                    tz,
                    run,
                    &bot_label,
                    &rec.call.model,
                    rec.call.purpose.as_str(),
                    rec.call.tokens,
                    ts_ms,
                );
            }
        }
        for step in (ctx.steps_for)(run.id)? {
            if recorded_steps.contains(&step.idx) {
                continue;
            }
            let Some(u) = (step.name == "model")
                .then_some(step.output.as_ref())
                .flatten()
                .and_then(|o| o.get("usage"))
                .and_then(|u| serde_json::from_value::<Usage>(u.clone()).ok())
            else {
                continue;
            };
            let raw_model = step
                .output
                .as_ref()
                .and_then(|o| o.get("model"))
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let purpose = if run.parent_run_id.is_some() {
                "subagent"
            } else if step
                .output
                .as_ref()
                .is_some_and(|o| o["stopped_by_guard"] == true)
            {
                "guard_close"
            } else {
                "executor"
            };
            let fallback_ts = if step.started_at_ms > 0 {
                step.started_at_ms
            } else {
                run.created_at_ms
            };
            let ts_ms = step
                .finished_at_ms
                .filter(|&t| t > 0)
                .unwrap_or(fallback_ts);
            self.record(ctx, tz, run, &bot_label, raw_model, purpose, Some(u), ts_ms);
        }
        Ok(())
    }
}

fn aggregate(ctx: &AggregateCtx<'_>, q: &UsageQuery) -> Result<Aggregated, ApiError> {
    if let Some(d) = q.days
        && (d == 0 || d > MAX_USAGE_DAYS)
    {
        return Err(ApiError::bad_request(format!(
            "days must be between 1 and {MAX_USAGE_DAYS}"
        )));
    }
    let tz = resolve_tz(q.tz.as_deref())?;
    let ref_ms = q.until_ms.unwrap_or(ctx.now_ms);
    let today = local_day(ref_ms, tz);
    let effective_since_ms = match q.days {
        Some(d) => {
            let start_date = today - Duration::days(i64::from(d - 1));
            let day_start = local_midnight_ms(start_date, tz);
            Some(q.since_ms.map_or(day_start, |s| s.max(day_start)))
        }
        None => q.since_ms,
    };

    let ledger = agent_core::usage::calls_with_timestamps_from_audit(ctx.audit);
    let mut acc = Accumulator::default();
    for run in ctx.runs {
        if effective_since_ms.is_some_and(|t| run.created_at_ms < t)
            || q.until_ms.is_some_and(|t| run.created_at_ms > t)
        {
            continue;
        }
        acc.fold_run(ctx, tz, run, &ledger)?;
    }

    let daily = build_daily_series(&acc.by_day, q, today, tz);
    let top_runs = top_runs_of(acc.by_run);
    let summary = UsageSummary {
        runs: acc.runs,
        total: acc.total.totals,
        by_bot: acc
            .by_bot
            .into_iter()
            .map(|(bot_id, totals)| BotUsage { bot_id, totals })
            .collect(),
        by_purpose: acc.by_purpose,
        by_model: acc
            .by_model
            .into_iter()
            .map(|(model, b)| ModelUsage {
                provider: provider_of(&model),
                model,
                totals: b.totals,
                calls_without_usage: b.calls_without_usage,
                calls_without_cost: b.calls_without_cost,
            })
            .collect(),
        by_provider: acc
            .by_provider
            .into_iter()
            .map(|(provider, b)| ProviderUsage {
                provider,
                totals: b.totals,
                calls_without_usage: b.calls_without_usage,
                calls_without_cost: b.calls_without_cost,
            })
            .collect(),
        daily,
        top_runs,
        calls_without_usage: acc.total.calls_without_usage,
        calls_without_cost: acc.total.calls_without_cost,
        time_zone: tz.name().to_owned(),
    };

    Ok(Aggregated {
        summary,
        csv_rows: acc.csv_rows,
    })
}

/// The most expensive runs: known cost first (highest first), then runs whose
/// price is unknown ranked by tokens; ties keep the newest run first.
fn top_runs_of(by_run: BTreeMap<Uuid, (Bucket, TopRunUsage)>) -> Vec<TopRunUsage> {
    let mut runs: Vec<TopRunUsage> = by_run
        .into_values()
        .map(|(b, mut r)| {
            r.totals = b.totals;
            r.calls_without_usage = b.calls_without_usage;
            r.calls_without_cost = b.calls_without_cost;
            r
        })
        .collect();
    let tokens = |r: &TopRunUsage| r.totals.prompt_tokens + r.totals.completion_tokens;
    runs.sort_by(|a, b| {
        let (ca, cb) = (a.totals.cost_usd, b.totals.cost_usd);
        cb.is_some()
            .cmp(&ca.is_some())
            .then_with(|| {
                cb.unwrap_or(0.0)
                    .partial_cmp(&ca.unwrap_or(0.0))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| tokens(b).cmp(&tokens(a)))
            .then_with(|| b.created_at_ms.cmp(&a.created_at_ms))
    });
    runs.retain(|r| r.totals.model_calls > 0);
    runs.truncate(TOP_RUNS);
    runs
}

fn build_daily_series(
    by_day: &BTreeMap<NaiveDate, Bucket>,
    q: &UsageQuery,
    today: NaiveDate,
    tz: Tz,
) -> Vec<DailyUsage> {
    let span = Duration::days(i64::from(MAX_USAGE_DAYS - 1));
    let bounds: Option<(NaiveDate, NaiveDate)> = if let Some(d) = q.days {
        let start = today - Duration::days(i64::from(d - 1));
        Some((start, today))
    } else if let Some(since) = q.since_ms {
        let end = by_day
            .keys()
            .next_back()
            .copied()
            .map_or(today, |last| last.max(today));
        let start = local_day(since, tz).max(end - span).min(end);
        Some((start, end))
    } else if let (Some(&first), Some(&last)) = (by_day.keys().next(), by_day.keys().next_back()) {
        let end = last.max(today);
        let start = first.max(end - span);
        Some((start, end))
    } else {
        None
    };

    let Some((start, end)) = bounds else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut cur = start;
    while cur <= end {
        let bucket = by_day.get(&cur).cloned().unwrap_or_default();
        out.push(DailyUsage {
            day: cur.format("%Y-%m-%d").to_string(),
            totals: bucket.totals,
            calls_without_usage: bucket.calls_without_usage,
            calls_without_cost: bucket.calls_without_cost,
        });
        let Some(next) = cur.succ_opt() else {
            break;
        };
        cur = next;
    }
    out
}

fn render_csv(rows: &BTreeMap<CsvKey, Bucket>) -> String {
    let mut out = String::from(
        "day,bot,model,purpose,calls,calls_without_usage,prompt_tokens,completion_tokens,cached_tokens,cost_usd\n",
    );
    for (key, b) in rows {
        let all_unknown = b.calls_without_usage == b.totals.model_calls;
        let prompt = if all_unknown {
            String::new()
        } else {
            b.totals.prompt_tokens.to_string()
        };
        let completion = if all_unknown {
            String::new()
        } else {
            b.totals.completion_tokens.to_string()
        };
        let cached = if all_unknown {
            String::new()
        } else {
            b.totals.cached_tokens.to_string()
        };
        let cost = b
            .totals
            .cost_usd
            .map_or_else(String::new, |c| format!("{c:.6}"));
        let _ = writeln!(
            out,
            "{},{},{},{},{},{},{prompt},{completion},{cached},{cost}",
            csv_escape(&key.day),
            csv_escape(&key.bot_label),
            csv_escape(&key.model),
            csv_escape(&key.purpose),
            b.totals.model_calls,
            b.calls_without_usage,
        );
    }
    out
}

fn compute_for_state(s: &AppState, q: &UsageQuery) -> Result<Aggregated, ApiError> {
    let st = s.storage();
    let user = s.user_id();
    let runs = st.list_runs(user)?;
    let audit = st.list_audit(user)?;
    let bots: HashMap<Uuid, String> = st
        .list_bots(user)?
        .into_iter()
        .map(|b| (b.id.0, b.name))
        .collect();
    let redactor = s.doctor_host().and_then(|h| h.redactor());
    let redact = |text: &str| match &redactor {
        Some(r) => r.redact(text),
        None => text.to_owned(),
    };
    let steps_for = |run_id: RunId| st.list_steps(run_id);
    let price_for = |model: &str| s.agent().price_for(model);
    aggregate(
        &AggregateCtx {
            runs: &runs,
            audit: &audit,
            bots: &bots,
            steps_for: &steps_for,
            price_for: &price_for,
            redact: &redact,
            now_ms: now_ms(),
        },
        q,
    )
}

/// Invocation usage by bot, model, provider, purpose and local calendar day,
/// with legacy model-step fallback for pre-ledger runs.
#[utoipa::path(get, path = "/v1/usage", tag = "usage", security(("bearer" = [])),
    params(UsageQuery),
    responses((status = 200, body = UsageSummary), (status = 400, body = ErrorBody)))]
pub(crate) async fn usage(
    State(s): State<AppState>,
    Query(q): Query<UsageQuery>,
) -> Result<Json<UsageSummary>, ApiError> {
    Ok(Json(compute_for_state(&s, &q)?.summary))
}

/// Formula-injection-safe CSV export of usage over the selected range
/// (owner-only). Columns: `day,bot,model,purpose,calls,calls_without_usage,
/// prompt_tokens,completion_tokens,cached_tokens,cost_usd`.
#[utoipa::path(get, path = "/v1/usage/export", tag = "usage", security(("bearer" = [])),
    params(UsageQuery),
    responses(
        (status = 200, description = "Usage breakdown as CSV", content_type = "text/csv", body = String),
        (status = 400, body = ErrorBody),
        (status = 403, body = ErrorBody)
    ))]
pub(crate) async fn export_csv(
    State(s): State<AppState>,
    Query(q): Query<UsageQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let csv = render_csv(&compute_for_state(&s, &q)?.csv_rows);
    Ok((
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"praxi-usage.csv\"",
            ),
            (header::CACHE_CONTROL, "no-store"),
        ],
        csv,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use protocol::{BotId, UserId};
    use serde_json::json;

    fn ms(year: i32, month: u32, day: u32, hour: u32, min: u32, sec: u32) -> i64 {
        Utc.with_ymd_and_hms(year, month, day, hour, min, sec)
            .single()
            .unwrap()
            .timestamp_millis()
    }

    fn make_run(
        id: RunId,
        user: UserId,
        bot: BotId,
        created_at_ms: i64,
        parent: Option<RunId>,
    ) -> Run {
        Run {
            id,
            user_id: user,
            bot_id: bot,
            chat_id: None,
            kind: "turn".into(),
            state: "succeeded".into(),
            error: None,
            created_at_ms,
            updated_at_ms: created_at_ms,
            parent_run_id: parent,
            spec: None,
            computer_target: None,
            computer_node: None,
            computer_node_name: None,
            paused_at_ms: None,
            pause_reason: None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn ledger_entry(
        id: i64,
        user: UserId,
        run: RunId,
        action: &str,
        ts_ms: i64,
        call_id: Uuid,
        purpose: &str,
        model: &str,
        tokens: Option<Usage>,
    ) -> AuditEntry {
        AuditEntry {
            id,
            user_id: user,
            run_id: Some(run),
            actor: "bot".into(),
            action: action.into(),
            ts_ms,
            detail: json!({
                "ledger_version": 1,
                "call_id": call_id,
                "purpose": purpose,
                "model": model,
                "step_n": 1,
                "tokens": tokens,
                "cost_usd": null,
            }),
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn aggregates_multiple_models_unknown_usage_and_subagents_once() {
        let user = UserId::new();
        let bot = BotId::new();
        let parent_run = RunId::new();
        let child_run = RunId::new();
        let t1 = ms(2026, 11, 20, 10, 0, 0);
        let t2 = ms(2026, 11, 20, 10, 5, 0);

        let runs = vec![
            make_run(parent_run, user, bot, t1, None),
            make_run(child_run, user, bot, t2, Some(parent_run)),
        ];

        let c_exec = Uuid::now_v7();
        let c_router = Uuid::now_v7();
        let c_sub = Uuid::now_v7();
        let c_unknown = Uuid::now_v7();

        let audit = vec![
            // Executor call on priced openai/gpt-4o
            ledger_entry(
                1,
                user,
                parent_run,
                "usage",
                t1,
                c_exec,
                "executor",
                "openai/gpt-4o",
                Some(Usage {
                    prompt_tokens: 1_000,
                    completion_tokens: 200,
                    cached_tokens: Some(100),
                    ..Usage::default()
                }),
            ),
            // Router call on unpriced ollama/llama3 (tokens known, cost unknown)
            ledger_entry(
                2,
                user,
                parent_run,
                "usage",
                t1,
                c_router,
                "router",
                "ollama/llama3",
                Some(Usage {
                    prompt_tokens: 300,
                    completion_tokens: 50,
                    ..Usage::default()
                }),
            ),
            // Crashed/unknown-token call on openai/gpt-4o (intent only, tokens: None)
            ledger_entry(
                3,
                user,
                parent_run,
                "llm.call_started",
                t1,
                c_unknown,
                "executor",
                "openai/gpt-4o",
                None,
            ),
            // Child subagent call on priced anthropic/claude-sonnet
            ledger_entry(
                4,
                user,
                child_run,
                "usage",
                t2,
                c_sub,
                "subagent",
                "anthropic/claude-sonnet",
                Some(Usage {
                    prompt_tokens: 500,
                    completion_tokens: 100,
                    ..Usage::default()
                }),
            ),
            // Parent summary row must NOT cause child calls to be counted twice.
            AuditEntry {
                id: 5,
                user_id: user,
                run_id: Some(parent_run),
                actor: "bot".into(),
                action: "run.usage_summary".into(),
                ts_ms: t2 + 1000,
                detail: json!({"subagent": {"calls": 1, "prompt_tokens": 500, "completion_tokens": 100}}),
            },
        ];

        let price = |m: &str| match m {
            "openai/gpt-4o" => Some(ModelPrice {
                input_per_mtok: 2.0,
                output_per_mtok: 10.0,
                cached_per_mtok: None,
            }),
            "anthropic/claude-sonnet" => Some(ModelPrice {
                input_per_mtok: 3.0,
                output_per_mtok: 15.0,
                cached_per_mtok: None,
            }),
            _ => None,
        };
        let bots = HashMap::from([(bot.0, "Atlas".into())]);
        let no_steps = |_: RunId| Ok(Vec::new());
        let no_redact = |s: &str| s.to_owned();

        let agg = aggregate(
            &AggregateCtx {
                runs: &runs,
                audit: &audit,
                bots: &bots,
                steps_for: &no_steps,
                price_for: &price,
                redact: &no_redact,
                now_ms: ms(2026, 11, 20, 12, 0, 0),
            },
            &UsageQuery {
                days: Some(7),
                tz: Some("UTC".into()),
                ..UsageQuery::default()
            },
        )
        .unwrap();

        let s = &agg.summary;
        assert_eq!(s.runs, 2);
        assert_eq!(s.total.model_calls, 4);
        assert_eq!(s.total.prompt_tokens, 1_800);
        assert_eq!(s.total.completion_tokens, 350);
        assert_eq!(s.total.cached_tokens, 100);
        assert_eq!(s.calls_without_usage, 1);
        assert_eq!(s.calls_without_cost, 2);

        // Per-model totals sum to overall totals.
        assert_eq!(
            s.by_model
                .iter()
                .map(|m| m.totals.prompt_tokens)
                .sum::<u64>(),
            s.total.prompt_tokens
        );
        assert_eq!(
            s.by_model
                .iter()
                .map(|m| m.totals.completion_tokens)
                .sum::<u64>(),
            s.total.completion_tokens
        );
        assert_eq!(
            s.by_model.iter().map(|m| m.totals.model_calls).sum::<u64>(),
            s.total.model_calls
        );
        assert_eq!(
            s.by_model
                .iter()
                .map(|m| m.calls_without_usage)
                .sum::<u64>(),
            s.calls_without_usage
        );

        let ollama = s
            .by_model
            .iter()
            .find(|m| m.model == "ollama/llama3")
            .unwrap();
        assert_eq!(ollama.provider.as_deref(), Some("ollama"));
        assert!(ollama.totals.cost_usd.is_none());
        assert_eq!(ollama.calls_without_usage, 0);
        assert_eq!(ollama.calls_without_cost, 1);

        let gpt = s
            .by_model
            .iter()
            .find(|m| m.model == "openai/gpt-4o")
            .unwrap();
        assert_eq!(gpt.totals.model_calls, 2);
        assert_eq!(gpt.calls_without_usage, 1);
        assert_eq!(gpt.calls_without_cost, 1);
        assert!(gpt.totals.cost_usd.is_some());

        // Top runs: the priced parent run (gpt-4o) outranks the cheaper child run.
        assert_eq!(s.top_runs.len(), 2);
        assert_eq!(s.top_runs[0].run_id, parent_run.0);
        assert!(s.top_runs[0].totals.cost_usd.is_some());
        assert_eq!(s.top_runs[1].run_id, child_run.0);

        // Subagent counted once, never doubled by run.usage_summary.
        assert_eq!(s.by_purpose["subagent"].model_calls, 1);
        assert_eq!(s.by_purpose["subagent"].prompt_tokens, 500);

        // 7-day rolling window ends on 2026-11-20.
        assert_eq!(s.daily.len(), 7);
        assert_eq!(s.daily[0].day, "2026-11-14");
        assert_eq!(s.daily[6].day, "2026-11-20");
        assert_eq!(s.daily[6].totals.model_calls, 4);
        assert_eq!(s.daily[6].calls_without_usage, 1);
    }

    #[test]
    fn timezone_edges_bucket_calls_by_local_midnight() {
        let user = UserId::new();
        let bot = BotId::new();
        let run = RunId::new();
        // 2026-11-20 22:30 UTC is:
        // - 2026-11-21 01:30 in Europe/Istanbul (UTC+3)
        // - 2026-11-20 14:30 in America/Los_Angeles (UTC-8)
        let ts = ms(2026, 11, 20, 22, 30, 0);
        let runs = vec![make_run(run, user, bot, ts, None)];
        let audit = vec![ledger_entry(
            1,
            user,
            run,
            "usage",
            ts,
            Uuid::now_v7(),
            "executor",
            "openai/gpt-4o",
            Some(Usage {
                prompt_tokens: 100,
                completion_tokens: 20,
                ..Usage::default()
            }),
        )];
        let bots = HashMap::from([(bot.0, "Atlas".into())]);
        let no_steps = |_: RunId| Ok(Vec::new());
        let no_price = |_: &str| None;
        let no_redact = |s: &str| s.to_owned();
        let ctx = AggregateCtx {
            runs: &runs,
            audit: &audit,
            bots: &bots,
            steps_for: &no_steps,
            price_for: &no_price,
            redact: &no_redact,
            now_ms: ms(2026, 11, 21, 0, 0, 0),
        };

        let ist = aggregate(
            &ctx,
            &UsageQuery {
                days: Some(3),
                tz: Some("Europe/Istanbul".into()),
                ..UsageQuery::default()
            },
        )
        .unwrap();
        assert_eq!(ist.summary.time_zone, "Europe/Istanbul");
        assert_eq!(
            ist.summary
                .daily
                .iter()
                .map(|d| (d.day.as_str(), d.totals.model_calls))
                .collect::<Vec<_>>(),
            vec![("2026-11-19", 0), ("2026-11-20", 0), ("2026-11-21", 1)]
        );

        let la = aggregate(
            &ctx,
            &UsageQuery {
                days: Some(3),
                tz: Some("America/Los_Angeles".into()),
                ..UsageQuery::default()
            },
        )
        .unwrap();
        // At 2026-11-21 00:00 UTC it is still 2026-11-20 16:00 in Los Angeles.
        assert_eq!(
            la.summary
                .daily
                .iter()
                .map(|d| (d.day.as_str(), d.totals.model_calls))
                .collect::<Vec<_>>(),
            vec![("2026-11-18", 0), ("2026-11-19", 0), ("2026-11-20", 1)]
        );
    }

    #[test]
    fn golden_csv_is_deterministic_and_formula_injection_safe() {
        let user = UserId::new();
        let bot_danger = BotId::new();
        let bot_normal = BotId::new();
        let r1 = RunId::new();
        let r2 = RunId::new();
        let t1 = ms(2026, 11, 19, 9, 0, 0);
        let t2 = ms(2026, 11, 20, 15, 0, 0);

        let runs = vec![
            make_run(r1, user, bot_danger, t1, None),
            make_run(r2, user, bot_normal, t2, None),
        ];
        let audit = vec![
            ledger_entry(
                1,
                user,
                r1,
                "usage",
                t1,
                Uuid::now_v7(),
                "executor",
                "openai/gpt-4o",
                Some(Usage {
                    prompt_tokens: 1_000,
                    completion_tokens: 500,
                    cached_tokens: Some(200),
                    ..Usage::default()
                }),
            ),
            // Unknown usage call on a model whose name starts with '+'
            ledger_entry(
                2,
                user,
                r2,
                "llm.call_started",
                t2,
                Uuid::now_v7(),
                "embedding",
                "+evil/embed",
                None,
            ),
            // Unpriced call with known tokens on a model starting with '@', bot with comma/quotes
            ledger_entry(
                3,
                user,
                r2,
                "usage",
                t2,
                Uuid::now_v7(),
                "router",
                "@at-model",
                Some(Usage {
                    prompt_tokens: 40,
                    completion_tokens: 10,
                    ..Usage::default()
                }),
            ),
        ];
        let bots = HashMap::from([
            (bot_danger.0, "=SUM(A1:A2)".into()),
            (bot_normal.0, "Nova, \"Pro\"".into()),
        ]);
        let price = |m: &str| {
            (m == "openai/gpt-4o").then_some(ModelPrice {
                input_per_mtok: 2.0,
                output_per_mtok: 10.0,
                cached_per_mtok: None,
            })
        };
        let no_steps = |_: RunId| Ok(Vec::new());
        let no_redact = |s: &str| s.to_owned();

        let agg = aggregate(
            &AggregateCtx {
                runs: &runs,
                audit: &audit,
                bots: &bots,
                steps_for: &no_steps,
                price_for: &price,
                redact: &no_redact,
                now_ms: ms(2026, 11, 20, 18, 0, 0),
            },
            &UsageQuery {
                tz: Some("UTC".into()),
                ..UsageQuery::default()
            },
        )
        .unwrap();

        let csv = render_csv(&agg.csv_rows);
        let expected = concat!(
            "day,bot,model,purpose,calls,calls_without_usage,prompt_tokens,completion_tokens,cached_tokens,cost_usd\n",
            "2026-11-19,'=SUM(A1:A2),openai/gpt-4o,executor,1,0,1000,500,200,0.007000\n",
            "2026-11-20,\"Nova, \"\"Pro\"\"\",'+evil/embed,embedding,1,1,,,,\n",
            "2026-11-20,\"Nova, \"\"Pro\"\"\",'@at-model,router,1,0,40,10,0,\n",
        );
        assert_eq!(csv, expected);
        assert_eq!(csv_escape("-2+3"), "'-2+3");
        assert_eq!(csv_escape(" =1+1"), "' =1+1");
        assert_eq!(csv_escape("=1,2"), "\"'=1,2\"");
    }

    #[test]
    fn rejects_invalid_days_and_time_zone() {
        let no_steps = |_: RunId| Ok(Vec::new());
        let no_price = |_: &str| None;
        let no_redact = |s: &str| s.to_owned();
        let bots = HashMap::new();
        let ctx = AggregateCtx {
            runs: &[],
            audit: &[],
            bots: &bots,
            steps_for: &no_steps,
            price_for: &no_price,
            redact: &no_redact,
            now_ms: ms(2026, 11, 20, 12, 0, 0),
        };
        assert!(
            aggregate(
                &ctx,
                &UsageQuery {
                    days: Some(0),
                    ..UsageQuery::default()
                }
            )
            .is_err()
        );
        assert!(
            aggregate(
                &ctx,
                &UsageQuery {
                    days: Some(MAX_USAGE_DAYS + 1),
                    ..UsageQuery::default()
                }
            )
            .is_err()
        );
        assert!(
            aggregate(
                &ctx,
                &UsageQuery {
                    tz: Some("Mars/Olympus".into()),
                    ..UsageQuery::default()
                }
            )
            .is_err()
        );
    }
}
