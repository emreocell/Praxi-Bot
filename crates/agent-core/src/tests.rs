//! Deterministic loop tests driven by a scripted fake `ChatProvider`.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use parking_lot::Mutex;
use permission_engine::Policy;
use protocol::event::TodoStatus;
use protocol::{BotId, ChatId, Event, UserId};
use provider_catalog::{
    ChatChunk, ChatProvider, ChatRequest, FinishReason, MessageRole, ProviderError, ToolCall, Usage,
};
use serde_json::{Value, json};
use storage::{Bot, Chat, Storage, User, now_ms, read_event_log};
use task_runtime::{RetryPolicy, RunState};
use tokio::sync::{Barrier, broadcast};
use tool_registry::{
    Effect, PermissionCategory, Platform, Tool, ToolContext, ToolError, ToolOutput, ToolRegistry,
    ToolSpec,
};

use crate::{
    AgentLoop, ApprovalScope, BotProfile, Delivery, LoopConfig, MessageMode, Providers, StreamItem,
};

// ---------- fakes ----------

enum Reply {
    Chunks(Vec<ChatChunk>),
    Hang,
}

fn text(s: &str) -> Reply {
    Reply::Chunks(vec![
        ChatChunk::TextDelta(s.to_owned()),
        ChatChunk::Usage(Usage {
            prompt_tokens: 10,
            completion_tokens: 5,
            cached_tokens: Some(2),
        }),
        ChatChunk::Done {
            finish_reason: FinishReason::Stop,
        },
    ])
}

fn calls(cs: &[(&str, &str, Value)]) -> Reply {
    let mut v: Vec<ChatChunk> = cs
        .iter()
        .map(|(id, name, args)| {
            ChatChunk::ToolCall(ToolCall {
                id: (*id).to_owned(),
                // The model sees dotted names with `.` replaced.
                name: name.replace('.', "__"),
                arguments: args.to_string(),
            })
        })
        .collect();
    v.push(ChatChunk::Done {
        finish_reason: FinishReason::ToolCalls,
    });
    Reply::Chunks(v)
}

struct Fake {
    replies: Mutex<VecDeque<Reply>>,
    requests: Mutex<Vec<ChatRequest>>,
}

impl Fake {
    fn new(replies: Vec<Reply>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies.into()),
            requests: Mutex::new(Vec::new()),
        })
    }
    fn request(&self, i: usize) -> ChatRequest {
        self.requests.lock()[i].clone()
    }
    fn n_requests(&self) -> usize {
        self.requests.lock().len()
    }
}

#[async_trait]
impl ChatProvider for Fake {
    fn id(&self) -> &'static str {
        "fake"
    }
    async fn stream(
        &self,
        req: ChatRequest,
    ) -> Result<BoxStream<'static, Result<ChatChunk, ProviderError>>, ProviderError> {
        self.requests.lock().push(req);
        match self.replies.lock().pop_front() {
            Some(Reply::Chunks(c)) => Ok(futures::stream::iter(c.into_iter().map(Ok)).boxed()),
            Some(Reply::Hang) => Ok(futures::stream::pending().boxed()),
            None => Err(ProviderError::InvalidRequest("script exhausted".into())),
        }
    }
}

struct TestTool {
    spec: ToolSpec,
    count: Arc<AtomicUsize>,
    delay: Duration,
    barrier: Option<Arc<Barrier>>,
    output: String,
}

fn tool(name: &str, effect: Effect, output: &str) -> TestTool {
    TestTool {
        spec: ToolSpec {
            name: name.to_owned(),
            description: format!("test tool {name}"),
            input_schema: json!({"type": "object"}),
            effect,
            category: PermissionCategory::Fs,
            platforms: vec![Platform::All],
        },
        count: Arc::new(AtomicUsize::new(0)),
        delay: Duration::ZERO,
        barrier: None,
        output: output.to_owned(),
    }
}

