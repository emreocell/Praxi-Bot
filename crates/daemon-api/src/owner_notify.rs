//! Owner notices: the daemon tells the owner on its own when a mission or a background run is
//! done, failed, or cannot go on without them (and, if they ask for it, at milestones).
//!
//! * The [`Hub`] keeps the notices (the app's notification panel reads them with
//!   `GET /v1/owner-notices`) and decides which of them are *pushed*: the owner's settings pick
//!   the events, quiet hours hold pushes back until the window ends. The Telegram and Discord
//!   bridges follow `GET /v1/owner-notices/stream` and render each notice in their chat.
//! * A notice carries only what the ledger and the run record say (counts, the bot's recorded
//!   reasons, a few finished items): no model call writes it, so it cannot claim more than
//!   happened.
//! * Approvals are never decided through a notice. They are listed as "waiting in the app"
//!   (`app_only`); only the questions of blocked mission items and of a run can be answered from
//!   a chat.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::Duration;

use agent_core::StreamItem;
use axum::{
    Json,
    extract::{Path as AxumPath, Query, State},
    http::HeaderMap,
    response::sse::{Event as SseEvent, KeepAlive, Sse},
};
use channel_api::notice::{
    MAX_LISTED, NoticeItem, NoticeKind, NoticeProgress, NoticeSource, OwnerNotice,
};
use chrono::{NaiveTime, TimeZone, Timelike, Utc};
use parking_lot::Mutex;
use protocol::{BotId, Event, RunId, TaskId, ToolCallId};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::mission_routes::{MissionAnswerInput, MissionAnswersInput};
use crate::{ApiError, AppState, WeakAppState, error::ErrorBody, routes::ApiJson};

/// File of the owner's settings, in the data directory.
pub const SETTINGS_FILE: &str = "owner-notify.json";
/// File of the notices (the panel log, the push log, the held ones), in the data directory, so a
/// restart loses none of them.
pub const NOTICES_FILE: &str = "owner-notices.json";
/// A notice that no bridge was handed before a restart is replayed to the first bridge that
/// connects, unless it is older than this (a day-old "finished" is not news).
const UNSENT_MAX_AGE_MS: i64 = 12 * 60 * 60 * 1000;
/// Notices the panel log keeps.
const LOG_MAX: usize = 200;
/// Notices the push log keeps for a bridge that reconnects.
const PUSH_MAX: usize = 100;
/// Notices held back by quiet hours; older ones fall out of the push (the panel keeps them).
const HELD_MAX: usize = 30;
/// A scheduled run that succeeds notifies at most this often per schedule.
const FINISHED_COALESCE_MS: i64 = 15 * 60 * 1000;
/// A scheduled run that fails notifies at most this often per schedule.
const FAILED_COALESCE_MS: i64 = 5 * 60 * 1000;
/// A standing order that reports the same problem again notifies at most this often.
const NEEDS_OWNER_COALESCE_MS: i64 = 60 * 60 * 1000;
/// What the notice says when a standing order ended without a clear `OK` or `NEEDS_OWNER(...)`.
const STANDING_UNCLEAR: &str =
    "The check ended without a clear OK or NEEDS_OWNER answer. Open the run to see what it found.";
/// What the notice says when a guard (step limit, budget, repeat detector) cut a standing
/// order short: an unfinished check is never a quiet one.
const STANDING_CUT_SHORT: &str =
    "The check was cut short by a limit before it finished. Open the run to see what it found.";
/// Questions and approvals of one run are batched into one notice within this window.
const BATCH_WINDOW: Duration = Duration::from_secs(4);
/// How often held notices are checked against the quiet window.
const HELD_TICK: Duration = Duration::from_secs(30);
/// Longest text kept per notice line.
const TEXT_MAX: usize = 300;
/// Runs whose questions are being batched at once; more are told about in the app only.
const MAX_WATCHED_RUNS: usize = 256;
/// Questions (and app-only items) kept per run and batch: one more than a message lists, so the
/// message still says "more in the app".
const MAX_WANTS: usize = MAX_LISTED + 1;

// ----------------------------------------------------------------------------- settings

/// A daily window (`HH:MM` to `HH:MM`, may cross midnight) in which nothing is pushed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct QuietWindow {
    /// Start, `HH:MM`.
    pub start: String,
    /// End, `HH:MM`.
    pub end: String,
    /// IANA zone (`Europe/Istanbul`); empty means this computer's zone.
    #[serde(default)]
    pub timezone: String,
}

/// What the owner wants to be told about on Telegram, Discord and other channels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)] // one independent switch per kind of event
pub struct OwnerNotifySettings {
    /// A mission or background run finished.
    pub finished: bool,
    /// One failed.
    pub failed: bool,
    /// One needs the owner (questions, approvals in the app).
    pub blocked: bool,
    /// A mission passed a milestone (25%, 50%, 75%).
    pub progress: bool,
    /// No pushes inside this window; they arrive when it ends.
    #[serde(default)]
    pub quiet_hours: Option<QuietWindow>,
}

impl Default for OwnerNotifySettings {
    fn default() -> Self {
        Self {
            finished: true,
            failed: true,
            blocked: true,
            progress: false,
            quiet_hours: None,
        }
    }
}

/// Longest time zone name accepted (the longest IANA name is 32 characters).
const MAX_ZONE_CHARS: usize = 64;

fn parse_hhmm(s: &str) -> Option<NaiveTime> {
    let (h, m) = s.trim().split_once(':')?;
    if h.len() != 2 || m.len() != 2 {
        return None;
    }
    NaiveTime::from_hms_opt(h.parse().ok()?, m.parse().ok()?, 0)
}

fn zone_of(name: &str) -> chrono_tz::Tz {
    name.trim()
        .parse()
        .ok()
        .or_else(|| {
            iana_time_zone::get_timezone()
                .ok()
                .and_then(|n| n.parse().ok())
        })
        .unwrap_or(chrono_tz::UTC)
}

