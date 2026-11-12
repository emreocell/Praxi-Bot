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
use permission_engine::{Level, Limits, Policy, Rule};
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
    /// HTTP 429 with an optional `Retry-After` duration.
    RateLimited(Option<Duration>),
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
            Some(Reply::RateLimited(retry_after)) => {
                Err(ProviderError::RateLimited { retry_after })
            }
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
    /// `Some(ids)`: only these bots may see the tool (like an MCP server's `bots`).
    pub(crate) bots: Option<Vec<String>>,
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
        bots: None,
    }
}

#[async_trait]
impl Tool for TestTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }
    fn visible_to(&self, bot_id: Option<&str>) -> bool {
        self.bots
            .as_ref()
            .is_none_or(|l| bot_id.is_some_and(|b| l.iter().any(|x| x == b)))
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

/// A loop whose registry holds the real web tools. No network is touched: no
/// search backend is configured and the default SSRF policy refuses every URL
/// used below before connecting.
fn web_loop(env: &Env, fake: &Arc<Fake>, profile: BotProfile, config: LoopConfig) -> AgentLoop {
    let registry = ToolRegistry::new();
    let mut web = builtin_tools::web_tools::WebToolsConfig::default();
    web.search.duckduckgo = false;
    builtin_tools::register_web_tools(&registry, web).unwrap();
    let lp = AgentLoop::new(
        Arc::clone(&env.storage),
        registry,
        Providers::single(Arc::clone(fake) as Arc<dyn ChatProvider>),
        config,
    );
    lp.add_profile(env.bot, profile);
    lp
}

#[tokio::test]
async fn web_search_runs_without_approval_under_the_default_policy() {
    let env = setup();
    let fake = Fake::new(vec![
        calls(&[("c1", "web_search", json!({"query": "x"}))]),
        text("done"),
    ]);
    let lp = web_loop(&env, &fake, profile(""), cfg(Policy::approve_destructive()));
    let run = lp.run_turn(env.chat, "search").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(env.storage.list_approvals(run).unwrap().len(), 0);
    // It ran (and failed offline: no backend), it was not parked.
    assert!(tool_messages(&fake.request(1))[0].contains("no search backend"));
}

#[tokio::test]
async fn web_fetch_asks_by_default_and_a_tool_grant_removes_the_prompt() {
    let env = setup();
    let url = json!({"url": "http://127.0.0.1:9/"});
    for name in ["web_extract", "web_feed", "web_sitemap", "web_metadata"] {
        let fake = Fake::new(vec![calls(&[("c1", name, url.clone())]), text("done")]);
        let lp = web_loop(&env, &fake, profile(""), cfg(Policy::approve_destructive()));
        let mut rx = lp.subscribe();
        let run = lp.run_turn(env.chat, "read it").unwrap();
        let Event::ApprovalNeeded(a) =
            until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
        else {
            unreachable!()
        };
        assert_eq!(a.tool_name, name);
        lp.approve(a.call_id, ApprovalScope::Once, env.user)
            .unwrap();
        assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
        // Approved, so it ran; the SSRF policy still refuses the target.
        assert!(tool_messages(&fake.request(1))[0].contains("blocked"));
    }

    // "Always allow this tool" for web_extract: the next call does not ask.
    let fake = Fake::new(vec![
        calls(&[("c1", "web_extract", url.clone())]),
        text("one"),
        calls(&[("c2", "web_extract", url.clone())]),
        text("two"),
    ]);
    let lp = web_loop(&env, &fake, profile(""), cfg(Policy::approve_destructive()));
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "first").unwrap();
    let Event::ApprovalNeeded(a) = until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
    else {
        unreachable!()
    };
    lp.approve(a.call_id, ApprovalScope::Always, env.user)
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    let run2 = lp.run_turn(env.chat, "second").unwrap();
    assert_eq!(lp.wait(run2).await.unwrap(), RunState::Succeeded);
    assert_eq!(
        env.storage.list_approvals(run2).unwrap().len(),
        0,
        "a tool-level grant removes the prompt"
    );
    assert!(tool_messages(&fake.request(3))[0].contains("blocked"));
}

#[tokio::test]
async fn browser_deny_still_denies_web_tools_and_write_browser_tools_still_ask() {
    let env = setup();
    let mut deny = Policy::approve_destructive();
    deny.categories
        .insert(PermissionCategory::Browser, Rule::new(Level::Deny));
    let fake = Fake::new(vec![
        calls(&[
            ("c1", "web_extract", json!({"url": "http://127.0.0.1:9/"})),
            ("c2", "web_search", json!({"query": "x"})),
            ("c3", "web_feed", json!({"url": "http://127.0.0.1:9/"})),
            ("c4", "web_sitemap", json!({"url": "http://127.0.0.1:9/"})),
            ("c5", "web_metadata", json!({"url": "http://127.0.0.1:9/"})),
        ]),
        text("stopped"),
    ]);
    let lp = web_loop(&env, &fake, profile(""), cfg(deny));
    let run = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    let msgs = tool_messages(&fake.request(1));
    assert_eq!(msgs.len(), 5);
    for m in &msgs {
        assert!(m.contains("Permission denied"), "{m}");
    }

    // A mutating browser tool is unchanged: it still parks for approval.
    let click = {
        let mut t = tool("browser_click", Effect::Write, "clicked");
        t.spec.category = PermissionCategory::Browser;
        Arc::new(t)
    };
    let fake = Fake::new(vec![
        calls(&[("c1", "browser_click", json!({}))]),
        text("fine"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&click)],
        profile(""),
        cfg(Policy::approve_destructive()),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "click").unwrap();
    let Event::ApprovalNeeded(a) = until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
    else {
        unreachable!()
    };
    assert_eq!(a.tool_name, "browser_click");
    assert_eq!(click.count.load(Ordering::SeqCst), 0);
    lp.cancel(run).unwrap();
}

