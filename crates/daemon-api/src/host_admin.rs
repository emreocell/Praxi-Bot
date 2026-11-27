//! Host administration: what the OWNER's paired desktop may do to this host.
//!
//! An always-on host starts with no model provider and no bots. The owner's
//! desktop can give it both over the paired peer link, so the host works like
//! the computer the owner already knows. The capability is explicit
//! (`Device::admin`, see `peer-network`), granted when the owner of the host
//! creates the desktop invite or later by the owner on the host itself
//! (`bot host admin`), and never held by phones, nodes or other hosts.
//!
//! Callers are identified by the transport only (`PeerIdentity`, inserted by
//! the iroh ingress). A bot, a tool, the MCP server facade, a CLI engine or a
//! scheduled run reaches the daemon without that extension and is refused.
//!
//! Secrets: an API key arrives inside the end-to-end encrypted peer link, is
//! written straight to the host's secret store and is never logged, echoed,
//! audited or kept in `providers.toml` (which only names the secret). The
//! audit rows carry the provider id and whether a key was set, nothing else.
//!
//! The administrator desktop IS the owner of the host: every owner setting
//! (execution policy, accounts and keys of mail, web search and dictation,
//! tool servers, skills, the mail channel and guard, fill secrets, spending
//! limits, bot engine and knowledge settings, audit and log reads) is open to
//! it, one principle in one place (`peer::guard`; the owner-gated handlers are
//! shown the host's own owner key for such a caller). Secrets in those
//! requests are write-only. Each change is audited as `host.admin.settings`
//! (class, method and path, never a body).
//!
//! Short, explicit never-list (`peer::admin_desktop_never`): minting or
//! changing administrators and the host's lifecycle (`/v1/host…`), the host's
//! own links to other computers (`/v1/hosts…`), MCP facade credentials,
//! diagnostics and repairs, chat-channel pairing and the signed webhook
//! ingress. The owner key itself never crosses the link.

use axum::{
    Extension, Json,
    extract::{DefaultBodyLimit, Path, State},
    http::StatusCode,
};
use provider_catalog::{ProviderChoice, ProviderConfig, api_key_secret_name, config_path};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{ApiError, AppState, PeerIdentity, PeerRole, host::HostMode, routes::ApiJson};

/// Administration calls per minute, all administrators together.
pub(crate) const MAX_ADMIN_CALLS_PER_MINUTE: usize = 60;
/// Largest administration request body.
pub(crate) const MAX_ADMIN_BODY: usize = 64 * 1024;
/// Longest accepted API key.
const MAX_KEY_BYTES: usize = 16 * 1024;

/// Stable codes the desktop turns into text.
pub mod codes {
    /// The caller is not a paired desktop (or the daemon is not a host).
    pub const FORBIDDEN: &str = "host_admin_forbidden";
    /// A desktop without the capability: the owner must confirm on the host.
    pub const REQUIRED: &str = "host_admin_required";
    /// The host has no secret store to put a key in.
    pub const NO_SECRETS: &str = "secrets_unavailable";
    /// The secret store refused the write.
    pub const SECRET_FAILED: &str = "secret_store_failed";
}

/// The command the owner runs on the host to confirm a desktop that was
/// paired before the capability existed.
pub const CONFIRM_COMMAND: &str = "bot host admin --all-desktops";
/// What to do on a host whose secret store cannot hold keys.
pub const SECRETS_HINT: &str = "Set BOT_SECRETS_BACKEND=file and a long random BOT_SECRETS_PASSPHRASE in the host's service environment (/etc/praxi/praxi.env), then restart the service.";

/// The paired desktop that is calling, once it holds the capability.
pub(crate) struct Admin {
    actor: String,
}

fn is_desktop_peer(peer: Option<&PeerIdentity>) -> Result<&PeerIdentity, ApiError> {
    match peer {
        Some(p) if p.role == PeerRole::Desktop => Ok(p),
        _ => Err(ApiError::new(
            StatusCode::FORBIDDEN,
            codes::FORBIDDEN,
            "host administration is only for the owner's paired desktop",
        )),
    }
}

fn require_host(s: &AppState) -> Result<(), ApiError> {
    if s.leader().config().mode == HostMode::Host {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::FORBIDDEN,
            codes::FORBIDDEN,
            "this daemon is not an always-on host",
        ))
    }
}