impl OwnerNotifySettings {
    /// Refuse a window that cannot be read.
    ///
    /// # Errors
    /// What is wrong, for the owner.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(q) = &self.quiet_hours {
            let (Some(start), Some(end)) = (parse_hhmm(&q.start), parse_hhmm(&q.end)) else {
                return Err("quiet hours need times like 22:00 and 08:00".into());
            };
            if start == end {
                return Err("quiet hours need a start that differs from the end".into());
            }
            let zone = q.timezone.trim();
            if zone.len() > MAX_ZONE_CHARS {
                return Err("the time zone name is too long".into());
            }
            if !zone.is_empty() && zone.parse::<chrono_tz::Tz>().is_err() {
                return Err("unknown time zone".into());
            }
        }
        Ok(())
    }

    /// Whether `at_ms` falls inside the quiet window (a window whose ends are equal never does).
    #[must_use]
    pub fn is_quiet(&self, at_ms: i64) -> bool {
        let Some(q) = &self.quiet_hours else {
            return false;
        };
        let (Some(start), Some(end)) = (parse_hhmm(&q.start), parse_hhmm(&q.end)) else {
            return false;
        };
        if start == end {
            return false;
        }
        let Some(utc) = Utc.timestamp_millis_opt(at_ms).single() else {
            return false;
        };
        let local = utc.with_timezone(&zone_of(&q.timezone)).time();
        let local = NaiveTime::from_hms_opt(local.hour(), local.minute(), 0).unwrap_or(local);
        if start < end {
            local >= start && local < end
        } else {
            local >= start || local < end
        }
    }

    fn wants(&self, kind: NoticeKind) -> bool {
        match kind {
            NoticeKind::Finished => self.finished,
            NoticeKind::Failed => self.failed,
            NoticeKind::Blocked => self.blocked,
            NoticeKind::Progress => self.progress,
        }
    }
}

fn load_settings(path: &Path) -> OwnerNotifySettings {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice::<OwnerNotifySettings>(&b).ok())
        .filter(|s| s.validate().is_ok())
        .unwrap_or_default()
}

/// Write the settings so that a crash leaves the old file or the new one, never half of one, and
/// only the daemon's user can read it.
fn save_settings(path: &Path, s: &OwnerNotifySettings) -> std::io::Result<()> {
    save_json(path, s)
}

fn save_json<T: Serialize>(path: &Path, s: &T) -> std::io::Result<()> {
    use std::io::Write as _;
    let tmp = path.with_extension("json.tmp");
    let written = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        #[cfg(unix)]
        {
            // A leftover file from an earlier crash keeps its own mode through `open`.
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(&serde_json::to_vec_pretty(s)?)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
        return written;
    }
    // Make the rename itself durable (best effort: not every file system can sync a directory).
    #[cfg(unix)]
    if let Some(dir) = path.parent()
        && let Ok(dir) = std::fs::File::open(dir)
    {
        let _ = dir.sync_all();
    }
    Ok(())
}

// ----------------------------------------------------------------------------- the hub

#[derive(Default)]
struct HubState {
    settings: OwnerNotifySettings,
    seq: u64,
    /// Every notice, for the app's panel.
    log: VecDeque<OwnerNotice>,
    /// The notices released for pushing, for a bridge that reconnects.
    pushed: VecDeque<OwnerNotice>,
    /// Waiting for the quiet window to end.
    held: VecDeque<OwnerNotice>,
    /// Last notice time per coalescing key.
    last_by_key: HashMap<String, i64>,
    /// Highest milestone (1..=3 quarters) already announced per mission.
    milestones: HashMap<String, u32>,
    /// Notices whose questions were answered (only ids still in `log`).
    answered: HashSet<String>,
    /// The newest notice a bridge was handed, so a restart knows what is still unsent.
    delivered_seq: u64,
}

/// What survives a restart: the bounded logs (see [`LOG_MAX`], [`PUSH_MAX`], [`HELD_MAX`]).
#[derive(Default, Serialize, Deserialize)]
struct Persisted {
    seq: u64,
    #[serde(default)]
    delivered_seq: u64,
    log: Vec<OwnerNotice>,
    pushed: Vec<OwnerNotice>,
    held: Vec<OwnerNotice>,
    #[serde(default)]
    answered: Vec<String>,
}

fn load_notices(path: &Path) -> Option<Persisted> {
    let bytes = std::fs::read(path).ok()?;
    match serde_json::from_slice::<Persisted>(&bytes) {
        Ok(p) => Some(p),
        Err(error) => {
            support_logs::tracing::warn!(target: "praxi_support", reason = "component_warning", %error, "owner notices could not be read; starting with none");
            None
        }
    }
}

fn newest<T>(mut items: Vec<T>, max: usize) -> VecDeque<T> {
    let skip = items.len().saturating_sub(max);
    items.drain(..skip);
    items.into()
}

/// The notice log and the push gate.
pub(crate) struct Hub {
    state: Mutex<HubState>,
    tx: broadcast::Sender<OwnerNotice>,
    path: Option<PathBuf>,
    /// Where the notices are kept (none: in memory only).
    notices_path: Option<PathBuf>,
    /// One writer at a time, so the file ends with the newest state.
    write_lock: Mutex<()>,
    /// With a notices file from an earlier run: the `seq` up to which a bridge had been handed
    /// notices when that run ended. The first bridge to connect without a cursor gets the newer
    /// ones.
    resume_after: Option<u64>,
}

/// A notice before it has an id and a place in the log.
pub(crate) struct Draft {
    pub kind: NoticeKind,
    pub source: NoticeSource,
    pub title: String,
    pub bot_name: String,
    pub bot_id: Option<String>,
    pub mission_id: Option<String>,
    pub run_id: Option<String>,
    pub chat_id: Option<String>,
    pub reason: Option<String>,
    pub progress: Option<NoticeProgress>,
    pub done: Vec<NoticeItem>,
    pub failed: Vec<NoticeItem>,
    pub questions: Vec<NoticeItem>,
    pub app_only: Vec<String>,
    pub can_retry: bool,
    /// Several notices with the same key inside the window collapse into the first.
    pub coalesce: Option<(String, i64)>,
}

impl Draft {
    pub(crate) fn new(kind: NoticeKind, source: NoticeSource, title: String) -> Self {
        Self {
            kind,
            source,
            title,
            bot_name: String::new(),
            bot_id: None,
            mission_id: None,
            run_id: None,
            chat_id: None,
            reason: None,
            progress: None,
            done: Vec::new(),
            failed: Vec::new(),
            questions: Vec::new(),
            app_only: Vec::new(),
            can_retry: false,
            coalesce: None,
        }
    }
}

