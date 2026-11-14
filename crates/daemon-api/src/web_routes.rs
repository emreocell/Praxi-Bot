//! Handlers of `/v1/web/search*`. Same guard (Host / Origin / bearer) as
//! every other route; API keys are write-only.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;

use crate::AppState;
use crate::error::{ApiError, ErrorBody};
use crate::routes::ApiJson;
use crate::web::{
    WebControl, WebControlError, WebSearchConfigDto, WebSearchConfigInput, WebSearchTestInput,
    WebSearchTestReport,
};

/// Longest accepted API key (bytes).
const MAX_KEY_BYTES: usize = 1024;
/// Longest accepted test query (characters).
const MAX_TEST_QUERY_CHARS: usize = 400;

impl From<WebControlError> for ApiError {
    fn from(e: WebControlError) -> Self {
        match e {
            WebControlError::Invalid(m) => {
                Self::new(StatusCode::UNPROCESSABLE_ENTITY, "bad_web_config", m)
            }
            WebControlError::Internal(m) => Self::internal(m),
        }
    }
}

fn web(s: &AppState) -> Result<Arc<dyn WebControl>, ApiError> {
    s.web().ok_or_else(|| {
        ApiError::unavailable("web search management is not available in this daemon")
    })
}

fn check_api_key(what: &str, v: &str) -> Result<(), ApiError> {
    if v.is_empty() || v.len() > MAX_KEY_BYTES || v.contains(['\0', '\r', '\n']) {
        return Err(ApiError::bad_request(format!(
            "`{what}` must be 1 to {MAX_KEY_BYTES} bytes without line breaks or NUL"
        )));
    }
    Ok(())
}

/// Current web search configuration (API keys are write-only: `brave_key_set`
/// and `tavily_key_set` indicate whether a key is stored, never its value).
#[utoipa::path(get, path = "/v1/web/search", tag = "web", security(("bearer" = [])),
    responses((status = 200, body = WebSearchConfigDto),
              (status = 503, body = ErrorBody, description = "web search management is not available")))]
pub(crate) async fn get_web_search(
    State(s): State<AppState>,
) -> Result<Json<WebSearchConfigDto>, ApiError> {
    Ok(Json(web(&s)?.get()))
}

/// Update web search configuration (`web.toml` and write-only API keys in the
/// secret store) and apply immediately for the next tool call.
#[utoipa::path(put, path = "/v1/web/search", tag = "web", security(("bearer" = [])),
    request_body = WebSearchConfigInput,
    responses((status = 200, body = WebSearchConfigDto),
              (status = 400, body = ErrorBody, description = "bad API key"),
              (status = 422, body = ErrorBody, description = "invalid config"),
              (status = 503, body = ErrorBody, description = "web search management is not available")))]
pub(crate) async fn update_web_search(
    State(s): State<AppState>,
    ApiJson(input): ApiJson<WebSearchConfigInput>,
) -> Result<Json<WebSearchConfigDto>, ApiError> {
    let control = web(&s)?;
    if let Some(k) = &input.brave_key {
        check_api_key("brave_key", k)?;
    }
    if let Some(k) = &input.tavily_key {
        check_api_key("tavily_key", k)?;
    }
    Ok(Json(control.update(input).await?))
}

/// Run a sample live search against the current web search configuration and
/// report which backend answered and how long it took. At most two probes run
/// at once (`429`).
#[utoipa::path(post, path = "/v1/web/search/test", tag = "web", security(("bearer" = [])),
    request_body = WebSearchTestInput,
    responses((status = 200, body = WebSearchTestReport),
              (status = 400, body = ErrorBody, description = "query too long"),
              (status = 422, body = ErrorBody, description = "invalid backend"),
              (status = 429, body = ErrorBody, description = "too many probes running"),
              (status = 503, body = ErrorBody, description = "web search management is not available")))]
pub(crate) async fn test_web_search(
    State(s): State<AppState>,
    ApiJson(input): ApiJson<WebSearchTestInput>,
) -> Result<Json<WebSearchTestReport>, ApiError> {
    let control = web(&s)?;
    if let Some(q) = &input.query
        && q.chars().count() > MAX_TEST_QUERY_CHARS
    {
        return Err(ApiError::bad_request(format!(
            "`query` must be at most {MAX_TEST_QUERY_CHARS} characters"
        )));
    }
    let _permit = s.web_test_permit()?;
    Ok(Json(control.test(input).await?))
}
