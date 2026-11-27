//! Mail completeness for verification and assistant work, over a fake IMAP/SMTP server and a
//! fake `AgentMail`: spam is searched and said so, click-tracking links of the big senders are
//! looked through, a reply to the owner's own address in its own thread needs no card (and
//! nothing else does), and attachments land in the bot's workspace under a safe name.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use secret_store::{MemoryStore, SecretRef, SecretStore};
use serde_json::{Value, json};
use tool_registry::{ContentPart, Tool, ToolContext, ToolError, ToolOutput};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::agentmail::AgentMailEndpoint;
use crate::attachment::AttachmentPlace;
use crate::fake_imap::{FakeImap, attachment_mail, date_ago, html_mail, text_mail};
use crate::testutil::{Protocol, server};
use crate::wait::{
    TrackingResolver, WaitEnd, WaitFilter, WaitPlan, expand_tracking_links, found_json, link_basis,
    wait_for_mail,
};
use crate::{AccountConfig, AccountKind, EmailConnector, tools};

const BOT: &str = "11111111-1111-4111-8111-111111111111";
const OTHER_BOT: &str = "22222222-2222-4222-8222-222222222222";

fn ctx() -> ToolContext {
    ToolContext {
        bot_id: Some(BOT.to_owned()),
        run_id: "run-1".into(),
        ..ToolContext::default()
    }
}

fn tool(conn: &Arc<EmailConnector>, name: &str) -> Arc<dyn Tool> {
    tools(conn)
        .into_iter()
        .find(|t| t.spec().name == name)
        .unwrap()
}

fn json_of(out: &ToolOutput) -> Value {
    let ContentPart::Json { data } = &out.content[0] else {
        panic!("expected JSON");
    };
    data.clone()
}

async fn call(conn: &Arc<EmailConnector>, name: &str, input: Value) -> Result<Value, ToolError> {
    tool(conn, name)
        .call(&ctx(), input)
        .await
        .map(|o| json_of(&o))
}

/// The call the way the loop makes it when the owner approves: the card is built first.
async fn approve_then_call(
    conn: &Arc<EmailConnector>,
    name: &str,
    input: Value,
) -> Result<Value, ToolError> {
    let t = tool(conn, name);
    t.approval_details(&ctx(), &input).await?;
    t.call(&ctx(), input).await.map(|o| json_of(&o))
}