impl Hub {
    pub(crate) fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(SETTINGS_FILE);
        let notices_path = data_dir.join(NOTICES_FILE);
        let mut state = HubState {
            settings: load_settings(&path),
            ..HubState::default()
        };
        let mut resume_after = None;
        if let Some(p) = load_notices(&notices_path) {
            state.seq = p.seq;
            state.delivered_seq = p.delivered_seq;
            state.log = newest(p.log, LOG_MAX);
            state.pushed = newest(p.pushed, PUSH_MAX);
            state.held = newest(p.held, HELD_MAX);
            state.answered = p.answered.into_iter().collect();
            resume_after = Some(p.delivered_seq);
        }
        Self {
            state: Mutex::new(state),
            tx: broadcast::channel(64).0,
            path: Some(path),
            notices_path: Some(notices_path),
            write_lock: Mutex::new(()),
            resume_after,
        }
    }

    /// Write the notices down. A failure is logged and does not stop the notice: it still goes
    /// out, it only would not survive a restart.
    fn persist(&self) {
        let Some(path) = &self.notices_path else {
            return;
        };
        let _one_writer = self.write_lock.lock();
        let snapshot = {
            let st = self.state.lock();
            Persisted {
                seq: st.seq,
                delivered_seq: st.delivered_seq,
                log: st.log.iter().cloned().collect(),
                pushed: st.pushed.iter().cloned().collect(),
                held: st.held.iter().cloned().collect(),
                answered: st.answered.iter().cloned().collect(),
            }
        };
        if let Err(error) = save_json(path, &snapshot) {
            support_logs::tracing::warn!(target: "praxi_support", reason = "component_warning", %error, "owner notices could not be saved");
        }
    }

    /// A bridge was handed the notice `seq`.
    pub(crate) fn mark_delivered(&self, seq: u64) {
        {
            let mut st = self.state.lock();
            if seq <= st.delivered_seq {
                return;
            }
            st.delivered_seq = seq;
        }
        self.persist();
    }

    #[cfg(test)]
    pub(crate) fn in_memory() -> Self {
        Self {
            state: Mutex::new(HubState::default()),
            tx: broadcast::channel(64).0,
            path: None,
            notices_path: None,
            write_lock: Mutex::new(()),
            resume_after: None,
        }
    }

    pub(crate) fn settings(&self) -> OwnerNotifySettings {
        self.state.lock().settings.clone()
    }

    pub(crate) fn set_settings(&self, s: OwnerNotifySettings, now_ms: i64) -> std::io::Result<()> {
        if let Some(path) = &self.path {
            save_settings(path, &s)?;
        }
        self.state.lock().settings = s;
        // A window that was just removed or moved may release what it held.
        self.release_held(now_ms);
        Ok(())
    }

    /// Record a notice and push it when the settings allow. Returns it (`None`: coalesced).
    pub(crate) fn publish(&self, d: Draft, now_ms: i64) -> Option<OwnerNotice> {
        let notice = self.publish_unsaved(d, now_ms);
        if notice.is_some() {
            self.persist();
        }
        notice
    }

    fn publish_unsaved(&self, d: Draft, now_ms: i64) -> Option<OwnerNotice> {
        let mut st = self.state.lock();
        Self::release_locked(&mut st, &self.tx, now_ms);
        if let Some((key, window)) = &d.coalesce {
            if st
                .last_by_key
                .get(key)
                .is_some_and(|at| now_ms.saturating_sub(*at) < *window)
            {
                return None;
            }
            if st.last_by_key.len() > 512 {
                st.last_by_key.clear();
            }
            st.last_by_key.insert(key.clone(), now_ms);
        }
        st.seq += 1;
        let notice = OwnerNotice {
            seq: st.seq,
            id: Uuid::now_v7().to_string(),
            at_ms: now_ms,
            kind: d.kind,
            source: d.source,
            title: d.title,
            bot_name: d.bot_name,
            bot_id: d.bot_id,
            mission_id: d.mission_id,
            run_id: d.run_id,
            chat_id: d.chat_id,
            reason: d.reason,
            progress: d.progress,
            done: d.done,
            failed: d.failed,
            questions: d.questions,
            app_only: d.app_only,
            can_retry: d.can_retry,
            late: false,
        };
        st.log.push_back(notice.clone());
        while st.log.len() > LOG_MAX {
            if let Some(old) = st.log.pop_front() {
                st.answered.remove(&old.id);
            }
        }
        if st.settings.wants(notice.kind) {
            if st.settings.is_quiet(now_ms) {
                // A milestone that was not seen in time is not worth a late message.
                if notice.kind != NoticeKind::Progress {
                    st.held.push_back(notice.clone());
                    while st.held.len() > HELD_MAX {
                        st.held.pop_front();
                    }
                }
            } else {
                Self::push_locked(&mut st, &self.tx, notice.clone());
            }
        }
        Some(notice)
    }

    fn push_locked(st: &mut HubState, tx: &broadcast::Sender<OwnerNotice>, n: OwnerNotice) {
        st.pushed.push_back(n.clone());
        while st.pushed.len() > PUSH_MAX {
            st.pushed.pop_front();
        }
        // No bridge listening is not an error: the notice stays in the push log.
        let _ = tx.send(n);
    }

    fn release_locked(st: &mut HubState, tx: &broadcast::Sender<OwnerNotice>, now_ms: i64) {
        if st.held.is_empty() || st.settings.is_quiet(now_ms) {
            return;
        }
        let held: Vec<OwnerNotice> = st.held.drain(..).collect();
        for mut n in held {
            if st.settings.wants(n.kind) {
                n.late = true;
                Self::push_locked(st, tx, n);
            }
        }
    }

    /// Push what quiet hours held back, once the window is over.
    pub(crate) fn release_held(&self, now_ms: i64) {
        let released = {
            let mut st = self.state.lock();
            let had = !st.held.is_empty();
            Self::release_locked(&mut st, &self.tx, now_ms);
            had && st.held.is_empty()
        };
        if released {
            self.persist();
        }
    }

    /// Every notice after `after` (the app's panel).
    pub(crate) fn recent(&self, after: u64) -> Vec<OwnerNotice> {
        self.state
            .lock()
            .log
            .iter()
            .filter(|n| n.seq > after)
            .cloned()
            .collect()
    }

    pub(crate) fn get(&self, id: &str) -> Option<OwnerNotice> {
        self.state.lock().log.iter().find(|n| n.id == id).cloned()
    }

    /// Take the right to answer the questions of `notice`: once, and (for a mission) only while
    /// no newer notice of that mission replaced it, so a stale message cannot answer what the
    /// mission asks now. Give it back with [`Self::release_answers`] if the answer failed.
    pub(crate) fn claim_answers(&self, notice: &OwnerNotice) -> Result<(), &'static str> {
        let mut st = self.state.lock();
        if st.answered.contains(&notice.id) {
            return Err("these questions were already answered");
        }
        if notice.source == NoticeSource::Mission
            && st.log.iter().any(|n| {
                n.source == NoticeSource::Mission
                    && n.mission_id == notice.mission_id
                    && n.seq > notice.seq
            })
        {
            return Err("a newer notice replaced this one: answer that one");
        }
        st.answered.insert(notice.id.clone());
        drop(st);
        self.persist();
        Ok(())
    }

    /// The answer was not taken: the notice can be answered again.
    pub(crate) fn release_answers(&self, id: &str) {
        self.state.lock().answered.remove(id);
        self.persist();
    }

    /// The pushed notices after `after`, and a receiver for the next ones, taken together so
    /// none is missed or repeated.
    pub(crate) fn subscribe_push(
        &self,
        after: Option<u64>,
    ) -> (Vec<OwnerNotice>, broadcast::Receiver<OwnerNotice>) {
        let st = self.state.lock();
        let rx = self.tx.subscribe();
        // No cursor: a bridge that has never connected starts with what is new from now on, plus
        // what an earlier run pushed and no bridge was handed before it ended.
        let now_ms = storage::now_ms();
        let replay = match (after, self.resume_after) {
            (Some(after), _) => st
                .pushed
                .iter()
                .filter(|n| n.seq > after)
                .cloned()
                .collect(),
            (None, Some(resume)) => st
                .pushed
                .iter()
                .filter(|n| n.seq > resume && now_ms.saturating_sub(n.at_ms) < UNSENT_MAX_AGE_MS)
                .cloned()
                .collect(),
            (None, None) => Vec::new(),
        };
        (replay, rx)
    }

    pub(crate) fn latest_push_seq(&self) -> u64 {
        self.state.lock().pushed.back().map_or(0, |n| n.seq)
    }

    /// Whether a milestone (1..=3) of `mission` is new; remembers it.
    pub(crate) fn new_milestone(&self, mission: &str, quarter: u32) -> bool {
        let mut st = self.state.lock();
        if st.milestones.len() > 256 {
            st.milestones.clear();
        }
        let seen = st.milestones.entry(mission.to_owned()).or_insert(0);
        if quarter > *seen {
            *seen = quarter;
            true
        } else {
            false
        }
    }

    /// Start counting a mission's milestones from where it is now (a daemon restart must not
    /// repeat the ones already passed).
    pub(crate) fn seed_milestone(&self, mission: &str, quarter: u32) {
        let mut st = self.state.lock();
        st.milestones.entry(mission.to_owned()).or_insert(quarter);
    }
}

