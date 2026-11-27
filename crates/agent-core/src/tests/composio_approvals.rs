//! Real Composio adapters through the production loop and replay/subagent gates:
//! money always asks, reads run without a card, writes follow the level.
use super::*;
use wiremock::matchers::{path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct Audit;
impl composio::Audit for Audit {
    fn record(&self, _: &str, _: Value) -> Result<(), String> {
        Ok(())
    }
}

fn tool(slug: &str) -> Value {
    json!({"slug":slug,"toolkit":{"slug":"github"},"version":"20260901_00","description":slug,
        "input_parameters":{"type":"object","properties":{"owner":{"type":"string"}}}})
}

async fn connected_registry(env: &Env) -> (ToolRegistry, MockServer) {
    let server = MockServer::start().await;
    for (url, body) in [
        (
            "/toolkits",
            json!({"items":[{"slug":"github","name":"GitHub"}]}),
        ),
        ("/auth_configs", json!({"items":[{"id":"ac_install"}]})),
        (
            "/connected_accounts/link",
            json!({"redirect_url":"https://connect.composio.dev/link/fake","connected_account_id":"ca_install","expires_at":"2030-01-01"}),
        ),
        (
            "/connected_accounts",
            json!({"items":[{"id":"ca_install","toolkit":{"slug":"github"},"status":"ACTIVE"}]}),
        ),
        (
            "/tools",
            json!({"items":[
                tool("GITHUB_LIST_REPOSITORY_ISSUES"),
                tool("GITHUB_CREATE_AN_ISSUE"),
                tool("GITHUB_ODD_THING"),
                tool("GITHUB_DELETE_A_REPOSITORY"),
                tool("GITHUB_PAY_INVOICE"),
            ]}),
        ),
    ] {
        Mock::given(path(url))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
    }
    Mock::given(path_regex("^/tools/execute/"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"successful":true,"data":{}})),
        )
        .mount(&server)
        .await;
    let registry = ToolRegistry::new();
    let svc = composio::ComposioService::new(composio::ServiceConfig {
        data_dir: env.dir.path().to_owned(),
        keys: Arc::new(composio::LocalKeySource::new(Arc::new(
            secret_store::MemoryStore::new(),
        ))),
        registry: registry.clone(),
        redact: Arc::new(str::to_owned),
        register_secret: Arc::new(|_| {}),
        audit: Arc::new(Audit),
        loopback_base: Some(server.uri()),
    })
    .unwrap();
    svc.set_key("fake-offline-key").await.unwrap();
    svc.connect("github").await.unwrap();
    svc.app_status("github").await.unwrap();
    (registry, server)
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Keep the end-to-end policy/caller matrix together.
async fn composio_payment_asks_for_all_policies_grants_and_callers() {
    for mode in [
        "full",
        "accounts_allow",
        "tool_grant",
        "category_grant",
        "chat_grant",
        "waiver",
        "schedule",
        "schedule_tool",
        "scheduled_replay",
        "subagent",
        "replay",
        "generic_run",
    ] {
        eprintln!("approval scenario: {mode}");
        let env = setup();
        let (registry, server) = connected_registry(&env).await;
        let direct = "app:github/pay_invoice";
        let (name, args) = if mode == "generic_run" {
            (
                "apps_run_action",
                json!({"app":"github","action":"pay_invoice","arguments":{"owner":"o"}}),
            )
        } else {
            (direct, json!({"owner":"o"}))
        };
        let t = registry.get(name).unwrap();
        assert!(t.always_ask() || t.always_ask_for(&args), "{mode}");
        let model_call = if name == "apps_run_action" {
            calls(&[("c1", name, args.clone())])
        } else {
            calls(&[("c1", "use_tool", json!({"name":name,"input":args}))])
        };
        let mut replies = vec![model_call, text("done")];
        if mode == "subagent" {
            replies = vec![
                calls(&[(
                    "spawn",
                    "spawn_subagent",
                    json!({"task":"pay the invoice","context":"use the connected GitHub app"}),
                )]),
                calls(&[("child", "use_tool", json!({"name":name,"input":args}))]),
                text("child done"),
                text("parent done"),
            ];
        }
        let fake = Fake::new(replies);
        let mut policy = if matches!(
            mode,
            "full"
                | "subagent"
                | "replay"
                | "schedule"
                | "schedule_tool"
                | "scheduled_replay"
                | "generic_run"
        ) {
            Policy::full()
        } else {
            Policy::approve_destructive()
        };
        if mode == "accounts_allow" {
            policy
                .categories
                .insert(PermissionCategory::Accounts, Rule::new(Level::Allow));
        }
        let lp = AgentLoop::new(
            env.storage.clone(),
            registry,
            Providers::single(fake as Arc<dyn ChatProvider>),
            cfg(policy),
        )
        .with_subagents();
        lp.add_profile(env.bot, profile(""));
        {
            let mut grants = lp.sh.grants.lock();
            if matches!(mode, "tool_grant" | "category_grant") {
                grants.always.push(crate::Grant {
                    bot_id: env.bot,
                    target: if mode == "tool_grant" {
                        GrantTarget::Tool(name.into())
                    } else {
                        GrantTarget::Category(PermissionCategory::Accounts)
                    },
                    granted_at_ms: 1,
                    computer: None,
                    inert: false,
                });
            }
            if mode == "chat_grant" {
                grants
                    .this_chat
                    .insert((env.chat, env.bot, name.into(), None));
            }
        }
        let store = workflow_replay::WorkflowStore::new(env.dir.path());
        let mut workflow = workflow_replay::Workflow::new("app_pay");
        workflow.steps = vec![workflow_replay::Step::Tool(workflow_replay::ToolStep {
            tool: name.into(),
            input: args.clone(),
        })];
        store.save(&workflow).unwrap();
        let lp = lp.with_workflows(
            store,
            workflow_replay::Handles {
                desktop: None,
                browser: None,
                tools: ToolRegistry::new(),
            },
        );
        let mut rx = lp.subscribe();
        let run = match mode {
            "schedule" => lp
                .run_scheduled(
                    env.chat,
                    protocol::TaskId::new(),
                    crate::ScheduleWork::Prompt("pay the invoice".into()),
                )
                .unwrap(),
            "schedule_tool" => lp
                .run_scheduled(
                    env.chat,
                    protocol::TaskId::new(),
                    crate::ScheduleWork::Tool {
                        name: "use_tool".into(),
                        input: json!({"name":name,"input":args}),
                    },
                )
                .unwrap(),
            "scheduled_replay" => lp
                .run_scheduled(
                    env.chat,
                    protocol::TaskId::new(),
                    crate::ScheduleWork::Workflow {
                        name: "app_pay".into(),
                        params: std::collections::BTreeMap::default(),
                    },
                )
                .unwrap(),
            "replay" => lp.run_turn(env.chat, "/workflow app_pay").unwrap(),
            _ => lp.run_turn(env.chat, "pay the invoice").unwrap(),
        };
        let mut observed = Vec::new();
        let a = loop {
            let next = tokio::time::timeout(Duration::from_secs(10), rx.recv()).await;
            let item = next
                .unwrap_or_else(|_| panic!("{mode}: no approval; events: {observed:?}"))
                .unwrap();
            if let Event::ApprovalNeeded(a) = item.envelope.event {
                break a;
            }
            observed.push(item.envelope.event);
        };
        assert_eq!(a.tool_name, name, "{mode}");
        assert_eq!(a.details.as_ref().unwrap()["kind"], "app_action");
        assert!(
            !server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .any(|r| r.url.path().starts_with("/tools/execute")),
            "{mode}: executed before approval"
        );
        for scope in [
            ApprovalScope::Always,
            ApprovalScope::AlwaysCategory,
            ApprovalScope::ThisChat,
        ] {
            assert!(
                lp.approve(a.call_id, scope, env.user).is_err(),
                "{mode}: standing approval accepted"
            );
        }
        if mode == "waiver" {
            lp.set_global_policy(Policy::full());
        }
        lp.approve(a.call_id, ApprovalScope::Once, env.user)
            .unwrap();
        assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded, "{mode}");
        let sent = server.received_requests().await.unwrap();
        assert_eq!(
            sent.iter()
                .filter(|r| r.url.path().starts_with("/tools/execute"))
                .count(),
            1,
            "{mode}"
        );
    }
}

