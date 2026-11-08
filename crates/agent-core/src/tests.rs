//! Deterministic loop tests driven by a scripted fake `ChatProvider`.

#![allow(clippy::assert_is_empty)]

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
    AgentLoop, ApprovalScope, BotProfile, Delivery, GrantTarget, LoopConfig, MessageMode,
    Providers, StreamItem,
};

// ---------- fakes ----------

enum Reply {
    Chunks(Vec<ChatChunk>),
    Hang,
    /// A provider error: `upstream` (retryable), `auth`, `api400`, `invalid`
    /// (hard stop), `unsupported` (not retried, but the chain continues),
    /// or `malformed` (retried once).
    Fail(&'static str),
}

fn text(s: &str) -> Reply {
    Reply::Chunks(vec![
        ChatChunk::TextDelta(s.to_owned()),
        ChatChunk::Usage(Usage {
            prompt_tokens: 10,
            completion_tokens: 5,
            cached_tokens: Some(2),
            cache_write_tokens: None,
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
                provider_extra: None,
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
            Some(Reply::Fail(kind)) => Err(match kind {
                "upstream" => ProviderError::Upstream {
                    status: 503,
                    message: "down".into(),
                },
                "auth" => ProviderError::Auth("bad key".into()),
                "malformed" => ProviderError::MalformedToolCall("finish reason x".into()),
                "unsupported" => ProviderError::Unsupported {
                    capability: "tools".into(),
                },
                "invalid" => ProviderError::InvalidRequest("bad request".into()),
                _ => ProviderError::Api {
                    status: 400,
                    message: "bad".into(),
                },
            }),
            None => Err(ProviderError::InvalidRequest("script exhausted".into())),
        }
    }
}

pub(crate) struct TestTool {
    pub(crate) spec: ToolSpec,
    pub(crate) count: Arc<AtomicUsize>,
    pub(crate) delay: Duration,
    pub(crate) barrier: Option<Arc<Barrier>>,
    pub(crate) output: String,
    /// When set, behaves like `builtin_tools::finish_text`: writes `output` to
    /// this file and returns `max_inline_bytes - 100` of it plus a `SpillRef`.
    pub(crate) self_spill: Option<PathBuf>,
}

pub(crate) fn tool(name: &str, effect: Effect, output: &str) -> TestTool {
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
        self_spill: None,
    }
}

#[async_trait]
impl Tool for TestTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }
    async fn call(&self, ctx: &ToolContext, _: Value) -> Result<ToolOutput, ToolError> {
        self.count.fetch_add(1, Ordering::SeqCst);
        if let Some(b) = &self.barrier {
            b.wait().await;
        }
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        if let Some(path) = &self.self_spill {
            std::fs::write(path, &self.output).unwrap();
            return Ok(ToolOutput {
                content: vec![tool_registry::ContentPart::Text {
                    text: self.output[..ctx.max_inline_bytes - 100].to_owned(),
                }],
                spill: Some(tool_registry::SpillRef {
                    path: path.display().to_string(),
                    mime_type: "text/plain".into(),
                    size_bytes: self.output.len() as u64,
                }),
            });
        }
        Ok(ToolOutput::text(self.output.clone()))
    }
}

// ---------- harness ----------

pub(crate) struct Env {
    pub(crate) dir: tempfile::TempDir,
    pub(crate) storage: Arc<Storage>,
    pub(crate) user: UserId,
    pub(crate) bot: BotId,
    pub(crate) chat: ChatId,
}

impl Env {
    /// Simulates a process crash: releases this process's storage handle but
    /// keeps the data directory alive. The caller must hold the returned
    /// `TempDir` until the test ends, or the directory is deleted before the
    /// "restarted" process reopens it (Linux does not block on open files).
    pub(crate) fn crash(self) -> tempfile::TempDir {
        let Env { dir, storage, .. } = self;
        drop(storage);
        dir
    }
}

pub(crate) fn setup() -> Env {
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

pub(crate) fn profile(extra: &str) -> BotProfile {
    BotProfile::from_toml_str(&format!(
        "[bot]\nid = \"t\"\nname = \"T\"\npersona = \"You are T.\"\n[models]\nexecutor = \"fake/m\"\n{extra}"
    ))
    .unwrap()
}

pub(crate) fn cfg(policy: Policy) -> LoopConfig {
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

pub(crate) async fn until(
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

/// Register `ask_user` / `todo_write` (the loop treats them by category).
fn register_meta(lp: &AgentLoop) {
    for tool in builtin_tools::meta::tools() {
        lp.sh.tools.register(tool);
    }
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
        text("summary so far"),
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
    // The guard no longer fails the turn: one closing call without tools.
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert!(lp.stopped_by_guard(run).unwrap());
    assert_eq!(
        r.count.load(Ordering::SeqCst),
        4,
        "5th identical call is never executed"
    );
    assert_eq!(fake.n_requests(), 6);
    let last = fake.request(5);
    assert!(
        last.tools.is_empty(),
        "tools are disabled in the closing call"
    );
    let note = last.messages.last().unwrap();
    assert_eq!(note.role, MessageRole::System);
    assert!(
        note.content.as_str().unwrap().contains("repeat detected"),
        "{:?}",
        note.content
    );
    // The unexecuted 5th call is answered so the provider accepts the history.
    assert!(
        tool_messages(&last)
            .last()
            .unwrap()
            .contains("not executed")
    );
    assert_eq!(
        env.storage
            .list_messages(env.chat)
            .unwrap()
            .last()
            .unwrap()
            .content,
        "summary so far"
    );
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
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert!(lp.stopped_by_guard(run).unwrap());
    assert_eq!(fake.n_requests(), 3, "two steps + the closing call");
    assert!(fake.request(2).tools.is_empty());
    assert!(
        fake.request(2)
            .messages
            .last()
            .unwrap()
            .content
            .as_str()
            .unwrap()
            .contains("step limit")
    );
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
                    cache_write_tokens: None,
                }),
            );
            Reply::Chunks(c)
        }
        Reply::Hang | Reply::Fail(_) => unreachable!(),
    };
    let fake = Fake::new(vec![with_usage("1", 1), text("never")]);
    let mut config = cfg(Policy::full());
    config.guards.max_tokens_per_turn = 50;
    let lp = make_loop(&env.storage, env.bot, &fake, &[t(&r)], profile(""), config);
    let run = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert!(lp.stopped_by_guard(run).unwrap());
    assert_eq!(fake.n_requests(), 2);
    assert!(
        fake.request(1)
            .messages
            .last()
            .unwrap()
            .content
            .as_str()
            .unwrap()
            .contains("token budget")
    );
}

#[tokio::test]
async fn failing_closing_call_still_fails_the_run() {
    let env = setup();
    let r = Arc::new(tool("t.read", Effect::Read, "x"));
    let fake = Fake::new(vec![
        calls(&[("1", "t.read", json!({"n": 1}))]),
        Reply::Fail("api400"),
    ]);
    let mut config = cfg(Policy::full());
    config.guards.max_steps = 1;
    let lp = make_loop(&env.storage, env.bot, &fake, &[t(&r)], profile(""), config);
    let run = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Failed);
    assert!(!lp.stopped_by_guard(run).unwrap());
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
async fn self_spilled_output_is_not_spilled_again() {
    let env = setup();
    let own_dir = tempfile::tempdir().unwrap();
    let mut tl = tool("t.selfspill", Effect::Read, &"y".repeat(30 * 1024));
    tl.self_spill = Some(own_dir.path().join("own-spill.txt"));
    let r = Arc::new(tl);
    let fake = Fake::new(vec![calls(&[("c1", "t.selfspill", json!({}))]), text("ok")]);
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
    let threshold = cfg(Policy::full()).guards.spill_threshold_bytes;
    assert!(
        msg.len() > threshold - 1024 && msg.len() < threshold + 1024,
        "model sees about the threshold inline ({} bytes)",
        msg.len()
    );
    assert_eq!(msg.matches("own-spill.txt").count(), 1);
    assert!(!msg.contains("output truncated"));
    // agent-core wrote no second spill file.
    assert!(!env.dir.path().join("spill").exists());
}

#[tokio::test]
async fn todo_tool_emits_event_without_permission() {
    let env = setup();
    let fake = Fake::new(vec![
        calls(&[(
            "c1",
            "todo_write",
            json!({"items": [
                {"content": "a", "status": "completed"},
                {"content": "b", "status": "in_progress"}
            ]}),
        )]),
        text("planned"),
    ]);
    // Even a read-only policy allows the Meta category.
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[],
        profile(""),
        cfg(Policy::read_only()),
    );
    register_meta(&lp);
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "plan it").unwrap();
    let Event::TodoUpdated(u) = until(&mut rx, |e| matches!(e, Event::TodoUpdated(_))).await else {
        unreachable!()
    };
    assert_eq!(u.items.len(), 2);
    assert_eq!(u.items[0].status, TodoStatus::Done);
    assert_eq!(u.items[1].text, "b");
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert!(tool_messages(&fake.request(1))[0].contains("Plan updated"));
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
async fn transient_provider_failure_is_retried() {
    let env = setup();
    let fake = Fake::new(vec![Reply::Fail("upstream"), text("recovered")]);
    let mut config = cfg(Policy::full());
    config.model_retry.max_attempts = 3;
    let lp = make_loop(&env.storage, env.bot, &fake, &[], profile(""), config);
    let run = lp.run_turn(env.chat, "hi").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(fake.n_requests(), 2);
}

