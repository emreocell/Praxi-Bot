#![allow(clippy::assert_is_empty, clippy::too_many_lines)]
//! Schedules: routes, the executor's dispatch, webhooks, shutdown order.

use std::sync::Arc;
use std::time::Duration;

use agent_core::{AgentLoop, LoopConfig, Providers, StreamItem};
use axum::http::{Method, StatusCode};
use permission_engine::Policy;
use protocol::{Event, RunId};
use serde_json::{Value, json};
use storage::Storage;
use tokio::sync::broadcast;
use tool_registry::ToolRegistry;
use workflow_replay::{Handles, Step, ToolStep, Workflow, WorkflowStore};

use super::{Danger, T, TOKEN, auth, call, make_chat, send, wait_state};
use crate::fake::FakeProvider;
use crate::{AppState, DaemonConfig, router};

/// A daemon with the workflow tools wired and the given global policy.
fn setup_with(policy: Policy) -> T {
    let dir = tempfile::tempdir().unwrap();
    let storage = Arc::new(Storage::open(dir.path()).unwrap());
    let tools = ToolRegistry::new();
    tools.register(Arc::new(Danger));
    let agent = AgentLoop::new(
        Arc::clone(&storage),
        tools,
        Providers::single(Arc::new(FakeProvider)),
        LoopConfig {
            global_policy: policy,
            legacy_tool_surface: true,
            ..LoopConfig::default()
        },
    )
    .with_workflows(
        WorkflowStore::new(dir.path()),
        Handles {
            desktop: None,
            browser: None,
            tools: ToolRegistry::new(),
        },
    );
    let state = AppState::new(agent.clone(), storage, DaemonConfig::new(TOKEN)).unwrap();
    state.set_owner_key(crate::tests::OWNER_KEY.into());
    T {
        app: router(state.clone()),
        state,
        agent,
        _dir: dir,
    }
}

fn save_workflow(t: &T, bot: &str) {
    let mut wf = Workflow::new("danger_wf");
    wf.steps = vec![Step::Tool(ToolStep {
        tool: "danger_run".to_owned(),
        input: json!({}),
    })];
    WorkflowStore::new(t.state.storage().data_dir())
        .for_bot(bot)
        .unwrap()
        .save(&wf)
        .unwrap();
}

#[allow(clippy::needless_pass_by_value)]
fn new_schedule(bot: &str, payload: Value) -> Value {
    json!({
        "bot_id": bot,
        "name": "nightly",
        "spec": {"every": {"seconds": 3600}},
        "payload": payload,
    })
}

async fn create(t: &T, body: Value) -> Value {
    let r = call(t, Method::POST, "/v1/schedules", Some(body)).await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.json());
    r.json()
}

fn prompt(text: &str) -> Value {
    json!({"kind": "agent_prompt", "text": text})
}

/// Follows the run a schedule starts through the loop's event stream.
struct Follow {
    sid: String,
    mine: Option<RunId>,
    evs: Vec<Event>,
}

impl Follow {
    fn new(sid: &str) -> Self {
        Self {
            sid: sid.to_owned(),
            mine: None,
            evs: Vec::new(),
        }
    }

    /// Record the next event.
    async fn step(&mut self, rx: &mut broadcast::Receiver<StreamItem>) -> Event {
        let item = tokio::time::timeout(Duration::from_secs(20), rx.recv())
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "timed out waiting for the scheduled run; saw {:?}",
                    self.evs
                )
            })
            .expect("stream closed");
        let ev = item.envelope.event;
        if let Event::RunStarted(r) = &ev
            && r.schedule_id.map(|s| s.to_string()).as_deref() == Some(self.sid.as_str())
        {
            self.mine = Some(r.run_id);
        }
        if self.mine.is_some() {
            self.evs.push(ev.clone());
        }
        ev
    }

    /// Until the run asks for an approval; returns its call id.
    async fn until_approval(
        &mut self,
        rx: &mut broadcast::Receiver<StreamItem>,
    ) -> protocol::ToolCallId {
        loop {
            if let Event::ApprovalNeeded(a) = self.step(rx).await
                && Some(a.run_id) == self.mine
            {
                return a.call_id;
            }
        }
    }

    /// Until the run is finished.
    async fn until_finished(
        mut self,
        rx: &mut broadcast::Receiver<StreamItem>,
    ) -> (RunId, Vec<Event>) {
        loop {
            let ev = self.step(rx).await;
            if let (Event::RunFinished(f), Some(m)) = (&ev, self.mine)
                && f.run_id == m
            {
                return (m, self.evs);
            }
        }
    }
}

/// The run a schedule started and its events up to `run_finished`.
async fn scheduled_run(rx: &mut broadcast::Receiver<StreamItem>, sid: &str) -> (RunId, Vec<Event>) {
    Follow::new(sid).until_finished(rx).await
}

/// Wait until the scheduler has released the schedule (the firing returned).
async fn settled(t: &T) {
    t.state.scheduler().unwrap().settle().await;
}

async fn last_assistant(t: &T, chat: &str) -> String {
    let msgs = call(t, Method::GET, &format!("/v1/chats/{chat}/messages"), None)
        .await
        .json();
    msgs.as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|m| m["role"] == "assistant")
        .map(|m| m["content"].as_str().unwrap().to_owned())
        .unwrap_or_default()
}

// ------------------------------------------------------------------ CRUD

