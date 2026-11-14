// Adapted from vercel-labs/agent-browser (Apache-2.0): raw CDP over tokio-tungstenite.
//! Minimal CDP client: request/response by id, event broadcast, flattened-session multiplexing.

use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, Weak};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use futures_util::stream::SplitSink;
use futures_util::{Sink as MessageSink, SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json, value::RawValue};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, Notify, broadcast, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Error as WsError;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::error::{Error, Result};

type Pending = Arc<StdMutex<HashMap<u64, oneshot::Sender<Result<Value>>>>>;
type Sink = SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>;
pub(crate) type ViewStopHookFn = dyn Fn() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync;
pub(crate) type ViewStopHook = Arc<ViewStopHookFn>;

const SCREENCAST_METHOD: &str = "Page.screencastFrame";
/// Maximum decoded `Page.screencastFrame` image size (`2 MiB`, matching `live_view::MAX_PAYLOAD`).
pub const DEFAULT_MAX_SCREENCAST_IMAGE_BYTES: usize = live_view::MAX_PAYLOAD;
/// Maximum serialized `Page.screencastFrame` CDP message size (`3 MiB`).
pub const DEFAULT_MAX_SCREENCAST_EVENT_BYTES: usize = 3 * 1024 * 1024;
const MAX_PENDING_DROPPED_ACKS: usize = 16;
const MAX_SCREENCAST_IMAGE_WIDTH: u32 = 1920;
const MAX_SCREENCAST_IMAGE_HEIGHT: u32 = 1080;

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
    /// Maximum serialized `Page.screencastFrame` message size before parsing parameters or decoding.
    pub max_screencast_event_bytes: usize,
    /// Maximum decoded `Page.screencastFrame` image payload size in bytes.
    pub max_screencast_image_bytes: usize,
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
            max_screencast_event_bytes: DEFAULT_MAX_SCREENCAST_EVENT_BYTES,
            max_screencast_image_bytes: DEFAULT_MAX_SCREENCAST_IMAGE_BYTES,
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
            || self.max_screencast_event_bytes == 0
            || self.max_screencast_image_bytes == 0
            || self.max_screencast_image_bytes > self.max_screencast_event_bytes
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

/// Metadata delivered with a CDP `Page.screencastFrame` event.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreencastMetadata {
    /// Top offset of the viewport in DIPs.
    pub offset_top: f64,
    /// Current page scale factor (pinch zoom).
    pub page_scale_factor: f64,
    /// Device/viewport width in DIPs.
    pub device_width: f64,
    /// Device/viewport height in DIPs.
    pub device_height: f64,
    /// Horizontal scroll offset in CSS pixels.
    pub scroll_offset_x: f64,
    /// Vertical scroll offset in CSS pixels.
    pub scroll_offset_y: f64,
    /// Optional compositor timestamp in seconds since epoch.
    pub timestamp: Option<f64>,
}

/// Reason a `Page.screencastFrame` message was rejected before delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ScreencastFault {
    EventTooLarge { size: usize, limit: usize },
    ImageTooLarge { size: usize, limit: usize },
    MalformedFrame(String),
}

#[derive(Debug, Clone)]
pub(crate) struct DecodedScreencastFrame {
    pub jpeg: Arc<[u8]>,
    pub width: u32,
    pub height: u32,
    /// Numeric `params.sessionId` for `Page.screencastFrameAck`, distinct from flattened CDP `sessionId`.
    pub ack_session_id: i64,
    pub metadata: ScreencastMetadata,
}

#[derive(Default)]
struct ScreencastSlotState {
    latest: Option<DecodedScreencastFrame>,
    dropped_acks: VecDeque<i64>,
    last_fault: Option<ScreencastFault>,
    dropped_frames: u64,
    rejected_frames: u64,
}

pub(crate) struct ScreencastSlot {
    state: StdMutex<ScreencastSlotState>,
    notify: Notify,
}

pub(crate) struct ScreencastTake {
    pub frame: Option<DecodedScreencastFrame>,
    pub dropped_acks: Vec<i64>,
    pub fault: Option<ScreencastFault>,
}

