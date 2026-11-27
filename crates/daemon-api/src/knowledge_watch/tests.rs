use super::*;
use agent_core::{AgentLoop, LoopConfig, Providers};
use memory_store::knowledge::NewSource;
use protocol::BotId;
use std::sync::Arc;
use storage::{Bot, Storage};
use tool_registry::ToolRegistry;

const P: Params = Params {
    tick_ms: 1_000,
    files_per_minute: 1_000,
    page_pause: Duration::ZERO,
    backoff_cap_ms: 60_000,
    churn_threshold: 3,
};

struct Host {
    state: AppState,
    owner: Scope,
    folder: tempfile::TempDir,
    source: Uuid,
    _data: tempfile::TempDir,
}

impl Host {
    fn new() -> Self {
        let data = tempfile::tempdir().unwrap();
        let storage = Arc::new(Storage::open(data.path()).unwrap());
        let agent = AgentLoop::new(
            Arc::clone(&storage),
            ToolRegistry::new(),
            Providers::single(Arc::new(crate::fake::FakeProvider)),
            LoopConfig {
                legacy_tool_surface: true,
                ..LoopConfig::default()
            },
        )
        .with_subagents();
        let state = AppState::new(agent, storage, crate::DaemonConfig::new("test-token")).unwrap();
        let bot = BotId::new();
        state
            .storage()
            .insert_bot(&Bot {
                id: bot,
                user_id: state.user_id(),
                name: "Watch test".into(),
                config: serde_json::json!({"bot":{"id":"watch-test","name":"Watch test"},"models":{"executor":"fake/model"},"router":{"enabled":false}}),
                created_at_ms: 1,
            })
            .unwrap();
        let owner = Scope {
            user_id: state.user_id(),
            bot_id: bot,
        };
        let folder = tempfile::tempdir().unwrap();
        let source = KnowledgeStore::open(state.data_dir(), owner)
            .unwrap()
            .add_source(NewSource::new(
                SourceKind::Folder,
                "Notes",
                folder.path().display().to_string(),
            ))
            .unwrap()
            .id;
        Self {
            state,
            owner,
            folder,
            source,
            _data: data,
        }
    }

    fn write(&self, name: &str, text: &str) {
        std::fs::write(self.folder.path().join(name), text).unwrap();
    }

    fn indexed(&self) -> Vec<String> {
        let mut names: Vec<_> = KnowledgeStore::open(self.state.data_dir(), self.owner)
            .unwrap()
            .list_documents(self.source)
            .unwrap()
            .into_iter()
            .map(|d| d.locator.trim_end_matches("#extracted-text").to_owned())
            .collect();
        names.sort();
        names
    }

    fn status(&self) -> KnowledgeWatchStatus {
        folder_statuses(&self.state, self.owner)
            .unwrap()
            .pop()
            .unwrap()
    }
}

#[tokio::test]
async fn changes_are_debounced_then_indexed_and_deletes_are_dropped() {
    let host = Host::new();
    host.write("a.md", "alpha notes");
    tick(&host.state, P, 1_000).await;
    assert_eq!(host.indexed(), ["a.md"]);

    host.write("b.md", "beta notes");
    std::fs::remove_file(host.folder.path().join("a.md")).unwrap();
    tick(&host.state, P, 2_000).await;
    // Seen, but not settled yet: nothing re-indexed, status says it is waiting.
    assert_eq!(host.indexed(), ["a.md"]);
    assert_eq!(host.status().state, KnowledgeWatchState::Waiting);

    tick(&host.state, P, 3_000).await;
    assert_eq!(host.indexed(), ["b.md"]);
    let status = host.status();
    assert_eq!(status.state, KnowledgeWatchState::Watching);
    assert_eq!(status.indexed_files, 1);
    assert_eq!(status.last_sync_ms, Some(3_000));

    host.write("b.md", "spectrometer updated notes");
    std::fs::rename(
        host.folder.path().join("b.md"),
        host.folder.path().join("renamed.md"),
    )
    .unwrap();
    tick(&host.state, P, 4_000).await;
    tick(&host.state, P, 5_000).await;
    assert_eq!(host.indexed(), ["renamed.md"]);
    let store = KnowledgeStore::open(host.state.data_dir(), host.owner).unwrap();
    assert_eq!(
        store
            .search(&memory_store::knowledge::SearchRequest::new("spectrometer"))
            .unwrap()
            .hits
            .len(),
        1
    );
    assert_eq!(
        store
            .search(&memory_store::knowledge::SearchRequest::new("beta"))
            .unwrap()
            .hits
            .len(),
        0
    );
}