#[async_trait]
impl Tool for TestTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }
    async fn call(&self, _: &ToolContext, _: Value) -> Result<ToolOutput, ToolError> {
        self.count.fetch_add(1, Ordering::SeqCst);
        if let Some(b) = &self.barrier {
            b.wait().await;
        }
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        Ok(ToolOutput::text(self.output.clone()))
    }
}

// ---------- harness ----------

struct Env {
    dir: tempfile::TempDir,
    storage: Arc<Storage>,
    user: UserId,
    bot: BotId,
    chat: ChatId,
}

fn setup() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let storage = Arc::new(Storage::open(dir.path()).unwrap());
    let (user, bot, chat) = (UserId::new(), BotId::new(), ChatId::new());
    storage
        .insert_user(&User {
            id: user,
            name: "u".into(),
            created_at_ms: now_ms(),
        })
        .unwrap();
    storage
        .insert_bot(&Bot {
            id: bot,
            user_id: user,
            name: "b".into(),
            config: json!({}),
            created_at_ms: now_ms(),
        })
        .unwrap();
    storage
        .insert_chat(&Chat {
            id: chat,
            user_id: user,
            bot_id: bot,
            title: "c".into(),
            created_at_ms: now_ms(),
        })
        .unwrap();
    Env {
        dir,
        storage,
        user,
        bot,
        chat,
    }
}

fn profile(extra: &str) -> BotProfile {
    BotProfile::from_toml_str(&format!(
        "[bot]\nid = \"t\"\nname = \"T\"\npersona = \"You are T.\"\n[models]\nexecutor = \"fake/m\"\n{extra}"
    ))
    .unwrap()
}

fn cfg(policy: Policy) -> LoopConfig {
    LoopConfig {
        global_policy: policy,
        model_retry: RetryPolicy {
            max_attempts: 1,
            initial_backoff_ms: 1,
            multiplier: 1,
            max_backoff_ms: 1,
        },
        ..LoopConfig::default()
    }
}

fn make_loop(
    storage: &Arc<Storage>,
    bot: BotId,
    fake: &Arc<Fake>,
    tools: &[&Arc<TestTool>],
    prof: BotProfile,
    config: LoopConfig,
) -> AgentLoop {
    let registry = ToolRegistry::new();
    for t in tools {
        registry.register(Arc::clone(t) as Arc<dyn Tool>);
    }
    let lp = AgentLoop::new(
        Arc::clone(storage),
        registry,
        Providers::single(Arc::clone(fake) as Arc<dyn ChatProvider>),
        config,
    );
    lp.add_profile(bot, prof);
    lp
}

async fn until(
    rx: &mut broadcast::Receiver<StreamItem>,
    mut f: impl FnMut(&Event) -> bool,
) -> Event {
    loop {
        let item = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("timed out waiting for event")
            .expect("stream closed");
        if f(&item.envelope.event) {
            return item.envelope.event;
        }
    }
}

fn tool_messages(req: &ChatRequest) -> Vec<String> {
    req.messages
        .iter()
        .filter(|m| m.role == MessageRole::Tool)
        .map(|m| m.content.as_str().unwrap_or_default().to_owned())
        .collect()
}

fn t<T>(x: &Arc<T>) -> &Arc<T> {
    x
}

// ---------- tests ----------