impl ScreencastSlot {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: StdMutex::new(ScreencastSlotState::default()),
            notify: Notify::new(),
        })
    }

    pub(crate) fn push_frame(&self, frame: DecodedScreencastFrame) {
        if let Ok(mut state) = self.state.lock() {
            if let Some(prev) = state.latest.replace(frame) {
                state.dropped_frames = state.dropped_frames.saturating_add(1);
                if state.dropped_acks.len() >= MAX_PENDING_DROPPED_ACKS {
                    state.dropped_acks.pop_front();
                }
                state.dropped_acks.push_back(prev.ack_session_id);
            }
            state.last_fault = None;
        }
        self.notify.notify_one();
    }

    pub(crate) fn push_fault(&self, fault: ScreencastFault) {
        if let Ok(mut state) = self.state.lock() {
            state.rejected_frames = state.rejected_frames.saturating_add(1);
            state.last_fault = Some(fault);
        }
        self.notify.notify_one();
    }

    pub(crate) fn notify(&self) {
        self.notify.notify_one();
    }

    pub(crate) async fn notified(&self) {
        self.notify.notified().await;
    }

    pub(crate) fn take(&self) -> ScreencastTake {
        let Ok(mut state) = self.state.lock() else {
            return ScreencastTake {
                frame: None,
                dropped_acks: Vec::new(),
                fault: None,
            };
        };
        ScreencastTake {
            frame: state.latest.take(),
            dropped_acks: state.dropped_acks.drain(..).collect(),
            fault: state.last_fault.take(),
        }
    }

    pub(crate) fn clear(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.latest = None;
            state.dropped_acks.clear();
            state.last_fault = None;
        }
        self.notify.notify_one();
    }
}

type ScreencastSinks = Arc<StdMutex<HashMap<String, Arc<ScreencastSlot>>>>;

pub(crate) struct ScreencastRegistration {
    sinks: ScreencastSinks,
    session_key: String,
    slot: Arc<ScreencastSlot>,
}

impl ScreencastRegistration {
    pub(crate) fn slot(&self) -> &Arc<ScreencastSlot> {
        &self.slot
    }
}

impl Drop for ScreencastRegistration {
    fn drop(&mut self) {
        self.slot.clear();
        if let Ok(mut map) = self.sinks.lock()
            && map
                .get(&self.session_key)
                .is_some_and(|cur| Arc::ptr_eq(cur, &self.slot))
        {
            map.remove(&self.session_key);
        }
    }
}

/// A connection to a CDP websocket endpoint shared by all sessions of a browser.
pub struct CdpClient {
    sink: Mutex<Sink>,
    next_id: AtomicU64,
    pending: Pending,
    events: broadcast::Sender<CdpEvent>,
    screencast_sinks: ScreencastSinks,
    active_views: StdMutex<Vec<Weak<ViewStopHookFn>>>,
    closed: Arc<AtomicBool>,
    close_tx: watch::Sender<bool>,
    reader: JoinHandle<()>,
    limits: CdpClientConfig,
    rejected_events: Arc<AtomicU64>,
    rejected_screencast_events: Arc<AtomicU64>,
    unsubscribed_screencast_events: Arc<AtomicU64>,
}

#[derive(Default)]
struct DispatchStats {
    rejected_generic: bool,
    rejected_screencast: bool,
    unsubscribed_screencast: bool,
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
        let screencast_sinks: ScreencastSinks = Arc::new(StdMutex::new(HashMap::new()));
        let active_views = StdMutex::new(Vec::new());
        let closed = Arc::new(AtomicBool::new(false));
        let (close_tx, _) = watch::channel(false);
        let rejected_events = Arc::new(AtomicU64::new(0));
        let rejected_screencast_events = Arc::new(AtomicU64::new(0));
        let unsubscribed_screencast_events = Arc::new(AtomicU64::new(0));