pub(crate) fn require_admin(s: &AppState, peer: Option<&PeerIdentity>) -> Result<Admin, ApiError> {
    let peer = is_desktop_peer(peer)?;
    require_host(s)?;
    if !s.leader().peer_is_admin(&peer.key) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            codes::REQUIRED,
            format!("the owner must confirm this desktop on the host: run `{CONFIRM_COMMAND}`"),
        ));
    }
    Ok(Admin {
        actor: format!("desktop-admin:{}", peer.key),
    })
}

/// The audit identity of the admin desktop with key `peer_key` (for work that
/// outlives the request that proved it, such as closing a terminal session).
pub(crate) fn admin_for(peer_key: &str) -> Admin {
    Admin {
        actor: format!("desktop-admin:{peer_key}"),
    }
}

pub(crate) fn audit(
    s: &AppState,
    admin: &Admin,
    action: &str,
    detail: &serde_json::Value,
) -> Result<(), ApiError> {
    s.storage()
        .append_audit(s.user_id(), None, &admin.actor, action, detail)
        .map(|_| ())
        .map_err(|_| ApiError::internal("cannot write the audit log"))
}

/// Rate admission that runs BEFORE the body is read or parsed, for a caller
/// that holds the capability (a refused caller cannot spend the owner's budget).
pub(crate) struct Gate;

impl axum::extract::FromRequestParts<AppState> for Gate {
    type Rejection = ApiError;
    fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        s: &AppState,
    ) -> impl std::future::Future<Output = Result<Self, ApiError>> + Send {
        let admitted = if s.is_host_admin(parts.extensions.get::<PeerIdentity>()) {
            s.leader().admit_admin_call()
        } else {
            Ok(())
        };
        std::future::ready(admitted.map(|()| Self))
    }
}

impl AppState {
    /// Whether `peer` is a paired desktop holding host administration on a
    /// daemon that runs as a host.
    pub(crate) fn is_host_admin(&self, peer: Option<&PeerIdentity>) -> bool {
        peer.is_some_and(|p| {
            p.role == PeerRole::Desktop
                && self.leader().config().mode == HostMode::Host
                && self.leader().peer_is_admin(&p.key)
        })
    }

    /// The peer as the bot-authority checks should see it: an administrator
    /// acts as the owner (`None`), every other peer stays restricted.
    pub(crate) fn bot_authority_peer<'a>(
        &self,
        peer: Option<&'a PeerIdentity>,
    ) -> Option<&'a PeerIdentity> {
        if self.is_host_admin(peer) { None } else { peer }
    }
}

/// `GET /v1/admin/state`.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AdminStateDto {
    /// This desktop holds host administration.
    pub granted: bool,
    /// Whether the host can store an API key.
    pub secrets_available: bool,
    /// The command the owner runs on the host when `granted` is false.
    pub confirm_command: Option<String>,
    /// What to do when `secrets_available` is false.
    pub secrets_hint: Option<String>,
    /// The owner made this host the bots' home (setup moved here).
    pub adopted: bool,
}

/// `PUT /v1/admin/home`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AdminHomeInput {
    pub adopted: bool,
}

const HOME_FILE: &str = "home-adopted.json";

/// Whether the owner adopted this host as the bots' home.
pub(crate) fn home_adopted(data: &std::path::Path) -> bool {
    std::fs::read(data.join(HOME_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .is_some_and(|v| v["adopted"] == true)
}

/// What the host reports about one CLI engine, as installed THERE.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AdminCliEngineDto {
    pub id: String,
    pub name: String,
    pub installed: bool,
    /// `None` when the CLI offers no safe way to check.
    pub signed_in: Option<bool>,
    /// Known credential file exists. Its contents and validity were not checked.
    #[serde(default)]
    pub credential_file_present: bool,
    pub version: Option<String>,
    /// Command that installs it on the host, when one is known.
    pub install_command: Option<String>,
    /// The host can run that command itself (a verified, fixed installer).
    pub can_install: bool,
    /// How to sign in on the host.
    pub login: String,
    /// How the app can drive that sign-in: `device_code`, `paste_code`, `browser` or `manual`.
    pub login_flow: String,
    /// The account the CLI says it is signed in as (an e-mail or user name), when it names one.
    pub account: Option<String>,
    pub docs_url: String,
    /// Where the CLI was found on the host (the program the app would run), when it is installed.
    #[serde(default)]
    pub path: Option<String>,
}

/// Who the host's coding CLIs run as, and where they live. Installs, sign-ins and checks all
/// happen as this account, whoever opened the terminal.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AdminCliEnvironmentDto {
    /// Account the service (and every CLI it starts) runs as; `null` when the system did not say.
    pub user: Option<String>,
    /// That account is the administrator: a CLI installed or signed in by anyone else is invisible.
    pub is_root: bool,
    /// Its home folder, where CLIs keep their installs and sign-in files.
    pub home: Option<String>,
    /// Folders searched for coding CLIs that exist right now, in order.
    pub search_dirs: Vec<String>,
    /// Start of a terminal command that runs one CLI step as that account
    /// (`sudo praxi cli`), for a person who is logged in over SSH as somebody else.
    pub terminal_command: String,
}

