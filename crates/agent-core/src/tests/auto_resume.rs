//! After a daemon restart the runs it interrupted continue by themselves when
//! their bot is at the Full level, a bounded number of times; every other run
//! stays paused with a reason. Recovery (which also runs during shutdown)
//! never resumes anything.

use super::*;
use task_runtime::{RESTART_LIMIT_PAUSE_REASON, RESTART_PAUSE_REASON};

fn full_bot() -> BotProfile {
    profile("[permissions]\npreset = \"full\"\n")
}

async fn model_call_started(fake: &Fake) {
    for _ in 0..500 {
        if fake.n_requests() >= 1 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the model call never started");
}

fn reason(env: &Env, run: protocol::RunId) -> Option<String> {
    env.storage.get_run(run).unwrap().unwrap().pause_reason
}

fn audit_count(env: &Env, run: protocol::RunId, action: &str) -> usize {
    env.storage
        .list_audit_for_run(env.user, run)
        .unwrap()
        .iter()
        .filter(|a| a.action == action)
        .count()
}

/// A run of `profile` that a graceful shutdown left paused.
async fn run_paused_by_shutdown(env: &Env, prof: BotProfile) -> protocol::RunId {
    let fake = Fake::new(vec![Reply::Hang]);
    let lp = make_loop(&env.storage, env.bot, &fake, &[], prof, cfg(Policy::full()));
    let run = lp.run_turn(env.chat, "long task").unwrap();
    model_call_started(&fake).await;
    assert_eq!(lp.shutdown(Duration::from_secs(5)).await, vec![run]);
    assert_eq!(lp.runtime().state(run).unwrap(), RunState::Paused);
    run
}

#[tokio::test]
async fn a_full_bot_continues_after_a_graceful_restart() {
    let env = setup();
    let run = run_paused_by_shutdown(&env, full_bot()).await;
    assert_eq!(reason(&env, run).as_deref(), Some(RESTART_PAUSE_REASON));

    let fake = Fake::new(vec![text("finished after restart")]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[],
        full_bot(),
        cfg(Policy::full()),
    );
    assert!(lp.recover().unwrap().is_empty());
    assert_eq!(lp.auto_resume_after_restart().await, vec![run]);
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(reason(&env, run), None, "the reason goes with the pause");
    assert_eq!(audit_count(&env, run, "run.auto_resumed"), 1);
}

#[tokio::test]
async fn a_bot_below_full_stays_paused_with_a_reason() {
    let env = setup();
    let run = run_paused_by_shutdown(&env, profile("")).await;

    let fake = Fake::new(vec![text("must not be asked")]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[],
        profile(""),
        cfg(Policy::full()),
    );
    assert!(lp.auto_resume_after_restart().await.is_empty());
    assert_eq!(lp.runtime().state(run).unwrap(), RunState::Paused);
    assert_eq!(reason(&env, run).as_deref(), Some(RESTART_PAUSE_REASON));
    assert_eq!(fake.n_requests(), 0);
    // The owner can still resume it by hand.
    lp.resume(run).unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
}

#[tokio::test]
async fn nothing_is_resumed_while_the_loop_shuts_down() {
    let env = setup();
    let fake = Fake::new(vec![Reply::Hang, text("must not be asked")]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[],
        full_bot(),
        cfg(Policy::full()),
    );
    let run = lp.run_turn(env.chat, "long task").unwrap();
    model_call_started(&fake).await;
    assert_eq!(lp.shutdown(Duration::from_secs(5)).await, vec![run]);
    // The daemon runs `recover` after the shutdown; neither it nor an
    // auto-resume may undo the pause.
    assert!(lp.recover().unwrap().is_empty());
    assert!(lp.auto_resume_after_restart().await.is_empty());
    assert_eq!(lp.runtime().state(run).unwrap(), RunState::Paused);
    assert_eq!(fake.n_requests(), 1);
    assert_eq!(audit_count(&env, run, "run.auto_resumed"), 0);
}

#[tokio::test]
async fn a_run_that_keeps_getting_interrupted_stops_being_resumed() {
    let env = setup();
    let run = run_paused_by_shutdown(&env, full_bot()).await;
    let mut config = cfg(Policy::full());
    config.max_auto_resumes = 2;
    for attempt in 1..=2 {
        let fake = Fake::new(vec![Reply::Hang]);
        let lp = make_loop(
            &env.storage,
            env.bot,
            &fake,
            &[],
            full_bot(),
            config.clone(),
        );
        assert_eq!(lp.auto_resume_after_restart().await, vec![run], "{attempt}");
        model_call_started(&fake).await;
        // Restarted again before it finished.
        assert_eq!(lp.shutdown(Duration::from_secs(5)).await, vec![run]);
        assert_eq!(audit_count(&env, run, "run.auto_resumed"), attempt);
    }
    let fake = Fake::new(vec![text("must not be asked")]);
    let lp = make_loop(&env.storage, env.bot, &fake, &[], full_bot(), config);
    assert!(lp.auto_resume_after_restart().await.is_empty());
    assert_eq!(lp.runtime().state(run).unwrap(), RunState::Paused);
    assert_eq!(
        reason(&env, run).as_deref(),
        Some(RESTART_LIMIT_PAUSE_REASON),
        "the owner sees why it stopped"
    );
    assert_eq!(audit_count(&env, run, "run.auto_resume_limit"), 1);
    assert_eq!(fake.n_requests(), 0);
    // Asking again changes nothing (no second limit record is a bug too).
    assert!(lp.auto_resume_after_restart().await.is_empty());
}

#[tokio::test]
async fn the_automatic_resume_can_be_switched_off() {
    let env = setup();
    let run = run_paused_by_shutdown(&env, full_bot()).await;
    let fake = Fake::new(vec![text("must not be asked")]);
    let mut config = cfg(Policy::full());
    config.max_auto_resumes = 0;
    let lp = make_loop(&env.storage, env.bot, &fake, &[], full_bot(), config);
    assert!(lp.auto_resume_after_restart().await.is_empty());
    assert_eq!(lp.runtime().state(run).unwrap(), RunState::Paused);
    assert_eq!(reason(&env, run).as_deref(), Some(RESTART_PAUSE_REASON));
}

#[tokio::test]
async fn runs_the_owner_paused_are_not_touched() {
    let env = setup();
    let run = run_paused_by_shutdown(&env, full_bot()).await;
    // Something other than a restart paused it (no restart reason).
    env.storage.set_run_pause_reason(run, None).unwrap();
    let fake = Fake::new(vec![text("must not be asked")]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[],
        full_bot(),
        cfg(Policy::full()),
    );
    assert!(lp.auto_resume_after_restart().await.is_empty());
    assert_eq!(lp.runtime().state(run).unwrap(), RunState::Paused);
}

/// A run left `Running` by a crashed process (its runtime is dropped without
/// a shutdown).
fn crashed_while_running(env: Env) -> (protocol::RunId, tempfile::TempDir) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let run = rt.block_on(async {
        let fake = Fake::new(vec![Reply::Hang]);
        let lp = make_loop(
            &env.storage,
            env.bot,
            &fake,
            &[],
            full_bot(),
            cfg(Policy::full()),
        );
        let run = lp.run_turn(env.chat, "long task").unwrap();
        model_call_started(&fake).await;
        run
    });
    drop(rt);
    (run, env.crash())
}

