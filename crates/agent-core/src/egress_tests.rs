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

use crate::taint::{self, ContentTaint};
use crate::tests::{Fake, cfg, profile, setup, t, text, tool_messages, until, with_class};
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

#[tokio::test]
async fn a_result_that_looks_like_a_denial_still_taints_when_the_tool_ran() {
    let env = setup();
    let read = Arc::new(with_class(
        "fs_read",
        Effect::Read,
        PermissionCategory::Fs,
        ContentClass::untrusted_private(),
        "Permission denied: the notes start with SECRET",
    ));
    let search = egress_tool(
        "web_search",
        Effect::Read,
        PermissionCategory::Browser,
        ContentClass::untrusted_egress(),
    );
    let fake = Fake::new(vec![
        calls(&[("c1", "fs_read", json!({"path": "notes.txt"}))]),
        calls(&[("c2", "web_search", json!({"query": "q"}))]),
        text("done"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&read), t(&search)],
        profile("[tools]\nallow = [\"fs_read\", \"web_search\"]\n"),
        cfg(Policy::full()),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "read").unwrap();
    let _ = until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await;
    assert_eq!(read.count.load(Ordering::SeqCst), 1);
    assert_eq!(search.count.load(Ordering::SeqCst), 0);
    let steps = env.storage.list_steps(run).unwrap();
    let tools = steps.iter().find(|s| s.name == "tools").unwrap();
    let fact = &tools.output.as_ref().unwrap()["results"][0]["fact"];
    assert_eq!(fact["ran"], true, "{fact}");
    assert_eq!(fact["reads_private"], true, "{fact}");
    lp.cancel(run).unwrap();
}

#[test]
fn reconstruction_uses_the_stored_fact_and_not_the_result_text() {
    let mut tainted = ContentTaint::default();
    taint::observe_history_step(
        &mut tainted,
        "tools",
        &json!({}),
        Some(&json!({
            "results": [{
                "text": "Permission denied",
                "is_error": false,
                "fact": {
                    "ran": true,
                    "ingests_untrusted": true,
                    "reads_private": true,
                    "label": "file notes.txt"
                }
            }]
        })),
    );
    assert!(tainted.exposure.private);
    assert_eq!(tainted.sources(), &["file notes.txt".to_owned()]);

    let mut skipped = ContentTaint::default();
    taint::observe_history_step(
        &mut skipped,
        "tools",
        &json!({}),
        Some(&json!({
            "results": [{
                "text": "the secret is SECRET",
                "is_error": false,
                "fact": { "ran": false }
            }]
        })),
    );
    assert!(!skipped.exposure.is_tainted());

    let mut missing = ContentTaint::default();
    taint::observe_history_step(
        &mut missing,
        "tools",
        &json!({}),
        Some(&json!({
            "results": [{ "text": "Error: unknown tool fs_read", "is_error": true }]
        })),
    );
    assert!(!missing.exposure.is_tainted());

    let mut from_model = ContentTaint::default();
    taint::observe_history_step(
        &mut from_model,
        "model",
        &json!({}),
        Some(&json!({
            "text": "ok",
            "executed": [{
                "ran": true,
                "reads_private": true,
                "label": "file secret.txt"
            }]
        })),
    );
    assert!(from_model.exposure.private);
}

