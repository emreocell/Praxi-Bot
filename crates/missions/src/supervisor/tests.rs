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
    // The stopped session winds down and releases the mission.
    h.store.set_lease(&h.mission, None).unwrap();
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
    let lost = h.store.begin_session(&h.mission, 1_500).unwrap();
    h.store.set_session_run(lost, "run-lost").unwrap();
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
    assert_eq!(
        h.runner.cancelled.lock().as_slice(),
        ["run-lost"],
        "the abandoned run is cancelled"
    );
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

/// A runner whose session lasts until the supervisor stops it, like a model that is busy.
/// Without `with_run` it never gets a run (it is still waiting for the chat to be free).
struct WaitingRunner {
    with_run: bool,
    started: AtomicU32,
    announced: AtomicU32,
    cancelled: Mutex<Vec<String>>,
}

#[async_trait]
impl SessionRunner for WaitingRunner {
    fn bot_ready(&self, _bot_id: &str) -> std::result::Result<(), String> {
        Ok(())
    }

    async fn run_session(
        &self,
        req: SessionRequest,
        on_started: &(dyn for<'a> Fn(&'a str) + Send + Sync),
    ) -> SessionResult {
        self.started.fetch_add(1, Ordering::SeqCst);
        if self.with_run {
            on_started("run-w");
        }
        req.cancel.cancelled().await;
        SessionResult {
            end: SessionEnd::Cancelled,
            model_steps: 0,
            cost_usd: 0.0,
        }
    }

    fn cancel_run(&self, run_id: &str) {
        self.cancelled.lock().push(run_id.to_owned());
    }

    async fn announce(&self, _m: &Mission, _text: &str) {
        self.announced.fetch_add(1, Ordering::SeqCst);
    }
}

struct Waiting {
    store: Arc<MissionStore>,
    runner: Arc<WaitingRunner>,
    sup: Arc<Supervisor>,
    shutdown: CancellationToken,
    mission: String,
}

fn waiting(with_run: bool) -> Waiting {
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
                budgets: Budgets::default(),
                items: sites(3),
            },
            1_000,
        )
        .unwrap();
    let runner = Arc::new(WaitingRunner {
        with_run,
        started: AtomicU32::new(0),
        announced: AtomicU32::new(0),
        cancelled: Mutex::new(vec![]),
    });
    let shutdown = CancellationToken::new();
    let sup = Arc::new(Supervisor::new(
        Arc::clone(&store),
        Arc::clone(&runner) as Arc<dyn SessionRunner>,
        Arc::new(ManualClock(AtomicI64::new(2_000))),
        SupervisorConfig {
            session_gap: Duration::ZERO,
            poll: Duration::from_millis(10),
        },
        shutdown.clone(),
    ));
    Waiting {
        store,
        runner,
        sup,
        shutdown,
        mission: m.id,
    }
}

