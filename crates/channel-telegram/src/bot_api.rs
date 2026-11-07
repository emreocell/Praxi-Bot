//! Telegram Bot API HTTP client (`getUpdates`, `sendMessage`, `editMessageText`,
//! `answerCallbackQuery`) and edit-rate throttling.

use std::fmt;
use std::time::{Duration, Instant};

use reqwest::Client;
use secret_store::{REDACTED, SecretValue};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::channel_trait::{ChatRef, IncomingKind, IncomingMessage, InlineButton};
use crate::error::{Result, TelegramError};

/// Default base URL of the official Telegram Bot API.
pub const DEFAULT_TELEGRAM_API_URL: &str = "https://api.telegram.org";

/// Default minimum interval between `editMessageText` calls (≤ 1 edit/s).
pub const DEFAULT_EDIT_INTERVAL: Duration = Duration::from_secs(1);

/// Maximum UTF-8 character count allowed by Telegram in a single message.
pub const MAX_MESSAGE_CHARS: usize = 4096;

/// Standard Telegram Bot API response envelope.
#[derive(Debug, Clone, Deserialize)]
pub struct ApiResponse<T> {
    /// `true` when the request succeeded.
    pub ok: bool,
    /// Payload present when `ok` is `true`.
    pub result: Option<T>,
    /// Human-readable error message when `ok` is `false`.
    pub description: Option<String>,
    /// Numerical error code when `ok` is `false`.
    pub error_code: Option<i64>,
}

/// An incoming Telegram update returned by `getUpdates`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Update {
    /// Monotonically increasing identifier of the update.
    pub update_id: i64,
    /// New incoming message of any kind (text, command, etc.).
    #[serde(default)]
    pub message: Option<TgMessage>,
    /// New incoming callback query from an inline keyboard button.
    #[serde(default)]
    pub callback_query: Option<CallbackQuery>,
}

impl Update {
    /// Convert this Telegram update into a channel-agnostic [`IncomingMessage`], if supported.
    #[must_use]
    pub fn into_incoming(self) -> Option<IncomingMessage> {
        if let Some(msg) = self.message {
            let text = msg.text?;
            let from = msg.from?;
            let username = from.username.or(from.first_name);
            return Some(IncomingMessage {
                update_id: self.update_id,
                chat: ChatRef(msg.chat.id),
                user_id: from.id,
                username,
                message_id: Some(msg.message_id),
                kind: IncomingKind::Text(text),
            });
        }
        if let Some(cb) = self.callback_query {
            let data = cb.data.unwrap_or_default();
            let chat_id = cb.message.as_ref().map_or(cb.from.id, |m| m.chat.id);
            let message_id = cb.message.as_ref().map(|m| m.message_id);
            let username = cb.from.username.or(cb.from.first_name);
            return Some(IncomingMessage {
                update_id: self.update_id,
                chat: ChatRef(chat_id),
                user_id: cb.from.id,
                username,
                message_id,
                kind: IncomingKind::Callback {
                    query_id: cb.id,
                    data,
                    message_id,
                },
            });
        }
        None
    }
}

/// A Telegram message object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TgMessage {
    /// Unique message identifier inside this chat.
    pub message_id: i64,
    /// Sender of the message.
    #[serde(default)]
    pub from: Option<TgUser>,
    /// Conversation the message belongs to.
    pub chat: TgChat,
    /// UTF-8 text of the message, if it is a text message.
    #[serde(default)]
    pub text: Option<String>,
}

/// A Telegram user or bot account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TgUser {
    /// Unique numeric identifier for this user or bot.
    pub id: i64,
    /// `true` if this user is a bot.
    #[serde(default)]
    pub is_bot: bool,
    /// User's or bot's first name.
    #[serde(default)]
    pub first_name: Option<String>,
    /// User's or bot's `@username`.
    #[serde(default)]
    pub username: Option<String>,
}

/// A Telegram chat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TgChat {
    /// Unique numeric identifier for this chat.
    pub id: i64,
    /// Type of chat (`"private"`, `"group"`, `"supergroup"`, or `"channel"`).
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
}

