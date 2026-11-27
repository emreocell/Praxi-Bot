//! Risk classes, per-bot access and spec download, against a fake server.

use std::sync::Arc;
use std::time::Duration;

use openapi_tools::{
    ApiSpec, ImportOptions, OpenApiConfig, OpenApiError, fetch_spec, fetch_spec_allowing_private,
    import_tools,
};
use secret_store::MemoryStore;
use serde_json::{Value, json};
use tool_registry::{Effect, PermissionCategory, Tool, ToolContext, ToolError};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SHOP: &str = include_str!("fixtures/shop.yaml");

fn cfg() -> OpenApiConfig {
    let mut c = OpenApiConfig::new(Arc::new(MemoryStore::new()));
    c.allow_private_hosts = true;
    c
}

fn shop_tools(server: &MockServer, cfg: &OpenApiConfig) -> Vec<Arc<dyn Tool>> {
    let opts = ImportOptions {
        base_url: Some(format!("{}/api", server.uri())),
        name_prefix: "shop".into(),
        ..ImportOptions::default()
    };
    import_tools(SHOP, &opts, cfg).unwrap().1
}

fn tool(tools: &[Arc<dyn Tool>], name: &str) -> Arc<dyn Tool> {
    tools
        .iter()
        .find(|t| t.spec().name == name)
        .unwrap_or_else(|| panic!("{name} missing"))
        .clone()
}

#[tokio::test]
async fn method_sets_the_effect_and_money_like_names_always_ask() {
    let server = MockServer::start().await;
    let tools = shop_tools(&server, &cfg());
    // (tool, effect, always asks, category)
    let want = [
        (
            "shop_listProducts",
            Effect::Read,
            false,
            PermissionCategory::Accounts,
        ),
        (
            "shop_createProduct",
            Effect::Write,
            false,
            PermissionCategory::Accounts,
        ),
        (
            "shop_deleteProduct",
            Effect::Destructive,
            false,
            PermissionCategory::Accounts,
        ),
        (
            "shop_createSession",
            Effect::Write,
            true,
            PermissionCategory::Money,
        ),
        (
            "shop_listInvoices",
            Effect::Read,
            true,
            PermissionCategory::Money,
        ),
        (
            "shop_settle",
            Effect::Write,
            true,
            PermissionCategory::Money,
        ),
        (
            "shop_getFile",
            Effect::Read,
            false,
            PermissionCategory::Accounts,
        ),
    ];
    for (name, effect, ask, category) in want {
        let t = tool(&tools, name);
        assert_eq!(t.spec().effect, effect, "{name}");
        assert_eq!(t.always_ask(), ask, "{name}");
        assert_eq!(t.spec().category, category, "{name}");
    }
}

#[tokio::test]
async fn the_gate_hides_the_tool_and_refuses_the_call_for_other_bots() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/products"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{"id": 1}])))
        .mount(&server)
        .await;
    let mut c = cfg();
    c.gate = Some(Arc::new(|tool, bot| {
        tool == "shop_listProducts" && bot == Some("bot-a")
    }));
    let tools = shop_tools(&server, &c);
    let list = tool(&tools, "shop_listProducts");
    assert!(list.visible_to(Some("bot-a")));
    assert!(!list.visible_to(Some("bot-b")));
    assert!(!list.visible_to(None));
    assert!(!tool(&tools, "shop_createProduct").visible_to(Some("bot-a")));

    let ctx = |bot: &str| ToolContext {
        bot_id: Some(bot.into()),
        ..ToolContext::default()
    };
    let err = list.call(&ctx("bot-b"), json!({})).await.unwrap_err();
    assert!(matches!(err, ToolError::PermissionDenied(_)), "{err}");
    assert!(server.received_requests().await.unwrap().is_empty());
    list.call(&ctx("bot-a"), json!({})).await.unwrap();
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn the_path_cannot_leave_the_base_url_host() {
    let server = MockServer::start().await;
    let other = MockServer::start().await;
    let tools = shop_tools(&server, &cfg());
    let del = tool(&tools, "shop_deleteProduct");
    // An id that looks like a URL or a traversal never changes host or path.
    let url = format!("{}/x", other.uri());
    for id in [url.as_str(), "//evil.example/x", "..", "a/../../b"] {
        let _ = del.call(&ToolContext::default(), json!({"id": id})).await;
    }
    assert!(other.received_requests().await.unwrap().is_empty());
    for req in server.received_requests().await.unwrap() {
        assert!(req.url.path().starts_with("/api/products/"), "{}", req.url);
    }
}

#[tokio::test]
async fn spec_download_reads_the_document() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/spec.yaml"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SHOP))
        .mount(&server)
        .await;
    let url = format!("{}/spec.yaml", server.uri());
    let text = fetch_spec_allowing_private(&url, 1 << 20, Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(
        ApiSpec::parse(&text, &ImportOptions::default())
            .unwrap()
            .title,
        "Shop"
    );
}

#[tokio::test]
async fn spec_download_refuses_private_hosts_by_default() {
    let server = MockServer::start().await;
    let err = fetch_spec(&server.uri(), 1 << 20, Duration::from_secs(5))
        .await
        .unwrap_err();
    assert!(matches!(err, OpenApiError::PrivateHost(_)), "{err}");
    assert!(server.received_requests().await.unwrap().is_empty());
    for bad in [
        "ftp://example.com/x",
        "file:///etc/passwd",
        "not a url",
        "https://u:p@example.com/x",
    ] {
        assert!(
            matches!(
                fetch_spec(bad, 1024, Duration::from_secs(1)).await,
                Err(OpenApiError::Fetch(_))
            ),
            "{bad}"
        );
    }
}

#[tokio::test]
async fn spec_download_is_capped_and_stays_on_the_host() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/big"))
        .respond_with(ResponseTemplate::new(200).set_body_string("x".repeat(5000)))
        .mount(&server)
        .await;
    let err = fetch_spec_allowing_private(
        &format!("{}/big", server.uri()),
        1000,
        Duration::from_secs(5),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(err, OpenApiError::SpecTooLarge { limit: 1000, .. }),
        "{err}"
    );

    let other = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/away"))
        .respond_with(ResponseTemplate::new(302).insert_header(
            "location",
            format!("http://localhost:{}/secret", other.address().port()),
        ))
        .mount(&server)
        .await;
    let err = fetch_spec_allowing_private(
        &format!("{}/away", server.uri()),
        1000,
        Duration::from_secs(5),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, OpenApiError::Fetch(_)), "{err}");
    assert!(other.received_requests().await.unwrap().is_empty());

    Mock::given(method("GET"))
        .and(path("/here"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/spec.json"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/spec.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(Value::Null))
        .mount(&server)
        .await;
    fetch_spec_allowing_private(
        &format!("{}/here", server.uri()),
        1000,
        Duration::from_secs(5),
    )
    .await
    .unwrap();
}
