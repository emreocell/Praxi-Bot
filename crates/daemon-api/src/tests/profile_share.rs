//! `/v1/profile-share/*`: owner-only export/import, preview, audit.
use super::*;

const SRC: &str = r#"
[bot]
id = "src"
name = "Source"
persona = "Be useful."
[models]
executor = "fake/echo"
[permissions]
preset = "full"
allow_egress_after_reads = true
[tools]
allow = ["*"]
"#;

async fn bot_count(t: &T) -> usize {
    call(t, Method::GET, "/v1/bots", None)
        .await
        .json()
        .as_array()
        .unwrap()
        .len()
}

#[tokio::test]
async fn export_import_round_trip_is_owner_only_audited_and_clamped() {
    let t = setup();
    let r = call(
        &t,
        Method::POST,
        "/v1/bots",
        Some(json!({ "profile_toml": SRC })),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.json());
    let bot = r.json()["id"].as_str().unwrap().to_owned();

    // a token-only caller cannot export or import
    for (path, body) in [
        ("/v1/profile-share/export", json!({ "bot_id": bot })),
        ("/v1/profile-share/import", json!({ "contents": "{}" })),
    ] {
        let r = call_token_only(&t, Method::POST, path, Some(body)).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{path}");
    }

    let r = call(
        &t,
        Method::POST,
        "/v1/profile-share/export",
        Some(json!({ "bot_id": bot })),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    let contents = r.json()["contents"].as_str().unwrap().to_owned();
    assert!(!contents.contains("egress"), "{contents}");
    assert!(contents.contains("\"preset\": \"full\""));

    // preview with the default ceiling: lowered to read-only, nothing created
    let before = bot_count(&t).await;
    let r = call(
        &t,
        Method::POST,
        "/v1/profile-share/preview",
        Some(json!({ "contents": contents })),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    assert_eq!(r.json()["granted_level"], "read-only");
    assert_eq!(r.json()["requested_level"], "full");
    assert_eq!(bot_count(&t).await, before);

    // import with an explicit, lower ceiling
    let r = call(
        &t,
        Method::POST,
        "/v1/profile-share/import",
        Some(json!({ "contents": contents, "permission_level": "read-and-draft", "name": "Copy" })),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.json());
    let cfg = &r.json()["bot"]["config"];
    assert_eq!(cfg["permissions"]["preset"], "read-and-draft");
    assert!(cfg["permissions"].get("allow_egress_after_reads").is_none());
    assert_eq!(r.json()["bot"]["name"], "Copy");
    assert_eq!(bot_count(&t).await, before + 1);

    // garbage is a 422, not a bot
    let r = call(
        &t,
        Method::POST,
        "/v1/profile-share/import",
        Some(json!({ "contents": "{\"x\":1}" })),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(bot_count(&t).await, before + 1);

    let audit = t.state.storage().list_audit(t.state.user_id()).unwrap();
    let actions: Vec<&str> = audit.iter().map(|a| a.action.as_str()).collect();
    assert!(
        actions.contains(&"profile.export") && actions.contains(&"profile.import"),
        "{actions:?}"
    );
}