// ----------------------------------------------------------------------------- builders

fn clip(text: &str, max: usize) -> String {
    let t = text.trim();
    if t.chars().count() <= max {
        return t.to_owned();
    }
    let mut out: String = t.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn item_of(i: &missions::DigestItem) -> NoticeItem {
    NoticeItem {
        key: i.key.clone(),
        title: clip(&i.title, 80),
        text: i.text.as_deref().map(|t| clip(t, TEXT_MAX)),
    }
}

/// The notice of a mission that ended or blocked (`None`: nothing to tell, e.g. the owner
/// stopped it themselves). `blocked` are the ledger's blocked items.
pub(crate) fn mission_draft(
    m: &missions::Mission,
    bot_name: &str,
    blocked: &[missions::Item],
) -> Option<Draft> {
    use missions::MissionState as S;
    let kind = match m.state {
        S::Done => NoticeKind::Finished,
        S::Blocked => NoticeKind::Blocked,
        S::Failed => NoticeKind::Failed,
        // A mission asleep until its wake has nothing to report yet.
        S::Running | S::Paused | S::Cancelled | S::Waiting => return None,
    };
    let digest = m.digest.as_ref()?;
    let mut d = Draft::new(kind, NoticeSource::Mission, clip(&m.title, 90));
    bot_name.clone_into(&mut d.bot_name);
    d.bot_id = Some(m.bot_id.clone());
    d.mission_id = Some(m.id.clone());
    d.chat_id = Some(m.chat_id.clone());
    d.reason = m
        .state_reason
        .as_deref()
        .map(|r| clip(r, TEXT_MAX))
        .filter(|r| !r.is_empty());
    d.progress = Some(NoticeProgress {
        total: digest.progress.total,
        done: digest.progress.done,
        blocked: digest.progress.blocked,
        failed: digest.progress.failed,
        skipped: digest.progress.skipped,
    });
    d.done = digest.done_sample.iter().take(3).map(item_of).collect();
    d.failed = digest.failed.iter().take(5).map(item_of).collect();
    for it in blocked {
        let reason = it.blocked_reason.as_deref().map(|t| clip(t, TEXT_MAX));
        if it.live_only {
            // A one-time code is never typed into a chat: the owner finishes that step live.
            d.app_only.push(clip(
                &format!(
                    "{}: {}",
                    it.title,
                    reason.as_deref().unwrap_or("needs you live in the app")
                ),
                TEXT_MAX,
            ));
        } else {
            d.questions.push(NoticeItem {
                key: it.key.clone(),
                title: clip(&it.title, 80),
                text: reason,
            });
        }
    }
    // A blocked mission with a question is released by the answer; one blocked on a budget or a
    // model error can only be told to go on.
    d.can_retry = kind == NoticeKind::Blocked && d.questions.is_empty() && d.app_only.is_empty();
    Some(d)
}

/// A milestone of a mission that is still running: which quarter (1..=3) its done count is at.
pub(crate) fn milestone_quarter(p: missions::Progress) -> u32 {
    if p.total < 4 {
        return 0;
    }
    (p.done.saturating_mul(4) / p.total).min(3)
}

fn progress_draft(m: &missions::Mission, bot_name: &str, p: missions::Progress) -> Draft {
    let mut d = Draft::new(
        NoticeKind::Progress,
        NoticeSource::Mission,
        clip(&m.title, 90),
    );
    bot_name.clone_into(&mut d.bot_name);
    d.bot_id = Some(m.bot_id.clone());
    d.mission_id = Some(m.id.clone());
    d.chat_id = Some(m.chat_id.clone());
    d.progress = Some(NoticeProgress {
        total: p.total,
        done: p.done,
        blocked: p.blocked,
        failed: p.failed,
        skipped: p.skipped,
    });
    d
}

/// What a scheduled run is, read from its durable kind (`turn:schedule:<id>[:<due>:<attempt>]`,
/// `turn:external_schedule:...`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScheduledKind {
    pub schedule: Uuid,
    pub due_ms: Option<i64>,
}

pub(crate) fn scheduled_kind(kind: &str) -> Option<ScheduledKind> {
    let rest = kind
        .strip_prefix("turn:external_schedule:")
        .or_else(|| kind.strip_prefix("turn:schedule:"))?;
    let mut parts = rest.split(':');
    let schedule = Uuid::parse_str(parts.next()?).ok()?;
    let due_ms = parts.next().and_then(|d| d.parse().ok());
    Some(ScheduledKind { schedule, due_ms })
}

