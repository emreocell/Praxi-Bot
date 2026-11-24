//! Daemon lifecycle for the desktop app (task 017).
//!
//! The app talks to `bot serve` over loopback HTTP.  [`connect`] probes the
//! configured port; if no daemon answers it starts `bot serve` as a child
//! process (a "sidecar": the `bot` binary next to the app), waits until it is
//! healthy and reads the per-install bearer token from `<data dir>/api-token`.
//! A daemon the app started is stopped again when the app exits; one that was
//! already running is never touched.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::listener::{self, Expect};

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
    /// Whether this app starts its daemon in safe mode (see [`restart`]).
    pub safe_mode: bool,
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
    /// Safe mode for every daemon this app starts, until a restart says otherwise. Held in
    /// memory only: a fresh launch of the app always starts normally.
    safe: Mutex<bool>,
    /// Daemon log length when the current child was spawned.
    log_offset: Mutex<u64>,
    /// What the start-up screen shows when the daemon does not come up (see [`diagnostics`]).
    diag: Mutex<DiagState>,
    /// The last lines the current child wrote to its stderr.
    tail: Arc<Mutex<VecDeque<String>>>,
    /// Set by the stderr pump when the child's stderr reached end of file.
    pump_done: Mutex<Option<Arc<AtomicBool>>>,
    /// When this app last spawned the daemon (see [`restart_is_redundant`]).
    last_spawn: Mutex<Option<Instant>>,
}

/// How far the start of the daemon got; the UI words it (`humanStartup`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// Nothing started yet.
    #[default]
    Idle,
    /// The OS is still creating the process (antivirus scans happen here).
    Spawning,
    /// The process runs; its health probe does not answer yet.
    Waiting,
    /// Connected.
    Ready,
    /// The `bot` sidecar is not where the app looks for it.
    SidecarMissing,
    /// The OS refused to run the sidecar (antivirus, `SmartScreen`, policy).
    SidecarBlocked,
    /// Any other failure to start the process.
    SpawnFailed,
    /// The process ended before it answered; see `exit_code` and `log_tail`.
    Exited,
    /// The process runs but never answered within the start timeout.
    NotResponding,
    /// Another program holds the daemon's port.
    PortInUse,
    /// The daemon answers but the app refused to hand it the token.
    Unreachable,
}

#[derive(Debug, Default)]
struct DiagState {
    stage: Stage,
    since: Option<Instant>,
    error: Option<String>,
    exit_code: Option<i32>,
}

/// Everything the "the service does not start" screen shows and copies.
#[derive(Debug, Clone, Serialize)]
pub struct Diagnostics {
    /// How far the start got.
    pub stage: Stage,
    /// Milliseconds since the current attempt began.
    pub elapsed_ms: u64,
    /// Port the daemon is expected on.
    pub port: u16,
    /// `http://127.0.0.1:PORT`.
    pub base_url: String,
    /// Data directory in use.
    pub data_dir: String,
    /// The `desktop-daemon.log` the app writes the daemon's output to.
    pub log_path: String,
    /// The `bot` binary the app tried to run.
    pub sidecar: String,
    /// Whether that path is a file.
    pub sidecar_exists: bool,
    /// PID of the running child, if any.
    pub pid: Option<u32>,
    /// Exit code of a child that ended (Windows NTSTATUS codes are negative).
    pub exit_code: Option<i32>,
    /// The reason in English, when known.
    pub error: Option<String>,
    /// The daemon's last output lines.
    pub log_tail: Vec<String>,
    /// Version of this app.
    pub app_version: String,
    /// Operating system.
    pub os: String,
}

const TAIL_LINES: usize = 60;
const MAX_LOG_BYTES: u64 = 512 * 1024;
const LOG_FILE_NAME: &str = "desktop-daemon.log";

/// `YYYY-MM-DDTHH:MM:SSZ` for a Unix time (UTC; no date crate needed).
fn format_utc(secs: u64) -> String {
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let z = i64::try_from(days).unwrap_or(0) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn now_stamp() -> String {
    format_utc(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
    )
}

fn ensure_logging(data_dir: &Path) {
    static LOGGING: std::sync::Once = std::sync::Once::new();
    LOGGING.call_once(|| {
        if support_logs::init(data_dir, "sidecar").is_err() {
            eprintln!("Sidecar diagnostics could not open the log folder.");
        }
    });
}

/// One timestamped line appended to `desktop-daemon.log` (errors are ignored: a log must never stop the app).
fn append_log(data_dir: &Path, who: &str, line: &str) {
    use std::io::Write;
    ensure_logging(data_dir);
    // Never retain arbitrary child stderr, UI restart reasons or OS errors.
    let safe = support_logs::safe_stderr(line);
    if who == "app" {
        let phase = if line.starts_with("starting")
            || line.starts_with("started")
            || line.starts_with("daemon spawned")
        {
            "spawning"
        } else if line.starts_with("restart") {
            "stopping"
        } else {
            "failed"
        };
        support_logs::tracing::info!(target: "praxi_support", phase);
    }
    rotate_log(data_dir);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path(data_dir))
    {
        let _ = writeln!(f, "{} [{who}] {safe}", now_stamp());
    }
}

