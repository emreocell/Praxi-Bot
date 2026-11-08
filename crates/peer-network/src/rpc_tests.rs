//! Hermetic RPC regression tests: in-memory streams and paused Tokio time.

use std::pin::Pin;
use std::sync::atomic::{AtomicU32, AtomicUsize};
use std::task::{Context, Poll, Waker};

use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream, ReadBuf};

use super::*;
use crate::store::Role;

#[derive(Default)]
struct StreamState {
    reset: AtomicU32,
    stop: AtomicU32,
    written: AtomicUsize,
    finished: AtomicUsize,
}

struct MemorySend {
    io: DuplexStream,
    state: Arc<StreamState>,
    stopped: CancellationToken,
}

impl AsyncWrite for MemorySend {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let result = Pin::new(&mut self.io).poll_write(cx, bytes);
        if let Poll::Ready(Ok(n)) = result {
            self.state.written.fetch_add(n, Ordering::SeqCst);
        }
        result
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.io).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.io).poll_shutdown(cx)
    }
}

impl RpcSend for MemorySend {
    fn finish(&mut self) -> Result<()> {
        self.state.finished.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn reset(&mut self, code: u32) {
        self.state.reset.store(code, Ordering::SeqCst);
    }

    fn stopped(&self) -> impl Future<Output = ()> + Send {
        self.stopped.cancelled()
    }
}

struct MemoryRecv {
    io: DuplexStream,
    state: Arc<StreamState>,
}

impl AsyncRead for MemoryRecv {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.io).poll_read(cx, buf)
    }
}

impl RpcRecv for MemoryRecv {
    fn stop(&mut self, code: u32) {
        self.state.stop.store(code, Ordering::SeqCst);
    }
}

struct MemoryPeer {
    writer: DuplexStream,
    reader: DuplexStream,
    state: Arc<StreamState>,
    stopped: CancellationToken,
}

fn memory_halves(capacity: usize) -> (MemorySend, MemoryRecv, MemoryPeer) {
    let (send_io, reader) = tokio::io::duplex(capacity);
    let (writer, recv_io) = tokio::io::duplex(capacity);
    let state = Arc::new(StreamState::default());
    let stopped = CancellationToken::new();
    (
        MemorySend {
            io: send_io,
            state: state.clone(),
            stopped: stopped.clone(),
        },
        MemoryRecv {
            io: recv_io,
            state: state.clone(),
        },
        MemoryPeer {
            writer,
            reader,
            state,
            stopped,
        },
    )
}

fn memory_stream(capacity: usize) -> (RpcStream<MemorySend, MemoryRecv>, MemoryPeer) {
    let (send, recv, remote) = memory_halves(capacity);
    (RpcStream::new(send, recv), remote)
}

fn poll<F: Future>(future: Pin<&mut F>) -> Poll<F::Output> {
    future.poll(&mut Context::from_waker(Waker::noop()))
}

fn device() -> Device {
    Device {
        id: "test-peer".into(),
        name: "test".into(),
        role: Role::Desktop,
        capabilities: Vec::new(),
        addr: None,
        paired_at: 0,
    }
}

fn request(method: &str, params: Value) -> Request {
    Request {
        jsonrpc: "2.0".into(),
        id: json!(1),
        method: method.into(),
        params,
    }
}

struct Handler {
    calls: AtomicUsize,
    delay: Duration,
    result: Value,
}

impl Default for Handler {
    fn default() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            delay: Duration::ZERO,
            result: Value::Null,
        }
    }
}

#[async_trait]
impl RpcHandler for Handler {
    async fn handle(
        &self,
        _peer: &Device,
        _method: &str,
        _params: Value,
    ) -> std::result::Result<Value, RpcError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        Ok(self.result.clone())
    }
}

fn assert_aborted(state: &StreamState, code: u32) {
    assert_eq!(state.reset.load(Ordering::SeqCst), code);
    assert_eq!(state.stop.load(Ordering::SeqCst), code);
}

