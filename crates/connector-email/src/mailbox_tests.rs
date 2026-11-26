//! The bot's own mailbox end to end over a fake IMAP server and a fake SMTP
//! server: search (sender, subject, body), read, attachments, verification-link
//! extraction, waiting (inbox, junk folder, mail delivered while waiting),
//! background waits, threaded replies and tracking-link resolution.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tool_registry::{ContentPart, Tool, ToolContext, ToolOutput};

use crate::fake_imap::{FakeImap, date_ago, html_mail, mail_with_attachment, text_mail};
use crate::testutil::{Protocol, server};
use crate::wait::{
    TrackingResolver, WaitEnd, WaitFilter, WaitPlan, WaitProgress, Waiters, expand_tracking_links,
    found_json, junk_folders, link_basis, wait_for_mail,
};
use crate::{EmailConnector, FolderInfo};

const BOT: &str = "11111111-1111-4111-8111-111111111111";

async fn rig() -> (FakeImap, Arc<EmailConnector>) {
    let imap = FakeImap::start(&[
        ("INBOX", None),
        ("Junk", Some("\\Junk")),
        ("Drafts", Some("\\Drafts")),
    ])
    .await;
    // The SMTP side is unused here; the port only has to be valid.
    let account = imap.account("own", Some(BOT), 1, "");
    let conn = imap.connector(account);
    (imap, conn)
}

fn ctx() -> ToolContext {
    ToolContext {
        bot_id: Some(BOT.to_owned()),
        run_id: "run-1".into(),
        ..ToolContext::default()
    }
}

fn tool(conn: &Arc<EmailConnector>, name: &str) -> Arc<dyn Tool> {
    crate::tools(conn)
        .into_iter()
        .find(|t| t.spec().name == name)
        .unwrap()
}

async fn call(conn: &Arc<EmailConnector>, name: &str, input: Value) -> Value {
    let out: ToolOutput = tool(conn, name).call(&ctx(), input).await.unwrap();
    let ContentPart::Json { data } = &out.content[0] else {
        panic!("expected JSON");
    };
    data.clone()
}

fn quick() -> WaitPlan {
    WaitPlan {
        timeout: Duration::from_secs(4),
        max_age: Duration::from_secs(900),
        first_pause: Duration::from_millis(40),
        max_pause: Duration::from_millis(80),
    }
}

fn verification(from: &str, id: &str) -> Vec<u8> {
    html_mail(
        from,
        "Please verify your address",
        id,
        &date_ago(30),
        "<p>Your verification code is 482913.</p><a href=\"https://www.site.test/verify?token=abc123\">Verify</a><a href=\"https://evil.test/x\">Other</a>",
    )
}

