//! Behavioral regressions for the independent source-management review.
use super::*;

fn state_at(dir: tempfile::TempDir, path: std::path::PathBuf) -> T {
    let storage = Arc::new(Storage::open(path).unwrap());
    let agent = AgentLoop::new(
        storage.clone(),
        ToolRegistry::new(),
        Providers::single(Arc::new(FakeProvider)),
        LoopConfig::default(),
    );
    let state = AppState::new(agent.clone(), storage, DaemonConfig::new(TOKEN)).unwrap();
    state.set_owner_key(OWNER_KEY.into());
    T {
        app: router(state.clone()),
        state,
        agent,
        _dir: dir,
    }
}

async fn delete_empty_bot(t: &T) {
    let bot = BotId::new();
    t.state
        .storage()
        .insert_bot(&storage::Bot {
            id: bot,
            user_id: t.state.user_id(),
            name: "Disposable".into(),
            config: json!({}),
            created_at_ms: 1,
        })
        .unwrap();
    let response = request(t, Method::DELETE, &format!("/v1/bots/{bot}"), None).await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.json());
}

#[tokio::test]
async fn review_81_relative_data_directory_does_not_block_bot_deletion() {
    let _serial = crate::knowledge_runtime::test_guard().await;
    let dir = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let path = std::path::PathBuf::from(dir.path().file_name().unwrap()).join("state");
    assert!(path.is_relative());
    let t = state_at(dir, path);
    delete_empty_bot(&t).await;
    assert!(t.state.data_dir().is_absolute());
}

#[cfg(unix)]
#[tokio::test]
async fn review_81_symlinked_data_parent_supports_brain_and_bot_deletion() {
    let _serial = crate::knowledge_runtime::test_guard().await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("real-home")).unwrap();
    std::os::unix::fs::symlink(dir.path().join("real-home"), dir.path().join("home")).unwrap();
    let path = dir.path().join("home/state");
    let t = state_at(dir, path);
    delete_empty_bot(&t).await;
    let (bot, _) = make_chat(&t).await;
    let brain = add(&t, &bot, json!({"kind":"shared_brain","label":"Brain"})).await;
    assert_eq!(
        brain["source"]["locator"],
        t.state.data_dir().join("brain").to_string_lossy().as_ref()
    );
}

