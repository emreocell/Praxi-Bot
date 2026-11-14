//! Deadlines, upstream failure, revoke and shutdown of live bodies and pending
//! input requests.
use super::view::{
    Active, FRAMES, Records, eventually, frame, health_app, heartbeat, setup, view_get,
    view_response,
};
use super::{pair, request, stop};
use crate::{ApiProtocol, ProxyState, Tuning, budget::Capacity};
use axum::{
    Router,
    body::{Body, Bytes},
    extract::Path,
    routing::{get, post},
};
use futures_util::stream;
use peer_network::Role;
use serde_json::json;
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering::SeqCst},
    },
    time::Duration,
};

const ROOMY: Capacity = Capacity {
    total: 256,
    long: 192,
    view: 16,
};

fn tuned(response_secs: u64) -> Tuning {
    Tuning {
        response_timeout: Duration::from_secs(response_secs),
        view_stall: Duration::from_secs(10),
    }
}

#[tokio::test]
async fn header_deadline_is_bounded_but_a_valid_long_body_outlives_it() {
    let dir = tempfile::tempdir().unwrap();
    let app = health_app()
        .route(
            "/slow-headers",
            get(|| async {
                tokio::time::sleep(Duration::from_secs(8)).await;
                "late"
            }),
        )
        .route(
            "/v1/view/sessions/{id}/frames",
            get(|Path(_): Path<String>| async {
                // Headers at once, then a body that lives 3x the header deadline.
                view_response(Body::from_stream(stream::unfold(0u64, |n| async move {
                    if n >= 30 {
                        return None;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    Some((Ok::<_, std::io::Error>(Bytes::from(heartbeat(n))), n + 1))
                })))
            }),
        );
    let tuning = tuned(1);
    let (host, client, proxy) = setup(
        dir.path(),
        Role::Desktop,
        ApiProtocol::for_test_with(app, ROOMY, crate::CONNECTION_CAPACITY, tuning),
        tuning,
    )
    .await;
    let started = std::time::Instant::now();
    let response = request(&proxy, reqwest::Method::GET, "/slow-headers")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 502, "response headers are bounded");
    assert!(started.elapsed() < Duration::from_secs(4));
    let response = view_get(&proxy).send().await.unwrap();
    assert_eq!(response.status(), 200);
    let started = std::time::Instant::now();
    let mut reader = Records::new(response.bytes_stream());
    let mut count = 0;
    while let Some(_record) = reader.next().await.expect("long body must survive") {
        count += 1;
    }
    assert_eq!(count, 30);
    assert!(
        started.elapsed() > Duration::from_secs(2),
        "the body outlived the 1 s header deadline"
    );
    stop(host, client, proxy).await;
}

