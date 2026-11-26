//! Deterministic supervisor scenarios: a scripted "model" that edits the
//! ledger through the same store the tools use, a manual clock, no sleeping.

use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};

use super::*;
use crate::model::Budgets;
use crate::store::{ItemUpdate, NewItem, NewMission};

struct ManualClock(AtomicI64);

impl Clock for ManualClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

type Script = dyn Fn(u32, &SessionRequest, &MissionStore) -> SessionResult + Send + Sync;

/// A runner whose "model" is a closure over (session number, request, store).
struct FakeRunner {
    store: Arc<MissionStore>,
    script: Box<Script>,
    calls: AtomicU32,
    briefs: Mutex<Vec<String>>,
    announced: Mutex<Vec<String>>,
    cancelled: Mutex<Vec<String>>,
    ready: Mutex<std::result::Result<(), String>>,
}

#[async_trait]
impl SessionRunner for FakeRunner {
    fn bot_ready(&self, _bot_id: &str) -> std::result::Result<(), String> {
        self.ready.lock().clone()
    }

    async fn run_session(
        &self,
        req: SessionRequest,
        on_started: &(dyn for<'a> Fn(&'a str) + Send + Sync),
    ) -> SessionResult {
        let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        on_started(&format!("run-{n}"));
        self.briefs.lock().push(req.brief.clone());
        (self.script)(n, &req, &self.store)
    }

    fn cancel_run(&self, run_id: &str) {
        self.cancelled.lock().push(run_id.to_owned());
    }

    async fn announce(&self, _m: &Mission, text: &str) {
        self.announced.lock().push(text.to_owned());
    }
}

struct Harness {
    store: Arc<MissionStore>,
    runner: Arc<FakeRunner>,
    sup: Arc<Supervisor>,
    clock: Arc<ManualClock>,
    shutdown: CancellationToken,
    mission: String,
}

fn ok(steps: u32) -> SessionResult {
    SessionResult {
        end: SessionEnd::Finished,
        model_steps: steps,
        cost_usd: 0.01,
    }
}

fn sites(n: usize) -> Vec<NewItem> {
    (0..n)
        .map(|i| NewItem {
            key: format!("site-{i}.example"),
            kind: "site".into(),
            title: format!("Sign up on site-{i}.example"),
            payload: None,
        })
        .collect()
}

fn set(store: &MissionStore, id: &str, key: &str, status: ItemStatus, reason: Option<&str>) {
    store
        .update_item(
            id,
            key,
            &ItemUpdate {
                status: Some(status),
                result: (status == ItemStatus::Done).then(|| "account created".to_owned()),
                blocked_reason: reason.map(str::to_owned),
                ..ItemUpdate::default()
            },
            false,
            1,
        )
        .unwrap();
}

fn harness(items: Vec<NewItem>, budgets: Budgets, script: Box<Script>) -> Harness {
    let store = Arc::new(MissionStore::open_in_memory().unwrap());
    let m = store
        .create(
            &NewMission {
                user_id: "u".into(),
                bot_id: "bot".into(),
                chat_id: "chat".into(),
                title: "Backlinks".into(),
                goal_text: "Get backlinks for my site".into(),
                target_done: None,
                budgets,
                items,
            },
            1_000,
        )
        .unwrap();
    let runner = Arc::new(FakeRunner {
        store: Arc::clone(&store),
        script,
        calls: AtomicU32::new(0),
        briefs: Mutex::new(vec![]),
        announced: Mutex::new(vec![]),
        cancelled: Mutex::new(vec![]),
        ready: Mutex::new(Ok(())),
    });
    let clock = Arc::new(ManualClock(AtomicI64::new(2_000)));
    let shutdown = CancellationToken::new();
    let sup = Arc::new(Supervisor::new(
        Arc::clone(&store),
        Arc::clone(&runner) as Arc<dyn SessionRunner>,
        Arc::clone(&clock) as Arc<dyn Clock>,
        SupervisorConfig {
            session_gap: Duration::ZERO,
            poll: Duration::from_millis(10),
        },
        shutdown.clone(),
    ));
    Harness {
        store,
        runner,
        sup,
        clock,
        shutdown,
        mission: m.id,
    }
}