impl AppState {
    pub(crate) fn owner_notify(&self) -> &Hub {
        &self.0.owner_notify
    }

    /// Follow the event feed for the endings and questions of scheduled runs (once per daemon).
    pub(crate) fn start_notice_watcher(&self) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        if self.0.notice_watcher.set(()).is_err() {
            return;
        }
        runtime.spawn(watch(
            self.downgrade(),
            self.0.agent.subscribe(),
            self.0.shutdown.child_token(),
        ));
    }

    fn bot_name(&self, bot: BotId) -> String {
        self.storage()
            .get_bot(bot)
            .ok()
            .flatten()
            .map(|b| b.name)
            .unwrap_or_default()
    }

    /// A mission ended or blocked: tell the owner. Called with the mission as the supervisor
    /// just wrote its digest.
    pub(crate) fn notify_mission_concluded(&self, m: &missions::Mission) {
        let Ok(store) = self.mission_store() else {
            return;
        };
        let blocked = if m.state == missions::MissionState::Blocked {
            store
                .items(&m.id, Some(missions::ItemStatus::Blocked))
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let bot = BotId(Uuid::parse_str(&m.bot_id).unwrap_or_default());
        if let Some(d) = mission_draft(m, &self.bot_name(bot), &blocked) {
            self.owner_notify().publish(d, storage::now_ms());
        }
    }

    /// A mission changed: announce a milestone when its done count passed a quarter.
    pub(crate) fn notify_mission_changed(&self, id: &str) {
        let Ok(store) = self.mission_store() else {
            return;
        };
        let Ok(Some(m)) = store.get(id) else {
            return;
        };
        let Ok(p) = store.progress(id) else {
            return;
        };
        let quarter = milestone_quarter(p);
        let hub = self.owner_notify();
        if m.state != missions::MissionState::Running || quarter == 0 {
            hub.seed_milestone(id, quarter);
            return;
        }
        if hub.new_milestone(id, quarter) {
            let bot = BotId(Uuid::parse_str(&m.bot_id).unwrap_or_default());
            hub.publish(
                progress_draft(&m, &self.bot_name(bot), p),
                storage::now_ms(),
            );
        }
    }

    /// The last thing the bot said in `run`, clipped: what a background run "did", and whether
    /// the run had read something private when it said it. (The text was already passed through
    /// the secret redactor when it was stored.)
    fn run_result_text(&self, run: &storage::Run) -> Option<(String, bool)> {
        self.run_last_message(run)
            .map(|m| (clip(&m.content, 500), m.exposure_private))
    }

    /// The last non-empty assistant message of `run`, whole.
    fn run_last_message(&self, run: &storage::Run) -> Option<storage::Message> {
        let chat = run.chat_id?;
        let (messages, _) = self
            .storage()
            .list_messages_bounded(chat, None, 40, 256 * 1024)
            .ok()?;
        messages
            .into_iter()
            .filter(|m| {
                m.role == "assistant" && m.run_id == Some(run.id) && !m.content.trim().is_empty()
            })
            .max_by_key(|m| m.created_at_ms)
    }

    /// A standing order that ended well says nothing unless it asked for the owner: `OK`
    /// publishes no notice at all; `NEEDS_OWNER(reason)`, or an ending that is neither, does.
    /// Silence must be earned: an `OK` from a run a guard cut short, or that the verdict parser
    /// finds ambiguous, is not silence.
    ///
    /// The reason is the bot's text, which may repeat what a page or mail said: it is scrubbed
    /// of hidden and control characters and bounded, and after private reads it stays in the
    /// app. What the notice is deduplicated on is that scrubbed text, never the raw one.
    fn notify_standing_order(&self, run: &storage::Run, kind: &ScheduledKind) {
        let message = self.run_last_message(run);
        let verdict = message
            .as_ref()
            .map_or(scheduler::StandingVerdict::Unclear, |m| {
                scheduler::StandingVerdict::parse(&m.content)
            });
        // Not knowing whether a guard stopped the run counts as it having.
        let cut_short = self.agent().stopped_by_guard(run.id).unwrap_or(true);
        let (reason, key) = match verdict {
            scheduler::StandingVerdict::Ok if cut_short => (STANDING_CUT_SHORT.to_owned(), None),
            scheduler::StandingVerdict::Ok => return,
            scheduler::StandingVerdict::NeedsOwner(reason) => {
                let reason = clip(&protocol::approval_text::scrub(&reason, false), TEXT_MAX);
                // Text the bot wrote after reading private or untrusted content is not a
                // reliable key: it is shown fixed, or an attacker may vary it at will. One
                // notice per schedule and window then.
                let loose = message
                    .as_ref()
                    .is_some_and(|m| m.exposure_private || m.exposure_untrusted);
                if reason.is_empty() {
                    (STANDING_UNCLEAR.to_owned(), None)
                } else if loose {
                    (reason, Some(String::new()))
                } else {
                    (reason.clone(), Some(reason))
                }
            }
            scheduler::StandingVerdict::Unclear => (STANDING_UNCLEAR.to_owned(), None),
        };
        let private = message.as_ref().is_some_and(|m| m.exposure_private) && key.is_some();
        let mut d = Draft::new(
            NoticeKind::Blocked,
            NoticeSource::Run,
            self.scheduled_title(kind),
        );
        d.bot_name = self.bot_name(run.bot_id);
        d.bot_id = Some(run.bot_id.to_string());
        d.run_id = Some(run.id.to_string());
        d.chat_id = run.chat_id.map(|c| c.to_string());
        d.reason = Some(if private {
            PRIVATE_RESULT.to_owned()
        } else {
            reason.clone()
        });
        // The same problem is not repeated at every check; a different one still comes
        // through, except where the text cannot be trusted to tell problems apart. An unclear
        // or cut-short ending has its own key, so it is never held back by a reason notice.
        let same = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            key.as_deref().unwrap_or(reason.as_str()).hash(&mut h);
            h.finish()
        };
        let which = if key.is_some() {
            "needs_owner"
        } else {
            "unclear"
        };
        d.coalesce = Some((
            format!("{}:{which}:{same:x}", kind.schedule),
            NEEDS_OWNER_COALESCE_MS,
        ));
        self.owner_notify().publish(d, storage::now_ms());
    }

    fn is_standing_order(&self, kind: &ScheduledKind) -> bool {
        self.schedule_store()
            .ok()
            .and_then(|s| s.get(TaskId(kind.schedule)).ok().flatten())
            .is_some_and(|s| matches!(s.payload, scheduler::Payload::StandingOrder { .. }))
    }

    fn scheduled_title(&self, kind: &ScheduledKind) -> String {
        self.schedule_store()
            .ok()
            .and_then(|s| s.get(TaskId(kind.schedule)).ok().flatten())
            .map_or_else(|| "Scheduled task".to_owned(), |s| clip(&s.name, 90))
    }

    /// A run ended. Only scheduled runs are reported here: a mission reports itself, and a run
    /// the owner started in the app or a chat is followed there.
    pub(crate) fn notify_run_finished(&self, run_id: RunId, error: Option<&str>) {
        let Ok(Some(run)) = self.storage().get_run(run_id) else {
            return;
        };
        if run.parent_run_id.is_some() || run.state == "cancelled" {
            return;
        }
        let Some(kind) = scheduled_kind(&run.kind) else {
            return;
        };
        let failed = error.is_some() || run.state == "failed";
        if !failed && self.is_standing_order(&kind) {
            self.notify_standing_order(&run, &kind);
            return;
        }
        let mut d = Draft::new(
            if failed {
                NoticeKind::Failed
            } else {
                NoticeKind::Finished
            },
            NoticeSource::Run,
            self.scheduled_title(&kind),
        );
        d.bot_name = self.bot_name(run.bot_id);
        d.bot_id = Some(run.bot_id.to_string());
        d.run_id = Some(run.id.to_string());
        d.chat_id = run.chat_id.map(|c| c.to_string());
        let sched = kind.schedule.to_string();
        if failed {
            d.reason = Some(clip(
                error.or(run.error.as_deref()).unwrap_or("the run failed"),
                TEXT_MAX,
            ));
            d.can_retry = kind.due_ms.is_some();
            d.coalesce = Some((format!("{sched}:failed"), FAILED_COALESCE_MS));
        } else {
            if let Some((text, private)) = self.run_result_text(&run) {
                // What a run said after it read private data (mail, files, accounts) does not
                // leave this computer through a chat service: the app has it.
                let text = if private {
                    PRIVATE_RESULT.to_owned()
                } else {
                    text
                };
                d.done.push(NoticeItem {
                    key: run.id.to_string(),
                    title: d.title.clone(),
                    text: Some(text),
                });
            }
            d.coalesce = Some((format!("{sched}:finished"), FINISHED_COALESCE_MS));
        }
        self.owner_notify().publish(d, storage::now_ms());
    }

    /// One batched "needs you" notice for a scheduled run.
    fn notify_run_needs_owner(&self, run_id: RunId, wants: RunWants) {
        let Ok(Some(run)) = self.storage().get_run(run_id) else {
            return;
        };
        let Some(kind) = scheduled_kind(&run.kind) else {
            return;
        };
        if run.parent_run_id.is_some() {
            return;
        }
        let mut d = Draft::new(
            NoticeKind::Blocked,
            NoticeSource::Run,
            self.scheduled_title(&kind),
        );
        d.bot_name = self.bot_name(run.bot_id);
        d.bot_id = Some(run.bot_id.to_string());
        d.run_id = Some(run.id.to_string());
        d.chat_id = run.chat_id.map(|c| c.to_string());
        d.questions = wants.questions;
        d.app_only = wants.app_only;
        self.owner_notify().publish(d, storage::now_ms());
    }
}

