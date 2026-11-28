//! Real router/storage tests for source ownership, bounded adapters and grants.
use super::*;
use memory_store::knowledge::{KnowledgeStore, Scope, SearchRequest};
use protocol::{BotId, ChatId, RunId};
use uuid::Uuid;

#[path = "knowledge_documents.rs"]
mod documents;
#[path = "knowledge_regressions.rs"]
mod review;

async fn request(t: &T, method: Method, path: &str, body: Option<Value>) -> Resp {
    // Index admission is deliberately host-wide. Other concurrently running
    // integration tests can briefly own that worker; there is no daemon queue.
    for _ in 0..100 {
        let result = call(t, method.clone(), path, body.clone()).await;
        if result.status != StatusCode::TOO_MANY_REQUESTS {
            return result;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("knowledge worker never became available");
}

fn index(t: &T, bot: &str) -> KnowledgeStore {
    KnowledgeStore::open(
        t.state.data_dir(),
        Scope {
            user_id: t.state.user_id(),
            bot_id: BotId(bot.parse().unwrap()),
        },
    )
    .unwrap()
}

async fn add(t: &T, bot: &str, body: Value) -> Value {
    let response = request(
        t,
        Method::POST,
        &format!("/v1/bots/{bot}/knowledge/sources"),
        Some(body),
    )
    .await;
    assert_eq!(response.status, StatusCode::CREATED, "{}", response.json());
    response.json()
}

#[tokio::test]
async fn knowledge_crud_is_authenticated_scoped_and_revision_checked() {
    let _serial = crate::knowledge_runtime::test_guard().await;
    let t = setup();
    let (bot, _) = make_chat(&t).await;
    let (other, _) = make_chat(&t).await;
    let base = format!("/v1/bots/{bot}/knowledge/sources");
    let unauthenticated = send(&t.app, Method::GET, &base, None, &[]).await;
    assert_eq!(unauthenticated.status, StatusCode::UNAUTHORIZED);
    for field in ["user_id", "bot_id", "trust", "remote_embedding"] {
        let mut input = json!({"kind":"note","label":"Selected note","content":"WPanel tenant hostname resolution"});
        input[field] = json!("forged");
        assert!(
            request(&t, Method::POST, &base, Some(input))
                .await
                .status
                .is_client_error()
        );
    }
    let source = add(&t, &bot, json!({"kind":"note","label":"Selected note","content":"WPanel tenant hostname resolution"})).await["source"].clone();
    assert!(source["remote_profile_fingerprint"].is_null());
    assert_eq!(
        index(&t, &bot)
            .search(&SearchRequest::new("hostname"))
            .unwrap()
            .hits
            .len(),
        1
    );
    assert_eq!(
        index(&t, &other)
            .search(&SearchRequest::new("hostname"))
            .unwrap()
            .hits
            .len(),
        0
    );
    let id = source["id"].as_str().unwrap();
    let revision = source["revision"].as_u64().unwrap();
    let foreign = request(
        &t,
        Method::DELETE,
        &format!("/v1/bots/{other}/knowledge/sources/{id}"),
        Some(json!({"expected_revision":revision})),
    )
    .await;
    assert_eq!(foreign.status, StatusCode::NOT_FOUND);
    let path = format!("{base}/{id}");
    let disabled = request(
        &t,
        Method::PUT,
        &path,
        Some(json!({"action":"disable","expected_revision":revision})),
    )
    .await;
    assert_eq!(disabled.status, StatusCode::OK);
    assert_eq!(
        index(&t, &bot)
            .search(&SearchRequest::new("hostname"))
            .unwrap()
            .hits
            .len(),
        0
    );
    let stale = request(
        &t,
        Method::PUT,
        &path,
        Some(json!({"action":"enable","expected_revision":revision})),
    )
    .await;
    assert_eq!(stale.status, StatusCode::CONFLICT);
    let enabled = request(
        &t,
        Method::PUT,
        &path,
        Some(json!({"action":"enable","expected_revision":disabled.json()["revision"]})),
    )
    .await;
    assert_eq!(enabled.status, StatusCode::OK);
    let removed = request(
        &t,
        Method::DELETE,
        &path,
        Some(json!({"expected_revision":enabled.json()["revision"]})),
    )
    .await;
    assert_eq!(removed.status, StatusCode::NO_CONTENT);
    assert_eq!(
        index(&t, &bot)
            .search(&SearchRequest::new("hostname"))
            .unwrap()
            .hits
            .len(),
        0
    );
}

#[tokio::test]
async fn knowledge_folder_resumes_prunes_and_saved_web_is_pasted_data() {
    let _serial = crate::knowledge_runtime::test_guard().await;
    let t = setup();
    let (bot, _) = make_chat(&t).await;
    let folder = tempfile::tempdir().unwrap();
    for i in 0..35 {
        std::fs::write(
            folder.path().join(format!("{i:02}.md")),
            format!("# Decision {i}\n\nshared benchmark retrieval text {i}\n"),
        )
        .unwrap();
    }
    std::fs::write(
        folder.path().join(".env"),
        "credential must never be indexed",
    )
    .unwrap();
    let first = add(
        &t,
        &bot,
        json!({"kind":"folder","label":"Engineering notes","path":folder.path()}),
    )
    .await;
    assert_eq!(first["indexing"]["processed"], 32);
    assert!(first["indexing"]["next_cursor"].is_string());
    let id = first["source"]["id"].as_str().unwrap();
    let route = format!("/v1/bots/{bot}/knowledge/sources/{id}/reindex");
    let next = request(&t, Method::POST, &route, Some(json!({"expected_revision":first["source"]["revision"],"cursor":first["indexing"]["next_cursor"]}))).await;
    assert_eq!(next.status, StatusCode::OK, "{}", next.json());
    assert_eq!(next.json()["source"]["documents"], 35);
    assert_eq!(next.json()["indexing"]["processed"], 3);
    assert!(next.json()["indexing"]["next_cursor"].is_null());
    std::fs::remove_file(folder.path().join("34.md")).unwrap();
    let resume = request(
        &t,
        Method::POST,
        &route,
        Some(json!({"expected_revision":next.json()["source"]["revision"],"cursor":"31.md"})),
    )
    .await;
    assert_eq!(resume.status, StatusCode::OK);
    assert_eq!(resume.json()["indexing"]["removed"], 1);
    assert_eq!(resume.json()["source"]["documents"], 34);
    let saved = add(&t, &bot, json!({"kind":"saved_web","label":"Saved offline page","url":"http://127.0.0.1:1/unreachable","html":true,"content":"<main><h1>Offline nebula</h1><p>Saved selected information.</p><script>badScriptProbe()</script></main>"})).await;
    assert_eq!(saved["source"]["documents"], 1);
    let hits = index(&t, &bot)
        .search(&SearchRequest::new("nebula"))
        .unwrap();
    assert_ne!(hits.hits.len(), 0);
    assert!(
        !serde_json::to_string(&hits)
            .unwrap()
            .contains("badScriptProbe")
    );
}

#[tokio::test]
async fn knowledge_conversation_requires_exact_bot_and_excludes_system_messages() {
    let _serial = crate::knowledge_runtime::test_guard().await;
    let t = setup();
    let (bot, chat) = make_chat(&t).await;
    let (other, _) = make_chat(&t).await;
    for (role, content) in [
        ("user", "chosen conversation constellation"),
        ("system", "system sentinel never index"),
    ] {
        t.state
            .storage()
            .insert_message(&storage::Message {
                id: Uuid::now_v7(),
                user_id: t.state.user_id(),
                chat_id: ChatId(chat.parse().unwrap()),
                run_id: None,
                role: role.into(),
                content: content.into(),
                created_at_ms: 1,
                exposure_untrusted: false,
                exposure_private: false,
                exposure_sources: Vec::new(),
            })
            .unwrap();
    }
    let foreign = request(
        &t,
        Method::POST,
        &format!("/v1/bots/{other}/knowledge/sources"),
        Some(json!({"kind":"conversation","label":"Foreign","chat_id":chat})),
    )
    .await;
    assert_eq!(foreign.status, StatusCode::NOT_FOUND);
    add(
        &t,
        &bot,
        json!({"kind":"conversation","label":"Selected chat","chat_id":chat}),
    )
    .await;
    assert_ne!(
        index(&t, &bot)
            .search(&SearchRequest::new("constellation"))
            .unwrap()
            .hits
            .len(),
        0
    );
    assert_eq!(
        index(&t, &bot)
            .search(&SearchRequest::new("sentinel"))
            .unwrap()
            .hits
            .len(),
        0
    );
}

#[tokio::test]
async fn knowledge_redacts_registered_values_before_persistence() {
    let _serial = crate::knowledge_runtime::test_guard().await;
    let dir = tempfile::tempdir().unwrap();
    let storage = Arc::new(Storage::open(dir.path()).unwrap());
    let mut redactor = secret_store::Redactor::new();
    let secret = "violet-copper-orchid-window";
    redactor.add_secret(secret);
    let agent = AgentLoop::new(
        storage.clone(),
        ToolRegistry::new(),
        Providers::single(Arc::new(FakeProvider)),
        LoopConfig {
            redactor: Some(redactor.into()),
            ..LoopConfig::default()
        },
    );
    let state = AppState::new(agent.clone(), storage, DaemonConfig::new(TOKEN)).unwrap();
    state.set_owner_key(OWNER_KEY.into());
    let t = T {
        app: router(state.clone()),
        state,
        agent,
        _dir: dir,
    };
    let (bot, _) = make_chat(&t).await;
    add(&t, &bot, json!({"kind":"note","label":"Credential masking","content":format!("The cobalt deployment value is {secret}.\nKeep useful architecture prose.")})).await;
    let found = index(&t, &bot)
        .search(&SearchRequest::new("cobalt"))
        .unwrap();
    assert_ne!(found.hits.len(), 0);
    assert!(!serde_json::to_string(&found).unwrap().contains(secret));
    assert_eq!(
        index(&t, &bot)
            .search(&SearchRequest::new("violet copper orchid window"))
            .unwrap()
            .hits
            .len(),
        0
    );
}

fn captured_email(t: &T, bot: &str, account: &str) -> (Uuid, Uuid) {
    let run = RunId::new();
    let call = Uuid::now_v7();
    t.state
        .storage()
        .insert_run(&storage::Run {
            id: run,
            user_id: t.state.user_id(),
            bot_id: BotId(bot.parse().unwrap()),
            chat_id: None,
            kind: "turn".into(),
            state: "completed".into(),
            error: None,
            created_at_ms: 10,
            updated_at_ms: 11,
            parent_run_id: None,
            spec: None,
            computer_target: None,
            computer_node: None,
            computer_node_name: None,
            paused_at_ms: None,
            pause_reason: None,
        })
        .unwrap();
    t.state.storage().insert_step(&storage::RunStep { id:Uuid::now_v7(),run_id:run,user_id:t.state.user_id(),idx:0,name:"tools".into(),input:json!({"calls":[{"id":"mail-call","name":"email_read","arguments":json!({"account":account,"uid":1}).to_string()}]}),idempotent:true,retry:None,status:"completed".into(),attempt:1,output:Some(json!({"results":[{"id":"mail-call","call_uuid":call,"name":"email_read","is_error":false,"text":"Authorized mailbox spectrometer evidence","spill_path":null}]})),error:None,started_at_ms:10,finished_at_ms:Some(11) }).unwrap();
    (run.0, call)
}

#[tokio::test]
async fn knowledge_email_uses_owned_capture_and_revokes_on_account_change() {
    let _serial = crate::knowledge_runtime::test_guard().await;
    let (t, _) = super::email::with_email();
    let (bot, _) = make_chat(&t).await;
    let (other, _) = make_chat(&t).await;
    let account = request(&t, Method::POST, "/v1/email/accounts", Some(json!({"name":"work","address":"reader@example.com","imap":{"host":"imap.example.com"},"smtp":{"host":"smtp.example.com","port":587,"security":"start_tls"},"password":"test-password-value","bot":bot}))).await;
    assert_eq!(account.status, StatusCode::CREATED, "{}", account.json());
    let (run, call_id) = captured_email(&t, &bot, "work");
    let captures = request(
        &t,
        Method::GET,
        &format!("/v1/bots/{bot}/knowledge/email-captures"),
        None,
    )
    .await;
    assert_eq!(captures.json().as_array().unwrap().len(), 1);
    let foreign = request(&t, Method::POST, &format!("/v1/bots/{other}/knowledge/sources"), Some(json!({"kind":"email","label":"Mailbox","account":"work","run_id":run,"tool_call_id":call_id}))).await;
    assert_eq!(foreign.status, StatusCode::NOT_FOUND);
    let source = add(&t, &bot, json!({"kind":"email","label":"Mailbox","account":"work","run_id":run,"tool_call_id":call_id})).await["source"].clone();
    assert_ne!(
        index(&t, &bot)
            .search(&SearchRequest::new("spectrometer"))
            .unwrap()
            .hits
            .len(),
        0
    );
    let rebound = request(
        &t,
        Method::PUT,
        "/v1/email/accounts/work",
        Some(json!({"bot":other})),
    )
    .await;
    assert_eq!(rebound.status, StatusCode::OK);
    assert_eq!(
        index(&t, &bot)
            .search(&SearchRequest::new("spectrometer"))
            .unwrap()
            .hits
            .len(),
        0
    );
    let current = index(&t, &bot)
        .source(source["id"].as_str().unwrap().parse().unwrap())
        .unwrap()
        .unwrap();
    let enabled = request(
        &t,
        Method::PUT,
        &format!("/v1/bots/{bot}/knowledge/sources/{}", current.id),
        Some(json!({"action":"enable","expected_revision":current.revision})),
    )
    .await;
    assert_eq!(enabled.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn knowledge_local_profile_is_host_installed_and_paths_are_not_api_inputs() {
    let _serial = crate::knowledge_runtime::test_guard().await;
    let t = setup();
    let (bot, _) = make_chat(&t).await;
    let route = format!("/v1/bots/{bot}/knowledge/settings");
    let initial = request(&t, Method::GET, &route, None).await;
    assert_eq!(initial.json()["auto_retrieve"], false);
    assert_eq!(initial.json()["auto_retrieval_available"], false);
    assert!(initial.json()["embedding"].is_null());
    assert_eq!(initial.json()["local_profiles"], json!([]));
    let artifact = json!({"path":t._dir.path().join("private-operator-selected-file"),"sha256":"a".repeat(64)});
    let artifacts = json!({"runtime":artifact,"model":artifact,"tokenizer":artifact,"config":artifact,"special_tokens_map":artifact,"tokenizer_config":artifact});
    let mut config = json!({"auto_retrieve":false,"embedding":{"provider_id":"local-onnx","model":"ignored-client-model","dimensions":999,"requests_per_minute":100,"requests_per_day":1000,"local_profile_id":"offline-e5"}});
    let mut forged = config.clone();
    forged["embedding"]["local_artifacts"] = artifacts.clone();
    assert_eq!(
        request(&t, Method::PUT, &route, Some(forged)).await.status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(&t, Method::PUT, &route, Some(config.clone()))
            .await
            .status,
        StatusCode::BAD_REQUEST,
        "an unknown profile cannot select files"
    );
    let host = t.state.data_dir().join("knowledge-host.json");
    std::fs::write(
        &host,
        json!({"profiles":{"offline-e5":{"label":"Installed E5","artifacts":artifacts}}})
            .to_string(),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&host, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let saved = request(&t, Method::PUT, &route, Some(config.clone())).await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.json());
    assert_eq!(saved.json()["embedding"]["dimensions"], 384);
    assert_eq!(saved.json()["embedding"]["model"], "multilingual-e5-small");
    assert_eq!(saved.json()["embedding"]["local_profile_id"], "offline-e5");
    assert_eq!(saved.json()["embedding"]["query_remote"], false);
    assert_eq!(saved.json()["local_profiles"][0]["label"], "Installed E5");
    assert!(
        !saved
            .json()
            .to_string()
            .contains("private-operator-selected-file")
    );
    assert!(!saved.json().to_string().contains("local_artifacts"));
    config["embedding"]["query_remote"] = true.into();
    assert_eq!(
        request(&t, Method::PUT, &route, Some(config)).await.status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(
            &t,
            Method::PUT,
            &route,
            Some(json!({"auto_retrieve":true,"embedding":null}))
        )
        .await
        .status,
        StatusCode::CONFLICT
    );
}

fn configure_remote(t: &T, endpoint: &str) {
    use provider_catalog::{
        Auth, CompatFlags, ModelCapabilities, ProviderConfig, ProviderEntry, ProviderKind,
        ProviderSettings,
    };
    let mut setting = ProviderSettings::new("fixture-embedding", 1);
    setting.custom = Some(ProviderEntry {
        id: "fixture-embedding".into(),
        kind: ProviderKind::OpenAiCompatible,
        base_url: endpoint.into(),
        auth: Auth::None,
        capabilities: ModelCapabilities {
            tools: false,
            vision: false,
            streaming: false,
            json_mode: false,
        },
        compat_flags: CompatFlags::default(),
        models_endpoint: None,
        models: Vec::new(),
    });
    let mut config = ProviderConfig::default();
    config.upsert(setting);
    config
        .save(&provider_catalog::config_path(&t.state.data_dir()))
        .unwrap();
}

#[tokio::test]
async fn knowledge_gemini_model_format_rejects_unknown_families() {
    let _serial = crate::knowledge_runtime::test_guard().await;
    let t = setup();
    let (bot, _) = make_chat(&t).await;
    configure_remote(&t, "https://embedding.example.invalid/v1beta");
    let path = provider_catalog::config_path(&t.state.data_dir());
    let mut providers = provider_catalog::ProviderConfig::load(&path).unwrap();
    let mut settings = providers.get("fixture-embedding").unwrap().clone();
    settings.custom.as_mut().unwrap().kind = provider_catalog::ProviderKind::Gemini;
    providers.upsert(settings);
    providers.save(&path).unwrap();
    let route = format!("/v1/bots/{bot}/knowledge/settings");
    for model in [
        "gemini-embedding-2-wrong-family",
        "unknown-embedding",
        "gemini-2.5-flash",
    ] {
        let response = request(&t, Method::PUT, &route, Some(json!({"auto_retrieve":false,"embedding":{"provider_id":"fixture-embedding","model":model,"dimensions":768,"requests_per_minute":100,"requests_per_day":1000}}))).await;
        assert_eq!(
            response.status,
            StatusCode::BAD_REQUEST,
            "accepted {model}: {}",
            response.json()
        );
    }
    for (model, format) in [
        ("gemini-embedding-001", "gemini001"),
        ("gemini-embedding-2", "gemini2"),
        ("gemini-embedding-2-preview", "gemini2"),
    ] {
        let response = request(&t, Method::PUT, &route, Some(json!({"auto_retrieve":false,"embedding":{"provider_id":"fixture-embedding","model":model,"dimensions":768,"requests_per_minute":100,"requests_per_day":1000}}))).await;
        assert_eq!(response.status, StatusCode::OK, "{}", response.json());
        let record = t
            .state
            .storage()
            .get_bot(BotId(bot.parse().unwrap()))
            .unwrap()
            .unwrap();
        assert_eq!(
            record.config["knowledge"]["embedding"]["profile"]["format"],
            format
        );
    }
}

#[tokio::test]
async fn knowledge_provider_aliases_share_secret_reference_quota() {
    let _serial = crate::knowledge_runtime::test_guard().await;
    let t = setup();
    let (bot, _) = make_chat(&t).await;
    configure_remote(&t, "https://embedding.example.invalid/v1");
    let path = provider_catalog::config_path(&t.state.data_dir());
    let mut providers = provider_catalog::ProviderConfig::load(&path).unwrap();
    let mut first = providers.get("fixture-embedding").unwrap().clone();
    first.key = Some(secret_store::SecretRef::new("same-project-key").unwrap());
    let mut alias = first.clone();
    alias.id = "embedding-alias".into();
    alias.custom.as_mut().unwrap().id = alias.id.clone();
    providers.upsert(first);
    providers.upsert(alias);
    providers.save(&path).unwrap();
    let mut groups = Vec::new();
    for provider in ["fixture-embedding", "embedding-alias"] {
        let response = request(&t, Method::PUT, &format!("/v1/bots/{bot}/knowledge/settings"), Some(json!({"auto_retrieve":false,"embedding":{"provider_id":provider,"model":"embed-v1","dimensions":384,"requests_per_minute":100,"requests_per_day":1000}}))).await;
        assert_eq!(response.status, StatusCode::OK, "{}", response.json());
        let record = t
            .state
            .storage()
            .get_bot(BotId(bot.parse().unwrap()))
            .unwrap()
            .unwrap();
        groups.push(record.config["knowledge"]["embedding"]["account_group"].clone());
    }
    assert_eq!(
        groups[0], groups[1],
        "aliases must debit one credential quota"
    );
    assert_ne!(
        groups[0], "same-project-key",
        "only a hashed reference is stored"
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One end-to-end owner-card scenario.
async fn knowledge_remote_grants_require_owner_cards_and_queries_are_independent() {
    let _serial = crate::knowledge_runtime::test_guard().await;
    let t = setup();
    let (bot, _) = make_chat(&t).await;
    configure_remote(&t, "https://embedding.example.invalid/v1");
    let route = format!("/v1/bots/{bot}/knowledge/settings");
    let sources = format!("/v1/bots/{bot}/knowledge/sources");
    let approvals = format!("/v1/bots/{bot}/knowledge/embedding-approvals");
    let source = add(
        &t,
        &bot,
        json!({"kind":"note","label":"Evidence","content":"Evidence about a heliosphere"}),
    )
    .await["source"]
        .clone();
    let source_route = format!("{sources}/{}", source["id"].as_str().unwrap());
    let mut input = json!({"auto_retrieve":false,"embedding":{"provider_id":"fixture-embedding","model":"embed-v1","dimensions":384,"requests_per_minute":100,"requests_per_day":1000}});
    let saved = request(&t, Method::PUT, &route, Some(input.clone())).await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.json());
    let fingerprint = saved.json()["embedding"]["profile_fingerprint"].clone();
    let shortcut = request(&t, Method::PUT, &source_route, Some(json!({"action":"grant_remote","expected_revision":source["revision"],"profile_fingerprint":fingerprint}))).await;
    assert_eq!(shortcut.status, StatusCode::BAD_REQUEST);
    let mut shortcut = input.clone();
    shortcut["embedding"]["query_remote"] = true.into();
    shortcut["embedding"]["consent_profile_fingerprint"] = fingerprint.clone();
    assert_eq!(
        request(&t, Method::PUT, &route, Some(shortcut))
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
    let card = request(
        &t,
        Method::POST,
        &approvals,
        Some(json!({"kind":"documents","source_id":source["id"]})),
    )
    .await;
    assert_eq!(card.status, StatusCode::OK, "{}", card.json());
    assert_eq!(card.json()["profile_fingerprint"], fingerprint);
    assert_eq!(card.json()["policy_decision"], "ask");
    assert!(
        request(&t, Method::GET, &sources, None).await.json()[0]["remote_profile_fingerprint"]
            .is_null()
    );
    let confirm = format!(
        "{approvals}/{}/confirm",
        card.json()["id"].as_str().unwrap()
    );
    assert_eq!(
        request(&t, Method::POST, &confirm, None).await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        request(&t, Method::POST, &confirm, None).await.status,
        StatusCode::NOT_FOUND,
        "confirmation must be single use"
    );
    assert_eq!(
        request(&t, Method::GET, &sources, None).await.json()[0]["remote_profile_fingerprint"],
        fingerprint
    );
    assert_eq!(
        request(&t, Method::GET, &route, None).await.json()["embedding"]["query_remote"],
        false
    );
    let card = request(
        &t,
        Method::POST,
        &approvals,
        Some(json!({"kind":"queries"})),
    )
    .await;
    assert_eq!(card.status, StatusCode::OK);
    let confirm = format!(
        "{approvals}/{}/confirm",
        card.json()["id"].as_str().unwrap()
    );
    assert_eq!(
        request(&t, Method::POST, &confirm, None).await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        request(&t, Method::GET, &route, None).await.json()["embedding"]["query_remote"],
        true
    );
    input["embedding"]["model"] = "embed-v2".into();
    assert_eq!(
        request(&t, Method::PUT, &route, Some(input.clone()))
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        request(&t, Method::GET, &route, None).await.json()["embedding"]["query_remote"],
        false
    );
    assert!(
        request(&t, Method::GET, &sources, None).await.json()[0]["remote_profile_fingerprint"]
            .is_null()
    );
    let record = index(&t, &bot)
        .source(source["id"].as_str().unwrap().parse().unwrap())
        .unwrap()
        .unwrap();
    assert!(
        record.remote_embedding.is_some(),
        "inert metadata preserves the old paid generation until replacement"
    );
    input["embedding"]["model"] = "embed-v1".into();
    assert_eq!(
        request(&t, Method::PUT, &route, Some(input)).await.status,
        StatusCode::OK
    );
    assert_eq!(
        request(&t, Method::GET, &route, None).await.json()["embedding"]["query_remote"],
        false,
        "restoring a profile must not resurrect host consent"
    );
    let revoked = request(
        &t,
        Method::PUT,
        &source_route,
        Some(json!({"action":"revoke_remote","expected_revision":record.revision})),
    )
    .await;
    assert_eq!(revoked.status, StatusCode::OK);
    assert!(
        index(&t, &bot)
            .source(record.id)
            .unwrap()
            .unwrap()
            .remote_embedding
            .is_none()
    );
    t.agent
        .set_global_policy(permission_engine::Policy::read_only());
    assert_eq!(
        request(
            &t,
            Method::POST,
            &approvals,
            Some(json!({"kind":"queries"}))
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn knowledge_generic_profile_save_cannot_restore_revoked_query_consent() {
    let _serial = crate::knowledge_runtime::test_guard().await;
    let t = setup();
    let (bot, _) = make_chat(&t).await;
    let settings = format!("/v1/bots/{bot}/knowledge/settings");
    assert_eq!(
        request(
            &t,
            Method::PUT,
            &settings,
            Some(json!({"auto_retrieve":false,"embedding":null}))
        )
        .await
        .status,
        StatusCode::OK
    );
    let loaded = request(&t, Method::GET, &format!("/v1/bots/{bot}"), None)
        .await
        .json();
    let mut stale = loaded["config"].clone();
    stale["knowledge"] = json!({"auto_retrieve":true,"embedding":{"query_consent":{"profile_fingerprint":"a".repeat(64)}}});
    let result = request(
        &t,
        Method::PUT,
        &format!("/v1/bots/{bot}"),
        Some(json!({"expected_version":loaded["version"],"config":stale})),
    )
    .await;
    assert_eq!(result.status, StatusCode::OK, "{}", result.json());
    assert_eq!(result.json()["config"]["knowledge"]["auto_retrieve"], false);
    assert!(result.json()["config"]["knowledge"]["embedding"].is_null());
}

#[tokio::test]
async fn knowledge_bot_deletion_removes_only_its_index_and_preserves_referenced_bot() {
    let _serial = crate::knowledge_runtime::test_guard().await;
    let t = setup();
    let created = request(
        &t,
        Method::POST,
        "/v1/bots",
        Some(json!({"profile_toml":PROFILE})),
    )
    .await;
    let bot = created.json()["id"].as_str().unwrap().to_owned();
    add(
        &t,
        &bot,
        json!({"kind":"note","label":"Note","content":"Selected knowledge"}),
    )
    .await;
    let directory = t.state.data_dir().join("bots").join(&bot);
    std::fs::write(directory.join("profile.toml"), "human-owned profile").unwrap();
    let deleted = request(&t, Method::DELETE, &format!("/v1/bots/{bot}"), None).await;
    assert_eq!(deleted.status, StatusCode::OK, "{}", deleted.json());
    assert!(!directory.join("knowledge.db").exists());
    assert_eq!(
        std::fs::read_to_string(directory.join("profile.toml")).unwrap(),
        "human-owned profile"
    );
    let (kept, kept_chat) = make_chat(&t).await;
    add(
        &t,
        &kept,
        json!({"kind":"note","label":"Keep me","content":"Retained quasar evidence"}),
    )
    .await;
    // A delete the daemon refuses (the bot has a run in progress) must leave its index alone.
    t.state
        .storage()
        .insert_run(&storage::Run {
            id: protocol::RunId::new(),
            user_id: t.state.user_id(),
            bot_id: protocol::BotId(kept.parse().unwrap()),
            chat_id: Some(protocol::ChatId(kept_chat.parse().unwrap())),
            kind: "turn".into(),
            state: "running".into(),
            error: None,
            created_at_ms: 1,
            updated_at_ms: 1,
            parent_run_id: None,
            spec: None,
            computer_target: None,
            computer_node: None,
            computer_node_name: None,
            paused_at_ms: None,
            pause_reason: None,
        })
        .unwrap();
    assert_eq!(
        request(&t, Method::DELETE, &format!("/v1/bots/{kept}"), None)
            .await
            .status,
        StatusCode::CONFLICT
    );
    assert_ne!(
        index(&t, &kept)
            .search(&SearchRequest::new("quasar"))
            .unwrap()
            .hits
            .len(),
        0
    );
}