/// Each session finishes at most `per_session` open items (a stand-in for the
/// 40-step wall), like a model that runs out of steps mid-list.
fn worker(per_session: usize) -> Box<Script> {
    Box::new(move |_, req, store| {
        let open = store.items(&req.mission_id, None).unwrap();
        for it in open.iter().filter(|i| i.status.is_open()).take(per_session) {
            set(store, &req.mission_id, &it.key, ItemStatus::Done, None);
        }
        SessionResult {
            end: SessionEnd::GuardStop,
            model_steps: 40,
            cost_usd: 0.02,
        }
    })
}

#[tokio::test]
async fn chains_sessions_past_the_step_wall_until_the_goal_is_met() {
    let h = harness(sites(10), Budgets::default(), worker(4));
    let end = h.sup.drive(&h.mission).await.unwrap();
    assert_eq!(end, MissionState::Done);
    assert_eq!(
        h.runner.calls.load(Ordering::SeqCst),
        3,
        "10 items at 4 per session"
    );
    let m = h.store.get(&h.mission).unwrap().unwrap();
    assert_eq!((m.sessions, m.model_steps), (3, 120));
    assert_eq!(h.store.progress(&h.mission).unwrap().done, 10);
    let d = m.digest.expect("digest written");
    assert_eq!(d.outcome, "done");
    assert!(h.runner.announced.lock()[0].contains("Mission finished"));
}

#[tokio::test]
async fn each_session_gets_a_fresh_brief_with_the_current_ledger() {
    let h = harness(sites(6), Budgets::default(), worker(3));
    h.sup.drive(&h.mission).await.unwrap();
    let briefs = h.runner.briefs.lock();
    assert!(briefs[0].contains("session 1") && briefs[0].contains("0 done"));
    assert!(briefs[1].contains("session 2") && briefs[1].contains("3 done"));
    assert!(briefs[1].contains("Get backlinks for my site"));
}

#[tokio::test]
async fn a_blocked_item_is_parked_the_rest_continues_and_the_owner_gets_one_digest() {
    let script: Box<Script> = Box::new(|_, req, store| {
        for it in store
            .items(&req.mission_id, None)
            .unwrap()
            .iter()
            .filter(|i| i.status.is_open())
        {
            if it.key == "site-2.example" && it.owner_answer.is_none() {
                set(
                    store,
                    &req.mission_id,
                    &it.key,
                    ItemStatus::Blocked,
                    Some("CAPTCHA on the sign-up form"),
                );
            } else {
                set(store, &req.mission_id, &it.key, ItemStatus::Done, None);
            }
        }
        ok(12)
    });
    let h = harness(sites(5), Budgets::default(), script);
    let end = h.sup.drive(&h.mission).await.unwrap();
    assert_eq!(end, MissionState::Blocked);
    assert_eq!(
        h.runner.calls.load(Ordering::SeqCst),
        1,
        "no session was spent waiting for the owner"
    );
    let p = h.store.progress(&h.mission).unwrap();
    assert_eq!((p.done, p.blocked), (4, 1));
    let m = h.store.get(&h.mission).unwrap().unwrap();
    assert!(m.state_reason.unwrap().contains("1 item(s) need you"));
    let d = m.digest.unwrap();
    assert_eq!(d.blocked.len(), 1);
    assert_eq!(
        d.blocked[0].text.as_deref(),
        Some("CAPTCHA on the sign-up form")
    );
    assert_eq!(h.runner.announced.lock().len(), 1, "one digest");

    // The owner answers once; the item goes back, the mission runs and finishes.
    let released = h
        .sup
        .answer_blocked(
            &h.mission,
            &[("site-2.example".into(), "I solved it".into())],
        )
        .unwrap();
    assert_eq!(released, 1);
    assert_eq!(h.sup.drive(&h.mission).await.unwrap(), MissionState::Done);
    assert!(h.runner.briefs.lock()[1].contains("ANSWERED BY THE OWNER"));
}

