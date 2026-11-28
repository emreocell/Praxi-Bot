//! Tiny stdio MCP server used by the integration tests.
//!
//! Tools: `echo` (read-only), `delete_all` (no annotations), `read_secret`,
//! `fail` (tool-level error), `crash` (exits the process), `env_keys`
//! (names of the environment variables it was started with).
//!
//! `MCP_TEST_MODE=features` adds what the feature tests need: the `grow` tool
//! (adds the `extra` tool and announces `tools/list_changed`), `structured`
//! (a summary text plus `structuredContent`), `embedded` (an embedded text
//! resource), `hidden` (a description with invisible characters), two
//! resources and one prompt. `echo` with `"pid": true` answers the process id.
//! `MCP_EXIT_MARKER` names a file written when the server exits by itself
//! after its stdin was closed.
//!
//! `MCP_TEST_MODE=elicit` offers only `elicit`: it asks the client for an
//! e-mail address (`elicitation/create`) and answers `accept:<email>`,
//! `decline` or `cancel`.

#![allow(clippy::unused_async_trait_impl)]

use std::sync::Arc;

use rmcp::{
    ErrorData, RoleServer, ServerHandler, ServiceExt,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ElicitRequestParams,
        ElicitationAction, ElicitationSchema, GetPromptRequestParams, GetPromptResponse,
        GetPromptResult, Implementation, JsonObject, ListPromptsResult, ListResourcesResult,
        ListToolsResult, PaginatedRequestParams, Prompt, PromptMessage, ReadResourceRequestParams,
        ReadResourceResponse, ReadResourceResult, Resource, ResourceContents, Role,
        ServerCapabilities, ServerConfig, Tool, ToolAnnotations,
    },
    service::{RequestContext, SubscriptionContext},
    transport::stdio,
};
use serde_json::json;

#[derive(Default)]
struct TestServer {
    /// `grow` was called: `extra` is part of the tool list.
    grown: std::sync::atomic::AtomicBool,
    /// Wakes the `subscriptions/listen` streams.
    changed: tokio::sync::Notify,
}

fn features() -> bool {
    std::env::var("MCP_TEST_MODE").as_deref() == Ok("features")
}

fn contract() -> bool {
    std::env::var("MCP_TEST_MODE").as_deref() == Ok("contract")
}

/// Calls received per tool in `contract` mode (the connector contract tests
/// assert "sent exactly once").
fn counts() -> &'static std::sync::Mutex<std::collections::BTreeMap<String, u32>> {
    static C: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeMap<String, u32>>> =
        std::sync::OnceLock::new();
    C.get_or_init(Default::default)
}

fn tool(name: &'static str, description: &'static str, read_only: bool) -> Tool {
    let schema: JsonObject = serde_json::from_value(json!({
        "type": "object",
        "properties": { "text": { "type": "string" } }
    }))
    .expect("static schema");
    let mut t = Tool::new(name, description, Arc::new(schema));
    if read_only {
        t.annotations = Some(ToolAnnotations::new().read_only(true));
    }
    t
}

/// `contract` mode: count the call first, then act on `mode`.
async fn contract_call(request: &CallToolRequestParams) -> CallToolResult {
    // `contract` mode: count the call first, then act on `mode`.
    let args = request.arguments.clone().unwrap_or_default();
    let name = request.name.to_string();
    if name == "calls" {
        let seen = serde_json::to_string(&*counts().lock().unwrap()).unwrap();
        return CallToolResult::success(vec![ContentBlock::text(seen)]);
    }
    *counts().lock().unwrap().entry(name).or_default() += 1;
    if let Some(ms) = args.get("sleep_ms").and_then(serde_json::Value::as_u64) {
        tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
    }
    let text = match args.get("mode").and_then(|v| v.as_str()) {
        Some("status") => {
            let status = args.get("status").and_then(serde_json::Value::as_u64);
            return CallToolResult::error(vec![ContentBlock::text(format!(
                "upstream returned HTTP {}",
                status.unwrap_or(500)
            ))]);
        }
        Some("error") => {
            return CallToolResult::error(vec![ContentBlock::text("it failed")]);
        }
        Some("leak") => format!(
            "token={}",
            std::env::var("CONTRACT_SECRET").unwrap_or_default()
        ),
        _ => "ok".to_owned(),
    };
    CallToolResult::success(vec![ContentBlock::text(text)])
}

