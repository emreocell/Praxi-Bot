//! MCP server management (`/v1/mcp/*`, `/v1/secrets/{name}`): the DTOs and the
//! [`McpControl`] trait the embedder implements.  The daemon owns no MCP state
//! itself; `bot serve` plugs in an implementation over `mcp-bridge` (see
//! `apps/cli/src/mcp_host.rs`).

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use utoipa::ToSchema;

/// Transport of an MCP server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpTransportKind {
    /// A child process speaking MCP over stdin / stdout.
    Stdio,
    /// A streamable-HTTP endpoint.
    Http,
}

/// Connection state of an MCP server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpStatus {
    /// Waiting for an explicit user sign-in; not a server failure.
    AuthRequired,
    /// Session is up.
    Connected,
    /// A (re)connect is in progress.
    Connecting,
    /// Not connected (disabled, or it reconnects lazily on the next call).
    Disconnected,
    /// The last connect attempt failed; see `error`.
    Error,
    /// Too many consecutive failures: its tools are hidden until
    /// `POST /v1/mcp/servers/{name}/reconnect`.
    Quarantined,
}

/// Class of the last connect error of an MCP server, so a UI can show a
/// localized "what to do" next to the raw `error` text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpErrorKind {
    /// The launcher program (`npx`, `uvx`, ...) is not installed or not on PATH.
    RuntimeMissing,
    /// The process could not be started for another reason.
    Spawn,
    /// The process exited or closed its pipes before the handshake finished
    /// (`stderr_tail` usually says why).
    Closed,
    /// No answer within the timeout.
    Timeout,
    /// The configuration or a referenced secret is invalid or missing.
    Config,
    /// A filesystem server needs a folder chosen by the user.
    NeedsFolder,
    /// Anything else.
    Other,
}

/// A configured MCP server (`GET /v1/mcp/servers`).  The full stored config,
/// except secret *values*: `secret_env` / `secret_headers` carry secret
/// **names** only. `enabled`, `trust_hints`, `public_content` and `local_only`
/// are independent switches.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct McpServerDto {
    /// Public OAuth identity and secret names only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth: Option<McpOAuthConfig>,
    /// Server name (the `<server>` of `mcp:<server>/<tool>`).
    pub name: String,
    /// Transport.
    pub transport: McpTransportKind,
    /// Executable (stdio).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Arguments (stdio).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    /// Endpoint (http).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Plain environment variables (stdio). Not secrets.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Working directory (stdio).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Plain request headers (http). Not secrets.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Glob allow list over server-side tool names; empty = all.
    #[serde(default)]
    pub tool_allow: Vec<String>,
    /// Glob exclude list over server-side tool names.
    #[serde(default)]
    pub tool_exclude: Vec<String>,
    /// Environment variable -> secret name (stdio).
    #[serde(default)]
    pub secret_env: BTreeMap<String, String>,
    /// HTTP header -> secret name (http).
    #[serde(default)]
    pub secret_headers: BTreeMap<String, String>,
    /// Disabled servers are kept but never connected.
    pub enabled: bool,
    /// Connection state.
    pub status: McpStatus,
    /// Most recent error, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Class of `error`, for a localized hint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<McpErrorKind>,
    /// Last lines the server wrote to stderr (secret-redacted, bounded) while
    /// it is in an error state; empty otherwise.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stderr_tail: Vec<String>,
    /// Number of tools currently registered.
    pub tool_count: u32,
    /// Permission category of every tool of the server.
    pub category: String,
    /// Whether the readOnlyHint annotations of the server are believed.
    pub trust_hints: bool,
    /// Tool results are public data. Default `false`: a result is a private read.
    #[serde(default)]
    pub public_content: bool,
    /// Tools cannot send data off the machine. Default `false`: every tool is egress.
    #[serde(default)]
    pub local_only: bool,
    /// The owner connected this server as a browser (Playwright and similar). Its in-page tools
    /// do not ask after a web page alone; they ask once the run read private data. Code outside
    /// the page and file moves always ask. The built-in Playwright preset counts as a browser
    /// without it (`is_browser`).
    #[serde(default)]
    pub browser: bool,
    /// Whether the daemon treats this server as a browser: `browser`, or the exact built-in
    /// Playwright preset.
    #[serde(default)]
    pub is_browser: bool,
    /// Bots that may use the server; `null` = all bots.
    pub bots: Option<Vec<String>>,
    /// Tools the server changed after the owner approved them. They are
    /// switched off (the model never sees them) until re-approved with
    /// `POST /v1/mcp/servers/{name}/approve-tool-changes`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_tools: Vec<McpToolChangeDto>,
}