#[tokio::test]
async fn blocked_items_do_not_stop_the_mission_while_other_items_remain() {
    // Session 1 blocks one item and does one more; session 2 must still run.
    let script: Box<Script> = Box::new(|n, req, store| {
        let open: Vec<_> = store
            .items(&req.mission_id, None)
            .unwrap()
            .into_iter()
            .filter(|i| i.status.is_open())
            .collect();
        if n == 1 {
            set(
                store,
                &req.mission_id,
                &open[0].key,
                ItemStatus::Blocked,
                Some("2FA code needed"),
            );
            set(store, &req.mission_id, &open[1].key, ItemStatus::Done, None);
        } else {
            for it in &open {
                set(store, &req.mission_id, &it.key, ItemStatus::Done, None);
            }
        }
        ok(10)
    });
    let h = harness(sites(4), Budgets::default(), script);
    h.sup.step(&h.mission).await.unwrap();
    assert_eq!(
        h.store.get(&h.mission).unwrap().unwrap().state,
        MissionState::Running
    );
    assert_eq!(
        h.sup.step(&h.mission).await.unwrap(),
        Step::Stopped(MissionState::Blocked)
    );
    let p = h.store.progress(&h.mission).unwrap();
    assert_eq!((p.done, p.blocked), (3, 1));
}

#[tokio::test]
async fn stops_at_the_session_budget_with_the_exact_numbers() {
    let budgets = Budgets {
        max_sessions: 2,
        ..Budgets::default()
    };
    let h = harness(sites(50), budgets, worker(1));
    let end = h.sup.drive(&h.mission).await.unwrap();
    assert_eq!(end, MissionState::Blocked);
    assert_eq!(h.runner.calls.load(Ordering::SeqCst), 2);
    let m = h.store.get(&h.mission).unwrap().unwrap();
    assert_eq!(
        m.state_reason.as_deref(),
        Some("session budget used up: 2 of 2 sessions")
    );
    assert_eq!(m.digest.unwrap().progress.done, 2);
}

#[tokio::test]
async fn stops_at_the_model_call_budget_and_the_spend_cap() {
    let budgets = Budgets {
        max_model_steps: 80,
        ..Budgets::default()
    };
    let h = harness(sites(50), budgets, worker(1));
    h.sup.drive(&h.mission).await.unwrap();
    assert!(
        h.store
            .get(&h.mission)
            .unwrap()
            .unwrap()
            .state_reason
            .unwrap()
            .starts_with("model-call budget used up: 80 of 80")
    );

    let budgets = Budgets {
        max_cost_usd: Some(0.05),
        ..Budgets::default()
    };
    let h = harness(sites(50), budgets, worker(1));
    h.sup.drive(&h.mission).await.unwrap();
    let reason = h
        .store
        .get(&h.mission)
        .unwrap()
        .unwrap()
        .state_reason
        .unwrap();
    assert!(
        reason.starts_with("spend cap reached: $0.06 of $0.05"),
        "{reason}"
    );
}

#[tokio::test]
async fn stops_at_the_deadline_by_the_clock_not_the_model() {
    let budgets = Budgets {
        max_minutes: 60,
        ..Budgets::default()
    };
    let h = harness(sites(50), budgets, worker(1));
    assert_eq!(h.sup.step(&h.mission).await.unwrap(), Step::Continue);
    h.clock.0.store(1_000 + 61 * 60_000, Ordering::SeqCst);
    assert_eq!(
        h.sup.step(&h.mission).await.unwrap(),
        Step::Stopped(MissionState::Blocked)
    );
    assert_eq!(h.runner.calls.load(Ordering::SeqCst), 1);
    assert!(
        h.store
            .get(&h.mission)
            .unwrap()
            .unwrap()
            .state_reason
            .unwrap()
            .contains("deadline reached")
    );
}

#[tokio::test]
async fn sessions_without_ledger_progress_block_the_mission() {
    let idle: Box<Script> = Box::new(|_, _, _| ok(5));
    let h = harness(sites(3), Budgets::default(), idle);
    let end = h.sup.drive(&h.mission).await.unwrap();
    assert_eq!(end, MissionState::Blocked);
    assert_eq!(
        h.runner.calls.load(Ordering::SeqCst),
        4,
        "max_stalled_sessions is 4"
    );
    assert!(
        h.store
            .get(&h.mission)
            .unwrap()
            .unwrap()
            .state_reason
            .unwrap()
            .contains("no progress")
    );
}

