//! Skill marketplace for the desktop app (`/v1/skills/market/*`).
//!
//! Browse a skill index, read what a skill would ask for BEFORE it is installed, install with an
//! explicit confirmation, update with a permission comparison, uninstall. Everything here is the
//! local owner's: peers are refused in the ingress registry and again in each handler.
//!
//! Rules enforced on the server (the UI only mirrors them):
//! * the install is bound to the fingerprint of the previewed content;
//! * a skill the scanner rates above Low needs `accept_risk`;
//! * an update that asks for more than the installed copy (new capabilities or a higher risk)
//!   needs `accept_more_access`;
//! * nothing is executed, secrets in skill text are refused, and the audit entry is written first.
//!
//! The default index is the one bundled with the app (`skills/index.json`); a custom index must be
//! an `https://` address on a named host. Index text is remote data: it is redacted and bounded.

use std::collections::BTreeSet;

use axum::Json;
use axum::extract::{Extension, Path, Query, State, rejection::JsonRejection};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use skills::hub::{
    CurlFetcher, Fetcher, FindingKind, GitCli, HubError, HubPaths, InstallOptions, InstallSource,
    InstalledSkill, Preview, RiskLevel, RiskReport, SkillHub, SourceSpec, scan_dir, validate_index,
};
use skills::owner::PreparedSkill;
use utoipa::ToSchema;

use crate::error::{ApiError, ErrorBody};
use crate::skill_hub::{HubFindingDto, hub_error, is_https_named_host};
use crate::skill_management::{
    DeleteSkillQuery, DeleteSkillResult, audit, check_secrets, input, local_owner, owner_error,
    using_bots,
};
use crate::{AppState, PeerIdentity};

const BUNDLED_INDEX: &str = include_str!("../../../skills/index.json");
/// Stands in for the bundled index when it is recorded as a skill's source, so an update re-reads
/// the index of the running app instead of a network address.
pub(crate) const BUNDLED_INDEX_URL: &str = "https://bundled.praxi.invalid/skills/index.json";
const MAX_TEXT: usize = 400;

/// Serves the bundled index for its stand-in address; everything else goes over curl.
struct MarketFetcher;

impl Fetcher for MarketFetcher {
    fn get(&self, url: &str) -> Result<Vec<u8>, HubError> {
        if url == BUNDLED_INDEX_URL {
            Ok(BUNDLED_INDEX.as_bytes().to_vec())
        } else {
            CurlFetcher.get(url)
        }
    }
}

pub(crate) fn market_hub(state: &AppState) -> SkillHub {
    SkillHub::with_backends(
        HubPaths::from_data_dir(&state.data_dir()),
        Box::new(MarketFetcher),
        Box::new(GitCli),
    )
}

/// Which index to read. Empty means the bundled one.
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MarketIndexInput {
    /// Custom `https://` index address; the bundled index when absent.
    pub url: Option<String>,
}

