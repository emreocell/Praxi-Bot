//! `secret_request`: the loop parks the run on the owner's form and the model
//! learns only `stored` or `cancelled`. The value itself is written by the
//! daemon's submit route (tested in daemon-api); here the test plays that route.

use protocol::RunId;
use secret_store::{FillSecrets, MemoryStore};
use std::fmt::Write as _;

use super::*;

const CANARY: &str = "canary-Correct-Horse-Battery-4417!";
const USER_CANARY: &str = "canary-user-name-7731";

fn loop_and_vault(
    env: &Env,
    fake: &Arc<Fake>,
    config: LoopConfig,
) -> (AgentLoop, Arc<FillSecrets>) {
    let lp = make_loop(&env.storage, env.bot, fake, &[], profile(""), config);
    let secrets = Arc::new(FillSecrets::in_memory(Arc::new(MemoryStore::new())));
    lp.sh
        .tools
        .register(builtin_tools::secret_request::secret_request_tool(
            Arc::clone(&secrets),
        ));
    (lp, secrets)
}

fn loop_with_request_tool(env: &Env, fake: &Arc<Fake>, config: LoopConfig) -> AgentLoop {
    loop_and_vault(env, fake, config).0
}

/// The tool is off by default; these tests turn it on explicitly.
fn on(mut c: LoopConfig) -> LoopConfig {
    c.secret_requests = true;
    c
}

fn ask() -> Value {
    json!({"name":"login/example","origin":"https://example.com","purpose":"Sign in to download the invoice","fields":"login"})
}

fn owner_turn(lp: &AgentLoop, chat: ChatId, text: &str) -> RunId {
    let crate::Delivery::NewTurn(run) = lp
        .send_user_message_from(chat, text, crate::MessageMode::Queue, "owner")
        .unwrap()
    else {
        panic!("expected a new owner turn");
    };
    run
}

fn requested(e: &Event) -> bool {
    matches!(e, Event::SecretRequested(_))
}

#[tokio::test]
async fn the_owner_answers_and_the_model_sees_only_stored() {
    let env = setup();
    let fake = Fake::new(vec![
        calls(&[("c1", "secret_request", ask())]),
        text("thanks"),
    ]);
    let (lp, secrets) = loop_and_vault(&env, &fake, on(cfg(Policy::full())));
    let mut rx = lp.subscribe();
    let run = owner_turn(&lp, env.chat, "log in for me");
    let Event::SecretRequested(card) = until(&mut rx, requested).await else {
        unreachable!()
    };
    assert_eq!(card.name, "login/example");
    assert_eq!(card.origin.as_deref(), Some("https://example.com"));
    assert_eq!(card.expires_in_secs, 300);
    assert!(!card.untrusted_content);
    assert_eq!(lp.runtime().state(run).unwrap(), RunState::WaitingApproval);
    assert!(lp.secret_request(card.call_id).is_some());

    // The submit route's part: claim once, store, wake the run.
    assert!(lp.claim_secret_request(card.call_id).is_some());
    secrets
        .store_request(
            &[
                (
                    "login/example/username".into(),
                    secret_store::SecretValue::new(USER_CANARY),
                ),
                (
                    "login/example/password".into(),
                    secret_store::SecretValue::new(CANARY),
                ),
            ],
            "https://example.com",
            &env.bot.to_string(),
            "Sign in to download the invoice",
            false,
        )
        .unwrap();
    assert!(
        lp.claim_secret_request(card.call_id).is_none(),
        "single use"
    );
    lp.finish_secret_request(card.call_id, true, env.user)
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);

    let request = fake.request(1);
    let msgs = tool_messages(&request);
    assert_eq!(msgs.len(), 1);
    assert!(msgs[0].starts_with("stored:"), "{}", msgs[0]);
    // Nothing the run wrote anywhere contains the value.
    let mut all = serde_json::to_string(&request).unwrap();
    while let Ok(i) = rx.try_recv() {
        all.push_str(&serde_json::to_string(&i.envelope.event).unwrap());
    }
    let _ = write!(all, "{:?}", env.storage.list_audit(env.user).unwrap());
    assert!(!all.contains(CANARY) && !all.contains(USER_CANARY));
    let audit = format!("{:?}", env.storage.list_audit(env.user).unwrap());
    assert!(audit.contains("secret_request.requested"));
}

