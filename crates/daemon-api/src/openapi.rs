//! `OpenAPI` 3 document generated from the route annotations (`utoipa`).
//!
//! The snapshot lives at `docs/api/openapi.json` in the repository root and is
//! checked by a test; regenerate with
//! `UPDATE_OPENAPI=1 cargo test -p daemon-api openapi_snapshot`.

use axum::Json;
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi};

use crate::{
    changes, cli_login, components, composio, composio_routes, doctor, doctor_routes, dto, email,
    email_routes, error, file_access, fill_routes, host, host_admin, host_copy, host_import,
    host_links, host_setup, knowledge, knowledge_settings, live_view, mcp, mcp_routes, mcp_server,
    routes, skill_management, skills, sse, terminal, trusted_folders, web, web_routes,
};

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
        crate::cli_engines::list, crate::cli_engines::refresh_models_route, crate::cli_engines::preview, crate::cli_engines::decide,
        host::status, host_links::list, host_links::pair, host_links::connect, host_links::disconnect, host_links::forget, host_links::computer_requests, host_links::decide_computer_request, host_links::set_computer_access, host_setup::start, host_setup::get, host_setup::host_key, host_setup::cancel, host_setup::ssh_keys, host::invite, host::revoke, host::set_admin, host_admin::state, host_admin::put_provider, host_admin::delete_provider, host_admin::set_default, host_admin::cli_engines, host_admin::install_cli_engine, cli_login::start_login, cli_login::login_status, cli_login::login_code, cli_login::cancel_login, host_admin::cli_engine_install_status, host_copy::plan, host_copy::copy, trusted_folders::list, trusted_folders::add, trusted_folders::remove, host_copy::home, host_admin::set_home, host_import::import_schedules, host_import::import_chats, host_import::import_bot, host_import::check_bots, host_import::activate_schedules, host_import::release_schedules, host_import::reclaim, host::standby, host::activate, host::update,
        routes::health,
        live_view::targets,
        live_view::availability,
        live_view::status,
        terminal::open, terminal::output, terminal::input, terminal::resize, terminal::close_route,
        terminal::host_open, terminal::host_output, terminal::host_input, terminal::host_resize, terminal::host_close,
        live_view::thumbnail,
        live_view::create_session,
        live_view::update_session,
        live_view::heartbeat,
        live_view::delete_session,
        live_view::takeover,
        live_view::local_override,
        live_view::local_recover,
        live_view::set_privileges,
        live_view::input::input,
        live_view::stream::frames,
        knowledge::list_sources, knowledge::create_source, knowledge::update_source, knowledge::delete_source, knowledge::reindex_source, knowledge::embed_source,
        file_access::get_files, file_access::put_files,
        knowledge_settings::get_settings, knowledge_settings::put_settings, knowledge::email_captures,
        knowledge_settings::request_embedding_approval, knowledge_settings::confirm_embedding_approval,
        knowledge_settings::revoke_embedding_grant, knowledge_settings::reset_embedding_cooldown,
        routes::list_bots,
        routes::create_bot,
        routes::get_bot,
        routes::list_grants,
        routes::revoke_grant,
        skills::list_skills,
        skills::get_bot_skills,
        skills::put_bot_skills,
        skill_management::create,
        skill_management::import,
        skill_management::update,
        skill_management::delete,
        routes::update_bot,
        routes::patch_bot,
        routes::delete_bot,
        routes::list_chats,
        routes::create_chat,
        routes::list_messages,
        routes::send_message,
        routes::list_approvals,
        routes::decide_approval,
        routes::answer_question,
        routes::list_runs,
        routes::get_run,
        routes::cancel_run,
        crate::stop::stop_runs,
        routes::resume_run,
        routes::remember_run,
        changes::get_run_changes,
        changes::preview_restore,
        changes::restore_changes,
        routes::usage,
        routes::get_control_state,
        routes::put_control_state,
        routes::release_control,
        routes::latest_cursor,
        sse::status,
        routes::decide_control,
        routes::get_policy,
        routes::put_policy,
        routes::list_providers,
        routes::detect_providers,
        routes::enable_cli_provider,
        routes::disable_cli_provider,
        crate::bot_engine::put_engine,
        crate::bot_engine::kill_engine,
        routes::reload_providers,
        doctor_routes::get_doctor,
        doctor_routes::repair,
        doctor_routes::report,
        routes::list_schedules,
        routes::create_schedule,
        routes::get_schedule,
        routes::update_schedule,
        routes::delete_schedule,
        routes::run_schedule_now,
        routes::parse_schedule,
        routes::webhook,
        routes::list_channels,
        routes::telegram_pair_code,
        mcp_routes::list_mcp_servers,
        mcp_routes::create_mcp_server,
        mcp_routes::update_mcp_server,
        mcp_routes::delete_mcp_server,
        mcp_routes::reconnect_mcp_server,
        mcp_routes::list_mcp_tools,
        mcp_routes::mcp_catalog,
        mcp_routes::start_mcp_auth,
        mcp_routes::complete_mcp_auth,
        mcp_routes::cancel_mcp_auth,
        mcp_routes::mcp_auth_status,
        mcp_routes::logout_mcp_auth,
        mcp_routes::set_secret,
        mcp_server::create_token, mcp_server::revoke_token,
        mcp_server::list_tools, mcp_server::call_tool,
        components::list_components, components::get_component, components::install_component,
        composio_routes::composio_status,
        composio_routes::composio_login_start,
        composio_routes::composio_login_status,
        composio_routes::composio_set_key,
        composio_routes::composio_remove_key,
        composio_routes::composio_catalog,
        composio_routes::composio_list_apps,
        composio_routes::composio_get_app,
        composio_routes::composio_connect,
        composio_routes::composio_set_enabled,
        composio_routes::composio_disconnect,
        fill_routes::list_fill_secrets,
        fill_routes::create_fill_secret,
        fill_routes::update_fill_secret,
        fill_routes::delete_fill_secret,
        fill_routes::list_fill_grants,
        fill_routes::create_fill_grant,
        fill_routes::revoke_fill_grant,
        email_routes::list_email_accounts,
        email_routes::create_email_account,
        email_routes::update_email_account,
        email_routes::delete_email_account,
        email_routes::test_email_account,
        email_routes::test_email_draft,
        web_routes::get_web_search,
        web_routes::update_web_search,
        web_routes::test_web_search,
        events_doc,
    ),
    components(schemas(
        terminal::OpenTerminalInput, terminal::TerminalInfo, terminal::TerminalOutput, terminal::TerminalInput, terminal::TerminalResize, ::live_view::ScreenStatus, ::live_view::ScreenStreamState, ::live_view::RecordHeader, ::live_view::FrameMetadata, ::live_view::SourceRect,
        dto::ReleaseResult, dto::ControlStateChanged, dto::ScreenPrivilegesInput, sse::SseStatus, sse::Gap, dto::Health, dto::HostInfo, dto::BotDto, dto::BotInput, dto::BotPatchInput, dto::BotPatchOp, dto::BotPatchKind, dto::BotConflictBody, dto::RuntimeConfigDto, dto::ChatDto, dto::CreateChat,
        dto::MessageDto, dto::Mode, dto::SendMessage, dto::DeliveryKind, dto::SendResponse,
        dto::ApprovalDto, dto::Decision, dto::Scope, dto::DecideApproval,
        dto::DecisionResponse, dto::RunDto, dto::StopScope, dto::StopRunsInput, dto::StopOutcome, dto::StopRunResult, dto::StopRunsResponse, dto::UsageTotals, dto::BotUsage,
        dto::UsageSummary, dto::AnswerQuestion, dto::AnswerResponse, dto::GrantDto,
        dto::GrantTargetDto, dto::RevokeGrant, dto::EffectivePolicyDto,
        dto::CategoryDecisionDto, dto::ControlMode, dto::ControlStateInput,
        dto::ControlStateDto, dto::LatestCursor, dto::ControlVerdict, dto::ControlDecisionInput,
        dto::ControlDecisionResponse, dto::PolicyDto, dto::PolicyInput, dto::ProvidersDto,
        dto::ConfiguredProviderDto, dto::ModelMetadataDto, dto::CatalogProviderDto, dto::DetectedProviderDto, dto::CliAuthState, dto::EnableCliProviderInput, crate::bot_engine::BotEngineInput, crate::cli_engines::CliNativeDto, crate::cli_engines::CliLevelDto, dto::ReloadResponse, dto::RememberResponse, dto::ScheduleDto, dto::ScheduleInput,
        dto::ParseSchedule, dto::ParsedSchedule, dto::AcceptedResponse, dto::ChannelDto, dto::PairCodeDto, dto::ScheduleChanged, dto::ScheduleChangeKind,
        mcp::McpTransportKind, mcp::McpStatus, mcp::McpErrorKind, mcp::McpServerDto, mcp::McpTransportObject,
        mcp::McpTransportInput, mcp::McpServerInput, mcp::McpToolDto, mcp::McpRequiredSecretDto,
        mcp::McpOAuthConfig, mcp::McpAuthState, mcp::McpAuthStart, mcp::McpAuthCallback,
        mcp::McpCatalogInstallDto, mcp::McpFolderDto, mcp::McpCatalogEntryDto, mcp::SetSecret, mcp::McpServerChanged,
        components::ComponentId, components::ComponentInstallState, components::ComponentDto,
        composio::ComposioConnState, composio::ComposioLinkMode, composio::ComposioLoginDto,
        composio::ComposioLoginState, composio::ComposioLoginStatusDto, composio::ComposioStatusDto, composio::ComposioKeyInput,
        composio::ComposioToolkitDto, composio::ComposioCatalogDto, composio::ComposioAppDto,
        composio::ComposioConnectDto, composio::ComposioEnabledInput, composio::ComposioDisconnectDto,
        mcp_server::McpScope, mcp_server::McpTokenGrant, mcp_server::CreateMcpToken,
        mcp_server::McpCallInput, mcp_server::ExportedMcpTool,
        email::EmailProviderKind, email::EmailSecurity, email::EmailImapServer, email::EmailSmtpServer,
        email::EmailAccountDto, email::EmailAccountInput, email::EmailAccountPatch,
        email::EmailProbeStatus, email::EmailTestReport,
        web::WebSearchConfigDto, web::WebSearchConfigInput, web::WebSearchTestInput,
        web::WebSearchTestReport,
        host::HostMode, host::HostState, host::HostLimits, host::HostConfig, host::HostConflict,
        host_links::HostLinkState, host_links::HostLinkInfo, host_links::HostLinkList, host_links::PairHostInput, host_links::HostLinkConnection,
        host_setup::StartHostSetupInput, host_setup::HostSetupState, host_setup::HostSetupStepStatus, host_setup::HostSetupStep, host_setup::HostKeyPrompt, host_setup::HostSetupJob, host_setup::HostKeyAnswer, host_setup::SshKeyHint, host_setup::SshKeyList,
        host::HostAdminInput, host_admin::AdminStateDto, host_admin::AdminCliEngineDto, host_admin::AdminCliInstallDto, cli_login::AdminCliLoginDto, cli_login::AdminLoginCodeInput, host_admin::AdminProviderInput, host_admin::AdminProviderDto, host_admin::AdminDefaultInput, trusted_folders::TrustedFoldersDto, trusted_folders::TrustFolderInput, host_copy::CopyPlanDto, host_copy::CopyProviderRow, host_copy::CopyBotRow, host_copy::CopyRequest, host_copy::CopyItemResult, host_copy::CopyReport, host_copy::HomeDto, host_admin::AdminHomeInput, host_import::ImportScheduleItem, host_import::ImportSchedulesInput, host_import::ImportScheduleResult, host_import::ImportSchedulesReport, host_import::ImportChat, host_import::ImportRun, host_import::ImportMessage, host_import::ImportChatsInput, host_import::ImportChatsReport, host_import::ImportBotInput, host_import::CheckBotItem, host_import::CheckBotsInput, host_import::CheckBotResult, host_import::CheckBotsReport, host_import::ImportBotResult, host_import::ScheduleIdsInput, host_import::ReclaimInput, host_import::ReclaimDto,
        host::HostStatus, host::HostPeer, host::InviteRole, host::HostInviteInput, host::HostInvite, host::ActivateInput,
        changes::CheckpointComputerDto, changes::CheckpointLocationDto, changes::VerificationDto, changes::ChangeKindDto, changes::DiffOmittedReasonDto, changes::UnifiedDiffDto, changes::FileChangeDto,
        changes::RunChangesDto, changes::RestoreActionDto, changes::RestorePreviewEntryDto,
        changes::RestorePreviewDto, changes::RestoreRequestDto, changes::RestoreResponseDto,
        skills::SkillScope, skills::SkillIssueScope, skills::SkillRequiresDto, skills::SkillHubInfoDto,
        skills::CatalogSkillDto, skills::SkillIssueDto, skills::SkillsCatalogDto,
        skills::SkillLocationsDto,
        skill_management::SkillDocumentInput, skill_management::SkillImportInput,
        skill_management::ManagedSkillDto, skill_management::DeleteSkillQuery,
        skill_management::SkillUsingBotDto, skill_management::DeleteSkillResult,
        skills::SkillAssignmentModeDto, skills::AssignedSkillState, skills::AssignedSkillDto,
        skills::AvailableSkillDto, skills::OwnSkillDto, skills::BotSkillsDto, skills::PutBotSkillsInput,
        doctor::Status, doctor::Repair, doctor::Check, doctor_routes::DoctorReport,
        doctor_routes::RepairResult,
        fill_routes::FillSecretDto, fill_routes::CreateFillSecret, fill_routes::UpdateFillSecret,
        fill_routes::FillGrantDto, fill_routes::CreateFillGrant,
        error::ErrorBody, error::ErrorDetail,
    )),
    modifiers(&Security),
    tags(
        (name = "view"), (name = "meta"), (name = "bots"), (name = "chats"), (name = "approvals"),
        (name = "runs"), (name = "usage"), (name = "events"), (name = "control"),
        (name = "policy"), (name = "providers"), (name = "schedules"), (name = "channels"),
        (name = "host"), (name = "admin"), (name = "mcp"), (name = "secrets"), (name = "email"), (name = "composio"), (name = "web"),
        (name = "skills"), (name = "doctor"), (name = "mcp-server")
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
