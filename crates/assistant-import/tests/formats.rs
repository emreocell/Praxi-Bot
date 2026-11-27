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
    assert_eq!(
        remote.detail.as_deref(),
        Some("https://mcp.example.com/mcp"),
        "the whole URL, not the host"
    );

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

fn home_with(files: &[(&str, &str)]) -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    for (path, text) in files {
        let file = home.path().join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    home
}

fn scan_home(home: &std::path::Path, options: &ScanOptions) -> Plan {
    let data = tempfile::tempdir().unwrap();
    build_plan(
        &Roots::at_home(home),
        data.path(),
        &BTreeSet::new(),
        options,
    )
}

fn folder_scan(folder: &std::path::Path) -> Plan {
    let home = tempfile::tempdir().unwrap();
    scan_home(
        home.path(),
        &ScanOptions {
            only: Some("folder".into()),
            folder: Some(folder.to_path_buf()),
        },
    )
}

#[test]
fn the_detail_of_a_server_is_its_whole_command_line() {
    let home = home_with(&[(
        ".cursor/mcp.json",
        r#"{"mcpServers":{
            "a":{"command":"npx","args":["-y","pkg@1.2.3","--evil-flag","--root=/etc","two words",""]},
            "b":{"url":"https://example.com/mcp?team=7&mode=full"}}}"#,
    )]);
    let plan = scan_home(home.path(), &ScanOptions::default());
    assert_eq!(
        server(&plan, "a").detail.as_deref(),
        Some(r#"npx -y pkg@1.2.3 --evil-flag --root=/etc "two words" """#),
        "every flag and argument, a space-holding one quoted"
    );
    assert_eq!(
        server(&plan, "b").detail.as_deref(),
        Some("https://example.com/mcp?team=7&mode=full")
    );
    assert!(
        server(&plan, "a").default_on,
        "short enough to read: ticked"
    );
}

#[test]
fn a_very_long_command_line_is_cut_by_characters_and_not_ticked() {
    // Multi-byte characters: the cut must fall on a character, not a byte.
    let filler = "ö".repeat(1500);
    let home = home_with(&[(
        ".cursor/mcp.json",
        &format!(
            r#"{{"mcpServers":{{"long":{{"command":"npx","args":["-y","pkg@1.2.3","{filler}","--tail"]}}}}}}"#
        ),
    )]);
    let plan = scan_home(home.path(), &ScanOptions::default());
    let item = server(&plan, "long");
    let detail = item.detail.as_deref().unwrap();
    assert!(detail.starts_with("npx -y pkg@1.2.3 öö"));
    assert!(detail.ends_with('…'), "the cut is visible");
    assert_eq!(detail.chars().count(), 1_001);
    assert!(!detail.contains("--tail"));
    assert!(item.import, "it can still be imported");
    assert!(
        !item.default_on,
        "not ticked: the owner cannot read all of it here"
    );
    assert!(item.note.contains("longer than shown"));
}

#[test]
fn instruction_text_is_not_in_the_scan_only_its_size() {
    let home = home_with(&[
        (
            ".claude/CLAUDE.md",
            "# Notes\nuse CANARY-instruction-text-6b1c always\nsecond line\n",
        ),
        (".codex/AGENTS.md", "Line one CANARY-agents-text-0d4e\n"),
    ]);
    let plan = scan_home(home.path(), &ScanOptions::default());
    let notes = find(&plan, Kind::Instructions, "CLAUDE.md");
    assert!(notes.import);
    assert_eq!(notes.detail.as_deref(), Some("59 bytes, 3 lines"));
    let everything = serde_json::to_string(&plan.items).unwrap();
    assert!(!everything.contains("CANARY-instruction-text"));
    assert!(!everything.contains("CANARY-agents-text"));
    assert!(!format!("{:?}", plan.items).contains("CANARY-instruction-text"));
    // The text is held for the run, which returns it once the owner ticked the item.
    assert!(
        plan.instructions
            .iter()
            .any(|i| i.text.contains("CANARY-instruction-text-6b1c"))
    );
}

#[test]
fn a_longer_instruction_file_shows_its_size_in_kb() {
    let home = home_with(&[(".claude/CLAUDE.md", &"Be kind.\n".repeat(300))]);
    let plan = scan_home(home.path(), &ScanOptions::default());
    let notes = find(&plan, Kind::Instructions, "CLAUDE.md");
    assert_eq!(notes.detail.as_deref(), Some("2.6 KB, 300 lines"));
}