#[test]
fn a_run_left_running_by_a_crash_is_recovered_and_continues() {
    let env = setup();
    let (bot, path) = (env.bot, env.dir.path().to_path_buf());
    let user = env.user;
    let (run, _data_dir) = crashed_while_running(env);

    let storage = Arc::new(Storage::open(&path).unwrap());
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let fake = Fake::new(vec![text("finished after the crash")]);
        let lp = make_loop(&storage, bot, &fake, &[], full_bot(), cfg(Policy::full()));
        assert_eq!(lp.recover().unwrap(), vec![run]);
        assert_eq!(lp.runtime().state(run).unwrap(), RunState::Paused);
        assert_eq!(
            storage
                .get_run(run)
                .unwrap()
                .unwrap()
                .pause_reason
                .as_deref(),
            Some(RESTART_PAUSE_REASON)
        );
        assert_eq!(fake.n_requests(), 0, "recovery alone replays nothing");
        assert_eq!(lp.auto_resume_after_restart().await, vec![run]);
        assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
        let resumed = storage
            .list_audit_for_run(user, run)
            .unwrap()
            .iter()
            .filter(|a| a.action == "run.auto_resumed")
            .count();
        assert_eq!(resumed, 1);
    });
}

/// A tool that always asks the owner, even at the Full level (money).
fn paying_tool() -> Arc<TestTool> {
    Arc::new(with_class(
        "t.pay",
        Effect::Write,
        PermissionCategory::Money,
        ContentClass::none(),
        "paid",
    ))
}

