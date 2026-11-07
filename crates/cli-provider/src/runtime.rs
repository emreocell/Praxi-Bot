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
    Json(req): Json<Value>,
) -> Result<(StatusCode, Json<Value>), StatusCode> {
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

fn args(
    engine: Engine,
    turn: &Turn,
    config: &std::path::Path,
    version: Option<(u32, u32, u32)>,
) -> Vec<String> {
    let mut args: Vec<String> = match engine {
        Engine::Claude => [
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
        .collect(),
        Engine::Codex => vec![
            "app-server".into(),
            "-c".into(),
            "features.shell_tool=false".into(),
            "-c".into(),
            "features.apply_patch_freeform=false".into(),
        ],
    };
    if engine == Engine::Claude {
        args.push(
            turn.tools
                .iter()
                .map(|s| format!("mcp__praxi__{}", tool_name(&s.name)))
                .collect::<Vec<_>>()
                .join(","),
        );
        args.extend(["--mcp-config".into(), config.to_string_lossy().into_owned()]);
        // Isolate hooks and user/project settings without copying login files.
        args.extend(["--settings".into(), "{\"disableAllHooks\":true}".into()]);
        if version.is_some_and(|v| v >= (1, 0, 122)) {
            args.extend(["--setting-sources".into(), String::new()]);
        }
        if version.is_some_and(|v| v >= (1, 0, 60)) {
            args.push("--strict-mcp-config".into());
        }
    }
    if let Some(model) = &turn.model {
        match engine {
            Engine::Claude => args.extend(["--model".into(), model.clone()]),
            Engine::Codex => args.extend([
                "-c".into(),
                format!("model={}", serde_json::to_string(model).expect("string")),
            ]),
        }
    }
    args
}

async fn send(stdin: &mut tokio::process::ChildStdin, value: Value) -> Result<(), CliError> {
    let mut bytes = serde_json::to_vec(&value).map_err(|_| CliError::Protocol)?;
    bytes.push(b'\n');
    stdin.write_all(&bytes).await?;
    stdin.flush().await?;
    Ok(())
}

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
        None
    };
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
    let mut cmd = tokio::process::Command::new(&resolved.program);
    cmd.args(&resolved.args).args(argv);
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
                "approvalPolicy":"on-request","sandbox":"read-only",
                "config":{
                    "features.shell_tool":false,"features.apply_patch_freeform":false,
                    "features.collab":false,"web_search":"disabled",
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
                        _ => return Err(CliError::NativeToolsOpen),
                    }
                }
                "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
                    return Err(CliError::NativeToolsOpen);
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
        let result = mcp(State(bridge.clone()), HeaderMap::new(), Json(req.clone())).await;
        assert_eq!(result.unwrap_err(), StatusCode::UNAUTHORIZED);
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer another-turn".parse().unwrap());
        assert_eq!(
            mcp(State(bridge.clone()), headers, Json(req.clone()))
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