/// One skill an index offers. Display text only; nothing here was scanned yet.
#[derive(Debug, Serialize, ToSchema)]
pub struct MarketSkillDto {
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    pub author: Option<String>,
    pub license: Option<String>,
    pub homepage: Option<String>,
    /// Installed from a skill source (can be updated or removed here).
    pub installed: bool,
    /// A skill with this name already exists (built in or made by the owner): it cannot be
    /// installed over.
    pub already_available: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MarketIndexDto {
    /// `bundled` or `custom`.
    pub source: String,
    pub name: Option<String>,
    pub skills: Vec<MarketSkillDto>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MarketPreviewInput {
    /// Index to read; the bundled one when absent. Ignored for an update.
    pub index_url: Option<String>,
    pub name: String,
    /// Compare the installed copy with what its source offers now.
    #[serde(default)]
    pub update: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MarketInstalledSummary {
    pub version: String,
    /// `low`, `medium` or `high`.
    pub risk: String,
    pub needs: Vec<String>,
}

/// What the skill would ask for, with the scan, shown before anything is installed.
#[derive(Debug, Serialize, ToSchema)]
pub struct MarketPreviewDto {
    pub name: String,
    pub description: String,
    pub version: String,
    /// `low`, `medium` or `high`.
    pub risk: String,
    pub active: bool,
    pub files: usize,
    pub total_bytes: u64,
    /// Pass back as `accept` to install exactly this content.
    pub fingerprint: String,
    pub findings: Vec<HubFindingDto>,
    /// What the skill may ask the bot to do: `scripts`, `network`, `credentials`, `hidden`,
    /// `steering`, `large` or `active_files`.
    pub needs: Vec<String>,
    /// Risk is above Low: the install needs `accept_risk`.
    pub requires_risk_acceptance: bool,
    /// The installed copy, for an update.
    pub installed: Option<MarketInstalledSummary>,
    pub added_needs: Vec<String>,
    pub removed_needs: Vec<String>,
    /// An update asks for more than before: it needs `accept_more_access`.
    pub requires_access_confirmation: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MarketPreviewResponse {
    /// An update check found nothing newer.
    pub up_to_date: bool,
    pub preview: Option<MarketPreviewDto>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MarketInstallInput {
    pub index_url: Option<String>,
    pub name: String,
    #[serde(default)]
    pub update: bool,
    /// `fingerprint` of the preview the owner confirmed.
    pub accept: String,
    /// The owner accepts a skill the scanner rates above Low.
    #[serde(default)]
    pub accept_risk: bool,
    /// The owner accepts that an update asks for more than the installed copy.
    #[serde(default)]
    pub accept_more_access: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MarketInstalledDto {
    pub name: String,
    pub description: String,
    pub version: String,
    /// `bundled`, `index`, `git` or `local`.
    pub source_kind: String,
    /// Host of the source for `index` and `git`; empty otherwise.
    pub source_host: String,
    pub commit: Option<String>,
    /// `low`, `medium` or `high` at install time.
    pub risk: String,
    pub needs: Vec<String>,
    /// `ok`, `missing` (folder gone) or `modified` (edited after install).
    pub state: String,
    pub installed_at: String,
}

fn risk_label(risk: RiskLevel) -> &'static str {
    match risk {
        RiskLevel::Low => "low",
        RiskLevel::Medium => "medium",
        RiskLevel::High => "high",
    }
}

/// Capabilities the scan saw in a skill, as stable codes the UI words in plain language.
pub(crate) fn needs_of(report: &RiskReport) -> Vec<String> {
    let mut set: BTreeSet<&str> = report
        .findings
        .iter()
        .map(|f| match f.kind {
            FindingKind::Script => "scripts",
            FindingKind::Network => "network",
            FindingKind::Credential => "credentials",
            FindingKind::Obfuscation => "hidden",
            FindingKind::Injection => "steering",
            FindingKind::Oversized => "large",
        })
        .collect();
    if report.active {
        set.insert("active_files");
    }
    set.into_iter().map(str::to_owned).collect()
}

fn resolve_index(url: Option<String>) -> Result<(String, bool), ApiError> {
    match url.map(|u| u.trim().to_owned()).filter(|u| !u.is_empty()) {
        None => Ok((BUNDLED_INDEX_URL.to_owned(), false)),
        Some(u) if is_https_named_host(&u) && u != BUNDLED_INDEX_URL => Ok((u, true)),
        Some(_) => Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "skill_index_invalid",
            "use an https:// address for the skill list",
        )),
    }
}

fn clip(state: &AppState, text: &str) -> String {
    let text: String = text.chars().take(MAX_TEXT).collect();
    state.agent().redact_external_text(text)
}

fn str_field(state: &AppState, v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(|t| clip(state, t))
        .filter(|t| !t.is_empty())
}

fn catalog_names(state: &AppState) -> Result<Vec<skills::Skill>, ApiError> {
    let index = state.skills_index()?;
    index
        .catalog()
        .map(|(catalog, _)| catalog)
        .map_err(|_| owner_error(skills::owner::OwnerError::Storage))
}

pub(crate) fn index_with(
    state: &AppState,
    hub: &SkillHub,
    url: Option<String>,
) -> Result<MarketIndexDto, ApiError> {
    let (url, custom) = resolve_index(url)?;
    let bytes = if url == BUNDLED_INDEX_URL {
        BUNDLED_INDEX.as_bytes().to_vec()
    } else {
        hub.fetch_index_document(&url).map_err(|e| match e {
            HubError::Source(_) => ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "skill_index_invalid",
                "the skill list is not valid",
            ),
            _ => ApiError::new(
                StatusCode::BAD_GATEWAY,
                "skill_source_unreachable",
                "the skill list could not be fetched",
            ),
        })?
    };
    if !validate_index(&bytes).is_ok() {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "skill_index_invalid",
            "the skill list is not valid",
        ));
    }
    let doc: Value = serde_json::from_slice(&bytes).map_err(|_| {
        ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "skill_index_invalid",
            "the skill list is not valid",
        )
    })?;
    let hub_installed: BTreeSet<String> = hub
        .list()
        .map_err(|e| hub_error(&e))?
        .into_iter()
        .map(|s| s.entry.name)
        .collect();
    let existing: BTreeSet<String> = catalog_names(state)?.into_iter().map(|s| s.name).collect();
    let skills = doc["skills"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| {
            let name = e.get("name")?.as_str()?.to_owned();
            let installed = hub_installed.contains(&name);
            Some(MarketSkillDto {
                description: str_field(state, e, "description").unwrap_or_default(),
                tags: e
                    .get("tags")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(|t| clip(state, t))
                    .collect(),
                author: str_field(state, e, "author"),
                license: str_field(state, e, "license"),
                homepage: str_field(state, e, "homepage"),
                already_available: !installed && existing.contains(&name),
                installed,
                name,
            })
        })
        .collect();
    Ok(MarketIndexDto {
        source: if custom { "custom" } else { "bundled" }.into(),
        name: str_field(state, &doc, "name"),
        skills,
    })
}