#[tokio::test]
async fn a_later_turn_stays_gated_while_the_tainted_answer_is_in_context() {
    let env = setup();
    let read = read_tool("fs_read", ContentClass::untrusted_private());
    let search = egress_tool(
        "web_search",
        Effect::Read,
        PermissionCategory::Browser,
        ContentClass::untrusted_egress(),
    );
    let sentinel = "PRIVATE_SENTINEL";
    let fake = Fake::new(vec![
        calls(&[("c1", "fs_read", json!({"path": "notes.txt"}))]),
        text(&format!("the file says {sentinel}")),
        calls(&[("c2", "web_search", json!({"query": "q"}))]),
        text("done"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&read), t(&search)],
        profile(""),
        cfg(Policy::full()),
    );
    let first = lp.run_turn(env.chat, "read the notes").unwrap();
    assert_eq!(lp.wait(first).await.unwrap(), RunState::Succeeded);
    let assistant = env
        .storage
        .list_messages(env.chat)
        .unwrap()
        .into_iter()
        .find(|m| m.role == "assistant")
        .unwrap();
    assert!(assistant.exposure_private);
    assert!(assistant.content.contains(sentinel));
    assert_eq!(search.count.load(Ordering::SeqCst), 0);

    let mut rx = lp.subscribe();
    let second = lp.run_turn(env.chat, "search for it").unwrap();
    let _ = until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await;
    assert_eq!(read.count.load(Ordering::SeqCst), 1);
    assert_eq!(search.count.load(Ordering::SeqCst), 0);
    lp.cancel(second).unwrap();
}

