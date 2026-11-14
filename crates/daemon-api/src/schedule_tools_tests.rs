#![allow(
    clippy::too_many_lines,
    clippy::assert_is_empty,
    clippy::many_single_char_names
)]
//! The daemon side of the bot schedule tools.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use agent_core::{AgentLoop, LoopConfig, Providers};
use builtin_tools::{
    SCHEDULE_TOOL_NAMES, ScheduleCaller, ScheduleChange, ScheduleHost, ScheduleRequest,
    ScheduleView,
};
use chrono::TimeZone;
use protocol::{BotId, ChatId, RunId, UserId};
use scheduler::{MisfirePolicy, Payload, Schedule, ScheduleOrigin, ScheduleSpec, ScheduleStore};
use serde_json::{Value, json};
use storage::{Bot, Chat, Run, Storage, User, now_ms};
use tokio::sync::broadcast;
use tool_registry::{ToolContext, ToolRegistry};

use crate::dto::{ScheduleChangeKind, ScheduleChanged};
use crate::fake::FakeProvider;
use crate::schedule_tools::sha256_hex as sha;
use crate::schedule_tools::{DaemonScheduleHost, ScheduleLimits, load_schedule_limits, upcoming};
use crate::{AppState, DaemonConfig};

struct Fx {
    host: DaemonScheduleHost,
    store: Arc<ScheduleStore>,
    storage: Arc<Storage>,
    user: UserId,
    a: BotId,
    b: BotId,
    chat_a: ChatId,
    run_a: RunId,
    clock: Arc<AtomicI64>,
    events: broadcast::Receiver<ScheduleChanged>,
    dir: tempfile::TempDir,
}

fn limits(max: usize, min_secs: u64) -> ScheduleLimits {
    ScheduleLimits {
        max_per_bot: max,
        min_interval_secs: min_secs,
        timezone: Some("Europe/Istanbul".into()),
    }
}

fn utc_ms(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> i64 {
    chrono::Utc
        .with_ymd_and_hms(y, mo, d, h, mi, 0)
        .unwrap()
        .timestamp_millis()
}

fn fx(l: ScheduleLimits) -> Fx {
    fx_at(l, now_ms(), true)
}

/// `known_user = false` builds a host whose user is not in the database, so
/// every audit write fails.
fn fx_at(l: ScheduleLimits, start_ms: i64, known_user: bool) -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let storage = Arc::new(Storage::open(dir.path()).unwrap());
    let user = UserId::new();
    storage
        .insert_user(&User {
            id: user,
            name: "u".into(),
            created_at_ms: now_ms(),
        })
        .unwrap();
    let mk_bot = |name: &str| {
        let id = BotId::new();
        storage
            .insert_bot(&Bot {
                id,
                user_id: user,
                name: name.into(),
                config: json!({}),
                created_at_ms: now_ms(),
            })
            .unwrap();
        id
    };
    let (a, b) = (mk_bot("a"), mk_bot("b"));
    let chat_a = ChatId::new();
    storage
        .insert_chat(&Chat {
            id: chat_a,
            user_id: user,
            bot_id: a,
            title: "chat".into(),
            created_at_ms: now_ms(),
        })
        .unwrap();
    let run_a = RunId::new();
    storage
        .insert_run(&Run {
            id: run_a,
            user_id: user,
            bot_id: a,
            chat_id: Some(chat_a),
            kind: "turn".into(),
            state: "running".into(),
            error: None,
            created_at_ms: now_ms(),
            updated_at_ms: now_ms(),
            parent_run_id: None,
            spec: None,
            computer_target: None,
            computer_node: None,
            computer_node_name: None,
            paused_at_ms: None,
            pause_reason: None,
        })
        .unwrap();
    let store = Arc::new(ScheduleStore::open(dir.path()).unwrap());
    let (tx, events) = broadcast::channel(64);
    let clock = Arc::new(AtomicI64::new(start_ms));
    let c = Arc::clone(&clock);
    let host_user = if known_user { user } else { UserId::new() };
    let host = DaemonScheduleHost::with_clock(
        Arc::clone(&store),
        Arc::clone(&storage),
        host_user,
        tx,
        l,
        Arc::new(move || c.load(Ordering::SeqCst)),
    );
    Fx {
        host,
        store,
        storage,
        user,
        a,
        b,
        chat_a,
        run_a,
        clock,
        events,
        dir,
    }
}

impl Fx {
    fn caller_a(&self) -> ScheduleCaller {
        ScheduleCaller {
            bot_id: self.a.to_string(),
            run_id: self.run_a.to_string(),
        }
    }
    fn caller_b(&self) -> ScheduleCaller {
        ScheduleCaller {
            bot_id: self.b.to_string(),
            run_id: String::new(),
        }
    }
    fn advance(&self, ms: i64) {
        self.clock.fetch_add(ms, Ordering::SeqCst);
    }
    /// The approval flow: preview (what the card shows), then apply.
    fn make(&self, c: &ScheduleCaller, req: &ScheduleRequest) -> Result<ScheduleView, String> {
        self.host.preview(c, &ScheduleChange::Create(req.clone()))?;
        self.host.create(c, req)
    }
    fn edit(
        &self,
        c: &ScheduleCaller,
        id: &str,
        req: &ScheduleRequest,
    ) -> Result<ScheduleView, String> {
        self.host.preview(
            c,
            &ScheduleChange::Update {
                id: id.into(),
                req: req.clone(),
            },
        )?;
        self.host.update(c, id, req)
    }
    fn toggle(&self, c: &ScheduleCaller, id: &str, on: bool) -> Result<ScheduleView, String> {
        let ch = if on {
            ScheduleChange::Resume { id: id.into() }
        } else {
            ScheduleChange::Pause { id: id.into() }
        };
        self.host.preview(c, &ch)?;
        self.host.set_enabled(c, id, on)
    }
    fn remove(&self, c: &ScheduleCaller, id: &str) -> Result<ScheduleView, String> {
        self.host
            .preview(c, &ScheduleChange::Delete { id: id.into() })?;
        self.host.delete(c, id)
    }
    /// A schedule not made by a bot tool.
    fn insert(
        &self,
        bot: BotId,
        origin: ScheduleOrigin,
        spec: ScheduleSpec,
        payload: Payload,
    ) -> Schedule {
        let mut sc = Schedule::new(
            bot,
            None,
            "made elsewhere",
            spec,
            payload,
            MisfirePolicy::default(),
            now_ms(),
        )
        .unwrap();
        sc.origin = origin;
        self.store.insert(&sc).unwrap();
        sc
    }
    fn audit(&self) -> Vec<storage::AuditEntry> {
        self.storage
            .list_audit(self.user)
            .unwrap()
            .into_iter()
            .filter(|e| e.action.starts_with("schedule."))
            .collect()
    }
}

fn req(name: &str, prompt: &str) -> ScheduleRequest {
    ScheduleRequest {
        name: Some(name.into()),
        prompt: Some(prompt.into()),
        ..ScheduleRequest::default()
    }
}

fn when(name: &str, w: &str) -> ScheduleRequest {
    ScheduleRequest {
        when: Some(w.into()),
        ..req(name, "do the thing")
    }
}

fn cron(c: &str) -> ScheduleRequest {
    ScheduleRequest {
        cron: Some(c.into()),
        ..req("n", "p")
    }
}

fn patch(f: impl FnOnce(&mut ScheduleRequest)) -> ScheduleRequest {
    let mut r = ScheduleRequest::default();
    f(&mut r);
    r
}

fn user_every_hour() -> ScheduleSpec {
    ScheduleSpec::Every { seconds: 3600 }
}

fn text(t: &str) -> Payload {
    Payload::AgentPrompt { text: t.into() }
}

// ---------------------------------------------------------------- creation

