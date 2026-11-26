//! `shell_interactive` end to end, through the registry like the agent loop calls it, against a
//! stand-in interactive program (`tests/fixtures/pty_fixture.rs`, started directly as a real
//! program: no shell is involved) that behaves like a CLI sign-in: it prints a link and a
//! one-time code, waits for a pasted code, answers, and exits non-zero on a wrong one.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use builtin_tools::{BuiltinConfig, GateRequest, ToolGate, register_builtin_tools};
use serde_json::{Value, json};
use tool_registry::{ContentPart, ToolContext, ToolRegistry};

struct AllowAll;

#[async_trait]
impl ToolGate for AllowAll {
    async fn authorize(&self, _req: &GateRequest) -> Result<(), String> {
        Ok(())
    }
}

/// The command line that starts the fixture in `mode`. The path is quoted: no shell reads it.
fn fixture(mode: &str) -> String {
    format!("\"{}\" {mode}", env!("CARGO_BIN_EXE_pty-fixture"))
}

fn registry() -> (tempfile::TempDir, ToolRegistry) {
    let data = tempfile::tempdir().unwrap();
    let reg = ToolRegistry::new();
    let cfg = BuiltinConfig::new(data.path().to_path_buf(), Arc::new(AllowAll));
    let _ = register_builtin_tools(&reg, cfg);
    (data, reg)
}

async fn try_call(reg: &ToolRegistry, ctx: &ToolContext, input: Value) -> Result<String, String> {
    let out = tokio::time::timeout(
        Duration::from_secs(60),
        reg.get("shell_interactive")
            .expect("shell_interactive is registered")
            .call(ctx, input),
    )
    .await
    .expect("the tool call hung")
    .map_err(|e| e.to_string())?;
    match &out.content[0] {
        ContentPart::Text { text } => Ok(text.clone()),
        other => panic!("{other:?}"),
    }
}

async fn call(reg: &ToolRegistry, input: Value) -> String {
    try_call(reg, &ToolContext::default(), input)
        .await
        .unwrap_or_else(|e| panic!("shell_interactive failed: {e}"))
}

fn session_of(answer: &str) -> String {
    answer
        .lines()
        .find_map(|l| l.strip_prefix("session: "))
        .unwrap_or_else(|| panic!("no session in {answer}"))
        .to_owned()
}

