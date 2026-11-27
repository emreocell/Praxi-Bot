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

use std::collections::{HashMap, VecDeque};
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
/// Questions and approvals of one run are batched into one notice within this window.
const BATCH_WINDOW: Duration = Duration::from_secs(4);
/// How often held notices are checked against the quiet window.
const HELD_TICK: Duration = Duration::from_secs(30);
/// Longest text kept per notice line.
const TEXT_MAX: usize = 300;

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
            if parse_hhmm(&q.start).is_none() || parse_hhmm(&q.end).is_none() {
                return Err("quiet hours need times like 22:00 and 08:00".into());
            }
            if !q.timezone.trim().is_empty() && q.timezone.trim().parse::<chrono_tz::Tz>().is_err()
            {
                return Err(format!("unknown time zone {:?}", q.timezone));
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

fn save_settings(path: &Path, s: &OwnerNotifySettings) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(s).unwrap_or_default())?;
    std::fs::rename(&tmp, path)
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
}

/// The notice log and the push gate.
pub(crate) struct Hub {
    state: Mutex<HubState>,
    tx: broadcast::Sender<OwnerNotice>,
    path: Option<PathBuf>,
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
        let state = HubState {
            settings: load_settings(&path),
            ..HubState::default()
        };
        Self {
            state: Mutex::new(state),
            tx: broadcast::channel(64).0,
            path: Some(path),
        }
    }

    #[cfg(test)]
    pub(crate) fn in_memory() -> Self {
        Self {
            state: Mutex::new(HubState::default()),
            tx: broadcast::channel(64).0,
            path: None,
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
            st.log.pop_front();
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
        let mut st = self.state.lock();
        Self::release_locked(&mut st, &self.tx, now_ms);
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

    /// The pushed notices after `after`, and a receiver for the next ones, taken together so
    /// none is missed or repeated.
    pub(crate) fn subscribe_push(
        &self,
        after: Option<u64>,
    ) -> (Vec<OwnerNotice>, broadcast::Receiver<OwnerNotice>) {
        let st = self.state.lock();
        let rx = self.tx.subscribe();
        // No cursor: a bridge that has never connected starts with what is new from now on.
        let replay = after.map_or_else(Vec::new, |after| {
            st.pushed
                .iter()
                .filter(|n| n.seq > after)
                .cloned()
                .collect()
        });
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
        S::Running | S::Paused | S::Cancelled => return None,
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

    /// The last thing the bot said in `run`, clipped: what a background run "did".
    fn run_result_text(&self, run: &storage::Run) -> Option<String> {
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
            .map(|m| clip(&m.content, 500))
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
            if let Some(text) = self.run_result_text(&run) {
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

/// What one run is waiting for, collected over a short window.
#[derive(Default)]
struct RunWants {
    questions: Vec<NoticeItem>,
    app_only: Vec<String>,
}

// ----------------------------------------------------------------------------- the watcher

/// Follow the daemon-wide event feed and turn the endings and the questions of scheduled runs
/// into notices; also releases what quiet hours held back. Ends with `cancel`.
pub(crate) async fn watch(
    weak: WeakAppState,
    mut rx: broadcast::Receiver<StreamItem>,
    cancel: CancellationToken,
) {
    let mut pending: HashMap<RunId, (tokio::time::Instant, RunWants)> = HashMap::new();
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
                        pending.remove(&f.run_id);
                        state.notify_run_finished(f.run_id, f.error.as_deref());
                    }
                    Event::QuestionAsked(q) => {
                        let entry = pending.entry(q.run_id).or_insert_with(|| {
                            (tokio::time::Instant::now() + BATCH_WINDOW, RunWants::default())
                        });
                        entry.1.questions.push(NoticeItem {
                            key: q.call_id.to_string(),
                            title: clip(&q.question, 80),
                            text: Some(clip(&q.question, TEXT_MAX)),
                        });
                    }
                    Event::ApprovalNeeded(a) => {
                        let entry = pending.entry(a.run_id).or_insert_with(|| {
                            (tokio::time::Instant::now() + BATCH_WINDOW, RunWants::default())
                        });
                        entry.1.app_only.push(clip(&format!("{}: {}", a.tool_name, a.summary), TEXT_MAX));
                    }
                    Event::SecretRequested(s) => {
                        let entry = pending.entry(s.run_id).or_insert_with(|| {
                            (tokio::time::Instant::now() + BATCH_WINDOW, RunWants::default())
                        });
                        entry.1.app_only.push("a secret is requested (type it in the app)".to_owned());
                    }
                    Event::HandoverRequested(h) => {
                        let entry = pending.entry(h.run_id).or_insert_with(|| {
                            (tokio::time::Instant::now() + BATCH_WINDOW, RunWants::default())
                        });
                        entry.1.app_only.push("the page needs you live (a code or a CAPTCHA)".to_owned());
                    }
                    _ => {}
                }
            }
            _ = tick.tick() => {
                let Some(state) = weak.upgrade() else { return };
                let now = tokio::time::Instant::now();
                let due: Vec<RunId> = pending
                    .iter()
                    .filter(|(_, (at, _))| *at <= now)
                    .map(|(r, _)| *r)
                    .collect();
                for run in due {
                    if let Some((_, wants)) = pending.remove(&run) {
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
    let stream = futures::stream::unfold(
        (first, rx, shutdown),
        |(mut first, mut rx, shutdown)| async move {
            if let Some(ev) = first.pop_front() {
                return Some((Ok(ev), (first, rx, shutdown)));
            }
            loop {
                tokio::select! {
                    () = shutdown.cancelled() => return None,
                    got = rx.recv() => match got {
                        Ok(n) => {
                            if let Some(ev) = notice_event(&n) {
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
    match notice.source {
        NoticeSource::Mission => {
            let mission = notice
                .mission_id
                .as_deref()
                .and_then(|m| Uuid::parse_str(m).ok())
                .ok_or_else(|| ApiError::conflict("this notice has no mission"))?;
            let r = crate::mission_routes::apply_answers(
                &s,
                mission,
                &MissionAnswersInput {
                    answers: input.answers,
                },
            )?;
            Ok(Json(NoticeAnswersDto {
                answered: r.released,
                skipped: r.skipped,
            }))
        }
        NoticeSource::Run => {
            if input.answers.iter().any(|a| a.skip) {
                return Err(ApiError::bad_request(
                    "a question of a running task needs an answer in words",
                ));
            }
            let mut answered = 0;
            for a in &input.answers {
                if a.answer.trim().is_empty() {
                    return Err(ApiError::bad_request("write an answer"));
                }
                let call = Uuid::parse_str(&a.key)
                    .map_err(|_| ApiError::bad_request("that question is not in this notice"))?;
                s.agent().answer_question(ToolCallId(call), &a.answer)?;
                answered += 1;
            }
            Ok(Json(NoticeAnswersDto {
                answered,
                skipped: 0,
            }))
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