fn subjects(v: &Value) -> Vec<String> {
    v["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["subject"].as_str().unwrap().to_owned())
        .collect()
}

// ---- Spam is searched, and said ---------------------------------------------------------------

async fn junk_rig() -> (FakeImap, Arc<EmailConnector>) {
    let imap = FakeImap::start(&[
        ("INBOX", None),
        ("Spam", Some("\\Junk")),
        ("Drafts", Some("\\Drafts")),
    ])
    .await;
    let conn = imap.connector(imap.account("own", Some(BOT), 1, ""));
    (imap, conn)
}

#[tokio::test]
async fn a_search_that_asks_for_junk_also_looks_in_the_junk_folder_and_says_where_a_match_was() {
    let (imap, conn) = junk_rig().await;
    imap.deliver(
        "INBOX",
        text_mail(
            "Shop <hello@shop.test>",
            "Your order",
            "i1@shop.test",
            &date_ago(900),
            "Order 5 shipped.",
        ),
    );
    imap.deliver(
        "Spam",
        text_mail(
            "Site <no-reply@site.test>",
            "Confirm your account",
            "s1@site.test",
            &date_ago(60),
            "Your code is 123456.",
        ),
    );

    // Not asked: the inbox only, as before.
    let plain = call(&conn, "email_search", json!({})).await.unwrap();
    assert_eq!(subjects(&plain), ["Your order"], "{plain}");
    assert!(plain.get("junk_note").is_none() && plain.get("folders_searched").is_none());

    let all = call(&conn, "email_search", json!({"include_junk": true}))
        .await
        .unwrap();
    // Newest first across both folders.
    assert_eq!(
        subjects(&all),
        ["Confirm your account", "Your order"],
        "{all}"
    );
    let spam = &all["messages"][0];
    assert_eq!(spam["folder"], "Spam");
    assert_eq!(spam["from_junk"], true);
    assert!(spam["uid"].as_u64().is_some());
    let inbox = &all["messages"][1];
    assert_eq!(inbox["folder"], "INBOX");
    assert_eq!(inbox["from_junk"], false);
    assert_eq!(all["from_junk"], 1);
    assert!(all["junk_note"].as_str().unwrap().contains("spam"), "{all}");
    assert_eq!(all["folders_searched"], json!(["INBOX", "Spam"]));
    assert_eq!(all["total_matches"], 2);

    // The mail is read from the folder the search named.
    let read = call(
        &conn,
        "email_read",
        json!({"folder": spam["folder"], "uid": spam["uid"]}),
    )
    .await
    .unwrap();
    assert!(
        read["message"]["body_text"]
            .as_str()
            .unwrap()
            .contains("123456"),
        "{read}"
    );
}

#[tokio::test]
async fn body_text_search_reaches_the_junk_folder_and_the_limit_applies_to_the_merge() {
    let (imap, conn) = junk_rig().await;
    for n in 0..3 {
        imap.deliver(
            "INBOX",
            text_mail(
                "A <a@one.test>",
                &format!("inbox {n}"),
                &format!("i{n}@one.test"),
                &date_ago(1000 - n * 10),
                "magic-word is here",
            ),
        );
    }
    imap.deliver(
        "Spam",
        text_mail(
            "B <b@two.test>",
            "spam hit",
            "s@two.test",
            &date_ago(5),
            "the magic-word again",
        ),
    );
    imap.deliver(
        "Spam",
        text_mail(
            "B <b@two.test>",
            "spam miss",
            "s2@two.test",
            &date_ago(4),
            "nothing to see",
        ),
    );
    let found = call(
        &conn,
        "email_search",
        json!({"text": "magic-word", "include_junk": true, "limit": 2}),
    )
    .await
    .unwrap();
    assert_eq!(subjects(&found), ["spam hit", "inbox 2"], "{found}");
    assert_eq!(found["total_matches"], 4);
    assert_eq!(found["returned"], 2);
}

#[tokio::test]
async fn a_junk_folder_that_cannot_be_searched_does_not_fail_the_search() {
    // No junk folder at all: include_junk finds nothing extra and the inbox still answers.
    let imap = FakeImap::start(&[("INBOX", None), ("Drafts", Some("\\Drafts"))]).await;
    let conn = imap.connector(imap.account("own", Some(BOT), 1, ""));
    imap.deliver(
        "INBOX",
        text_mail("A <a@one.test>", "hi", "i@one.test", &date_ago(10), "x"),
    );
    let out = call(&conn, "email_search", json!({"include_junk": true}))
        .await
        .unwrap();
    assert_eq!(subjects(&out), ["hi"]);
    assert_eq!(out["folders_searched"], json!(["INBOX"]));
    assert!(out.get("junk_note").is_none());
}

#[tokio::test]
async fn a_wait_that_finds_the_mail_in_spam_says_so_in_words() {
    let (imap, conn) = junk_rig().await;
    imap.deliver(
        "Spam",
        html_mail(
            "Site <no-reply@site.test>",
            "Verify your address",
            "w1@site.test",
            &date_ago(20),
            "<p>Code 482913</p><a href=\"https://www.site.test/verify?t=1\">Verify</a>",
        ),
    );
    let filter = WaitFilter::new(Some("site.test"), None, None).unwrap();
    let plan = WaitPlan {
        timeout: Duration::from_secs(4),
        max_age: Duration::from_secs(900),
        since: None,
        first_pause: Duration::from_millis(40),
        max_pause: Duration::from_millis(80),
    };
    let end = wait_for_mail(
        &conn.scoped(Some(BOT)),
        Some("own"),
        &filter,
        &plan,
        std::future::pending(),
    )
    .await
    .unwrap();
    let WaitEnd::Found(detail) = end else {
        panic!("not found");
    };
    let basis = link_basis(Some("https://site.test"), Some("https://site.test:443"));
    let v = found_json(&detail, "own", &basis, &[]);
    assert_eq!(v["from_junk"], true);
    assert_eq!(v["folder"], "Spam");
    assert!(
        v["junk_note"].as_str().unwrap().contains("spam folder"),
        "{v}"
    );
}

// ---- AgentMail: spam by label -------------------------------------------------------------------

const KEY: &str = "am_completeness_KEY_77aa";
const INBOX: &str = "bot@agentmail.to";
const INBOX_ENC: &str = "bot%40agentmail.to";

fn am_account(name: &str, bot: Option<&str>) -> AccountConfig {
    AccountConfig {
        name: name.into(),
        address: INBOX.into(),
        username: String::new(),
        password: SecretRef::new(format!("email/{name}/api_key")).unwrap(),
        imap: crate::ImapSettings::default(),
        smtp: crate::SmtpSettings::default(),
        drafts_folder: "Drafts".into(),
        extra_root_pem: None,
        timeout_secs: 30,
        bot: bot.map(str::to_owned),
        allowed_senders: Vec::new(),
        identity: crate::IdentitySettings::default(),
        kind: AccountKind::AgentMail,
        inbox_id: Some(INBOX.into()),
    }
}

fn am_connector(server: &MockServer, accounts: Vec<AccountConfig>) -> Arc<EmailConnector> {
    let store = Arc::new(MemoryStore::new());
    for a in &accounts {
        store.set(a.password.name(), KEY).unwrap();
    }
    let c = EmailConnector::reloadable_with(
        store,
        AgentMailEndpoint::loopback_mock_for_tests(&server.uri()).unwrap(),
    );
    c.replace_accounts(accounts).unwrap();
    Arc::new(c)
}

fn am_msg(id: &str, subject: &str, labels: &[&str]) -> Value {
    json!({
        "inbox_id": INBOX, "thread_id": "t", "message_id": id, "labels": labels,
        "timestamp": "2026-11-12T09:00:00Z", "from": "no-reply@site.test", "to": [INBOX],
        "size": 10, "updated_at": "2026-11-12T09:00:00Z", "created_at": "2026-11-12T09:00:00Z",
        "subject": subject, "text": "Your code is 482913.",
    })
}

#[tokio::test]
async fn an_agentmail_search_for_spam_marks_each_match_by_the_providers_label() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/v0/inboxes/{INBOX_ENC}/messages")))
        .and(query_param("include_spam", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 2,
            "messages": [
                am_msg("m-spam", "Confirm your account", &["received", "spam"]),
                am_msg("m-ok", "Your receipt", &["received", "unread"]),
            ]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let conn = am_connector(&server, vec![am_account("bot-mail", Some(BOT))]);
    let out = call(&conn, "email_search", json!({"include_junk": true}))
        .await
        .unwrap();
    let msgs = out["messages"].as_array().unwrap();
    assert_eq!(msgs[0]["from_junk"], true, "{out}");
    assert_eq!(msgs[1]["from_junk"], false, "{out}");
    assert_eq!(out["from_junk"], 1);
    assert!(out["junk_note"].as_str().is_some());
    // A mailbox without folders has no folder list to show.
    assert!(out.get("folders_searched").is_none());
}

#[tokio::test]
async fn an_agentmail_wait_that_finds_spam_says_so() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/v0/inboxes/{INBOX_ENC}/messages")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1,
            "messages": [am_msg("m-spam", "Verify your address", &["received", "spam"])]
        })))
        .mount(&server)
        .await;
    let mut detail = am_msg("m-spam", "Verify your address", &["received", "spam"]);
    detail["timestamp"] = json!(chrono_free_now());
    Mock::given(method("GET"))
        .and(path(format!("/v0/inboxes/{INBOX_ENC}/messages/m-spam")))
        .respond_with(ResponseTemplate::new(200).set_body_json(detail))
        .mount(&server)
        .await;
    let conn = am_connector(&server, vec![am_account("bot-mail", Some(BOT))]);
    let filter = WaitFilter::new(Some("site.test"), None, None).unwrap();
    let plan = WaitPlan {
        timeout: Duration::from_secs(4),
        max_age: Duration::from_secs(86_400 * 365 * 5),
        since: None,
        first_pause: Duration::from_millis(40),
        max_pause: Duration::from_millis(80),
    };
    let end = wait_for_mail(
        &conn.scoped(Some(BOT)),
        Some("bot-mail"),
        &filter,
        &plan,
        std::future::pending(),
    )
    .await
    .unwrap();
    let WaitEnd::Found(d) = end else {
        panic!("not found");
    };
    assert_eq!(d.from_junk, Some(true));
    let v = found_json(&d, "bot-mail", &link_basis(None, None), &[]);
    assert_eq!(v["from_junk"], true, "{v}");
    assert!(v["junk_note"].as_str().is_some());
}

