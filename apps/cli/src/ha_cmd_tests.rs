//! `bot ha ...` against a live daemon with a real `HomeAssistantHost` and a fake hub.

use std::sync::Arc;

use agent_core::Providers;
use daemon_api::fake::FakeProvider;
use daemon_api::{AppState, DaemonConfig};
use secret_store::{MemoryStore, SecretStore};
use storage::Storage;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::client::ExitError;

const TOKEN: &str = "abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd";
const HA_TOKEN: &str = "ha-cli-token-never-printed";

fn code_of(e: &(dyn std::error::Error + 'static)) -> i32 {
    e.downcast_ref::<ExitError>().map_or(-1, |x| x.code)
}

struct Daemon {
    _dir: tempfile::TempDir,
    store: Arc<dyn SecretStore>,
    client: Client,
}

async fn daemon() -> Daemon {
    let dir = tempfile::tempdir().unwrap();
    let storage = Arc::new(Storage::open(dir.path()).unwrap());
    let agent = crate::serve::build_agent(
        &storage,
        Providers::single(Arc::new(FakeProvider)),
        dir.path(),
        None,
    );
    let store: Arc<dyn SecretStore> = Arc::new(MemoryStore::new());
    let host = crate::homeassistant_host::start_homeassistant(
        dir.path(),
        &agent.tool_registry(),
        Some(&store),
        1,
    )
    .unwrap();
    let state = AppState::new(agent, storage, DaemonConfig::new(TOKEN)).unwrap();
    state.set_owner_key(crate::client::TEST_OWNER_KEY.into());
    state.set_homeassistant(host);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(daemon_api::serve(listener, state, std::future::pending()));
    Daemon {
        _dir: dir,
        store,
        client: Client::new(&url, TOKEN.to_owned())
            .with_owner_key(Some(crate::client::TEST_OWNER_KEY.to_owned())),
    }
}

/// A hub that accepts `HA_TOKEN` only and reports three entities.
async fn hub() -> MockServer {
    let server = MockServer::start().await;
    let bearer = format!("Bearer {HA_TOKEN}");
    Mock::given(method("GET"))
        .and(path("/api/"))
        .and(header("authorization", bearer.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"message": "API running."})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/states"))
        .and(header("authorization", bearer.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"entity_id": "light.a", "state": "on", "attributes": {}},
            {"entity_id": "light.b", "state": "off", "attributes": {}},
            {"entity_id": "sensor.c", "state": "1", "attributes": {}}
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    server
}

fn text(out: &[u8]) -> String {
    String::from_utf8_lossy(out).into_owned()
}

#[test]
fn the_setup_body_carries_the_address_the_token_and_the_allowed_services() {
    let body = setup_body("http://h:8123", Some("t"), &["cover.open_cover".to_owned()]);
    assert_eq!(body["url"], "http://h:8123");
    assert_eq!(body["token"], "t");
    assert_eq!(body["trusted_services"], json!(["cover.open_cover"]));
    assert!(setup_body("http://h:8123", None, &[])["token"].is_null());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn setup_status_test_access_and_remove_against_a_live_daemon_never_print_the_token() {
    let d = daemon().await;
    let hub = hub().await;
    let c = &d.client;
    let mut out = Vec::new();

    status(c, false, &mut out).await.unwrap();
    assert!(text(&out).contains("not set up"), "{}", text(&out));

    out.clear();
    setup(
        c,
        &hub.uri(),
        Some(HA_TOKEN),
        &["cover.open_cover".to_owned()],
        false,
        &mut out,
    )
    .await
    .unwrap();
    let said = text(&out);
    assert!(
        said.contains("3 entities") && !said.contains(HA_TOKEN),
        "{said}"
    );
    assert_eq!(
        d.store
            .get("homeassistant/token")
            .unwrap()
            .unwrap()
            .expose(),
        HA_TOKEN
    );

    out.clear();
    status(c, true, &mut out).await.unwrap();
    let said = text(&out);
    assert!(!said.contains(HA_TOKEN), "{said}");
    let v: Value = serde_json::from_str(said.trim()).unwrap();
    assert_eq!(
        (v["configured"].as_bool(), v["token_set"].as_bool()),
        (Some(true), Some(true))
    );
    assert_eq!(v["trusted_services"], json!(["cover.open_cover"]));
    assert!(v["bots"].is_null());

    out.clear();
    status(c, false, &mut out).await.unwrap();
    let said = text(&out);
    assert!(
        said.contains("every bot") && said.contains("cover.open_cover"),
        "{said}"
    );

    out.clear();
    test(c, None, None, false, &mut out).await.unwrap();
    assert!(text(&out).contains("ok: 3 entities"), "{}", text(&out));

    out.clear();
    access(c, &[], false, false, &mut out).await.unwrap();
    assert!(text(&out).contains("no bot"), "{}", text(&out));
    out.clear();
    access(c, &[], true, false, &mut out).await.unwrap();
    assert!(text(&out).contains("every bot"), "{}", text(&out));
    let err = access(c, &["nobody".to_owned()], false, false, &mut out)
        .await
        .unwrap_err();
    assert_eq!(code_of(&*err), exit::NOT_FOUND);

    out.clear();
    remove(c, false, &mut out).await.unwrap();
    assert!(d.store.get("homeassistant/token").unwrap().is_none());
    let err = remove(c, false, &mut out).await.unwrap_err();
    assert_eq!(code_of(&*err), exit::NOT_FOUND);
    let err = test(c, None, None, false, &mut out).await.unwrap_err();
    assert_eq!(code_of(&*err), exit::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_token_exits_6_and_saves_nothing() {
    let d = daemon().await;
    let hub = hub().await;
    let c = &d.client;
    let mut out = Vec::new();

    let err = setup(c, &hub.uri(), Some("wrong-token"), &[], false, &mut out)
        .await
        .unwrap_err();
    assert_eq!(code_of(&*err), exit::REJECTED);
    assert!(!err.to_string().contains("wrong-token"), "{err}");
    assert!(d.store.get("homeassistant/token").unwrap().is_none());

    // A test of an unsaved address reports the refusal and exits 6.
    let err = test(c, Some(&hub.uri()), Some("wrong-token"), false, &mut out)
        .await
        .unwrap_err();
    assert_eq!(code_of(&*err), exit::REJECTED);
    assert!(
        text(&out).contains("refused the access key"),
        "{}",
        text(&out)
    );
    out.clear();
    test(c, Some(&hub.uri()), Some(HA_TOKEN), false, &mut out)
        .await
        .unwrap();
    assert!(text(&out).contains("ok: 3 entities"));

    // Setup without a token for a hub that was never saved is refused too.
    let err = setup(c, &hub.uri(), None, &[], false, &mut out)
        .await
        .unwrap_err();
    assert_eq!(code_of(&*err), exit::REJECTED);
}
