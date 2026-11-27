//! The [`Channel`] implementation: gateway in, REST out. Every outgoing text is
//! redacted first, then mention-escaped, then split.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use channel_api::{
    Channel, ChannelCaps, ChannelError, ChatRef, IncomingMessage, InlineButton, OutgoingMessage,
};
use futures::stream::BoxStream;
use secret_store::{Redactor, SecretValue};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::error::DiscordError;
use crate::gateway::{self, GatewayHealth, parse_chat};
use crate::rest::DiscordRest;
use crate::text::{MAX_MESSAGE_CHARS, clamp_chars, neutralize_mentions, split_message};

/// Buttons Discord allows: 5 rows of 5.
const MAX_ROWS: usize = 5;
const MAX_PER_ROW: usize = 5;

/// Gateway + REST for one bot token.
pub struct DiscordTransport {
    rest: Arc<DiscordRest>,
    gateway_url: String,
    token: SecretValue,
    redactor: Arc<Redactor>,
    backoff: Duration,
    health: Arc<GatewayHealth>,
    cancel: CancellationToken,
    stream_secrets: Vec<String>,
}

impl DiscordTransport {
    /// Build a transport; nothing connects until [`Channel::receive`] is called.
    #[must_use]
    #[allow(clippy::needless_pass_by_value)]
    pub fn new(
        rest: Arc<DiscordRest>,
        gateway_url: String,
        token: SecretValue,
        redactor: Arc<Redactor>,
        backoff: Duration,
        cancel: CancellationToken,
    ) -> Self {
        let mut redactor = (*redactor).clone();
        for secret in secret_store::secret_variants(token.expose()) {
            redactor.add_secret(&secret);
        }
        Self {
            rest,
            gateway_url,
            token: token.clone(),
            redactor: Arc::new(redactor),
            backoff,
            health: Arc::new(GatewayHealth::default()),
            cancel,
            stream_secrets: vec![token.expose().to_owned()],
        }
    }

    /// Register known secret renderings when withholding incomplete streamed text.
    #[must_use]
    pub fn with_stream_secrets(mut self, secrets: Vec<String>) -> Self {
        self.stream_secrets.extend(secrets);
        self
    }

    /// Hold the unfinished word and any incomplete known secret, including
    /// secrets with spaces. Redact the resulting prefix before splitting it.
    #[must_use]
    pub fn stream_prefix<'a>(&self, text: &'a str) -> &'a str {
        let mut cut = text
            .char_indices()
            .rev()
            .find(|(_, c)| c.is_whitespace())
            .map_or(0, |(i, c)| i + c.len_utf8());
        for secret in &self.stream_secrets {
            let start = text.len().saturating_sub(secret.len());
            for (i, _) in text.char_indices().filter(|(i, _)| *i >= start) {
                let tail = &text[i..];
                if tail.len() < secret.len() && secret.starts_with(tail) {
                    cut = cut.min(i);
                    break;
                }
            }
        }
        &text[..cut]
    }

    /// Gateway connection state.
    #[must_use]
    pub fn health(&self) -> &Arc<GatewayHealth> {
        &self.health
    }

    /// The REST client (connection test, tests).
    #[must_use]
    pub fn rest(&self) -> &Arc<DiscordRest> {
        &self.rest
    }

    /// Redact secrets, then break mentions. Order matters: a secret must be
    /// removed before the text is cut or decorated.
    #[must_use]
    pub fn clean(&self, text: &str) -> String {
        neutralize_mentions(&self.redactor.redact(text))
    }

    /// Redact only (button labels, callback answers).
    #[must_use]
    pub fn redact(&self, text: &str) -> String {
        self.redactor.redact(text)
    }
}

fn components(buttons: &[Vec<InlineButton>], redactor: &Redactor) -> Option<Value> {
    if buttons.is_empty() {
        return None;
    }
    let rows: Vec<Value> = buttons
        .iter()
        .take(MAX_ROWS)
        .map(|row| {
            let items: Vec<Value> = row
                .iter()
                .take(MAX_PER_ROW)
                .map(|b| {
                    let style = if b.callback_data.starts_with("approve:once:") {
                        3
                    } else if b.callback_data.starts_with("approve:deny:") {
                        4
                    } else {
                        2
                    };
                    json!({
                        "type": 2,
                        "style": style,
                        "label": clamp_chars(&redactor.redact(&b.text), 80),
                        "custom_id": b.callback_data.chars().take(100).collect::<String>(),
                    })
                })
                .collect();
            json!({"type": 1, "components": items})
        })
        .collect();
    Some(Value::Array(rows))
}

impl DiscordTransport {
    /// The private chat with `user_id` (opened on first use).
    ///
    /// # Errors
    /// Invalid user id, network or API errors.
    pub async fn open_dm(&self, user_id: &str) -> crate::error::Result<ChatRef> {
        self.rest
            .open_dm(user_id)
            .await
            .map(|id| gateway::dm_chat(&id))
    }
}

#[async_trait]
impl Channel for DiscordTransport {
    async fn receive(&self) -> channel_api::Result<BoxStream<'static, IncomingMessage>> {
        let (tx, rx) = mpsc::channel(64);
        tokio::spawn(gateway::run(
            self.gateway_url.clone(),
            self.token.clone(),
            self.backoff,
            tx,
            self.cancel.child_token(),
            Arc::clone(&self.health),
        ));
        Ok(Box::pin(futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|m| (m, rx))
        })))
    }

    async fn send(&self, to: ChatRef, msg: OutgoingMessage) -> channel_api::Result<String> {
        let parts = parse_chat(&to)
            .ok_or_else(|| ChannelError::InvalidInput("not a Discord chat reference".into()))?;
        let channel_id = parts.channel_id;
        let clean = self.clean(&msg.text);
        let comps = components(&msg.buttons, &self.redactor);
        if let Some(id) = msg.edit_message_id {
            let text = clamp_chars(&clean, MAX_MESSAGE_CHARS);
            self.rest
                .edit_message(channel_id, &id, &text, comps.as_ref())
                .await?;
            return Ok(id);
        }
        let pieces = split_message(&clean, MAX_MESSAGE_CHARS);
        let last = pieces.len() - 1;
        let mut id = String::new();
        for (i, piece) in pieces.iter().enumerate() {
            // Buttons only on the last piece, so they sit under the whole text.
            let c = if i == last { comps.as_ref() } else { None };
            id = self.rest.create_message(channel_id, piece, c).await?;
        }
        Ok(id)
    }

    async fn answer_callback(&self, query_id: &str, text: Option<&str>) -> channel_api::Result<()> {
        let (id, token) = query_id
            .split_once(':')
            .ok_or_else(|| ChannelError::InvalidInput("not a Discord interaction".into()))?;
        let text = text.map(|t| self.clean(t));
        self.rest
            .interaction_callback(id, token, text.as_deref())
            .await
            .map_err(DiscordError::into)
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
