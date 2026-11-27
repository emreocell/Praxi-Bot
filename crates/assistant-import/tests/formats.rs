//! One fixture per source format: what is found, what is skipped and why, and that no secret
//! value ever reaches a plan item, a config or a debug print.

use std::collections::BTreeSet;
use std::path::PathBuf;

use assistant_import::{App, Item, Kind, Plan, Reason, Roots, ScanOptions, build_plan};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn scan(options: &ScanOptions, taken: &[&str]) -> Plan {
    let data = tempfile::tempdir().unwrap();
    let taken: BTreeSet<String> = taken.iter().map(|s| (*s).to_owned()).collect();
    build_plan(
        &Roots::at_home(fixtures().join("home")),
        data.path(),
        &taken,
        options,
    )
}

fn only(app: &str) -> Plan {
    scan(
        &ScanOptions {
            only: Some(app.into()),
            folder: None,
        },
        &[],
    )
}

fn find<'a>(plan: &'a Plan, kind: Kind, name: &str) -> &'a Item {
    plan.items
        .iter()
        .find(|i| i.kind == kind && i.name == name)
        .unwrap_or_else(|| panic!("no {kind:?} {name}: {:#?}", plan.items))
}

fn server<'a>(plan: &'a Plan, name: &str) -> &'a Item {
    find(plan, Kind::McpServer, name)
}

const CANARIES: [&str; 8] = [
    "CANARY-claude-desktop-key-7f3a",
    "CANARY-claude-desktop-header-91bc",
    "CANARY-claude-code-token-22de",
    "CANARY-cursor-db-pass-5aa1",
    "CANARY-vscode-env-3c9d",
    "CANARY-codex-env-8b21",
    "CANARY-codex-auth-key-0e5f",
    "LINEAR_TOKEN",
];

#[test]
fn claude_desktop_and_code_servers_arrive_disabled_with_secret_slots() {
    let plan = only("claude");
    let fetch = server(&plan, "fetch");
    assert!(fetch.import && fetch.reason == Reason::ReadyWithSecrets);
    assert_eq!(fetch.secrets.len(), 1);
    assert_eq!(fetch.secrets[0].name, "mcp/fetch/env/API_KEY");
    assert_eq!(fetch.secrets[0].place, "env");
    assert_eq!(fetch.source, "~/.config/Claude/claude_desktop_config.json");
    assert_eq!(fetch.detail.as_deref(), Some("uvx mcp-server-fetch==0.6.2"));

    let remote = server(&plan, "remote");
    assert_eq!(remote.secrets[0].place, "header");
    assert_eq!(remote.detail.as_deref(), Some("mcp.example.com"));

    assert_eq!(server(&plan, "loose").reason, Reason::Unpinned);
    assert_eq!(server(&plan, "legacy").reason, Reason::Unsupported);
    assert_eq!(server(&plan, "danger").reason, Reason::NeverList);

    let github = server(&plan, "github");
    assert!(github.import);
    assert_eq!(github.source, "~/.claude.json");

    assert!(plan.servers.iter().all(|s| !s.parsed.config.enabled));
    assert_eq!(plan.servers.len(), 3);
}

#[test]
fn claude_skills_agents_instructions_and_commands() {
    let plan = only("claude");
    let tidy = find(&plan, Kind::Skill, "tidy-notes");
    assert!(tidy.import && tidy.reason == Reason::Ready);

    let shady = find(&plan, Kind::Skill, "shady");
    assert!(!shady.import && shady.reason == Reason::ReviewFirst);
    assert!(shady.note.contains("bot skills install"));

    let agent = find(&plan, Kind::Skill, "code-reviewer");
    assert!(agent.import);
    assert!(
        agent
            .note
            .contains("tools and model settings are not copied")
    );
    // Only the name and description of the agent reach the staged skill.
    let staged = plan
        .skills
        .iter()
        .find(|s| s.item_id == agent.id)
        .map(|s| std::fs::read_to_string(s.dir.join("SKILL.md")).unwrap())
        .unwrap();
    assert!(staged.contains("You review code changes."));
    assert!(!staged.contains("tools:") && !staged.contains("sonnet"));

    let notes = find(&plan, Kind::Instructions, "CLAUDE.md");
    assert!(
        notes.import && !notes.default_on,
        "instructions are never ticked by default"
    );
    assert_eq!(plan.instructions.len(), 1);

    assert_eq!(
        find(&plan, Kind::Other, "commands").reason,
        Reason::NotImported
    );
}

