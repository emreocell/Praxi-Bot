#![allow(clippy::too_many_lines)]
use super::*;
use ::live_view::*;
use async_trait::async_trait;
use axum::{
    Router,
    body::Body,
    http::{Method, Request},
    response::Response,
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tower::ServiceExt;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const PIXEL_SECRET: &[u8] = b"pixel-password-OTP-never-persist-4815162342";
const TEXT_SECRET: &str = "human-input-secret-never-persist-7654321";
#[derive(Default)]
struct Sink {
    calls: AtomicUsize,
    resets: AtomicUsize,
    pin: AtomicBool,
    fail_at: AtomicUsize,
    hold: AtomicBool,
    entered: tokio::sync::Notify,
    proceed: tokio::sync::Notify,
}
#[async_trait]
impl InputSink for Sink {
    async fn inject(&self, _event: &InputEvent) -> Result<(), ViewError> {
        self.entered.notify_one();
        if self.hold.load(Ordering::SeqCst) {
            self.proceed.notified().await;
        }
        let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if self.fail_at.load(Ordering::SeqCst) == n {
            Err(ViewError::PartialInjection)
        } else {
            Ok(())
        }
    }
    async fn release_pressed_inputs(&self) -> Result<(), ViewError> {
        self.resets.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn set_takeover_active(&self, active: bool) -> Result<(), ViewError> {
        self.pin.store(active, Ordering::SeqCst);
        Ok(())
    }
}
struct Target {
    info: Mutex<TargetInfo>,
    source: Arc<SyntheticSource>,
    sink: Arc<Sink>,
}
impl Target {
    fn new() -> Self {
        Self {
            info: Mutex::new(TargetInfo {
                target_id: Uuid::now_v7(),
                kind: TargetKind::Browser,
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
            }),
            source: Arc::default(),
            sink: Arc::default(),
        }
    }
    fn frame(&self, seq: u64, revision: u64) {
        self.source
            .publish(Frame {
                metadata: FrameMetadata {
                    target_id: self.info().target_id,
                    generation: self.info().generation,
                    frame_seq: seq,
                    captured_at_ms: u64::try_from(storage::now_ms()).unwrap(),
                    cause: Cause::Refresh,
                    action_seq: None,
                    codec: Codec::Jpeg,
                    width: 640,
                    height: 400,
                    source_rect: SourceRect {
                        x: -1280.0,
                        y: 0.0,
                        width: 1280,
                        height: 800,
                        units: Units::PhysicalPx,
                    },
                    geometry_revision: revision,
                    repeated: false,
                },
                payload: Arc::from(PIXEL_SECRET),
            })
            .unwrap();
    }
}
#[async_trait]
impl ViewTarget for Target {
    fn info(&self) -> TargetInfo {
        self.info.lock().clone()
    }
    fn frame_source(&self) -> Arc<dyn FrameSource> {
        self.source.clone()
    }
    fn input_sink(&self) -> Option<Arc<dyn InputSink>> {
        Some(self.sink.clone())
    }
}
struct Test {
    state: AppState,
    target: Arc<Target>,
    dir: tempfile::TempDir,
}
impl Test {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(storage::Storage::open(dir.path()).unwrap());
        let agent = agent_core::AgentLoop::new(
            storage.clone(),
            tool_registry::ToolRegistry::new(),
            agent_core::Providers::single(Arc::new(crate::fake::FakeProvider)),
            agent_core::LoopConfig::default(),
        );
        let state = AppState::new(agent, storage, crate::DaemonConfig::new(TOKEN)).unwrap();
        let target = Arc::new(Target::new());
        state.live_view().registry.register(target.clone());
        Self { state, target, dir }
    }
    fn router(&self, peer: Option<&str>) -> Router {
        if peer.is_some() {
            crate::peer_router(self.state.clone())
        } else {
            crate::router(self.state.clone())
        }
    }
    async fn response(
        &self,
        peer: Option<&str>,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Response {
        let mut req = Request::builder()
            .method(method)
            .uri(path)
            .header("host", "localhost")
            .header("authorization", format!("Bearer {TOKEN}"))
            .header("accept", MEDIA_TYPE)
            .header("content-type", "application/json")
            .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
            .unwrap();
        if let Some(key) = peer {
            req.extensions_mut().insert(crate::PeerIdentity {
                key: key.into(),
                role: crate::PeerRole::Desktop,
            });
        }
        self.router(peer).oneshot(req).await.unwrap()
    }
    async fn call(
        &self,
        peer: Option<&str>,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let response = self.response(peer, method, path, body).await;
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }
    async fn session(&self, peer: Option<&str>) -> Uuid {
        let (status,value)=self.call(peer,Method::POST,"/v1/view/sessions",Some(json!({"target_id":self.target.info().target_id,"profile":"watch","max_width":960,"max_height":600,"codecs":["jpeg"],"client_id":Uuid::now_v7()}))).await;
        assert_eq!(status, StatusCode::CREATED, "{value}");
        Uuid::parse_str(value["session_id"].as_str().unwrap()).unwrap()
    }
    async fn active(&self, peer: Option<&str>) -> (Uuid, TakeoverInfo, Body) {
        let id = self.session(peer).await;
        let response = self
            .response(
                peer,
                Method::GET,
                &format!("/v1/view/sessions/{id}/frames"),
                None,
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let mut body = response.into_body();
        body.frame().await.unwrap().unwrap();
        self.target.frame(1, 1);
        for _ in 0..4 {
            if self
                .state
                .live_view()
                .sessions
                .lock()
                .get(&id)
                .unwrap()
                .geometry
                .is_some()
            {
                break;
            }
            let _ = tokio::time::timeout(Duration::from_millis(250), body.frame()).await;
        }
        let (status, v) = self
            .call(
                peer,
                Method::POST,
                "/v1/control/takeover",
                Some(json!({"session_id":id,"request_id":Uuid::now_v7()})),
            )
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{v}");
        for _ in 0..100 {
            if self.state.hub().takeover().unwrap().info.state == ViewState::Active {
                break;
            }
            tokio::task::yield_now().await;
        }
        let takeover = self.state.hub().takeover().unwrap().info;
        assert_eq!(takeover.state, ViewState::Active);
        (id, takeover, body)
    }
    fn batch(t: &TakeoverInfo, first: u64) -> Value {
        json!({"v":1,"takeover_id":t.id,"control_epoch":t.control_epoch,"generation":1,"geometry_revision":1,"first_seq":first,"events":[{"type":"text","text":TEXT_SECRET}]})
    }
    async fn release(
        &self,
        peer: Option<&str>,
        t: &TakeoverInfo,
        request: Uuid,
    ) -> (StatusCode, Value) {
        self.call(peer,Method::POST,"/v1/control/release",Some(json!({"takeover_id":t.id,"control_epoch":t.control_epoch,"request_id":request,"outcome":"completed","human_action":"login_completed"}))).await
    }
}

#[tokio::test]
async fn all_routes_require_auth_host_origin_and_never_query_tokens() {
    let t = Test::new();
    let id = Uuid::now_v7();
    for (method, path, body) in [
        (Method::GET, "/v1/view/targets".into(), None),
        (Method::POST, "/v1/view/sessions".into(), Some(json!({}))),
        (Method::GET, format!("/v1/view/sessions/{id}/frames"), None),
        (
            Method::PATCH,
            format!("/v1/view/sessions/{id}"),
            Some(json!({})),
        ),
        (
            Method::PUT,
            format!("/v1/view/sessions/{id}/heartbeat"),
            Some(json!({})),
        ),
        (Method::DELETE, format!("/v1/view/sessions/{id}"), None),
        (
            Method::POST,
            format!("/v1/view/sessions/{id}/input"),
            Some(json!({})),
        ),
        (Method::POST, "/v1/control/takeover".into(), Some(json!({}))),
    ] {
        for (extra, expected) in [
            (None, StatusCode::UNAUTHORIZED),
            (Some(("host", "evil.example")), StatusCode::FORBIDDEN),
            (
                Some(("origin", "https://evil.example")),
                StatusCode::FORBIDDEN,
            ),
        ] {
            let mut request = Request::builder()
                .method(method.clone())
                .uri(format!("{path}?token={TOKEN}"))
                .header(
                    "host",
                    extra
                        .filter(|(k, _)| *k == "host")
                        .map_or("localhost", |(_, v)| v),
                );
            if let Some(("origin", v)) = extra {
                request = request.header("origin", v);
            }
            let response = t
                .router(None)
                .oneshot(
                    request
                        .body(
                            body.as_ref()
                                .map_or_else(Body::empty, |v| Body::from(v.to_string())),
                        )
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), expected, "{method} {path}");
        }
    }
    assert_eq!(
        t.call(
            None,
            Method::GET,
            &format!("/v1/view/targets?token={TOKEN}"),
            None
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    for role in [crate::PeerRole::Node, crate::PeerRole::Host] {
        let mut req = Request::builder()
            .uri("/v1/view/targets")
            .body(Body::empty())
            .unwrap();
        req.extensions_mut().insert(crate::PeerIdentity {
            key: "node".into(),
            role,
        });
        assert_eq!(
            crate::peer_router(t.state.clone())
                .oneshot(req)
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
    }
}
#[tokio::test]
async fn watch_has_no_overlay_registration_one_stream_and_actor_binding() {
    let t = Test::new();
    let id = t.session(Some("alice")).await;
    assert!(!t.target.sink.pin.load(Ordering::SeqCst));
    assert_eq!(t.target.source.subscribers(), 0);
    assert!(!t.state.hub().app_connected());
    let path = format!("/v1/view/sessions/{id}/frames");
    assert_eq!(
        t.response(Some("bob"), Method::GET, &path, None)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    let response = t.response(Some("alice"), Method::GET, &path, None).await;
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert_eq!(
        t.response(Some("alice"), Method::GET, &path, None)
            .await
            .status(),
        StatusCode::CONFLICT
    );
    assert!(!t.state.hub().app_connected());
    drop(response);
    for _ in 0..100 {
        if t.target.source.subscribers() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(t.target.source.subscribers(), 0);
}
#[tokio::test]
async fn owner_only_input_release_and_legacy_release_bypasses_are_closed() {
    let t = Test::new();
    let (id, owned, _body) = t.active(Some("alice")).await;
    assert!(t.target.sink.pin.load(Ordering::SeqCst));
    let path = format!("/v1/view/sessions/{id}/input");
    assert_eq!(
        t.call(
            Some("bob"),
            Method::POST,
            &path,
            Some(Test::batch(&owned, 1))
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        t.release(Some("bob"), &owned, Uuid::now_v7()).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        t.call(None, Method::POST, "/v1/control/release", None)
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        t.call(
            None,
            Method::PUT,
            "/v1/control/state",
            Some(json!({"mode":"idle","release":true}))
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let (status, _) = t
        .call(
            Some("alice"),
            Method::POST,
            &path,
            Some(Test::batch(&owned, 1)),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let release_id = Uuid::now_v7();
    assert_eq!(
        t.release(Some("alice"), &owned, release_id).await.0,
        StatusCode::OK
    );
    assert!(t.state.hub().latched().is_none());
    assert!(!t.target.sink.pin.load(Ordering::SeqCst));
    assert_eq!(
        t.release(Some("alice"), &owned, release_id).await.0,
        StatusCode::OK,
        "lost response retry"
    );
    assert_eq!(
        t.call(
            Some("alice"),
            Method::POST,
            &path,
            Some(Test::batch(&owned, 2))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(t.target.sink.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn duplicate_results_never_reinject_gaps_mismatch_and_geometry_reject() {
    let t = Test::new();
    let (id, owned, _body) = t.active(None).await;
    let path = format!("/v1/view/sessions/{id}/input");
    let batch = Test::batch(&owned, 1);
    let first = t.call(None, Method::POST, &path, Some(batch.clone())).await;
    assert_eq!(first.0, StatusCode::OK);
    let second = t.call(None, Method::POST, &path, Some(batch.clone())).await;
    assert_eq!(second, first);
    assert_eq!(t.target.sink.calls.load(Ordering::SeqCst), 1);
    let mut changed = batch;
    changed["events"][0]["text"] = "different".into();
    assert_eq!(
        t.call(None, Method::POST, &path, Some(changed)).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        t.call(None, Method::POST, &path, Some(Test::batch(&owned, 3)))
            .await
            .0,
        StatusCode::CONFLICT
    );
    let mut changed = Test::batch(&owned, 2);
    changed["generation"] = 2.into();
    assert_eq!(
        t.call(None, Method::POST, &path, Some(changed)).await.0,
        StatusCode::CONFLICT
    );
    let mut changed = Test::batch(&owned, 2);
    changed["events"] = json!([{"type":"pointer","phase":"down","x":0.5,"y":0.5,"button":"left","buttons":1,"modifiers":[]}]);
    changed["geometry_revision"] = 2.into();
    assert_eq!(
        t.call(None, Method::POST, &path, Some(changed)).await.0,
        StatusCode::CONFLICT
    );
    let mut changed = Test::batch(&owned, 2);
    changed["control_epoch"] = 999.into();
    assert_eq!(
        t.call(None, Method::POST, &path, Some(changed)).await.0,
        StatusCode::FORBIDDEN
    );
}
#[tokio::test]
async fn partial_injection_invalidates_input_and_retains_pause() {
    let t = Test::new();
    let (id, owned, _body) = t.active(None).await;
    t.target.sink.fail_at.store(2, Ordering::SeqCst);
    let mut batch = Test::batch(&owned, 1);
    batch["events"] = json!([{"type":"text","text":TEXT_SECRET},{"type":"reset"},{"type":"text","text":"second"},{"type":"text","text":"third"}]);
    let (status, v) = t
        .call(
            None,
            Method::POST,
            &format!("/v1/view/sessions/{id}/input"),
            Some(batch),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["applied_through"], 2);
    assert_eq!(v["state"], "paused");
    assert!(t.state.hub().latched().is_some());
    assert_eq!(t.target.sink.calls.load(Ordering::SeqCst), 2);
    assert!(t.target.sink.resets.load(Ordering::SeqCst) >= 3);
    assert_eq!(
        t.release(None, &owned, Uuid::now_v7()).await.0,
        StatusCode::FORBIDDEN
    );
}
#[tokio::test]
async fn release_is_a_barrier_for_in_flight_input_and_old_epochs() {
    let t = Arc::new(Test::new());
    let (id, owned, _body) = t.active(None).await;
    t.target.sink.hold.store(true, Ordering::SeqCst);
    let worker = t.clone();
    let batch = Test::batch(&owned, 1);
    let injection = tokio::spawn(async move {
        worker
            .call(
                None,
                Method::POST,
                &format!("/v1/view/sessions/{id}/input"),
                Some(batch),
            )
            .await
    });
    t.target.sink.entered.notified().await;
    let worker = t.clone();
    let takeover = owned.clone();
    let release =
        tokio::spawn(async move { worker.release(None, &takeover, Uuid::now_v7()).await });
    tokio::task::yield_now().await;
    assert!(!release.is_finished());
    t.target.sink.proceed.notify_one();
    assert_eq!(injection.await.unwrap().0, StatusCode::OK);
    assert_eq!(release.await.unwrap().0, StatusCode::OK);
    assert_eq!(
        t.call(
            None,
            Method::POST,
            &format!("/v1/view/sessions/{id}/input"),
            Some(Test::batch(&owned, 2))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(t.target.sink.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn revoke_closes_open_stream_resets_inputs_and_never_resumes() {
    let t = Test::new();
    let (id, owned, mut body) = t.active(Some("alice")).await;
    t.state
        .set_screen_privileges(
            "peer:alice".into(),
            ScreenPrivileges {
                view: false,
                input: false,
            },
        )
        .await
        .unwrap();
    for _ in 0..10 {
        if body.frame().await.is_none() {
            break;
        }
    }
    assert!(t.state.hub().latched().is_some());
    assert_eq!(
        t.state.hub().takeover().unwrap().info.state,
        ViewState::Paused
    );
    assert_eq!(
        t.call(
            Some("alice"),
            Method::POST,
            &format!("/v1/view/sessions/{id}/input"),
            Some(Test::batch(&owned, 1))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert!(!t.target.sink.pin.load(Ordering::SeqCst));
}
#[tokio::test(start_paused = true)]
async fn session_ttl_is_not_extended_by_stream_and_heartbeat_cannot_revive() {
    let t = Test::new();
    let id = t.session(None).await;
    tokio::time::advance(Duration::from_secs(31)).await;
    tokio::task::yield_now().await;
    let status = t
        .call(
            None,
            Method::PUT,
            &format!("/v1/view/sessions/{id}/heartbeat"),
            Some(json!({})),
        )
        .await
        .0;
    assert!(matches!(status, StatusCode::NOT_FOUND | StatusCode::GONE));
}
#[tokio::test]
async fn separate_input_privilege_phone_scope_and_session_quotas() {
    let t = Test::new();
    t.state
        .set_screen_privileges(
            "peer:alice".into(),
            ScreenPrivileges {
                view: true,
                input: false,
            },
        )
        .await
        .unwrap();
    let id = t.session(Some("alice")).await;
    assert_eq!(
        t.call(
            Some("alice"),
            Method::POST,
            "/v1/control/takeover",
            Some(json!({"session_id":id,"request_id":Uuid::now_v7()}))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let _ = t.session(Some("alice")).await;
    let v = json!({"target_id":t.target.info().target_id,"profile":"watch","max_width":960,"max_height":600,"codecs":["jpeg"],"client_id":Uuid::now_v7()});
    assert_eq!(
        t.call(Some("alice"), Method::POST, "/v1/view/sessions", Some(v))
            .await
            .0,
        StatusCode::TOO_MANY_REQUESTS
    );
    let _ = t.session(Some("bob")).await;
    let _ = t.session(Some("bob")).await;
    assert_eq!(t.call(Some("carol"),Method::POST,"/v1/view/sessions",Some(json!({"target_id":t.target.info().target_id,"profile":"watch","max_width":960,"max_height":600,"codecs":["jpeg"],"client_id":Uuid::now_v7()}))).await.0,StatusCode::TOO_MANY_REQUESTS);
    t.state.hub().put_state(crate::dto::ControlMode::Paused);
    let mut request=Request::builder().method(Method::POST).uri("/v1/control/takeover").header("content-type","application/json").body(Body::from(json!({"session_id":id,"request_id":Uuid::now_v7(),"recover":true,"reason":"owner_lost"}).to_string())).unwrap();
    request.extensions_mut().insert(crate::PeerIdentity {
        key: "alice".into(),
        role: crate::PeerRole::Phone,
    });
    assert_eq!(
        crate::peer_router(t.state.clone())
            .oneshot(request)
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
}
#[tokio::test]
async fn secrets_never_enter_audit_latch_files_events_or_continuation() {
    let t = Test::new();
    let (id, owned, mut body) = t.active(None).await;
    t.target.frame(2, 1);
    let mut observed = false;
    for _ in 0..10 {
        if let Ok(Some(Ok(frame))) =
            tokio::time::timeout(Duration::from_millis(300), body.frame()).await
            && let Ok(bytes) = frame.into_data()
            && bytes.windows(PIXEL_SECRET.len()).any(|w| w == PIXEL_SECRET)
        {
            observed = true;
            break;
        }
    }
    assert!(observed, "fake pixel frame not streamed");
    assert_eq!(
        t.call(
            None,
            Method::POST,
            &format!("/v1/view/sessions/{id}/input"),
            Some(Test::batch(&owned, 1))
        )
        .await
        .0,
        StatusCode::OK
    );
    let release_id = Uuid::now_v7();
    assert_eq!(t.release(None, &owned, release_id).await.0, StatusCode::OK);
    for path in std::fs::read_dir(t.dir.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_file())
    {
        let bytes = std::fs::read(path).unwrap();
        assert!(!bytes.windows(PIXEL_SECRET.len()).any(|w| w == PIXEL_SECRET));
        assert!(
            !bytes
                .windows(TEXT_SECRET.len())
                .any(|w| w == TEXT_SECRET.as_bytes())
        );
    }
    let journal = t.state.hub().release_journal(release_id).unwrap();
    assert!(journal.summary.contains("user reports login completed"));
    assert!(!journal.summary.contains(TEXT_SECRET));
}

#[tokio::test]
async fn audit_failure_denies_new_view_and_interrupts_active_input() {
    let t = Test::new();
    let (id, owned, _body) = t.active(None).await;
    let db = rusqlite::Connection::open(t.dir.path().join("bot.db")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_live_audit BEFORE INSERT ON audit_log BEGIN SELECT RAISE(ABORT, 'injected audit failure'); END;").unwrap();
    let status = t
        .call(
            None,
            Method::POST,
            &format!("/v1/view/sessions/{id}/input"),
            Some(Test::batch(&owned, 1)),
        )
        .await
        .0;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(t.target.sink.calls.load(Ordering::SeqCst), 0);
    assert!(t.state.hub().latched().is_some());
    let status=t.call(None,Method::POST,"/v1/view/sessions",Some(json!({"target_id":t.target.info().target_id,"profile":"watch","max_width":960,"max_height":600,"codecs":["jpeg"],"client_id":Uuid::now_v7()}))).await.0;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}
#[tokio::test]
async fn rate_overflow_suspends_input_instead_of_growing_a_queue() {
    let t = Test::new();
    let (id, owned, _body) = t.active(None).await;
    let path = format!("/v1/view/sessions/{id}/input");
    for (first, expected) in [
        (1, StatusCode::OK),
        (33, StatusCode::OK),
        (65, StatusCode::TOO_MANY_REQUESTS),
    ] {
        let mut batch = Test::batch(&owned, first);
        batch["events"] = json!(vec![json!({"type":"reset"}); 32]);
        assert_eq!(
            t.call(None, Method::POST, &path, Some(batch)).await.0,
            expected
        );
    }
    assert_eq!(
        t.state.hub().takeover().unwrap().info.state,
        ViewState::Paused
    );
    assert!(t.state.hub().latched().is_some());
}
#[tokio::test]
async fn explicit_reacquisition_rotates_epoch_and_old_input_never_applies() {
    let t = Test::new();
    let (id, old, body) = t.active(Some("alice")).await;
    drop(body);
    for _ in 0..100 {
        if t.state.hub().takeover().unwrap().info.state == ViewState::Paused {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let response = t
        .response(
            Some("alice"),
            Method::GET,
            &format!("/v1/view/sessions/{id}/frames"),
            None,
        )
        .await;
    let mut body = response.into_body();
    body.frame().await.unwrap().unwrap();
    t.target.frame(2, 1);
    let (status, v) = t
        .call(
            Some("alice"),
            Method::POST,
            "/v1/control/takeover",
            Some(
                json!({"session_id":id,"request_id":Uuid::now_v7(),"reacquire_takeover_id":old.id}),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{v}");
    assert_ne!(v["control_epoch"], old.control_epoch);
    assert_ne!(v["takeover_id"], old.id.to_string());
    assert_eq!(
        t.call(
            Some("alice"),
            Method::POST,
            &format!("/v1/view/sessions/{id}/input"),
            Some(Test::batch(&old, 1))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}
#[tokio::test(start_paused = true)]
async fn blocked_body_is_cancelled_after_two_seconds_and_capture_is_dropped() {
    let t = Test::new();
    let id = t.session(None).await;
    let response = t
        .response(
            None,
            Method::GET,
            &format!("/v1/view/sessions/{id}/frames"),
            None,
        )
        .await;
    tokio::task::yield_now().await;
    for seq in 0..10_000 {
        t.target.frame(seq, 1);
    }
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(2100)).await;
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert_eq!(t.target.source.subscribers(), 0);
    drop(response);
    assert!(!t.state.hub().app_connected());
}

fn completed_run(t: &Test) -> (protocol::RunId, protocol::ChatId) {
    let bot = protocol::BotId::new();
    let chat = protocol::ChatId::new();
    let run = protocol::RunId::new();
    let now = storage::now_ms();
    let user = t.state.user_id();
    t.state
        .storage()
        .insert_bot(&storage::Bot {
            id: bot,
            user_id: user,
            name: "fake".into(),
            config: json!({}),
            created_at_ms: now,
        })
        .unwrap();
    t.state
        .storage()
        .insert_chat(&storage::Chat {
            id: chat,
            user_id: user,
            bot_id: bot,
            title: "fake".into(),
            created_at_ms: now,
        })
        .unwrap();
    t.state
        .storage()
        .insert_run(&storage::Run {
            id: run,
            user_id: user,
            bot_id: bot,
            chat_id: Some(chat),
            kind: "turn".into(),
            state: "succeeded".into(),
            error: None,
            created_at_ms: now,
            updated_at_ms: now,
            parent_run_id: None,
            spec: None,
        })
        .unwrap();
    t.target.info.lock().run_id = Some(run.0);
    (run, chat)
}
#[tokio::test]
async fn continuation_is_durable_exactly_once_and_does_not_restart_a_finished_run() {
    let t = Test::new();
    let (_session, owned, _body) = t.active(None).await;
    let (run, chat) = completed_run(&t);
    let request = Uuid::now_v7();
    assert_eq!(t.release(None, &owned, request).await.0, StatusCode::OK);
    assert_eq!(t.release(None, &owned, request).await.0, StatusCode::OK);
    let messages = t.state.storage().list_messages(chat).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].role, "system");
    assert!(messages[0].content.contains("user reports login completed"));
    assert_eq!(
        t.state.storage().get_run(run).unwrap().unwrap().state,
        "succeeded"
    );
    let mut saved = t.state.hub().release_journal(request).unwrap();
    flush_note(&t.state, &mut saved).unwrap();
    assert_eq!(t.state.storage().list_messages(chat).unwrap().len(), 1);
}
#[tokio::test]
async fn crash_after_note_delivery_reconciles_by_message_id_and_keeps_pause() {
    let t = Test::new();
    let (_session, owned, _body) = t.active(None).await;
    let (run, chat) = completed_run(&t);
    let request = Uuid::now_v7();
    let mut journal = t
        .state
        .hub()
        .prepare_release(ReleaseJournal {
            takeover_id: owned.id,
            epoch: owned.control_epoch,
            request_id: request,
            actor: Actor::local(&t.state).key,
            human_action: None,
            summary: "Human intervention completed; 1 pointer event".into(),
            runs: vec![run],
            messages: vec![],
            completed: false,
            noted: false,
        })
        .unwrap();
    flush_note(&t.state, &mut journal).unwrap();
    let restored = crate::ControlHub::with_latch_file(
        crate::ControlConfig::default(),
        t.dir.path().join("control-latch.json"),
    );
    assert!(restored.latched().is_some());
    assert!(restored.release_journal(request).unwrap().noted);
    flush_note(&t.state, &mut journal).unwrap();
    assert_eq!(t.state.storage().list_messages(chat).unwrap().len(), 1);
    assert!(t.state.hub().latched().is_some());
}

#[tokio::test]
async fn cancelled_input_awaiter_invalidates_epoch_and_drains_actual_backend_before_recovery() {
    let t = Arc::new(Test::new());
    let (id, owned, _body) = t.active(None).await;
    t.target.sink.hold.store(true, Ordering::SeqCst);
    let worker = t.clone();
    let batch = Test::batch(&owned, 1);
    let request = tokio::spawn(async move {
        worker
            .call(
                None,
                Method::POST,
                &format!("/v1/view/sessions/{id}/input"),
                Some(batch),
            )
            .await
    });
    t.target.sink.entered.notified().await;
    request.abort();
    let _ = request.await;
    assert_eq!(
        t.state.hub().takeover().unwrap().info.state,
        ViewState::Paused
    );
    assert_eq!(t.state.live_view().pending_input.load(Ordering::SeqCst), 1);
    let (status,_)=t.call(None,Method::POST,"/v1/control/takeover",Some(json!({"session_id":id,"request_id":Uuid::now_v7(),"reacquire_takeover_id":owned.id}))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    t.target.sink.proceed.notify_one();
    for _ in 0..100 {
        if t.state.live_view().pending_input.load(Ordering::SeqCst) == 0 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(t.state.live_view().pending_input.load(Ordering::SeqCst), 0);
    assert!(t.target.sink.resets.load(Ordering::SeqCst) >= 2);
    assert!(t.state.hub().latched().is_some());
}

#[tokio::test]
async fn bounded_duplicate_window_requires_resync_after_sixty_four_batches() {
    let t = Test::new();
    let (id, owned, _body) = t.active(None).await;
    let path = format!("/v1/view/sessions/{id}/input");
    let request = |seq| {
        let mut batch = Test::batch(&owned, seq);
        batch["events"] = json!([{"type":"reset"}]);
        batch
    };
    for seq in 1..=65 {
        assert_eq!(
            t.call(None, Method::POST, &path, Some(request(seq)))
                .await
                .0,
            StatusCode::OK
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let (status, result) = t.call(None, Method::POST, &path, Some(request(1))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(result["error"]["code"], "resync_required");
    assert_eq!(
        t.call(None, Method::POST, &path, Some(request(2))).await.0,
        StatusCode::OK
    );
    assert_eq!(t.target.sink.calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn takeover_session_binding_and_text_byte_rate_are_independent_limits() {
    let t = Test::new();
    let (id, owned, _body) = t.active(None).await;
    let other = t.session(None).await;
    assert_eq!(
        t.call(
            None,
            Method::POST,
            &format!("/v1/view/sessions/{other}/input"),
            Some(Test::batch(&owned, 1))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let mut first = Test::batch(&owned, 1);
    first["events"] =
        json!([{"type":"text","text":"x".repeat(4096)},{"type":"text","text":"y".repeat(4096)}]);
    assert_eq!(
        t.call(
            None,
            Method::POST,
            &format!("/v1/view/sessions/{id}/input"),
            Some(first)
        )
        .await
        .0,
        StatusCode::OK
    );
    let mut next = Test::batch(&owned, 3);
    next["events"] = json!([{"type":"text","text":"z".repeat(4096)}]);
    assert_eq!(
        t.call(
            None,
            Method::POST,
            &format!("/v1/view/sessions/{id}/input"),
            Some(next)
        )
        .await
        .0,
        StatusCode::TOO_MANY_REQUESTS
    );
}
