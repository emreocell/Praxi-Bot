//! Wire protocol: ALPNs, framing, JSON-RPC 2.0 types, pairing and event messages.

use iroh::{EndpointAddr, endpoint::RecvStream, endpoint::SendStream};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{PeerError, Result};
use crate::store::Role;

/// ALPN of the pairing protocol (unauthenticated, invite-gated).
pub const ALPN_PAIR: &[u8] = b"bot/pair/1";
/// ALPN of the JSON-RPC protocol (paired keys only).
pub const ALPN_RPC: &[u8] = b"bot/rpc/1";
/// ALPN of the event / heartbeat protocol (paired keys only).
pub const ALPN_EVENTS: &[u8] = b"bot/events/1";

/// Application protocol version carried inside pairing and `node.info`.
///
/// The ALPN suffix is the transport-level major version; this number lets two
/// peers that did connect report an incompatibility with a clear message.
pub const PROTOCOL_VERSION: u32 = 1;

/// Largest accepted RPC / event frame (screenshots travel base64-encoded).
pub const MAX_FRAME: usize = 32 * 1024 * 1024;
/// Largest accepted pairing frame.
pub const MAX_PAIR_FRAME: usize = 64 * 1024;

/// QUIC application close code: the key is not (or no longer) paired.
pub const CLOSE_NOT_PAIRED: u32 = 403;
/// QUIC application close code: the device was revoked while connected.
pub const CLOSE_REVOKED: u32 = 410;
/// QUIC application close code: the endpoint is shutting down.
pub const CLOSE_SHUTDOWN: u32 = 0;
/// Stream stop code used when a caller cancels a request.
pub const STOP_CANCELLED: u32 = 499;

// ── Framing ───────────────────────────────────────────────────────────────────

/// Write one length-prefixed (u32 big endian) frame.
///
/// # Errors
///
/// Fails if the frame exceeds `max` or the stream is broken.
pub async fn write_frame(send: &mut SendStream, payload: &[u8], max: usize) -> Result<()> {
    if payload.len() > max {
        return Err(PeerError::Protocol(format!(
            "message of {} bytes exceeds the {max} byte limit",
            payload.len()
        )));
    }
    let len = u32::try_from(payload.len())
        .map_err(|_| PeerError::Protocol("message too large".into()))?;
    send.write_all(&len.to_be_bytes())
        .await
        .map_err(|e| PeerError::Connection(e.to_string()))?;
    send.write_all(payload)
        .await
        .map_err(|e| PeerError::Connection(e.to_string()))
}

/// Read buffer growth step, so a header cannot force a large up-front allocation.
const READ_CHUNK: usize = 64 * 1024;

/// Read one frame; `Ok(None)` means the peer finished the stream cleanly
/// before starting a new frame.
///
/// # Errors
///
/// Fails on oversize frames, truncated frames and stream errors.
pub async fn read_frame(recv: &mut RecvStream, max: usize) -> Result<Option<Vec<u8>>> {
    let mut header = [0u8; 4];
    match recv.read_exact(&mut header).await {
        Ok(()) => {}
        Err(iroh::endpoint::ReadExactError::FinishedEarly(0)) => return Ok(None),
        Err(e) => return Err(PeerError::Connection(e.to_string())),
    }
    let len = u32::from_be_bytes(header) as usize;
    if len > max {
        return Err(PeerError::Protocol(format!(
            "incoming message of {len} bytes exceeds the {max} byte limit"
        )));
    }
    // Grow the buffer as data actually arrives instead of trusting the header,
    // so a peer announcing a huge frame and then stalling costs little memory.
    let mut buf: Vec<u8> = Vec::with_capacity(len.min(READ_CHUNK));
    while buf.len() < len {
        let start = buf.len();
        let take = (len - start).min(READ_CHUNK);
        buf.resize(start + take, 0);
        recv.read_exact(&mut buf[start..])
            .await
            .map_err(|e| PeerError::Connection(e.to_string()))?;
    }
    Ok(Some(buf))
}

// ── JSON-RPC 2.0 ──────────────────────────────────────────────────────────────

/// Invalid JSON was received.
pub const ERR_PARSE: i64 = -32700;
/// The request object is invalid.
pub const ERR_INVALID_REQUEST: i64 = -32600;
/// The method does not exist.
pub const ERR_METHOD_NOT_FOUND: i64 = -32601;
/// The parameters are invalid.
pub const ERR_INVALID_PARAMS: i64 = -32602;
/// Internal JSON-RPC error.
pub const ERR_INTERNAL: i64 = -32603;
/// The peer is not allowed to call this method.
pub const ERR_FORBIDDEN: i64 = -32003;
/// The handler ran out of time.
pub const ERR_TIMEOUT: i64 = -32008;

/// A JSON-RPC request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    pub id: Value,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

/// The `error` member of a JSON-RPC response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    /// Build an error without data.
    #[must_use]
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    /// Attach structured data.
    #[must_use]
    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }
}

/// A JSON-RPC response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    pub(crate) fn ok(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub(crate) fn err(id: Value, error: RpcError) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(error),
        }
    }
}

// ── Pairing ───────────────────────────────────────────────────────────────────

/// First (and only) message of a joining peer on `bot/pair/1`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairRequest {
    pub protocol_version: u32,
    /// One-time secret from the ticket.
    pub secret: String,
    /// Six-digit code from the ticket.
    pub code: String,
    /// Display name of the joining peer.
    pub name: String,
    /// Role the joining peer takes.
    pub role: Role,
    /// Capabilities the joining peer offers.
    pub capabilities: Vec<String>,
    /// Current dialable address of the joining peer.
    pub addr: EndpointAddr,
}

/// Why a pairing attempt was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectReason {
    VersionMismatch,
    InvalidInvite,
    Expired,
    RoleMismatch,
    NotAccepting,
    Internal,
}

/// The core's answer on `bot/pair/1`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PairResponse {
    Accepted {
        protocol_version: u32,
        name: String,
        role: Role,
        capabilities: Vec<String>,
        addr: EndpointAddr,
    },
    Rejected {
        reason: RejectReason,
        protocol_version: u32,
        message: String,
    },
}

// ── Events ────────────────────────────────────────────────────────────────────

/// A message pushed by a peer on `bot/events/1`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    /// Event kind, e.g. `heartbeat`.
    pub kind: String,
    /// Sender clock, unix milliseconds.
    pub ts_ms: u64,
    /// Kind-specific payload.
    #[serde(default)]
    pub data: Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_omits_absent_members() {
        let r = Response::ok(Value::from(1), Value::from("x"));
        let s = serde_json::to_string(&r).unwrap();
        assert!(!s.contains("error"));
        let r = Response::err(Value::from(1), RpcError::new(ERR_INTERNAL, "boom"));
        let s = serde_json::to_string(&r).unwrap();
        assert!(!s.contains("result"));
    }

    #[test]
    fn request_defaults_params() {
        let r: Request =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"method":"node.ping"}"#).unwrap();
        assert!(r.params.is_null());
    }
}