#[tokio::test(start_paused = true)]
async fn stalled_streams_hit_both_caps_and_release_buffers_at_deadline() {
    let global = RpcBudget::new(3, 3 * STREAM_BYTES);
    let local_a = RpcBudget::new(2, 2 * STREAM_BYTES);
    let local_b = RpcBudget::new(2, 2 * STREAM_BYTES);
    let peer = device();
    let handler = Handler::default();
    let deadline = deadline(Duration::from_secs(10)).unwrap();
    let mut held = Vec::new();
    let mut remotes = Vec::new();
    for local in [&local_a, &local_a, &local_b] {
        let (stream, mut remote) = memory_stream(128 * 1024);
        // The reader consumes a real partial frame and retains a buffer.
        remote
            .writer
            .write_all(&u32::try_from(MAX_RPC_REQUEST).unwrap().to_be_bytes())
            .await
            .unwrap();
        remote
            .writer
            .write_all(&vec![b'x'; 64 * 1024])
            .await
            .unwrap();
        let (stream, admission) = admit(stream, local, &global).unwrap();
        let mut work = Box::pin(handle_stream(stream, &peer, &handler, deadline, admission));
        assert!(poll(work.as_mut()).is_pending());
        held.push(work);
        remotes.push(remote);
    }
    for local in [&local_a, &local_b] {
        let (stream, remote) = memory_stream(16);
        assert!(admit(stream, local, &global).is_none());
        assert_aborted(&remote.state, STOP_BUSY);
        assert_eq!(remote.state.written.load(Ordering::SeqCst), 0);
    }
    assert_eq!(global.bytes.available_permits(), 0);
    tokio::time::advance(Duration::from_secs(10)).await;
    for mut work in held {
        assert!(
            poll(work.as_mut()).is_ready(),
            "partial frame survived its deadline"
        );
    }
    for remote in remotes {
        assert_aborted(&remote.state, STOP_CANCELLED);
    }
    assert_eq!(handler.calls.load(Ordering::SeqCst), 0);
    assert_eq!(global.slots.available_permits(), 3);
    assert_eq!(global.bytes.available_permits(), 3 * STREAM_BYTES);
    assert_eq!(local_a.bytes.available_permits(), 2 * STREAM_BYTES);
    assert_eq!(local_b.bytes.available_permits(), 2 * STREAM_BYTES);
    let (stream, _remote) = memory_stream(16);
    assert!(admit(stream, &local_a, &global).is_some());
}

#[tokio::test]
async fn byte_budgets_apply_before_spawning_even_with_free_slots() {
    let global = RpcBudget::new(4, STREAM_BYTES);
    let local_a = RpcBudget::new(4, STREAM_BYTES);
    let local_b = RpcBudget::new(4, 4 * STREAM_BYTES);
    let (stream, _remote) = memory_stream(16);
    let first = admit(stream, &local_a, &global).unwrap();
    for local in [&local_a, &local_b] {
        let (stream, remote) = memory_stream(16);
        assert!(admit(stream, local, &global).is_none());
        assert_aborted(&remote.state, STOP_BUSY);
    }
    assert_eq!(local_a.slots.available_permits(), 3);
    assert_eq!(local_b.slots.available_permits(), 4);
    drop(first);
    assert_eq!(global.bytes.available_permits(), STREAM_BYTES);
    assert_eq!(global.slots.available_permits(), 4);
}

