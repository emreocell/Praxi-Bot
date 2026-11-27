//! Owner-only engine settings of a bot: who runs its file and shell actions.
//!
//! `PUT /v1/bots/{id}/engine` and `POST /v1/bots/{id}/engine/kill` are the only
//! ways to change the stored `engine` table. Bot create / update refuse it, peers
//! are refused by the guard, and no tool can reach either route.
use crate::{ApiError, AppState, dto::BotDto, error::ErrorBody, live_view::Actor, routes::ApiJson};
use agent_core::{EngineConsent, EngineSettings};
use axum::{
    Extension, Json,
    extract::{Path, State},
};
use cli_provider::{ApprovalLevel, Driver, EngineMode, level_plan};
use protocol::BotId;
use serde::Deserialize;
use serde_json::{Value, json};
use utoipa::ToSchema;

/// New engine settings for one bot.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct BotEngineInput {
    /// `praxi_governed` (Praxi runs files and shell) or `native_tools` (the CLI does).
    pub mode: String,
    /// `ask` (default), `accept_edits`, `auto` or `full`.
    #[serde(default)]
    pub approval_level: Option<String>,
    /// Absolute workspace folder for `native_tools`; when omitted the bot keeps the folder it
    /// has, else the daemon uses the bot's own workspace folder under the workspaces root.
    #[serde(default)]
    pub workspace: Option<String>,
    /// The owner confirms that a CLI runs this bot's file and shell actions.
    #[serde(default)]
    pub consent_native_tools: bool,
    /// The owner confirms Full access (no approval prompts) for this bot.
    #[serde(default)]
    pub consent_full_access: bool,
    /// The app applied this engine without a dialog because the bot is on Full access (the owner's
    /// pick of the engine is the consent). The daemon checks the stored permission preset itself:
    /// anything but Full is refused (403), and the level must be `full`.
    #[serde(default)]
    pub full_access_bot: bool,
}

fn refuse(message: impl Into<String>) -> ApiError {
    ApiError::new(
        axum::http::StatusCode::UNPROCESSABLE_ENTITY,
        "bad_engine",
        message,
    )
}

/// The native driver behind an executor model reference (`cli:claude/sonnet`).
fn executor_driver(executor: &str) -> Option<Driver> {
    let id = executor.strip_prefix("cli:")?.split('/').next()?;
    Driver::from_id(id)
}

/// The folder a bot works in when the owner did not choose one: the bot's own workspace folder
/// under the workspaces root (`/var/lib/praxi-work/<bot>` on a host, `<Documents>/Praxi/<bot>` on a
/// desktop). That root is checked against Praxi's data folder and the credential folders before
/// anything is created, and the bot's folder is made private (a plain directory, no links). The
/// result passes the same folder rule as a folder the owner chose, with the real home folder and the
/// data folder of this daemon. A daemon without a workspaces root has no default: nothing is created
/// under the home folder (on a host that is the data folder itself), and the owner picks a folder.
fn default_workspace(s: &AppState, bot: &storage::Bot) -> Option<String> {
    let data_dir = s.storage().data_dir();
    let dir = s.agent().bot_workspace_dir(bot.id)?;
    vetted_folder(&dir, data_dir).map(|canonical| canonical.display().to_string())
}

/// A folder the daemon picked itself (the bot's own, or the one it already has) is used only when
/// it is a real directory, not a link or junction, and its canonical path passes the folder rule
/// with the daemon's real home folder and data folder. Returns that canonical path.
fn vetted_folder(dir: &std::path::Path, data_dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let meta = std::fs::symlink_metadata(dir).ok()?;
    if !meta.is_dir() || meta.file_type().is_symlink() || is_reparse_point(&meta) {
        return None;
    }
    let canonical = storage::without_verbatim_prefix(std::fs::canonicalize(dir).ok()?);
    builtin_tools::workspace_refusal_with_home(
        &canonical,
        data_dir,
        builtin_tools::service_home().as_deref(),
    )
    .is_none()
    .then_some(canonical)
}