#[test]
fn a_config_file_over_the_cap_is_not_read() {
    let pad = " ".repeat(1024 * 1024);
    let home = home_with(&[(
        ".cursor/mcp.json",
        &format!(r#"{{"mcpServers":{{}}}}{pad}"#),
    )]);
    let plan = scan_home(home.path(), &ScanOptions::default());
    assert_eq!(plan.items.len(), 1, "{:#?}", plan.items);
    assert_eq!(plan.items[0].reason, Reason::Unreadable);
    assert!(plan.items[0].note.contains("too large"));
    // Just under the cap is read.
    let pad = " ".repeat(1024 * 1024 - 100);
    let home = home_with(&[(
        ".cursor/mcp.json",
        &format!(r#"{{"mcpServers":{{"ok":{{"command":"npx","args":["-y","a@1.0.0"]}}}}}}{pad}"#),
    )]);
    let plan = scan_home(home.path(), &ScanOptions::default());
    assert!(server(&plan, "ok").import);
}

#[test]
fn a_markdown_file_over_the_cap_is_not_read() {
    let folder = tempfile::tempdir().unwrap();
    std::fs::write(folder.path().join("big.md"), "x".repeat(256 * 1024 + 1)).unwrap();
    std::fs::write(folder.path().join("fine.md"), "# Fine\nA small prompt.").unwrap();
    std::fs::write(folder.path().join("AGENTS.md"), "y".repeat(300 * 1024)).unwrap();
    let plan = folder_scan(folder.path());
    let big = find(&plan, Kind::Other, "big");
    assert_eq!(big.reason, Reason::Unreadable);
    assert!(big.note.contains("too large"));
    assert!(find(&plan, Kind::Skill, "fine").import);
    assert_eq!(
        find(&plan, Kind::Instructions, "AGENTS.md").reason,
        Reason::Unreadable
    );
    assert!(plan.instructions.is_empty());
}

#[test]
fn the_file_count_is_capped_while_walking() {
    let folder = tempfile::tempdir().unwrap();
    for i in 0..350 {
        std::fs::write(
            folder.path().join(format!("p{i:03}.md")),
            format!("# P{i}\nText."),
        )
        .unwrap();
    }
    // Plenty of other entries that are no markdown: they only count as entries looked at.
    for i in 0..2500 {
        std::fs::write(folder.path().join(format!("z{i:04}.dat")), "").unwrap();
    }
    let plan = folder_scan(folder.path());
    assert_eq!(plan.items.len(), assistant_import::MAX_FOLDER_FILES);
}

#[test]
fn a_scan_stops_reading_at_the_total_byte_cap() {
    let folder = tempfile::tempdir().unwrap();
    // 100 files of 200 KB (each under the per-file cap): 20 MB, over the 16 MB total.
    let body = format!("# Big\n{}", "word ".repeat(40_000));
    for i in 0..100 {
        std::fs::write(folder.path().join(format!("f{i:03}.md")), &body).unwrap();
    }
    let plan = folder_scan(folder.path());
    let stopped = plan
        .items
        .iter()
        .filter(|i| i.note.contains("size limit"))
        .count();
    assert!(
        stopped > 10,
        "{stopped} files were refused for the total cap"
    );
    assert_eq!(plan.items.len(), 100);
}

#[test]
fn a_skill_folder_that_is_too_big_is_not_handed_to_the_hub() {
    let folder = tempfile::tempdir().unwrap();
    let skill = folder.path().join("heavy");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: heavy\ndescription: Heavy\n---\nText",
    )
    .unwrap();
    std::fs::write(skill.join("blob.bin"), vec![0u8; 2 * 1024 * 1024]).unwrap();
    let many = folder.path().join("many");
    std::fs::create_dir_all(&many).unwrap();
    std::fs::write(
        many.join("SKILL.md"),
        "---\nname: many\ndescription: Many\n---\nText",
    )
    .unwrap();
    for i in 0..250 {
        std::fs::write(many.join(format!("f{i}.txt")), "x").unwrap();
    }
    let plan = folder_scan(folder.path());
    for name in ["heavy", "many"] {
        let item = find(&plan, Kind::Skill, name);
        assert!(!item.import && item.reason == Reason::Invalid, "{item:?}");
        assert!(item.note.contains("too large"), "{}", item.note);
    }
}

#[cfg(unix)]
mod links {
    use super::*;
    use std::os::unix::fs::symlink;

    fn outside() -> tempfile::TempDir {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(
            outside.path().join("secret.md"),
            "# Secret\nCANARY-outside-file-4d2e",
        )
        .unwrap();
        std::fs::create_dir_all(outside.path().join("stolen")).unwrap();
        std::fs::write(
            outside.path().join("stolen/SKILL.md"),
            "---\nname: stolen\ndescription: Outside\n---\nCANARY-outside-file-4d2e",
        )
        .unwrap();
        outside
    }

    fn nothing_leaked(plan: &Plan) {
        let all = format!("{:?}{:?}", plan.items, plan.skills);
        assert!(!all.contains("CANARY-outside-file-4d2e"), "{all}");
        for skill in &plan.skills {
            let text = std::fs::read_to_string(skill.dir.join("SKILL.md")).unwrap();
            assert!(!text.contains("CANARY-outside-file-4d2e"));
        }
    }

    #[test]
    fn links_inside_skill_folders_and_subfolders_are_skipped() {
        let folder = tempfile::tempdir().unwrap();
        let out = outside();
        // A skill folder whose SKILL.md is a link.
        std::fs::create_dir_all(folder.path().join("linked-skill")).unwrap();
        symlink(
            out.path().join("stolen/SKILL.md"),
            folder.path().join("linked-skill/SKILL.md"),
        )
        .unwrap();
        // A real skill folder holding a link to an outside file.
        std::fs::create_dir_all(folder.path().join("real")).unwrap();
        std::fs::write(
            folder.path().join("real/SKILL.md"),
            "---\nname: real\ndescription: Real\n---\nText",
        )
        .unwrap();
        symlink(
            out.path().join("secret.md"),
            folder.path().join("real/notes.md"),
        )
        .unwrap();
        // A sub folder that is a link, one level down in a real folder, and a linked skill dir.
        std::fs::create_dir_all(folder.path().join("group")).unwrap();
        symlink(out.path().join("stolen"), folder.path().join("group/inner")).unwrap();
        symlink(out.path().join("stolen"), folder.path().join("top-linked")).unwrap();
        std::fs::create_dir_all(folder.path().join("docs")).unwrap();
        symlink(
            out.path().join("secret.md"),
            folder.path().join("docs/x.md"),
        )
        .unwrap();
        let plan = folder_scan(folder.path());
        nothing_leaked(&plan);
        assert!(
            plan.skills.is_empty(),
            "no skill with a link in it is offered: {:#?}",
            plan.items
        );
        assert!(
            !plan
                .items
                .iter()
                .any(|i| i.name == "x" || i.name == "stolen"),
            "{:#?}",
            plan.items
        );
        let real = find(&plan, Kind::Skill, "real");
        assert!(!real.import && real.note.contains("link"), "{real:?}");
    }

    #[test]
    fn a_folder_reached_through_a_link_stays_inside_the_real_folder() {
        let out = outside();
        let base = tempfile::tempdir().unwrap();
        let link = base.path().join("via-link");
        symlink(out.path(), &link).unwrap();
        // The owner chose a link: the canonical folder is what is read and what the rest is
        // checked against; the outside file is inside *that* folder, so it is read as the folder's own.
        let plan = folder_scan(&link);
        assert!(plan.items.iter().any(|i| i.name == "secret"));
        // A link out of the canonical folder is not followed.
        symlink(base.path(), out.path().join("up")).unwrap();
        std::fs::write(base.path().join("above.md"), "# Above\nCANARY-above-9a2f").unwrap();
        let plan = folder_scan(&link);
        assert!(
            !plan.items.iter().any(|i| i.name == "above"),
            "{:#?}",
            plan.items
        );
    }

    #[test]
    fn a_pipe_in_a_folder_does_not_block_the_scan() {
        let folder = tempfile::tempdir().unwrap();
        let made = std::process::Command::new("mkfifo")
            .arg(folder.path().join("pipe.md"))
            .status();
        if !made.is_ok_and(|s| s.success()) {
            return;
        }
        std::fs::write(folder.path().join("fine.md"), "# Fine\nText.").unwrap();
        let plan = folder_scan(folder.path());
        assert!(find(&plan, Kind::Skill, "fine").import);
        assert!(!plan.items.iter().any(|i| i.name == "pipe"));
    }
}

// `/proc`, `/sys` and `/dev` are system folders on Unix; on Windows they are not absolute paths.
#[cfg(unix)]
#[test]
fn a_system_folder_is_refused() {
    use assistant_import::FolderError;
    for path in [
        "/proc",
        "/proc/self",
        "/sys",
        "/sys/kernel",
        "/dev",
        "/dev/shm",
    ] {
        let path = std::path::Path::new(path);
        if cfg!(target_os = "linux") || !path.exists() {
            let got = assistant_import::resolve_folder(path);
            assert!(
                matches!(
                    got,
                    Err(FolderError::SystemFolder | FolderError::Unreadable)
                ),
                "{path:?}: {got:?}"
            );
        }
    }
    assert_eq!(
        assistant_import::resolve_folder(std::path::Path::new("/proc")),
        Err(FolderError::SystemFolder)
    );
    assert_eq!(
        assistant_import::resolve_folder(std::path::Path::new("relative")),
        Err(FolderError::NotAPath)
    );
    let plan = folder_scan(std::path::Path::new("/proc"));
    assert_eq!(plan.items.len(), 1);
    assert_eq!(plan.items[0].reason, Reason::Unreadable);
    assert!(plan.items[0].note.contains("system folders"));
}

#[cfg(unix)]
#[test]
fn a_link_to_a_system_folder_and_dotdot_are_resolved_before_the_check() {
    use assistant_import::FolderError;
    let base = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink("/proc", base.path().join("p")).unwrap();
    assert_eq!(
        assistant_import::resolve_folder(&base.path().join("p")),
        Err(FolderError::SystemFolder)
    );
    // `..` is gone from the result.
    std::fs::create_dir_all(base.path().join("a/b")).unwrap();
    let got = assistant_import::resolve_folder(&base.path().join("a/b/../b")).unwrap();
    assert_eq!(got, base.path().join("a/b").canonicalize().unwrap());
    // Outside the home folder is fine: the folder is the owner's choice.
    assert!(assistant_import::resolve_folder(base.path()).is_ok());
}
