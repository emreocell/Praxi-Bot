//! Handlers of `/v1/email/*`. Same guard (Host / Origin / bearer) as every other
//! route; passwords are write-only.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;

use crate::AppState;
use crate::email::{
    EmailAccountDto, EmailAccountInput, EmailControl, EmailControlError, EmailTestReport,
};
use crate::error::{ApiError, ErrorBody};
use crate::mcp_routes::MAX_BOT_ID_LEN;
use crate::routes::ApiJson;
use protocol::BotId;

/// Longest accepted password (bytes).
const MAX_PASSWORD_BYTES: usize = 1024;

impl From<EmailControlError> for ApiError {
    fn from(e: EmailControlError) -> Self {
        match e {
            EmailControlError::NotFound(w) => Self::not_found(w),
            EmailControlError::Conflict(m) => Self::conflict(m),
            EmailControlError::Invalid(m) => {
                Self::new(StatusCode::UNPROCESSABLE_ENTITY, "bad_email_config", m)
            }
            EmailControlError::Internal(m) => Self::internal(m),
        }
    }
}

fn email(s: &AppState) -> Result<Arc<dyn EmailControl>, ApiError> {
    s.email()
        .ok_or_else(|| ApiError::unavailable("e-mail management is not available in this daemon"))
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
fn canonical_bot(s: &AppState, raw: &str) -> Result<String, ApiError> {
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
    if input.password.is_none() {
        return Err(ApiError::bad_request("`password` is required"));
    }
    Ok((StatusCode::CREATED, Json(control.add(input).await?)))
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
        match obj.get("bot") {
            Some(serde_json::Value::String(b)) => {
                let canon = canonical_bot(&s, b)?;
                obj.insert("bot".into(), serde_json::Value::String(canon));
            }
            Some(serde_json::Value::Null) | None => {}
            Some(_) => return Err(ApiError::bad_request("`bot` must be a string or null")),
        }
    }
    Ok(Json(control.update(&name, patch).await?))
}

/// Remove an account and the password it owned. Takes effect for the next tool
/// call.
#[utoipa::path(delete, path = "/v1/email/accounts/{name}", tag = "email",
    security(("bearer" = [])),
    params(("name" = String, Path, description = "Account name")),
    responses((status = 204), (status = 404, body = ErrorBody),
              (status = 503, body = ErrorBody, description = "e-mail management is not available")))]
pub(crate) async fn delete_email_account(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> Result<StatusCode, ApiError> {
    email(&s)?.remove(&name).await?;
    Ok(StatusCode::NO_CONTENT)
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
    let _permit = s.email_test_permit()?;
    Ok(Json(control.test_draft(input).await?))
}
