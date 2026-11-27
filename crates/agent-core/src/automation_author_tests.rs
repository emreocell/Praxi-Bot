//! Who wrote a schedule or trigger decides whether the Full lane serves it: the OWNER's own
//! automation on a bot at the Full level runs without cards (money still asks); one a model, a
//! paired phone or an unknown author wrote keeps its once-only cards. A trigger's event stays
//! untrusted data either way.

use std::sync::Arc;

use permission_engine::{Decision, Policy};
use protocol::{Event, PermissionCategory, RunId, TaskId};
use serde_json::json;
use tool_registry::{ContentClass, Effect};

use crate::ScheduleWork;
use crate::full_lane_tests::{Asks, decide};
use crate::tests::{Fake, calls, lane_cfg, make_loop, profile, setup, text, until, with_class};

const FULL: &str = "[permissions]\npreset = \"full\"\n";
const BALANCED: &str = "[permissions]\npreset = \"approve-destructive\"\n";

/// How the run starts.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Start {
    /// A timer: `run_scheduled`.
    Timer,
    /// A schedule that waits for outside data (a scheduled job's signed webhook).
    Trigger,
    /// A registry rule (mail, webhook, file watch): the daemon wraps the event itself.
    Rule,
}

const ALL: [Start; 3] = [Start::Timer, Start::Trigger, Start::Rule];

fn start_run(lp: &crate::AgentLoop, chat: protocol::ChatId, start: Start, id: TaskId) -> RunId {
    let prompt = "Check the directory and sign us up.";
    match start {
        Start::Timer => lp
            .run_scheduled(chat, id, ScheduleWork::Prompt(prompt.into()))
            .unwrap(),
        Start::Trigger => lp
            .run_scheduled_external(chat, id, ScheduleWork::Prompt(prompt.into()), None)
            .unwrap(),
        Start::Rule => lp.run_trigger_prompt(chat, id, None, prompt).unwrap(),
    }
}

fn tools() -> Vec<Arc<Asks>> {
    vec![
        Asks::new("ask_shell", PermissionCategory::Shell, ContentClass::none()),
        Asks::new(
            "ask_messaging",
            PermissionCategory::Messaging,
            ContentClass::none(),
        ),
        Asks::new("ask_money", PermissionCategory::Money, ContentClass::none()),
    ]
}

/// A finished run of `start` on a bot of the given level, with `owner_wrote` as the store's
/// answer, and its context.
async fn ctx_of(
    start: Start,
    preset: &str,
    owner_wrote: impl Fn(TaskId) -> bool + Send + Sync + 'static,
) -> (
    crate::tests::Env,
    crate::AgentLoop,
    Arc<crate::shared::RunCtx>,
    TaskId,
) {
    let env = setup();
    let fake = Fake::new(vec![text("ok")]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[],
        profile(preset),
        lane_cfg(Policy::full()),
    );
    for tool in tools() {
        lp.tool_registry()
            .register(tool as Arc<dyn tool_registry::Tool>);
    }
    lp.set_schedule_owner_check(Arc::new(owner_wrote));
    let id = TaskId::new();
    let run = start_run(&lp, env.chat, start, id);
    lp.wait(run).await.unwrap();
    let row = env.storage.get_run(run).unwrap().unwrap();
    lp.sh.runs.lock().clear();
    let ctx = lp.sh.ctx(&row).unwrap();
    (env, lp, ctx, id)
}

fn verdicts(lp: &crate::AgentLoop, ctx: &crate::shared::RunCtx) -> Vec<(String, Decision)> {
    tools()
        .iter()
        .map(|tool| (tool.spec.name.clone(), decide(lp, ctx, tool, &json!({})).0))
        .collect()
}

#[tokio::test]
async fn an_owner_schedule_or_trigger_on_full_runs_without_cards_except_money() {
    for start in ALL {
        let (_env, lp, ctx, _) = ctx_of(start, FULL, |_| true).await;
        for (name, decision) in verdicts(&lp, &ctx) {
            if name == "ask_money" {
                assert!(decision.is_ask(), "{start:?}/{name}: money always asks");
            } else {
                assert_eq!(decision, Decision::Allow, "{start:?}/{name}");
            }
        }
    }
}

#[tokio::test]
async fn a_schedule_the_owner_did_not_write_keeps_asking() {
    // written by a model, a paired phone, an import, or before authorship was recorded
    for start in ALL {
        let (_env, lp, ctx, _) = ctx_of(start, FULL, |_| false).await;
        assert!(!lp.sh.full_lane_run(&ctx), "{start:?}");
        for (name, decision) in verdicts(&lp, &ctx) {
            assert!(decision.is_ask(), "{start:?}/{name}: {decision:?}");
        }
    }
}

#[tokio::test]
async fn the_lane_asks_about_the_schedule_behind_the_run_and_nothing_else() {
    for start in ALL {
        let asked: Arc<parking_lot::Mutex<Vec<TaskId>>> = Arc::default();
        let log = Arc::clone(&asked);
        let (_env, lp, ctx, id) = ctx_of(start, FULL, move |id| {
            log.lock().push(id);
            true
        })
        .await;
        assert!(lp.sh.full_lane_run(&ctx));
        let asked = asked.lock();
        assert!(
            !asked.is_empty() && asked.iter().all(|a| *a == id),
            "{start:?}"
        );
    }
}

