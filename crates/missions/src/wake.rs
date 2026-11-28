//! Wakes: how a mission that parked an item gets going again.
//!
//! `mission_wait` parks one ledger item on a *wait*: a timer (`after`), a mail in the bot's own
//! inbox, or a webhook the owner configured. The session ends instead of holding a run. The
//! supervisor's wake loop (leader only) watches the waits; when one fires it turns the item
//! back to `todo`, records a *wake* and starts the next session, whose brief carries the wake.
//! A wait that runs out marks its item `blocked` (the owner is told) and the mission goes on.
//!
//! Everything a wake carries from the outside (a mail summary, a webhook body) is untrusted
//! data: it is stored capped, put into the brief only inside a fenced data block and never
//! becomes a user message (see [`crate::brief`]).

use std::fmt;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::clip;

/// How often one mail wait looks into the mailbox, in seconds.
pub const MAIL_POLL_SECS: i64 = 60;
/// The pause after a failed look grows up to this many seconds.
pub const MAIL_POLL_BACKOFF_MAX_SECS: i64 = 10 * 60;
/// Mailboxes looked into per wake-loop pass; the rest waits for the next pass.
pub const MAX_PROBES_PER_PASS: usize = 8;
/// Wakes that may take a waiting mission back to `running` per hour (a wake storm stops here;
/// the wakes stay recorded and are delivered when the window reopens).
pub const WAKE_RELEASES_PER_HOUR: u32 = 6;
/// How long an ended wait (fired, timed out, cancelled) and a consumed wake stay in the database
/// before the supervisor deletes them, in milliseconds (30 days: long enough to read a timeline).
pub const WAKE_RETENTION_MS: i64 = 30 * 24 * 3_600_000;
/// How often the wake loop prunes old waits and wakes, in milliseconds.
pub const PRUNE_EVERY_MS: i64 = 3_600_000;
/// Most unconsumed wakes one mission keeps; a newer wake pushes the oldest out.
pub const MAX_PENDING_WAKES: usize = 50;
/// Longest filter text of a mail wait, in characters.
pub const MAX_FILTER_CHARS: usize = 100;
/// Longest reason the bot gives a wait, in characters.
pub const MAX_REASON_CHARS: usize = 200;

/// What a wait waits for, stored as JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WaitSpec {
    /// A timer: fires at the wait's `due_at_ms`.
    After,
    /// A mail in the bot's own inbox that matches the filter.
    Mail {
        /// Which mail.
        filter: MailFilter,
    },
    /// A call of a webhook the owner created (an incoming webhook of this bot).
    Webhook {
        /// The incoming webhook's id.
        trigger_id: String,
        /// Its name, for the owner's screen.
        name: String,
    },
    /// A recurrence rule (`FREQ=...`); fires at each occurrence until the series ends.
    Every {
        /// RFC 5545 `RRULE` (ASCII, checked by [`crate::recur`]).
        rrule: String,
        /// IANA time zone for local wall time; omitted means UTC.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timezone: Option<String>,
        /// Series anchor (`DTSTART`), Unix ms.
        dtstart_ms: i64,
    },
    /// A child run (subagent or helper) finishes.
    Child {
        /// The waiting child's run id.
        run_id: String,
    },
}

impl WaitSpec {
    /// Stable name of the kind: `after`, `mail`, `webhook`.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::After => "after",
            Self::Mail { .. } => "mail",
            Self::Webhook { .. } => "webhook",
            Self::Every { .. } => "every",
            Self::Child { .. } => "child",
        }
    }

    /// One plain English line for timeline events and the reason of a timed-out item.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::After => "a timer".to_owned(),
            Self::Mail { filter } => format!("a mail {}", filter.describe()),
            Self::Webhook { name, .. } => format!("the webhook \"{}\"", clip(name, 60)),
            Self::Every { rrule, .. } => format!("the schedule \"{}\"", clip(rrule, 60)),
            Self::Child { run_id } => format!("helper run {}", clip(run_id, 36)),
        }
    }
}