#[test]
fn natural_language_creation_uses_the_users_timezone_and_previews_three_firings() {
    let f = fx(limits(20, 300));
    let v = f
        .make(&f.caller_a(), &when("Standup", "every weekday at 10:30"))
        .unwrap();
    assert_eq!(v.schedule, "Every weekday at 10:30");
    assert_eq!(v.timezone, "Europe/Istanbul");
    assert_eq!(v.prompt, "do the thing");
    assert!(v.enabled);
    assert_eq!((v.origin.as_str(), v.editable), ("bot", true));
    assert_eq!(v.next_runs.len(), 3);
    for t in &v.next_runs {
        assert!(t.contains("T10:30:00+03:00"), "{t}");
    }
    let sc = f.store.list_by_bot(f.a).unwrap().remove(0);
    assert_eq!(sc.chat_id, Some(f.chat_a));
    assert_eq!(sc.origin, ScheduleOrigin::Bot, "origin is recorded");
    assert!(matches!(sc.payload, Payload::AgentPrompt { ref text } if text == "do the thing"));
    assert!(sc.next_run_at.is_some());
}

#[test]
fn explicit_timezone_and_turkish_phrase() {
    let f = fx(limits(20, 300));
    let mut r = when("Sabah", "her gün 09:00");
    r.tz = Some("America/New_York".into());
    let v = f.make(&f.caller_a(), &r).unwrap();
    assert_eq!(v.timezone, "America/New_York");
    assert_eq!(v.schedule, "Every day at 09:00");
    for t in &v.next_runs {
        assert!(t.contains("T09:00:00-0"), "{t}");
    }
    r.tz = Some("Mars/Olympus".into());
    assert!(
        f.make(&f.caller_a(), &r)
            .unwrap_err()
            .contains("unknown timezone")
    );
}

#[test]
fn cron_creation_with_and_without_embedded_timezone() {
    let f = fx(limits(20, 300));
    let v = f.make(&f.caller_a(), &cron("30 10 * * 1-5")).unwrap();
    assert_eq!(v.schedule, "Every weekday at 10:30");
    assert_eq!(v.timezone, "Europe/Istanbul");
    let v = f
        .make(&f.caller_a(), &cron("0 8 * * 1,3 Europe/London"))
        .unwrap();
    assert_eq!(v.timezone, "Europe/London");
    assert_eq!(v.schedule, "Every Monday, Wednesday at 08:00");
    assert!(
        f.make(&f.caller_a(), &cron("not a cron"))
            .unwrap_err()
            .contains("cron")
    );
}

#[test]
fn one_off_ok_and_when_and_cron_are_exclusive() {
    let f = fx(limits(20, 300));
    let v = f.make(&f.caller_a(), &when("Once", "in 2 hours")).unwrap();
    assert!(v.schedule.starts_with("Once, at "), "{}", v.schedule);
    assert_eq!(v.next_runs.len(), 1);
    let mut both = when("x", "daily");
    both.cron = Some("0 9 * * *".into());
    assert!(
        f.make(&f.caller_a(), &both)
            .unwrap_err()
            .contains("not both")
    );
    assert!(
        f.make(&f.caller_a(), &req("x", "y"))
            .unwrap_err()
            .contains("`when`")
    );
    assert!(
        f.make(&f.caller_a(), &when("x", "whenever you feel like it"))
            .unwrap_err()
            .contains("cannot understand")
    );
}

// ---------------------------------------------------------------- isolation & ownership

#[test]
fn bot_b_cannot_see_or_touch_bot_a() {
    let f = fx(limits(20, 300));
    let v = f.make(&f.caller_a(), &when("A's", "daily")).unwrap();
    assert!(f.host.list(&f.caller_b()).unwrap().is_empty());
    assert_eq!(f.host.list(&f.caller_a()).unwrap().len(), 1);
    let b = f.caller_b();
    let p = patch(|r| r.prompt = Some("hijack".into()));
    let changes = [
        ScheduleChange::Update {
            id: v.id.clone(),
            req: p.clone(),
        },
        ScheduleChange::Pause { id: v.id.clone() },
        ScheduleChange::Resume { id: v.id.clone() },
        ScheduleChange::Delete { id: v.id.clone() },
    ];
    for ch in &changes {
        let e = f.host.preview(&b, ch).unwrap_err();
        assert!(e.contains("no schedule with id"), "{e}");
    }
    for e in [
        f.host.update(&b, &v.id, &p).unwrap_err(),
        f.host.set_enabled(&b, &v.id, false).unwrap_err(),
        f.host.delete(&b, &v.id).unwrap_err(),
    ] {
        assert!(
            e.contains("no schedule with id")
                || e.contains("not previewed")
                || e.contains("changed"),
            "{e}"
        );
    }
    let sc = f.store.list_by_bot(f.a).unwrap().remove(0);
    assert!(sc.enabled);
    assert_eq!(sc.name, "A's");
    let e = f.remove(&b, &uuid::Uuid::now_v7().to_string()).unwrap_err();
    assert!(e.contains("no schedule with id"));
    assert!(
        f.audit()
            .iter()
            .all(|e| e.detail["bot_id"] == f.a.to_string())
    );
}

#[test]
fn a_bot_may_read_but_never_change_schedules_the_user_made() {
    let f = fx(limits(20, 300));
    let user_made = f.insert(
        f.a,
        ScheduleOrigin::User,
        user_every_hour(),
        text("user's job"),
    );
    let id = user_made.id.0.to_string();
    let listed = f.host.list(&f.caller_a()).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].origin, "user");
    assert!(!listed[0].editable, "marked read-only");
    assert_eq!(listed[0].prompt, "user's job");
    let c = f.caller_a();
    for ch in [
        ScheduleChange::Update {
            id: id.clone(),
            req: patch(|r| r.prompt = Some("x".into())),
        },
        ScheduleChange::Pause { id: id.clone() },
        ScheduleChange::Resume { id: id.clone() },
        ScheduleChange::Delete { id: id.clone() },
    ] {
        let e = f.host.preview(&c, &ch).unwrap_err();
        assert!(
            e.contains("created by the user") && e.contains("only read"),
            "{e}"
        );
    }
    assert!(f.host.delete(&c, &id).is_err());
    assert!(f.host.set_enabled(&c, &id, false).is_err());
    let after = f.store.get(user_made.id).unwrap().unwrap();
    assert!(after.enabled);
    assert_eq!(after.revision, user_made.revision);
    assert!(f.audit().is_empty(), "nothing was changed or logged");
}

#[test]
fn a_bot_cannot_edit_or_resume_a_schedule_with_a_webhook_but_may_pause_or_delete_it() {
    let f = fx(limits(20, 300));
    let v = f.make(&f.caller_a(), &when("hooked", "daily")).unwrap();
    let id = scheduler::TaskId(uuid::Uuid::parse_str(&v.id).unwrap());
    f.toggle(&f.caller_a(), &v.id, false).unwrap();
    f.store
        .set_trigger(id, &scheduler::Trigger::Webhook { secret: "s".into() })
        .unwrap();
    for e in [
        f.edit(
            &f.caller_a(),
            &v.id,
            &patch(|r| r.prompt = Some("x".into())),
        )
        .unwrap_err(),
        f.toggle(&f.caller_a(), &v.id, true).unwrap_err(),
    ] {
        assert!(e.contains("webhook"), "{e}");
    }
    assert!(!f.store.get(id).unwrap().unwrap().enabled);
    f.remove(&f.caller_a(), &v.id).unwrap();
    assert!(f.store.get(id).unwrap().is_none());
}

// ---------------------------------------------------------------- limits

