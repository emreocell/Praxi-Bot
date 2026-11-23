//! Local-owner skill mutations. Peers are denied in the ingress registry and
//! again in each handler. Audit intent is committed before any filesystem
//! mutation, and no skill content, source path or user-selected name is audited.

use axum::Json;
use axum::extract::{Extension, Path, Query, State, rejection::JsonRejection};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use skills::owner::{OwnerError, OwnerSkillStore, PreparedSkill};
use utoipa::ToSchema;

use crate::error::{ApiError, ErrorBody};
use crate::{AppState, PeerIdentity};

/// A complete SKILL.md or the three fields used to author one.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(untagged)]
pub enum SkillDocumentInput {
    /// Complete Markdown including frontmatter.
    Markdown {
        /// Name/description frontmatter and Markdown instructions.
        skill_md: String,
        /// Optional optimistic version check when updating.
        expected_version: Option<String>,
    },
    /// Form fields, serialized into the same runtime SKILL.md format.
    Fields {
        /// Safe lowercase skill name.
        name: String,
        /// One-line description, at most 200 characters.
        description: String,
        /// Markdown instructions.
        instructions: String,
        /// Optional optimistic version check when updating.
        expected_version: Option<String>,
    },
}

impl SkillDocumentInput {
    fn prepare(self) -> Result<(PreparedSkill, Option<String>), ApiError> {
        let (text, expected) = match self {
            Self::Markdown {
                skill_md,
                expected_version,
            } => (skill_md, expected_version),
            Self::Fields {
                name,
                description,
                instructions,
                expected_version,
            } => {
                skills::owner::validate_owner_name(&name).map_err(owner_error)?;
                if description.chars().any(char::is_control) {
                    return Err(owner_error(OwnerError::Invalid));
                }
                let description = serde_json::to_string(&description)
                    .map_err(|_| owner_error(OwnerError::Invalid))?;
                (
                    format!("---\nname: {name}\ndescription: {description}\n---\n{instructions}\n"),
                    expected_version,
                )
            }
        };
        Ok((
            PreparedSkill::markdown(&text).map_err(owner_error)?,
            expected,
        ))
    }
}

/// Import one folder or a ZIP with SKILL.md at its root. No URL fetching.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SkillImportInput {
    /// Snapshot a local owner-selected folder.
    Folder {
        /// Absolute folder path. Links, special and executable files are refused.
        path: String,
    },
    /// Upload a bounded ZIP; bytes are never extracted into a caller path.
    Zip {
        /// Standard Base64 of a ZIP, compressed and expanded limit 8 MiB.
        base64: String,
    },
}

impl SkillImportInput {
    fn prepare(self) -> Result<PreparedSkill, ApiError> {
        match self {
            Self::Folder { path } => {
                let path = std::path::Path::new(&path);
                if !path.is_absolute() {
                    return Err(owner_error(OwnerError::Invalid));
                }
                PreparedSkill::folder(path).map_err(owner_error)
            }
            Self::Zip { base64 } => {
                if base64.len() > skills::owner::MAX_IMPORT_BYTES.div_ceil(3) * 4 {
                    return Err(owner_error(OwnerError::TooLarge));
                }
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(base64)
                    .map_err(|_| owner_error(OwnerError::Invalid))?;
                PreparedSkill::zip(&bytes).map_err(owner_error)
            }
        }
    }
}

/// Metadata only; the response never includes instruction bodies or files.
#[derive(Debug, Serialize, ToSchema)]
pub struct ManagedSkillDto {
    pub name: String,
    pub description: String,
    pub version: String,
    /// Always `shared` for owner-managed skills.
    pub scope: String,
}

/// Explicit opt-in for deleting an assigned skill.
#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct DeleteSkillQuery {
    #[serde(default)]
    pub force: bool,
}

/// Bots currently assigned to a skill, also returned on an assignment conflict.
#[derive(Debug, Serialize, ToSchema)]
pub struct SkillUsingBotDto {
    pub id: String,
    pub name: String,
}

/// Deletion result or assignment conflict. Force keeps explicit assignments as
/// missing, so a reinstall restores the owner's selected set.
#[derive(Debug, Serialize, ToSchema)]
pub struct DeleteSkillResult {
    pub deleted: bool,
    pub used_by: Vec<SkillUsingBotDto>,
}

pub(crate) fn local_owner(peer: Option<&Extension<PeerIdentity>>) -> Result<(), ApiError> {
    if peer.is_some() {
        Err(ApiError::forbidden(
            "only the local owner may manage shared skills",
        ))
    } else {
        Ok(())
    }
}