#[tokio::test]
async fn a_tainted_message_outside_the_history_bound_does_not_gate_the_next_turn() {
    let env = setup();
    let read = read_tool("fs_read", ContentClass::untrusted_private());
    let search = egress_tool(
        "web_search",
        Effect::Read,
        PermissionCategory::Browser,
        ContentClass::untrusted_egress(),
    );
    let fake = Fake::new(vec![
        calls(&[("c1", "fs_read", json!({"path": "notes.txt"}))]),
        text("the file says PRIVATE_SENTINEL"),
        calls(&[("c2", "web_search", json!({"query": "q"}))]),
        text("done"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&read), t(&search)],
        profile(""),
        cfg(Policy::full()),
    );
    let first = lp.run_turn(env.chat, "read the notes").unwrap();
    assert_eq!(lp.wait(first).await.unwrap(), RunState::Succeeded);
    // Newer messages fill the history budget, so the tainted answer is not
    // placed in the next prompt. Its bits stay on the row and do not apply.
    let stamp = storage::now_ms() + 60_000;
    env.storage
        .insert_message(&storage::Message {
            id: uuid::Uuid::now_v7(),
            user_id: env.user,
            chat_id: env.chat,
            run_id: None,
            role: "user".into(),
            content: "z".repeat(65_500),
            created_at_ms: stamp,
            exposure_untrusted: false,
            exposure_private: false,
            exposure_sources: Vec::new(),
        })
        .unwrap();
    env.storage
        .insert_message(&storage::Message {
            id: uuid::Uuid::now_v7(),
            user_id: env.user,
            chat_id: env.chat,
            run_id: None,
            role: "assistant".into(),
            content: "clean reply".into(),
            created_at_ms: stamp + 1,
            exposure_untrusted: false,
            exposure_private: false,
            exposure_sources: Vec::new(),
        })
        .unwrap();
    let second = lp.run_turn(env.chat, "search").unwrap();
    let state = tokio::time::timeout(Duration::from_secs(8), lp.wait(second))
        .await
        .expect("the search asked even though the tainted answer is outside the bound")
        .unwrap();
    assert_eq!(state, RunState::Succeeded);
    assert_eq!(search.count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn external_metadata_is_untrusted_text_and_does_not_gate_web_search() {
    let env = setup();
    let leak = Arc::new(with_class(
        "listPets",
        Effect::Read,
        PermissionCategory::Fs,
        ContentClass::declared(true, true, false).with_external_metadata(),
        "remote schema",
    ));
    let search = egress_tool(
        "web_search",
        Effect::Read,
        PermissionCategory::Browser,
        ContentClass::untrusted_egress(),
    );
    let fake = Fake::new(vec![
        calls(&[("c1", "web_search", json!({"query": "q"}))]),
        text("done"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&leak), t(&search)],
        profile(""),
        cfg(Policy::full()),
    );
    let run = lp.run_turn(env.chat, "search").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(
        search.count.load(Ordering::SeqCst),
        1,
        "a remote description is not a private read, so web_search stays prompt-free"
    );
    let model = env
        .storage
        .list_steps(run)
        .unwrap()
        .into_iter()
        .find(|s| s.name == "model")
        .unwrap();
    let fact = &model.output.as_ref().unwrap()["metadata"][0];
    assert_eq!(fact["reads_private"], false, "{fact}");
    assert_eq!(fact["ingests_untrusted"], true, "{fact}");
}

#[tokio::test]
async fn discovery_output_exposure_taints_the_following_egress() {
    let env = setup();
    let mut finder = with_class(
        "find_tool",
        Effect::Read,
        PermissionCategory::Meta,
        ContentClass::none(),
        "mcp:evil/leak description",
    );
    finder.exposure = Some(tool_registry::OutputExposure {
        ingests_untrusted: true,
        reads_private: true,
        label: "mcp evil".into(),
    });
    let finder = Arc::new(finder);
    let search = egress_tool(
        "web_search",
        Effect::Read,
        PermissionCategory::Browser,
        ContentClass::untrusted_egress(),
    );
    let fake = Fake::new(vec![
        calls(&[("c1", "find_tool", json!({"query": "leak"}))]),
        calls(&[("c2", "web_search", json!({"query": "q"}))]),
        text("done"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&finder), t(&search)],
        profile(""),
        cfg(Policy::full()),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "find then search").unwrap();
    let card = card_of(&until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await);
    assert!(
        card["reason"].as_str().unwrap().contains("mcp evil"),
        "{card}"
    );
    assert_eq!(finder.count.load(Ordering::SeqCst), 1);
    assert_eq!(search.count.load(Ordering::SeqCst), 0);
    lp.cancel(run).unwrap();
}

#[tokio::test]
async fn the_email_card_lists_every_recipient_and_search_lists_language() {
    let env = setup();
    let page = read_tool("fs_read", ContentClass::untrusted_private());
    let send = egress_tool(
        "email_send",
        Effect::Write,
        PermissionCategory::Messaging,
        ContentClass::egress(),
    );
    let search = egress_tool(
        "web_search",
        Effect::Read,
        PermissionCategory::Browser,
        ContentClass::untrusted_egress(),
    );
    let fake = Fake::new(vec![
        calls(&[("c1", "fs_read", json!({"path": "notes.txt"}))]),
        calls(&[(
            "c2",
            "email_send",
            json!({
                "to": ["Ada <ada@example.com>", "Bea <bea@other.test>"],
                "cc": ["Cy <cy@example.com>"],
                "bcc": ["Dee <dee@hidden.test>"],
                "subject": "hi",
                "body": "secret body",
                "account": "work"
            }),
        )]),
        text("done"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&page), t(&send), t(&search)],
        profile(""),
        cfg(Policy::full()),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "mail").unwrap();
    let card = card_of(&until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await);
    let payload = card["payload"].as_str().unwrap();
    for addr in [
        "ada@example.com",
        "bea@other.test",
        "cy@example.com",
        "dee@hidden.test",
    ] {
        assert!(payload.contains(addr), "{payload}");
    }
    assert!(payload.contains("secret body"), "{payload}");
    assert!(payload.contains("Account: work"), "{payload}");
    let host = card["host"].as_str().unwrap();
    assert!(host.contains("example.com"), "{host}");
    assert!(host.contains("other.test"), "{host}");
    assert!(host.contains("hidden.test"), "{host}");
    lp.cancel(run).unwrap();

    let env = setup();
    let page = read_tool("fs_read", ContentClass::untrusted_private());
    let search = egress_tool(
        "web_search",
        Effect::Read,
        PermissionCategory::Browser,
        ContentClass::untrusted_egress(),
    );
    let fake = Fake::new(vec![
        calls(&[("c1", "fs_read", json!({"path": "notes.txt"}))]),
        calls(&[(
            "c2",
            "web_search",
            json!({"query": "weather", "language": "tr", "limit": 3}),
        )]),
        text("done"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&page), t(&search)],
        profile(""),
        cfg(Policy::full()),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "search").unwrap();
    let card = card_of(&until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await);
    let payload = card["payload"].as_str().unwrap();
    assert!(payload.contains("weather"), "{payload}");
    assert!(payload.contains("language: tr"), "{payload}");
    assert!(payload.contains("limit: 3"), "{payload}");
    lp.cancel(run).unwrap();
}

#[tokio::test]
async fn a_secret_in_the_egress_payload_is_refused_before_approval() {
    let env = setup();
    let read = read_tool("fs_read", ContentClass::untrusted_private());
    let search = egress_tool(
        "web_search",
        Effect::Read,
        PermissionCategory::Browser,
        ContentClass::untrusted_egress(),
    );
    let secret = "sk-abcdefghijklmnopqrstuvwxyz0123";
    let fake = Fake::new(vec![
        calls(&[("c1", "fs_read", json!({"path": "notes.txt"}))]),
        calls(&[("c2", "web_search", json!({"query": secret}))]),
        text("ok"),
    ]);
    let mut config = cfg(Policy::full());
    config.redactor = Some(secret_store::Redactor::new().into());
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&read), t(&search)],
        profile(""),
        config,
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(search.count.load(Ordering::SeqCst), 0);
    let msg = tool_messages(&fake.request(2)).join("\n");
    assert!(msg.contains("looks like a secret"), "{msg}");
    assert!(!msg.contains(secret));
    while let Ok(item) = rx.try_recv() {
        assert!(!matches!(item.envelope.event, Event::ApprovalNeeded(_)));
    }
}

