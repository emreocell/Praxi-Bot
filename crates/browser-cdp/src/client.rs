// Adapted from vercel-labs/agent-browser (Apache-2.0): raw CDP over tokio-tungstenite.
//! Minimal CDP client: request/response by id, event broadcast, flattened-session multiplexing.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use std::{borrow::Cow, collections::HashMap};

use futures_util::stream::SplitSink;
use futures_util::{Sink as MessageSink, SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json, value::RawValue};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, broadcast, oneshot};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Error as WsError;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::error::{Error, Result};

type Pending = Arc<StdMutex<HashMap<u64, oneshot::Sender<Result<Value>>>>>;
type Sink = SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>;

/// Default per-command timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Transport and retained-message limits for a CDP connection.
///
/// A reply above `max_reply_bytes` but within the hard wire limits fails only
/// its command. A frame/message above the hard limits closes the transport:
/// its request id cannot be recovered safely from an incomplete frame.
#[derive(Debug, Clone)]
pub struct CdpClientConfig {
    /// Maximum time for TCP/TLS connection and websocket handshake together.
    pub connect_timeout: Duration,
    /// Default absolute deadline for lock acquisition, write and response together.
    pub command_timeout: Duration,
    /// Hard maximum websocket message size (including fragmented messages).
    pub max_message_bytes: usize,
    /// Hard maximum websocket frame payload size.
    pub max_frame_bytes: usize,
    /// Maximum serialized reply size before parsing its result/error body.
    pub max_reply_bytes: usize,
    /// Maximum serialized event size before parsing/retaining its parameters.
    pub max_event_bytes: usize,
    /// Upper bound on serialized event bytes retained by the broadcast ring.
    /// The slot count is rounded down to a power of two and capped at 4096;
    /// allocator/JSON representation overhead and consumer-owned copies are extra.
    pub event_buffer_bytes: usize,
}

impl Default for CdpClientConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(10),
            command_timeout: DEFAULT_TIMEOUT,
            max_message_bytes: 64 * 1024 * 1024,
            max_frame_bytes: 64 * 1024 * 1024,
            max_reply_bytes: 32 * 1024 * 1024,
            max_event_bytes: 64 * 1024,
            event_buffer_bytes: 16 * 1024 * 1024,
        }
    }
}

impl CdpClientConfig {
    fn event_capacity(&self) -> Result<usize> {
        if self.max_frame_bytes == 0
            || self.max_frame_bytes > self.max_message_bytes
            || self.max_reply_bytes == 0
            || self.max_reply_bytes > self.max_message_bytes
            || self.max_event_bytes == 0
            || self.max_event_bytes > self.max_reply_bytes
            || self.event_buffer_bytes < self.max_event_bytes
            || self.max_message_bytes > usize::MAX - 128 * 1024 - 14
        {
            return Err(Error::WebSocket(
                "invalid CDP byte-limit configuration".into(),
            ));
        }
        let slots = (self.event_buffer_bytes / self.max_event_bytes).min(4096);
        let rounded = slots.next_power_of_two();
        // Tokio's broadcast channel rounds capacity UP. Round DOWN first so
        // its actual capacity times the largest accepted event fits the budget.
        Ok(if rounded > slots {
            rounded / 2
        } else {
            rounded
        })
    }
}

/// Registration owned by the command future, including while waiting for the
/// writer lock. Cancellation, timeout, send error and normal completion all drop it.
struct PendingGuard {
    pending: Pending,
    id: u64,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        if let Ok(mut entries) = self.pending.lock() {
            entries.remove(&self.id);
        }
    }
}

/// A CDP event (a message without an `id`).
#[derive(Debug, Clone)]
pub struct CdpEvent {
    /// Method name, e.g. `Page.loadEventFired`.
    pub method: String,
    /// Event parameters.
    pub params: Value,
    /// Flattened session the event belongs to (`None` for the browser session).
    pub session_id: Option<String>,
}

/// A connection to a CDP websocket endpoint shared by all sessions of a browser.
pub struct CdpClient {
    sink: Mutex<Sink>,
    next_id: AtomicU64,
    pending: Pending,
    events: broadcast::Sender<CdpEvent>,
    closed: Arc<AtomicBool>,
    reader: JoinHandle<()>,
    limits: CdpClientConfig,
    rejected_events: Arc<AtomicU64>,
}

