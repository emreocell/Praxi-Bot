//! The e-mail *channel* (`/v1/email/channel*`): new mail in one folder reaches a
//! bot as untrusted text; replies are proposed by the bot and sent only after
//! the owner approves the exact text. The daemon owns no state here: `bot serve`
//! plugs in an [`EmailChannelControl`] over `connector-email`. Every route is
//! owner-only (see `owner.rs`) and refused for paired phones (see `peer.rs`).

use std::sync::Arc;

use async_trait::async_trait;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::AppState;
use crate::email::EmailControlError;
use crate::error::{ApiError, ErrorBody};
use crate::routes::ApiJson;

/// How much of a mail the bot sees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EmailChannelMode {
    /// Sender, subject, date and a short snippet.
    #[default]
    Summary,
    /// The whole plain-text body (capped).
    Full,
}

/// What the owner chose for the e-mail channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EmailChannelSettingsDto {
    /// Off until the owner turns it on.
    pub enabled: bool,
    /// Saved e-mail account to watch.
    pub account: String,
    /// Folder to watch.
    pub folder: String,
    /// Bot that receives the mail (storage id); `null` = nobody yet.
    pub bot: Option<String>,
    /// Seconds between checks (60 to 3600).
    pub poll_secs: u64,
    /// Summary (default) or full text.
    pub mode: EmailChannelMode,
    /// Most mails delivered per check.
    pub max_per_poll: usize,
    /// The bot may propose replies (each needs the owner's approval).
    pub replies_enabled: bool,
}

/// A reply waiting for the owner's approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EmailPendingReplyDto {
    /// Id used to approve or deny.
    pub id: String,
    /// Exact recipients.
    pub to: Vec<String>,
    /// Subject.
    pub subject: String,
    /// The whole body, exactly as it will be sent. Plain text: show it as text.
    pub body: String,
    /// When it was proposed (Unix ms).
    pub created_ms: u64,
}

/// The e-mail channel as the app shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EmailChannelDto {
    /// The saved choices.
    pub settings: EmailChannelSettingsDto,
    /// The channel is running in this daemon.
    pub running: bool,
    /// Replies waiting for approval.
    pub pending_replies: usize,
}

/// Implemented by the embedder.
#[async_trait]
pub trait EmailChannelControl: Send + Sync {
    /// Current settings and state.
    fn get(&self) -> EmailChannelDto;
    /// Validate, save and apply new settings.
    ///
    /// # Errors
    /// [`EmailControlError::Invalid`] for bad values or an unknown account.
    async fn put(
        &self,
        settings: EmailChannelSettingsDto,
    ) -> Result<EmailChannelDto, EmailControlError>;
    /// Replies waiting for approval, oldest first.
    fn replies(&self) -> Vec<EmailPendingReplyDto>;
    /// Send the waiting reply `id` exactly as stored.
    ///
    /// # Errors
    /// Not found, rate limit, or the server refusing the mail.
    async fn approve(&self, id: &str) -> Result<(), EmailControlError>;
    /// Drop the waiting reply `id`.
    ///
    /// # Errors
    /// [`EmailControlError::NotFound`].
    fn deny(&self, id: &str) -> Result<(), EmailControlError>;
}

fn control(s: &AppState) -> Result<Arc<dyn EmailChannelControl>, ApiError> {
    s.email_channel()
        .ok_or_else(|| ApiError::unavailable("the e-mail channel is not available in this daemon"))
}

/// Settings and state of the e-mail channel.
#[utoipa::path(get, path = "/v1/email/channel", tag = "email", security(("bearer" = [])),
    responses((status = 200, body = EmailChannelDto),
              (status = 503, body = ErrorBody, description = "not available")))]
pub(crate) async fn get_channel(
    State(s): State<AppState>,
) -> Result<Json<EmailChannelDto>, ApiError> {
    Ok(Json(control(&s)?.get()))
}