/// An incoming callback query from a callback button in an [`InlineKeyboardMarkup`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallbackQuery {
    /// Unique identifier for this query.
    pub id: String,
    /// Sender who pressed the button.
    pub from: TgUser,
    /// Message with the callback button that originated the query.
    #[serde(default)]
    pub message: Option<TgMessage>,
    /// Data associated with the callback button.
    #[serde(default)]
    pub data: Option<String>,
}

/// One button of an inline keyboard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InlineKeyboardButton {
    /// Label text on the button.
    pub text: String,
    /// Data to be sent in a [`CallbackQuery`] to the bot when the button is pressed (1–64 bytes).
    pub callback_data: String,
}

impl From<InlineButton> for InlineKeyboardButton {
    fn from(b: InlineButton) -> Self {
        Self {
            text: b.text,
            callback_data: b.callback_data,
        }
    }
}

/// Inline keyboard attached directly to a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InlineKeyboardMarkup {
    /// Array of button rows, each represented by an array of [`InlineKeyboardButton`] objects.
    pub inline_keyboard: Vec<Vec<InlineKeyboardButton>>,
}

impl InlineKeyboardMarkup {
    /// Build an inline keyboard from rows of [`InlineButton`]s.
    #[must_use]
    pub fn from_rows(rows: Vec<Vec<InlineButton>>) -> Option<Self> {
        if rows.is_empty() {
            None
        } else {
            Some(Self {
                inline_keyboard: rows
                    .into_iter()
                    .map(|r| r.into_iter().map(InlineKeyboardButton::from).collect())
                    .collect(),
            })
        }
    }
}

#[derive(Debug, Serialize)]
struct GetUpdatesBody<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    offset: Option<i64>,
    timeout: u64,
    allowed_updates: &'a [&'a str],
}

#[derive(Debug, Serialize)]
struct SendMessageBody<'a> {
    chat_id: i64,
    text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reply_markup: Option<&'a InlineKeyboardMarkup>,
}

#[derive(Debug, Serialize)]
struct EditMessageTextBody<'a> {
    chat_id: i64,
    message_id: i64,
    text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reply_markup: Option<&'a InlineKeyboardMarkup>,
}

#[derive(Debug, Serialize)]
struct AnswerCallbackQueryBody<'a> {
    callback_query_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<&'a str>,
}

/// Clamp a string to at most [`MAX_MESSAGE_CHARS`] Unicode scalar values.
#[must_use]
pub fn clamp_telegram_text(text: &str) -> String {
    if text.chars().count() <= MAX_MESSAGE_CHARS {
        return text.to_owned();
    }
    let truncated: String = text.chars().take(MAX_MESSAGE_CHARS - 1).collect();
    format!("{truncated}…")
}

/// HTTP client for the Telegram Bot API.
///
/// The bot token is held inside a [`SecretValue`] and is scrubbed from any
/// transport error strings so it can never leak into logs or error reports.
#[derive(Clone)]
pub struct BotApiClient {
    http: Client,
    base_url: String,
    token: SecretValue,
}

impl fmt::Debug for BotApiClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BotApiClient")
            .field("base_url", &self.base_url)
            .field("token", &REDACTED)
            .finish_non_exhaustive()
    }
}

impl BotApiClient {
    /// Create a client targeting [`DEFAULT_TELEGRAM_API_URL`].
    #[must_use]
    pub fn new(token: SecretValue) -> Self {
        Self::with_base_url(token, DEFAULT_TELEGRAM_API_URL)
    }

    /// Create a client targeting a custom base URL (e.g. a local `wiremock` server).
    #[must_use]
    pub fn with_base_url(token: SecretValue, base_url: impl Into<String>) -> Self {
        let base_url = base_url.into().trim_end_matches('/').to_owned();
        Self {
            http: Client::new(),
            base_url,
            token,
        }
    }

