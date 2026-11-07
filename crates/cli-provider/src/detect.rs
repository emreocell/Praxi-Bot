use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use process_wrap::tokio::{ChildWrapper, CommandWrap, KillOnDrop};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;

use crate::runtime::CliError;

/// Authentication status reported by a CLI provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthState {
    SignedIn,
    SignedOut,
    Unknown,
}

/// A detected coding CLI engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectedCli {
    /// Engine id (`claude`, `codex`, `cursor`, `opencode`, `gemini`, `chatgpt-plan`).
    pub id: String,
    /// Display name.
    pub display_name: String,
    /// Whether the CLI binary was found on PATH and `--version` succeeded.
    pub installed: bool,
    /// CLI version (first line, trimmed, max 80 chars, or null).
    pub version: Option<String>,
    /// Authentication status (`signed_in`, `signed_out`, `unknown`).
    pub auth: AuthState,
    /// Account label (email or org name, max 160 chars, or null).
    pub account_label: Option<String>,
    /// Whether this installed engine supports turn execution.
    pub runnable: bool,
    /// Stable detail code (`not_on_path`, `timeout`, `auth_unparsed`, etc., or null).
    pub detail: Option<String>,
}

/// Hermetic PATH and filesystem environment for detection.
#[derive(Debug, Clone)]
pub struct PathEnv {
    /// Ordered list of directories to search for binaries.
    pub path_dirs: Vec<PathBuf>,
    /// Extensions to try on Windows (e.g. `.COM`, `.EXE`, `.BAT`, `.CMD`).
    pub pathext: Vec<String>,
    /// User home directory (for `~/.gemini/oauth_creds.json`).
    pub home_dir: Option<PathBuf>,
    /// `%LOCALAPPDATA%` on Windows (for `%LOCALAPPDATA%\opencode\auth.json`).
    pub local_app_data: Option<PathBuf>,
    /// `$XDG_DATA_HOME` on Unix (for `$XDG_DATA_HOME/opencode/auth.json`).
    pub xdg_data_home: Option<PathBuf>,
    /// Environment variables (for checking non-empty API keys, and test flags).
    pub env_vars: HashMap<String, String>,
}

impl Default for PathEnv {
    fn default() -> Self {
        Self::from_ambient()
    }
}

impl PathEnv {
    /// Construct from ambient OS environment.
    #[must_use]
    pub fn from_ambient() -> Self {
        let path_dirs = std::env::var_os("PATH")
            .map_or_else(Vec::new, |val| std::env::split_paths(&val).collect());

        #[cfg(windows)]
        let pathext = std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_owned())
            .split(';')
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        #[cfg(not(windows))]
        let pathext = vec![String::new()];

        let home_dir = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from);

        let local_app_data = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
        let xdg_data_home = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from);

        let env_vars = std::env::vars().collect();

        Self {
            path_dirs,
            pathext,
            home_dir,
            local_app_data,
            xdg_data_home,
            env_vars,
        }
    }

    /// Construct a sealed test environment pointing only to `bin_dir`.
    #[must_use]
    pub fn for_test(bin_dir: PathBuf) -> Self {
        Self {
            path_dirs: vec![bin_dir],
            pathext: vec![
                ".COM".to_owned(),
                ".EXE".to_owned(),
                ".BAT".to_owned(),
                ".CMD".to_owned(),
                String::new(),
            ],
            home_dir: None,
            local_app_data: None,
            xdg_data_home: None,
            env_vars: HashMap::new(),
        }
    }

    #[must_use]
    pub fn with_home(mut self, path: PathBuf) -> Self {
        self.home_dir = Some(path);
        self
    }

    #[must_use]
    pub fn with_local_app_data(mut self, path: PathBuf) -> Self {
        self.local_app_data = Some(path);
        self
    }

    #[must_use]
    pub fn with_xdg_data_home(mut self, path: PathBuf) -> Self {
        self.xdg_data_home = Some(path);
        self
    }

    #[must_use]
    pub fn with_env_var(mut self, key: impl Into<String>, val: impl Into<String>) -> Self {
        self.env_vars.insert(key.into(), val.into());
        self
    }
}

/// Resolved executable invocation.
#[derive(Debug, Clone)]
pub struct ResolvedCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShimError {
    Refused,
}

/// Look up a command word on `PATH` using `env.pathext`.
/// Rejects names containing a newline or path separator.
#[must_use]
pub fn find_candidate(argv0: &str, env: &PathEnv) -> Option<PathBuf> {
    if argv0.is_empty()
        || argv0.contains('\n')
        || argv0.contains('\r')
        || argv0.contains('/')
        || argv0.contains('\\')
    {
        return None;
    }

    for dir in &env.path_dirs {
        for ext in &env.pathext {
            let candidate = if ext.is_empty() {
                dir.join(argv0)
            } else {
                dir.join(format!("{argv0}{ext}"))
            };
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        let direct = dir.join(argv0);
        if direct.is_file() {
            return Some(direct);
        }
    }
    None
}

fn is_batch_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat"))
}

