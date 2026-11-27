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
    FolderPreview, SelectedSource, SkipReason, SkipSummary, valid_locator,
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
    // A credential, browser-profile or daemon-state folder is not listed at all,
    // not even its sub-folder names, and the refusal reads like any unopenable
    // folder so it does not confirm that the folder exists.
    let dir = std::fs::canonicalize(&start)
        .ok()
        .filter(|dir| dir.is_dir())
        .filter(|dir| SelectedSource::folder_browsable_on_host(dir, data_dir))
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
/// Credential, browser-profile and daemon-state folders are refused (400).
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

/// Whether the bytes are a PDF: the signature sits in the first kilobyte.
fn looks_like_pdf(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(1024)]
        .windows(5)
        .any(|window| window == b"%PDF-")
}

/// The file name for a `Content-Disposition` header: the last path segment with
/// quotes, backslashes, control characters and path separators replaced, so the
/// header cannot be split or carry a path.
fn download_name(locator: &str) -> String {
    let name = locator.rsplit('/').next().unwrap_or_default();
    let name: String = name
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '"' | '\\' | '/' | ':' | ';' | '%') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let name = name.trim().trim_matches('.').to_owned();
    if name.is_empty() {
        "file".to_owned()
    } else {
        name
    }
}

/// `attachment; filename="<ascii>"; filename*=UTF-8''<encoded>`.
fn attachment_header(name: &str) -> Option<HeaderValue> {
    let ascii: String = name
        .chars()
        .map(|c| if c.is_ascii() { c } else { '_' })
        .collect();
    let encoded = percent_encoding::utf8_percent_encode(name, percent_encoding::NON_ALPHANUMERIC);
    HeaderValue::from_str(&format!(
        "attachment; filename=\"{ascii}\"; filename*=UTF-8''{encoded}"
    ))
    .ok()
}

/// Open the file behind a citation: its bytes, for a document that is indexed
/// from an enabled file or folder source of this bot. Owner only.
///
/// The file is always sent as an attachment (`application/octet-stream`), so a
/// browser that navigates here saves it instead of rendering it. The one
/// exception is the PDF viewer path: a `.pdf` whose bytes start like a PDF and
/// that was not asked for as a download is sent inline as `application/pdf`.
#[utoipa::path(get, path = "/v1/bots/{bot}/knowledge/sources/{source}/file", tag = "knowledge", security(("bearer" = [])), params(("bot" = Uuid, Path), ("source" = Uuid, Path), ("locator" = String, Query, description = "Path of the file inside the source"), ("download" = Option<bool>, Query, description = "Force a download even for a PDF")), responses((status = 200, description = "File bytes with attachment, nosniff, sandbox, frame-deny and no-store headers", content_type = "application/octet-stream", body = String), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)))]
pub(crate) async fn get_source_file(
    State(s): State<AppState>,
    ApiPath((bot, id)): ApiPath<(Uuid, Uuid)>,
    Query(query): Query<FileQuery>,
) -> Result<Response, ApiError> {
    let owner = crate::knowledge::scope(&s, bot)?;
    let data_dir = s.data_dir();
    let locator = query.locator;
    if !valid_locator(&locator) {
        return Err(ApiError::bad_request("that file path is not valid"));
    }
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
        // document under `<path inside the source>#extracted-text`. The match is
        // exact (letter case included); `read_bytes` repeats it against a fresh
        // scan, then opens the file through a no-follow handle under the source
        // root and checks that it resolves to exactly that path.
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
        Ok((download_name(&locator), bytes))
    })
    .await?;
    file_response(&name, bytes, query.download)
}

/// The response for an opened file (see [`get_source_file`] for the rules).
pub(crate) fn file_response(
    name: &str,
    bytes: Vec<u8>,
    download: bool,
) -> Result<Response, ApiError> {
    let viewer = !download
        && Path::new(name)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
        && looks_like_pdf(&bytes);
    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(header::X_FRAME_OPTIONS, "DENY")
        .header(header::CONTENT_SECURITY_POLICY, "sandbox")
        .header(header::CACHE_CONTROL, "no-store");
    if viewer {
        response = response
            .header(header::CONTENT_TYPE, "application/pdf")
            .header(header::CONTENT_DISPOSITION, "inline");
    } else {
        response = response
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .header(
                header::CONTENT_DISPOSITION,
                attachment_header(name).unwrap_or_else(|| HeaderValue::from_static("attachment")),
            );
    }
    response
        .body(Body::from(bytes))
        .map_err(|e| ApiError::internal(e.to_string()))
        .map(IntoResponse::into_response)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header_of(response: &Response, name: header::HeaderName) -> &str {
        response.headers().get(name).unwrap().to_str().unwrap()
    }

    #[test]
    fn every_file_is_an_attachment_except_a_real_pdf_for_the_viewer() {
        let pdf = b"%PDF-1.7\n1 0 obj".to_vec();
        let html = b"<script>alert(1)</script>".to_vec();
        for (name, bytes, download, inline) in [
            ("lease.pdf", pdf.clone(), false, true),
            ("LEASE.PDF", pdf.clone(), false, true),
            ("lease.pdf", pdf.clone(), true, false),
            // A "pdf" that is not one is never rendered by the browser.
            ("trick.pdf", html.clone(), false, false),
            ("notes.txt", b"hello".to_vec(), false, false),
            ("page.html", html.clone(), false, false),
            ("page.svg", html, false, false),
            ("budget.csv", b"a,b".to_vec(), false, false),
        ] {
            let response = file_response(name, bytes, download).unwrap();
            let disposition = header_of(&response, header::CONTENT_DISPOSITION);
            assert_eq!(disposition == "inline", inline, "{name} {download}");
            if inline {
                assert_eq!(
                    header_of(&response, header::CONTENT_TYPE),
                    "application/pdf"
                );
            } else {
                assert!(
                    disposition.starts_with("attachment;"),
                    "{name}: {disposition}"
                );
                assert_eq!(
                    header_of(&response, header::CONTENT_TYPE),
                    "application/octet-stream",
                    "{name}"
                );
            }
            assert_eq!(header_of(&response, header::X_FRAME_OPTIONS), "DENY");
            assert_eq!(
                header_of(&response, header::X_CONTENT_TYPE_OPTIONS),
                "nosniff"
            );
            assert_eq!(
                header_of(&response, header::CONTENT_SECURITY_POLICY),
                "sandbox"
            );
            assert_eq!(header_of(&response, header::CACHE_CONTROL), "no-store");
        }
    }

    #[test]
    fn download_names_cannot_split_the_header_or_carry_a_path() {
        for locator in [
            "housing/lease.md",
            "a/b\"c\r\nSet-Cookie: x=y.txt",
            "dir/..%2f..%2fx.txt",
            "dir/we\\ird;name.txt",
        ] {
            let name = download_name(locator);
            assert!(!name.contains(['\r', '\n', '"', '/', '\\']), "{name}");
            let header = attachment_header(&name).expect("a valid header value");
            assert!(
                header
                    .to_str()
                    .unwrap()
                    .starts_with("attachment; filename=\"")
            );
        }
        assert_eq!(download_name("dir/lease.md"), "lease.md");
        assert_eq!(download_name("dir/.."), "file");
        let header = attachment_header("kira sözleşmesi.pdf").unwrap();
        let header = header.to_str().unwrap();
        assert!(header.is_ascii());
        assert!(
            header.contains("filename=\"kira s_zle_mesi.pdf\""),
            "{header}"
        );
        assert!(
            header.contains("filename*=UTF-8''kira%20s%C3%B6zle%C5%9Fmesi%2Epdf"),
            "{header}"
        );
    }
}
