//! `/v1/openapi-apps/*` against an in-memory `OpenApiAppsControl`: owner-only,
//! audit-first mutations, input checks, no credential in any answer or audit row.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use axum::http::{Method, StatusCode};
use serde_json::{Value, json};

use super::{call, call_token_only, send, setup};
use crate::openapi_apps::{
    OpenApiAppDto, OpenApiAppInput, OpenApiAppUpdate, OpenApiAppsControl, OpenApiAppsError,
    OpenApiAuthDto, OpenApiAuthKind, OpenApiEffect, OpenApiOperationDto,
};

const SECRET: &str = "sk-live-0123456789abcdef";

#[derive(Default)]
struct Fake {
    calls: AtomicUsize,
}

fn dto(id: &str) -> OpenApiAppDto {
    OpenApiAppDto {
        id: id.into(),
        name: "Shop".into(),
        title: "Shop API".into(),
        version: "1".into(),
        base_url: "https://shop.example/api".into(),
        spec_url: None,
        auth: OpenApiAuthDto {
            kind: OpenApiAuthKind::Bearer,
            name: None,
            username: None,
            secret_set: true,
        },
        operations: vec![OpenApiOperationDto {
            name: format!("api_{id}_listProducts"),
            method: "GET".into(),
            path: "/products".into(),
            summary: "List products".into(),
            tags: vec![],
            effect: OpenApiEffect::Read,
            always_ask: false,
            enabled: false,
            bots: None,
        }],
        skipped: vec![],
        warnings: vec![],
        security_hints: vec![],
        enabled_count: 0,
        max_enabled: 50,
    }
}

#[async_trait]
impl OpenApiAppsControl for Fake {
    fn list(&self) -> Vec<OpenApiAppDto> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        vec![dto("shop")]
    }
    fn get(&self, id: &str) -> Result<OpenApiAppDto, OpenApiAppsError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if id == "shop" {
            Ok(dto(id))
        } else {
            Err(OpenApiAppsError::NotFound(format!("app {id}")))
        }
    }
    async fn add(&self, input: OpenApiAppInput) -> Result<OpenApiAppDto, OpenApiAppsError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if input.spec_url.as_deref() == Some("https://down.example/spec") {
            return Err(OpenApiAppsError::Upstream("the server answered 503".into()));
        }
        if input.name == "bad" {
            return Err(OpenApiAppsError::Invalid("no usable server URL".into()));
        }
        Ok(dto("shop"))
    }
    async fn update(
        &self,
        id: &str,
        _input: OpenApiAppUpdate,
    ) -> Result<OpenApiAppDto, OpenApiAppsError> {
        self.get(id)
    }
    async fn refresh(&self, id: &str) -> Result<OpenApiAppDto, OpenApiAppsError> {
        self.get(id)
    }
    async fn remove(&self, id: &str) -> Result<(), OpenApiAppsError> {
        self.get(id).map(|_| ())
    }
    async fn forget_bot(&self, _bot_id: &str) -> Result<usize, OpenApiAppsError> {
        Ok(0)
    }
}

fn add_body() -> Value {
    json!({
        "name": "Shop",
        "spec_url": "https://shop.example/openapi.json",
        "auth": { "kind": "bearer", "secret": SECRET },
    })
}

fn routes() -> Vec<(Method, &'static str, Option<Value>)> {
    vec![
        (Method::GET, "/v1/openapi-apps", None),
        (Method::GET, "/v1/openapi-apps/shop", None),
        (Method::POST, "/v1/openapi-apps", Some(add_body())),
        (
            Method::PUT,
            "/v1/openapi-apps/shop",
            Some(json!({ "operations": [{ "name": "api_shop_listProducts", "bots": null }] })),
        ),
        (Method::POST, "/v1/openapi-apps/shop/refresh", None),
        (Method::DELETE, "/v1/openapi-apps/shop", None),
    ]
}

#[tokio::test]
async fn every_route_needs_the_owner() {
    let t = setup();
    let fake = Arc::new(Fake::default());
    t.state.set_openapi_apps(fake.clone());
    for (m, p, body) in routes() {
        let r = send(&t.app, m.clone(), p, body.clone(), &[]).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{m} {p}");
        let r = call_token_only(&t, m.clone(), p, body.clone()).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "token only: {m} {p}");
    }
    assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
    for (m, p, body) in routes() {
        let r = call(&t, m.clone(), p, body).await;
        assert!(r.status.is_success(), "{m} {p} -> {}", r.status);
    }
}

#[tokio::test]
async fn without_a_manager_the_routes_say_unavailable() {
    let t = setup();
    let r = call(&t, Method::GET, "/v1/openapi-apps", None).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn mutations_are_audited_without_the_credential_and_the_answer_never_has_it() {
    let t = setup();
    t.state.set_openapi_apps(Arc::new(Fake::default()));
    for (m, p, body) in routes() {
        let r = call(&t, m, p, body).await;
        assert!(!String::from_utf8_lossy(&r.body).contains(SECRET), "{p}");
    }
    let audit = t.state.storage().list_audit(t.state.user_id()).unwrap();
    let actions: Vec<&str> = audit.iter().map(|a| a.action.as_str()).collect();
    for want in [
        "openapi_app.added",
        "openapi_app.updated",
        "openapi_app.refreshed",
        "openapi_app.removed",
    ] {
        assert!(actions.contains(&want), "{want} missing in {actions:?}");
    }
    for row in &audit {
        assert!(!row.detail.to_string().contains(SECRET), "{}", row.action);
    }
}

#[tokio::test]
async fn bad_input_is_refused_before_the_manager_sees_it() {
    let t = setup();
    let fake = Arc::new(Fake::default());
    t.state.set_openapi_apps(fake.clone());
    let post = |body: Value| call(&t, Method::POST, "/v1/openapi-apps", Some(body));
    // Neither or both sources.
    assert_eq!(
        post(json!({"name": "x"})).await.status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        post(
            json!({"name": "x", "spec_url": "https://a.example/s", "spec_text": "openapi: 3.0.0"})
        )
        .await
        .status,
        StatusCode::BAD_REQUEST
    );
    // Empty name, a credential with a line break, an unknown field.
    assert_eq!(
        post(json!({"name": " ", "spec_text": "x"})).await.status,
        StatusCode::BAD_REQUEST
    );
    let r =
        post(json!({"name": "x", "spec_text": "x", "auth": {"kind": "bearer", "secret": "a\nb"}}))
            .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(!String::from_utf8_lossy(&r.body).contains("a\nb"));
    assert_eq!(
        post(json!({"name": "x", "spec_text": "x", "extra": 1}))
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
    for id in ["Shop", "a-b", "a%2Fb", "x".repeat(40).as_str()] {
        let r = call(&t, Method::GET, &format!("/v1/openapi-apps/{id}"), None).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{id}");
    }
    assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn manager_errors_map_to_their_statuses() {
    let t = setup();
    t.state.set_openapi_apps(Arc::new(Fake::default()));
    let post = |body: Value| call(&t, Method::POST, "/v1/openapi-apps", Some(body));
    let r = post(json!({"name": "bad", "spec_text": "x"})).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(r.json()["error"]["code"], "bad_request");
    let r = post(json!({"name": "ok", "spec_url": "https://down.example/spec"})).await;
    assert_eq!(r.status, StatusCode::BAD_GATEWAY);
    assert_eq!(r.json()["error"]["code"], "unavailable");
    let r = call(&t, Method::GET, "/v1/openapi-apps/nope", None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}
