use crate::{CONNECT_TIMEOUT, RESPONSE_TIMEOUT, STREAMS, oversized, request_body};
use async_trait::async_trait;
use axum::{Router, body::Body, extract::Request, http::StatusCode, response::IntoResponse};
use daemon_api::{AppState, PeerIdentity, PeerRole, peer::PeerName};
use hyper::{server::conn::http1, service::service_fn};
use hyper_util::rt::{TokioIo, TokioTimer};
use iroh::endpoint::Connection;
use peer_network::{Device, ProtocolHandler, Role};
use std::{convert::Infallible, sync::Arc};
use tokio::{sync::Semaphore, task::JoinSet};
use tower::ServiceExt;

/// Paired transport ingress. Register under `peer_network::ALPN_API` only.
pub struct ApiProtocol {
    router: Router,
    slots: Arc<Semaphore>,
}
impl ApiProtocol {
    #[cfg(test)]
    pub(crate) fn for_test(router: Router) -> Self {
        Self {
            router,
            slots: Arc::new(Semaphore::new(256)),
        }
    }

    #[must_use]
    pub fn new(state: AppState) -> Self {
        Self {
            router: daemon_api::peer_router(state),
            slots: Arc::new(Semaphore::new(256)),
        }
    }
}

#[async_trait]
impl ProtocolHandler for ApiProtocol {
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
        let slots = Arc::new(Semaphore::new(STREAMS));
        let mut tasks = JoinSet::new();
        loop {
            tokio::select! {
                biased;
                _ = conn.closed() => break,
                Some(_) = tasks.join_next(), if !tasks.is_empty() => {},
                accepted = conn.accept_bi() => {
                    let Ok((send, recv)) = accepted else { break };
                    let (Ok(local), Ok(global)) = (slots.clone().try_acquire_owned(), self.slots.clone().try_acquire_owned()) else {
                        drop((send, recv));
                        continue;
                    };
                    let (router, identity, name) = (self.router.clone(), identity.clone(), name.clone());
                    tasks.spawn(async move {
                        let _permits = (local, global);
                        let service = service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                            let (router, identity, name) = (router.clone(), identity.clone(), name.clone());
                            async move {
                                let mut req: Request = req.map(Body::new);
                                if oversized(req.headers()) {
                                    return Ok::<_, Infallible>(StatusCode::PAYLOAD_TOO_LARGE.into_response());
                                }
                                req.extensions_mut().insert(identity);
                                req.extensions_mut().insert(name);
                                let req = req.map(request_body);
                                let response = match tokio::time::timeout(RESPONSE_TIMEOUT, router.oneshot(req)).await {
                                    Ok(Ok(response)) => response,
                                    Ok(Err(never)) => match never {},
                                    Err(_) => StatusCode::GATEWAY_TIMEOUT.into_response(),
                                };
                                Ok(response)
                            }
                        });
                        let io = TokioIo::new(tokio::io::join(recv, send));
                        let _ = http1::Builder::new().timer(TokioTimer::new())
                            .header_read_timeout(CONNECT_TIMEOUT).max_buf_size(32 * 1024)
                            .keep_alive(false).serve_connection(io, service).await;
                    });
                }
            }
        }
        // Dropping JoinSet cancels all active bodies, including SSE, on revoke.
    }
}
