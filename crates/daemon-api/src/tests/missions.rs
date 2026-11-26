//! `/v1/missions`: owner-only, off until enabled, Full level only, and the blocked-items list
//! is answered in one request.

use axum::http::{Method, StatusCode};
use serde_json::{Value, json};

use super::{PROFILE, T, call, call_token_only, setup};

const FULL_PROFILE: &str = r#"
[bot]
id = "tester"
name = "Tester"

[models]
executor = "fake/echo"

[permissions]
preset = "full"
"#;

async fn bot(t: &T, profile: &str, name: &str) -> String {
    let r = call(
        t,
        Method::POST,
        "/v1/bots",
        Some(json!({"profile_toml": profile, "name": name})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.json());
    r.json()["id"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn missions_answer_503_until_enabled_and_changes_are_owner_only() {
    let t = setup();
    let r = call(&t, Method::GET, "/v1/missions", None).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    t.state.enable_missions().unwrap();
    let r = call(&t, Method::GET, "/v1/missions", None).await;
    assert_eq!((r.status, r.json()), (StatusCode::OK, json!([])));

    let id = bot(&t, FULL_PROFILE, "Ada").await;
    let body = json!({"bot_id": id, "goal": "Get backlinks for my site"});
    for (m, path) in [
        (Method::POST, "/v1/missions".to_owned()),
        (
            Method::POST,
            format!("/v1/missions/{}/pause", uuid::Uuid::nil()),
        ),
        (
            Method::POST,
            format!("/v1/missions/{}/resume", uuid::Uuid::nil()),
        ),
        (
            Method::POST,
            format!("/v1/missions/{}/cancel", uuid::Uuid::nil()),
        ),
        (
            Method::POST,
            format!("/v1/missions/{}/answers", uuid::Uuid::nil()),
        ),
    ] {
        let r = call_token_only(&t, m, &path, Some(body.clone())).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{path}");
        assert_eq!(r.json()["error"]["code"], "owner_key_required", "{path}");
    }
    // Owner key but the supervisor is not running (not the leader, safe mode): 503, nothing created.
    let r = call(&t, Method::POST, "/v1/missions", Some(body)).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    let r = call(&t, Method::GET, "/v1/missions", None).await;
    assert_eq!(r.json(), json!([]));
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one ordered walk through the routes
async fn a_mission_needs_a_full_level_bot_a_goal_and_can_be_paused_and_stopped() {
    let t = setup();
    t.state.enable_missions().unwrap();
    t.state.start_missions();
    let low = bot(&t, PROFILE, "Low").await;
    let r = call(
        &t,
        Method::POST,
        "/v1/missions",
        Some(json!({"bot_id": low, "goal": "do it"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(
        r.json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Full level")
    );

    let full = bot(&t, FULL_PROFILE, "Ada").await;
    for goal in ["", "   "] {
        let r = call(
            &t,
            Method::POST,
            "/v1/missions",
            Some(json!({"bot_id": full, "goal": goal})),
        )
        .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{goal:?}");
    }
    let r = call(
        &t,
        Method::POST,
        "/v1/missions",
        Some(
            json!({"bot_id": full, "goal": "Get backlinks for my site", "budgets": {
            "max_sessions": 0, "max_model_steps": 10, "max_minutes": 10,
            "max_cost_usd": null, "max_stalled_sessions": 2, "max_failed_sessions": 2}}),
        ),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::BAD_REQUEST,
        "invalid budgets are refused"
    );

    let r = call(
        &t,
        Method::POST,
        "/v1/missions",
        Some(json!({"bot_id": full, "goal": "Get backlinks for my site", "title": "Backlinks"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    let d: Value = r.json();
    let id = d["mission"]["id"].as_str().unwrap().to_owned();
    assert_eq!(d["mission"]["title"], "Backlinks");
    assert_eq!(d["mission"]["goal"], "Get backlinks for my site");
    assert_eq!(d["mission"]["progress"]["left"], 0);

    // The mission has its own chat; its audit entry holds ids, not the goal.
    let chat = d["mission"]["chat_id"].as_str().unwrap();
    assert_eq!(
        call(&t, Method::GET, &format!("/v1/chats/{chat}/messages"), None)
            .await
            .status,
        StatusCode::OK
    );
    let audit = t.state.storage().list_audit(t.state.user_id()).unwrap();
    let created: Vec<_> = audit
        .iter()
        .filter(|a| a.action == "mission.created")
        .collect();
    assert_eq!(created.len(), 1);
    assert!(!created[0].detail.to_string().contains("backlinks"));

    let r = call(&t, Method::POST, &format!("/v1/missions/{id}/pause"), None).await;
    assert_eq!(
        (r.status, r.json()["mission"]["state"].clone()),
        (StatusCode::OK, json!("paused"))
    );
    let r = call(&t, Method::POST, &format!("/v1/missions/{id}/pause"), None).await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "pausing a paused mission is a no-op"
    );
    let active = call(&t, Method::GET, "/v1/missions?state=active", None)
        .await
        .json();
    assert_eq!(active.as_array().unwrap().len(), 1);
    let other_bot = call(&t, Method::GET, &format!("/v1/missions?bot_id={low}"), None)
        .await
        .json();
    assert_eq!(other_bot, json!([]));

    let r = call(&t, Method::POST, &format!("/v1/missions/{id}/cancel"), None).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    assert_eq!(r.json()["mission"]["state"], "cancelled");
    assert_eq!(r.json()["digest"]["outcome"], "cancelled");
    let r = call(&t, Method::POST, &format!("/v1/missions/{id}/resume"), None).await;
    assert_eq!(
        r.status,
        StatusCode::CONFLICT,
        "a stopped mission cannot be resumed"
    );
    let active = call(&t, Method::GET, "/v1/missions?state=active", None)
        .await
        .json();
    assert_eq!(active, json!([]));
    let r = call(
        &t,
        Method::GET,
        &format!("/v1/missions/{}", uuid::Uuid::now_v7()),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn blocked_items_are_answered_or_skipped_in_one_request() {
    let t = setup();
    t.state.enable_missions().unwrap();
    t.state.start_missions();
    let full = bot(&t, FULL_PROFILE, "Ada").await;
    let r = call(
        &t,
        Method::POST,
        "/v1/missions",
        Some(json!({"bot_id": full, "goal": "Sign up on sites"})),
    )
    .await;
    let id = r.json()["mission"]["id"].as_str().unwrap().to_owned();
    // Keep the supervisor out of the way while the ledger is edited.
    call(&t, Method::POST, &format!("/v1/missions/{id}/pause"), None).await;

    // The bot's tools wrote this ledger (here: directly through the store).
    let store = t.state.mission_store().unwrap();
    let items: Vec<missions::NewItem> = ["a.example", "b.example", "c.example", "d.example"]
        .iter()
        .map(|k| missions::NewItem {
            key: (*k).into(),
            kind: "site".into(),
            title: format!("Sign up on {k}"),
            payload: None,
        })
        .collect();
    store.upsert_items(&id, &items, 1).unwrap();
    let block = |k: &str, why: &str| {
        store
            .update_item(
                &id,
                k,
                &missions::ItemUpdate {
                    status: Some(missions::ItemStatus::Blocked),
                    blocked_reason: Some(why.into()),
                    ..missions::ItemUpdate::default()
                },
                false,
                2,
            )
            .unwrap();
    };
    block("a.example", "CAPTCHA on the sign-up form");
    block("b.example", "needs the owner's phone number");
    block("c.example", "2FA code needed");
    let d = call(&t, Method::GET, &format!("/v1/missions/{id}"), None)
        .await
        .json();
    assert_eq!(d["blocked"].as_array().unwrap().len(), 3);
    assert_eq!(d["mission"]["progress"]["blocked"], 3);
    assert_eq!(
        d["mission"]["progress"]["left"], 1,
        "left = still for the bot: d.example"
    );
    assert_eq!(
        d["blocked"][0]["blocked_reason"],
        "CAPTCHA on the sign-up form"
    );

    let path = format!("/v1/missions/{id}/answers");
    // An item with neither an answer nor skip is refused and nothing is applied.
    let r = call(
        &t,
        Method::POST,
        &path,
        Some(json!({"answers": [{"key": "a.example", "answer": "solved"}, {"key": "b.example"}]})),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(store.progress(&id).unwrap().blocked, 3);
    let r = call(&t, Method::POST, &path, Some(json!({"answers": []}))).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    let r = call(
        &t,
        Method::POST,
        &path,
        Some(json!({"answers": [
            {"key": "a.example", "answer": "I solved the CAPTCHA, try again"},
            {"key": "b.example", "skip": true}
        ]})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    assert_eq!(r.json(), json!({"released": 1, "skipped": 1}));
    let p = store.progress(&id).unwrap();
    assert_eq!(
        (p.blocked, p.todo, p.skipped),
        (1, 2, 1),
        "c.example stays blocked"
    );
    let items = store.items(&id, None).unwrap();
    assert_eq!(
        items[0].owner_answer.as_deref(),
        Some("I solved the CAPTCHA, try again")
    );
    call(&t, Method::POST, &format!("/v1/missions/{id}/cancel"), None).await;
}
