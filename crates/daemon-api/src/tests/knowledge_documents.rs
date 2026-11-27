//! "Ask my documents": folder preview, visible skips, citations, opening a cited
//! file, and forgetting a folder completely.
use super::*;
use tool_registry::ToolContext;

async fn get_file(t: &T, bot: &str, source: &str, locator: &str) -> Resp {
    let query: String = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("locator", locator)
        .finish();
    request(
        t,
        Method::GET,
        &format!("/v1/bots/{bot}/knowledge/sources/{source}/file?{query}"),
        None,
    )
    .await
}

fn folder_with_files() -> tempfile::TempDir {
    let folder = tempfile::tempdir().unwrap();
    std::fs::create_dir(folder.path().join("housing")).unwrap();
    std::fs::write(
        folder.path().join("housing/lease.md"),
        "# Lease\n\nThe security deposit is 1800 euro, returned within thirty days.\n",
    )
    .unwrap();
    std::fs::write(folder.path().join("budget.csv"), "item,amount\nrent,900\n").unwrap();
    std::fs::write(folder.path().join(".env"), "API_TOKEN=do-not-index").unwrap();
    std::fs::write(folder.path().join("passwords.txt"), "bank: hunter2").unwrap();
    std::fs::write(folder.path().join("photo.png"), [0_u8; 8]).unwrap();
    folder
}