#[tokio::test(start_paused = true)]
async fn server_uses_one_deadline_for_read_handle_and_nonreading_peer() {
    let local = RpcBudget::new(1, STREAM_BYTES);
    let global = RpcBudget::new(1, STREAM_BYTES);
    let peer = device();
    let handler = Handler {
        delay: Duration::from_secs(4),
        result: json!("x".repeat(4096)),
        ..Handler::default()
    };
    let (stream, mut remote) = memory_stream(1024);
    let (stream, admission) = admit(stream, &local, &global).unwrap();
    let mut work = Box::pin(handle_stream(
        stream,
        &peer,
        &handler,
        deadline(Duration::from_secs(10)).unwrap(),
        admission,
    ));
    assert!(poll(work.as_mut()).is_pending());
    tokio::time::advance(Duration::from_secs(3)).await;
    write_frame(
        &mut remote.writer,
        &serde_json::to_vec(&request("test", Value::Null)).unwrap(),
        MAX_RPC_REQUEST,
    )
    .await
    .unwrap();
    assert!(poll(work.as_mut()).is_pending());
    assert_eq!(handler.calls.load(Ordering::SeqCst), 1);
    tokio::time::advance(Duration::from_secs(4)).await;
    assert!(poll(work.as_mut()).is_pending());
    assert!(remote.state.written.load(Ordering::SeqCst) > 0);
    // The peer never drains its response. The original ten seconds, not a
    // fresh timeout after the handler, must govern this blocked write.
    tokio::time::advance(Duration::from_secs(3)).await;
    assert!(poll(work.as_mut()).is_ready());
    assert_aborted(&remote.state, STOP_CANCELLED);
    assert_eq!(global.bytes.available_permits(), STREAM_BYTES);
}

#[tokio::test(start_paused = true)]
async fn server_delivery_ack_also_has_a_deadline() {
    let local = RpcBudget::new(1, STREAM_BYTES);
    let global = RpcBudget::new(1, STREAM_BYTES);
    let peer = device();
    let handler = Handler::default();
    let (stream, mut remote) = memory_stream(1024);
    write_frame(
        &mut remote.writer,
        &serde_json::to_vec(&request("test", Value::Null)).unwrap(),
        MAX_RPC_REQUEST,
    )
    .await
    .unwrap();
    let (stream, admission) = admit(stream, &local, &global).unwrap();
    let mut work = Box::pin(handle_stream(
        stream,
        &peer,
        &handler,
        deadline(Duration::from_secs(5)).unwrap(),
        admission,
    ));
    assert!(poll(work.as_mut()).is_pending());
    assert_eq!(remote.state.finished.load(Ordering::SeqCst), 1);
    tokio::time::advance(Duration::from_secs(5)).await;
    assert!(poll(work.as_mut()).is_ready());
    assert_aborted(&remote.state, STOP_CANCELLED);
}

