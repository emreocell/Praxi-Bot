#![allow(clippy::used_underscore_binding, clippy::many_single_char_names)]
//! Integration tests: the router is driven with `tower::ServiceExt` (no port),
//! `serve` is exercised on an ephemeral loopback port.

use std::sync::Arc;
use std::time::Duration;

use agent_core::{AgentLoop, LoopConfig, Providers};
use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use storage::Storage;
use tool_registry::{
    Effect, PermissionCategory, Platform, Tool, ToolContext, ToolError, ToolOutput, ToolRegistry,
    ToolSpec,
};
use tower::ServiceExt;

use crate::fake::FakeProvider;
use crate::{AppState, DaemonConfig, router};

mod schedules;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

const PROFILE: &str = r#"
[bot]
id = "tester"
name = "Tester"

[models]
executor = "fake/echo"
"#;

struct Danger;

#[async_trait]
impl Tool for Danger {
    fn spec(&self) -> &ToolSpec {
        static SPEC: std::sync::OnceLock<ToolSpec> = std::sync::OnceLock::new();
        SPEC.get_or_init(|| ToolSpec {
            name: "danger_run".to_owned(),
            description: "needs approval".to_owned(),
            input_schema: json!({"type": "object"}),
            effect: Effect::Write,
            category: PermissionCategory::Fs,
            platforms: vec![Platform::All],
        })
    }
    async fn call(&self, _: &ToolContext, _: Value) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::text("boom done"))
    }
}

struct T {
    app: Router,
    state: AppState,
    agent: AgentLoop,
    _dir: tempfile::TempDir,
}

fn setup() -> T {
    let dir = tempfile::tempdir().unwrap();
    let storage = Arc::new(Storage::open(dir.path()).unwrap());
    let tools = ToolRegistry::new();
    tools.register(Arc::new(Danger));
    tools.register(Arc::new(Ask));
    let agent = AgentLoop::new(
        Arc::clone(&storage),
        tools,
        Providers::single(Arc::new(FakeProvider)),
        LoopConfig::default(),
    );
    let state = AppState::new(agent.clone(), storage, DaemonConfig::new(TOKEN)).unwrap();
    T {
        app: router(state.clone()),
        state,
        agent,
        _dir: dir,
    }
}

