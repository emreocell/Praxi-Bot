//! Owner-authored rules on existing schedules, not another execution engine.
use chrono::{DateTime, Timelike, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ScheduleSpec, Trigger};

/// Maximum queued events and bytes per rule, including during quiet hours.
pub const QUEUE_CAP: usize = 100;
pub const QUEUE_BYTES: usize = 48 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TriggerSource {
    /// Uses the same permission-gated `email_search` path as `Trigger::Email`.
    Mail {
        account: String,
        folder: String,
        own_address: String,
        #[serde(default = "default_poll")]
        poll_seconds: u64,
    },
    /// References the already signed, replay-protected incoming webhook.
    Webhook { id: uuid::Uuid },
    /// Uses the existing scheduler clock and occurrence recovery.
    Time { spec: ScheduleSpec },
    /// A new item in an RSS/Atom feed. The owner's URL, fetched politely with the SSRF guard.
    Feed {
        url: String,
        #[serde(default = "default_feed_poll")]
        poll_seconds: u64,
    },
    /// The text a CSS selector picks on a page changed. Same fetch rules as `Feed`.
    Page {
        url: String,
        selector: String,
        #[serde(default = "default_page_poll")]
        poll_seconds: u64,
    },
    /// A calendar event starts within `minutes_before` minutes (`calendar_list_events`).
    Calendar {
        #[serde(default)]
        account: Option<String>,
        minutes_before: u32,
        #[serde(default = "default_calendar_poll")]
        poll_seconds: u64,
    },
    /// A Home Assistant entity changes state (`ha_get_state`).
    HomeAssistant {
        entity_id: String,
        #[serde(default)]
        to_state: Option<String>,
        #[serde(default = "default_ha_poll")]
        poll_seconds: u64,
    },
}

const fn default_poll() -> u64 {
    120
}
/// Web sources are other people's servers: never faster than every 15 minutes.
pub const WEB_MIN_POLL_SECONDS: u64 = 900;
const fn default_feed_poll() -> u64 {
    1800
}
const fn default_page_poll() -> u64 {
    3600
}
const fn default_calendar_poll() -> u64 {
    120
}
const fn default_ha_poll() -> u64 {
    60
}

impl TriggerSource {
    /// The polled trigger of an owner source that needs no mail filters; `None` for the rest.
    #[must_use]
    pub fn watch_trigger(&self) -> Option<Trigger> {
        match self {
            Self::Feed { url, .. } => Some(Trigger::Feed { url: url.clone() }),
            Self::Page { url, selector, .. } => Some(Trigger::PageChange {
                url: url.clone(),
                selector: selector.clone(),
            }),
            Self::Calendar {
                account,
                minutes_before,
                ..
            } => Some(Trigger::Calendar {
                account: account.clone().filter(|a| !a.is_empty()),
                minutes_before: *minutes_before,
            }),
            Self::HomeAssistant {
                entity_id,
                to_state,
                ..
            } => Some(Trigger::HomeAssistant {
                entity_id: entity_id.clone(),
                to_state: to_state.clone().filter(|s| !s.is_empty()),
            }),
            Self::Mail { .. } | Self::Webhook { .. } | Self::Time { .. } => None,
        }
    }

