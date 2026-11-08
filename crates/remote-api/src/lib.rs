//! Streaming HTTP/1.1 over the paired-only `bot/api/1` ALPN.
mod proxy;
#[cfg(feature = "server")]
mod server;
pub use proxy::{ApiProxy, ProxyState};
#[cfg(feature = "server")]
pub use server::ApiProtocol;

use axum::body::Body;
use futures_util::stream;
use http_body_util::{BodyExt, StreamBody};
use std::{io, time::Duration};

/// Maximum streamed request size (responses, including SSE, have no size cap).
pub const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;
/// Request header, dial and stream-open deadline.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Maximum time to produce response headers; SSE bodies remain unbounded.
pub const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
/// Concurrent bidirectional streams one `bot/api/1` connection may carry.
pub const STREAMS: usize = 32;
/// Request-body bytes one API connection may have in flight.
///
/// [`STREAMS`] requests, each up to [`MAX_REQUEST_BYTES`].
pub const CONNECTION_BODY_BUDGET: u64 = (STREAMS as u64) * (MAX_REQUEST_BYTES as u64);

// Enforce the cap even with chunked encoding; preserve frames and backpressure.
pub(crate) fn request_body(body: Body) -> Body {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    Body::new(StreamBody::new(stream::unfold(
        (body, 0usize, false),
        move |(mut body, mut size, done)| async move {
            if done {
                return None;
            }
            let frame = tokio::time::timeout_at(
                deadline.min(tokio::time::Instant::now() + CONNECT_TIMEOUT),
                body.frame(),
            )
            .await;
            let result = match frame {
                Ok(None) => return None,
                Ok(Some(Ok(frame))) => {
                    size = size.saturating_add(frame.data_ref().map_or(0, axum::body::Bytes::len));
                    if size > MAX_REQUEST_BYTES {
                        Err(io::Error::other("request body too large"))
                    } else {
                        Ok(frame)
                    }
                }
                Ok(Some(Err(e))) => Err(io::Error::other(e)),
                Err(_) => Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "request body timeout",
                )),
            };
            let done = result.is_err();
            Some((result, (body, size, done)))
        },
    )))
}

pub(crate) fn oversized(headers: &axum::http::HeaderMap) -> bool {
    headers
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .is_some_and(|size| size > MAX_REQUEST_BYTES as u64)
}

pub(crate) struct AbortTask(pub tokio::task::JoinHandle<()>);
impl Drop for AbortTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[cfg(all(test, feature = "server"))]
mod tests;