#[tokio::test]
async fn text_only_turn_persists_streams_and_records_usage() {
    let env = setup();
    let fake = Fake::new(vec![text("hello there")]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[],
        profile(""),
        cfg(Policy::full()),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "hi").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);

    let mut evs = Vec::new();
    while let Ok(i) = rx.try_recv() {
        evs.push(i.envelope.event);
    }
    assert!(matches!(evs.first(), Some(Event::RunStarted(_))));
    assert!(
        evs.iter()
            .any(|e| matches!(e, Event::MessageDelta(d) if d.text == "hello there"))
    );
    assert!(matches!(evs.last(), Some(Event::RunFinished(f)) if f.error.is_none()));
    // NDJSON log mirrors the stream.
    assert_eq!(
        read_event_log(env.dir.path(), run).unwrap().len(),
        evs.len()
    );

    let msgs = env.storage.list_messages(env.chat).unwrap();
    assert_eq!(msgs.len(), 2);
    assert_eq!(
        (msgs[0].role.as_str(), msgs[0].content.as_str()),
        ("user", "hi")
    );
    assert_eq!(
        (msgs[1].role.as_str(), msgs[1].content.as_str()),
        ("assistant", "hello there")
    );

    let usage: Vec<_> = env
        .storage
        .list_audit(env.user)
        .unwrap()
        .into_iter()
        .filter(|a| a.action == "usage")
        .collect();
    assert_eq!(usage.len(), 1);
    assert_eq!(usage[0].detail["prompt_tokens"], 10);
    assert_eq!(usage[0].detail["cached_tokens"], 2);

    // System persona + user message went to the provider.
    let req = fake.request(0);
    assert_eq!(req.messages[0].role, MessageRole::System);
    assert!(
        req.messages[0]
            .content
            .as_str()
            .unwrap()
            .starts_with("You are T.")
    );
}

#[tokio::test]
async fn allowed_tool_call_executes_and_result_reaches_model() {
    let env = setup();
    let w = Arc::new(tool("t.write", Effect::Write, "written"));
    let fake = Fake::new(vec![
        calls(&[("c1", "t.write", json!({"p": 1}))]),
        text("done"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&w)],
        profile(""),
        cfg(Policy::full()),
    );
    let run = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(w.count.load(Ordering::SeqCst), 1);
    let req = fake.request(1);
    assert_eq!(tool_messages(&req), vec!["written".to_owned()]);
    // Dotted names are exposed with `__`.
    let names: Vec<String> = fake
        .request(0)
        .tools
        .iter()
        .map(|d| d["function"]["name"].as_str().unwrap().to_owned())
        .collect();
    assert!(names.contains(&"t__write".to_owned()));
    assert!(names.contains(&"plan__todo_write".to_owned()));
    // Steps: user_message, model, tools, model.
    let steps = env.storage.list_steps(run).unwrap();
    assert_eq!(
        steps.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        ["user_message", "model", "tools", "model"]
    );
    assert!(!steps[2].idempotent, "write calls are never replayable");
}

#[tokio::test]
async fn ask_then_approve_continues() {
    let env = setup();
    let w = Arc::new(tool("t.write", Effect::Write, "written"));
    let fake = Fake::new(vec![calls(&[("c1", "t.write", json!({}))]), text("done")]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&w)],
        profile(""),
        cfg(Policy::approve_destructive()),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();

    let Event::ApprovalNeeded(a) = until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
    else {
        unreachable!()
    };
    assert_eq!(a.tool_name, "t.write");
    assert_eq!(lp.runtime().state(run).unwrap(), RunState::WaitingApproval);
    assert_eq!(
        w.count.load(Ordering::SeqCst),
        0,
        "must not run before approval"
    );

    lp.approve(a.call_id, ApprovalScope::Once, env.user)
        .unwrap();
    assert!(
        lp.approve(a.call_id, ApprovalScope::Once, env.user)
            .is_err(),
        "decided once"
    );
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(w.count.load(Ordering::SeqCst), 1);
    let approvals = env.storage.list_approvals(run).unwrap();
    assert_eq!(approvals.len(), 1);
    assert_eq!(approvals[0].decision.as_deref(), Some("approved"));
}