#[tokio::test]
async fn without_a_wired_store_no_schedule_counts_as_the_owners() {
    let env = setup();
    let fake = Fake::new(vec![text("ok")]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[],
        profile(FULL),
        lane_cfg(Policy::full()),
    );
    *lp.sh.schedule_owner.write() = None;
    let run = start_run(&lp, env.chat, Start::Timer, TaskId::new());
    lp.wait(run).await.unwrap();
    let row = env.storage.get_run(run).unwrap().unwrap();
    lp.sh.runs.lock().clear();
    assert!(!lp.sh.full_lane_run(&lp.sh.ctx(&row).unwrap()));
}

#[tokio::test]
async fn only_the_full_level_gets_the_owners_automation_lane() {
    for start in ALL {
        let (_env, lp, ctx, _) = ctx_of(start, BALANCED, |_| true).await;
        assert!(!lp.sh.full_lane_run(&ctx), "{start:?}");
    }
}

/// A run that no schedule stands behind (a plain turn passed off as a schedule) never gets the
/// lane, whatever the store would say.
#[tokio::test]
async fn a_scheduled_turn_with_no_schedule_behind_it_never_gets_the_lane() {
    let env = setup();
    let fake = Fake::new(vec![text("ok")]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[],
        profile(FULL),
        lane_cfg(Policy::full()),
    );
    let run = lp
        .run_turn_as(env.chat, "go", protocol::InitiatorKind::Schedule)
        .unwrap();
    lp.wait(run).await.unwrap();
    let row = env.storage.get_run(run).unwrap().unwrap();
    lp.sh.runs.lock().clear();
    assert!(!lp.sh.full_lane_run(&lp.sh.ctx(&row).unwrap()));
}

/// A subagent counts through the schedule at the root of its run, and only that one's author.
#[tokio::test]
async fn a_subagent_inherits_the_lane_of_its_owner_schedule_and_only_that() {
    for (owner_wrote, expected) in [(true, true), (false, false)] {
        let (env, lp, ctx, _) = ctx_of(Start::Timer, FULL, move |_| owner_wrote).await;
        let parent = env.storage.get_run(ctx.run_id).unwrap().unwrap();
        let child = storage::Run {
            id: RunId::new(),
            parent_run_id: Some(parent.id),
            kind: "turn:subagent".into(),
            spec: Some(json!({
                "task": "t", "max_steps": 3, "token_budget": 1000, "depth": 1, "spawn_key": "k",
            })),
            ..parent
        };
        env.storage.insert_run(&child).unwrap();
        let ctx = lp.sh.ctx(&child).unwrap();
        assert_eq!(
            lp.sh.full_lane_run(&ctx),
            expected,
            "owner_wrote={owner_wrote}"
        );
    }
}

/// A trigger fired by an external mail runs on the Full lane, but the mail body is still
/// untrusted data: the run holds untrusted content from its first call, the event reaches the
/// model as a fenced block, and money still asks.
#[tokio::test]
async fn a_trigger_event_runs_in_the_lane_but_stays_untrusted_data() {
    for start in [Start::Trigger, Start::Rule] {
        let env = setup();
        let pay = Arc::new(with_class(
            "pay",
            Effect::Destructive,
            PermissionCategory::Money,
            ContentClass::none(),
            "paid",
        ));
        let fake = Fake::new(vec![
            calls(&[("c", "pay", json!({"amount": 5}))]),
            text("done"),
        ]);
        let lp = make_loop(
            &env.storage,
            env.bot,
            &fake,
            &[&pay],
            profile(FULL),
            lane_cfg(Policy::full()),
        );
        lp.set_schedule_owner_check(Arc::new(|_| true));
        let mut rx = lp.subscribe();
        let run = start_run(&lp, env.chat, start, TaskId::new());
        let event = until(&mut rx, |e| {
            matches!(e, Event::ApprovalNeeded(_) | Event::RunFinished(_))
        })
        .await;
        let Event::ApprovalNeeded(card) = event else {
            panic!("{start:?}: money must ask");
        };
        assert_eq!(card.tool_name, "pay");
        // the run is parked on the card, so its exposure can be read
        let untrusted = lp
            .sh
            .runs
            .lock()
            .get(&run)
            .is_some_and(|ctx| ctx.taint.lock().exposure.untrusted);
        assert!(untrusted, "{start:?}: the event is untrusted from step 0");
        lp.cancel(run).unwrap();
        if start == Start::Trigger {
            let first = serde_json::to_string(&fake.request(0).messages).unwrap();
            assert!(first.contains("untrusted source data"), "{first}");
        }
    }
}

/// The timer's own run is not untrusted, which is what makes the test above mean something.
#[tokio::test]
async fn a_timer_run_holds_no_untrusted_content_to_begin_with() {
    let env = setup();
    let pay = Arc::new(with_class(
        "pay",
        Effect::Destructive,
        PermissionCategory::Money,
        ContentClass::none(),
        "paid",
    ));
    let fake = Fake::new(vec![
        calls(&[("c", "pay", json!({"amount": 5}))]),
        text("done"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[&pay],
        profile(FULL),
        lane_cfg(Policy::full()),
    );
    let mut rx = lp.subscribe();
    let run = start_run(&lp, env.chat, Start::Timer, TaskId::new());
    let event = until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await;
    assert!(matches!(event, Event::ApprovalNeeded(_)));
    let untrusted = lp
        .sh
        .runs
        .lock()
        .get(&run)
        .is_some_and(|ctx| ctx.taint.lock().exposure.untrusted);
    assert!(!untrusted);
    lp.cancel(run).unwrap();
}
