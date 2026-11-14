//! Ownership persistence, runtime gating and authenticated host routes.
use super::*;
use crate::host::{HostConfig, HostEvent, HostMode, HostState};
use peer_network::{LeaderInfo, LeaderState, PeerConfig, PeerNetwork, RelayConfig};

#[test]
fn ownership_roundtrip_defaults_and_validation() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(HostConfig::load(dir.path()).unwrap(), HostConfig::default());
    let config = HostConfig {
        mode: HostMode::Host,
        state: HostState::Fresh,
        epoch: 42,
        limits: crate::host::HostLimits {
            max_computer_runs: Some(2),
            ..Default::default()
        },
        ..HostConfig::default()
    };
    config.save(dir.path()).unwrap();
    assert_eq!(HostConfig::load(dir.path()).unwrap(), config);
    let mut invalid = config;
    invalid.limits.max_computer_runs = Some(0);
    assert!(invalid.save(dir.path()).is_err());
    std::fs::write(
        dir.path().join("host.toml"),
        "mode = 'client'\nstate = 'active'",
    )
    .unwrap();
    assert!(HostConfig::load(dir.path()).is_err());
    std::fs::write(dir.path().join("host.toml"), "state = 'broken'").unwrap();
    assert!(HostConfig::load(dir.path()).is_err());
}

struct ChannelCounter(std::sync::atomic::AtomicBool);
#[async_trait]
impl crate::ChannelControl for ChannelCounter {
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
        }
    }
    fn create_pair_code(&self) -> Result<crate::dto::PairCodeDto, String> {
        Err("unused".into())
    }
}

#[tokio::test]
async fn standby_blocks_fake_clock_ticks_and_activation_restarts_components() {
    let t = setup_with(|data| {
        HostConfig {
            state: HostState::Standby,
            ..HostConfig::default()
        }
        .save(data)
        .unwrap();
    });
    let clock = Arc::new(scheduler::FakeClock::new(1_000));
    *t.state.0.scheduler_clock.lock() = Some(clock.clone());
    let (bot, chat) = make_chat(&t).await;
    let channel = Arc::new(ChannelCounter(std::sync::atomic::AtomicBool::new(false)));
    t.state.add_channel(channel.clone());
    let schedule = scheduler::Schedule::new(
        protocol::BotId(bot.parse().unwrap()),
        Some(protocol::ChatId(chat.parse().unwrap())),
        "gated",
        scheduler::ScheduleSpec::Every { seconds: 1 },
        scheduler::Payload::AgentPrompt {
            text: "hello".into(),
        },
        scheduler::MisfirePolicy::RunOnce,
        1_000,
    )
    .unwrap();
    let store = t.state.schedule_store().unwrap();
    store.insert(&schedule).unwrap();
    t.state.start_leader_components().unwrap();
    clock.advance_ms(2_000);
    tokio::task::yield_now().await;
    assert!(t.state.scheduler().is_err());
    assert!(!crate::ChannelControl::info(&*channel).running);
    assert_eq!(store.get(schedule.id).unwrap().unwrap().last_run_at, None);
    t.state.leader().activate(false).await.unwrap();
    let scheduler = t.state.scheduler().unwrap();
    scheduler.settle().await;
    assert!(crate::ChannelControl::info(&*channel).running);
    assert!(
        store
            .get(schedule.id)
            .unwrap()
            .unwrap()
            .last_run_at
            .is_some()
    );
    t.state.leader().standby().await.unwrap();
    // A caller retaining the old engine handle cannot bypass the daemon's
    // final admission gate after standby (including a raced webhook/run-now).
    let runs_before = t
        .state
        .storage()
        .list_runs(t.state.user_id())
        .unwrap()
        .len();
    scheduler.run_now(schedule.id).unwrap();
    scheduler.settle().await;
    assert_eq!(
        t.state
            .storage()
            .list_runs(t.state.user_id())
            .unwrap()
            .len(),
        runs_before
    );
    let last = store.get(schedule.id).unwrap().unwrap().last_run_at;
    clock.advance_ms(5_000);
    tokio::task::yield_now().await;
    assert_eq!(store.get(schedule.id).unwrap().unwrap().last_run_at, last);
    assert!(!crate::ChannelControl::info(&*channel).running);
    t.state.leader().activate(false).await.unwrap();
    assert!(crate::ChannelControl::info(&*channel).running);
    t.state.stop_components().await;
}