/// An RFC 3339 time a few seconds ago (the wait ignores mail older than its window).
fn chrono_free_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        - 5;
    mail_parser::DateTime::from_timestamp(i64::try_from(secs).unwrap()).to_rfc3339()
}

#[tokio::test]
async fn an_agentmail_body_search_says_it_is_not_supported_and_what_to_do() {
    let server = MockServer::start().await;
    let conn = am_connector(&server, vec![am_account("bot-mail", Some(BOT))]);
    let e = call(&conn, "email_search", json!({"text": "invoice"}))
        .await
        .unwrap_err();
    assert!(e.to_string().contains("bodies"), "{e}");
    assert!(server.received_requests().await.unwrap().is_empty());
}

// ---- Click-tracking links of the big senders ---------------------------------------------------------------

/// Answers the way each sender's redirect does: the target is the real site.
struct Esp;

#[async_trait]
impl TrackingResolver for Esp {
    async fn hop(&self, url: &str) -> Option<String> {
        Some(format!("https://www.site.test/verify?from={}", url.len()))
    }
}

#[tokio::test]
async fn the_click_wrapped_links_of_the_five_big_senders_are_judged_by_their_real_target() {
    let wrapped = [
        ("SendGrid", "https://u1234.ct.sendgrid.net/ls/click?upn=abc"),
        ("Mailgun", "https://email.mailgun.net/c/eyJhIjoxfQ"),
        (
            "Mailchimp",
            "https://site.us1.list-manage.com/track/click?u=1&id=2&e=3",
        ),
        (
            "Mandrill",
            "https://mandrillapp.com/track/click/30/site.test?p=x",
        ),
        (
            "Postmark",
            "https://click.pstmrk.it/3s/site.test%2Fverify/aBcD/xyz/AAAAAA/xyz",
        ),
        (
            "AWS SES",
            "https://abc123.r.us-east-1.awstrack.me/L0/https:%2F%2Fsite.test%2Fverify/1/0100/x",
        ),
    ];
    let (imap, conn) = junk_rig().await;
    for (n, (name, url)) in wrapped.iter().enumerate() {
        let uid = imap.deliver(
            "INBOX",
            html_mail(
                "Site <no-reply@site.test>",
                &format!("Confirm via {name}"),
                &format!("e{n}@site.test"),
                &date_ago(10),
                &format!("<a href=\"{url}\">Confirm</a>"),
            ),
        );
        let mut detail = conn
            .scoped(Some(BOT))
            .fetch_id(Some("own"), "INBOX", &uid.to_string(), 20_000)
            .await
            .unwrap();
        let basis = link_basis(Some("https://site.test"), Some("https://site.test:443"));
        // Before: the sender's own host says nothing about the site.
        let before = found_json(&detail, "own", &basis, &[]);
        assert!(
            before["followable_links"].as_array().unwrap().is_empty(),
            "{name}: {before}"
        );
        let n = expand_tracking_links(&mut detail, &Esp, &basis, &[], std::future::pending()).await;
        assert_eq!(n, 1, "{name} is looked through");
        let after = found_json(&detail, "own", &basis, &[]);
        let link = after["followable_links"][0].as_str().unwrap_or_default();
        assert!(
            link.starts_with("https://www.site.test/verify"),
            "{name}: {after}"
        );
    }
}

