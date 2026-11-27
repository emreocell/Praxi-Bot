//! Authenticated ingress for paired devices. The transport supplies identity;
//! HTTP headers can never grant a role.
use crate::{ApiError, AppState};
use axum::{
    Extension, Json, Router,
    extract::{Request, State},
    http::Method,
    middleware::Next,
    response::{IntoResponse, Response},
};

/// Role granted by the pairing store, never by the remote HTTP client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerRole {
    Desktop,
    Phone,
    Node,
    Host,
}

/// Authenticated transport identity.
#[derive(Clone, Debug)]
pub struct PeerIdentity {
    pub key: String,
    pub role: PeerRole,
}

/// The selected path of the authenticated QUIC connection, without peer addresses.
#[derive(Clone, Copy, Debug, serde::Serialize, utoipa::ToSchema)]
pub struct PeerLink {
    pub relay: bool,
    pub rtt_ms: u32,
}

/// Return the path for this request's tunnel, or null on local HTTP.
#[utoipa::path(get, path = "/v1/peer/link", tag = "peer", security(("bearer" = [])), responses((status = 200, body = Option<PeerLink>)))]
pub(crate) async fn link(path: Option<Extension<PeerLink>>) -> Json<Option<PeerLink>> {
    Json(path.map(|Extension(info)| info))
}

/// Generic JSON/TOML profile editors retain ordinary bot editing, but cannot
/// create, replace or clear host-local knowledge settings. Omission preserves
/// the current subtree rather than implicitly disabling an owner configuration.
pub(crate) fn protect_knowledge_config(
    peer: Option<&PeerIdentity>,
    current: Option<&serde_json::Value>,
    next: &mut serde_json::Value,
) -> Result<(), ApiError> {
    if peer.is_none() {
        return Ok(());
    }
    if let Some(incoming) = next.get("knowledge") {
        if Some(incoming) != current {
            return Err(ApiError::forbidden(
                "knowledge settings are managed by the local owner",
            ));
        }
    } else if let Some(current) = current {
        next.as_object_mut()
            .ok_or_else(|| ApiError::bad_request("bot config must be an object"))?
            .insert("knowledge".into(), current.clone());
    }
    Ok(())
}

/// Future host transfer endpoints forbidden to phones.
pub const PHONE_DENIED_POST_PATHS: &[&str] = &["/v1/host/import", "/v1/host/handover"];

/// `POST /v1/runs/stop`: only phones and desktops may stop runs (and then only
/// with scope `listed`: the handler refuses `all_existing` to every peer). The
/// request is attributed in the audit log like an approval. Pinned here so the
/// route stays closed to node and host peers even if the blanket role rule
/// below is ever loosened.
pub const PEER_RUN_STOP_PATH: &str = "/v1/runs/stop";

/// Host-local provider management endpoints forbidden to ALL paired peers.
pub const PEER_DENIED_POST_PATHS: &[&str] = &[
    "/v1/providers/cli/enable",
    "/v1/providers/cli/disable",
    "/v1/providers/reload",
];

/// Host-local diagnostics forbidden to ALL paired peers, for every method.
/// Repairs (`/v1/doctor/repair/*`) are actions of the user at this host's
/// desktop UI; the check list and the support report describe the host (paths,
/// platform, provider set), so peers do not read them either.
pub const PEER_DENIED_PREFIX_DOCTOR: &str = "/v1/doctor";

/// `/v1/hosts…` manages THIS computer's links to paired hosts (pair, connect,
/// disconnect). It is an action of the local user at this computer's UI and is
/// closed to every paired peer role, for every method: a peer must never be
/// able to make this daemon open a channel to another host or read the
/// loopback proxy token it hands out.
pub const PEER_DENIED_PREFIX_HOSTS: &str = "/v1/hosts";