fn pay() -> Reply {
    calls(&[("p1", "t.pay", json!({}))])
}

#[tokio::test]
async fn a_run_parked_on_an_approval_is_not_resumed_by_a_graceful_restart() {
    let env = setup();
    let pay_tool = paying_tool();
    let fake = Fake::new(vec![pay(), text("must not be asked")]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[&pay_tool],
        full_bot(),
        cfg(Policy::full()),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "pay the invoice").unwrap();
    until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await;
    assert_eq!(lp.runtime().state(run).unwrap(), RunState::WaitingApproval);
    assert_eq!(lp.shutdown(Duration::from_secs(5)).await, vec![run]);
    assert_eq!(lp.runtime().state(run).unwrap(), RunState::Paused);
    assert_eq!(reason(&env, run), None, "a person was being waited for");

    let again = Fake::new(vec![text("must not be asked")]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &again,
        &[&pay_tool],
        full_bot(),
        cfg(Policy::full()),
    );
    lp.recover().unwrap();
    assert!(lp.auto_resume_after_restart().await.is_empty());
    assert_eq!(lp.runtime().state(run).unwrap(), RunState::Paused);
    assert_eq!(again.n_requests(), 0);
    assert_eq!(pay_tool.count.load(Ordering::SeqCst), 0);
    assert_eq!(audit_count(&env, run, "run.auto_resumed"), 0);
}

#[test]
fn a_run_parked_on_an_approval_when_the_daemon_crashed_is_not_resumed() {
    let env = setup();
    let (bot, chat, user) = (env.bot, env.chat, env.user);
    let path = env.dir.path().to_path_buf();
    let pay_tool = paying_tool();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let run = rt.block_on(async {
        let fake = Fake::new(vec![pay(), Reply::Hang]);
        let lp = make_loop(
            &env.storage,
            bot,
            &fake,
            &[&pay_tool],
            full_bot(),
            cfg(Policy::full()),
        );
        let mut rx = lp.subscribe();
        let run = lp.run_turn(chat, "pay the invoice").unwrap();
        until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await;
        for _ in 0..500 {
            if lp.runtime().state(run).unwrap() == RunState::WaitingApproval {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(lp.runtime().state(run).unwrap(), RunState::WaitingApproval);
        run
    });
    drop(rt);
    let _data_dir = env.crash();

    let storage = Arc::new(Storage::open(&path).unwrap());
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let fake = Fake::new(vec![text("must not be asked")]);
        let lp = make_loop(
            &storage,
            bot,
            &fake,
            &[&pay_tool],
            full_bot(),
            cfg(Policy::full()),
        );
        // Dropping the runtime unwinds the parked approval back to `running`; a killed
        // process would have left what the run showed while it waited.
        storage
            .update_run_state(run, RunState::WaitingApproval.as_str(), None)
            .unwrap();
        assert_eq!(lp.recover().unwrap(), vec![run]);
        let row = storage.get_run(run).unwrap().unwrap();
        assert_eq!(row.state, "paused");
        assert_eq!(row.pause_reason, None, "a person was being waited for");
        assert!(lp.auto_resume_after_restart().await.is_empty());
        assert_eq!(fake.n_requests(), 0);
        let resumed = storage
            .list_audit_for_run(user, run)
            .unwrap()
            .iter()
            .filter(|a| a.action == "run.auto_resumed")
            .count();
        assert_eq!(resumed, 0);
    });
}

