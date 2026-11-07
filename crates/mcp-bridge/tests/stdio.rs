//! End-to-end tests against the stdio `test_server` example.

use std::{path::PathBuf, time::Duration};

use mcp_bridge::{
    Backoff, McpManager, McpPolicy, McpServerConfig, McpTransport, OS_ESSENTIAL_ENV, ServerStatus,
};
use serde_json::json;
use tool_registry::{ContentPart, Effect, ToolContext, ToolRegistry};

fn server_exe() -> PathBuf {
    // target/debug/deps/<test>.exe -> target/debug/examples/test_server[.exe]
    let mut p = std::env::current_exe().unwrap();
    p.pop();
    p.pop();
    p.push("examples");
    p.push(format!("test_server{}", std::env::consts::EXE_SUFFIX));
    assert!(p.exists(), "build the example first: {}", p.display());
    p
}

fn config(id: &str) -> McpServerConfig {
    McpServerConfig::stdio(id, server_exe().to_string_lossy(), [""; 0])
}

fn fast_policy(quarantine_after: u32) -> McpPolicy {
    McpPolicy {
        quarantine_after,
        backoff: Backoff {
            initial: Duration::from_millis(10),
            max: Duration::from_millis(20),
            max_attempts: 2,
        },
        connect_timeout: Duration::from_secs(20),
        call_timeout: Duration::from_secs(20),
    }
}

