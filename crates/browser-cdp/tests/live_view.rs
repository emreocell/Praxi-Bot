//! Hermetic integration tests for CDP live-view screencasting and viewport-mapped human input,
//! plus an ignored real-Chrome/Edge end-to-end test.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use browser_cdp::{
    BROWSER_INPUT_LIMITATION, Browser, BrowserViewConfig, BrowserViewTarget,
    DEFAULT_MAX_SCREENCAST_EVENT_BYTES, DEFAULT_MAX_SCREENCAST_IMAGE_BYTES, LaunchOptions,
    ViewportGeometry,
};
use futures_util::{SinkExt, StreamExt};
use live_view::{
    Button, Cause, Codec, EffectiveProfile, FrameSubscription, InputEvent, KeyPhase, Modifier,
    PointerPhase, Reason, TargetKind, Units, ViewError, ViewTarget,
};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::{Notify, mpsc};
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;

type HoldGate = (String, Arc<Notify>, Arc<Notify>);

#[derive(Debug, Clone)]
struct RecordedCommand {
    session_id: Option<String>,
    method: String,
    params: Value,
}

#[derive(Default)]
struct FakeServerState {
    commands: Mutex<Vec<RecordedCommand>>,
    cmd_notify: Notify,
    out_tx: Mutex<Option<mpsc::UnboundedSender<String>>>,
    next_target: AtomicU64,
    next_session: AtomicU64,
    attached_targets: Mutex<Vec<(String, String)>>,
    start_screencast_error: Mutex<Option<(i64, String)>>,
    fail_next_method: Mutex<HashMap<String, (i64, String)>>,
    hold_next_method: Mutex<Option<HoldGate>>,
    capture_screenshot_calls: AtomicUsize,
    layout_metrics: Mutex<Value>,
}

impl FakeServerState {
    fn new() -> Arc<Self> {
        let state = Arc::new(Self::default());
        *state.layout_metrics.lock().expect("layout lock") = json!({
            "cssVisualViewport": {
                "clientWidth": 1000.0,
                "clientHeight": 800.0,
                "scale": 1.0,
                "pageX": 0.0,
                "pageY": 0.0
            }
        });
        state
    }

    fn send_raw(&self, text: String) {
        if let Some(tx) = self.out_tx.lock().expect("out_tx lock").as_ref() {
            let _ = tx.send(text);
        }
    }

    fn send_event(&self, session_id: Option<&str>, method: &str, params: &Value) {
        let mut msg = json!({
            "method": method,
            "params": params,
        });
        if let Some(sid) = session_id {
            msg["sessionId"] = Value::String(sid.to_string());
        }
        self.send_raw(msg.to_string());
    }

    fn send_screencast_frame(&self, session_id: &str, ack_id: u64, jpeg: &[u8], meta: &Value) {
        let b64 = base64::engine::general_purpose::STANDARD.encode(jpeg);
        self.send_event(
            Some(session_id),
            "Page.screencastFrame",
            &json!({
                "data": b64,
                "metadata": meta,
                "sessionId": ack_id,
            }),
        );
    }

    fn commands(&self) -> Vec<RecordedCommand> {
        self.commands.lock().expect("commands lock").clone()
    }

    fn count_method(&self, method: &str) -> usize {
        self.commands
            .lock()
            .expect("commands lock")
            .iter()
            .filter(|c| c.method == method)
            .count()
    }

    fn clear_commands(&self) {
        self.commands.lock().expect("commands lock").clear();
    }

    async fn wait_for_method_count(&self, method: &str, target_count: usize) {
        loop {
            let notified = self.cmd_notify.notified();
            if self.count_method(method) >= target_count {
                return;
            }
            notified.await;
        }
    }
}

struct FakeCdpServer {
    addr: String,
    state: Arc<FakeServerState>,
    handle: tokio::task::JoinHandle<()>,
}

impl FakeCdpServer {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback ws");
        let port = listener.local_addr().expect("local_addr").port();
        let addr = format!("ws://127.0.0.1:{port}");
        let state = FakeServerState::new();
        let st = Arc::clone(&state);

        let handle = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let st_conn = Arc::clone(&st);
                tokio::spawn(async move {
                    let Ok(ws) = accept_async(stream).await else {
                        return;
                    };
                    let (mut ws_tx, mut ws_rx) = ws.split();
                    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
                    *st_conn.out_tx.lock().expect("out_tx lock") = Some(tx);

                    let writer = tokio::spawn(async move {
                        while let Some(msg) = rx.recv().await {
                            if ws_tx.send(Message::Text(msg.into())).await.is_err() {
                                break;
                            }
                        }
                    });

                    while let Some(Ok(frame)) = ws_rx.next().await {
                        let Message::Text(txt) = frame else {
                            if matches!(frame, Message::Close(_)) {
                                break;
                            }
                            continue;
                        };
                        let Ok(val) = serde_json::from_str::<Value>(&txt) else {
                            continue;
                        };
                        let id = val.get("id").and_then(Value::as_u64).unwrap_or(0);
                        let method = val
                            .get("method")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        let session_id = val
                            .get("sessionId")
                            .and_then(Value::as_str)
                            .map(str::to_string);
                        let params = val.get("params").cloned().unwrap_or_else(|| json!({}));

                        if method == "Page.captureScreenshot" {
                            st_conn
                                .capture_screenshot_calls
                                .fetch_add(1, Ordering::SeqCst);
                        }

                        {
                            let mut cmds = st_conn.commands.lock().expect("commands lock");
                            cmds.push(RecordedCommand {
                                session_id: session_id.clone(),
                                method: method.clone(),
                                params: params.clone(),
                            });
                        }
                        st_conn.cmd_notify.notify_waiters();

                        let hold = {
                            let mut guard = st_conn.hold_next_method.lock().expect("hold lock");
                            match guard.as_ref() {
                                Some((m, _, _)) if m == &method => guard.take(),
                                _ => None,
                            }
                        };
                        if let Some((_, reached, release)) = hold {
                            let st_reply = Arc::clone(&st_conn);
                            let method_clone = method.clone();
                            tokio::spawn(async move {
                                reached.notify_one();
                                release.notified().await;
                                let resp = build_reply(&st_reply, id, &method_clone, &params);
                                st_reply.send_raw(resp.to_string());
                            });
                            continue;
                        }

                        let resp = build_reply(&st_conn, id, &method, &params);
                        st_conn.send_raw(resp.to_string());
                    }

                    writer.abort();
                });
            }
        });

        Self {
            addr,
            state,
            handle,
        }
    }

    fn ws_url(&self) -> String {
        self.addr.clone()
    }
}

