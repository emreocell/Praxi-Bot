//! Stop and approvals: after the owner stops a run every open approval of it
//! is recorded as denied exactly once, and nothing the owner (or a client)
//! sends afterwards can approve, grant or start a tool for that run.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use permission_engine::Policy;
use protocol::{Event, RunId, ToolCallId};
use serde_json::json;

use crate::tests::{Env, Fake, TestTool, cfg, make_loop, profile, setup, text, tool, until};
use crate::{AgentLoop, ApprovalScope};

struct Rig {
    env: Env,
    lp: AgentLoop,
    write: Arc<TestTool>,
    rx: tokio::sync::broadcast::Receiver<crate::StreamItem>,
    run: RunId,
    call: ToolCallId,
}

async fn parked() -> Rig {
    let env = setup();
    let write = Arc::new(tool("t.write", tool_registry::Effect::Write, "ran"));
    let fake = Fake::new(vec![
        crate::tests::calls(&[("c1", "t.write", json!({}))]),
        text("must not run"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[crate::tests::t(&write)],
        profile(""),
        cfg(Policy::approve_destructive()),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let Event::ApprovalNeeded(a) = until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
    else {
        unreachable!()
    };
    Rig {
        env,
        lp,
        write,
        rx,
        run,
        call: a.call_id,
    }
}

async fn settle(rig: &Rig) {
    for _ in 0..200 {
        if rig
            .lp
            .runtime()
            .state(rig.run)
            .is_ok_and(task_runtime::RunState::is_terminal)
            && rig.lp.sh.pending.lock().is_empty()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
}

fn denied_events(rig: &mut Rig) -> usize {
    let mut n = 0;
    while let Ok(item) = rig.rx.try_recv() {
        if matches!(&item.envelope.event, Event::ToolCallDenied(d) if d.call_id == rig.call) {
            n += 1;
        }
    }
    n
}

fn decisions(rig: &Rig) -> Vec<Option<String>> {
    rig.env
        .storage
        .list_approvals(rig.run)
        .unwrap()
        .into_iter()
        .map(|a| a.decision)
        .collect()
}

fn denial_audits(rig: &Rig) -> usize {
    rig.env
        .storage
        .list_audit(rig.env.user)
        .unwrap()
        .iter()
        .filter(|r| r.run_id == Some(rig.run) && r.action == "approval.denied")
        .count()
}

/// Second Stop press: `force_cancel` concludes (and clears the stop marker)
/// before the aborted drive task drops its pending guard. The current-thread
/// runtime keeps that order deterministic.
#[tokio::test]
async fn force_stop_records_the_open_approval_as_denied_once() {
    let mut rig = parked().await;
    rig.lp.force_cancel(rig.run).unwrap();
    settle(&rig).await;
    assert_eq!(decisions(&rig), [Some("denied".to_owned())]);
    assert_eq!(denied_events(&mut rig), 1);
    assert_eq!(denial_audits(&rig), 1);
    assert_eq!(rig.write.count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn an_approval_sent_after_stop_is_refused_and_never_runs_the_tool() {
    let mut rig = parked().await;
    rig.lp.cancel(rig.run).unwrap();
    let late = rig
        .lp
        .approve(rig.call, ApprovalScope::Always, rig.env.user);
    assert!(late.is_err(), "approving a stopped run's card must fail");
    // Double resolve: a second click and an answer are no-ops.
    assert!(
        rig.lp
            .approve(rig.call, ApprovalScope::Once, rig.env.user)
            .is_err()
    );
    assert!(rig.lp.answer_question(rig.call, "yes").is_err());
    settle(&rig).await;
    assert_eq!(decisions(&rig), [Some("denied".to_owned())]);
    assert_eq!(denied_events(&mut rig), 1);
    assert_eq!(rig.write.count.load(Ordering::SeqCst), 0);
    assert!(
        rig.lp.sh.grants.lock().always.is_empty(),
        "no standing grant for a stopped run"
    );
}

/// The verdict is already sent when Stop lands, inside the window between the
/// wake-up and the tool call (hooked deterministically under `exec_gate`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_between_approve_and_start_runs_no_tool_and_mints_no_grant() {
    let mut rig = parked().await;
    let fired = Arc::new(AtomicBool::new(false));
    let lp = rig.lp.clone();
    let run = rig.run;
    let fired_h = Arc::clone(&fired);
    *rig.lp.sh.exec_commit_hook.lock() = Some(Arc::new(move |_| {
        if !fired_h.swap(true, Ordering::SeqCst) {
            lp.cancel(run).unwrap();
        }
    }));
    rig.lp
        .approve(rig.call, ApprovalScope::Always, rig.env.user)
        .unwrap();
    settle(&rig).await;
    assert!(fired.load(Ordering::SeqCst), "the commit hook ran");
    assert_eq!(
        rig.write.count.load(Ordering::SeqCst),
        0,
        "no tool after Stop"
    );
    assert_eq!(decisions(&rig), [Some("denied".to_owned())]);
    assert_eq!(denied_events(&mut rig), 1);
    assert!(
        rig.lp.sh.grants.lock().always.is_empty(),
        "no standing grant for a stopped run"
    );
}

/// Stop lands while an approval is open. Releasing the card wakes the step on
/// another worker; the token must already be cancelled by then, otherwise the
/// step finishes as "denied", the drive loop plans a second model call and the
/// run ends `succeeded`/`failed` instead of `cancelled`. The hook holds Stop
/// between "token signalled" and "cards released" until the drive task could
/// have run on (bounded wait, no sleep-for-effect on the passing path).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_never_lets_the_woken_step_start_another_model_call() {
    let env = setup();
    let write = Arc::new(tool("t.write", tool_registry::Effect::Write, "ran"));
    let fake = Fake::new(vec![
        crate::tests::calls(&[("c1", "t.write", json!({}))]),
        text("must not be reached"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[crate::tests::t(&write)],
        profile(""),
        cfg(Policy::approve_destructive()),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await;
    let probe = Arc::clone(&fake);
    *lp.sh.stop_split_hook.lock() = Some(Arc::new(move || {
        // On a broken order the woken step reaches the next model call here.
        for _ in 0..300 {
            if probe.n_requests() >= 2 {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }));
    lp.cancel(run).unwrap();
    for _ in 0..200 {
        if lp
            .runtime()
            .state(run)
            .is_ok_and(task_runtime::RunState::is_terminal)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        lp.runtime().state(run).unwrap(),
        task_runtime::RunState::Cancelled
    );
    assert_eq!(fake.n_requests(), 1, "no model call after Stop");
    assert_eq!(write.count.load(Ordering::SeqCst), 0);
}
