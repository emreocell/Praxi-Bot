//! Safe mode: decided at process start, pauses (never deletes) everything that acts alone.
use super::*;

struct Probe(std::sync::atomic::AtomicBool);
#[async_trait]
impl crate::ChannelControl for Probe {
    fn start(&self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
    async fn stop(&self) {
        self.0.store(false, std::sync::atomic::Ordering::SeqCst);
    }
    fn info(&self) -> crate::dto::ChannelDto {
        crate::dto::ChannelDto {
            id: "telegram".into(),
            configured: true,
            enabled: true,
            running: self.0.load(std::sync::atomic::Ordering::SeqCst),
            paired_users: 0,
            status: None,
        }
    }
    fn create_pair_code(&self) -> Result<crate::dto::PairCodeDto, String> {
        Err("unused".into())
    }
}

fn safe_setup(safe: bool, config: LoopConfig) -> T {
    let dir = tempfile::tempdir().unwrap();
    let storage = Arc::new(Storage::open(dir.path()).unwrap());
    let tools = ToolRegistry::new();
    tools.register(Arc::new(Danger));
    let agent = AgentLoop::new(
        Arc::clone(&storage),
        tools,
        Providers::single(Arc::new(FakeProvider)),
        config,
    );
    let mut cfg = DaemonConfig::new(TOKEN);
    cfg.safe_mode = safe;
    let state = AppState::new(agent.clone(), storage, cfg).unwrap();
    state.set_owner_key(OWNER_KEY.into());
    T {
        app: router(state.clone()),
        state,
        agent,
        _dir: dir,
    }
}

#[tokio::test]
async fn safe_mode_starts_no_scheduler_and_no_channel_and_keeps_the_schedule() {
    let t = safe_setup(true, LoopConfig::default());
    let clock = Arc::new(scheduler::FakeClock::new(1_000));
    *t.state.0.scheduler_clock.lock() = Some(clock.clone());
    let (bot, chat) = make_chat(&t).await;
    let channel = Arc::new(Probe(std::sync::atomic::AtomicBool::new(false)));
    t.state.add_channel(channel.clone());
    let schedule = scheduler::Schedule::new(
        protocol::BotId(bot.parse().unwrap()),
        Some(protocol::ChatId(chat.parse().unwrap())),
        "due",
        scheduler::ScheduleSpec::Every { seconds: 1 },
        scheduler::Payload::AgentPrompt { text: "hi".into() },
        scheduler::MisfirePolicy::RunOnce,
        1_000,
    )
    .unwrap();
    let store = t.state.schedule_store().unwrap();
    store.insert(&schedule).unwrap();

    t.state.start_leader_components().unwrap();
    t.state.start_scheduler().unwrap();
    clock.advance_ms(5_000);
    tokio::task::yield_now().await;

    assert!(t.state.scheduler().is_err(), "no scheduler in safe mode");
    assert!(!crate::ChannelControl::info(&*channel).running);
    let kept = store.get(schedule.id).unwrap().unwrap();
    assert_eq!(kept.last_run_at, None, "the due schedule never fired");
    assert_eq!(kept.name, "due", "and it was not changed or deleted");
    let health = send(&t.app, Method::GET, "/v1/health", None, &[]).await;
    assert_eq!(health.json()["safe_mode"], true);
    assert_eq!(health.json()["channels_active"], false);
}

#[tokio::test]
async fn normal_start_still_runs_the_scheduler_and_channels() {
    let t = safe_setup(false, LoopConfig::default());
    let channel = Arc::new(Probe(std::sync::atomic::AtomicBool::new(false)));
    t.state.add_channel(channel.clone());
    t.state.start_leader_components().unwrap();
    assert!(t.state.scheduler().is_ok());
    assert!(crate::ChannelControl::info(&*channel).running);
    let health = send(&t.app, Method::GET, "/v1/health", None, &[]).await;
    assert_eq!(health.json()["safe_mode"], false);
}

#[tokio::test]
async fn safe_mode_chat_answers_but_a_tool_call_is_refused_without_asking() {
    let t = safe_setup(
        true,
        LoopConfig {
            safe_mode: true,
            ..LoopConfig::default()
        },
    );
    let (_, chat) = make_chat(&t).await;
    let run = send_msg(&t, &chat, "call:danger_run {}", "queue").await["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    wait_state(&t, &run, "succeeded").await;
    let frames = events(&t, &format!("?run_id={run}"), None).await;
    let kinds: Vec<&str> = frames.iter().map(|f| f.1.as_str()).collect();
    assert!(!kinds.contains(&"approval_needed"), "{kinds:?}");
    let done = frames
        .iter()
        .find(|f| f.1 == "tool_call_completed")
        .expect("the refusal is reported as the call's result");
    assert!(
        done.2["event"]["error"]
            .as_str()
            .is_some_and(|e| e.contains("unknown tool")),
        "{}",
        done.2
    );
    let all =
        serde_json::to_string(&frames.iter().map(|f| f.2.clone()).collect::<Vec<_>>()).unwrap();
    assert!(!all.contains("boom done"), "the tool must not have run");
}

#[test]
fn no_api_route_can_switch_safe_mode() {
    let spec = crate::openapi::openapi_json().to_lowercase();
    let doc: serde_json::Value = serde_json::from_str(&spec).unwrap();
    for path in doc["paths"].as_object().unwrap().keys() {
        assert!(!path.contains("safe"), "route {path} must not exist");
    }
}
