//! JSON-RPC 2.0 over iroh bi-streams: one request/response pair per stream.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use iroh::endpoint::{Connection, ConnectionError, RecvStream, SendStream};
use serde_json::Value;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::error::{PeerError, Result};
use crate::net::Inner;
use crate::proto::{
    CLOSE_NOT_PAIRED, CLOSE_REVOKED, ERR_INTERNAL, ERR_INVALID_REQUEST, ERR_PARSE, ERR_TIMEOUT,
    MAX_FRAME, Request, Response, RpcError, STOP_CANCELLED, read_frame, write_frame,
};
use crate::store::Device;

/// Default per-request timeout.
pub const DEFAULT_RPC_TIMEOUT: Duration = Duration::from_secs(30);

/// Implemented by whatever answers RPC calls on the serving side.
#[async_trait]
pub trait RpcHandler: Send + Sync {
    /// Handle `method` called by the (already paired) `peer`.
    async fn handle(
        &self,
        peer: &Device,
        method: &str,
        params: Value,
    ) -> std::result::Result<Value, RpcError>;
}

// ── Server ────────────────────────────────────────────────────────────────────

/// Serve streams of one connection until it closes.
pub(crate) async fn serve_connection(
    conn: Connection,
    peer: Device,
    handler: Arc<dyn RpcHandler>,
    timeout: Duration,
) {
    let peer = Arc::new(peer);
    while let Ok((send, recv)) = conn.accept_bi().await {
        let handler = handler.clone();
        let peer = peer.clone();
        tokio::spawn(async move {
            handle_stream(send, recv, &peer, handler.as_ref(), timeout).await;
        });
    }
}

async fn handle_stream(
    mut send: SendStream,
    mut recv: RecvStream,
    peer: &Device,
    handler: &dyn RpcHandler,
    timeout: Duration,
) {
    let Ok(Some(bytes)) = read_frame(&mut recv, MAX_FRAME).await else {
        let _ = send.reset(STOP_CANCELLED.into());
        return;
    };
    let response = match serde_json::from_slice::<Request>(&bytes) {
        Err(e) => Response::err(
            Value::Null,
            RpcError::new(ERR_PARSE, format!("parse error: {e}")),
        ),
        Ok(req) if req.jsonrpc != "2.0" => Response::err(
            req.id,
            RpcError::new(ERR_INVALID_REQUEST, "jsonrpc must be \"2.0\""),
        ),
        Ok(req) => {
            let id = req.id.clone();
            let work = tokio::time::timeout(timeout, handler.handle(peer, &req.method, req.params));
            tokio::select! {
                r = work => match r {
                    Ok(Ok(v)) => Response::ok(id, v),
                    Ok(Err(e)) => Response::err(id, e),
                    Err(_) => Response::err(id, RpcError::new(ERR_TIMEOUT, "handler timed out")),
                },
                // The caller cancelled (stopped reading): abandon the work.
                _ = send.stopped() => return,
            }
        }
    };
    let body = serde_json::to_vec(&response).unwrap_or_default();
    if write_frame(&mut send, &body, MAX_FRAME).await.is_err() {
        // Typically an oversize result: tell the caller instead of hanging.
        let fallback = Response::err(
            response.id,
            RpcError::new(ERR_INTERNAL, "response could not be sent (too large?)"),
        );
        let body = serde_json::to_vec(&fallback).unwrap_or_default();
        let _ = write_frame(&mut send, &body, MAX_FRAME).await;
    }
    let _ = send.finish();
    // Keep the stream alive until the caller has read the response.
    let _ = send.stopped().await;
}

// ── Client ────────────────────────────────────────────────────────────────────

/// Calls RPC methods on a paired peer; reconnects lazily when the link drops.
pub struct RpcClient {
    inner: Arc<Inner>,
    peer: iroh::PublicKey,
    conn: Mutex<Option<Connection>>,
    next_id: AtomicU64,
    timeout: Duration,
}

