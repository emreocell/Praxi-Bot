//! `/v1/email/channel*`: owner-only, nothing is sent without an owner approval.

use std::sync::Arc;

use async_trait::async_trait;
use axum::http::{Method, StatusCode};
use parking_lot::Mutex;
use serde_json::json;

use super::{TOKEN, call, send, setup};
use crate::email::EmailControlError;
use crate::email_channel::{
    EmailChannelControl, EmailChannelDto, EmailChannelMode, EmailChannelSettingsDto,
    EmailPendingReplyDto,
};

struct Fake {
    settings: Mutex<EmailChannelSettingsDto>,
    replies: Mutex<Vec<EmailPendingReplyDto>>,
    sent: Mutex<Vec<String>>,
}

#[async_trait]
impl EmailChannelControl for Fake {
    fn get(&self) -> EmailChannelDto {
        EmailChannelDto {
            settings: self.settings.lock().clone(),
            running: false,
            pending_replies: self.replies.lock().len(),
        }
    }
    async fn put(&self, s: EmailChannelSettingsDto) -> Result<EmailChannelDto, EmailControlError> {
        *self.settings.lock() = s;
        Ok(self.get())
    }
    fn replies(&self) -> Vec<EmailPendingReplyDto> {
        self.replies.lock().clone()
    }
    async fn approve(&self, id: &str) -> Result<(), EmailControlError> {
        let mut r = self.replies.lock();
        let at = r
            .iter()
            .position(|p| p.id == id)
            .ok_or_else(|| EmailControlError::NotFound("reply".into()))?;
        self.sent.lock().push(r.remove(at).body);
        Ok(())
    }
    fn deny(&self, id: &str) -> Result<(), EmailControlError> {
        let mut r = self.replies.lock();
        let before = r.len();
        r.retain(|p| p.id != id);
        if r.len() == before {
            return Err(EmailControlError::NotFound("reply".into()));
        }
        Ok(())
    }
}

#[tokio::test]
async fn settings_and_reply_approval_are_owner_only() {
    let t = setup();
    let fake = Arc::new(Fake {
        settings: Mutex::new(EmailChannelSettingsDto {
            enabled: false,
            account: String::new(),
            folder: "INBOX".into(),
            bot: None,
            poll_secs: 120,
            mode: EmailChannelMode::Summary,
            max_per_poll: 10,
            replies_enabled: false,
        }),
        replies: Mutex::new(vec![EmailPendingReplyDto {
            id: "r1".into(),
            to: vec!["ann@example.com".into()],
            subject: "Re: hi".into(),
            body: "See you Tuesday".into(),
            created_ms: 1,
        }]),
        sent: Mutex::new(Vec::new()),
    });
    t.state.set_email_channel(fake.clone());

    let r = call(&t, Method::GET, "/v1/email/channel", None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["settings"]["replies_enabled"], false);
    assert_eq!(r.json()["pending_replies"], 1);

    // A holder of only the API token (a bot, the CLI) can neither change the
    // settings, read the waiting replies, approve nor deny.
    let auth = format!("Bearer {TOKEN}");
    let settings = json!({"enabled": true, "account": "w", "folder": "INBOX", "bot": null,
        "poll_secs": 120, "mode": "full", "max_per_poll": 10, "replies_enabled": true});
    for (m, p, b) in [
        (Method::PUT, "/v1/email/channel", Some(settings.clone())),
        (Method::GET, "/v1/email/channel/replies", None),
        (Method::POST, "/v1/email/channel/replies/r1/approve", None),
        (Method::POST, "/v1/email/channel/replies/r1/deny", None),
    ] {
        let r = send(
            &t.app,
            m,
            p,
            b,
            &[("authorization", auth.as_str()), ("x-test-token-only", "1")],
        )
        .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{p}");
    }
    assert!(fake.sent.lock().is_empty());
    assert!(!fake.settings.lock().replies_enabled);

    // The owner can.
    let r = call(&t, Method::PUT, "/v1/email/channel", Some(settings)).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(fake.settings.lock().replies_enabled);
    let r = call(&t, Method::GET, "/v1/email/channel/replies", None).await;
    assert_eq!(r.json()[0]["body"], "See you Tuesday");
    let r = call(
        &t,
        Method::POST,
        "/v1/email/channel/replies/r1/approve",
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(fake.sent.lock().as_slice(), ["See you Tuesday"]);
    let r = call(&t, Method::POST, "/v1/email/channel/replies/r1/deny", None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}
