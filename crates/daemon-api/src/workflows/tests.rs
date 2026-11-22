//! HTTP recording, user-only persistence and permission-gated replay.
#![allow(clippy::too_many_lines)]
use super::*;
use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use futures::{StreamExt, stream::BoxStream};
use http_body_util::BodyExt;
use protocol::Event;
use provider_catalog::{ChatChunk, ChatProvider, ChatRequest, FinishReason, ProviderError};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tool_registry::{
    Effect, PermissionCategory, Platform, Tool, ToolContext, ToolError, ToolOutput, ToolRegistry,
    ToolSpec,
};
use tower::ServiceExt;

struct Provider {
    saved: AtomicBool,
    executor_calls: AtomicUsize,
    requests: parking_lot::Mutex<Vec<String>>,
}
#[async_trait]
impl ChatProvider for Provider {
    fn id(&self) -> &'static str {
        "fake"
    }
    async fn stream(
        &self,
        req: ChatRequest,
    ) -> Result<BoxStream<'static, Result<ChatChunk, ProviderError>>, ProviderError> {
        self.requests.lock().push(format!("{req:?}"));
        if req.model.0 == "fake/classifier" {
            let text = if self.saved.load(Ordering::SeqCst) {
                r#"{"route":"workflow","name":"recorded","confidence":1.0}"#
            } else {
                r#"{"route":"tool","name":"mutate","confidence":1.0}"#
            };
            return Ok(futures::stream::iter(vec![
                Ok(ChatChunk::TextDelta(text.into())),
                Ok(ChatChunk::Done {
                    finish_reason: FinishReason::Stop,
                }),
            ])
            .boxed());
        }
        self.executor_calls.fetch_add(1, Ordering::SeqCst);
        crate::fake::FakeProvider.stream(req).await
    }
}

struct TestTool {
    spec: ToolSpec,
    calls: Arc<AtomicUsize>,
    inputs: Arc<parking_lot::Mutex<Vec<Value>>>,
}
#[async_trait]
impl Tool for TestTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }
    async fn call(&self, _: &ToolContext, input: Value) -> Result<ToolOutput, ToolError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inputs.lock().push(input);
        Ok(ToolOutput::text(if self.spec.name == "browser_snapshot" {
            r#"# Shop (https://example.test/cart)
  [e1] textbox "Search"
  [e2] button "Go"
"#
        } else if self.spec.name == "local_write" {
            // Empty output keeps a recording of this tool clean: any non-empty
            // tool text taints a recording, and a tainted replay cannot carry
            // a runtime secret (approval cards must not show registered secrets).
            ""
        } else {
            "done"
        }))
    }
}

struct Env {
    state: AppState,
    chat: ChatId,
    dir: tempfile::TempDir,
    provider: Arc<Provider>,
    calls: Arc<AtomicUsize>,
    inputs: Arc<parking_lot::Mutex<Vec<Value>>>,
}
fn setup() -> Env {
    setup_with(tempfile::tempdir().unwrap(), false, None)
}

