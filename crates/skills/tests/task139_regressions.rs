//! Behavioral ARCH-02 regressions using only the pre-task public API.
//! This suite fails on the pre-139 main and passes on the scoped implementation.

use serde_json::{Value, json};
use skills::{SKILL_FILE, SkillIndex, SkillPaths, tools};
use std::fs;
use std::sync::Arc;
use tempfile::TempDir;
use tool_registry::{ContentPart, Effect, PermissionCategory, Tool, ToolContext, ToolOutput};

fn content(name: &str, body: &str) -> String {
    format!("---\nname: {name}\ndescription: Description for {name}\n---\n{body}\n")
}

fn context(bot: Option<&str>) -> ToolContext {
    ToolContext {
        bot_id: bot.map(str::to_owned),
        ..ToolContext::default()
    }
}

fn tool(index: &SkillIndex, name: &str) -> Arc<dyn Tool> {
    tools(index)
        .into_iter()
        .find(|t| t.spec().name == name)
        .unwrap()
}

fn data(output: &ToolOutput) -> &Value {
    output
        .content
        .iter()
        .find_map(|part| match part {
            ContentPart::Json { data } => Some(data),
            _ => None,
        })
        .unwrap()
}

fn put(root: &std::path::Path, name: &str, body: &str) {
    fs::create_dir_all(root.join(name)).unwrap();
    fs::write(root.join(name).join(SKILL_FILE), content(name, body)).unwrap();
}

#[tokio::test]
async fn legacy_tool_index_isolates_calling_bots() {
    let tmp = TempDir::new().unwrap();
    // Existing API: this behavioral test also compiles on pre-139 code, where
    // bot B incorrectly reads and overwrites A's bound directory.
    let index = SkillIndex::load(SkillPaths::from_data_dir(tmp.path(), Some("a")));
    let write = tool(&index, "skill_write");
    let read = tool(&index, "skill_read");
    write
        .call(
            &context(Some("a")),
            json!({"name":"x","content":content("x", "private A")}),
        )
        .await
        .unwrap();
    assert!(
        read.call(&context(Some("b")), json!({"name":"x"}))
            .await
            .is_err()
    );
    let listed = tool(&index, "skill_list")
        .call(&context(Some("b")), json!({}))
        .await
        .unwrap();
    assert!(
        !data(&listed)
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["name"] == "x")
    );
    write
        .call(
            &context(Some("b")),
            json!({"name":"x","content":content("x", "private B")}),
        )
        .await
        .unwrap();
    assert_eq!(
        SkillIndex::load(SkillPaths::from_data_dir(tmp.path(), Some("a")))
            .read("x", None)
            .unwrap(),
        "private A"
    );
    assert_eq!(
        SkillIndex::load(SkillPaths::from_data_dir(tmp.path(), Some("b")))
            .read("x", None)
            .unwrap(),
        "private B"
    );
    assert!(!tmp.path().join("skills/x/SKILL.md").exists());
}

#[tokio::test]
async fn botless_tools_cannot_read_a_bound_bot_or_write_shared() {
    let tmp = TempDir::new().unwrap();
    let index = SkillIndex::load(SkillPaths::from_data_dir(tmp.path(), Some("a")));
    index
        .write("private", &content("private", "private body"))
        .unwrap();
    assert!(
        tool(&index, "skill_read")
            .call(&context(None), json!({"name":"private"}))
            .await
            .is_err()
    );
    let error = tool(&index, "skill_write")
        .call(
            &context(None),
            json!({"name":"x","content":content("x", "body")}),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("bot_id"), "{error}");
    assert!(!tmp.path().join("skills/x").exists());
}

#[tokio::test]
async fn invalid_bot_ids_are_refused_by_every_operation() {
    let tmp = TempDir::new().unwrap();
    let index = SkillIndex::load(SkillPaths::from_data_dir(tmp.path(), Some("a")));
    let registered = tools(&index);
    for id in [
        "", "../evil", "a/b", "a\\b", "C:evil", "A", "a.", "con", "nul", "com1",
    ] {
        for operation in &registered {
            let result = operation
                .call(
                    &context(Some(id)),
                    json!({"name":"x","content":content("x", "body")}),
                )
                .await;
            assert!(result.is_err(), "{} accepted {id:?}", operation.spec().name);
        }
    }
    assert!(!tmp.path().join("bots").exists());
}

