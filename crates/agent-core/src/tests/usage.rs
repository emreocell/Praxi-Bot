//! Call ledger tests use only scripted providers and temporary SQLite stores.
use super::*;

fn rows(env: &Env, action: &str) -> Vec<storage::AuditEntry> {
    env.storage
        .list_audit(env.user)
        .unwrap()
        .into_iter()
        .filter(|row| row.action == action)
        .collect()
}

#[tokio::test]
async fn cut_stream_cannot_dispatch_a_tool_and_settles_its_usage_once() {
    let env = setup();
    let read = Arc::new(tool("t.read", Effect::Read, "must never run"));
    let fake = Fake::new(vec![
        Reply::Chunks(vec![
            ChatChunk::ToolCall(ToolCall::new("cut", "t__read", "{}")),
            ChatChunk::ToolCallDelta {
                call_id: "second".into(),
                name: Some("t__read".into()),
                input_delta: "{\"partial\":".into(),
            },
            ChatChunk::Usage(Usage {
                prompt_tokens: 47,
                completion_tokens: 3,
                ..Usage::default()
            }),
        ]),
        text("recovered"),
    ]);
    let mut config = cfg(Policy::full());
    config.model_retry.max_attempts = 1;
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&read)],
        profile("fallback = [\"fake/backup\"]\n"),
        config,
    );
    let run = lp.run_turn(env.chat, "hi").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(fake.request(1).model.0, "fake/backup");
    assert_eq!(read.count.load(Ordering::SeqCst), 0);
    let settlements = rows(&env, "usage");
    assert_eq!(settlements.len(), 2);
    assert_eq!(
        settlements
            .iter()
            .filter(|r| r.detail["prompt_tokens"] == 47)
            .count(),
        1
    );
    assert_eq!(rows(&env, "llm.call_started").len(), 2);
}

async fn paused_after_model_call() -> (Env, AgentLoop, Arc<Fake>, protocol::RunId) {
    let env = setup();
    let write = Arc::new(tool("t.write", Effect::Write, "must stay unapproved"));
    let fake = Fake::new(vec![calls(&[("c1", "t.write", json!({}))])]);
    let mut config = cfg(Policy::approve_destructive());
    config.schedule_approval_timeout = Duration::from_millis(50);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&write)],
        profile(""),
        config,
    );
    let run = lp
        .run_scheduled(
            env.chat,
            protocol::TaskId::new(),
            crate::ScheduleWork::Prompt("go".into()),
        )
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Paused);
    assert_eq!(fake.n_requests(), 1);
    assert_eq!(write.count.load(Ordering::SeqCst), 0);
    assert!(!lp.sh.tasks.lock().contains_key(&run));
    assert!(rows(&env, "run.usage_summary").is_empty());
    (env, lp, fake, run)
}

#[tokio::test(start_paused = true)]
async fn ledger_idle_paused_cancellation_writes_one_summary_including_after_restart() {
    for restart in [false, true] {
        let (env, lp, fake, run) = paused_after_model_call().await;
        let lp = if restart {
            let recovered = make_loop(
                &env.storage,
                env.bot,
                &fake,
                &[],
                profile(""),
                cfg(Policy::full()),
            );
            recovered.recover().unwrap();
            recovered
        } else {
            lp
        };
        lp.cancel(run).unwrap();
        assert_eq!(lp.runtime().state(run).unwrap(), RunState::Cancelled);
        let summary = rows(&env, "run.usage_summary");
        assert_eq!(summary.len(), 1);
        assert_eq!(summary[0].run_id, Some(run));
        assert_eq!(summary[0].detail["executor"]["calls"], 1);
        assert_eq!(fake.n_requests(), 1);
        crate::usage::finish(&lp.sh, run);
        assert_eq!(rows(&env, "run.usage_summary").len(), 1);
    }
}