/// Keep the log small: past [`MAX_LOG_BYTES`] it becomes `desktop-daemon.log.1` (one generation).
fn rotate_log(data_dir: &Path) {
    let path = log_path(data_dir);
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_LOG_BYTES) {
        let old = data_dir.join(format!("{LOG_FILE_NAME}.1"));
        let _ = std::fs::remove_file(&old);
        let _ = std::fs::rename(&path, &old);
    }
}

/// Copy the child's stderr into the log (timestamped) and the in-memory tail.
fn pump_stderr(
    stderr: ChildStderr,
    data_dir: PathBuf,
    tail: Arc<Mutex<VecDeque<String>>>,
    done: Arc<AtomicBool>,
) {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut raw = Vec::new();
        loop {
            raw.clear();
            match reader.read_until(b'\n', &mut raw) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let text = String::from_utf8_lossy(&raw);
            let line = support_logs::safe_stderr(text.trim_end());
            if line.is_empty() {
                continue;
            }
            if line.contains(NO_PROVIDER_MARKER) || !line.starts_with('{') {
                append_log(&data_dir, "bot", &line);
            }
            let mut t = tail.lock();
            if t.len() >= TAIL_LINES {
                t.pop_front();
            }
            t.push_back(line);
        }
        done.store(true, Ordering::SeqCst);
    });
}

/// `bind` failures as the OS words them, for "another program holds the port".
fn looks_like_port_clash(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    [
        "address already in use",
        "only one usage of each socket address",
        "os error 10048",
        "os error 10013",
        "os error 98",
        "os error 48",
    ]
    .iter()
    .any(|m| t.contains(m))
}

/// What a failed `spawn` means for the person (see [`Stage`]).
fn classify_spawn_error(e: &std::io::Error, sidecar_exists: bool) -> Stage {
    match (e.kind(), e.raw_os_error()) {
        // ERROR_VIRUS_INFECTED / ERROR_ACCESS_DISABLED_BY_POLICY / ..._NO_SAFER_UI_BY_POLICY
        (_, Some(225 | 1260 | 1261)) | (std::io::ErrorKind::PermissionDenied, _) => {
            Stage::SidecarBlocked
        }
        (std::io::ErrorKind::NotFound, _) => Stage::SidecarMissing,
        _ if !sidecar_exists => Stage::SidecarMissing,
        _ => Stage::SpawnFailed,
    }
}