#[tokio::test]
async fn higher_epoch_demotes_and_force_activation_increments_persisted_epoch() {
    let t = setup();
    t.state.start_scheduler().unwrap();
    let key =
        peer_network::identity::load_or_create_secret_key(&t._dir.path().join("test-peer.key"))
            .unwrap()
            .public()
            .to_string();
    t.state
        .leader()
        .observe(
            &key,
            LeaderInfo {
                epoch: 8,
                state: LeaderState::Active,
            },
        )
        .await
        .unwrap();
    let config = t.state.leader().config();
    assert_eq!((config.state, config.epoch), (HostState::Standby, 8));
    assert_eq!(config.leader.as_deref(), Some(key.as_str()));
    assert!(t.state.scheduler().is_err());
    assert!(t.state.leader().activate(false).await.is_err());
    let config = t.state.leader().activate(true).await.unwrap();
    assert_eq!((config.state, config.epoch), (HostState::Active, 9));
    assert_eq!(HostConfig::load(t._dir.path()).unwrap(), config);
    t.state.stop_components().await;
}

#[tokio::test]
async fn equal_epoch_stops_both_and_emits_notification() {
    let a = setup();
    let b = setup();
    let mut a_events = a.state.leader().subscribe();
    let mut b_events = b.state.leader().subscribe();
    a.state.start_scheduler().unwrap();
    b.state.start_scheduler().unwrap();
    let info = LeaderInfo {
        epoch: 0,
        state: LeaderState::Active,
    };
    let a_key =
        peer_network::identity::load_or_create_secret_key(&a._dir.path().join("test-peer.key"))
            .unwrap()
            .public()
            .to_string();
    let b_key =
        peer_network::identity::load_or_create_secret_key(&b._dir.path().join("test-peer.key"))
            .unwrap()
            .public()
            .to_string();
    tokio::try_join!(
        a.state.leader().observe(&a_key, info),
        b.state.leader().observe(&b_key, info)
    )
    .unwrap();
    for (state, rx) in [(&a.state, &mut a_events), (&b.state, &mut b_events)] {
        assert_eq!(state.leader().config().state, HostState::Standby);
        assert!(state.scheduler().is_err());
        assert!(matches!(
            rx.recv().await.unwrap(),
            HostEvent::HostStateChanged(_)
        ));
        assert!(
            matches!(rx.recv().await.unwrap(), HostEvent::HostConflict(c) if c.reason == "equal_epoch" && !c.notification.is_empty())
        );
    }
}

