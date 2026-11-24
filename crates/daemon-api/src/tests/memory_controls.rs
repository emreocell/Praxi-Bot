//! Per-bot memory settings, the "Needs your OK" review list and Basic/Smart recall.

#![allow(clippy::assert_is_empty)]

use super::*;
use memory_store::{Kind, NewMemory, Trust};
use protocol::BotId;
use uuid::Uuid;

async fn peer_call(t: &T, role: crate::PeerRole, method: Method, path: &str) -> StatusCode {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .unwrap();
    req.extensions_mut().insert(crate::PeerIdentity {
        key: "peer-dev".into(),
        role,
    });
    crate::peer_router(t.state.clone())
        .oneshot(req)
        .await
        .unwrap()
        .status()
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one ordered scenario: validate, store, preview, profile edit, peers
async fn memory_settings_are_owner_only_validated_and_survive_profile_edits() {
    let t = setup();
    let (bot, _) = make_chat(&t).await;
    let path = format!("/v1/bots/{bot}/memory/settings");

    let first = call(&t, Method::GET, &path, None).await;
    assert_eq!(first.status, StatusCode::OK);
    assert_eq!(first.json()["mode"], "on");
    assert_eq!(first.json()["budget_tokens"], 800);
    assert_eq!(first.json()["share_with_other_bots"], true);

    // Out-of-range budget and unknown mode are refused with plain 400s.
    let bad = call(
        &t,
        Method::PUT,
        &path,
        Some(json!({"mode": "on", "budget_tokens": 90_000, "share_with_other_bots": true})),
    )
    .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    let bad = call(
        &t,
        Method::PUT,
        &path,
        Some(json!({"mode": "sometimes", "budget_tokens": null, "share_with_other_bots": true})),
    )
    .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);

    let set = call(
        &t,
        Method::PUT,
        &path,
        Some(json!({"mode": "off", "budget_tokens": 1200, "share_with_other_bots": false})),
    )
    .await;
    assert_eq!(set.status, StatusCode::OK, "{}", set.json());
    let got = call(&t, Method::GET, &path, None).await.json();
    assert_eq!(
        (got["mode"].clone(), got["budget_tokens"].clone()),
        (json!("off"), json!(1200))
    );
    assert_eq!(got["share_with_other_bots"], false);

    // Off: the preview shows nothing even though a memory matches.
    let store = t.state.memory_store().unwrap();
    store
        .add(
            &NewMemory::new(
                t.state.user_id(),
                None,
                Kind::Profile,
                "Likes green tea every morning",
            ),
            "t",
        )
        .unwrap();
    let preview = call(
        &t,
        Method::GET,
        &format!("/v1/bots/{bot}/memory/preview?query=green%20tea"),
        None,
    )
    .await;
    assert!(preview.json()["hits"].as_array().unwrap().is_empty());

    // A profile edit cannot smuggle the memory table in or out.
    let version = call(&t, Method::GET, &format!("/v1/bots/{bot}"), None)
        .await
        .json()["version"]
        .clone();
    let edit = call(
        &t,
        Method::PUT,
        &format!("/v1/bots/{bot}"),
        Some(json!({
            "profile_toml": format!("{PROFILE}\n[memory]\nmode = \"on\"\n"),
            "expected_version": version
        })),
    )
    .await;
    assert_eq!(edit.status, StatusCode::FORBIDDEN, "{}", edit.json());
    assert_eq!(edit.json()["error"]["code"], "memory_owner_only");
    let still = call(&t, Method::GET, &path, None).await.json();
    assert_eq!(still["mode"], "off");
    // Neither can a JSON-patch edit of the bot profile.
    let json_patch = call(
        &t,
        Method::PATCH,
        &format!("/v1/bots/{bot}"),
        Some(json!({
            "expected_version": version,
            "ops": [{"op": "set", "path": "/memory/mode", "value": "on"}]
        })),
    )
    .await;
    assert_ne!(json_patch.status, StatusCode::OK, "{}", json_patch.json());
    assert_eq!(
        call(&t, Method::GET, &path, None).await.json()["mode"],
        "off"
    );

    // Peers (phone, other computer, ...) cannot change it.
    for role in [
        crate::PeerRole::Desktop,
        crate::PeerRole::Phone,
        crate::PeerRole::Node,
        crate::PeerRole::Host,
    ] {
        assert_eq!(
            peer_call(&t, role, Method::PUT, &path).await,
            StatusCode::FORBIDDEN,
            "{role:?}"
        );
    }
}

