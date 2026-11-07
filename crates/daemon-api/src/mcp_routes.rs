//! Handlers of `/v1/mcp/*` and `POST /v1/secrets/{name}`.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;

use crate::AppState;
use crate::error::{ApiError, ErrorBody};
use crate::mcp::{
    McpCatalogEntryDto, McpControl, McpControlError, McpServerDto, McpServerInput, McpToolDto,
    SetSecret,
};
use crate::routes::ApiJson;

/// Largest accepted secret value.
const MAX_SECRET_BYTES: usize = 64 * 1024;

impl From<McpControlError> for ApiError {
    fn from(e: McpControlError) -> Self {
        match e {
            McpControlError::NotFound(w) => Self::not_found(w),
            McpControlError::Conflict(m) => Self::conflict(m),
            McpControlError::Invalid(m) => {
                Self::new(StatusCode::UNPROCESSABLE_ENTITY, "bad_mcp_config", m)
            }
            McpControlError::Internal(m) => Self::internal(m),
        }
    }
}

fn mcp(s: &AppState) -> Result<Arc<dyn McpControl>, ApiError> {
    s.mcp()
        .ok_or_else(|| ApiError::unavailable("MCP management is not available in this daemon"))
}

/// All configured MCP servers with their live status and full config.
/// Secret values are never included; `secret_env` and `secret_headers` list
/// secret names.
#[utoipa::path(get, path = "/v1/mcp/servers", tag = "mcp", security(("bearer" = [])),
    responses((status = 200, body = Vec<McpServerDto>),
              (status = 503, body = ErrorBody, description = "MCP management is not available")))]
pub(crate) async fn list_mcp_servers(
    State(s): State<AppState>,
) -> Result<Json<Vec<McpServerDto>>, ApiError> {
    Ok(Json(mcp(&s)?.list()))
}

/// Add an MCP server, connect it (when enabled) and save `mcp.toml`.  A server
/// that cannot connect is still added (`status: error`).  Secrets are given as
/// secret names (`secret_env` / `secret_headers`), never as values; `category`
/// defaults to `accounts`, `meta` is rejected, `trust_hints` defaults to `false`.
#[utoipa::path(post, path = "/v1/mcp/servers", tag = "mcp", security(("bearer" = [])),
    request_body = McpServerInput,
    responses((status = 201, body = McpServerDto),
              (status = 409, body = ErrorBody, description = "the name is taken"),
              (status = 422, body = ErrorBody, description = "invalid config"),
              (status = 503, body = ErrorBody, description = "MCP management is not available")))]
pub(crate) async fn create_mcp_server(
    State(s): State<AppState>,
    ApiJson(input): ApiJson<McpServerInput>,
) -> Result<(StatusCode, Json<McpServerDto>), ApiError> {
    let server = mcp(&s)?.add(input).await?;
    Ok((StatusCode::CREATED, Json(server)))
}

/// Merge a partial config onto the stored server (it reconnects) and save
/// `mcp.toml`.  Omitted fields keep their previous values.  `bots: null`
/// means every bot.  Secret values are never accepted or returned; send
/// secret **names** in `secret_env` / `secret_headers`.
#[utoipa::path(put, path = "/v1/mcp/servers/{name}", tag = "mcp", security(("bearer" = [])),
    params(("name" = String, Path, description = "Server name")),
    request_body = crate::mcp::McpServerPatch,
    responses((status = 200, body = McpServerDto), (status = 404, body = ErrorBody),
              (status = 422, body = ErrorBody, description = "invalid config"),
              (status = 503, body = ErrorBody, description = "MCP management is not available")))]
pub(crate) async fn update_mcp_server(
    State(s): State<AppState>,
    Path(name): Path<String>,
    ApiJson(patch): ApiJson<serde_json::Value>,
) -> Result<Json<McpServerDto>, ApiError> {
    Ok(Json(mcp(&s)?.update(&name, patch).await?))
}

