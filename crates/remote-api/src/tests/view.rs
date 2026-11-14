//! Live-view transport: binary passthrough, stream budget, stalls and loss.
//!
//! The fake services here stream ADR 0029 records produced with the real
//! `live-view` wire codec; the daemon itself is only used in `daemon.rs`.
use super::{config, pair, request, stop};
use crate::{ApiProtocol, ApiProxy, ProxyState, Tuning};
use axum::{
    Router,
    body::{Body, Bytes},
    extract::Path,
    http::{HeaderValue, StatusCode, header},
    response::Response,
    routing::{get, post, put},
};
use futures_util::{Stream, StreamExt, stream};
use live_view::{
    Cause, Codec, FrameMetadata, MEDIA_TYPE, RecordHeader, SourceRect, Units,
    wire::{Decoder, Record},
};
use peer_network::{ALPN_API, PeerNetwork, Role};
use serde_json::json;
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering::SeqCst},
    },
    time::Duration,
};
use tokio::sync::mpsc;
use uuid::Uuid;

pub(super) const FRAMES: &str = "/v1/view/sessions/00000000-0000-0000-0000-000000000001/frames";

pub(super) fn tuning(stall_ms: u64) -> Tuning {
    Tuning {
        response_timeout: Duration::from_secs(30),
        view_stall: Duration::from_millis(stall_ms),
    }
}

pub(super) async fn setup(
    dir: &std::path::Path,
    role: Role,
    protocol: ApiProtocol,
    tuning: Tuning,
) -> (PeerNetwork, PeerNetwork, ApiProxy) {
    let mut cfg = config(dir, "host", Role::Host);
    cfg.extra_protocols
        .push((ALPN_API.to_vec(), Arc::new(protocol)));
    let host = PeerNetwork::start(cfg, None).await.unwrap();
    let client = PeerNetwork::start(config(dir, "client", role), None)
        .await
        .unwrap();
    pair(&host, &client, role).await;
    let mut proxy = ApiProxy::start_with(client.clone(), host.id(), tuning)
        .await
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(10),
        proxy.state.wait_for(|s| *s == ProxyState::Connected),
    )
    .await
    .unwrap()
    .unwrap();
    (host, client, proxy)
}

fn metadata(seq: u64) -> FrameMetadata {
    FrameMetadata {
        target_id: Uuid::from_u128(2),
        generation: 4,
        frame_seq: seq,
        captured_at_ms: 1_794_132_000_000 + seq,
        cause: Cause::Action,
        action_seq: Some(12),
        codec: Codec::Jpeg,
        width: 960,
        height: 600,
        source_rect: SourceRect {
            x: 0.0,
            y: 0.0,
            width: 1280,
            height: 800,
            units: Units::PhysicalPx,
        },
        geometry_revision: 3,
        repeated: false,
    }
}

/// Deterministic, position-dependent pixels: any reordering or cut shows.
fn pixels(seq: u64, len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| u8::try_from((seq.wrapping_mul(31) + i as u64 * 7) % 251).unwrap())
        .collect()
}

pub(super) fn frame(seq: u64, len: usize) -> Vec<u8> {
    Record {
        header: RecordHeader::Frame {
            v: 1,
            session_id: Uuid::from_u128(1),
            sent_at_ms: 1_794_132_000_010 + seq,
            frame: metadata(seq),
        },
        payload: pixels(seq, len),
    }
    .encode()
    .unwrap()
}

pub(super) fn heartbeat(ms: u64) -> Vec<u8> {
    Record {
        header: RecordHeader::Heartbeat {
            v: 1,
            server_time_ms: ms,
        },
        payload: Vec::new(),
    }
    .encode()
    .unwrap()
}

/// A live-view response as the daemon sends it (plus hop-by-hop noise).
pub(super) fn view_response(body: Body) -> Response {
    let mut response = Response::new(body);
    let h = response.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(MEDIA_TYPE));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert("keep-alive", HeaderValue::from_static("timeout=5"));
    h.insert("x-hop", HeaderValue::from_static("secret"));
    h.insert(header::CONNECTION, HeaderValue::from_static("x-hop"));
    response
}