/// Add or update a provider. The key is write-only.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AdminProviderInput {
    /// Never echoed, logged or audited.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Only for a custom OpenAI-compatible endpoint.
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// Make this the host's default provider.
    #[serde(default)]
    pub make_default: bool,
}

impl Drop for AdminProviderInput {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.api_key);
    }
}

impl std::fmt::Debug for AdminProviderInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdminProviderInput")
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

/// The provider as stored on the host.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AdminProviderDto {
    pub id: String,
    pub has_key: bool,
    pub model: Option<String>,
    pub is_default: bool,
}

/// `PUT /v1/admin/default-provider`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AdminDefaultInput {
    pub id: String,
    #[serde(default)]
    pub model: Option<String>,
}

fn valid_provider_id(id: &str) -> bool {
    id.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric())
        && id.len() <= 64
        && id.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

fn valid_text(text: &str, max: usize) -> bool {
    !text.is_empty() && text.len() <= max && !text.chars().any(char::is_control)
}

#[utoipa::path(get, operation_id = "admin_state", path = "/v1/admin/state", tag = "admin", security(("bearer" = [])),
    responses((status = 200, body = AdminStateDto), (status = 403, body = crate::error::ErrorBody)))]
pub(crate) async fn state(
    State(s): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    _gate: Gate,
) -> Result<Json<AdminStateDto>, ApiError> {
    let peer = is_desktop_peer(peer.as_ref().map(|p| &p.0))?;
    require_host(&s)?;
    let granted = s.leader().peer_is_admin(&peer.key);
    let secrets_available = s.secret_store().is_some();
    Ok(Json(AdminStateDto {
        granted,
        secrets_available,
        confirm_command: (!granted).then(|| CONFIRM_COMMAND.to_owned()),
        secrets_hint: (!secrets_available).then(|| SECRETS_HINT.to_owned()),
        adopted: home_adopted(&s.data_dir()),
    }))
}

#[utoipa::path(put, operation_id = "admin_set_home", path = "/v1/admin/home", tag = "admin", security(("bearer" = [])),
    request_body = AdminHomeInput,
    responses((status = 204), (status = 403, body = crate::error::ErrorBody)))]
pub(crate) async fn set_home(
    State(s): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    _gate: Gate,
    ApiJson(input): ApiJson<AdminHomeInput>,
) -> Result<StatusCode, ApiError> {
    let admin = require_admin(&s, peer.as_ref().map(|p| &p.0))?;
    audit(
        &s,
        &admin,
        "host.admin.home_set",
        &serde_json::json!({"adopted": input.adopted}),
    )?;
    let data = s.data_dir();
    std::fs::write(
        data.join(HOME_FILE),
        serde_json::json!({"adopted": input.adopted, "at_ms": storage::now_ms()}).to_string(),
    )
    .map_err(ApiError::internal)?;
    Ok(StatusCode::NO_CONTENT)
}

fn provider_dto(cfg: &ProviderConfig, id: &str) -> Option<AdminProviderDto> {
    let p = cfg.get(id)?;
    Some(AdminProviderDto {
        id: p.id.clone(),
        has_key: p.key.is_some(),
        model: p.default_model.clone(),
        is_default: cfg.effective_default().is_some_and(|d| d.id == p.id),
    })
}

fn config_error(e: impl std::fmt::Display) -> ApiError {
    ApiError::internal(format!("providers.toml: {e}"))
}

#[utoipa::path(put, operation_id = "admin_put_provider", path = "/v1/admin/providers/{id}", tag = "admin", security(("bearer" = [])),
    params(("id" = String, Path)), request_body = AdminProviderInput,
    responses((status = 200, body = AdminProviderDto), (status = 403, body = crate::error::ErrorBody),
              (status = 400, body = crate::error::ErrorBody), (status = 503, body = crate::error::ErrorBody)))]
