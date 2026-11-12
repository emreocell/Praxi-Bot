#![allow(clippy::too_many_lines, clippy::assert_is_empty)]
//! The daemon side of the bot schedule tools.

use std::sync::Arc;

use agent_core::{AgentLoop, LoopConfig, Providers};
use builtin_tools::{
    SCHEDULE_TOOL_NAMES, ScheduleCaller, ScheduleChange, ScheduleHost, ScheduleRequest,
};
use protocol::{BotId, ChatId, RunId, UserId};
use scheduler::{Payload, ScheduleStore};
use serde_json::{Value, json};
use storage::{Bot, Chat, Run, Storage, User, now_ms};
use tokio::sync::broadcast;
use tool_registry::{ToolContext, ToolRegistry};

use crate::dto::{ScheduleChangeKind, ScheduleChanged};
use crate::fake::FakeProvider;
use crate::schedule_tools::{DaemonScheduleHost, ScheduleLimits, load_schedule_limits};
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
    events: broadcast::Receiver<ScheduleChanged>,
    _dir: tempfile::TempDir,
}

fn limits(max: usize, min_secs: u64) -> ScheduleLimits {
    ScheduleLimits {
        max_per_bot: max,
        min_interval_secs: min_secs,
        timezone: Some("Europe/Istanbul".into()),
    }
}

