use std::sync::Arc;

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use parking_lot::Mutex;
use protocol::{BotId, ChatId, TaskId};
use serde_json::json;
use tokio::sync::Notify;

use crate::{
    FakeClock, MisfirePolicy, Payload, RUN_ALL_CAP, Result, Schedule, ScheduleExecutor,
    ScheduleSpec, ScheduleStore, Scheduler, SchedulerError, Trigger, WebhookMeta, handle_webhook,
    parse_natural_language, parse_natural_language_with_tz, sign_webhook_body, sign_webhook_hex,
    verify_webhook_signature,
};

#[path = "consistency_tests.rs"]
mod consistency;

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

#[test]
fn offline_skip_keeps_a_retryable_reason_and_successful_retry_clears_it() {
    let store = ScheduleStore::open_in_memory().unwrap();
    let schedule = Schedule::new(
        BotId::new(),
        None,
        "offline",
        ScheduleSpec::Every { seconds: 60 },
        Payload::AgentPrompt { text: "go".into() },
        MisfirePolicy::Skip,
        0,
    )
    .unwrap();
    store.insert(&schedule).unwrap();
    let due = schedule.next_run_at.unwrap();
    store
        .reserve_occurrences(&schedule, &[due], Some(due + 60_000))
        .unwrap();
    store
        .claim_occurrence(schedule.id, schedule.revision, due, due)
        .unwrap()
        .unwrap();
    store.note_network_retry(schedule.id, due).unwrap();
    assert!(
        store
            .occurrence_note(schedule.id, due)
            .unwrap()
            .unwrap()
            .contains("Waiting")
    );
    store.note_offline(schedule.id, due, true).unwrap();
    store.finish_occurrence(schedule.id, due, false).unwrap();
    let row = store.get_occurrence(schedule.id, due).unwrap().unwrap();
    assert_eq!(row.state, crate::OccurrenceState::Missed);
    assert!(
        store
            .occurrence_note(schedule.id, due)
            .unwrap()
            .unwrap()
            .contains("Skipped")
    );
    store
        .claim_retry_occurrence(schedule.id, due, due + 30_000, &|_| crate::AttemptRun::None)
        .unwrap();
    store.finish_occurrence(schedule.id, due, true).unwrap();
    assert!(store.occurrence_note(schedule.id, due).unwrap().is_none());
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
            text: "Triage webhook payload: {input}".into(),
        }
    );

    assert_eq!(exec.recorded()[0].external_input.as_deref(), Some(body));

    // Also test separate external data via free `handle_webhook` function
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
            params: json!({ "event": "{input}" }),
        }
    );

    scheduler.shutdown().await;
}

fn webhook_fixture(
    payload: Payload,
) -> (
    Arc<ScheduleStore>,
    Arc<RecordingExecutor>,
    Scheduler,
    TaskId,
) {
    let t0 = 1_700_000_000_000_i64;
    let store = Arc::new(ScheduleStore::open_in_memory().unwrap());
    let exec = RecordingExecutor::new();
    let clock = Arc::new(FakeClock::new(t0));
    let sched = Schedule::new(
        BotId::new(),
        None,
        "hook",
        ScheduleSpec::Every { seconds: 3_600 },
        payload,
        MisfirePolicy::Skip,
        t0,
    )
    .unwrap();
    let id = sched.id;
    store.insert(&sched).unwrap();
    store
        .set_trigger(
            id,
            &Trigger::Webhook {
                secret: "s3cret".into(),
            },
        )
        .unwrap();
    let scheduler = Scheduler::start_with_clock(Arc::clone(&store), exec.clone(), clock);
    (store, exec, scheduler, id)
}

#[test]
fn webhook_does_not_inject_undeclared_input_param() {
    let p = Payload::Workflow {
        name: "w".into(),
        params: json!({ "a": 1 }),
    };
    assert_eq!(
        p.with_webhook_input("body"),
        Payload::Workflow {
            name: "w".into(),
            params: json!({ "a": 1 }),
        }
    );
    let n = Payload::Workflow {
        name: "w".into(),
        params: serde_json::Value::Null,
    };
    assert_eq!(
        n.with_webhook_input("body"),
        Payload::Workflow {
            name: "w".into(),
            params: serde_json::Value::Null,
        }
    );
}

