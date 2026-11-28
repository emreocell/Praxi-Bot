//! The wake loop's look into the bot's own inbox, over an `AgentMail` mock server: a reply is
//! matched to the mail the bot sent (its thread), and acted on only then.

use std::sync::Arc;

use builtin_tools::connector_email::{
    AccountConfig, AccountKind, AgentMailEndpoint, EmailConnector, IdentitySettings, ImapSettings,
    MailHandle, SendLogEntry, SmtpSettings,
};
use missions::wake::{MailCheck, MailFilter, MailProbe};
use secret_store::{MemoryStore, SecretRef, SecretStore};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::mission_wakes::OwnInboxProbe;

const BOT: &str = "11111111-1111-4111-8111-111111111111";
const INBOX: &str = "bot@agentmail.to";
const INBOX_ENC: &str = "bot%40agentmail.to";

fn account() -> AccountConfig {
    AccountConfig {
        name: "bot-mail".into(),
        address: INBOX.into(),
        username: String::new(),
        password: SecretRef::new("email/bot-mail/api_key").unwrap(),
        imap: ImapSettings::default(),
        smtp: SmtpSettings::default(),
        drafts_folder: "Drafts".into(),
        extra_root_pem: None,
        timeout_secs: 30,
        bot: Some(BOT.into()),
        allowed_senders: Vec::new(),
        identity: IdentitySettings::default(),
        kind: AccountKind::AgentMail,
        inbox_id: Some(INBOX.into()),
    }
}

fn reply(id: &str, from: &str, thread: &str, subject: &str, text: &str) -> Value {
    json!({
        "inbox_id": INBOX, "thread_id": thread, "message_id": id, "labels": ["received", "unread"],
        "timestamp": chrono::Utc::now().to_rfc3339(), "from": from, "to": [INBOX],
        "size": 10, "subject": subject, "text": text,
    })
}

async fn rig(msg: Value) -> (MockServer, Arc<EmailConnector>, OwnInboxProbe) {
    let server = MockServer::start().await;
    let id = msg["message_id"].as_str().unwrap().to_owned();
    Mock::given(method("GET"))
        .and(path(format!("/v0/inboxes/{INBOX_ENC}/messages")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"count": 1, "messages": [msg.clone()]})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/v0/inboxes/{INBOX_ENC}/messages/{id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(msg))
        .mount(&server)
        .await;
    let store = Arc::new(MemoryStore::new());
    store
        .set("email/bot-mail/api_key", "am_probe_test_KEY_77aa")
        .unwrap();
    let conn = EmailConnector::reloadable_with(
        store,
        AgentMailEndpoint::loopback_mock_for_tests(&server.uri()).unwrap(),
    );
    conn.replace_accounts(vec![account()]).unwrap();
    let conn = Arc::new(conn);
    // The bot sent a mail to ann in thread th-1, from mission m-1.
    conn.guard().record(SendLogEntry {
        at: builtin_tools::connector_email::guard::now_secs(),
        at_utc: String::new(),
        bot: Some(BOT.into()),
        run: "run".into(),
        account: "bot-mail".into(),
        from: INBOX.into(),
        recipients: vec!["ann@example.org".into()],
        subject: "Hello".into(),
        message_id: "api-sent-1".into(),
        thread_id: Some("th-1".into()),
        mission_id: Some("m-1".into()),
        opt_out_line: true,
        overridden: Vec::new(),
    });
    let handle = Arc::new(MailHandle::new());
    handle.set(Arc::clone(&conn));
    (server, conn, OwnInboxProbe::new(handle))
}