struct Resp {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Resp {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

async fn send(
    app: &Router,
    method: Method,
    path: &str,
    body: Option<Value>,
    extra: &[(&str, &str)],
) -> Resp {
    let mut b = Request::builder().method(method).uri(path);
    let mut has_host = false;
    for (k, v) in extra {
        has_host |= k.eq_ignore_ascii_case("host");
        b = b.header(*k, *v);
    }
    if !has_host {
        b = b.header("host", "127.0.0.1:47821");
    }
    let req = match body {
        Some(v) => b
            .header("content-type", "application/json")
            .body(Body::from(v.to_string())),
        None => b.body(Body::empty()),
    }
    .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let (parts, body) = resp.into_parts();
    Resp {
        status: parts.status,
        headers: parts.headers,
        body: body.collect().await.unwrap().to_bytes().to_vec(),
    }
}

fn auth() -> (&'static str, String) {
    ("authorization", format!("Bearer {TOKEN}"))
}

async fn call(t: &T, method: Method, path: &str, body: Option<Value>) -> Resp {
    let (k, v) = auth();
    send(&t.app, method, path, body, &[(k, v.as_str())]).await
}

async fn make_chat(t: &T) -> (String, String) {
    let r = call(
        t,
        Method::POST,
        "/v1/bots",
        Some(json!({ "profile_toml": PROFILE })),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.json());
    let bot = r.json()["id"].as_str().unwrap().to_owned();
    let r = call(t, Method::POST, "/v1/chats", Some(json!({ "bot_id": bot }))).await;
    assert_eq!(r.status, StatusCode::CREATED);
    (bot, r.json()["id"].as_str().unwrap().to_owned())
}

async fn send_msg(t: &T, chat: &str, text: &str, mode: &str) -> Value {
    let r = call(
        t,
        Method::POST,
        &format!("/v1/chats/{chat}/messages"),
        Some(json!({ "text": text, "mode": mode })),
    )
    .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{:?}", r.json());
    r.json()
}

async fn wait_state(t: &T, run: &str, want: &str) {
    for _ in 0..200 {
        let r = call(t, Method::GET, &format!("/v1/runs/{run}"), None).await;
        if r.json()["state"] == want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("run {run} never reached {want}");
}

/// Parsed SSE frames: `(id, event, data)`.
fn frames(body: &[u8]) -> Vec<(String, String, Value)> {
    String::from_utf8_lossy(body)
        .split("\n\n")
        .filter_map(|block| {
            let (mut id, mut ev, mut data) = (None, None, None);
            for line in block.lines() {
                if let Some(v) = line.strip_prefix("id:") {
                    id = Some(v.trim().to_owned());
                } else if let Some(v) = line.strip_prefix("event:") {
                    ev = Some(v.trim().to_owned());
                } else if let Some(v) = line.strip_prefix("data:") {
                    data = serde_json::from_str(v.trim()).ok();
                }
            }
            Some((id.unwrap_or_default(), ev?, data?))
        })
        .collect()
}

async fn events(t: &T, query: &str, last_id: Option<&str>) -> Vec<(String, String, Value)> {
    let (k, v) = auth();
    let mut hdrs = vec![(k, v.as_str())];
    if let Some(l) = last_id {
        hdrs.push(("last-event-id", l));
    }
    let r = tokio::time::timeout(
        Duration::from_secs(10),
        send(
            &t.app,
            Method::GET,
            &format!("/v1/events{query}"),
            None,
            &hdrs,
        ),
    )
    .await
    .expect("scoped stream must end after run_finished");
    assert_eq!(r.status, StatusCode::OK);
    assert!(
        r.headers["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );
    frames(&r.body)
}

// ------------------------------------------------------------ guards

#[tokio::test]
async fn auth_required_and_health_is_public() {
    let t = setup();
    let r = send(&t.app, Method::GET, "/v1/bots", None, &[]).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(r.headers["www-authenticate"], "Bearer");
    assert_eq!(r.json()["error"]["code"], "unauthorized");
    for bad in ["Bearer nope", "Basic abc", TOKEN, ""] {
        let r = send(
            &t.app,
            Method::GET,
            "/v1/bots",
            None,
            &[("authorization", bad)],
        )
        .await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{bad}");
    }
    let r = call(&t, Method::GET, "/v1/bots", None).await;
    assert_eq!(r.status, StatusCode::OK);
    let r = send(&t.app, Method::GET, "/v1/health", None, &[]).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["status"], "ok");
    // Events and every other route are protected too.
    let r = send(&t.app, Method::GET, "/v1/events", None, &[]).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn host_header_must_be_loopback() {
    let t = setup();
    let (k, v) = auth();
    for host in [
        "evil.com",
        "localhost.evil.com",
        "192.168.1.2:47821",
        "[::2]",
    ] {
        let r = send(
            &t.app,
            Method::GET,
            "/v1/health",
            None,
            &[("host", host), (k, v.as_str())],
        )
        .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{host}");
    }
    for host in ["localhost", "localhost:1234", "127.0.0.1:9", "[::1]:80"] {
        let r = send(
            &t.app,
            Method::GET,
            "/v1/bots",
            None,
            &[("host", host), (k, v.as_str())],
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{host}");
    }
    // No Host header at all.
    let req = Request::builder()
        .uri("/v1/health")
        .body(Body::empty())
        .unwrap();
    let resp = t.app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn browser_origin_must_be_the_desktop_app() {
    let t = setup();
    let (k, v) = auth();
    let r = send(
        &t.app,
        Method::GET,
        "/v1/bots",
        None,
        &[("origin", "https://evil.com"), (k, v.as_str())],
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = send(
        &t.app,
        Method::GET,
        "/v1/bots",
        None,
        &[("origin", "null"), (k, v.as_str())],
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // The desktop app's origin passes and gets CORS headers.
    let r = send(
        &t.app,
        Method::GET,
        "/v1/bots",
        None,
        &[("origin", "tauri://localhost"), (k, v.as_str())],
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        r.headers["access-control-allow-origin"],
        "tauri://localhost"
    );
    // Preflight carries no credentials and still succeeds, but only for allowed origins.
    let r = send(
        &t.app,
        Method::OPTIONS,
        "/v1/bots",
        None,
        &[("origin", "http://tauri.localhost")],
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert!(
        r.headers["access-control-allow-headers"]
            .to_str()
            .unwrap()
            .contains("authorization")
    );
    let r = send(
        &t.app,
        Method::OPTIONS,
        "/v1/bots",
        None,
        &[("origin", "http://evil.com")],
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
}

// -------------------------------------------------------------- bots

#[tokio::test]
async fn bots_crud() {
    let t = setup();
    let r = call(&t, Method::POST, "/v1/bots", Some(json!({}))).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = call(
        &t,
        Method::POST,
        "/v1/bots",
        Some(json!({"config": {"bot": {"id": "x"}}})),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(r.json()["error"]["code"], "bad_profile");
    let r = call(&t, Method::POST, "/v1/bots", None).await;
    assert_eq!(r.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_eq!(r.json()["error"]["code"], "unsupported_media_type");

    let r = call(
        &t,
        Method::POST,
        "/v1/bots",
        Some(json!({"profile_toml": PROFILE, "name": "Custom"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let bot = r.json();
    assert_eq!(bot["name"], "Custom");
    let path = format!("/v1/bots/{}", bot["id"].as_str().unwrap());

    let r = call(&t, Method::GET, "/v1/bots", None).await;
    assert_eq!(r.json().as_array().unwrap().len(), 1);
    let r = call(&t, Method::GET, &path, None).await;
    assert_eq!(r.json()["config"]["models"]["executor"], "fake/echo");

    let r = call(
        &t,
        Method::PUT,
        &path,
        Some(json!({"config": {
            "bot": {"id": "tester", "name": "Renamed"},
            "models": {"executor": "fake/other"}
        }})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["name"], "Renamed");
    let r = call(&t, Method::GET, &path, None).await;
    assert_eq!(r.json()["config"]["models"]["executor"], "fake/other");

    assert_eq!(
        call(&t, Method::DELETE, &path, None).await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(&t, Method::GET, &path, None).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(&t, Method::DELETE, &path, None).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(&t, Method::GET, "/v1/bots/not-a-uuid", None)
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn bot_with_chats_cannot_be_deleted() {
    let t = setup();
    let (bot, _) = make_chat(&t).await;
    let r = call(&t, Method::DELETE, &format!("/v1/bots/{bot}"), None).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
}

// ---------------------------------------------------- messages / events

#[tokio::test]
async fn send_message_streams_events_in_order() {
    let t = setup();
    let (_, chat) = make_chat(&t).await;
    let sent = send_msg(&t, &chat, "hello brave new world", "queue").await;
    assert_eq!(sent["delivery"], "new_turn");
    let run = sent["run_id"].as_str().unwrap().to_owned();

    let fr = events(&t, &format!("?run_id={run}"), None).await;
    let kinds: Vec<&str> = fr.iter().map(|f| f.1.as_str()).collect();
    assert_eq!(kinds.first(), Some(&"run_started"));
    assert_eq!(kinds.last(), Some(&"run_finished"));
    assert!(kinds.iter().filter(|k| **k == "message_delta").count() >= 3);
    // seq runs 0..n without gaps; the SSE id mirrors it; data is an Envelope.
    for (i, (id, _, data)) in fr.iter().enumerate() {
        assert_eq!(id, &i.to_string());
        assert_eq!(data["seq"], i);
        assert_eq!(data["v"], 1);
        assert_eq!(data["event"]["run_id"], run.as_str());
    }
    let text: String = fr
        .iter()
        .filter(|f| f.1 == "message_delta")
        .map(|f| f.2["event"]["text"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(text, "echo: hello brave new world");

    wait_state(&t, &run, "succeeded").await;
    let msgs = call(&t, Method::GET, &format!("/v1/chats/{chat}/messages"), None)
        .await
        .json();
    let roles: Vec<&str> = msgs
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["role"].as_str().unwrap())
        .collect();
    assert_eq!(roles, ["user", "assistant"]);
    assert_eq!(msgs[1]["content"], "echo: hello brave new world");

    let usage = call(&t, Method::GET, "/v1/usage", None).await.json();
    assert_eq!(usage["runs"], 1);
    assert_eq!(usage["total"]["model_calls"], 1);
    assert_eq!(usage["total"]["prompt_tokens"], 10);
    assert_eq!(usage["by_bot"].as_array().unwrap().len(), 1);

    // Validation.
    let r = call(
        &t,
        Method::POST,
        &format!("/v1/chats/{chat}/messages"),
        Some(json!({"text": "  "})),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = call(
        &t,
        Method::POST,
        &format!("/v1/chats/{}/messages", uuid::Uuid::now_v7()),
        Some(json!({"text": "x"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn sse_resumes_from_last_event_id() {
    let t = setup();
    let (_, chat) = make_chat(&t).await;
    let run = send_msg(&t, &chat, "one two three four", "queue").await["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let all = events(&t, &format!("?run_id={run}"), None).await;
    assert!(all.len() > 4);

    // Per-run cursor: only events after seq 2 are replayed, in order.
    let resumed = events(&t, &format!("?run_id={run}"), Some("2")).await;
    let ids: Vec<String> = resumed.iter().map(|f| f.0.clone()).collect();
    let expected: Vec<String> = (3..all.len()).map(|i| i.to_string()).collect();
    assert_eq!(ids, expected);
    assert_eq!(resumed.last().unwrap().1, "run_finished");

    // Cursor past the end: nothing left to replay, the finished run just ends.
    let last = (all.len() - 1).to_string();
    let none = events(&t, &format!("?run_id={run}"), Some(&last)).await;
    assert_eq!(none.len(), 0);

    // Malformed cursor.
    let (k, v) = auth();
    let r = send(
        &t.app,
        Method::GET,
        &format!("/v1/events?run_id={run}"),
        None,
        &[(k, v.as_str()), ("last-event-id", "abc")],
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    // Unknown run.
    let r = call(
        &t,
        Method::GET,
        &format!("/v1/events?run_id={}", uuid::Uuid::now_v7()),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

/// Read SSE frames from a streaming body until `done(frames)` holds.
async fn read_until(
    body: &mut axum::body::Body,
    mut done: impl FnMut(&[(String, String, Value)]) -> bool,
) -> Vec<(String, String, Value)> {
    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let fr = frames(&buf);
            if done(&fr) {
                return fr;
            }
            let frame = body.frame().await.expect("stream ended").unwrap();
            if let Some(d) = frame.data_ref() {
                buf.extend_from_slice(d);
            }
        }
    })
    .await
    .expect("expected SSE frames did not arrive")
}

async fn open_global(t: &T, last_id: Option<&str>) -> axum::body::Body {
    let (k, v) = auth();
    let mut b = Request::builder()
        .uri("/v1/events")
        .header("host", "localhost")
        .header(k, v.as_str());
    if let Some(l) = last_id {
        b = b.header("last-event-id", l);
    }
    let resp = t
        .app
        .clone()
        .oneshot(b.body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    resp.into_body()
}

fn run_finished_of(fr: &[(String, String, Value)], run: &str) -> bool {
    fr.iter()
        .any(|f| f.1 == "run_finished" && f.2["event"]["run_id"] == run)
}

#[tokio::test]
async fn global_stream_resumes_across_all_runs_with_gseq() {
    let t = setup();
    let (_, chat) = make_chat(&t).await;
    let first = send_msg(&t, &chat, "a b", "queue").await["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    wait_state(&t, &first, "succeeded").await;
    let second = send_msg(&t, &chat, "c d", "queue").await["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    wait_state(&t, &second, "succeeded").await;

    let index = t.state.storage().list_event_index(0, 10_000).unwrap();
    assert!(index.len() > 6);
    assert!(
        index.iter().zip(1u64..).all(|(r, n)| r.gseq == n),
        "gapless daemon-wide numbering"
    );
    let total = index.len();

    // Cursor in the middle of run 1: the rest of run 1, then all of run 2,
    // ids are the daemon-wide gseq, strictly increasing without gaps.
    let cut = 3usize;
    let mut body = open_global(&t, Some(&cut.to_string())).await;
    let fr = read_until(&mut body, |fr| fr.len() >= total - cut).await;
    let ids: Vec<usize> = fr.iter().map(|f| f.0.parse().unwrap()).collect();
    assert_eq!(ids, (cut + 1..=total).collect::<Vec<_>>());
    let runs: Vec<&str> = fr
        .iter()
        .map(|f| f.2["event"]["run_id"].as_str().unwrap())
        .collect();
    assert_eq!(runs.first(), Some(&first.as_str()));
    assert_eq!(runs.last(), Some(&second.as_str()));
    assert!(run_finished_of(&fr, &first) && run_finished_of(&fr, &second));

    // The same stream continues live with the next gseq.
    let third = send_msg(&t, &chat, "e f", "queue").await["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let live = read_until(&mut body, |fr| run_finished_of(fr, &third)).await;
    let ids: Vec<usize> = live.iter().map(|f| f.0.parse().unwrap()).collect();
    let live_ids: Vec<usize> = ids.into_iter().filter(|i| *i > total).collect();
    assert_eq!(live_ids.first(), Some(&(total + 1)));
    assert!(live_ids.windows(2).all(|w| w[1] == w[0] + 1));
    drop(body);

    // A cursor at the end replays nothing; the stream just waits for new events.
    let end = t.state.storage().last_gseq().unwrap();
    let mut body = open_global(&t, Some(&end.to_string())).await;
    let fourth = send_msg(&t, &chat, "g h", "queue").await["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let fr = read_until(&mut body, |fr| run_finished_of(fr, &fourth)).await;
    assert_eq!(fr.first().unwrap().0, (end + 1).to_string());
    assert!(fr.iter().all(|f| f.2["event"]["run_id"] == fourth.as_str()));
    drop(body);

    // Without a cursor the stream is live only (no history).
    let before = t.state.storage().last_gseq().unwrap();
    let mut body = open_global(&t, None).await;
    let fifth = send_msg(&t, &chat, "i j", "queue").await["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let fr = read_until(&mut body, |fr| run_finished_of(fr, &fifth)).await;
    assert_eq!(fr.first().unwrap().0, (before + 1).to_string());
    drop(body);

    // A cursor from the future (e.g. a reset database) is clamped, and the
    // pre-gseq `<run>:<seq>` form is rejected.
    let mut body = open_global(&t, Some("999999999")).await;
    let sixth = send_msg(&t, &chat, "k l", "queue").await["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let fr = read_until(&mut body, |fr| run_finished_of(fr, &sixth)).await;
    assert!(fr.iter().all(|f| f.2["event"]["run_id"] == sixth.as_str()));
    drop(body);
    let (k, v) = auth();
    let r = send(
        &t.app,
        Method::GET,
        "/v1/events",
        None,
        &[(k, v.as_str()), ("last-event-id", &format!("{first}:1"))],
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

// --------------------------------------------------------- approvals

async fn pending_call(t: &T) -> (String, String) {
    for _ in 0..200 {
        let r = call(t, Method::GET, "/v1/approvals", None).await;
        if let Some(a) = r.json().as_array().and_then(|a| a.first().cloned()) {
            return (
                a["call_id"].as_str().unwrap().to_owned(),
                a["run_id"].as_str().unwrap().to_owned(),
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("no pending approval");
}

#[tokio::test]
async fn approval_flow_approve() {
    let t = setup();
    let (_, chat) = make_chat(&t).await;
    let run = send_msg(&t, &chat, "call:danger_run {}", "queue").await["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (call_id, run_of_call) = pending_call(&t).await;
    assert_eq!(run, run_of_call);
    wait_state(&t, &run, "waiting_approval").await;

    // While waiting: queue vs steer on a busy chat.
    let q = send_msg(&t, &chat, "later", "queue").await;
    assert_eq!(q["delivery"], "queued");
    assert!(q["run_id"].is_null());
    let s = send_msg(&t, &chat, "now", "steer").await;
    assert_eq!(s["delivery"], "steered");
    assert_eq!(s["run_id"], run.as_str());

    let r = call(
        &t,
        Method::POST,
        &format!("/v1/approvals/{call_id}"),
        Some(json!({"decision": "approve", "scope": "once"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    assert_eq!(r.json()["decision"], "approve");
    // Deciding twice: nothing pending anymore.
    let r = call(
        &t,
        Method::POST,
        &format!("/v1/approvals/{call_id}"),
        Some(json!({"decision": "deny"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    let fr = events(&t, &format!("?run_id={run}"), None).await;
    let kinds: Vec<&str> = fr.iter().map(|f| f.1.as_str()).collect();
    let pos = |k: &str| kinds.iter().position(|x| *x == k).unwrap();
    assert!(pos("approval_needed") < pos("tool_call_approved"));
    assert!(pos("tool_call_approved") < pos("tool_call_completed"));
    assert_eq!(kinds.last(), Some(&"run_finished"));
    assert_eq!(
        fr[pos("approval_needed")].2["event"]["tool_name"],
        "danger_run"
    );

    let all = call(
        &t,
        Method::GET,
        &format!("/v1/approvals?run_id={run}"),
        None,
    )
    .await
    .json();
    assert_eq!(all[0]["decision"], "approved");
    assert_eq!(
        call(&t, Method::GET, "/v1/approvals", None).await.json(),
        json!([])
    );
}

#[tokio::test]
async fn approval_flow_deny_and_unknown() {
    let t = setup();
    let (_, chat) = make_chat(&t).await;
    let run = send_msg(&t, &chat, "call:danger_run {}", "queue").await["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (call_id, _) = pending_call(&t).await;
    let r = call(
        &t,
        Method::POST,
        &format!("/v1/approvals/{call_id}"),
        Some(json!({"decision": "deny", "reason": "no thanks"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let fr = events(&t, &format!("?run_id={run}"), None).await;
    assert!(fr.iter().any(|f| f.1 == "tool_call_denied"));
    assert!(!fr.iter().any(|f| f.1 == "tool_call_approved"));

    for bad in [uuid::Uuid::now_v7().to_string()] {
        let r = call(
            &t,
            Method::POST,
            &format!("/v1/approvals/{bad}"),
            Some(json!({"decision": "approve"})),
        )
        .await;
        assert_eq!(r.status, StatusCode::NOT_FOUND);
    }
    let r = call(
        &t,
        Method::POST,
        &format!("/v1/approvals/{call_id}"),
        Some(json!({"decision": "maybe"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

// ------------------------------------------------------ runs / shutdown

#[tokio::test]
async fn cancel_and_resume_rules() {
    let t = setup();
    let (_, chat) = make_chat(&t).await;
    let run = send_msg(&t, &chat, "call:danger_run {}", "queue").await["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    pending_call(&t).await;

    // Resume is only for paused runs.
    let r = call(&t, Method::POST, &format!("/v1/runs/{run}/resume"), None).await;
    assert_eq!(r.status, StatusCode::CONFLICT);

    let r = call(&t, Method::POST, &format!("/v1/runs/{run}/cancel"), None).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    wait_state(&t, &run, "cancelled").await;
    let r = call(&t, Method::POST, &format!("/v1/runs/{run}/cancel"), None).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(
        call(&t, Method::GET, "/v1/runs", None).await.json()[0]["id"],
        run.as_str()
    );
    let r = call(
        &t,
        Method::POST,
        &format!("/v1/runs/{}/cancel", uuid::Uuid::now_v7()),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn remember_is_refused_for_runs_without_a_route_and_unknown_runs() {
    let t = setup();
    let (_, chat) = make_chat(&t).await;
    let run = send_msg(&t, &chat, "hello", "queue").await["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    wait_state(&t, &run, "succeeded").await;
    let r = call(&t, Method::GET, &format!("/v1/runs/{run}"), None).await;
    assert_eq!(r.json()["can_remember"], false, "{:?}", r.json());
    let r = call(&t, Method::GET, "/v1/runs", None).await;
    assert_eq!(r.json()[0]["can_remember"], false);
    let r = call(&t, Method::POST, &format!("/v1/runs/{run}/remember"), None).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{:?}", r.json());
    let r = call(
        &t,
        Method::POST,
        &format!("/v1/runs/{}/remember", uuid::Uuid::now_v7()),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn create_chat_returns_chat_id_next_to_id() {
    let t = setup();
    let (bot, chat) = make_chat(&t).await;
    let r = call(
        &t,
        Method::POST,
        "/v1/chats",
        Some(json!({ "bot_id": bot })),
    )
    .await;
    let v = r.json();
    assert_eq!(v["chat_id"], v["id"]);
    assert_ne!(v["chat_id"].as_str().unwrap(), chat);
}

struct FakeChannel {
    running: bool,
}

impl crate::ChannelControl for FakeChannel {
    fn info(&self) -> crate::dto::ChannelDto {
        crate::dto::ChannelDto {
            id: "telegram".into(),
            configured: true,
            enabled: true,
            running: self.running,
            paired_users: 2,
        }
    }
    fn create_pair_code(&self) -> Result<crate::dto::PairCodeDto, String> {
        if self.running {
            Ok(crate::dto::PairCodeDto {
                code: "123456".into(),
                expires_in_secs: 600,
            })
        } else {
            Err("the telegram channel is not running".into())
        }
    }
}

#[tokio::test]
async fn channels_and_pair_code_need_auth_and_a_running_channel() {
    let t = setup();
    // No channel registered: empty list, pair-code is 404.
    let r = call(&t, Method::GET, "/v1/channels", None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json(), json!([]));
    let r = call(&t, Method::POST, "/v1/channels/telegram/pair-code", None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    t.state.add_channel(Arc::new(FakeChannel { running: true }));
    for (m, p) in [
        (Method::GET, "/v1/channels"),
        (Method::POST, "/v1/channels/telegram/pair-code"),
    ] {
        let r = send(&t.app, m, p, None, &[]).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{p}");
    }
    let r = call(&t, Method::GET, "/v1/channels", None).await;
    assert_eq!(r.json()[0]["running"], true);
    assert_eq!(r.json()[0]["paired_users"], 2);
    let r = call(&t, Method::POST, "/v1/channels/telegram/pair-code", None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["code"], "123456");
    assert_eq!(r.json()["expires_in_secs"], 600);

    let t2 = setup();
    t2.state
        .add_channel(Arc::new(FakeChannel { running: false }));
    let r = call(&t2, Method::POST, "/v1/channels/telegram/pair-code", None).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn graceful_shutdown_checkpoints_running_runs_and_closes_streams() {
    let t = setup();
    let (_, chat) = make_chat(&t).await;
    let run = send_msg(&t, &chat, "call:danger_run {}", "queue").await["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    wait_state(&t, &run, "waiting_approval").await;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    assert!(listener.local_addr().unwrap().ip().is_loopback());
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(crate::serve(listener, t.state.clone(), async move {
        let _ = rx.await;
    }));

    // An open SSE stream (through the router) must not block shutdown.
    let (k, v) = auth();
    let req = Request::builder()
        .uri("/v1/events")
        .header("host", "localhost")
        .header(k, v.as_str())
        .body(Body::empty())
        .unwrap();
    let resp = t.app.clone().oneshot(req).await.unwrap();
    let mut body = resp.into_body();

    tx.send(()).unwrap();
    let paused = tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("serve returns")
        .unwrap()
        .unwrap();
    assert_eq!(paused.len(), 1);
    assert_eq!(paused[0].to_string(), run);
    let state = t.agent.runtime().state(paused[0]).unwrap();
    assert_eq!(state.as_str(), "paused");
    // The daemon refuses new turns once it is shutting down.
    let (_, chat2) = make_chat(&t).await;
    let r = call(
        &t,
        Method::POST,
        &format!("/v1/chats/{chat2}/messages"),
        Some(json!({"text": "late"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{:?}", r.json());
    assert_eq!(r.json()["error"]["code"], "unavailable");
    // The stream ends instead of hanging.
    let end = tokio::time::timeout(Duration::from_secs(5), async {
        while body.frame().await.is_some() {}
    })
    .await;
    assert!(end.is_ok());
}

// ------------------------------------------- questions, grants, policy

struct Ask;

#[async_trait]
impl Tool for Ask {
    fn spec(&self) -> &ToolSpec {
        static SPEC: std::sync::OnceLock<ToolSpec> = std::sync::OnceLock::new();
        SPEC.get_or_init(|| ToolSpec {
            name: "ask_user".to_owned(),
            description: "question card".to_owned(),
            input_schema: json!({"type": "object"}),
            effect: Effect::Read,
            category: PermissionCategory::Meta,
            platforms: vec![Platform::All],
        })
    }
    async fn call(&self, _: &ToolContext, input: Value) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput {
            content: vec![tool_registry::ContentPart::Json {
                data: json!({
                    "kind": "question_card",
                    "question": input["question"],
                    "options": input["options"],
                }),
            }],
            spill: None,
        })
    }
}

/// The `call_id` of the run's newest `question_asked` event.
async fn question_call(t: &T, run: &str) -> String {
    let id: uuid::Uuid = run.parse().unwrap();
    for _ in 0..200 {
        let log = storage::read_event_log(t.state.storage().data_dir(), protocol::RunId(id))
            .unwrap_or_default();
        for env in log.iter().rev() {
            if let protocol::Event::QuestionAsked(q) = &env.event {
                return q.call_id.to_string();
            }
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("no question was asked");
}

#[tokio::test]
async fn question_card_is_answered_through_the_api() {
    let t = setup();
    let (_, chat) = make_chat(&t).await;
    let run = send_msg(
        &t,
        &chat,
        r#"call:ask_user {"question":"Which colour?","options":["red","blue"]}"#,
        "queue",
    )
    .await["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let call_id = question_call(&t, &run).await;
    wait_state(&t, &run, "waiting_approval").await;

    // Bad ids and bodies.
    let r = call(
        &t,
        Method::POST,
        "/v1/questions/not-a-uuid",
        Some(json!({"answer": "x"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = call(
        &t,
        Method::POST,
        &format!("/v1/questions/{call_id}"),
        Some(json!({})),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    // Unauthenticated.
    let r = send(
        &t.app,
        Method::POST,
        &format!("/v1/questions/{call_id}"),
        Some(json!({"answer": "blue"})),
        &[],
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);

    let r = call(
        &t,
        Method::POST,
        &format!("/v1/questions/{call_id}"),
        Some(json!({"answer": "blue"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    assert_eq!(r.json()["call_id"], call_id.as_str());
    wait_state(&t, &run, "succeeded").await;
    let msgs = call(&t, Method::GET, &format!("/v1/chats/{chat}/messages"), None)
        .await
        .json();
    let last = msgs.as_array().unwrap().last().unwrap();
    assert!(
        last["content"]
            .as_str()
            .unwrap()
            .contains("The user answered: blue"),
        "{last}"
    );
    // Answering twice: nothing is waiting any more.
    let r = call(
        &t,
        Method::POST,
        &format!("/v1/questions/{call_id}"),
        Some(json!({"answer": "red"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn grants_are_listed_revoked_and_shown_in_the_effective_policy() {
    let t = setup();
    let (bot, chat) = make_chat(&t).await;
    let bot_path = format!("/v1/bots/{bot}");

    let eff = call(&t, Method::GET, &bot_path, None).await.json()["effective_policy"].clone();
    assert!(eff["policy"].is_null());
    assert_eq!(eff["categories"]["fs"]["decision"], "ask");
    assert_eq!(eff["categories"]["fs"]["via_grant"], false);
    assert_eq!(eff["grants"], json!([]));
    assert!(eff["categories"].get("meta").is_none());
    // The list endpoint does not carry it.
    let list = call(&t, Method::GET, "/v1/bots", None).await.json();
    assert!(list[0].get("effective_policy").is_none());

    // Approve with the new `always_category` scope.
    let run = send_msg(&t, &chat, "call:danger_run {}", "queue").await["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (call_id, _) = pending_call(&t).await;
    let r = call(
        &t,
        Method::POST,
        &format!("/v1/approvals/{call_id}"),
        Some(json!({"decision": "approve", "scope": "always_category"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    wait_state(&t, &run, "succeeded").await;
    let r = call(
        &t,
        Method::POST,
        &format!("/v1/approvals/{call_id}"),
        Some(json!({"decision": "approve", "scope": "bogus"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    let grants = call(&t, Method::GET, &format!("{bot_path}/grants"), None).await;
    assert_eq!(grants.status, StatusCode::OK);
    let g = grants.json();
    assert_eq!(g.as_array().unwrap().len(), 1);
    assert_eq!(g[0]["target"], json!({"kind": "category", "name": "fs"}));
    assert!(g[0]["granted_at_ms"].as_i64().unwrap() > 0);

    // The effective policy now tells the truth.
    let eff = call(&t, Method::GET, &bot_path, None).await.json()["effective_policy"].clone();
    assert_eq!(eff["categories"]["fs"]["decision"], "allow");
    assert_eq!(eff["categories"]["fs"]["via_grant"], true);
    assert_eq!(eff["categories"]["shell"]["decision"], "ask");
    assert_eq!(eff["grants"], g);
    // ... and a second tool call of that category needs no approval.
    let run2 = send_msg(&t, &chat, "call:danger_run {}", "queue").await["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    wait_state(&t, &run2, "succeeded").await;

    // Revoke: validation, success, repeat.
    let path = format!("{bot_path}/grants");
    for bad in [
        json!({"target": {"kind": "category", "name": "nonsense"}}),
        json!({"target": {"kind": "category", "name": "meta"}}),
        json!({"target": {"kind": "tool", "name": " "}}),
    ] {
        let r = call(&t, Method::DELETE, &path, Some(bad)).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{:?}", r.json());
    }
    let r = call(&t, Method::DELETE, &path, Some(json!({"target": 1}))).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = call(
        &t,
        Method::DELETE,
        &path,
        Some(json!({"target": {"kind": "tool", "name": "danger_run"}})),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "no such tool grant");
    let rev = json!({"target": {"kind": "category", "name": "fs"}});
    let r = call(&t, Method::DELETE, &path, Some(rev.clone())).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r = call(&t, Method::DELETE, &path, Some(rev)).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(call(&t, Method::GET, &path, None).await.json(), json!([]));
    let eff = call(&t, Method::GET, &bot_path, None).await.json()["effective_policy"].clone();
    assert_eq!(eff["categories"]["fs"]["decision"], "ask");

    // A tool-level grant via `always`, and unknown bots.
    let run3 = send_msg(&t, &chat, "call:danger_run {}", "queue").await["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (call3, _) = pending_call(&t).await;
    call(
        &t,
        Method::POST,
        &format!("/v1/approvals/{call3}"),
        Some(json!({"decision": "approve", "scope": "always"})),
    )
    .await;
    wait_state(&t, &run3, "succeeded").await;
    let g = call(&t, Method::GET, &path, None).await.json();
    assert_eq!(
        g[0]["target"],
        json!({"kind": "tool", "name": "danger_run"})
    );
    let eff = call(&t, Method::GET, &bot_path, None).await.json()["effective_policy"].clone();
    assert_eq!(
        eff["categories"]["fs"]["decision"], "ask",
        "a tool grant does not open the category"
    );
    assert_eq!(eff["grants"], g);
    let unknown = format!("/v1/bots/{}/grants", uuid::Uuid::now_v7());
    assert_eq!(
        call(&t, Method::GET, &unknown, None).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(
            &t,
            Method::DELETE,
            &unknown,
            Some(json!({"target": {"kind": "tool", "name": "x"}}))
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
}

// ------------------------------------------------------ SSE token auth

#[tokio::test]
async fn events_route_accepts_the_token_in_the_query_only_there() {
    let t = setup();
    let (_, chat) = make_chat(&t).await;
    let run = send_msg(&t, &chat, "x y", "queue").await["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    wait_state(&t, &run, "succeeded").await;

    let url = format!("/v1/events?run_id={run}&token={TOKEN}");
    let r = send(&t.app, Method::GET, &url, None, &[]).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_ne!(frames(&r.body).len(), 0);
    // Also with the desktop app's browser origin.
    let r = send(
        &t.app,
        Method::GET,
        &url,
        None,
        &[("origin", "tauri://localhost")],
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        r.headers["access-control-allow-origin"],
        "tauri://localhost"
    );

    // Wrong / empty / near-miss tokens.
    let wrong = format!("{}0", &TOKEN[..63]);
    for bad in [wrong.as_str(), "", "short", TOKEN.to_uppercase().as_str()] {
        let r = send(
            &t.app,
            Method::GET,
            &format!("/v1/events?run_id={run}&token={bad}"),
            None,
            &[],
        )
        .await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "token {bad:?}");
    }

    // Host / Origin checks still come first.
    let r = send(&t.app, Method::GET, &url, None, &[("host", "evil.com")]).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = send(
        &t.app,
        Method::GET,
        &url,
        None,
        &[("origin", "https://evil.com")],
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);

    // An Authorization header, when present, is the one that counts.
    let r = send(
        &t.app,
        Method::GET,
        &url,
        None,
        &[("authorization", "Bearer wrong")],
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);

    // Every other route (and method) ignores `?token=`.
    for path in [
        format!("/v1/bots?token={TOKEN}"),
        format!("/v1/runs?token={TOKEN}"),
        format!("/v1/approvals?token={TOKEN}"),
        format!("/v1/events/extra?token={TOKEN}"),
    ] {
        let r = send(&t.app, Method::GET, &path, None, &[]).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{path}");
    }
    let r = send(
        &t.app,
        Method::POST,
        &format!("/v1/events?token={TOKEN}"),
        None,
        &[],
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

// ------------------------------------------------------------ openapi

#[test]
fn openapi_snapshot() {
    let json = crate::openapi_json();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/api/openapi.json");
    if std::env::var_os("UPDATE_OPENAPI").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &json).unwrap();
    }
    let on_disk = std::fs::read_to_string(&path)
        .expect("docs/api/openapi.json missing: run with UPDATE_OPENAPI=1");
    assert_eq!(
        on_disk.replace("\r\n", "\n"),
        json,
        "openapi snapshot is stale: UPDATE_OPENAPI=1 cargo test -p daemon-api openapi_snapshot"
    );
    let v: Value = serde_json::from_str(&json).unwrap();
    for p in [
        "/v1/health",
        "/v1/bots",
        "/v1/events",
        "/v1/runs/{id}/resume",
        "/v1/runs/{id}/remember",
        "/v1/usage",
        "/v1/questions/{call_id}",
        "/v1/bots/{id}/grants",
        "/v1/control/state",
        "/v1/control/{request_id}",
        "/v1/policy",
        "/v1/providers",
        "/v1/providers/reload",
    ] {
        assert!(v["paths"].get(p).is_some(), "{p}");
    }
}

// ------------------------------------------------------- control bridge

/// A daemon whose bot has the (fake) desktop tools and a full policy, so a
/// scripted click needs no approval.
fn setup_desktop() -> (T, Arc<crate::fake::FakeDesktop>) {
    let dir = tempfile::tempdir().unwrap();
    let storage = Arc::new(Storage::open(dir.path()).unwrap());
    let desktop = Arc::new(crate::fake::FakeDesktop::default());
    let mut bt = builtin_tools::BuiltinConfig::new(
        dir.path().join("bt"),
        Arc::new(builtin_tools::ResolverGate::new(
            permission_engine::PermissionResolver::new(),
            permission_engine::Policy::full(),
            None,
            protocol::InitiatorKind::Human,
        )),
    );
    bt.desktop = Some(desktop.clone());
    bt.desktop_settings.action_settle = Duration::ZERO;
    let agent = AgentLoop::new(
        Arc::clone(&storage),
        ToolRegistry::new(),
        Providers::single(Arc::new(FakeProvider)),
        LoopConfig {
            global_policy: permission_engine::Policy::full(),
            discovery_threshold: 1000,
            ..LoopConfig::default()
        },
    )
    .with_builtin_tools(bt);
    let mut cfg = DaemonConfig::new(TOKEN);
    cfg.control = crate::ControlConfig {
        proceed_timeout: Duration::from_millis(150),
        refuse_timeout: Duration::from_millis(600),
        heartbeat_ttl: Duration::from_secs(5),
    };
    let state = AppState::new(agent.clone(), storage, cfg).unwrap();
    (
        T {
            app: router(state.clone()),
            state,
            agent,
            _dir: dir,
        },
        desktop,
    )
}

/// Send `text`, wait for the run and return the assistant's reply (the fake
/// provider echoes the tool result as `tool said: ...`).
async fn run_and_reply(t: &T, chat: &str, text: &str) -> String {
    run_and_reply_seen(t, chat, text).await.0
}

/// [`run_and_reply`] plus every event of the run.  Waits on the loop's event
/// stream (`run_finished`), not on the clock: the only timeout is a generous
/// failure bound.
async fn run_and_reply_seen(t: &T, chat: &str, text: &str) -> (String, Vec<protocol::Event>) {
    let mut rx = t.agent.subscribe();
    let run = send_msg(t, chat, text, "queue").await["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut events = Vec::new();
    loop {
        let item = tokio::time::timeout(Duration::from_secs(60), rx.recv())
            .await
            .expect("run never finished")
            .expect("stream closed");
        let ev = item.envelope.event;
        let finished =
            matches!(&ev, protocol::Event::RunFinished(f) if f.run_id.to_string() == run);
        let mine = match &ev {
            protocol::Event::RunStarted(r) => r.run_id.to_string() == run,
            _ => true,
        };
        if mine {
            events.push(ev);
        }
        if finished {
            break;
        }
    }
    wait_state(t, &run, "succeeded").await;
    let msgs = call(t, Method::GET, &format!("/v1/chats/{chat}/messages"), None)
        .await
        .json();
    let reply = msgs
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|m| m["role"] == "assistant")
        .unwrap()["content"]
        .as_str()
        .unwrap()
        .to_owned();
    (reply, events)
}

#[tokio::test]
async fn control_routes_validate_and_report_the_registration() {
    let t = setup();
    let r = call(&t, Method::GET, "/v1/control/state", None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.json()["mode"].is_null());
    assert_eq!(r.json()["expires_in_ms"], 0);

    let r = call(
        &t,
        Method::PUT,
        "/v1/control/state",
        Some(json!({"mode": "human_driving"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    assert_eq!(r.json()["mode"], "human_driving");
    assert!(r.json()["expires_in_ms"].as_u64().unwrap() > 10_000);
    let r = call(&t, Method::GET, "/v1/control/state", None).await;
    assert_eq!(r.json()["mode"], "human_driving");

    let r = call(
        &t,
        Method::PUT,
        "/v1/control/state",
        Some(json!({"mode": "sideways"})),
    )
    .await;
    assert!(r.status.is_client_error());
    // Unknown / malformed request ids.
    let r = call(
        &t,
        Method::POST,
        &format!("/v1/control/{}", uuid::Uuid::now_v7()),
        Some(json!({"decision": "proceed"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = call(
        &t,
        Method::POST,
        "/v1/control/not-a-uuid",
        Some(json!({"decision": "proceed"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    // Needs the bearer token like everything else.
    let r = send(&t.app, Method::GET, "/v1/control/state", None, &[]).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn human_driving_refuses_the_next_bot_click_with_a_clear_message() {
    let (t, desktop) = setup_desktop();
    let (_, chat) = make_chat(&t).await;
    let r = run_and_reply(&t, &chat, "call:desktop_get_state {}").await;
    assert!(r.contains("Button 2"), "{r}");

    // Nobody registered: the click just runs.
    let r = run_and_reply(&t, &chat, r#"call:desktop_click {"index": 2}"#).await;
    assert!(r.contains("dispatched"), "{r}");
    assert_eq!(desktop.actions.load(std::sync::atomic::Ordering::SeqCst), 1);

    // The human takes the wheel: refused at once, nothing is dispatched.
    call(
        &t,
        Method::PUT,
        "/v1/control/state",
        Some(json!({"mode": "human_driving"})),
    )
    .await;
    let r = run_and_reply(&t, &chat, "call:desktop_get_state {}").await;
    assert!(r.contains("Button 2"), "reads are not gated: {r}");
    let (r, events) = run_and_reply_seen(&t, &chat, r#"call:desktop_click {"index": 3}"#).await;
    assert!(r.contains("taken control of the desktop"), "{r}");
    // "At once" means no round trip to the app (which would have emitted a
    // `control_requested` and waited for its answer), not a wall-clock bound.
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, protocol::Event::ControlRequested(_))),
        "no round trip while the human drives: {events:?}"
    );
    assert_eq!(desktop.actions.load(std::sync::atomic::Ordering::SeqCst), 1);

    // Handing the wheel back lets the bot continue (an idle registration that
    // never answers would time out, so report bot_driving only after the app
    // answers; here nobody listens on SSE and the registration says idle).
    call(
        &t,
        Method::PUT,
        "/v1/control/state",
        Some(json!({"mode": "idle"})),
    )
    .await;
    // Registered but silent: refused after the long timeout (600 ms here).
    let r = run_and_reply(&t, &chat, "call:desktop_get_state {}").await;
    assert!(r.contains("Button"), "{r}");
    let r = run_and_reply(&t, &chat, r#"call:desktop_click {"index": 3}"#).await;
    assert!(r.contains("did not confirm"), "{r}");
    assert_eq!(desktop.actions.load(std::sync::atomic::Ordering::SeqCst), 1);
}

/// Answer every `control_requested` event of the loop with `decision`; returns
/// the events seen.
fn auto_answer(
    t: &T,
    decision: &'static str,
    reason: Option<&'static str>,
) -> Arc<parking_lot::Mutex<Vec<Value>>> {
    let seen = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let mut rx = t.agent.subscribe();
    let app = t.app.clone();
    let log = Arc::clone(&seen);
    tokio::spawn(async move {
        while let Ok(item) = rx.recv().await {
            if let protocol::Event::ControlRequested(c) = item.envelope.event {
                log.lock().push(serde_json::to_value(&c).unwrap());
                let (k, v) = auth();
                let r = send(
                    &app,
                    Method::POST,
                    &format!("/v1/control/{}", c.request_id),
                    Some(json!({"decision": decision, "reason": reason})),
                    &[(k, v.as_str())],
                )
                .await;
                assert_eq!(r.status, StatusCode::OK);
            }
        }
    });
    seen
}

#[tokio::test]
async fn a_registered_app_confirms_each_click_before_it_runs() {
    let (t, desktop) = setup_desktop();
    let (_, chat) = make_chat(&t).await;
    call(
        &t,
        Method::PUT,
        "/v1/control/state",
        Some(json!({"mode": "bot_driving"})),
    )
    .await;
    let seen = auto_answer(&t, "proceed", None);
    run_and_reply(&t, &chat, "call:desktop_get_state {}").await;
    let r = run_and_reply(&t, &chat, r#"call:desktop_click {"index": 2}"#).await;
    assert!(r.contains("dispatched"), "{r}");
    assert_eq!(desktop.actions.load(std::sync::atomic::Ordering::SeqCst), 1);

    let seen = seen.lock().clone();
    assert_eq!(seen.len(), 1, "the read was not gated, the click was");
    assert_eq!(seen[0]["action"], "click");
    assert_eq!(seen[0]["label"], "button \"Button 2\"");
    assert_eq!(seen[0]["rect"]["x"], 600.0);
    assert_eq!(seen[0]["rect"]["width"], 110.0);
}

#[tokio::test]
async fn an_app_refusal_reaches_the_model_and_nothing_is_dispatched() {
    let (t, desktop) = setup_desktop();
    let (_, chat) = make_chat(&t).await;
    call(
        &t,
        Method::PUT,
        "/v1/control/state",
        Some(json!({"mode": "bot_driving"})),
    )
    .await;
    let _seen = auto_answer(&t, "refuse", Some("the user pressed Stop"));
    run_and_reply(&t, &chat, "call:desktop_get_state {}").await;
    let r = run_and_reply(&t, &chat, r#"call:desktop_click {"index": 1}"#).await;
    assert!(r.contains("the user pressed Stop"), "{r}");
    assert_eq!(desktop.actions.load(std::sync::atomic::Ordering::SeqCst), 0);
}

// ---------------------------------------------------------------- policy

#[tokio::test]
async fn global_policy_is_editable_and_persisted() {
    let t = setup();
    let p = call(&t, Method::GET, "/v1/policy", None).await.json();
    assert_eq!(p["preset"], "approve-destructive");
    assert_eq!(p["categories"]["fs"], "ask");
    assert!(p["categories"].get("meta").is_none());

    let r = call(
        &t,
        Method::PUT,
        "/v1/policy",
        Some(json!({"preset": "read-and-draft", "categories": {"fs": "allow", "money": "deny"}})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    let p = r.json();
    assert!(p["preset"].is_null(), "no longer a preset: {p}");
    assert_eq!(p["categories"]["fs"], "allow");
    assert_eq!(p["categories"]["money"], "deny");
    assert_eq!(p["categories"]["shell"], "ask");
    assert_eq!(call(&t, Method::GET, "/v1/policy", None).await.json(), p);
    // It is live in the loop and survives a restart (same data dir).
    assert!(
        t.agent
            .global_policy()
            .categories
            .contains_key(&protocol::PermissionCategory::Fs)
    );
    let reborn = AppState::new(
        AgentLoop::new(
            Arc::new(Storage::open(t._dir.path()).unwrap()),
            ToolRegistry::new(),
            Providers::single(Arc::new(FakeProvider)),
            LoopConfig::default(),
        ),
        Arc::new(Storage::open(t._dir.path()).unwrap()),
        DaemonConfig::new(TOKEN),
    );
    // A second handle on the same sqlite file is fine for this read-only check.
    if let Ok(reborn) = reborn {
        let app = router(reborn);
        let (k, v) = auth();
        let r = send(&app, Method::GET, "/v1/policy", None, &[(k, v.as_str())]).await;
        assert_eq!(r.json(), p, "persisted policy is loaded at start");
    }

    // Only a preset.
    let r = call(
        &t,
        Method::PUT,
        "/v1/policy",
        Some(json!({"preset": "full"})),
    )
    .await;
    assert_eq!(r.json()["preset"], "full");
    assert_eq!(r.json()["categories"]["money"], "allow");

    for bad in [
        json!({"preset": "yolo"}),
        json!({"categories": {"nope": "allow"}}),
        json!({"categories": {"meta": "deny"}}),
        json!({"categories": {"fs": "maybe"}}),
    ] {
        let r = call(&t, Method::PUT, "/v1/policy", Some(bad.clone())).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}");
    }
    // A rejected change leaves the policy alone.
    assert_eq!(
        call(&t, Method::GET, "/v1/policy", None).await.json()["preset"],
        "full"
    );
}

#[tokio::test]
async fn the_persisted_policy_changes_what_asks() {
    let t = setup();
    let (_, chat) = make_chat(&t).await;
    // Default: danger_run (fs write) asks.
    let run = send_msg(&t, &chat, "call:danger_run {}", "queue").await["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (call_id, _) = pending_call(&t).await;
    call(
        &t,
        Method::POST,
        &format!("/v1/approvals/{call_id}"),
        Some(json!({"decision": "deny"})),
    )
    .await;
    wait_state(&t, &run, "succeeded").await;
    // After PUT policy full it no longer asks.
    call(
        &t,
        Method::PUT,
        "/v1/policy",
        Some(json!({"preset": "full"})),
    )
    .await;
    let reply = run_and_reply(&t, &chat, "call:danger_run {}").await;
    assert!(reply.contains("boom done"), "{reply}");
}

// ------------------------------------------------------------- providers

struct Second;

#[async_trait]
impl provider_catalog::ChatProvider for Second {
    fn id(&self) -> &'static str {
        "second"
    }
    async fn stream(
        &self,
        _req: provider_catalog::ChatRequest,
    ) -> Result<
        futures::stream::BoxStream<
            'static,
            Result<provider_catalog::ChatChunk, provider_catalog::ProviderError>,
        >,
        provider_catalog::ProviderError,
    > {
        Err(provider_catalog::ProviderError::InvalidRequest(
            "unused".into(),
        ))
    }
    async fn list_models(&self) -> Result<Vec<String>, provider_catalog::ProviderError> {
        Ok(vec!["m1".into(), "m2".into()])
    }
}

fn catalog_entry(id: &str) -> provider_catalog::ProviderEntry {
    provider_catalog::ProviderEntry {
        id: id.into(),
        kind: provider_catalog::ProviderKind::OpenAiCompatible,
        base_url: "https://api.example.test/v1".into(),
        auth: provider_catalog::Auth::ApiKeyEnv("EXAMPLE_KEY".into()),
        capabilities: provider_catalog::ModelCapabilities {
            tools: true,
            vision: false,
            streaming: true,
            json_mode: false,
        },
        compat_flags: provider_catalog::CompatFlags::default(),
        models_endpoint: None,
        models: Vec::new(),
    }
}

#[tokio::test]
async fn providers_route_without_a_host_lists_only_what_is_loaded() {
    let t = setup();
    let r = call(&t, Method::GET, "/v1/providers", None).await;
    assert_eq!(r.status, StatusCode::OK);
    let p = r.json();
    assert_eq!(p["default_provider"], "fake");
    assert_eq!(p["active"], json!(["fake"]));
    assert_eq!(p["configured"], json!([]));
    assert_eq!(p["catalog"], json!([]));
    let r = call(&t, Method::POST, "/v1/providers/reload", None).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn providers_are_listed_with_models_and_reload_swaps_them() {
    let t = setup();
    let mut cfg = provider_catalog::ProviderConfig::default();
    let mut s = provider_catalog::ProviderSettings::new("second", 1);
    s.default_model = Some("m1".into());
    cfg.upsert(s);
    cfg.upsert(provider_catalog::ProviderSettings::new("ghost", 2));
    cfg.save(&provider_catalog::config_path(t._dir.path()))
        .unwrap();

    let fail = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let fail2 = Arc::clone(&fail);
    t.state.set_provider_host(crate::ProviderHost {
        catalog: provider_catalog::Catalog::from_entries([
            catalog_entry("second"),
            catalog_entry("aaa"),
        ]),
        reload: Box::new(move || {
            if fail2.load(std::sync::atomic::Ordering::SeqCst) {
                return Err("no usable provider".into());
            }
            Ok(Providers::single(Arc::new(Second)).with(Arc::new(FakeProvider)))
        }),
    });

    let p = call(&t, Method::GET, "/v1/providers", None).await.json();
    let ids: Vec<&str> = p["catalog"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["aaa", "second"], "sorted catalog");
    assert_eq!(p["catalog"][0]["auth"], "api_key_env");
    assert_eq!(p["catalog"][0]["key_env"], "EXAMPLE_KEY");
    assert_eq!(p["catalog"][0]["configured"], false);
    assert_eq!(p["catalog"][1]["configured"], true);
    let conf = p["configured"].as_array().unwrap();
    assert_eq!(conf.len(), 2);
    assert_eq!(conf[0]["id"], "second");
    assert_eq!(conf[0]["is_default"], true);
    assert_eq!(conf[0]["default_model"], "m1");
    assert_eq!(conf[0]["active"], false, "not loaded until reload");
    assert_eq!(conf[0]["models"], json!([]));
    assert!(conf[0].get("key").is_none());

    // A failing reload keeps the old providers.
    fail.store(true, std::sync::atomic::Ordering::SeqCst);
    let r = call(&t, Method::POST, "/v1/providers/reload", None).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        r.json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no usable")
    );
    assert_eq!(t.agent.providers().ids(), ["fake"]);

    fail.store(false, std::sync::atomic::Ordering::SeqCst);
    let r = call(&t, Method::POST, "/v1/providers/reload", None).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    assert_eq!(r.json()["default_provider"], "second");
    assert_eq!(r.json()["active"], json!(["fake", "second"]));

    let p = call(&t, Method::GET, "/v1/providers", None).await.json();
    let conf = p["configured"].as_array().unwrap();
    assert_eq!(conf[0]["active"], true);
    assert_eq!(conf[0]["models"], json!(["m1", "m2"]));
    assert_eq!(conf[1]["active"], false);
    assert_eq!(p["default_provider"], "second");
}

// ----------------------------------------------------------- usage cost

#[tokio::test]
async fn usage_carries_cost_only_when_a_price_is_known() {
    let t = setup();
    let (_, chat) = make_chat(&t).await;
    run_and_reply(&t, &chat, "hello").await;
    let u = call(&t, Method::GET, "/v1/usage", None).await.json();
    assert!(u["total"]["cost_usd"].is_null(), "{u}");

    // Same run, priced model.
    let dir = tempfile::tempdir().unwrap();
    let storage = Arc::new(Storage::open(dir.path()).unwrap());
    let mut config = LoopConfig::default();
    config.prices.insert(
        "fake/echo".into(),
        agent_core::ModelPrice {
            input_per_mtok: 2.0,
            output_per_mtok: 10.0,
            cached_per_mtok: None,
        },
    );
    let agent = AgentLoop::new(
        Arc::clone(&storage),
        ToolRegistry::new(),
        Providers::single(Arc::new(FakeProvider)),
        config,
    );
    let state = AppState::new(agent.clone(), storage, DaemonConfig::new(TOKEN)).unwrap();
    let priced = T {
        app: router(state.clone()),
        state,
        agent,
        _dir: dir,
    };
    let (bot, chat) = make_chat(&priced).await;
    run_and_reply(&priced, &chat, "hello").await;
    let u = call(&priced, Method::GET, "/v1/usage", None).await.json();
    // 10 prompt + 5 completion tokens.
    let want = (10.0 * 2.0 + 5.0 * 10.0) / 1_000_000.0;
    let got = u["total"]["cost_usd"].as_f64().unwrap();
    assert!((got - want).abs() < 1e-12, "{got} vs {want}");
    let by_bot = u["by_bot"].as_array().unwrap();
    assert_eq!(by_bot[0]["bot_id"], bot.as_str());
    assert!((by_bot[0]["cost_usd"].as_f64().unwrap() - want).abs() < 1e-12);
}

#[tokio::test]
async fn usage_cost_uses_the_providers_catalog_prices_and_follows_a_reload() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Arc::new(Storage::open(dir.path()).unwrap());
    let priced = |input: f64| {
        Providers::single(Arc::new(FakeProvider)).with_catalog_prices(
            "fake",
            &[provider_catalog::ModelInfo {
                id: "echo".into(),
                input_per_mtok: Some(input),
                output_per_mtok: Some(10.0),
                ..provider_catalog::ModelInfo::default()
            }],
        )
    };
    let agent = AgentLoop::new(
        Arc::clone(&storage),
        ToolRegistry::new(),
        priced(2.0),
        LoopConfig::default(),
    );
    let state = AppState::new(agent.clone(), storage, DaemonConfig::new(TOKEN)).unwrap();
    let t = T {
        app: router(state.clone()),
        state,
        agent,
        _dir: dir,
    };
    let (_, chat) = make_chat(&t).await;
    run_and_reply(&t, &chat, "hello").await;
    let cost = |u: &Value| u["total"]["cost_usd"].as_f64().unwrap();
    let u = call(&t, Method::GET, "/v1/usage", None).await.json();
    assert!(
        (cost(&u) - (10.0 * 2.0 + 5.0 * 10.0) / 1e6).abs() < 1e-12,
        "{u}"
    );

    // A providers reload brings new prices; `/v1/usage` prices every recorded
    // call at the prices currently loaded.
    t.agent.set_providers(priced(4.0));
    run_and_reply(&t, &chat, "again").await;
    let u = call(&t, Method::GET, "/v1/usage", None).await.json();
    let want = (20.0 * 4.0 + 10.0 * 10.0) / 1e6;
    assert!((cost(&u) - want).abs() < 1e-12, "{u}");
}

#[tokio::test]
async fn tool_completion_events_carry_a_preview_and_duration() {
    let t = setup();
    let (_, chat) = make_chat(&t).await;
    let run = send_msg(&t, &chat, "call:danger_run {}", "queue").await["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (call_id, _) = pending_call(&t).await;
    call(
        &t,
        Method::POST,
        &format!("/v1/approvals/{call_id}"),
        Some(json!({"decision": "approve"})),
    )
    .await;
    wait_state(&t, &run, "succeeded").await;
    let fr = events(&t, &format!("?run_id={run}"), None).await;
    let done = fr.iter().find(|f| f.1 == "tool_call_completed").unwrap();
    assert_eq!(done.2["event"]["output_preview"], "boom done");
    assert!(done.2["event"]["duration_ms"].is_u64());
}
