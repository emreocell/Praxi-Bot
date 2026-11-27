//! The in-app terminal of a host: a shell on the bots' home, opened by the owner's paired
//! admin desktop over the EXISTING paired link (no SSH, no new port).
//!
//! Authority (this is a shell as the service user, so it is as strict as the install route):
//!
//! * host side (`/v1/admin/terminal...`): only a paired DESKTOP that holds host
//!   administration (`Device::admin`, checked on every request) on a daemon that runs as a
//!   host. A bot, a tool, the MCP facade, a schedule, a phone, a node or any holder of only
//!   the API token has no peer identity (or the wrong role) and is refused, so it can neither
//!   open a session nor read its output;
//! * desktop side (`/v1/hosts/{id}/terminal...`): the local user's owner key is required, the
//!   same key that guards every other action that reaches into a host;
//! * one session per desktop (a new one replaces the old), at most [`MAX_SESSIONS`] in total;
//!   a session belongs to the desktop that opened it and nobody else can poll, type into or
//!   close it;
//! * it ends when the desktop stops polling ([`DISCONNECT_AFTER`]), after [`IDLE_AFTER`]
//!   without input, at the hard cap, when the desktop loses administration or is unpaired,
//!   and when the shell exits;
//! * the environment is the shell tool's allowlist (see `builtin_tools::shell::terminal_env`: the bot shell's allowlist plus the `bot` directory on PATH, which a bot's own shell does not get)
//!   plus the data dir: no owner key, API token, vault passphrase or provider key;
//! * audit: session open and close (peer, size, duration, byte counts, reason). Keystrokes
//!   and output are never recorded.
//!
//! Output is a bounded ring ([`MAX_BUFFER`]); the desktop polls with the absolute byte offset
//! it has seen. Linux hosts only: elsewhere the open route answers `terminal_unsupported`.

#![cfg_attr(not(unix), allow(dead_code, unused_imports))]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use base64::Engine as _;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    ApiError, AppState, PeerIdentity, PeerRole,
    host_admin::{admin_for, audit, require_admin},
    routes::ApiJson,
};

/// Sessions on one host, all desktops together.
pub(crate) const MAX_SESSIONS: usize = 4;
/// No poll or input from the desktop for this long: it is gone, the shell is killed.
pub(crate) const DISCONNECT_AFTER: Duration = Duration::from_secs(45);
/// No input for this long: the shell is killed.
pub(crate) const IDLE_AFTER: Duration = Duration::from_secs(15 * 60);
/// No session lives longer than this.
pub(crate) const HARD_CAP: Duration = Duration::from_secs(8 * 60 * 60);
/// Most output kept for a slow reader; older bytes are dropped (`dropped: true`).
pub(crate) const MAX_BUFFER: usize = 256 * 1024;
/// Most bytes one poll returns.
const MAX_POLL_BYTES: usize = 64 * 1024;
/// Longest long-poll wait.
const MAX_WAIT_MS: u64 = 3_000;
/// Largest input chunk (decoded).
const MAX_INPUT_BYTES: usize = 16 * 1024;
/// Request body limit of the terminal routes (base64 of [`MAX_INPUT_BYTES`] plus JSON).
const MAX_BODY: usize = 32 * 1024;

pub mod codes {
    /// This host cannot run a terminal (not a Linux host).
    pub const UNSUPPORTED: &str = "terminal_unsupported";
    /// The shell could not be started.
    pub const START_FAILED: &str = "terminal_start_failed";
    /// All terminal slots of this host are in use.
    pub const BUSY: &str = "terminal_busy";
    /// The session is gone (ended, replaced or never opened by this desktop).
    pub const GONE: &str = "terminal_gone";
    /// The host is online but runs a daemon older than this app (no terminal route).
    pub const HOST_OUTDATED: &str = "host_outdated";
    /// The host is online but refused this desktop for the terminal.
    pub const NOT_PERMITTED: &str = "host_not_permitted";
    /// The host answered, but with an error of its own (not a refusal, not a missing route).
    pub const HOST_FAILED: &str = "host_terminal_failed";
}

/// The terminal sessions of this daemon, by the key of the desktop that owns each.
#[derive(Default)]
pub(crate) struct Terminals {
    sessions: Mutex<BTreeMap<String, Arc<Session>>>,
}

/// Output kept for the reader: `data[0]` is the byte at absolute offset `start`.
#[derive(Default)]
struct OutBuf {
    data: Vec<u8>,
    start: u64,
    eof: bool,
}

impl OutBuf {
    fn end(&self) -> u64 {
        self.start + self.data.len() as u64
    }

    fn push(&mut self, bytes: &[u8]) {
        self.data.extend_from_slice(bytes);
        if self.data.len() > MAX_BUFFER {
            let cut = self.data.len() - MAX_BUFFER;
            self.data.drain(..cut);
            self.start += cut as u64;
        }
    }

    /// Up to [`MAX_POLL_BYTES`] from absolute offset `after`, and whether bytes
    /// before that were dropped.
    fn read_from(&self, after: u64) -> (Vec<u8>, u64, bool) {
        let dropped = after < self.start;
        let from = after.max(self.start);
        let skip = usize::try_from(from - self.start).unwrap_or(usize::MAX);
        let chunk: Vec<u8> = self
            .data
            .get(skip..)
            .unwrap_or_default()
            .iter()
            .copied()
            .take(MAX_POLL_BYTES)
            .collect();
        let next = from + chunk.len() as u64;
        (chunk, next, dropped)
    }
}

struct Session {
    id: Uuid,
    /// Key of the desktop that owns it.
    peer: String,
    opened: Instant,
    out: Mutex<OutBuf>,
    wake: tokio::sync::Notify,
    last_seen: Mutex<Instant>,
    last_input: Mutex<Instant>,
    bytes_in: AtomicU64,
    /// Exit code of the shell and when it ended.
    exit: Mutex<Option<(i32, Instant)>>,
    closed: AtomicBool,
    #[cfg(unix)]
    pty: pty::Pty,
}

