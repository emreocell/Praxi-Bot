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
    validity: Mutex<Option<u32>>,
}

fn summary(uid: u32, from: &str, subject: &str) -> MessageSummary {
    MessageSummary {
        uid,
        id: None,
        subject: subject.into(),
        from: from.into(),
        from_addr: address_of(from),
        to: vec!["me@example.com".into()],
        date: Some("2026-11-08T12:00:00Z".into()),
        unread: true,
        size: Some(500),
        automation_headers: None,
        has_attachment: None,
        folder: None,
        from_junk: None,
    }
}

impl Fake {
    fn new(unread: Vec<MessageSummary>) -> Arc<Self> {
        Arc::new(Self {
            unread: Mutex::new(unread),
            sent: Mutex::new(Vec::new()),
            fail_get: false,
            validity: Mutex::new(Some(7)),
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
        Ok(Found {
            messages: m,
            total,
            uid_validity: *self.validity.lock().unwrap(),
            next_page_token: None,
        })
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
            links: Vec::new(),
            reply_to: None,
            references: Vec::new(),
            recipients: Vec::new(),
            found_in: None,
            from_junk: None,
            sender_authenticated: None,
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

/// An empty `allowed` list stands for "the whole example.com domain" here: the
/// channel itself delivers nothing for an empty list (see
/// `an_empty_allowed_senders_list_delivers_nothing`).
fn channel(f: &Arc<Fake>, replies: bool, allowed: &[String]) -> EmailChannel {
    let settings = EmailChannelSettings {
        replies_enabled: replies,
        ..EmailChannelSettings::default()
    };
    let allowed = if allowed.is_empty() {
        vec!["@example.com".to_string()]
    } else {
        allowed.to_vec()
    };
    EmailChannel::new(f.clone(), "me@example.com", &allowed, settings, None).unwrap()
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
    assert_eq!(ch.poll_once().await.unwrap().len(), 0);
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
    assert_eq!(ch.poll_once().await.unwrap().len(), 0);
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
    // The 10 mails over the limit were not committed: they come next, in order.
    let next = ch.poll_once().await.unwrap();
    assert_eq!(next.len(), 10, "the rest, and no notice any more");
    assert_eq!(next[0].message_id.as_deref(), Some("20"));
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

fn draft_only_account() -> crate::AccountConfig {
    let mut a: crate::AccountConfig = toml::from_str(
        r#"
name = "main"
address = "me@example.com"
password = "email/main"
[imap]
host = "imap.example.com"
[smtp]
host = "smtp.example.com"
[identity]
send_mode = "draft_only"
"#,
    )
    .unwrap();
    a.allowed_senders = vec!["@example.com".to_string()];
    a
}

#[tokio::test]
async fn a_draft_only_account_never_queues_or_sends_a_channel_reply() {
    let f = Fake::new(vec![]);
    let settings = EmailChannelSettings {
        replies_enabled: true,
        ..EmailChannelSettings::default()
    };
    let ch = EmailChannel::for_account(f.clone(), &draft_only_account(), settings, None).unwrap();
    ch.poll_once().await.unwrap();
    f.arrive(summary(8, "Ann <ann@example.com>", "Question"));
    ch.poll_once().await.unwrap();
    // The model proposing a reply is told why it is refused.
    let err = ch
        .send(
            ChatRef("ann@example.com".into()),
            OutgoingMessage::text("hi"),
        )
        .await
        .unwrap_err();
    assert!(matches!(&err, ChannelError::InvalidInput(m) if m.contains("draft only")));
    // Even a reply that got into the queue some other way is not sent.
    let id = ch
        .core
        .gate
        .lock()
        .unwrap()
        .propose(
            now_ms(),
            "r1".into(),
            &["ann@example.com".into()],
            "s",
            "body",
            |_| true,
        )
        .unwrap()
        .id;
    assert!(matches!(
        ch.approve_reply(&id).await,
        Err(EmailError::Input(_))
    ));
    assert!(f.sent.lock().unwrap().is_empty(), "nothing was sent");
}

#[tokio::test]
async fn the_reply_card_names_the_identity_and_its_mode() {
    let f = Fake::new(vec![]);
    let mut a = draft_only_account();
    a.identity.send_mode = crate::SendMode::Ask;
    let settings = EmailChannelSettings {
        replies_enabled: true,
        ..EmailChannelSettings::default()
    };
    let ch = EmailChannel::for_account(f.clone(), &a, settings, None).unwrap();
    ch.poll_once().await.unwrap();
    f.arrive(summary(8, "Ann <ann@example.com>", "Question"));
    ch.poll_once().await.unwrap();
    ch.send(
        ChatRef("ann@example.com".into()),
        OutgoingMessage::text("hi"),
    )
    .await
    .unwrap();
    let card = ch.pending_replies()[0].card();
    assert!(
        card.contains("From: me@example.com (account: main) (send mode: ask)"),
        "{card}"
    );
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
        &["ann@example.com".to_string()],
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

#[tokio::test]
async fn a_mail_that_could_not_be_handed_over_is_offered_again() {
    let f = Fake::new(vec![summary(3, "old@example.com", "Old")]);
    let ch = channel(&f, false, &[]);
    assert_eq!(ch.poll_pending().await.unwrap().len(), 0);
    f.arrive(summary(4, "a@example.com", "first"));
    f.arrive(summary(5, "b@example.com", "second"));
    // Delivery of both failed: nothing is committed, so nothing is lost.
    assert_eq!(ch.poll_pending().await.unwrap().len(), 2);
    let again = ch.poll_pending().await.unwrap();
    assert_eq!(again.len(), 2, "still pending after a failed hand-over");
    // The first was handed over, the second not: only the second comes back.
    ch.commit(uid_of(&again[0]).unwrap());
    let rest = ch.poll_pending().await.unwrap();
    assert_eq!(rest.len(), 1);
    assert_eq!(rest[0].sender.id, "b@example.com");
    ch.commit(uid_of(&rest[0]).unwrap());
    assert_eq!(ch.poll_pending().await.unwrap().len(), 0);
}

#[tokio::test]
async fn changed_uidvalidity_resets_the_position_without_replaying_mail() {
    let f = Fake::new(vec![summary(90, "old@example.com", "Old")]);
    let ch = channel(&f, false, &[]);
    assert_eq!(ch.poll_once().await.unwrap().len(), 0);
    f.arrive(summary(91, "a@example.com", "new"));
    assert_eq!(ch.poll_once().await.unwrap().len(), 1);
    // The server renumbered the folder: small UIDs now, all of them old mail.
    *f.validity.lock().unwrap() = Some(8);
    f.unread.lock().unwrap().clear();
    f.arrive(summary(1, "x@example.com", "renumbered"));
    f.arrive(summary(2, "y@example.com", "renumbered"));
    assert_eq!(ch.poll_once().await.unwrap().len(), 0);
    // ...and mail that arrives afterwards is delivered although its UID is
    // far below the old position.
    f.arrive(summary(3, "z@example.com", "really new"));
    let got = ch.poll_once().await.unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].sender.id, "z@example.com");
}

#[tokio::test]
async fn an_approved_reply_goes_through_the_guard_and_the_send_log() {
    let f = Fake::new(vec![]);
    let ch = channel(&f, true, &[]);
    let guard = Arc::new(MailGuard::in_memory());
    ch.attach_guard(guard.clone(), "work");
    ch.poll_once().await.unwrap();
    for (uid, who) in [(8, "Ann <ann@example.com>"), (9, "Bob <bob@example.com>")] {
        f.arrive(summary(uid, who, "Question"));
    }
    ch.poll_once().await.unwrap();
    let propose = |to: &str| {
        let ch = ch.clone();
        let to = to.to_owned();
        async move {
            ch.send(ChatRef(to), OutgoingMessage::text("Subject: Re: Q\nYes."))
                .await
                .unwrap()
        }
    };
    // Ann asked not to be written to (spelled differently): the approval is refused, nothing goes
    // out and the reply keeps waiting.
    guard.add_opt_out("ANN+x@example.com", "").unwrap();
    let ann = propose("ann@example.com").await;
    let e = ch.approve_reply(&ann).await.unwrap_err();
    assert!(
        matches!(e, EmailError::Input(ref m) if m.contains("Not sent")),
        "{e:?}"
    );
    assert!(f.sent.lock().unwrap().is_empty());
    assert_eq!(ch.pending_replies().len(), 1);
    // Bob is fine: it is sent and logged.
    let bob = propose("bob@example.com").await;
    ch.approve_reply(&bob).await.unwrap();
    assert_eq!(f.sent.lock().unwrap().len(), 1);
    let log = guard.log(10);
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].recipients, ["bob@example.com"]);
    assert_eq!(log[0].run, "email-channel-reply");
}

#[tokio::test]
async fn an_empty_allowed_senders_list_delivers_nothing() {
    let f = Fake::new(vec![summary(3, "old@example.com", "Old")]);
    // No allowed sender named: the default must never be "anyone".
    let ch = EmailChannel::new(
        f.clone(),
        "me@example.com",
        &[],
        EmailChannelSettings::default(),
        None,
    )
    .unwrap();
    assert_eq!(ch.poll_pending().await.unwrap().len(), 0);
    f.arrive(summary(4, "stranger@evil.org", "Hello"));
    f.arrive(summary(5, "Ann <ann@example.com>", "Hi"));
    assert_eq!(ch.poll_pending().await.unwrap().len(), 0);
    assert_eq!(ch.ignored_count(), 2, "ignored and counted, not delivered");
    // The ignored mails are not offered again, and not counted twice.
    assert_eq!(ch.poll_pending().await.unwrap().len(), 0);
    assert_eq!(ch.ignored_count(), 2);

    // With a named sender only that sender gets through.
    let ch = channel(&f, false, &["ann@example.com".to_string()]);
    assert_eq!(ch.poll_pending().await.unwrap().len(), 0);
    f.arrive(summary(6, "stranger@evil.org", "Hello again"));
    f.arrive(summary(7, "Ann <ann@example.com>", "Hi again"));
    let got = ch.poll_pending().await.unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].sender.id, "ann@example.com");
    assert_eq!(ch.ignored_count(), 1);
}

