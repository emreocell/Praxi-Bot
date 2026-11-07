use std::sync::Arc;

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use parking_lot::Mutex;
use protocol::{BotId, ChatId, TaskId};
use serde_json::json;
use tokio::sync::Notify;

use crate::{
    FakeClock, MisfirePolicy, Payload, RUN_ALL_CAP, Result, Schedule, ScheduleExecutor,
    ScheduleSpec, ScheduleStore, Scheduler, SchedulerError, Trigger, handle_webhook,
    parse_natural_language, parse_natural_language_with_tz, sign_webhook_body, sign_webhook_hex,
    verify_webhook_signature,
};

#[derive(Default)]
struct RecordingExecutor {
    runs: Mutex<Vec<Schedule>>,
    gate: Option<(Arc<Notify>, Arc<Notify>)>,
}

impl RecordingExecutor {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn gated() -> (Arc<Self>, Arc<Notify>, Arc<Notify>) {
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let exec = Arc::new(Self {
            runs: Mutex::new(Vec::new()),
            gate: Some((Arc::clone(&entered), Arc::clone(&release))),
        });
        (exec, entered, release)
    }

    fn count(&self) -> usize {
        self.runs.lock().len()
    }

    fn recorded(&self) -> Vec<Schedule> {
        self.runs.lock().clone()
    }
}

#[async_trait]
impl ScheduleExecutor for RecordingExecutor {
    async fn execute(&self, schedule: &Schedule) -> Result<()> {
        self.runs.lock().push(schedule.clone());
        if let Some((entered, release)) = &self.gate {
            entered.notify_waiters();
            entered.notify_one();
            release.notified().await;
        }
        Ok(())
    }
}

fn utc_ms(year: i32, month: u32, day: u32, hour: u32, min: u32, sec: u32) -> i64 {
    Utc.with_ymd_and_hms(year, month, day, hour, min, sec)
        .single()
        .expect("valid UTC datetime")
        .timestamp_millis()
}

#[tokio::test]
async fn cron_next_run_across_dst_spring_forward_and_fall_back() {
    // 1. Spring-forward in America/New_York:
    // On Sunday 2026-03-08 at 02:00 EST (UTC-5), clocks jump to 03:00 EDT (UTC-4).
    // - Saturday 2026-03-07 09:00 EST = 2026-03-07 14:00 UTC
    // - Sunday   2026-03-08 09:00 EDT = 2026-03-08 13:00 UTC (23 hours later!)
    let sat_before_9 = utc_ms(2026, 3, 7, 13, 0, 0);
    let sat_9_est = utc_ms(2026, 3, 7, 14, 0, 0);
    let sun_9_edt = utc_ms(2026, 3, 8, 13, 0, 0);
    assert_eq!(sun_9_edt - sat_9_est, 23 * 3_600_000);

    let store = Arc::new(ScheduleStore::open_in_memory().unwrap());
    let exec = RecordingExecutor::new();
    let clock = Arc::new(FakeClock::new(sat_before_9));

    let sched_row = Schedule::new(
        BotId::new(),
        Some(ChatId::new()),
        "ny-morning",
        ScheduleSpec::cron_tz("0 9 * * *", "America/New_York"),
        Payload::AgentPrompt {
            text: "morning digest".into(),
        },
        MisfirePolicy::Skip,
        sat_before_9,
    )
    .unwrap();
    assert_eq!(sched_row.next_run_at, Some(sat_9_est));
    let id = sched_row.id;
    store.insert(&sched_row).unwrap();

    let scheduler = Scheduler::start_with_clock(Arc::clone(&store), exec.clone(), clock.clone());
    scheduler.settle().await;
    assert_eq!(exec.count(), 0);

    // Advance 1 hour to Saturday 09:00 EST -> fires!
    clock.advance_ms(3_600_000);
    scheduler.settle().await;
    assert_eq!(exec.count(), 1);

    // Verify next_run_at is Sunday 09:00 EDT (23 hours later across spring-forward).
    let updated = store.get(id).unwrap().unwrap();
    assert_eq!(updated.next_run_at, Some(sun_9_edt));

    // Advance 23 hours -> fires on Sunday 09:00 EDT!
    clock.advance_ms(23 * 3_600_000);
    scheduler.settle().await;
    assert_eq!(exec.count(), 2);

    scheduler.shutdown().await;

    // 2. Fall-back in America/New_York:
    // On Sunday 2026-11-01 at 02:00 EDT (UTC-4), clocks fall back to 01:00 EST (UTC-5).
    // - Saturday 2026-10-31 09:00 EDT = 2026-10-31 13:00 UTC
    // - Sunday   2026-11-01 09:00 EST = 2026-11-01 14:00 UTC (25 hours later!)
    let oct_sat_9_edt = utc_ms(2026, 10, 31, 13, 0, 0);
    let nov_sun_9_est = utc_ms(2026, 11, 1, 14, 0, 0);
    assert_eq!(nov_sun_9_est - oct_sat_9_edt, 25 * 3_600_000);

    let spec = ScheduleSpec::Cron("CRON_TZ=America/New_York 0 9 * * *".into());
    assert_eq!(spec.next_after(oct_sat_9_edt).unwrap(), Some(nov_sun_9_est));
}