impl CdpClient {
    /// Connect to a `ws://` debugging endpoint.
    pub async fn connect(url: &str) -> Result<Arc<Self>> {
        Self::connect_with_config(url, CdpClientConfig::default()).await
    }

    /// Connect with explicit finite transport/message limits and deadlines.
    pub async fn connect_with_config(url: &str, limits: CdpClientConfig) -> Result<Arc<Self>> {
        let event_capacity = limits.event_capacity()?;
        let write_buffer = (128 * 1024).min(limits.max_message_bytes);
        let config = WebSocketConfig::default()
            .read_buffer_size((128 * 1024).min(limits.max_frame_bytes))
            .write_buffer_size(write_buffer)
            .max_write_buffer_size(limits.max_message_bytes + write_buffer + 14)
            .max_message_size(Some(limits.max_message_bytes))
            .max_frame_size(Some(limits.max_frame_bytes));
        let (ws, _) = tokio::time::timeout(
            limits.connect_timeout,
            tokio_tungstenite::connect_async_with_config(url, Some(config), false),
        )
        .await
        .map_err(|_| Error::Timeout(format!("CDP connect after {:?}", limits.connect_timeout)))?
        .map_err(|e| Error::WebSocket(e.to_string()))?;
        let (sink, mut stream) = ws.split();
        let pending: Pending = Arc::new(StdMutex::new(HashMap::new()));
        let (events, _) = broadcast::channel(event_capacity);
        let closed = Arc::new(AtomicBool::new(false));
        let rejected_events = Arc::new(AtomicU64::new(0));

        let reader = {
            let pending = pending.clone();
            let events = events.clone();
            let closed = closed.clone();
            let limits = limits.clone();
            let rejected_events = rejected_events.clone();
            tokio::spawn(async move {
                let mut transport_error = None;
                while let Some(msg) = stream.next().await {
                    let text = match msg {
                        Ok(Message::Text(t)) => t,
                        // In tungstenite, an over-limit frame leaves the unread
                        // frame header in place. Retrying cannot resynchronize it.
                        Err(error) => {
                            transport_error = Some(error.to_string());
                            break;
                        }
                        Ok(Message::Close(_)) => break,
                        Ok(_) => continue,
                    };
                    if matches!(
                        dispatch(text.as_str(), &pending, &events, &limits),
                        Err(Error::MessageTooLarge { kind: "event", .. })
                    ) {
                        rejected_events.fetch_add(1, Ordering::Relaxed);
                    }
                }
                closed.store(true, Ordering::SeqCst);
                let drained: Vec<_> = pending
                    .lock()
                    .map(|mut p| p.drain().collect())
                    .unwrap_or_default();
                for (_, tx) in drained {
                    let error = transport_error
                        .as_ref()
                        .map_or(Error::Closed, |message| Error::WebSocket(message.clone()));
                    let _ = tx.send(Err(error));
                }
            })
        };

        Ok(Arc::new(Self {
            sink: Mutex::new(sink),
            next_id: AtomicU64::new(1),
            pending,
            events,
            closed,
            reader,
            limits,
            rejected_events,
        }))
    }

    /// Subscribe to all events (every session). Subscribe *before* sending the command that triggers them.
    pub fn subscribe(&self) -> broadcast::Receiver<CdpEvent> {
        self.events.subscribe()
    }

    /// Events discarded because they exceeded the configured per-event byte cap.
    /// Queue eviction is reported separately by each receiver's `Lagged` error.
    pub fn rejected_event_count(&self) -> u64 {
        self.rejected_events.load(Ordering::Relaxed)
    }

    /// True once the websocket has closed.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// Send a command on the browser session.
    pub async fn send(&self, method: &str, params: Value) -> Result<Value> {
        self.send_on(None, method, params).await
    }

