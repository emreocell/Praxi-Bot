//! The systemd watchdog of the daemon: a heartbeat that only goes out while the daemon answers.
//!
//! The beat is not "the runtime still ticks": it asks the daemon's own `GET /v1/health` over the
//! loopback, so a daemon whose HTTP stack, runtime or state lock is stuck stops beating and
//! systemd (`WatchdogSec=`) restarts it. Outside systemd it does nothing.

use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;

/// How long one probe may take before the beat is skipped.
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

/// Start the heartbeat when systemd asked for one (`WatchdogSec=`) and tell it the daemon is
/// serving. Returns at once; the beat stops with `cancel`.
pub(crate) fn start(addr: SocketAddr, cancel: CancellationToken) {
    if let Err(e) = liveness::notify::send("READY=1\nSTATUS=serving") {
        support_logs::tracing::warn!(target: "praxi_support", reason = "component_warning", "systemd notify failed: {e}");
    }
    let Some(interval) = liveness::notify::watchdog_interval() else {
        return;
    };
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    support_logs::tracing::info!(target: "praxi_support", phase = "watchdog_started", interval_ms = u64::try_from(interval.as_millis()).unwrap_or(u64::MAX));
    runtime.spawn(liveness::notify::heartbeat(interval, cancel, move || {
        probe_health(addr, PROBE_TIMEOUT)
    }));
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
    async fn nobody_listening_is_not_alive() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        assert!(!probe_health(addr, Duration::from_secs(2)).await);
    }
}