    /// Base URL configured on this client.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn endpoint(&self, method: &str) -> String {
        format!("{}/bot{}/{method}", self.base_url, self.token.expose())
    }

    fn sanitize_error(&self, err: &reqwest::Error) -> TelegramError {
        let raw = err.to_string();
        let tok = self.token.expose();
        let clean = if !tok.is_empty() && raw.contains(tok) {
            raw.replace(tok, REDACTED)
        } else {
            raw
        };
        TelegramError::Http(clean)
    }

    async fn post<B: Serialize, R: DeserializeOwned>(
        &self,
        method: &str,
        body: &B,
        timeout: Duration,
    ) -> Result<R> {
        let resp = self
            .http
            .post(self.endpoint(method))
            .timeout(timeout)
            .json(body)
            .send()
            .await
            .map_err(|e| self.sanitize_error(&e))?;

        let parsed: ApiResponse<R> = resp.json().await.map_err(|e| self.sanitize_error(&e))?;

        if !parsed.ok {
            let mut desc = parsed
                .description
                .unwrap_or_else(|| format!("{method} failed"));
            let tok = self.token.expose();
            if !tok.is_empty() && desc.contains(tok) {
                desc = desc.replace(tok, REDACTED);
            }
            return Err(TelegramError::TelegramApi {
                error_code: parsed.error_code,
                description: desc,
            });
        }

        parsed.result.ok_or_else(|| TelegramError::TelegramApi {
            error_code: parsed.error_code,
            description: format!("{method} returned ok=true without result"),
        })
    }

    /// Long-poll `getUpdates` for new messages and callback queries.
    ///
    /// # Errors
    /// Transport or Telegram API errors.
    pub async fn get_updates(&self, offset: Option<i64>, timeout_secs: u64) -> Result<Vec<Update>> {
        let body = GetUpdatesBody {
            offset,
            timeout: timeout_secs,
            allowed_updates: &["message", "callback_query"],
        };
        let http_timeout = Duration::from_secs(timeout_secs.saturating_add(10).max(15));
        self.post("getUpdates", &body, http_timeout).await
    }

    /// Send a text message (with optional inline keyboard) via `sendMessage`.
    ///
    /// # Errors
    /// Transport or Telegram API errors.
    pub async fn send_message(
        &self,
        chat_id: i64,
        text: &str,
        reply_markup: Option<&InlineKeyboardMarkup>,
    ) -> Result<TgMessage> {
        let clamped = clamp_telegram_text(text);
        let body = SendMessageBody {
            chat_id,
            text: &clamped,
            reply_markup,
        };
        self.post("sendMessage", &body, Duration::from_secs(15))
            .await
    }

    /// Edit an existing text message in place via `editMessageText`.
    ///
    /// If Telegram replies with `400 Bad Request: message is not modified`, this
    /// method treats the call as a no-op success.
    ///
    /// # Errors
    /// Transport or Telegram API errors.
    pub async fn edit_message_text(
        &self,
        chat_id: i64,
        message_id: i64,
        text: &str,
        reply_markup: Option<&InlineKeyboardMarkup>,
    ) -> Result<TgMessage> {
        let clamped = clamp_telegram_text(text);
        let body = EditMessageTextBody {
            chat_id,
            message_id,
            text: &clamped,
            reply_markup,
        };
        match self
            .post("editMessageText", &body, Duration::from_secs(15))
            .await
        {
            Ok(msg) => Ok(msg),
            Err(TelegramError::TelegramApi {
                error_code: Some(400),
                ref description,
            }) if description.contains("message is not modified") => Ok(TgMessage {
                message_id,
                from: None,
                chat: TgChat {
                    id: chat_id,
                    kind: None,
                },
                text: Some(clamped),
            }),
            Err(e) => Err(e),
        }
    }

    /// Acknowledge an inline button press via `answerCallbackQuery`.
    ///
    /// # Errors
    /// Transport or Telegram API errors.
    pub async fn answer_callback_query(
        &self,
        callback_query_id: &str,
        text: Option<&str>,
    ) -> Result<bool> {
        let body = AnswerCallbackQueryBody {
            callback_query_id,
            text,
        };
        self.post("answerCallbackQuery", &body, Duration::from_secs(15))
            .await
    }
}

/// Throttles in-place message edits to at most one `editMessageText` per `min_interval` (default 1 s).
#[derive(Debug)]
pub struct ThrottledEditor {
    chat_id: i64,
    min_interval: Duration,
    message_id: Option<i64>,
    last_sent_text: String,
    pending_text: String,
    last_touch_at: Option<Instant>,
}

