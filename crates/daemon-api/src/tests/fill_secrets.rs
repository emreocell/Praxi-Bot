//! `/v1/fill-secrets*` and `/v1/fill-grants*`: owner-only management of the
//! secrets the fill tools may type.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use parking_lot::Mutex;
use secret_store::{FillSecrets, MemoryStore, SecretError, SecretStore, SecretValue};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::{T, call, setup};
use crate::AppState;

const VALUE: &str = "hunter2-Correct-Horse-9!";

/// A store that records how many audit rows existed each time it was written
/// or deleted, so a test can prove the audit row comes BEFORE the mutation.
struct OrderStore {
    inner: MemoryStore,
    state: Mutex<Option<AppState>>,
    writes: Mutex<Vec<(String, String, usize)>>,
}

impl OrderStore {
    fn audit_len(&self) -> usize {
        self.state
            .lock()
            .as_ref()
            .map_or(0, |s| s.storage().list_audit(s.user_id()).unwrap().len())
    }
}

impl SecretStore for OrderStore {
    fn get(&self, name: &str) -> Result<Option<SecretValue>, SecretError> {
        self.inner.get(name)
    }
    fn set(&self, name: &str, value: &str) -> Result<(), SecretError> {
        let n = self.audit_len();
        self.writes.lock().push(("set".into(), name.into(), n));
        self.inner.set(name, value)
    }
    fn delete(&self, name: &str) -> Result<bool, SecretError> {
        let n = self.audit_len();
        self.writes.lock().push(("delete".into(), name.into(), n));
        self.inner.delete(name)
    }
    fn list_names(&self) -> Result<Vec<String>, SecretError> {
        self.inner.list_names()
    }
}

struct Rig {
    t: T,
    store: Arc<OrderStore>,
    secrets: Arc<FillSecrets>,
    redactor: agent_core::SharedRedactor,
}

fn rig() -> Rig {
    let t = setup();
    let store = Arc::new(OrderStore {
        inner: MemoryStore::new(),
        state: Mutex::new(Some(t.state.clone())),
        writes: Mutex::new(Vec::new()),
    });
    let redactor = agent_core::SharedRedactor::default();
    let hook = redactor.clone();
    let secrets = Arc::new(
        FillSecrets::open(
            store.clone() as Arc<dyn SecretStore>,
            t.state.data_dir().as_path(),
        )
        .unwrap()
        .with_value_hook(Arc::new(move |v: &str| hook.register(v))),
    );
    t.state.set_fill_secrets(Arc::clone(&secrets));
    t.state
        .set_secret_store(store.clone() as Arc<dyn SecretStore>);
    Rig {
        t,
        store,
        secrets,
        redactor,
    }
}

fn txt(r: &super::Resp) -> String {
    String::from_utf8_lossy(&r.body).into_owned()
}

fn audit_actions(t: &T) -> Vec<String> {
    t.state
        .storage()
        .list_audit(t.state.user_id())
        .unwrap()
        .into_iter()
        .map(|a| a.action)
        .collect()
}

fn audit_dump(t: &T) -> String {
    format!(
        "{:?}",
        t.state.storage().list_audit(t.state.user_id()).unwrap()
    )
}

async fn create(r: &Rig, name: &str, origins: &[&str]) -> super::Resp {
    call(
        &r.t,
        Method::POST,
        "/v1/fill-secrets",
        Some(json!({"name": name, "value": VALUE, "origins": origins})),
    )
    .await
}

