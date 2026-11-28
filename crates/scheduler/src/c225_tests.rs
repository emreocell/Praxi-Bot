//! Enabled feature tests; all clocks, sources and model-start callbacks are fakes.
use crate::*;
use async_trait::async_trait;
use parking_lot::Mutex;
use script_runtime::ScriptHost;
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

struct FakeRead(Mutex<Value>);
#[async_trait]
impl ScriptHost for FakeRead {
    async fn call_tool(&self, _: &str, _: Value) -> std::result::Result<Value, String> {
        Ok(self.0.lock().clone())
    }
}
struct Host {
    read: Arc<FakeRead>,
    runs: Mutex<Vec<Schedule>>,
    entries: Mutex<Vec<watch::Observation>>,
    fail: AtomicBool,
    hang: AtomicBool,
}
impl Host {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            read: Arc::new(FakeRead(Mutex::new(json!("same")))),
            runs: Mutex::new(Vec::new()),
            entries: Mutex::new(Vec::new()),
            fail: AtomicBool::new(false),
            hang: AtomicBool::new(false),
        })
    }
}
#[async_trait]
impl ScheduleExecutor for Host {
    async fn execute(&self, s: &Schedule) -> Result<()> {
        self.runs.lock().push(s.clone());
        Ok(())
    }
    async fn precheck(&self, s: &Schedule, previous: Option<&str>) -> Result<precheck::Outcome> {
        if self.hang.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        if self.fail.load(Ordering::SeqCst) {
            return Err(SchedulerError::Execution(
                "upstream secret error must not escape".into(),
            ));
        }
        precheck::evaluate(s.precheck.as_ref().unwrap(), previous, self.read.clone()).await
    }
    async fn poll_trigger(&self, _: &Schedule, _: &Trigger) -> Result<Vec<watch::Observation>> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(SchedulerError::Execution("private source failure".into()));
        }
        Ok(self.entries.lock().clone())
    }
}
fn definition() -> Schedule {
    Schedule::new(
        BotId::new(),
        None,
        "check",
        ScheduleSpec::Every { seconds: 1 },
        Payload::AgentPrompt {
            text: "review {input}".into(),
        },
        MisfirePolicy::Skip,
        1000,
    )
    .unwrap()
}

#[tokio::test]
async fn c225_hash_check_skips_three_without_run_then_starts_once_and_persists() {
    let store = Arc::new(ScheduleStore::open_in_memory().unwrap());
    let host = Host::new();
    let clock = Arc::new(FakeClock::new(1000));
    let mut s = definition();
    s.precheck = Some(Precheck::HashChanged {
        tool: "read".into(),
        input: json!({}),
    });
    store.insert(&s).unwrap();
    let scheduler = Scheduler::start_with_clock(store.clone(), host.clone(), clock.clone());
    for _ in 0..3 {
        clock.advance_ms(1000);
        scheduler.settle().await;
    }
    assert_eq!(host.runs.lock().len(), 0);
    assert_eq!(store.check_state(&s).unwrap().skipped_by_precheck, 3);
    *host.read.0.lock() = json!("changed");
    clock.advance_ms(1000);
    scheduler.settle().await;
    assert_eq!(host.runs.lock().len(), 1);
    assert!(
        host.runs.lock()[0]
            .external_input
            .as_ref()
            .unwrap()
            .contains("changed")
    );
    scheduler.shutdown().await;
}

