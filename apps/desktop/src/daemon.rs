//! Daemon lifecycle for the desktop app (task 017).
//!
//! The app talks to `bot serve` over loopback HTTP.  [`connect`] probes the
//! configured port; if no daemon answers it starts `bot serve` as a child
//! process (a "sidecar": the `bot` binary next to the app), waits until it is
//! healthy and reads the per-install bearer token from `<data dir>/api-token`.
//! A daemon the app started is stopped again when the app exits; one that was
//! already running is never touched.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use parking_lot::Mutex;
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Default TCP port of `bot serve` (mirrors `daemon_api::DEFAULT_PORT`).
pub const DEFAULT_PORT: u16 = 47821;
/// Token file name inside the data directory (mirrors `daemon_api::TOKEN_FILE_NAME`).
const TOKEN_FILE_NAME: &str = "api-token";
/// How long a freshly started daemon may take to answer its health probe.
const START_TIMEOUT: Duration = Duration::from_secs(20);
const PROBE_TIMEOUT: Duration = Duration::from_millis(800);

/// What the web UI needs to talk to the daemon.
#[derive(Debug, Clone, Serialize)]
pub struct Connection {
    /// `http://127.0.0.1:PORT`.
    pub base_url: String,
    /// Bearer token (`api-token` file of the data directory).
    pub token: String,
    /// `true` when this app started the daemon (and will stop it on exit).
    pub started_by_app: bool,
    /// Whether the daemon was started with the fake (echo) provider.
    pub fake_provider: bool,
    /// Data directory in use.
    pub data_dir: String,
}

/// Owns the daemon child process, if the app started one.
#[derive(Default)]
pub struct DaemonHandle {
    /// Serialises [`connect`] so two concurrent calls (React strict mode, a
    /// retry) never start two daemons.
    connecting: tokio::sync::Mutex<()>,
    child: Mutex<Option<Child>>,
    fake: Mutex<bool>,
}

impl DaemonHandle {
    /// Stop the daemon if (and only if) this app started it.
    pub fn stop(&self) {
        if let Some(mut child) = self.child.lock().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn started_by_app(&self) -> bool {
        let mut guard = self.child.lock();
        match guard.as_mut() {
            // `try_wait` is `Ok(None)` while the process is still running.
            Some(c) => matches!(c.try_wait(), Ok(None)),
            None => false,
        }
    }
}

/// Port from `BOT_PORT`, else the default.
fn port() -> u16 {
    std::env::var("BOT_PORT")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(DEFAULT_PORT)
}

fn base_url() -> String {
    match std::env::var("BOT_URL") {
        Ok(u) if !u.trim().is_empty() => u.trim().trim_end_matches('/').to_owned(),
        _ => format!("http://127.0.0.1:{}", port()),
    }
}

/// `BOT_DATA_DIR`, else the OS data directory (same resolver as the CLI).
fn data_dir() -> Result<PathBuf, String> {
    storage::data_dir().ok_or_else(|| "cannot determine the data directory".to_owned())
}

/// Whether the environment asks for the deterministic fake provider
/// (manual runs and demos: `PRAXIBOT_FAKE_PROVIDER=1`).
fn wants_fake() -> bool {
    matches!(
        std::env::var("PRAXIBOT_FAKE_PROVIDER").as_deref(),
        Ok("1" | "true")
    )
}

/// `host:port` of a `http://host:port` URL.
fn host_port(url: &str) -> Option<String> {
    let rest = url.strip_prefix("http://")?;
    Some(rest.split('/').next()?.to_owned())
}

/// Plain HTTP/1.1 `GET /v1/health` over loopback (no HTTP client needed).
async fn healthy(url: &str) -> bool {
    let Some(hp) = host_port(url) else {
        return false;
    };
    let probe = async {
        let mut s = TcpStream::connect(&hp).await.ok()?;
        let req = format!("GET /v1/health HTTP/1.1\r\nHost: {hp}\r\nConnection: close\r\n\r\n");
        s.write_all(req.as_bytes()).await.ok()?;
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).await.ok()?;
        let text = String::from_utf8_lossy(&buf);
        (text.starts_with("HTTP/1.1 200") && text.contains("\"status\":\"ok\"")).then_some(())
    };
    matches!(
        tokio::time::timeout(PROBE_TIMEOUT, probe).await,
        Ok(Some(()))
    )
}

