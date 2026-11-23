//! `/v1/hosts/setup…`: the desktop app's "Add server" wizard.
//!
//! The wizard connects to the user's own server over SSH, installs or detects
//! Praxi there, creates the pairing invite ON the server and pairs this
//! computer, so the user never sees a ticket. The SSH work is the
//! `host-provision` engine (the same code as `bot host add`); this module is
//! only the HTTP seam: start a job, poll its progress, answer the first-use
//! host-key question, cancel.
//!
//! Local owner only: the routes live under `/v1/hosts`, which the peer router
//! refuses for every paired role, and each handler refuses a peer identity
//! again. Nothing here carries a secret: the private key is referenced by path
//! only (never read into a response), invites and tickets stay inside the
//! embedder, and step text is already sanitised by the engine.
use axum::{
    Extension, Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{ApiError, AppState, routes::ApiJson};

/// `POST /v1/hosts/setup`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct StartHostSetupInput {
    /// Host name or IP address of the server.
    pub host: String,
    /// SSH user (default `ubuntu`).
    #[serde(default)]
    pub user: Option<String>,
    /// SSH port (default 22).
    #[serde(default)]
    pub port: Option<u32>,
    /// Path of a private key file. Omit to use the SSH agent. Only the path is
    /// used: the file is never copied into Praxi's data or logs.
    #[serde(default)]
    pub key_path: Option<String>,
    /// Local name for the server (default: derived from the address).
    #[serde(default)]
    pub name: Option<String>,
}

/// `POST /v1/hosts/setup/update`: update the program on a server this computer
/// added with the wizard (its SSH details are stored here already).
#[derive(Debug, Deserialize, ToSchema)]
pub struct StartHostUpdateInput {
    /// Paired host id (`GET /v1/hosts`).
    pub host_id: String,
}

/// What a job does on the server.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HostJobKind {
    /// Add (or repair) a server.
    #[default]
    Setup,
    /// Update the program on a server that is already added.
    Update,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HostSetupState {
    Running,
    /// First contact: the user must confirm the host key fingerprint.
    AwaitingHostKey,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HostSetupStepStatus {
    Pass,
    Warn,
    Fail,
    Skip,
}

/// One finished step. Texts are English and technical (for "copy details");
/// the app shows its own localised sentence per `id`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct HostSetupStep {
    pub id: String,
    pub title: String,
    pub status: HostSetupStepStatus,
    pub detail: String,
    pub hint: Option<String>,
}

/// The server's SSH host key, shown on first use.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct HostKeyPrompt {
    pub host: String,
    pub port: u16,
    pub fingerprint: String,
}

/// A setup job as seen by the app (poll `GET /v1/hosts/setup/{job}`).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct HostSetupJob {
    pub id: String,
    /// Setup of a new server or update of an existing one.
    #[serde(default)]
    pub kind: HostJobKind,
    pub state: HostSetupState,
    /// `user@host:port` being set up.
    pub target: String,
    pub steps: Vec<HostSetupStep>,
    /// Free-text progress lines ("installing ...").
    pub notes: Vec<String>,
    pub host_key: Option<HostKeyPrompt>,
    /// Paired host id once pairing worked: select it in the computer list.
    pub host_id: Option<String>,
    pub host_name: Option<String>,
    /// Live-view targets on the host (`null`: not known / older host).
    pub view_targets: Option<u32>,
    /// False while Praxi has no published release: only a server that already
    /// runs Praxi can be added.
    pub install_available: bool,
}

/// `POST /v1/hosts/setup/{job}/host-key`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct HostKeyAnswer {
    pub accept: bool,
}

/// A private key file found in `~/.ssh` (path only).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SshKeyHint {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct SshKeyList {
    pub keys: Vec<SshKeyHint>,
}

#[derive(Debug)]
pub enum HostSetupError {
    NotFound,
    /// Not acceptable input (message is safe to show).
    Invalid(String),
    /// Another setup is running, or the job is not waiting for that answer.
    Conflict(String),
}

/// What `/v1/hosts/setup` needs from the embedder.
///
/// Every fallible method returns [`HostSetupError`]: `NotFound` for an
/// unknown job, `Invalid` for input the embedder refuses (safe to show) and
/// `Conflict` when another setup runs or the job is not in the right state.
#[allow(clippy::missing_errors_doc)] // one shared error contract, described above
pub trait HostSetup: Send + Sync {
    fn start(&self, input: StartHostSetupInput) -> Result<HostSetupJob, HostSetupError>;
    /// Update the program on an already added server (the SSH details the
    /// wizard stored; nothing new is persisted). `Invalid` when this computer
    /// has no stored SSH details for that server: the app then shows the
    /// copy-paste commands instead.
    fn start_update(&self, _input: StartHostUpdateInput) -> Result<HostSetupJob, HostSetupError> {
        Err(HostSetupError::Invalid(
            "updating a server is not available in this daemon".into(),
        ))
    }
    fn job(&self, id: &str) -> Option<HostSetupJob>;
    fn answer_host_key(&self, id: &str, accept: bool) -> Result<HostSetupJob, HostSetupError>;
    fn cancel(&self, id: &str) -> Result<HostSetupJob, HostSetupError>;
    fn ssh_keys(&self) -> Vec<SshKeyHint>;
}

impl From<HostSetupError> for ApiError {
    fn from(e: HostSetupError) -> Self {
        match e {
            HostSetupError::NotFound => Self::not_found("setup job"),
            HostSetupError::Invalid(m) => Self::bad_request(m),
            HostSetupError::Conflict(m) => {
                Self::new(StatusCode::CONFLICT, "host_setup_conflict", m)
            }
        }
    }
}