#[tokio::test]
async fn secret_looking_files_are_never_indexed() {
    let host = Host::new();
    host.write("plan.md", "visible");
    host.write(".env", "TOKEN=abc");
    host.write("credentials.json", "{}");
    tick(&host.state, P, 1_000).await;
    assert_eq!(host.indexed(), ["plan.md"]);
}

#[cfg(unix)]
#[tokio::test]
async fn symlinks_out_of_the_folder_are_not_followed() {
    let host = Host::new();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("private.md"), "outside").unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("private.md"),
        host.folder.path().join("link.md"),
    )
    .unwrap();
    host.write("own.md", "inside");
    tick(&host.state, P, 1_000).await;
    assert_eq!(host.indexed(), ["own.md"]);
}

#[tokio::test]
async fn constant_churn_backs_off_instead_of_indexing() {
    let host = Host::new();
    host.write("a.md", "v0");
    tick(&host.state, P, 1_000).await;
    for (i, now) in [2_000, 3_000, 4_000, 5_000].into_iter().enumerate() {
        host.write(
            "a.md",
            &format!("a longer version {i}{}", "x".repeat(i * 3)),
        );
        tick(&host.state, P, now).await;
    }
    assert_eq!(host.status().state, KnowledgeWatchState::BackingOff);
    // Still the first version: churn never settled.
    let store = KnowledgeStore::open(host.state.data_dir(), host.owner).unwrap();
    assert_eq!(store.list_documents(host.source).unwrap().len(), 1);
    assert!(host.state.0.knowledge_watch.sources.lock()[&host.source].next_check_ms >= 6_000);
}

#[tokio::test]
async fn the_per_minute_file_budget_spreads_a_large_folder_over_time() {
    let host = Host::new();
    for i in 0..18 {
        host.write(&format!("f{i:02}.md"), &format!("note {i}"));
    }
    let slow = Params {
        files_per_minute: 10,
        ..P
    };
    tick(&host.state, slow, 1_000).await;
    // Yield after eight files, then stop the next page exactly at the budget.
    assert_eq!(host.indexed().len(), 8);
    tick(&host.state, slow, 2_000).await;
    assert_eq!(host.indexed().len(), 10);
    assert_eq!(host.status().state, KnowledgeWatchState::Waiting);
    tick(&host.state, slow, 30_000).await;
    assert_eq!(host.indexed().len(), 10);
    tick(&host.state, slow, 62_000).await;
    assert_eq!(host.indexed().len(), 18);
}

#[tokio::test]
async fn paused_folder_is_left_alone_until_resumed() {
    let host = Host::new();
    host.write("a.md", "alpha");
    tick(&host.state, P, 1_000).await;
    host.state
        .0
        .knowledge_watch
        .set_paused(&host.state.data_dir(), host.source, true)
        .unwrap();
    host.write("b.md", "beta");
    tick(&host.state, P, 2_000).await;
    tick(&host.state, P, 3_000).await;
    assert_eq!(host.indexed(), ["a.md"]);
    assert_eq!(host.status().state, KnowledgeWatchState::Paused);
    // The pause survives a restart (new hub reads the file).
    assert!(
        WatchHub::default()
            .paused_set(&host.state.data_dir())
            .contains(&host.source)
    );
    host.state
        .0
        .knowledge_watch
        .set_paused(&host.state.data_dir(), host.source, false)
        .unwrap();
    tick(&host.state, P, 4_000).await;
    assert_eq!(host.indexed(), ["a.md", "b.md"]);
}

#[tokio::test]
async fn a_missing_folder_reports_a_plain_error_and_recovers() {
    let host = Host::new();
    host.write("a.md", "alpha");
    tick(&host.state, P, 1_000).await;
    let path = host.folder.path().to_path_buf();
    let moved = path.with_extension("moved");
    std::fs::rename(&path, &moved).unwrap();
    tick(&host.state, P, 10_000).await;
    assert_eq!(host.status().error.unwrap().code, "folder_missing");
    std::fs::rename(&moved, &path).unwrap();
    tick(&host.state, P, 100_000).await;
    assert!(host.status().error.is_none());
}

