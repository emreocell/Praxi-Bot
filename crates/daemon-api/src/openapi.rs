//! `OpenAPI` 3 document generated from the route annotations (`utoipa`).
//!
//! The snapshot lives at `docs/api/openapi.json` in the repository root and is
//! checked by a test; regenerate with
//! `UPDATE_OPENAPI=1 cargo test -p daemon-api openapi_snapshot`.

use axum::Json;
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi};

use crate::bot_limits_routes;
use crate::changes;
use crate::cli_login;
use crate::components;
use crate::composio;
use crate::composio_routes;
use crate::doctor;
use crate::doctor_routes;
use crate::dto;
use crate::email;
use crate::email_routes;
use crate::error;
use crate::file_access;
use crate::fill_routes;
use crate::host;
use crate::host_admin;
use crate::host_copy;
use crate::host_import;
use crate::host_links;
use crate::host_setup;
use crate::knowledge;
use crate::knowledge_settings;
use crate::live_view;
use crate::mcp;
use crate::mcp_routes;
use crate::mcp_server;
use crate::memory;
use crate::peer;
use crate::profile_share;
use crate::profile_share_routes;
use crate::routes;
use crate::secret_request_routes;
use crate::skill_hub;
use crate::skill_management;
use crate::skills;
use crate::spend_routes;
use crate::sse;
use crate::terminal;
use crate::trusted_folders;
use crate::usage;
use crate::voice;
use crate::voice_routes;
use crate::web;
use crate::web_routes;

struct Security;

impl Modify for Security {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        if let Some(c) = openapi.components.as_mut() {
            c.add_security_scheme(
                "bearer",
                SecurityScheme::Http(
                    HttpBuilder::new()
                        .scheme(HttpAuthScheme::Bearer)
                        .description(Some("Per-install token stored in `<data dir>/api-token`."))
                        .build(),
                ),
            );
        }
    }
}

