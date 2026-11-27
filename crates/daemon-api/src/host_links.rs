//! `/v1/hosts…`: this computer's links to paired always-on hosts.
//!
//! The local daemon owns the iroh identity and the pairing store, so it is the
//! desktop app's client for a host: it can pair with a host ticket and open a
//! guarded loopback proxy (`remote-api`) to a paired host. The app then talks
//! to the HOST's API through that proxy, as a Desktop peer, subject to the
//! host's own peer guard. These routes are an action of the local user: the
//! peer router refuses `/v1/hosts…` for every paired role
//! (`peer::PEER_DENIED_PREFIX_HOSTS`).
//!
//! Daemon-api only defines the seam ([`HostLinks`]); the embedder (the CLI)
//! implements it, because `remote-api` depends on this crate and not the
//! other way round.
use axum::{
    Extension, Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{ApiError, AppState, routes::ApiJson};

/// Connection state of one paired host as seen from this computer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HostLinkState {
    /// Paired, no channel open (the app has not selected it).
    Idle,
    /// A channel is being opened or re-opened.
    Connecting,
    /// The channel is up and the host answered its health probe.
    Connected,
    /// The channel dropped; reconnecting. The host may be off.
    Offline,
    /// The host refused this computer (revoked or not paired on its side).
    Unreachable,
}

/// A paired host.
// One flag per thing the owner switches separately (wire shape).
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct HostLinkInfo {
    /// Paired device key (public, not a secret).
    pub id: String,
    /// Display name the host announced at pairing.
    pub name: String,
    pub state: HostLinkState,
    /// One short, path-free sentence for `offline` / `unreachable`.
    pub detail: Option<String>,
    /// The owner lets this host's bots use THIS computer (off by default).
    #[serde(default)]
    pub computer_access: bool,
    /// ... and may look at the screen without asking first each session.
    #[serde(default)]
    pub computer_access_always: bool,
    /// The owner lets this host's bots use this computer's browser.
    #[serde(default)]
    pub computer_browser: bool,
    /// ... run commands on it.
    #[serde(default)]
    pub computer_commands: bool,
    /// ... read its files.
    #[serde(default)]
    pub computer_files: bool,
}

/// `GET /v1/hosts`.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct HostLinkList {
    /// Version of this computer's daemon.
    pub local_version: String,
    /// HTTP API version of this computer's daemon.
    pub local_api_version: String,
    /// Release tag this app was built from (`v0.1.0`), when the build knows it.
    /// Names the version "Update server" installs and the tag in the copy-paste commands.
    #[serde(default)]
    pub local_release_tag: Option<String>,
    pub hosts: Vec<HostLinkInfo>,
}

/// `GET /v1/hosts/{id}/version`: what a paired server runs.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct HostVersion {
    pub version: String,
    /// Release tag the server was built from; absent for untagged builds and old servers.
    #[serde(default)]
    pub release_tag: Option<String>,
    /// The server's exact build id; empty for servers that predate it.
    #[serde(default)]
    pub build_id: String,
    pub api_version: String,
}

/// `POST /v1/hosts/pair`.
#[derive(Deserialize, ToSchema)]
pub struct PairHostInput {
    /// Pairing ticket printed by `bot host invite --role desktop` on the host.
    pub ticket: String,
}

/// An open channel: a loopback endpoint that forwards to the host's API over
/// the paired connection. `token` is a bearer for that proxy only, valid while
/// the channel lives; it is never logged.
#[derive(Serialize, ToSchema)]
pub struct HostLinkConnection {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub token: String,
    pub state: HostLinkState,
}

impl std::fmt::Debug for HostLinkConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostLinkConnection")
            .field("id", &self.id)
            .field("base_url", &self.base_url)
            .field("token", &"<redacted>")
            .finish_non_exhaustive()
    }
}

/// Why a host-link operation failed.
#[derive(Debug)]
pub enum HostLinkError {
    /// Unknown or not-a-host device.
    NotFound,
    /// The ticket or request is not acceptable (message is safe to show).
    Invalid(String),
    /// The transport protocol was rejected before a host API could be reached.
    ProtocolMismatch,
    /// The server runs a Praxi release this computer may not work with; only its status and the
    /// update request are allowed until the releases match.
    Incompatible,
    /// Pairing or dialing failed (message is safe to show).
    Failed(String),
}

