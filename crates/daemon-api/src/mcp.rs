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

/// A configured MCP server (`GET /v1/mcp/servers`).  The full stored config,
/// except secret *values*: `secret_env` / `secret_headers` carry secret
/// **names** only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct McpServerDto {
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
    /// Number of tools currently registered.
    pub tool_count: u32,
    /// Permission category of every tool of the server.
    pub category: String,
    /// Whether the readOnlyHint annotations of the server are believed.
    pub trust_hints: bool,
    /// Bots that may use the server; `null` = all bots.
    pub bots: Option<Vec<String>>,
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
/// `POST /v1/secrets/{name}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct McpServerInput {
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
}

/// What `/v1/mcp/*` needs from the embedder: the managed MCP servers, their
/// persistence (`<data dir>/mcp.toml`, written atomically) and the catalog.
#[async_trait]
pub trait McpControl: Send + Sync {
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
    /// Lift a quarantine / retry the connection.  A connect failure is not an
    /// error: it shows as `status: error` in the returned server.
    ///
    /// # Errors
    /// [`McpControlError`].
    async fn reconnect(&self, name: &str) -> Result<McpServerDto, McpControlError>;
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
