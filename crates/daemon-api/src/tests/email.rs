//! `/v1/email/*` against an in-memory `EmailControl`.

use std::sync::Arc;

use async_trait::async_trait;
use axum::http::{Method, StatusCode};
use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio::sync::Semaphore;

use super::{T, TOKEN, call, make_chat, send, setup};
use crate::email::{
    EmailAccountDto, EmailAccountInput, EmailControl, EmailControlError, EmailProbeStatus,
    EmailTestReport,
};

const SECRET: &str = "hunter2-app-password-XYZ";

struct FakeEmail {
    accounts: Mutex<Vec<EmailAccountDto>>,
    /// Passwords the "secret store" holds, by account name.
    passwords: Mutex<Vec<(String, String)>>,
    forgotten: Mutex<Vec<String>>,
    fail_forget: bool,
    /// When set, `test_draft` waits for it (to hold test slots open).
    hold: Option<Arc<Semaphore>>,
    /// Runs at the start of `add` (a bot deleted "in between").
    before_add: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    /// Draft tests that started (before waiting on `hold`).
    entered: std::sync::atomic::AtomicUsize,
}

impl FakeEmail {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            accounts: Mutex::new(Vec::new()),
            passwords: Mutex::new(Vec::new()),
            forgotten: Mutex::new(Vec::new()),
            fail_forget: false,
            hold: None,
            before_add: Mutex::new(None),
            entered: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    fn dto(input: &EmailAccountInput) -> EmailAccountDto {
        EmailAccountDto {
            name: input.name.clone(),
            provider: input.provider,
            inbox_id: None,
            address: input.address.clone(),
            username: input.username.clone().unwrap_or_default(),
            imap: input.imap.clone(),
            smtp: input.smtp.clone(),
            drafts_folder: input
                .drafts_folder
                .clone()
                .unwrap_or_else(|| "Drafts".into()),
            bot: input.bot.clone(),
            allowed_senders: input.allowed_senders.clone(),
            password_set: input.password.is_some(),
            custom_ca: false,
        }
    }
}

#[async_trait]
impl EmailControl for FakeEmail {
    fn list(&self) -> Vec<EmailAccountDto> {
        self.accounts.lock().clone()
    }
    async fn add(&self, input: EmailAccountInput) -> Result<EmailAccountDto, EmailControlError> {
        if let Some(hook) = self.before_add.lock().as_ref() {
            hook();
        }
        let mut all = self.accounts.lock();
        if all.iter().any(|a| a.name == input.name) {
            return Err(EmailControlError::Conflict(format!(
                "{} exists",
                input.name
            )));
        }
        if input.address.is_empty() {
            return Err(EmailControlError::Invalid("invalid sender address".into()));
        }
        self.passwords.lock().push((
            input.name.clone(),
            input.password.clone().unwrap_or_default(),
        ));
        let dto = Self::dto(&input);
        all.push(dto.clone());
        Ok(dto)
    }
    async fn update(&self, name: &str, patch: Value) -> Result<EmailAccountDto, EmailControlError> {
        let mut all = self.accounts.lock();
        let slot = all
            .iter_mut()
            .find(|a| a.name == name)
            .ok_or_else(|| EmailControlError::NotFound(format!("email account {name}")))?;
        if let Some(obj) = patch.as_object() {
            if obj.contains_key("bot") {
                slot.bot = obj["bot"].as_str().map(str::to_owned);
            }
            if let Some(a) = obj.get("address").and_then(Value::as_str) {
                slot.address = a.to_owned();
            }
            if obj.get("password").and_then(Value::as_str).is_some() {
                slot.password_set = true;
            }
        }
        Ok(slot.clone())
    }
    async fn remove(&self, name: &str) -> Result<(), EmailControlError> {
        let mut all = self.accounts.lock();
        let before = all.len();
        all.retain(|a| a.name != name);
        if all.len() == before {
            return Err(EmailControlError::NotFound(format!("email account {name}")));
        }
        Ok(())
    }
    async fn test_saved(&self, name: &str) -> Result<EmailTestReport, EmailControlError> {
        if !self.accounts.lock().iter().any(|a| a.name == name) {
            return Err(EmailControlError::NotFound(format!("email account {name}")));
        }
        Ok(report(EmailProbeStatus::Ok, EmailProbeStatus::AuthFailed))
    }
    async fn test_draft(
        &self,
        input: EmailAccountInput,
    ) -> Result<EmailTestReport, EmailControlError> {
        self.entered
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let Some(hold) = &self.hold {
            hold.acquire().await.unwrap().forget();
        }
        if input.password.is_none() && input.name != "saved" {
            return Err(EmailControlError::Invalid("no password".into()));
        }
        Ok(report(EmailProbeStatus::Ok, EmailProbeStatus::Ok))
    }
    async fn forget_bot(&self, bot_id: &str) -> Result<usize, EmailControlError> {
        self.forgotten.lock().push(bot_id.to_owned());
        if self.fail_forget {
            return Err(EmailControlError::Internal("disk full".into()));
        }
        Ok(0)
    }
}

