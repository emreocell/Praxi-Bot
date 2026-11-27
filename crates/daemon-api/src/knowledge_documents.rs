//! "Ask my documents": the owner-facing helpers around a chosen folder.
//!
//! * browse the folders of the machine the daemon runs on (this computer, or
//!   the server), names only, to pick one;
//! * preview what choosing a folder would index, with sizes, an estimate and
//!   what is left out (secret-looking files first of all);
//! * open the file behind a citation.
//!
//! All three are owner-only. None of them can grant anything: choosing a folder
//! still goes through `create_source`, which re-checks every rule. Opening a
//! file only serves a document that is currently indexed from an enabled file
//! or folder source of this bot, read through the same no-follow capability as
//! indexing.

use std::path::{Path, PathBuf};

use axum::{
    Json,
    body::Body,
    extract::{Path as ApiPath, Query, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use memory_store::knowledge::source_adapters::{
    FolderPreview, SelectedSource, SkipReason, SkipSummary,
};
use memory_store::knowledge::{KnowledgeStore, SourceKind};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{ApiError, AppState, error::ErrorBody, routes::ApiJson};

/// Most sub-folders listed at once.
const MAX_FOLDER_ENTRIES: usize = 500;

/// A sub-folder in the folder picker.
#[derive(Debug, Serialize, ToSchema)]
pub struct KnowledgeFolderEntry {
    pub name: String,
    pub path: String,
    /// False for credential/browser-profile folders and the daemon's own data.
    pub selectable: bool,
}

/// One folder and its sub-folders (names only; no file is listed or read).
#[derive(Debug, Serialize, ToSchema)]
pub struct KnowledgeFolderListing {
    pub path: String,
    pub parent: Option<String>,
    /// Whether this very folder may be chosen.
    pub selectable: bool,
    pub entries: Vec<KnowledgeFolderEntry>,
    /// More than 500 sub-folders: only the first ones are listed.
    pub truncated: bool,
    /// Drive roots on Windows; empty elsewhere.
    pub roots: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct BrowseQuery {
    path: Option<String>,
}

/// Show a path without the Windows verbatim prefix `\\?\`.
fn display_path(path: &Path) -> String {
    let text = path.display().to_string();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => rest.to_owned(),
        _ => text,
    }
}

#[cfg(windows)]
fn drive_roots() -> Vec<String> {
    ('A'..='Z')
        .map(|letter| format!("{letter}:\\"))
        .filter(|root| Path::new(root).is_dir())
        .collect()
}

#[cfg(not(windows))]
fn drive_roots() -> Vec<String> {
    Vec::new()
}

fn browse(data_dir: &Path, requested: Option<&str>) -> Result<KnowledgeFolderListing, ApiError> {
    let start = match requested.map(str::trim).filter(|path| !path.is_empty()) {
        Some(path) => PathBuf::from(path),
        None => crate::doctor::home_dir().map_or_else(
            || PathBuf::from(std::path::MAIN_SEPARATOR_STR),
            PathBuf::from,
        ),
    };
    if !start.is_absolute() || start.to_string_lossy().chars().any(char::is_control) {
        return Err(ApiError::bad_request("choose an absolute folder path"));
    }
    let dir = std::fs::canonicalize(&start)
        .ok()
        .filter(|dir| dir.is_dir())
        .ok_or_else(|| ApiError::bad_request("that folder cannot be opened"))?;
    let mut entries = Vec::new();
    let mut truncated = false;
    let mut names: Vec<(String, PathBuf)> = std::fs::read_dir(&dir)
        .map_err(|_| ApiError::bad_request("that folder cannot be read"))?
        .flatten()
        // `DirEntry::file_type` does not follow links: a link to a folder is not listed.
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| Some((entry.file_name().into_string().ok()?, entry.path())))
        .filter(|(name, _)| !name.starts_with('.'))
        .collect();
    names.sort_by_key(|(name, _)| name.to_lowercase());
    for (name, path) in names {
        if entries.len() == MAX_FOLDER_ENTRIES {
            truncated = true;
            break;
        }
        entries.push(KnowledgeFolderEntry {
            selectable: SelectedSource::folder_allowed_on_host(&path, data_dir),
            path: display_path(&path),
            name,
        });
    }
    Ok(KnowledgeFolderListing {
        parent: dir.parent().map(display_path),
        selectable: SelectedSource::folder_allowed_on_host(&dir, data_dir),
        path: display_path(&dir),
        entries,
        truncated,
        roots: drive_roots(),
    })
}

/// List the sub-folders of a folder on the machine the daemon runs on. Owner only.
#[utoipa::path(get, path = "/v1/knowledge/folders", tag = "knowledge", security(("bearer" = [])), params(("path" = Option<String>, Query, description = "Absolute folder path; default is the home folder")), responses((status = 200, body = KnowledgeFolderListing), (status = 400, body = ErrorBody)))]
pub(crate) async fn browse_folders(
    State(s): State<AppState>,
    Query(query): Query<BrowseQuery>,
) -> Result<Json<KnowledgeFolderListing>, ApiError> {
    let data_dir = s.data_dir();
    crate::knowledge::read_blocking(move || browse(&data_dir, query.path.as_deref()).map(Json))
        .await
}

/// Folder to preview.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeFolderPreviewInput {
    /// Absolute path on the machine the daemon runs on.
    pub path: String,
}

