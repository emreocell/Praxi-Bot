//! Per-bot spending limits (ADR-0034): daily / weekly / monthly, soft and hard,
//! in money and/or tokens.
//!
//! Enforced in [`crate::usage::CallGuard::start`], below every caller (chat,
//! scheduled and channel-triggered runs, subagents, memory and router calls),
//! so no API client can skip it. The counters are the usage ledger itself
//! (`audit_log` rows of the run's bot), which is durable; limits live in the
//! `bot_spend_limits` table and are changed only through owner-only routes.
//!
//! Counting: recorded `cost_usd` counts as money; a model without a known price
//! counts tokens only; a call whose provider reported no usage counts the
//! conservative estimate taken at call start, so missing data never means free.

use std::collections::HashMap;

use chrono::{DateTime, Datelike, Duration, Local, TimeZone};
use protocol::{BotId, UserId};
use serde::{Deserialize, Serialize};
use storage::Storage;

/// Stable code at the start of the run error message when a hard limit stops a call.
pub const BOT_LIMIT_REACHED: &str = "bot_limit_reached";
/// Stable code of the soft-limit / 80% notification message.
pub const BOT_LIMIT_WARNING: &str = "bot_limit_warning";

/// Money limits must be within these bounds (USD).
pub const MIN_USD: f64 = 0.01;
/// See [`MIN_USD`].
pub const MAX_USD: f64 = 1_000_000.0;
/// Token limits must be within `1..=MAX_TOKENS`.
pub const MAX_TOKENS: u64 = 1_000_000_000_000;

/// Prompt size assumed for the first call of a run.
const DEFAULT_PROMPT_TOKENS: u64 = 4_000;
/// Completion cap assumed when the loop config sets no `max_tokens`.
const DEFAULT_COMPLETION_TOKENS: u64 = 4_096;

/// Default daily hard cap of a bot without stored limits (new bots): USD.
pub const DEFAULT_DAILY_HARD_USD: f64 = 5.0;
/// Default daily hard cap of a bot without stored limits (new bots): tokens.
pub const DEFAULT_DAILY_HARD_TOKENS: u64 = 10_000_000;

/// Limit period. All are local calendar periods; the week starts on Monday.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Period {
    /// Local calendar day.
    Daily,
    /// Local calendar week (Monday start).
    Weekly,
    /// Local calendar month.
    Monthly,
}

impl Period {
    /// All periods, shortest first.
    pub const ALL: [Self; 3] = [Self::Daily, Self::Weekly, Self::Monthly];

    /// Stable lowercase name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Daily => "daily",
            Self::Weekly => "weekly",
            Self::Monthly => "monthly",
        }
    }

    /// `[start, end)` of the local period containing `now_ms`, in ms since epoch.
    #[must_use]
    pub fn bounds_ms(self, now_ms: i64) -> (i64, i64) {
        let now: DateTime<Local> = Local
            .timestamp_millis_opt(now_ms)
            .single()
            .unwrap_or_else(Local::now);
        let midnight = |d: chrono::NaiveDate| {
            // A DST gap can swallow local midnight: take the first valid hour.
            (0..=3)
                .find_map(|h| {
                    Local
                        .from_local_datetime(&d.and_hms_opt(h, 0, 0)?)
                        .earliest()
                })
                .map_or(0, |t| t.timestamp_millis())
        };
        let today = now.date_naive();
        match self {
            Self::Daily => (midnight(today), midnight(today + Duration::days(1))),
            Self::Weekly => {
                let monday =
                    today - Duration::days(i64::from(today.weekday().num_days_from_monday()));
                (midnight(monday), midnight(monday + Duration::days(7)))
            }
            Self::Monthly => crate::spend::month_bounds_ms(now_ms),
        }
    }
}

/// Limits of one period. Every field is optional; `soft_*` only notifies,
/// `hard_*` stops the run before the model call.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PeriodLimit {
    /// Notify at 80% and 100% of this amount (USD).
    pub soft_usd: Option<f64>,
    /// Stop model calls beyond this amount (USD).
    pub hard_usd: Option<f64>,
    /// Notify at 80% and 100% of this many tokens.
    pub soft_tokens: Option<u64>,
    /// Stop model calls beyond this many tokens.
    pub hard_tokens: Option<u64>,
}

/// The limits of one bot.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BotLimits {
    /// Per local day.
    pub daily: PeriodLimit,
    /// Per local week.
    pub weekly: PeriodLimit,
    /// Per local month.
    pub monthly: PeriodLimit,
}

impl BotLimits {
    /// What a bot without stored limits gets: a modest hard daily cap.
    #[must_use]
    pub fn default_for_new_bot() -> Self {
        Self {
            daily: PeriodLimit {
                hard_usd: Some(DEFAULT_DAILY_HARD_USD),
                hard_tokens: Some(DEFAULT_DAILY_HARD_TOKENS),
                ..PeriodLimit::default()
            },
            ..Self::default()
        }
    }