fn report(imap: EmailProbeStatus, smtp: EmailProbeStatus) -> EmailTestReport {
    EmailTestReport {
        imap,
        smtp,
        api: None,
        ok: imap == EmailProbeStatus::Ok && smtp == EmailProbeStatus::Ok,
    }
}

fn with_email() -> (T, Arc<FakeEmail>) {
    let t = setup();
    let fake = FakeEmail::new();
    t.state.set_email(fake.clone());
    (t, fake)
}

fn body(name: &str) -> Value {
    json!({
        "name": name,
        "address": "me@example.com",
        "imap": {"host": "imap.example.com"},
        "smtp": {"host": "smtp.example.com", "port": 587, "security": "start_tls"},
        "password": SECRET,
    })
}

#[tokio::test]
async fn email_routes_need_the_bearer_token_and_a_good_origin() {
    let (t, _) = with_email();
    for (m, p) in [
        (Method::GET, "/v1/email/accounts"),
        (Method::POST, "/v1/email/accounts"),
        (Method::PUT, "/v1/email/accounts/x"),
        (Method::DELETE, "/v1/email/accounts/x"),
        (Method::POST, "/v1/email/accounts/x/test"),
        (Method::POST, "/v1/email/test"),
    ] {
        let r = send(&t.app, m.clone(), p, Some(body("x")), &[]).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{m} {p}");
        // A web page on another origin is refused even with the token.
        let auth = format!("Bearer {TOKEN}");
        let r = send(
            &t.app,
            m.clone(),
            p,
            Some(body("x")),
            &[
                ("origin", "https://evil.com"),
                ("authorization", auth.as_str()),
            ],
        )
        .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{m} {p}");
        // So is a rebinding Host.
        let r = send(
            &t.app,
            m.clone(),
            p,
            Some(body("x")),
            &[("host", "evil.com"), ("authorization", auth.as_str())],
        )
        .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{m} {p}");
    }
}