#[tokio::test]
async fn folder_preview_lists_what_will_be_read_and_names_secret_files() {
    let _serial = crate::knowledge_runtime::test_guard().await;
    let t = setup();
    let (bot, _) = make_chat(&t).await;
    let folder = folder_with_files();
    let route = format!("/v1/bots/{bot}/knowledge/folder-preview");
    let preview = request(
        &t,
        Method::POST,
        &route,
        Some(json!({"path": folder.path()})),
    )
    .await;
    assert_eq!(preview.status, StatusCode::OK, "{}", preview.json());
    let preview = preview.json();
    assert_eq!(preview["file_count"], 2);
    let names: Vec<_> = preview["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["locator"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["budget.csv", "housing/lease.md"]);
    assert_eq!(preview["by_type"]["text"], 2);
    assert_eq!(preview["skipped"]["secret"], 2);
    assert_eq!(preview["skipped"]["unsupported"], 1);
    let secret: Vec<_> = preview["skipped"]["listed"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["reason"] == "secret")
        .map(|f| f["locator"].as_str().unwrap())
        .collect();
    assert!(
        secret.contains(&".env") && secret.contains(&"passwords.txt"),
        "{secret:?}"
    );
    // Nothing was stored by looking.
    assert!(
        index(&t, &bot).list_sources().unwrap().is_empty(),
        "a preview must not create a source"
    );
    // The daemon's own data and a folder around it are refused, like choosing them.
    for path in [t.state.data_dir(), t.state.data_dir().join("..")] {
        let refused = request(&t, Method::POST, &route, Some(json!({"path": path}))).await;
        assert_eq!(refused.status, StatusCode::CONFLICT, "{path:?}");
    }
    // Owner only.
    let (key, value) = ("authorization", format!("Bearer {TOKEN}"));
    let token_only = send(
        &t.app,
        Method::POST,
        &route,
        Some(json!({"path": folder.path()})),
        &[(key, value.as_str()), ("x-test-token-only", "1")],
    )
    .await;
    assert_eq!(token_only.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn folder_browser_lists_folders_only_and_greys_out_unsafe_ones() {
    let _serial = crate::knowledge_runtime::test_guard().await;
    let t = setup();
    let root = tempfile::tempdir().unwrap();
    for name in ["Documents", ".hidden", ".ssh", "Taxes"] {
        std::fs::create_dir(root.path().join(name)).unwrap();
    }
    std::fs::write(root.path().join("loose.txt"), "a file").unwrap();
    let query: String = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("path", &root.path().display().to_string())
        .finish();
    let listing = request(
        &t,
        Method::GET,
        &format!("/v1/knowledge/folders?{query}"),
        None,
    )
    .await;
    assert_eq!(listing.status, StatusCode::OK, "{}", listing.json());
    let listing = listing.json();
    let names: Vec<_> = listing["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    // Folders only, no dot-folders, no files.
    assert_eq!(names, ["Documents", "Taxes"]);
    assert!(
        listing["entries"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["selectable"] == true)
    );
    assert_eq!(listing["selectable"], true);
    // A relative path is refused; so is a missing folder.
    for bad in ["relative/path", "/definitely/not/here"] {
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("path", bad)
            .finish();
        let refused = request(
            &t,
            Method::GET,
            &format!("/v1/knowledge/folders?{query}"),
            None,
        )
        .await;
        assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{bad}");
    }
    // The daemon's own data folder is listed as not selectable.
    let parent = t.state.data_dir().parent().unwrap().to_path_buf();
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("path", &parent.display().to_string())
        .finish();
    let listing = request(
        &t,
        Method::GET,
        &format!("/v1/knowledge/folders?{query}"),
        None,
    )
    .await;
    assert_eq!(
        listing.json()["selectable"],
        false,
        "a folder around the data directory"
    );
}

#[tokio::test]
async fn adding_a_folder_skips_secrets_visibly_and_refuses_a_private_key_file() {
    let _serial = crate::knowledge_runtime::test_guard().await;
    let t = setup();
    let (bot, _) = make_chat(&t).await;
    let folder = folder_with_files();
    std::fs::write(
        folder.path().join("deploy.md"),
        "Deploy notes\n-----BEGIN OPENSSH PRIVATE KEY-----\nAAAAB3NzaC1yc2EAAAADAQABAAABAQ\n-----END OPENSSH PRIVATE KEY-----\n",
    )
    .unwrap();
    let added = add(
        &t,
        &bot,
        json!({"kind":"folder","label":"Home papers","path":folder.path()}),
    )
    .await;
    let status = &added["indexing"];
    assert_eq!(status["processed"], 2, "{status}");
    assert_eq!(status["skipped"]["secret"], 2);
    assert_eq!(
        status["failures"],
        json!([{"locator":"deploy.md","code":"secret_content"}])
    );
    // The key body never reached the index, and neither did the secret files.
    let store = index(&t, &bot);
    for probe in ["AAAAB3NzaC1yc2EAAAADAQABAAABAQ", "do-not-index", "hunter2"] {
        assert!(
            store
                .search(&SearchRequest::new(probe))
                .unwrap()
                .hits
                .is_empty(),
            "{probe}"
        );
    }
}

#[tokio::test]
async fn documents_search_cites_the_file_and_the_open_route_serves_only_indexed_files() {
    let _serial = crate::knowledge_runtime::test_guard().await;
    let t = setup();
    let (bot, _) = make_chat(&t).await;
    let (other, _) = make_chat(&t).await;
    let folder = folder_with_files();
    // A note that matches better must not leak into a documents answer.
    add(
        &t,
        &bot,
        json!({"kind":"note","label":"Scratch","content":"security deposit security deposit deposit"}),
    )
    .await;
    let added = add(
        &t,
        &bot,
        json!({"kind":"folder","label":"Home papers","path":folder.path()}),
    )
    .await;
    let source = added["source"]["id"].as_str().unwrap().to_owned();
    let tool = t
        .agent
        .tool_registry()
        .get("documents_search")
        .expect("documents_search is registered with the knowledge runtime");
    assert!(tool.visible_to(Some(&bot)));
    assert!(
        !tool.visible_to(Some(&other)),
        "a bot with no folder does not see it"
    );
    let ctx = ToolContext {
        bot_id: Some(bot.clone()),
        run_id: "run-test".into(),
        ..ToolContext::default()
    };
    let out = tool
        .call(&ctx, json!({"query":"security deposit amount"}))
        .await
        .unwrap();
    let text = match &out.content[0] {
        tool_registry::ContentPart::Text { text } => text.clone(),
        other => panic!("{other:?}"),
    };
    let body: Value = serde_json::from_str(&text).unwrap();
    let hits = body["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1, "{body}");
    let expected_path = folder.path().join("housing/lease.md").display().to_string();
    assert_eq!(hits[0]["path"], expected_path);
    let citation = hits[0]["citation"].as_str().unwrap();
    assert!(citation.starts_with("[lease.md, lines 1-"), "{citation}");
    assert!(citation.contains(&format!("(praxi-doc://{source}/housing/lease.md?lines=")));
    assert!(out.exposure.as_ref().unwrap().reads_private);

    // The citation opens the file; only indexed files of this bot's enabled source.
    let opened = get_file(&t, &bot, &source, "housing/lease.md").await;
    assert_eq!(opened.status, StatusCode::OK);
    assert!(String::from_utf8_lossy(&opened.body).contains("security deposit is 1800"));
    assert_eq!(
        opened.headers.get("x-content-type-options").unwrap(),
        "nosniff"
    );
    for refused in [
        ".env",
        "passwords.txt",
        "photo.png",
        "../outside.txt",
        "housing/../.env",
        "/etc/passwd",
        "missing.md",
    ] {
        let r = get_file(&t, &bot, &source, refused).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{refused}");
    }
    let foreign = get_file(&t, &other, &source, "housing/lease.md").await;
    assert_eq!(
        foreign.status,
        StatusCode::NOT_FOUND,
        "another bot's source"
    );
    let (key, value) = ("authorization", format!("Bearer {TOKEN}"));
    let token_only = send(
        &t.app,
        Method::GET,
        &format!("/v1/bots/{bot}/knowledge/sources/{source}/file?locator=housing%2Flease.md"),
        None,
        &[(key, value.as_str()), ("x-test-token-only", "1")],
    )
    .await;
    assert_eq!(token_only.status, StatusCode::FORBIDDEN);

    // Disabling the folder closes the file and the search.
    let revision = added["source"]["revision"].as_u64().unwrap();
    let disabled = request(
        &t,
        Method::PUT,
        &format!("/v1/bots/{bot}/knowledge/sources/{source}"),
        Some(json!({"action":"disable","expected_revision":revision})),
    )
    .await;
    assert_eq!(disabled.status, StatusCode::OK);
    assert_eq!(
        get_file(&t, &bot, &source, "housing/lease.md").await.status,
        StatusCode::NOT_FOUND
    );
    assert!(!tool.visible_to(Some(&bot)));
}

#[tokio::test]
async fn removing_a_folder_forgets_everything_it_taught_the_bot() {
    let _serial = crate::knowledge_runtime::test_guard().await;
    let t = setup();
    let (bot, _) = make_chat(&t).await;
    let folder = folder_with_files();
    let added = add(
        &t,
        &bot,
        json!({"kind":"folder","label":"Home papers","path":folder.path()}),
    )
    .await;
    let source = added["source"]["id"].as_str().unwrap().to_owned();
    assert_eq!(
        index(&t, &bot)
            .search(&SearchRequest::new("security deposit"))
            .unwrap()
            .hits
            .len(),
        1
    );
    let removed = request(
        &t,
        Method::DELETE,
        &format!("/v1/bots/{bot}/knowledge/sources/{source}"),
        Some(json!({"expected_revision": added["source"]["revision"]})),
    )
    .await;
    assert_eq!(removed.status, StatusCode::NO_CONTENT);
    assert_eq!(
        index(&t, &bot)
            .search(&SearchRequest::new("security deposit"))
            .unwrap()
            .hits,
        Vec::new()
    );
    let path = t
        .state
        .data_dir()
        .join("bots")
        .join(&bot)
        .join("knowledge.db");
    let conn = rusqlite::Connection::open(path).unwrap();
    for table in [
        "sources",
        "documents",
        "chunks",
        "chunks_fts",
        "knowledge_embeddings",
        "embedding_cache",
    ] {
        let count: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            count, 0,
            "{table} still holds rows after the folder was forgotten"
        );
    }
    // The folder on disk is untouched.
    assert!(folder.path().join("housing/lease.md").exists());
    // And the file route no longer opens anything.
    assert_eq!(
        get_file(&t, &bot, &source, "housing/lease.md").await.status,
        StatusCode::NOT_FOUND
    );
}