/// `existing`: reopen this directory (bot and chat already stored), e.g. to
/// start the same data in safe mode.
fn setup_with(dir: tempfile::TempDir, safe_mode: bool, existing: Option<ChatId>) -> Env {
    let storage = Arc::new(storage::Storage::open(dir.path()).unwrap());
    let registry = ToolRegistry::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let inputs = Arc::new(parking_lot::Mutex::new(Vec::new()));
    for (name, effect) in [
        ("mutate", Effect::Write),
        ("local_write", Effect::Write),
        ("browser_fill", Effect::Write),
        ("browser_click", Effect::Write),
        ("desktop_drag", Effect::Write),
        ("fs_read", Effect::Read),
        ("browser_snapshot", Effect::Read),
    ] {
        registry.register(Arc::new(TestTool {
            spec: ToolSpec {
                name: name.into(),
                description: name.into(),
                input_schema: json!({"type":"object"}),
                effect,
                category: PermissionCategory::Fs,
                platforms: vec![Platform::All],
                // Same classes as the built-ins these doubles stand for.
                content: match name {
                    "fs_read" => tool_registry::ContentClass::private_read(),
                    "browser_snapshot" => tool_registry::ContentClass::untrusted_private(),
                    // No egress: the secret-replay test must not need a card that shows a secret.
                    "local_write" => tool_registry::ContentClass::declared(false, false, false),
                    _ => tool_registry::ContentClass::all(),
                },
            },
            calls: calls.clone(),
            inputs: inputs.clone(),
        }));
    }
    let provider = Arc::new(Provider {
        saved: AtomicBool::new(false),
        executor_calls: AtomicUsize::new(0),
        requests: parking_lot::Mutex::default(),
    });
    let agent = agent_core::AgentLoop::new(
        storage.clone(),
        registry,
        agent_core::Providers::single(provider.clone()),
        agent_core::LoopConfig {
            global_policy: permission_engine::Policy::full(),
            legacy_tool_surface: true,
            redactor: Some(agent_core::SharedRedactor::default()),
            safe_mode,
            ..Default::default()
        },
    )
    .with_workflows(
        WorkflowStore::new(dir.path()),
        workflow_replay::Handles {
            desktop: None,
            browser: None,
            tools: ToolRegistry::new(),
        },
    );
    let state = AppState::new(
        agent,
        storage.clone(),
        crate::DaemonConfig::new("test-token"),
    )
    .unwrap();
    state.set_owner_key(crate::tests::OWNER_KEY.into());
    let profile = agent_core::BotProfile::from_toml_str("[bot]\nid='test'\nname='test'\n[models]\nexecutor='fake/echo'\nclassifier='fake/classifier'\n").unwrap();
    if let Some(chat) = existing {
        let bot_id = storage.get_chat(chat).unwrap().unwrap().bot_id;
        state.agent().add_profile(bot_id, profile);
        return Env {
            state,
            chat,
            dir,
            provider,
            calls,
            inputs,
        };
    }
    let bot = storage::Bot {
        id: protocol::BotId::new(),
        user_id: state.user_id(),
        name: "test".into(),
        config: json!({"bot":{"id":"test","name":"test"},"models":{"executor":"fake/echo","classifier":"fake/classifier"}}),
        created_at_ms: 0,
    };
    storage.insert_bot(&bot).unwrap();
    state.agent().add_profile(bot.id, profile);
    let chat = storage::Chat {
        id: ChatId::new(),
        user_id: state.user_id(),
        bot_id: bot.id,
        title: "test".into(),
        created_at_ms: 0,
    };
    storage.insert_chat(&chat).unwrap();
    Env {
        state,
        chat: chat.id,
        dir,
        provider,
        calls,
        inputs,
    }
}