impl DaemonHandle {
    /// Stop the daemon if (and only if) this app started it.
    pub fn stop(&self) {
        if let Some(mut child) = self.child.lock().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn set_stage(&self, stage: Stage, error: Option<String>) {
        let mut d = self.diag.lock();
        if stage == Stage::Spawning {
            d.since = Some(Instant::now());
            d.exit_code = None;
        }
        d.stage = stage;
        d.error = error;
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

/// The daemon the UI may talk to. The webview CSP (`connect-src` in
/// `tauri.conf.json`) only allows loopback origins, so a `BOT_URL` that points
/// anywhere else would be blocked by the webview with no useful error. It is
/// refused here instead, with a message that says why.
fn base_url() -> Result<String, String> {
    match std::env::var("BOT_URL") {
        Ok(u) if !u.trim().is_empty() => validate_bot_url(u.trim()),
        _ => Ok(format!("http://127.0.0.1:{}", port())),
    }
}

/// Accepts `http://127.0.0.1:PORT`, `http://localhost:PORT` and `http://[::1]:PORT`
/// (optionally with a path, which is dropped); everything else is an error.
fn validate_bot_url(url: &str) -> Result<String, String> {
    let url = url.trim_end_matches('/');
    let loopback = host_port(url)
        .and_then(|hp| {
            hp.rsplit_once(':')
                .map(|(h, p)| (h.to_owned(), p.to_owned()))
        })
        .is_some_and(|(host, port)| {
            matches!(host.as_str(), "127.0.0.1" | "localhost" | "[::1]")
                && port.parse::<u16>().is_ok()
        });
    if loopback {
        Ok(url.to_owned())
    } else {
        Err(format!(
            "BOT_URL must be http://127.0.0.1:PORT, http://localhost:PORT or http://[::1]:PORT \
             (got {url}): the desktop app only connects to a daemon on this machine, \
             because its content security policy blocks every other origin"
        ))
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

/// What `GET /v1/health` says about the service.  The contract (shared with the daemon-api and
/// UI tests through `crates/daemon-api/tests/fixtures/health-contract.json`): the service is UP
/// iff the reply is HTTP 200 and `status` is `ok` or `setup_required`.  A fresh install has no
/// provider yet and the daemon then reports `setup_required`: that is a running daemon (the app
/// opens its setup screen), never a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HealthState {
    Unreachable,
    Starting,
    Degraded,
    SetupRequired,
    Ok,
}

impl HealthState {
    fn is_up(self) -> bool {
        self != Self::Unreachable
    }
}

/// Classify the raw HTTP reply to `GET /v1/health`.
fn health_state(text: &str) -> HealthState {
    if !text.starts_with("HTTP/1.1 200") {
        return HealthState::Unreachable;
    }
    let body = text.split_once("\r\n\r\n").map_or("", |(_, b)| b);
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body.trim()) else {
        return HealthState::Unreachable;
    };
    let setup = match v.get("status").and_then(serde_json::Value::as_str) {
        Some("ok") => false,
        Some("setup_required") => true,
        _ => return HealthState::Unreachable,
    };
    match v.get("startup").and_then(serde_json::Value::as_str) {
        Some("degraded") => HealthState::Degraded,
        Some("starting") => HealthState::Starting,
        _ if setup => HealthState::SetupRequired,
        _ => HealthState::Ok,
    }
}

fn health_reply_is_alive(text: &str) -> bool {
    health_state(text).is_up()
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
        health_reply_is_alive(&text).then_some(())
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
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut cmd = Command::new(bot_binary());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// The per-launch owner key: random, held only in memory here and in the
/// daemon it is handed to (on its stdin), shown to the main window only.
///
/// # Errors
/// The OS offers no randomness; nothing is cached, so a later call retries.
pub fn owner_key() -> Result<&'static str, String> {
    use std::fmt::Write as _;
    static KEY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    if let Some(key) = KEY.get() {
        return Ok(key);
    }
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| format!("no OS randomness for the owner key: {e}"))?;
    let key = bytes.iter().fold(String::new(), |mut hex, b| {
        let _ = write!(hex, "{b:02x}");
        hex
    });
    Ok(KEY.get_or_init(|| key))
}

fn spawn_daemon(handle: &DaemonHandle, data_dir: &Path, fake: bool) -> Result<Child, String> {
    let mut cmd = bot_command();
    cmd.arg("serve").arg("--port").arg(port().to_string());
    cmd.arg("--owner-key-stdin");
    if *handle.safe.lock() {
        cmd.arg("--safe");
    }
    // A stray `BOT_SAFE_MODE` in the app's own environment must not decide for the owner.
    cmd.env_remove("BOT_SAFE_MODE");
    if fake {
        cmd.arg("--fake-provider");
    }
    cmd.env("BOT_DATA_DIR", data_dir);
    // The dev server (`tauri dev`) has its own browser origin.
    if cfg!(debug_assertions) {
        cmd.env("BOT_ALLOWED_ORIGINS", "http://localhost:1420");
    }
    if let Err(e) = std::fs::create_dir_all(data_dir) {
        let msg = format!("cannot create the data folder {} ({e})", data_dir.display());
        handle.set_stage(Stage::SpawnFailed, Some(msg.clone()));
        return Err(msg);
    }
    let bin = bot_binary();
    let sidecar_exists = Path::new(&bin).is_file();
    append_log(
        data_dir,
        "app",
        &format!(
            "starting `{}` serve --port {} (sidecar file present: {sidecar_exists}, fake provider: {fake}, app {})",
            bin.to_string_lossy(),
            port(),
            env!("CARGO_PKG_VERSION"),
        ),
    );
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| {
        let msg = format!(
            "cannot start `{}` ({e}); set PRAXIBOT_BOT_BIN or put `bot` next to the app",
            bin.to_string_lossy()
        );
        append_log(data_dir, "app", &msg);
        handle.set_stage(classify_spawn_error(&e, sidecar_exists), Some(msg.clone()));
        msg
    })?;
    handle.tail.lock().clear();
    if let Some(stderr) = child.stderr.take() {
        let done = Arc::new(AtomicBool::new(false));
        *handle.pump_done.lock() = Some(Arc::clone(&done));
        pump_stderr(
            stderr,
            data_dir.to_path_buf(),
            Arc::clone(&handle.tail),
            done,
        );
    }
    append_log(data_dir, "app", &format!("started pid {}", child.id()));
    // One line on the child's stdin, then the pipe is closed: the key is in no
    // argument, file or environment variable.
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        let key = match owner_key() {
            Ok(key) => key,
            Err(e) => {
                let _ = child.kill();
                handle.set_stage(Stage::SpawnFailed, Some(e.clone()));
                return Err(e);
            }
        };
        // Written and flushed, then `stdin` drops at the end of this block and closes the pipe,
        // so the daemon's `read_line` returns at once.
        if let Err(e) = writeln!(stdin, "{key}").and_then(|()| stdin.flush()) {
            append_log(
                data_dir,
                "app",
                &format!("could not hand the owner key to the daemon: {e}"),
            );
        }
    }
    Ok(child)
}

