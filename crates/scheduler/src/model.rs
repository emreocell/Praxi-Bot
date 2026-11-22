//! Core data model for scheduled tasks, specifications, payloads, misfire
//! policies, and triggers.

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use croner::Cron;
use protocol::{BotId, ChatId, TaskId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Result, SchedulerError};

/// Maximum number of missed occurrences executed when [`MisfirePolicy::RunAll`]
/// is configured.
pub const RUN_ALL_CAP: usize = 10;

/// Type alias for a schedule's unique identifier (`UUIDv7` via [`protocol::TaskId`]).
pub type ScheduleId = TaskId;

/// Type alias for [`ScheduleSpec`].
pub type Spec = ScheduleSpec;

/// Type alias for [`Payload`].
pub type SchedulePayload = Payload;

/// Policy governing occurrences missed while the daemon was stopped or the
/// machine was asleep / across a wall-clock jump.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MisfirePolicy {
    /// Skip all missed occurrences and advance `next_run_at` to the first
    /// occurrence strictly after `now`.
    #[default]
    Skip,
    /// Fire once immediately for the missed window, then advance `next_run_at`
    /// to the first occurrence strictly after `now`.
    RunOnce,
    /// Fire all missed occurrences in chronological order (capped at
    /// [`RUN_ALL_CAP`] = 10), then advance `next_run_at` to the first
    /// occurrence strictly after `now`.
    RunAll,
}

impl MisfirePolicy {
    /// Stable lowercase string representation stored in SQLite.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Skip => "skip",
            Self::RunOnce => "run_once",
            Self::RunAll => "run_all",
        }
    }

    /// Parse a stored misfire policy string.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "skip" => Some(Self::Skip),
            "run_once" => Some(Self::RunOnce),
            "run_all" => Some(Self::RunAll),
            _ => None,
        }
    }
}

/// When a schedule fires.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleSpec {
    /// 5-field cron expression, optionally with an IANA timezone
    /// (e.g. `"0 9 * * 1-5"`, `"0 9 * * 1-5 Europe/Istanbul"`, or
    /// `"CRON_TZ=America/New_York 0 9 * * *"`). Defaults to `UTC` when no
    /// timezone is specified.
    Cron(String),
    /// Fixed-interval recurring schedule in seconds (`seconds > 0`).
    Every {
        /// Interval in seconds (must be `>= 1`).
        seconds: u64,
    },
    /// One-off schedule at an exact Unix epoch timestamp in milliseconds.
    At {
        /// Target Unix timestamp in milliseconds.
        unix_ms: i64,
    },
}

impl ScheduleSpec {
    /// Construct a 5-field cron specification in `UTC` (or with an embedded timezone).
    #[must_use]
    pub fn cron(expr: impl Into<String>) -> Self {
        Self::Cron(expr.into())
    }

    /// Construct a 5-field cron specification in the given IANA timezone.
    #[must_use]
    pub fn cron_tz(expr: &str, tz: &str) -> Self {
        let expr = expr.trim();
        let tz = tz.trim();
        Self::Cron(format!("{expr} {tz}"))
    }

