//! The background loops are watched: a panic in the scheduler, the mission wake loop or the
//! mission drivers restarts that loop, tells the owner once, and shows in health and the doctor.

use std::time::Duration;

use axum::http::{Method, StatusCode};
use liveness::{Fault, LoopState};
use serde_json::Value;

use super::{T, call, send, setup};

async fn until(what: &str, mut ok: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(20), async {
        while !ok() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for: {what}"));
}

fn restarts(t: &T, name: &str) -> u32 {
    t.state
        .loop_guard()
        .statuses()
        .iter()
        .find(|s| s.name == name)
        .map_or(0, |s| s.restarts)
}

async fn notices(t: &T) -> Vec<Value> {
    let r = call(t, Method::GET, "/v1/owner-notices", None).await;
    assert_eq!(r.status, StatusCode::OK);
    r.json().as_array().cloned().unwrap_or_default()
}

fn titles(list: &[Value]) -> Vec<String> {
    list.iter()
        .filter_map(|n| n["title"].as_str().map(str::to_owned))
        .collect()
}

/// `background` of the unauthenticated `GET /v1/health`.
async fn health_background(t: &T) -> Value {
    let r = send(&t.app, Method::GET, "/v1/health", None, &[]).await;
    assert_eq!(r.status, StatusCode::OK);
    let body = r.json();
    // Nothing about the loops themselves (names, crash counts) goes to a caller without a token.
    let text = body.to_string();
    for private in ["scheduler", "mission_wakes", "mission_drivers", "restarts"] {
        assert!(!text.contains(private), "health leaks `{private}`: {text}");
    }
    body["background"].clone()
}

async fn doctor_row(t: &T) -> Value {
    let r = call(t, Method::GET, "/v1/doctor", None).await;
    r.json()["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "background_loops")
        .cloned()
        .expect("a background_loops row")
}

#[tokio::test]
async fn health_and_doctor_list_the_loops_before_anything_crashed() {
    let t = setup();
    t.state.start_scheduler().unwrap();
    let health = health_background(&t).await;
    assert_eq!(health, serde_json::json!({"status": "ok"}));
    let row = doctor_row(&t).await;
    assert_eq!(row["status"], "ok");
    // The owner's doctor names the loops.
    assert!(
        row["detail"].as_str().unwrap().contains("scheduler"),
        "{row}"
    );
    t.state.stop_components().await;
}

#[tokio::test]
async fn a_scheduler_pass_that_panics_is_caught_and_the_owner_is_told() {
    let t = setup();
    t.state
        .loop_guard()
        .inject_panic(scheduler::LOOP_NAME, Fault::Pass, 1);
    t.state.start_scheduler().unwrap();
    until("the scheduler pass to crash", || {
        restarts(&t, scheduler::LOOP_NAME) == 1
    })
    .await;
    let list = notices(&t).await;
    assert_eq!(
        titles(&list),
        vec!["Schedules stopped and were restarted".to_owned()]
    );
    assert_eq!(list[0]["kind"], "failed");
    assert_eq!(health_background(&t).await["status"], "ok");
    let row = doctor_row(&t).await;
    assert_eq!(row["status"], "warn");
    assert!(
        row["detail"].as_str().unwrap().contains("scheduler (1x)"),
        "{row}"
    );
    // The loop goes on: the next tick is answered.
    t.state.scheduler().unwrap().tick().await;
    t.state.stop_components().await;
}

#[tokio::test]
async fn a_scheduler_loop_that_panics_is_restarted() {
    let t = setup();
    t.state
        .loop_guard()
        .inject_panic(scheduler::LOOP_NAME, Fault::Task, 1);
    t.state.start_scheduler().unwrap();
    until("the scheduler loop to be restarted", || {
        restarts(&t, scheduler::LOOP_NAME) == 1
    })
    .await;
    assert_eq!(
        titles(&notices(&t).await),
        vec!["Schedules stopped and were restarted".to_owned()]
    );
    // After the restart the loop answers a tick again.
    tokio::time::timeout(Duration::from_secs(20), t.state.scheduler().unwrap().tick())
        .await
        .expect("the restarted loop runs a pass");
    t.state.stop_components().await;
}

#[tokio::test]
async fn the_mission_wake_loop_is_restarted_after_a_panic() {
    for at in [Fault::Pass, Fault::Task] {
        let t = setup();
        t.state.enable_missions().unwrap();
        t.state
            .loop_guard()
            .inject_panic(missions::supervisor::WAKE_LOOP, at, 1);
        t.state.start_missions();
        until("the wake loop to crash", || {
            restarts(&t, missions::supervisor::WAKE_LOOP) == 1
        })
        .await;
        assert_eq!(
            titles(&notices(&t).await),
            vec!["Mission wake-ups stopped and were restarted".to_owned()],
            "{at:?}"
        );
        assert_eq!(restarts(&t, missions::supervisor::DRIVER_LOOP), 0);
        t.state.stop_components().await;
    }
}

#[tokio::test]
async fn the_mission_driver_loop_is_restarted_after_a_panic() {
    for at in [Fault::Pass, Fault::Task] {
        let t = setup();
        t.state.enable_missions().unwrap();
        t.state
            .loop_guard()
            .inject_panic(missions::supervisor::DRIVER_LOOP, at, 1);
        t.state.start_missions();
        until("the driver loop to crash", || {
            restarts(&t, missions::supervisor::DRIVER_LOOP) == 1
        })
        .await;
        assert_eq!(
            titles(&notices(&t).await),
            vec!["Missions stopped and were restarted".to_owned()],
            "{at:?}"
        );
        assert_eq!(restarts(&t, missions::supervisor::WAKE_LOOP), 0);
        t.state.stop_components().await;
    }
}

#[tokio::test]
async fn a_loop_that_keeps_crashing_is_degraded_everywhere() {
    let t = setup();
    t.state.start_scheduler().unwrap();
    let guard = t.state.loop_guard();
    for _ in 0..5 {
        guard.record_crash(scheduler::LOOP_NAME, liveness::Cause::Panicked);
    }
    assert!(guard.is_degraded());
    assert_eq!(guard.statuses()[0].state, LoopState::Degraded);
    assert_eq!(health_background(&t).await["status"], "degraded");
    let row = doctor_row(&t).await;
    assert_eq!(row["status"], "fail");
    assert!(
        row["detail"].as_str().unwrap().contains("scheduler"),
        "{row}"
    );
    // One notice for the first crash and one for giving up.
    assert_eq!(
        titles(&notices(&t).await),
        vec![
            "Schedules stopped and were restarted".to_owned(),
            "Schedules are not running".to_owned()
        ]
    );
    // The overall doctor verdict follows.
    let r = call(&t, Method::GET, "/v1/doctor", None).await;
    assert_eq!(r.json()["ok"], false);
    t.state.stop_components().await;
}
