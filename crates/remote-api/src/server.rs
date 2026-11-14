use crate::{
    CONNECT_TIMEOUT, CONNECTION_BODY_BUDGET, CONNECTION_CAPACITY, SERVER_CONTROL_RESERVE,
    SERVER_STREAMS, SERVER_VIEW_STREAMS, STREAMS, Tuning,
    budget::{self, Budget, Capacity, Class, Held},
    guard::{GuardedBody, Lease},
    oversized, request_body,
};
use async_trait::async_trait;
use axum::{
    Router,
    body::Body,
    extract::Request,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use daemon_api::{AppState, PeerIdentity, PeerRole, peer::PeerName};
use hyper::{server::conn::http1, service::service_fn};
use hyper_util::rt::{TokioIo, TokioTimer};
use iroh::endpoint::Connection;
use peer_network::{Device, ProtocolHandler, Role};
use std::{
    convert::Infallible,
    sync::{Arc, Mutex},
};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

const SERVER_CAPACITY: Capacity = Capacity {
    total: SERVER_STREAMS,
    long: SERVER_STREAMS - SERVER_CONTROL_RESERVE,
    view: SERVER_VIEW_STREAMS,
};

/// Paired transport ingress. Register under `peer_network::ALPN_API` only.
pub struct ApiProtocol {
    router: Router,
    global: Budget,
    connection: Capacity,
    tuning: Tuning,
}
impl ApiProtocol {
    #[cfg(test)]
    pub(crate) fn for_test(router: Router) -> Self {
        Self::for_test_with(
            router,
            SERVER_CAPACITY,
            CONNECTION_CAPACITY,
            Tuning::default(),
        )
    }

    #[cfg(test)]
    pub(crate) fn for_test_with(
        router: Router,
        global: Capacity,
        connection: Capacity,
        tuning: Tuning,
    ) -> Self {
        Self {
            router,
            global: Budget::new(global),
            connection,
            tuning,
        }
    }

    #[must_use]
    pub fn new(state: AppState) -> Self {
        Self {
            router: daemon_api::peer_router(state),
            global: Budget::new(SERVER_CAPACITY),
            connection: CONNECTION_CAPACITY,
            tuning: Tuning::default(),
        }
    }
}

fn overloaded(class: Class) -> Response {
    let (code, message) = match class {
        Class::View => ("too_many_streams", "too many live-view streams"),
        Class::Stream => ("too_many_streams", "too many open event streams"),
        Class::Ordinary => ("unavailable", "no stream capacity available"),
    };
    let status = if class == Class::Ordinary {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::TOO_MANY_REQUESTS
    };
    let mut response = (
        status,
        axum::Json(serde_json::json!({"error": {"code": code, "message": message}})),
    )
        .into_response();
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
    response
}

/// Per-stream admission. The stream already holds an ordinary permit on the
/// connection and server budgets; classification may raise it to a long-lived
/// class on both, or fail at once.
struct Admission {
    local: Budget,
    global: Budget,
    held: Mutex<Option<(Held, Held)>>,
}

impl Admission {
    fn upgrade(&self, class: Class) -> Result<(), Class> {
        let mut guard = self.held.lock().map_err(|_| Class::Ordinary)?;
        let (local, global) = guard.as_mut().ok_or(Class::Ordinary)?;
        self.local.upgrade(local, class).map_err(|o| o.0)?;
        self.global.upgrade(global, class).map_err(|o| o.0)
    }

    fn class(&self) -> Class {
        self.held
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|(l, _)| l.class))
            .unwrap_or(Class::Ordinary)
    }

    fn take(&self) -> Option<(Held, Held)> {
        self.held.lock().ok().and_then(|mut g| g.take())
    }
}

#[async_trait]
impl ProtocolHandler for ApiProtocol {
    fn max_bidi_streams(&self) -> usize {
        // The connectivity probe consumes one stream id. Quinn only returns that
        // credit after more than an eighth of the window has closed, which is
        // later than the STREAMS application requests that follow the probe.
        STREAMS + 1
    }

    fn receive_window(&self) -> u64 {
        CONNECTION_BODY_BUDGET
    }