fn subjects(v: &Value) -> Vec<String> {
    v["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["subject"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn search_filters_by_sender_subject_body_and_read_state() {
    let (imap, conn) = rig().await;
    imap.deliver(
        "INBOX",
        text_mail(
            "Shop <hello@shop.test>",
            "Your invoice",
            "m1@shop.test",
            &date_ago(500),
            "Invoice number 77 is attached.",
        ),
    );
    imap.deliver(
        "INBOX",
        verification("Site <no-reply@site.test>", "m2@site.test"),
    );
    imap.deliver(
        "INBOX",
        text_mail(
            "Ann <ann@example.com>",
            "Lunch?",
            "m3@example.com",
            &date_ago(60),
            "Shall we eat at noon?",
        ),
    );

    let all = call(&conn, "email_search", json!({})).await;
    assert_eq!(all["returned"], 3);
    assert_eq!(subjects(&all)[0], "Lunch?", "newest first");

    let by_from = call(&conn, "email_search", json!({"from": "site.test"})).await;
    assert_eq!(subjects(&by_from), ["Please verify your address"]);

    let by_subject = call(&conn, "email_search", json!({"subject": "invoice"})).await;
    assert_eq!(subjects(&by_subject), ["Your invoice"]);

    let by_body = call(&conn, "email_search", json!({"text": "noon"})).await;
    assert_eq!(subjects(&by_body), ["Lunch?"]);

    let none = call(&conn, "email_search", json!({"text": "no such words"})).await;
    assert_eq!(none["returned"], 0);

    // Reading does not mark read; marking read changes the unread filter.
    let unread = call(&conn, "email_search", json!({"unread": true})).await;
    assert_eq!(unread["returned"], 3);
    call(&conn, "email_mark_read", json!({"uid": 1})).await;
    let unread = call(&conn, "email_search", json!({"unread": true})).await;
    assert_eq!(unread["returned"], 2);
}

#[tokio::test]
async fn reading_a_mail_returns_the_body_and_numbered_attachments() {
    let (imap, conn) = rig().await;
    let uid = imap.deliver(
        "INBOX",
        mail_with_attachment("Notes for you", &date_ago(20)),
    );
    let read = call(&conn, "email_read", json!({"uid": uid})).await;
    let m = &read["message"];
    assert!(
        m["body_text"]
            .as_str()
            .unwrap()
            .contains("See the attachment.")
    );
    assert_eq!(m["attachments"][0]["id"], "0");
    assert_eq!(m["attachments"][0]["filename"], "notes.txt");
    assert!(read["untrusted_external_content"].as_bool().unwrap());
}

#[tokio::test]
async fn an_attachment_is_downloaded_to_the_attachment_folder_and_never_executed() {
    let (imap, conn) = rig().await;
    let dir = tempfile::tempdir().unwrap();
    conn.set_attachments_dir(dir.path().to_path_buf());
    let uid = imap.deliver(
        "INBOX",
        mail_with_attachment("Notes for you", &date_ago(20)),
    );
    let out = call(
        &conn,
        "email_download_attachment",
        json!({"uid": uid, "attachment_id": "0"}),
    )
    .await;
    let saved = std::path::PathBuf::from(out["saved_to"].as_str().unwrap());
    assert!(saved.starts_with(dir.path()));
    assert_eq!(std::fs::read(&saved).unwrap(), b"hello attachment");
    assert_eq!(out["executed"], false);
    assert_eq!(out["text"], "hello attachment");
    assert_eq!(out["filename"], "notes.txt");

    let e = tool(&conn, "email_download_attachment")
        .call(&ctx(), json!({"uid": uid, "attachment_id": "9"}))
        .await
        .unwrap_err();
    assert!(e.to_string().contains("not found"), "{e}");
}

#[tokio::test]
async fn the_verification_link_is_extracted_judged_and_the_code_found() {
    let (imap, conn) = rig().await;
    let uid = imap.deliver(
        "INBOX",
        verification("Site <no-reply@site.test>", "m2@site.test"),
    );
    let scoped = conn.scoped(Some(BOT));
    let detail = scoped
        .fetch_id(Some("own"), "INBOX", &uid.to_string(), 20_000)
        .await
        .unwrap();
    let basis = link_basis(Some("https://site.test"), Some("https://www.site.test:443"));
    let v = found_json(&detail, "own", &basis, &[]);
    assert_eq!(v["codes"][0], "482913");
    assert_eq!(
        v["followable_links"][0],
        "https://www.site.test/verify?token=abc123"
    );
    let other = serde_json::to_string(&v["other_links"]).unwrap();
    assert!(other.contains("evil.test"), "{other}");
    assert!(
        !v.to_string().contains("evil.test/x"),
        "no foreign URL is handed out"
    );
}

struct Stub;

#[async_trait]
impl TrackingResolver for Stub {
    async fn hop(&self, url: &str) -> Option<String> {
        url.contains("sendgrid.net")
            .then(|| "https://www.site.test/confirm?t=real-token".to_owned())
    }
}

#[tokio::test]
async fn a_click_tracking_link_is_looked_through_one_hop_and_the_target_judged() {
    let (imap, conn) = rig().await;
    let uid = imap.deliver(
        "INBOX",
        html_mail(
            "Site <no-reply@site.test>",
            "Confirm your email",
            "trk@site.test",
            &date_ago(10),
            "<a href=\"https://u123.ct.sendgrid.net/ls/click?upn=abc\">Confirm</a><a href=\"https://other-tracker.test/c/1\">x</a>",
        ),
    );
    let scoped = conn.scoped(Some(BOT));
    let mut detail = scoped
        .fetch_id(Some("own"), "INBOX", &uid.to_string(), 20_000)
        .await
        .unwrap();
    let basis = link_basis(Some("https://site.test"), Some("https://site.test:443"));
    // Before: the tracker is just an external host.
    let before = found_json(&detail, "own", &basis, &[]);
    assert_eq!(before["followable_links"].as_array().unwrap().len(), 0);
    let n = expand_tracking_links(&mut detail, &Stub, &basis, &[]).await;
    assert_eq!(n, 1, "only known tracking hosts are looked through");
    let after = found_json(&detail, "own", &basis, &[]);
    assert_eq!(
        after["followable_links"][0],
        "https://www.site.test/confirm?t=real-token"
    );
}

#[test]
fn junk_folders_are_found_by_attribute_or_name() {
    let f = |name: &str, attrs: &[&str]| FolderInfo {
        name: name.into(),
        delimiter: Some("/".into()),
        attributes: attrs.iter().map(|a| (*a).to_owned()).collect(),
    };
    let list = [
        f("INBOX", &[]),
        f("Sent", &[]),
        f("Weird", &["Junk"]),
        f("[Gmail]/Spam", &[]),
        f("Önemsiz E-posta", &[]),
        f("Archive", &[]),
    ];
    assert_eq!(
        junk_folders(&list),
        ["Weird", "[Gmail]/Spam"],
        "capped at two"
    );
    assert_eq!(junk_folders(&list[..2]), Vec::<String>::new());
}

#[tokio::test]
async fn wait_finds_mail_that_is_already_there_and_ignores_other_senders() {
    let (imap, conn) = rig().await;
    imap.deliver(
        "INBOX",
        text_mail(
            "Ann <ann@example.com>",
            "Verify (not from the site)",
            "x1@example.com",
            &date_ago(20),
            "hello",
        ),
    );
    imap.deliver(
        "INBOX",
        verification("Site <no-reply@site.test>", "m2@site.test"),
    );
    let scoped = conn.scoped(Some(BOT));
    let filter = WaitFilter::new(Some("site.test"), None, None).unwrap();
    let end = wait_for_mail(
        &scoped,
        Some("own"),
        &filter,
        &quick(),
        std::future::pending(),
    )
    .await
    .unwrap();
    let WaitEnd::Found(d) = end else {
        panic!("expected the mail")
    };
    assert_eq!(d.summary.from_addr, "no-reply@site.test");
}

#[tokio::test]
async fn wait_polls_the_junk_folder_too() {
    let (imap, conn) = rig().await;
    imap.deliver(
        "Junk",
        verification("Site <no-reply@site.test>", "junk1@site.test"),
    );
    let scoped = conn.scoped(Some(BOT));
    let filter = WaitFilter::new(Some("site.test"), None, None).unwrap();
    let end = wait_for_mail(
        &scoped,
        Some("own"),
        &filter,
        &quick(),
        std::future::pending(),
    )
    .await
    .unwrap();
    assert!(matches!(end, WaitEnd::Found(_)), "{end:?}");
}

#[tokio::test]
async fn wait_returns_mail_delivered_while_it_waits() {
    let (imap, conn) = rig().await;
    let imap = Arc::new(imap);
    let later = Arc::clone(&imap);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        later.deliver(
            "INBOX",
            verification("Site <no-reply@site.test>", "late@site.test"),
        );
    });
    let scoped = conn.scoped(Some(BOT));
    let filter = WaitFilter::new(Some("site.test"), None, Some("482913")).unwrap();
    let end = wait_for_mail(
        &scoped,
        Some("own"),
        &filter,
        &quick(),
        std::future::pending(),
    )
    .await
    .unwrap();
    assert!(matches!(end, WaitEnd::Found(_)), "{end:?}");
}