async fn look(probe: &OwnInboxProbe, mission: &str) -> Option<missions::wake::MailHit> {
    let filter = MailFilter::new(Some("example.org"), None, None, None).unwrap();
    let now_ms = chrono::Utc::now().timestamp_millis();
    probe
        .check(&MailCheck {
            mission_id: mission,
            bot_id: BOT,
            filter: &filter,
            registered_at_ms: now_ms - 60_000,
            not_before_ms: 0,
            now_ms,
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn an_out_of_office_reply_in_the_sent_thread_asks_for_a_follow_up_in_three_days() {
    let msg = reply(
        "in-1",
        "Ann <ann@example.org>",
        "th-1",
        "Automatic reply: Hello",
        "I am out of the office. I will answer when I am back.",
    );
    let (_server, conn, probe) = rig(msg).await;
    let now_ms = chrono::Utc::now().timestamp_millis();
    let hit = look(&probe, "m-1").await.expect("a hit");
    let at = hit.follow_up_at_ms.expect("a follow-up date");
    let three_days = 3 * 24 * 3600 * 1000;
    assert!((at - now_ms - three_days).abs() < 60_000, "{at} {now_ms}");
    assert_eq!(hit.summary["inbound"]["actions"][0]["stated"], false);
    assert_eq!(conn.guard().opt_outs(), Vec::new());
    // Another mission's wait is not put off by a notice about this mission's mail.
    assert_eq!(look(&probe, "m-2").await.unwrap().follow_up_at_ms, None);
}

#[tokio::test]
async fn an_out_of_office_reply_to_a_mail_of_no_mission_never_puts_a_wait_off() {
    let msg = reply(
        "in-ooo-2",
        "Ann <ann@example.org>",
        "th-1",
        "Automatic reply: Hello",
        "I am out of the office.",
    );
    let (_server, conn, probe) = rig(msg).await;
    // The newest mail in that thread was sent outside any mission (a chat, a schedule).
    conn.guard().record(SendLogEntry {
        at: builtin_tools::connector_email::guard::now_secs(),
        at_utc: String::new(),
        bot: Some(BOT.into()),
        run: "run-chat".into(),
        account: "bot-mail".into(),
        from: INBOX.into(),
        recipients: vec!["ann@example.org".into()],
        subject: "Hello again".into(),
        message_id: "api-sent-2".into(),
        thread_id: Some("th-1".into()),
        mission_id: None,
        opt_out_line: true,
        overridden: Vec::new(),
    });
    let hit = look(&probe, "m-1").await.expect("a hit");
    assert_eq!(hit.summary["inbound"]["mission_id"], Value::Null);
    assert_eq!(hit.follow_up_at_ms, None);
    // The id of the mail itself travels with the hit, so the same notice counts once.
    assert_eq!(hit.message_id.as_deref(), Some("in-ooo-2"));
}

#[tokio::test]
async fn a_stop_reply_in_the_sent_thread_suppresses_the_sender_and_still_wakes_the_mission() {
    let msg = reply(
        "in-2",
        "ann@example.org",
        "th-1",
        "Re: Hello",
        "STOP\n\n> Hello",
    );
    let (_server, conn, probe) = rig(msg).await;
    let hit = look(&probe, "m-1").await.expect("a hit");
    assert_eq!(hit.summary["inbound"]["actions"][0]["action"], "suppressed");
    assert_eq!(hit.summary["inbound"]["actions"][0]["reason"], "stop_reply");
    assert_eq!(hit.summary["inbound"]["mission_id"], "m-1");
    let list = conn.guard().opt_outs();
    assert_eq!(list[0].value, "ann@example.org");
    assert_eq!(list[0].reason, "stop_reply");
    assert_eq!(list[0].message_id, "api-sent-1");
    // The wake itself is not suppressed: the bot hears about it.
    assert_eq!(hit.follow_up_at_ms, None);
}

#[tokio::test]
async fn a_stop_from_a_thread_we_never_wrote_in_changes_nothing() {
    let msg = reply(
        "in-3",
        "ann@example.org",
        "other-thread",
        "Re: Hello",
        "STOP",
    );
    let (_server, conn, probe) = rig(msg).await;
    let hit = look(&probe, "m-1").await.expect("a hit");
    assert_eq!(
        hit.summary["inbound"]["actions"][0]["why"],
        "no_matching_send"
    );
    assert_eq!(conn.guard().opt_outs(), Vec::new());
}