/// Guard: the owner flow works, a value is write-only everywhere, every
/// mutation is audited BEFORE it happens, and the production redactor masks the
/// value from the moment it is stored.
#[tokio::test]
async fn the_owner_manages_secrets_and_no_response_or_audit_row_carries_a_value() {
    let r = rig();
    let resp = create(&r, "login/github", &["https://GitHub.com/"]).await;
    assert_eq!(resp.status, StatusCode::CREATED, "{:?}", resp.json());
    assert_eq!(resp.json()["origins"], json!(["https://github.com"]));
    assert_eq!(resp.json()["has_value"], true);
    let mut seen = txt(&resp);

    // The value is masked from this moment.
    assert!(r.redactor.contains_secret(VALUE));
    assert_eq!(r.redactor.redact(&format!("a {VALUE} b")), "a [REDACTED] b");

    let list = call(&r.t, Method::GET, "/v1/fill-secrets", None).await;
    assert_eq!(list.status, StatusCode::OK);
    assert_eq!(list.json()[0]["name"], "login/github");
    seen.push_str(&txt(&list));

    let upd = call(
        &r.t,
        Method::PUT,
        "/v1/fill-secrets/login/github",
        Some(json!({"origins": ["https://github.com", "https://gist.github.com"]})),
    )
    .await;
    assert_eq!(upd.status, StatusCode::OK, "{:?}", upd.json());
    seen.push_str(&txt(&upd));

    let grant = call(
        &r.t,
        Method::POST,
        "/v1/fill-grants",
        Some(json!({"secret": "login/github", "origin": "https://github.com"})),
    )
    .await;
    assert_eq!(grant.status, StatusCode::CREATED, "{:?}", grant.json());
    let gid = grant.json()["id"].as_str().unwrap().to_owned();
    seen.push_str(&txt(&grant));
    let grants = call(&r.t, Method::GET, "/v1/fill-grants", None).await;
    assert_eq!(grants.json().as_array().unwrap().len(), 1);
    seen.push_str(&txt(&grants));
    let rev = call(
        &r.t,
        Method::DELETE,
        &format!("/v1/fill-grants/{gid}"),
        None,
    )
    .await;
    assert_eq!(rev.status, StatusCode::NO_CONTENT);

    let del = call(&r.t, Method::DELETE, "/v1/fill-secrets/login/github", None).await;
    assert_eq!(del.status, StatusCode::NO_CONTENT);
    assert_eq!(r.secrets.list().unwrap().len(), 0);

    assert!(
        !seen.contains(VALUE) && !seen.contains("Correct-Horse"),
        "{seen}"
    );
    let audit = audit_dump(&r.t);
    assert!(!audit.contains("Correct-Horse"), "{audit}");
    let actions = audit_actions(&r.t);
    for a in [
        "fill_secret.create",
        "fill_secret.update",
        "fill_grant.create",
        "fill_grant.revoke",
        "fill_secret.delete",
    ] {
        assert!(actions.iter().any(|x| x == a), "{a} in {actions:?}");
    }
    // The audit row was already there when the store was written / deleted.
    let writes = r.store.writes.lock().clone();
    let set = writes.iter().find(|w| w.0 == "set").unwrap();
    assert!(
        set.2 >= 1,
        "create must be audited before the value is stored: {writes:?}"
    );
    let del = writes.iter().find(|w| w.0 == "delete").unwrap();
    assert!(
        del.2 >= 4,
        "delete must be audited before the value goes: {writes:?}"
    );
}