#[cfg(windows)]
fn is_reparse_point(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    meta.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn is_reparse_point(_: &std::fs::Metadata) -> bool {
    false
}

/// Validate a change and return the settings to store. Pure apart from the
/// workspace folder check; consent flags are required on each widening switch.
fn next_settings(
    current: &EngineSettings,
    executor: &str,
    input: &BotEngineInput,
    data_dir: &std::path::Path,
    now_ms: i64,
) -> Result<EngineSettings, ApiError> {
    let mode: EngineMode = input.mode.parse().map_err(refuse)?;
    if mode == EngineMode::PraxiGoverned {
        return Ok(EngineSettings::default());
    }
    let driver = executor_driver(executor).ok_or_else(|| {
        refuse("choose a coding CLI model for this bot (a `cli:<engine>` model) before using its own tools")
    })?;
    let level: ApprovalLevel = input
        .approval_level
        .as_deref()
        .map_or(Ok(ApprovalLevel::Ask), str::parse)
        .map_err(refuse)?;
    let plan = level_plan(driver, level);
    if !plan.offered {
        return Err(refuse(format!(
            "{} has no \"{}\" level; it would run as \"{}\"",
            driver.display_name(),
            level.as_str(),
            plan.applied.as_str()
        )));
    }
    let workspace = input
        .workspace
        .as_deref()
        .map(str::trim)
        .filter(|w| !w.is_empty())
        .ok_or_else(|| refuse("a workspace folder is required"))?;
    if let Some(why) = builtin_tools::workspace_refusal_with_home(
        std::path::Path::new(workspace),
        data_dir,
        builtin_tools::service_home().as_deref(),
    ) {
        return Err(refuse(why));
    }
    let mut consent: EngineConsent = current.consent.clone();
    let was_native = current.mode == EngineMode::NativeTools;
    if !was_native {
        if !input.consent_native_tools {
            return Err(refuse(
                "consent is required: the CLI will run file and shell actions itself",
            ));
        }
        consent.native_tools_at_ms = Some(now_ms);
    }
    if level == ApprovalLevel::Full
        && !(was_native && current.approval_level == ApprovalLevel::Full)
    {
        if !input.consent_full_access {
            return Err(refuse(
                "consent is required for Full access: the CLI will not ask before acting",
            ));
        }
        consent.full_access_at_ms = Some(now_ms);
    }
    Ok(EngineSettings {
        mode,
        approval_level: level,
        workspace: Some(workspace.to_owned()),
        consent,
    })
}

fn summary(e: &EngineSettings) -> Value {
    json!({
        "mode": e.mode.as_str(),
        "approval_level": e.approval_level.as_str(),
        "workspace": e.workspace,
    })
}

fn store(s: &AppState, bot: &mut storage::Bot, settings: &EngineSettings) -> Result<i64, ApiError> {
    let obj = bot
        .config
        .as_object_mut()
        .ok_or_else(|| ApiError::bad_request("bot config must be an object"))?;
    obj.insert(
        "engine".into(),
        serde_json::to_value(settings).map_err(ApiError::internal)?,
    );
    let (_, current) = s
        .storage()
        .get_bot_versioned(bot.id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("bot"))?;
    match s
        .storage()
        .update_bot(bot.id, &bot.name, &bot.config, current)
        .map_err(ApiError::internal)?
    {
        storage::BotPatch::Applied {
            bot: stored,
            version,
        } => {
            s.refresh_profile(&stored);
            *bot = stored;
            Ok(version)
        }
        storage::BotPatch::Stale { .. } => Err(ApiError::conflict(
            "the bot changed while the engine was being saved; retry",
        )),
        _ => Err(ApiError::not_found("bot")),
    }
}

fn dto(bot: storage::Bot, version: i64) -> BotDto {
    crate::routes::bot_dto(bot, version)
}

/// Set a bot's engine mode, approval level and workspace (local owner only).
#[utoipa::path(put, path = "/v1/bots/{id}/engine", tag = "bots", security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "Bot id")),
    request_body = BotEngineInput,
    responses((status = 200, body = BotDto), (status = 403, body = ErrorBody),
              (status = 404, body = ErrorBody), (status = 422, body = ErrorBody)))]