#[tokio::test]
async fn ask_then_deny_tells_model_and_skips_tool() {
    let env = setup();
    let w = Arc::new(tool("t.write", Effect::Write, "written"));
    let fake = Fake::new(vec![
        calls(&[("c1", "t.write", json!({}))]),
        text("ok, not doing it"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&w)],
        profile(""),
        cfg(Policy::approve_destructive()),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let Event::ApprovalNeeded(a) = until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
    else {
        unreachable!()
    };
    lp.deny(a.call_id, env.user, Some("too risky".into()))
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(w.count.load(Ordering::SeqCst), 0);
    let msgs = tool_messages(&fake.request(1));
    assert_eq!(msgs.len(), 1);
    assert!(
        msgs[0].contains("denied by the user") && msgs[0].contains("too risky"),
        "{msgs:?}"
    );
    assert_eq!(
        env.storage.list_approvals(run).unwrap()[0]
            .decision
            .as_deref(),
        Some("denied")
    );
}

#[tokio::test]
async fn policy_deny_never_asks() {
    let env = setup();
    let w = Arc::new(tool("t.write", Effect::Write, "written"));
    let fake = Fake::new(vec![calls(&[("c1", "t.write", json!({}))]), text("fine")]);
    // Bot policy (read-only preset = deny) beats the permissive global policy.
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&w)],
        profile("[permissions]\npreset = \"read-only\"\n"),
        cfg(Policy::full()),
    );
    let run = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(w.count.load(Ordering::SeqCst), 0);
    assert!(tool_messages(&fake.request(1))[0].contains("Permission denied"));
    assert_eq!(env.storage.list_approvals(run).unwrap().len(), 0);
}

#[tokio::test]
async fn independent_reads_run_concurrently_in_one_step() {
    let env = setup();
    let barrier = Arc::new(Barrier::new(2));
    let mut a = tool("t.read_a", Effect::Read, "A");
    let mut b = tool("t.read_b", Effect::Read, "B");
    a.barrier = Some(Arc::clone(&barrier));
    b.barrier = Some(Arc::clone(&barrier));
    let (a, b) = (Arc::new(a), Arc::new(b));
    let fake = Fake::new(vec![
        calls(&[("c1", "t.read_a", json!({})), ("c2", "t.read_b", json!({}))]),
        text("both"),
    ]);
    let mut config = cfg(Policy::full());
    config.guards.tool_timeout = Duration::from_secs(3);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&a), t(&b)],
        profile(""),
        config,
    );
    let run = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    // A sequential run would deadlock on the barrier and hit the timeout.
    assert_eq!(
        tool_messages(&fake.request(1)),
        vec!["A".to_owned(), "B".to_owned()]
    );
    let steps = env.storage.list_steps(run).unwrap();
    assert_eq!(steps.iter().filter(|s| s.name == "tools").count(), 1);
    assert!(steps[2].idempotent);
}

#[tokio::test]
async fn writes_run_sequentially_one_step_each() {
    let env = setup();
    let r = Arc::new(tool("t.read", Effect::Read, "R"));
    let w = Arc::new(tool("t.write", Effect::Write, "W"));
    let fake = Fake::new(vec![
        calls(&[
            ("c1", "t.read", json!({})),
            ("c2", "t.write", json!({})),
            ("c3", "t.read", json!({"x": 1})),
        ]),
        text("ok"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&r), t(&w)],
        profile(""),
        cfg(Policy::full()),
    );
    let run = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    let steps = env.storage.list_steps(run).unwrap();
    assert_eq!(steps.iter().filter(|s| s.name == "tools").count(), 3);
    assert_eq!(tool_messages(&fake.request(1)), vec!["R", "W", "R"]);
}

#[tokio::test]
async fn repeat_detector_warns_then_stops() {
    let env = setup();
    let r = Arc::new(tool("t.read", Effect::Read, "same"));
    let same = |id: &str| calls(&[(id, "t.read", json!({"q": "x"}))]);
    let fake = Fake::new(vec![
        same("1"),
        same("2"),
        same("3"),
        same("4"),
        same("5"),
        text("unreachable"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&r)],
        profile(""),
        cfg(Policy::full()),
    );
    let run = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Failed);
    let err = env.storage.get_run(run).unwrap().unwrap().error.unwrap();
    assert!(err.contains("repeat detected"), "{err}");
    assert_eq!(
        r.count.load(Ordering::SeqCst),
        4,
        "5th identical call is never executed"
    );
    assert_eq!(fake.n_requests(), 5);
    let third = tool_messages(&fake.request(3));
    assert!(!third[1].contains("warning"));
    assert!(
        third[2].contains("warning"),
        "3rd identical call carries the warning: {third:?}"
    );
}

