//! Tiny stdio MCP server used by the integration tests.
//!
//! Tools: `echo` (read-only), `delete_all` (no annotations), `read_secret`,
//! `fail` (tool-level error), `crash` (exits the process), `env_keys`
//! (names of the environment variables it was started with).

#![allow(clippy::unused_async_trait_impl)]

use std::sync::Arc;

use rmcp::{
    ErrorData, RoleServer, ServerHandler, ServiceExt,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
        JsonObject, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig,
        Tool, ToolAnnotations,
    },
    service::RequestContext,
    transport::stdio,
};
use serde_json::json;

struct TestServer;

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

impl ServerHandler for TestServer {
    fn get_info(&self) -> ServerConfig {
        let config = ServerConfig::new(ServerCapabilities::builder().enable_tools().build());
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
            Ok("secret-error") => {
                return Err(ErrorData::internal_error(
                    std::env::var("API_KEY").unwrap(),
                    None,
                ));
            }
            _ => {}
        }
        let mut tools = vec![
            tool("echo", "Echo the given text back", true),
            tool("delete_all", "Delete everything", false),
            tool("read_secret", "Read a secret value", true),
            tool("fail", "Always returns a tool-level error", false),
            tool("crash", "Terminates the server process", false),
            tool("env_keys", "List the environment variable names", true),
        ];
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
        Ok(ListToolsResult::with_all_items(tools))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
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
            "echo" => CallToolResult::success(vec![ContentBlock::text(text)]),
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
    let service = TestServer.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
