//! MCP client bridge: any MCP server (stdio or streamable HTTP) becomes a set
//! of `mcp:<server>/<tool>` tools in a [`tool_registry::ToolRegistry`].
//!
//! * [`McpServerConfig`] / [`McpConfigStore`] describe and persist servers.
//! * [`McpManager`] connects (reconnecting with [`Backoff`]), registers the
//!   filtered tools, counts failures and quarantines misbehaving servers.
//! * Stdio children get a **minimal environment** (only the declared variables,
//!   see [`OS_ESSENTIAL_ENV`]) and their stderr is captured to the log.
//! * Tools are `Destructive`; a server's `readOnlyHint` is honoured only when
//!   the user sets `trust_hints = true` for that server.
//! * Every tool of a server gets the user-configured `category` (default
//!   `accounts`; `meta` is rejected).
//! * Secrets: `secret_headers` / `secret_env` map a header or variable to a
//!   *secret name*, resolved through [`McpManager::with_secret_store`] only at
//!   connect time; values never reach the config file, `Debug` or logs.
//! * A `secret_headers` value may name its secret once —
//!   `Authorization = "Bearer {secret:mcp/github-token}"` — and the fetched
//!   value is substituted for the placeholder, so an entry can send a scheme
//!   without asking the user to paste one.  At most one placeholder; a value
//!   without one keeps its old meaning (the secret name, used verbatim).
//!
//! # Example `mcp.toml`
//!
//! ```toml
//! [[server]]
//! id = "github"
//! category = "accounts"      # accounts (default) | fs | shell | browser | ...
//! trust_hints = false        # true: believe the server's readOnlyHint
//! tool_exclude = ["delete_*"]
//! [server.transport]
//! type = "http"
//! url = "https://example.com/mcp"
//! [server.secret_headers]
//! Authorization = "mcp/github-token"   # secret name, not the value
//!
//! [[server]]
//! id = "files"
//! category = "fs"
//! [server.transport]
//! type = "stdio"
//! command = "npx"
//! args = ["-y", "@modelcontextprotocol/server-filesystem@2.2.0", "/path/to/a/folder"]
//! [server.secret_env]
//! API_KEY = "mcp/files-key"
//! ```
//!
//! Large tool sets are meant to be fronted by `agent-router`'s discovery
//! meta-tools rather than shown to the model directly.

mod catalog;
mod client;
mod config;
mod diagnostics;
mod discovery;
mod error;
mod manager;
pub mod mcpb;
pub mod oauth;
pub mod oauth_net;
pub mod registry;
mod roots;
pub mod runtime;
mod session;
mod tool;
mod transport;

pub use catalog::{
    CatalogEntry, CatalogInstall, RequiredFolder, RequiredSecret, builtin_catalog,
    check_stdio_package_pin,
};
pub use config::{McpConfigStore, McpServerConfig, McpTransport, OAuthConfig, TomlFileStore};
pub use diagnostics::ExternalRedactor;
pub use error::{McpError, McpErrorKind, Result, runtime_install_hint};
pub use manager::{Backoff, McpManager, McpPolicy, McpServerHandle, ServerSnapshot, ServerStatus};
pub use roots::{
    FS_SERVER_PACKAGE, PathGuard, canonicalize_filesystem_args, filesystem_roots, root_problem,
};
pub use tool::{qualified_name, registered_spec};
pub use transport::OS_ESSENTIAL_ENV;