#[tokio::test]
async fn every_and_at_schedules_and_wake_on_store_changes() {
    let t0 = 1_700_000_000_000_i64;
    let store = Arc::new(ScheduleStore::open_in_memory().unwrap());
    let exec = RecordingExecutor::new();
    let clock = Arc::new(FakeClock::new(t0));

    let scheduler = Scheduler::start_with_clock(Arc::clone(&store), exec.clone(), clock.clone());
    scheduler.settle().await;

    // Insert an `At` one-shot schedule at t0 + 10s and an `Every` 30s schedule while running.
    let one_shot = Schedule::new(
        BotId::new(),
        None,
        "remind-once",
        ScheduleSpec::At {
            unix_ms: t0 + 10_000,
        },
        Payload::AgentPrompt {
            text: "stand up".into(),
        },
        MisfirePolicy::RunOnce,
        t0,
    )
    .unwrap();
    let one_shot_id = one_shot.id;
    store.insert(&one_shot).unwrap();

    let recurring = Schedule::new(
        BotId::new(),
        None,
        "heartbeat",
        ScheduleSpec::Every { seconds: 30 },
        Payload::Tool {
            name: "ping".into(),
            input: json!({"ok": true}),
        },
        MisfirePolicy::Skip,
        t0,
    )
    .unwrap();
    let recurring_id = recurring.id;
    store.insert(&recurring).unwrap();

    // Advance 10s -> one-shot fires; recurring is at 30s so does not fire yet.
    clock.advance_ms(10_000);
    scheduler.settle().await;
    assert_eq!(exec.count(), 1);
    assert_eq!(exec.recorded()[0].id, one_shot_id);
    assert_eq!(store.get(one_shot_id).unwrap().unwrap().next_run_at, None);

    // Advance 20s (to t0 + 30s) -> recurring fires for the first time, one-shot never fires again.
    clock.advance_ms(20_000);
    scheduler.settle().await;
    assert_eq!(exec.count(), 2);
    assert_eq!(exec.recorded()[1].id, recurring_id);
    assert_eq!(
        store.get(recurring_id).unwrap().unwrap().next_run_at,
        Some(t0 + 60_000)
    );

    // Advance another 30s (to t0 + 60s) -> recurring fires a second time.
    clock.advance_ms(30_000);
    scheduler.settle().await;
    assert_eq!(exec.count(), 3);
    assert_eq!(exec.recorded()[2].id, recurring_id);

    scheduler.shutdown().await;
}

