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
    /// The program that launches the server (`npx`, `uvx`, ...) is not
    /// installed or not on `PATH`. Detected before anything is spawned.
    #[error("cannot start MCP server `{server}`: `{command}` was not found on PATH. {hint}")]
    RuntimeMissing {
        /// Server id.
        server: String,
        /// The command that was not found.
        command: String,
        /// What the user should install (English, actionable).
        hint: String,
    },
    /// A filesystem server has no usable folder to serve: the user must choose one.
    #[error("MCP server `{server}` needs a folder to serve: {reason}")]
    NeedsFolder {
        /// Server id.
        server: String,
        /// Why the current setting cannot be used.
        reason: String,
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
    /// The server wants an OAuth sign-in (none stored, or it was rejected and
    /// could not be refreshed). Only the user can fix it; it is not a failure
    /// of the server and never counts towards a quarantine.
    #[error(
        "MCP server `{0}` requires authorization: sign in to it, or store an access token in \
         the secret store and reference it under `secret_headers.Authorization`"
    )]
    AuthRequired(String),
    /// No server with this id is registered.
    #[error("unknown MCP server `{0}`")]
    UnknownServer(String),
    /// The tool definition offered now is not the one the owner approved.
    #[error("MCP server `{server}` changed tool `{tool}` again; review the new definition")]
    ToolChangedAgain {
        /// Server id.
        server: String,
        /// Tool name.
        tool: String,
    },
}

/// Coarse class of a failure, for UIs that show a localized "what to do" hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpErrorKind {
    /// The launcher program (`npx`, `uvx`, ...) is not installed.
    RuntimeMissing,
    /// The process could not be started for another reason.
    Spawn,
    /// The process exited or closed its pipes before the handshake finished.
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

impl McpError {
    /// The coarse class of this error.
    #[must_use]
    pub fn kind(&self) -> McpErrorKind {
        match self {
            Self::RuntimeMissing { .. } => McpErrorKind::RuntimeMissing,
            Self::Spawn { .. } => McpErrorKind::Spawn,
            Self::Timeout { .. } => McpErrorKind::Timeout,
            Self::Config(_) => McpErrorKind::Config,
            Self::NeedsFolder { .. } => McpErrorKind::NeedsFolder,
            Self::Transport { message, .. } if message.contains("connection closed") => {
                McpErrorKind::Closed
            }
            _ => McpErrorKind::Other,
        }
    }
}

/// An actionable English hint for a launcher program that is not installed.
#[must_use]
pub fn runtime_install_hint(command: &str) -> String {
    let base = std::path::Path::new(command)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(command)
        .to_ascii_lowercase();
    match base.as_str() {
        "npx" | "npm" | "node" => "Install Node.js (LTS) from https://nodejs.org, restart Praxi-Bot and reconnect.".to_owned(),
        "uvx" | "uv" => "Install uv (https://docs.astral.sh/uv/getting-started/installation/; on Windows: `winget install --id=astral-sh.uv -e`), restart Praxi-Bot and reconnect.".to_owned(),
        "pipx" => "Install pipx (https://pipx.pypa.io), restart Praxi-Bot and reconnect.".to_owned(),
        "docker" => "Install Docker (or add its folder to PATH), restart Praxi-Bot and reconnect.".to_owned(),
        _ => format!("Install `{command}` (or give its full path in the server settings), then reconnect."),
    }
}

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, McpError>;