        let reader = {
            let pending = pending.clone();
            let events = events.clone();
            let screencast_sinks = screencast_sinks.clone();
            let closed = closed.clone();
            let close_tx = close_tx.clone();
            let limits = limits.clone();
            let rejected_events = rejected_events.clone();
            let rejected_screencast_events = rejected_screencast_events.clone();
            let unsubscribed_screencast_events = unsubscribed_screencast_events.clone();
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
                    match dispatch(text.as_str(), &pending, &events, &screencast_sinks, &limits) {
                        Ok(stats) => {
                            if stats.rejected_generic {
                                rejected_events.fetch_add(1, Ordering::Relaxed);
                            }
                            if stats.rejected_screencast {
                                rejected_screencast_events.fetch_add(1, Ordering::Relaxed);
                            }
                            if stats.unsubscribed_screencast {
                                unsubscribed_screencast_events.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        Err(Error::MessageTooLarge { kind: "event", .. }) => {
                            rejected_events.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(_) => {}
                    }
                }
                closed.store(true, Ordering::SeqCst);
                close_tx.send_replace(true);
                if let Ok(sinks) = screencast_sinks.lock() {
                    for slot in sinks.values() {
                        slot.notify();
                    }
                }
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
            screencast_sinks,
            active_views,
            closed,
            close_tx,
            reader,
            limits,
            rejected_events,
            rejected_screencast_events,
            unsubscribed_screencast_events,
        }))
    }

    /// Subscribe to all events (every session). Subscribe *before* sending the command that triggers them.
    pub fn subscribe(&self) -> broadcast::Receiver<CdpEvent> {
        self.events.subscribe()
    }

    /// Register a single-slot `Page.screencastFrame` sink for a flattened session (or `None` for root).
    ///
    /// Replaces any prior registration for the same session key. Dropping the returned
    /// guard removes the registration if it still points to this slot.
    pub(crate) fn register_screencast_sink(
        &self,
        session_id: Option<&str>,
    ) -> ScreencastRegistration {
        let session_key = session_id.unwrap_or_default().to_string();
        let slot = ScreencastSlot::new();
        if let Ok(mut map) = self.screencast_sinks.lock() {
            map.insert(session_key.clone(), slot.clone());
        }
        ScreencastRegistration {
            sinks: self.screencast_sinks.clone(),
            session_key,
            slot,
        }
    }

    /// Register a weak stop callback invoked before closing the browser.
    pub(crate) fn register_view_stop_hook(&self, hook: &ViewStopHook) {
        if let Ok(mut hooks) = self.active_views.lock() {
            hooks.retain(|w| w.strong_count() > 0);
            hooks.push(Arc::downgrade(hook));
        }
    }

    /// Stop all live views registered on this connection (used before `Browser::close`).
    pub(crate) async fn stop_active_views(&self) {
        let hooks: Vec<ViewStopHook> = {
            let Ok(mut guard) = self.active_views.lock() else {
                return;
            };
            let live: Vec<_> = guard.iter().filter_map(Weak::upgrade).collect();
            guard.retain(|w| w.strong_count() > 0);
            live
        };
        let futures: Vec<_> = hooks.into_iter().map(|hook| hook()).collect();
        futures_util::future::join_all(futures).await;
    }

    /// Events discarded because they exceeded the configured per-event byte cap.
    /// Queue eviction is reported separately by each receiver's `Lagged` error.
    pub fn rejected_event_count(&self) -> u64 {
        self.rejected_events.load(Ordering::Relaxed)
    }

    /// Screencast frames rejected due to byte budget or JPEG/metadata validation failure.
    pub fn rejected_screencast_event_count(&self) -> u64 {
        self.rejected_screencast_events.load(Ordering::Relaxed)
    }

    /// Screencast frames discarded immediately because no live-view sink was subscribed.
    pub fn unsubscribed_screencast_event_count(&self) -> u64 {
        self.unsubscribed_screencast_events.load(Ordering::Relaxed)
    }

    /// Returns the configured transport and screencast byte limits.
    pub fn limits(&self) -> &CdpClientConfig {
        &self.limits
    }

    /// True once the websocket has closed.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// Subscribe to websocket closure as a level-triggered watch receiver.
    pub(crate) fn subscribe_closed(&self) -> watch::Receiver<bool> {
        self.close_tx.subscribe()
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
        self.closed.store(true, Ordering::SeqCst);
        self.close_tx.send_replace(true);
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawScreencastParams<'a> {
    #[serde(borrow)]
    data: Cow<'a, str>,
    metadata: RawScreencastMetadata,
    session_id: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawScreencastMetadata {
    #[serde(default)]
    offset_top: f64,
    #[serde(default = "default_scale_factor")]
    page_scale_factor: f64,
    device_width: f64,
    device_height: f64,
    #[serde(default)]
    scroll_offset_x: f64,
    #[serde(default)]
    scroll_offset_y: f64,
    #[serde(default)]
    timestamp: Option<f64>,
}

const fn default_scale_factor() -> f64 {
    1.0
}

fn dispatch(
    text: &str,
    pending: &Pending,
    events: &broadcast::Sender<CdpEvent>,
    screencast_sinks: &ScreencastSinks,
    limits: &CdpClientConfig,
) -> Result<DispatchStats> {
    let mut stats = DispatchStats::default();
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
        if method == SCREENCAST_METHOD {
            dispatch_screencast(
                text.len(),
                message.session_id.as_deref(),
                message.params,
                screencast_sinks,
                limits,
                &mut stats,
            );
            return Ok(stats);
        }
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
    Ok(stats)
}

fn dispatch_screencast(
    raw_len: usize,
    session_id: Option<&str>,
    raw_params: Option<&RawValue>,
    screencast_sinks: &ScreencastSinks,
    limits: &CdpClientConfig,
    stats: &mut DispatchStats,
) {
    let session_key = session_id.unwrap_or_default();
    let slot = screencast_sinks
        .lock()
        .ok()
        .and_then(|map| map.get(session_key).cloned());

    if raw_len > limits.max_screencast_event_bytes {
        stats.rejected_screencast = true;
        if let Some(slot) = slot {
            slot.push_fault(ScreencastFault::EventTooLarge {
                size: raw_len,
                limit: limits.max_screencast_event_bytes,
            });
        }
        return;
    }

    let Some(slot) = slot else {
        stats.unsubscribed_screencast = true;
        return;
    };

    let Some(raw) = raw_params else {
        stats.rejected_screencast = true;
        slot.push_fault(ScreencastFault::MalformedFrame(
            "screencastFrame missing params".into(),
        ));
        return;
    };

    let parsed: RawScreencastParams<'_> = match serde_json::from_str(raw.get()) {
        Ok(parsed) => parsed,
        Err(error) => {
            stats.rejected_screencast = true;
            slot.push_fault(ScreencastFault::MalformedFrame(format!(
                "invalid screencastFrame params: {error}"
            )));
            return;
        }
    };

    let Some(metadata) = validate_screencast_metadata(&parsed.metadata) else {
        stats.rejected_screencast = true;
        slot.push_fault(ScreencastFault::MalformedFrame(
            "invalid screencastFrame metadata geometry".into(),
        ));
        return;
    };

    match decode_screencast_jpeg(parsed.data.as_ref(), limits.max_screencast_image_bytes) {
        Ok((jpeg, width, height)) => {
            slot.push_frame(DecodedScreencastFrame {
                jpeg,
                width,
                height,
                ack_session_id: parsed.session_id,
                metadata,
            });
        }
        Err(fault) => {
            stats.rejected_screencast = true;
            slot.push_fault(fault);
        }
    }
}