fn valid_token(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn read_token(data_dir: &Path) -> Result<String, String> {
    if let Ok(t) = std::env::var("BOT_API_TOKEN")
        && !t.trim().is_empty()
    {
        return Ok(t.trim().to_owned());
    }
    let path = data_dir.join(TOKEN_FILE_NAME);
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read the API token ({}): {e}", path.display()))?;
    let t = raw.trim();
    if valid_token(t) {
        Ok(t.to_owned())
    } else {
        Err(format!("{} is malformed", path.display()))
    }
}

/// Target triple embedded by `build.rs` for Tauri `externalBin` sidecar lookup.
pub const TARGET_TRIPLE: &str = env!("TAURI_TARGET_TRIPLE");

fn is_non_empty_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() > 0)
}

/// Candidate sidecar file names in priority order:
/// 1. `bot-<target-triple>[.exe]` (Tauri `externalBin` target-triple convention)
/// 2. `bot[.exe]` (installed NSIS sidecar / Cargo workspace binary next to the app)
#[must_use]
pub fn sidecar_candidate_names(triple: &str, windows: bool) -> [String; 2] {
    let ext = if windows { ".exe" } else { "" };
    [format!("bot-{triple}{ext}"), format!("bot{ext}")]
}

fn resolve_bot_binary(
    env_override: Option<OsString>,
    current_exe_dir: Option<&Path>,
    manifest_dir: Option<&Path>,
) -> OsString {
    if let Some(p) = env_override.filter(|p| !p.is_empty()) {
        return p;
    }
    let names = sidecar_candidate_names(TARGET_TRIPLE, cfg!(windows));
    if let Some(dir) = current_exe_dir {
        for name in &names {
            let direct = dir.join(name);
            if is_non_empty_file(&direct) {
                return direct.into_os_string();
            }
            let nested = dir.join("binaries").join(name);
            if is_non_empty_file(&nested) {
                return nested.into_os_string();
            }
        }
    }
    if let Some(manifest) = manifest_dir {
        let staged = manifest.join("binaries").join(&names[0]);
        if is_non_empty_file(&staged) {
            return staged.into_os_string();
        }
    }
    OsString::from("bot")
}

/// Where the `bot` binary lives: `PRAXIBOT_BOT_BIN`, the bundled Tauri sidecar
/// (`bot-<target-triple>[.exe]` or `bot[.exe]` next to the executable), a
/// staged sidecar in `apps/desktop/binaries/`, or `bot` on `PATH`.
#[must_use]
pub fn bot_binary() -> OsString {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    resolve_bot_binary(
        std::env::var_os("PRAXIBOT_BOT_BIN"),
        exe_dir.as_deref(),
        Some(Path::new(env!("CARGO_MANIFEST_DIR"))),
    )
}

