//! Request diagnostics: one terminal record per failed request, including
//! guard denials and extractor rejections. Neither body nor headers are read.
use axum::{
    Json,
    extract::{MatchedPath, Query, Request, State},
    middleware::Next,
    response::Response,
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::{ApiError, AppState, error::ErrorBody};

#[derive(Clone)]
pub(crate) struct ErrorCode(pub &'static str);

pub(crate) async fn request_log(req: Request, next: Next) -> Response {
    let method = match req.method().as_str() {
        "GET" => "GET",
        "POST" => "POST",
        "PUT" => "PUT",
        "PATCH" => "PATCH",
        "DELETE" => "DELETE",
        "HEAD" => "HEAD",
        "OPTIONS" => "OPTIONS",
        _ => "OTHER",
    };
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map_or("unmatched", MatchedPath::as_str)
        .to_owned();
    // Ignore any client-supplied id: it may itself carry a credential.
    let id = uuid::Uuid::now_v7().to_string();
    let mut response = next.run(req).await;
    let status = response.status();
    if status.is_client_error() || status.is_server_error() {
        let fallback = status
            .canonical_reason()
            .unwrap_or("http_error")
            .to_ascii_lowercase()
            .replace(' ', "_");
        let code = response
            .extensions()
            .get::<ErrorCode>()
            .map_or(fallback.as_str(), |c| c.0);
        if status.is_server_error() {
            support_logs::tracing::error!(target: "praxi_support", method, route = route.as_str(), status = u64::from(status.as_u16()), error_code = code, request_id = id.as_str());
        } else {
            support_logs::tracing::warn!(target: "praxi_support", method, route = route.as_str(), status = u64::from(status.as_u16()), error_code = code, request_id = id.as_str());
        }
    }
    if let Ok(value) = id.parse() {
        response.headers_mut().insert("x-request-id", value);
    }
    response
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub(crate) struct LogQuery {
    /// Exact level: trace, debug, info, warn, error. Omit for all levels.
    level: Option<String>,
    /// UTC Unix milliseconds, inclusive.
    since: Option<u64>,
}

#[derive(Serialize, ToSchema)]
pub(crate) struct RecentLogs {
    /// At most 500 redacted JSON lines, oldest first, across daemon and sidecar.
    lines: Vec<String>,
}

#[utoipa::path(get, path = "/v1/logs/recent", tag = "doctor", params(LogQuery), security(("bearer" = [])),
    responses((status = 200, body = RecentLogs, description = "Local owner only; redacted operational metadata"),
    (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 500, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub(crate) async fn recent(
    State(s): State<AppState>,
    Query(q): Query<LogQuery>,
) -> Result<Json<RecentLogs>, ApiError> {
    if q.level
        .as_deref()
        .is_some_and(|l| !matches!(l, "trace" | "debug" | "info" | "warn" | "error"))
    {
        return Err(ApiError::bad_request(
            "choose trace, debug, info, warn or error",
        ));
    }
    let dir = s.data_dir();
    let entries = tokio::task::spawn_blocking(move || {
        support_logs::recent(&dir, q.level.as_deref(), q.since)
    })
    .await
    .map_err(|_| ApiError::unavailable("recent logs could not be read; try again"))?
    .map_err(|_| {
        ApiError::unavailable("recent logs could not be read; check access to the data folder")
    })?;
    let lines = entries
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| ApiError::internal("log formatting failed"))?;
    Ok(Json(RecentLogs { lines }))
}