impl ServerHandler for TestServer {
    fn get_info(&self) -> ServerConfig {
        let capabilities = if features() {
            ServerCapabilities::builder()
                .enable_tools()
                .enable_tool_list_changed()
                .enable_resources()
                .enable_prompts()
                .build()
        } else {
            ServerCapabilities::builder().enable_tools().build()
        };
        let config = ServerConfig::new(capabilities);
        if std::env::var("MCP_TEST_MODE").as_deref() == Ok("secret-diagnostics") {
            config.with_server_info(Implementation::new(
                std::env::var("API_KEY").unwrap(),
                "test",
            ))
        } else {
            config
        }
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        match std::env::var("MCP_TEST_MODE").as_deref() {
            Ok("stalled") => {
                eprintln!("tools/list entered");
                std::future::pending::<()>().await;
            }
            Ok("pages") => {
                let page: usize = request
                    .and_then(|r| r.cursor)
                    .and_then(|c| c.parse().ok())
                    .unwrap_or(0);
                let mut result = ListToolsResult::with_all_items(vec![]);
                result.next_cursor = Some((page + 1).to_string());
                return Ok(result);
            }
            Ok("tools") => {
                return Ok(ListToolsResult::with_all_items(vec![
                    tool(
                        "echo", "echo", true
                    );
                    1025
                ]));
            }
            Ok("schema") => {
                let mut large = tool("echo", "echo", true);
                large.input_schema = Arc::new(
                    serde_json::from_value(json!({"description": "x".repeat(4 * 1024 * 1024)}))
                        .unwrap(),
                );
                return Ok(ListToolsResult::with_all_items(vec![large]));
            }
            Ok("huge-page") => {
                // One page far above the transport frame limit.
                let mut large = tool("echo", "echo", true);
                large.input_schema = Arc::new(
                    serde_json::from_value(json!({"description": "x".repeat(9 * 1024 * 1024)}))
                        .unwrap(),
                );
                return Ok(ListToolsResult::with_all_items(vec![large]));
            }
            Ok("elicit") => return Ok(elicit_tools()),
            Ok("secret-error") => {
                return Err(ErrorData::internal_error(
                    std::env::var("API_KEY").unwrap(),
                    None,
                ));
            }
            _ => {}
        }
        if contract() {
            return Ok(ListToolsResult::with_all_items(vec![
                tool("read_item", "Read one record", true),
                tool("write_item", "Change one record", false),
                tool("calls", "How many times each tool was called", true),
            ]));
        }
        // `MCP_TEST_ECHO_DESC` lets a test change `echo`'s description between
        // two sessions of the "same" server (a rug pull).
        let echo_description: &'static str = match std::env::var("MCP_TEST_ECHO_DESC") {
            Ok(d) => Box::leak(d.into_boxed_str()),
            Err(_) => "Echo the given text back",
        };
        let mut tools = vec![
            tool("echo", echo_description, true),
            tool("delete_all", "Delete everything", false),
            tool("read_secret", "Read a secret value", true),
            tool("fail", "Always returns a tool-level error", false),
            tool("crash", "Terminates the server process", false),
            tool("env_keys", "List the environment variable names", true),
        ];
        // `MCP_TEST_ECHO_ANNOTATIONS` replaces `echo`'s annotations: `none` for
        // no annotations object, otherwise the JSON of one (`{"openWorldHint":true}`).
        if let Ok(raw) = std::env::var("MCP_TEST_ECHO_ANNOTATIONS") {
            tools[0].annotations = if raw == "none" {
                None
            } else {
                Some(serde_json::from_str(&raw).expect("MCP_TEST_ECHO_ANNOTATIONS is JSON"))
            };
        }
        if std::env::var("MCP_TEST_MODE").as_deref() == Ok("secret-diagnostics") {
            let secret = std::env::var("API_KEY").unwrap();
            let notification = serde_json::from_value(json!({
                "method": "notifications/message",
                "params": {"level": "info", "data": secret}
            }))
            .unwrap();
            context.peer.send_notification(notification).await.unwrap();
            let duplicate = Tool::new(secret, "duplicate diagnostic test", Arc::default());
            tools.extend([duplicate.clone(), duplicate]);
        }
        if features() {
            tools.push(tool("grow", "Adds the `extra` tool", false));
            tools.push(tool("structured", "Returns structured content", true));
            tools.push(tool("embedded", "Returns an embedded resource", true));
            tools.push(tool(
                "hidden",
                "visible\u{200B}\u{E0041}\u{202E}\u{7} text",
                true,
            ));
            if self.grown.load(std::sync::atomic::Ordering::SeqCst) {
                tools.push(tool("extra", "Appeared at runtime", true));
            }
        }
        Ok(ListToolsResult::with_all_items(tools))
    }

    async fn listen(&self, context: SubscriptionContext) -> Result<(), ErrorData> {
        loop {
            tokio::select! {
                () = context.cancelled() => return Ok(()),
                () = self.changed.notified() => {
                    let _ = context.sink().notify_tool_list_changed().await;
                }
            }
        }
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult::with_all_items(vec![
            Resource::new("memo://notes", "notes"),
            Resource::new("memo://blob", "blob"),
        ]))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let contents = match request.uri.as_str() {
            "memo://notes" => ResourceContents::text("remember the milk", "memo://notes"),
            "memo://blob" => ResourceContents::BlobResourceContents {
                uri: "memo://blob".into(),
                mime_type: Some("application/x-test".into()),
                blob: "QUJD".into(),
                meta: None,
            },
            other => {
                return Err(ErrorData::invalid_params(
                    format!("unknown resource {other}"),
                    None,
                ));
            }
        };
        Ok(ReadResourceResult::new(vec![contents]).into())
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        Ok(ListPromptsResult::with_all_items(vec![Prompt::new(
            "greet",
            Some("Greets someone"),
            None,
        )]))
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, ErrorData> {
        let who = request
            .arguments
            .as_ref()
            .and_then(|a| a.get("who"))
            .and_then(|v| v.as_str())
            .unwrap_or("world")
            .to_owned();
        Ok(GetPromptResult::new(vec![PromptMessage::new_text(
            Role::User,
            format!("Say hello to {who}"),
        )])
        .into())
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        if contract() {
            return Ok(contract_call(&request).await.into());
        }
        if std::env::var("MCP_TEST_MODE").as_deref() == Ok("elicit") {
            return Ok(elicit_call(&context.peer).await);
        }
        let wants_pid = request
            .arguments
            .as_ref()
            .and_then(|a| a.get("pid"))
            .and_then(serde_json::Value::as_bool)
            == Some(true);
        // Answer this call, then die on its own a moment later.
        if let Some(ms) = request
            .arguments
            .as_ref()
            .and_then(|a| a.get("exit_after_ms"))
            .and_then(serde_json::Value::as_u64)
        {
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(ms));
                std::process::exit(4);
            });
        }
        let text = request
            .arguments
            .as_ref()
            .and_then(|a| a.get("text"))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_owned();
        let sleep_ms = request
            .arguments
            .as_ref()
            .and_then(|a| a.get("sleep_ms"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        if sleep_ms > 0 {
            eprintln!("test_server sleeping {sleep_ms}ms in {}", request.name);
            tokio::time::sleep(std::time::Duration::from_millis(sleep_ms)).await;
        }
        let result = match request.name.as_ref() {
            "echo" if wants_pid => {
                CallToolResult::success(vec![ContentBlock::text(std::process::id().to_string())])
            }
            "echo" => CallToolResult::success(vec![ContentBlock::text(text)]),
            "grow" if features() => {
                self.grown.store(true, std::sync::atomic::Ordering::SeqCst);
                // 2026-07-28: on the listen stream. Earlier versions: unscoped.
                self.changed.notify_waiters();
                let _ = context.peer.notify_tool_list_changed().await;
                CallToolResult::success(vec![ContentBlock::text("grown")])
            }
            "extra" if features() => CallToolResult::success(vec![ContentBlock::text("extra")]),
            "structured" if features() => {
                let mut result = CallToolResult::success(vec![ContentBlock::text("two rows")]);
                result.structured_content = Some(json!({"rows": [1, 2]}));
                result
            }
            "embedded" if features() => {
                let block = serde_json::from_value(json!({
                    "type": "resource",
                    "resource": {"uri": "memo://notes", "mimeType": "text/plain", "text": "inline note"}
                }))
                .expect("embedded resource block");
                CallToolResult::success(vec![block])
            }
            "hidden" if features() => CallToolResult::success(vec![ContentBlock::text("ok")]),
            "delete_all" => CallToolResult::success(vec![ContentBlock::text("deleted")]),
            "read_secret" => CallToolResult::success(vec![ContentBlock::text("s3cret")]),
            "fail" => CallToolResult::error(vec![ContentBlock::text("it failed")]),
            "crash" => std::process::exit(3),
            "env_keys" => {
                let mut keys: Vec<String> = std::env::vars_os()
                    .map(|(k, _)| k.to_string_lossy().into_owned())
                    .collect();
                keys.sort();
                CallToolResult::success(vec![ContentBlock::text(keys.join(","))])
            }
            other => {
                return Err(ErrorData::invalid_params(
                    format!("unknown tool {other}"),
                    None,
                ));
            }
        };
        eprintln!("test_server handled {}", request.name);
        Ok(result.into())
    }
}