impl Session {
    fn seen(&self) {
        *self.last_seen.lock() = Instant::now();
    }
}

#[cfg(unix)]
mod pty {
    //! Pseudo-terminal plumbing: no `unsafe`. The controlling terminal is set up by
    //! `setsid -w -c` (util-linux), so the shell has job control.
    use std::ffi::OsString;
    use std::fs::File;
    use std::io::Write;
    use std::process::Stdio;

    use rustix::process::{Pid, Signal};
    use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
    use rustix::termios::{Winsize, tcsetwinsize};

    pub(super) struct Pty {
        master: File,
        pid: u32,
    }

    fn size(cols: u16, rows: u16) -> Winsize {
        Winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        }
    }

    /// How the shell gets its terminal.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) enum Strategy {
        /// `setsid -w -c`: own session and controlling terminal, so the shell has job control.
        Setsid,
        /// A plain shell in its own process group: works where `setsid -c` is refused
        /// (containers, sandboxes, a session leader); the shell warns it has no job control.
        Plain,
    }

    pub(super) fn open_pty(cols: u16, rows: u16) -> std::io::Result<(File, File)> {
        let master =
            openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).map_err(std::io::Error::from)?;
        grantpt(&master).map_err(std::io::Error::from)?;
        unlockpt(&master).map_err(std::io::Error::from)?;
        let name = ptsname(&master, Vec::new()).map_err(std::io::Error::from)?;
        let name = name
            .to_str()
            .map_err(|_| std::io::Error::other("pty name is not text"))?
            .to_owned();
        let master = File::from(master);
        tcsetwinsize(&master, size(cols, rows)).map_err(std::io::Error::from)?;
        // `O_NOCTTY` is essential: the daemon runs as a systemd service, i.e. as a session leader
        // with no controlling terminal, and opening a terminal device without it makes THIS pty
        // the daemon's own controlling terminal. When the shell ends and the master is closed, the
        // kernel then hangs the terminal up and sends SIGHUP to the daemon, which dies (systemd
        // calls that a clean stop and starts it again), and `setsid -c` for the shell fails with
        // EPERM because the terminal already belongs to the daemon's session.
        let slave = rustix::fs::open(
            name.as_str(),
            rustix::fs::OFlags::RDWR | rustix::fs::OFlags::NOCTTY | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(std::io::Error::from)?;
        Ok((master, File::from(slave)))
    }

    /// Whether `setsid -w -c` really works on this host, found by running it once on a throwaway
    /// pty (`setsid` may exist yet get EPERM for `-c`). Any failure means [`Strategy::Plain`].
    pub(super) fn probe(setsid_bin: &str) -> Strategy {
        let Ok((_master, slave)) = open_pty(80, 24) else {
            return Strategy::Plain;
        };
        let (Ok(i), Ok(o), Ok(e)) = (slave.try_clone(), slave.try_clone(), slave.try_clone())
        else {
            return Strategy::Plain;
        };
        let Ok(mut child) = std::process::Command::new(setsid_bin)
            .args(["-w", "-c", "true"])
            .stdin(Stdio::from(i))
            .stdout(Stdio::from(o))
            .stderr(Stdio::from(e))
            .spawn()
        else {
            return Strategy::Plain;
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => return Strategy::Setsid,
                Ok(Some(_)) | Err(_) => return Strategy::Plain,
                Ok(None) if std::time::Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Strategy::Plain;
                }
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
            }
        }
    }

    static HOST: std::sync::OnceLock<Strategy> = std::sync::OnceLock::new();

    /// The strategy for this host, probed once and remembered.
    pub(super) fn host_strategy() -> Strategy {
        *HOST.get_or_init(|| probe("setsid"))
    }

    /// Resolves `cell` (probing at most once) on a blocking thread, so the first terminal open
    /// never parks an async worker for the probe's up to 3 s.
    pub(super) async fn strategy_off_runtime(
        cell: &'static std::sync::OnceLock<Strategy>,
        probe: impl FnOnce() -> Strategy + Send + 'static,
    ) -> Strategy {
        if let Some(s) = cell.get() {
            return *s;
        }
        tokio::task::spawn_blocking(move || *cell.get_or_init(probe))
            .await
            .unwrap_or(Strategy::Plain)
    }

    /// Makes sure [`host_strategy`] is known, probing off the async runtime if needed.
    pub(super) async fn warm_host_strategy() {
        strategy_off_runtime(&HOST, || probe("setsid")).await;
    }

    /// A shell on a fresh pseudo-terminal; returns the handle, the child and a reader of
    /// the terminal's output.
    pub(super) fn spawn(
        shell: &str,
        cols: u16,
        rows: u16,
        env: &[(OsString, OsString)],
        cwd: Option<&std::path::Path>,
    ) -> std::io::Result<(Pty, tokio::process::Child, File)> {
        spawn_with(host_strategy(), shell, cols, rows, env, cwd)
    }

    pub(super) fn spawn_with(
        strategy: Strategy,
        shell: &str,
        cols: u16,
        rows: u16,
        env: &[(OsString, OsString)],
        cwd: Option<&std::path::Path>,
    ) -> std::io::Result<(Pty, tokio::process::Child, File)> {
        let (master, slave) = open_pty(cols, rows)?;
        let mut cmd = match strategy {
            Strategy::Setsid => {
                let mut c = tokio::process::Command::new("setsid");
                c.args(["-w", "-c", shell, "-l"]);
                c
            }
            Strategy::Plain => {
                let mut c = tokio::process::Command::new(shell);
                c.arg("-l");
                c.process_group(0);
                c
            }
        };
        cmd.env_clear();
        for (k, v) in env {
            cmd.env(k, v);
        }
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }
        cmd.stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave.try_clone()?))
            .kill_on_drop(true);
        let child = cmd.spawn()?;
        let pid = child
            .id()
            .ok_or_else(|| std::io::Error::other("the shell ended at once"))?;
        let reader = master.try_clone()?;
        Ok((Pty { master, pid }, child, reader))
    }

    /// `(pid, ppid, session)` of every process (Linux `/proc`; empty elsewhere).
    fn process_table() -> Vec<(i32, i32, i32)> {
        let Ok(dir) = std::fs::read_dir("/proc") else {
            return Vec::new();
        };
        dir.filter_map(|e| {
            let pid: i32 = e.ok()?.file_name().to_str()?.parse().ok()?;
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
            // `pid (comm) state ppid pgrp session ...`; comm may hold spaces and parentheses.
            let rest = &stat[stat.rfind(')')? + 1..];
            let mut f = rest.split_whitespace().skip(1);
            let parent: i32 = f.next()?.parse().ok()?;
            let _pgrp = f.next()?;
            let session: i32 = f.next()?.parse().ok()?;
            Some((pid, parent, session))
        })
        .collect()
    }

    /// `root`, the processes in session `root`, and all their descendants.
    fn tree_of(root: i32) -> Vec<i32> {
        let table = process_table();
        let mut tree: Vec<i32> = table
            .iter()
            .filter(|(pid, _, session)| *pid == root || *session == root)
            .map(|(pid, _, _)| *pid)
            .collect();
        let mut i = 0;
        while i < tree.len() {
            let parent = tree[i];
            for (candidate, its_parent, _) in &table {
                if *its_parent == parent && !tree.contains(candidate) {
                    tree.push(*candidate);
                }
            }
            i += 1;
        }
        tree
    }

    impl Pty {
        pub(super) fn write_all(&self, bytes: &[u8]) -> std::io::Result<()> {
            let mut f = self.master.try_clone()?;
            f.write_all(bytes)?;
            f.flush()
        }

        pub(super) fn resize(&self, cols: u16, rows: u16) -> std::io::Result<()> {
            tcsetwinsize(&self.master, size(cols, rows)).map_err(std::io::Error::from)
        }

        /// `sig` to the shell's whole process group AND to every process of its tree: its
        /// descendants and everything still in its session (`nohup` / own-pgid jobs). The caller
        /// must only call this while the shell has not been reaped, so the pid is still ours.
        pub(super) fn kill_group(&self, sig: Signal) {
            let Ok(root) = i32::try_from(self.pid) else {
                return;
            };
            let mut victims = tree_of(root);
            victims.retain(|p| *p != root);
            if let Some(pid) = Pid::from_raw(root) {
                let _ = rustix::process::kill_process_group(pid, sig);
            }
            for p in victims {
                if let Some(pid) = Pid::from_raw(p) {
                    let _ = rustix::process::kill_process(pid, sig);
                }
            }
        }

        pub(super) fn hangup(&self) {
            self.kill_group(Signal::HUP);
        }

        pub(super) fn kill(&self) {
            self.kill_group(Signal::KILL);
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenTerminalInput {
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct TerminalInfo {
    pub id: Uuid,
    pub cols: u16,
    pub rows: u16,
    /// Seconds without any request after which the host ends the session.
    pub disconnect_after_secs: u64,
    /// Seconds without input after which the host ends the session.
    pub idle_after_secs: u64,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct TerminalOutput {
    /// Base64 of the next output bytes (terminal output is bytes, not text).
    pub data: String,
    /// Offset to pass as `after` next time.
    pub next: u64,
    /// Output older than the buffer was dropped before it was read.
    pub dropped: bool,
    /// The shell has ended (exit code, -1 when it was killed) and all output was read.
    pub exited: Option<i32>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TerminalInput {
    /// Base64 of the bytes to type (at most 16 KiB).
    pub data: String,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TerminalResize {
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Deserialize)]
pub struct OutputQuery {
    #[serde(default)]
    after: u64,
    #[serde(default)]
    wait_ms: u64,
}

fn clamp_size(cols: u16, rows: u16) -> (u16, u16) {
    (cols.clamp(20, 400), rows.clamp(5, 200))
}

fn peer_key(peer: Option<&Extension<PeerIdentity>>) -> Result<String, ApiError> {
    peer.filter(|p| p.0.role == PeerRole::Desktop)
        .map(|p| p.0.key.clone())
        .ok_or_else(|| ApiError::forbidden("the terminal is only for the owner's paired desktop"))
}

fn session_of(s: &AppState, peer: &str, id: &str) -> Result<Arc<Session>, ApiError> {
    let gone = || {
        ApiError::new(
            StatusCode::NOT_FOUND,
            codes::GONE,
            "the terminal session ended",
        )
    };
    let id: Uuid = id.parse().map_err(|_| gone())?;
    s.0.terminals
        .sessions
        .lock()
        .get(peer)
        .filter(|x| x.id == id)
        .cloned()
        .ok_or_else(gone)
}

/// End a session: kill the shell's process group, forget it, write the closing audit row.
fn close(s: &AppState, session: &Arc<Session>, reason: &str) {
    if session.closed.swap(true, Ordering::SeqCst) {
        return;
    }
    {
        let mut map = s.0.terminals.sessions.lock();
        if map.get(&session.peer).is_some_and(|x| x.id == session.id) {
            map.remove(&session.peer);
        }
    }
    // Only while the shell has not been reaped: after that its pid may belong to someone else.
    #[cfg(unix)]
    if session.exit.lock().is_none() {
        session.pty.hangup();
        let session = Arc::clone(session);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(2)).await;
            if session.exit.lock().is_none() {
                session.pty.kill();
            }
        });
    }
    let out_bytes = session.out.lock().end();
    let exit = session.exit.lock().map(|(code, _)| code);
    let _ = audit(
        s,
        &admin_for(&session.peer),
        "host.admin.terminal_close",
        &serde_json::json!({
            "session": session.id,
            "reason": reason,
            "seconds": session.opened.elapsed().as_secs(),
            "bytes_in": session.bytes_in.load(Ordering::Relaxed),
            "bytes_out": out_bytes,
            "exit": exit,
        }),
    );
    session.wake.notify_waiters();
}

/// Watches one session until it ends, whatever the desktop does (or stops doing).
#[cfg(unix)]
fn spawn_watch(s: &AppState, session: Arc<Session>) {
    let weak = Arc::downgrade(&s.0);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let Some(inner) = weak.upgrade() else {
                return;
            };
            let s = AppState(inner);
            if session.closed.load(Ordering::SeqCst) {
                return;
            }
            let identity = PeerIdentity {
                key: session.peer.clone(),
                role: PeerRole::Desktop,
            };
            let why = if s.shutdown_token().is_cancelled() {
                Some("shutdown")
            } else if !s.is_host_admin(Some(&identity)) {
                Some("revoked")
            } else if session.last_seen.lock().elapsed() >= DISCONNECT_AFTER {
                Some("disconnected")
            } else if session.last_input.lock().elapsed() >= IDLE_AFTER {
                Some("idle")
            } else if session.opened.elapsed() >= HARD_CAP {
                Some("time_limit")
            } else {
                None
            };
            if let Some(why) = why {
                close(&s, &session, why);
                return;
            }
        }
    });
}

