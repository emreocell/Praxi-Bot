//! Telegram numeric identities and durable inbox representation.
//! Shared transport primitives live in `channel-api`.

use protocol::ChannelRef;
use serde::{Deserialize, Serialize};

pub use channel_api::{Channel, ChannelCaps, InlineButton};
/// Telegram callback payload with numeric message identifiers.
pub type IncomingKind = channel_api::IncomingKind<i64>;
/// Telegram outgoing payload with numeric edit identifiers.
pub type OutgoingMessage = channel_api::OutgoingMessage<i64>;

/// Identifies a destination chat on the messaging channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ChatRef(pub i64);

impl ChatRef {
    /// Convert to a [`protocol::ChannelRef`] (`"telegram:<chat_id>"`).
    #[must_use]
    pub fn to_channel_ref(self) -> ChannelRef {
        ChannelRef::new("telegram", self.0.to_string())
    }
}

impl From<i64> for ChatRef {
    fn from(id: i64) -> Self {
        Self(id)
    }
}

impl std::fmt::Display for ChatRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// An incoming event received from a messaging channel (ADR-0019).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncomingMessage {
    /// Monotonically increasing Telegram `update_id`.
    pub update_id: i64,
    /// Chat where the event originated.
    pub chat: ChatRef,
    /// Telegram numeric user id of the sender.
    pub user_id: i64,
    /// Optional username or display name of the sender.
    pub username: Option<String>,
    /// Language tag of the sender's Telegram client, if sent. Used only to pick
    /// the language of a chat that is paired for the first time.
    #[serde(default)]
    pub language_code: Option<String>,
    /// Message id (for text messages).
    pub message_id: Option<i64>,
    /// Message content or callback payload.
    pub kind: IncomingKind,
}

impl From<IncomingMessage> for channel_api::IncomingMessage {
    fn from(msg: IncomingMessage) -> Self {
        Self {
            event_id: msg.update_id.to_string(),
            chat: channel_api::ChatRef(msg.chat.0.to_string()),
            sender: channel_api::SenderIdentity {
                id: msg.user_id.to_string(),
                display_name: msg.username,
            },
            message_id: msg.message_id.map(|id| id.to_string()),
            kind: match msg.kind {
                IncomingKind::Text(text) => channel_api::IncomingKind::Text(text),
                IncomingKind::Unsupported(label) => channel_api::IncomingKind::Unsupported(label),
                IncomingKind::Callback {
                    query_id,
                    data,
                    message_id,
                } => channel_api::IncomingKind::Callback {
                    query_id,
                    data,
                    message_id: message_id.map(|id| id.to_string()),
                },
            },
        }
    }
}