#[tokio::test]
async fn hidden_characters_in_an_egress_payload_are_refused_and_not_sent() {
    let env = setup();
    let read = read_tool("fs_read", ContentClass::untrusted_private());
    let search = egress_tool(
        "web_search",
        Effect::Read,
        PermissionCategory::Browser,
        ContentClass::untrusted_egress(),
    );
    let query = format!("weather{}", '\u{E0041}');
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
        profile(""),
        cfg(Policy::full()),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "search").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(search.count.load(Ordering::SeqCst), 0);
    let msg = tool_messages(&fake.request(2)).join("\n");
    assert!(msg.contains("hidden or control characters"), "{msg}");
    assert!(!msg.contains('\u{E0041}'), "{msg}");
    while let Ok(item) = rx.try_recv() {
        assert!(!matches!(item.envelope.event, Event::ApprovalNeeded(_)));
    }
}

#[tokio::test]
async fn external_metadata_still_gates_a_fetch() {
    let env = setup();
    let leak = Arc::new(with_class(
        "listPets",
        Effect::Read,
        PermissionCategory::Fs,
        ContentClass::declared(true, true, false).with_external_metadata(),
        "remote schema",
    ));
    let fetch = egress_tool(
        "web_extract",
        Effect::Write,
        PermissionCategory::Browser,
        ContentClass::untrusted_egress(),
    );
    let fake = Fake::new(vec![
        calls(&[(
            "c1",
            "web_extract",
            json!({"url": "https://evil.example/hook"}),
        )]),
        text("done"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&leak), t(&fetch)],
        profile(""),
        cfg(Policy::full()),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "fetch").unwrap();
    let card = card_of(&until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await);
    assert!(
        card["reason"].as_str().unwrap().contains("listPets"),
        "{card}"
    );
    assert_eq!(fetch.count.load(Ordering::SeqCst), 0);
    lp.cancel(run).unwrap();
}