#[cfg(unix)]
fn shell_path() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|p| p.starts_with('/') && std::path::Path::new(p).is_file())
        .unwrap_or_else(|| {
            if std::path::Path::new("/bin/bash").is_file() {
                "/bin/bash".into()
            } else {
                "/bin/sh".into()
            }
        })
}

/// `POST /v1/admin/terminal`: open a shell on this host for the calling admin desktop.
#[utoipa::path(post, operation_id = "admin_terminal_open", path = "/v1/admin/terminal", tag = "admin", security(("bearer" = [])),
    request_body = OpenTerminalInput,
    responses((status = 200, body = TerminalInfo), (status = 403, body = crate::error::ErrorBody), (status = 409, body = crate::error::ErrorBody)))]
pub(crate) async fn open(
    State(s): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    ApiJson(input): ApiJson<OpenTerminalInput>,
) -> Result<Json<TerminalInfo>, ApiError> {
    let admin = require_admin(&s, peer.as_ref().map(|p| &p.0))?;
    let key = peer_key(peer.as_ref())?;
    // The first open probes `setsid -c` (up to 3 s): do that on a blocking thread.
    #[cfg(unix)]
    pty::warm_host_strategy().await;
    open_session(&s, &admin, key, input)
}

#[cfg(not(unix))]
fn open_session(
    _s: &AppState,
    _admin: &crate::host_admin::Admin,
    _key: String,
    _input: OpenTerminalInput,
) -> Result<Json<TerminalInfo>, ApiError> {
    Err(ApiError::new(
        StatusCode::CONFLICT,
        codes::UNSUPPORTED,
        "the terminal needs a Linux host",
    ))
}

