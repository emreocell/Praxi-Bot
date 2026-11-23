use super::*;
use agent_core::{AgentLoop, LoopConfig, Providers, SharedRedactor};
use memory_store::knowledge::embedding::EmbeddingStatus;
use memory_store::knowledge::{
    DocumentFormat, DocumentInput, NewSource, RemoteEmbeddingConsent, SearchMode, Source,
    SourceKind, SourceUpdate,
};
use provider_catalog::{
    Auth, CompatFlags, EmbeddingTaskFormat, ModelCapabilities, ProviderEntry, ProviderKind,
    ProviderSettings,
};
use secret_store::SecretStore;
use serde_json::{Value, json};
use storage::{Bot, Storage};
use tool_registry::{ContentPart, ToolContext, ToolRegistry};
use wiremock::{Mock, MockServer, Request, ResponseTemplate, matchers::path};

struct Host {
    state: AppState,
    bot: BotId,
    secrets: Arc<secret_store::MemoryStore>,
    _dir: tempfile::TempDir,
}

impl Host {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(Storage::open(dir.path()).unwrap());
        let secrets = Arc::new(secret_store::MemoryStore::new());
        secrets.set("embedding-key", "first-provider-key").unwrap();
        let redactor = SharedRedactor::default();
        let refresher = Arc::clone(&secrets);
        redactor.set_refresher(Arc::new(move |redactor| {
            redactor
                .refresh_from_store(Some(refresher.as_ref()), &[])
                .unwrap();
        }));
        let agent = AgentLoop::new(
            Arc::clone(&storage),
            ToolRegistry::new(),
            Providers::single(Arc::new(crate::fake::FakeProvider)),
            LoopConfig {
                redactor: Some(redactor),
                legacy_tool_surface: true,
                ..LoopConfig::default()
            },
        )
        .with_subagents();
        let state = AppState::new(agent, storage, crate::DaemonConfig::new("test-token")).unwrap();
        state.set_secret_store(secrets.clone());
        let host = Self {
            state,
            bot: BotId::new(),
            secrets,
            _dir: dir,
        };
        host.add_bot(host.bot);
        host
    }

    fn add_bot(&self, bot: BotId) {
        self.state
            .storage()
            .insert_bot(&Bot {
                id: bot,
                user_id: self.state.user_id(),
                name: "Knowledge test".into(),
                config: json!({"bot":{"id":"knowledge-test","name":"Knowledge test"},"models":{"executor":"fake/model"},"router":{"enabled":false}}),
                created_at_ms: 1,
            })
            .unwrap();
    }

    fn store(&self, bot: BotId) -> KnowledgeStore {
        KnowledgeStore::open(
            self.state.data_dir(),
            Scope {
                user_id: self.state.user_id(),
                bot_id: bot,
            },
        )
        .unwrap()
    }

    fn source(&self, bot: BotId, content: &str) -> Source {
        let store = self.store(bot);
        let source = store
            .add_source(NewSource::new(SourceKind::Note, "Saffron", "note:saffron"))
            .unwrap();
        store
            .index_document(
                source.id,
                source.revision,
                &DocumentInput {
                    locator: "saffron.md".into(),
                    title: "Operations".into(),
                    format: DocumentFormat::Markdown,
                    content: content.into(),
                    mtime_ms: None,
                },
            )
            .unwrap();
        store.source(source.id).unwrap().unwrap()
    }

    fn save(&self, bot: BotId, config: Option<&EmbeddingConfig>) {
        let mut row = self.state.bot_of_user(bot).unwrap();
        let version = self.state.storage().bot_version(bot).unwrap().unwrap();
        row.config["knowledge"] = json!({"embedding":config});
        self.state
            .storage()
            .update_bot(bot, "Knowledge test", &row.config, version)
            .unwrap();
        self.state.refresh_profile(&row);
        invalidate_bot(&self.state, bot);
    }

    fn approve_query(&self, bot: BotId) {
        let challenge = grant_request(&self.state, bot, EmbeddingGrantTarget::Queries).unwrap();
        grant_confirm(&self.state, bot, challenge.id).unwrap();
    }

    fn provider(&self, endpoint: &str, enabled: bool) {
        let entry = ProviderEntry {
            id: "test-embedding".into(),
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
        };
        let mut providers = ProviderConfig::default();
        providers.upsert(ProviderSettings {
            id: entry.id.clone(),
            enabled,
            key: Some(secret_store::SecretRef::new("embedding-key").unwrap()),
            cli: None,
            default_model: None,
            added_at_ms: 1,
            custom: Some(entry),
            model_caps: BTreeMap::new(),
        });
        providers
            .save(&provider_catalog::config_path(&self.state.data_dir()))
            .unwrap();
    }

    fn config(&self, endpoint: &str) -> EmbeddingConfig {
        self.provider(endpoint, true);
        EmbeddingConfig::new(
            EmbeddingProfile {
                provider_id: "test-embedding".into(),
                model: "vector-v1".into(),
                model_revision: None,
                endpoint_identity: endpoint.into(),
                dimensions: 2,
                request_dimensions: true,
                format: EmbeddingTaskFormat::Plain,
            },
            "same-owner-project",
        )
    }

    fn grant(&self, bot: BotId, source: &Source, _config: &EmbeddingConfig) -> Source {
        let challenge = grant_request(
            &self.state,
            bot,
            EmbeddingGrantTarget::Documents {
                source_id: source.id,
            },
        )
        .unwrap();
        grant_confirm(&self.state, bot, challenge.id).unwrap();
        self.store(bot).source(source.id).unwrap().unwrap()
    }

    async fn search(&self, bot: BotId, query: &str) -> SearchResponse {
        self.state
            .0
            .knowledge_runtime
            .get()
            .unwrap()
            .search(bot, SearchRequest::new(query))
            .await
            .unwrap()
    }
}