#[tokio::test]
async fn wait_times_out_without_matching_mail() {
    let (_imap, conn) = rig().await;
    let scoped = conn.scoped(Some(BOT));
    let plan = WaitPlan {
        timeout: Duration::from_millis(200),
        ..quick()
    };
    let filter = WaitFilter::new(Some("site.test"), None, None).unwrap();
    let end = wait_for_mail(&scoped, Some("own"), &filter, &plan, std::future::pending())
        .await
        .unwrap();
    assert!(
        matches!(end, WaitEnd::TimedOut { polls } if polls >= 1),
        "{end:?}"
    );
}

#[tokio::test]
async fn a_background_wait_is_pending_then_collected_once_and_never_blocks_long() {
    let (imap, conn) = rig().await;
    let waiters = Waiters::new();
    let filter = WaitFilter::new(Some("site.test"), None, None).unwrap();
    let id = waiters
        .start(&conn, Some(BOT), "run-1", "own", &filter, &quick())
        .unwrap();
    // The same wait started again joins the running one.
    assert_eq!(
        waiters
            .start(&conn, Some(BOT), "run-1", "own", &filter, &quick())
            .unwrap(),
        id
    );
    // Another bot cannot collect it.
    assert!(matches!(
        waiters
            .progress(
                &id,
                Some("someone-else"),
                Duration::from_millis(10),
                std::future::pending()
            )
            .await,
        WaitProgress::Unknown
    ));
    // Nothing yet: the call returns after its short block, pending.
    let started = std::time::Instant::now();
    let p = waiters
        .progress(
            &id,
            Some(BOT),
            Duration::from_millis(150),
            std::future::pending(),
        )
        .await;
    assert!(matches!(p, WaitProgress::Pending { .. }), "{p:?}");
    assert!(started.elapsed() < Duration::from_secs(2));

    imap.deliver(
        "INBOX",
        verification("Site <no-reply@site.test>", "bg@site.test"),
    );
    let p = waiters
        .progress(
            &id,
            Some(BOT),
            Duration::from_secs(3),
            std::future::pending(),
        )
        .await;
    assert!(
        matches!(p, WaitProgress::Finished(WaitEnd::Found(_))),
        "{p:?}"
    );
    // Collected once, then forgotten.
    assert!(matches!(
        waiters
            .progress(
                &id,
                Some(BOT),
                Duration::from_millis(10),
                std::future::pending()
            )
            .await,
        WaitProgress::Unknown
    ));
}