pub(crate) async fn put_engine(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    Path(bid): Path<String>,
    ApiJson(mut input): ApiJson<BotEngineInput>,
) -> Result<Json<BotDto>, ApiError> {
    crate::cli_engines::local_owner(&actor)?;
    let mut bot = s.bot_of_user(BotId(crate::routes::id(&bid)?))?;
    let profile: agent_core::BotProfile =
        serde_json::from_value(bot.config.clone()).map_err(ApiError::internal)?;
    // "No dialog because the bot is on Full access" is checked here, from the stored preset (the
    // one spelling rule of the runtime), never taken from the client.
    if input.full_access_bot {
        if profile.permissions.mode() != permission_engine::AutonomyMode::Full {
            return Err(ApiError::forbidden(
                "this bot is not on Full access, so its engine needs the owner's own consent",
            ));
        }
        if input.mode != EngineMode::NativeTools.as_str()
            || input.approval_level.as_deref() != Some(cli_provider::ApprovalLevel::Full.as_str())
        {
            return Err(refuse(
                "a Full access bot runs its engine's own tools at the Full level",
            ));
        }
    }
    if input.mode == EngineMode::NativeTools.as_str()
        && input
            .workspace
            .as_deref()
            .is_none_or(|w| w.trim().is_empty())
    {
        // A bot that already has a folder keeps it when its engine or level changes, unless the
        // folder rule refuses it now (a folder saved by an older version, or one inside the data folder).
        let data_dir = s.storage().data_dir();
        let kept = profile
            .engine
            .workspace
            .clone()
            .filter(|w| vetted_folder(std::path::Path::new(w), data_dir).is_some());
        input.workspace = Some(match kept {
            Some(kept) => kept,
            None => default_workspace(&s, &bot).ok_or_else(|| {
                refuse("could not create a work folder for this bot; choose a folder in the bot's engine settings")
            })?,
        });
    }
    let data_dir = s.storage().data_dir().to_path_buf();
    let next = next_settings(
        &profile.engine,
        &profile.models.executor,
        &input,
        &data_dir,
        storage::now_ms(),
    )?;
    // Fail closed: the audit row is written before the setting takes effect.
    s.storage()
        .append_audit(
            s.user_id(),
            None,
            "user",
            "engine.updated",
            &json!({
                "bot_id": bot.id.to_string(),
                "from": summary(&profile.engine),
                "to": summary(&next),
                "consent_native_tools": input.consent_native_tools,
                "consent_full_access": input.consent_full_access,
                "full_access_bot": input.full_access_bot,
            }),
        )
        .map_err(ApiError::internal)?;
    let version = store(&s, &mut bot, &next)?;
    if next.mode == EngineMode::PraxiGoverned {
        s.agent().kill_native_runs(bot.id);
    }
    Ok(Json(dto(bot, version)))
}

/// Kill switch: back to Praxi-governed and end the bot's running CLI processes.
#[utoipa::path(post, path = "/v1/bots/{id}/engine/kill", tag = "bots", security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "Bot id")),
    responses((status = 200, body = BotDto), (status = 403, body = ErrorBody),
              (status = 404, body = ErrorBody)))]
