//! The real daemon behind the real proxy: watch, take over, input, release,
//! other devices, link loss. Synthetic frames and an input sink; no display.
use super::view::{Records, eventually, setup};
use super::{config, pair, state_with_registry, stop};
use crate::{ApiProtocol, ApiProxy, ProxyState, Tuning};
use async_trait::async_trait;
use axum::http::StatusCode;
use daemon_api::live_view::ScreenPrivileges;
use live_view::{
    Capabilities, Cause, Codec, Frame, FrameMetadata, FrameSource, InputEvent, InputSink,
    MEDIA_TYPE, RecordHeader, SourceRect, SyntheticSource, TargetInfo, TargetKind, TargetRegistry,
    Units, ViewError, ViewTarget,
};
use peer_network::{PeerNetwork, Role};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering::SeqCst},
    },
    time::Duration,
};
use tower::ServiceExt;
use uuid::Uuid;

const SECRET_PIXELS: &[u8] = b"synthetic-pixels-never-persisted";
const LOCAL_TOKEN: &str = "server-secret-never-on-client";

#[derive(Default)]
struct Sink {
    injected: AtomicUsize,
    resets: AtomicUsize,
}
#[async_trait]
impl InputSink for Sink {
    async fn inject(&self, _event: &InputEvent) -> Result<(), ViewError> {
        self.injected.fetch_add(1, SeqCst);
        Ok(())
    }
    async fn release_pressed_inputs(&self) -> Result<(), ViewError> {
        self.resets.fetch_add(1, SeqCst);
        Ok(())
    }
}

struct Target {
    id: Uuid,
    source: Arc<SyntheticSource>,
    sink: Arc<Sink>,
}
impl Target {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            id: Uuid::now_v7(),
            source: Arc::default(),
            sink: Arc::default(),
        })
    }
    fn publish(&self, seq: u64) {
        self.source
            .publish(Frame {
                metadata: FrameMetadata {
                    target_id: self.id,
                    generation: 1,
                    frame_seq: seq,
                    captured_at_ms: u64::try_from(storage::now_ms()).unwrap(),
                    cause: Cause::Refresh,
                    action_seq: None,
                    codec: Codec::Jpeg,
                    width: 640,
                    height: 400,
                    source_rect: SourceRect {
                        x: 0.0,
                        y: 0.0,
                        width: 1280,
                        height: 800,
                        units: Units::PhysicalPx,
                    },
                    geometry_revision: 1,
                    repeated: false,
                },
                payload: Arc::from(SECRET_PIXELS),
            })
            .unwrap();
    }
}
#[async_trait]
impl ViewTarget for Target {
    fn info(&self) -> TargetInfo {
        TargetInfo {
            target_id: self.id,
            kind: TargetKind::Desktop,
            host_id: "hermetic".into(),
            bot_id: None,
            run_id: None,
            generation: 1,
            capabilities: Capabilities {
                view: true,
                input: true,
                codecs: vec![Codec::Jpeg],
                reason: None,
            },
            unavailable_reason: None,
        }
    }
    fn frame_source(&self) -> Arc<dyn FrameSource> {
        self.source.clone()
    }
    fn input_sink(&self) -> Option<Arc<dyn InputSink>> {
        Some(self.sink.clone())
    }
}

struct Daemon {
    state: daemon_api::AppState,
    target: Arc<Target>,
    _dir: tempfile::TempDir,
}

fn daemon() -> Daemon {
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(TargetRegistry::default());
    let target = Target::new();
    registry.register(target.clone());
    let state = state_with_registry(&dir.path().join("db"), registry);
    Daemon {
        state,
        target,
        _dir: dir,
    }
}

impl Daemon {
    async fn grant(&self, key: &PeerNetwork) {
        self.state
            .set_screen_privileges(
                format!("peer:{}", key.id()),
                ScreenPrivileges {
                    view: true,
                    input: true,
                },
            )
            .await
            .unwrap();
    }

    /// The daemon's own loopback view of the control state (local user).
    async fn local_state(&self) -> Value {
        let req = axum::http::Request::builder()
            .uri("/v1/control/state")
            .header("host", "localhost")
            .header("authorization", format!("Bearer {LOCAL_TOKEN}"))
            .body(axum::body::Body::empty())
            .unwrap();
        let response = daemon_api::router(self.state.clone())
            .oneshot(req)
            .await
            .unwrap();
        let bytes = http_body_util::BodyExt::collect(response.into_body())
            .await
            .unwrap()
            .to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }
}

struct Session {
    id: String,
    reader: Records<
        futures_util::stream::BoxStream<'static, Result<axum::body::Bytes, reqwest::Error>>,
    >,
    frame_seen: bool,
}

