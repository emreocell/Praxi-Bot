//! The daemon's side of a `Type=notify` systemd unit: readiness and a watchdog heartbeat that only
//! go out while the daemon answers.
//!
//! Neither is "the process runs" or "the runtime still ticks": both ask the daemon's own
//! `GET /v1/health` over the loopback. `READY=1` waits for the first answer (so a start is "up"
//! when the HTTP server serves, however long the migrations before it took), and a daemon whose
//! HTTP stack, runtime or state lock gets stuck stops beating, so systemd (`WatchdogSec=`)
//! restarts it. Outside systemd it does nothing.

use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;

/// How long one probe may take before it counts as failed.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Whether the daemon answers `GET /v1/health` on `addr` with a 200 within `limit`.
pub(crate) async fn probe_health(addr: SocketAddr, limit: Duration) -> bool {
    let probe = async {
        let mut stream = TcpStream::connect(addr).await.ok()?;
        let request = format!(
            "GET /v1/health HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n",
            addr.port()
        );
        stream.write_all(request.as_bytes()).await.ok()?;
        let mut head = [0u8; 12];
        stream.read_exact(&mut head).await.ok()?;
        Some(head.starts_with(b"HTTP/1.1 200"))
    };
    tokio::time::timeout(limit, probe)
        .await
        .ok()
        .flatten()
        .unwrap_or(false)
}

/// Start the systemd side of the daemon: `READY=1` once its own `GET /v1/health` answers and, when
/// the unit has `WatchdogSec=`, a heartbeat that goes on only while it keeps answering. Returns at
/// once; the task ends with `cancel`. Without a `NOTIFY_SOCKET` (not started by systemd) nothing
/// is spawned.
pub(crate) fn start(addr: SocketAddr, cancel: CancellationToken) {
    if !liveness::notify::is_enabled() {
        return;
    }
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    runtime.spawn(liveness::notify::supervise_service(
        liveness::notify::watchdog_interval(),
        cancel,
        move || probe_health(addr, PROBE_TIMEOUT),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn canned(reply: &'static [u8], answer: bool) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(async move {
                    let mut buf = [0u8; 256];
                    let _ = s.read(&mut buf).await;
                    if answer {
                        let _ = s.write_all(reply).await;
                    } else {
                        // A hung daemon: the connection is open and nothing comes back.
                        tokio::time::sleep(Duration::from_secs(30)).await;
                    }
                });
            }
        });
        addr
    }

    #[tokio::test]
    async fn a_daemon_that_answers_200_is_alive() {
        let addr = canned(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}", true).await;
        assert!(probe_health(addr, Duration::from_secs(2)).await);
    }

    #[tokio::test]
    async fn an_error_reply_is_not_alive() {
        let addr = canned(b"HTTP/1.1 500 Internal Server Error\r\n\r\n", true).await;
        assert!(!probe_health(addr, Duration::from_secs(2)).await);
    }

    #[tokio::test]
    async fn a_daemon_that_never_answers_is_not_alive() {
        let addr = canned(b"", false).await;
        assert!(!probe_health(addr, Duration::from_millis(200)).await);
    }

    #[tokio::test]
    async fn a_port_that_is_bound_but_not_served_yet_is_not_ready() {
        // The daemon binds the port first and starts serving later: the kernel completes the
        // connection, and the request sits unanswered. READY=1 must wait for the answer.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        assert!(!probe_health(addr, Duration::from_millis(200)).await);
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(async move {
                    let mut buf = [0u8; 256];
                    let _ = s.read(&mut buf).await;
                    let _ = s
                        .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}")
                        .await;
                });
            }
        });
        assert!(probe_health(addr, Duration::from_secs(2)).await);
    }

    #[tokio::test]
    async fn nobody_listening_is_not_alive() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        assert!(!probe_health(addr, Duration::from_secs(2)).await);
    }
}