/// A resolver that lands on another site: the wrapped link is judged by where it goes.
struct Elsewhere;

#[async_trait]
impl TrackingResolver for Elsewhere {
    async fn hop(&self, _url: &str) -> Option<String> {
        Some("https://phish.test/login".to_owned())
    }
}

#[tokio::test]
async fn a_wrapped_link_that_leads_to_another_site_stays_external() {
    let (imap, conn) = junk_rig().await;
    let uid = imap.deliver(
        "INBOX",
        html_mail(
            "Site <no-reply@site.test>",
            "Confirm",
            "x@site.test",
            &date_ago(10),
            "<a href=\"https://u1.ct.sendgrid.net/ls/click?upn=abc\">Confirm</a>",
        ),
    );
    let mut detail = conn
        .scoped(Some(BOT))
        .fetch_id(Some("own"), "INBOX", &uid.to_string(), 20_000)
        .await
        .unwrap();
    let basis = link_basis(Some("https://site.test"), Some("https://site.test:443"));
    expand_tracking_links(&mut detail, &Elsewhere, &basis, &[], std::future::pending()).await;
    let v = found_json(&detail, "own", &basis, &[]);
    assert!(v["followable_links"].as_array().unwrap().is_empty(), "{v}");
    assert!(v.to_string().contains("phish.test"), "{v}");
}

// ---- A reply to the owner's own address, in its own thread, needs no card ------------------------------------

fn original(from: &str, extra: &str, id: &str) -> Vec<u8> {
    format!(
        "From: {from}\r\nTo: bot@example.com\r\nSubject: Hello bot\r\nMessage-ID: <{id}>\r\n{extra}Date: {}\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nCan you confirm?\r\n",
        date_ago(60)
    )
    .into_bytes()
}

async fn owner_rig(allowed: &[&str]) -> (FakeImap, crate::testutil::Server, Arc<EmailConnector>) {
    let imap = FakeImap::start(&[("INBOX", None), ("Drafts", Some("\\Drafts"))]).await;
    let smtp = server(Protocol::Smtp).await;
    let mut account = imap.account("own", Some(BOT), smtp.port, &smtp.pem);
    account.allowed_senders = allowed.iter().map(|s| (*s).to_owned()).collect();
    let conn = imap.connector(account);
    (imap, smtp, conn)
}

fn sent_count(smtp: &crate::testutil::Server) -> usize {
    smtp.messages.lock().unwrap().len()
}