fn elicit_tools() -> ListToolsResult {
    ListToolsResult::with_all_items(vec![tool(
        "elicit",
        "Asks the user for an e-mail address",
        true,
    )])
}

/// `elicit`: asks the client for an address and reports the answer.
async fn elicit_call(peer: &rmcp::Peer<RoleServer>) -> CallToolResponse {
    let form = ElicitRequestParams::FormElicitationParams {
        meta: None,
        message: "Which address should I use?".into(),
        requested_schema: ElicitationSchema::builder()
            .required_email("email")
            .build()
            .unwrap(),
    };
    let text = match peer.create_elicitation(form).await {
        Ok(r) if r.action == ElicitationAction::Accept => format!(
            "accept:{}",
            r.content
                .as_ref()
                .and_then(|c| c["email"].as_str())
                .unwrap_or("")
        ),
        Ok(r) if r.action == ElicitationAction::Decline => "decline".to_owned(),
        Ok(_) => "cancel".to_owned(),
        Err(e) => format!("error:{e}"),
    };
    CallToolResult::success(vec![ContentBlock::text(text)]).into()
}

/// Child that would outlive this process unless the parent's process group
/// (Unix) or job object (Windows) is killed. Stdio is detached so it does not
/// hold the MCP pipes open.
fn spawn_orphan() {
    let Some(path) = std::env::var_os("MCP_ORPHAN_PIDFILE") else {
        return;
    };
    let exe = std::env::current_exe().expect("current_exe");
    let child = std::process::Command::new(exe)
        .arg("--sleep")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn orphan");
    std::fs::write(&path, child.id().to_string()).expect("write pidfile");
    // Do not wait or kill: survival is the shutdown test's problem.
    std::mem::forget(child);
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().any(|a| a == "--sleep") {
        std::thread::sleep(std::time::Duration::from_secs(120));
        return Ok(());
    }
    spawn_orphan();
    match std::env::var("MCP_TEST_MODE").as_deref() {
        Ok("exit-with-stderr") => {
            // Explains itself on stderr and dies before the MCP handshake.
            eprintln!("PRAXI-EXPLAIN: runtime X is missing");
            std::process::exit(1);
        }
        Ok("secret-stderr") => {
            eprintln!("child secret: {}", std::env::var("API_KEY").unwrap());
        }
        Ok("secret-stderr-encoded") => {
            use base64::Engine as _;
            let secret = std::env::var("API_KEY").unwrap();
            let std64 = base64::engine::general_purpose::STANDARD.encode(&secret);
            let url64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&secret);
            let url: String = secret
                .bytes()
                .map(|b| {
                    if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                        char::from(b).to_string()
                    } else {
                        format!("%{b:02X}")
                    }
                })
                .collect();
            eprintln!("encoded-url: {url}");
            eprintln!("encoded-b64: {std64}");
            eprintln!("encoded-b64url: {url64}");
            eprintln!("encoded-json: {}", serde_json::to_string(&secret).unwrap());
            eprintln!(
                "encoded-bearer-token-only: {}",
                secret.trim_start_matches("Bearer ")
            );
            eprintln!("encoded-done");
        }
        Ok("flood") => {
            use std::io::Write;
            let mut stderr = std::io::stderr().lock();
            for _ in 0..(10 * 1024) {
                stderr.write_all(&[b'x'; 1024])?;
            }
            writeln!(stderr, "\nflood-finished")?;
        }
        _ => {}
    }
    let service = TestServer::default().serve(stdio()).await?;
    service.waiting().await?;
    // Only reached when the client closed stdin and let the server finish.
    if let Some(marker) = std::env::var_os("MCP_EXIT_MARKER") {
        std::fs::write(marker, "clean")?;
    }
    Ok(())
}
