//! Ownership persistence, runtime gating and authenticated host routes.
use super::*;
use crate::host::{HostConfig, HostEvent, HostMode, HostState};
use peer_network::{
    HeartbeatConfig, LeaderInfo, LeaderState, PeerConfig, PeerNetwork, RelayConfig,
};

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
            status: None,
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
    attach_network_as(t, peer_network::Role::Desktop).await
}

async fn attach_network_as(t: &T, role: peer_network::Role) -> PeerNetwork {
    let mut config = PeerConfig::new(t._dir.path().join("peer"));
    config.local_role = role;
    config.relay = RelayConfig::Disabled;
    config.bind_addr = Some("127.0.0.1:0".parse().unwrap());
    let net = PeerNetwork::start(config, None).await.unwrap();
    t.state.leader().set_network(net.clone());
    net
}

#[tokio::test]
async fn installer_command_is_unavailable_until_join_support_lands_and_default_ttls() {
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
            assert!(invite["install_command"].is_null());
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
async fn telegram_conflict_preserves_ownership_and_scheduler_and_emits_reason() {
    let t = setup();
    t.state.leader().start().await.unwrap();
    let before = t.state.leader().config();
    let mut events = t.state.leader().subscribe();
    t.state.leader().telegram_conflict();
    assert_eq!(t.state.leader().config(), before);
    assert_eq!(HostConfig::load(t._dir.path()).unwrap(), before);
    assert!(t.state.scheduler().is_ok());
    assert!(
        matches!(events.recv().await.unwrap(), HostEvent::HostConflict(c) if c.reason == "telegram_409")
    );
    t.state.stop_components().await;
}

#[tokio::test]
async fn host_notifications_reach_the_global_sse_stream_without_replay_ids() {
    let t = setup();
    let mut body = super::open_global(&t, None).await;
    t.state.leader().telegram_conflict();
    let frames = super::read_until(&mut body, |frames| {
        frames.iter().any(|frame| frame.1 == "host_conflict")
    })
    .await;
    let names: Vec<_> = frames.iter().map(|frame| frame.1.as_str()).collect();
    assert_eq!(names, ["host_conflict"]);
    assert!(frames.iter().all(|frame| frame.0.is_empty()));
    assert_eq!(frames[0].2["reason"], "telegram_409");
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
    let saved: toml::Table = std::fs::read_to_string(dir.path().join("host.toml"))
        .unwrap()
        .parse()
        .unwrap();
    let before: toml::Table = original.parse().unwrap();
    assert_eq!(saved["limits"], before["limits"]);
    assert_eq!(saved["future_setting"], before["future_setting"]);
    assert_eq!(HostConfig::load(dir.path()).unwrap(), config);
}

async fn phone_request(t: &T, method: Method, path: &str, body: Option<Value>) -> StatusCode {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .extension(crate::PeerIdentity {
            key: "phone-key".into(),
            role: crate::PeerRole::Phone,
        })
        .body(body.map_or_else(Body::empty, |v| Body::from(v.to_string())))
        .unwrap();
    crate::peer_router(t.state.clone())
        .oneshot(request)
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn phone_cannot_mint_any_invite() {
    let t = setup();
    let net = attach_network(&t).await;
    for role in ["desktop", "host", "node", "phone"] {
        assert_eq!(
            phone_request(
                &t,
                Method::POST,
                "/v1/host/invite",
                Some(json!({"role":role}))
            )
            .await,
            StatusCode::FORBIDDEN,
            "{role}"
        );
    }
    let desktop = Request::builder()
        .method(Method::POST)
        .uri("/v1/host/invite")
        .header("content-type", "application/json")
        .extension(crate::PeerIdentity {
            key: "desktop-key".into(),
            role: crate::PeerRole::Desktop,
        })
        .body(Body::from(json!({"role":"host"}).to_string()))
        .unwrap();
    assert_eq!(
        crate::peer_router(t.state.clone())
            .oneshot(desktop)
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(net.devices().len(), 0);
    net.shutdown().await;
}

#[tokio::test]
async fn denied_invite_and_revoke_attempts_are_audited() {
    let t = setup();
    let other = setup();
    let net = attach_network(&t).await;
    let other_net = attach_network_as(&other, peer_network::Role::Desktop).await;
    let invite = net.create_invite(peer_network::Role::Desktop).await;
    other_net
        .pair(&peer_network::PairTicket::parse(&invite.ticket).unwrap())
        .await
        .unwrap();
    assert_eq!(
        phone_request(
            &t,
            Method::POST,
            "/v1/host/invite",
            Some(json!({"role":"phone"}))
        )
        .await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        phone_request(
            &t,
            Method::DELETE,
            &format!("/v1/host/devices/{}", other_net.id()),
            None
        )
        .await,
        StatusCode::FORBIDDEN
    );
    assert!(net.device(&other_net.id()).is_some());
    let audit = t.state.storage().list_audit(t.state.user_id()).unwrap();
    for action in ["host.invite", "host.revoke"] {
        let row = audit.iter().find(|entry| entry.action == action).unwrap();
        assert_eq!(row.actor, "phone:phone-key");
        assert_eq!(row.detail["detail"]["phase"], "denied");
        assert!(!row.detail.to_string().contains(&invite.ticket));
        assert!(!row.detail.to_string().contains(&invite.code));
    }
    net.shutdown().await;
    other_net.shutdown().await;
}

#[tokio::test]
async fn phone_cannot_revoke_non_phone_devices() {
    for role in [
        peer_network::Role::Desktop,
        peer_network::Role::Host,
        peer_network::Role::Node,
        peer_network::Role::Phone,
    ] {
        let t = setup();
        let other = setup();
        let net = attach_network(&t).await;
        let other_net = attach_network_as(&other, role).await;
        let invite = net.create_invite(role).await;
        other_net
            .pair(&peer_network::PairTicket::parse(&invite.ticket).unwrap())
            .await
            .unwrap();
        let status = phone_request(
            &t,
            Method::DELETE,
            &format!("/v1/host/devices/{}", other_net.id()),
            None,
        )
        .await;
        assert_eq!(
            status,
            if role == peer_network::Role::Phone {
                StatusCode::NO_CONTENT
            } else {
                StatusCode::FORBIDDEN
            }
        );
        assert_eq!(
            net.device(&other_net.id()).is_some(),
            role != peer_network::Role::Phone
        );
        net.shutdown().await;
        other_net.shutdown().await;
    }
}

#[tokio::test]
async fn higher_epoch_records_leader_without_losing_fresh_import_eligibility() {
    let t = setup_with(|data| {
        HostConfig {
            mode: HostMode::Host,
            state: HostState::Fresh,
            ..Default::default()
        }
        .save(data)
        .unwrap();
    });
    // A metadata-only heartbeat must not start components or consume fresh
    // even when starting the scheduler would fail.
    std::fs::create_dir(t._dir.path().join("schedules.db")).unwrap();
    let peer = peer_network::identity::load_or_create_secret_key(&t._dir.path().join("peer.key"))
        .unwrap()
        .public()
        .to_string();
    t.state
        .leader()
        .observe(
            &peer,
            LeaderInfo {
                epoch: 8,
                state: LeaderState::Active,
            },
        )
        .await
        .unwrap();
    let config = HostConfig::load(t._dir.path()).unwrap();
    assert!(t.state.scheduler().is_err());
    assert_eq!(config.state, HostState::Fresh);
    assert_eq!(config.epoch, 8);
    assert_eq!(config.leader.as_deref(), Some(peer.as_str()));
    t.state.stop_components().await;
}

#[tokio::test]
async fn malicious_peer_epoch_is_rejected_and_force_recovers_from_ceiling() {
    let t = setup();
    let peer = peer_network::identity::load_or_create_secret_key(&t._dir.path().join("peer.key"))
        .unwrap()
        .public()
        .to_string();
    for epoch in [
        u64::MAX,
        crate::host::EPOCH_CEILING + 1,
        crate::host::MAX_PEER_EPOCH_ADVANCE + 1,
    ] {
        t.state
            .leader()
            .observe(
                &peer,
                LeaderInfo {
                    epoch,
                    state: LeaderState::Active,
                },
            )
            .await
            .unwrap();
        assert_eq!(t.state.leader().config(), HostConfig::default());
    }
    for stored_epoch in [crate::host::EPOCH_CEILING, u64::try_from(i64::MAX).unwrap()] {
        t.state
            .leader()
            .transition_to(HostConfig {
                epoch: stored_epoch,
                state: HostState::Standby,
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(t.state.leader().activate(false).await.is_err());
        let recovered = t.state.leader().activate(true).await.unwrap();
        assert_eq!(recovered.state, HostState::Active);
        assert_eq!(recovered.epoch, crate::host::EPOCH_CEILING);
        assert_eq!(HostConfig::load(t._dir.path()).unwrap(), recovered);
    }
    t.state.stop_components().await;
}

async fn cache_sender_heartbeat(local: &PeerNetwork, remote: &PeerNetwork, info: LeaderInfo) {
    remote.set_leader_info(Some(info));
    let task = remote.spawn_heartbeat(
        local.id(),
        HeartbeatConfig {
            interval: Duration::from_millis(20),
            initial_backoff: Duration::from_millis(10),
            max_backoff: Duration::from_millis(40),
            connect_timeout: Duration::from_secs(2),
        },
        Arc::new(|_| {}),
    );
    let sender = remote.id();
    tokio::time::timeout(Duration::from_secs(3), async {
        while local.peer_leader(&sender) != Some(info) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("authenticated heartbeat cached");
    task.abort();
}

#[tokio::test]
async fn conflict_notifications_follow_the_authenticated_heartbeat_cache() {
    for (local, remote, epoch, expect) in [
        (
            HostState::Active,
            LeaderState::Active,
            3,
            HostState::Standby,
        ),
        (
            HostState::Active,
            LeaderState::Standby,
            3,
            HostState::Active,
        ),
        (HostState::Active, LeaderState::Fresh, 3, HostState::Active),
        (HostState::Active, LeaderState::Active, 4, HostState::Active),
        (
            HostState::Standby,
            LeaderState::Active,
            3,
            HostState::Standby,
        ),
        (HostState::Fresh, LeaderState::Active, 3, HostState::Fresh),
    ] {
        let t = setup_with(|data| {
            HostConfig {
                mode: if local == HostState::Fresh {
                    HostMode::Host
                } else {
                    HostMode::Standalone
                },
                state: local,
                epoch: 3,
                ..Default::default()
            }
            .save(data)
            .unwrap();
        });
        let other = setup();
        let net = attach_network(&t).await;
        let other_net = attach_network(&other).await;
        let invite = net.create_invite(peer_network::Role::Desktop).await;
        other_net
            .pair(&peer_network::PairTicket::parse(&invite.ticket).unwrap())
            .await
            .unwrap();
        cache_sender_heartbeat(
            &net,
            &other_net,
            LeaderInfo {
                epoch,
                state: remote,
            },
        )
        .await;
        t.state
            .leader()
            .observe_conflict(&peer_network::key_to_string(&other_net.id()))
            .await
            .unwrap();
        assert_eq!(
            t.state.leader().config().state,
            expect,
            "{local:?} {remote:?} {epoch}"
        );
        net.shutdown().await;
        other_net.shutdown().await;
        t.state.stop_components().await;
    }
}

#[tokio::test]
async fn boot_peer_verification_blocks_scheduler_and_automatic_admission() {
    let t = setup();
    let other = setup();
    let net = attach_network(&t).await;
    let other_net = attach_network(&other).await;
    let invite = net.create_invite(peer_network::Role::Desktop).await;
    other_net
        .pair(&peer_network::PairTicket::parse(&invite.ticket).unwrap())
        .await
        .unwrap();
    t.state.leader().set_network(net.clone());
    t.state.start_leader_components().unwrap();
    assert!(t.state.scheduler().is_err());
    assert!(t.state.leader().with_active(|| ()).is_err());
    net.shutdown().await;
    other_net.shutdown().await;
}

#[tokio::test]
async fn ownership_and_device_actions_audit_actor_state_and_epoch_without_tickets() {
    let t = setup();
    let other = setup();
    let net = attach_network(&t).await;
    let other_net = attach_network_as(&other, peer_network::Role::Phone).await;
    assert_eq!(
        phone_request(&t, Method::POST, "/v1/host/standby", None).await,
        StatusCode::OK
    );
    assert_eq!(
        phone_request(
            &t,
            Method::POST,
            "/v1/host/activate",
            Some(json!({"force":true}))
        )
        .await,
        StatusCode::OK
    );
    let invite = call(
        &t,
        Method::POST,
        "/v1/host/invite",
        Some(json!({"role":"phone"})),
    )
    .await
    .json();
    other_net
        .pair(&peer_network::PairTicket::parse(invite["ticket"].as_str().unwrap()).unwrap())
        .await
        .unwrap();
    assert_eq!(
        phone_request(
            &t,
            Method::DELETE,
            &format!("/v1/host/devices/{}", other_net.id()),
            None
        )
        .await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(&t, Method::POST, "/v1/host/update", None).await.status,
        StatusCode::CONFLICT
    );
    let audit = t.state.storage().list_audit(t.state.user_id()).unwrap();
    for (action, actor) in [
        ("host.standby", "phone:phone-key"),
        ("host.activate", "phone:phone-key"),
        ("host.invite", "local"),
        ("host.revoke", "phone:phone-key"),
        ("host.update", "local"),
    ] {
        let row = audit.iter().find(|r| r.action == action).unwrap();
        assert_eq!(row.actor, actor);
        assert!(row.detail["previous_state"].is_string());
        assert!(row.detail["new_epoch"].is_u64());
        assert!(
            !row.detail
                .to_string()
                .contains(invite["ticket"].as_str().unwrap())
        );
        if action == "host.activate" {
            assert_eq!(row.detail["previous_state"], "standby");
            assert_eq!(row.detail["new_state"], "active");
            assert_eq!(row.detail["new_epoch"], 1);
        }
    }
    t.state.stop_components().await;
    net.shutdown().await;
    other_net.shutdown().await;
}

#[tokio::test]
async fn activation_is_idempotent_unless_forced() {
    let t = setup();
    assert_eq!(t.state.leader().activate(false).await.unwrap().epoch, 0);
    assert_eq!(t.state.leader().activate(true).await.unwrap().epoch, 1);
    assert_eq!(t.state.leader().activate(false).await.unwrap().epoch, 1);
    t.state.stop_components().await;
}

struct FailedStartupProbe {
    data: std::path::PathBuf,
    observed: Arc<std::sync::Mutex<Option<HostState>>>,
}
#[async_trait]
impl crate::ChannelControl for FailedStartupProbe {
    fn start(&self) {}
    async fn stop(&self) {
        *self.observed.lock().unwrap() = Some(HostConfig::load(&self.data).unwrap().state);
    }
    fn info(&self) -> crate::dto::ChannelDto {
        panic!("unused")
    }
    fn create_pair_code(&self) -> Result<crate::dto::PairCodeDto, String> {
        panic!("unused")
    }
}

#[tokio::test]
async fn failed_activation_has_durable_standby_before_rollback() {
    let t = setup_with(|data| {
        HostConfig {
            state: HostState::Standby,
            ..Default::default()
        }
        .save(data)
        .unwrap();
    });
    std::fs::create_dir(t._dir.path().join("schedules.db")).unwrap();
    let observed = Arc::new(std::sync::Mutex::new(None));
    t.state.add_channel(Arc::new(FailedStartupProbe {
        data: t._dir.path().into(),
        observed: observed.clone(),
    }));
    assert!(t.state.leader().activate(true).await.is_err());
    assert_eq!(*observed.lock().unwrap(), Some(HostState::Standby));
    assert_eq!(
        HostConfig::load(t._dir.path()).unwrap().state,
        HostState::Standby
    );
    assert_eq!(t.state.leader().config().state, HostState::Standby);
}

struct StatusChannel {
    starts: std::sync::atomic::AtomicUsize,
    status: std::sync::Mutex<Option<String>>,
}
#[async_trait]
impl crate::ChannelControl for StatusChannel {
    fn start(&self) {
        self.starts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        *self.status.lock().unwrap() = Some("running".into());
    }
    async fn stop(&self) {}
    fn info(&self) -> crate::dto::ChannelDto {
        let status = self.status.lock().unwrap().clone();
        crate::dto::ChannelDto {
            id: "telegram".into(),
            configured: true,
            enabled: true,
            running: status.as_deref() == Some("running"),
            paired_users: 0,
            status,
        }
    }
    fn create_pair_code(&self) -> Result<crate::dto::PairCodeDto, String> {
        Err("unused".into())
    }
}

#[tokio::test]
async fn activate_without_force_restarts_only_a_conflicted_channel() {
    let t = setup();
    let conflict = Arc::new(StatusChannel {
        starts: std::sync::atomic::AtomicUsize::new(0),
        status: std::sync::Mutex::new(Some("conflict".into())),
    });
    let idle = Arc::new(ChannelCounter(std::sync::atomic::AtomicBool::new(false)));
    t.state.add_channel(conflict.clone());
    t.state.add_channel(idle.clone());
    let epoch = t.state.leader().config().epoch;
    t.state.leader().activate(false).await.unwrap();
    assert_eq!(t.state.leader().config().epoch, epoch);
    assert_eq!(conflict.starts.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(conflict.status.lock().unwrap().as_deref(), Some("running"));
    assert!(!idle.0.load(std::sync::atomic::Ordering::SeqCst));
    t.state.leader().activate(false).await.unwrap();
    assert_eq!(conflict.starts.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(t.state.leader().config().epoch, epoch);
}

#[tokio::test]
async fn offline_desktop_peer_starts_after_boot_check_deadline() {
    let t = setup();
    let other = setup();
    let net = attach_network(&t).await;
    let other_net = attach_network(&other).await;
    let invite = net.create_invite(peer_network::Role::Desktop).await;
    other_net
        .pair(&peer_network::PairTicket::parse(&invite.ticket).unwrap())
        .await
        .unwrap();
    other_net.shutdown().await;
    t.state.leader().set_network(net.clone());
    t.state
        .leader()
        .set_peer_check_timeout(Duration::from_millis(200));
    let channel = Arc::new(ChannelCounter(std::sync::atomic::AtomicBool::new(false)));
    t.state.add_channel(channel.clone());
    let started = tokio::time::Instant::now();
    // Startup also opens local components; allow for a loaded CI runner while
    // still requiring the offline peer check and startup to finish promptly.
    tokio::time::timeout(Duration::from_secs(15), t.state.leader().start())
        .await
        .expect("deadline must end the peer check")
        .unwrap();
    let elapsed = started.elapsed();
    assert!(elapsed >= Duration::from_millis(150), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(10), "{elapsed:?}");
    assert_eq!(t.state.leader().config().state, HostState::Active);
    assert!(channel.0.load(std::sync::atomic::Ordering::SeqCst));
    net.shutdown().await;
}

#[tokio::test]
async fn health_is_served_while_leadership_waits_for_a_peer() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    let t = setup();
    let other = setup();
    let net = attach_network(&t).await;
    let other_net = attach_network(&other).await;
    let invite = net.create_invite(peer_network::Role::Desktop).await;
    other_net
        .pair(&peer_network::PairTicket::parse(&invite.ticket).unwrap())
        .await
        .unwrap();
    other_net.shutdown().await;
    t.state.leader().set_network(net.clone());
    let channel = Arc::new(ChannelCounter(std::sync::atomic::AtomicBool::new(false)));
    t.state.add_channel(channel.clone());

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop, stopping) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(crate::serve(listener, t.state.clone(), async move {
        let _ = stopping.await;
    }));
    let response = tokio::time::timeout(Duration::from_secs(3), async {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(b"GET /v1/health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        response
    })
    .await
    .expect("HTTP must respond before the peer check deadline");
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(t.state.leader().with_active(|| ()).is_err());
    assert!(!channel.0.load(std::sync::atomic::Ordering::SeqCst));

    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(3), server)
        .await
        .expect("shutdown must cancel the peer check")
        .unwrap()
        .unwrap();
    t.state.leader().detach_network();
    net.shutdown().await;
}

async fn paired_higher_owner(t: &T) -> (PeerNetwork, PeerNetwork) {
    let other = setup();
    let net = attach_network(t).await;
    let other_net = attach_network(&other).await;
    let invite = net.create_invite(peer_network::Role::Desktop).await;
    other_net
        .pair(&peer_network::PairTicket::parse(&invite.ticket).unwrap())
        .await
        .unwrap();
    cache_sender_heartbeat(
        &net,
        &other_net,
        LeaderInfo {
            epoch: 9,
            state: LeaderState::Active,
        },
    )
    .await;
    t.state.leader().set_network(net.clone());
    (net, other_net)
}

#[tokio::test]
async fn failed_demotion_audit_does_not_abort_startup() {
    let t = setup();
    let (net, other_net) = paired_higher_owner(&t).await;
    let db = rusqlite::Connection::open(t._dir.path().join("bot.db")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_host_audit BEFORE INSERT ON audit_log BEGIN SELECT RAISE(ABORT, 'injected audit failure'); END;")
        .unwrap();
    let channel = Arc::new(ChannelCounter(std::sync::atomic::AtomicBool::new(false)));
    t.state.add_channel(channel.clone());
    t.state.leader().start().await.unwrap();
    assert_eq!(t.state.leader().config().state, HostState::Standby);
    assert_eq!(t.state.leader().config().epoch, 9);
    assert_eq!(
        HostConfig::load(t._dir.path()).unwrap().state,
        HostState::Standby
    );
    assert!(
        t.state
            .storage()
            .list_audit(t.state.user_id())
            .unwrap()
            .iter()
            .all(|row| row.action != "host.observe")
    );
    assert!(!channel.0.load(std::sync::atomic::Ordering::SeqCst));
    net.shutdown().await;
    other_net.shutdown().await;
}

#[tokio::test]
async fn failed_demotion_save_does_not_abort_startup() {
    let t = setup();
    let (net, other_net) = paired_higher_owner(&t).await;
    let path = t._dir.path().join("host.toml");
    if path.exists() {
        std::fs::remove_file(&path).unwrap();
    }
    std::fs::create_dir(&path).unwrap();
    let channel = Arc::new(ChannelCounter(std::sync::atomic::AtomicBool::new(false)));
    t.state.add_channel(channel.clone());
    t.state.leader().start().await.unwrap();
    assert!(
        path.is_dir(),
        "a failed save must leave the demotion unpersisted"
    );
    assert_eq!(t.state.leader().config().state, HostState::Standby);
    assert_eq!(t.state.leader().config().epoch, 9);
    assert!(!channel.0.load(std::sync::atomic::Ordering::SeqCst));
    net.shutdown().await;
    other_net.shutdown().await;
}

struct OrderChannel(crate::AppState);
#[async_trait]
impl crate::ChannelControl for OrderChannel {
    async fn stop(&self) {
        self.0.0.order.lock().push("channel");
    }
    fn info(&self) -> crate::dto::ChannelDto {
        crate::dto::ChannelDto {
            id: "telegram".into(),
            configured: true,
            enabled: true,
            running: true,
            paired_users: 0,
            status: None,
        }
    }
    fn create_pair_code(&self) -> Result<crate::dto::PairCodeDto, String> {
        Err("unused".into())
    }
}

#[tokio::test]
async fn shutdown_stops_channels_before_the_scheduler() {
    let t = setup();
    t.state.add_channel(Arc::new(OrderChannel(t.state.clone())));
    t.state.start_scheduler().unwrap();
    t.state.stop_components().await;
    assert_eq!(t.state.shutdown_order(), ["channel", "scheduler"]);
}

#[tokio::test]
async fn startup_reports_starting_with_503_then_degraded_when_the_store_cannot_open() {
    use tokio::net::TcpListener;
    // A directory where the schedule database should be makes opening it fail.
    let t = setup_with(|data| std::fs::create_dir(data.join("schedules.db")).unwrap());
    t.state
        .leader()
        .set_startup(crate::host::StartupPhase::Starting, None);
    let Err(err) = t.state.scheduler() else {
        panic!("scheduler must refuse while starting");
    };
    assert_eq!(err.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(err.code_str(), "starting");
    let health = call(&t, Method::GET, "/v1/health", None).await.json();
    assert_eq!(health["status"], "ok");
    assert_eq!(health["startup"], "starting");
    assert_eq!(health["components_ready"], false);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (stop, stopping) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(crate::serve(listener, t.state.clone(), async move {
        let _ = stopping.await;
    }));
    tokio::time::timeout(Duration::from_secs(5), async {
        while t.state.leader().startup().0 != crate::host::StartupPhase::Degraded {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("a failed store open must surface as degraded");
    let health = call(&t, Method::GET, "/v1/health", None).await.json();
    assert_eq!(health["startup"], "degraded");
    assert_eq!(health["components_ready"], false);
    let reason = health["startup_reason"].as_str().unwrap();
    assert!(!reason.contains("schedules.db") && !reason.contains('/') && !reason.contains('\\'));
    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
}

async fn run_startup_to_degraded(
    t: &T,
    prepare: impl std::future::Future<Output = ()> + Send + 'static,
    timeout: Duration,
) -> serde_json::Value {
    use tokio::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (stop, stopping) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(crate::serve_with_startup_timeout(
        listener,
        t.state.clone(),
        prepare,
        async move {
            let _ = stopping.await;
        },
        timeout,
    ));
    tokio::time::timeout(Duration::from_secs(5), async {
        while t.state.leader().startup().0 != crate::host::StartupPhase::Degraded {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("startup must end degraded, not stay starting");
    let health = call(t, Method::GET, "/v1/health", None).await.json();
    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
    health
}

#[tokio::test]
async fn a_panicking_startup_task_ends_degraded_with_a_fixed_reason() {
    let t = setup();
    let health = run_startup_to_degraded(
        &t,
        async { panic!("boom at /secret/path") },
        Duration::from_secs(60),
    )
    .await;
    assert_eq!(health["startup"], "degraded");
    let reason = health["startup_reason"].as_str().unwrap();
    assert!(reason.contains("unexpectedly") && !reason.contains("secret"));
}

#[tokio::test]
async fn a_hanging_startup_times_out_into_degraded() {
    let t = setup();
    let health =
        run_startup_to_degraded(&t, std::future::pending(), Duration::from_millis(100)).await;
    assert_eq!(health["startup"], "degraded");
    assert!(
        health["startup_reason"]
            .as_str()
            .unwrap()
            .contains("in time")
    );
}

#[test]
fn degraded_reason_follows_the_failure() {
    use crate::host::StartupReason as R;
    assert_eq!(
        R::from_error(&crate::ApiError::unavailable("daemon stopped")),
        R::DaemonStopped
    );
    assert_eq!(
        R::from_error(&crate::ApiError::internal("x")),
        R::StoreUnavailable
    );
    assert_eq!(
        R::from_error(&crate::ApiError::bad_request("x")),
        R::ComponentFailed
    );
}

#[tokio::test]
async fn a_successful_activate_clears_degraded_and_channels_active_is_separate() {
    let t = setup();
    t.state
        .leader()
        .set_degraded(crate::host::StartupReason::StoreUnavailable);
    let health = call(&t, Method::GET, "/v1/health", None).await.json();
    assert_eq!(health["startup"], "degraded");
    assert_eq!(health["channels_active"], false);
    t.state.leader().activate(true).await.unwrap();
    let health = call(&t, Method::GET, "/v1/health", None).await.json();
    assert_eq!(health["startup"], "ready");
    assert!(health.get("startup_reason").is_none());
    assert_eq!(health["components_ready"], true);
    assert_eq!(health["channels_active"], true);
    t.state.stop_components().await;
}

#[tokio::test]
async fn activate_while_verifying_needs_the_epoch_check_and_force() {
    let t = setup();
    let (net, other_net) = paired_higher_owner(&t).await;
    // Still verifying: a known higher active leader refuses a plain activate.
    let err = t.state.leader().activate(false).await.unwrap_err();
    assert_eq!(err.status(), StatusCode::CONFLICT);
    assert!(t.state.leader().with_active(|| ()).is_err());
    // Force takes ownership above every known epoch and ends verification.
    let config = t.state.leader().activate(true).await.unwrap();
    assert_eq!(config.epoch, 10);
    assert!(t.state.leader().with_active(|| ()).is_ok());
    t.state.stop_components().await;
    net.shutdown().await;
    other_net.shutdown().await;
}

/// A paused run a restart interrupted, of a Full-level bot.
async fn restart_interrupted_full_run(t: &T) -> protocol::RunId {
    let r = call(
        t,
        Method::POST,
        "/v1/bots",
        Some(json!({ "profile_toml": format!("{PROFILE}\n[permissions]\npreset = \"full\"\n") })),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.json());
    let bot = r.json()["id"].as_str().unwrap().to_owned();
    let chat = call(t, Method::POST, "/v1/chats", Some(json!({ "bot_id": bot }))).await;
    assert_eq!(chat.status, StatusCode::CREATED);
    let run = protocol::RunId::new();
    t.state
        .storage()
        .insert_run(&storage::Run {
            id: run,
            user_id: t.state.user_id(),
            bot_id: protocol::BotId(bot.parse().unwrap()),
            chat_id: Some(protocol::ChatId(
                chat.json()["id"].as_str().unwrap().parse().unwrap(),
            )),
            parent_run_id: None,
            kind: "turn".into(),
            spec: None,
            state: "paused".into(),
            created_at_ms: storage::now_ms(),
            updated_at_ms: storage::now_ms(),
            error: None,
            computer_target: None,
            computer_node: None,
            computer_node_name: None,
            paused_at_ms: Some(storage::now_ms()),
            pause_reason: Some(task_runtime::RESTART_PAUSE_REASON.into()),
        })
        .unwrap();
    run
}

fn auto_resume_attempts(t: &T, run: protocol::RunId) -> usize {
    t.state
        .storage()
        .list_audit_for_run(t.state.user_id(), run)
        .unwrap()
        .iter()
        .filter(|a| a.action == "run.auto_resumed")
        .count()
}

#[tokio::test]
async fn a_standby_node_does_not_resume_interrupted_runs_until_it_becomes_the_leader() {
    let t = setup_with(|data| {
        HostConfig {
            state: HostState::Standby,
            ..HostConfig::default()
        }
        .save(data)
        .unwrap();
    });
    let run = restart_interrupted_full_run(&t).await;
    t.state.start_leader_components().unwrap();
    for _ in 0..20 {
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        auto_resume_attempts(&t, run),
        0,
        "a standby node drives nothing"
    );
    assert_eq!(
        t.state.storage().get_run(run).unwrap().unwrap().state,
        "paused"
    );
    // Taking over as the leader starts the components, and with them the resume.
    t.state.leader().activate(false).await.unwrap();
    let mut attempts = 0;
    for _ in 0..200 {
        attempts = auto_resume_attempts(&t, run);
        if attempts > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(attempts, 1, "the leader continues the interrupted run once");
    t.state.stop_components().await;
}

#[tokio::test]
async fn update_requests_are_audited_with_their_tag_and_outcome() {
    let t = setup();
    for (body, tag) in [
        (json!({"tag":"v0.1.0-alpha.9"}), json!("v0.1.0-alpha.9")),
        // Text that is not a release tag never reaches the audit trail.
        (json!({"tag":"../../etc/passwd"}), json!(null)),
    ] {
        let response = call(&t, Method::POST, "/v1/host/update", Some(body)).await;
        assert_eq!(response.status, StatusCode::CONFLICT);
        let audit = t.state.storage().list_audit(t.state.user_id()).unwrap();
        let last = audit
            .iter()
            .rfind(|row| row.action == "host.update")
            .unwrap();
        assert_eq!(last.detail["detail"]["phase"], "rejected");
        assert_eq!(last.detail["detail"]["outcome"], "unavailable");
        assert_eq!(last.detail["detail"]["tag"], tag);
    }
    let phases: Vec<_> = t
        .state
        .storage()
        .list_audit(t.state.user_id())
        .unwrap()
        .iter()
        .filter(|row| row.action == "host.update")
        .map(|row| row.detail["detail"]["phase"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(phases, ["requested", "rejected", "requested", "rejected"]);
}

#[tokio::test]
async fn update_request_body_is_bounded() {
    let t = setup();
    let big = json!({"tag": format!("v{}", "1".repeat(crate::host_admin::MAX_ADMIN_BODY))});
    let response = call(&t, Method::POST, "/v1/host/update", Some(big)).await;
    assert_eq!(response.status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn update_requests_have_their_own_small_budget() {
    let t = setup();
    let leader = t.state.leader();
    for _ in 0..3 {
        leader.admit_update().unwrap();
    }
    let error = leader.admit_update().unwrap_err();
    assert_eq!(error.status(), StatusCode::TOO_MANY_REQUESTS);
    // Other administration calls and invites keep their own budgets.
    leader.admit_admin_call().unwrap();
}

#[test]
fn a_running_status_does_not_outlive_the_updater() {
    use crate::host::{HostUpdateState, read_update_status};
    use std::time::{Duration, SystemTime};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("status.json");
    let now = SystemTime::now();
    let written = |state: &str| {
        std::fs::write(
            &path,
            format!(r#"{{"state":"{state}","tag":"v0.1.0-alpha.9"}}"#),
        )
        .unwrap();
    };
    // Nothing recorded yet, or a file that is not a status: idle.
    assert!(matches!(
        read_update_status(&path, now).state,
        HostUpdateState::Idle
    ));
    std::fs::write(&path, "{").unwrap();
    assert!(matches!(
        read_update_status(&path, now).state,
        HostUpdateState::Idle
    ));
    // A recent run is shown as running; one that outlived the unit's time limit has failed.
    written("running");
    assert!(matches!(
        read_update_status(&path, now).state,
        HostUpdateState::Running
    ));
    let later = now + Duration::from_secs(46 * 60);
    let stale = read_update_status(&path, later);
    assert!(matches!(stale.state, HostUpdateState::Failed));
    assert_eq!(stale.tag.as_deref(), Some("v0.1.0-alpha.9"));
    // Other states are never rewritten, however old.
    written("updated");
    assert!(matches!(
        read_update_status(&path, later).state,
        HostUpdateState::Updated
    ));
    // A status file larger than the bound is ignored, not read whole.
    std::fs::write(&path, vec![b' '; 1 << 20]).unwrap();
    assert!(matches!(
        read_update_status(&path, now).state,
        HostUpdateState::Idle
    ));
}