/// What a notice says instead of a result the run produced after reading private data.
const PRIVATE_RESULT: &str = "The result has private content: open it in the app.";

/// What one run is waiting for, collected over a short window.
#[derive(Default)]
struct RunWants {
    questions: Vec<NoticeItem>,
    app_only: Vec<String>,
    /// Something was dropped for want of room (logged once per run).
    overflowed: bool,
}

impl RunWants {
    /// Keep one more question; past the limit it stays in the app and is dropped here.
    fn ask(&mut self, q: NoticeItem) {
        if self.questions.len() < MAX_WANTS {
            self.questions.push(q);
        } else {
            self.overflow();
        }
    }

    /// Keep one more app-only item; past the limit it stays in the app and is dropped here.
    fn app(&mut self, text: String) {
        if self.app_only.len() < MAX_WANTS {
            self.app_only.push(text);
        } else {
            self.overflow();
        }
    }

    fn overflow(&mut self) {
        if !self.overflowed {
            self.overflowed = true;
            support_logs::tracing::warn!(target: "praxi_support", reason = "component_warning", "daemon-api: owner notices: a run is waiting for more than {MAX_WANTS} things; the rest are shown in the app only");
        }
    }
}

/// The runs whose questions are being batched, bounded in number (and per run, see [`RunWants`]).
#[derive(Default)]
struct Watched {
    runs: HashMap<RunId, (tokio::time::Instant, RunWants)>,
    /// A run was refused room because the table was full (logged once per fill).
    full_warned: bool,
}

impl Watched {
    /// The batch of `run`, started if it is new. `None` when the table is full: that run's
    /// questions stay in the app.
    fn wants(&mut self, run: RunId) -> Option<&mut RunWants> {
        if !self.runs.contains_key(&run) && self.runs.len() >= MAX_WATCHED_RUNS {
            if !self.full_warned {
                self.full_warned = true;
                support_logs::tracing::warn!(target: "praxi_support", reason = "component_warning", "daemon-api: owner notices: too many runs are waiting for the owner at once; the newest are shown in the app only");
            }
            return None;
        }
        if self.runs.len() < MAX_WATCHED_RUNS / 2 {
            self.full_warned = false;
        }
        let entry = self.runs.entry(run).or_insert_with(|| {
            (
                tokio::time::Instant::now() + BATCH_WINDOW,
                RunWants::default(),
            )
        });
        Some(&mut entry.1)
    }
}

// ----------------------------------------------------------------------------- the watcher

