//! Install a skill from a git repository with a safety scan first (`/v1/skills/hub/*`).
//!
//! Two stateless steps. `preview` fetches the repository into a private staging folder, scans it
//! and returns the findings plus a fingerprint of exactly that content. `install` fetches again and
//! installs only when the fresh fingerprint equals the one the owner accepted, so a repository that
//! changed in between is refused. The install itself is the owner-managed path (`/v1/skills/import`):
//! links, special and executable files are refused, secrets are refused, nothing is executed.
//! Only the local owner may call these routes; only `https://` repositories on a named host.

use axum::Json;
use axum::extract::{Extension, State, rejection::JsonRejection};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use skills::hub::{HubError, HubPaths, InstallSource, Preview, SkillHub};
use skills::owner::PreparedSkill;
use utoipa::ToSchema;

use crate::error::{ApiError, ErrorBody};
use crate::skill_management::{ManagedSkillDto, create_prepared, input, local_owner, owner_error};
use crate::{AppState, PeerIdentity};

/// Where the skill lives.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HubSourceInput {
    /// `https://` clone URL of a public repository.
    pub url: String,
    /// Branch, tag or full commit; the default branch when absent.
    pub rev: Option<String>,
    /// Folder of the skill inside the repository; the repository root when absent.
    pub subdir: Option<String>,
}

/// Install request: the source plus the fingerprint of the previewed content.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HubInstallInput {
    /// `https://` clone URL of a public repository.
    pub url: String,
    /// Branch, tag or full commit; the default branch when absent.
    pub rev: Option<String>,
    /// Folder of the skill inside the repository; the repository root when absent.
    pub subdir: Option<String>,
    /// `fingerprint` from the preview the owner accepted.
    pub accept: String,
}

/// One scan finding. The text never contains a matched secret.
#[derive(Debug, Serialize, ToSchema)]
pub struct HubFindingDto {
    /// `info`, `warn` or `high`.
    pub severity: String,
    /// `script`, `network`, `credential`, `obfuscation`, `injection` or `oversized`.
    pub kind: String,
    pub file: String,
    pub line: Option<usize>,
    pub message: String,
}

/// What the scan found, shown before anything is installed.
#[derive(Debug, Serialize, ToSchema)]
pub struct HubPreviewDto {
    pub name: String,
    pub description: String,
    pub version: String,
    /// `low`, `medium` or `high`.
    pub risk: String,
    /// True when the folder holds scripts or other active files (stored, never run).
    pub active: bool,
    pub files: usize,
    pub total_bytes: u64,
    /// Pass this back as `accept` to install exactly this content.
    pub fingerprint: String,
    pub findings: Vec<HubFindingDto>,
}

fn label<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

pub(crate) fn hub_error(error: &HubError) -> ApiError {
    // The hub's own text can echo remote output; the API keeps one fixed sentence per class.
    let (status, code, message) = match error {
        HubError::Source(_) | HubError::InvalidSkill(_) | HubError::Unsafe(_) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_skill",
            "the repository does not hold a valid, safe skill",
        ),
        HubError::AlreadyInstalled(_) => (
            StatusCode::CONFLICT,
            "skill_conflict",
            "a skill with this name is already installed",
        ),
        HubError::NotInstalled(_) => (
            StatusCode::NOT_FOUND,
            "skill_not_found",
            "this skill was not installed from a skill source",
        ),
        HubError::HashMismatch { .. } => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_skill",
            "the skill does not match the fingerprint its index lists",
        ),
        HubError::TooLarge(_) => (
            StatusCode::PAYLOAD_TOO_LARGE,
            "skill_too_large",
            "the skill is too large",
        ),
        HubError::Git(_) => (
            StatusCode::BAD_GATEWAY,
            "skill_source_unreachable",
            "the repository could not be fetched",
        ),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "skill_storage",
            "the skill could not be prepared",
        ),
    };
    ApiError::new(status, code, message)
}

/// `https://` on a named host: no credentials, no IP literal, no `localhost`.
pub(crate) fn is_https_named_host(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let numeric = host
        .chars()
        .all(|c| c.is_ascii_digit() || c == '.' || c == ':');
    !(host.is_empty()
        || host.contains(['@', '[', ']'])
        || !host.contains('.')
        || numeric
        || host.eq_ignore_ascii_case("localhost"))
}

