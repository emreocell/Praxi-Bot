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

/// Whether systemd listens for notifications (`NOTIFY_SOCKET` is set). Outside systemd it is not,
/// and everything in this module does nothing.
#[must_use]
pub fn is_enabled() -> bool {
    std::env::var("NOTIFY_SOCKET").is_ok_and(|s| !s.is_empty())
}

/// How often to send `WATCHDOG=1`: a quarter of the time systemd allows (`WATCHDOG_USEC`), so
/// three beats can be missed. `None` when no watchdog is set or it is meant for another process.
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
    Some(Duration::from_micros(usec / 4).max(Duration::from_millis(100)))
}

/// How long to wait before the next probe while the daemon is not answering: the next beat must
/// not wait a whole interval after a probe failed, or a few failures in a row eat the watchdog's
/// whole budget.
const RETRY_AFTER_FAILURE: Duration = Duration::from_secs(5);
/// How often the first probe is repeated while the daemon is still starting.
const STARTUP_POLL: Duration = Duration::from_millis(500);

/// Tell systemd the daemon is serving, then keep it from restarting the daemon while it is well.
///
/// * `READY=1` goes out only after `probe` succeeded once, so `Type=notify` (and whatever is
///   ordered after the unit) waits until the daemon answers, not until it is merely running.
///   While it does not, the probe is repeated; systemd's `TimeoutStartSec=` ends a start that
///   never gets there.
/// * With `watchdog` set (the beat interval, see [`watchdog_interval`]) `WATCHDOG=1` follows after
///   every successful probe. A failed probe is retried after a few seconds instead of a whole
///   interval, so a transient failure costs one beat at most and a daemon that stays stuck is
///   still killed by `WatchdogSec=`.
///
/// Does nothing at all, not even the probe, without a `NOTIFY_SOCKET` (outside systemd). Returns
/// when `cancel` fires.
pub async fn supervise_service<F, Fut>(
    watchdog: Option<Duration>,
    cancel: CancellationToken,
    probe: F,
) where
    F: Fn() -> Fut,
    Fut: Future<Output = bool>,
{
    if !is_enabled() {
        return;
    }
    run_service(watchdog, cancel, probe, send).await;
}

async fn run_service<F, Fut, S>(
    watchdog: Option<Duration>,
    cancel: CancellationToken,
    probe: F,
    send: S,
) where
    F: Fn() -> Fut,
    Fut: Future<Output = bool>,
    S: Fn(&str) -> io::Result<()>,
{
    // Nothing is written to the support log from here but fixed phases: an `io::Error` can carry
    // the path of the socket.
    let say = |state: &str| {
        let delivered = send(state).is_ok();
        if !delivered {
            support_logs::tracing::warn!(target: "praxi_support", reason = "component_warning", "systemd notify failed");
        }
        delivered
    };
    let mut wait = Duration::ZERO;
    // Until the daemon answers once there is nothing to report.
    loop {
        if sleep_or_cancelled(&cancel, wait).await {
            return;
        }
        // A READY=1 that was not delivered is tried again: systemd would otherwise time the
        // start out.
        if probe().await && say("READY=1\nSTATUS=serving") {
            support_logs::tracing::info!(target: "praxi_support", phase = "notify_ready");
            break;
        }
        wait = STARTUP_POLL;
    }
    let Some(interval) = watchdog else {
        return;
    };
    support_logs::tracing::info!(
        target: "praxi_support",
        phase = "watchdog_started",
        interval_ms = u64::try_from(interval.as_millis()).unwrap_or(u64::MAX)
    );
    let mut failures = 0u64;
    wait = Duration::ZERO;
    loop {
        if sleep_or_cancelled(&cancel, wait).await {
            return;
        }
        if probe().await {
            if failures > 0 {
                support_logs::tracing::info!(target: "praxi_support", phase = "watchdog_probe_recovered", failures);
                failures = 0;
            }
            let _ = say("WATCHDOG=1");
            wait = interval;
        } else {
            // Log the first failure of a streak only: a hung daemon fails for minutes.
            if failures == 0 {
                support_logs::tracing::warn!(target: "praxi_support", phase = "watchdog_probe_failed", failures = 1u64);
            }
            failures = failures.saturating_add(1);
            wait = RETRY_AFTER_FAILURE.min(interval);
        }
    }
}

