//! JSON-RPC 2.0 over iroh bi-streams: one request/response pair per stream.

use std::future::Future;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use iroh::endpoint::{
    Connection, ConnectionError, QuicTransportConfig, RecvStream, SendStream, VarInt,
};
use serde::Serialize;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::error::{PeerError, Result};
use crate::net::Inner;
use crate::proto::{
    CLOSE_NOT_PAIRED, CLOSE_REVOKED, ERR_INTERNAL, ERR_INVALID_PARAMS, ERR_INVALID_REQUEST,
    ERR_PARSE, MAX_FRAME, MAX_RPC_REQUEST, Request, Response, RpcError, STOP_BUSY, STOP_CANCELLED,
    read_frame, write_frame,
};
use crate::store::Device;

/// Default per-request timeout, including connection, framing and response delivery.
pub const DEFAULT_RPC_TIMEOUT: Duration = Duration::from_secs(30);

// Reserve both encoded buffers before a task can allocate either one. JSON values
// and handler-owned memory are additionally bounded by the number of active calls;
// these byte limits are not a claim to measure the allocator's total heap usage.
const STREAM_BYTES: usize = MAX_RPC_REQUEST + MAX_FRAME;

/// Admission limits shared by all incoming RPC connections of one endpoint.
#[derive(Debug, Clone, Copy)]
pub struct RpcLimits {
    /// Maximum admitted requests on one connection.
    pub streams_per_connection: usize,
    /// Maximum admitted requests across all connections.
    pub streams_total: usize,
    /// Encoded request/response buffer reservation available to one connection.
    pub bytes_per_connection: usize,
    /// Encoded request/response buffer reservation available to the endpoint.
    pub bytes_total: usize,
}

impl Default for RpcLimits {
    fn default() -> Self {
        Self {
            streams_per_connection: 4,
            streams_total: 16,
            bytes_per_connection: 4 * STREAM_BYTES,
            bytes_total: 16 * STREAM_BYTES,
        }
    }
}

impl RpcLimits {
    pub(crate) fn validate(self) -> Result<()> {
        if self.streams_per_connection == 0
            || self.streams_total == 0
            || u32::try_from(self.streams_per_connection).is_err()
            || self.streams_per_connection > Semaphore::MAX_PERMITS
            || self.streams_total > Semaphore::MAX_PERMITS
            || self.bytes_per_connection < STREAM_BYTES
            || self.bytes_total < STREAM_BYTES
            || self.bytes_per_connection > Semaphore::MAX_PERMITS
            || self.bytes_total > Semaphore::MAX_PERMITS
        {
            return Err(PeerError::Protocol("invalid RPC admission limits".into()));
        }
        Ok(())
    }
}

/// Unidirectional streams the shared endpoint advertises.
///
/// Heartbeats open one unidirectional stream. Zero would stop that link from
/// reconnecting; the RPC task simply never accepts unidirectional streams.
const ENDPOINT_UNI_STREAMS: u32 = 1;

/// QUIC transport limits installed on the endpoint before `bind`.
///
/// The handshake advertises these values, and `set_max_concurrent_*` after
/// accept cannot take that credit back. `max_concurrent_bidi_streams` is
/// `limits.streams_per_connection` (pairing opens one bidirectional stream).
/// `max_concurrent_uni_streams` is [`ENDPOINT_UNI_STREAMS`]. `receive_window`
/// is `limits.bytes_per_connection`, so per-stream windows cannot stack up to
/// the QUIC maximum.
///
/// `limits` must already have passed [`RpcLimits::validate`].
#[must_use]
pub(crate) fn endpoint_transport_config(limits: RpcLimits) -> QuicTransportConfig {
    let bidi =
        u32::try_from(limits.streams_per_connection).expect("RPC limits were validated at startup");
    let window =
        u64::try_from(limits.bytes_per_connection).expect("RPC limits were validated at startup");
    QuicTransportConfig::builder()
        .max_concurrent_bidi_streams(VarInt::from_u32(bidi))
        .max_concurrent_uni_streams(VarInt::from_u32(ENDPOINT_UNI_STREAMS))
        .receive_window(
            VarInt::from_u64(window).expect("per-connection byte budget fits a QUIC varint"),
        )
        .build()
}

pub(crate) struct RpcBudget {
    slots: Arc<Semaphore>,
    bytes: Arc<Semaphore>,
}

impl RpcBudget {
    pub(crate) fn new(slots: usize, bytes: usize) -> Self {
        Self {
            slots: Arc::new(Semaphore::new(slots)),
            bytes: Arc::new(Semaphore::new(bytes)),
        }
    }

    fn try_acquire(&self) -> Option<BudgetPermit> {
        let slots = self.slots.clone().try_acquire_owned().ok()?;
        let bytes = self
            .bytes
            .clone()
            .try_acquire_many_owned(
                u32::try_from(STREAM_BYTES).expect("fixed frame limits fit u32"),
            )
            .ok()?;
        Some(BudgetPermit {
            _slots: slots,
            _bytes: bytes,
        })
    }
}

