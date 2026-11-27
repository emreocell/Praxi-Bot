//! `POST /v1/import/scan` and `POST /v1/import/run`: what is found, what is written, and that no
//! secret value ever leaves the daemon (response, audit rows, server config).

use std::path::PathBuf;
use std::sync::Arc;

use axum::http::{Method, StatusCode};
use secret_store::{MemoryStore, SecretStore};
use serde_json::{Value, json};

use super::mcp::{FakeMcp, with_mcp};
use super::{T, call, call_token_only};
use crate::SkillAssignments;
use crate::mcp::McpControl;

const CANARIES: [&str; 5] = [
    "CANARY-claude-desktop-key-7f3a",
    "CANARY-claude-desktop-header-91bc",
    "CANARY-claude-code-token-22de",
    "CANARY-cursor-db-pass-5aa1",
    "CANARY-codex-auth-key-0e5f",
];

fn home() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../assistant-import/tests/fixtures/home")
}

fn setup() -> (T, Arc<FakeMcp>, Arc<MemoryStore>) {
    let (t, fake) = with_mcp();
    let store = Arc::new(MemoryStore::new());
    t.state.set_secret_store(store.clone());
    t.state
        .set_import_roots(assistant_import::Roots::at_home(home()));
    std::fs::create_dir_all(t.state.data_dir().join("skills")).unwrap();
    let assignments = Arc::new(SkillAssignments::new(t.state.storage_arc()));
    let index =
        ::skills::SkillIndex::load_roots(::skills::SkillRoots::from_data_dir(&t.state.data_dir()))
            .with_assignments(Arc::clone(&assignments) as Arc<dyn ::skills::AssignmentSource>);
    t.state.set_skills(index).unwrap();
    (t, fake, store)
}

/// Every byte under `dir` (databases, logs, configs): the canary must not be in any file.
fn files_hold(dir: &std::path::Path, needle: &str) -> bool {
    std::fs::read_dir(dir).unwrap().flatten().any(|e| {
        let path = e.path();
        if path.is_dir() {
            files_hold(&path, needle)
        } else {
            std::fs::read(&path)
                .is_ok_and(|b| b.windows(needle.len()).any(|w| w == needle.as_bytes()))
        }
    })
}

