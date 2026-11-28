//! `/v1/web/search*` against an in-memory `WebControl`.

use std::sync::Arc;

use async_trait::async_trait;
use axum::http::{Method, StatusCode};
use parking_lot::Mutex;
use serde_json::{Value, json};

use super::{T, TOKEN, call, call_token_only, send, setup};
use crate::web::{
    WebControl, WebControlError, WebSearchConfigDto, WebSearchConfigInput, WebSearchTestInput,
    WebSearchTestReport,
};

const SECRET_BRAVE: &str = "BSA-writeonly-secret-xyz";
const SECRET_TAVILY: &str = "tvly-writeonly-secret-abc";

struct FakeWeb {
    cfg: Mutex<WebSearchConfigDto>,
    brave_secret: Mutex<Option<String>>,
    tavily_secret: Mutex<Option<String>>,
    /// How many live probes reached the control.
    probes: Mutex<usize>,
}

impl FakeWeb {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            cfg: Mutex::new(WebSearchConfigDto {
                order: vec![
                    "searxng".into(),
                    "brave".into(),
                    "tavily".into(),
                    "duckduckgo".into(),
                    "duckduckgo_lite".into(),
                    "bing".into(),
                ],
                active_backends: vec!["duckduckgo".into(), "duckduckgo_lite".into(), "bing".into()],
                searxng_url: None,
                brave_key_set: false,
                tavily_key_set: false,
                firecrawl_url: None,
                firecrawl_key_set: false,
                firecrawl_scrape: false,
                firecrawl_allow_self_hosted_scrape: false,
                duckduckgo: true,
                duckduckgo_lite: true,
                bing: true,
                config_error: None,
                needs_provider: false,
            }),
            brave_secret: Mutex::new(None),
            tavily_secret: Mutex::new(None),
            probes: Mutex::new(0),
        })
    }
}

#[async_trait]
impl WebControl for FakeWeb {
    fn get(&self) -> WebSearchConfigDto {
        self.cfg.lock().clone()
    }

    async fn update(
        &self,
        input: WebSearchConfigInput,
    ) -> Result<WebSearchConfigDto, WebControlError> {
        if let Some(order) = &input.order
            && order.iter().any(|b| b == "unknown")
        {
            return Err(WebControlError::Invalid("unknown backend".into()));
        }
        let mut guard = self.cfg.lock();
        if let Some(order) = input.order {
            guard.order = order;
        }
        if let Some(url) = input.searxng_url {
            guard.searxng_url = if url.trim().is_empty() {
                None
            } else {
                Some(url)
            };
        }
        if input.clear_brave_key == Some(true) {
            *self.brave_secret.lock() = None;
            guard.brave_key_set = false;
        } else if let Some(k) = input.brave_key {
            *self.brave_secret.lock() = Some(k);
            guard.brave_key_set = true;
        }
        if input.clear_tavily_key == Some(true) {
            *self.tavily_secret.lock() = None;
            guard.tavily_key_set = false;
        } else if let Some(k) = input.tavily_key {
            *self.tavily_secret.lock() = Some(k);
            guard.tavily_key_set = true;
        }
        if let Some(v) = input.duckduckgo {
            guard.duckduckgo = v;
        }
        if let Some(v) = input.duckduckgo_lite {
            guard.duckduckgo_lite = v;
        }
        if let Some(v) = input.bing {
            guard.bing = v;
        }
        guard.active_backends = guard
            .order
            .iter()
            .filter(|name| match name.as_str() {
                "searxng" => guard.searxng_url.is_some(),
                "brave" => guard.brave_key_set,
                "tavily" => guard.tavily_key_set,
                "duckduckgo" => guard.duckduckgo,
                "duckduckgo_lite" => guard.duckduckgo_lite,
                "bing" => guard.bing,
                _ => false,
            })
            .cloned()
            .collect();
        Ok(guard.clone())
    }

    async fn test(
        &self,
        input: WebSearchTestInput,
    ) -> Result<WebSearchTestReport, WebControlError> {
        *self.probes.lock() += 1;
        if input.backend.as_deref() == Some("unknown") {
            return Err(WebControlError::Invalid("unknown backend".into()));
        }
        let backend = input.backend.unwrap_or_else(|| {
            self.cfg
                .lock()
                .active_backends
                .first()
                .cloned()
                .unwrap_or_default()
        });
        Ok(WebSearchTestReport {
            ok: !backend.is_empty(),
            query: input.query.unwrap_or_else(|| "rust programming".into()),
            backend: (!backend.is_empty()).then_some(backend),
            result_count: 5,
            elapsed_ms: 42,
            error: None,
            fallback_errors: vec!["duckduckgo: challenge".into()],
            blocked: false,
        })
    }
}

fn with_web() -> (T, Arc<FakeWeb>) {
    let t = setup();
    let web = FakeWeb::new();
    t.state.set_web(Arc::clone(&web) as Arc<dyn WebControl>);
    (t, web)
}