fn consent(config: &EmbeddingConfig) -> RemoteEmbeddingConsent {
    RemoteEmbeddingConsent {
        profile_fingerprint: config.profile.fingerprint().unwrap(),
        granted_at_ms: 1,
    }
}

async fn server() -> MockServer {
    let server = MockServer::start().await;
    mount_vectors(&server).await;
    server
}

async fn mount_vectors(server: &MockServer) {
    Mock::given(path("/embeddings"))
        .respond_with(|request: &Request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            let data = body["input"]
                .as_array()
                .unwrap()
                .iter()
                .enumerate()
                .map(|(index, _)| json!({"index":index,"embedding":[1.0,0.0]}))
                .collect::<Vec<_>>();
            ResponseTemplate::new(200).set_body_json(json!({"model":"vector-v1","data":data}))
        })
        .mount(server)
        .await;
}

async fn ready(host: &Host, server: &MockServer) -> (EmbeddingConfig, Source) {
    let mut config = host.config(&server.uri());
    config.query_consent = Some(consent(&config));
    host.save(host.bot, Some(&config));
    let source = host.source(host.bot, "Saffron snapshots restore a database.");
    let source = host.grant(host.bot, &source, &config);
    host.approve_query(host.bot);
    let progress = embedding_batch(&host.state, host.bot, source.id, source.revision)
        .await
        .unwrap();
    assert_eq!(progress.embedded_chunks, 1);
    (config, source)
}

#[path = "admission_tests.rs"]
mod admission_tests;

#[tokio::test]
async fn installed_runtime_works_without_any_provider_and_respects_bot_scope() {
    let _serial = test_guard().await;
    let host = Host::new();
    host.source(host.bot, "Saffron snapshots restore a database.");
    let own = host.search(host.bot, "saffron").await;
    assert_eq!(own.mode, SearchMode::Lexical);
    assert_eq!(own.hits.len(), 1);
    let other = BotId::new();
    host.add_bot(other);
    assert_eq!(host.search(other, "saffron").await.hits, Vec::new());
}

#[tokio::test]
async fn authority_serializes_one_directory_without_blocking_another_host() {
    let _serial = test_guard().await;
    let first = Host::new();
    let second = Host::new();
    let gate = authority_lock(&first.state, first.bot).lock_owned().await;
    assert!(
        authority_lock(&first.state.clone(), first.bot)
            .try_lock_owned()
            .is_err()
    );
    let server = server().await;
    ready(&second, &server).await;
    assert_eq!(
        second.search(second.bot, "disaster recovery").await.mode,
        SearchMode::Hybrid
    );
    drop(gate);
    assert!(
        authority_lock(&first.state, first.bot)
            .try_lock_owned()
            .is_ok()
    );
}

