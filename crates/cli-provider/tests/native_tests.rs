//! Native-tools sessions against the hermetic fake CLI: one run per dialect family.
use async_trait::async_trait;
use cli_provider::{
    ApprovalLevel, Driver, PathEnv,
    runtime::{
        NativePermission, NativeQuestion, NativeTurn, ToolReply, Turn, TurnHost, run_native_turn,
    },
};
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::{path::Path, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;
use tool_registry::{Effect, PermissionCategory, ToolSpec};

mod common;

struct Host {
    allow: bool,
    /// What the person chooses for a question (`None`: nobody chose).
    choose: Option<usize>,
    questions: Mutex<Vec<NativeQuestion>>,
    permissions: Mutex<Vec<NativePermission>>,
    text: Mutex<String>,
}
#[async_trait]
impl TurnHost for Host {
    fn text(&self, t: &str) {
        self.text.lock().push_str(t);
    }
    async fn call(&self, _: &str, _: Value) -> ToolReply {
        panic!("no Praxi tool is called in this test")
    }
    async fn approve(&self, _: &str, _: Value) -> bool {
        panic!("no Praxi tool approval in this test")
    }
    async fn permission(&self, request: NativePermission) -> bool {
        self.permissions.lock().push(request);
        self.allow
    }
    async fn question(&self, request: NativeQuestion) -> Option<usize> {
        self.questions.lock().push(request);
        self.choose
    }
}

fn turn() -> Turn {
    Turn {
        prompt: "private prompt".into(),
        system: "private system".into(),
        model: None,
        tools: vec![ToolSpec {
            name: "known.tool".into(),
            description: "known".into(),
            input_schema: json!({"type":"object"}),
            effect: Effect::Read,
            category: PermissionCategory::Fs,
            platforms: vec![],
            content: tool_registry::ContentClass::none(),
        }],
    }
}

/// A `fixture` executable on a sealed PATH, a log folder and a workspace folder.
fn setup() -> (tempfile::TempDir, tempfile::TempDir, PathEnv) {
    let binary = Path::new(env!("CARGO_BIN_EXE_native-cli-fixture"));
    let dir = common::fixture_dir(binary);
    common::install_binary(
        binary,
        &dir.path()
            .join(format!("fixture{}", std::env::consts::EXE_SUFFIX)),
    );
    let env = PathEnv::for_test(dir.path().into())
        .with_env_var("FAKE_LOG", dir.path().display().to_string())
        .with_env_var("API_KEY", "SENTINEL_ENV_KEY")
        .with_env_var("MY_TOKEN", "SENTINEL_ENV_TOKEN");
    (dir, tempfile::tempdir().unwrap(), env)
}

async fn run(
    driver: Driver,
    level: ApprovalLevel,
    allow: bool,
) -> (tempfile::TempDir, tempfile::TempDir, Arc<Host>) {
    let (dir, workspace, env) = setup();
    let host = Arc::new(Host {
        allow,
        choose: None,
        questions: Mutex::new(Vec::new()),
        permissions: Mutex::new(Vec::new()),
        text: Mutex::new(String::new()),
    });
    let nt = NativeTurn {
        turn: turn(),
        driver,
        level,
        workspace: workspace.path().to_owned(),
    };
    tokio::time::timeout(
        Duration::from_secs(30),
        run_native_turn("fixture", &env, nt, host.clone(), CancellationToken::new()),
    )
    .await
    .expect("native turn timed out")
    .expect("native turn failed");
    (dir, workspace, host)
}

fn log(dir: &tempfile::TempDir, name: &str) -> String {
    std::fs::read_to_string(dir.path().join(name)).unwrap_or_else(|_| panic!("{name} not logged"))
}

fn same_dir(logged: &str, expected: &Path) -> bool {
    std::fs::canonicalize(logged).unwrap() == std::fs::canonicalize(expected).unwrap()
}

#[tokio::test]
async fn claude_stream_json_runs_in_workspace_with_its_own_tools() {
    for allow in [true, false] {
        let (dir, workspace, host) = run(Driver::Claude, ApprovalLevel::AcceptEdits, allow).await;
        let argv = log(&dir, "argv");
        let flags: Vec<&str> = argv.lines().collect();
        let at = |flag: &str| flags.iter().position(|a| *a == flag).map(|i| flags[i + 1]);
        // The CLI's own tools stay on: no `--tools ""`, mode from the bot's level.
        assert!(!flags.contains(&"--tools"));
        assert_eq!(at("--permission-mode"), Some("acceptEdits"));
        assert_eq!(at("--permission-prompt-tool"), Some("mcp__praxi__approve"));
        assert!(!argv.contains("private prompt"));
        assert!(same_dir(&log(&dir, "cwd"), workspace.path()));
        // Environment allowlist: no provider keys or tokens reach the CLI.
        let env = log(&dir, "env");
        assert!(
            !env.contains("API_KEY") && !env.contains("MY_TOKEN"),
            "{env}"
        );
        // The CLI's permission prompt became one Praxi request and was answered.
        let permissions = host.permissions.lock();
        assert_eq!(permissions.len(), 1);
        assert_eq!(permissions[0].tool, "Bash");
        assert!(permissions[0].summary.contains("touch note.txt"));
        assert!(log(&dir, "verdict").contains(if allow { "\"allow\"" } else { "\"deny\"" }));
        assert_eq!(host.text.lock().as_str(), "native done");
    }
}

#[tokio::test]
async fn acp_agent_permissions_follow_the_level_and_fs_is_refused() {
    // Ask: the request goes to Praxi and a deny selects the agent's reject option.
    let (dir, workspace, host) = run(Driver::Cursor, ApprovalLevel::Ask, false).await;
    assert_eq!(host.permissions.lock().len(), 1);
    assert!(log(&dir, "permission_answer").contains("\"optionId\":\"no\""));
    assert!(log(&dir, "fs_answer").contains("\"error\""));
    assert_eq!(log(&dir, "argv"), "acp");
    let init: Value = serde_json::from_str(&log(&dir, "init")).unwrap();
    assert_eq!(init["clientCapabilities"]["fs"]["writeTextFile"], false);
    assert_eq!(init["clientCapabilities"]["terminal"], false);
    let new: Value = serde_json::from_str(&log(&dir, "session_new")).unwrap();
    assert!(same_dir(new["cwd"].as_str().unwrap(), workspace.path()));
    assert_eq!(new["mcpServers"][0]["name"], "praxi");
    assert_eq!(host.text.lock().as_str(), "native acp");
    // Full access: accepted without a card.
    let (dir, _w, host) = run(Driver::Cursor, ApprovalLevel::Full, false).await;
    assert!(host.permissions.lock().is_empty());
    assert!(log(&dir, "permission_answer").contains("\"optionId\":\"yes\""));
}

#[tokio::test]
async fn agy_receives_http_bridge_even_without_capability_advertisement() {
    for mode in ["acp-list", "acp-no-capabilities"] {
        let (dir, workspace, env) = setup();
        let env = env.with_env_var("FAKE_MODE", mode);
        let host = Arc::new(Host {
            allow: false,
            choose: None,
            questions: Mutex::new(Vec::new()),
            permissions: Mutex::new(Vec::new()),
            text: Mutex::new(String::new()),
        });
        let nt = NativeTurn {
            turn: turn(),
            driver: Driver::Agy,
            level: ApprovalLevel::Full,
            workspace: workspace.path().to_owned(),
        };
        tokio::time::timeout(
            Duration::from_secs(30),
            run_native_turn("fixture", &env, nt, host, CancellationToken::new()),
        )
        .await
        .expect("agent timed out")
        .expect("agent failed");
        let new: Value = serde_json::from_str(&log(&dir, "session_new")).unwrap();
        assert_eq!(new["mcpServers"][0]["type"], "http");
        assert_eq!(new["mcpServers"][0]["name"], "praxi");
        if mode == "acp-no-capabilities" {
            assert_eq!(log(&dir, "bridge_tool_count"), "2");
        }
    }
}

#[tokio::test]
async fn agy_silently_ignoring_bridge_stops_before_prompt() {
    let (dir, workspace, env) = setup();
    let host = Arc::new(Host {
        allow: false,
        choose: None,
        questions: Mutex::new(Vec::new()),
        permissions: Mutex::new(Vec::new()),
        text: Mutex::new(String::new()),
    });
    let nt = NativeTurn {
        turn: turn(),
        driver: Driver::Agy,
        level: ApprovalLevel::Full,
        workspace: workspace.path().to_owned(),
    };
    let error = tokio::time::timeout(
        Duration::from_secs(30),
        run_native_turn("fixture", &env, nt, host, CancellationToken::new()),
    )
    .await
    .expect("agent timed out")
    .unwrap_err();
    assert_eq!(
        error,
        cli_provider::runtime::CliError::PraxiBridgeUnavailable
    );
    assert!(!dir.path().join("prompt").exists());
}

#[tokio::test]
async fn missing_bridge_capability_stops_before_prompt_for_other_agents() {
    let (dir, workspace, env) = setup();
    let env = env.with_env_var("FAKE_MODE", "acp-no-capabilities");
    let host = Arc::new(Host {
        allow: false,
        choose: None,
        questions: Mutex::new(Vec::new()),
        permissions: Mutex::new(Vec::new()),
        text: Mutex::new(String::new()),
    });
    let nt = NativeTurn {
        turn: turn(),
        driver: Driver::Cursor,
        level: ApprovalLevel::Full,
        workspace: workspace.path().to_owned(),
    };
    let error = tokio::time::timeout(
        Duration::from_secs(30),
        run_native_turn("fixture", &env, nt, host, CancellationToken::new()),
    )
    .await
    .expect("agent timed out")
    .unwrap_err();
    assert_eq!(
        error,
        cli_provider::runtime::CliError::PraxiBridgeUnavailable
    );
    assert!(!dir.path().join("session_new").exists());
    assert!(!dir.path().join("prompt").exists());
}

#[tokio::test]
async fn agy_rejecting_http_bridge_stops_before_prompt() {
    let (dir, workspace, env) = setup();
    let env = env.with_env_var("FAKE_MODE", "acp-reject-bridge");
    let host = Arc::new(Host {
        allow: false,
        choose: None,
        questions: Mutex::new(Vec::new()),
        permissions: Mutex::new(Vec::new()),
        text: Mutex::new(String::new()),
    });
    let nt = NativeTurn {
        turn: turn(),
        driver: Driver::Agy,
        level: ApprovalLevel::Full,
        workspace: workspace.path().to_owned(),
    };
    let error = tokio::time::timeout(
        Duration::from_secs(30),
        run_native_turn("fixture", &env, nt, host, CancellationToken::new()),
    )
    .await
    .expect("agent timed out")
    .unwrap_err();
    assert_eq!(
        error,
        cli_provider::runtime::CliError::PraxiBridgeUnavailable
    );
    assert!(!dir.path().join("prompt").exists());
}

/// One run of the Kimi fake session that asks two multiple-choice questions.
async fn kimi_questions(
    level: ApprovalLevel,
    allow: bool,
    choose: Option<usize>,
) -> (tempfile::TempDir, Arc<Host>) {
    let (dir, workspace, env) = setup();
    let env = env.with_env_var("FAKE_MODE", "acp-kimi-question");
    let host = Arc::new(Host {
        allow,
        choose,
        questions: Mutex::new(Vec::new()),
        permissions: Mutex::new(Vec::new()),
        text: Mutex::new(String::new()),
    });
    let nt = NativeTurn {
        turn: turn(),
        driver: Driver::Kimi,
        level,
        workspace: workspace.path().to_owned(),
    };
    tokio::time::timeout(
        Duration::from_secs(30),
        run_native_turn("fixture", &env, nt, host.clone(), CancellationToken::new()),
    )
    .await
    .expect("kimi turn timed out")
    .expect("kimi turn failed");
    (dir, host)
}

#[tokio::test]
async fn kimi_questions_reach_the_host_with_the_real_options_and_are_never_answered_for_the_owner()
{
    // Whatever the level and whatever a permission approval would say (`allow: true`), the
    // question is a question: no permission request, the CLI's own answers, option 1 never
    // picked on its own. Nobody chose: the CLI gets its "Skip".
    for level in [ApprovalLevel::Ask, ApprovalLevel::Auto, ApprovalLevel::Full] {
        if !cli_provider::level_plan(Driver::Kimi, level).offered {
            continue;
        }
        let (dir, host) = kimi_questions(level, true, None).await;
        assert!(host.permissions.lock().is_empty(), "{level:?}");
        let questions = host.questions.lock();
        assert_eq!(questions.len(), 2, "{level:?}");
        assert_eq!(questions[0].question, "Which database?");
        assert_eq!(questions[0].options, ["Postgres", "SQLite"]);
        assert_eq!(questions[1].options, ["Yes", "No"]);
        for name in ["question_answer", "question_answer_2"] {
            let answer = log(&dir, name);
            assert!(
                answer.contains("\"optionId\":\"q0_skip\""),
                "{level:?} {answer}"
            );
        }
    }
}

#[tokio::test]
async fn kimi_gets_the_answer_the_owner_chose() {
    // Kimi cannot be held at Ask on Linux and macOS: use the level the picker starts on.
    let level = level_picker_starts_on(Driver::Kimi);
    let (dir, host) = kimi_questions(level, false, Some(1)).await;
    assert!(host.permissions.lock().is_empty());
    // The second option of each question, not the first.
    assert!(log(&dir, "question_answer").contains("\"optionId\":\"q0_opt_1\""));
    assert!(log(&dir, "question_answer_2").contains("\"optionId\":\"q0_opt_1\""));
    // An index the CLI did not offer is a skip, not a wrapped or clamped answer.
    let (dir, _host) = kimi_questions(level, true, Some(7)).await;
    assert!(log(&dir, "question_answer").contains("q0_skip"));
}

/// The level the desktop's one consent step starts on: Ask when the engine offers it, else its first
/// offered level (`OpenCode`, and Kimi on Linux and macOS, cannot be held at Ask).
fn level_picker_starts_on(driver: Driver) -> ApprovalLevel {
    let offered: Vec<_> = ApprovalLevel::ALL
        .into_iter()
        .filter(|l| cli_provider::level_plan(driver, *l).offered)
        .collect();
    if offered.contains(&ApprovalLevel::Ask) {
        ApprovalLevel::Ask
    } else {
        offered[0]
    }
}

#[tokio::test]
async fn every_engine_runs_its_first_message_with_the_settings_the_picker_gives_it() {
    for driver in Driver::all() {
        let level = level_picker_starts_on(driver);
        let (dir, workspace, env) = setup();
        let env = match driver {
            // `vibe-acp` is its own entry point and takes no arguments.
            Driver::Vibe => env.with_env_var("FAKE_DRIVER", "vibe"),
            _ => env,
        };
        let host = Arc::new(Host {
            allow: true,
            choose: None,
            questions: Mutex::new(Vec::new()),
            permissions: Mutex::new(Vec::new()),
            text: Mutex::new(String::new()),
        });
        let nt = NativeTurn {
            turn: turn(),
            driver,
            level,
            workspace: workspace.path().to_owned(),
        };
        let result = tokio::time::timeout(
            Duration::from_secs(30),
            run_native_turn("fixture", &env, nt, host.clone(), CancellationToken::new()),
        )
        .await
        .unwrap_or_else(|_| panic!("{}: first message timed out", driver.id()));
        result.unwrap_or_else(|e| panic!("{}: first message failed: {e:?}", driver.id()));
        assert!(
            !host.text.lock().is_empty(),
            "{}: the fake CLI's answer reached the chat",
            driver.id()
        );
        assert!(
            same_dir(&log(&dir, "cwd"), workspace.path()),
            "{}",
            driver.id()
        );
    }
}

#[tokio::test]
async fn refuses_below_enforceable_levels_and_workspace_executables() {
    use cli_provider::runtime::CliError;
    // OpenCode cannot be held at Ask: the turn is refused before anything starts.
    let (dir, workspace, env) = setup();
    let host = Arc::new(Host {
        allow: true,
        choose: None,
        questions: Mutex::new(Vec::new()),
        permissions: Mutex::new(Vec::new()),
        text: Mutex::new(String::new()),
    });
    let nt = |level, workspace: &Path| NativeTurn {
        turn: turn(),
        driver: Driver::OpenCode,
        level,
        workspace: workspace.to_owned(),
    };
    let err = run_native_turn(
        "fixture",
        &env,
        nt(ApprovalLevel::Ask, workspace.path()),
        host.clone(),
        CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(err, CliError::NativeToolsOpen);
    assert!(!dir.path().join("argv").exists());
    // A CLI that lives inside the workspace is never resolved or run.
    let inside = tempfile::tempdir().unwrap();
    common::install_binary(
        Path::new(env!("CARGO_BIN_EXE_native-cli-fixture")),
        &inside
            .path()
            .join(format!("fixture{}", std::env::consts::EXE_SUFFIX)),
    );
    let env = PathEnv::for_test(inside.path().into())
        .with_env_var("FAKE_LOG", inside.path().display().to_string());
    let err = run_native_turn(
        "fixture",
        &env,
        nt(ApprovalLevel::Full, inside.path()),
        host,
        CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(err, CliError::NotInstalled | CliError::ShimRefused),
        "{err:?}"
    );
    assert!(!inside.path().join("argv").exists());
}