/// Run one model call of `name` and say whether a card came up first. A card is
/// approved once; the run must then succeed and send exactly one request.
/// `owner_full`: the bot is on the Full level and the owner typed the message in the app
/// (otherwise the run is a plain turn on the given policy).
async fn card_for(policy: Policy, owner_full: bool, name: &str, args: Value) -> bool {
    let env = setup();
    let (registry, server) = connected_registry(&env).await;
    let model_call = if name == "apps_run_action" || name == "apps_read_action" {
        calls(&[("c1", name, args.clone())])
    } else {
        calls(&[("c1", "use_tool", json!({"name":name,"input":args}))])
    };
    let fake = Fake::new(vec![model_call, text("done")]);
    let lp = AgentLoop::new(
        env.storage.clone(),
        registry,
        Providers::single(fake as Arc<dyn ChatProvider>),
        cfg(policy),
    );
    lp.add_profile(
        env.bot,
        profile(if owner_full {
            "[permissions]\npreset = \"full\"\n"
        } else {
            ""
        }),
    );
    let mut rx = lp.subscribe();
    let run = if owner_full {
        let crate::Delivery::NewTurn(run) = lp
            .send_user_message_from(env.chat, "do it", crate::MessageMode::Steer, "owner")
            .unwrap()
        else {
            unreachable!()
        };
        run
    } else {
        lp.run_turn(env.chat, "do it").unwrap()
    };
    let mut card = None;
    // `wait` takes the run's task handle, so the one future is kept for after the approval.
    let waiting = lp.wait(run);
    tokio::pin!(waiting);
    let finished = {
        loop {
            tokio::select! {
                // The card event is queued before the run reports WaitingApproval.
                biased;
                item = rx.recv() => {
                    if let Event::ApprovalNeeded(a) = item.unwrap().envelope.event {
                        card = Some(a);
                        break None;
                    }
                }
                state = &mut waiting => break Some(state.unwrap()),
                () = tokio::time::sleep(Duration::from_secs(10)) => panic!("{name}: stuck"),
            }
        }
    };
    let sent = || async {
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path().starts_with("/tools/execute"))
            .count()
    };
    if let Some(a) = card {
        assert_eq!(a.tool_name, name);
        assert_eq!(sent().await, 0, "{name}: executed before approval");
        lp.approve(a.call_id, ApprovalScope::Once, env.user)
            .unwrap();
        assert_eq!(waiting.await.unwrap(), RunState::Succeeded, "{name}");
        assert_eq!(sent().await, 1, "{name}");
        true
    } else {
        assert_eq!(finished, Some(RunState::Succeeded), "{name}");
        assert_eq!(
            sent().await,
            1,
            "{name}: ran without a card but sent nothing"
        );
        false
    }
}

