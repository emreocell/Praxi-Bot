//! `email_wait_for` and the mail side of `account_register`, over a loopback
//! `AgentMail` mock.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use connector_email::{
    AccountConfig, AccountKind, AgentMailEndpoint, EmailConnector, IdentitySettings, ImapSettings,
    MailHandle, PageOrigin, SmtpSettings, WaitPlan,
};
use secret_store::{FillSecrets, MemoryStore, SecretRef, SecretStore};
use serde_json::{Value, json};
use tool_registry::{ContentPart, Effect, PermissionCategory, Tool, ToolContext, ToolError};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::account_register::{AccountRegisterConfig, account_register_tool_with_mail};
use crate::mail_wait::EmailWaitFor;

const BOT_A: &str = "11111111-1111-4111-8111-111111111111";
const BOT_B: &str = "22222222-2222-4222-8222-222222222222";

fn account(name: &str, bot: Option<&str>, address: &str) -> AccountConfig {
    AccountConfig {
        name: name.into(),
        address: address.into(),
        username: String::new(),
        password: SecretRef::new(format!("email/{name}/api_key")).unwrap(),
        imap: ImapSettings::default(),
        smtp: SmtpSettings::default(),
        drafts_folder: "Drafts".into(),
        extra_root_pem: None,
        timeout_secs: 30,
        bot: bot.map(str::to_owned),
        allowed_senders: Vec::new(),
        identity: IdentitySettings::default(),
        kind: AccountKind::AgentMail,
        inbox_id: Some(address.into()),
    }
}

fn connector(server: &MockServer, accounts: Vec<AccountConfig>) -> Arc<EmailConnector> {
    let store = Arc::new(MemoryStore::new());
    for a in &accounts {
        store
            .set(a.password.name(), "am_tool_test_KEY_31ff")
            .unwrap();
    }
    let c = EmailConnector::reloadable_with(
        store,
        AgentMailEndpoint::loopback_mock_for_tests(&server.uri()).unwrap(),
    );
    c.replace_accounts(accounts).unwrap();
    Arc::new(c)
}

/// RFC 3339 UTC for `secs` since the epoch (civil-from-days, no date library).
fn rfc3339(secs: u64) -> String {
    let days = i64::try_from(secs / 86_400).unwrap();
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn recent() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    rfc3339(now - 20)
}

fn verification_mail(inbox: &str) -> Value {
    json!({
        "inbox_id": inbox, "thread_id": "t", "message_id": "msgone", "labels": ["unread"],
        "timestamp": recent(), "from": "Example <no-reply@mail.example.com>", "to": [inbox],
        "size": 10, "updated_at": recent(), "created_at": recent(),
        "subject": "Confirm your account",
        "text": "Your verification code is 482913.",
        "html": "<a href=\"https://app.example.com/verify?token=abc\">Confirm</a> <a href=\"https://tracker.evil.test/p\">x</a>",
        "attachments": []
    })
}