fn fx(l: ScheduleLimits) -> Fx {
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
        })
        .unwrap();
    let store = Arc::new(ScheduleStore::open_in_memory().unwrap());
    let (tx, events) = broadcast::channel(32);
    let host = DaemonScheduleHost::new(Arc::clone(&store), Arc::clone(&storage), user, tx, l);
    Fx {
        host,
        store,
        storage,
        user,
        a,
        b,
        chat_a,
        run_a,
        events,
        _dir: dir,
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

#[test]
fn natural_language_creation_uses_the_users_timezone_and_previews_three_firings() {
    let f = fx(limits(20, 300));
    let v = f
        .host
        .create(&f.caller_a(), &when("Standup", "every weekday at 10:30"))
        .unwrap();
    assert_eq!(v.schedule, "Every weekday at 10:30");
    assert_eq!(v.timezone, "Europe/Istanbul");
    assert_eq!(v.prompt, "do the thing");
    assert!(v.enabled);
    assert_eq!(v.next_runs.len(), 3);
    for t in &v.next_runs {
        assert!(t.contains("T10:30:00+03:00"), "{t}");
    }
    // Stored for the right bot, with the run's chat, as an agent prompt.
    let sc = f.store.list_by_bot(f.a).unwrap().remove(0);
    assert_eq!(sc.chat_id, Some(f.chat_a));
    assert!(matches!(sc.payload, Payload::AgentPrompt { ref text } if text == "do the thing"));
    assert!(sc.next_run_at.is_some());
}

#[test]
fn explicit_timezone_and_turkish_phrase() {
    let f = fx(limits(20, 300));
    let mut r = when("Sabah", "her gün 09:00");
    r.tz = Some("America/New_York".into());
    let v = f.host.create(&f.caller_a(), &r).unwrap();
    assert_eq!(v.timezone, "America/New_York");
    assert_eq!(v.schedule, "Every day at 09:00");
    for t in &v.next_runs {
        assert!(t.contains("T09:00:00-0"), "{t}");
    }
    r.tz = Some("Mars/Olympus".into());
    let e = f.host.create(&f.caller_a(), &r).unwrap_err();
    assert!(e.contains("unknown timezone"), "{e}");
}

#[test]
fn cron_creation_with_and_without_embedded_timezone() {
    let f = fx(limits(20, 300));
    let v = f
        .host
        .create(&f.caller_a(), &cron("30 10 * * 1-5"))
        .unwrap();
    assert_eq!(v.schedule, "Every weekday at 10:30");
    assert_eq!(v.timezone, "Europe/Istanbul", "default timezone applied");
    let v = f
        .host
        .create(&f.caller_a(), &cron("0 8 * * 1,3 Europe/London"))
        .unwrap();
    assert_eq!(v.timezone, "Europe/London");
    assert_eq!(v.schedule, "Every Monday, Wednesday at 08:00");
    let e = f
        .host
        .create(&f.caller_a(), &cron("not a cron"))
        .unwrap_err();
    assert!(e.contains("cron"), "{e}");
}

#[test]
fn one_off_in_the_future_ok_and_when_and_cron_are_exclusive() {
    let f = fx(limits(20, 300));
    let v = f
        .host
        .create(&f.caller_a(), &when("Once", "in 2 hours"))
        .unwrap();
    assert!(v.schedule.starts_with("Once, at "), "{}", v.schedule);
    assert_eq!(v.next_runs.len(), 1);
    let mut both = when("x", "daily");
    both.cron = Some("0 9 * * *".into());
    assert!(
        f.host
            .create(&f.caller_a(), &both)
            .unwrap_err()
            .contains("not both")
    );
    assert!(
        f.host
            .create(&f.caller_a(), &req("x", "y"))
            .unwrap_err()
            .contains("`when`")
    );
    assert!(
        f.host
            .create(&f.caller_a(), &when("x", "whenever you feel like it"))
            .unwrap_err()
            .contains("cannot understand")
    );
}

#[test]
fn bot_b_cannot_see_or_touch_bot_a() {
    let f = fx(limits(20, 300));
    let v = f.host.create(&f.caller_a(), &when("A's", "daily")).unwrap();
    assert!(f.host.list(&f.caller_b()).unwrap().is_empty());
    assert_eq!(f.host.list(&f.caller_a()).unwrap().len(), 1);
    let b = f.caller_b();
    let patch = ScheduleRequest {
        prompt: Some("hijack".into()),
        ..ScheduleRequest::default()
    };
    for e in [
        f.host.update(&b, &v.id, &patch).unwrap_err(),
        f.host.set_enabled(&b, &v.id, false).unwrap_err(),
        f.host.set_enabled(&b, &v.id, true).unwrap_err(),
        f.host.delete(&b, &v.id).unwrap_err(),
        f.host
            .preview(&b, &ScheduleChange::Resume { id: v.id.clone() })
            .unwrap_err(),
        f.host
            .preview(
                &b,
                &ScheduleChange::Update {
                    id: v.id.clone(),
                    req: patch.clone(),
                },
            )
            .unwrap_err(),
    ] {
        assert!(e.contains("no schedule with id"), "{e}");
    }
    // Untouched, and a made-up id looks the same as another bot's.
    let sc = f.store.list_by_bot(f.a).unwrap().remove(0);
    assert!(sc.enabled);
    assert_eq!(sc.name, "A's");
    assert!(matches!(sc.payload, Payload::AgentPrompt { ref text } if text == "do the thing"));
    let e = f
        .host
        .delete(&b, &uuid::Uuid::now_v7().to_string())
        .unwrap_err();
    assert!(e.contains("no schedule with id"));
    // Isolation also covers audit: B's attempts wrote nothing.
    assert!(
        f.storage
            .list_audit(f.user)
            .unwrap()
            .iter()
            .all(|e| e.detail["bot_id"] == f.a.to_string())
    );
}

#[test]
fn quota_is_per_bot_and_counts_every_origin() {
    let f = fx(limits(2, 300));
    // A schedule made elsewhere (the UI) counts too.
    let ui = scheduler::Schedule::new(
        f.a,
        None,
        "ui",
        scheduler::ScheduleSpec::cron("0 9 * * *"),
        Payload::AgentPrompt { text: "x".into() },
        scheduler::MisfirePolicy::default(),
        now_ms(),
    )
    .unwrap();
    f.store.insert(&ui).unwrap();
    f.host.create(&f.caller_a(), &when("one", "daily")).unwrap();
    let e = f
        .host
        .create(&f.caller_a(), &when("two", "daily"))
        .unwrap_err();
    assert!(
        e.contains("schedule limit reached") && e.contains("2 of 2"),
        "{e}"
    );
    let e = f
        .host
        .preview(&f.caller_a(), &ScheduleChange::Create(when("two", "daily")))
        .unwrap_err();
    assert!(
        e.contains("schedule limit reached"),
        "approval is never requested over quota"
    );
    // Bot B has its own quota; deleting frees a slot.
    f.host.create(&f.caller_b(), &when("b1", "daily")).unwrap();
    let id = f.host.list(&f.caller_a()).unwrap()[0].id.clone();
    f.host.delete(&f.caller_a(), &id).unwrap();
    f.host
        .create(&f.caller_a(), &when("again", "daily"))
        .unwrap();
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
        f.host
            .create(&f.caller_a(), &when(&format!("s{i}"), "daily"))
            .unwrap();
    }
    assert!(
        f.host
            .create(&f.caller_a(), &when("s20", "daily"))
            .unwrap_err()
            .contains("20 of 20")
    );
}