/// Which mail is the expected one. All text is matched case-insensitively against what the bot's
/// own inbox returns; nothing here is sent anywhere. At least one of the first three is set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailFilter {
    /// Sender address domain (`example.com` also matches `mail.example.com`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_domain: Option<String>,
    /// Text the subject must contain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_contains: Option<String>,
    /// Text the subject or the body must contain (part of the verification link, a word).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyword: Option<String>,
    /// The site the bot is signing up on (`https://example.com`), for the next session's brief.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site: Option<String>,
}

fn clean_text(name: &str, v: Option<&str>) -> Result<Option<String>, String> {
    let Some(v) = v.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    if v.chars().count() > MAX_FILTER_CHARS || v.chars().any(char::is_control) {
        return Err(format!(
            "`{name}` must be plain text of at most {MAX_FILTER_CHARS} characters"
        ));
    }
    Ok(Some(v.to_owned()))
}

impl MailFilter {
    /// Build a filter from the bot's words.
    ///
    /// # Errors
    /// No filter at all (it would match any mail), control characters, over-long text, a `site`
    /// that is not an http(s) address.
    pub fn new(
        from_domain: Option<&str>,
        subject_contains: Option<&str>,
        keyword: Option<&str>,
        site: Option<&str>,
    ) -> Result<Self, String> {
        let from_domain =
            clean_text("from_domain", from_domain)?.map(|d| d.trim_start_matches('@').to_owned());
        let subject_contains = clean_text("subject_contains", subject_contains)?;
        let keyword = clean_text("keyword", keyword)?;
        if from_domain.is_none() && subject_contains.is_none() && keyword.is_none() {
            return Err(
                "a mail wait needs at least one of from_domain, subject_contains or keyword: without a filter any mail would wake the mission".into(),
            );
        }
        let site = clean_text("site", site)?;
        if let Some(s) = &site
            && (!(s.starts_with("https://") || s.starts_with("http://"))
                || s.contains(char::is_whitespace))
        {
            return Err("`site` must be a web address such as https://example.com".into());
        }
        Ok(Self {
            from_domain,
            subject_contains,
            keyword,
            site,
        })
    }

    /// "from example.com about "verify"" for logs and reasons.
    #[must_use]
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if let Some(d) = &self.from_domain {
            parts.push(format!("from {d}"));
        }
        if let Some(s) = &self.subject_contains {
            parts.push(format!("with \"{}\" in the subject", clip(s, 40)));
        }
        if let Some(k) = &self.keyword {
            parts.push(format!("containing \"{}\"", clip(k, 40)));
        }
        parts.join(" ")
    }
}

/// Where a wait stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitState {
    /// Registered, being watched.
    Waiting,
    /// It happened: the item went back to `todo` with a wake.
    Fired,
    /// It did not happen in time.
    TimedOut,
    /// Nobody needs it any more (the item moved on, the mission ended).
    Cancelled,
}

impl WaitState {
    /// Stable database name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Fired => "fired",
            Self::TimedOut => "timed_out",
            Self::Cancelled => "cancelled",
        }
    }

    /// Inverse of [`Self::as_str`].
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "waiting" => Self::Waiting,
            "fired" => Self::Fired,
            "timed_out" => Self::TimedOut,
            "cancelled" => Self::Cancelled,
            _ => return None,
        })
    }
}

/// One registered wait.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Wait {
    /// Row id.
    pub id: i64,
    /// The mission.
    pub mission_id: String,
    /// The item parked on it.
    pub item_key: String,
    /// What it waits for.
    pub spec: WaitSpec,
    /// Why the bot waits (its own words, shown to the owner; data, never an instruction).
    pub reason: String,
    /// Where it stands.
    pub state: WaitState,
    /// When it was registered.
    pub created_at_ms: i64,
    /// A timer: when it fires. A mail or webhook wait: when it gives up.
    pub due_at_ms: i64,
    /// A mail wait: the next look into the mailbox is not before this.
    pub next_poll_at_ms: i64,
    /// Looks into the mailbox so far.
    pub polls: u32,
    /// The last failure of a look (the wait goes on).
    pub last_error: Option<String>,
    /// When it ended.
    pub ended_at_ms: Option<i64>,
}