#[tokio::test]
async fn a_mail_that_keeps_failing_is_skipped_after_three_attempts() {
    let f = Fake::new(vec![summary(3, "old@example.com", "Old")]);
    let ch = channel(&f, false, &[]);
    assert_eq!(ch.poll_pending().await.unwrap().len(), 0);
    f.arrive(summary(4, "a@example.com", "poison"));
    f.arrive(summary(5, "b@example.com", "fine"));
    for attempt in 1..MAX_DELIVERY_ATTEMPTS {
        let batch = ch.poll_pending().await.unwrap();
        assert_eq!(batch.len(), 2, "attempt {attempt}: still blocking");
        assert_eq!(
            ch.delivery_failed(uid_of(&batch[0]).unwrap()),
            DeliveryOutcome::RetryLater
        );
    }
    let batch = ch.poll_pending().await.unwrap();
    assert_eq!(batch.len(), 2);
    assert_eq!(
        ch.delivery_failed(uid_of(&batch[0]).unwrap()),
        DeliveryOutcome::Skipped
    );
    assert_eq!(ch.skipped_count(), 1);
    // The poison mail is gone from the queue; the mail behind it flows.
    let rest = ch.poll_pending().await.unwrap();
    assert_eq!(rest.len(), 1);
    assert_eq!(rest[0].sender.id, "b@example.com");
    ch.commit(uid_of(&rest[0]).unwrap());
    assert_eq!(ch.poll_pending().await.unwrap().len(), 0);
}