#[tokio::test]
async fn source_and_query_grants_are_independent_and_live_tool_uses_hybrid_cache() {
    let _serial = test_guard().await;
    let server = server().await;
    let host = Host::new();
    let mut config = host.config(&server.uri());
    host.save(host.bot, Some(&config));
    let source = host.source(host.bot, "Saffron snapshots restore a database.");
    let progress = embedding_batch(&host.state, host.bot, source.id, source.revision)
        .await
        .unwrap();
    assert_eq!(progress.status, EmbeddingStatus::AwaitingConsent);
    assert!(server.received_requests().await.unwrap().is_empty());
    let source = host.grant(host.bot, &source, &config);
    assert!(
        embedding_batch(&host.state, host.bot, source.id, source.revision - 1)
            .await
            .is_err()
    );
    let progress = embedding_batch(&host.state, host.bot, source.id, source.revision)
        .await
        .unwrap();
    assert_eq!(progress.embedded_chunks, 1);
    assert_eq!(
        host.search(host.bot, "saffron").await.mode,
        SearchMode::Lexical
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    config.query_consent = Some(consent(&config));
    host.save(host.bot, Some(&config));
    host.approve_query(host.bot);
    let semantic = host.search(host.bot, "disaster recovery").await;
    assert_eq!(semantic.mode, SearchMode::Hybrid);
    assert_eq!(semantic.hits[0].source_id, source.id);
    assert_eq!(
        host.search(host.bot, "disaster recovery").await.mode,
        SearchMode::Hybrid
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
    config.query_consent = None;
    host.save(host.bot, Some(&config));
    revoke_grant(&host.state, host.bot, EmbeddingGrantTarget::Queries).unwrap();
    assert_eq!(
        host.search(host.bot, "disaster recovery").await.mode,
        SearchMode::Lexical
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn cached_service_resolves_rotated_secret_and_rejects_changed_endpoint_or_disabled_provider()
{
    let _serial = test_guard().await;
    let server = server().await;
    let other = MockServer::start().await;
    let host = Host::new();
    ready(&host, &server).await;
    host.secrets
        .set("embedding-key", "rotated-provider-key")
        .unwrap();
    assert_eq!(
        host.search(host.bot, "disaster recovery").await.mode,
        SearchMode::Hybrid
    );
    let requests = server.received_requests().await.unwrap();
    assert_eq!(
        requests[0].headers.get("authorization").unwrap(),
        "Bearer first-provider-key"
    );
    assert_eq!(
        requests[1].headers.get("authorization").unwrap(),
        "Bearer rotated-provider-key"
    );
    host.provider(&other.uri(), true);
    assert_eq!(
        host.search(host.bot, "alternate recovery").await.mode,
        SearchMode::LexicalDegraded
    );
    assert!(other.received_requests().await.unwrap().is_empty());
    host.provider(&server.uri(), false);
    assert_eq!(
        host.search(host.bot, "another recovery").await.mode,
        SearchMode::LexicalDegraded
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn newly_registered_secret_is_refreshed_before_document_and_query_egress() {
    let _serial = test_guard().await;
    let server = server().await;
    let host = Host::new();
    let mut config = host.config(&server.uri());
    config.query_consent = Some(consent(&config));
    host.save(host.bot, Some(&config));
    let source = host.source(host.bot, "Saffron later-only-secret snapshot.");
    let source = host.grant(host.bot, &source, &config);
    host.approve_query(host.bot);
    host.secrets.set("late-key", "later-only-secret").unwrap();
    embedding_batch(&host.state, host.bot, source.id, source.revision)
        .await
        .unwrap();
    host.search(host.bot, "where later-only-secret").await;
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    for request in requests {
        let body = String::from_utf8(request.body).unwrap();
        assert!(!body.contains("later-only-secret"));
        assert!(body.contains("[REDACTED]"));
    }
}

#[tokio::test]
async fn owner_budget_is_shared_across_bots_and_survives_service_eviction() {
    let _serial = test_guard().await;
    let server = server().await;
    let host = Host::new();
    let mut config = host.config(&server.uri());
    config.budget.requests_per_day = 1;
    let other = BotId::new();
    host.add_bot(other);
    for bot in [host.bot, other] {
        config.account_group = bot.to_string();
        host.save(bot, Some(&config));
        let source = host.source(bot, "Saffron snapshots restore a database.");
        let source = host.grant(bot, &source, &config);
        let progress = embedding_batch(&host.state, bot, source.id, source.revision)
            .await
            .unwrap();
        if bot == host.bot {
            assert_eq!(progress.embedded_chunks, 1);
            forget_bot(&host.state, bot);
        } else {
            assert_eq!(progress.status, EmbeddingStatus::Deferred);
            assert_eq!(progress.embedded_chunks, 0);
        }
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn bot_json_cannot_authorize_document_or_query_egress() {
    let _serial = test_guard().await;
    let server = server().await;
    let host = Host::new();
    let mut config = host.config(&server.uri());
    config.query_consent = Some(consent(&config));
    host.save(host.bot, Some(&config));
    let source = host.source(host.bot, "Saffron snapshots restore a database.");
    // Write the same shape an imported or generic bot/source JSON update can forge.
    let source = host
        .store(host.bot)
        .update_source(
            source.id,
            source.revision,
            SourceUpdate {
                remote_embedding: Some(Some(consent(&config))),
                ..SourceUpdate::default()
            },
        )
        .unwrap();
    let progress = embedding_batch(&host.state, host.bot, source.id, source.revision)
        .await
        .unwrap();
    assert_eq!(progress.status, EmbeddingStatus::AwaitingConsent);
    host.search(host.bot, "saffron").await;
    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "JSON fields minted egress authority"
    );
}

#[test]
fn bot_json_cannot_select_native_runtime_or_model_artifacts() {
    let artifact = json!({"path":"/tmp/attacker-selected-library.so","sha256":"0".repeat(64)});
    let payload = json!({
        "profile": {"provider_id":"local-onnx","model":"multilingual-e5-small","model_revision":null,
            "endpoint_identity":format!("local-onnx:{}", "0".repeat(64)),"dimensions":384,"request_dimensions":false,"format":"e5"},
        "account_group":"untrusted", "local_artifacts": {
            "runtime":artifact,"model":artifact,"tokenizer":artifact,"config":artifact,
            "special_tokens_map":artifact,"tokenizer_config":artifact
        }
    });
    assert!(
        serde_json::from_value::<EmbeddingConfig>(payload).is_err(),
        "bot config accepted executable paths"
    );
}

#[tokio::test]
async fn revoked_source_cannot_return_cached_vectors() {
    let _serial = test_guard().await;
    let server = server().await;
    let host = Host::new();
    let (_, source) = ready(&host, &server).await;
    assert_eq!(
        host.search(host.bot, "disaster recovery").await.hits.len(),
        1
    );
    host.store(host.bot)
        .update_source(
            source.id,
            source.revision,
            SourceUpdate {
                enabled: Some(false),
                remote_embedding: Some(None),
                ..SourceUpdate::default()
            },
        )
        .unwrap();
    assert_eq!(
        host.search(host.bot, "disaster recovery").await.hits,
        Vec::new()
    );
}

#[tokio::test]
async fn failed_jobs_require_explicit_retry_and_keep_consumed_account_quota() {
    let _serial = test_guard().await;
    let server = MockServer::start().await;
    Mock::given(path("/embeddings"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    let host = Host::new();
    let mut config = host.config(&server.uri());
    config.budget.requests_per_day = 2;
    host.save(host.bot, Some(&config));
    let source = host.source(host.bot, "Saffron snapshots restore a database.");
    let source = host.grant(host.bot, &source, &config);
    let failed = embedding_batch(&host.state, host.bot, source.id, source.revision)
        .await
        .unwrap();
    assert_eq!(failed.status, EmbeddingStatus::Failed);
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    server.reset().await;
    mount_vectors(&server).await;
    let paused = embedding_batch(&host.state, host.bot, source.id, source.revision)
        .await
        .unwrap();
    assert_eq!(paused.status, EmbeddingStatus::Failed);
    assert!(server.received_requests().await.unwrap().is_empty());
    let retried = retry_embedding_batch(&host.state, host.bot, source.id, source.revision)
        .await
        .unwrap();
    assert_eq!(retried.embedded_chunks, 1);
    let second = host.source(host.bot, "A different document exhausts the shared cap.");
    let second = host.grant(host.bot, &second, &config);
    let deferred = retry_embedding_batch(&host.state, host.bot, second.id, second.revision)
        .await
        .unwrap();
    assert_eq!(deferred.status, EmbeddingStatus::Deferred);
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}