#[test]
fn cursor_references_are_dropped_and_keys_in_values_become_slots() {
    let plan = only("cursor");
    let linear = server(&plan, "linear");
    assert!(linear.import);
    assert_eq!(
        linear.reason,
        Reason::Ready,
        "a variable reference holds no secret"
    );
    assert_eq!(linear.dropped, ["Authorization"]);
    assert_eq!(linear.secrets.len(), 0);

    let files = server(&plan, "files");
    assert_eq!(files.reason, Reason::Variables);

    let db = server(&plan, "db");
    assert_eq!(db.secrets.len(), 1);
    assert_eq!(db.secrets[0].name, "mcp/db/env/DATABASE_URL");
}

#[test]
fn vscode_mcp_json_allows_comments_and_trailing_commas() {
    let plan = only("vscode");
    let context7 = server(&plan, "context7");
    assert!(context7.import);
    assert_eq!(context7.dropped, ["CONTEXT7_API_KEY"]);
    let clock = server(&plan, "clock");
    assert_eq!(clock.secrets[0].name, "mcp/clock/env/TZ_TOKEN");
    assert_eq!(clock.source, "~/.config/Code/User/mcp.json");
}

#[test]
fn codex_toml_servers_prompts_and_agents_md() {
    let plan = only("codex");
    let docs = server(&plan, "docs");
    assert!(docs.import);
    assert_eq!(docs.secrets[0].name, "mcp/docs/env/DOCS_TOKEN");
    let figma = server(&plan, "figma");
    assert!(figma.import);
    assert_eq!(
        figma.dropped,
        ["Authorization"],
        "the token comes from an env var: not copied"
    );
    // Every other header value is held back for the secret store, not kept in the config.
    assert_eq!(figma.secrets.len(), 1);
    assert_eq!(figma.secrets[0].name, "mcp/figma/header/x-client");

    let review = find(&plan, Kind::Skill, "review");
    assert!(review.import);
    assert_eq!(review.detail.as_deref(), Some("Review the staged changes"));

    assert!(find(&plan, Kind::Instructions, "AGENTS.md").import);
    let auth = find(&plan, Kind::Other, "auth.json");
    assert_eq!(auth.reason, Reason::NeverCopied);
}

#[test]
fn a_folder_of_skills_and_prompts_is_read_only_when_named() {
    let none = scan(&ScanOptions::default(), &[]);
    assert!(none.items.iter().all(|i| i.app != App::Folder));

    let plan = scan(
        &ScanOptions {
            only: Some("folder".into()),
            folder: Some(fixtures().join("folder")),
        },
        &[],
    );
    assert!(find(&plan, Kind::Skill, "release-notes").import);
    assert!(find(&plan, Kind::Skill, "standup").import);
    assert!(find(&plan, Kind::Skill, "plain").import);
    assert!(find(&plan, Kind::Instructions, "AGENTS.md").import);
    assert!(
        !plan
            .items
            .iter()
            .any(|i| i.name == "README" || i.name == "readme"),
        "README is not a prompt"
    );
}

#[test]
fn a_missing_folder_is_reported_not_ignored() {
    let plan = scan(
        &ScanOptions {
            only: Some("folder".into()),
            folder: Some(fixtures().join("does-not-exist")),
        },
        &[],
    );
    assert_eq!(plan.items.len(), 1);
    assert_eq!(plan.items[0].reason, Reason::Unreadable);
}

#[test]
fn existing_names_are_skipped_and_ids_are_unique() {
    let plan = scan(&ScanOptions::default(), &["fetch", "docs"]);
    assert_eq!(server(&plan, "fetch").reason, Reason::AlreadyHave);
    assert_eq!(server(&plan, "docs").reason, Reason::AlreadyHave);
    let ids: BTreeSet<&str> = plan.items.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(ids.len(), plan.items.len(), "an id names exactly one item");
}