struct BudgetPermit {
    _slots: OwnedSemaphorePermit,
    _bytes: OwnedSemaphorePermit,
}

struct Admission {
    _connection: BudgetPermit,
    _global: BudgetPermit,
}

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

// Keep cancellation cleanup in one place, including when a call future is
// dropped by its caller. The traits also let the actual framing/deadline path be
// exercised with in-memory streams, without opening a socket in regression tests.
trait RpcSend: AsyncWrite + Unpin + Send {
    fn finish(&mut self) -> Result<()>;
    fn reset(&mut self, code: u32);
    fn stopped(&self) -> impl Future<Output = ()> + Send;
}

impl RpcSend for SendStream {
    fn finish(&mut self) -> Result<()> {
        Self::finish(self).map_err(|e| PeerError::Connection(e.to_string()))
    }

    fn reset(&mut self, code: u32) {
        let _ = Self::reset(self, code.into());
    }

    fn stopped(&self) -> impl Future<Output = ()> + Send {
        let stopped = Self::stopped(self);
        async move {
            let _ = stopped.await;
        }
    }
}

trait RpcRecv: AsyncRead + Unpin + Send {
    fn stop(&mut self, code: u32);
}

impl RpcRecv for RecvStream {
    fn stop(&mut self, code: u32) {
        let _ = Self::stop(self, code.into());
    }
}

struct RpcStream<S: RpcSend, R: RpcRecv> {
    send: S,
    recv: R,
    complete: bool,
}

impl<S: RpcSend, R: RpcRecv> RpcStream<S, R> {
    fn new(send: S, recv: R) -> Self {
        Self {
            send,
            recv,
            complete: false,
        }
    }

    fn abort(&mut self, code: u32) {
        self.send.reset(code);
        self.recv.stop(code);
        self.complete = true;
    }
}

impl<S: RpcSend, R: RpcRecv> Drop for RpcStream<S, R> {
    fn drop(&mut self) {
        if !self.complete {
            self.abort(STOP_CANCELLED);
        }
    }
}

fn deadline(timeout: Duration) -> Result<Instant> {
    Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| PeerError::Protocol("RPC timeout is out of range".into()))
}

async fn within_deadline<T>(
    deadline: Instant,
    cancel: &CancellationToken,
    work: impl Future<Output = Result<T>>,
) -> Result<T> {
    tokio::select! {
        // Never poll work (not even the connection lock) if already cancelled.
        biased;
        () = cancel.cancelled() => Err(PeerError::Cancelled),
        () = tokio::time::sleep_until(deadline) => Err(PeerError::Timeout),
        result = work => result,
    }
}

fn request_limit(method: &str) -> usize {
    match method {
        "node.ping" | "node.info" | "desktop.windows" | "desktop.capabilities" => 4 * 1024,
        "desktop.snapshot" | "desktop.screenshot" => 16 * 1024,
        "desktop.launch" => 64 * 1024,
        _ => MAX_RPC_REQUEST,
    }
}