#[tokio::test]
async fn a_stop_that_lands_just_before_the_shutdown_stays_a_stop() {
    let env = setup();
    let fake = Fake::new(vec![Reply::Hang, text("must not be asked")]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[],
        full_bot(),
        cfg(Policy::full()),
    );
    let run = lp.run_turn(env.chat, "long task").unwrap();
    model_call_started(&fake).await;
    // The owner presses Stop; the daemon is told to shut down before the run
    // has unwound.
    lp.cancel(run).unwrap();
    assert!(
        lp.shutdown(Duration::from_secs(5)).await.is_empty(),
        "a stopped run is not a run the shutdown paused"
    );
    assert_eq!(lp.runtime().state(run).unwrap(), RunState::Cancelled);
    assert_eq!(reason(&env, run), None);

    let again = Fake::new(vec![text("must not be asked")]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &again,
        &[],
        full_bot(),
        cfg(Policy::full()),
    );
    lp.recover().unwrap();
    assert!(lp.auto_resume_after_restart().await.is_empty());
    assert_eq!(lp.runtime().state(run).unwrap(), RunState::Cancelled);
    assert_eq!(again.n_requests(), 0);
}

#[tokio::test]
async fn the_attempt_is_on_record_before_the_run_starts() {
    let env = setup();
    let run = run_paused_by_shutdown(&env, full_bot()).await;
    // Whatever state the run is in when the audit row lands is copied away.
    let conn = rusqlite::Connection::open(env.dir.path().join("bot.db")).unwrap();
    conn.execute_batch(
        "CREATE TABLE probe (state TEXT);
         CREATE TRIGGER probe_state BEFORE INSERT ON audit_log
         WHEN NEW.action = 'run.auto_resumed'
         BEGIN INSERT INTO probe SELECT state FROM runs WHERE id = NEW.run_id; END;",
    )
    .unwrap();
    let fake = Fake::new(vec![text("finished")]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[],
        full_bot(),
        cfg(Policy::full()),
    );
    assert_eq!(lp.auto_resume_after_restart().await, vec![run]);
    let seen: Vec<String> = conn
        .prepare("SELECT state FROM probe")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(seen, ["paused"], "counted before the run left `paused`");
}

#[tokio::test]
async fn an_audit_log_that_cannot_be_written_resumes_nothing() {
    let env = setup();
    let run = run_paused_by_shutdown(&env, full_bot()).await;
    let conn = rusqlite::Connection::open(env.dir.path().join("bot.db")).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER no_attempt_record BEFORE INSERT ON audit_log
         WHEN NEW.action = 'run.auto_resumed'
         BEGIN SELECT RAISE(ABORT, 'fake audit failure'); END;",
    )
    .unwrap();
    let fake = Fake::new(vec![text("must not be asked")]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[],
        full_bot(),
        cfg(Policy::full()),
    );
    assert!(lp.auto_resume_after_restart().await.is_empty());
    assert_eq!(lp.runtime().state(run).unwrap(), RunState::Paused);
    assert_eq!(fake.n_requests(), 0);
    // It stays paused with its reason: the owner can still continue it by hand.
    assert_eq!(reason(&env, run).as_deref(), Some(RESTART_PAUSE_REASON));
}

