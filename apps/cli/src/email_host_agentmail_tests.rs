//! `AgentMail` accounts in [`super::EmailHost`], against a local mock of the
//! provider API (no real service, no real key).

use secret_store::MemoryStore;
use serde_json::{Value, json};
use tool_registry::{ContentPart, ToolContext, ToolError};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;

const BOT_A: &str = "11111111-1111-4111-8111-111111111111";
const BOT_B: &str = "22222222-2222-4222-8222-222222222222";
const KEY: &str = "am_host_test_KEY_c0ffee99";
const NEW_KEY: &str = "am_host_test_KEY_NEW_deadbeef";

struct Fixture {
    dir: tempfile::TempDir,
    store: Arc<dyn SecretStore>,
    registry: ToolRegistry,
    host: Arc<EmailHost>,
}

fn fixture(server: &MockServer) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn SecretStore> = Arc::new(MemoryStore::new());
    let registry = ToolRegistry::new();
    let endpoint = AgentMailEndpoint::loopback_mock_for_tests(&server.uri()).unwrap();
    let (host, _) = start_email_with(dir.path(), &registry, Some(&store), endpoint).unwrap();
    Fixture {
        dir,
        store,
        registry,
        host,
    }
}

fn input(name: &str, bot: Option<&str>) -> EmailAccountInput {
    serde_json::from_value(json!({
        "name": name, "provider": "agentmail", "api_key": KEY,
        "name_hint": "helper", "bot": bot,
    }))
    .unwrap()
}

fn inbox_json(addr: &str) -> Value {
    json!({
        "pod_id": "p", "inbox_id": addr, "email": addr,
        "updated_at": "2026-11-12T10:00:00Z", "created_at": "2026-11-12T10:00:00Z"
    })
}

async fn mock_create(server: &MockServer, addr: &str) {
    Mock::given(method("POST"))
        .and(path("/v0/inboxes"))
        .and(header("authorization", format!("Bearer {KEY}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(inbox_json(addr)))
        .mount(server)
        .await;
}

fn toml_of(f: &Fixture) -> String {
    std::fs::read_to_string(f.dir.path().join(EMAIL_FILE)).unwrap()
}

fn secret(f: &Fixture, name: &str) -> Option<String> {
    f.store.get(name).unwrap().map(|v| v.expose().to_owned())
}

async fn deletes(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.method.as_str() == "DELETE")
        .count()
}

#[tokio::test]
async fn adding_an_agentmail_account_creates_the_inbox_and_keeps_the_key_in_the_secret_store() {
    let server = MockServer::start().await;
    mock_create(&server, "helper@agentmail.to").await;
    let f = fixture(&server);
    let dto = f.host.add(input("bot-mail", Some(BOT_A))).await.unwrap();
    assert_eq!(dto.provider, EmailProviderKind::Agentmail);
    assert_eq!(dto.address, "helper@agentmail.to");
    assert_eq!(dto.inbox_id.as_deref(), Some("helper@agentmail.to"));
    assert_eq!(dto.bot.as_deref(), Some(BOT_A));
    assert!(dto.password_set);
    assert_eq!(dto.imap.host, "");

    let text = toml_of(&f);
    assert!(text.contains("kind = \"agentmail\""), "{text}");
    assert!(
        text.contains("inbox_id = \"helper@agentmail.to\""),
        "{text}"
    );
    assert!(text.contains("email/bot-mail/api_key"), "{text}");
    assert!(!text.contains(KEY), "the key is never in email.toml");
    assert_eq!(secret(&f, "email/bot-mail/api_key").as_deref(), Some(KEY));
    assert!(secret(&f, "email/bot-mail/password").is_none());
    let json = serde_json::to_string(&f.host.list()).unwrap();
    assert!(!json.contains(KEY), "{json}");

    // The body of the create request carries the hint and an idempotency id.
    let reqs = server.received_requests().await.unwrap();
    let body: Value = serde_json::from_slice(&reqs[0].body).unwrap();
    assert_eq!(body["username"], "helper");
    assert!(body["client_id"].as_str().unwrap().starts_with("praxi-"));

    // A restart sees the same account.
    let again = EmailHost::open(f.dir.path(), Arc::clone(&f.store));
    assert_eq!(again.list(), f.host.list());
}

#[tokio::test]
async fn invalid_sender_allowlist_never_creates_a_remote_inbox() {
    let server = MockServer::start().await;
    mock_create(&server, "helper@agentmail.to").await;
    let f = fixture(&server);
    let mut request = input("bot-mail", Some(BOT_A));
    request.allowed_senders = vec!["not-an-address".to_owned()];
    let err = f.host.add(request).await.unwrap_err();
    assert!(matches!(err, EmailControlError::Invalid(_)), "{err:?}");
    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "invalid local configuration must fail before any provider request"
    );
    assert_eq!(f.host.list().len(), 0);
    assert!(secret(&f, "email/bot-mail/api_key").is_none());
    assert!(!f.dir.path().join(EMAIL_FILE).exists());
}

#[tokio::test]
async fn a_provider_refusal_saves_nothing_and_never_echoes_the_key() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "code": "unknown_api_key", "message": format!("bad key {KEY}")
        })))
        .mount(&server)
        .await;
    let f = fixture(&server);
    let e = f.host.add(input("bot-mail", None)).await.unwrap_err();
    let EmailControlError::Upstream(m) = &e else {
        panic!("{e:?}")
    };
    assert!(m.contains("rejected the API key"), "{m}");
    assert!(!format!("{e:?}").contains(KEY));
    assert_eq!(f.host.list().len(), 0);
    assert!(secret(&f, "email/bot-mail/api_key").is_none());
    assert!(!f.dir.path().join(EMAIL_FILE).exists());
}

