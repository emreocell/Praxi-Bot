//! `POST /v1/connection-test/provider` against a fake provider server, one case per outcome.

use std::sync::Arc;
use std::time::Duration;

use agent_core::{LoopConfig, Providers, SharedRedactor};
use axum::http::{Method, StatusCode};
use provider_catalog::{
    Auth, CompatFlags, ModelCapabilities, OpenAiCompatConfig, OpenAiCompatProvider, ProviderEntry,
    ProviderKind,
};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::{T, call, setup_with_config};

const KEY: &str = "sk-test-connection-canary-12345";

fn provider_at(base: &str, models_endpoint: bool) -> Arc<OpenAiCompatProvider> {
    let entry = ProviderEntry {
        id: "fake".into(),
        kind: ProviderKind::OpenAiCompatible,
        base_url: base.into(),
        auth: Auth::None,
        capabilities: ModelCapabilities {
            tools: true,
            vision: false,
            streaming: true,
            json_mode: false,
        },
        compat_flags: CompatFlags::default(),
        models_endpoint: models_endpoint.then(|| format!("{base}/models")),
        models: Vec::new(),
    };
    let cfg = OpenAiCompatConfig {
        max_retries: 0,
        connect_timeout: Duration::from_secs(2),
        timeout: Duration::from_secs(2),
        ..OpenAiCompatConfig::default()
    };
    Arc::new(OpenAiCompatProvider::with_api_key(entry, cfg, Some(KEY.into())).unwrap())
}

fn app_with(base: &str) -> T {
    let redactor = SharedRedactor::default();
    redactor.register(KEY);
    let t = setup_with_config(
        |_| {},
        LoopConfig {
            redactor: Some(redactor),
            ..LoopConfig::default()
        },
    );
    t.agent
        .set_providers(Providers::single(provider_at(base, true)));
    t
}

async fn run(t: &T) -> Value {
    let r = call(
        t,
        Method::POST,
        "/v1/connection-test/provider",
        Some(json!({"id": "fake"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json());
    r.json()
}

#[tokio::test]
async fn every_provider_outcome_is_typed_and_never_echoes_the_key() {
    // (status, body, expected outcome)
    let cases = [
        (
            401,
            json!({"error": {"code": "invalid_api_key", "message": format!("bad key {KEY}")}}),
            "bad_key",
        ),
        (403, json!({"error": {"message": "forbidden"}}), "bad_key"),
        (
            400,
            json!({"error": {"status": "INVALID_ARGUMENT", "message": "API key not valid. Please pass a valid API key."}}),
            "bad_key",
        ),
        (
            429,
            json!({"error": {"code": "insufficient_quota", "message": "You exceeded your current quota"}}),
            "quota_exceeded",
        ),
        (
            404,
            json!({"error": {"message": "no such route"}}),
            "wrong_endpoint",
        ),
        (
            503,
            json!({"error": {"message": "overloaded"}}),
            "provider_outage",
        ),
        (418, json!({"error": {"message": "teapot"}}), "unknown"),
    ];
    for (status, body, want) in cases {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(status).set_body_json(body))
            .mount(&server)
            .await;
        let r = run(&app_with(&server.uri())).await;
        assert_eq!(r["outcome"], want, "status {status}: {r}");
        assert!(!r.to_string().contains(KEY), "key echoed for {status}: {r}");
        if matches!(status, 400 | 404 | 418 | 503) {
            // statuses the provider layer keeps are passed on for the expandable details
            assert_eq!(r["http_status"], status, "{r}");
            assert!(
                r["details"].as_str().unwrap().contains(&status.to_string()),
                "{r}"
            );
        }
    }
}

#[tokio::test]
async fn provider_ok_wrong_endpoint_and_no_network() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": [{"id": "m1"}]})))
        .mount(&server)
        .await;
    assert_eq!(run(&app_with(&server.uri())).await["outcome"], "ok");

    // A 200 that is not a model list: the base URL points at some other site.
    let other = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"hello": "world"})))
        .mount(&other)
        .await;
    assert_eq!(
        run(&app_with(&other.uri())).await["outcome"],
        "wrong_endpoint"
    );

    // Nothing listens there.
    let dead = {
        // A port that was just free and has nothing listening (a dropped MockServer is pooled
        // and would still answer).
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", l.local_addr().unwrap())
    };
    assert_eq!(run(&app_with(&dead)).await["outcome"], "no_network");

    // Unknown provider id.
    let t = app_with(&dead);
    let r = call(
        &t,
        Method::POST,
        "/v1/connection-test/provider",
        Some(json!({"id": "nope"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}
