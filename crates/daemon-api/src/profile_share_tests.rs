use super::*;

fn ctx() -> ImportContext {
    ImportContext {
        known_tools: [
            "fs_read",
            "fs_write",
            "shell_run",
            "web_search",
            "browser_open",
            "ask_user",
            "todo_write",
            "pc_info",
        ]
        .map(str::to_owned)
        .to_vec(),
        providers: vec!["gemini".to_owned()],
        installed_skills: vec![("known-skill".to_owned(), "1".to_owned())],
    }
}

fn opts(ceiling: SharePreset) -> ImportOptions {
    ImportOptions {
        ceiling,
        name: None,
        executor: Some("gemini/flash".to_owned()),
        computer_files: false,
    }
}

fn file(body: &str) -> String {
    format!(r#"{{"format":"praxi-bot-profile","format_version":1,{body}}}"#)
}

#[test]
fn every_gallery_template_parses_and_imports_safely() {
    let all = templates();
    assert_eq!(
        all.len(),
        TEMPLATE_FILES.len(),
        "a template failed to parse"
    );
    for t in &all {
        let meta = t.meta.as_ref().unwrap();
        for lang in ["en", "tr", "de"] {
            assert!(!meta.names[lang].is_empty() && !meta.descriptions[lang].is_empty());
        }
        assert_ne!(meta.names["en"], meta.names["tr"]);
        let plan = plan_import(t, &opts(SharePreset::ApproveDestructive), &ctx()).unwrap();
        serde_json::from_value::<agent_core::BotProfile>(plan.config.clone())
            .unwrap_or_else(|e| panic!("{}: {e}", meta.id));
        assert!(plan.preview.granted_level <= SharePreset::ApproveDestructive);
        assert!(
            plan.config
                .pointer("/permissions/allow_egress_after_reads")
                .is_none()
        );
    }
}

#[test]
fn export_carries_no_private_data() {
    let config = json!({
        "bot": {"id": "CANARY-id", "name": "Mira", "persona": "Be kind.",
                "avatar": "data:image/png;CANARY-avatar", "shape": "cloud", "color": "#fff"},
        "models": {"executor": "gemini/flash", "planner": "cli:CANARY-engine",
                   "api_key": "CANARY-key", "fallback": ["cli:CANARY2", "ollama/x"]},
        "permissions": {"preset": "approve_destructive", "allow_egress_after_reads": true,
            "categories": {"shell": {"level": {"level": "deny"}}}, "grants": ["CANARY-grant"]},
        "tools": {"allow": ["fs_*", "mcp:CANARY-server/*", "composio_CANARY"],
                  "exclude": ["fs_delete"], "manage_bots": true},
        "files": {"mode": "folder", "folders": ["C:/Users/CANARY-folder"]},
        "engine": {"workspace": "C:/CANARY-ws", "consent": {"full_access_at_ms": 5}},
        "runtime": {"computer": "server:CANARY-node"},
        "knowledge": {"sources": ["CANARY-source"]},
        "memory": "CANARY-memory"
    });
    let p = export("Mira", &config, vec![], vec![]).unwrap();
    let (name, text) = to_file(&p);
    assert_eq!(name, "mira.praxi-bot.json");
    assert!(!text.contains("CANARY"), "{text}");
    for word in [
        "egress",
        "manage_bots",
        "grants",
        "engine",
        "folders",
        "knowledge",
        "avatar",
    ] {
        assert!(!text.contains(word), "{word} leaked: {text}");
    }
    assert_eq!(p.permissions.preset, Some(SharePreset::ApproveDestructive));
    assert_eq!(p.tools.allow, ["fs_*"]);
    assert_eq!(parse(&text).unwrap(), p);
}

#[test]
fn export_refuses_a_persona_with_a_secret() {
    let config = json!({"bot": {"persona": "my key is sk-abcdefghijklmnop1234"}});
    assert!(export("X", &config, vec![], vec![]).is_err());
}

#[test]
fn import_never_exceeds_the_ceiling() {
    let p = parse(&file(
        r#""bot":{"name":"B"},"permissions":{"preset":"full","categories":{"shell":{"level":"allow","by_initiator":{"schedule":"allow"}},"money":{"level":"deny"}}}"#,
    ))
    .unwrap();
    let plan = plan_import(&p, &opts(SharePreset::ReadAndDraft), &ctx()).unwrap();
    assert_eq!(plan.preview.granted_level, SharePreset::ReadAndDraft);
    assert_eq!(
        plan.config.pointer("/permissions/preset").unwrap(),
        "read-and-draft"
    );
    let shell = plan
        .config
        .pointer("/permissions/categories/shell")
        .unwrap();
    assert_eq!(shell.pointer("/level/level").unwrap(), "ask");
    assert_eq!(
        shell.pointer("/by_initiator/schedule/level").unwrap(),
        "ask"
    );
    // a stricter rule is kept
    assert_eq!(
        plan.config
            .pointer("/permissions/categories/money/level/level")
            .unwrap(),
        "deny"
    );
    assert!(plan.preview.notes.iter().any(|n| n.code == "level_lowered"));
    assert!(plan.preview.notes.iter().any(|n| n.code == "rule_lowered"));
    assert!(
        plan.preview
            .categories
            .iter()
            .all(|c| c.granted <= ShareLevel::Ask)
    );
    // the default ceiling is the safest level
    let dflt = plan_import(&p, &opts(SharePreset::ReadOnly), &ctx()).unwrap();
    assert!(
        dflt.preview
            .categories
            .iter()
            .all(|c| c.granted == ShareLevel::Deny)
    );
}

#[test]
fn import_drops_unknown_tools_and_never_widens_an_emptied_list() {
    let p = parse(&file(
        r#""bot":{"name":"B"},"tools":{"allow":["nope_*","mcp:srv/*"],"exclude":["fs_delete"]}"#,
    ))
    .unwrap();
    let plan = plan_import(&p, &opts(SharePreset::Full), &ctx()).unwrap();
    let codes: Vec<_> = plan.preview.notes.iter().map(|n| n.code.as_str()).collect();
    assert!(codes.contains(&"tool_dropped_unknown") && codes.contains(&"tool_dropped_binding"));
    assert!(codes.contains(&"tools_all_unknown"));
    assert!(
        !plan.preview.all_tools,
        "an emptied allow list must not mean every tool"
    );
    assert_eq!(
        plan.config.pointer("/tools/allow").unwrap(),
        &json!(["ask_user", "todo_write"])
    );
}

#[test]
fn import_gives_no_file_access_unless_confirmed() {
    let p = parse(&file(
        r#""bot":{"name":"B"},"permissions":{"preset":"full"}"#,
    ))
    .unwrap();
    let none = plan_import(&p, &opts(SharePreset::Full), &ctx()).unwrap();
    assert_eq!(none.config.pointer("/files/mode").unwrap(), "folder");
    assert!(!none.preview.can_run_shell && !none.preview.can_use_files);
    let mut o = opts(SharePreset::Full);
    o.computer_files = true;
    let some = plan_import(&p, &o, &ctx()).unwrap();
    assert!(some.preview.can_run_shell && some.preview.can_use_files);
}

#[test]
fn model_must_exist_here_and_cli_is_never_taken() {
    let p = parse(&file(
        r#""bot":{"name":"B"},"models":{"executor":"cli:claude/x"}"#,
    ))
    .unwrap();
    let mut o = opts(SharePreset::ReadOnly);
    o.executor = None;
    let plan = plan_import(&p, &o, &ctx()).unwrap();
    assert!(plan.executor.is_none());
    o.executor = Some("cli:claude/x".to_owned());
    assert!(plan_import(&p, &o, &ctx()).is_err());
}

#[test]
fn hostile_files_are_refused() {
    let huge = "a".repeat(MAX_FILE_BYTES + 1);
    assert!(parse(&huge).is_err());
    let persona = "x".repeat(MAX_PERSONA_CHARS + 1);
    assert!(
        parse(&file(&format!(
            r#""bot":{{"name":"B","persona":"{persona}"}}"#
        )))
        .is_err()
    );
    for body in [
        // standing grants, egress waiver, engine, folders: not in the schema
        r#""bot":{"name":"B"},"grants":[{"tool":"shell_run"}]"#,
        r#""bot":{"name":"B"},"permissions":{"allow_egress_after_reads":true}"#,
        r#""bot":{"name":"B"},"engine":{"mode":"native_tools"}"#,
        r#""bot":{"name":"B"},"files":{"folders":["C:/"]}"#,
        r#""bot":{"name":"B"},"tools":{"manage_bots":true}"#,
        // path traversal in skill names
        r#""bot":{"name":"B"},"skills":[{"name":"../../etc/passwd"}]"#,
        r#""bot":{"name":"B"},"skills":[{"name":"a/b"}]"#,
        // bad model, tool, category, level
        r#""bot":{"name":"B"},"models":{"executor":"http://evil/x y"}"#,
        r#""bot":{"name":"B"},"tools":{"allow":["../x"]}"#,
        r#""bot":{"name":"B"},"permissions":{"categories":{"nope":{"level":"allow"}}}"#,
        r#""bot":{"name":"B"},"permissions":{"preset":"god"}"#,
        // control or bidi characters, secrets, empty name
        r#""bot":{"name":"B","persona":"hi\u202Ethere"}"#,
        r#""bot":{"name":"B","persona":"key sk-abcdefghijklmnop1234"}"#,
        r#""bot":{"name":" "}"#,
    ] {
        assert!(parse(&file(body)).is_err(), "accepted: {body}");
    }
    assert!(parse(r#"{"format":"x","format_version":1,"bot":{"name":"B"}}"#).is_err());
    let newer =
        parse(r#"{"format":"praxi-bot-profile","format_version":2,"bot":{"name":"B"},"future":1}"#)
            .unwrap_err();
    assert!(newer.contains("version 2"), "{newer}");
}

#[test]
fn urls_and_paths_in_text_stay_inert() {
    let p = parse(&file(
        r#""bot":{"name":"B","persona":"see file:///C:/secret and http://x.invalid/a"},"schedules":[{"title":"t","prompt":"p"}]"#,
    ))
    .unwrap();
    let plan = plan_import(&p, &opts(SharePreset::ReadOnly), &ctx()).unwrap();
    assert!(plan.preview.persona.contains("file:///"));
    assert!(
        plan.preview
            .notes
            .iter()
            .any(|n| n.code == "schedules_not_created")
    );
}
