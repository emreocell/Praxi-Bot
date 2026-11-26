//! A turn that ends with work left is continued by the runtime: the owner is not
//! asked to type "go on" after every step. Bounded, never for money, and a real
//! question still ends the turn.

use std::sync::Arc;

use permission_engine::Policy;
use provider_catalog::MessageRole;
use serde_json::json;
use task_runtime::RunState;

use crate::tests::{Fake, calls, cfg, make_loop, profile, setup, text};

const FULL: &str = "[permissions]\npreset = \"full\"\n";
const BALANCED: &str = "[permissions]\npreset = \"approve-destructive\"\n";

fn register_meta(lp: &crate::AgentLoop) {
    for tool in builtin_tools::meta::tools() {
        lp.sh.tools.register(tool);
    }
}

/// Run one turn with `script` and return (requests the model got, final chat text, fake).
async fn run(
    extra: &str,
    script: Vec<crate::tests::Reply>,
    configure: impl FnOnce(&mut crate::LoopConfig),
) -> (usize, String, Arc<Fake>) {
    let env = setup();
    let fake = Fake::new(script);
    let mut config = cfg(Policy::full());
    configure(&mut config);
    let lp = make_loop(&env.storage, env.bot, &fake, &[], profile(extra), config);
    register_meta(&lp);
    let run = lp
        .run_turn(env.chat, "Register me on the backlink sites")
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    let last = env
        .storage
        .list_messages(env.chat)
        .unwrap()
        .into_iter()
        .rfind(|m| m.role == "assistant")
        .map(|m| m.content)
        .unwrap_or_default();
    (fake.n_requests(), last, fake)
}

fn last_user_text(fake: &Fake, i: usize) -> String {
    let req = crate::tests::without_ledger(fake.request(i));
    let m = req.messages.last().unwrap();
    assert_eq!(
        m.role,
        MessageRole::User,
        "the continuation is a user-role message"
    );
    m.content.as_str().unwrap_or_default().to_owned()
}

#[tokio::test]
async fn full_level_answers_a_request_for_permission_and_the_task_goes_on() {
    let (n, last, fake) = run(
        FULL,
        vec![
            text("I registered on site 1. Shall I continue with the next one?"),
            text("Done: registered on both sites."),
        ],
        |_| {},
    )
    .await;
    assert_eq!(n, 2, "one automatic continuation, no human in between");
    assert_eq!(last, "Done: registered on both sites.");
    let note = last_user_text(&fake, 1);
    assert!(
        note.contains("Automatic reply from the Praxi runtime, not typed by the owner"),
        "{note}"
    );
    assert!(
        note.contains("yes, continue") && note.contains("Never move money"),
        "{note}"
    );
}

#[tokio::test]
async fn the_example_phrases_from_the_owner_are_all_continued() {
    for ask in [
        "I did one, shall I do the next?",
        "I can also research other companies, want me to?",
        "Should I continue?",
        "Devam edeyim mi?",
    ] {
        let (n, last, _) = run(FULL, vec![text(ask), text("Done: all of it.")], |_| {}).await;
        assert_eq!((n, last.as_str()), (2, "Done: all of it."), "{ask}");
    }
}

#[tokio::test]
async fn other_levels_keep_the_old_behaviour_for_a_permission_question() {
    let (n, last, _) = run(
        BALANCED,
        vec![
            text("I registered on site 1. Shall I continue?"),
            text("never reached"),
        ],
        |_| {},
    )
    .await;
    assert_eq!(n, 1);
    assert!(last.ends_with("Shall I continue?"));
}

#[tokio::test]
async fn a_real_question_still_ends_the_turn() {
    let (n, last, _) = run(
        FULL,
        vec![
            text("Which e-mail address should I use for the sign-ups?"),
            text("never reached"),
        ],
        |_| {},
    )
    .await;
    assert_eq!(n, 1);
    assert!(last.starts_with("Which e-mail"));
}

#[tokio::test]
async fn money_is_never_answered_automatically() {
    let (n, _, _) = run(
        FULL,
        vec![
            text("The premium listing costs $20. Shall I continue and pay?"),
            text("never reached"),
        ],
        |_| {},
    )
    .await;
    assert_eq!(n, 1);
}