#[tokio::test]
async fn a_retried_create_uses_one_client_id_so_only_one_inbox_exists() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    mock_create(&server, "helper@agentmail.to").await;
    let f = fixture(&server);
    f.host.add(input("bot-mail", None)).await.unwrap();
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 2);
    let ids: Vec<Value> = reqs
        .iter()
        .map(|r| serde_json::from_slice::<Value>(&r.body).unwrap()["client_id"].clone())
        .collect();
    assert_eq!(ids[0], ids[1]);
    assert_eq!(f.host.list().len(), 1);
}

#[tokio::test]
async fn a_name_that_is_taken_is_a_conflict_and_creates_no_second_inbox() {
    let server = MockServer::start().await;
    mock_create(&server, "helper@agentmail.to").await;
    let f = fixture(&server);
    f.host.add(input("bot-mail", None)).await.unwrap();
    let e = f.host.add(input("bot-mail", None)).await.unwrap_err();
    assert!(matches!(e, EmailControlError::Conflict(_)), "{e:?}");
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn when_saving_fails_the_freshly_created_inbox_is_taken_back() {
    let server = MockServer::start().await;
    mock_create(&server, "helper@agentmail.to").await;
    Mock::given(method("DELETE"))
        .and(path("/v0/inboxes/helper%40agentmail.to"))
        .respond_with(ResponseTemplate::new(202))
        .expect(1)
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    // An unreadable email.toml makes every save fail.
    std::fs::write(dir.path().join(EMAIL_FILE), "this is not toml [").unwrap();
    let store: Arc<dyn SecretStore> = Arc::new(MemoryStore::new());
    let registry = ToolRegistry::new();
    let endpoint = AgentMailEndpoint::loopback_mock_for_tests(&server.uri()).unwrap();
    let (host, _) = start_email_with(dir.path(), &registry, Some(&store), endpoint).unwrap();
    let e = host.add(input("bot-mail", None)).await.unwrap_err();
    assert!(matches!(e, EmailControlError::Internal(_)), "{e:?}");
    assert!(store.get("email/bot-mail/api_key").unwrap().is_none());
    assert_eq!(deletes(&server).await, 1);
}

#[tokio::test]
async fn removing_an_account_never_deletes_the_remote_inbox_unless_asked() {
    let server = MockServer::start().await;
    mock_create(&server, "helper@agentmail.to").await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(202))
        .mount(&server)
        .await;
    let f = fixture(&server);
    f.host.add(input("one", None)).await.unwrap();
    f.host.remove("one").await.unwrap();
    assert_eq!(deletes(&server).await, 0, "default: local only");
    assert!(
        secret(&f, "email/one/api_key").is_none(),
        "the stored key is removed"
    );

    f.host.add(input("two", None)).await.unwrap();
    f.host.remove_account("two", true).await.unwrap();
    assert_eq!(deletes(&server).await, 1);
    assert_eq!(f.host.list().len(), 0);
    assert!(secret(&f, "email/two/api_key").is_none());
}

