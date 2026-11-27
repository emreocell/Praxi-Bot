//! The same black-box contract runs for every registered native driver.
use std::{path::Path, sync::Arc, time::Duration};

use async_trait::async_trait;
use cli_provider::{
    ApprovalLevel, Driver, PathEnv,
    adapter::EngineAdapter,
    runtime::{CliError, ToolReply, Turn, TurnHost, TurnUsage},
};
use parking_lot::Mutex;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

mod common;

fn approval_contract(adapter: &dyn EngineAdapter) -> Result<(), &'static str> {
    for requested in ApprovalLevel::ALL {
        let plan = adapter.level_plan(requested);
        if plan.requested != requested || plan.applied.rank() > requested.rank() {
            return Err("adapter widened the approval grant");
        }
    }
    Ok(())
}

struct BrokenAdapter;
#[async_trait]
impl EngineAdapter for BrokenAdapter {
    fn spec(&self) -> &'static cli_provider::CliEngineSpec {
        Driver::Claude.spec()
    }
    fn level_plan(&self, _: ApprovalLevel) -> cli_provider::LevelPlan {
        Driver::Claude.level_plan(ApprovalLevel::Full)
    }
    #[allow(clippy::too_many_arguments)]
    async fn run_turn(
        &self,
        _: &str,
        _: &PathEnv,
        _: Turn,
        _: std::path::PathBuf,
        _: ApprovalLevel,
        _: Arc<dyn TurnHost>,
        _: CancellationToken,
    ) -> Result<cli_provider::runtime::TurnUsage, CliError> {
        unreachable!("broken policy must be rejected before spawn")
    }
}

#[test]
fn conformance_rejects_a_deliberately_broken_adapter() {
    assert_eq!(
        approval_contract(&BrokenAdapter),
        Err("adapter widened the approval grant")
    );
}

fn detection_contract(driver: Driver, detected: &cli_provider::DetectedCli) {
    assert_eq!(detected.id, driver.spec().id);
    assert!(!detected.installed || detected.version.as_ref().is_some_and(|v| !v.is_empty()));
}

#[tokio::test]
async fn opt_in_read_only_installed_adapter_detection() {
    if std::env::var("BOT_CLI_CONFORMANCE_REAL").as_deref() != Ok("1") {
        return;
    }
    let detector = cli_provider::Detector::new();
    for driver in Driver::all() {
        if let Some(detected) = driver.detect(&detector).await {
            approval_contract(&driver).unwrap();
            detection_contract(driver, &detected);
        }
    }
}

#[tokio::test]
async fn every_adapter_detects_only_hermetic_binaries() {
    let binary = Path::new(env!("CARGO_BIN_EXE_fake-cli"));
    let bin = common::fixture_dir(binary);
    let missing = cli_provider::Detector::with_env(PathEnv::for_test(bin.path().into()));
    for driver in Driver::all() {
        // Retired engines may be omitted entirely when not installed.
        assert!(driver.detect(&missing).await.is_none_or(|d| !d.installed));
        common::install_binary(
            binary,
            &bin.path().join(format!(
                "{}{}",
                driver.spec().command,
                std::env::consts::EXE_SUFFIX
            )),
        );
    }
    let detector = cli_provider::Detector::with_env(PathEnv::for_test(bin.path().into()));
    for driver in Driver::all() {
        let detected = driver
            .detect(&detector)
            .await
            .expect("installed adapter omitted");
        assert!(detected.installed, "{driver:?}: {detected:?}");
        detection_contract(driver, &detected);
        assert_eq!(detected.version.as_deref(), Some("fake 1.2.3"));
    }
}

#[derive(Default)]
struct Host(Mutex<String>, Mutex<Option<(u64, u64)>>);
#[async_trait]
impl TurnHost for Host {
    fn text(&self, text: &str) {
        self.0.lock().push_str(text);
    }
    fn usage_snapshot(&self, usage: &TurnUsage) {
        *self.1.lock() = Some((usage.input, usage.output));
    }
    async fn call(&self, _: &str, _: Value) -> ToolReply {
        panic!("unexpected tool call")
    }
    async fn approve(&self, _: &str, _: Value) -> bool {
        false
    }
}