#[tokio::test]
async fn non_retryable_errors_are_not_retried() {
    for kind in ["auth", "api400"] {
        let env = setup();
        let fake = Fake::new(vec![Reply::Fail(kind), text("never")]);
        let mut config = cfg(Policy::full());
        config.model_retry.max_attempts = 3;
        let lp = make_loop(
            &env.storage,
            env.bot,
            &fake,
            &[],
            profile("fallback = [\"fake/b\"]\n"),
            config,
        );
        let run = lp.run_turn(env.chat, "hi").unwrap();
        assert_eq!(lp.wait(run).await.unwrap(), RunState::Failed, "{kind}");
        assert_eq!(fake.n_requests(), 1, "{kind}: no retry and no fallback");
    }
}

#[tokio::test]
async fn model_call_timeout_covers_a_hanging_provider() {
    let env = setup();
    let fake = Fake::new(vec![Reply::Hang, text("never")]);
    let mut config = cfg(Policy::full());
    config.guards.model_timeout = Duration::from_millis(150);
    config.model_retry.max_attempts = 3;
    let lp = make_loop(&env.storage, env.bot, &fake, &[], profile(""), config);
    let run = lp.run_turn(env.chat, "hi").unwrap();
    let state = tokio::time::timeout(Duration::from_secs(10), lp.wait(run))
        .await
        .expect("the model-call timeout must end the turn")
        .unwrap();
    assert_eq!(state, RunState::Failed);
    assert_eq!(fake.n_requests(), 1);
}

#[tokio::test]
async fn malformed_tool_call_is_retried_exactly_once() {
    // Once: the second attempt succeeds.
    let env = setup();
    let fake = Fake::new(vec![Reply::Fail("malformed"), text("fine now")]);
    let mut config = cfg(Policy::full());
    config.model_retry.max_attempts = 5;
    let lp = make_loop(&env.storage, env.bot, &fake, &[], profile(""), config);
    let run = lp.run_turn(env.chat, "hi").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(fake.n_requests(), 2);

    // Not twice, however many attempts the policy allows.
    let env = setup();
    let fake = Fake::new(vec![
        Reply::Fail("malformed"),
        Reply::Fail("malformed"),
        text("never"),
    ]);
    let mut config = cfg(Policy::full());
    config.model_retry.max_attempts = 5;
    let lp = make_loop(&env.storage, env.bot, &fake, &[], profile(""), config);
    let run = lp.run_turn(env.chat, "hi").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Failed);
    assert_eq!(fake.n_requests(), 2);
}

// ---------- schedule-started runs ----------

fn run_started(rx: &mut broadcast::Receiver<StreamItem>) -> Vec<protocol::event::RunStarted> {
    let mut v = Vec::new();
    while let Ok(i) = rx.try_recv() {
        if let Event::RunStarted(r) = i.envelope.event {
            v.push(r);
        }
    }
    v
}

fn tool_work(name: &str) -> crate::ScheduleWork {
    crate::ScheduleWork::Tool {
        name: name.into(),
        input: json!({"a": 1}),
    }
}

#[tokio::test]
async fn scheduled_tool_run_calls_the_tool_without_a_model_and_carries_the_schedule_id() {
    let env = setup();
    let w = Arc::new(tool("t.write", Effect::Write, "written"));
    let fake = Fake::new(vec![]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&w)],
        profile(""),
        cfg(Policy::full()),
    );
    let mut rx = lp.subscribe();
    let sid = protocol::TaskId::new();
    let run = lp
        .run_scheduled(env.chat, sid, tool_work("t.write"))
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(w.count.load(Ordering::SeqCst), 1);
    assert_eq!(fake.n_requests(), 0, "no model call");
    let started = run_started(&mut rx);
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].schedule_id, Some(sid));
    let r = env.storage.get_run(run).unwrap().unwrap();
    assert_eq!(crate::shared::schedule_id_of(&r.kind), Some(sid));
    assert_eq!(
        crate::shared::initiator_of(&r.kind),
        protocol::InitiatorKind::Schedule
    );
    let names: Vec<_> = env
        .storage
        .list_steps(run)
        .unwrap()
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(names, ["user_message", "tools"]);
}

#[tokio::test]
async fn human_runs_have_no_schedule_id() {
    let env = setup();
    let fake = Fake::new(vec![text("hi")]);
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
    lp.wait(run).await.unwrap();
    assert_eq!(run_started(&mut rx)[0].schedule_id, None);
}

#[tokio::test]
async fn scheduled_prompt_run_is_a_normal_turn_with_the_schedule_initiator() {
    let env = setup();
    let fake = Fake::new(vec![text("report ready")]);
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
            crate::ScheduleWork::Prompt("daily report".into()),
        )
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(fake.n_requests(), 1);
    let msgs = env.storage.list_messages(env.chat).unwrap();
    assert_eq!(msgs[0].content, "daily report");
    assert_eq!(msgs[1].content, "report ready");
}

#[tokio::test]
async fn schedule_runs_never_skip_the_money_floor_even_with_a_full_policy_and_grant() {
    let env = setup();
    let mut pay = tool("t.pay", Effect::Write, "paid");
    pay.spec.category = PermissionCategory::Money;
    let pay = Arc::new(pay);
    let fake = Fake::new(vec![]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&pay)],
        profile(""),
        cfg(Policy::full()),
    );
    let mut rx = lp.subscribe();
    let run = lp
        .run_scheduled(env.chat, protocol::TaskId::new(), tool_work("t.pay"))
        .unwrap();
    let Event::ApprovalNeeded(a) = until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
    else {
        unreachable!()
    };
    assert_eq!(pay.count.load(Ordering::SeqCst), 0);
    lp.approve(a.call_id, ApprovalScope::Always, env.user)
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(pay.count.load(Ordering::SeqCst), 1);
    // The "always" grant does not waive the floor on the next firing.
    let run2 = lp
        .run_scheduled(env.chat, protocol::TaskId::new(), tool_work("t.pay"))
        .unwrap();
    until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await;
    assert_eq!(pay.count.load(Ordering::SeqCst), 1);
    lp.cancel(run2).unwrap();
}

#[tokio::test]
async fn scheduled_run_pauses_when_nobody_answers_the_approval() {
    let env = setup();
    let w = Arc::new(tool("t.write", Effect::Write, "written"));
    let fake = Fake::new(vec![calls(&[("c1", "t.write", json!({}))]), text("never")]);
    let mut config = cfg(Policy::approve_destructive());
    config.schedule_approval_timeout = Duration::from_millis(150);
    let lp = make_loop(&env.storage, env.bot, &fake, &[t(&w)], profile(""), config);
    let mut rx = lp.subscribe();
    let run = lp
        .run_scheduled(
            env.chat,
            protocol::TaskId::new(),
            crate::ScheduleWork::Prompt("go".into()),
        )
        .unwrap();
    let state = tokio::time::timeout(Duration::from_secs(10), lp.wait(run))
        .await
        .expect("the approval timeout must end the wait")
        .unwrap();
    assert_eq!(state, RunState::Paused);
    assert_eq!(w.count.load(Ordering::SeqCst), 0, "never ran unapproved");
    let approvals = env.storage.list_approvals(run).unwrap();
    assert_eq!(approvals[0].decision.as_deref(), Some("expired"));
    let mut saw_error = false;
    while let Ok(i) = rx.try_recv() {
        if let Event::Error(e) = i.envelope.event {
            saw_error |= e.recoverable && e.message.contains("nobody answered");
        }
    }
    assert!(saw_error, "a recoverable 'paused' error event");
    assert!(lp.sh.active.lock().get(&env.chat).is_none());
}

#[tokio::test]
async fn human_runs_wait_for_approvals_beyond_the_schedule_timeout() {
    let env = setup();
    let w = Arc::new(tool("t.write", Effect::Write, "written"));
    let fake = Fake::new(vec![calls(&[("c1", "t.write", json!({}))]), text("done")]);
    let mut config = cfg(Policy::approve_destructive());
    config.schedule_approval_timeout = Duration::from_millis(50);
    let lp = make_loop(&env.storage, env.bot, &fake, &[t(&w)], profile(""), config);
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let Event::ApprovalNeeded(a) = until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
    else {
        unreachable!()
    };
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(lp.runtime().state(run).unwrap(), RunState::WaitingApproval);
    lp.approve(a.call_id, ApprovalScope::Once, env.user)
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
}

#[test]
fn retryable_classification() {
    use crate::exec::is_retryable;
    assert!(is_retryable(&ProviderError::Timeout));
    assert!(is_retryable(&ProviderError::RateLimited {
        retry_after: None
    }));
    assert!(is_retryable(&ProviderError::Connection("x".into())));
    assert!(is_retryable(&ProviderError::MalformedToolCall("x".into())));
    assert!(is_retryable(&ProviderError::Api {
        status: 429,
        message: String::new()
    }));
    assert!(!is_retryable(&ProviderError::Auth("x".into())));
    assert!(!is_retryable(&ProviderError::InvalidRequest("x".into())));
    assert!(!is_retryable(&ProviderError::Unsupported {
        capability: "tools".into(),
    }));
    assert!(!is_retryable(&ProviderError::Api {
        status: 404,
        message: String::new()
    }));
}

#[tokio::test]
async fn fallback_model_is_tried_after_retries_are_exhausted() {
    let env = setup();
    let fake = Fake::new(vec![
        Reply::Fail("upstream"),
        Reply::Fail("upstream"),
        text("from the fallback"),
    ]);
    let mut config = cfg(Policy::full());
    config.model_retry.max_attempts = 2;
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[],
        profile("fallback = [\"fake/b\"]\n"),
        config,
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "hi").unwrap();
    let Event::ModelFallback(f) = until(&mut rx, |e| matches!(e, Event::ModelFallback(_))).await
    else {
        unreachable!()
    };
    assert_eq!((f.from.as_str(), f.to.as_str()), ("fake/m", "fake/b"));
    assert!(f.reason.contains("down"), "{}", f.reason);
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(fake.n_requests(), 3);
    assert_eq!(fake.request(0).model.0, "fake/m");
    assert_eq!(fake.request(1).model.0, "fake/m");
    assert_eq!(fake.request(2).model.0, "fake/b");
    assert_eq!(
        env.storage
            .list_messages(env.chat)
            .unwrap()
            .last()
            .unwrap()
            .content,
        "from the fallback"
    );
}