#[tokio::test]
async fn web_routes_need_bearer_token_and_good_origin() {
    let (t, _) = with_web();
    for (m, p, b) in [
        (Method::GET, "/v1/web/search", None),
        (Method::PUT, "/v1/web/search", Some(json!({"bing": true}))),
        (Method::POST, "/v1/web/search/test", Some(json!({}))),
    ] {
        let r = send(&t.app, m.clone(), p, b.clone(), &[]).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{m} {p}");
        let auth = format!("Bearer {TOKEN}");
        let r = send(
            &t.app,
            m.clone(),
            p,
            b.clone(),
            &[("origin", "https://evil.com"), ("authorization", &auth)],
        )
        .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{m} {p}");
    }
}

#[tokio::test]
async fn without_web_control_routes_return_503() {
    let t = setup();
    for (m, p, b) in [
        (Method::GET, "/v1/web/search", None),
        (Method::PUT, "/v1/web/search", Some(json!({"bing": true}))),
        (Method::POST, "/v1/web/search/test", Some(json!({}))),
    ] {
        let r = call(&t, m, p, b).await;
        assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{p}");
    }
}

#[tokio::test]
async fn update_web_search_stores_keys_write_only_and_never_echoes_them() {
    let (t, web) = with_web();
    let r = call(
        &t,
        Method::PUT,
        "/v1/web/search",
        Some(json!({
            "brave_key": SECRET_BRAVE,
            "tavily_key": SECRET_TAVILY,
            "order": ["tavily", "brave", "duckduckgo", "duckduckgo_lite", "bing", "searxng"]
        })),
    )
    .await;
    let r_text = String::from_utf8_lossy(&r.body);
    assert_eq!(r.status, StatusCode::OK, "{r_text}");
    assert!(
        !r_text.contains(SECRET_BRAVE),
        "brave key leaked in response"
    );
    assert!(
        !r_text.contains(SECRET_TAVILY),
        "tavily key leaked in response"
    );
    let v: Value = r.json();
    assert_eq!(v["brave_key_set"], json!(true));
    assert_eq!(v["tavily_key_set"], json!(true));
    assert_eq!(web.brave_secret.lock().as_deref(), Some(SECRET_BRAVE));
    assert_eq!(web.tavily_secret.lock().as_deref(), Some(SECRET_TAVILY));

    let g = call(&t, Method::GET, "/v1/web/search", None).await;
    assert_eq!(g.status, StatusCode::OK);
    let g_text = String::from_utf8_lossy(&g.body);
    assert!(!g_text.contains(SECRET_BRAVE));
    assert!(!g_text.contains(SECRET_TAVILY));
}

#[tokio::test]
async fn update_and_test_validate_keys_query_length_and_invalid_config() {
    let (t, _) = with_web();
    let r = call(
        &t,
        Method::PUT,
        "/v1/web/search",
        Some(json!({"brave_key": ""})),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    for key in [
        String::new(),
        "a".repeat(1025),
        "a\rb".into(),
        "a\nb".into(),
        "a\0b".into(),
    ] {
        let r = call(
            &t,
            Method::PUT,
            "/v1/web/search",
            Some(json!({"firecrawl_key": key})),
        )
        .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST);
    }

    let r = call(
        &t,
        Method::PUT,
        "/v1/web/search",
        Some(json!({"order": ["unknown"]})),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);

    let long_q = "a".repeat(401);
    let r = call(
        &t,
        Method::POST,
        "/v1/web/search/test",
        Some(json!({"query": long_q})),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    let r = call(
        &t,
        Method::POST,
        "/v1/web/search/test",
        Some(json!({"query": "rust"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let body: Value = r.json();
    assert_eq!(body["ok"], json!(true));
    assert_eq!(body["elapsed_ms"], json!(42));
}

/// Writing the search settings (keys, the `SearXNG` address) and probing them spend keys and make
/// the daemon contact an address the caller chose: both are owner-only. A holder of only the
/// general API token is refused before the handler runs, and nothing changes.
#[tokio::test]
async fn changing_and_probing_web_search_needs_the_owner_key() {
    let (t, web) = with_web();
    let before = web.get();

    let put = json!({
        "searxng_url": "http://169.254.169.254/",
        "brave_key": SECRET_BRAVE,
        "bing": false
    });
    let r = call_token_only(&t, Method::PUT, "/v1/web/search", Some(put.clone())).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.json()["error"]["code"], "owner_key_required");
    // the trailing-slash and double-slash spellings reach the same route and the same guard
    for p in ["/v1/web/search/", "//v1//web//search"] {
        let r = call_token_only(&t, Method::PUT, p, Some(put.clone())).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{p}");
    }
    let r = call_token_only(
        &t,
        Method::POST,
        "/v1/web/search/test",
        Some(json!({"query": "rust"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.json()["error"]["code"], "owner_key_required");
    assert_eq!(*web.probes.lock(), 0, "no probe may run for a token holder");
    assert_eq!(web.get().searxng_url, before.searxng_url);
    assert_eq!(web.get().bing, before.bing);
    assert!(web.brave_secret.lock().is_none(), "no key may be stored");

    // reading the (secret-free) settings stays open to the token: the UI and doctor show them
    let r = call_token_only(&t, Method::GET, "/v1/web/search", None).await;
    assert_eq!(r.status, StatusCode::OK);

    // the owner can do both
    let r = call(
        &t,
        Method::PUT,
        "/v1/web/search",
        Some(json!({"bing": false})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let r = call(
        &t,
        Method::POST,
        "/v1/web/search/test",
        Some(json!({"query": "rust"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(*web.probes.lock(), 1);
}
