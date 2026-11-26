//! The bot's own mailbox end to end over a fake IMAP server and a fake SMTP
//! server: search (sender, subject, body), read, attachments, verification-link
//! extraction, waiting (inbox, junk folder, mail delivered while waiting),
//! background waits, threaded replies and tracking-link resolution.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tool_registry::{ContentPart, Tool, ToolContext, ToolOutput};

use crate::fake_imap::{
    FakeImap, attachment_mail, date_ago, html_mail, mail_with_attachment, text_mail,
};
use crate::testutil::{Protocol, server};
use crate::wait::{
    TrackingResolver, WaitEnd, WaitFilter, WaitOwner, WaitPlan, WaitProgress, Waiters,
    expand_tracking_links, found_json, junk_folders, link_basis, wait_for_mail,
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

/// A call the way the loop makes it: the tool's approval card is built first (what the owner
/// is shown, and what the tool freezes), then the approved call runs.
async fn approve_then_call(
    conn: &Arc<EmailConnector>,
    name: &str,
    input: Value,
) -> Result<Value, tool_registry::ToolError> {
    let t = tool(conn, name);
    t.approval_details(&ctx(), &input).await?;
    let out: ToolOutput = t.call(&ctx(), input).await?;
    let ContentPart::Json { data } = &out.content[0] else {
        panic!("expected JSON");
    };
    Ok(data.clone())
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
    let n = expand_tracking_links(&mut detail, &Stub, &basis, &[], std::future::pending()).await;
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

fn owner<'a>(run: &'a str, account: &'a str) -> WaitOwner<'a> {
    WaitOwner {
        bot: Some(BOT),
        run,
        account,
    }
}

/// `progress` as the tool calls it: short blocks, until the wait is no longer pending. One
/// mailbox round trip (TLS, login, folder list, search) can take seconds on a slow runner, so
/// a single short block says nothing about whether the wait works; the bound is a minute.
async fn collect(waiters: &Waiters, id: &str, who: WaitOwner<'_>) -> WaitProgress {
    let give_up = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let p = waiters
            .progress(id, who, Duration::from_secs(2), std::future::pending())
            .await;
        if !matches!(p, WaitProgress::Pending { .. }) || std::time::Instant::now() > give_up {
            return p;
        }
    }
}

/// A plan that outlives a slow runner; the mail is found by the poll after it is delivered.
fn patient() -> WaitPlan {
    WaitPlan {
        timeout: Duration::from_secs(90),
        ..quick()
    }
}

#[tokio::test]
async fn a_background_wait_is_pending_then_collected_once_and_never_blocks_long() {
    let (imap, conn) = rig().await;
    let waiters = Waiters::new();
    let filter = WaitFilter::new(Some("site.test"), None, None).unwrap();
    let mine = owner("run-1", "own");
    let id = waiters.start(&conn, mine, &filter, &patient()).unwrap();
    // The same wait started again by the same run joins the one still polling.
    assert_eq!(waiters.start(&conn, mine, &filter, &patient()).unwrap(), id);
    // Another bot cannot collect it.
    let other_bot = WaitOwner {
        bot: Some("someone-else"),
        ..mine
    };
    assert!(matches!(
        waiters
            .progress(
                &id,
                other_bot,
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
            mine,
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
    let p = collect(&waiters, &id, mine).await;
    assert!(
        matches!(p, WaitProgress::Finished(WaitEnd::Found(_))),
        "{p:?}"
    );
    // Collected once, then forgotten.
    assert!(matches!(
        waiters
            .progress(&id, mine, Duration::from_millis(10), std::future::pending())
            .await,
        WaitProgress::Unknown
    ));
}

#[tokio::test]
async fn stopping_a_run_ends_its_background_waits() {
    let (_imap, conn) = rig().await;
    let waiters = Waiters::new();
    let filter = WaitFilter::new(Some("site.test"), None, None).unwrap();
    let mine = owner("run-9", "own");
    let id = waiters.start(&conn, mine, &filter, &quick()).unwrap();
    waiters.cancel_tagged("run-9");
    assert!(matches!(
        waiters
            .progress(&id, mine, Duration::from_millis(10), std::future::pending())
            .await,
        WaitProgress::Unknown
    ));
}

/// Another run of the same bot (a scheduled one, or one a stranger's mail started) must not
/// join, collect or stop this run's wait for a verification mail.
#[tokio::test]
async fn a_wait_belongs_to_the_run_that_started_it() {
    let (_imap, conn) = rig().await;
    let waiters = Waiters::new();
    let filter = WaitFilter::new(Some("site.test"), None, None).unwrap();
    let (chat, scheduled) = (owner("run-chat", "own"), owner("run-sched", "own"));
    let first = waiters.start(&conn, chat, &filter, &patient()).unwrap();
    // The identical ask from another run is a wait of its own, not a join.
    let second = waiters
        .start(&conn, scheduled, &filter, &patient())
        .unwrap();
    assert_ne!(first, second);
    // It cannot read the other run's id...
    assert!(matches!(
        waiters
            .progress(
                &first,
                scheduled,
                Duration::from_millis(10),
                std::future::pending()
            )
            .await,
        WaitProgress::Unknown
    ));
    // ...and stopping it leaves the other run's wait alone.
    waiters.cancel_tagged("run-sched");
    assert!(matches!(
        waiters
            .progress(
                &first,
                chat,
                Duration::from_millis(10),
                std::future::pending()
            )
            .await,
        WaitProgress::Pending { .. }
    ));
    assert!(matches!(
        waiters
            .progress(
                &second,
                scheduled,
                Duration::from_millis(10),
                std::future::pending()
            )
            .await,
        WaitProgress::Unknown
    ));
}

/// The id alone is not enough: it must be asked about for the account it polls.
#[tokio::test]
async fn a_wait_is_collected_only_for_its_own_account() {
    let (_imap, conn) = rig().await;
    let waiters = Waiters::new();
    let filter = WaitFilter::new(Some("site.test"), None, None).unwrap();
    let id = waiters
        .start(&conn, owner("run-1", "own"), &filter, &patient())
        .unwrap();
    assert!(matches!(
        waiters
            .progress(
                &id,
                owner("run-1", "work"),
                Duration::from_millis(10),
                std::future::pending()
            )
            .await,
        WaitProgress::Unknown
    ));
    assert!(matches!(
        waiters
            .progress(
                &id,
                owner("run-1", "own"),
                Duration::from_millis(10),
                std::future::pending()
            )
            .await,
        WaitProgress::Pending { .. }
    ));
}

/// A finished wait holds an old answer; asking again must poll afresh, not hand it back.
#[tokio::test]
async fn a_finished_wait_is_not_joined_by_a_new_ask() {
    let (imap, conn) = rig().await;
    let waiters = Waiters::new();
    imap.deliver(
        "INBOX",
        verification("Site <no-reply@site.test>", "old@site.test"),
    );
    let filter = WaitFilter::new(Some("site.test"), None, None).unwrap();
    let mine = owner("run-1", "own");
    let old = waiters.start(&conn, mine, &filter, &patient()).unwrap();
    // Let it find the mail already there, and leave the answer uncollected.
    tokio::time::timeout(Duration::from_secs(60), async {
        while !waiters.is_finished(&old) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the wait finishes");
    let fresh = waiters.start(&conn, mine, &filter, &patient()).unwrap();
    assert_ne!(old, fresh, "a finished wait is never reused");
}

/// Ids are random UUIDs, not a counter and a clock that another run could guess.
#[tokio::test]
async fn wait_ids_cannot_be_guessed() {
    let (_imap, conn) = rig().await;
    let waiters = Waiters::new();
    let a = waiters
        .start(
            &conn,
            owner("run-1", "own"),
            &WaitFilter::new(Some("a.test"), None, None).unwrap(),
            &quick(),
        )
        .unwrap();
    let b = waiters
        .start(
            &conn,
            owner("run-1", "own"),
            &WaitFilter::new(Some("b.test"), None, None).unwrap(),
            &quick(),
        )
        .unwrap();
    assert_ne!(a, b);
    for id in [&a, &b] {
        let uuid = id.strip_prefix("mw-").expect("prefix");
        let parsed = uuid::Uuid::parse_str(uuid).expect("a uuid");
        assert_eq!(parsed.get_version_num(), 4, "{id}");
    }
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
    let out = approve_then_call(
        &conn,
        "email_send",
        json!({"reply_to_uid": uid, "body": "Confirmed, thanks."}),
    )
    .await
    .unwrap();
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

// ---- Review findings: attachments ----------------------------------------------------------

#[tokio::test]
async fn an_attachment_declared_as_text_that_is_not_text_is_saved_but_not_inlined() {
    let (imap, conn) = rig().await;
    let dir = tempfile::tempdir().unwrap();
    conn.set_attachments_dir(dir.path().to_path_buf());
    // The sender says text/plain; the bytes carry NUL and control characters.
    let uid = imap.deliver(
        "INBOX",
        attachment_mail(
            "Invoice",
            &date_ago(20),
            "invoice.txt",
            "text/plain",
            b"MZ\0\0\x01\x02 not really text",
        ),
    );
    let out = call(
        &conn,
        "email_download_attachment",
        json!({"uid": uid, "attachment_id": "0"}),
    )
    .await;
    assert!(out.get("text").is_none(), "{out}");
    assert!(std::path::Path::new(out["saved_to"].as_str().unwrap()).is_file());
}

#[tokio::test]
async fn an_html_attachment_is_inlined_as_what_a_person_sees() {
    let (imap, conn) = rig().await;
    let dir = tempfile::tempdir().unwrap();
    conn.set_attachments_dir(dir.path().to_path_buf());
    let uid = imap.deliver(
        "INBOX",
        attachment_mail(
            "Page",
            &date_ago(20),
            "page.html",
            "text/html",
            b"<p>Visible words</p><div style=\"display:none\">HIDDEN INJECTION</div><!-- COMMENT INJECTION -->",
        ),
    );
    let out = call(
        &conn,
        "email_download_attachment",
        json!({"uid": uid, "attachment_id": "0"}),
    )
    .await;
    let text = out["text"].as_str().unwrap();
    assert!(text.contains("Visible words"), "{text}");
    assert!(!text.contains("INJECTION"), "{text}");
}

#[tokio::test]
async fn a_file_name_with_path_parts_comes_back_as_a_plain_name() {
    let (imap, conn) = rig().await;
    let dir = tempfile::tempdir().unwrap();
    conn.set_attachments_dir(dir.path().to_path_buf());
    let uid = imap.deliver(
        "INBOX",
        attachment_mail(
            "Script",
            &date_ago(20),
            "../../etc/cron.d/evil.sh",
            "application/octet-stream",
            b"echo hi",
        ),
    );
    let out = call(
        &conn,
        "email_download_attachment",
        json!({"uid": uid, "attachment_id": "0"}),
    )
    .await;
    assert_eq!(out["filename"], "evil.sh");
    // Where it was saved never depends on the declared name.
    let saved = std::path::PathBuf::from(out["saved_to"].as_str().unwrap());
    assert!(saved.starts_with(dir.path()), "{saved:?}");
    assert!(!saved.to_string_lossy().contains("evil"), "{saved:?}");
}

#[tokio::test]
async fn an_account_name_that_could_leave_the_attachment_folder_is_refused() {
    let imap = FakeImap::start(&[("INBOX", None)]).await;
    for bad in ["../escape", "a/b", "..", ".hidden", "a\\b", ""] {
        let mut acct = imap.account("placeholder", Some(BOT), 1, "");
        acct.name = bad.into();
        let store = Arc::new(secret_store::MemoryStore::new());
        let conn = EmailConnector::reloadable(store);
        assert!(
            conn.replace_accounts(vec![acct]).is_err(),
            "{bad:?} must not be accepted as an account name"
        );
    }
    let ok = imap.account("work-2.mail", Some(BOT), 1, "");
    assert!(ok.validate().is_ok());
}

#[test]
fn the_attachment_cap_leaves_room_for_base64() {
    let max = crate::MAX_ATTACHMENT_BYTES;
    let cap = crate::imap::attachment_fetch_cap(max);
    // base64: 4 characters per 3 bytes, a CRLF per 76, plus the mail's own headers and text.
    let encoded = max.div_ceil(3) * 4;
    let with_breaks = encoded + (encoded / 76 + 1) * 2;
    assert!(
        cap >= u64::try_from(with_breaks).unwrap() + u64::from(crate::message::MAX_MESSAGE_BYTES),
        "{cap}"
    );
    assert_eq!(crate::imap::attachment_fetch_cap(usize::MAX), u64::MAX);
}

/// The server's `RFC822.SIZE` is its own claim: the bytes it then sends are checked again.
#[tokio::test]
async fn a_server_that_lies_about_the_size_is_refused_after_the_download() {
    let (imap, conn) = rig().await;
    // Just over what a cap of 1000 bytes allows (1000 + 500 + 1 MiB of message), once base64
    // encoded. No larger: this travels over TLS in a debug build on a slow runner.
    let big = vec![b'A'; 850_000];
    let uid = imap.deliver(
        "INBOX",
        attachment_mail(
            "Big",
            &date_ago(20),
            "big.bin",
            "application/octet-stream",
            &big,
        ),
    );
    imap.folders.lock().unwrap().get_mut("INBOX").unwrap()[0].reported_size = Some(100);
    let mut acct = conn.accounts().remove(0);
    // The default of the fixture (5 s) is for small mails; the point here is the size check.
    acct.timeout_secs = 120;
    let provider = conn.provider_for(&acct).await.unwrap();
    // A cap of 1000 bytes lets a message of about a megabyte through its size check (the
    // lie), but not through the check of what really arrived.
    let e = provider
        .get_attachment_in("INBOX", &uid.to_string(), "0", 1000)
        .await
        .map(|_| ())
        .unwrap_err();
    assert!(
        e.to_string().contains("too large to download"),
        "the downloaded bytes must be bounded too: {e}"
    );
}

// ---- Review findings: replies ---------------------------------------------------------------

fn original(from: &str, extra_headers: &str, id: &str) -> Vec<u8> {
    format!(
        "From: {from}\r\nTo: bot@example.com\r\nSubject: Hello bot\r\nMessage-ID: <{id}>\r\n{extra_headers}Date: {}\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nCan you confirm?\r\n",
        date_ago(60)
    )
    .into_bytes()
}

async fn reply_rig() -> (FakeImap, crate::testutil::Server, Arc<EmailConnector>) {
    let imap = FakeImap::start(&[("INBOX", None), ("Drafts", Some("\\Drafts"))]).await;
    let smtp = server(Protocol::Smtp).await;
    let account = imap.account("own", Some(BOT), smtp.port, &smtp.pem);
    let conn = imap.connector(account);
    (imap, smtp, conn)
}

#[tokio::test]
async fn the_card_of_a_reply_shows_the_resolved_recipient_and_subject() {
    let (imap, _smtp, conn) = reply_rig().await;
    let uid = imap.deliver(
        "INBOX",
        original(
            "Ann <ann@example.com>",
            "Reply-To: Billing <billing@elsewhere.test>\r\n",
            "o1@example.com",
        ),
    );
    let card = tool(&conn, "email_send")
        .approval_details(&ctx(), &json!({"reply_to_uid": uid, "body": "Thanks!"}))
        .await
        .unwrap()
        .expect("a mail card");
    // Reply-To wins over From; the owner sees where the answer really goes.
    assert_eq!(card["to"], json!(["billing@elsewhere.test"]));
    assert_eq!(card["subject"], "Re: Hello bot");
    assert!(card["approval_ref"].as_str().is_some());
}

#[tokio::test]
async fn a_reply_that_was_never_shown_to_the_owner_is_not_sent() {
    let (imap, smtp, conn) = reply_rig().await;
    let uid = imap.deliver(
        "INBOX",
        original("Ann <ann@example.com>", "", "o1@example.com"),
    );
    // No approval card was built for this call.
    let e = tool(&conn, "email_send")
        .call(&ctx(), json!({"reply_to_uid": uid, "body": "Thanks!"}))
        .await
        .unwrap_err();
    assert!(
        e.to_string().contains("not the one the owner approved"),
        "{e}"
    );
    assert!(smtp.messages.lock().unwrap().is_empty());
}

/// The owner approves a reply to message N; before the send the message under N is another
/// one (a different sender). The mail that was approved is not the mail that would go out.
#[tokio::test]
async fn a_reply_whose_original_changed_after_approval_is_not_sent() {
    let (imap, smtp, conn) = reply_rig().await;
    let uid = imap.deliver(
        "INBOX",
        original("Ann <ann@example.com>", "", "o1@example.com"),
    );
    let input = json!({"reply_to_uid": uid, "body": "Thanks!"});
    let t = tool(&conn, "email_send");
    t.approval_details(&ctx(), &input).await.unwrap();
    // The same uid now holds a mail from someone else.
    imap.folders.lock().unwrap().get_mut("INBOX").unwrap()[0].raw =
        original("Mallory <mallory@evil.test>", "", "o2@evil.test");
    let e = t.call(&ctx(), input).await.unwrap_err();
    assert!(
        e.to_string().contains("not the one the owner approved"),
        "{e}"
    );
    assert!(
        smtp.messages.lock().unwrap().is_empty(),
        "nothing may be delivered"
    );
}

#[tokio::test]
async fn an_approved_reply_goes_out_once() {
    let (imap, smtp, conn) = reply_rig().await;
    let uid = imap.deliver(
        "INBOX",
        original("Ann <ann@example.com>", "", "o1@example.com"),
    );
    let input = json!({"reply_to_uid": uid, "body": "Thanks!"});
    let out = approve_then_call(&conn, "email_send", input.clone())
        .await
        .unwrap();
    assert_eq!(out["sent"], true, "{out}");
    // The card was used up: the same call again needs a new approval.
    let e = tool(&conn, "email_send")
        .call(&ctx(), input)
        .await
        .unwrap_err();
    assert!(
        e.to_string().contains("not the one the owner approved"),
        "{e}"
    );
    assert_eq!(smtp.messages.lock().unwrap().len(), 1);
}

/// Ids inherited from a stranger's headers that cannot be written safely are left out; the
/// reply still goes out (it must not fail after the owner approved it).
#[tokio::test]
async fn a_reply_to_a_mail_with_bad_reference_ids_still_goes_out_without_them() {
    let (imap, smtp, conn) = reply_rig().await;
    let uid = imap.deliver(
        "INBOX",
        original(
            "Ann <ann@example.com>",
            "References: <good@example.com> <bad\"quote@example.com> <non-\u{e4}scii@example.com> <back\\slash@example.com>\r\n",
            "o1@example.com",
        ),
    );
    let out = approve_then_call(
        &conn,
        "email_send",
        json!({"reply_to_uid": uid, "body": "Thanks!"}),
    )
    .await
    .unwrap();
    assert_eq!(out["sent"], true, "{out}");
    let raw = String::from_utf8(smtp.messages.lock().unwrap()[0].clone()).unwrap();
    assert!(
        raw.contains("References: <good@example.com> <o1@example.com>"),
        "{raw}"
    );
    assert!(!raw.contains("quote@"), "{raw}");
    assert!(!raw.contains("scii@"), "{raw}");
    assert!(!raw.contains("slash@"), "{raw}");
}

#[test]
fn only_plain_printable_ids_may_be_written_into_threading_headers() {
    use crate::smtp::valid_threading_id as v;
    assert_eq!(
        v("<a.b-c@example.com>").as_deref(),
        Some("a.b-c@example.com")
    );
    assert_eq!(v("  x@y  ").as_deref(), Some("x@y"));
    for bad in [
        "",
        "<>",
        "a b@c",
        "a\"b@c",
        "a\\b@c",
        "a<b@c",
        "\u{e9}@example.com",
        "a\u{7f}b@c",
        "a\r\nBcc: x@y",
        &"x".repeat(999),
    ] {
        assert_eq!(v(bad), None, "{bad:?}");
    }
}

#[tokio::test]
async fn a_reply_to_address_that_differs_from_the_sender_is_visible_in_email_read() {
    let (imap, _smtp, conn) = reply_rig().await;
    let spoofed = imap.deliver(
        "INBOX",
        original(
            "Ann <ann@example.com>",
            "Reply-To: attacker@evil.test\r\n",
            "o1@example.com",
        ),
    );
    let same = imap.deliver(
        "INBOX",
        original(
            "Bob <bob@example.com>",
            "Reply-To: bob@example.com\r\n",
            "o2@example.com",
        ),
    );
    let none = imap.deliver(
        "INBOX",
        original("Cy <cy@example.com>", "", "o3@example.com"),
    );
    let read = call(&conn, "email_read", json!({"uid": spoofed})).await;
    assert_eq!(read["message"]["reply_to"], "attacker@evil.test", "{read}");
    for uid in [same, none] {
        let read = call(&conn, "email_read", json!({"uid": uid})).await;
        assert!(read["message"].get("reply_to").is_none(), "{read}");
    }
}

// ---- Review findings: tracking links, junk, credentials ---------------------------------------

struct Never;

#[async_trait]
impl TrackingResolver for Never {
    async fn hop(&self, _: &str) -> Option<String> {
        std::future::pending().await
    }
}

#[tokio::test]
async fn looking_through_tracking_links_ends_when_the_run_is_stopped() {
    let (imap, conn) = rig().await;
    let uid = imap.deliver(
        "INBOX",
        html_mail(
            "Site <no-reply@site.test>",
            "Confirm",
            "trk2@site.test",
            &date_ago(10),
            "<a href=\"https://u1.ct.sendgrid.net/ls/click?upn=a\">Confirm</a>",
        ),
    );
    let mut detail = conn
        .scoped(Some(BOT))
        .fetch_id(Some("own"), "INBOX", &uid.to_string(), 20_000)
        .await
        .unwrap();
    let basis = link_basis(Some("https://site.test"), Some("https://site.test:443"));
    // A hop that never answers, and a run that is already stopped.
    let n = tokio::time::timeout(
        Duration::from_secs(5),
        expand_tracking_links(&mut detail, &Never, &basis, &[], async {}),
    )
    .await
    .expect("a stopped run must not wait for a hop");
    assert_eq!(n, 0);
}

/// `verify::analyse` reads only the first links of a mail: a looked-through target pushed to the
/// end of a link-heavy mail vanished.
#[tokio::test]
async fn a_looked_through_target_is_not_lost_in_a_link_heavy_mail() {
    use std::fmt::Write as _;
    let (imap, conn) = rig().await;
    let mut html =
        String::from("<a href=\"https://u9.ct.sendgrid.net/ls/click?upn=z\">Confirm</a>");
    for i in 0..80 {
        let _ = write!(html, "<a href=\"https://filler{i}.test/p\">x</a>");
    }
    let uid = imap.deliver(
        "INBOX",
        html_mail(
            "Site <no-reply@site.test>",
            "Confirm",
            "trk3@site.test",
            &date_ago(10),
            &html,
        ),
    );
    let mut detail = conn
        .scoped(Some(BOT))
        .fetch_id(Some("own"), "INBOX", &uid.to_string(), 20_000)
        .await
        .unwrap();
    // The mail has more links than are ever read: it already holds the most that are kept.
    assert_eq!(
        detail.links.len(),
        crate::verify::MAX_LINKS_SCANNED,
        "setup"
    );
    let basis = link_basis(Some("https://site.test"), Some("https://site.test:443"));
    let n = expand_tracking_links(&mut detail, &Stub, &basis, &[], std::future::pending()).await;
    assert_eq!(n, 1);
    assert_eq!(
        detail.links[0],
        "https://www.site.test/confirm?t=real-token"
    );
    let v = found_json(&detail, "own", &basis, &[]);
    assert_eq!(
        v["followable_links"][0],
        "https://www.site.test/confirm?t=real-token"
    );
}

#[tokio::test]
async fn the_hop_asks_the_policy_it_was_given() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Counting(AtomicUsize, web_tools::DefaultUrlPolicy);
    impl web_tools::UrlPolicy for Counting {
        fn check_url(&self, url: &url::Url) -> Result<(), web_tools::PolicyViolation> {
            self.0.fetch_add(1, Ordering::SeqCst);
            self.1.check_url(url)
        }
        fn check_ip(
            &self,
            host: &str,
            ip: std::net::IpAddr,
        ) -> Result<(), web_tools::PolicyViolation> {
            self.1.check_ip(host, ip)
        }
    }
    let policy = Arc::new(Counting(
        AtomicUsize::new(0),
        web_tools::DefaultUrlPolicy {
            deny_hosts: vec!["click.sendgrid.net".into()],
            ..web_tools::DefaultUrlPolicy::default()
        },
    ));
    let resolver = crate::WebResolver::with_policy(policy.clone()).unwrap();
    // The daemon's deny list stops it before anything is sent.
    assert_eq!(
        resolver.hop("https://click.sendgrid.net/ls/click").await,
        None
    );
    assert_eq!(policy.0.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_found_mail_says_which_folder_it_was_in_and_whether_that_is_junk() {
    let (imap, conn) = rig().await;
    imap.deliver(
        "Junk",
        verification("Site <no-reply@site.test>", "junk2@site.test"),
    );
    let scoped = conn.scoped(Some(BOT));
    let filter = WaitFilter::new(Some("site.test"), None, None).unwrap();
    let end = wait_for_mail(
        &scoped,
        Some("own"),
        &filter,
        &patient(),
        std::future::pending(),
    )
    .await
    .unwrap();
    let WaitEnd::Found(d) = end else {
        panic!("expected the mail from the junk folder")
    };
    let basis = link_basis(None, None);
    let v = found_json(&d, "own", &basis, &[]);
    assert_eq!(v["folder"], "Junk", "{v}");
    assert_eq!(v["from_junk"], true, "{v}");

    imap.deliver(
        "INBOX",
        verification("Site <no-reply@site.test>", "inbox2@site.test"),
    );
    imap.folders
        .lock()
        .unwrap()
        .get_mut("Junk")
        .unwrap()
        .clear();
    let end = wait_for_mail(
        &scoped,
        Some("own"),
        &filter,
        &patient(),
        std::future::pending(),
    )
    .await
    .unwrap();
    let WaitEnd::Found(d) = end else {
        panic!("expected the inbox mail")
    };
    let v = found_json(&d, "own", &basis, &[]);
    assert_eq!(v["folder"], "INBOX", "{v}");
    assert_eq!(v["from_junk"], false, "{v}");
}

#[tokio::test]
async fn an_unreadable_secret_never_forwards_the_stores_own_words() {
    let store: Arc<dyn secret_store::SecretStore> = Arc::new(secret_store::MemoryStore::new());
    let r = secret_store::SecretRef::new("email/ghost-account/password").unwrap();
    let e = crate::cred::resolve_secret(&store, &r).await.unwrap_err();
    let text = tool_registry::ToolError::from(e).to_string();
    assert!(
        text.contains("the stored secret could not be read"),
        "{text}"
    );
    assert!(
        !text.contains("ghost-account"),
        "the reference must not be echoed: {text}"
    );
    assert!(text.contains("Settings"), "the fix hint stays: {text}");
}
