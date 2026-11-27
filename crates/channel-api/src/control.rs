//! Phone control of missions: the commands the owner types in a bridge (`/missions`,
//! `/mission <id>`, `/pause`, `/resume`, `/stop`, `/mission new`), the way an id is read, and
//! the wording of every answer, in English and Turkish, with the same text on every channel.
//!
//! Nothing here talks to the daemon or authorizes anything: the bridge only calls it for the
//! paired owner's own message in a private chat (never a forward, never a group). What a mission,
//! a bot or a web page wrote is clipped and made inert for the channel (see
//! [`crate::notice::Flavor`]); everything else is the channel's own wording.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::notice::{Flavor, NoticeLang, clip, inert, pick, quoted};

/// Characters of a short mission id (the tail of the UUID; its head is a timestamp, so two
/// missions started within a minute share it).
pub const SHORT_ID_CHARS: usize = 8;
/// Fewest characters of an id the owner may type.
pub const MIN_ID_CHARS: usize = 4;
/// Longest goal a mission takes (the daemon's own limit).
pub const MAX_GOAL_CHARS: usize = 4_000;
/// Missions a list shows.
const LIST_MAX: usize = 10;
/// Longest reply, in characters (Discord's limit is 2000).
const REPLY_MAX: usize = 1_800;
/// Longest line of text that comes from the mission.
const LINE_CHARS: usize = 200;

// ---------------------------------------------------------------- wire types

/// Item counts of a mission (the daemon's `progress`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
pub struct MissionCounts {
    /// All items.
    #[serde(default)]
    pub total: u32,
    /// Finished.
    #[serde(default)]
    pub done: u32,
    /// Parked for the owner.
    #[serde(default)]
    pub blocked: u32,
    /// Parked on a timer, a mail or a webhook.
    #[serde(default)]
    pub waiting: u32,
    /// Given up.
    #[serde(default)]
    pub failed: u32,
    /// Still for the bot.
    #[serde(default)]
    pub left: u32,
}

/// A mission with its headline numbers (the daemon's `MissionDto`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct MissionBrief {
    /// Mission id (a UUID).
    pub id: String,
    /// Short label.
    #[serde(default)]
    pub title: String,
    /// `running`, `waiting`, `paused`, `blocked`, `done`, `failed` or `cancelled`.
    pub state: String,
    /// Why it is paused, blocked or failed.
    #[serde(default)]
    pub state_reason: Option<String>,
    /// Counts.
    #[serde(default)]
    pub progress: MissionCounts,
    /// A session is working right now.
    #[serde(default)]
    pub session_running: bool,
    /// Last change, Unix ms.
    #[serde(default)]
    pub updated_at_ms: i64,
}

impl MissionBrief {
    /// Ended for good: done, failed or cancelled.
    #[must_use]
    pub fn is_ended(&self) -> bool {
        matches!(self.state.as_str(), "done" | "failed" | "cancelled")
    }
}

/// One ledger item (the daemon's `MissionItemDto`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct MissionLine {
    /// Dedupe key.
    #[serde(default)]
    pub key: String,
    /// What to do.
    #[serde(default)]
    pub title: String,
    /// `todo`, `doing`, `done`, `blocked`, `waiting`, `failed` or `skipped`.
    #[serde(default)]
    pub status: String,
    /// What was achieved.
    #[serde(default)]
    pub result: Option<String>,
    /// What the owner has to do or answer.
    #[serde(default)]
    pub blocked_reason: Option<String>,
    /// Last change, Unix ms.
    #[serde(default)]
    pub updated_at_ms: i64,
}

/// A line of the digest (the daemon's `MissionDigestItemDto`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct DigestLine {
    /// Item key.
    #[serde(default)]
    pub key: String,
    /// Item title.
    #[serde(default)]
    pub title: String,
    /// Reason, error or result.
    #[serde(default)]
    pub text: Option<String>,
}

/// The report written when a mission ended or blocked (the daemon's `MissionDigestDto`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct DigestView {
    /// `done`, `blocked`, `failed` or `cancelled`.
    #[serde(default)]
    pub outcome: String,
    /// Why.
    #[serde(default)]
    pub reason: Option<String>,
    /// Items given up.
    #[serde(default)]
    pub failed: Vec<DigestLine>,
    /// A few finished items.
    #[serde(default)]
    pub done_sample: Vec<DigestLine>,
    /// Decisions the bot recorded.
    #[serde(default)]
    pub decisions: Vec<String>,
}

/// A mission with its ledger and last report (the daemon's `MissionDetailDto`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct MissionView {
    /// The mission.
    pub mission: MissionBrief,
    /// Every ledger item.
    #[serde(default)]
    pub items: Vec<MissionLine>,
    /// The items that wait for the owner.
    #[serde(default)]
    pub blocked: Vec<MissionLine>,
    /// The last report, once the mission ended or blocked.
    #[serde(default)]
    pub digest: Option<DigestView>,
}

/// What `/status` adds to the daemon's health line.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Fleet {
    /// Names of the bots.
    pub bots: Vec<String>,
    /// Runs working right now (`None`: could not be read).
    pub running_runs: Option<usize>,
    /// Missions by state (`None`: missions are off or could not be read).
    pub missions: Option<MissionTally>,
}

/// Open missions by state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MissionTally {
    /// Working or waiting for a timer, a mail or a webhook.
    pub running: usize,
    /// Waiting for the owner.
    pub blocked: usize,
    /// Paused.
    pub paused: usize,
}

impl MissionTally {
    /// Count the open missions of `list`.
    #[must_use]
    pub fn of(list: &[MissionBrief]) -> Self {
        let mut t = Self::default();
        for m in list {
            match m.state.as_str() {
                "running" | "waiting" => t.running += 1,
                "blocked" => t.blocked += 1,
                "paused" => t.paused += 1,
                _ => {}
            }
        }
        t
    }
}

// ------------------------------------------------------------------ commands

/// A control command, in the language-neutral form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Control {
    /// `/help`.
    Help,
    /// `/missions`.
    Missions,
    /// `/mission <id>` (empty: no id given).
    Mission(String),
    /// `/mission new <text>`.
    NewMission(String),
    /// `/pause <id>`.
    Pause(String),
    /// `/resume <id>`.
    Resume(String),
    /// `/stop <id>` with an id (a bare `/stop` still stops the chat's run).
    Stop(String),
}