#[tokio::test]
async fn a_click_or_scroll_after_a_private_read_does_not_ask_but_typing_does() {
    let env = setup();
    let read = read_tool("desktop_get_state", ContentClass::untrusted_private());
    let click = Arc::new(with_class(
        "desktop_click",
        Effect::Write,
        PermissionCategory::Desktop,
        builtin_tools::classify("desktop_click").unwrap(),
        "clicked",
    ));
    let scroll = Arc::new(with_class(
        "browser_scroll",
        Effect::Read,
        PermissionCategory::Browser,
        builtin_tools::classify("browser_scroll").unwrap(),
        "scrolled",
    ));
    let bare_click = Arc::new(with_class(
        "browser_click",
        Effect::Write,
        PermissionCategory::Browser,
        ContentClass::all(),
        "clicked",
    ));
    let typed = Arc::new(with_class(
        "desktop_type",
        Effect::Write,
        PermissionCategory::Desktop,
        builtin_tools::classify("desktop_type").unwrap(),
        "typed",
    ));
    let fake = Fake::new(vec![
        calls(&[("c1", "desktop_get_state", json!({}))]),
        calls(&[("c2", "desktop_click", json!({"index": 1}))]),
        calls(&[("c3", "browser_scroll", json!({"dy": 400}))]),
        calls(&[("c4", "browser_click", json!({"ref": "e1"}))]),
        calls(&[("c5", "desktop_type", json!({"text": "hello"}))]),
        text("done"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&read), t(&click), t(&scroll), t(&bare_click), t(&typed)],
        profile(""),
        cfg(Policy::full()),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "look and act").unwrap();
    let _card = card_of(&until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await);
    assert_eq!(click.count.load(Ordering::SeqCst), 1);
    assert_eq!(scroll.count.load(Ordering::SeqCst), 1);
    assert_eq!(bare_click.count.load(Ordering::SeqCst), 1);
    assert_eq!(typed.count.load(Ordering::SeqCst), 0);
    lp.cancel(run).unwrap();
}

#[tokio::test]
async fn browser_click_prompt_text_still_asks_after_a_read() {
    let env = setup();
    let read = read_tool("fs_read", ContentClass::untrusted_private());
    let click = Arc::new(with_class(
        "browser_click",
        Effect::Write,
        PermissionCategory::Browser,
        ContentClass::all(),
        "clicked",
    ));
    let fake = Fake::new(vec![
        calls(&[("c1", "fs_read", json!({"path": "notes.txt"}))]),
        calls(&[(
            "c2",
            "browser_click",
            json!({"ref": "e1", "prompt_text": "SECRET_DATA"}),
        )]),
        text("done"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&read), t(&click)],
        profile(""),
        cfg(Policy::full()),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "click").unwrap();
    let card = card_of(&until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await);
    let payload = card["payload"].as_str().unwrap();
    assert!(payload.contains("SECRET_DATA"), "{payload}");
    assert!(payload.contains("prompt_text"), "{payload}");
    assert_eq!(click.count.load(Ordering::SeqCst), 0);
    lp.cancel(run).unwrap();
}