/// Follow the daemon-wide event feed and turn the endings and the questions of scheduled runs
/// into notices; also releases what quiet hours held back. Ends with `cancel`.
pub(crate) async fn watch(
    weak: WeakAppState,
    mut rx: broadcast::Receiver<StreamItem>,
    cancel: CancellationToken,
) {
    let mut pending = Watched::default();
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    let mut since_held = Duration::ZERO;
    loop {
        tokio::select! {
            () = cancel.cancelled() => return,
            item = rx.recv() => {
                let item = match item {
                    Ok(i) => i,
                    // Events were missed: an ending can be lost, never made up.
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return,
                };
                let Some(state) = weak.upgrade() else { return };
                match &item.envelope.event {
                    Event::RunFinished(f) => {
                        pending.runs.remove(&f.run_id);
                        state.notify_run_finished(f.run_id, f.error.as_deref());
                    }
                    Event::QuestionAsked(q) => {
                        if let Some(w) = pending.wants(q.run_id) {
                            w.ask(NoticeItem {
                                key: q.call_id.to_string(),
                                title: clip(&q.question, 80),
                                text: Some(clip(&q.question, TEXT_MAX)),
                            });
                        }
                    }
                    Event::ApprovalNeeded(a) => {
                        if let Some(w) = pending.wants(a.run_id) {
                            w.app(clip(&format!("{}: {}", a.tool_name, a.summary), TEXT_MAX));
                        }
                    }
                    Event::SecretRequested(s) => {
                        if let Some(w) = pending.wants(s.run_id) {
                            w.app("a secret is requested (type it in the app)".to_owned());
                        }
                    }
                    Event::HandoverRequested(h) => {
                        if let Some(w) = pending.wants(h.run_id) {
                            w.app("the page needs you live (a code or a CAPTCHA)".to_owned());
                        }
                    }
                    _ => {}
                }
            }
            _ = tick.tick() => {
                let Some(state) = weak.upgrade() else { return };
                let now = tokio::time::Instant::now();
                let due: Vec<RunId> = pending
                    .runs
                    .iter()
                    .filter(|(_, (at, _))| *at <= now)
                    .map(|(r, _)| *r)
                    .collect();
                for run in due {
                    if let Some((_, wants)) = pending.runs.remove(&run) {
                        state.notify_run_needs_owner(run, wants);
                    }
                }
                since_held += Duration::from_millis(500);
                if since_held >= HELD_TICK {
                    since_held = Duration::ZERO;
                    state.owner_notify().release_held(storage::now_ms());
                }
            }
        }
    }
}

// ----------------------------------------------------------------------------- routes

/// `GET /v1/owner-notices` query.
#[derive(Debug, Deserialize)]
pub(crate) struct AfterQuery {
    /// Only notices after this `seq`.
    #[serde(default)]
    after: Option<u64>,
}

/// The notices of the last while, oldest first (the app's notification panel).
#[utoipa::path(get, path = "/v1/owner-notices", tag = "notifications", security(("bearer" = [])),
    params(("after" = Option<u64>, Query, description = "Only notices with a larger seq")),
    responses((status = 200, body = Vec<OwnerNotice>)))]
pub(crate) async fn list_notices(
    State(s): State<AppState>,
    Query(q): Query<AfterQuery>,
) -> Json<Vec<OwnerNotice>> {
    Json(s.owner_notify().recent(q.after.unwrap_or(0)))
}

/// The owner's push settings.
#[utoipa::path(get, path = "/v1/owner-notices/settings", tag = "notifications",
    security(("bearer" = [])), responses((status = 200, body = OwnerNotifySettings)))]
pub(crate) async fn get_notice_settings(State(s): State<AppState>) -> Json<OwnerNotifySettings> {
    Json(s.owner_notify().settings())
}

/// Change what is pushed to Telegram, Discord and other channels, and the quiet hours (owner
/// only).
#[utoipa::path(put, path = "/v1/owner-notices/settings", tag = "notifications",
    security(("bearer" = [])), request_body = OwnerNotifySettings,
    responses((status = 200, body = OwnerNotifySettings), (status = 400, body = ErrorBody),
              (status = 403, body = ErrorBody)))]
pub(crate) async fn put_notice_settings(
    State(s): State<AppState>,
    headers: HeaderMap,
    ApiJson(input): ApiJson<OwnerNotifySettings>,
) -> Result<Json<OwnerNotifySettings>, ApiError> {
    s.require_owner(&headers)?;
    input.validate().map_err(ApiError::bad_request)?;
    s.owner_notify()
        .set_settings(input.clone(), storage::now_ms())
        .map_err(ApiError::internal)?;
    let _ = s.storage().append_audit(
        s.user_id(),
        None,
        "user",
        "owner_notify.settings",
        &json!({"finished": input.finished, "failed": input.failed, "blocked": input.blocked,
                "progress": input.progress, "quiet_hours": input.quiet_hours.is_some()}),
    );
    Ok(Json(input))
}

/// The pushed notices as server-sent events (`event: notice`, one JSON `OwnerNotice` per event),
/// for the Telegram and Discord bridges. `after` replays the pushed notices missed since that
/// `seq`; without it the stream starts with what is new from now on. A `hello` event first says
/// the newest pushed `seq`.
#[utoipa::path(get, path = "/v1/owner-notices/stream", tag = "notifications",
    security(("bearer" = [])),
    params(("after" = Option<u64>, Query, description = "Replay pushed notices with a larger seq")),
    responses((status = 200, content_type = "text/event-stream")))]
pub(crate) async fn stream_notices(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<AfterQuery>,
) -> Result<Sse<impl futures::Stream<Item = Result<SseEvent, std::convert::Infallible>>>, ApiError>
{
    // Owner only like the rest of the notice routes; a stream that never ends cannot sit in the
    // owner-route table, whose sweep test waits for each response to finish.
    s.require_owner(&headers)?;
    let hub = s.owner_notify();
    let (replay, rx) = hub.subscribe_push(q.after);
    let hello = SseEvent::default()
        .event("hello")
        .data(json!({"latest_seq": hub.latest_push_seq()}).to_string());
    let shutdown = s.shutdown_token();
    let first: VecDeque<SseEvent> = std::iter::once(hello)
        .chain(replay.iter().filter_map(notice_event))
        .collect();
    if let Some(last) = replay.last() {
        hub.mark_delivered(last.seq);
    }
    let state = s.clone();
    let stream = futures::stream::unfold(
        (first, rx, shutdown),
        move |(mut first, mut rx, shutdown)| {
            let state = state.clone();
            async move {
                if let Some(ev) = first.pop_front() {
                    return Some((Ok(ev), (first, rx, shutdown)));
                }
                loop {
                    tokio::select! {
                        () = shutdown.cancelled() => return None,
                        got = rx.recv() => match got {
                            Ok(n) => {
                                if let Some(ev) = notice_event(&n) {
                                    state.owner_notify().mark_delivered(n.seq);
                                    return Some((Ok(ev), (first, rx, shutdown)));
                                }
                            }
                            // The bridge fell behind: say so and let it replay with `after`.
                            Err(broadcast::error::RecvError::Lagged(_)) => {
                                let ev = SseEvent::default().event("lagged").data("{}");
                                return Some((Ok(ev), (first, rx, shutdown)));
                            }
                            Err(broadcast::error::RecvError::Closed) => return None,
                        },
                    }
                }
            }
        },
    );
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

fn notice_event(n: &OwnerNotice) -> Option<SseEvent> {
    serde_json::to_string(n).ok().map(|d| {
        SseEvent::default()
            .event("notice")
            .id(n.seq.to_string())
            .data(d)
    })
}

/// What the owner answers to the questions of one notice.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NoticeAnswersInput {
    /// One entry per answered or skipped question, by the question's `key`.
    pub answers: Vec<MissionAnswerInput>,
}

