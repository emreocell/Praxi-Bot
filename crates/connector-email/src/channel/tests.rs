use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use channel_api::{Channel, ChannelError, ChatRef, IncomingKind, OutgoingMessage};
use secret_store::{MemoryStore, SecretStore};

use super::*;
use crate::config::AccountKind;
use crate::imap_provider::ImapSmtpProvider;
use crate::message::{AttachmentInfo, MessageDetail, MessageSummary};
use crate::provider::{Capabilities, Found};
use crate::smtp::OutgoingMail;
use crate::testutil::{self, Protocol};

struct Fake {
    unread: Mutex<Vec<MessageSummary>>,
    sent: Mutex<Vec<OutgoingMail>>,
    fail_get: bool,
}

fn summary(uid: u32, from: &str, subject: &str) -> MessageSummary {
    MessageSummary {
        uid,
        id: None,
        subject: subject.into(),
        from: from.into(),
        to: vec!["me@example.com".into()],
        date: Some("2026-11-08T12:00:00Z".into()),
        unread: true,
        size: Some(500),
    }
}

impl Fake {
    fn new(unread: Vec<MessageSummary>) -> Arc<Self> {
        Arc::new(Self {
            unread: Mutex::new(unread),
            sent: Mutex::new(Vec::new()),
            fail_get: false,
        })
    }
    fn arrive(&self, m: MessageSummary) {
        self.unread.lock().unwrap().push(m);
    }
}

#[async_trait]
impl MailboxProvider for Fake {
    fn kind(&self) -> AccountKind {
        AccountKind::ImapSmtp
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            create_inbox: false,
            delete_inbox: false,
            folders: true,
            search: true,
            mark_read: false,
            drafts: false,
            list_new: false,
            attachments: false,
            reply: false,
            push: false,
        }
    }
    async fn check(&self) -> Result<(), EmailError> {
        Ok(())
    }
    async fn search(&self, _r: &SearchRequest<'_>) -> Result<Found, EmailError> {
        let mut m = self.unread.lock().unwrap().clone();
        m.sort_by_key(|m| std::cmp::Reverse(m.uid));
        let total = m.len();
        Ok(Found { messages: m, total })
    }
    async fn get_message(
        &self,
        _f: &str,
        id: &str,
        _max: usize,
    ) -> Result<MessageDetail, EmailError> {
        if self.fail_get {
            return Err(EmailError::Imap("boom".into()));
        }
        let uid: u32 = id.parse().unwrap();
        let summary = self
            .unread
            .lock()
            .unwrap()
            .iter()
            .find(|m| m.uid == uid)
            .cloned()
            .unwrap();
        Ok(MessageDetail {
            summary,
            cc: vec![],
            message_id: None,
            body_text: format!("Hello\u{202E}evil\u{200B}\n\n\n\n{}", "x".repeat(2000)),
            body_truncated: false,
            attachments: vec![AttachmentInfo {
                id: None,
                filename: Some("invoice.pdf".into()),
                content_type: "application/pdf".into(),
                size: 2048,
            }],
        })
    }
    async fn send(&self, mail: &OutgoingMail) -> Result<Receipt, EmailError> {
        self.sent.lock().unwrap().push(mail.clone());
        Ok(Receipt {
            id: "250 ok".into(),
            thread_id: None,
        })
    }
}

fn channel(f: &Arc<Fake>, replies: bool, allowed: &[String]) -> EmailChannel {
    let settings = EmailChannelSettings {
        replies_enabled: replies,
        ..EmailChannelSettings::default()
    };
    EmailChannel::new(f.clone(), "me@example.com", allowed, settings, None).unwrap()
}

