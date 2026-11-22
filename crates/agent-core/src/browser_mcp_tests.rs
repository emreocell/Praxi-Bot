//! Actions inside the browser the owner connected (a browser-kind MCP server)
//! and "allow for this task" (a run-scoped grant for one MCP tool).

use std::sync::Arc;
use std::sync::atomic::Ordering;

use permission_engine::Policy;
use protocol::{Event, PermissionCategory};
use serde_json::{Value, json};
use task_runtime::RunState;
use tool_registry::{ContentClass, Effect};

use crate::tests::{Fake, calls, cfg, make_loop, profile, setup, t, text, until, with_class};
use crate::{ApprovalScope, LoopError};

/// A tool of an owner-connected browser server, classified as the MCP bridge does.
fn page_tool(name: &str) -> Arc<crate::tests::TestTool> {
    Arc::new(with_class(
        name,
        Effect::Write,
        PermissionCategory::Browser,
        ContentClass::declared(true, false, true).in_owner_browser(),
        "ok",
    ))
}

/// A tool of another MCP server whose results are marked public: untrusted, egress.
/// (Its own result is not a private read, so only the later file read adds one.)
fn generic_mcp(name: &str) -> Arc<crate::tests::TestTool> {
    Arc::new(with_class(
        name,
        Effect::Destructive,
        PermissionCategory::Accounts,
        ContentClass::declared(true, false, true),
        "ok",
    ))
}

fn use_tool(id: &str, name: &str, input: Value) -> crate::tests::Reply {
    calls(&[(id, "use_tool", json!({"name": name, "input": input}))])
}

fn approval(event: Event) -> protocol::event::ApprovalNeeded {
    let Event::ApprovalNeeded(a) = event else {
        panic!("expected an approval, got {event:?}");
    };
    a
}