#[tokio::test]
async fn stopping_a_run_ends_its_background_waits() {
    let (_imap, conn) = rig().await;
    let waiters = Waiters::new();
    let filter = WaitFilter::new(Some("site.test"), None, None).unwrap();
    let id = waiters
        .start(&conn, Some(BOT), "run-9", "own", &filter, &quick())
        .unwrap();
    waiters.cancel_tagged("run-9");
    assert!(matches!(
        waiters
            .progress(
                &id,
                Some(BOT),
                Duration::from_millis(10),
                std::future::pending()
            )
            .await,
        WaitProgress::Unknown
    ));
}

#[tokio::test]
async fn a_reply_carries_the_threading_headers_and_goes_to_the_sender() {
    let imap = FakeImap::start(&[("INBOX", None), ("Drafts", Some("\\Drafts"))]).await;
    let smtp = server(Protocol::Smtp).await;
    let account = imap.account("own", Some(BOT), smtp.port, &smtp.pem);
    let conn = imap.connector(account);
    let uid = imap.deliver(
        "INBOX",
        text_mail(
            "Ann Example <ann@example.com>",
            "Hello bot",
            "orig-1@example.com",
            &date_ago(60),
            "Can you confirm?",
        ),
    );
    let out = call(
        &conn,
        "email_send",
        json!({"reply_to_uid": uid, "body": "Confirmed, thanks."}),
    )
    .await;
    assert_eq!(out["sent"], true, "{out}");
    let sent = smtp.messages.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    let raw = String::from_utf8(sent[0].clone()).unwrap();
    assert!(raw.contains("Subject: Re: Hello bot"), "{raw}");
    assert!(raw.contains("In-Reply-To: <orig-1@example.com>"), "{raw}");
    assert!(raw.contains("References: <orig-1@example.com>"), "{raw}");
    assert!(raw.contains("ann@example.com"), "{raw}");
}

#[test]
fn failures_come_with_a_plain_fix_hint() {
    use crate::EmailError;
    let e: tool_registry::ToolError = EmailError::Imap("login failed".into()).into();
    assert!(e.to_string().contains("Fix:"), "{e}");
    assert!(e.to_string().contains("Test connection"), "{e}");
    let e: tool_registry::ToolError = EmailError::Imap("examine rejected".into()).into();
    assert!(e.to_string().contains("email_list_folders"), "{e}");
    let e: tool_registry::ToolError = EmailError::Credential("missing".into()).into();
    assert!(e.to_string().contains("Settings"), "{e}");
}
