use std::sync::atomic::{AtomicU32, Ordering};

use super::*;

fn quick(max_crashes: u32) -> Policy {
    Policy {
        max_crashes,
        backoff_base: Duration::from_millis(5),
        backoff_max: Duration::from_millis(20),
        stable_after: Duration::from_secs(60),
    }
}

fn recorder(guard: &Guard) -> Arc<Mutex<Vec<LoopEvent>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    guard.set_notifier(Arc::new(move |e| sink.lock().push(*e)));
    seen
}

#[tokio::test]
async fn a_panicking_loop_is_restarted_and_the_owner_hears_once() {
    let guard = Guard::new(quick(5));
    let seen = recorder(&guard);
    let starts = Arc::new(AtomicU32::new(0));
    let cancel = CancellationToken::new();
    let counter = Arc::clone(&starts);
    let token = cancel.clone();
    let handle = guard.supervise("demo", cancel.clone(), move || {
        let n = counter.fetch_add(1, Ordering::SeqCst);
        let token = token.clone();
        async move {
            assert!(n >= 2, "boom {n}");
            token.cancelled().await;
        }
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while starts.load(Ordering::SeqCst) < 3 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let status = guard.statuses().pop().unwrap();
    assert_eq!(status.state, LoopState::Running);
    assert_eq!((status.restarts, status.crashes_in_row), (2, 2));
    assert!(!guard.is_degraded());
    // Two crashes in one streak: one notice.
    assert_eq!(
        *seen.lock(),
        vec![LoopEvent {
            name: "demo",
            kind: EventKind::Restarted,
            crashes: 1
        }]
    );
    cancel.cancel();
    handle.await.unwrap();
}

#[tokio::test]
async fn too_many_crashes_in_a_row_give_up_and_say_so() {
    let guard = Guard::new(quick(3));
    let seen = recorder(&guard);
    let starts = Arc::new(AtomicU32::new(0));
    let counter = Arc::clone(&starts);
    let handle = guard.supervise("demo", CancellationToken::new(), move || {
        counter.fetch_add(1, Ordering::SeqCst);
        async { panic!("always") }
    });
    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(starts.load(Ordering::SeqCst), 3);
    assert!(guard.is_degraded());
    assert_eq!(guard.statuses()[0].state, LoopState::Degraded);
    let kinds: Vec<_> = seen.lock().iter().map(|e| (e.kind, e.crashes)).collect();
    assert_eq!(
        kinds,
        vec![(EventKind::Restarted, 1), (EventKind::GaveUp, 3)]
    );
}

#[tokio::test(start_paused = true)]
async fn a_long_healthy_stretch_resets_the_streak() {
    let guard = Guard::new(quick(3));
    assert!(matches!(
        guard.record_crash("demo", Cause::Panicked),
        Verdict::Retry(_)
    ));
    assert!(matches!(
        guard.record_crash("demo", Cause::Panicked),
        Verdict::Retry(_)
    ));
    tokio::time::advance(Duration::from_secs(61)).await;
    assert!(matches!(
        guard.record_crash("demo", Cause::Panicked),
        Verdict::Retry(_)
    ));
    let status = guard.statuses().pop().unwrap();
    assert_eq!((status.restarts, status.crashes_in_row), (3, 1));
    assert!(!guard.is_degraded());
}

#[tokio::test(start_paused = true)]
async fn the_backoff_doubles_up_to_its_cap() {
    let guard = Guard::new(Policy {
        max_crashes: 10,
        backoff_base: Duration::from_secs(1),
        backoff_max: Duration::from_secs(5),
        stable_after: Duration::from_secs(600),
    });
    let waits: Vec<_> = (0..5)
        .map(|_| match guard.record_crash("demo", Cause::Panicked) {
            Verdict::Retry(d) => d.as_secs(),
            Verdict::GiveUp => 0,
        })
        .collect();
    assert_eq!(waits, vec![1, 2, 4, 5, 5]);
}

#[tokio::test]
async fn a_loop_that_returns_by_itself_is_restarted() {
    let guard = Guard::new(quick(5));
    let starts = Arc::new(AtomicU32::new(0));
    let cancel = CancellationToken::new();
    let counter = Arc::clone(&starts);
    let token = cancel.clone();
    let handle = guard.supervise("demo", cancel.clone(), move || {
        let n = counter.fetch_add(1, Ordering::SeqCst);
        let token = token.clone();
        async move {
            if n >= 1 {
                token.cancelled().await;
            }
        }
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while starts.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(guard.statuses()[0].restarts, 1);
    cancel.cancel();
    handle.await.unwrap();
}

#[tokio::test]
async fn cancelling_stops_the_loop_without_a_restart() {
    let guard = Guard::new(quick(5));
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let handle = guard.supervise("demo", cancel.clone(), move || {
        let token = token.clone();
        async move { token.cancelled().await }
    });
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(guard.statuses()[0].restarts, 0);
}

#[tokio::test]
async fn a_bad_pass_is_caught_and_counted_without_a_restart() {
    let guard = Guard::new(quick(3));
    let seen = recorder(&guard);
    let ok = guard.guarded("demo", async { 7 }).await;
    assert_eq!(ok.unwrap(), 7);
    let bad: Result<(), Verdict> = guard.guarded("demo", async { panic!("bad pass") }).await;
    assert!(matches!(bad, Err(Verdict::Retry(_))));
    assert_eq!(seen.lock().len(), 1);
    let bad: Result<(), Verdict> = guard.guarded("demo", async { panic!("bad pass") }).await;
    assert!(matches!(bad, Err(Verdict::Retry(_))));
    let bad: Result<(), Verdict> = guard.guarded("demo", async { panic!("bad pass") }).await;
    assert_eq!(bad.unwrap_err(), Verdict::GiveUp);
    assert!(guard.is_degraded());
}

#[tokio::test]
async fn an_injected_fault_fires_the_requested_number_of_times() {
    let guard = Guard::new(quick(9));
    guard.inject_panic("demo", Fault::Pass, 2);
    for _ in 0..2 {
        assert!(guard.guarded("demo", async {}).await.is_err());
    }
    assert!(guard.guarded("demo", async {}).await.is_ok());
    // Another loop and another place are not hit.
    guard.inject_panic("demo", Fault::Task, 1);
    assert!(guard.guarded("demo", async {}).await.is_ok());
}

/// What the support log (`daemon.jsonl`) holds after `run` made loops crash: it is the only place
/// a crash is recorded for the owner's support bundle, and it keeps no free text.
fn support_log_of(run: impl FnOnce()) -> String {
    let dir = tempfile::tempdir().unwrap();
    let subscriber = support_logs::subscriber(dir.path(), "daemon", "info").unwrap();
    support_logs::tracing::subscriber::with_default(subscriber, run);
    std::fs::read_to_string(dir.path().join("logs/daemon.jsonl")).unwrap_or_default()
}

#[test]
fn a_crash_reaches_the_support_log_without_the_panic_text() {
    const CANARY: &str = "sk-canary-12345678901234567890 /home/canary/private";
    let log = support_log_of(|| {
        let guard = Guard::new(quick(2));
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            for _ in 0..2 {
                let outcome = guard.guarded("demo", async { panic!("{CANARY}") }).await;
                assert!(outcome.is_err());
            }
        });
    });
    for secret in ["sk-canary", "canary/private", "12345678901234567890"] {
        assert!(
            !log.contains(secret),
            "the panic text reached the log: {log}"
        );
    }
    let lines: Vec<serde_json::Value> = log
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2, "one record per crash: {log}");
    let fields = |i: usize| &lines[i]["fields"];
    assert_eq!(fields(0)["reason"], "loop_panicked");
    assert_eq!(fields(0)["loop_name"], "demo");
    assert_eq!(fields(0)["crashes"], "1");
    assert!(fields(0)["restart_in_ms"].is_string());
    assert_eq!(fields(1)["reason"], "loop_gave_up");
    assert_eq!(fields(1)["crashes"], "2");
    assert_eq!(lines[1]["level"], "error");
}

#[test]
fn a_loop_that_ended_by_itself_says_so_in_the_support_log() {
    let log = support_log_of(|| {
        let guard = Guard::new(quick(5));
        assert!(matches!(
            guard.record_crash("demo", Cause::Ended),
            Verdict::Retry(_)
        ));
    });
    assert!(log.contains("loop_ended"), "{log}");
}
