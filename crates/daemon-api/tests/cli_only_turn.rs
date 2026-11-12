//! Integration test verifying a CLI-only daemon (no HTTP provider) can enable
//! `cli:claude` through `POST /v1/providers/cli/enable`, leave `setup_required`,
//! and run a full chat turn via the hermetic CLI fixture binary.
use std::sync::Arc;
use std::time::Duration;

use agent_core::{AgentLoop, LoopConfig, Providers, build_providers};
use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use daemon_api::{AppState, DaemonConfig, ProviderHost, router};
use http_body_util::BodyExt as _;
use protocol::PermissionCategory;
use provider_catalog::Catalog;
use serde_json::{Value, json};
use storage::Storage;
use tool_registry::{
    Effect, Platform, Tool, ToolContext, ToolError, ToolOutput, ToolRegistry, ToolSpec,
};
use tower::ServiceExt as _;

const TOKEN: &str = "cli-only-test-token-0123456789abcdef";

struct FixtureReadTool {
    spec: ToolSpec,
}

#[async_trait]
impl Tool for FixtureReadTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, _: &ToolContext, _: Value) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::text("fixture tool ok"))
    }
}

async fn request(
    app: &Router,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let bytes = body.map_or_else(Vec::new, |v| serde_json::to_vec(&v).unwrap());
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "127.0.0.1:47821")
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(Body::from(bytes))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let raw = resp.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&raw).unwrap_or(Value::Null);
    (status, json)
}

fn build_cli_only_app(root: &std::path::Path) -> Router {
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let storage = Arc::new(Storage::open(&data_dir).unwrap());
    let tools = ToolRegistry::new();
    tools.register(Arc::new(FixtureReadTool {
        spec: ToolSpec {
            name: "fixture_read".into(),
            description: "fixture read tool".into(),
            input_schema: json!({"type": "object"}),
            effect: Effect::Read,
            category: PermissionCategory::Fs,
            platforms: vec![Platform::All],
        },
    }));

    let agent = AgentLoop::new(
        Arc::clone(&storage),
        tools,
        Providers::unconfigured(),
        LoopConfig::default(),
    );
    let state = AppState::new(agent, storage, DaemonConfig::new(TOKEN)).unwrap();
    let reload_dir = data_dir;
    state.set_provider_host(ProviderHost {
        catalog: Catalog::default(),
        reload: Box::new(move || {
            build_providers(
                &reload_dir,
                &Catalog::default(),
                &secret_store::MemoryStore::new(),
                &mut |_| {},
            )
        }),
    });
    router(state)
}

async fn run_child_turn() {
    let root = std::path::PathBuf::from(std::env::var("DAEMON_CLI_ROOT").unwrap());
    let app = build_cli_only_app(&root);

    let (status, health) = request(&app, Method::GET, "/v1/health", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(health["status"], "setup_required");

    let (status, enabled) = request(
        &app,
        Method::POST,
        "/v1/providers/cli/enable",
        Some(json!({ "id": "claude" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{enabled}");
    assert_eq!(enabled["id"], "cli:claude");
    assert_eq!(enabled["active"], true);

    let (status, health) = request(&app, Method::GET, "/v1/health", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(health["status"], "ok", "{health}");

    let (status, providers) = request(&app, Method::GET, "/v1/providers", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(providers["active"], json!(["cli:claude"]));
    assert_eq!(providers["default_provider"], "cli:claude");

    let profile_toml = "\
[bot]
id = \"cli-only\"
name = \"CLI Only\"

[models]
executor = \"cli:claude/default\"
";
    let (status, bot) = request(
        &app,
        Method::POST,
        "/v1/bots",
        Some(json!({ "profile_toml": profile_toml })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{bot}");
    let bot_id = bot["id"].as_str().unwrap().to_owned();

    let (status, chat) = request(
        &app,
        Method::POST,
        "/v1/chats",
        Some(json!({ "bot_id": bot_id })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{chat}");
    let chat_id = chat["id"].as_str().unwrap().to_owned();

    let (status, accepted) = request(
        &app,
        Method::POST,
        &format!("/v1/chats/{chat_id}/messages"),
        Some(json!({ "text": "say hello", "mode": "steer" })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
    let run_id = accepted["run_id"].as_str().unwrap().to_owned();

    let mut final_run = Value::Null;
    for _ in 0..400 {
        let (status, run) = request(&app, Method::GET, &format!("/v1/runs/{run_id}"), None).await;
        assert_eq!(status, StatusCode::OK, "{run}");
        let state_str = run["state"].as_str().unwrap_or("");
        if matches!(state_str, "succeeded" | "failed" | "cancelled") {
            final_run = run;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(final_run["state"], "succeeded", "{final_run}");

    let (status, msgs) = request(
        &app,
        Method::GET,
        &format!("/v1/chats/{chat_id}/messages"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{msgs}");
    let has_hello = msgs.as_array().unwrap().iter().any(|m| {
        m["role"] == "assistant"
            && m["content"]
                .as_str()
                .is_some_and(|text| text.contains("hello"))
    });
    assert!(
        has_hello,
        "expected assistant reply containing 'hello': {msgs}"
    );
}

#[test]
fn cli_only_enable_and_chat_turn_end_to_end() {
    if std::env::var("DAEMON_CLI_TURN_CHILD").ok().as_deref() == Some("1") {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_child_turn());
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let bin_dir = dir.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::fs::copy(
        env!("CARGO_BIN_EXE_daemon-cli-fixture"),
        bin_dir.join(format!("claude{}", std::env::consts::EXE_SUFFIX)),
    )
    .unwrap();

    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "cli_only_enable_and_chat_turn_end_to_end",
            "--nocapture",
        ])
        .env("DAEMON_CLI_TURN_CHILD", "1")
        .env("DAEMON_CLI_ROOT", dir.path())
        .env("PATH", &bin_dir)
        .env("FAKE_LOG", dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
