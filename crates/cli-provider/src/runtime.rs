//! Per-turn CLI sessions. Wire dialects are locked by `tests/fixtures/runtime_cli.rs`.
//! No credential files are opened here. Prompts and results only cross stdin/MCP.
#[cfg(not(windows))]
use std::sync::atomic::{AtomicU32, Ordering};
use std::{
    io::Write,
    net::SocketAddr,
    process::Stdio,
    sync::{Arc, OnceLock},
};

use async_trait::async_trait;
use axum::{
    Json, Router,
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode},
    routing::post,
};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio_util::sync::CancellationToken;
use tool_registry::ToolSpec;

use crate::detect::{
    PathEnv, apply_sanitized_env, binary_pin, find_candidate, isolate, kill_tree, resolve_spawn,
};

/// Supported session dialect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    Claude,
    Codex,
}

/// Stable, non-retryable CLI run errors. Raw child output is never an error message.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CliError {
    #[error("not_installed")]
    NotInstalled,
    #[error("shim_refused")]
    ShimRefused,
    #[error("native_tools_open")]
    NativeToolsOpen,
    #[error("cancelled")]
    Cancelled,
    #[error("quota_exceeded (subscription; non-retryable)")]
    Quota,
    #[error("cli_protocol_error")]
    Protocol,
    #[error("cli_io_error")]
    Io,
    /// The installed Codex still has `unified_exec`, so the turn is not started.
    #[error("The installed Codex cannot disable unified_exec, so Praxi will not run it.")]
    CodexUnconfinable,
    /// The file at the exact path that would be spawned changed between probes.
    #[error("binary_changed")]
    BinaryChanged,
}
impl From<std::io::Error> for CliError {
    fn from(_: std::io::Error) -> Self {
        Self::Io
    }
}

/// Result in the MCP tools/call format.
#[derive(Debug, Clone)]
pub struct ToolReply {
    pub text: String,
    pub is_error: bool,
}
impl ToolReply {
    fn wire(&self) -> Value {
        json!({"content":[{"type":"text","text":self.text}],"isError":self.is_error})
    }
}

/// Host boundary: implementations use the agent's existing permission resolver and executor.
#[async_trait]
pub trait TurnHost: Send + Sync + 'static {
    fn text(&self, text: &str);
    async fn call(&self, name: &str, input: Value) -> ToolReply;
    async fn approve(&self, name: &str, input: Value) -> bool;
}

/// Session input. Neither prompt is ever used to build arguments.
pub struct Turn {
    pub prompt: String,
    pub system: String,
    pub model: Option<String>,
    pub tools: Vec<ToolSpec>,
}

/// Subscription token accounting, when supplied by the child.
#[derive(Debug, Default)]
pub struct TurnUsage {
    pub reported: bool,
    pub input: u64,
    pub output: u64,
    pub cached: Option<u64>,
}

/// A configured CLI. Constructing it does no I/O and spawns nothing.
pub struct CliEngine {
    pub engine: Engine,
    pub command: String,
    pub env: PathEnv,
}

struct CancelOnDrop(CancellationToken);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

impl CliEngine {
    /// Run one session, killing the process tree even if this future is dropped.
    ///
    /// # Errors
    /// Stable [`CliError`] codes; CLI quotas are never retried.
    pub async fn run_turn(
        &self,
        turn: Turn,
        host: Arc<dyn TurnHost>,
        cancel: CancellationToken,
    ) -> Result<TurnUsage, CliError> {
        let stop = cancel.child_token();
        let _guard = CancelOnDrop(stop.clone());
        let engine = self.engine;
        let command = self.command.clone();
        let env = self.env.clone();
        tokio::spawn(async move { session(engine, &command, &env, turn, host, stop).await })
            .await
            .map_err(|_| CliError::Io)?
    }
}

/// Reversible, collision-free MCP name encoding (including dots, colons and slashes).
#[must_use]
pub fn tool_name(name: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::from("t_");
    for b in name.bytes() {
        write!(&mut out, "{b:02x}").expect("string write");
    }
    out
}

#[derive(Clone)]
struct Bridge {
    capability: String,
    tools: Arc<Vec<ToolSpec>>,
    host: Arc<dyn TurnHost>,
    stop: CancellationToken,
    /// `0` until the turn's CLI process exists. Connections are refused until then.
    #[cfg(not(windows))]
    child_pid: Arc<AtomicU32>,
    /// Kill-on-close job created by `isolate` for this turn. Empty until spawn.
    #[cfg(windows)]
    job: crate::detect::SharedJob,
    listen: Arc<OnceLock<SocketAddr>>,
}

impl Bridge {
    fn resolve(&self, wire: &str) -> Option<&str> {
        let wire = wire.strip_prefix("mcp__praxi__").unwrap_or(wire);
        self.tools
            .iter()
            .find(|s| tool_name(&s.name) == wire)
            .map(|s| s.name.as_str())
    }