    /// Validate that the schedule specification is well-formed.
    ///
    /// # Errors
    /// Returns [`SchedulerError::InvalidSpec`] if the cron expression or
    /// timezone is invalid or if `Every { seconds: 0 }`.
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Cron(raw) => {
                let _ = ParsedCron::parse(raw)?;
                Ok(())
            }
            Self::Every { seconds } => {
                if *seconds == 0 {
                    return Err(SchedulerError::InvalidSpec(
                        "every seconds must be > 0".into(),
                    ));
                }
                Ok(())
            }
            Self::At { .. } => Ok(()),
        }
    }

    /// Compute the next occurrence strictly after `after_ms` (Unix epoch ms).
    ///
    /// Returns `Ok(None)` when no future occurrence exists (e.g. `At` in the
    /// past or at `after_ms`).
    ///
    /// # Errors
    /// Fails if the specification is invalid.
    pub fn next_after(&self, after_ms: i64) -> Result<Option<i64>> {
        match self {
            Self::Cron(raw) => ParsedCron::parse(raw)?.next_after(after_ms),
            Self::Every { seconds } => {
                if *seconds == 0 {
                    return Err(SchedulerError::InvalidSpec(
                        "every seconds must be > 0".into(),
                    ));
                }
                let period_ms = i64::try_from(*seconds)
                    .unwrap_or(i64::MAX / 1000)
                    .saturating_mul(1000);
                Ok(Some(after_ms.saturating_add(period_ms)))
            }
            Self::At { unix_ms } => {
                if *unix_ms > after_ms {
                    Ok(Some(*unix_ms))
                } else {
                    Ok(None)
                }
            }
        }
    }

    /// Compute up to `count` upcoming occurrences strictly after `after_ms`
    /// (Unix epoch ms).
    ///
    /// # Errors
    /// Fails if the specification is invalid.
    pub fn next_occurrences(&self, after_ms: i64, count: usize) -> Result<Vec<i64>> {
        self.validate()?;
        if count == 0 {
            return Ok(Vec::new());
        }
        match self {
            Self::Cron(raw) => {
                let parsed = ParsedCron::parse(raw)?;
                let mut out = Vec::with_capacity(count);
                let mut cursor = after_ms;
                while out.len() < count {
                    let Some(next) = parsed.next_after(cursor)? else {
                        break;
                    };
                    out.push(next);
                    cursor = next;
                }
                Ok(out)
            }
            Self::Every { seconds } => {
                let period_ms = i64::try_from(*seconds)
                    .unwrap_or(i64::MAX / 1000)
                    .saturating_mul(1000);
                let mut out = Vec::with_capacity(count);
                let mut cursor = after_ms;
                for _ in 0..count {
                    let next = cursor.saturating_add(period_ms);
                    if next <= cursor {
                        break;
                    }
                    out.push(next);
                    cursor = next;
                }
                Ok(out)
            }
            Self::At { unix_ms } => {
                if *unix_ms > after_ms {
                    Ok(vec![*unix_ms])
                } else {
                    Ok(Vec::new())
                }
            }
        }
    }

    /// Compute the next occurrence strictly after `now_ms`, anchored to a
    /// previously scheduled due timestamp `first_due_ms`.
    ///
    /// For [`ScheduleSpec::Every`], this preserves the phase of `first_due_ms`
    /// (`first_due_ms + k * period_ms > now_ms`).
    ///
    /// # Errors
    /// Fails if the specification is invalid.
    pub fn next_after_due(&self, first_due_ms: i64, now_ms: i64) -> Result<Option<i64>> {
        match self {
            Self::Cron(_) => self.next_after(now_ms.max(first_due_ms)),
            Self::Every { seconds } => {
                if *seconds == 0 {
                    return Err(SchedulerError::InvalidSpec(
                        "every seconds must be > 0".into(),
                    ));
                }
                let period_ms = i64::try_from(*seconds)
                    .unwrap_or(i64::MAX / 1000)
                    .saturating_mul(1000);
                if now_ms < first_due_ms {
                    Ok(Some(first_due_ms))
                } else {
                    let elapsed = now_ms - first_due_ms;
                    let steps = (elapsed / period_ms).saturating_add(1);
                    Ok(Some(
                        first_due_ms.saturating_add(steps.saturating_mul(period_ms)),
                    ))
                }
            }
            Self::At { .. } => Ok(None),
        }
    }

    /// Collect all due occurrences in `[first_due_ms, now_ms]` (capped at
    /// [`RUN_ALL_CAP`]) along with the next future occurrence strictly after
    /// `now_ms`.
    ///
    /// # Errors
    /// Fails if the specification is invalid.
    pub fn collect_due_occurrences(
        &self,
        first_due_ms: i64,
        now_ms: i64,
    ) -> Result<(Vec<i64>, Option<i64>)> {
        if first_due_ms > now_ms {
            return Ok((Vec::new(), Some(first_due_ms)));
        }
        match self {
            Self::At { .. } => Ok((vec![first_due_ms], None)),
            Self::Every { seconds } => {
                if *seconds == 0 {
                    return Err(SchedulerError::InvalidSpec(
                        "every seconds must be > 0".into(),
                    ));
                }
                let period_ms = i64::try_from(*seconds)
                    .unwrap_or(i64::MAX / 1000)
                    .saturating_mul(1000);
                let mut due = Vec::new();
                let mut cursor = first_due_ms;
                while cursor <= now_ms && due.len() < RUN_ALL_CAP {
                    due.push(cursor);
                    cursor = cursor.saturating_add(period_ms);
                }
                let elapsed = now_ms - first_due_ms;
                let steps = (elapsed / period_ms).saturating_add(1);
                let next = first_due_ms.saturating_add(steps.saturating_mul(period_ms));
                Ok((due, Some(next)))
            }
            Self::Cron(raw) => {
                let parsed = ParsedCron::parse(raw)?;
                let mut due = vec![first_due_ms];
                let mut cursor = first_due_ms;
                while due.len() < RUN_ALL_CAP {
                    let Some(next) = parsed.next_after(cursor)? else {
                        return Ok((due, None));
                    };
                    if next <= now_ms {
                        due.push(next);
                        cursor = next;
                    } else {
                        return Ok((due, Some(next)));
                    }
                }
                let next = parsed.next_after(now_ms)?;
                Ok((due, next))
            }
        }
    }
}