#[tokio::test]
async fn a_pending_request_parks_only_its_run_and_can_resume() {
    let env = setup();
    let other_chat = ChatId(uuid::Uuid::now_v7());
    env.storage
        .insert_chat(&Chat {
            id: other_chat,
            user_id: env.user,
            bot_id: env.bot,
            title: "Other task".into(),
            created_at_ms: now_ms(),
        })
        .unwrap();
    let fake = Fake::new(vec![
        calls(&[("c1", "secret_request", ask())]),
        text("other task completed"),
        text("first task resumed"),
    ]);
    let lp = loop_with_request_tool(&env, &fake, on(cfg(Policy::full())));
    let mut events = lp.subscribe();
    let first = owner_turn(&lp, env.chat, "sign in for me");
    let Event::SecretRequested(card) = until(&mut events, requested).await else {
        unreachable!()
    };

    let other = owner_turn(&lp, other_chat, "do the other task");
    assert_eq!(lp.wait(other).await.unwrap(), RunState::Succeeded);
    assert_eq!(
        lp.runtime().state(first).unwrap(),
        RunState::WaitingApproval
    );

    assert!(lp.claim_secret_request(card.call_id).is_some());
    lp.finish_secret_request(card.call_id, true, env.user)
        .unwrap();
    assert_eq!(lp.wait(first).await.unwrap(), RunState::Succeeded);
    assert!(tool_messages(&fake.request(2))[0].starts_with("stored:"));
}

#[tokio::test]
async fn only_the_secret_cancel_path_can_answer_a_secret_request() {
    for forge in [false, true] {
        let env = setup();
        let fake = Fake::new(vec![calls(&[("c1", "secret_request", ask())]), text("ok")]);
        let lp = loop_with_request_tool(&env, &fake, on(cfg(Policy::full())));
        let mut rx = lp.subscribe();
        let run = owner_turn(&lp, env.chat, "go");
        let Event::SecretRequested(card) = until(&mut rx, requested).await else {
            unreachable!()
        };
        if forge {
            // `POST /v1/questions/{id}` must not be able to claim "stored".
            assert!(lp.answer_question(card.call_id, "stored").is_err());
            assert!(lp.deny(card.call_id, env.user, None).is_err());
            assert!(lp.secret_request(card.call_id).is_some());
        }
        lp.finish_secret_request(card.call_id, false, env.user)
            .unwrap();
        assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
        let msgs = tool_messages(&fake.request(1));
        assert!(msgs[0].starts_with("cancelled:"), "{forge}: {}", msgs[0]);
        assert!(lp.secret_request(card.call_id).is_none());
    }
}

#[tokio::test]
async fn a_request_nobody_answers_expires_and_leaves_nothing() {
    let env = setup();
    let fake = Fake::new(vec![calls(&[("c1", "secret_request", ask())]), text("ok")]);
    let mut config = on(cfg(Policy::full()));
    config.secret_request_timeout = Duration::from_millis(50);
    let lp = loop_with_request_tool(&env, &fake, config);
    let mut rx = lp.subscribe();
    let run = owner_turn(&lp, env.chat, "go");
    let Event::SecretRequested(card) = until(&mut rx, requested).await else {
        unreachable!()
    };
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    let msgs = tool_messages(&fake.request(1));
    assert!(msgs[0].contains("did not answer in time"), "{}", msgs[0]);
    assert!(lp.secret_request(card.call_id).is_none());
    assert!(
        env.storage
            .list_audit(env.user)
            .unwrap()
            .iter()
            .any(|entry| entry.action == "secret_request.expired")
    );
    assert!(
        lp.finish_secret_request(card.call_id, true, env.user)
            .is_err()
    );
}

#[tokio::test]
async fn a_channel_run_is_refused_and_no_form_is_shown() {
    let env = setup();
    let fake = Fake::new(vec![calls(&[("c1", "secret_request", ask())]), text("ok")]);
    let lp = loop_with_request_tool(&env, &fake, on(cfg(Policy::full())));
    let mut rx = lp.subscribe();
    let run = lp
        .run_turn_as(env.chat, "log in", protocol::InitiatorKind::Channel)
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    let msgs = tool_messages(&fake.request(1));
    assert!(msgs[0].starts_with("cancelled:"), "{}", msgs[0]);
    assert!(msgs[0].contains("started at the desktop"), "{}", msgs[0]);
    while let Ok(i) = rx.try_recv() {
        assert!(!requested(&i.envelope.event), "a form was shown");
    }
}