#[tokio::test]
async fn an_audit_log_that_cannot_be_read_resumes_nothing() {
    let env = setup();
    let run = run_paused_by_shutdown(&env, full_bot()).await;
    let conn = rusqlite::Connection::open(env.dir.path().join("bot.db")).unwrap();
    conn.execute_batch("ALTER TABLE audit_log RENAME TO audit_log_unreadable")
        .unwrap();
    let fake = Fake::new(vec![text("must not be asked")]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[],
        full_bot(),
        cfg(Policy::full()),
    );
    assert!(lp.auto_resume_after_restart().await.is_empty());
    assert_eq!(lp.runtime().state(run).unwrap(), RunState::Paused);
    assert_eq!(
        fake.n_requests(),
        0,
        "an unknown count is not 'no attempts yet'"
    );
}

#[tokio::test]
async fn a_run_that_did_not_start_gives_its_attempt_back() {
    let env = setup();
    let run = run_paused_by_shutdown(&env, full_bot()).await;
    let mut config = cfg(Policy::full());
    config.max_auto_resumes = 1;
    // The chat is busy (another run owns it): the resume is refused.
    let fake = Fake::new(vec![Reply::Hang, text("finished")]);
    let lp = make_loop(&env.storage, env.bot, &fake, &[], full_bot(), config);
    let other = lp.run_turn(env.chat, "something else").unwrap();
    model_call_started(&fake).await;
    assert!(lp.auto_resume_after_restart().await.is_empty());
    assert_eq!(audit_count(&env, run, "run.auto_resumed"), 1);
    assert_eq!(audit_count(&env, run, "run.auto_resume_unused"), 1);
    lp.cancel(other).unwrap();
    lp.wait(other).await.ok();
    // Still within its single attempt: it continues once the chat is free.
    assert_eq!(lp.auto_resume_after_restart().await, vec![run]);
}

#[tokio::test]
async fn the_deny_all_lock_stops_the_automatic_resume() {
    let env = setup();
    let run = run_paused_by_shutdown(&env, full_bot()).await;

    // Locked deny-all mode (an unusable saved policy).
    let fake = Fake::new(vec![text("finished")]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[],
        full_bot(),
        cfg(Policy::full()),
    );
    lp.set_policy_locked(true);
    assert!(lp.auto_resume_after_restart().await.is_empty());
    assert_eq!(lp.runtime().state(run).unwrap(), RunState::Paused);
    assert_eq!(fake.n_requests(), 0);
    assert_eq!(reason(&env, run).as_deref(), Some(RESTART_PAUSE_REASON));
    assert_eq!(audit_count(&env, run, "run.auto_resumed"), 0);

    // Unlocked again, it continues.
    lp.set_policy_locked(false);
    assert_eq!(lp.auto_resume_after_restart().await, vec![run]);
}

#[tokio::test]
async fn a_bot_lowered_below_full_while_the_daemon_was_down_does_not_continue() {
    let env = setup();
    let run = run_paused_by_shutdown(&env, full_bot()).await;
    // The owner lowered the bot before the next start (the profile is read again then).
    let fake = Fake::new(vec![text("must not be asked")]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[],
        profile("[permissions]\npreset = \"approve_destructive\"\n"),
        cfg(Policy::full()),
    );
    assert!(lp.auto_resume_after_restart().await.is_empty());
    assert_eq!(lp.runtime().state(run).unwrap(), RunState::Paused);
    assert_eq!(fake.n_requests(), 0);
}

#[tokio::test]
async fn a_cut_off_cli_session_is_not_resumed_by_itself() {
    let env = setup();
    let run = run_paused_by_shutdown(&env, full_bot()).await;
    // The interrupted model step was a CLI session: its tool outcomes are unknown.
    let conn = rusqlite::Connection::open(env.dir.path().join("bot.db")).unwrap();
    conn.execute(
        "UPDATE run_steps SET input_json = json_set(input_json, '$.model', 'cli:fixture') \
         WHERE name = 'model'",
        [],
    )
    .unwrap();
    let fake = Fake::new(vec![text("must not be asked")]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[],
        full_bot(),
        cfg(Policy::full()),
    );
    assert!(lp.auto_resume_after_restart().await.is_empty());
    assert_eq!(lp.runtime().state(run).unwrap(), RunState::Paused);
    assert_eq!(audit_count(&env, run, "run.auto_resumed"), 0);
}