/// Parsed 5-field cron expression paired with an IANA timezone.
#[derive(Debug, Clone)]
pub struct ParsedCron {
    cron: Cron,
    tz: Tz,
}

impl ParsedCron {
    /// Parse a 5-field cron string with optional timezone.
    ///
    /// Supported forms:
    /// - `"0 9 * * 1-5"` (5 fields, defaults to `UTC`)
    /// - `"0 9 * * 1-5 Europe/Istanbul"` (5 fields + trailing IANA timezone)
    /// - `"CRON_TZ=Europe/Istanbul 0 9 * * 1-5"` or `"TZ=Europe/Istanbul 0 9 * * 1-5"`
    ///
    /// # Errors
    /// Returns [`SchedulerError::InvalidSpec`] if the field count is not 5,
    /// the cron syntax is invalid, or the timezone name is unknown.
    pub fn parse(raw: &str) -> Result<Self> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(SchedulerError::InvalidSpec("empty cron expression".into()));
        }
        let tokens: Vec<&str> = trimmed.split_whitespace().collect();
        let (expr_str, tz) = match tokens.as_slice() {
            [m, h, dom, mon, dow] => (format!("{m} {h} {dom} {mon} {dow}"), chrono_tz::UTC),
            [first, m, h, dom, mon, dow]
                if first.starts_with("CRON_TZ=") || first.starts_with("TZ=") =>
            {
                let (_, tz_name) = first.split_once('=').unwrap_or(("", ""));
                let tz = parse_tz(tz_name)?;
                (format!("{m} {h} {dom} {mon} {dow}"), tz)
            }
            [m, h, dom, mon, dow, tz_token] => {
                let tz_clean = tz_token
                    .trim_start_matches('@')
                    .trim_start_matches('[')
                    .trim_end_matches(']');
                let tz = parse_tz(tz_clean)?;
                (format!("{m} {h} {dom} {mon} {dow}"), tz)
            }
            _ => {
                return Err(SchedulerError::InvalidSpec(format!(
                    "expected 5-field cron expression (optionally with timezone), got: '{raw}'"
                )));
            }
        };

        let cron = Cron::new(&expr_str)
            .parse()
            .map_err(|e| SchedulerError::InvalidSpec(format!("cron '{expr_str}': {e}")))?;

        Ok(Self { cron, tz })
    }

    /// Return the parsed timezone.
    #[must_use]
    pub const fn timezone(&self) -> Tz {
        self.tz
    }

    /// Compute the next occurrence strictly after `after_ms` (Unix epoch ms).
    ///
    /// # Errors
    /// Returns [`SchedulerError::InvalidSpec`] if timestamp conversion or cron
    /// search fails.
    pub fn next_after(&self, after_ms: i64) -> Result<Option<i64>> {
        let utc_dt: DateTime<Utc> = DateTime::from_timestamp_millis(after_ms).ok_or_else(|| {
            SchedulerError::InvalidSpec(format!("timestamp out of range: {after_ms}"))
        })?;
        let local_dt = utc_dt.with_timezone(&self.tz);
        let Ok(first) = self.cron.find_next_occurrence(&local_dt, false) else {
            return Ok(None);
        };
        let first_ms = first.timestamp_millis();
        if first_ms > after_ms {
            return Ok(Some(first_ms));
        }
        // If `after_ms` had sub-minute milliseconds and `find_next_occurrence`
        // landed on the start of the same minute, step past it.
        let Ok(second) = self.cron.find_next_occurrence(&first, false) else {
            return Ok(None);
        };
        Ok(Some(second.timestamp_millis()))
    }
}

