use std::fs;
use std::path::Path;

use serde_json::json;
use tempfile::TempDir;
use tool_registry::ToolContext;

use crate::*;

fn skill_text(name: &str, desc: &str, extra_front: &str, body: &str) -> String {
    format!("---\nname: {name}\ndescription: {desc}\n{extra_front}---\n{body}\n")
}

fn put(root: &Path, dir: &str, text: &str) {
    let d = root.join(dir);
    fs::create_dir_all(&d).unwrap();
    fs::write(d.join("SKILL.md"), text).unwrap();
}

fn paths(tmp: &TempDir) -> SkillPaths {
    SkillPaths {
        user: Some(tmp.path().join("skills")),
        bot: Some(tmp.path().join("bots").join("b1").join("skills")),
    }
}

// ---- parsing ----

#[test]
fn parses_full_frontmatter() {
    let text = "---\nname: my-skill\ndescription: \"Does things\"\nversion: 1.2.0\nwhen:\n  domains: [GitHub.com, 'example.org']\n  apps: [Code.exe]\n  keywords: [Upload, file]\n---\n# Title\nBody here.\n";
    let s = parse_skill(text, Source::User, None).unwrap();
    assert_eq!(s.name, "my-skill");
    assert_eq!(s.description, "Does things");
    assert_eq!(s.version, "1.2.0");
    assert_eq!(s.when.domains, ["github.com", "example.org"]);
    assert_eq!(s.when.apps, ["Code.exe"]);
    assert_eq!(s.when.keywords, ["upload", "file"]);
    assert_eq!(s.body, "# Title\nBody here.");
}

#[test]
fn parses_crlf_and_defaults_version() {
    let text = "---\r\nname: a\r\ndescription: d\r\n---\r\nbody\r\n";
    let s = parse_skill(text, Source::User, None).unwrap();
    assert_eq!(s.version, "0.1.0");
    assert!(s.when.is_empty());
    assert_eq!(s.body, "body");
}

#[test]
fn validation_errors() {
    let bad = |text: &str| parse_skill(text, Source::User, None).unwrap_err();
    assert!(bad("no frontmatter").contains("frontmatter"));
    assert!(bad("---\nname: a\ndescription: d\nbody").contains("unterminated"));
    assert!(bad("---\ndescription: d\n---\nb").contains("`name`"));
    assert!(bad("---\nname: a\n---\nb").contains("`description`"));
    assert!(bad("---\nname: Bad Name\ndescription: d\n---\nb").contains("name"));
    assert!(
        bad("---\nname: a\ndescription: d\nfoo: 1\n---\nb").contains("unknown frontmatter key")
    );
    assert!(bad("---\nname: a\ndescription: d\nname: b\n---\nb").contains("duplicate"));
    assert!(bad("---\nname: a\ndescription: d\nwhen:\n  domains: x.com\n---\nb").contains("list"));
    assert!(
        bad("---\nname: a\ndescription: d\nwhen:\n  colour: [x]\n---\nb")
            .contains("unknown `when`")
    );
    assert!(bad("---\nname: a\ndescription: d\n---\n\n").contains("body is empty"));
    let long = "x".repeat(MAX_DESCRIPTION_LEN + 1);
    assert!(bad(&skill_text("a", &long, "", "b")).contains("longer"));
    let huge = skill_text("a", "d", "", &"x".repeat(MAX_SKILL_BYTES));
    assert!(bad(&huge).contains("larger"));
}

#[test]
fn name_rules() {
    for ok in ["a", "github.com", "my_skill-2"] {
        assert!(validate_name(ok).is_ok(), "{ok}");
    }
    for bad in [
        "", "-a", ".a", "a..b", "a.", "A", "a b", "a/b", "a\\b", "../x",
    ] {
        assert!(validate_name(bad).is_err(), "{bad}");
    }
}

// ---- loading / precedence ----

#[test]
fn bundled_skills_all_valid() {
    let tmp = TempDir::new().unwrap();
    let idx = SkillIndex::load(SkillPaths::default());
    assert!(idx.issues().is_empty(), "{:?}", idx.issues());
    let names: Vec<_> = idx.list().into_iter().map(|s| s.name).collect();
    assert_eq!(names.len(), 7);
    assert!(names.contains(&"github-basics".to_string()));
    drop(tmp);
}

