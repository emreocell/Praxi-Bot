//! Handlers of `/v1/email/*`. Same guard (Host / Origin / bearer) as every other
//! route; passwords are write-only.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;

use crate::AppState;
use crate::email::{
    EmailAccountDto, EmailAccountInput, EmailControl, EmailControlError, EmailGuardSettingsDto,
    EmailMessageDto, EmailOptOutDto, EmailOptOutInput, EmailProviderKind, EmailSendLogDto,
    EmailSendTestResponse, EmailTestReport,
};
use crate::email_bot_tools;
use crate::error::{ApiError, ErrorBody};
use crate::mcp_routes::MAX_BOT_ID_LEN;
use crate::routes::ApiJson;
use protocol::BotId;

/// Longest accepted password (bytes).
const MAX_PASSWORD_BYTES: usize = 1024;
/// Longest accepted existing-inbox address or id (bytes), as the provider's own limit.
const MAX_INBOX_BYTES: usize = 320;

impl From<EmailControlError> for ApiError {
    fn from(e: EmailControlError) -> Self {
        match e {
            EmailControlError::NotFound(w) => Self::not_found(w),
            EmailControlError::Conflict(m) => Self::conflict(m),
            EmailControlError::Invalid(m) => {
                Self::new(StatusCode::UNPROCESSABLE_ENTITY, "bad_email_config", m)
            }
            EmailControlError::Internal(m) => Self::internal(m),
            EmailControlError::Upstream(m) => {
                Self::new(StatusCode::BAD_GATEWAY, "email_provider_error", m)
            }
        }
    }
}

fn email(s: &AppState) -> Result<Arc<dyn EmailControl>, ApiError> {
    s.email()
        .ok_or_else(|| ApiError::unavailable("e-mail management is not available in this daemon"))
}

fn check_api_key(p: &str) -> Result<(), ApiError> {
    if p.is_empty() || p.len() > MAX_PASSWORD_BYTES || p.contains(['\0', '\r', '\n']) {
        return Err(ApiError::bad_request(
            "the API key must be 1 to 1024 bytes without line breaks or NUL",
        ));
    }
    Ok(())
}

fn check_password(p: &str) -> Result<(), ApiError> {
    if p.is_empty() || p.len() > MAX_PASSWORD_BYTES || p.contains(['\0', '\r', '\n']) {
        return Err(ApiError::bad_request(
            "the password must be 1 to 1024 bytes without line breaks or NUL",
        ));
    }
    Ok(())
}

/// A bound bot must exist; the id is stored in its canonical (lower-case,
/// hyphenated) form, the one `ToolContext::bot_id` carries.
pub(crate) fn canonical_bot(s: &AppState, raw: &str) -> Result<String, ApiError> {
    if raw.is_empty() || raw.len() > MAX_BOT_ID_LEN {
        return Err(ApiError::bad_request("malformed bot id"));
    }
    let id = raw
        .parse()
        .map_err(|_| ApiError::bad_request("malformed bot id"))?;
    Ok(s.bot_of_user(BotId(id))?.id.to_string())
}

fn check_input(s: &AppState, input: &mut EmailAccountInput) -> Result<(), ApiError> {
    if let Some(p) = &input.password {
        check_password(p)?;
    }
    if let Some(k) = &input.api_key {
        check_api_key(k)?;
    }
    if let Some(inbox) = &mut input.inbox {
        let trimmed = inbox.trim();
        if trimmed.is_empty() {
            input.inbox = None;
        } else if trimmed.len() > MAX_INBOX_BYTES || trimmed.chars().any(char::is_control) {
            return Err(ApiError::bad_request(
                "the inbox must be an address or id of at most 320 characters",
            ));
        } else {
            *inbox = trimmed.to_owned();
        }
    }
    if let Some(bot) = input.bot.take() {
        input.bot = Some(canonical_bot(s, &bot)?);
    }
    Ok(())
}

/// All accounts, with the non-secret fields only (`password_set` says whether
/// a password is stored; it is never returned).
#[utoipa::path(get, path = "/v1/email/accounts", tag = "email", security(("bearer" = [])),
    responses((status = 200, body = Vec<EmailAccountDto>),
              (status = 503, body = ErrorBody, description = "e-mail management is not available")))]
pub(crate) async fn list_email_accounts(
    State(s): State<AppState>,
) -> Result<Json<Vec<EmailAccountDto>>, ApiError> {
    Ok(Json(email(&s)?.list()))
}