#[tokio::test]
async fn misfire_policies_on_wall_clock_jump_skip_run_once_and_run_all_capped() {
    let t0 = 1_700_000_000_000_i64;
    let store = Arc::new(ScheduleStore::open_in_memory().unwrap());
    let exec = RecordingExecutor::new();
    let clock = Arc::new(FakeClock::new(t0));

    let bot_id = BotId::new();
    let skip_sched = Schedule::new(
        bot_id,
        None,
        "skip-job",
        ScheduleSpec::Every { seconds: 60 },
        Payload::AgentPrompt {
            text: "skip".into(),
        },
        MisfirePolicy::Skip,
        t0,
    )
    .unwrap();
    let once_sched = Schedule::new(
        bot_id,
        None,
        "once-job",
        ScheduleSpec::Every { seconds: 60 },
        Payload::AgentPrompt {
            text: "once".into(),
        },
        MisfirePolicy::RunOnce,
        t0,
    )
    .unwrap();
    let all_sched = Schedule::new(
        bot_id,
        None,
        "all-job",
        ScheduleSpec::Every { seconds: 60 },
        Payload::Workflow {
            name: "sync".into(),
            params: json!({"full": false}),
        },
        MisfirePolicy::RunAll,
        t0,
    )
    .unwrap();

    let skip_id = skip_sched.id;
    let once_id = once_sched.id;
    let all_id = all_sched.id;

    store.insert(&skip_sched).unwrap();
    store.insert(&once_sched).unwrap();
    store.insert(&all_sched).unwrap();

    let scheduler = Scheduler::start_with_clock(Arc::clone(&store), exec.clone(), clock.clone());
    scheduler.settle().await;

    // Simulate OS sleep for 25 minutes: wall clock jumps forward by 25 * 60_000 ms
    // while monotonic clock does not advance (25 missed occurrences at 1..=25 min).
    clock.jump_wall_ms(25 * 60_000);
    scheduler.settle().await;

    assert!(scheduler.wall_clock_jumps_detected() >= 1);

    let runs = exec.recorded();
    let skip_fires = runs.iter().filter(|s| s.id == skip_id).count();
    let once_fires = runs.iter().filter(|s| s.id == once_id).count();
    let all_fires = runs.iter().filter(|s| s.id == all_id).count();

    assert_eq!(skip_fires, 0, "MisfirePolicy::Skip must skip missed runs");
    assert_eq!(
        once_fires, 1,
        "MisfirePolicy::RunOnce must fire exactly once"
    );
    assert_eq!(
        all_fires, RUN_ALL_CAP,
        "MisfirePolicy::RunAll must cap missed executions at 10"
    );

    // All three schedules must have their `next_run_at` set to t0 + 26 minutes.
    let expected_next = Some(t0 + 26 * 60_000);
    assert_eq!(
        store.get(skip_id).unwrap().unwrap().next_run_at,
        expected_next
    );
    assert_eq!(
        store.get(once_id).unwrap().unwrap().next_run_at,
        expected_next
    );
    assert_eq!(
        store.get(all_id).unwrap().unwrap().next_run_at,
        expected_next
    );

    // Normal advance by 1 minute (to t0 + 26 min) fires all 3 schedules on time (including Skip).
    clock.advance_ms(60_000);
    scheduler.settle().await;

    let runs = exec.recorded();
    assert_eq!(runs.iter().filter(|s| s.id == skip_id).count(), 1);
    assert_eq!(runs.iter().filter(|s| s.id == once_id).count(), 2);
    assert_eq!(
        runs.iter().filter(|s| s.id == all_id).count(),
        RUN_ALL_CAP + 1
    );

    scheduler.shutdown().await;
}

#[tokio::test]
async fn no_double_fire_after_restart_persists_before_executing() {
    let dir = tempfile::tempdir().unwrap();
    let t0 = 1_700_000_000_000_i64;
    let t_fire = t0 + 60_000;

    let schedule_id: TaskId;
    {
        let store = Arc::new(ScheduleStore::open(dir.path()).unwrap());
        let (exec, entered, release) = RecordingExecutor::gated();
        let clock = Arc::new(FakeClock::new(t0));

        let sched = Schedule::new(
            BotId::new(),
            None,
            "critical-payment-check",
            ScheduleSpec::Every { seconds: 60 },
            Payload::AgentPrompt {
                text: "run once per slot".into(),
            },
            MisfirePolicy::RunAll,
            t0,
        )
        .unwrap();
        schedule_id = sched.id;
        store.insert(&sched).unwrap();

        let scheduler =
            Scheduler::start_with_clock(Arc::clone(&store), exec.clone(), clock.clone());

        // Advance to t_fire and wait until the executor has been entered (mid-execution!).
        let entered_fut = entered.notified();
        clock.advance_ms(60_000);
        scheduler.tick().await;
        entered_fut.await;

        // While the execution is still in-flight (not finished), verify that SQLite
        // ALREADY persisted last_run_at = t_fire and next_run_at = t_fire + 60_000!
        let persisted = store.get(schedule_id).unwrap().unwrap();
        assert_eq!(persisted.last_run_at, Some(t_fire));
        assert_eq!(persisted.next_run_at, Some(t_fire + 60_000));
        assert_eq!(exec.count(), 1);

        // Simulate daemon stop / restart.
        release.notify_waiters();
        scheduler.shutdown().await;
    }

    // Reopen the SQLite store after restart at the exact same timestamp `t_fire`
    // (or slightly after `t_fire + 10_000` before the next 60s slot).
    let store2 = Arc::new(ScheduleStore::open(dir.path()).unwrap());
    let exec2 = RecordingExecutor::new();
    let clock2 = Arc::new(FakeClock::new(t_fire + 10_000));

    let scheduler2 = Scheduler::start_with_clock(Arc::clone(&store2), exec2.clone(), clock2);
    scheduler2.settle().await;

    // Must NOT re-fire the t_fire occurrence after restart!
    assert_eq!(exec2.count(), 0);
    scheduler2.shutdown().await;
}