/// One tool whose description or schema changed after approval.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct McpToolChangeDto {
    /// Tool name on the server.
    pub tool: String,
    /// The approved description.
    pub before: String,
    /// The description the server offers now (untrusted text: show it as text).
    pub after: String,
    /// The visible description text differs.
    pub description_changed: bool,
    /// The input schema differs.
    pub schema_changed: bool,
    /// Something not visible in the text differs (hidden characters, title,
    /// annotations).
    pub other_changed: bool,
    /// The tool is new on a server that already had approved tools (or the
    /// approvals could not be read); `before` is empty.
    #[serde(default)]
    pub new_tool: bool,
    /// Hash of the definition shown here; the approval must quote it.
    pub hash: String,
}

/// Body of `POST /v1/mcp/servers/{name}/approve-tool-changes`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct McpApproveChangesInput {
    /// The tool to approve.
    pub tool: String,
    /// `hash` of the changed definition the owner was shown. If the server
    /// offers something else now the request fails with `409
    /// mcp_tool_changed_again`.
    pub hash: String,
}

/// Detailed transport of an MCP server body (the `McpServerConfig` shape).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpTransportObject {
    /// Child process.
    Stdio {
        /// Executable.
        command: String,
        /// Arguments.
        #[serde(default)]
        args: Vec<String>,
        /// Plain (non-secret) environment variables.
        #[serde(default)]
        env: BTreeMap<String, String>,
        /// Working directory.
        #[serde(default)]
        cwd: Option<String>,
    },
    /// Streamable HTTP.
    Http {
        /// Endpoint URL.
        url: String,
        /// Plain (non-secret) request headers.
        #[serde(default)]
        headers: BTreeMap<String, String>,
    },
}

/// `transport` of an MCP server body: either the kind (`"stdio"` / `"http"`,
/// with `command` / `args` / `url` as sibling fields, the shape of
/// [`McpServerDto`]) or the nested `McpServerConfig` object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(untagged)]
pub enum McpTransportInput {
    /// `"stdio"` or `"http"`.
    Kind(McpTransportKind),
    /// `{ "type": "stdio", "command": ... }`.
    Object(McpTransportObject),
}

/// Body of `POST /v1/mcp/servers`: the `mcp-bridge` server config.  Secrets go
/// in `secret_headers` / `secret_env` as secret **names** (store the value with
/// `POST /v1/secrets/{name}`). The boolean switches are independent.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct McpServerInput {
    /// HTTP OAuth identity; client secrets are secret-store references.
    #[serde(default)]
    pub oauth: Option<McpOAuthConfig>,
    /// Server name; `id` is accepted as an alias.  Required on POST; on PUT the
    /// path wins (a different name in the body is rejected).
    #[serde(default, alias = "id")]
    pub name: Option<String>,
    /// Transport (see [`McpTransportInput`]).
    pub transport: McpTransportInput,
    /// Executable (stdio, with `transport: "stdio"`).
    #[serde(default)]
    pub command: Option<String>,
    /// Arguments (stdio, with `transport: "stdio"`).
    #[serde(default)]
    pub args: Option<Vec<String>>,
    /// Endpoint (http, with `transport: "http"`).
    #[serde(default)]
    pub url: Option<String>,
    /// Plain environment variables (stdio, with `transport: "stdio"`).
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Working directory (stdio, with `transport: "stdio"`).
    #[serde(default)]
    pub cwd: Option<String>,
    /// Plain request headers (http, with `transport: "http"`).
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Default `true`.
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Glob allow list over server-side tool names; empty = all.
    #[serde(default)]
    pub tool_allow: Vec<String>,
    /// Glob exclude list over server-side tool names.
    #[serde(default)]
    pub tool_exclude: Vec<String>,
    /// Believe the readOnlyHint annotations of the server.  Default `false`.
    #[serde(default)]
    pub trust_hints: bool,
    /// Tool results are public data. Default `false` (a result is a private read).
    #[serde(default)]
    pub public_content: bool,
    /// Tools cannot send data off the machine. Default `false` (every tool is egress).
    #[serde(default)]
    pub local_only: bool,
    /// The owner connects this server as a browser (see `McpServerDto::browser`). Default `false`.
    #[serde(default)]
    pub browser: bool,
    /// Permission category of the tools; default `accounts`; `meta` is rejected.
    #[serde(default)]
    pub category: Option<String>,
    /// HTTP header -> secret name.
    #[serde(default)]
    pub secret_headers: BTreeMap<String, String>,
    /// Environment variable -> secret name.
    #[serde(default)]
    pub secret_env: BTreeMap<String, String>,
    /// Bots that may use the server; absent / `null` = all bots.
    #[serde(default)]
    pub bots: Option<Vec<String>>,
}

