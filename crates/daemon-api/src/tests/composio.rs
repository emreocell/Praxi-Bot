//! `/v1/composio/*` against an in-memory `ComposioControl`: auth, audit-first
//! mutations, owner-only peer matrix, reserved secret names.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use secret_store::{MemoryStore, SecretStore};
use serde_json::json;
use tower::ServiceExt;

use super::{call, call_token_only, send, setup};
use crate::composio::{
    ComposioActionClass, ComposioActionDto, ComposioAppDto, ComposioCatalogDto,
    ComposioCatalogQuery, ComposioConnState, ComposioConnectDto, ComposioControl,
    ComposioControlError, ComposioDisconnectDto, ComposioLinkMode, ComposioLoginDto,
    ComposioLoginState, ComposioLoginStatusDto, ComposioStatusDto, ComposioTestDto,
};

const KEY: &str = "ck_test_SECRETKEY_0123456789";

type MutationCheck = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Default)]
struct Fake {
    calls: AtomicUsize,
    mutation_check: Option<MutationCheck>,
}

impl Fake {
    fn before(&self, action: &str) {
        if let Some(check) = &self.mutation_check {
            check(action);
        }
    }
}

fn app(slug: &str) -> ComposioAppDto {
    ComposioAppDto {
        account: Some("octocat".into()),
        slug: slug.into(),
        name: slug.into(),
        state: ComposioConnState::Active,
        enabled: true,
        native_first: false,
        bots: None,
        tools: 3,
    }
}

#[async_trait]
impl ComposioControl for Fake {
    async fn status(&self) -> ComposioStatusDto {
        ComposioStatusDto {
            account: None,
            configured: self.calls.load(Ordering::SeqCst) > 0,
            mode: ComposioLinkMode::None,
            key_setup_url: "https://platform.composio.dev/".into(),
            login_expired: false,
        }
    }
    async fn login_start(&self) -> Result<ComposioLoginDto, ComposioControlError> {
        self.before("composio.login_started");
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ComposioLoginDto {
            login_url: "https://dashboard.composio.dev/?cliKey=x".into(),
            expires_at: String::new(),
        })
    }
    async fn login_poll(&self) -> Result<ComposioLoginStatusDto, ComposioControlError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ComposioLoginStatusDto {
            state: ComposioLoginState::Pending,
        })
    }
    async fn set_key(&self, key: &str) -> Result<(), ComposioControlError> {
        self.before("composio.key_set");
        if key == "bad" {
            return Err(ComposioControlError::Invalid(
                "Composio rejected this API key".into(),
            ));
        }
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn remove_key(&self) -> Result<(), ComposioControlError> {
        self.before("composio.key_removed");
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn catalog(
        &self,
        _: ComposioCatalogQuery,
    ) -> Result<ComposioCatalogDto, ComposioControlError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ComposioCatalogDto {
            items: vec![],
            next_cursor: None,
            source: "bundled".into(),
            updated_at: None,
            refreshing: false,
            error: None,
        })
    }
    fn apps(&self) -> Vec<ComposioAppDto> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        vec![app("github")]
    }

    async fn actions(&self, _: &str) -> Result<Vec<ComposioActionDto>, ComposioControlError> {
        self.before("composio.actions_viewed");
        Ok(vec![ComposioActionDto {
            name: "list_issues".into(),
            description: "List repository issues".into(),
            class: ComposioActionClass::Read,
        }])
    }
    async fn app(&self, slug: &str) -> Result<ComposioAppDto, ComposioControlError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(app(slug))
    }
    async fn connect(&self, slug: &str) -> Result<ComposioConnectDto, ComposioControlError> {
        self.before("composio.connect_started");
        if slug == "expired_login" {
            return Err(ComposioControlError::LoginExpired);
        }
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ComposioConnectDto {
            redirect_url: "https://connect.composio.dev/link/x".into(),
            expires_at: String::new(),
        })
    }
    async fn set_enabled(
        &self,
        slug: &str,
        _: bool,
    ) -> Result<ComposioAppDto, ComposioControlError> {
        self.before("composio.app_enabled");
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(app(slug))
    }
    async fn logo(
        &self,
        slug: &str,
    ) -> Result<Option<crate::composio::ComposioLogo>, ComposioControlError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok((slug == "gmail").then(|| crate::composio::ComposioLogo {
            bytes: [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a].to_vec(),
            content_type: "image/png",
        }))
    }

    async fn disconnect(&self, _: &str) -> Result<ComposioDisconnectDto, ComposioControlError> {
        self.before("composio.disconnected");
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ComposioDisconnectDto {
            remote_removed: true,
        })
    }

    async fn test(&self, slug: &str) -> Result<ComposioTestDto, ComposioControlError> {
        self.before("composio.app_test");
        self.calls.fetch_add(1, Ordering::SeqCst);
        if slug == "expired" {
            return Err(ComposioControlError::Invalid(
                "the app is not connected".into(),
            ));
        }
        let authorization_only = slug == "slack";
        Ok(ComposioTestDto {
            authorization_only,
            account: Some("me@example.com".into()),
            items: if authorization_only {
                vec![]
            } else {
                vec!["Budget".into()]
            },
        })
    }

    async fn set_bots(
        &self,
        slug: &str,
        bots: Option<Vec<String>>,
    ) -> Result<ComposioAppDto, ComposioControlError> {
        self.before("composio.app_bots");
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ComposioAppDto { bots, ..app(slug) })
    }
}