/// Resolve a candidate executable, refusing unsafe `.cmd`/`.bat` shims.
///
/// # Errors
/// Returns [`ShimError::Refused`] if the candidate is a Windows `.cmd` or `.bat` shim
/// that cannot be safely unwrapped without executing `cmd.exe`.
pub fn resolve_spawn(candidate: &Path, env: &PathEnv) -> Result<ResolvedCommand, ShimError> {
    if !is_batch_file(candidate) {
        return Ok(ResolvedCommand {
            program: candidate.to_path_buf(),
            args: Vec::new(),
        });
    }

    // Check if the shim can be unwrapped to node + js script
    if let Ok(text) = std::fs::read_to_string(candidate) {
        let dir = candidate.parent().unwrap_or_else(|| Path::new("."));
        if let Some(target) = parse_npm_shim_target(&text) {
            let script_path = dir.join(&target);
            if script_path.is_file() {
                let node = find_node_executable(dir, env);
                if let Some(node) = node {
                    return Ok(ResolvedCommand {
                        program: node,
                        args: vec![script_path.to_string_lossy().into_owned()],
                    });
                }
            }
            return Err(ShimError::Refused);
        }
        // Native installers also ship a batch wrapper around a sibling .exe.
        // Trust only a literal target spelled in the shim, never shell expansion.
        for line in text.lines() {
            for prefix in ["\"%~dp0\\", "\"%dp0%\\", "\"%~dp0"] {
                if let Some((_, rest)) = line.split_once(prefix)
                    && let Some((target, _)) = rest.split_once('"')
                    && !target.contains(['/', '\\', '%', ':'])
                    && Path::new(target)
                        .extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("exe"))
                    && dir.join(target).is_file()
                {
                    return Ok(ResolvedCommand {
                        program: dir.join(target),
                        args: Vec::new(),
                    });
                }
            }
        }
    }

    // Refuse unwrapping rather than passing to cmd.exe
    Err(ShimError::Refused)
}

fn is_js_extension(target: &str) -> bool {
    let p = Path::new(target);
    p.extension().is_some_and(|ext| {
        ext.eq_ignore_ascii_case("js")
            || ext.eq_ignore_ascii_case("cjs")
            || ext.eq_ignore_ascii_case("mjs")
    })
}

fn parse_npm_shim_target(text: &str) -> Option<String> {
    for prefix in ["\"%~dp0\\", "\"%dp0%\\", "\"%~dp0"] {
        for (idx, _) in text.match_indices(prefix) {
            if let Some((target, _)) = text[idx + prefix.len()..].split_once('"')
                && !target.contains(['%', '\n', '\r', ':'])
                && is_js_extension(target)
            {
                return Some(target.to_owned());
            }
        }
    }
    None
}

fn find_node_executable(near: &Path, env: &PathEnv) -> Option<PathBuf> {
    let local = near.join("node.exe");
    if local.is_file() {
        return Some(local);
    }
    // `node.cmd` / `node.bat` would be re-parsed by `cmd.exe` together with the
    // script path. Refuse those instead of spawning them.
    let found = find_candidate("node", env)?;
    if is_batch_file(&found) {
        return None;
    }
    Some(found)
}

#[derive(Debug)]
enum ProbeError {
    Timeout,
    OutputTooLarge,
    Io,
    SpawnFailed,
}

const MAX_OUTPUT_BYTES: usize = 64 * 1024; // 64 KiB
const READ_CHUNK: usize = 4096;
const PROBE_TIMEOUT: Duration = Duration::from_secs(8);

