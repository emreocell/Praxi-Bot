//! Telegram Bot API channel adapter (ADR-0019, ADR-0009, ADR-0012).
//!
//! Lets the user message their bots from Telegram while away from the PC, watch
//! streaming progress edited in place (throttled to ≤ 1 edit/s), and approve or
//! deny gated tool actions with inline buttons (`Approve once`, `Always`, `Deny`).
//!
//! # Architecture
//! - [`bot_api::BotApiClient`]: long polling (`getUpdates`), `sendMessage`,
//!   `editMessageText` (with [`bot_api::ThrottledEditor`]), inline keyboards, and
//!   `answerCallbackQuery` over `reqwest` + `serde`.
//! - [`pairing::PairingStore`]: stores the bot token in [`secret_store`] under
//!   [`TELEGRAM_BOT_TOKEN_SECRET`], generates one-time 6-digit pairing codes with
//!   a 10-minute expiry ([`PAIR_CODE_TTL`]), persists paired Telegram user IDs and
//!   chat→bot mappings, and rate-limits polite refusals to unpaired users.
//! - [`bridge::TelegramChannel`]: implements [`Channel`] and bridges updates to
//!   the daemon via [`DaemonClient`] (either in-process or out-of-process with
//!   [`HttpDaemonClient`]), redacting all outgoing text and tool outputs through
//!   [`secret_store::Redactor`].
//!
//! # Follow-up CLI / Daemon Wiring (`bot channels telegram setup|pair|start`)
//! Because `apps/cli` and `daemon-api` are owned by another concurrent task,
//! the CLI subcommands can be wired in a follow-up as follows:
//! - `bot channels telegram setup`: prompt for the `BotFather` token (never on argv)
//!   and call [`store_bot_token`] on the workspace [`secret_store::SecretStore`].
//! - `bot channels telegram pair`: generate a 6-digit code via
//!   [`TelegramChannel::create_pair_code`] (or a daemon route wrapping it) and
//!   print `"Send /pair <code> to your Telegram bot within 10 minutes"`.
//! - `bot channels telegram start`: open the secret store, build
//!   [`TelegramConfig::new`] (with `<data_dir>/telegram_state.json`) and
//!   [`HttpDaemonClient::new`] (using the daemon's local URL and bearer token),
//!   and run [`TelegramChannel::start`].

pub mod bot_api;
pub mod bridge;
pub mod channel_trait;
pub mod daemon_client;
pub mod error;
pub mod pairing;

#[cfg(test)]
mod tests;

pub use bot_api::{
    ApiResponse, BotApiClient, CallbackQuery, DEFAULT_EDIT_INTERVAL, DEFAULT_TELEGRAM_API_URL,
    InlineKeyboardButton, InlineKeyboardMarkup, MAX_MESSAGE_CHARS, TgChat, TgMessage, TgUser,
    ThrottledEditor, Update, clamp_telegram_text,
};
pub use bridge::{
    DEFAULT_POLL_BACKOFF, DEFAULT_POLL_TIMEOUT_SECS, TelegramChannel, TelegramConfig,
};
pub use channel_trait::{
    Channel, ChannelCaps, ChatRef, IncomingKind, IncomingMessage, InlineButton, OutgoingMessage,
};
pub use daemon_client::{
    ApiClient, ApprovalDecision, ApprovalScope, BotSummary, ChatSummary, DaemonClient,
    DaemonStatus, DeliveryKind, HttpDaemonClient, SendOutcome, format_approval_callback,
    format_question_callback, parse_approval_callback, parse_question_callback,
};
pub use error::{Result, TelegramError};
pub use pairing::{
    MAX_PAIR_ATTEMPTS, PAIR_CODE_TTL, PairAttempt, PairingStore, REFUSAL_COOLDOWN,
    TELEGRAM_BOT_TOKEN_SECRET, UNPAIRED_REFUSAL_TEXT, load_bot_token, store_bot_token,
    store_bot_token_as,
};