/// Read until `needle` shows up in the answers (output or prompt), like a patient bot would.
async fn read_until(reg: &ToolRegistry, id: &str, needle: &str) -> String {
    let mut seen = String::new();
    for _ in 0..30 {
        let a = call(reg, json!({"action":"read","session":id,"wait_ms":2000})).await;
        seen.push_str(&a);
        if seen.contains(needle) {
            return seen;
        }
        if a.contains("state: finished") || a.contains("state: failed") {
            break;
        }
    }
    panic!("never saw {needle:?} in {seen}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fake_login_is_completed_by_typing_the_code() {
    let (_d, reg) = registry();
    let started = call(
        &reg,
        json!({"action":"start","command":fixture("login"),"wait_ms":15000}),
    )
    .await;
    let id = session_of(&started);
    // The link and code reach the bot; the secret in the debug line does not.
    let seen = if started.contains("AB12-CD34") {
        started
    } else {
        read_until(&reg, &id, "AB12-CD34").await
    };
    assert!(seen.contains("https://auth.example.com/device"), "{seen}");
    assert!(!seen.contains("abcDEF0123456789"), "secret leaked: {seen}");
    // Waiting for the person: the state says so instead of hanging.
    let waiting = read_until(&reg, &id, "Paste code").await;
    assert!(
        waiting.contains("state: waiting_for_input") || waiting.contains("Paste code"),
        "{waiting}"
    );
    let screen = call(&reg, json!({"action":"screen","session":&id})).await;
    assert!(screen.contains("AB12-CD34"), "{screen}");
    assert!(
        !screen.contains("abcDEF0123456789"),
        "secret leaked: {screen}"
    );
    let listed = call(&reg, json!({"action":"list"})).await;
    assert!(listed.contains(&id), "{listed}");

    let typed = call(
        &reg,
        json!({"action":"write","session":&id,"input":"GOODCODE","enter":true,"wait_ms":15000}),
    )
    .await;
    let done =
        if typed.contains("Signed in as ada@example.com") && typed.contains("state: finished") {
            typed
        } else {
            read_until(&reg, &id, "state: finished").await
        };
    assert!(done.contains("exit_code: 0"), "{done}");
    // A finished session is forgotten: reading it again is an error, not a hang.
    let gone = try_call(
        &reg,
        &ToolContext::default(),
        json!({"action":"read","session":&id}),
    )
    .await;
    assert!(gone.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wrong_code_fails_with_the_exit_code_and_a_kill_is_clean() {
    let (_d, reg) = registry();
    let command = fixture("login");
    let started = call(
        &reg,
        json!({"action":"start","command":command,"wait_ms":15000}),
    )
    .await;
    let id = session_of(&started);
    read_until(&reg, &id, "Paste code").await;
    let wrong = call(
        &reg,
        json!({"action":"write","session":&id,"input":"nope","enter":true,"wait_ms":15000}),
    )
    .await;
    let failed = if wrong.contains("state: failed") {
        wrong
    } else {
        read_until(&reg, &id, "state: failed").await
    };
    assert!(failed.contains("exit_code: 4"), "{failed}");

    // And a session that is simply abandoned is killed on request.
    let started = call(
        &reg,
        json!({"action":"start","command":command,"wait_ms":0}),
    )
    .await;
    let id = session_of(&started);
    let killed = call(&reg, json!({"action":"kill","session":&id})).await;
    assert!(killed.contains("killed"), "{killed}");
    let listed = call(&reg, json!({"action":"list"})).await;
    assert!(listed.contains("no interactive sessions"), "{listed}");
}

/// The bypass strings of the security review, through the registry like the loop calls it: none
/// of them starts anything, and nothing is left open.
#[tokio::test(flavor = "multi_thread")]
async fn a_shell_cannot_be_started_here_however_it_is_spelled() {
    let (_d, reg) = registry();
    let ctx = ToolContext::default();
    for bad in [
        "bash",
        "sh -i",
        "/bin/bash --login",
        "b\"\"ash",
        "'b'ash",
        "\\bash",
        "p`wsh",
        "c^md",
        "{ bash; }",
        "! sh",
        "(bash)",
        "FOO=1 sudo -i",
        "command bash",
        "eval bash",
        "nohup bash",
        "setsid bash",
        "timeout 100 bash",
        "nice bash",
        "stdbuf -o0 bash",
        "rlwrap bash",
        "env sh",
        "python3.12",
        "python3 -c 'import pty'",
        "$(echo bash)",
        "echo hi; bash",
        "ls && ssh host",
        "echo hi | python",
        "\"/usr/bin/zsh\"",
        "powershell.exe",
        "C:\\Windows\\System32\\cmd.exe",
        "docker run -it ubuntu",
        "less /etc/passwd",
        "vim",
        "git log",
        "awk 'BEGIN{system(\"sh\")}'",
    ] {
        let err = try_call(&reg, &ctx, json!({"action":"start","command":bad}))
            .await
            .expect_err(bad);
        assert!(!err.is_empty(), "{bad}");
    }
    // `shell` is no longer an option, and the old wrapper is not reachable through it.
    let err = try_call(
        &reg,
        &ctx,
        json!({"action":"start","command":fixture("ask"),"shell":"bash"}),
    )
    .await
    .expect_err("shell");
    assert!(err.contains("no shell"), "{err}");
    let listed = call(&reg, json!({"action":"list"})).await;
    assert!(listed.contains("no interactive sessions"), "{listed}");
}

/// A command typed in pieces is judged as the line it becomes, from the session's own folder.
#[tokio::test(flavor = "multi_thread")]
async fn typing_a_catastrophic_command_in_pieces_is_refused() {
    let (_d, reg) = registry();
    let ctx = ToolContext::default();
    let started = call(
        &reg,
        json!({"action":"start","command":fixture("echo"),"wait_ms":10000}),
    )
    .await;
    let id = session_of(&started);
    read_until(&reg, &id, ">").await;
    // Each piece alone is harmless.
    call(
        &reg,
        json!({"action":"write","session":&id,"input":"rm ","wait_ms":200}),
    )
    .await;
    let err = try_call(
        &reg,
        &ctx,
        json!({"action":"write","session":&id,"input":"-rf /","wait_ms":200}),
    )
    .await
    .expect_err("the second piece completes `rm -rf /`");
    assert!(err.contains("never-list"), "{err}");
    assert!(
        !err.contains("-rf"),
        "the typed text is not echoed back: {err}"
    );
    // Nothing of it reached the program: the line it holds is still `rm `.
    let after = call(
        &reg,
        json!({"action":"write","session":&id,"key":"enter","wait_ms":3000}),
    )
    .await;
    let seen = if after.contains("got rm") {
        after
    } else {
        read_until(&reg, &id, "got rm").await
    };
    assert!(!seen.contains("-rf"), "{seen}");
    // A harmless line still goes through.
    let ok = call(
        &reg,
        json!({"action":"write","session":&id,"input":"hello","enter":true,"wait_ms":3000}),
    )
    .await;
    let ok = if ok.contains("got hello") {
        ok
    } else {
        read_until(&reg, &id, "got hello").await
    };
    assert!(ok.contains("got hello"), "{ok}");
    call(&reg, json!({"action":"kill","session":&id})).await;
}

/// A relative path in what is typed means the session's folder: a CLI sign-in store reached as
/// `.claude/...` from the home folder is the store.
#[tokio::test(flavor = "multi_thread")]
async fn a_relative_store_path_is_judged_from_the_session_folder() {
    let Some(home) = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)
        .filter(|h| h.is_absolute() && h.is_dir())
    else {
        return;
    };
    let (_d, reg) = registry();
    let ctx = ToolContext::default();
    let typed = "cat .claude/.credentials.json";
    let started = call(
        &reg,
        json!({"action":"start","command":fixture("echo"),"cwd":home,"wait_ms":10000}),
    )
    .await;
    let id = session_of(&started);
    read_until(&reg, &id, ">").await;
    let err = try_call(
        &reg,
        &ctx,
        json!({"action":"write","session":&id,"input":typed,"enter":true,"wait_ms":200}),
    )
    .await
    .expect_err("from the home folder this is a sign-in store");
    assert!(err.contains("never-list"), "{err}");
    call(&reg, json!({"action":"kill","session":&id})).await;
    // The same words in an unrelated folder are ordinary text.
    let elsewhere = tempfile::tempdir().unwrap();
    let started = call(
        &reg,
        json!({"action":"start","command":fixture("echo"),"cwd":elsewhere.path(),"wait_ms":10000}),
    )
    .await;
    let id = session_of(&started);
    read_until(&reg, &id, ">").await;
    let ok = call(
        &reg,
        json!({"action":"write","session":&id,"input":typed,"enter":true,"wait_ms":3000}),
    )
    .await;
    assert!(ok.contains("session:"), "{ok}");
    call(&reg, json!({"action":"kill","session":&id})).await;
}

/// 2000 characters and a tab used to spin the transcript forever while it held the session's
/// lock: every call on the session hung. They all answer now.
#[tokio::test(flavor = "multi_thread")]
async fn a_tab_at_the_end_of_a_2000_character_line_does_not_hang_the_session() {
    let (_d, reg) = registry();
    let started = call(
        &reg,
        json!({"action":"start","command":fixture("tab2000"),"wait_ms":10000}),
    )
    .await;
    let id = session_of(&started);
    read_until(&reg, &id, "done>").await;
    // `try_call` fails the test when a call takes a minute.
    let screen = call(&reg, json!({"action":"screen","session":&id})).await;
    assert!(screen.contains("session:"), "{screen}");
    let listed = call(&reg, json!({"action":"list"})).await;
    assert!(listed.contains(&id), "{listed}");
    let killed = call(&reg, json!({"action":"kill","session":&id})).await;
    assert!(killed.contains("killed"), "{killed}");
}

/// A program that ends after printing more than one answer carries stays readable: `start` used
/// to drop it with its output still unread.
#[tokio::test(flavor = "multi_thread")]
async fn a_finished_program_with_unread_output_stays_until_it_is_read() {
    let (_d, reg) = registry();
    let first = call(
        &reg,
        json!({"action":"start","command":fixture("flood"),"wait_ms":10000}),
    )
    .await;
    let id = session_of(&first);
    assert!(first.contains("more_output: true"), "{first}");
    let mut text = first;
    for _ in 0..40 {
        if !text.contains("more_output: true") {
            break;
        }
        text = call(&reg, json!({"action":"read","session":&id,"wait_ms":500})).await;
    }
    assert!(text.contains("line 1999"), "the end was reached: {text}");
    assert!(text.contains("state: finished"), "{text}");
}

/// One bot cannot take every slot: the quota is per bot, with a ceiling for all together.
#[tokio::test(flavor = "multi_thread")]
async fn sessions_are_limited_per_bot_and_one_bot_cannot_starve_another() {
    let (_d, reg) = registry();
    let bot = |name: &str| ToolContext {
        bot_id: Some(name.into()),
        ..ToolContext::default()
    };
    let start = json!({"action":"start","command":fixture("sleep"),"wait_ms":0});
    for _ in 0..builtin_tools::pty_shell::MAX_SESSIONS_PER_BOT {
        try_call(&reg, &bot("a"), start.clone()).await.unwrap();
    }
    let err = try_call(&reg, &bot("a"), start.clone())
        .await
        .expect_err("bot a is at its quota");
    assert!(err.contains("already has"), "{err}");
    // Another bot is not affected.
    let other = try_call(&reg, &bot("b"), start.clone()).await.unwrap();
    // Neither can bot a see or stop it.
    let id = session_of(&other);
    assert!(
        try_call(&reg, &bot("a"), json!({"action":"kill","session":&id}))
            .await
            .is_err()
    );
    for who in ["a", "b"] {
        let listed = try_call(&reg, &bot(who), json!({"action":"list"}))
            .await
            .unwrap();
        for line in listed.lines().filter(|l| l.starts_with("pty-")) {
            let id = line.split_whitespace().next().unwrap();
            try_call(&reg, &bot(who), json!({"action":"kill","session":id}))
                .await
                .unwrap();
        }
    }
}

/// A secret the program printed in pieces (its own line wrapping, or a narrow terminal) is
/// masked as one, in a read and on the screen.
#[tokio::test(flavor = "multi_thread")]
async fn a_secret_printed_across_lines_is_masked_in_reads_and_on_the_screen() {
    let (_d, reg) = registry();
    let secret = format!("sk-{}", "abcDEF0123456789".repeat(3));
    let command = format!("{} 20 {secret}", fixture("wrapped"));
    let started = call(
        &reg,
        json!({"action":"start","command":command,"cols":20,"wait_ms":10000}),
    )
    .await;
    let id = session_of(&started);
    let seen = read_until(&reg, &id, "done>").await;
    let screen = call(&reg, json!({"action":"screen","session":&id})).await;
    for shown in [&started, &seen, &screen] {
        for piece in secret.as_bytes().chunks(20) {
            let piece = String::from_utf8_lossy(piece);
            assert!(!shown.contains(piece.as_ref()), "{piece:?} leaked: {shown}");
        }
    }
    // The command the bot sees in `list` is masked too.
    let listed = call(&reg, json!({"action":"list"})).await;
    assert!(!listed.contains("abcDEF0123456789"), "{listed}");
    call(&reg, json!({"action":"kill","session":&id})).await;
}