#[test]
fn quota_is_per_bot_and_counts_every_origin() {
    let f = fx(limits(2, 300));
    f.insert(
        f.a,
        ScheduleOrigin::User,
        ScheduleSpec::cron("0 9 * * *"),
        text("x"),
    );
    f.make(&f.caller_a(), &when("one", "daily")).unwrap();
    let e = f.make(&f.caller_a(), &when("two", "daily")).unwrap_err();
    assert!(
        e.contains("schedule limit reached") && e.contains("2 of 2"),
        "{e}"
    );
    f.make(&f.caller_b(), &when("b1", "daily")).unwrap();
    let mine = f.host.list(&f.caller_a()).unwrap();
    let id = mine.iter().find(|s| s.editable).unwrap().id.clone();
    f.remove(&f.caller_a(), &id).unwrap();
    f.make(&f.caller_a(), &when("again", "daily")).unwrap();
}

#[test]
fn default_limits_are_twenty_and_five_minutes() {
    let l = ScheduleLimits::default();
    assert_eq!((l.max_per_bot, l.min_interval_secs), (20, 300));
    let f = fx(ScheduleLimits {
        timezone: Some("UTC".into()),
        ..l
    });
    for i in 0..20 {
        f.make(&f.caller_a(), &when(&format!("s{i}"), "daily"))
            .unwrap();
    }
    assert!(
        f.make(&f.caller_a(), &when("s20", "daily"))
            .unwrap_err()
            .contains("20 of 20")
    );
}

#[test]
fn minimum_interval_applies_to_phrases_and_cron_including_dense_bursts() {
    let f = fx(limits(20, 300));
    for (r, what) in [
        (when("x", "every minute"), "1 minute"),
        (when("x", "every 2 minutes"), "2 minutes"),
        (cron("* * * * *"), "1 minute"),
        (cron("*/2 * * * *"), "2 minutes"),
        (cron("0,2 * * * *"), "2 minutes"),
        // One burst a year: every minute from 09:00 to 09:59 on 1 January.
        (cron("*/1 9 1 1 *"), "1 minute"),
    ] {
        let e = f.make(&f.caller_a(), &r).unwrap_err();
        assert!(e.contains("minimum interval is 5 minutes"), "{e}");
        assert!(e.contains(what), "{e} / {what}");
    }
    f.make(&f.caller_a(), &when("ok", "every 5 minutes"))
        .unwrap();
    f.make(&f.caller_a(), &cron("*/5 * * * *")).unwrap();
    f.make(&f.caller_a(), &cron("0 * * * *")).unwrap();
    let f = fx(limits(20, 60));
    f.make(&f.caller_a(), &when("m", "every minute")).unwrap();
    let f = fx(limits(20, 3600));
    let e = f
        .make(&f.caller_a(), &when("m", "every 30 minutes"))
        .unwrap_err();
    assert!(e.contains("minimum interval is 1 hour"), "{e}");
}

#[test]
fn every_below_the_minimum_is_rejected_for_the_raw_spec_too() {
    let f = fx(limits(20, 300));
    let spec = ScheduleSpec::Every { seconds: 299 };
    let e = f.host.check_interval(&spec, now_ms()).unwrap_err();
    assert!(e.contains("4 minutes") || e.contains("299 seconds"), "{e}");
    f.host
        .check_interval(&ScheduleSpec::Every { seconds: 300 }, now_ms())
        .unwrap();
}

#[test]
fn dst_transitions_neither_skip_the_preview_nor_create_a_sub_minimum_gap() {
    // 2027-03-14 02:00 America/New_York does not exist (spring forward);
    // 2027-11-07 01:30 happens twice (fall back).
    let ny = |expr: &str| ScheduleSpec::cron_tz(expr, "America/New_York");
    for (start, expr) in [
        (utc_ms(2027, 3, 13, 12, 0), "30 2 * * *"),
        (utc_ms(2027, 3, 14, 6, 30), "*/5 * * * *"),
        (utc_ms(2027, 11, 6, 12, 0), "30 1 * * *"),
        (utc_ms(2027, 11, 7, 4, 30), "*/5 * * * *"),
    ] {
        let f = fx_at(limits(20, 300), start, true);
        let spec = ny(expr);
        let times = upcoming(&spec, start, 3);
        assert_eq!(
            times.len(),
            3,
            "{expr}: preview must still list three firings"
        );
        f.host
            .check_interval(&spec, start)
            .unwrap_or_else(|e| panic!("{expr} at {start}: {e}"));
        let gaps: Vec<i64> = times.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(gaps.iter().all(|g| *g >= 300_000), "{expr}: {gaps:?}");
    }
    // Through the host: a daily 02:30 created the day before the gap works.
    let f = fx_at(limits(20, 300), utc_ms(2027, 3, 13, 12, 0), true);
    let mut r = cron("30 2 * * *");
    r.tz = Some("America/New_York".into());
    let v = f.make(&f.caller_a(), &r).unwrap();
    assert_eq!(v.next_runs.len(), 3);
}

// ---------------------------------------------------------------- update / pause / resume

#[test]
fn update_card_shows_old_and_new_and_changes_only_what_is_given() {
    let f = fx(limits(20, 300));
    let v = f.make(&f.caller_a(), &when("Standup", "daily")).unwrap();
    let p = f
        .host
        .preview(
            &f.caller_a(),
            &ScheduleChange::Update {
                id: v.id.clone(),
                req: patch(|r| {
                    r.prompt = Some("new prompt".into());
                    r.when = Some("every weekday at 7:15".into());
                }),
            },
        )
        .unwrap();
    assert_eq!(p.action, "update");
    assert_eq!(p.schedule_id.as_deref(), Some(v.id.as_str()));
    assert_eq!(
        (p.name.as_str(), p.prompt.as_str()),
        ("Standup", "new prompt")
    );
    assert_eq!(p.schedule, "Every weekday at 07:15");
    let prev = p.previous.expect("old values for old -> new");
    assert_eq!(prev.prompt, "do the thing");
    assert_eq!(prev.schedule, "Every day at 09:00");
    assert_eq!(prev.name, "Standup");
    // Apply what was previewed.
    let u = f
        .host
        .update(
            &f.caller_a(),
            &v.id,
            &patch(|r| {
                r.prompt = Some("new prompt".into());
                r.when = Some("every weekday at 7:15".into());
            }),
        )
        .unwrap();
    assert_eq!(
        (u.name.as_str(), u.prompt.as_str()),
        ("Standup", "new prompt")
    );
    assert_eq!(u.schedule, "Every weekday at 07:15");
    let e = f
        .edit(&f.caller_a(), &v.id, &patch(|r| r.tz = Some("UTC".into())))
        .unwrap_err();
    assert!(e.contains("`tz` only applies"), "{e}");
    let e = f
        .edit(
            &f.caller_a(),
            &v.id,
            &patch(|r| r.when = Some("every minute".into())),
        )
        .unwrap_err();
    assert!(e.contains("minimum interval"), "{e}");
}

#[test]
fn pause_resume_and_delete_cards_name_the_schedule_and_its_timing() {
    let f = fx(limits(20, 300));
    let v = f
        .make(&f.caller_a(), &when("Nightly", "every weekday at 10:30"))
        .unwrap();
    for (ch, action) in [
        (ScheduleChange::Pause { id: v.id.clone() }, "pause"),
        (ScheduleChange::Resume { id: v.id.clone() }, "resume"),
        (ScheduleChange::Delete { id: v.id.clone() }, "delete"),
    ] {
        let p = f.host.preview(&f.caller_a(), &ch).unwrap();
        assert_eq!(p.action, action);
        assert_eq!(p.name, "Nightly");
        assert_eq!(p.schedule, "Every weekday at 10:30");
        assert_eq!(p.schedule_id.as_deref(), Some(v.id.as_str()));
    }
}

