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
