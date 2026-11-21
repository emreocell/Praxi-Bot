//! First-run smoke test: the REAL `bot serve` on an EMPTY data directory (no providers, no
//! peers, no network needed) on a free loopback port.
//!
//! Guards the class of bug where the desktop app told a brand-new user "the service is running
//! but not responding": the daemon answers `GET /v1/health` with `setup_required` when no
//! provider is configured, and that has to count as "service is up".  The readiness rule is the
//! shared fixture `crates/daemon-api/tests/fixtures/health-contract.json` (also read by the
//! desktop Rust tests and the UI tests).  Never uses port 47821 or a real data directory.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Cold start of a debug build on a slow CI runner can take a while; polling returns as soon as
/// the daemon answers, so a generous deadline costs nothing.
const START_DEADLINE: Duration = Duration::from_secs(90);
const STOP_DEADLINE: Duration = Duration::from_secs(30);

/// `(http status, body)` of a plain HTTP/1.1 GET over loopback.
fn get(addr: SocketAddr, path: &str, bearer: Option<&str>) -> std::io::Result<(u16, String)> {
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let auth = bearer.map_or(String::new(), |t| format!("Authorization: Bearer {t}\r\n"));
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {addr}\r\n{auth}Connection: close\r\n\r\n"
    )?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw)?;
    let text = String::from_utf8_lossy(&raw).into_owned();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| std::io::Error::other(format!("no HTTP status in {text:?}")))?;
    let body = text
        .split_once("\r\n\r\n")
        .map_or("", |(_, b)| b)
        .to_owned();
    Ok((status, body))
}

/// The daemon process; killed on drop so a failed assertion never leaks it.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait_for_exit(child: &mut Child) -> std::process::ExitStatus {
    let deadline = Instant::now() + STOP_DEADLINE;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "daemon did not stop within {STOP_DEADLINE:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Start the daemon on an empty data dir; returns it, its address, the deadline used and the
/// thread that collects its stdout.
fn start_daemon(
    data: &std::path::Path,
) -> (Daemon, SocketAddr, Instant, std::thread::JoinHandle<String>) {
    start_daemon_with(data, &[])
}

fn start_daemon_with(
    data: &std::path::Path,
    extra: &[&str],
) -> (Daemon, SocketAddr, Instant, std::thread::JoinHandle<String>) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_bot"));
    command
        .arg("--data-dir")
        .arg(data)
        .args(["--secrets-backend", "file", "--port", "0", "serve"])
        .args(extra)
        .env("BOT_SECRETS_PASSPHRASE", "first-run-smoke-test-only")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for name in [
        "BOT_API_TOKEN",
        "BOT_CATALOG_DIR",
        "BOT_DATA_DIR",
        "BOT_PORT",
        "BOT_URL",
        "BOT_SECRETS_BACKEND",
        "BOT_SAFE_MODE",
    ] {
        command.env_remove(name);
    }
    let mut daemon = Daemon(command.spawn().expect("start bot serve"));

    // The daemon announces its (free) port on stdout; keep draining so it never blocks.
    let stdout = daemon.0.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel::<String>();
    let reader = std::thread::spawn(move || {
        let mut all = String::new();
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            all.push_str(&line);
            all.push('\n');
            let _ = tx.send(line);
        }
        all
    });
    let deadline = Instant::now() + START_DEADLINE;
    let addr: SocketAddr = loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let line = rx
            .recv_timeout(left)
            .unwrap_or_else(|e| panic!("daemon never announced its address: {e}"));
        if let Some(url) = line.strip_prefix("listening on http://") {
            break url.trim().parse().expect("listening address");
        }
    };
    assert_ne!(
        addr.port(),
        47821,
        "the smoke test must not use the real port"
    );
    assert!(addr.ip().is_loopback());

    (daemon, addr, deadline, reader)
}