#[tokio::test]
async fn page_actions_after_a_web_page_do_not_ask_on_full_but_a_private_read_brings_the_card_back()
{
    let env = setup();
    let snapshot = page_tool("mcp:pw/browser_snapshot");
    let click = page_tool("mcp:pw/browser_click");
    let evaluate = page_tool("mcp:pw/browser_evaluate");
    let navigate = page_tool("mcp:pw/browser_navigate");
    let file = Arc::new(with_class(
        "fs_read",
        Effect::Read,
        PermissionCategory::Fs,
        ContentClass::private_read(),
        "secret notes",
    ));
    let fake = Fake::new(vec![
        use_tool("c1", "mcp:pw/browser_snapshot", json!({})),
        use_tool("c2", "mcp:pw/browser_click", json!({"ref": "e1"})),
        use_tool(
            "c3",
            "mcp:pw/browser_evaluate",
            json!({"function": "() => 1"}),
        ),
        calls(&[("c4", "fs_read", json!({"path": "notes.txt"}))]),
        use_tool(
            "c5",
            "mcp:pw/browser_navigate",
            json!({"url": "https://example.com/?q=1"}),
        ),
        use_tool(
            "c6",
            "mcp:pw/browser_navigate",
            json!({"url": "https://example.com/?q=2"}),
        ),
        text("done"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[
            t(&snapshot),
            t(&click),
            t(&evaluate),
            t(&navigate),
            t(&file),
        ],
        profile(""),
        cfg(Policy::full()),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "play the reaction test").unwrap();
    // Snapshot, click and evaluate ran with no card; the navigation after the file read asks.
    let card = approval(until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await);
    assert_eq!(snapshot.count.load(Ordering::SeqCst), 1);
    assert_eq!(click.count.load(Ordering::SeqCst), 1);
    assert_eq!(evaluate.count.load(Ordering::SeqCst), 1);
    assert_eq!(navigate.count.load(Ordering::SeqCst), 0);
    let details = card.details.clone().unwrap();
    assert_eq!(details["reason_code"], "egress_browser");
    assert_eq!(details["host"], "example.com");
    assert_eq!(details["browser_action"], true);
    assert_eq!(details["run_grant"], true);
    // A standing tool grant is not offered (nor accepted) next to private data.
    assert_eq!(details["always_offered"], false);
    assert!(matches!(
        lp.approve(card.call_id, ApprovalScope::Always, env.user),
        Err(LoopError::OnceOnly(_))
    ));
    lp.approve(card.call_id, ApprovalScope::RunTool, env.user)
        .unwrap();
    // The second navigation in the same run, with nothing new read, is covered.
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(navigate.count.load(Ordering::SeqCst), 2);
    assert_eq!(env.storage.list_approvals(run).unwrap().len(), 1);
}

#[tokio::test]
async fn a_task_grant_covers_one_tool_and_stops_once_the_run_reads_something_private() {
    let env = setup();
    let page = page_tool("mcp:pw/browser_snapshot");
    let crm = generic_mcp("mcp:crm/update");
    let other = generic_mcp("mcp:crm/delete");
    let file = Arc::new(with_class(
        "fs_read",
        Effect::Read,
        PermissionCategory::Fs,
        ContentClass::private_read(),
        "secret",
    ));
    let fake = Fake::new(vec![
        use_tool("c1", "mcp:pw/browser_snapshot", json!({})),
        use_tool("c2", "mcp:crm/update", json!({"id": 1})),
        use_tool("c3", "mcp:crm/update", json!({"id": 2})),
        use_tool("c4", "mcp:crm/delete", json!({"id": 3})),
        calls(&[("c5", "fs_read", json!({"path": "a.txt"}))]),
        use_tool("c6", "mcp:crm/update", json!({"id": 4})),
        text("done"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&page), t(&crm), t(&other), t(&file)],
        profile(""),
        cfg(Policy::full()),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "update the crm").unwrap();
    // A generic MCP tool after a web page still asks (untrusted only is not enough for it).
    let first = approval(until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await);
    assert_eq!(first.tool_name, "mcp:crm/update");
    assert_eq!(first.details.as_ref().unwrap()["run_grant"], true);
    lp.approve(first.call_id, ApprovalScope::RunTool, env.user)
        .unwrap();
    // The second update ran without a card; another tool of the same server asks.
    let second = approval(until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await);
    assert_eq!(second.tool_name, "mcp:crm/delete");
    assert_eq!(crm.count.load(Ordering::SeqCst), 2);
    lp.approve(second.call_id, ApprovalScope::Once, env.user)
        .unwrap();
    // After a private read the task grant no longer covers the same tool.
    let third = approval(until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await);
    assert_eq!(third.tool_name, "mcp:crm/update");
    assert_eq!(crm.count.load(Ordering::SeqCst), 2);
    lp.cancel(run).unwrap();
}

#[tokio::test]
async fn the_task_scope_is_refused_on_a_card_that_did_not_offer_it() {
    let env = setup();
    let page = page_tool("mcp:pw/browser_snapshot");
    let shell = Arc::new(with_class(
        "shell_run",
        Effect::Destructive,
        PermissionCategory::Shell,
        ContentClass::all().executes(),
        "ran",
    ));
    let fake = Fake::new(vec![
        use_tool("c1", "mcp:pw/browser_snapshot", json!({})),
        calls(&[("c2", "shell_run", json!({"command": "ls"}))]),
        text("done"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&page), t(&shell)],
        profile(""),
        cfg(Policy::full()),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    // A shell after a web page read keeps asking, and "for this task" is not offered for it.
    let card = approval(until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await);
    assert_eq!(card.tool_name, "shell_run");
    assert_eq!(card.details.as_ref().unwrap()["run_grant"], false);
    assert!(matches!(
        lp.approve(card.call_id, ApprovalScope::RunTool, env.user),
        Err(LoopError::OnceOnly(_))
    ));
    lp.cancel(run).unwrap();
}