fn is_hosts_path(path: &str) -> bool {
    path.strip_prefix(PEER_DENIED_PREFIX_HOSTS)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

fn is_mutating(method: &Method) -> bool {
    *method != Method::GET && *method != Method::HEAD && *method != Method::OPTIONS
}

/// `path` is `prefix` or below it (decoded and slash-normalized).
fn under(path: &str, prefix: &str) -> bool {
    path.strip_prefix(prefix)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// Profile parts that decide what a bot may do without asking: the permission
/// preset, per-category levels and unattended rules (`permissions`) and the
/// tool allow/exclude lists (`tools`).
const EXECUTION_KEYS: [&str; 3] = ["permissions", "tools", "delegation"];

/// On a host a paired peer may edit a bot (name, persona, language, model
/// choice among configured providers) but never its execution authority:
/// `permissions`, `tools`, or switching the executor to an external `cli:`
/// engine. A save that repeats the stored values is fine; creating a bot
/// with these parts, or changing them, is refused; omitting them on update
/// keeps the stored ones.
pub(crate) fn protect_execution_config(
    peer: Option<&PeerIdentity>,
    host_mode: bool,
    current: Option<&serde_json::Value>,
    next: &mut serde_json::Value,
) -> Result<(), ApiError> {
    if peer.is_none() || !host_mode {
        return Ok(());
    }
    let refuse = || {
        ApiError::forbidden(
            "permissions and tools of a bot on a host are managed on the host itself",
        )
    };
    for key in EXECUTION_KEYS {
        let cur = current.and_then(|c| c.get(key));
        match (next.get(key), cur) {
            (Some(incoming), cur) if Some(incoming) != cur => return Err(refuse()),
            (None, Some(cur)) => {
                next.as_object_mut()
                    .ok_or_else(|| ApiError::bad_request("bot config must be an object"))?
                    .insert(key.into(), cur.clone());
            }
            _ => {}
        }
    }
    let executor = |v: &serde_json::Value| v.pointer("/models/executor").cloned();
    if let Some(new) = executor(next)
        && new.as_str().is_some_and(|e| e.starts_with("cli:"))
        && Some(new) != current.and_then(executor)
    {
        return Err(refuse());
    }
    Ok(())
}

/// THE principle: a paired desktop that holds host administration
/// (`Device::admin`, granted by the host's owner, re-read on every request) IS
/// the owner of that host. It already opens a terminal there and signs the
/// coding CLIs in, so it is let through everything the owner gate and the peer
/// gate refuse to other peers, EXCEPT this list. The list is short on purpose,
/// every row has a reason, and `admin_desktop_never_is_exact` pins it.
///
/// An administrator desktop must never be able to
///
/// 1. mint or change administrators or leave the host in a new state: pairing
///    invites, revoking devices, granting or withdrawing administration,
///    standby/activate/update, state import and hand over (`/v1/host…`, any
///    method except reads);
/// 2. use THIS host as a springboard to a third computer: the host's own
///    outgoing links, their terminals and copy-setup (`/v1/hosts…`);
/// 3. create credentials for the MCP facade (`/v1/mcp-server…`), which other
///    programs on the host present as their own identity;
/// 4. run host repairs or read the host's diagnostics (`/v1/doctor…`);
/// 5. pair a chat channel account (`/v1/channels/telegram/pair-code`): that
///    hands bot control to another person's account;
/// 6. use the signed ingress of an incoming webhook (`/v1/incoming-webhooks…`):
///    its credential is the HMAC secret, never a paired device;
/// 7. grant or withdraw the screen permissions of paired devices
///    (`/v1/view/privileges…`): that is `bot host screen` on the host;
/// 8. reveal the plaintext of a stored fill secret (`POST /v1/fill-secrets/reveal`):
///    a login, password or card is typed by the bot on the host and never needs
///    to be read over the link, so a stolen administrator desktop cannot drain
///    the vault (the handler also insists on the person at the host's own app).
///
/// No other route returns secret material: the API token and the owner key are
/// never an output, provider/mail/search/voice keys and secrets are write-only,
/// MCP facade tokens and host copy-setup are on this list (3, 2), and the one
/// open route that shows a credential once is creating an incoming webhook
/// (its HMAC secret, intended: see `docs/guide/security.md`).
///
/// Two more refusals hold for every peer and are checked elsewhere: answering
/// an overlay question of the host's own screen (`answers_overlay_question`)
/// and the owner key itself (it is never an input or an output of any route;
/// the guard strips it from what a peer sends).
pub(crate) fn admin_desktop_never(method: &Method, check_path: &str) -> Option<&'static str> {
    if is_mutating(method) && under(check_path, "/v1/host") {
        return Some("pairing and host lifecycle are managed on the host itself");
    }
    if under(check_path, "/v1/hosts") {
        return Some("host links are managed by the local user only");
    }
    if under(check_path, "/v1/mcp-server") {
        return Some("the MCP server facade is host-local and requires scoped credentials");
    }
    if under(check_path, "/v1/doctor") {
        return Some("host-local diagnostics are not allowed for peers");
    }
    if *method == Method::POST && check_path == "/v1/channels/telegram/pair-code" {
        return Some("host transfer and channel pairing are done on the host itself");
    }
    if under(check_path, "/v1/incoming-webhooks") {
        return Some("the signed ingress of an incoming webhook is not a peer path");
    }
    if under(check_path, "/v1/view/privileges") {
        return Some("screen permissions of devices are granted on the host itself");
    }
    if *method == Method::POST && check_path == "/v1/fill-secrets/reveal" {
        return Some("revealing a stored secret is done at the host's own app");
    }
    None
}

/// A read of a sensitive collection by the administrator desktop (the audit
/// trail, a chat export, the list of stored secret names, the mail send log,
/// the usage export): attributed like a change, path and method only.
fn sensitive_read(method: &Method, check_path: &str) -> bool {
    if !matches!(*method, Method::GET | Method::HEAD) {
        return false;
    }
    matches!(
        check_path,
        "/v1/audit"
            | "/v1/audit/export"
            | "/v1/fill-secrets"
            | "/v1/email/guard/send-log"
            | "/v1/email/guard/send-log.csv"
            | "/v1/usage/export"
    ) || check_path
        .strip_prefix("/v1/chats/")
        .and_then(|rest| rest.strip_suffix("/export"))
        .is_some_and(|id| !id.is_empty() && !id.contains('/'))
        || is_knowledge_file_path(check_path)
}

/// `/v1/bots/{bot}/knowledge/sources/{source}/file`: the bytes of a file the owner
/// chose to index, opened from a citation.
fn is_knowledge_file_path(check_path: &str) -> bool {
    let parts: Vec<&str> = check_path.trim_start_matches('/').split('/').collect();
    matches!(
        parts.as_slice(),
        ["v1", "bots", bot, "knowledge", "sources", source, "file"] if !bot.is_empty() && !source.is_empty()
    )
}

/// `/v1/bots/{bot}/knowledge/folder-preview`: what choosing a folder would index.
fn is_knowledge_folder_preview_path(check_path: &str) -> bool {
    let parts: Vec<&str> = check_path.trim_start_matches('/').split('/').collect();
    matches!(
        parts.as_slice(),
        ["v1", "bots", bot, "knowledge", "folder-preview"] if !bot.is_empty()
    )
}

/// The name an audit row gives the kind of owner action: the first path segment
/// after `/v1` (and the sub-resource of a bot), never an id, a name or a body.
fn audit_class(check_path: &str) -> String {
    let mut parts = check_path
        .trim_start_matches('/')
        .split('/')
        .skip(1)
        .filter(|segment| !segment.is_empty());
    let first = parts.next().unwrap_or("-");
    if first == "bots" {
        return match parts.nth(1) {
            Some(sub) => format!("bots/{sub}"),
            None => "bots".to_owned(),
        };
    }
    first.to_owned()
}

/// What an ORDINARY Desktop peer may not change on a daemon that runs as an
/// always-on host (`mode = host`): there it is an operator of bots, runs, chats
/// and the screen. Credentials, tool servers, skills and the policy are the
/// owner's, and the host's administrator desktop is the owner
/// (`admin_desktop_never` lists what even that desktop may not do). Returns
/// the refusal reason for `method path`.
fn host_mode_desktop_denial(method: &Method, check_path: &str) -> Option<&'static str> {
    let mutating = is_mutating(method);
    let under = |prefix: &str| under(check_path, prefix);
    if under("/v1/policy") && mutating {
        return Some("the execution policy is managed on the host itself");
    }
    if under("/v1/secrets") && mutating {
        return Some("secrets are managed on the host itself");
    }
    if under("/v1/import") && mutating {
        return Some("other assistants' setups are imported on the host itself");
    }
    if (under("/v1/mcp") || under("/v1/skills") || is_bot_skills_path(check_path)) && mutating {
        return Some("tool servers and skills are managed on the host itself");
    }
    if (under("/v1/email")
        || under("/v1/web/search")
        || under("/v1/calendar")
        || under("/v1/homeassistant"))
        && mutating
    {
        return Some("account credentials are managed on the host itself");
    }
    // The dictation key and endpoint: audio could be redirected to another service.
    // Transcribing (POST /v1/voice/transcribe) stays open to the operator.
    if check_path == "/v1/voice" && mutating {
        return Some("the dictation service and its key are managed on the host itself");
    }
    if *method == Method::POST
        && (PHONE_DENIED_POST_PATHS.contains(&check_path)
            || check_path == "/v1/channels/telegram/pair-code"
            || check_path == "/v1/channels/discord/pair-code")
    {
        return Some("host transfer and channel pairing are done on the host itself");
    }
    None
}

