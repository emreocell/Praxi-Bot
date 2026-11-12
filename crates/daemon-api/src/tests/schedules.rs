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
    T {
        app: router(state.clone()),
        state,
        agent,
        _dir: dir,
    }
}

fn save_workflow(t: &T) {
    let mut wf = Workflow::new("danger_wf");
    wf.steps = vec![Step::Tool(ToolStep {
        tool: "danger_run".to_owned(),
        input: json!({}),
    })];
    WorkflowStore::new(t.state.storage().data_dir())
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
    save_workflow(&t);
    let (bot, _) = make_chat(&t).await;

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
    assert_eq!(echoed.trim(), "echo: deploy said v1.2");
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
    assert!(t.state.scheduler().is_ok(), "serve started the scheduler");
    tx.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("serve returns")
        .unwrap()
        .unwrap();
    assert_eq!(t.state.shutdown_order(), ["scheduler", "agent"]);
    // A stopped scheduler does not tick any more.
    tokio::time::timeout(Duration::from_secs(5), t.state.scheduler().unwrap().tick())
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
