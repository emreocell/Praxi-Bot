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
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, JsonObject,
        ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
        ToolAnnotations,
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
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(vec![
            tool("echo", "Echo the given text back", true),
            tool("delete_all", "Delete everything", false),
            tool("read_secret", "Read a secret value", true),
            tool("fail", "Always returns a tool-level error", false),
            tool("crash", "Terminates the server process", false),
            tool("env_keys", "List the environment variable names", true),
        ]))
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let service = TestServer.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