/// Guard: bad input is refused with a clear status and changes nothing.
#[tokio::test]
async fn bad_names_origins_and_grants_are_refused() {
    let r = rig();
    for (name, origins, expect) in [
        (
            "composio/gmail",
            vec!["https://a.test"],
            StatusCode::BAD_REQUEST,
        ),
        ("fill/x", vec!["https://a.test"], StatusCode::BAD_REQUEST),
        ("a b", vec!["https://a.test"], StatusCode::BAD_REQUEST),
        ("ok/name", vec!["https://*.a.test"], StatusCode::BAD_REQUEST),
        (
            "ok/name",
            vec!["https://a.test/login"],
            StatusCode::BAD_REQUEST,
        ),
        ("ok/name", vec!["ftp://a.test"], StatusCode::BAD_REQUEST),
    ] {
        let resp = create(&r, name, &origins).await;
        assert_eq!(resp.status, expect, "{name} {origins:?}: {:?}", resp.json());
    }
    assert_eq!(r.secrets.list().unwrap().len(), 0);
    assert_eq!(r.store.writes.lock().len(), 0);

    assert_eq!(
        create(&r, "ok/name", &["https://a.test"]).await.status,
        StatusCode::CREATED
    );
    assert_eq!(
        create(&r, "ok/name", &["https://a.test"]).await.status,
        StatusCode::CONFLICT
    );
    // A grant needs an allowed origin and an existing secret.
    let g = call(
        &r.t,
        Method::POST,
        "/v1/fill-grants",
        Some(json!({"secret": "ok/name", "origin": "https://b.test"})),
    )
    .await;
    assert_eq!(g.status, StatusCode::BAD_REQUEST);
    let g = call(
        &r.t,
        Method::POST,
        "/v1/fill-grants",
        Some(json!({"secret": "nope", "origin": "https://a.test"})),
    )
    .await;
    assert_eq!(g.status, StatusCode::NOT_FOUND);
    assert_eq!(
        call(&r.t, Method::DELETE, "/v1/fill-secrets/nope", None)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
}

/// Guard: a daemon without the service says so (no panic, no partial state).
#[tokio::test]
async fn without_the_service_the_routes_are_unavailable() {
    let t = setup();
    let resp = call(&t, Method::GET, "/v1/fill-secrets", None).await;
    assert_eq!(resp.status, StatusCode::SERVICE_UNAVAILABLE);
}

/// Guard: the peer router refuses every fill route to every peer role for every
/// method, whatever the path spelling; nothing is written or audited.
/// Fails if `is_fill_path` stops matching (encoded / double-slash / trailing).
#[tokio::test]
async fn every_peer_role_is_refused_on_every_spelling_of_every_fill_route() {
    let r = rig();
    assert_eq!(
        create(&r, "login/github", &["https://github.com"])
            .await
            .status,
        StatusCode::CREATED
    );
    let before_audit = audit_actions(&r.t).len();
    let before_writes = r.store.writes.lock().len();
    let g = r
        .secrets
        .grant_create("login/github", "https://github.com")
        .unwrap();

    let mut routes: Vec<(Method, String, Option<Value>)> = Vec::new();
    for p in [
        "/v1/fill-secrets",
        "/v1/fill-secrets/",
        "//v1//fill-secrets",
        "/v1/fill-secrets/reveal",
        "/v1/fill-secrets/login/github",
        "/v1/fill-secrets/login/github/",
        "/v1/fill-secrets//login//github",
        "/v1/fill-secrets%2Flogin%2Fgithub",
        "/v1%2Ffill-secrets",
        "/v1/fill-grants",
        "/v1/fill-grants/",
        "//v1/fill-grants",
        &format!("/v1/fill-grants/{}", g.id),
        &format!("/v1/fill-grants/{}/", g.id),
        &format!("/v1/fill-grants%2F{}", g.id),
    ] {
        for m in [Method::GET, Method::POST, Method::PUT, Method::DELETE] {
            let body = (m == Method::POST || m == Method::PUT).then(|| {
                json!({"name": "evil/x", "value": "evil-value-1234", "origins": ["https://evil.test"],
                       "secret": "login/github", "origin": "https://evil.test"})
            });
            routes.push((m, p.to_owned(), body));
        }
    }
    for role in [
        crate::PeerRole::Desktop,
        crate::PeerRole::Phone,
        crate::PeerRole::Node,
        crate::PeerRole::Host,
    ] {
        for (method, path, body) in &routes {
            let b = Request::builder().method(method.clone()).uri(path.as_str());
            let mut req = if let Some(v) = body {
                b.header("content-type", "application/json")
                    .body(Body::from(v.to_string()))
            } else {
                b.body(Body::empty())
            }
            .unwrap();
            req.extensions_mut().insert(crate::PeerIdentity {
                key: "peer-key".into(),
                role,
            });
            let resp = crate::peer_router(r.t.state.clone())
                .oneshot(req)
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::FORBIDDEN,
                "role={role:?} {method} {path} must be 403"
            );
        }
    }
    assert_eq!(
        audit_actions(&r.t).len(),
        before_audit,
        "a refused peer is not audited as a change"
    );
    assert_eq!(r.store.writes.lock().len(), before_writes);
    assert_eq!(r.secrets.list().unwrap().len(), 1);
    assert_eq!(r.secrets.grants().len(), 1);
    assert_eq!(r.secrets.list().unwrap()[0].origins, ["https://github.com"]);
    // Control: the local owner still reaches the route.
    assert_eq!(
        call(&r.t, Method::GET, "/v1/fill-secrets", None)
            .await
            .status,
        StatusCode::OK
    );
}

/// Guard: the generic secrets sink (which a paired desktop may reach) cannot
/// create or overwrite a fill secret, so an allow-list can never be bypassed.
#[tokio::test]
async fn the_generic_secrets_route_cannot_touch_the_fill_namespace() {
    let r = rig();
    for path in [
        "/v1/secrets/fill/login/github",
        "/v1/secrets/FILL/login/github",
        "/v1/secrets/Fill/x",
    ] {
        let local = call(
            &r.t,
            Method::POST,
            path,
            Some(json!({"value": "overwritten-value-1"})),
        )
        .await;
        assert_eq!(local.status, StatusCode::BAD_REQUEST, "{path}");
        let mut req = Request::builder()
            .method(Method::POST)
            .uri(path)
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"value": "overwritten-value-1"}).to_string(),
            ))
            .unwrap();
        req.extensions_mut().insert(crate::PeerIdentity {
            key: "peer-key".into(),
            role: crate::PeerRole::Desktop,
        });
        let resp = crate::peer_router(r.t.state.clone())
            .oneshot(req)
            .await
            .unwrap();
        assert!(
            resp.status() == StatusCode::BAD_REQUEST
                || resp.status() == StatusCode::SERVICE_UNAVAILABLE,
            "{path}: {}",
            resp.status()
        );
    }
    assert_eq!(r.store.inner.list_names().unwrap().len(), 0);
}