    async fn rpc(&self, req: &Value) -> Value {
        let id = &req["id"];
        let result = match req["method"].as_str().unwrap_or("") {
            "initialize" => {
                json!({"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"praxi","version":"0.1.0"}})
            }
            "ping" => json!({}),
            "tools/list" => {
                json!({"tools":self.tools.iter().map(|s| json!({"name":tool_name(&s.name),"description":s.description,"inputSchema":s.input_schema})).collect::<Vec<_>>()})
            }
            "tools/call" => {
                if let Some(name) = req["params"]["name"].as_str().and_then(|n| self.resolve(n)) {
                    tokio::select! {
                        () = self.stop.cancelled() => ToolReply {text:"turn ended".into(), is_error:true}.wire(),
                        r = self.host.call(name, req["params"]["arguments"].clone()) => r.wire(),
                    }
                } else {
                    ToolReply {
                        text: "tool not mounted".into(),
                        is_error: true,
                    }
                    .wire()
                }
            }
            "notifications/initialized" => Value::Null,
            _ => {
                return json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"unknown method"}});
            }
        };
        json!({"jsonrpc":"2.0","id":id,"result":result})
    }
}
async fn mcp(
    State(b): State<Bridge>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<Value>,
) -> Result<(StatusCode, Json<Value>), StatusCode> {
    // The bearer is not enough: any same-user process can read the sidecar.
    // Windows accepts the socket only when its owner is inside this turn's job.
    // Unix still requires the child or a real forking descendant.
    let allowed = match b.listen.get().copied() {
        Some(listen) => {
            #[cfg(windows)]
            let gate = {
                let job = b.job.clone();
                tokio::task::spawn_blocking(move || {
                    crate::peer::peer_is_allowed(peer, listen, &job)
                })
            };
            #[cfg(not(windows))]
            let gate = {
                let root = b.child_pid.load(Ordering::SeqCst);
                tokio::task::spawn_blocking(move || {
                    crate::peer::peer_is_allowed(peer, listen, root)
                })
            };
            gate.await.unwrap_or(false)
        }
        None => false,
    };
    if !allowed {
        return Err(StatusCode::FORBIDDEN);
    }
    // A per-turn unguessable capability lives only in the private config, never argv/env.
    if headers.get("authorization").and_then(|v| v.to_str().ok()) != Some(b.capability.as_str()) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    if req.get("id").is_none() {
        return Ok((StatusCode::ACCEPTED, Json(Value::Null)));
    }
    Ok((StatusCode::OK, Json(b.rpc(&req).await)))
}

struct BridgeServer {
    file: tempfile::NamedTempFile,
    task: tokio::task::JoinHandle<()>,
    stop: CancellationToken,
    url: String,
    authorization: String,
    #[cfg(not(windows))]
    child_pid: Arc<AtomicU32>,
}
impl Drop for BridgeServer {
    fn drop(&mut self) {
        self.stop.cancel();
        self.task.abort();
    }
}
impl BridgeServer {
    async fn open(bridge: Bridge) -> Result<Self, CliError> {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let listen = listener.local_addr()?;
        bridge.listen.set(listen).map_err(|_| CliError::Io)?;
        let url = format!("http://127.0.0.1:{}/mcp", listen.port());
        let authorization = bridge.capability.clone();
        #[cfg(not(windows))]
        let child_pid = Arc::clone(&bridge.child_pid);
        let config = json!({"mcpServers":{"praxi":{"type":"http","url":url,"headers":{"Authorization":authorization}}}});
        let mut file = tempfile::Builder::new()
            .prefix("praxi-mcp-")
            .suffix(".json")
            .tempfile()?;
        restrict_sidecar(file.path()).await?;
        // NamedTempFile is created mode 0600 on Unix. Do not chmod after writing.
        file.write_all(config.to_string().as_bytes())?;
        let stop = bridge.stop.clone();
        let shutdown = stop.clone();
        let app = Router::new().route("/mcp", post(mcp)).with_state(bridge);
        let task = tokio::spawn(async move {
            let _ = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await;
        });
        Ok(Self {
            file,
            task,
            stop,
            url,
            authorization,
            #[cfg(not(windows))]
            child_pid,
        })
    }

    #[cfg(not(windows))]
    fn allow_child(&self, pid: u32) {
        self.child_pid.store(pid, Ordering::SeqCst);
    }
}

// Windows' equivalent of 0600: a protected DACL granting only the current SID.
// Set it while the newly created file is still empty, before writing capabilities.
#[cfg(windows)]
async fn restrict_sidecar(path: &std::path::Path) -> Result<(), CliError> {
    let system = std::env::var_os("SYSTEMROOT")
        .map_or_else(
            || std::path::PathBuf::from(r"C:\Windows"),
            std::path::PathBuf::from,
        )
        .join("System32");
    let mut who = tokio::process::Command::new(system.join("whoami.exe"));
    apply_sanitized_env(&mut who, &PathEnv::from_ambient());
    who.args(["/user", "/fo", "csv", "/nh"])
        .kill_on_drop(true)
        .creation_flags(0x0800_0000);
    let out = tokio::time::timeout(std::time::Duration::from_secs(3), who.output())
        .await
        .map_err(|_| CliError::Io)??;
    let text = String::from_utf8_lossy(&out.stdout);
    let sid = text
        .split(['"', ',', '\r', '\n'])
        .find(|s| {
            s.starts_with("S-1-")
                && s.chars()
                    .all(|c| c.is_ascii_digit() || matches!(c, 'S' | '-'))
        })
        .ok_or(CliError::Io)?;
    let mut acl = tokio::process::Command::new(system.join("icacls.exe"));
    apply_sanitized_env(&mut acl, &PathEnv::from_ambient());
    acl.arg(path)
        .args(["/inheritance:r", "/grant:r", &format!("*{sid}:(F)")])
        .kill_on_drop(true)
        .creation_flags(0x0800_0000);
    let out = tokio::time::timeout(std::time::Duration::from_secs(3), acl.output())
        .await
        .map_err(|_| CliError::Io)??;
    if !out.status.success() {
        return Err(CliError::Io);
    }
    Ok(())
}
#[cfg(not(windows))]
async fn restrict_sidecar(_: &std::path::Path) -> Result<(), CliError> {
    Ok(())
}

/// Minimum supported versions for CLI providers. Below these floors, we refuse to spawn.
const MIN_CLAUDE_VERSION: (u32, u32, u32) = (2, 1, 257);
const MIN_CLAUDE_VERSION_STRIC_MCP: (u32, u32, u32) = (1, 0, 60);
const MIN_CLAUDE_VERSION_SETTING_SOURCES: (u32, u32, u32) = (1, 0, 122);

/// Permission mode accepted by Claude Code 2.1.314 (`claude --help`).
const CLAUDE_PERMISSION_MODE: &str = "manual";

