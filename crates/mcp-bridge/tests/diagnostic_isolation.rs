//! A separate test process allows a global subscriber to observe dependency
//! tasks that do not inherit the caller's thread-local tracing dispatcher.

use std::{sync::Arc, time::Duration};

use mcp_bridge::{Backoff, McpManager, McpPolicy, McpServerConfig, McpTransport};
use secret_store::{MemoryStore, SecretStore};
use serde_json::json;
use tool_registry::{ContentPart, ToolContext, ToolRegistry};

#[path = "support/capture.rs"]
mod capture;
#[path = "support/example_bin.rs"]
mod example_bin;

#[tokio::test(flavor = "multi_thread")]
async fn dependency_payload_logs_cannot_escape_to_the_global_subscriber() {
    const SECRET: &str = "mcp-protocol-sentinel-7f0294b1";
    let capture = capture::Capture::default();
    tracing::subscriber::set_global_default(capture.clone()).unwrap();
    tracing::info!("application tracing remains enabled");

    let example = example_bin::test_server();
    let directory = tempfile::tempdir().unwrap();
    let unicode = directory.path().join("Masaüstü-秘密");
    std::fs::create_dir(&unicode).unwrap();
    let executable = unicode.join(format!("test_server{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(example, &executable).unwrap();

    let registry = ToolRegistry::new();
    let store = Arc::new(MemoryStore::new());
    store.set("test/secret", SECRET).unwrap();
    let manager = McpManager::with_policy(
        registry.clone(),
        McpPolicy {
            backoff: Backoff {
                max_attempts: 1,
                ..Backoff::default()
            },
            connect_timeout: Duration::from_secs(20),
            ..McpPolicy::default()
        },
    )
    .with_secret_store(store);
    let mut config = McpServerConfig::stdio("protocol", executable.to_str().unwrap(), [""; 0]);
    config
        .secret_env
        .insert("API_KEY".into(), "test/secret".into());
    if let McpTransport::Stdio { env, .. } = &mut config.transport {
        env.insert("MCP_TEST_MODE".into(), "secret-diagnostics".into());
    }
    assert_eq!(manager.add_server(config).await.unwrap(), 7);

    // Initialization metadata, notifications and protocol values can contain
    // credentials. Containing dependency logs must not mutate tool results.
    let result = registry
        .get("mcp:protocol/echo")
        .unwrap()
        .call(&ToolContext::default(), json!({"text": SECRET}))
        .await
        .unwrap();
    assert!(
        result
            .content
            .iter()
            .any(|part| { matches!(part, ContentPart::Text { text } if text == SECRET) })
    );
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if manager
                .stderr_tail("protocol")
                .iter()
                .any(|line| line.contains("test_server handled echo"))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("child diagnostics were not forwarded");
    let stderr = manager.stderr_tail("protocol").join("\n");
    manager.shutdown().await;
    drop(manager);
    drop(registry);

    // Explicitly delete after completion: Windows will refuse this while the
    // copied executable or its directory is still held by the child process.
    directory.close().unwrap();
    let trace = capture.0.lock().unwrap().clone();
    assert!(!trace.contains(SECRET), "raw secret in dependency tracing");
    assert!(trace.contains("application tracing remains enabled"));
    assert!(trace.contains("test_server handled echo"));
    assert!(trace.contains("[REDACTED]"));
    assert!(trace.contains("MCP tool name already registered; skipped"));
    assert!(!stderr.contains(SECRET), "raw secret in captured stderr");
}
