//! Handlers of `/v1/openapi-apps/*`.  Owner-only (`OWNER_ROUTES`; every paired
//! role except the owner's desktop is refused).  Every mutation writes its
//! audit row first and fails closed when it cannot.  Audit rows carry ids and
//! counts, never a spec, a credential or a header.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde_json::json;

use crate::AppState;
use crate::error::{ApiError, ErrorBody};
use crate::live_view::Actor;
use crate::openapi_apps::{
    OpenApiAppDto, OpenApiAppInput, OpenApiAppUpdate, OpenApiAppsControl, OpenApiAppsError,
    OpenApiAuthInput,
};
use crate::routes::ApiJson;

/// Longest accepted spec document (bytes), the importer's own limit.
pub(crate) const MAX_SPEC_BYTES: usize = 8 * 1024 * 1024;
const MAX_SECRET_BYTES: usize = 4096;

impl From<OpenApiAppsError> for ApiError {
    fn from(e: OpenApiAppsError) -> Self {
        match e {
            OpenApiAppsError::NotFound(w) => Self::not_found(w),
            // The generic codes already have words in every language; the message
            // (a plain sentence) says what exactly was wrong.
            OpenApiAppsError::Invalid(m) => {
                Self::new(StatusCode::UNPROCESSABLE_ENTITY, "bad_request", m)
            }
            OpenApiAppsError::Upstream(m) => Self::new(StatusCode::BAD_GATEWAY, "unavailable", m),
            OpenApiAppsError::Internal(m) => Self::internal(m),
        }
    }
}

fn control(s: &AppState) -> Result<Arc<dyn OpenApiAppsControl>, ApiError> {
    s.openapi_apps().ok_or_else(|| {
        ApiError::unavailable("connecting REST APIs is not available in this daemon")
    })
}

fn check_id(id: &str) -> Result<(), ApiError> {
    let ok = !id.is_empty()
        && id.len() <= 32
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    ok.then_some(())
        .ok_or_else(|| ApiError::bad_request("malformed app id"))
}

fn check_auth(auth: &OpenApiAuthInput) -> Result<(), ApiError> {
    if let Some(secret) = &auth.secret
        && (secret.is_empty()
            || secret.len() > MAX_SECRET_BYTES
            || secret.contains(['\0', '\r', '\n']))
    {
        return Err(ApiError::bad_request(
            "the credential must be 1 to 4096 bytes without line breaks or NUL",
        ));
    }
    Ok(())
}

/// Audit row, written before the change. Fails closed.
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

/// The connected REST APIs and the operations found in their documents.
#[utoipa::path(get, path = "/v1/openapi-apps", tag = "openapi-apps", security(("bearer" = [])),
    responses((status = 200, body = Vec<OpenApiAppDto>),
              (status = 503, body = ErrorBody, description = "not available in this daemon")))]
pub(crate) async fn list_openapi_apps(
    State(s): State<AppState>,
) -> Result<Json<Vec<OpenApiAppDto>>, ApiError> {
    Ok(Json(control(&s)?.list()))
}

/// One connected REST API.
#[utoipa::path(get, path = "/v1/openapi-apps/{id}", tag = "openapi-apps", security(("bearer" = [])),
    params(("id" = String, Path, description = "app id")),
    responses((status = 200, body = OpenApiAppDto),
              (status = 404, body = ErrorBody, description = "no such app"),
              (status = 503, body = ErrorBody, description = "not available in this daemon")))]
pub(crate) async fn get_openapi_app(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<OpenApiAppDto>, ApiError> {
    check_id(&id)?;
    Ok(Json(control(&s)?.get(&id)?))
}

/// Connect a REST API from its `OpenAPI` 3.x document (`spec_url` or `spec_text`).
/// The credential goes to the secret store and is never returned. Requests only
/// ever go to `base_url`'s host; redirects to other hosts are not followed.
/// Every operation starts disabled.
#[utoipa::path(post, path = "/v1/openapi-apps", tag = "openapi-apps", security(("bearer" = [])),
    request_body = OpenApiAppInput,
    responses((status = 201, body = OpenApiAppDto),
              (status = 400, body = ErrorBody, description = "malformed input"),
              (status = 422, body = ErrorBody, description = "unusable document, base URL or address"),
              (status = 502, body = ErrorBody, description = "the document could not be downloaded"),
              (status = 503, body = ErrorBody, description = "not available in this daemon")))]