#[tokio::test]
async fn max_steps_guard_stops_the_turn() {
    let env = setup();
    let r = Arc::new(tool("t.read", Effect::Read, "x"));
    let fake = Fake::new(vec![
        calls(&[("1", "t.read", json!({"n": 1}))]),
        calls(&[("2", "t.read", json!({"n": 2}))]),
        text("never"),
    ]);
    let mut config = cfg(Policy::full());
    config.guards.max_steps = 2;
    let lp = make_loop(&env.storage, env.bot, &fake, &[t(&r)], profile(""), config);
    let run = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Failed);
    assert!(
        env.storage
            .get_run(run)
            .unwrap()
            .unwrap()
            .error
            .unwrap()
            .contains("step limit")
    );
    assert_eq!(fake.n_requests(), 2);
}

#[tokio::test]
async fn token_budget_guard_stops_the_turn() {
    let env = setup();
    let r = Arc::new(tool("t.read", Effect::Read, "x"));
    let with_usage = |id: &str, n: u32| match calls(&[(id, "t.read", json!({"n": n}))]) {
        Reply::Chunks(mut c) => {
            c.insert(
                0,
                ChatChunk::Usage(Usage {
                    prompt_tokens: 100,
                    completion_tokens: 0,
                    cached_tokens: None,
                }),
            );
            Reply::Chunks(c)
        }
        Reply::Hang => unreachable!(),
    };
    let fake = Fake::new(vec![with_usage("1", 1), text("never")]);
    let mut config = cfg(Policy::full());
    config.guards.max_tokens_per_turn = 50;
    let lp = make_loop(&env.storage, env.bot, &fake, &[t(&r)], profile(""), config);
    let run = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Failed);
    assert!(
        env.storage
            .get_run(run)
            .unwrap()
            .unwrap()
            .error
            .unwrap()
            .contains("token budget")
    );
}

#[tokio::test]
async fn tool_timeout_becomes_an_error_result() {
    let env = setup();
    let mut slow = tool("t.slow", Effect::Read, "late");
    slow.delay = Duration::from_secs(30);
    let slow = Arc::new(slow);
    let fake = Fake::new(vec![
        calls(&[("c1", "t.slow", json!({}))]),
        text("moving on"),
    ]);
    let mut config = cfg(Policy::full());
    config.guards.tool_timeout = Duration::from_millis(100);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&slow)],
        profile(""),
        config,
    );
    let run = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert!(tool_messages(&fake.request(1))[0].contains("timed out"));
}

#[tokio::test]
async fn large_output_is_spilled_with_preview_and_path() {
    let env = setup();
    let big = "x".repeat(40 * 1024);
    let r = Arc::new(tool("t.read", Effect::Read, &big));
    let fake = Fake::new(vec![calls(&[("c1", "t.read", json!({}))]), text("ok")]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&r)],
        profile(""),
        cfg(Policy::full()),
    );
    let run = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    let msg = tool_messages(&fake.request(1)).remove(0);
    assert!(
        msg.len() < 4 * 1024,
        "model only sees a preview ({} bytes)",
        msg.len()
    );
    let marker = "full output saved to ";
    let path = PathBuf::from(
        msg.split(marker)
            .nth(1)
            .expect("path in message")
            .trim_end_matches(']'),
    );
    assert!(path.starts_with(env.dir.path().join("spill")));
    assert_eq!(std::fs::read_to_string(path).unwrap().len(), big.len());
}

#[tokio::test]
async fn todo_tool_emits_event_without_permission() {
    let env = setup();
    let fake = Fake::new(vec![
        calls(&[(
            "c1",
            "plan.todo_write",
            json!({"todos": [{"text": "a", "status": "done"}, {"text": "b", "status": "in_progress"}]}),
        )]),
        text("planned"),
    ]);
    // Default policy would Ask for everything; the built-in plan tool is exempt.
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[],
        profile(""),
        cfg(Policy::approve_destructive()),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "plan it").unwrap();
    let Event::TodoUpdated(u) = until(&mut rx, |e| matches!(e, Event::TodoUpdated(_))).await else {
        unreachable!()
    };
    assert_eq!(u.items.len(), 2);
    assert_eq!(u.items[0].status, TodoStatus::Done);
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert!(tool_messages(&fake.request(1))[0].contains("1/2 done"));
}

