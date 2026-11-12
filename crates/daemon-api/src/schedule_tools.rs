//! The daemon side of the bot schedule tools: a [`ScheduleHost`] over the
//! schedule store, with per-bot scoping, limits, audit and UI notifications.
//!
//! Limits and the default timezone come from `<data dir>/schedule.toml`
//! ([`load_schedule_limits`]); every key is optional.

use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;

use builtin_tools::{
    ScheduleCaller, ScheduleChange, ScheduleHost, SchedulePreview, ScheduleRequest, ScheduleView,
};
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use protocol::{BotId, ChatId, RunId, TaskId, UserId};
use scheduler::{
    MisfirePolicy, ParsedCron, Payload, Schedule, ScheduleSpec, ScheduleStore,
    parse_natural_language_with_tz,
};
use serde::Deserialize;
use serde_json::json;
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
    /// Most schedules one bot may have (any origin).
    pub max_per_bot: usize,
    /// Shortest allowed gap between firings of a bot-made schedule.
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

/// Read `<data_dir>/schedule.toml`. A missing file is the default; a bad one
/// yields the default plus a warning (it never stops the daemon).
#[must_use]
pub fn load_schedule_limits(data_dir: &Path) -> (ScheduleLimits, Vec<String>) {
    let path = data_dir.join(SCHEDULE_FILE);
    if !path.exists() {
        return (ScheduleLimits::default(), Vec::new());
    }
    let parsed = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))
        .and_then(|s| {
            toml::from_str::<ScheduleLimits>(&s).map_err(|e| format!("{}: {e}", path.display()))
        })
        .and_then(|l| match l.timezone.as_deref().map(Tz::from_str) {
            Some(Err(_)) => Err(format!(
                "{}: unknown timezone {:?}",
                path.display(),
                l.timezone.unwrap_or_default()
            )),
            _ => Ok(l),
        });
    match parsed {
        Ok(l) => (l, Vec::new()),
        Err(e) => (
            ScheduleLimits::default(),
            vec![format!("{e}; using the default schedule limits")],
        ),
    }
}

