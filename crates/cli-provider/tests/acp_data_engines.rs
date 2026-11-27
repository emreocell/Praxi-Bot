//! Data-driven ACP engines (`providers/cli/<id>.toml`) against the hermetic fake ACP agent.
//!
//! The fixture is never a vendor CLI: these tests prove how Praxi launches and answers an
//! engine from its entry, not that the vendor's CLI behaves as the entry says.
use std::{path::Path, sync::Arc, time::Duration};

use async_trait::async_trait;
use cli_provider::{
    ApprovalLevel, Driver, PathEnv, acp_data,
    adapter::EngineAdapter,
    runtime::{CliError, NativePermission, ToolReply, Turn, TurnHost},
};
use parking_lot::Mutex;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

mod common;

#[derive(Default)]
struct Host {
    allow: bool,
    permissions: Mutex<Vec<NativePermission>>,
}
#[async_trait]
impl TurnHost for Host {
    fn text(&self, _: &str) {}
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
}

fn data_drivers() -> Vec<Driver> {
    Driver::all()
        .into_iter()
        .filter(|d| matches!(d, Driver::Data(_)))
        .collect()
}

fn engine(driver: Driver) -> &'static acp_data::AcpEngine {
    let Driver::Data(data) = driver else {
        panic!("{driver:?} is not data-driven")
    };
    data.engine()
}

fn setup(mode: &str) -> (tempfile::TempDir, tempfile::TempDir, PathEnv) {
    let binary = Path::new(env!("CARGO_BIN_EXE_native-cli-fixture"));
    let bin = common::fixture_dir(binary);
    common::install_binary(
        binary,
        &bin.path()
            .join(format!("fixture{}", std::env::consts::EXE_SUFFIX)),
    );
    let env = PathEnv::for_test(bin.path().into())
        .with_env_var("FAKE_LOG", bin.path().display().to_string())
        .with_env_var("FAKE_MODE", mode)
        .with_env_var("OPENAI_API_KEY", "SENTINEL_KEY");
    (bin, tempfile::tempdir().unwrap(), env)
}

async fn run(
    driver: Driver,
    level: ApprovalLevel,
    model: Option<&str>,
    mode: &str,
    allow: bool,
) -> (tempfile::TempDir, Arc<Host>, Result<(), CliError>) {
    let (bin, workspace, env) = setup(mode);
    let host = Arc::new(Host {
        allow,
        permissions: Mutex::new(Vec::new()),
    });
    let result = tokio::time::timeout(
        Duration::from_secs(30),
        driver.run_turn(
            "fixture",
            &env,
            Turn {
                prompt: "PRIVATE_PROMPT".into(),
                system: "PRIVATE_SYSTEM".into(),
                model: model.map(Into::into),
                tools: vec![],
            },
            workspace.path().to_owned(),
            level,
            host.clone(),
            CancellationToken::new(),
        ),
    )
    .await
    .expect("turn timed out")
    .map(|_| ());
    (bin, host, result)
}

fn log(bin: &tempfile::TempDir, name: &str) -> String {
    std::fs::read_to_string(bin.path().join(name)).unwrap_or_default()
}

#[test]
fn the_issue_lists_are_data_entries() {
    let ids: Vec<&str> = data_drivers().into_iter().map(Driver::id).collect();
    assert_eq!(ids, ["auggie", "cline"]);
    for driver in data_drivers() {
        // Registered like any engine, never runnable under Praxi's own tools, experimental.
        let spec = cli_provider::engine_spec(driver.id()).unwrap();
        assert!(spec.can_enable() && !spec.runnable);
        assert_eq!(driver.support(), cli_provider::Support::Experimental);
        assert_eq!(Driver::from_id(driver.id()), Some(driver));
        assert_ne!(engine(driver).unverified.len(), 0);
    }
}

#[tokio::test]
async fn the_launch_is_the_entry_text_and_nothing_else() {
    for driver in data_drivers() {
        for level in ApprovalLevel::ALL {
            let plan = driver.level_plan(level);
            if !plan.enforced {
                continue;
            }
            let (bin, _host, result) = run(driver, level, None, "", false).await;
            result.unwrap_or_else(|e| panic!("{} {level:?}: {e}", driver.id()));
            let expected: Vec<String> = engine(driver)
                .args
                .iter()
                .chain(plan.flags)
                .map(|a| (*a).to_owned())
                .collect();
            assert_eq!(
                log(&bin, "argv"),
                expected.join("\n"),
                "{} {level:?}",
                driver.id()
            );
            for private in ["PRIVATE_PROMPT", "PRIVATE_SYSTEM", "SENTINEL_"] {
                assert!(!log(&bin, "argv").contains(private));
                assert!(!log(&bin, "env").contains(private));
            }
            // The mode is only sent when the entry names one.
            match plan.acp_mode {
                Some(mode) => assert_eq!(log(&bin, "set_mode"), format!("{mode:?}")),
                None => assert!(!bin.path().join("set_mode").exists()),
            }
        }
    }
}