/// The English name of a command written under its Turkish name (`gorev` is `mission`);
/// anything else is returned as is, in lower case. Telegram commands are ASCII only.
#[must_use]
pub fn canonical(cmd: &str) -> String {
    let c = cmd.to_ascii_lowercase();
    match c.as_str() {
        "yardim" => "help",
        "gorevler" => "missions",
        "gorev" => "mission",
        "duraklat" => "pause",
        "surdur" => "resume",
        "durdur" => "stop",
        "durum" => "status",
        other => other,
    }
    .to_owned()
}

impl Control {
    /// The command for `cmd` and its argument, or `None` when it is not one of these (`/stop`
    /// without an id included: the bridge's own stop of the chat's run stays as it was).
    /// `cmd` is lower case, without the `/` or `!`, and either language's name.
    #[must_use]
    pub fn parse(cmd: &str, arg: &str) -> Option<Self> {
        let arg = arg.trim();
        match canonical(cmd).as_str() {
            "help" => Some(Self::Help),
            "missions" => Some(Self::Missions),
            "mission" => {
                let (head, rest) = arg.split_once(char::is_whitespace).unwrap_or((arg, ""));
                if matches!(head.to_lowercase().as_str(), "new" | "yeni") {
                    Some(Self::NewMission(rest.trim().to_owned()))
                } else {
                    Some(Self::Mission(arg.to_owned()))
                }
            }
            "pause" => Some(Self::Pause(arg.to_owned())),
            "resume" => Some(Self::Resume(arg.to_owned())),
            "stop" if !arg.is_empty() => Some(Self::Stop(arg.to_owned())),
            _ => None,
        }
    }

    /// Starting a mission is limited more tightly than looking at one.
    #[must_use]
    pub fn is_start(&self) -> bool {
        matches!(self, Self::NewMission(_))
    }
}

/// The 8 characters shown for a mission id.
#[must_use]
pub fn short_id(id: &str) -> String {
    let hex: Vec<char> = id.chars().filter(|c| *c != '-').collect();
    hex[hex.len().saturating_sub(SHORT_ID_CHARS)..]
        .iter()
        .collect()
}

/// What an id the owner typed names.
#[derive(Debug, PartialEq, Eq)]
pub enum Pick<'a> {
    /// Exactly one mission.
    One(&'a MissionBrief),
    /// No mission ends with it.
    None,
    /// Several do: ask for more characters.
    Ambiguous(usize),
    /// Not an id at all (not hex, too short).
    Invalid,
}

/// Find the mission `typed` names: the whole id, or at least [`MIN_ID_CHARS`] hex characters of
/// its end (what `/missions` shows). Dashes and case do not matter.
#[must_use]
pub fn pick_mission<'a>(list: &'a [MissionBrief], typed: &str) -> Pick<'a> {
    let t: String = typed
        .trim()
        .trim_start_matches('#')
        .chars()
        .filter(|c| *c != '-')
        .map(|c| c.to_ascii_lowercase())
        .collect();
    if t.len() < MIN_ID_CHARS || t.len() > 32 || !t.chars().all(|c| c.is_ascii_hexdigit()) {
        return Pick::Invalid;
    }
    let hits: Vec<&MissionBrief> = list
        .iter()
        .filter(|m| {
            m.id.chars()
                .filter(|c| *c != '-')
                .map(|c| c.to_ascii_lowercase())
                .collect::<String>()
                .ends_with(&t)
        })
        .collect();
    match hits.as_slice() {
        [] => Pick::None,
        [one] => Pick::One(one),
        many => Pick::Ambiguous(many.len()),
    }
}

// ---------------------------------------------------------- stop confirmation

/// Callback payload of the "Stop it" button.
#[must_use]
pub fn stop_confirm_callback(id: &str) -> String {
    format!("mc:y:{id}")
}

/// Callback payload of the "Keep going" button.
#[must_use]
pub fn stop_cancel_callback(id: &str) -> String {
    format!("mc:n:{id}")
}

/// A press of one of the stop buttons.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopPress {
    /// Stop it.
    Confirm(String),
    /// Leave it.
    Cancel(String),
}

/// The press in a `mc:` callback payload (the id is a UUID, checked here).
#[must_use]
pub fn parse_stop_callback(data: &str) -> Option<StopPress> {
    let rest = data.strip_prefix("mc:")?;
    let (action, id) = rest.split_once(':')?;
    let ok = id.len() == 36
        && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
        && id.chars().filter(|c| *c == '-').count() == 4;
    if !ok {
        return None;
    }
    match action {
        "y" => Some(StopPress::Confirm(id.to_owned())),
        "n" => Some(StopPress::Cancel(id.to_owned())),
        _ => None,
    }
}

/// The authenticated account of the paired owner: a number the service gave (a Telegram user id,
/// a Discord snowflake), never a username or a display name. A hub keeps its rate windows and its
/// stop questions per `OwnerId`, so what one account does never counts against another.
///
/// The constructors are the only way in; the bridge builds one from the id the service
/// authenticated, and a name that is not a plain number cannot become one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OwnerId(u64);

impl OwnerId {
    /// A Telegram user id (positive; negative ids are groups and channels).
    #[must_use]
    pub fn telegram(user_id: i64) -> Option<Self> {
        u64::try_from(user_id).ok().filter(|n| *n > 0).map(Self)
    }

    /// A Discord snowflake as the gateway sends it: plain decimal digits, no sign, no padding.
    /// The caller must pass the verified author id of the event, never `display_name`.
    #[must_use]
    pub fn snowflake(id: &str) -> Option<Self> {
        let canonical =
            !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) && !id.starts_with('0');
        if !canonical {
            return None;
        }
        id.parse().ok().filter(|n| *n > 0).map(Self)
    }

    /// The id as the service writes it.
    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }
}

/// The stops the owner was asked to confirm and has not answered yet. A button only counts when
/// the bridge offered it to that owner a moment ago, and only once.
#[derive(Debug)]
pub struct ConfirmBook {
    ttl: Duration,
    asked: VecDeque<(String, OwnerId, Instant)>,
}

/// Open questions kept for one owner.
const CONFIRM_MAX: usize = 16;

impl ConfirmBook {
    /// A book whose questions lapse after `ttl`.
    #[must_use]
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            asked: VecDeque::new(),
        }
    }

    /// `owner` was asked whether to stop mission `id`. Only the oldest question of the same owner
    /// makes room, so one account never pushes another's out.
    pub fn ask(&mut self, id: &str, owner: OwnerId, now: Instant) {
        self.asked.retain(|(i, o, _)| !(i == id && *o == owner));
        self.asked.push_back((id.to_owned(), owner, now));
        while self.asked.iter().filter(|(_, o, _)| *o == owner).count() > CONFIRM_MAX {
            if let Some(oldest) = self.asked.iter().position(|(_, o, _)| *o == owner) {
                self.asked.remove(oldest);
            }
        }
    }

    /// Take the open question for `id` and `owner`: `true` once, and only while it is fresh.
    pub fn take(&mut self, id: &str, owner: OwnerId, now: Instant) -> bool {
        let ttl = self.ttl;
        self.asked
            .retain(|(_, _, at)| now.saturating_duration_since(*at) < ttl);
        let before = self.asked.len();
        self.asked.retain(|(i, o, _)| !(i == id && *o == owner));
        self.asked.len() != before
    }
}

