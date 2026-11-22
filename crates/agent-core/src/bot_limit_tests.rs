//! Per-bot spending limits: enforced before the model call, durable, fail closed.

use permission_engine::Policy;
use protocol::{Event, RunId};
use provider_catalog::ChatChunk;
use serde_json::json;
use task_runtime::RunState;

use crate::bot_limits::{self, BOT_LIMIT_REACHED, BOT_LIMIT_WARNING, BotLimits, Period};
use crate::tests::{Env, Fake, Reply, cfg, make_loop, profile, setup, text};
use crate::{AgentLoop, ModelPrice};

/// $1 per token: a test call (10 in, 5 out) costs $15, the first-call estimate is $4010.
fn priced() -> crate::LoopConfig {
    let mut c = cfg(Policy::full());
    c.max_tokens = Some(10);
    c.prices.insert(
        "fake/m".into(),
        ModelPrice {
            input_per_mtok: 1_000_000.0,
            output_per_mtok: 1_000_000.0,
            cached_per_mtok: None,
        },
    );
    c
}

fn set_limits(env: &Env, limits: &BotLimits) {
    env.storage
        .set_bot_spend_limits(
            env.user,
            env.bot,
            &serde_json::to_string(limits).unwrap(),
            &json!({}),
        )
        .unwrap();
}

fn daily(f: impl FnOnce(&mut bot_limits::PeriodLimit)) -> BotLimits {
    let mut l = BotLimits::default();
    f(&mut l.daily);
    l
}

async fn finish(
    lp: &AgentLoop,
    rx: &mut tokio::sync::broadcast::Receiver<crate::shared::StreamItem>,
    run: RunId,
) -> (RunState, Vec<Event>) {
    let state = lp.wait(run).await.unwrap();
    let mut evs = Vec::new();
    while let Ok(i) = rx.try_recv() {
        evs.push(i.envelope.event);
    }
    (state, evs)
}

fn run_error(evs: &[Event]) -> Option<String> {
    evs.iter().find_map(|e| match e {
        Event::RunFinished(f) => f.error.clone(),
        _ => None,
    })
}

#[tokio::test]
async fn hard_limit_stops_the_run_before_the_model_call() {
    let env = setup();
    set_limits(&env, &daily(|d| d.hard_usd = Some(100.0)));
    let fake = Fake::new(vec![text("hello")]);
    let lp = make_loop(&env.storage, env.bot, &fake, &[], profile(""), priced());
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "hi").unwrap();
    let (state, evs) = finish(&lp, &mut rx, run).await;
    assert_eq!(state, RunState::Failed);
    assert_eq!(fake.n_requests(), 0, "no provider request may be made");
    let msg = run_error(&evs).expect("typed error");
    assert!(
        msg.contains(&format!("[{BOT_LIMIT_REACHED} period=daily metric=usd")),
        "{msg}"
    );
    let audit = env.storage.list_audit(env.user).unwrap();
    assert!(audit.iter().any(|a| a.action == "bot_limit.blocked"));
}

#[tokio::test]
async fn unknown_price_counts_tokens_only_and_unreported_usage_is_not_free() {
    // No price known: a tiny money cap cannot block, a token cap does.
    let env = setup();
    set_limits(&env, &daily(|d| d.hard_usd = Some(0.01)));
    let fake = Fake::new(vec![text("a")]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[],
        profile(""),
        cfg(Policy::full()),
    );
    let run = lp.run_turn(env.chat, "hi").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    let u = bot_limits::period_usage(
        &env.storage,
        env.user,
        env.bot,
        Period::Daily,
        storage::now_ms(),
    )
    .unwrap();
    assert_eq!((u.usd, u.tokens), (0.0, 15));

    // Provider reports no usage: the start-of-call estimate counts instead.
    let env = setup();
    set_limits(&env, &daily(|d| d.hard_usd = Some(5_000.0)));
    let silent = Reply::Chunks(vec![ChatChunk::TextDelta("x".into())]);
    let fake = Fake::new(vec![silent, text("never")]);
    let lp = make_loop(&env.storage, env.bot, &fake, &[], profile(""), priced());
    let run = lp.run_turn(env.chat, "one").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    let u = bot_limits::period_usage(
        &env.storage,
        env.user,
        env.bot,
        Period::Daily,
        storage::now_ms(),
    )
    .unwrap();
    assert!(u.usd >= 4_000.0 && u.unreported_calls == 1, "{u:?}");
    let run = lp.run_turn(env.chat, "two").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Failed);
    assert_eq!(fake.n_requests(), 1);
}

