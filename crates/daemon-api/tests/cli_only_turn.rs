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

async fn run_failing_turn(root: &std::path::Path) -> Value {
    let app = build_cli_only_app(root);

    let (status, enabled) = request(
        &app,
        Method::POST,
        "/v1/providers/cli/enable",
        Some(json!({ "id": "claude" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{enabled}");

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

    for _ in 0..400 {
        let (status, run) = request(&app, Method::GET, &format!("/v1/runs/{run_id}"), None).await;
        assert_eq!(status, StatusCode::OK, "{run}");
        let state_str = run["state"].as_str().unwrap_or("");
        if matches!(state_str, "succeeded" | "failed" | "cancelled") {
            return run;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("run {run_id} did not finish in time");
}

#[test]
fn cli_only_missing_and_signed_out_surface_actionable_errors() {
    if let Ok(mode) = std::env::var("DAEMON_CLI_FAIL_CHILD") {
        let root = std::path::PathBuf::from(std::env::var("DAEMON_CLI_ROOT").unwrap());
        let run = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_failing_turn(&root));
        assert_eq!(run["state"], "failed", "{run}");
        let err = run["error"].as_str().unwrap_or("");
        match mode.as_str() {
            "missing" => {
                assert!(err.contains("not_installed"), "{err}");
                assert!(err.contains("Install or update Claude Code"), "{err}");
            }
            "signed-out" => {
                assert!(err.contains("signed_out"), "{err}");
                assert!(
                    err.contains("Sign in by running `claude` in a terminal"),
                    "{err}"
                );
            }
            other => panic!("unexpected mode: {other}"),
        }
        return;
    }

    // 1. Missing claude binary on PATH.
    let missing_dir = tempfile::tempdir().unwrap();
    let empty_bin = missing_dir.path().join("empty-bin");
    std::fs::create_dir_all(&empty_bin).unwrap();
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "cli_only_missing_and_signed_out_surface_actionable_errors",
            "--nocapture",
        ])
        .env("DAEMON_CLI_FAIL_CHILD", "missing")
        .env("DAEMON_CLI_ROOT", missing_dir.path())
        .env("PATH", &empty_bin)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "missing child failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // 2. Installed fixture CLI that reports signed-out at turn time.
    let signed_out_dir = tempfile::tempdir().unwrap();
    let bin_dir = signed_out_dir.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::fs::copy(
        env!("CARGO_BIN_EXE_daemon-cli-fixture"),
        bin_dir.join(format!("claude{}", std::env::consts::EXE_SUFFIX)),
    )
    .unwrap();
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "cli_only_missing_and_signed_out_surface_actionable_errors",
            "--nocapture",
        ])
        .env("DAEMON_CLI_FAIL_CHILD", "signed-out")
        .env("DAEMON_CLI_ROOT", signed_out_dir.path())
        .env("PATH", &bin_dir)
        .env("FAKE_MODE", "signed-out")
        .env("FAKE_LOG", signed_out_dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "signed-out child failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn enable_cli_provider_waits_for_cross_process_lock_without_blocking_health() {
    if let Ok(lock_dir) = std::env::var("DAEMON_CLI_LOCK_HOLD_DIR") {
        let cfg_path = provider_catalog::config_path(std::path::Path::new(&lock_dir));
        let _guard = provider_catalog::ProviderConfig::lock(&cfg_path).unwrap();
        let ready = std::env::var("DAEMON_CLI_LOCK_READY").unwrap();
        std::fs::write(&ready, "locked").unwrap();
        std::thread::sleep(Duration::from_millis(300));
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let ready = dir.path().join("ready");
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "enable_cli_provider_waits_for_cross_process_lock_without_blocking_health",
            "--nocapture",
        ])
        .env("DAEMON_CLI_LOCK_HOLD_DIR", &data_dir)
        .env("DAEMON_CLI_LOCK_READY", &ready)
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !ready.exists() {
        assert!(std::time::Instant::now() < deadline, "child did not lock");
        std::thread::sleep(Duration::from_millis(10));
    }

    let app = build_cli_only_app(dir.path());
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let app_enable = app.clone();
            let enable_fut = tokio::spawn(async move {
                request(
                    &app_enable,
                    Method::POST,
                    "/v1/providers/cli/enable",
                    Some(json!({ "id": "claude" })),
                )
                .await
            });
            // While enable_cli_provider is waiting on providers.lock in
            // spawn_blocking, /v1/health stays responsive on the runtime.
            let (health_status, _) = request(&app, Method::GET, "/v1/health", None).await;
            assert_eq!(health_status, StatusCode::OK);
            let (status, body) = enable_fut.await.unwrap();
            assert_eq!(status, StatusCode::OK, "{body}");
        });

    assert!(child.wait().unwrap().success());
}