/// The answer of a host to one request sent over its peer link.
#[derive(Debug, Clone)]
pub struct HostCall {
    pub status: u16,
    /// Parsed JSON body (`null` when the host sent none).
    pub body: serde_json::Value,
}

/// What `/v1/hosts` needs from the embedder.
#[async_trait::async_trait]
pub trait HostLinks: Send + Sync {
    /// Paired devices that are hosts, with their channel state. Never dials.
    fn list(&self) -> Vec<HostLinkInfo>;
    /// Pair with a host by ticket (starts the peer endpoint if needed).
    async fn pair(&self, ticket: &str) -> Result<HostLinkInfo, HostLinkError>;
    /// Open (or reuse) the channel to a paired host.
    async fn connect(&self, id: &str) -> Result<HostLinkConnection, HostLinkError>;
    /// Close the channel; the pairing stays. Returns whether one was open.
    fn disconnect(&self, id: &str) -> bool;
    /// Remove the pairing with a host from this computer (rollback of a
    /// pairing that did not finish). Returns whether a pairing was removed.
    async fn forget(&self, _id: &str) -> bool {
        false
    }
    /// Let (or stop letting) a paired host's bots use this computer.
    async fn set_computer_access(
        &self,
        _id: &str,
        _access: &crate::pc_access::ComputerAccessInput,
    ) -> Result<(), HostLinkError> {
        Err(HostLinkError::NotFound)
    }
    /// Requests of hosts to act on this computer, waiting for the owner, and
    /// the hosts looking at the screen right now.
    fn computer_requests(&self) -> crate::pc_access::PcRequestList {
        crate::pc_access::PcRequestList {
            requests: Vec::new(),
            reading: Vec::new(),
        }
    }
    /// Answer one request; `false` when it is gone.
    fn decide_computer_request(&self, _id: &str, _allow: bool) -> bool {
        false
    }
    /// Send one JSON request to a paired host through its channel, as this
    /// computer's Desktop peer. Used only by the owner-initiated "copy my
    /// setup": the host applies its own peer guard to it.
    async fn call(
        &self,
        _id: &str,
        _method: &str,
        _path: &str,
        _body: Option<serde_json::Value>,
    ) -> Result<HostCall, HostLinkError> {
        Err(HostLinkError::Failed(
            "this daemon cannot send requests to a host".into(),
        ))
    }
}

impl From<HostLinkError> for ApiError {
    fn from(e: HostLinkError) -> Self {
        match e {
            HostLinkError::NotFound => Self::not_found("host"),
            HostLinkError::Invalid(m) => Self::bad_request(m),
            HostLinkError::ProtocolMismatch => Self::conflict(
                "the pairing protocol differs; install the same Praxi release on the server and desktop, then create a fresh invite",
            ),
            HostLinkError::Incompatible => Self::conflict(
                "the server runs a different Praxi release; update it first, then try again",
            ),
            HostLinkError::Failed(m) => Self::new(StatusCode::BAD_GATEWAY, "host_link_failed", m),
        }
    }
}

fn links(s: &AppState) -> Result<std::sync::Arc<dyn HostLinks>, ApiError> {
    s.host_links()
        .ok_or_else(|| ApiError::unavailable("host links are not available in this daemon"))
}

/// A device key is public, but still bounded before it reaches the embedder.
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.chars().all(|c| c.is_ascii_alphanumeric())
}

fn audit(s: &AppState, action: &str, detail: &serde_json::Value) -> Result<(), ApiError> {
    s.storage()
        .append_audit(s.user_id(), None, "local-ui", action, detail)
        .map(|_| ())
        .map_err(ApiError::internal)
}