#[tokio::test]
async fn a_reply_to_the_owners_address_goes_out_without_a_card_and_is_threaded() {
    let (imap, smtp, conn) = owner_rig(&["ann@example.com"]).await;
    let uid = imap.deliver(
        "INBOX",
        original("Ann <ann@example.com>", "", "o1@example.com"),
    );
    let input = json!({"reply_to_uid": uid, "body": "Done, thanks."});
    let t = tool(&conn, "email_send");
    // The loop asks the tool first: a grant stands in for the card.
    assert!(t.always_ask_for(&input));
    let grant = t.standing_grant(&ctx(), &input).await.expect("a grant");
    assert_eq!(grant.id, "mail.owner_thread_reply");
    assert!(grant.label.contains("ann@example.com"), "{}", grant.label);
    // The call runs with no card ever built.
    let out = t.call(&ctx(), input).await.map(|o| json_of(&o)).unwrap();
    assert_eq!(out["sent"], true, "{out}");
    assert_eq!(out["owner_sent_anyway"], false, "{out}");
    let raw = String::from_utf8(smtp.messages.lock().unwrap()[0].clone()).unwrap();
    assert!(raw.contains("In-Reply-To: <o1@example.com>"), "{raw}");
    assert!(raw.contains("References: <o1@example.com>"), "{raw}");
    assert!(raw.contains("Subject: Re: Hello bot"), "{raw}");
}

#[tokio::test]
async fn a_second_reply_to_the_owner_is_not_held_back_as_a_repeat_recipient() {
    let (imap, smtp, conn) = owner_rig(&["ann@example.com"]).await;
    let uid = imap.deliver(
        "INBOX",
        original("Ann <ann@example.com>", "", "o1@example.com"),
    );
    for _ in 0..2 {
        let input = json!({"reply_to_uid": uid, "body": "Noted."});
        // The loop builds the details first (it freezes a card nobody is shown).
        let t = tool(&conn, "email_send");
        t.approval_details(&ctx(), &input).await.unwrap();
        assert!(t.standing_grant(&ctx(), &input).await.is_some());
        let out = t.call(&ctx(), input).await.map(|o| json_of(&o)).unwrap();
        assert_eq!(out["sent"], true, "{out}");
        assert_eq!(out["owner_sent_anyway"], false, "never an override: {out}");
    }
    assert_eq!(sent_count(&smtp), 2);
}

/// Every way the grant must NOT apply: the call then needs a shown card, as before.
#[tokio::test]
async fn nothing_but_an_exact_owner_address_in_its_own_thread_skips_the_card() {
    // 1. A domain entry is "who may talk to the bot", not "the owner's own address".
    let (imap, smtp, conn) = owner_rig(&["@example.com"]).await;
    let uid = imap.deliver(
        "INBOX",
        original("Ann <ann@example.com>", "", "o1@example.com"),
    );
    let input = json!({"reply_to_uid": uid, "body": "x"});
    let t = tool(&conn, "email_send");
    assert!(t.standing_grant(&ctx(), &input).await.is_none());
    let e = t.call(&ctx(), input).await.unwrap_err();
    assert!(
        e.to_string().contains("not the one the owner approved"),
        "{e}"
    );
    assert_eq!(sent_count(&smtp), 0);

    // 2. A sender who is not listed.
    let (imap, smtp, conn) = owner_rig(&["ann@example.com"]).await;
    let uid = imap.deliver(
        "INBOX",
        original("Mallory <mallory@evil.test>", "", "o2@evil.test"),
    );
    let input = json!({"reply_to_uid": uid, "body": "x"});
    let t = tool(&conn, "email_send");
    assert!(t.standing_grant(&ctx(), &input).await.is_none());
    assert!(t.call(&ctx(), input).await.is_err());
    assert_eq!(sent_count(&smtp), 0);

    // 3. The listed address in From, but Reply-To sends the answer elsewhere.
    let (imap, smtp, conn) = owner_rig(&["ann@example.com"]).await;
    let uid = imap.deliver(
        "INBOX",
        original(
            "Ann <ann@example.com>",
            "Reply-To: drop@evil.test\r\n",
            "o3@example.com",
        ),
    );
    let input = json!({"reply_to_uid": uid, "body": "x"});
    let t = tool(&conn, "email_send");
    assert!(t.standing_grant(&ctx(), &input).await.is_none());
    assert!(t.call(&ctx(), input).await.is_err());
    assert_eq!(sent_count(&smtp), 0);

    // 4. The model names recipients, asks for reply-all, or sends a plain mail.
    let (imap, smtp, conn) = owner_rig(&["ann@example.com"]).await;
    let uid = imap.deliver(
        "INBOX",
        original("Ann <ann@example.com>", "", "o4@example.com"),
    );
    let t = tool(&conn, "email_send");
    for input in [
        json!({"reply_to_uid": uid, "to": ["x@evil.test"], "body": "x"}),
        json!({"reply_to_uid": uid, "cc": ["x@evil.test"], "body": "x"}),
        json!({"reply_to_uid": uid, "to": ["ann@example.com"], "body": "x"}),
        json!({"reply_to_uid": uid, "reply_all": true, "body": "x"}),
    ] {
        assert!(t.standing_grant(&ctx(), &input).await.is_none(), "{input}");
        assert!(t.call(&ctx(), input).await.is_err());
    }
    // A plain mail (not a reply) is never an owner-thread reply, whoever it goes to.
    let plain = json!({"to": ["ann@example.com"], "subject": "hi", "body": "x"});
    assert!(t.standing_grant(&ctx(), &plain).await.is_none());
    assert_eq!(sent_count(&smtp), 0);
}