/// `POST /v1/chats` and `POST /v1/chats/{id}/messages` start work on this
/// daemon; a paired peer doing so is attributed in the audit log.
fn starts_chat_work(method: &Method, check_path: &str) -> bool {
    if *method != Method::POST {
        return false;
    }
    if check_path == "/v1/chats" {
        return true;
    }
    check_path
        .strip_prefix("/v1/chats/")
        .and_then(|rest| rest.strip_suffix("/messages"))
        .is_some_and(|id| !id.is_empty() && !id.contains('/'))
}

/// The owner-only fill-secret management prefixes, refused to ALL paired peers
/// for every method (a device must never create a secret, widen an allow-list
/// or add a standing grant). `crate::fill_routes::is_fill_path` matches them on
/// the decoded, slash-normalised path.
pub const PEER_DENIED_FILL_PREFIXES: &[&str] = &[
    "/v1/fill-secrets",
    "/v1/fill-grants",
    "/v1/secret-requests",
    "/v1/handovers",
];

/// MCP server management starts programs and names host secrets: a mutation
/// is an action of the local user at this host, never of a paired peer.
pub const PEER_DENIED_PREFIX_MCP: &str = "/v1/mcp";

/// Whether `path` (decoded and slash-normalized) is under `/v1/mcp`.
fn is_mcp_path(path: &str) -> bool {
    path.strip_prefix(PEER_DENIED_PREFIX_MCP)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// Whether `path` (decoded and slash-normalized) is a diagnostics endpoint.
fn is_doctor_path(path: &str) -> bool {
    path.strip_prefix(PEER_DENIED_PREFIX_DOCTOR)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// Composio app connections are the owner's: refused to every paired role
/// except Desktop (which acts for the owner), for every method.
pub const PEER_OWNER_PREFIX_COMPOSIO: &str = "/v1/composio";

/// Whether `path` (decoded and slash-normalized) is a Composio endpoint.
fn is_composio_path(path: &str) -> bool {
    path.strip_prefix(PEER_OWNER_PREFIX_COMPOSIO)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// Installing local components is the owner's too (same rule as Composio).
pub const PEER_OWNER_PREFIX_COMPONENTS: &str = "/v1/components";

fn is_components_path(path: &str) -> bool {
    path.strip_prefix(PEER_OWNER_PREFIX_COMPONENTS)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// Same API and permissions as loopback, with transport authentication.
pub fn peer_router(state: AppState) -> Router {
    crate::api_routes()
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::mission_routes::mission_gate,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::live_view::audit_ingress,
        ))
        .layer(axum::middleware::from_fn_with_state(state.clone(), guard))
        .layer(axum::middleware::from_fn(crate::logs::request_log))
        .with_state(state)
}

/// Optional paired device display name for audit attribution.
#[derive(Clone, Debug)]
pub struct PeerName(pub String);

fn normalize_deny_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut prev_slash = false;
    for ch in path.chars() {
        if ch == '/' {
            if !prev_slash {
                out.push('/');
                prev_slash = true;
            }
        } else {
            out.push(ch);
            prev_slash = false;
        }
    }
    if out.len() > 1 && out.ends_with('/') {
        out.pop();
    }
    out
}

/// A write to `/v1/bots/{id}/site-rules[/...]` (the owner's per-site and per-app rules).
fn is_site_rules_write(method: &Method, path: &str) -> bool {
    !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
        && path
            .strip_prefix("/v1/bots/")
            .and_then(|rest| rest.split_once('/'))
            .is_some_and(|(bot, rest)| {
                !bot.is_empty() && (rest == "site-rules" || rest.starts_with("site-rules/"))
            })
}

/// Why `method path` is refused to every paired peer role, if it is: the
/// host-local provider management, MCP management and diagnostics endpoints.
#[allow(clippy::too_many_lines)] // one ordered chain of guard checks
fn host_local_denial(method: &Method, check_path: &str) -> Option<&'static str> {
    if crate::browser_profiles::is_profile_path(check_path) {
        return Some("browser sessions are managed by the local owner only");
    }
    if check_path == "/v1/channels/discord/pair-code" {
        return Some("Discord pairing is done on this computer by the owner");
    }
    if is_incoming_webhook_path(check_path) {
        return Some("incoming webhooks are managed on this computer by the owner");
    }
    if under(check_path, "/v1/logs") {
        return Some("recent logs are available only to the local owner");
    }
    if under(check_path, "/v1/audit") {
        return Some("the audit log is available only to the local owner");
    }
    if under(check_path, "/v1/email/guard") {
        return Some(
            "the outgoing-mail guard (send log, opt-out list, limits) is an owner action on this host",
        );
    }
    if under(check_path, "/v1/connection-test") {
        return Some("connection tests spend this host's stored keys and are an owner action");
    }
    if under(check_path, "/v1/mcp/import") {
        return Some("MCP imports are managed by the owner of this host");
    }
    if check_path == "/v1/mcp-server" || check_path.starts_with("/v1/mcp-server/") {
        return Some("the MCP server facade is host-local and requires scoped credentials");
    }
    if check_path == "/v1/profile-share" || check_path.starts_with("/v1/profile-share/") {
        return Some("bot profile files are handled by the owner on this host only");
    }
    if under(check_path, "/v1/favicons") {
        return Some("site icons are fetched for the local owner only");
    }
    if under(check_path, "/v1/missions") {
        return Some("missions (goals, ledgers, answers) are the owner's on this host");
    }
    if check_path == "/v1/data-usage" || check_path.starts_with("/v1/data-usage/") {
        return Some("data folder sizes and Clear cache are a host-local owner action");
    }
    if is_workspace_owner_path(check_path) {
        return Some("workspace files, settings and cleanup are a host-local owner action");
    }
    if under(check_path, "/v1/embeddings") || under(check_path, "/v1/decision-helper") {
        return Some("embedding providers and their keys are a host-local owner setting");
    }
    if check_path == "/v1/trusted-folders" {
        return Some("trusted folders are a host-local owner setting");
    }
    if (check_path == "/v1/providers/cli"
        || check_path.starts_with("/v1/providers/cli/")
        || check_path == "/v1/providers/detected"
        || check_path == "/v1/local-models"
        || check_path == "/v1/local-install")
        || (*method == Method::POST && PEER_DENIED_POST_PATHS.contains(&check_path))
    {
        return Some("host-local provider management is not allowed for peers");
    }
    if check_path
        .strip_prefix("/v1/mcp/servers/")
        .and_then(|rest| rest.split_once('/'))
        .is_some_and(|(_, rest)| rest == "auth" || rest.starts_with("auth/"))
    {
        return Some("MCP sign-in is a host-local user action, forbidden to peers");
    }
    // MCP configuration decides which secrets are sent where and which
    // commands run on this host: create / update / delete / reconnect are
    // host-local user actions for every peer role (no card or ADR grants a
    // peer role MCP management).  Reading the list and tools stays allowed.
    if *method != Method::GET && is_mcp_servers_path(check_path) {
        return Some("MCP server management is a host-local user action, forbidden to peers");
    }
    if !matches!(*method, Method::GET | Method::HEAD)
        && (check_path == "/v1/skills" || check_path.starts_with("/v1/skills/"))
    {
        return Some("shared skill management is local-owner only");
    }
    if *method == Method::POST && PEER_DENIED_POST_PATHS.contains(&check_path) {
        return Some("host-local provider management is not allowed for peers");
    }
    if is_hosts_path(check_path) {
        return Some("host links are managed by the local user only");
    }
    if crate::fill_routes::is_fill_path(check_path) {
        return Some("fill secrets are managed by the local owner only");
    }
    if is_mcp_path(check_path) && !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) {
        return Some("MCP server management is only allowed for the local user");
    }
    if !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
        && check_path
            .strip_prefix("/v1/bots/")
            .and_then(|rest| rest.split_once('/'))
            .is_some_and(|(bot, rest)| {
                !bot.is_empty() && (rest == "knowledge" || rest.starts_with("knowledge/"))
            })
    {
        return Some("knowledge sources and embedding authority are managed by the local owner");
    }
    // Choosing a folder is the local owner's act, so a peer may not browse the host's
    // folders (the listing would only be the first step of choosing one), preview one,
    // or read an indexed file's bytes: whatever the method and whatever the peer's role.
    if check_path == "/v1/knowledge/folders"
        || is_knowledge_folder_preview_path(check_path)
        || is_knowledge_file_path(check_path)
    {
        return Some("the host's folders and files are browsed and opened by the local owner");
    }
    if is_site_rules_write(method, check_path) {
        return Some("per-site and per-app permission rules are managed by the local owner");
    }
    if !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
        && (check_path == "/v1/memory" || check_path.starts_with("/v1/memory/"))
    {
        return Some("personal memory is managed by the local owner");
    }
    if let Some(why) = bot_setting_denial(method, check_path) {
        return Some(why);
    }
    is_doctor_path(check_path).then_some("host-local diagnostics are not allowed for peers")
}