#[tokio::test]
async fn cline_pins_the_auto_approve_flag_at_every_level() {
    let cline = Driver::from_id("cline").unwrap();
    for (level, flag) in [
        (ApprovalLevel::Ask, "false"),
        // No edits-only or reviewer mode is documented: these run as Ask, never wider.
        (ApprovalLevel::AcceptEdits, "false"),
        (ApprovalLevel::Auto, "false"),
        (ApprovalLevel::Full, "true"),
    ] {
        let (bin, _host, result) = run(cline, level, None, "", false).await;
        result.unwrap();
        assert_eq!(
            log(&bin, "argv"),
            format!("--acp\n--auto-approve\n{flag}"),
            "{level:?}"
        );
    }
    assert!(!cline.level_plan(ApprovalLevel::AcceptEdits).offered);
    assert!(!cline.level_plan(ApprovalLevel::Auto).offered);
    assert!(cline.level_plan(ApprovalLevel::Full).acp_allow_all);
}

#[tokio::test]
async fn a_level_praxi_cannot_enforce_is_refused_before_anything_starts() {
    let auggie = Driver::from_id("auggie").unwrap();
    for level in [
        ApprovalLevel::Ask,
        ApprovalLevel::AcceptEdits,
        ApprovalLevel::Auto,
    ] {
        let plan = auggie.level_plan(level);
        assert!(!plan.enforced && !plan.offered, "{level:?}");
        let (bin, _host, result) = run(auggie, level, None, "", true).await;
        assert_eq!(result.unwrap_err(), CliError::NativeToolsOpen, "{level:?}");
        assert!(
            !bin.path().join("argv").exists(),
            "{level:?}: a process was started"
        );
    }
    let (bin, host, result) = run(auggie, ApprovalLevel::Full, None, "", false).await;
    result.unwrap();
    assert_eq!(log(&bin, "argv"), "--acp");
    // Full approves the forwarded permission request itself: no card.
    assert!(host.permissions.lock().is_empty());
    assert!(log(&bin, "permission_answer").contains("\"optionId\":\"yes\""));
}

#[tokio::test]
async fn a_model_is_never_guessed() {
    for driver in data_drivers() {
        assert_eq!(engine(driver).model, acp_data::ModelSelection::None);
        let level = if driver.level_plan(ApprovalLevel::Ask).enforced {
            ApprovalLevel::Ask
        } else {
            ApprovalLevel::Full
        };
        let (bin, _host, result) = run(driver, level, Some("gpt-4o"), "", true).await;
        assert_eq!(result.unwrap_err(), CliError::Protocol, "{}", driver.id());
        assert!(
            !bin.path().join("argv").exists(),
            "{}: started",
            driver.id()
        );
        // `default` is the CLI's own model: nothing is sent and the turn runs.
        let (bin, _host, result) = run(driver, level, Some("default"), "", true).await;
        result.unwrap();
        assert!(!bin.path().join("set_config_option").exists());
    }
}

#[tokio::test]
async fn permission_answers_are_the_exact_option_and_a_tool_denial_is_its_own_case() {
    let cline = Driver::from_id("cline").unwrap();
    // Normal tool use: the agent offers allow-always, allow-once, reject-once, reject-always.
    // Approve picks exactly the "once" allow, deny exactly the "once" reject; the "always"
    // options, listed first, are never taken below Full.
    for (allow, option) in [(true, "yes"), (false, "no")] {
        let (bin, host, result) = run(
            cline,
            ApprovalLevel::Ask,
            None,
            "acp-permission-options",
            allow,
        )
        .await;
        result.unwrap();
        assert_eq!(host.permissions.lock().len(), 1);
        let answer: Value = serde_json::from_str(&log(&bin, "permission_answer")).unwrap();
        assert_eq!(
            answer["result"]["outcome"]["outcome"], "selected",
            "{allow}"
        );
        assert_eq!(answer["result"]["outcome"]["optionId"], option, "{allow}");
    }
    // No "once" option at all: the request is cancelled, whatever the owner chose.
    for allow in [true, false] {
        let (bin, _host, result) =
            run(cline, ApprovalLevel::Ask, None, "acp-only-always", allow).await;
        result.unwrap();
        let answer: Value = serde_json::from_str(&log(&bin, "permission_answer")).unwrap();
        assert_eq!(
            answer["result"]["outcome"]["outcome"], "cancelled",
            "{allow}"
        );
        assert!(answer["result"]["outcome"].get("optionId").is_none());
    }
}