/// Only `https://` on a named host: no credentials, no IP literal, no `localhost`.
fn source_of(
    url: String,
    rev: Option<String>,
    subdir: Option<String>,
) -> Result<InstallSource, ApiError> {
    if !is_https_named_host(&url) {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_skill",
            "use an https:// repository address",
        ));
    }
    Ok(InstallSource::Git {
        url,
        rev: rev.filter(|r| !r.is_empty()),
        subdir: subdir.filter(|s| !s.is_empty()),
    })
}

fn preview_dto(state: &AppState, p: &Preview) -> HubPreviewDto {
    let redact = |text: &str| state.agent().redact_external_text(text.to_owned());
    HubPreviewDto {
        name: redact(&p.name),
        description: redact(&p.description),
        version: redact(&p.version),
        risk: label(&p.report.risk()),
        active: p.report.active,
        files: p.report.files,
        total_bytes: p.report.total_bytes,
        fingerprint: p.report.fingerprint.clone(),
        findings: p
            .report
            .findings
            .iter()
            .map(|f| HubFindingDto {
                severity: label(&f.severity),
                kind: label(&f.kind),
                file: f.file.clone(),
                line: f.line,
                message: redact(&f.message),
            })
            .collect(),
    }
}

fn hub_for(state: &AppState) -> SkillHub {
    SkillHub::new(HubPaths::from_data_dir(&state.data_dir()))
}

pub(crate) fn preview_with(
    state: &AppState,
    hub: &SkillHub,
    source: &InstallSource,
) -> Result<HubPreviewDto, ApiError> {
    let preview = hub.preview(source).map_err(|e| hub_error(&e))?;
    Ok(preview_dto(state, &preview))
}

pub(crate) fn install_with(
    state: &AppState,
    hub: &SkillHub,
    source: &InstallSource,
    accept: &str,
) -> Result<ManagedSkillDto, ApiError> {
    let preview = hub.preview(source).map_err(|e| hub_error(&e))?;
    if !preview.report.fingerprint.eq_ignore_ascii_case(accept) {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "skill_changed",
            "the repository changed since the preview; review it again",
        ));
    }
    let prepared = PreparedSkill::folder(preview.root()).map_err(owner_error)?;
    create_prepared(state, &prepared, "skill.hub_install_requested")
}

/// Fetch a repository into a private staging folder and scan it; nothing is installed.
#[utoipa::path(post, path = "/v1/skills/hub/preview", tag = "skills", security(("bearer" = [])),
    request_body = HubSourceInput,
    responses((status = 200, body = HubPreviewDto), (status = 403, body = ErrorBody),
              (status = 422, body = ErrorBody), (status = 502, body = ErrorBody)))]
pub(crate) async fn hub_preview(
    State(state): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    json: Result<Json<HubSourceInput>, JsonRejection>,
) -> Result<Json<HubPreviewDto>, ApiError> {
    local_owner(peer.as_ref())?;
    let body = input(json)?;
    let source = source_of(body.url, body.rev, body.subdir)?;
    let dto = tokio::task::spawn_blocking(move || preview_with(&state, &hub_for(&state), &source))
        .await
        .map_err(|_| ApiError::internal("skill worker failed"))??;
    Ok(Json(dto))
}

/// Install the previewed content when it is still exactly what the owner accepted.
#[utoipa::path(post, path = "/v1/skills/hub/install", tag = "skills", security(("bearer" = [])),
    request_body = HubInstallInput,
    responses((status = 201, body = ManagedSkillDto), (status = 403, body = ErrorBody),
              (status = 409, body = ErrorBody), (status = 422, body = ErrorBody),
              (status = 502, body = ErrorBody)))]
pub(crate) async fn hub_install(
    State(state): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    json: Result<Json<HubInstallInput>, JsonRejection>,
) -> Result<(StatusCode, Json<ManagedSkillDto>), ApiError> {
    local_owner(peer.as_ref())?;
    let body = input(json)?;
    let source = source_of(body.url, body.rev, body.subdir)?;
    let accept = body.accept;
    let dto = tokio::task::spawn_blocking(move || {
        let _guard = state.0.skill_mutations.lock();
        install_with(&state, &hub_for(&state), &source, &accept)
    })
    .await
    .map_err(|_| ApiError::internal("skill worker failed"))??;
    Ok((StatusCode::CREATED, Json(dto)))
}
