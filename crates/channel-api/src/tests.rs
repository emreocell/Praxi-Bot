use std::sync::Mutex;
use std::time::Instant;

use async_trait::async_trait;
use futures_core::stream::BoxStream;
use futures_util::{StreamExt, stream};

use crate::approval::approval_row;
use crate::pairing::{MAX_PAIR_ATTEMPTS, PAIR_CODE_TTL, PairingInvites, PairingMatch};
use crate::{
    Channel, ChannelCaps, ChatRef, IncomingKind, IncomingMessage, InlineButton, OutgoingMessage,
    Result, SenderIdentity,
};

#[derive(Default)]
struct FakeChannel {
    incoming: Mutex<Vec<IncomingMessage>>,
    sent: Mutex<Vec<(ChatRef, OutgoingMessage)>>,
    callbacks: Mutex<Vec<String>>,
}

#[async_trait]
impl Channel for FakeChannel {
    async fn receive(&self) -> Result<BoxStream<'static, IncomingMessage>> {
        Ok(Box::pin(stream::iter(std::mem::take(
            &mut *self.incoming.lock().unwrap(),
        ))))
    }

    async fn send(&self, to: ChatRef, msg: OutgoingMessage) -> Result<String> {
        let id = msg
            .edit_message_id
            .clone()
            .unwrap_or_else(|| "message:opaque".into());
        self.sent.lock().unwrap().push((to, msg));
        Ok(id)
    }

    async fn answer_callback(&self, query_id: &str, _text: Option<&str>) -> Result<()> {
        self.callbacks.lock().unwrap().push(query_id.into());
        Ok(())
    }

    fn capabilities(&self) -> ChannelCaps {
        ChannelCaps {
            inline_buttons: true,
            message_editing: true,
            file_upload: false,
            voice_messages: false,
        }
    }
}

fn event(id: &str, kind: IncomingKind) -> IncomingMessage {
    IncomingMessage {
        event_id: id.into(),
        chat: ChatRef("room:opaque".into()),
        sender: SenderIdentity {
            id: "user:opaque".into(),
            display_name: Some("Owner".into()),
        },
        message_id: Some("message:opaque".into()),
        forwarded: false,
        edited: false,
        kind,
    }
}

#[tokio::test]
async fn fake_channel_runs_transport_conformance() {
    let expected = vec![
        event("event:text", IncomingKind::Text("hello".into())),
        event(
            "event:attachment",
            IncomingKind::Unsupported("voice".into()),
        ),
        event(
            "event:callback",
            IncomingKind::Callback {
                query_id: "callback:opaque".into(),
                data: "once".into(),
                message_id: Some("message:opaque".into()),
            },
        ),
    ];
    let channel = FakeChannel {
        incoming: Mutex::new(expected.clone()),
        ..FakeChannel::default()
    };
    crate::conformance::assert_transport(
        &channel,
        &expected,
        expected[0].chat.clone(),
        "callback:opaque",
    )
    .await
    .unwrap();
    let sent = channel.sent.lock().unwrap();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0].1.buttons[0][0].callback_data, "once");
    assert_eq!(sent[1].1.edit_message_id.as_deref(), Some("message:opaque"));
    assert_eq!(*channel.callbacks.lock().unwrap(), ["callback:opaque"]);
}

#[tokio::test]
async fn fake_channel_drives_pairing_and_detailed_approval_policy() {
    let now = Instant::now();
    let mut invites = PairingInvites::default();
    invites.create("123456".into(), PAIR_CODE_TTL, now);
    let pair = event("pair", IncomingKind::Text("/pair 123456".into()));
    let callback = event(
        "approve",
        IncomingKind::Callback {
            query_id: "q".into(),
            data: "once".into(),
            message_id: None,
        },
    );
    let channel = FakeChannel {
        incoming: Mutex::new(vec![pair, callback]),
        ..FakeChannel::default()
    };
    let mut incoming = channel.receive().await.unwrap();
    let pair = incoming.next().await.unwrap();
    let IncomingKind::Text(text) = pair.kind else {
        panic!("expected pairing command")
    };
    let PairingMatch::Valid(ticket) = invites.check(
        pair.sender.id.clone(),
        text.strip_prefix("/pair ").unwrap(),
        now,
    ) else {
        panic!("valid invite")
    };
    // The adapter persists this identity before consuming its ticket.
    let paired = pair.sender.id;
    invites.consume(&paired, ticket);
    assert!(matches!(
        invites.check("another-user".into(), "123456", now),
        PairingMatch::InvalidCode
    ));
    let row = approval_row(
        true,
        false,
        InlineButton::new("Approve once", "once"),
        Some(InlineButton::new("This task", "run")),
        InlineButton::new("Always", "always"),
        InlineButton::new("Deny", "deny"),
    );
    channel
        .send(
            pair.chat,
            OutgoingMessage::text("Full details").with_buttons(vec![row]),
        )
        .await
        .unwrap();
    let callback = incoming.next().await.unwrap();
    assert_eq!(callback.sender.id, paired);
    let IncomingKind::Callback { query_id, data, .. } = callback.kind else {
        panic!("callback")
    };
    assert_eq!(data, "once");
    assert_eq!(
        channel.sent.lock().unwrap()[0].1.buttons[0]
            .iter()
            .map(|b| b.callback_data.as_str())
            .collect::<Vec<_>>(),
        ["once", "deny"]
    );
    channel
        .answer_callback(&query_id, Some("Approved"))
        .await
        .unwrap();
}

#[test]
fn pairing_keeps_global_guess_budget_and_sender_lockout() {
    let now = Instant::now();
    let mut invites = PairingInvites::default();
    invites.create("123456".into(), PAIR_CODE_TTL, now);
    for user in 0..MAX_PAIR_ATTEMPTS {
        assert!(matches!(
            invites.check(user, "wrong", now),
            PairingMatch::InvalidCode
        ));
    }
    assert!(matches!(
        invites.check(99, "123456", now),
        PairingMatch::InvalidCode
    ));
    for _ in 0..MAX_PAIR_ATTEMPTS {
        let _ = invites.check(42, "wrong", now);
    }
    invites.create("654321".into(), PAIR_CODE_TTL, now);
    assert!(matches!(
        invites.check(42, "654321", now),
        PairingMatch::RateLimited
    ));
    assert!(matches!(
        invites.check(43, "654321", now),
        PairingMatch::Valid(_)
    ));
}