#[tokio::test]
async fn a_settled_change_waits_for_a_running_search_instead_of_cancelling_it() {
    let host = Host::new();
    host.write("a.md", "alpha notes");
    tick(&host.state, P, 1_000).await;
    assert_eq!(host.indexed(), ["a.md"]);
    host.write("b.md", "beta notes");
    tick(&host.state, P, 2_000).await;

    let (running, cancel) =
        crate::knowledge_runtime::test_begin_activity(&host.state, host.owner.bot_id);
    tick(&host.state, P, 3_000).await;
    assert!(!cancel.is_cancelled(), "the running search was cancelled");
    assert_eq!(
        host.indexed(),
        ["a.md"],
        "indexed while a search was running"
    );

    drop(running);
    tick(&host.state, P, 4_000).await;
    assert_eq!(host.indexed(), ["a.md", "b.md"]);
}

#[tokio::test]
async fn only_consented_enabled_folders_are_watched_and_attempts_share_a_budget() {
    let host = Host::new();
    let unselected = tempfile::tempdir().unwrap();
    std::fs::write(unselected.path().join("private.md"), "unselected").unwrap();
    host.write("a.md", "allowed");
    // Selectable, but not text: reading it fails, so the attempt is charged and the
    // folder reports skipped files. (An oversized file no longer gets this far: the
    // scan leaves it out and names it in the "skipped" note instead.)
    std::fs::write(host.folder.path().join("b.md"), [0xff_u8, 0xfe, 0xfd]).unwrap();
    let store = KnowledgeStore::open(host.state.data_dir(), host.owner).unwrap();
    let disabled = store
        .add_source(NewSource::new(
            SourceKind::Folder,
            "Disabled",
            unselected.path().display().to_string(),
        ))
        .unwrap();
    store
        .update_source(
            disabled.id,
            disabled.revision,
            memory_store::knowledge::SourceUpdate {
                enabled: Some(false),
                ..Default::default()
            },
        )
        .unwrap();
    let slow = Params {
        files_per_minute: 2,
        ..P
    };
    tick(&host.state, slow, 1_000).await;
    assert_eq!(host.indexed(), ["a.md"]);
    assert_eq!(host.status().error.unwrap().code, "some_files_skipped");
    assert_eq!(store.list_documents(disabled.id).unwrap(), []);
    assert_eq!(host.state.0.knowledge_watch.budget.lock().used, 2);
    host.state
        .0
        .knowledge_watch
        .set_paused(&host.state.data_dir(), host.source, false)
        .unwrap();
    host.write("c.md", "new note");
    tick(&host.state, slow, 2_000).await;
    assert_eq!(
        host.indexed(),
        ["a.md"],
        "resume cannot reset the shared budget"
    );
}

#[tokio::test]
async fn watch_routes_require_the_owner_and_enforce_bot_scope() {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;
    let host = Host::new();
    host.state
        .set_owner_key("owner-watch-key-with-at-least-32-bytes".into());
    let app = crate::router(host.state.clone());
    let base = format!("/v1/bots/{}/knowledge", host.owner.bot_id);
    for (method, path) in [
        ("GET", format!("{base}/watch")),
        ("HEAD", format!("{base}/watch")),
        ("PUT", format!("{base}/sources/{}/watch", host.source)),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(&path)
                    .header("host", "127.0.0.1:47821")
                    .header("authorization", "Bearer test-token")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"paused":true}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    for (bot, expected) in [
        (host.owner.bot_id.0, StatusCode::OK),
        (Uuid::now_v7(), StatusCode::NOT_FOUND),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!(
                        "/v1/bots/{bot}/knowledge/sources/{}/watch",
                        host.source
                    ))
                    .header("host", "127.0.0.1:47821")
                    .header("authorization", "Bearer test-token")
                    .header(
                        "x-praxi-owner-key",
                        "owner-watch-key-with-at-least-32-bytes",
                    )
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"paused":true}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }
    assert_eq!(host.status().state, KnowledgeWatchState::Paused);
}