/// [`ScheduleHost`] over the daemon's schedule store.
pub(crate) struct DaemonScheduleHost {
    store: Arc<ScheduleStore>,
    storage: Arc<Storage>,
    user_id: UserId,
    events: broadcast::Sender<ScheduleChanged>,
    limits: ScheduleLimits,
    /// Serialises "count, then insert" so the quota cannot be raced.
    gate: parking_lot::Mutex<()>,
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
fn upcoming(spec: &ScheduleSpec, from_ms: i64, n: usize) -> Vec<i64> {
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

/// A validated, not yet stored definition.
struct Plan {
    name: String,
    prompt: String,
    spec: ScheduleSpec,
    tz: Tz,
}

impl DaemonScheduleHost {
    pub(crate) fn new(
        store: Arc<ScheduleStore>,
        storage: Arc<Storage>,
        user_id: UserId,
        events: broadcast::Sender<ScheduleChanged>,
        limits: ScheduleLimits,
    ) -> Self {
        Self {
            store,
            storage,
            user_id,
            events,
            limits,
            gate: parking_lot::Mutex::new(()),
        }
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

    /// The caller's own schedule `id`; another bot's schedule is "not found".
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

    fn check_interval(&self, spec: &ScheduleSpec, now: i64) -> Result<(), String> {
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
            (None, None) => return Err("give `when` (for example \"every weekday at 10:30\") or `cron`".into()),
            (Some(w), None) => parse_natural_language_with_tz(w, now, Some(tz.name())).ok_or_else(|| {
                format!("cannot understand {w:?}; try \"every weekday at 10:30\", \"every 15 minutes\" or \"in 2 hours\", or pass `cron`")
            })?,
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

    fn plan_create(
        &self,
        caller: &ScheduleCaller,
        req: &ScheduleRequest,
        now: i64,
    ) -> Result<Plan, String> {
        let bot = Self::bot(caller)?;
        let name = req.name.as_deref().map(str::trim).unwrap_or_default();
        let prompt = req.prompt.as_deref().map(str::trim).unwrap_or_default();
        if name.is_empty() || prompt.is_empty() {
            return Err("`name` and `prompt` must not be empty".into());
        }
        self.quota(bot)?;
        let (spec, tz) = self.spec_of(req, now)?;
        Ok(Plan {
            name: name.to_owned(),
            prompt: prompt.to_owned(),
            spec,
            tz,
        })
    }

    /// The stored schedule with `req` applied.
    fn plan_update(
        &self,
        current: &Schedule,
        req: &ScheduleRequest,
        now: i64,
    ) -> Result<Plan, String> {
        let Payload::AgentPrompt { text } = &current.payload else {
            return Err(
                "this schedule runs a workflow or tool; a bot can only pause or delete it".into(),
            );
        };
        let name = match req.name.as_deref().map(str::trim) {
            Some("") => return Err("`name` must not be empty".into()),
            Some(n) => n.to_owned(),
            None => current.name.clone(),
        };
        let prompt = match req.prompt.as_deref().map(str::trim) {
            Some("") => return Err("`prompt` must not be empty".into()),
            Some(p) => p.to_owned(),
            None => text.clone(),
        };
        let retimed = req.when.is_some() || req.cron.is_some();
        if !retimed && req.tz.is_some() {
            return Err("`tz` only applies together with `when` or `cron`".into());
        }
        let (spec, tz) = if retimed {
            self.spec_of(req, now)?
        } else {
            let s = current.spec.clone();
            let tz = spec_tz(&s, self.default_tz());
            (s, tz)
        };
        Ok(Plan {
            name,
            prompt,
            spec,
            tz,
        })
    }

    fn view(&self, s: &Schedule) -> ScheduleView {
        let tz = spec_tz(&s.spec, self.default_tz());
        let now = now_ms();
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
        }
    }

    fn preview_of(action: &str, p: &Plan, now: i64) -> SchedulePreview {
        SchedulePreview {
            action: action.to_owned(),
            name: p.name.clone(),
            prompt: p.prompt.clone(),
            schedule: describe(&p.spec, p.tz),
            timezone: p.tz.name().to_owned(),
            next_runs: upcoming(&p.spec, now, 3)
                .into_iter()
                .map(|t| local(t, p.tz))
                .collect(),
        }
    }

    fn notify(&self, id: TaskId, kind: ScheduleChangeKind) {
        // No subscriber is not an error.
        let _ = self.events.send(ScheduleChanged::new(id.0, kind));
    }

    fn audit(&self, caller: &ScheduleCaller, action: &str, s: &Schedule, change: &str) {
        let run = Uuid::parse_str(&caller.run_id).ok().map(RunId);
        let tz = spec_tz(&s.spec, self.default_tz());
        let name: String = s.name.chars().take(80).collect();
        let _ = self.storage.append_audit(
            self.user_id,
            run,
            "bot",
            action,
            &json!({
                "bot_id": caller.bot_id,
                "run_id": caller.run_id,
                "schedule_id": s.id.0.to_string(),
                "name": name,
                "change": change,
                "schedule": describe(&s.spec, tz),
                "prompt_chars": prompt_of(&s.payload).chars().count(),
            }),
        );
    }

    /// The chat of the calling run (the schedule posts there).
    fn chat_of(&self, caller: &ScheduleCaller, bot: BotId) -> Option<ChatId> {
        let run = Uuid::parse_str(&caller.run_id).ok()?;
        let run = self.storage.get_run(RunId(run)).ok().flatten()?;
        let chat = self.storage.get_chat(run.chat_id?).ok().flatten()?;
        (chat.bot_id == bot && chat.user_id == self.user_id).then_some(chat.id)
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
        let now = now_ms();
        match change {
            ScheduleChange::Create(req) => {
                let p = self.plan_create(caller, req, now)?;
                Ok(Self::preview_of("create", &p, now))
            }
            ScheduleChange::Update { id, req } => {
                let cur = self.own(caller, id)?;
                let p = self.plan_update(&cur, req, now)?;
                Ok(Self::preview_of("update", &p, now))
            }
            ScheduleChange::Resume { id } => {
                let cur = self.own(caller, id)?;
                let tz = spec_tz(&cur.spec, self.default_tz());
                Ok(Self::preview_of(
                    "resume",
                    &Plan {
                        name: cur.name.clone(),
                        prompt: prompt_of(&cur.payload),
                        spec: cur.spec.clone(),
                        tz,
                    },
                    now,
                ))
            }
        }
    }

    fn create(
        &self,
        caller: &ScheduleCaller,
        req: &ScheduleRequest,
    ) -> Result<ScheduleView, String> {
        let bot = Self::bot(caller)?;
        let now = now_ms();
        let _gate = self.gate.lock();
        let p = self.plan_create(caller, req, now)?;
        let sc = Schedule::new(
            bot,
            self.chat_of(caller, bot),
            p.name,
            p.spec,
            Payload::AgentPrompt { text: p.prompt },
            MisfirePolicy::default(),
            now,
        )
        .map_err(err)?;
        self.store.insert(&sc).map_err(err)?;
        self.notify(sc.id, ScheduleChangeKind::Created);
        self.audit(caller, "schedule.create", &sc, "created");
        Ok(self.view(&sc))
    }

    fn update(
        &self,
        caller: &ScheduleCaller,
        id: &str,
        req: &ScheduleRequest,
    ) -> Result<ScheduleView, String> {
        let now = now_ms();
        let mut sc = self.own(caller, id)?;
        let p = self.plan_update(&sc, req, now)?;
        sc.name = p.name;
        sc.payload = Payload::AgentPrompt { text: p.prompt };
        sc.spec = p.spec;
        let (sc, _) = self
            .store
            .update_definition_and_rearm(&sc, None, now)
            .map_err(err)?
            .ok_or_else(|| "that schedule was deleted meanwhile".to_owned())?;
        self.notify(sc.id, ScheduleChangeKind::Updated);
        self.audit(caller, "schedule.update", &sc, "updated");
        Ok(self.view(&sc))
    }

    fn set_enabled(
        &self,
        caller: &ScheduleCaller,
        id: &str,
        enabled: bool,
    ) -> Result<ScheduleView, String> {
        let now = now_ms();
        let sc = self.own(caller, id)?;
        let (sc, was) = self
            .store
            .update_definition_and_rearm(&sc, Some(enabled), now)
            .map_err(err)?
            .ok_or_else(|| "that schedule was deleted meanwhile".to_owned())?;
        self.notify(
            sc.id,
            if was == sc.enabled {
                ScheduleChangeKind::Updated
            } else {
                ScheduleChangeKind::Toggled
            },
        );
        self.audit(
            caller,
            "schedule.update",
            &sc,
            if enabled { "resumed" } else { "paused" },
        );
        Ok(self.view(&sc))
    }

    fn delete(&self, caller: &ScheduleCaller, id: &str) -> Result<ScheduleView, String> {
        let sc = self.own(caller, id)?;
        let view = self.view(&sc);
        self.store.remove_trigger(sc.id).map_err(err)?;
        self.store.delete(sc.id).map_err(err)?;
        self.notify(sc.id, ScheduleChangeKind::Deleted);
        self.audit(caller, "schedule.delete", &sc, "deleted");
        Ok(view)
    }
}
