//! `/v1/secret-requests/{call_id}`: the owner answers a bot's credential form.
//! The value goes into the vault and nowhere else.

use std::sync::Arc;
use std::time::Duration;

use axum::http::{Method, StatusCode};
use secret_store::{FillSecrets, MemoryStore, SecretStore};
use serde_json::json;

use super::{T, call, call_token_only, make_chat, send_msg, setup_with_config, wait_state};

const CANARY: &str = "canary-Correct-Horse-Battery-4417!";
const USER_CANARY: &str = "canary-user-name-7731";

struct Rig {
    t: T,
    secrets: Arc<FillSecrets>,
    redactor: agent_core::SharedRedactor,
}

fn rig() -> Rig {
    rig_on(Arc::new(MemoryStore::new()))
}

fn rig_on(store: Arc<dyn SecretStore>) -> Rig {
    // The tool is off by default (no desktop form yet); these tests switch it on.
    let t = setup_with_config(
        |_| {},
        agent_core::LoopConfig {
            secret_requests: true,
            ..agent_core::LoopConfig::default()
        },
    );
    let redactor = agent_core::SharedRedactor::default();
    let hook = redactor.clone();
    let secrets = Arc::new(
        FillSecrets::open(store, t.state.data_dir().as_path())
            .unwrap()
            .with_value_hook(Arc::new(move |v: &str| hook.register(v))),
    );
    t.state.set_fill_secrets(Arc::clone(&secrets));
    t.agent
        .tool_registry()
        .register(builtin_tools::secret_request::secret_request_tool(
            Arc::clone(&secrets),
        ));
    Rig {
        t,
        secrets,
        redactor,
    }
}