#[allow(clippy::too_many_lines)] // validate, store, config, reload, audit: one ordered write
pub(crate) async fn put_provider(
    State(s): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    _gate: Gate,
    Path(id): Path<String>,
    ApiJson(input): ApiJson<AdminProviderInput>,
) -> Result<Json<AdminProviderDto>, ApiError> {
    let admin = require_admin(&s, peer.as_ref().map(|p| &p.0))?;
    if !valid_provider_id(&id) {
        return Err(ApiError::bad_request("invalid provider id"));
    }
    let key = input.api_key.as_deref().map(str::trim);
    if let Some(k) = key
        && (!valid_text(k, MAX_KEY_BYTES))
    {
        return Err(ApiError::bad_request(
            "the API key is empty, too long or malformed",
        ));
    }
    if let Some(m) = &input.model
        && !valid_text(m, 200)
    {
        return Err(ApiError::bad_request("invalid model name"));
    }
    if let Some(u) = &input.base_url
        && !(valid_text(u, 512) && (u.starts_with("http://") || u.starts_with("https://")))
    {
        return Err(ApiError::bad_request("base_url must be an http(s) URL"));
    }
    let host = s
        .provider_host()
        .ok_or_else(|| ApiError::conflict("this daemon has no provider configuration"))?;
    let store = match key {
        Some(_) => Some(s.secret_store().ok_or_else(|| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                codes::NO_SECRETS,
                SECRETS_HINT,
            )
        })?),
        None => s.secret_store(),
    };
    let data = s.data_dir();
    let existing_custom = ProviderConfig::load(&config_path(&data))
        .map_err(config_error)?
        .get(&id)
        .and_then(|p| p.custom.clone());
    let choice = if host.catalog.get(&id).is_some() {
        ProviderChoice::Catalog(id.clone())
    } else if let Some(base_url) = input
        .base_url
        .clone()
        .or(existing_custom.map(|e| e.base_url))
    {
        ProviderChoice::Custom {
            id: id.clone(),
            base_url,
        }
    } else {
        return Err(ApiError::bad_request(
            "unknown provider; give base_url for a custom endpoint",
        ));
    };
    audit(
        &s,
        &admin,
        "host.admin.provider_set",
        &serde_json::json!({"provider": id, "key_set": key.is_some(), "phase": "requested"}),
    )?;
    let (catalog, make_default, model) = (
        host.catalog.clone(),
        input.make_default,
        input.model.clone(),
    );
    let (key, id2, data2) = (
        key.map(|k| zeroize::Zeroizing::new(k.to_owned())),
        id.clone(),
        data.clone(),
    );
    let result = tokio::task::spawn_blocking(move || {
        let store = store.ok_or_else(|| "no secret store".to_owned())?;
        provider_catalog::configure_provider(
            &catalog,
            store.as_ref(),
            &data2,
            &choice,
            key.as_deref().map(String::as_str),
            model.as_deref(),
            storage::now_ms(),
        )
        .map_err(|e| e.to_string())?;
        if make_default {
            let path = config_path(&data2);
            let _guard = ProviderConfig::lock(&path).map_err(|e| e.to_string())?;
            let mut cfg = ProviderConfig::load(&path).map_err(|e| e.to_string())?;
            cfg.set_default(&id2).map_err(|e| e.to_string())?;
            cfg.save(&path).map_err(|e| e.to_string())?;
        }
        Ok::<_, String>(())
    })
    .await
    .map_err(ApiError::internal)?;
    if result.is_err() {
        // The text of a store error is not forwarded: it must never carry a value.
        audit(
            &s,
            &admin,
            "host.admin.provider_set",
            &serde_json::json!({"provider": id, "phase": "failed"}),
        )?;
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            codes::SECRET_FAILED,
            "the host could not save the provider; check its secret store",
        ));
    }
    // A provider that does not load yet (for instance no key) is still saved.
    let _ = crate::routes::reload_from_host(&s).await;
    audit(
        &s,
        &admin,
        "host.admin.provider_set",
        &serde_json::json!({"provider": id, "phase": "done"}),
    )?;
    let cfg = ProviderConfig::load(&config_path(&data)).map_err(config_error)?;
    provider_dto(&cfg, &id)
        .map(Json)
        .ok_or_else(|| ApiError::internal("provider vanished after save"))
}

#[utoipa::path(delete, operation_id = "admin_delete_provider", path = "/v1/admin/providers/{id}", tag = "admin", security(("bearer" = [])),
    params(("id" = String, Path)),
    responses((status = 204), (status = 403, body = crate::error::ErrorBody), (status = 404, body = crate::error::ErrorBody)))]
