//! Handlers of `/v1/homeassistant*`. Same guard (Host / Origin / bearer) as every other route, and
//! every one is owner-only (`owner::OWNER_ROUTES`); the access token is write-only.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde_json::json;

use crate::AppState;
use crate::error::{ApiError, ErrorBody};
use crate::homeassistant::{
    HomeAssistantBotsInput, HomeAssistantControl, HomeAssistantControlError, HomeAssistantDto,
    HomeAssistantInput, HomeAssistantSaved, HomeAssistantTestInput, HomeAssistantTestReport,
};
use crate::live_view::Actor;
use crate::routes::ApiJson;

/// Longest accepted token (bytes). Home Assistant tokens are JWTs of a few hundred bytes.
const MAX_TOKEN_BYTES: usize = 4096;
/// Most services in the owner's list.
const MAX_TRUSTED: usize = 64;
/// Most bots in an access list.
const MAX_BOTS: usize = 256;

impl From<HomeAssistantControlError> for ApiError {
    fn from(e: HomeAssistantControlError) -> Self {
        match e {
            HomeAssistantControlError::NotFound(w) => Self::not_found(w),
            HomeAssistantControlError::Invalid(m) => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "bad_homeassistant_config",
                m,
            ),
            HomeAssistantControlError::Internal(m) => Self::internal(m),
        }
    }
}

fn control(s: &AppState) -> Result<Arc<dyn HomeAssistantControl>, ApiError> {
    s.homeassistant().ok_or_else(|| {
        ApiError::unavailable("Home Assistant setup is not available in this daemon")
    })
}

fn check_token(t: &str) -> Result<(), ApiError> {
    if t.is_empty() || t.len() > MAX_TOKEN_BYTES || t.contains(['\0', '\r', '\n', ' ']) {
        return Err(ApiError::bad_request(
            "the access key must be 1 to 4096 characters without spaces or line breaks",
        ));
    }
    Ok(())
}

/// Audit row, written before the change. Fails closed. Never carries the address or the token.
fn audit(
    s: &AppState,
    actor: &Actor,
    action: &str,
    detail: &serde_json::Value,
) -> Result<(), ApiError> {
    s.storage()
        .append_audit(s.user_id(), None, &actor.key, action, detail)
        .map(|_| ())
        .map_err(|_| ApiError::unavailable("audit log unavailable; nothing was changed"))
}

/// The saved Home Assistant setup: the address, whether a token is stored (never the token), the
/// owner's list of services bots may call without a card, and which bots may use it. Owner only.
#[utoipa::path(get, path = "/v1/homeassistant", tag = "homeassistant", security(("bearer" = [])),
    responses((status = 200, body = HomeAssistantDto),
              (status = 403, body = ErrorBody, description = "owner key required"),
              (status = 503, body = ErrorBody, description = "Home Assistant setup is not available")))]
pub(crate) async fn get_homeassistant(
    State(s): State<AppState>,
) -> Result<Json<HomeAssistantDto>, ApiError> {
    Ok(Json(control(&s)?.get()))
}

/// Check the hub (`GET /api/`, then count its entities), and only then save it. The `token` goes to
/// the secret store (never to `homeassistant.toml`, never returned). Takes effect for the next
/// tool call, no restart needed. Without `token` the stored one is kept, but only when the address
/// is unchanged. Owner only.
#[utoipa::path(put, path = "/v1/homeassistant", tag = "homeassistant", security(("bearer" = [])),
    request_body = HomeAssistantInput,
    responses((status = 200, body = HomeAssistantSaved),
              (status = 400, body = ErrorBody, description = "bad token or list"),
              (status = 403, body = ErrorBody, description = "owner key required"),
              (status = 422, body = ErrorBody, description = "invalid address, or the hub refused the token"),
              (status = 429, body = ErrorBody, description = "too many tests running"),
              (status = 503, body = ErrorBody, description = "Home Assistant setup is not available")))]