#[tokio::test]
async fn composio_reads_skip_the_card_and_writes_follow_the_level() {
    let direct = json!({"owner":"o"});
    let via = |action: &str| json!({"app":"github","action":action,"arguments":{"owner":"o"}});
    // (tool, input, card on the default level, card on Full)
    let cases = [
        (
            "app:github/list_repository_issues",
            direct.clone(),
            false,
            false,
        ),
        ("app:github/create_an_issue", direct.clone(), true, false),
        ("app:github/odd_thing", direct.clone(), true, false),
        (
            "app:github/delete_a_repository",
            direct.clone(),
            true,
            false,
        ),
        ("app:github/pay_invoice", direct, true, true),
        (
            "apps_read_action",
            via("list_repository_issues"),
            false,
            false,
        ),
        ("apps_run_action", via("create_an_issue"), true, false),
        ("apps_run_action", via("pay_invoice"), true, true),
    ];
    for (name, input, default_card, full_card) in cases {
        let got = card_for(Policy::approve_destructive(), false, name, input.clone()).await;
        assert_eq!(got, default_card, "{name} on the default level");
        let got = card_for(Policy::full(), true, name, input.clone()).await;
        assert_eq!(got, full_card, "{name} on Full");
        // A run the owner did not start by hand never gets the Full waiver.
        if name != "app:github/list_repository_issues" && name != "apps_read_action" {
            let got = card_for(Policy::full(), false, name, input).await;
            assert!(
                got,
                "{name}: a Full policy without the owner's own run still asks"
            );
        }
    }
}
