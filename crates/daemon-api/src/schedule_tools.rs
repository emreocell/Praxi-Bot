//! The daemon side of the bot schedule tools: a [`ScheduleHost`] over the
//! schedule store, with per-bot scoping, ownership, limits, audit and UI
//! notifications.
//!
//! * A bot may change only schedules a bot-tool call created for it
//!   ([`ScheduleOrigin::Bot`]); user-made schedules are read-only to it.
//! * `preview` resolves a change once (timing, schedule revision) and freezes
//!   it; the later apply stores exactly that, or fails if the row changed.
//! * Limits and the default timezone come from `<data dir>/schedule.toml`
//!   ([`load_schedule_limits`]); a bad file disables the tools (fail closed).

use std::collections::VecDeque;
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;

use builtin_tools::{
    PreviousSchedule, ScheduleCaller, ScheduleChange, ScheduleHost, SchedulePreview,
    ScheduleRequest, ScheduleView,
};
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use protocol::{BotId, ChatId, RunId, TaskId, UserId};
use scheduler::{
    MisfirePolicy, ParsedCron, Payload, Schedule, ScheduleOrigin, ScheduleSpec, ScheduleStore,
    parse_natural_language_with_tz,
};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use storage::{Storage, now_ms};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::dto::{ScheduleChangeKind, ScheduleChanged};

/// File with the schedule-tool limits (`<data dir>/schedule.toml`).
pub const SCHEDULE_FILE: &str = "schedule.toml";

/// Default [`ScheduleLimits::max_per_bot`].
pub const DEFAULT_MAX_SCHEDULES_PER_BOT: usize = 20;
/// Default [`ScheduleLimits::min_interval_secs`] (5 minutes).
pub const DEFAULT_MIN_INTERVAL_SECS: u64 = 300;
/// Largest accepted `max_per_bot`.
pub const MAX_SCHEDULES_CEILING: usize = 1_000;
/// Largest accepted `min_interval_secs` (one week).
pub const MIN_INTERVAL_CEILING_SECS: u64 = 7 * 86_400;
/// Longest prompt a bot may schedule, in characters.
pub const MAX_PROMPT_CHARS: usize = 4_000;
/// Longest schedule name, in characters.
pub const MAX_NAME_CHARS: usize = 80;
/// Longest raw `when` / `cron` / `tz` argument, in characters.
pub const MAX_WHEN_CHARS: usize = 200;
/// See [`MAX_WHEN_CHARS`].
pub const MAX_CRON_CHARS: usize = 120;
/// See [`MAX_WHEN_CHARS`].
pub const MAX_TZ_CHARS: usize = 64;

/// Limits for schedules a bot creates (schedules made in the UI are not
/// limited by the interval, but they count toward the per-bot maximum).
///
/// ```toml
/// max_per_bot = 20
/// min_interval_secs = 300
/// timezone = "Europe/Istanbul"   # default for `when`/`cron`; else the system's
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ScheduleLimits {
    /// Most schedules one bot may have (any origin); `1..=1000`.
    pub max_per_bot: usize,
    /// Shortest allowed gap between firings of a bot-made schedule;
    /// `1..=604800` seconds.
    pub min_interval_secs: u64,
    /// IANA timezone used when a call names none; `None` = the system's,
    /// else UTC.
    pub timezone: Option<String>,
}

impl Default for ScheduleLimits {
    fn default() -> Self {
        Self {
            max_per_bot: DEFAULT_MAX_SCHEDULES_PER_BOT,
            min_interval_secs: DEFAULT_MIN_INTERVAL_SECS,
            timezone: None,
        }
    }
}

impl ScheduleLimits {
    /// Reject values that make no sense (0, absurdly large, unknown timezone).
    ///
    /// # Errors
    /// What is wrong.
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=MAX_SCHEDULES_CEILING).contains(&self.max_per_bot) {
            return Err(format!(
                "max_per_bot must be between 1 and {MAX_SCHEDULES_CEILING}, got {}",
                self.max_per_bot
            ));
        }
        if !(1..=MIN_INTERVAL_CEILING_SECS).contains(&self.min_interval_secs) {
            return Err(format!(
                "min_interval_secs must be between 1 and {MIN_INTERVAL_CEILING_SECS}, got {}",
                self.min_interval_secs
            ));
        }
        if let Some(tz) = &self.timezone
            && Tz::from_str(tz).is_err()
        {
            return Err(format!("unknown timezone {tz:?}"));
        }
        Ok(())
    }
}