#[tokio::test]
async fn per_schedule_concurrency_limit_of_one_skips_when_still_running() {
    let t0 = 1_700_000_000_000_i64;
    let store = Arc::new(ScheduleStore::open_in_memory().unwrap());
    let (exec, entered, release) = RecordingExecutor::gated();
    let clock = Arc::new(FakeClock::new(t0));

    let sched = Schedule::new(
        BotId::new(),
        None,
        "slow-job",
        ScheduleSpec::Every { seconds: 10 },
        Payload::AgentPrompt {
            text: "heavy work".into(),
        },
        MisfirePolicy::RunAll,
        t0,
    )
    .unwrap();
    let id = sched.id;
    store.insert(&sched).unwrap();

    let scheduler = Scheduler::start_with_clock(Arc::clone(&store), exec.clone(), clock.clone());

    // 1. At t0 + 10s, Run 1 starts and blocks inside executor.
    let entered_fut = entered.notified();
    clock.advance_ms(10_000);
    scheduler.tick().await;
    entered_fut.await;
    assert!(scheduler.is_in_flight(id));
    assert_eq!(exec.count(), 1);

    // 2. At t0 + 20s, Run 1 is still in flight -> Run 2 must be skipped!
    clock.advance_ms(10_000);
    scheduler.tick().await;
    assert_eq!(exec.count(), 1);
    assert_eq!(scheduler.skipped_runs(id), 1);
    assert_eq!(
        store.get(id).unwrap().unwrap().next_run_at,
        Some(t0 + 30_000)
    );

    // 3. At t0 + 30s, Run 1 is still in flight -> Run 3 must also be skipped!
    clock.advance_ms(10_000);
    scheduler.tick().await;
    assert_eq!(exec.count(), 1);
    assert_eq!(scheduler.skipped_runs(id), 2);
    assert_eq!(
        store.get(id).unwrap().unwrap().next_run_at,
        Some(t0 + 40_000)
    );

    // 4. Release Run 1 and wait for in-flight to drain.
    release.notify_waiters();
    scheduler.settle().await;
    assert!(!scheduler.is_in_flight(id));

    // 5. At t0 + 40s, no run is in flight -> Run 4 starts!
    let entered_fut2 = entered.notified();
    clock.advance_ms(10_000);
    scheduler.tick().await;
    entered_fut2.await;
    assert_eq!(exec.count(), 2);
    release.notify_waiters();
    scheduler.settle().await;

    scheduler.shutdown().await;
}

#[tokio::test]
async fn webhook_hmac_sha256_verification_and_input_substitution() {
    let t0 = 1_700_000_000_000_i64;
    let store = Arc::new(ScheduleStore::open_in_memory().unwrap());
    let exec = RecordingExecutor::new();
    let clock = Arc::new(FakeClock::new(t0));

    let sched = Schedule::new(
        BotId::new(),
        Some(ChatId::new()),
        "github-webhook",
        ScheduleSpec::Every { seconds: 3_600 },
        Payload::AgentPrompt {
            text: "Triage webhook payload: {input}".into(),
        },
        MisfirePolicy::Skip,
        t0,
    )
    .unwrap();
    let id = sched.id;
    store.insert(&sched).unwrap();

    let secret = "whsec_test_secret_key_99";
    store
        .set_trigger(
            id,
            &Trigger::Webhook {
                secret: secret.into(),
            },
        )
        .unwrap();

    let scheduler = Scheduler::start_with_clock(Arc::clone(&store), exec.clone(), clock);

    let body = r#"{"action":"opened","issue":42}"#;
    let sig_header = sign_webhook_body(secret, body.as_bytes());
    let sig_raw_hex = sign_webhook_hex(secret, body.as_bytes());

    // Direct signature verifier checks
    assert!(verify_webhook_signature(secret, body.as_bytes(), &sig_header).is_ok());
    assert!(verify_webhook_signature(secret, body.as_bytes(), &sig_raw_hex).is_ok());
    assert!(matches!(
        verify_webhook_signature("wrong", body.as_bytes(), &sig_header),
        Err(SchedulerError::InvalidSignature)
    ));
    assert!(matches!(
        verify_webhook_signature(secret, b"tampered", &sig_header),
        Err(SchedulerError::InvalidSignature)
    ));
    assert!(matches!(
        verify_webhook_signature(secret, body.as_bytes(), "sha256=not_hex"),
        Err(SchedulerError::InvalidSignature)
    ));

    // Invalid signature via handle_webhook must not execute
    let bad = scheduler.handle_webhook(id, body, "sha256=0000").await;
    assert!(matches!(bad, Err(SchedulerError::InvalidSignature)));
    assert_eq!(exec.count(), 0);

    // Valid signature via Scheduler::handle_webhook fires payload with {input} substituted
    scheduler
        .handle_webhook(id, body, &sig_header)
        .await
        .unwrap();
    assert_eq!(exec.count(), 1);
    assert_eq!(
        exec.recorded()[0].payload,
        Payload::AgentPrompt {
            text: format!("Triage webhook payload: {body}"),
        }
    );

    // Also test Workflow and Tool payload {input} substitution via free `handle_webhook` function
    let wf_sched = Schedule::new(
        BotId::new(),
        None,
        "wf-hook",
        ScheduleSpec::Every { seconds: 3_600 },
        Payload::Workflow {
            name: "deploy".into(),
            params: json!({ "event": "{input}" }),
        },
        MisfirePolicy::Skip,
        t0,
    )
    .unwrap();
    let wf_id = wf_sched.id;
    store.insert(&wf_sched).unwrap();
    store
        .set_trigger(
            wf_id,
            &Trigger::Webhook {
                secret: secret.into(),
            },
        )
        .unwrap();

    handle_webhook(&store, exec.as_ref(), wf_id, body, &sig_raw_hex)
        .await
        .unwrap();
    assert_eq!(exec.count(), 2);
    assert_eq!(
        exec.recorded()[1].payload,
        Payload::Workflow {
            name: "deploy".into(),
            params: json!({ "event": body }),
        }
    );

    scheduler.shutdown().await;
}