/// Create an account. The `password` is stored in the secret store (never in
/// `email.toml`, never returned). `bot` binds the account to one bot (it must
/// exist); without it every bot that may use the e-mail tools can use it. Takes
/// effect for the next tool call, no restart needed.
#[utoipa::path(post, path = "/v1/email/accounts", tag = "email", security(("bearer" = [])),
    request_body = EmailAccountInput,
    responses((status = 201, body = EmailAccountDto),
              (status = 400, body = ErrorBody, description = "bad password or bot id"),
              (status = 404, body = ErrorBody, description = "unknown bot"),
              (status = 409, body = ErrorBody, description = "the name is taken"),
              (status = 422, body = ErrorBody, description = "invalid config"),
              (status = 503, body = ErrorBody, description = "e-mail management is not available")))]
pub(crate) async fn create_email_account(
    State(s): State<AppState>,
    ApiJson(mut input): ApiJson<EmailAccountInput>,
) -> Result<(StatusCode, Json<EmailAccountDto>), ApiError> {
    let control = email(&s)?;
    check_input(&s, &mut input)?;
    match input.provider {
        EmailProviderKind::ImapSmtp if input.password.is_none() => {
            return Err(ApiError::bad_request("`password` is required"));
        }
        EmailProviderKind::Agentmail if input.api_key.is_none() => {
            return Err(ApiError::bad_request("`api_key` is required"));
        }
        _ => {}
    }
    let created = control.add(input).await?;
    // The bot may have been deleted between the check and the insert (its
    // cleanup ran before the account existed): look again and take the account
    // back, so nothing stays bound to a bot that is gone.
    if let Some(bot) = &created.bot
        && let Err(e) = canonical_bot(&s, bot)
    {
        let _ = control.remove(&created.name).await;
        return Err(e);
    }
    // The owner just gave a bot this mailbox: it gets the mail tools with it.
    email_bot_tools::sync(&s, &control.list(), &email_bot_tools::bots_of(&created));
    Ok((StatusCode::CREATED, Json(created)))
}

/// Merge a partial config onto the stored account. Omitted fields keep their
/// values; `bot: null` makes the account usable by every bot; a `password`
/// replaces the stored one. Takes effect for the next tool call.
#[utoipa::path(put, path = "/v1/email/accounts/{name}", tag = "email", security(("bearer" = [])),
    params(("name" = String, Path, description = "Account name")),
    request_body = crate::email::EmailAccountPatch,
    responses((status = 200, body = EmailAccountDto),
              (status = 400, body = ErrorBody, description = "bad password or bot id"),
              (status = 404, body = ErrorBody),
              (status = 422, body = ErrorBody, description = "invalid config"),
              (status = 503, body = ErrorBody, description = "e-mail management is not available")))]
pub(crate) async fn update_email_account(
    State(s): State<AppState>,
    Path(name): Path<String>,
    ApiJson(mut patch): ApiJson<serde_json::Value>,
) -> Result<Json<EmailAccountDto>, ApiError> {
    let control = email(&s)?;
    if let Some(obj) = patch.as_object_mut() {
        match obj.get("password") {
            Some(serde_json::Value::String(p)) => check_password(p)?,
            Some(serde_json::Value::Null) | None => {}
            Some(_) => return Err(ApiError::bad_request("`password` must be a string")),
        }
        match obj.get("api_key") {
            Some(serde_json::Value::String(k)) => check_api_key(k)?,
            Some(serde_json::Value::Null) | None => {}
            Some(_) => return Err(ApiError::bad_request("`api_key` must be a string")),
        }
        match obj.get("bot") {
            Some(serde_json::Value::String(b)) => {
                let canon = canonical_bot(&s, b)?;
                obj.insert("bot".into(), serde_json::Value::String(canon));
            }
            Some(serde_json::Value::Null) | None => {}
            Some(_) => return Err(ApiError::bad_request("`bot` must be a string or null")),
        }
    }
    let before = control.list().into_iter().find(|a| a.name == name);
    let previous = before.as_ref().map(|a| a.bot.clone());
    let bound = patch
        .get("bot")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    // Reserve the bounded scan before mutating the mailbox. The detached task
    // owns both guards, so disconnecting the HTTP request cannot skip cleanup.
    let permit = crate::knowledge::worker_permit(s.data_dir())?;
    tokio::spawn(async move {
        let _knowledge_authority = crate::knowledge::email_authorities(&s, &name).await?;
        let updated = control.update(&name, patch).await?;
        if let Some(bot) = bound
            && let Err(e) = canonical_bot(&s, &bot)
        {
            if let Some(prev) = &previous {
                let _ = control
                    .update(&name, serde_json::json!({ "bot": prev }))
                    .await;
            }
            return Err(e);
        }
        if previous.as_ref() != Some(&updated.bot) {
            let state = s.clone();
            let account = name.clone();
            if let Err(error) = crate::knowledge::blocking_with_permit(permit, move || {
                crate::knowledge::revoke_email_sources(&state, &account)
            })
            .await
            {
                if let Some(prev) = &previous {
                    let _ = control
                        .update(&name, serde_json::json!({ "bot": prev }))
                        .await;
                }
                return Err(error);
            }
        }
        let mut affected = email_bot_tools::bots_of(&updated);
        if let Some(before) = &before {
            affected.extend(email_bot_tools::bots_of(before));
        }
        email_bot_tools::sync(&s, &control.list(), &affected);
        Ok(Json(updated))
    })
    .await
    .map_err(|_| ApiError::internal("email authority task failed"))?
}