pub(crate) async fn kill_engine(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    Path(bid): Path<String>,
) -> Result<Json<BotDto>, ApiError> {
    crate::cli_engines::local_owner(&actor)?;
    let mut bot = s.bot_of_user(BotId(crate::routes::id(&bid)?))?;
    let killed = s.agent().kill_native_runs(bot.id);
    s.storage()
        .append_audit(
            s.user_id(),
            None,
            "user",
            "engine.killed",
            &json!({"bot_id": bot.id.to_string(), "processes": killed}),
        )
        .map_err(ApiError::internal)?;
    let version = store(&s, &mut bot, &EngineSettings::default())?;
    Ok(Json(dto(bot, version)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(
        mode: &str,
        level: Option<&str>,
        ws: &std::path::Path,
        native: bool,
        full: bool,
    ) -> BotEngineInput {
        BotEngineInput {
            mode: mode.into(),
            approval_level: level.map(str::to_owned),
            workspace: Some(ws.display().to_string()),
            consent_native_tools: native,
            consent_full_access: full,
            full_access_bot: false,
        }
    }

    /// What the picker's consent dialog starts on, and sends: Ask when the engine offers it,
    /// else its first offered level (`OpenCode`, and Kimi on Linux, cannot be held at Ask).
    fn dialog_default(driver: Driver) -> ApprovalLevel {
        let offered: Vec<ApprovalLevel> = ApprovalLevel::ALL
            .into_iter()
            .filter(|l| level_plan(driver, *l).offered)
            .collect();
        if offered.contains(&ApprovalLevel::Ask) {
            ApprovalLevel::Ask
        } else {
            offered[0]
        }
    }

    #[test]
    fn every_engine_accepts_what_the_picker_dialog_sends_for_a_fresh_bot() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        let work = tmp.path().join("work");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        for driver in Driver::all() {
            let level = dialog_default(driver);
            let executor = format!("cli:{}/default", driver.id());
            let chosen = next_settings(
                &EngineSettings::default(),
                &executor,
                &input(
                    "native_tools",
                    Some(level.as_str()),
                    &work,
                    true,
                    level == ApprovalLevel::Full,
                ),
                &data,
                9,
            )
            .unwrap_or_else(|e| panic!("{}: {e:?}", driver.id()));
            assert_eq!(chosen.mode, EngineMode::NativeTools, "{}", driver.id());
            assert_eq!(chosen.approval_level, level, "{}", driver.id());
            assert_eq!(
                chosen.consent.native_tools_at_ms,
                Some(9),
                "{}",
                driver.id()
            );
            // Picking another engine later keeps the level without asking again.
            let again = next_settings(
                &chosen,
                &executor,
                &input("native_tools", Some(level.as_str()), &work, false, false),
                &data,
                10,
            )
            .unwrap_or_else(|e| panic!("{} again: {e:?}", driver.id()));
            assert_eq!(again.consent, chosen.consent, "{}", driver.id());
        }
    }

    #[test]
    fn switching_to_native_and_full_needs_consent_and_a_safe_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        let work = tmp.path().join("work");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        let cur = EngineSettings::default();
        let go = |i: &BotEngineInput, cur: &EngineSettings, exec: &str| {
            next_settings(cur, exec, i, &data, 7)
        };
        // Default level is Ask, and consent is mandatory to switch.
        assert!(
            go(
                &input("native_tools", None, &work, false, false),
                &cur,
                "cli:claude/sonnet"
            )
            .is_err()
        );
        let native = go(
            &input("native_tools", None, &work, true, false),
            &cur,
            "cli:claude/sonnet",
        )
        .unwrap();
        assert_eq!(native.approval_level, ApprovalLevel::Ask);
        assert_eq!(native.consent.native_tools_at_ms, Some(7));
        assert_eq!(native.consent.full_access_at_ms, None);
        // Full needs its own consent even for a bot already in native mode.
        assert!(
            go(
                &input("native_tools", Some("full"), &work, true, false),
                &native,
                "cli:claude"
            )
            .is_err()
        );
        let full = go(
            &input("native_tools", Some("full"), &work, false, true),
            &native,
            "cli:claude",
        )
        .unwrap();
        assert_eq!(full.consent.full_access_at_ms, Some(7));
        // The Praxi data dir, an HTTP model and an unoffered level are refused.
        assert!(
            go(
                &input("native_tools", None, &data, true, false),
                &cur,
                "cli:claude"
            )
            .is_err()
        );
        assert!(
            go(
                &input("native_tools", None, &work, true, false),
                &cur,
                "openai/gpt"
            )
            .is_err()
        );
        assert!(
            go(
                &input("native_tools", Some("accept_edits"), &work, true, false),
                &cur,
                "cli:cursor"
            )
            .is_err()
        );
        // OpenCode cannot be held below Full, so Ask is refused; Full needs its consent.
        assert!(
            go(
                &input("native_tools", None, &work, true, false),
                &cur,
                "cli:opencode"
            )
            .is_err()
        );
        assert!(
            go(
                &input("native_tools", Some("full"), &work, true, true),
                &cur,
                "cli:opencode"
            )
            .is_ok()
        );
        // Going back to Praxi-governed resets everything, no consent needed.
        assert_eq!(
            go(
                &input("praxi_governed", None, &work, false, false),
                &full,
                "cli:claude"
            )
            .unwrap(),
            EngineSettings::default()
        );
    }
}