#[test]
#[allow(clippy::too_many_lines)]
fn natural_language_parser_handles_at_least_25_english_and_turkish_phrases() {
    let now_ms = 1_700_000_000_000_i64;

    let cases: &[(&str, ScheduleSpec)] = &[
        // --- Card examples ---
        ("her gün 09:00", ScheduleSpec::Cron("0 9 * * *".into())),
        ("every monday at 8", ScheduleSpec::Cron("0 8 * * 1".into())),
        (
            "2 saat sonra",
            ScheduleSpec::At {
                unix_ms: now_ms + 2 * 3_600_000,
            },
        ),
        (
            "in 30 minutes",
            ScheduleSpec::At {
                unix_ms: now_ms + 30 * 60_000,
            },
        ),
        // --- English Cron phrases ---
        ("every day at 09:00", ScheduleSpec::Cron("0 9 * * *".into())),
        ("daily at 14:30", ScheduleSpec::Cron("30 14 * * *".into())),
        (
            "every weekday 09:00",
            ScheduleSpec::Cron("0 9 * * 1-5".into()),
        ),
        (
            "every weekday at 8:15am",
            ScheduleSpec::Cron("15 8 * * 1-5".into()),
        ),
        (
            "every weekend at 10:00",
            ScheduleSpec::Cron("0 10 * * 0,6".into()),
        ),
        (
            "every friday at 5pm",
            ScheduleSpec::Cron("0 17 * * 5".into()),
        ),
        (
            "every sunday at 12:00",
            ScheduleSpec::Cron("0 12 * * 0".into()),
        ),
        (
            "on wednesdays at 18:45",
            ScheduleSpec::Cron("45 18 * * 3".into()),
        ),
        (
            "every month at 08:00",
            ScheduleSpec::Cron("0 8 1 * *".into()),
        ),
        ("every midnight", ScheduleSpec::Cron("0 0 * * *".into())),
        ("every noon", ScheduleSpec::Cron("0 12 * * *".into())),
        // --- Turkish Cron phrases ---
        (
            "her gün saat 09:00'da",
            ScheduleSpec::Cron("0 9 * * *".into()),
        ),
        ("her sabah 08:30", ScheduleSpec::Cron("30 8 * * *".into())),
        ("her akşam 8", ScheduleSpec::Cron("0 20 * * *".into())),
        (
            "hafta içi her gün 09:00",
            ScheduleSpec::Cron("0 9 * * 1-5".into()),
        ),
        (
            "hafta içi saat 09:30",
            ScheduleSpec::Cron("30 9 * * 1-5".into()),
        ),
        (
            "hafta sonu 11:00",
            ScheduleSpec::Cron("0 11 * * 0,6".into()),
        ),
        (
            "her pazartesi 08:00",
            ScheduleSpec::Cron("0 8 * * 1".into()),
        ),
        (
            "her çarşamba saat 15:00",
            ScheduleSpec::Cron("0 15 * * 3".into()),
        ),
        (
            "cuma günleri 17:30",
            ScheduleSpec::Cron("30 17 * * 5".into()),
        ),
        ("her pazar saat 10", ScheduleSpec::Cron("0 10 * * 0".into())),
        ("her ay 09:00", ScheduleSpec::Cron("0 9 1 * *".into())),
        // --- English & Turkish Every (interval) phrases ---
        ("every 15 minutes", ScheduleSpec::Every { seconds: 900 }),
        ("every 2 hours", ScheduleSpec::Every { seconds: 7_200 }),
        ("every 30 seconds", ScheduleSpec::Every { seconds: 30 }),
        ("hourly", ScheduleSpec::Every { seconds: 3_600 }),
        ("her 10 dakikada bir", ScheduleSpec::Every { seconds: 600 }),
        ("her 2 saatte bir", ScheduleSpec::Every { seconds: 7_200 }),
        ("her 45 saniyede bir", ScheduleSpec::Every { seconds: 45 }),
        ("saat başı", ScheduleSpec::Every { seconds: 3_600 }),
        // --- English & Turkish At (relative one-off) phrases ---
        (
            "in 2 hours",
            ScheduleSpec::At {
                unix_ms: now_ms + 2 * 3_600_000,
            },
        ),
        (
            "in 45 seconds",
            ScheduleSpec::At {
                unix_ms: now_ms + 45_000,
            },
        ),
        (
            "in 3 days",
            ScheduleSpec::At {
                unix_ms: now_ms + 3 * 86_400_000,
            },
        ),
        (
            "in half an hour",
            ScheduleSpec::At {
                unix_ms: now_ms + 1_800_000,
            },
        ),
        (
            "15 dakika sonra",
            ScheduleSpec::At {
                unix_ms: now_ms + 15 * 60_000,
            },
        ),
        (
            "yarım saat sonra",
            ScheduleSpec::At {
                unix_ms: now_ms + 1_800_000,
            },
        ),
        (
            "bir saat sonra",
            ScheduleSpec::At {
                unix_ms: now_ms + 3_600_000,
            },
        ),
        (
            "3 gün sonra",
            ScheduleSpec::At {
                unix_ms: now_ms + 3 * 86_400_000,
            },
        ),
    ];

    assert!(
        cases.len() >= 20,
        "acceptance requires >= 20 NL phrases, got {}",
        cases.len()
    );

    for &(phrase, ref expected) in cases {
        let actual = parse_natural_language(phrase, now_ms)
            .unwrap_or_else(|| panic!("expected phrase '{phrase}' to parse"));
        assert_eq!(&actual, expected, "mismatch on phrase '{phrase}'");
        actual.validate().expect("parsed spec must be valid");
    }

    // Timezone injection into parsed Cron phrases
    assert_eq!(
        parse_natural_language_with_tz("her gün 09:00", now_ms, Some("Europe/Istanbul")),
        Some(ScheduleSpec::Cron("0 9 * * * Europe/Istanbul".into()))
    );

    // Invalid phrases return None
    for invalid in [
        "",
        "   ",
        "summarize my inbox",
        "every 0 minutes",
        "in 0 hours",
        "her gün 25:00",
        "every monday at 99",
    ] {
        assert_eq!(
            parse_natural_language(invalid, now_ms),
            None,
            "expected '{invalid}' to return None"
        );
    }
}

