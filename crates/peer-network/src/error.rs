//! Error type for the peer network.

use thiserror::Error;

/// Errors from pairing, connections, and RPC calls.
#[derive(Debug, Error)]
pub enum PeerError {
    /// Local filesystem problem (identity key, device list).
    #[error("storage error: {0}")]
    Storage(String),
    /// The endpoint could not be created or has been closed.
    #[error("endpoint error: {0}")]
    Endpoint(String),
    /// A connection could not be established or broke.
    #[error("connection error: {0}")]
    Connection(String),
    /// A malformed ticket, frame, or message.
    #[error("protocol error: {0}")]
    Protocol(String),
    /// The two sides speak different protocol versions.
    #[error(
        "incompatible protocol version: local v{local}, remote v{remote}; update the older side"
    )]
    VersionMismatch { local: u32, remote: u32 },
    /// The remote side refused pairing.
    #[error("pairing rejected: {0}")]
    PairingRejected(String),
    /// The remote side closed the link because this key is not (or no longer) paired.
    #[error("not paired: {0}")]
    NotPaired(String),
    /// The target device is not in the device list.
    #[error("unknown device: {0}")]
    UnknownDevice(String),
    /// The remote RPC handler returned a JSON-RPC error.
    #[error("rpc error {code}: {message}")]
    Rpc {
        code: i64,
        message: String,
        data: Option<serde_json::Value>,
    },
    /// The request did not complete in time.
    #[error("request timed out")]
    Timeout,
    /// The request was cancelled by the caller.
    #[error("request cancelled")]
    Cancelled,
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, PeerError>;
