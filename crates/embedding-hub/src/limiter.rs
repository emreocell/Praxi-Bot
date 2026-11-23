//! Client-side rate limiter: requests per minute, tokens per minute and
//! requests per day, so a free key is never pushed into a 429 ban.
//!
//! The limiter keeps a safety margin (it plans to use 90% of each limit) and
//! never sleeps by itself: [`RateLimiter::reserve`] tells the caller how long
//! to wait so the caller can decide to wait or to use the next provider.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

const MINUTE: Duration = Duration::from_secs(60);
const DAY: Duration = Duration::from_secs(24 * 3600);

/// Provider limits; `None` means "not limited by the client".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    /// Requests per minute.
    #[serde(default)]
    pub rpm: Option<u32>,
    /// Tokens per minute.
    #[serde(default)]
    pub tpm: Option<u32>,
    /// Requests per day.
    #[serde(default)]
    pub rpd: Option<u32>,
}

/// One meter: used out of limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meter {
    /// Used in the current window.
    pub used: u64,
    /// The provider's limit, if known.
    pub limit: Option<u64>,
}

/// What the Settings bars show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Requests in the last minute.
    pub requests_per_minute: Meter,
    /// Tokens in the last minute.
    pub tokens_per_minute: Meter,
    /// Requests in the last 24 hours.
    pub requests_per_day: Meter,
}

/// Why a request cannot go out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Try again after this long.
    Wait(Duration),
    /// The request alone is larger than the per-minute token budget.
    TooLarge,
}

/// Rolling-window limiter (in memory; resets when the daemon restarts).
#[derive(Debug)]
pub struct RateLimiter {
    limits: Limits,
    /// (time, tokens), oldest first; kept for 24 hours.
    events: VecDeque<(Instant, u64)>,
    blocked_until: Option<Instant>,
}

/// 90% of the provider limit, at least 1.
fn margin(limit: u32) -> u64 {
    (u64::from(limit) * 9 / 10).max(1)
}

/// Conservative token estimate (~3 characters per token).
#[must_use]
pub fn estimate_tokens(text: &str) -> u64 {
    (text.chars().count() as u64) / 3 + 1
}