fn parse_tz(name: &str) -> Result<Tz> {
    name.parse::<Tz>()
        .map_err(|_| SchedulerError::InvalidSpec(format!("unknown timezone '{name}'")))
}

/// Action executed when a schedule or trigger fires.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Payload {
    /// Send a prompt to the bot's agent loop.
    AgentPrompt {
        /// Prompt text (may reference `{input}` when fired via a webhook).
        text: String,
    },
    /// Replay a deterministic parameterised workflow.
    Workflow {
        /// Workflow name.
        name: String,
        /// Workflow parameters JSON object (may reference `{input}`).
        #[serde(default)]
        params: Value,
    },
    /// Directly invoke a registered tool.
    Tool {
        /// Tool name.
        name: String,
        /// Tool input JSON value (may reference `{input}`).
        #[serde(default)]
        input: Value,
    },
}

impl Payload {
    /// Return a copy of this payload with `{input}` replaced by `body` (used by
    /// webhook triggers).
    #[must_use]
    pub fn with_webhook_input(&self, body: &str) -> Self {
        match self {
            Self::AgentPrompt { text } => Self::AgentPrompt {
                text: text.replace("{input}", body),
            },
            // `input` is only injected through an explicit `{input}` placeholder:
            // most workflows do not declare an `input` parameter and
            // `bind_params` rejects undeclared ones. Null params stay null.
            Self::Workflow { name, params } => Self::Workflow {
                name: name.clone(),
                params: substitute_input_in_value(params, body),
            },
            Self::Tool { name, input } => {
                let rendered = if input.is_null() {
                    Value::String(body.to_owned())
                } else {
                    substitute_input_in_value(input, body)
                };
                Self::Tool {
                    name: name.clone(),
                    input: rendered,
                }
            }
        }
    }
}

fn substitute_input_in_value(v: &Value, body: &str) -> Value {
    match v {
        Value::String(s) => Value::String(s.replace("{input}", body)),
        Value::Array(arr) => Value::Array(
            arr.iter()
                .map(|x| substitute_input_in_value(x, body))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, x)| (k.clone(), substitute_input_in_value(x, body)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// External event trigger attached to a schedule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Trigger {
    /// Poll local filenames and metadata inside explicitly configured roots.
    FileWatch {
        /// Local directory inside an explicitly configured workspace root.
        path: String,
        /// Relative filename glob; no file content is read.
        glob: String,
        /// Changes of interest.
        events: Vec<FileEvent>,
        /// Coalesce a burst for this many milliseconds (at most one minute).
        debounce_ms: u64,
    },
    /// Poll INBOX headers through the registered e-mail connector.
    Email {
        /// Configured account name (credentials remain in the secret store).
        account: String,
        /// Required nonempty sender substring.
        from_filter: String,
        /// Optional subject substring.
        #[serde(default)]
        subject_filter: Option<String>,
    },
    Webhook {
        /// Shared secret used to verify `HMAC-SHA256(secret, body)`.
        secret: String,
    },
}

/// Filesystem events supported by the bounded polling watcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileEvent {
    /// A new directory entry appeared.
    Created,
    /// Metadata changed for an existing file.
    Modified,
}

impl Trigger {
    /// Validate definition bounds, independently of the host's permission policy.
    ///
    /// # Errors
    /// An empty/oversized filter, invalid glob or debounce, or empty secret.
    pub fn validate(&self) -> Result<()> {
        let valid = match self {
            Self::Webhook { secret } => !secret.is_empty(),
            Self::FileWatch {
                path,
                glob,
                events,
                debounce_ms,
            } => {
                !path.trim().is_empty()
                    && path.len() <= 4096
                    && !glob.is_empty()
                    && glob.len() <= 512
                    && globset::Glob::new(glob).is_ok()
                    && !events.is_empty()
                    && events.len() <= 2
                    && *debounce_ms <= 60_000
            }
            Self::Email {
                account,
                from_filter,
                subject_filter,
            } => {
                !account.trim().is_empty()
                    && account.len() <= 256
                    && !from_filter.trim().is_empty()
                    && from_filter.len() <= 256
                    && subject_filter.as_ref().is_none_or(|s| s.len() <= 256)
            }
        };
        if valid {
            Ok(())
        } else {
            Err(SchedulerError::InvalidSpec(
                "invalid trigger definition".into(),
            ))
        }
    }
}

/// A deterministic, read-only condition evaluated before a model run exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Precheck {
    /// First observation establishes a baseline; only changed output runs.
    HashChanged {
        /// Registered Read tool.
        tool: String,
        /// Exact input approved with this definition.
        input: Value,
    },
    /// Capability-free `QuickJS` async function body. All tools must be Read.
    Script {
        /// Full source, displayed in the schedule approval preview.
        code: String,
    },
}