#[tokio::test]
async fn a_file_write_does_not_count_as_a_private_read() {
    let env = setup();
    let write = Arc::new(with_class(
        "fs_write",
        Effect::Write,
        PermissionCategory::Fs,
        builtin_tools::classify("fs_write").unwrap(),
        "wrote",
    ));
    let search = egress_tool(
        "web_search",
        Effect::Read,
        PermissionCategory::Browser,
        ContentClass::untrusted_egress(),
    );
    let fake = Fake::new(vec![
        calls(&[(
            "c1",
            "fs_write",
            json!({"path": "notes.txt", "content": "hi"}),
        )]),
        calls(&[("c2", "web_search", json!({"query": "weather"}))]),
        text("done"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&write), t(&search)],
        profile(""),
        cfg(Policy::full()),
    );
    let state = tokio::time::timeout(
        Duration::from_secs(8),
        lp.wait(lp.run_turn(env.chat, "write then search").unwrap()),
    )
    .await
    .expect("a write tainted the run, so the search asked")
    .unwrap();
    assert_eq!(state, RunState::Succeeded);
    assert_eq!(search.count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_later_scheduled_firing_does_not_inherit_taint_from_its_own_history() {
    let env = setup();
    let read = read_tool("fs_read", ContentClass::untrusted_private());
    let search = egress_tool(
        "web_search",
        Effect::Read,
        PermissionCategory::Browser,
        ContentClass::untrusted_egress(),
    );
    let fake = Fake::new(vec![
        calls(&[("c1", "fs_read", json!({"path": "notes.txt"}))]),
        text("day one PRIVATE_SENTINEL"),
        calls(&[("c2", "web_search", json!({"query": "weather"}))]),
        text("day two"),
        calls(&[("c3", "fs_read", json!({"path": "mail.txt"}))]),
        text("human saw the mailbox"),
        calls(&[("c4", "web_search", json!({"query": "leak"}))]),
        text("unused"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&read), t(&search)],
        profile(""),
        cfg(Policy::full()),
    );
    let sid = protocol::TaskId::new();
    let day1 = lp
        .run_scheduled(env.chat, sid, ScheduleWork::Prompt("read the notes".into()))
        .unwrap();
    assert_eq!(lp.wait(day1).await.unwrap(), RunState::Succeeded);
    let day2 = lp
        .run_scheduled(
            env.chat,
            sid,
            ScheduleWork::Prompt("search the weather".into()),
        )
        .unwrap();
    let state = tokio::time::timeout(Duration::from_secs(8), lp.wait(day2))
        .await
        .expect("day 2 asked because it inherited its own history")
        .unwrap();
    assert_eq!(state, RunState::Succeeded);
    assert_eq!(search.count.load(Ordering::SeqCst), 1);
    let seen = fake
        .request(2)
        .messages
        .iter()
        .map(|m| m.content.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !seen.contains("PRIVATE_SENTINEL"),
        "yesterday's answer is back in the prompt: {seen}"
    );

    let human = lp.run_turn(env.chat, "read the mail").unwrap();
    assert_eq!(lp.wait(human).await.unwrap(), RunState::Succeeded);
    let mut rx = lp.subscribe();
    let day3 = lp
        .run_scheduled(env.chat, sid, ScheduleWork::Prompt("search again".into()))
        .unwrap();
    let card = card_of(&until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await);
    assert_eq!(card["kind"], "egress");
    assert_eq!(search.count.load(Ordering::SeqCst), 1);
    lp.cancel(day3).unwrap();
}

#[tokio::test]
async fn a_no_tool_turn_that_can_see_tainted_history_is_untrusted_memory() {
    let env = setup();
    let read = read_tool("fs_read", ContentClass::untrusted_private());
    let fake = Fake::new(vec![
        calls(&[("c1", "fs_read", json!({"path": "notes.txt"}))]),
        text("the file says PRIVATE"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&read)],
        profile(""),
        cfg(Policy::full()),
    );
    let first = lp.run_turn(env.chat, "read the notes").unwrap();
    assert_eq!(lp.wait(first).await.unwrap(), RunState::Succeeded);
    let run = storage::Run {
        id: protocol::RunId::new(),
        user_id: env.user,
        bot_id: env.bot,
        chat_id: Some(env.chat),
        kind: "turn".into(),
        state: "running".into(),
        error: None,
        created_at_ms: storage::now_ms(),
        updated_at_ms: storage::now_ms(),
        parent_run_id: None,
        spec: None,
    };
    env.storage.insert_run(&run).unwrap();
    env.storage
        .insert_message(&storage::Message {
            id: uuid::Uuid::now_v7(),
            user_id: env.user,
            chat_id: env.chat,
            run_id: Some(run.id),
            role: "user".into(),
            content: "what did you find?".into(),
            created_at_ms: storage::now_ms(),
            exposure_untrusted: false,
            exposure_private: false,
            exposure_sources: Vec::new(),
        })
        .unwrap();
    let ex = crate::memory::Exchange::of(&lp.sh, &run, "it said PRIVATE").unwrap();
    assert!(
        ex.untrusted,
        "a turn that can see a tainted answer must not be stored as user trust"
    );
}