/// Query of `DELETE /v1/email/accounts/{name}`.
#[derive(Debug, serde::Deserialize, utoipa::IntoParams)]
pub(crate) struct DeleteAccountQuery {
    /// Also delete the inbox at the provider (`agentmail` accounts). Default
    /// `false`: removing an account only forgets it locally.
    #[serde(default)]
    delete_inbox: bool,
}

/// Remove an account and the secret it owned. Takes effect for the next tool
/// call. With `delete_inbox=true` the inbox is first deleted at the provider
/// (irreversible); if that fails the account is kept (`502`) so it can be
/// retried. Deleting a bot never deletes inboxes at the provider.
#[utoipa::path(delete, path = "/v1/email/accounts/{name}", tag = "email",
    security(("bearer" = [])),
    params(("name" = String, Path, description = "Account name"), DeleteAccountQuery),
    responses((status = 204), (status = 404, body = ErrorBody),
              (status = 422, body = ErrorBody, description = "the account has no remote inbox"),
              (status = 502, body = ErrorBody, description = "the provider refused or failed"),
              (status = 503, body = ErrorBody, description = "e-mail management is not available")))]
pub(crate) async fn delete_email_account(
    State(s): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<DeleteAccountQuery>,
) -> Result<StatusCode, ApiError> {
    let control = email(&s)?;
    let permit = crate::knowledge::worker_permit(s.data_dir())?;
    tokio::spawn(async move {
        remove_account_with_links(&s, &control, &name, q.delete_inbox, permit).await?;
        Ok(StatusCode::NO_CONTENT)
    })
    .await
    .map_err(|_| ApiError::internal("email authority task failed"))?
}

/// Remove one account (and, when asked, its inbox at the provider), then everything that hung
/// off it: the knowledge sources read from it and the tools it gave bots. Shared by the account
/// route and the bot delete.
pub(crate) async fn remove_account_with_links(
    s: &AppState,
    control: &Arc<dyn EmailControl>,
    name: &str,
    delete_inbox: bool,
    permit: tokio::sync::OwnedSemaphorePermit,
) -> Result<(), ApiError> {
    let before = control.list().into_iter().find(|a| a.name == name);
    let _knowledge_authority = crate::knowledge::email_authorities(s, name).await?;
    control.remove_account(name, delete_inbox).await?;
    let s2 = s.clone();
    let owned = name.to_owned();
    crate::knowledge::blocking_with_permit(permit, move || {
        crate::knowledge::revoke_email_sources(&s2, &owned)
    })
    .await?;
    // The bots that owned this mailbox lose the tools it gave them (and
    // only those), unless another account still binds them.
    if let Some(before) = &before {
        email_bot_tools::sync(s, &control.list(), &email_bot_tools::bots_of(before));
    }
    Ok(())
}

/// Try IMAP login and SMTP authentication for a stored account. TLS exactly as
/// configured; **no mail is sent or read**. A failing server is a normal `200`
/// with a status per server (`auth_failed`, `tls`, `unreachable`, ...), never
/// the server's own words. At most two tests run at once (`429`).
#[utoipa::path(post, path = "/v1/email/accounts/{name}/test", tag = "email",
    security(("bearer" = [])),
    params(("name" = String, Path, description = "Account name")),
    responses((status = 200, body = EmailTestReport), (status = 404, body = ErrorBody),
              (status = 429, body = ErrorBody, description = "too many tests running"),
              (status = 503, body = ErrorBody, description = "e-mail management is not available")))]