async fn request(
    env: &Env,
    method: Method,
    path: &str,
    body: Option<Value>,
    authenticated: bool,
) -> (StatusCode, Value) {
    let bot = env
        .state
        .storage()
        .get_chat(env.chat)
        .unwrap()
        .unwrap()
        .bot_id;
    let scoped = if matches!(method, Method::GET | Method::DELETE)
        && path.starts_with("/v1/workflows")
        && !path.contains('?')
    {
        format!("{path}?bot_id={bot}")
    } else {
        path.to_owned()
    };
    let mut req = Request::builder()
        .method(method)
        .uri(scoped)
        .header("host", "127.0.0.1");
    if authenticated {
        req = req
            .header("authorization", "Bearer test-token")
            .header("x-praxi-owner-key", crate::tests::OWNER_KEY);
    }
    let req = req
        .header("content-type", "application/json")
        .body(Body::from(body.map_or(String::new(), |v| v.to_string())))
        .unwrap();
    let response = crate::router(env.state.clone()).oneshot(req).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn approve_replays(env: &Env) -> tokio::task::JoinHandle<()> {
    let mut events = env.state.agent().subscribe();
    let state = env.state.clone();
    tokio::spawn(async move {
        while let Ok(item) = events.recv().await {
            if let Event::ApprovalNeeded(approval) = item.envelope.event {
                state
                    .agent()
                    .approve(
                        approval.call_id,
                        agent_core::ApprovalScope::Once,
                        state.user_id(),
                    )
                    .unwrap();
            }
        }
    })
}

async fn source(env: &Env) -> RunId {
    // Writes after a read ask since main's tainted-egress rule: the source run is
    // approved here and the approver stops before any replay under test.
    let approver = approve_replays(env);
    let run = env.state.agent().run_turn(env.chat, "call:fs_read {}\ncall:mutate {}\ncall:browser_snapshot {}\ncall:mutate {}\ncall:mutate {}").unwrap();
    assert_eq!(
        env.state.agent().wait(run).await.unwrap(),
        task_runtime::RunState::Succeeded
    );
    approver.abort();
    // `run_turn` records no origin; the desktop app's send route marks the
    // message `owner`, which is what makes a run recordable.
    mark_owner(env, run);
    run
}

fn mark_owner(env: &Env, run: RunId) {
    for m in env.state.storage().list_messages(env.chat).unwrap() {
        if m.run_id == Some(run) && m.role == "user" {
            env.state
                .storage()
                .set_message_source(m.id, "owner")
                .unwrap();
        }
    }
}

#[tokio::test]
async fn propose_review_save_list_replay_and_delete() {
    let env = setup();
    let id = source(&env).await;
    let path = format!("/v1/runs/{id}/workflow-proposal");
    assert_eq!(
        request(&env, Method::POST, &path, None, false).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (status, proposal) = request(&env, Method::POST, &path, None, true).await;
    assert_eq!(status, StatusCode::OK, "{proposal}");
    assert_eq!(proposal["workflow"]["steps"].as_array().unwrap().len(), 3);
    assert_eq!(proposal["skipped"].as_array().unwrap().len(), 2);
    assert!(
        WorkflowStore::new(env.dir.path())
            .list()
            .unwrap()
            .is_empty(),
        "proposals never save"
    );
    let save = json!({"run_id":id.0, "name":"recorded", "description":"call:fs_read {} call:mutate {} call:browser_snapshot {} call:mutate {} call:mutate {}", "params":proposal["workflow"]["params"], "steps":proposal["workflow"]["steps"],"confirmed_steps":[0,1,2]});
    assert_eq!(
        request(
            &env,
            Method::POST,
            "/v1/workflows",
            Some(save.clone()),
            true
        )
        .await
        .0,
        StatusCode::CREATED
    );
    assert_eq!(
        request(&env, Method::POST, "/v1/workflows", Some(save), true)
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        request(&env, Method::GET, "/v1/workflows", None, true)
            .await
            .1[0]["name"],
        "recorded"
    );
    assert!(
        !env.state
            .agent()
            .tool_names()
            .iter()
            .any(|n| n == "workflow_save")
    );
    let listed = env
        .state
        .agent()
        .tool_registry()
        .get("workflow_list")
        .unwrap()
        .call(
            &ToolContext {
                bot_id: Some(
                    env.state
                        .storage()
                        .get_chat(env.chat)
                        .unwrap()
                        .unwrap()
                        .bot_id
                        .to_string(),
                ),
                ..Default::default()
            },
            json!({}),
        )
        .await
        .unwrap();
    assert!(serde_json::to_string(&listed).unwrap().contains("recorded"));
    let approvals = approve_replays(&env);
    env.provider.saved.store(true, Ordering::SeqCst);
    let before = env.provider.executor_calls.load(Ordering::SeqCst);
    let run = env.state.agent().run_turn(env.chat, "call:fs_read {}\ncall:mutate {}\ncall:browser_snapshot {}\ncall:mutate {}\ncall:mutate {}").unwrap();
    assert_eq!(
        env.state.agent().wait(run).await.unwrap(),
        task_runtime::RunState::Succeeded
    );
    assert_eq!(
        env.provider.executor_calls.load(Ordering::SeqCst),
        before,
        "replay never calls executor model"
    );
    env.state.agent().flush_journal();
    let log = storage::read_event_log(env.dir.path(), run).unwrap();
    assert!(log.iter().any(|e| matches!(&e.event, protocol::Event::RouteDecided(r) if (r.level == "K2" || r.level == "K3") && r.route["kind"] == "workflow")));
    assert_eq!(
        log.iter()
            .filter(|e| matches!(&e.event, protocol::Event::WorkflowStepCompleted(_)))
            .count(),
        3
    );
    approvals.abort();
    env.state.agent().remember_route(run).unwrap();
    assert_eq!(
        request(&env, Method::DELETE, "/v1/workflows/recorded", None, true)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        request(&env, Method::GET, "/v1/workflows/recorded", None, true)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let cache = rusqlite::Connection::open(env.dir.path().join("router").join(
        format!("{}.sqlite", env.state.storage().get_chat(env.chat).unwrap().unwrap().bot_id),
    ))
    .unwrap();
    let cached: i64 = cache
        .query_row(
            "SELECT COUNT(*) FROM router_intent_cache WHERE workflow = 'recorded'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        cached, 0,
        "workflow_changed must remove K1 entries on deletion"
    );
}

#[tokio::test]
async fn runtime_secrets_never_reach_history_steps_journal_or_provider() {
    let env = setup();
    // A step that needs approval cannot show a runtime secret on its card (the
    // approval layer refuses it), so this bot has the egress waiver and the
    // replayed tool runs without asking; the test is about where the value goes.
    let bot_id = env
        .state
        .storage()
        .get_chat(env.chat)
        .unwrap()
        .unwrap()
        .bot_id;
    env.state.agent().add_profile(
        bot_id,
        agent_core::BotProfile::from_toml_str(
            "[bot]
id='test'
name='test'
[models]
executor='fake/echo'
classifier='fake/classifier'
[permissions]
allow_egress_after_reads=true
",
        )
        .unwrap(),
    );
    let id = env
        .state
        .agent()
        .run_turn(env.chat, "call:local_write {\"password\":\"p4ssword-218\"}")
        .unwrap();
    assert_eq!(
        env.state.agent().wait(id).await.unwrap(),
        task_runtime::RunState::Succeeded
    );
    mark_owner(&env, id);
    let (status, proposal) = request(
        &env,
        Method::POST,
        &format!("/v1/runs/{id}/workflow-proposal"),
        None,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{proposal}");
    assert!(!proposal.to_string().contains("p4ssword-218"));
    let mut params = proposal["workflow"]["params"].clone();
    params[0]["default"] = json!("do-not-persist");
    let mut input = json!({"run_id":id.0,"name":"password","description":"safe","params":params,"steps":proposal["workflow"]["steps"],"confirmed_steps":[0,1,2]});
    assert_eq!(
        request(
            &env,
            Method::POST,
            "/v1/workflows",
            Some(input.clone()),
            true
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    input["params"] = proposal["workflow"]["params"].clone();
    input["steps"] = json!([{"kind":"shell","command":"injected"}]);
    assert_eq!(
        request(
            &env,
            Method::POST,
            "/v1/workflows",
            Some(input.clone()),
            true
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    input["steps"] = proposal["workflow"]["steps"].clone();
    assert_eq!(
        request(&env, Method::POST, "/v1/workflows", Some(input), true)
            .await
            .0,
        StatusCode::CREATED
    );
    let bot = env
        .state
        .storage()
        .get_chat(env.chat)
        .unwrap()
        .unwrap()
        .bot_id;
    let toml = std::fs::read_to_string(
        WorkflowStore::new(env.dir.path())
            .for_bot(&bot.to_string())
            .unwrap()
            .dir()
            .join("password.toml"),
    )
    .unwrap();
    assert!(!toml.contains("p4ssword-218"));
    assert!(!toml.contains("default ="));
    let (status, _) = request(
        &env,
        Method::POST,
        "/v1/workflows/password/run",
        Some(json!({"chat_id":env.chat.0,"params":{"recorded_secret_1":"abc"}})),
        true,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "short values cannot be safely registered by the shared redactor"
    );
    let approvals = approve_replays(&env);
    env.inputs.lock().clear();
    let (status, body) = request(&env, Method::POST, "/v1/workflows/password/run",
        Some(json!({"chat_id":env.chat.0,"params":{"recorded_secret_1":"reviewed value=\"with spaces\"\\and\nnewlines"}})), true).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let replay = RunId(body["run_id"].as_str().unwrap().parse().unwrap());
    assert_eq!(
        env.state.agent().wait(replay).await.unwrap(),
        task_runtime::RunState::Succeeded
    );
    let row = env.state.storage().get_run(replay).unwrap().unwrap();
    assert_eq!(
        row.kind, "turn",
        "human permissions must apply to the user action"
    );
    approvals.abort();
    env.state.agent().flush_journal();
    let secret = "reviewed value=\"with spaces\"\\and\nnewlines";
    // The value travels out of band but must still reach the replayed tool.
    assert_eq!(
        env.inputs.lock().as_slice(),
        [json!({"password": secret})],
        "human replay must hand the runtime value to the tool"
    );
    let steps = env.state.storage().list_steps(replay).unwrap();
    assert!(!format!("{steps:?}").contains("reviewed value="));
    let messages = env.state.storage().list_messages(env.chat).unwrap();
    assert!(!format!("{messages:?}").contains("reviewed value="));
    let log = storage::read_event_log(env.dir.path(), replay).unwrap();
    assert!(
        !serde_json::to_string(&log)
            .unwrap()
            .contains("reviewed value=")
    );
    let followup = env
        .state
        .agent()
        .run_turn(env.chat, "what happened?")
        .unwrap();
    env.state.agent().wait(followup).await.unwrap();
    let requests = env.provider.requests.lock();
    assert!(
        requests
            .iter()
            .all(|request| !request.contains(secret) && !request.contains("reviewed value="))
    );
}

#[tokio::test]
async fn short_passwords_do_not_corrupt_tool_correlation_ids() {
    let env = setup();
    let _approver = approve_replays(&env); // a write after a read asks (tainted egress)
    let id = env
        .state
        .agent()
        .run_turn(env.chat, "call:mutate {\"password\":\"a\"}")
        .unwrap();
    assert_eq!(
        env.state.agent().wait(id).await.unwrap(),
        task_runtime::RunState::Succeeded
    );
    mark_owner(&env, id);
    let (status, proposal) = request(
        &env,
        Method::POST,
        &format!("/v1/runs/{id}/workflow-proposal"),
        None,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{proposal}");
    assert_eq!(proposal["workflow"]["steps"].as_array().unwrap().len(), 1);
    assert_eq!(proposal["required_params"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn only_owner_typed_runs_are_proposable_not_token_or_telegram_turns() {
    let env = setup();
    let id = source(&env).await;
    let message = env
        .state
        .storage()
        .list_messages(env.chat)
        .unwrap()
        .into_iter()
        .find(|m| m.run_id == Some(id) && m.role == "user")
        .expect("user message of the run");
    let path = format!("/v1/runs/{id}/workflow-proposal");
    let propose = || request(&env, Method::POST, &path, None, true);
    // A run kind of `turn` is also what a token-only client or Telegram starts;
    // only the recorded message source tells them apart from the owner.
    for refused in ["local_api", "telegram", "peer", ""] {
        env.state
            .storage()
            .set_message_source(message.id, refused)
            .unwrap();
        assert_eq!(
            propose().await.0,
            StatusCode::CONFLICT,
            "source {refused:?}"
        );
    }
    env.state
        .storage()
        .set_message_source(message.id, "owner")
        .unwrap();
    assert_eq!(propose().await.0, StatusCode::OK);
}

#[tokio::test]
async fn failed_and_child_runs_conflict_and_run_endpoint_keeps_permissions() {
    let env = setup();
    let id = source(&env).await;
    let original = env.state.storage().get_run(id).unwrap().unwrap();
    // Third-party initiated runs (channel/external agent/schedule/routine)
    // must not be proposable: their text would count as the user's request.
    for (state, parent, kind) in [
        ("failed", None, original.kind.as_str()),
        ("succeeded", Some(id), original.kind.as_str()),
        ("succeeded", None, "turn:channel"),
        ("succeeded", None, "turn:external_agent:abc"),
    ] {
        let mut row = original.clone();
        row.id = RunId::new();
        row.state = state.into();
        row.parent_run_id = parent;
        row.kind = kind.into();
        env.state.storage().insert_run(&row).unwrap();
        assert_eq!(
            request(
                &env,
                Method::POST,
                &format!("/v1/runs/{}/workflow-proposal", row.id),
                None,
                true
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
    }
    let (_, proposal) = request(
        &env,
        Method::POST,
        &format!("/v1/runs/{id}/workflow-proposal"),
        None,
        true,
    )
    .await;
    let save = json!({"run_id":id.0,"name":"recorded","description":"task","params":proposal["workflow"]["params"], "steps":proposal["workflow"]["steps"],"confirmed_steps":[0,1,2]});
    assert_eq!(
        request(&env, Method::POST, "/v1/workflows", Some(save), true)
            .await
            .0,
        StatusCode::CREATED
    );
    env.state
        .agent()
        .set_global_policy(permission_engine::Policy::read_only());
    let before = env.calls.load(Ordering::SeqCst);
    let (status, body) = request(
        &env,
        Method::POST,
        "/v1/workflows/recorded/run",
        Some(json!({"chat_id":env.chat.0,"params":{}})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let run = RunId(body["run_id"].as_str().unwrap().parse().unwrap());
    let _ = env.state.agent().wait(run).await.unwrap();
    assert_eq!(
        env.calls.load(Ordering::SeqCst),
        before,
        "read-only policy must deny replay writes"
    );
}

#[tokio::test]
async fn phone_peers_cannot_read_propose_save_delete_or_run_workflows() {
    let env = setup();
    for (method, path) in [
        (Method::GET, "/v1/workflows"),
        (Method::GET, "/v1/workflows/flow"),
        (Method::POST, "/v1/workflows"),
        (Method::DELETE, "/v1/workflows/flow"),
        (Method::POST, "/v1/workflows/flow/run"),
        (Method::POST, "/v1/%77orkflows/flow/run"),
        (Method::POST, "/v1//workflows/flow/run/"),
        (
            Method::POST,
            "/v1/runs/00000000-0000-0000-0000-000000000000/workflow-proposal",
        ),
    ] {
        let mut req = Request::builder()
            .method(method)
            .uri(path)
            .body(Body::empty())
            .unwrap();
        req.extensions_mut().insert(crate::PeerIdentity {
            key: "phone".into(),
            role: crate::PeerRole::Phone,
        });
        let response = crate::peer_router(env.state.clone())
            .oneshot(req)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{path}");
    }
    let bot = env
        .state
        .storage()
        .get_chat(env.chat)
        .unwrap()
        .unwrap()
        .bot_id;
    let mut req = Request::builder()
        .uri(format!("/v1/workflows?bot_id={bot}"))
        .body(Body::empty())
        .unwrap();
    req.extensions_mut().insert(crate::PeerIdentity {
        key: "desktop".into(),
        role: crate::PeerRole::Desktop,
    });
    assert_eq!(
        crate::peer_router(env.state)
            .oneshot(req)
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn dropped_mutating_call_refuses_both_proposal_and_save() {
    let env = setup();
    let _approver = approve_replays(&env); // the drag taints the run, so the last write asks
    let id = env
        .state
        .agent()
        .run_turn(
            env.chat,
            "call:mutate {}\ncall:desktop_drag {}\ncall:mutate {}",
        )
        .unwrap();
    assert_eq!(
        env.state.agent().wait(id).await.unwrap(),
        task_runtime::RunState::Succeeded
    );
    assert_eq!(
        request(
            &env,
            Method::POST,
            &format!("/v1/runs/{id}/workflow-proposal"),
            None,
            true
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let body = json!({"run_id":id.0,"name":"incomplete","description":"missing drag","params":[],"steps":[]});
    assert_eq!(
        request(&env, Method::POST, "/v1/workflows", Some(body), true)
            .await
            .0,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn tainted_proposal_requires_each_step_confirmation_and_persists_source() {
    let env = setup();
    let id = source(&env).await;
    let (_, proposal) = request(
        &env,
        Method::POST,
        &format!("/v1/runs/{id}/workflow-proposal"),
        None,
        true,
    )
    .await;
    assert_eq!(proposal["workflow"]["source"]["untrusted_content"], true);
    assert_eq!(
        proposal["step_provenance"][0]["origin"],
        "run_content_or_model"
    );
    assert_eq!(proposal["step_provenance"].as_array().unwrap().len(), 3);
    assert!(
        proposal["step_provenance"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["needs_confirmation"] == true)
    );
    let mut body = json!({"run_id":id.0,"name":"tainted","description":"review","params":proposal["workflow"]["params"],"steps":proposal["workflow"]["steps"]});
    assert_eq!(
        request(
            &env,
            Method::POST,
            "/v1/workflows",
            Some(body.clone()),
            true
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    body["confirmed_steps"] = json!([0, 1]);
    assert_eq!(
        request(
            &env,
            Method::POST,
            "/v1/workflows",
            Some(body.clone()),
            true
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    body["confirmed_steps"] = json!([0, 1, 2]);
    let (status, saved) = request(&env, Method::POST, "/v1/workflows", Some(body), true).await;
    assert_eq!(status, StatusCode::CREATED, "{saved}");
    assert_eq!(saved["source"]["run_id"], id.to_string());
    let before = env.calls.load(Ordering::SeqCst);
    let mut events = env.state.agent().subscribe();
    let (_, replay) = request(
        &env,
        Method::POST,
        "/v1/workflows/tainted/run",
        Some(json!({"chat_id":env.chat.0})),
        true,
    )
    .await;
    let run = RunId(replay["run_id"].as_str().unwrap().parse().unwrap());
    for index in 0..3 {
        let call_id = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                if let Event::ApprovalNeeded(a) = events.recv().await.unwrap().envelope.event {
                    break a.call_id;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(env.calls.load(Ordering::SeqCst), before + index);
        // An always grant is refused for an egress approval, so it cannot waive the following tainted step.
        env.state
            .agent()
            .approve(
                call_id,
                agent_core::ApprovalScope::Always,
                env.state.user_id(),
            )
            .unwrap_err();
        env.state
            .agent()
            .approve(
                call_id,
                agent_core::ApprovalScope::Once,
                env.state.user_id(),
            )
            .unwrap();
    }
    assert_eq!(
        env.state.agent().wait(run).await.unwrap(),
        task_runtime::RunState::Succeeded
    );
}

#[tokio::test]
async fn real_browser_fill_short_text_does_not_change_other_step_locators() {
    let env = setup();
    let _approver = approve_replays(&env); // a write after a read asks (tainted egress)
    let id = env.state.agent().run_turn(env.chat, "call:browser_snapshot {}\ncall:browser_fill {\"ref\":\"e1\",\"text\":\"e\"}\ncall:browser_click {\"ref\":\"e2\"}").unwrap();
    assert_eq!(
        env.state.agent().wait(id).await.unwrap(),
        task_runtime::RunState::Succeeded
    );
    mark_owner(&env, id);
    let (status, proposal) = request(
        &env,
        Method::POST,
        &format!("/v1/runs/{id}/workflow-proposal"),
        None,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{proposal}");
    let steps = &proposal["workflow"]["steps"];
    assert_eq!(steps[0]["target"]["name"], "Search");
    assert_eq!(steps[1]["target"]["name"], "Go");
    assert_eq!(steps[1]["url_pattern"], "https://example.test/*");
    assert_eq!(steps[0]["value"], "{recorded_secret_1}");
    assert!(
        proposal["workflow"]["params"][0]
            .get("default")
            .is_none_or(Value::is_null)
    );
}

#[tokio::test]
async fn workflows_are_bot_scoped_and_corrupt_files_do_not_break_list() {
    let env = setup();
    let id = source(&env).await;
    let (_, proposal) = request(
        &env,
        Method::POST,
        &format!("/v1/runs/{id}/workflow-proposal"),
        None,
        true,
    )
    .await;
    let body = json!({"run_id":id.0,"name":"owned","description":"review","params":proposal["workflow"]["params"],"steps":proposal["workflow"]["steps"],"confirmed_steps":[0,1,2]});
    assert_eq!(
        request(&env, Method::POST, "/v1/workflows", Some(body), true)
            .await
            .0,
        StatusCode::CREATED
    );
    let original = env.state.storage().get_chat(env.chat).unwrap().unwrap();
    let mut bot = env
        .state
        .storage()
        .get_bot(original.bot_id)
        .unwrap()
        .unwrap();
    bot.id = protocol::BotId::new();
    env.state.storage().insert_bot(&bot).unwrap();
    let profile = agent_core::BotProfile::from_toml_str(
        "[bot]\nid='other'\nname='other'\n[models]\nexecutor='fake/echo'\n",
    )
    .unwrap();
    env.state.agent().add_profile(bot.id, profile);
    let mut chat = original.clone();
    chat.id = ChatId::new();
    chat.bot_id = bot.id;
    env.state.storage().insert_chat(&chat).unwrap();
    assert_eq!(
        request(
            &env,
            Method::GET,
            &format!("/v1/workflows?bot_id={}", bot.id),
            None,
            true
        )
        .await
        .1,
        json!([])
    );
    assert_eq!(
        request(
            &env,
            Method::POST,
            "/v1/workflows/owned/run",
            Some(json!({"chat_id":chat.id.0})),
            true
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let scoped = WorkflowStore::new(env.dir.path())
        .for_bot(&original.bot_id.to_string())
        .unwrap();
    std::fs::write(scoped.dir().join("corrupt.toml"), "invalid TOML [").unwrap();
    let (status, list) = request(&env, Method::GET, "/v1/workflows", None, true).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert!(scoped.dir().starts_with(env.dir.path().join("bots")));
    let hidden = agent_core::BotProfile::from_toml_str("[bot]\nid='test'\nname='test'\n[models]\nexecutor='fake/echo'\n[tools]\nallow=['fs_read']\n").unwrap();
    env.state.agent().add_profile(original.bot_id, hidden);
    assert_eq!(
        request(
            &env,
            Method::POST,
            &format!("/v1/runs/{id}/workflow-proposal"),
            None,
            true
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn bot_file_tools_cannot_forge_automation_in_the_scoped_store() {
    let env = setup();
    let bot = env
        .state
        .storage()
        .get_chat(env.chat)
        .unwrap()
        .unwrap()
        .bot_id;
    let store = WorkflowStore::new(env.dir.path())
        .for_bot(&bot.to_string())
        .unwrap();
    std::fs::create_dir_all(store.dir()).unwrap();
    let gate = builtin_tools::ResolverGate::new(
        permission_engine::PermissionResolver::new(),
        permission_engine::Policy::full(),
        None,
        protocol::InitiatorKind::Human,
    );
    let config = builtin_tools::BuiltinConfig::new(env.dir.path().to_path_buf(), Arc::new(gate));
    let tools = builtin_tools::build_builtin_tools(config, &ToolRegistry::new());
    let write = tools.iter().find(|t| t.spec().name == "fs_write").unwrap();
    let path = store.dir().join("forged.toml");
    let result = write
        .call(
            &ToolContext::default(),
            json!({"path":path,"content":"version=1\nname='forged'\nsteps=[]\n"}),
        )
        .await;
    assert!(
        matches!(result, Err(ToolError::PermissionDenied(_))),
        "{result:?}"
    );
    assert!(!path.exists());
}

/// Safe mode must not replay a saved workflow, by a matching chat line or by
/// the run endpoint: the tools behind the steps never run.
#[tokio::test]
async fn safe_mode_never_replays_a_saved_workflow() {
    let env = setup();
    let id = source(&env).await;
    let (_, proposal) = request(
        &env,
        Method::POST,
        &format!("/v1/runs/{id}/workflow-proposal"),
        None,
        true,
    )
    .await;
    let save = json!({"run_id":id.0, "name":"recorded", "description":"d", "params":proposal["workflow"]["params"], "steps":proposal["workflow"]["steps"],"confirmed_steps":[0,1,2]});
    assert_eq!(
        request(&env, Method::POST, "/v1/workflows", Some(save), true)
            .await
            .0,
        StatusCode::CREATED
    );
    let chat = env.chat;
    let Env { dir, .. } = env;
    let env = setup_with(dir, true, Some(chat));
    let approvals = approve_replays(&env);
    env.provider.saved.store(true, Ordering::SeqCst);
    let run = env
        .state
        .agent()
        .run_turn(env.chat, "do the recorded thing")
        .unwrap();
    env.state.agent().wait(run).await.unwrap();
    assert_eq!(
        env.calls.load(Ordering::SeqCst),
        0,
        "chat line replayed a workflow"
    );
    let (status, _) = request(
        &env,
        Method::POST,
        "/v1/workflows/recorded/run",
        Some(json!({"chat_id": chat.0, "params": {}})),
        true,
    )
    .await;
    assert!(!status.is_success(), "run endpoint accepted: {status}");
    assert_eq!(
        env.calls.load(Ordering::SeqCst),
        0,
        "endpoint replayed a workflow"
    );
    approvals.abort();
}