/// Wait `wait`; `true` when `cancel` fired first.
async fn sleep_or_cancelled(cancel: &CancellationToken, wait: Duration) -> bool {
    tokio::select! {
        () = cancel.cancelled() => true,
        () = tokio::time::sleep(wait) => cancel.is_cancelled(),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::net::UnixDatagram;

    use super::*;

    #[test]
    fn the_interval_is_a_quarter_of_the_watchdog_time() {
        assert_eq!(
            interval_from(Some("180000000"), None, 7),
            Some(Duration::from_secs(45))
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

    type Sent = std::sync::Arc<parking_lot::Mutex<Vec<(Duration, String)>>>;

    /// Run the supervision on a paused clock against a scripted probe (`true` = answers), for
    /// `for_how_long`, and say what was sent when.
    async fn run_scripted(
        watchdog: Option<Duration>,
        answers: impl Fn(Duration) -> bool + Send + 'static,
        for_how_long: Duration,
    ) -> Vec<(Duration, String)> {
        let started = tokio::time::Instant::now();
        let sent: Sent = Sent::default();
        let log = std::sync::Arc::clone(&sent);
        let cancel = CancellationToken::new();
        let task = tokio::spawn({
            let cancel = cancel.clone();
            async move {
                run_service(
                    watchdog,
                    cancel,
                    move || {
                        let up = answers(started.elapsed());
                        async move { up }
                    },
                    move |state: &str| {
                        log.lock().push((started.elapsed(), state.to_owned()));
                        Ok(())
                    },
                )
                .await;
            }
        });
        tokio::time::sleep(for_how_long).await;
        cancel.cancel();
        task.await.unwrap();
        // (a statement of its own: the guard would otherwise live to the end of the block)
        #[allow(clippy::let_and_return)]
        let all = sent.lock().clone();
        all
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[tokio::test(start_paused = true)]
    async fn ready_is_sent_only_once_the_daemon_answers() {
        // The daemon starts answering after 40 s (a slow migration, say).
        let sent = run_scripted(None, |t| t >= secs(40), secs(60)).await;
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0].1, "READY=1\nSTATUS=serving");
        assert!(sent[0].0 >= secs(40) && sent[0].0 < secs(41), "{sent:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn nothing_is_sent_while_the_daemon_never_answers() {
        let sent = run_scripted(Some(secs(45)), |_| false, secs(600)).await;
        assert!(sent.is_empty(), "no READY=1, no WATCHDOG=1: {sent:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn the_watchdog_beats_after_ready_and_each_beat_is_a_successful_probe() {
        let sent = run_scripted(Some(secs(45)), |_| true, secs(100)).await;
        let states: Vec<&str> = sent.iter().map(|(_, s)| s.as_str()).collect();
        assert_eq!(
            states,
            vec![
                "READY=1\nSTATUS=serving",
                "WATCHDOG=1",
                "WATCHDOG=1",
                "WATCHDOG=1"
            ]
        );
        // One beat right after ready, then every interval.
        let spacing = sent[2].0.saturating_sub(sent[1].0);
        assert!(spacing >= secs(45) && spacing < secs(46), "{sent:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_few_failed_probes_do_not_starve_the_watchdog() {
        // WatchdogSec=180 (interval 45 s). The daemon answers, then does not for a minute (40 s to
        // 100 s): the probes at 45 s and every retry after it fail.
        let sent = run_scripted(
            Some(secs(45)),
            |t| !(secs(40)..secs(100)).contains(&t),
            secs(200),
        )
        .await;
        let beats: Vec<Duration> = sent
            .iter()
            .filter(|(_, s)| s == "WATCHDOG=1")
            .map(|(t, _)| *t)
            .collect();
        // The silence stays far inside the 180 s systemd allows, however many probes failed in it
        // (with a whole interval between probes, two failures alone would have used 135 s).
        let gaps: Vec<Duration> = beats
            .windows(2)
            .map(|w| w[1].saturating_sub(w[0]))
            .collect();
        assert!(gaps.iter().all(|g| *g < secs(120)), "{beats:?} {gaps:?}");
        // And it recovered: a beat follows the hiccup within the retry delay.
        assert!(
            beats.iter().any(|t| *t >= secs(100) && *t < secs(106)),
            "{beats:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_stuck_daemon_stops_the_beats() {
        let sent = run_scripted(Some(secs(45)), |t| t < secs(10), secs(500)).await;
        let last_beat = sent.iter().map(|(t, _)| *t).max().unwrap();
        assert!(last_beat < secs(10), "{sent:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn without_a_watchdog_only_ready_is_sent() {
        let sent = run_scripted(None, |_| true, secs(500)).await;
        assert_eq!(sent.len(), 1, "{sent:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_failing_socket_is_retried_until_ready_gets_through() {
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let counted = std::sync::Arc::clone(&attempts);
        let cancel = CancellationToken::new();
        let task = tokio::spawn({
            let cancel = cancel.clone();
            async move {
                run_service(
                    None,
                    cancel,
                    || async { true },
                    move |_: &str| {
                        if counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < 2 {
                            Err(io::Error::other("no socket"))
                        } else {
                            Ok(())
                        }
                    },
                )
                .await;
            }
        });
        tokio::time::timeout(secs(30), task).await.unwrap().unwrap();
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn without_a_notify_socket_nothing_runs_not_even_the_probe() {
        // The test binary is not started by systemd.
        assert!(!is_enabled(), "NOTIFY_SOCKET must not be set for the tests");
        let probes = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let counted = std::sync::Arc::clone(&probes);
        supervise_service(Some(secs(1)), CancellationToken::new(), move || {
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async { true }
        })
        .await;
        assert_eq!(probes.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
}
