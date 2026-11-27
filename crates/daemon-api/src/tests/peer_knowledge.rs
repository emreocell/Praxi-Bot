//! Paired profile editors cannot bypass host-local knowledge authority routes.
use super::*;

async fn peer_request(
    t: &T,
    role: crate::PeerRole,
    method: Method,
    path: &str,
    body: Value,
) -> Resp {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    req.extensions_mut().insert(crate::PeerIdentity {
        key: "knowledge-review-peer".into(),
        role,
    });
    let response = crate::peer_router(t.state.clone())
        .oneshot(req)
        .await
        .unwrap();
    let (parts, body) = response.into_parts();
    Resp {
        status: parts.status,
        headers: parts.headers,
        body: body.collect().await.unwrap().to_bytes().to_vec(),
    }
}

async fn rejects_knowledge_changes(role: crate::PeerRole) {
    let t = setup();
    let (id, _) = make_chat(&t).await;
    let bot_id = protocol::BotId(id.parse().unwrap());
    let mut bot = t.state.bot_of_user(bot_id).unwrap();
    let original = json!({"auto_retrieve":false,"embedding":{"profile":{"model":"approved-model"},"budget":{"requests_per_minute":1},"local_profile_id":"installed-profile"}});
    bot.config["knowledge"] = original.clone();
    t.state
        .storage()
        .update_bot(bot.id, &bot.name, &bot.config, 1)
        .unwrap();
    let mut variants = Vec::new();
    let mut changed = original.clone();
    changed["auto_retrieve"] = true.into();
    variants.push(changed);
    let mut changed = original.clone();
    changed["embedding"]["budget"]["requests_per_minute"] = 100_000.into();
    variants.push(changed);
    let mut changed = original.clone();
    changed["embedding"]["local_profile_id"] = "different-profile".into();
    variants.push(changed);
    let mut changed = original.clone();
    changed["embedding"]["local_artifacts"] =
        json!({"runtime":{"path":"/tmp/untrusted-runtime","sha256":"a".repeat(64)}});
    variants.push(changed);
    let mut changed = original.clone();
    changed["embedding"]["query_consent"] =
        json!({"profile_fingerprint":"a".repeat(64),"granted_at_ms":1});
    variants.push(changed);
    let mut changed = original.clone();
    changed["sources"] = json!([{"remote_embedding":{"profile_fingerprint":"b".repeat(64)}}]);
    variants.push(changed);
    variants.push(json!({}));
    let version = t.state.storage().bot_version(bot_id).unwrap().unwrap();
    for (variant, knowledge) in variants.into_iter().enumerate() {
        let mut config = bot.config.clone();
        config["knowledge"] = knowledge;
        for body in [
            json!({"expected_version":version,"config":config}),
            json!({"expected_version":version,"profile_toml":toml::to_string(&config).unwrap()}),
        ] {
            for (method, path) in [
                (Method::POST, "/v1/bots".to_owned()),
                (Method::PUT, format!("/v1/bots/{id}")),
            ] {
                let response = peer_request(&t, role, method.clone(), &path, body.clone()).await;
                assert_eq!(
                    response.status,
                    StatusCode::FORBIDDEN,
                    "{role:?} {method} variant {variant}: {}",
                    response.json()
                );
                assert_eq!(
                    t.state.bot_of_user(bot_id).unwrap().config["knowledge"],
                    original
                );
            }
        }
    }
    if matches!(role, crate::PeerRole::Phone | crate::PeerRole::Desktop) {
        for include_knowledge in [true, false] {
            let mut config = bot.config.clone();
            config["bot"]["name"] = "Peer renamed bot".into();
            if !include_knowledge {
                config.as_object_mut().unwrap().remove("knowledge");
            }
            for as_toml in [false, true] {
                let version = t.state.storage().bot_version(bot_id).unwrap().unwrap();
                let body = if as_toml {
                    json!({"expected_version":version,"profile_toml":toml::to_string(&config).unwrap()})
                } else {
                    json!({"expected_version":version,"config":config})
                };
                let response =
                    peer_request(&t, role, Method::PUT, &format!("/v1/bots/{id}"), body).await;
                assert_eq!(
                    response.status,
                    StatusCode::OK,
                    "ordinary peer edit: {}",
                    response.json()
                );
                assert_eq!(response.json()["config"]["knowledge"], original);
                assert_eq!(response.json()["name"], "Peer renamed bot");
            }
        }
    }
    let version = t.state.storage().bot_version(bot_id).unwrap().unwrap();
    let owner = call(
        &t,
        Method::PUT,
        &format!("/v1/bots/{id}"),
        Some(json!({"expected_version":version,"name":"Owner rename","config":bot.config})),
    )
    .await;
    assert_eq!(owner.status, StatusCode::OK);
}

#[tokio::test]
async fn peer_knowledge_phone_cannot_change_profile_authority() {
    rejects_knowledge_changes(crate::PeerRole::Phone).await;
}
#[tokio::test]
async fn peer_knowledge_desktop_cannot_change_profile_authority() {
    rejects_knowledge_changes(crate::PeerRole::Desktop).await;
}
#[tokio::test]
async fn peer_knowledge_node_cannot_change_profile_authority() {
    rejects_knowledge_changes(crate::PeerRole::Node).await;
}
#[tokio::test]
async fn peer_knowledge_host_cannot_change_profile_authority() {
    rejects_knowledge_changes(crate::PeerRole::Host).await;
}

/// Choosing a folder is the local owner's act: a paired device may neither browse the
/// host's folders, preview one, nor read an indexed file, whatever its role (the host
/// itself included).
#[tokio::test]
async fn peers_cannot_browse_preview_or_open_the_hosts_folders_and_files() {
    let t = setup();
    let (id, _) = make_chat(&t).await;
    let source = uuid::Uuid::new_v4();
    for role in [
        crate::PeerRole::Phone,
        crate::PeerRole::Desktop,
        crate::PeerRole::Node,
        crate::PeerRole::Host,
    ] {
        for (method, path, body) in [
            (Method::GET, "/v1/knowledge/folders".to_owned(), json!({})),
            (
                Method::GET,
                "/v1/knowledge/folders?path=/".to_owned(),
                json!({}),
            ),
            (
                Method::POST,
                format!("/v1/bots/{id}/knowledge/folder-preview"),
                json!({"path": "/"}),
            ),
            (
                Method::GET,
                format!("/v1/bots/{id}/knowledge/folder-preview"),
                json!({}),
            ),
            (
                Method::GET,
                format!("/v1/bots/{id}/knowledge/sources/{source}/file?locator=a.pdf"),
                json!({}),
            ),
            (
                Method::GET,
                format!(
                    "/v1/bots/{id}//knowledge/sources/{source}/file?locator=a.pdf&download=true"
                ),
                json!({}),
            ),
            (
                Method::HEAD,
                format!("/v1/bots/{id}/knowledge/sources/{source}/file?locator=a.pdf"),
                json!({}),
            ),
        ] {
            let response = peer_request(&t, role, method.clone(), &path, body).await;
            assert_eq!(
                response.status,
                StatusCode::FORBIDDEN,
                "{role:?} {method} {path}"
            );
        }
    }
}