async fn serve_mail(server: &MockServer, inbox: &str, messages: Vec<Value>) {
    // Request paths are strictly percent-encoded (`@` is `%40`).
    let inbox = inbox.replace('@', "%40");
    Mock::given(method("GET"))
        .and(path(format!("/v0/inboxes/{inbox}/messages")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"count": messages.len(), "messages": messages})),
        )
        .mount(server)
        .await;
    for m in &messages {
        let id = m["message_id"].as_str().unwrap();
        Mock::given(method("GET"))
            .and(path(format!("/v0/inboxes/{inbox}/messages/{id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(m.clone()))
            .mount(server)
            .await;
    }
}

struct Page(Option<&'static str>);

#[async_trait]
impl PageOrigin for Page {
    async fn top_origin(&self, _: &ToolContext) -> Option<String> {
        self.0.map(str::to_owned)
    }
}

fn quick() -> WaitPlan {
    WaitPlan {
        timeout: Duration::from_millis(150),
        max_age: Duration::from_secs(900),
        since: None,
        first_pause: Duration::from_millis(20),
        max_pause: Duration::from_millis(40),
    }
}

fn ctx(bot: &str, run: &str) -> ToolContext {
    ToolContext {
        bot_id: Some(bot.to_owned()),
        run_id: run.to_owned(),
        ..ToolContext::default()
    }
}

fn data(out: &tool_registry::ToolOutput) -> &Value {
    let ContentPart::Json { data } = &out.content[0] else {
        panic!("expected JSON")
    };
    data
}

fn tool(mail: &Arc<MailHandle>, page: Option<&'static str>) -> EmailWaitFor {
    EmailWaitFor::new(Arc::clone(mail), Arc::new(Page(page)), Some(quick()))
}

async fn rig(accounts: Vec<AccountConfig>, inbox: &str, mails: Vec<Value>) -> Arc<MailHandle> {
    let server = Box::leak(Box::new(MockServer::start().await));
    serve_mail(server, inbox, mails).await;
    let mail = Arc::new(MailHandle::new());
    mail.set(connector(server, accounts));
    mail
}

#[test]
fn the_tool_is_a_read_of_the_messaging_family_and_asks_like_email_read() {
    let t = EmailWaitFor::new(Arc::new(MailHandle::new()), Arc::new(Page(None)), None);
    let s = t.spec();
    assert_eq!(s.effect, Effect::Read);
    assert_eq!(s.category, PermissionCategory::Messaging);
    // Like email_read it is untrusted and reaches a remote mailbox, so a mail- or
    // webhook-started (tainted) run is still asked. Unlike email_read it is not a private
    // read: it refuses every mailbox but the bot's own identity and returns no mail body.
    assert!(s.content.ingests_untrusted && s.content.egress);
    assert!(!s.content.reads_private);
    assert!(!t.visible_to(Some("bot")), "no connector, no tool");
}

#[test]
fn plan_is_capped_at_ten_minutes() {
    let p = WaitPlan::minutes(99, Some(999));
    assert_eq!(p.timeout, Duration::from_secs(600));
    assert_eq!(p.max_age, Duration::from_secs(3600));
}

#[test]
fn only_a_plan_without_a_named_age_carries_the_start_watermark() {
    let now = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap();
    let default = WaitPlan::minutes(5, None);
    let since = default.since.expect("a watermark");
    assert!(
        (now - since - connector_email::wait::START_GRACE_SECS).abs() <= 2,
        "{since} vs {now}"
    );
    // Naming an age asks for older mail on purpose.
    assert_eq!(WaitPlan::minutes(5, Some(30)).since, None);
}

#[tokio::test]
async fn a_bot_waits_on_its_own_inbox_and_gets_a_short_untrusted_answer() {
    let mail = rig(
        vec![account("a-mail", Some(BOT_A), "a@agentmail.to")],
        "a@agentmail.to",
        vec![verification_mail("a@agentmail.to")],
    )
    .await;
    let t = tool(&mail, Some("https://app.example.com:443"));
    assert!(t.visible_to(Some(BOT_A)));
    assert!(
        !t.visible_to(Some(BOT_B)),
        "a bot without an identity does not see it"
    );
    let card = t.egress_extra(&ctx(BOT_A, "r1"), &json!({})).unwrap();
    assert!(card.contains("a@agentmail.to"), "{card}");
    // The card says what really happens: junk is read and a tracking hop is a request out.
    assert!(card.contains("junk folder"), "{card}");
    assert!(
        card.contains("click-tracking") && card.contains("GET"),
        "{card}"
    );
    assert!(!card.contains("nothing is sent"), "{card}");
    let out = t
        .call(
            &ctx(BOT_A, "r-found"),
            json!({"site":"https://example.com","from_domain":"example.com"}),
        )
        .await
        .unwrap();
    let d = data(&out);
    assert_eq!(d["found"], true);
    assert_eq!(d["untrusted_data"], true);
    assert_eq!(d["codes"], json!(["482913"]));
    assert_eq!(
        d["followable_links"],
        json!(["https://app.example.com/verify?token=abc"])
    );
    assert!(d.to_string().contains("external link, not followed"));
}

#[tokio::test]
async fn the_site_is_checked_against_the_live_page_not_the_models_word() {
    let mail = rig(
        vec![account("a-mail", Some(BOT_A), "a@agentmail.to")],
        "a@agentmail.to",
        vec![verification_mail("a@agentmail.to")],
    )
    .await;
    // The model says example.com; the browser is really somewhere else.
    let out = tool(&mail, Some("https://evil.test:443"))
        .call(&ctx(BOT_A, "r-lie"), json!({"site":"https://example.com"}))
        .await
        .unwrap();
    assert_eq!(data(&out)["followable_links"], json!([]));
    // No browser page at all.
    let out = tool(&mail, None)
        .call(
            &ctx(BOT_A, "r-nopage"),
            json!({"site":"https://example.com"}),
        )
        .await
        .unwrap();
    assert_eq!(data(&out)["followable_links"], json!([]));
}

#[tokio::test]
async fn other_bots_and_shared_accounts_are_refused() {
    let mail = rig(
        vec![
            account("a-mail", Some(BOT_A), "a@agentmail.to"),
            account("pool", None, "pool@agentmail.to"),
        ],
        "a@agentmail.to",
        vec![verification_mail("a@agentmail.to")],
    )
    .await;
    let t = tool(&mail, None);
    // Bot B names A's account: refused like a missing one.
    let e = t
        .call(&ctx(BOT_B, "r-b"), json!({"account":"a-mail"}))
        .await
        .unwrap_err();
    assert!(matches!(e, ToolError::InvalidInput(m) if m.contains("unknown account")));
    // The account every bot may use is not anybody's OWN identity.
    let e = t
        .call(&ctx(BOT_A, "r-pool"), json!({"account":"pool"}))
        .await
        .unwrap_err();
    assert!(matches!(e, ToolError::InvalidInput(m) if m.contains("own mail identity")));
}

#[tokio::test]
async fn times_out_with_a_short_no_mail_answer_and_stop_cancels() {
    let mail = rig(
        vec![account("a-mail", Some(BOT_A), "a@agentmail.to")],
        "a@agentmail.to",
        vec![],
    )
    .await;
    let out = tool(&mail, None)
        .call(&ctx(BOT_A, "r-timeout"), json!({}))
        .await
        .unwrap();
    assert_eq!(data(&out)["found"], false);
    assert!(
        data(&out)["note"]
            .as_str()
            .unwrap()
            .contains("Do not guess")
    );

    // A long wait ends at once when the run is stopped.
    let long = EmailWaitFor::new(
        Arc::clone(&mail),
        Arc::new(Page(None)),
        Some(WaitPlan {
            timeout: Duration::from_secs(600),
            ..quick()
        }),
    );
    let started = std::time::Instant::now();
    let h = tokio::spawn(async move { long.call(&ctx(BOT_A, "r-stop"), json!({})).await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    crate::wait::cancel_run_waits("r-stop");
    let e = h.await.unwrap().unwrap_err();
    assert!(e.to_string().contains("stopped"), "{e}");
    assert!(started.elapsed() < Duration::from_secs(5));
}

fn register_rig(mail: &Arc<MailHandle>) -> (Arc<dyn Tool>, Arc<FillSecrets>) {
    let secrets = Arc::new(FillSecrets::in_memory(Arc::new(MemoryStore::new())));
    let tool = account_register_tool_with_mail(
        Arc::clone(&secrets),
        AccountRegisterConfig::default(),
        Arc::clone(mail),
    );
    (tool, secrets)
}

#[tokio::test]
async fn a_sign_up_uses_the_bots_identity_and_the_vault_records_which_one() {
    let mail = rig(
        vec![
            account("a-mail", Some(BOT_A), "a@agentmail.to"),
            account("pool", None, "pool@agentmail.to"),
        ],
        "a@agentmail.to",
        vec![],
    )
    .await;
    let (tool, secrets) = register_rig(&mail);
    let c = ctx(BOT_A, "r-reg");
    // No login, no mailbox named: the bot's own identity is used.
    let input = json!({"site":"https://example.com"});
    let card = tool.approval_details(&c, &input).await.unwrap().unwrap();
    assert!(
        card["login_id"]
            .as_str()
            .unwrap()
            .contains("a@agentmail.to"),
        "{card}"
    );
    assert!(
        card["email_account"].as_str().unwrap().contains("a-mail"),
        "{card}"
    );
    let out = tool.call(&c, input).await.unwrap();
    let text = format!("{out:?}");
    assert!(text.contains("a@agentmail.to"), "{text}");
    assert!(text.contains("email_wait_for"), "{text}");
    let accounts = secrets.accounts(Some(BOT_A)).unwrap();
    assert_eq!(accounts[0].login_id, "a@agentmail.to");
    assert_eq!(accounts[0].email_account.as_deref(), Some("a-mail"));

    // A mailbox that is not one of the bot's own identities is refused: the shared
    // owner mailbox, another bot's, and a made-up name.
    let tool = register_rig(&mail).0;
    for named in ["pool", "nothing-like-it"] {
        let e = tool
            .call(
                &c,
                json!({"site":"https://example.org","email_account":named}),
            )
            .await
            .unwrap_err();
        assert!(matches!(e, ToolError::InvalidInput(_)), "{named}: {e:?}");
    }
    // Without any identity and without a login there is nothing to sign up with.
    let e = tool
        .call(&ctx(BOT_B, "r-b"), json!({"site":"https://example.net"}))
        .await
        .unwrap_err();
    assert!(matches!(e, ToolError::InvalidInput(m) if m.contains("login_id")));
}

#[tokio::test]
async fn the_probe_reports_the_page_the_browser_is_on_not_what_anyone_claims() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Arc::new(crate::browser::fake::FakeBrowser::default());
    let slot = Arc::new(connector_email::PageOriginSlot::new());
    // Before the browser tools exist the slot knows nothing.
    assert_eq!(slot.top_origin(&ctx(BOT_A, "r-probe")).await, None);
    let _tools = crate::browser::tools_with_origin(
        fake.clone(),
        dir.path(),
        None,
        None,
        Some(Arc::clone(&slot)),
    );
    // No page yet.
    assert_eq!(slot.top_origin(&ctx(BOT_A, "r-probe")).await, None);
    *fake.url.lock() = Some("https://Accounts.Example.com/signup?a=1#x".to_owned());
    assert_eq!(
        slot.top_origin(&ctx(BOT_A, "r-probe")).await.as_deref(),
        Some("https://accounts.example.com:443")
    );
    // A non-web page has no origin.
    *fake.url.lock() = Some("about:blank".to_owned());
    assert_eq!(slot.top_origin(&ctx(BOT_A, "r-probe")).await, None);
}

fn patient(mail: &Arc<MailHandle>) -> EmailWaitFor {
    EmailWaitFor::new(
        Arc::clone(mail),
        Arc::new(Page(None)),
        Some(WaitPlan {
            timeout: Duration::from_secs(600),
            ..quick()
        }),
    )
}

/// The id of a pending wait is useful to the run (and account) that started it, to nobody else;
/// and the wait ends with its run.
#[tokio::test]
async fn a_pending_wait_id_works_only_for_its_own_run_and_account() {
    let mail = rig(
        vec![
            account("a-mail", Some(BOT_A), "a@agentmail.to"),
            account("a-mail-2", Some(BOT_A), "a2@agentmail.to"),
        ],
        "a@agentmail.to",
        vec![],
    )
    .await;
    let t = patient(&mail);
    let start = |run: &'static str| {
        let t = &t;
        async move {
            let out = t
                .call(
                    &ctx(BOT_A, run),
                    json!({"account": "a-mail", "block_seconds": 1}),
                )
                .await
                .unwrap();
            let d = data(&out);
            assert_eq!(d["pending"], true, "{d}");
            d["wait_id"].as_str().unwrap().to_owned()
        }
    };
    let id_a = start("run-scope-a").await;
    // The identical ask in another run (same bot) is a wait of its own.
    let id_b = start("run-scope-b").await;
    assert_ne!(id_a, id_b);

    let collect = |run: &'static str, input: Value| {
        let t = &t;
        async move { t.call(&ctx(BOT_A, run), input).await }
    };
    // Another run cannot collect it...
    let e = collect(
        "run-scope-b",
        json!({"account": "a-mail", "wait_id": id_a, "block_seconds": 1}),
    )
    .await
    .unwrap_err();
    assert!(e.to_string().contains("no such wait"), "{e}");
    // ...nor can the same run under another of its accounts.
    let e = collect(
        "run-scope-a",
        json!({"account": "a-mail-2", "wait_id": id_a, "block_seconds": 1}),
    )
    .await
    .unwrap_err();
    assert!(e.to_string().contains("no such wait"), "{e}");
    // The run that started it still can.
    let out = collect(
        "run-scope-a",
        json!({"account": "a-mail", "wait_id": id_a, "block_seconds": 1}),
    )
    .await
    .unwrap();
    assert_eq!(data(&out)["pending"], true);

    // Stopping run b leaves run a's wait alone.
    crate::wait::cancel_run_waits("run-scope-b");
    let out = collect(
        "run-scope-a",
        json!({"account": "a-mail", "wait_id": id_a, "block_seconds": 1}),
    )
    .await
    .unwrap();
    assert_eq!(data(&out)["pending"], true);
    // A run that is over stops polling, but its wait is kept: the bot's next run takes it
    // over by its id (for the same account only), and an owner Stop forgets it for good.
    crate::wait::end_run_mail_waits("run-scope-a");
    let e = collect(
        "run-scope-c",
        json!({"account": "a-mail-2", "wait_id": id_a, "block_seconds": 1}),
    )
    .await
    .unwrap_err();
    assert!(e.to_string().contains("no such wait"), "{e}");
    let out = collect(
        "run-scope-c",
        json!({"account": "a-mail", "wait_id": id_a, "block_seconds": 1}),
    )
    .await
    .unwrap();
    assert_eq!(data(&out)["pending"], true);
    assert_eq!(data(&out)["wait_id"], id_a.as_str());
    crate::wait::cancel_run_waits("run-scope-c");
    let e = collect(
        "run-scope-d",
        json!({"account": "a-mail", "wait_id": id_a, "block_seconds": 1}),
    )
    .await
    .unwrap_err();
    assert!(e.to_string().contains("no such wait"), "{e}");
}

/// The browser moves on while the wait goes on.
struct MovingPage(parking_lot::Mutex<Option<String>>);

impl MovingPage {
    fn on(origin: &str) -> Arc<Self> {
        Arc::new(Self(parking_lot::Mutex::new(Some(origin.to_owned()))))
    }
    fn go(&self, origin: &str) {
        *self.0.lock() = Some(origin.to_owned());
    }
}

#[async_trait]
impl PageOrigin for MovingPage {
    async fn top_origin(&self, _: &ToolContext) -> Option<String> {
        self.0.lock().clone()
    }
}

/// An inbox the test fills while a wait is polling it.
struct LiveInbox(Arc<parking_lot::Mutex<Vec<Value>>>);

impl wiremock::Respond for LiveInbox {
    fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
        let messages = self.0.lock().clone();
        ResponseTemplate::new(200)
            .set_body_json(json!({"count": messages.len(), "messages": messages}))
    }
}

async fn live_inbox(inbox: &str) -> (Arc<MailHandle>, Arc<parking_lot::Mutex<Vec<Value>>>) {
    let server = Box::leak(Box::new(MockServer::start().await));
    let held = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let enc = inbox.replace('@', "%40");
    Mock::given(method("GET"))
        .and(path(format!("/v0/inboxes/{enc}/messages")))
        .respond_with(LiveInbox(Arc::clone(&held)))
        .mount(server)
        .await;
    let mail_json = verification_mail(inbox);
    Mock::given(method("GET"))
        .and(path(format!("/v0/inboxes/{enc}/messages/msgone")))
        .respond_with(ResponseTemplate::new(200).set_body_json(mail_json))
        .mount(server)
        .await;
    let mail = Arc::new(MailHandle::new());
    mail.set(connector(
        server,
        vec![account("a-mail", Some(BOT_A), inbox)],
    ));
    (mail, held)
}

/// The model's word about the site must match the page the wait STARTED on, not the one the
/// browser shows by the time the mail comes (the positive case is the next test).
#[tokio::test]
async fn a_claimed_site_that_is_not_the_start_page_verifies_nothing() {
    let (mail, inbox) = live_inbox("a@agentmail.to").await;
    let page = MovingPage::on("https://app.example.com:443");
    let t = EmailWaitFor::new(
        Arc::clone(&mail),
        Arc::clone(&page) as Arc<dyn PageOrigin>,
        Some(WaitPlan {
            timeout: Duration::from_secs(600),
            ..quick()
        }),
    );
    let site_a = json!({"site": "https://example.com", "block_seconds": 1});
    let out = t.call(&ctx(BOT_A, "r-site-a"), site_a).await.unwrap();
    let wait_id = data(&out)["wait_id"].as_str().unwrap().to_owned();
    // The bot goes on to site B; site A's mail arrives meanwhile.
    page.go("https://other-site.test:443");
    inbox.lock().push(verification_mail("a@agentmail.to"));
    // Claiming another site than the one the wait started on verifies nothing.
    let wrong = t
        .call(
            &ctx(BOT_A, "r-site-a"),
            json!({"site": "https://other-site.test", "wait_id": wait_id, "block_seconds": 30}),
        )
        .await
        .unwrap();
    assert_eq!(data(&wrong)["found"], true);
    assert_eq!(data(&wrong)["followable_links"], json!([]));
}

/// The walkthrough's bug: the tool says "do other useful work", the bot opens site B, and site
/// A's mail then found `followable_links: []` because the page was read when the mail came.
/// Collected by id after the bot left, the links still belong to the site the form was
/// submitted on.
#[tokio::test]
async fn collecting_by_id_after_moving_on_keeps_the_start_origin() {
    let (mail, inbox) = live_inbox("a@agentmail.to").await;
    let page = MovingPage::on("https://app.example.com:443");
    let t = EmailWaitFor::new(
        Arc::clone(&mail),
        Arc::clone(&page) as Arc<dyn PageOrigin>,
        Some(WaitPlan {
            timeout: Duration::from_secs(600),
            ..quick()
        }),
    );
    let out = t
        .call(
            &ctx(BOT_A, "r-moves-on"),
            json!({"site": "https://example.com", "block_seconds": 1}),
        )
        .await
        .unwrap();
    assert_eq!(data(&out)["pending"], true);
    let wait_id = data(&out)["wait_id"].as_str().unwrap().to_owned();
    page.go("https://other-site.test:443");
    inbox.lock().push(verification_mail("a@agentmail.to"));
    let out = t
        .call(
            &ctx(BOT_A, "r-moves-on"),
            json!({"site": "https://example.com", "wait_id": wait_id, "block_seconds": 30}),
        )
        .await
        .unwrap();
    assert_eq!(data(&out)["found"], true, "{}", data(&out));
    assert_eq!(
        data(&out)["followable_links"],
        json!(["https://app.example.com/verify?token=abc"])
    );
}

/// The same, across a session boundary: the first run ends with the wait still pending, the next
/// run (a new run id) collects it by id and gets the start origin and the mail.
#[tokio::test]
async fn a_wait_collected_by_the_next_run_still_knows_where_it_started() {
    let (mail, inbox) = live_inbox("a@agentmail.to").await;
    let page = MovingPage::on("https://app.example.com:443");
    let t = EmailWaitFor::new(
        Arc::clone(&mail),
        Arc::clone(&page) as Arc<dyn PageOrigin>,
        Some(WaitPlan {
            timeout: Duration::from_secs(600),
            ..quick()
        }),
    );
    let out = t
        .call(
            &ctx(BOT_A, "session-1"),
            json!({"site": "https://example.com", "block_seconds": 1}),
        )
        .await
        .unwrap();
    let wait_id = data(&out)["wait_id"].as_str().unwrap().to_owned();
    // Session one hits its step limit; the browser tab is gone in the next one.
    crate::wait::end_run_mail_waits("session-1");
    *page.0.lock() = None;
    inbox.lock().push(verification_mail("a@agentmail.to"));
    let out = t
        .call(
            &ctx(BOT_A, "session-2"),
            json!({"site": "https://example.com", "wait_id": wait_id, "block_seconds": 30}),
        )
        .await
        .unwrap();
    assert_eq!(data(&out)["found"], true, "{}", data(&out));
    assert_eq!(
        data(&out)["followable_links"],
        json!(["https://app.example.com/verify?token=abc"]),
        "judged against the page session one started on"
    );
}

/// A hop that never answers must not hold the call when the run is stopped.
#[tokio::test]
async fn stop_ends_a_call_that_is_looking_through_a_tracking_link() {
    struct Silent;
    #[async_trait]
    impl connector_email::TrackingResolver for Silent {
        async fn hop(&self, _: &str) -> Option<String> {
            std::future::pending().await
        }
    }
    let mut mail_with_tracker = verification_mail("a@agentmail.to");
    mail_with_tracker["html"] =
        json!("<a href=\"https://u1.ct.sendgrid.net/ls/click?upn=a\">Confirm</a>");
    let mail = rig(
        vec![account("a-mail", Some(BOT_A), "a@agentmail.to")],
        "a@agentmail.to",
        vec![mail_with_tracker],
    )
    .await;
    let t = EmailWaitFor::new(
        Arc::clone(&mail),
        Arc::new(Page(Some("https://example.com:443"))),
        Some(quick()),
    )
    .with_resolver(Some(Arc::new(Silent)));
    let started = std::time::Instant::now();
    let h = tokio::spawn(async move {
        t.call(
            &ctx(BOT_A, "r-hop-stop"),
            json!({"site":"https://example.com"}),
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    crate::wait::cancel_run_waits("r-hop-stop");
    let e = tokio::time::timeout(Duration::from_secs(10), h)
        .await
        .expect("a stopped run must not wait for a hop")
        .unwrap()
        .unwrap_err();
    assert!(e.to_string().contains("stopped"), "{e}");
    assert!(started.elapsed() < Duration::from_secs(10));
}