fn turn() -> Turn {
    Turn {
        prompt: "PRIVATE_PROMPT".into(),
        system: "PRIVATE_SYSTEM".into(),
        model: None,
        tools: vec![],
    }
}

fn setup(driver: Driver, mode: &str) -> (tempfile::TempDir, tempfile::TempDir, PathEnv) {
    let binary = Path::new(env!("CARGO_BIN_EXE_native-cli-fixture"));
    let bin = common::fixture_dir(binary);
    common::install_binary(
        binary,
        &bin.path()
            .join(format!("fixture{}", std::env::consts::EXE_SUFFIX)),
    );
    let env = PathEnv::for_test(bin.path().into())
        .with_env_var("FAKE_LOG", bin.path().display().to_string())
        .with_env_var("FAKE_DRIVER", driver.id())
        .with_env_var("FAKE_MODE", mode)
        .with_env_var("OPENAI_API_KEY", "SENTINEL_KEY")
        .with_env_var("BOT_API_TOKEN", "SENTINEL_TOKEN")
        .with_env_var("NODE_OPTIONS", "SENTINEL_INJECTION");
    (bin, tempfile::tempdir().unwrap(), env)
}

/// Ask, or Full where the CLI cannot enforce Ask on this platform (`OpenCode`; Kimi Code on
/// Linux and macOS), so every driver still runs the whole contract.
fn level(driver: Driver) -> ApprovalLevel {
    if cli_provider::level_plan(driver, ApprovalLevel::Ask).enforced {
        ApprovalLevel::Ask
    } else {
        ApprovalLevel::Full
    }
}