/// How many questions an answer took.
#[derive(Debug, Serialize, ToSchema)]
pub struct NoticeAnswersDto {
    /// Questions that went back to the bot.
    pub answered: u32,
    /// Questions skipped.
    pub skipped: u32,
}

/// Answer the questions of a blocked notice from a chat (owner only). A blocked mission's
/// answers release its items and run it again; a background run's question is answered the way
/// the app's question card does. Never an approval, and never a one-time code.
#[utoipa::path(post, path = "/v1/owner-notices/{id}/answers", tag = "notifications",
    security(("bearer" = [])),
    params(("id" = String, Path, description = "Notice id")),
    request_body = NoticeAnswersInput,
    responses((status = 200, body = NoticeAnswersDto), (status = 400, body = ErrorBody),
              (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
              (status = 409, body = ErrorBody)))]
pub(crate) async fn answer_notice(
    State(s): State<AppState>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
    ApiJson(input): ApiJson<NoticeAnswersInput>,
) -> Result<Json<NoticeAnswersDto>, ApiError> {
    s.require_owner(&headers)?;
    let notice = s
        .owner_notify()
        .get(&id)
        .ok_or_else(|| ApiError::not_found("this notice is gone"))?;
    if notice.kind != NoticeKind::Blocked || notice.questions.is_empty() {
        return Err(ApiError::conflict("this notice asks nothing"));
    }
    if input.answers.is_empty() || input.answers.len() > MAX_LISTED {
        return Err(ApiError::bad_request("send at least one answer"));
    }
    // Only the questions the notice listed can be answered through it.
    for a in &input.answers {
        if !notice
            .questions
            .iter()
            .take(MAX_LISTED)
            .any(|q| q.key == a.key)
        {
            return Err(ApiError::bad_request("that question is not in this notice"));
        }
    }
    let mut seen = HashSet::new();
    if !input.answers.iter().all(|a| seen.insert(a.key.as_str())) {
        return Err(ApiError::bad_request("answer each question once"));
    }
    if notice.source == NoticeSource::Run {
        if input.answers.iter().any(|a| a.skip) {
            return Err(ApiError::bad_request(
                "a question of a running task needs an answer in words",
            ));
        }
        if input.answers.iter().any(|a| a.answer.trim().is_empty()) {
            return Err(ApiError::bad_request("write an answer"));
        }
        // A question of a run can only be answered while the run still waits for it.
        for a in &input.answers {
            let waiting =
                Uuid::parse_str(&a.key).is_ok_and(|call| s.agent().is_pending(ToolCallId(call)));
            if !waiting {
                return Err(ApiError::conflict(
                    "that question is not waiting for an answer any more",
                ));
            }
        }
    }
    s.owner_notify()
        .claim_answers(&notice)
        .map_err(ApiError::conflict)?;
    let done = answer_questions(&s, &notice, input);
    if done.is_err() {
        s.owner_notify().release_answers(&notice.id);
    }
    done.map(Json)
}

/// Hand the answers of an owner to the mission or run the notice is about.
fn answer_questions(
    s: &AppState,
    notice: &OwnerNotice,
    input: NoticeAnswersInput,
) -> Result<NoticeAnswersDto, ApiError> {
    match notice.source {
        NoticeSource::Mission => {
            let mission = notice
                .mission_id
                .as_deref()
                .and_then(|m| Uuid::parse_str(m).ok())
                .ok_or_else(|| ApiError::conflict("this notice has no mission"))?;
            let r = crate::mission_routes::apply_answers(
                s,
                mission,
                &MissionAnswersInput {
                    answers: input.answers,
                },
            )?;
            Ok(NoticeAnswersDto {
                answered: r.released,
                skipped: r.skipped,
            })
        }
        NoticeSource::Run => {
            let mut answered = 0;
            for a in &input.answers {
                let call = Uuid::parse_str(&a.key)
                    .map_err(|_| ApiError::bad_request("that question is not in this notice"))?;
                s.agent().answer_question(ToolCallId(call), &a.answer)?;
                answered += 1;
            }
            Ok(NoticeAnswersDto {
                answered,
                skipped: 0,
            })
        }
    }
}

/// Try again: resume a blocked mission, or retry the failed occurrence of a scheduled task
/// (owner only). The daemon resolves the target from the notice; a chat never names one.
#[utoipa::path(post, path = "/v1/owner-notices/{id}/retry", tag = "notifications",
    security(("bearer" = [])),
    params(("id" = String, Path, description = "Notice id")),
    responses((status = 202, description = "started"), (status = 403, body = ErrorBody),
              (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)))]
pub(crate) async fn retry_notice(
    State(s): State<AppState>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> Result<axum::http::StatusCode, ApiError> {
    s.require_owner(&headers)?;
    let notice = s
        .owner_notify()
        .get(&id)
        .ok_or_else(|| ApiError::not_found("this notice is gone"))?;
    if !notice.can_retry {
        return Err(ApiError::conflict("there is nothing to retry here"));
    }
    match notice.source {
        NoticeSource::Mission => {
            let mission = notice
                .mission_id
                .as_deref()
                .and_then(|m| Uuid::parse_str(m).ok())
                .ok_or_else(|| ApiError::conflict("this notice has no mission"))?;
            crate::mission_routes::resume_for_owner(&s, mission)?;
        }
        NoticeSource::Run => {
            let run = notice
                .run_id
                .as_deref()
                .and_then(|r| Uuid::parse_str(r).ok())
                .ok_or_else(|| ApiError::conflict("this notice has no run"))?;
            let run = s
                .storage()
                .get_run(RunId(run))?
                .ok_or_else(|| ApiError::not_found("the run is gone"))?;
            let kind = scheduled_kind(&run.kind)
                .and_then(|k| k.due_ms.map(|d| (k.schedule, d)))
                .ok_or_else(|| ApiError::conflict("this run cannot be retried"))?;
            let _ = crate::routes::retry_schedule_occurrence(
                State(s.clone()),
                AxumPath((kind.0.to_string(), kind.1)),
            )
            .await?;
        }
    }
    Ok(axum::http::StatusCode::ACCEPTED)
}

#[cfg(test)]
mod tests;