impl RpcClient {
    pub(crate) fn new(inner: Arc<Inner>, peer: iroh::PublicKey, timeout: Duration) -> Self {
        Self {
            inner,
            peer,
            conn: Mutex::new(None),
            next_id: AtomicU64::new(1),
            timeout,
        }
    }

    /// Override the per-request timeout.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The peer this client talks to.
    #[must_use]
    pub fn peer(&self) -> iroh::PublicKey {
        self.peer
    }

    async fn connection(&self) -> Result<Connection> {
        let mut guard = self.conn.lock().await;
        if let Some(c) = guard.as_ref().filter(|c| c.close_reason().is_none()) {
            return Ok(c.clone());
        }
        let c = self.inner.dial(&self.peer, crate::proto::ALPN_RPC).await?;
        *guard = Some(c.clone());
        Ok(c)
    }

    /// Call `method` with `params`.
    ///
    /// # Errors
    ///
    /// [`PeerError::Rpc`] for remote errors, [`PeerError::Timeout`],
    /// [`PeerError::NotPaired`] if the peer dropped us, or connection errors.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.call_cancellable(method, params, &CancellationToken::new())
            .await
    }

    /// Like [`call`](Self::call) but abortable: cancelling `cancel` stops the
    /// request on the wire so the node abandons the work.
    ///
    /// # Errors
    ///
    /// Additionally returns [`PeerError::Cancelled`] when `cancel` fires.
    pub async fn call_cancellable(
        &self,
        method: &str,
        params: Value,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        let conn = self.connection().await?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let req = Request {
            jsonrpc: "2.0".into(),
            id: Value::from(id),
            method: method.to_string(),
            params,
        };
        let body = serde_json::to_vec(&req).map_err(|e| PeerError::Protocol(e.to_string()))?;
        let (mut send, mut recv) = conn.open_bi().await.map_err(|e| map_conn_err(&conn, &e))?;
        write_frame(&mut send, &body, MAX_FRAME)
            .await
            .map_err(|e| remap(&conn, e))?;
        send.finish()
            .map_err(|e| PeerError::Connection(e.to_string()))?;

        let outcome = {
            let work = tokio::time::timeout(self.timeout, read_frame(&mut recv, MAX_FRAME));
            tokio::select! {
                r = work => Some(r),
                () = cancel.cancelled() => None,
            }
        };
        let frame = match outcome {
            None => {
                let _ = recv.stop(STOP_CANCELLED.into());
                return Err(PeerError::Cancelled);
            }
            Some(Err(_elapsed)) => {
                let _ = recv.stop(STOP_CANCELLED.into());
                return Err(PeerError::Timeout);
            }
            Some(Ok(r)) => r.map_err(|e| remap(&conn, e))?,
        };
        let bytes = frame.ok_or_else(|| {
            PeerError::Connection("stream closed before a response arrived".into())
        })?;
        let resp: Response = serde_json::from_slice(&bytes)
            .map_err(|e| PeerError::Protocol(format!("bad response: {e}")))?;
        if let Some(e) = resp.error {
            return Err(PeerError::Rpc {
                code: e.code,
                message: e.message,
                data: e.data,
            });
        }
        resp.result
            .ok_or_else(|| PeerError::Protocol("response has neither result nor error".into()))
    }
}

/// Turn a connection-level failure into a precise error when the peer closed
/// the link deliberately (not paired / revoked).
pub(crate) fn map_conn_err(conn: &Connection, e: &impl std::fmt::Display) -> PeerError {
    if let Some(ConnectionError::ApplicationClosed(c)) = conn.close_reason() {
        let code = c.error_code.into_inner();
        if code == u64::from(CLOSE_NOT_PAIRED) || code == u64::from(CLOSE_REVOKED) {
            return PeerError::NotPaired(String::from_utf8_lossy(&c.reason).into_owned());
        }
    }
    PeerError::Connection(e.to_string())
}

fn remap(conn: &Connection, e: PeerError) -> PeerError {
    match e {
        PeerError::Connection(m) => map_conn_err(conn, &m),
        other => other,
    }
}