/// Decrements when the producer's body is dropped (client gone, stream reset).
pub(super) struct Active(pub Arc<AtomicUsize>);
impl Active {
    pub(super) fn enter(counter: &Arc<AtomicUsize>) -> Self {
        counter.fetch_add(1, SeqCst);
        Self(counter.clone())
    }
}
impl Drop for Active {
    fn drop(&mut self) {
        self.0.fetch_sub(1, SeqCst);
    }
}

pub(super) async fn eventually(what: &str, limit: Duration, mut done: impl FnMut() -> bool) {
    tokio::time::timeout(limit, async {
        while !done() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

/// Pull complete records out of an HTTP body, however it is fragmented.
pub(super) struct Records<S> {
    stream: S,
    decoder: Decoder,
    chunk: Bytes,
    pub(super) raw: Vec<u8>,
}
impl<S: Stream<Item = Result<Bytes, reqwest::Error>> + Unpin> Records<S> {
    pub(super) fn new(stream: S) -> Self {
        Self {
            stream,
            decoder: Decoder::default(),
            chunk: Bytes::new(),
            raw: Vec::new(),
        }
    }
    /// Next record, `Ok(None)` at a clean end of body, `Err` if the body was cut.
    pub(super) async fn next(&mut self) -> Result<Option<Record>, String> {
        loop {
            if !self.chunk.is_empty() {
                let (used, record) = self.decoder.feed(&self.chunk).map_err(|e| e.to_string())?;
                self.raw.extend_from_slice(&self.chunk[..used]);
                self.chunk = self.chunk.slice(used..);
                if let Some(record) = record {
                    return Ok(Some(record));
                }
                continue;
            }
            match self.stream.next().await {
                Some(Ok(chunk)) => self.chunk = chunk,
                Some(Err(e)) => return Err(e.to_string()),
                None => {
                    return self
                        .decoder
                        .finish()
                        .map(|()| None)
                        .map_err(|_| "body ended inside a record".to_owned());
                }
            }
        }
    }
}

fn frame_seq(record: &Record) -> Option<u64> {
    match &record.header {
        RecordHeader::Frame { frame, .. } => Some(frame.frame_seq),
        _ => None,
    }
}

pub(super) fn health_app() -> Router {
    Router::new().route("/v1/health", get(|| async { StatusCode::OK }))
}

/// Control traffic the ADR names: heartbeat, input, release, cancel.
pub(super) fn control_app(router: Router, hits: &Arc<AtomicUsize>) -> Router {
    let (a, b, c, d) = (hits.clone(), hits.clone(), hits.clone(), hits.clone());
    router
        .route(
            "/v1/view/sessions/{id}/heartbeat",
            put(move |Path(_): Path<String>| async move {
                a.fetch_add(1, SeqCst);
                axum::Json(json!({"ok": true}))
            }),
        )
        .route(
            "/v1/view/sessions/{id}/input",
            post(move |Path(_): Path<String>, body: Bytes| async move {
                b.fetch_add(1, SeqCst);
                axum::Json(json!({"accepted_through": body.len()}))
            }),
        )
        .route(
            "/v1/control/release",
            post(move || async move {
                c.fetch_add(1, SeqCst);
                axum::Json(json!({"released": true}))
            }),
        )
        .route(
            "/v1/view/sessions/{id}",
            axum::routing::delete(move |Path(_): Path<String>| async move {
                d.fetch_add(1, SeqCst);
                StatusCode::NO_CONTENT
            }),
        )
}

pub(super) async fn control_round_trip(proxy: &ApiProxy, within: Duration) {
    let client = reqwest::Client::new();
    let bearer = &proxy.token;
    let base = &proxy.base_url;
    tokio::time::timeout(within, async {
        let health = client.get(format!("{base}/v1/health")).send().await;
        assert_eq!(health.unwrap().status(), 200);
        let id = "00000000-0000-0000-0000-000000000001";
        let heartbeat = client
            .put(format!("{base}/v1/view/sessions/{id}/heartbeat"))
            .bearer_auth(bearer)
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(heartbeat.status(), 200);
        let input = client
            .post(format!("{base}/v1/view/sessions/{id}/input"))
            .bearer_auth(bearer)
            .json(&json!({"v": 1}))
            .send()
            .await
            .unwrap();
        assert_eq!(input.status(), 200);
        let release = client
            .post(format!("{base}/v1/control/release"))
            .bearer_auth(bearer)
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(release.status(), 200);
        let cancel = client
            .delete(format!("{base}/v1/view/sessions/{id}"))
            .bearer_auth(bearer)
            .send()
            .await
            .unwrap();
        assert_eq!(cancel.status(), 204);
    })
    .await
    .expect("control requests must complete while view bodies hold streams");
}

pub(super) fn view_get(proxy: &ApiProxy) -> reqwest::RequestBuilder {
    request(proxy, reqwest::Method::GET, FRAMES).header("Accept", MEDIA_TYPE)
}

#[tokio::test]
async fn golden_records_cross_unchanged_under_arbitrary_fragmentation() {
    let dir = tempfile::tempdir().unwrap();
    let (tx, rx) = mpsc::channel::<Bytes>(1);
    let rx = Arc::new(tokio::sync::Mutex::new(Some(rx)));
    let app = health_app().route(
        "/v1/view/sessions/{id}/frames",
        get(move |Path(_): Path<String>| {
            let rx = rx.clone();
            async move {
                let rx = rx.lock().await.take().unwrap();
                view_response(Body::from_stream(stream::unfold(rx, |mut rx| async move {
                    rx.recv().await.map(|b| (Ok::<_, std::io::Error>(b), rx))
                })))
            }
        }),
    );
    let (host, client, proxy) = setup(
        dir.path(),
        Role::Desktop,
        ApiProtocol::for_test(app),
        Tuning::default(),
    )
    .await;

    // Real ADR fixtures, a >64 KiB record and a 1.5 MiB one.
    let golden_heartbeat = include_bytes!("../../../live-view/tests/fixtures/heartbeat.bin");
    let golden_frame = include_bytes!("../../../live-view/tests/fixtures/frame.bin");
    let mut records: Vec<Vec<u8>> = vec![
        golden_heartbeat.to_vec(),
        golden_frame.to_vec(),
        frame(1, 70 * 1024),
        heartbeat(5),
        frame(2, 1536 * 1024),
        frame(3, 1),
        heartbeat(6),
    ];
    for seq in 4..40 {
        records.push(frame(seq, 1 + usize::try_from(seq).unwrap() * 997));
    }

    let response = view_get(&proxy).send().await.unwrap();
    assert_eq!(response.status(), 200);
    let headers = response.headers().clone();
    assert_eq!(headers[header::CONTENT_TYPE], MEDIA_TYPE);
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert!(
        !headers.contains_key("x-hop"),
        "nominated hop header leaked"
    );
    assert!(!headers.contains_key("keep-alive"));
    assert!(!headers.contains_key(header::CONTENT_ENCODING));
    let mut reader = Records::new(response.bytes_stream());

    // The first record arrives while the producer is still open and idle.
    tx.send(Bytes::from(records[0].clone())).await.unwrap();
    let first = tokio::time::timeout(Duration::from_secs(5), reader.next())
        .await
        .expect("first record must not wait for the end of the body")
        .unwrap()
        .unwrap();
    assert!(matches!(first.header, RecordHeader::Heartbeat { .. }));

    // The rest in pseudo-random fragments from 1 byte up to ~100 KiB, including
    // splits inside the 8-byte prefix, the JSON header and the payload.
    let rest: Vec<u8> = records[1..].concat();
    let producer = tokio::spawn(async move {
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut at = 0;
        while at < rest.len() {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            let size = match state >> 60 {
                0..=5 => 1 + usize::try_from(state >> 40).unwrap() % 16,
                6..=11 => 1 + usize::try_from(state >> 40).unwrap() % 1500,
                _ => 1 + usize::try_from(state >> 40).unwrap() % 100_000,
            };
            let end = (at + size).min(rest.len());
            tx.send(Bytes::copy_from_slice(&rest[at..end]))
                .await
                .unwrap();
            at = end;
        }
        tx // keep the body open until the test has checked the tail
    });
    let mut received = vec![first];
    while received.len() < records.len() {
        let record = tokio::time::timeout(Duration::from_secs(20), reader.next())
            .await
            .unwrap()
            .unwrap()
            .expect("body ended early");
        received.push(record);
    }
    // Byte-identical: re-encoding each parsed record reproduces what was sent,
    // and the raw bytes seen by the client equal the concatenation.
    for (record, sent) in received.iter().zip(&records) {
        assert_eq!(&record.encode().unwrap(), sent);
    }
    assert_eq!(reader.raw, records.concat());
    let big = &received[4];
    assert_eq!(frame_seq(big), Some(2));
    assert_eq!(big.payload, pixels(2, 1536 * 1024));
    let tx = producer.await.unwrap();
    drop(tx);
    assert!(
        reader.next().await.unwrap().is_none(),
        "clean end when the producer finishes"
    );
    stop(host, client, proxy).await;
}

#[tokio::test]
async fn ten_thousand_frames_slow_reader_bounded_and_no_files_written() {
    const COUNT: u64 = 10_000;
    const PAYLOAD: usize = 4096;
    let dir = tempfile::tempdir().unwrap();
    let produced = Arc::new(AtomicU64::new(0));
    let consumed = Arc::new(AtomicU64::new(0));
    let max_ahead = Arc::new(AtomicU64::new(0));
    let app = health_app().route(
        "/v1/view/sessions/{id}/frames",
        get({
            let (produced, consumed, max_ahead) =
                (produced.clone(), consumed.clone(), max_ahead.clone());
            move |Path(_): Path<String>| {
                let (produced, consumed, max_ahead) =
                    (produced.clone(), consumed.clone(), max_ahead.clone());
                async move {
                    view_response(Body::from_stream(stream::unfold(0u64, move |seq| {
                        let (produced, consumed, max_ahead) =
                            (produced.clone(), consumed.clone(), max_ahead.clone());
                        async move {
                            if seq >= COUNT {
                                return None;
                            }
                            // The frame is generated only when the body is
                            // polled, so this measures real backpressure.
                            let ahead = produced.load(SeqCst).saturating_sub(consumed.load(SeqCst));
                            max_ahead.fetch_max(ahead, SeqCst);
                            let record = frame(seq, PAYLOAD);
                            produced.fetch_add(record.len() as u64, SeqCst);
                            Some((Ok::<_, std::io::Error>(Bytes::from(record)), seq + 1))
                        }
                    })))
                }
            }
        }),
    );
    let (host, client, proxy) = setup(
        dir.path(),
        Role::Desktop,
        ApiProtocol::for_test(app),
        Tuning::default(),
    )
    .await;
    let files = || {
        let mut out = Vec::new();
        let mut dirs = vec![dir.path().to_path_buf()];
        while let Some(d) = dirs.pop() {
            for entry in std::fs::read_dir(d).unwrap().flatten() {
                let meta = entry.metadata().unwrap();
                if meta.is_dir() {
                    dirs.push(entry.path());
                } else {
                    out.push((entry.path(), meta.len()));
                }
            }
        }
        out.sort();
        out
    };
    let before = files();
    let response = view_get(&proxy).send().await.unwrap();
    assert_eq!(response.status(), 200);
    let mut reader = Records::new(response.bytes_stream());
    let mut seq = 0u64;
    while let Some(record) = reader.next().await.expect("body must not be cut") {
        assert_eq!(frame_seq(&record), Some(seq), "frames arrive in order");
        assert_eq!(record.payload, pixels(seq, PAYLOAD));
        seq += 1;
        consumed.fetch_add(reader.raw.len() as u64, SeqCst);
        reader.raw.clear();
        if seq.is_multiple_of(100) {
            // A slow reader: the producer must be held back, not buffered.
            tokio::time::sleep(Duration::from_millis(4)).await;
        }
    }
    assert_eq!(seq, COUNT);
    let total = produced.load(SeqCst);
    let ahead = max_ahead.load(SeqCst);
    eprintln!("10k frames: {total} bytes total, largest producer lead over reader {ahead} bytes");
    assert!(
        ahead < total / 2,
        "producer ran {ahead} of {total} bytes ahead: the body is being buffered"
    );
    assert_eq!(before, files(), "streaming a view body must write no files");
    stop(host, client, proxy).await;
}