fn text(m: &IncomingMessage) -> &str {
    match &m.kind {
        IncomingKind::Text(t) => t,
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn only_mail_after_switch_on_arrives_once_as_untrusted_summary() {
    let f = Fake::new(vec![summary(3, "old@example.com", "Old")]);
    let ch = channel(&f, false, &[]);
    assert!(
        ch.poll_once().await.unwrap().is_empty(),
        "old mail is not replayed"
    );
    f.arrive(summary(
        4,
        "Ann <Ann@Example.com>",
        "Hi\u{202E} there\r\nSubject: forged",
    ));
    let got = ch.poll_once().await.unwrap();
    assert_eq!(got.len(), 1);
    let t = text(&got[0]);
    assert!(t.starts_with(UNTRUSTED_BANNER));
    assert_eq!(got[0].sender.id, "ann@example.com");
    assert!(t.contains("Subject: Hi there  Subject: forged"), "{t}");
    assert!(!t.contains('\u{202E}') && !t.contains('\u{200B}'));
    assert!(t.contains("Attachments (not opened): invoice.pdf (2 KB)"));
    assert!(
        t.chars().count() < UNTRUSTED_BANNER.len() + 300 + 200,
        "summary is short"
    );
    assert!(
        ch.poll_once().await.unwrap().is_empty(),
        "no second delivery"
    );
}

#[tokio::test]
async fn filters_floods_and_unreadable_mail() {
    let f = Fake::new(vec![]);
    let ch = channel(&f, false, &["@example.com".to_string()]);
    ch.poll_once().await.unwrap();
    f.arrive(summary(1, "me@example.com", "my own mail"));
    f.arrive(summary(2, "evil@elsewhere.org", "spam"));
    for uid in 10..30 {
        f.arrive(summary(uid, "bob@example.com", "flood"));
    }
    let got = ch.poll_once().await.unwrap();
    assert_eq!(got.len(), 11, "10 mails plus one notice");
    assert!(text(got.last().unwrap()).contains("10 more new mails"));
    assert!(got.iter().all(|m| m.sender.id != "evil@elsewhere.org"));
    assert_eq!(ch.poll_once().await.unwrap().len(), 0);

    let mut bad = Fake::new(vec![]);
    Arc::get_mut(&mut bad).unwrap().fail_get = true;
    let ch = channel(&bad, false, &[]);
    ch.poll_once().await.unwrap();
    bad.arrive(summary(5, "a@example.com", "x"));
    let got = ch.poll_once().await.unwrap();
    assert!(text(&got[0]).contains("could not be read"));
}

#[tokio::test]
async fn replies_are_off_by_default_and_never_sent_without_approval() {
    let f = Fake::new(vec![]);
    let off = channel(&f, false, &[]);
    let err = off
        .send(
            ChatRef("ann@example.com".into()),
            OutgoingMessage::text("hi"),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ChannelError::InvalidInput(_)));

    let ch = channel(&f, true, &[]);
    ch.poll_once().await.unwrap();
    // A stranger the mailbox never heard from is refused.
    assert!(
        ch.send(
            ChatRef("eve@evil.org".into()),
            OutgoingMessage::text("secrets")
        )
        .await
        .is_err()
    );
    f.arrive(summary(8, "Ann <ann@example.com>", "Question"));
    ch.poll_once().await.unwrap();
    let id = ch
        .send(
            ChatRef("ann@example.com".into()),
            OutgoingMessage::text("Subject: Re: Question\nYes, Tuesday works."),
        )
        .await
        .unwrap();
    assert!(f.sent.lock().unwrap().is_empty(), "proposing sends nothing");
    let pending = ch.pending_replies();
    assert_eq!(pending.len(), 1);
    let card = pending[0].card();
    assert!(card.contains("To: ann@example.com") && card.contains("Yes, Tuesday works."));

    ch.approve_reply(&id).await.unwrap();
    let sent = f.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].to, vec!["ann@example.com"]);
    assert_eq!(sent[0].body, "Yes, Tuesday works.");
    assert!(sent[0].cc.is_empty() && sent[0].bcc.is_empty());
    assert!(matches!(
        ch.approve_reply(&id).await,
        Err(EmailError::NotFound(_))
    ));
}