/// A wait to register.
#[derive(Debug, Clone)]
pub struct NewWait {
    /// The item to park.
    pub item_key: String,
    /// What to wait for.
    pub spec: WaitSpec,
    /// Why.
    pub reason: String,
    /// A timer: when it fires. A mail or webhook wait: when it gives up.
    pub due_at_ms: i64,
}

/// What woke a mission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakeReason {
    /// A timer ran out.
    Timer,
    /// The expected mail arrived.
    Mail,
    /// The webhook was called.
    Webhook,
    /// A recurrence rule fired.
    Schedule,
    /// A child run finished.
    Child,
}

impl WakeReason {
    /// Stable database name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Timer => "timer",
            Self::Mail => "mail",
            Self::Webhook => "webhook",
            Self::Schedule => "schedule",
            Self::Child => "child",
        }
    }

    /// Inverse of [`Self::as_str`].
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "timer" => Self::Timer,
            "mail" => Self::Mail,
            "webhook" => Self::Webhook,
            "schedule" => Self::Schedule,
            "child" => Self::Child,
            _ => return None,
        })
    }
}

impl fmt::Display for WakeReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A recorded wake. It stays in the brief until its item leaves `todo`/`doing`, so a session
/// that dies before acting on it does not lose the payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Wake {
    /// Row id.
    pub id: i64,
    /// The mission.
    pub mission_id: String,
    /// The wait that fired.
    pub wait_id: Option<i64>,
    /// The item to continue.
    pub item_key: String,
    /// What woke it.
    pub reason: WakeReason,
    /// What came from the outside (untrusted): a mail summary or a webhook body. Capped.
    pub payload: Option<Value>,
    /// When it was recorded.
    pub created_at_ms: i64,
    /// Why the bot parked the item (its own words, data).
    pub wait_reason: Option<String>,
    /// A mail wait: the site the bot named for the sign-up.
    pub site: Option<String>,
}

/// What one look into a mailbox asks.
#[derive(Debug, Clone, Copy)]
pub struct MailCheck<'a> {
    /// The mission the wait belongs to.
    pub mission_id: &'a str,
    /// Mail that arrived before this moment (Unix ms) is not the awaited one, whatever the
    /// wait's own start says: an out-of-office notice the wait was already put off for.
    pub not_before_ms: i64,
    /// The mission's bot: only its own mail identity is looked into.
    pub bot_id: &'a str,
    /// Which mail.
    pub filter: &'a MailFilter,
    /// When the wait was registered; mail from before the shortly-before window does not count.
    pub registered_at_ms: i64,
    /// Now.
    pub now_ms: i64,
}

/// The mail that was waited for.
#[derive(Debug, Clone, PartialEq)]
pub struct MailHit {
    /// Sender, subject, time, one-time codes and the judged links, already cleaned and capped.
    /// Untrusted: it comes from a stranger.
    pub summary: Value,
    /// The mail says the person is away and the wait should go on until then (Unix ms) instead
    /// of waking the item: set only for a mail matched to a mail we sent. The wait gives up no
    /// earlier than this; a hit after [`MAX_MAIL_DEFERS`] earlier ones wakes the item anyway.
    pub follow_up_at_ms: Option<i64>,
}

/// How often one mail wait is put off for an out-of-office reply before the mail wakes the item.
pub const MAX_MAIL_DEFERS: u32 = 3;