impl ThrottledEditor {
    /// Create a throttled editor for `chat_id` with the given minimum edit interval.
    #[must_use]
    pub fn new(chat_id: i64, min_interval: Duration) -> Self {
        Self {
            chat_id,
            min_interval,
            message_id: None,
            last_sent_text: String::new(),
            pending_text: String::new(),
            last_touch_at: None,
        }
    }

    /// Telegram `message_id` of the streamed message, once the initial message has been sent.
    #[must_use]
    pub fn message_id(&self) -> Option<i64> {
        self.message_id
    }

    /// Latest accumulated text (whether or not it has been flushed yet).
    #[must_use]
    pub fn pending_text(&self) -> &str {
        &self.pending_text
    }

    /// Text most recently sent or edited on Telegram.
    #[must_use]
    pub fn last_sent_text(&self) -> &str {
        &self.last_sent_text
    }

    /// Update the streamed message with `full_text` (already redacted).
    ///
    /// - If no message exists yet, sends a new message via `sendMessage`.
    /// - If a message exists and `min_interval` has elapsed since the last send/edit,
    ///   edits it via `editMessageText`.
    /// - Otherwise buffers `full_text` until the throttle window elapses or [`Self::flush_final`] is called.
    ///
    /// # Errors
    /// Transport or Telegram API errors.
    pub async fn push(&mut self, api: &BotApiClient, full_text: &str) -> Result<Option<i64>> {
        if full_text.is_empty() {
            return Ok(self.message_id);
        }
        full_text.clone_into(&mut self.pending_text);
        let now = Instant::now();

        match self.message_id {
            None => {
                let msg = api.send_message(self.chat_id, full_text, None).await?;
                self.message_id = Some(msg.message_id);
                full_text.clone_into(&mut self.last_sent_text);
                self.last_touch_at = Some(now);
                Ok(Some(msg.message_id))
            }
            Some(msg_id) => {
                let ready = self
                    .last_touch_at
                    .is_none_or(|t| now.duration_since(t) >= self.min_interval);
                if ready && self.pending_text != self.last_sent_text {
                    api.edit_message_text(self.chat_id, msg_id, &self.pending_text, None)
                        .await?;
                    self.last_sent_text.clone_from(&self.pending_text);
                    self.last_touch_at = Some(now);
                }
                Ok(Some(msg_id))
            }
        }
    }

    /// Flush buffered text if `min_interval` has elapsed since the last send/edit.
    ///
    /// # Errors
    /// Transport or Telegram API errors.
    pub async fn flush_if_due(&mut self, api: &BotApiClient) -> Result<bool> {
        let Some(msg_id) = self.message_id else {
            return Ok(false);
        };
        if self.pending_text == self.last_sent_text || self.pending_text.is_empty() {
            return Ok(false);
        }
        let now = Instant::now();
        if self
            .last_touch_at
            .is_none_or(|t| now.duration_since(t) >= self.min_interval)
        {
            api.edit_message_text(self.chat_id, msg_id, &self.pending_text, None)
                .await?;
            self.last_sent_text.clone_from(&self.pending_text);
            self.last_touch_at = Some(now);
            return Ok(true);
        }
        Ok(false)
    }

    /// Flush any remaining buffered text at the end of a run so the final message is complete.
    ///
    /// # Errors
    /// Transport or Telegram API errors.
    pub async fn flush_final(&mut self, api: &BotApiClient) -> Result<Option<i64>> {
        if self.pending_text.is_empty() {
            return Ok(self.message_id);
        }
        match self.message_id {
            None => {
                let msg = api
                    .send_message(self.chat_id, &self.pending_text, None)
                    .await?;
                self.message_id = Some(msg.message_id);
                self.last_sent_text.clone_from(&self.pending_text);
                self.last_touch_at = Some(Instant::now());
                Ok(Some(msg.message_id))
            }
            Some(msg_id) => {
                if self.pending_text != self.last_sent_text {
                    api.edit_message_text(self.chat_id, msg_id, &self.pending_text, None)
                        .await?;
                    self.last_sent_text.clone_from(&self.pending_text);
                    self.last_touch_at = Some(Instant::now());
                }
                Ok(Some(msg_id))
            }
        }
    }
}
