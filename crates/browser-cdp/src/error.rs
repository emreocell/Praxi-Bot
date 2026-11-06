//! Error type for the browser-cdp crate.

use std::fmt;

/// Errors produced while driving a browser over CDP.
#[derive(Debug)]
pub enum Error {
    /// Filesystem or socket failure.
    Io(String),
    /// WebSocket transport failure.
    WebSocket(String),
    /// The browser answered a command with a protocol error.
    Protocol {
        /// CDP error code.
        code: i64,
        /// CDP error message.
        message: String,
    },
    /// An operation did not finish in time.
    Timeout(String),
    /// The CDP connection is closed.
    Closed,
    /// A message could not be (de)serialized or had an unexpected shape.
    Json(String),
    /// No Chrome/Edge binary could be located.
    BrowserNotFound,
    /// The browser process failed to start or expose a debugging endpoint.
    Launch(String),
    /// The element ref was never issued or has been invalidated.
    UnknownRef(String),
    /// The element behind a ref could not be used (detached, not visible, ...).
    Element(String),
    /// JavaScript evaluation threw.
    Js(String),
    /// Navigation failed.
    Navigation(String),
}

/// Crate result alias.
pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(m) => write!(f, "io error: {m}"),
            Self::WebSocket(m) => write!(f, "websocket error: {m}"),
            Self::Protocol { code, message } => write!(f, "CDP error {code}: {message}"),
            Self::Timeout(m) => write!(f, "timeout: {m}"),
            Self::Closed => write!(f, "CDP connection closed"),
            Self::Json(m) => write!(f, "json error: {m}"),
            Self::BrowserNotFound => write!(f, "no Chrome or Edge installation found"),
            Self::Launch(m) => write!(f, "browser launch failed: {m}"),
            Self::UnknownRef(r) => write!(f, "unknown or stale element ref '{r}'"),
            Self::Element(m) => write!(f, "element error: {m}"),
            Self::Js(m) => write!(f, "javascript error: {m}"),
            Self::Navigation(m) => write!(f, "navigation failed: {m}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e.to_string())
    }
}

impl Error {
    /// True when the connection itself is unusable (as opposed to a per-element failure).
    pub fn is_fatal(&self) -> bool {
        matches!(
            self,
            Self::Io(_)
                | Self::WebSocket(_)
                | Self::Closed
                | Self::Launch(_)
                | Self::BrowserNotFound
        )
    }
}