#[tokio::test]
async fn the_owner_thread_grant_respects_the_opt_out_list_and_draft_only_and_the_binding() {
    // Opt-out list.
    let (imap, smtp, conn) = owner_rig(&["ann@example.com"]).await;
    let uid = imap.deliver(
        "INBOX",
        original("Ann <ann@example.com>", "", "o1@example.com"),
    );
    conn.guard()
        .add_opt_out("ann@example.com", "asked")
        .unwrap();
    let input = json!({"reply_to_uid": uid, "body": "x"});
    let t = tool(&conn, "email_send");
    assert!(t.standing_grant(&ctx(), &input).await.is_none());
    assert!(t.call(&ctx(), input).await.is_err());
    assert_eq!(sent_count(&smtp), 0);

    // Draft only: no grant, and a send is a draft.
    let imap = FakeImap::start(&[("INBOX", None), ("Drafts", Some("\\Drafts"))]).await;
    let mut account = imap.account("own", Some(BOT), 1, "");
    account.allowed_senders = vec!["ann@example.com".into()];
    account.identity.send_mode = crate::SendMode::DraftOnly;
    let conn = imap.connector(account);
    let uid = imap.deliver(
        "INBOX",
        original("Ann <ann@example.com>", "", "o1@example.com"),
    );
    let input = json!({"reply_to_uid": uid, "body": "x"});
    assert!(
        tool(&conn, "email_send")
            .standing_grant(&ctx(), &input)
            .await
            .is_none()
    );

    // An account that is not this bot's own identity (bound to another bot) is not reachable at all,
    // and an unbound one every bot may use is not "own" either.
    let imap = FakeImap::start(&[("INBOX", None), ("Drafts", Some("\\Drafts"))]).await;
    let mut account = imap.account("pool", None, 1, "");
    account.allowed_senders = vec!["ann@example.com".into()];
    let conn = imap.connector(account);
    let uid = imap.deliver(
        "INBOX",
        original("Ann <ann@example.com>", "", "o1@example.com"),
    );
    let input = json!({"reply_to_uid": uid, "body": "x"});
    assert!(
        tool(&conn, "email_send")
            .standing_grant(&ctx(), &input)
            .await
            .is_none()
    );
    let other = ToolContext {
        bot_id: Some(OTHER_BOT.to_owned()),
        ..ctx()
    };
    assert!(
        tool(&conn, "email_send")
            .standing_grant(&other, &input)
            .await
            .is_none()
    );
}

#[tokio::test]
async fn a_reply_to_anyone_else_still_needs_the_card_and_still_goes_out_once_approved() {
    let (imap, smtp, conn) = owner_rig(&["ann@example.com"]).await;
    let uid = imap.deliver(
        "INBOX",
        original("Bob <bob@example.com>", "", "o9@example.com"),
    );
    let input = json!({"reply_to_uid": uid, "body": "Hello Bob"});
    let t = tool(&conn, "email_send");
    assert!(t.always_ask(), "a send is always confirmed");
    assert!(t.standing_grant(&ctx(), &input).await.is_none());
    assert!(t.call(&ctx(), input.clone()).await.is_err());
    let out = approve_then_call(&conn, "email_send", input).await.unwrap();
    assert_eq!(out["sent"], true);
    assert_eq!(sent_count(&smtp), 1);
}

