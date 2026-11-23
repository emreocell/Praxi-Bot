//! Handlers of `/v1/mcp/*` and `POST /v1/secrets/{name}`.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Extension, Json};

use crate::AppState;
use crate::error::{ApiError, ErrorBody};
use crate::mcp::{
    McpApproveChangesInput, McpCatalogEntryDto, McpControl, McpControlError, McpServerDto,
    McpServerInput, McpToolDto, SetSecret,
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
            McpControlError::ToolChangedAgain(m) => {
                Self::new(StatusCode::CONFLICT, "mcp_tool_changed_again", m)
            }
        }
    }
}

fn mcp(s: &AppState) -> Result<Arc<dyn McpControl>, ApiError> {
    s.mcp()
        .ok_or_else(|| ApiError::unavailable("MCP management is not available in this daemon"))
}

type MaybeActor = Option<Extension<crate::live_view::Actor>>;

/// Longest server or secret name copied into an audit row.
const MAX_AUDIT_NAME: usize = 128;

/// The redacted, bounded `name` and actor of an audit row.  Only names go in:
/// never a config body, URL, header, environment value or secret value.
fn audit_fields(s: &AppState, actor: &MaybeActor, name: &str) -> (String, String) {
    let redact = |text: &str| s.mcp().map_or_else(|| text.to_owned(), |m| m.redact(text));
    let who = actor
        .as_ref()
        .map_or_else(|| "user".to_owned(), |Extension(a)| a.key.clone());
    let name: String = name.chars().take(MAX_AUDIT_NAME).collect();
    (redact(&who), redact(&name))
}

/// Record an MCP or secret mutation in the audit log **before** it runs.
/// Fails closed: nothing happens when the row cannot be written.
fn audit_before(
    s: &AppState,
    actor: &MaybeActor,
    action: &str,
    name: &str,
) -> Result<(), ApiError> {
    let (who, name) = audit_fields(s, actor, name);
    s.storage()
        .append_audit(
            s.user_id(),
            None,
            &who,
            action,
            &serde_json::json!({ "name": name, "phase": "requested" }),
        )
        .map(|_| ())
        .map_err(|_| ApiError::internal("cannot write the audit log"))
}

/// Record how a mutation ended (status only, no error text).
fn audit_after<T>(
    s: &AppState,
    actor: &MaybeActor,
    action: &str,
    name: &str,
    result: &Result<T, ApiError>,
) {
    let (who, name) = audit_fields(s, actor, name);
    let status = result
        .as_ref()
        .map_or_else(|e| e.status().as_u16(), |_| 200);
    // Best effort: the action already ran and its request row exists.
    let _ = s.storage().append_audit(
        s.user_id(),
        None,
        &who,
        action,
        &serde_json::json!({ "name": name, "phase": "completed", "status": status }),
    );
}

/// Audit around `op`: a row before (fail closed) and one after.
async fn audited<T, F>(
    s: &AppState,
    actor: &MaybeActor,
    action: &str,
    name: &str,
    op: F,
) -> Result<T, ApiError>
where
    F: std::future::Future<Output = Result<T, ApiError>>,
{
    audit_before(s, actor, action, name)?;
    let result = op.await;
    audit_after(s, actor, action, name, &result);
    result
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

/// Most ids a `bots` list (or a `bots_add` / `bots_remove` list) may carry.
pub(crate) const MAX_BOT_IDS: usize = 256;
/// Longest bot id (a UUID is 36 characters).
pub(crate) const MAX_BOT_ID_LEN: usize = 64;

/// Reject oversized or malformed bot id lists with `400`.
pub(crate) fn check_bot_ids(field: &str, ids: &[String]) -> Result<(), ApiError> {
    if ids.len() > MAX_BOT_IDS {
        return Err(ApiError::bad_request(format!(
            "`{field}` has more than {MAX_BOT_IDS} ids"
        )));
    }
    if ids
        .iter()
        .any(|id| id.is_empty() || id.len() > MAX_BOT_ID_LEN)
    {
        return Err(ApiError::bad_request(format!(
            "every id in `{field}` must be 1 to {MAX_BOT_ID_LEN} characters"
        )));
    }
    Ok(())
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
    actor: MaybeActor,
    ApiJson(input): ApiJson<McpServerInput>,
) -> Result<(StatusCode, Json<McpServerDto>), ApiError> {
    if let Some(bots) = &input.bots {
        check_bot_ids("bots", bots)?;
    }
    let host = mcp(&s)?;
    let name = input.name.clone().unwrap_or_default();
    let server = audited(&s, &actor, "mcp.server_create", &name, async {
        Ok(host.add(input).await?)
    })
    .await?;
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
    actor: MaybeActor,
    Path(name): Path<String>,
    ApiJson(patch): ApiJson<serde_json::Value>,
) -> Result<Json<McpServerDto>, ApiError> {
    for field in ["bots", "bots_add", "bots_remove"] {
        if let Some(arr) = patch.get(field).and_then(serde_json::Value::as_array) {
            let ids: Vec<String> = arr
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect();
            check_bot_ids(field, &ids)?;
        }
    }
    let host = mcp(&s)?;
    audited(&s, &actor, "mcp.server_update", &name, async {
        Ok(Json(host.update(&name, patch).await?))
    })
    .await
}

/// Disconnect and remove a server (its tools disappear) and save `mcp.toml`.
#[utoipa::path(delete, path = "/v1/mcp/servers/{name}", tag = "mcp", security(("bearer" = [])),
    params(("name" = String, Path, description = "Server name")),
    responses((status = 204), (status = 404, body = ErrorBody),
              (status = 503, body = ErrorBody, description = "MCP management is not available")))]
