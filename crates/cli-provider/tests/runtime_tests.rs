use async_trait::async_trait;
use cli_provider::{
    PathEnv,
    runtime::{CliEngine, CliError, Engine, ToolReply, Turn, TurnHost},
};
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;
use tool_registry::{Effect, PermissionCategory, ToolSpec};
const PROMPT: &str = "private prompt\n%PATH%\nsecond line";
struct Host {
    allow: bool,
    calls: AtomicUsize,
    checks: AtomicUsize,
    approvals: AtomicUsize,
    text: Mutex<String>,
}
impl Host {
    fn new(allow: bool) -> Arc<Self> {
        Arc::new(Self {
            allow,
            calls: AtomicUsize::new(0),
            checks: AtomicUsize::new(0),
            approvals: AtomicUsize::new(0),
            text: Mutex::new(String::new()),
        })
    }
}
#[async_trait]
impl TurnHost for Host {
    fn text(&self, t: &str) {
        self.text.lock().push_str(t);
    }
    async fn call(&self, name: &str, _: Value) -> ToolReply {
        assert_eq!(name, "known.tool");
        self.checks.fetch_add(1, Ordering::SeqCst);
        if self.allow {
            self.calls.fetch_add(1, Ordering::SeqCst);
        }
        ToolReply {
            text: if self.allow { "tool output" } else { "denied" }.into(),
            is_error: !self.allow,
        }
    }
    async fn approve(&self, _: &str, _: Value) -> bool {
        self.approvals.fetch_add(1, Ordering::SeqCst);
        self.allow
    }
}
fn turn() -> Turn {
    Turn {
        prompt: PROMPT.into(),
        system: "system private\nprompt".into(),
        model: Some("fixture-model".into()),
        tools: vec![ToolSpec {
            name: "known.tool".into(),
            description: "known tool".into(),
            input_schema: json!({"type":"object"}),
            effect: Effect::Read,
            category: PermissionCategory::Fs,
            platforms: vec![],
        }],
    }
}
fn fixture(engine: Engine, mode: &str, shim: bool) -> (tempfile::TempDir, CliEngine) {
    let dir = tempfile::tempdir().unwrap();
    let command = if shim { "shim" } else { "fixture" };
    let dest = dir.path().join(if shim {
        "node.exe".into()
    } else {
        format!("fixture{}", std::env::consts::EXE_SUFFIX)
    });
    std::fs::copy(env!("CARGO_BIN_EXE_runtime-cli-fixture"), dest).unwrap();
    if shim {
        std::fs::write(dir.path().join("cli.js"), "// fixture node only").unwrap();
        std::fs::write(
            dir.path().join("shim.cmd"),
            "@echo off\r\n\"%~dp0\\node.exe\" \"%~dp0\\cli.js\" %*\r\n",
        )
        .unwrap();
    }
    std::fs::write(dir.path().join("auth.json"), "SENTINEL_AUTH_CONTENT").unwrap();
    let env = PathEnv::for_test(dir.path().into())
        .with_env_var("FAKE_LOG", dir.path().display().to_string())
        .with_env_var("FAKE_MODE", mode)
        .with_env_var("CODEX_HOME", dir.path().display().to_string())
        .with_env_var("CLAUDE_CONFIG_DIR", dir.path().display().to_string())
        .with_env_var("API_KEY", "SENTINEL_ENV_KEY")
        .with_env_var("MY_TOKEN", "SENTINEL_ENV_TOKEN")
        .with_env_var("MY_SECRET", "SENTINEL_ENV_SECRET")
        .with_env_var("MY_CREDENTIAL", "SENTINEL_ENV_CREDENTIAL");
    (
        dir,
        CliEngine {
            engine,
            command: command.into(),
            env,
        },
    )
}
async fn run(engine: Engine, allow: bool, shim: bool) {
    let (dir, cli) = fixture(engine, "", shim);
    let host = Host::new(allow);
    let usage = tokio::time::timeout(
        Duration::from_secs(20),
        cli.run_turn(turn(), host.clone(), CancellationToken::new()),
    )
    .await
    .unwrap()
    .unwrap();
    let argv = std::fs::read_to_string(dir.path().join("argv")).unwrap();
    assert!(!argv.contains(PROMPT));
    assert!(!argv.contains("private prompt"));
    assert!(!argv.contains("%PATH%"));
    assert!(!argv.contains("system private"));
    assert!(!argv.contains("tool output"));
    assert!(
        std::fs::read_to_string(dir.path().join("stdin"))
            .unwrap()
            .contains("%PATH%")
    );
    let env = std::fs::read_to_string(dir.path().join("env")).unwrap();
    assert!(!env.contains("SENTINEL_"));
    assert!(!host.text.lock().contains("SENTINEL_"));
    assert!(!host.text.lock().contains("PRIVATE_THINKING"));
    assert_eq!(host.calls.load(Ordering::SeqCst), usize::from(allow));
    if engine == Engine::Claude || allow {
        let result: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join("tool_reply")).unwrap())
                .unwrap();
        assert_eq!(result["result"]["isError"], !allow);
        assert_eq!(usage.input, 12);
    }
    if engine == Engine::Codex {
        assert_eq!(host.approvals.load(Ordering::SeqCst), 1);
    }
    if engine == Engine::Claude {
        assert_eq!(host.text.lock().as_str(), "hello");
        let config = std::fs::read_to_string(dir.path().join("config")).unwrap();
        assert!(!std::path::Path::new(&config).exists());
    }
}
#[tokio::test]
async fn claude_allow() {
    run(Engine::Claude, true, false).await;
}
#[tokio::test]
async fn claude_deny() {
    run(Engine::Claude, false, false).await;
}
#[tokio::test]
async fn codex_allow() {
    run(Engine::Codex, true, false).await;
}
#[tokio::test]
async fn codex_deny() {
    run(Engine::Codex, false, false).await;
}
#[tokio::test]
async fn claude_cmd_prompt_absent() {
    run(Engine::Claude, true, true).await;
}
#[tokio::test]
async fn codex_cmd_prompt_absent() {
    run(Engine::Codex, true, true).await;
}
#[tokio::test]
async fn native_tools_are_refused() {
    for (engine, mode) in [(Engine::Claude, "bash"), (Engine::Codex, "native")] {
        let (_dir, cli) = fixture(engine, mode, false);
        let host = Host::new(true);
        assert_eq!(
            cli.run_turn(turn(), host.clone(), CancellationToken::new())
                .await
                .unwrap_err(),
            CliError::NativeToolsOpen
        );
        assert_eq!(host.calls.load(Ordering::SeqCst), 0);
    }
}
#[tokio::test]
async fn quota_and_unbounded_lines_are_non_retryable() {
    for (mode, error) in [("quota", CliError::Quota), ("flood", CliError::Protocol)] {
        let (_dir, cli) = fixture(Engine::Claude, mode, false);
        assert_eq!(
            cli.run_turn(turn(), Host::new(true), CancellationToken::new())
                .await
                .unwrap_err(),
            error
        );
    }
}
#[tokio::test]
async fn cancellation_and_future_drop_reap_child() {
    for abort in [false, true] {
        let (dir, cli) = fixture(Engine::Codex, "hang-tree", false);
        let cancel = CancellationToken::new();
        let c = cancel.clone();
        let task = tokio::spawn(async move { cli.run_turn(turn(), Host::new(true), c).await });
        let pid_path = dir.path().join("pid");
        tokio::time::timeout(Duration::from_secs(15), async {
            while !pid_path.exists() || !dir.path().join("descendant/pid").exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        if abort {
            task.abort();
        } else {
            cancel.cancel();
        }
        let _ = task.await;
        for pid_file in [pid_path, dir.path().join("descendant/pid")] {
            let pid = std::fs::read_to_string(pid_file).unwrap();
            tokio::time::timeout(Duration::from_secs(15), async {
                loop {
                    #[cfg(windows)]
                    let alive = String::from_utf8_lossy(
                        &tokio::process::Command::new("tasklist.exe")
                            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
                            .output()
                            .await
                            .unwrap()
                            .stdout,
                    )
                    .contains(&format!("\"{pid}\""));
                    #[cfg(unix)]
                    let alive = std::path::Path::new(&format!("/proc/{pid}")).exists();
                    if !alive {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(30)).await;
                }
            })
            .await
            .unwrap();
        }
    }
}
#[tokio::test]
async fn missing_and_refused_shims_are_typed() {
    let dir = tempfile::tempdir().unwrap();
    let cli = CliEngine {
        engine: Engine::Codex,
        command: "missing".into(),
        env: PathEnv::for_test(dir.path().into()),
    };
    assert_eq!(
        cli.run_turn(turn(), Host::new(true), CancellationToken::new())
            .await
            .unwrap_err(),
        CliError::NotInstalled
    );
    std::fs::write(dir.path().join("missing.cmd"), "@echo off").unwrap();
    assert_eq!(
        cli.run_turn(turn(), Host::new(true), CancellationToken::new())
            .await
            .unwrap_err(),
        CliError::ShimRefused
    );
}
