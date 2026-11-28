//! `/v1/owner-notices`: the owner hears of a mission or background run on their own, sets what
//! is pushed, answers blocked questions from a chat, and never decides an approval through it.

use std::time::Duration;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::missions::seed_mission;
use super::{T, auth, call, call_token_only, read_until, setup};
use protocol::{BotId, ChatId, Event, RunId};

const FULL_PROFILE: &str = r#"
[bot]
id = "tester"
name = "Tester"

[models]
executor = "fake/echo"

[permissions]
preset = "full"
"#;

async fn full_bot(t: &T) -> String {
    let r = call(
        t,
        Method::POST,
        "/v1/bots",
        Some(json!({"profile_toml": FULL_PROFILE, "name": "Scout"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.json());
    r.json()["id"].as_str().unwrap().to_owned()
}

async fn open_stream(t: &T, query: &str) -> Body {
    let (k, v) = auth();
    let resp = t
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/v1/owner-notices/stream{query}"))
                .header("host", "localhost")
                .header("x-praxi-owner-key", super::OWNER_KEY)
                .header(k, v.as_str())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        resp.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/event-stream"))
    );
    resp.into_body()
}

fn notices(frames: &[(String, String, Value)]) -> Vec<&Value> {
    frames
        .iter()
        .filter(|(_, ev, _)| ev == "notice")
        .map(|(_, _, d)| d)
        .collect()
}

/// A mission with one answerable question, one that needs the owner live, and a failed item,
/// left `blocked` as the supervisor leaves it.
fn blocked_mission(t: &T, bot: &str) -> String {
    t.state.enable_missions().unwrap();
    t.state.start_missions();
    let (id, _, _) = seed_mission(t, bot);
    let store = t.state.mission_store().unwrap();
    store
        .set_state(&id, missions::MissionState::Running, None, 3)
        .unwrap();
    for k in ["b.example", "c.example", "d.example"] {
        store
            .upsert_items(
                &id,
                &[missions::NewItem {
                    key: k.into(),
                    kind: "site".into(),
                    title: format!("Sign up on {k}"),
                    payload: None,
                }],
                4,
            )
            .unwrap();
    }
    let set = |k: &str, upd: missions::ItemUpdate| {
        store.update_item(&id, k, &upd, false, 5).unwrap();
    };
    set(
        "a.example",
        missions::ItemUpdate {
            status: Some(missions::ItemStatus::Done),
            result: Some("account created".into()),
            ..missions::ItemUpdate::default()
        },
    );
    set(
        "b.example",
        missions::ItemUpdate {
            status: Some(missions::ItemStatus::Blocked),
            blocked_reason: Some("Which company name should I use?".into()),
            ..missions::ItemUpdate::default()
        },
    );
    set(
        "c.example",
        missions::ItemUpdate {
            status: Some(missions::ItemStatus::Blocked),
            blocked_reason: Some("a 2FA code is needed".into()),
            live_only: Some(true),
            ..missions::ItemUpdate::default()
        },
    );
    set(
        "d.example",
        missions::ItemUpdate {
            status: Some(missions::ItemStatus::Failed),
            error: Some("needs a payment".into()),
            ..missions::ItemUpdate::default()
        },
    );
    store
        .set_state(
            &id,
            missions::MissionState::Blocked,
            Some("needs the owner"),
            6,
        )
        .unwrap();
    id
}

#[tokio::test]
async fn every_notice_route_is_owner_only() {
    // A digest carries mission titles, the bot's reasons and its questions: a holder of only the
    // API token (a CLI engine, an MCP client, a script) must not read or change any of it.
    let t = setup();
    let id = uuid::Uuid::nil();
    for (m, path, body) in [
        (Method::GET, "/v1/owner-notices".to_owned(), json!({})),
        (
            Method::GET,
            "/v1/owner-notices/stream".to_owned(),
            json!({}),
        ),
        (
            Method::GET,
            "/v1/owner-notices/settings".to_owned(),
            json!({}),
        ),
        (
            Method::PUT,
            "/v1/owner-notices/settings".to_owned(),
            json!({"finished": true, "failed": true, "blocked": true, "progress": false}),
        ),
        (
            Method::POST,
            format!("/v1/owner-notices/{id}/answers"),
            json!({"answers": [{"key": "a", "answer": "b"}]}),
        ),
        (
            Method::POST,
            format!("/v1/owner-notices/{id}/retry"),
            json!({}),
        ),
    ] {
        let r = call_token_only(&t, m.clone(), &path, Some(body)).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{m} {path}");
        assert_eq!(
            r.json()["error"]["code"],
            "owner_key_required",
            "{m} {path}"
        );
    }
}

#[tokio::test]
async fn settings_default_sensibly_round_trip_and_refuse_a_bad_window() {
    let t = setup();
    let r = call(&t, Method::GET, "/v1/owner-notices/settings", None).await;
    assert_eq!(
        r.json(),
        json!({"finished": true, "failed": true, "blocked": true, "progress": false,
               "quiet_hours": null})
    );
    let want = json!({"finished": true, "failed": false, "blocked": true, "progress": true,
        "quiet_hours": {"start": "23:00", "end": "07:30", "timezone": "Europe/Istanbul"}});
    let r = call(
        &t,
        Method::PUT,
        "/v1/owner-notices/settings",
        Some(want.clone()),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    let r = call(&t, Method::GET, "/v1/owner-notices/settings", None).await;
    assert_eq!(r.json(), want);
    let mut bad = want;
    bad["quiet_hours"]["start"] = json!("late");
    let r = call(&t, Method::PUT, "/v1/owner-notices/settings", Some(bad)).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one ordered walk: conclude, push, answer
async fn a_blocked_mission_is_pushed_once_and_its_questions_are_answered_from_a_chat() {
    let t = setup();
    let bot = full_bot(&t).await;
    let id = blocked_mission(&t, &bot);
    let mut stream = open_stream(&t, "").await;
    read_until(&mut stream, |f| f.iter().any(|(_, ev, _)| ev == "hello")).await;

    t.state
        .mission_supervisor()
        .unwrap()
        .conclude(&id)
        .await
        .unwrap();

    let frames = read_until(&mut stream, |f| !notices(f).is_empty()).await;
    let pushed = notices(&frames);
    assert_eq!(pushed.len(), 1, "one message batches everything");
    let n = pushed[0].clone();
    assert_eq!(n["kind"], "blocked");
    assert_eq!(n["source"], "mission");
    assert_eq!(n["mission_id"], id.as_str());
    assert_eq!(n["bot_name"], "Scout");
    assert_eq!(n["questions"].as_array().unwrap().len(), 1);
    assert_eq!(n["questions"][0]["key"], "b.example");
    assert_eq!(
        n["questions"][0]["text"],
        "Which company name should I use?"
    );
    assert_eq!(
        n["app_only"].as_array().unwrap().len(),
        1,
        "the code waits in the app"
    );
    assert_eq!(n["progress"]["done"], 1);
    assert_eq!(n["failed"][0]["key"], "d.example");
    let notice_id = n["id"].as_str().unwrap().to_owned();

    // The panel reads the same notice.
    let list = call(&t, Method::GET, "/v1/owner-notices", None)
        .await
        .json();
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["id"], notice_id.as_str());

    let path = format!("/v1/owner-notices/{notice_id}/answers");
    // Only the questions the notice lists: not the live-only item, not an unknown key.
    for key in ["c.example", "z.example"] {
        let r = call(
            &t,
            Method::POST,
            &path,
            Some(json!({"answers": [{"key": key, "answer": "482913"}]})),
        )
        .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{key}: {:?}", r.json());
    }
    let store = t.state.mission_store().unwrap();
    assert_eq!(store.progress(&id).unwrap().blocked, 2, "nothing changed");

    let r = call(
        &t,
        Method::POST,
        &path,
        Some(json!({"answers": [{"key": "b.example", "answer": "Webioo Ltd"}]})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    assert_eq!(r.json(), json!({"answered": 1, "skipped": 0}));
    // A notice answers once: the same message again changes nothing and says so.
    let again = call(
        &t,
        Method::POST,
        &path,
        Some(json!({"answers": [{"key": "b.example", "answer": "Other Ltd"}]})),
    )
    .await;
    assert_eq!(again.status, StatusCode::CONFLICT, "{:?}", again.json());
    let item = store
        .items(&id, None)
        .unwrap()
        .into_iter()
        .find(|i| i.key == "b.example")
        .unwrap();
    assert_ne!(
        item.status,
        missions::ItemStatus::Blocked,
        "the answer released the item"
    );

    // Nothing to retry on a notice that is answered by words.
    let r = call(
        &t,
        Method::POST,
        &format!("/v1/owner-notices/{notice_id}/retry"),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_mission_blocked_on_a_budget_is_resumed_by_try_again() {
    let t = setup();
    let bot = full_bot(&t).await;
    t.state.enable_missions().unwrap();
    t.state.start_missions();
    let (id, _, _) = seed_mission(&t, &bot);
    let store = t.state.mission_store().unwrap();
    store
        .set_state(&id, missions::MissionState::Running, None, 5)
        .unwrap();
    store
        .set_state(
            &id,
            missions::MissionState::Blocked,
            Some("session budget used up"),
            6,
        )
        .unwrap();
    t.state
        .mission_supervisor()
        .unwrap()
        .conclude(&id)
        .await
        .unwrap();
    let list = call(&t, Method::GET, "/v1/owner-notices", None)
        .await
        .json();
    let n = &list[0];
    assert_eq!(n["kind"], "blocked");
    assert_eq!(n["can_retry"], true);
    assert_eq!(n["reason"], "session budget used up");
    let r = call(
        &t,
        Method::POST,
        &format!("/v1/owner-notices/{}/retry", n["id"].as_str().unwrap()),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{:?}", r.json());
    assert_ne!(
        store.get(&id).unwrap().unwrap().state,
        missions::MissionState::Blocked
    );
}

#[tokio::test]
async fn a_mission_the_owner_stopped_does_not_notify() {
    let t = setup();
    let bot = full_bot(&t).await;
    t.state.enable_missions().unwrap();
    t.state.start_missions();
    let (id, _, _) = seed_mission(&t, &bot);
    t.state
        .mission_supervisor()
        .unwrap()
        .cancel(&id)
        .await
        .unwrap();
    let list = call(&t, Method::GET, "/v1/owner-notices", None)
        .await
        .json();
    assert_eq!(list, json!([]));
}

#[tokio::test]
async fn unknown_notices_and_notices_without_questions_answer_cleanly() {
    let t = setup();
    let r = call(
        &t,
        Method::POST,
        &format!("/v1/owner-notices/{}/answers", uuid::Uuid::now_v7()),
        Some(json!({"answers": [{"key": "a", "answer": "b"}]})),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = call(
        &t,
        Method::POST,
        &format!("/v1/owner-notices/{}/retry", uuid::Uuid::now_v7()),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

fn scheduled_run(t: &T, bot: &str, kind: &str) -> RunId {
    scheduled_run_saying(t, bot, kind, "Posted 3 new leads to the sheet.", false)
}

/// A finished run whose last message is `said`, written after it read private data or not.
fn scheduled_run_saying(t: &T, bot: &str, kind: &str, said: &str, private: bool) -> RunId {
    let chat = storage::Chat {
        id: ChatId::new(),
        user_id: t.state.user_id(),
        bot_id: BotId(bot.parse().unwrap()),
        title: "Daily report".into(),
        created_at_ms: storage::now_ms(),
    };
    t.state.storage().insert_chat(&chat).unwrap();
    let run = RunId::new();
    t.state
        .storage()
        .insert_run(&storage::Run {
            id: run,
            user_id: t.state.user_id(),
            bot_id: BotId(bot.parse().unwrap()),
            chat_id: Some(chat.id),
            kind: kind.to_owned(),
            state: "succeeded".to_owned(),
            error: None,
            created_at_ms: storage::now_ms(),
            updated_at_ms: storage::now_ms(),
            parent_run_id: None,
            spec: None,
            computer_target: None,
            computer_node: None,
            computer_node_name: None,
            paused_at_ms: None,
            pause_reason: None,
        })
        .unwrap();
    t.state
        .storage()
        .insert_message(&storage::Message {
            id: uuid::Uuid::now_v7(),
            user_id: t.state.user_id(),
            chat_id: chat.id,
            run_id: Some(run),
            role: "assistant".into(),
            content: said.into(),
            created_at_ms: storage::now_ms(),
            exposure_untrusted: false,
            exposure_private: private,
            exposure_sources: Vec::new(),
        })
        .unwrap();
    run
}

async fn wait_for_notices(t: &T, want: usize) -> Value {
    for _ in 0..200 {
        let list = call(t, Method::GET, "/v1/owner-notices", None).await.json();
        if list.as_array().unwrap().len() >= want {
            return list;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("no notice arrived");
}

#[tokio::test]
async fn a_scheduled_run_that_finishes_on_its_own_is_reported_with_what_it_said() {
    let t = setup();
    let bot = full_bot(&t).await;
    t.state.start_notice_watcher();
    let sched = uuid::Uuid::now_v7();
    // A run in an ordinary chat is the owner's own, and is not reported here.
    let plain = scheduled_run(&t, &bot, "turn");
    t.state.agent().emit_run_event(
        plain,
        Event::RunFinished(protocol::event::RunFinished {
            run_id: plain,
            error: None,
        }),
    );
    let run = scheduled_run(&t, &bot, &format!("turn:schedule:{sched}:1700000000000:1"));
    t.state.agent().emit_run_event(
        run,
        Event::RunFinished(protocol::event::RunFinished {
            run_id: run,
            error: None,
        }),
    );
    let list = wait_for_notices(&t, 1).await;
    assert_eq!(list.as_array().unwrap().len(), 1, "{list}");
    let n = &list[0];
    assert_eq!(n["kind"], "finished");
    assert_eq!(n["source"], "run");
    assert_eq!(n["run_id"], run.to_string());
    assert_eq!(n["done"][0]["text"], "Posted 3 new leads to the sheet.");
    assert_eq!(n["bot_name"], "Scout");
}

#[tokio::test]
async fn a_failed_scheduled_run_gives_its_reason_and_offers_a_retry() {
    let t = setup();
    let bot = full_bot(&t).await;
    t.state.start_notice_watcher();
    let sched = uuid::Uuid::now_v7();
    let run = scheduled_run(&t, &bot, &format!("turn:schedule:{sched}:1700000000000:1"));
    t.state
        .storage()
        .update_run_state(run, "failed", Some("provider unreachable"))
        .unwrap();
    t.state.agent().emit_run_event(
        run,
        Event::RunFinished(protocol::event::RunFinished {
            run_id: run,
            error: Some("provider unreachable".into()),
        }),
    );
    let list = wait_for_notices(&t, 1).await;
    let n = &list[0];
    assert_eq!(n["kind"], "failed");
    assert_eq!(n["reason"], "provider unreachable");
    assert_eq!(n["can_retry"], true);
}

#[tokio::test]
async fn questions_and_approvals_of_a_scheduled_run_arrive_as_one_notice_and_approvals_stay_in_the_app()
 {
    let t = setup();
    let bot = full_bot(&t).await;
    t.state.start_notice_watcher();
    let sched = uuid::Uuid::now_v7();
    let run = scheduled_run(&t, &bot, &format!("turn:schedule:{sched}"));
    let emit = |event: Event| t.state.agent().emit_run_event(run, event);
    emit(Event::QuestionAsked(protocol::event::QuestionAsked {
        run_id: run,
        call_id: protocol::ToolCallId::new(),
        question: "Which sheet should I use?".into(),
        options: vec![],
    }));
    emit(Event::ApprovalNeeded(protocol::event::ApprovalNeeded {
        run_id: run,
        call_id: protocol::ToolCallId::new(),
        tool_name: "shop_buy".into(),
        input: json!({}),
        summary: "Pay $12 for the premium listing".into(),
        details: None,
    }));
    emit(Event::QuestionAsked(protocol::event::QuestionAsked {
        run_id: run,
        call_id: protocol::ToolCallId::new(),
        question: "And which tab?".into(),
        options: vec![],
    }));
    let list = wait_for_notices(&t, 1).await;
    tokio::time::sleep(Duration::from_millis(700)).await;
    let list2 = call(&t, Method::GET, "/v1/owner-notices", None)
        .await
        .json();
    assert_eq!(
        list2.as_array().unwrap().len(),
        list.as_array().unwrap().len()
    );
    assert_eq!(
        list2.as_array().unwrap().len(),
        1,
        "all of it in one notice"
    );
    let n = &list2[0];
    assert_eq!(n["kind"], "blocked");
    assert_eq!(n["questions"].as_array().unwrap().len(), 2);
    assert_eq!(n["app_only"].as_array().unwrap().len(), 1);
    assert!(n["app_only"][0].as_str().unwrap().contains("Pay $12"));
    // There is no route that decides an approval through a notice.
    let notice = n["id"].as_str().unwrap();
    for suffix in ["approve", "decide", "approval"] {
        let r = call(
            &t,
            Method::POST,
            &format!("/v1/owner-notices/{notice}/{suffix}"),
            Some(json!({"decision": "approve"})),
        )
        .await;
        assert!(
            matches!(
                r.status,
                StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
            ),
            "{suffix}"
        );
    }
    // A question of a task is answered in words, never skipped.
    let key = n["questions"][0]["key"].as_str().unwrap();
    let r = call(
        &t,
        Method::POST,
        &format!("/v1/owner-notices/{notice}/answers"),
        Some(json!({"answers": [{"key": key, "skip": true}]})),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_bridge_that_reconnects_replays_what_it_missed() {
    let t = setup();
    let bot = full_bot(&t).await;
    let id = blocked_mission(&t, &bot);
    t.state
        .mission_supervisor()
        .unwrap()
        .conclude(&id)
        .await
        .unwrap();
    // A bridge that connects after the notice was pushed (this run) still gets it, and the hello
    // names the newest seq.
    let mut late = open_stream(&t, "").await;
    let f = read_until(&mut late, |f| !notices(f).is_empty()).await;
    assert_eq!(f[0].2["latest_seq"], 1);
    assert_eq!(notices(&f)[0]["kind"], "blocked");
    // One that says it last saw seq 1 gets nothing, one that last saw seq 0 gets the notice.
    let mut caught_up = open_stream(&t, "?after=1").await;
    let f = read_until(&mut caught_up, |f| f.iter().any(|(_, ev, _)| ev == "hello")).await;
    assert_eq!(notices(&f).len(), 0);
    let mut back = open_stream(&t, "?after=0").await;
    let f = read_until(&mut back, |f| !notices(f).is_empty()).await;
    assert_eq!(notices(&f)[0]["kind"], "blocked");
}

/// Ask a stream for more for a moment, as the HTTP connection does once it has taken a frame.
async fn poll_a_little(body: &mut Body) {
    let _ = tokio::time::timeout(Duration::from_millis(150), async {
        use http_body_util::BodyExt as _;
        let _ = body.frame().await;
    })
    .await;
}

#[tokio::test]
async fn a_restarted_bridge_gets_what_it_was_not_handed_and_nothing_twice() {
    let t = setup();
    let hub = t.state.owner_notify();
    let publish = |title: &str| {
        let mut d = crate::owner_notify::Draft::new(
            channel_api::notice::NoticeKind::Finished,
            channel_api::notice::NoticeSource::Run,
            title.to_owned(),
        );
        d.bot_name = "Scout".into();
        hub.publish(d, storage::now_ms()).unwrap();
    };
    let titles = |f: &[(String, String, Value)]| -> Vec<String> {
        notices(f)
            .iter()
            .map(|n| n["title"].as_str().unwrap().to_owned())
            .collect()
    };
    publish("one");
    publish("two");

    // The bridge connects, is handed both and keeps listening.
    let mut telegram = open_stream(&t, "?bridge=telegram").await;
    let f = read_until(&mut telegram, |f| notices(f).len() == 2).await;
    assert_eq!(titles(&f), vec!["one", "two"]);
    poll_a_little(&mut telegram).await;
    drop(telegram);

    // It restarts (no cursor): nothing is repeated. Another notice came while it was away.
    publish("three");
    let mut telegram = open_stream(&t, "?bridge=telegram").await;
    let f = read_until(&mut telegram, |f| !notices(f).is_empty()).await;
    assert_eq!(titles(&f), vec!["three"]);

    // The connection broke before the bridge read past it: the notice is still owed.
    drop(telegram);
    let mut telegram = open_stream(&t, "?bridge=telegram").await;
    let f = read_until(&mut telegram, |f| !notices(f).is_empty()).await;
    assert_eq!(
        titles(&f),
        vec!["three"],
        "taken but not confirmed: sent again"
    );
    poll_a_little(&mut telegram).await;
    drop(telegram);

    // A second bridge has its own cursor and was handed nothing yet.
    let mut discord = open_stream(&t, "?bridge=discord").await;
    let f = read_until(&mut discord, |f| notices(f).len() == 3).await;
    assert_eq!(titles(&f), vec!["one", "two", "three"]);
    poll_a_little(&mut discord).await;
    drop(discord);

    // Both are up to date now.
    for bridge in ["telegram", "discord"] {
        let mut s = open_stream(&t, &format!("?bridge={bridge}")).await;
        let f = read_until(&mut s, |f| f.iter().any(|(_, ev, _)| ev == "hello")).await;
        assert!(notices(&f).is_empty(), "{bridge} was handed everything");
    }
}

#[tokio::test]
async fn a_stale_mission_notice_cannot_answer_what_the_mission_asks_now() {
    let t = setup();
    let bot = full_bot(&t).await;
    let id = blocked_mission(&t, &bot);
    let store = t.state.mission_store().unwrap();
    // The mission blocked and the owner was told; it blocked again on the same item and the
    // owner was told again.
    t.state
        .mission_supervisor()
        .unwrap()
        .conclude(&id)
        .await
        .unwrap();
    let mission = store.get(&id).unwrap().unwrap();
    t.state.notify_mission_concluded(&mission);
    let list = call(&t, Method::GET, "/v1/owner-notices", None)
        .await
        .json();
    let (old, new) = (
        list[0]["id"].as_str().unwrap(),
        list[1]["id"].as_str().unwrap(),
    );
    let body = json!({"answers": [{"key": "b.example", "answer": "Webioo Ltd"}]});
    let r = call(
        &t,
        Method::POST,
        &format!("/v1/owner-notices/{old}/answers"),
        Some(body.clone()),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{:?}", r.json());
    assert_eq!(store.progress(&id).unwrap().blocked, 2, "nothing changed");
    let r = call(
        &t,
        Method::POST,
        &format!("/v1/owner-notices/{new}/answers"),
        Some(body),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
}

#[tokio::test]
async fn a_question_of_a_run_is_answered_only_while_the_run_still_waits_for_it() {
    let t = setup();
    let bot = full_bot(&t).await;
    t.state.start_notice_watcher();
    let sched = uuid::Uuid::now_v7();
    let run = scheduled_run(&t, &bot, &format!("turn:schedule:{sched}"));
    let call_id = protocol::ToolCallId::new();
    t.state.agent().emit_run_event(
        run,
        Event::QuestionAsked(protocol::event::QuestionAsked {
            run_id: run,
            call_id,
            question: "Which sheet?".into(),
            options: vec![],
        }),
    );
    let list = wait_for_notices(&t, 1).await;
    let notice = list[0]["id"].as_str().unwrap();
    let path = format!("/v1/owner-notices/{notice}/answers");
    // The wait is gone (answered in the app, or the run ended): a late chat message is refused
    // and says why, and the notice is not used up.
    let body = json!({"answers": [{"key": call_id.to_string(), "answer": "Sheet 2"}]});
    let r = call(&t, Method::POST, &path, Some(body.clone())).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{:?}", r.json());
    assert!(
        r.json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not waiting"),
        "{:?}",
        r.json()
    );
    // The same key twice in one message is a mistake, not two answers.
    let twice = json!({"answers": [
        {"key": call_id.to_string(), "answer": "a"},
        {"key": call_id.to_string(), "answer": "b"},
    ]});
    let r = call(&t, Method::POST, &path, Some(twice)).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{:?}", r.json());
}

#[tokio::test]
async fn what_a_run_said_after_reading_private_data_stays_out_of_the_notice() {
    let t = setup();
    let bot = full_bot(&t).await;
    t.state.start_notice_watcher();
    let said = "Your landlord wrote that the deposit of 4,200 EUR is due on the 3rd.";
    let sched = uuid::Uuid::now_v7();
    let run = scheduled_run_saying(&t, &bot, &format!("turn:schedule:{sched}"), said, true);
    t.state.agent().emit_run_event(
        run,
        Event::RunFinished(protocol::event::RunFinished {
            run_id: run,
            error: None,
        }),
    );
    let list = wait_for_notices(&t, 1).await;
    let text = list[0]["done"][0]["text"].as_str().unwrap();
    assert!(
        !text.contains("landlord") && !text.contains("4,200"),
        "{text}"
    );
    assert!(text.contains("open it in the app"), "{text}");
    assert!(!list.to_string().contains("landlord"));
}

#[tokio::test]
async fn a_run_that_asks_endlessly_makes_one_bounded_notice() {
    let t = setup();
    let bot = full_bot(&t).await;
    t.state.start_notice_watcher();
    let sched = uuid::Uuid::now_v7();
    let run = scheduled_run(&t, &bot, &format!("turn:schedule:{sched}"));
    for i in 0..200 {
        t.state.agent().emit_run_event(
            run,
            Event::QuestionAsked(protocol::event::QuestionAsked {
                run_id: run,
                call_id: protocol::ToolCallId::new(),
                question: format!("Question {i}?"),
                options: vec![],
            }),
        );
        t.state.agent().emit_run_event(
            run,
            Event::ApprovalNeeded(protocol::event::ApprovalNeeded {
                run_id: run,
                call_id: protocol::ToolCallId::new(),
                tool_name: "shop_buy".into(),
                input: json!({}),
                summary: format!("Pay {i}"),
                details: None,
            }),
        );
        // Let the watcher keep up, so the test is about the bound and not about lagging.
        tokio::task::yield_now().await;
    }
    let list = wait_for_notices(&t, 1).await;
    let n = &list[0];
    assert_eq!(
        n["questions"].as_array().unwrap().len(),
        channel_api::notice::MAX_LISTED + 1,
        "one more than a message lists, so it says there is more"
    );
    assert_eq!(
        n["app_only"].as_array().unwrap().len(),
        channel_api::notice::MAX_LISTED + 1
    );
    assert_eq!(
        n["questions"][0]["title"], "Question 0?",
        "the first are kept"
    );
}