#[tokio::test]
async fn an_agentmail_reply_threads_through_the_providers_reply_endpoint() {
    let server = MockServer::start().await;
    let mut original = am_msg("m1", "Hello bot", &["received"]);
    original["from"] = json!("Ann <ann@example.com>");
    Mock::given(method("GET"))
        .and(path(format!("/v0/inboxes/{INBOX_ENC}/messages/m1")))
        .respond_with(ResponseTemplate::new(200).set_body_json(original))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/v0/inboxes/{INBOX_ENC}/messages/m1/reply")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"message_id": "r1", "thread_id": "t"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let mut acct = am_account("bot-mail", Some(BOT));
    acct.allowed_senders = vec!["ann@example.com".into()];
    let conn = am_connector(&server, vec![acct]);
    let input = json!({"reply_to_message_id": "m1", "body": "Hi Ann"});
    let t = tool(&conn, "email_send");
    assert!(t.standing_grant(&ctx(), &input).await.is_some());
    let out = t.call(&ctx(), input).await.map(|o| json_of(&o)).unwrap();
    assert_eq!(out["sent"], true, "{out}");
    assert_eq!(out["in_reply_to"], "m1");
}

// ---- Attachments: the bot's workspace, a safe name, what the bytes are ---------------------------------------

struct Place {
    root: PathBuf,
    saved: Mutex<Vec<PathBuf>>,
    max: Option<u64>,
}

impl Place {
    fn new(root: &Path, max: Option<u64>) -> Arc<Self> {
        Arc::new(Self {
            root: root.to_path_buf(),
            saved: Mutex::new(Vec::new()),
            max,
        })
    }
}

impl AttachmentPlace for Place {
    fn folder_for(&self, bot: Option<&str>) -> Option<PathBuf> {
        (bot == Some(BOT)).then(|| self.root.clone())
    }

    fn allow_write(&self, _bot: Option<&str>, _path: &Path, bytes: u64) -> Result<(), String> {
        match self.max {
            Some(m) if bytes > m => Err("the bot's workspace is full".into()),
            _ => Ok(()),
        }
    }

    fn saved(&self, path: &Path) {
        self.saved.lock().unwrap().push(path.to_path_buf());
    }
}

async fn attachment_rig(
    name: &str,
    declared: &str,
    bytes: &[u8],
) -> (FakeImap, Arc<EmailConnector>, u32) {
    let imap = FakeImap::start(&[("INBOX", None), ("Drafts", Some("\\Drafts"))]).await;
    let conn = imap.connector(imap.account("own", Some(BOT), 1, ""));
    let uid = imap.deliver(
        "INBOX",
        attachment_mail("Files", &date_ago(20), name, declared, bytes),
    );
    (imap, conn, uid)
}

#[tokio::test]
async fn an_attachment_is_saved_in_the_bots_workspace_and_the_host_is_told() {
    let (_imap, conn, uid) = attachment_rig("../../notes.txt", "text/plain", b"hello").await;
    let ws = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    conn.set_attachments_dir(data.path().to_path_buf());
    let place = Place::new(ws.path(), None);
    conn.set_attachment_place(place.clone());
    let out = call(
        &conn,
        "email_download_attachment",
        json!({"uid": uid, "attachment_id": "0"}),
    )
    .await
    .unwrap();
    let saved = PathBuf::from(out["saved_to"].as_str().unwrap());
    assert_eq!(
        saved,
        ws.path()
            .join("downloads")
            .join("email")
            .join("own")
            .join("notes.txt")
    );
    assert_eq!(std::fs::read(&saved).unwrap(), b"hello");
    assert_eq!(out["saved_as"], "notes.txt");
    assert_eq!(out["content_type"], "text/plain");
    assert_eq!(out["executed"], false);
    assert_eq!(*place.saved.lock().unwrap(), std::slice::from_ref(&saved));
    assert!(std::fs::read_dir(data.path()).unwrap().next().is_none());

    // The same file again never replaces the first.
    let again = call(
        &conn,
        "email_download_attachment",
        json!({"uid": uid, "attachment_id": "0"}),
    )
    .await
    .unwrap();
    assert_eq!(again["saved_as"], "notes-1.txt");
    assert_eq!(std::fs::read(&saved).unwrap(), b"hello");
}

#[tokio::test]
async fn a_bot_without_a_workspace_keeps_the_daemons_folder() {
    let (_imap, conn, uid) = attachment_rig("a.txt", "text/plain", b"hello").await;
    let ws = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    conn.set_attachments_dir(data.path().to_path_buf());
    conn.set_attachment_place(Place::new(ws.path(), None));
    // A caller the place has no folder for (another bot, here) is served by the daemon's folder.
    let other = ToolContext {
        bot_id: Some(OTHER_BOT.to_owned()),
        ..ctx()
    };
    let mut account = conn.accounts().remove(0);
    account.bot = None;
    conn.replace_accounts(vec![account]).unwrap();
    let out = tool(&conn, "email_download_attachment")
        .call(&other, json!({"uid": uid, "attachment_id": "0"}))
        .await
        .map(|o| json_of(&o))
        .unwrap();
    let saved = PathBuf::from(out["saved_to"].as_str().unwrap());
    assert!(saved.starts_with(data.path().join("own")), "{saved:?}");
    assert!(std::fs::read_dir(ws.path()).unwrap().next().is_none());
}

