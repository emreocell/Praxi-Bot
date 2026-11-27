//! Small transport contract for messaging adapters (ADR-0019).
//!
//! Identifiers are opaque strings scoped to an adapter. Polling offsets and
//! service-specific delivery/acknowledgement state stay in the adapter.
//! Receiving an event does not authorize a daemon action: pairing, allowlists,
//! owner authentication and permission checks still apply at the bridge.

use async_trait::async_trait;
use futures_core::stream::BoxStream;
use serde::{Deserialize, Serialize};

pub mod approval;
pub mod conformance;
mod error;
pub mod notice;
pub mod pairing;
pub use error::{ChannelError, Result};

/// Destination within one adapter; use `protocol::ChannelRef` to qualify it
/// with a channel name when storing it across adapters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ChatRef(pub String);

impl std::fmt::Display for ChatRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Authenticated service identity supplied by the adapter, never by message text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SenderIdentity {
    /// Opaque service user id, stable across display-name changes.
    pub id: String,
    /// Optional display name; never an authorization identity.
    pub display_name: Option<String>,
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
pub enum IncomingKind<M = String> {
    /// A text message or slash command (`/pair`, `/bots`, `/use`, `/stop`, `/status`).
    Text(String),
    /// An attachment this channel cannot read yet (voice, photo, file ...); the
    /// payload is its human label.
    Unsupported(String),
    /// A voice note the adapter can download (Ogg Opus), so the host may transcribe it.
    Voice {
        /// Opaque adapter id of the recording.
        file_id: String,
        /// Length announced by the service, in seconds.
        duration_secs: u32,
    },
    /// An inline button callback press.
    Callback {
        /// Opaque callback id used to acknowledge the interaction.
        query_id: String,
        /// The `callback_data` string attached to the pressed button.
        data: String,
        /// Message id of the message that carried the inline keyboard, if available.
        message_id: Option<M>,
    },
}

/// Incoming event; service offsets remain private to the adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncomingMessage {
    /// Stable opaque event id for deduplication; not a polling cursor.
    pub event_id: String,
    /// Destination that received the event.
    pub chat: ChatRef,
    /// Service-verified sender.
    pub sender: SenderIdentity,
    /// Message identifier, when available.
    pub message_id: Option<String>,
    /// Text, unsupported attachment label, or callback.
    pub kind: IncomingKind,
}

/// An outgoing message or in-place edit to be sent to a [`ChatRef`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutgoingMessage<M = String> {
    /// Message body; adapters must redact secrets before sending.
    pub text: String,
    /// Optional rows of inline keyboard buttons.
    pub buttons: Vec<Vec<InlineButton>>,
    /// When `Some(message_id)`, edits that existing message in place instead of sending a new one.
    pub edit_message_id: Option<M>,
}

impl<M> OutgoingMessage<M> {
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
    pub fn edit(message_id: M, text: impl Into<String>) -> Self {
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

/// Object-safe interface for external messaging channels (ADR-0019).
#[async_trait]
pub trait Channel: Send + Sync {
    /// Receive events. The adapter owns delivery recovery and polling cursors.
    ///
    /// # Errors
    /// Adapter transport or storage failure.
    async fn receive(&self) -> Result<BoxStream<'static, IncomingMessage>>;

    /// Send or edit a message, redacting secrets before transport; return its id.
    ///
    /// # Errors
    /// Invalid adapter identifiers, unsupported operation, or transport failure.
    async fn send(&self, to: ChatRef, msg: OutgoingMessage) -> Result<String>;

    /// Acknowledge a callback, redacting any acknowledgement text.
    ///
    /// # Errors
    /// Unsupported callbacks or transport failure.
    async fn answer_callback(&self, query_id: &str, text: Option<&str>) -> Result<()>;

    /// Supported operations. Advertising a capability does not authorize its use.
    fn capabilities(&self) -> ChannelCaps;
}

#[cfg(test)]
mod tests;
