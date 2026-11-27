//! `/v1/secret-requests/{call_id}`: the owner answers a bot's credential form.
//! The value goes into the vault and nowhere else.

use std::sync::Arc;
use std::time::Duration;

use axum::http::{Method, StatusCode};
use secret_store::{FillSecrets, MemoryStore, SecretStore};
use serde_json::json;

use super::{T, auth, call, call_token_only, make_chat, send, setup_with_config, wait_state};

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
    // This test embeds the loop without the desktop-launch configuration.
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
    let (run, call, _bot) = open_form_as(r, text).await;
    (run, call)
}

/// [`open_form`] that also returns the id of the bot that asked.
async fn open_form_as(r: &Rig, text: &str) -> (String, String, String) {
    open_form_prepared(r, text, |_| {}).await
}

/// [`open_form_as`] that runs `before(bot)` once the bot exists and before it asks
/// (for example to grant it an entry it may then ask to replace).
async fn open_form_prepared(
    r: &Rig,
    text: &str,
    before: impl FnOnce(&str),
) -> (String, String, String) {
    let (bot, chat) = make_chat(&r.t).await;
    before(&bot);
    let (key, value) = auth();
    let response = send(
        &r.t.app,
        Method::POST,
        &format!("/v1/chats/{chat}/messages"),
        Some(json!({"text": text, "mode": "queue"})),
        &[(key, &value), ("x-praxi-message-source", "local_ui")],
    )
    .await;
    assert_eq!(response.status, StatusCode::ACCEPTED);
    let run = response.json()["run_id"].as_str().unwrap().to_owned();
    let id: uuid::Uuid = run.parse().unwrap();
    for _ in 0..200 {
        let log = storage::read_event_log(r.t.state.storage().data_dir(), protocol::RunId(id))
            .unwrap_or_default();
        for env in log.iter().rev() {
            if let protocol::Event::SecretRequested(q) = &env.event {
                return (run, q.call_id.to_string(), bot);
            }
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("no secret request was opened");
}

/// Whether the owner's form for this run says it REPLACES an existing entry.
fn requested_replaces(r: &Rig, run: &str) -> bool {
    let id: uuid::Uuid = run.parse().unwrap();
    let log = storage::read_event_log(r.t.state.storage().data_dir(), protocol::RunId(id))
        .unwrap_or_default();
    log.iter()
        .find_map(|env| match &env.event {
            protocol::Event::SecretRequested(q) => Some(q.replaces_existing),
            _ => None,
        })
        .expect("a secret request event")
}

/// The owner lets `bot` use `name` (a Vault grant).
fn grant(r: &Rig, name: &str, bots: &[&str]) {
    let bots: Vec<String> = bots.iter().map(|b| (*b).to_owned()).collect();
    r.secrets
        .upsert_with(
            name,
            secret_store::FillSecretUpdate {
                bot_ids: Some(&bots),
                ..secret_store::FillSecretUpdate::default()
            },
        )
        .unwrap();
}

const ASK: &str = r#"call:secret_request {"name":"login/example","origin":"https://example.com","purpose":"Sign in to download the invoice","fields":"login"}"#;

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
#[allow(clippy::too_many_lines)] // one ordered walk through the route and everything it must not touch
async fn the_owner_submits_and_the_value_only_reaches_the_vault() {
    let r = rig();
    let (run, call_id) = open_form(&r, ASK).await;
    wait_state(&r.t, &run, "waiting_approval").await;
    let path = format!("/v1/secret-requests/{call_id}");
    let body = json!({"username": USER_CANARY, "password": CANARY});

    // A token holder is not the owner: refused, and the form keeps waiting.
    let denied = call_token_only(&r.t, Method::POST, &path, Some(body.clone())).await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    assert_eq!(denied.json()["error"]["code"], "owner_key_required");
    assert!(!r.secrets.exists("login/example/password"));

    // A submission cannot change the destination, and errors carry no values.
    let invalid = [
        json!({"username": USER_CANARY, "password": CANARY, "origins": ["https://other.example"]}),
        json!({"value": CANARY}),
        json!({"username": "short", "password": CANARY}),
    ];
    for input in invalid {
        let bad = call(&r.t, Method::POST, &path, Some(input)).await;
        assert_eq!(bad.status, StatusCode::BAD_REQUEST);
        let response = String::from_utf8_lossy(&bad.body);
        assert!(!response.contains(CANARY) && !response.contains(USER_CANARY));
    }
    assert!(!r.secrets.exists("login/example/password"));
    assert!(!r.secrets.exists("login/example/username"));

    let ok = call(&r.t, Method::POST, &path, Some(body.clone())).await;
    assert_eq!(ok.status, StatusCode::OK, "{:?}", ok.json());
    assert_eq!(ok.json()["outcome"], "stored");
    assert_eq!(ok.json()["run_notified"], true);
    assert!(!String::from_utf8_lossy(&ok.body).contains(CANARY));
    wait_state(&r.t, &run, "succeeded").await;

    assert_eq!(
        r.secrets
            .resolve_value_for_owner("login/example/password")
            .unwrap()
            .expose(),
        CANARY
    );
    let list = call(&r.t, Method::GET, "/v1/fill-secrets", None).await;
    let list_text = String::from_utf8_lossy(&list.body).into_owned();
    assert!(list_text.contains("https://example.com"));
    assert!(!list_text.contains(CANARY) && !list_text.contains(USER_CANARY));
    let metadata =
        std::fs::read_to_string(secret_store::metadata_path(r.t.state.data_dir().as_path()))
            .unwrap();
    assert!(metadata.contains("Sign in to download the invoice"));
    assert!(!metadata.contains(CANARY) && !metadata.contains(USER_CANARY));
    let bot = r.secrets.list().unwrap().remove(0).bot_ids.remove(0);
    for name in ["login/example/username", "login/example/password"] {
        assert!(
            r.secrets
                .check_browser_in(
                    name,
                    Some("https://example.com"),
                    Some("https://example.com"),
                    Some(&bot)
                )
                .is_ok()
        );
        assert!(
            r.secrets
                .check_browser_in(
                    name,
                    Some("https://other.example"),
                    Some("https://example.com"),
                    Some(&bot)
                )
                .is_err()
        );
        assert!(
            r.secrets
                .check_browser_in(
                    name,
                    Some("https://example.com"),
                    Some("https://example.com"),
                    Some("other-bot")
                )
                .is_err()
        );
    }
    assert!(r.redactor.contains_secret(CANARY), "masked at once");

    let kept = everything_stored_about_the_run(&r, &run);
    assert!(
        !kept.contains(CANARY) && !kept.contains(USER_CANARY),
        "{kept}"
    );
    assert!(kept.contains("secret_request.submit") && kept.contains("secret_request.stored"));
    let chat =
        r.t.state
            .storage()
            .get_run(protocol::RunId(run.parse().unwrap()))
            .unwrap()
            .unwrap()
            .chat_id
            .unwrap();
    let transcript = r.t.state.storage().list_messages(chat).unwrap();
    assert!(
        transcript
            .iter()
            .all(|message| !message.content.contains(CANARY)
                && !message.content.contains(USER_CANARY))
    );

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
    let (run, call_id, bot) = open_form_as(&r, ASK).await;
    let path = format!("/v1/secret-requests/{call_id}");

    // The name appears while the card is open (another window, the CLI), and the
    // owner has allowed this bot on it.
    r.secrets
        .upsert(
            "login/example/password",
            Some("older-value-123"),
            None,
            None,
        )
        .unwrap();
    grant(&r, "login/example/password", &[&bot]);
    let clash = call(
        &r.t,
        Method::POST,
        &path,
        Some(json!({"username": "valid-user", "password": CANARY})),
    )
    .await;
    assert_eq!(clash.status, StatusCode::CONFLICT);
    assert_eq!(clash.json()["error"]["code"], "secret_request_exists");
    assert_eq!(
        r.secrets
            .resolve_value_for_owner("login/example/password")
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

/// Guard: a form cannot overwrite another bot's account, even when that account was
/// created after the card opened (the tool refuses it up front; the route is the root).
#[tokio::test]
async fn a_form_cannot_replace_another_bots_account_that_appeared_while_it_was_open() {
    let r = rig();
    let (run, call_id) = open_form(
        &r,
        r#"call:secret_request {"name":"login/example","origin":"https://example.com","purpose":"Sign in","fields":"login","replace":true}"#,
    )
    .await;
    let path = format!("/v1/secret-requests/{call_id}");
    r.secrets
        .register_account(&secret_store::NewAccount {
            name: "login/example/password",
            site: "https://example.com",
            login_id: "me",
            email_account: None,
            bot_id: "another-bot",
            note: None,
            password_len: 20,
            symbols: true,
        })
        .unwrap();
    let before = r
        .secrets
        .resolve_value_for("login/example/password", Some("another-bot"))
        .unwrap()
        .expose()
        .to_owned();
    let denied = call(
        &r.t,
        Method::POST,
        &path,
        Some(json!({"username": "valid-user", "password": CANARY})),
    )
    .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN, "{:?}", denied.json());
    assert!(!String::from_utf8_lossy(&denied.body).contains(CANARY));
    let still = r
        .secrets
        .resolve_value_for("login/example/password", Some("another-bot"))
        .unwrap();
    assert_eq!(still.expose(), before, "the account value was replaced");
    assert!(!r.secrets.exists("login/example/username"));
    // The form is still open: the owner can cancel it.
    let cancelled = call(&r.t, Method::POST, &format!("{path}/cancel"), None).await;
    assert_eq!(cancelled.status, StatusCode::OK);
    wait_state(&r.t, &run, "succeeded").await;
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
    // The entries are this bot's (the owner allowed it before it asked): only then
    // does the owner get a "replace" form.
    let (run, call_id, bot) = open_form_prepared(
        &r,
        r#"call:secret_request {"name":"login/example","origin":"https://example.com","purpose":"Sign in","fields":"login","replace":true}"#,
        |bot| {
            for name in ["login/example/username", "login/example/password"] {
                r.secrets.upsert_with(name, secret_store::FillSecretUpdate {
                    bot_ids: Some(&[bot.to_owned()]),
                    origins: Some(&["https://example.com".to_owned()]),
                    ..secret_store::FillSecretUpdate::default()
                }).unwrap();
            }
        },
    )
    .await;
    assert!(
        requested_replaces(&r, &run),
        "the owning bot's replace request is shown as a replace"
    );
    wait_state(&r.t, &run, "waiting_approval").await;
    let path = format!("/v1/secret-requests/{call_id}");
    let body = json!({"username": USER_CANARY, "password": CANARY});
    let _ = &bot;

    // The next two writes are this call's: the second one fails.
    store.fail_at.store(store.sets.load(SeqCst) + 1, SeqCst);
    let failed = call(&r.t, Method::POST, &path, Some(body.clone())).await;
    assert_ne!(failed.status, StatusCode::OK);
    let get = |n: &str| {
        r.secrets
            .resolve_value_for_owner(n)
            .unwrap()
            .expose()
            .to_owned()
    };
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
    // The replace changed the values only: the grant is still exactly this bot.
    for n in ["login/example/username", "login/example/password"] {
        let info = r.secrets.list().unwrap().into_iter().find(|i| i.name == n);
        assert_eq!(info.unwrap().bot_ids, std::slice::from_ref(&bot));
    }
}

/// Guard: bot B asks (replace) for an entry the owner made and granted to bots A and
/// C. The card opens exactly as for a free name, but the owner's submit is refused:
/// value, grants and origins stay, and the form is still open to cancel.
#[tokio::test]
async fn a_form_cannot_take_over_an_owner_made_entry_granted_to_other_bots() {
    let r = rig();
    let origins = vec!["https://example.com".to_owned()];
    for (n, v) in [
        ("login/example/username", "owner-user"),
        ("login/example/password", "owner-pass-123"),
    ] {
        r.secrets
            .upsert_with(
                n,
                secret_store::FillSecretUpdate {
                    value: Some(v),
                    origins: Some(&origins),
                    bot_ids: Some(&["bot-a".to_owned(), "bot-c".to_owned()]),
                    ..secret_store::FillSecretUpdate::default()
                },
            )
            .unwrap();
    }
    let (run, call_id) = open_form(
        &r,
        r#"call:secret_request {"name":"login/example","origin":"https://example.com","purpose":"Session expired, re-enter the password","fields":"login","replace":true}"#,
    )
    .await;
    // Scope check at the root: bot B never gets a "replace" form for an entry it
    // may not use, even though it passed replace:true.
    assert!(
        !requested_replaces(&r, &run),
        "the owner was asked to replace an entry of other bots"
    );
    let path = format!("/v1/secret-requests/{call_id}");
    let denied = call(
        &r.t,
        Method::POST,
        &path,
        Some(json!({"username": USER_CANARY, "password": CANARY})),
    )
    .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN, "{:?}", denied.json());
    assert!(!String::from_utf8_lossy(&denied.body).contains(CANARY));
    for (n, v) in [
        ("login/example/username", "owner-user"),
        ("login/example/password", "owner-pass-123"),
    ] {
        assert_eq!(r.secrets.resolve_value_for_owner(n).unwrap().expose(), v);
        let info = r.secrets.list().unwrap().into_iter().find(|i| i.name == n);
        let info = info.unwrap();
        assert_eq!(info.bot_ids, ["bot-a", "bot-c"]);
        assert_eq!(info.origins, origins);
    }
    let cancelled = call(&r.t, Method::POST, &format!("{path}/cancel"), None).await;
    assert_eq!(cancelled.status, StatusCode::OK);
    wait_state(&r.t, &run, "succeeded").await;
}

const FULL_BOT: &str = r#"
[bot]
id = "tester"
name = "Tester"

[models]
executor = "fake/echo"

[permissions]
preset = "full"
"#;

/// A mission with a parked item `a.example` (and `b.example` still to do), plus a form that a
/// mission session opened for it. The session is over: nothing waits on the form.
async fn mission_form(r: &Rig) -> (String, String, agent_core::SecretRequestInfo) {
    r.t.state.enable_missions().unwrap();
    r.t.state.start_missions();
    let (run, call_id, bot) = open_form_as(r, ASK).await;
    // Hand the live form over to a mission: take the run's own copy and stop that run.
    let id = protocol::ToolCallId(call_id.parse().unwrap());
    let mut info = r.t.agent.claim_secret_request(id).expect("form is open");
    r.t.agent
        .cancel(protocol::RunId(run.parse().unwrap()))
        .unwrap();
    wait_state(&r.t, &run, "cancelled").await;

    let made = call(
        &r.t,
        Method::POST,
        "/v1/bots",
        Some(json!({"profile_toml": FULL_BOT, "name": "Ada"})),
    )
    .await;
    let full = made.json()["id"].as_str().unwrap().to_owned();
    let created = call(
        &r.t,
        Method::POST,
        "/v1/missions",
        Some(json!({"bot_id": full, "goal": "Sign up on sites"})),
    )
    .await;
    let mission = created.json()["mission"]["id"].as_str().unwrap().to_owned();
    // Keep the supervisor out of the way while the ledger is edited.
    call(
        &r.t,
        Method::POST,
        &format!("/v1/missions/{mission}/pause"),
        None,
    )
    .await;
    let store = r.t.state.mission_store().unwrap();
    let items: Vec<missions::NewItem> = ["a.example", "b.example"]
        .iter()
        .map(|k| missions::NewItem {
            key: (*k).into(),
            kind: "site".into(),
            title: format!("Sign up on {k}"),
            payload: None,
        })
        .collect();
    store.upsert_items(&mission, &items, 1).unwrap();
    store
        .update_item(
            &mission,
            "a.example",
            &missions::ItemUpdate {
                status: Some(missions::ItemStatus::Blocked),
                blocked_reason: Some("needs the credential \"login/example\"".into()),
                ..missions::ItemUpdate::default()
            },
            false,
            2,
        )
        .unwrap();
    info.mission = Some(agent_core::MissionTarget {
        mission_id: mission.clone(),
        item: "a.example".into(),
    });
    info.deadline = tokio::time::Instant::now() + Duration::from_secs(3600);
    r.t.agent.restore_secret_request(id, info.clone());
    assert_eq!(bot, info.bot_id);
    (mission, call_id, info)
}

/// Missions: the owner's answer stores the value (bound to the asking bot and origin), and
/// releases only the parked item, with the entry name and no value.
#[tokio::test]
async fn a_mission_form_stores_the_value_and_releases_only_its_item() {
    let r = rig();
    let (mission, call_id, info) = mission_form(&r).await;
    let store = r.t.state.mission_store().unwrap();
    let path = format!("/v1/secret-requests/{call_id}");
    let body = json!({"username": USER_CANARY, "password": CANARY});

    let denied = call_token_only(&r.t, Method::POST, &path, Some(body.clone())).await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    assert_eq!(
        store
            .items(&mission, Some(missions::ItemStatus::Blocked))
            .unwrap()
            .len(),
        1,
        "a token holder released nothing"
    );

    let ok = call(&r.t, Method::POST, &path, Some(body.clone())).await;
    assert_eq!(ok.status, StatusCode::OK, "{:?}", ok.json());
    assert_eq!(ok.json()["outcome"], "stored");
    assert_eq!(ok.json()["run_notified"], true, "the item was released");
    assert!(!String::from_utf8_lossy(&ok.body).contains(CANARY));

    let items = store.items(&mission, None).unwrap();
    let a = items.iter().find(|i| i.key == "a.example").unwrap();
    assert_eq!(a.status, missions::ItemStatus::Todo);
    let answer = a.owner_answer.as_deref().unwrap();
    assert!(answer.contains("login/example"), "{answer}");
    assert!(!answer.contains(CANARY) && !answer.contains(USER_CANARY));
    let b = items.iter().find(|i| i.key == "b.example").unwrap();
    assert_eq!(b.status, missions::ItemStatus::Todo);
    assert!(b.owner_answer.is_none(), "the other item is untouched");

    // Bound to the bot, origin and purpose that were shown on the form.
    for name in ["login/example/username", "login/example/password"] {
        assert!(
            r.secrets
                .check_browser_in(
                    name,
                    Some(&info.origin),
                    Some(&info.origin),
                    Some(&info.bot_id)
                )
                .is_ok()
        );
        assert!(
            r.secrets
                .check_browser_in(
                    name,
                    Some("https://other.example"),
                    Some("https://other.example"),
                    Some(&info.bot_id)
                )
                .is_err()
        );
    }
    let metadata =
        std::fs::read_to_string(secret_store::metadata_path(r.t.state.data_dir().as_path()))
            .unwrap();
    assert!(!metadata.contains(CANARY) && !metadata.contains(USER_CANARY));
    let audit = format!(
        "{:?}",
        r.t.state.storage().list_audit(r.t.state.user_id()).unwrap()
    );
    assert!(audit.contains("secret_request.stored") && audit.contains("a.example"));
    assert!(!audit.contains(CANARY) && !audit.contains(USER_CANARY));

    let again = call(&r.t, Method::POST, &path, Some(body)).await;
    assert_eq!(again.json()["error"]["code"], "secret_request_gone");
}

#[tokio::test]
async fn cancelling_a_mission_form_stores_nothing_and_keeps_the_item_parked() {
    let r = rig();
    let (mission, call_id, _) = mission_form(&r).await;
    let store = r.t.state.mission_store().unwrap();
    let cancelled = call(
        &r.t,
        Method::POST,
        &format!("/v1/secret-requests/{call_id}/cancel"),
        None,
    )
    .await;
    assert_eq!(cancelled.status, StatusCode::OK);
    assert_eq!(cancelled.json()["outcome"], "cancelled");
    assert!(!r.secrets.exists("login/example/password"));
    assert_eq!(
        store
            .items(&mission, Some(missions::ItemStatus::Blocked))
            .unwrap()
            .len(),
        1
    );
    let audit = format!(
        "{:?}",
        r.t.state.storage().list_audit(r.t.state.user_id()).unwrap()
    );
    assert!(audit.contains("secret_request.cancelled"));
    let again = call(
        &r.t,
        Method::POST,
        &format!("/v1/secret-requests/{call_id}/cancel"),
        None,
    )
    .await;
    assert_eq!(again.json()["error"]["code"], "secret_request_gone");
}

#[tokio::test]
async fn a_mission_form_takes_no_value_once_the_mission_was_stopped() {
    let r = rig();
    let (mission, call_id, _) = mission_form(&r).await;
    let stopped = call(
        &r.t,
        Method::POST,
        &format!("/v1/missions/{mission}/cancel"),
        None,
    )
    .await;
    assert_eq!(stopped.status, StatusCode::OK, "{:?}", stopped.json());
    let late = call(
        &r.t,
        Method::POST,
        &format!("/v1/secret-requests/{call_id}"),
        Some(json!({"username": USER_CANARY, "password": CANARY})),
    )
    .await;
    assert_eq!(late.status, StatusCode::NOT_FOUND);
    assert_eq!(late.json()["error"]["code"], "secret_request_gone");
    assert!(!r.secrets.exists("login/example/password"));
}
