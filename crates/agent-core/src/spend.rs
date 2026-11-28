//! Monthly spending limit (per installation, default off).
//!
//! The limit is checked before every model call in `CallGuard::start` (`usage`),
//! the one place all paid calls pass through (foreground, scheduled, subagent,
//! memory and router calls alike). Month-to-date spend is the sum of the
//! `cost_usd` the usage ledger recorded for calls that finished in the current
//! local calendar month. Calls whose model has no known price record
//! `cost_usd: null`: they are not counted and never guessed, and a call to such a
//! model is not blocked (it cannot add a known cost).

use chrono::{DateTime, Datelike, Local, TimeZone};
use parking_lot::{Mutex, RwLock};
use protocol::{RunId, UserId};
use storage::Storage;

/// Stable code at the start of the run error message when the limit stops a call.
pub const SPEND_LIMIT_REACHED: &str = "spend_limit_reached";

/// Smallest limit the API accepts (USD).
pub const MIN_LIMIT_USD: f64 = 1.0;
/// Largest limit the API accepts (USD); keeps the value sane, not a policy.
pub const MAX_LIMIT_USD: f64 = 1_000_000.0;

/// Prompt size assumed for the first call of a run (no earlier call to learn from).
const DEFAULT_PROMPT_TOKENS: u64 = 4_000;
/// Completion cap assumed when the loop config sets no `max_tokens`.
const DEFAULT_COMPLETION_TOKENS: u64 = 4_096;

/// Smallest time into the month before a month-end projection is shown; the
/// first minutes of a month would extrapolate a few cents into a huge number.
pub const MIN_FORECAST_ELAPSED_MS: i64 = 24 * 60 * 60 * 1000;

/// Live limit, the in-memory "already warned this month" marker (the durable
/// marker is the `spend.limit_warning` audit row) and the estimates of calls
/// that started but have not ended yet.
#[derive(Default)]
pub(crate) struct SpendState {
    pub limit: RwLock<Option<f64>>,
    pub warned_month: Mutex<Option<i64>>,
    /// Sum of the estimates of in-flight calls (USD). Spend is only recorded
    /// when a call ends, so without this N parallel calls could all pass at 99%.
    /// Process-wide, so it may over-count across users: that only errs safe.
    pub reserved: Mutex<Reserved>,
    /// Per-bot limits state (see `bot_limits.rs`).
    pub bot: crate::bot_limits::BotLimitState,
}

/// In-flight reservations: the USD sum and how many calls hold one. The count
/// lets the sum snap back to exactly zero when the last call ends, so float
/// rounding cannot leave a tiny positive remainder behind.
#[derive(Default)]
pub(crate) struct Reserved {
    pub usd: f64,
    pub calls: u32,
}

impl Reserved {
    pub fn add(&mut self, amount: f64) {
        if amount > 0.0 {
            self.usd += amount;
            self.calls += 1;
        }
    }

    pub fn release(&mut self, amount: f64) {
        if amount > 0.0 {
            self.calls = self.calls.saturating_sub(1);
            self.usd = if self.calls == 0 {
                0.0
            } else {
                (self.usd - amount).max(0.0)
            };
        }
    }
}

/// Month-to-date accounting.
#[derive(Debug, Clone, PartialEq)]
pub struct SpendStatus {
    /// Start of the local calendar month, ms since epoch.
    pub month_start_ms: i64,
    /// Start of the next local month, ms since epoch.
    pub month_end_ms: i64,
    /// Sum of recorded `cost_usd` this month.
    pub spent_usd: f64,
    /// Finished calls this month with unknown price (not counted).
    pub unpriced_calls: u64,
}

/// `[start, end)` of the local calendar month containing `now_ms`.
#[must_use]
pub fn month_bounds_ms(now_ms: i64) -> (i64, i64) {
    let now: DateTime<Local> = Local
        .timestamp_millis_opt(now_ms)
        .single()
        .unwrap_or_else(Local::now);
    // Local midnight on the 1st may not exist (DST gap): fall back to the
    // first valid instant of that day instead of 0, which would count the
    // whole history.
    let first = |y: i32, m: u32| {
        (0..=3)
            .find_map(|h| Local.with_ymd_and_hms(y, m, 1, h, 0, 0).earliest())
            .map_or(0, |d| d.timestamp_millis())
    };
    let (y, m) = (now.year(), now.month());
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    (first(y, m), first(ny, nm))
}

/// Linear projection of month-end spend, `None` before
/// [`MIN_FORECAST_ELAPSED_MS`] of the month has elapsed or any spend was recorded.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn forecast_usd(status: &SpendStatus, now_ms: i64) -> Option<f64> {
    let elapsed = (now_ms - status.month_start_ms) as f64;
    let total = (status.month_end_ms - status.month_start_ms) as f64;
    (elapsed >= MIN_FORECAST_ELAPSED_MS as f64 && total > 0.0 && status.spent_usd > 0.0)
        .then(|| status.spent_usd / elapsed * total)
}

/// Month-to-date spend of `user` from the usage ledger.
///
/// # Errors
/// Fails on a database error.
pub fn spend_status(st: &Storage, user: UserId, now_ms: i64) -> storage::Result<SpendStatus> {
    let (month_start_ms, month_end_ms) = month_bounds_ms(now_ms);
    let (spent_usd, unpriced_calls) = st.usage_totals_since(user, month_start_ms)?;
    Ok(SpendStatus {
        month_start_ms,
        month_end_ms,
        spent_usd,
        unpriced_calls,
    })
}

/// Conservative cost of the next call: the largest prompt this run already
/// sent (or a default) plus the full completion cap, at the model's price.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub(crate) fn estimate_usd(
    st: &Storage,
    user: UserId,
    run: RunId,
    price: crate::ModelPrice,
    max_tokens: Option<u32>,
) -> f64 {
    let prompt = st
        .max_run_prompt_tokens(user, run)
        .ok()
        .flatten()
        .unwrap_or(DEFAULT_PROMPT_TOKENS);
    let completion = max_tokens.map_or(DEFAULT_COMPLETION_TOKENS, u64::from);
    price.cost(prompt, completion, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn month_bounds_contain_now_and_forecast_is_linear() {
        let now = Local::now().timestamp_millis();
        let (start, end) = month_bounds_ms(now);
        assert!(start <= now && now < end);
        let status = SpendStatus {
            month_start_ms: 0,
            month_end_ms: 1_000,
            spent_usd: 10.0,
            unpriced_calls: 0,
        };
        // Linear once past the minimum elapsed time; no forecast before.
        let day = MIN_FORECAST_ELAPSED_MS;
        let status = SpendStatus {
            month_end_ms: day * 4,
            ..status
        };
        let f = forecast_usd(&status, day).unwrap();
        assert!((f - 40.0).abs() < 1e-9, "{f}");
        assert!(forecast_usd(&status, day - 1).is_none());
        assert!(forecast_usd(&status, 0).is_none());
    }
}