/// Read until EOF, but stop as soon as another chunk would pass `limit`.
///
/// The check happens before the chunk is stored. Callers must not buffer the
/// remainder with `read_to_end` and only then compare lengths.
async fn read_capped<R>(reader: &mut R, buf: &mut Vec<u8>, limit: usize) -> Result<(), ProbeError>
where
    R: AsyncReadExt + Unpin,
{
    let mut chunk = [0u8; READ_CHUNK];
    loop {
        let n = reader.read(&mut chunk).await.map_err(|_| ProbeError::Io)?;
        if n == 0 {
            return Ok(());
        }
        if buf.len().saturating_add(n) > limit {
            return Err(ProbeError::OutputTooLarge);
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

/// Drop inherited environment, including `KEY` / `TOKEN` / `SECRET` / `CREDENTIAL`.
///
/// `PATH` and the Windows system-root variables stay so `taskkill` / `kill` can
/// still be resolved and can start. Nothing else is copied from the parent.
fn scrub_process_env(cmd: &mut tokio::process::Command) {
    cmd.env_clear();
    // Names only. None of these contain KEY, TOKEN, SECRET, or CREDENTIAL.
    // PATH/PATHEXT stay so a relative program name can still be resolved after
    // `env_clear` (Rust searches the child's PATH in that case).
    for key in ["SYSTEMROOT", "WINDIR", "PATH", "PATHEXT"] {
        if let Some(val) = std::env::var_os(key) {
            cmd.env(key, val);
        }
    }
}

fn tree_kill_program() -> PathBuf {
    #[cfg(windows)]
    {
        std::env::var_os("SYSTEMROOT")
            .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from)
            .join("System32")
            .join("taskkill.exe")
    }
    #[cfg(not(windows))]
    {
        let bin = PathBuf::from("/bin/kill");
        if bin.is_file() {
            bin
        } else {
            PathBuf::from("kill")
        }
    }
}

pub(crate) async fn kill_tree(child: &mut dyn ChildWrapper) {
    if let Some(pid) = child.id() {
        let mut cmd = tokio::process::Command::new(tree_kill_program());
        #[cfg(windows)]
        cmd.args(["/F", "/T", "/PID", &pid.to_string()]);
        #[cfg(not(windows))]
        cmd.args(["-9", &format!("-{pid}")]);
        scrub_process_env(&mut cmd);
        let _ = cmd.output().await;
    }
    let _ = child.start_kill();
    let _ = child.wait().await;
}

/// The same isolation for probes and sessions: groups on Unix, kill-on-close
/// jobs on Windows (including when the parent runtime is torn down).
pub(crate) fn isolate(cmd: tokio::process::Command) -> CommandWrap {
    let mut wrap = CommandWrap::from(cmd);
    wrap.wrap(KillOnDrop);
    #[cfg(unix)]
    wrap.wrap(process_wrap::tokio::ProcessGroup::leader());
    #[cfg(windows)]
    {
        wrap.wrap(process_wrap::tokio::CreationFlags(
            windows::Win32::System::Threading::CREATE_NO_WINDOW,
        ));
        wrap.wrap(process_wrap::tokio::JobObject);
    }
    wrap
}

pub(crate) fn apply_sanitized_env(cmd: &mut tokio::process::Command, env: &PathEnv) {
    cmd.env_clear();

    let path_val = std::env::join_paths(&env.path_dirs).unwrap_or_default();
    cmd.env("PATH", path_val);

    let safe_passthrough = [
        "SYSTEMROOT",
        "WINDIR",
        "USERPROFILE",
        "HOME",
        "LOCALAPPDATA",
        "APPDATA",
        "TEMP",
        "TMP",
        "TMPDIR",
        "PATHEXT",
        "COMSPEC",
    ];

    for key in safe_passthrough {
        if let Some(val) = env.env_vars.get(key) {
            cmd.env(key, val);
        } else if let Ok(val) = std::env::var(key) {
            cmd.env(key, val);
        }
    }

    // Preserve the user's non-secret environment, including CLI home overrides.
    for (k, v) in &env.env_vars {
        let upper = k.to_ascii_uppercase();
        if upper == "PATH"
            || upper.contains("KEY")
            || upper.contains("TOKEN")
            || upper.contains("SECRET")
            || upper.contains("CREDENTIAL")
        {
            continue;
        }
        cmd.env(k, v);
    }
}

async fn run_process_probe(
    resolved: &ResolvedCommand,
    args: &[&str],
    env: &PathEnv,
) -> Result<(std::process::ExitStatus, String, String), ProbeError> {
    // Last line of defence: Rust runs `.cmd`/`.bat` through `cmd.exe /c` and
    // appends the arguments. Never start those.
    if is_batch_file(&resolved.program) {
        return Err(ProbeError::SpawnFailed);
    }

    let mut cmd = tokio::process::Command::new(&resolved.program);
    cmd.args(&resolved.args);
    cmd.args(args);
    cmd.stdin(std::process::Stdio::null()).kill_on_drop(true);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    apply_sanitized_env(&mut cmd, env);

    #[cfg(unix)]
    {
        cmd.process_group(0);
    }

    let mut child = isolate(cmd).spawn().map_err(|_| ProbeError::SpawnFailed)?;
    let mut stdout = child.stdout().take().expect("piped");
    let mut stderr = child.stderr().take().expect("piped");

    let read_fut = async {
        let mut stdout_buf = Vec::new();
        let mut stderr_buf = Vec::new();
        let mut stdout_chunk = [0u8; READ_CHUNK];
        let mut stderr_chunk = [0u8; READ_CHUNK];
        let mut stdout_done = false;
        let mut stderr_done = false;

        loop {
            tokio::select! {
                n = stdout.read(&mut stdout_chunk), if !stdout_done => {
                    let n = n.map_err(|_| ProbeError::Io)?;
                    if n == 0 {
                        stdout_done = true;
                    } else if stdout_buf.len() + n > MAX_OUTPUT_BYTES {
                        return Err(ProbeError::OutputTooLarge);
                    } else {
                        stdout_buf.extend_from_slice(&stdout_chunk[..n]);
                    }
                }
                n = stderr.read(&mut stderr_chunk), if !stderr_done => {
                    let n = n.map_err(|_| ProbeError::Io)?;
                    if n == 0 {
                        stderr_done = true;
                    } else if stderr_buf.len() + n > MAX_OUTPUT_BYTES {
                        return Err(ProbeError::OutputTooLarge);
                    } else {
                        stderr_buf.extend_from_slice(&stderr_chunk[..n]);
                    }
                }
                status = child.wait() => {
                    let status = status.map_err(|_| ProbeError::Io)?;
                    if !stdout_done {
                        read_capped(&mut stdout, &mut stdout_buf, MAX_OUTPUT_BYTES).await?;
                    }
                    if !stderr_done {
                        read_capped(&mut stderr, &mut stderr_buf, MAX_OUTPUT_BYTES).await?;
                    }
                    return Ok((
                        status,
                        String::from_utf8_lossy(&stdout_buf).into_owned(),
                        String::from_utf8_lossy(&stderr_buf).into_owned(),
                    ));
                }
            }
        }
    };

    match tokio::time::timeout(PROBE_TIMEOUT, read_fut).await {
        Ok(Ok(res)) => Ok(res),
        Ok(Err(ProbeError::OutputTooLarge)) => {
            kill_tree(child.as_mut()).await;
            Err(ProbeError::OutputTooLarge)
        }
        Ok(Err(e)) => {
            kill_tree(child.as_mut()).await;
            Err(e)
        }
        Err(_) => {
            kill_tree(child.as_mut()).await;
            Err(ProbeError::Timeout)
        }
    }
}

/// Probe a version without opening login files or exposing raw diagnostics.
pub(crate) async fn claude_version(
    resolved: &ResolvedCommand,
    env: &PathEnv,
) -> Option<(u32, u32, u32)> {
    let Ok((status, stdout, _)) = run_process_probe(resolved, &["--version"], env).await else {
        return None;
    };
    if !status.success() {
        return None;
    }
    stdout.split_whitespace().find_map(|part| {
        let parts: Vec<u32> = part
            .split('.')
            .map(str::parse)
            .collect::<Result<_, _>>()
            .ok()?;
        (parts.len() == 3).then(|| (parts[0], parts[1], parts[2]))
    })
}

/// Probe Codex version without opening login files or exposing raw diagnostics.
pub(crate) async fn codex_version(
    resolved: &ResolvedCommand,
    env: &PathEnv,
) -> Result<Option<(u32, u32, u32)>, CliError> {
    let Ok((status, stdout, _)) = run_process_probe(resolved, &["--version"], env).await else {
        return Err(CliError::NotInstalled);
    };
    if !status.success() {
        return Err(CliError::NotInstalled);
    }
    Ok(stdout.split_whitespace().find_map(|part| {
        let parts: Vec<u32> = part
            .split('.')
            .map(str::parse)
            .collect::<Result<_, _>>()
            .ok()?;
        (parts.len() == 3).then(|| (parts[0], parts[1], parts[2]))
    }))
}

fn parse_version_string(stdout: &str) -> Option<String> {
    let first_line = stdout.lines().next()?.trim();
    if first_line.is_empty() {
        return None;
    }
    let bounded: String = first_line.chars().take(80).collect();
    Some(bounded)
}

fn bound_label(s: &str, max_len: usize) -> Option<String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    let bounded: String = trimmed.chars().take(max_len).collect();
    Some(bounded)
}

/// Coding CLI detector with an in-memory cache.
pub struct Detector {
    env: PathEnv,
    cache: tokio::sync::Mutex<Option<(Instant, Vec<DetectedCli>)>>,
}

impl Default for Detector {
    fn default() -> Self {
        Self::new()
    }
}

impl Detector {
    /// Create a detector using the ambient environment.
    #[must_use]
    pub fn new() -> Self {
        Self::with_env(PathEnv::from_ambient())
    }

    /// Create a detector with a custom `PathEnv` (for hermetic tests).
    #[must_use]
    pub fn with_env(env: PathEnv) -> Self {
        Self {
            env,
            cache: tokio::sync::Mutex::new(None),
        }
    }

    /// Detect installed, signed-in coding CLIs.
    ///
    /// Caches successful probes for 60 seconds unless `refresh` is true.
    pub async fn detect(&self, refresh: bool) -> Vec<DetectedCli> {
        if !refresh {
            let guard = self.cache.lock().await;
            let valid_cache = guard
                .as_ref()
                .filter(|(t, _)| t.elapsed() < Duration::from_secs(60));
            if let Some((_, cached)) = valid_cache {
                return cached.clone();
            }
        }

        let fresh = Box::pin(self.detect_fresh()).await;

        let mut guard = self.cache.lock().await;
        *guard = Some((Instant::now(), fresh.clone()));
        fresh
    }

    async fn detect_fresh(&self) -> Vec<DetectedCli> {
        let (claude, codex, cursor, opencode, gemini) = tokio::join!(
            self.probe_claude(),
            self.probe_codex(),
            self.probe_cursor(),
            self.probe_opencode(),
            self.probe_gemini(),
        );

        let chatgpt_plan = DetectedCli {
            id: "chatgpt-plan".to_owned(),
            display_name: "ChatGPT plan".to_owned(),
            installed: codex.installed,
            version: codex.version.clone(),
            auth: AuthState::Unknown,
            account_label: None,
            runnable: false,
            detail: codex.detail.clone(),
        };

        vec![claude, codex, cursor, opencode, gemini, chatgpt_plan]
    }

    #[allow(clippy::too_many_lines)]
    async fn probe_engine_version(
        &self,
        id: &str,
        display_name: &str,
        argv0: &str,
    ) -> (DetectedCli, Option<ResolvedCommand>) {
        let not_found = DetectedCli {
            id: id.to_owned(),
            display_name: display_name.to_owned(),
            installed: false,
            version: None,
            auth: AuthState::Unknown,
            account_label: None,
            runnable: false,
            detail: Some("not_on_path".to_owned()),
        };

        let Some(candidate) = find_candidate(argv0, &self.env) else {
            return (not_found, None);
        };

        let resolved = match resolve_spawn(&candidate, &self.env) {
            Ok(cmd) => cmd,
            Err(ShimError::Refused) => {
                return (
                    DetectedCli {
                        id: id.to_owned(),
                        display_name: display_name.to_owned(),
                        installed: true,
                        version: None,
                        auth: AuthState::Unknown,
                        account_label: None,
                        runnable: false,
                        detail: Some("shim_refused".to_owned()),
                    },
                    None,
                );
            }
        };

        match run_process_probe(&resolved, &["--version"], &self.env).await {
            Ok((status, stdout, _stderr)) => {
                if status.success() {
                    let version = parse_version_string(&stdout);
                    (
                        DetectedCli {
                            id: id.to_owned(),
                            display_name: display_name.to_owned(),
                            installed: true,
                            version,
                            auth: AuthState::Unknown,
                            account_label: None,
                            runnable: matches!(id, "claude" | "codex"),
                            detail: None,
                        },
                        Some(resolved),
                    )
                } else {
                    // Non-zero exit on --version means not installed (broken shim), not signed out
                    (
                        DetectedCli {
                            id: id.to_owned(),
                            display_name: display_name.to_owned(),
                            installed: false,
                            version: None,
                            auth: AuthState::Unknown,
                            account_label: None,
                            runnable: false,
                            detail: Some("broken_shim".to_owned()),
                        },
                        None,
                    )
                }
            }
            Err(ProbeError::Timeout) => (
                DetectedCli {
                    id: id.to_owned(),
                    display_name: display_name.to_owned(),
                    installed: true,
                    version: None,
                    auth: AuthState::Unknown,
                    account_label: None,
                    runnable: false,
                    detail: Some("timeout".to_owned()),
                },
                None,
            ),
            Err(ProbeError::OutputTooLarge) => (
                DetectedCli {
                    id: id.to_owned(),
                    display_name: display_name.to_owned(),
                    installed: true,
                    version: None,
                    auth: AuthState::Unknown,
                    account_label: None,
                    runnable: false,
                    detail: Some("output_too_large".to_owned()),
                },
                None,
            ),
            Err(ProbeError::SpawnFailed | ProbeError::Io) => (
                DetectedCli {
                    id: id.to_owned(),
                    display_name: display_name.to_owned(),
                    installed: false,
                    version: None,
                    auth: AuthState::Unknown,
                    account_label: None,
                    runnable: false,
                    detail: Some("spawn_failed".to_owned()),
                },
                None,
            ),
        }
    }

    async fn probe_claude(&self) -> DetectedCli {
        let (mut cli, resolved) = self
            .probe_engine_version("claude", "Claude Code", "claude")
            .await;
        if !cli.installed || resolved.is_none() || cli.detail.is_some() {
            return cli;
        }
        let resolved = resolved.unwrap();

        match run_process_probe(&resolved, &["auth", "status", "--json"], &self.env).await {
            Ok((_status, stdout, _stderr)) => {
                #[derive(Deserialize)]
                struct ClaudeAuth {
                    #[serde(rename = "loggedIn")]
                    logged_in: Option<bool>,
                    email: Option<String>,
                    #[serde(rename = "orgName")]
                    org_name: Option<String>,
                    #[serde(rename = "authMethod", default)]
                    _auth_method: Option<String>,
                }

                if let Ok(parsed) = serde_json::from_str::<ClaudeAuth>(&stdout) {
                    if parsed.logged_in == Some(true) {
                        cli.auth = AuthState::SignedIn;
                        cli.account_label = parsed
                            .email
                            .as_deref()
                            .and_then(|e| bound_label(e, 160))
                            .or_else(|| {
                                parsed.org_name.as_deref().and_then(|o| bound_label(o, 160))
                            });
                        cli.detail = None;
                    } else if parsed.logged_in == Some(false) {
                        cli.auth = AuthState::SignedOut;
                        cli.account_label = None;
                        cli.detail = None;
                    } else {
                        cli.auth = AuthState::Unknown;
                        cli.detail = Some("auth_unparsed".to_owned());
                    }
                } else {
                    cli.auth = AuthState::Unknown;
                    cli.detail = Some("auth_unparsed".to_owned());
                }
            }
            Err(ProbeError::Timeout) => {
                cli.auth = AuthState::Unknown;
                cli.detail = Some("timeout".to_owned());
            }
            Err(ProbeError::OutputTooLarge) => {
                cli.auth = AuthState::Unknown;
                cli.detail = Some("output_too_large".to_owned());
            }
            Err(ProbeError::SpawnFailed | ProbeError::Io) => {
                cli.auth = AuthState::Unknown;
                cli.detail = Some("auth_unparsed".to_owned());
            }
        }

        cli
    }

    async fn probe_codex(&self) -> DetectedCli {
        let (mut cli, resolved) = self.probe_engine_version("codex", "Codex", "codex").await;
        if !cli.installed || resolved.is_none() || cli.detail.is_some() {
            return cli;
        }
        let resolved = resolved.unwrap();

        match run_process_probe(&resolved, &["login", "status"], &self.env).await {
            Ok((_status, stdout, stderr)) => {
                let first_line = stdout
                    .lines()
                    .next()
                    .filter(|l| !l.trim().is_empty())
                    .or_else(|| stderr.lines().next().filter(|l| !l.trim().is_empty()))
                    .unwrap_or("")
                    .trim();

                let lower = first_line.to_ascii_lowercase();
                if lower.starts_with("logged in") {
                    cli.auth = AuthState::SignedIn;
                    cli.detail = None;
                } else if lower.starts_with("not logged in")
                    || lower.starts_with("error checking login status")
                {
                    cli.auth = AuthState::SignedOut;
                    cli.detail = None;
                } else {
                    cli.auth = AuthState::Unknown;
                    cli.detail = Some("auth_unparsed".to_owned());
                }
            }
            Err(ProbeError::Timeout) => {
                cli.auth = AuthState::Unknown;
                cli.detail = Some("timeout".to_owned());
            }
            Err(ProbeError::OutputTooLarge) => {
                cli.auth = AuthState::Unknown;
                cli.detail = Some("output_too_large".to_owned());
            }
            Err(ProbeError::SpawnFailed | ProbeError::Io) => {
                cli.auth = AuthState::Unknown;
                cli.detail = Some("auth_unparsed".to_owned());
            }
        }

        cli
    }

    async fn probe_cursor(&self) -> DetectedCli {
        let (mut cli, resolved) = self
            .probe_engine_version("cursor", "Cursor", "cursor-agent")
            .await;
        if !cli.installed || resolved.is_none() || cli.detail.is_some() {
            return cli;
        }
        let resolved = resolved.unwrap();

        // 1. Try `status --format json`
        match run_process_probe(&resolved, &["status", "--format", "json"], &self.env).await {
            Ok((_status, stdout, _stderr)) => {
                if let Some(auth_bool) = parse_cursor_json(&stdout) {
                    cli.auth = if auth_bool {
                        AuthState::SignedIn
                    } else {
                        AuthState::SignedOut
                    };
                    cli.detail = None;
                    return cli;
                }
            }
            Err(ProbeError::Timeout) => {
                cli.auth = AuthState::Unknown;
                cli.detail = Some("timeout".to_owned());
                return cli;
            }
            Err(ProbeError::OutputTooLarge) => {
                cli.auth = AuthState::Unknown;
                cli.detail = Some("output_too_large".to_owned());
                return cli;
            }
            Err(_) => {}
        }

        // 2. Fall back to plain `status`
        match run_process_probe(&resolved, &["status"], &self.env).await {
            Ok((_status, stdout, stderr)) => {
                let combined = format!("{stdout}\n{stderr}").to_ascii_lowercase();
                if combined.contains("login successful")
                    || combined.contains("logged in")
                    || combined.contains("signed in")
                    || combined.contains("authenticated")
                {
                    cli.auth = AuthState::SignedIn;
                    cli.detail = None;
                } else if combined.contains("not logged in")
                    || combined.contains("not signed in")
                    || combined.contains("not authenticated")
                    || combined.contains("unauthenticated")
                    || combined.contains("logged out")
                {
                    cli.auth = AuthState::SignedOut;
                    cli.detail = None;
                } else {
                    cli.auth = AuthState::Unknown;
                    cli.detail = Some("auth_unparsed".to_owned());
                }
            }
            Err(ProbeError::Timeout) => {
                cli.auth = AuthState::Unknown;
                cli.detail = Some("timeout".to_owned());
            }
            Err(ProbeError::OutputTooLarge) => {
                cli.auth = AuthState::Unknown;
                cli.detail = Some("output_too_large".to_owned());
            }
            Err(ProbeError::SpawnFailed | ProbeError::Io) => {
                cli.auth = AuthState::Unknown;
                cli.detail = Some("auth_unparsed".to_owned());
            }
        }

        cli
    }

    async fn probe_opencode(&self) -> DetectedCli {
        let (mut cli, resolved) = self
            .probe_engine_version("opencode", "OpenCode", "opencode")
            .await;
        if !cli.installed || resolved.is_none() || cli.detail.is_some() {
            return cli;
        }

        // Check non-empty OPENCODE_API_KEY in probe env
        if self
            .env
            .env_vars
            .get("OPENCODE_API_KEY")
            .is_some_and(|v| !v.trim().is_empty())
        {
            cli.auth = AuthState::SignedIn;
            cli.detail = None;
            return cli;
        }

        // Do not read auth.json. Stat known paths only.
        let mut paths: Vec<PathBuf> = Vec::new();
        if let Some(xdg) = &self.env.xdg_data_home {
            paths.push(xdg.join("opencode").join("auth.json"));
        }
        if let Some(local_app) = &self.env.local_app_data {
            paths.push(local_app.join("opencode").join("auth.json"));
        }
        if let Some(home) = &self.env.home_dir {
            paths.push(
                home.join(".local")
                    .join("share")
                    .join("opencode")
                    .join("auth.json"),
            );
            #[cfg(target_os = "macos")]
            paths.push(
                home.join("Library")
                    .join("Application Support")
                    .join("opencode")
                    .join("auth.json"),
            );
        }

        let mut file_found = false;
        for path in paths {
            // STAT only: size + mtime, NEVER read bytes!
            if std::fs::metadata(&path).is_ok_and(|meta| meta.is_file()) {
                file_found = true;
                break;
            }
        }

        cli.auth = if file_found {
            AuthState::Unknown
        } else {
            AuthState::SignedOut
        };
        cli.detail = None;

        cli
    }

    async fn probe_gemini(&self) -> DetectedCli {
        let (mut cli, resolved) = self
            .probe_engine_version("gemini", "Gemini CLI", "gemini")
            .await;
        if !cli.installed || resolved.is_none() || cli.detail.is_some() {
            return cli;
        }

        // Check non-empty GEMINI_API_KEY or GOOGLE_API_KEY in probe env
        let has_key = self
            .env
            .env_vars
            .get("GEMINI_API_KEY")
            .is_some_and(|v| !v.trim().is_empty())
            || self
                .env
                .env_vars
                .get("GOOGLE_API_KEY")
                .is_some_and(|v| !v.trim().is_empty());

        if has_key {
            cli.auth = AuthState::SignedIn;
            cli.detail = None;
            return cli;
        }

        // Check exists ~/.gemini/oauth_creds.json (exists check only, no read)
        let exists = self
            .env
            .home_dir
            .as_ref()
            .map(|h| h.join(".gemini").join("oauth_creds.json"))
            .is_some_and(|p| std::fs::metadata(&p).is_ok_and(|m| m.is_file()));

        cli.auth = if exists {
            AuthState::Unknown
        } else {
            AuthState::SignedOut
        };
        cli.detail = None;

        cli
    }
}

fn truthy_auth_flag(val: &serde_json::Value) -> Option<bool> {
    match val {
        serde_json::Value::Bool(b) => Some(*b),
        serde_json::Value::String(s) => {
            let norm = s.trim().to_ascii_lowercase();
            match norm.as_str() {
                "true" | "authenticated" | "logged_in" | "logged-in" | "yes" => Some(true),
                "false" | "unauthenticated" | "logged_out" | "logged-out" | "no" => Some(false),
                _ => None,
            }
        }
        _ => None,
    }
}

fn parse_cursor_json(stdout: &str) -> Option<bool> {
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).ok()?;
    let obj = parsed.as_object()?;

    let candidate_keys = [
        "isAuthenticated",
        "authenticated",
        "loggedIn",
        "logged_in",
        "status",
    ];
    for k in candidate_keys {
        if let Some(flag) = obj.get(k).and_then(truthy_auth_flag) {
            return Some(flag);
        }
    }

    if let Some(auth_val) = obj.get("auth").and_then(serde_json::Value::as_object) {
        for k in candidate_keys {
            if let Some(flag) = auth_val.get(k).and_then(truthy_auth_flag) {
                return Some(flag);
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::{Context, Poll};
    use tokio::io::{AsyncRead, ReadBuf};

    struct ChunkReader {
        left: usize,
        taken: std::sync::Arc<AtomicUsize>,
    }

    impl AsyncRead for ChunkReader {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            if self.left == 0 {
                return Poll::Ready(Ok(()));
            }
            let n = READ_CHUNK.min(self.left).min(buf.remaining());
            let bytes = [b'A'; READ_CHUNK];
            buf.put_slice(&bytes[..n]);
            self.left -= n;
            self.taken.fetch_add(n, Ordering::SeqCst);
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn output_cap_is_enforced_while_reading() {
        let taken = std::sync::Arc::new(AtomicUsize::new(0));
        let mut reader = ChunkReader {
            left: MAX_OUTPUT_BYTES * 4,
            taken: std::sync::Arc::clone(&taken),
        };
        let mut buf = Vec::new();
        let err = read_capped(&mut reader, &mut buf, MAX_OUTPUT_BYTES)
            .await
            .unwrap_err();
        assert!(matches!(err, ProbeError::OutputTooLarge));
        assert!(buf.len() <= MAX_OUTPUT_BYTES);
        let n = taken.load(Ordering::SeqCst);
        assert!(
            n <= MAX_OUTPUT_BYTES + READ_CHUNK,
            "reader yielded {n} bytes; the cap must stop the read, not run after it"
        );
    }

    #[test]
    fn node_cmd_is_refused_and_node_exe_is_used() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("cli.js"), "").unwrap();
        std::fs::write(root.join("tool.cmd"), "\"%~dp0\\cli.js\"\r\n").unwrap();
        std::fs::write(root.join("node.cmd"), "@echo off\r\n").unwrap();
        let env = PathEnv::for_test(root.to_path_buf());
        let candidate = find_candidate("tool", &env).unwrap();
        assert!(matches!(
            resolve_spawn(&candidate, &env),
            Err(ShimError::Refused)
        ));

        std::fs::write(root.join("node.exe"), "").unwrap();
        let resolved = resolve_spawn(&candidate, &env).unwrap();
        assert_eq!(
            resolved.program.file_name().and_then(|n| n.to_str()),
            Some("node.exe")
        );
        assert!(resolved.args.iter().any(|arg| arg.ends_with("cli.js")));
    }

    #[tokio::test]
    async fn scrubbed_command_does_not_inherit_secret_env() {
        #[cfg(windows)]
        let program = std::env::var_os("SYSTEMROOT")
            .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from)
            .join("System32")
            .join("cmd.exe");
        #[cfg(windows)]
        let args = ["/d", "/c", "set"];
        #[cfg(not(windows))]
        let program = PathBuf::from("/usr/bin/env");
        #[cfg(not(windows))]
        let args: [&str; 0] = [];

        let mut cmd = tokio::process::Command::new(&program);
        cmd.args(args);
        cmd.env("API_KEY", "SENTINEL_KEY_DO_NOT_LEAK");
        cmd.env("ACCESS_TOKEN", "SENTINEL_TOKEN_DO_NOT_LEAK");
        cmd.env("DB_SECRET", "SENTINEL_SECRET_DO_NOT_LEAK");
        cmd.env("USER_CREDENTIAL", "SENTINEL_CREDENTIAL_DO_NOT_LEAK");
        scrub_process_env(&mut cmd);
        let out = cmd.output().await.expect("scrubbed command");
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            !text.contains("SENTINEL_"),
            "kill helper environment still contains a secret:\n{text}"
        );
    }
}