    /// The limit of one period.
    #[must_use]
    pub fn period(&self, p: Period) -> &PeriodLimit {
        match p {
            Period::Daily => &self.daily,
            Period::Weekly => &self.weekly,
            Period::Monthly => &self.monthly,
        }
    }

    /// Whether any limit is set.
    #[must_use]
    pub fn any(&self) -> bool {
        Period::ALL
            .iter()
            .any(|p| *self.period(*p) != PeriodLimit::default())
    }

    /// Reject out-of-range values and a soft limit above its hard limit.
    ///
    /// # Errors
    /// A human-readable reason.
    pub fn validate(&self) -> Result<(), String> {
        for p in Period::ALL {
            let l = self.period(p);
            let name = p.as_str();
            for (label, v) in [("soft_usd", l.soft_usd), ("hard_usd", l.hard_usd)] {
                if let Some(v) = v
                    && !(v.is_finite() && (MIN_USD..=MAX_USD).contains(&v))
                {
                    return Err(format!(
                        "{name}.{label} must be between {MIN_USD} and {MAX_USD}"
                    ));
                }
            }
            for (label, v) in [
                ("soft_tokens", l.soft_tokens),
                ("hard_tokens", l.hard_tokens),
            ] {
                if let Some(v) = v
                    && !(1..=MAX_TOKENS).contains(&v)
                {
                    return Err(format!("{name}.{label} must be between 1 and {MAX_TOKENS}"));
                }
            }
            if let (Some(s), Some(h)) = (l.soft_usd, l.hard_usd)
                && s > h
            {
                return Err(format!(
                    "{name}: the soft money limit is above the hard limit"
                ));
            }
            if let (Some(s), Some(h)) = (l.soft_tokens, l.hard_tokens)
                && s > h
            {
                return Err(format!(
                    "{name}: the soft token limit is above the hard limit"
                ));
            }
        }
        Ok(())
    }
}

/// The effective limits of `bot`.
///
/// No row = the new-bot default; a row that cannot be parsed or validated is an
/// error and the caller must fail closed, never treat it as "no limits".
///
/// # Errors
/// Database error or unusable stored value.
pub fn load_limits(st: &Storage, bot: BotId) -> Result<BotLimits, String> {
    match st.get_bot_spend_limits(bot) {
        Ok(None) => Ok(BotLimits::default_for_new_bot()),
        Ok(Some(raw)) => {
            let l: BotLimits = serde_json::from_str(&raw)
                .map_err(|e| format!("stored bot limits are unreadable: {e}"))?;
            l.validate()
                .map_err(|e| format!("stored bot limits are invalid: {e}"))?;
            Ok(l)
        }
        Err(e) => Err(format!("bot limits unavailable: {e}")),
    }
}

/// Usage of one bot in one period.
#[derive(Debug, Clone, PartialEq)]
pub struct PeriodUsage {
    /// Period start (ms since epoch).
    pub start_ms: i64,
    /// Period end, when the counter resets (ms since epoch).
    pub end_ms: i64,
    /// Money counted in the period (USD).
    pub usd: f64,
    /// Tokens counted in the period.
    pub tokens: u64,
    /// Calls in the period whose provider reported no usage (counted by estimate).
    pub unreported_calls: u64,
}

/// Usage of `bot` in `period`.
///
/// # Errors
/// Fails on a database error.
pub fn period_usage(
    st: &Storage,
    user: UserId,
    bot: BotId,
    period: Period,
    now_ms: i64,
) -> storage::Result<PeriodUsage> {
    let (start_ms, end_ms) = period.bounds_ms(now_ms);
    let (usd, tokens, unreported_calls) = st.bot_usage_since(user, bot, start_ms)?;
    Ok(PeriodUsage {
        start_ms,
        end_ms,
        usd,
        tokens,
        unreported_calls,
    })
}

/// Measure of a limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Metric {
    Usd,
    Tokens,
}

impl Metric {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Usd => "usd",
            Self::Tokens => "tokens",
        }
    }
}

/// In-flight estimates of one bot (so parallel calls cannot all pass just under a cap).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct BotReserved {
    pub usd: f64,
    pub tokens: u64,
    pub calls: u32,
}

/// Process-wide state of the per-bot limits.
#[derive(Default)]
pub(crate) struct BotLimitState {
    pub reserved: parking_lot::Mutex<HashMap<BotId, BotReserved>>,
    /// Alerts already issued in this process (the durable marker is the audit row).
    pub alerted: parking_lot::Mutex<std::collections::HashSet<String>>,
}

/// Estimate held while one call is in flight; released when it ends.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Reservation {
    pub bot: BotId,
    pub usd: f64,
    pub tokens: u64,
}

impl BotLimitState {
    pub(crate) fn release(&self, r: Option<Reservation>) {
        let Some(r) = r else { return };
        let mut map = self.reserved.lock();
        if let Some(e) = map.get_mut(&r.bot) {
            e.calls = e.calls.saturating_sub(1);
            e.tokens = e.tokens.saturating_sub(r.tokens);
            e.usd = (e.usd - r.usd).max(0.0);
            if e.calls == 0 {
                map.remove(&r.bot);
            }
        }
    }
}