#[test]
fn pause_and_resume_toggle_the_timer() {
    let f = fx(limits(20, 300));
    let v = f.make(&f.caller_a(), &when("n", "daily")).unwrap();
    let p = f.toggle(&f.caller_a(), &v.id, false).unwrap();
    assert!(!p.enabled && p.next_runs.is_empty());
    assert!(!f.store.list_by_bot(f.a).unwrap()[0].enabled);
    let r = f.toggle(&f.caller_a(), &v.id, true).unwrap();
    assert!(r.enabled);
    assert_eq!(r.next_runs.len(), 3);
    assert!(f.store.list_by_bot(f.a).unwrap()[0].next_run_at.is_some());
}

#[test]
fn pause_and_delete_work_without_an_approval_preview_but_resume_does_not() {
    let f = fx(limits(20, 300));
    let v = f.make(&f.caller_a(), &when("n", "daily")).unwrap();
    // The loop only previews when it asks; pause / delete may run unasked.
    assert!(
        !f.host
            .set_enabled(&f.caller_a(), &v.id, false)
            .unwrap()
            .enabled
    );
    let e = f.host.set_enabled(&f.caller_a(), &v.id, true).unwrap_err();
    assert!(e.contains("not previewed"), "{e}");
    assert!(
        f.host
            .create(&f.caller_a(), &when("x", "daily"))
            .unwrap_err()
            .contains("not previewed")
    );
    f.host.delete(&f.caller_a(), &v.id).unwrap();
}

#[test]
fn the_interval_limit_is_applied_on_resume() {
    let f = fx(limits(20, 300));
    let sc = f.insert(
        f.a,
        ScheduleOrigin::Bot,
        ScheduleSpec::Every { seconds: 60 },
        text("p"),
    );
    let id = sc.id.0.to_string();
    f.host.set_enabled(&f.caller_a(), &id, false).unwrap();
    let e = f.toggle(&f.caller_a(), &id, true).unwrap_err();
    assert!(e.contains("minimum interval"), "{e}");
    assert!(!f.store.get(sc.id).unwrap().unwrap().enabled);
}

#[test]
fn a_workflow_schedule_cannot_be_rewritten_by_a_bot() {
    let f = fx(limits(20, 300));
    let sc = f.insert(
        f.a,
        ScheduleOrigin::Bot,
        ScheduleSpec::cron("0 9 * * *"),
        Payload::Workflow {
            name: "w".into(),
            params: Value::Null,
        },
    );
    let id = sc.id.0.to_string();
    let e = f
        .edit(&f.caller_a(), &id, &patch(|r| r.prompt = Some("x".into())))
        .unwrap_err();
    assert!(e.contains("workflow or tool"), "{e}");
    assert!(!f.toggle(&f.caller_a(), &id, false).unwrap().enabled);
}

// ---------------------------------------------------------------- what is approved is what is stored

#[test]
fn a_relative_time_is_frozen_at_preview_not_recomputed_after_the_approval_wait() {
    let start = utc_ms(2026, 11, 12, 12, 0);
    let f = fx_at(limits(20, 300), start, true);
    let r = when("Once", "in 2 hours");
    let p = f
        .host
        .preview(&f.caller_a(), &ScheduleChange::Create(r.clone()))
        .unwrap();
    assert_eq!(p.next_runs.len(), 1);
    assert!(
        p.next_runs[0].starts_with("2026-11-12T17:00:00+03:00"),
        "{:?}",
        p.next_runs
    );
    f.advance(90 * 60_000); // the user takes 90 minutes to approve
    let v = f.host.create(&f.caller_a(), &r).unwrap();
    assert_eq!(v.next_runs, p.next_runs, "stored exactly what was shown");
    let sc = f.store.list_by_bot(f.a).unwrap().remove(0);
    assert_eq!(
        sc.spec,
        ScheduleSpec::At {
            unix_ms: start + 2 * 3_600_000
        }
    );
}

#[test]
fn a_one_shot_whose_frozen_time_passed_fails_clearly_before_any_change() {
    let start = utc_ms(2026, 11, 12, 12, 0);
    let f = fx_at(limits(20, 300), start, true);
    let r = when("Soon", "in 5 minutes");
    f.host
        .preview(&f.caller_a(), &ScheduleChange::Create(r.clone()))
        .unwrap();
    f.advance(20 * 60_000); // slow approval
    let e = f.host.create(&f.caller_a(), &r).unwrap_err();
    assert!(
        e.contains("already passed") && e.contains("nothing was created"),
        "{e}"
    );
    assert!(f.store.list_by_bot(f.a).unwrap().is_empty());
    assert!(f.audit().is_empty());
}

#[test]
fn an_edit_fails_if_the_row_changed_after_the_preview() {
    let f = fx(limits(20, 300));
    let v = f.make(&f.caller_a(), &when("n", "daily")).unwrap();
    let id = scheduler::TaskId(uuid::Uuid::parse_str(&v.id).unwrap());
    let r = patch(|r| r.prompt = Some("approved text".into()));
    f.host
        .preview(
            &f.caller_a(),
            &ScheduleChange::Update {
                id: v.id.clone(),
                req: r.clone(),
            },
        )
        .unwrap();
    // The user edits the schedule while the card is open.
    let mut cur = f.store.get(id).unwrap().unwrap();
    cur.name = "renamed by user".into();
    f.store
        .update_definition_and_rearm(&cur, None, now_ms())
        .unwrap();
    let e = f.host.update(&f.caller_a(), &v.id, &r).unwrap_err();
    assert!(
        e.contains("changed or was deleted") && e.contains("nothing was applied"),
        "{e}"
    );
    let after = f.store.get(id).unwrap().unwrap();
    assert_eq!(after.name, "renamed by user");
    assert!(matches!(after.payload, Payload::AgentPrompt { ref text } if text == "do the thing"));
    // Same for resume and delete.
    f.toggle(&f.caller_a(), &v.id, false).unwrap();
    f.host
        .preview(&f.caller_a(), &ScheduleChange::Resume { id: v.id.clone() })
        .unwrap();
    let mut cur = f.store.get(id).unwrap().unwrap();
    cur.name = "again".into();
    f.store
        .update_definition_and_rearm(&cur, None, now_ms())
        .unwrap();
    assert!(
        f.host
            .set_enabled(&f.caller_a(), &v.id, true)
            .unwrap_err()
            .contains("changed")
    );
    assert!(!f.store.get(id).unwrap().unwrap().enabled);
}

// ---------------------------------------------------------------- hygiene

#[test]
fn names_and_prompts_are_capped_and_stripped_of_control_and_bidi_characters() {
    let f = fx(limits(20, 300));
    let c = f.caller_a();
    let ok = |name: &str, prompt: &str| {
        let mut r = when(name, "daily");
        r.prompt = Some(prompt.into());
        f.host.preview(&c, &ScheduleChange::Create(r))
    };
    assert!(
        ok("n", "line one\n\tline two").is_ok(),
        "newline and tab are fine"
    );
    assert!(ok(&"n".repeat(80), "p").is_ok());
    assert!(
        ok(&"n".repeat(81), "p")
            .unwrap_err()
            .contains("limit is 80")
    );
    assert!(ok("n", &"p".repeat(4000)).is_ok());
    assert!(
        ok("n", &"p".repeat(4001))
            .unwrap_err()
            .contains("limit is 4000")
    );
    for bad in [
        "a\u{0}b",
        "a\u{1b}[31mb",
        "a\rb",
        "a\u{202E}b",
        "a\u{2066}b",
        "a\u{200F}b",
        // Invisible text in an instruction channel.
        "a\u{200B}b",
        "a\u{2060}b",
        "a\u{FEFF}b",
        "a\u{00AD}b",
        "a\u{2028}b",
        "a\u{2029}b",
        "a\u{E0100}b",
        "a\u{E0041}b", // a Unicode tag character
        "a\u{034F}b",
        // Invisible fillers that render as blank.
        "a\u{0890}b",
        "a\u{0891}b",
        "a\u{3164}b",
        "a\u{115F}b",
        "a\u{1160}b",
        "a\u{2800}b",
        "a\u{17B4}b",
        "a\u{17B5}b",
        "a\u{FFA0}b",
        "a\u{FFF0}b",
        "a\u{E0000}b",
    ] {
        let e = ok("n", bad).unwrap_err();
        assert!(e.contains("direction-override"), "{bad:?}: {e}");
        let e = ok(bad, "p").unwrap_err();
        assert!(e.contains("direction-override"), "{bad:?}: {e}");
    }
    // Emoji machinery gets its own plain explanation.
    for bad in [
        "a\u{200C}b",
        "a\u{200D}b",
        "a\u{FE0F}b",
        "\u{1F468}\u{200D}\u{1F4BB}",
    ] {
        for e in [ok("n", bad).unwrap_err(), ok(bad, "p").unwrap_err()] {
            assert!(
                e.contains("variation selector or a zero-width (non-)joiner")
                    && e.contains("not accepted in schedule prompts and names")
                    && e.contains("plain text"),
                "{bad:?}: {e}"
            );
        }
    }
    // Ordinary non-ASCII text is fine (homoglyphs are not filtered).
    assert!(ok("Günlük özet ☕", "Özetle: é ü ğ — ok").is_ok());
    assert!(ok("n", "   ").unwrap_err().contains("must not be empty"));
    assert!(ok("a\nb", "p").is_err(), "no newline in a name");
    // Updates are checked the same way.
    let v = f.make(&c, &when("n", "daily")).unwrap();
    let e = f
        .edit(&c, &v.id, &patch(|r| r.prompt = Some("x\u{202E}y".into())))
        .unwrap_err();
    assert!(e.contains("direction-override"), "{e}");
}

