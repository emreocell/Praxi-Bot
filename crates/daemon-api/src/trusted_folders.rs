//! Folders the owner trusts as their own content. A git work tree with a remote is
//! otherwise outside content (its README can carry injected text); the owner lifts that per
//! folder. Changing the list takes the owner key; it is never offered to a peer.

use axum::{Extension, Json, extract::State, http::HeaderMap};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{ApiError, AppState, routes::ApiJson};

#[derive(Debug, Serialize, ToSchema)]
pub struct TrustedFoldersDto {
    /// Folders, as stored (normalised: `/` separators, lower case on Windows).
    pub folders: Vec<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TrustFolderInput {
    /// An existing directory (an absolute path).
    pub path: String,
}

fn local_only(peer: Option<&Extension<crate::PeerIdentity>>) -> Result<(), ApiError> {
    if peer.is_some() {
        return Err(ApiError::forbidden("trusted folders are local only"));
    }
    Ok(())
}

fn list_of(s: &AppState) -> Result<Json<TrustedFoldersDto>, ApiError> {
    Ok(Json(TrustedFoldersDto {
        folders: s.storage().trusted_folders()?,
    }))
}

/// The folders Praxi treats as the owner's own content.
#[utoipa::path(get, path = "/v1/trusted-folders", operation_id = "list_trusted_folders", tag = "files", security(("bearer" = [])),
    responses((status = 200, body = TrustedFoldersDto)))]
pub(crate) async fn list(
    State(s): State<AppState>,
    peer: Option<Extension<crate::PeerIdentity>>,
) -> Result<Json<TrustedFoldersDto>, ApiError> {
    local_only(peer.as_ref())?;
    list_of(&s)
}

/// Trust a folder ("I trust this folder"). Owner key required.
#[utoipa::path(post, path = "/v1/trusted-folders", operation_id = "trust_folder", tag = "files", security(("bearer" = [])),
    request_body = TrustFolderInput,
    responses((status = 200, body = TrustedFoldersDto), (status = 403, body = crate::error::ErrorBody)))]
pub(crate) async fn add(
    State(s): State<AppState>,
    peer: Option<Extension<crate::PeerIdentity>>,
    headers: HeaderMap,
    ApiJson(input): ApiJson<TrustFolderInput>,
) -> Result<Json<TrustedFoldersDto>, ApiError> {
    local_only(peer.as_ref())?;
    s.require_owner(&headers)?;
    let path = std::path::Path::new(&input.path);
    if !path.is_absolute() {
        return Err(ApiError::bad_request("the folder must be an absolute path"));
    }
    let real = std::fs::canonicalize(path)
        .ok()
        .filter(|p| p.is_dir())
        .ok_or_else(|| ApiError::bad_request("that folder does not exist"))?;
    // A drive root would make everything "own content".
    if !builtin_tools::provenance::trust_entry_allowed(&real, &s.data_dir()) {
        return Err(ApiError::bad_request(
            "choose a project folder outside home, Downloads, temporary and Praxi data directories",
        ));
    }
    s.storage()
        .trust_folder(&builtin_tools::provenance::normalize(&real))?;
    s.storage().append_audit(
        s.user_id(),
        None,
        "user",
        "trusted_folder.add",
        &serde_json::json!({ "folder": input.path }),
    )?;
    list_of(&s)
}

/// Stop trusting a folder. Owner key required.
#[utoipa::path(delete, path = "/v1/trusted-folders", operation_id = "untrust_folder", tag = "files", security(("bearer" = [])),
    request_body = TrustFolderInput,
    responses((status = 200, body = TrustedFoldersDto), (status = 403, body = crate::error::ErrorBody)))]
pub(crate) async fn remove(
    State(s): State<AppState>,
    peer: Option<Extension<crate::PeerIdentity>>,
    headers: HeaderMap,
    ApiJson(input): ApiJson<TrustFolderInput>,
) -> Result<Json<TrustedFoldersDto>, ApiError> {
    local_only(peer.as_ref())?;
    s.require_owner(&headers)?;
    // The stored spelling (the list shows exactly it); a typed path is normalised too.
    let stored = builtin_tools::provenance::normalize(std::path::Path::new(&input.path));
    s.storage().untrust_folder(&stored)?;
    s.storage().append_audit(
        s.user_id(),
        None,
        "user",
        "trusted_folder.remove",
        &serde_json::json!({ "folder": input.path }),
    )?;
    list_of(&s)
}