async fn until(what: &str, cond: impl Fn() -> bool) {
    for _ in 0..500 {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

fn session_outcomes(store: &MissionStore, mission: &str) -> Vec<String> {
    store
        .events(mission, 0, 100)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "session_ended")
        .map(|e| e.data["outcome"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn a_pause_reaches_a_session_that_is_still_waiting_for_the_chat() {
    // No run and no real lease yet: only the session's token can stop the runner.
    let w = waiting(false);
    let sup = Arc::clone(&w.sup);
    let id = w.mission.clone();
    let session = tokio::spawn(async move { sup.step(&id).await.unwrap() });
    let store = Arc::clone(&w.store);
    let mission = w.mission.clone();
    until("the session to take the mission", move || {
        store
            .get(&mission)
            .unwrap()
            .unwrap()
            .lease_run_id
            .is_some_and(|l| l.starts_with("session:"))
    })
    .await;
    w.sup.pause(&w.mission).unwrap();
    assert_eq!(
        session.await.unwrap(),
        Step::Stopped(MissionState::Paused),
        "the runner gave up waiting"
    );
    assert!(
        w.runner.cancelled.lock().is_empty(),
        "the placeholder lease is not a run id"
    );
    assert_eq!(session_outcomes(&w.store, &w.mission), ["cancelled"]);
    assert_eq!(w.store.get(&w.mission).unwrap().unwrap().lease_run_id, None);
}

#[tokio::test]
async fn one_mission_never_has_two_sessions_at_once() {
    let w = waiting(true);
    let sup = Arc::clone(&w.sup);
    let id = w.mission.clone();
    let first = tokio::spawn(async move { sup.step(&id).await.unwrap() });
    let store = Arc::clone(&w.store);
    let mission = w.mission.clone();
    until("the first session", move || {
        store
            .get(&mission)
            .unwrap()
            .unwrap()
            .lease_run_id
            .as_deref()
            == Some("run-w")
    })
    .await;
    // The same supervisor, and a second one on the same ledger (a leader that came back).
    assert_eq!(
        w.sup.step(&w.mission).await.unwrap(),
        Step::Stopped(MissionState::Running)
    );
    let other = Arc::new(Supervisor::new(
        Arc::clone(&w.store),
        Arc::clone(&w.runner) as Arc<dyn SessionRunner>,
        Arc::new(ManualClock(AtomicI64::new(2_000))),
        SupervisorConfig::default(),
        CancellationToken::new(),
    ));
    assert_eq!(
        other.step(&w.mission).await.unwrap(),
        Step::Stopped(MissionState::Running)
    );
    assert_eq!(w.runner.started.load(Ordering::SeqCst), 1);
    assert_eq!(w.store.get(&w.mission).unwrap().unwrap().sessions, 1);
    w.sup.pause(&w.mission).unwrap();
    assert_eq!(w.runner.cancelled.lock().as_slice(), ["run-w"]);
    assert_eq!(first.await.unwrap(), Step::Stopped(MissionState::Paused));
}

#[tokio::test]
async fn a_shutdown_pause_interrupts_the_session_instead_of_blocking_the_mission() {
    // The agent loop's own shutdown pauses the run: the session ends `Paused` while the
    // supervisor is shutting down. That is an interruption, not an unanswered approval.
    let slot: Arc<std::sync::OnceLock<CancellationToken>> = Arc::default();
    let script_slot = Arc::clone(&slot);
    let h = harness(
        sites(3),
        Budgets::default(),
        Box::new(move |_, _, _| {
            script_slot.get().unwrap().cancel();
            SessionResult {
                end: SessionEnd::Paused,
                model_steps: 3,
                cost_usd: 0.0,
            }
        }),
    );
    slot.set(h.shutdown.clone()).unwrap();
    assert_eq!(
        h.sup.step(&h.mission).await.unwrap(),
        Step::Stopped(MissionState::Running)
    );
    let m = h.store.get(&h.mission).unwrap().unwrap();
    assert_eq!(m.state, MissionState::Running);
    assert_eq!(m.digest, None);
    assert_eq!(session_outcomes(&h.store, &h.mission), ["interrupted"]);
    assert!(h.runner.announced.lock().is_empty());
    assert!(h.runner.cancelled.lock().is_empty());
}

#[tokio::test]
async fn an_unanswered_approval_blocks_the_mission_and_abandons_the_paused_run() {
    let h = harness(
        sites(3),
        Budgets::default(),
        Box::new(|_, _, _| SessionResult {
            end: SessionEnd::Paused,
            model_steps: 4,
            cost_usd: 0.0,
        }),
    );
    assert_eq!(
        h.sup.step(&h.mission).await.unwrap(),
        Step::Stopped(MissionState::Blocked)
    );
    assert_eq!(
        h.runner.cancelled.lock().as_slice(),
        ["run-1"],
        "nobody can resume the old run next to its replacement"
    );
    let m = h.store.get(&h.mission).unwrap().unwrap();
    assert!(m.state_reason.unwrap().contains("approval"));
    assert_eq!(session_outcomes(&h.store, &h.mission), ["paused"]);
    assert_eq!(h.runner.announced.lock().len(), 1, "one digest");
}

#[tokio::test]
async fn shutdown_stops_and_joins_the_drivers() {
    let w = waiting(true);
    let handle = w.sup.spawn();
    let store = Arc::clone(&w.store);
    let mission = w.mission.clone();
    until("the session to start", move || {
        store
            .get(&mission)
            .unwrap()
            .unwrap()
            .lease_run_id
            .as_deref()
            == Some("run-w")
    })
    .await;
    w.shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("the loop joins its drivers")
        .unwrap();
    assert!(w.sup.driving.lock().is_empty(), "no driver is left alive");
    assert!(w.sup.active.lock().is_empty());
    let m = w.store.get(&w.mission).unwrap().unwrap();
    assert_eq!(
        m.state,
        MissionState::Running,
        "continues after the restart"
    );
    assert_eq!(m.lease_run_id, None);
    assert_eq!(session_outcomes(&w.store, &w.mission), ["interrupted"]);
}

#[tokio::test]
async fn stopping_a_mission_during_a_session_posts_one_digest() {
    let w = waiting(true);
    let sup = Arc::clone(&w.sup);
    let id = w.mission.clone();
    let driver = tokio::spawn(async move { sup.step(&id).await.unwrap() });
    let store = Arc::clone(&w.store);
    let mission = w.mission.clone();
    until("the session to start", move || {
        store
            .get(&mission)
            .unwrap()
            .unwrap()
            .lease_run_id
            .as_deref()
            == Some("run-w")
    })
    .await;
    let m = w.sup.cancel(&w.mission).await.unwrap();
    assert_eq!(m.state, MissionState::Cancelled);
    // The session winds down and sees a stopped mission: it must not announce again.
    assert_eq!(
        driver.await.unwrap(),
        Step::Stopped(MissionState::Cancelled)
    );
    assert_eq!(w.runner.announced.load(Ordering::SeqCst), 1);
    assert_eq!(
        w.store
            .get(&w.mission)
            .unwrap()
            .unwrap()
            .digest
            .unwrap()
            .outcome,
        "cancelled"
    );
}

#[tokio::test]
async fn a_mission_that_runs_again_gets_a_fresh_digest_for_its_next_ending() {
    let h = harness(sites(2), Budgets::default(), worker(5));
    h.sup.block(&h.mission, "needs a look").await.unwrap();
    assert_eq!(h.runner.announced.lock().len(), 1);
    // Concluding the same ending again does not post it again.
    h.sup.conclude(&h.mission).await.unwrap();
    assert_eq!(h.runner.announced.lock().len(), 1);
    // Stopping a blocked mission is a different ending: a new digest.
    h.sup.cancel(&h.mission).await.unwrap();
    assert_eq!(h.runner.announced.lock().len(), 2);
    assert_eq!(
        h.store
            .get(&h.mission)
            .unwrap()
            .unwrap()
            .digest
            .unwrap()
            .outcome,
        "cancelled"
    );

    let h = harness(sites(2), Budgets::default(), worker(5));
    h.sup.block(&h.mission, "needs a look").await.unwrap();
    h.sup.resume(&h.mission).unwrap();
    assert_eq!(
        h.store.get(&h.mission).unwrap().unwrap().digest,
        None,
        "a running mission shows no old digest"
    );
    h.sup.block(&h.mission, "needs a look again").await.unwrap();
    assert_eq!(h.runner.announced.lock().len(), 2);
}

#[tokio::test]
async fn flipping_an_item_between_todo_and_doing_is_not_progress() {
    // A model that keeps "starting" and "putting back" an item would reset the stalled-session
    // count forever and burn the session budget. The attempt cap ends it, and the stall
    // detector blocks the mission.
    let flip: Box<Script> = Box::new(|_, req, store| {
        let it = store
            .items(&req.mission_id, None)
            .unwrap()
            .into_iter()
            .find(|i| i.status.is_open())
            .unwrap();
        for status in [ItemStatus::Doing, ItemStatus::Todo] {
            let _ = store.update_item(
                &req.mission_id,
                &it.key,
                &ItemUpdate {
                    status: Some(status),
                    ..ItemUpdate::default()
                },
                false,
                7,
            );
        }
        ok(5)
    });
    let h = harness(sites(3), Budgets::default(), flip);
    assert_eq!(
        h.sup.drive(&h.mission).await.unwrap(),
        MissionState::Blocked
    );
    assert_eq!(
        h.runner.calls.load(Ordering::SeqCst),
        4,
        "max_stalled_sessions"
    );
    let m = h.store.get(&h.mission).unwrap().unwrap();
    assert!(m.state_reason.unwrap().contains("no progress"));
    assert_eq!(
        h.store.items(&h.mission, None).unwrap()[0].attempts,
        MAX_ITEM_ATTEMPTS
    );
}

#[tokio::test]
async fn restart_requeues_an_item_the_crash_left_doing() {
    let h = harness(sites(2), Budgets::default(), worker(5));
    let lost = h.store.begin_session(&h.mission, 1_500).unwrap();
    h.store.set_session_run(lost, "run-lost").unwrap();
    set(
        &h.store,
        &h.mission,
        "site-0.example",
        ItemStatus::Doing,
        None,
    );
    let handle = h.sup.spawn();
    until("the mission to finish", {
        let store = Arc::clone(&h.store);
        let id = h.mission.clone();
        move || store.get(&id).unwrap().unwrap().state == MissionState::Done
    })
    .await;
    h.shutdown.cancel();
    handle.await.unwrap();
    let p = h.store.progress(&h.mission).unwrap();
    assert_eq!(
        p.done, 2,
        "the stranded item was worked again, not left doing"
    );
    assert_eq!(p.doing, 0);
}