/// Guard: Vault metadata, per-bot scoping, `ask_before_use`, and confirmed owner
/// reveal (with Cache-Control: no-store, audit without value, and token-only
/// refusal).
#[tokio::test]
#[allow(clippy::too_many_lines, clippy::assert_is_empty)]
async fn vault_bot_scope_ask_toggle_and_confirmed_owner_reveal() {
    let r = rig();
    let bot_a = call(
        &r.t,
        Method::POST,
        "/v1/bots",
        Some(json!({ "profile_toml": super::PROFILE })),
    )
    .await
    .json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // Unknown bot id is refused.
    let bad_bot = call(
        &r.t,
        Method::POST,
        "/v1/fill-secrets",
        Some(json!({
            "name": "login/badbot",
            "value": VALUE,
            "origins": ["https://github.com"],
            "bot_ids": ["00000000-0000-4000-8000-000000000000"],
        })),
    )
    .await;
    assert_eq!(bad_bot.status, StatusCode::NOT_FOUND);

    // Create with Vault fields; default bot_ids is empty and ask_before_use is true.
    let created = call(
        &r.t,
        Method::POST,
        "/v1/fill-secrets",
        Some(json!({
            "name": "login/github",
            "value": VALUE,
            "kind": "login",
            "site_name": "GitHub",
            "login_id": "alice@example.com",
            "note": "work login",
            "origins": ["https://github.com"],
        })),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{:?}", created.json());
    assert_eq!(created.json()["kind"], "login");
    assert_eq!(created.json()["site_name"], "GitHub");
    assert_eq!(created.json()["login_id"], "alice@example.com");
    assert_eq!(created.json()["note"], "work login");
    assert_eq!(created.json()["bot_ids"], json!([]));
    assert_eq!(created.json()["ask_before_use"], true);

    // Update bot scope and toggle ask_before_use off -> creates grant.
    let updated = call(
        &r.t,
        Method::PUT,
        "/v1/fill-secrets/login/github",
        Some(json!({ "bot_ids": [bot_a], "ask_before_use": false })),
    )
    .await;
    assert_eq!(updated.status, StatusCode::OK, "{:?}", updated.json());
    assert_eq!(updated.json()["bot_ids"], json!([bot_a]));
    assert_eq!(updated.json()["ask_before_use"], false);
    assert_eq!(r.secrets.grants().len(), 1);

    // Toggle ask_before_use back on -> revokes grant.
    let updated = call(
        &r.t,
        Method::PUT,
        "/v1/fill-secrets/login/github",
        Some(json!({ "ask_before_use": true })),
    )
    .await;
    assert_eq!(updated.status, StatusCode::OK);
    assert_eq!(updated.json()["ask_before_use"], true);
    assert!(r.secrets.grants().is_empty());

    // Reveal refuses token-only caller and unconfirmed request.
    let token_only = super::call_token_only(
        &r.t,
        Method::POST,
        "/v1/fill-secrets/reveal",
        Some(json!({ "name": "login/github", "confirm": true })),
    )
    .await;
    assert_eq!(token_only.status, StatusCode::FORBIDDEN);
    assert_eq!(token_only.json()["error"]["code"], "owner_key_required");

    let unconfirmed = call(
        &r.t,
        Method::POST,
        "/v1/fill-secrets/reveal",
        Some(json!({ "name": "login/github", "confirm": false })),
    )
    .await;
    assert_eq!(unconfirmed.status, StatusCode::BAD_REQUEST);

    // Confirmed owner reveal returns the value, logs audit WITHOUT value, and sets no-store.
    let revealed = call(
        &r.t,
        Method::POST,
        "/v1/fill-secrets/reveal",
        Some(json!({ "name": "login/github", "confirm": true })),
    )
    .await;
    assert_eq!(revealed.status, StatusCode::OK);
    assert_eq!(
        revealed
            .headers
            .get(axum::http::header::CACHE_CONTROL)
            .unwrap(),
        "no-store"
    );
    let body = revealed.json();
    assert_eq!(body["name"], "login/github");
    assert_eq!(body["value"], VALUE);

    let audit = audit_dump(&r.t);
    assert!(audit.contains("fill_secret.reveal"), "{audit}");
    assert!(
        !audit.contains(VALUE) && !audit.contains("Correct-Horse"),
        "{audit}"
    );
}

/// Review of #990, finding 4. The composer stores a pasted value with `POST /v1/fill-secrets` and
/// sends only `{name, value}`. The write is owner-only on the server (a holder of only the API
/// token is refused before anything is written or audited), and what it creates belongs to NO bot
/// until the owner scopes it: not the bot of the chat the value was pasted in, not another bot.
#[tokio::test]
#[allow(clippy::too_many_lines, clippy::assert_is_empty)]
async fn a_pasted_value_is_stored_by_the_owner_only_and_belongs_to_no_bot_until_scoped() {
    use secret_store::FillError;

    let r = rig();
    let new_bot = |name: &'static str| {
        let r = &r;
        async move {
            let profile = super::PROFILE
                .replace("id = \"tester\"", &format!("id = \"{name}\""))
                .replace("name = \"Tester\"", &format!("name = \"{name}\""));
            let resp = call(
                &r.t,
                Method::POST,
                "/v1/bots",
                Some(json!({ "profile_toml": profile })),
            )
            .await;
            assert_eq!(resp.status, StatusCode::CREATED, "{:?}", resp.json());
            resp.json()["id"].as_str().unwrap().to_owned()
        }
    };
    let active = new_bot("active").await;
    let other = new_bot("other").await;
    let name = "pasted/openai-style-key";
    let composer_body = json!({ "name": name, "value": VALUE });
    let before_audit = audit_actions(&r.t).len();

    // A caller with only the API token (a CLI engine, a script) cannot store or scope anything.
    let denied = super::call_token_only(
        &r.t,
        Method::POST,
        "/v1/fill-secrets",
        Some(composer_body.clone()),
    )
    .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    assert_eq!(denied.json()["error"]["code"], "owner_key_required");
    assert!(!txt(&denied).contains(VALUE));
    assert!(r.store.writes.lock().is_empty(), "nothing was written");
    assert_eq!(
        audit_actions(&r.t).len(),
        before_audit,
        "nothing was audited"
    );
    assert!(r.secrets.list().unwrap().is_empty());

    // The owner's request: stored, scoped to nobody, and a bot id that is not one of the owner's
    // bots is refused by the server (nothing written).
    let writes = r.store.writes.lock().len();
    for (bots, status) in [
        (json!(["not-a-bot-id"]), StatusCode::BAD_REQUEST),
        (
            json!(["00000000-0000-4000-8000-000000000000"]),
            StatusCode::NOT_FOUND,
        ),
    ] {
        let refused = call(
            &r.t,
            Method::POST,
            "/v1/fill-secrets",
            Some(json!({ "name": name, "value": VALUE, "bot_ids": bots })),
        )
        .await;
        assert_eq!(refused.status, status, "{:?}", refused.json());
    }
    assert_eq!(
        r.store.writes.lock().len(),
        writes,
        "a refused scope writes nothing"
    );
    let created = call(&r.t, Method::POST, "/v1/fill-secrets", Some(composer_body)).await;
    assert_eq!(created.status, StatusCode::CREATED, "{:?}", created.json());
    assert_eq!(created.json()["bot_ids"], json!([]));
    assert_eq!(created.json()["ask_before_use"], true);

    // No bot can use it: the chat's own bot, another bot, or a caller that names no bot.
    for bot in [Some(active.as_str()), Some(other.as_str()), None] {
        assert!(
            matches!(
                r.secrets.check_bot(name, bot),
                Err(FillError::NotAllowed(_))
            ),
            "{bot:?}"
        );
        assert!(
            matches!(
                r.secrets.resolve_value_for(name, bot),
                Err(FillError::NotAllowed(_))
            ),
            "{bot:?}"
        );
        assert!(!r.secrets.exists_for_bot(name, bot), "{bot:?}");
        assert!(r.secrets.list_for_bot(bot).unwrap().is_empty(), "{bot:?}");
    }

    // Scoping it is the owner's too, and it reaches that bot only.
    let scope = json!({ "bot_ids": [active] });
    let path = format!("/v1/fill-secrets/{name}");
    let denied = super::call_token_only(&r.t, Method::PUT, &path, Some(scope.clone())).await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    assert!(r.secrets.check_bot(name, Some(&active)).is_err());
    let scoped = call(&r.t, Method::PUT, &path, Some(scope)).await;
    assert_eq!(scoped.status, StatusCode::OK, "{:?}", scoped.json());
    assert!(r.secrets.check_bot(name, Some(&active)).is_ok());
    assert!(matches!(
        r.secrets.check_bot(name, Some(&other)),
        Err(FillError::NotAllowed(_))
    ));
    assert!(matches!(
        r.secrets.resolve_value_for(name, Some(&other)),
        Err(FillError::NotAllowed(_))
    ));
    assert!(!audit_dump(&r.t).contains(VALUE));
}