#[tokio::test]
async fn codex_usage_reaches_the_host_before_an_unfinished_native_turn_fails() {
    let driver = Driver::Codex;
    let (_bin, workspace, env) = setup(driver, "usage-then-cut");
    let host = Arc::new(Host::default());
    let error = driver
        .run_turn(
            "fixture",
            &env,
            turn(),
            workspace.path().to_owned(),
            level(driver),
            host.clone(),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(error, CliError::Protocol);
    assert_eq!(*host.1.lock(), Some((12, 4)));
}

async fn vibe_turn(mode: &str, level: ApprovalLevel) -> (tempfile::TempDir, Result<(), CliError>) {
    let (bin, workspace, env) = setup(Driver::Vibe, mode);
    let result = Driver::Vibe
        .run_turn(
            "fixture",
            &env,
            turn(),
            workspace.path().to_owned(),
            level,
            Arc::new(Host::default()),
            CancellationToken::new(),
        )
        .await
        .map(|_| ());
    (bin, result)
}

#[tokio::test]
async fn vibe_without_an_api_key_is_signed_out_before_any_prompt() {
    // `vibe-acp` answers `initialize` without credentials (so detection works signed out),
    // then refuses `session/new` with -32000 "Missing API key for mistral provider.".
    let (bin, result) = vibe_turn("acp-signed-out", ApprovalLevel::Ask).await;
    assert_eq!(result.unwrap_err(), CliError::SignedOut);
    assert!(log(&bin, "init").contains("praxi"));
    assert!(!bin.path().join("prompt").exists());
}

#[tokio::test]
async fn vibe_runs_only_in_a_mode_the_session_offers() {
    // `smart-approve` is gated off in this session, and `vibe-acp` would answer
    // `session/set_mode` for it with an empty success: Auto is refused before the prompt.
    let (bin, result) = vibe_turn("acp-modes", ApprovalLevel::Auto).await;
    assert_eq!(result.unwrap_err(), CliError::Protocol);
    assert!(!bin.path().join("set_mode").exists());
    assert!(!bin.path().join("prompt").exists());
    // An offered mode is set before the prompt is sent.
    let (bin, result) = vibe_turn("acp-modes", ApprovalLevel::Ask).await;
    result.unwrap();
    assert_eq!(log(&bin, "set_mode"), "\"ask\"");
    assert!(log(&bin, "prompt").contains("PRIVATE_PROMPT"));
}

#[tokio::test]
async fn vibe_refuses_a_model_instead_of_running_its_own() {
    // `vibe-acp` would save a model chosen over ACP as the user's default, so Praxi sends
    // none, and a model other than `default` is refused before the CLI starts.
    let (bin, workspace, env) = setup(Driver::Vibe, "");
    let mut chosen = turn();
    chosen.model = Some("devstral-2".into());
    let error = Driver::Vibe
        .run_turn(
            "fixture",
            &env,
            chosen,
            workspace.path().to_owned(),
            ApprovalLevel::Ask,
            Arc::new(Host::default()),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(error, CliError::Protocol);
    assert!(!bin.path().join("session_new").exists());
}

async fn goose_turn(
    mode: &str,
    level: ApprovalLevel,
    model: Option<&str>,
) -> (tempfile::TempDir, Result<(), CliError>) {
    let (bin, workspace, env) = setup(Driver::Goose, mode);
    let mut chosen = turn();
    chosen.model = model.map(Into::into);
    let result = Driver::Goose
        .run_turn(
            "fixture",
            &env,
            chosen,
            workspace.path().to_owned(),
            level,
            Arc::new(Host::default()),
            CancellationToken::new(),
        )
        .await
        .map(|_| ());
    (bin, result)
}

#[tokio::test]
async fn goose_without_a_provider_is_signed_out_before_any_prompt() {
    // `goose acp` answers `initialize` without credentials, then fails `session/new` with
    // -32603 "Failed to resolve provider and model: ...". This exact text is pinned: a Goose
    // release that changes it must fail here, not silently become a protocol error.
    let (bin, result) = goose_turn("acp-goose-unconfigured", ApprovalLevel::Ask, None).await;
    assert_eq!(result.unwrap_err(), CliError::SignedOut);
    assert!(log(&bin, "init").contains("praxi"));
    assert!(!bin.path().join("prompt").exists());
}

#[tokio::test]
async fn goose_always_sets_the_mode_and_sends_the_model_for_the_session_only() {
    // A new Goose session starts in `auto`, so even Ask sends `approve` before the prompt.
    let (bin, result) = goose_turn("acp-goose", ApprovalLevel::Ask, None).await;
    result.unwrap();
    assert_eq!(log(&bin, "set_mode"), "\"approve\"");
    assert!(!bin.path().join("set_config_option").exists());
    assert!(log(&bin, "prompt").contains("PRIVATE_PROMPT"));
    // Accept edits has no Goose mode of its own and runs as Ask.
    let (bin, result) = goose_turn("acp-goose", ApprovalLevel::AcceptEdits, None).await;
    result.unwrap();
    assert_eq!(log(&bin, "set_mode"), "\"approve\"");
    // A chosen model is the `model` config option, which Goose keeps on the session.
    let (bin, result) = goose_turn("acp-goose", ApprovalLevel::Auto, Some("gpt-4o")).await;
    result.unwrap();
    assert_eq!(log(&bin, "set_mode"), "\"smart_approve\"");
    let option: Value = serde_json::from_str(&log(&bin, "set_config_option")).unwrap();
    assert_eq!(option["configId"], "model");
    assert_eq!(option["value"], "gpt-4o");
    assert!(log(&bin, "prompt").contains("PRIVATE_PROMPT"));
}

fn log(bin: &tempfile::TempDir, name: &str) -> String {
    std::fs::read_to_string(bin.path().join(name)).unwrap()
}

async fn started(bin: &tempfile::TempDir) {
    tokio::time::timeout(Duration::from_secs(15), async {
        while !bin.path().join("pid").exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("fixture did not start");
}

async fn exited(bin: &tempfile::TempDir) {
    let pid = log(bin, "pid");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            #[cfg(windows)]
            let alive = {
                // Observe the process without opening an unsafe OS handle.
                let out = tokio::process::Command::new("tasklist.exe")
                    .args([
                        "/FI",
                        &format!("PID eq {}", pid.trim()),
                        "/FO",
                        "CSV",
                        "/NH",
                    ])
                    .output()
                    .await
                    .unwrap();
                String::from_utf8_lossy(&out.stdout).contains(&format!("\"{}\"", pid.trim()))
            };
            #[cfg(unix)]
            let alive = tokio::process::Command::new("kill")
                .args(["-0", pid.trim()])
                .stderr(std::process::Stdio::null())
                .status()
                .await
                .unwrap()
                .success();
            if !alive {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("adapter left its CLI running");
}

#[tokio::test]
async fn every_adapter_spawns_streams_and_confines_launch_data() {
    for driver in Driver::all() {
        let adapter: &dyn EngineAdapter = &driver;
        assert_eq!(adapter.spec().id, driver.id());
        approval_contract(adapter).unwrap();
        let (bin, workspace, env) = setup(driver, "");
        let host = Arc::new(Host::default());
        tokio::time::timeout(
            Duration::from_secs(20),
            adapter.run_turn(
                "fixture",
                &env,
                turn(),
                workspace.path().into(),
                level(driver),
                host.clone(),
                CancellationToken::new(),
            ),
        )
        .await
        .expect("session timeout")
        .unwrap_or_else(|e| panic!("{driver:?}: {e}"));
        let argv = log(&bin, "argv");
        let environment = log(&bin, "env");
        for private in ["PRIVATE_PROMPT", "PRIVATE_SYSTEM", "SENTINEL_", "Bearer "] {
            assert!(!argv.contains(private), "{driver:?}: argv leak");
        }
        assert!(
            !environment.contains("SENTINEL_"),
            "{driver:?}: environment leak"
        );
        assert_eq!(
            std::fs::canonicalize(log(&bin, "cwd")).unwrap(),
            std::fs::canonicalize(workspace.path()).unwrap()
        );
        assert!(
            host.0.lock().starts_with("native "),
            "{driver:?}: missing stream"
        );
        assert!(!host.0.lock().contains("PRIVATE_"));
        exited(&bin).await;
    }
}

#[tokio::test]
async fn every_adapter_cancels_and_cleans_up_after_a_caller_timeout() {
    for driver in Driver::all() {
        for timeout in [false, true] {
            let (bin, workspace, env) = setup(driver, "hang");
            let cancel = CancellationToken::new();
            let mut future = Box::pin(driver.run_turn(
                "fixture",
                &env,
                turn(),
                workspace.path().into(),
                level(driver),
                Arc::new(Host::default()),
                cancel.clone(),
            ));
            tokio::select! {
                result = &mut future => panic!("{driver:?} exited before cancellation: {result:?}"),
                () = started(&bin) => {}
            }
            if timeout {
                assert!(
                    tokio::time::timeout(Duration::from_millis(30), &mut future)
                        .await
                        .is_err()
                );
                drop(future);
            } else {
                cancel.cancel();
                assert_eq!(
                    tokio::time::timeout(Duration::from_secs(5), &mut future)
                        .await
                        .unwrap()
                        .unwrap_err(),
                    CliError::Cancelled
                );
            }
            exited(&bin).await;
        }
    }
}

#[tokio::test]
async fn every_adapter_rejects_bad_output_and_workspace_executables() {
    for driver in Driver::all() {
        for mode in ["exit", "flood"] {
            let (_bin, workspace, env) = setup(driver, mode);
            let host = Arc::new(Host::default());
            let error = tokio::time::timeout(
                Duration::from_secs(20),
                driver.run_turn(
                    "fixture",
                    &env,
                    turn(),
                    workspace.path().into(),
                    level(driver),
                    host.clone(),
                    CancellationToken::new(),
                ),
            )
            .await
            .unwrap()
            .unwrap_err();
            assert_eq!(error, CliError::Protocol, "{driver:?}, {mode}");
            assert_eq!(error.to_string(), "cli_protocol_error");
            assert!(host.0.lock().is_empty());
        }
        let (bin, _workspace, env) = setup(driver, "");
        let error = driver
            .run_turn(
                "fixture",
                &env,
                turn(),
                bin.path().into(),
                level(driver),
                Arc::new(Host::default()),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert_eq!(error, CliError::NotInstalled, "{driver:?}");
        assert!(!bin.path().join("argv").exists());
    }
}
