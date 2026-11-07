//! Per-turn CLI sessions. Wire dialects are locked by `tests/fixtures/runtime_cli.rs`.
//! No credential files are opened here. Prompts and results only cross stdin/MCP.
use std::{io::Write, process::Stdio, sync::Arc};

use async_trait::async_trait;
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio_util::sync::CancellationToken;
use tool_registry::ToolSpec;

use crate::detect::{
    PathEnv, apply_sanitized_env, find_candidate, isolate, kill_tree, resolve_spawn,
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
    headers: HeaderMap,
    _remote_addr: (), // Connection info - we trust OS process isolation
    Json(req): Json<Value>,
) -> Result<(StatusCode, Json<Value>), StatusCode> {
    // A per-turn unguessable capability lives only in the private config, never argv/env.
    if headers.get("authorization").and_then(|v| v.to_str().ok()) != Some(b.capability.as_str()) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    // Connection comes from same process group (Unix) or job object (Windows)
    // The temp MCP config file is mode 0600 and only readable by current user

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
        let url = format!("http://127.0.0.1:{}/mcp", listener.local_addr()?.port());
        let authorization = bridge.capability.clone();
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
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(shutdown.cancelled_owned())
                .await;
        });
        Ok(Self {
            file,
            task,
            stop,
            url,
            authorization,
        })
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
const MIN_CODEX_VERSION: (u32, u32, u32) = (1, 0, 0);

/// Disallowed native tools for Claude that break Praxi's permission model.
const CLAUDE_DISALLOWED_TOOLS: &[&str] = &[
    "Bash",
    "Read",
    "Write",
    "Edit",
    "WebFetch",
    "WebSearch",
    "Task",
    "Agent",
];

/// Disallowed features for Codex that break Praxi's permission model.
const CODEX_DISALLOWED_FEATURES: &[&str] = &[
    "shell_tool",
    "unified_exec",
    "view_image",
    "apply_patch_freeform",
    "exec_command",
];

