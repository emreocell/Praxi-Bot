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
async fn phone_cannot_mint_privileged_invites() {
    let t = setup();
    let net = attach_network(&t).await;
    for role in ["desktop", "host", "node"] {
        assert_eq!(
            phone_request(
                &t,
                Method::POST,
                "/v1/host/invite",
                Some(json!({"role":role}))
            )
            .await,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        phone_request(
            &t,
            Method::POST,
            "/v1/host/invite",
            Some(json!({"role":"phone"}))
        )
        .await,
        StatusCode::OK
    );
    net.shutdown().await;
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

#[tokio::test]
async fn conflict_notifications_require_active_sender_and_active_local_owner() {
    let peer_dir = tempfile::tempdir().unwrap();
    let peer = peer_network::identity::load_or_create_secret_key(&peer_dir.path().join("peer.key"))
        .unwrap()
        .public()
        .to_string();
    for local in [HostState::Fresh, HostState::Standby, HostState::Active] {
        for remote in [
            LeaderState::Fresh,
            LeaderState::Standby,
            LeaderState::Active,
        ] {
            let t = setup_with(|data| {
                HostConfig {
                    mode: HostMode::Host,
                    state: local,
                    epoch: 3,
                    ..Default::default()
                }
                .save(data)
                .unwrap();
            });
            t.state
                .leader()
                .observe_conflict(
                    &peer,
                    LeaderInfo {
                        epoch: 3,
                        state: remote,
                    },
                )
                .await
                .unwrap();
            assert_eq!(
                t.state.leader().config().state,
                if local == HostState::Active && remote == LeaderState::Active {
                    HostState::Standby
                } else {
                    local
                }
            );
        }
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