#[test]
fn minimum_interval_applies_to_phrases_and_cron() {
    let f = fx(limits(20, 300));
    for (r, what) in [
        (when("x", "every minute"), "1 minute"),
        (when("x", "every 2 minutes"), "2 minutes"),
        (cron("* * * * *"), "1 minute"),
        (cron("*/2 * * * *"), "2 minutes"),
        (cron("0,2 * * * *"), "2 minutes"),
    ] {
        let e = f.host.create(&f.caller_a(), &r).unwrap_err();
        assert!(e.contains("minimum interval is 5 minutes"), "{e}");
        assert!(e.contains(what), "{e} / {what}");
    }
    f.host
        .create(&f.caller_a(), &when("ok", "every 5 minutes"))
        .unwrap();
    f.host.create(&f.caller_a(), &cron("*/5 * * * *")).unwrap();
    f.host.create(&f.caller_a(), &cron("0 * * * *")).unwrap();
    // Configurable.
    let f = fx(limits(20, 60));
    f.host
        .create(&f.caller_a(), &when("m", "every minute"))
        .unwrap();
    let f = fx(limits(20, 3600));
    let e = f
        .host
        .create(&f.caller_a(), &when("m", "every 30 minutes"))
        .unwrap_err();
    assert!(e.contains("minimum interval is 1 hour"), "{e}");
}

#[test]
fn updates_change_only_what_is_given_and_rearm() {
    let f = fx(limits(20, 300));
    let v = f
        .host
        .create(&f.caller_a(), &when("Standup", "daily"))
        .unwrap();
    let u = f
        .host
        .update(
            &f.caller_a(),
            &v.id,
            &ScheduleRequest {
                prompt: Some("new prompt".into()),
                ..ScheduleRequest::default()
            },
        )
        .unwrap();
    assert_eq!(
        (u.name.as_str(), u.prompt.as_str()),
        ("Standup", "new prompt")
    );
    assert_eq!(u.schedule, "Every day at 09:00");
    let u = f
        .host
        .update(
            &f.caller_a(),
            &v.id,
            &ScheduleRequest {
                when: Some("every weekday at 7:15".into()),
                ..ScheduleRequest::default()
            },
        )
        .unwrap();
    assert_eq!(u.schedule, "Every weekday at 07:15");
    let e = f
        .host
        .update(
            &f.caller_a(),
            &v.id,
            &ScheduleRequest {
                tz: Some("UTC".into()),
                ..ScheduleRequest::default()
            },
        )
        .unwrap_err();
    assert!(e.contains("`tz` only applies"), "{e}");
    let e = f
        .host
        .update(
            &f.caller_a(),
            &v.id,
            &ScheduleRequest {
                when: Some("every minute".into()),
                ..ScheduleRequest::default()
            },
        )
        .unwrap_err();
    assert!(e.contains("minimum interval"), "{e}");
}

#[test]
fn pause_and_resume_toggle_the_timer() {
    let f = fx(limits(20, 300));
    let v = f.host.create(&f.caller_a(), &when("n", "daily")).unwrap();
    let p = f.host.set_enabled(&f.caller_a(), &v.id, false).unwrap();
    assert!(!p.enabled);
    assert!(p.next_runs.is_empty());
    let sc = f.store.list_by_bot(f.a).unwrap().remove(0);
    assert!(!sc.enabled);
    let r = f.host.set_enabled(&f.caller_a(), &v.id, true).unwrap();
    assert!(r.enabled);
    assert_eq!(r.next_runs.len(), 3);
    assert!(f.store.list_by_bot(f.a).unwrap()[0].next_run_at.is_some());
    let pv = f
        .host
        .preview(&f.caller_a(), &ScheduleChange::Resume { id: v.id })
        .unwrap();
    assert_eq!(pv.action, "resume");
    assert_eq!(pv.next_runs.len(), 3);
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
    assert_eq!(pv.schedule, "Every weekday at 10:30");
    assert_eq!(pv.timezone, "Europe/Istanbul");
    assert_eq!(pv.next_runs.len(), 3);
    assert!(f.store.list_by_bot(f.a).unwrap().is_empty());
}