#[tokio::test]
async fn paired_and_bearer_only_messages_cannot_open_a_form() {
    for source in ["peer_admin", "peer", "local_api"] {
        let env = setup();
        let fake = Fake::new(vec![calls(&[("c1", "secret_request", ask())]), text("ok")]);
        let lp = loop_with_request_tool(&env, &fake, on(cfg(Policy::full())));
        let mut rx = lp.subscribe();
        let crate::Delivery::NewTurn(run) = lp
            .send_user_message_from(env.chat, "sign in", crate::MessageMode::Queue, source)
            .unwrap()
        else {
            panic!("expected a new turn");
        };
        assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
        assert!(tool_messages(&fake.request(1))[0].contains("started at the desktop"));
        while let Ok(item) = rx.try_recv() {
            assert!(!requested(&item.envelope.event));
        }
    }
}

#[tokio::test]
async fn the_global_switch_and_the_per_run_limit_refuse() {
    let env = setup();
    let fake = Fake::new(vec![calls(&[("c1", "secret_request", ask())]), text("ok")]);
    let mut config = on(cfg(Policy::full()));
    config.secret_requests = false;
    let lp = loop_with_request_tool(&env, &fake, config);
    let run = owner_turn(&lp, env.chat, "go");
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert!(tool_messages(&fake.request(1))[0].contains("switched off"));

    let env = setup();
    let fake = Fake::new(vec![calls(&[("c1", "secret_request", ask())]), text("ok")]);
    let mut config = on(cfg(Policy::full()));
    config.max_secret_requests_per_run = 0;
    let lp = loop_with_request_tool(&env, &fake, config);
    let run = owner_turn(&lp, env.chat, "go");
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert!(tool_messages(&fake.request(1))[0].contains("too often"));
}

#[test]
fn the_switch_is_off_by_default_in_the_loop() {
    assert!(!LoopConfig::default().secret_requests);
}

/// A submit that claimed the form before the timer fired must still be reported
/// as `stored`, however long the vault write takes.
#[tokio::test]
async fn a_submit_that_overlaps_the_timeout_is_reported_as_stored() {
    let env = setup();
    let fake = Fake::new(vec![calls(&[("c1", "secret_request", ask())]), text("ok")]);
    let mut config = on(cfg(Policy::full()));
    config.secret_request_timeout = Duration::from_millis(300);
    let (lp, secrets) = loop_and_vault(&env, &fake, config);
    let mut rx = lp.subscribe();
    let run = owner_turn(&lp, env.chat, "go");
    let Event::SecretRequested(card) = until(&mut rx, requested).await else {
        unreachable!()
    };
    assert!(lp.claim_secret_request(card.call_id).is_some());
    // The timer fires while the submit is still writing.
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert_eq!(lp.runtime().state(run).unwrap(), RunState::WaitingApproval);
    secrets
        .upsert("login/example", Some(CANARY), None, None)
        .unwrap();
    lp.finish_secret_request(card.call_id, true, env.user)
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    let msgs = tool_messages(&fake.request(1));
    assert!(msgs[0].starts_with("stored:"), "{}", msgs[0]);
}

/// If the claimed submit fails and the form is put back after the timer fired, the
/// form expires (the run is not parked forever) and the model hears `cancelled`.
#[tokio::test]
async fn a_failed_late_submit_still_expires_the_form() {
    let env = setup();
    let fake = Fake::new(vec![calls(&[("c1", "secret_request", ask())]), text("ok")]);
    let mut config = on(cfg(Policy::full()));
    // Long enough that the claim below always happens before the timer, even on a slow runner.
    config.secret_request_timeout = Duration::from_millis(400);
    let lp = loop_with_request_tool(&env, &fake, config);
    let mut rx = lp.subscribe();
    let run = owner_turn(&lp, env.chat, "go");
    let Event::SecretRequested(card) = until(&mut rx, requested).await else {
        unreachable!()
    };
    let info = lp.claim_secret_request(card.call_id).unwrap();
    tokio::time::sleep(Duration::from_millis(1000)).await;
    lp.restore_secret_request(card.call_id, info);
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert!(tool_messages(&fake.request(1))[0].contains("did not answer in time"));
}