#[cfg(unix)]
#[allow(clippy::too_many_lines, clippy::needless_pass_by_value)] // one ordered start-up; `input` is the request body
fn open_session(
    s: &AppState,
    admin: &crate::host_admin::Admin,
    key: String,
    input: OpenTerminalInput,
) -> Result<Json<TerminalInfo>, ApiError> {
    if !cfg!(target_os = "linux") {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            codes::UNSUPPORTED,
            "the terminal needs a Linux host",
        ));
    }
    let (cols, rows) = clamp_size(input.cols, input.rows);
    // One session per desktop: a new one replaces the old.
    let old = s.0.terminals.sessions.lock().get(&key).cloned();
    if let Some(old) = old {
        close(s, &old, "replaced");
    }
    if s.0.terminals.sessions.lock().len() >= MAX_SESSIONS {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            codes::BUSY,
            "all terminal slots of this host are in use",
        ));
    }
    let env = session_env(builtin_tools::shell::terminal_env(), s.data_dir());
    // Start in HOME when the service has one (under the hardened unit `/home` is hidden), else
    // in the data dir, the one place the unit may write.
    let start_dir = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .filter(|h| h.is_dir())
        .unwrap_or_else(|| s.data_dir());
    let (handle, mut child, reader) =
        pty::spawn(&shell_path(), cols, rows, &env, Some(start_dir.as_path())).map_err(|_| {
            ApiError::new(
                StatusCode::CONFLICT,
                codes::START_FAILED,
                "the shell could not be started",
            )
        })?;
    let now = Instant::now();
    let session = Arc::new(Session {
        id: Uuid::now_v7(),
        peer: key.clone(),
        opened: now,
        out: Mutex::new(OutBuf::default()),
        wake: tokio::sync::Notify::new(),
        last_seen: Mutex::new(now),
        last_input: Mutex::new(now),
        bytes_in: AtomicU64::new(0),
        exit: Mutex::new(None),
        closed: AtomicBool::new(false),
        pty: handle,
    });
    audit(
        s,
        admin,
        "host.admin.terminal_open",
        &serde_json::json!({"session": session.id, "cols": cols, "rows": rows}),
    )?;
    s.0.terminals
        .sessions
        .lock()
        .insert(key, Arc::clone(&session));
    // The terminal's output, on its own thread: a blocking read must not sit on the runtime.
    {
        let session = Arc::clone(&session);
        let _ = std::thread::Builder::new()
            .name("terminal-reader".into())
            .spawn(move || {
                use std::io::Read as _;
                let mut reader = reader;
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            session.out.lock().push(&buf[..n]);
                            session.wake.notify_waiters();
                        }
                    }
                }
                session.out.lock().eof = true;
                session.wake.notify_waiters();
            });
    }
    {
        let session = Arc::clone(&session);
        tokio::spawn(async move {
            let code = child
                .wait()
                .await
                .ok()
                .and_then(|status| status.code())
                .unwrap_or(-1);
            *session.exit.lock() = Some((code, Instant::now()));
            session.wake.notify_waiters();
        });
    }
    spawn_watch(s, Arc::clone(&session));
    Ok(Json(TerminalInfo {
        id: session.id,
        cols,
        rows,
        disconnect_after_secs: DISCONNECT_AFTER.as_secs(),
        idle_after_secs: IDLE_AFTER.as_secs(),
    }))
}