/// The API description.  `GET /v1/events` (SSE) is documented in the module
/// docs of the crate source (`sse.rs`); its payload is `protocol::Envelope`.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "Praxi-Bot daemon API",
        version = "1.0.0",
        description = "Local HTTP API of the bot daemon. Bind: 127.0.0.1 only. Host must be \
            loopback; a browser Origin must be an allowed desktop-app origin; all routes \
            except GET /v1/health need `Authorization: Bearer <token>`."
    ),
    paths(
        crate::chat_history::search,
        crate::chat_history::export,
        changes::get_run_changes,
        changes::preview_restore,
        changes::restore_changes,
        cli_login::cancel_login,
        cli_login::login_code,
        cli_login::login_status,
        cli_login::start_login,
        components::get_component,
        components::install_component,
        components::list_components,
        composio_routes::composio_catalog,
        composio_routes::composio_connect,
        composio_routes::composio_disconnect,
        composio_routes::composio_get_app,
        composio_routes::composio_list_apps,
        composio_routes::composio_login_start,
        composio_routes::composio_login_status,
        composio_routes::composio_remove_key,
        composio_routes::composio_set_enabled,
        composio_routes::composio_set_key,
        composio_routes::composio_status,
        crate::attachment_routes::download,
        crate::attachment_routes::upload,
        crate::bot_engine::kill_engine,
        crate::bot_engine::put_engine,
        crate::cli_engines::decide,
        crate::cli_engines::list,
        crate::cli_engines::preview,
        crate::cli_engines::refresh_models_route,
        crate::stop::stop_runs,
        crate::workflows::delete,
        crate::workflows::list,
        crate::workflows::propose,
        crate::workflows::run,
        crate::workflows::save,
        crate::workflows::show,
        doctor_routes::get_doctor,
        doctor_routes::repair,
        doctor_routes::report,
        email_routes::create_email_account,
        email_routes::delete_email_account,
        email_routes::list_email_accounts,
        email_routes::list_email_messages,
        email_routes::send_email_test,
        email_routes::test_email_account,
        email_routes::test_email_draft,
        email_routes::update_email_account,
        events_doc,
        file_access::get_files,
        file_access::put_files,
        fill_routes::create_fill_grant,
        fill_routes::create_fill_secret,
        fill_routes::delete_fill_secret,
        fill_routes::list_fill_grants,
        fill_routes::list_fill_secrets,
        fill_routes::revoke_fill_grant,
        fill_routes::update_fill_secret,
        secret_request_routes::cancel_secret_request,
        secret_request_routes::submit_secret_request,
        host::activate,
        host::invite,
        host::revoke,
        host::set_admin,
        host::standby,
        host::status,
        host::update,
        host_admin::cli_engine_install_status,
        host_admin::cli_engines,
        host_admin::delete_provider,
        host_admin::install_cli_engine,
        host_admin::put_provider,
        host_admin::set_default,
        host_admin::set_home,
        host_admin::state,
        host_copy::copy,
        host_copy::home,
        host_copy::plan,
        host_import::activate_schedules,
        host_import::check_bots,
        host_import::import_bot,
        host_import::import_chats,
        host_import::import_schedules,
        host_import::reclaim,
        host_import::release_schedules,
        host_links::computer_requests,
        host_links::connect,
        host_links::decide_computer_request,
        host_links::disconnect,
        host_links::forget,
        host_links::list,
        host_links::pair,
        host_links::set_computer_access,
        host_setup::cancel,
        host_setup::get,
        host_setup::host_key,
        host_setup::ssh_keys,
        host_setup::start,
        knowledge::create_source,
        knowledge::delete_source,
        knowledge::email_captures,
        knowledge::embed_source,
        knowledge::list_sources,
        knowledge::reindex_source,
        knowledge::update_source,
        knowledge_settings::confirm_embedding_approval,
        knowledge_settings::get_settings,
        knowledge_settings::put_settings,
        knowledge_settings::request_embedding_approval,
        knowledge_settings::reset_embedding_cooldown,
        knowledge_settings::revoke_embedding_grant,
        live_view::ack_frame,
        live_view::availability,
        live_view::create_session,
        live_view::delete_session,
        live_view::heartbeat,
        live_view::input::input,
        live_view::local_override,
        live_view::local_recover,
        live_view::set_privileges,
        live_view::status,
        live_view::stream::frames,
        live_view::takeover,
        live_view::targets,
        live_view::thumbnail,
        live_view::update_session,
        mcp_routes::cancel_mcp_auth,
        mcp_routes::complete_mcp_auth,
        mcp_routes::create_mcp_server,
        mcp_routes::delete_mcp_server,
        mcp_routes::list_mcp_servers,
        mcp_routes::list_mcp_tools,
        mcp_routes::logout_mcp_auth,
        mcp_routes::mcp_auth_status,
        mcp_routes::mcp_catalog,
        mcp_routes::reconnect_mcp_server,
        mcp_routes::set_secret,
        mcp_routes::start_mcp_auth,
        mcp_routes::update_mcp_server,
        mcp_server::call_tool,
        mcp_server::create_token,
        mcp_server::list_tools,
        mcp_server::revoke_token,
        memory::create_memory,
        memory::delete_memory,
        memory::list_memories,
        memory::preview_bot_memory,
        memory::undo_memory,
        memory::update_memory,
        peer::link,
        routes::answer_question,
        routes::cancel_run,
        routes::create_bot,
        routes::create_chat,
        routes::create_schedule,
        routes::decide_approval,
        routes::decide_control,
        routes::delete_bot,
        routes::delete_chat,
        routes::delete_schedule,
        routes::detect_providers,
        crate::local_models::get_local_models,
        crate::local_install::get_local_install,
        crate::local_install::start_local_install,
        crate::local_install::stop_local_install,
        routes::disable_cli_provider,
        routes::enable_cli_provider,
        routes::get_bot,
        routes::get_control_state,
        routes::get_policy,
        routes::get_run,
        routes::get_schedule,
        routes::health,
        routes::latest_cursor,
        routes::list_approvals,
        routes::list_bots,
        profile_share_routes::export_profile,
        profile_share_routes::import_profile,
        profile_share_routes::preview_profile,
        profile_share_routes::profile_templates,
        routes::list_channels,
        routes::list_chats,
        routes::list_grants,
        routes::list_messages,
        routes::list_providers,
        routes::list_runs,
        routes::list_schedule_history,
        routes::list_schedules,
        routes::parse_schedule,
        routes::patch_bot,
        routes::preview_schedule,
        routes::put_control_state,
        routes::put_policy,
        routes::release_control,
        routes::reload_providers,
        routes::remember_run,
        routes::resume_run,
        routes::retry_schedule_occurrence,
        routes::revoke_grant,
        routes::run_schedule_now,
        routes::run_usage,
        routes::send_message,
        routes::resend_last_turn,
        routes::telegram_pair_code,
        routes::update_bot,
        routes::update_chat,
        routes::update_schedule,
        routes::webhook,
        skill_hub::hub_install,
        skill_hub::hub_preview,
        skill_management::create,
        skill_management::delete,
        skill_management::import,
        skill_management::update,
        skills::get_bot_skills,
        skills::list_skills,
        skills::put_bot_skills,
        sse::status,
        terminal::close_route,
        terminal::host_close,
        terminal::host_input,
        terminal::host_open,
        terminal::host_output,
        terminal::host_resize,
        terminal::input,
        terminal::open,
        terminal::output,
        terminal::resize,
        trusted_folders::add,
        trusted_folders::list,
        trusted_folders::remove,
        usage::export_csv,
        usage::usage,
        voice_routes::get_voice,
        voice_routes::transcribe,
        voice_routes::update_voice,
        bot_limits_routes::get_bot_spend_limits,
        bot_limits_routes::put_bot_spend_limits,
        spend_routes::get_spend_limit,
        spend_routes::put_spend_limit,
        web_routes::get_web_search,
        web_routes::test_web_search,
        web_routes::update_web_search,
    ),
    components(schemas(
        ::live_view::FrameMetadata,
        ::live_view::RecordHeader,
        ::live_view::ScreenStatus,
        ::live_view::ScreenStreamState,
        ::live_view::SourceRect,
        changes::ChangeKindDto,
        changes::CheckpointComputerDto,
        changes::CheckpointLocationDto,
        changes::DiffOmittedReasonDto,
        changes::FileChangeDto,
        changes::RestoreActionDto,
        changes::RestorePreviewDto,
        changes::RestorePreviewEntryDto,
        changes::RestoreRequestDto,
        changes::RestoreResponseDto,
        changes::RunChangesDto,
        changes::UnifiedDiffDto,
        changes::VerificationDto,
        cli_login::AdminCliLoginDto,
        cli_login::AdminLoginCodeInput,
        components::ComponentDto,
        components::ComponentId,
        components::ComponentInstallState,
        composio::ComposioAppDto,
        composio::ComposioCatalogDto,
        composio::ComposioConnState,
        composio::ComposioConnectDto,
        composio::ComposioDisconnectDto,
        composio::ComposioEnabledInput,
        composio::ComposioKeyInput,
        composio::ComposioLinkMode,
        composio::ComposioLoginDto,
        composio::ComposioLoginState,
        composio::ComposioLoginStatusDto,
        composio::ComposioStatusDto,
        composio::ComposioToolkitDto,
        crate::bot_engine::BotEngineInput,
        crate::cli_engines::CliLevelDto,
        crate::cli_engines::CliNativeDto,
        doctor::Check,
        doctor::Repair,
        doctor::Status,
        doctor_routes::DoctorReport,
        doctor_routes::RepairResult,
        dto::AcceptedResponse,
        dto::AnswerQuestion,
        dto::AnswerResponse,
        dto::ApprovalDto,
        dto::AttachmentDto,
        dto::BotConflictBody,
        dto::BotDto,
        dto::BotInput,
        dto::BotPatchInput,
        dto::BotPatchKind,
        dto::BotPatchOp,
        dto::BotUsage,
        dto::CatalogProviderDto,
        dto::CategoryDecisionDto,
        dto::ChannelDto,
        dto::ChatDto,
        dto::CliAuthState,
        dto::ConfiguredProviderDto,
        dto::ControlDecisionInput,
        dto::ControlDecisionResponse,
        dto::ControlMode,
        dto::ControlStateChanged,
        dto::ControlStateDto,
        dto::ControlStateInput,
        dto::ControlVerdict,
        dto::CreateChat,
        dto::DailyUsage,
        dto::DecideApproval,
        dto::Decision,
        dto::DecisionResponse,
        dto::DeliveryKind,
        dto::DetectedProviderDto,
        crate::local_models::LocalModelsDto,
        crate::local_install::LocalInstallDto,
        crate::local_install::ManagedModelDto,
        crate::local_install::ManagedFitDto,
        crate::local_install::InstallStateDto,
        crate::local_install::InstallPhaseDto,
        crate::local_install::InstallErrorKindDto,
        crate::local_install::StartInstallRequest,
        crate::local_models::HardwareDto,
        crate::local_models::LocalAdviceDto,
        crate::local_models::LocalModelDto,
        crate::local_models::LocalRuntimeDto,
        crate::local_models::FitDto,
        crate::local_models::MemoryBasisDto,
        dto::EffectivePolicyDto,
        dto::EnableCliProviderInput,
        dto::GrantDto,
        dto::GrantTargetDto,
        dto::Health,
        dto::HostInfo,
        dto::LatestCursor,
        dto::MessageDto,
        dto::Mode,
        dto::ModelMetadataDto,
        dto::ModelUsage,
        dto::PairCodeDto,
        dto::ParseSchedule,
        dto::ParsedSchedule,
        dto::PolicyDto,
        dto::PolicyInput,
        dto::PreviewSchedule,
        dto::ProviderUsage,
        dto::ProvidersDto,
        dto::ReleaseResult,
        dto::ReloadResponse,
        dto::RememberResponse,
        dto::RevokeGrant,
        dto::RunDto,
        dto::RunUsageDto,
        dto::RuntimeConfigDto,
        dto::ScheduleChangeKind,
        dto::ScheduleChanged,
        dto::ScheduleDto,
        dto::ScheduleHistoryEntryDto,
        dto::ScheduleInput,
        dto::Scope,
        dto::ScreenPrivilegesInput,
        dto::SendMessage,
        dto::SendResponse,
        dto::ResendMessage,
        dto::ResendResponse,
        dto::StopOutcome,
        dto::StopRunResult,
        dto::StopRunsInput,
        dto::StopRunsResponse,
        dto::StopScope,
        dto::UpdateChat,
        dto::UsageSummary,
        dto::UsageTotals,
        email::EmailAccountDto,
        email::EmailAccountInput,
        email::EmailAccountPatch,
        email::EmailImapServer,
        email::EmailMessageDto,
        email::EmailProbeStatus,
        email::EmailProviderKind,
        email::EmailSecurity,
        email::EmailSendTestResponse,
        email::EmailSmtpServer,
        email::EmailTestReport,
        error::ErrorBody,
        error::ErrorDetail,
        fill_routes::CreateFillGrant,
        fill_routes::CreateFillSecret,
        fill_routes::FillGrantDto,
        fill_routes::FillSecretDto,
        fill_routes::UpdateFillSecret,
        secret_request_routes::SecretRequestAnswer,
        secret_request_routes::SubmitSecretRequest,
        host::ActivateInput,
        host::HostAdminInput,
        host::HostConfig,
        host::HostConflict,
        host::HostInvite,
        host::HostInviteInput,
        host::HostLimits,
        host::HostMode,
        host::HostPeer,
        host::HostState,
        host::HostStatus,
        host::InviteRole,
        host_admin::AdminCliEngineDto,
        host_admin::AdminCliInstallDto,
        host_admin::AdminDefaultInput,
        host_admin::AdminHomeInput,
        host_admin::AdminProviderDto,
        host_admin::AdminProviderInput,
        host_admin::AdminStateDto,
        host_copy::CopyBotRow,
        host_copy::CopyItemResult,
        host_copy::CopyPlanDto,
        host_copy::CopyProviderRow,
        host_copy::CopyReport,
        host_copy::CopyRequest,
        host_copy::HomeDto,
        host_import::CheckBotItem,
        host_import::CheckBotResult,
        host_import::CheckBotsInput,
        host_import::CheckBotsReport,
        host_import::ImportBotInput,
        profile_share::CategoryPreview,
        profile_share::ImportPreview,
        profile_share::PreviewNote,
        profile_share::ScheduleIdea,
        profile_share::ShareLevel,
        profile_share::SharePreset,
        profile_share::SharedBot,
        profile_share::SharedModels,
        profile_share::SharedPermissions,
        profile_share::SharedProfile,
        profile_share::SharedRule,
        profile_share::SharedSkill,
        profile_share::SharedTools,
        profile_share::SkillPreview,
        profile_share::TemplateMeta,
        profile_share_routes::ExportInput,
        profile_share_routes::ExportOutput,
        profile_share_routes::ImportInput,
        profile_share_routes::ImportOutput,
        host_import::ImportBotResult,
        host_import::ImportChat,
        host_import::ImportChatsInput,
        host_import::ImportChatsReport,
        host_import::ImportMessage,
        host_import::ImportRun,
        host_import::ImportScheduleItem,
        host_import::ImportScheduleResult,
        host_import::ImportSchedulesInput,
        host_import::ImportSchedulesReport,
        host_import::ReclaimDto,
        host_import::ReclaimInput,
        host_import::ScheduleIdsInput,
        host_links::HostLinkConnection,
        host_links::HostLinkInfo,
        host_links::HostLinkList,
        host_links::HostLinkState,
        host_links::PairHostInput,
        host_setup::HostKeyAnswer,
        host_setup::HostKeyPrompt,
        host_setup::HostSetupJob,
        host_setup::HostSetupState,
        host_setup::HostSetupStep,
        host_setup::HostSetupStepStatus,
        host_setup::SshKeyHint,
        host_setup::SshKeyList,
        host_setup::StartHostSetupInput,
        mcp::McpAuthCallback,
        mcp::McpAuthStart,
        mcp::McpAuthState,
        mcp::McpCatalogEntryDto,
        mcp::McpCatalogInstallDto,
        mcp::McpErrorKind,
        mcp::McpFolderDto,
        mcp::McpOAuthConfig,
        mcp::McpRequiredSecretDto,
        mcp::McpServerChanged,
        mcp::McpServerDto,
        mcp::McpServerInput,
        mcp::McpStatus,
        mcp::McpToolDto,
        mcp::McpTransportInput,
        mcp::McpTransportKind,
        mcp::McpTransportObject,
        mcp::SetSecret,
        mcp_server::CreateMcpToken,
        mcp_server::ExportedMcpTool,
        mcp_server::McpCallInput,
        mcp_server::McpScope,
        mcp_server::McpTokenGrant,
        memory::CreateMemoryInput,
        memory::MemoryItemDto,
        memory::MemoryKindDto,
        memory::MemoryPreviewDto,
        memory::MemoryPreviewHitDto,
        memory::MemoryTrustDto,
        memory::UpdateMemoryInput,
        peer::PeerLink,
        skill_hub::HubFindingDto,
        skill_hub::HubInstallInput,
        skill_hub::HubPreviewDto,
        skill_hub::HubSourceInput,
        skill_management::DeleteSkillQuery,
        skill_management::DeleteSkillResult,
        skill_management::ManagedSkillDto,
        skill_management::SkillDocumentInput,
        skill_management::SkillImportInput,
        skill_management::SkillUsingBotDto,
        skills::AssignedSkillDto,
        skills::AssignedSkillState,
        skills::AvailableSkillDto,
        skills::BotSkillsDto,
        skills::CatalogSkillDto,
        skills::OwnSkillDto,
        skills::PutBotSkillsInput,
        skills::SkillAssignmentModeDto,
        skills::SkillHubInfoDto,
        skills::SkillIssueDto,
        skills::SkillIssueScope,
        skills::SkillLocationsDto,
        skills::SkillRequiresDto,
        skills::SkillScope,
        skills::SkillsCatalogDto,
        sse::Gap,
        sse::SseStatus,
        terminal::OpenTerminalInput,
        terminal::TerminalInfo,
        terminal::TerminalInput,
        terminal::TerminalOutput,
        terminal::TerminalResize,
        trusted_folders::TrustFolderInput,
        trusted_folders::TrustedFoldersDto,
        voice::TranscribeInput,
        voice::TranscribeOutput,
        voice::VoiceConfigDto,
        voice::VoiceConfigInput,
        bot_limits_routes::BotLimitUsageDto,
        bot_limits_routes::BotLimitsDto,
        bot_limits_routes::BotSpendLimitsDto,
        bot_limits_routes::PeriodLimitDto,
        bot_limits_routes::PeriodUsageDto,
        bot_limits_routes::PutBotSpendLimitsInput,
        spend_routes::SpendLimitDto,
        spend_routes::SpendLimitInput,
        web::WebSearchConfigDto,
        web::WebSearchConfigInput,
        web::WebSearchTestInput,
        web::WebSearchTestReport,
    )),
    modifiers(&Security),
    tags(
        (name = "admin"),
        (name = "approvals"),
        (name = "bots"),
        (name = "channels"),
        (name = "chats"),
        (name = "composio"),
        (name = "control"),
        (name = "doctor"),
        (name = "email"),
        (name = "events"),
        (name = "host"),
        (name = "mcp"),
        (name = "mcp-server"),
        (name = "memory"),
        (name = "meta"),
        (name = "policy"),
        (name = "providers"),
        (name = "runs"),
        (name = "schedules"),
        (name = "secrets"),
        (name = "skills"),
        (name = "usage"),
        (name = "view"),
        (name = "voice"),
        (name = "web"),
    )
)]
pub struct ApiDoc;

