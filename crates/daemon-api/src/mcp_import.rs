//! The local owner's read-only previews and explicit MCP installation routes.

use std::collections::BTreeMap;

use axum::{
    Json,
    body::to_bytes,
    extract::{
        FromRequest, Multipart, Query, Request, State, multipart::Field, rejection::JsonRejection,
    },
    http::StatusCode,
};
use mcp_bridge::{
    mcpb::{self, Inspection, MAX_BUNDLE_BYTES},
    registry::{self, RegistryPreview},
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use zeroize::{Zeroize, Zeroizing};

use crate::{
    AppState,
    error::{ApiError, ErrorBody},
    mcp::{McpControlError, McpServerDto},
    mcp_routes::{MaybeActor, audited, mcp},
};

const MAX_APPROVAL_BYTES: usize = 64 * 1024;
pub(crate) const MAX_INSTALL_BODY: usize = MAX_BUNDLE_BYTES + MAX_APPROVAL_BYTES + 4096;
static BUNDLE_UPLOAD: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

impl From<mcp_bridge::McpError> for McpControlError {
    fn from(error: mcp_bridge::McpError) -> Self {
        match error {
            mcp_bridge::McpError::Config(message) => Self::Invalid(message),
            mcp_bridge::McpError::UnknownServer(name) => {
                Self::NotFound(format!("mcp server {name}"))
            }
            e @ mcp_bridge::McpError::ToolChangedAgain { .. } => {
                Self::ToolChangedAgain(e.to_string())
            }
            other => Self::Internal(other.to_string()),
        }
    }
}

/// Install settings, including write-only secrets.
#[derive(Default, Deserialize, ToSchema)]
#[serde(transparent)]
#[schema(value_type = BTreeMap<String, String>)]
pub struct ImportValues(pub BTreeMap<String, String>);

impl Drop for ImportValues {
    fn drop(&mut self) {
        self.0.values_mut().for_each(String::zeroize);
    }
}

/// JSON metadata inside the bundle installation form.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct BundleApproval {
    /// SHA-256 returned by the inspection and approved by the owner.
    pub approval_sha256: String,
    /// Owner-provided MCPB user settings.
    #[serde(default)]
    #[schema(write_only)]
    pub values: ImportValues,
}

/// Multipart installation request. The browser uploads the reviewed file again.
#[derive(ToSchema)]
#[allow(dead_code)] // OpenAPI describes the multipart fields parsed below.
struct BundleInstallForm {
    #[schema(value_type = String, format = Binary)]
    bundle: Vec<u8>,
    /// JSON-encoded `BundleApproval`.
    approval: String,
}

/// Search performed by the local owner, sent to the official registry.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub(crate) struct RegistrySearch {
    q: String,
}

/// One search result and the supported launch previews it offers.
#[derive(Serialize, ToSchema)]
pub(crate) struct RegistryResult {
    name: String,
    description: String,
    version: String,
    publisher: String,
    namespace_verified: bool,
    active: bool,
    repository: Option<String>,
    sources: Vec<RegistryPreview>,
}

/// Explicit registry install approval. The server is looked up again by version.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RegistryApproval {
    name: String,
    version: String,
    approval_sha256: String,
    #[serde(default)]
    #[schema(write_only)]
    values: ImportValues,
}

/// Optional checksum independently provided by the owner, computed over the full file.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub(crate) struct BundleChecksum {
    expected_sha256: Option<String>,
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn upload_slot() -> Result<tokio::sync::SemaphorePermit<'static>, ApiError> {
    BUNDLE_UPLOAD.try_acquire().map_err(|_| {
        ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "bundle_busy",
            "another bundle is being processed",
        )
    })
}

fn too_large() -> ApiError {
    ApiError::new(
        StatusCode::PAYLOAD_TOO_LARGE,
        "bundle_too_large",
        "bundle upload exceeds the size limit",
    )
}