#[tokio::test]
async fn unreadable_limits_fail_closed_and_new_bots_get_the_default_cap() {
    let env = setup();
    env.storage
        .set_bot_spend_limits(env.user, env.bot, "{not json", &json!({}))
        .unwrap();
    let fake = Fake::new(vec![text("hello")]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[],
        profile(""),
        cfg(Policy::full()),
    );
    let run = lp.run_turn(env.chat, "hi").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Failed);
    assert_eq!(fake.n_requests(), 0);

    // A bot without a stored row gets the modest default daily cap.
    let other = protocol::BotId::new();
    env.storage
        .insert_bot(&storage::Bot {
            id: other,
            user_id: env.user,
            name: "new".into(),
            config: json!({}),
            created_at_ms: storage::now_ms(),
        })
        .unwrap();
    let l = bot_limits::load_limits(&env.storage, other).unwrap();
    assert_eq!(l, BotLimits::default_for_new_bot());
    assert!(l.daily.hard_usd.is_some());
}

#[tokio::test]
async fn soft_limit_notifies_once_at_80_and_once_at_100() {
    let env = setup();
    set_limits(&env, &daily(|d| d.soft_usd = Some(18.0)));
    let replies = (0..4).map(|_| text("ok")).collect();
    let fake = Fake::new(replies);
    let lp = make_loop(&env.storage, env.bot, &fake, &[], profile(""), priced());
    let mut rx = lp.subscribe();
    let mut all = Vec::new();
    for _ in 0..4 {
        let run = lp.run_turn(env.chat, "hi").unwrap();
        let (state, evs) = finish(&lp, &mut rx, run).await;
        assert_eq!(state, RunState::Succeeded, "a soft limit never stops a run");
        all.extend(evs);
    }
    let warnings: Vec<String> = all
        .iter()
        .filter_map(|e| match e {
            Event::Error(r) if r.message.contains(BOT_LIMIT_WARNING) => Some(r.message.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        warnings.iter().filter(|m| m.contains("level=80")).count(),
        1,
        "{warnings:?}"
    );
    assert_eq!(
        warnings.iter().filter(|m| m.contains("level=100")).count(),
        1,
        "{warnings:?}"
    );
}

#[tokio::test]
async fn parallel_calls_of_one_bot_cannot_all_pass_just_under_the_cap() {
    // The cap fits ONE first-call estimate ($4010) but not two.
    let env = setup();
    set_limits(&env, &daily(|d| d.hard_usd = Some(5_000.0)));
    let chat2 = protocol::ChatId::new();
    env.storage
        .insert_chat(&storage::Chat {
            id: chat2,
            user_id: env.user,
            bot_id: env.bot,
            title: "c2".into(),
            created_at_ms: storage::now_ms(),
        })
        .unwrap();
    let fake = Fake::new(vec![Reply::Hang, text("never")]);
    let lp = make_loop(&env.storage, env.bot, &fake, &[], profile(""), priced());
    let first = lp.run_turn(env.chat, "one").unwrap();
    for _ in 0..200 {
        if fake.n_requests() == 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert_eq!(fake.n_requests(), 1, "first call is in flight");
    let second = lp.run_turn(chat2, "two").unwrap();
    assert_eq!(lp.wait(second).await.unwrap(), RunState::Failed);
    assert_eq!(
        fake.n_requests(),
        1,
        "second call never reached the provider"
    );
    lp.cancel(first).unwrap();
}

fn unpriced(max_tokens: u32) -> crate::LoopConfig {
    let mut c = cfg(Policy::full());
    c.max_tokens = Some(max_tokens);
    c
}

#[tokio::test]
async fn a_large_first_prompt_is_counted_at_admission() {
    // The default first-call guess (4000 tokens) fits a 10_000 token cap; a
    // 60 KB prompt (about 20_000 tokens) must not.
    let env = setup();
    set_limits(&env, &daily(|d| d.hard_tokens = Some(10_000)));
    let fake = Fake::new(vec![text("never")]);
    let lp = make_loop(&env.storage, env.bot, &fake, &[], profile(""), unpriced(10));
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, &"x".repeat(60_000)).unwrap();
    let (state, evs) = finish(&lp, &mut rx, run).await;
    assert_eq!(state, RunState::Failed);
    assert_eq!(fake.n_requests(), 0, "no provider request may be made");
    let msg = run_error(&evs).expect("typed error");
    assert!(
        msg.contains(BOT_LIMIT_REACHED) && msg.contains("metric=tokens"),
        "{msg}"
    );
}

#[tokio::test]
async fn a_stream_that_crosses_the_hard_limit_is_cut_off() {
    // Admission passes (about 4010 of 5000 tokens) but the reply keeps going.
    let env = setup();
    set_limits(&env, &daily(|d| d.hard_tokens = Some(5_000)));
    let chunks = (0..12)
        .map(|_| ChatChunk::TextDelta("y".repeat(1_000)))
        .collect();
    let fake = Fake::new(vec![Reply::Chunks(chunks)]);
    let lp = make_loop(&env.storage, env.bot, &fake, &[], profile(""), unpriced(10));
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "hi").unwrap();
    let (state, evs) = finish(&lp, &mut rx, run).await;
    assert_eq!(state, RunState::Failed);
    let msg = run_error(&evs).expect("typed error");
    assert!(msg.contains(BOT_LIMIT_REACHED), "{msg}");
    let streamed: usize = evs
        .iter()
        .map(|e| match e {
            Event::MessageDelta(d) => d.text.len(),
            _ => 0,
        })
        .sum();
    assert!(
        streamed < 12_000,
        "the stream was read to the end: {streamed}"
    );
    // The cut call still counts, by the larger estimate.
    let u = bot_limits::period_usage(
        &env.storage,
        env.user,
        env.bot,
        Period::Daily,
        storage::now_ms(),
    )
    .unwrap();
    assert!(u.tokens > 4_010, "{u:?}");
}

#[tokio::test]
async fn soft_alerts_are_not_repeated_after_a_restart() {
    let env = setup();
    set_limits(&env, &daily(|d| d.soft_usd = Some(18.0)));
    let warnings = |evs: &[Event]| -> Vec<String> {
        evs.iter()
            .filter_map(|e| match e {
                Event::Error(r) if r.message.contains(BOT_LIMIT_WARNING) => Some(r.message.clone()),
                _ => None,
            })
            .collect()
    };
    let mut all = Vec::new();
    // Run 1 spends $15, run 2 starts at 83% (80 alert); then the process "restarts".
    let fake = Fake::new(vec![text("ok"), text("ok")]);
    let lp = make_loop(&env.storage, env.bot, &fake, &[], profile(""), priced());
    let mut rx = lp.subscribe();
    for _ in 0..2 {
        let run = lp.run_turn(env.chat, "hi").unwrap();
        all.extend(finish(&lp, &mut rx, run).await.1);
    }
    drop(rx);
    drop(lp);
    // Fresh process state, same database: 80 is known, 100 is new, then both are quiet.
    let fake = Fake::new(vec![text("ok"), text("ok")]);
    let lp = make_loop(&env.storage, env.bot, &fake, &[], profile(""), priced());
    let mut rx = lp.subscribe();
    for _ in 0..2 {
        let run = lp.run_turn(env.chat, "hi").unwrap();
        all.extend(finish(&lp, &mut rx, run).await.1);
    }
    let w = warnings(&all);
    assert_eq!(
        w.iter().filter(|m| m.contains("level=80")).count(),
        1,
        "{w:?}"
    );
    assert_eq!(
        w.iter().filter(|m| m.contains("level=100")).count(),
        1,
        "{w:?}"
    );
}

fn started_run(lp: &AgentLoop, env: &Env) -> (storage::Run, std::sync::Arc<crate::shared::RunCtx>) {
    let id = lp
        .rt
        .create_run(env.user, env.bot, Some(env.chat), "turn")
        .unwrap();
    let row = env.storage.get_run(id).unwrap().unwrap();
    let ctx = lp.sh.ctx(&row).unwrap();
    (row, ctx)
}

#[tokio::test]
async fn a_huge_tool_call_stream_is_cut_by_the_hard_limit() {
    // Tool-call arguments are output too: about 12 KB of them cross a 5000 token cap.
    let env = setup();
    set_limits(&env, &daily(|d| d.hard_tokens = Some(5_000)));
    let chunks = (0..12)
        .map(|_| ChatChunk::ToolCallDelta {
            call_id: "c1".into(),
            name: None,
            input_delta: "z".repeat(1_000),
        })
        .collect();
    let fake = Fake::new(vec![Reply::Chunks(chunks)]);
    let lp = make_loop(&env.storage, env.bot, &fake, &[], profile(""), unpriced(10));
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "hi").unwrap();
    let (state, evs) = finish(&lp, &mut rx, run).await;
    assert_eq!(state, RunState::Failed);
    let msg = run_error(&evs).expect("typed error");
    assert!(msg.contains(BOT_LIMIT_REACHED), "{msg}");
}