#[tokio::test]
async fn mails_over_the_poll_limit_stay_unread_for_the_next_poll() {
    let f = Fake::new(vec![]);
    let settings = EmailChannelSettings {
        max_per_poll: 2,
        ..EmailChannelSettings::default()
    };
    let ch = EmailChannel::new(
        f.clone(),
        "me@example.com",
        &["@example.com".to_string()],
        settings,
        None,
    )
    .unwrap();
    ch.poll_pending().await.unwrap();
    for uid in 1..=5 {
        f.arrive(summary(uid, "a@example.com", &format!("mail {uid}")));
    }
    let first = ch.poll_pending().await.unwrap();
    assert_eq!(first.len(), 3, "2 mails and the notice");
    assert!(text(&first[2]).contains("3 more new mails"));
    // Delivering everything the host got (the notice included) must not commit the rest.
    for m in &first {
        ch.commit(uid_of(m).unwrap());
    }
    let second = ch.poll_pending().await.unwrap();
    assert_eq!(second.len(), 3, "mails 3 and 4, and a notice for mail 5");
    assert_eq!(second[0].message_id.as_deref(), Some("3"));
    for m in &second {
        ch.commit(uid_of(m).unwrap());
    }
    let third = ch.poll_pending().await.unwrap();
    assert_eq!(third.len(), 1);
    assert_eq!(third[0].message_id.as_deref(), Some("5"));
}

