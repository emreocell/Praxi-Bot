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
            Some((id?, ev?, data?))
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

#[tokio::test]
async fn global_stream_is_live_and_resumes_a_run_cursor() {
    let t = setup();
    let (_, chat) = make_chat(&t).await;
    let first = send_msg(&t, &chat, "a b", "queue").await["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    wait_state(&t, &first, "succeeded").await;

    // Open the global stream with a cursor into the finished run, then start a
    // second run: replay of run 1 (seq > 1) followed by live run 2 events.
    let (k, v) = auth();
    let cursor = format!("{first}:1");
    let req = Request::builder()
        .uri("/v1/events")
        .header("host", "localhost")
        .header(k, v.as_str())
        .header("last-event-id", cursor)
        .body(Body::empty())
        .unwrap();
    let resp = t.app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let mut body = resp.into_body();

    let second = send_msg(&t, &chat, "c d", "queue").await["run_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let mut buf = Vec::new();
    let finished = |buf: &[u8], run: &str| {
        frames(buf)
            .iter()
            .any(|f| f.1 == "run_finished" && f.2["event"]["run_id"] == run)
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        while !finished(&buf, &second) {
            let frame = body.frame().await.unwrap().unwrap();
            if let Some(d) = frame.data_ref() {
                buf.extend_from_slice(d);
            }
        }
    })
    .await
    .expect("global stream delivers run 2 live");

    let fr = frames(&buf);
    let of = |run: &str| -> Vec<u64> {
        fr.iter()
            .filter(|f| f.2["event"]["run_id"] == run)
            .map(|f| f.2["seq"].as_u64().unwrap())
            .collect()
    };
    let r1 = of(&first);
    assert!(r1.first() == Some(&2) && r1.windows(2).all(|w| w[1] == w[0] + 1));
    let r2 = of(&second);
    assert_eq!(r2.first(), Some(&0));
    assert!(r2.windows(2).all(|w| w[1] == w[0] + 1));
    assert!(fr.iter().all(|f| f.0.contains(':')));
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
    // The stream ends instead of hanging.
    let end = tokio::time::timeout(Duration::from_secs(5), async {
        while body.frame().await.is_some() {}
    })
    .await;
    assert!(end.is_ok());
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
        "/v1/usage",
    ] {
        assert!(v["paths"].get(p).is_some(), "{p}");
    }
}