#[test]
fn cron_dst_gap_and_overlap_fire_exactly_once_per_day() {
    // Spring forward 2026-03-08 in New York: 02:30 local does not exist.
    let spec = ScheduleSpec::cron_tz("30 2 * * *", "America/New_York");
    let mut cursor = utc_ms(2026, 3, 6, 12, 0, 0);
    let mut fires = Vec::new();
    for _ in 0..4 {
        cursor = spec.next_after(cursor).unwrap().expect("next occurrence");
        fires.push(cursor);
    }
    // Strictly increasing, and at most one fire per calendar day (no skip-day, no double).
    assert!(fires.windows(2).all(|w| w[0] < w[1]));
    let days: std::collections::BTreeSet<i64> = fires.iter().map(|f| f / 86_400_000).collect();
    assert_eq!(days.len(), fires.len(), "one fire per UTC day: {fires:?}");

    // Fall back 2026-11-01: 01:30 local happens twice; must fire once.
    let spec = ScheduleSpec::cron_tz("30 1 * * *", "America/New_York");
    let from = utc_ms(2026, 10, 31, 12, 0, 0);
    let first = spec.next_after(from).unwrap().unwrap();
    let second = spec.next_after(first).unwrap().unwrap();
    let third = spec.next_after(second).unwrap().unwrap();
    assert_eq!(first, utc_ms(2026, 11, 1, 5, 30, 0)); // 01:30 EDT
    assert_eq!(second, utc_ms(2026, 11, 2, 6, 30, 0)); // next day 01:30 EST
    assert!(third > second);

    // Plain Istanbul (no DST since 2016) stays at fixed UTC+3.
    let ist = ScheduleSpec::cron_tz("0 9 * * *", "Europe/Istanbul");
    assert_eq!(
        ist.next_after(utc_ms(2026, 7, 1, 0, 0, 0)).unwrap(),
        Some(utc_ms(2026, 7, 1, 6, 0, 0))
    );
    assert!(
        ScheduleSpec::cron("0 9 * * * Mars/Phobos")
            .validate()
            .is_err()
    );
    assert!(ScheduleSpec::cron("61 9 * * *").validate().is_err());
}