fn validate_screencast_metadata(raw: &RawScreencastMetadata) -> Option<ScreencastMetadata> {
    let finite = raw.offset_top.is_finite()
        && raw.page_scale_factor.is_finite()
        && raw.device_width.is_finite()
        && raw.device_height.is_finite()
        && raw.scroll_offset_x.is_finite()
        && raw.scroll_offset_y.is_finite()
        && raw.timestamp.is_none_or(|t| t.is_finite() && t >= 0.0);
    if !finite
        || raw.offset_top < 0.0
        || raw.page_scale_factor <= 0.0
        || raw.device_width <= 0.0
        || raw.device_height <= raw.offset_top
    {
        return None;
    }
    Some(ScreencastMetadata {
        offset_top: raw.offset_top,
        page_scale_factor: raw.page_scale_factor,
        device_width: raw.device_width,
        device_height: raw.device_height,
        scroll_offset_x: raw.scroll_offset_x,
        scroll_offset_y: raw.scroll_offset_y,
        timestamp: raw.timestamp,
    })
}

fn decode_screencast_jpeg(
    data: &str,
    max_image_bytes: usize,
) -> std::result::Result<(Arc<[u8]>, u32, u32), ScreencastFault> {
    let b64 = data.as_bytes();
    if b64.len() < 4 || !b64.len().is_multiple_of(4) || b64.ends_with(b"===") {
        return Err(ScreencastFault::MalformedFrame(
            "invalid base64 frame length".into(),
        ));
    }
    let pad = if b64.ends_with(b"==") {
        2usize
    } else {
        usize::from(b64.ends_with(b"="))
    };
    let upper = (b64.len() / 4) * 3;
    let decoded_len = upper.saturating_sub(pad);
    if decoded_len < 4 {
        return Err(ScreencastFault::MalformedFrame(
            "screencast frame shorter than JPEG header".into(),
        ));
    }
    if decoded_len > max_image_bytes {
        return Err(ScreencastFault::ImageTooLarge {
            size: decoded_len,
            limit: max_image_bytes,
        });
    }

    // Validate JPEG SOI (`FF D8`) on stack before allocating the full image buffer.
    let mut head = [0u8; 3];
    match BASE64_STANDARD.decode_slice(&b64[..4], &mut head) {
        Ok(3) if head[0] == 0xFF && head[1] == 0xD8 => {}
        Ok(_) => {
            return Err(ScreencastFault::MalformedFrame(
                "screencast frame is not JPEG (missing SOI)".into(),
            ));
        }
        Err(error) => {
            return Err(ScreencastFault::MalformedFrame(format!(
                "invalid base64 header: {error}"
            )));
        }
    }

    // Allocate exact conservative upper bound (at most +2 bytes over decoded_len).
    let mut buf = vec![0u8; upper];
    let written = BASE64_STANDARD
        .decode_slice(b64, &mut buf)
        .map_err(|error| {
            ScreencastFault::MalformedFrame(format!("invalid base64 payload: {error}"))
        })?;
    buf.truncate(written);
    if buf.len() > max_image_bytes {
        return Err(ScreencastFault::ImageTooLarge {
            size: buf.len(),
            limit: max_image_bytes,
        });
    }

    let (width, height) = parse_jpeg_dimensions(&buf)
        .map_err(|reason| ScreencastFault::MalformedFrame(reason.into()))?;
    Ok((Arc::from(buf), width, height))
}