fn installed_summary(hub: &SkillHub, name: &str) -> Option<MarketInstalledSummary> {
    let entry = hub
        .list()
        .ok()?
        .into_iter()
        .find(|s| s.entry.name == name)?;
    let report = scan_dir(&hub.paths().skills_dir.join(name)).ok();
    Some(MarketInstalledSummary {
        version: entry.entry.version,
        risk: risk_label(report.as_ref().map_or(entry.entry.risk, RiskReport::risk)).into(),
        needs: report.as_ref().map(needs_of).unwrap_or_default(),
    })
}

fn is_hub_installed(hub: &SkillHub, name: &str) -> Result<bool, ApiError> {
    Ok(hub
        .list()
        .map_err(|e| hub_error(&e))?
        .iter()
        .any(|s| s.entry.name == name))
}

fn preview_dto(state: &AppState, hub: &SkillHub, p: &Preview, update: bool) -> MarketPreviewDto {
    let redact = |text: &str| state.agent().redact_external_text(text.to_owned());
    let needs = needs_of(&p.report);
    let installed = update.then(|| installed_summary(hub, &p.name)).flatten();
    let (added, removed, more) = installed.as_ref().map_or_else(
        || (Vec::new(), Vec::new(), false),
        |old| {
            let before: BTreeSet<&String> = old.needs.iter().collect();
            let after: BTreeSet<&String> = needs.iter().collect();
            let added: Vec<String> = after.difference(&before).map(|s| (*s).clone()).collect();
            let removed: Vec<String> = before.difference(&after).map(|s| (*s).clone()).collect();
            let risk_up = rank(p.report.risk()) > rank_label(&old.risk);
            let more = !added.is_empty() || risk_up;
            (added, removed, more)
        },
    );
    MarketPreviewDto {
        name: redact(&p.name),
        description: redact(&p.description),
        version: redact(&p.version),
        risk: risk_label(p.report.risk()).into(),
        active: p.report.active,
        files: p.report.files,
        total_bytes: p.report.total_bytes,
        fingerprint: p.report.fingerprint.clone(),
        findings: p
            .report
            .findings
            .iter()
            .map(|f| HubFindingDto {
                severity: serde_json::to_value(f.severity)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_default(),
                kind: serde_json::to_value(f.kind)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_default(),
                file: f.file.clone(),
                line: f.line,
                message: redact(&f.message),
            })
            .collect(),
        needs,
        requires_risk_acceptance: p.report.risk() != RiskLevel::Low,
        installed,
        added_needs: added,
        removed_needs: removed,
        requires_access_confirmation: more,
    }
}

