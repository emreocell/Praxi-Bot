use axum::{
    Extension, Router,
    http::{Method, StatusCode},
    routing::get,
};
use support_logs::tracing::instrument::WithSubscriber;

use super::{TOKEN, call, send, setup};
use crate::{ApiError, PeerIdentity, PeerRole};

#[tokio::test]
async fn recent_logs_require_owner_and_deny_a_paired_desktop() {
    let t = setup();
    let r = send(
        &t.app,
        Method::GET,
        "/v1/logs/recent",
        None,
        &[
            ("authorization", &format!("Bearer {TOKEN}")),
            ("x-test-token-only", "true"),
        ],
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.json()["error"]["code"], "owner_key_required");
    let r = send(
        &t.app,
        Method::HEAD,
        "/v1/logs/recent",
        None,
        &[
            ("authorization", &format!("Bearer {TOKEN}")),
            ("x-test-token-only", "true"),
        ],
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let peer = crate::peer_router(t.state.clone()).layer(Extension(PeerIdentity {
        key: "paired-test-peer".into(),
        role: PeerRole::Desktop,
    }));
    let r = send(&peer, Method::GET, "/v1/logs/recent", None, &[]).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = call(&t, Method::GET, "/v1/logs/recent", None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["lines"], serde_json::json!([]));
    let r = call(&t, Method::GET, "/v1/logs/recent?level=wrong", None).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn failed_requests_log_once_with_templates_codes_and_generated_ids() {
    let dir = tempfile::tempdir().unwrap();
    let subscriber = support_logs::subscriber(dir.path(), "daemon", "trace").unwrap();
    async {
        let app = Router::new()
            .route(
                "/v1/probe/{id}",
                get(|| async {
                    ApiError::conflict("secret-canary@example.test C:/Users/canary/private")
                }),
            )
            .route(
                "/v1/denied",
                get(|| async { ApiError::forbidden("canary-token") }),
            )
            .route(
                "/v1/busy",
                get(|| async { ApiError::unavailable("sk-canary-key") }),
            )
            .layer(axum::middleware::from_fn(crate::logs::request_log));
        for (path, status) in [
            ("/v1/probe/private-id?token=canary", StatusCode::CONFLICT),
            ("/v1/denied", StatusCode::FORBIDDEN),
            ("/v1/busy", StatusCode::SERVICE_UNAVAILABLE),
        ] {
            let r = send(
                &app,
                Method::GET,
                path,
                None,
                &[("x-request-id", "canary-token")],
            )
            .await;
            assert_eq!(r.status, status);
            assert!(uuid::Uuid::parse_str(r.headers["x-request-id"].to_str().unwrap()).is_ok());
        }
    }
    .with_subscriber(subscriber)
    .await;
    let entries = support_logs::recent(dir.path(), None, None).unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].fields["route"], "/v1/probe/{id}");
    for (e, (status, code, level)) in entries.iter().zip([
        ("409", "conflict", "warn"),
        ("403", "forbidden", "warn"),
        ("503", "unavailable", "error"),
    ]) {
        assert_eq!(e.fields["method"], "GET");
        assert_eq!(e.fields["status"], status);
        assert_eq!(e.fields["error_code"], code);
        assert_eq!(e.level, level);
        assert!(e.fields.contains_key("request_id"));
    }
    let text = serde_json::to_string(&entries).unwrap();
    for canary in ["private-id", "canary", "Users", "token="] {
        assert!(!text.contains(canary));
    }
}