pub(crate) async fn delete_mcp_server(
    State(s): State<AppState>,
    actor: MaybeActor,
    Path(name): Path<String>,
) -> Result<StatusCode, ApiError> {
    let host = mcp(&s)?;
    audited(&s, &actor, "mcp.server_delete", &name, async {
        host.remove(&name).await?;
        Ok(StatusCode::NO_CONTENT)
    })
    .await
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
    actor: MaybeActor,
    Path(name): Path<String>,
) -> Result<Json<McpServerDto>, ApiError> {
    let host = mcp(&s)?;
    audited(&s, &actor, "mcp.server_reconnect", &name, async {
        Ok(Json(host.reconnect(&name).await?))
    })
    .await
}

/// Approve the tools a server changed after the owner approved them, so they
/// are switched on again. Owner only.
#[utoipa::path(post, path = "/v1/mcp/servers/{name}/approve-tool-changes", tag = "mcp",
    security(("bearer" = [])),
    params(("name" = String, Path, description = "Server name")),
    request_body = McpApproveChangesInput,
    responses((status = 200, body = McpServerDto), (status = 404, body = ErrorBody),
              (status = 409, body = ErrorBody, description = "The tool changed again since it was shown (`mcp_tool_changed_again`)"),
              (status = 503, body = ErrorBody, description = "MCP management is not available")))]
pub(crate) async fn approve_mcp_tool_changes(
    State(s): State<AppState>,
    actor: MaybeActor,
    Path(name): Path<String>,
    ApiJson(body): ApiJson<McpApproveChangesInput>,
) -> Result<Json<McpServerDto>, ApiError> {
    let host = mcp(&s)?;
    audited(&s, &actor, "mcp.tool_changes_approved", &name, async {
        Ok(Json(
            host.approve_tool_changes(&name, &body.tool, &body.hash)
                .await?,
        ))
    })
    .await
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
    actor: MaybeActor,
    Path(name): Path<String>,
    ApiJson(input): ApiJson<SetSecret>,
) -> Result<StatusCode, ApiError> {
    if secret_store::is_host_credential(&name) {
        return Err(ApiError::forbidden(
            "host credentials require their dedicated owner route",
        ));
    }
    let store = s
        .secret_store()
        .ok_or_else(|| ApiError::unavailable("this daemon has no secret store"))?;
    secret_store::validate_name(&name).map_err(|_| ApiError::bad_request("invalid secret name"))?;
    // Secrets owned by a native integration (the Composio key) are written by
    // that integration's own route only, by no role at all through this one.
    if secret_store::is_reserved_name(&name) {
        return Err(ApiError::forbidden("this secret name is reserved"));
    }
    // The fill namespace belongs to the owner-only `/v1/fill-secrets` routes,
    // which bind each secret to its allow-lists; this generic sink (reachable
    // by a paired desktop) must not create or overwrite one.
    if secret_store::is_fill_name(&name) {
        return Err(ApiError::bad_request(
            "names starting with `fill/` are managed through /v1/fill-secrets",
        ));
    }
    // OAuth records are written by the MCP bridge's credential store only;
    // a planted record would be presented as this server's sign-in.
    if secret_store::is_managed_oauth_name(&name) {
        return Err(ApiError::bad_request(
            "this name is reserved for MCP sign-ins and cannot be set",
        ));
    }
    if input.value.is_empty() || input.value.len() > MAX_SECRET_BYTES {
        return Err(ApiError::bad_request(
            "the value must be between 1 byte and 64 KiB",
        ));
    }
    audited(&s, &actor, "secret.set", &name.clone(), async move {
        // A backend error is not echoed in full, so a value can never leak into a response.
        tokio::task::spawn_blocking(move || store.set(&name, &input.value))
            .await
            .map_err(ApiError::internal)?
            .map_err(|_| ApiError::internal("cannot store the secret"))?;
        Ok(StatusCode::NO_CONTENT)
    })
    .await
}

