//! `bot email add --provider agentmail` against a live daemon whose provider is
//! a local mock (no real service, no real key).

use std::sync::Arc;

use agent_core::Providers;
use connector_email::AgentMailEndpoint;
use daemon_api::fake::FakeProvider;
use daemon_api::{AppState, DaemonConfig};
use secret_store::{MemoryStore, SecretStore};
use serde_json::json;
use storage::Storage;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;

const TOKEN: &str = "abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd";
const KEY: &str = "am_cli_test_KEY_0badc0de";

async fn deletes(s: &MockServer) -> usize {
    s.received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.method.as_str() == "DELETE")
        .count()
}

async fn daemon(server: &MockServer) -> (tempfile::TempDir, Arc<dyn SecretStore>, Client) {
    let dir = tempfile::tempdir().unwrap();
    let storage = Arc::new(Storage::open(dir.path()).unwrap());
    let agent = crate::serve::build_agent(
        &storage,
        Providers::single(Arc::new(FakeProvider)),
        dir.path(),
        None,
    );
    let store: Arc<dyn SecretStore> = Arc::new(MemoryStore::new());
    let endpoint = AgentMailEndpoint::loopback_mock_for_tests(&server.uri()).unwrap();
    let (host, _) = crate::email_host::start_email_with(
        dir.path(),
        &agent.tool_registry(),
        Some(&store),
        endpoint,
    )
    .unwrap();
    let state = AppState::new(agent, storage, DaemonConfig::new(TOKEN)).unwrap();
    state.set_email(host);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(daemon_api::serve(listener, state, std::future::pending()));
    (dir, store, Client::new(&url, TOKEN.to_owned()))
}

#[test]
fn the_agentmail_body_carries_the_key_only_in_the_request_body() {
    let senders = vec!["boss@example.com".to_owned()];
    let a = AgentmailAdd {
        name: "helper",
        name_hint: "helper",
        domain: None,
        bot: Some("b-1"),
        allowed_senders: &senders,
    };
    let body = add_agentmail_body(&a, KEY);
    assert_eq!(body["provider"], "agentmail");
    assert_eq!(body["api_key"], KEY);
    assert_eq!(body["bot"], "b-1");
    assert!(body["domain"].is_null());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn add_list_test_and_rm_with_and_without_remote_deletion_never_print_the_key() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v0/inboxes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "pod_id":"p","inbox_id":"helper@agentmail.to","email":"helper@agentmail.to",
            "updated_at":"2026-11-12T10:00:00Z","created_at":"2026-11-12T10:00:00Z"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v0/inboxes/helper@agentmail.to"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"inbox_id":"helper@agentmail.to","email":"helper@agentmail.to"})))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(202))
        .mount(&server)
        .await;
    let (dir, store, c) = daemon(&server).await;
    let mut out = Vec::new();

    let a = AgentmailAdd {
        name: "helper",
        name_hint: "helper",
        domain: None,
        bot: None,
        allowed_senders: &[],
    };
    add_agentmail(&c, &a, KEY, false, &mut out).await.unwrap();
    let said = String::from_utf8(out.clone()).unwrap();
    assert!(said.contains("helper@agentmail.to"), "{said}");
    assert!(!said.contains(KEY));
    assert_eq!(
        store.get("email/helper/api_key").unwrap().unwrap().expose(),
        KEY
    );
    let file = std::fs::read_to_string(dir.path().join("email.toml")).unwrap();
    assert!(!file.contains(KEY));

    out.clear();
    list(&c, false, &mut out).await.unwrap();
    let said = String::from_utf8(out.clone()).unwrap();
    assert!(said.contains("agentmail") && said.contains("key set"), "{said}");
    assert!(!said.contains(KEY));

    out.clear();
    test(&c, "helper", false, &mut out).await.unwrap();
    assert!(String::from_utf8(out.clone()).unwrap().contains("api: ok"));

    // Default removal is local only.
    out.clear();
    rm_with(&c, "helper", false, false, &mut out).await.unwrap();
    assert_eq!(deletes(&server).await, 0);

    add_agentmail(&c, &a, KEY, false, &mut Vec::new()).await.unwrap();
    rm_with(&c, "helper", true, false, &mut Vec::new()).await.unwrap();
    assert_eq!(deletes(&server).await, 1, "only with --delete-inbox");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rejected_key_is_reported_without_echoing_it() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({"code":"unknown_api_key"})))
        .mount(&server)
        .await;
    let (_dir, _store, c) = daemon(&server).await;
    let a = AgentmailAdd {
        name: "helper",
        name_hint: "helper",
        domain: None,
        bot: None,
        allowed_senders: &[],
    };
    let e = add_agentmail(&c, &a, KEY, false, &mut Vec::new()).await.unwrap_err();
    let text = e.to_string();
    assert!(text.contains("rejected the API key"), "{text}");
    assert!(!text.contains(KEY));
}