    async fn handle(&self, conn: Connection, device: Device) {
        let role = match device.role {
            Role::Desktop => PeerRole::Desktop,
            Role::Phone => PeerRole::Phone,
            Role::Node => PeerRole::Node,
            Role::Host => PeerRole::Host,
        };
        let identity = PeerIdentity {
            key: device.id,
            role,
        };
        let name = PeerName(device.name);
        let local = Budget::new(self.connection);
        let mut tasks = JoinSet::new();
        loop {
            tokio::select! {
                biased;
                _ = conn.closed() => break,
                Some(_) = tasks.join_next(), if !tasks.is_empty() => {},
                accepted = conn.accept_bi() => {
                    let Ok((send, recv)) = accepted else { break };
                    let (Ok(local_held), Ok(global_held)) = (local.acquire(Class::Ordinary), self.global.acquire(Class::Ordinary)) else {
                        drop((send, recv));
                        continue;
                    };
                    let admission = Arc::new(Admission {
                        local: local.clone(),
                        global: self.global.clone(),
                        held: Mutex::new(Some((local_held, global_held))),
                    });
                    tasks.spawn(serve_stream(
                        recv,
                        send,
                        self.router.clone(),
                        identity.clone(),
                        name.clone(),
                        admission,
                        self.tuning,
                    ));
                }
            }
        }
        // Dropping JoinSet cancels all active bodies, including SSE, on revoke.
    }
}

/// Serve exactly one HTTP/1.1 exchange on one bidirectional stream.
async fn serve_stream(
    recv: iroh::endpoint::RecvStream,
    send: iroh::endpoint::SendStream,
    router: Router,
    identity: PeerIdentity,
    name: PeerName,
    admission: Arc<Admission>,
    tuning: Tuning,
) {
    // Fired only by the stall watchdog: it ends this one stream.
    let stalled = CancellationToken::new();
    let service =
        {
            let (admission, stalled) = (admission.clone(), stalled.clone());
            service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                let (router, identity, name) = (router.clone(), identity.clone(), name.clone());
                let (admission, stalled) = (admission.clone(), stalled.clone());
                async move {
                    let mut req: Request = req.map(Body::new);
                    if oversized(req.headers()) {
                        return Ok::<_, Infallible>(StatusCode::PAYLOAD_TOO_LARGE.into_response());
                    }
                    // Transport identity only: nothing a client sends can name a
                    // peer key or a role.
                    budget::strip_identity_headers(req.headers_mut());
                    let class = budget::request_class(req.method(), req.uri(), req.headers());
                    if let Err(class) = admission.upgrade(class) {
                        return Ok(overloaded(class));
                    }
                    req.extensions_mut().insert(identity);
                    req.extensions_mut().insert(name);
                    let req = req.map(request_body);
                    let response =
                        match tokio::time::timeout(tuning.response_timeout, router.oneshot(req))
                            .await
                        {
                            Ok(Ok(response)) => response,
                            Ok(Err(never)) => match never {},
                            Err(_) => return Ok(StatusCode::GATEWAY_TIMEOUT.into_response()),
                        };
                    // The response decides the final cost, so an unusual request
                    // spelling cannot hide a long-lived body.
                    let actual = budget::response_class(response.headers());
                    if actual > admission.class()
                        && let Err(class) = admission.upgrade(actual)
                    {
                        return Ok(overloaded(class));
                    }
                    let Some(held) = admission.take() else {
                        return Ok(StatusCode::INTERNAL_SERVER_ERROR.into_response());
                    };
                    let stall = (actual == Class::View).then_some(tuning.view_stall);
                    let lease = Lease::new(stalled, held);
                    let (parts, body) = response.into_parts();
                    Ok(Response::from_parts(
                        parts,
                        Body::new(GuardedBody::new(body, lease, stall)),
                    ))
                }
            })
        };
    let io = TokioIo::new(tokio::io::join(recv, send));
    let serve = http1::Builder::new()
        .timer(TokioTimer::new())
        .header_read_timeout(CONNECT_TIMEOUT)
        .max_buf_size(32 * 1024)
        .keep_alive(false)
        .serve_connection(io, service);
    // A stalled consumer drops the connection future: the body, the upstream
    // producer and the QUIC stream go with it.
    tokio::select! {
        _ = serve => {}
        () = stalled.cancelled() => {}
    }
    drop(admission);
}

#[cfg(test)]
mod limits {
    use super::ApiProtocol;
    use crate::{CONNECTION_BODY_BUDGET, MAX_REQUEST_BYTES, STREAMS};
    use peer_network::ProtocolHandler;

    #[test]
    fn advertises_per_connection_stream_and_body_budget() {
        let protocol = ApiProtocol::for_test(axum::Router::new());
        assert_eq!(protocol.max_bidi_streams(), STREAMS + 1);
        assert_eq!(protocol.receive_window(), CONNECTION_BODY_BUDGET);
        assert_eq!(
            CONNECTION_BODY_BUDGET,
            u64::try_from(STREAMS).unwrap() * u64::try_from(MAX_REQUEST_BYTES).unwrap()
        );
    }
}