impl RateLimiter {
    /// New limiter.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            events: VecDeque::new(),
            blocked_until: None,
        }
    }

    /// The configured limits.
    #[must_use]
    pub fn limits(&self) -> Limits {
        self.limits
    }

    fn prune(&mut self, now: Instant) {
        while self
            .events
            .front()
            .is_some_and(|(t, _)| now.saturating_duration_since(*t) >= DAY)
        {
            self.events.pop_front();
        }
    }

    fn minute_events(&self, now: Instant) -> Vec<(Instant, u64)> {
        self.events
            .iter()
            .copied()
            .filter(|(t, _)| now.saturating_duration_since(*t) < MINUTE)
            .collect()
    }

    /// Largest number of tokens one request may carry.
    #[must_use]
    pub fn token_budget(&self) -> Option<u64> {
        self.limits.tpm.map(margin)
    }

    /// Reserve one request of `tokens`. `Ok` records it.
    ///
    /// # Errors
    /// [`Refusal::Wait`] with the time until it fits, or [`Refusal::TooLarge`].
    pub fn reserve(&mut self, now: Instant, tokens: u64) -> Result<(), Refusal> {
        self.prune(now);
        if let Some(until) = self.blocked_until {
            if until > now {
                return Err(Refusal::Wait(until - now));
            }
            self.blocked_until = None;
        }
        if self.token_budget().is_some_and(|b| tokens > b) {
            return Err(Refusal::TooLarge);
        }
        let mut wait = Duration::ZERO;
        if let Some(limit) = self.limits.rpd
            && self.events.len() as u64 >= margin(limit)
        {
            // The oldest request leaves the 24 hour window first.
            let oldest = self.events.front().map_or(now, |(t, _)| *t);
            wait = wait.max((oldest + DAY).saturating_duration_since(now));
        }
        let minute = self.minute_events(now);
        if let Some(limit) = self.limits.rpm
            && minute.len() as u64 >= margin(limit)
        {
            let idx = minute.len() - usize::try_from(margin(limit)).unwrap_or(1);
            wait = wait.max((minute[idx].0 + MINUTE).saturating_duration_since(now));
        }
        if let Some(budget) = self.token_budget() {
            let mut used: u64 = minute.iter().map(|(_, k)| k).sum();
            if used + tokens > budget {
                // Walk forward until enough old tokens have expired.
                for (t, k) in &minute {
                    used -= k;
                    wait = wait.max((*t + MINUTE).saturating_duration_since(now));
                    if used + tokens <= budget {
                        break;
                    }
                }
            }
        }
        if wait > Duration::ZERO {
            return Err(Refusal::Wait(wait.max(Duration::from_millis(1))));
        }
        self.events.push_back((now, tokens));
        Ok(())
    }

    /// The provider said 429: stay away for `d` whatever our own counters say.
    pub fn block_for(&mut self, now: Instant, d: Duration) {
        let until = now + d;
        if self.blocked_until.is_none_or(|b| b < until) {
            self.blocked_until = Some(until);
        }
    }

    /// Current usage for the meter bars.
    pub fn usage(&mut self, now: Instant) -> Usage {
        self.prune(now);
        let minute = self.minute_events(now);
        Usage {
            requests_per_minute: Meter {
                used: minute.len() as u64,
                limit: self.limits.rpm.map(u64::from),
            },
            tokens_per_minute: Meter {
                used: minute.iter().map(|(_, k)| k).sum(),
                limit: self.limits.tpm.map(u64::from),
            },
            requests_per_day: Meter {
                used: self.events.len() as u64,
                limit: self.limits.rpd.map(u64::from),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn rpm_margin_makes_the_caller_wait_instead_of_hitting_the_limit() {
        let t0 = Instant::now();
        let mut l = RateLimiter::new(Limits {
            rpm: Some(10),
            ..Limits::default()
        });
        // 90% of 10 = 9 requests fit, the 10th waits for the first to expire.
        for _ in 0..9 {
            l.reserve(t0, 1).unwrap();
        }
        assert_eq!(l.reserve(t0, 1), Err(Refusal::Wait(secs(60))));
        assert!(l.reserve(t0 + secs(61), 1).is_ok());
    }

    #[test]
    fn token_budget_and_daily_limit_are_enforced() {
        let t0 = Instant::now();
        let mut l = RateLimiter::new(Limits {
            rpm: None,
            tpm: Some(1000),
            rpd: None,
        });
        assert_eq!(l.reserve(t0, 901), Err(Refusal::TooLarge));
        l.reserve(t0, 800).unwrap();
        // 800 + 800 > 900: wait for the first minute to pass.
        assert_eq!(l.reserve(t0, 800), Err(Refusal::Wait(secs(60))));

        // rpd 2 -> margin 1: after one request the day window decides.
        let mut d = RateLimiter::new(Limits {
            rpm: None,
            tpm: None,
            rpd: Some(2),
        });
        d.reserve(t0, 1).unwrap();
        assert_eq!(
            d.reserve(t0 + secs(61), 1),
            Err(Refusal::Wait(secs(24 * 3600 - 61)))
        );
    }

    #[test]
    fn retry_after_blocks_and_usage_reports_bars() {
        let t0 = Instant::now();
        let mut l = RateLimiter::new(Limits {
            rpm: Some(100),
            tpm: Some(30_000),
            rpd: Some(1000),
        });
        l.reserve(t0, 120).unwrap();
        l.block_for(t0, secs(30));
        assert_eq!(l.reserve(t0 + secs(5), 1), Err(Refusal::Wait(secs(25))));
        let u = l.usage(t0 + secs(5));
        assert_eq!(u.requests_per_minute.used, 1);
        assert_eq!(u.tokens_per_minute.used, 120);
        assert_eq!(u.requests_per_day.limit, Some(1000));
        assert!(l.reserve(t0 + secs(31), 1).is_ok());
    }
}