pub(crate) fn owner_error(error: OwnerError) -> ApiError {
    let (status, code) = match error {
        OwnerError::Invalid => (StatusCode::UNPROCESSABLE_ENTITY, "invalid_skill"),
        OwnerError::TooLarge => (StatusCode::PAYLOAD_TOO_LARGE, "skill_too_large"),
        OwnerError::Conflict => (StatusCode::CONFLICT, "skill_conflict"),
        OwnerError::NotFound => (StatusCode::NOT_FOUND, "skill_not_found"),
        OwnerError::Storage => (StatusCode::INTERNAL_SERVER_ERROR, "skill_storage"),
    };
    ApiError::new(status, code, error.to_string())
}

pub(crate) fn input<T>(json: Result<Json<T>, JsonRejection>) -> Result<T, ApiError> {
    json.map(|Json(value)| value)
        .map_err(|_| ApiError::bad_request("invalid skill request body"))
}

pub(crate) fn store(state: &AppState) -> OwnerSkillStore {
    OwnerSkillStore::new(state.data_dir().join("skills"))
}

pub(crate) fn audit(state: &AppState, action: &str, files: usize) -> Result<(), ApiError> {
    // Only fixed host-authored action and numeric counts enter the audit log.
    state
        .storage()
        .append_audit(
            state.user_id(),
            None,
            "local_owner",
            action,
            &serde_json::json!({"files": files}),
        )
        .map_err(|_| ApiError::internal("skill audit could not be persisted"))?;
    Ok(())
}

fn scrub(state: &AppState, skill: &skills::Skill) -> ManagedSkillDto {
    let redact = |text: &String| state.agent().redact_external_text(text.clone());
    ManagedSkillDto {
        name: redact(&skill.name),
        description: redact(&skill.description),
        version: redact(&skill.version),
        scope: "shared".into(),
    }
}

pub(crate) fn check_secrets(state: &AppState, prepared: &PreparedSkill) -> Result<(), ApiError> {
    state.agent().refresh_external_redactor();
    if prepared
        .contains_filtered_content(|text| state.agent().redact_external_text(text.to_owned()))
    {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "skill_contains_secret",
            "skill content contains a secret; use a secret-store reference instead",
        ));
    }
    Ok(())
}

pub(crate) fn create_prepared(
    state: &AppState,
    prepared: &PreparedSkill,
    action: &str,
) -> Result<ManagedSkillDto, ApiError> {
    let index = state.skills_index()?;
    check_secrets(state, prepared)?;
    let (catalog, _) = index
        .catalog()
        .map_err(|_| owner_error(OwnerError::Storage))?;
    if catalog.iter().any(|s| s.name == prepared.name()) {
        return Err(owner_error(OwnerError::Conflict));
    }
    audit(state, action, prepared.file_count())?;
    let result = store(state).create(prepared).map_err(owner_error)?;
    index.reload();
    Ok(scrub(state, &result))
}

/// Create a trusted shared skill authored by the local owner.
#[utoipa::path(post, path = "/v1/skills", tag = "skills", security(("bearer" = [])),
    request_body = SkillDocumentInput,
    responses((status = 201, body = ManagedSkillDto), (status = 403, body = ErrorBody),
              (status = 409, body = ErrorBody), (status = 422, body = ErrorBody)))]
pub(crate) async fn create(
    State(state): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    json: Result<Json<SkillDocumentInput>, JsonRejection>,
) -> Result<(StatusCode, Json<ManagedSkillDto>), ApiError> {
    local_owner(peer.as_ref())?;
    let (prepared, _) = input(json)?.prepare()?;
    let dto = tokio::task::spawn_blocking(move || {
        let _guard = state.0.skill_mutations.lock();
        create_prepared(&state, &prepared, "skill.create_requested")
    })
    .await
    .map_err(|_| ApiError::internal("skill worker failed"))??;
    Ok((StatusCode::CREATED, Json(dto)))
}

/// Import a complete bounded skill tree; no imported code is executed.
#[utoipa::path(post, path = "/v1/skills/import", tag = "skills", security(("bearer" = [])),
    request_body = SkillImportInput,
    responses((status = 201, body = ManagedSkillDto), (status = 403, body = ErrorBody),
              (status = 409, body = ErrorBody), (status = 413, body = ErrorBody),
              (status = 422, body = ErrorBody)))]
