use crate::{
    AbortTask, CONNECT_TIMEOUT, CONNECTION_CAPACITY, Tuning,
    budget::{self, Budget, Class},
    guard::{GuardedBody, Lease},
    oversized, request_body,
};
use axum::{
    Router,
    body::Body,
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use http_body_util::BodyExt;
use hyper::client::conn::http1;
use hyper_util::rt::TokioIo;
use iroh::{PublicKey, endpoint::Connection};
use peer_network::{ALPN_API, PeerNetwork};
use std::{fmt::Write as _, io, time::Duration};
use tokio::{net::TcpListener, sync::watch};

/// Connection status of the remote API tunnel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProxyState {
    Connected,
    Reconnecting,
    Unreachable(String),
}

/// A guarded loopback endpoint. Dropping it cancels serving and reconnecting.
pub struct ApiProxy {
    pub base_url: String,
    pub token: String,
    pub state: watch::Receiver<ProxyState>,
    _listener: AbortTask,
    _manager: AbortTask,
    cancel: tokio_util::sync::CancellationToken,
}

impl Drop for ApiProxy {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

struct CloseConnection(Connection);
impl Drop for CloseConnection {
    fn drop(&mut self) {
        self.0.close(0u32.into(), b"proxy stopped");
    }
}

#[derive(Clone)]
struct Proxy {
    token: String,
    conn: watch::Receiver<Option<Connection>>,
    budget: Budget,
    tuning: Tuning,
    cancel: tokio_util::sync::CancellationToken,
}

impl ApiProxy {
    /// Start an ephemeral loopback proxy for a paired daemon.
    ///
    /// # Errors
    /// Returns an error if randomness or the loopback bind fails, or the peer
    /// is not paired locally. Dial failures are reported through `state`.
    pub async fn start(net: PeerNetwork, peer: PublicKey) -> io::Result<Self> {
        Self::start_with(net, peer, Tuning::default()).await
    }

    pub(crate) async fn start_with(
        net: PeerNetwork,
        peer: PublicKey,
        tuning: Tuning,
    ) -> io::Result<Self> {
        if net.device(&peer).is_none() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "remote peer is not paired",
            ));
        }
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let base_url = format!("http://{}", listener.local_addr()?);
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|e| io::Error::other(e.to_string()))?;
        let token = bytes
            .iter()
            .fold(String::with_capacity(64), |mut token, byte| {
                let _ = write!(token, "{byte:02x}");
                token
            });
        let (status, state) = watch::channel(ProxyState::Reconnecting);
        let (connections, conn) = watch::channel(None);
        let manager = AbortTask(tokio::spawn(async move {
            let mut delay = Duration::from_millis(100);
            loop {
                let dial =
                    tokio::time::timeout(CONNECT_TIMEOUT, net.connect(&peer, ALPN_API)).await;
                match dial {
                    Ok(Ok(conn)) => {
                        // Verify HTTP ingress before announcing connectivity: QUIC
                        // alone may complete before a revoked key is rejected.
                        let _close = CloseConnection(conn.clone());
                        match tokio::time::timeout(CONNECT_TIMEOUT, probe(&conn)).await {
                            Ok(Ok(())) => {
                                connections.send_replace(Some(conn.clone()));
                                status.send_replace(ProxyState::Connected);
                                delay = Duration::from_millis(100);
                                let reason = conn.closed().await;
                                connections.send_replace(None);
                                let revoked = matches!(&reason, iroh::endpoint::ConnectionError::ApplicationClosed(e)
                                    if e.error_code == peer_network::proto::CLOSE_REVOKED.into() || e.error_code == peer_network::proto::CLOSE_NOT_PAIRED.into());
                                if revoked {
                                    status
                                        .send_replace(ProxyState::Unreachable(reason.to_string()));
                                } else {
                                    status.send_replace(ProxyState::Reconnecting);
                                }
                            }
                            Ok(Err(e)) => {
                                status.send_replace(ProxyState::Unreachable(e.to_string()));
                            }
                            Err(_) => {
                                status.send_replace(ProxyState::Unreachable(
                                    "health probe timed out".into(),
                                ));
                            }
                        }
                    }
                    Ok(Err(e)) => {
                        status.send_replace(ProxyState::Unreachable(e.to_string()));
                    }
                    Err(_) => {
                        status.send_replace(ProxyState::Unreachable("connection timed out".into()));
                    }
                }
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(5));
                // Keep Unreachable observable between bounded reconnect attempts.
            }
        }));
        let cancel = tokio_util::sync::CancellationToken::new();
        let proxy = Proxy {
            token: token.clone(),
            conn,
            budget: Budget::new(CONNECTION_CAPACITY),
            tuning,
            cancel: cancel.clone(),
        };
        let app = Router::new().fallback(forward).with_state(proxy);
        let stop = cancel.clone();
        let listener = AbortTask(tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(stop.cancelled_owned())
                .await;
        }));
        Ok(Self {
            base_url,
            token,
            state,
            _listener: listener,
            _manager: manager,
            cancel,
        })
    }
}