#[tokio::test]
async fn webhook_delivery_id_replay_is_rejected() {
    let (_store, exec, scheduler, id) = webhook_fixture(Payload::AgentPrompt { text: "x".into() });
    let body = "payload";
    let sig = sign_webhook_body("s3cret", body.as_bytes());
    let meta = WebhookMeta {
        delivery_id: Some("d-1".into()),
        timestamp_ms: None,
    };
    scheduler
        .handle_webhook_with_meta(id, body, &sig, &meta)
        .await
        .unwrap();
    assert!(matches!(
        scheduler
            .handle_webhook_with_meta(id, body, &sig, &meta)
            .await,
        Err(SchedulerError::InvalidSignature)
    ));
    // The delivery id is not signed: a new id on a captured request is still a replay.
    let other = WebhookMeta {
        delivery_id: Some("d-2".into()),
        timestamp_ms: None,
    };
    assert!(matches!(
        scheduler
            .handle_webhook_with_meta(id, body, &sig, &other)
            .await,
        Err(SchedulerError::InvalidSignature)
    ));
    // A different (validly signed) body is a different delivery.
    let body2 = "payload-2";
    scheduler
        .handle_webhook_with_meta(
            id,
            body2,
            &sign_webhook_body("s3cret", body2.as_bytes()),
            &other,
        )
        .await
        .unwrap();
    assert_eq!(exec.count(), 2);
    scheduler.shutdown().await;
}

#[tokio::test]
async fn webhook_without_any_replay_headers_is_still_deduplicated() {
    let (_store, exec, scheduler, id) = webhook_fixture(Payload::AgentPrompt { text: "x".into() });
    let body = "payload";
    let sig = sign_webhook_body("s3cret", body.as_bytes());
    let meta = WebhookMeta::default();
    scheduler
        .handle_webhook_with_meta(id, body, &sig, &meta)
        .await
        .unwrap();
    assert!(matches!(
        scheduler
            .handle_webhook_with_meta(id, body, &sig, &meta)
            .await,
        Err(SchedulerError::InvalidSignature)
    ));
    assert_eq!(exec.count(), 1);
    scheduler.shutdown().await;
}

#[tokio::test]
async fn replay_is_remembered_until_the_timestamp_leaves_the_window() {
    use crate::webhook::{REPLAY_WINDOW_MS, ReplayGuard, prepare_webhook};
    let (store, _exec, scheduler, id) = webhook_fixture(Payload::AgentPrompt { text: "x".into() });
    let now = 1_700_000_000_000_i64;
    let ts = now + REPLAY_WINDOW_MS - 1_000; // accepted: just inside the future skew
    let body = "payload";
    let sig = sign_webhook_body("s3cret", format!("{ts}.{body}").as_bytes());
    let meta = WebhookMeta {
        delivery_id: None,
        timestamp_ms: Some(ts),
    };
    let guard = ReplayGuard::default();
    let (_, pending) = prepare_webhook(&store, id, body, &sig, now, &meta, Some(&guard)).unwrap();
    assert!(guard.commit(pending.unwrap(), now));
    // Just after `now + window` the old logic had forgotten it; the timestamp is still valid.
    // `prepare_webhook` no longer refuses replays itself (that happens at `commit`, after
    // the Busy check), so the second attempt authenticates but cannot be committed.
    let later = now + REPLAY_WINDOW_MS + 500;
    let (_, pending) = prepare_webhook(&store, id, body, &sig, later, &meta, Some(&guard)).unwrap();
    assert!(!guard.commit(pending.unwrap(), later));
    // Once the timestamp is out of the window the request fails on staleness anyway.
    let much_later = ts + REPLAY_WINDOW_MS + 1;
    assert!(matches!(
        prepare_webhook(&store, id, body, &sig, much_later, &meta, Some(&guard)),
        Err(SchedulerError::InvalidSignature)
    ));
    scheduler.shutdown().await;
}

