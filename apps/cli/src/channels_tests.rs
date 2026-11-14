//! Hermetic tests of the Telegram wiring: a `wiremock` server stands in for the
//! Telegram Bot API, no real token is involved.

use std::sync::Arc;
use std::time::Duration;

use channel_telegram::store_bot_token;
use daemon_api::ChannelControl;
use secret_store::{MemoryStore, SecretStore, SecretValue};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::channels::*;

const TOKEN: &str = "123456789:AAH_cli_test_token_value_xyz";

fn store_with_token() -> Arc<dyn SecretStore> {
    let s = MemoryStore::new();
    store_bot_token(&s, TOKEN).unwrap();
    Arc::new(s)
}

async fn telegram_mock() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("/bot{TOKEN}/getUpdates")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"ok": true, "result": []}))
                .set_delay(Duration::from_millis(50)),
        )
        .mount(&server)
        .await;
    server
}

async fn polls(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path().ends_with("/getUpdates"))
        .count()
}

async fn wait_for_first_poll(server: &MockServer) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while polls(server).await == 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the channel polled the (mock) Telegram API");
}

#[test]
fn enabled_defaults_on_and_follows_the_switch() {
    let dir = tempfile::tempdir().unwrap();
    assert!(telegram_enabled(dir.path()));
    set_telegram_enabled(dir.path(), false).unwrap();
    assert!(!telegram_enabled(dir.path()));
    set_telegram_enabled(dir.path(), true).unwrap();
    assert!(telegram_enabled(dir.path()));
}

#[test]
fn setup_stores_the_token_without_printing_it_and_disable_keeps_it() {
    let dir = tempfile::tempdir().unwrap();
    let store = MemoryStore::new();
    assert!(!telegram_configured(&store).unwrap());
    let mut said = Vec::new();
    telegram_setup(&store, dir.path(), &SecretValue::new(TOKEN), &mut |l| {
        said.push(l.to_owned());
    })
    .unwrap();
    assert!(telegram_configured(&store).unwrap());
    assert!(telegram_enabled(dir.path()));
    assert!(said.iter().all(|l| !l.contains(TOKEN)), "{said:?}");
    assert!(
        telegram_setup(&store, dir.path(), &SecretValue::new("  "), &mut |_| {}).is_err(),
        "an empty token is refused"
    );

    telegram_disable(dir.path(), &mut |_| {}).unwrap();
    assert!(!telegram_enabled(dir.path()));
    assert!(telegram_configured(&store).unwrap(), "the token stays");
}

#[test]
fn status_lines_and_json() {
    let mut out = Vec::new();
    let daemon = json!({"id": "telegram", "running": true, "paired_users": 2});
    telegram_status(true, true, Some(&daemon), 0, false, &mut out).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("configured: yes") && text.contains("running: yes"));
    assert!(text.contains("paired users: 2"));

    let mut out = Vec::new();
    telegram_status(true, false, None, 3, true, &mut out).unwrap();
    let v: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["running"], false);
    assert_eq!(v["daemon_reachable"], false);
    assert_eq!(v["paired_users"], 3, "falls back to the state file");
    assert_eq!(v["enabled"], false);
}

#[tokio::test]
async fn not_configured_or_disabled_means_not_running_and_no_codes() {
    let dir = tempfile::tempdir().unwrap();
    let t = start_telegram(
        dir.path(),
        Arc::new(MemoryStore::new()),
        "http://127.0.0.1:1",
        "tok",
        None,
    );
    assert!(!t.info().running && !t.info().configured);
    assert!(t.create_pair_code().unwrap_err().contains("not configured"));

    set_telegram_enabled(dir.path(), false).unwrap();
    let t = start_telegram(
        dir.path(),
        store_with_token(),
        "http://127.0.0.1:1",
        "tok",
        None,
    );
    let i = t.info();
    assert!(i.configured && !i.enabled && !i.running);
    assert!(t.create_pair_code().unwrap_err().contains("disabled"));
}