/// Marker `bot serve` prints (to the daemon log) when it cannot build a single
/// provider: none configured, all disabled, or the key is missing from the
/// secret store.  These are onboarding states in which the app must still open
/// (with the fake provider) so the user can fix them in the UI.
const NO_PROVIDER_MARKER: &str = "no usable provider";

fn log_path(data_dir: &Path) -> PathBuf {
    data_dir.join(LOG_FILE_NAME)
}

fn log_len(data_dir: &Path) -> u64 {
    std::fs::metadata(log_path(data_dir)).map_or(0, |m| m.len())
}

/// Whether the daemon log, from byte `offset` on (what the just-exited child
/// wrote), reports the "no usable provider" start failure.  Any other early
/// exit (corrupt config, DB error, port clash) is a real failure and is not
/// masked by the fake fallback.
fn no_usable_provider_since(data_dir: &Path, offset: u64) -> bool {
    std::fs::read(log_path(data_dir)).is_ok_and(|b| {
        let from = usize::try_from(offset).unwrap_or(usize::MAX).min(b.len());
        String::from_utf8_lossy(&b[from..]).contains(NO_PROVIDER_MARKER)
    })
}

/// Spawn the daemon and remember where its log output will start.
fn spawn_tracked(handle: &DaemonHandle, data: &Path, fake: bool) -> Result<(), String> {
    handle.set_stage(Stage::Spawning, None);
    // The OS may take long to create the process (antivirus scan of a new, unsigned program).
    let _ = std::fs::create_dir_all(data);
    rotate_log(data);
    let offset = log_len(data);
    let child = spawn_daemon(handle, data, fake)?;
    handle.set_stage(Stage::Waiting, None);
    *handle.log_offset.lock() = offset;
    *handle.child.lock() = Some(child);
    *handle.fake.lock() = fake;
    *handle.last_spawn.lock() = Some(Instant::now());
    append_log(
        data,
        "app",
        &format!("daemon spawned (fake provider: {fake})"),
    );
    Ok(())
}

/// PID of the app-owned child while it runs.
fn child_pid(handle: &DaemonHandle) -> Option<u32> {
    let mut g = handle.child.lock();
    let c = g.as_mut()?;
    matches!(c.try_wait(), Ok(None)).then(|| c.id())
}

/// Loopback port of `url` when its host is a local one (remote `BOT_URL`s are
/// the user's explicit choice and cannot be checked by socket owner).
fn loopback_port(url: &str) -> Option<u16> {
    let hp = host_port(url)?;
    let (host, port) = hp.rsplit_once(':')?;
    matches!(host, "127.0.0.1" | "localhost")
        .then(|| port.parse().ok())
        .flatten()
}

/// Identity gate run before the bearer token is handed out.  The health probe
/// is unauthenticated, so an answer only proves "something listens".  The
/// owning process of the socket must be the child this app spawned (exact PID),
/// or, when the app has no running child and did not just spawn one, a process
/// of the same OS user.  After our own spawn with no running child the
/// listener is a squatter that won the bind race.
async fn check_listener(handle: &DaemonHandle, url: &str, after_spawn: bool) -> Result<(), String> {
    let Some(port) = loopback_port(url) else {
        return Ok(());
    };
    let expect = match child_pid(handle) {
        Some(pid) => Expect::Child(pid),
        None if after_spawn => {
            return Err(format!(
                "something other than the daemon this app started answers at {url}; \
                 refusing to send the API token"
            ));
        }
        None => Expect::SameUser,
    };
    tokio::task::spawn_blocking(move || listener::verify(port, expect))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("{e}; refusing to send the API token to {url}"))
}

