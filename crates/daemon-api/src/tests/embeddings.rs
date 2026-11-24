//! `/v1/embeddings`: owner-only writes, write-only keys, honest states.

use std::sync::Arc;

use agent_core::{AgentLoop, LoopConfig, Providers};
use axum::http::{Method, StatusCode};
use embedding_hub::{EmbeddingConfig, EmbeddingHub};
use secret_store::SecretStore;
use serde_json::json;
use storage::Storage;
use tool_registry::ToolRegistry;

use super::{OWNER_KEY, T, TOKEN, call, call_token_only};
use crate::fake::FakeProvider;
use crate::{AppState, DaemonConfig, router};

const KEY: &str = "AIza-embedding-writeonly-secret";

fn setup_hub() -> (T, Arc<secret_store::MemoryStore>) {
    let dir = tempfile::tempdir().unwrap();
    let storage = Arc::new(Storage::open(dir.path()).unwrap());
    let hub = Arc::new(EmbeddingHub::new(EmbeddingConfig::default(), &|_| None, None).unwrap());
    let agent = AgentLoop::new(
        Arc::clone(&storage),
        ToolRegistry::new(),
        Providers::single(Arc::new(FakeProvider)).with_embedding_hub(hub),
        LoopConfig::default(),
    );
    let state = AppState::new(agent.clone(), storage, DaemonConfig::new(TOKEN)).unwrap();
    state.set_owner_key(OWNER_KEY.into());
    let store = Arc::new(secret_store::MemoryStore::new());
    state.set_secret_store(store.clone());
    (
        T {
            app: router(state.clone()),
            state,
            agent,
            _dir: dir,
        },
        store,
    )
}

fn provider(allow: bool) -> serde_json::Value {
    json!({
        "id": "gemini-free", "kind": "gemini", "model": "gemini-embedding-001",
        "dimensions": 768, "allow_remote": allow, "confirm_remote": allow, "api_key": KEY
    })
}

#[tokio::test]
async fn default_has_no_provider_and_lists_the_dated_catalog() {
    let (t, _) = setup_hub();
    let r = call(&t, Method::GET, "/v1/embeddings", None).await;
    assert_eq!(r.status, StatusCode::OK);
    let v = r.json();
    assert_eq!(v["providers"], json!([]));
    let models = v["catalog"]["models"].as_array().unwrap();
    let g = models
        .iter()
        .find(|m| m["model"] == "gemini-embedding-001")
        .unwrap();
    assert_eq!(g["free_tier"]["rpm"], 100);
    assert_eq!(
        v["catalog"]["decision_models"][0]["free_tier"]["rpd"],
        10_000
    );
}

#[tokio::test]
async fn saving_is_owner_only_and_the_key_never_comes_back() {
    let (t, store) = setup_hub();
    let body = json!({"providers": [provider(true)], "chains": {"memory": ["gemini-free"]}});
    let r = call_token_only(&t, Method::PUT, "/v1/embeddings", Some(body.clone())).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.json()["error"]["code"], "owner_key_required");

    let r = call(&t, Method::PUT, "/v1/embeddings", Some(body)).await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let text = String::from_utf8_lossy(&r.body).to_string();
    assert!(!text.contains(KEY));
    let v = r.json();
    assert_eq!(v["providers"][0]["has_key"], true);
    assert_eq!(v["providers"][0]["allow_remote"], true);
    assert_eq!(v["chains"]["memory"], json!(["gemini-free"]));
    // The key is in the secret store; the settings file only names it.
    assert_eq!(
        store
            .get("embedding-gemini-free")
            .unwrap()
            .unwrap()
            .expose(),
        KEY
    );
    let file = std::fs::read_to_string(t.state.data_dir().join("embeddings.json")).unwrap();
    assert!(!file.contains(KEY));
    assert!(file.contains("embedding-gemini-free"));
}

#[tokio::test]
async fn invalid_setups_and_unconsented_tests_are_refused_in_plain_words() {
    let (t, _) = setup_hub();
    let bad =
        json!({"providers": [{"id": "mine", "kind": "compatible", "model": "m", "dimensions": 8}]});
    let r = call(&t, Method::PUT, "/v1/embeddings", Some(bad)).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(
        r.json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("endpoint")
    );

    let ok = json!({"providers": [provider(false)]});
    call(&t, Method::PUT, "/v1/embeddings", Some(ok)).await;
    let r = call(&t, Method::POST, "/v1/embeddings/gemini-free/test", None).await;
    assert_eq!(r.json()["outcome"], "not_allowed");
}

#[tokio::test]
async fn consent_does_not_follow_a_changed_destination() {
    let (t, _) = setup_hub();
    let first = json!({"providers": [provider(true)]});
    let r = call(&t, Method::PUT, "/v1/embeddings", Some(first)).await;
    assert_eq!(r.json()["providers"][0]["allow_remote"], true);

    // Same destination re-saved the way the UI does (old flag, no fresh tick): kept.
    let mut same = provider(true);
    same["confirm_remote"] = json!(false);
    same.as_object_mut().unwrap().remove("api_key");
    let r = call(
        &t,
        Method::PUT,
        "/v1/embeddings",
        Some(json!({"providers": [same.clone()]})),
    )
    .await;
    assert_eq!(r.json()["providers"][0]["allow_remote"], true);

    // Another endpoint with the old flag and no fresh tick: consent is dropped.
    let mut moved = same.clone();
    moved["kind"] = json!("compatible");
    moved["base_url"] = json!("https://embeddings.example.com/v1");
    let r = call(
        &t,
        Method::PUT,
        "/v1/embeddings",
        Some(json!({"providers": [moved.clone()]})),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    assert_eq!(r.json()["providers"][0]["allow_remote"], false);

    // A fresh tick for that destination is honoured.
    moved["confirm_remote"] = json!(true);
    let r = call(
        &t,
        Method::PUT,
        "/v1/embeddings",
        Some(json!({"providers": [moved]})),
    )
    .await;
    assert_eq!(r.json()["providers"][0]["allow_remote"], true);
}

#[tokio::test]
async fn metadata_and_lan_endpoints_are_refused() {
    let (t, _) = setup_hub();
    for url in [
        "http://169.254.169.254/v1",
        "https://10.0.0.5/v1",
        "http://example.com/v1",
    ] {
        let p = json!({"id": "mine", "kind": "compatible", "model": "m", "dimensions": 8,
            "base_url": url, "allow_remote": true, "confirm_remote": true, "api_key": KEY});
        let r = call(
            &t,
            Method::PUT,
            "/v1/embeddings",
            Some(json!({"providers": [p]})),
        )
        .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{url}");
    }
}
