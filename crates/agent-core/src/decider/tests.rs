use super::*;
use crate::tests::{Fake, cfg, make_loop, profile, setup, text};
use permission_engine::Policy;
use serde_json::json;

#[test]
fn settings_are_opt_in_and_validate_limits_and_provider_kind() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(FILE);
    assert!(!Settings::load(dir.path()).unwrap().enabled);

    std::fs::write(
        &path,
        r#"
enabled = true
[[backends]]
api = "openrouter_decisions"
model = "openrouter/vendor/decision-model"
rpm = 10
"#,
    )
    .unwrap();
    let settings = Settings::load(dir.path()).unwrap();
    assert!(settings.enabled);
    assert_eq!(settings.backends[0].api, Api::OpenRouterDecisions);

    for invalid in [
        "enabled = true",
        "timeout_ms = 1501",
        "max_calls_per_turn = 0",
        "[[backends]]\nmodel = \"fake/m\"\nrpm = 0",
        "[[backends]]\napi = \"gemini_decisions\"\nmodel = \"custom/m\"\nrpm = 10",
    ] {
        std::fs::write(&path, invalid).unwrap();
        assert!(Settings::load(dir.path()).is_err(), "{invalid}");
    }
}

#[test]
fn provider_reload_rebuilds_the_opt_in_pool_without_probing_the_provider() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = Catalog::from_entries([toml::from_str(include_str!(
        "../../../../providers/catalog/ollama.toml"
    ))
    .unwrap()]);
    let store = secret_store::MemoryStore::new();
    let mut config = ProviderConfig::default();
    config.upsert(provider_catalog::ProviderSettings::new("ollama", 0));
    config
        .save(&provider_catalog::config_path(dir.path()))
        .unwrap();
    let load = || crate::build_providers(dir.path(), &catalog, &store, &mut |_| {}).unwrap();
    assert!(load().decider().is_none());
    std::fs::write(
        dir.path().join(FILE),
        r#"
enabled = true
max_calls_per_turn = 3
[[backends]]
model = "missing/no-provider"
rpm = 10
[[backends]]
model = "ollama/fixture-model"
rpm = 20
"#,
    )
    .unwrap();
    assert_eq!(load().decider().unwrap().budget().remaining(), 3);
}

#[tokio::test]
async fn turn_decider_uses_only_redacted_user_text_and_records_its_own_call() {
    let env = setup();
    let tool_secret = "fixture-secret-for-decider";
    let live_redactor = crate::SharedRedactor::default();
    live_redactor.register(tool_secret);
    let classifier = Fake::new(vec![text(
        r#"{"answers":{"hello":{"type":"noul","noul":0.9}}}"#,
    )]);
    let main = Fake::new(vec![crate::tests::Reply::Hang]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &main,
        &[],
        profile(""),
        crate::LoopConfig {
            redactor: Some(live_redactor),
            ..cfg(Policy::full())
        },
    );
    let run_id = lp
        .run_turn(env.chat, &format!("original greeting {tool_secret}"))
        .unwrap();
    env.storage
        .insert_message(&storage::Message {
            id: uuid::Uuid::now_v7(),
            user_id: env.user,
            chat_id: env.chat,
            run_id: Some(run_id),
            role: "tool".into(),
            content: "raw tool fixture".into(),
            created_at_ms: storage::now_ms(),
            exposure_untrusted: true,
            exposure_private: false,
            exposure_sources: vec!["fixture".into()],
        })
        .unwrap();
    let run = env.storage.get_run(run_id).unwrap().unwrap();
    let scope = Scope {
        sh: Arc::clone(&lp.sh),
        ctx: lp.sh.ctx(&run).unwrap(),
    };
    let decider = TurnDecider {
        pool: Decider::new(
            vec![Backend::new(
                Box::new(ChatJsonBackend(Arc::new(DeciderClassifier {
                    model: ModelRef("fake/decider".into()),
                    provider: Arc::clone(&classifier) as Arc<dyn ChatProvider>,
                }))),
                NonZeroU32::new(10).unwrap(),
            )],
            MAX_TIMEOUT,
        ),
        max_calls: 1,
    };
    let questions: Questions = [(
        "hello".into(),
        Question::YesNo {
            instructions: format!("Is this a greeting? {tool_secret}"),
        },
    )]
    .into();
    let budget = decider.budget();
    assert_eq!(
        decider.decide(questions.clone(), &budget).await,
        Answers::new()
    );
    let answer = scope.run(decider.decide(questions, &budget)).await;
    assert_eq!(
        answer["hello"],
        agent_router::decider::Answer::YesNo { yes: 0.9 }
    );

    let sent = classifier.request(0);
    assert_eq!(sent.tools, Vec::<serde_json::Value>::new());
    let request = serde_json::to_string(&sent).unwrap();
    assert!(request.contains("original greeting"));
    assert!(request.contains(secret_store::REDACTED));
    assert!(!request.contains(tool_secret));
    assert!(!request.contains("raw tool fixture"));
    let rows = env.storage.list_audit_for_run(env.user, run_id).unwrap();
    let decider: Vec<_> = rows
        .iter()
        .filter(|r| r.detail["purpose"] == "decider")
        .collect();
    assert_eq!(decider.len(), 2);
    assert_eq!(decider[0].action, "llm.call_started");
    assert_eq!(decider[1].action, "usage");
    assert_eq!(decider[1].detail["call_id"], decider[0].detail["call_id"]);
    assert_eq!(decider[1].detail["tokens"]["prompt_tokens"], json!(10));
    lp.cancel(run_id).unwrap();
}
