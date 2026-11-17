//! Handlers of `/v1/composio/*`.  Owner-only (the peer guard refuses every
//! peer role but Desktop).  Every mutation writes its audit row first and fails
//! closed when it cannot.  The audit details never carry the key.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde_json::json;

use crate::AppState;
use crate::composio::{
    ComposioAppDto, ComposioCatalogDto, ComposioCatalogQuery, ComposioConnectDto, ComposioControl,
    ComposioControlError, ComposioDisconnectDto, ComposioEnabledInput, ComposioKeyInput,
    ComposioLoginDto, ComposioLoginStatusDto, ComposioStatusDto,
};
use crate::error::{ApiError, ErrorBody};
use crate::live_view::Actor;
use crate::routes::ApiJson;

impl From<ComposioControlError> for ApiError {
    fn from(e: ComposioControlError) -> Self {
        match e {
            ComposioControlError::NotFound(w) => Self::not_found(w),
            ComposioControlError::Invalid(m) => {
                Self::new(StatusCode::UNPROCESSABLE_ENTITY, "bad_composio_request", m)
            }
            ComposioControlError::NotConfigured => Self::new(
                StatusCode::CONFLICT,
                "composio_not_configured",
                "Composio is not linked yet: start the one-time login first",
            ),
            ComposioControlError::RateLimited(m) => {
                Self::new(StatusCode::TOO_MANY_REQUESTS, "composio_rate_limited", m)
            }
            ComposioControlError::Upstream(m) => {
                Self::new(StatusCode::BAD_GATEWAY, "composio_error", m)
            }
            ComposioControlError::Internal(m) => Self::internal(m),
        }
    }
}

fn composio(s: &AppState) -> Result<Arc<dyn ComposioControl>, ApiError> {
    s.composio()
        .ok_or_else(|| ApiError::unavailable("Composio is not available in this daemon"))
}

/// `GET /v1/composio/logos/{slug}`: the app's logo, fetched and validated by the daemon (the
/// webview's CSP allows no remote images). Authenticated like every route; the bytes are served
/// with `nosniff` and a CSP that forbids anything active even if a viewer rendered them as a page.
pub(crate) async fn composio_logo(
    State(s): State<AppState>,
    Path(slug): Path<String>,
) -> Result<axum::response::Response, ApiError> {
    check_slug(&slug)?;
    let logo = composio(&s)?
        .logo(&slug)
        .await?
        .ok_or_else(|| ApiError::not_found("logo"))?;
    axum::response::Response::builder()
        .status(StatusCode::OK)
        .header("content-type", logo.content_type)
        .header("x-content-type-options", "nosniff")
        .header(
            "content-security-policy",
            "default-src 'none'; style-src 'unsafe-inline'; sandbox",
        )
        .header("cache-control", "private, max-age=86400")
        .body(axum::body::Body::from(logo.bytes))
        .map_err(ApiError::internal)
}

/// `app` slugs are `[a-z0-9_]`, at most 64 characters.
fn check_slug(slug: &str) -> Result<(), ApiError> {
    if slug.is_empty()
        || slug.len() > 64
        || !slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return Err(ApiError::bad_request("malformed app slug"));
    }
    Ok(())
}

/// Audit row, written before the change.  Fails closed.
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

/// Key status.
#[utoipa::path(get, path = "/v1/composio", tag = "composio", security(("bearer" = [])),
    responses((status = 200, body = ComposioStatusDto),
              (status = 503, body = ErrorBody, description = "Composio is not available")))]
pub(crate) async fn composio_status(
    State(s): State<AppState>,
) -> Result<Json<ComposioStatusDto>, ApiError> {
    Ok(Json(composio(&s)?.status()))
}

/// Start the one-time browser login: Composio makes a session, the caller
/// opens `login_url` in the system browser and polls `GET /v1/composio/login`.
/// No key is typed or copied.
#[utoipa::path(post, path = "/v1/composio/login", tag = "composio", security(("bearer" = [])),
    responses((status = 200, body = ComposioLoginDto),
              (status = 502, body = ErrorBody, description = "Composio unreachable")))]
pub(crate) async fn composio_login_start(
    State(s): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
) -> Result<Json<ComposioLoginDto>, ApiError> {
    let c = composio(&s)?;
    audit(&s, &actor, "composio.login_started", &json!({}))?;
    Ok(Json(c.login_start().await?))
}

/// State of the browser login (poll while it is `pending`).
#[utoipa::path(get, path = "/v1/composio/login", tag = "composio", security(("bearer" = [])),
    responses((status = 200, body = ComposioLoginStatusDto)))]
pub(crate) async fn composio_login_status(
    State(s): State<AppState>,
) -> Result<Json<ComposioLoginStatusDto>, ApiError> {
    Ok(Json(composio(&s)?.login_poll().await?))
}

/// Validate (one cheap authenticated call to Composio) and store the project
/// API key in the OS secret store.  **Write-only**: no route returns it.
#[utoipa::path(put, path = "/v1/composio/key", tag = "composio", security(("bearer" = [])),
    request_body = ComposioKeyInput,
    responses((status = 200, body = ComposioStatusDto),
              (status = 422, body = ErrorBody, description = "malformed or rejected key"),
              (status = 502, body = ErrorBody, description = "Composio unreachable")))]
pub(crate) async fn composio_set_key(
    State(s): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    ApiJson(input): ApiJson<ComposioKeyInput>,
) -> Result<Json<ComposioStatusDto>, ApiError> {
    let c = composio(&s)?;
    audit(&s, &actor, "composio.key_set", &json!({}))?;
    c.set_key(&input.value).await?;
    Ok(Json(c.status()))
}