#[tokio::test]
async fn unsupported_on_the_primary_falls_back_and_records_the_reason() {
    let env = setup();
    let fake = Fake::new(vec![Reply::Fail("unsupported"), text("from the fallback")]);
    let mut config = cfg(Policy::full());
    config.model_retry.max_attempts = 3;
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[],
        profile("fallback = [\"fake/b\"]\n"),
        config,
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "hi").unwrap();
    let Event::ModelFallback(f) = until(&mut rx, |e| matches!(e, Event::ModelFallback(_))).await
    else {
        unreachable!()
    };
    assert_eq!((f.from.as_str(), f.to.as_str()), ("fake/m", "fake/b"));
    assert!(f.reason.contains("tools"), "{}", f.reason);
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(fake.n_requests(), 2, "the same model is not retried");
    assert_eq!(fake.request(0).model.0, "fake/m");
    assert_eq!(fake.request(1).model.0, "fake/b");
    assert_eq!(
        env.storage
            .list_messages(env.chat)
            .unwrap()
            .last()
            .unwrap()
            .content,
        "from the fallback"
    );
}

#[tokio::test]
async fn invalid_request_on_the_primary_stops_the_chain() {
    let env = setup();
    let fake = Fake::new(vec![Reply::Fail("invalid"), text("never")]);
    let mut config = cfg(Policy::full());
    config.model_retry.max_attempts = 3;
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[],
        profile("fallback = [\"fake/b\"]\n"),
        config,
    );
    let run = lp.run_turn(env.chat, "hi").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Failed);
    assert_eq!(fake.n_requests(), 1, "no retry and no fallback");
}

#[tokio::test]
async fn every_model_unsupported_names_the_gap_and_model_caps() {
    let env = setup();
    let fake = Fake::new(vec![Reply::Fail("unsupported"), Reply::Fail("unsupported")]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[],
        profile("fallback = [\"fake/b\"]\n"),
        cfg(Policy::full()),
    );
    let run = lp.run_turn(env.chat, "hi").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Failed);
    assert_eq!(fake.n_requests(), 2);
    let err = env.storage.get_run(run).unwrap().unwrap().error.unwrap();
    assert!(err.contains("fake/m"), "{err}");
    assert!(err.contains("fake/b"), "{err}");
    assert!(err.contains("tools"), "{err}");
    assert!(err.contains("model_caps"), "{err}");
}

#[tokio::test]
async fn all_models_failing_fails_the_run() {
    let env = setup();
    let fake = Fake::new(vec![Reply::Fail("upstream"), Reply::Fail("upstream")]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[],
        profile("fallback = [\"fake/b\"]\n"),
        cfg(Policy::full()),
    );
    let run = lp.run_turn(env.chat, "hi").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Failed);
    assert_eq!(fake.n_requests(), 2);
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
    let _data_dir = env.crash();

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
    let _data_dir = env.crash();

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
    let _data_dir = env.crash();

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

// ---------- question cards ----------

#[tokio::test]
async fn ask_user_waits_for_the_answer_and_feeds_it_back() {
    let env = setup();
    let fake = Fake::new(vec![
        calls(&[(
            "c1",
            "ask_user",
            json!({"question": "Which file?", "options": ["a.txt", "b.txt"]}),
        )]),
        text("using b"),
    ]);
    // Meta is allowed even by a read-only policy: no approval prompt.
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[],
        profile(""),
        cfg(Policy::read_only()),
    );
    register_meta(&lp);
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let Event::QuestionAsked(q) = until(&mut rx, |e| matches!(e, Event::QuestionAsked(_))).await
    else {
        unreachable!()
    };
    assert_eq!(q.question, "Which file?");
    assert_eq!(q.options, ["a.txt", "b.txt"]);
    assert_eq!(lp.runtime().state(run).unwrap(), RunState::WaitingApproval);
    let pending = env.storage.list_approvals(run).unwrap();
    assert_eq!(pending.len(), 1);
    assert!(pending[0].summary.starts_with("question:"));
    assert!(pending[0].decision.is_none());

    lp.answer_question(q.call_id, "b.txt").unwrap();
    assert!(lp.answer_question(q.call_id, "again").is_err());
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(
        tool_messages(&fake.request(1)),
        vec!["The user answered: b.txt".to_owned()]
    );
    assert_eq!(
        env.storage.list_approvals(run).unwrap()[0]
            .decision
            .as_deref(),
        Some("answered")
    );
}