/// A file that would be indexed.
#[derive(Debug, Serialize, ToSchema)]
pub struct KnowledgePreviewFile {
    pub locator: String,
    pub bytes: u64,
}

/// A file or folder that is left out, with the reason.
#[derive(Debug, Serialize, ToSchema)]
pub struct KnowledgeSkippedFile {
    /// Path inside the folder; folders end in `/`.
    pub locator: String,
    /// `secret` (looks like it holds secrets) or `too_large`.
    pub reason: String,
}

/// What a scan left out. Hidden and unsupported entries are only counted.
#[derive(Debug, Default, Serialize, ToSchema)]
pub struct KnowledgeSkipped {
    pub listed: Vec<KnowledgeSkippedFile>,
    pub secret: usize,
    pub too_large: usize,
    pub hidden: usize,
    pub unsupported: usize,
}

impl From<&SkipSummary> for KnowledgeSkipped {
    fn from(skipped: &SkipSummary) -> Self {
        Self {
            listed: skipped
                .listed
                .iter()
                .map(|file| KnowledgeSkippedFile {
                    locator: file.locator.clone(),
                    reason: match file.reason {
                        SkipReason::Secret => "secret",
                        SkipReason::TooLarge => "too_large",
                    }
                    .to_owned(),
                })
                .collect(),
            secret: skipped.secret,
            too_large: skipped.too_large,
            hidden: skipped.hidden,
            unsupported: skipped.unsupported,
        }
    }
}

/// What indexing a folder would read, before anything is read or stored.
#[derive(Debug, Serialize, ToSchema)]
pub struct KnowledgeFolderPreview {
    /// The first 200 files; `file_count` is the full count.
    pub files: Vec<KnowledgePreviewFile>,
    pub file_count: usize,
    pub total_bytes: u64,
    /// Files per kind: `pdf`, `docx`, `xlsx`, `text`, `web`, `code`.
    pub by_type: std::collections::BTreeMap<String, usize>,
    pub skipped: KnowledgeSkipped,
    /// Rough time to index on a typical computer, in seconds.
    pub estimated_seconds: u64,
}

impl From<FolderPreview> for KnowledgeFolderPreview {
    fn from(preview: FolderPreview) -> Self {
        Self {
            skipped: KnowledgeSkipped::from(&preview.skipped),
            files: preview
                .files
                .into_iter()
                .map(|file| KnowledgePreviewFile {
                    locator: file.locator,
                    bytes: file.bytes,
                })
                .collect(),
            file_count: preview.file_count,
            total_bytes: preview.total_bytes,
            by_type: preview.by_type,
            estimated_seconds: preview.estimated_seconds,
        }
    }
}