pub(crate) async fn delete_provider(
    State(s): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    _gate: Gate,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let admin = require_admin(&s, peer.as_ref().map(|p| &p.0))?;
    if !valid_provider_id(&id) {
        return Err(ApiError::bad_request("invalid provider id"));
    }
    audit(
        &s,
        &admin,
        "host.admin.provider_remove",
        &serde_json::json!({"provider": id}),
    )?;
    let data = s.data_dir();
    let store = s.secret_store();
    let id2 = id.clone();
    let removed = tokio::task::spawn_blocking(move || {
        let path = config_path(&data);
        let _guard = ProviderConfig::lock(&path).map_err(config_error)?;
        let mut cfg = ProviderConfig::load(&path).map_err(config_error)?;
        let removed = cfg.remove(&id2);
        if removed {
            cfg.save(&path).map_err(config_error)?;
            if let Some(store) = store {
                let _ = store.delete(&api_key_secret_name(&id2));
            }
        }
        Ok::<_, ApiError>(removed)
    })
    .await
    .map_err(ApiError::internal)??;
    if !removed {
        return Err(ApiError::not_found("provider"));
    }
    let _ = crate::routes::reload_from_host(&s).await;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(put, operation_id = "admin_set_default_provider", path = "/v1/admin/default-provider", tag = "admin", security(("bearer" = [])),
    request_body = AdminDefaultInput,
    responses((status = 200, body = AdminProviderDto), (status = 403, body = crate::error::ErrorBody), (status = 404, body = crate::error::ErrorBody)))]
pub(crate) async fn set_default(
    State(s): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    _gate: Gate,
    ApiJson(input): ApiJson<AdminDefaultInput>,
) -> Result<Json<AdminProviderDto>, ApiError> {
    let admin = require_admin(&s, peer.as_ref().map(|p| &p.0))?;
    if !input.id.starts_with("cli:") && !valid_provider_id(&input.id) {
        return Err(ApiError::bad_request("invalid provider id"));
    }
    if let Some(m) = &input.model
        && !valid_text(m, 200)
    {
        return Err(ApiError::bad_request("invalid model name"));
    }
    audit(
        &s,
        &admin,
        "host.admin.default_set",
        &serde_json::json!({"provider": input.id, "model": input.model}),
    )?;
    let data = s.data_dir();
    let (id, model) = (input.id.clone(), input.model.clone());
    let data2 = data.clone();
    tokio::task::spawn_blocking(move || {
        let path = config_path(&data2);
        let _guard = ProviderConfig::lock(&path).map_err(config_error)?;
        let mut cfg = ProviderConfig::load(&path).map_err(config_error)?;
        if cfg.get(&id).is_none() {
            return Err(ApiError::not_found("provider"));
        }
        cfg.set_default(&id).map_err(config_error)?;
        if let Some(m) = &model {
            cfg.set_model(&id, m).map_err(config_error)?;
        }
        cfg.save(&path).map_err(config_error)
    })
    .await
    .map_err(ApiError::internal)??;
    let _ = crate::routes::reload_from_host(&s).await;
    let cfg = ProviderConfig::load(&config_path(&data)).map_err(config_error)?;
    provider_dto(&cfg, &input.id)
        .map(Json)
        .ok_or_else(|| ApiError::not_found("provider"))
}

#[utoipa::path(get, operation_id = "admin_cli_engines", path = "/v1/admin/cli-engines", tag = "admin", security(("bearer" = [])),
    responses((status = 200, body = Vec<AdminCliEngineDto>), (status = 403, body = crate::error::ErrorBody)))]
pub(crate) async fn cli_engines(
    State(s): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    _gate: Gate,
) -> Result<Json<Vec<AdminCliEngineDto>>, ApiError> {
    require_admin(&s, peer.as_ref().map(|p| &p.0))?;
    let detector = s.detector();
    let rows = detector.detect(false).await;
    let npm_ready = cli_provider::install::resolve_installer(detector.environment()).is_ok();
    Ok(Json(
        rows.into_iter()
            .filter_map(|row| {
                let spec = cli_provider::engine_spec(&row.id)?;
                Some(AdminCliEngineDto {
                    signed_in: match row.auth {
                        cli_provider::AuthState::SignedIn => Some(true),
                        cli_provider::AuthState::SignedOut => Some(false),
                        cli_provider::AuthState::Unknown => None,
                    },
                    credential_file_present: row.detail.as_deref() == Some("auth_metadata_only"),
                    install_command: spec
                        .install()
                        .map(cli_provider::registry::InstallCommand::display),
                    // Never offered when the host could not honour it: an npm engine needs npm or a
                    // machine Praxi can fetch Node.js for.
                    can_install: spec.install().is_some_and(|i| {
                        i.verified
                            && i.executable
                            && (i.program != "npm"
                                || npm_ready
                                || cli_provider::node_runtime::supported_target().is_some())
                    }),
                    login: spec.login.into(),
                    login_flow: crate::cli_login::flow_name(spec.id).into(),
                    account: row.account_label.clone(),
                    docs_url: spec.docs_url.into(),
                    path: row
                        .installed
                        .then(|| cli_provider::search::find_engine(spec, detector.environment()))
                        .flatten()
                        .map(|p| p.display().to_string()),
                    id: row.id,
                    name: row.display_name,
                    installed: row.installed,
                    version: row.version,
                })
            })
            .collect(),
    ))
}