async fn probe(conn: &Connection) -> io::Result<()> {
    let (send, recv) = conn.open_bi().await.map_err(io::Error::other)?;
    let (mut sender, connection) = http1::handshake(TokioIo::new(tokio::io::join(recv, send)))
        .await
        .map_err(io::Error::other)?;
    // Finish the exchange. Aborting the driver leaves the bidi stream open and
    // holds one unit of the connection's stream credit.
    let driver = tokio::spawn(async move {
        let _ = connection.await;
    });
    let result = probe_health(&mut sender).await;
    if result.is_err() {
        driver.abort();
        return result;
    }
    drop(sender);
    let _ = tokio::time::timeout(CONNECT_TIMEOUT, driver).await;
    Ok(())
}

async fn probe_health(
    sender: &mut hyper::client::conn::http1::SendRequest<Body>,
) -> io::Result<()> {
    let req = Request::builder()
        .uri("/v1/health")
        .header(header::HOST, "iroh.local")
        .body(Body::empty())
        .map_err(io::Error::other)?;
    let response = sender.send_request(req).await.map_err(io::Error::other)?;
    if response.status() != StatusCode::OK {
        return Err(io::Error::other("remote API health probe rejected"));
    }
    response
        .into_body()
        .collect()
        .await
        .map_err(io::Error::other)?;
    Ok(())
}

fn error(status: StatusCode, message: &str) -> Response {
    (status, axum::Json(serde_error(status, message))).into_response()
}

fn serde_error(status: StatusCode, message: &str) -> serde_json::Value {
    let code = match status {
        StatusCode::UNAUTHORIZED => "unauthorized",
        StatusCode::FORBIDDEN => "forbidden",
        StatusCode::PAYLOAD_TOO_LARGE => "payload_too_large",
        StatusCode::TOO_MANY_REQUESTS => "too_many_streams",
        _ => "unavailable",
    };
    serde_json::json!({"error": {"code": code, "message": message}})
}

fn loopback(host: &str) -> bool {
    // Same exact accepted hostnames and port syntax as daemon-api::auth.
    let h = host.trim().to_ascii_lowercase();
    let port = |p: &str| !p.is_empty() && p.len() <= 5 && p.bytes().all(|b| b.is_ascii_digit());
    let name = if let Some(rest) = h.strip_prefix('[') {
        match rest.split_once(']') {
            Some((name, tail)) if tail.is_empty() || tail.strip_prefix(':').is_some_and(port) => {
                name
            }
            _ => return false,
        }
    } else {
        match h.rsplit_once(':') {
            Some((name, p)) if port(p) => name,
            Some(_) => return false,
            None => &h,
        }
    };
    matches!(name, "localhost" | "127.0.0.1" | "::1")
}

fn ct_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