/// Stop the daemon and check it ends (and releases the port).
#[cfg_attr(not(unix), allow(clippy::needless_pass_by_value))]
fn stop_cleanly(mut daemon: Daemon, addr: SocketAddr, reader: std::thread::JoinHandle<String>) {
    #[cfg(unix)]
    {
        // The port check below is Windows-only; SIGTERM + the stop line prove it on unix.
        let _ = &addr;
        let pid = daemon.0.id().to_string();
        let killed = Command::new("kill").args(["-TERM", &pid]).status().unwrap();
        assert!(killed.success());
        let exit = wait_for_exit(&mut daemon.0);
        assert!(exit.success(), "daemon exited with {exit:?} after SIGTERM");
        let output = reader.join().unwrap();
        assert!(
            output.contains("stopped;"),
            "no clean-stop line in: {output}"
        );
    }
    #[cfg(not(unix))]
    {
        // Windows delivers stop requests as console events, which cannot be sent to a child
        // from safe Rust (`unsafe_code` is denied). Stop it the way the desktop app does and
        // check that the process ends and releases the port.
        let _ = &reader;
        daemon.0.kill().unwrap();
        wait_for_exit(&mut daemon.0);
        let deadline = Instant::now() + STOP_DEADLINE;
        while TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_ok() {
            assert!(
                Instant::now() < deadline,
                "port {addr} still answers after stop"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

#[test]
fn first_run_daemon_reports_setup_required_as_up_and_stops_cleanly() {
    let data = tempfile::tempdir().unwrap();
    let (daemon, addr, deadline, reader) = start_daemon(data.path());

    // Poll (no fixed sleeps) until /v1/health answers.
    let (status, body) = loop {
        match get(addr, "/v1/health", None) {
            Ok(reply) => break reply,
            Err(e) => {
                assert!(Instant::now() < deadline, "health never answered: {e}");
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    };
    assert_eq!(status, 200, "{body}");
    let health: serde_json::Value = serde_json::from_str(&body).unwrap();

    // Shared contract: an empty data dir is a healthy first run, reported as setup_required.
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../crates/daemon-api/tests/fixtures/health-contract.json"
    ))
    .unwrap();
    let accepted: Vec<&str> = fixture["accepted_status"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    let reported = health["status"].as_str().unwrap();
    assert!(
        accepted.contains(&reported),
        "status {reported} is not accepted"
    );
    assert_eq!(
        reported, "setup_required",
        "no provider is configured: {body}"
    );
    assert!(health["version"].is_string() && health["api_version"].is_string());
    assert!(
        matches!(health["startup"].as_str(), Some("starting" | "ready")),
        "a first run must not start degraded: {body}"
    );

    // A protected route refuses a request without a token, and a wrong one.
    let (status, _) = get(addr, "/v1/bots", None).unwrap();
    assert_eq!(status, 401, "protected route without a token");
    let (status, _) = get(addr, "/v1/bots", Some(&"0".repeat(64))).unwrap();
    assert_eq!(status, 401, "protected route with a wrong token");

    stop_cleanly(daemon, addr, reader);
}

/// Safe mode on the real binary: the daemon says so, and the configuration the owner had
/// (MCP servers, schedule limits) is left byte for byte as it was.
#[test]
fn safe_mode_daemon_reports_it_and_leaves_the_configuration_untouched() {
    let data = tempfile::tempdir().unwrap();
    let kept = [
        (
            "mcp.toml",
            "# owner's servers, keep me exactly
",
        ),
        (
            "schedule.toml",
            "max_per_bot = 3
",
        ),
    ];
    for (name, text) in kept {
        std::fs::write(data.path().join(name), text).unwrap();
    }
    let (daemon, addr, deadline, reader) = start_daemon_with(data.path(), &["--safe"]);
    let body = loop {
        match get(addr, "/v1/health", None) {
            Ok((200, body)) => break body,
            other => {
                assert!(
                    Instant::now() < deadline,
                    "health never answered: {other:?}"
                );
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    };
    let health: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(health["safe_mode"], true, "{body}");
    assert_eq!(health["channels_active"], false, "{body}");
    stop_cleanly(daemon, addr, reader);
    for (name, text) in kept {
        assert_eq!(
            std::fs::read_to_string(data.path().join(name)).unwrap(),
            text,
            "{name} must not change in safe mode"
        );
    }
}

#[test]
fn safe_mode_cannot_be_combined_with_peer_networking() {
    let data = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_bot"))
        .arg("--data-dir")
        .arg(data.path())
        .args([
            "--secrets-backend",
            "file",
            "--port",
            "0",
            "serve",
            "--safe",
            "--peer",
        ])
        .env("BOT_SECRETS_PASSPHRASE", "first-run-smoke-test-only")
        .env_remove("BOT_SAFE_MODE")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("safe mode cannot be combined"));
}