/// Start a run that asks for a login and return `(run, call_id)` once the form is open.
async fn open_form(r: &Rig, text: &str) -> (String, String) {
    let (_, chat) = make_chat(&r.t).await;
    let run = send_msg(&r.t, &chat, text, "queue").await["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let id: uuid::Uuid = run.parse().unwrap();
    for _ in 0..200 {
        let log = storage::read_event_log(r.t.state.storage().data_dir(), protocol::RunId(id))
            .unwrap_or_default();
        for env in log.iter().rev() {
            if let protocol::Event::SecretRequested(q) = &env.event {
                return (run, q.call_id.to_string());
            }
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("no secret request was opened");
}

const ASK: &str = r#"call:secret_request {"name":"login/example","purpose":"Sign in to download the invoice","fields":"login"}"#;

fn everything_stored_about_the_run(r: &Rig, run: &str) -> String {
    let id: uuid::Uuid = run.parse().unwrap();
    let log = storage::read_event_log(r.t.state.storage().data_dir(), protocol::RunId(id))
        .unwrap_or_default();
    format!(
        "{log:?}{:?}{:?}",
        r.t.state.storage().list_audit(r.t.state.user_id()).unwrap(),
        r.t.state.storage().list_steps(protocol::RunId(id)).unwrap()
    )
}

/// Guard: the value reaches the vault (and the redactor) through the owner
/// route only; the run ends `succeeded` having learnt only `stored`; no event,
/// journal step, audit row or response carries the value; the form is single-use.
#[tokio::test]
async fn the_owner_submits_and_the_value_only_reaches_the_vault() {
    let r = rig();
    let (run, call_id) = open_form(&r, ASK).await;
    wait_state(&r.t, &run, "waiting_approval").await;
    let path = format!("/v1/secret-requests/{call_id}");
    let body =
        json!({"username": USER_CANARY, "password": CANARY, "origins": ["https://Example.com/"]});

    // A token holder is not the owner: refused, and the form keeps waiting.
    let denied = call_token_only(&r.t, Method::POST, &path, Some(body.clone())).await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    assert_eq!(denied.json()["error"]["code"], "owner_key_required");
    assert!(!r.secrets.exists("login/example/password"));

    // A wrong shape is refused without quoting the body and keeps the form open.
    let bad = call(
        &r.t,
        Method::POST,
        &path,
        Some(json!({"value": CANARY, "origins": 5})),
    )
    .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    assert!(!String::from_utf8_lossy(&bad.body).contains(CANARY));
    let wrong_fields = call(&r.t, Method::POST, &path, Some(json!({"value": CANARY}))).await;
    assert_eq!(wrong_fields.status, StatusCode::BAD_REQUEST);
    assert!(!String::from_utf8_lossy(&wrong_fields.body).contains(CANARY));
    assert!(!r.secrets.exists("login/example/password"));

    let ok = call(&r.t, Method::POST, &path, Some(body.clone())).await;
    assert_eq!(ok.status, StatusCode::OK, "{:?}", ok.json());
    assert_eq!(ok.json()["outcome"], "stored");
    assert_eq!(ok.json()["run_notified"], true);
    assert!(!String::from_utf8_lossy(&ok.body).contains(CANARY));
    wait_state(&r.t, &run, "succeeded").await;

    assert_eq!(
        r.secrets
            .resolve_value("login/example/password")
            .unwrap()
            .expose(),
        CANARY
    );
    let list = call(&r.t, Method::GET, "/v1/fill-secrets", None).await;
    let list_text = String::from_utf8_lossy(&list.body).into_owned();
    assert!(list_text.contains("https://example.com") && !list_text.contains(CANARY));
    assert!(r.redactor.contains_secret(CANARY), "masked at once");

    let kept = everything_stored_about_the_run(&r, &run);
    assert!(
        !kept.contains(CANARY) && !kept.contains(USER_CANARY),
        "{kept}"
    );
    assert!(kept.contains("secret_request.submit"));

    // Single use.
    let again = call(&r.t, Method::POST, &path, Some(body)).await;
    assert_eq!(again.status, StatusCode::NOT_FOUND);
    assert_eq!(again.json()["error"]["code"], "secret_request_gone");
}

/// Guard: cancel stores nothing and a name created while the form was open is
/// not overwritten without the bot's `replace`.
#[tokio::test]
async fn cancel_leaves_nothing_and_an_existing_name_is_not_replaced_silently() {
    let r = rig();
    let (run, call_id) = open_form(&r, ASK).await;
    let path = format!("/v1/secret-requests/{call_id}");

    // The name appears while the card is open (another window, the CLI).
    r.secrets
        .upsert(
            "login/example/password",
            Some("older-value-123"),
            None,
            None,
        )
        .unwrap();
    let clash = call(
        &r.t,
        Method::POST,
        &path,
        Some(json!({"username": "u", "password": CANARY})),
    )
    .await;
    assert_eq!(clash.status, StatusCode::CONFLICT);
    assert_eq!(clash.json()["error"]["code"], "secret_request_exists");
    assert_eq!(
        r.secrets
            .resolve_value("login/example/password")
            .unwrap()
            .expose(),
        "older-value-123"
    );

    let cancelled = call(&r.t, Method::POST, &format!("{path}/cancel"), None).await;
    assert_eq!(cancelled.status, StatusCode::OK);
    assert_eq!(cancelled.json()["outcome"], "cancelled");
    wait_state(&r.t, &run, "succeeded").await;
    assert!(!r.secrets.exists("login/example/username"));
    let again = call(&r.t, Method::POST, &format!("{path}/cancel"), None).await;
    assert_eq!(again.json()["error"]["code"], "secret_request_gone");
}

/// Guard: a paired device never reaches the routes (the peer router matches the prefix).
#[test]
fn the_peer_router_treats_secret_requests_as_a_fill_route() {
    for p in [
        "/v1/secret-requests/x",
        "/v1/secret-requests/x/cancel",
        "/v1/secret-requests",
    ] {
        assert!(crate::fill_routes::is_fill_path(p), "{p}");
    }
    assert!(crate::owner::is_owner_route(
        "POST",
        "/v1/secret-requests/abc/cancel"
    ));
}

/// A store whose write number `fail_at` (counted from `arm`) fails: a keychain error
/// on the second half of a login.
struct FailingStore {
    inner: MemoryStore,
    sets: std::sync::atomic::AtomicUsize,
    fail_at: std::sync::atomic::AtomicUsize,
}

impl SecretStore for FailingStore {
    fn get(&self, n: &str) -> Result<Option<secret_store::SecretValue>, secret_store::SecretError> {
        self.inner.get(n)
    }
    fn set(&self, n: &str, v: &str) -> Result<(), secret_store::SecretError> {
        use std::sync::atomic::Ordering::SeqCst;
        if self.sets.fetch_add(1, SeqCst) == self.fail_at.load(SeqCst) {
            return Err(secret_store::SecretError::Keyring("boom".into()));
        }
        self.inner.set(n, v)
    }
    fn delete(&self, n: &str) -> Result<bool, secret_store::SecretError> {
        self.inner.delete(n)
    }
    fn list_names(&self) -> Result<Vec<String>, secret_store::SecretError> {
        self.inner.list_names()
    }
}

/// Guard: a replace whose second write fails leaves the old username AND password,
/// and the form is still waiting so the owner can try again.
#[tokio::test]
async fn a_failed_login_replace_keeps_the_old_username_and_password() {
    use std::sync::atomic::Ordering::SeqCst;
    let store = Arc::new(FailingStore {
        inner: MemoryStore::new(),
        sets: 0.into(),
        fail_at: usize::MAX.into(),
    });
    let r = rig_on(store.clone());
    r.secrets
        .upsert("login/example/username", Some("old-user"), None, None)
        .unwrap();
    r.secrets
        .upsert("login/example/password", Some("old-pass-123"), None, None)
        .unwrap();
    let (run, call_id) = open_form(
        &r,
        r#"call:secret_request {"name":"login/example","purpose":"Sign in","fields":"login","replace":true}"#,
    )
    .await;
    wait_state(&r.t, &run, "waiting_approval").await;
    let path = format!("/v1/secret-requests/{call_id}");
    let body = json!({"username": USER_CANARY, "password": CANARY});

    // The next two writes are this call's: the second one fails.
    store.fail_at.store(store.sets.load(SeqCst) + 1, SeqCst);
    let failed = call(&r.t, Method::POST, &path, Some(body.clone())).await;
    assert_ne!(failed.status, StatusCode::OK);
    let get = |n: &str| r.secrets.resolve_value(n).unwrap().expose().to_owned();
    assert_eq!(get("login/example/username"), "old-user");
    assert_eq!(get("login/example/password"), "old-pass-123");
    let state = call(&r.t, Method::GET, &format!("/v1/runs/{run}"), None).await;
    assert_eq!(
        state.json()["state"],
        "waiting_approval",
        "the form must still be waiting"
    );

    // Retry works once the store is healthy.
    store.fail_at.store(usize::MAX, SeqCst);
    let ok = call(&r.t, Method::POST, &path, Some(body)).await;
    assert_eq!(ok.status, StatusCode::OK, "{:?}", ok.json());
    assert_eq!(get("login/example/username"), USER_CANARY);
    assert_eq!(get("login/example/password"), CANARY);
}
