//! Error type of the MCP bridge.

use thiserror::Error;

/// Everything that can go wrong while configuring, connecting to or calling an
/// MCP server.
#[derive(Debug, Error)]
pub enum McpError {
    /// The server configuration is invalid.
    #[error("invalid MCP server config: {0}")]
    Config(String),
    /// Reading or writing the persisted configuration failed.
    #[error("MCP config store: {0}")]
    Store(String),
    /// The server process could not be started.
    #[error("cannot start MCP server `{server}`: {message}")]
    Spawn {
        /// Server id.
        server: String,
        /// Underlying error.
        message: String,
    },
    /// The MCP handshake or a request failed at the transport/protocol level.
    #[error("MCP server `{server}`: {message}")]
    Transport {
        /// Server id.
        server: String,
        /// Underlying error.
        message: String,
    },
    /// The server did not answer within the configured timeout.
    #[error("MCP server `{server}` timed out after {seconds}s")]
    Timeout {
        /// Server id.
        server: String,
        /// Timeout in seconds.
        seconds: u64,
    },
    /// The server is quarantined; re-enable it manually.
    #[error("MCP server `{0}` is quarantined after repeated failures; re-enable it manually")]
    Quarantined(String),
    /// The server is disabled in its configuration.
    #[error("MCP server `{0}` is disabled")]
    Disabled(String),
    /// No server with this id is registered.
    #[error("unknown MCP server `{0}`")]
    UnknownServer(String),
}

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, McpError>;
