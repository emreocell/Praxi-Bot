//! A panic in the scheduler loop does not stop schedules for good.

use std::time::Duration;

use liveness::{EventKind, Fault, Guard, LoopState, Policy};

use super::*;
use crate::LOOP_NAME;

fn quick(max_crashes: u32) -> Guard {
    Guard::new(Policy {
        max_crashes,
        backoff_base: Duration::from_millis(5),
        backoff_max: Duration::from_millis(20),
        stable_after: Duration::from_secs(60),
    })
}

fn every_second() -> Schedule {
    Schedule::new(
        BotId::new(),
        None,
        "tick",
        ScheduleSpec::Every { seconds: 1 },
        Payload::AgentPrompt { text: "go".into() },
        MisfirePolicy::RunAll,
        0,
    )
    .unwrap()
}

fn watch(guard: &Guard) -> Arc<Mutex<Vec<EventKind>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    guard.set_notifier(Arc::new(move |e| sink.lock().push(e.kind)));
    seen
}

async fn fires_after_fault(at: Fault) {
    let store = Arc::new(ScheduleStore::open_in_memory().unwrap());
    store.insert(&every_second()).unwrap();
    let exec = RecordingExecutor::new();
    let clock = Arc::new(FakeClock::new(0));
    let guard = quick(5);
    let seen = watch(&guard);
    guard.inject_panic(LOOP_NAME, at, 1);
    let scheduler =
        Scheduler::start_guarded(Arc::clone(&store), exec.clone(), clock.clone(), &guard);
    tokio::time::timeout(Duration::from_secs(10), async {
        while guard.statuses().iter().all(|s| s.restarts == 0) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the panic was noticed");
    assert_eq!(*seen.lock(), vec![EventKind::Restarted], "{at:?}");
    // The schedule still fires after the crash.
    clock.advance_ms(1_000);
    tokio::time::timeout(Duration::from_secs(10), scheduler.settle())
        .await
        .expect("the loop runs a pass again");
    assert_eq!(
        exec.count(),
        1,
        "{at:?}: the schedule fired after the restart"
    );
    assert_eq!(guard.statuses()[0].state, LoopState::Running);
    scheduler.shutdown().await;
}

#[tokio::test]
async fn a_panic_in_a_pass_costs_that_pass_only() {
    fires_after_fault(Fault::Pass).await;
}

#[tokio::test]
async fn a_panic_that_kills_the_loop_restarts_it() {
    fires_after_fault(Fault::Task).await;
}

#[tokio::test]
async fn a_loop_that_keeps_panicking_is_given_up_and_nobody_waits_for_it() {
    let store = Arc::new(ScheduleStore::open_in_memory().unwrap());
    let exec = RecordingExecutor::new();
    let clock = Arc::new(FakeClock::new(0));
    let guard = quick(3);
    let seen = watch(&guard);
    guard.inject_panic(LOOP_NAME, Fault::Pass, 10);
    let scheduler = Scheduler::start_guarded(store, exec, clock, &guard);
    tokio::time::timeout(Duration::from_secs(10), async {
        while !guard.is_degraded() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the guard gives up");
    assert_eq!(*seen.lock(), vec![EventKind::Restarted, EventKind::GaveUp]);
    tokio::time::timeout(Duration::from_secs(10), scheduler.tick())
        .await
        .expect("a tick does not hang on a loop that is gone");
    scheduler.shutdown().await;
}

#[tokio::test]
async fn a_loop_that_dies_at_every_start_is_given_up_and_a_waiting_tick_returns() {
    let store = Arc::new(ScheduleStore::open_in_memory().unwrap());
    let exec = RecordingExecutor::new();
    let clock = Arc::new(FakeClock::new(0));
    // Slow restarts, so a tick is already waiting when the guard gives up.
    let guard = Guard::new(Policy {
        max_crashes: 3,
        backoff_base: Duration::from_millis(100),
        backoff_max: Duration::from_millis(100),
        stable_after: Duration::from_secs(60),
    });
    // The task itself panics before it reaches a pass: the loop never gets to say it is gone.
    guard.inject_panic(LOOP_NAME, Fault::Task, 10);
    let scheduler = Scheduler::start_guarded(store, exec, clock, &guard);
    let waiting = {
        let scheduler = scheduler.clone();
        tokio::spawn(async move { scheduler.tick().await })
    };
    tokio::time::timeout(Duration::from_secs(10), waiting)
        .await
        .expect("a tick that was waiting returns when the scheduler is given up")
        .unwrap();
    assert!(guard.is_degraded());
    tokio::time::timeout(Duration::from_secs(10), scheduler.tick())
        .await
        .expect("and so does a tick after it");
    scheduler.shutdown().await;
}