#[utoipa::path(get, operation_id = "hosts_list", path = "/v1/hosts", tag = "host", security(("bearer" = [])), responses((status = 200, body = HostLinkList)))]
pub(crate) async fn list(State(s): State<AppState>) -> Json<HostLinkList> {
    Json(HostLinkList {
        local_version: env!("CARGO_PKG_VERSION").into(),
        local_api_version: crate::API_VERSION.into(),
        local_release_tag: crate::release_tag().map(str::to_owned),
        hosts: s.host_links().map(|l| l.list()).unwrap_or_default(),
    })
}

#[utoipa::path(get, operation_id = "hosts_version", path = "/v1/hosts/{id}/version", tag = "host", security(("bearer" = [])), params(("id" = String, Path)), responses((status = 200, body = HostVersion), (status = 502, body = crate::error::ErrorBody)))]
pub(crate) async fn host_version(
    State(s): State<AppState>,
    peer: Option<Extension<crate::PeerIdentity>>,
    Path(id): Path<String>,
) -> Result<Json<HostVersion>, ApiError> {
    if peer.is_some() {
        return Err(ApiError::forbidden("host links are local only"));
    }
    if !valid_id(&id) {
        return Err(ApiError::bad_request("invalid host id"));
    }
    let call = links(&s)?.call(&id, "GET", "/v1/host", None).await?;
    let text = |k: &str| {
        call.body[k].as_str().map(|v| {
            v.chars()
                .filter(|c| !c.is_control())
                .take(64)
                .collect::<String>()
        })
    };
    match (call.status, text("version")) {
        (200, Some(version)) => Ok(Json(HostVersion {
            version,
            release_tag: text("release_tag").filter(|t| !t.is_empty()),
            build_id: text("build_id").unwrap_or_default(),
            api_version: text("api_version").unwrap_or_default(),
        })),
        _ => Err(ApiError::new(
            StatusCode::BAD_GATEWAY,
            "host_link_failed",
            "the server did not report its version",
        )),
    }
}

#[utoipa::path(post, operation_id = "hosts_pair", path = "/v1/hosts/pair", tag = "host", security(("bearer" = [])), request_body = PairHostInput, responses((status = 200, body = HostLinkInfo)))]
pub(crate) async fn pair(
    State(s): State<AppState>,
    peer: Option<Extension<crate::PeerIdentity>>,
    ApiJson(input): ApiJson<PairHostInput>,
) -> Result<Json<HostLinkInfo>, ApiError> {
    if peer.is_some() {
        return Err(ApiError::forbidden("host links are local only"));
    }
    let ticket = input.ticket.trim();
    if ticket.is_empty() || ticket.len() > 2048 {
        return Err(ApiError::bad_request("ticket is empty or too long"));
    }
    let links = links(&s)?;
    // The ticket is a one-time secret: it is never written to the audit log.
    audit(
        &s,
        "host_link.pair",
        &serde_json::json!({"phase":"requested"}),
    )?;
    let info = links.pair(ticket).await?;
    audit(
        &s,
        "host_link.pair",
        &serde_json::json!({"phase":"paired","host":info.id}),
    )?;
    Ok(Json(info))
}

#[utoipa::path(post, operation_id = "hosts_connect", path = "/v1/hosts/{id}/connect", tag = "host", security(("bearer" = [])), params(("id" = String, Path)), responses((status = 200, body = HostLinkConnection)))]
pub(crate) async fn connect(
    State(s): State<AppState>,
    peer: Option<Extension<crate::PeerIdentity>>,
    Path(id): Path<String>,
) -> Result<Json<HostLinkConnection>, ApiError> {
    if peer.is_some() {
        return Err(ApiError::forbidden("host links are local only"));
    }
    if !valid_id(&id) {
        return Err(ApiError::bad_request("invalid host id"));
    }
    let links = links(&s)?;
    audit(&s, "host_link.connect", &serde_json::json!({"host":id}))?;
    Ok(Json(links.connect(&id).await?))
}