// ---------------------------------------------------------------- audit & events

#[test]
fn every_create_update_pause_delete_is_audited_with_bot_run_and_old_values() {
    let f = fx(limits(20, 300));
    let v = f.make(&f.caller_a(), &when("n", "daily")).unwrap();
    f.edit(
        &f.caller_a(),
        &v.id,
        &patch(|r| r.prompt = Some("a new prompt".into())),
    )
    .unwrap();
    f.toggle(&f.caller_a(), &v.id, false).unwrap();
    f.toggle(&f.caller_a(), &v.id, true).unwrap();
    f.remove(&f.caller_a(), &v.id).unwrap();
    let log = f.audit();
    let actions: Vec<(&str, &str)> = log
        .iter()
        .map(|e| (e.action.as_str(), e.detail["change"].as_str().unwrap()))
        .collect();
    assert_eq!(
        actions,
        [
            ("schedule.create", "created"),
            ("schedule.update", "updated"),
            ("schedule.update", "paused"),
            ("schedule.update", "resumed"),
            ("schedule.delete", "deleted"),
        ]
    );
    for e in &log {
        assert_eq!(e.actor, "bot");
        assert_eq!(e.run_id, Some(f.run_a));
        assert_eq!(e.detail["bot_id"], f.a.to_string());
        assert_eq!(e.detail["run_id"], f.run_a.to_string());
        assert_eq!(e.detail["schedule_id"], v.id);
        assert!(
            e.detail.get("prompt").is_none(),
            "the prompt text is not copied into audit"
        );
    }
    let upd = &log[1].detail;
    assert_eq!(upd["old_prompt_chars"], "do the thing".len());
    assert_eq!(upd["prompt_chars"], "a new prompt".len());
    assert_ne!(upd["old_prompt_sha256"], upd["prompt_sha256"]);
    // The known SHA-256 test vector, so this is not checking the function against itself.
    assert_eq!(
        sha("abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        upd["old_prompt_sha256"],
        sha("do the thing"),
        "SHA-256 of the old prompt"
    );
    assert_eq!(upd["prompt_sha256"], sha("a new prompt"));
    assert_eq!(upd["old_schedule"], "Every day at 09:00");
}

#[test]
fn a_failed_audit_write_fails_the_operation_before_anything_is_committed() {
    let f = fx_at(limits(20, 300), now_ms(), false);
    let e = f.make(&f.caller_a(), &when("n", "daily")).unwrap_err();
    assert!(
        e.contains("audit log could not be written") && e.contains("nothing was changed"),
        "{e}"
    );
    assert!(f.store.list_by_bot(f.a).unwrap().is_empty());
    // An existing bot schedule is neither edited, paused nor deleted.
    let sc = f.insert(f.a, ScheduleOrigin::Bot, user_every_hour(), text("keep"));
    let id = sc.id.0.to_string();
    f.store
        .set_trigger(sc.id, &scheduler::Trigger::Webhook { secret: "s".into() })
        .unwrap();
    assert!(
        f.edit(
            &f.caller_a(),
            &id,
            &patch(|r| r.prompt = Some("new".into()))
        )
        .is_err()
    );
    assert!(f.toggle(&f.caller_a(), &id, false).is_err());
    assert!(f.remove(&f.caller_a(), &id).is_err());
    let after = f.store.get(sc.id).unwrap().unwrap();
    assert!(after.enabled);
    assert!(matches!(after.payload, Payload::AgentPrompt { ref text } if text == "keep"));
    assert_eq!(after.revision, sc.revision);
    assert!(
        f.store.get_trigger(sc.id).unwrap().is_some(),
        "a refused delete removes nothing, the webhook trigger included"
    );
}

#[test]
fn a_successful_delete_takes_the_webhook_trigger_with_it() {
    let f = fx(limits(20, 300));
    let v = f.make(&f.caller_a(), &when("n", "daily")).unwrap();
    let id = scheduler::TaskId(uuid::Uuid::parse_str(&v.id).unwrap());
    f.store
        .set_trigger(id, &scheduler::Trigger::Webhook { secret: "s".into() })
        .unwrap();
    f.remove(&f.caller_a(), &v.id).unwrap();
    assert!(f.store.get(id).unwrap().is_none());
    assert!(f.store.get_trigger(id).unwrap().is_none());
}

#[test]
fn a_change_that_fails_after_the_audit_entry_leaves_a_failed_entry() {
    let f = fx(limits(20, 300));
    let sc = f.insert(f.a, ScheduleOrigin::Bot, user_every_hour(), text("p"));
    f.host.audit_failed(
        &f.caller_a(),
        "schedule.update",
        "updated",
        &sc,
        "disk full",
    );
    let log = f.storage.list_audit(f.user).unwrap();
    let e = log.iter().find(|e| e.action == "schedule.failed").unwrap();
    assert_eq!(e.detail["attempted"], "schedule.update");
    assert_eq!(e.detail["error"], "disk full");
    assert_eq!(e.detail["schedule_id"], sc.id.0.to_string());
    assert_eq!(e.actor, "bot");
}

#[test]
fn the_ui_is_told_about_every_change() {
    let mut f = fx(limits(20, 300));
    let v = f.make(&f.caller_a(), &when("n", "daily")).unwrap();
    f.edit(&f.caller_a(), &v.id, &patch(|r| r.name = Some("n2".into())))
        .unwrap();
    f.toggle(&f.caller_a(), &v.id, false).unwrap();
    f.remove(&f.caller_a(), &v.id).unwrap();
    let kinds: Vec<ScheduleChangeKind> = std::iter::from_fn(|| f.events.try_recv().ok())
        .map(|e| {
            assert_eq!(e.schedule_id.to_string(), v.id);
            e.kind
        })
        .collect();
    assert_eq!(
        kinds,
        [
            ScheduleChangeKind::Created,
            ScheduleChangeKind::Updated,
            ScheduleChangeKind::Toggled,
            ScheduleChangeKind::Deleted
        ]
    );
}

#[test]
fn preview_shows_exact_prompt_schedule_and_three_times_without_storing() {
    let f = fx(limits(20, 300));
    let pv = f
        .host
        .preview(
            &f.caller_a(),
            &ScheduleChange::Create(ScheduleRequest {
                when: Some("every weekday at 10:30".into()),
                ..req("Standup", "  Summarise my inbox.  ")
            }),
        )
        .unwrap();
    assert_eq!(pv.action, "create");
    assert_eq!(pv.prompt, "Summarise my inbox.");
    assert_eq!(pv.prompt_chars, 19);
    assert_eq!(pv.schedule, "Every weekday at 10:30");
    assert_eq!(pv.timezone, "Europe/Istanbul");
    assert_eq!(pv.next_runs.len(), 3);
    assert!(f.store.list_by_bot(f.a).unwrap().is_empty());
}

// ---------------------------------------------------------------- schedule.toml

#[test]
fn schedule_toml_sets_limits() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        load_schedule_limits(dir.path()).unwrap(),
        ScheduleLimits::default()
    );
    std::fs::write(
        dir.path().join("schedule.toml"),
        "max_per_bot = 3\nmin_interval_secs = 900\ntimezone = \"Europe/Istanbul\"\n",
    )
    .unwrap();
    let l = load_schedule_limits(dir.path()).unwrap();
    assert_eq!(
        (l.max_per_bot, l.min_interval_secs, l.timezone.as_deref()),
        (3, 900, Some("Europe/Istanbul"))
    );
}

