//! A model chain that fails only for transient reasons does not fail the run:
//! the step waits (exponential backoff, `transient_outage_budget`) and resumes.
//! Every test runs on a paused clock, so the waits cost no real time.

use super::*;

fn outage_cfg(budget_secs: u64) -> LoopConfig {
    let mut config = cfg(Policy::full());
    config.transient_outage_budget = Duration::from_secs(budget_secs);
    config
}

fn started(
    env: &Env,
    fake: &Arc<Fake>,
    config: LoopConfig,
) -> (AgentLoop, protocol::RunId, tokio::time::Instant) {
    let lp = make_loop(&env.storage, env.bot, fake, &[], profile(""), config);
    let run = lp.run_turn(env.chat, "hi").unwrap();
    (lp, run, tokio::time::Instant::now())
}

#[tokio::test(start_paused = true)]
async fn an_outage_is_waited_out_and_the_run_resumes() {
    let env = setup();
    let fake = Fake::new(vec![
        Reply::Fail("upstream"),
        Reply::Fail("upstream"),
        text("back again"),
    ]);
    let (lp, run, t0) = started(&env, &fake, outage_cfg(600));
    let mut rx = lp.subscribe();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(fake.n_requests(), 3);
    // Waits of 5 s and 10 s, each between half and all of that (jitter).
    let waited = t0.elapsed();
    assert!(
        waited >= Duration::from_millis(7_500) && waited <= Duration::from_secs(15),
        "waited {waited:?}"
    );
    let mut notices = 0;
    while let Ok(item) = rx.try_recv() {
        if let Event::Error(e) = item.envelope.event {
            assert!(e.recoverable, "a wait is not a failure");
            assert!(e.message.contains("trying again"), "{}", e.message);
            notices += 1;
        }
    }
    assert_eq!(notices, 2, "the user is told about each wait");
}

#[tokio::test(start_paused = true)]
async fn a_long_retry_after_is_waited_when_the_outage_budget_allows() {
    let env = setup();
    // Longer than the model timeout (300 s): no single call can wait for it.
    let fake = Fake::new(vec![
        Reply::RateLimited(Some(Duration::from_secs(400))),
        text("recovered"),
    ]);
    let (lp, run, t0) = started(&env, &fake, outage_cfg(600));
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(fake.n_requests(), 2);
    assert!(t0.elapsed() >= Duration::from_secs(400));
}

#[tokio::test(start_paused = true)]
async fn a_retry_after_beyond_the_outage_budget_fails_at_once() {
    let env = setup();
    let fake = Fake::new(vec![Reply::RateLimited(Some(Duration::from_secs(700)))]);
    let (lp, run, t0) = started(&env, &fake, outage_cfg(600));
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Failed);
    assert_eq!(fake.n_requests(), 1);
    assert!(t0.elapsed() < Duration::from_secs(1));
}

#[tokio::test(start_paused = true)]
async fn an_outage_that_outlasts_the_budget_fails_the_run_with_the_provider_error() {
    let env = setup();
    let fake = Fake::new((0..30).map(|_| Reply::Fail("upstream")).collect());
    let (lp, run, t0) = started(&env, &fake, outage_cfg(20));
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Failed);
    let n = fake.n_requests();
    assert!((2..=5).contains(&n), "{n} requests");
    assert!(
        t0.elapsed() <= Duration::from_secs(20),
        "{:?}",
        t0.elapsed()
    );
    let err = env.storage.get_run(run).unwrap().unwrap().error.unwrap();
    assert!(err.contains("down"), "{err}");
}

#[tokio::test(start_paused = true)]
async fn a_zero_budget_keeps_the_old_fail_fast_behaviour() {
    let env = setup();
    let fake = Fake::new(vec![Reply::Fail("upstream"), text("never reached")]);
    let (lp, run, _) = started(&env, &fake, outage_cfg(0));
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Failed);
    assert_eq!(fake.n_requests(), 1);
}

#[tokio::test(start_paused = true)]
async fn errors_that_waiting_cannot_fix_do_not_wait() {
    for kind in ["auth", "api400", "invalid"] {
        let env = setup();
        let fake = Fake::new(vec![Reply::Fail(kind), text("never reached")]);
        let (lp, run, t0) = started(&env, &fake, outage_cfg(600));
        assert_eq!(lp.wait(run).await.unwrap(), RunState::Failed, "{kind}");
        assert_eq!(fake.n_requests(), 1, "{kind}");
        assert!(t0.elapsed() < Duration::from_secs(1), "{kind}");
    }
}

#[tokio::test(start_paused = true)]
async fn stop_ends_the_wait_without_another_attempt() {
    let env = setup();
    let fake = Fake::new(vec![Reply::Fail("upstream"), text("never reached")]);
    let (lp, run, _) = started(&env, &fake, outage_cfg(600));
    // Let the first attempt fail and the wait begin.
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(fake.n_requests(), 1);
    lp.cancel(run).unwrap();
    let state = tokio::time::timeout(Duration::from_secs(2), lp.wait(run))
        .await
        .expect("stop must not wait for the backoff to end")
        .unwrap();
    assert_eq!(state, RunState::Cancelled);
    assert_eq!(fake.n_requests(), 1, "no attempt after the stop");
}

#[tokio::test(start_paused = true)]
async fn the_budget_is_wall_time_so_slow_attempts_count_too() {
    let env = setup();
    // Every attempt hangs until the stream idle timeout (2 minutes). Only the sleeps used
    // to be counted, which let a 10 minute budget stall for the better part of
    // an hour.
    let fake = Fake::new((0..40).map(|_| Reply::Hang).collect());
    let (lp, run, t0) = started(&env, &fake, outage_cfg(600));
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Failed);
    let n = fake.n_requests();
    assert!((2..=6).contains(&n), "{n} requests");
    let took = t0.elapsed();
    assert!(
        took >= Duration::from_secs(600) && took <= Duration::from_secs(700),
        "the step ended after {took:?}"
    );
}

struct Named(&'static str, Arc<Fake>);

#[async_trait]
impl ChatProvider for Named {
    fn id(&self) -> &str {
        self.0
    }
    async fn stream(
        &self,
        req: ChatRequest,
    ) -> Result<provider_catalog::ChatStream, ProviderError> {
        self.1.stream(req).await
    }
}

#[tokio::test(start_paused = true)]
async fn a_providers_retry_after_is_kept_while_another_one_keeps_failing() {
    let env = setup();
    // The primary asks for 400 s; the fallback answers 503 over and over.
    let primary = Fake::new(vec![
        Reply::RateLimited(Some(Duration::from_secs(400))),
        text("primary is back"),
    ]);
    let fallback = Fake::new((0..60).map(|_| Reply::Fail("upstream")).collect());
    let lp = make_loop(
        &env.storage,
        env.bot,
        &primary,
        &[],
        profile("fallback = [\"other/m\"]\n"),
        outage_cfg(600),
    );
    lp.set_providers(
        Providers::single(Arc::new(Named("fake", primary.clone())))
            .with(Arc::new(Named("other", fallback.clone()))),
    );
    let t0 = tokio::time::Instant::now();
    let run = lp.run_turn(env.chat, "hi").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    // The primary was not asked again before its 400 s were over, however many
    // rounds the failing fallback made the chain go through.
    assert_eq!(primary.n_requests(), 2);
    assert!(
        t0.elapsed() >= Duration::from_secs(400),
        "{:?}",
        t0.elapsed()
    );
    assert!(fallback.n_requests() >= 5, "{}", fallback.n_requests());
}