fn cors(response: &mut Response, origin: Option<HeaderValue>, preflight: bool) {
    if let Some(origin) = origin {
        let headers = response.headers_mut();
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
        headers.insert(header::VARY, HeaderValue::from_static("Origin"));
        if preflight {
            headers.insert(
                header::ACCESS_CONTROL_ALLOW_METHODS,
                HeaderValue::from_static("GET, POST, PUT, DELETE, OPTIONS"),
            );
            headers.insert(
                header::ACCESS_CONTROL_ALLOW_HEADERS,
                HeaderValue::from_static("authorization, content-type, last-event-id"),
            );
            headers.insert(
                header::ACCESS_CONTROL_MAX_AGE,
                HeaderValue::from_static("600"),
            );
        }
    }
}

async fn forward(State(proxy): State<Proxy>, mut req: Request) -> Response {
    if !req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .is_some_and(loopback)
    {
        return error(StatusCode::FORBIDDEN, "host not allowed");
    }
    let origin = match req.headers().get(header::ORIGIN) {
        None => None,
        Some(value)
            if value.to_str().is_ok_and(|s| {
                matches!(
                    s,
                    "tauri://localhost" | "http://tauri.localhost" | "https://tauri.localhost"
                )
            }) =>
        {
            Some(value.clone())
        }
        Some(_) => return error(StatusCode::FORBIDDEN, "origin not allowed"),
    };
    let preflight = req.method() == Method::OPTIONS && origin.is_some();
    if preflight {
        let mut response = StatusCode::NO_CONTENT.into_response();
        cors(&mut response, origin, true);
        return response;
    }
    let bearer = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, token)| token.trim());
    let query_token = if req.method() == Method::GET && req.uri().path() == "/v1/events" {
        req.uri()
            .query()
            .and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("token=")))
            .filter(|t| !t.is_empty())
    } else {
        None
    };
    // Preserve the daemon's public health probe and signed webhook exception.
    let public = (req.method() == Method::GET && req.uri().path() == "/v1/health")
        || (req.method() == Method::POST && req.uri().path().starts_with("/v1/webhooks/"));
    if !public
        && !bearer
            .or(query_token)
            .is_some_and(|t| ct_eq(t, &proxy.token))
    {
        let mut response = error(StatusCode::UNAUTHORIZED, "missing or invalid bearer token");
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        cors(&mut response, origin, false);
        return response;
    }
    // View and control routes take the bearer in an Authorization header only.
    // Reject (rather than strip) a credential in the URL, after authentication
    // like the daemon: it would otherwise reach history, logs and referrers.
    if budget::view_credential_query(req.uri()) {
        let mut response = error(StatusCode::BAD_REQUEST, "URL credentials are forbidden");
        cors(&mut response, origin, false);
        return response;
    }
    let mut response = if oversized(req.headers()) {
        error(StatusCode::PAYLOAD_TOO_LARGE, "request body too large")
    } else {
        // Never forward a local credential, even a query token supplied alongside
        // Authorization, or a percent-encoded token parameter on another route.
        let query = req.uri().query().map(|q| {
            url::form_urlencoded::parse(q.as_bytes())
                .into_owned()
                .filter(|(k, _)| k != "token")
                .collect::<Vec<_>>()
        });
        let mut target = req.uri().path().to_owned();
        if let Some(query) = query {
            let query = url::form_urlencoded::Serializer::new(String::new())
                .extend_pairs(query)
                .finish();
            if !query.is_empty() {
                target.push('?');
                target.push_str(&query);
            }
        }
        let Ok(target) = target.parse::<Uri>() else {
            return error(StatusCode::BAD_REQUEST, "invalid request target");
        };
        *req.uri_mut() = target;
        strip_hop_headers(req.headers_mut());
        // Identity and role come from the pairing, never from a client header.
        budget::strip_identity_headers(req.headers_mut());
        req.headers_mut().remove(header::AUTHORIZATION);
        req.headers_mut().remove(header::PROXY_AUTHORIZATION);
        req.headers_mut()
            .insert(header::HOST, HeaderValue::from_static("iroh.local"));
        let class = budget::request_class(req.method(), req.uri(), req.headers());
        tunnel(&proxy, req.map(request_body), class).await
    };
    cors(&mut response, origin, false);
    response
}