fn is_workspace_owner_path(path: &str) -> bool {
    under(path, "/v1/workspaces")
        || path
            .strip_prefix("/v1/bots/")
            .and_then(|rest| rest.split_once('/'))
            .is_some_and(|(bot, rest)| {
                !bot.is_empty() && (rest == "workspace" || rest.starts_with("workspace/"))
            })
}

/// A write to one of a bot's own safety settings (memory, engine, spending limits, page reader):
/// only this computer may change them.
fn bot_setting_denial(method: &Method, check_path: &str) -> Option<&'static str> {
    if *method == Method::GET {
        return None;
    }
    if is_bot_memory_settings_path(check_path) {
        return Some("a bot's memory settings can only be changed at this computer");
    }
    if is_bot_engine_path(check_path) {
        return Some("a bot's engine settings can only be changed at this computer");
    }
    if is_bot_spend_limits_path(check_path) {
        return Some("a bot's spending limits can only be changed at this computer");
    }
    if is_bot_page_reader_path(check_path) {
        return Some("a bot's page reader setting can only be changed at this computer");
    }
    None
}

/// An encoded `/` (`%2F`) inside a path that is under `/v1/mcp`.
fn encoded_slash_in_mcp_path(raw: &str, decoded: &str) -> bool {
    is_mcp_path(decoded) && raw.to_ascii_lowercase().contains("%2f")
}

/// `GET /v1/email/accounts/{name}/messages`: the headers of a mailbox.
fn is_email_messages_path(path: &str) -> bool {
    path.strip_prefix("/v1/email/accounts/")
        .and_then(|rest| rest.strip_suffix("/messages"))
        .is_some_and(|name| !name.is_empty() && !name.contains('/'))
}

/// `/v1/mcp/servers` or anything below it (decoded and slash-normalized).
fn is_mcp_servers_path(path: &str) -> bool {
    path.strip_prefix("/v1/mcp/servers")
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// `POST /v1/control/{request_id}` answers an overlay question: local only.
fn answers_overlay_question(path: &str) -> bool {
    path.strip_prefix("/v1/control/")
        .is_some_and(|rest| uuid::Uuid::parse_str(rest).is_ok())
}

/// `POST /v1/runs/stop` from a role that may not stop runs.
fn stop_refused(method: &Method, check_path: &str, role: PeerRole) -> bool {
    *method == Method::POST
        && check_path == PEER_RUN_STOP_PATH
        && !matches!(role, PeerRole::Desktop | PeerRole::Phone)
}

// File contents and host restores require a desktop user.
fn host_file_review(path: &str) -> bool {
    path.starts_with("/v1/runs/") && path.split('/').nth(4) == Some("changes")
}

/// `/v1/bots/{id}/memory/settings`: whether and how a bot remembers.
fn is_bot_memory_settings_path(path: &str) -> bool {
    path.strip_prefix("/v1/bots/")
        .and_then(|rest| rest.strip_suffix("/memory/settings"))
        .is_some_and(|id| !id.is_empty() && !id.contains('/'))
}

/// `/v1/bots/{id}/engine[...]` decides who runs a bot's files and shell.
fn is_bot_engine_path(path: &str) -> bool {
    path.strip_prefix("/v1/bots/")
        .and_then(|rest| rest.split_once("/engine"))
        .is_some_and(|(id, tail)| {
            !id.is_empty() && !id.contains('/') && (tail.is_empty() || tail == "/kill")
        })
}

/// Incoming webhook management and the signed ingress. Peers never call either:
/// a tunnel the owner chooses hits loopback, not the peer router.
fn is_incoming_webhook_path(path: &str) -> bool {
    if under(path, "/v1/incoming-webhooks") {
        return true;
    }
    let mut parts = path.split('/');
    parts.next() == Some("")
        && parts.next() == Some("v1")
        && parts.next() == Some("bots")
        && parts.next().is_some_and(|id| !id.is_empty())
        && parts.next() == Some("incoming-webhooks")
}

/// `/v1/bots/{id}/spend-limits`: the cap a bot (or a peer) must not lift.
fn is_bot_spend_limits_path(path: &str) -> bool {
    path.strip_prefix("/v1/bots/")
        .and_then(|rest| rest.strip_suffix("/spend-limits"))
        .is_some_and(|id| !id.is_empty() && !id.contains('/'))
}

/// `/v1/bots/{id}/page-reader`: the protection between a page and the bot.
fn is_bot_page_reader_path(path: &str) -> bool {
    path.strip_prefix("/v1/bots/")
        .and_then(|rest| rest.strip_suffix("/page-reader"))
        .is_some_and(|id| !id.is_empty() && !id.contains('/'))
}

/// `/v1/bots/{id}/skills` mutates a bot's prompt-trust boundary.
fn is_bot_skills_path(path: &str) -> bool {
    path.strip_prefix("/v1/bots/")
        .and_then(|rest| rest.strip_suffix("/skills"))
        .is_some_and(|id| !id.is_empty() && !id.contains('/'))
}

fn audit_chat_start(
    state: &AppState,
    peer: &PeerIdentity,
    req: &Request,
    check_path: &str,
) -> Result<(), storage::StorageError> {
    let name = req
        .extensions()
        .get::<PeerName>()
        .map_or(peer.key.as_str(), |n| n.0.as_str());
    let role = if peer.role == PeerRole::Phone {
        "phone"
    } else {
        "desktop"
    };
    state
        .storage()
        .append_audit(
            state.user_id(),
            None,
            &format!("{role}:{name}"),
            "peer.chat_requested",
            &serde_json::json!({"peer_key":peer.key,"path":check_path}),
        )
        .map(|_| ())
}

/// Why an ORDINARY peer (not the host's administrator desktop) is refused
/// `method path` as an owner action, if it is: the owner settings
/// (`host_local_denial`, `host_mode_desktop_denial`) and every owner-gated
/// route that is not one a paired device may call
/// (`crate::owner::peer_may_call`). A desktop of a daemon that is not an
/// always-on host keeps its older, wider reach.
fn owner_denial(
    method: &Method,
    check_path: &str,
    role: PeerRole,
    host_mode: bool,
) -> Option<&'static str> {
    if let Some(why) = host_local_denial(method, check_path) {
        return Some(why);
    }
    if !host_mode && role == PeerRole::Desktop {
        return None;
    }
    host_mode_desktop_denial(method, check_path).or_else(|| {
        (!crate::owner::peer_may_call(role, method.as_str(), check_path))
            .then_some("peer role does not allow this operation")
    })
}