    /// Send a command on a flattened session (or the browser session when `None`).
    pub async fn send_on(
        &self,
        session: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Value> {
        self.send_with_timeout(session, method, params, self.limits.command_timeout)
            .await
    }

    /// Like [`CdpClient::send_on`] with an explicit timeout.
    pub async fn send_with_timeout(
        &self,
        session: Option<&str>,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| Error::Timeout("CDP command deadline is out of range".into()))?;
        if self.is_closed() {
            return Err(Error::Closed);
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let mut msg = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = session {
            msg["sessionId"] = json!(s);
        }
        let text = msg.to_string();
        if text.len() > self.limits.max_message_bytes {
            return Err(Error::MessageTooLarge {
                kind: "request",
                size: text.len(),
                limit: self.limits.max_message_bytes,
            });
        }
        let (tx, rx) = oneshot::channel();
        {
            let mut entries = self.pending.lock().map_err(|_| Error::Closed)?;
            // The reader sets closed before draining under this same mutex.
            if self.is_closed() {
                return Err(Error::Closed);
            }
            entries.insert(id, tx);
        }
        let _registration = PendingGuard {
            pending: self.pending.clone(),
            id,
        };
        exchange(&self.sink, Message::text(text), rx, deadline)
            .await
            .map_err(|_| Error::Timeout(format!("{method} after {timeout:?}")))?
    }
}

/// One deadline covers queueing behind another writer, socket backpressure and
/// the reply. Generic over the sink so blocked writes can be tested without OS
/// socket-buffer sizes or wall-clock timing assumptions.
async fn exchange<S: MessageSink<Message, Error = WsError> + Unpin>(
    sink: &Mutex<S>,
    message: Message,
    response: oneshot::Receiver<Result<Value>>,
    deadline: Instant,
) -> std::result::Result<Result<Value>, tokio::time::error::Elapsed> {
    tokio::time::timeout_at(deadline, async {
        let mut writer = sink.lock().await;
        writer
            .send(message)
            .await
            .map_err(|error| Error::WebSocket(error.to_string()))?;
        drop(writer);
        response.await.unwrap_or(Err(Error::Closed))
    })
    .await
}

impl Drop for CdpClient {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

#[derive(Deserialize)]
struct Envelope<'a> {
    id: Option<u64>,
    #[serde(borrow)]
    method: Option<Cow<'a, str>>,
    #[serde(rename = "sessionId", borrow)]
    session_id: Option<Cow<'a, str>>,
    // Scan envelopes without materializing potentially enormous result/params
    // trees. The RawValue borrows the websocket message until size checks pass.
    #[serde(borrow)]
    result: Option<&'a RawValue>,
    #[serde(borrow)]
    params: Option<&'a RawValue>,
    #[serde(borrow)]
    error: Option<&'a RawValue>,
}

fn dispatch(
    text: &str,
    pending: &Pending,
    events: &broadcast::Sender<CdpEvent>,
    limits: &CdpClientConfig,
) -> Result<()> {
    let message: Envelope<'_> = serde_json::from_str(text)?;
    if let Some(id) = message.id {
        let tx = pending.lock().ok().and_then(|mut p| p.remove(&id));
        if let Some(tx) = tx {
            let res = if text.len() > limits.max_reply_bytes {
                Err(Error::MessageTooLarge {
                    kind: "reply",
                    size: text.len(),
                    limit: limits.max_reply_bytes,
                })
            } else {
                parse_reply(&message)
            };
            let _ = tx.send(res);
        }
    } else if let Some(method) = message.method {
        if text.len() > limits.max_event_bytes {
            return Err(Error::MessageTooLarge {
                kind: "event",
                size: text.len(),
                limit: limits.max_event_bytes,
            });
        }
        let _ = events.send(CdpEvent {
            method: method.into_owned(),
            params: message
                .params
                .map_or(Ok(Value::Null), |raw| serde_json::from_str(raw.get()))?,
            session_id: message.session_id.map(Cow::into_owned),
        });
    }
    Ok(())
}

fn parse_reply(message: &Envelope<'_>) -> Result<Value> {
    if let Some(raw) = message.error {
        let mut error: Value = serde_json::from_str(raw.get())?;
        let code = error.get("code").and_then(Value::as_i64).unwrap_or(-1);
        let message = match error.get_mut("message").map(Value::take) {
            Some(Value::String(message)) => message,
            _ => "unknown".into(),
        };
        return Err(Error::Protocol { code, message });
    }
    message.result.map_or(Ok(Value::Null), |raw| {
        serde_json::from_str(raw.get()).map_err(Error::from)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spawn a tiny websocket server; `handler` maps a request to messages to send back.
    async fn server(handler: fn(&Value) -> Vec<Value>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (s, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(s).await.unwrap();
            while let Some(Ok(Message::Text(t))) = ws.next().await {
                let req: Value = serde_json::from_str(t.as_str()).unwrap();
                for m in handler(&req) {
                    ws.send(Message::text(m.to_string())).await.unwrap();
                }
            }
        });
        url
    }

    #[tokio::test]
    async fn request_response_and_events() {
        let url = server(|req| {
            let sid = req.get("sessionId").cloned().unwrap_or(Value::Null);
            vec![
                json!({"method": "Test.event", "params": {"n": 1}, "sessionId": "S1"}),
                json!({"id": req["id"], "result": {"echo": req["method"], "session": sid}}),
            ]
        })
        .await;
        let c = CdpClient::connect(&url).await.unwrap();
        let mut rx = c.subscribe();
        let r = c.send_on(Some("S1"), "Foo.bar", json!({})).await.unwrap();
        assert_eq!(r["echo"], "Foo.bar");
        assert_eq!(r["session"], "S1");
        let ev = rx.recv().await.unwrap();
        assert_eq!(ev.method, "Test.event");
        assert_eq!(ev.session_id.as_deref(), Some("S1"));
    }

    #[tokio::test]
    async fn audit_rel11_oversized_event_is_not_retained() {
        let url = server(|req| {
            vec![
                json!({"method": "Test.large", "params": {"text": "x".repeat(512 * 1024)}}),
                json!({"id": req["id"], "result": {}}),
            ]
        })
        .await;
        let client = CdpClient::connect(&url).await.unwrap();
        let mut events = client.subscribe();
        // The reply follows the event on the wire, so receipt proves that the
        // reader has already processed (and should have rejected) the event.
        client.send("Trigger", json!({})).await.unwrap();
        assert!(matches!(
            events.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
        assert_eq!(client.rejected_event_count(), 1);
    }

    #[tokio::test]
    async fn audit_rel12_dropped_send_removes_pending_entry() {
        use futures_util::FutureExt;

        let url = server(|_| vec![]).await;
        let client = CdpClient::connect(&url).await.unwrap();
        let sink = client.sink.lock().await;
        let mut request = Box::pin(client.send("Blocked", json!({})));
        assert!(request.as_mut().now_or_never().is_none());
        assert_eq!(client.pending.lock().unwrap().len(), 1);
        drop(request);
        assert_eq!(client.pending.lock().unwrap().len(), 0);
        drop(sink);
    }

    fn small_limits() -> CdpClientConfig {
        CdpClientConfig {
            max_message_bytes: 8192,
            max_frame_bytes: 8192,
            max_reply_bytes: 1024,
            max_event_bytes: 256,
            event_buffer_bytes: 1500,
            ..CdpClientConfig::default()
        }
    }

    #[tokio::test]
    async fn oversized_reply_fails_only_its_call_and_next_succeeds() {
        let url = server(|req| {
            let result = if req["method"] == "Large" {
                json!({"text": "x".repeat(2048)})
            } else {
                json!({"ok": true})
            };
            vec![json!({"id": req["id"], "result": result})]
        })
        .await;
        let client = CdpClient::connect_with_config(&url, small_limits())
            .await
            .unwrap();
        let error = client.send("Large", json!({})).await.unwrap_err();
        assert!(matches!(
            error,
            Error::MessageTooLarge {
                kind: "reply",
                limit: 1024,
                ..
            }
        ));
        assert!(!error.is_fatal());
        assert_eq!(client.pending.lock().unwrap().len(), 0);
        assert!(!client.is_closed());
        assert_eq!(client.send("Next", json!({})).await.unwrap()["ok"], true);
    }

    #[tokio::test]
    async fn oversized_wire_frame_fails_pending_and_closes_connection() {
        let url = server(|req| {
            vec![json!({
                "id": req["id"], "result": {"text": "x".repeat(2048)}
            })]
        })
        .await;
        let mut limits = small_limits();
        limits.max_frame_bytes = 1024;
        let client = CdpClient::connect_with_config(&url, limits).await.unwrap();
        let error = client.send("Large", json!({})).await.unwrap_err();
        assert!(
            matches!(error, Error::WebSocket(ref message) if message.contains("Message too long"))
        );
        assert!(client.is_closed());
        assert_eq!(client.pending.lock().unwrap().len(), 0);
    }

    #[test]
    fn event_flood_respects_byte_budget_after_channel_rounding() {
        let limits = small_limits();
        let capacity = limits.event_capacity().unwrap();
        assert_eq!(capacity, 4); // 1500 / 256 = 5 slots, rounded DOWN to 4.
        let pending = Arc::new(StdMutex::new(HashMap::new()));
        let (events, mut receiver) = broadcast::channel(capacity);
        for index in 0..1000 {
            let message =
                json!({"method": "Test.event", "params": {"index": index, "text": "x".repeat(64)}})
                    .to_string();
            dispatch(&message, &pending, &events, &limits).unwrap();
            assert!(events.len() * limits.max_event_bytes <= limits.event_buffer_bytes);
        }
        assert!(matches!(
            receiver.try_recv(),
            Err(broadcast::error::TryRecvError::Lagged(996))
        ));
        for index in 996..1000 {
            assert_eq!(receiver.try_recv().unwrap().params["index"], index);
        }
        assert!(matches!(
            receiver.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn oversized_outgoing_command_does_not_register_or_break_connection() {
        let url = server(|req| vec![json!({"id": req["id"], "result": {"ok": true}})]).await;
        let client = CdpClient::connect_with_config(&url, small_limits())
            .await
            .unwrap();
        let error = client
            .send("Large", json!({"text": "x".repeat(8192)}))
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            Error::MessageTooLarge {
                kind: "request",
                ..
            }
        ));
        assert_eq!(client.pending.lock().unwrap().len(), 0);
        assert_eq!(client.send("Next", json!({})).await.unwrap()["ok"], true);
    }

    #[test]
    fn invalid_limits_fail_before_channel_allocation_or_connect() {
        let mut limits = small_limits();
        limits.max_event_bytes = 0;
        assert!(limits.event_capacity().is_err());
        limits = small_limits();
        limits.event_buffer_bytes = 255;
        assert!(limits.event_capacity().is_err());
        limits = small_limits();
        limits.max_frame_bytes = limits.max_message_bytes + 1;
        assert!(limits.event_capacity().is_err());
    }

    #[derive(Default)]
    struct TestSink {
        block_flush: bool,
        writes: usize,
    }

    impl MessageSink<Message> for TestSink {
        type Error = WsError;

        fn poll_ready(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::result::Result<(), WsError>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn start_send(
            mut self: std::pin::Pin<&mut Self>,
            _: Message,
        ) -> std::result::Result<(), WsError> {
            self.writes += 1;
            Ok(())
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::result::Result<(), WsError>> {
            if self.block_flush {
                std::task::Poll::Pending
            } else {
                std::task::Poll::Ready(Ok(()))
            }
        }

        fn poll_close(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::result::Result<(), WsError>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    #[tokio::test(start_paused = true)]
    async fn blocked_socket_write_uses_deadline_and_releases_writer() {
        use futures_util::FutureExt;

        let sink = Mutex::new(TestSink {
            block_flush: true,
            writes: 0,
        });
        let (_sender, receiver) = oneshot::channel();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut request = Box::pin(exchange(&sink, Message::text("{}"), receiver, deadline));
        assert!(request.as_mut().now_or_never().is_none());
        tokio::time::advance(Duration::from_secs(5)).await;
        assert!(request.await.is_err());
        assert_eq!(sink.try_lock().unwrap().writes, 1);
        assert_eq!(Instant::now(), deadline);
    }

    #[tokio::test(start_paused = true)]
    async fn writer_lock_and_reply_share_one_absolute_deadline() {
        use futures_util::FutureExt;

        let sink = Mutex::new(TestSink::default());
        let held = sink.lock().await;
        let (_sender, receiver) = oneshot::channel();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut request = Box::pin(exchange(&sink, Message::text("{}"), receiver, deadline));
        assert!(request.as_mut().now_or_never().is_none());
        tokio::time::advance(Duration::from_secs(4)).await;
        drop(held);
        assert!(request.as_mut().now_or_never().is_none());
        assert_eq!(sink.try_lock().unwrap().writes, 1);
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(request.await.is_err());
        assert_eq!(Instant::now(), deadline);
    }

    #[tokio::test]
    async fn writer_lock_timeout_cleans_the_real_pending_map() {
        use futures_util::FutureExt;

        let url = server(|_| vec![]).await;
        let client = CdpClient::connect(&url).await.unwrap();
        let held = client.sink.lock().await;
        tokio::time::pause();
        let mut request =
            Box::pin(client.send_with_timeout(None, "Blocked", json!({}), Duration::from_secs(5)));
        assert!(request.as_mut().now_or_never().is_none());
        assert_eq!(client.pending.lock().unwrap().len(), 1);
        tokio::time::advance(Duration::from_secs(5)).await;
        assert!(matches!(request.await, Err(Error::Timeout(_))));
        assert_eq!(client.pending.lock().unwrap().len(), 0);
        drop(held);
    }

    #[tokio::test]
    async fn stalled_websocket_handshake_has_a_connect_deadline() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let (accepted, wait_accepted) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (_socket, _) = listener.accept().await.unwrap();
            accepted.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        let request = tokio::spawn(async move {
            CdpClient::connect_with_config(
                &url,
                CdpClientConfig {
                    connect_timeout: Duration::from_secs(10),
                    ..small_limits()
                },
            )
            .await
        });
        // Pause only after the loopback connection is accepted. No race between
        // virtual timer auto-advance and establishing a real network connection.
        wait_accepted.await.unwrap();
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(10)).await;
        assert!(matches!(request.await.unwrap(), Err(Error::Timeout(_))));
        server.abort();
    }

    #[tokio::test]
    async fn oversized_message_does_not_kill_connection() {
        let url = server(|req| {
            let big = "x".repeat(20 * 1024 * 1024);
            vec![json!({"id": req["id"], "result": {"big": big}})]
        })
        .await;
        let c = CdpClient::connect(&url).await.unwrap();
        let r = c.send("Page.captureScreenshot", json!({})).await.unwrap();
        assert_eq!(r["big"].as_str().unwrap().len(), 20 * 1024 * 1024);
        assert!(!c.is_closed());
        let r = c.send("Again", json!({})).await.unwrap();
        assert!(r["big"].is_string());
    }

    #[tokio::test]
    async fn protocol_error_and_timeout() {
        let url = server(|req| {
            if req["method"] == "Slow.cmd" {
                vec![]
            } else {
                vec![json!({"id": req["id"], "error": {"code": -32000, "message": "nope"}})]
            }
        })
        .await;
        let c = CdpClient::connect(&url).await.unwrap();
        let e = c.send("A.b", json!({})).await.unwrap_err();
        assert!(matches!(e, Error::Protocol { code: -32000, .. }));
        let e = c
            .send_with_timeout(None, "Slow.cmd", json!({}), Duration::from_millis(100))
            .await
            .unwrap_err();
        assert!(matches!(e, Error::Timeout(_)));
    }

    #[tokio::test]
    async fn out_of_order_responses() {
        let url = server(|req| {
            if req["method"] == "First" {
                vec![]
            } else {
                vec![
                    json!({"id": req["id"], "result": {"who": "second"}}),
                    json!({"id": 1, "result": {"who": "first"}}),
                ]
            }
        })
        .await;
        let c = CdpClient::connect(&url).await.unwrap();
        let c2 = c.clone();
        let a = tokio::spawn(async move { c2.send("First", json!({})).await.unwrap() });
        tokio::time::sleep(Duration::from_millis(50)).await;
        let b = c.send("Second", json!({})).await.unwrap();
        assert_eq!(b["who"], "second");
        assert_eq!(a.await.unwrap()["who"], "first");
    }
}