#[tokio::test]
async fn held_memories_are_listed_approved_discarded_and_never_previewed_until_approved() {
    let t = setup();
    let (bot, _) = make_chat(&t).await;
    let store = t.state.memory_store().unwrap();
    let user = t.state.user_id();
    let mut held = NewMemory::new(
        user,
        Some(BotId(bot.parse().unwrap())),
        Kind::Episodic,
        "The shop opens at nine",
    );
    held.trust = Trust::Untrusted;
    held.pending = true;
    let a = store.add(&held, "web page").unwrap();
    held.content = "Parking is free on sundays".into();
    let b = store.add(&held, "web page").unwrap();

    let list = call(&t, Method::GET, "/v1/memory/pending", None)
        .await
        .json();
    assert_eq!(list.as_array().unwrap().len(), 2);
    assert_eq!(list[0]["pending"], true);
    let preview = |q: &'static str| {
        let t = &t;
        let bot = bot.clone();
        async move {
            call(
                t,
                Method::GET,
                &format!("/v1/bots/{bot}/memory/preview?query={q}"),
                None,
            )
            .await
            .json()["hits"]
                .as_array()
                .unwrap()
                .len()
        }
    };
    assert_eq!(
        preview("shop%20opens").await,
        0,
        "held: never in the preview"
    );

    let ok = call(
        &t,
        Method::POST,
        &format!("/v1/memory/{}/approve", a.id),
        None,
    )
    .await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.json());
    assert_eq!(ok.json()["pending"], false);
    assert_eq!(preview("shop%20opens").await, 1);
    // Approving twice, or an unknown id, is a plain 404.
    let again = call(
        &t,
        Method::POST,
        &format!("/v1/memory/{}/approve", a.id),
        None,
    )
    .await;
    assert_eq!(again.status, StatusCode::NOT_FOUND);

    let gone = call(
        &t,
        Method::POST,
        &format!("/v1/memory/{}/discard", b.id),
        None,
    )
    .await;
    assert_eq!(gone.status, StatusCode::OK);
    assert_eq!(gone.json()["deleted"], true);
    let list = call(&t, Method::GET, "/v1/memory/pending", None)
        .await
        .json();
    assert!(list.as_array().unwrap().is_empty());

    for role in [crate::PeerRole::Phone, crate::PeerRole::Desktop] {
        for path in [
            format!("/v1/memory/{}/approve", Uuid::now_v7()),
            format!("/v1/memory/{}/discard", Uuid::now_v7()),
        ] {
            assert_eq!(
                peer_call(&t, role, Method::POST, &path).await,
                StatusCode::FORBIDDEN,
                "{role:?} {path}"
            );
        }
    }
}

#[tokio::test]
async fn recall_mode_defaults_to_basic_and_smart_needs_a_source_and_consent() {
    let t = setup();
    let (bot, _) = make_chat(&t).await;
    let status = call(&t, Method::GET, "/v1/memory/embedding", None).await;
    assert_eq!(status.status, StatusCode::OK);
    assert_eq!(status.json()["mode"], "basic");
    assert_eq!(status.json()["state"], "ready");
    assert!(status.json()["sources"].as_array().unwrap().is_empty());

    for (body, expected) in [
        (json!({"mode": "smart"}), StatusCode::BAD_REQUEST),
        (
            // The bot has no embedding model configured for its knowledge.
            json!({"mode": "smart", "from_bot": bot, "consent_send_text": true}),
            StatusCode::CONFLICT,
        ),
        (json!({"mode": "fancy"}), StatusCode::BAD_REQUEST),
    ] {
        let r = call(&t, Method::PUT, "/v1/memory/embedding", Some(body)).await;
        assert_eq!(r.status, expected, "{}", r.json());
    }
    // The guard was released after each refusal: Basic is accepted.
    let ok = call(
        &t,
        Method::PUT,
        "/v1/memory/embedding",
        Some(json!({"mode": "basic"})),
    )
    .await;
    assert_eq!(ok.status, StatusCode::ACCEPTED, "{}", ok.json());
    assert_eq!(
        peer_call(
            &t,
            crate::PeerRole::Phone,
            Method::PUT,
            "/v1/memory/embedding"
        )
        .await,
        StatusCode::FORBIDDEN
    );
}