#[tokio::test]
async fn a_full_workspace_refuses_the_download_and_writes_nothing() {
    let (_imap, conn, uid) = attachment_rig("a.txt", "text/plain", b"hello").await;
    let ws = tempfile::tempdir().unwrap();
    conn.set_attachment_place(Place::new(ws.path(), Some(2)));
    let e = call(
        &conn,
        "email_download_attachment",
        json!({"uid": uid, "attachment_id": "0"}),
    )
    .await
    .unwrap_err();
    assert!(e.to_string().contains("workspace is full"), "{e}");
    assert!(std::fs::read_dir(ws.path()).unwrap().next().is_none());
}

#[tokio::test]
async fn a_program_dressed_as_a_pdf_is_saved_as_a_dead_file_and_flagged() {
    let (_imap, conn, uid) = attachment_rig(
        "Invoice 2026.pdf",
        "application/pdf",
        b"MZ\x90\x00\x03 rest",
    )
    .await;
    let ws = tempfile::tempdir().unwrap();
    conn.set_attachment_place(Place::new(ws.path(), None));
    let out = call(
        &conn,
        "email_download_attachment",
        json!({"uid": uid, "attachment_id": "0"}),
    )
    .await
    .unwrap();
    assert_eq!(out["saved_as"], "Invoice 2026.pdf.bin");
    assert_eq!(out["declared_content_type"], "application/pdf");
    assert_eq!(
        out["content_type"],
        "application/vnd.microsoft.portable-executable"
    );
    assert!(
        out["warning"].as_str().unwrap().contains("program"),
        "{out}"
    );
    assert!(out.get("text").is_none());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(out["saved_to"].as_str().unwrap())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0, "no execute bit");
    }
}

#[tokio::test]
async fn a_real_pdf_keeps_its_name_and_a_wrong_declared_type_is_flagged() {
    let (_imap, conn, uid) =
        attachment_rig("report.pdf", "image/png", b"%PDF-1.7\n1 0 obj\n").await;
    let ws = tempfile::tempdir().unwrap();
    conn.set_attachment_place(Place::new(ws.path(), None));
    let out = call(
        &conn,
        "email_download_attachment",
        json!({"uid": uid, "attachment_id": "0"}),
    )
    .await
    .unwrap();
    assert_eq!(out["saved_as"], "report.pdf");
    assert_eq!(out["content_type"], "application/pdf");
    assert!(
        out["warning"].as_str().unwrap().contains("declared type"),
        "{out}"
    );
}

#[tokio::test]
async fn an_attachment_over_the_size_cap_is_refused_before_anything_is_written() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/v0/inboxes/{INBOX_ENC}/messages/m1/attachments/att1"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "attachment_id": "att1", "size": 11 * 1024 * 1024, "filename": "big.zip",
            "download_url": format!("{}/cdn/a", server.uri()),
            "expires_at": "2026-11-12T11:00:00Z"
        })))
        .mount(&server)
        .await;
    let conn = am_connector(&server, vec![am_account("bot-mail", Some(BOT))]);
    let ws = tempfile::tempdir().unwrap();
    conn.set_attachment_place(Place::new(ws.path(), None));
    let e = call(
        &conn,
        "email_download_attachment",
        json!({"message_id": "m1", "attachment_id": "att1"}),
    )
    .await
    .unwrap_err();
    // The provider refuses a declared size above the cap before it downloads anything.
    assert!(e.to_string().contains("oversized"), "{e}");
    assert!(std::fs::read_dir(ws.path()).unwrap().next().is_none());
}

#[tokio::test]
async fn an_attachment_named_like_a_path_never_leaves_the_account_folder() {
    for bad in [
        "../../../etc/passwd",
        "..\\..\\boot.ini",
        "/etc/shadow",
        "a/../../b.txt",
        "....//x",
        "\u{202E}txt.exe",
    ] {
        let (_imap, conn, uid) = attachment_rig(bad, "application/octet-stream", b"data").await;
        let ws = tempfile::tempdir().unwrap();
        conn.set_attachment_place(Place::new(ws.path(), None));
        let out = call(
            &conn,
            "email_download_attachment",
            json!({"uid": uid, "attachment_id": "0"}),
        )
        .await
        .unwrap();
        let saved = PathBuf::from(out["saved_to"].as_str().unwrap());
        let home = ws.path().join("downloads").join("email").join("own");
        assert_eq!(saved.parent().unwrap(), home, "{bad}: {saved:?}");
        assert!(saved.is_file(), "{bad}");
    }
}