/// Read `<data_dir>/schedule.toml`. A missing file is the default.
///
/// # Errors
/// The file exists but cannot be read, parsed or validated. The caller must
/// then NOT enable the schedule tools: falling back to the defaults would
/// silently undo limits the user tightened.
pub fn load_schedule_limits(data_dir: &Path) -> Result<ScheduleLimits, String> {
    let path = data_dir.join(SCHEDULE_FILE);
    if !path.exists() {
        return Ok(ScheduleLimits::default());
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let limits: ScheduleLimits =
        toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    limits
        .validate()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(limits)
}

/// A change resolved at preview time, applied as-is later.
struct Frozen {
    key: String,
    /// `bot|run` that previewed it (per-owner cap).
    owner: String,
    /// Identifies the preview (the approval) this entry was made for.
    nonce: String,
    made_ms: i64,
    name: String,
    prompt: String,
    spec: ScheduleSpec,
    tz: Tz,
    /// The schedule being changed and its revision when previewed.
    target: Option<(TaskId, i64)>,
}

/// Frozen changes are valid this long; at most this many per `bot|run`, and
/// this many overall (a run cannot push out another run's pending approval).
const FROZEN_TTL_MS: i64 = 6 * 3_600_000;
const FROZEN_PER_OWNER: usize = 16;
const FROZEN_MAX: usize = 256;

type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// [`ScheduleHost`] over the daemon's schedule store.
pub(crate) struct DaemonScheduleHost {
    store: Arc<ScheduleStore>,
    storage: Arc<Storage>,
    user_id: UserId,
    events: broadcast::Sender<ScheduleChanged>,
    limits: ScheduleLimits,
    clock: Clock,
    /// Serialises "count, then insert" so the quota cannot be raced.
    gate: parking_lot::Mutex<()>,
    frozen: parking_lot::Mutex<VecDeque<Frozen>>,
}

/// Upper bound on cron occurrences inspected for the minimum interval.
const INTERVAL_PROBES: usize = 48;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn local(ms: i64, tz: Tz) -> String {
    DateTime::<Utc>::from_timestamp_millis(ms)
        .map(|d| d.with_timezone(&tz).to_rfc3339())
        .unwrap_or_default()
}

/// The next `n` firings after `from_ms`.
pub(crate) fn upcoming(spec: &ScheduleSpec, from_ms: i64, n: usize) -> Vec<i64> {
    let mut out = Vec::new();
    let mut at = from_ms;
    while out.len() < n {
        match spec.next_after(at) {
            Ok(Some(next)) => {
                out.push(next);
                at = next;
            }
            _ => break,
        }
    }
    out
}

fn plural(n: u64, unit: &str) -> String {
    if n == 1 {
        format!("{n} {unit}")
    } else {
        format!("{n} {unit}s")
    }
}

/// The 5 cron fields of `raw` without a timezone token.
fn cron_fields(raw: &str) -> Vec<&str> {
    let tokens: Vec<&str> = raw.split_whitespace().collect();
    match tokens.as_slice() {
        [first, rest @ ..] if first.starts_with("CRON_TZ=") || first.starts_with("TZ=") => {
            rest.to_vec()
        }
        [_, _, _, _, _, _] => tokens[..5].to_vec(),
        _ => tokens,
    }
}

fn weekday_name(d: &str) -> Option<&'static str> {
    Some(match d {
        "0" | "7" => "Sunday",
        "1" => "Monday",
        "2" => "Tuesday",
        "3" => "Wednesday",
        "4" => "Thursday",
        "5" => "Friday",
        "6" => "Saturday",
        _ => return None,
    })
}

fn describe_cron(raw: &str) -> String {
    let f = cron_fields(raw);
    if let [m, h, "*", "*", dow] = f.as_slice()
        && let (Ok(m), Ok(h)) = (m.parse::<u8>(), h.parse::<u8>())
        && m < 60
        && h < 24
    {
        let at = format!("{h:02}:{m:02}");
        match *dow {
            "*" => return format!("Every day at {at}"),
            "1-5" => return format!("Every weekday at {at}"),
            "0,6" | "6,0" | "6,7" => return format!("Every weekend day at {at}"),
            d => {
                let names: Option<Vec<&str>> = d.split(',').map(weekday_name).collect();
                if let Some(names) = names {
                    return format!("Every {} at {at}", names.join(", "));
                }
            }
        }
    }
    format!("Cron \"{}\"", f.join(" "))
}

/// Human-readable timing, in `tz` for one-offs.
fn describe(spec: &ScheduleSpec, tz: Tz) -> String {
    match spec {
        ScheduleSpec::Every { seconds } => {
            let s = *seconds;
            let text = if s.is_multiple_of(86_400) {
                plural(s / 86_400, "day")
            } else if s.is_multiple_of(3_600) {
                plural(s / 3_600, "hour")
            } else if s.is_multiple_of(60) {
                plural(s / 60, "minute")
            } else {
                plural(s, "second")
            };
            format!("Every {text}")
        }
        ScheduleSpec::At { unix_ms } => format!("Once, at {}", local(*unix_ms, tz)),
        ScheduleSpec::Cron(raw) => describe_cron(raw),
    }
}