/// Conservative prompt and completion tokens of the next call.
///
/// The prompt is the largest seen so far in the run (or a default for the first
/// call), raised to the request's own size estimate when that is bigger.
pub(crate) fn estimate_tokens(
    prompt_seen: Option<u64>,
    prompt_hint: Option<u64>,
    max_tokens: Option<u32>,
) -> (u64, u64) {
    (
        prompt_seen
            .unwrap_or(DEFAULT_PROMPT_TOKENS)
            .max(prompt_hint.unwrap_or(0)),
        max_tokens.map_or(DEFAULT_COMPLETION_TOKENS, u64::from),
    )
}

/// The limit an exceeded check names, for the error message and the audit row.
pub(crate) struct Breach {
    pub period: Period,
    pub metric: Metric,
    pub limit: f64,
    pub used: f64,
    pub resets_ms: i64,
}

/// First hard limit that the next call would break.
#[allow(clippy::cast_precision_loss)]
pub(crate) fn find_breach(
    limits: &BotLimits,
    usage: &[(Period, PeriodUsage)],
    reserved: BotReserved,
    est_usd: Option<f64>,
    est_tokens: u64,
) -> Option<Breach> {
    for (p, u) in usage {
        let l = limits.period(*p);
        if let Some(hard) = l.hard_usd {
            // Unknown price: the call adds no known cost, but a limit already
            // reached still stops it.
            let add = est_usd.unwrap_or(0.0);
            let used = u.usd + reserved.usd;
            if used + add > hard || u.usd >= hard {
                return Some(Breach {
                    period: *p,
                    metric: Metric::Usd,
                    limit: hard,
                    used,
                    resets_ms: u.end_ms,
                });
            }
        }
        if let Some(hard) = l.hard_tokens {
            let used = u.tokens.saturating_add(reserved.tokens);
            if used.saturating_add(est_tokens) > hard || u.tokens >= hard {
                return Some(Breach {
                    period: *p,
                    metric: Metric::Tokens,
                    limit: hard as f64,
                    used: used as f64,
                    resets_ms: u.end_ms,
                });
            }
        }
    }
    None
}

/// One crossed notification threshold.
pub(crate) struct Alert {
    pub period: Period,
    pub metric: Metric,
    pub level: u32,
    pub base: f64,
    pub start_ms: i64,
}

/// Thresholds crossed by the recorded usage. Levels 80 and 100 apply to the
/// soft limit; without one, 80 applies to the hard limit (100 is the stop).
#[allow(clippy::cast_precision_loss)]
pub(crate) fn crossed_alerts(limits: &BotLimits, usage: &[(Period, PeriodUsage)]) -> Vec<Alert> {
    let mut out = Vec::new();
    for (p, u) in usage {
        let l = limits.period(*p);
        for (metric, used, soft, hard) in [
            (Metric::Usd, u.usd, l.soft_usd, l.hard_usd),
            (
                Metric::Tokens,
                u.tokens as f64,
                l.soft_tokens.map(|v| v as f64),
                l.hard_tokens.map(|v| v as f64),
            ),
        ] {
            let (base, top) = match (soft, hard) {
                (Some(s), _) => (s, 100),
                (None, Some(h)) => (h, 80),
                (None, None) => continue,
            };
            for level in [80_u32, 100] {
                if level <= top && used >= base * f64::from(level) / 100.0 {
                    out.push(Alert {
                        period: *p,
                        metric,
                        level,
                        base,
                        start_ms: u.start_ms,
                    });
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_has_a_daily_hard_cap_and_validates() {
        let d = BotLimits::default_for_new_bot();
        assert!(d.validate().is_ok());
        assert_eq!(d.daily.hard_usd, Some(DEFAULT_DAILY_HARD_USD));
        assert!(!BotLimits::default().any());
    }

    #[test]
    fn validate_rejects_nonsense() {
        let mut l = BotLimits::default();
        l.daily.hard_usd = Some(f64::NAN);
        assert!(l.validate().is_err());
        let mut l = BotLimits::default();
        l.weekly.soft_tokens = Some(10);
        l.weekly.hard_tokens = Some(5);
        assert!(l.validate().is_err());
        let mut l = BotLimits::default();
        l.monthly.hard_tokens = Some(0);
        assert!(l.validate().is_err());
    }

    #[test]
    fn period_bounds_contain_now() {
        let now = Local::now().timestamp_millis();
        for p in Period::ALL {
            let (s, e) = p.bounds_ms(now);
            assert!(s <= now && now < e, "{p:?}");
        }
        let (ds, de) = Period::Daily.bounds_ms(now);
        let (ws, we) = Period::Weekly.bounds_ms(now);
        assert!(ws <= ds && de <= we);
    }
}