pub(crate) async fn add_openapi_app(
    State(s): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    ApiJson(input): ApiJson<OpenApiAppInput>,
) -> Result<(StatusCode, Json<OpenApiAppDto>), ApiError> {
    let control = control(&s)?;
    if input.name.trim().is_empty() || input.name.chars().count() > 80 {
        return Err(ApiError::bad_request("`name` must be 1 to 80 characters"));
    }
    match (&input.spec_url, &input.spec_text) {
        (Some(_), Some(_)) | (None, None) => {
            return Err(ApiError::bad_request(
                "give exactly one of `spec_url` and `spec_text`",
            ));
        }
        (None, Some(t)) if t.len() > MAX_SPEC_BYTES => {
            return Err(ApiError::bad_request("the document is larger than 8 MiB"));
        }
        _ => {}
    }
    if let Some(auth) = &input.auth {
        check_auth(auth)?;
    }
    audit(
        &s,
        &actor,
        "openapi_app.added",
        &json!({ "name": input.name.trim(), "from_url": input.spec_url.is_some() }),
    )?;
    let dto = control.add(input).await?;
    Ok((StatusCode::CREATED, Json(dto)))
}

/// Change the name, base URL or credentials, or the set of enabled operations
/// and the bots that may use each. Applied to the bots' tools at once.
#[utoipa::path(put, path = "/v1/openapi-apps/{id}", tag = "openapi-apps", security(("bearer" = [])),
    params(("id" = String, Path, description = "app id")),
    request_body = OpenApiAppUpdate,
    responses((status = 200, body = OpenApiAppDto),
              (status = 400, body = ErrorBody, description = "malformed input"),
              (status = 404, body = ErrorBody, description = "no such app"),
              (status = 422, body = ErrorBody, description = "unknown operation, too many enabled or bad base URL"),
              (status = 503, body = ErrorBody, description = "not available in this daemon")))]
pub(crate) async fn update_openapi_app(
    State(s): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    Path(id): Path<String>,
    ApiJson(input): ApiJson<OpenApiAppUpdate>,
) -> Result<Json<OpenApiAppDto>, ApiError> {
    check_id(&id)?;
    let control = control(&s)?;
    if let Some(auth) = &input.auth {
        check_auth(auth)?;
    }
    audit(
        &s,
        &actor,
        "openapi_app.updated",
        &json!({
            "id": id,
            "operations": input.operations.as_ref().map(Vec::len),
            "base_url_changed": input.base_url.is_some(),
            "auth_changed": input.auth.is_some(),
        }),
    )?;
    Ok(Json(control.update(&id, input).await?))
}

/// Download the document again (apps added from a URL). Enabled operations that
/// still exist stay enabled.
#[utoipa::path(post, path = "/v1/openapi-apps/{id}/refresh", tag = "openapi-apps", security(("bearer" = [])),
    params(("id" = String, Path, description = "app id")),
    responses((status = 200, body = OpenApiAppDto),
              (status = 404, body = ErrorBody, description = "no such app"),
              (status = 422, body = ErrorBody, description = "the app has no document URL or the document is unusable"),
              (status = 502, body = ErrorBody, description = "the document could not be downloaded"),
              (status = 503, body = ErrorBody, description = "not available in this daemon")))]
pub(crate) async fn refresh_openapi_app(
    State(s): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    Path(id): Path<String>,
) -> Result<Json<OpenApiAppDto>, ApiError> {
    check_id(&id)?;
    let control = control(&s)?;
    audit(&s, &actor, "openapi_app.refreshed", &json!({ "id": id }))?;
    Ok(Json(control.refresh(&id).await?))
}

/// Disconnect an API: its tools, its saved document and its credential are removed.
#[utoipa::path(delete, path = "/v1/openapi-apps/{id}", tag = "openapi-apps", security(("bearer" = [])),
    params(("id" = String, Path, description = "app id")),
    responses((status = 204, description = "removed"),
              (status = 404, body = ErrorBody, description = "no such app"),
              (status = 503, body = ErrorBody, description = "not available in this daemon")))]
pub(crate) async fn remove_openapi_app(
    State(s): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    check_id(&id)?;
    let control = control(&s)?;
    audit(&s, &actor, "openapi_app.removed", &json!({ "id": id }))?;
    control.remove(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}