fn rank(risk: RiskLevel) -> u8 {
    match risk {
        RiskLevel::Low => 0,
        RiskLevel::Medium => 1,
        RiskLevel::High => 2,
    }
}

fn rank_label(label: &str) -> u8 {
    match label {
        "high" => 2,
        "medium" => 1,
        _ => 0,
    }
}

fn existing_conflict() -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "skill_conflict",
        "a skill with this name already exists",
    )
}

/// Preview an index entry (or an update of an installed skill); nothing is installed.
pub(crate) fn preview_market(
    state: &AppState,
    hub: &SkillHub,
    body: &MarketPreviewInput,
) -> Result<MarketPreviewResponse, ApiError> {
    if body.update {
        return match hub.preview_update(&body.name).map_err(|e| hub_error(&e))? {
            None => Ok(MarketPreviewResponse {
                up_to_date: true,
                preview: None,
            }),
            Some(p) => Ok(MarketPreviewResponse {
                up_to_date: false,
                preview: Some(preview_dto(state, hub, &p, true)),
            }),
        };
    }
    let (url, _) = resolve_index(body.index_url.clone())?;
    if is_hub_installed(hub, &body.name)?
        || catalog_names(state)?.iter().any(|s| s.name == body.name)
    {
        return Err(existing_conflict());
    }
    let p = hub
        .preview(&InstallSource::Index {
            url,
            name: body.name.clone(),
        })
        .map_err(|e| hub_error(&e))?;
    Ok(MarketPreviewResponse {
        up_to_date: false,
        preview: Some(preview_dto(state, hub, &p, false)),
    })
}

pub(crate) fn install_market(
    state: &AppState,
    hub: &SkillHub,
    body: &MarketInstallInput,
) -> Result<MarketInstalledDto, ApiError> {
    let staged = if body.update {
        hub.preview_update(&body.name).map_err(|e| hub_error(&e))?
    } else {
        let (url, _) = resolve_index(body.index_url.clone())?;
        if is_hub_installed(hub, &body.name)?
            || catalog_names(state)?.iter().any(|s| s.name == body.name)
        {
            return Err(existing_conflict());
        }
        Some(
            hub.preview(&InstallSource::Index {
                url,
                name: body.name.clone(),
            })
            .map_err(|e| hub_error(&e))?,
        )
    };
    let Some(preview) = staged else {
        // Nothing newer than the installed copy: report it as it is.
        return installed_one(state, hub, &body.name);
    };
    if !preview
        .report
        .fingerprint
        .eq_ignore_ascii_case(&body.accept)
    {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "skill_changed",
            "the skill changed since the preview; review it again",
        ));
    }
    if preview.report.risk() != RiskLevel::Low && !body.accept_risk {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "skill_risk_not_accepted",
            "this skill needs your explicit acceptance of its risk",
        ));
    }
    if body.update {
        let dto = preview_dto(state, hub, &preview, true);
        if dto.requires_access_confirmation && !body.accept_more_access {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "skill_access_not_accepted",
                "this update asks for more than the installed version; confirm it first",
            ));
        }
    }
    let prepared = PreparedSkill::folder(preview.root()).map_err(owner_error)?;
    check_secrets(state, &prepared)?;
    let index = state.skills_index()?;
    audit(
        state,
        if body.update {
            "skill.market_update_requested"
        } else {
            "skill.market_install_requested"
        },
        prepared.file_count(),
    )?;
    hub.install(
        &preview,
        &InstallOptions {
            accept: Some(preview.report.fingerprint.clone()),
            replace: body.update,
            trust_local: false,
        },
    )
    .map_err(|e| hub_error(&e))?;
    index.reload();
    installed_one(state, hub, &body.name)
}