    /// How often the scheduler checks this source; `None` for sources it does not poll.
    #[must_use]
    pub const fn poll_seconds(&self) -> Option<u64> {
        match self {
            Self::Mail { poll_seconds, .. }
            | Self::Feed { poll_seconds, .. }
            | Self::Page { poll_seconds, .. }
            | Self::Calendar { poll_seconds, .. }
            | Self::HomeAssistant { poll_seconds, .. } => Some(*poll_seconds),
            Self::Webhook { .. } | Self::Time { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SenderMatch {
    /// The sender filter is one exact address.
    Equals,
    /// Legacy marker, and the default when a stored or posted rule names no
    /// mode. There is NO substring mode: `migrate_legacy` turns it into
    /// `equals`/`domain` when the value looks like an address or a domain;
    /// free text is refused (a stored trigger is disabled with a notice).
    /// `validate` never accepts `contains`.
    #[default]
    Contains,
    /// The sender filter is a domain; subdomains match on a label boundary.
    Domain,
}

impl SenderMatch {
    /// The mode that fits a filter value under the shared sender rule
    /// ([`protocol::sender`]): `equals` for an address, `domain` for a domain,
    /// `None` for anything else (free text).
    #[must_use]
    pub fn for_value(value: &str) -> Option<Self> {
        match protocol::sender::SenderRule::parse(value)? {
            protocol::sender::SenderRule::Address(_) => Some(Self::Equals),
            protocol::sender::SenderRule::Domain(_) => Some(Self::Domain),
        }
    }
}

impl Filters {
    /// Move a legacy `contains` filter onto the shared rule: the value becomes
    /// its canonical address/`@domain` form with the matching mode. Returns
    /// `false` (nothing changed) when the value is free text; the caller must
    /// then disable the trigger and tell the owner.
    pub fn migrate_legacy(&mut self) -> bool {
        if self.sender_match != SenderMatch::Contains {
            return true;
        }
        let Some(canonical) = protocol::sender::migrate_legacy_contains(&self.sender) else {
            return false;
        };
        let Some(mode) = SenderMatch::for_value(&canonical) else {
            return false;
        };
        self.sender = canonical;
        self.sender_match = mode;
        true
    }
}

/// Mail rules need a sender: an address (`equals`) or a domain (`domain`),
/// and the mode must say what the value is. `contains` is refused.
fn sender_filter_consistent(filters: &Filters) -> bool {
    SenderMatch::for_value(&filters.sender)
        .is_some_and(|m| m == filters.sender_match && m != SenderMatch::Contains)
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Filters {
    #[serde(default)]
    pub sender: String,
    #[serde(default)]
    pub sender_match: SenderMatch,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub has_attachment: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuietHours {
    /// Local minutes since midnight, interpreted in `timezone` on every event.
    pub start: u16,
    pub end: u16,
    pub timezone: String,
    #[serde(default)]
    pub drop: bool,
}

impl QuietHours {
    #[must_use]
    pub fn contains(&self, now: i64) -> bool {
        let Some(dt) = DateTime::<Utc>::from_timestamp_millis(now) else {
            return true;
        };
        let Ok(tz) = self.timezone.parse::<chrono_tz::Tz>() else {
            return true;
        };
        let dt = dt.with_timezone(&tz);
        let minute = u16::try_from(dt.hour() * 60 + dt.minute()).unwrap_or(0);
        if self.start < self.end {
            minute >= self.start && minute < self.end
        } else {
            minute >= self.start || minute < self.end
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub per_hour: u32,
    pub per_day: u32,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            per_hour: 10,
            per_day: 50,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub source: TriggerSource,
    #[serde(default)]
    pub filters: Filters,
    #[serde(default)]
    pub quiet_hours: Option<QuietHours>,
    #[serde(default)]
    pub limits: Limits,
    /// The strictest configured ceiling among a bot's rules applies to all.
    #[serde(default = "default_bot_limits")]
    pub bot_limits: Limits,
}
fn default_bot_limits() -> Limits {
    Limits {
        per_hour: 30,
        per_day: 150,
    }
}

impl Rule {
    /// Definition validation is shared by API and persistence.
    ///
    /// # Errors
    /// Invalid source, filters, quiet hours or bounded rate limits.
    pub fn validate(&self) -> Result<(), &'static str> {
        for limits in [&self.limits, &self.bot_limits] {
            if limits.per_hour == 0
                || limits.per_day == 0
                || limits.per_hour > 1000
                || limits.per_day > 10000
            {
                return Err("choose limits between 1 and 1,000 per hour and 1 and 10,000 per day");
            }
        }
        if let Some(q) = &self.quiet_hours
            && (q.start >= 1440
                || q.end >= 1440
                || q.start == q.end
                || q.timezone.parse::<chrono_tz::Tz>().is_err())
        {
            return Err("choose different quiet-hour times and a valid time zone");
        }
        if [&self.filters.sender, &self.filters.subject]
            .iter()
            .any(|s| !safe_config(s, 256) || s.len() > 256)
        {
            return Err("filters are too long or contain invisible characters");
        }
        match &self.source {
            TriggerSource::Mail {
                account,
                folder,
                own_address,
                poll_seconds,
            } => {
                if account.trim().is_empty()
                    || folder.trim().is_empty()
                    || !own_address.contains('@')
                    || !safe_config(account, 256)
                    || !safe_config(folder, 256)
                    || !safe_config(own_address, 320)
                    || !sender_filter_consistent(&self.filters)
                    || !(60..=86400).contains(poll_seconds)
                {
                    return Err(
                        "mail needs an account, folder, own address, sender filter and a polling interval of at least 60 seconds",
                    );
                }
            }
            TriggerSource::Time { spec } => {
                spec.validate().map_err(|_| "choose a valid schedule")?;
                let next = spec
                    .next_occurrences(0, 3)
                    .map_err(|_| "choose a valid schedule")?;
                if next
                    .windows(2)
                    .any(|pair| pair[1].saturating_sub(pair[0]) < 300_000)
                {
                    return Err("choose a schedule at least five minutes apart");
                }
                if matches!(spec, ScheduleSpec::Every { seconds } if *seconds < 300) {
                    return Err("choose a schedule at least five minutes apart");
                }
            }
            TriggerSource::Webhook { .. } => {}
            TriggerSource::Feed { poll_seconds, .. } | TriggerSource::Page { poll_seconds, .. } => {
                let trigger = self
                    .source
                    .watch_trigger()
                    .ok_or("choose a feed or page address")?;
                if trigger.validate().is_err() {
                    return Err(
                        "use an http(s) address without a password and a selector of visible characters",
                    );
                }
                if !(WEB_MIN_POLL_SECONDS..=86400).contains(poll_seconds) {
                    return Err(
                        "check a web address at most every 15 minutes (900 to 86,400 seconds)",
                    );
                }
            }
            TriggerSource::Calendar { poll_seconds, .. } => {
                let trigger = self.source.watch_trigger().ok_or("choose a calendar")?;
                if trigger.validate().is_err() {
                    return Err("choose 1 to 1,440 minutes before the event");
                }
                if !(60..=3600).contains(poll_seconds) {
                    return Err("check the calendar every 60 to 3,600 seconds");
                }
            }
            TriggerSource::HomeAssistant { poll_seconds, .. } => {
                let trigger = self.source.watch_trigger().ok_or("choose an entity")?;
                if trigger.validate().is_err() {
                    return Err("choose an entity such as light.kitchen and an optional state");
                }
                if !(30..=3600).contains(poll_seconds) {
                    return Err("check Home Assistant every 30 to 3,600 seconds");
                }
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn trigger(&self) -> Option<Trigger> {
        match &self.source {
            TriggerSource::Mail { account, .. } => Some(Trigger::Email {
                account: account.clone(),
                from_filter: self.filters.sender.clone(),
                subject_filter: (!self.filters.subject.is_empty())
                    .then(|| self.filters.subject.clone()),
            }),
            other => other.watch_trigger(),
        }
    }
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self.source {
            TriggerSource::Mail { .. } => "trigger.mail",
            TriggerSource::Webhook { .. } => "trigger.webhook",
            TriggerSource::Time { .. } => "trigger.schedule",
            TriggerSource::Feed { .. } => "trigger.feed",
            TriggerSource::Page { .. } => "trigger.page",
            TriggerSource::Calendar { .. } => "trigger.calendar",
            TriggerSource::HomeAssistant { .. } => "trigger.home_assistant",
        }
    }

    /// Metadata must be explicitly available; missing loop/attachment metadata
    /// never implies a positive attachment match. Bodies are never fetched here.
    #[must_use]
    pub fn accepts_mail(&self, message: &Value) -> bool {
        let TriggerSource::Mail {
            own_address,
            folder,
            ..
        } = &self.source
        else {
            return false;
        };
        // Security: sender filters match the parsed ADDRESS only. The `From`
        // display name is attacker-controlled (`"boss@corp.com" <x@evil.test>`)
        // and is never used for matching.
        let address = message["from_addr"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_lowercase();
        if address.is_empty() || !message["automation_headers"].is_object() {
            return false;
        }
        let wanted = self.filters.sender.trim().to_lowercase();
        // ONE shared rule (also used by the mail channel): the parsed address
        // equals the wanted address or sits in the wanted domain (label
        // boundary). Empty or free-text filters match nothing.
        let sender_matches = protocol::sender::sender_matches(&address, &wanted);
        let header = |key: &str| {
            message["automation_headers"][key]
                .as_str()
                .unwrap_or("")
                .to_lowercase()
        };
        let explicit_no_reply = wanted.contains("no-reply")
            || wanted.contains("noreply")
            || wanted.contains("no_reply");
        let no_reply = ["no-reply", "noreply", "no_reply"]
            .iter()
            .any(|s| address.contains(s));
        sender_matches
            && address != own_address.trim().to_lowercase()
            && (header("auto_submitted").is_empty() || header("auto_submitted") == "no")
            && !matches!(header("precedence").as_str(), "bulk" | "list")
            && header("list_unsubscribe").is_empty()
            && (!no_reply || explicit_no_reply)
            && message["subject"]
                .as_str()
                .unwrap_or("")
                .to_lowercase()
                .contains(&self.filters.subject.to_lowercase())
            && message["folder"]
                .as_str()
                .is_none_or(|v| v.eq_ignore_ascii_case(folder))
            && (!self.filters.has_attachment || message["has_attachment"].as_bool() == Some(true))
    }
}

#[must_use]
pub fn safe_config(text: &str, cap: usize) -> bool {
    text.chars().count() <= cap
        && !text
            .chars()
            .any(|c| c.is_control() || protocol::approval_text::is_hidden(c))
}

/// Apply before persisting or displaying external headers, not just at render.
#[must_use]
pub fn clean_header(text: &str, cap: usize) -> String {
    text.chars()
        .filter(|c| !c.is_control() && !protocol::approval_text::is_hidden(*c))
        .take(cap)
        .collect()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RuleState {
    #[serde(default)]
    pub pending: Vec<Value>,
    #[serde(default)]
    pub fired: Vec<i64>,
    pub last_error: Option<String>,
    #[serde(default)]
    pub last_fired_at_ms: Option<i64>,
    #[serde(default)]
    pub not_before_ms: i64,
}
impl RuleState {
    pub fn enqueue(&mut self, input: Value) -> bool {
        if self.pending.len() >= QUEUE_CAP {
            return false;
        }
        self.pending.push(input);
        if serde_json::to_vec(&self.pending).map_or(true, |v| v.len() > QUEUE_BYTES) {
            self.pending.pop();
            return false;
        }
        true
    }
}