#[utoipa::path(get, operation_id = "admin_cli_environment", path = "/v1/admin/cli-environment", tag = "admin", security(("bearer" = [])),
    responses((status = 200, body = AdminCliEnvironmentDto), (status = 403, body = crate::error::ErrorBody)))]
pub(crate) async fn cli_environment(
    State(s): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    _gate: Gate,
) -> Result<Json<AdminCliEnvironmentDto>, ApiError> {
    require_admin(&s, peer.as_ref().map(|p| &p.0))?;
    Ok(Json(cli_environment_of(s.detector().environment())))
}

fn cli_environment_of(env: &cli_provider::PathEnv) -> AdminCliEnvironmentDto {
    let who = cli_provider::identity::run_as(env);
    AdminCliEnvironmentDto {
        terminal_command: cli_provider::identity::terminal_command(&who),
        search_dirs: env
            .path_dirs
            .iter()
            .filter(|d| d.is_dir())
            .map(|d| d.display().to_string())
            .collect(),
        home: who.home.as_ref().map(|h| h.display().to_string()),
        is_root: who.is_root,
        user: who.user,
    }
}

/// Progress of an install the owner's desktop started on this host.
#[derive(Debug, Clone, Default)]
pub(crate) struct AdminInstallRun {
    pub state: &'static str,
    pub error: Option<&'static str>,
    pub output: String,
}

/// Where Node.js is downloaded from, and whether redirects must stay on https. Tests point it at a local server.
fn node_dist() -> (String, bool) {
    #[cfg(test)]
    if let Some(base) = NODE_DIST_TEST.lock().clone() {
        return (base, false);
    }
    (cli_provider::node_runtime::DIST_BASE.to_owned(), true)
}

#[cfg(test)]
pub(crate) static NODE_DIST_TEST: parking_lot::Mutex<Option<String>> =
    parking_lot::Mutex::new(None);

/// Bytes of installer output kept for the progress view.
const INSTALL_TAIL_BYTES: usize = 4096;

/// `GET/POST /v1/admin/cli-engines/{id}/install`: where the install stands.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AdminCliInstallDto {
    pub engine: String,
    /// `idle`, `installing`, `done` or `failed`.
    pub state: String,
    /// Stable code of the last failure (`exit`, `timeout`, `npm_missing`...).
    pub error: Option<String>,
    /// Last lines of installer output (untrusted text, bounded).
    pub output: String,
    /// Found on the host right now.
    pub installed: bool,
    /// Signed in, when the CLI can say.
    pub signed_in: Option<bool>,
}

/// Only fixed registry argv runs; the client names an engine id, nothing else.
fn installable(id: &str) -> Result<&'static cli_provider::registry::CliEngineSpec, ApiError> {
    let spec =
        cli_provider::engine_spec(id).ok_or_else(|| ApiError::not_found("unknown engine"))?;
    if spec.install().is_some_and(|i| i.verified && i.executable) {
        Ok(spec)
    } else {
        Err(ApiError::new(
            StatusCode::CONFLICT,
            "install_copy_only",
            "this engine has no verified installer: use the command or the official page",
        ))
    }
}

async fn install_dto(
    s: &AppState,
    spec: &cli_provider::registry::CliEngineSpec,
) -> AdminCliInstallDto {
    let run =
        s.0.cli_installs
            .admin
            .lock()
            .get(spec.id)
            .cloned()
            .unwrap_or_else(|| AdminInstallRun {
                state: "idle",
                ..AdminInstallRun::default()
            });
    let row = s
        .detector()
        .detect(false)
        .await
        .into_iter()
        .find(|r| r.id == spec.id);
    AdminCliInstallDto {
        engine: spec.id.into(),
        state: run.state.into(),
        error: run.error.map(str::to_owned),
        output: run.output,
        installed: row.as_ref().is_some_and(|r| r.installed),
        signed_in: row.and_then(|r| match r.auth {
            cli_provider::AuthState::SignedIn => Some(true),
            cli_provider::AuthState::SignedOut => Some(false),
            cli_provider::AuthState::Unknown => None,
        }),
    }
}