fn invalid_bundle(error: mcp_bridge::McpError) -> ApiError {
    ApiError::from(McpControlError::from(error))
}

#[utoipa::path(get, path = "/v1/mcp/import/registry", tag = "mcp", security(("bearer" = [])),
    params(RegistrySearch), responses((status = 200, body = Vec<RegistryResult>),
        (status = 403, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub(crate) async fn search_registry(
    State(state): State<AppState>,
    Query(input): Query<RegistrySearch>,
) -> Result<Json<Vec<RegistryResult>>, ApiError> {
    mcp(&state)?;
    let query = input.q.trim();
    if query.is_empty() || query.len() > 200 {
        return Err(ApiError::bad_request(
            "enter a registry search of 1 to 200 bytes",
        ));
    }
    let entries = registry::search(query, 30)
        .await
        .map_err(|_| ApiError::unavailable("could not read the official MCP Registry"))?;
    Ok(Json(
        entries
            .into_iter()
            .map(|entry| {
                let sources = (0..entry.sources.len())
                    .filter_map(|index| entry.preview(index).ok())
                    .collect();
                RegistryResult {
                    name: entry.name,
                    description: entry.description,
                    version: entry.version,
                    publisher: entry.publisher,
                    namespace_verified: entry.namespace_verified,
                    active: entry.active,
                    repository: entry.repository,
                    sources,
                }
            })
            .collect(),
    ))
}

#[utoipa::path(post, path = "/v1/mcp/import/registry", tag = "mcp", security(("bearer" = [])),
    request_body = RegistryApproval, responses((status = 201, body = McpServerDto),
        (status = 403, body = ErrorBody), (status = 409, body = ErrorBody)))]
pub(crate) async fn install_registry(
    State(state): State<AppState>,
    actor: MaybeActor,
    input: Result<Json<RegistryApproval>, JsonRejection>,
) -> Result<(StatusCode, Json<McpServerDto>), ApiError> {
    let Json(input) = input.map_err(|_| ApiError::bad_request("invalid registry approval"))?;
    if !valid_sha256(&input.approval_sha256) {
        return Err(ApiError::bad_request("invalid registry approval digest"));
    }
    let host = mcp(&state)?;
    let entry = registry::lookup(&input.name, &input.version)
        .await
        .map_err(|_| registry_changed())?;
    let index = approved_source(&entry, &input.approval_sha256)?;
    let server = audited(&state, &actor, "mcp.registry_install", &input.name, async {
        Ok(host.install_registry(entry, index, input.values).await?)
    })
    .await?;
    Ok((StatusCode::CREATED, Json(server)))
}

fn registry_changed() -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "registry_changed",
        "the approved registry version or source is no longer available",
    )
}

fn approved_source(entry: &registry::RegistryEntry, digest: &str) -> Result<usize, ApiError> {
    (0..entry.sources.len())
        .find(|&index| {
            entry
                .preview(index)
                .is_ok_and(|source| source.approval_sha256.eq_ignore_ascii_case(digest))
        })
        .ok_or_else(registry_changed)
}

#[utoipa::path(post, path = "/v1/mcp/import/bundle/inspect", tag = "mcp", security(("bearer" = [])),
    params(BundleChecksum), request_body(content = String, content_type = "application/octet-stream"),
    responses((status = 200, body = Inspection), (status = 403, body = ErrorBody),
        (status = 413, body = ErrorBody), (status = 422, body = ErrorBody)))]
pub(crate) async fn inspect_bundle(
    State(state): State<AppState>,
    Query(input): Query<BundleChecksum>,
    request: Request,
) -> Result<Json<Inspection>, ApiError> {
    mcp(&state)?;
    if input
        .expected_sha256
        .as_deref()
        .is_some_and(|value| !valid_sha256(value))
    {
        return Err(ApiError::bad_request(
            "expected SHA-256 must be 64 hexadecimal characters",
        ));
    }
    let slot = upload_slot()?;
    let bytes = to_bytes(request.into_body(), MAX_BUNDLE_BYTES)
        .await
        .map_err(|_| too_large())?;
    let card = tokio::task::spawn_blocking(move || {
        let _slot = slot;
        mcpb::inspect_with_checksum(&bytes, input.expected_sha256.as_deref())
    })
    .await
    .map_err(|_| ApiError::internal("bundle inspection failed"))?
    .map_err(invalid_bundle)?;
    Ok(Json(card))
}