fn routes() -> Vec<(Method, &'static str, Option<serde_json::Value>)> {
    vec![
        (Method::GET, "/v1/composio", None),
        (Method::POST, "/v1/composio/login", None),
        (Method::GET, "/v1/composio/login", None),
        (Method::PUT, "/v1/composio/key", Some(json!({"value": KEY}))),
        (Method::DELETE, "/v1/composio/key", None),
        (Method::GET, "/v1/composio/toolkits?search=ins", None),
        (Method::GET, "/v1/composio/toolkits/github/actions", None),
        (Method::GET, "/v1/composio/apps", None),
        (Method::GET, "/v1/composio/apps/github", None),
        (Method::POST, "/v1/composio/apps/github/connect", None),
        (
            Method::PUT,
            "/v1/composio/apps/github/enabled",
            Some(json!({"enabled": false})),
        ),
        (
            Method::PUT,
            "/v1/composio/apps/github/bots",
            Some(json!({"bots": null})),
        ),
        (Method::POST, "/v1/composio/apps/github/test", None),
        (Method::DELETE, "/v1/composio/apps/github", None),
        (Method::GET, "/v1/composio/logos/gmail", None),
    ]
}

#[tokio::test]
async fn a_logo_is_served_to_the_local_owner_with_nosniff_and_a_locked_down_csp() {
    let t = setup();
    t.state.set_composio(Arc::new(Fake::default()));
    let r = call(&t, Method::GET, "/v1/composio/logos/gmail", None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.headers["content-type"], "image/png");
    assert_eq!(r.headers["x-content-type-options"], "nosniff");
    assert!(
        r.headers["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("default-src 'none'")
    );
    // No logo known for the app: 404, not an error page. A malformed slug never reaches the service.
    let none = call(&t, Method::GET, "/v1/composio/logos/unknownapp", None).await;
    assert_eq!(none.status, StatusCode::NOT_FOUND);
    let bad = call(
        &t,
        Method::GET,
        "/v1/composio/logos/..%2F..%2Fsecrets",
        None,
    )
    .await;
    assert!(matches!(
        bad.status,
        StatusCode::BAD_REQUEST | StatusCode::NOT_FOUND
    ));
    let denied = send(&t.app, Method::GET, "/v1/composio/logos/gmail", None, &[]).await;
    assert_eq!(denied.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_rejected_composio_login_is_a_409_with_its_own_code_not_a_bare_502() {
    let t = setup();
    t.state.set_composio(Arc::new(Fake::default()));
    let r = call(
        &t,
        Method::POST,
        "/v1/composio/apps/expired_login/connect",
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.json()["error"]["code"], "composio_login_expired");
}

#[tokio::test]
async fn every_route_needs_the_token_and_works_for_the_local_owner() {
    let t = setup();
    let fake = Arc::new(Fake::default());
    t.state.set_composio(fake.clone());
    for (m, p, _) in routes() {
        let r = send(&t.app, m.clone(), p, None, &[]).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{m} {p}");
    }
    assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
    for (m, p, body) in routes() {
        let r = call(&t, m.clone(), p, body).await;
        assert!(r.status.is_success(), "{m} {p} -> {}", r.status);
    }
}

#[tokio::test]
async fn identities_catalog_logos_action_previews_and_login_poll_require_owner_proof_even_for_head()
{
    let t = setup();
    t.state.set_composio(Arc::new(Fake::default()));
    for path in [
        "/v1/composio",
        "/v1/composio/apps",
        "/v1/composio/apps/github",
        "/v1/composio/toolkits?search=ins",
        "/v1/composio/toolkits/github/actions",
        "/v1/composio/logos/gmail",
        "/v1/composio/login",
    ] {
        for method in [Method::GET, Method::HEAD] {
            let denied = call_token_only(&t, method.clone(), path, None).await;
            assert_eq!(denied.status, StatusCode::FORBIDDEN, "{method} {path}");
            let allowed = call(&t, method.clone(), path, None).await;
            assert_eq!(allowed.status, StatusCode::OK, "{method} {path}");
        }
    }
}

#[tokio::test]
async fn mutations_write_an_audit_row_first_and_never_the_key() {
    let t = setup();
    let state = t.state.clone();
    t.state.set_composio(Arc::new(Fake {
        mutation_check: Some(Arc::new(move |action| {
            let rows = state.storage().list_audit(state.user_id()).unwrap();
            let row = rows.last().expect("audit must exist at mutation entry");
            assert_eq!(row.action, action, "audit must precede this mutation");
            assert_eq!(row.actor, format!("local-ui:{}", state.user_id()));
        })),
        ..Fake::default()
    }));
    for (m, p, body) in routes() {
        let _ = call(&t, m, p, body).await;
    }
    let audit = t.state.storage().list_audit(t.state.user_id()).unwrap();
    let actions: Vec<&str> = audit.iter().map(|a| a.action.as_str()).collect();
    for want in [
        "composio.key_set",
        "composio.key_removed",
        "composio.login_started",
        "composio.connect_started",
        "composio.app_enabled",
        "composio.app_bots",
        "composio.app_test",
        "composio.actions_viewed",
        "composio.disconnected",
    ] {
        assert!(actions.contains(&want), "{want} missing in {actions:?}");
    }
    assert!(!format!("{audit:?}").contains(KEY));
    // A rejected key still leaves its request row, and the error does not echo input.
    let r = call(
        &t,
        Method::PUT,
        "/v1/composio/key",
        Some(json!({"value": "bad"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    let r = call(&t, Method::GET, "/v1/composio/apps/..%2Fx", None).await;
    assert_ne!(r.status, StatusCode::OK);
    let r = call(&t, Method::GET, "/v1/composio/apps/Bad%20Slug", None).await;
    assert_ne!(r.status, StatusCode::OK);
}

#[tokio::test]
async fn without_a_host_the_routes_say_unavailable() {
    let t = setup();
    let r = call(&t, Method::GET, "/v1/composio", None).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn only_the_desktop_peer_reaches_composio_in_every_path_spelling() {
    let t = setup();
    let fake = Arc::new(Fake::default());
    t.state.set_composio(fake.clone());
    let mut spellings: Vec<(Method, String, Option<serde_json::Value>)> = Vec::new();
    for (m, p, b) in routes() {
        spellings.push((m.clone(), p.to_owned(), b.clone()));
        let (path, query) = p.split_once('?').map_or((p, ""), |(a, b)| (a, b));
        let q = if query.is_empty() {
            String::new()
        } else {
            format!("?{query}")
        };
        spellings.push((m.clone(), format!("{path}/{q}"), b.clone()));
        spellings.push((
            m.clone(),
            format!("{}{q}", path.replacen("/v1/", "//v1//", 1)),
            b.clone(),
        ));
        spellings.push((
            m.clone(),
            format!("{}{q}", path.replacen("/v1/composio", "/v1%2Fcomposio", 1)),
            b.clone(),
        ));
        spellings.push((
            m,
            format!("{}{q}", path.replacen("composio", "%63omposio", 1)),
            b,
        ));
    }
    for role in [
        crate::PeerRole::Phone,
        crate::PeerRole::Node,
        crate::PeerRole::Host,
    ] {
        for (m, p, b) in &spellings {
            let mut req = Request::builder()
                .method(m.clone())
                .uri(p.as_str())
                .header("content-type", "application/json")
                .body(Body::from(
                    b.as_ref().map(ToString::to_string).unwrap_or_default(),
                ))
                .unwrap();
            req.extensions_mut().insert(crate::PeerIdentity {
                key: "peer-key".into(),
                role,
            });
            let resp = crate::peer_router(t.state.clone())
                .oneshot(req)
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::FORBIDDEN,
                "role={role:?} {m} {p}"
            );
        }
    }
    assert_eq!(
        fake.calls.load(Ordering::SeqCst),
        0,
        "no peer call reached the service"
    );
    // The owner's Desktop peer is allowed.
    let mut req = Request::builder()
        .method(Method::GET)
        .uri("/v1/composio/apps")
        .body(Body::empty())
        .unwrap();
    req.extensions_mut().insert(crate::PeerIdentity {
        key: "peer-key".into(),
        role: crate::PeerRole::Desktop,
    });
    let resp = crate::peer_router(t.state.clone())
        .oneshot(req)
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn the_reserved_key_name_cannot_be_written_through_the_secrets_route() {
    let t = setup();
    let store = Arc::new(MemoryStore::new());
    t.state.set_secret_store(store.clone());
    for name in ["composio/api-key", "Composio/API-KEY", "composio/anything"] {
        let r = call(
            &t,
            Method::POST,
            &format!("/v1/secrets/{name}"),
            Some(json!({"value": KEY})),
        )
        .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{name}");
    }
    assert_eq!(store.list_names().unwrap(), Vec::<String>::new());
    let r = call(
        &t,
        Method::POST,
        "/v1/secrets/mcp/ok",
        Some(json!({"value": "fine-value"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn failed_audit_prevents_every_mutation_dispatch() {
    let t = setup();
    let fake = Arc::new(Fake::default());
    t.state.set_composio(fake.clone());
    let db = rusqlite::Connection::open(t._dir.path().join("bot.db")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_composio_audit BEFORE INSERT ON audit_log BEGIN SELECT RAISE(ABORT, 'injected audit failure'); END;").unwrap();
    for (method, path, body) in routes()
        .into_iter()
        .filter(|(m, path, _)| *m != Method::GET || path.ends_with("/actions"))
    {
        let r = call(&t, method, path, body).await;
        assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{path}");
    }
    assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn legacy_provider_quarantine_is_visible_in_the_repair_api() {
    let t = setup();
    std::fs::write(t._dir.path().join("providers.toml"), "[[provider]]\nid='legacy'\nkey='composio/legacy-provider-key'\nadded_at_ms=1\n[[provider]]\nid='valid'\nadded_at_ms=2\n").unwrap();
    let response = call(&t, Method::GET, "/v1/providers", None).await;
    assert_eq!(response.status, StatusCode::OK);
    let data = response.json();
    assert_eq!(data["configured"][0]["enabled"], false);
    assert!(
        data["configured"][0]["models_error"]
            .as_str()
            .unwrap()
            .contains("Reserved secret reference")
    );
    assert_eq!(data["configured"][1]["enabled"], true);
}

#[tokio::test]
async fn the_test_and_bot_routes_return_the_account_and_the_chosen_bots_and_name_refusals() {
    let t = setup();
    t.state.set_composio(Arc::new(Fake::default()));
    let r = call(&t, Method::POST, "/v1/composio/apps/github/test", None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["account"], "me@example.com");
    assert_eq!(r.json()["items"], json!(["Budget"]));
    let authorization = call(&t, Method::POST, "/v1/composio/apps/slack/test", None).await;
    assert_eq!(authorization.status, StatusCode::OK);
    assert_eq!(authorization.json()["authorization_only"], true);
    assert_eq!(authorization.json()["items"], json!([]));
    let refused = call(&t, Method::POST, "/v1/composio/apps/expired/test", None).await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(refused.json()["error"]["code"], "bad_composio_request");
    let bots = json!({"bots": ["11111111-1111-1111-1111-111111111111"]});
    let r = call(&t, Method::PUT, "/v1/composio/apps/github/bots", Some(bots)).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        r.json()["bots"],
        json!(["11111111-1111-1111-1111-111111111111"])
    );
    let bad = call(&t, Method::POST, "/v1/composio/apps/Bad%20Slug/test", None).await;
    assert_ne!(bad.status, StatusCode::OK);
}

#[tokio::test]
async fn the_bots_route_refuses_a_body_without_an_explicit_bots_field() {
    let t = setup();
    t.state.set_composio(Arc::new(Fake::default()));
    let uri = "/v1/composio/apps/github/bots";
    for body in [
        json!({}),
        json!({"bot_ids": ["11111111-1111-1111-1111-111111111111"]}),
        json!({"bots": null, "extra": 1}),
        json!({"bots": ["11111111-1111-1111-1111-111111111111"], "bot": []}),
    ] {
        let r = call(&t, Method::PUT, uri, Some(body.clone())).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{body}");
    }
    // An explicit `null` still means every bot, and `[]` nobody.
    for body in [json!({"bots": null}), json!({"bots": []})] {
        let r = call(&t, Method::PUT, uri, Some(body.clone())).await;
        assert_eq!(r.status, StatusCode::OK, "{body}");
    }
}
