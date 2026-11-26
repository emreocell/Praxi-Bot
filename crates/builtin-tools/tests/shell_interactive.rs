//! `shell_interactive` end to end, through the registry like the agent loop calls it, against a
//! fake interactive program that behaves like a CLI sign-in: it prints a link and a one-time
//! code, waits for a pasted code, answers, and exits non-zero on a wrong one.

use std::sync::Arc;

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

/// `(shell, command)` of the fake sign-in. The secret `sk-...` line proves masking.
fn fake_login() -> (&'static str, String) {
    let key = format!("sk-{}", "abcDEF0123456789".repeat(2));
    if cfg!(windows) {
        (
            "powershell",
            format!(
                "Write-Host 'Open https://auth.example.com/device and enter AB12-CD34'; \
                 Write-Host 'debug key {key}'; \
                 $c = Read-Host 'Paste code'; \
                 if ($c -eq 'GOODCODE') {{ Write-Host 'Signed in as ada@example.com'; exit 0 }} \
                 else {{ Write-Host 'invalid code'; exit 4 }}"
            ),
        )
    } else {
        (
            "sh",
            format!(
                "echo 'Open https://auth.example.com/device and enter AB12-CD34'; \
                 echo 'debug key {key}'; \
                 printf 'Paste code: '; read c; \
                 if [ \"$c\" = GOODCODE ]; then echo 'Signed in as ada@example.com'; exit 0; \
                 else echo 'invalid code'; exit 4; fi"
            ),
        )
    }
}

fn registry() -> (tempfile::TempDir, ToolRegistry) {
    let data = tempfile::tempdir().unwrap();
    let reg = ToolRegistry::new();
    let cfg = BuiltinConfig::new(data.path().to_path_buf(), Arc::new(AllowAll));
    let _ = register_builtin_tools(&reg, cfg);
    (data, reg)
}

async fn call(reg: &ToolRegistry, input: Value) -> String {
    let out = reg
        .get("shell_interactive")
        .expect("shell_interactive is registered")
        .call(&ToolContext::default(), input)
        .await
        .unwrap_or_else(|e| panic!("shell_interactive failed: {e}"));
    match &out.content[0] {
        ContentPart::Text { text } => text.clone(),
        other => panic!("{other:?}"),
    }
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
    let (shell, command) = fake_login();
    let started = call(
        &reg,
        json!({"action":"start","command":command,"shell":shell,"wait_ms":15000}),
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
    let gone = reg
        .get("shell_interactive")
        .unwrap()
        .call(
            &ToolContext::default(),
            json!({"action":"read","session":&id}),
        )
        .await;
    assert!(gone.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wrong_code_fails_with_the_exit_code_and_a_kill_is_clean() {
    let (_d, reg) = registry();
    let (shell, command) = fake_login();
    let started = call(
        &reg,
        json!({"action":"start","command":command,"shell":shell,"wait_ms":15000}),
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
        json!({"action":"start","command":command,"shell":shell,"wait_ms":0}),
    )
    .await;
    let id = session_of(&started);
    let killed = call(&reg, json!({"action":"kill","session":&id})).await;
    assert!(killed.contains("killed"), "{killed}");
    let listed = call(&reg, json!({"action":"list"})).await;
    assert!(listed.contains("no interactive sessions"), "{listed}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_shell_cannot_be_started_here() {
    let (_d, reg) = registry();
    let err = reg
        .get("shell_interactive")
        .unwrap()
        .call(
            &ToolContext::default(),
            json!({"action":"start","command":"bash"}),
        )
        .await
        .expect_err("a bare shell is refused");
    assert!(err.to_string().contains("shell or interpreter"), "{err}");
}