/// Built-ins removed from Claude. `claude --help` names Bash, PowerShell and
/// the other code-running tools under `--restricted`; the rest are this
/// install's tools (`sdk-tools.d.ts`), including Workflow which runs a script.
const CLAUDE_DISALLOWED_TOOLS: &[&str] = &[
    "Bash",
    "PowerShell",
    "Read",
    "Write",
    "Edit",
    "NotebookEdit",
    "Glob",
    "Grep",
    "WebFetch",
    "WebSearch",
    "Task",
    "Agent",
    "Workflow",
];

/// Features `codex features list` shows as stable and on, plus the shell family
/// that would still run under `sandbox_mode = "read-only"`.
const CODEX_CLOSED_FEATURES: &[&str] = &[
    "shell_tool",
    "unified_exec",
    "view_image",
    "browser_use",
    "browser_use_external",
    "computer_use",
    "hooks",
    "multi_agent",
    "plugins",
];

fn args(
    engine: Engine,
    turn: &Turn,
    config: &std::path::Path,
    version: Option<(u32, u32, u32)>,
    codex_mcp_servers: &[String],
) -> Vec<String> {
    match engine {
        Engine::Claude => claude_args(turn, config, version),
        Engine::Codex => codex_args(turn, codex_mcp_servers),
    }
}