pub(crate) async fn put_homeassistant(
    State(s): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    ApiJson(input): ApiJson<HomeAssistantInput>,
) -> Result<Json<HomeAssistantSaved>, ApiError> {
    let c = control(&s)?;
    if let Some(t) = &input.token {
        check_token(t)?;
    }
    if input.trusted_services.len() > MAX_TRUSTED {
        return Err(ApiError::bad_request(
            "the list of allowed services is too long",
        ));
    }
    let _permit = s.email_test_permit()?;
    audit(
        &s,
        &actor,
        "homeassistant.save",
        &json!({"token_changed": input.token.is_some(),
                "trusted": input.trusted_services.len()}),
    )?;
    Ok(Json(c.save(input).await?))
}

/// Choose which bots may use Home Assistant. `bots: null` = every bot, `[]` = none. Owner only.
#[utoipa::path(put, path = "/v1/homeassistant/bots", tag = "homeassistant", security(("bearer" = [])),
    request_body = HomeAssistantBotsInput,
    responses((status = 200, body = HomeAssistantDto),
              (status = 400, body = ErrorBody, description = "too many bots"),
              (status = 403, body = ErrorBody, description = "owner key required"),
              (status = 404, body = ErrorBody, description = "Home Assistant is not set up"),
              (status = 503, body = ErrorBody, description = "Home Assistant setup is not available")))]
pub(crate) async fn put_homeassistant_bots(
    State(s): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    ApiJson(input): ApiJson<HomeAssistantBotsInput>,
) -> Result<Json<HomeAssistantDto>, ApiError> {
    let c = control(&s)?;
    if input.bots.as_ref().is_some_and(|b| b.len() > MAX_BOTS) {
        return Err(ApiError::bad_request("too many bots"));
    }
    audit(
        &s,
        &actor,
        "homeassistant.bots",
        &json!({"all": input.bots.is_none(), "count": input.bots.as_ref().map(Vec::len)}),
    )?;
    Ok(Json(c.set_bots(input.bots).await?))
}

/// Remove the setup and its stored token. The `ha_*` tools disappear from every bot at once.
/// Owner only.
#[utoipa::path(delete, path = "/v1/homeassistant", tag = "homeassistant", security(("bearer" = [])),
    responses((status = 204, description = "removed"),
              (status = 403, body = ErrorBody, description = "owner key required"),
              (status = 404, body = ErrorBody, description = "Home Assistant is not set up"),
              (status = 503, body = ErrorBody, description = "Home Assistant setup is not available")))]
pub(crate) async fn delete_homeassistant(
    State(s): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
) -> Result<StatusCode, ApiError> {
    let c = control(&s)?;
    audit(&s, &actor, "homeassistant.remove", &json!({}))?;
    c.remove().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Try the saved hub (an empty body), or an address and token that are not saved yet (`url` and
/// `token`), and count its entities. A hub that cannot be reached or refuses the token answers `200`
/// with `ok: false` and a plain message. The daemon only ever dials the address given here or the
/// saved one. At most two connection tests run at once (`429`). Owner only.
#[utoipa::path(post, path = "/v1/homeassistant/test", tag = "homeassistant", security(("bearer" = [])),
    request_body = HomeAssistantTestInput,
    responses((status = 200, body = HomeAssistantTestReport),
              (status = 400, body = ErrorBody, description = "bad token"),
              (status = 403, body = ErrorBody, description = "owner key required"),
              (status = 404, body = ErrorBody, description = "nothing saved to test"),
              (status = 422, body = ErrorBody, description = "an address needs a token, and the other way round"),
              (status = 429, body = ErrorBody, description = "too many tests running"),
              (status = 503, body = ErrorBody, description = "Home Assistant setup is not available")))]
pub(crate) async fn test_homeassistant(
    State(s): State<AppState>,
    ApiJson(input): ApiJson<HomeAssistantTestInput>,
) -> Result<Json<HomeAssistantTestReport>, ApiError> {
    let c = control(&s)?;
    if let Some(t) = &input.token {
        check_token(t)?;
    }
    let _permit = s.email_test_permit()?;
    Ok(Json(c.test(input).await?))
}