async fn call(
    proxy: &ApiProxy,
    method: reqwest::Method,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = reqwest::Client::new()
        .request(method, format!("{}{path}", proxy.base_url))
        .bearer_auth(&proxy.token)
        .header("Accept", MEDIA_TYPE);
    if let Some(body) = body {
        req = req.json(&body);
    }
    let response = req.send().await.unwrap();
    let status = StatusCode::from_u16(response.status().as_u16()).unwrap();
    let text = response.text().await.unwrap_or_default();
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

async fn open_session(proxy: &ApiProxy, target: &Target) -> Session {
    let (status, created) = call(
        proxy,
        reqwest::Method::POST,
        "/v1/view/sessions",
        Some(
            json!({"target_id": target.id, "profile": "watch", "max_width": 960,
            "max_height": 600, "codecs": ["jpeg"], "client_id": Uuid::now_v7()}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["session_id"].as_str().unwrap().to_owned();
    let response = reqwest::Client::new()
        .get(format!("{}/v1/view/sessions/{id}/frames", proxy.base_url))
        .bearer_auth(&proxy.token)
        .header("Accept", MEDIA_TYPE)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert_eq!(response.headers()["content-type"], MEDIA_TYPE);
    Session {
        id,
        reader: Records::new(futures_util::StreamExt::boxed(response.bytes_stream())),
        frame_seen: false,
    }
}

impl Session {
    /// Read records until the next frame; the payload must be the synthetic one.
    async fn next_frame(&mut self, target: &Target, seq: u64) {
        target.publish(seq);
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let record = self.reader.next().await.unwrap().expect("stream ended");
                if let RecordHeader::Frame { frame, .. } = &record.header
                    && frame.frame_seq == seq
                {
                    assert_eq!(record.payload, SECRET_PIXELS);
                    self.frame_seen = true;
                    return;
                }
            }
        })
        .await
        .expect("frame reaches the paired client");
    }
}

async fn take_over(proxy: &ApiProxy, session: &str) -> Value {
    let (status, accepted) = call(
        proxy,
        reqwest::Method::POST,
        "/v1/control/takeover",
        Some(json!({"session_id": session, "request_id": Uuid::now_v7()})),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
    for _ in 0..200 {
        let (_, state) = call(proxy, reqwest::Method::GET, "/v1/control/state", None).await;
        if state["takeover"]["state"] == "active" {
            return state["takeover"].clone();
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("takeover never became active");
}

fn batch(takeover: &Value, first: u64) -> Value {
    json!({"v": 1, "takeover_id": takeover["id"], "control_epoch": takeover["control_epoch"],
        "generation": 1, "geometry_revision": 1, "first_seq": first,
        "events": [{"type": "text", "text": "synthetic input"}]})
}

fn release_body(takeover: &Value) -> Value {
    json!({"takeover_id": takeover["id"], "control_epoch": takeover["control_epoch"],
        "request_id": Uuid::now_v7(), "outcome": "completed", "human_action": "other"})
}

#[tokio::test]
async fn paired_desktop_watches_takes_over_types_and_releases_through_the_proxy() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = daemon();
    let (host, client, proxy) = setup(
        dir.path(),
        Role::Desktop,
        ApiProtocol::new(daemon.state.clone()),
        Tuning::default(),
    )
    .await;
    // Default: a paired device may neither watch nor type until the local user grants it.
    let (status, _) = call(&proxy, reqwest::Method::GET, "/v1/view/targets", None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    daemon.grant(&client).await;
    let (status, targets) = call(&proxy, reqwest::Method::GET, "/v1/view/targets", None).await;
    assert_eq!(status, StatusCode::OK, "{targets}");
    assert_eq!(targets[0]["target_id"], daemon.target.id.to_string());

    let mut session = open_session(&proxy, &daemon.target).await;
    session.next_frame(&daemon.target, 1).await;
    let takeover = take_over(&proxy, &session.id).await;
    session.next_frame(&daemon.target, 2).await;
    let (status, result) = call(
        &proxy,
        reqwest::Method::POST,
        &format!("/v1/view/sessions/{}/input", session.id),
        Some(batch(&takeover, 1)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["applied_through"], 1);
    assert_eq!(daemon.target.sink.injected.load(SeqCst), 1);
    // The local daemon sees a human driving; the bot's actions are paused.
    assert_eq!(daemon.local_state().await["latched"], "human_driving");

    // Bitirdim: release resumes the bot and only then.
    let (status, released) = call(
        &proxy,
        reqwest::Method::POST,
        "/v1/control/release",
        Some(release_body(&takeover)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{released}");
    assert!(daemon.local_state().await["latched"].is_null());
    // Input from the finished epoch is refused.
    let (status, _) = call(
        &proxy,
        reqwest::Method::POST,
        &format!("/v1/view/sessions/{}/input", session.id),
        Some(batch(&takeover, 2)),
    )
    .await;
    assert!(status.is_client_error(), "{status}");
    assert_eq!(daemon.target.sink.injected.load(SeqCst), 1);
    let (status, _) = call(
        &proxy,
        reqwest::Method::DELETE,
        &format!("/v1/view/sessions/{}", session.id),
        None,
    )
    .await;
    assert!(status.is_success());
    stop(host, client, proxy).await;
}

#[tokio::test]
async fn another_paired_device_cannot_use_the_session_or_release_the_takeover() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = daemon();
    let (host, client, proxy) = setup(
        dir.path(),
        Role::Desktop,
        ApiProtocol::new(daemon.state.clone()),
        Tuning::default(),
    )
    .await;
    // A second paired desktop with the same grants.
    let other = PeerNetwork::start(config(dir.path(), "other", Role::Desktop), None)
        .await
        .unwrap();
    pair(&host, &other, Role::Desktop).await;
    let mut other_proxy = ApiProxy::start(other.clone(), host.id()).await.unwrap();
    other_proxy
        .state
        .wait_for(|s| *s == ProxyState::Connected)
        .await
        .unwrap();
    daemon.grant(&client).await;
    daemon.grant(&other).await;

    let mut session = open_session(&proxy, &daemon.target).await;
    session.next_frame(&daemon.target, 1).await;
    let takeover = take_over(&proxy, &session.id).await;
    // The other device cannot read the stream, type, heartbeat or release.
    let path = format!("/v1/view/sessions/{}", session.id);
    for (method, suffix, body) in [
        (reqwest::Method::GET, "/frames", None),
        (reqwest::Method::POST, "/input", Some(batch(&takeover, 1))),
        (reqwest::Method::PUT, "/heartbeat", Some(json!({}))),
        (reqwest::Method::DELETE, "", None),
    ] {
        let (status, _) = call(
            &other_proxy,
            method.clone(),
            &format!("{path}{suffix}"),
            body,
        )
        .await;
        assert!(
            status == StatusCode::NOT_FOUND || status == StatusCode::FORBIDDEN,
            "{method} {suffix}: {status}"
        );
    }
    let (status, _) = call(
        &other_proxy,
        reqwest::Method::POST,
        "/v1/control/release",
        Some(release_body(&takeover)),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(daemon.target.sink.injected.load(SeqCst), 0);
    assert_eq!(daemon.local_state().await["latched"], "human_driving");
    // The owner is unaffected.
    let (status, _) = call(
        &proxy,
        reqwest::Method::POST,
        &format!("{path}/input"),
        Some(batch(&takeover, 1)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    drop(other_proxy);
    other.shutdown().await;
    stop(host, client, proxy).await;
}

#[tokio::test]
async fn link_loss_keeps_the_latch_paused_and_stale_input_is_refused_after_reconnect() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = daemon();
    let (host, client, mut proxy) = setup(
        dir.path(),
        Role::Desktop,
        ApiProtocol::new(daemon.state.clone()),
        Tuning::default(),
    )
    .await;
    daemon.grant(&client).await;
    let mut session = open_session(&proxy, &daemon.target).await;
    session.next_frame(&daemon.target, 1).await;
    let takeover = take_over(&proxy, &session.id).await;
    let (status, _) = call(
        &proxy,
        reqwest::Method::POST,
        &format!("/v1/view/sessions/{}/input", session.id),
        Some(batch(&takeover, 1)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Cut the link while a human is driving.
    host.revoke(&client.id()).unwrap();
    tokio::time::timeout(
        Duration::from_secs(5),
        proxy
            .state
            .wait_for(|s| matches!(s, ProxyState::Unreachable(_))),
    )
    .await
    .unwrap()
    .unwrap();
    let cut = tokio::time::timeout(Duration::from_secs(5), session.reader.next())
        .await
        .unwrap();
    assert!(cut.is_err(), "loss ends the body with an error");
    // The daemon pauses input but keeps the bot paused: nothing released it.
    eventually("input interrupted", Duration::from_secs(40), || {
        daemon.target.sink.resets.load(SeqCst) > 0
    })
    .await;
    let local = daemon.local_state().await;
    assert!(
        !local["latched"].is_null(),
        "the latch must survive the loss: {local}"
    );

    // Re-pair: the proxy reconnects, but yesterday's batch is not replayed and
    // today's request with the old epoch is refused.
    pair(&host, &client, Role::Desktop).await;
    daemon.grant(&client).await;
    tokio::time::timeout(
        Duration::from_secs(15),
        proxy.state.wait_for(|s| *s == ProxyState::Connected),
    )
    .await
    .unwrap()
    .unwrap();
    let injected = daemon.target.sink.injected.load(SeqCst);
    let (status, body) = call(
        &proxy,
        reqwest::Method::POST,
        &format!("/v1/view/sessions/{}/input", session.id),
        Some(batch(&takeover, 2)),
    )
    .await;
    assert!(status.is_client_error(), "{status} {body}");
    assert_eq!(daemon.target.sink.injected.load(SeqCst), injected);
    assert!(!daemon.local_state().await["latched"].is_null());
    stop(host, client, proxy).await;
}