/// `GET /v1/admin/terminal/{id}/output?after=&wait_ms=`: the output since byte `after`
/// (waits up to 3 s for some).
#[utoipa::path(get, operation_id = "admin_terminal_output", path = "/v1/admin/terminal/{id}/output", tag = "admin", security(("bearer" = [])),
    params(("id" = String, Path), ("after" = Option<u64>, Query), ("wait_ms" = Option<u64>, Query)),
    responses((status = 200, body = TerminalOutput), (status = 403, body = crate::error::ErrorBody), (status = 404, body = crate::error::ErrorBody)))]
pub(crate) async fn output(
    State(s): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    Path(id): Path<String>,
    Query(q): Query<OutputQuery>,
) -> Result<Json<TerminalOutput>, ApiError> {
    require_admin(&s, peer.as_ref().map(|p| &p.0))?;
    let key = peer_key(peer.as_ref())?;
    let session = session_of(&s, &key, &id)?;
    let deadline = Instant::now() + Duration::from_millis(q.wait_ms.min(MAX_WAIT_MS));
    loop {
        session.seen();
        // Register for the wake-up before looking, so nothing is missed in between.
        let woken = session.wake.notified();
        tokio::pin!(woken);
        woken.as_mut().enable();
        let (chunk, next, dropped, finished) = {
            let out = session.out.lock();
            let (chunk, next, dropped) = out.read_from(q.after);
            let caught_up = next >= out.end();
            // The shell is over when the terminal reported its end of output, or a second
            // after it exited (a background program may hold the terminal open).
            let over = session.closed.load(Ordering::SeqCst)
                || session
                    .exit
                    .lock()
                    .is_some_and(|(_, at)| out.eof || at.elapsed() >= Duration::from_secs(1));
            (chunk, next, dropped, over && caught_up)
        };
        let now = Instant::now();
        if !chunk.is_empty() || finished || now >= deadline {
            let exited = finished.then(|| session.exit.lock().map_or(-1, |(code, _)| code));
            return Ok(Json(TerminalOutput {
                data: base64::engine::general_purpose::STANDARD.encode(&chunk),
                next,
                dropped,
                exited,
            }));
        }
        let _ = tokio::time::timeout(deadline - now, woken).await;
    }
}

/// `POST /v1/admin/terminal/{id}/input`.
#[utoipa::path(post, operation_id = "admin_terminal_input", path = "/v1/admin/terminal/{id}/input", tag = "admin", security(("bearer" = [])),
    params(("id" = String, Path)), request_body = TerminalInput,
    responses((status = 204), (status = 403, body = crate::error::ErrorBody), (status = 404, body = crate::error::ErrorBody)))]
pub(crate) async fn input(
    State(s): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    Path(id): Path<String>,
    ApiJson(input): ApiJson<TerminalInput>,
) -> Result<StatusCode, ApiError> {
    require_admin(&s, peer.as_ref().map(|p| &p.0))?;
    let key = peer_key(peer.as_ref())?;
    let session = session_of(&s, &key, &id)?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(input.data.as_bytes())
        .ok()
        .filter(|b| b.len() <= MAX_INPUT_BYTES)
        .ok_or_else(|| ApiError::bad_request("input is not base64 or longer than 16 KiB"))?;
    session.seen();
    *session.last_input.lock() = Instant::now();
    session
        .bytes_in
        .fetch_add(bytes.len() as u64, Ordering::Relaxed);
    write_input(&session, bytes).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(unix)]
async fn write_input(session: &Arc<Session>, bytes: Vec<u8>) -> Result<(), ApiError> {
    let session = Arc::clone(session);
    tokio::task::spawn_blocking(move || session.pty.write_all(&bytes))
        .await
        .map_err(ApiError::internal)?
        .map_err(|_| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                codes::GONE,
                "the terminal session ended",
            )
        })
}

#[cfg(not(unix))]
#[allow(clippy::unused_async)]
async fn write_input(_session: &Arc<Session>, _bytes: Vec<u8>) -> Result<(), ApiError> {
    Err(ApiError::new(
        StatusCode::CONFLICT,
        codes::UNSUPPORTED,
        "the terminal needs a Linux host",
    ))
}

/// `POST /v1/admin/terminal/{id}/resize`.
#[utoipa::path(post, operation_id = "admin_terminal_resize", path = "/v1/admin/terminal/{id}/resize", tag = "admin", security(("bearer" = [])),
    params(("id" = String, Path)), request_body = TerminalResize,
    responses((status = 204), (status = 403, body = crate::error::ErrorBody), (status = 404, body = crate::error::ErrorBody)))]