impl Precheck {
    /// Reject oversized definitions before persistence or execution.
    ///
    /// # Errors
    /// Invalid name or definition larger than 64 KiB.
    pub fn validate(&self) -> Result<()> {
        let valid = match self {
            Self::HashChanged { tool, input } => {
                !tool.is_empty() && tool.len() <= 256 && serde_json::to_vec(input)?.len() <= 65_536
            }
            Self::Script { code } => {
                !code.trim().is_empty()
                    && code.len() <= 65_536
                    && !code.chars().any(|c| {
                        protocol::approval_text::is_hidden(c)
                            || (c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
                    })
            }
        };
        if valid {
            Ok(())
        } else {
            Err(SchedulerError::InvalidSpec(
                "invalid precheck definition".into(),
            ))
        }
    }
}

/// Who created a schedule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleOrigin {
    /// The user (UI, CLI or API). Bots may only read these.
    #[default]
    User,
    /// A bot, through its schedule tools (after the user approved it).
    Bot,
}

impl ScheduleOrigin {
    /// Stable lowercase string stored in SQLite.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Bot => "bot",
        }
    }

    /// Parse a stored origin string.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "user" => Some(Self::User),
            "bot" => Some(Self::Bot),
            _ => None,
        }
    }
}

/// Lifecycle state of a persisted schedule occurrence in `occurrences`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OccurrenceState {
    /// Reserved by the timer engine, not yet dispatched.
    Pending,
    /// Claimed for execution by the scheduler.
    Dispatched,
    /// Executor acknowledged a durable run start.
    Started,
    /// Execution completed successfully.
    Succeeded,
    /// Execution returned an error.
    Failed,
    /// Pending occurrence found unresolved at daemon startup.
    Missed,
    /// Dispatched occurrence found unresolved at daemon startup.
    Interrupted,
}

impl OccurrenceState {
    /// Stable lowercase string stored in SQLite.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Dispatched => "dispatched",
            Self::Started => "started",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Missed => "missed",
            Self::Interrupted => "interrupted",
        }
    }

    /// Parse a stored occurrence state string.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "dispatched" => Some(Self::Dispatched),
            "started" => Some(Self::Started),
            "succeeded" => Some(Self::Succeeded),
            "failed" => Some(Self::Failed),
            "missed" => Some(Self::Missed),
            "interrupted" => Some(Self::Interrupted),
            _ => None,
        }
    }

    /// Whether an explicit owner retry may re-dispatch this occurrence.
    #[must_use]
    pub const fn is_retryable(self) -> bool {
        matches!(self, Self::Failed | Self::Missed | Self::Interrupted)
    }
}

