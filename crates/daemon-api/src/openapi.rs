//! `OpenAPI` 3 document generated from the route annotations (`utoipa`).
//!
//! The snapshot lives at `docs/api/openapi.json` in the repository root and is
//! checked by a test; regenerate with
//! `UPDATE_OPENAPI=1 cargo test -p daemon-api openapi_snapshot`.

use axum::Json;
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi};

use crate::{dto, email, email_routes, error, mcp, mcp_routes, routes, sse};

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
        routes::health,
        routes::list_bots,
        routes::create_bot,
        routes::get_bot,
        routes::list_grants,
        routes::revoke_grant,
        routes::update_bot,
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
        routes::resume_run,
        routes::remember_run,
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
        routes::reload_providers,
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
        mcp_routes::set_secret,
        email_routes::list_email_accounts,
        email_routes::create_email_account,
        email_routes::update_email_account,
        email_routes::delete_email_account,
        email_routes::test_email_account,
        email_routes::test_email_draft,
        events_doc,
    ),
    components(schemas(
        sse::SseStatus, sse::Gap, dto::Health, dto::BotDto, dto::BotInput, dto::ChatDto, dto::CreateChat,
        dto::MessageDto, dto::Mode, dto::SendMessage, dto::DeliveryKind, dto::SendResponse,
        dto::ApprovalDto, dto::Decision, dto::Scope, dto::DecideApproval,
        dto::DecisionResponse, dto::RunDto, dto::UsageTotals, dto::BotUsage,
        dto::UsageSummary, dto::AnswerQuestion, dto::AnswerResponse, dto::GrantDto,
        dto::GrantTargetDto, dto::RevokeGrant, dto::EffectivePolicyDto,
        dto::CategoryDecisionDto, dto::ControlMode, dto::ControlStateInput,
        dto::ControlStateDto, dto::LatestCursor, dto::ControlVerdict, dto::ControlDecisionInput,
        dto::ControlDecisionResponse, dto::PolicyDto, dto::PolicyInput, dto::ProvidersDto,
        dto::ConfiguredProviderDto, dto::CatalogProviderDto, dto::DetectedProviderDto, dto::CliAuthState, dto::EnableCliProviderInput, dto::ReloadResponse, dto::RememberResponse, dto::ScheduleDto, dto::ScheduleInput,
        dto::ParseSchedule, dto::ParsedSchedule, dto::AcceptedResponse, dto::ChannelDto, dto::PairCodeDto, dto::ScheduleChanged, dto::ScheduleChangeKind,
        mcp::McpTransportKind, mcp::McpStatus, mcp::McpServerDto, mcp::McpTransportObject,
        mcp::McpTransportInput, mcp::McpServerInput, mcp::McpToolDto, mcp::McpRequiredSecretDto,
        mcp::McpCatalogInstallDto, mcp::McpCatalogEntryDto, mcp::SetSecret, mcp::McpServerChanged,
        email::EmailSecurity, email::EmailImapServer, email::EmailSmtpServer,
        email::EmailAccountDto, email::EmailAccountInput, email::EmailAccountPatch,
        email::EmailProbeStatus, email::EmailTestReport,
        error::ErrorBody, error::ErrorDetail,
    )),
    modifiers(&Security),
    tags(
        (name = "meta"), (name = "bots"), (name = "chats"), (name = "approvals"),
        (name = "runs"), (name = "usage"), (name = "events"), (name = "control"),
        (name = "policy"), (name = "providers"), (name = "schedules"), (name = "channels"),
        (name = "mcp"), (name = "secrets"), (name = "email")
    )
)]
pub struct ApiDoc;

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
