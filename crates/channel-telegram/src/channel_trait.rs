//! Channel plugin trait and message primitives (ADR-0019).
//!
//! `crates/channel-api` is currently a placeholder stub on `main`. To respect
//! task boundaries (editing only `crates/channel-telegram`, root `Cargo.toml`,
//! and `Cargo.lock`), these traits live here and can be re-exported from or
//! moved into `channel-api` when wiring multiple channels.

use async_trait::async_trait;
use futures::stream::BoxStream;
use protocol::ChannelRef;
use serde::{Deserialize, Serialize};

use crate::error::Result;

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

/// Capabilities advertised by a messaging channel (ADR-0019).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct ChannelCaps {
    /// Supports interactive inline buttons (for approvals and question options).
    pub inline_buttons: bool,
    /// Supports editing a previously sent message in place (for streaming deltas).
    pub message_editing: bool,
    /// Supports file attachments.
    pub file_upload: bool,
    /// Supports voice messages.
    pub voice_messages: bool,
}

impl ChannelCaps {
    /// Default capabilities of the Telegram Bot API v0 adapter.
    #[must_use]
    pub const fn telegram_v0() -> Self {
        Self {
            inline_buttons: true,
            message_editing: true,
            file_upload: false,
            voice_messages: false,
        }
    }
}

/// An inline button attached to an [`OutgoingMessage`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InlineButton {
    /// Visible label on the button.
    pub text: String,
    /// Opaque payload returned when the user presses the button (≤ 64 bytes on Telegram).
    pub callback_data: String,
}

impl InlineButton {
    /// Create a new inline keyboard button.
    #[must_use]
    pub fn new(text: impl Into<String>, callback_data: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            callback_data: callback_data.into(),
        }
    }
}

/// Payload of an [`IncomingMessage`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum IncomingKind {
    /// A text message or slash command (`/pair`, `/bots`, `/use`, `/stop`, `/status`).
    Text(String),
    /// An inline button callback press.
    Callback {
        /// Telegram callback query id (used for `answerCallbackQuery`).
        query_id: String,
        /// The `callback_data` string attached to the pressed button.
        data: String,
        /// Message id of the message that carried the inline keyboard, if available.
        message_id: Option<i64>,
    },
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
    /// Message id (for text messages).
    pub message_id: Option<i64>,
    /// Message content or callback payload.
    pub kind: IncomingKind,
}

/// An outgoing message or in-place edit to be sent to a [`ChatRef`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutgoingMessage {
    /// Message body (always passed through the secret [`secret_store::Redactor`] before sending).
    pub text: String,
    /// Optional rows of inline keyboard buttons.
    pub buttons: Vec<Vec<InlineButton>>,
    /// When `Some(message_id)`, edits that existing message in place instead of sending a new one.
    pub edit_message_id: Option<i64>,
}

impl OutgoingMessage {
    /// Plain text message.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            buttons: Vec::new(),
            edit_message_id: None,
        }
    }

    /// Edit an existing message in place.
    #[must_use]
    pub fn edit(message_id: i64, text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            buttons: Vec::new(),
            edit_message_id: Some(message_id),
        }
    }

    /// Attach rows of inline buttons.
    #[must_use]
    pub fn with_buttons(mut self, buttons: Vec<Vec<InlineButton>>) -> Self {
        self.buttons = buttons;
        self
    }
}

/// Common interface for external messaging channels (ADR-0019).
#[async_trait]
pub trait Channel: Send + Sync {
    /// Stream of incoming messages from the channel.
    async fn receive(&self) -> Result<BoxStream<'static, IncomingMessage>>;

    /// Send a new message or edit an existing message in `to`; returns the Telegram `message_id`.
    async fn send(&self, to: ChatRef, msg: OutgoingMessage) -> Result<i64>;

    /// Capabilities supported by this channel implementation.
    fn capabilities(&self) -> ChannelCaps;
}