#[allow(clippy::too_many_lines)] // one ordered chain of guard checks; order is the contract
async fn guard(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let Some(peer) = req.extensions().get::<PeerIdentity>().cloned() else {
        return ApiError::forbidden("paired peer identity required").into_response();
    };
    // Decode and collapse redundant slashes before checking: encoded or
    // non-canonical path characters must not bypass scopes.
    let path = match percent_encoding::percent_decode_str(req.uri().path()).decode_utf8() {
        Ok(path) => path.into_owned(),
        Err(_) => return ApiError::forbidden("invalid path").into_response(),
    };
    let check_path = normalize_deny_path(&path);
    let method = req.method().clone();
    let host_mode = state.leader().config().mode == crate::host::HostMode::Host;
    let desktop_on_host = host_mode && peer.role == PeerRole::Desktop;
    // THE principle (see `admin_desktop_never`): the host's administrator
    // desktop is the owner. It is let through the owner settings and the
    // owner-gated routes that every other peer is refused, minus a short list.
    let admin = desktop_on_host && state.is_host_admin(Some(&peer));
    let owner_denied = owner_denial(&method, &check_path, peer.role, host_mode);
    let mut owner_action = false;
    if desktop_on_host
        && (admin || owner_denied.is_some())
        && let Some(never) = admin_desktop_never(&method, &check_path)
    {
        return ApiError::forbidden(never).into_response();
    }
    match owner_denied {
        None => {}
        Some(why) if !desktop_on_host => return ApiError::forbidden(why).into_response(),
        // An ordinary desktop is told what to do, not just "no".
        Some(_) if !admin => {
            return ApiError::new(
                axum::http::StatusCode::FORBIDDEN,
                crate::host_admin::codes::REQUIRED,
                format!(
                    "the owner must confirm this desktop on the host: run `{}`",
                    crate::host_admin::CONFIRM_COMMAND
                ),
            )
            .into_response();
        }
        Some(_) => owner_action = true,
    }
    if is_components_path(&check_path) && peer.role != PeerRole::Desktop {
        return ApiError::forbidden("components are managed by the owner's desktop only")
            .into_response();
    }
    if (is_email_messages_path(&check_path) || under(&check_path, "/v1/email/channel"))
        && peer.role != PeerRole::Desktop
    {
        return ApiError::forbidden("mailboxes are read from the owner's desktop only")
            .into_response();
    }
    if is_composio_path(&check_path) && peer.role != PeerRole::Desktop {
        return ApiError::forbidden("Composio connections are managed by the owner's desktop only")
            .into_response();
    }
    // An encoded `/` inside an MCP path (e.g. in the server-name segment) is
    // never legitimate: refuse it for every method.
    if encoded_slash_in_mcp_path(req.uri().path(), &check_path) {
        return ApiError::forbidden("encoded slash in an MCP path").into_response();
    }
    let denied = peer.role != PeerRole::Desktop
        && (host_file_review(&check_path)
            || peer.role != PeerRole::Phone
            // Workflow definitions reveal commands and destinations. Phones
            // have no read, proposal, mutation or replay access.
            || check_path == "/v1/workflows"
            || check_path.starts_with("/v1/workflows/")
            || check_path.ends_with("/workflow-proposal")
            || (req.method() == Method::POST
                && PHONE_DENIED_POST_PATHS.contains(&check_path.as_str()))
            || (matches!(*req.method(), Method::POST | Method::PUT)
                && check_path.starts_with("/v1/secrets"))
            || (req.method() != Method::GET
                && (is_bot_skills_path(&check_path) || check_path == "/v1/skills")));
    if denied || stop_refused(req.method(), &check_path, peer.role) {
        return ApiError::forbidden("peer role does not allow this operation").into_response();
    }
    if host_mode {
        // Pairing and the host lifecycle (invite, revoke, standby, activate,
        // update, import, handover) are done on the host itself: a paired
        // peer, whatever its role and even as administrator, cannot mint
        // tickets, revoke devices or change who leads.
        if is_mutating(req.method()) && under(&check_path, "/v1/host") {
            return ApiError::forbidden(
                "pairing and host lifecycle are managed on the host itself",
            )
            .into_response();
        }
    }
    if req.method() == Method::POST && answers_overlay_question(&check_path) {
        return ApiError::forbidden("only the local user answers overlay questions")
            .into_response();
    }
    if crate::live_view::reject_query_credentials(&req) {
        return ApiError::bad_request("URL credentials are forbidden").into_response();
    }
    req.extensions_mut()
        .insert(crate::live_view::Actor::peer_for(&state, &peer));
    // The owner key is never something a peer brings. The administrator desktop
    // of a host is the owner there, so the handlers that check for the owner
    // (they only see headers) are shown this host's own key; nobody else gets it.
    req.headers_mut().remove(crate::owner::OWNER_HEADER);
    if admin
        && let Some(key) = state.owner_key()
        && let Ok(mut value) = axum::http::HeaderValue::from_str(&key)
    {
        value.set_sensitive(true);
        req.headers_mut().insert(crate::owner::OWNER_HEADER, value);
    }
    // An owner change made by the administrator desktop is attributed before it
    // runs (fail closed) and shares the administration budget. Class, method
    // and path only, never the body. Decisions have their own rows below.
    let decision = path.starts_with("/v1/approvals/")
        || path.starts_with("/v1/questions/")
        || path.starts_with("/v1/control/");
    let policy_before = (admin && method == Method::PUT && check_path == "/v1/policy")
        .then(|| crate::routes::coarse_policy_level(&state));
    if admin
        && !decision
        && ((is_mutating(&method)
            && (owner_action || crate::owner::is_owner_route(method.as_str(), &check_path)))
            || sensitive_read(&method, &check_path))
    {
        if let Err(error) = state.leader().admit_admin_call() {
            return error.into_response();
        }
        if let Err(error) = crate::host_admin::audit(
            &state,
            &crate::host_admin::admin_for(&peer.key),
            "host.admin.settings",
            &serde_json::json!({"class": audit_class(&check_path), "method": method.as_str(), "path": check_path}),
        ) {
            return error.into_response();
        }
    }
    // New schedule source/read capabilities remain local-owner only even on
    // encoded or slash-normalized aliases. Existing plain timer requests keep
    // their earlier peer policy; handlers also guard already-configured rows.
    let req = if matches!(*req.method(), Method::POST | Method::PUT)
        && (check_path == "/v1/schedules"
            || check_path
                .strip_prefix("/v1/schedules/")
                .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok()))
    {
        let (parts, body) = req.into_parts();
        let Ok(bytes) = axum::body::to_bytes(body, 128 * 1024).await else {
            return ApiError::bad_request("schedule body exceeds limit").into_response();
        };
        if serde_json::from_slice::<serde_json::Value>(&bytes)
            .is_ok_and(|v| v.get("precheck").is_some() || v.get("trigger").is_some())
        {
            return ApiError::forbidden("schedule triggers and prechecks are local-owner only")
                .into_response();
        }
        Request::from_parts(parts, axum::body::Body::from(bytes))
    } else {
        req
    };
    let (req, control_transition) = if req.method() == Method::PUT && path == "/v1/control/state" {
        let (parts, body) = req.into_parts();
        let Ok(bytes) = axum::body::to_bytes(body, 4096).await else {
            return ApiError::bad_request("control state body exceeds 4096 bytes").into_response();
        };
        let input: crate::dto::ControlStateInput = match serde_json::from_slice(&bytes) {
            Ok(input) => input,
            Err(e) => return ApiError::bad_request(e.to_string()).into_response(),
        };
        // A device without the input grant is refused by the handler (and that
        // denial is coalesced there): it must not write a decision row per try.
        let granted = state
            .live_view()
            .allowed(&crate::live_view::Actor::peer_for(&state, &peer), true);
        let changed = granted && (input.release || state.hub().reported_mode() != Some(input.mode));
        (
            Request::from_parts(parts, axum::body::Body::from(bytes)),
            changed,
        )
    } else {
        (req, false)
    };
    // Record the initiator in the existing SQLite audit log before dispatch.
    // Fail closed if attribution cannot be persisted. The normal approval audit
    // and permission engine remain in place and record the resulting decision.
    let mut audit_actor = None;
    let refused_control = matches!(
        path.as_str(),
        "/v1/control/release"
            | "/v1/control/takeover"
            | "/v1/control/override"
            | "/v1/control/recover"
    ) && !state
        .live_view()
        .allowed(&crate::live_view::Actor::peer_for(&state, &peer), true);
    if (req.method() == Method::POST
        && !refused_control
        && (path.starts_with("/v1/approvals/")
            || path.starts_with("/v1/questions/")
            || path.starts_with("/v1/control/")
            || check_path == PEER_RUN_STOP_PATH))
        || control_transition
    {
        let role = if peer.role == PeerRole::Phone {
            "phone"
        } else {
            "desktop"
        };
        let name = req
            .extensions()
            .get::<PeerName>()
            .map_or(peer.key.as_str(), |n| n.0.as_str());
        let actor = format!("{role}:{name}");
        if let Err(error) = state.storage().append_audit(
            state.user_id(),
            None,
            &actor,
            "peer.decision_requested",
            &serde_json::json!({"peer_key":peer.key,"path":path}),
        ) {
            return ApiError::internal(error).into_response();
        }
        audit_actor = Some(actor);
    }
    if starts_chat_work(req.method(), &check_path)
        && let Err(error) = audit_chat_start(&state, &peer, &req, &check_path)
    {
        // Fail closed: remote work is not started without its attribution row.
        return ApiError::internal(error).into_response();
    }
    let response = next.run(req).await;
    // The policy is the one change whose size matters at a glance: the coarse level (a preset name
    // or `custom`) before and after, never the body.
    if let Some(before) = policy_before
        && response.status().is_success()
        && let Err(error) = crate::host_admin::audit(
            &state,
            &crate::host_admin::admin_for(&peer.key),
            "host.admin.settings",
            &serde_json::json!({"class": "policy", "method": "PUT", "path": "/v1/policy",
                "level_before": before, "level_after": crate::routes::coarse_policy_level(&state)}),
        )
    {
        return error.into_response();
    }
    if let Some(actor) = audit_actor
        && let Err(error) = state.storage().append_audit(
            state.user_id(), None, &actor, "peer.decision_completed",
            &serde_json::json!({"peer_key":peer.key,"path":path,"status":response.status().as_u16()}),
        ) { return ApiError::internal(error).into_response(); }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peers_never_touch_the_mail_guard_routes() {
        for path in [
            "/v1/email/guard/settings",
            "/v1/email/guard/opt-out",
            "/v1/email/guard/opt-out/ann%40x.com",
            "/v1/email/guard/send-log",
            "/v1/email/guard/send-log.csv",
        ] {
            for method in [
                Method::GET,
                Method::HEAD,
                Method::POST,
                Method::PUT,
                Method::DELETE,
            ] {
                assert!(
                    host_local_denial(&method, path).is_some(),
                    "{method} {path}"
                );
            }
        }
    }

    /// The short list of what even the host's administrator desktop may not do:
    /// every row, and the neighbours that must stay open.
    #[test]
    fn admin_desktop_never_is_exact() {
        for (method, path) in [
            // 1. administrators and the host's lifecycle
            (Method::POST, "/v1/host/invite"),
            (Method::DELETE, "/v1/host/devices/abc"),
            (Method::PUT, "/v1/host/devices/abc/admin"),
            (Method::POST, "/v1/host/standby"),
            (Method::POST, "/v1/host/activate"),
            (Method::POST, "/v1/host/update"),
            (Method::POST, "/v1/host/import"),
            (Method::POST, "/v1/host/handover"),
            // 2. the host's own links to other computers
            (Method::GET, "/v1/hosts"),
            (Method::POST, "/v1/hosts/pair"),
            (Method::POST, "/v1/hosts/abc/terminal"),
            (Method::POST, "/v1/hosts/abc/copy-setup"),
            (Method::PUT, "/v1/hosts/abc/computer-access"),
            // 3. MCP facade credentials
            (Method::POST, "/v1/mcp-server/tokens"),
            (Method::DELETE, "/v1/mcp-server/tokens/abc"),
            // 4. repairs and diagnostics
            (Method::GET, "/v1/doctor"),
            (Method::POST, "/v1/doctor/repair/x"),
            // 5. chat channel pairing
            (Method::POST, "/v1/channels/telegram/pair-code"),
            // 6. the signed webhook ingress
            (Method::POST, "/v1/incoming-webhooks/abc"),
            // 7. screen permissions of devices
            (Method::PUT, "/v1/view/privileges/abc"),
            // 8. plaintext of a stored secret
            (Method::POST, "/v1/fill-secrets/reveal"),
        ] {
            assert!(
                admin_desktop_never(&method, path).is_some(),
                "{method} {path}"
            );
        }
        for (method, path) in [
            // reads of the host's own state stay open
            (Method::GET, "/v1/host"),
            (Method::GET, "/v1/host/screen"),
            // owner settings are the administrator's
            (Method::PUT, "/v1/policy"),
            (Method::POST, "/v1/secrets/provider/key"),
            (Method::POST, "/v1/mcp/servers"),
            (Method::PUT, "/v1/mcp/servers/x"),
            (Method::POST, "/v1/skills"),
            (Method::GET, "/v1/fill-secrets"),
            (Method::POST, "/v1/fill-secrets"),
            (Method::DELETE, "/v1/fill-secrets/site/login"),
            (Method::POST, "/v1/fill-secrets/reveal/more"),
            (Method::PUT, "/v1/email/channel"),
            (Method::PUT, "/v1/email/guard/settings"),
            (Method::POST, "/v1/connection-test/provider"),
            (Method::GET, "/v1/audit"),
            (Method::GET, "/v1/chats/search"),
            (Method::PUT, "/v1/web/search"),
            (Method::POST, "/v1/calendar/accounts"),
            (Method::POST, "/v1/calendar/test"),
            (Method::PUT, "/v1/voice"),
            (Method::PUT, "/v1/spend-limit"),
            (Method::POST, "/v1/bots/abc/incoming-webhooks"),
            (
                Method::POST,
                "/v1/bots/abc/incoming-webhooks/x/rotate-secret",
            ),
            (Method::GET, "/v1/channels/telegram"),
            // a path that only starts like a listed one
            (Method::POST, "/v1/hostage"),
            (Method::GET, "/v1/mcp-servers"),
            (Method::POST, "/v1/doctors"),
        ] {
            assert_eq!(admin_desktop_never(&method, path), None, "{method} {path}");
        }
    }

    #[test]
    fn sensitive_reads_of_the_admin_desktop_are_attributed() {
        for path in [
            "/v1/audit",
            "/v1/audit/export",
            "/v1/fill-secrets",
            "/v1/email/guard/send-log.csv",
            "/v1/usage/export",
            "/v1/chats/0190f/export",
            "/v1/bots/0190f/knowledge/sources/0190a/file",
        ] {
            assert!(sensitive_read(&Method::GET, path), "{path}");
            assert!(sensitive_read(&Method::HEAD, path), "{path}");
            assert!(!sensitive_read(&Method::POST, path), "{path}");
        }
        for path in [
            "/v1/chats",
            "/v1/chats/x/y/export",
            "/v1/policy",
            "/v1/bots",
            "/v1/bots/0190f/knowledge/sources",
            "/v1/bots/0190f/knowledge/sources/0190a",
            "/v1/bots/0190f/knowledge/sources/0190a/file/x",
        ] {
            assert!(!sensitive_read(&Method::GET, path), "{path}");
        }
    }

    #[test]
    fn an_audit_class_names_the_kind_of_change_and_never_an_id() {
        for (path, class) in [
            ("/v1/policy", "policy"),
            ("/v1/email/accounts/work", "email"),
            ("/v1/web/search", "web"),
            ("/v1/voice", "voice"),
            ("/v1/bots/0190f/engine", "bots/engine"),
            ("/v1/bots/0190f/knowledge/sources/s1", "bots/knowledge"),
            ("/v1/bots/0190f", "bots"),
            ("/v1/secrets/provider/key", "secrets"),
        ] {
            assert_eq!(audit_class(path), class, "{path}");
        }
    }

    #[test]
    fn an_ordinary_peer_is_refused_every_owner_route_that_is_not_a_device_task() {
        // A phone decides approvals and runs timers; it never changes the policy.
        for (role, method, path, refused) in [
            (PeerRole::Phone, Method::POST, "/v1/approvals/x", false),
            (PeerRole::Phone, Method::POST, "/v1/schedules", false),
            (PeerRole::Phone, Method::PUT, "/v1/policy", true),
            (PeerRole::Phone, Method::PUT, "/v1/spend-limit", true),
            (
                PeerRole::Phone,
                Method::POST,
                "/v1/voice/local/install",
                true,
            ),
            (PeerRole::Phone, Method::GET, "/v1/workflows", true),
            (
                PeerRole::Phone,
                Method::POST,
                "/v1/channels/telegram/pair-code",
                true,
            ),
            // An ordinary desktop of a host keeps its operator tasks, not the settings.
            (PeerRole::Desktop, Method::POST, "/v1/approvals/x", false),
            (PeerRole::Desktop, Method::GET, "/v1/workflows", false),
            (PeerRole::Desktop, Method::PUT, "/v1/composio/key", false),
            (PeerRole::Desktop, Method::PUT, "/v1/policy", true),
            (PeerRole::Desktop, Method::PUT, "/v1/spend-limit", true),
            (PeerRole::Desktop, Method::GET, "/v1/chats/search", true),
            (PeerRole::Desktop, Method::GET, "/v1/usage/export", true),
            (PeerRole::Desktop, Method::PUT, "/v1/bots/x/files", true),
            (
                PeerRole::Desktop,
                Method::POST,
                "/v1/chats/x/skill-draft",
                true,
            ),
            // Calendar logins are credentials, like the mail accounts: a phone never changes
            // them and an ordinary desktop of a host does not either. Reading the list is fine.
            (PeerRole::Phone, Method::POST, "/v1/calendar/accounts", true),
            (
                PeerRole::Phone,
                Method::PUT,
                "/v1/calendar/accounts/home",
                true,
            ),
            (
                PeerRole::Phone,
                Method::DELETE,
                "/v1/calendar/accounts/home",
                true,
            ),
            (PeerRole::Phone, Method::POST, "/v1/calendar/test", true),
            (
                PeerRole::Desktop,
                Method::POST,
                "/v1/calendar/accounts",
                true,
            ),
            (
                PeerRole::Desktop,
                Method::PUT,
                "/v1/calendar/accounts/home",
                true,
            ),
            (
                PeerRole::Desktop,
                Method::DELETE,
                "/v1/calendar/accounts/home",
                true,
            ),
            (PeerRole::Desktop, Method::POST, "/v1/calendar/test", true),
            (
                PeerRole::Desktop,
                Method::GET,
                "/v1/calendar/accounts",
                false,
            ),
        ] {
            assert_eq!(
                owner_denial(&method, path, role, true).is_some(),
                refused,
                "{role:?} {method} {path}"
            );
        }
        // A desktop of a daemon that is not a host keeps its older reach.
        assert!(owner_denial(&Method::PUT, "/v1/policy", PeerRole::Desktop, false).is_none());
        assert!(owner_denial(&Method::PUT, "/v1/policy", PeerRole::Phone, false).is_some());
        // The calendar is no different from the mail accounts there: the paired desktop is the
        // owner's other computer. What it cannot do is read a saved password back by pointing
        // the account elsewhere (the daemon refuses an update that moves the address or the
        // login without the password typed again), so it needs no extra wall here.
        for path in ["/v1/calendar/accounts/home", "/v1/email/accounts/work"] {
            assert!(owner_denial(&Method::PUT, path, PeerRole::Desktop, false).is_none());
            assert!(owner_denial(&Method::PUT, path, PeerRole::Phone, false).is_some());
        }
    }

    #[test]
    fn peers_never_change_a_bots_engine() {
        for path in ["/v1/bots/abc/engine", "/v1/bots/abc/engine/kill"] {
            for method in [Method::PUT, Method::POST] {
                assert!(
                    host_local_denial(&method, path).is_some(),
                    "{method} {path}"
                );
            }
        }
        assert!(host_local_denial(&Method::PUT, "/v1/bots/abc").is_none());
    }

    #[test]
    fn peers_never_run_connection_tests() {
        for path in ["/v1/connection-test/provider", "/v1/connection-test/mcp"] {
            for method in [Method::POST, Method::GET] {
                assert!(
                    host_local_denial(&method, path).is_some(),
                    "{method} {path}"
                );
            }
        }
    }

    #[test]
    fn peers_never_read_data_usage_or_clear_the_cache() {
        // Role-independent: host_local_denial runs before any role check.
        for path in ["/v1/data-usage", "/v1/data-usage/clear-cache"] {
            for method in [Method::GET, Method::POST, Method::DELETE] {
                assert!(
                    host_local_denial(&method, path).is_some(),
                    "{method} {path}"
                );
            }
        }
    }

    #[test]
    fn peers_never_touch_missions() {
        for path in [
            "/v1/missions",
            "/v1/missions/abc",
            "/v1/missions/abc/answers",
            "/v1/missions/abc/cancel",
        ] {
            for method in [Method::GET, Method::HEAD, Method::POST] {
                assert!(
                    host_local_denial(&method, path).is_some(),
                    "{method} {path}"
                );
            }
        }
        assert!(host_local_denial(&Method::GET, "/v1/missions-x").is_none());
    }

    #[test]
    fn peers_never_fetch_favicons() {
        for method in [Method::GET, Method::HEAD, Method::POST] {
            assert!(
                host_local_denial(&method, "/v1/favicons/example.com").is_some(),
                "{method}"
            );
        }
    }

    #[test]
    fn peers_never_change_a_bots_spend_limits() {
        let path = "/v1/bots/abc/spend-limits";
        // The guard is role-independent: a Phone or Desktop peer cannot erase
        // the caps (all-null body) or raise one; PUT/POST/DELETE are all refused.
        for method in [Method::PUT, Method::POST, Method::DELETE, Method::PATCH] {
            assert!(host_local_denial(&method, path).is_some(), "{method}");
        }
        assert!(host_local_denial(&Method::GET, path).is_none());
        assert!(host_local_denial(&Method::PUT, "/v1/bots/abc").is_none());
    }

    #[test]
    fn peers_never_browse_preview_or_open_the_hosts_folders_and_files() {
        let paths = [
            "/v1/knowledge/folders",
            "/v1/bots/abc/knowledge/folder-preview",
            "/v1/bots/abc/knowledge/sources/s1/file",
        ];
        for path in paths {
            for method in [
                Method::GET,
                Method::HEAD,
                Method::POST,
                Method::PUT,
                Method::DELETE,
            ] {
                assert!(
                    host_local_denial(&method, path).is_some(),
                    "{method} {path}"
                );
                // Doubled slashes are collapsed before the check.
                let doubled = normalize_deny_path(&path.replace("/knowledge", "//knowledge"));
                assert!(
                    host_local_denial(&method, &doubled).is_some(),
                    "{method} {doubled}"
                );
            }
        }
        // Reading the list of sources stays an ordinary peer read.
        assert!(host_local_denial(&Method::GET, "/v1/bots/abc/knowledge/sources").is_none());
        assert!(host_local_denial(&Method::GET, "/v1/bots/abc/knowledge/sources/s1").is_none());
    }

    #[test]
    fn peers_never_change_a_bots_page_reader() {
        let path = "/v1/bots/abc/page-reader";
        for method in [Method::PUT, Method::POST, Method::DELETE, Method::PATCH] {
            assert!(host_local_denial(&method, path).is_some(), "{method}");
        }
        assert!(host_local_denial(&Method::GET, path).is_none());
    }

    #[test]
    fn only_phone_and_desktop_may_post_run_stop() {
        for (role, refused) in [
            (PeerRole::Phone, false),
            (PeerRole::Desktop, false),
            (PeerRole::Node, true),
            (PeerRole::Host, true),
        ] {
            assert_eq!(
                stop_refused(&Method::POST, PEER_RUN_STOP_PATH, role),
                refused,
                "{role:?}"
            );
        }
        // Other paths and methods are not this rule's business.
        assert!(!stop_refused(&Method::POST, "/v1/runs", PeerRole::Node));
        assert!(!stop_refused(
            &Method::GET,
            PEER_RUN_STOP_PATH,
            PeerRole::Node
        ));
    }
}
