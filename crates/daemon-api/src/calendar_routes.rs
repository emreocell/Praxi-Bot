//! Handlers of `/v1/calendar/*`. Same guard (Host / Origin / bearer) as every
//! other route; the password is write-only.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;

use crate::AppState;
use crate::calendar::{
    CalendarAccountDto, CalendarAccountInput, CalendarControl, CalendarControlError,
    CalendarTestInput, CalendarTestReport,
};
use crate::error::{ApiError, ErrorBody};
use crate::routes::ApiJson;

/// Longest accepted password (bytes).
const MAX_PASSWORD_BYTES: usize = 1024;

impl From<CalendarControlError> for ApiError {
    fn from(e: CalendarControlError) -> Self {
        match e {
            CalendarControlError::NotFound(w) => Self::not_found(w),
            CalendarControlError::Conflict(m) => Self::conflict(m),
            CalendarControlError::Invalid(m) => {
                Self::new(StatusCode::UNPROCESSABLE_ENTITY, "bad_calendar_config", m)
            }
            CalendarControlError::Internal(m) => Self::internal(m),
        }
    }
}

fn calendar(s: &AppState) -> Result<Arc<dyn CalendarControl>, ApiError> {
    s.calendar()
        .ok_or_else(|| ApiError::unavailable("calendar management is not available in this daemon"))
}

fn check_password(p: &str) -> Result<(), ApiError> {
    if p.is_empty() || p.len() > MAX_PASSWORD_BYTES || p.contains(['\0', '\r', '\n']) {
        return Err(ApiError::bad_request(
            "the password must be 1 to 1024 bytes without line breaks or NUL",
        ));
    }
    Ok(())
}

/// The saved calendar accounts, non-secret fields only (`password_set` says
/// whether a password is stored; it is never returned).
#[utoipa::path(get, path = "/v1/calendar/accounts", tag = "calendar", security(("bearer" = [])),
    responses((status = 200, body = Vec<CalendarAccountDto>),
              (status = 503, body = ErrorBody, description = "calendar management is not available")))]
pub(crate) async fn list_calendar_accounts(
    State(s): State<AppState>,
) -> Result<Json<Vec<CalendarAccountDto>>, ApiError> {
    Ok(Json(calendar(&s)?.list()))
}

/// Add a calendar account. The `password` goes to the secret store (never to
/// `calendar.toml`, never returned). Takes effect for the next tool call, no
/// restart needed.
#[utoipa::path(post, path = "/v1/calendar/accounts", tag = "calendar", security(("bearer" = [])),
    request_body = CalendarAccountInput,
    responses((status = 201, body = CalendarAccountDto),
              (status = 400, body = ErrorBody, description = "bad password"),
              (status = 409, body = ErrorBody, description = "the name is taken"),
              (status = 422, body = ErrorBody, description = "invalid config or the server refused the login"),
              (status = 503, body = ErrorBody, description = "calendar management is not available")))]
pub(crate) async fn create_calendar_account(
    State(s): State<AppState>,
    ApiJson(input): ApiJson<CalendarAccountInput>,
) -> Result<(StatusCode, Json<CalendarAccountDto>), ApiError> {
    let control = calendar(&s)?;
    match &input.password {
        Some(p) => check_password(p)?,
        None => return Err(ApiError::bad_request("`password` is required")),
    }
    Ok((StatusCode::CREATED, Json(control.create(input).await?)))
}

/// Change an account. Without `password` the stored one is kept.
#[utoipa::path(put, path = "/v1/calendar/accounts/{name}", tag = "calendar", security(("bearer" = [])),
    params(("name" = String, Path, description = "account name")),
    request_body = CalendarAccountInput,
    responses((status = 200, body = CalendarAccountDto),
              (status = 400, body = ErrorBody, description = "bad password"),
              (status = 404, body = ErrorBody, description = "no such account"),
              (status = 422, body = ErrorBody, description = "invalid config or the server refused the login"),
              (status = 503, body = ErrorBody, description = "calendar management is not available")))]
pub(crate) async fn update_calendar_account(
    State(s): State<AppState>,
    Path(name): Path<String>,
    ApiJson(input): ApiJson<CalendarAccountInput>,
) -> Result<Json<CalendarAccountDto>, ApiError> {
    let control = calendar(&s)?;
    if let Some(p) = &input.password {
        check_password(p)?;
    }
    Ok(Json(control.update(&name, input).await?))
}

/// Remove an account and its stored password.
#[utoipa::path(delete, path = "/v1/calendar/accounts/{name}", tag = "calendar", security(("bearer" = [])),
    params(("name" = String, Path, description = "account name")),
    responses((status = 204, description = "removed"),
              (status = 404, body = ErrorBody, description = "no such account"),
              (status = 503, body = ErrorBody, description = "calendar management is not available")))]
pub(crate) async fn delete_calendar_account(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> Result<StatusCode, ApiError> {
    calendar(&s)?.delete(&name).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Try a login and list the calendars behind it: a saved account (`name`) or an
/// unsaved login (`url`, `username`, `password`). A server that cannot be
/// reached or refuses the login answers `200` with `ok: false` and a plain
/// message. At most two connection tests run at once (`429`).
#[utoipa::path(post, path = "/v1/calendar/test", tag = "calendar", security(("bearer" = [])),
    request_body = CalendarTestInput,
    responses((status = 200, body = CalendarTestReport),
              (status = 400, body = ErrorBody, description = "bad password"),
              (status = 422, body = ErrorBody, description = "nothing to test"),
              (status = 429, body = ErrorBody, description = "too many tests running"),
              (status = 503, body = ErrorBody, description = "calendar management is not available")))]
pub(crate) async fn test_calendar(
    State(s): State<AppState>,
    ApiJson(input): ApiJson<CalendarTestInput>,
) -> Result<Json<CalendarTestReport>, ApiError> {
    let control = calendar(&s)?;
    if let Some(p) = &input.password {
        check_password(p)?;
    }
    let _permit = s.email_test_permit()?;
    Ok(Json(control.test(input).await?))
}