#[tokio::test]
async fn bot_tool_allow_exclude_hides_and_blocks_tools() {
    let env = setup();
    let r = Arc::new(tool("t.read", Effect::Read, "r"));
    let w = Arc::new(tool("t.write", Effect::Write, "w"));
    let fake = Fake::new(vec![calls(&[("c1", "t.write", json!({}))]), text("ok")]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&r), t(&w)],
        profile("[tools]\nexclude = [\"t.write\"]\n"),
        cfg(Policy::full()),
    );
    let run = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    let names: Vec<String> = fake
        .request(0)
        .tools
        .iter()
        .map(|d| d["function"]["name"].as_str().unwrap().to_owned())
        .collect();
    assert!(names.contains(&"t__read".to_owned()) && !names.contains(&"t__write".to_owned()));
    assert_eq!(
        w.count.load(Ordering::SeqCst),
        0,
        "excluded tool cannot be called even by name"
    );
    assert!(tool_messages(&fake.request(1))[0].contains("unknown tool"));
}

#[tokio::test]
async fn this_chat_grant_skips_later_asks() {
    let env = setup();
    let w = Arc::new(tool("t.write", Effect::Write, "w"));
    let fake = Fake::new(vec![
        calls(&[("c1", "t.write", json!({"n": 1}))]),
        calls(&[("c2", "t.write", json!({"n": 2}))]),
        text("done"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&w)],
        profile(""),
        cfg(Policy::approve_destructive()),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let Event::ApprovalNeeded(a) = until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
    else {
        unreachable!()
    };
    lp.approve(a.call_id, ApprovalScope::ThisChat, env.user)
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(w.count.load(Ordering::SeqCst), 2);
    assert_eq!(
        env.storage.list_approvals(run).unwrap().len(),
        1,
        "second call needed no approval"
    );
}

#[tokio::test]
async fn always_grant_survives_restart_via_audit_log() {
    let env = setup();
    let w = Arc::new(tool("t.write", Effect::Write, "w"));
    let fake = Fake::new(vec![calls(&[("c1", "t.write", json!({}))]), text("done")]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&w)],
        profile(""),
        cfg(Policy::approve_destructive()),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let Event::ApprovalNeeded(a) = until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
    else {
        unreachable!()
    };
    lp.approve(a.call_id, ApprovalScope::Always, env.user)
        .unwrap();
    lp.wait(run).await.unwrap();

    // "Restart": a fresh loop over the same storage.
    let w2 = Arc::new(tool("t.write", Effect::Write, "w"));
    let fake2 = Fake::new(vec![
        calls(&[("c1", "t.write", json!({}))]),
        text("done again"),
    ]);
    let lp2 = make_loop(
        &env.storage,
        env.bot,
        &fake2,
        &[t(&w2)],
        profile(""),
        cfg(Policy::approve_destructive()),
    );
    let run2 = lp2.run_turn(env.chat, "again").unwrap();
    assert_eq!(lp2.wait(run2).await.unwrap(), RunState::Succeeded);
    assert_eq!(w2.count.load(Ordering::SeqCst), 1);
    assert_eq!(env.storage.list_approvals(run2).unwrap().len(), 0);
}

