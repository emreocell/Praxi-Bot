//! Approval memory: a remembered rule covers one target and nothing wider.

#![allow(clippy::assert_is_empty)]

use std::sync::Arc;
use std::sync::atomic::Ordering;

use permission_engine::Policy;
use protocol::{Event, PermissionCategory, RunId};
use serde_json::json;
use task_runtime::RunState;
use tool_registry::{ContentClass, Effect};

use crate::scoped_grant::{Scope, ScopeKind};
use crate::tests::{
    Env, Fake, TestTool, calls, cfg, make_loop, profile, setup, t, text, until, with_class,
};
use crate::{AgentLoop, ApprovalScope, GrantTarget, LoopError};

const ASK: &str = "[permissions]\npreset = \"read-and-draft\"\n";

fn tool(name: &str, effect: Effect, category: PermissionCategory) -> Arc<TestTool> {
    Arc::new(with_class(
        name,
        effect,
        category,
        ContentClass::none(),
        "ok",
    ))
}

fn scoped(tool: &str, kind: ScopeKind, pattern: &str) -> GrantTarget {
    GrantTarget::Scoped {
        tool: tool.to_owned(),
        scope: Scope {
            kind,
            pattern: pattern.to_owned(),
        },
    }
}

/// Start a turn, wait for its approval card and answer it with `scope`.
async fn approve(env: &Env, lp: &AgentLoop, scope: ApprovalScope) -> RunId {
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let Event::ApprovalNeeded(card) =
        until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
    else {
        unreachable!()
    };
    lp.approve(card.call_id, scope, env.user).unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    run
}

/// Start a turn and report whether it asked (a call that does not ask runs to the end).
async fn asks(env: &Env, lp: &AgentLoop) -> bool {
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let done = lp.wait(run);
    tokio::pin!(done);
    tokio::select! {
        _ = until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))) => {
            lp.cancel(run).unwrap();
            let _ = done.await;
            true
        }
        state = &mut done => {
            assert_eq!(state.unwrap(), RunState::Succeeded);
            false
        }
    }
}

#[tokio::test]
async fn a_folder_rule_covers_that_folder_only_survives_restart_and_is_revocable() {
    let env = setup();
    let write = tool("fs_write", Effect::Write, PermissionCategory::Fs);
    let paths = ["docs/a.md", "docs/sub/b.md", "src/c.rs"];
    let script = paths
        .iter()
        .enumerate()
        .flat_map(|(i, p)| {
            [
                calls(&[(&format!("c{i}"), "fs_write", json!({"path": p}))]),
                text("ok"),
            ]
        })
        .collect::<Vec<_>>();
    let fake = Fake::new(script);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&write)],
        profile(ASK),
        cfg(Policy::approve_destructive()),
    );
    approve(&env, &lp, ApprovalScope::Always).await;
    let rule = scoped("fs_write", ScopeKind::Folder, "docs");
    assert_eq!(
        lp.list_grants(env.bot)
            .iter()
            .map(|g| &g.target)
            .collect::<Vec<_>>(),
        vec![&rule]
    );
    // Below the folder: no card. A sibling folder: a card.
    assert!(!asks(&env, &lp).await);
    assert!(asks(&env, &lp).await);
    assert_eq!(write.count.load(Ordering::SeqCst), 2);

    // Durable: a fresh loop on the same storage still has the rule; revoking removes it.
    let fake2 = Fake::new(vec![
        calls(&[("c9", "fs_write", json!({"path": "docs/z.md"}))]),
        text("ok"),
    ]);
    let lp2 = make_loop(
        &env.storage,
        env.bot,
        &fake2,
        &[t(&write)],
        profile(ASK),
        cfg(Policy::approve_destructive()),
    );
    assert_eq!(lp2.list_grants(env.bot).len(), 1);
    assert!(lp2.revoke_grant(env.bot, &rule).unwrap());
    assert!(lp2.list_grants(env.bot).is_empty());
}

#[tokio::test]
async fn a_command_rule_is_the_exact_command_and_a_compound_command_is_once_only() {
    let env = setup();
    let shell = tool("shell_run", Effect::Destructive, PermissionCategory::Shell);
    let fake = Fake::new(vec![
        calls(&[("c1", "shell_run", json!({"command": "git status -s"}))]),
        text("ok"),
        calls(&[("c2", "shell_run", json!({"command": "git status --branch"}))]),
        text("ok"),
        // A run cancelled at its card never reaches its closing text.
        calls(&[("c3", "shell_run", json!({"command": "git push"}))]),
        calls(&[(
            "c4",
            "shell_run",
            json!({"command": "git status; rm -rf x"}),
        )]),
        text("ok"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&shell)],
        profile(ASK),
        cfg(Policy::approve_destructive()),
    );
    approve(&env, &lp, ApprovalScope::Always).await;
    assert_eq!(
        lp.list_grants(env.bot)[0].target,
        scoped("shell_run", ScopeKind::Command, "git status")
    );
    assert!(!asks(&env, &lp).await, "same program and sub-command");
    assert!(asks(&env, &lp).await, "another sub-command");
    // A compound command is approved once: the daemon refuses a standing choice for it.
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let Event::ApprovalNeeded(card) =
        until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
    else {
        unreachable!()
    };
    let details = card.details.clone().unwrap();
    assert_eq!(details["once_only"], true);
    assert_eq!(details["risk"], "high", "a shell command is desktop-only");
    assert!(details.get("memory").is_none());
    assert!(matches!(
        lp.approve(card.call_id, ApprovalScope::Always, env.user),
        Err(LoopError::OnceOnly(_))
    ));
    lp.cancel(run).unwrap();
    let _ = lp.wait(run).await;
}

#[tokio::test]
async fn a_remembered_command_does_not_waive_the_untrusted_content_gate() {
    let env = setup();
    let shell = Arc::new(with_class(
        "shell_run",
        Effect::Destructive,
        PermissionCategory::Shell,
        ContentClass::all().executes(),
        "ok",
    ));
    let page = Arc::new(with_class(
        "web_read",
        Effect::Read,
        PermissionCategory::Browser,
        ContentClass::untrusted_private(),
        "page",
    ));
    let fake = Fake::new(vec![
        calls(&[("c1", "shell_run", json!({"command": "ls"}))]),
        text("ok"),
        calls(&[("c2", "web_read", json!({"url": "https://a.example"}))]),
        calls(&[("c3", "shell_run", json!({"command": "ls"}))]),
        text("ok"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&shell), t(&page)],
        profile(ASK),
        cfg(Policy::approve_destructive()),
    );
    approve(&env, &lp, ApprovalScope::Always).await;
    // The rule exists, but a run that read a page asks before starting any program.
    assert!(asks(&env, &lp).await);
    assert_eq!(shell.count.load(Ordering::SeqCst), 1);
}
