//! Hermetic transport tests: UDP loopback only, relay/discovery disabled.
use crate::{ApiProtocol, ApiProxy, ProxyState};
use agent_core::{AgentLoop, LoopConfig, Providers};
use axum::{
    Router,
    body::Body,
    http::{Method, Request, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use daemon_api::{AppState, DaemonConfig};
use futures_util::{StreamExt, stream};
use http_body_util::BodyExt;
use peer_network::{ALPN_API, PairTicket, PeerConfig, PeerNetwork, RelayConfig, Role};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use storage::Storage;
use tool_registry::ToolRegistry;
use tower::ServiceExt;

mod auth;
mod daemon;
mod loss;
mod streams;
mod view;

fn config(dir: &std::path::Path, name: &str, role: Role) -> PeerConfig {
    let mut cfg = PeerConfig::new(dir.join(name));
    cfg.relay = RelayConfig::Disabled;
    cfg.bind_addr = Some(([127, 0, 0, 1], 0).into());
    cfg.local_role = role;
    cfg.display_name = name.into();
    cfg
}
fn state(dir: &std::path::Path) -> AppState {
    let storage = Arc::new(Storage::open(dir).unwrap());
    let agent = AgentLoop::new(
        storage.clone(),
        ToolRegistry::new(),
        Providers::single(Arc::new(daemon_api::fake::FakeProvider)),
        LoopConfig::default(),
    );
    AppState::new(
        agent,
        storage,
        DaemonConfig::new("server-secret-never-on-client"),
    )
    .unwrap()
}
fn state_with_registry(
    dir: &std::path::Path,
    registry: Arc<live_view::TargetRegistry>,
) -> AppState {
    let storage = Arc::new(Storage::open(dir).unwrap());
    let agent = AgentLoop::new(
        storage.clone(),
        ToolRegistry::new(),
        Providers::single(Arc::new(daemon_api::fake::FakeProvider)),
        LoopConfig::default(),
    );
    AppState::with_view_registry(
        agent,
        storage,
        DaemonConfig::new("server-secret-never-on-client"),
        registry,
    )
    .unwrap()
}
async fn pair(host: &PeerNetwork, client: &PeerNetwork, role: Role) {
    let invite = host.create_invite(role).await;
    client
        .pair(&PairTicket::parse(&invite.ticket).unwrap())
        .await
        .unwrap();
}
async fn networks(
    dir: &std::path::Path,
    role: Role,
    protocol: ApiProtocol,
) -> (PeerNetwork, PeerNetwork, ApiProxy) {
    let mut cfg = config(dir, "host", Role::Host);
    cfg.extra_protocols
        .push((ALPN_API.to_vec(), Arc::new(protocol)));
    let host = PeerNetwork::start(cfg, None).await.unwrap();
    let client = PeerNetwork::start(config(dir, "client", role), None)
        .await
        .unwrap();
    pair(&host, &client, role).await;
    let mut proxy = ApiProxy::start(client.clone(), host.id()).await.unwrap();
    tokio::time::timeout(
        Duration::from_secs(10),
        proxy.state.wait_for(|s| *s == ProxyState::Connected),
    )
    .await
    .unwrap()
    .unwrap();
    (host, client, proxy)
}
fn request(proxy: &ApiProxy, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
    reqwest::Client::new()
        .request(method, format!("{}{path}", proxy.base_url))
        .bearer_auth(&proxy.token)
}
async fn stop(host: PeerNetwork, client: PeerNetwork, proxy: ApiProxy) {
    drop(proxy);
    client.shutdown().await;
    host.shutdown().await;
}

#[tokio::test]
async fn daemon_routes_and_two_live_sse_events() {
    let dir = tempfile::tempdir().unwrap();
    let (host, client, proxy) = networks(
        dir.path(),
        Role::Desktop,
        ApiProtocol::new(state(&dir.path().join("db"))),
    )
    .await;
    let health = request(&proxy, reqwest::Method::GET, "/v1/health")
        .send()
        .await
        .unwrap();
    assert_eq!(health.status(), 200);
    assert_eq!(health.json::<Value>().await.unwrap()["status"], "ok");
    let bots = request(&proxy, reqwest::Method::GET, "/v1/bots")
        .send()
        .await
        .unwrap();
    assert_eq!(bots.status(), 200);
    assert_eq!(
        bots.json::<Vec<Value>>().await.unwrap(),
        Vec::<Value>::new()
    );
    let bot: Value = request(&proxy, reqwest::Method::POST, "/v1/bots").json(&json!({"profile_toml":"[bot]\nid='test'\nname='Test'\n[models]\nexecutor='fake/echo'"})).send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    let chat: Value = request(&proxy, reqwest::Method::POST, "/v1/chats")
        .json(&json!({"bot_id":bot["id"]}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let events = reqwest::Client::new()
        .get(format!(
            "{}/v1/events?token={}",
            proxy.base_url, proxy.token
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(events.status(), 200);
    assert!(
        events.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );
    let mut events = events.bytes_stream();
    let response = request(
        &proxy,
        reqwest::Method::POST,
        &format!("/v1/chats/{}/messages", chat["id"].as_str().unwrap()),
    )
    .json(&json!({"text":"hello streaming world"}))
    .send()
    .await
    .unwrap();
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );
    let mut text = String::new();
    tokio::time::timeout(Duration::from_secs(10), async {
        while text.matches("\nevent:").count() + usize::from(text.starts_with("event:")) < 2 {
            text.push_str(&String::from_utf8_lossy(
                &events.next().await.unwrap().unwrap(),
            ));
        }
    })
    .await
    .unwrap();
    drop(events);
    stop(host, client, proxy).await;
}

#[tokio::test]
async fn proxy_guard_and_credential_stripping() {
    let dir = tempfile::tempdir().unwrap();
    let app = Router::new()
        .route("/v1/health", get(|| async { StatusCode::OK }))
        .fallback(|req: Request<Body>| async move {
            assert!(!req.headers().contains_key("authorization"));
            assert!(!req.headers().contains_key("proxy-authorization"));
            assert!(!req.headers().contains_key("x-hop"));
            assert!(!req.uri().query().unwrap_or_default().contains("token"));
            assert_eq!(req.uri().query(), Some("keep=yes"));
            StatusCode::OK
        });
    let (host, client, proxy) =
        networks(dir.path(), Role::Desktop, ApiProtocol::for_test(app)).await;
    for path in ["/v1/bots", "/v1/events"] {
        assert_eq!(
            reqwest::get(format!("{}{path}", proxy.base_url))
                .await
                .unwrap()
                .status(),
            401
        );
    }
    for (header, value) in [("Host", "evil.example"), ("Origin", "https://evil.example")] {
        assert_eq!(
            request(&proxy, reqwest::Method::GET, "/v1/bots")
                .header(header, value)
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
    }
    let preflight = reqwest::Client::new()
        .request(
            reqwest::Method::OPTIONS,
            format!("{}/v1/bots", proxy.base_url),
        )
        .header("Origin", "tauri://localhost")
        .send()
        .await
        .unwrap();
    assert_eq!(preflight.status(), 204);
    assert_eq!(
        preflight.headers()["access-control-allow-origin"],
        "tauri://localhost"
    );
    assert_eq!(
        request(
            &proxy,
            reqwest::Method::GET,
            "/v1/events?%74oken=hidden&token=local&keep=yes"
        )
        .header("Connection", "x-hop")
        .header("x-hop", "secret")
        .header("proxy-authorization", "secret")
        .send()
        .await
        .unwrap()
        .status(),
        200
    );
    stop(host, client, proxy).await;
}

#[tokio::test]
async fn phone_scope_and_other_roles_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let st = state(&dir.path().join("db"));
    let (host, client, proxy) =
        networks(dir.path(), Role::Phone, ApiProtocol::new(st.clone())).await;
    assert_eq!(
        request(&proxy, reqwest::Method::GET, "/v1/bots")
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    for (method, path) in [
        (reqwest::Method::POST, "/v1/host/import"),
        (reqwest::Method::POST, "/v1/host/handover"),
        (reqwest::Method::POST, "/v1/secrets/name"),
        (reqwest::Method::PUT, "/v1/secrets/name"),
        (reqwest::Method::POST, "/v1/%73ecrets/name"),
    ] {
        let response = request(&proxy, method, path).send().await.unwrap();
        assert_eq!(response.status(), 403);
        assert_eq!(
            response.json::<Value>().await.unwrap()["error"]["code"],
            "forbidden"
        );
    }
    stop(host, client, proxy).await;
    for role in [
        None,
        Some(daemon_api::PeerRole::Node),
        Some(daemon_api::PeerRole::Host),
    ] {
        let mut req = Request::builder()
            .uri("/v1/health")
            .body(Body::empty())
            .unwrap();
        if let Some(role) = role {
            req.extensions_mut().insert(daemon_api::PeerIdentity {
                key: "test".into(),
                role,
            });
        }
        assert_eq!(
            daemon_api::peer_router(st.clone())
                .oneshot(req)
                .await
                .unwrap()
                .status(),
            403
        );
    }
}

#[tokio::test]
async fn unpaired_and_revoked_peers_refused_then_repair_recovers() {
    let dir = tempfile::tempdir().unwrap();
    let (host, client, mut proxy) = networks(
        dir.path(),
        Role::Desktop,
        ApiProtocol::new(state(&dir.path().join("db"))),
    )
    .await;
    let outsider = PeerNetwork::start(config(dir.path(), "outsider", Role::Desktop), None)
        .await
        .unwrap();
    assert!(outsider.connect(&host.id(), ALPN_API).await.is_err());
    assert!(ApiProxy::start(outsider.clone(), host.id()).await.is_err());
    // Also bypass the client's paired check and exercise the server's ingress.
    let raw = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .bind_addr(std::net::SocketAddr::from(([127, 0, 0, 1], 0)))
        .unwrap()
        .bind()
        .await
        .unwrap();
    let conn = raw.connect(host.addr(), ALPN_API).await.unwrap();
    let reason = tokio::time::timeout(Duration::from_secs(5), conn.closed())
        .await
        .unwrap();
    assert!(
        matches!(reason, iroh::endpoint::ConnectionError::ApplicationClosed(ref e) if e.error_code == peer_network::proto::CLOSE_NOT_PAIRED.into())
    );
    raw.close().await;
    let events = request(&proxy, reqwest::Method::GET, "/v1/events")
        .send()
        .await
        .unwrap();
    let mut events = events.bytes_stream();
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
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(Ok(_)) = events.next().await {}
    })
    .await
    .unwrap();
    // Revoked identity is refused by transport even though the client retains
    // its old pairing record.
    let conn = client.connect(&host.id(), ALPN_API).await.unwrap();
    let reason = tokio::time::timeout(Duration::from_secs(5), conn.closed())
        .await
        .unwrap();
    assert!(
        matches!(reason, iroh::endpoint::ConnectionError::ApplicationClosed(ref e)
        if e.error_code == peer_network::proto::CLOSE_NOT_PAIRED.into())
    );
    pair(&host, &client, Role::Desktop).await;
    tokio::time::timeout(
        Duration::from_secs(15),
        proxy.state.wait_for(|s| *s == ProxyState::Connected),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        request(&proxy, reqwest::Method::GET, "/v1/bots")
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    outsider.shutdown().await;
    stop(host, client, proxy).await;
}

struct Active(Arc<std::sync::atomic::AtomicUsize>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[tokio::test]
async fn five_megabyte_upload_cancel_and_stream_limits() {
    let dir = tempfile::tempdir().unwrap();
    let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let app = Router::new()
        .route("/v1/health", get(|| async { StatusCode::OK }))
        .route(
            "/upload",
            post(|body: Body| async move {
                let mut body = body.into_data_stream();
                let mut bytes = 0;
                while let Some(chunk) = body.next().await {
                    bytes += chunk.unwrap().len();
                }
                bytes.to_string()
            }),
        )
        .route(
            "/events",
            get({
                let active = active.clone();
                move || {
                    let active = active.clone();
                    async move {
                        active.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        let guard = Active(active);
                        Body::from_stream(stream::unfold(guard, |guard| async move {
                            tokio::time::sleep(Duration::from_millis(10)).await;
                            Some((Ok::<_, std::io::Error>("data: alive\n\n"), guard))
                        }))
                        .into_response()
                    }
                }
            }),
        );
    let (host, client, proxy) =
        networks(dir.path(), Role::Desktop, ApiProtocol::for_test(app)).await;
    let chunks = stream::iter((0..80).map(|_| Ok::<_, std::io::Error>(vec![42u8; 65536])));
    let result = request(&proxy, reqwest::Method::POST, "/upload")
        .body(reqwest::Body::wrap_stream(chunks))
        .send()
        .await
        .unwrap();
    assert_eq!(result.status(), 200);
    assert_eq!(result.text().await.unwrap(), (5 * 1024 * 1024).to_string());
    // More sequential cancellations than the stream cap catches leaked permits.
    for _ in 0..40 {
        let response = request(&proxy, reqwest::Method::GET, "/events")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        drop(response);
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        while active.load(std::sync::atomic::Ordering::SeqCst) != 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        request(&proxy, reqwest::Method::POST, "/upload")
            .header("content-length", (crate::MAX_REQUEST_BYTES + 1).to_string())
            .send()
            .await
            .unwrap()
            .status(),
        413
    );
    stop(host, client, proxy).await;
}

#[tokio::test]
async fn streaming_request_cap_includes_chunked_bodies() {
    let body = Body::from_stream(stream::iter(
        (0..257).map(|_| Ok::<_, std::io::Error>(vec![0u8; 65536])),
    ));
    assert!(crate::request_body(body).collect().await.is_err());
    let body = Body::from_stream(stream::pending::<Result<axum::body::Bytes, std::io::Error>>());
    // An idle upload cannot retain a stream indefinitely.
    assert!(
        tokio::time::timeout(Duration::from_secs(12), crate::request_body(body).collect())
            .await
            .unwrap()
            .is_err()
    );
}

#[tokio::test]
async fn peer_decision_audit_contains_key_and_name() {
    let dir = tempfile::tempdir().unwrap();
    let st = state(dir.path());
    let mut req = Request::builder()
        .method(Method::POST)
        .uri("/v1/approvals/not-a-uuid")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"decision":"deny"}"#))
        .unwrap();
    req.extensions_mut().insert(daemon_api::PeerIdentity {
        key: "paired-key".into(),
        role: daemon_api::PeerRole::Phone,
    });
    req.extensions_mut()
        .insert(daemon_api::peer::PeerName("Alice".into()));
    let response = daemon_api::peer_router(st.clone())
        .oneshot(req)
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    let storage = Storage::open(dir.path()).unwrap();
    let user = storage.list_users().unwrap().remove(0);
    let audit = storage.list_audit(user.id).unwrap();
    assert_eq!(audit.last().unwrap().actor, "phone:Alice");
    assert_eq!(audit.last().unwrap().detail["peer_key"], "paired-key");
}
#[tokio::test]
async fn concurrent_stream_cap_and_proxy_drop_release_bodies() {
    let dir = tempfile::tempdir().unwrap();
    let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let app = Router::new()
        .route("/v1/health", get(|| async { StatusCode::OK }))
        .route(
            "/events",
            get({
                let active = active.clone();
                move || {
                    let active = active.clone();
                    async move {
                        active.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        Body::from_stream(stream::unfold(Active(active), |guard| async move {
                            tokio::time::sleep(Duration::from_millis(10)).await;
                            Some((Ok::<_, std::io::Error>("data: alive\n\n"), guard))
                        }))
                    }
                }
            }),
        );
    let (host, client, proxy) =
        networks(dir.path(), Role::Desktop, ApiProtocol::for_test(app)).await;
    let mut responses = Vec::new();
    for _ in 0..crate::STREAMS {
        let response = request(&proxy, reqwest::Method::GET, "/events")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        responses.push(response);
    }
    let response = request(&proxy, reqwest::Method::GET, "/events")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 502);
    drop(proxy);
    tokio::time::timeout(Duration::from_secs(5), async {
        while active.load(std::sync::atomic::Ordering::SeqCst) != 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    drop(responses);
    client.shutdown().await;
    host.shutdown().await;
}

#[tokio::test]
async fn peer_http_header_read_deadline_closes_slow_stream() {
    use tokio::io::AsyncReadExt;
    let dir = tempfile::tempdir().unwrap();
    let (host, client, proxy) = networks(
        dir.path(),
        Role::Desktop,
        ApiProtocol::new(state(&dir.path().join("db"))),
    )
    .await;
    let conn = client.connect(&host.id(), ALPN_API).await.unwrap();
    let (mut send, mut recv) = conn.open_bi().await.unwrap();
    send.write_all(b"GET /v1/health HTTP/1.1\r\nHost:")
        .await
        .unwrap();
    let mut response = Vec::new();
    let result = tokio::time::timeout(
        Duration::from_secs(12),
        AsyncReadExt::read_to_end(&mut recv, &mut response),
    )
    .await
    .unwrap();
    assert!(
        result.is_err()
            || response.is_empty()
            || String::from_utf8_lossy(&response).contains("408")
    );
    // The timed-out stream did not kill the paired transport or its capacity.
    assert_eq!(
        request(&proxy, reqwest::Method::GET, "/v1/bots")
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    stop(host, client, proxy).await;
}

#[tokio::test]
async fn paired_node_and_host_roles_receive_http_forbidden() {
    for role in [Role::Node, Role::Host] {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config(dir.path(), "host", Role::Host);
        cfg.extra_protocols.push((
            ALPN_API.to_vec(),
            Arc::new(ApiProtocol::new(state(&dir.path().join("db")))),
        ));
        let host = PeerNetwork::start(cfg, None).await.unwrap();
        let client = PeerNetwork::start(config(dir.path(), "client", role), None)
            .await
            .unwrap();
        pair(&host, &client, role).await;
        let conn = client.connect(&host.id(), ALPN_API).await.unwrap();
        let (send, recv) = conn.open_bi().await.unwrap();
        let (mut sender, connection) = hyper::client::conn::http1::handshake(
            hyper_util::rt::TokioIo::new(tokio::io::join(recv, send)),
        )
        .await
        .unwrap();
        let driver = crate::AbortTask(tokio::spawn(async move {
            let _ = connection.await;
        }));
        let request = Request::builder()
            .uri("/v1/health")
            .header("Host", "remote")
            .body(Body::empty())
            .unwrap();
        let response = sender.send_request(request).await.unwrap();
        assert_eq!(response.status(), 403);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"]["code"], "forbidden");
        drop(driver);
        client.shutdown().await;
        host.shutdown().await;
    }
}