#[test]
fn precedence_bundled_user_bot() {
    let tmp = TempDir::new().unwrap();
    let p = paths(&tmp);
    put(
        p.user.as_ref().unwrap(),
        "github-basics",
        &skill_text("github-basics", "user version", "", "u"),
    );
    put(
        p.user.as_ref().unwrap(),
        "only-user",
        &skill_text("only-user", "u", "", "u"),
    );
    put(
        p.bot.as_ref().unwrap(),
        "github-basics",
        &skill_text("github-basics", "bot version", "", "b"),
    );
    put(
        p.bot.as_ref().unwrap(),
        "only-user",
        &skill_text("only-user", "bot wins", "", "b"),
    );
    let idx = SkillIndex::load(p);
    let gh = idx.get("github-basics").unwrap();
    assert_eq!(
        (gh.source, gh.description.as_str()),
        (Source::Bot, "bot version")
    );
    assert_eq!(idx.get("only-user").unwrap().description, "bot wins");
    assert_eq!(idx.get("date-pickers").unwrap().source, Source::Bundled);
}

#[test]
fn user_overrides_bundled_without_bot() {
    let tmp = TempDir::new().unwrap();
    let p = paths(&tmp);
    put(
        p.user.as_ref().unwrap(),
        "x",
        &skill_text("custom-dropdowns", "mine", "", "u"),
    );
    let idx = SkillIndex::load(p);
    assert_eq!(idx.get("custom-dropdowns").unwrap().source, Source::User);
}

#[test]
fn invalid_skills_are_reported_not_fatal() {
    let tmp = TempDir::new().unwrap();
    let p = paths(&tmp);
    let user = p.user.clone().unwrap();
    put(&user, "broken", "garbage");
    put(&user, "good", &skill_text("good", "ok", "", "b"));
    fs::create_dir_all(user.join("empty-dir")).unwrap();
    fs::write(user.join("stray.txt"), "x").unwrap();
    let idx = SkillIndex::load(p);
    assert!(idx.get("good").is_some());
    let issues = idx.issues();
    assert_eq!(issues.len(), 2, "{issues:?}");
    assert!(issues.iter().all(|i| i.source == Source::User));
}

// ---- prompt block ----

#[test]
fn prompt_block_lists_and_prioritizes_suggested() {
    let idx = SkillIndex::load(SkillPaths::default());
    let ctx = SkillContext {
        domain: Some("gist.github.com".into()),
        ..SkillContext::default()
    };
    let block = idx.prompt_block(&ctx);
    let first = block.lines().nth(1).unwrap();
    assert!(first.starts_with("- [suggested] github-basics:"), "{first}");
    assert!(block.len() <= PROMPT_BUDGET_BYTES);
    assert!(block.contains("custom-dropdowns"));
}

#[test]
fn prompt_block_truncates_to_budget() {
    let tmp = TempDir::new().unwrap();
    let p = paths(&tmp);
    let user = p.user.clone().unwrap();
    let desc = "d".repeat(150);
    for i in 0..40 {
        let n = format!("extra-{i:02}");
        put(&user, &n, &skill_text(&n, &desc, "", "b"));
    }
    let idx = SkillIndex::load(p);
    let block = idx.prompt_block(&SkillContext::default());
    assert!(block.len() <= PROMPT_BUDGET_BYTES, "{}", block.len());
    assert!(block.contains("more (use skill_list)"), "{block}");
    // a suggested skill stays visible even when the budget is exhausted
    let ctx = SkillContext {
        app: Some("SystemSettings.exe".into()),
        ..SkillContext::default()
    };
    let block = idx.prompt_block(&ctx);
    assert!(block.contains("[suggested] windows-settings-navigation"));
    assert!(block.len() <= PROMPT_BUDGET_BYTES);
}

// ---- matching ----

#[test]
fn domain_app_keyword_matching() {
    let s = parse_skill(
        &skill_text(
            "m",
            "d",
            "when:\n  domains: [github.com]\n  apps: [Notepad]\n  keywords: [upload]\n",
            "b",
        ),
        Source::User,
        None,
    )
    .unwrap();
    let dom = |d: &str| SkillContext {
        domain: Some(d.into()),
        ..SkillContext::default()
    };
    assert!(s.matches(&dom("github.com")));
    assert!(s.matches(&dom("GitHub.com.")));
    assert!(s.matches(&dom("gist.github.com")));
    assert!(!s.matches(&dom("notgithub.com")));
    assert!(!s.matches(&dom("github.com.evil.io")));
    let app = |a: &str| SkillContext {
        app: Some(a.into()),
        ..SkillContext::default()
    };
    assert!(s.matches(&app("notepad.exe")));
    assert!(!s.matches(&app("notepad++")));
    let txt = SkillContext {
        text: Some("Please UPLOAD the file".into()),
        ..SkillContext::default()
    };
    assert!(s.matches(&txt));
    assert!(!s.matches(&SkillContext::default()));
}

// ---- reading files / traversal ----