fn strip_hop_headers(headers: &mut HeaderMap) {
    let nominated: Vec<_> = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|s| s.trim().to_owned())
        .collect();
    for name in nominated {
        headers.remove(name);
    }
    for name in [
        "connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
    ] {
        headers.remove(name);
    }
}

/// Why an exchange could not start or continue.
enum Refusal {
    /// A budget pool was full: fail fast rather than queue.
    Overloaded(Class),
    Io(io::Error),
}

impl From<io::Error> for Refusal {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

fn refusal(refusal: Refusal) -> Response {
    match refusal {
        Refusal::Overloaded(Class::View) => {
            error(StatusCode::TOO_MANY_REQUESTS, "too many live-view streams")
        }
        Refusal::Overloaded(Class::Stream) => {
            error(StatusCode::TOO_MANY_REQUESTS, "too many open event streams")
        }
        // Unchanged behaviour: every stream of the connection is in use.
        Refusal::Overloaded(Class::Ordinary) => {
            error(StatusCode::BAD_GATEWAY, "no stream capacity available")
        }
        Refusal::Io(e) => error(StatusCode::BAD_GATEWAY, &e.to_string()),
    }
}

async fn tunnel(proxy: &Proxy, req: Request, class: Class) -> Response {
    match exchange(proxy, req, class).await {
        Ok(response) => response,
        Err(e) => refusal(e),
    }
}

async fn exchange(proxy: &Proxy, req: Request, class: Class) -> Result<Response, Refusal> {
    // `Overloaded` names the pool that was full; an exhausted total is reported
    // as ordinary whatever was asked for.
    let mut held = proxy
        .budget
        .acquire(class)
        .map_err(|o| Refusal::Overloaded(o.0))?;
    let mut connection_rx = proxy.conn.clone();
    let conn = tokio::time::timeout(
        CONNECT_TIMEOUT,
        connection_rx.wait_for(|c| c.as_ref().is_some_and(|c| c.close_reason().is_none())),
    )
    .await
    .map_err(io::Error::other)?
    .map_err(io::Error::other)?
    .clone()
    .ok_or_else(|| io::Error::other("remote daemon unreachable"))?;
    // The deadline covers dial, request upload and response *headers* only. It
    // ends before the body is returned, so a legitimately endless body survives.
    let (response, driver) = tokio::time::timeout(proxy.tuning.response_timeout, async {
        let (send, recv) = tokio::time::timeout(CONNECT_TIMEOUT, conn.open_bi())
            .await
            .map_err(io::Error::other)?
            .map_err(io::Error::other)?;
        let io = TokioIo::new(tokio::io::join(recv, send));
        let (mut sender, connection) = http1::handshake(io).await.map_err(io::Error::other)?;
        let driver = AbortTask(tokio::spawn(async move {
            let _ = connection.await;
        }));
        let response = sender.send_request(req).await.map_err(io::Error::other)?;
        Ok::<_, io::Error>((response, driver))
    })
    .await
    .map_err(io::Error::other)??;
    let (mut parts, body) = response.into_parts();
    strip_hop_headers(&mut parts.headers);
    // The response, not only the request, decides what the body costs.
    let actual = budget::response_class(&parts.headers);
    if actual > held.class {
        proxy
            .budget
            .upgrade(&mut held, actual)
            .map_err(|o| Refusal::Overloaded(o.0))?;
    }
    let stall = (held.class == Class::View).then_some(proxy.tuning.view_stall);
    // Driver and permits belong to the body. Dropping the body (client cancel,
    // stall, proxy stop) aborts the driver and resets the QUIC stream at once.
    let lease = Lease::new(proxy.cancel.child_token(), (held, driver));
    let body = GuardedBody::new(Body::new(body), lease, stall);
    Ok(Response::from_parts(parts, Body::new(body)))
}