/// Body of `PUT /v1/mcp/servers/{name}`.  Every field is optional: a field left
/// out keeps the stored value.  `bots: null` means every bot.  The handler
/// reads the raw JSON so an omitted field is not the same as `null`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ToSchema)]
pub struct McpServerPatch {
    /// HTTP OAuth identity; null clears it.
    #[serde(default)]
    pub oauth: Option<McpOAuthConfig>,
    /// Server name; `id` is accepted as an alias.  The path wins (a different
    /// name in the body is rejected).
    #[serde(default, alias = "id")]
    pub name: Option<String>,
    /// Transport (see [`McpTransportInput`]).  Omit to keep the stored transport.
    #[serde(default)]
    pub transport: Option<McpTransportInput>,
    /// Executable (stdio).
    #[serde(default)]
    pub command: Option<String>,
    /// Arguments (stdio).
    #[serde(default)]
    pub args: Option<Vec<String>>,
    /// Endpoint (http).
    #[serde(default)]
    pub url: Option<String>,
    /// Plain environment variables (stdio).  Present object replaces the map.
    #[serde(default)]
    pub env: Option<BTreeMap<String, String>>,
    /// Working directory (stdio).  `null` clears it.
    #[serde(default)]
    #[schema(nullable = true)]
    pub cwd: Option<String>,
    /// Plain request headers (http).  Present object replaces the map.
    #[serde(default)]
    pub headers: Option<BTreeMap<String, String>>,
    /// Disabled servers are kept but never connected.
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Glob allow list over server-side tool names; empty = all.
    #[serde(default)]
    pub tool_allow: Option<Vec<String>>,
    /// Glob exclude list over server-side tool names.
    #[serde(default)]
    pub tool_exclude: Option<Vec<String>>,
    /// Believe the readOnlyHint annotations of the server.
    #[serde(default)]
    pub trust_hints: Option<bool>,
    /// Tool results are public data. Omit to keep the stored value.
    #[serde(default)]
    pub public_content: Option<bool>,
    /// Tools cannot send data off the machine. Omit to keep the stored value.
    #[serde(default)]
    pub local_only: Option<bool>,
    /// The owner connects this server as a browser. Omit to keep the stored value.
    #[serde(default)]
    pub browser: Option<bool>,
    /// Permission category of the tools; `null` clears it; `meta` is rejected.
    #[serde(default)]
    #[schema(nullable = true)]
    pub category: Option<String>,
    /// HTTP header -> secret name.  Present object replaces the map.
    #[serde(default)]
    pub secret_headers: Option<BTreeMap<String, String>>,
    /// Environment variable -> secret name.  Present object replaces the map.
    #[serde(default)]
    pub secret_env: Option<BTreeMap<String, String>>,
    /// Bots that may use the server.  Omit to keep the list; `null` = all bots.
    #[serde(default)]
    #[schema(nullable = true)]
    pub bots: Option<Vec<String>>,
    /// Bot ids to add to `bots`, applied atomically on the stored list (no
    /// read-modify-write by the client).  No effect while the server is open to
    /// every bot (`bots: null`).  Applied after `bots`.
    #[serde(default)]
    pub bots_add: Option<Vec<String>>,
    /// Bot ids to remove from `bots`, applied atomically; an emptied list stays
    /// `[]` (no bot), it never becomes `null`.  Applied after `bots_add`.
    #[serde(default)]
    pub bots_remove: Option<Vec<String>>,
}