#[tokio::test]
async fn review_81_startup_removes_staging_alias_but_preserves_published_note() {
    let _serial = crate::knowledge_runtime::test_guard().await;
    let name = format!(".praxi-brain-{}.tmp", Uuid::now_v7());
    let directory_name = format!(".praxi-brain-{}.tmp", Uuid::now_v7());
    #[cfg(unix)]
    let outside = tempfile::NamedTempFile::new().unwrap();
    #[cfg(unix)]
    let symlink_name = format!(".praxi-brain-{}.tmp", Uuid::now_v7());
    let t = super::super::setup_with(|root| {
        let brain = root.join("brain");
        std::fs::create_dir(&brain).unwrap();
        std::fs::write(
            brain.join(&name),
            "# Approved decision\n\nDurable publication after crash.",
        )
        .unwrap();
        std::fs::hard_link(brain.join(&name), brain.join("decision.md")).unwrap();
        std::fs::write(
            brain.join(".praxi-brain-human.tmp"),
            "Not a generated staging filename.",
        )
        .unwrap();
        std::fs::create_dir(brain.join(&directory_name)).unwrap();
        #[cfg(unix)]
        {
            std::fs::write(outside.path(), "Outside note must survive.").unwrap();
            std::os::unix::fs::symlink(outside.path(), brain.join(&symlink_name)).unwrap();
        }
    });
    assert!(!t.state.data_dir().join("brain").join(name).exists());
    assert!(
        t.state
            .data_dir()
            .join("brain/.praxi-brain-human.tmp")
            .exists()
    );
    assert!(
        t.state
            .data_dir()
            .join("brain")
            .join(directory_name)
            .is_dir()
    );
    assert_eq!(
        std::fs::read_to_string(t.state.data_dir().join("brain/decision.md")).unwrap(),
        "# Approved decision\n\nDurable publication after crash."
    );
    #[cfg(unix)]
    {
        assert_eq!(
            std::fs::read_to_string(outside.path()).unwrap(),
            "Outside note must survive."
        );
        assert!(
            std::fs::symlink_metadata(t.state.data_dir().join("brain").join(symlink_name))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
    let (bot, _) = make_chat(&t).await;
    let brain = add(&t, &bot, json!({"kind":"shared_brain","label":"Brain"})).await;
    assert_eq!(brain["source"]["documents"], 1);
}

#[tokio::test]
async fn review_81_email_unrelated_and_failed_changes_preserve_source_grants() {
    let _serial = crate::knowledge_runtime::test_guard().await;
    let (t, control) = super::super::email::with_email();
    let (bot, _) = make_chat(&t).await;
    let (other, _) = make_chat(&t).await;
    let created = request(&t, Method::POST, "/v1/email/accounts", Some(json!({"name":"review-mail","address":"reader@example.com","imap":{"host":"imap.example.com"},"smtp":{"host":"smtp.example.com","port":587,"security":"start_tls"},"password":"fixture-password","bot":bot}))).await;
    assert_eq!(created.status, StatusCode::CREATED);
    let (run, call_id) = captured_email(&t, &bot, "review-mail");
    let source = add(&t, &bot, json!({"kind":"email","label":"Mailbox","account":"review-mail","run_id":run,"tool_call_id":call_id})).await["source"].clone();
    let id = source["id"].as_str().unwrap().parse().unwrap();
    for patch in [
        json!({"password":"rotated-fixture-password"}),
        json!({"drafts_folder":"Drafts2"}),
        json!({"bot":bot}),
    ] {
        let response = request(
            &t,
            Method::PUT,
            "/v1/email/accounts/review-mail",
            Some(patch),
        )
        .await;
        assert_eq!(response.status, StatusCode::OK);
        assert!(
            index(&t, &bot).source(id).unwrap().unwrap().enabled,
            "non-binding patch revoked the source"
        );
    }
    control
        .fail_update
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let failed = request(
        &t,
        Method::PUT,
        "/v1/email/accounts/review-mail",
        Some(json!({"bot":other})),
    )
    .await;
    assert_eq!(failed.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(index(&t, &bot).source(id).unwrap().unwrap().enabled);
    control
        .fail_update
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let failed_delete = request(
        &t,
        Method::DELETE,
        "/v1/email/accounts/review-mail?delete_inbox=true",
        None,
    )
    .await;
    assert_eq!(failed_delete.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(index(&t, &bot).source(id).unwrap().unwrap().enabled);
    let rebound = request(
        &t,
        Method::PUT,
        "/v1/email/accounts/review-mail",
        Some(json!({"bot":other})),
    )
    .await;
    assert_eq!(rebound.status, StatusCode::OK);
    assert!(!index(&t, &bot).source(id).unwrap().unwrap().enabled);
}

#[tokio::test]
async fn review_81_long_file_and_web_locators_are_not_secret_values() {
    let _serial = crate::knowledge_runtime::test_guard().await;
    let t = setup();
    let (bot, _) = make_chat(&t).await;
    let root = tempfile::tempdir().unwrap();
    let folder = root.path().join("company-handbook/engineering/runbooks");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(
        folder.join("database-failover-guide.md"),
        "# Failover\n\nReplica promotion checklist.",
    )
    .unwrap();
    let added = add(
        &t,
        &bot,
        json!({"kind":"folder","label":"Engineering","path":folder}),
    )
    .await;
    assert_eq!(added["source"]["documents"], 1);
    let page = add(&t, &bot, json!({"kind":"saved_web","label":"Rust async","url":"https://example.com/blog/2024/understanding-rust-async-await-patterns","content":"Runtime scheduling and asynchronous cancellation."})).await;
    assert_eq!(page["source"]["documents"], 1);
}

#[tokio::test]
async fn review_81_bad_folder_entries_advance_pages_and_allow_pruning() {
    let _serial = crate::knowledge_runtime::test_guard().await;
    let t = setup();
    let (bot, _) = make_chat(&t).await;
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("00-bad.txt"), [0xff, 0xfe]).unwrap();
    std::fs::write(
        root.path().join("01-bad.pdf"),
        b"encrypted or malformed PDF",
    )
    .unwrap();
    for i in 2..35 {
        std::fs::write(
            root.path().join(format!("{i:02}.md")),
            format!("Selected evidence nebula {i}"),
        )
        .unwrap();
    }
    let first = add(
        &t,
        &bot,
        json!({"kind":"folder","label":"Notes","path":root.path()}),
    )
    .await;
    assert_eq!(first["indexing"]["processed"], 30);
    assert_eq!(first["indexing"]["failures"].as_array().unwrap().len(), 2);
    assert_eq!(first["indexing"]["next_cursor"], "31.md");
    let id = first["source"]["id"].as_str().unwrap();
    let route = format!("/v1/bots/{bot}/knowledge/sources/{id}/reindex");
    let second = request(&t, Method::POST, &route, Some(json!({"expected_revision":first["source"]["revision"],"cursor":first["indexing"]["next_cursor"]}))).await;
    assert_eq!(second.status, StatusCode::OK, "{}", second.json());
    assert_eq!(second.json()["source"]["documents"], 33);
    assert!(second.json()["indexing"]["next_cursor"].is_null());
    std::fs::remove_file(root.path().join("34.md")).unwrap();
    let third = request(
        &t,
        Method::POST,
        &route,
        Some(json!({"expected_revision":second.json()["source"]["revision"],"cursor":"31.md"})),
    )
    .await;
    assert_eq!(third.status, StatusCode::OK, "{}", third.json());
    assert_eq!(third.json()["indexing"]["removed"], 1);
    assert_eq!(third.json()["source"]["documents"], 32);
}

#[tokio::test]
async fn review_81_daemon_state_and_ancestor_are_never_source_grants() {
    let _serial = crate::knowledge_runtime::test_guard().await;
    let dir = tempfile::tempdir().unwrap();
    let storage = Arc::new(Storage::open(dir.path().join("state")).unwrap());
    let agent = AgentLoop::new(
        storage.clone(),
        ToolRegistry::new(),
        Providers::single(Arc::new(FakeProvider)),
        LoopConfig::default(),
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
    std::fs::write(
        t.state.data_dir().join("private.md"),
        "Other bots private transcript.",
    )
    .unwrap();
    let base = format!("/v1/bots/{bot}/knowledge/sources");
    for path in [
        t.state.data_dir(),
        t._dir.path().to_owned(),
        t.state.data_dir().join("private.md"),
    ] {
        let kind = if path.is_dir() { "folder" } else { "file" };
        let response = request(
            &t,
            Method::POST,
            &base,
            Some(json!({"kind":kind,"label":"State","path":path})),
        )
        .await;
        assert!(
            response.status.is_client_error(),
            "state/ancestor admitted: {}",
            response.json()
        );
    }
    assert_eq!(
        index(&t, &bot).list_sources().unwrap(),
        [] as [memory_store::knowledge::Source; 0]
    );
    let brain = add(&t, &bot, json!({"kind":"shared_brain","label":"Brain"})).await;
    assert!(brain["source"]["enabled"].as_bool().unwrap());
    let as_folder = request(
        &t,
        Method::POST,
        &base,
        Some(
            json!({"kind":"folder","label":"Brain alias","path":t.state.data_dir().join("brain")}),
        ),
    )
    .await;
    assert!(
        as_folder.status.is_client_error(),
        "brain exception is explicit kind only"
    );
}

#[tokio::test]
async fn review_81_imported_file_requires_path_selection_before_enable() {
    let _serial = crate::knowledge_runtime::test_guard().await;
    let t = setup();
    let (bot, _) = make_chat(&t).await;
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("notes.md");
    std::fs::write(&path, "Selected local note.").unwrap();
    let source = add(
        &t,
        &bot,
        json!({"kind":"file","label":"Imported note","path":path}),
    )
    .await["source"]
        .clone();
    let bot_id = BotId(bot.parse().unwrap());
    KnowledgeStore::prepare_import(
        t.state
            .data_dir()
            .join("bots")
            .join(&bot)
            .join("knowledge.db"),
        Scope {
            user_id: t.state.user_id(),
            bot_id,
        },
    )
    .unwrap();
    let store = index(&t, &bot);
    let source = store
        .source(source["id"].as_str().unwrap().parse().unwrap())
        .unwrap()
        .unwrap();
    let enabled = request(
        &t,
        Method::PUT,
        &format!("/v1/bots/{bot}/knowledge/sources/{}", source.id),
        Some(json!({"action":"enable","expected_revision":source.revision})),
    )
    .await;
    assert_eq!(
        enabled.status,
        StatusCode::CONFLICT,
        "an import is not a local selection: {}",
        enabled.json()
    );
    assert!(!store.source(source.id).unwrap().unwrap().enabled);
    let rebound = request(
        &t,
        Method::PUT,
        &format!("/v1/bots/{bot}/knowledge/sources/{}", source.id),
        Some(json!({"action":"rebind","expected_revision":source.revision,"path":path})),
    )
    .await;
    assert_eq!(rebound.status, StatusCode::OK, "{}", rebound.json());
    assert_eq!(rebound.json()["id"], source.id.to_string());
    assert_eq!(rebound.json()["label"], source.label);
    assert_eq!(
        rebound.json()["documents"],
        0,
        "old imported bytes cannot survive the new binding"
    );
    assert_eq!(rebound.json()["revision"], source.revision + 1);
    assert!(rebound.json()["remote_profile_fingerprint"].is_null());
    let indexed = request(
        &t,
        Method::POST,
        &format!("/v1/bots/{bot}/knowledge/sources/{}/reindex", source.id),
        Some(json!({"expected_revision":rebound.json()["revision"]})),
    )
    .await;
    assert_eq!(indexed.status, StatusCode::OK, "{}", indexed.json());
    assert_eq!(indexed.json()["source"]["documents"], 1);
}

#[tokio::test]
async fn review_81_shared_brain_has_a_default_label() {
    let _serial = crate::knowledge_runtime::test_guard().await;
    let t = setup();
    let (bot, _) = make_chat(&t).await;
    let response = add(&t, &bot, json!({"kind":"shared_brain","label":""})).await;
    assert_eq!(response["source"]["label"], "Shared brain");
}

#[tokio::test]
async fn review_81_enable_scans_files_on_a_blocking_worker() {
    let _serial = crate::knowledge_runtime::test_guard().await;
    let t = setup();
    let (bot, _) = make_chat(&t).await;
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("source.txt"), "Selected contents.").unwrap();
    let source = add(
        &t,
        &bot,
        json!({"kind":"folder","label":"Source","path":root.path()}),
    )
    .await["source"]
        .clone();
    let route = format!(
        "/v1/bots/{bot}/knowledge/sources/{}",
        source["id"].as_str().unwrap()
    );
    let disabled = request(
        &t,
        Method::PUT,
        &route,
        Some(json!({"action":"disable","expected_revision":source["revision"]})),
    )
    .await;
    assert_eq!(disabled.status, StatusCode::OK);
    let (sender, receiver) = std::sync::mpsc::channel();
    *crate::knowledge::SOURCE_WORKER_PROBE.lock() = Some((BotId(bot.parse().unwrap()), sender));
    let async_thread = std::thread::current().id();
    let result = request(
        &t,
        Method::PUT,
        &route,
        Some(json!({"action":"enable","expected_revision":disabled.json()["revision"]})),
    )
    .await;
    *crate::knowledge::SOURCE_WORKER_PROBE.lock() = None;
    assert_eq!(result.status, StatusCode::OK, "{}", result.json());
    assert_ne!(
        receiver.try_recv().unwrap(),
        async_thread,
        "a folder walk must not occupy the current-thread HTTP executor"
    );
}

#[tokio::test]
async fn review_81_large_saved_html_reaches_bounded_source_adapter() {
    let _serial = crate::knowledge_runtime::test_guard().await;
    let t = setup();
    let (bot, _) = make_chat(&t).await;
    for padding in ["x".repeat(2 * 1024 * 1024), "\nü".repeat(1024 * 1024)] {
        let html = format!("<main><h1>Kept evidence</h1></main><!--{padding}-->");
        assert!(html.len() < 4 * 1024 * 1024);
        let response = request(&t, Method::POST, &format!("/v1/bots/{bot}/knowledge/sources"), Some(json!({"kind":"saved_web","label":"Snapshot","url":"https://example.com/page","html":true,"content":html}))).await;
        assert_eq!(
            response.status,
            StatusCode::CREATED,
            "valid decoded input with JSON escaping: {}",
            response.json()
        );
    }
    let response = request(&t, Method::POST, &format!("/v1/bots/{bot}/knowledge/sources"), Some(json!({"kind":"saved_web","label":"Oversized text","url":"https://example.com/page","content":"ordinary text ".repeat(160_000)}))).await;
    assert_eq!(
        response.status,
        StatusCode::CONFLICT,
        "extracted text limit must be explicit: {}",
        response.json()
    );
    assert!(
        response.json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("text exceeds")
    );
}

#[tokio::test]
async fn review_81_peer_roles_cannot_mutate_knowledge_authority() {
    let _serial = crate::knowledge_runtime::test_guard().await;
    let t = setup();
    let (bot, _) = make_chat(&t).await;
    let id = Uuid::now_v7();
    for role in [
        crate::PeerRole::Desktop,
        crate::PeerRole::Phone,
        crate::PeerRole::Node,
        crate::PeerRole::Host,
    ] {
        for (method, suffix) in [
            (Method::POST, "sources".to_owned()),
            (Method::PUT, format!("sources/{id}")),
            (Method::DELETE, format!("sources/{id}")),
            (Method::POST, format!("sources/{id}/reindex")),
            (Method::POST, format!("sources/{id}/embed")),
            (Method::PUT, "settings".to_owned()),
            (Method::POST, "embedding-approvals".to_owned()),
            (Method::POST, format!("embedding-approvals/{id}/confirm")),
            (Method::DELETE, "embedding-grants".to_owned()),
            (Method::POST, "embedding-cooldown/reset".to_owned()),
        ] {
            for path in [
                format!("/v1/bots/{bot}/knowledge/{suffix}"),
                format!("/v1//bots/{bot}/knowledge/{suffix}/"),
                format!("/v1/bots/{bot}/knowledge%2F{suffix}"),
            ] {
                let mut req = Request::builder()
                    .method(method.clone())
                    .uri(&path)
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap();
                req.extensions_mut().insert(crate::PeerIdentity {
                    key: "review-peer".into(),
                    role,
                });
                let response = crate::peer_router(t.state.clone())
                    .oneshot(req)
                    .await
                    .unwrap();
                assert_eq!(
                    response.status(),
                    StatusCode::FORBIDDEN,
                    "{role:?} {method} {path}"
                );
            }
        }
    }
    assert_eq!(
        index(&t, &bot).list_sources().unwrap(),
        [] as [memory_store::knowledge::Source; 0]
    );
    let local = add(
        &t,
        &bot,
        json!({"kind":"note","label":"Owner","content":"Explicit owner-selected knowledge."}),
    )
    .await;
    assert_eq!(local["source"]["documents"], 1);
}

async fn metadata_refresh_case(action: &str) {
    use secret_store::SecretStore;
    let _serial = crate::knowledge_runtime::test_guard().await;
    let dir = tempfile::tempdir().unwrap();
    let storage = Arc::new(Storage::open(dir.path()).unwrap());
    let secrets = Arc::new(secret_store::MemoryStore::new());
    let redactor = agent_core::SharedRedactor::default();
    let refresh = Arc::clone(&secrets);
    redactor.set_refresher(Arc::new(move |redactor| {
        redactor
            .refresh_from_store(Some(refresh.as_ref()), &[])
            .unwrap();
    }));
    let agent = AgentLoop::new(
        storage.clone(),
        ToolRegistry::new(),
        Providers::single(Arc::new(FakeProvider)),
        LoopConfig {
            redactor: Some(redactor),
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
    let files = tempfile::tempdir().unwrap();
    let original = files.path().join("selected.md");
    std::fs::write(&original, "Selected non-secret source.").unwrap();
    let source = add(
        &t,
        &bot,
        json!({"kind":"file","label":"Current label","path":original}),
    )
    .await["source"]
        .clone();
    let secret = "late-registered-secret";
    secrets.set("late-key", secret).unwrap();
    let next = files.path().join(format!("{secret}.md"));
    std::fs::write(&next, "Selected non-secret replacement.").unwrap();
    let result = request(&t, Method::PUT, &format!("/v1/bots/{bot}/knowledge/sources/{}", source["id"].as_str().unwrap()), Some(json!({"action":action,"expected_revision":source["revision"],"label":format!("Label {secret}"),"path":next}))).await;
    assert_eq!(
        result.status,
        StatusCode::BAD_REQUEST,
        "fresh host secret must be checked before {action}: {}",
        result.json()
    );
    let record = index(&t, &bot)
        .source(source["id"].as_str().unwrap().parse().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(record.revision, source["revision"].as_u64().unwrap());
    assert_eq!(record.label, "Current label");
    assert_eq!(record.locator, original.to_string_lossy());
}

#[tokio::test]
async fn review_81_metadata_refresh_precedes_rename() {
    metadata_refresh_case("rename").await;
}

#[tokio::test]
async fn review_81_metadata_refresh_precedes_rebind() {
    metadata_refresh_case("rebind").await;
}

#[tokio::test]
async fn review_81_owner_scope_precedes_authority_lane() {
    let _serial = crate::knowledge_runtime::test_guard().await;
    let t = setup();
    let foreign = protocol::UserId::new();
    t.state
        .storage()
        .insert_user(&storage::User {
            id: foreign,
            name: "Other owner".into(),
            created_at_ms: 1,
        })
        .unwrap();
    let bot = BotId::new();
    t.state
        .storage()
        .insert_bot(&storage::Bot {
            id: bot,
            user_id: foreign,
            name: "Private bot".into(),
            config: json!({}),
            created_at_ms: 1,
        })
        .unwrap();
    let lane = crate::knowledge_runtime::authority_lock(&t.state, bot);
    let _active = lane.lock_owned().await;
    for (method, suffix, body) in [
        (
            Method::PUT,
            "settings".into(),
            Some(json!({"auto_retrieve":false,"embedding":null})),
        ),
        (
            Method::POST,
            "embedding-approvals".into(),
            Some(json!({"kind":"queries"})),
        ),
        (
            Method::POST,
            format!("embedding-approvals/{}/confirm", Uuid::now_v7()),
            None,
        ),
        (
            Method::DELETE,
            "embedding-grants".into(),
            Some(json!({"kind":"queries"})),
        ),
        (Method::POST, "embedding-cooldown/reset".into(), None),
    ] {
        let response = call(
            &t,
            method.clone(),
            &format!("/v1/bots/{bot}/knowledge/{suffix}"),
            body,
        )
        .await;
        assert_eq!(
            response.status,
            StatusCode::NOT_FOUND,
            "foreign bot must be scoped before its work can be observed or cancelled: {method} {suffix}: {}",
            response.json()
        );
    }
    assert_eq!(
        t.state.storage().get_bot(bot).unwrap().unwrap().config,
        json!({})
    );
}