#[tokio::test]
async fn an_unfinished_plan_is_continued_at_every_level() {
    for extra in [FULL, BALANCED] {
        let plan = json!({"items": [
            {"content": "Sign up on site 1", "status": "in_progress"},
            {"content": "Sign up on site 2", "status": "pending"}
        ]});
        let (n, last, fake) = run(
            extra,
            vec![
                calls(&[("c1", "todo_write", plan)]),
                text("Site 1 is registered."),
                text("Done: both sites."),
            ],
            |_| {},
        )
        .await;
        assert_eq!(n, 3, "{extra}");
        assert_eq!(last, "Done: both sites.");
        let note = last_user_text(&fake, 2);
        assert!(
            note.contains("plan is not finished") && note.contains("2 open step(s)"),
            "{note}"
        );
    }
}

#[tokio::test]
async fn a_finished_plan_or_a_done_marker_ends_the_turn() {
    let plan = json!({"items": [{"content": "Sign up", "status": "completed"}]});
    let (n, _, _) = run(
        FULL,
        vec![
            calls(&[("c1", "todo_write", plan)]),
            text("All done."),
            text("never reached"),
        ],
        |_| {},
    )
    .await;
    assert_eq!(n, 2, "nothing is open: the answer after the plan is final");
    let open = json!({"items": [{"content": "Sign up", "status": "pending"}]});
    let (n, _, _) = run(
        FULL,
        vec![
            calls(&[("c1", "todo_write", open)]),
            text("Done: nothing else was needed."),
            text("never reached"),
        ],
        |_| {},
    )
    .await;
    assert_eq!(n, 2, "an explicit Done: wins over a stale plan");
}

#[tokio::test]
async fn a_model_that_keeps_asking_is_given_up_on_after_two_idle_continuations() {
    let ask = || text("Shall I continue?");
    let (n, _, _) = run(FULL, vec![ask(), ask(), ask(), ask(), ask()], |_| {}).await;
    assert_eq!(
        n, 3,
        "ask, continue, ask, continue, ask: then the turn ends"
    );
}

#[tokio::test]
async fn auto_continue_can_be_switched_off_and_respects_the_step_budget() {
    let ask = || text("Shall I continue?");
    let (n, _, _) = run(FULL, vec![ask(), ask()], |c| c.guards.auto_continue_max = 0).await;
    assert_eq!(n, 1, "off");
    let (n, _, _) = run(FULL, vec![ask(), ask()], |c| c.guards.max_steps = 1).await;
    assert_eq!(n, 1, "no continuation past the step budget");
    let (n, _, _) = run(FULL, vec![ask(), ask()], |c| {
        c.guards.auto_continue_window = std::time::Duration::ZERO;
    })
    .await;
    assert_eq!(n, 1, "no continuation past the time budget");
}

#[tokio::test]
async fn a_scheduled_or_external_run_is_not_continued_by_the_owner_rule() {
    let env = setup();
    let fake = Fake::new(vec![text("Shall I continue?"), text("never reached")]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[],
        profile(FULL),
        cfg(Policy::full()),
    );
    let run = lp
        .run_turn_as(env.chat, "do it", protocol::InitiatorKind::ExternalAgent)
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(fake.n_requests(), 1);
}

#[test]
fn every_level_is_told_to_finish_the_task_but_only_a_subagent_is_not() {
    use crate::history::system_prompt;
    let balanced = crate::profile::BotProfile::from_toml_str(&format!(
        "[bot]\nid = 't'\nname = 'T'\n[models]\nexecutor = 'fake/m'\n{BALANCED}"
    ))
    .unwrap();
    let p = system_prompt(&balanced, false, None);
    for needle in [
        "work through all of it in this turn",
        "do not stop after one item of a list",
        "ask all your questions once, together, before you start",
        "park that item",
    ] {
        assert!(p.contains(needle), "{needle}: {p}");
    }
    assert!(!system_prompt(&balanced, true, None).contains("work through all of it"));
}