pub(crate) async fn test_email_account(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<EmailTestReport>, ApiError> {
    let control = email(&s)?;
    let _permit = s.email_test_permit()?;
    Ok(Json(control.test_saved(&name).await?))
}

/// Same test for an account that is not saved yet (nothing is stored). With no
/// `password` in the body, the stored password of the account with the same
/// `name` is used. Same limits and the same coarse report as the saved test.
#[utoipa::path(post, path = "/v1/email/test", tag = "email", security(("bearer" = [])),
    request_body = EmailAccountInput,
    responses((status = 200, body = EmailTestReport),
              (status = 400, body = ErrorBody, description = "bad password or bot id"),
              (status = 422, body = ErrorBody, description = "invalid config or no password"),
              (status = 429, body = ErrorBody, description = "too many tests running"),
              (status = 503, body = ErrorBody, description = "e-mail management is not available")))]
pub(crate) async fn test_email_draft(
    State(s): State<AppState>,
    ApiJson(mut input): ApiJson<EmailAccountInput>,
) -> Result<Json<EmailTestReport>, ApiError> {
    let control = email(&s)?;
    // The binding plays no part in a test; do not require the bot to exist.
    input.bot = None;
    if let Some(p) = &input.password {
        check_password(p)?;
    }
    if let Some(k) = &input.api_key {
        check_api_key(k)?;
    }
    let _permit = s.email_test_permit()?;
    Ok(Json(control.test_draft(input).await?))
}

/// Most messages `GET .../messages` returns.
const MAX_MESSAGES: usize = 50;
/// Default page of `GET .../messages`.
const DEFAULT_MESSAGES: usize = 20;

/// Query of `GET /v1/email/accounts/{name}/messages`.
#[derive(Debug, serde::Deserialize, utoipa::IntoParams)]
pub(crate) struct MessagesQuery {
    /// How many messages, 1 to 50 (default 20).
    limit: Option<usize>,
}

/// The newest messages of an account's inbox, headers only and read-only
/// (nothing is marked read, no body is fetched). Subject and sender are
/// untrusted text from the sender. Shares the "at most two at once" limit of
/// the connection test (`429`).
#[utoipa::path(get, path = "/v1/email/accounts/{name}/messages", tag = "email",
    security(("bearer" = [])),
    params(("name" = String, Path, description = "Account name"), MessagesQuery),
    responses((status = 200, body = Vec<EmailMessageDto>), (status = 404, body = ErrorBody),
              (status = 422, body = ErrorBody, description = "no password stored, or not supported"),
              (status = 429, body = ErrorBody, description = "too many checks running"),
              (status = 502, body = ErrorBody, description = "the mailbox failed"),
              (status = 503, body = ErrorBody, description = "e-mail management is not available")))]
pub(crate) async fn list_email_messages(
    State(s): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<MessagesQuery>,
) -> Result<Json<Vec<EmailMessageDto>>, ApiError> {
    let control = email(&s)?;
    let _permit = s.email_test_permit()?;
    let limit = q.limit.unwrap_or(DEFAULT_MESSAGES).clamp(1, MAX_MESSAGES);
    Ok(Json(control.recent_messages(&name, limit).await?))
}

/// Send one fixed test message from the account to **its own address**: no
/// recipient, subject or body can be given, so it can only ever reach the
/// mailbox itself. Same limit as the connection test (`429`).
#[utoipa::path(post, path = "/v1/email/accounts/{name}/send-test", tag = "email",
    security(("bearer" = [])),
    params(("name" = String, Path, description = "Account name")),
    responses((status = 200, body = EmailSendTestResponse), (status = 404, body = ErrorBody),
              (status = 422, body = ErrorBody, description = "no password stored, or not supported"),
              (status = 429, body = ErrorBody, description = "too many checks running"),
              (status = 502, body = ErrorBody, description = "the mailbox failed"),
              (status = 503, body = ErrorBody, description = "e-mail management is not available")))]