#[test]
fn gate_limits_denies_and_expires() {
    let mut g = ReplyGate::default();
    let all = |_: &str| true;
    let r = |g: &mut ReplyGate, t, id: &str| {
        g.propose(t, id.into(), &["a@example.com".into()], "s", "body", all)
    };
    let first = r(&mut g, 0, "1").unwrap();
    assert!(g.deny(0, &first.id));
    assert!(!g.deny(0, &first.id));
    for i in 0..replies::MAX_PENDING {
        r(&mut g, 0, &format!("p{i}")).unwrap();
    }
    assert!(r(&mut g, 0, "over").is_err(), "queue is bounded");
    assert!(
        g.list(replies::REPLY_TTL_MS + 1).is_empty(),
        "old proposals expire"
    );
    for i in 0..MAX_SENDS_PER_HOUR {
        let id = format!("s{i}");
        r(&mut g, 10, &id).unwrap();
        g.take_for_send(10, &id).unwrap();
    }
    r(&mut g, 10, "late").unwrap();
    assert!(g.take_for_send(11, "late").is_err(), "hourly send limit");
    assert!(g.take_for_send(3_700_000, "late").is_ok(), "limit resets");
    assert!(
        g.propose(0, "x".into(), &["a@b.c, d@e.f".into()], "s", "b", all)
            .is_err(),
        "address lists are refused"
    );
}

#[test]
fn settings_default_to_safe_values_and_reject_bad_ones() {
    let d = EmailChannelSettings::default();
    assert!(!d.enabled && !d.replies_enabled && d.mode == ChannelMode::Summary);
    assert!(d.validate().is_ok());
    for bad in [
        EmailChannelSettings {
            poll_secs: 5,
            ..d.clone()
        },
        EmailChannelSettings {
            enabled: true,
            ..d.clone()
        },
        EmailChannelSettings {
            max_per_poll: 0,
            ..d.clone()
        },
        EmailChannelSettings {
            folder: "IN\r\nBOX".into(),
            ..d.clone()
        },
    ] {
        assert!(bad.validate().is_err(), "{bad:?}");
    }
    assert!(toml::from_str::<EmailChannelSettings>("enabled = true\nunknown = 1").is_err());
}

#[tokio::test]
async fn works_over_a_fake_tls_imap_and_smtp_server() {
    let imap = testutil::server(Protocol::Imap).await;
    let smtp = testutil::server(Protocol::Smtp).await;
    let acct = testutil::account(&imap, &smtp, crate::SmtpSecurity::Implicit);
    let secrets = Arc::new(MemoryStore::new());
    secrets.set("email/test", testutil::PASSWORD).unwrap();
    let provider = Arc::new(ImapSmtpProvider::new(acct, secrets));
    // The fake mailbox holds UIDs 7 and 42: pretend 7 was already seen.
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state.json");
    std::fs::write(&state, r#"{"last_uid":7,"folder":"INBOX","known":[]}"#).unwrap();
    let settings = EmailChannelSettings {
        replies_enabled: true,
        ..EmailChannelSettings::default()
    };
    let ch = EmailChannel::new(
        provider,
        "me@example.com",
        &[],
        settings,
        Some(state.clone()),
    )
    .unwrap();
    let got = ch.poll_once().await.unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].event_id, "email:INBOX:42");
    assert!(text(&got[0]).contains("Hello from fixture"));
    assert!(text(&got[0]).contains("a.pdf"));
    assert!(
        std::fs::read_to_string(&state)
            .unwrap()
            .contains("\"last_uid\":42")
    );
    // Nothing was marked read and no body literal beyond PEEK was requested.
    assert!(
        !imap
            .commands
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.contains("STORE"))
    );

    let id = ch
        .send(
            ChatRef("ann@example.com".into()),
            OutgoingMessage::text("Thanks!"),
        )
        .await
        .unwrap();
    assert!(smtp.messages.lock().unwrap().is_empty());
    ch.approve_reply(&id).await.unwrap();
    let wire = String::from_utf8(smtp.messages.lock().unwrap()[0].clone()).unwrap();
    assert!(wire.contains("ann@example.com") && wire.contains("Thanks!"));
}