fn claude_args(
    turn: &Turn,
    config: &std::path::Path,
    version: Option<(u32, u32, u32)>,
) -> Vec<String> {
    let mut args: Vec<String> = [
        "-p",
        "--output-format",
        "stream-json",
        "--input-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--permission-mode",
        CLAUDE_PERMISSION_MODE,
        "--tools",
        "",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();

    // `--tools ""` disables every built-in. MCP tools come from `--mcp-config`.
    args.extend(["--mcp-config".into(), config.to_string_lossy().into_owned()]);

    let disallowed_str = CLAUDE_DISALLOWED_TOOLS.join(",");
    let settings = serde_json::json!({
        "disableAllHooks": true,
        "permissions": {"deny": CLAUDE_DISALLOWED_TOOLS}
    })
    .to_string();
    args.extend([
        "--disallowedTools".into(),
        disallowed_str,
        "--settings".into(),
        settings,
    ]);

    // Add version-gated flags
    if let Some(v) = version {
        if v >= MIN_CLAUDE_VERSION_SETTING_SOURCES {
            args.extend(["--setting-sources".into(), String::new()]);
        }
        if v >= MIN_CLAUDE_VERSION_STRIC_MCP {
            args.push("--strict-mcp-config".into());
        }
    }

    if let Some(model) = &turn.model {
        args.extend(["--model".into(), model.clone()]);
    }

    args
}

fn codex_args(turn: &Turn, mcp_servers: &[String]) -> Vec<String> {
    let mut args = Vec::new();
    args.push("app-server".into());
    // `-c` values are TOML (`codex --help`). Booleans are bare; strings are quoted.
    for feature in CODEX_CLOSED_FEATURES {
        args.extend(["-c".into(), format!("features.{feature}=false")]);
    }
    args.extend([
        "-c".into(),
        "web_search=\"disabled\"".into(),
        "-c".into(),
        "sandbox_mode=\"read-only\"".into(),
        "-c".into(),
        "approval_policy=\"on-request\"".into(),
    ]);
    for name in mcp_servers {
        args.extend(["-c".into(), format!("mcp_servers.{name}.enabled=false")]);
    }
    if let Some(model) = &turn.model {
        args.extend([
            "-c".into(),
            format!("model={}", serde_json::to_string(model).expect("string")),
        ]);
    }
    args
}

/// `true` when this Claude's `--help` advertises `manual` and `--tools ""`.
fn claude_launch_accepted(help: &str) -> bool {
    permission_modes(help)
        .iter()
        .any(|mode| mode == CLAUDE_PERMISSION_MODE)
        && help.contains("--disallowedTools")
        && help.contains("--settings")
        && help.contains("disable all tools")
}

fn permission_modes(help: &str) -> Vec<String> {
    let Some(start) = help.find("--permission-mode") else {
        return Vec::new();
    };
    let rest = &help[start..];
    let Some(choices_at) = rest.find("choices:") else {
        return Vec::new();
    };
    let rest = &rest[choices_at + "choices:".len()..];
    let end = rest.find(')').unwrap_or(rest.len().min(400));
    quoted_strings(&rest[..end])
}

fn quoted_strings(blob: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = blob;
    while let Some(start) = rest.find('"') {
        rest = &rest[start + 1..];
        let Some(end) = rest.find('"') else {
            break;
        };
        out.push(rest[..end].to_owned());
        rest = &rest[end + 1..];
    }
    out
}

#[cfg_attr(not(test), allow(dead_code))]
fn codex_config_path(env: &PathEnv) -> Result<std::path::PathBuf, CliError> {
    if let Some(home) = env
        .env_vars
        .get("CODEX_HOME")
        .filter(|value| !value.is_empty())
    {
        return Ok(std::path::PathBuf::from(home).join("config.toml"));
    }
    let home = env
        .home_dir
        .clone()
        .or_else(|| {
            env.env_vars
                .get("USERPROFILE")
                .map(std::path::PathBuf::from)
        })
        .or_else(|| env.env_vars.get("HOME").map(std::path::PathBuf::from))
        .ok_or(CliError::NativeToolsOpen)?;
    Ok(home.join(".codex").join("config.toml"))
}

/// Names of MCP servers in the user's Codex config. Missing file means none.
/// An unreadable file, an include, or a name that cannot be targeted with `-c`
/// refuses the turn: those servers would stay enabled.
#[cfg_attr(not(test), allow(dead_code))]
fn codex_user_mcp_servers(env: &PathEnv) -> Result<Vec<String>, CliError> {
    let path = codex_config_path(env)?;
    match std::fs::read_to_string(&path) {
        Ok(text) => codex_mcp_server_names(&text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(_) => Err(CliError::NativeToolsOpen),
    }
}

#[cfg_attr(not(test), allow(dead_code))]
fn codex_mcp_server_names(text: &str) -> Result<Vec<String>, CliError> {
    let parsed: toml::Value = toml::from_str(text).map_err(|_| CliError::NativeToolsOpen)?;
    let Some(root) = parsed.as_table() else {
        return Err(CliError::NativeToolsOpen);
    };
    if root.contains_key("include") || root.contains_key("includes") {
        return Err(CliError::NativeToolsOpen);
    }
    let Some(servers) = root.get("mcp_servers") else {
        return Ok(Vec::new());
    };
    let Some(table) = servers.as_table() else {
        return Err(CliError::NativeToolsOpen);
    };
    let mut names = Vec::new();
    for name in table.keys() {
        if !mcp_server_name_ok(name) {
            return Err(CliError::NativeToolsOpen);
        }
        names.push(name.clone());
    }
    names.sort();
    Ok(names)
}

#[cfg_attr(not(test), allow(dead_code))]
fn mcp_server_name_ok(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn codex_closed_features() -> serde_json::Map<String, Value> {
    CODEX_CLOSED_FEATURES
        .iter()
        .map(|name| ((*name).to_owned(), Value::Bool(false)))
        .collect()
}

/// The `thread/start` result must echo the closed sandbox, every closed
/// feature, and an empty environment list. A missing field is a refusal.
fn codex_confinement_echoed(result: &Value) -> bool {
    let Some(config) = result.get("config").filter(|value| value.is_object()) else {
        return false;
    };
    let features_closed = CODEX_CLOSED_FEATURES.iter().all(|name| {
        config["features"][*name] == false || config[&format!("features.{name}")] == false
    });
    if !features_closed || config["web_search"] != "disabled" {
        return false;
    }
    let sandbox = if result.get("sandbox").is_some() {
        &result["sandbox"]
    } else {
        &config["sandbox_mode"]
    };
    let sandbox_closed = sandbox == "read-only" || sandbox["type"] == "readOnly";
    let environments = if result.get("environments").is_some() {
        &result["environments"]
    } else if result["thread"].get("environments").is_some() {
        &result["thread"]["environments"]
    } else {
        return false;
    };
    sandbox_closed && environments.as_array().is_some_and(Vec::is_empty)
}

async fn measured_version(
    engine: Engine,
    resolved: &crate::detect::ResolvedCommand,
    env: &PathEnv,
    stop: &CancellationToken,
) -> Result<Option<(u32, u32, u32)>, CliError> {
    if engine == Engine::Claude {
        tokio::select! {
            () = stop.cancelled() => Err(CliError::Cancelled),
            v = crate::detect::claude_version(resolved, env) => Ok(v),
        }
    } else {
        // Same path the turn would spawn. Codex is still not started.
        crate::detect::codex_version(resolved, env).await
    }
}

async fn send(stdin: &mut tokio::process::ChildStdin, value: Value) -> Result<(), CliError> {
    let mut bytes = serde_json::to_vec(&value).map_err(|_| CliError::Protocol)?;
    bytes.push(b'\n');
    stdin.write_all(&bytes).await?;
    stdin.flush().await?;
    Ok(())
}

#[allow(clippy::too_many_lines)] // Security-critical session handling, fixture-locked
async fn session(
    engine: Engine,
    command: &str,
    env: &PathEnv,
    turn: Turn,
    host: Arc<dyn TurnHost>,
    stop: CancellationToken,
) -> Result<TurnUsage, CliError> {
    let candidate = find_candidate(command, env).ok_or(CliError::NotInstalled)?;
    let resolved = resolve_spawn(&candidate, env).map_err(|_| CliError::ShimRefused)?;
    #[cfg(windows)]
    let job = crate::detect::SharedJob::new();
    let bridge = Bridge {
        capability: format!("Bearer {}{}", uuid::Uuid::now_v7(), uuid::Uuid::now_v7()),
        tools: Arc::new(turn.tools.clone()),
        host: host.clone(),
        stop: stop.clone(),
        #[cfg(not(windows))]
        child_pid: Arc::new(AtomicU32::new(0)),
        #[cfg(windows)]
        job: job.clone(),
        listen: Arc::new(OnceLock::new()),
    };
    let server = BridgeServer::open(bridge.clone()).await?;
    if stop.is_cancelled() {
        return Err(CliError::Cancelled);
    }
    let pin = binary_pin(&resolved.program).ok_or(CliError::NotInstalled)?;
    let version = measured_version(engine, &resolved, env, &stop).await?;
    if binary_pin(&resolved.program).as_ref() != Some(&pin) {
        return Err(CliError::BinaryChanged);
    }

    // Enforce minimum version floors - refuse to spawn if below minimum.
    // Codex is not started at all: this install cannot turn unified_exec off.
    if engine == Engine::Claude {
        let min_ver = MIN_CLAUDE_VERSION;
        if let Some(v) = version {
            if v < min_ver {
                eprintln!("Claude Code version {v:?} is below minimum required {min_ver:?}");
                return Err(CliError::NotInstalled);
            }
        } else {
            // Missing version detection means we cannot verify security features
            return Err(CliError::NotInstalled);
        }
        let help = crate::detect::claude_help(&resolved, env)
            .await
            .ok_or(CliError::NativeToolsOpen)?;
        if !claude_launch_accepted(&help) {
            return Err(CliError::NativeToolsOpen);
        }
    }
    let version_now = measured_version(engine, &resolved, env, &stop).await?;
    let pin_now = binary_pin(&resolved.program).ok_or(CliError::BinaryChanged)?;
    if version_now != version || pin_now != pin {
        return Err(CliError::BinaryChanged);
    }
    if engine == Engine::Codex {
        return Err(CliError::CodexUnconfinable);
    }
    let mcp_servers = Vec::new();

    let argv = args(engine, &turn, server.file.path(), version, &mcp_servers);
    if resolved.program.to_string_lossy().encode_utf16().count()
        + resolved
            .args
            .iter()
            .chain(&argv)
            .map(|a| a.encode_utf16().count() * 2 + 3)
            .sum::<usize>()
        > 30_000
    {
        return Err(CliError::Protocol);
    }

    // Create an empty per-turn working directory to isolate the child
    let workdir = tempfile::tempdir().map_err(|_| CliError::Io)?;

    let mut cmd = tokio::process::Command::new(&resolved.program);
    cmd.args(&resolved.args)
        .args(argv)
        .current_dir(workdir.path()); // Set to empty dir, not project
    // Codex receives MCP configuration over the thread/start stdin frame.
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    apply_sanitized_env(&mut cmd, env);
    #[cfg(unix)]
    cmd.process_group(0);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    #[cfg(windows)]
    let mut isolated = isolate(cmd, job);
    #[cfg(not(windows))]
    let mut isolated = isolate(cmd);
    let mut child = isolated.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            CliError::NotInstalled
        } else {
            CliError::Io
        }
    })?;
    #[cfg(not(windows))]
    {
        let child_pid = child
            .id()
            .filter(|pid| *pid != 0)
            .ok_or(CliError::NativeToolsOpen)?;
        server.allow_child(child_pid);
    }

    // Store workdir for lifetime of session so cwd stays isolated
    let _workdir = workdir;

    let mut stdin = child.stdin().take().ok_or(CliError::Io)?;
    let stdout = child.stdout().take().ok_or(CliError::Io)?;
    let mut stderr = child.stderr().take().ok_or(CliError::Io)?;
    // Fixed byte ring; intentionally never surfaced/logged, so credential-looking
    // child diagnostics cannot escape before the host redactor sees them.
    let stderr_task = tokio::spawn(async move {
        let mut ring = std::collections::VecDeque::with_capacity(4096);
        let mut buf = [0; 1024];
        while let Ok(n) = stderr.read(&mut buf).await {
            if n == 0 {
                break;
            }
            for b in &buf[..n] {
                if ring.len() == 4096 {
                    ring.pop_front();
                }
                ring.push_back(*b);
            }
        }
    });
    let result = tokio::select! {
        () = stop.cancelled() => Err(CliError::Cancelled),
        r = protocol(engine, &turn, &bridge, &server, host.as_ref(), &mut stdin, stdout) => r,
    };
    stop.cancel();
    kill_tree(child.as_mut()).await;
    stderr_task.abort();
    result
}