/// A tool of an MCP server (`GET /v1/mcp/servers/{name}/tools`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct McpToolDto {
    /// Qualified tool name (`mcp:<server>/<tool>`).
    pub name: String,
    /// Description given by the server.
    pub description: String,
    /// `read`, `write` or `destructive` (`destructive` unless the server is trusted).
    pub effect: String,
}

/// A secret the user must provide to install a catalog entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct McpRequiredSecretDto {
    /// Secret name (store it with `POST /v1/secrets/{name}`).
    pub name: String,
    /// Label for the input field.
    pub label: String,
    /// Where the user can get the value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help_url: Option<String>,
}

/// A folder the user must choose when adding a catalog entry; the app appends
/// it as the last argument.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct McpFolderDto {
    /// Label of the folder field.
    pub label: String,
    /// A safe default offered to the user (their Documents folder), if there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
}

/// How to install a catalog entry: send it (plus `name` and `category`) to
/// `POST /v1/mcp/servers` after storing the `required_secrets`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct McpCatalogInstallDto {
    /// `stdio` or `http`.
    pub transport: McpTransportKind,
    /// Executable (stdio).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Arguments (stdio).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    /// Endpoint (http).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Non-secret environment variables (stdio).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Environment variable -> secret name (stdio).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub secret_env: BTreeMap<String, String>,
    /// Header -> secret name (http).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub secret_headers: BTreeMap<String, String>,
    /// Secrets to collect first.
    pub required_secrets: Vec<McpRequiredSecretDto>,
    /// A folder to collect first (filesystem servers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<McpFolderDto>,
    /// Sign-in needs a client the owner creates themselves (Google).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth_client: Option<McpCatalogOAuthClientDto>,
    /// What the "Test" button runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test: Option<McpCatalogTestDto>,
    /// Tools to let through (glob over the server-side name); empty = all.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_allow: Vec<String>,
}

/// A sign-in client the owner creates themselves with the authorization server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct McpCatalogOAuthClientDto {
    /// The authorization server the client is registered with.
    pub issuer: String,
    /// The read-only scopes the sign-in asks for.
    pub scopes: Vec<String>,
    /// Ask for a refresh token too.
    pub offline_access: bool,
    /// Which guided setup walks through creating the client (`google`).
    pub setup: String,
}

/// The fixed call of an entry's "Test" button.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct McpCatalogTestDto {
    /// Server-side tool name.
    pub tool: String,
    /// Fixed arguments.
    pub arguments: serde_json::Value,
}

/// An entry of the curated MCP catalog (`GET /v1/mcp/catalog`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct McpCatalogEntryDto {
    /// Stable id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// One-line description.
    pub description: String,
    /// Permission category of its tools.
    pub category: String,
    /// Project page.
    pub homepage: String,
    /// simple-icons slug, or `null`.
    pub icon: Option<String>,
    /// How to install it.
    pub install: McpCatalogInstallDto,
}

/// Body of `POST /v1/secrets/{name}`.
#[derive(Deserialize, ToSchema)]
pub struct SetSecret {
    /// The secret value.  Write-only: no route returns it.
    pub value: String,
}

impl std::fmt::Debug for SetSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SetSecret(<redacted>)")
    }
}

/// Data of the `mcp_server_changed` SSE event of the daemon-wide stream: a
/// server was added, changed, removed, or its status moved.  A notification
/// only (refetch `/v1/mcp/servers`); no SSE `id`, not replayed, not an envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct McpServerChanged {
    /// Always `mcp_server_changed`.
    #[serde(rename = "type")]
    pub kind_tag: String,
    /// Server name.
    pub name: String,
    /// New status; `disconnected` for a removed server.
    pub status: McpStatus,
}

impl McpServerChanged {
    /// The notification for `name`.
    #[must_use]
    pub fn new(name: impl Into<String>, status: McpStatus) -> Self {
        Self {
            kind_tag: "mcp_server_changed".to_owned(),
            name: name.into(),
            status,
        }
    }
}

