//! The systemd notify protocol (`sd_notify`) and a watchdog heartbeat.
//!
//! With `WatchdogSec=` in the unit, systemd kills and restarts a service that stops sending
//! `WATCHDOG=1`. Outside systemd (no `NOTIFY_SOCKET`, no `WATCHDOG_USEC`) every call here does
//! nothing, so the daemon runs the same way on Windows, macOS and a plain terminal.

use std::future::Future;
use std::io;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

/// Send `state` (`READY=1`, `WATCHDOG=1`, `STOPPING=1`, `STATUS=...`) to systemd, if it listens.
///
/// # Errors
/// The socket named by `NOTIFY_SOCKET` could not be written.
pub fn send(state: &str) -> io::Result<()> {
    match std::env::var("NOTIFY_SOCKET") {
        Ok(socket) if !socket.is_empty() => send_to(&socket, state),
        _ => Ok(()),
    }
}

/// Send `state` to the datagram socket `socket` (a path, or `@name` for an abstract socket).
#[cfg(unix)]
fn send_to(socket: &str, state: &str) -> io::Result<()> {
    use std::os::unix::net::UnixDatagram;
    let sock = UnixDatagram::unbound()?;
    if let Some(name) = socket.strip_prefix('@') {
        #[cfg(target_os = "linux")]
        {
            use std::os::linux::net::SocketAddrExt as _;
            let addr = std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes())?;
            sock.send_to_addr(state.as_bytes(), &addr)?;
            return Ok(());
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = name;
            return Ok(());
        }
    }
    sock.send_to(state.as_bytes(), socket)?;
    Ok(())
}

#[cfg(not(unix))]
fn send_to(_socket: &str, _state: &str) -> io::Result<()> {
    Ok(())
}

/// How often to send `WATCHDOG=1`: a third of the time systemd allows (`WATCHDOG_USEC`), so two
/// beats can be missed. `None` when no watchdog is set or it is meant for another process.
#[must_use]
pub fn watchdog_interval() -> Option<Duration> {
    interval_from(
        std::env::var("WATCHDOG_USEC").ok().as_deref(),
        std::env::var("WATCHDOG_PID").ok().as_deref(),
        std::process::id(),
    )
}

fn interval_from(usec: Option<&str>, pid: Option<&str>, own_pid: u32) -> Option<Duration> {
    let usec: u64 = usec?.trim().parse().ok().filter(|u| *u > 0)?;
    if let Some(pid) = pid
        && pid.trim().parse::<u32>().ok() != Some(own_pid)
    {
        return None;
    }
    Some(Duration::from_micros(usec / 3).max(Duration::from_millis(100)))
}

/// Send `WATCHDOG=1` every `interval` for as long as `alive` says the daemon is well. A beat is
/// skipped (not stopped) when it says no, so the daemon recovering in time keeps its process.
/// Returns when `cancel` fires.
pub async fn heartbeat<F, Fut>(interval: Duration, cancel: CancellationToken, alive: F)
where
    F: Fn() -> Fut,
    Fut: Future<Output = bool>,
{
    let mut tick = tokio::time::interval(interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            () = cancel.cancelled() => return,
            _ = tick.tick() => {}
        }
        if alive().await
            && let Err(e) = send("WATCHDOG=1")
        {
            tracing::warn!("systemd watchdog: could not send the heartbeat: {e}");
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::net::UnixDatagram;

    use super::*;

    #[test]
    fn the_interval_is_a_third_of_the_watchdog_time() {
        assert_eq!(
            interval_from(Some("90000000"), None, 7),
            Some(Duration::from_secs(30))
        );
        assert_eq!(interval_from(None, None, 7), None);
        assert_eq!(interval_from(Some("0"), None, 7), None);
        assert_eq!(interval_from(Some("soon"), None, 7), None);
    }

    #[test]
    fn a_watchdog_meant_for_another_process_is_ignored() {
        assert!(interval_from(Some("3000000"), Some("7"), 7).is_some());
        assert_eq!(interval_from(Some("3000000"), Some("8"), 7), None);
    }

    #[test]
    fn a_message_reaches_the_notify_socket() {
        let dir = std::env::temp_dir().join(format!("liveness-notify-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("notify.sock");
        let server = UnixDatagram::bind(&path).unwrap();
        send_to(path.to_str().unwrap(), "READY=1").unwrap();
        let mut buf = [0u8; 64];
        let n = server.recv(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"READY=1");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn an_abstract_socket_name_is_understood() {
        use std::os::linux::net::SocketAddrExt as _;
        let name = format!("liveness-test-{}", std::process::id());
        let addr = std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes()).unwrap();
        let server = UnixDatagram::bind_addr(&addr).unwrap();
        send_to(&format!("@{name}"), "WATCHDOG=1").unwrap();
        let mut buf = [0u8; 64];
        let n = server.recv(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"WATCHDOG=1");
    }

    #[tokio::test]
    async fn a_beat_is_skipped_when_the_daemon_is_not_well() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let checks = std::sync::Arc::new(AtomicU32::new(0));
        let cancel = CancellationToken::new();
        let counted = std::sync::Arc::clone(&checks);
        let stop = cancel.clone();
        let task = tokio::spawn(heartbeat(Duration::from_millis(5), cancel, move || {
            let counted = std::sync::Arc::clone(&counted);
            let stop = stop.clone();
            async move {
                if counted.fetch_add(1, Ordering::SeqCst) >= 2 {
                    stop.cancel();
                }
                false
            }
        }));
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap();
        assert!(checks.load(Ordering::SeqCst) >= 3);
    }
}