#[tokio::test]
async fn a_cli_prompt_larger_than_the_cap_is_refused_before_the_session() {
    let env = setup();
    set_limits(&env, &daily(|d| d.hard_tokens = Some(10_000)));
    let fake = Fake::new(vec![]);
    let lp = make_loop(&env.storage, env.bot, &fake, &[], profile(""), unpriced(10));
    let (row, ctx) = started_run(&lp, &env);
    let executor = crate::exec::TurnExecutor { sh: lp.sh.clone() };
    let mut req = provider_catalog::ChatRequest::new(
        provider_catalog::ModelRef("cli:claude".into()),
        vec![provider_catalog::Message::text(
            provider_catalog::MessageRole::User,
            "x".repeat(60_000),
        )],
    );
    req.max_tokens = Some(10);
    let err = executor
        .stream_once(&ctx, &row, req, &[], crate::CallPurpose::Executor, None)
        .await
        .expect_err("must be refused");
    assert!(err.to_string().contains(BOT_LIMIT_REACHED), "{err}");
}

#[tokio::test]
async fn a_call_that_outgrows_its_reservation_blocks_the_next_admission() {
    // Cap 20_000. Call A is admitted at about 4_010 and streams to about 18_000
    // (still under the cap, so no cut). Call B (about 4_010 more) must now be refused.
    let env = setup();
    set_limits(&env, &daily(|d| d.hard_tokens = Some(20_000)));
    let fake = Fake::new(vec![]);
    let lp = make_loop(&env.storage, env.bot, &fake, &[], profile(""), unpriced(10));
    let (_, ctx_a) = started_run(&lp, &env);
    let (_, ctx_b) = started_run(&lp, &env);
    let mut a =
        crate::usage::CallGuard::start(&lp.sh, &ctx_a, "fake/m", crate::CallPurpose::Executor)
            .expect("A admitted");
    a.stream_progress(42_000).expect("A is still under the cap");
    let b = crate::usage::CallGuard::start(&lp.sh, &ctx_b, "fake/m", crate::CallPurpose::Executor);
    let err = b.err().expect("B must be refused while A is in flight");
    assert!(err.contains(BOT_LIMIT_REACHED), "{err}");
    drop(a);
}
