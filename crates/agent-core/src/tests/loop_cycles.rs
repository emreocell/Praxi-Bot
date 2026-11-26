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

#[tokio::test]
async fn page_observations_record_the_browser_tab_and_other_results_do_not() {
    let env = setup();
    let mut snapshot = with_class(
        "browser_snapshot",
        Effect::Read,
        PermissionCategory::Browser,
        ContentClass::none(),
        "# Page (http://a/)
button",
    );
    snapshot.tab = Some("TAB-7".to_owned());
    let snapshot = Arc::new(snapshot);
    let mut read = tool("t.read", Effect::Read, "plain");
    read.tab = Some("must-not-be-recorded".to_owned());
    let read = Arc::new(read);
    let fake = Fake::new(vec![
        calls(&[("1", "browser_snapshot", json!({}))]),
        calls(&[("2", "t.read", json!({}))]),
        text("done"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[&snapshot, &read],
        profile(""),
        cfg(Policy::full()),
    );
    let run = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    let tabs: Vec<Option<String>> = env
        .storage
        .list_steps(run)
        .unwrap()
        .iter()
        .filter(|s| s.name == "tools")
        .map(|s| {
            s.output.as_ref().unwrap()["results"][0]["tab"]
                .as_str()
                .map(str::to_owned)
        })
        .collect();
    assert_eq!(tabs, vec![Some("TAB-7".to_owned()), None]);
}

#[tokio::test]
async fn a_progressing_loop_is_never_stopped() {
    // The click always answers "clicked", but every snapshot is a new page:
    // that is progress, however long it goes on.
    let env = setup();
    let clicker = Arc::new(tool("t.click", Effect::Read, "clicked"));
    let mut snapper = tool("t.snap", Effect::Read, "# Page (http://a/)\nitem");
    snapper.numbered = true;
    let snapper = Arc::new(snapper);
    let mut replies: Vec<Reply> = (0..9).flat_map(|i| [click(i), snap(i)]).collect();
    replies.push(text("done paging"));
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
    assert_eq!(snapper.count.load(Ordering::SeqCst), 9);
    let last = fake.request(fake.n_requests() - 1);
    assert!(
        tool_messages(&last)
            .iter()
            .all(|m| !m.contains("[loop-hint:")),
        "no hint for a loop that makes progress"
    );
}

#[tokio::test]
async fn identical_untrusted_pages_in_fresh_frames_are_still_a_cycle() {
    // Every observation of an outside page is framed with a new random block
    // id, so the stored texts differ byte for byte though the page does not.
    let env = setup();
    let clicker = Arc::new(tool("t.click", Effect::Read, "clicked"));
    let snapper = Arc::new(with_class(
        "t.snap",
        Effect::Read,
        PermissionCategory::Fs,
        ContentClass::declared(true, false, false),
        "# Page (http://a/)\nbutton e1",
    ));
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
    let ran = clicker.count.load(Ordering::SeqCst) + snapper.count.load(Ordering::SeqCst);
    assert!(
        ran < 16,
        "the framed cycle must stop early, {ran} calls ran"
    );
}