#[tokio::test]
async fn c225_ten_check_errors_fail_open_with_fixed_note_then_pause() {
    let store = Arc::new(ScheduleStore::open_in_memory().unwrap());
    let host = Host::new();
    host.fail.store(true, Ordering::SeqCst);
    let clock = Arc::new(FakeClock::new(1000));
    let mut s = definition();
    s.precheck = Some(Precheck::Script {
        code: "return true;".into(),
    });
    store.insert(&s).unwrap();
    let scheduler = Scheduler::start_with_clock(store.clone(), host.clone(), clock.clone());
    for _ in 0..10 {
        clock.advance_ms(1000);
        scheduler.settle().await;
    }
    assert_eq!(host.runs.lock().len(), 10);
    assert!(host.runs.lock().iter().all(|s| s.precheck_failed));
    assert!(!store.get(s.id).unwrap().unwrap().enabled);
    assert_eq!(store.check_state(&s).unwrap().precheck_errors, 10);
    clock.advance_ms(1000);
    scheduler.settle().await;
    assert_eq!(host.runs.lock().len(), 10);
    scheduler.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn c225_host_timeout_fails_open_without_waiting_for_unattended_approval() {
    let store = Arc::new(ScheduleStore::open_in_memory().unwrap());
    let host = Host::new();
    host.hang.store(true, Ordering::SeqCst);
    let clock = Arc::new(FakeClock::new(1000));
    let mut s = definition();
    s.precheck = Some(Precheck::Script {
        code: "return true;".into(),
    });
    store.insert(&s).unwrap();
    let scheduler = Scheduler::start_with_clock(store, host.clone(), clock);
    scheduler.run_now(s.id, true).unwrap();
    tokio::task::yield_now().await;
    assert_eq!(host.runs.lock().len(), 0);
    tokio::time::advance(precheck::TIMEOUT).await;
    scheduler.settle().await;
    assert_eq!(host.runs.lock().len(), 1);
    assert!(host.runs.lock()[0].precheck_failed);
    scheduler.shutdown().await;
}

#[tokio::test]
async fn c225_file_trigger_polling_disables_timer_and_coalesces_five_names() {
    let store = Arc::new(ScheduleStore::open_in_memory().unwrap());
    let host = Host::new();
    let clock = Arc::new(FakeClock::new(1000));
    let s = definition();
    let trigger = Trigger::FileWatch {
        path: "workspace".into(),
        glob: "*.txt".into(),
        events: vec![FileEvent::Created],
        debounce_ms: 1000,
    };
    store.insert_config(&s, Some(&trigger)).unwrap();
    let scheduler = Scheduler::start_with_clock(store.clone(), host.clone(), clock.clone());
    scheduler.settle().await;
    assert_eq!(store.list_due(i64::MAX).unwrap().len(), 0);
    *host.entries.lock() = (0..5)
        .map(|n| watch::Observation {
            key: format!("file{n}.txt"),
            fingerprint: "metadata".into(),
            input: json!(format!("file{n}.txt")),
        })
        .collect();
    clock.advance_ms(30_000);
    scheduler.settle().await;
    assert_eq!(host.runs.lock().len(), 0);
    clock.advance_ms(1000);
    scheduler.settle().await;
    assert_eq!(host.runs.lock().len(), 1);
    assert_eq!(
        serde_json::from_str::<Vec<String>>(host.runs.lock()[0].external_input.as_ref().unwrap())
            .unwrap()
            .len(),
        5
    );
    clock.advance_ms(30_000);
    scheduler.settle().await;
    assert_eq!(host.runs.lock().len(), 1);
    scheduler.shutdown().await;
}

#[tokio::test]
async fn c225_email_trigger_new_identity_starts_one_run_and_ten_source_errors_pause() {
    let store = Arc::new(ScheduleStore::open_in_memory().unwrap());
    let host = Host::new();
    let clock = Arc::new(FakeClock::new(1000));
    let s = definition();
    let trigger = Trigger::Email {
        account: "fake".into(),
        from_filter: "sender@example.test".into(),
        subject_filter: None,
    };
    store.insert_config(&s, Some(&trigger)).unwrap();
    let scheduler = Scheduler::start_with_clock(store.clone(), host.clone(), clock.clone());
    scheduler.settle().await;
    *host.entries.lock() = vec![watch::Observation {
        key: "uid:1".into(),
        fingerprint: "header".into(),
        input: json!({"message_id":"uid:1","from":"sender@example.test","subject":"notice"}),
    }];
    for _ in 0..2 {
        clock.advance_ms(watch::EMAIL_POLL_MS);
        scheduler.settle().await;
    }
    assert_eq!(host.runs.lock().len(), 1);
    assert!(
        host.runs.lock()[0]
            .external_input
            .as_ref()
            .unwrap()
            .contains("uid:1")
    );
    host.fail.store(true, Ordering::SeqCst);
    for _ in 0..10 {
        clock.advance_ms(watch::EMAIL_POLL_MS);
        scheduler.settle().await;
    }
    assert!(!store.get(s.id).unwrap().unwrap().enabled);
    assert_eq!(store.check_state(&s).unwrap().trigger_errors, 10);
    assert_eq!(host.runs.lock().len(), 1);
    scheduler.shutdown().await;
}

#[test]
fn c225_old_webhook_database_migrates_without_changing_secret_or_payload() {
    let dir = tempfile::tempdir().unwrap();
    let id = TaskId::new();
    let conn = rusqlite::Connection::open(dir.path().join("schedules.db")).unwrap();
    for sql in [
        include_str!("../migrations/0001_schedules.sql"),
        include_str!("../migrations/0002_schedule_consistency.sql"),
        include_str!("../migrations/0003_occurrence_recovery.sql"),
        include_str!("../migrations/0004_schedule_origin.sql"),
    ] {
        conn.execute_batch(sql).unwrap();
    }
    conn.pragma_update(None, "user_version", 4).unwrap();
    conn.execute("INSERT INTO schedules(id,bot_id,name,spec_json,payload_json,enabled,created_at_ms,misfire_policy,origin) VALUES (?1,?2,'legacy',?3,?4,1,1000,'skip','user')",rusqlite::params![id.to_string(),BotId::new().to_string(),r#"{"every":{"seconds":60}}"#,r#"{"kind":"agent_prompt","text":"legacy {input}"}"#]).unwrap();
    conn.execute(
        "INSERT INTO schedule_triggers(schedule_id,trigger_json,created_at_ms) VALUES (?1,?2,1000)",
        rusqlite::params![
            id.to_string(),
            r#"{"kind":"webhook","secret":"old-fixture"}"#
        ],
    )
    .unwrap();
    drop(conn);
    let store = ScheduleStore::open(dir.path()).unwrap();
    let row = store.get(id).unwrap().unwrap();
    assert_eq!(row.precheck, None);
    assert_eq!(
        row.payload,
        Payload::AgentPrompt {
            text: "legacy {input}".into()
        }
    );
    assert_eq!(
        store.get_trigger(id).unwrap(),
        Some(Trigger::Webhook {
            secret: "old-fixture".into()
        })
    );
}