fn multipart_error(error: axum::extract::multipart::MultipartError) -> ApiError {
    if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
        too_large()
    } else {
        ApiError::bad_request("invalid bundle upload")
    }
}

async fn read_part(mut field: Field<'_>, max: usize) -> Result<Vec<u8>, ApiError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = field.chunk().await.map_err(multipart_error)? {
        if chunk.len() > max - bytes.len() {
            return Err(too_large());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[utoipa::path(post, path = "/v1/mcp/import/bundle", tag = "mcp", security(("bearer" = [])),
    request_body(content = BundleInstallForm, content_type = "multipart/form-data"),
    responses((status = 201, body = McpServerDto), (status = 403, body = ErrorBody),
        (status = 413, body = ErrorBody), (status = 422, body = ErrorBody)))]
pub(crate) async fn install_bundle(
    State(state): State<AppState>,
    actor: MaybeActor,
    request: Request,
) -> Result<(StatusCode, Json<McpServerDto>), ApiError> {
    let host = mcp(&state)?;
    let _slot = upload_slot()?;
    let mut form = Multipart::from_request(request, &state)
        .await
        .map_err(|_| ApiError::bad_request("invalid bundle upload"))?;
    let (mut bundle, mut approval) = (None, None);
    while let Some(field) = form.next_field().await.map_err(multipart_error)? {
        match field.name() {
            Some("bundle") if bundle.is_none() => {
                bundle = Some(read_part(field, MAX_BUNDLE_BYTES).await?)
            }
            Some("approval") if approval.is_none() => {
                let bytes = Zeroizing::new(read_part(field, MAX_APPROVAL_BYTES).await?);
                let input: BundleApproval = serde_json::from_slice(&bytes)
                    .map_err(|_| ApiError::bad_request("invalid bundle approval"))?;
                if !valid_sha256(&input.approval_sha256) {
                    return Err(ApiError::bad_request("invalid bundle approval digest"));
                }
                approval = Some(input);
            }
            _ => return Err(ApiError::bad_request("unexpected bundle upload field")),
        }
    }
    let bundle = bundle.ok_or_else(|| ApiError::bad_request("bundle file is missing"))?;
    let approval = approval.ok_or_else(|| ApiError::bad_request("bundle approval is missing"))?;
    let server = audited(&state, &actor, "mcp.bundle_install", "bundle", async {
        Ok(host.install_bundle(bundle, approval).await?)
    })
    .await?;
    Ok((StatusCode::CREATED, Json(server)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_approval_survives_reordering_but_not_a_changed_launch_or_status() {
        let mut entry = registry::parse_search(include_bytes!(
            "../../mcp-bridge/tests/fixtures/registry-search.json"
        ))
        .unwrap()
        .remove(0);
        let approved = entry.preview(0).unwrap().approval_sha256;
        assert_eq!(approved_source(&entry, &approved).unwrap(), 0);
        entry.sources.swap(0, 1);
        assert_eq!(approved_source(&entry, &approved).unwrap(), 1);
        let registry::Source::Npm { args, .. } = &mut entry.sources[1] else {
            panic!()
        };
        args.push("extra".into());
        assert_eq!(
            approved_source(&entry, &approved).unwrap_err().status(),
            StatusCode::CONFLICT
        );
        let current = entry.preview(1).unwrap().approval_sha256;
        entry.active = false;
        assert_eq!(
            approved_source(&entry, &current).unwrap_err().status(),
            StatusCode::CONFLICT
        );
    }
}