fn index_with_extra() -> (TempDir, SkillIndex) {
    let tmp = TempDir::new().unwrap();
    let p = paths(&tmp);
    let user = p.user.clone().unwrap();
    put(
        &user,
        "withfiles",
        &skill_text("withfiles", "d", "", "main body"),
    );
    fs::create_dir_all(user.join("withfiles/refs")).unwrap();
    fs::write(user.join("withfiles/refs/api.md"), "api notes").unwrap();
    fs::write(user.join("secret.txt"), "top secret").unwrap();
    (tmp, SkillIndex::load(p))
}

#[test]
fn reads_body_and_extra_files() {
    let (_t, idx) = index_with_extra();
    assert_eq!(idx.read("withfiles", None).unwrap(), "main body");
    assert_eq!(
        idx.read("withfiles", Some("refs/api.md")).unwrap(),
        "api notes"
    );
    assert_eq!(
        idx.read("withfiles", Some("./refs/api.md")).unwrap(),
        "api notes"
    );
    assert_eq!(
        idx.read("withfiles", Some("SKILL.md")).unwrap(),
        "main body"
    );
    assert_eq!(idx.extra_files("withfiles"), ["refs/api.md"]);
    assert!(idx.read("withfiles", Some("nope.md")).is_err());
    assert!(idx.read("missing", None).is_err());
    assert!(
        idx.read("github-basics", Some("x.md"))
            .unwrap_err()
            .contains("bundled")
    );
}

#[test]
fn read_blocks_path_traversal() {
    let (_t, idx) = index_with_extra();
    for bad in [
        "../secret.txt",
        "refs/../../secret.txt",
        "..",
        "/etc/passwd",
        "C:\\Windows\\win.ini",
        "..\\secret.txt",
        "",
    ] {
        assert!(idx.read("withfiles", Some(bad)).is_err(), "{bad}");
    }
}

#[test]
fn write_blocks_traversal_and_mismatch() {
    let tmp = TempDir::new().unwrap();
    let idx = SkillIndex::load(paths(&tmp));
    let good = skill_text("n", "d", "", "b");
    for bad in ["../evil", "a/b", "a\\b", "..", "/abs", "C:evil"] {
        assert!(
            idx.write(bad, &skill_text(bad, "d", "", "b")).is_err(),
            "{bad}"
        );
        assert!(idx.write(bad, &good).is_err(), "{bad}");
    }
    assert!(
        idx.write("other", &good)
            .unwrap_err()
            .contains("does not match")
    );
    assert!(idx.write("n", "not a skill").is_err());
    assert!(!tmp.path().join("evil").exists());
    assert!(!tmp.path().join("bots/b1/skills/other").exists());
}

#[test]
fn write_without_bot_dir_fails() {
    let idx = SkillIndex::load(SkillPaths::default());
    assert!(
        idx.write("n", &skill_text("n", "d", "", "b"))
            .unwrap_err()
            .contains("per-bot")
    );
}

// ---- tools ----

#[tokio::test]
async fn tools_roundtrip() {
    let tmp = TempDir::new().unwrap();
    let idx = SkillIndex::load(paths(&tmp));
    let tools = tools(&idx);
    assert_eq!(tools.len(), 3);
    let find = |n: &str| tools.iter().find(|t| t.spec().name == n).unwrap().clone();
    for t in &tools {
        assert!(t.spec().is_valid());
    }
    assert_eq!(
        find("skill_write").spec().effect,
        tool_registry::Effect::Write
    );
    assert_eq!(
        find("skill_read").spec().effect,
        tool_registry::Effect::Read
    );
    assert_eq!(
        find("skill_list").spec().category,
        tool_registry::PermissionCategory::Meta
    );
    assert_ne!(
        find("skill_write").spec().category,
        tool_registry::PermissionCategory::Meta
    );

    let ctx = ToolContext::default();
    let content = skill_text("learned", "Learned thing", "", "Step 1");
    find("skill_write")
        .call(&ctx, json!({"name":"learned","content":content}))
        .await
        .unwrap();
    // visible through the shared index and the read tool
    assert!(idx.get("learned").is_some());
    let out = find("skill_read")
        .call(&ctx, json!({"name":"learned"}))
        .await
        .unwrap();
    assert!(format!("{out:?}").contains("Step 1"));
    let list = find("skill_list").call(&ctx, json!({})).await.unwrap();
    assert!(format!("{list:?}").contains("learned"));

    // traversal through the tools is rejected
    assert!(
        find("skill_read")
            .call(&ctx, json!({"name":"learned","file":"../../x"}))
            .await
            .is_err()
    );
    assert!(
        find("skill_write")
            .call(&ctx, json!({"name":"../x","content":content}))
            .await
            .is_err()
    );
    assert!(find("skill_read").call(&ctx, json!({})).await.is_err());
}
