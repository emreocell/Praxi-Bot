//! Routes of the shareable bot profile feature (see [`crate::profile_share`]).
//!
//! * `GET  /v1/profile-share/templates`: the built-in gallery (read-only data).
//! * `POST /v1/profile-share/export`:    owner only; one bot to a file.
//! * `POST /v1/profile-share/preview`:   what a file would be allowed to do; no state change.
//! * `POST /v1/profile-share/import`:    owner only; creates the bot under the confirmed ceiling.
//!
//! Paired peers are refused for the whole prefix (the profile is the owner's
//! own business and carries persona text and permissions). Export and import
//! are audited.

use axum::{Json, extract::State, http::StatusCode};
use protocol::BotId;
use serde::{Deserialize, Serialize};
use storage::Bot;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    ApiError, AppState,
    dto::BotDto,
    error::ErrorBody,
    profile_share::{
        self, ImportContext, ImportOptions, ImportPreview, ScheduleIdea, SharePreset,
        SharedProfile, SharedSkill,
    },
    routes::{ApiJson, bot_dto},
};

fn bad_profile(msg: String) -> ApiError {
    ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "bad_profile", msg)
}

/// Body of `POST /v1/profile-share/export`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ExportInput {
    /// Bot to export.
    pub bot_id: Uuid,
    /// Also export the bot's text-prompt schedules as ideas (never as live schedules).
    #[serde(default)]
    pub include_schedules: bool,
}

/// An exported profile file.
#[derive(Debug, Serialize, ToSchema)]
pub struct ExportOutput {
    /// Suggested file name (`<name>.praxi-bot.json`).
    pub filename: String,
    /// The file's text.
    pub contents: String,
}

/// Body of `POST /v1/profile-share/preview` and `/import`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportInput {
    /// The file's text (at most 64 KiB).
    pub contents: String,
    /// Highest level the new bot may get; default `read-only`. The file can
    /// only be lowered to it, never raised.
    #[serde(default)]
    pub permission_level: Option<SharePreset>,
    /// Name override.
    #[serde(default)]
    pub name: Option<String>,
    /// Main model (`provider/model`). Required to import when the file's own
    /// model is not available here.
    #[serde(default)]
    pub model: Option<String>,
    /// Allow the whole computer for file tools. Default: no files until folders are chosen.
    #[serde(default)]
    pub computer_files: bool,
}

/// Result of an import.
#[derive(Debug, Serialize, ToSchema)]
pub struct ImportOutput {
    /// The new bot.
    pub bot: BotDto,
    /// What it was allowed.
    pub preview: ImportPreview,
}

fn options(input: &ImportInput) -> ImportOptions {
    ImportOptions {
        ceiling: input.permission_level.unwrap_or(SharePreset::ReadOnly),
        name: input.name.clone(),
        executor: input.model.clone(),
        computer_files: input.computer_files,
    }
}

fn context(s: &AppState) -> ImportContext {
    let installed_skills = s
        .skills_index()
        .map(|index| {
            index
                .list()
                .into_iter()
                .map(|sk| (sk.name, sk.version))
                .collect()
        })
        .unwrap_or_default();
    ImportContext {
        known_tools: s.agent().tool_names(),
        providers: s.agent().providers().ids(),
        installed_skills,
    }
}

/// The built-in template gallery.
#[utoipa::path(get, path = "/v1/profile-share/templates", tag = "bots",
    security(("bearer" = [])),
    responses((status = 200, body = Vec<SharedProfile>)))]
pub(crate) async fn profile_templates() -> Json<Vec<SharedProfile>> {
    Json(profile_share::templates())
}

/// Export one bot as a shareable profile file. Owner key required.
#[utoipa::path(post, path = "/v1/profile-share/export", tag = "bots",
    security(("bearer" = [])), request_body = ExportInput,
    responses((status = 200, body = ExportOutput), (status = 404, body = ErrorBody),
              (status = 422, body = ErrorBody)))]
pub(crate) async fn export_profile(
    State(s): State<AppState>,
    ApiJson(input): ApiJson<ExportInput>,
) -> Result<Json<ExportOutput>, ApiError> {
    let bot = s.bot_of_user(BotId(input.bot_id))?;
    let skills = bot_skill_refs(&s, bot.id);
    let schedules = if input.include_schedules {
        schedule_ideas(&s, bot.id)
    } else {
        Vec::new()
    };
    let profile =
        profile_share::export(&bot.name, &bot.config, skills, schedules).map_err(bad_profile)?;
    let (filename, contents) = profile_share::to_file(&profile);
    s.storage()
        .append_audit(
            s.user_id(),
            None,
            "user",
            "profile.export",
            &serde_json::json!({
                "bot_id": bot.id.0,
                "schedules": profile.schedules.len(),
                "skills": profile.skills.len(),
            }),
        )
        .map_err(ApiError::internal)?;
    Ok(Json(ExportOutput { filename, contents }))
}