/// Host notifications: `host_state_changed` carries `HostConfig`, `host_conflict` carries
/// `HostConflict` (reason, peer, epoch, notification). These live refetch hints have no cursor.
/// Documentation stub for the SSE route (the handler lives in `sse.rs`).
///
/// Server-sent events; each `data:` is a `protocol::Envelope` JSON object, the
/// SSE `event:` is its `type`.  With `run_id` the SSE `id` is the run-local
/// `seq` and `Last-Event-ID: <seq>` resumes from the run's NDJSON log.  Without
/// it the stream covers all runs: the SSE `id` is a daemon-wide, strictly
/// increasing `gseq` (persisted) and `Last-Event-ID: <gseq>` replays every
/// missed event of every run, in order, before going live.  Browsers cannot set
/// headers on `EventSource`, so this route (and only this one) also accepts the
/// API token as the `token` query parameter (Host / Origin checks still apply;
/// the token is compared in constant time; beware that URLs may be logged).
///
/// The daemon-wide stream (no `run_id`) also carries `schedule_changed` events
/// (`dto::ScheduleChanged`: `{"type":"schedule_changed","schedule_id","kind"}`,
/// kind `created` / `updated` / `toggled` / `deleted`) when a schedule is changed
/// through the API.  They have no SSE `id`, are not replayed and are not
/// envelopes: treat them as a hint to refetch `/v1/schedules`.
///
/// A replay gap is explicit: `event: gap` has an `id` equal to `to_gseq` and
/// data `{from_gseq, to_gseq, run_id, reason}`; reason is `log_line_missing` or
/// `log_unreadable`. It is audited before the cursor passes the missing row.
/// `/v1/events/status.sse_gaps_total` (authenticated) counts these notifications since daemon startup.
/// After three consecutive replay failures (or live-buffer overflow),
/// `event: error` carries `{code: "replay_unavailable", last_event_id}` and the
/// stream closes. This error has no `id`; reconnect with the last received ID.
/// Retry delays start at 250 ms and double up to 5 s. Live buffering is limited
/// to 1000 events and 4 MiB per subscriber; replay chunks hold at most 2 MiB of serialized events (a single larger event closes the stream with `replay_unavailable`), and at most `max_sse_subscribers` streams (default 16) may be open: further requests get `503`. The desktop currently ignores unknown
/// event names; card 144 can surface gap/error notifications without changing
/// the ordering or cursor contract.
///
/// It likewise carries `mcp_server_changed` events (`mcp::McpServerChanged`:
/// `{"type":"mcp_server_changed","name","status"}`) when an MCP server is
/// added, updated, removed or changes status (connecting / connected / error /
/// quarantined ...); same rules: a hint to refetch `/v1/mcp/servers`.
///
/// And `control_state_changed` events (`dto::ControlStateChanged`:
/// `{"type":"control_state_changed","state","owner","target_id","reason"}`) when
/// a screen takeover is acquired, becomes active, is interrupted, released or
/// recovered (no ids, epochs, pixels or text); not replayed.
///
/// And `view_targets_changed` events (`{"type":"view_targets_changed"}`) when a screen
/// target appears, goes idle, attaches to a page or closes, or a missing-screen
/// reason changes: a pixel-free hint to refetch `/v1/view/targets` and
/// `/v1/view/availability`; not replayed.
#[utoipa::path(get, path = "/v1/events", tag = "events", security(("bearer" = [])),
    params(
        ("run_id" = Option<uuid::Uuid>, Query, description = "Follow only this run; the stream ends after run_finished"),
        ("token" = Option<String>, Query, description = "API token, for clients that cannot send Authorization (EventSource)"),
        ("Last-Event-ID" = Option<String>, Header, description = "Resume cursor: `<seq>` with run_id, else the daemon-wide `<gseq>`")
    ),
    responses((status = 200, description = "text/event-stream of protocol::Envelope",
        content_type = "text/event-stream", body = String),
        (status = 404, body = error::ErrorBody)))]
#[allow(dead_code)]
fn events_doc() {}

/// The `OpenAPI` document as pretty JSON (with a trailing newline).
///
/// # Panics
/// Never in practice (the document always serialises).
#[must_use]
pub fn openapi_json() -> String {
    let mut s = ApiDoc::openapi()
        .to_pretty_json()
        .expect("openapi serialises");
    s.push('\n');
    s
}

pub(crate) async fn serve_openapi() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}
