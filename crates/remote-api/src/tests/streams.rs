//! Stream budget, stalled consumers and permit release, end to end over iroh.
use super::request;
use super::stop;
use super::view::{
    Active, FRAMES, control_app, control_round_trip, eventually, frame, health_app, heartbeat,
    setup, tuning, view_get, view_response,
};
use crate::{ApiProtocol, Tuning, budget::Capacity};
use axum::{
    Router,
    body::{Body, Bytes},
    extract::Path,
    http::{HeaderValue, header},
    response::Response,
    routing::get,
};
use futures_util::{StreamExt, stream};
use live_view::MEDIA_TYPE;
use peer_network::{ALPN_API, Role};
use serde_json::Value;
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering::SeqCst},
    },
    time::Duration,
};

/// An endless stream of frames that counts its lifetime in `active`.
/// `chunk == 0` is an idle producer that only sends heartbeats.
fn endless_view(active: &Arc<AtomicUsize>, produced: &Arc<AtomicU64>, chunk: usize) -> Response {
    let guard = Active::enter(active);
    let produced = produced.clone();
    view_response(Body::from_stream(stream::unfold(
        (guard, 0u64),
        move |(guard, seq)| {
            let produced = produced.clone();
            async move {
                if chunk == 0 {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    return Some((
                        Ok::<_, std::io::Error>(Bytes::from(heartbeat(seq))),
                        (guard, seq + 1),
                    ));
                }
                let record = frame(seq, chunk);
                produced.fetch_add(record.len() as u64, SeqCst);
                Some((Ok(Bytes::from(record)), (guard, seq + 1)))
            }
        },
    )))
}

fn sse_response(active: &Arc<AtomicUsize>) -> Response {
    let guard = Active::enter(active);
    let mut response = Response::new(Body::from_stream(stream::unfold(guard, |guard| async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        Some((Ok::<_, std::io::Error>("data: alive\n\n"), guard))
    })));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    response
}

fn long_lived_app(
    active: &Arc<AtomicUsize>,
    produced: &Arc<AtomicU64>,
    hits: &Arc<AtomicUsize>,
    chunk: usize,
) -> Router {
    let (a, p, s) = (active.clone(), produced.clone(), active.clone());
    control_app(health_app(), hits)
        .route(
            "/v1/view/sessions/{id}/frames",
            get(move |Path(_): Path<String>| {
                let (a, p) = (a.clone(), p.clone());
                async move { endless_view(&a, &p, chunk) }
            }),
        )
        .route(
            "/v1/unlisted/stream",
            get({
                let (a, p) = (active.clone(), produced.clone());
                move || {
                    let (a, p) = (a.clone(), p.clone());
                    async move { endless_view(&a, &p, chunk) }
                }
            }),
        )
        .route(
            "/v1/events",
            get(move || {
                let s = s.clone();
                async move { sse_response(&s) }
            }),
        )
}

const ROOMY: Capacity = Capacity {
    total: 256,
    long: 192,
    view: 16,
};