/// Install one CLI engine ON THE HOST with the registry's fixed command.
/// Admin desktop only; one install at a time; output bounded.
#[utoipa::path(post, operation_id = "admin_cli_engine_install", path = "/v1/admin/cli-engines/{id}/install", tag = "admin", security(("bearer" = [])),
    params(("id" = String, Path)),
    responses((status = 202, body = AdminCliInstallDto), (status = 403, body = crate::error::ErrorBody), (status = 409, body = crate::error::ErrorBody)))]
#[allow(clippy::too_many_lines)] // One admission, bootstrap and install sequence.
pub(crate) async fn install_cli_engine(
    State(s): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    _gate: Gate,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<AdminCliInstallDto>), ApiError> {
    let admin = require_admin(&s, peer.as_ref().map(|p| &p.0))?;
    let spec = installable(&id)?;
    let detector = s.detector();
    let needs_npm = spec.install().is_some_and(|i| i.program == "npm");
    let npm_present =
        !needs_npm || cli_provider::install::resolve_installer(detector.environment()).is_ok();
    // No npm: Praxi fetches its own Node.js below the data dir (no root) when this machine
    // is one it can honestly do that for; otherwise the owner is told what to do.
    let bootstrap = if npm_present {
        None
    } else {
        Some(cli_provider::node_runtime::supported_target().ok_or_else(|| {
            ApiError::new(
                StatusCode::CONFLICT,
                "prerequisite_missing",
                "Node.js (npm) is not installed on the server and Praxi cannot install it on this system; install Node.js 22 LTS by hand",
            )
        })?)
    };
    let permit = std::sync::Arc::clone(&s.0.cli_installs.running)
        .try_acquire_owned()
        .map_err(|_| ApiError::conflict("an install is already running"))?;
    audit(
        &s,
        &admin,
        "provider.cli_install.admin_started",
        &serde_json::json!({"engine": spec.id, "bootstraps_node": bootstrap.is_some()}),
    )?;
    s.0.cli_installs.admin.lock().insert(
        spec.id.to_owned(),
        AdminInstallRun {
            state: "installing",
            ..AdminInstallRun::default()
        },
    );
    let task = s.clone();
    tokio::spawn(async move {
        let _permit = permit;
        let shutdown = task.shutdown_token();
        let data = task.data_dir();
        let say = {
            let sink = task.clone();
            move |text: &str| {
                let mut book = sink.0.cli_installs.admin.lock();
                if let Some(run) = book.get_mut(spec.id) {
                    run.output.push_str(text);
                    run.output.push('\n');
                    if run.output.len() > INSTALL_TAIL_BYTES {
                        let mut cut = run.output.len() - INSTALL_TAIL_BYTES;
                        while !run.output.is_char_boundary(cut) {
                            cut += 1;
                        }
                        run.output.drain(..cut);
                    }
                }
            }
        };
        let mut error: Option<&'static str> = None;
        if let Some(target) = bootstrap {
            say("Installing Node.js (LTS) into the Praxi data directory...");
            let (base, https_only) = node_dist();
            let step = async {
                let version =
                    cli_provider::node_runtime::install_node(&base, target, &data, https_only)
                        .await?;
                cli_provider::node_runtime::verify_runs(&data, &version).await?;
                Ok::<String, &'static str>(version)
            };
            let result = tokio::select! {
                r = step => r,
                () = shutdown.cancelled() => Err("shutdown"),
            };
            let _ = audit(
                &task,
                &admin,
                "provider.node_runtime.admin_installed",
                &serde_json::json!({"engine": spec.id, "version": result.as_ref().ok(), "error": result.as_ref().err()}),
            );
            match result {
                Ok(version) => {
                    say(&format!("Node.js {version} installed."));
                    // Detection and installs now see the runtime on the search path.
                    task.set_detector(std::sync::Arc::new(cli_provider::Detector::new()));
                }
                Err(code) => error = Some(code),
            }
        }
        if error.is_none() {
            let detector = task.detector();
            let sink = say.clone();
            let work = cli_provider::install::run_install(
                spec.id,
                detector.environment(),
                &data,
                cli_provider::install::INSTALL_TIMEOUT,
                move |text| sink(&text),
            );
            let result = tokio::select! {
                r = work => r,
                () = shutdown.cancelled() => Err("shutdown"),
            };
            error = result.err();
        }
        let rows = task.detector().detect(true).await;
        if error.is_none() && !rows.iter().any(|row| row.id == spec.id && row.installed) {
            error = Some("not_detected");
        }
        if error.is_none() {
            // The entry point as the install flow left it: a later sign-in runs nothing else.
            crate::cli_login::record_install_pin(&task, spec.id).await;
        }
        if let Some(run) = task.0.cli_installs.admin.lock().get_mut(spec.id) {
            run.state = if error.is_none() { "done" } else { "failed" };
            run.error = error;
        }
        let _ = audit(
            &task,
            &admin,
            "provider.cli_install.admin_finished",
            &serde_json::json!({"engine": spec.id, "error": error}),
        );
    });
    Ok((StatusCode::ACCEPTED, Json(install_dto(&s, spec).await)))
}