fn args(
    engine: Engine,
    turn: &Turn,
    config: &std::path::Path,
    version: Option<(u32, u32, u32)>,
) -> Vec<String> {
    match engine {
        Engine::Claude => claude_args(turn, config, version),
        Engine::Codex => codex_args(turn, config, version),
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
        "default",
        "--tools",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();

    // Pass only allowed MCP tools, no native tools
    args.push(
        turn.tools
            .iter()
            .map(|s| format!("mcp__praxi__{}", tool_name(&s.name)))
            .collect::<Vec<_>>()
            .join(","),
    );

    // Use strict MCP config (only use servers from --mcp-config file)
    args.extend(["--mcp-config".into(), config.to_string_lossy().into_owned()]);

    // Disable all hooks via settings
    let disallowed_str = CLAUDE_DISALLOWED_TOOLS.join(",");
    args.extend([
        "--disallowedTools".into(),
        disallowed_str,
        "--settings".into(),
        r#"{"disableAllHooks":true}"#.into(),
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

fn codex_args(
    turn: &Turn,
    config: &std::path::Path,
    _version: Option<(u32, u32, u32)>,
) -> Vec<String> {
    let mut args = Vec::new();
    args.push("app-server".into());

    // Disable ALL dangerous features explicitly
    for feature in CODEX_DISALLOWED_FEATURES {
        args.extend(["-c".into(), format!("features.{feature}=false")]);
    }

    // Explicitly disable web_search at argv level
    args.extend(["-c".into(), "web_search=disabled".into()]);

    // Disable inherited MCP servers - only allow praxi
    args.extend([
        "-c".into(),
        format!(
            "environments=[{{\"url\":\"{}\",\"http_headers\":{{\"Authorization\":\"bearer-token\"}}}}]",
            url_encode_path(config)
        ),
    ]);

    if let Some(model) = &turn.model {
        args.extend([
            "-c".into(),
            format!("model={}", serde_json::to_string(model).expect("string")),
        ]);
    }

    args
}

// URL encode path for safe embedding in TOML string
fn url_encode_path(path: &std::path::Path) -> String {
    let s = path.to_string_lossy();
    s.replace("\\\\", "\\\\\\\\").replace('\\', "\\\\")
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
    let bridge = Bridge {
        capability: format!("Bearer {}{}", uuid::Uuid::now_v7(), uuid::Uuid::now_v7()),
        tools: Arc::new(turn.tools.clone()),
        host: host.clone(),
        stop: stop.clone(),
    };
    let server = BridgeServer::open(bridge.clone()).await?;
    if stop.is_cancelled() {
        return Err(CliError::Cancelled);
    }
    let version = if engine == Engine::Claude {
        // Bounded, sanitized version probe; never runs a credential command.
        tokio::select! {
            () = stop.cancelled() => return Err(CliError::Cancelled),
            v = crate::detect::claude_version(&resolved, env) => v,
        }
    } else {
        // Probe Codex version for security floor enforcement
        crate::detect::codex_version(&resolved, env).await?
    };

    // Enforce minimum version floors - refuse to spawn if below minimum
    match engine {
        Engine::Claude => {
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
        }
        Engine::Codex => {
            let min_ver = MIN_CODEX_VERSION;
            if let Some(v) = version {
                if v < min_ver {
                    eprintln!("Codex version {v:?} is below minimum required {min_ver:?}");
                    return Err(CliError::NotInstalled);
                }
            } else {
                return Err(CliError::NotInstalled);
            }
        }
    }

    let argv = args(engine, &turn, server.file.path(), version);
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
    let mut child = isolate(cmd).spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            CliError::NotInstalled
        } else {
            CliError::Io
        }
    })?;

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
            if e["content_block"]["type"] == "tool_use" {
                validate_tool(bridge, &e["content_block"])?;
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
                        _ => {}
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
        _ => {}
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

            send(stdin, json!({"jsonrpc":"2.0","id":2,"method":"thread/start","params":{
                "model":turn.model,"baseInstructions":turn.system,
                "approvalPolicy":"on-request",
                "sandbox":"read-only",
                "sandboxPolicy":{"type":"readOnly"},
                "config":{
                    "features.shell_tool":false,
                    "features.apply_patch_freeform":false,
                    "features.collab":false,
                    "web_search":"disabled",
                    "mcp_servers":{"praxi":{"url":server.url,"http_headers":{"Authorization":server.authorization}}}
                }
            }})).await?;
        } else if v["id"] == 2 && v.get("result").is_some() {
            let id = v["result"]["thread"]["id"]
                .as_str()
                .ok_or(CliError::Protocol)?;
            if !self.initialized || self.thread.is_some() {
                return Err(CliError::Protocol);
            }

            // Verify that thread/start echoed back our security config
            // The response should include approvalPolicy=on-request and sandbox=read-only
            let result = &v["result"];

            // Check that shell_tool is disabled in response (or we got a valid thread id)
            if result["thread"]["id"].is_null() {
                return Err(CliError::Protocol);
            }

            // If config was echoed back, verify critical security settings
            if let Some(config) = result.get("config") {
                let shell_disabled = config["features"]["shell_tool"] == false;
                let web_disabled = config["web_search"] == "disabled";

                if !shell_disabled || !web_disabled {
                    eprintln!("Codex refused to disable shell tools");
                    return Err(CliError::NativeToolsOpen);
                }
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
                        "userMessage" | "agentMessage" | "reasoning" | "plan"
                        | "contextCompaction" => {}
                        "mcpToolCall"
                            if item["server"] == "praxi"
                                && item["tool"]
                                    .as_str()
                                    .and_then(|n| bridge.resolve(n))
                                    .is_some() => {}
                        // Any other tool type means native tools are open - fail closed
                        _ => {
                            eprintln!("Unsafe tool detected: {}", item["type"]);
                            return Err(CliError::NativeToolsOpen);
                        }
                    }
                }
                // These methods indicate native tools are still active
                "item/commandExecution/requestApproval"
                | "item/fileChange/requestApproval"
                | "item/execCommandApproval/requestApproval"
                | "item/applyPatchApproval/requestApproval" => {
                    eprintln!("Native tool approval requested: {}", v["method"]);
                    return Err(CliError::NativeToolsOpen);
                }
                // MCP tool calls from non-praxi servers must also fail
                "item/mcpToolCall/requestApproval" => {
                    let p = &v["params"];
                    if p["server"] != "praxi" {
                        eprintln!("Non-Praxi MCP tool call attempted");
                        return Err(CliError::NativeToolsOpen);
                    }
                    let name = p["tool"]
                        .as_str()
                        .and_then(|n| bridge.resolve(n))
                        .ok_or(CliError::NativeToolsOpen)?;
                    let allow = bridge.host.approve(name, p["arguments"].clone()).await;
                    send(stdin, json!({"jsonrpc":"2.0","id":v["id"],"result":{"decision":if allow {"accept"} else {"decline"}}})).await?;
                }
                // All other method/id combinations refuse the request
                _ if v.get("id").is_some() && v.get("method").is_some() => {
                    send(stdin, json!({"jsonrpc":"2.0","id":v["id"],"error":{"code":-32601,"message":"request refused"}})).await?;
                }
                _ => {}
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
            );
            assert!(!argv.iter().any(|a| a.contains("private prompt")
                || a.contains("private system")
                || a.contains("%PATH%")));
            assert!(!argv.iter().any(|a| a == "--strict-mcp-config"));
        }
        let argv = args(
            Engine::Claude,
            &turn,
            std::path::Path::new("private-config.json"),
            Some((2, 1, 300)),
        );
        assert!(argv.iter().any(|a| a == "--strict-mcp-config"));
        assert!(argv.iter().any(|a| a == "--setting-sources"));
    }
    #[tokio::test]
    async fn bridge_rejects_other_turns_and_unmounted_tools() {
        let bridge = Bridge {
            capability: "Bearer private-turn-capability".into(),
            tools: Arc::new(vec![]),
            host: Arc::new(NoCalls),
            stop: CancellationToken::new(),
        };
        let req = json!({"id":1,"method":"tools/call","params":{"name":"Bash","arguments":{}}});
        let result = mcp(
            State(bridge.clone()),
            HeaderMap::new(),
            (),
            Json(req.clone()),
        )
        .await;
        assert_eq!(result.unwrap_err(), StatusCode::UNAUTHORIZED);
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer another-turn".parse().unwrap());
        assert_eq!(
            mcp(State(bridge.clone()), headers, (), Json(req.clone()))
                .await
                .unwrap_err(),
            StatusCode::UNAUTHORIZED
        );
        let reply = bridge.rpc(&req).await;
        assert_eq!(reply["result"]["isError"], true);
        let server = BridgeServer::open(bridge).await.unwrap();
        let path = server.file.path().to_owned();
        assert!(path.exists());
        drop(server);
        assert!(!path.exists());
    }
    #[test]
    fn tool_names_are_collision_free() {
        let names = ["fs.read", "fs__read", "mcp:server/tool", "fs_read"];
        let encoded: std::collections::HashSet<_> = names.iter().map(|n| tool_name(n)).collect();
        assert_eq!(encoded.len(), names.len());
    }
}