/// Disconnect and remove a server (its tools disappear) and save `mcp.toml`.
#[utoipa::path(delete, path = "/v1/mcp/servers/{name}", tag = "mcp", security(("bearer" = [])),
    params(("name" = String, Path, description = "Server name")),
    responses((status = 204), (status = 404, body = ErrorBody),
              (status = 503, body = ErrorBody, description = "MCP management is not available")))]
pub(crate) async fn delete_mcp_server(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> Result<StatusCode, ApiError> {
    mcp(&s)?.remove(&name).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Reconnect a server (also lifts a quarantine) and refresh its tools.  A
/// failed connect is reported as `status: error`, not as an HTTP error.
#[utoipa::path(post, path = "/v1/mcp/servers/{name}/reconnect", tag = "mcp",
    security(("bearer" = [])),
    params(("name" = String, Path, description = "Server name")),
    responses((status = 200, body = McpServerDto), (status = 404, body = ErrorBody),
              (status = 503, body = ErrorBody, description = "MCP management is not available")))]
pub(crate) async fn reconnect_mcp_server(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<McpServerDto>, ApiError> {
    Ok(Json(mcp(&s)?.reconnect(&name).await?))
}

/// The tools a server currently exposes (after its allow / exclude filters).
#[utoipa::path(get, path = "/v1/mcp/servers/{name}/tools", tag = "mcp",
    security(("bearer" = [])),
    params(("name" = String, Path, description = "Server name")),
    responses((status = 200, body = Vec<McpToolDto>), (status = 404, body = ErrorBody),
              (status = 503, body = ErrorBody, description = "MCP management is not available")))]
pub(crate) async fn list_mcp_tools(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Vec<McpToolDto>>, ApiError> {
    Ok(Json(mcp(&s)?.tools(&name)?))
}

/// The curated catalog of popular MCP servers (one-click install data).
#[utoipa::path(get, path = "/v1/mcp/catalog", tag = "mcp", security(("bearer" = [])),
    responses((status = 200, body = Vec<McpCatalogEntryDto>),
              (status = 503, body = ErrorBody, description = "MCP management is not available")))]
pub(crate) async fn mcp_catalog(
    State(s): State<AppState>,
) -> Result<Json<Vec<McpCatalogEntryDto>>, ApiError> {
    Ok(Json(mcp(&s)?.catalog()))
}

/// Store a secret (create or overwrite).  **Write-only**: no route returns a
/// secret value.  The name may contain `/` (e.g. `mcp/github-token`).
#[utoipa::path(post, path = "/v1/secrets/{name}", tag = "secrets", security(("bearer" = [])),
    params(("name" = String, Path, description = "Secret name, `[A-Za-z0-9._:/-]`, at most 128 bytes")),
    request_body = SetSecret,
    responses((status = 204), (status = 400, body = ErrorBody, description = "bad name or value"),
              (status = 503, body = ErrorBody, description = "no secret store")))]
pub(crate) async fn set_secret(
    State(s): State<AppState>,
    Path(name): Path<String>,
    ApiJson(input): ApiJson<SetSecret>,
) -> Result<StatusCode, ApiError> {
    let store = s
        .secret_store()
        .ok_or_else(|| ApiError::unavailable("this daemon has no secret store"))?;
    secret_store::validate_name(&name).map_err(|_| ApiError::bad_request("invalid secret name"))?;
    if input.value.is_empty() || input.value.len() > MAX_SECRET_BYTES {
        return Err(ApiError::bad_request(
            "the value must be between 1 byte and 64 KiB",
        ));
    }
    // A backend error is not echoed in full, so a value can never leak into a response.
    tokio::task::spawn_blocking(move || store.set(&name, &input.value))
        .await
        .map_err(ApiError::internal)?
        .map_err(|_| ApiError::internal("cannot store the secret"))?;
    Ok(StatusCode::NO_CONTENT)
}
