//! Risk classes: decided from the catalogue entry, mapped to effect, permission
//! category and asking, and never changeable by the model.

use serde_json::{Value, json};
use tool_registry::{Effect, PermissionCategory, ToolError};
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

use super::{Harness, connected, ctx, harness};
use crate::Risk;

fn item(slug: &str, extra: Value) -> Value {
    let mut v = json!({"slug": slug, "description": slug, "toolkit": {"slug": "github"},
        "version": "20260901_00",
        "input_parameters": {"type": "object", "properties": {"x": {"type": "string"}}}});
    if let Value::Object(more) = extra {
        v.as_object_mut().unwrap().extend(more);
    }
    v
}

/// One fake catalogue entry per class, plus the awkward ones.
async fn with_catalog(h: &Harness) {
    let items = vec![
        item("GITHUB_LIST_ISSUES", json!({"http_method": "GET"})),
        item("GITHUB_CREATE_ISSUE", json!({})),
        item("GITHUB_ODD_THING", json!({})),
        item("GITHUB_DELETE_REPOSITORY", json!({"http_method": "DELETE"})),
        item("GITHUB_PAY_INVOICE", json!({})),
        item(
            "GITHUB_SEARCH_REPOSITORIES",
            json!({"http_method": "POST", "tags": ["readOnlyHint"]}),
        ),
        item("GITHUB_LIST_STARRED", json!({"http_method": "POST"})),
        item("GITHUB_LIST_WATCHED", json!({})),
        item("GITHUB_CREATE_INVOICE", json!({"http_method": "POST"})),
        item(
            "GITHUB_FIND_FORKS",
            json!({"http_method": "POST", "tags": ["read-only-hint"]}),
        ),
    ];
    Mock::given(path("/tools"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": items})))
        .with_priority(2)
        .mount(&h.server)
        .await;
    h.svc.forget_discovery("github");
    h.svc.sync_tools("github").await;
}

async fn catalog_harness() -> Harness {
    let h = harness().await;
    connected(&h).await;
    with_catalog(&h).await;
    h
}

async fn mount_execute(h: &Harness) {
    Mock::given(method("POST"))
        .and(wiremock::matchers::path_regex("^/tools/execute/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"successful": true, "data": {"ok": 1}})),
        )
        .mount(&h.server)
        .await;
}

async fn executed(h: &Harness) -> usize {
    h.server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path().starts_with("/tools/execute/"))
        .count()
}

#[tokio::test]
async fn every_class_maps_to_its_effect_category_and_asking() {
    let h = catalog_harness().await;
    let classes = h.svc.toolkit_actions("github").await.unwrap();
    let class_of = |n: &str| classes.iter().find(|a| a.name == n).unwrap().class;
    assert_eq!(class_of("list_issues"), Risk::Read);
    assert_eq!(class_of("create_issue"), Risk::Write);
    assert_eq!(
        class_of("odd_thing"),
        Risk::Write,
        "ambiguous falls to write"
    );
    assert_eq!(class_of("delete_repository"), Risk::Destructive);
    assert_eq!(class_of("pay_invoice"), Risk::Money);
    assert_eq!(
        class_of("search_repositories"),
        Risk::Read,
        "a POST that says it is read-only"
    );
    assert_eq!(
        class_of("list_starred"),
        Risk::Write,
        "a POST lookup without the hint"
    );

    assert_eq!(
        class_of("list_watched"),
        Risk::Write,
        "a lookup name with no method from Composio is not a read"
    );
    assert_eq!(class_of("create_invoice"), Risk::Money);
    assert_eq!(
        class_of("find_forks"),
        Risk::Read,
        "a hyphenated read-only tag is normalised"
    );

    let acc = PermissionCategory::Accounts;
    for (action, effect, category, asks) in [
        ("list_issues", Effect::Read, acc, false),
        ("search_repositories", Effect::Read, acc, false),
        ("create_issue", Effect::Write, acc, false),
        ("odd_thing", Effect::Write, acc, false),
        ("list_starred", Effect::Write, acc, false),
        ("delete_repository", Effect::Destructive, acc, false),
        (
            "pay_invoice",
            Effect::Destructive,
            PermissionCategory::Money,
            true,
        ),
    ] {
        let t = h.registry.get(&format!("app:github/{action}")).unwrap();
        assert_eq!(t.spec().effect, effect, "{action}");
        assert_eq!(t.spec().category, category, "{action}");
        assert_eq!(t.always_ask(), asks, "{action}");
    }
}

#[tokio::test]
async fn the_class_is_shown_in_the_preview_and_by_find() {
    let h = catalog_harness().await;
    let preview = h.svc.preview_actions("github").await.unwrap();
    let of = |n: &str| preview.iter().find(|a| a.name == n).unwrap().class;
    assert_eq!(of("list_issues"), Risk::Read);
    assert_eq!(of("pay_invoice"), Risk::Money);
    let find = h.registry.get("apps_find_action").unwrap();
    let out = find
        .call(&ctx(), json!({"app": "github", "query": "pay invoice"}))
        .await
        .unwrap();
    assert!(format!("{:?}", out.content).contains("\\\"class\\\":\\\"money\\\""));
}

#[tokio::test]
async fn reads_and_writes_run_without_a_card_but_a_payment_never_does() {
    let h = catalog_harness().await;
    mount_execute(&h).await;
    let args = json!({"x": "v"});
    for action in [
        "list_issues",
        "create_issue",
        "odd_thing",
        "delete_repository",
    ] {
        let t = h.registry.get(&format!("app:github/{action}")).unwrap();
        t.call(&ctx(), args.clone())
            .await
            .unwrap_or_else(|e| panic!("{action}: {e}"));
    }
    assert_eq!(executed(&h).await, 4);
    let pay = h.registry.get("app:github/pay_invoice").unwrap();
    assert!(matches!(
        pay.call(&ctx(), args.clone()).await,
        Err(ToolError::PermissionDenied(_))
    ));
    assert_eq!(executed(&h).await, 4, "nothing was sent for the payment");
    // With the card it runs.
    pay.approval_details(&ctx(), &args).await.unwrap();
    pay.call(&ctx(), args).await.unwrap();
    assert_eq!(executed(&h).await, 5);
}

#[tokio::test]
async fn the_model_cannot_name_a_class() {
    let h = catalog_harness().await;
    mount_execute(&h).await;
    // A class smuggled into the arguments of a direct tool is an unknown argument.
    let pay = h.registry.get("app:github/pay_invoice").unwrap();
    for smuggled in [
        json!({"x": "v", "class": "read"}),
        json!({"x": "v", "risk": "read"}),
    ] {
        assert!(matches!(
            pay.call(&ctx(), smuggled).await,
            Err(ToolError::InvalidInput(_))
        ));
    }
    // The generic tools read the class from the catalogue, whatever the call says.
    let run = h.registry.get("apps_run_action").unwrap();
    let money = json!({"app": "github", "action": "pay_invoice", "class": "read",
        "arguments": {"x": "v"}});
    assert!(run.always_ask_for(&money));
    assert!(run.always_ask_for(&json!({"app": "github", "action": "not_discovered"})));
    assert!(
        !run.always_ask_for(&json!({"app": "github", "action": "create_issue", "class": "money"}))
    );
    // The read tool refuses everything that is not a read, with or without a card.
    let read = h.registry.get("apps_read_action").unwrap();
    assert!(!read.always_ask_for(&money));
    for action in [
        "pay_invoice",
        "create_issue",
        "odd_thing",
        "delete_repository",
    ] {
        let input = json!({"app": "github", "action": action, "arguments": {"x": "v"}});
        assert!(
            matches!(
                read.call(&ctx(), input.clone()).await,
                Err(ToolError::InvalidInput(_))
            ),
            "{action}"
        );
        assert!(
            matches!(
                read.approval_details(&ctx(), &input).await,
                Err(ToolError::InvalidInput(_))
            ),
            "{action}"
        );
    }
    assert_eq!(executed(&h).await, 0);
    let ok = json!({"app": "github", "action": "list_issues", "arguments": {"x": "v"}});
    read.call(&ctx(), ok).await.unwrap();
    assert_eq!(executed(&h).await, 1);
    // The generic run tool will not send a payment nobody was asked about.
    assert!(run.call(&ctx(), money).await.is_err());
    assert_eq!(executed(&h).await, 1);
    // Effects of the generic pair.
    assert_eq!(read.spec().effect, Effect::Read);
    assert_eq!(run.spec().effect, Effect::Destructive);
}

#[tokio::test]
async fn an_action_that_became_riskier_after_registration_is_not_run_unasked() {
    let h = catalog_harness().await;
    mount_execute(&h).await;
    let t = h.registry.get("app:github/list_issues").unwrap();
    assert_eq!(t.spec().effect, Effect::Read);
    // The catalogue now says the same slug is a DELETE.
    Mock::given(path("/tools"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": [
            item("GITHUB_LIST_ISSUES", json!({"http_method": "DELETE"}))]})))
        .with_priority(1)
        .mount(&h.server)
        .await;
    h.svc.forget_discovery("github");
    let r = t.call(&ctx(), json!({"x": "v"})).await;
    assert!(matches!(r, Err(ToolError::PermissionDenied(_))), "{r:?}");
    assert_eq!(executed(&h).await, 0);
}