#[tokio::test(start_paused = true)]
async fn ledger_idle_wait_pc_failure_keeps_earlier_model_calls() {
    let (env, lp, fake, run) = paused_after_model_call().await;
    // Recover the durable state of a turn that already called a model and
    // then lost its computer while paused. The actual timeout handler runs.
    env.storage
        .set_run_pause_reason(run, Some(crate::WAIT_FOR_PC_REASON))
        .unwrap();
    lp.fail_waiting_pc_run(run, Duration::from_secs(60));
    lp.fail_waiting_pc_run(run, Duration::from_secs(60));
    assert_eq!(lp.runtime().state(run).unwrap(), RunState::Failed);
    let summary = rows(&env, "run.usage_summary");
    assert_eq!(summary.len(), 1);
    assert_eq!(summary[0].detail["executor"]["calls"], 1);
    assert_eq!(fake.n_requests(), 1);
}

#[tokio::test]
async fn ledger_counts_four_executor_calls_even_without_token_usage() {
    let env = setup();
    let read = Arc::new(tool("t.read", Effect::Read, "tool-output-sentinel"));
    let fake = Fake::new(vec![
        calls(&[("a", "t.read", json!({"n": 1}))]),
        calls(&[("b", "t.read", json!({"n": 2}))]),
        calls(&[("c", "t.read", json!({"n": 3}))]),
        text("assistant-output-sentinel"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[&read],
        profile(""),
        cfg(Policy::full()),
    );
    let run = lp.run_turn(env.chat, "user-prompt-sentinel").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    let usage = rows(&env, "usage");
    assert_eq!(usage.len(), 4);
    assert!(
        usage
            .iter()
            .all(|r| r.detail["purpose"] == "executor" && r.detail["calls"] == 1)
    );
    assert!(usage[..3].iter().all(|r| r.detail["tokens"].is_null()));
    assert_eq!(usage[3].detail["tokens"]["prompt_tokens"], 10);
    assert_eq!(
        usage
            .iter()
            .map(|r| r.detail["tool_rounds"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [0, 1, 2, 3]
    );
    assert_eq!(rows(&env, "llm.call_started").len(), 4);
    let summary = rows(&env, "run.usage_summary");
    assert_eq!(summary.len(), 1);
    assert_eq!(summary[0].detail["executor"]["calls"], 4);
    assert_eq!(summary[0].detail["executor"]["prompt_tokens"], 10);
    let encoded = usage
        .iter()
        .map(|r| r.detail.to_string())
        .collect::<String>();
    for content in [
        "user-prompt-sentinel",
        "tool-output-sentinel",
        "assistant-output-sentinel",
    ] {
        assert!(!encoded.contains(content));
    }
    // Positive control: this exact user content really reached the fake.
    assert!(
        serde_json::to_string(&fake.request(0))
            .unwrap()
            .contains("user-prompt-sentinel")
    );
}

#[tokio::test]
async fn ledger_guard_close_is_distinct_from_executor() {
    let env = setup();
    let read = Arc::new(tool("t.read", Effect::Read, "x"));
    let fake = Fake::new(vec![
        calls(&[("a", "t.read", json!({"n":1}))]),
        text("closing"),
    ]);
    let mut config = cfg(Policy::full());
    config.guards.max_steps = 1;
    let lp = make_loop(&env.storage, env.bot, &fake, &[&read], profile(""), config);
    let run = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert!(lp.stopped_by_guard(run).unwrap());
    let usage = rows(&env, "usage");
    assert_eq!(usage.len(), 2);
    assert_eq!(usage[0].detail["purpose"], "executor");
    assert_eq!(usage[1].detail["purpose"], "guard_close");
    assert_eq!(
        rows(&env, "run.usage_summary")[0].detail["guard_close"]["calls"],
        1
    );
}

#[tokio::test]
async fn ledger_counts_failed_attempts_and_provider_retries() {
    let env = setup();
    let fake = Fake::new(vec![Reply::Fail("upstream"), text("recovered")]);
    let mut config = cfg(Policy::full());
    config.model_retry.max_attempts = 2;
    let lp = make_loop(&env.storage, env.bot, &fake, &[], profile(""), config);
    let run = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    let usage = rows(&env, "usage");
    assert_eq!(usage.len(), fake.n_requests());
    assert_eq!(usage.len(), 2);
    assert!(usage[0].detail["tokens"].is_null());
    assert_eq!(
        rows(&env, "run.usage_summary")[0].detail["executor"]["calls"],
        2
    );
}

#[tokio::test(start_paused = true)]
async fn ledger_keeps_a_timed_out_invocation_with_unknown_tokens() {
    let env = setup();
    let fake = Fake::new(vec![Reply::Hang]);
    let mut config = cfg(Policy::full());
    config.guards.model_timeout = Duration::from_millis(20);
    let lp = make_loop(&env.storage, env.bot, &fake, &[], profile(""), config);
    let run = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Failed);
    assert_eq!(fake.n_requests(), 1);
    let usage = rows(&env, "usage");
    assert_eq!(usage.len(), 1);
    assert!(usage[0].detail["tokens"].is_null());
    assert_eq!(
        rows(&env, "run.usage_summary")[0].detail["executor"]["calls"],
        1
    );
}

#[tokio::test]
async fn ledger_memory_and_single_summary_include_background_calls() {
    let env = setup();
    let (_, memory) = memory_cfg();
    let fake = Fake::new(vec![text("hello"), text("[]")]);
    let mut config = cfg(Policy::full());
    config.memory = Some(memory);
    let lp = make_loop(&env.storage, env.bot, &fake, &[], profile(""), config);
    let run = lp.run_turn(env.chat, "I am Ayse").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    lp.flush_background().await;
    let usage = rows(&env, "usage");
    assert_eq!(usage.len(), 2);
    assert_eq!(usage[1].run_id, Some(run));
    assert_eq!(usage[1].detail["purpose"], "memory");
    assert_eq!(usage[1].detail["tokens"]["completion_tokens"], 5);
    let summaries = rows(&env, "run.usage_summary");
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].detail["executor"]["calls"], 1);
    assert_eq!(summaries[0].detail["memory"]["calls"], 1);
    assert!(!lp.sh.runs.lock().contains_key(&run));
    crate::usage::finish(&lp.sh, run);
    assert_eq!(rows(&env, "run.usage_summary").len(), 1);
}

#[tokio::test]
async fn ledger_model_name_uses_the_production_redactor() {
    let env = setup();
    let secret = "ledger-model-secret-5005100";
    let mut prof = profile("");
    prof.models.executor = format!("fake/{secret}");
    let fake = Fake::new(vec![text("answer")]);
    let mut config = cfg(Policy::full());
    config.redactor = Some(redactor_with(secret));
    let lp = make_loop(&env.storage, env.bot, &fake, &[], prof, config);
    let run = lp
        .run_turn(env.chat, "user-sentinel-not-for-audit")
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    for action in ["llm.call_started", "usage", "run.usage_summary"] {
        let records = rows(&env, action);
        assert!(!records.is_empty());
        for record in records {
            let encoded = record.detail.to_string();
            assert!(!encoded.contains(secret));
            assert!(!encoded.contains("user-sentinel-not-for-audit"));
        }
    }
    assert_ne!(
        rows(&env, "usage")[0].detail["model"],
        format!("fake/{secret}")
    );
}

#[tokio::test]
async fn ledger_audit_intent_failure_prevents_the_provider_call() {
    let env = setup();
    let conn = rusqlite::Connection::open(env.dir.path().join("bot.db")).unwrap();
    conn.execute_batch("CREATE TRIGGER block_model_ledger BEFORE INSERT ON audit_log WHEN NEW.action = 'llm.call_started' BEGIN SELECT RAISE(FAIL, 'unavailable'); END;").unwrap();
    let fake = Fake::new(vec![text("must not call")]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[],
        profile(""),
        cfg(Policy::full()),
    );
    let run = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Failed);
    assert_eq!(fake.n_requests(), 0);
    assert!(rows(&env, "usage").is_empty());
}