/// Why an [`McpControl`] operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpControlError {
    /// No such server (`404`).
    NotFound(String),
    /// The name is taken (`409`).
    Conflict(String),
    /// The config is invalid (`422`).
    Invalid(String),
    /// The config could not be saved, or another internal failure (`500`).
    Internal(String),
    /// The tool definition changed again since the owner was shown it (`409`,
    /// `mcp_tool_changed_again`).
    ToolChangedAgain(String),
}

/// OAuth configuration contains no credential values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct McpOAuthConfig {
    pub client_id: Option<String>,
    pub client_secret_ref: Option<String>,
    pub client_metadata_url: Option<String>,
    #[serde(default)]
    pub allow_loopback_http: bool,
    /// The authorization server a pre-registered client is bound to;
    /// required with `client_id` / `client_secret_ref`.
    #[serde(default)]
    pub issuer: Option<String>,
    /// Scopes the sign-in asks for (empty = what the server advertises).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
    /// Also ask for a refresh token (Google needs it to stay signed in).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub offline_access: bool,
}

/// Authentication state, independent of transport errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpAuthState {
    SignedOut,
    SigningIn,
    SignedIn,
    AuthRequired,
}

/// `POST /v1/mcp/servers/{name}/auth/logout` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct McpAuthLogoutDto {
    pub state: McpAuthState,
    /// The provider confirmed revocation. When `false`, sign-out here succeeded
    /// but the owner may need to revoke access at the provider manually.
    pub remote_revoked: bool,
}

/// Body of `POST /v1/mcp/servers/{name}/auth/callback`.
#[derive(Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct McpAuthCallback {
    /// The full address the browser was redirected to (`http://127.0.0.1:<port>/callback?...`).
    pub redirect_url: String,
}

/// Browser destination for a user-started, five-minute sign-in.
#[derive(Clone, Serialize, Deserialize, ToSchema)]
pub struct McpAuthStart {
    pub authorization_url: String,
    pub expires_in: u64,
    /// Where the authorization server sends the browser back to.
    pub redirect_uri: String,
    /// `true`: nothing listens on this daemon for the redirect; the app that
    /// caught it on the owner's computer has to forward it to
    /// `POST /v1/mcp/servers/{name}/auth/callback`.
    #[serde(default)]
    pub relayed: bool,
}

/// Optional body of `POST /v1/mcp/servers/{name}/auth/start`.
#[derive(Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct McpAuthStartInput {
    /// The browser is on another computer (the owner's), which listens on this
    /// `127.0.0.1` port for the redirect and forwards it. Omit it when the
    /// browser runs where this daemon runs. Ports below 1024 are refused.
    #[serde(default)]
    pub relay_port: Option<u16>,
}

/// What the owner's "Test" button saw: a few items from one fixed read-only
/// call. Shown to the owner only; never given to a model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct McpTestResult {
    /// The server-side tool that was called.
    pub tool: String,
    /// At most three short, single-line items.
    pub items: Vec<String>,
    /// The answer held more than the items shown.
    pub more: bool,
    /// When no list could be recognised: the start of the answer, cut short.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// Most items a test result shows.
pub const TEST_ITEMS: usize = 3;
/// Longest item or text of a test result, in characters.
const TEST_CHARS: usize = 160;
const TEST_TEXT_CHARS: usize = 600;

fn one_line(text: &str, max: usize) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > max {
        let cut: String = flat.chars().take(max).collect();
        format!("{cut}…")
    } else {
        flat
    }
}

/// Keys that most often hold what a person would call the item's name.
const LABEL_KEYS: &[&str] = &[
    "subject",
    "title",
    "name",
    "summary",
    "displayName",
    "snippet",
    "sender",
    "id",
];

fn label_of(value: &serde_json::Value, depth: usize) -> Option<String> {
    match value {
        serde_json::Value::String(s) if !s.trim().is_empty() => Some(one_line(s, TEST_CHARS)),
        serde_json::Value::Object(map) if depth < 3 => LABEL_KEYS
            .iter()
            .find_map(|k| map.get(*k).and_then(|v| label_of(v, depth + 1)))
            .or_else(|| map.values().find_map(|v| label_of(v, depth + 1))),
        serde_json::Value::Array(items) if depth < 3 => {
            items.iter().find_map(|v| label_of(v, depth + 1))
        }
        _ => None,
    }
}