fn parse_jpeg_dimensions(bytes: &[u8]) -> std::result::Result<(u32, u32), &'static str> {
    if bytes.len() < 4 || bytes[0] != 0xFF || bytes[1] != 0xD8 {
        return Err("missing JPEG SOI marker");
    }
    let mut pos = 2usize;
    while pos < bytes.len() {
        if bytes[pos] != 0xFF {
            return Err("invalid JPEG marker alignment");
        }
        while pos < bytes.len() && bytes[pos] == 0xFF {
            pos += 1;
        }
        if pos >= bytes.len() {
            break;
        }
        let marker = bytes[pos];
        pos += 1;
        match marker {
            0x00 | 0x01 | 0xD0..=0xD8 => continue,
            0xD9 | 0xDA => return Err("JPEG reached scan/end before SOF header"),
            _ => {}
        }
        if pos + 2 > bytes.len() {
            return Err("truncated JPEG segment length");
        }
        let seg_len = u16::from_be_bytes([bytes[pos], bytes[pos + 1]]) as usize;
        if seg_len < 2 || pos + seg_len > bytes.len() {
            return Err("invalid JPEG segment length");
        }
        let is_sof = matches!(
            marker,
            0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF
        );
        if is_sof {
            if seg_len < 7 {
                return Err("truncated JPEG SOF segment");
            }
            let height = u32::from(u16::from_be_bytes([bytes[pos + 3], bytes[pos + 4]]));
            let width = u32::from(u16::from_be_bytes([bytes[pos + 5], bytes[pos + 6]]));
            if !(1..=MAX_SCREENCAST_IMAGE_WIDTH).contains(&width)
                || !(1..=MAX_SCREENCAST_IMAGE_HEIGHT).contains(&height)
            {
                return Err("JPEG dimensions outside live-view bounds");
            }
            return Ok((width, height));
        }
        pos += seg_len;
    }
    Err("JPEG missing start-of-frame header")
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
        let sinks = ScreencastSinks::default();
        let (events, mut receiver) = broadcast::channel(capacity);
        for index in 0..1000 {
            let message =
                json!({"method": "Test.event", "params": {"index": index, "text": "x".repeat(64)}})
                    .to_string();
            dispatch(&message, &pending, &events, &sinks, &limits).unwrap();
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

    fn minimal_jpeg(width: u16, height: u16, target_len: usize) -> Vec<u8> {
        let mut bytes = vec![
            0xFF,
            0xD8, // SOI
            0xFF,
            0xC0,
            0x00,
            0x0B,
            0x08, // SOF0
            (height >> 8) as u8,
            (height & 0xFF) as u8,
            (width >> 8) as u8,
            (width & 0xFF) as u8,
            0x01,
            0x01,
            0x11,
            0x00, // 1 component
            0xFF,
            0xDA,
            0x00,
            0x08,
            0x01,
            0x01,
            0x00,
            0x00,
            0x3F,
            0x00, // SOS
        ];
        if target_len > bytes.len() + 2 {
            bytes.resize(target_len - 2, 0xAA);
        }
        bytes.extend_from_slice(&[0xFF, 0xD9]);
        bytes
    }

    #[tokio::test]
    async fn screencast_frame_over_64kib_bypasses_generic_ring_and_routes_by_session() {
        let big_jpeg = minimal_jpeg(960, 600, 128 * 1024);
        let b64 = BASE64_STANDARD.encode(&big_jpeg);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (s, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(s).await.unwrap();
            while let Some(Ok(Message::Text(t))) = ws.next().await {
                let req: Value = serde_json::from_str(t.as_str()).unwrap();
                let frames = vec![
                    json!({
                        "method": "Page.screencastFrame",
                        "sessionId": "TAB-OTHER",
                        "params": {
                            "data": b64,
                            "metadata": {"deviceWidth": 960, "deviceHeight": 600, "pageScaleFactor": 1},
                            "sessionId": 99
                        }
                    }),
                    json!({
                        "method": "Page.screencastFrame",
                        "sessionId": "TAB-1",
                        "params": {
                            "data": b64,
                            "metadata": {"deviceWidth": 960, "deviceHeight": 600, "pageScaleFactor": 1, "offsetTop": 0},
                            "sessionId": 42
                        }
                    }),
                    json!({"method": "Page.loadEventFired", "sessionId": "TAB-1", "params": {"timestamp": 1.0}}),
                    json!({"id": req["id"], "result": {"ok": true}}),
                ];
                for msg in frames {
                    ws.send(Message::text(msg.to_string())).await.unwrap();
                }
            }
        });

        let client = CdpClient::connect(&url).await.unwrap();
        let reg = client.register_screencast_sink(Some("TAB-1"));
        let mut generic_rx = client.subscribe();
        let reply = client.send("Sync", json!({})).await.unwrap();
        assert_eq!(reply["ok"], true);

        let take = reg.slot().take();
        let frame = take.frame.expect("TAB-1 screencast frame received");
        assert_eq!(frame.ack_session_id, 42);
        assert_eq!(frame.width, 960);
        assert_eq!(frame.height, 600);
        assert_eq!(frame.jpeg.len(), big_jpeg.len());
        assert_eq!(client.unsubscribed_screencast_event_count(), 1);
        assert_eq!(client.rejected_screencast_event_count(), 0);

        // Broadcast ring must only contain Page.loadEventFired, never the 128 KiB screencast payload.
        let ev = generic_rx.try_recv().unwrap();
        assert_eq!(ev.method, "Page.loadEventFired");
        assert!(matches!(
            generic_rx.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn screencast_bounds_exact_2mib_and_rejects_oversized_malformed_unsubscribed() {
        let exact_2mib = minimal_jpeg(1280, 800, 2 * 1024 * 1024);
        assert_eq!(exact_2mib.len(), 2 * 1024 * 1024);
        let over_2mib = minimal_jpeg(1280, 800, 2 * 1024 * 1024 + 1);
        let over_3mib_event = "A".repeat(3 * 1024 * 1024 + 64);
        let non_jpeg_b64 = BASE64_STANDARD.encode(b"\x89PNG\r\n\x1a\nnotjpeg");
        let no_sof_b64 = BASE64_STANDARD.encode([0xFF, 0xD8, 0xFF, 0xD9]);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (s, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(s).await.unwrap();
            while let Some(Ok(Message::Text(t))) = ws.next().await {
                let req: Value = serde_json::from_str(t.as_str()).unwrap();
                match req["method"].as_str() {
                    Some("Batch") => {
                        let msgs = vec![
                            json!({"method": "Page.screencastFrame", "sessionId": "S1", "params": {"data": BASE64_STANDARD.encode(&exact_2mib), "metadata": {"deviceWidth": 1280, "deviceHeight": 800}, "sessionId": 1}}),
                            json!({"id": req["id"], "result": {"step": 1}}),
                        ];
                        for m in msgs {
                            ws.send(Message::text(m.to_string())).await.unwrap();
                        }
                    }
                    Some("InvalidBatch") => {
                        let msgs = vec![
                            json!({"method": "Page.screencastFrame", "sessionId": "S1", "params": {"data": BASE64_STANDARD.encode(&over_2mib), "metadata": {"deviceWidth": 1280, "deviceHeight": 800}, "sessionId": 2}}),
                            json!({"method": "Page.screencastFrame", "sessionId": "S1", "params": {"data": non_jpeg_b64, "metadata": {"deviceWidth": 1280, "deviceHeight": 800}, "sessionId": 3}}),
                            json!({"method": "Page.screencastFrame", "sessionId": "S1", "params": {"data": no_sof_b64, "metadata": {"deviceWidth": 1280, "deviceHeight": 800}, "sessionId": 4}}),
                            json!({"method": "Page.screencastFrame", "sessionId": "S1", "params": {"data": over_3mib_event, "metadata": {"deviceWidth": 1280, "deviceHeight": 800}, "sessionId": 5}}),
                            json!({"method": "Test.bigGeneric", "sessionId": "S1", "params": {"text": "x".repeat(70 * 1024)}}),
                            json!({"id": req["id"], "result": {"step": 2}}),
                        ];
                        for m in msgs {
                            ws.send(Message::text(m.to_string())).await.unwrap();
                        }
                    }
                    Some("BurstFlood") => {
                        let small = BASE64_STANDARD.encode(minimal_jpeg(640, 480, 128));
                        for i in 1..=25 {
                            ws.send(Message::text(
                                json!({"method": "Page.screencastFrame", "sessionId": "S1", "params": {"data": small, "metadata": {"deviceWidth": 640, "deviceHeight": 480}, "sessionId": i}})
                                    .to_string(),
                            ))
                            .await
                            .unwrap();
                        }
                        ws.send(Message::text(
                            json!({"id": req["id"], "result": {"step": 3}}).to_string(),
                        ))
                        .await
                        .unwrap();
                    }
                    _ => {
                        ws.send(Message::text(
                            json!({"id": req["id"], "result": {}}).to_string(),
                        ))
                        .await
                        .unwrap();
                    }
                }
            }
        });

        let client = CdpClient::connect(&url).await.unwrap();
        let reg = client.register_screencast_sink(Some("S1"));
        assert_eq!(client.send("Batch", json!({})).await.unwrap()["step"], 1);
        let take = reg.slot().take();
        assert_eq!(take.frame.unwrap().jpeg.len(), 2 * 1024 * 1024);

        assert_eq!(
            client.send("InvalidBatch", json!({})).await.unwrap()["step"],
            2
        );
        let take2 = reg.slot().take();
        assert!(take2.frame.is_none());
        assert!(matches!(
            take2.fault,
            Some(ScreencastFault::EventTooLarge { .. })
        ));
        assert_eq!(client.rejected_screencast_event_count(), 4);
        assert_eq!(client.rejected_event_count(), 1);
        assert!(!client.is_closed());

        // Single-slot retention: 25 bursts keep only frame 25 and cap dropped_acks at MAX_PENDING_DROPPED_ACKS.
        assert_eq!(
            client.send("BurstFlood", json!({})).await.unwrap()["step"],
            3
        );
        let take3 = reg.slot().take();
        assert_eq!(take3.frame.unwrap().ack_session_id, 25);
        assert_eq!(take3.dropped_acks.len(), MAX_PENDING_DROPPED_ACKS);
        assert_eq!(take3.dropped_acks[0], 9);
        assert_eq!(*take3.dropped_acks.last().unwrap(), 24);

        // Unsubscribing clears sink and drops subsequent frames without queueing.
        drop(reg);
        assert_eq!(client.send("Batch", json!({})).await.unwrap()["step"], 1);
        assert_eq!(client.unsubscribed_screencast_event_count(), 1);
    }
}