fn bot_skill_refs(s: &AppState, bot: BotId) -> Vec<SharedSkill> {
    let (Ok(index), Ok(Some(row))) = (
        s.skills_index(),
        s.storage()
            .inspect_bot_skill_assignment(bot)
            .map(|l| match l {
                storage::SkillAssignmentLookup::Ok(row) => row,
                storage::SkillAssignmentLookup::Corrupt { .. } => None,
            }),
    ) else {
        return Vec::new();
    };
    let versions: Vec<(String, String)> = index
        .list()
        .into_iter()
        .map(|sk| (sk.name, sk.version))
        .collect();
    row.names
        .iter()
        .map(|n| SharedSkill {
            name: n.clone(),
            version: versions
                .iter()
                .find(|(v, _)| v == n)
                .map(|(_, ver)| ver.clone()),
        })
        .collect()
}

fn schedule_ideas(s: &AppState, bot: BotId) -> Vec<ScheduleIdea> {
    let Ok(store) = s.schedule_store() else {
        return Vec::new();
    };
    store
        .list_by_bot(bot)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|sc| {
            let scheduler::Payload::AgentPrompt { text } = sc.payload else {
                return None;
            };
            let when = match sc.spec {
                scheduler::ScheduleSpec::Cron(c) => Some(format!("cron {c}")),
                scheduler::ScheduleSpec::Every { seconds } => Some(format!("every {seconds} s")),
                scheduler::ScheduleSpec::At { .. } => None,
            };
            Some(ScheduleIdea {
                title: sc.name,
                prompt: text,
                when,
            })
        })
        .take(16)
        .collect()
}

fn plan(s: &AppState, input: &ImportInput) -> Result<profile_share::ImportPlan, ApiError> {
    let profile = profile_share::parse(&input.contents).map_err(bad_profile)?;
    profile_share::plan_import(&profile, &options(input), &context(s)).map_err(bad_profile)
}

/// Show what a profile file would be allowed to do. Creates nothing.
#[utoipa::path(post, path = "/v1/profile-share/preview", tag = "bots",
    security(("bearer" = [])), request_body = ImportInput,
    responses((status = 200, body = ImportPreview), (status = 422, body = ErrorBody)))]
pub(crate) async fn preview_profile(
    State(s): State<AppState>,
    ApiJson(input): ApiJson<ImportInput>,
) -> Result<Json<ImportPreview>, ApiError> {
    Ok(Json(plan(&s, &input)?.preview))
}

/// Create a bot from a profile file under the confirmed permission ceiling.
/// Owner key required.
#[utoipa::path(post, path = "/v1/profile-share/import", tag = "bots",
    security(("bearer" = [])), request_body = ImportInput,
    responses((status = 201, body = ImportOutput), (status = 422, body = ErrorBody)))]
pub(crate) async fn import_profile(
    State(s): State<AppState>,
    ApiJson(input): ApiJson<ImportInput>,
) -> Result<(StatusCode, Json<ImportOutput>), ApiError> {
    let plan = plan(&s, &input)?;
    if plan.executor.is_none() {
        return Err(bad_profile(
            "choose a model: the profile's own model is not available here".to_owned(),
        ));
    }
    let mut config = plan.config;
    // The same guards as `POST /v1/bots` (the caller is the owner).
    crate::peer::protect_execution_config(
        None,
        s.leader().config().mode == crate::host::HostMode::Host,
        None,
        &mut config,
    )?;
    // The "no folders yet" value is a constant of ours that the generic guard
    // would refuse (it insists on a folder); only the whole-computer choice
    // goes through it.
    if input.computer_files {
        crate::file_access::guard_files_config(&s, None, None, &mut config)?;
    }
    let _check: agent_core::BotProfile = serde_json::from_value(config.clone())
        .map_err(|e| bad_profile(format!("invalid bot profile: {e}")))?;
    let bot = Bot {
        id: BotId::new(),
        user_id: s.user_id(),
        name: plan.name,
        config,
        created_at_ms: storage::now_ms(),
    };
    let preview = plan.preview;
    let audit = serde_json::json!({
        "bot_id": bot.id.0,
        "granted_level": preview.granted_level.as_str(),
        "requested_level": preview.requested_level.as_str(),
        "notes": preview.notes.iter().map(|n| n.code.as_str()).collect::<Vec<_>>(),
    });
    let dto = tokio::task::spawn_blocking(move || {
        let _skill_guard = s.0.skill_mutations.lock();
        s.storage().insert_bot(&bot)?;
        s.refresh_profile(&bot);
        s.storage()
            .append_audit(s.user_id(), None, "user", "profile.import", &audit)
            .map_err(ApiError::internal)?;
        Ok::<_, ApiError>(bot_dto(bot, 1))
    })
    .await
    .map_err(|_| ApiError::internal("bot worker failed"))??;
    Ok((
        StatusCode::CREATED,
        Json(ImportOutput { bot: dto, preview }),
    ))
}
