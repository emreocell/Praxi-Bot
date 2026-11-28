//! Explicitly invoked, billable smoke test against the signed-in local agy CLI.
//! Run: `cargo test -p cli-provider --test real_agy -- --ignored --nocapture`
use async_trait::async_trait;
use cli_provider::{
    ApprovalLevel, Driver, PathEnv,
    runtime::{
        NativePermission, NativeTurn, ToolReply, Turn, TurnHost, run_native_turn, tool_name,
    },
};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tool_registry::{ContentClass, Effect, PermissionCategory, ToolSpec};

#[derive(Default)]
struct Host {
    listed: AtomicBool,
    called: AtomicBool,
}

#[async_trait]
impl TurnHost for Host {
    fn text(&self, _: &str) {}
    fn tools_listed(&self) {
        self.listed.store(true, Ordering::SeqCst);
    }
    async fn call(&self, name: &str, _: Value) -> ToolReply {
        if name == "praxi_echo" {
            self.called.store(true, Ordering::SeqCst);
        }
        ToolReply {
            text: "hello".into(),
            is_error: false,
        }
    }
    async fn approve(&self, _: &str, _: Value) -> bool {
        false
    }
    async fn permission(&self, request: NativePermission) -> bool {
        is_echo_tool(&request.tool)
    }
}

fn is_echo_tool(tool: &str) -> bool {
    tool.strip_prefix("mcp__praxi__").unwrap_or(tool) == tool_name("praxi_echo")
}

#[test]
fn only_exact_echo_identity_is_auto_approved() {
    let echo = tool_name("praxi_echo");
    assert!(is_echo_tool(&echo));
    assert!(is_echo_tool(&format!("mcp__praxi__{echo}")));
    assert!(!is_echo_tool(&format!("other_{echo}")));
    assert!(!is_echo_tool(&format!("{echo}_other")));
}

#[tokio::test]
#[ignore = "manual, uses the signed-in agy account and incurs model usage"]
async fn real_agy_http_bridge() {
    let workspace = tempfile::tempdir().unwrap();
    let host = Arc::new(Host::default());
    let turn = NativeTurn {
        driver: Driver::Agy,
        level: ApprovalLevel::Ask,
        workspace: workspace.path().to_owned(),
        turn: Turn {
            system: String::new(),
            prompt: format!(
                "Call the available {} tool with message hello.",
                tool_name("praxi_echo")
            ),
            model: Some("gemini-3.8-flash-low".into()),
            tools: vec![ToolSpec {
                name: "praxi_echo".into(),
                description: "Return a harmless greeting.".into(),
                input_schema: json!({"type":"object","properties":{"message":{"type":"string"}}}),
                effect: Effect::Read,
                category: PermissionCategory::Fs,
                platforms: vec![],
                content: ContentClass::none(),
            }],
        },
    };
    let result = tokio::time::timeout(
        Duration::from_secs(120),
        run_native_turn(
            "agy",
            &PathEnv::from_ambient(),
            turn,
            host.clone(),
            CancellationToken::new(),
        ),
    )
    .await;
    let listed = host.listed.load(Ordering::SeqCst);
    let called = host.called.load(Ordering::SeqCst);
    println!("tools/list={listed} tools/call={called} result={result:?}");
    assert!(
        listed && called,
        "agy did not load and call the HTTP bridge"
    );
    assert!(result.is_ok_and(|r| r.is_ok()), "agy turn failed");
}