/// Looks into the bot's own mailbox once. The daemon implements it over the mail connector;
/// tests implement it with a fake inbox.
#[async_trait]
pub trait MailProbe: Send + Sync {
    /// `Ok(None)`: nothing matching yet. `Err`: the look failed (the wait goes on, slower).
    ///
    /// # Errors
    /// A short reason (no mail identity, the mailbox refused, the network).
    async fn check(&self, req: &MailCheck<'_>) -> Result<Option<MailHit>, String>;
}

/// What `mission_wait` may rely on when it registers a wait.
pub trait WakeChecks: Send + Sync {
    /// The bot can read a mail identity of its own.
    ///
    /// # Errors
    /// Why not, in a sentence for the model.
    fn mail_ready(&self, bot_id: &str) -> Result<(), String>;

    /// `trigger_id` is an enabled incoming webhook the owner created for this bot. Returns its
    /// name.
    ///
    /// # Errors
    /// Why not, in a sentence for the model.
    fn webhook(&self, bot_id: &str, trigger_id: &str) -> Result<String, String>;

    /// `child_run_id` is a run in flight that belongs to `parent_run_id`. Returns a short label
    /// for the owner UI.
    ///
    /// # Errors
    /// Why not, in a sentence for the model.
    fn child_run(&self, parent_run_id: &str, child_run_id: &str) -> Result<String, String> {
        let _ = (parent_run_id, child_run_id);
        Err("waiting for a helper is not available here".into())
    }
}

/// When a child run a mission was waiting for finishes, the runtime delivers it here.
pub trait ChildWakeSink: Send + Sync {
    /// `summary` is untrusted: the child's final text. When `mission_id` is set, only that
    /// mission's waits are woken.
    fn deliver(&self, child_run_id: &str, summary: &str, mission_id: Option<&str>);
}

/// Clean text that came from the outside before it is stored in a wake: control characters
/// become spaces and the length is capped.
#[must_use]
pub fn clean_external(text: &str, max: usize) -> String {
    let clean: String = text
        .chars()
        .map(|c| {
            if c.is_control() && c != '\n' && c != '\t' {
                ' '
            } else {
                c
            }
        })
        .collect();
    clip(&clean, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mail_filter_needs_something_to_match_on() {
        assert!(MailFilter::new(None, None, None, Some("https://example.com")).is_err());
        assert!(MailFilter::new(Some("  "), None, None, None).is_err());
        let f = MailFilter::new(Some("@Example.com"), Some("verify"), None, None).unwrap();
        assert_eq!(f.from_domain.as_deref(), Some("Example.com"));
        assert_eq!(
            f.describe(),
            "from Example.com with \"verify\" in the subject"
        );
    }

    #[test]
    fn a_mail_filter_rejects_control_characters_long_text_and_odd_sites() {
        assert!(MailFilter::new(Some("a\u{7}b"), None, None, None).is_err());
        assert!(MailFilter::new(None, Some(&"x".repeat(101)), None, None).is_err());
        for site in ["example.com", "javascript:alert(1)", "https://a b.com"] {
            assert!(
                MailFilter::new(Some("a.com"), None, None, Some(site)).is_err(),
                "{site}"
            );
        }
        assert!(MailFilter::new(Some("a.com"), None, None, Some("https://a.com/x")).is_ok());
    }

    #[test]
    fn specs_round_trip_as_tagged_json() {
        let spec = WaitSpec::Mail {
            filter: MailFilter::new(Some("a.com"), None, None, None).unwrap(),
        };
        let json = serde_json::to_string(&spec).unwrap();
        assert!(json.contains("\"kind\":\"mail\""), "{json}");
        assert_eq!(serde_json::from_str::<WaitSpec>(&json).unwrap(), spec);
        let after: WaitSpec = serde_json::from_str("{\"kind\":\"after\"}").unwrap();
        assert_eq!(after, WaitSpec::After);
        assert_eq!(after.kind(), "after");
    }

    #[test]
    fn external_text_is_cleaned_and_capped() {
        assert_eq!(clean_external("a\u{0}b\u{1b}c\nd", 50), "a b c\nd");
        assert!(clean_external(&"x".repeat(100), 10).chars().count() <= 10);
    }
}