#[tokio::test]
async fn all_host_routes_require_auth_and_health_exposes_ownership() {
    let t = setup();
    for (method, path, body) in [
        (Method::GET, "/v1/host".into(), None),
        (
            Method::POST,
            "/v1/host/invite".into(),
            Some(json!({"role":"phone"})),
        ),
        (
            Method::DELETE,
            format!("/v1/host/devices/{}", "a".repeat(64)),
            None,
        ),
        (Method::POST, "/v1/host/standby".into(), None),
        (
            Method::POST,
            "/v1/host/activate".into(),
            Some(json!({"force":false})),
        ),
        (Method::POST, "/v1/host/update".into(), None),
    ] {
        assert_eq!(
            send(&t.app, method.clone(), &path, body.clone(), &[])
                .await
                .status,
            StatusCode::UNAUTHORIZED
        );
        let request = Request::builder()
            .method(method)
            .uri(path)
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            crate::peer_router(t.state.clone())
                .oneshot(request)
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
    }
    call(&t, Method::POST, "/v1/host/standby", None).await;
    let health = call(&t, Method::GET, "/v1/health", None).await.json();
    assert_eq!(health["host"]["mode"], "standalone");
    assert_eq!(health["host"]["state"], "standby");
    assert_eq!(health["host"]["epoch"], 0);
    assert_eq!(
        call(&t, Method::POST, "/v1/host/update", None).await.status,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn phone_can_manage_ownership_but_cannot_import_handover_or_write_secrets() {
    let t = setup();
    for (path, body, status) in [
        ("/v1/host/standby", None, StatusCode::OK),
        (
            "/v1/host/activate",
            Some(json!({"force":false})),
            StatusCode::OK,
        ),
        ("/v1/host/import", Some(json!({})), StatusCode::FORBIDDEN),
        ("/v1/host/handover", Some(json!({})), StatusCode::FORBIDDEN),
        (
            "/v1/secrets/test",
            Some(json!({"value":"test"})),
            StatusCode::FORBIDDEN,
        ),
    ] {
        let request = Request::builder()
            .method(Method::POST)
            .uri(path)
            .header("content-type", "application/json")
            .extension(crate::PeerIdentity {
                key: "phone".into(),
                role: crate::PeerRole::Phone,
            })
            .body(body.map_or_else(Body::empty, |v| Body::from(v.to_string())))
            .unwrap();
        assert_eq!(
            crate::peer_router(t.state.clone())
                .oneshot(request)
                .await
                .unwrap()
                .status(),
            status,
            "{path}"
        );
    }
    t.state.stop_components().await;
}

async fn attach_network(t: &T) -> PeerNetwork {
    let mut config = PeerConfig::new(t._dir.path().join("peer"));
    config.relay = RelayConfig::Disabled;
    config.bind_addr = Some("127.0.0.1:0".parse().unwrap());
    let net = PeerNetwork::start(config, None).await.unwrap();
    t.state.leader().set_network(net.clone());
    net
}

#[tokio::test]
async fn install_command_only_for_host_invites_from_non_host_install_and_default_ttls() {
    for host_mode in [false, true] {
        let t = setup_with(|data| {
            if host_mode {
                HostConfig {
                    mode: HostMode::Host,
                    state: HostState::Fresh,
                    ..HostConfig::default()
                }
                .save(data)
                .unwrap();
            }
        });
        let net = attach_network(&t).await;
        for role in ["desktop", "phone", "node", "host"] {
            let before = storage::now_ms();
            let response = call(
                &t,
                Method::POST,
                "/v1/host/invite",
                Some(json!({"role":role})),
            )
            .await;
            assert_eq!(response.status, StatusCode::OK);
            let invite = response.json();
            assert_eq!(
                invite["install_command"].is_string(),
                role == "host" && !host_mode
            );
            assert!(invite["qr_svg"].as_str().unwrap().contains("<svg"));
            let ttl_ms = if role == "host" { 900_000 } else { 120_000 };
            assert!(
                (before + ttl_ms..=storage::now_ms() + ttl_ms)
                    .contains(&invite["expires_at_ms"].as_i64().unwrap())
            );
        }
        assert_eq!(
            call(
                &t,
                Method::POST,
                "/v1/host/invite",
                Some(json!({"role":"host", "ttl_secs":0}))
            )
            .await
            .status,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            call(
                &t,
                Method::POST,
                "/v1/host/invite",
                Some(json!({"role":"phone", "ttl_secs":60}))
            )
            .await
            .status,
            StatusCode::OK
        );
        assert_eq!(
            call(
                &t,
                Method::POST,
                "/v1/host/invite",
                Some(json!({"role":"phone"}))
            )
            .await
            .status,
            StatusCode::TOO_MANY_REQUESTS
        );
        net.shutdown().await;
    }
}

#[tokio::test]
async fn telegram_conflict_demotes_and_emits_reason() {
    let t = setup();
    let mut events = t.state.leader().subscribe();
    t.state.leader().telegram_conflict().await.unwrap();
    assert_eq!(t.state.leader().config().state, HostState::Standby);
    assert!(matches!(
        events.recv().await.unwrap(),
        HostEvent::HostStateChanged(_)
    ));
    assert!(
        matches!(events.recv().await.unwrap(), HostEvent::HostConflict(c) if c.reason == "telegram_409")
    );
}

#[tokio::test]
async fn host_notifications_reach_the_global_sse_stream_without_replay_ids() {
    let t = setup();
    let mut body = super::open_global(&t, None).await;
    t.state.leader().telegram_conflict().await.unwrap();
    let frames = super::read_until(&mut body, |frames| {
        frames.iter().any(|frame| frame.1 == "host_conflict")
    })
    .await;
    let names: Vec<_> = frames.iter().map(|frame| frame.1.as_str()).collect();
    assert_eq!(names, ["host_state_changed", "host_conflict"]);
    assert!(frames.iter().all(|frame| frame.0.is_empty()));
    assert_eq!(frames[0].2["state"], "standby");
    assert_eq!(frames[1].2["reason"], "telegram_409");
}

#[tokio::test]
async fn handover_handle_can_commit_client_mode_but_never_decrease_epochs() {
    let t = setup();
    let target = HostConfig {
        mode: HostMode::Client,
        state: HostState::Standby,
        epoch: 7,
        ..HostConfig::default()
    };
    t.state
        .leader()
        .transition_to(target.clone())
        .await
        .unwrap();
    assert_eq!(HostConfig::load(t._dir.path()).unwrap(), target);
    assert!(
        t.state
            .leader()
            .transition_to(HostConfig::default())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn standby_still_stops_components_when_atomic_persistence_fails() {
    let t = setup();
    t.state.leader().start().await.unwrap();
    std::fs::create_dir(t._dir.path().join("host.toml")).unwrap();
    assert!(t.state.leader().standby().await.is_err());
    assert_eq!(t.state.leader().config().state, HostState::Standby);
    assert!(t.state.scheduler().is_err());
    assert!(t.state.0.scheduler.lock().is_none());
}

#[test]
fn openapi_operation_ids_are_unique_for_generated_clients() {
    let document: Value = serde_json::from_str(&crate::openapi_json()).unwrap();
    let mut ids = std::collections::HashSet::new();
    for path in document["paths"].as_object().unwrap().values() {
        for operation in path.as_object().unwrap().values() {
            if let Some(id) = operation["operationId"].as_str() {
                assert!(ids.insert(id), "duplicate operationId {id}");
            }
        }
    }
}

#[tokio::test]
async fn fresh_host_can_run_without_claiming_equal_epoch_active_ownership() {
    let t = setup_with(|data| {
        HostConfig {
            mode: HostMode::Host,
            state: HostState::Fresh,
            ..HostConfig::default()
        }
        .save(data)
        .unwrap();
    });
    t.state.leader().start().await.unwrap();
    let peer =
        peer_network::identity::load_or_create_secret_key(&t._dir.path().join("test-peer.key"))
            .unwrap()
            .public()
            .to_string();
    t.state
        .leader()
        .observe(
            &peer,
            LeaderInfo {
                epoch: 0,
                state: LeaderState::Active,
            },
        )
        .await
        .unwrap();
    assert_eq!(t.state.leader().config().state, HostState::Fresh);
    assert!(t.state.scheduler().is_ok());
    t.state.stop_components().await;
}

#[test]
fn main_format_host_limits_and_unknown_keys_survive_ownership_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let original = "future_setting='keep'\n[limits]\nmax_computer_runs=2\nbrowser_idle_seconds=180\nfuture_limit=7\n";
    std::fs::write(dir.path().join("host.toml"), original).unwrap();
    let mut config = HostConfig::load(dir.path()).unwrap();
    assert_eq!(config.limits.browser_idle_seconds, Some(180));
    config.state = HostState::Standby;
    config.save(dir.path()).unwrap();
    let saved: toml::Table = std::fs::read_to_string(dir.path().join("host.toml")).unwrap().parse().unwrap();
    let before: toml::Table = original.parse().unwrap();
    assert_eq!(saved["limits"], before["limits"]);
    assert_eq!(saved["future_setting"], before["future_setting"]);
    assert_eq!(HostConfig::load(dir.path()).unwrap(), config);
}