async fn protocol(
    engine: Engine,
    turn: &Turn,
    bridge: &Bridge,
    server: &BridgeServer,
    host: &dyn TurnHost,
    stdin: &mut tokio::process::ChildStdin,
    stdout: tokio::process::ChildStdout,
) -> Result<TurnUsage, CliError> {
    if engine == Engine::Claude {
        send(stdin, json!({"type":"user","message":{"role":"user","content":format!("{}\n\n{}", turn.system, turn.prompt)}})).await?;
    } else {
        send(
            stdin,
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
                "clientInfo":{"name":"praxi","version":"0.1.0"},"capabilities":{}
            }}),
        )
        .await?;
    }
    let mut reader = BufReader::new(stdout);
    let mut usage = TurnUsage::default();
    let mut codex = CodexState::default();
    let mut partial = false;
    loop {
        // A hard allocation cap applies even when a child never writes LF.
        let mut bytes = Vec::new();
        let n = (&mut reader)
            .take(1024 * 1024 + 1)
            .read_until(b'\n', &mut bytes)
            .await?;
        if n == 0 || n > 1024 * 1024 {
            return Err(CliError::Protocol);
        }
        let v: Value = serde_json::from_slice(&bytes).map_err(|_| CliError::Protocol)?;
        if v.get("error").is_some() || v["is_error"] == true || v["method"] == "error" {
            return Err(child_error(&v));
        }
        let done = match engine {
            Engine::Claude => claude_frame(&v, bridge, host, &mut usage, &mut partial)?,
            Engine::Codex => {
                codex
                    .frame(&v, turn, bridge, server, stdin, &mut usage)
                    .await?
            }
        };
        if done {
            return Ok(usage);
        }
    }
}
fn child_error(v: &Value) -> CliError {
    let text = v.to_string().to_ascii_lowercase();
    if ["quota", "rate_limit", "rate limit", "usage limit"]
        .iter()
        .any(|s| text.contains(s))
    {
        CliError::Quota
    } else {
        CliError::Protocol
    }
}

fn claude_frame(
    v: &Value,
    bridge: &Bridge,
    host: &dyn TurnHost,
    usage: &mut TurnUsage,
    partial: &mut bool,
) -> Result<bool, CliError> {
    match v["type"].as_str().unwrap_or("") {
        "stream_event" => {
            let e = &v["event"];
            if e["type"] == "message_start" {
                *partial = false;
            }
            if e["delta"]["type"] == "text_delta"
                && let Some(t) = e["delta"]["text"].as_str()
            {
                *partial = true;
                host.text(t);
            }
            if let Some(kind) = e["content_block"]["type"].as_str() {
                match kind {
                    "tool_use" => validate_tool(bridge, &e["content_block"])?,
                    "text" | "thinking" => {}
                    _ => return Err(CliError::NativeToolsOpen),
                }
            }
        }
        "assistant" => {
            if let Some(blocks) = v["message"]["content"].as_array() {
                for block in blocks {
                    match block["type"].as_str() {
                        Some("tool_use") => validate_tool(bridge, block)?,
                        Some("text") if !*partial => {
                            if let Some(text) = block["text"].as_str() {
                                host.text(text);
                            }
                        }
                        Some("text" | "thinking") => {}
                        _ => return Err(CliError::NativeToolsOpen),
                    }
                }
            }
            *partial = false;
        }
        "result" => {
            if v["subtype"]
                .as_str()
                .is_some_and(|s| s.starts_with("error"))
            {
                return Err(child_error(v));
            }
            read_usage(&v["usage"], usage);
            return Ok(true);
        }
        _ => return Err(CliError::NativeToolsOpen),
    }
    Ok(false)
}