impl Drop for FakeCdpServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

fn build_reply(state: &FakeServerState, id: u64, method: &str, params: &Value) -> Value {
    if let Some((code, msg)) = state
        .fail_next_method
        .lock()
        .expect("fail_next_method lock")
        .remove(method)
    {
        return json!({
            "id": id,
            "error": { "code": code, "message": msg }
        });
    }

    if method == "Page.startScreencast"
        && let Some((code, msg)) = state
            .start_screencast_error
            .lock()
            .expect("start error lock")
            .clone()
    {
        return json!({
            "id": id,
            "error": { "code": code, "message": msg }
        });
    }

    let result = match method {
        "Target.createTarget" => {
            let n = state.next_target.fetch_add(1, Ordering::SeqCst) + 1;
            json!({ "targetId": format!("target-{n}") })
        }
        "Target.attachToTarget" => {
            let tid = params
                .get("targetId")
                .and_then(Value::as_str)
                .unwrap_or("target-1")
                .to_string();
            let n = state.next_session.fetch_add(1, Ordering::SeqCst) + 1;
            let sid = format!("sess-{n}");
            state
                .attached_targets
                .lock()
                .expect("attached lock")
                .push((tid, sid.clone()));
            json!({ "sessionId": sid })
        }
        "Target.getTargets" => {
            let infos: Vec<Value> = state
                .attached_targets
                .lock()
                .expect("attached lock")
                .iter()
                .map(|(tid, _)| {
                    json!({
                        "targetId": tid,
                        "type": "page",
                        "title": tid,
                        "url": "https://example.test/"
                    })
                })
                .collect();
            json!({ "targetInfos": infos })
        }
        "Page.getLayoutMetrics" => state.layout_metrics.lock().expect("layout lock").clone(),
        "Page.getFrameTree" => json!({
            "frameTree": {
                "frame": { "id": "frame-1", "url": "https://example.test/" }
            }
        }),
        "Accessibility.getFullAXTree" => json!({ "nodes": [] }),
        "DOM.getDocument" => json!({
            "root": { "nodeId": 1, "backendNodeId": 1, "children": [] }
        }),
        "Runtime.evaluate" => {
            if params.get("expression").and_then(Value::as_str) == Some("document.readyState") {
                json!({ "result": { "type": "string", "value": "complete" } })
            } else {
                json!({ "result": { "type": "boolean", "value": true } })
            }
        }
        "Browser.getVersion" => json!({ "product": "FakeChrome/157.0" }),
        _ => json!({}),
    };

    json!({
        "id": id,
        "result": result,
    })
}

fn minimal_jpeg(width: u16, height: u16, extra_pad_bytes: usize) -> Vec<u8> {
    let mut out = vec![
        0xFF,
        0xD8, // SOI
        0xFF,
        0xC0, // SOF0
        0x00,
        0x0B, // segment length
        0x08, // precision
        (height >> 8) as u8,
        (height & 0xFF) as u8,
        (width >> 8) as u8,
        (width & 0xFF) as u8,
        0x01,
        0x01,
        0x11,
        0x00,
    ];
    let mut remaining = extra_pad_bytes;
    while remaining > 0 {
        let payload_len = remaining.min(60_000);
        let seg_len = u16::try_from(payload_len + 2).expect("chunk <= 60002");
        out.push(0xFF);
        out.push(0xFE);
        out.push((seg_len >> 8) as u8);
        out.push((seg_len & 0xFF) as u8);
        out.resize(out.len() + payload_len, b'A');
        remaining -= payload_len;
    }
    out.push(0xFF);
    out.push(0xD9); // EOI
    out
}

fn default_meta(width: f64, height: f64) -> Value {
    json!({
        "offsetTop": 0.0,
        "pageScaleFactor": 1.0,
        "deviceWidth": width,
        "deviceHeight": height,
        "scrollOffsetX": 0.0,
        "scrollOffsetY": 0.0,
        "timestamp": 1_700_000_000.0
    })
}