#[test]
fn stale_indexes_cannot_overwrite_a_newer_version() {
    let tmp = TempDir::new().unwrap();
    let paths = SkillPaths::from_data_dir(tmp.path(), Some("a"));
    let first = SkillIndex::load(paths.clone());
    first.write("x", &content("x", "original")).unwrap();
    let stale = SkillIndex::load(paths);
    first.write("x", &content("x", "winner")).unwrap();
    let error = stale.write("x", &content("x", "lost update")).unwrap_err();
    assert!(error.contains("version conflict"), "{error}");
    assert_eq!(first.get("x").unwrap().version, "0.1.1");
    assert_eq!(first.read("x", None).unwrap(), "winner");
}

#[tokio::test]
async fn publication_is_explicit_requires_bot_context_and_uses_write_approval() {
    let tmp = TempDir::new().unwrap();
    put(&tmp.path().join("skills"), "shared-only", "shared");
    let index = SkillIndex::load(SkillPaths::from_data_dir(tmp.path(), Some("a")));
    let publish = tool(&index, "skill_publish");
    assert_eq!(publish.spec().effect, Effect::Write);
    assert_eq!(publish.spec().category, PermissionCategory::Fs);
    assert!(
        publish
            .call(&context(None), json!({"name":"shared-only"}))
            .await
            .unwrap_err()
            .to_string()
            .contains("bot_id")
    );
    assert!(
        publish
            .call(&context(Some("a")), json!({"name":"shared-only"}))
            .await
            .is_err()
    );
    tool(&index, "skill_write")
        .call(
            &context(Some("a")),
            json!({"name":"x","content":content("x", "own")}),
        )
        .await
        .unwrap();
    assert!(
        SkillIndex::load(SkillPaths::from_data_dir(tmp.path(), Some("b")))
            .get("x")
            .is_none()
    );
    assert!(
        publish
            .call(&context(Some("a")), json!({"name":"x","replace":"true"}))
            .await
            .is_err()
    );
    publish
        .call(&context(Some("a")), json!({"name":"x"}))
        .await
        .unwrap();
    assert_eq!(
        SkillIndex::load(SkillPaths::from_data_dir(tmp.path(), Some("b")))
            .read("x", None)
            .unwrap(),
        "own"
    );
}

#[tokio::test]
async fn shared_listing_exposes_the_shared_scope() {
    let tmp = TempDir::new().unwrap();
    put(&tmp.path().join("skills"), "x", "shared");
    let index = SkillIndex::load(SkillPaths::from_data_dir(tmp.path(), None));
    let output = tool(&index, "skill_list")
        .call(&context(Some("a")), json!({}))
        .await
        .unwrap();
    let row = data(&output)
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "x")
        .unwrap();
    assert_eq!(row["scope"], "shared");
}

#[tokio::test]
async fn legacy_unbound_index_resolves_each_callers_bot_directory() {
    let tmp = TempDir::new().unwrap();
    let index = SkillIndex::load(SkillPaths::from_data_dir(tmp.path(), None));
    tool(&index, "skill_write")
        .call(
            &context(Some("a")),
            json!({"name":"x","content":content("x", "private")}),
        )
        .await
        .unwrap();
    assert!(
        tool(&index, "skill_read")
            .call(&context(Some("b")), json!({"name":"x"}))
            .await
            .is_err()
    );
    assert!(
        index.get("x").is_none(),
        "the unbound root index must not gain a private scope"
    );
    assert!(tmp.path().join("bots/a/skills/x/SKILL.md").exists());
    assert!(!tmp.path().join("skills/x/SKILL.md").exists());
}

#[tokio::test]
async fn arbitrary_fixed_paths_do_not_authorize_a_callers_write_scope() {
    let tmp = TempDir::new().unwrap();
    let index = SkillIndex::load(SkillPaths {
        user: Some(tmp.path().join("custom-shared")),
        bot: Some(tmp.path().join("custom-private")),
    });
    let result = tool(&index, "skill_write")
        .call(
            &context(Some("a")),
            json!({"name":"x","content":content("x", "body")}),
        )
        .await;
    assert!(result.is_err());
    assert!(!tmp.path().join("custom-private/x/SKILL.md").exists());
    let index = SkillIndex::load(SkillPaths {
        user: Some(tmp.path().join("custom-shared")),
        bot: None,
    });
    assert!(
        tool(&index, "skill_write")
            .call(
                &context(Some("a")),
                json!({"name":"x","content":content("x", "body")})
            )
            .await
            .is_err()
    );
    assert!(!tmp.path().join("bots").exists());
}