#[tokio::test]
async fn steer_is_injected_before_next_model_call() {
    let env = setup();
    let mut slow = tool("t.slow", Effect::Read, "slow-result");
    slow.delay = Duration::from_millis(300);
    let slow = Arc::new(slow);
    let fake = Fake::new(vec![
        calls(&[("c1", "t.slow", json!({}))]),
        text("adjusted"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&slow)],
        profile(""),
        cfg(Policy::full()),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "start").unwrap();
    until(&mut rx, |e| matches!(e, Event::ToolCallRequested(_))).await;
    let d = lp
        .send_user_message(env.chat, "actually, do X", MessageMode::Steer)
        .unwrap();
    assert_eq!(d, Delivery::Steered(run));
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);

    let req = fake.request(1);
    let roles: Vec<&MessageRole> = req.messages.iter().map(|m| &m.role).collect();
    // system, user, assistant(tool call), tool, user(steer)
    assert_eq!(
        roles,
        [
            &MessageRole::System,
            &MessageRole::User,
            &MessageRole::Assistant,
            &MessageRole::Tool,
            &MessageRole::User
        ]
    );
    assert_eq!(req.messages[4].content, json!("actually, do X"));
}

#[tokio::test]
async fn queue_runs_as_next_turn_with_history() {
    let env = setup();
    let mut slow = tool("t.slow", Effect::Read, "r");
    slow.delay = Duration::from_millis(200);
    let slow = Arc::new(slow);
    let fake = Fake::new(vec![
        calls(&[("c1", "t.slow", json!({}))]),
        text("first answer"),
        text("second answer"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&slow)],
        profile(""),
        cfg(Policy::full()),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "one").unwrap();
    until(&mut rx, |e| matches!(e, Event::ToolCallRequested(_))).await;
    assert_eq!(
        lp.send_user_message(env.chat, "two", MessageMode::Queue)
            .unwrap(),
        Delivery::Queued
    );
    assert!(lp.run_turn(env.chat, "three").is_err(), "chat is busy");
    lp.wait(run).await.unwrap();
    // Second turn starts by itself.
    let mut finished = 0;
    until(&mut rx, |e| {
        if matches!(e, Event::RunFinished(_)) {
            finished += 1;
        }
        finished == 2
    })
    .await;

    assert_eq!(fake.n_requests(), 3);
    let last = fake.request(2);
    let contents: Vec<String> = last
        .messages
        .iter()
        .filter(|m| matches!(m.role, MessageRole::User | MessageRole::Assistant))
        .map(|m| m.content.as_str().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(contents, ["one", "first answer", "two"]);
}

#[tokio::test]
async fn provider_failure_is_retried_for_model_steps() {
    // First stream() errors (script exhausted -> InvalidRequest), retry sees the reply.
    let env = setup();
    let fake = Fake::new(vec![]);
    let mut config = cfg(Policy::full());
    config.model_retry.max_attempts = 3;
    let lp = make_loop(&env.storage, env.bot, &fake, &[], profile(""), config);
    let run = lp.run_turn(env.chat, "hi").unwrap();
    // Feed the reply after the first failed attempt.
    tokio::time::sleep(Duration::from_millis(5)).await;
    fake.replies.lock().push_back(text("recovered"));
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert!(fake.n_requests() >= 2);
}

// ---------- crash / recover ----------

#[test]
fn crash_mid_tool_then_recover_never_replays_the_tool() {
    let env = setup();
    let path = env.dir.path().to_path_buf();
    let (user, bot, chat) = (env.user, env.bot, env.chat);

    // Process 1: the write tool hangs; the whole runtime is dropped ("crash").
    let rt1 = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let run = rt1.block_on(async {
        let mut hang = tool("t.write", Effect::Write, "never");
        hang.delay = Duration::from_secs(3600);
        let hang = Arc::new(hang);
        let fake = Fake::new(vec![calls(&[("c1", "t.write", json!({}))]), Reply::Hang]);
        let lp = make_loop(
            &env.storage,
            bot,
            &fake,
            &[t(&hang)],
            profile(""),
            cfg(Policy::full()),
        );
        let run = lp.run_turn(chat, "write it").unwrap();
        for _ in 0..500 {
            if hang.count.load(Ordering::SeqCst) == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(hang.count.load(Ordering::SeqCst), 1, "tool started");
        run
    });
    drop(rt1);
    drop(env);

    // Process 2: reopen the same data dir.
    let storage = Arc::new(Storage::open(&path).unwrap());
    let rt2 = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    rt2.block_on(async {
        let w = Arc::new(tool("t.write", Effect::Write, "written"));
        let fake = Fake::new(vec![text("I could not confirm the write")]);
        let lp = make_loop(
            &storage,
            bot,
            &fake,
            &[t(&w)],
            profile(""),
            cfg(Policy::full()),
        );
        assert_eq!(lp.recover().unwrap(), vec![run]);
        assert_eq!(lp.runtime().state(run).unwrap(), RunState::Paused);
        let steps = storage.list_steps(run).unwrap();
        assert_eq!(steps.last().unwrap().status, "interrupted");
        assert_eq!(steps.last().unwrap().name, "tools");

        lp.resume(run).unwrap();
        assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
        assert_eq!(
            w.count.load(Ordering::SeqCst),
            0,
            "side-effecting step must not be replayed"
        );
        let msgs = tool_messages(&fake.request(0));
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].contains("interrupted"), "{msgs:?}");
        let chat_msgs = storage.list_messages(chat).unwrap();
        assert_eq!(
            chat_msgs.last().unwrap().content,
            "I could not confirm the write"
        );
        let _ = user;
    });
}