#[tokio::test]
async fn view_streams_cannot_starve_control_traffic_and_permits_return() {
    let dir = tempfile::tempdir().unwrap();
    let (active, produced, hits) = (Arc::default(), Arc::default(), Arc::default());
    let (host, client, proxy) = setup(
        dir.path(),
        Role::Desktop,
        ApiProtocol::for_test(long_lived_app(&active, &produced, &hits, 0)),
        Tuning::default(),
    )
    .await;
    // Stalled readers: the bodies are never read, only held open.
    let mut views = Vec::new();
    for _ in 0..crate::VIEW_STREAMS {
        let response = view_get(&proxy).send().await.unwrap();
        assert_eq!(response.status(), 200);
        views.push(response);
    }
    let started = std::time::Instant::now();
    let fifth = view_get(&proxy).send().await.unwrap();
    assert_eq!(fifth.status(), 429, "excess view streams fail at once");
    assert_eq!(
        fifth.json::<Value>().await.unwrap()["error"]["code"],
        "too_many_streams"
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    control_round_trip(&proxy, Duration::from_secs(5)).await;

    // The long-lived pool (24) is shared with SSE: 20 more fit, the next is
    // refused promptly.
    let long = crate::STREAMS - crate::CONTROL_RESERVE;
    let mut events = Vec::new();
    for _ in 0..long - crate::VIEW_STREAMS {
        let response = request(&proxy, reqwest::Method::GET, "/v1/events")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        events.push(response);
    }
    let extra = request(&proxy, reqwest::Method::GET, "/v1/events")
        .send()
        .await
        .unwrap();
    assert_eq!(extra.status(), 429);
    // Heartbeat, input, release, cancel and health still complete, repeatedly,
    // with every long-lived slot taken: the reserve is real.
    for _ in 0..3 {
        control_round_trip(&proxy, Duration::from_secs(5)).await;
    }
    assert_eq!(hits.load(SeqCst), 4 * 4);
    assert_eq!(active.load(SeqCst), long);
    drop((views, events));
    eventually("upstream bodies dropped", Duration::from_secs(10), || {
        active.load(SeqCst) == 0
    })
    .await;
    // Permits are back: far more open/cancel cycles than the cap succeed.
    for _ in 0..crate::STREAMS * 2 {
        let response = view_get(&proxy).send().await.unwrap();
        assert_eq!(response.status(), 200);
        drop(response);
        eventually("cycled body dropped", Duration::from_secs(10), || {
            active.load(SeqCst) == 0
        })
        .await;
    }
    eventually("cycled bodies dropped", Duration::from_secs(10), || {
        active.load(SeqCst) == 0
    })
    .await;
    stop(host, client, proxy).await;
}

/// One raw HTTP/1.1 exchange on its own bi-stream of `conn`.
async fn raw_get(
    conn: &iroh::endpoint::Connection,
    path: &str,
) -> (hyper::Response<hyper::body::Incoming>, crate::AbortTask) {
    raw_get_with(conn, path, true).await
}

async fn raw_get_with(
    conn: &iroh::endpoint::Connection,
    path: &str,
    accept_view: bool,
) -> (hyper::Response<hyper::body::Incoming>, crate::AbortTask) {
    let (send, recv) = conn.open_bi().await.unwrap();
    let (mut sender, connection) = hyper::client::conn::http1::handshake(
        hyper_util::rt::TokioIo::new(tokio::io::join(recv, send)),
    )
    .await
    .unwrap();
    let driver = crate::AbortTask(tokio::spawn(async move {
        let _ = connection.await;
    }));
    let mut request = axum::http::Request::builder()
        .uri(path)
        .header("Host", "remote");
    if accept_view {
        request = request.header("Accept", MEDIA_TYPE);
    }
    let request = request.body(Body::empty()).unwrap();
    (sender.send_request(request).await.unwrap(), driver)
}

#[tokio::test]
async fn server_budget_limits_view_streams_per_connection_and_per_daemon() {
    let dir = tempfile::tempdir().unwrap();
    let (active, produced, hits) = (Arc::default(), Arc::default(), Arc::default());
    // Daemon-wide: only 6 view streams over any number of connections.
    let global = Capacity {
        total: 64,
        long: 48,
        view: 6,
    };
    let (host, client, proxy) = setup(
        dir.path(),
        Role::Desktop,
        ApiProtocol::for_test_with(
            long_lived_app(&active, &produced, &hits, 0),
            global,
            crate::CONNECTION_CAPACITY,
            Tuning::default(),
        ),
        Tuning::default(),
    )
    .await;
    let first = client.connect(&host.id(), ALPN_API).await.unwrap();
    let second = client.connect(&host.id(), ALPN_API).await.unwrap();
    let mut open = Vec::new();
    for _ in 0..crate::VIEW_STREAMS {
        let (response, driver) = raw_get(&first, FRAMES).await;
        assert_eq!(response.status(), 200);
        open.push((response, driver));
    }
    // Per connection: a fifth is refused by the daemon side itself, quickly.
    let started = std::time::Instant::now();
    let (response, _driver) = raw_get(&first, FRAMES).await;
    assert_eq!(response.status(), 429);
    assert_eq!(response.headers()[header::RETRY_AFTER], "1");
    assert!(started.elapsed() < Duration::from_secs(2));
    // A request that names neither the path nor the media type is still
    // classified by what comes back: a live-view body needs a view permit.
    let (response, _driver) = raw_get_with(&first, "/v1/unlisted/stream", false).await;
    assert_eq!(response.status(), 429);
    // Daemon-wide: the second connection only gets the 2 that are left.
    for expected in [200, 200, 429] {
        let (response, driver) = raw_get(&second, FRAMES).await;
        assert_eq!(response.status(), expected);
        open.push((response, driver));
    }
    // Ordinary requests still work on both connections.
    for conn in [&first, &second] {
        let (response, _driver) = raw_get_with(conn, "/v1/health", false).await;
        assert_eq!(response.status(), 200);
    }
    control_round_trip(&proxy, Duration::from_secs(5)).await;
    drop(open);
    eventually("server released bodies", Duration::from_secs(10), || {
        active.load(SeqCst) == 0
    })
    .await;
    for _ in 0..global.view * 2 {
        let (response, driver) = raw_get(&second, FRAMES).await;
        assert_eq!(response.status(), 200, "permits return after cancel/drop");
        drop((response, driver));
        // The daemon side frees a stream's permits once it notices the reset.
        eventually("server released body", Duration::from_secs(10), || {
            active.load(SeqCst) == 0
        })
        .await;
    }
    stop(host, client, proxy).await;
}

#[tokio::test]
async fn stalled_view_reader_cancels_upstream_cuts_the_body_and_frees_permits() {
    let dir = tempfile::tempdir().unwrap();
    let (active, produced, hits) = (Arc::default(), Arc::default(), Arc::default());
    // 64 KiB records as fast as the transport takes them; 500 ms stall limit.
    let (host, client, proxy) = setup(
        dir.path(),
        Role::Desktop,
        ApiProtocol::for_test_with(
            long_lived_app(&active, &produced, &hits, 64 * 1024),
            ROOMY,
            crate::CONNECTION_CAPACITY,
            tuning(500),
        ),
        tuning(500),
    )
    .await;
    let response = view_get(&proxy).send().await.unwrap();
    assert_eq!(response.status(), 200);
    let mut body = response.bytes_stream();
    // Read a little, then stop reading but keep the response open.
    for _ in 0..4 {
        body.next().await.unwrap().unwrap();
    }
    eventually(
        "stalled body cancelled upstream",
        Duration::from_secs(60),
        || active.load(SeqCst) == 0,
    )
    .await;
    let lead = produced.load(SeqCst);
    eprintln!("stalled reader: producer wrote {lead} bytes before it was cancelled");
    // The cut is visible to the reader as an error, never as a clean end.
    let end = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            match body.next().await {
                Some(Ok(_)) => {}
                other => break other,
            }
        }
    })
    .await
    .unwrap();
    assert!(
        matches!(end, Some(Err(_))),
        "a cancelled stream must not look like a finished one: {end:?}"
    );
    // Its permits are free again at once.
    for _ in 0..crate::VIEW_STREAMS + 2 {
        let response = view_get(&proxy).send().await.unwrap();
        assert_eq!(response.status(), 200);
        drop(response);
        eventually("cycled body dropped", Duration::from_secs(10), || {
            active.load(SeqCst) == 0
        })
        .await;
    }
    stop(host, client, proxy).await;
}
