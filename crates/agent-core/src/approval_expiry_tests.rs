//! A pending card never outlives what it waits for: it expires with its timeout, with the tool
//! around it and with its run, and the run does not stay in `waiting_approval` afterwards.

#![allow(clippy::too_many_lines)]

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use async_trait::async_trait;
use builtin_tools::{GateRequest, ToolGate};
use permission_engine::Policy;
use protocol::{Event, PermissionCategory};
use serde_json::{Value, json};
use task_runtime::RunState;
use tool_registry::{
    ContentClass, Effect, Platform, Tool, ToolContext, ToolError, ToolOutput, ToolSpec,
};

use crate::approval::LoopGate;
use crate::tests::{Fake, calls, cfg, make_loop, profile, setup, text, tool, until};
use crate::{AgentLoop, ApprovalScope};

/// A tool that asks the user from inside its own call (as `script_run` and the replay tools do),
/// optionally giving up on the answer after `give_up_after` (a tool with its own deadline).
struct InnerAsk {
    spec: ToolSpec,
    gate: LoopGate,
    give_up_after: Option<Duration>,
}

#[async_trait]
impl Tool for InnerAsk {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolContext, _: Value) -> Result<ToolOutput, ToolError> {
        let req = GateRequest {
            run_id: ctx.run_id.clone(),
            tool: "t.write".to_owned(),
            category: PermissionCategory::Fs,
            effect: Effect::Write,
            raw_command: String::new(),
            input: json!({}),
            content: ContentClass::none(),
            control: None,
        };
        let asked = self.gate.authorize(&req);
        let answer = match self.give_up_after {
            Some(limit) => tokio::time::timeout(limit, asked)
                .await
                .unwrap_or_else(|_| Err("gave up waiting".into())),
            None => asked.await,
        };
        match answer {
            Ok(()) => Ok(ToolOutput::text("inner call allowed")),
            Err(why) => Ok(ToolOutput::text(format!("inner call refused: {why}"))),
        }
    }
}

fn inner_ask(lp: &AgentLoop, give_up_after: Option<Duration>) {
    lp.sh.tools.register(Arc::new(InnerAsk {
        spec: ToolSpec {
            name: "inner_ask".into(),
            description: "asks from inside".into(),
            input_schema: json!({"type": "object"}),
            effect: Effect::Read,
            category: PermissionCategory::Fs,
            platforms: vec![Platform::All],
            content: ContentClass::none(),
        },
        gate: LoopGate {
            sh: Arc::downgrade(&lp.sh),
        },
        give_up_after,
    }) as Arc<dyn Tool>);
}

fn drain(rx: &mut tokio::sync::broadcast::Receiver<crate::StreamItem>) -> Vec<Event> {
    let mut out = Vec::new();
    while let Ok(i) = rx.try_recv() {
        out.push(i.envelope.event);
    }
    out
}

#[tokio::test(start_paused = true)]
async fn an_unanswered_approval_expires_and_the_run_carries_on() {
    let env = setup();
    let w = Arc::new(tool("t.write", Effect::Write, "written"));
    let fake = Fake::new(vec![
        calls(&[("c1", "t.write", json!({}))]),
        text("moved on"),
    ]);
    let mut config = cfg(Policy::approve_destructive());
    config.approval_timeout = Duration::from_millis(150);
    let lp = make_loop(&env.storage, env.bot, &fake, &[&w], profile(""), config);
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await;
    let state = tokio::time::timeout(Duration::from_secs(10), lp.wait(run))
        .await
        .expect("the approval timeout must end the wait")
        .unwrap();
    assert_eq!(state, RunState::Succeeded, "the run carried on");
    assert_eq!(w.count.load(Ordering::SeqCst), 0, "never ran unapproved");
    assert_eq!(
        env.storage.list_approvals(run).unwrap()[0]
            .decision
            .as_deref(),
        Some("expired")
    );
    let denied = drain(&mut rx).into_iter().find_map(|e| match e {
        Event::ToolCallDenied(d) => d.reason,
        _ => None,
    });
    assert!(
        denied.is_some_and(|r| r.starts_with("expired")),
        "the card closes as expired"
    );
    assert!(lp.sh.pending.lock().is_empty() && lp.sh.parked.lock().is_empty());
}

#[tokio::test]
async fn a_stopped_run_leaves_no_pending_approval_behind() {
    let env = setup();
    let w = Arc::new(tool("t.write", Effect::Write, "written"));
    let fake = Fake::new(vec![calls(&[("c1", "t.write", json!({}))]), text("never")]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[&w],
        profile(""),
        cfg(Policy::approve_destructive()),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await;
    lp.cancel(run).unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Cancelled);
    assert_eq!(
        env.storage.list_approvals(run).unwrap()[0]
            .decision
            .as_deref(),
        Some("denied"),
        "Stop is the owner saying no, not a timeout"
    );
    assert!(lp.sh.pending.lock().is_empty() && lp.sh.parked.lock().is_empty());
}

#[tokio::test]
async fn the_tool_clock_does_not_run_while_its_inner_call_waits_for_the_user() {
    let env = setup();
    let w = Arc::new(tool("t.write", Effect::Write, "written"));
    let fake = Fake::new(vec![calls(&[("c1", "inner_ask", json!({}))]), text("done")]);
    let mut config = cfg(Policy::approve_destructive());
    config.guards.tool_timeout = Duration::from_millis(150);
    let lp = make_loop(&env.storage, env.bot, &fake, &[&w], profile(""), config);
    inner_ask(&lp, None);
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let Event::ApprovalNeeded(card) =
        until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
    else {
        unreachable!()
    };
    // Well past the tool timeout: the card is still the thing the run waits for.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(lp.runtime().state(run).unwrap(), RunState::WaitingApproval);
    lp.approve(card.call_id, ApprovalScope::Once, env.user)
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    let timed_out = drain(&mut rx).into_iter().any(|e| {
        matches!(e, Event::ToolCallCompleted(c)
            if c.error.as_deref().is_some_and(|m| m.contains("timed out")))
    });
    assert!(!timed_out, "waiting for the user is not a tool timeout");
    assert_eq!(
        w.count.load(Ordering::SeqCst),
        0,
        "the inner call is only an authorisation"
    );
}

#[tokio::test]
async fn an_abandoned_inner_wait_closes_its_card_and_frees_the_run() {
    let env = setup();
    let w = Arc::new(tool("t.write", Effect::Write, "written"));
    let fake = Fake::new(vec![calls(&[("c1", "inner_ask", json!({}))]), text("done")]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[&w],
        profile(""),
        cfg(Policy::approve_destructive()),
    );
    // The tool stops waiting after 100 ms (its own deadline): the wait future is dropped.
    inner_ask(&lp, Some(Duration::from_millis(100)));
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let Event::ApprovalNeeded(card) =
        until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
    else {
        unreachable!()
    };
    let expired = until(&mut rx, |e| matches!(e, Event::ToolCallDenied(_))).await;
    let Event::ToolCallDenied(denied) = expired else {
        unreachable!()
    };
    assert_eq!(denied.call_id, card.call_id);
    assert!(denied.reason.is_some_and(|r| r.starts_with("expired")));
    assert_eq!(
        env.storage.list_approvals(run).unwrap()[0]
            .decision
            .as_deref(),
        Some("expired")
    );
    // The run is not left in `waiting_approval`: it finishes its turn.
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert!(lp.sh.pending.lock().is_empty() && lp.sh.parked.lock().is_empty());
}
