//! A coding CLI that was just added works at the first message: with the settings the desktop's
//! one consent step stores it runs, and without them it is refused cleanly (nothing is launched).
//! One forked harness per engine, with a private PATH so no test mutates process-global env.
use agent_core::{AgentLoop, ApprovalScope, BotProfile, LoopConfig, Providers};
use async_trait::async_trait;
use cli_provider::{ApprovalLevel, Driver};
use permission_engine::Policy;
use protocol::{BotId, ChatId, Event, UserId};
use provider_catalog::{
    ChatChunk, ChatProvider, ChatRequest, ProviderConfig, ProviderError, ProviderSettings,
    config::{CliEngine, CliSettings},
};
use serde_json::json;
use std::{sync::Arc, time::Duration};
use storage::{Bot, Chat, Storage, User};
use task_runtime::RunState;
use tool_registry::ToolRegistry;

struct NoHttp;
#[async_trait]
impl ChatProvider for NoHttp {
    fn id(&self) -> &'static str {
        "unused-http"
    }
    async fn stream(
        &self,
        _: ChatRequest,
    ) -> Result<futures::stream::BoxStream<'static, Result<ChatChunk, ProviderError>>, ProviderError>
    {
        panic!("CLI turn reached ChatProvider::stream")
    }
    async fn list_models(&self) -> Result<Vec<String>, ProviderError> {
        Ok(vec![])
    }
}

/// What the consent step starts on and stores for a fresh bot: Ask when offered, else the first
/// offered level.
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

#[test]
fn every_engine_runs_the_first_message_once_the_owner_agreed_in_the_picker() {
    if let Ok(id) = std::env::var("NATIVE_FIRST_ENGINE") {
        tokio::runtime::Runtime::new().unwrap().block_on(child(&id));
        return;
    }
    for driver in Driver::ALL {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        std::fs::create_dir(dir.path().join("work")).unwrap();
        // `vibe-acp` is its own entry point; the fixture tells it by its name.
        let name = if driver == Driver::Vibe {
            "vibe-acp"
        } else {
            "fixture"
        };
        std::fs::copy(
            env!("CARGO_BIN_EXE_native-cli-fixture"),
            bin.join(format!("{name}{}", std::env::consts::EXE_SUFFIX)),
        )
        .unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .env_clear()
            .env("HOME", dir.path())
            .env("USERPROFILE", dir.path())
            .env("APPDATA", dir.path().join("roaming"))
            .env("LOCALAPPDATA", dir.path().join("local"));
        for key in ["SYSTEMROOT", "WINDIR", "TEMP", "TMP"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        let out = command
            .args([
                "--exact",
                "every_engine_runs_the_first_message_once_the_owner_agreed_in_the_picker",
                "--nocapture",
            ])
            .env("NATIVE_FIRST_ENGINE", driver.id())
            .env("NATIVE_FIRST_ROOT", dir.path())
            .env("PATH", &bin)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}:\n{}\n{}",
            driver.id(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[allow(clippy::too_many_lines)] // One harness: setup, refused default, then the consented first message.
async fn child(id: &str) {
    let driver = Driver::from_id(id).unwrap();
    let root = std::path::PathBuf::from(std::env::var("NATIVE_FIRST_ROOT").unwrap());
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
    let provider_id = format!("cli:{id}");
    let mut settings = ProviderSettings::new(&provider_id, 0);
    settings.cli = Some(CliSettings::Cli {
        engine: serde_json::from_value::<CliEngine>(json!(id)).unwrap(),
        command: if driver == Driver::Vibe {
            "vibe-acp"
        } else {
            "fixture"
        }
        .into(),
    });
    ProviderConfig {
        quarantined: std::collections::BTreeMap::default(),
        providers: vec![settings],
        default_provider: None,
    }
    .save(&provider_catalog::config_path(store.data_dir()))
    .unwrap();
    let lp = AgentLoop::new(
        store.clone(),
        ToolRegistry::new(),
        Providers::single(Arc::new(NoHttp)),
        LoopConfig {
            model_retry: task_runtime::RetryPolicy {
                max_attempts: 1,
                ..LoopConfig::default().model_retry
            },
            global_policy: Policy::full(),
            ..LoopConfig::default()
        },
    );
    let mut rx = lp.subscribe();
    let level = level_picker_starts_on(driver);
    let workspace = root.join("work");
    let native = format!(
        "[bot]\nid='fixture'\nname='Fixture'\n[models]\nexecutor='{provider_id}/default'\n\
         [engine]\nmode='native_tools'\napproval_level='{}'\nworkspace='{}'\n\
         [engine.consent]\nnative_tools_at_ms=1\n{}",
        level.as_str(),
        workspace.display().to_string().replace('\\', "/"),
        if level == ApprovalLevel::Full {
            "full_access_at_ms=1\n"
        } else {
            ""
        }
    );
    let governed = format!(
        "[bot]\nid='fixture'\nname='Fixture'\n[models]\nexecutor='{provider_id}/default'\n"
    );
    // Claude Code also runs confined; every other engine is refused before anything starts,
    // and says so in words the desktop turns into its consent step.
    if driver != Driver::Claude {
        lp.add_profile(bot, BotProfile::from_toml_str(&governed).unwrap());
        let run = lp.run_turn(chat, "hello").unwrap();
        assert_eq!(
            wait(&lp, &mut rx, user, run).await,
            RunState::Failed,
            "{id}: default settings"
        );
        let events = format!(
            "{:?}",
            storage::read_event_log(root.join("store").as_path(), run).unwrap()
        );
        assert!(events.contains("engine_needs_native"), "{id}: {events}");
    }
    lp.add_profile(bot, BotProfile::from_toml_str(&native).unwrap());
    let run = lp.run_turn(chat, "hello").unwrap();
    let state = wait(&lp, &mut rx, user, run).await;
    let events = format!(
        "{:?}",
        storage::read_event_log(root.join("store").as_path(), run).unwrap()
    );
    assert_eq!(
        state,
        RunState::Succeeded,
        "{id} at {}: {events}",
        level.as_str()
    );
    assert!(events.contains("MessageDelta"), "{id}: {events}");
    lp.shutdown(Duration::ZERO).await;
}

/// Wait for the run, answering the fake CLI's permission prompt (its own `touch note.txt`) once.
async fn wait(
    lp: &AgentLoop,
    rx: &mut tokio::sync::broadcast::Receiver<agent_core::StreamItem>,
    user: UserId,
    run: protocol::RunId,
) -> RunState {
    let mut done = Box::pin(tokio::time::timeout(Duration::from_secs(30), lp.wait(run)));
    loop {
        tokio::select! {
            state = &mut done => return state.unwrap().unwrap(),
            event = rx.recv() => {
                if let Ok(envelope) = event
                    && let Event::ApprovalNeeded(a) = envelope.envelope.event
                {
                    lp.approve(a.call_id, ApprovalScope::Once, user).unwrap();
                }
            }
        }
    }
}