#[tokio::test]
async fn a_failed_remote_delete_keeps_the_account_and_a_missing_inbox_counts_as_done() {
    let server = MockServer::start().await;
    mock_create(&server, "helper@agentmail.to").await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"code":"not_found"})))
        .mount(&server)
        .await;
    let f = fixture(&server);
    f.host.add(input("one", None)).await.unwrap();
    let e = f.host.remove_account("one", true).await.unwrap_err();
    assert!(matches!(e, EmailControlError::Upstream(_)), "{e:?}");
    assert_eq!(f.host.list().len(), 1, "kept so the user can retry");
    assert!(secret(&f, "email/one/api_key").is_some());
    f.host.remove_account("one", true).await.unwrap();
    assert_eq!(f.host.list().len(), 0);
}

#[tokio::test]
async fn the_remote_delete_flag_is_refused_for_imap_accounts() {
    let server = MockServer::start().await;
    let f = fixture(&server);
    let port = std::net::TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let imap: EmailAccountInput = serde_json::from_value(json!({
        "name": "mine", "address": "me@example.com",
        "imap": {"host": "127.0.0.1", "port": port},
        "smtp": {"host": "127.0.0.1", "port": port}, "password": "pw-pw-pw-pw",
    }))
    .unwrap();
    f.host.add(imap).await.unwrap();
    let e = f.host.remove_account("mine", true).await.unwrap_err();
    assert!(matches!(e, EmailControlError::Invalid(_)), "{e:?}");
    assert_eq!(f.host.list().len(), 1);
}

#[tokio::test]
async fn deleting_a_bot_removes_its_agentmail_accounts_and_keys_but_never_the_remote_inbox() {
    let server = MockServer::start().await;
    mock_create(&server, "helper@agentmail.to").await;
    let f = fixture(&server);
    f.host.add(input("a-mail", Some(BOT_A))).await.unwrap();
    f.host.add(input("b-mail", Some(BOT_B))).await.unwrap();
    assert_eq!(f.host.forget_bot(BOT_A).await.unwrap(), 1);
    let left: Vec<String> = f.host.list().into_iter().map(|a| a.name).collect();
    assert_eq!(left, ["b-mail"]);
    assert!(secret(&f, "email/a-mail/api_key").is_none());
    assert!(secret(&f, "email/b-mail/api_key").is_some());
    assert_eq!(
        deletes(&server).await,
        0,
        "no data loss the user did not ask for"
    );
}