// ----------------------------------------------------------------- rate limit

/// How many commands the owner may send, so a stuck client or a stolen session cannot hammer
/// the daemon: a window for everything, a smaller one for starting missions.
#[derive(Debug)]
pub struct RateLimit {
    any: Window,
    start: Window,
}

#[derive(Debug)]
struct Window {
    max: usize,
    span: Duration,
    hits: VecDeque<Instant>,
    told: bool,
}

impl Window {
    fn new(max: usize, span: Duration) -> Self {
        Self {
            max,
            span,
            hits: VecDeque::new(),
            told: false,
        }
    }

    fn prune(&mut self, now: Instant) {
        while self
            .hits
            .front()
            .is_some_and(|t| now.saturating_duration_since(*t) >= self.span)
        {
            self.hits.pop_front();
        }
        if self.hits.len() < self.max {
            self.told = false;
        }
    }

    fn full(&self) -> bool {
        self.hits.len() >= self.max
    }
}

/// The answer of [`RateLimit::check`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rate {
    /// Go ahead.
    Allowed,
    /// Too many; say so (the first time in this window).
    Refused,
    /// Too many; say nothing (already told).
    Silent,
}

impl RateLimit {
    /// Commands per minute, and missions started per ten minutes.
    #[must_use]
    pub fn new(per_minute: usize, starts_per_ten_minutes: usize) -> Self {
        Self {
            any: Window::new(per_minute, Duration::from_secs(60)),
            start: Window::new(starts_per_ten_minutes, Duration::from_secs(600)),
        }
    }

    /// Count one command at `now`, or refuse it.
    pub fn check(&mut self, ctl: &Control, now: Instant) -> Rate {
        self.count(ctl.is_start(), now)
    }

    /// Count one press of a button ("Stop it", "Keep going") against the same window as the
    /// commands. Never [`Rate::Refused`]: a button that is too fast is ignored, not answered.
    pub fn check_press(&mut self, now: Instant) -> Rate {
        match self.count(false, now) {
            Rate::Allowed => Rate::Allowed,
            Rate::Refused | Rate::Silent => Rate::Silent,
        }
    }

    fn count(&mut self, starts: bool, now: Instant) -> Rate {
        self.any.prune(now);
        self.start.prune(now);
        let blocked = if self.any.full() {
            Some(&mut self.any)
        } else if starts && self.start.full() {
            Some(&mut self.start)
        } else {
            None
        };
        if let Some(w) = blocked {
            return if std::mem::replace(&mut w.told, true) {
                Rate::Silent
            } else {
                Rate::Refused
            };
        }
        self.any.hits.push_back(now);
        if starts {
            self.start.hits.push_back(now);
        }
        Rate::Allowed
    }
}

impl Default for RateLimit {
    fn default() -> Self {
        Self::new(20, 3)
    }
}

// ------------------------------------------------------------------- wording

/// What a state change did (for the wording of an answer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// `/pause`.
    Pause,
    /// `/resume`.
    Resume,
    /// `/stop`.
    Stop,
}