/// The timezone a spec is read in (`fallback` for specs that have none).
fn spec_tz(spec: &ScheduleSpec, fallback: Tz) -> Tz {
    match spec {
        ScheduleSpec::Cron(raw) => ParsedCron::parse(raw).map_or(fallback, |p| p.timezone()),
        _ => fallback,
    }
}

/// Prompt of an agent-prompt schedule; a short note for other payloads.
fn prompt_of(p: &Payload) -> String {
    match p {
        Payload::AgentPrompt { text } => text.clone(),
        Payload::Workflow { name, .. } => format!("(runs workflow {name})"),
        Payload::Tool { name, .. } => format!("(calls tool {name})"),
    }
}

fn human_secs(s: u64) -> String {
    if s >= 3_600 && s.is_multiple_of(3_600) {
        plural(s / 3_600, "hour")
    } else if s >= 60 && s.is_multiple_of(60) {
        plural(s / 60, "minute")
    } else {
        plural(s, "second")
    }
}

/// Characters that hide or disguise text. The stored prompt is run unattended
/// as an instruction, so nothing invisible may ride along: Unicode category
/// Cf (format: zero-width, bidi, soft hyphen, BOM, tag characters ...), Zl, Zp
/// and variation selectors, plus the combining grapheme joiner. Homoglyphs
/// (look-alike letters) are NOT filtered.
fn is_hidden(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{034F}'
            | '\u{0600}'..='\u{0605}'
            | '\u{0890}'
            | '\u{0891}'
            | '\u{115F}'
            | '\u{1160}'
            | '\u{2800}'
            | '\u{3164}'
            | '\u{061C}'
            | '\u{06DD}'
            | '\u{070F}'
            | '\u{08E2}'
            | '\u{180B}'..='\u{180F}'
            | '\u{200B}'..='\u{200F}'
            | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{110BD}'
            | '\u{110CD}'
            | '\u{13430}'..='\u{1343F}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0000}'..='\u{E007F}'
            | '\u{E0100}'..='\u{E01EF}'
    )
}

/// Trim and check a free-text field: not empty, at most `max` characters,
/// no control characters except newline and tab, no bidi controls.
fn clean_text(field: &str, raw: &str, max: usize, allow_newlines: bool) -> Result<String, String> {
    let t = raw.trim();
    if t.is_empty() {
        return Err(format!("`{field}` must not be empty"));
    }
    let n = t.chars().count();
    if n > max {
        return Err(format!("`{field}` is {n} characters; the limit is {max}"));
    }
    for c in t.chars() {
        let bad_control = c.is_control() && !(allow_newlines && (c == '\n' || c == '\t'));
        if matches!(c, '\u{FE0F}' | '\u{200D}' | '\u{200C}') {
            return Err(format!(
                "`{field}` contains an emoji variation selector or a zero-width (non-)joiner (U+{:04X}). Emoji built with variation selectors or joiners, and zero-width joiners in general, are not accepted in schedule prompts and names; use plain text instead",
                u32::from(c)
            ));
        }
        if bad_control || is_hidden(c) {
            return Err(format!(
                "`{field}` contains a control, invisible or direction-override character (U+{:04X}); remove it",
                u32::from(c)
            ));
        }
    }
    Ok(t.to_owned())
}