#[tokio::test]
async fn repeated_session_failures_block_with_the_last_error() {
    let fail: Box<Script> = Box::new(|_, _, _| SessionResult {
        end: SessionEnd::Failed("provider unreachable".into()),
        model_steps: 0,
        cost_usd: 0.0,
    });
    let h = harness(sites(3), Budgets::default(), fail);
    assert_eq!(
        h.sup.drive(&h.mission).await.unwrap(),
        MissionState::Blocked
    );
    assert_eq!(h.runner.calls.load(Ordering::SeqCst), 3);
    assert!(
        h.store
            .get(&h.mission)
            .unwrap()
            .unwrap()
            .state_reason
            .unwrap()
            .contains("provider unreachable")
    );
}

#[tokio::test]
async fn a_failed_session_in_between_does_not_end_the_mission() {
    let script: Box<Script> = Box::new(|n, req, store| {
        if n == 1 {
            return SessionResult {
                end: SessionEnd::Failed("blip".into()),
                model_steps: 0,
                cost_usd: 0.0,
            };
        }
        for it in store
            .items(&req.mission_id, None)
            .unwrap()
            .iter()
            .filter(|i| i.status.is_open())
        {
            set(store, &req.mission_id, &it.key, ItemStatus::Done, None);
        }
        ok(3)
    });
    let h = harness(sites(2), Budgets::default(), script);
    assert_eq!(h.sup.drive(&h.mission).await.unwrap(), MissionState::Done);
}

#[tokio::test]
async fn an_empty_ledger_gets_planned_in_the_first_session() {
    let script: Box<Script> = Box::new(|n, req, store| {
        if n == 1 {
            store.upsert_items(&req.mission_id, &sites(3), 5).unwrap();
        } else {
            for it in store
                .items(&req.mission_id, None)
                .unwrap()
                .iter()
                .filter(|i| i.status.is_open())
            {
                set(store, &req.mission_id, &it.key, ItemStatus::Done, None);
            }
        }
        ok(5)
    });
    let h = harness(vec![], Budgets::default(), script);
    assert_eq!(h.sup.drive(&h.mission).await.unwrap(), MissionState::Done);
    assert!(h.runner.briefs.lock()[0].contains("THE LEDGER IS EMPTY"));
}

#[tokio::test]
async fn items_left_doing_are_requeued_then_given_up() {
    // The model marks the item doing and never finishes it (runs out of steps).
    let script: Box<Script> = Box::new(|_, req, store| {
        let it = store
            .items(&req.mission_id, None)
            .unwrap()
            .into_iter()
            .find(|i| i.status.is_open())
            .unwrap();
        set(store, &req.mission_id, &it.key, ItemStatus::Doing, None);
        SessionResult {
            end: SessionEnd::GuardStop,
            model_steps: 40,
            cost_usd: 0.0,
        }
    });
    let h = harness(sites(1), Budgets::default(), script);
    assert_eq!(h.sup.drive(&h.mission).await.unwrap(), MissionState::Done);
    let it = &h.store.items(&h.mission, None).unwrap()[0];
    assert_eq!(it.status, ItemStatus::Failed);
    assert_eq!(it.attempts, MAX_ITEM_ATTEMPTS);
    assert_eq!(h.runner.calls.load(Ordering::SeqCst), MAX_ITEM_ATTEMPTS);
}

#[tokio::test]
async fn the_bot_must_be_ready_for_unattended_work() {
    let h = harness(sites(2), Budgets::default(), worker(5));
    *h.runner.ready.lock() = Err("the bot is not on the Full level".into());
    assert_eq!(
        h.sup.drive(&h.mission).await.unwrap(),
        MissionState::Blocked
    );
    assert_eq!(h.runner.calls.load(Ordering::SeqCst), 0);
    assert!(
        h.store
            .get(&h.mission)
            .unwrap()
            .unwrap()
            .state_reason
            .unwrap()
            .contains("Full level")
    );
}

#[tokio::test]
async fn a_tool_completed_mission_is_concluded_without_another_session() {
    let script: Box<Script> = Box::new(|_, req, store| {
        for it in &store.items(&req.mission_id, None).unwrap() {
            set(store, &req.mission_id, &it.key, ItemStatus::Done, None);
        }
        store
            .set_state(&req.mission_id, MissionState::Done, Some("all done"), 9)
            .unwrap();
        ok(4)
    });
    let h = harness(sites(2), Budgets::default(), script);
    assert_eq!(h.sup.drive(&h.mission).await.unwrap(), MissionState::Done);
    assert_eq!(h.runner.calls.load(Ordering::SeqCst), 1);
    assert_eq!(h.runner.announced.lock().len(), 1);
    assert!(h.store.get(&h.mission).unwrap().unwrap().digest.is_some());
}