#[tokio::test(start_paused = true)]
async fn method_caps_reject_before_the_handler() {
    let local = RpcBudget::new(1, STREAM_BYTES);
    let global = RpcBudget::new(1, STREAM_BYTES);
    let peer = device();
    let handler = Handler::default();
    let (stream, mut remote) = memory_stream(16 * 1024);
    let oversized = request("node.info", json!("x".repeat(4096)));
    write_frame(
        &mut remote.writer,
        &serde_json::to_vec(&oversized).unwrap(),
        MAX_RPC_REQUEST,
    )
    .await
    .unwrap();
    let (stream, admission) = admit(stream, &local, &global).unwrap();
    let mut work = Box::pin(handle_stream(
        stream,
        &peer,
        &handler,
        deadline(Duration::from_secs(10)).unwrap(),
        admission,
    ));
    assert!(poll(work.as_mut()).is_pending());
    let bytes = read_frame(&mut remote.reader, MAX_FRAME)
        .await
        .unwrap()
        .unwrap();
    let response: Response = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(response.error.unwrap().code, ERR_INVALID_PARAMS);
    assert_eq!(handler.calls.load(Ordering::SeqCst), 0);
    remote.stopped.cancel();
    assert!(poll(work.as_mut()).is_ready());
    assert_eq!(remote.state.reset.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn precancelled_call_never_opens_or_writes() {
    let (send, recv, remote) = memory_halves(1024);
    let opens = AtomicUsize::new(0);
    let cancel = CancellationToken::new();
    cancel.cancel();
    let result = exchange(
        request("test", Value::Null),
        deadline(Duration::from_secs(10)).unwrap(),
        &cancel,
        async {
            opens.fetch_add(1, Ordering::SeqCst);
            Ok((send, recv))
        },
    )
    .await;
    assert!(matches!(result, Err(PeerError::Cancelled)));
    assert_eq!(opens.load(Ordering::SeqCst), 0);
    assert_eq!(remote.state.written.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn client_deadline_includes_connection_lock_dial_and_open() {
    let (send, recv, remote) = memory_halves(1024);
    let connection_lock = Mutex::new(());
    let guard = connection_lock.lock().await;
    let phases = AtomicUsize::new(0);
    let cancel = CancellationToken::new();
    let mut work = Box::pin(exchange(
        request("test", Value::Null),
        deadline(Duration::from_secs(10)).unwrap(),
        &cancel,
        async {
            let _guard = connection_lock.lock().await;
            phases.store(1, Ordering::SeqCst);
            // Simulate dial then open_bi under the same lazy-open future
            // used by RpcClient (no actual sockets or DNS in this test).
            tokio::time::sleep(Duration::from_secs(4)).await;
            phases.store(2, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_secs(4)).await;
            phases.store(3, Ordering::SeqCst);
            Ok((send, recv))
        },
    ));
    assert!(poll(work.as_mut()).is_pending());
    tokio::time::advance(Duration::from_secs(3)).await;
    drop(guard);
    assert!(poll(work.as_mut()).is_pending());
    assert_eq!(phases.load(Ordering::SeqCst), 1);
    tokio::time::advance(Duration::from_secs(4)).await;
    assert!(poll(work.as_mut()).is_pending());
    assert_eq!(phases.load(Ordering::SeqCst), 2);
    tokio::time::advance(Duration::from_secs(3)).await;
    assert!(matches!(
        poll(work.as_mut()),
        Poll::Ready(Err(PeerError::Timeout))
    ));
    assert_eq!(phases.load(Ordering::SeqCst), 2);
    assert_eq!(remote.state.written.load(Ordering::SeqCst), 0);
    assert!(connection_lock.try_lock().is_ok());
}

#[tokio::test(start_paused = true)]
async fn cancellation_interrupts_connection_wait_without_writing() {
    let (send, recv, remote) = memory_halves(1024);
    let connection_lock = Mutex::new(());
    let _guard = connection_lock.lock().await;
    let cancel = CancellationToken::new();
    let mut work = Box::pin(exchange(
        request("test", Value::Null),
        deadline(Duration::from_secs(10)).unwrap(),
        &cancel,
        async {
            let _guard = connection_lock.lock().await;
            Ok((send, recv))
        },
    ));
    assert!(poll(work.as_mut()).is_pending());
    cancel.cancel();
    assert!(matches!(
        poll(work.as_mut()),
        Poll::Ready(Err(PeerError::Cancelled))
    ));
    assert_eq!(remote.state.written.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn nonreading_peer_times_out_and_both_halves_are_reset() {
    // Capacity one stalls during the four-byte header, before response read.
    let (send, recv, remote) = memory_halves(1);
    let cancel = CancellationToken::new();
    let mut work = Box::pin(exchange(
        request("test", Value::Null),
        deadline(Duration::from_secs(10)).unwrap(),
        &cancel,
        async { Ok((send, recv)) },
    ));
    assert!(poll(work.as_mut()).is_pending());
    assert_eq!(remote.state.written.load(Ordering::SeqCst), 1);
    tokio::time::advance(Duration::from_secs(10)).await;
    assert!(matches!(
        poll(work.as_mut()),
        Poll::Ready(Err(PeerError::Timeout))
    ));
    assert_aborted(&remote.state, STOP_CANCELLED);
}

#[tokio::test(start_paused = true)]
async fn cancellation_and_dropping_call_reset_both_halves() {
    for cancel_explicitly in [true, false] {
        let (send, recv, remote) = memory_halves(1024);
        let cancel = CancellationToken::new();
        let mut work = Box::pin(exchange(
            request("test", Value::Null),
            deadline(Duration::from_secs(10)).unwrap(),
            &cancel,
            async { Ok((send, recv)) },
        ));
        assert!(poll(work.as_mut()).is_pending());
        assert_eq!(remote.state.finished.load(Ordering::SeqCst), 1);
        if cancel_explicitly {
            cancel.cancel();
            assert!(matches!(
                poll(work.as_mut()),
                Poll::Ready(Err(PeerError::Cancelled))
            ));
        }
        drop(work);
        assert_aborted(&remote.state, STOP_CANCELLED);
    }
}

#[tokio::test(start_paused = true)]
async fn response_read_uses_time_remaining_after_open() {
    let (send, recv, remote) = memory_halves(1024);
    let cancel = CancellationToken::new();
    let mut work = Box::pin(exchange(
        request("test", Value::Null),
        deadline(Duration::from_secs(10)).unwrap(),
        &cancel,
        async {
            tokio::time::sleep(Duration::from_secs(8)).await;
            Ok((send, recv))
        },
    ));
    assert!(poll(work.as_mut()).is_pending());
    tokio::time::advance(Duration::from_secs(8)).await;
    assert!(poll(work.as_mut()).is_pending());
    assert_eq!(remote.state.finished.load(Ordering::SeqCst), 1);
    tokio::time::advance(Duration::from_secs(2)).await;
    assert!(matches!(
        poll(work.as_mut()),
        Poll::Ready(Err(PeerError::Timeout))
    ));
    assert_aborted(&remote.state, STOP_CANCELLED);
}

#[tokio::test]
async fn request_and_frame_caps_are_enforced_without_network() {
    let (send, recv, remote) = memory_halves(1024);
    let opens = AtomicUsize::new(0);
    let result = exchange(
        request("node.ping", json!("x".repeat(4096))),
        deadline(Duration::from_secs(10)).unwrap(),
        &CancellationToken::new(),
        async {
            opens.fetch_add(1, Ordering::SeqCst);
            Ok((send, recv))
        },
    )
    .await;
    assert!(matches!(result, Err(PeerError::Protocol(_))));
    assert_eq!(opens.load(Ordering::SeqCst), 0);
    assert_eq!(remote.state.written.load(Ordering::SeqCst), 0);
    let mut frame = u32::try_from(MAX_RPC_REQUEST + 1)
        .unwrap()
        .to_be_bytes()
        .as_slice()
        .to_vec();
    assert!(matches!(
        read_frame(&mut frame.as_slice(), MAX_RPC_REQUEST).await,
        Err(PeerError::Protocol(_))
    ));
    // EOF is only clean before the first header byte.
    frame.clear();
    assert!(
        read_frame(&mut frame.as_slice(), MAX_RPC_REQUEST)
            .await
            .unwrap()
            .is_none()
    );
    frame.push(0);
    assert!(
        read_frame(&mut frame.as_slice(), MAX_RPC_REQUEST)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn successful_exchange_does_not_reset_streams() {
    let (send, recv, mut remote) = memory_halves(1024);
    write_frame(
        &mut remote.writer,
        &serde_json::to_vec(&Response::ok(json!(1), json!("ok"))).unwrap(),
        MAX_FRAME,
    )
    .await
    .unwrap();
    let value = exchange(
        request("test", Value::Null),
        deadline(Duration::from_secs(10)).unwrap(),
        &CancellationToken::new(),
        async { Ok((send, recv)) },
    )
    .await
    .unwrap();
    assert_eq!(value, json!("ok"));
    assert_eq!(remote.state.reset.load(Ordering::SeqCst), 0);
    assert_eq!(remote.state.stop.load(Ordering::SeqCst), 0);
    assert!(remote.reader.read_u32().await.unwrap() > 0);
}

#[test]
fn invalid_limits_fail_closed() {
    assert!(RpcLimits::default().validate().is_ok());
    assert!(
        RpcLimits {
            streams_total: 0,
            ..RpcLimits::default()
        }
        .validate()
        .is_err()
    );
    assert!(
        RpcLimits {
            bytes_per_connection: STREAM_BYTES - 1,
            ..RpcLimits::default()
        }
        .validate()
        .is_err()
    );
    assert!(
        RpcLimits {
            bytes_total: usize::MAX,
            ..RpcLimits::default()
        }
        .validate()
        .is_err()
    );
}