fn source_parts(spec: &SourceSpec) -> (&'static str, String) {
    let host_of = |url: &str| {
        url.strip_prefix("https://")
            .or_else(|| url.strip_prefix("ssh://"))
            .and_then(|r| r.split(['/', '?', '#']).next())
            .unwrap_or("")
            .rsplit('@')
            .next()
            .unwrap_or("")
            .to_owned()
    };
    match spec {
        SourceSpec::Index { index_url, .. } if index_url == BUNDLED_INDEX_URL => {
            ("bundled", String::new())
        }
        SourceSpec::Index { index_url, .. } => ("index", host_of(index_url)),
        SourceSpec::Git { url, .. } => ("git", host_of(url)),
        SourceSpec::Local { .. } => ("local", String::new()),
    }
}

fn installed_dto(
    state: &AppState,
    hub: &SkillHub,
    s: &InstalledSkill,
    descriptions: &[skills::Skill],
) -> MarketInstalledDto {
    let (kind, host) = source_parts(&s.entry.source);
    let redact = |text: &str| state.agent().redact_external_text(text.to_owned());
    let needs = if s.present {
        scan_dir(&hub.paths().skills_dir.join(&s.entry.name))
            .map(|r| needs_of(&r))
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    MarketInstalledDto {
        name: redact(&s.entry.name),
        description: descriptions
            .iter()
            .find(|d| d.name == s.entry.name)
            .map(|d| redact(&d.description))
            .unwrap_or_default(),
        version: redact(&s.entry.version),
        source_kind: kind.into(),
        source_host: redact(&host),
        commit: s.entry.commit.clone(),
        risk: risk_label(s.entry.risk).into(),
        needs,
        state: match (s.present, s.modified) {
            (false, _) => "missing",
            (true, true) => "modified",
            _ => "ok",
        }
        .into(),
        installed_at: s.entry.installed_at.clone(),
    }
}

fn installed_one(
    state: &AppState,
    hub: &SkillHub,
    name: &str,
) -> Result<MarketInstalledDto, ApiError> {
    let descriptions = catalog_names(state)?;
    hub.list()
        .map_err(|e| hub_error(&e))?
        .iter()
        .find(|s| s.entry.name == name)
        .map(|s| installed_dto(state, hub, s, &descriptions))
        .ok_or_else(|| owner_error(skills::owner::OwnerError::NotFound))
}

pub(crate) fn list_installed(
    state: &AppState,
    hub: &SkillHub,
) -> Result<Vec<MarketInstalledDto>, ApiError> {
    let descriptions = catalog_names(state)?;
    Ok(hub
        .list()
        .map_err(|e| hub_error(&e))?
        .iter()
        .map(|s| installed_dto(state, hub, s, &descriptions))
        .collect())
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, ApiError> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| ApiError::internal("skill worker failed"))?
}

/// Read the skill list: the bundled one, or a custom `https://` index.
#[utoipa::path(post, path = "/v1/skills/market/index", tag = "skills", security(("bearer" = [])),
    request_body = MarketIndexInput,
    responses((status = 200, body = MarketIndexDto), (status = 403, body = ErrorBody),
              (status = 422, body = ErrorBody), (status = 502, body = ErrorBody)))]
pub(crate) async fn market_index(
    State(state): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    json: Result<Json<MarketIndexInput>, JsonRejection>,
) -> Result<Json<MarketIndexDto>, ApiError> {
    local_owner(peer.as_ref())?;
    let body = input(json)?;
    let dto = blocking(move || index_with(&state, &market_hub(&state), body.url)).await?;
    Ok(Json(dto))
}

/// Fetch one skill, scan it and compare it with the installed copy; nothing is installed.
#[utoipa::path(post, path = "/v1/skills/market/preview", tag = "skills", security(("bearer" = [])),
    request_body = MarketPreviewInput,
    responses((status = 200, body = MarketPreviewResponse), (status = 403, body = ErrorBody),
              (status = 404, body = ErrorBody), (status = 409, body = ErrorBody),
              (status = 422, body = ErrorBody), (status = 502, body = ErrorBody)))]