/// Progress of the install above (poll while `state` is `installing`).
#[utoipa::path(get, operation_id = "admin_cli_engine_install_status", path = "/v1/admin/cli-engines/{id}/install", tag = "admin", security(("bearer" = [])),
    params(("id" = String, Path)),
    responses((status = 200, body = AdminCliInstallDto), (status = 403, body = crate::error::ErrorBody)))]
pub(crate) async fn cli_engine_install_status(
    State(s): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    _gate: Gate,
    Path(id): Path<String>,
) -> Result<Json<AdminCliInstallDto>, ApiError> {
    require_admin(&s, peer.as_ref().map(|p| &p.0))?;
    let spec =
        cli_provider::engine_spec(&id).ok_or_else(|| ApiError::not_found("unknown engine"))?;
    Ok(Json(install_dto(&s, spec).await))
}

#[utoipa::path(post, operation_id = "admin_host_update", path = "/v1/admin/host-update", tag = "admin", security(("bearer" = [])), request_body = crate::host::HostUpdateInput,
    responses((status = 202), (status = 403, body = crate::error::ErrorBody), (status = 409, body = crate::error::ErrorBody)))]
pub(crate) async fn update_host(
    State(s): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    _gate: Gate,
    crate::routes::ApiJson(input): crate::routes::ApiJson<crate::host::HostUpdateInput>,
) -> Result<StatusCode, ApiError> {
    require_admin(&s, peer.as_ref().map(|p| &p.0))?;
    crate::host::update(State(s), peer, Some(Json(input))).await
}

/// The administration routes with their own, small body limit.
pub(crate) fn routes() -> axum::Router<AppState> {
    use axum::routing::{get, post, put};
    axum::Router::new()
        .route("/v1/admin/state", get(state))
        .route("/v1/admin/host-update", post(update_host))
        .route("/v1/admin/cli-engines", get(cli_engines))
        .route("/v1/admin/cli-environment", get(cli_environment))
        .route(
            "/v1/admin/cli-engines/{id}/install",
            get(cli_engine_install_status).post(install_cli_engine),
        )
        .route(
            "/v1/admin/cli-engines/{id}/login",
            get(crate::cli_login::login_status)
                .post(crate::cli_login::start_login)
                .delete(crate::cli_login::cancel_login),
        )
        .route(
            "/v1/admin/cli-engines/{id}/login/code",
            post(crate::cli_login::login_code),
        )
        .route("/v1/admin/home", put(set_home))
        .route(
            "/v1/admin/import/schedules",
            post(crate::host_import::import_schedules),
        )
        .route(
            "/v1/admin/import/chats",
            post(crate::host_import::import_chats),
        )
        .route(
            "/v1/admin/import/bots",
            post(crate::host_import::import_bot),
        )
        .route(
            "/v1/admin/import/bots/check",
            post(crate::host_import::check_bots),
        )
        .route(
            "/v1/admin/import/schedules/activate",
            post(crate::host_import::activate_schedules),
        )
        .route(
            "/v1/admin/import/schedules/release",
            post(crate::host_import::release_schedules),
        )
        .route("/v1/admin/default-provider", put(set_default))
        .route(
            "/v1/admin/providers/{id}",
            put(put_provider).delete(delete_provider),
        )
        .layer(DefaultBodyLimit::max(MAX_ADMIN_BODY))
        .merge(crate::terminal::routes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_ids_are_plain_and_never_cli() {
        for ok in ["openai", "gemini-openai-compat", "my.local_1"] {
            assert!(valid_provider_id(ok), "{ok}");
        }
        for bad in ["", "cli:claude", "Open AI", "a/b", "..", &"x".repeat(65)] {
            assert!(!valid_provider_id(bad), "{bad}");
        }
    }
}