fn text_of(parts: &[ContentPart]) -> String {
    parts
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

async fn call(reg: &ToolRegistry, name: &str, input: serde_json::Value) -> Result<String, String> {
    let tool = reg
        .get(name)
        .unwrap_or_else(|| panic!("{name} not registered"));
    tool.call(&ToolContext::default(), input)
        .await
        .map(|o| text_of(&o.content))
        .map_err(|e| e.to_string())
}

#[tokio::test(flavor = "multi_thread")]
async fn lists_registers_and_calls_tools() {
    let reg = ToolRegistry::new();
    let mgr = McpManager::with_policy(reg.clone(), fast_policy(3));
    assert_eq!(mgr.add_server(config("t")).await.unwrap(), 6);

    let echo = reg.get("mcp:t/echo").unwrap();
    assert_eq!(echo.spec().effect, Effect::Read);
    assert_eq!(
        echo.spec().input_schema["properties"]["text"]["type"],
        "string"
    );
    // No annotations -> Destructive (ADR-0004).
    assert_eq!(
        reg.get("mcp:t/delete_all").unwrap().spec().effect,
        Effect::Destructive
    );

    assert_eq!(
        call(&reg, "mcp:t/echo", json!({"text": "hello"}))
            .await
            .unwrap(),
        "hello"
    );
    // Tool-level errors surface as errors but do not count as failures.
    assert!(
        call(&reg, "mcp:t/fail", json!({}))
            .await
            .unwrap_err()
            .contains("it failed")
    );
    let snap = mgr.snapshot("t").unwrap();
    assert_eq!(snap.status, ServerStatus::Connected);
    assert_eq!(snap.consecutive_failures, 0);

    // stderr is captured.
    let mut seen = false;
    for _ in 0..50 {
        if mgr
            .stderr_tail("t")
            .iter()
            .any(|l| l.contains("test_server handled"))
        {
            seen = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(seen, "stderr not captured: {:?}", mgr.stderr_tail("t"));

    mgr.remove_server("t").await.unwrap();
    assert!(reg.get("mcp:t/echo").is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn allow_and_exclude_filter_tools() {
    let reg = ToolRegistry::new();
    let mgr = McpManager::with_policy(reg.clone(), fast_policy(3));
    let mut cfg = config("f");
    cfg.tool_allow = vec!["e*".into(), "read_*".into()];
    cfg.tool_exclude = vec!["read_secret".into()];
    assert_eq!(mgr.add_server(cfg).await.unwrap(), 2);
    let mut names: Vec<_> = reg.list().into_iter().map(|s| s.name).collect();
    names.sort();
    assert_eq!(names, ["mcp:f/echo", "mcp:f/env_keys"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn child_gets_only_declared_environment() {
    let reg = ToolRegistry::new();
    let mgr = McpManager::with_policy(reg.clone(), fast_policy(3));
    let mut cfg = config("e");
    if let McpTransport::Stdio { env, .. } = &mut cfg.transport {
        env.insert("MCP_TEST_DECLARED".into(), "1".into());
    }
    mgr.add_server(cfg).await.unwrap();
    // The test process itself has CARGO_MANIFEST_DIR / PATH etc.
    assert!(std::env::var_os("CARGO_MANIFEST_DIR").is_some());
    let keys = call(&reg, "mcp:e/env_keys", json!({})).await.unwrap();
    let got: Vec<String> = keys
        .split(',')
        .filter(|k| !k.is_empty())
        .map(str::to_owned)
        .collect();
    for k in &got {
        assert!(
            k == "MCP_TEST_DECLARED" || OS_ESSENTIAL_ENV.iter().any(|e| e.eq_ignore_ascii_case(k)),
            "undeclared variable leaked into the child: {k} (all: {got:?})"
        );
    }
    assert!(got.iter().any(|k| k == "MCP_TEST_DECLARED"));
}

#[tokio::test(flavor = "multi_thread")]
async fn quarantine_after_consecutive_call_failures_and_manual_reenable() {
    let reg = ToolRegistry::new();
    let mgr = McpManager::with_policy(reg.clone(), fast_policy(2));
    mgr.add_server(config("q")).await.unwrap();

    // 1st crash: failure counted, server reconnects lazily.
    assert!(call(&reg, "mcp:q/crash", json!({})).await.is_err());
    let snap = mgr.snapshot("q").unwrap();
    assert_eq!(snap.consecutive_failures, 1);
    assert_ne!(snap.status, ServerStatus::Quarantined);
    // A working call in between resets the counter.
    assert_eq!(
        call(&reg, "mcp:q/echo", json!({"text": "x"}))
            .await
            .unwrap(),
        "x"
    );
    assert_eq!(mgr.snapshot("q").unwrap().consecutive_failures, 0);

    // Two consecutive crashes -> quarantined, tools hidden.
    assert!(call(&reg, "mcp:q/crash", json!({})).await.is_err());
    let echo = reg.get("mcp:q/echo").unwrap();
    assert!(call(&reg, "mcp:q/crash", json!({})).await.is_err());
    let snap = mgr.snapshot("q").unwrap();
    assert_eq!(snap.status, ServerStatus::Quarantined);
    assert_eq!(snap.tools.len(), 0);
    assert_eq!(reg.list().len(), 0, "quarantined tools must be hidden");

    // A stale handle is refused too.
    let err = echo
        .call(&ToolContext::default(), json!({"text": "x"}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("quarantined"), "{err}");
    assert!(mgr.connect("q").await.is_err());

    // Manual re-enable restores everything.
    assert_eq!(mgr.reenable("q").await.unwrap(), 6);
    assert_eq!(mgr.snapshot("q").unwrap().status, ServerStatus::Connected);
    assert_eq!(
        call(&reg, "mcp:q/echo", json!({"text": "back"}))
            .await
            .unwrap(),
        "back"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn connect_failures_quarantine_and_disabled_servers_stay_idle() {
    let reg = ToolRegistry::new();
    let mgr = McpManager::with_policy(reg.clone(), fast_policy(2));
    let bad = McpServerConfig::stdio("bad", "definitely-not-a-real-binary-xyz", [""; 0]);
    assert!(mgr.add_server(bad).await.is_err());
    assert_eq!(mgr.snapshot("bad").unwrap().consecutive_failures, 1);
    assert!(mgr.connect("bad").await.is_err());
    assert_eq!(
        mgr.snapshot("bad").unwrap().status,
        ServerStatus::Quarantined
    );
    assert!(
        mgr.connect("bad")
            .await
            .unwrap_err()
            .to_string()
            .contains("quarantined")
    );

    let mut off = config("off");
    off.enabled = false;
    assert_eq!(mgr.add_server(off).await.unwrap(), 0);
    assert_eq!(mgr.snapshot("off").unwrap().status, ServerStatus::Disabled);
    assert_eq!(reg.list().len(), 0);

    // Duplicate ids are rejected.
    assert!(mgr.add_server(config("off")).await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn discovery_surface_fronts_a_large_mcp_server() {
    use agent_router::{FIND_TOOL, SurfaceMode, ToolSurface, USE_TOOL};

    let reg = ToolRegistry::new();
    let mgr = McpManager::with_policy(reg.clone(), fast_policy(3));
    mgr.add_server(config("d")).await.unwrap();

    // 6 MCP tools: above a threshold of 3 the model only sees the meta-tools.
    let surface = ToolSurface::new(reg.clone(), 3);
    assert_eq!(surface.mode(), SurfaceMode::Discovery);
    assert_eq!(surface.specs().len(), 3);
    // Below the threshold they would be passed directly.
    assert_eq!(
        ToolSurface::new(reg.clone(), 10).mode(),
        SurfaceMode::Direct
    );

    let ctx = ToolContext::default();
    let find = surface.resolve(FIND_TOOL).unwrap();
    let hits = find
        .call(&ctx, json!({"query": "echo text back", "k": 2}))
        .await
        .unwrap();
    let ContentPart::Json { data } = &hits.content[0] else {
        panic!("expected json")
    };
    assert_eq!(data[0]["name"], "mcp:d/echo");

    let out = surface
        .resolve(USE_TOOL)
        .unwrap()
        .call(
            &ctx,
            json!({"name": "mcp:d/echo", "input": {"text": "via meta"}}),
        )
        .await
        .unwrap();
    assert_eq!(text_of(&out.content), "via meta");
}