fn connection(handle: &DaemonHandle, url: String, data: &Path) -> Result<Connection, String> {
    let token = read_token(data)?;
    ensure_logging(data);
    support_logs::tracing::info!(target: "praxi_support", phase = "ready");
    Ok(Connection {
        base_url: url,
        token,
        started_by_app: handle.started_by_app(),
        fake_provider: *handle.fake.lock(),
        safe_mode: *handle.safe.lock(),
        data_dir: data.display().to_string(),
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
    connect_locked(handle).await
}

/// Stop a daemon this app started and connect again.  The `connecting` lock is
/// held across stop + connect so a concurrent [`connect`] can neither observe
/// the half-stopped state nor race the restart.
///
/// # Errors
/// Same as [`connect`].
pub async fn restart(
    handle: &DaemonHandle,
    safe: Option<bool>,
    reason: &str,
) -> Result<Connection, String> {
    let _one_at_a_time = handle.connecting.lock().await;
    let data = data_dir()?;
    ensure_logging(&data);
    let reason = clean_reason(reason);
    let log_reason = if reason.starts_with("retry") {
        "retry"
    } else if safe.is_some() {
        "safe_mode"
    } else if reason.contains("provider") {
        "providers_changed"
    } else {
        "owner_request"
    };
    support_logs::tracing::info!(target: "praxi_support", phase = "stopping", reason = log_reason);
    let age = handle.last_spawn.lock().map(|t| t.elapsed());
    let mode_change = safe.is_some_and(|s| s != *handle.safe.lock());
    // Only "retry..." requests (a button pressed while the service is coming back) may be
    // merged; a providers reload or a mode switch always restarts.
    if reason.starts_with("retry")
        && restart_is_redundant(age, mode_change)
        && handle.started_by_app()
    {
        let url = base_url()?;
        if healthy(&url).await {
            support_logs::tracing::info!(target: "praxi_support", reason = "restart_skipped");
            // Another restart (or the start-up) brought the service up moments ago and it
            // answers: killing it again would only cause a second gap.
            append_log(
                &data,
                "app",
                &format!("restart skipped (service just started), reason: {reason}"),
            );
            return connect_locked(handle).await;
        }
    }
    append_log(
        &data,
        "app",
        &format!("restart requested, reason: {reason}, safe mode: {safe:?}"),
    );
    handle.stop();
    // `None` keeps the current mode (a provider reload must not leave safe mode);
    // only the owner's explicit choice in the app changes it.
    if let Some(safe) = safe {
        *handle.safe.lock() = safe;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    connect_locked(handle).await
}

/// A restart asked within this long after the app spawned the service, while it answers, is a
/// duplicate (two buttons, a retry racing the banner) and is skipped.
const RESTART_COALESCE: Duration = Duration::from_secs(5);

/// Whether a restart request adds nothing: the service was spawned moments ago and the request
/// does not change the mode (an owner's safe-mode switch must always be honoured).
fn restart_is_redundant(since_spawn: Option<Duration>, mode_change: bool) -> bool {
    !mode_change && since_spawn.is_some_and(|age| age < RESTART_COALESCE)
}

/// The reason of a restart as one short log token (it comes from the UI: never trust its shape).
fn clean_reason(reason: &str) -> String {
    let t: String = reason
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ' '))
        .take(60)
        .collect();
    if t.trim().is_empty() {
        "unspecified".to_owned()
    } else {
        t
    }
}

/// [`connect`] with the `connecting` lock already held by the caller.
async fn connect_locked(handle: &DaemonHandle) -> Result<Connection, String> {
    let url = base_url()?;
    let data = data_dir()?;
    if healthy(&url).await {
        check_listener(handle, &url, false).await?;
        handle.set_stage(Stage::Ready, None);
        return connection(handle, url, &data);
    }
    // A previously started child that died is dropped first.
    if !handle.started_by_app() {
        handle.stop();
        if std::env::var_os("BOT_URL").is_some() {
            return Err(format!("no daemon answers at {url}"));
        }
        if let Some(who) = foreign_listener(&url).await {
            let msg = format!("port {} is in use by another program{who}", port());
            append_log(&data, "app", &msg);
            handle.set_stage(Stage::PortInUse, Some(msg.clone()));
            return Err(msg);
        }
        let fake = wants_fake();
        spawn_tracked(handle, &data, fake)?;
    }
    let mut fake = *handle.fake.lock();
    let deadline = tokio::time::Instant::now() + START_TIMEOUT;
    loop {
        if healthy(&url).await {
            if let Err(e) = check_listener(handle, &url, true).await {
                append_log(&data, "app", &e);
                handle.set_stage(Stage::Unreachable, Some(e.clone()));
                return Err(e);
            }
            break;
        }
        // A child that already exited will never answer.
        if let Some(code) = child_exit_code(handle) {
            wait_for_pump(handle).await;
            // No usable provider (none configured, all disabled, key missing):
            // `bot serve` exits immediately.  Retry once with `--fake-provider`
            // so the UI can still connect and the user can fix it, but only
            // for that reason.
            let offset = *handle.log_offset.lock();
            if !fake
                && no_usable_provider_since(&data, offset)
                && spawn_tracked(handle, &data, true).is_ok()
            {
                fake = true;
                tokio::time::sleep(Duration::from_millis(250)).await;
                continue;
            }
            return Err(exited(handle, &data, code));
        }
        if tokio::time::Instant::now() >= deadline {
            let msg = format!(
                "the daemon did not become healthy at {url} (see {})",
                log_path(&data).display()
            );
            append_log(&data, "app", &msg);
            handle.set_stage(Stage::NotResponding, Some(msg.clone()));
            return Err(msg);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    handle.set_stage(Stage::Ready, None);
    connection(handle, url, &data)
}

/// Record that the child ended with `code` and word it.
fn exited(handle: &DaemonHandle, data: &Path, code: Option<i32>) -> String {
    let log_exit = code.map_or_else(|| "unknown".to_owned(), |c| c.to_string());
    support_logs::tracing::warn!(target: "praxi_support", phase = "failed", reason = "child_exited", exit_code = log_exit.as_str());
    let tail: Vec<String> = handle.tail.lock().iter().cloned().collect();
    let clash = looks_like_port_clash(&tail.join("\n"));
    let shown = code.map_or_else(|| "unknown".to_owned(), |c| c.to_string());
    let msg = if clash {
        format!("port {} is in use by another program", port())
    } else {
        format!(
            "the daemon exited right after start with code {shown} (see {})",
            log_path(data).display()
        )
    };
    append_log(data, "app", &format!("daemon exited with code {shown}"));
    {
        let mut d = handle.diag.lock();
        d.stage = if clash {
            Stage::PortInUse
        } else {
            Stage::Exited
        };
        d.exit_code = code;
        d.error = Some(msg.clone());
    }
    msg
}

/// `Some(code)` once the app-owned child has ended (`code` is `None` when the OS gave none).
#[allow(clippy::option_option)] // outer: has it ended; inner: did the OS give a code
fn child_exit_code(handle: &DaemonHandle) -> Option<Option<i32>> {
    let mut g = handle.child.lock();
    let c = g.as_mut()?;
    match c.try_wait() {
        Ok(None) => None,
        Ok(Some(status)) => Some(status.code()),
        Err(_) => Some(None),
    }
}

/// Give the stderr pump a moment to copy the last words of a child that just ended.
async fn wait_for_pump(handle: &DaemonHandle) {
    let done = handle.pump_done.lock().clone();
    let Some(done) = done else { return };
    for _ in 0..20 {
        if done.load(Ordering::SeqCst) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Whether a socket is open on `url` that is not a Praxi daemon (it accepts connections but never
/// answers the health probe).  Returns " (pid N)" when the owner is known, else an empty string.
async fn foreign_listener(url: &str) -> Option<String> {
    let hp = host_port(url)?;
    let open = matches!(
        tokio::time::timeout(PROBE_TIMEOUT, TcpStream::connect(&hp)).await,
        Ok(Ok(_))
    );
    if !open {
        return None;
    }
    // A daemon that is just binding answers within a moment.
    for _ in 0..4 {
        tokio::time::sleep(Duration::from_millis(400)).await;
        if healthy(url).await {
            return None;
        }
    }
    let owners = loopback_port(url)
        .map(listener::listener_owners)
        .unwrap_or_default();
    Some(
        owners
            .iter()
            .next()
            .map_or_else(String::new, |pid| format!(" (pid {pid})")),
    )
}

/// The start-up facts for the "the service does not start" screen.
#[must_use]
pub fn diagnostics(handle: &DaemonHandle) -> Diagnostics {
    let data = data_dir().unwrap_or_default();
    let bin = bot_binary();
    let (stage, since, mut error, mut exit_code) = {
        let d = handle.diag.lock();
        (d.stage, d.since, d.error.clone(), d.exit_code)
    };
    // The process may have ended since the start loop last looked.
    if matches!(stage, Stage::Waiting | Stage::NotResponding)
        && let Some(code) = child_exit_code(handle)
    {
        exit_code = code;
        error.get_or_insert_with(|| "the daemon exited".to_owned());
    }
    let mut log_tail: Vec<String> = handle.tail.lock().iter().cloned().collect();
    if log_tail.is_empty() {
        log_tail = read_tail(&log_path(&data), 30);
    }
    Diagnostics {
        stage,
        elapsed_ms: since.map_or(0, |s| {
            u64::try_from(s.elapsed().as_millis()).unwrap_or(u64::MAX)
        }),
        port: port(),
        base_url: base_url().unwrap_or_default(),
        data_dir: data.display().to_string(),
        log_path: log_path(&data).display().to_string(),
        sidecar: bin.to_string_lossy().into_owned(),
        sidecar_exists: Path::new(&bin).is_file(),
        pid: child_pid(handle),
        exit_code,
        error,
        log_tail,
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
        os: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
    }
}

/// The first-run prerequisite check for this app's data folder, service file and port.
#[must_use]
pub fn prereq_report(handle: &DaemonHandle) -> crate::prereq::Report {
    let data = data_dir().unwrap_or_default();
    crate::prereq::check(
        &data,
        Path::new(&bot_binary()),
        port(),
        handle.started_by_app(),
    )
}

/// The last `n` lines of a text file (lossy UTF-8; empty when unreadable).
fn read_tail(path: &Path, n: usize) -> Vec<String> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    let from = bytes.len().saturating_sub(64 * 1024);
    let text = String::from_utf8_lossy(&bytes[from..]);
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..]
        .iter()
        .map(|l| support_logs::safe_stderr(l))
        .collect()
}

/// The folder that holds `desktop-daemon.log`, created when missing (for "Open log folder").
///
/// # Errors
/// The data directory cannot be determined or created.
pub fn log_folder() -> Result<PathBuf, String> {
    let dir = data_dir()?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {} ({e})", dir.display()))?;
    Ok(dir)
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
    fn bot_url_must_be_a_loopback_http_origin() {
        for ok in [
            "http://127.0.0.1:47821",
            "http://localhost:9000/",
            "http://[::1]:47821",
        ] {
            assert!(validate_bot_url(ok).is_ok(), "{ok}");
        }
        assert_eq!(
            validate_bot_url("http://localhost:9000/").as_deref(),
            Ok("http://localhost:9000")
        );
        for bad in [
            "http://example.com:47821",
            "http://192.168.1.5:47821",
            "http://0.0.0.0:47821",
            "https://127.0.0.1:47821",
            "http://127.0.0.1@evil.example:80",
            "http://127.0.0.1",
            "http://localhost:notaport",
            "127.0.0.1:47821",
        ] {
            let err = validate_bot_url(bad).expect_err(bad);
            assert!(err.contains("BOT_URL must be"), "{bad}: {err}");
        }
    }

    #[test]
    fn token_must_be_64_hex() {
        assert!(valid_token(&"a".repeat(64)));
        assert!(!valid_token("short"));
        assert!(!valid_token(&"g".repeat(64)));
    }

    #[test]
    fn health_states_follow_the_shared_contract_fixture() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../crates/daemon-api/tests/fixtures/health-contract.json"
        ))
        .unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let want = match case["state"].as_str().unwrap() {
                "unreachable" => HealthState::Unreachable,
                "starting" => HealthState::Starting,
                "degraded" => HealthState::Degraded,
                "setup_required" => HealthState::SetupRequired,
                "ok" => HealthState::Ok,
                other => panic!("{name}: unknown state {other}"),
            };
            let bodies = if case["body"].is_null() {
                vec![String::new()]
            } else {
                // compact and pretty-printed JSON must classify the same
                vec![
                    serde_json::to_string(&case["body"]).unwrap(),
                    serde_json::to_string_pretty(&case["body"]).unwrap(),
                ]
            };
            for body in bodies {
                let text = match case["http_status"].as_u64() {
                    Some(code) => {
                        format!("HTTP/1.1 {code} X\r\n\r\n{body}")
                    }
                    None => String::new(),
                };
                assert_eq!(health_state(&text), want, "{name}");
                assert_eq!(
                    health_state(&text).is_up(),
                    case["up"].as_bool().unwrap(),
                    "{name}"
                );
            }
        }
    }

    #[test]
    fn health_reply_of_a_fresh_install_counts_as_alive() {
        let reply = |body: &str| format!("HTTP/1.1 200 OK\r\n\r\n{body}");
        assert!(health_reply_is_alive(&reply(
            r#"{"status":"setup_required","version":"0"}"#
        )));
        assert!(health_reply_is_alive(&reply(r#"{"status":"ok"}"#)));
        assert!(!health_reply_is_alive(&reply(r#"{"status":"broken"}"#)));
        assert!(!health_reply_is_alive(
            "HTTP/1.1 503 Service Unavailable\r\n\r\n{\"status\":\"ok\"}"
        ));
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

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "praxibot-daemon-{tag}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn fake_fallback_only_for_the_no_usable_provider_exit() {
        let d = temp_dir("fake");
        let log = d.join("desktop-daemon.log");
        // Old output before the offset is ignored.
        std::fs::write(&log, "Error: no usable provider; run `bot setup`\n").unwrap();
        let off = log_len(&d);
        assert!(!no_usable_provider_since(&d, off));
        // Disabled-only / missing-key onboarding exit: fall back.
        let mut f = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
        std::io::Write::write_all(&mut f, b"Error: no usable provider; run `bot setup`\n").unwrap();
        assert!(no_usable_provider_since(&d, off));
        // A different failure (corrupt config) is a real error.
        std::fs::write(&log, "Error: providers.toml: parse error\n").unwrap();
        assert!(!no_usable_provider_since(&d, 0));
        // No log at all.
        let _ = std::fs::remove_file(&log);
        assert!(!no_usable_provider_since(&d, 0));
        let _ = std::fs::remove_dir_all(&d);
    }

    async fn ok_daemon() -> u16 {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                let mut b = [0u8; 512];
                let _ = s.read(&mut b).await;
                let body = r#"{"status":"ok"}"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = s.write_all(resp.as_bytes()).await;
            }
        });
        port
    }

    #[tokio::test]
    async fn token_is_withheld_from_a_foreign_listener() {
        // Same `/proc` race as `resolves_our_own_listener`: the exact-pid
        // check at the bottom shares this process with every other test.
        if !crate::listener::enter_isolated(
            "daemon::tests::token_is_withheld_from_a_foreign_listener",
        ) {
            return;
        }
        // This test process plays the squatter: it answers a healthy probe.
        let port = ok_daemon().await;
        let url = format!("http://127.0.0.1:{port}");
        assert!(healthy(&url).await);

        // The app spawned a child (any process that is not the listener).
        let handle = DaemonHandle::default();
        let mut cmd = if cfg!(windows) {
            let mut c = Command::new("cmd");
            c.args(["/C", "ping -n 30 127.0.0.1 >NUL"]);
            c
        } else {
            let mut c = Command::new("sleep");
            c.arg("30");
            c
        };
        *handle.child.lock() = Some(cmd.stdout(Stdio::null()).spawn().unwrap());
        let err = check_listener(&handle, &url, true).await.unwrap_err();
        assert!(err.contains("refusing to send the API token"), "{err}");

        // Squatter wins the race and our child is gone: still refused.
        handle.stop();
        assert!(check_listener(&handle, &url, true).await.is_err());

        // Control: the check passes for the real owner of the socket.
        assert!(listener::verify(port, Expect::Child(std::process::id())).is_ok());
        // A pre-existing daemon of the same user is accepted.
        assert!(check_listener(&handle, &url, false).await.is_ok());
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

    #[test]
    fn log_timestamps_are_utc() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(format_utc(1_709_164_800), "2024-02-29T00:00:00Z");
    }

    #[test]
    fn a_second_restart_right_after_a_spawn_is_redundant_unless_the_mode_changes() {
        let fresh = Some(Duration::from_secs(1));
        assert!(restart_is_redundant(fresh, false));
        assert!(!restart_is_redundant(fresh, true));
        assert!(!restart_is_redundant(Some(Duration::from_secs(30)), false));
        assert!(!restart_is_redundant(None, false));
    }

    #[test]
    fn restart_reasons_are_short_plain_log_tokens() {
        assert_eq!(clean_reason(""), "unspecified");
        assert_eq!(
            clean_reason(
                "banner
FAKE log line"
            ),
            "bannerFAKE log line"
        );
        assert_eq!(clean_reason(&"a".repeat(500)).len(), 60);
    }

    #[test]
    fn a_dead_start_is_worded_by_its_cause() {
        // The OS refuses the program: blocked, not "missing".
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert_eq!(classify_spawn_error(&denied, true), Stage::SidecarBlocked);
        let virus = std::io::Error::from_raw_os_error(225);
        assert_eq!(classify_spawn_error(&virus, true), Stage::SidecarBlocked);
        let gone = std::io::Error::from(std::io::ErrorKind::NotFound);
        assert_eq!(classify_spawn_error(&gone, false), Stage::SidecarMissing);
        // Another port user, as Windows and Linux word a failed bind.
        assert!(looks_like_port_clash(
            "Error: Only one usage of each socket address (protocol/network address/port) is normally permitted. (os error 10048)"
        ));
        assert!(looks_like_port_clash(
            "Error: Address already in use (os error 98)"
        ));
        assert!(!looks_like_port_clash("Error: OS keychain unavailable"));

        // A child that ended with a missing-DLL status: the code and the last output are kept.
        let handle = DaemonHandle::default();
        handle
            .tail
            .lock()
            .push_back("(secret backend: Keyring)".to_owned());
        let dir = temp_dir("exited");
        let msg = exited(&handle, &dir, Some(-1_073_741_515));
        assert!(msg.contains("-1073741515"), "{msg}");
        {
            let d = handle.diag.lock();
            assert_eq!(d.stage, Stage::Exited);
            assert_eq!(d.exit_code, Some(-1_073_741_515));
        }
        let log = std::fs::read_to_string(log_path(&dir)).unwrap();
        assert!(
            log.contains("[app] daemon exited with code -1073741515"),
            "{log}"
        );
        handle
            .tail
            .lock()
            .push_back("Error: Address already in use (os error 98)".to_owned());
        let _ = exited(&handle, &dir, Some(1));
        assert_eq!(handle.diag.lock().stage, Stage::PortInUse);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_log_is_rotated_once_it_is_large() {
        let d = temp_dir("rotate");
        append_log(&d, "app", "small");
        rotate_log(&d);
        assert!(log_path(&d).exists());
        std::fs::write(
            log_path(&d),
            vec![b'x'; usize::try_from(MAX_LOG_BYTES).unwrap() + 1],
        )
        .unwrap();
        rotate_log(&d);
        assert!(!log_path(&d).exists());
        assert!(d.join("desktop-daemon.log.1").exists());
        let _ = std::fs::remove_dir_all(&d);
    }
}