/// A persisted timer occurrence record from `occurrences`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Occurrence {
    /// Owning schedule id.
    pub schedule_id: TaskId,
    /// Scheduled due timestamp (Unix epoch milliseconds).
    pub due_ms: i64,
    /// Schedule revision at reservation or retry time.
    pub revision: i64,
    /// Linked agent run id, when recorded by the executor.
    pub run_id: Option<String>,
    /// Current occurrence state.
    pub state: OccurrenceState,
    /// Index of the run attempt in the durable idempotency key.
    pub attempt: u32,
    /// Manual retries done so far.
    pub retry_count: u32,
    /// When the last manual retry was claimed (Unix ms).
    pub last_retry_ms: Option<i64>,
}

/// What the executor knows about the run of one occurrence attempt key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptRun {
    /// No run exists under the key.
    None,
    /// The run ended failed or cancelled; a new attempt may start.
    Failed,
    /// The run is live, paused, or waiting for approval.
    Active,
    /// The run already finished successfully: the side effect happened.
    Succeeded,
}

/// A persisted schedule definition and its runtime state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    /// Unique schedule identifier (`UUIDv7`).
    pub id: TaskId,
    /// Bot profile that executes this schedule.
    pub bot_id: BotId,
    /// Optional chat thread where results should be posted.
    pub chat_id: Option<ChatId>,
    /// Human-readable name.
    pub name: String,
    /// Firing specification (`Cron`, `Every`, or `At`).
    pub spec: ScheduleSpec,
    /// Action executed when triggered (`AgentPrompt`, `Workflow`, or `Tool`).
    pub payload: Payload,
    /// Optional condition evaluated before creating any agent run.
    #[serde(default)]
    pub precheck: Option<Precheck>,
    /// Ephemeral external data. Never persisted as a trusted schedule prompt.
    #[serde(skip)]
    pub external_input: Option<String>,
    /// Fixed failure note, with no upstream errors or source bytes.
    #[serde(skip)]
    pub precheck_failed: bool,
    /// Whether this schedule is active.
    pub enabled: bool,
    /// Creation timestamp (Unix epoch milliseconds).
    pub created_at: i64,
    /// Timestamp of the last occurrence that fired (Unix epoch milliseconds).
    pub last_run_at: Option<i64>,
    /// Timestamp of the next scheduled occurrence (Unix epoch milliseconds).
    pub next_run_at: Option<i64>,
    /// Misfire policy applied after downtime or wall-clock jumps.
    pub misfire_policy: MisfirePolicy,
    /// Internal definition generation. Timer progress does not change it.
    /// This is a persistence/concurrency detail, not part of the schedule DTO.
    #[serde(skip)]
    pub revision: i64,
    /// Who created it (immutable after creation).
    #[serde(default)]
    pub origin: ScheduleOrigin,
    /// Occurrences that were reserved or dispatched but never confirmed
    /// because the process stopped (`missed` / `interrupted`). They are not
    /// replayed automatically; UIs can show them as "missed".
    #[serde(default)]
    pub missed_occurrences: u32,
}

impl Schedule {
    /// Create a new enabled schedule at `now_ms` and compute its initial
    /// `next_run_at`.
    ///
    /// # Errors
    /// Fails if `spec` is invalid.
    pub fn new(
        bot_id: BotId,
        chat_id: Option<ChatId>,
        name: impl Into<String>,
        spec: ScheduleSpec,
        payload: Payload,
        misfire_policy: MisfirePolicy,
        now_ms: i64,
    ) -> Result<Self> {
        spec.validate()?;
        let next_run_at = match &spec {
            ScheduleSpec::At { unix_ms } => Some(*unix_ms),
            _ => spec.next_after(now_ms)?,
        };
        Ok(Self {
            id: TaskId::new(),
            bot_id,
            chat_id,
            name: name.into(),
            spec,
            payload,
            precheck: None,
            external_input: None,
            precheck_failed: false,
            enabled: true,
            created_at: now_ms,
            last_run_at: None,
            next_run_at,
            misfire_policy,
            revision: 0,
            origin: ScheduleOrigin::User,
            missed_occurrences: 0,
        })
    }
}