/// Save new settings (owner only). Replies stay off unless switched on here.
#[utoipa::path(put, path = "/v1/email/channel", tag = "email", security(("bearer" = [])),
    request_body = EmailChannelSettingsDto,
    responses((status = 200, body = EmailChannelDto),
              (status = 404, body = ErrorBody, description = "unknown bot"),
              (status = 422, body = ErrorBody, description = "invalid settings"),
              (status = 503, body = ErrorBody, description = "not available")))]
pub(crate) async fn put_channel(
    State(s): State<AppState>,
    ApiJson(mut input): ApiJson<EmailChannelSettingsDto>,
) -> Result<Json<EmailChannelDto>, ApiError> {
    let control = control(&s)?;
    if let Some(bot) = input.bot.take() {
        input.bot = Some(crate::email_routes::canonical_bot(&s, &bot)?);
    }
    Ok(Json(control.put(input).await?))
}

/// Replies waiting for approval (owner only).
#[utoipa::path(get, path = "/v1/email/channel/replies", tag = "email", security(("bearer" = [])),
    responses((status = 200, body = Vec<EmailPendingReplyDto>),
              (status = 503, body = ErrorBody, description = "not available")))]
pub(crate) async fn list_replies(
    State(s): State<AppState>,
) -> Result<Json<Vec<EmailPendingReplyDto>>, ApiError> {
    Ok(Json(control(&s)?.replies()))
}

/// Approve a waiting reply: it is sent exactly as listed (owner only).
#[utoipa::path(post, path = "/v1/email/channel/replies/{id}/approve", tag = "email",
    security(("bearer" = [])), params(("id" = String, Path, description = "Reply id")),
    responses((status = 204, description = "sent"),
              (status = 404, body = ErrorBody, description = "no such reply"),
              (status = 502, body = ErrorBody, description = "the mail server refused it"),
              (status = 503, body = ErrorBody, description = "not available")))]
pub(crate) async fn approve_reply(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    control(&s)?.approve(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Drop a waiting reply (owner only).
#[utoipa::path(post, path = "/v1/email/channel/replies/{id}/deny", tag = "email",
    security(("bearer" = [])), params(("id" = String, Path, description = "Reply id")),
    responses((status = 204, description = "dropped"),
              (status = 404, body = ErrorBody, description = "no such reply"),
              (status = 503, body = ErrorBody, description = "not available")))]
pub(crate) async fn deny_reply(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    control(&s)?.deny(&id)?;
    Ok(StatusCode::NO_CONTENT)
}

impl AppState {
    /// Start a run for one received e-mail, for the in-process e-mail channel
    /// only. There is deliberately no HTTP route and no message-source hint for
    /// this: an API client cannot make a chat message count as "e-mail", and
    /// cannot start one. Each mail gets its own chat (titled with a fixed
    /// word, never with the subject) and a run that is tainted from its first
    /// model call: `instruction` is the only task text, `mail` rides beside it
    /// as an untrusted data block and every tool asks.
    ///
    /// # Errors
    /// Safe mode, or any run-start error.
    pub fn deliver_email(
        &self,
        bot: protocol::BotId,
        instruction: &str,
        mail: &str,
    ) -> Result<protocol::RunId, ApiError> {
        if self.safe_mode() {
            return Err(ApiError::conflict(
                "the e-mail channel is paused in safe mode",
            ));
        }
        let envelope = serde_json::to_string(&serde_json::json!({
            "instruction": instruction,
            "payload": mail,
        }))
        .map_err(|_| ApiError::internal("could not store the mail"))?;
        let chat = storage::Chat {
            id: protocol::ChatId::new(),
            user_id: self.user_id(),
            bot_id: bot,
            title: "E-mail".to_owned(),
            created_at_ms: storage::now_ms(),
        };
        self.storage().insert_chat(&chat)?;
        let started = self
            .leader()
            .with_active(|| self.agent().run_incoming_email(chat.id, &envelope));
        match started {
            Ok(Ok(run)) => Ok(run),
            Ok(Err(err)) => {
                let _ = self.storage().delete_chat(chat.id);
                Err(err.into())
            }
            Err(err) => {
                let _ = self.storage().delete_chat(chat.id);
                Err(err)
            }
        }
    }
}