pub(crate) async fn import(
    State(state): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    json: Result<Json<SkillImportInput>, JsonRejection>,
) -> Result<(StatusCode, Json<ManagedSkillDto>), ApiError> {
    local_owner(peer.as_ref())?;
    let input = input(json)?;
    let dto = tokio::task::spawn_blocking(move || {
        let prepared = input.prepare()?;
        let _guard = state.0.skill_mutations.lock();
        create_prepared(&state, &prepared, "skill.import_requested")
    })
    .await
    .map_err(|_| ApiError::internal("skill worker failed"))??;
    Ok((StatusCode::CREATED, Json(dto)))
}

/// Atomically update SKILL.md and retain auxiliary files.
#[utoipa::path(put, path = "/v1/skills/{name}", tag = "skills", security(("bearer" = [])),
    params(("name" = String, Path)), request_body = SkillDocumentInput,
    responses((status = 200, body = ManagedSkillDto), (status = 403, body = ErrorBody),
              (status = 404, body = ErrorBody), (status = 409, body = ErrorBody),
              (status = 422, body = ErrorBody)))]
pub(crate) async fn update(
    State(state): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    Path(name): Path<String>,
    json: Result<Json<SkillDocumentInput>, JsonRejection>,
) -> Result<Json<ManagedSkillDto>, ApiError> {
    local_owner(peer.as_ref())?;
    skills::owner::validate_owner_name(&name).map_err(owner_error)?;
    let (prepared, expected) = input(json)?.prepare()?;
    let dto = tokio::task::spawn_blocking(move || {
        let _guard = state.0.skill_mutations.lock();
        let index = state.skills_index()?;
        check_secrets(&state, &prepared)?;
        audit(&state, "skill.update_requested", 1)?;
        let updated = store(&state)
            .update(&name, &prepared, expected.as_deref())
            .map_err(owner_error)?;
        index.reload();
        Ok::<_, ApiError>(scrub(&state, &updated))
    })
    .await
    .map_err(|_| ApiError::internal("skill worker failed"))??;
    Ok(Json(dto))
}

pub(crate) fn using_bots(state: &AppState, name: &str) -> Result<Vec<SkillUsingBotDto>, ApiError> {
    let bots = state
        .storage()
        .list_bots(state.user_id())
        .map_err(|_| owner_error(OwnerError::Storage))?;
    let mut result = Vec::new();
    for bot in bots {
        let assignment = state
            .storage()
            .get_bot_skill_assignment(bot.id)
            .map_err(|_| owner_error(OwnerError::Storage))?;
        let assigned = assignment.is_none_or(|a| {
            a.mode == storage::SkillAssignmentMode::All || a.names.iter().any(|n| n == name)
        });
        if assigned {
            result.push(SkillUsingBotDto {
                id: bot.id.to_string(),
                name: state.agent().redact_external_text(bot.name),
            });
        }
    }
    Ok(result)
}

/// Refuse assigned skills unless force is explicit; report their bot IDs/names.
#[utoipa::path(delete, path = "/v1/skills/{name}", tag = "skills", security(("bearer" = [])),
    params(("name" = String, Path), ("force" = Option<bool>, Query)),
    responses((status = 200, body = DeleteSkillResult), (status = 403, body = ErrorBody),
              (status = 404, body = ErrorBody), (status = 409, body = DeleteSkillResult),
              (status = 422, body = ErrorBody)))]
pub(crate) async fn delete(
    State(state): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    Path(name): Path<String>,
    query: Result<Query<DeleteSkillQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Response, ApiError> {
    local_owner(peer.as_ref())?;
    skills::owner::validate_owner_name(&name).map_err(owner_error)?;
    let Query(query) = query.map_err(|_| ApiError::bad_request("invalid skill query"))?;
    let (status, dto) = tokio::task::spawn_blocking(move || {
        let _guard = state.0.skill_mutations.lock();
        let index = state.skills_index()?;
        state.agent().refresh_external_redactor();
        store(&state).get(&name).map_err(owner_error)?;
        let used_by = using_bots(&state, &name)?;
        if !query.force && !used_by.is_empty() {
            return Ok((
                StatusCode::CONFLICT,
                DeleteSkillResult {
                    deleted: false,
                    used_by,
                },
            ));
        }
        audit(&state, "skill.delete_requested", 0)?;
        store(&state).delete(&name).map_err(owner_error)?;
        index.reload();
        Ok::<_, ApiError>((
            StatusCode::OK,
            DeleteSkillResult {
                deleted: true,
                used_by,
            },
        ))
    })
    .await
    .map_err(|_| ApiError::internal("skill worker failed"))??;
    Ok((status, Json(dto)).into_response())
}