#[tokio::test]
async fn without_an_embedder_the_routes_answer_503() {
    let t = setup();
    for (m, p) in [
        (Method::GET, "/v1/email/accounts"),
        (Method::DELETE, "/v1/email/accounts/x"),
        (Method::POST, "/v1/email/accounts/x/test"),
    ] {
        let r = call(&t, m, p, None).await;
        assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{p}");
    }
    let r = call(&t, Method::POST, "/v1/email/accounts", Some(body("x"))).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn account_crud_follows_the_contract_and_never_echoes_the_password() {
    let (t, fake) = with_email();
    let mut every_body: Vec<Vec<u8>> = Vec::new();

    let r = call(&t, Method::GET, "/v1/email/accounts", None).await;
    assert_eq!((r.status, r.json()), (StatusCode::OK, json!([])));

    // A password is required to create.
    let mut no_pw = body("work");
    no_pw.as_object_mut().unwrap().remove("password");
    let r = call(&t, Method::POST, "/v1/email/accounts", Some(no_pw)).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    for bad in ["", "a\r\nb", "x\0y", &"p".repeat(1025)] {
        let mut b = body("work");
        b["password"] = json!(bad);
        let r = call(&t, Method::POST, "/v1/email/accounts", Some(b)).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad:?}");
        every_body.push(r.body);
    }
    assert!(fake.accounts.lock().is_empty());

    let r = call(&t, Method::POST, "/v1/email/accounts", Some(body("work"))).await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.json());
    let a = r.json();
    assert_eq!(a["name"], "work");
    assert_eq!(a["smtp"]["security"], "start_tls");
    assert_eq!(a["imap"]["port"], 993, "default port");
    assert_eq!(a["password_set"], true);
    assert!(a["bot"].is_null() && a.get("bot").is_some());
    assert!(a.get("password").is_none());
    every_body.push(r.body);
    // The embedder did receive it (it stores it); the response did not carry it.
    assert_eq!(fake.passwords.lock()[0].1, SECRET);

    let r = call(&t, Method::POST, "/v1/email/accounts", Some(body("work"))).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    every_body.push(r.body);
    let mut invalid = body("other");
    invalid["address"] = json!("");
    let r = call(&t, Method::POST, "/v1/email/accounts", Some(invalid)).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    every_body.push(r.body);

    let r = call(&t, Method::GET, "/v1/email/accounts", None).await;
    assert_eq!(r.json().as_array().unwrap().len(), 1);
    every_body.push(r.body);

    let r = call(
        &t,
        Method::PUT,
        "/v1/email/accounts/work",
        Some(json!({"address": "new@example.com", "password": SECRET})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    assert_eq!(r.json()["address"], "new@example.com");
    every_body.push(r.body);
    for bad in [
        json!({"password": 5}),
        json!({"password": "a\nb"}),
        json!({"bot": 7}),
    ] {
        let r = call(&t, Method::PUT, "/v1/email/accounts/work", Some(bad)).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST);
    }
    let r = call(&t, Method::PUT, "/v1/email/accounts/nope", Some(json!({}))).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    let r = call(&t, Method::DELETE, "/v1/email/accounts/work", None).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r = call(&t, Method::DELETE, "/v1/email/accounts/work", None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    for b in every_body {
        let text = String::from_utf8_lossy(&b);
        assert!(!text.contains(SECRET), "password leaked: {text}");
    }
}

#[tokio::test]
async fn a_bot_binding_must_name_an_existing_bot_and_is_stored_canonically() {
    let (t, _) = with_email();
    let (bot, _chat) = make_chat(&t).await;

    let mut b = body("mine");
    b["bot"] = json!(bot.to_uppercase());
    let r = call(&t, Method::POST, "/v1/email/accounts", Some(b)).await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.json());
    assert_eq!(r.json()["bot"], bot.as_str(), "lower-case hyphenated form");

    let mut b = body("ghost");
    b["bot"] = json!("33333333-3333-4333-8333-333333333333");
    let r = call(&t, Method::POST, "/v1/email/accounts", Some(b)).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let mut b = body("junk");
    b["bot"] = json!("not-a-uuid");
    let r = call(&t, Method::POST, "/v1/email/accounts", Some(b)).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    let r = call(
        &t,
        Method::PUT,
        "/v1/email/accounts/mine",
        Some(json!({"bot": "33333333-3333-4333-8333-333333333333"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // `null` is the explicit "every bot".
    let r = call(
        &t,
        Method::PUT,
        "/v1/email/accounts/mine",
        Some(json!({"bot": null})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.json()["bot"].is_null());
}

#[tokio::test]
async fn connection_tests_report_coarse_status_and_are_rate_limited() {
    let t = setup();
    let hold = Arc::new(Semaphore::new(0));
    let fake = Arc::new(FakeEmail {
        hold: Some(hold.clone()),
        ..Arc::try_unwrap(FakeEmail::new()).ok().unwrap()
    });
    t.state.set_email(fake.clone());

    // Saved test: a normal 200 with a status per server.
    let r = call(&t, Method::POST, "/v1/email/accounts", Some(body("saved"))).await;
    assert_eq!(r.status, StatusCode::CREATED);
    let r = call(&t, Method::POST, "/v1/email/accounts/saved/test", None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        r.json(),
        json!({"imap": "ok", "smtp": "auth_failed", "ok": false})
    );
    assert!(!String::from_utf8_lossy(&r.body).contains(SECRET));
    let r = call(&t, Method::POST, "/v1/email/accounts/nope/test", None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    // Two draft tests hold the slots; a third is refused until one finishes.
    let mut running = Vec::new();
    for _ in 0..2 {
        let app = t.app.clone();
        running.push(tokio::spawn(async move {
            let auth = format!("Bearer {TOKEN}");
            send(
                &app,
                Method::POST,
                "/v1/email/test",
                Some(body("draft")),
                &[("authorization", auth.as_str())],
            )
            .await
        }));
    }
    while fake.entered.load(std::sync::atomic::Ordering::SeqCst) < 2 {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let refused = call(&t, Method::POST, "/v1/email/test", Some(body("draft"))).await;
    assert_eq!(refused.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(refused.json()["error"]["code"], "too_many_requests");
    hold.add_permits(2);
    for h in running {
        assert_eq!(h.await.unwrap().status, StatusCode::OK);
    }
    // The slots are free again once the held tests finished.
    hold.add_permits(1);
    let r = call(&t, Method::POST, "/v1/email/test", Some(body("draft"))).await;
    assert_eq!(r.status, StatusCode::OK);
}
#[tokio::test]
async fn a_draft_test_takes_the_password_from_the_body_or_the_stored_account() {
    let (t, _) = with_email();
    let r = call(&t, Method::POST, "/v1/email/test", Some(body("x"))).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["ok"], true);
    // No password and nothing stored under that name: refused.
    let mut b = body("x");
    b.as_object_mut().unwrap().remove("password");
    let r = call(&t, Method::POST, "/v1/email/test", Some(b.clone())).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    // The stored one is reused by name.
    b["name"] = json!("saved");
    let r = call(&t, Method::POST, "/v1/email/test", Some(b)).await;
    assert_eq!(r.status, StatusCode::OK);
    // A bad password never reaches the embedder.
    let mut b = body("x");
    b["password"] = json!("a\r\nb");
    let r = call(&t, Method::POST, "/v1/email/test", Some(b)).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn deleting_a_bot_asks_the_embedder_to_forget_its_accounts() {
    let t = setup();
    let fake = Arc::new(FakeEmail {
        fail_forget: true,
        ..Arc::try_unwrap(FakeEmail::new()).ok().unwrap()
    });
    t.state.set_email(fake.clone());
    let (bot, _) = make_chat(&t).await;
    // A bot without chats can be deleted: make a fresh one.
    let r = call(
        &t,
        Method::POST,
        "/v1/bots",
        Some(json!({"profile_toml": "[bot]\nid = \"spare\"\nname = \"Spare\"\n[models]\nexecutor = \"fake/echo\"\n"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.json());
    let spare = r.json()["id"].as_str().unwrap().to_owned();
    assert_ne!(spare, bot);
    let r = call(&t, Method::DELETE, &format!("/v1/bots/{spare}"), None).await;
    // The deletion itself succeeds even when the cleanup failed (the leftover
    // account is bound to an id that is never reused, so it stays inert).
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(*fake.forgotten.lock(), vec![spare]);
}

#[test]
fn input_debug_never_prints_the_password() {
    let input: EmailAccountInput = serde_json::from_value(body("x")).unwrap();
    assert!(!format!("{input:?}").contains(SECRET));
    let patch: crate::email::EmailAccountPatch =
        serde_json::from_value(json!({"password": SECRET})).unwrap();
    assert!(!format!("{patch:?}").contains(SECRET));
}

#[tokio::test]
async fn an_account_cannot_end_up_bound_to_a_bot_deleted_during_creation() {
    let (t, fake) = with_email();
    let r = call(
        &t,
        Method::POST,
        "/v1/bots",
        Some(json!({"profile_toml": "[bot]
id = \"racy\"
name = \"Racy\"
[models]
executor = \"fake/echo\"
"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.json());
    let bot = r.json()["id"].as_str().unwrap().to_owned();
    // The bot is deleted after the route checked it and before the insert.
    let state = t.state.clone();
    let id: uuid::Uuid = bot.parse().unwrap();
    *fake.before_add.lock() = Some(Box::new(move || {
        state.storage().delete_bot(protocol::BotId(id)).unwrap();
    }));
    let mut b = body("racy");
    b["bot"] = json!(bot);
    let r = call(&t, Method::POST, "/v1/email/accounts", Some(b)).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{:?}", r.json());
    assert!(
        fake.accounts.lock().is_empty(),
        "the account was taken back"
    );
}

// ---- AgentMail kind ---------------------------------------------------------

const API_KEY: &str = "am_route_test_KEY_5521";

/// Records `remove_account` calls; can fail like an unhappy provider.
struct RemoteFake {
    calls: Mutex<Vec<(String, bool)>>,
    fail_with: Option<EmailControlError>,
}

#[async_trait]
impl EmailControl for RemoteFake {
    fn list(&self) -> Vec<EmailAccountDto> {
        Vec::new()
    }
    async fn add(&self, input: EmailAccountInput) -> Result<EmailAccountDto, EmailControlError> {
        let mut dto = FakeEmail::dto(&input);
        dto.address = "created@agentmail.to".into();
        dto.inbox_id = Some("created@agentmail.to".into());
        dto.password_set = input.api_key.is_some();
        Ok(dto)
    }
    async fn update(&self, _: &str, _: Value) -> Result<EmailAccountDto, EmailControlError> {
        Err(EmailControlError::NotFound("x".into()))
    }
    async fn remove(&self, name: &str) -> Result<(), EmailControlError> {
        self.remove_account(name, false).await
    }
    async fn remove_account(&self, name: &str, remote: bool) -> Result<(), EmailControlError> {
        self.calls.lock().push((name.to_owned(), remote));
        self.fail_with.clone().map_or(Ok(()), Err)
    }
    async fn test_saved(&self, _: &str) -> Result<EmailTestReport, EmailControlError> {
        Ok(report(EmailProbeStatus::Ok, EmailProbeStatus::Ok))
    }
    async fn test_draft(&self, _: EmailAccountInput) -> Result<EmailTestReport, EmailControlError> {
        Ok(report(EmailProbeStatus::Ok, EmailProbeStatus::Ok))
    }
    async fn forget_bot(&self, _: &str) -> Result<usize, EmailControlError> {
        Ok(0)
    }
}

fn remote(fail_with: Option<EmailControlError>) -> (T, Arc<RemoteFake>) {
    let t = setup();
    let fake = Arc::new(RemoteFake {
        calls: Mutex::new(Vec::new()),
        fail_with,
    });
    t.state.set_email(fake.clone());
    (t, fake)
}

#[tokio::test]
async fn agentmail_create_needs_a_well_formed_write_only_api_key() {
    let (t, _) = remote(None);
    let ok = json!({"name":"bot-mail","provider":"agentmail","api_key":API_KEY,"name_hint":"helper"});
    let r = call(&t, Method::POST, "/v1/email/accounts", Some(ok)).await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.json());
    let a = r.json();
    assert_eq!(a["provider"], "agentmail");
    assert_eq!(a["address"], "created@agentmail.to");
    assert_eq!(a["password_set"], true);
    assert!(!String::from_utf8_lossy(&r.body).contains(API_KEY), "the key is never echoed");

    let no_key = json!({"name":"bot-mail","provider":"agentmail"});
    let r = call(&t, Method::POST, "/v1/email/accounts", Some(no_key)).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    for bad in ["", "a\r\nb", "x\0y", &"k".repeat(1025)] {
        let b = json!({"name":"bot-mail","provider":"agentmail","api_key":bad});
        let r = call(&t, Method::POST, "/v1/email/accounts", Some(b)).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad:?}");
        assert!(!String::from_utf8_lossy(&r.body).contains("kkkk"));
    }
    let r = call(
        &t,
        Method::PUT,
        "/v1/email/accounts/bot-mail",
        Some(json!({"api_key": 5})),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn deleting_an_account_asks_for_the_remote_inbox_only_when_told_to() {
    let (t, fake) = remote(None);
    let r = call(&t, Method::DELETE, "/v1/email/accounts/bot-mail", None).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r = call(
        &t,
        Method::DELETE,
        "/v1/email/accounts/bot-mail?delete_inbox=true",
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(
        *fake.calls.lock(),
        vec![("bot-mail".to_owned(), false), ("bot-mail".to_owned(), true)],
        "the default never deletes remotely"
    );
}

#[tokio::test]
async fn a_failing_provider_is_a_502_with_a_coarse_message() {
    let (t, _) = remote(Some(EmailControlError::Upstream(
        "the provider rejected the API key".into(),
    )));
    let r = call(
        &t,
        Method::DELETE,
        "/v1/email/accounts/bot-mail?delete_inbox=true",
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_GATEWAY);
    assert_eq!(r.json()["error"]["code"], "email_provider_error");
}

#[test]
fn input_and_patch_debug_never_print_the_api_key() {
    let input: EmailAccountInput =
        serde_json::from_value(json!({"name":"x","provider":"agentmail","api_key":API_KEY}))
            .unwrap();
    assert!(!format!("{input:?}").contains(API_KEY));
    let patch: crate::email::EmailAccountPatch =
        serde_json::from_value(json!({"api_key": API_KEY})).unwrap();
    assert!(!format!("{patch:?}").contains(API_KEY));
}

#[test]
fn old_clients_that_send_no_provider_still_mean_imap_smtp() {
    let input: EmailAccountInput = serde_json::from_value(body("x")).unwrap();
    assert_eq!(input.provider, crate::email::EmailProviderKind::ImapSmtp);
}
