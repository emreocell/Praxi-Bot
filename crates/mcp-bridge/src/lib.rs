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
//! args = ["-y", "@modelcontextprotocol/server-filesystem", "."]
//! [server.secret_env]
//! API_KEY = "mcp/files-key"
//! ```
//!
//! Large tool sets are meant to be fronted by `agent-router`'s discovery
//! meta-tools rather than shown to the model directly.

mod config;
mod error;
mod manager;
mod tool;
mod transport;

pub use config::{McpConfigStore, McpServerConfig, McpTransport, TomlFileStore};
pub use error::{McpError, Result};
pub use manager::{Backoff, McpManager, McpPolicy, ServerSnapshot, ServerStatus};
pub use tool::qualified_name;
pub use transport::OS_ESSENTIAL_ENV;
