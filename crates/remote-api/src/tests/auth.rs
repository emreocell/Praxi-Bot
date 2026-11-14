//! Authentication and identity on the view routes.
use super::view::{FRAMES, health_app, setup, view_response};
use super::{request, stop};
use crate::{ApiProtocol, Tuning};
use axum::{
    Router,
    body::Body,
    extract::{Extension, Request},
    http::HeaderMap,
    routing::get,
};
use daemon_api::PeerIdentity;
use live_view::MEDIA_TYPE;
use peer_network::{ALPN_API, Role};
use serde_json::{Value, json};

/// Echoes what the daemon side saw: transport identity and request headers.
fn whoami_app() -> Router {
    health_app()
        .route(
            "/v1/whoami",
            get(
                |Extension(who): Extension<PeerIdentity>, headers: HeaderMap| async move {
                    let names: Vec<_> = headers.keys().map(|k| k.as_str().to_owned()).collect();
                    axum::Json(json!({"key": who.key, "role": format!("{:?}", who.role), "headers": names}))
                },
            ),
        )
        .route(
            "/v1/view/targets",
            get(|| async { axum::Json(json!([])) }),
        )
        .route(
            "/v1/view/sessions/{id}/frames",
            get(|req: Request| async move {
                // Frames never see a query-string credential.
                assert!(req.uri().query().is_none(), "{:?}", req.uri());
                view_response(Body::empty())
            }),
        )
}

#[tokio::test]
async fn view_routes_need_the_local_token_loopback_host_and_header_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let (host, client, proxy) = setup(
        dir.path(),
        Role::Desktop,
        ApiProtocol::for_test(whoami_app()),
        Tuning::default(),
    )
    .await;
    let http = reqwest::Client::new();
    let paths = ["/v1/view/targets", FRAMES, "/v1/view/sessions/x"];
    for path in paths {
        let url = format!("{}{path}", proxy.base_url);
        // Missing and wrong local token.
        assert_eq!(http.get(&url).send().await.unwrap().status(), 401, "{path}");
        let wrong = http.get(&url).bearer_auth("not-the-token").send().await;
        assert_eq!(wrong.unwrap().status(), 401, "{path}");
        // The /v1/events query-token compatibility does not extend to view paths.
        let query = http
            .get(format!("{url}?token={}", proxy.token))
            .send()
            .await
            .unwrap();
        assert_eq!(query.status(), 401, "{path}");
        // A correct header plus a credential in the URL is refused, not stripped.
        for key in ["token", "%74oken", "access_token", "authorization"] {
            let both = http
                .get(format!("{url}?{key}=x"))
                .bearer_auth(&proxy.token)
                .send()
                .await
                .unwrap();
            assert_eq!(both.status(), 400, "{path}?{key}");
        }
        for (header, value) in [("Host", "evil.example"), ("Origin", "https://evil.example")] {
            let response = request(&proxy, reqwest::Method::GET, path)
                .header(header, value)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 403, "{path} {header}");
        }
    }
    // The takeover and release routes refuse URL credentials too.
    for path in ["/v1/control/takeover", "/v1/control/release"] {
        let response = request(&proxy, reqwest::Method::POST, &format!("{path}?token=x"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400, "{path}");
    }
    // Header credentials work and reach the route without the query.
    let ok = request(&proxy, reqwest::Method::GET, FRAMES)
        .header("Accept", MEDIA_TYPE)
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 200);
    stop(host, client, proxy).await;
}

#[tokio::test]
async fn identity_and_role_come_from_the_pairing_never_from_headers() {
    for role in [Role::Desktop, Role::Phone] {
        let dir = tempfile::tempdir().unwrap();
        let (host, client, proxy) = setup(
            dir.path(),
            role,
            ApiProtocol::for_test(whoami_app()),
            Tuning::default(),
        )
        .await;
        let spoof = [
            ("X-Peer-Key", "spoofed-key"),
            ("X-Peer-Role", "desktop"),
            ("X-Praxi-Role", "desktop"),
            ("X-Praxi-Peer", "spoofed-key"),
            ("X-Forwarded-For", "10.0.0.1"),
            ("Forwarded", "for=10.0.0.1"),
            ("X-Real-Ip", "10.0.0.1"),
        ];
        let mut call = request(&proxy, reqwest::Method::GET, "/v1/whoami");
        for (k, v) in spoof {
            call = call.header(k, v);
        }
        let seen: Value = call.send().await.unwrap().json().await.unwrap();
        assert_eq!(seen["key"], client.id().to_string());
        assert_eq!(
            seen["role"],
            if role == Role::Phone {
                "Phone"
            } else {
                "Desktop"
            }
        );
        let headers = seen["headers"].to_string().to_ascii_lowercase();
        for forbidden in [
            "authorization",
            "x-peer",
            "x-praxi",
            "x-forwarded",
            "forwarded",
            "x-real-ip",
        ] {
            assert!(
                !headers.contains(forbidden),
                "{forbidden} reached the daemon: {headers}"
            );
        }
        // A raw paired client (no proxy) cannot spoof either.
        let conn = client.connect(&host.id(), ALPN_API).await.unwrap();
        let (send, recv) = conn.open_bi().await.unwrap();
        let (mut sender, connection) = hyper::client::conn::http1::handshake(
            hyper_util::rt::TokioIo::new(tokio::io::join(recv, send)),
        )
        .await
        .unwrap();
        let _driver = crate::AbortTask(tokio::spawn(async move {
            let _ = connection.await;
        }));
        let req = axum::http::Request::builder()
            .uri("/v1/whoami")
            .header("Host", "remote")
            .header("X-Peer-Key", "spoofed-key")
            .header("X-Praxi-Role", "desktop")
            .body(Body::empty())
            .unwrap();
        let response = sender.send_request(req).await.unwrap();
        let bytes = http_body_util::BodyExt::collect(response.into_body())
            .await
            .unwrap()
            .to_bytes();
        let seen: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(seen["key"], client.id().to_string());
        assert!(!seen["headers"].to_string().contains("x-peer"));
        assert!(!seen["headers"].to_string().contains("x-praxi"));
        stop(host, client, proxy).await;
    }
}