/// The first array in `value`, shallowest first (a list of files, events,
/// threads, whatever the tool calls it).
fn first_list(value: &serde_json::Value) -> Option<&Vec<serde_json::Value>> {
    let mut level = vec![value];
    for _ in 0..4 {
        let mut next = Vec::new();
        for v in level {
            match v {
                serde_json::Value::Array(a) => return Some(a),
                serde_json::Value::Object(m) => next.extend(m.values()),
                _ => {}
            }
        }
        level = next;
    }
    None
}

/// Turn what a test call returned into at most [`TEST_ITEMS`] short lines,
/// whatever the server calls its fields. Without a recognisable list the
/// answer's start is shown instead.
#[must_use]
pub fn summarize_test_output(tool: &str, text: &str) -> McpTestResult {
    let parsed = serde_json::from_str::<serde_json::Value>(text.trim()).ok();
    if let Some(list) = parsed.as_ref().and_then(first_list) {
        let items: Vec<String> = list
            .iter()
            .filter_map(|v| label_of(v, 0))
            .take(TEST_ITEMS)
            .collect();
        if !items.is_empty() || list.is_empty() {
            return McpTestResult {
                tool: tool.to_owned(),
                more: list.len() > TEST_ITEMS,
                items,
                text: None,
            };
        }
    }
    let shown = one_line(text, TEST_TEXT_CHARS);
    McpTestResult {
        tool: tool.to_owned(),
        items: Vec::new(),
        more: false,
        text: (!shown.is_empty()).then_some(shown),
    }
}

/// What `/v1/mcp/*` needs from the embedder: the managed MCP servers, their
/// persistence (`<data dir>/mcp.toml`, written atomically) and the catalog.
#[async_trait]
pub trait McpControl: Send + Sync {
    /// Install the exact bundle approved by the owner and leave it disabled.
    ///
    /// # Errors
    /// Invalid approval, bundle or settings, or a persistence failure.
    async fn install_bundle(
        self: std::sync::Arc<Self>,
        _bundle: Vec<u8>,
        _input: crate::mcp_import::BundleApproval,
    ) -> Result<McpServerDto, McpControlError> {
        Err(McpControlError::Invalid(
            "MCP bundle installs are unavailable".into(),
        ))
    }