#[tokio::test]
async fn pause_stops_the_live_session_and_starts_no_new_one() {
    let h = harness(sites(5), Budgets::default(), worker(1));
    h.store.set_lease(&h.mission, Some("run-live")).unwrap();
    h.sup.pause(&h.mission).unwrap();
    assert_eq!(h.runner.cancelled.lock().as_slice(), ["run-live"]);
    assert_eq!(
        h.sup.step(&h.mission).await.unwrap(),
        Step::Stopped(MissionState::Paused)
    );
    assert_eq!(h.runner.calls.load(Ordering::SeqCst), 0);
    h.sup.resume(&h.mission).unwrap();
    assert_eq!(h.sup.step(&h.mission).await.unwrap(), Step::Continue);
}

#[tokio::test]
async fn a_run_stopped_by_the_owner_pauses_the_mission() {
    let stopped: Box<Script> = Box::new(|_, _, _| SessionResult {
        end: SessionEnd::Cancelled,
        model_steps: 2,
        cost_usd: 0.0,
    });
    let h = harness(sites(3), Budgets::default(), stopped);
    assert_eq!(h.sup.drive(&h.mission).await.unwrap(), MissionState::Paused);
}

#[tokio::test]
async fn cancel_ends_the_mission_with_a_digest() {
    let h = harness(sites(3), Budgets::default(), worker(1));
    let m = h.sup.cancel(&h.mission).await.unwrap();
    assert_eq!(m.state, MissionState::Cancelled);
    assert_eq!(m.digest.unwrap().outcome, "cancelled");
    assert!(h.runner.announced.lock()[0].contains("Mission stopped"));
}

#[tokio::test]
async fn shutdown_leaves_the_mission_running_for_the_next_start() {
    let h = harness(
        sites(3),
        Budgets::default(),
        Box::new(|_, _, _| SessionResult {
            end: SessionEnd::Cancelled,
            model_steps: 1,
            cost_usd: 0.0,
        }),
    );
    h.shutdown.cancel();
    assert_eq!(
        h.sup.step(&h.mission).await.unwrap(),
        Step::Stopped(MissionState::Running)
    );
    let m = h.store.get(&h.mission).unwrap().unwrap();
    assert_eq!(m.state, MissionState::Running);
}

#[tokio::test]
async fn restart_abandons_the_open_session_and_continues_from_the_ledger() {
    let h = harness(sites(4), Budgets::default(), worker(4));
    // A session was in flight when the daemon died.
    h.store.begin_session(&h.mission, 1_500).unwrap();
    h.store.set_lease(&h.mission, Some("run-lost")).unwrap();
    let handle = h.sup.spawn();
    for _ in 0..200 {
        if h.store.get(&h.mission).unwrap().unwrap().state == MissionState::Done {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    h.shutdown.cancel();
    handle.await.unwrap();
    let m = h.store.get(&h.mission).unwrap().unwrap();
    assert_eq!(m.state, MissionState::Done);
    let outcomes: Vec<_> = h
        .store
        .events(&h.mission, 0, 100)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "session_ended")
        .map(|e| e.data["outcome"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(outcomes.first().map(String::as_str), Some("interrupted"));
}

#[tokio::test]
async fn the_loop_drives_several_missions_independently() {
    let h = harness(sites(3), Budgets::default(), worker(3));
    let second = h
        .store
        .create(
            &NewMission {
                user_id: "u".into(),
                bot_id: "bot".into(),
                chat_id: "chat2".into(),
                title: "Second".into(),
                goal_text: "g2".into(),
                target_done: None,
                budgets: Budgets::default(),
                items: sites(3),
            },
            1_000,
        )
        .unwrap();
    let handle = h.sup.spawn();
    for _ in 0..300 {
        let a = h.store.get(&h.mission).unwrap().unwrap().state;
        let b = h.store.get(&second.id).unwrap().unwrap().state;
        if a == MissionState::Done && b == MissionState::Done {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    h.shutdown.cancel();
    handle.await.unwrap();
    assert_eq!(
        h.store.get(&h.mission).unwrap().unwrap().state,
        MissionState::Done
    );
    assert_eq!(
        h.store.get(&second.id).unwrap().unwrap().state,
        MissionState::Done
    );
}