#[test]
fn a_bad_schedule_toml_is_an_error_never_the_looser_defaults() {
    let dir = tempfile::tempdir().unwrap();
    for bad in [
        "max_per_bot = \"many\"",
        "min_interval_secs = 900\nbogus = 1",
        "timezone = \"Nope/Zone\"",
        "max_per_bot = 0",
        "max_per_bot = 100000",
        "min_interval_secs = 0",
        "min_interval_secs = 99999999999",
        "= nonsense",
    ] {
        std::fs::write(dir.path().join("schedule.toml"), bad).unwrap();
        let e = load_schedule_limits(dir.path()).unwrap_err();
        assert!(e.contains("schedule.toml"), "{bad}: {e}");
    }
    // A tightened file with one broken key does not become the defaults.
    std::fs::write(
        dir.path().join("schedule.toml"),
        "max_per_bot = 2\nmin_interval_secs = 0",
    )
    .unwrap();
    assert!(load_schedule_limits(dir.path()).is_err());
}

// ---------------------------------------------------------------- registration

#[tokio::test]
async fn enabling_registers_six_always_ask_tools_that_run_as_the_calling_bot() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Arc::new(Storage::open(dir.path()).unwrap());
    let agent = AgentLoop::new(
        Arc::clone(&storage),
        ToolRegistry::new(),
        Providers::single(Arc::new(FakeProvider)),
        LoopConfig::default(),
    );
    let state = AppState::new(agent.clone(), storage, DaemonConfig::new("t")).unwrap();
    assert_eq!(state.enable_schedule_tools(limits(20, 300)).unwrap(), 6);
    let mut events = state.subscribe_schedule_events();
    let reg = agent.tool_registry();
    for name in SCHEDULE_TOOL_NAMES {
        let t = reg.get(name).unwrap_or_else(|| panic!("{name} registered"));
        let gated = matches!(
            name,
            "schedule_create" | "schedule_update" | "schedule_resume"
        );
        assert_eq!(t.always_ask(), gated, "{name}");
    }
    let create = reg.get("schedule_create").unwrap();
    let args = json!({"name":"n","prompt":"p","when":"daily"});
    assert!(
        create
            .call(&ToolContext::default(), args.clone())
            .await
            .is_err()
    );
    let bot = BotId::new();
    state
        .storage()
        .insert_bot(&Bot {
            id: bot,
            user_id: state.user_id(),
            name: "b".into(),
            config: json!({}),
            created_at_ms: now_ms(),
        })
        .unwrap();
    let ctx = ToolContext {
        bot_id: Some(bot.to_string()),
        ..ToolContext::default()
    };
    // Without the approval preview nothing is stored.
    let e = create.call(&ctx, args.clone()).await.unwrap_err();
    assert!(e.to_string().contains("not previewed"), "{e}");
    assert!(
        state
            .schedule_store()
            .unwrap()
            .list_by_bot(bot)
            .unwrap()
            .is_empty()
    );
    // With it (what the loop does when it asks), the schedule lands, marked as a bot's.
    let details = create.approval_details(&ctx, &args).await.unwrap().unwrap();
    assert_eq!(details["action"], "create");
    create.call(&ctx, args).await.unwrap();
    let stored = state.schedule_store().unwrap().list_by_bot(bot).unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].origin, ScheduleOrigin::Bot);
    assert_eq!(events.try_recv().unwrap().kind, ScheduleChangeKind::Created);
    assert_eq!(state.enable_schedule_tools(limits(20, 300)).unwrap(), 0);
}

// ---------------------------------------------------------------- frozen store: bound to the approval

#[test]
fn a_denied_preview_is_discarded_so_the_next_card_is_what_gets_stored() {
    let start = utc_ms(2026, 11, 12, 12, 0);
    let f = fx_at(limits(20, 300), start, true);
    let r = when("Once", "in 2 hours");
    let ch = ScheduleChange::Create(r.clone());
    // T0: card shown (T0+2h), user denies.
    let p0 = f.host.preview(&f.caller_a(), &ch).unwrap();
    f.host.discard(&f.caller_a(), &ch, &p0.approval_ref);
    f.advance(30 * 60_000);
    // T0+30m: the same call again; the card shows T0+2h30m, user approves.
    let p = f.host.preview(&f.caller_a(), &ch).unwrap();
    assert!(
        p.next_runs[0].starts_with("2026-11-12T17:30:00+03:00"),
        "{:?}",
        p.next_runs
    );
    f.advance(60_000);
    let v = f.host.create(&f.caller_a(), &r).unwrap();
    assert_eq!(
        v.next_runs, p.next_runs,
        "stored what the second card showed"
    );
    let sc = f.store.list_by_bot(f.a).unwrap().remove(0);
    assert_eq!(
        sc.spec,
        ScheduleSpec::At {
            unix_ms: start + 150 * 60_000
        }
    );
}

#[test]
fn a_second_identical_preview_is_refused_while_the_first_is_pending() {
    let start = utc_ms(2026, 11, 12, 12, 0);
    let f = fx_at(limits(20, 300), start, true);
    let r = when("Once", "in 2 hours");
    let ch = ScheduleChange::Create(r.clone());
    let a = f.host.preview(&f.caller_a(), &ch).unwrap(); // card A: 14:00
    assert!(a.next_runs[0].starts_with("2026-11-12T17:00:00+03:00"));
    f.advance(30 * 60_000);
    // Card B (same call, 12:30) must not replace A's entry.
    let e = f.host.preview(&f.caller_a(), &ch).unwrap_err();
    assert!(
        e.contains(
            "an identical request is awaiting the user's decision; do not repeat it, wait for the result of the earlier call"
        ),
        "{e}"
    );
    // User approves A: exactly A's time is stored.
    f.host.create(&f.caller_a(), &r).unwrap();
    let sc = f.store.list_by_bot(f.a).unwrap().remove(0);
    assert_eq!(
        sc.spec,
        ScheduleSpec::At {
            unix_ms: start + 120 * 60_000
        }
    );
    // One preview, one apply: the key is free again afterwards.
    f.host.preview(&f.caller_a(), &ch).unwrap();
}

#[test]
fn closing_an_approval_drops_only_the_entry_made_for_it() {
    let f = fx(limits(20, 300));
    let r = when("n", "daily");
    let ch = ScheduleChange::Create(r.clone());
    let p = f.host.preview(&f.caller_a(), &ch).unwrap();
    // A close from some other approval (wrong reference) changes nothing.
    f.host.discard(&f.caller_a(), &ch, "some-other-approval");
    assert!(f.host.preview(&f.caller_a(), &ch).is_err(), "still pending");
    f.host.create(&f.caller_a(), &r).unwrap();
    // A close that arrives after the entry was used (or for a stale ref) is harmless.
    let p2 = f.host.preview(&f.caller_a(), &ch).unwrap();
    f.host.discard(&f.caller_a(), &ch, &p.approval_ref);
    f.host.create(&f.caller_a(), &r).unwrap();
    assert_eq!(f.store.list_by_bot(f.a).unwrap().len(), 2);
    assert_ne!(p.approval_ref, p2.approval_ref);
}