    /// Install a registry source revalidated against the owner's approval.
    /// # Errors
    /// Invalid settings, a name conflict, or a persistence failure.
    async fn install_registry(
        self: std::sync::Arc<Self>,
        _entry: mcp_bridge::registry::RegistryEntry,
        _source: usize,
        _values: crate::mcp_import::ImportValues,
    ) -> Result<McpServerDto, McpControlError> {
        Err(McpControlError::Invalid(
            "MCP registry installs are unavailable".into(),
        ))
    }
    /// Start sign-in only for an explicit user action. Never exposed as a tool.
    /// # Errors
    /// Unknown server, unsupported transport or discovery/policy failure.
    async fn auth_start(
        &self,
        _name: &str,
        _relay_port: Option<u16>,
    ) -> Result<McpAuthStart, McpControlError> {
        Err(McpControlError::Invalid("OAuth is unavailable".into()))
    }
    /// The owner's "Test" button: one fixed read-only call named by the
    /// catalog entry, summarised to a few lines. Never a bot call.
    /// # Errors
    /// Unknown server, no test for this app, not connected, or the call failed.
    async fn test_call(&self, _name: &str) -> Result<McpTestResult, McpControlError> {
        Err(McpControlError::Invalid("testing is unavailable".into()))
    }
    /// Finish a pending sign-in with the redirect URL the user pasted (a host
    /// without a browser); same `state` check and one-shot rule as the
    /// loopback redirect.
    /// # Errors
    /// No sign-in waiting, a wrong redirect, or a refused exchange.
    async fn auth_callback(
        &self,
        _name: &str,
        _redirect_url: &str,
    ) -> Result<McpAuthState, McpControlError> {
        Err(McpControlError::Invalid("OAuth is unavailable".into()))
    }
    /// Cancel and join the pending flow before returning.
    /// # Errors
    /// Unknown server or unsupported transport.
    async fn auth_cancel(&self, _name: &str) -> Result<McpAuthState, McpControlError> {
        Err(McpControlError::Invalid("OAuth is unavailable".into()))
    }
    /// Read state without returning credentials or contacting an authorization server.
    /// # Errors
    /// Unknown server or secret-store failure.
    async fn auth_status(&self, _name: &str) -> Result<McpAuthState, McpControlError> {
        Err(McpControlError::Invalid("OAuth is unavailable".into()))
    }
    /// Cancel pending sign-in, delete credentials and disconnect.
    /// # Errors
    /// Unknown server or secret-store failure.
    async fn auth_logout(&self, _name: &str) -> Result<McpAuthLogoutDto, McpControlError> {
        Err(McpControlError::Invalid("OAuth is unavailable".into()))
    }
    /// Mask secrets in `text` with the daemon's production redactor before it
    /// goes into an audit row.  The default leaves text unchanged.
    fn redact(&self, text: &str) -> String {
        text.to_owned()
    }
    /// All servers, ordered by name.
    fn list(&self) -> Vec<McpServerDto>;
    /// Add and (if enabled) connect a server, then persist.
    ///
    /// # Errors
    /// [`McpControlError`].
    async fn add(&self, input: McpServerInput) -> Result<McpServerDto, McpControlError>;
    /// Merge `patch` onto the stored config of server `name` (omitted fields
    /// keep their previous values; `bots: null` means all bots), reconnect,
    /// then persist.
    ///
    /// # Errors
    /// [`McpControlError`].
    async fn update(
        &self,
        name: &str,
        patch: serde_json::Value,
    ) -> Result<McpServerDto, McpControlError>;
    /// Disconnect and forget a server, then persist.
    ///
    /// # Errors
    /// [`McpControlError`].
    async fn remove(&self, name: &str) -> Result<(), McpControlError>;
    /// [`Self::remove`], then also delete the OAuth client secret the server
    /// pointed at, unless another server still uses that secret. The default
    /// (a host without a secret store) only removes the server.
    ///
    /// # Errors
    /// [`McpControlError`] from the removal; a secret that cannot be deleted
    /// does not undo it.
    async fn remove_forgetting_client_secret(&self, name: &str) -> Result<(), McpControlError> {
        self.remove(name).await
    }
    /// Lift a quarantine / retry the connection.  A connect failure is not an
    /// error: it shows as `status: error` in the returned server.
    ///
    /// # Errors
    /// [`McpControlError`].
    async fn reconnect(&self, name: &str) -> Result<McpServerDto, McpControlError>;
    /// Approve the definition of `tool` with `hash` (the one shown to the
    /// owner) and switch the tool on.
    ///
    /// # Errors
    /// [`McpControlError`].
    async fn approve_tool_changes(
        &self,
        _name: &str,
        _tool: &str,
        _hash: &str,
    ) -> Result<McpServerDto, McpControlError> {
        Err(McpControlError::Internal(
            "tool approval is not supported by this host".into(),
        ))
    }
    /// Remove a deleted bot's id from every server's `bots` list (an atomic
    /// remove-id, never a list written back from a snapshot).  A list that
    /// becomes empty stays an explicit empty list ("no bot"); it never turns
    /// into `null` ("all bots").  Returns the number of servers changed.
    ///
    /// # Errors
    /// The first [`McpControlError`] of an update (the remaining servers are
    /// still tried).
    async fn forget_bot(&self, bot_id: &str) -> Result<usize, McpControlError> {
        // Each server is updated with the atomic `bots_remove` (the embedder
        // applies it under its own lock), not with a list read from a snapshot.
        let mut changed = 0;
        let mut first_err = None;
        for server in self.list() {
            if server.bots.is_none() {
                continue;
            }
            match self
                .update(&server.name, serde_json::json!({ "bots_remove": [bot_id] }))
                .await
            {
                Ok(_) => changed += 1,
                Err(e) => {
                    first_err.get_or_insert(e);
                }
            }
        }
        first_err.map_or(Ok(changed), Err)
    }
    /// The tools a server currently exposes.
    ///
    /// # Errors
    /// [`McpControlError::NotFound`].
    fn tools(&self, name: &str) -> Result<Vec<McpToolDto>, McpControlError>;
    /// The curated catalog.
    fn catalog(&self) -> Vec<McpCatalogEntryDto>;
    /// Status changes, for the `mcp_server_changed` SSE event.
    fn subscribe(&self) -> broadcast::Receiver<McpServerChanged>;
}