#[tokio::test]
async fn schedule_crud_and_validation() {
    let t = super::setup();
    let (bot, chat) = make_chat(&t).await;

    let s = create(&t, new_schedule(&bot, prompt("hi"))).await;
    let id = s["id"].as_str().unwrap().to_owned();
    assert_eq!(s["enabled"], true);
    assert_eq!(s["has_webhook"], false);
    assert_eq!(s["misfire_policy"], "skip");
    assert!(s["next_run_at_ms"].as_i64().unwrap() > storage::now_ms());
    assert!(s["last_run_at_ms"].is_null());
    assert_eq!(s["missed_occurrences"], 0);

    let list = call(&t, Method::GET, "/v1/schedules", None).await.json();
    assert_eq!(list.as_array().unwrap().len(), 1);
    let one = call(&t, Method::GET, &format!("/v1/schedules/{id}"), None).await;
    assert_eq!(one.status, StatusCode::OK);
    assert_eq!(one.json()["name"], "nightly");

    // Update: new spec re-arms the timer, a webhook secret flips has_webhook,
    // the bot cannot change.
    let mut body = new_schedule(&bot, prompt("hello"));
    body["spec"] = json!({"every": {"seconds": 60}});
    body["chat_id"] = json!(chat);
    body["webhook_secret"] = json!("k");
    body["misfire_policy"] = json!("run_once");
    let r = call(&t, Method::PUT, &format!("/v1/schedules/{id}"), Some(body)).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    let u = r.json();
    assert_eq!(u["spec"], json!({"every": {"seconds": 60}}));
    assert_eq!(u["payload"]["text"], "hello");
    assert_eq!(u["chat_id"], json!(chat));
    assert_eq!(u["has_webhook"], true);
    assert_eq!(u["misfire_policy"], "run_once");
    assert!(u["next_run_at_ms"].as_i64().unwrap() < s["next_run_at_ms"].as_i64().unwrap());
    // Disabling clears the timer; the secret stays when omitted.
    let mut body = new_schedule(&bot, prompt("hello"));
    body["enabled"] = json!(false);
    let r = call(&t, Method::PUT, &format!("/v1/schedules/{id}"), Some(body)).await;
    assert_eq!(r.json()["enabled"], false);
    assert!(r.json()["next_run_at_ms"].is_null());
    assert_eq!(r.json()["has_webhook"], true);
    let mut body = new_schedule(&bot, prompt("hello"));
    body["webhook_secret"] = json!("");
    let r = call(&t, Method::PUT, &format!("/v1/schedules/{id}"), Some(body)).await;
    assert_eq!(r.json()["has_webhook"], false);
    let other_bot = make_chat(&t).await.0;
    let r = call(
        &t,
        Method::PUT,
        &format!("/v1/schedules/{id}"),
        Some(new_schedule(&other_bot, prompt("x"))),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);

    // Validation.
    for (what, mut body) in [
        ("bad spec", new_schedule(&bot, prompt("x"))),
        ("bad cron", new_schedule(&bot, prompt("x"))),
        ("zero interval", new_schedule(&bot, prompt("x"))),
        ("bad payload", new_schedule(&bot, json!({"kind": "nope"}))),
        ("empty prompt", new_schedule(&bot, prompt("  "))),
        ("bad policy", new_schedule(&bot, prompt("x"))),
    ] {
        match what {
            "bad spec" => body["spec"] = json!({"weekly": 1}),
            "bad cron" => body["spec"] = json!({"cron": "not a cron"}),
            "zero interval" => body["spec"] = json!({"every": {"seconds": 0}}),
            "bad policy" => body["misfire_policy"] = json!("sometimes"),
            _ => {}
        }
        let r = call(&t, Method::POST, "/v1/schedules", Some(body)).await;
        assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{what}");
        assert_eq!(r.json()["error"]["code"], "bad_schedule", "{what}");
    }
    let r = call(
        &t,
        Method::POST,
        "/v1/schedules",
        Some(new_schedule(&uuid::Uuid::now_v7().to_string(), prompt("x"))),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "unknown bot");

    // Unknown / malformed ids, auth, delete.
    let r = call(
        &t,
        Method::GET,
        &format!("/v1/schedules/{}", uuid::Uuid::now_v7()),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = call(&t, Method::GET, "/v1/schedules/nope", None).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = send(&t.app, Method::GET, "/v1/schedules", None, &[]).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = call(&t, Method::DELETE, &format!("/v1/schedules/{id}"), None).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r = call(&t, Method::DELETE, &format!("/v1/schedules/{id}"), None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let list = call(&t, Method::GET, "/v1/schedules", None).await.json();
    assert!(list.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn parse_route_understands_english_and_turkish_or_says_422() {
    let t = super::setup();
    let r = call(
        &t,
        Method::POST,
        "/v1/schedules/parse",
        Some(json!({"text": "every 15 minutes"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    assert_eq!(r.json()["spec"], json!({"every": {"seconds": 900}}));
    assert!(r.json()["next_run_at_ms"].as_i64().unwrap() > storage::now_ms());

    let r = call(
        &t,
        Method::POST,
        "/v1/schedules/parse",
        Some(json!({"text": "her gün 09:00", "tz": "Europe/Istanbul"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    let cron = r.json()["spec"]["cron"].as_str().unwrap().to_owned();
    assert!(
        cron.starts_with("0 9 * * *") && cron.contains("Europe/Istanbul"),
        "{cron}"
    );

    let r = call(
        &t,
        Method::POST,
        "/v1/schedules/parse",
        Some(json!({"text": "whenever the moon is full"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(r.json()["error"]["code"], "bad_schedule");
    let r = call(
        &t,
        Method::POST,
        "/v1/schedules/parse",
        Some(json!({"text": "her gün 09:00", "tz": "Mars/Olympus"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "bad timezone");
}

// -------------------------------------------------- executor and firing

#[tokio::test]
async fn run_now_dispatches_a_prompt_into_a_chat_named_after_the_schedule() {
    let t = super::setup();
    t.state.start_scheduler().unwrap();
    let (bot, _) = make_chat(&t).await;
    let s = create(&t, new_schedule(&bot, prompt("water the plants"))).await;
    let sid = s["id"].as_str().unwrap().to_owned();
    let mut rx = t.agent.subscribe();
    let r = call(
        &t,
        Method::POST,
        &format!("/v1/schedules/{sid}/run-now"),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{:?}", r.json());
    assert_eq!(r.json()["status"], "accepted");
    let (run, evs) = scheduled_run(&mut rx, &sid).await;

    // A normal run: visible in /v1/runs, kind carries the schedule, events have it.
    let got = call(&t, Method::GET, &format!("/v1/runs/{run}"), None)
        .await
        .json();
    assert_eq!(got["state"], "succeeded");
    assert_eq!(got["kind"], format!("turn:schedule:{sid}"));
    assert_eq!(got["schedule_id"], sid, "RunDto names the schedule");
    let listed = call(&t, Method::GET, "/v1/runs", None).await.json();
    assert!(
        listed
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == run.to_string() && r["schedule_id"] == sid)
    );
    assert!(matches!(evs.first(), Some(Event::RunStarted(r)) if r.run_id == run));
    // The chat was created and named after the schedule.
    let chats = call(&t, Method::GET, "/v1/chats", None).await.json();
    let chat = chats
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["title"] == "nightly")
        .expect("chat named after the schedule");
    assert_eq!(
        last_assistant(&t, chat["id"].as_str().unwrap()).await,
        "echo: water the plants"
    );
    // The timer is untouched; last_run_at is set.
    let after = call(&t, Method::GET, &format!("/v1/schedules/{sid}"), None)
        .await
        .json();
    assert_eq!(after["next_run_at_ms"], s["next_run_at_ms"]);
    assert!(after["last_run_at_ms"].as_i64().is_some());

    // Firing again reuses the chat.
    settled(&t).await;
    let mut rx = t.agent.subscribe();
    call(
        &t,
        Method::POST,
        &format!("/v1/schedules/{sid}/run-now"),
        None,
    )
    .await;
    scheduled_run(&mut rx, &sid).await;
    let chats = call(&t, Method::GET, "/v1/chats", None).await.json();
    assert_eq!(
        chats
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["title"] == "nightly")
            .count(),
        1
    );
}

#[tokio::test]
async fn run_now_dispatches_a_workflow_and_a_tool_through_the_gate() {
    let t = setup_with(Policy::full());
    t.state.start_scheduler().unwrap();
    let (bot, _) = make_chat(&t).await;
    save_workflow(&t, &bot);

    let wf = create(
        &t,
        new_schedule(
            &bot,
            json!({"kind": "workflow", "name": "danger_wf", "params": {}}),
        ),
    )
    .await;
    let wf_id = wf["id"].as_str().unwrap().to_owned();
    let mut rx = t.agent.subscribe();
    call(
        &t,
        Method::POST,
        &format!("/v1/schedules/{wf_id}/run-now"),
        None,
    )
    .await;
    let (run, evs) = scheduled_run(&mut rx, &wf_id).await;
    assert!(
        evs.iter()
            .any(|e| matches!(e, Event::WorkflowStepStarted(s) if s.workflow == "danger_wf")),
        "workflow replayed"
    );
    wait_state(&t, &run.to_string(), "succeeded").await;

    let tool = create(
        &t,
        new_schedule(
            &bot,
            json!({"kind": "tool", "name": "danger_run", "input": {}}),
        ),
    )
    .await;
    let tool_id = tool["id"].as_str().unwrap().to_owned();
    let mut rx = t.agent.subscribe();
    call(
        &t,
        Method::POST,
        &format!("/v1/schedules/{tool_id}/run-now"),
        None,
    )
    .await;
    let (run, evs) = scheduled_run(&mut rx, &tool_id).await;
    assert!(
        evs.iter()
            .any(|e| matches!(e, Event::ToolCallCompleted(c) if c.error.is_none() && c.output_preview.as_deref() == Some("boom done"))),
        "tool called: {evs:?}"
    );
    wait_state(&t, &run.to_string(), "succeeded").await;
}

#[tokio::test]
async fn a_due_schedule_fires_at_startup_and_starts_a_run_with_its_id() {
    let t = super::setup();
    let (bot, _) = make_chat(&t).await;
    let mut body = new_schedule(&bot, prompt("catch up"));
    // Overdue by a minute; `run_once` fires it when the scheduler starts.
    body["spec"] = json!({"at": {"unix_ms": storage::now_ms() - 60_000}});
    body["misfire_policy"] = json!("run_once");
    let s = create(&t, body).await;
    let sid = s["id"].as_str().unwrap().to_owned();
    let mut rx = t.agent.subscribe();
    t.state.start_scheduler().unwrap();
    let (run, evs) = scheduled_run(&mut rx, &sid).await;
    let started = evs
        .iter()
        .find_map(|e| match e {
            Event::RunStarted(r) => Some(r.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(started.run_id, run);
    assert_eq!(started.schedule_id.unwrap().to_string(), sid);
    // The same event on the SSE log of the run.
    let log = storage::read_event_log(t.state.storage().data_dir(), run).unwrap();
    let json = serde_json::to_value(&log[0].event).unwrap();
    assert_eq!(json["type"], "run_started");
    assert_eq!(json["schedule_id"], sid);
}

#[tokio::test]
async fn schedule_runs_still_need_approval_and_the_schedule_stays_busy_meanwhile() {
    // The default policy asks for `danger_run`.
    let t = super::setup();
    t.state.start_scheduler().unwrap();
    let (bot, _) = make_chat(&t).await;
    let s = create(
        &t,
        new_schedule(
            &bot,
            json!({"kind": "tool", "name": "danger_run", "input": {}}),
        ),
    )
    .await;
    let sid = s["id"].as_str().unwrap().to_owned();
    let mut rx = t.agent.subscribe();
    call(
        &t,
        Method::POST,
        &format!("/v1/schedules/{sid}/run-now"),
        None,
    )
    .await;
    let mut follow = Follow::new(&sid);
    let call_id = follow.until_approval(&mut rx).await;
    // In flight (waiting for the user): run-now is refused.
    let r = call(
        &t,
        Method::POST,
        &format!("/v1/schedules/{sid}/run-now"),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{:?}", r.json());
    let r = call(
        &t,
        Method::POST,
        &format!("/v1/approvals/{call_id}"),
        Some(json!({"decision": "approve"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    let (run, _) = follow.until_finished(&mut rx).await;
    wait_state(&t, &run.to_string(), "succeeded").await;
}

#[tokio::test]
async fn run_now_needs_a_running_scheduler_and_a_known_schedule() {
    let t = super::setup();
    let (bot, _) = make_chat(&t).await;
    let s = create(&t, new_schedule(&bot, prompt("x"))).await;
    let sid = s["id"].as_str().unwrap();
    let r = call(
        &t,
        Method::POST,
        &format!("/v1/schedules/{sid}/run-now"),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    t.state.start_scheduler().unwrap();
    let r = call(
        &t,
        Method::POST,
        &format!("/v1/schedules/{}/run-now", uuid::Uuid::now_v7()),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

// -------------------------------------------------------------- webhooks

fn sig(secret: &str, body: &str) -> String {
    scheduler::sign_webhook_body(secret, body.as_bytes())
}

/// POST without the bearer token.
async fn hook(t: &T, id: &str, body: &str, extra: &[(&str, &str)]) -> super::Resp {
    use axum::body::Body;
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let mut b = axum::http::Request::builder()
        .method(Method::POST)
        .uri(format!("/v1/webhooks/{id}"));
    let mut has_host = false;
    for (k, v) in extra {
        has_host |= k.eq_ignore_ascii_case("host");
        b = b.header(*k, *v);
    }
    if !has_host {
        b = b.header("host", "127.0.0.1:47821");
    }
    let resp = t
        .app
        .clone()
        .oneshot(b.body(Body::from(body.to_owned())).unwrap())
        .await
        .unwrap();
    let (parts, body) = resp.into_parts();
    super::Resp {
        status: parts.status,
        headers: parts.headers,
        body: body.collect().await.unwrap().to_bytes().to_vec(),
    }
}

#[tokio::test]
async fn webhook_auth_size_and_status_codes() {
    let t = super::setup();
    t.state.start_scheduler().unwrap();
    let (bot, _) = make_chat(&t).await;
    let mut body = new_schedule(&bot, prompt("deploy said {input}"));
    body["webhook_secret"] = json!("s3cret");
    let s = create(&t, body).await;
    let sid = s["id"].as_str().unwrap().to_owned();
    let plain = create(&t, new_schedule(&bot, prompt("no hook"))).await;
    let plain_id = plain["id"].as_str().unwrap().to_owned();

    // No bearer token needed, but a signature is.
    let r = hook(&t, &sid, "ok", &[]).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "no signature");
    assert_eq!(r.json()["error"]["code"], "invalid_signature");
    let r = hook(&t, &sid, "ok", &[("x-signature-256", &sig("wrong", "ok"))]).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "wrong secret");
    let r = hook(
        &t,
        &sid,
        "ok2",
        &[("x-signature-256", &sig("s3cret", "ok"))],
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::UNAUTHORIZED,
        "signature of another body"
    );
    let r = hook(&t, &sid, "ok", &[("x-signature-256", "sha256=zz")]).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "garbage signature");
    // 404: unknown schedule, schedule without a trigger.
    let r = hook(
        &t,
        &uuid::Uuid::now_v7().to_string(),
        "ok",
        &[("x-signature-256", &sig("s3cret", "ok"))],
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = hook(
        &t,
        &plain_id,
        "ok",
        &[("x-signature-256", &sig("s3cret", "ok"))],
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "no webhook trigger");
    let r = hook(&t, "nope", "ok", &[]).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    // Host check still applies; nothing ran so far.
    let r = hook(
        &t,
        &sid,
        "ok",
        &[
            ("host", "evil.com"),
            ("x-signature-256", &sig("s3cret", "ok")),
        ],
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = hook(
        &t,
        &sid,
        "ok",
        &[
            ("origin", "https://evil.com"),
            ("x-signature-256", &sig("s3cret", "ok")),
        ],
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "foreign origin");
    // Body limit: 256 KB.
    let big = "a".repeat(256 * 1024 + 1);
    let r = hook(&t, &sid, &big, &[("x-signature-256", &sig("s3cret", &big))]).await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        call(&t, Method::GET, "/v1/runs", None).await.json(),
        json!([]),
        "nothing ran for any refused call"
    );
    // The other routes still need the bearer token.
    let r = send(
        &t.app,
        Method::POST,
        &format!("/v1/schedules/{sid}/run-now"),
        None,
        &[],
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);

    // Exactly 256 KB is fine; a valid call is accepted and the body is `{input}`.
    let mut rx = t.agent.subscribe();
    let edge = "b".repeat(256 * 1024);
    let r = hook(
        &t,
        &sid,
        &edge,
        &[("x-signature-256", &sig("s3cret", &edge))],
    )
    .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{:?}", r.json());
    assert_eq!(r.json()["status"], "accepted");
    scheduled_run(&mut rx, &sid).await;
    settled(&t).await;
    let mut rx = t.agent.subscribe();
    let r = hook(
        &t,
        &sid,
        "v1.2",
        &[("x-signature-256", &sig("s3cret", "v1.2"))],
    )
    .await;
    assert_eq!(r.status, StatusCode::ACCEPTED);
    let (_, evs) = scheduled_run(&mut rx, &sid).await;
    let echoed: String = evs
        .iter()
        .filter_map(|e| match e {
            Event::MessageDelta(d) => Some(d.text.as_str()),
            _ => None,
        })
        .collect();
    assert!(echoed.contains("deploy said"));
    assert!(echoed.contains("v1.2"));
    assert!(echoed.contains("untrusted source data"));
    settled(&t).await;

    // 409: disabled.
    let mut body = new_schedule(&bot, prompt("deploy said {input}"));
    body["enabled"] = json!(false);
    let r = call(&t, Method::PUT, &format!("/v1/schedules/{sid}"), Some(body)).await;
    assert_eq!(r.status, StatusCode::OK);
    let r = hook(&t, &sid, "ok", &[("x-signature-256", &sig("s3cret", "ok"))]).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "disabled");
}

#[tokio::test]
async fn webhook_is_409_while_a_run_is_in_flight_and_503_without_a_scheduler() {
    let t = super::setup();
    let (bot, _) = make_chat(&t).await;
    let mut body = new_schedule(
        &bot,
        json!({"kind": "tool", "name": "danger_run", "input": {}}),
    );
    body["webhook_secret"] = json!("k");
    let s = create(&t, body).await;
    let sid = s["id"].as_str().unwrap().to_owned();
    let r = hook(&t, &sid, "x", &[("x-signature-256", &sig("k", "x"))]).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);

    t.state.start_scheduler().unwrap();
    let mut rx = t.agent.subscribe();
    let r = hook(&t, &sid, "x", &[("x-signature-256", &sig("k", "x"))]).await;
    assert_eq!(r.status, StatusCode::ACCEPTED);
    let mut follow = Follow::new(&sid);
    let call_id = follow.until_approval(&mut rx).await;
    let r = hook(&t, &sid, "x", &[("x-signature-256", &sig("k", "x"))]).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "busy");
    call(
        &t,
        Method::POST,
        &format!("/v1/approvals/{call_id}"),
        Some(json!({"decision": "deny"})),
    )
    .await;
    follow.until_finished(&mut rx).await;
}

// -------------------------------------------------------------- shutdown

#[tokio::test]
async fn shutdown_stops_the_scheduler_before_the_agent_loop() {
    let t = super::setup();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(crate::serve(listener, t.state.clone(), async move {
        let _ = rx.await;
    }));
    // `serve` opens the store and starts the scheduler.
    for _ in 0..200 {
        if t.state.scheduler().is_ok() {
            break;
        }
        tokio::task::yield_now().await;
    }
    let scheduler = t.state.scheduler().expect("serve started the scheduler");
    tx.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("serve returns")
        .unwrap()
        .unwrap();
    assert_eq!(t.state.shutdown_order(), ["scheduler", "agent"]);
    // A stopped scheduler does not tick any more.
    assert!(
        t.state.scheduler().is_err(),
        "stopped engine is detached for a future activation"
    );
    tokio::time::timeout(Duration::from_secs(5), scheduler.tick())
        .await
        .expect("tick returns at once on a stopped scheduler");
    let _ = auth();
}

#[tokio::test]
async fn schedule_mutations_are_announced_on_the_global_stream() {
    let t = super::setup();
    let (bot, chat) = make_chat(&t).await;
    let mut body = super::open_global(&t, None).await;

    let s = create(&t, new_schedule(&bot, prompt("tick"))).await;
    let sid = s["id"].as_str().unwrap().to_owned();
    let put = |enabled: bool, name: &str| {
        let mut b = new_schedule(&bot, prompt("tick"));
        b["enabled"] = json!(enabled);
        b["name"] = json!(name);
        b
    };
    let r = call(
        &t,
        Method::PUT,
        &format!("/v1/schedules/{sid}"),
        Some(put(true, "renamed")),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    let r = call(
        &t,
        Method::PUT,
        &format!("/v1/schedules/{sid}"),
        Some(put(false, "renamed")),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    let r = call(&t, Method::DELETE, &format!("/v1/schedules/{sid}"), None).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    // A rejected change announces nothing; neither does a read.
    let r = call(&t, Method::DELETE, &format!("/v1/schedules/{sid}"), None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let _ = call(&t, Method::GET, "/v1/schedules", None).await;
    // A run event after them proves the order and that nothing else came.
    let run = super::send_msg(&t, &chat, "after", "queue").await["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let fr = super::read_until(&mut body, |fr| super::run_finished_of(fr, &run)).await;
    let changes: Vec<(String, String)> = fr
        .iter()
        .filter(|f| f.1 == "schedule_changed")
        .map(|f| {
            assert_eq!(f.0, "", "no SSE id: not part of the replay cursor");
            assert_eq!(f.2["type"], "schedule_changed");
            (
                f.2["schedule_id"].as_str().unwrap().to_owned(),
                f.2["kind"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    let kinds: Vec<&str> = changes.iter().map(|c| c.1.as_str()).collect();
    assert_eq!(kinds, ["created", "updated", "toggled", "deleted"]);
    assert!(changes.iter().all(|c| c.0 == sid));
}

// ------------------------------------------- bot-made schedules: ownership & approval

#[tokio::test]
async fn editing_a_bot_made_schedule_in_the_ui_hands_it_to_the_user() {
    use crate::schedule_tools::DaemonScheduleHost;
    let t = setup_with(Policy::approve_destructive());
    let (bot, _chat) = make_chat(&t).await;
    let created = create(&t, new_schedule(&bot, prompt("hi"))).await;
    assert_eq!(
        created["origin"], "user",
        "API-made schedules are the user's"
    );
    let id = created["id"].as_str().unwrap().to_owned();
    let tid = scheduler::TaskId(uuid::Uuid::parse_str(&id).unwrap());
    let store = t.state.schedule_store().unwrap();
    store
        .set_origin(tid, scheduler::ScheduleOrigin::Bot)
        .unwrap();
    let listed = call(&t, Method::GET, &format!("/v1/schedules/{id}"), None)
        .await
        .json();
    assert_eq!(listed["origin"], "bot", "the list can show a badge");
    let rev = store.get(tid).unwrap().unwrap().revision;
    // The user rewrites it in the UI.
    let mut body = new_schedule(&bot, prompt("rewritten by the user"));
    body["name"] = json!("user's version");
    let r = call(&t, Method::PUT, &format!("/v1/schedules/{id}"), Some(body)).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    assert_eq!(r.json()["origin"], "user");
    let after = store.get(tid).unwrap().unwrap();
    assert_eq!(after.origin, scheduler::ScheduleOrigin::User);
    assert!(after.revision > rev);
    // So the bot can no longer change what the user wrote.
    let (tx, _rx) = tokio::sync::broadcast::channel(4);
    let host = DaemonScheduleHost::new(
        store,
        t.state.storage_arc(),
        t.state.user_id(),
        tx,
        crate::ScheduleLimits::default(),
    );
    let me = builtin_tools::ScheduleCaller {
        bot_id: bot.clone(),
        run_id: String::new(),
    };
    let e = builtin_tools::ScheduleHost::preview(
        &host,
        &me,
        &builtin_tools::ScheduleChange::Pause { id },
    )
    .unwrap_err();
    assert!(e.contains("created by the user"), "{e}");
}

#[tokio::test]
async fn a_schedule_approval_over_http_cannot_be_an_always_grant() {
    let t = setup_with(Policy::approve_destructive());
    t.state
        .enable_schedule_tools(crate::ScheduleLimits {
            timezone: Some("UTC".into()),
            ..crate::ScheduleLimits::default()
        })
        .unwrap();
    let (bot, chat) = make_chat(&t).await;
    let msg = r#"call:schedule_create {"name":"Standup","prompt":"say hi","when":"every weekday at 10:30"}"#;
    let run = super::send_msg(&t, &chat, msg, "queue").await["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (call_id, _) = super::pending_call(&t).await;
    // Crafted approvals asking for a standing grant are refused, server-side.
    for scope in ["always", "always_category", "this_chat"] {
        let r = call(
            &t,
            Method::POST,
            &format!("/v1/approvals/{call_id}"),
            Some(json!({"decision": "approve", "scope": scope})),
        )
        .await;
        assert_eq!(
            r.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{scope}: {:?}",
            r.json()
        );
        assert_eq!(r.json()["error"]["code"], "approval_once_only");
    }
    assert!(
        t.state
            .schedule_store()
            .unwrap()
            .list_by_bot(scheduler::BotId(uuid::Uuid::parse_str(&bot).unwrap()))
            .unwrap()
            .is_empty(),
        "nothing was created by the refused approvals"
    );
    // The card carried the details; approving once creates exactly what it showed.
    let ok = call(
        &t,
        Method::POST,
        &format!("/v1/approvals/{call_id}"),
        Some(json!({"decision": "approve", "scope": "once"})),
    )
    .await;
    assert_eq!(ok.status, StatusCode::OK, "{:?}", ok.json());
    super::wait_state(&t, &run, "succeeded").await;
    let stored = t
        .state
        .schedule_store()
        .unwrap()
        .list_by_bot(scheduler::BotId(uuid::Uuid::parse_str(&bot).unwrap()))
        .unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].origin, scheduler::ScheduleOrigin::Bot);
    let grants = call(&t, Method::GET, &format!("/v1/bots/{bot}/grants"), None).await;
    assert_eq!(
        grants.json().as_array().map_or(0, Vec::len),
        0,
        "no grant exists"
    );
    let log = super::events(&t, &format!("?run_id={run}"), None).await;
    let needed = log.iter().find(|f| f.1 == "approval_needed").unwrap();
    let d = &needed.2["event"]["details"];
    assert_eq!(d["kind"], "schedule");
    assert_eq!(d["prompt"], "say hi");
    assert_eq!(d["schedule"], "Every weekday at 10:30");
}

#[tokio::test]
async fn c225_false_precheck_creates_no_run_or_model_event_and_counts_skip() {
    let t = super::setup();
    let (bot, _) = make_chat(&t).await;
    let mut body = new_schedule(&bot, prompt("must never run"));
    body["precheck"] = json!({"kind":"script","code":"return false;"});
    let created = create(&t, body).await;
    let id = created["id"].as_str().unwrap();
    t.state.start_scheduler().unwrap();
    let mut rx = t.agent.subscribe();
    for _ in 0..3 {
        let result = call(
            &t,
            Method::POST,
            &format!("/v1/schedules/{id}/run-now"),
            None,
        )
        .await;
        assert_eq!(result.status, StatusCode::ACCEPTED);
        settled(&t).await;
    }
    assert!(
        t.state
            .storage()
            .list_runs(t.state.user_id())
            .unwrap()
            .is_empty()
    );
    assert!(rx.try_recv().is_err());
    let result = call(&t, Method::GET, &format!("/v1/schedules/{id}"), None).await;
    assert_eq!(result.json()["skipped_by_precheck"], 3);
    t.state.scheduler().unwrap().shutdown().await;
}

#[tokio::test]
async fn c225_precheck_error_starts_external_run_with_fixed_note_not_upstream_error() {
    let t = super::setup();
    let (bot, _) = make_chat(&t).await;
    let mut body = new_schedule(&bot, prompt("review status"));
    body["precheck"] = json!({"kind":"script","code":"throw new Error('UPSTREAM_PRIVATE_ERROR');"});
    let created = create(&t, body).await;
    let id = created["id"].as_str().unwrap();
    t.state.start_scheduler().unwrap();
    let mut rx = t.agent.subscribe();
    let result = call(
        &t,
        Method::POST,
        &format!("/v1/schedules/{id}/run-now"),
        None,
    )
    .await;
    assert_eq!(result.status, StatusCode::ACCEPTED);
    let (run, _) = scheduled_run(&mut rx, id).await;
    settled(&t).await;
    let row = t.state.storage().get_run(run).unwrap().unwrap();
    assert!(row.kind.starts_with("turn:external_schedule:"));
    let messages = t
        .state
        .storage()
        .list_messages(row.chat_id.unwrap())
        .unwrap();
    let source = messages
        .iter()
        .find(|m| m.role == "external_schedule")
        .unwrap();
    assert!(source.content.contains(scheduler::precheck::FAILURE_NOTE));
    assert!(!source.content.contains("UPSTREAM_PRIVATE_ERROR"));
    assert!(messages.iter().all(|m| m.role != "user"));
    t.state.scheduler().unwrap().shutdown().await;
}

#[tokio::test]
async fn c225_plain_firing_after_webhook_or_removed_precheck_never_reuses_external_history() {
    for with_precheck in [false, true] {
        let t = super::setup();
        let (bot, _) = make_chat(&t).await;
        t.state.start_scheduler().unwrap();
        let mut body = new_schedule(&bot, prompt("review source {input}"));
        if with_precheck {
            body["precheck"] =
                json!({"kind":"script","code":"return {run:true,input:'external notice'};"});
        } else {
            body["webhook_secret"] = json!("fake-secret");
        }
        let created = create(&t, body).await;
        let sid = created["id"].as_str().unwrap();
        let mut rx = t.agent.subscribe();
        let response = if with_precheck {
            call(
                &t,
                Method::POST,
                &format!("/v1/schedules/{sid}/run-now"),
                None,
            )
            .await
        } else {
            hook(
                &t,
                sid,
                "external notice",
                &[("x-signature-256", &sig("fake-secret", "external notice"))],
            )
            .await
        };
        assert_eq!(response.status, StatusCode::ACCEPTED);
        let (external_run, _) = scheduled_run(&mut rx, sid).await;
        settled(&t).await;
        let external = t.state.storage().get_run(external_run).unwrap().unwrap();
        let external_chat = external.chat_id.unwrap();
        assert!(external.kind.starts_with("turn:external_schedule:"));
        // Both implicit same-title reuse and an explicitly selected isolated
        // chat must fall back to a normal chat, without weakening isolation.
        for explicit in [false, true] {
            let mut edit = new_schedule(&bot, prompt("ordinary timer work"));
            edit["precheck"] = Value::Null;
            if explicit {
                edit["chat_id"] = json!(external_chat.to_string());
            }
            let response = call(&t, Method::PUT, &format!("/v1/schedules/{sid}"), Some(edit)).await;
            assert_eq!(response.status, StatusCode::OK, "{:?}", response.json());
            let mut rx = t.agent.subscribe();
            assert_eq!(
                call(
                    &t,
                    Method::POST,
                    &format!("/v1/schedules/{sid}/run-now"),
                    None
                )
                .await
                .status,
                StatusCode::ACCEPTED
            );
            let (plain_run, _) = scheduled_run(&mut rx, sid).await;
            settled(&t).await;
            let plain = t.state.storage().get_run(plain_run).unwrap().unwrap();
            assert!(plain.kind.starts_with("turn:schedule:"));
            assert_ne!(plain.chat_id, Some(external_chat));
            assert!(
                t.state
                    .storage()
                    .list_messages(plain.chat_id.unwrap())
                    .unwrap()
                    .iter()
                    .all(|m| m.role != "external_schedule")
            );
        }
        assert!(
            t.agent
                .run_turn(external_chat, "ordinary follow-up")
                .is_err()
        );
        t.state.stop_scheduler().await;
    }
}

#[tokio::test]
async fn c225_real_temp_folder_and_fake_clock_fire_one_external_run_for_five_matching_names() {
    let t = super::setup();
    let workspace = tempfile::tempdir().unwrap();
    let mut builtins = builtin_tools::BuiltinConfig::new(
        t.state.storage().data_dir().to_path_buf(),
        Arc::new(builtin_tools::ResolverGate::new(
            permission_engine::PermissionResolver::new(),
            Policy::full(),
            None,
            protocol::InitiatorKind::Schedule,
        )),
    );
    builtins.fs_allowed_roots = vec![workspace.path().to_path_buf()];
    let _ = t.agent.clone().with_builtin_tools(builtins);
    t.agent.set_global_policy(Policy::full());
    let (bot, _) = make_chat(&t).await;
    let clock = Arc::new(scheduler::FakeClock::new(storage::now_ms()));
    *t.state.0.scheduler_clock.lock() = Some(clock.clone());
    let mut body = new_schedule(&bot, prompt("review {input}"));
    body["trigger"] = json!({"kind":"file_watch","path":workspace.path(),"glob":"*.txt","events":["created"],"debounce_ms":1000});
    let created = create(&t, body).await;
    assert!(created["next_run_at_ms"].is_null());
    let sid = created["id"].as_str().unwrap();
    t.state.start_scheduler().unwrap();
    settled(&t).await;
    for n in 0..5 {
        std::fs::write(
            workspace.path().join(format!("file{n}.txt")),
            "PRIVATE_FILE_BODY",
        )
        .unwrap();
    }
    std::fs::write(workspace.path().join("excluded.csv"), "PRIVATE_FILE_BODY").unwrap();
    clock.advance_ms(30_000);
    settled(&t).await;
    assert_eq!(
        call(&t, Method::GET, "/v1/runs", None).await.json(),
        json!([])
    );
    clock.advance_ms(999);
    settled(&t).await;
    assert_eq!(
        call(&t, Method::GET, "/v1/runs", None).await.json(),
        json!([])
    );
    let mut rx = t.agent.subscribe();
    clock.advance_ms(1);
    settled(&t).await;
    let (run, _) = scheduled_run(&mut rx, sid).await;
    let row = t.state.storage().get_run(run).unwrap().unwrap();
    assert!(row.kind.starts_with("turn:external_schedule:"));
    let messages = t
        .state
        .storage()
        .list_messages(row.chat_id.unwrap())
        .unwrap();
    let source = messages
        .iter()
        .find(|m| m.role == "external_schedule")
        .unwrap();
    for n in 0..5 {
        assert!(source.content.contains(&format!("file{n}.txt")));
    }
    assert!(!source.content.contains("excluded.csv"));
    assert!(!source.content.contains("PRIVATE_FILE_BODY"));
    clock.advance_ms(30_000);
    settled(&t).await;
    assert_eq!(
        call(&t, Method::GET, "/v1/runs", None)
            .await
            .json()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    t.state.stop_scheduler().await;
}

#[tokio::test]
async fn parse_preview_and_dto_return_next_five_runs_across_dst() {
    let t = super::setup();
    let (bot, _) = make_chat(&t).await;

    let parsed = call(
        &t,
        Method::POST,
        "/v1/schedules/parse",
        Some(json!({"text": "every 10 minutes"})),
    )
    .await;
    assert_eq!(parsed.status, StatusCode::OK);
    let runs = parsed.json()["next_runs_ms"].as_array().unwrap().clone();
    assert_eq!(runs.len(), 5);
    assert_eq!(runs[0].as_i64(), parsed.json()["next_run_at_ms"].as_i64());

    // DST spring-forward in America/New_York: 2026-03-07 09:00 EST (14:00 UTC) -> 2026-03-08 09:00 EDT (13:00 UTC).
    let from_ms = chrono::NaiveDate::from_ymd_opt(2026, 3, 7)
        .unwrap()
        .and_hms_opt(12, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp_millis();
    let preview = call(
        &t,
        Method::POST,
        "/v1/schedules/preview",
        Some(json!({
            "spec": {"cron": "0 9 * * *"},
            "tz": "America/New_York",
            "from_ms": from_ms,
            "count": 5
        })),
    )
    .await;
    assert_eq!(preview.status, StatusCode::OK, "{:?}", preview.json());
    let p_runs: Vec<i64> = preview.json()["next_runs_ms"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_i64().unwrap())
        .collect();
    assert_eq!(p_runs.len(), 5);
    assert_eq!(p_runs[1] - p_runs[0], 23 * 3_600_000);
    assert_eq!(p_runs[2] - p_runs[1], 24 * 3_600_000);

    let created = create(&t, new_schedule(&bot, prompt("ping"))).await;
    assert_eq!(created["next_runs_ms"].as_array().unwrap().len(), 5);
    assert_eq!(created["next_runs_ms"][0], created["next_run_at_ms"]);
    let sid = created["id"].as_str().unwrap();
    let got = call(&t, Method::GET, &format!("/v1/schedules/{sid}"), None)
        .await
        .json();
    assert_eq!(got["next_runs_ms"].as_array().unwrap().len(), 5);
}

#[tokio::test]
async fn schedule_history_and_owner_only_idempotent_retry() {
    let t = super::setup();
    let clock = Arc::new(scheduler::FakeClock::new(storage::now_ms()));
    *t.state.0.scheduler_clock.lock() = Some(clock.clone());
    t.state.start_scheduler().unwrap();
    let (bot, _) = make_chat(&t).await;

    // 1. Create a schedule whose first timer occurrence needs approval and is cancelled -> `failed`.
    let mut body = new_schedule(
        &bot,
        json!({"kind": "tool", "name": "danger_run", "input": {}}),
    );
    body["spec"] = json!({"every": {"seconds": 60}});
    body["misfire_policy"] = json!("run_once");
    let created = create(&t, body).await;
    let sid = created["id"].as_str().unwrap().to_owned();
    let failed_due_ms = created["next_run_at_ms"].as_i64().unwrap();

    let mut rx = t.agent.subscribe();
    clock.advance_ms(61_000);
    let mut follow = Follow::new(&sid);
    let _call_id = follow.until_approval(&mut rx).await;
    let failed_run = follow.mine.expect("scheduled run started");
    let cancel = call(
        &t,
        Method::POST,
        &format!("/v1/runs/{failed_run}/cancel"),
        None,
    )
    .await;
    assert_eq!(cancel.status, StatusCode::OK, "{:?}", cancel.json());
    let (_, _) = follow.until_finished(&mut rx).await;
    settled(&t).await;

    let orig_next = call(&t, Method::GET, &format!("/v1/schedules/{sid}"), None)
        .await
        .json()["next_run_at_ms"]
        .as_i64()
        .unwrap();

    // 2. History includes the failed occurrence with its linked run_id and chat_id.
    let hist = call(
        &t,
        Method::GET,
        &format!("/v1/schedules/{sid}/history"),
        None,
    )
    .await;
    assert_eq!(hist.status, StatusCode::OK, "{:?}", hist.json());
    let items = hist.json().as_array().unwrap().clone();
    let failed_entry = items
        .iter()
        .find(|e| e["due_ms"] == failed_due_ms)
        .expect("failed occurrence in history");
    assert_eq!(failed_entry["status"], "failed");
    assert_eq!(failed_entry["run_id"], failed_run.to_string());
    assert_eq!(failed_entry["retryable"], true);
    assert!(failed_entry["started_at_ms"].as_i64().is_some());
    assert!(failed_entry["finished_at_ms"].as_i64().is_some());
    assert!(failed_entry["chat_id"].as_str().is_some());

    // 3. Owner-only enforcement: bearer token without owner key is refused.
    let retry_path = format!("/v1/schedules/{sid}/occurrences/{failed_due_ms}/retry");
    let unauth = super::call_token_only(&t, Method::POST, &retry_path, None).await;
    assert_eq!(unauth.status, StatusCode::FORBIDDEN);
    assert_eq!(unauth.json()["error"]["code"], "owner_key_required");

    // 4. Retry with owner key creates a normal run under the same permissions & approvals,
    // and preserves next_run_at_ms.
    let mut rx = t.agent.subscribe();
    let retried = call(&t, Method::POST, &retry_path, None).await;
    assert_eq!(retried.status, StatusCode::ACCEPTED, "{:?}", retried.json());
    let mut follow2 = Follow::new(&sid);
    let call_id2 = follow2.until_approval(&mut rx).await;
    let approve = call(
        &t,
        Method::POST,
        &format!("/v1/approvals/{call_id2}"),
        Some(json!({"decision": "approve"})),
    )
    .await;
    assert_eq!(approve.status, StatusCode::OK, "{:?}", approve.json());
    let (retry_run, _) = follow2.until_finished(&mut rx).await;
    settled(&t).await;

    let after = call(&t, Method::GET, &format!("/v1/schedules/{sid}"), None)
        .await
        .json();
    assert_eq!(after["next_run_at_ms"], orig_next);

    let hist2 = call(
        &t,
        Method::GET,
        &format!("/v1/schedules/{sid}/history"),
        None,
    )
    .await
    .json();
    let retried_entry = hist2
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["due_ms"] == failed_due_ms)
        .unwrap();
    assert_eq!(retried_entry["status"], "succeeded");
    assert_eq!(retried_entry["run_id"], retry_run.to_string());
    assert_eq!(retried_entry["retryable"], false);

    // 5. Idempotency: a second retry on the same occurrence refuses to double-fire.
    let again = call(&t, Method::POST, &retry_path, None).await;
    assert_eq!(again.status, StatusCode::CONFLICT);
    let total_runs = call(&t, Method::GET, "/v1/runs", None)
        .await
        .json()
        .as_array()
        .unwrap()
        .len();
    assert_eq!(total_runs, 2);

    t.state.stop_scheduler().await;
}

#[tokio::test]
async fn retry_after_restart_returns_409_while_the_first_run_still_waits_and_never_duplicates_it() {
    let t = super::setup();
    let clock = Arc::new(scheduler::FakeClock::new(storage::now_ms()));
    *t.state.0.scheduler_clock.lock() = Some(clock.clone());
    t.state.start_scheduler().unwrap();
    let (bot, _) = make_chat(&t).await;
    let mut body = new_schedule(
        &bot,
        json!({"kind": "tool", "name": "danger_run", "input": {}}),
    );
    body["spec"] = json!({"every": {"seconds": 60}});
    body["misfire_policy"] = json!("run_once");
    let created = create(&t, body).await;
    let sid = created["id"].as_str().unwrap().to_owned();
    let due_ms = created["next_run_at_ms"].as_i64().unwrap();

    // The timer fires; the run waits for an approval nobody has given.
    let mut rx = t.agent.subscribe();
    clock.advance_ms(61_000);
    let mut follow = Follow::new(&sid);
    let _call_id = follow.until_approval(&mut rx).await;
    let first_run = follow.mine.expect("scheduled run started");

    // Daemon restart: the scheduler starts again and reclassifies the
    // unresolved dispatch as `interrupted`.
    t.state.stop_scheduler().await;
    t.state.start_scheduler().unwrap();
    settled(&t).await;

    let retry_path = format!("/v1/schedules/{sid}/occurrences/{due_ms}/retry");
    let hist = call(
        &t,
        Method::GET,
        &format!("/v1/schedules/{sid}/history"),
        None,
    )
    .await
    .json();
    let entry = hist
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["due_ms"] == due_ms)
        .expect("occurrence in history")
        .clone();
    assert_eq!(
        entry["run_id"],
        first_run.to_string(),
        "existing run linked"
    );
    assert_eq!(entry["status"], "waiting_approval");
    assert_eq!(entry["retryable"], false);

    let refused = call(&t, Method::POST, &retry_path, None).await;
    assert_eq!(refused.status, StatusCode::CONFLICT, "{:?}", refused.json());
    let runs = call(&t, Method::GET, "/v1/runs", None).await.json();
    assert_eq!(
        runs.as_array().unwrap().len(),
        1,
        "no second run was started"
    );

    // Once that run ended failed/cancelled, a retry starts exactly one new run.
    let cancel = call(
        &t,
        Method::POST,
        &format!("/v1/runs/{first_run}/cancel"),
        None,
    )
    .await;
    assert_eq!(cancel.status, StatusCode::OK, "{:?}", cancel.json());
    let (_, _) = follow.until_finished(&mut rx).await;
    settled(&t).await;
    let retried = call(&t, Method::POST, &retry_path, None).await;
    assert_eq!(retried.status, StatusCode::ACCEPTED, "{:?}", retried.json());
    let mut follow2 = Follow::new(&sid);
    let _ = follow2.until_approval(&mut rx).await;
    let runs = call(&t, Method::GET, "/v1/runs", None).await.json();
    assert_eq!(runs.as_array().unwrap().len(), 2);

    t.state.stop_scheduler().await;
}