async fn scan(t: &T, only: &str) -> Vec<Value> {
    let r = call(
        t,
        Method::POST,
        "/v1/import/scan",
        Some(json!({"only": only})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    r.json()["items"].as_array().unwrap().clone()
}

fn id_of(items: &[Value], kind: &str, name: &str) -> String {
    items
        .iter()
        .find(|i| i["kind"] == kind && i["name"] == name)
        .unwrap_or_else(|| panic!("no {kind} {name}: {items:#?}"))["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn scan_lists_what_was_found_and_never_a_value() {
    let (t, fake, store) = setup();
    let r = call(&t, Method::POST, "/v1/import/scan", Some(json!({}))).await;
    assert_eq!(r.status, StatusCode::OK);
    let body = String::from_utf8(r.body.clone()).unwrap();
    for canary in CANARIES {
        assert!(!body.contains(canary), "{canary} in the scan response");
    }
    let items = r.json()["items"].as_array().unwrap().clone();
    let fetch = items
        .iter()
        .find(|i| i["kind"] == "mcp_server" && i["name"] == "fetch" && i["app"] == "claude")
        .unwrap();
    assert_eq!(fetch["import"], true);
    assert_eq!(fetch["reason"], "ready_with_secrets");
    assert_eq!(fetch["secrets"][0]["name"], "mcp/fetch/env/API_KEY");
    // Scanning writes nothing: no server, no secret, no skill.
    assert_eq!(fake.list().len(), 0);
    assert!(store.get("mcp/fetch/env/API_KEY").unwrap().is_none());
    assert!(!t.state.data_dir().join("skills/tidy-notes").exists());
}

#[tokio::test]
async fn only_the_owner_may_look_or_import() {
    let (t, _, _) = setup();
    for path in ["/v1/import/scan", "/v1/import/run"] {
        let r = call_token_only(&t, Method::POST, path, Some(json!({"ids": []}))).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{path}");
    }
}

#[tokio::test]
async fn bad_input_is_refused() {
    let (t, _, _) = setup();
    for body in [
        json!({"only": "nope"}),
        json!({"only": "folder"}),
        json!({"folder": "relative/dir"}),
        json!({"folder": "/proc"}),
        json!({"surprise": 1}),
    ] {
        let r = call(&t, Method::POST, "/v1/import/scan", Some(body.clone())).await;
        assert!(r.status.is_client_error(), "{body}: {}", r.status);
    }
}

#[tokio::test]
async fn a_confirmed_secret_is_stored_and_the_server_arrives_disabled() {
    let (t, fake, store) = setup();
    let items = scan(&t, "claude").await;
    let fetch = id_of(&items, "mcp_server", "fetch");
    let r = call(
        &t,
        Method::POST,
        "/v1/import/run",
        Some(json!({
            "only": "claude",
            "ids": [fetch],
            "confirmed_secrets": ["mcp/fetch/env/API_KEY"],
        })),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    let result = &r.json()["results"][0];
    assert_eq!(result["status"], "imported", "{result}");
    assert_eq!(result["secrets_stored"], json!(["mcp/fetch/env/API_KEY"]));
    assert_eq!(
        store
            .get("mcp/fetch/env/API_KEY")
            .unwrap()
            .unwrap()
            .expose(),
        "CANARY-claude-desktop-key-7f3a"
    );
    let servers = fake.list();
    assert_eq!(servers.len(), 1);
    assert!(!servers[0].enabled, "imported servers arrive disabled");
    assert_eq!(servers[0].secret_env["API_KEY"], "mcp/fetch/env/API_KEY");
    assert!(servers[0].env.is_empty());
    // Canary: the value is in the store and nowhere else.
    let everything = format!(
        "{}{:?}{:?}",
        String::from_utf8(r.body.clone()).unwrap(),
        servers,
        t.state.storage().list_audit(t.state.user_id()).unwrap()
    );
    assert!(!everything.contains("CANARY-claude-desktop-key-7f3a"));
    assert!(
        !files_hold(&t.state.data_dir(), "CANARY-claude-desktop-key-7f3a"),
        "the value reached a file of the data directory"
    );
}

#[tokio::test]
async fn an_unconfirmed_secret_is_left_out_and_reported() {
    let (t, fake, store) = setup();
    let items = scan(&t, "claude").await;
    let remote = id_of(&items, "mcp_server", "remote");
    let r = call(
        &t,
        Method::POST,
        "/v1/import/run",
        Some(json!({"only": "claude", "ids": [remote], "confirmed_secrets": []})),
    )
    .await;
    let result = &r.json()["results"][0];
    assert_eq!(result["status"], "imported");
    assert_eq!(result["secrets_left_out"], json!(["Authorization"]));
    assert_eq!(result["secrets_stored"], json!([]));
    let servers = fake.list();
    assert!(servers[0].secret_headers.is_empty());
    assert!(store.get("mcp/remote/env/Authorization").unwrap().is_none());
    assert!(!format!("{servers:?}").contains("CANARY-claude-desktop-header-91bc"));
}

#[tokio::test]
async fn items_that_cannot_be_imported_are_skipped_with_their_reason() {
    let (t, fake, _) = setup();
    let items = scan(&t, "claude").await;
    let ids = [
        id_of(&items, "mcp_server", "loose"),
        id_of(&items, "mcp_server", "danger"),
        id_of(&items, "skill", "shady"),
    ];
    let r = call(
        &t,
        Method::POST,
        "/v1/import/run",
        Some(json!({"only": "claude", "ids": ids, "confirmed_secrets": []})),
    )
    .await;
    let results = r.json()["results"].as_array().unwrap().clone();
    assert_eq!(results.len(), 3);
    assert_eq!(results[0]["reason"], "unpinned");
    assert_eq!(results[1]["reason"], "never_list");
    assert_eq!(results[2]["reason"], "review_first");
    assert!(results.iter().all(|r| r["status"] == "skipped"));
    assert_eq!(fake.list().len(), 0);
}

#[tokio::test]
async fn an_unknown_or_repeated_id_is_reported_once() {
    let (t, _, _) = setup();
    let r = call(
        &t,
        Method::POST,
        "/v1/import/run",
        Some(json!({"ids": ["claude:mcp:ghost", "claude:mcp:ghost"]})),
    )
    .await;
    let results = r.json()["results"].as_array().unwrap().clone();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["status"], "failed");
    assert_eq!(results[0]["reason"], "not_found");
}

#[tokio::test]
async fn a_server_with_the_same_name_is_not_replaced() {
    let (t, fake, store) = setup();
    let items = scan(&t, "cursor").await;
    let name = items
        .iter()
        .find(|i| i["kind"] == "mcp_server" && i["import"] == true)
        .map(|i| i["name"].as_str().unwrap().to_owned())
        .unwrap();
    let id = id_of(&items, "mcp_server", &name);
    // The first import creates it...
    let run = || {
        call(
            &t,
            Method::POST,
            "/v1/import/run",
            Some(json!({"only": "cursor", "ids": [id], "confirmed_secrets": []})),
        )
    };
    assert_eq!(run().await.json()["results"][0]["status"], "imported");
    // ...a second import finds it taken: the plan skips it, nothing is replaced or stored.
    let again = run().await.json();
    assert_eq!(again["results"][0]["status"], "skipped", "{again}");
    assert_eq!(again["results"][0]["reason"], "already_have");
    assert_eq!(fake.list().len(), 1);
    let _ = store;
}

#[tokio::test]
async fn a_clean_skill_is_installed_through_the_hub() {
    let (t, _, _) = setup();
    let items = scan(&t, "claude").await;
    let id = id_of(&items, "skill", "tidy-notes");
    let r = call(
        &t,
        Method::POST,
        "/v1/import/run",
        Some(json!({"only": "claude", "ids": [id]})),
    )
    .await;
    assert_eq!(
        r.json()["results"][0]["status"],
        "imported",
        "{:?}",
        r.json()
    );
    assert!(
        t.state
            .data_dir()
            .join("skills/tidy-notes/SKILL.md")
            .exists()
    );
    // Scanned again, the skill is now one we have.
    let items = scan(&t, "claude").await;
    let again = items
        .iter()
        .find(|i| i["kind"] == "skill" && i["name"] == "tidy-notes")
        .unwrap();
    assert_eq!(again["reason"], "already_have");
}

#[tokio::test]
async fn instructions_come_back_as_text_for_the_bot_edit_route() {
    let (t, _, _) = setup();
    let items = scan(&t, "claude").await;
    let notes = items.iter().find(|i| i["kind"] == "instructions").unwrap();
    assert_eq!(notes["default_on"], false);
    let r = call(
        &t,
        Method::POST,
        "/v1/import/run",
        Some(json!({"only": "claude", "ids": [notes["id"]]})),
    )
    .await;
    let result = &r.json()["results"][0];
    assert_eq!(result["status"], "imported");
    assert!(result["text"].as_str().unwrap().len() > 10);
}

#[tokio::test]
async fn a_folder_is_read_only_when_named() {
    let (t, _, _) = setup();
    let folder = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../assistant-import/tests/fixtures/folder")
        .canonicalize()
        .unwrap();
    let r = call(&t, Method::POST, "/v1/import/scan", Some(json!({}))).await;
    assert!(
        r.json()["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["app"] != "folder")
    );
    let r = call(
        &t,
        Method::POST,
        "/v1/import/scan",
        Some(json!({"only": "folder", "folder": folder})),
    )
    .await;
    let items = r.json()["items"].as_array().unwrap().clone();
    assert!(
        items
            .iter()
            .any(|i| i["app"] == "folder" && i["kind"] == "skill")
    );
}

#[tokio::test]
async fn a_system_folder_is_refused_on_scan_and_run() {
    let (t, _, _) = setup();
    let base = tempfile::tempdir().unwrap();
    // `/proc`, `/sys` and `/dev` are system folders on Unix; on Windows they are not absolute
    // paths at all and are refused for that reason, so the list is Unix-only.
    #[allow(unused_mut)]
    let mut folders: Vec<String> = Vec::new();
    #[cfg(unix)]
    {
        folders.extend(["/proc", "/proc/self", "/sys", "/dev"].map(str::to_owned));
        std::os::unix::fs::symlink("/proc", base.path().join("to-proc")).unwrap();
        folders.push(base.path().join("to-proc").display().to_string());
    }
    for folder in &folders {
        for (path, body) in [
            (
                "/v1/import/scan",
                json!({"only": "folder", "folder": folder}),
            ),
            (
                "/v1/import/run",
                json!({"only": "folder", "folder": folder, "ids": []}),
            ),
        ] {
            let r = call(&t, Method::POST, path, Some(body)).await;
            assert_eq!(r.status, StatusCode::BAD_REQUEST, "{path} {folder}");
            assert!(String::from_utf8_lossy(&r.body).contains("system folders"));
        }
    }
    // Neither a missing path, a file nor a path with `..` into nothing is read.
    for folder in [
        base.path().join("nope"),
        PathBuf::from("/etc/hostname"),
        base.path().join("a/../../../../nope"),
    ] {
        let r = call(
            &t,
            Method::POST,
            "/v1/import/scan",
            Some(json!({"only": "folder", "folder": folder})),
        )
        .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{folder:?}");
    }
}

#[tokio::test]
async fn a_folder_outside_the_home_folder_is_the_owners_choice() {
    let (t, _, _) = setup();
    let folder = tempfile::tempdir().unwrap();
    std::fs::write(
        folder.path().join("prompt.md"),
        "# Prompt\nWrite a short summary.",
    )
    .unwrap();
    let dotted = folder.path().join("sub/..");
    std::fs::create_dir_all(folder.path().join("sub")).unwrap();
    let r = call(
        &t,
        Method::POST,
        "/v1/import/scan",
        Some(json!({"only": "folder", "folder": dotted})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    let items = r.json()["items"].as_array().unwrap().clone();
    assert!(
        items
            .iter()
            .any(|i| i["name"] == "prompt" && i["import"] == true)
    );
}

#[cfg(unix)]
#[tokio::test]
async fn nothing_outside_the_chosen_folder_is_read() {
    let (t, _, _) = setup();
    let folder = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(
        outside.path().join("secret.md"),
        "# S\nCANARY-route-outside-77ab",
    )
    .unwrap();
    std::os::unix::fs::symlink(outside.path().join("secret.md"), folder.path().join("a.md"))
        .unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("secret.md"),
        folder.path().join("AGENTS.md"),
    )
    .unwrap();
    std::os::unix::fs::symlink(outside.path(), folder.path().join("dir")).unwrap();
    let r = call(
        &t,
        Method::POST,
        "/v1/import/scan",
        Some(json!({"only": "folder", "folder": folder.path()})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["items"], json!([]));
    assert!(!String::from_utf8_lossy(&r.body).contains("CANARY-route-outside"));
}

#[tokio::test]
async fn the_scan_shows_the_size_of_instructions_never_their_text() {
    let (t, _, _) = setup();
    let notes = std::fs::read_to_string(home().join(".claude/CLAUDE.md")).unwrap();
    let r = call(
        &t,
        Method::POST,
        "/v1/import/scan",
        Some(json!({"only": "claude"})),
    )
    .await;
    let body = String::from_utf8(r.body.clone()).unwrap();
    for line in notes.lines().filter(|l| l.trim().len() > 8) {
        assert!(!body.contains(line.trim()), "{line:?} is in the scan");
    }
    let items = r.json()["items"].as_array().unwrap().clone();
    let item = items.iter().find(|i| i["kind"] == "instructions").unwrap();
    assert!(item["detail"].as_str().unwrap().contains("lines"), "{item}");
    // The text comes back from the run, after the owner ticked it.
    let r = call(
        &t,
        Method::POST,
        "/v1/import/run",
        Some(json!({"only": "claude", "ids": [item["id"]]})),
    )
    .await;
    assert!(
        r.json()["results"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Answer briefly")
    );
}

#[tokio::test]
async fn the_scan_shows_the_whole_command_line_and_url() {
    let (t, _, _) = setup();
    let items = scan(&t, "claude").await;
    let detail = |name: &str| {
        items
            .iter()
            .find(|i| i["kind"] == "mcp_server" && i["name"] == name)
            .unwrap()["detail"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    assert_eq!(detail("fetch"), "uvx mcp-server-fetch==0.6.2");
    assert_eq!(detail("remote"), "https://mcp.example.com/mcp");
}

#[tokio::test]
async fn a_confirmed_secret_name_is_checked_and_an_unknown_one_refused() {
    let (t, fake, store) = setup();
    let items = scan(&t, "claude").await;
    let fetch = id_of(&items, "mcp_server", "fetch");
    let long = format!("mcp/fetch/env/{}", "A".repeat(300));
    for bad in [
        String::new(),
        "has space".to_owned(),
        "__reserved".to_owned(),
        "mcp/fetch/env/API_KEY\n".to_owned(),
        "ünï".to_owned(),
        long,
        // Well formed, but no server of this scan has it.
        "mcp/other/env/TOKEN".to_owned(),
        "email/account/password".to_owned(),
    ] {
        let r = call(
            &t,
            Method::POST,
            "/v1/import/run",
            Some(json!({
                "only": "claude",
                "ids": [fetch],
                "confirmed_secrets": ["mcp/fetch/env/API_KEY", bad],
            })),
        )
        .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad:?}: {:?}", r.json());
    }
    // Refused before anything was written.
    assert_eq!(fake.list().len(), 0);
    assert!(store.get("mcp/fetch/env/API_KEY").unwrap().is_none());
    // The known name still works.
    let r = call(
        &t,
        Method::POST,
        "/v1/import/run",
        Some(json!({"only": "claude", "ids": [fetch], "confirmed_secrets": ["mcp/fetch/env/API_KEY"]})),
    )
    .await;
    assert_eq!(r.json()["results"][0]["status"], "imported");
}