/// Delete the key; every Composio tool disappears.
#[utoipa::path(delete, path = "/v1/composio/key", tag = "composio", security(("bearer" = [])),
    responses((status = 200, body = ComposioStatusDto)))]
pub(crate) async fn composio_remove_key(
    State(s): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
) -> Result<Json<ComposioStatusDto>, ApiError> {
    let c = composio(&s)?;
    audit(&s, &actor, "composio.key_removed", &json!({}))?;
    c.remove_key().await?;
    Ok(Json(c.status()))
}

fn short(field: &str, v: Option<&str>, max: usize) -> Result<(), ApiError> {
    if v.is_some_and(|x| x.len() > max || x.chars().any(char::is_control)) {
        return Err(ApiError::bad_request(format!(
            "`{field}` is too long or malformed"
        )));
    }
    Ok(())
}

/// Composio's toolkit catalog (searchable, paged), with this installation's
/// state per toolkit.  Served from the local cache and the bundled popular
/// apps: it never waits for the network, a stale cache is refreshed in the
/// background.
#[utoipa::path(get, path = "/v1/composio/toolkits", tag = "composio", security(("bearer" = [])),
    params(ComposioCatalogQuery),
    responses((status = 200, body = ComposioCatalogDto),
              (status = 502, body = ErrorBody)))]
pub(crate) async fn composio_catalog(
    State(s): State<AppState>,
    Query(q): Query<ComposioCatalogQuery>,
) -> Result<Json<ComposioCatalogDto>, ApiError> {
    short("search", q.search.as_deref(), 100)?;
    short("group", q.group.as_deref(), 60)?;
    short("cursor", q.cursor.as_deref(), 512)?;
    Ok(Json(composio(&s)?.catalog(q).await?))
}

/// Apps the owner connected or started to connect.
#[utoipa::path(get, path = "/v1/composio/apps", tag = "composio", security(("bearer" = [])),
    responses((status = 200, body = Vec<ComposioAppDto>)))]
pub(crate) async fn composio_list_apps(
    State(s): State<AppState>,
) -> Result<Json<Vec<ComposioAppDto>>, ApiError> {
    Ok(Json(composio(&s)?.apps()))
}

/// One app, refreshed from Composio (poll this while sign-in is pending).
#[utoipa::path(get, path = "/v1/composio/apps/{slug}", tag = "composio", security(("bearer" = [])),
    params(("slug" = String, Path, description = "Toolkit slug")),
    responses((status = 200, body = ComposioAppDto), (status = 404, body = ErrorBody)))]
pub(crate) async fn composio_get_app(
    State(s): State<AppState>,
    Path(slug): Path<String>,
) -> Result<Json<ComposioAppDto>, ApiError> {
    check_slug(&slug)?;
    Ok(Json(composio(&s)?.app(&slug).await?))
}

/// Start connecting an app: returns the Composio-hosted consent link, valid
/// once.  The caller opens it in the system browser.  Connecting is the
/// owner's opt-in for that app.
#[utoipa::path(post, path = "/v1/composio/apps/{slug}/connect", tag = "composio",
    security(("bearer" = [])),
    params(("slug" = String, Path, description = "Toolkit slug")),
    responses((status = 200, body = ComposioConnectDto), (status = 409, body = ErrorBody),
              (status = 502, body = ErrorBody)))]
pub(crate) async fn composio_connect(
    State(s): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    Path(slug): Path<String>,
) -> Result<Json<ComposioConnectDto>, ApiError> {
    check_slug(&slug)?;
    let c = composio(&s)?;
    audit(
        &s,
        &actor,
        "composio.connect_started",
        &json!({"app": slug}),
    )?;
    Ok(Json(c.connect(&slug).await?))
}

/// Switch an app's tools on or off.
#[utoipa::path(put, path = "/v1/composio/apps/{slug}/enabled", tag = "composio",
    security(("bearer" = [])),
    params(("slug" = String, Path, description = "Toolkit slug")),
    request_body = ComposioEnabledInput,
    responses((status = 200, body = ComposioAppDto), (status = 404, body = ErrorBody)))]
pub(crate) async fn composio_set_enabled(
    State(s): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    Path(slug): Path<String>,
    ApiJson(input): ApiJson<ComposioEnabledInput>,
) -> Result<Json<ComposioAppDto>, ApiError> {
    check_slug(&slug)?;
    let c = composio(&s)?;
    audit(
        &s,
        &actor,
        "composio.app_enabled",
        &json!({"app": slug, "enabled": input.enabled}),
    )?;
    Ok(Json(c.set_enabled(&slug, input.enabled).await?))
}

/// Disconnect an app: deletes the Composio-side account when possible and
/// always forgets the app locally.
#[utoipa::path(delete, path = "/v1/composio/apps/{slug}", tag = "composio",
    security(("bearer" = [])),
    params(("slug" = String, Path, description = "Toolkit slug")),
    responses((status = 200, body = ComposioDisconnectDto), (status = 404, body = ErrorBody)))]
pub(crate) async fn composio_disconnect(
    State(s): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    Path(slug): Path<String>,
) -> Result<Json<ComposioDisconnectDto>, ApiError> {
    check_slug(&slug)?;
    let c = composio(&s)?;
    audit(&s, &actor, "composio.disconnected", &json!({"app": slug}))?;
    Ok(Json(c.disconnect(&slug).await?))
}