#[derive(Default)]
struct CodexState {
    initialized: bool,
    thread: Option<String>,
    started: bool,
}
impl CodexState {
    #[allow(clippy::too_many_lines)] // One fixture-locked JSON-RPC state machine.
    async fn frame(
        &mut self,
        v: &Value,
        turn: &Turn,
        bridge: &Bridge,
        server: &BridgeServer,
        stdin: &mut tokio::process::ChildStdin,
        usage: &mut TurnUsage,
    ) -> Result<bool, CliError> {
        if v["id"] == 1 && v.get("result").is_some() {
            if self.initialized {
                return Err(CliError::Protocol);
            }
            self.initialized = true;
            send(
                stdin,
                json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
            )
            .await?;

            let config = json!({
                "features": codex_closed_features(),
                "web_search": "disabled",
                "sandbox_mode": "read-only",
                "approval_policy": "on-request",
                "mcp_servers": {"praxi": {"url": server.url, "http_headers": {"Authorization": server.authorization}}}
            });
            send(
                stdin,
                json!({"jsonrpc":"2.0","id":2,"method":"thread/start","params":{
                    "model":turn.model,"baseInstructions":turn.system,
                    "approvalPolicy":"on-request",
                    "sandbox":"read-only",
                    "sandboxPolicy":{"type":"readOnly"},
                    "environments": [],
                    "config": config
                }}),
            )
            .await?;
        } else if v["id"] == 2 && v.get("result").is_some() {
            let id = v["result"]["thread"]["id"]
                .as_str()
                .ok_or(CliError::Protocol)?;
            if !self.initialized || self.thread.is_some() {
                return Err(CliError::Protocol);
            }
            if !codex_confinement_echoed(&v["result"]) {
                return Err(CliError::NativeToolsOpen);
            }

            self.thread = Some(id.to_owned());
            send(
                stdin,
                json!({"jsonrpc":"2.0","id":3,"method":"turn/start","params":{
                    "threadId":id,"input":[{"type":"text","text":turn.prompt}],
                    "approvalPolicy":"on-request","sandboxPolicy":{"type":"readOnly"}
                }}),
            )
            .await?;
        } else if v["id"] == 3 && v.get("result").is_some() {
            if self.thread.is_none() || self.started {
                return Err(CliError::Protocol);
            }
            self.started = true;
        } else {
            match v["method"].as_str().unwrap_or("") {
                "item/agentMessage/delta" => {
                    if self.thread.is_none() {
                        return Err(CliError::Protocol);
                    }
                    if let Some(t) = v["params"]["delta"].as_str() {
                        bridge.host.text(t);
                    }
                }
                "thread/tokenUsage/updated" => {
                    read_usage(&v["params"]["tokenUsage"]["last"], usage);
                }
                "turn/completed" => {
                    if self.thread.is_none() || v["params"]["turn"]["status"] != "completed" {
                        return Err(child_error(v));
                    }
                    return Ok(true);
                }
                "item/started" | "item/completed" => {
                    let item = &v["params"]["item"];
                    match item["type"].as_str().unwrap_or("") {
                        "userMessage" | "agentMessage" | "reasoning" => {}
                        "mcpToolCall"
                            if item["server"] == "praxi"
                                && item["tool"]
                                    .as_str()
                                    .and_then(|n| bridge.resolve(n))
                                    .is_some() => {}
                        _ => return Err(CliError::NativeToolsOpen),
                    }
                }
                "item/mcpToolCall/requestApproval" => {
                    let p = &v["params"];
                    if p["server"] != "praxi" {
                        return Err(CliError::NativeToolsOpen);
                    }
                    let name = p["tool"]
                        .as_str()
                        .and_then(|n| bridge.resolve(n))
                        .ok_or(CliError::NativeToolsOpen)?;
                    let allow = bridge.host.approve(name, p["arguments"].clone()).await;
                    send(stdin, json!({"jsonrpc":"2.0","id":v["id"],"result":{"decision":if allow {"accept"} else {"decline"}}})).await?;
                }
                // Approval methods, output deltas, and anything else are a
                // native action. Stop before the next stdout line is applied.
                _ => return Err(CliError::NativeToolsOpen),
            }
        }
        Ok(false)
    }
}
fn validate_tool(bridge: &Bridge, block: &Value) -> Result<(), CliError> {
    if block["name"]
        .as_str()
        .and_then(|n| bridge.resolve(n))
        .is_none()
    {
        return Err(CliError::NativeToolsOpen);
    }
    Ok(())
}
fn read_usage(v: &Value, usage: &mut TurnUsage) {
    if !v.is_object() {
        return;
    }
    usage.reported = v.get("input_tokens").is_some() || v.get("inputTokens").is_some();
    usage.input = v["input_tokens"]
        .as_u64()
        .or_else(|| v["inputTokens"].as_u64())
        .unwrap_or(0);
    usage.output = v["output_tokens"]
        .as_u64()
        .or_else(|| v["outputTokens"].as_u64())
        .unwrap_or(0);
    usage.cached = v["cache_read_input_tokens"]
        .as_u64()
        .or_else(|| v["cachedInputTokens"].as_u64());
}