#[test]
fn crash_mid_model_call_replays_the_model_step() {
    let env = setup();
    let path = env.dir.path().to_path_buf();
    let bot = env.bot;
    let chat = env.chat;
    let rt1 = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let run = rt1.block_on(async {
        let fake = Fake::new(vec![Reply::Hang]);
        let lp = make_loop(
            &env.storage,
            bot,
            &fake,
            &[],
            profile(""),
            cfg(Policy::full()),
        );
        let run = lp.run_turn(chat, "hello").unwrap();
        for _ in 0..500 {
            if fake.n_requests() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        run
    });
    drop(rt1);
    drop(env);

    let storage = Arc::new(Storage::open(&path).unwrap());
    let rt2 = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    rt2.block_on(async {
        let fake = Fake::new(vec![text("hello again")]);
        let lp = make_loop(&storage, bot, &fake, &[], profile(""), cfg(Policy::full()));
        lp.recover().unwrap();
        lp.resume(run).unwrap();
        assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
        assert_eq!(
            fake.n_requests(),
            1,
            "model step is idempotent and replayed once"
        );
        assert_eq!(
            storage.list_messages(chat).unwrap().last().unwrap().content,
            "hello again"
        );
    });
}

#[test]
fn crash_while_waiting_for_approval_recovers_to_paused() {
    let env = setup();
    let path = env.dir.path().to_path_buf();
    let (bot, chat) = (env.bot, env.chat);
    let rt1 = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let run = rt1.block_on(async {
        let w = Arc::new(tool("t.write", Effect::Write, "w"));
        let fake = Fake::new(vec![calls(&[("c1", "t.write", json!({}))]), Reply::Hang]);
        let lp = make_loop(
            &env.storage,
            bot,
            &fake,
            &[t(&w)],
            profile(""),
            cfg(Policy::approve_destructive()),
        );
        let mut rx = lp.subscribe();
        let run = lp.run_turn(chat, "go").unwrap();
        until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await;
        run
    });
    drop(rt1);
    drop(env);

    let storage = Arc::new(Storage::open(&path).unwrap());
    let rt2 = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    rt2.block_on(async {
        let w = Arc::new(tool("t.write", Effect::Write, "w"));
        let fake = Fake::new(vec![text("understood")]);
        let lp = make_loop(
            &storage,
            bot,
            &fake,
            &[t(&w)],
            profile(""),
            cfg(Policy::approve_destructive()),
        );
        assert_eq!(lp.recover().unwrap(), vec![run]);
        assert_eq!(lp.runtime().state(run).unwrap(), RunState::Paused);
        assert_eq!(
            storage.list_approvals(run).unwrap()[0].decision.as_deref(),
            Some("expired")
        );
        lp.resume(run).unwrap();
        assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
        assert_eq!(w.count.load(Ordering::SeqCst), 0);
    });
}
