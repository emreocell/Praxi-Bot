// Adapted from vercel-labs/agent-browser (Apache-2.0): raw CDP over tokio-tungstenite.
//! Minimal CDP client: request/response by id, event broadcast, flattened-session multiplexing.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, broadcast, oneshot};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::error::{Error, Result};

type Pending = Arc<StdMutex<HashMap<u64, oneshot::Sender<Result<Value>>>>>;
type Sink = SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>;

/// Default per-command timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

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
    timeout: Duration,
}

impl CdpClient {
    /// Connect to a `ws://` debugging endpoint.
    pub async fn connect(url: &str) -> Result<Arc<Self>> {
        let (ws, _) = tokio_tungstenite::connect_async(url)
            .await
            .map_err(|e| Error::WebSocket(e.to_string()))?;
        let (sink, mut stream) = ws.split();
        let pending: Pending = Arc::new(StdMutex::new(HashMap::new()));
        let (events, _) = broadcast::channel(4096);
        let closed = Arc::new(AtomicBool::new(false));

        let reader = {
            let pending = pending.clone();
            let events = events.clone();
            let closed = closed.clone();
            tokio::spawn(async move {
                while let Some(msg) = stream.next().await {
                    let text = match msg {
                        Ok(Message::Text(t)) => t,
                        Ok(Message::Close(_)) | Err(_) => break,
                        Ok(_) => continue,
                    };
                    let Ok(v) = serde_json::from_str::<Value>(text.as_str()) else {
                        continue;
                    };
                    dispatch(&v, &pending, &events);
                }
                closed.store(true, Ordering::SeqCst);
                let drained: Vec<_> = pending
                    .lock()
                    .map(|mut p| p.drain().collect())
                    .unwrap_or_default();
                for (_, tx) in drained {
                    let _ = tx.send(Err(Error::Closed));
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
            timeout: DEFAULT_TIMEOUT,
        }))
    }

    /// Subscribe to all events (every session). Subscribe *before* sending the command that triggers them.
    pub fn subscribe(&self) -> broadcast::Receiver<CdpEvent> {
        self.events.subscribe()
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
        self.send_with_timeout(session, method, params, self.timeout)
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
        if self.is_closed() {
            return Err(Error::Closed);
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .map_err(|_| Error::Closed)?
            .insert(id, tx);
        let mut msg = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = session {
            msg["sessionId"] = json!(s);
        }
        let sent = self
            .sink
            .lock()
            .await
            .send(Message::text(msg.to_string()))
            .await;
        if let Err(e) = sent {
            self.forget(id);
            return Err(Error::WebSocket(e.to_string()));
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(res)) => res,
            Ok(Err(_)) => Err(Error::Closed),
            Err(_) => {
                self.forget(id);
                Err(Error::Timeout(format!("{method} after {timeout:?}")))
            }
        }
    }

    fn forget(&self, id: u64) {
        if let Ok(mut p) = self.pending.lock() {
            p.remove(&id);
        }
    }
}

impl Drop for CdpClient {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

fn dispatch(v: &Value, pending: &Pending, events: &broadcast::Sender<CdpEvent>) {
    if let Some(id) = v.get("id").and_then(Value::as_u64) {
        let tx = pending.lock().ok().and_then(|mut p| p.remove(&id));
        if let Some(tx) = tx {
            let res = match v.get("error") {
                Some(err) => Err(Error::Protocol {
                    code: err.get("code").and_then(Value::as_i64).unwrap_or(-1),
                    message: err
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_string(),
                }),
                None => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
            };
            let _ = tx.send(res);
        }
    } else if let Some(method) = v.get("method").and_then(Value::as_str) {
        let _ = events.send(CdpEvent {
            method: method.to_string(),
            params: v.get("params").cloned().unwrap_or(Value::Null),
            session_id: v
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::to_string),
        });
    }
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