#[tokio::test]
async fn upstream_failure_mid_record_cuts_the_body_and_never_continues_it() {
    let dir = tempfile::tempdir().unwrap();
    let whole = frame(1, 8000);
    let partial = frame(2, 8000);
    let (w, p) = (whole.clone(), partial[..3000].to_vec());
    // Error variant: the producer dies; clean variant: it ends inside a record.
    let app = health_app().route(
        "/v1/view/sessions/{id}/frames",
        get(move |Path(_): Path<String>| {
            let (w, p) = (w.clone(), p.clone());
            async move {
                // The failure comes after the head and two chunks were flushed.
                view_response(Body::from_stream(stream::unfold(0u8, move |step| {
                    let (w, p) = (w.clone(), p.clone());
                    async move {
                        match step {
                            0 => Some((Ok::<_, std::io::Error>(Bytes::from(w)), 1)),
                            1 => Some((Ok(Bytes::from(p)), 2)),
                            2 => {
                                tokio::time::sleep(Duration::from_millis(300)).await;
                                Some((Err(std::io::Error::other("producer failed")), 3))
                            }
                            _ => None,
                        }
                    }
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
    let response = view_get(&proxy).send().await.unwrap();
    assert_eq!(response.status(), 200);
    let mut reader = Records::new(response.bytes_stream());
    assert!(reader.next().await.unwrap().is_some(), "complete record");
    // The partial record is never completed and never followed by another.
    let cut = reader.next().await;
    assert!(
        cut.is_err(),
        "a cut stream must error, got {:?}",
        cut.as_ref().map(Option::is_some)
    );
    stop(host, client, proxy).await;

    let dir = tempfile::tempdir().unwrap();
    let (w, p) = (whole, partial[..3000].to_vec());
    let app = health_app().route(
        "/v1/view/sessions/{id}/frames",
        get(move |Path(_): Path<String>| {
            let (w, p) = (w.clone(), p.clone());
            async move {
                view_response(Body::from_stream(stream::iter([
                    Ok::<_, std::io::Error>(Bytes::from(w)),
                    Ok(Bytes::from(p)),
                ])))
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
    let response = view_get(&proxy).send().await.unwrap();
    let mut reader = Records::new(response.bytes_stream());
    assert!(reader.next().await.unwrap().is_some());
    let cut = reader.next().await;
    assert!(
        cut.is_err(),
        "the client's parser must see the unfinished record: {:?}",
        cut.as_ref().map(Option::is_some)
    );
    stop(host, client, proxy).await;
}

struct Fixture {
    active: Arc<AtomicUsize>,
    inputs: Arc<AtomicUsize>,
    pending_inputs: Arc<AtomicUsize>,
}

/// Idle view stream (heartbeats) that stops half way through a record, and an
/// input route that never answers: both are open when the link is cut.
fn hanging_app(f: &Fixture) -> Router {
    let active = f.active.clone();
    let (inputs, pending) = (f.inputs.clone(), f.pending_inputs.clone());
    health_app()
        .route(
            "/v1/view/sessions/{id}/frames",
            get(move |Path(_): Path<String>| {
                let guard = Active::enter(&active);
                async move {
                    let half = frame(9, 8000)[..4000].to_vec();
                    view_response(Body::from_stream(stream::unfold(
                        (guard, 0u8),
                        move |(guard, step)| {
                            let half = half.clone();
                            async move {
                                if step == 0 {
                                    return Some((
                                        Ok::<_, std::io::Error>(Bytes::from(half)),
                                        (guard, 1),
                                    ));
                                }
                                tokio::time::sleep(Duration::from_secs(3600)).await;
                                None
                            }
                        },
                    )))
                }
            }),
        )
        .route(
            "/v1/view/sessions/{id}/input",
            post(move |Path(_): Path<String>| {
                let guard = Active::enter(&pending);
                inputs.fetch_add(1, SeqCst);
                async move {
                    let _guard = guard;
                    std::future::pending::<()>().await;
                    "never"
                }
            }),
        )
}

#[tokio::test]
async fn revoke_cancels_view_body_and_pending_input_and_never_replays_the_post() {
    let dir = tempfile::tempdir().unwrap();
    let f = Fixture {
        active: Arc::default(),
        inputs: Arc::default(),
        pending_inputs: Arc::default(),
    };
    let (host, client, mut proxy) = setup(
        dir.path(),
        Role::Desktop,
        ApiProtocol::for_test(hanging_app(&f)),
        Tuning::default(),
    )
    .await;
    let response = view_get(&proxy).send().await.unwrap();
    assert_eq!(response.status(), 200);
    let mut reader = Records::new(response.bytes_stream());
    let pending = tokio::spawn({
        let (url, token) = (
            format!("{}{}", proxy.base_url, FRAMES.replace("frames", "input")),
            proxy.token.clone(),
        );
        async move {
            reqwest::Client::new()
                .post(url)
                .bearer_auth(token)
                .json(&json!({"v": 1}))
                .send()
                .await
        }
    });
    eventually(
        "input request reached the daemon",
        Duration::from_secs(5),
        || f.pending_inputs.load(SeqCst) == 1,
    )
    .await;
    assert_eq!(f.active.load(SeqCst), 1);

    // Cut the paired link in the middle of a record.
    host.revoke(&client.id()).unwrap();
    tokio::time::timeout(
        Duration::from_secs(5),
        proxy
            .state
            .wait_for(|s| matches!(s, ProxyState::Unreachable(_))),
    )
    .await
    .expect("ProxyState reflects the revoke")
    .unwrap();
    let cut = tokio::time::timeout(Duration::from_secs(5), reader.next())
        .await
        .expect("the body ends at once")
        .map(|r| r.map(|_| ()));
    assert!(
        cut.is_err(),
        "mid-record loss is an error, not an end: {:?}",
        cut.as_ref().map(Option::is_some)
    );
    let input = tokio::time::timeout(Duration::from_secs(5), pending)
        .await
        .expect("the pending input request is cancelled at once")
        .unwrap();
    assert!(
        input.map_or(true, |r| !r.status().is_success()),
        "input must not report success after the link was cut"
    );
    eventually(
        "daemon side dropped body and input",
        Duration::from_secs(5),
        || f.active.load(SeqCst) == 0 && f.pending_inputs.load(SeqCst) == 0,
    )
    .await;

    // Re-pairing recovers the same proxy. Nothing was replayed.
    pair(&host, &client, Role::Desktop).await;
    tokio::time::timeout(
        Duration::from_secs(15),
        proxy.state.wait_for(|s| *s == ProxyState::Connected),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(f.inputs.load(SeqCst), 1, "the POST was never sent again");
    let mut open = Vec::new();
    for _ in 0..crate::VIEW_STREAMS {
        let r = view_get(&proxy).send().await.unwrap();
        assert_eq!(r.status(), 200, "no stream budget leaked by the loss");
        open.push(r);
    }
    drop(open);
    stop(host, client, proxy).await;
}

#[tokio::test]
async fn proxy_shutdown_cancels_open_bodies_and_pending_requests() {
    let dir = tempfile::tempdir().unwrap();
    let f = Fixture {
        active: Arc::default(),
        inputs: Arc::default(),
        pending_inputs: Arc::default(),
    };
    let (host, client, proxy) = setup(
        dir.path(),
        Role::Desktop,
        ApiProtocol::for_test(hanging_app(&f)),
        Tuning::default(),
    )
    .await;
    let response = view_get(&proxy).send().await.unwrap();
    let mut reader = Records::new(response.bytes_stream());
    let input = tokio::spawn({
        let (url, token) = (
            format!("{}{}", proxy.base_url, FRAMES.replace("frames", "input")),
            proxy.token.clone(),
        );
        async move {
            reqwest::Client::new()
                .post(url)
                .bearer_auth(token)
                .json(&json!({"v": 1}))
                .send()
                .await
        }
    });
    eventually("input pending", Duration::from_secs(5), || {
        f.pending_inputs.load(SeqCst) == 1
    })
    .await;
    drop(proxy);
    let cut = tokio::time::timeout(Duration::from_secs(5), reader.next())
        .await
        .expect("body ends when the proxy stops");
    assert!(
        cut.is_err(),
        "proxy stop is an error, not a clean end: {:?}",
        cut.as_ref().map(Option::is_some)
    );
    let input = tokio::time::timeout(Duration::from_secs(5), input)
        .await
        .expect("pending request ends when the proxy stops")
        .unwrap();
    assert!(input.map_or(true, |r| !r.status().is_success()));
    eventually("remote side released", Duration::from_secs(5), || {
        f.active.load(SeqCst) == 0 && f.pending_inputs.load(SeqCst) == 0
    })
    .await;
    client.shutdown().await;
    host.shutdown().await;
}