#[test]
fn an_expired_entry_does_not_block_a_new_preview() {
    let f = fx(limits(20, 300));
    let ch = ScheduleChange::Create(when("n", "daily"));
    f.host.preview(&f.caller_a(), &ch).unwrap();
    f.advance(7 * 3_600_000);
    f.host.preview(&f.caller_a(), &ch).unwrap();
}

#[test]
fn expired_entries_do_not_block_other_runs_or_reuse_old_timestamps() {
    let start = utc_ms(2026, 11, 12, 12, 0);
    let f = fx_at(limits(20, 300), start, true);
    // 1. Previewing after expiry replaces the frozen entry so create stores the new time.
    let r = when("Once", "in 2 hours");
    let ch = ScheduleChange::Create(r.clone());
    let p1 = f.host.preview(&f.caller_a(), &ch).unwrap();
    assert!(p1.next_runs[0].starts_with("2026-11-12T17:00:00+03:00"));
    f.advance(7 * 3_600_000);
    let p2 = f.host.preview(&f.caller_a(), &ch).unwrap();
    assert!(p2.next_runs[0].starts_with("2026-11-13T00:00:00+03:00"));
    let v = f.host.create(&f.caller_a(), &r).unwrap();
    assert_eq!(v.next_runs, p2.next_runs);
    let sc = f.store.list_by_bot(f.a).unwrap().remove(0);
    assert_eq!(
        sc.spec,
        ScheduleSpec::At {
            unix_ms: start + 9 * 3_600_000
        }
    );

    // 2. Filling FROZEN_MAX (256) with other runs' entries and expiring them
    // must free capacity for a new run's preview.
    for i in 0..256 {
        let c = ScheduleCaller {
            bot_id: f.a.to_string(),
            run_id: format!("fill-{i}"),
        };
        f.host
            .preview(&c, &ScheduleChange::Create(when("s", "daily")))
            .unwrap();
    }
    let newcomer = ScheduleCaller {
        bot_id: f.a.to_string(),
        run_id: "newcomer".into(),
    };
    assert!(
        f.host
            .preview(&newcomer, &ScheduleChange::Create(when("s", "daily")))
            .is_err(),
        "capacity is full before expiry"
    );
    f.advance(7 * 3_600_000);
    f.host
        .preview(&newcomer, &ScheduleChange::Create(when("s", "daily")))
        .expect("expired entries from other runs must be purged");
}

#[test]
fn crlf_prompts_are_normalised_to_lf_while_bare_cr_is_still_rejected() {
    let f = fx(limits(20, 300));
    let c = f.caller_a();
    let mut r = when("Windows prompt", "daily");
    r.prompt = Some("line 1\r\nline 2\r\n".into());
    let p = f
        .host
        .preview(&c, &ScheduleChange::Create(r.clone()))
        .expect("CRLF prompt must be accepted");
    assert_eq!(p.prompt, "line 1\nline 2");
    let v = f.host.create(&c, &r).unwrap();
    assert_eq!(v.prompt, "line 1\nline 2");

    // Names still forbid newlines after CRLF -> LF normalisation.
    let name_crlf = when("bad\r\nname", "daily");
    assert!(
        f.host
            .preview(&c, &ScheduleChange::Create(name_crlf))
            .is_err()
    );
    // Bare CR is still rejected even in prompts.
    let mut bare_cr = when("bare", "daily");
    bare_cr.prompt = Some("a\rb".into());
    assert!(
        f.host
            .preview(&c, &ScheduleChange::Create(bare_cr))
            .unwrap_err()
            .contains("direction-override")
    );
}

#[test]
fn a_stale_preview_past_its_time_to_live_is_refused() {
    let f = fx(limits(20, 300));
    let r = when("n", "daily");
    f.host
        .preview(&f.caller_a(), &ScheduleChange::Create(r.clone()))
        .unwrap();
    f.advance(7 * 3_600_000);
    let e = f.host.create(&f.caller_a(), &r).unwrap_err();
    assert!(e.contains("not previewed") && e.contains("too old"), "{e}");
    assert!(f.store.list_by_bot(f.a).unwrap().is_empty());
    // And a stale entry never turns into a spurious "changed" for an edit.
    let v = f.make(&f.caller_a(), &when("m", "daily")).unwrap();
    let p = patch(|r| r.prompt = Some("x".into()));
    f.host
        .preview(
            &f.caller_a(),
            &ScheduleChange::Update {
                id: v.id.clone(),
                req: p.clone(),
            },
        )
        .unwrap();
    f.advance(7 * 3_600_000);
    assert!(
        f.host
            .update(&f.caller_a(), &v.id, &p)
            .unwrap_err()
            .contains("too old")
    );
}

#[test]
fn preview_spam_never_evicts_another_runs_pending_entry() {
    let f = fx(limits(20, 300));
    let run_b = ScheduleCaller {
        bot_id: f.a.to_string(),
        run_id: "run-b".into(),
    };
    let rb = when("B's", "daily");
    f.host
        .preview(&run_b, &ScheduleChange::Create(rb.clone()))
        .unwrap();
    // 20 other runs each try 40 distinct previews (more than the overall cap in total).
    let mut refused = 0;
    for run in 0..20 {
        let c = ScheduleCaller {
            bot_id: f.a.to_string(),
            run_id: format!("spam-{run}"),
        };
        for i in 0..40 {
            if f.host
                .preview(&c, &ScheduleChange::Create(when(&format!("s{i}"), "daily")))
                .is_err()
            {
                refused += 1;
            }
        }
    }
    assert!(
        refused > 0,
        "once the overall cap is reached, newcomers are refused"
    );
    f.host.create(&run_b, &rb).unwrap();
    assert_eq!(f.store.list_by_bot(f.a).unwrap().len(), 1);
    // Within the per-run cap a run evicts only its own oldest entry.
    let c0 = ScheduleCaller {
        bot_id: f.a.to_string(),
        run_id: "spam-0".into(),
    };
    assert!(
        f.host
            .create(&c0, &when("s0", "daily"))
            .unwrap_err()
            .contains("not previewed")
    );
}

#[test]
fn overlong_when_cron_and_tz_are_refused_before_parsing() {
    let f = fx(limits(20, 300));
    let c = f.caller_a();
    let try_it = |r: ScheduleRequest| f.host.preview(&c, &ScheduleChange::Create(r));
    let mut r = req("n", "p");
    r.when = Some("every day ".repeat(30));
    assert!(
        try_it(r)
            .unwrap_err()
            .contains("`when` is 300 characters; the limit is 200")
    );
    let mut r = req("n", "p");
    r.cron = Some("*/5 ".repeat(40));
    assert!(try_it(r).unwrap_err().contains("`cron`"));
    let mut r = when("n", "daily");
    r.tz = Some("Europe/".to_owned() + &"x".repeat(80));
    assert!(try_it(r).unwrap_err().contains("`tz`"));
}

// ---------------------------------------------------------------- registry-wide guard