pub(crate) async fn send_email_test(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<EmailSendTestResponse>, ApiError> {
    let control = email(&s)?;
    let _permit = s.email_test_permit()?;
    Ok(Json(EmailSendTestResponse {
        sent_to: control.send_test(&name).await?,
    }))
}

/// Default and largest number of send-log lines in one answer.
const DEFAULT_LOG: usize = 200;
const MAX_LOG: usize = 5000;

/// Query of `GET /v1/email/guard/send-log`.
#[derive(Debug, serde::Deserialize, utoipa::IntoParams)]
pub(crate) struct SendLogQuery {
    /// How many lines, newest first (default 200, at most 5000).
    limit: Option<usize>,
}

/// The values the outgoing-mail guard uses (owner only).
#[utoipa::path(get, path = "/v1/email/guard/settings", tag = "email",
    security(("bearer" = [])),
    responses((status = 200, body = EmailGuardSettingsDto), (status = 503, body = ErrorBody)))]
pub(crate) async fn get_guard_settings(
    State(s): State<AppState>,
) -> Result<Json<EmailGuardSettingsDto>, ApiError> {
    Ok(Json(email(&s)?.guard_settings()?))
}

/// Change the guard values (owner only).
#[utoipa::path(put, path = "/v1/email/guard/settings", tag = "email",
    security(("bearer" = [])), request_body = EmailGuardSettingsDto,
    responses((status = 200, body = EmailGuardSettingsDto), (status = 422, body = ErrorBody),
              (status = 503, body = ErrorBody)))]
pub(crate) async fn put_guard_settings(
    State(s): State<AppState>,
    ApiJson(body): ApiJson<EmailGuardSettingsDto>,
) -> Result<Json<EmailGuardSettingsDto>, ApiError> {
    Ok(Json(email(&s)?.set_guard_settings(body)?))
}

/// The opt-out list (owner only).
#[utoipa::path(get, path = "/v1/email/guard/opt-out", tag = "email",
    security(("bearer" = [])),
    responses((status = 200, body = Vec<EmailOptOutDto>), (status = 503, body = ErrorBody)))]
pub(crate) async fn list_opt_outs(
    State(s): State<AppState>,
) -> Result<Json<Vec<EmailOptOutDto>>, ApiError> {
    Ok(Json(email(&s)?.opt_outs()?))
}

/// Add an address or domain to the opt-out list (owner only).
#[utoipa::path(post, path = "/v1/email/guard/opt-out", tag = "email",
    security(("bearer" = [])), request_body = EmailOptOutInput,
    responses((status = 200, body = EmailOptOutDto), (status = 422, body = ErrorBody),
              (status = 503, body = ErrorBody)))]
pub(crate) async fn add_opt_out(
    State(s): State<AppState>,
    ApiJson(body): ApiJson<EmailOptOutInput>,
) -> Result<Json<EmailOptOutDto>, ApiError> {
    Ok(Json(email(&s)?.add_opt_out(body)?))
}

/// Remove an entry from the opt-out list (owner only).
#[utoipa::path(delete, path = "/v1/email/guard/opt-out/{value}", tag = "email",
    security(("bearer" = [])),
    params(("value" = String, Path, description = "The address or domain")),
    responses((status = 204), (status = 404, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub(crate) async fn remove_opt_out(
    State(s): State<AppState>,
    Path(value): Path<String>,
) -> Result<StatusCode, ApiError> {
    email(&s)?.remove_opt_out(&value)?;
    Ok(StatusCode::NO_CONTENT)
}

/// The send log, newest first (owner only: it lists recipients).
#[utoipa::path(get, path = "/v1/email/guard/send-log", tag = "email",
    security(("bearer" = [])), params(SendLogQuery),
    responses((status = 200, body = Vec<EmailSendLogDto>), (status = 503, body = ErrorBody)))]
pub(crate) async fn get_send_log(
    State(s): State<AppState>,
    Query(q): Query<SendLogQuery>,
) -> Result<Json<Vec<EmailSendLogDto>>, ApiError> {
    let limit = q.limit.unwrap_or(DEFAULT_LOG).clamp(1, MAX_LOG);
    Ok(Json(email(&s)?.send_log(limit)?))
}

/// The whole send log as a CSV file (owner only).
#[utoipa::path(get, path = "/v1/email/guard/send-log.csv", tag = "email",
    security(("bearer" = [])),
    responses((status = 200, content_type = "text/csv", body = String), (status = 503, body = ErrorBody)))]
pub(crate) async fn export_send_log(
    State(s): State<AppState>,
) -> Result<impl axum::response::IntoResponse, ApiError> {
    let csv = email(&s)?.send_log_csv()?;
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, "text/csv; charset=utf-8"),
            (
                axum::http::header::CONTENT_DISPOSITION,
                "attachment; filename=\"send-log.csv\"",
            ),
        ],
        csv,
    ))
}