pub(crate) async fn market_preview(
    State(state): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    json: Result<Json<MarketPreviewInput>, JsonRejection>,
) -> Result<Json<MarketPreviewResponse>, ApiError> {
    local_owner(peer.as_ref())?;
    let body = input(json)?;
    let dto = blocking(move || preview_market(&state, &market_hub(&state), &body)).await?;
    Ok(Json(dto))
}

/// Install or update a skill the owner confirmed.
#[utoipa::path(post, path = "/v1/skills/market/install", tag = "skills", security(("bearer" = [])),
    request_body = MarketInstallInput,
    responses((status = 201, body = MarketInstalledDto), (status = 403, body = ErrorBody),
              (status = 409, body = ErrorBody), (status = 422, body = ErrorBody),
              (status = 502, body = ErrorBody)))]
pub(crate) async fn market_install(
    State(state): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    json: Result<Json<MarketInstallInput>, JsonRejection>,
) -> Result<(StatusCode, Json<MarketInstalledDto>), ApiError> {
    local_owner(peer.as_ref())?;
    let body = input(json)?;
    let dto = blocking(move || {
        let _guard = state.0.skill_mutations.lock();
        install_market(&state, &market_hub(&state), &body)
    })
    .await?;
    Ok((StatusCode::CREATED, Json(dto)))
}

/// Skills installed from a skill source, with version, source and what they ask for.
#[utoipa::path(get, path = "/v1/skills/market/installed", tag = "skills", security(("bearer" = [])),
    responses((status = 200, body = Vec<MarketInstalledDto>), (status = 403, body = ErrorBody)))]
pub(crate) async fn market_installed(
    State(state): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
) -> Result<Json<Vec<MarketInstalledDto>>, ApiError> {
    local_owner(peer.as_ref())?;
    let dto = blocking(move || list_installed(&state, &market_hub(&state))).await?;
    Ok(Json(dto))
}

pub(crate) fn uninstall_market(
    state: &AppState,
    hub: &SkillHub,
    name: &str,
    force: bool,
) -> Result<(StatusCode, DeleteSkillResult), ApiError> {
    if !is_hub_installed(hub, name)? {
        return Err(owner_error(skills::owner::OwnerError::NotFound));
    }
    let index = state.skills_index()?;
    state.agent().refresh_external_redactor();
    let used_by = using_bots(state, name)?;
    if !force && !used_by.is_empty() {
        return Ok((
            StatusCode::CONFLICT,
            DeleteSkillResult {
                deleted: false,
                used_by,
            },
        ));
    }
    audit(state, "skill.market_uninstall_requested", 0)?;
    hub.uninstall(name).map_err(|e| hub_error(&e))?;
    index.reload();
    Ok((
        StatusCode::OK,
        DeleteSkillResult {
            deleted: true,
            used_by,
        },
    ))
}

/// Remove an installed skill; refused while bots use it unless `force` is explicit.
#[utoipa::path(delete, path = "/v1/skills/market/installed/{name}", tag = "skills",
    security(("bearer" = [])),
    params(("name" = String, Path), ("force" = Option<bool>, Query)),
    responses((status = 200, body = DeleteSkillResult), (status = 403, body = ErrorBody),
              (status = 404, body = ErrorBody), (status = 409, body = DeleteSkillResult),
              (status = 422, body = ErrorBody)))]
pub(crate) async fn market_uninstall(
    State(state): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    Path(name): Path<String>,
    query: Result<Query<DeleteSkillQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Response, ApiError> {
    local_owner(peer.as_ref())?;
    skills::owner::validate_owner_name(&name).map_err(owner_error)?;
    let Query(query) = query.map_err(|_| ApiError::bad_request("invalid skill query"))?;
    let (status, dto) = blocking(move || {
        let _guard = state.0.skill_mutations.lock();
        uninstall_market(&state, &market_hub(&state), &name, query.force)
    })
    .await?;
    Ok((status, Json(dto)).into_response())
}