#[tokio::test]
async fn every_mutating_schedule_tool_has_the_expected_approval_behaviour() {
    let f = fx(limits(20, 300));
    let host: Arc<dyn ScheduleHost> = Arc::new(f.host);
    let tools = builtin_tools::schedule::tools(&host);
    let names: Vec<String> = tools.iter().map(|t| t.spec().name.clone()).collect();
    assert_eq!(names, SCHEDULE_TOOL_NAMES);
    let caller = ScheduleCaller {
        bot_id: f.a.to_string(),
        run_id: f.run_a.to_string(),
    };
    let ctx = ToolContext {
        run_id: caller.run_id.clone(),
        bot_id: Some(caller.bot_id.clone()),
        ..ToolContext::default()
    };
    let existing = host
        .preview(&caller, &ScheduleChange::Create(when("n", "daily")))
        .unwrap();
    assert_eq!(existing.action, "create");
    host.create(&caller, &when("n", "daily")).unwrap();
    let id = host.list(&caller).unwrap()[0].id.clone();
    for t in &tools {
        let name = t.spec().name.as_str();
        if name == "schedule_list" {
            assert_eq!(t.spec().effect, tool_registry::Effect::Read);
            assert!(!t.always_ask());
            continue;
        }
        assert_eq!(t.spec().effect, tool_registry::Effect::Write, "{name}");
        assert_ne!(
            t.spec().category,
            protocol::PermissionCategory::Meta,
            "{name}"
        );
        let (input, action, gated) = match name {
            "schedule_create" => (
                json!({"name":"x","prompt":"p","when":"daily"}),
                "create",
                true,
            ),
            "schedule_update" => (json!({"id": id, "prompt":"q"}), "update", true),
            "schedule_resume" => (json!({"id": id}), "resume", true),
            "schedule_pause" => (json!({"id": id}), "pause", false),
            "schedule_delete" => (json!({"id": id}), "delete", false),
            other => panic!("unexpected schedule tool {other}"),
        };
        assert_eq!(t.always_ask(), gated, "{name}");
        let d = t
            .approval_details(&ctx, &input)
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("{name} must provide approval details"));
        assert_eq!(d["kind"], "schedule", "{name}");
        assert_eq!(d["action"], action, "{name}");
        assert!(
            d["schedule"].as_str().is_some_and(|s| !s.is_empty()),
            "{name}"
        );
        t.approval_closed(&ctx, &input, None); // never panics
    }
}

#[test]
fn deleting_does_not_delete_a_schedule_the_user_edited_meanwhile() {
    let f = fx(limits(20, 300));
    let v = f.make(&f.caller_a(), &when("mine", "daily")).unwrap();
    let id = scheduler::TaskId(uuid::Uuid::parse_str(&v.id).unwrap());
    f.host
        .preview(&f.caller_a(), &ScheduleChange::Delete { id: v.id.clone() })
        .unwrap();
    // A UI edit lands between the approval card and the delete.
    let mut cur = f.store.get(id).unwrap().unwrap();
    cur.name = "user's rewrite".into();
    f.store
        .update_definition_and_rearm(&cur, None, now_ms())
        .unwrap();
    let e = f.host.delete(&f.caller_a(), &v.id).unwrap_err();
    assert!(e.contains("changed or was deleted"), "{e}");
    assert_eq!(f.store.get(id).unwrap().unwrap().name, "user's rewrite");
    assert!(
        f.audit().iter().all(|e| e.action != "schedule.delete"),
        "no deletion was logged: nothing was deleted"
    );
    // Ownership handed to the user in between: also kept.
    f.host
        .preview(&f.caller_a(), &ScheduleChange::Delete { id: v.id.clone() })
        .unwrap();
    f.store.set_origin(id, ScheduleOrigin::User).unwrap();
    assert!(f.host.delete(&f.caller_a(), &v.id).is_err());
    assert!(f.store.get(id).unwrap().is_some());
}

#[test]
fn deleting_does_not_delete_when_edited_between_check_and_delete_exact() {
    let f = fx(limits(20, 300));
    let v = f.make(&f.caller_a(), &when("mine", "daily")).unwrap();
    let id = scheduler::TaskId(uuid::Uuid::parse_str(&v.id).unwrap());
    f.host
        .preview(&f.caller_a(), &ScheduleChange::Delete { id: v.id.clone() })
        .unwrap();
    // Interleave a UI edit *after* `current_matches` has passed and *before*
    // `delete_exact` runs: only the exact (id, revision, origin) check saves it.
    let store_h = Arc::clone(&f.store);
    *f.host.before_delete_hook.lock() = Some(Box::new(move || {
        let mut cur = store_h.get(id).unwrap().unwrap();
        cur.name = "edited mid-delete".into();
        store_h
            .update_definition_and_rearm(&cur, None, now_ms())
            .unwrap();
    }));
    let e = f.host.delete(&f.caller_a(), &v.id).unwrap_err();
    assert!(e.contains("changed or was deleted"), "{e}");
    assert_eq!(
        f.store.get(id).unwrap().unwrap().name,
        "edited mid-delete",
        "the interleaved UI edit must survive"
    );
    assert!(
        f.audit().iter().any(|e| e.action == "schedule.failed"),
        "failed delete_exact after audit must be marked schedule.failed"
    );

    // Same interleave, this time handing ownership to the user mid-delete.
    f.host
        .preview(&f.caller_a(), &ScheduleChange::Delete { id: v.id.clone() })
        .unwrap();
    let store_h = Arc::clone(&f.store);
    *f.host.before_delete_hook.lock() = Some(Box::new(move || {
        store_h.set_origin(id, ScheduleOrigin::User).unwrap();
    }));
    let e = f.host.delete(&f.caller_a(), &v.id).unwrap_err();
    assert!(e.contains("changed or was deleted"), "{e}");
    assert_eq!(
        f.store.get(id).unwrap().unwrap().origin,
        ScheduleOrigin::User
    );
}

/// Make every write to `schedules.db` fail from now on (a second connection
/// installs triggers), as a disk or lock failure after the audit write would.
fn break_writes(f: &Fx, what: &[&str]) {
    let conn = rusqlite::Connection::open(f.dir.path().join("schedules.db")).unwrap();
    for w in what {
        conn.execute_batch(&format!(
            "CREATE TRIGGER block_{w} BEFORE {w} ON schedules BEGIN SELECT RAISE(ABORT, 'disk on fire'); END;"
        ))
        .unwrap();
    }
}

#[test]
fn schedule_failed_is_written_when_the_store_fails_after_the_audit_entry() {
    let f = fx(limits(20, 300));
    let v = f.make(&f.caller_a(), &when("keep", "daily")).unwrap();
    break_writes(&f, &["INSERT", "UPDATE", "DELETE"]);
    let attempts: Vec<(&str, Result<ScheduleView, String>)> = vec![
        ("create", f.make(&f.caller_a(), &when("new", "daily"))),
        (
            "update",
            f.edit(
                &f.caller_a(),
                &v.id,
                &patch(|r| r.prompt = Some("x".into())),
            ),
        ),
        ("pause", f.toggle(&f.caller_a(), &v.id, false)),
        ("delete", f.remove(&f.caller_a(), &v.id)),
    ];
    for (what, r) in &attempts {
        let e = r.as_ref().unwrap_err();
        assert!(e.contains("disk on fire"), "{what}: {e}");
    }
    let log = f.storage.list_audit(f.user).unwrap();
    let failed: Vec<(&str, &str)> = log
        .iter()
        .filter(|e| e.action == "schedule.failed")
        .map(|e| {
            (
                e.detail["attempted"].as_str().unwrap(),
                e.detail["change"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        failed,
        [
            ("schedule.create", "created"),
            ("schedule.update", "updated"),
            ("schedule.update", "paused"),
            ("schedule.delete", "deleted"),
        ]
    );
    for e in log.iter().filter(|e| e.action == "schedule.failed") {
        assert!(e.detail["error"].as_str().unwrap().contains("disk on fire"));
        assert_eq!(e.detail["bot_id"], f.a.to_string());
    }
    // Nothing changed.
    let still = f.store.list_by_bot(f.a).unwrap();
    assert_eq!(still.len(), 1);
    assert!(still[0].enabled);
}