#[tokio::test]
async fn disabled_or_busy_delivery_can_be_retried() {
    let (store, exec, scheduler, id) = webhook_fixture(Payload::AgentPrompt { text: "x".into() });
    let body = "payload";
    let sig = sign_webhook_body("s3cret", body.as_bytes());
    let meta = WebhookMeta {
        delivery_id: Some("gh-guid".into()),
        timestamp_ms: None,
    };
    store.set_enabled(id, false, None).unwrap();
    assert!(matches!(
        scheduler
            .handle_webhook_with_meta(id, body, &sig, &meta)
            .await,
        Err(SchedulerError::Disabled(_))
    ));
    store.set_enabled(id, true, Some(i64::MAX)).unwrap();
    // Same delivery (same GUID, same body) redelivered: accepted, not 401.
    scheduler
        .handle_webhook_with_meta(id, body, &sig, &meta)
        .await
        .unwrap();
    assert_eq!(exec.count(), 1);
    // Now it really is a duplicate.
    assert!(matches!(
        scheduler
            .handle_webhook_with_meta(id, body, &sig, &meta)
            .await,
        Err(SchedulerError::InvalidSignature)
    ));
    scheduler.shutdown().await;
}

#[tokio::test]
async fn webhook_timestamp_window_is_enforced() {
    let (_store, exec, scheduler, id) = webhook_fixture(Payload::AgentPrompt { text: "x".into() });
    let now = 1_700_000_000_000_i64; // FakeClock wall time
    let body = "payload";
    let sign = |ts: i64| sign_webhook_body("s3cret", format!("{ts}.{body}").as_bytes());
    let ok = WebhookMeta {
        delivery_id: None,
        timestamp_ms: Some(now - 60_000),
    };
    scheduler
        .handle_webhook_with_meta(id, body, &sign(now - 60_000), &ok)
        .await
        .unwrap();
    let stale = WebhookMeta {
        delivery_id: None,
        timestamp_ms: Some(now - 10 * 60_000),
    };
    assert!(matches!(
        scheduler
            .handle_webhook_with_meta(id, body, &sign(now - 10 * 60_000), &stale)
            .await,
        Err(SchedulerError::InvalidSignature)
    ));
    // Body-only signature does not validate when a timestamp is claimed.
    assert!(matches!(
        scheduler
            .handle_webhook_with_meta(id, body, &sign_webhook_body("s3cret", body.as_bytes()), &ok)
            .await,
        Err(SchedulerError::InvalidSignature)
    ));
    assert_eq!(exec.count(), 1);
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
fn natural_language_parser_supports_additional_turkish_phrases() {
    let now_ms = 1_700_000_000_000_i64;
    let cases = [
        ("hafta içleri", ScheduleSpec::Cron("0 9 * * 1-5".into())),
        (
            "hafta içleri saat 09:00",
            ScheduleSpec::Cron("0 9 * * 1-5".into()),
        ),
        (
            "HAFTA İÇLERİ saat 17.45’te",
            ScheduleSpec::Cron("45 17 * * 1-5".into()),
        ),
        ("yarım saatte bir", ScheduleSpec::Every { seconds: 1_800 }),
        (
            "bir buçuk saat sonra",
            ScheduleSpec::At {
                unix_ms: now_ms + 5_400_000,
            },
        ),
    ];

    for (phrase, expected) in cases {
        assert_eq!(
            parse_natural_language(phrase, now_ms).as_ref(),
            Some(&expected),
            "mismatch on phrase '{phrase}'"
        );
        expected.validate().expect("parsed spec must be valid");
    }

    assert_eq!(
        parse_natural_language_with_tz("hafta içleri saat 09:00", now_ms, Some("Europe/Istanbul")),
        Some(ScheduleSpec::Cron("0 9 * * 1-5 Europe/Istanbul".into()))
    );

    for ambiguous in ["yarın sabah", "hafta sonuna doğru", "birkaç saat sonra"] {
        assert_eq!(
            parse_natural_language(ambiguous, now_ms),
            None,
            "expected '{ambiguous}' to return None"
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
    // The executor receives raw external data separately from the trusted definition.
    assert_eq!(exec.recorded()[0].external_input.as_deref(), Some("body"));
    assert_eq!(
        exec.recorded()[0].payload,
        Payload::Tool {
            name: "echo".into(),
            input: json!(null)
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
        scheduler.run_now(TaskId::new(), true),
        Err(SchedulerError::NotFound(_))
    ));
    let waiting = entered.notified();
    tokio::pin!(waiting);
    waiting.as_mut().enable();
    scheduler.run_now(id, true).unwrap();
    waiting.await;
    assert!(scheduler.is_in_flight(id));
    // Busy while the first one runs, for both entry points.
    assert!(matches!(
        scheduler.run_now(id, true),
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
            text: "go {input}".into()
        }
    );
    scheduler.shutdown().await;
}

#[test]
fn next_occurrences_preview_specs_timezones_and_dst_edges() {
    // Interval: 5 strictly increasing runs spaced by 15 minutes.
    let every = ScheduleSpec::Every { seconds: 900 };
    let t0 = utc_ms(2026, 3, 7, 10, 0, 0);
    let runs = every.next_occurrences(t0, 5).unwrap();
    assert_eq!(runs.len(), 5);
    assert!(runs.windows(2).all(|w| w[1] - w[0] == 900_000));

    // One-shot: returns single future timestamp, or empty when past.
    let future_at = ScheduleSpec::At {
        unix_ms: t0 + 60_000,
    };
    assert_eq!(
        future_at.next_occurrences(t0, 5).unwrap(),
        vec![t0 + 60_000]
    );
    assert_eq!(
        future_at.next_occurrences(t0 + 60_000, 5).unwrap(),
        Vec::<i64>::new()
    );

    // Weekly and monthly in Europe/Berlin across spring-forward (2026-03-29 02:00 -> 03:00 CEST).
    // Before 2026-03-29: CET = UTC+1 (09:00 local = 08:00 UTC).
    // On/after 2026-03-29: CEST = UTC+2 (09:00 local = 07:00 UTC).
    let weekly_berlin = ScheduleSpec::cron_tz("0 9 * * 0", "Europe/Berlin");
    let weekly_runs = weekly_berlin
        .next_occurrences(utc_ms(2026, 3, 20, 0, 0, 0), 5)
        .unwrap();
    assert_eq!(
        weekly_runs,
        vec![
            utc_ms(2026, 3, 22, 8, 0, 0), // CET
            utc_ms(2026, 3, 29, 7, 0, 0), // CEST (DST Sunday)
            utc_ms(2026, 4, 5, 7, 0, 0),
            utc_ms(2026, 4, 12, 7, 0, 0),
            utc_ms(2026, 4, 19, 7, 0, 0),
        ]
    );

    let monthly_berlin = ScheduleSpec::cron_tz("0 9 1 * *", "Europe/Berlin");
    let monthly_runs = monthly_berlin
        .next_occurrences(utc_ms(2026, 2, 15, 0, 0, 0), 5)
        .unwrap();
    assert_eq!(
        monthly_runs,
        vec![
            utc_ms(2026, 3, 1, 8, 0, 0), // CET
            utc_ms(2026, 4, 1, 7, 0, 0), // CEST
            utc_ms(2026, 5, 1, 7, 0, 0),
            utc_ms(2026, 6, 1, 7, 0, 0),
            utc_ms(2026, 7, 1, 7, 0, 0),
        ]
    );

    // Daily in America/New_York across fall-back (2026-11-01 02:00 EDT -> 01:00 EST).
    let daily_ny = ScheduleSpec::cron_tz("30 1 * * *", "America/New_York");
    let ny_runs = daily_ny
        .next_occurrences(utc_ms(2026, 10, 30, 0, 0, 0), 5)
        .unwrap();
    assert_eq!(
        ny_runs,
        vec![
            utc_ms(2026, 10, 30, 5, 30, 0), // EDT (UTC-4)
            utc_ms(2026, 10, 31, 5, 30, 0), // EDT (UTC-4)
            utc_ms(2026, 11, 1, 5, 30, 0),  // first 01:30 EDT on fall-back day (no duplicate)
            utc_ms(2026, 11, 2, 6, 30, 0),  // EST (UTC-5)
            utc_ms(2026, 11, 3, 6, 30, 0),  // EST (UTC-5)
        ]
    );

    // Invalid spec is still rejected even for count = 0.
    assert!(
        ScheduleSpec::cron("0 9 * * * Mars/Olympus")
            .next_occurrences(t0, 0)
            .is_err()
    );
}

#[tokio::test]
async fn retry_occurrence_is_idempotent_uses_current_spec_and_preserves_timer() {
    let dir = tempfile::tempdir().unwrap();
    let t0 = 1_700_000_000_000_i64;
    let bot = BotId::new();
    let sched = Schedule::new(
        bot,
        None,
        "missed-job",
        ScheduleSpec::Every { seconds: 60 },
        Payload::AgentPrompt { text: "old".into() },
        MisfirePolicy::Skip,
        t0,
    )
    .unwrap();
    let id = sched.id;
    let due_ms = sched.next_run_at.unwrap();

    // Seed a pending occurrence and crash-recover it into `missed`.
    {
        let store = ScheduleStore::open(dir.path()).unwrap();
        store.insert(&sched).unwrap();
        assert!(
            store
                .reserve_occurrences(&sched, &[due_ms], Some(due_ms + 60_000))
                .unwrap()
        );
        assert_eq!(store.recover_unresolved_occurrences().unwrap(), 1);
        let occ = store.get_occurrence(id, due_ms).unwrap().unwrap();
        assert_eq!(occ.state, crate::OccurrenceState::Missed);
    }

    // Edit the prompt before retrying so we verify retry uses the latest definition.
    let store = Arc::new(ScheduleStore::open(dir.path()).unwrap());
    let mut edited = store.get(id).unwrap().unwrap();
    edited.payload = Payload::AgentPrompt {
        text: "updated".into(),
    };
    assert!(store.update_definition(&edited).unwrap());
    let next_before = store.get(id).unwrap().unwrap().next_run_at;

    let exec = RecordingExecutor::new();
    let clock = Arc::new(FakeClock::new(t0 + 10_000));
    let scheduler = Scheduler::start_with_clock(Arc::clone(&store), exec.clone(), clock);
    scheduler.settle().await;
    assert_eq!(exec.count(), 0);

    // First retry succeeds and runs once with the updated payload.
    scheduler.retry_occurrence(id, due_ms).unwrap();
    scheduler.settle().await;
    assert_eq!(exec.count(), 1);
    assert_eq!(
        exec.recorded()[0].payload,
        Payload::AgentPrompt {
            text: "updated".into()
        }
    );
    let occ = store.get_occurrence(id, due_ms).unwrap().unwrap();
    assert_eq!(occ.state, crate::OccurrenceState::Succeeded);
    assert_eq!(store.get(id).unwrap().unwrap().next_run_at, next_before);

    // Second retry on the now-succeeded occurrence is refused and never double-fires.
    assert!(matches!(
        scheduler.retry_occurrence(id, due_ms),
        Err(SchedulerError::Conflict(_))
    ));
    scheduler.settle().await;
    assert_eq!(exec.count(), 1);

    // Unknown occurrence returns NotFound.
    assert!(matches!(
        scheduler.retry_occurrence(id, due_ms + 999),
        Err(SchedulerError::NotFound(_))
    ));
    scheduler.shutdown().await;
}

fn dispatched_row(store: &ScheduleStore, t0: i64) -> (Schedule, i64) {
    let sched = Schedule::new(
        BotId::new(),
        None,
        "occ",
        ScheduleSpec::Every { seconds: 60 },
        Payload::AgentPrompt { text: "p".into() },
        MisfirePolicy::Skip,
        t0,
    )
    .unwrap();
    let due = sched.next_run_at.unwrap();
    store.insert(&sched).unwrap();
    assert!(
        store
            .reserve_occurrences(&sched, &[due], Some(due + 60_000))
            .unwrap()
    );
    (sched, due)
}

/// Executor whose durable run key reports a fixed outcome for every attempt.
struct KeyedExecutor(crate::AttemptRun);

#[async_trait]
impl ScheduleExecutor for KeyedExecutor {
    async fn execute(&self, _schedule: &Schedule) -> Result<()> {
        Ok(())
    }

    fn attempt_run(&self, _id: TaskId, _due: i64, _attempt: u32) -> crate::AttemptRun {
        self.0
    }
}

#[tokio::test]
async fn startup_settles_an_interrupted_occurrence_whose_run_key_says_done() {
    use crate::{AttemptRun, OccurrenceState};
    let store = Arc::new(ScheduleStore::open_in_memory().unwrap());
    let (sched, due) = dispatched_row(&store, 1_700_000_000_000);
    store
        .claim_occurrence(sched.id, sched.revision, due, 1)
        .unwrap()
        .unwrap();
    // The side effect finished, then the process was killed before the outcome was written.
    let first = Scheduler::start_with_clock(
        Arc::clone(&store),
        Arc::new(KeyedExecutor(AttemptRun::Succeeded)),
        Arc::new(FakeClock::new(1_700_000_000_000)),
    );
    first.settle().await;
    let occ = store.get_occurrence(sched.id, due).unwrap().unwrap();
    assert_eq!(occ.state, OccurrenceState::Succeeded);
}

#[test]
fn retry_after_restart_never_restarts_a_run_that_already_started() {
    use crate::AttemptRun;
    let store = ScheduleStore::open_in_memory().unwrap();
    let (sched, due) = dispatched_row(&store, 1_700_000_000_000);
    let id = sched.id;
    // The timer dispatched it and the run started (run id recorded)...
    store
        .claim_occurrence(id, sched.revision, due, 1)
        .unwrap()
        .unwrap();
    assert!(store.set_occurrence_run_id(id, due, "run-1").unwrap());
    // ...then the daemon restarted.
    assert_eq!(store.recover_unresolved_occurrences().unwrap(), 1);
    let occ = store.get_occurrence(id, due).unwrap().unwrap();
    assert_eq!(occ.state, crate::OccurrenceState::Interrupted);
    assert_eq!(occ.run_id.as_deref(), Some("run-1"));

    // Run still alive / paused / waiting: retry is refused, row untouched.
    let err = store
        .claim_retry_occurrence(id, due, 100_000, &|_| AttemptRun::Active)
        .unwrap_err();
    assert!(matches!(err, SchedulerError::Conflict(_)), "{err}");
    let occ = store.get_occurrence(id, due).unwrap().unwrap();
    assert_eq!(occ.state, crate::OccurrenceState::Interrupted);
    assert_eq!(
        occ.run_id.as_deref(),
        Some("run-1"),
        "run_id is never cleared"
    );
    assert_eq!(occ.retry_count, 0);

    // Run ended failed: retry moves to the next attempt key, run_id kept.
    let (_, attempt) = store
        .claim_retry_occurrence(id, due, 100_000, &|a| {
            assert_eq!(a, 0);
            AttemptRun::Failed
        })
        .unwrap();
    assert_eq!(attempt, 1);
    let occ = store.get_occurrence(id, due).unwrap().unwrap();
    assert_eq!(occ.state, crate::OccurrenceState::Dispatched);
    assert_eq!(occ.run_id.as_deref(), Some("run-1"));
}

#[test]
fn missed_row_retry_works_once_and_reuses_the_attempt_key() {
    use crate::AttemptRun;
    let store = ScheduleStore::open_in_memory().unwrap();
    let (sched, due) = dispatched_row(&store, 1_700_000_000_000);
    assert_eq!(store.recover_unresolved_occurrences().unwrap(), 1);
    let (_, attempt) = store
        .claim_retry_occurrence(sched.id, due, 5, &|_| AttemptRun::None)
        .unwrap();
    assert_eq!(attempt, 0);
    assert!(matches!(
        store.claim_retry_occurrence(sched.id, due, 999_999, &|_| AttemptRun::None),
        Err(SchedulerError::Conflict(_))
    ));
}

#[test]
fn concurrent_double_retry_claims_the_occurrence_once() {
    use crate::AttemptRun;
    let store = Arc::new(ScheduleStore::open_in_memory().unwrap());
    let (sched, due) = dispatched_row(&store, 1_700_000_000_000);
    store.recover_unresolved_occurrences().unwrap();
    let wins: usize = (0..2)
        .map(|_| {
            let store = Arc::clone(&store);
            let id = sched.id;
            std::thread::spawn(move || {
                store
                    .claim_retry_occurrence(id, due, 10, &|_| AttemptRun::None)
                    .is_ok()
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|h| usize::from(h.join().unwrap()))
        .sum();
    assert_eq!(wins, 1);
}

#[test]
fn retry_is_refused_for_disabled_schedules_inside_the_cooldown_and_after_the_cap() {
    use crate::{AttemptRun, MAX_OCCURRENCE_RETRIES, RETRY_COOLDOWN_MS};
    let store = ScheduleStore::open_in_memory().unwrap();
    let (sched, due) = dispatched_row(&store, 1_700_000_000_000);
    let id = sched.id;
    store.recover_unresolved_occurrences().unwrap();

    // Disabled schedule: refused, the row stays retryable for later.
    let mut off = store.get(id).unwrap().unwrap();
    off.enabled = false;
    assert!(store.update_definition(&off).unwrap());
    assert!(matches!(
        store.claim_retry_occurrence(id, due, 1, &|_| AttemptRun::None),
        Err(SchedulerError::Disabled(_))
    ));
    let mut on = store.get(id).unwrap().unwrap();
    on.enabled = true;
    assert!(store.update_definition(&on).unwrap());

    let mut now = 1_000;
    for n in 0..MAX_OCCURRENCE_RETRIES {
        store
            .claim_retry_occurrence(id, due, now, &|_| AttemptRun::Failed)
            .unwrap();
        store.finish_occurrence(id, due, false).unwrap();
        // Inside the cooldown a second try is rate limited.
        if n + 1 < MAX_OCCURRENCE_RETRIES {
            assert!(
                matches!(
                    store.claim_retry_occurrence(id, due, now + 1_000, &|_| AttemptRun::Failed),
                    Err(SchedulerError::RateLimited(_))
                ),
                "retry {n}"
            );
        }
        now += RETRY_COOLDOWN_MS + 1;
    }
    let err = store
        .claim_retry_occurrence(id, due, now, &|_| AttemptRun::Failed)
        .unwrap_err();
    assert!(matches!(err, SchedulerError::Conflict(_)), "{err}");
}

#[tokio::test]
async fn retry_that_did_not_run_is_recorded_failed_not_succeeded() {
    let t0 = 1_700_000_000_000_i64;
    let store = Arc::new(ScheduleStore::open_in_memory().unwrap());
    let (sched, due) = dispatched_row(&store, t0);
    store.recover_unresolved_occurrences().unwrap();
    let exec = RecordingExecutor::new();
    let clock = Arc::new(FakeClock::new(t0 + 10_000));
    let scheduler = Scheduler::start_with_clock(Arc::clone(&store), exec.clone(), clock);
    scheduler.settle().await;
    // The schedule moves to a server before the retry starts: the engine
    // skips it, which must not read as success.
    assert!(store.freeze(sched.id, "host").unwrap());
    let outcome =
        crate::engine::execute_checked(&store, exec.as_ref(), &sched, Some((due, 0)), t0, false)
            .await
            .unwrap();
    assert_eq!(outcome, crate::engine::Dispatch::Skipped);
    assert_eq!(exec.count(), 0);
    scheduler.shutdown().await;
}

#[test]
fn cron_at_a_nonexistent_dst_local_time_fires_once_at_the_transition_instant() {
    // 2026-03-08 02:30 does not exist in America/New_York (02:00 EST -> 03:00 EDT).
    // croner moves such a run forward to the transition instant (03:00 EDT =
    // 07:00 UTC) on that day only; the other days stay at 02:30 local.
    let before = utc_ms(2026, 3, 7, 12, 0, 0);
    let spec = ScheduleSpec::cron_tz("30 2 * * *", "America/New_York");
    let runs = spec.next_occurrences(before, 3).unwrap();
    assert_eq!(
        runs,
        vec![
            utc_ms(2026, 3, 8, 7, 0, 0),  // gap day: 03:00 EDT, exactly once
            utc_ms(2026, 3, 9, 6, 30, 0), // 02:30 EDT
            utc_ms(2026, 3, 10, 6, 30, 0),
        ]
    );
}

/// croner 5 rejects weekday/month names in the wrong field instead of silently reading them as
/// numbers (`0 0 * * jan` used to run on Mondays); a bare `L` in day-of-week is rejected too.
/// Names in their own fields and a `SUN-SUN` range keep working and mean what they say.
#[test]
fn cron_names_in_wrong_fields_are_rejected_and_right_fields_work() {
    for bad in ["0 0 * * jan", "0 0 * mon *", "0 0 * * L"] {
        assert!(
            crate::ParsedCron::parse(bad).is_err(),
            "{bad} must be rejected"
        );
    }
    let sundays = crate::ParsedCron::parse("0 0 * * SUN-SUN").expect("SUN-SUN parses");
    // 2026-11-26 is a Thursday: the next run is Sunday 2026-11-29 00:00 UTC, not the next day.
    let after = Utc
        .with_ymd_and_hms(2026, 11, 26, 12, 0, 0)
        .unwrap()
        .timestamp_millis();
    let next = sundays.next_after(after).unwrap().unwrap();
    assert_eq!(
        next,
        Utc.with_ymd_and_hms(2026, 11, 29, 0, 0, 0)
            .unwrap()
            .timestamp_millis()
    );
    assert!(crate::ParsedCron::parse("0 9 * jan-mar MON-FRI").is_ok());
}