#[tokio::test]
async fn declining_a_question_tells_the_model() {
    let env = setup();
    let fake = Fake::new(vec![
        calls(&[("c1", "ask_user", json!({"question": "Sure?"}))]),
        text("ok"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[],
        profile(""),
        cfg(Policy::full()),
    );
    register_meta(&lp);
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let Event::QuestionAsked(q) = until(&mut rx, |e| matches!(e, Event::QuestionAsked(_))).await
    else {
        unreachable!()
    };
    lp.deny(q.call_id, env.user, None).unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert!(tool_messages(&fake.request(1))[0].contains("declined"));
}

#[test]
fn crash_while_waiting_for_an_answer_recovers_to_paused() {
    let env = setup();
    let path = env.dir.path().to_path_buf();
    let (bot, chat) = (env.bot, env.chat);
    let rt1 = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let run = rt1.block_on(async {
        let fake = Fake::new(vec![
            calls(&[("c1", "ask_user", json!({"question": "Which?"}))]),
            Reply::Hang,
        ]);
        let lp = make_loop(
            &env.storage,
            bot,
            &fake,
            &[],
            profile(""),
            cfg(Policy::full()),
        );
        register_meta(&lp);
        let mut rx = lp.subscribe();
        let run = lp.run_turn(chat, "go").unwrap();
        until(&mut rx, |e| matches!(e, Event::QuestionAsked(_))).await;
        run
    });
    drop(rt1);
    let _data_dir = env.crash();

    let storage = Arc::new(Storage::open(&path).unwrap());
    let rt2 = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    rt2.block_on(async {
        let fake = Fake::new(vec![text("thanks")]);
        let lp = make_loop(&storage, bot, &fake, &[], profile(""), cfg(Policy::full()));
        register_meta(&lp);
        assert_eq!(lp.recover().unwrap(), vec![run]);
        assert_eq!(lp.runtime().state(run).unwrap(), RunState::Paused);
        assert_eq!(
            storage.list_approvals(run).unwrap()[0].decision.as_deref(),
            Some("expired")
        );
        // The question step is a read (replayable): resuming asks it again.
        let mut rx = lp.subscribe();
        lp.resume(run).unwrap();
        let Event::QuestionAsked(q) =
            until(&mut rx, |e| matches!(e, Event::QuestionAsked(_))).await
        else {
            unreachable!()
        };
        lp.answer_question(q.call_id, "the first one").unwrap();
        assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
        assert_eq!(
            tool_messages(&fake.request(0)),
            vec!["The user answered: the first one".to_owned()]
        );
    });
}

// ---------- grants ----------

async fn approve_always(
    env: &Env,
    lp: &AgentLoop,
    rx: &mut broadcast::Receiver<StreamItem>,
    run: protocol::RunId,
    scope: ApprovalScope,
) {
    let Event::ApprovalNeeded(a) = until(rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
    else {
        unreachable!()
    };
    lp.approve(a.call_id, scope, env.user).unwrap();
    lp.wait(run).await.unwrap();
}

#[tokio::test]
async fn always_grants_are_listed_and_revocable() {
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
    assert!(lp.list_grants(env.bot).is_empty());
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    approve_always(&env, &lp, &mut rx, run, ApprovalScope::Always).await;

    let grants = lp.list_grants(env.bot);
    assert_eq!(grants.len(), 1);
    assert_eq!(grants[0].target, GrantTarget::Tool("t.write".into()));
    assert!(
        env.storage
            .list_audit(env.user)
            .unwrap()
            .iter()
            .any(|a| a.action == "grant.always"),
        "audit entry kept"
    );

    // Revoke: the next call asks again; the revocation survives a restart.
    assert!(
        lp.revoke_grant(env.bot, &GrantTarget::Tool("t.write".into()))
            .unwrap()
    );
    assert!(
        !lp.revoke_grant(env.bot, &GrantTarget::Tool("t.write".into()))
            .unwrap()
    );
    assert!(lp.list_grants(env.bot).is_empty());

    let fake2 = Fake::new(vec![calls(&[("c1", "t.write", json!({}))]), text("done")]);
    let lp2 = make_loop(
        &env.storage,
        env.bot,
        &fake2,
        &[t(&w)],
        profile(""),
        cfg(Policy::approve_destructive()),
    );
    assert!(lp2.list_grants(env.bot).is_empty(), "revocation is durable");
    let mut rx2 = lp2.subscribe();
    let run2 = lp2.run_turn(env.chat, "again").unwrap();
    until(&mut rx2, |e| matches!(e, Event::ApprovalNeeded(_))).await;
    lp2.cancel(run2).unwrap();
}

#[tokio::test]
async fn category_grant_covers_other_tools_but_not_the_money_floor() {
    let env = setup();
    let w1 = Arc::new(tool("t.one", Effect::Write, "1"));
    let w2 = Arc::new(tool("t.two", Effect::Write, "2"));
    let mut pay = tool("t.pay", Effect::Write, "paid");
    pay.spec.category = PermissionCategory::Money;
    let pay = Arc::new(pay);
    let fake = Fake::new(vec![
        calls(&[("c1", "t.one", json!({}))]),
        text("one done"),
        calls(&[("c2", "t.two", json!({}))]),
        text("two done"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&w1), t(&w2), t(&pay)],
        profile(""),
        cfg(Policy::approve_destructive()),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "one").unwrap();
    approve_always(&env, &lp, &mut rx, run, ApprovalScope::AlwaysCategory).await;
    assert_eq!(
        lp.list_grants(env.bot)[0].target,
        GrantTarget::Category(PermissionCategory::Fs)
    );

    // Another tool of the same category needs no approval now.
    let run2 = lp.run_turn(env.chat, "two").unwrap();
    assert_eq!(lp.wait(run2).await.unwrap(), RunState::Succeeded);
    assert_eq!(w2.count.load(Ordering::SeqCst), 1);

    // A category grant on Money must never waive the floor.
    let fake3 = Fake::new(vec![calls(&[("c3", "t.pay", json!({}))]), text("x")]);
    let lp3 = make_loop(
        &env.storage,
        env.bot,
        &fake3,
        &[t(&pay)],
        profile(""),
        cfg(Policy::full()),
    );
    lp3.sh.grants.lock().always.push(crate::Grant {
        bot_id: env.bot,
        target: GrantTarget::Category(PermissionCategory::Money),
        granted_at_ms: 0,
    });
    let mut rx3 = lp3.subscribe();
    let run3 = lp3.run_turn(env.chat, "pay").unwrap();
    until(&mut rx3, |e| matches!(e, Event::ApprovalNeeded(_))).await;
    assert_eq!(pay.count.load(Ordering::SeqCst), 0);
    lp3.cancel(run3).unwrap();
}

// ---------- builtin tools wiring ----------

fn builtin_cfg(env: &Env, gate: Arc<dyn builtin_tools::ToolGate>) -> builtin_tools::BuiltinConfig {
    builtin_tools::BuiltinConfig::new(env.dir.path().join("bt"), gate)
}

struct NoGate;

#[async_trait]
impl builtin_tools::ToolGate for NoGate {
    async fn authorize(&self, _req: &builtin_tools::GateRequest) -> Result<(), String> {
        Err("the loop must replace this gate".into())
    }
}

#[tokio::test]
async fn script_inner_calls_ask_for_approval_interactively() {
    let env = setup();
    let file = env
        .dir
        .path()
        .join("out.txt")
        .display()
        .to_string()
        .replace('\\', "/");
    let code = format!("return await tools.call('fs_write', {{path: '{file}', content: 'hi'}});");
    let fake = Fake::new(vec![
        calls(&[("c1", "script_run", json!({ "code": code }))]),
        text("script done"),
    ]);
    // Scripts themselves are allowed; their inner writes ask.
    let mut policy = Policy::approve_destructive();
    policy.categories.insert(
        PermissionCategory::Script,
        permission_engine::Rule::new(permission_engine::Level::Allow),
    );
    let lp = make_loop(&env.storage, env.bot, &fake, &[], profile(""), cfg(policy))
        .with_builtin_tools(builtin_cfg(&env, Arc::new(NoGate)));
    assert!(lp.sh.tools.get("fs_write").is_some());
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "write it").unwrap();
    let Event::ApprovalNeeded(a) = until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
    else {
        unreachable!()
    };
    assert_eq!(a.tool_name, "fs_write");
    assert_eq!(lp.runtime().state(run).unwrap(), RunState::WaitingApproval);
    assert!(!std::path::Path::new(&file).exists());
    lp.approve(a.call_id, ApprovalScope::Once, env.user)
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "hi");
}

#[tokio::test]
async fn script_inner_call_denied_by_the_user_throws_in_the_script() {
    let env = setup();
    let file = env
        .dir
        .path()
        .join("never.txt")
        .display()
        .to_string()
        .replace('\\', "/");
    let code = format!(
        "try {{ await tools.call('fs_write', {{path: '{file}', content: 'x'}}); return 'wrote'; }} catch (e) {{ return 'blocked: ' + e; }}"
    );
    let fake = Fake::new(vec![
        calls(&[("c1", "script_run", json!({ "code": code }))]),
        text("ok"),
    ]);
    let mut policy = Policy::approve_destructive();
    policy.categories.insert(
        PermissionCategory::Script,
        permission_engine::Rule::new(permission_engine::Level::Allow),
    );
    let lp = make_loop(&env.storage, env.bot, &fake, &[], profile(""), cfg(policy))
        .with_builtin_tools(builtin_cfg(&env, Arc::new(NoGate)));
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let Event::ApprovalNeeded(a) = until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
    else {
        unreachable!()
    };
    lp.deny(a.call_id, env.user, Some("no".into())).unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert!(!std::path::Path::new(&file).exists());
    assert!(tool_messages(&fake.request(1))[0].contains("blocked"));
}

// ---------- tool surface ----------

#[tokio::test]
async fn many_tools_switch_to_discovery_and_use_tool_is_gated_by_the_target() {
    let env = setup();
    let r = Arc::new(tool("t.read", Effect::Read, "read ok"));
    let w = Arc::new(tool("t.write", Effect::Write, "write ok"));
    let extra: Vec<Arc<TestTool>> = (0..4)
        .map(|i| Arc::new(tool(&format!("t.extra{i}"), Effect::Read, "x")))
        .collect();
    let mut all = vec![t(&r), t(&w)];
    all.extend(extra.iter());
    let fake = Fake::new(vec![
        calls(&[("c1", "use_tool", json!({"name": "t.read", "input": {}}))]),
        calls(&[("c2", "use_tool", json!({"name": "t.write", "input": {}}))]),
        text("done"),
    ]);
    let mut config = cfg(Policy::approve_destructive());
    config.discovery_threshold = 3;
    let lp = make_loop(&env.storage, env.bot, &fake, &all, profile(""), config);
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    // The read target runs without a prompt; the write target asks (as t.write).
    let Event::ApprovalNeeded(a) = until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
    else {
        unreachable!()
    };
    assert_eq!(a.tool_name, "t.write");
    assert_eq!(r.count.load(Ordering::SeqCst), 1);
    assert_eq!(w.count.load(Ordering::SeqCst), 0);
    lp.approve(a.call_id, ApprovalScope::Once, env.user)
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(w.count.load(Ordering::SeqCst), 1);

    let names: Vec<String> = fake
        .request(0)
        .tools
        .iter()
        .map(|d| d["function"]["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names, ["find_tool", "get_tool_details", "use_tool"]);
    assert_eq!(tool_messages(&fake.request(1)), vec!["read ok".to_owned()]);
}

#[tokio::test]
async fn use_tool_cannot_reach_tools_hidden_from_the_bot() {
    let env = setup();
    let w = Arc::new(tool("t.write", Effect::Write, "w"));
    let extra: Vec<Arc<TestTool>> = (0..4)
        .map(|i| Arc::new(tool(&format!("t.extra{i}"), Effect::Read, "x")))
        .collect();
    let mut all = vec![t(&w)];
    all.extend(extra.iter());
    let fake = Fake::new(vec![
        calls(&[("c1", "use_tool", json!({"name": "t.write", "input": {}}))]),
        text("blocked"),
    ]);
    let mut config = cfg(Policy::full());
    config.discovery_threshold = 3;
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &all,
        profile("[tools]\nexclude = [\"t.write\"]\n"),
        config,
    );
    let run = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(w.count.load(Ordering::SeqCst), 0);
    assert!(tool_messages(&fake.request(1))[0].contains("unknown tool"));
}

fn req_tool_names(req: &ChatRequest) -> Vec<String> {
    req.tools
        .iter()
        .map(|d| d["function"]["name"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn mcp_tools_never_push_native_tools_behind_use_tool() {
    let env = setup();
    let native: Vec<Arc<TestTool>> = (0..40)
        .map(|i| Arc::new(tool(&format!("n.t{i:02}"), Effect::Read, "x")))
        .collect();
    let mcp_write = Arc::new(tool("mcp:x/t00", Effect::Write, "mcp ok"));
    let mut mcp: Vec<Arc<TestTool>> = vec![Arc::clone(&mcp_write)];
    mcp.extend((1..30).map(|i| Arc::new(tool(&format!("mcp:x/t{i:02}"), Effect::Read, "x"))));
    let all: Vec<&Arc<TestTool>> = native.iter().chain(mcp.iter()).collect();
    let fake = Fake::new(vec![
        // A namespaced tool called directly is unknown; via use_tool it asks (Write).
        calls(&[("c1", "mcp:x/t01", json!({}))]),
        calls(&[("c2", "use_tool", json!({"name": "mcp:x/t00", "input": {}}))]),
        text("done"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &all,
        profile(""),
        cfg(Policy::approve_destructive()),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let Event::ApprovalNeeded(a) = until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
    else {
        unreachable!()
    };
    // The permission check evaluates the use_tool *target*.
    assert_eq!(a.tool_name, "mcp:x/t00");
    lp.approve(a.call_id, ApprovalScope::Once, env.user)
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(mcp_write.count.load(Ordering::SeqCst), 1);

    let names = req_tool_names(&fake.request(0));
    assert_eq!(names.len(), 43, "{names:?}");
    assert!(
        native
            .iter()
            .all(|n| names.contains(&n.spec.name.replace('.', "__")))
    );
    assert!(names.ends_with(&[
        "find_tool".to_owned(),
        "get_tool_details".to_owned(),
        "use_tool".to_owned()
    ]));
    assert!(!names.iter().any(|n| n.starts_with("mcp:")));
    assert!(tool_messages(&fake.request(1))[0].contains("unknown tool"));
}

#[tokio::test]
async fn hidden_native_tools_lower_the_counted_surface_size() {
    let env = setup();
    let native: Vec<Arc<TestTool>> = (0..6)
        .map(|i| Arc::new(tool(&format!("n.t{i}"), Effect::Read, "x")))
        .collect();
    let mcp = Arc::new(tool("mcp:x/a", Effect::Read, "x"));
    let mut all: Vec<&Arc<TestTool>> = native.iter().collect();
    all.push(&mcp);
    let mut config = cfg(Policy::full());
    config.discovery_threshold = 4;

    // 6 visible native tools > 4: discovery.
    let fake = Fake::new(vec![text("a")]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &all,
        profile(""),
        config.clone(),
    );
    let run = lp.run_turn(env.chat, "go").unwrap();
    lp.wait(run).await.unwrap();
    assert_eq!(
        req_tool_names(&fake.request(0)),
        ["find_tool", "get_tool_details", "use_tool"]
    );

    // Hiding two lowers the count to 4: native tools are direct again.
    let fake = Fake::new(vec![text("b")]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &all,
        profile("[tools]\nexclude = [\"n.t0\", \"n.t1\"]\n"),
        config,
    );
    let run = lp.run_turn(env.chat, "go").unwrap();
    lp.wait(run).await.unwrap();
    assert_eq!(req_tool_names(&fake.request(0)).len(), 4 + 3);
}

#[tokio::test]
async fn platform_hidden_tool_cannot_be_called_by_name() {
    let env = setup();
    let other = if cfg!(windows) {
        Platform::Linux
    } else {
        Platform::Windows
    };
    let mut foreign = tool("foreign_click", Effect::Write, "clicked");
    foreign.spec.platforms = vec![other];
    let foreign = Arc::new(foreign);
    let local = Arc::new(tool("local_read", Effect::Read, "ok"));
    let fake = Fake::new(vec![
        calls(&[("c1", "foreign_click", json!({}))]),
        text("done"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&foreign), t(&local)],
        profile(""),
        cfg(Policy::full()),
    );
    let run = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(foreign.count.load(Ordering::SeqCst), 0);
    let names = req_tool_names(&fake.request(0));
    assert!(names.contains(&"local_read".to_owned()), "{names:?}");
    assert!(!names.iter().any(|n| n == "foreign_click"), "{names:?}");
    assert!(tool_messages(&fake.request(1))[0].contains("unknown tool"));
}

#[tokio::test]
async fn k2_tool_candidates_carry_schema_parameters() {
    let env = setup();
    let mut with_schema = tool("n.send", Effect::Read, "x");
    with_schema.spec.input_schema = json!({
        "type": "object",
        "properties": { "to": {}, "body": {} },
        "required": ["to"]
    });
    let w = Arc::new(with_schema);
    let fake = Fake::new(vec![text("a")]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[&w],
        profile(""),
        cfg(Policy::full()),
    );
    let cands = lp.sh.router_candidates(&profile(""));
    let c = cands.iter().find(|c| c.name == "n.send").unwrap();
    let json = serde_json::to_value(c).unwrap();
    let params = json["params"].as_array().unwrap();
    assert_eq!(params.len(), 2);
    assert!(
        params
            .iter()
            .any(|p| p["name"] == "to" && p["required"] == true)
    );
    assert!(
        params
            .iter()
            .any(|p| p["name"] == "body" && p["required"] == false)
    );
}

// ---------- redaction ----------

#[tokio::test]
async fn redactor_masks_tool_output_and_events() {
    let env = setup();
    let secret = "sk-abcdefghijklmnopqrstuvwxyz0123";
    let r = Arc::new(tool("t.read", Effect::Read, &format!("key is {secret} ok")));
    let fake = Fake::new(vec![
        calls(&[("c1", "t.read", json!({"note": secret}))]),
        text("done"),
    ]);
    let mut config = cfg(Policy::full());
    config.redactor = Some(secret_store::Redactor::new().into());
    let lp = make_loop(&env.storage, env.bot, &fake, &[t(&r)], profile(""), config);
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);

    let to_model = tool_messages(&fake.request(1));
    assert!(!to_model[0].contains(secret), "{to_model:?}");
    assert!(to_model[0].contains("[REDACTED]"));

    let mut saw_requested = false;
    while let Ok(i) = rx.try_recv() {
        let json = serde_json::to_string(&i.envelope.event).unwrap();
        assert!(!json.contains(secret), "leaked in event: {json}");
        saw_requested |= matches!(i.envelope.event, Event::ToolCallRequested(_));
    }
    assert!(saw_requested);
    let log = serde_json::to_string(&read_event_log(env.dir.path(), run).unwrap()).unwrap();
    assert!(log.contains("[REDACTED]"));
    assert!(!log.contains(secret));
}

// SEC-02 (redaction half): the production redactor is a shared handle that is
// refreshed from the secret store; a secret stored after the loop started is
// masked in the provider message, the broadcast stream and the stored log.
#[tokio::test]
async fn refreshed_redactor_masks_a_newly_stored_secret_everywhere() {
    let env = setup();
    let sentinel = "vault-sentinel-VALUE-4711";
    let r = Arc::new(tool(
        "t.read",
        Effect::Read,
        &format!("data {sentinel} end"),
    ));
    let fake = Fake::new(vec![
        calls(&[("c1", "t.read", json!({"note": sentinel}))]),
        text("done"),
    ]);
    let handle = crate::SharedRedactor::default();
    let mut config = cfg(Policy::full());
    config.redactor = Some(handle.clone());
    let lp = make_loop(&env.storage, env.bot, &fake, &[t(&r)], profile(""), config);
    // Not yet known: nothing to mask.
    assert!(!handle.contains_secret(sentinel));
    // A secret is stored later and the daemon refreshes the handle.
    let store = secret_store::MemoryStore::new();
    secret_store::SecretStore::set(&store, "k", sentinel).unwrap();
    handle
        .refresh_from_store(Some(&store), &["daemon-bearer-token-xyz".to_owned()])
        .unwrap();
    assert!(handle.contains_secret("daemon-bearer-token-xyz"));

    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);

    let to_model = tool_messages(&fake.request(1));
    assert!(!to_model[0].contains(sentinel), "{to_model:?}");
    assert!(to_model[0].contains("[REDACTED]"));
    let mut n = 0;
    while let Ok(i) = rx.try_recv() {
        n += 1;
        let json = serde_json::to_string(&i.envelope.event).unwrap();
        assert!(!json.contains(sentinel), "leaked on the stream: {json}");
    }
    assert!(n > 0);
    let log = serde_json::to_string(&read_event_log(env.dir.path(), run).unwrap()).unwrap();
    assert!(
        log.contains("[REDACTED]") && !log.contains(sentinel),
        "{log}"
    );
}

// ---------- event journal (REL-02) ----------

#[derive(Clone, Default)]
struct ProbeSink {
    written: Arc<Mutex<Vec<storage::JournalRecord>>>,
    sync_delay: Duration,
}

impl storage::JournalSink for ProbeSink {
    fn write(&mut self, rec: &storage::JournalRecord) -> storage::Result<()> {
        self.written.lock().push(rec.clone());
        Ok(())
    }
    fn sync(&mut self) -> storage::Result<()> {
        std::thread::sleep(self.sync_delay);
        Ok(())
    }
}

/// A loop with a finished run whose journal writes into `probe`.
async fn loop_with_probe(probe: &ProbeSink) -> (Env, AgentLoop, protocol::RunId) {
    let env = setup();
    let fake = Fake::new(vec![text("hi")]);
    let mut config = cfg(Policy::full());
    config.journal = crate::JournalSlot::with_journal(storage::EventJournal::new(probe.clone()));
    let lp = make_loop(&env.storage, env.bot, &fake, &[], profile(""), config);
    let run = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    (env, lp, run)
}

fn run_ctx(lp: &AgentLoop, run: protocol::RunId) -> Arc<crate::shared::RunCtx> {
    // A finished run has dropped its context; rebuild it from the stored row.
    let row = lp.sh.storage.get_run(run).unwrap().unwrap();
    lp.sh.ctx(&row).unwrap()
}

fn delta_event(run: protocol::RunId, text: &str) -> Event {
    Event::MessageDelta(protocol::event::MessageDelta {
        run_id: run,
        text: text.into(),
    })
}

fn barrier_event(run: protocol::RunId) -> Event {
    Event::Error(protocol::event::RunError {
        run_id: run,
        message: "x".into(),
        recoverable: true,
    })
}

fn todo_event(run: protocol::RunId) -> Event {
    Event::TodoUpdated(protocol::event::TodoUpdated {
        run_id: run,
        items: vec![],
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn emit_does_not_block_on_a_slow_fsync_and_barrier_events_are_durable() {
    let probe = ProbeSink {
        sync_delay: Duration::from_millis(400),
        ..ProbeSink::default()
    };
    let (_env, lp, run) = loop_with_probe(&probe).await;
    let ctx = run_ctx(&lp, run);
    let start = std::time::Instant::now();
    for i in 0..100 {
        lp.sh.emit(&ctx, delta_event(run, &format!("t{i} ")));
    }
    assert!(
        start.elapsed() < Duration::from_millis(300),
        "emit waited for the disk: {:?}",
        start.elapsed()
    );
    // A barrier event (approval/state) returns only once it is written.
    lp.sh.emit(&ctx, barrier_event(run));
    let w = probe.written.lock();
    assert!(
        w.iter()
            .any(|r| matches!(r.envelope.event, Event::Error(_))),
        "barrier event not durable on return"
    );
    // Every delta enqueued before it is there too, in order.
    let text: String = w
        .iter()
        .filter_map(|r| match &r.envelope.event {
            Event::MessageDelta(d) => Some(d.text.clone()),
            _ => None,
        })
        .collect();
    assert!(text.ends_with("t99 "), "{text}");
    assert!(text.contains("t0 t1 t2 "), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_emit_keeps_gseq_and_seq_monotonic_in_journal_and_stream() {
    let probe = ProbeSink::default();
    let (_env, lp, run) = loop_with_probe(&probe).await;
    let ctx = run_ctx(&lp, run);
    let lp = Arc::new(lp);
    let mut rx = lp.subscribe();
    let mut hs = Vec::new();
    for _ in 0..8 {
        let (lp, ctx) = (Arc::clone(&lp), Arc::clone(&ctx));
        hs.push(std::thread::spawn(move || {
            for _ in 0..50 {
                lp.sh.emit(&ctx, todo_event(ctx.run_id));
            }
        }));
    }
    for h in hs {
        h.join().unwrap();
    }
    lp.sh.emit(&ctx, barrier_event(run));
    let mut streamed = Vec::new();
    while let Ok(i) = rx.try_recv() {
        streamed.push((i.gseq, i.envelope.seq));
    }
    assert_eq!(streamed.len(), 401);
    assert!(
        streamed
            .windows(2)
            .all(|p| p[0].0 < p[1].0 && p[0].1 < p[1].1)
    );
    let w = probe.written.lock();
    let first = streamed[0].0;
    let journaled: Vec<(u64, u64)> = w
        .iter()
        .filter(|r| r.gseq >= first)
        .map(|r| (r.gseq, r.envelope.seq))
        .collect();
    assert_eq!(journaled, streamed, "journal order must equal stream order");
}

// ---------- memory ----------

fn memory_cfg() -> (Arc<memory_store::MemoryStore>, crate::MemoryConfig) {
    let store = Arc::new(
        memory_store::MemoryStore::open_in_memory(Arc::new(memory_store::HashEmbedder::default()))
            .unwrap(),
    );
    let cfg = crate::MemoryConfig::new(Arc::clone(&store));
    (store, cfg)
}

#[tokio::test]
async fn recalled_memory_is_prepended_to_the_user_message_not_the_system_prompt() {
    let env = setup();
    let (store, mem) = memory_cfg();
    store
        .add(
            &memory_store::NewMemory::new(
                env.user,
                None,
                memory_store::Kind::Profile,
                "The user loves jazz music",
            ),
            "test",
        )
        .unwrap();
    // Reply 2 answers the background extraction call (no candidates).
    let fake = Fake::new(vec![text("jazz it is"), text("[]")]);
    let mut config = cfg(Policy::full());
    config.memory = Some(mem);
    let lp = make_loop(&env.storage, env.bot, &fake, &[], profile(""), config);
    let run = lp.run_turn(env.chat, "recommend some jazz music").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    lp.flush_background().await;

    let req = fake.request(0);
    let system = req.messages[0].content.as_str().unwrap();
    assert!(!system.contains("jazz"), "system prompt stays memory-free");
    let user = req.messages[1].content.as_str().unwrap();
    assert!(
        user.contains("<recalled_memory>") && user.contains("loves jazz"),
        "{user}"
    );
    assert!(user.ends_with("recommend some jazz music"));
    // The stored chat message is the raw one.
    assert_eq!(
        env.storage.list_messages(env.chat).unwrap()[0].content,
        "recommend some jazz music"
    );
}

#[tokio::test]
async fn finished_turn_is_extracted_into_memory_in_the_background() {
    let env = setup();
    let (store, mem) = memory_cfg();
    let extraction = json!([{
        "kind": "profile",
        "content": "The user is called Ayse",
        "quote": "I am Ayse",
        "hypothetical": false,
        "importance": 0.8
    }])
    .to_string();
    let fake = Fake::new(vec![text("Nice to meet you"), text(&extraction)]);
    let mut config = cfg(Policy::full());
    config.memory = Some(mem);
    let lp = make_loop(&env.storage, env.bot, &fake, &[], profile(""), config);
    let run = lp.run_turn(env.chat, "I am Ayse").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    lp.flush_background().await;

    assert_eq!(fake.n_requests(), 2, "second request is the extraction");
    let prompt = fake.request(1).messages[0]
        .content
        .as_str()
        .unwrap()
        .to_owned();
    assert!(prompt.contains("User: I am Ayse") && prompt.contains("Assistant: Nice to meet you"));
    let mems = store
        .list(env.user, &memory_store::ListFilter::default())
        .unwrap();
    assert_eq!(mems.len(), 1);
    assert_eq!(mems[0].content, "The user is called Ayse");
}

#[tokio::test]
async fn disabled_memory_does_nothing() {
    let env = setup();
    let (store, mut mem) = memory_cfg();
    mem.enabled = false;
    let fake = Fake::new(vec![text("hi"), text("[]")]);
    let mut config = cfg(Policy::full());
    config.memory = Some(mem);
    let lp = make_loop(&env.storage, env.bot, &fake, &[], profile(""), config);
    let run = lp.run_turn(env.chat, "I am Ayse").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    lp.flush_background().await;
    assert_eq!(fake.n_requests(), 1);
    assert!(
        store
            .list(env.user, &memory_store::ListFilter::default())
            .unwrap()
            .is_empty()
    );
}

// ---------- discovery visibility ----------

#[tokio::test]
async fn find_tool_and_get_tool_details_only_see_tools_visible_to_the_bot() {
    let env = setup();
    let secret = Arc::new(tool("t.secret", Effect::Read, "s"));
    let extra: Vec<Arc<TestTool>> = (0..4)
        .map(|i| Arc::new(tool(&format!("t.extra{i}"), Effect::Read, "x")))
        .collect();
    let mut all = vec![t(&secret)];
    all.extend(extra.iter());
    let fake = Fake::new(vec![
        calls(&[("c1", "find_tool", json!({"query": "test tool", "k": 25}))]),
        calls(&[("c2", "get_tool_details", json!({"name": "t.secret"}))]),
        text("done"),
    ]);
    let mut config = cfg(Policy::full());
    config.discovery_threshold = 3;
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &all,
        profile("[tools]\nexclude = [\"t.secret\"]\n"),
        config,
    );
    let run = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);

    let found = &tool_messages(&fake.request(1))[0];
    assert!(found.contains("t.extra0"), "{found}");
    assert!(!found.contains("t.secret"), "leaked via find_tool: {found}");
    let details = &tool_messages(&fake.request(2))[1];
    assert!(details.contains("unknown tool"), "{details}");
    assert!(!details.contains("input_schema"), "{details}");
}

// ---------- global event order ----------

#[tokio::test]
async fn events_carry_a_persisted_daemon_wide_sequence() {
    let env = setup();
    let fake = Fake::new(vec![text("one"), text("two")]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[],
        profile(""),
        cfg(Policy::full()),
    );
    let mut rx = lp.subscribe();
    let r1 = lp.run_turn(env.chat, "a").unwrap();
    lp.wait(r1).await.unwrap();
    let mut items = Vec::new();
    while let Ok(i) = rx.try_recv() {
        items.push(i);
    }
    assert!(items.len() >= 3);
    assert!(
        items.iter().zip(1u64..).all(|(i, n)| i.gseq == n),
        "gapless, starting at 1"
    );
    let idx = env.storage.list_event_index(0, 10_000).unwrap();
    let got: Vec<_> = idx.iter().map(|r| (r.gseq, r.run_id, r.seq)).collect();
    let want: Vec<_> = items
        .iter()
        .map(|i| (i.gseq, i.run_id, i.envelope.seq))
        .collect();
    assert_eq!(got, want);

    // A fresh loop over the same storage continues the counter.
    let fake2 = Fake::new(vec![text("two")]);
    let lp2 = make_loop(
        &env.storage,
        env.bot,
        &fake2,
        &[],
        profile(""),
        cfg(Policy::full()),
    );
    let mut rx2 = lp2.subscribe();
    let r2 = lp2.run_turn(env.chat, "b").unwrap();
    lp2.wait(r2).await.unwrap();
    let first = rx2.try_recv().unwrap();
    assert_eq!(first.gseq, items.len() as u64 + 1);
}

// ---------- effective policy ----------

#[tokio::test]
async fn effective_policy_merges_category_grants_but_not_floors_or_denies() {
    let env = setup();
    let fake = Fake::new(vec![]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[],
        profile(""),
        cfg(Policy::approve_destructive()),
    );
    let before = lp.effective_policy(env.bot).unwrap();
    assert!(before.policy.is_none() && before.grants.is_empty());
    assert_eq!(
        before.categories[&PermissionCategory::Fs].decision,
        crate::PolicyDecision::Ask
    );
    for c in [PermissionCategory::Fs, PermissionCategory::Money] {
        lp.sh.grants.lock().always.push(crate::Grant {
            bot_id: env.bot,
            target: GrantTarget::Category(c),
            granted_at_ms: 1,
        });
    }
    lp.sh.grants.lock().always.push(crate::Grant {
        bot_id: env.bot,
        target: GrantTarget::Tool("t.x".into()),
        granted_at_ms: 2,
    });
    let eff = lp.effective_policy(env.bot).unwrap();
    let fs = &eff.categories[&PermissionCategory::Fs];
    assert_eq!(
        (fs.decision.clone(), fs.via_grant),
        (crate::PolicyDecision::Allow, true)
    );
    let money = &eff.categories[&PermissionCategory::Money];
    assert_eq!(
        (money.decision.clone(), money.via_grant),
        (crate::PolicyDecision::Ask, false),
        "the money floor is never waived"
    );
    assert_eq!(
        eff.categories[&PermissionCategory::Shell].decision,
        crate::PolicyDecision::Ask
    );
    assert_eq!(eff.grants.len(), 3);
    assert!(!eff.categories.contains_key(&PermissionCategory::Meta));

    // A bot-level deny is not widened by a grant.
    let lp2 = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[],
        profile("[permissions]\npreset = \"read-only\"\n"),
        cfg(Policy::full()),
    );
    lp2.sh.grants.lock().always.push(crate::Grant {
        bot_id: env.bot,
        target: GrantTarget::Category(PermissionCategory::Fs),
        granted_at_ms: 1,
    });
    let eff2 = lp2.effective_policy(env.bot).unwrap();
    assert!(eff2.policy.is_some());
    assert_eq!(
        eff2.categories[&PermissionCategory::Fs].decision,
        crate::PolicyDecision::Deny
    );
    assert!(lp2.effective_policy(BotId::new()).is_err());
}

// ---------- shutdown ----------

fn step_states(storage: &Storage, run: protocol::RunId) -> Vec<(String, String)> {
    storage
        .list_steps(run)
        .unwrap()
        .into_iter()
        .map(|s| (s.name, s.status))
        .collect()
}

#[tokio::test]
async fn shutdown_cancels_a_model_call_pauses_the_run_and_refuses_new_work() {
    let env = setup();
    let fake = Fake::new(vec![Reply::Hang]);
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
    let started = std::time::Instant::now();
    let paused = lp.shutdown(Duration::from_secs(30)).await;
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "a model stream is cancelled at once, not waited for"
    );
    assert_eq!(paused, vec![run]);
    assert_eq!(lp.runtime().state(run).unwrap(), RunState::Paused);
    assert_eq!(
        step_states(&env.storage, run).last().unwrap(),
        &("model".to_owned(), "interrupted".to_owned())
    );
    assert!(matches!(
        lp.run_turn(env.chat, "again"),
        Err(crate::LoopError::ShuttingDown)
    ));
    assert!(matches!(
        lp.send_user_message(env.chat, "again", MessageMode::Queue),
        Err(crate::LoopError::ShuttingDown)
    ));
    assert!(matches!(
        lp.resume(run),
        Err(crate::LoopError::ShuttingDown)
    ));

    // Next start: `recover` has nothing left to do and the run resumes
    // (the idempotent model step is replayed).
    let fake2 = Fake::new(vec![text("hello again")]);
    let lp2 = make_loop(
        &env.storage,
        env.bot,
        &fake2,
        &[],
        profile(""),
        cfg(Policy::full()),
    );
    assert!(lp2.recover().unwrap().is_empty());
    lp2.resume(run).unwrap();
    assert_eq!(lp2.wait(run).await.unwrap(), RunState::Succeeded);
    // The paused run's log has no `run_finished` before the resume.
    let log = read_event_log(env.dir.path(), run).unwrap();
    let finished = log
        .iter()
        .filter(|e| matches!(e.event, Event::RunFinished(_)))
        .count();
    assert_eq!(finished, 1);
    assert!(
        log.iter()
            .any(|e| matches!(&e.event, Event::Error(x) if x.recoverable))
    );
}

#[tokio::test]
async fn shutdown_waits_for_a_running_tool_step_within_the_grace_period() {
    let env = setup();
    let mut slow = tool("t.write", Effect::Write, "written");
    slow.delay = Duration::from_millis(300);
    let slow = Arc::new(slow);
    let fake = Fake::new(vec![calls(&[("c1", "t.write", json!({}))]), Reply::Hang]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&slow)],
        profile(""),
        cfg(Policy::full()),
    );
    let run = lp.run_turn(env.chat, "write").unwrap();
    for _ in 0..500 {
        if slow.count.load(Ordering::SeqCst) == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(slow.count.load(Ordering::SeqCst), 1, "tool is running");
    let paused = lp.shutdown(Duration::from_secs(30)).await;
    assert_eq!(paused, vec![run]);
    let steps = step_states(&env.storage, run);
    assert_eq!(
        steps.last().unwrap(),
        &("tools".to_owned(), "succeeded".to_owned()),
        "the tool step finished and its result is durable: {steps:?}"
    );
    // The next model call was never started.
    assert_eq!(fake.n_requests(), 1);
    assert_eq!(lp.runtime().state(run).unwrap(), RunState::Paused);

    // Resume continues from the stored tool result; the tool is not re-run.
    let fake2 = Fake::new(vec![text("all written")]);
    let again = Arc::new(tool("t.write", Effect::Write, "again"));
    let lp2 = make_loop(
        &env.storage,
        env.bot,
        &fake2,
        &[t(&again)],
        profile(""),
        cfg(Policy::full()),
    );
    lp2.resume(run).unwrap();
    assert_eq!(lp2.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(again.count.load(Ordering::SeqCst), 0);
    assert_eq!(tool_messages(&fake2.request(0)), vec!["written".to_owned()]);
}

#[tokio::test]
async fn shutdown_interrupts_a_tool_that_outlives_the_grace_period() {
    let env = setup();
    let mut hang = tool("t.write", Effect::Write, "never");
    hang.delay = Duration::from_secs(3600);
    let hang = Arc::new(hang);
    let fake = Fake::new(vec![calls(&[("c1", "t.write", json!({}))])]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&hang)],
        profile(""),
        cfg(Policy::full()),
    );
    let run = lp.run_turn(env.chat, "write").unwrap();
    for _ in 0..500 {
        if hang.count.load(Ordering::SeqCst) == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let paused = lp.shutdown(Duration::from_millis(150)).await;
    assert_eq!(paused, vec![run]);
    assert_eq!(
        step_states(&env.storage, run).last().unwrap(),
        &("tools".to_owned(), "interrupted".to_owned())
    );

    // On resume the interrupted (side-effecting) tool is never replayed.
    let fake2 = Fake::new(vec![text("could not confirm")]);
    let w = Arc::new(tool("t.write", Effect::Write, "w"));
    let lp2 = make_loop(
        &env.storage,
        env.bot,
        &fake2,
        &[t(&w)],
        profile(""),
        cfg(Policy::full()),
    );
    lp2.resume(run).unwrap();
    assert_eq!(lp2.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(w.count.load(Ordering::SeqCst), 0);
    assert!(tool_messages(&fake2.request(0))[0].contains("interrupted"));
}

#[tokio::test]
async fn shutdown_pauses_a_run_waiting_for_approval_and_expires_the_request() {
    let env = setup();
    let w = Arc::new(tool("t.write", Effect::Write, "w"));
    let fake = Fake::new(vec![calls(&[("c1", "t.write", json!({}))])]);
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
    let paused = lp.shutdown(Duration::from_secs(30)).await;
    assert_eq!(paused, vec![run]);
    assert_eq!(lp.runtime().state(run).unwrap(), RunState::Paused);
    let approvals = env.storage.list_approvals(run).unwrap();
    assert_eq!(approvals[0].decision.as_deref(), Some("expired"));
    assert_eq!(w.count.load(Ordering::SeqCst), 0);
    assert!(matches!(
        lp.approve(a.call_id, ApprovalScope::Once, env.user),
        Err(crate::LoopError::NoPendingApproval(_))
    ));
}

#[tokio::test]
async fn shutdown_with_nothing_running_returns_immediately_and_flushes_memory() {
    let env = setup();
    let (store, mem) = memory_cfg();
    let extraction = json!([{
        "kind": "profile",
        "content": "The user is called Ayse",
        "quote": "I am Ayse",
        "hypothetical": false,
        "importance": 0.8
    }])
    .to_string();
    let fake = Fake::new(vec![text("hi Ayse"), text(&extraction)]);
    let mut config = cfg(Policy::full());
    config.memory = Some(mem);
    let lp = make_loop(&env.storage, env.bot, &fake, &[], profile(""), config);
    let run = lp.run_turn(env.chat, "I am Ayse").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert!(lp.shutdown(Duration::from_secs(5)).await.is_empty());
    assert_eq!(
        store
            .list(env.user, &memory_store::ListFilter::default())
            .unwrap()
            .len(),
        1,
        "background extraction was flushed"
    );
}

// ---------- tool output preview / duration ----------

fn completed_events(
    rx: &mut broadcast::Receiver<StreamItem>,
) -> Vec<protocol::event::ToolCallCompleted> {
    let mut out = Vec::new();
    while let Ok(i) = rx.try_recv() {
        if let Event::ToolCallCompleted(c) = i.envelope.event {
            out.push(c);
        }
    }
    out
}

#[tokio::test]
async fn output_preview_is_redacted_and_capped_at_2kb() {
    let env = setup();
    let secret = "sk-abcdefghijklmnopqrstuvwxyz0123";
    let small = Arc::new(tool(
        "t.small",
        Effect::Read,
        &format!("key is {secret} ok"),
    ));
    let big = Arc::new(tool("t.big", Effect::Read, &"x".repeat(5000)));
    let fake = Fake::new(vec![
        calls(&[("c1", "t.small", json!({}))]),
        calls(&[("c2", "t.big", json!({}))]),
        text("done"),
    ]);
    let mut config = cfg(Policy::full());
    config.redactor = Some(secret_store::Redactor::new().into());
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&small), t(&big)],
        profile(""),
        config,
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);

    let done = completed_events(&mut rx);
    assert_eq!(done.len(), 2);
    let p0 = done[0].output_preview.as_deref().unwrap();
    assert!(p0.contains("[REDACTED]") && !p0.contains(secret), "{p0}");
    let p1 = done[1].output_preview.as_deref().unwrap();
    assert!(p1.len() <= 2048, "{}", p1.len());
    assert!(p1.ends_with("[truncated]"), "{p1}");
    assert!(done.iter().all(|c| c.duration_ms.is_some()));
}

#[tokio::test]
async fn tool_duration_excludes_the_approval_wait() {
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
    tokio::time::sleep(Duration::from_millis(600)).await;
    lp.approve(a.call_id, ApprovalScope::Once, env.user)
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    let done = completed_events(&mut rx);
    let ms = done[0].duration_ms.unwrap();
    assert!(
        ms < 300,
        "the 600 ms approval wait leaked into the duration: {ms}"
    );
}

// ---------- desktop control bridge ----------

type GateSlot = Arc<Mutex<Option<Arc<dyn builtin_tools::DesktopControlGate>>>>;

struct GateTool {
    spec: ToolSpec,
    gate: GateSlot,
}

#[async_trait]
impl Tool for GateTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }
    async fn call(&self, ctx: &ToolContext, _: Value) -> Result<ToolOutput, ToolError> {
        let gate = self.gate.lock().clone().expect("gate installed");
        gate.before_action(&builtin_tools::ControlRequest {
            run_id: ctx.run_id.clone(),
            rect: Some(protocol::event::ControlRect {
                x: 10.0,
                y: 20.0,
                width: 30.0,
                height: 40.0,
            }),
            label: "Save".into(),
            action: "click".into(),
        })
        .await
        .map_err(ToolError::Execution)?;
        Ok(ToolOutput::text("clicked"))
    }
}

struct TestGate {
    refuse: bool,
    seen: Mutex<Vec<crate::ControlRequest>>,
}

#[async_trait]
impl crate::ControlGate for TestGate {
    async fn before_action(
        &self,
        req: crate::ControlRequest,
        publish: &(dyn Fn() + Send + Sync),
    ) -> Result<(), String> {
        self.seen.lock().push(req);
        publish();
        if self.refuse {
            Err("the user has taken control".into())
        } else {
            Ok(())
        }
    }
}

fn gate_loop(env: &Env, fake: &Arc<Fake>) -> AgentLoop {
    let slot: GateSlot = Arc::new(Mutex::new(None));
    let gt = Arc::new(GateTool {
        spec: ToolSpec {
            name: "t.desk".into(),
            description: "test desktop tool".into(),
            input_schema: json!({"type": "object"}),
            effect: Effect::Read,
            category: PermissionCategory::Desktop,
            platforms: vec![Platform::All],
        },
        gate: Arc::clone(&slot),
    });
    let registry = ToolRegistry::new();
    registry.register(gt as Arc<dyn Tool>);
    let lp = AgentLoop::new(
        Arc::clone(&env.storage),
        registry,
        Providers::single(Arc::clone(fake) as Arc<dyn ChatProvider>),
        cfg(Policy::full()),
    );
    lp.add_profile(env.bot, profile(""));
    *slot.lock() = Some(Arc::new(crate::control::LoopControl {
        sh: Arc::downgrade(&lp.sh),
    }));
    lp
}

#[tokio::test]
async fn control_gate_sees_the_run_and_can_publish_the_request_event() {
    let env = setup();
    let fake = Fake::new(vec![calls(&[("c1", "t.desk", json!({}))]), text("done")]);
    let lp = gate_loop(&env, &fake);
    let gate = Arc::new(TestGate {
        refuse: false,
        seen: Mutex::new(Vec::new()),
    });
    lp.set_control_gate(gate.clone());
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);

    let seen = gate.seen.lock().clone();
    assert_eq!(seen.len(), 1);
    assert_eq!((seen[0].run_id, seen[0].bot_id), (run, env.bot));
    assert_eq!(
        (seen[0].label.as_str(), seen[0].action.as_str()),
        ("Save", "click")
    );
    let mut event = None;
    while let Ok(i) = rx.try_recv() {
        if let Event::ControlRequested(c) = i.envelope.event {
            event = Some(c);
        }
    }
    let c = event.expect("control_requested was published");
    assert_eq!(c.request_id, seen[0].request_id);
    assert!((c.rect.unwrap().width - 30.0).abs() < 1e-9);
    assert!(tool_messages(&fake.request(1))[0].contains("clicked"));
}

#[tokio::test]
async fn a_refusing_control_gate_fails_the_tool_call_with_its_message() {
    let env = setup();
    let fake = Fake::new(vec![calls(&[("c1", "t.desk", json!({}))]), text("done")]);
    let lp = gate_loop(&env, &fake);
    lp.set_control_gate(Arc::new(TestGate {
        refuse: true,
        seen: Mutex::new(Vec::new()),
    }));
    let run = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert!(tool_messages(&fake.request(1))[0].contains("taken control"));
}

#[tokio::test]
async fn without_a_control_gate_the_action_just_runs() {
    let env = setup();
    let fake = Fake::new(vec![calls(&[("c1", "t.desk", json!({}))]), text("done")]);
    let lp = gate_loop(&env, &fake);
    let run = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert!(tool_messages(&fake.request(1))[0].contains("clicked"));
}

#[tokio::test]
async fn global_policy_can_be_replaced_at_runtime() {
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
    lp.set_global_policy(Policy::full());
    let run = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(
        lp.wait(run).await.unwrap(),
        RunState::Succeeded,
        "no approval needed any more"
    );
    assert_eq!(w.count.load(Ordering::SeqCst), 1);
}

// ---------- F05 regressions ----------

#[tokio::test]
async fn parallel_questions_keep_the_run_waiting_until_the_last_is_answered() {
    let env = setup();
    let fake = Fake::new(vec![
        calls(&[
            ("c1", "ask_user", json!({"question": "First?"})),
            ("c2", "ask_user", json!({"question": "Second?"})),
        ]),
        text("both answered"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[],
        profile(""),
        cfg(Policy::read_only()),
    );
    register_meta(&lp);
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let mut ids = Vec::new();
    for _ in 0..2 {
        let Event::QuestionAsked(q) =
            until(&mut rx, |e| matches!(e, Event::QuestionAsked(_))).await
        else {
            unreachable!()
        };
        ids.push(q.call_id);
    }
    assert_eq!(lp.runtime().state(run).unwrap(), RunState::WaitingApproval);

    lp.answer_question(ids[0], "one").unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        lp.runtime().state(run).unwrap(),
        RunState::WaitingApproval,
        "the second question is still open"
    );
    lp.answer_question(ids[1], "two").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert!(lp.sh.parked.lock().is_empty(), "park counter is released");
}

fn one_candidate() -> String {
    json!([{
        "kind": "profile",
        "content": "The user is called Ayse",
        "quote": "I am Ayse",
        "hypothetical": false,
        "importance": 0.8
    }])
    .to_string()
}

#[tokio::test]
async fn memory_from_non_human_turns_is_stored_untrusted() {
    use memory_store::Trust;
    let env = setup();
    let (store, mem) = memory_cfg();
    let fake = Fake::new(vec![text("Hello"), text(&one_candidate())]);
    let mut config = cfg(Policy::full());
    config.memory = Some(mem);
    let lp = make_loop(&env.storage, env.bot, &fake, &[], profile(""), config);
    let run = lp
        .run_turn_as(env.chat, "I am Ayse", protocol::InitiatorKind::Channel)
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    lp.flush_background().await;
    let mems = store
        .list(env.user, &memory_store::ListFilter::default())
        .unwrap();
    assert_eq!(mems.len(), 1);
    assert_eq!(mems[0].trust, Trust::Untrusted);
}

#[tokio::test]
async fn memory_from_a_turn_that_used_a_tool_is_stored_untrusted() {
    use memory_store::Trust;
    let env = setup();
    let (store, mem) = memory_cfg();
    let r = Arc::new(tool("t.read", Effect::Read, "page says: I am Ayse"));
    let fake = Fake::new(vec![
        calls(&[("c1", "t.read", json!({}))]),
        text("Noted"),
        text(&one_candidate()),
    ]);
    let mut config = cfg(Policy::full());
    config.memory = Some(mem);
    let lp = make_loop(&env.storage, env.bot, &fake, &[t(&r)], profile(""), config);
    let run = lp.run_turn(env.chat, "I am Ayse, read it").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    lp.flush_background().await;
    let mems = store
        .list(env.user, &memory_store::ListFilter::default())
        .unwrap();
    assert_eq!(mems.len(), 1);
    assert_eq!(mems[0].trust, Trust::Untrusted);
}

#[tokio::test]
async fn a_plain_human_turn_keeps_user_trust() {
    use memory_store::Trust;
    let env = setup();
    let (store, mem) = memory_cfg();
    let fake = Fake::new(vec![text("Hello"), text(&one_candidate())]);
    let mut config = cfg(Policy::full());
    config.memory = Some(mem);
    let lp = make_loop(&env.storage, env.bot, &fake, &[], profile(""), config);
    let run = lp.run_turn(env.chat, "I am Ayse").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    lp.flush_background().await;
    let mems = store
        .list(env.user, &memory_store::ListFilter::default())
        .unwrap();
    assert_eq!(mems[0].trust, Trust::User);
}

#[tokio::test]
async fn a_queued_follow_up_is_put_back_when_its_turn_cannot_start() {
    let env = setup();
    let mut slow = tool("t.slow", Effect::Read, "r");
    slow.delay = Duration::from_millis(200);
    let slow = Arc::new(slow);
    let fake = Fake::new(vec![calls(&[("c1", "t.slow", json!({}))]), text("first")]);
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
    lp.send_user_message(env.chat, "two", MessageMode::Queue)
        .unwrap();
    // Starting any turn now fails; the follow-up must not vanish.
    lp.sh
        .shutting_down
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let _ = lp.wait(run).await;
    until(&mut rx, |e| matches!(e, Event::RunFinished(_))).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let queued = lp
        .sh
        .queued
        .lock()
        .get(&env.chat)
        .cloned()
        .unwrap_or_default();
    assert_eq!(queued, ["two"]);
    assert!(
        !lp.sh.active.lock().contains_key(&env.chat),
        "the chat slot is released"
    );
}