/// SHA-256 of `text`, hex: a fingerprint of prompts for the audit log.
pub(crate) fn sha256_hex(text: &str) -> String {
    use std::fmt::Write as _;
    Sha256::digest(text.as_bytes())
        .iter()
        .fold(String::new(), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

impl DaemonScheduleHost {
    pub(crate) fn new(
        store: Arc<ScheduleStore>,
        storage: Arc<Storage>,
        user_id: UserId,
        events: broadcast::Sender<ScheduleChanged>,
        limits: ScheduleLimits,
    ) -> Self {
        Self::with_clock(store, storage, user_id, events, limits, Arc::new(now_ms))
    }

    pub(crate) fn with_clock(
        store: Arc<ScheduleStore>,
        storage: Arc<Storage>,
        user_id: UserId,
        events: broadcast::Sender<ScheduleChanged>,
        limits: ScheduleLimits,
        clock: Clock,
    ) -> Self {
        Self {
            store,
            storage,
            user_id,
            events,
            limits,
            clock,
            gate: parking_lot::Mutex::new(()),
            frozen: parking_lot::Mutex::new(VecDeque::new()),
        }
    }

    fn now(&self) -> i64 {
        (self.clock)()
    }

    fn default_tz(&self) -> Tz {
        self.limits
            .timezone
            .as_deref()
            .and_then(|n| Tz::from_str(n).ok())
            .or_else(|| {
                iana_time_zone::get_timezone()
                    .ok()
                    .and_then(|n| Tz::from_str(&n).ok())
            })
            .unwrap_or(chrono_tz::UTC)
    }

    fn bot(caller: &ScheduleCaller) -> Result<BotId, String> {
        Uuid::parse_str(&caller.bot_id)
            .map(BotId)
            .map_err(|_| "schedule tools need a valid calling bot".to_owned())
    }

    /// The caller's own schedule `id` (read access); another bot's schedule is
    /// "not found".
    fn own(&self, caller: &ScheduleCaller, id: &str) -> Result<Schedule, String> {
        let bot = Self::bot(caller)?;
        let not_found = || format!("no schedule with id {id:?}; call schedule_list to see yours");
        let tid = Uuid::parse_str(id.trim())
            .map(TaskId)
            .map_err(|_| not_found())?;
        self.store
            .get(tid)
            .map_err(err)?
            .filter(|s| s.bot_id == bot)
            .ok_or_else(not_found)
    }

    /// The caller's own schedule that a bot made (write access).
    fn own_editable(&self, caller: &ScheduleCaller, id: &str) -> Result<Schedule, String> {
        let s = self.own(caller, id)?;
        if s.origin != ScheduleOrigin::Bot {
            return Err(format!(
                "schedule {:?} was created by the user, so you can only read it; ask the user to change it",
                s.name
            ));
        }
        Ok(s)
    }

    /// A webhook trigger lets outsiders fire the schedule, so a bot may not
    /// edit or resume it (it may still pause or delete it).
    fn refuse_if_webhook(&self, s: &Schedule) -> Result<(), String> {
        if self.store.get_trigger(s.id).map_err(err)?.is_some() {
            return Err(format!(
                "schedule {:?} has a webhook trigger, so only the user can edit or resume it",
                s.name
            ));
        }
        Ok(())
    }

    pub(crate) fn check_interval(&self, spec: &ScheduleSpec, now: i64) -> Result<(), String> {
        let min = self.limits.min_interval_secs;
        let too_often = |gap_secs: u64| {
            format!(
                "that schedule fires every {} but the minimum interval is {}; choose a less frequent one",
                human_secs(gap_secs),
                human_secs(min)
            )
        };
        match spec {
            ScheduleSpec::Every { seconds } if *seconds < min => Err(too_often(*seconds)),
            ScheduleSpec::Cron(_) => {
                let times = upcoming(spec, now, INTERVAL_PROBES);
                let gap = times
                    .windows(2)
                    .map(|w| w[1] - w[0])
                    .min()
                    .and_then(|g| u64::try_from(g / 1000).ok());
                match gap {
                    Some(g) if g < min => Err(too_often(g)),
                    _ => Ok(()),
                }
            }
            _ => Ok(()),
        }
    }

    /// `when` / `cron` / `tz` of `req` as a validated spec.
    fn spec_of(&self, req: &ScheduleRequest, now: i64) -> Result<(ScheduleSpec, Tz), String> {
        for (field, v, max) in [
            ("when", &req.when, MAX_WHEN_CHARS),
            ("cron", &req.cron, MAX_CRON_CHARS),
            ("tz", &req.tz, MAX_TZ_CHARS),
        ] {
            if let Some(v) = v {
                let n = v.chars().count();
                if n > max {
                    return Err(format!("`{field}` is {n} characters; the limit is {max}"));
                }
            }
        }
        let tz = match req.tz.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
            Some(n) => Tz::from_str(n).map_err(|_| {
                format!("unknown timezone {n:?} (use an IANA name such as \"Europe/Istanbul\")")
            })?,
            None => self.default_tz(),
        };
        let when = req.when.as_deref().map(str::trim).filter(|w| !w.is_empty());
        let cron = req.cron.as_deref().map(str::trim).filter(|w| !w.is_empty());
        let spec = match (when, cron) {
            (Some(_), Some(_)) => return Err("give either `when` or `cron`, not both".into()),
            (None, None) => {
                return Err(
                    "give `when` (for example \"every weekday at 10:30\") or `cron`".into(),
                );
            }
            (Some(w), None) => {
                parse_natural_language_with_tz(w, now, Some(tz.name())).ok_or_else(|| {
                    format!(
                        "cannot understand {w:?}; try \"every weekday at 10:30\", \"every 15 minutes\" or \"in 2 hours\", or pass `cron`"
                    )
                })?
            }
            (None, Some(c)) => {
                if c.split_whitespace().count() == 5 {
                    ScheduleSpec::cron_tz(c, tz.name())
                } else {
                    ScheduleSpec::cron(c)
                }
            }
        };
        spec.validate().map_err(err)?;
        let tz = spec_tz(&spec, tz);
        self.check_interval(&spec, now)?;
        if let ScheduleSpec::At { unix_ms } = spec
            && unix_ms <= now
        {
            return Err("that time is already in the past".into());
        }
        Ok((spec, tz))
    }

    fn quota(&self, bot: BotId) -> Result<(), String> {
        let n = self.store.list_by_bot(bot).map_err(err)?.len();
        if n >= self.limits.max_per_bot {
            return Err(format!(
                "schedule limit reached: this bot already has {n} of {} allowed; delete one first",
                self.limits.max_per_bot
            ));
        }
        Ok(())
    }

    fn owner(caller: &ScheduleCaller) -> String {
        format!("{}|{}", caller.bot_id, caller.run_id)
    }

    fn key(caller: &ScheduleCaller, change: &ScheduleChange) -> String {
        format!(
            "{}|{}|{}",
            caller.bot_id,
            caller.run_id,
            serde_json::to_string(change).unwrap_or_default()
        )
    }

    /// Remember `f` for the approval about to be shown.
    ///
    /// * An unexpired entry for the same call is still waiting for its
    ///   approval: the second preview is refused (replacing it would let card A
    ///   store card B's time). Entries of closed approvals are gone already.
    /// * Caps only ever evict the requester's OWN oldest entry; when the
    ///   overall cap is reached and the requester has none, the preview is
    ///   refused rather than pushing out another run's pending approval.
    fn freeze(&self, f: Frozen) -> Result<(), String> {
        let now = self.now();
        let mut q = self.frozen.lock();
        q.retain(|x| now - x.made_ms < FROZEN_TTL_MS);
        if q.iter().any(|x| x.key == f.key) {
            return Err(
                "an identical approval is already waiting; answer or dismiss it first".into(),
            );
        }
        let evict_own_oldest = |q: &mut VecDeque<Frozen>| -> bool {
            q.iter()
                .position(|x| x.owner == f.owner)
                .is_some_and(|i| q.remove(i).is_some())
        };
        while q.iter().filter(|x| x.owner == f.owner).count() >= FROZEN_PER_OWNER {
            evict_own_oldest(&mut q);
        }
        while q.len() >= FROZEN_MAX {
            if !evict_own_oldest(&mut q) {
                return Err(
                    "too many approvals are waiting right now; try again after some are answered"
                        .into(),
                );
            }
        }
        q.push_back(f);
        Ok(())
    }

    /// The current frozen change for exactly this call, removed; `None` when
    /// there is none or it is past its time-to-live.
    fn take(&self, key: &str) -> Option<Frozen> {
        let now = self.now();
        let mut q = self.frozen.lock();
        let i = q.iter().position(|f| f.key == key)?;
        let f = q.remove(i)?;
        (now - f.made_ms < FROZEN_TTL_MS).then_some(f)
    }

    /// Resolve `change` against the current state (no side effects).
    fn resolve(&self, caller: &ScheduleCaller, change: &ScheduleChange) -> Result<Frozen, String> {
        let now = self.now();
        let key = Self::key(caller, change);
        match change {
            ScheduleChange::Create(req) => {
                let bot = Self::bot(caller)?;
                let name = clean_text(
                    "name",
                    req.name.as_deref().unwrap_or_default(),
                    MAX_NAME_CHARS,
                    false,
                )?;
                let prompt = clean_text(
                    "prompt",
                    req.prompt.as_deref().unwrap_or_default(),
                    MAX_PROMPT_CHARS,
                    true,
                )?;
                self.quota(bot)?;
                let (spec, tz) = self.spec_of(req, now)?;
                Ok(Frozen {
                    key,
                    owner: Self::owner(caller),
                    nonce: Uuid::now_v7().to_string(),
                    made_ms: now,
                    name,
                    prompt,
                    spec,
                    tz,
                    target: None,
                })
            }
            ScheduleChange::Update { id, req } => {
                let cur = self.own_editable(caller, id)?;
                self.refuse_if_webhook(&cur)?;
                let Payload::AgentPrompt { text } = &cur.payload else {
                    return Err(
                        "this schedule runs a workflow or tool; a bot can only pause or delete it"
                            .into(),
                    );
                };
                let name = match req.name.as_deref() {
                    Some(n) => clean_text("name", n, MAX_NAME_CHARS, false)?,
                    None => cur.name.clone(),
                };
                let prompt = match req.prompt.as_deref() {
                    Some(p) => clean_text("prompt", p, MAX_PROMPT_CHARS, true)?,
                    None => text.clone(),
                };
                let retimed = req.when.is_some() || req.cron.is_some();
                if !retimed && req.tz.is_some() {
                    return Err("`tz` only applies together with `when` or `cron`".into());
                }
                let (spec, tz) = if retimed {
                    self.spec_of(req, now)?
                } else {
                    (cur.spec.clone(), spec_tz(&cur.spec, self.default_tz()))
                };
                Ok(Frozen {
                    key,
                    owner: Self::owner(caller),
                    nonce: Uuid::now_v7().to_string(),
                    made_ms: now,
                    name,
                    prompt,
                    spec,
                    tz,
                    target: Some((cur.id, cur.revision)),
                })
            }
            ScheduleChange::Resume { id } | ScheduleChange::Pause { id } => {
                let cur = self.own_editable(caller, id)?;
                let resume = matches!(change, ScheduleChange::Resume { .. });
                if resume {
                    self.refuse_if_webhook(&cur)?;
                    // The limit applies when re-arming too.
                    self.check_interval(&cur.spec, now)?;
                }
                Ok(self.same_as(key, Self::owner(caller), &cur, now))
            }
            ScheduleChange::Delete { id } => {
                let cur = self.own_editable(caller, id)?;
                Ok(self.same_as(key, Self::owner(caller), &cur, now))
            }
        }
    }

    /// A frozen change that keeps the stored definition as it is.
    fn same_as(&self, key: String, owner: String, cur: &Schedule, now: i64) -> Frozen {
        Frozen {
            key,
            owner,
            nonce: Uuid::now_v7().to_string(),
            made_ms: now,
            name: cur.name.clone(),
            prompt: prompt_of(&cur.payload),
            spec: cur.spec.clone(),
            tz: spec_tz(&cur.spec, self.default_tz()),
            target: Some((cur.id, cur.revision)),
        }
    }

    fn view(&self, s: &Schedule) -> ScheduleView {
        let tz = spec_tz(&s.spec, self.default_tz());
        let now = self.now();
        ScheduleView {
            id: s.id.0.to_string(),
            name: s.name.clone(),
            prompt: prompt_of(&s.payload),
            enabled: s.enabled,
            schedule: describe(&s.spec, tz),
            timezone: tz.name().to_owned(),
            next_runs: if s.enabled {
                upcoming(&s.spec, now, 3)
                    .into_iter()
                    .map(|t| local(t, tz))
                    .collect()
            } else {
                Vec::new()
            },
            last_run: s.last_run_at.map(|t| local(t, tz)),
            origin: s.origin.as_str().to_owned(),
            editable: s.origin == ScheduleOrigin::Bot,
        }
    }

    fn preview_of(
        &self,
        action: &str,
        f: &Frozen,
        before: Option<&Schedule>,
        id: Option<TaskId>,
    ) -> SchedulePreview {
        let now = self.now();
        let drop_prompt = matches!(action, "pause" | "delete");
        let prompt = if drop_prompt {
            String::new()
        } else {
            f.prompt.clone()
        };
        SchedulePreview {
            action: action.to_owned(),
            schedule_id: id.map(|i| i.0.to_string()),
            name: f.name.clone(),
            prompt_chars: prompt.chars().count(),
            prompt,
            schedule: describe(&f.spec, f.tz),
            timezone: f.tz.name().to_owned(),
            next_runs: if drop_prompt {
                Vec::new()
            } else {
                upcoming(&f.spec, now, 3)
                    .into_iter()
                    .map(|t| local(t, f.tz))
                    .collect()
            },
            approval_ref: String::new(),
            previous: before.filter(|_| action == "update").map(|b| {
                let tz = spec_tz(&b.spec, self.default_tz());
                PreviousSchedule {
                    name: b.name.clone(),
                    prompt: prompt_of(&b.payload),
                    schedule: describe(&b.spec, tz),
                    timezone: tz.name().to_owned(),
                }
            }),
        }
    }

    fn notify(&self, id: TaskId, kind: ScheduleChangeKind) {
        // No subscriber is not an error.
        let _ = self.events.send(ScheduleChanged::new(id.0, kind));
    }

    /// Write the audit entry; a failure fails the operation (this runs before
    /// the change is committed).
    fn audit(
        &self,
        caller: &ScheduleCaller,
        action: &str,
        change: &str,
        s: &Schedule,
        old: Option<&Schedule>,
    ) -> Result<(), String> {
        let run = Uuid::parse_str(&caller.run_id).ok().map(RunId);
        let tz = spec_tz(&s.spec, self.default_tz());
        let name: String = s.name.chars().take(MAX_NAME_CHARS).collect();
        let mut detail = json!({
            "bot_id": caller.bot_id,
            "run_id": caller.run_id,
            "schedule_id": s.id.0.to_string(),
            "name": name,
            "change": change,
            "schedule": describe(&s.spec, tz),
            "prompt_chars": prompt_of(&s.payload).chars().count(),
            "prompt_sha256": sha256_hex(&prompt_of(&s.payload)),
        });
        if let (Some(old), Some(o)) = (old, detail.as_object_mut()) {
            let p = prompt_of(&old.payload);
            o.insert("old_prompt_chars".into(), json!(p.chars().count()));
            o.insert("old_prompt_sha256".into(), json!(sha256_hex(&p)));
            o.insert(
                "old_name".into(),
                json!(old.name.chars().take(MAX_NAME_CHARS).collect::<String>()),
            );
            o.insert(
                "old_schedule".into(),
                json!(describe(&old.spec, spec_tz(&old.spec, self.default_tz()))),
            );
        }
        self.storage
            .append_audit(self.user_id, run, "bot", action, &detail)
            .map(|_| ())
            .map_err(|e| format!("the audit log could not be written, so nothing was changed: {e}"))
    }

    /// The audit entry written before a change turned out to be wrong (the
    /// change failed): say so, so the log never claims something that did not
    /// happen. The audit and schedule stores are different databases, so this
    /// follow-up entry replaces a shared transaction.
    pub(crate) fn audit_failed(
        &self,
        caller: &ScheduleCaller,
        action: &str,
        change: &str,
        s: &Schedule,
        error: &str,
    ) {
        let run = Uuid::parse_str(&caller.run_id).ok().map(RunId);
        let detail = json!({
            "bot_id": caller.bot_id,
            "run_id": caller.run_id,
            "schedule_id": s.id.0.to_string(),
            "attempted": action,
            "change": change,
            "error": error.chars().take(300).collect::<String>(),
        });
        if let Err(e) =
            self.storage
                .append_audit(self.user_id, run, "bot", "schedule.failed", &detail)
        {
            eprintln!(
                "daemon-api: ERROR: could not record that schedule change {change} failed ({error}): {e}"
            );
        }
    }

    /// The chat of the calling run (the schedule posts there).
    fn chat_of(&self, caller: &ScheduleCaller, bot: BotId) -> Option<ChatId> {
        let run = Uuid::parse_str(&caller.run_id).ok()?;
        let run = self.storage.get_run(RunId(run)).ok().flatten()?;
        let chat = self.storage.get_chat(run.chat_id?).ok().flatten()?;
        (chat.bot_id == bot && chat.user_id == self.user_id).then_some(chat.id)
    }

    /// The frozen change for this call, if any; otherwise (only for changes
    /// that reduce authority: pause, delete) resolved now.
    fn frozen_or_fresh(
        &self,
        caller: &ScheduleCaller,
        change: &ScheduleChange,
        may_resolve_now: bool,
    ) -> Result<Frozen, String> {
        match self.take(&Self::key(caller, change)) {
            Some(f) => Ok(f),
            None if may_resolve_now => self.resolve(caller, change),
            None => {
                Err("this change was not previewed for approval (or the approval is too old), so it was not applied".to_owned())
            }
        }
    }

    /// The stored row must still be the one that was approved.
    fn current_matches(&self, caller: &ScheduleCaller, f: &Frozen) -> Result<Schedule, String> {
        let (id, rev) = f.target.ok_or("internal: no target")?;
        let changed = || {
            "that schedule changed or was deleted after the approval was requested; nothing was applied. Call schedule_list and ask again"
                .to_owned()
        };
        let cur = self
            .own_editable(caller, &id.0.to_string())
            .map_err(|_| changed())?;
        if cur.revision != rev {
            return Err(changed());
        }
        Ok(cur)
    }

    /// Re-check, audit, then commit a frozen update / pause / resume.
    fn apply_edit(
        &self,
        caller: &ScheduleCaller,
        f: &Frozen,
        enabled: Option<bool>,
        label: &str,
    ) -> Result<Schedule, String> {
        let now = self.now();
        let cur = self.current_matches(caller, f)?;
        if label != "paused" {
            self.refuse_if_webhook(&cur)?;
        }
        let mut next = cur.clone();
        next.name.clone_from(&f.name);
        if matches!(cur.payload, Payload::AgentPrompt { .. }) {
            next.payload = Payload::AgentPrompt {
                text: f.prompt.clone(),
            };
        }
        next.spec = f.spec.clone();
        if let Some(e) = enabled {
            next.enabled = e;
        }
        if let ScheduleSpec::At { unix_ms } = next.spec
            && next.enabled
            && unix_ms <= now
        {
            return Err(format!(
                "the one-time moment {} has already passed; nothing was applied",
                local(unix_ms, f.tz)
            ));
        }
        self.audit(caller, "schedule.update", label, &next, Some(&cur))?;
        let applied = self.store.update_definition(&next).map_err(err).and_then(|ok| {
            if ok {
                Ok(())
            } else {
                Err("that schedule changed or was deleted while it was being applied; nothing was applied".to_owned())
            }
        });
        if let Err(e) = applied {
            self.audit_failed(caller, "schedule.update", label, &next, &e);
            return Err(e);
        }
        let after = self
            .store
            .get(cur.id)
            .map_err(err)?
            .ok_or("that schedule was deleted meanwhile")?;
        self.notify(
            cur.id,
            if cur.enabled == after.enabled {
                ScheduleChangeKind::Updated
            } else {
                ScheduleChangeKind::Toggled
            },
        );
        Ok(after)
    }
}

impl ScheduleHost for DaemonScheduleHost {
    fn list(&self, caller: &ScheduleCaller) -> Result<Vec<ScheduleView>, String> {
        let bot = Self::bot(caller)?;
        Ok(self
            .store
            .list_by_bot(bot)
            .map_err(err)?
            .iter()
            .map(|s| self.view(s))
            .collect())
    }

    fn preview(
        &self,
        caller: &ScheduleCaller,
        change: &ScheduleChange,
    ) -> Result<SchedulePreview, String> {
        let f = self.resolve(caller, change)?;
        let (action, before, id) = match change {
            ScheduleChange::Create(_) => ("create", None, None),
            ScheduleChange::Update { id, .. } => {
                let b = self.own(caller, id)?;
                let tid = b.id;
                ("update", Some(b), Some(tid))
            }
            ScheduleChange::Resume { id } => ("resume", None, Some(self.own(caller, id)?.id)),
            ScheduleChange::Pause { id } => ("pause", None, Some(self.own(caller, id)?.id)),
            ScheduleChange::Delete { id } => ("delete", None, Some(self.own(caller, id)?.id)),
        };
        let mut p = self.preview_of(action, &f, before.as_ref(), id);
        p.approval_ref.clone_from(&f.nonce);
        self.freeze(f)?;
        Ok(p)
    }

    fn discard(&self, caller: &ScheduleCaller, change: &ScheduleChange, approval_ref: &str) {
        let key = Self::key(caller, change);
        self.frozen
            .lock()
            .retain(|f| !(f.key == key && f.nonce == approval_ref));
    }

    fn create(
        &self,
        caller: &ScheduleCaller,
        req: &ScheduleRequest,
    ) -> Result<ScheduleView, String> {
        let bot = Self::bot(caller)?;
        let f = self.frozen_or_fresh(caller, &ScheduleChange::Create(req.clone()), false)?;
        let now = self.now();
        let _gate = self.gate.lock();
        self.quota(bot)?;
        if let ScheduleSpec::At { unix_ms } = f.spec
            && unix_ms <= now
        {
            return Err(format!(
                "the one-time moment {} has already passed; nothing was created",
                local(unix_ms, f.tz)
            ));
        }
        let mut sc = Schedule::new(
            bot,
            self.chat_of(caller, bot),
            f.name,
            f.spec,
            Payload::AgentPrompt { text: f.prompt },
            MisfirePolicy::default(),
            now,
        )
        .map_err(err)?;
        sc.origin = ScheduleOrigin::Bot;
        self.audit(caller, "schedule.create", "created", &sc, None)?;
        if let Err(e) = self.store.insert(&sc).map_err(err) {
            self.audit_failed(caller, "schedule.create", "created", &sc, &e);
            return Err(e);
        }
        self.notify(sc.id, ScheduleChangeKind::Created);
        Ok(self.view(&sc))
    }

    fn update(
        &self,
        caller: &ScheduleCaller,
        id: &str,
        req: &ScheduleRequest,
    ) -> Result<ScheduleView, String> {
        let change = ScheduleChange::Update {
            id: id.to_owned(),
            req: req.clone(),
        };
        let f = self.frozen_or_fresh(caller, &change, false)?;
        let after = self.apply_edit(caller, &f, None, "updated")?;
        Ok(self.view(&after))
    }

    fn set_enabled(
        &self,
        caller: &ScheduleCaller,
        id: &str,
        enabled: bool,
    ) -> Result<ScheduleView, String> {
        let change = if enabled {
            ScheduleChange::Resume { id: id.to_owned() }
        } else {
            ScheduleChange::Pause { id: id.to_owned() }
        };
        let f = self.frozen_or_fresh(caller, &change, !enabled)?;
        let label = if enabled { "resumed" } else { "paused" };
        let after = self.apply_edit(caller, &f, Some(enabled), label)?;
        Ok(self.view(&after))
    }

    fn delete(&self, caller: &ScheduleCaller, id: &str) -> Result<ScheduleView, String> {
        let change = ScheduleChange::Delete { id: id.to_owned() };
        let f = self.frozen_or_fresh(caller, &change, true)?;
        let cur = self.current_matches(caller, &f)?;
        let view = self.view(&cur);
        self.audit(caller, "schedule.delete", "deleted", &cur, None)?;
        // One statement: the webhook trigger goes with the schedule (cascade),
        // so nothing is removed unless the whole delete succeeds.
        let deleted = self
            .store
            .delete_exact(cur.id, cur.revision, ScheduleOrigin::Bot)
            .map_err(err)
            .and_then(|ok| {
                if ok {
                    Ok(())
                } else {
                    Err("that schedule changed or was deleted after the approval was requested; nothing was deleted".to_owned())
                }
            });
        if let Err(e) = deleted {
            self.audit_failed(caller, "schedule.delete", "deleted", &cur, &e);
            return Err(e);
        }
        self.notify(cur.id, ScheduleChangeKind::Deleted);
        Ok(view)
    }
}