fn setup(s: &AppState) -> Result<std::sync::Arc<dyn HostSetup>, ApiError> {
    s.host_setup()
        .ok_or_else(|| ApiError::unavailable("server setup is not available in this daemon"))
}

fn local_only(peer: Option<&Extension<crate::PeerIdentity>>) -> Result<(), ApiError> {
    if peer.is_some() {
        Err(ApiError::forbidden("server setup is local only"))
    } else {
        Ok(())
    }
}

fn valid_job(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

fn audit(s: &AppState, action: &str, detail: &serde_json::Value) -> Result<(), ApiError> {
    s.storage()
        .append_audit(s.user_id(), None, "local-ui", action, detail)
        .map(|_| ())
        .map_err(ApiError::internal)
}

#[utoipa::path(post, operation_id = "hosts_setup_start", path = "/v1/hosts/setup", tag = "host", security(("bearer" = [])), request_body = StartHostSetupInput, responses((status = 200, body = HostSetupJob)))]
pub(crate) async fn start(
    State(s): State<AppState>,
    peer: Option<Extension<crate::PeerIdentity>>,
    ApiJson(input): ApiJson<StartHostSetupInput>,
) -> Result<Json<HostSetupJob>, ApiError> {
    local_only(peer.as_ref())?;
    let setup = setup(&s)?;
    // The key file path is not written to the audit log.
    audit(
        &s,
        "host_setup.start",
        &serde_json::json!({
            "host": input.host.chars().take(255).collect::<String>(),
            "auth": if input.key_path.is_some() { "key_file" } else { "agent" },
        }),
    )?;
    Ok(Json(setup.start(input)?))
}

#[utoipa::path(post, operation_id = "hosts_setup_update", path = "/v1/hosts/setup/update", tag = "host", security(("bearer" = [])), request_body = StartHostUpdateInput, responses((status = 200, body = HostSetupJob), (status = 400, body = crate::error::ErrorBody), (status = 409, body = crate::error::ErrorBody)))]
pub(crate) async fn start_update(
    State(s): State<AppState>,
    peer: Option<Extension<crate::PeerIdentity>>,
    headers: axum::http::HeaderMap,
    ApiJson(input): ApiJson<StartHostUpdateInput>,
) -> Result<Json<HostSetupJob>, ApiError> {
    local_only(peer.as_ref())?;
    s.require_owner(&headers)?;
    if input.host_id.is_empty()
        || input.host_id.len() > 128
        || !input.host_id.chars().all(|c| c.is_ascii_alphanumeric())
    {
        return Err(ApiError::bad_request("invalid host id"));
    }
    let setup = setup(&s)?;
    // Only the public device id is logged: no address, user name or key path.
    audit(
        &s,
        "host_update.start",
        &serde_json::json!({"host": input.host_id}),
    )?;
    Ok(Json(setup.start_update(input)?))
}

#[utoipa::path(get, operation_id = "hosts_setup_get", path = "/v1/hosts/setup/{job}", tag = "host", security(("bearer" = [])), params(("job" = String, Path)), responses((status = 200, body = HostSetupJob)))]
pub(crate) async fn get(
    State(s): State<AppState>,
    peer: Option<Extension<crate::PeerIdentity>>,
    Path(job): Path<String>,
) -> Result<Json<HostSetupJob>, ApiError> {
    local_only(peer.as_ref())?;
    if !valid_job(&job) {
        return Err(ApiError::bad_request("invalid job id"));
    }
    setup(&s)?
        .job(&job)
        .map(Json)
        .ok_or_else(|| HostSetupError::NotFound.into())
}

#[utoipa::path(post, operation_id = "hosts_setup_host_key", path = "/v1/hosts/setup/{job}/host-key", tag = "host", security(("bearer" = [])), params(("job" = String, Path)), request_body = HostKeyAnswer, responses((status = 200, body = HostSetupJob)))]
pub(crate) async fn host_key(
    State(s): State<AppState>,
    peer: Option<Extension<crate::PeerIdentity>>,
    Path(job): Path<String>,
    ApiJson(answer): ApiJson<HostKeyAnswer>,
) -> Result<Json<HostSetupJob>, ApiError> {
    local_only(peer.as_ref())?;
    if !valid_job(&job) {
        return Err(ApiError::bad_request("invalid job id"));
    }
    let setup = setup(&s)?;
    audit(
        &s,
        "host_setup.host_key",
        &serde_json::json!({"accepted": answer.accept}),
    )?;
    Ok(Json(setup.answer_host_key(&job, answer.accept)?))
}

#[utoipa::path(post, operation_id = "hosts_setup_cancel", path = "/v1/hosts/setup/{job}/cancel", tag = "host", security(("bearer" = [])), params(("job" = String, Path)), responses((status = 200, body = HostSetupJob)))]
pub(crate) async fn cancel(
    State(s): State<AppState>,
    peer: Option<Extension<crate::PeerIdentity>>,
    Path(job): Path<String>,
) -> Result<Json<HostSetupJob>, ApiError> {
    local_only(peer.as_ref())?;
    if !valid_job(&job) {
        return Err(ApiError::bad_request("invalid job id"));
    }
    let setup = setup(&s)?;
    audit(&s, "host_setup.cancel", &serde_json::json!({}))?;
    Ok(Json(setup.cancel(&job)?))
}

#[utoipa::path(get, operation_id = "hosts_setup_ssh_keys", path = "/v1/hosts/setup/ssh-keys", tag = "host", security(("bearer" = [])), responses((status = 200, body = SshKeyList)))]
pub(crate) async fn ssh_keys(
    State(s): State<AppState>,
    peer: Option<Extension<crate::PeerIdentity>>,
) -> Result<Json<SshKeyList>, ApiError> {
    local_only(peer.as_ref())?;
    Ok(Json(SshKeyList {
        keys: setup(&s)?.ssh_keys(),
    }))
}