pub(crate) async fn resize(
    State(s): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    Path(id): Path<String>,
    ApiJson(input): ApiJson<TerminalResize>,
) -> Result<StatusCode, ApiError> {
    require_admin(&s, peer.as_ref().map(|p| &p.0))?;
    let key = peer_key(peer.as_ref())?;
    let session = session_of(&s, &key, &id)?;
    session.seen();
    let (cols, rows) = clamp_size(input.cols, input.rows);
    #[cfg(unix)]
    session.pty.resize(cols, rows).map_err(ApiError::internal)?;
    #[cfg(not(unix))]
    let _ = (cols, rows);
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /v1/admin/terminal/{id}`: end the session.
#[utoipa::path(delete, operation_id = "admin_terminal_close", path = "/v1/admin/terminal/{id}", tag = "admin", security(("bearer" = [])),
    params(("id" = String, Path)),
    responses((status = 204), (status = 403, body = crate::error::ErrorBody)))]
pub(crate) async fn close_route(
    State(s): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_admin(&s, peer.as_ref().map(|p| &p.0))?;
    let key = peer_key(peer.as_ref())?;
    // Closing what is already gone is not an error.
    if let Ok(session) = session_of(&s, &key, &id) {
        close(&s, &session, "closed_by_user");
    }
    Ok(StatusCode::NO_CONTENT)
}

/// The host-side terminal routes (with their own small body limit).
/// The environment of one terminal session: the shell's (`base`), the terminal type, the data
/// folder, and the CLI folders on `PATH`.
fn session_env(
    mut base: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    data_dir: std::path::PathBuf,
) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    base.push(("TERM".into(), "xterm-256color".into()));
    base.push(("BOT_DATA_DIR".into(), data_dir.into_os_string()));
    with_cli_install_dirs(&mut base);
    base
}

/// The service user's CLI folders (`~/.local/bin`, the npm prefix, ...) on the terminal's `PATH`:
/// detection finds a CLI there, so a command typed in this terminal must find it too. A server
/// installed before the unit set a `PATH` of its own would otherwise answer "command not found"
/// for a CLI the app lists as installed.
fn with_cli_install_dirs(env: &mut Vec<(std::ffi::OsString, std::ffi::OsString)>) {
    let Some(home) = env
        .iter()
        .find(|(k, _)| k == "HOME")
        .map(|(_, v)| std::path::PathBuf::from(v))
    else {
        return;
    };
    let current = env.iter().position(|(k, _)| k == "PATH");
    let path = current.map(|i| env[i].1.clone());
    let Some(joined) = cli_provider::identity::path_with_install_dirs(path.as_deref(), &home)
    else {
        return;
    };
    match current {
        Some(i) => env[i].1 = joined,
        None => env.push(("PATH".into(), joined)),
    }
}

pub(crate) fn routes() -> axum::Router<AppState> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/v1/admin/terminal", post(open))
        .route(
            "/v1/admin/terminal/{id}",
            axum::routing::delete(close_route),
        )
        .route("/v1/admin/terminal/{id}/output", get(output))
        .route("/v1/admin/terminal/{id}/input", post(input))
        .route("/v1/admin/terminal/{id}/resize", post(resize))
        .layer(axum::extract::DefaultBodyLimit::max(MAX_BODY))
}

// ---------------------------------------------------------------------------------------
// The desktop's side: `/v1/hosts/{id}/terminal...` forwards to the host over the paired
// link. The owner key is required for every call (same as copying keys to a host).

fn valid_host(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.chars().all(|c| c.is_ascii_alphanumeric())
}

/// A host's error, with the codes the desktop words. `host_link_failed` means the host did not
/// answer (the link or a gateway in between failed: 502/503/504); a host that DID answer with
/// something else is `host_terminal_failed`, so "unreachable" is never said of a computer that
/// replied.
fn host_error(status: u16, body: &serde_json::Value) -> ApiError {
    let code = body["error"]["code"].as_str().unwrap_or_default();
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
    let known: &'static str = match code {
        codes::UNSUPPORTED => codes::UNSUPPORTED,
        codes::START_FAILED => codes::START_FAILED,
        codes::BUSY => codes::BUSY,
        codes::GONE => codes::GONE,
        crate::host_admin::codes::REQUIRED => crate::host_admin::codes::REQUIRED,
        crate::host_admin::codes::FORBIDDEN => crate::host_admin::codes::FORBIDDEN,
        // A host that answers 404/405 without one of our codes does not know the
        // terminal route at all: it runs an older daemon than this app.
        _ if matches!(status.as_u16(), 404 | 405) => codes::HOST_OUTDATED,
        // The host refused this desktop (not its paired owner, or the route is closed).
        _ if matches!(status.as_u16(), 401 | 403) => codes::NOT_PERMITTED,
        _ if matches!(status.as_u16(), 502..=504) => "host_link_failed",
        _ => codes::HOST_FAILED,
    };
    // The desktop reads a 404 as "session gone": an outdated host is a gateway problem.
    let status = if known == codes::HOST_OUTDATED {
        StatusCode::BAD_GATEWAY
    } else {
        status
    };
    ApiError::new(
        status,
        known,
        body["error"]["message"]
            .as_str()
            .unwrap_or("the host refused")
            .to_owned(),
    )
}

async fn forward(
    s: &AppState,
    peer: Option<&Extension<PeerIdentity>>,
    headers: &axum::http::HeaderMap,
    host: &str,
    method: &str,
    path: &str,
    body: Option<serde_json::Value>,
) -> Result<serde_json::Value, ApiError> {
    if peer.is_some() {
        return Err(ApiError::forbidden("host links are local only"));
    }
    s.require_owner(headers)?;
    if !valid_host(host) {
        return Err(ApiError::bad_request("invalid host id"));
    }
    let links = s
        .host_links()
        .ok_or_else(|| ApiError::unavailable("host links are not available in this daemon"))?;
    let call = links.call(host, method, path, body).await?;
    if (200..300).contains(&call.status) {
        Ok(call.body)
    } else {
        Err(host_error(call.status, &call.body))
    }
}

fn session_path(tid: &str) -> Result<String, ApiError> {
    tid.parse::<Uuid>()
        .map(|u| format!("/v1/admin/terminal/{u}"))
        .map_err(|_| ApiError::bad_request("invalid terminal id"))
}

/// `POST /v1/hosts/{id}/terminal`: open a terminal on the host (owner key).
#[utoipa::path(post, operation_id = "hosts_terminal_open", path = "/v1/hosts/{id}/terminal", tag = "host", security(("bearer" = [])),
    params(("id" = String, Path)), request_body = OpenTerminalInput,
    responses((status = 200, body = TerminalInfo), (status = 403, body = crate::error::ErrorBody)))]
pub(crate) async fn host_open(
    State(s): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    headers: axum::http::HeaderMap,
    Path(host): Path<String>,
    ApiJson(input): ApiJson<OpenTerminalInput>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let body = serde_json::json!({"cols": input.cols, "rows": input.rows});
    forward(
        &s,
        peer.as_ref(),
        &headers,
        &host,
        "POST",
        "/v1/admin/terminal",
        Some(body),
    )
    .await
    .map(Json)
}

/// `GET /v1/hosts/{id}/terminal/{tid}/output?after=&wait_ms=`.
#[utoipa::path(get, operation_id = "hosts_terminal_output", path = "/v1/hosts/{id}/terminal/{tid}/output", tag = "host", security(("bearer" = [])),
    params(("id" = String, Path), ("tid" = String, Path), ("after" = Option<u64>, Query), ("wait_ms" = Option<u64>, Query)),
    responses((status = 200, body = TerminalOutput), (status = 403, body = crate::error::ErrorBody)))]
pub(crate) async fn host_output(
    State(s): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    headers: axum::http::HeaderMap,
    Path((host, tid)): Path<(String, String)>,
    Query(q): Query<OutputQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let path = format!(
        "{}/output?after={}&wait_ms={}",
        session_path(&tid)?,
        q.after,
        q.wait_ms.min(MAX_WAIT_MS)
    );
    forward(&s, peer.as_ref(), &headers, &host, "GET", &path, None)
        .await
        .map(Json)
}

/// `POST /v1/hosts/{id}/terminal/{tid}/input`.
#[utoipa::path(post, operation_id = "hosts_terminal_input", path = "/v1/hosts/{id}/terminal/{tid}/input", tag = "host", security(("bearer" = [])),
    params(("id" = String, Path), ("tid" = String, Path)), request_body = TerminalInput,
    responses((status = 204), (status = 403, body = crate::error::ErrorBody)))]
pub(crate) async fn host_input(
    State(s): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    headers: axum::http::HeaderMap,
    Path((host, tid)): Path<(String, String)>,
    ApiJson(input): ApiJson<TerminalInput>,
) -> Result<StatusCode, ApiError> {
    let path = format!("{}/input", session_path(&tid)?);
    forward(
        &s,
        peer.as_ref(),
        &headers,
        &host,
        "POST",
        &path,
        Some(serde_json::json!({"data": input.data})),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /v1/hosts/{id}/terminal/{tid}/resize`.
#[utoipa::path(post, operation_id = "hosts_terminal_resize", path = "/v1/hosts/{id}/terminal/{tid}/resize", tag = "host", security(("bearer" = [])),
    params(("id" = String, Path), ("tid" = String, Path)), request_body = TerminalResize,
    responses((status = 204), (status = 403, body = crate::error::ErrorBody)))]