#[cfg(test)]
mod tests {
    use super::*;
    struct NoCalls;
    #[async_trait]
    impl TurnHost for NoCalls {
        fn text(&self, _: &str) {}
        async fn call(&self, _: &str, _: Value) -> ToolReply {
            panic!("unexpected tool body")
        }
        async fn approve(&self, _: &str, _: Value) -> bool {
            panic!("unexpected approval")
        }
    }
    #[test]
    fn argv_contains_no_prompt_and_version_flags_are_gated() {
        let turn = Turn {
            prompt: "private prompt\n%PATH%".into(),
            system: "private system\n%PATH%".into(),
            model: Some("fixture-model".into()),
            tools: vec![],
        };
        for engine in [Engine::Claude, Engine::Codex] {
            let argv = args(
                engine,
                &turn,
                std::path::Path::new("private-config.json"),
                None,
                &[],
            );
            assert!(!argv.iter().any(|a| a.contains("private prompt")
                || a.contains("private system")
                || a.contains("%PATH%")));
            assert!(!argv.iter().any(|a| a == "--strict-mcp-config"));
            assert!(
                !argv
                    .iter()
                    .any(|a| a.contains("bearer") || a.contains("environments="))
            );
        }
        let claude = args(
            Engine::Claude,
            &turn,
            std::path::Path::new("private-config.json"),
            Some((2, 1, 300)),
            &[],
        );
        assert!(claude.iter().any(|a| a == "--strict-mcp-config"));
        assert!(claude.iter().any(|a| a == "--setting-sources"));
        assert_eq!(
            claude
                .windows(2)
                .find(|pair| pair[0] == "--permission-mode")
                .map(|pair| pair[1].as_str()),
            Some("manual")
        );
        assert_eq!(
            claude
                .windows(2)
                .find(|pair| pair[0] == "--tools")
                .map(|pair| pair[1].as_str()),
            Some("")
        );
        let denied = claude
            .windows(2)
            .find(|pair| pair[0] == "--disallowedTools")
            .map(|pair| pair[1].as_str())
            .unwrap();
        for name in CLAUDE_DISALLOWED_TOOLS {
            assert!(denied.split(',').any(|tool| tool == *name), "{name}");
        }
        let settings = claude
            .windows(2)
            .find(|pair| pair[0] == "--settings")
            .map(|pair| pair[1].as_str())
            .unwrap();
        let settings: Value = serde_json::from_str(settings).unwrap();
        assert_eq!(
            settings["permissions"]["deny"].as_array().unwrap().len(),
            CLAUDE_DISALLOWED_TOOLS.len()
        );
        let codex = args(
            Engine::Codex,
            &turn,
            std::path::Path::new("private-config.json"),
            None,
            &["demo".into()],
        );
        for feature in CODEX_CLOSED_FEATURES {
            assert!(
                codex
                    .iter()
                    .any(|arg| arg == &format!("features.{feature}=false"))
            );
        }
        assert!(codex.iter().any(|arg| arg == "web_search=\"disabled\""));
        assert!(codex.iter().any(|arg| arg == "sandbox_mode=\"read-only\""));
        assert!(
            codex
                .iter()
                .any(|arg| arg == "approval_policy=\"on-request\"")
        );
        assert!(
            codex
                .iter()
                .any(|arg| arg == "mcp_servers.demo.enabled=false")
        );
        assert!(
            !codex
                .iter()
                .any(|arg| arg.contains("exec_command") || arg.contains("apply_patch"))
        );
    }
    #[test]
    fn claude_launch_requires_a_known_permission_mode() {
        let accepted = "\
--permission-mode <mode> (choices: \"acceptEdits\", \"auto\", \"bypassPermissions\", \"manual\", \"dontAsk\", \"plan\")
--tools Use \"\" to disable all tools
--disallowedTools
--settings
";
        assert!(claude_launch_accepted(accepted));
        let unknown = accepted.replace("\"manual\", ", "");
        assert!(!claude_launch_accepted(&unknown));
        assert!(!claude_launch_accepted(
            "--permission-mode <mode> (choices: \"manual\")"
        ));
    }
    #[test]
    fn codex_mcp_config_names_are_closed_or_refused() {
        let dir = tempfile::tempdir().unwrap();
        let env = PathEnv::for_test(dir.path().into())
            .with_env_var("CODEX_HOME", dir.path().display().to_string());
        assert_eq!(codex_user_mcp_servers(&env).unwrap(), Vec::<String>::new());
        std::fs::write(
            dir.path().join("config.toml"),
            "[mcp_servers.demo]\ncommand = \"helper\"\n[mcp_servers.other_tool]\ncommand = \"y\"\n",
        )
        .unwrap();
        assert_eq!(
            codex_user_mcp_servers(&env).unwrap(),
            vec!["demo".to_owned(), "other_tool".to_owned()]
        );
        std::fs::write(
            dir.path().join("config.toml"),
            "[mcp_servers.\"a.b\"]\ncommand = \"helper\"\n",
        )
        .unwrap();
        assert_eq!(
            codex_user_mcp_servers(&env).unwrap_err(),
            CliError::NativeToolsOpen
        );
        std::fs::write(
            dir.path().join("config.toml"),
            "include = [\"other.toml\"]\n",
        )
        .unwrap();
        assert_eq!(
            codex_user_mcp_servers(&env).unwrap_err(),
            CliError::NativeToolsOpen
        );
    }
    #[test]
    fn missing_codex_echo_is_not_closed() {
        assert!(!codex_confinement_echoed(&json!({"thread":{"id":"t"}})));
        assert!(!codex_confinement_echoed(&json!({
            "thread":{"id":"t"},
            "config":{"features":{"shell_tool":false},"web_search":"disabled"}
        })));
        let mut features = serde_json::Map::new();
        for name in CODEX_CLOSED_FEATURES {
            features.insert((*name).to_owned(), json!(false));
        }
        assert!(codex_confinement_echoed(&json!({
            "thread":{"id":"t","environments":[]},
            "sandbox":"read-only",
            "environments":[],
            "config":{"features":features,"web_search":"disabled","sandbox_mode":"read-only"}
        })));
    }
    fn bridge() -> Bridge {
        Bridge {
            capability: "Bearer private-turn-capability".into(),
            tools: Arc::new(vec![]),
            host: Arc::new(NoCalls),
            stop: CancellationToken::new(),
            #[cfg(not(windows))]
            child_pid: Arc::new(AtomicU32::new(std::process::id())),
            #[cfg(windows)]
            job: crate::detect::SharedJob::new(),
            listen: Arc::new(OnceLock::new()),
        }
    }
    fn http_post(url: &str, auth: Option<&str>, body: &str) -> String {
        use std::io::{Read, Write};
        let address = url
            .strip_prefix("http://")
            .unwrap()
            .split('/')
            .next()
            .unwrap();
        let mut stream = std::net::TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let auth = auth
            .map(|value| format!("Authorization: {value}\r\n"))
            .unwrap_or_default();
        write!(
            stream,
            "POST /mcp HTTP/1.1\r\nHost: {address}\r\n{auth}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut out = String::new();
        stream.read_to_string(&mut out).unwrap();
        out
    }
    #[tokio::test]
    async fn bridge_rejects_other_turns_and_unmounted_tools() {
        let bridge = bridge();
        let req = json!({"id":1,"method":"tools/call","params":{"name":"Bash","arguments":{}}});
        let reply = bridge.rpc(&req).await;
        assert_eq!(reply["result"]["isError"], true);
        #[cfg(windows)]
        let job = bridge.job.clone();
        let server = BridgeServer::open(bridge).await.unwrap();
        let path = server.file.path().to_owned();
        let config: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let url = config["mcpServers"]["praxi"]["url"]
            .as_str()
            .unwrap()
            .to_owned();
        let body = req.to_string();
        let unauth = post_allowed(
            &url,
            None,
            &body,
            #[cfg(windows)]
            &job,
        )
        .await;
        assert!(unauth.starts_with("HTTP/1.1 401"), "{unauth}");
        let wrong = post_allowed(
            &url,
            Some("Bearer another-turn"),
            &body,
            #[cfg(windows)]
            &job,
        )
        .await;
        assert!(wrong.starts_with("HTTP/1.1 401"), "{wrong}");
        let ok = post_allowed(
            &url,
            Some("Bearer private-turn-capability"),
            &body,
            #[cfg(windows)]
            &job,
        )
        .await;
        assert!(ok.starts_with("HTTP/1.1 200"), "{ok}");
        assert!(ok.contains("\"isError\":true"));
        assert!(path.exists());
        drop(server);
        assert!(!path.exists());
    }
    #[tokio::test]
    async fn non_child_connection_rejected() {
        let bridge = bridge();
        #[cfg(not(windows))]
        bridge.child_pid.store(u32::MAX, Ordering::SeqCst);
        let server = BridgeServer::open(bridge).await.unwrap();
        let config: Value =
            serde_json::from_str(&std::fs::read_to_string(server.file.path()).unwrap()).unwrap();
        let url = config["mcpServers"]["praxi"]["url"]
            .as_str()
            .unwrap()
            .to_owned();
        let auth = config["mcpServers"]["praxi"]["headers"]["Authorization"]
            .as_str()
            .unwrap()
            .to_owned();
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"Bash","arguments":{}}}"#.to_owned();
        let response = post_off_runtime(url, Some(auth), body).await;
        assert!(response.starts_with("HTTP/1.1 403"), "{response}");
    }
    async fn post_off_runtime(url: String, auth: Option<String>, body: String) -> String {
        tokio::task::spawn_blocking(move || http_post(&url, auth.as_deref(), &body))
            .await
            .unwrap()
    }
    async fn post_allowed(
        url: &str,
        auth: Option<&str>,
        body: &str,
        #[cfg(windows)] job: &crate::detect::SharedJob,
    ) -> String {
        #[cfg(not(windows))]
        {
            post_off_runtime(url.to_owned(), auth.map(str::to_owned), body.to_owned()).await
        }
        #[cfg(windows)]
        {
            post_inside_job(job, url, auth, body).await
        }
    }
    #[cfg(windows)]
    async fn post_inside_job(
        job: &crate::detect::SharedJob,
        url: &str,
        auth: Option<&str>,
        body: &str,
    ) -> String {
        let dir = tempfile::tempdir().unwrap();
        let body_path = dir.path().join("body.json");
        std::fs::write(&body_path, body).unwrap();
        let curl = std::env::var_os("SYSTEMROOT")
            .map_or_else(
                || std::path::PathBuf::from(r"C:\Windows"),
                std::path::PathBuf::from,
            )
            .join("System32")
            .join("curl.exe");
        let mut cmd = tokio::process::Command::new(curl);
        cmd.arg("-sS").arg("-i");
        if let Some(auth) = auth {
            cmd.arg("-H").arg(format!("Authorization: {auth}"));
        }
        cmd.arg("-H")
            .arg("Content-Type: application/json")
            .arg("--data-binary")
            .arg(format!("@{}", body_path.display()))
            .arg(url)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let mut wrap = isolate(cmd, job.clone());
        let mut child = wrap.spawn().expect("curl inside the turn job");
        let mut stdout = child.stdout().take().expect("stdout");
        let mut stderr = child.stderr().take().expect("stderr");
        let status = tokio::time::timeout(std::time::Duration::from_secs(20), child.wait())
            .await
            .expect("curl timed out")
            .expect("curl wait");
        let mut out = Vec::new();
        let mut err = Vec::new();
        let _ = stdout.read_to_end(&mut out).await;
        let _ = stderr.read_to_end(&mut err).await;
        assert!(
            status.success(),
            "curl failed: {}",
            String::from_utf8_lossy(&err)
        );
        String::from_utf8_lossy(&out).into_owned()
    }
    #[test]
    fn tool_names_are_collision_free() {
        let names = ["fs.read", "fs__read", "mcp:server/tool", "fs_read"];
        let encoded: std::collections::HashSet<_> = names.iter().map(|n| tool_name(n)).collect();
        assert_eq!(encoded.len(), names.len());
    }
}