#[test]
fn a_tool_allow_list_without_web_hides_the_web_tools() {
    let hidden = profile(
        "[tools]
allow = [\"fs_*\"]
",
    );
    let shown = profile(
        "[tools]
allow = [\"fs_*\", \"web_*\"]
",
    );
    for name in builtin_tools::web_tools::TOOL_NAMES {
        assert!(!hidden.tool_visible(name), "{name}");
        assert!(shown.tool_visible(name), "{name}");
    }
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
        Reply::Hang | Reply::Fail(_) | Reply::RateLimited(_) => unreachable!(),
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
    let cands = lp.sh.router_candidates(&profile(""), env.bot);
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

// ---------- review fix round: persistence, request, encodings, shutdown ----------

fn persona_profile(persona: &str) -> BotProfile {
    BotProfile::from_toml_str(&format!(
        "[bot]\nid = \"t\"\nname = \"T\"\npersona = \"{persona}\"\n[models]\nexecutor = \"fake/m\"\n"
    ))
    .unwrap()
}

fn redactor_with(secret: &str) -> crate::SharedRedactor {
    let h = crate::SharedRedactor::default();
    h.register(secret);
    h
}

#[tokio::test]
async fn persisted_messages_and_provider_request_are_redacted() {
    let env = setup();
    let secret = "plain-sentinel-KEY-5150";
    let fake = Fake::new(vec![text(&format!("the key is {secret}"))]);
    let mut config = cfg(Policy::full());
    config.redactor = Some(redactor_with(secret));
    // The secret sits in the system prompt (persona) and in the user text.
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[],
        persona_profile(&format!("You are T. Internal: {secret}")),
        config,
    );
    let run = lp
        .run_turn(env.chat, &format!("please keep {secret} safe"))
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    // Provider request: every message (system prompt and user text).
    let req = serde_json::to_string(&fake.request(0).messages).unwrap();
    assert!(!req.contains(secret), "leaked to the provider: {req}");
    assert!(req.contains("[REDACTED]"));
    // Stored chat messages: user text and the assistant's final answer.
    let msgs = env.storage.list_messages(env.chat).unwrap();
    assert!(msgs.len() >= 2);
    for m in &msgs {
        assert!(
            !m.content.contains(secret),
            "stored {}: {}",
            m.role,
            m.content
        );
    }
    assert!(msgs.iter().any(|m| m.content.contains("[REDACTED]")));
}

#[test]
fn redact_event_fails_closed_when_the_event_cannot_be_transformed() {
    let run = protocol::RunId::new();
    let r = redactor_with("fail-closed-secret-1");
    let ev = delta_event(run, "fail-closed-secret-1 raw");
    let broken = serde_json::from_str::<Value>("{");
    assert!(broken.is_err());
    let out = crate::shared::redact_event_checked(&r, run, ev, broken);
    let json = serde_json::to_string(&out).unwrap();
    assert!(!json.contains("fail-closed-secret-1"), "{json}");
    assert!(matches!(out, Event::Error(_)));
    // A redacted value that no longer deserialises is withheld too.
    let ev = delta_event(run, "x");
    let bad =
        json!({"type": "message_delta", "run_id": "not-a-uuid", "text": "fail-closed-secret-1"});
    let out = crate::shared::redact_event_checked(&r, run, ev, Ok(bad));
    assert!(matches!(out, Event::Error(_)), "{out:?}");
}

#[test]
fn encoded_forms_of_a_secret_are_masked() {
    use base64::Engine as _;
    let secret = "s3cr3t/Value+with=odd\"chars\nline2";
    let h = crate::SharedRedactor::default();
    h.refresh_from_store(None, &[secret.to_owned()]).unwrap();
    let json_form = serde_json::to_string(secret).unwrap();
    let url_form: String = secret
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    let b64_std = base64::engine::general_purpose::STANDARD.encode(secret);
    let b64_url = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(secret);
    // Embedded at every alignment inside a larger base64 text (basic-auth style).
    let embedded: Vec<String> = ["", "a", "ab"]
        .iter()
        .map(|p| base64::engine::general_purpose::STANDARD.encode(format!("{p}user:{secret}tail")))
        .collect();
    for (name, form) in [
        ("plain", secret.to_owned()),
        ("json", json_form),
        ("url", url_form),
        ("b64", b64_std),
        ("b64url", b64_url),
        ("emb0", embedded[0].clone()),
        ("emb1", embedded[1].clone()),
        ("emb2", embedded[2].clone()),
    ] {
        assert!(h.contains_secret(&format!("x {form} y")), "{name}: {form}");
    }
    // Whole-secret forms are fully masked, not just detected.
    assert!(
        !h.redact(&format!("a {} b", serde_json::to_string(secret).unwrap()))
            .contains("odd")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_flushes_non_barrier_tail_events_to_the_journal() {
    let probe = ProbeSink {
        sync_delay: Duration::from_millis(200),
        ..ProbeSink::default()
    };
    let (_env, lp, run) = loop_with_probe(&probe).await;
    let ctx = run_ctx(&lp, run);
    lp.sh.emit(&ctx, delta_event(run, "tail-before-shutdown"));
    lp.shutdown(Duration::from_millis(50)).await;
    let w = probe.written.lock();
    assert!(
        w.iter().any(|r| matches!(
            &r.envelope.event,
            Event::MessageDelta(d) if d.text == "tail-before-shutdown"
        )),
        "tail event not durable after shutdown"
    );
}

#[tokio::test]
async fn redactor_is_refreshed_at_run_start_and_before_the_user_message_is_stored() {
    let env = setup();
    let secret = "late-added-secret-8841";
    let fake = Fake::new(vec![text("ok")]);
    let handle = crate::SharedRedactor::default();
    // The "store" learns the secret only when the loop asks for a refresh.
    handle.set_refresher(Arc::new(move |h| h.register(secret)));
    let mut config = cfg(Policy::full());
    config.redactor = Some(handle.clone());
    let lp = make_loop(&env.storage, env.bot, &fake, &[], profile(""), config);
    assert!(!handle.contains_secret(secret));
    let run = lp.run_turn(env.chat, &format!("here: {secret}")).unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    let msgs = env.storage.list_messages(env.chat).unwrap();
    assert!(msgs.iter().all(|m| !m.content.contains(secret)), "{msgs:?}");
    let req = serde_json::to_string(&fake.request(0).messages).unwrap();
    assert!(!req.contains(secret), "{req}");
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

#[tokio::test]
async fn bot_scoped_tools_are_hidden_from_other_bots_in_every_model_surface() {
    let env = setup();
    let mut scoped = tool("mcp:srv/secret", Effect::Read, "s");
    scoped.bots = Some(vec!["some-other-bot".into()]);
    let scoped = Arc::new(scoped);
    let mut open = tool("mcp:open/ok", Effect::Read, "ok");
    open.bots = None;
    let open = Arc::new(open);
    let mut listed = tool("mcp:mine/ok", Effect::Read, "ok");
    listed.bots = Some(vec![env.bot.to_string()]);
    let listed = Arc::new(listed);
    let mut empty = tool("mcp:nobody/ok", Effect::Read, "ok");
    empty.bots = Some(Vec::new());
    let empty = Arc::new(empty);
    let natives: Vec<Arc<TestTool>> = (0..4)
        .map(|i| Arc::new(tool(&format!("t.extra{i}"), Effect::Read, "x")))
        .collect();
    let mut all = vec![&scoped, &open, &listed, &empty];
    all.extend(natives.iter());
    let fake = Fake::new(vec![
        calls(&[("c1", "find_tool", json!({"query": "mcp tool", "k": 25}))]),
        calls(&[("c2", "get_tool_details", json!({"name": "mcp:srv/secret"}))]),
        calls(&[(
            "c3",
            "use_tool",
            json!({"name": "mcp:srv/secret", "input": {}}),
        )]),
        text("done"),
    ]);
    let mut config = cfg(Policy::full());
    config.discovery_threshold = 3;
    let lp = make_loop(&env.storage, env.bot, &fake, &all, profile(""), config);
    let run = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);

    let found = &tool_messages(&fake.request(1))[0];
    assert!(found.contains("mcp:open/ok"), "{found}");
    assert!(found.contains("mcp:mine/ok"), "{found}");
    assert!(!found.contains("mcp:srv/secret"), "leaked: {found}");
    assert!(
        !found.contains("mcp:nobody/ok"),
        "empty list = no bot: {found}"
    );
    let details = &tool_messages(&fake.request(2))[1];
    assert!(details.contains("unknown tool"), "{details}");
    let used = &tool_messages(&fake.request(3))[2];
    assert!(used.contains("unknown tool"), "{used}");
    assert_eq!(scoped.count.load(Ordering::SeqCst), 0);

    // The router classifier never sees names / descriptions of tools this bot
    // may not use; a spec the registry does not know is not visible at all.
    let cands: Vec<String> = lp
        .sh
        .router_candidates(&profile(""), env.bot)
        .into_iter()
        .map(|c| c.name)
        .collect();
    assert!(cands.contains(&"mcp:mine/ok".to_owned()), "{cands:?}");
    assert!(cands.contains(&"mcp:open/ok".to_owned()), "{cands:?}");
    assert!(!cands.contains(&"mcp:srv/secret".to_owned()), "{cands:?}");
    assert!(!cands.contains(&"mcp:nobody/ok".to_owned()), "{cands:?}");
    let ghost = tool("mcp:ghost/x", Effect::Read, "g").spec;
    assert!(!lp.sh.spec_visible(&profile(""), env.bot, &ghost));
}

#[tokio::test]
async fn bot_scoped_tool_is_not_listed_directly_below_the_discovery_threshold() {
    let env = setup();
    let mut scoped = tool("n_secret", Effect::Read, "s");
    scoped.bots = Some(vec!["some-other-bot".into()]);
    let scoped = Arc::new(scoped);
    let mut mine = tool("n_mine", Effect::Read, "m");
    mine.bots = Some(vec![env.bot.to_string()]);
    let mine = Arc::new(mine);
    let fake = Fake::new(vec![calls(&[("c1", "n_secret", json!({}))]), text("done")]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[&scoped, &mine],
        profile(""),
        cfg(Policy::full()),
    );
    let run = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    let names = req_tool_names(&fake.request(0));
    assert!(names.iter().any(|n| n.contains("mine")), "{names:?}");
    assert!(!names.iter().any(|n| n.contains("secret")), "{names:?}");
    assert!(tool_messages(&fake.request(1))[0].contains("unknown tool"));
    assert_eq!(scoped.count.load(Ordering::SeqCst), 0);
}

/// `mcp:*` (the wizard's General preset) makes MCP tools *reachable*, not
/// *prompt-free*: an MCP tool still raises an approval by default.  (The
/// preset list itself is guarded by `apps/cli` tests.)
#[tokio::test]
async fn mcp_glob_exposes_mcp_tools_but_they_still_ask() {
    let env = setup();
    let mut mcp = tool("mcp:srv/do_it", Effect::Destructive, "mcp done");
    mcp.spec.category = PermissionCategory::Accounts;
    let mcp = Arc::new(mcp);
    let read = Arc::new(tool("fs_read", Effect::Read, "x"));
    let fake = Fake::new(vec![
        calls(&[(
            "c1",
            "use_tool",
            json!({"name": "mcp:srv/do_it", "input": {}}),
        )]),
        text("done"),
    ]);
    let prof = profile(
        "[tools]
allow = [\"ask_user\", \"find_tool\", \"get_tool_details\", \"use_tool\", \"fs_read\", \"mcp:*\"]
",
    );
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[&mcp, &read],
        prof,
        cfg(Policy::approve_destructive()),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let Event::ApprovalNeeded(a) = until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
    else {
        unreachable!()
    };
    assert_eq!(a.tool_name, "mcp:srv/do_it");
    assert_eq!(mcp.count.load(Ordering::SeqCst), 0);
    lp.approve(a.call_id, ApprovalScope::Once, env.user)
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(mcp.count.load(Ordering::SeqCst), 1);
}

/// The fail-closed `spec_visible` must not hide anything an ordinary bot may
/// use: with the whole built-in set registered, the bot sees exactly the
/// registry's tools (platform permitting), the discovery meta-tools included.
#[tokio::test]
async fn fail_closed_visibility_hides_no_registered_tool_from_an_ordinary_bot() {
    let env = setup();
    let fake = Fake::new(vec![text("x")]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[],
        profile(""),
        cfg(Policy::full()),
    )
    .with_builtin_tools(builtin_cfg(&env, Arc::new(NoGate)));
    let registered: std::collections::BTreeSet<String> = lp
        .sh
        .tools
        .list()
        .into_iter()
        .filter(|s| crate::shared::platform_ok(&s.platforms))
        .map(|s| s.name)
        .collect();
    assert!(registered.len() > 20, "{registered:?}");
    let visible: std::collections::BTreeSet<String> = lp
        .sh
        .visible_specs(&profile(""), env.bot)
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(visible, registered);
    // And through the model-facing surface: nothing it offers is unregistered.
    let offered = lp.sh.model_specs(&profile(""), env.bot, &[]);
    assert!(!offered.is_empty());
    for spec in offered {
        assert!(registered.contains(&spec.name), "{}", spec.name);
    }
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
        crate::PolicyDecision::ask()
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
        (crate::PolicyDecision::ask(), false),
        "the money floor is never waived"
    );
    assert_eq!(
        eff.categories[&PermissionCategory::Shell].decision,
        crate::PolicyDecision::ask()
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

fn desk_req(run_id: String) -> builtin_tools::ControlRequest {
    builtin_tools::ControlRequest {
        run_id,
        rect: None,
        label: "Save".into(),
        action: "click".into(),
    }
}

#[tokio::test]
async fn control_bridge_fails_closed_for_unknown_runs_and_a_dropped_loop() {
    use builtin_tools::DesktopControlGate as _;
    let env = setup();
    let fake = Fake::new(vec![]);
    let lp = gate_loop(&env, &fake);
    lp.set_control_gate(Arc::new(TestGate {
        refuse: false,
        seen: Mutex::new(Vec::new()),
    }));
    let bridge = crate::control::LoopControl {
        sh: Arc::downgrade(&lp.sh),
    };
    // Unknown (never live) run id and an unparsable id both refuse.
    let err = bridge
        .before_action(&desk_req(uuid::Uuid::now_v7().to_string()))
        .await
        .unwrap_err();
    assert!(err.contains("not active"), "{err}");
    assert!(
        bridge
            .before_action(&desk_req("nope".into()))
            .await
            .is_err()
    );
    // Dropped loop refuses too.
    drop(lp);
    assert!(
        bridge
            .before_action(&desk_req(uuid::Uuid::now_v7().to_string()))
            .await
            .is_err()
    );
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

#[tokio::test]
async fn locked_policy_denies_everything_even_under_a_permissive_policy() {
    // SEC-13: the locked deny-all mode ignores global / bot allowances.
    let env = setup();
    let w = Arc::new(tool("t.write", Effect::Write, "written"));
    let fake = Fake::new(vec![calls(&[("c1", "t.write", json!({}))]), text("done")]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&w)],
        profile(""),
        cfg(Policy::full()),
    );
    lp.set_policy_locked(true);
    let run = lp.run_turn(env.chat, "go").unwrap();
    let _ = lp.wait(run).await;
    assert_eq!(w.count.load(Ordering::SeqCst), 0, "denied while locked");
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

// ---------- SEC-01 / SEC-09 / REL-20 / REL-21 ----------

fn fs_limits() -> Policy {
    let mut policy = Policy::full();
    policy.categories.insert(
        PermissionCategory::Fs,
        Rule::new(Level::AllowWithLimits(Limits {
            max_per_hour: Some(1),
            max_amount_minor: None,
            path_patterns: vec!["workspace/**".into()],
        })),
    );
    policy
}

fn shell_deny() -> Policy {
    let mut policy = Policy::approve_destructive();
    policy
        .categories
        .insert(PermissionCategory::Shell, Rule::new(Level::Deny));
    policy
}

#[tokio::test]
async fn allow_with_limits_read_still_parks_even_with_read_shortcut() {
    let env = setup();
    let read = Arc::new(tool("fs_read", Effect::Read, "contents"));
    let fake = Fake::new(vec![calls(&[("c1", "fs_read", json!({}))]), text("done")]);
    let mut config = cfg(fs_limits());
    config.read_tools_skip_ask = true;
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&read)],
        profile(""),
        config,
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "read it").unwrap();
    let Event::ApprovalNeeded(a) = until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
    else {
        unreachable!()
    };
    assert_eq!(a.tool_name, "fs_read");
    assert_eq!(
        read.count.load(Ordering::SeqCst),
        0,
        "limits are not waived"
    );
    lp.approve(a.call_id, ApprovalScope::Once, env.user)
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(read.count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn allow_with_limits_read_still_parks_when_a_grant_exists() {
    let env = setup();
    let read = Arc::new(tool("fs_read", Effect::Read, "contents"));
    let fake = Fake::new(vec![calls(&[("c1", "fs_read", json!({}))]), text("done")]);
    let mut config = cfg(fs_limits());
    config.read_tools_skip_ask = true;
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&read)],
        profile(""),
        config,
    );
    lp.sh.grants.lock().always.push(crate::Grant {
        bot_id: env.bot,
        target: GrantTarget::Tool("fs_read".into()),
        granted_at_ms: 1,
    });
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "read it").unwrap();
    until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await;
    assert_eq!(
        read.count.load(Ordering::SeqCst),
        0,
        "an existing grant must not waive unsupported limits"
    );
    lp.cancel(run).unwrap();
}

#[tokio::test]
async fn approving_a_shell_call_after_shell_is_denied_does_not_run_or_grant() {
    let env = setup();
    let mut shell = tool("shell_run", Effect::Write, "ran");
    shell.spec.category = PermissionCategory::Shell;
    let shell = Arc::new(shell);
    let fake = Fake::new(vec![
        calls(&[("c1", "shell_run", json!({"command": "echo hi"}))]),
        text("stopped"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&shell)],
        profile(""),
        cfg(Policy::approve_destructive()),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let Event::ApprovalNeeded(a) = until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
    else {
        unreachable!()
    };
    assert_eq!(shell.count.load(Ordering::SeqCst), 0);
    lp.set_global_policy(shell_deny());
    let _ = lp.approve(a.call_id, ApprovalScope::Always, env.user);
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(
        shell.count.load(Ordering::SeqCst),
        0,
        "the command must not run"
    );
    assert!(
        lp.list_grants(env.bot).is_empty(),
        "a deny must not create a grant"
    );
    let msg = tool_messages(&fake.request(1)).join("\n");
    assert!(
        msg.contains("denied by policy"),
        "the model should see the policy denial, got {msg}"
    );
}

#[tokio::test]
async fn retry_after_beyond_budget_is_one_request_and_surfaces_rate_limited() {
    let env = setup();
    let fake = Fake::new(vec![
        Reply::RateLimited(Some(Duration::from_secs(60))),
        text("should not be asked"),
    ]);
    let mut config = cfg(Policy::full());
    // More than one attempt, with a budget far below 60s, so the old loop
    // would send again after a short backoff.
    config.model_retry.max_attempts = 3;
    config.model_retry.initial_backoff_ms = 500;
    config.model_retry.multiplier = 2;
    config.model_retry.max_backoff_ms = 8_000;
    let lp = make_loop(&env.storage, env.bot, &fake, &[], profile(""), config);
    let run = lp.run_turn(env.chat, "hi").unwrap();
    let state = tokio::time::timeout(Duration::from_secs(3), lp.wait(run))
        .await
        .expect("must not sleep for Retry-After: 60")
        .unwrap();
    assert_eq!(state, RunState::Failed);
    assert_eq!(
        fake.n_requests(),
        1,
        "no second request after a 60s Retry-After"
    );
    let err = env.storage.get_run(run).unwrap().unwrap().error.unwrap();
    assert!(err.contains("rate limited"), "{err}");
}

#[tokio::test]
async fn retry_after_within_budget_waits_then_retries_once() {
    let env = setup();
    let fake = Fake::new(vec![
        Reply::RateLimited(Some(Duration::from_millis(200))),
        text("recovered"),
    ]);
    let mut config = cfg(Policy::full());
    config.model_retry.max_attempts = 2;
    // The outer backoff is much longer than Retry-After. Honouring the header
    // finishes well inside 3s; using the backoff does not.
    config.model_retry.initial_backoff_ms = 5_000;
    config.model_retry.multiplier = 1;
    config.model_retry.max_backoff_ms = 5_000;
    let lp = make_loop(&env.storage, env.bot, &fake, &[], profile(""), config);
    let started = std::time::Instant::now();
    let run = lp.run_turn(env.chat, "hi").unwrap();
    let state = tokio::time::timeout(Duration::from_secs(3), lp.wait(run))
        .await
        .expect("must wait Retry-After, not the 5s backoff")
        .unwrap();
    let elapsed = started.elapsed();
    assert_eq!(state, RunState::Succeeded);
    assert_eq!(fake.n_requests(), 2);
    assert!(
        elapsed >= Duration::from_millis(150),
        "Retry-After was not waited, elapsed {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(3),
        "waited longer than Retry-After, elapsed {elapsed:?}"
    );
}

#[tokio::test]
async fn finished_runs_leave_the_task_map_and_wait_still_returns_state() {
    let env = setup();
    let fake = Fake::new(vec![text("a"), text("b"), text("c")]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[],
        profile(""),
        cfg(Policy::full()),
    );
    let mut ids = Vec::new();
    for msg in ["a", "b", "c"] {
        let run = lp.run_turn(env.chat, msg).unwrap();
        let mut reaped = false;
        for _ in 0..200 {
            let done = lp.runtime().state(run).ok() == Some(RunState::Succeeded);
            let gone = !lp.sh.tasks.lock().contains_key(&run);
            if done && gone {
                reaped = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            reaped,
            "run {run} finished but its task handle is still tracked"
        );
        ids.push(run);
    }
    assert!(
        lp.sh.tasks.lock().is_empty(),
        "finished handles must not accumulate"
    );
    for run in ids {
        assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    }
}

#[tokio::test]
async fn finishing_task_removes_only_its_own_handle_and_resume_refuses_while_it_remains() {
    let env = setup();
    let w = Arc::new(tool("t.write", Effect::Write, "written"));
    let fake = Fake::new(vec![calls(&[("c1", "t.write", json!({}))]), text("never")]);
    let mut config = cfg(Policy::approve_destructive());
    config.schedule_approval_timeout = Duration::from_millis(80);
    let lp = make_loop(&env.storage, env.bot, &fake, &[t(&w)], profile(""), config);
    let barrier = Arc::new(Barrier::new(2));
    *lp.sh.task_reap_gate.lock() = Some(Arc::clone(&barrier));
    let run = lp
        .run_scheduled(
            env.chat,
            protocol::TaskId::new(),
            crate::ScheduleWork::Prompt("go".into()),
        )
        .unwrap();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let paused = lp.runtime().state(run).ok() == Some(RunState::Paused);
        let entered = lp.sh.task_reap_entered.load(Ordering::SeqCst);
        if paused && entered {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "drive task never parked at the reap gate (state={:?})",
            lp.runtime().state(run)
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let id_before = lp
        .sh
        .tasks
        .lock()
        .get(&run)
        .map(tokio::task::JoinHandle::id)
        .expect("finishing task still holds the map entry");
    let err = lp.resume(run).unwrap_err();
    assert!(
        matches!(err, crate::LoopError::StillFinishing(id) if id == run),
        "{err}"
    );
    assert_eq!(
        lp.sh
            .tasks
            .lock()
            .get(&run)
            .map(tokio::task::JoinHandle::id),
        Some(id_before),
        "resume must not replace the live handle"
    );
    assert!(
        !lp.sh.active.lock().contains_key(&env.chat),
        "a refused resume must not keep the chat"
    );

    // A newer handle for the same run (what a racy insert would store). The
    // finishing task must not remove it: dropping that JoinHandle detaches
    // the resumed driver.
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let dummy = tokio::spawn(async move {
        let _ = rx.await;
    });
    let dummy_id = dummy.id();
    {
        let mut tasks = lp.sh.tasks.lock();
        let previous = tasks.insert(run, dummy);
        drop(previous);
    }
    barrier.wait().await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !lp.sh.task_reap_done.load(Ordering::SeqCst) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "finishing task did not reach its remove"
        );
        tokio::task::yield_now().await;
    }
    let kept = lp
        .sh
        .tasks
        .lock()
        .remove(&run)
        .expect("reap removed a different task's handle");
    assert_eq!(kept.id(), dummy_id);
    assert!(!kept.is_finished(), "the newer task must still be joinable");
    let _ = tx.send(());
    let _ = kept.await;
    *lp.sh.task_reap_gate.lock() = None;
}

async fn until_flag(flag: &std::sync::atomic::AtomicBool, label: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !flag.load(Ordering::SeqCst) {
        assert!(tokio::time::Instant::now() < deadline, "{label}");
        tokio::task::yield_now().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn policy_deny_after_approval_recheck_does_not_run_the_tool_or_keep_the_grant() {
    use std::sync::atomic::AtomicBool;
    use std::sync::{Condvar, Mutex as StdMutex};

    let env = setup();
    let mut shell = tool("shell_run", Effect::Write, "ran");
    shell.spec.category = PermissionCategory::Shell;
    let shell = Arc::new(shell);
    let fake = Fake::new(vec![
        calls(&[("c1", "shell_run", json!({"command": "echo hi"}))]),
        text("stopped"),
    ]);
    let lp = make_loop(
        &env.storage,
        env.bot,
        &fake,
        &[t(&shell)],
        profile(""),
        cfg(Policy::approve_destructive()),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let Event::ApprovalNeeded(a) = until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
    else {
        unreachable!()
    };

    let entered = Arc::new(AtomicBool::new(false));
    let release = Arc::new((StdMutex::new(false), Condvar::new()));
    let entered_h = Arc::clone(&entered);
    let release_h = Arc::clone(&release);
    *lp.sh.exec_commit_hook.lock() = Some(Arc::new(move |sh| {
        entered_h.store(true, Ordering::SeqCst);
        let (lock, cv) = &*release_h;
        let mut go = lock.lock().unwrap();
        while !*go {
            go = cv.wait(go).unwrap();
        }
        drop(go);
        // The policy moves to deny while the pre-tool check still holds
        // `exec_gate`, before `decide` runs again.
        crate::approval::install_global_policy(sh, shell_deny());
    }));
    lp.approve(a.call_id, ApprovalScope::Always, env.user)
        .unwrap();

    until_flag(&entered, "pre-tool commit hook did not run").await;
    assert!(
        lp.sh.exec_gate.try_lock().is_none(),
        "the pre-tool recheck must hold exec_gate"
    );

    let saw_busy = Arc::new(AtomicBool::new(false));
    let finished = Arc::new(AtomicBool::new(false));
    let saw_busy_t = Arc::clone(&saw_busy);
    let finished_t = Arc::clone(&finished);
    let lp_t = lp.clone();
    std::thread::spawn(move || {
        loop {
            if lp_t.sh.exec_gate.try_lock().is_none() {
                saw_busy_t.store(true, Ordering::SeqCst);
                break;
            }
            std::thread::yield_now();
        }
        lp_t.set_global_policy(shell_deny());
        finished_t.store(true, Ordering::SeqCst);
    });
    until_flag(&saw_busy, "observer never saw exec_gate held").await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !finished.load(Ordering::SeqCst),
        "set_global_policy must block on the exec gate until the pre-tool recheck finishes"
    );

    {
        let (lock, cv) = &*release;
        *lock.lock().unwrap() = true;
        cv.notify_one();
    }
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(
        shell.count.load(Ordering::SeqCst),
        0,
        "the command must not run"
    );
    assert!(
        lp.list_grants(env.bot).is_empty(),
        "a deny must not keep a grant"
    );
    let msg = tool_messages(&fake.request(1)).join("\n");
    assert!(
        msg.contains("denied by policy"),
        "the model should see the policy denial, got {msg}"
    );
    until_flag(
        &finished,
        "set_global_policy did not finish after the gate was released",
    )
    .await;
}

// ---------- always-ask tools (schedule delegation) ----------

/// A tool like `schedule_create`: category `System`, write effect, confirmed
/// on every call, with approval-card details (or a veto).
struct AskTool {
    spec: ToolSpec,
    count: Arc<AtomicUsize>,
    veto: Option<&'static str>,
    /// Times `approval_closed` ran (approval ended without the call running).
    closed: Arc<AtomicUsize>,
    /// The `approval_ref` tokens `approval_closed` received.
    tokens: Arc<Mutex<Vec<Option<String>>>>,
    /// Offer details at all (an always-ask tool that does not is refused).
    with_details: bool,
}

fn ask_tool(veto: Option<&'static str>) -> Arc<AskTool> {
    Arc::new(AskTool {
        spec: ToolSpec {
            name: "sched_make".into(),
            description: "test schedule tool".into(),
            input_schema: json!({"type": "object"}),
            effect: Effect::Write,
            category: PermissionCategory::System,
            platforms: vec![Platform::All],
        },
        count: Arc::new(AtomicUsize::new(0)),
        veto,
        closed: Arc::new(AtomicUsize::new(0)),
        tokens: Arc::new(Mutex::new(Vec::new())),
        with_details: true,
    })
}

#[async_trait]
impl Tool for AskTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }
    fn always_ask(&self) -> bool {
        true
    }
    async fn approval_details(
        &self,
        ctx: &ToolContext,
        input: &Value,
    ) -> Result<Option<Value>, ToolError> {
        if let Some(msg) = self.veto {
            return Err(ToolError::Execution(msg.into()));
        }
        if !self.with_details {
            return Ok(None);
        }
        Ok(Some(json!({
            "kind": "schedule",
            "prompt": input["prompt"],
            "bot_id": ctx.bot_id,
            "next_runs": ["a", "b", "c"],
            "approval_ref": "tok-xyz",
        })))
    }
    fn approval_closed(&self, _: &ToolContext, _: &Value, token: Option<&str>) {
        self.closed.fetch_add(1, Ordering::SeqCst);
        self.tokens.lock().push(token.map(str::to_owned));
    }
    async fn call(&self, _: &ToolContext, _: Value) -> Result<ToolOutput, ToolError> {
        self.count.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput::text("scheduled"))
    }
}

fn ask_loop(env: &Env, fake: &Arc<Fake>, tool: &Arc<AskTool>, policy: Policy) -> AgentLoop {
    let registry = ToolRegistry::new();
    registry.register(Arc::clone(tool) as Arc<dyn Tool>);
    let lp = AgentLoop::new(
        Arc::clone(&env.storage),
        registry,
        Providers::single(Arc::clone(fake) as Arc<dyn ChatProvider>),
        cfg(policy),
    );
    lp.add_profile(env.bot, profile(""));
    lp
}

#[tokio::test]
async fn always_ask_tool_asks_even_when_policy_allows() {
    let env = setup();
    let tool = ask_tool(None);
    let fake = Fake::new(vec![
        calls(&[("c1", "sched_make", json!({"prompt": "do x"}))]),
        text("done"),
    ]);
    let lp = ask_loop(&env, &fake, &tool, Policy::full());
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let Event::ApprovalNeeded(a) = until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
    else {
        unreachable!()
    };
    assert_eq!(
        tool.count.load(Ordering::SeqCst),
        0,
        "not run before approval"
    );
    lp.approve(a.call_id, ApprovalScope::Once, env.user)
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(tool.count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn always_ask_tool_ignores_tool_category_and_chat_grants() {
    let env = setup();
    let tool = ask_tool(None);
    let fake = Fake::new(vec![
        calls(&[("c1", "sched_make", json!({"prompt": "p"}))]),
        text("done"),
    ]);
    let lp = ask_loop(&env, &fake, &tool, Policy::approve_destructive());
    {
        let mut g = lp.sh.grants.lock();
        for target in [
            GrantTarget::Tool("sched_make".into()),
            GrantTarget::Category(PermissionCategory::System),
        ] {
            g.always.push(crate::Grant {
                bot_id: env.bot,
                target,
                granted_at_ms: 1,
            });
        }
        g.this_chat.insert((env.chat, "sched_make".into()));
    }
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let Event::ApprovalNeeded(_) = until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
    else {
        unreachable!("grants must not silently cover an always-ask tool")
    };
    assert_eq!(tool.count.load(Ordering::SeqCst), 0);
    lp.cancel(run).unwrap();
}

#[tokio::test]
async fn approving_an_always_ask_tool_with_always_mints_no_grant() {
    let env = setup();
    let tool = ask_tool(None);
    let fake = Fake::new(vec![
        calls(&[("c1", "sched_make", json!({"prompt": "p"}))]),
        calls(&[("c2", "sched_make", json!({"prompt": "p"}))]),
        text("done"),
    ]);
    let lp = ask_loop(&env, &fake, &tool, Policy::approve_destructive());
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    for scope in [ApprovalScope::Always, ApprovalScope::AlwaysCategory] {
        let Event::ApprovalNeeded(a) =
            until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
        else {
            unreachable!()
        };
        assert!(lp.approve(a.call_id, scope, env.user).is_err());
        lp.approve(a.call_id, ApprovalScope::Once, env.user)
            .unwrap();
    }
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(
        tool.count.load(Ordering::SeqCst),
        2,
        "second call asked again"
    );
    assert!(lp.list_grants(env.bot).is_empty(), "no grant is recorded");
    assert!(lp.sh.grants.lock().this_chat.is_empty());
}

#[tokio::test]
async fn always_ask_tool_is_still_denied_by_a_deny_policy() {
    let env = setup();
    let tool = ask_tool(None);
    let fake = Fake::new(vec![
        calls(&[("c1", "sched_make", json!({"prompt": "p"}))]),
        text("ok"),
    ]);
    let mut policy = Policy::approve_destructive();
    policy
        .categories
        .insert(PermissionCategory::System, Rule::new(Level::Deny));
    let lp = ask_loop(&env, &fake, &tool, policy);
    let run = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(tool.count.load(Ordering::SeqCst), 0);
    assert!(
        tool_messages(&fake.request(1))
            .join("\n")
            .contains("Permission denied")
    );
}

#[tokio::test]
async fn approval_event_carries_tool_details_and_bot_permission() {
    let env = setup();
    let tool = ask_tool(None);
    let fake = Fake::new(vec![
        calls(&[("c1", "sched_make", json!({"prompt": "water the plants"}))]),
        text("done"),
    ]);
    let lp = ask_loop(&env, &fake, &tool, Policy::approve_destructive());
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let Event::ApprovalNeeded(a) = until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
    else {
        unreachable!()
    };
    let d = a.details.clone().expect("details on the card");
    assert_eq!(d["kind"], "schedule");
    assert_eq!(d["prompt"], "water the plants");
    assert_eq!(d["bot_id"], env.bot.to_string(), "tool saw the calling bot");
    assert_eq!(d["permission"]["level"], "ask_first");
    assert_eq!(d["permission"]["ask"].as_array().unwrap().len(), 10);
    lp.cancel(run).unwrap();
}

#[tokio::test]
async fn a_vetoed_call_never_reaches_the_user() {
    let env = setup();
    let tool = ask_tool(Some("schedule limit reached (20)"));
    let fake = Fake::new(vec![
        calls(&[("c1", "sched_make", json!({"prompt": "p"}))]),
        text("ok"),
    ]);
    let lp = ask_loop(&env, &fake, &tool, Policy::approve_destructive());
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(tool.count.load(Ordering::SeqCst), 0);
    let msg = tool_messages(&fake.request(1)).join("\n");
    assert!(msg.contains("schedule limit reached (20)"), "{msg}");
    while let Ok(item) = rx.try_recv() {
        assert!(
            !matches!(item.envelope.event, Event::ApprovalNeeded(_)),
            "no approval card for a vetoed call"
        );
    }
}

#[tokio::test]
async fn an_always_ask_approval_cannot_be_given_as_always_on_any_channel() {
    let env = setup();
    let tool = ask_tool(None);
    let fake = Fake::new(vec![
        calls(&[("c1", "sched_make", json!({"prompt": "p"}))]),
        text("done"),
    ]);
    let lp = ask_loop(&env, &fake, &tool, Policy::approve_destructive());
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let Event::ApprovalNeeded(a) = until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
    else {
        unreachable!()
    };
    for scope in [
        ApprovalScope::Always,
        ApprovalScope::AlwaysCategory,
        ApprovalScope::ThisChat,
    ] {
        let e = lp.approve(a.call_id, scope, env.user).unwrap_err();
        assert!(matches!(e, crate::LoopError::OnceOnly(_)), "{scope:?}: {e}");
    }
    assert_eq!(tool.count.load(Ordering::SeqCst), 0, "still waiting");
    // The refusal did not consume the approval: once still works.
    lp.approve(a.call_id, ApprovalScope::Once, env.user)
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(tool.count.load(Ordering::SeqCst), 1);
    assert_eq!(
        tool.closed.load(Ordering::SeqCst),
        0,
        "approved: not closed"
    );
}

#[tokio::test]
async fn a_plain_approval_can_still_be_given_as_always() {
    let env = setup();
    let w = Arc::new(tool("fs_write", Effect::Write, "ok"));
    let fake = Fake::new(vec![calls(&[("c1", "fs_write", json!({}))]), text("done")]);
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
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
}

#[tokio::test]
async fn an_always_ask_tool_without_details_is_refused_not_asked() {
    let env = setup();
    let mut t = ask_tool(None);
    Arc::get_mut(&mut t).unwrap().with_details = false;
    let fake = Fake::new(vec![
        calls(&[("c1", "sched_make", json!({"prompt": "p"}))]),
        text("ok"),
    ]);
    let lp = ask_loop(&env, &fake, &t, Policy::approve_destructive());
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(t.count.load(Ordering::SeqCst), 0);
    while let Ok(item) = rx.try_recv() {
        assert!(!matches!(item.envelope.event, Event::ApprovalNeeded(_)));
    }
    assert!(
        tool_messages(&fake.request(1))
            .join("\n")
            .contains("approval details")
    );
}

#[tokio::test]
async fn denying_or_cancelling_an_approval_tells_the_tool_to_drop_its_preview() {
    let env = setup();
    let t = ask_tool(None);
    let fake = Fake::new(vec![
        calls(&[("c1", "sched_make", json!({"prompt": "p"}))]),
        text("ok"),
    ]);
    let lp = ask_loop(&env, &fake, &t, Policy::approve_destructive());
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let Event::ApprovalNeeded(a) = until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
    else {
        unreachable!()
    };
    lp.deny(a.call_id, env.user, None).unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(t.closed.load(Ordering::SeqCst), 1);
    assert_eq!(t.count.load(Ordering::SeqCst), 0);
    // A cancelled run closes it too.
    let t2 = ask_tool(None);
    let fake2 = Fake::new(vec![calls(&[("c1", "sched_make", json!({"prompt": "p"}))])]);
    let lp2 = ask_loop(&env, &fake2, &t2, Policy::approve_destructive());
    let mut rx2 = lp2.subscribe();
    let run2 = lp2.run_turn(env.chat, "go").unwrap();
    until(&mut rx2, |e| matches!(e, Event::ApprovalNeeded(_))).await;
    lp2.cancel(run2).unwrap();
    for _ in 0..200 {
        if t2.closed.load(Ordering::SeqCst) == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(t2.closed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn details_that_redaction_would_change_are_refused_before_asking() {
    let env = setup();
    let t = ask_tool(None);
    let secret = "sk-abcdefghijklmnopqrstuvwxyz0123";
    let fake = Fake::new(vec![
        calls(&[(
            "c1",
            "sched_make",
            json!({"prompt": format!("use key {secret}")}),
        )]),
        text("ok"),
    ]);
    let mut config = cfg(Policy::approve_destructive());
    config.redactor = Some(secret_store::Redactor::new().into());
    let registry = ToolRegistry::new();
    registry.register(Arc::clone(&t) as Arc<dyn Tool>);
    let lp = AgentLoop::new(
        Arc::clone(&env.storage),
        registry,
        Providers::single(Arc::clone(&fake) as Arc<dyn ChatProvider>),
        config,
    );
    lp.add_profile(env.bot, profile(""));
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(t.count.load(Ordering::SeqCst), 0);
    assert_eq!(t.closed.load(Ordering::SeqCst), 1, "preview dropped");
    let msg = tool_messages(&fake.request(1)).join("\n");
    assert!(msg.contains("looks like a secret"), "{msg}");
    assert!(!msg.contains(secret));
    while let Ok(item) = rx.try_recv() {
        assert!(!matches!(item.envelope.event, Event::ApprovalNeeded(_)));
    }
}

#[tokio::test]
async fn the_approval_ref_reaches_the_close_hook_but_never_the_card() {
    let env = setup();
    let t = ask_tool(None);
    let fake = Fake::new(vec![
        calls(&[("c1", "sched_make", json!({"prompt": "p"}))]),
        text("ok"),
    ]);
    let lp = ask_loop(&env, &fake, &t, Policy::approve_destructive());
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let Event::ApprovalNeeded(a) = until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
    else {
        unreachable!()
    };
    let d = a.details.unwrap();
    assert!(d.get("approval_ref").is_none(), "{d}");
    assert_eq!(d["kind"], "schedule");
    lp.deny(a.call_id, env.user, None).unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(*t.tokens.lock(), [Some("tok-xyz".to_owned())]);
}

#[tokio::test]
async fn an_approved_permit_that_never_executes_releases_the_tools_entry() {
    let env = setup();
    let t = ask_tool(None);
    let fake = Fake::new(vec![
        calls(&[("c1", "sched_make", json!({"prompt": "p"}))]),
        text("ok"),
    ]);
    let lp = ask_loop(&env, &fake, &t, Policy::approve_destructive());
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let Event::ApprovalNeeded(a) = until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
    else {
        unreachable!()
    };
    // Tighten the policy *inside* `ExecPermit::commit`, after `request_approval`
    // has already returned `Ok(ExecPermit)`: `commit` fails, dropping the permit
    // and its `ClosedGuard`, which must release the tool's frozen entry.
    let hook_ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let hook_ran_h = Arc::clone(&hook_ran);
    *lp.sh.exec_commit_hook.lock() = Some(Arc::new(move |sh| {
        hook_ran_h.store(true, Ordering::SeqCst);
        let mut deny = Policy::approve_destructive();
        deny.categories
            .insert(PermissionCategory::System, Rule::new(Level::Deny));
        crate::approval::install_global_policy(sh, deny);
    }));
    lp.approve(a.call_id, ApprovalScope::Once, env.user)
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert!(
        hook_ran.load(Ordering::SeqCst),
        "ExecPermit::commit must run"
    );
    assert_eq!(t.count.load(Ordering::SeqCst), 0, "never executed");
    assert_eq!(t.closed.load(Ordering::SeqCst), 1);
    assert_eq!(*t.tokens.lock(), [Some("tok-xyz".to_owned())]);
    let msg = tool_messages(&fake.request(1)).join("\n");
    assert!(msg.contains("denied by policy"), "{msg}");
}

#[tokio::test]
async fn an_executed_approval_does_not_call_the_close_hook() {
    let env = setup();
    let t = ask_tool(None);
    let fake = Fake::new(vec![
        calls(&[("c1", "sched_make", json!({"prompt": "p"}))]),
        text("ok"),
    ]);
    let lp = ask_loop(&env, &fake, &t, Policy::approve_destructive());
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    let Event::ApprovalNeeded(a) = until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
    else {
        unreachable!()
    };
    lp.approve(a.call_id, ApprovalScope::Once, env.user)
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(t.count.load(Ordering::SeqCst), 1);
    assert_eq!(t.closed.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_multiline_secret_in_the_details_is_refused_like_any_other() {
    let env = setup();
    let t = ask_tool(None);
    let pem = concat!(
        "-----BEGIN PRIVATE KEY-----\n",
        "MIIEvQIBADANBgkqhkiG9w0BAQEFAASC\n",
        "\"quoted\" \\ back\n",
        "-----END PRIVATE KEY-----"
    );
    let fake = Fake::new(vec![
        calls(&[(
            "c1",
            "sched_make",
            json!({"prompt": format!("deploy with {pem}")}),
        )]),
        text("ok"),
    ]);
    let mut redactor = secret_store::Redactor::new();
    redactor.add_secret(pem);
    let mut config = cfg(Policy::approve_destructive());
    config.redactor = Some(redactor.into());
    let registry = ToolRegistry::new();
    registry.register(Arc::clone(&t) as Arc<dyn Tool>);
    let lp = AgentLoop::new(
        Arc::clone(&env.storage),
        registry,
        Providers::single(Arc::clone(&fake) as Arc<dyn ChatProvider>),
        config,
    );
    lp.add_profile(env.bot, profile(""));
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(t.count.load(Ordering::SeqCst), 0);
    assert_eq!(t.closed.load(Ordering::SeqCst), 1);
    assert!(
        tool_messages(&fake.request(1))
            .join(
                "
"
            )
            .contains("looks like a secret")
    );
    while let Ok(item) = rx.try_recv() {
        assert!(!matches!(item.envelope.event, Event::ApprovalNeeded(_)));
    }
}
