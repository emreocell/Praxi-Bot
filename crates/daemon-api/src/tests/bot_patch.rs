#![allow(clippy::too_many_lines, clippy::needless_pass_by_value)]
//! `PATCH /v1/bots/{id}`: field-level edits with optimistic concurrency.

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::{T, call, setup};
use crate::{PeerIdentity, PeerRole, peer_router};

const FULL: &str = r#"
[bot]
id = "tester"
name = "Tester"
persona = "Careful."

[models]
executor = "fake/echo"
fallback = ["custom/odd:model"]

[runtime]
computer = "off"
"#;

async fn make_bot(t: &T) -> (String, Value) {
    let r = call(
        t,
        Method::POST,
        "/v1/bots",
        Some(json!({ "profile_toml": FULL })),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let v = r.json();
    (format!("/v1/bots/{}", v["id"].as_str().unwrap()), v)
}

fn set(path: &str, value: Value) -> Value {
    json!({"op": "set", "path": path, "value": value})
}

fn unset(path: &str) -> Value {
    json!({"op": "unset", "path": path})
}

#[tokio::test]
async fn patch_changes_only_the_named_fields_and_bumps_the_version() {
    let t = setup();
    let (path, bot) = make_bot(&t).await;
    assert_eq!(bot["version"], 1);

    let r = call(
        &t,
        Method::PATCH,
        &path,
        Some(json!({
            "expected_version": 1,
            "name": "Renamed",
            "ops": [set("/bot/persona", json!("Bold.")), set("/models/planner", json!("fake/plan"))]
        })),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let v = r.json();
    assert_eq!(v["version"], 2);
    assert_eq!(v["name"], "Renamed");
    assert_eq!(v["config"]["bot"]["persona"], "Bold.");
    assert_eq!(v["config"]["models"]["planner"], "fake/plan");
    // Everything not named is exactly as stored: the other model keys, the runtime target.
    assert_eq!(v["config"]["models"]["executor"], "fake/echo");
    assert_eq!(
        v["config"]["models"]["fallback"],
        json!(["custom/odd:model"])
    );
    assert_eq!(v["config"]["runtime"]["computer"], "off");
    assert_eq!(v["runtime"]["computer"], "off");

    let got = call(&t, Method::GET, &path, None).await.json();
    assert_eq!(got["version"], 2);
    assert_eq!(got["config"], v["config"]);

    // unset removes one key and leaves its siblings.
    let r = call(
        &t,
        Method::PATCH,
        &path,
        Some(
            json!({"expected_version": 2, "ops": [unset("/models/planner"), unset("/nope/deep")]}),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let v = r.json();
    assert_eq!(v["version"], 3);
    assert!(v["config"]["models"].get("planner").is_none());
    assert_eq!(v["config"]["models"]["executor"], "fake/echo");
}

#[tokio::test]
async fn a_stale_version_is_refused_with_the_current_bot_and_writes_nothing() {
    let t = setup();
    let (path, _) = make_bot(&t).await;
    // Another client wins first.
    let r = call(
        &t,
        Method::PATCH,
        &path,
        Some(json!({"expected_version": 1, "ops": [set("/bot/persona", json!("Theirs."))]})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);

    let r = call(
        &t,
        Method::PATCH,
        &path,
        Some(json!({"expected_version": 1, "name": "Mine", "ops": [set("/bot/color", json!("#fff"))]})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    let body = r.json();
    assert_eq!(body["error"]["code"], "version_conflict");
    assert_eq!(body["current"]["version"], 2);
    assert_eq!(body["current"]["config"]["bot"]["persona"], "Theirs.");
    let got = call(&t, Method::GET, &path, None).await.json();
    assert_eq!(got["name"], "Tester");
    assert!(got["config"]["bot"].get("color").is_none());

    // A full PUT also bumps the version, so a patch built on the old one is stale.
    let r = call(
        &t,
        Method::PUT,
        &path,
        Some(json!({"expected_version": 2, "config": {"bot": {"id": "tester", "name": "T"}, "models": {"executor": "fake/echo"}}})),
    )
    .await;
    assert_eq!(r.json()["version"], 3);
    let r = call(
        &t,
        Method::PATCH,
        &path,
        Some(json!({"expected_version": 2, "ops": []})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn each_permission_category_is_patched_on_its_own() {
    let t = setup();
    let (path, bot) = make_bot(&t).await;
    // Start: custom fs=ask, shell=ask.
    let r = call(
        &t,
        Method::PATCH,
        &path,
        Some(json!({"expected_version": bot["version"], "ops": [
            set("/permissions/categories/fs/level", json!({"level": "ask"})),
            set("/permissions/categories/shell/level", json!({"level": "ask"})),
        ]})),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let v0 = r.json()["version"].as_i64().unwrap();

    // Client B sets shell=deny; client A (which loaded v0) edited only fs.
    let r = call(
        &t,
        Method::PATCH,
        &path,
        Some(json!({"expected_version": v0, "ops": [
            set("/permissions/categories/shell/level", json!({"level": "deny"})),
        ]})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let r = call(
        &t,
        Method::PATCH,
        &path,
        Some(json!({"expected_version": v0, "ops": [
            set("/permissions/categories/fs/level", json!({"level": "deny"})),
        ]})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    // "Overwrite": A re-sends only fs against the version the conflict returned.
    let current = r.json()["current"]["version"].as_i64().unwrap();
    let r = call(
        &t,
        Method::PATCH,
        &path,
        Some(json!({"expected_version": current, "ops": [
            set("/permissions/categories/fs/level", json!({"level": "deny"})),
        ]})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let cats = &r.json()["config"]["permissions"]["categories"];
    assert_eq!(cats["fs"]["level"]["level"], "deny");
    assert_eq!(cats["shell"]["level"]["level"], "deny");
}

#[tokio::test]
async fn bad_patches_are_refused_without_a_write() {
    let t = setup();
    let (path, bot) = make_bot(&t).await;
    let v = bot["version"].clone();
    for (ops, why) in [
        (vec![set("models/executor", json!("x"))], "no leading slash"),
        (vec![set("/models//executor", json!("x"))], "empty segment"),
        (
            vec![set("/models/executor/deep", json!(1))],
            "through a scalar",
        ),
        (
            vec![set("/models/fallback/0", json!("x"))],
            "through an array",
        ),
        (
            vec![set("/runtime/computer", json!("moon"))],
            "invalid profile",
        ),
    ] {
        let r = call(
            &t,
            Method::PATCH,
            &path,
            Some(json!({"expected_version": v, "ops": ops})),
        )
        .await;
        assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{why}");
    }
    let r = call(
        &t,
        Method::PATCH,
        &path,
        Some(json!({"expected_version": v, "name": "  "})),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let got = call(&t, Method::GET, &path, None).await.json();
    assert_eq!(got["version"], v);
    assert_eq!(
        call(
            &t,
            Method::PATCH,
            "/v1/bots/00000000-0000-7000-8000-000000000000",
            Some(json!({"expected_version": 1}))
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn runtime_target_is_patched_separately_from_everything_else() {
    let t = setup();
    let (path, bot) = make_bot(&t).await;
    let r = call(
        &t,
        Method::PATCH,
        &path,
        Some(json!({"expected_version": bot["version"], "ops": [set("/runtime/computer", json!("host"))]})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let v = r.json();
    assert_eq!(v["runtime"]["computer"], "host");
    assert_eq!(v["config"]["bot"]["persona"], "Careful.");
}

async fn peer(t: &T, role: PeerRole, method: Method, path: &str, body: Value) -> StatusCode {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    req.extensions_mut().insert(PeerIdentity {
        key: format!("peer-{role:?}"),
        role,
    });
    peer_router(t.state.clone())
        .oneshot(req)
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn peer_roles_treat_patch_like_put() {
    let t = setup();
    let (path, bot) = make_bot(&t).await;
    let body =
        json!({"expected_version": bot["version"], "ops": [set("/bot/persona", json!("P"))]});
    // Roles other than desktop and phone are refused every non-GET bot call, PUT included.
    for role in [PeerRole::Node, PeerRole::Host] {
        assert_eq!(
            peer(&t, role, Method::PATCH, &path, body.clone()).await,
            StatusCode::FORBIDDEN,
            "{role:?}"
        );
        assert_eq!(
            peer(&t, role, Method::PUT, &path, json!({"config": {}})).await,
            StatusCode::FORBIDDEN
        );
    }
    for role in [PeerRole::Desktop, PeerRole::Phone] {
        let b = call(&t, Method::GET, &path, None).await.json();
        let body =
            json!({"expected_version": b["version"], "ops": [set("/bot/persona", json!("P"))]});
        assert_eq!(
            peer(&t, role, Method::PATCH, &path, body).await,
            StatusCode::OK,
            "{role:?}"
        );
    }
}

#[tokio::test]
async fn review95_patch_never_changes_knowledge_settings() {
    let t = setup();
    let (path, bot) = make_bot(&t).await;
    let body = json!({"expected_version": bot["version"],
        "ops": [set("/knowledge/auto_retrieve", json!(true))]});
    for role in [PeerRole::Desktop, PeerRole::Phone] {
        assert_eq!(
            peer(&t, role, Method::PATCH, &path, body.clone()).await,
            StatusCode::FORBIDDEN,
            "{role:?}"
        );
    }
    let after = call(&t, Method::GET, &path, None).await.json();
    assert_eq!(after["version"], bot["version"]);
    assert!(after["config"].get("knowledge").is_none());
    // Generic profile edits never change knowledge for anyone: the dedicated
    // endpoint owns it, so the local owner is refused too and nothing is written.
    let r = call(&t, Method::PATCH, &path, Some(body)).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let still = call(&t, Method::GET, &path, None).await.json();
    assert_eq!(still["version"], bot["version"]);
}

#[tokio::test]
async fn review95_legacy_put_requires_the_loaded_revision_and_never_reverts_a_patch() {
    let t = setup();
    let (path, before) = make_bot(&t).await;
    let r = call(
        &t,
        Method::PATCH,
        &path,
        Some(json!({"expected_version":1,"ops":[
            set("/permissions/categories/shell/level", json!({"level":"deny"})),
            set("/runtime/computer", json!("host"))
        ]})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let newer = r.json();
    let audit = t.state.storage().list_audit(t.state.user_id()).unwrap();
    let missing = call(
        &t,
        Method::PUT,
        &path,
        Some(json!({"name":"Mine", "config":before["config"]})),
    )
    .await;
    assert_eq!(missing.status, StatusCode::PRECONDITION_REQUIRED);
    assert_eq!(missing.json()["error"]["code"], "precondition_required");
    let stale = call(
        &t,
        Method::PUT,
        &path,
        Some(json!({"expected_version":1,"name":"Mine","config":before["config"]})),
    )
    .await;
    assert_eq!(stale.status, StatusCode::CONFLICT);
    assert_eq!(stale.json()["current"]["version"], newer["version"]);
    assert_eq!(stale.json()["current"]["config"], newer["config"]);
    let current = call(&t, Method::GET, &path, None).await.json();
    assert_eq!(current["config"], newer["config"]);
    assert_eq!(current["version"], 2);
    // A deliberate replacement against the fresh revision preserves omitted runtime.
    let mut config = newer["config"].clone();
    config.as_object_mut().unwrap().remove("runtime");
    let saved = call(
        &t,
        Method::PUT,
        &path,
        Some(json!({"expected_version":2,"name":"Mine","config":config})),
    )
    .await;
    assert_eq!(saved.status, StatusCode::OK);
    assert_eq!(saved.json()["version"], 3);
    assert_eq!(saved.json()["config"]["runtime"]["computer"], "host");
    assert_eq!(
        saved.json()["config"]["permissions"],
        newer["config"]["permissions"]
    );
    let stale_patch = call(
        &t,
        Method::PATCH,
        &path,
        Some(json!({"expected_version":2,"name":"Old"})),
    )
    .await;
    assert_eq!(stale_patch.status, StatusCode::CONFLICT);
    assert_eq!(stale_patch.json()["current"]["name"], "Mine");
    // Profile PUT/PATCH have never appended an audit row; preserve that contract.
    assert_eq!(
        t.state.storage().list_audit(t.state.user_id()).unwrap(),
        audit
    );
}

#[tokio::test]
async fn review95_patch_limits_depth_size_and_operation_count_without_writing() {
    let t = setup();
    let (path, before) = make_bot(&t).await;
    for ops in [
        vec![set(&format!("/future{}", "/x".repeat(100_000)), json!(1))],
        vec![set(&format!("/future{}", "/x".repeat(32)), json!(1))],
        vec![set("/future", json!({"nested": "x".repeat(180_000)}))],
        vec![set("/future", json!(1)); 129],
    ] {
        let r = call(
            &t,
            Method::PATCH,
            &path,
            Some(json!({"expected_version":1,"ops":ops})),
        )
        .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{:?}", r.json());
    }
    // Shallow operations cannot build a too-deep result over successive requests either.
    let mut value = json!(1);
    for _ in 0..32 {
        value = json!({"x":value});
    }
    let r = call(
        &t,
        Method::PATCH,
        &path,
        Some(json!({"expected_version":1,"ops":[set("/future",value)]})),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let current = call(&t, Method::GET, &path, None).await.json();
    assert_eq!(current["config"], before["config"]);
    assert_eq!(current["version"], 1);
}

#[tokio::test]
async fn review95_profile_put_patch_peer_roles_cover_encoded_and_noncanonical_paths() {
    let t = setup();
    let (canonical, _) = make_bot(&t).await;
    let bid = canonical.rsplit('/').next().unwrap();
    for role in [
        PeerRole::Node,
        PeerRole::Host,
        PeerRole::Desktop,
        PeerRole::Phone,
    ] {
        for method in [Method::PUT, Method::PATCH] {
            for path in [
                canonical.clone(),
                format!("{canonical}/"),
                format!("/v1//bots//{bid}"),
                format!("/v1/%62ots/{bid}"),
                format!("/v1/bots%2F{bid}"),
            ] {
                let before = call(&t, Method::GET, &canonical, None).await.json();
                let body = if method == Method::PUT {
                    json!({"expected_version":before["version"],"config":before["config"],"name":"Peer"})
                } else {
                    json!({"expected_version":before["version"],"name":"Peer"})
                };
                let status = peer(&t, role, method.clone(), &path, body).await;
                if matches!(role, PeerRole::Node | PeerRole::Host) {
                    assert_eq!(status, StatusCode::FORBIDDEN, "{role:?} {method} {path}");
                } else if path == canonical {
                    assert_eq!(status, StatusCode::OK, "{role:?} {method} {path}");
                } else {
                    assert_eq!(status, StatusCode::NOT_FOUND, "{role:?} {method} {path}");
                }
                let after = call(&t, Method::GET, &canonical, None).await.json();
                let delta = i64::from(status == StatusCode::OK);
                assert_eq!(
                    after["version"].as_i64().unwrap(),
                    before["version"].as_i64().unwrap() + delta
                );
            }
        }
    }
}

#[tokio::test]
async fn review95_get_list_and_put_responses_keep_their_own_snapshot_under_concurrent_writes() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let t = setup();
    let (path, bot) = make_bot(&t).await;
    let bid = bot["id"].as_str().unwrap().to_owned();
    let saved = call(
        &t,
        Method::PATCH,
        &path,
        Some(json!({"expected_version":1,"name":"2"})),
    )
    .await;
    assert_eq!(saved.status, StatusCode::OK);
    let stop = Arc::new(AtomicBool::new(false));
    let started = Arc::new(std::sync::Barrier::new(2));
    let writer = {
        let stop = stop.clone();
        let started = started.clone();
        let db = t._dir.path().join("bot.db");
        std::thread::spawn(move || {
            let conn = rusqlite::Connection::open(db).unwrap();
            conn.busy_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            started.wait();
            while !stop.load(Ordering::SeqCst) {
                conn.execute(
                    "UPDATE bots SET name=CAST(version+1 AS TEXT),version=version+1 WHERE id=?1",
                    [&bid],
                )
                .unwrap();
                std::thread::yield_now();
            }
        })
    };
    started.wait();
    let mut mismatches = Vec::new();
    for _ in 0..300 {
        let got = call(&t, Method::GET, &path, None).await.json();
        if got["name"].as_str().unwrap().parse::<i64>().unwrap() != got["version"].as_i64().unwrap()
        {
            mismatches.push(got);
        }
        let list = call(&t, Method::GET, "/v1/bots", None).await.json();
        let got = &list[0];
        if got["name"].as_str().unwrap().parse::<i64>().unwrap() != got["version"].as_i64().unwrap()
        {
            mismatches.push(got.clone());
        }
        let expected = got["version"].as_i64().unwrap();
        let response = call(&t, Method::PUT, &path, Some(json!({"expected_version":expected,"name":(expected+1).to_string(),"config":got["config"]}))).await;
        if response.status == StatusCode::OK && response.json()["version"] != expected + 1 {
            mismatches.push(response.json());
        }
    }
    stop.store(true, Ordering::SeqCst);
    writer.join().unwrap();
    assert!(
        mismatches.is_empty(),
        "inconsistent snapshots: {:?}",
        mismatches.first()
    );
}

#[tokio::test]
async fn changing_the_computer_needs_the_owner_key_but_other_edits_do_not() {
    let t = setup();
    let (path, _) = make_bot(&t).await;
    let to_server = json!({
        "expected_version": 1,
        "ops": [set("/runtime/computer", json!("server:abc"))]
    });
    let r = super::call_token_only(&t, Method::PATCH, &path, Some(to_server.clone())).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{:?}", r.json());
    let r = super::call_token_only(
        &t,
        Method::PATCH,
        &path,
        Some(json!({"expected_version": 1, "ops": [set("/bot/persona", json!("Bold."))]})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    // The desktop app (owner key) may switch the computer.
    let mut owner = to_server;
    owner["expected_version"] = json!(2);
    let r = call(&t, Method::PATCH, &path, Some(owner)).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    assert_eq!(r.json()["config"]["runtime"]["computer"], "server:abc");
}