#[utoipa::path(delete, operation_id = "hosts_forget", path = "/v1/hosts/{id}", tag = "host", security(("bearer" = [])), params(("id" = String, Path)), responses((status = 204), (status = 404, body = crate::error::ErrorBody)))]
pub(crate) async fn forget(
    State(s): State<AppState>,
    peer: Option<Extension<crate::PeerIdentity>>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    if peer.is_some() {
        return Err(ApiError::forbidden("host links are local only"));
    }
    if !valid_id(&id) {
        return Err(ApiError::bad_request("invalid host id"));
    }
    let links = links(&s)?;
    audit(&s, "host_link.forget", &serde_json::json!({"host":id}))?;
    if links.forget(&id).await {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("host"))
    }
}

#[utoipa::path(delete, operation_id = "hosts_disconnect", path = "/v1/hosts/{id}/connect", tag = "host", security(("bearer" = [])), params(("id" = String, Path)), responses((status = 204)))]
pub(crate) async fn disconnect(
    State(s): State<AppState>,
    peer: Option<Extension<crate::PeerIdentity>>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    if peer.is_some() {
        return Err(ApiError::forbidden("host links are local only"));
    }
    if !valid_id(&id) {
        return Err(ApiError::bad_request("invalid host id"));
    }
    let links = links(&s)?;
    audit(&s, "host_link.disconnect", &serde_json::json!({"host":id}))?;
    links.disconnect(&id);
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(get, operation_id = "hosts_computer_requests", path = "/v1/hosts/computer-requests", tag = "host", security(("bearer" = [])), responses((status = 200, body = crate::pc_access::PcRequestList)))]
pub(crate) async fn computer_requests(
    State(s): State<AppState>,
    peer: Option<Extension<crate::PeerIdentity>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<crate::pc_access::PcRequestList>, ApiError> {
    if peer.is_some() {
        return Err(ApiError::forbidden("host links are local only"));
    }
    s.require_owner(&headers)?;
    Ok(Json(s.host_links().map_or(
        crate::pc_access::PcRequestList {
            requests: Vec::new(),
            reading: Vec::new(),
        },
        |l| l.computer_requests(),
    )))
}

#[utoipa::path(post, operation_id = "hosts_decide_computer_request", path = "/v1/hosts/computer-requests/{rid}", tag = "host", security(("bearer" = [])), params(("rid" = String, Path)), request_body = crate::pc_access::PcRequestDecision, responses((status = 204), (status = 404, body = crate::error::ErrorBody)))]
pub(crate) async fn decide_computer_request(
    State(s): State<AppState>,
    peer: Option<Extension<crate::PeerIdentity>>,
    headers: axum::http::HeaderMap,
    Path(rid): Path<String>,
    ApiJson(input): ApiJson<crate::pc_access::PcRequestDecision>,
) -> Result<StatusCode, ApiError> {
    if peer.is_some() {
        return Err(ApiError::forbidden("host links are local only"));
    }
    s.require_owner(&headers)?;
    let links = links(&s)?;
    audit(
        &s,
        "host_link.computer_request",
        &serde_json::json!({"request":rid,"allow":input.allow}),
    )?;
    if links.decide_computer_request(&rid, input.allow) {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("request"))
    }
}

#[utoipa::path(put, operation_id = "hosts_set_computer_access", path = "/v1/hosts/{id}/computer-access", tag = "host", security(("bearer" = [])), params(("id" = String, Path)), request_body = crate::pc_access::ComputerAccessInput, responses((status = 204), (status = 404, body = crate::error::ErrorBody)))]
pub(crate) async fn set_computer_access(
    State(s): State<AppState>,
    peer: Option<Extension<crate::PeerIdentity>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
    ApiJson(input): ApiJson<crate::pc_access::ComputerAccessInput>,
) -> Result<StatusCode, ApiError> {
    if peer.is_some() {
        return Err(ApiError::forbidden("host links are local only"));
    }
    s.require_owner(&headers)?;
    if !valid_id(&id) {
        return Err(ApiError::bad_request("invalid host id"));
    }
    let links = links(&s)?;
    audit(
        &s,
        "host_link.computer_access",
        &serde_json::json!({"host":id,"enabled":input.enabled,"always_read":input.always_read,"browser":input.browser,"commands":input.commands,"files":input.files}),
    )?;
    links.set_computer_access(&id, &input).await?;
    Ok(StatusCode::NO_CONTENT)
}