fn parsed(uid: u32, from_header: &str) -> MessageSummary {
    let raw = format!(
        "From: {from_header}

To: me@example.com

Subject: x



body

"
    );
    crate::message::parse_summary(uid, raw.as_bytes(), true, Some(500))
}

#[tokio::test]
async fn a_long_display_name_cannot_hide_the_real_sender() {
    let f = Fake::new(vec![]);
    let ch = EmailChannel::new(
        f.clone(),
        "me@example.com",
        &["colleague@corp.example".to_string()],
        EmailChannelSettings::default(),
        None,
    )
    .unwrap();
    ch.poll_pending().await.unwrap();
    // The real sender is stranger@evil.org; the display name is built so that the
    // 256-character cap cuts the real address off and leaves the colleague's.
    let pad = "x".repeat(300);
    f.arrive(parsed(
        4,
        &format!("\"<colleague@corp.example>{pad}\" <stranger@evil.org>"),
    ));
    // Two From addresses, the first allowed: not one verifiable sender.
    f.arrive(parsed(5, "colleague@corp.example, stranger@evil.org"));
    // A quoted / escaped angle bracket inside the name.
    f.arrive(parsed(
        6,
        "\"a\\> <colleague@corp.example>\" <stranger@evil.org>",
    ));
    assert_eq!(f.unread.lock().unwrap()[0].from_addr, "stranger@evil.org");
    assert_eq!(ch.poll_pending().await.unwrap().len(), 0);
    assert_eq!(ch.ignored_count(), 3);
    // A normal "Name <addr>" and a bare address still work.
    f.arrive(parsed(7, "Colleague <colleague@corp.example>"));
    f.arrive(parsed(8, "colleague@corp.example"));
    let got = ch.poll_pending().await.unwrap();
    assert_eq!(got.len(), 2);
    assert!(got.iter().all(|m| m.sender.id == "colleague@corp.example"));
}

#[test]
fn address_of_fails_closed() {
    assert_eq!(address_of("Ann <Ann@Example.com>"), "ann@example.com");
    assert_eq!(address_of("ann@example.com"), "ann@example.com");
    assert_eq!(address_of("<ann@example.com>"), "ann@example.com");
    assert_eq!(address_of("Ann <ann@example.com> trailing"), "");
    assert_eq!(address_of("x>y <a@b.c"), "");
    assert_eq!(address_of("a@b.c <d@e.f>"), "d@e.f");
    assert_eq!(address_of("Name <a b@c.d>"), "");
}

#[tokio::test]
async fn changing_the_allowed_senders_applies_to_a_running_channel() {
    let f = Fake::new(vec![]);
    let ch = channel(&f, false, &["ann@example.com".to_string()]);
    ch.poll_pending().await.unwrap();
    f.arrive(summary(1, "ann@example.com", "one"));
    assert_eq!(ch.poll_pending().await.unwrap().len(), 1);
    ch.commit(1);
    // The owner empties the list: the next mail from the old sender is ignored.
    ch.set_allowed_senders(&[]);
    f.arrive(summary(2, "ann@example.com", "two"));
    assert_eq!(ch.poll_pending().await.unwrap().len(), 0);
    assert_eq!(ch.ignored_count(), 1);
    ch.set_allowed_senders(&["ann@example.com".to_string()]);
    f.arrive(summary(3, "ann@example.com", "three"));
    assert_eq!(ch.poll_pending().await.unwrap().len(), 1);
    // A bad entry closes the channel instead of opening it.
    ch.set_allowed_senders(&["not an address".to_string()]);
    f.arrive(summary(4, "ann@example.com", "four"));
    assert_eq!(ch.poll_pending().await.unwrap().len(), 0);
}

#[tokio::test]
async fn a_domain_rule_covers_subdomains_but_not_lookalikes() {
    let f = Fake::new(vec![]);
    // A bare domain is accepted and means the same as `@corp.com`.
    let ch = channel(&f, false, &["corp.com".to_string()]);
    ch.poll_pending().await.unwrap();
    f.arrive(summary(1, "A <a@corp.com>", "one"));
    f.arrive(summary(2, "B <b@mail.corp.com>", "two"));
    f.arrive(summary(3, "C <c@corp.com.evil.test>", "three"));
    f.arrive(summary(4, "D <d@evilcorp.com>", "four"));
    let got = ch.poll_pending().await.unwrap();
    assert_eq!(got.len(), 2);
    assert_eq!(ch.ignored_count(), 2);
}