// A length check after to_vec would already have allocated an unbounded buffer.
fn encode_bounded(value: &impl Serialize, max: usize) -> Result<Vec<u8>> {
    struct Buffer {
        bytes: Vec<u8>,
        max: usize,
    }

    impl Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.max - self.bytes.len() {
                return Err(std::io::Error::other("RPC message exceeds its byte limit"));
            }
            let needed = self.bytes.len() + bytes.len();
            if needed > self.bytes.capacity() {
                let capacity = needed
                    .max(self.bytes.capacity().saturating_mul(2))
                    .min(self.max);
                self.bytes.reserve_exact(capacity - self.bytes.len());
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let mut buffer = Buffer {
        bytes: Vec::new(),
        max,
    };
    serde_json::to_writer(&mut buffer, value).map_err(|e| PeerError::Protocol(e.to_string()))?;
    Ok(buffer.bytes)
}

// ── Server ────────────────────────────────────────────────────────────────────

fn admit<S: RpcSend, R: RpcRecv>(
    mut stream: RpcStream<S, R>,
    connection: &RpcBudget,
    global: &RpcBudget,
) -> Option<(RpcStream<S, R>, Admission)> {
    let permits = connection
        .try_acquire()
        .and_then(|local| global.try_acquire().map(|global| (local, global)));
    if let Some((local, global)) = permits {
        Some((
            stream,
            Admission {
                _connection: local,
                _global: global,
            },
        ))
    } else {
        // Do not queue tasks waiting for permits: a paired peer could otherwise
        // pin arbitrarily many stream handles even without a body allocation.
        stream.abort(STOP_BUSY);
        None
    }
}

/// Serve streams of one connection until it closes.
pub(crate) async fn serve_connection(
    conn: Connection,
    peer: Device,
    handler: Arc<dyn RpcHandler>,
    timeout: Duration,
    limits: RpcLimits,
    global: Arc<RpcBudget>,
) {
    let local = RpcBudget::new(limits.streams_per_connection, limits.bytes_per_connection);
    // Stream credit was fixed in the handshake by `endpoint_transport_config`.
    // Admission still rejects bidi work that exceeds the byte budget. This
    // loop does not accept unidirectional streams.
    let peer = Arc::new(peer);
    while let Ok((send, recv)) = conn.accept_bi().await {
        let Ok(deadline) = deadline(timeout) else {
            conn.close(400_u32.into(), b"invalid RPC timeout");
            break;
        };
        let Some((stream, admission)) = admit(RpcStream::new(send, recv), &local, &global) else {
            continue;
        };
        let handler = handler.clone();
        let peer = peer.clone();
        tokio::spawn(async move {
            handle_stream(stream, &peer, handler.as_ref(), deadline, admission).await;
        });
    }
}

async fn handle_stream<S: RpcSend, R: RpcRecv>(
    mut stream: RpcStream<S, R>,
    peer: &Device,
    handler: &dyn RpcHandler,
    deadline: Instant,
    _admission: Admission,
) {
    let work = async {
        let bytes = read_frame(&mut stream.recv, MAX_RPC_REQUEST)
            .await?
            .ok_or_else(|| PeerError::Protocol("empty RPC request".into()))?;
        let frame_len = bytes.len();
        let request = serde_json::from_slice::<Request>(&bytes);
        drop(bytes);
        let response = match request {
            Err(e) => Response::err(
                Value::Null,
                RpcError::new(ERR_PARSE, format!("parse error: {e}")),
            ),
            Ok(req) if req.jsonrpc != "2.0" => Response::err(
                req.id,
                RpcError::new(ERR_INVALID_REQUEST, "jsonrpc must be \"2.0\""),
            ),
            Ok(req) if frame_len > request_limit(&req.method) => Response::err(
                req.id,
                RpcError::new(
                    ERR_INVALID_PARAMS,
                    "request exceeds the method's byte limit",
                ),
            ),
            Ok(req) => {
                tokio::select! {
                    biased;
                    () = stream.send.stopped() => return Err(PeerError::Cancelled),
                    result = handler.handle(peer, &req.method, req.params) => match result {
                        Ok(value) => Response::ok(req.id, value),
                        Err(error) => Response::err(req.id, error),
                    },
                }
            }
        };
        let body = match encode_bounded(&response, MAX_FRAME) {
            Ok(body) => body,
            Err(_) => encode_bounded(
                &Response::err(
                    response.id,
                    RpcError::new(ERR_INTERNAL, "response exceeds its byte limit"),
                ),
                MAX_FRAME,
            )?,
        };
        write_frame(&mut stream.send, &body, MAX_FRAME).await?;
        stream.send.finish()?;
        // Delivery is part of the same deadline, including a peer that never
        // reads its response. Never restart the timer after reading or handling.
        stream.send.stopped().await;
        Ok(())
    };
    if within_deadline(deadline, &CancellationToken::new(), work)
        .await
        .is_ok()
    {
        stream.complete = true;
    }
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

    /// Override the timeout for the entire request, including connecting.
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

    /// Like [`call`](Self::call) but abortable: cancelling `cancel` stops both
    /// halves of an opened request stream so the node abandons the work.
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
        if cancel.is_cancelled() {
            return Err(PeerError::Cancelled);
        }
        let deadline = deadline(self.timeout)?;
        let request = Request {
            jsonrpc: "2.0".into(),
            id: Value::from(self.next_id.fetch_add(1, Ordering::Relaxed)),
            method: method.to_string(),
            params,
        };
        let mut connection = None;
        // The lazy open future includes the mutex wait and dial, not just
        // open_bi. The exchange's single deadline starts before all of them.
        let open = async {
            let conn = self.connection().await?;
            connection = Some(conn.clone());
            conn.open_bi().await.map_err(|e| map_conn_err(&conn, &e))
        };
        exchange(request, deadline, cancel, open)
            .await
            .map_err(|error| match &connection {
                Some(conn) => remap(conn, error),
                None => error,
            })
    }
}

async fn exchange<S: RpcSend, R: RpcRecv>(
    request: Request,
    deadline: Instant,
    cancel: &CancellationToken,
    open: impl Future<Output = Result<(S, R)>>,
) -> Result<Value> {
    within_deadline(deadline, cancel, async {
        let body = encode_bounded(&request, request_limit(&request.method))?;
        let (send, recv) = open.await?;
        let mut stream = RpcStream::new(send, recv);
        write_frame(&mut stream.send, &body, MAX_RPC_REQUEST).await?;
        stream.send.finish()?;
        let bytes = read_frame(&mut stream.recv, MAX_FRAME)
            .await?
            .ok_or_else(|| {
                PeerError::Connection("stream closed before a response arrived".into())
            })?;
        let response: Response = serde_json::from_slice(&bytes)
            .map_err(|e| PeerError::Protocol(format!("bad response: {e}")))?;
        if let Some(e) = response.error {
            stream.complete = true;
            return Err(PeerError::Rpc {
                code: e.code,
                message: e.message,
                data: e.data,
            });
        }
        let result = response
            .result
            .ok_or_else(|| PeerError::Protocol("response has neither result nor error".into()))?;
        stream.complete = true;
        Ok(result)
    })
    .await
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

#[cfg(test)]
#[path = "rpc_tests.rs"]
mod tests;
