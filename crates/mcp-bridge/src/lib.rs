//! MCP client bridge: any MCP server (stdio or streamable HTTP) becomes a set
//! of `mcp:<server>/<tool>` tools in a [`tool_registry::ToolRegistry`].
//!
//! * [`McpServerConfig`] / [`McpConfigStore`] describe and persist servers.
//! * [`McpManager`] connects (reconnecting with [`Backoff`]), registers the
//!   filtered tools, counts failures and quarantines misbehaving servers.
//! * Stdio children get a **minimal environment** (only the declared variables,
//!   see [`OS_ESSENTIAL_ENV`]) and their stderr is captured to the log.
//! * Tools are `Destructive` unless the server annotates `readOnlyHint`.
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