#[tokio::test]
async fn testing_an_agentmail_account_is_one_cheap_authenticated_call() {
    let server = MockServer::start().await;
    mock_create(&server, "helper@agentmail.to").await;
    Mock::given(method("GET"))
        .and(path("/v0/inboxes/helper%40agentmail.to"))
        .and(header("authorization", format!("Bearer {KEY}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(inbox_json("helper@agentmail.to")))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v0/inboxes/helper%40agentmail.to"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({"code":"unknown_api_key"})))
        .mount(&server)
        .await;
    let f = fixture(&server);
    f.host.add(input("bot-mail", None)).await.unwrap();
    let ok = f.host.test_saved("bot-mail").await.unwrap();
    assert!(ok.ok);
    assert_eq!(ok.api, Some(EmailProbeStatus::Ok));
    let bad = f.host.test_saved("bot-mail").await.unwrap();
    assert!(!bad.ok);
    assert_eq!(bad.api, Some(EmailProbeStatus::AuthFailed));
    assert!(!serde_json::to_string(&bad).unwrap().contains(KEY));
    let e = f.host.test_saved("nope").await.unwrap_err();
    assert!(matches!(e, EmailControlError::NotFound(_)), "{e:?}");
}

#[tokio::test]
async fn a_draft_test_uses_the_key_in_the_request_or_the_stored_one_of_the_same_account() {
    let server = MockServer::start().await;
    mock_create(&server, "helper@agentmail.to").await;
    Mock::given(method("GET"))
        .and(path("/v0/inboxes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"count":0,"inboxes":[]})))
        .mount(&server)
        .await;
    let f = fixture(&server);
    // No saved account and no key: asked to enter one.
    let none: EmailAccountInput =
        serde_json::from_value(json!({"name":"bot-mail","provider":"agentmail"})).unwrap();
    assert!(matches!(
        f.host.test_draft(none.clone()).await.unwrap_err(),
        EmailControlError::Invalid(_)
    ));
    let with_key: EmailAccountInput =
        serde_json::from_value(json!({"name":"bot-mail","provider":"agentmail","api_key":NEW_KEY}))
            .unwrap();
    assert!(f.host.test_draft(with_key).await.unwrap().ok);
    f.host.add(input("bot-mail", None)).await.unwrap();
    assert!(f.host.test_draft(none).await.unwrap().ok, "stored key");
    assert!(
        secret(&f, "email/bot-mail/api_key").as_deref() == Some(KEY),
        "nothing was stored by a test"
    );
}

#[tokio::test]
async fn an_agentmail_account_only_takes_bot_senders_and_a_new_key_and_the_target_cannot_move() {
    let server = MockServer::start().await;
    mock_create(&server, "helper@agentmail.to").await;
    let f = fixture(&server);
    f.host.add(input("bot-mail", Some(BOT_A))).await.unwrap();
    for patch in [
        json!({"address": "other@agentmail.to"}),
        json!({"imap": {"host": "imap.evil.example"}}),
        json!({"smtp": {"host": "smtp.evil.example"}}),
        json!({"username": "x"}),
        json!({"password": "pw"}),
    ] {
        let e = f.host.update("bot-mail", patch.clone()).await.unwrap_err();
        assert!(matches!(e, EmailControlError::Invalid(_)), "{patch}: {e:?}");
    }
    let before = f.host.list();
    assert_eq!(before[0].inbox_id.as_deref(), Some("helper@agentmail.to"));

    // Rebinding and a new key are fine; the key replaces the stored one.
    let dto = f
        .host
        .update(
            "bot-mail",
            json!({"bot": BOT_B, "allowed_senders": ["Ann@Example.com"], "api_key": NEW_KEY}),
        )
        .await
        .unwrap();
    assert_eq!(dto.bot.as_deref(), Some(BOT_B));
    assert_eq!(dto.allowed_senders, ["ann@example.com"]);
    assert_eq!(
        secret(&f, "email/bot-mail/api_key").as_deref(),
        Some(NEW_KEY)
    );
    assert!(!toml_of(&f).contains(NEW_KEY));

    // An API key on an IMAP account is refused (and a password on this kind above).
    let port = std::net::TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let imap: EmailAccountInput = serde_json::from_value(json!({
        "name": "mine", "address": "me@example.com",
        "imap": {"host": "127.0.0.1", "port": port},
        "smtp": {"host": "127.0.0.1", "port": port}, "password": "pw-pw-pw-pw",
    }))
    .unwrap();
    f.host.add(imap).await.unwrap();
    let e = f
        .host
        .update("mine", json!({"api_key": NEW_KEY}))
        .await
        .unwrap_err();
    assert!(matches!(e, EmailControlError::Invalid(_)), "{e:?}");
}

#[tokio::test]
async fn imap_accounts_still_require_the_password_again_when_the_target_changes() {
    let server = MockServer::start().await;
    let f = fixture(&server);
    let port = std::net::TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let imap: EmailAccountInput = serde_json::from_value(json!({
        "name": "mine", "address": "me@example.com",
        "imap": {"host": "127.0.0.1", "port": port},
        "smtp": {"host": "127.0.0.1", "port": port}, "password": "pw-pw-pw-pw",
    }))
    .unwrap();
    f.host.add(imap).await.unwrap();
    let e = f
        .host
        .update(
            "mine",
            json!({"imap": {"host": "imap.evil.example", "port": 993}}),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&e, EmailControlError::Invalid(m) if m.contains("password again")),
        "{e:?}"
    );
}

async fn call(
    f: &Fixture,
    bot: Option<&str>,
    name: &str,
    input: Value,
) -> Result<Value, ToolError> {
    let tool = f.registry.get(name).unwrap();
    let ctx = ToolContext {
        bot_id: bot.map(str::to_owned),
        ..ToolContext::default()
    };
    let out = tool.call(&ctx, input).await?;
    let ContentPart::Json { data } = &out.content[0] else {
        panic!("json")
    };
    Ok(data.clone())
}

#[tokio::test]
async fn an_agentmail_account_is_isolated_to_its_bot_through_the_registered_tools() {
    let server = MockServer::start().await;
    mock_create(&server, "helper@agentmail.to").await;
    Mock::given(method("GET"))
        .and(path("/v0/inboxes/helper%40agentmail.to/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"count":0,"messages":[]})))
        .mount(&server)
        .await;
    let f = fixture(&server);
    f.host.add(input("bot-mail", Some(BOT_A))).await.unwrap();
    let tool = f.registry.get("email_search").unwrap();
    assert!(tool.visible_to(Some(BOT_A)) && !tool.visible_to(Some(BOT_B)));
    let ok = call(&f, Some(BOT_A), "email_search", json!({}))
        .await
        .unwrap();
    assert_eq!(ok["returned"], 0);
    let before = server.received_requests().await.unwrap().len();
    for bot in [Some(BOT_B), None] {
        let e = call(&f, bot, "email_search", json!({"account": "bot-mail"}))
            .await
            .unwrap_err();
        assert!(format!("{e:?}").contains("unknown account"), "{e:?}");
    }
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        before,
        "another bot never causes a request with this account key"
    );
}

#[tokio::test]
async fn the_owner_lists_headers_and_sends_a_test_only_to_the_mailbox_itself() {
    let server = MockServer::start().await;
    mock_create(&server, "helper@agentmail.to").await;
    Mock::given(method("GET"))
        .and(path("/v0/inboxes/helper%40agentmail.to/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1,
            "messages": [{
                "message_id": "m1", "from": "Ann <ann@example.com>", "to": ["helper@agentmail.to"],
                "subject": "Hello", "timestamp": "2026-11-12T10:00:00Z", "labels": ["unread"]
            }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v0/inboxes/helper%40agentmail.to/messages/send"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"message_id": "sent_1"})))
        .expect(1)
        .mount(&server)
        .await;
    let f = fixture(&server);
    // Bound to a bot: the owner's view still reaches it (as that bot), nobody else does.
    f.host.add(input("bot-mail", Some(BOT_A))).await.unwrap();

    let rows = f.host.recent_messages("bot-mail", 5).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "m1");
    assert_eq!(rows[0].subject, "Hello");
    assert!(rows[0].unread);

    assert_eq!(
        f.host.send_test("bot-mail").await.unwrap(),
        "helper@agentmail.to"
    );
    let reqs = server.received_requests().await.unwrap();
    let sent = reqs
        .iter()
        .find(|r| r.url.path().ends_with("/messages/send"))
        .unwrap();
    let body: Value = serde_json::from_slice(&sent.body).unwrap();
    assert_eq!(body["to"], json!(["helper@agentmail.to"]));

    assert!(matches!(
        f.host.recent_messages("nope", 5).await,
        Err(EmailControlError::NotFound(_))
    ));
}

const SCOPED_KEY: &str = "am_us_inbox_SCOPED_sentinel_9f3a";

fn connect_input(name: &str, inbox: &str, bot: Option<&str>) -> EmailAccountInput {
    serde_json::from_value(json!({
        "name": name, "provider": "agentmail", "api_key": SCOPED_KEY,
        "inbox": inbox, "bot": bot,
    }))
    .unwrap()
}

#[tokio::test]
async fn an_existing_inbox_connects_with_a_scoped_key_and_nothing_is_created() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v0/inboxes/praxi%40agentmail.to"))
        .and(header(
            "authorization",
            format!("Bearer {SCOPED_KEY}").as_str(),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(inbox_json("praxi@agentmail.to")))
        .mount(&server)
        .await;
    let f = fixture(&server);
    let dto = f
        .host
        .add(connect_input(
            "praxi-mail",
            "praxi@agentmail.to",
            Some(BOT_A),
        ))
        .await
        .unwrap();
    assert_eq!(dto.provider, EmailProviderKind::Agentmail);
    assert_eq!(dto.address, "praxi@agentmail.to");
    assert_eq!(dto.inbox_id.as_deref(), Some("praxi@agentmail.to"));
    assert_eq!(dto.bot.as_deref(), Some(BOT_A));
    assert!(dto.password_set);
    // No create call, no delete call: only the one read.
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].method.as_str(), "GET");
    // The key is in the secret store only, never in the file, the list or a restart's view.
    assert_eq!(
        secret(&f, "email/praxi-mail/api_key").as_deref(),
        Some(SCOPED_KEY)
    );
    let text = toml_of(&f);
    assert!(text.contains("inbox_id = \"praxi@agentmail.to\""), "{text}");
    assert!(!text.contains(SCOPED_KEY), "{text}");
    assert!(
        !serde_json::to_string(&f.host.list())
            .unwrap()
            .contains(SCOPED_KEY)
    );
    // Connecting the same inbox twice is refused without another account.
    let e = f
        .host
        .add(connect_input("again", "praxi@agentmail.to", None))
        .await
        .unwrap_err();
    assert!(matches!(e, EmailControlError::Conflict(_)), "{e:?}");
    assert_eq!(f.host.list().len(), 1);
}

#[tokio::test]
async fn connecting_an_existing_inbox_reports_a_forbidden_or_unknown_inbox_without_saving() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v0/inboxes/other%40agentmail.to"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "name": "ForbiddenError", "message": format!("key {SCOPED_KEY} may not")
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v0/inboxes/missing%40agentmail.to"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "name": "NotFoundError", "message": format!("no inbox for {SCOPED_KEY}")
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v0/inboxes/revoked%40agentmail.to"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({"code": "unknown_api_key"})))
        .mount(&server)
        .await;
    let f = fixture(&server);

    let e = f
        .host
        .add(connect_input("a", "other@agentmail.to", None))
        .await
        .unwrap_err();
    let EmailControlError::Upstream(m) = &e else {
        panic!("{e:?}")
    };
    assert!(m.contains("not allowed"), "{m}");

    let e = f
        .host
        .add(connect_input("b", "missing@agentmail.to", None))
        .await
        .unwrap_err();
    let EmailControlError::Invalid(m) = &e else {
        panic!("{e:?}")
    };
    assert!(m.contains("no inbox with that address"), "{m}");

    let e = f
        .host
        .add(connect_input("c", "revoked@agentmail.to", None))
        .await
        .unwrap_err();
    assert!(matches!(e, EmailControlError::Upstream(_)), "{e:?}");

    // Whatever was refused, the key is in no error text and nothing was saved.
    for name in ["a", "b", "c"] {
        assert!(secret(&f, &format!("email/{name}/api_key")).is_none());
    }
    assert!(!f.dir.path().join(EMAIL_FILE).exists());
    assert_eq!(f.host.list().len(), 0);
    let reqs = server.received_requests().await.unwrap();
    assert!(
        reqs.iter().all(|r| r.method.as_str() == "GET"),
        "never a create"
    );
}

#[tokio::test]
async fn an_unusable_existing_inbox_id_is_refused_before_any_request() {
    let server = MockServer::start().await;
    let f = fixture(&server);
    for bad in ["a/b@agentmail.to", "x y@agentmail.to", "..", "a?b"] {
        let e = f.host.add(connect_input("x", bad, None)).await.unwrap_err();
        assert!(matches!(e, EmailControlError::Invalid(_)), "{bad}: {e:?}");
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

/// Deleting a bot with "delete its inbox too": decided under the host's lock, account by account.
/// Only an inbox that is still the bot's alone is deleted at the provider; one another bot shares
/// (whatever the case of its id) passes to that bot, and one another bot lists as its default is
/// not deleted at the provider either.
#[tokio::test]
async fn a_deleted_bots_own_inbox_goes_at_the_provider_only_when_it_is_still_its_alone() {
    let server = MockServer::start().await;
    mock_create(&server, "helper@agentmail.to").await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(202))
        .mount(&server)
        .await;
    let f = fixture(&server);
    f.host.add(input("own", Some(BOT_A))).await.unwrap();
    let mut shared = input("shared", Some(BOT_A));
    shared.shared_with = vec![BOT_A.to_uppercase(), BOT_B.to_uppercase()];
    f.host.add(shared).await.unwrap();
    let mut listed = input("listed", Some(BOT_A));
    listed.default_for = vec![BOT_B.to_owned()];
    f.host.add(listed).await.unwrap();

    let out = f
        .host
        .forget_bot_mailboxes(&BOT_A.to_uppercase(), true)
        .await
        .unwrap();
    assert_eq!(
        deletes(&server).await,
        1,
        "only the inbox that was its alone"
    );
    assert_eq!(out.inboxes_deleted, ["helper@agentmail.to"]);
    let mut removed = out.removed.clone();
    removed.sort();
    assert_eq!(removed, ["listed", "own"]);
    assert_eq!(out.kept, ["shared"]);
    assert_eq!(out.inboxes_not_deleted.len(), 1);
    assert_eq!(out.inboxes_not_deleted[0].account, "listed");
    // The shared account passed to the other bot, with no copy of either id left in its list.
    let left = f.host.list();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].name, "shared");
    assert!(left[0].bot.as_deref().unwrap().eq_ignore_ascii_case(BOT_B));
    assert_eq!(left[0].shared_with, Vec::<String>::new());
    assert!(secret(&f, "email/shared/api_key").is_some());
    assert!(secret(&f, "email/own/api_key").is_none());
}

/// When the provider refuses, the bot's account stays with its key (bound to the gone bot, so no
/// bot can use it) and is reported; the next attempt finishes it.
#[tokio::test]
async fn a_refused_inbox_delete_keeps_the_account_for_a_retry() {
    let server = MockServer::start().await;
    mock_create(&server, "helper@agentmail.to").await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(202))
        .mount(&server)
        .await;
    let f = fixture(&server);
    f.host.add(input("own", Some(BOT_A))).await.unwrap();
    let out = f.host.forget_bot_mailboxes(BOT_A, true).await.unwrap();
    assert!(out.removed.is_empty() && out.inboxes_deleted.is_empty());
    assert_eq!(out.inboxes_not_deleted.len(), 1);
    assert_eq!(out.inboxes_not_deleted[0].address, "helper@agentmail.to");
    assert_eq!(f.host.list().len(), 1);
    assert!(secret(&f, "email/own/api_key").is_some());

    let out = f.host.forget_bot_mailboxes(BOT_A, true).await.unwrap();
    assert_eq!(out.removed, ["own"]);
    assert_eq!(out.inboxes_deleted, ["helper@agentmail.to"]);
    assert_eq!(f.host.list().len(), 0);
    assert!(secret(&f, "email/own/api_key").is_none());
}
