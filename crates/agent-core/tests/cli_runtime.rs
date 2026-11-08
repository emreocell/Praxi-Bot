//! Fork the harness with a private PATH so no test mutates process-global env.
use agent_core::{AgentLoop, ApprovalScope, BotProfile, LoopConfig, Providers};
use async_trait::async_trait;
use permission_engine::Policy;
use protocol::{BotId, ChatId, Event, UserId};
use provider_catalog::{
    ChatChunk, ChatProvider, ChatRequest, ProviderConfig, ProviderError, ProviderSettings,
    config::{CliEngine, CliSettings},
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use storage::{Bot, Chat, Storage, User};
use task_runtime::RunState;
use tool_registry::{
    Effect, PermissionCategory, Tool, ToolContext, ToolError, ToolOutput, ToolRegistry, ToolSpec,
};

struct NoHttp;
#[async_trait]
impl ChatProvider for NoHttp {
    fn id(&self) -> &'static str {
        "unused-http"
    }
    async fn stream(
        &self,
        request: ChatRequest,
    ) -> Result<futures::stream::BoxStream<'static, Result<ChatChunk, ProviderError>>, ProviderError>
    {
        if request.model.0 == "unused-http/fail" {
            return Err(ProviderError::Upstream {
                status: 503,
                message: "fixture failure".into(),
            });
        }
        panic!("CLI turn reached ChatProvider::stream")
    }
    async fn list_models(&self) -> Result<Vec<String>, ProviderError> {
        Ok(vec![])
    }
}
struct KnownTool {
    spec: ToolSpec,
    count: AtomicUsize,
}
#[async_trait]
impl Tool for KnownTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }
    async fn call(&self, _: &ToolContext, _: Value) -> Result<ToolOutput, ToolError> {
        self.count.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput::text("known result"))
    }
}
fn run_case(case: &str) {
    if std::env::var("C062_CASE").ok().as_deref() == Some(case) {
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(child(case));
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(
        env!("CARGO_BIN_EXE_agent-cli-fixture"),
        dir.path()
            .join(format!("fixture{}", std::env::consts::EXE_SUFFIX)),
    )
    .unwrap();
    std::fs::write(dir.path().join("auth.json"), "C062_AUTH_SENTINEL").unwrap();
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", case, "--nocapture"])
        .env("C062_CASE", case)
        .env("C062_ROOT", dir.path())
        .env("PATH", dir.path())
        .env("FAKE_LOG", dir.path())
        .env(
            "FAKE_MODE",
            if case == "shutdown_reaps_cli_tree" {
                "hang-tree"
            } else {
                ""
            },
        )
        .env("CODEX_HOME", dir.path())
        .env("CLAUDE_CONFIG_DIR", dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
#[test]
fn claude_allowed_turn_streams_tools_and_usage() {
    run_case("claude_allowed_turn_streams_tools_and_usage");
}
#[test]
fn claude_denied_tool_returns_error() {
    run_case("claude_denied_tool_returns_error");
}
#[test]
fn codex_approval_allow_runs_once() {
    run_case("codex_approval_allow_runs_once");
}
#[test]
fn codex_approval_deny_skips_tool() {
    run_case("codex_approval_deny_skips_tool");
}
#[test]
fn shutdown_reaps_cli_tree() {
    run_case("shutdown_reaps_cli_tree");
}
#[test]
fn http_fallback_hands_off_to_cli() {
    run_case("http_fallback_hands_off_to_cli");
}

#[allow(clippy::too_many_lines)]
async fn child(case: &str) {
    let root = std::path::PathBuf::from(std::env::var("C062_ROOT").unwrap());
    let store = Arc::new(Storage::open(root.join("store")).unwrap());
    let (user, bot, chat) = (UserId::new(), BotId::new(), ChatId::new());
    store
        .insert_user(&User {
            id: user,
            name: "test".into(),
            created_at_ms: 0,
        })
        .unwrap();
    store
        .insert_bot(&Bot {
            id: bot,
            user_id: user,
            name: "test".into(),
            config: json!({}),
            created_at_ms: 0,
        })
        .unwrap();
    store
        .insert_chat(&Chat {
            id: chat,
            user_id: user,
            bot_id: bot,
            title: "test".into(),
            created_at_ms: 0,
        })
        .unwrap();
    let codex = case.starts_with("codex") || case.starts_with("shutdown");
    let deny = case.contains("denied") || case.contains("deny");
    let id = if codex { "cli:codex" } else { "cli:claude" };
    let mut settings = ProviderSettings::new(id, 0);
    settings.cli = Some(CliSettings::Cli {
        engine: if codex {
            CliEngine::Codex
        } else {
            CliEngine::Claude
        },
        command: "fixture".into(),
    });
    ProviderConfig {
        providers: vec![settings],
        default_provider: None,
    }
    .save(&provider_catalog::config_path(store.data_dir()))
    .unwrap();
    let tool = Arc::new(KnownTool {
        spec: ToolSpec {
            name: "known.tool".into(),
            description: "Known".into(),
            input_schema: json!({"type":"object"}),
            effect: Effect::Write,
            category: PermissionCategory::Fs,
            platforms: vec![],
        },
        count: AtomicUsize::new(0),
    });
    let registry = ToolRegistry::new();
    registry.register(tool.clone());
    let lp = AgentLoop::new(
        store.clone(),
        registry,
        Providers::single(Arc::new(NoHttp)),
        LoopConfig {
            global_policy: if codex {
                Policy::approve_destructive()
            } else if deny {
                Policy::read_only()
            } else {
                Policy::full()
            },
            ..LoopConfig::default()
        },
    );
    let fallback = case == "http_fallback_hands_off_to_cli";
    let executor = if fallback { "unused-http/fail" } else { id };
    let fallbacks = if fallback {
        "fallback=['cli:claude']"
    } else {
        ""
    };
    lp.add_profile(
        bot,
        BotProfile::from_toml_str(&format!(
            "[bot]\nid='fixture'\nname='Fixture'\n[models]\nexecutor='{executor}'\n{fallbacks}"
        ))
        .unwrap(),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(chat, "secret prompt\n%PATH%").unwrap();
    if case == "shutdown_reaps_cli_tree" {
        tokio::time::timeout(Duration::from_secs(20), async {
            while !root.join("descendant/pid").exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        lp.shutdown(Duration::ZERO).await;
        assert_eq!(lp.runtime().state(run).unwrap(), RunState::Paused);
        for path in [root.join("pid"), root.join("descendant/pid")] {
            let pid = std::fs::read_to_string(path).unwrap();
            tokio::time::timeout(Duration::from_secs(15), async {
                loop {
                    #[cfg(windows)]
                    let alive = String::from_utf8_lossy(
                        &tokio::process::Command::new(
                            std::path::PathBuf::from(std::env::var("SYSTEMROOT").unwrap())
                                .join("System32/tasklist.exe"),
                        )
                        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
                        .output()
                        .await
                        .unwrap()
                        .stdout,
                    )
                    .contains(&format!("\"{pid}\""));
                    #[cfg(not(windows))]
                    let alive = tokio::process::Command::new("/bin/kill")
                        .args(["-0", &pid])
                        .output()
                        .await
                        .unwrap()
                        .status
                        .success();
                    if !alive {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(30)).await;
                }
            })
            .await
            .unwrap();
        }
        // Restarting a paused CLI turn must not replay unknown side effects.
        let old_pid = std::fs::read_to_string(root.join("pid")).unwrap();
        let restarted = AgentLoop::new(
            store.clone(),
            ToolRegistry::new(),
            Providers::single(Arc::new(NoHttp)),
            LoopConfig::default(),
        );
        restarted.add_profile(
            bot,
            BotProfile::from_toml_str(
                "[bot]\nid='fixture'\nname='Fixture'\n[models]\nexecutor='cli:codex'",
            )
            .unwrap(),
        );
        restarted.resume(run).unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(10), restarted.wait(run))
                .await
                .unwrap()
                .unwrap(),
            RunState::Failed
        );
        assert_eq!(std::fs::read_to_string(root.join("pid")).unwrap(), old_pid);
        restarted.shutdown(Duration::ZERO).await;
        return;
    }
    if codex {
        let approval = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if let Event::ApprovalNeeded(a) = rx.recv().await.unwrap().envelope.event {
                    break a;
                }
            }
        })
        .await
        .unwrap();
        if deny {
            lp.deny(approval.call_id, user, None).unwrap();
        } else {
            lp.approve(approval.call_id, ApprovalScope::Once, user)
                .unwrap();
        }
    }
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(20), lp.wait(run))
            .await
            .unwrap()
            .unwrap(),
        RunState::Succeeded
    );
    assert_eq!(tool.count.load(Ordering::SeqCst), usize::from(!deny));
    let events = storage::read_event_log(root.join("store").as_path(), run).unwrap();
    let event_text = format!("{events:?}");
    assert!(!event_text.contains("C062_AUTH_SENTINEL"));
    let approvals = events
        .iter()
        .filter(|e| matches!(e.event, Event::ApprovalNeeded(_)))
        .count();
    assert_eq!(approvals, usize::from(codex));
    if !deny || !codex {
        assert!(event_text.contains("MessageDelta"));
        assert!(event_text.contains("ToolCallRequested"));
        assert!(event_text.contains("ToolCallCompleted"));
        let usage = store
            .list_audit(user)
            .unwrap()
            .into_iter()
            .find(|a| a.action == "usage")
            .unwrap();
        assert_eq!(usage.detail["cost_source"], "subscription_estimate");
        assert_eq!(usage.detail["prompt_tokens"], 12);
    }
    lp.shutdown(Duration::ZERO).await;
}