#[test]
fn every_create_update_pause_delete_is_audited_with_bot_and_run() {
    let f = fx(limits(20, 300));
    let v = f.host.create(&f.caller_a(), &when("n", "daily")).unwrap();
    f.host
        .update(
            &f.caller_a(),
            &v.id,
            &ScheduleRequest {
                name: Some("n2".into()),
                ..ScheduleRequest::default()
            },
        )
        .unwrap();
    f.host.set_enabled(&f.caller_a(), &v.id, false).unwrap();
    f.host.set_enabled(&f.caller_a(), &v.id, true).unwrap();
    f.host.delete(&f.caller_a(), &v.id).unwrap();
    let log: Vec<_> = f
        .storage
        .list_audit(f.user)
        .unwrap()
        .into_iter()
        .filter(|e| e.action.starts_with("schedule."))
        .collect();
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
}

#[test]
fn the_ui_is_told_about_every_change() {
    let mut f = fx(limits(20, 300));
    let v = f.host.create(&f.caller_a(), &when("n", "daily")).unwrap();
    f.host
        .update(
            &f.caller_a(),
            &v.id,
            &ScheduleRequest {
                name: Some("n2".into()),
                ..ScheduleRequest::default()
            },
        )
        .unwrap();
    f.host.set_enabled(&f.caller_a(), &v.id, false).unwrap();
    f.host.delete(&f.caller_a(), &v.id).unwrap();
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
fn a_workflow_schedule_can_be_paused_but_not_rewritten() {
    let f = fx(limits(20, 300));
    let sc = scheduler::Schedule::new(
        f.a,
        None,
        "wf",
        scheduler::ScheduleSpec::cron("0 9 * * *"),
        Payload::Workflow {
            name: "w".into(),
            params: Value::Null,
        },
        scheduler::MisfirePolicy::default(),
        now_ms(),
    )
    .unwrap();
    f.store.insert(&sc).unwrap();
    let id = sc.id.0.to_string();
    let e = f
        .host
        .update(
            &f.caller_a(),
            &id,
            &ScheduleRequest {
                prompt: Some("x".into()),
                ..ScheduleRequest::default()
            },
        )
        .unwrap_err();
    assert!(e.contains("workflow or tool"), "{e}");
    assert!(
        !f.host
            .set_enabled(&f.caller_a(), &id, false)
            .unwrap()
            .enabled
    );
}

#[test]
fn schedule_toml_sets_limits_and_bad_files_fall_back_with_a_warning() {
    let dir = tempfile::tempdir().unwrap();
    let (l, w) = load_schedule_limits(dir.path());
    assert_eq!(l, ScheduleLimits::default());
    assert!(w.is_empty());
    std::fs::write(
        dir.path().join("schedule.toml"),
        "max_per_bot = 3\nmin_interval_secs = 900\ntimezone = \"Europe/Istanbul\"\n",
    )
    .unwrap();
    let (l, w) = load_schedule_limits(dir.path());
    assert_eq!(
        (l.max_per_bot, l.min_interval_secs, l.timezone.as_deref()),
        (3, 900, Some("Europe/Istanbul"))
    );
    assert!(w.is_empty());
    for bad in [
        "max_per_bot = \"many\"",
        "bogus = 1",
        "timezone = \"Nope/Zone\"",
    ] {
        std::fs::write(dir.path().join("schedule.toml"), bad).unwrap();
        let (l, w) = load_schedule_limits(dir.path());
        assert_eq!(l, ScheduleLimits::default(), "{bad}");
        assert_eq!(w.len(), 1, "{bad}");
    }
}

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
    // A bot-less call is refused; a real bot's call lands in the store and
    // wakes the UI stream.
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
    create.call(&ctx, args).await.unwrap();
    assert_eq!(
        state
            .schedule_store()
            .unwrap()
            .list_by_bot(bot)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(events.try_recv().unwrap().kind, ScheduleChangeKind::Created);
    // A second call registers nothing new (names are unique).
    assert_eq!(state.enable_schedule_tools(limits(20, 300)).unwrap(), 0);
}