#[test]
fn no_secret_value_reaches_the_plan_items_or_the_configs() {
    let plan = scan(
        &ScanOptions {
            only: None,
            folder: Some(fixtures().join("folder")),
        },
        &[],
    );
    let everything = format!(
        "{} {:?} {:?}",
        serde_json::to_string(&plan.items).unwrap(),
        plan.servers,
        plan.servers
            .iter()
            .map(|s| serde_json::to_string(&s.parsed.config).unwrap())
            .collect::<Vec<_>>()
    );
    for canary in CANARIES {
        assert!(!everything.contains(canary), "{canary} leaked");
    }
    // The values are held for the secret store, once, in the parsed server.
    let held: Vec<&str> = plan
        .servers
        .iter()
        .flat_map(|s| s.parsed.secrets.iter().map(|(_, v)| v.as_str()))
        .collect();
    assert!(held.contains(&"CANARY-claude-desktop-key-7f3a"));
}

#[test]
fn a_key_inside_the_command_line_is_not_imported() {
    let home = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".cursor")).unwrap();
    std::fs::write(
        home.path().join(".cursor/mcp.json"),
        r#"{"mcpServers":{"inline":{"command":"npx","args":["-y","some-mcp@1.0.0","--api-key","sk-abcdefghijklmnop1234"]}}}"#,
    )
    .unwrap();
    let plan = build_plan(
        &Roots::at_home(home.path()),
        data.path(),
        &BTreeSet::new(),
        &ScanOptions::default(),
    );
    let item = server(&plan, "inline");
    assert_eq!(item.reason, Reason::SecretInCommand);
    assert!(
        !serde_json::to_string(&plan.items)
            .unwrap()
            .contains("sk-abcdefghijklmnop1234")
    );
    assert!(plan.servers.is_empty());
}

#[test]
fn hidden_characters_and_long_files_do_not_become_instructions() {
    let home = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".claude")).unwrap();
    std::fs::write(
        home.path().join(".claude/CLAUDE.md"),
        "Be kind.\u{202E}evil\n",
    )
    .unwrap();
    let plan = build_plan(
        &Roots::at_home(home.path()),
        data.path(),
        &BTreeSet::new(),
        &ScanOptions::default(),
    );
    assert!(!find(&plan, Kind::Instructions, "CLAUDE.md").import);

    let long = "Keep it short and kind. ".repeat(400);
    std::fs::write(home.path().join(".claude/CLAUDE.md"), &long).unwrap();
    let plan = build_plan(
        &Roots::at_home(home.path()),
        data.path(),
        &BTreeSet::new(),
        &ScanOptions::default(),
    );
    let notes = find(&plan, Kind::Instructions, "CLAUDE.md");
    assert_eq!(notes.reason, Reason::TooLong);
    assert!(
        find(&plan, Kind::Skill, "claude-md").import,
        "a long file is offered as a skill"
    );
    assert!(plan.instructions.is_empty());
}

#[test]
fn an_unreadable_or_broken_file_is_listed() {
    let home = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".cursor")).unwrap();
    std::fs::write(home.path().join(".cursor/mcp.json"), "{ not json").unwrap();
    std::fs::create_dir_all(home.path().join(".codex")).unwrap();
    std::fs::write(home.path().join(".codex/config.toml"), "= nope").unwrap();
    let plan = build_plan(
        &Roots::at_home(home.path()),
        data.path(),
        &BTreeSet::new(),
        &ScanOptions::default(),
    );
    assert_eq!(plan.items.len(), 2);
    assert!(
        plan.items
            .iter()
            .all(|i| i.reason == Reason::Unreadable && !i.import)
    );
}

#[cfg(unix)]
#[test]
fn a_link_in_a_chosen_folder_is_never_followed() {
    let folder = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.md"), "CANARY-outside-file-4d2e").unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("secret.md"),
        folder.path().join("AGENTS.md"),
    )
    .unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("secret.md"),
        folder.path().join("linked.md"),
    )
    .unwrap();
    std::os::unix::fs::symlink(outside.path(), folder.path().join("linked-dir")).unwrap();
    let plan = scan(
        &ScanOptions {
            only: Some("folder".into()),
            folder: Some(folder.path().to_path_buf()),
        },
        &[],
    );
    assert!(plan.items.is_empty(), "{:#?}", plan.items);
    assert!(!format!("{:?}", plan.items).contains("CANARY-outside-file-4d2e"));
}