#[tokio::test]
async fn restart_after_downtime_applies_misfire_policy_and_at_fires_once() {
    let dir = tempfile::tempdir().unwrap();
    let t0 = 1_700_000_000_000_i64;
    let bot = BotId::new();
    let mk = |name: &str, spec: ScheduleSpec, policy| {
        Schedule::new(
            bot,
            None,
            name,
            spec,
            Payload::AgentPrompt { text: name.into() },
            policy,
            t0,
        )
        .unwrap()
    };
    let skip = mk(
        "skip",
        ScheduleSpec::Every { seconds: 60 },
        MisfirePolicy::Skip,
    );
    let once = mk(
        "once",
        ScheduleSpec::Every { seconds: 60 },
        MisfirePolicy::RunOnce,
    );
    let all = mk(
        "all",
        ScheduleSpec::Every { seconds: 60 },
        MisfirePolicy::RunAll,
    );
    let at = mk(
        "at",
        ScheduleSpec::At {
            unix_ms: t0 + 30_000,
        },
        MisfirePolicy::RunOnce,
    );
    {
        let store = ScheduleStore::open(dir.path()).unwrap();
        for s in [&skip, &once, &all, &at] {
            store.insert(s).unwrap();
        }
    }
    // Daemon was off for 3 hours.
    let store = Arc::new(ScheduleStore::open(dir.path()).unwrap());
    let exec = RecordingExecutor::new();
    let clock = Arc::new(FakeClock::new(t0 + 3 * 3_600_000));
    let scheduler = Scheduler::start_with_clock(Arc::clone(&store), exec.clone(), clock.clone());
    scheduler.settle().await;
    let count = |id: TaskId| exec.recorded().iter().filter(|s| s.id == id).count();
    assert_eq!(count(skip.id), 0);
    assert_eq!(count(once.id), 1);
    assert_eq!(count(all.id), RUN_ALL_CAP);
    assert_eq!(count(at.id), 1);
    assert_eq!(store.get(at.id).unwrap().unwrap().next_run_at, None);
    scheduler.shutdown().await;

    // A second restart immediately afterwards must not replay anything.
    let exec2 = RecordingExecutor::new();
    let scheduler = Scheduler::start_with_clock(Arc::clone(&store), exec2.clone(), clock);
    scheduler.settle().await;
    assert_eq!(exec2.count(), 0);
    scheduler.shutdown().await;
}

#[tokio::test]
async fn backward_clock_jump_pulls_recurring_schedule_back() {
    let t0 = 1_700_000_000_000_i64;
    let store = Arc::new(ScheduleStore::open_in_memory().unwrap());
    let exec = RecordingExecutor::new();
    let clock = Arc::new(FakeClock::new(t0));
    let s = Schedule::new(
        BotId::new(),
        None,
        "tick",
        ScheduleSpec::Every { seconds: 60 },
        Payload::AgentPrompt { text: "x".into() },
        MisfirePolicy::Skip,
        t0,
    )
    .unwrap();
    let id = s.id;
    store.insert(&s).unwrap();
    // Simulate a row persisted under a clock that was an hour ahead.
    store.advance_next_run(id, Some(t0 + 3_600_000)).unwrap();
    let scheduler = Scheduler::start_with_clock(Arc::clone(&store), exec.clone(), clock.clone());
    scheduler.settle().await;
    clock.jump_wall_ms(-5_000);
    scheduler.settle().await;
    assert!(scheduler.wall_clock_jumps_detected() >= 1);
    assert_eq!(
        store.get(id).unwrap().unwrap().next_run_at,
        Some(t0 - 5_000 + 60_000)
    );
    assert_eq!(exec.count(), 0, "a backward jump must not fire anything");
    scheduler.shutdown().await;
}