async fn pump_tasks() {
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn idle_target_reports_browser_idle_and_never_launches_chrome() {
    let cfg = BrowserViewConfig::default();
    let target = BrowserViewTarget::new_idle(cfg.clone());

    let info = target.info();
    assert_eq!(info.target_id, cfg.target_id);
    assert_eq!(info.kind, TargetKind::Browser);
    assert!(!info.capabilities.view);
    assert!(!info.capabilities.input);
    assert_eq!(info.capabilities.codecs, vec![Codec::Jpeg]);
    assert_eq!(info.capabilities.reason, Some(Reason::BrowserIdle));
    assert_eq!(info.unavailable_reason, Some(Reason::BrowserIdle));
    assert!(target.input_sink().is_none());

    let src = target.frame_source();
    assert!(matches!(
        src.subscribe(EffectiveProfile::watch()).await,
        Err(ViewError::Unavailable)
    ));
    assert!(matches!(
        src.request_refresh(1).await,
        Err(ViewError::Unavailable)
    ));
    assert_eq!(target.status().generation, 0);

    // Explicitly attaching a running page bumps generation and activates capabilities.
    let server = FakeCdpServer::start().await;
    let browser = Browser::connect(&server.ws_url()).await.expect("connect");
    let page = browser.new_page("about:blank").await.expect("new_page");

    target.attach_page(&page).expect("attach_page");
    let info_attached = target.info();
    assert_eq!(info_attached.generation, 1);
    assert!(info_attached.capabilities.view);
    assert!(info_attached.capabilities.input);
    assert_eq!(info_attached.unavailable_reason, None);
    assert!(target.input_sink().is_some());

    // Subscribe and receive one frame cleanly, then deliberately retire back to BrowserIdle.
    let mut sub = src
        .subscribe(EffectiveProfile::watch())
        .await
        .expect("subscribe");
    server
        .state
        .wait_for_method_count("Page.startScreencast", 1)
        .await;
    let (_, sess) = server.state.attached_targets.lock().unwrap()[0].clone();
    server.state.send_screencast_frame(
        &sess,
        1,
        &minimal_jpeg(640, 480, 0),
        &default_meta(640.0, 480.0),
    );
    sub.frames.changed().await.expect("frame arrives");
    assert!(sub.frames.borrow().is_some());

    drop(sub);
    target.detach_to_idle().await.expect("detach_to_idle");
    let info_idle_again = target.info();
    assert_eq!(info_idle_again.generation, 2);
    assert!(!info_idle_again.capabilities.view);
    assert_eq!(
        info_idle_again.capabilities.reason,
        Some(Reason::BrowserIdle)
    );
    assert_eq!(
        server.state.capture_screenshot_calls.load(Ordering::SeqCst),
        0
    );
}

#[tokio::test]
async fn subscribe_before_start_and_single_tab_close_never_falls_back() {
    let server = FakeCdpServer::start().await;
    let browser = Browser::connect(&server.ws_url()).await.expect("connect");
    let page_a = browser.new_page("about:blank").await.expect("page_a");
    let page_b = browser.new_page("about:blank").await.expect("page_b");
    let sess_a = page_a.session_id().to_string();
    let target_a = page_a.target_id().to_string();

    let target = page_a.live_view(BrowserViewConfig::default());
    assert!(matches!(
        target.detach_to_idle().await,
        Err(ViewError::Unsupported)
    ));

    let src = target.frame_source();
    let mut sub = src
        .subscribe(EffectiveProfile::watch())
        .await
        .expect("subscribe");

    server
        .state
        .wait_for_method_count("Page.startScreencast", 1)
        .await;
    let start_cmd = server
        .state
        .commands()
        .into_iter()
        .find(|c| c.method == "Page.startScreencast")
        .expect("startScreencast recorded");
    assert_eq!(start_cmd.params["format"], "jpeg");
    assert_eq!(start_cmd.params["quality"], 45);
    assert_eq!(start_cmd.params["maxWidth"], 960);
    assert_eq!(start_cmd.params["maxHeight"], 600);
    assert_eq!(start_cmd.params["everyNthFrame"], 10);
    assert_eq!(start_cmd.session_id.as_deref(), Some(sess_a.as_str()));

    let jpg = minimal_jpeg(960, 600, 0);
    server
        .state
        .send_screencast_frame(&sess_a, 7, &jpg, &default_meta(960.0, 600.0));
    sub.frames.changed().await.expect("frame delivered");
    let f = sub.frames.borrow().clone().expect("frame present");
    assert_eq!((f.metadata.width, f.metadata.height), (960, 600));

    // Simulate unexpected tab close via `Target.targetDestroyed`.
    server.state.send_event(
        None,
        "Target.targetDestroyed",
        &json!({ "targetId": target_a }),
    );

    let _ = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if !target.info().capabilities.view {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;

    let info_after = target.info();
    assert!(!info_after.capabilities.view);
    assert!(!info_after.capabilities.input);
    assert_eq!(info_after.unavailable_reason, Some(Reason::Unavailable));
    assert!(info_after.generation >= 2);
    assert!(target.input_sink().is_none());
    assert!(matches!(
        target.attach_page(&page_b),
        Err(ViewError::Unavailable)
    ));
    assert!(matches!(
        src.subscribe(EffectiveProfile::live()).await,
        Err(ViewError::Unavailable)
    ));
    assert_eq!(
        server.state.capture_screenshot_calls.load(Ordering::SeqCst),
        0
    );
}

#[tokio::test]
async fn flattened_session_routing_ack_id_multi_consumer_and_cancel_cleanup() {
    let server = FakeCdpServer::start().await;
    let browser = Browser::connect(&server.ws_url()).await.expect("connect");
    let page_1 = browser.new_page("about:blank").await.expect("page_1");
    let page_2 = browser.new_page("about:blank").await.expect("page_2");
    let sess_1 = page_1.session_id().to_string();
    let sess_2 = page_2.session_id().to_string();

    let view_1 = page_1.live_view(BrowserViewConfig::default());
    let view_2 = page_2.live_view(BrowserViewConfig::default());
    let src_1 = view_1.frame_source();
    let src_2 = view_2.frame_source();

    let mut first_sub_1 = src_1
        .subscribe(EffectiveProfile::live())
        .await
        .expect("first_sub_1");
    let second_sub_1 = src_1
        .subscribe(EffectiveProfile::live())
        .await
        .expect("second_sub_1");
    let mut sub_2 = src_2
        .subscribe(EffectiveProfile::live())
        .await
        .expect("sub_2");
    server
        .state
        .wait_for_method_count("Page.startScreencast", 2)
        .await;

    let jpg_1 = minimal_jpeg(800, 600, 90_000);
    let jpg_2 = minimal_jpeg(1024, 768, 120_000);
    assert!(jpg_1.len() > 64 * 1024);

    server
        .state
        .send_screencast_frame(&sess_1, 42, &jpg_1, &default_meta(800.0, 600.0));
    server
        .state
        .send_screencast_frame(&sess_2, 99, &jpg_2, &default_meta(1024.0, 768.0));

    first_sub_1.frames.changed().await.expect("frame 1");
    sub_2.frames.changed().await.expect("frame 2");
    let f1 = first_sub_1.frames.borrow().clone().expect("f1");
    let f2 = sub_2.frames.borrow().clone().expect("f2");
    assert_eq!(
        (f1.metadata.width, f1.metadata.height, f1.payload.len()),
        (800, 600, jpg_1.len())
    );
    assert_eq!(
        (f2.metadata.width, f2.metadata.height, f2.payload.len()),
        (1024, 768, jpg_2.len())
    );

    server
        .state
        .wait_for_method_count("Page.screencastFrameAck", 2)
        .await;
    let acks: Vec<_> = server
        .state
        .commands()
        .into_iter()
        .filter(|c| c.method == "Page.screencastFrameAck")
        .collect();
    assert!(acks.iter().any(|c| {
        c.session_id.as_deref() == Some(sess_1.as_str()) && c.params["sessionId"] == 42
    }));
    assert!(acks.iter().any(|c| {
        c.session_id.as_deref() == Some(sess_2.as_str()) && c.params["sessionId"] == 99
    }));

    verify_multi_consumer_and_cancel_cleanup(&server, &view_1, &src_1, first_sub_1, second_sub_1)
        .await;
    drop(sub_2);
}

async fn verify_multi_consumer_and_cancel_cleanup(
    server: &FakeCdpServer,
    view_1: &BrowserViewTarget,
    src_1: &Arc<dyn live_view::FrameSource>,
    first_sub_1: FrameSubscription,
    second_sub_1: FrameSubscription,
) {
    // Dropping 1 of 2 subscribers keeps capture alive; dropping the last stops screencast.
    let stops_before = server.state.count_method("Page.stopScreencast");
    drop(second_sub_1);
    pump_tasks().await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(
        server.state.count_method("Page.stopScreencast"),
        stops_before
    );
    assert!(view_1.status().worker_running);

    let mut watcher_copy = first_sub_1.frames.clone();
    drop(first_sub_1);
    server
        .state
        .wait_for_method_count("Page.stopScreencast", stops_before + 1)
        .await;
    let _ = watcher_copy.changed().await;
    assert!(watcher_copy.borrow().is_none());
    for _ in 0..20 {
        if !view_1.status().worker_running {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(!view_1.status().worker_running);
    assert_eq!(view_1.status().active_profile, None);

    // Cancelling `subscribe()` mid-flight cleans up sink and stops screencast.
    let reached = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    *server.state.hold_next_method.lock().expect("hold lock") = Some((
        "Page.startScreencast".into(),
        Arc::clone(&reached),
        Arc::clone(&release),
    ));
    let src_1_clone = Arc::clone(src_1);
    let sub_fut =
        tokio::spawn(async move { src_1_clone.subscribe(EffectiveProfile::live()).await });
    reached.notified().await;
    sub_fut.abort();
    let _ = sub_fut.await;
    release.notify_one();

    server
        .state
        .wait_for_method_count("Page.stopScreencast", stops_before + 2)
        .await;
}

#[tokio::test]
async fn bounded_screencast_ingest_and_unrecoverable_session_failure() {
    let server = FakeCdpServer::start().await;
    let browser = Browser::connect(&server.ws_url()).await.expect("connect");
    let page = browser.new_page("about:blank").await.expect("page");
    let sess = page.session_id().to_string();

    let view = page.live_view(BrowserViewConfig::default());
    let src = view.frame_source();
    let mut sub = src
        .subscribe(EffectiveProfile::live())
        .await
        .expect("subscribe");
    server
        .state
        .wait_for_method_count("Page.startScreencast", 1)
        .await;

    // Generic 70 KiB event obeys 64 KiB cap while 80 KiB screencast succeeds.
    let mut broadcast_rx = browser.client().subscribe();
    let big_log = "x".repeat(70 * 1024);
    server.state.send_event(
        Some(&sess),
        "Runtime.consoleAPICalled",
        &json!({ "args": [{ "value": big_log }] }),
    );
    let ok_jpg = minimal_jpeg(1280, 720, 80_000);
    server
        .state
        .send_screencast_frame(&sess, 1, &ok_jpg, &default_meta(1280.0, 720.0));

    sub.frames
        .changed()
        .await
        .expect("80 KiB screencast delivered");
    assert_eq!(browser.client().rejected_event_count(), 1);
    assert!(
        broadcast_rx.try_recv().is_err(),
        "70 KiB generic event dropped"
    );

    // Flood oversized/malformed/unsubscribed screencast events; ordinary RPC replies stay healthy.
    let huge_event = json!({
        "sessionId": sess,
        "method": "Page.screencastFrame",
        "params": {
            "data": "A".repeat(DEFAULT_MAX_SCREENCAST_EVENT_BYTES + 1024),
            "metadata": default_meta(1280.0, 720.0),
            "sessionId": 2
        }
    });
    server.state.send_raw(huge_event.to_string());

    let over_2mib = vec![b'B'; DEFAULT_MAX_SCREENCAST_IMAGE_BYTES + 1];
    server
        .state
        .send_screencast_frame(&sess, 3, &over_2mib, &default_meta(1280.0, 720.0));

    let unsub_jpg = minimal_jpeg(640, 480, 1_100_000);
    server.state.send_screencast_frame(
        "sess-unsubscribed",
        4,
        &unsub_jpg,
        &default_meta(640.0, 480.0),
    );

    let bad_dims = minimal_jpeg(1921, 100, 0);
    server
        .state
        .send_screencast_frame(&sess, 5, &bad_dims, &default_meta(1921.0, 100.0));

    let ver = browser
        .version()
        .await
        .expect("version RPC healthy after flood");
    assert_eq!(ver["product"], "FakeChrome/157.0");
    assert!(browser.client().rejected_screencast_event_count() >= 3);
    assert!(browser.client().unsubscribed_screencast_event_count() >= 1);

    // Sustained session error (`-32001`) explicitly fails the view.
    *server.state.start_screencast_error.lock().unwrap() =
        Some((-32001, "Session with given id not found".into()));
    let _ = src.request_refresh(9).await;

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if !view.info().capabilities.view {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("view marked unavailable after session error");

    assert_eq!(view.info().unavailable_reason, Some(Reason::Unavailable));
    assert!(view.status().last_error.is_some());
    drop(sub);
}

async fn verify_watch_cold_start_and_action_bypass(
    server: &FakeCdpServer,
    view: &BrowserViewTarget,
    src: &Arc<dyn live_view::FrameSource>,
    sub: &mut FrameSubscription,
    sess: &str,
) -> (u64, u64) {
    server
        .state
        .wait_for_method_count("Page.startScreencast", 1)
        .await;
    let first_start = server
        .state
        .commands()
        .into_iter()
        .rfind(|c| c.method == "Page.startScreencast")
        .unwrap();
    assert_eq!(first_start.params["everyNthFrame"], 10);

    // Static cold page emits nothing within 200ms watch_idle_grace -> falls back once to everyNthFrame=1.
    pump_tasks().await;
    tokio::time::advance(Duration::from_millis(210)).await;
    server
        .state
        .wait_for_method_count("Page.startScreencast", 2)
        .await;
    let fallback_start = server
        .state
        .commands()
        .into_iter()
        .rfind(|c| c.method == "Page.startScreencast")
        .unwrap();
    assert_eq!(fallback_start.params["everyNthFrame"], 1);

    let stops0 = server.state.count_method("Page.stopScreencast");
    server.state.send_screencast_frame(
        sess,
        10,
        &minimal_jpeg(960, 600, 0),
        &default_meta(960.0, 600.0),
    );
    sub.frames.changed().await.expect("initial watch frame");
    let initial = sub.frames.borrow().clone().expect("initial");
    assert_eq!(initial.metadata.cause, Cause::Idle);
    assert!(!initial.metadata.repeated);
    server
        .state
        .wait_for_method_count("Page.stopScreencast", stops0 + 1)
        .await;

    // During 1.5s of the 3s Watch wait, no background startScreencast fires.
    tokio::time::advance(Duration::from_millis(1500)).await;
    pump_tasks().await;
    assert_eq!(server.state.count_method("Page.startScreencast"), 2);

    // Action `request_refresh(77)` bypasses Watch interval immediately with `everyNthFrame = 1`.
    src.request_refresh(77).await.expect("request_refresh");
    server
        .state
        .wait_for_method_count("Page.startScreencast", 3)
        .await;
    let refresh_start = server
        .state
        .commands()
        .into_iter()
        .rfind(|c| c.method == "Page.startScreencast")
        .unwrap();
    assert_eq!(refresh_start.params["everyNthFrame"], 1);
    assert!(view.status().refresh_pending);

    let stops1 = server.state.count_method("Page.stopScreencast");
    server.state.send_screencast_frame(
        sess,
        11,
        &minimal_jpeg(960, 600, 1024),
        &default_meta(960.0, 600.0),
    );
    sub.frames.changed().await.expect("action frame");
    let action_frame = sub.frames.borrow().clone().expect("action frame");
    assert_eq!(action_frame.metadata.cause, Cause::Action);
    assert_eq!(action_frame.metadata.action_seq, Some(77));
    assert!(!action_frame.metadata.repeated);
    assert!(!view.status().refresh_pending);
    assert!(!view.status().stale);
    server
        .state
        .wait_for_method_count("Page.stopScreencast", stops1 + 1)
        .await;

    (
        action_frame.metadata.frame_seq,
        action_frame.metadata.captured_at_ms,
    )
}

async fn verify_stale_timeout_and_live_emission_gating(
    server: &FakeCdpServer,
    view: &BrowserViewTarget,
    page: &browser_cdp::Page,
    src: &Arc<dyn live_view::FrameSource>,
    sub: &mut FrameSubscription,
    sess: &str,
    saved: (u64, u64),
) {
    // Let 750ms new-paint timeout expire with no frame -> stale + repeated preserves metadata.
    src.request_refresh(88).await.expect("request_refresh 88");
    server
        .state
        .wait_for_method_count("Page.startScreencast", 4)
        .await;
    pump_tasks().await;
    tokio::time::advance(Duration::from_millis(760)).await;

    sub.frames
        .changed()
        .await
        .expect("repeated stale frame emitted");
    let repeated = sub.frames.borrow().clone().expect("repeated frame");
    assert!(repeated.metadata.repeated);
    assert_eq!(repeated.metadata.cause, Cause::Idle);
    assert_eq!(repeated.metadata.frame_seq, saved.0);
    assert_eq!(repeated.metadata.captured_at_ms, saved.1);
    assert!(view.status().stale);
    assert!(view.status().refresh_pending);
    assert_eq!(view.info().capabilities.reason, Some(Reason::Stale));

    // Fresh frame clears stale; then a Page action triggers `Cause::Action` refresh.
    let stops2 = server.state.count_method("Page.stopScreencast");
    server.state.send_screencast_frame(
        sess,
        12,
        &minimal_jpeg(960, 600, 512),
        &default_meta(960.0, 600.0),
    );
    sub.frames
        .changed()
        .await
        .expect("fresh frame clears stale");
    assert!(!view.status().stale);
    assert!(!view.status().refresh_pending);
    server
        .state
        .wait_for_method_count("Page.stopScreencast", stops2 + 1)
        .await;

    let starts_now = server.state.count_method("Page.startScreencast");
    let _ = page.press_key("Enter").await.expect("page action");
    server
        .state
        .wait_for_method_count("Page.startScreencast", starts_now + 1)
        .await;
    let stops3 = server.state.count_method("Page.stopScreencast");
    server.state.send_screencast_frame(
        sess,
        13,
        &minimal_jpeg(960, 600, 600),
        &default_meta(960.0, 600.0),
    );
    sub.frames.changed().await.expect("page-action refreshed");
    assert_eq!(
        sub.frames.borrow().as_ref().unwrap().metadata.cause,
        Cause::Action
    );

    // `Page.frameNavigated` also notifies refresh on attached view.
    let starts_nav = server.state.count_method("Page.startScreencast");
    server
        .state
        .wait_for_method_count("Page.stopScreencast", stops3 + 1)
        .await;
    server.state.send_event(
        Some(sess),
        "Page.frameNavigated",
        &json!({ "frame": { "id": "frame-1", "url": "https://example.test/next" } }),
    );
    server
        .state
        .wait_for_method_count("Page.startScreencast", starts_nav + 1)
        .await;

    // Live mode `max_fps = 2` (500ms emission gating) ACKs all 6 burst frames without blocking.
    let mut live_prof = EffectiveProfile::live();
    live_prof.max_fps = 2;
    let acks_before = view.status().acks_sent;
    let emitted_before = view.status().frames_emitted;
    src.set_profile(live_prof).await.expect("set_profile live");

    for ack_id in 20..26_u64 {
        server.state.send_screencast_frame(
            sess,
            ack_id,
            &minimal_jpeg(1280, 720, 200),
            &default_meta(1280.0, 720.0),
        );
        tokio::time::advance(Duration::from_millis(5)).await;
        pump_tasks().await;
    }

    assert!(view.status().acks_sent >= acks_before + 6);
    assert!(server.state.count_method("Page.screencastFrameAck") >= 6);
    let emitted_delta = view.status().frames_emitted - emitted_before;
    assert!(
        emitted_delta <= 2,
        "expected <= 2 emitted under max_fps=2, got {emitted_delta}"
    );
    assert!(view.status().frames_dropped >= 4);
}

#[tokio::test(start_paused = true)]
async fn watch_live_timings_action_refresh_stale_and_emission_gating() {
    let server = FakeCdpServer::start().await;
    let browser = Browser::connect(&server.ws_url()).await.expect("connect");
    let page = browser.new_page("about:blank").await.expect("page");
    let sess = page.session_id().to_string();

    let cfg = BrowserViewConfig {
        new_paint_timeout: Duration::from_millis(750),
        watch_idle_grace: Duration::from_millis(200),
        watch_window_interval: Duration::from_secs(3),
        live_heartbeat_interval: Duration::from_millis(1000),
        ..BrowserViewConfig::default()
    };
    let view = page.live_view(cfg);
    let src = view.frame_source();

    let mut sub = src
        .subscribe(EffectiveProfile::watch())
        .await
        .expect("subscribe");

    let saved =
        verify_watch_cold_start_and_action_bypass(&server, &view, &src, &mut sub, &sess).await;
    verify_stale_timeout_and_live_emission_gating(
        &server, &view, &page, &src, &mut sub, &sess, saved,
    )
    .await;
    assert_eq!(
        server.state.capture_screenshot_calls.load(Ordering::SeqCst),
        0
    );
    drop(sub);
}

fn verify_pure_geometry_and_notes() {
    let geom = ViewportGeometry::from_metadata(
        browser_cdp::ScreencastMetadata {
            offset_top: 40.0,
            page_scale_factor: 2.0,
            device_width: 1000.0,
            device_height: 840.0,
            scroll_offset_x: 333.0,
            scroll_offset_y: 777.0,
            timestamp: None,
        },
        1,
    );
    assert!((geom.css_width - 500.0).abs() < f64::EPSILON);
    assert!((geom.css_height - 400.0).abs() < f64::EPSILON);
    let (cx, cy) = geom.normalized_to_css(0.5, 0.25).expect("valid coords");
    assert!((cx - 250.0).abs() < 1e-6);
    assert!((cy - 100.0).abs() < 1e-6);
    assert!(matches!(
        geom.normalized_to_css(-0.01, 0.5),
        Err(ViewError::InvalidRecord)
    ));
    assert!(matches!(
        geom.normalized_to_css(1.01, 0.5),
        Err(ViewError::InvalidRecord)
    ));
    assert!(matches!(
        geom.normalized_to_css(f64::NAN, 0.5),
        Err(ViewError::InvalidRecord)
    ));

    assert!(BROWSER_INPUT_LIMITATION.contains("viewport"));
    assert_eq!(
        BrowserViewTarget::input_limitation_note(),
        BROWSER_INPUT_LIMITATION
    );
}

async fn verify_pointer_wheel_key_text_and_reset(
    server: &FakeCdpServer,
    sink: &Arc<dyn live_view::InputSink>,
) {
    server.state.clear_commands();
    sink.inject(&InputEvent::Pointer {
        phase: PointerPhase::Move,
        x: 0.5,
        y: 0.25,
        button: None,
        buttons: 0,
        modifiers: vec![Modifier::Shift],
    })
    .await
    .expect("pointer move");

    assert!(matches!(
        sink.inject(&InputEvent::Wheel {
            x: 0.5,
            y: 0.5,
            delta_x: f64::INFINITY,
            delta_y: 0.0,
            modifiers: vec![],
        })
        .await,
        Err(ViewError::InvalidRecord)
    ));
    sink.inject(&InputEvent::Wheel {
        x: 0.5,
        y: 0.25,
        delta_x: 9999.0,
        delta_y: -9999.0,
        modifiers: vec![Modifier::Ctrl],
    })
    .await
    .expect("wheel clamped");

    sink.inject(&InputEvent::Key {
        phase: KeyPhase::Down,
        code: "ControlLeft".into(),
        key: "Control".into(),
        location: 1,
        repeat: false,
        modifiers: vec![Modifier::Ctrl],
    })
    .await
    .expect("ctrl down");
    sink.inject(&InputEvent::Key {
        phase: KeyPhase::Down,
        code: "KeyA".into(),
        key: "a".into(),
        location: 0,
        repeat: false,
        modifiers: vec![Modifier::Ctrl],
    })
    .await
    .expect("key a down");
    sink.inject(&InputEvent::Key {
        phase: KeyPhase::Down,
        code: "Enter".into(),
        key: "Enter".into(),
        location: 0,
        repeat: false,
        modifiers: vec![],
    })
    .await
    .expect("enter down");
    sink.inject(&InputEvent::Text {
        text: "ğüşöçıİĞÜŞÖÇ 🚀".into(),
    })
    .await
    .expect("turkish text");
    sink.inject(&InputEvent::Pointer {
        phase: PointerPhase::Down,
        x: 0.5,
        y: 0.25,
        button: Some(Button::Left),
        buttons: 1,
        modifiers: vec![],
    })
    .await
    .expect("pointer down");

    let cmds = server.state.commands();
    let move_cmd = cmds
        .iter()
        .find(|c| c.method == "Input.dispatchMouseEvent" && c.params["type"] == "mouseMoved")
        .expect("mouseMoved");
    assert!((move_cmd.params["x"].as_f64().unwrap() - 250.0).abs() < 1e-6);
    assert!((move_cmd.params["y"].as_f64().unwrap() - 100.0).abs() < 1e-6);
    assert_eq!(move_cmd.params["modifiers"], 8);

    let wheel_cmd = cmds
        .iter()
        .find(|c| c.method == "Input.dispatchMouseEvent" && c.params["type"] == "mouseWheel")
        .expect("mouseWheel");
    assert_eq!(wheel_cmd.params["deltaX"], 1200.0);
    assert_eq!(wheel_cmd.params["deltaY"], -1200.0);
    assert_eq!(wheel_cmd.params["modifiers"], 2);

    verify_key_text_and_reset_assertions(&cmds, server, sink).await;
}

async fn verify_key_text_and_reset_assertions(
    cmds: &[RecordedCommand],
    server: &FakeCdpServer,
    sink: &Arc<dyn live_view::InputSink>,
) {
    let a_cmd = cmds
        .iter()
        .find(|c| c.method == "Input.dispatchKeyEvent" && c.params["code"] == "KeyA")
        .expect("key a");
    assert_eq!(a_cmd.params["type"], "rawKeyDown");
    assert!(a_cmd.params.get("text").is_none());

    let enter_cmd = cmds
        .iter()
        .find(|c| c.method == "Input.dispatchKeyEvent" && c.params["code"] == "Enter")
        .expect("enter");
    assert_eq!(enter_cmd.params["type"], "keyDown");
    assert_eq!(enter_cmd.params["text"], "\r");

    let text_cmd = cmds
        .iter()
        .find(|c| c.method == "Input.insertText")
        .expect("insertText");
    assert_eq!(text_cmd.params["text"], "ğüşöçıİĞÜŞÖÇ 🚀");

    server.state.clear_commands();
    sink.inject(&InputEvent::Reset).await.expect("reset");
    let reset_cmds = server.state.commands();
    assert_eq!(reset_cmds.len(), 4);
    assert_eq!(reset_cmds[0].params["type"], "mouseReleased");
    assert_eq!(reset_cmds[1].params["code"], "Enter");
    assert_eq!(reset_cmds[2].params["code"], "KeyA");
    assert_eq!(reset_cmds[3].params["code"], "ControlLeft");

    server.state.clear_commands();
    sink.release_pressed_inputs()
        .await
        .expect("idempotent release");
    assert!(server.state.commands().is_empty());
}

async fn verify_partial_failure_and_cancel_safety(
    server: &FakeCdpServer,
    sink: &Arc<dyn live_view::InputSink>,
) {
    sink.inject(&InputEvent::Key {
        phase: KeyPhase::Down,
        code: "KeyZ".into(),
        key: "z".into(),
        location: 0,
        repeat: false,
        modifiers: vec![],
    })
    .await
    .expect("z down");
    server.state.fail_next_method.lock().unwrap().insert(
        "Input.dispatchKeyEvent".into(),
        (-32000, "transient fault".into()),
    );
    server.state.clear_commands();
    let res = sink
        .inject(&InputEvent::Key {
            phase: KeyPhase::Down,
            code: "KeyX".into(),
            key: "x".into(),
            location: 0,
            repeat: false,
            modifiers: vec![],
        })
        .await;
    assert!(matches!(res, Err(ViewError::PartialInjection)));
    assert!(server.state.commands().iter().any(|c| {
        c.method == "Input.dispatchKeyEvent"
            && c.params["type"] == "keyUp"
            && c.params["code"] == "KeyZ"
    }));

    // Abort `inject(Pointer::Down)` mid-flight, then `release_pressed_inputs()` still releases it.
    let reached = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    *server.state.hold_next_method.lock().expect("hold lock") = Some((
        "Input.dispatchMouseEvent".into(),
        Arc::clone(&reached),
        Arc::clone(&release),
    ));
    let sink_clone = Arc::clone(sink);
    let inflight = tokio::spawn(async move {
        sink_clone
            .inject(&InputEvent::Pointer {
                phase: PointerPhase::Down,
                x: 0.5,
                y: 0.5,
                button: Some(Button::Right),
                buttons: 2,
                modifiers: vec![],
            })
            .await
    });
    reached.notified().await;
    inflight.abort();
    let _ = inflight.await;
    release.notify_one();

    server.state.clear_commands();
    sink.release_pressed_inputs()
        .await
        .expect("release after aborted inject");
    assert!(server.state.commands().iter().any(|c| {
        c.method == "Input.dispatchMouseEvent"
            && c.params["type"] == "mouseReleased"
            && c.params["button"] == "right"
    }));
}

#[tokio::test]
async fn viewport_coordinate_mapping_input_injection_and_cancel_safety() {
    verify_pure_geometry_and_notes();

    let pin_log = Arc::new(Mutex::new(Vec::<bool>::new()));
    let pin_log_clone = Arc::clone(&pin_log);
    let cfg = BrowserViewConfig {
        takeover_pin: Some(Arc::new(move |active| {
            pin_log_clone.lock().unwrap().push(active);
        })),
        ..BrowserViewConfig::default()
    };

    let server = FakeCdpServer::start().await;
    let browser = Browser::connect(&server.ws_url()).await.expect("connect");
    let page = browser.new_page("about:blank").await.expect("page");
    let sess = page.session_id().to_string();

    let view = page.live_view(cfg);
    let src = view.frame_source();
    let sink = view.input_sink().expect("input_sink");
    let mut sub = src
        .subscribe(EffectiveProfile::live())
        .await
        .expect("subscribe");
    server
        .state
        .wait_for_method_count("Page.startScreencast", 1)
        .await;

    let zoomed_meta = json!({
        "offsetTop": 40.0,
        "pageScaleFactor": 2.0,
        "deviceWidth": 1000.0,
        "deviceHeight": 840.0,
        "scrollOffsetX": 333.0,
        "scrollOffsetY": 777.0,
        "timestamp": 1000.0
    });
    server
        .state
        .send_screencast_frame(&sess, 1, &minimal_jpeg(1000, 840, 0), &zoomed_meta);
    sub.frames.changed().await.expect("frame 1");
    let f1 = sub.frames.borrow().clone().unwrap();
    assert_eq!(f1.metadata.geometry_revision, 1);
    assert_eq!(view.status().geometry_revision, 1);
    assert_eq!(f1.metadata.source_rect.units, Units::CssPx);
    assert!((f1.metadata.source_rect.y - 20.0).abs() < 1e-6);
    assert_eq!(
        (
            f1.metadata.source_rect.width,
            f1.metadata.source_rect.height
        ),
        (500, 400)
    );

    server.state.send_screencast_frame(
        &sess,
        2,
        &minimal_jpeg(1200, 900, 0),
        &default_meta(1200.0, 900.0),
    );
    sub.frames.changed().await.expect("frame 2");
    assert_eq!(view.status().geometry_revision, 2);

    server
        .state
        .send_screencast_frame(&sess, 3, &minimal_jpeg(1000, 840, 0), &zoomed_meta);
    sub.frames.changed().await.expect("frame 3");
    assert_eq!(view.status().geometry_revision, 3);

    sink.set_takeover_active(true).await.expect("takeover on");
    assert_eq!(*pin_log.lock().unwrap(), vec![true]);

    verify_pointer_wheel_key_text_and_reset(&server, &sink).await;
    verify_partial_failure_and_cancel_safety(&server, &sink).await;

    sink.set_takeover_active(false).await.expect("takeover off");
    assert_eq!(*pin_log.lock().unwrap(), vec![true, false]);
    drop(sub);
}

#[cfg(unix)]
fn is_pid_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .output()
        .is_ok_and(|o| o.status.success())
}

#[cfg(windows)]
fn is_pid_alive(pid: u32) -> bool {
    let filter = format!("PID eq {pid}");
    std::process::Command::new("tasklist")
        .args(["/FI", &filter, "/NH", "/FO", "CSV"])
        .output()
        .is_ok_and(|o| {
            let s = String::from_utf8_lossy(&o.stdout);
            s.contains(&format!("\"{pid}\""))
        })
}

async fn spawn_parent_and_grandchild(pid_file: &PathBuf) -> (tokio::process::Child, u32, bool) {
    #[cfg(unix)]
    {
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c")
            .arg("sleep 60 & echo $! > \"$1\"; wait")
            .arg("--")
            .arg(pid_file)
            .process_group(0);
        let child = cmd.spawn().expect("spawn sh + sleep");
        for _ in 0..40 {
            if let Ok(txt) = tokio::fs::read_to_string(pid_file).await
                && let Ok(gpid) = txt.trim().parse::<u32>()
            {
                return (child, gpid, true);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("grandchild pid file never written on unix");
    }

    #[cfg(windows)]
    {
        let script = format!(
            "$p = Start-Process -FilePath 'cmd.exe' -ArgumentList '/c','ping -n 60 127.0.0.1 >nul' -PassThru -WindowStyle Hidden; \
             [System.IO.File]::WriteAllText('{}', [string]$p.Id); \
             Start-Sleep -Seconds 60",
            pid_file.display().to_string().replace('\'', "''")
        );
        let child = tokio::process::Command::new("powershell")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &script,
            ])
            .spawn()
            .expect("spawn powershell + cmd grandchild");
        for _ in 0..80 {
            if let Ok(txt) = tokio::fs::read_to_string(pid_file).await
                && let Ok(gpid) = txt.trim().parse::<u32>()
            {
                return (child, gpid, false);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("grandchild pid file never written on windows");
    }
}

#[tokio::test]
async fn browser_close_stops_live_view_before_close_and_kills_process_tree() {
    let server = FakeCdpServer::start().await;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let pid_file = std::env::temp_dir().join(format!("praxi-171-tree-{stamp}.pid"));

    let (child, grandchild_pid, own_pg) = spawn_parent_and_grandchild(&pid_file).await;
    let parent_pid = child.id().expect("child pid present");
    assert!(is_pid_alive(parent_pid));
    assert!(is_pid_alive(grandchild_pid));

    let browser = Browser::connect_with_owned_child(&server.ws_url(), child, own_pg)
        .await
        .expect("connect_with_owned_child");
    let page = browser.new_page("about:blank").await.expect("page");
    let view = page.live_view(BrowserViewConfig::default());
    let src = view.frame_source();
    let _sub = src
        .subscribe(EffectiveProfile::live())
        .await
        .expect("subscribe");
    server
        .state
        .wait_for_method_count("Page.startScreencast", 1)
        .await;

    server.state.clear_commands();
    browser.close().await.expect("browser close");

    let cmds = server.state.commands();
    let stop_idx = cmds
        .iter()
        .position(|c| c.method == "Page.stopScreencast")
        .expect("Page.stopScreencast called on close");
    let close_idx = cmds
        .iter()
        .position(|c| c.method == "Browser.close")
        .expect("Browser.close called");
    assert!(
        stop_idx < close_idx,
        "Page.stopScreencast ({stop_idx}) must run BEFORE Browser.close ({close_idx})"
    );

    for _ in 0..40 {
        if !is_pid_alive(parent_pid) && !is_pid_alive(grandchild_pid) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let _ = std::fs::remove_file(&pid_file);
    assert!(
        !is_pid_alive(parent_pid),
        "parent PID {parent_pid} still alive"
    );
    assert!(
        !is_pid_alive(grandchild_pid),
        "grandchild PID {grandchild_pid} still alive"
    );
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn live_chrome_screencast_zoom_scroll_turkish_and_close() {
    let browser = Browser::launch(LaunchOptions::default())
        .await
        .expect("launch Chrome/Edge");
    let html = r#"<!doctype html><html><body style="margin:0;height:3000px">
      <button id="hit" style="position:fixed;left:280px;top:160px;width:80px;height:80px"
        onclick="document.getElementById('inp').focus(); window.__clicked=(window.__clicked||0)+1">Go</button>
      <input id="inp" style="position:fixed;left:10px;top:10px" />
    </body></html>"#;
    let url = format!(
        "data:text/html;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(html)
    );
    let page = browser.new_page(&url).await.expect("open page");

    let _ = page.evaluate("window.scrollTo(0, 600)").await;
    page.client()
        .send_on(
            Some(page.session_id()),
            "Emulation.setPageScaleFactor",
            json!({ "pageScaleFactor": 2.0 }),
        )
        .await
        .expect("setPageScaleFactor");

    let view = page.live_view(BrowserViewConfig::default());
    let src = view.frame_source();
    let sink = view.input_sink().expect("input sink");

    let mut sub = src
        .subscribe(EffectiveProfile::live())
        .await
        .expect("subscribe");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(frame) = sub.frames.borrow().clone()
                && frame.payload.starts_with(&[0xFF, 0xD8])
            {
                return frame;
            }
            sub.frames.changed().await.expect("frame changed");
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "received real JPEG screencast frame timeout: status={:?} rejected_sc={} unsub_sc={} rejected={}",
            view.status(),
            browser.client().rejected_screencast_event_count(),
            browser.client().unsubscribed_screencast_event_count(),
            browser.client().rejected_event_count()
        )
    });

    sink.set_takeover_active(true).await.expect("takeover");
    for phase in [PointerPhase::Move, PointerPhase::Down, PointerPhase::Up] {
        let (button, buttons) = match phase {
            PointerPhase::Move => (None, 0),
            PointerPhase::Down => (Some(Button::Left), 1),
            PointerPhase::Up => (Some(Button::Left), 0),
        };
        sink.inject(&InputEvent::Pointer {
            phase,
            x: 0.5,
            y: 0.5,
            button,
            buttons,
            modifiers: vec![],
        })
        .await
        .expect("pointer phase");
    }
    sink.inject(&InputEvent::Text {
        text: "ğüşöçı".into(),
    })
    .await
    .expect("insertText");
    sink.set_takeover_active(false).await.expect("release");

    assert_eq!(page.evaluate("window.__clicked || 0").await.unwrap(), 1);
    assert_eq!(
        page.evaluate("document.getElementById('inp').value")
            .await
            .unwrap(),
        "ğüşöçı"
    );

    let _ = src.stop().await;
    drop(sub);
    browser.close().await.expect("clean close");
}
