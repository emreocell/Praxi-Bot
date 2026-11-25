//! Stop must end the run within about a second in every phase the owner hits
//! the button: an idle model call, a stream, a tool, an approval, a queued
//! follow-up, a hanging browser open, and a long schedule run.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use permission_engine::Policy;
use provider_catalog::{ChatChunk, ChatProvider, ChatRequest, ProviderError, Usage};
use serde_json::json;
use task_runtime::RunState;
use tool_registry::{
    Effect, PermissionCategory, Platform, Tool, ToolContext, ToolError, ToolOutput, ToolRegistry,
    ToolSpec,
};

use protocol::Event;

use crate::tests::{Fake, Reply, cfg, make_loop, profile, setup, text, tool, until};
use crate::{AgentLoop, MessageMode, Providers, ScheduleWork};

const LIMIT: Duration = Duration::from_millis(1500);

async fn cancelled_within(lp: &AgentLoop, run: protocol::RunId) -> RunState {
    let state = tokio::time::timeout(LIMIT, async {
        loop {
            if let Ok(state) = lp.runtime().state(run)
                && state.is_terminal()
            {
                return state;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("Stop left the run non-terminal");
    assert_eq!(state, RunState::Cancelled);
    assert_ne!(lp.runtime().state(run).unwrap(), RunState::Running);
    state
}

fn spec(name: &str) -> ToolSpec {
    ToolSpec {
        name: name.to_owned(),
        description: "stop fixture".into(),
        input_schema: json!({"type": "object"}),
        effect: Effect::Read,
        category: PermissionCategory::Fs,
        platforms: vec![Platform::All],
        content: tool_registry::ContentClass::none(),
    }
}

struct FlagTool {
    spec: ToolSpec,
    started: Arc<AtomicBool>,
    dropped: Arc<AtomicBool>,
    block: bool,
}

struct DropFlag(Arc<AtomicBool>);
impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[async_trait]
impl Tool for FlagTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }
    async fn call(&self, _: &ToolContext, _: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let _flag = DropFlag(Arc::clone(&self.dropped));
        self.started.store(true, Ordering::SeqCst);
        if self.block {
            // A blocking wait does not poll the run's cancel token.
            std::thread::sleep(Duration::from_secs(4));
        } else {
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
        Ok(ToolOutput::text("should not finish"))
    }
}

fn loop_with(env: &crate::tests::Env, tool: Arc<dyn Tool>, fake: Arc<Fake>) -> AgentLoop {
    let registry = ToolRegistry::new();
    registry.register(tool);
    let lp = AgentLoop::new(
        Arc::clone(&env.storage),
        registry,
        Providers::single(fake),
        cfg(Policy::full()),
    );
    lp.add_profile(env.bot, profile(""));
    lp
}

async fn until_flag(flag: &AtomicBool) {
    for _ in 0..500 {
        if flag.load(Ordering::SeqCst) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the phase never started");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_aborts_an_idle_model_call_within_a_second() {
    let env = setup();
    let fake = Fake::new(vec![Reply::Hang, text("must not be asked")]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[],
        profile(""),
        cfg(Policy::full()),
    );
    let run = lp.run_turn(env.chat, "hello").unwrap();
    for _ in 0..500 {
        if fake.n_requests() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(fake.n_requests(), 1, "the model call started");
    lp.cancel(run).unwrap();
    cancelled_within(&lp, run).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        fake.n_requests(),
        1,
        "Stop must not start another model call"
    );
}

/// One text delta, then silence. Dropping the stream is the abort.
struct StreamThenHang {
    drops: Arc<AtomicUsize>,
}

struct StreamDrop(Arc<AtomicUsize>);
impl Drop for StreamDrop {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl ChatProvider for StreamThenHang {
    fn id(&self) -> &'static str {
        "fake"
    }
    async fn stream(
        &self,
        _: ChatRequest,
    ) -> Result<BoxStream<'static, Result<ChatChunk, ProviderError>>, ProviderError> {
        let drops = Arc::clone(&self.drops);
        let chunks = vec![
            Ok(ChatChunk::TextDelta("partial ".into())),
            Ok(ChatChunk::Usage(Usage {
                prompt_tokens: 1,
                completion_tokens: 1,
                cached_tokens: None,
                cache_write_tokens: None,
            })),
        ];
        let stream = futures::stream::iter(chunks).chain(futures::stream::pending());
        Ok(futures::stream::unfold(
            (Box::pin(stream), StreamDrop(drops)),
            |(mut inner, flag)| async move {
                let item = inner.next().await?;
                Some((item, (inner, flag)))
            },
        )
        .boxed())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_aborts_a_streaming_model_call_and_drops_the_request() {
    let env = setup();
    let drops = Arc::new(AtomicUsize::new(0));
    let provider = Arc::new(StreamThenHang {
        drops: Arc::clone(&drops),
    });
    let lp = AgentLoop::new(
        Arc::clone(&env.storage),
        ToolRegistry::new(),
        Providers::single(provider),
        cfg(Policy::full()),
    );
    lp.add_profile(env.bot, profile(""));
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "hello").unwrap();
    until(&mut rx, |e| matches!(e, Event::MessageDelta(_))).await;
    lp.cancel(run).unwrap();
    cancelled_within(&lp, run).await;
    for _ in 0..50 {
        if drops.load(Ordering::SeqCst) > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        drops.load(Ordering::SeqCst) > 0,
        "the stream was not aborted"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_drops_a_running_tool_within_a_second() {
    let env = setup();
    let started = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicBool::new(false));
    let tool = Arc::new(FlagTool {
        spec: spec("t.slow"),
        started: Arc::clone(&started),
        dropped: Arc::clone(&dropped),
        block: false,
    });
    let fake = Fake::new(vec![
        crate::tests::calls(&[("c1", "t.slow", json!({}))]),
        text("must not run"),
    ]);
    let lp = loop_with(&env, tool, fake);
    let run = lp.run_turn(env.chat, "go").unwrap();
    until_flag(&started).await;
    lp.cancel(run).unwrap();
    cancelled_within(&lp, run).await;
    assert!(
        dropped.load(Ordering::SeqCst),
        "the tool future was not dropped"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_denies_a_pending_approval_and_does_not_leave_the_run_waiting() {
    let env = setup();
    let write = Arc::new(tool("t.write", Effect::Write, "nope"));
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
    until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await;
    lp.cancel(run).unwrap();
    cancelled_within(&lp, run).await;
    assert_eq!(
        env.storage.list_approvals(run).unwrap()[0]
            .decision
            .as_deref(),
        Some("denied"),
        "Stop is the owner saying no, not a timeout"
    );
    assert_eq!(write.count.load(Ordering::SeqCst), 0);
    assert!(lp.sh.pending.lock().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_keeps_queued_messages_and_does_not_start_them() {
    let env = setup();
    let mut slow = tool("t.slow", Effect::Read, "r");
    slow.delay = Duration::from_secs(30);
    let slow = Arc::new(slow);
    let fake = Fake::new(vec![
        crate::tests::calls(&[("c1", "t.slow", json!({}))]),
        text("must not be asked for"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[crate::tests::t(&slow)],
        profile(""),
        cfg(Policy::full()),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "one").unwrap();
    until(&mut rx, |e| matches!(e, Event::ToolCallRequested(_))).await;
    assert_eq!(
        lp.send_user_message(env.chat, "two", MessageMode::Queue)
            .unwrap(),
        crate::Delivery::Queued
    );
    lp.cancel(run).unwrap();
    cancelled_within(&lp, run).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(fake.n_requests(), 1, "the queued message started a turn");
    let queued = lp
        .sh
        .queued
        .lock()
        .get(&env.chat)
        .and_then(|q| q.front().map(|(text, _)| text.clone()));
    assert_eq!(queued.as_deref(), Some("two"));
    assert!(
        lp.run_turn(env.chat, "three").is_ok(),
        "the chat is free again"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_aborts_a_hanging_browser_open_and_drops_the_tool() {
    let env = setup();
    let started = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicBool::new(false));
    let browser = Arc::new(FlagTool {
        spec: spec("browser_open"),
        started: Arc::clone(&started),
        dropped: Arc::clone(&dropped),
        block: false,
    });
    let fake = Fake::new(vec![
        crate::tests::calls(&[("c1", "browser_open", json!({"url": "https://example.test"}))]),
        text("must not run"),
    ]);
    let lp = loop_with(&env, browser, fake);
    let run = lp.run_turn(env.chat, "open the page").unwrap();
    until_flag(&started).await;
    lp.cancel(run).unwrap();
    cancelled_within(&lp, run).await;
    assert!(
        dropped.load(Ordering::SeqCst),
        "the hanging page open was not aborted"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_ends_a_long_schedule_run_within_a_second() {
    let env = setup();
    let fake = Fake::new(vec![Reply::Hang, text("must not be asked")]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[],
        profile(""),
        cfg(Policy::full()),
    );
    let run = lp
        .run_scheduled(
            env.chat,
            protocol::TaskId::new(),
            ScheduleWork::Prompt("daily report".into()),
        )
        .unwrap();
    for _ in 0..500 {
        if fake.n_requests() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(fake.n_requests(), 1);
    let started = Instant::now();
    lp.cancel(run).unwrap();
    cancelled_within(&lp, run).await;
    assert!(started.elapsed() < LIMIT);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_ends_a_blocking_tool_that_never_polls_cancel() {
    let env = setup();
    let started = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicBool::new(false));
    let tool = Arc::new(FlagTool {
        spec: spec("t.block"),
        started: Arc::clone(&started),
        dropped: Arc::clone(&dropped),
        block: true,
    });
    let fake = Fake::new(vec![
        crate::tests::calls(&[("c1", "t.block", json!({}))]),
        text("must not run"),
    ]);
    let lp = loop_with(&env, tool, fake);
    let run = lp.run_turn(env.chat, "go").unwrap();
    until_flag(&started).await;
    let started_at = Instant::now();
    lp.cancel(run).unwrap();
    cancelled_within(&lp, run).await;
    assert!(
        started_at.elapsed() < LIMIT,
        "a blocking tool kept the run alive for {:?}",
        started_at.elapsed()
    );
}