/// A `bot` command with the console window suppressed on Windows.
#[must_use]
pub fn bot_command() -> Command {
    let mut cmd = Command::new(bot_binary());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

fn spawn_daemon(data_dir: &Path, fake: bool) -> Result<Child, String> {
    let mut cmd = bot_command();
    cmd.arg("serve").arg("--port").arg(port().to_string());
    if fake {
        cmd.arg("--fake-provider");
    }
    cmd.env("BOT_DATA_DIR", data_dir);
    // The dev server (`tauri dev`) has its own browser origin.
    if cfg!(debug_assertions) {
        cmd.env("BOT_ALLOWED_ORIGINS", "http://localhost:1420");
    }
    let _ = std::fs::create_dir_all(data_dir);
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(data_dir.join("desktop-daemon.log"))
        .map_or_else(|_| Stdio::null(), Stdio::from);
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(log);
    cmd.spawn().map_err(|e| {
        format!(
            "cannot start `{}` ({e}); set PRAXIBOT_BOT_BIN or put `bot` next to the app",
            bot_binary().to_string_lossy()
        )
    })
}

/// Make sure a daemon is reachable (starting one when needed) and return the
/// connection details.
///
/// # Errors
/// The daemon cannot be started, does not become healthy in time, or its token
/// cannot be read.
pub async fn connect(handle: &DaemonHandle) -> Result<Connection, String> {
    let _one_at_a_time = handle.connecting.lock().await;
    let url = base_url();
    let data = data_dir()?;
    if !healthy(&url).await {
        // A previously started child that died is dropped first.
        if !handle.started_by_app() {
            handle.stop();
            if std::env::var_os("BOT_URL").is_some() {
                return Err(format!("no daemon answers at {url}"));
            }
            let mut fake = wants_fake();
            *handle.child.lock() = Some(spawn_daemon(&data, fake)?);
            *handle.fake.lock() = fake;
            let deadline = tokio::time::Instant::now() + START_TIMEOUT;
            while !healthy(&url).await {
                if tokio::time::Instant::now() >= deadline {
                    return Err(format!(
                        "the daemon did not become healthy at {url} (see {})",
                        data.join("desktop-daemon.log").display()
                    ));
                }
                // A child that already exited will never answer.
                let exited = handle
                    .child
                    .lock()
                    .as_mut()
                    .is_some_and(|c| matches!(c.try_wait(), Ok(Some(_))));
                if exited {
                    // On first run before any provider is configured in `providers.toml`,
                    // `bot serve` exits immediately. Retry once with `--fake-provider` if
                    // we haven't already so the UI can still connect during onboarding.
                    if !fake {
                        fake = true;
                        if let Ok(child) = spawn_daemon(&data, true) {
                            *handle.child.lock() = Some(child);
                            *handle.fake.lock() = true;
                            tokio::time::sleep(Duration::from_millis(250)).await;
                            continue;
                        }
                    }
                    return Err(format!(
                        "the daemon exited right after start (see {})",
                        data.join("desktop-daemon.log").display()
                    ));
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            let token = read_token(&data)?;
            return Ok(Connection {
                base_url: url,
                token,
                started_by_app: handle.started_by_app(),
                fake_provider: *handle.fake.lock(),
                data_dir: data.display().to_string(),
            });
        }
        let deadline = tokio::time::Instant::now() + START_TIMEOUT;
        while !healthy(&url).await {
            if tokio::time::Instant::now() >= deadline {
                return Err(format!(
                    "the daemon did not become healthy at {url} (see {})",
                    data.join("desktop-daemon.log").display()
                ));
            }
            // A child that already exited will never answer.
            if handle
                .child
                .lock()
                .as_mut()
                .is_some_and(|c| matches!(c.try_wait(), Ok(Some(_))))
            {
                return Err(format!(
                    "the daemon exited right after start (see {})",
                    data.join("desktop-daemon.log").display()
                ));
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
    let token = read_token(&data)?;
    Ok(Connection {
        base_url: url,
        token,
        started_by_app: handle.started_by_app(),
        fake_provider: *handle.fake.lock(),
        data_dir: data.display().to_string(),
    })
}

/// Output of a `bot` CLI invocation.
#[derive(Debug, Serialize)]
pub struct CliOutput {
    /// Process exit code (`-1` if killed).
    pub code: i32,
    /// Standard output.
    pub stdout: String,
    /// Standard error.
    pub stderr: String,
}

/// Run `bot providers <args>` (OS-keychain backed provider management lives in
/// the CLI; the daemon API has no provider endpoints yet).  `api_key`, if any,
/// is handed over through an environment variable of the child only.
///
/// # Errors
/// The binary cannot be run.
pub async fn run_providers_cli(
    args: Vec<String>,
    api_key: Option<String>,
) -> Result<CliOutput, String> {
    let data = data_dir()?;
    let mut cmd = bot_command();
    cmd.arg("--data-dir")
        .arg(&data)
        .arg("providers")
        .args(&args);
    cmd.env("BOT_DATA_DIR", &data);
    if let Some(key) = api_key.filter(|k| !k.is_empty()) {
        cmd.env(KEY_ENV, key);
    }
    cmd.stdin(Stdio::null());
    let out = tokio::task::spawn_blocking(move || cmd.output())
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("cannot run `bot` ({e})"))?;
    Ok(CliOutput {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

/// Environment variable that carries an API key to `bot providers add`.
pub const KEY_ENV: &str = "PRAXIBOT_UI_API_KEY";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_port_parses_loopback_urls() {
        assert_eq!(
            host_port("http://127.0.0.1:47821").as_deref(),
            Some("127.0.0.1:47821")
        );
        assert_eq!(
            host_port("http://127.0.0.1:1/x").as_deref(),
            Some("127.0.0.1:1")
        );
        assert_eq!(host_port("https://x"), None);
    }

    #[test]
    fn token_must_be_64_hex() {
        assert!(valid_token(&"a".repeat(64)));
        assert!(!valid_token("short"));
        assert!(!valid_token(&"g".repeat(64)));
    }

    #[tokio::test]
    async fn health_probe_is_false_when_nothing_listens() {
        assert!(!healthy("http://127.0.0.1:1").await);
    }

    #[tokio::test]
    async fn health_probe_accepts_an_ok_daemon() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut b = [0u8; 512];
            let _ = s.read(&mut b).await;
            let body = r#"{"status":"ok","version":"0","uptime_secs":1}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            s.write_all(resp.as_bytes()).await.unwrap();
        });
        assert!(healthy(&format!("http://{addr}")).await);
    }

    #[test]
    fn sidecar_candidate_names_include_target_triple_first() {
        let win = sidecar_candidate_names("x86_64-pc-windows-msvc", true);
        assert_eq!(win[0], "bot-x86_64-pc-windows-msvc.exe");
        assert_eq!(win[1], "bot.exe");

        let unix = sidecar_candidate_names("aarch64-apple-darwin", false);
        assert_eq!(unix[0], "bot-aarch64-apple-darwin");
        assert_eq!(unix[1], "bot");
    }

    #[test]
    fn resolve_bot_binary_prefers_sidecar_and_ignores_empty_stubs() {
        let unique = format!(
            "praxibot-sidecar-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let dir = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(dir.join("binaries")).unwrap();

        let names = sidecar_candidate_names(TARGET_TRIPLE, cfg!(windows));
        let triple_bin = dir.join(&names[0]);
        let plain_bin = dir.join(&names[1]);

        // Empty stub (0 bytes) must be ignored in favour of a non-empty binary.
        std::fs::write(&triple_bin, b"").unwrap();
        std::fs::write(&plain_bin, b"real-binary").unwrap();
        assert_eq!(
            resolve_bot_binary(None, Some(&dir), None),
            plain_bin.clone().into_os_string()
        );

        // Once the target-triple sidecar is non-empty, it takes priority over plain `bot.exe`.
        std::fs::write(&triple_bin, b"sidecar-binary").unwrap();
        assert_eq!(
            resolve_bot_binary(None, Some(&dir), None),
            triple_bin.clone().into_os_string()
        );

        // Explicit PRAXIBOT_BOT_BIN overrides everything.
        assert_eq!(
            resolve_bot_binary(Some(OsString::from("/custom/bot")), Some(&dir), None),
            OsString::from("/custom/bot")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
