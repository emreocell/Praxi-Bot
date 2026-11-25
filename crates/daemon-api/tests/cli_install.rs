use std::{path::Path, sync::Arc};

use agent_core::{AgentLoop, LoopConfig, Providers};
use axum::{
    Router,
    body::Body,
    http::{Method, Request, StatusCode},
};
use daemon_api::{AppState, DaemonConfig, router};
use http_body_util::BodyExt as _;
use serde_json::{Value, json};
use storage::Storage;
use tool_registry::ToolRegistry;
use tower::ServiceExt as _;

#[path = "../../cli-provider/tests/common/mod.rs"]
mod common;

const TOKEN: &str = "install-fixture-owner-0123456789abcdef";

const OWNER_KEY: &str = "ownerkey0123456789abcdef0123456789abcdef0123456789abcdef";

async fn request(app: &Router, method: Method, path: &str, body: Value) -> (StatusCode, String) {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "127.0.0.1:47821")
        .header("authorization", format!("Bearer {TOKEN}"))
        .header("x-praxi-owner-key", OWNER_KEY)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

#[test]
fn durable_audit_precedes_real_spawn_and_completion_refreshes_cached_detection() {
    if let Ok(root) = std::env::var("DAEMON_INSTALL_FIXTURE_ROOT") {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(exercise_install(Path::new(&root)));
        return;
    }
    let binary = Path::new(env!("CARGO_BIN_EXE_daemon-install-fixture"));
    let root = common::fixture_dir(binary);
    let bin = root.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    // Every engine name is intercepted, including GUI fallback lookup. No
    // installed vendor CLI or credentials are used in this child test process.
    for name in [
        "npm",
        "claude",
        "codex",
        "cursor-agent",
        "opencode",
        "gemini",
        "agy",
        "qwen",
        "kiro-cli",
        "qodercli",
        "copilot",
        "kimi",
        "vibe-acp",
    ] {
        common::install_binary(
            binary,
            &bin.join(format!("{name}{}", std::env::consts::EXE_SUFFIX)),
        );
    }
    let mut child = std::process::Command::new(std::env::current_exe().unwrap());
    child
        .args([
            "--exact",
            "durable_audit_precedes_real_spawn_and_completion_refreshes_cached_detection",
            "--nocapture",
        ])
        .env_clear()
        .env("PATH", &bin)
        .env("HOME", root.path())
        .env("USERPROFILE", root.path())
        .env("DAEMON_INSTALL_FIXTURE_ROOT", root.path());
    for key in ["SYSTEMROOT", "WINDIR", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(key) {
            child.env(key, value);
        }
    }
    let output = child.output().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn exercise_install(root: &Path) {
    let bin = root.join("bin");
    let storage = Arc::new(Storage::open(root).unwrap());
    let agent = AgentLoop::new(
        Arc::clone(&storage),
        ToolRegistry::new(),
        Providers::unconfigured(),
        LoopConfig::default(),
    );
    let state = AppState::new(agent, storage, DaemonConfig::new(TOKEN)).unwrap();
    state.set_owner_key(OWNER_KEY.into());
    let app = router(state.clone());
    let (_, before) = request(&app, Method::GET, "/v1/providers/cli", Value::Null).await;
    let before: Vec<Value> = serde_json::from_str(&before).unwrap();
    assert!(
        before
            .iter()
            .all(|row| row["detected"]["version"] == "1.0.0 (Fixture)")
    );
    let (status, preview) = request(
        &app,
        Method::POST,
        "/v1/providers/cli/install",
        json!({"id":"opencode"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!bin.join("argv.json").exists(), "preview must never spawn");
    let preview: Value = serde_json::from_str(&preview).unwrap();
    let path = format!(
        "/v1/providers/cli/install/{}/decision",
        preview["approval_id"].as_str().unwrap()
    );
    let (status, stream) = request(&app, Method::POST, &path, json!({"decision":"approve"})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(stream.contains("\"ok\":true"), "{stream}");
    assert!(stream.contains("fixture installer output"));
    assert!(bin.join("audit-observed-before-install").exists());
    let argv: Value =
        serde_json::from_slice(&std::fs::read(bin.join("argv.json")).unwrap()).unwrap();
    assert_eq!(argv, json!(["install", "-g", "opencode-ai"]));
    let (_, after) = request(&app, Method::GET, "/v1/providers/cli", Value::Null).await;
    let after: Vec<Value> = serde_json::from_str(&after).unwrap();
    let opencode = after
        .iter()
        .find(|row| row["detected"]["id"] == "opencode")
        .unwrap();
    assert_eq!(
        opencode["detected"]["installed"], true,
        "cached detection was not refreshed"
    );
    assert_eq!(opencode["detected"]["version"], "9.9.9 (Fixture)");
    assert_eq!(opencode["detected"]["runnable"], false);
    assert_eq!(
        request(&app, Method::POST, &path, json!({"decision":"approve"}))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}