/// Start a sign-in from an authenticated local user's explicit action.
#[utoipa::path(post, path = "/v1/mcp/servers/{name}/auth/start", tag = "mcp", security(("bearer" = [])),
    params(("name" = String, Path)), responses((status = 200, body = crate::mcp::McpAuthStart), (status = 404, body = ErrorBody), (status = 422, body = ErrorBody)))]
pub(crate) async fn start_mcp_auth(
    State(s): State<AppState>,
    actor: MaybeActor,
    Path(name): Path<String>,
) -> Result<Json<crate::mcp::McpAuthStart>, ApiError> {
    let host = mcp(&s)?;
    audited(&s, &actor, "mcp.auth_start", &name, async {
        Ok(Json(host.auth_start(&name).await?))
    })
    .await
}

/// Finish a pending sign-in with a pasted redirect URL (host without a browser).
#[utoipa::path(post, path = "/v1/mcp/servers/{name}/auth/callback", tag = "mcp", security(("bearer" = [])),
    params(("name" = String, Path)), request_body = crate::mcp::McpAuthCallback,
    responses((status = 200, body = crate::mcp::McpAuthState), (status = 404, body = ErrorBody), (status = 422, body = ErrorBody)))]
pub(crate) async fn complete_mcp_auth(
    State(s): State<AppState>,
    actor: MaybeActor,
    Path(name): Path<String>,
    Json(body): Json<crate::mcp::McpAuthCallback>,
) -> Result<Json<crate::mcp::McpAuthState>, ApiError> {
    let host = mcp(&s)?;
    // The redirect URL carries the authorization code: never in the audit row.
    audited(&s, &actor, "mcp.auth_callback", &name, async {
        Ok(Json(host.auth_callback(&name, &body.redirect_url).await?))
    })
    .await
}

/// Close the one-shot listener and cancel a pending token exchange.
#[utoipa::path(post, path = "/v1/mcp/servers/{name}/auth/cancel", tag = "mcp", security(("bearer" = [])),
    params(("name" = String, Path)), responses((status = 200, body = crate::mcp::McpAuthState), (status = 404, body = ErrorBody)))]
pub(crate) async fn cancel_mcp_auth(
    State(s): State<AppState>,
    actor: MaybeActor,
    Path(name): Path<String>,
) -> Result<Json<crate::mcp::McpAuthState>, ApiError> {
    let host = mcp(&s)?;
    audited(&s, &actor, "mcp.auth_cancel", &name, async {
        Ok(Json(host.auth_cancel(&name).await?))
    })
    .await
}

/// Read authentication state, never credentials.
#[utoipa::path(get, path = "/v1/mcp/servers/{name}/auth/status", tag = "mcp", security(("bearer" = [])),
    params(("name" = String, Path)), responses((status = 200, body = crate::mcp::McpAuthState), (status = 404, body = ErrorBody)))]
pub(crate) async fn mcp_auth_status(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<crate::mcp::McpAuthState>, ApiError> {
    Ok(Json(mcp(&s)?.auth_status(&name).await?))
}

/// Cancel sign-in and forget stored tokens before disconnecting.
#[utoipa::path(post, path = "/v1/mcp/servers/{name}/auth/logout", tag = "mcp", security(("bearer" = [])),
    params(("name" = String, Path)), responses((status = 200, body = crate::mcp::McpAuthState), (status = 404, body = ErrorBody)))]
pub(crate) async fn logout_mcp_auth(
    State(s): State<AppState>,
    actor: MaybeActor,
    Path(name): Path<String>,
) -> Result<Json<crate::mcp::McpAuthState>, ApiError> {
    let host = mcp(&s)?;
    audited(&s, &actor, "mcp.auth_logout", &name, async {
        Ok(Json(host.auth_logout(&name).await?))
    })
    .await
}