/// An answer to the owner.
#[derive(Debug, Clone, Copy)]
pub enum Say<'a> {
    /// The commands.
    Help,
    /// No mission yet.
    NoMissions,
    /// The list.
    List(&'a [MissionBrief]),
    /// One mission with its last report.
    Detail(&'a MissionView),
    /// An id is needed.
    NeedId(Action),
    /// `/mission` alone.
    NeedMissionId,
    /// Not an id.
    BadId,
    /// No such mission.
    NotFound,
    /// Several match.
    Ambiguous(usize),
    /// Done: paused, resumed or stopped.
    Changed(Action, &'a MissionBrief),
    /// The mission is in a state where that does not apply.
    CannotNow(Action, &'a MissionBrief),
    /// Ask whether to stop it for good (the buttons are the bridge's).
    ConfirmStop(&'a MissionBrief),
    /// The owner chose to keep it.
    StopCancelled,
    /// The question is too old, or was never asked.
    StopExpired,
    /// `/mission new` without a text.
    NewUsage,
    /// The text is longer than a mission takes.
    GoalTooLong,
    /// A mission started.
    Started(&'a MissionBrief),
    /// The daemon refused the start (the bot is not on the Full level, bad text).
    NotStarted,
    /// There is no bot to run it.
    NoBot,
    /// The daemon runs no missions (off, or this host does not have them).
    Unavailable,
    /// The daemon could not be reached or said no.
    Failed,
    /// Too many commands.
    TooFast,
    /// A forwarded message.
    Forwarded,
    /// Not in a private chat.
    OnlyPrivate,
}

impl Flavor {
    /// What a command starts with.
    #[must_use]
    pub fn prefix(self) -> char {
        match self {
            Self::Plain => '/',
            Self::Discord => '!',
        }
    }
}

/// Label of the button that stops a mission.
#[must_use]
pub fn stop_label(lang: NoticeLang) -> &'static str {
    pick(lang, "Stop it", "Durdur")
}

/// Label of the button that leaves a mission as it is.
#[must_use]
pub fn keep_label(lang: NoticeLang) -> &'static str {
    pick(lang, "Keep going", "Devam etsin")
}

fn state_words(lang: NoticeLang, state: &str) -> (&'static str, &'static str) {
    match state {
        "running" => ("▶", pick(lang, "running", "çalışıyor")),
        "waiting" => (
            "⏳",
            pick(lang, "waiting for a timer or a reply", "bir şey bekliyor"),
        ),
        "paused" => ("⏸", pick(lang, "paused", "duraklatıldı")),
        "blocked" => ("✋", pick(lang, "needs you", "senden bir şey gerekiyor")),
        "done" => ("✅", pick(lang, "done", "tamamlandı")),
        "failed" => ("⚠️", pick(lang, "failed", "başarısız")),
        "cancelled" => ("⏹", pick(lang, "stopped", "durduruldu")),
        _ => ("•", pick(lang, "unknown", "bilinmiyor")),
    }
}

fn title_of(m: &MissionBrief, flavor: Flavor) -> String {
    let t = if m.title.trim().is_empty() {
        "-"
    } else {
        &m.title
    };
    inert(&clip(t, 60), flavor)
}

fn counts_line(lang: NoticeLang, p: &MissionCounts) -> String {
    let mut s = match lang {
        NoticeLang::En => format!("{} of {} done", p.done, p.total),
        NoticeLang::Tr => format!("{} / {} tamam", p.done, p.total),
    };
    let mut part = |n: u32, en: &str, tr: &str| {
        if n > 0 {
            let _ = write!(s, ", {n} {}", pick(lang, en, tr));
        }
    };
    part(p.blocked, "waiting for you", "seni bekliyor");
    part(
        p.waiting,
        "waiting on a timer or a reply",
        "zamanlayıcı ya da yanıt bekliyor",
    );
    part(p.failed, "given up", "vazgeçildi");
    part(p.left, "left", "kaldı");
    s
}

/// Cut `text` to the reply limit, at a line end when it can.
fn fit(text: String) -> String {
    if text.chars().count() <= REPLY_MAX {
        return text;
    }
    let cut: String = text.chars().take(REPLY_MAX - 1).collect();
    let head = cut.rfind('\n').map_or(cut.as_str(), |i| &cut[..i]);
    format!("{head}\n…")
}

fn action_words(lang: NoticeLang, a: Action) -> &'static str {
    match a {
        Action::Pause => pick(lang, "pause", "duraklat"),
        Action::Resume => pick(lang, "resume", "sürdür"),
        Action::Stop => pick(lang, "stop", "durdur"),
    }
}

fn command_name(a: Action) -> &'static str {
    match a {
        Action::Pause => "pause",
        Action::Resume => "resume",
        Action::Stop => "stop",
    }
}

fn help(lang: NoticeLang, p: char) -> String {
    match lang {
        NoticeLang::En => format!(
            "Missions from your phone (only you, in this private chat):\n\
             {p}missions: list them\n\
             {p}mission <id>: details and the last report\n\
             {p}mission new <text>: start a mission\n\
             {p}pause <id> · {p}resume <id> · {p}stop <id> (asks you to confirm)\n\
             {p}status: bots, runs and the server\n\
             The id is the 8 characters {p}missions shows.\n\
             Turkish names: {p}gorevler, {p}gorev, {p}gorev yeni, {p}duraklat, {p}surdur, {p}durdur, {p}durum, {p}yardim."
        ),
        NoticeLang::Tr => format!(
            "Telefondan görevler (yalnızca sen, bu özel sohbette):\n\
             {p}gorevler: görevleri listele\n\
             {p}gorev <id>: ayrıntılar ve son rapor\n\
             {p}gorev yeni <metin>: yeni görev başlat\n\
             {p}duraklat <id> · {p}surdur <id> · {p}durdur <id> (onay ister)\n\
             {p}durum: botlar, çalışanlar ve sunucu\n\
             Id, {p}gorevler listesindeki 8 karakterdir.\n\
             İngilizce adları da çalışır: {p}missions, {p}mission, {p}mission new, {p}pause, {p}resume, {p}stop, {p}status, {p}help."
        ),
    }
}

fn list(lang: NoticeLang, flavor: Flavor, all: &[MissionBrief]) -> String {
    // Open ones first, then the newest ended ones (the daemon lists newest first).
    let mut ordered: Vec<&MissionBrief> = all.iter().filter(|m| !m.is_ended()).collect();
    ordered.extend(all.iter().filter(|m| m.is_ended()));
    let mut s = match lang {
        NoticeLang::En => format!("Missions ({}):\n", all.len()),
        NoticeLang::Tr => format!("Görevler ({}):\n", all.len()),
    };
    for m in ordered.iter().take(LIST_MAX) {
        let (icon, word) = state_words(lang, &m.state);
        let _ = writeln!(
            s,
            "{icon} {} {word} · {}/{} — {}",
            short_id(&m.id),
            m.progress.done,
            m.progress.total,
            title_of(m, flavor)
        );
    }
    if all.len() > LIST_MAX {
        let _ = writeln!(
            s,
            "{}",
            pick(lang, "…and more in the app.", "…ve devamı uygulamada.")
        );
    }
    let p = flavor.prefix();
    let _ = write!(
        s,
        "{}",
        match lang {
            NoticeLang::En => format!("{p}mission <id> for details."),
            NoticeLang::Tr => format!("Ayrıntı için {p}gorev <id>."),
        }
    );
    s
}

fn digest_lines(s: &mut String, flavor: Flavor, lines: &[DigestLine], max: usize, bullet: &str) {
    for d in lines.iter().take(max) {
        let text = d
            .text
            .as_deref()
            .filter(|t| !t.trim().is_empty())
            .unwrap_or(&d.title);
        let _ = writeln!(
            s,
            "{}",
            quoted(flavor, &format!("{bullet} {}", clip(text, LINE_CHARS)))
        );
    }
}

fn detail(lang: NoticeLang, flavor: Flavor, v: &MissionView) -> String {
    let m = &v.mission;
    let (icon, word) = state_words(lang, &m.state);
    let mut s = format!("{icon} {} ({})\n", title_of(m, flavor), short_id(&m.id));
    let _ = writeln!(s, "{}: {word}", pick(lang, "State", "Durum"));
    let _ = writeln!(s, "{}", counts_line(lang, &m.progress));
    if let Some(r) = m.state_reason.as_deref().filter(|r| !r.trim().is_empty()) {
        let _ = writeln!(s, "{}", pick(lang, "Why:", "Neden:"));
        let _ = writeln!(s, "{}", quoted(flavor, &clip(r, LINE_CHARS)));
    }
    if !v.blocked.is_empty() {
        s.push_str(pick(lang, "\nWaiting for you:\n", "\nSeni bekleyenler:\n"));
        for b in v.blocked.iter().take(3) {
            let why = b
                .blocked_reason
                .as_deref()
                .filter(|t| !t.trim().is_empty())
                .unwrap_or(&b.title);
            let line = format!("{}: {}", clip(&b.key, 40), clip(why, LINE_CHARS));
            let _ = writeln!(s, "{}", quoted(flavor, &line));
        }
        if v.blocked.len() > 3 {
            let _ = writeln!(
                s,
                "{}",
                pick(lang, "…more in the app.", "…devamı uygulamada.")
            );
        }
        let _ = writeln!(
            s,
            "{}",
            pick(
                lang,
                "Answer them in the app, or with /answer when the question was sent here.",
                "Uygulamada yanıtla; soru buraya geldiyse /answer ile de olur."
            )
        );
    }
    if let Some(d) = &v.digest {
        let _ = writeln!(
            s,
            "\n{}",
            match lang {
                NoticeLang::En => format!("Last report ({}):", clip(&d.outcome, 20)),
                NoticeLang::Tr => format!("Son rapor ({}):", clip(&d.outcome, 20)),
            }
        );
        if let Some(r) = d.reason.as_deref().filter(|r| !r.trim().is_empty()) {
            let _ = writeln!(s, "{}", quoted(flavor, &clip(r, LINE_CHARS)));
        }
        digest_lines(&mut s, flavor, &d.done_sample, 3, "•");
        if !d.failed.is_empty() {
            s.push_str(pick(lang, "Given up:\n", "Vazgeçilenler:\n"));
            digest_lines(&mut s, flavor, &d.failed, 3, "-");
        }
        if !d.decisions.is_empty() {
            s.push_str(pick(lang, "Decisions:\n", "Kararlar:\n"));
            for dec in d.decisions.iter().take(3) {
                let _ = writeln!(
                    s,
                    "{}",
                    quoted(flavor, &format!("- {}", clip(dec, LINE_CHARS)))
                );
            }
        }
    } else {
        // Nothing reported yet: the newest finished items are the best picture of the work.
        let mut done: Vec<&MissionLine> = v
            .items
            .iter()
            .filter(|i| {
                i.status == "done" && i.result.as_deref().is_some_and(|r| !r.trim().is_empty())
            })
            .collect();
        done.sort_by_key(|i| std::cmp::Reverse(i.updated_at_ms));
        if !done.is_empty() {
            s.push_str(pick(lang, "\nLatest results:\n", "\nSon sonuçlar:\n"));
            for i in done.iter().take(3) {
                let text = i.result.as_deref().unwrap_or(&i.title);
                let _ = writeln!(
                    s,
                    "{}",
                    quoted(flavor, &format!("• {}", clip(text, LINE_CHARS)))
                );
            }
        }
    }
    s.trim_end().to_owned()
}

/// The wording of `s`, for a chat in `lang` on a channel of `flavor`.
#[must_use]
#[allow(clippy::too_many_lines)] // one arm of wording per answer
pub fn say(lang: NoticeLang, flavor: Flavor, what: &Say<'_>) -> String {
    let pre = flavor.prefix();
    let out = match *what {
        Say::Help => help(lang, pre),
        Say::NoMissions => match lang {
            NoticeLang::En => format!("No missions yet. Start one with {pre}mission new <text>."),
            NoticeLang::Tr => format!("Henüz görev yok. {pre}gorev yeni <metin> ile başlat."),
        },
        Say::List(all) => list(lang, flavor, all),
        Say::Detail(view) => detail(lang, flavor, view),
        Say::NeedId(action) => match lang {
            NoticeLang::En => format!(
                "Which mission? Write {pre}{} <id>, the 8 characters {pre}missions shows.",
                command_name(action)
            ),
            NoticeLang::Tr => format!(
                "Hangi görev? {pre}{} <id> yaz; id, {pre}gorevler listesindeki 8 karakterdir.",
                match action {
                    Action::Pause => "duraklat",
                    Action::Resume => "surdur",
                    Action::Stop => "durdur",
                }
            ),
        },
        Say::NeedMissionId => match lang {
            NoticeLang::En => format!("Write {pre}mission <id>, or {pre}missions for the list."),
            NoticeLang::Tr => format!("{pre}gorev <id> yaz; liste için {pre}gorevler."),
        },
        Say::BadId => match lang {
            NoticeLang::En => {
                format!("That is not a mission id. Use the 8 characters {pre}missions shows.")
            }
            NoticeLang::Tr => {
                format!("Bu bir görev id'si değil. {pre}gorevler listesindeki 8 karakteri kullan.")
            }
        },
        Say::NotFound => match lang {
            NoticeLang::En => format!("No mission with that id. See {pre}missions."),
            NoticeLang::Tr => format!("Bu id'li bir görev yok. {pre}gorevler listesine bak."),
        },
        Say::Ambiguous(n) => match lang {
            NoticeLang::En => format!("That matches {n} missions. Write more of the id."),
            NoticeLang::Tr => format!("Bu, {n} göreve uyuyor. Id'nin daha fazlasını yaz."),
        },
        Say::Changed(action, brief) => {
            let (icon, en, tr) = match action {
                Action::Pause => ("⏸", "Paused", "Duraklatıldı"),
                Action::Resume => ("▶", "Resumed", "Sürdürülüyor"),
                Action::Stop => ("⏹", "Stopped", "Durduruldu"),
            };
            let mut text = format!(
                "{icon} {}: {} ({})",
                pick(lang, en, tr),
                title_of(brief, flavor),
                short_id(&brief.id)
            );
            if action == Action::Stop {
                text.push('\n');
                text.push_str(pick(
                    lang,
                    "The report follows in a moment.",
                    "Rapor birazdan gelir.",
                ));
            }
            text
        }
        Say::CannotNow(action, brief) => {
            let (_, word) = state_words(lang, &brief.state);
            match lang {
                NoticeLang::En => format!(
                    "Cannot {} “{}” now: it is {word}.",
                    action_words(lang, action),
                    title_of(brief, flavor)
                ),
                NoticeLang::Tr => format!(
                    "“{}” şu an {} edilemez: {word}.",
                    title_of(brief, flavor),
                    action_words(lang, action)
                ),
            }
        }
        Say::ConfirmStop(brief) => match lang {
            NoticeLang::En => format!(
                "Stop “{}” ({}) for good? {} of {} done so far. A stopped mission cannot be resumed.",
                title_of(brief, flavor),
                short_id(&brief.id),
                brief.progress.done,
                brief.progress.total
            ),
            NoticeLang::Tr => format!(
                "“{}” ({}) kalıcı olarak durdurulsun mu? Şimdilik {} / {} tamam. Durdurulan görev yeniden başlatılamaz.",
                title_of(brief, flavor),
                short_id(&brief.id),
                brief.progress.done,
                brief.progress.total
            ),
        },
        Say::StopCancelled => pick(lang, "Okay, nothing changed.", "Tamam, hiçbir şey değişmedi.").to_owned(),
        Say::StopExpired => match lang {
            NoticeLang::En => format!("That question has expired. Send {pre}stop <id> again."),
            NoticeLang::Tr => format!("Bu sorunun süresi doldu. {pre}durdur <id> komutunu yeniden gönder."),
        },
        Say::NewUsage => match lang {
            NoticeLang::En => format!("Write the task after it: {pre}mission new <what to do>."),
            NoticeLang::Tr => format!("Görevi komutun ardına yaz: {pre}gorev yeni <ne yapılacak>."),
        },
        Say::GoalTooLong => match lang {
            NoticeLang::En => format!("That task is too long. A mission takes up to {MAX_GOAL_CHARS} characters."),
            NoticeLang::Tr => format!("Bu görev çok uzun. Bir görev en fazla {MAX_GOAL_CHARS} karakter alır."),
        },
        Say::Started(brief) => match lang {
            NoticeLang::En => format!(
                "🚀 Mission started: {} ({}). I will message you when it finishes or needs you. {pre}mission {} shows how it goes.",
                title_of(brief, flavor),
                short_id(&brief.id),
                short_id(&brief.id)
            ),
            NoticeLang::Tr => format!(
                "🚀 Görev başladı: {} ({}). Bitince ya da sana ihtiyaç olunca yazarım. Gidişatı {pre}gorev {} ile görürsün.",
                title_of(brief, flavor),
                short_id(&brief.id),
                short_id(&brief.id)
            ),
        },
        Say::NotStarted => pick(
            lang,
            "I could not start it. The bot has to be on the Full level (set it in the app), and the task has to be written out.",
            "Başlatamadım. Botun Tam (Full) düzeyde olması gerekir (uygulamadan ayarla) ve görev yazılı olmalı.",
        )
        .to_owned(),
        Say::NoBot => pick(
            lang,
            "There is no bot to run it. Create one in the app first.",
            "Çalıştıracak bir bot yok. Önce uygulamada bir bot oluştur.",
        )
        .to_owned(),
        Say::Unavailable => pick(
            lang,
            "Missions are not available on this computer right now.",
            "Görevler şu an bu bilgisayarda kullanılamıyor.",
        )
        .to_owned(),
        Say::Failed => pick(
            lang,
            "That did not work. Try again, or open the app.",
            "Olmadı. Yeniden dene ya da uygulamayı aç.",
        )
        .to_owned(),
        Say::TooFast => pick(
            lang,
            "Too many commands. Please wait a minute.",
            "Çok fazla komut. Lütfen bir dakika bekle.",
        )
        .to_owned(),
        Say::Forwarded => pick(
            lang,
            "A forwarded message cannot run a command. Type it yourself.",
            "İletilen bir mesaj komut çalıştıramaz. Komutu kendin yaz.",
        )
        .to_owned(),
        Say::OnlyPrivate => pick(
            lang,
            "Mission commands only work in our private chat.",
            "Görev komutları yalnızca özel sohbetimizde çalışır.",
        )
        .to_owned(),
    };
    fit(out)
}

/// The lines `/status` adds after the daemon's health line.
#[must_use]
pub fn status_extra(lang: NoticeLang, flavor: Flavor, f: &Fleet) -> String {
    let mut s = String::new();
    let names: Vec<String> = f
        .bots
        .iter()
        .take(8)
        .map(|b| inert(&clip(b, 30), flavor))
        .collect();
    let more = f.bots.len().saturating_sub(names.len());
    let _ = write!(
        s,
        "{} ({}): {}",
        pick(lang, "Bots", "Botlar"),
        f.bots.len(),
        if names.is_empty() {
            "-".to_owned()
        } else {
            names.join(", ")
        }
    );
    if more > 0 {
        let _ = write!(s, " +{more}");
    }
    s.push('\n');
    if let Some(n) = f.running_runs {
        let _ = writeln!(s, "{}: {n}", pick(lang, "Running now", "Şu an çalışan"));
    }
    if let Some(t) = f.missions {
        let _ = match lang {
            NoticeLang::En => writeln!(
                s,
                "Missions: {} running, {} waiting for you, {} paused",
                t.running, t.blocked, t.paused
            ),
            NoticeLang::Tr => writeln!(
                s,
                "Görevler: {} çalışıyor, {} seni bekliyor, {} duraklatıldı",
                t.running, t.blocked, t.paused
            ),
        };
    }
    fit(s.trim_end().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID_A: &str = "0192f6c4-1111-7000-8000-aaaaaaaa1234";
    const ID_B: &str = "0192f6c4-2222-7000-8000-bbbbbbbb1234";

    fn brief(id: &str, state: &str, title: &str) -> MissionBrief {
        MissionBrief {
            id: id.into(),
            title: title.into(),
            state: state.into(),
            state_reason: None,
            progress: MissionCounts {
                total: 10,
                done: 6,
                blocked: 2,
                waiting: 0,
                failed: 1,
                left: 1,
            },
            session_running: false,
            updated_at_ms: 1,
        }
    }

    #[test]
    fn turkish_names_map_to_the_english_commands() {
        for (tr, en) in [
            ("gorevler", "missions"),
            ("GOREV", "mission"),
            ("duraklat", "pause"),
            ("surdur", "resume"),
            ("durdur", "stop"),
            ("durum", "status"),
            ("yardim", "help"),
        ] {
            assert_eq!(canonical(tr), en);
        }
        assert_eq!(canonical("bots"), "bots");
    }

    #[test]
    fn commands_parse_in_both_languages() {
        assert_eq!(Control::parse("missions", ""), Some(Control::Missions));
        assert_eq!(Control::parse("gorevler", ""), Some(Control::Missions));
        assert_eq!(
            Control::parse("mission", "abcd1234"),
            Some(Control::Mission("abcd1234".into()))
        );
        assert_eq!(
            Control::parse("mission", "new  watch my site "),
            Some(Control::NewMission("watch my site".into()))
        );
        assert_eq!(
            Control::parse("gorev", "yeni siteyi izle"),
            Some(Control::NewMission("siteyi izle".into()))
        );
        // `newsletter` is an id attempt, not `new`.
        assert_eq!(
            Control::parse("mission", "newsletter"),
            Some(Control::Mission("newsletter".into()))
        );
        assert_eq!(
            Control::parse("mission", ""),
            Some(Control::Mission(String::new()))
        );
        assert_eq!(
            Control::parse("pause", "abcd"),
            Some(Control::Pause("abcd".into()))
        );
        assert_eq!(
            Control::parse("surdur", "abcd"),
            Some(Control::Resume("abcd".into()))
        );
        assert_eq!(
            Control::parse("durdur", "abcd"),
            Some(Control::Stop("abcd".into()))
        );
        // A bare stop is the bridge's own stop of the chat's run.
        assert_eq!(Control::parse("stop", ""), None);
        assert_eq!(Control::parse("status", ""), None);
        assert_eq!(Control::parse("bots", ""), None);
    }

    #[test]
    fn an_id_is_the_tail_of_the_uuid_and_dashes_and_case_do_not_matter() {
        assert_eq!(short_id(ID_A), "aaaa1234");
        assert_eq!(short_id(ID_A).len(), SHORT_ID_CHARS);
        let list = vec![brief(ID_A, "running", "A"), brief(ID_B, "paused", "B")];
        // Both start the same way (a timestamp): only the tail tells them apart.
        assert_eq!(pick_mission(&list, "aaaa1234"), Pick::One(&list[0]));
        assert_eq!(pick_mission(&list, "#BBBB1234"), Pick::One(&list[1]));
        assert_eq!(pick_mission(&list, ID_B), Pick::One(&list[1]));
        assert_eq!(pick_mission(&list, "1234"), Pick::Ambiguous(2));
        assert_eq!(pick_mission(&list, "cccc"), Pick::None);
        assert_eq!(pick_mission(&list, "abc"), Pick::Invalid);
        assert_eq!(pick_mission(&list, "newsletter"), Pick::Invalid);
        assert_eq!(pick_mission(&list, ""), Pick::Invalid);
    }

    #[test]
    fn stop_callbacks_round_trip_and_reject_anything_else() {
        let yes = stop_confirm_callback(ID_A);
        assert!(yes.len() <= 64, "Telegram allows 64 bytes");
        assert_eq!(
            parse_stop_callback(&yes),
            Some(StopPress::Confirm(ID_A.into()))
        );
        assert_eq!(
            parse_stop_callback(&stop_cancel_callback(ID_A)),
            Some(StopPress::Cancel(ID_A.into()))
        );
        for bad in [
            "mc:y:",
            "mc:y:abc",
            "mc:z:0192f6c4-1111-7000-8000-aaaaaaaa1234",
            "nr:abc",
            "mc:y:0192f6c4111170008000aaaaaaaa12345678",
        ] {
            assert_eq!(parse_stop_callback(bad), None, "{bad}");
        }
    }

    fn owner(n: u64) -> OwnerId {
        OwnerId::snowflake(&n.to_string()).unwrap()
    }

    #[test]
    fn an_owner_id_is_a_plain_number_never_a_name() {
        assert_eq!(
            OwnerId::snowflake("1234567890").map(OwnerId::get),
            Some(1_234_567_890)
        );
        assert_eq!(OwnerId::telegram(42).map(OwnerId::get), Some(42));
        for bad in [
            "",
            "0",
            "007",
            "-5",
            "+5",
            "4 2",
            "alice",
            "42a",
            "１２",
            "99999999999999999999999",
        ] {
            assert_eq!(OwnerId::snowflake(bad), None, "{bad:?}");
        }
        for bad in [0, -1, -1_001_234_567_890] {
            assert_eq!(OwnerId::telegram(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_stop_question_is_answered_once_by_the_one_asked_while_fresh() {
        let t0 = Instant::now();
        let mut book = ConfirmBook::new(Duration::from_secs(300));
        book.ask(ID_A, owner(42), t0);
        assert!(!book.take(ID_A, owner(43), t0), "someone else");
        assert!(!book.take(ID_B, owner(42), t0), "another mission");
        assert!(book.take(ID_A, owner(42), t0 + Duration::from_secs(10)));
        assert!(
            !book.take(ID_A, owner(42), t0 + Duration::from_secs(11)),
            "only once"
        );
        book.ask(ID_A, owner(42), t0);
        assert!(
            !book.take(ID_A, owner(42), t0 + Duration::from_secs(301)),
            "expired"
        );
    }

    #[test]
    fn one_owner_cannot_push_another_owners_question_out_of_the_book() {
        let t0 = Instant::now();
        let mut book = ConfirmBook::new(Duration::from_secs(300));
        book.ask(ID_A, owner(1), t0);
        for n in 0..(CONFIRM_MAX * 2) {
            book.ask(&format!("mission-{n}"), owner(2), t0);
        }
        assert!(
            book.take(ID_A, owner(1), t0),
            "the first owner's question stayed"
        );
        assert!(
            !book.take("mission-0", owner(2), t0),
            "the flooder lost the oldest"
        );
        assert!(book.take(&format!("mission-{}", CONFIRM_MAX * 2 - 1), owner(2), t0));
    }

    #[test]
    fn button_presses_count_against_the_window_and_are_never_answered() {
        let t0 = Instant::now();
        let mut rl = RateLimit::new(3, 1);
        assert_eq!(rl.check(&Control::Missions, t0), Rate::Allowed);
        assert_eq!(rl.check_press(t0), Rate::Allowed);
        assert_eq!(rl.check_press(t0), Rate::Allowed);
        // Spent: both a press and a command wait, and the press says nothing.
        assert_eq!(rl.check_press(t0), Rate::Silent);
        assert_eq!(rl.check_press(t0), Rate::Silent);
        assert_eq!(rl.check(&Control::Missions, t0), Rate::Silent);
        assert_eq!(rl.check_press(t0 + Duration::from_secs(61)), Rate::Allowed);
    }

    #[test]
    fn the_rate_limit_tells_once_and_starts_are_limited_harder() {
        let t0 = Instant::now();
        let mut rl = RateLimit::new(3, 1);
        let list = Control::Missions;
        for _ in 0..3 {
            assert_eq!(rl.check(&list, t0), Rate::Allowed);
        }
        assert_eq!(rl.check(&list, t0), Rate::Refused);
        assert_eq!(rl.check(&list, t0), Rate::Silent);
        assert_eq!(rl.check(&list, t0 + Duration::from_secs(61)), Rate::Allowed);

        let mut rl = RateLimit::new(50, 1);
        let start = Control::NewMission("x".into());
        assert_eq!(rl.check(&start, t0), Rate::Allowed);
        assert_eq!(rl.check(&start, t0 + Duration::from_secs(5)), Rate::Refused);
        // Looking is still fine; starting again waits ten minutes.
        assert_eq!(rl.check(&list, t0 + Duration::from_secs(5)), Rate::Allowed);
        assert_eq!(
            rl.check(&start, t0 + Duration::from_secs(601)),
            Rate::Allowed
        );
    }

    #[test]
    fn a_list_shows_open_missions_first_in_both_languages() {
        let all = vec![
            brief(ID_B, "done", "Old one"),
            brief(ID_A, "running", "Backlinks"),
        ];
        let en = say(NoticeLang::En, Flavor::Plain, &Say::List(&all));
        let a = en.find("aaaa1234").unwrap();
        let b = en.find("bbbb1234").unwrap();
        assert!(a < b, "{en}");
        assert!(en.contains("running") && en.contains("6/10"), "{en}");
        let tr = say(NoticeLang::Tr, Flavor::Plain, &Say::List(&all));
        assert!(
            tr.contains("çalışıyor") && tr.contains("Görevler (2)"),
            "{tr}"
        );
    }

    #[test]
    fn text_a_mission_wrote_is_inert_on_both_channels() {
        let mut m = brief(ID_A, "blocked", "Visit https://evil.example and @everyone");
        m.state_reason = Some("click [here](https://evil.example) /stop now".into());
        let v = MissionView {
            mission: m,
            items: vec![],
            blocked: vec![MissionLine {
                key: "k".into(),
                title: "t".into(),
                status: "blocked".into(),
                result: None,
                blocked_reason: Some("<@123> use **this** link evil.com".into()),
                updated_at_ms: 1,
            }],
            digest: None,
        };
        for flavor in [Flavor::Plain, Flavor::Discord] {
            let out = say(NoticeLang::En, flavor, &Say::Detail(&v));
            assert!(!out.contains("https://"), "{out}");
            assert!(
                !out.contains("@everyone") && !out.contains("<@123>"),
                "{out}"
            );
            assert!(!out.contains("evil.com"), "{out}");
            assert!(!out.contains(" /stop"), "{out}");
        }
        let discord = say(NoticeLang::En, Flavor::Discord, &Say::Detail(&v));
        assert!(discord.contains("\\*\\*this\\*\\*"), "{discord}");
    }

    #[test]
    fn a_detail_without_a_report_shows_the_latest_results() {
        let v = MissionView {
            mission: brief(ID_A, "running", "Backlinks"),
            items: vec![
                MissionLine {
                    key: "a".into(),
                    title: "A".into(),
                    status: "done".into(),
                    result: Some("older result".into()),
                    blocked_reason: None,
                    updated_at_ms: 1,
                },
                MissionLine {
                    key: "b".into(),
                    title: "B".into(),
                    status: "done".into(),
                    result: Some("newer result".into()),
                    blocked_reason: None,
                    updated_at_ms: 2,
                },
            ],
            blocked: vec![],
            digest: None,
        };
        let out = say(NoticeLang::En, Flavor::Plain, &Say::Detail(&v));
        assert!(out.contains("Latest results"), "{out}");
        assert!(
            out.find("newer").unwrap() < out.find("older").unwrap(),
            "{out}"
        );
    }

    #[test]
    fn a_detail_with_a_report_shows_it() {
        let v = MissionView {
            mission: brief(ID_A, "done", "Backlinks"),
            items: vec![],
            blocked: vec![],
            digest: Some(DigestView {
                outcome: "done".into(),
                reason: Some("all sites visited".into()),
                failed: vec![DigestLine {
                    key: "x".into(),
                    title: "x".into(),
                    text: Some("refused".into()),
                }],
                done_sample: vec![DigestLine {
                    key: "y".into(),
                    title: "y".into(),
                    text: Some("listed".into()),
                }],
                decisions: vec!["skip paid sites".into()],
            }),
        };
        let out = say(NoticeLang::Tr, Flavor::Plain, &Say::Detail(&v));
        for want in [
            "Son rapor",
            "all sites visited",
            "listed",
            "refused",
            "skip paid sites",
        ] {
            assert!(out.contains(want), "{want} in {out}");
        }
    }

    #[test]
    fn replies_stay_inside_the_message_limit() {
        let many: Vec<MissionBrief> = (0..40)
            .map(|i| {
                brief(
                    &format!("0192f6c4-1111-7000-8000-{i:012x}"),
                    "running",
                    &"t".repeat(300),
                )
            })
            .collect();
        let out = say(NoticeLang::En, Flavor::Discord, &Say::List(&many));
        assert!(out.chars().count() <= REPLY_MAX, "{}", out.chars().count());
        assert!(out.contains("more in the app"));
    }

    #[test]
    fn every_answer_has_wording_in_both_languages() {
        let m = brief(ID_A, "paused", "Backlinks");
        let v = MissionView {
            mission: m.clone(),
            items: vec![],
            blocked: vec![],
            digest: None,
        };
        let all = [m.clone()];
        let says = [
            Say::Help,
            Say::NoMissions,
            Say::List(&all),
            Say::Detail(&v),
            Say::NeedId(Action::Pause),
            Say::NeedId(Action::Stop),
            Say::NeedMissionId,
            Say::BadId,
            Say::NotFound,
            Say::Ambiguous(2),
            Say::Changed(Action::Pause, &m),
            Say::Changed(Action::Stop, &m),
            Say::CannotNow(Action::Resume, &m),
            Say::ConfirmStop(&m),
            Say::StopCancelled,
            Say::StopExpired,
            Say::NewUsage,
            Say::GoalTooLong,
            Say::Started(&m),
            Say::NotStarted,
            Say::NoBot,
            Say::Unavailable,
            Say::Failed,
            Say::TooFast,
            Say::Forwarded,
            Say::OnlyPrivate,
        ];
        for s in &says {
            let en = say(NoticeLang::En, Flavor::Plain, s);
            let tr = say(NoticeLang::Tr, Flavor::Plain, s);
            assert!(!en.is_empty() && !tr.is_empty());
            assert_ne!(en, tr, "{s:?} is not translated");
        }
    }

    #[test]
    fn help_names_both_languages_and_uses_the_channels_prefix() {
        let en = say(NoticeLang::En, Flavor::Plain, &Say::Help);
        assert!(en.contains("/missions") && en.contains("/gorevler"), "{en}");
        let dc = say(NoticeLang::Tr, Flavor::Discord, &Say::Help);
        assert!(
            dc.contains("!gorevler") && dc.contains("!mission new"),
            "{dc}"
        );
        assert!(!dc.contains("/gorevler"), "{dc}");
    }

    #[test]
    fn status_lines_cover_bots_runs_and_missions() {
        let f = Fleet {
            bots: vec!["Scout".into(), "Writer".into()],
            running_runs: Some(2),
            missions: Some(MissionTally {
                running: 1,
                blocked: 1,
                paused: 0,
            }),
        };
        let en = status_extra(NoticeLang::En, Flavor::Plain, &f);
        assert!(en.contains("Bots (2): Scout, Writer"), "{en}");
        assert!(en.contains("Running now: 2"), "{en}");
        assert!(
            en.contains("1 running, 1 waiting for you, 0 paused"),
            "{en}"
        );
        let tr = status_extra(NoticeLang::Tr, Flavor::Plain, &Fleet::default());
        assert!(tr.contains("Botlar (0): -"), "{tr}");
        assert!(!tr.contains("Görevler"), "{tr}");
    }
}