/// Show what choosing this folder would index. Reads no document. Owner only.
#[utoipa::path(post, path = "/v1/bots/{bot}/knowledge/folder-preview", tag = "knowledge", security(("bearer" = [])), params(("bot" = Uuid, Path)), request_body = KnowledgeFolderPreviewInput, responses((status = 200, body = KnowledgeFolderPreview), (status = 409, body = ErrorBody)))]
pub(crate) async fn preview_folder(
    State(s): State<AppState>,
    ApiPath(bot): ApiPath<Uuid>,
    ApiJson(input): ApiJson<KnowledgeFolderPreviewInput>,
) -> Result<Json<KnowledgeFolderPreview>, ApiError> {
    crate::knowledge::scope(&s, bot)?;
    let data_dir = s.data_dir();
    crate::knowledge::read_blocking(move || {
        let selected = SelectedSource::open_on_host(Path::new(&input.path), true, &data_dir, false)
            .map_err(crate::knowledge::err)?;
        Ok(Json(selected.preview().into()))
    })
    .await
}

/// Which indexed file to open.
#[derive(Debug, Deserialize)]
pub(crate) struct FileQuery {
    /// Path of the file inside the source (as in the citation link).
    locator: String,
    #[serde(default)]
    download: bool,
}

fn content_type(name: &str) -> &'static str {
    match Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "pdf" => "application/pdf",
        "md" | "markdown" | "txt" | "csv" | "tsv" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// Open the file behind a citation: its bytes, for a document that is indexed
/// from an enabled file or folder source of this bot. Owner only.
#[utoipa::path(get, path = "/v1/bots/{bot}/knowledge/sources/{source}/file", tag = "knowledge", security(("bearer" = [])), params(("bot" = Uuid, Path), ("source" = Uuid, Path), ("locator" = String, Query, description = "Path of the file inside the source"), ("download" = Option<bool>, Query, description = "Send as a download")), responses((status = 200, description = "File bytes with nosniff and sandbox headers", content_type = "application/octet-stream", body = String), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)))]
pub(crate) async fn get_source_file(
    State(s): State<AppState>,
    ApiPath((bot, id)): ApiPath<(Uuid, Uuid)>,
    Query(query): Query<FileQuery>,
) -> Result<Response, ApiError> {
    let owner = crate::knowledge::scope(&s, bot)?;
    let data_dir = s.data_dir();
    let locator = query.locator;
    let (name, bytes) = crate::knowledge::read_blocking(move || {
        let store = KnowledgeStore::open_existing(&data_dir, owner)
            .map_err(crate::knowledge::err)?
            .ok_or_else(|| ApiError::not_found("knowledge source"))?;
        let source = store
            .source(id)
            .map_err(crate::knowledge::err)?
            .filter(|source| matches!(source.kind, SourceKind::File | SourceKind::Folder))
            .filter(|source| source.enabled && !source.locator.starts_with("imported:"))
            .ok_or_else(|| ApiError::not_found("knowledge source"))?;
        // Only what is indexed can be opened: the file adapter stores each
        // document under `<path inside the source>#extracted-text`.
        let wanted = format!("{locator}#extracted-text");
        if !store
            .list_documents(id)
            .map_err(crate::knowledge::err)?
            .iter()
            .any(|document| document.locator == wanted)
        {
            return Err(ApiError::not_found("indexed file"));
        }
        let selected = SelectedSource::open_on_host(
            Path::new(&source.locator),
            source.kind == SourceKind::Folder,
            &data_dir,
            false,
        )
        .map_err(crate::knowledge::err)?;
        let (bytes, _) = selected
            .read_bytes(&locator)
            .map_err(crate::knowledge::err)?;
        let name = locator.rsplit('/').next().unwrap_or("file").to_owned();
        Ok((name, bytes))
    })
    .await?;
    let kind = if query.download {
        "application/octet-stream"
    } else {
        content_type(&name)
    };
    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, kind)
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(header::CONTENT_SECURITY_POLICY, "sandbox")
        .header(header::CACHE_CONTROL, "no-store");
    let safe_name = name.replace(['"', '\r', '\n', '\\'], "_");
    if let Ok(value) = HeaderValue::from_str(&format!(
        "{}; filename=\"{}\"",
        if query.download {
            "attachment"
        } else {
            "inline"
        },
        safe_name.escape_default()
    )) {
        response = response.header(header::CONTENT_DISPOSITION, value);
    }
    response
        .body(Body::from(bytes))
        .map_err(|e| ApiError::internal(e.to_string()))
        .map(IntoResponse::into_response)
}