#[tokio::test]
async fn the_channel_starts_polls_pairs_and_stops() {
    let dir = tempfile::tempdir().unwrap();
    let server = telegram_mock().await;
    let t = start_telegram(
        dir.path(),
        store_with_token(),
        "http://127.0.0.1:1",
        "daemon-token",
        Some(&server.uri()),
    );
    let i = t.info();
    assert!(i.configured && i.enabled && i.running);
    assert_eq!(i.paired_users, 0);
    let code = t.create_pair_code().unwrap();
    assert_eq!(code.expires_in_secs, 600);
    assert!(code.code.len() == 6 && code.code.bytes().all(|b| b.is_ascii_digit()));

    wait_for_first_poll(&server).await;
    t.shutdown().await;
    let after = polls(&server).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(polls(&server).await, after, "no polling after shutdown");
}

/// The whole daemon wiring: `/v1/channels` and the pair-code route over HTTP,
/// and the channel stops with the daemon.
#[tokio::test]
async fn the_daemon_runs_the_channel_and_stops_it_on_shutdown() {
    use agent_core::{AgentLoop, LoopConfig, Providers};
    use daemon_api::{AppState, DaemonConfig};

    const API_TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let dir = tempfile::tempdir().unwrap();
    let storage = Arc::new(storage::Storage::open(dir.path()).unwrap());
    let agent = AgentLoop::new(
        Arc::clone(&storage),
        tool_registry::ToolRegistry::new(),
        Providers::single(Arc::new(daemon_api::fake::FakeProvider)),
        LoopConfig::default(),
    );
    let state = AppState::new(agent, storage, DaemonConfig::new(API_TOKEN)).unwrap();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let server = telegram_mock().await;
    let t = start_telegram(
        dir.path(),
        store_with_token(),
        &format!("http://{addr}"),
        API_TOKEN,
        Some(&server.uri()),
    );
    state.add_channel(t.clone());
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let daemon = tokio::spawn(crate::serve::run_daemon(listener, state, Some(t), async {
        let _ = stopped.await;
    }));

    let http = reqwest::Client::new();
    let url = |p: &str| format!("http://{addr}{p}");
    let no_auth = http
        .post(url("/v1/channels/telegram/pair-code"))
        .send()
        .await
        .unwrap();
    assert_eq!(no_auth.status(), 401);
    let list: Value = http
        .get(url("/v1/channels"))
        .bearer_auth(API_TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list[0]["id"], "telegram");
    assert_eq!(list[0]["running"], true);
    let code: Value = http
        .post(url("/v1/channels/telegram/pair-code"))
        .bearer_auth(API_TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(code["code"].as_str().unwrap().len(), 6);
    assert_eq!(code["expires_in_secs"], 600);

    wait_for_first_poll(&server).await;
    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(10), daemon)
        .await
        .expect("the daemon stopped")
        .unwrap()
        .unwrap();
    let after = polls(&server).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        polls(&server).await,
        after,
        "the channel stopped with the daemon"
    );
}

#[tokio::test]
async fn standby_telegram_runtime_does_not_poll_until_started_and_can_restart() {
    let dir = tempfile::tempdir().unwrap();
    let server = telegram_mock().await;
    let runtime = telegram_runtime(
        dir.path(),
        store_with_token(),
        "http://127.0.0.1:1",
        "token",
        Some(&server.uri()),
        None,
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(polls(&server).await, 0);
    assert!(!runtime.info().running);
    runtime.start();
    wait_for_first_poll(&server).await;
    assert!(runtime.info().running);
    runtime.stop().await;
    assert!(!runtime.info().running);
    let stopped_at = polls(&server).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(polls(&server).await, stopped_at);
    runtime.start();
    tokio::time::timeout(Duration::from_secs(5), async {
        while polls(&server).await == stopped_at {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(runtime.info().running);
    runtime.shutdown().await;
}
