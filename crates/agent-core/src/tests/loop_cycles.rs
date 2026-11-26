//! Cycle detection in the run loop: a model that alternates two calls whose
//! results never change gets a recovery hint first and is stopped after.

use super::*;

fn click(n: u32) -> Reply {
    calls(&[(&format!("c{n}"), "t.click", json!({"ref": "e1"}))])
}

fn snap(n: u32) -> Reply {
    calls(&[(&format!("s{n}"), "t.snap", json!({}))])
}

fn tools() -> (Arc<TestTool>, Arc<TestTool>) {
    (
        Arc::new(tool("t.click", Effect::Read, "clicked")),
        Arc::new(tool(
            "t.snap",
            Effect::Read,
            "# Page (http://a/)\nbutton e1",
        )),
    )
}

#[tokio::test]
async fn click_snapshot_cycle_gets_a_hint_then_stops() {
    let env = setup();
    let (clicker, snapper) = tools();
    let mut replies: Vec<Reply> = (0..8).flat_map(|i| [click(i), snap(i)]).collect();
    replies.push(text("I am stuck on this page"));
    let fake = Fake::new(replies);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[&clicker, &snapper],
        profile(""),
        cfg(Policy::full()),
    );
    let run = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert!(lp.stopped_by_guard(run).unwrap());
    // cycle_stop = 5: the fifth repetition of the pair is never executed.
    let ran = clicker.count.load(Ordering::SeqCst) + snapper.count.load(Ordering::SeqCst);
    assert!(ran < 16, "the cycle must stop early, {ran} calls ran");
    let last = fake.request(fake.n_requests() - 1);
    assert!(last.tools.is_empty(), "closing call has no tools");
    let note = last.messages.last().unwrap().content.as_str().unwrap();
    assert!(note.contains("loop detected"), "{note}");
    let seen = tool_messages(&last).join("\n");
    assert!(
        seen.contains("[loop-hint:") && seen.contains("-> "),
        "the model must have been told before the stop: {seen}"
    );
}

#[tokio::test]
async fn hint_arrives_before_the_stop_and_only_after_the_warn_threshold() {
    let env = setup();
    let (clicker, snapper) = tools();
    // Three full repetitions, then an answer: past `cycle_warn`, short of `cycle_stop`.
    let mut replies: Vec<Reply> = (0..3).flat_map(|i| [click(i), snap(i)]).collect();
    replies.push(text("done"));
    let fake = Fake::new(replies);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[&clicker, &snapper],
        profile(""),
        cfg(Policy::full()),
    );
    let run = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert!(
        !lp.stopped_by_guard(run).unwrap(),
        "no stop below cycle_stop"
    );
    let last = fake.request(fake.n_requests() - 1);
    let msgs = tool_messages(&last);
    assert_eq!(msgs.len(), 6);
    assert!(
        msgs[..3].iter().all(|m| !m.contains("[loop-hint:")),
        "no hint during the first repetitions: {msgs:?}"
    );
    assert!(
        msgs[4..].iter().any(|m| m.contains("[loop-hint:")),
        "hint expected on the third repetition: {msgs:?}"
    );
}

#[tokio::test]
async fn changing_arguments_are_progress_not_a_cycle() {
    let env = setup();
    let (clicker, snapper) = tools();
    let mut replies: Vec<Reply> = (0..8)
        .flat_map(|i| {
            [
                calls(&[(&format!("c{i}"), "t.click", json!({"ref": format!("e{i}")}))]),
                snap(i),
            ]
        })
        .collect();
    replies.push(text("all clicked"));
    let fake = Fake::new(replies);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[&clicker, &snapper],
        profile(""),
        cfg(Policy::full()),
    );
    let run = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert!(!lp.stopped_by_guard(run).unwrap());
    let last = fake.request(fake.n_requests() - 1);
    assert!(
        tool_messages(&last)
            .iter()
            .all(|m| !m.contains("[loop-hint:")),
        "a different ref each time is not a loop"
    );
}

#[tokio::test]
async fn cycle_detection_can_be_switched_off() {
    let env = setup();
    let (clicker, snapper) = tools();
    let mut replies: Vec<Reply> = (0..8).flat_map(|i| [click(i), snap(i)]).collect();
    replies.push(text("finished anyway"));
    let fake = Fake::new(replies);
    let mut config = cfg(Policy::full());
    config.guards.cycle_warn = 0;
    config.guards.cycle_stop = 0;
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[&clicker, &snapper],
        profile(""),
        config,
    );
    let run = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert!(!lp.stopped_by_guard(run).unwrap());
    assert_eq!(clicker.count.load(Ordering::SeqCst), 8);
}