#[tokio::test]
async fn webhook_rejects_empty_secret_disabled_and_concurrent_runs() {
    let t0 = 1_700_000_000_000_i64;
    let store = Arc::new(ScheduleStore::open_in_memory().unwrap());
    let (exec, entered, release) = RecordingExecutor::gated();
    let clock = Arc::new(FakeClock::new(t0));
    let s = Schedule::new(
        BotId::new(),
        None,
        "hook",
        ScheduleSpec::Every { seconds: 3_600 },
        Payload::Tool {
            name: "echo".into(),
            input: json!(null),
        },
        MisfirePolicy::Skip,
        t0,
    )
    .unwrap();
    let id = s.id;
    store.insert(&s).unwrap();
    assert!(
        store
            .set_trigger(
                id,
                &Trigger::Webhook {
                    secret: String::new()
                }
            )
            .is_err()
    );
    store
        .set_trigger(id, &Trigger::Webhook { secret: "k".into() })
        .unwrap();
    let scheduler = Scheduler::start_with_clock(Arc::clone(&store), exec.clone(), clock);

    // Unknown schedule id.
    assert!(matches!(
        scheduler.handle_webhook(TaskId::new(), "b", "00").await,
        Err(SchedulerError::NotFound(_))
    ));

    // First call blocks inside the executor; the second must be refused as busy.
    let sig = sign_webhook_body("k", b"body");
    let s2 = scheduler.clone();
    let sig2 = sig.clone();
    let first = tokio::spawn(async move { s2.handle_webhook(id, "body", &sig2).await });
    entered.notified().await;
    assert!(matches!(
        scheduler.handle_webhook(id, "body", &sig).await,
        Err(SchedulerError::Busy(_))
    ));
    release.notify_waiters();
    first.await.unwrap().unwrap();
    assert_eq!(exec.count(), 1);
    // A null tool input receives the raw body.
    assert_eq!(
        exec.recorded()[0].payload,
        Payload::Tool {
            name: "echo".into(),
            input: json!("body")
        }
    );

    // Disabled schedules refuse (after a valid signature).
    store.set_enabled(id, false, None).unwrap();
    assert!(matches!(
        scheduler.handle_webhook(id, "body", &sig).await,
        Err(SchedulerError::Disabled(_))
    ));
    // The webhook never rewrote next_run_at.
    assert_eq!(store.get(id).unwrap().unwrap().next_run_at, None);
    scheduler.shutdown().await;
}

#[tokio::test]
async fn run_now_and_trigger_webhook_return_before_the_execution_ends() {
    let store = Arc::new(ScheduleStore::open_in_memory().unwrap());
    let (exec, entered, release) = RecordingExecutor::gated();
    let s = Schedule::new(
        BotId::new(),
        None,
        "later",
        ScheduleSpec::Every { seconds: 3600 },
        Payload::AgentPrompt {
            text: "go {input}".into(),
        },
        MisfirePolicy::Skip,
        0,
    )
    .unwrap();
    let id = s.id;
    store.insert(&s).unwrap();
    store
        .set_trigger(id, &Trigger::Webhook { secret: "k".into() })
        .unwrap();
    let clock = Arc::new(FakeClock::new(0));
    let scheduler = Scheduler::start_with_clock(Arc::clone(&store), exec.clone(), clock);

    assert!(matches!(
        scheduler.run_now(TaskId::new()),
        Err(SchedulerError::NotFound(_))
    ));
    let waiting = entered.notified();
    tokio::pin!(waiting);
    waiting.as_mut().enable();
    scheduler.run_now(id).unwrap();
    waiting.await;
    assert!(scheduler.is_in_flight(id));
    // Busy while the first one runs, for both entry points.
    assert!(matches!(
        scheduler.run_now(id),
        Err(SchedulerError::Busy(_))
    ));
    let sig = sign_webhook_body("k", b"x");
    assert!(matches!(
        scheduler.trigger_webhook(id, "x", &sig),
        Err(SchedulerError::Busy(_))
    ));
    release.notify_one();
    scheduler.settle().await;
    assert!(!scheduler.is_in_flight(id));
    // run-now keeps the timer.
    assert_eq!(store.get(id).unwrap().unwrap().next_run_at, s.next_run_at);

    // Webhook: bad signature is refused up front; a good one runs with {input}.
    assert!(matches!(
        scheduler.trigger_webhook(id, "x", "sha256=00"),
        Err(SchedulerError::InvalidSignature)
    ));
    let waiting = entered.notified();
    tokio::pin!(waiting);
    waiting.as_mut().enable();
    scheduler.trigger_webhook(id, "x", &sig).unwrap();
    waiting.await;
    release.notify_one();
    scheduler.settle().await;
    let runs = exec.recorded();
    assert_eq!(runs.len(), 2);
    assert_eq!(
        runs[1].payload,
        Payload::AgentPrompt {
            text: "go x".into()
        }
    );
    scheduler.shutdown().await;
}