pub(crate) async fn host_resize(
    State(s): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    headers: axum::http::HeaderMap,
    Path((host, tid)): Path<(String, String)>,
    ApiJson(input): ApiJson<TerminalResize>,
) -> Result<StatusCode, ApiError> {
    let path = format!("{}/resize", session_path(&tid)?);
    forward(
        &s,
        peer.as_ref(),
        &headers,
        &host,
        "POST",
        &path,
        Some(serde_json::json!({"cols": input.cols, "rows": input.rows})),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /v1/hosts/{id}/terminal/{tid}`.
#[utoipa::path(delete, operation_id = "hosts_terminal_close", path = "/v1/hosts/{id}/terminal/{tid}", tag = "host", security(("bearer" = [])),
    params(("id" = String, Path), ("tid" = String, Path)),
    responses((status = 204), (status = 403, body = crate::error::ErrorBody)))]
pub(crate) async fn host_close(
    State(s): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    headers: axum::http::HeaderMap,
    Path((host, tid)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let path = session_path(&tid)?;
    forward(&s, peer.as_ref(), &headers, &host, "DELETE", &path, None).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_terminal_finds_the_clis_the_service_user_installed() {
        use std::ffi::OsString;
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".local/bin")).unwrap();
        // Only folders that nobody else can write are put on the `PATH`.
        #[cfg(unix)]
        let chmod = |dir: &str, mode| {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(home.path().join(dir), std::fs::Permissions::from_mode(mode))
                .unwrap();
        };
        #[cfg(unix)]
        for dir in ["", ".local", ".local/bin"] {
            chmod(dir, 0o750);
        }
        let env: Vec<(OsString, OsString)> = vec![
            ("HOME".into(), home.path().into()),
            (
                "PATH".into(),
                std::env::join_paths(["/usr/bin", "/bin"]).unwrap(),
            ),
        ];
        let env = super::session_env(env, home.path().join("data"));
        assert!(env.iter().any(|(k, _)| k == "TERM"));
        let path = env.iter().find(|(k, _)| k == "PATH").unwrap().1.clone();
        let dirs: Vec<_> = std::env::split_paths(&path).collect();
        assert_eq!(dirs[0], std::path::PathBuf::from("/usr/bin"));
        assert!(dirs.contains(&home.path().join(".local/bin")));
        // A HOME whose CLI folder others can write is session data from outside: not added.
        #[cfg(unix)]
        {
            chmod(".local/bin", 0o777);
            let env = vec![
                ("HOME".into(), home.path().into()),
                (
                    "PATH".into(),
                    std::env::join_paths(["/usr/bin", "/bin"]).unwrap(),
                ),
            ];
            let env = super::session_env(env, home.path().join("data"));
            let path = env.iter().find(|(k, _)| k == "PATH").unwrap().1.clone();
            assert!(!std::env::split_paths(&path).any(|d| d.starts_with(home.path())));
        }
        // No HOME, no change (nothing to look in).
        let mut bare: Vec<(OsString, OsString)> = vec![("PATH".into(), "/bin".into())];
        super::with_cli_install_dirs(&mut bare);
        assert_eq!(bare[0].1, OsString::from("/bin"));
    }

    use super::*;

    #[test]
    fn host_answers_are_worded_by_cause_not_as_unreachable() {
        let empty = serde_json::json!({});
        assert_eq!(host_error(404, &empty).code_str(), codes::HOST_OUTDATED);
        assert_eq!(host_error(403, &empty).code_str(), codes::NOT_PERMITTED);
        // The link or a gateway failed: the computer did not answer.
        assert_eq!(host_error(502, &empty).code_str(), "host_link_failed");
        assert_eq!(host_error(504, &empty).code_str(), "host_link_failed");
        // The computer answered with an error of its own: not "unreachable".
        assert_eq!(host_error(500, &empty).code_str(), codes::HOST_FAILED);
        assert_eq!(host_error(429, &empty).code_str(), codes::HOST_FAILED);
        let gone = serde_json::json!({"error":{"code":codes::GONE}});
        assert_eq!(host_error(404, &gone).code_str(), codes::GONE);
    }

    #[test]
    fn output_ring_keeps_the_newest_bytes_and_reports_what_was_dropped() {
        let mut out = OutBuf::default();
        out.push(b"hello ");
        out.push(b"world");
        assert_eq!(out.read_from(0), (b"hello world".to_vec(), 11, false));
        assert_eq!(out.read_from(6), (b"world".to_vec(), 11, false));
        // Past the limit the oldest bytes go and a reader that was behind is told.
        out.push(&vec![b'x'; MAX_BUFFER]);
        assert_eq!(out.start, 11);
        let (chunk, next, dropped) = out.read_from(0);
        assert!(dropped && chunk.len() == MAX_POLL_BYTES);
        assert_eq!(next, 11 + MAX_POLL_BYTES as u64);
        // Caught up: nothing to read.
        assert_eq!(out.read_from(out.end()).0, Vec::<u8>::new());
    }

    #[test]
    fn sizes_are_clamped_and_ids_are_checked_before_they_reach_the_host() {
        assert_eq!(clamp_size(0, 0), (20, 5));
        assert_eq!(clamp_size(9999, 9999), (400, 200));
        assert!(session_path("../../etc").is_err());
        assert!(session_path(&Uuid::nil().to_string()).is_ok());
        assert!(valid_host("abc123") && !valid_host("a/b") && !valid_host(""));
    }

    #[cfg(unix)]
    fn fake_setsid(tag: &str, body: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let path =
            std::env::temp_dir().join(format!("praxi-fake-setsid-{}-{tag}", std::process::id()));
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh
{body}
"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[cfg(unix)]
    #[test]
    fn probe_falls_back_when_setsid_exists_but_is_refused() {
        // The sandbox case: setsid is there, `-c` fails with EPERM.
        let refused = fake_setsid(
            "refused",
            "echo 'setsid: failed to set the controlling terminal: Operation not permitted' >&2; exit 1",
        );
        assert_eq!(pty::probe(&refused), pty::Strategy::Plain);
        assert_eq!(pty::probe("/nonexistent/setsid"), pty::Strategy::Plain);
        let fine = fake_setsid("fine", "exit 0");
        assert_eq!(pty::probe(&fine), pty::Strategy::Setsid);
    }

    /// The host bug: the daemon is a systemd service (a session leader without a controlling
    /// terminal), and a pty slave opened without `O_NOCTTY` became ITS controlling terminal, so
    /// closing the pty sent SIGHUP to the daemon (the host restarted at the first terminal). The
    /// test runs the pty code in a child that is such a session leader (`setsid -w`).
    #[cfg(target_os = "linux")]
    #[test]
    fn a_pty_never_becomes_the_controlling_terminal_of_the_daemon_that_opens_it() {
        const HELPER: &str = "PRAXI_PTY_CTTY_HELPER";
        const TEST: &str = "terminal::tests::a_pty_never_becomes_the_controlling_terminal_of_the_daemon_that_opens_it";
        if std::env::var_os(HELPER).is_some() {
            let (master, slave) = pty::open_pty(80, 24).expect("a pty");
            let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
            // `pid (comm) state ppid pgrp session tty_nr ...`
            let tty_nr: i64 = stat[stat.rfind(')').unwrap() + 1..]
                .split_whitespace()
                .nth(4)
                .unwrap()
                .parse()
                .unwrap();
            drop(slave);
            drop(master);
            // A hang-up of a controlling terminal would kill this process by now.
            std::thread::sleep(Duration::from_millis(400));
            println!("survived tty_nr={tty_nr}");
            return;
        }
        let exe = std::env::current_exe().unwrap();
        let Ok(out) = std::process::Command::new("setsid")
            .arg("-w")
            .arg(exe)
            .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
            .env(HELPER, "1")
            .output()
        else {
            return; // no util-linux setsid here
        };
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && text.contains("survived tty_nr=0"),
            "the pty became the daemon's controlling terminal ({:?}): {text}",
            out.status
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn the_strategy_probe_does_not_block_the_async_runtime() {
        static CELL: std::sync::OnceLock<pty::Strategy> = std::sync::OnceLock::new();
        let ticks = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let t = ticks.clone();
        let ticker = tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(20)).await;
                t.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        });
        // A probe that takes 500 ms on a single-threaded runtime: a blocking probe would starve
        // the ticker for the whole time.
        let s = pty::strategy_off_runtime(&CELL, || {
            std::thread::sleep(Duration::from_millis(500));
            pty::Strategy::Setsid
        })
        .await;
        ticker.abort();
        assert_eq!(s, pty::Strategy::Setsid);
        assert!(
            ticks.load(std::sync::atomic::Ordering::SeqCst) >= 5,
            "the runtime stalled during the probe"
        );
        // Remembered: a second call does not probe again.
        let again = pty::strategy_off_runtime(&CELL, || unreachable!()).await;
        assert_eq!(again, pty::Strategy::Setsid);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn plain_strategy_gives_a_working_shell() {
        use std::io::Read;
        let (pty, mut child, mut reader) =
            pty::spawn_with(pty::Strategy::Plain, "/bin/sh", 80, 24, &[], None).unwrap();
        pty.write_all(
            b"echo praxi-$((40+2))
exit
",
        )
        .unwrap();
        let status = tokio::time::timeout(std::time::Duration::from_secs(10), child.wait())
            .await
            .expect("shell ends")
            .unwrap();
        assert!(status.success());
        let mut out = Vec::new();
        let mut buf = [0u8; 1024];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }
        assert!(String::from_utf8_lossy(&out).contains("praxi-42"));
    }
}
