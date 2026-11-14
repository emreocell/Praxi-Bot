//! Subagent, skills-wiring and catalog-price tests, driven by a request-aware
//! fake provider (children run concurrently, so a fixed reply queue would not
//! be deterministic).

#![allow(
    clippy::assert_is_empty,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::unnecessary_wraps
)]

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use parking_lot::Mutex;
use permission_engine::Policy;
use protocol::{Event, RunId};
use provider_catalog::{
    ChatChunk, ChatProvider, ChatRequest, FinishReason, MessageRole, ModelInfo, ProviderError,
    ToolCall, Usage,
};
use serde_json::{Value, json};
use storage::{Storage, read_event_log};
use task_runtime::RunState;
use tokio::sync::{Barrier, broadcast};
use tool_registry::{ContentClass, Effect, PermissionCategory, Tool, ToolRegistry};

use crate::tests::{Env, cfg, profile, setup, tool, until, with_class};
use crate::{
    AgentLoop, ApprovalScope, BotProfile, LoopConfig, ModelPrice, Providers, SkillHints, StreamItem,
};

// ---------- a provider that decides from the request ----------

type Decide = Box<dyn Fn(&ChatRequest) -> Option<Vec<ChatChunk>> + Send + Sync>;

struct Brain {
    decide: Decide,
    requests: Mutex<Vec<ChatRequest>>,
    /// Every request of a subagent waits here (proves they run concurrently).
    child_barrier: Option<Arc<Barrier>>,
}

impl Brain {
    fn new(
        decide: impl Fn(&ChatRequest) -> Option<Vec<ChatChunk>> + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            decide: Box::new(decide),
            requests: Mutex::new(Vec::new()),
            child_barrier: None,
        })
    }

    fn with_barrier(
        n: usize,
        decide: impl Fn(&ChatRequest) -> Option<Vec<ChatChunk>> + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            decide: Box::new(decide),
            requests: Mutex::new(Vec::new()),
            child_barrier: Some(Arc::new(Barrier::new(n))),
        })
    }

    fn requests(&self) -> Vec<ChatRequest> {
        self.requests.lock().clone()
    }
}

#[async_trait]
impl ChatProvider for Brain {
    fn id(&self) -> &'static str {
        "fake"
    }
    async fn stream(
        &self,
        req: ChatRequest,
    ) -> Result<BoxStream<'static, Result<ChatChunk, ProviderError>>, ProviderError> {
        self.requests.lock().push(req.clone());
        if is_child(&req)
            && let Some(b) = &self.child_barrier
        {
            tokio::time::timeout(Duration::from_secs(10), b.wait())
                .await
                .expect("subagents did not run concurrently");
        }
        Ok(match (self.decide)(&req) {
            Some(chunks) => futures::stream::iter(chunks.into_iter().map(Ok)).boxed(),
            None => futures::stream::pending().boxed(),
        })
    }
}

fn system_of(req: &ChatRequest) -> String {
    req.messages
        .iter()
        .find(|m| m.role == MessageRole::System)
        .and_then(|m| m.content.as_str())
        .unwrap_or_default()
        .to_owned()
}

fn is_child(req: &ChatRequest) -> bool {
    system_of(req).contains("You are a subagent")
}

fn last_user(req: &ChatRequest) -> String {
    req.messages
        .iter()
        .rev()
        .find(|m| m.role == MessageRole::User)
        .and_then(|m| m.content.as_str())
        .unwrap_or_default()
        .to_owned()
}

fn results(req: &ChatRequest) -> Vec<String> {
    req.messages
        .iter()
        .filter(|m| m.role == MessageRole::Tool)
        .map(|m| m.content.as_str().unwrap_or_default().to_owned())
        .collect()
}

fn tool_names(req: &ChatRequest) -> Vec<String> {
    req.tools
        .iter()
        .filter_map(|t| t["function"]["name"].as_str().map(str::to_owned))
        .collect()
}

fn say(s: &str) -> Option<Vec<ChatChunk>> {
    Some(vec![
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

fn call(cs: &[(&str, &str, Value)]) -> Option<Vec<ChatChunk>> {
    let mut v: Vec<ChatChunk> = cs
        .iter()
        .map(|(id, name, args)| {
            ChatChunk::ToolCall(ToolCall {
                id: (*id).to_owned(),
                name: name.replace('.', "__"),
                arguments: args.to_string(),
                provider_extra: None,
            })
        })
        .collect();
    v.push(ChatChunk::Done {
        finish_reason: FinishReason::ToolCalls,
    });
    Some(v)
}

fn spawn(id: &str, task: &str) -> (String, String, Value) {
    (
        id.to_owned(),
        "spawn_subagent".to_owned(),
        json!({ "task": task }),
    )
}

fn calls_owned(cs: Vec<(String, String, Value)>) -> Option<Vec<ChatChunk>> {
    let refs: Vec<(&str, &str, Value)> = cs
        .iter()
        .map(|(a, b, c)| (a.as_str(), b.as_str(), c.clone()))
        .collect();
    call(&refs)
}

fn make(
    storage: &Arc<Storage>,
    bot: protocol::BotId,
    brain: &Arc<Brain>,
    tools: &[Arc<dyn Tool>],
    prof: BotProfile,
    config: LoopConfig,
) -> AgentLoop {
    let registry = ToolRegistry::new();
    for t in tools {
        registry.register(Arc::clone(t));
    }
    let lp = AgentLoop::new(
        Arc::clone(storage),
        registry,
        Providers::single(Arc::clone(brain) as Arc<dyn ChatProvider>),
        config,
    )
    .with_subagents();
    lp.add_profile(bot, prof);
    lp
}

fn drain(rx: &mut broadcast::Receiver<StreamItem>) -> Vec<StreamItem> {
    let mut v = Vec::new();
    while let Ok(i) = rx.try_recv() {
        v.push(i);
    }
    v
}

fn children(env: &Env, parent: RunId) -> Vec<storage::Run> {
    env.storage.list_child_runs(parent).unwrap()
}

// ---------- tests ----------

#[tokio::test]
async fn a_child_run_works_end_to_end() {
    let env = setup();
    let brain = Brain::new(|req| {
        if is_child(req) {
            say("three")
        } else if results(req).is_empty() {
            call(&[(
                "c1",
                "spawn_subagent",
                json!({"task": "count to three", "context": "the number is 3"}),
            )])
        } else {
            say(&format!("parent saw: {}", results(req)[0]))
        }
    });
    let lp = make(
        &env.storage,
        env.bot,
        &brain,
        &[],
        profile(""),
        cfg(Policy::full()),
    );
    let mut rx = lp.subscribe();
    let parent = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(parent).await.unwrap(), RunState::Succeeded);

    // The parent got the child's final text as the tool result.
    let reqs = brain.requests();
    let parent_last = reqs.iter().rfind(|r| !is_child(r)).unwrap();
    assert_eq!(results(parent_last), ["three"]);

    // One persisted child run, linked to the parent, succeeded.
    let kids = children(&env, parent);
    assert_eq!(kids.len(), 1);
    let child = &kids[0];
    assert_eq!(child.parent_run_id, Some(parent));
    assert_eq!(child.kind, "turn:subagent");
    assert_eq!(child.state, "succeeded");
    assert_eq!(child.chat_id, Some(env.chat));

    // Fresh history: system + (task + parent context) only, and the subagent note.
    let creq = reqs.iter().find(|r| is_child(r)).unwrap();
    let non_system: Vec<_> = creq
        .messages
        .iter()
        .filter(|m| m.role != MessageRole::System)
        .collect();
    assert_eq!(non_system.len(), 1);
    let first = non_system[0].content.as_str().unwrap();
    assert!(first.starts_with("count to three"), "{first}");
    assert!(first.contains("the number is 3"), "{first}");

    // Events: started on the parent stream, the child's events carry the
    // parent id, no run_started / run_finished for the child, finished last.
    let items = drain(&mut rx);
    let started = items
        .iter()
        .position(
            |i| matches!(&i.envelope.event, Event::SubagentStarted(s) if s.run_id == child.id),
        )
        .expect("subagent_started");
    let Event::SubagentStarted(s) = &items[started].envelope.event else {
        unreachable!()
    };
    assert_eq!(s.parent_run_id, parent);
    assert_eq!(s.task, "count to three");
    assert!(s.call_id.is_some());
    assert_eq!(
        items[started].run_id, parent,
        "announced on the parent's stream"
    );
    let child_items: Vec<_> = items.iter().filter(|i| i.run_id == child.id).collect();
    assert!(!child_items.is_empty());
    assert!(
        child_items
            .iter()
            .all(|i| i.envelope.parent_run_id == Some(parent))
    );
    assert!(!child_items.iter().any(|i| matches!(
        i.envelope.event,
        Event::RunStarted(_) | Event::RunFinished(_)
    )));
    assert!(
        items
            .iter()
            .all(|i| i.run_id != parent || i.envelope.parent_run_id.is_none())
    );
    let fin = items
        .iter()
        .position(
            |i| matches!(&i.envelope.event, Event::SubagentFinished(f) if f.run_id == child.id),
        )
        .expect("subagent_finished");
    let Event::SubagentFinished(f) = &items[fin].envelope.event else {
        unreachable!()
    };
    assert_eq!(
        (f.state.as_str(), f.summary.as_str()),
        ("succeeded", "three")
    );
    assert!(started < fin);
    // The child's own log carries the parent id too.
    let log = read_event_log(env.storage.data_dir(), child.id).unwrap();
    assert!(!log.is_empty() && log.iter().all(|e| e.parent_run_id == Some(parent)));

    // The chat only holds the parent's exchange.
    let msgs = env.storage.list_messages(env.chat).unwrap();
    assert_eq!(msgs.len(), 2, "{msgs:?}");
    assert!(msgs.iter().all(|m| m.run_id == Some(parent)));
}

#[tokio::test]
async fn parallel_children_run_concurrently_up_to_three() {
    let env = setup();
    // Every child waits for two siblings: deadlocks unless all 3 run at once.
    let brain = Brain::with_barrier(3, |req| {
        if is_child(req) {
            say(&format!("done {}", last_user(req)))
        } else if results(req).is_empty() {
            calls_owned(vec![
                spawn("c1", "t1"),
                spawn("c2", "t2"),
                spawn("c3", "t3"),
                spawn("c4", "t4"),
            ])
        } else {
            say("finished")
        }
    });
    let lp = make(
        &env.storage,
        env.bot,
        &brain,
        &[],
        profile(""),
        cfg(Policy::full()),
    );
    let parent = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(parent).await.unwrap(), RunState::Succeeded);

    let reqs = brain.requests();
    let last = reqs.iter().rfind(|r| !is_child(r)).unwrap();
    let res = results(last);
    assert_eq!(res.len(), 4);
    assert_eq!(res[0], "done t1");
    assert_eq!(res[1], "done t2");
    assert_eq!(res[2], "done t3");
    assert!(res[3].contains("too many subagents"), "{res:?}");
    assert_eq!(children(&env, parent).len(), 3);
}

#[tokio::test]
async fn depth_is_limited_to_two() {
    let env = setup();
    let brain = Brain::new(|req| {
        let task = last_user(req);
        if let Some(r) = results(req).first() {
            return say(&format!("{task} got [{r}]"));
        }
        match task.as_str() {
            "go" => calls_owned(vec![spawn("a", "L1")]),
            "L1" => calls_owned(vec![spawn("b", "L2")]),
            "L2" => calls_owned(vec![spawn("c", "L3")]),
            _ => say("unexpected"),
        }
    });
    let lp = make(
        &env.storage,
        env.bot,
        &brain,
        &[],
        profile(""),
        cfg(Policy::full()),
    );
    let parent = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(parent).await.unwrap(), RunState::Succeeded);

    let _ = brain.requests();
    let final_text = &env
        .storage
        .list_messages(env.chat)
        .unwrap()
        .last()
        .unwrap()
        .content
        .clone();
    assert!(
        final_text.contains("L1 got [L2 got [Error: subagent depth limit"),
        "{final_text}"
    );

    // No run was created for the refused spawn: parent + child + grandchild.
    let child = &children(&env, parent)[0];
    assert_eq!(children(&env, child.id).len(), 1);
    assert_eq!(children(&env, children(&env, child.id)[0].id).len(), 0);
}

#[tokio::test]
async fn an_approval_inside_a_child_bubbles_up_with_the_child_run_id() {
    let env = setup();
    let danger = Arc::new(tool("danger", Effect::Write, "did it"));
    let brain = Brain::new(|req| {
        if is_child(req) {
            if results(req).is_empty() {
                call(&[("d1", "danger", json!({}))])
            } else {
                say("child done")
            }
        } else if results(req).is_empty() {
            calls_owned(vec![spawn("c1", "do the dangerous thing")])
        } else {
            say("parent done")
        }
    });
    let lp = make(
        &env.storage,
        env.bot,
        &brain,
        &[Arc::clone(&danger) as Arc<dyn Tool>],
        profile(""),
        cfg(Policy::approve_destructive()),
    );
    let mut rx = lp.subscribe();
    let parent = lp.run_turn(env.chat, "go").unwrap();
    let Event::ApprovalNeeded(a) = until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
    else {
        unreachable!()
    };
    assert_eq!(a.tool_name, "danger");
    let child = children(&env, parent)[0].id;
    assert_eq!(a.run_id, child, "approval belongs to the child run");
    assert_ne!(a.run_id, parent);
    assert_eq!(
        lp.runtime().state(child).unwrap(),
        RunState::WaitingApproval
    );
    assert_eq!(danger.count.load(Ordering::SeqCst), 0);

    lp.approve(a.call_id, ApprovalScope::Once, env.user)
        .unwrap();
    assert_eq!(lp.wait(parent).await.unwrap(), RunState::Succeeded);
    assert_eq!(danger.count.load(Ordering::SeqCst), 1);
    assert_eq!(
        env.storage.list_approvals(child).unwrap()[0]
            .decision
            .as_deref(),
        Some("approved")
    );
}

#[tokio::test]
async fn a_child_sees_the_intersection_of_the_parents_tools_and_the_requested_ones() {
    let env = setup();
    let (a, b, c) = (
        Arc::new(tool("t_a", Effect::Read, "a")),
        Arc::new(tool("t_b", Effect::Read, "b")),
        Arc::new(tool("t_c", Effect::Read, "c")),
    );
    let brain = Brain::new(|req| {
        if is_child(req) {
            say("ok")
        } else if results(req).is_empty() {
            call(&[(
                "c1",
                "spawn_subagent",
                // `t_c` is hidden from the parent, `nope` does not exist.
                json!({"task": "x", "tools": ["t_a", "t_c", "nope"]}),
            )])
        } else {
            say("fin")
        }
    });
    let tools: Vec<Arc<dyn Tool>> = vec![a, b, c];
    let lp = make(
        &env.storage,
        env.bot,
        &brain,
        &tools,
        profile("[tools]\nexclude = [\"t_c\"]\n"),
        cfg(Policy::full()),
    );
    let parent = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(parent).await.unwrap(), RunState::Succeeded);
    let reqs = brain.requests();
    let mut parent_tools = tool_names(reqs.iter().find(|r| !is_child(r)).unwrap());
    parent_tools.sort();
    assert_eq!(parent_tools, ["spawn_subagent", "t_a", "t_b"]);
    assert_eq!(
        tool_names(reqs.iter().find(|r| is_child(r)).unwrap()),
        ["t_a"]
    );
}

#[tokio::test]
async fn model_role_and_max_steps_shape_the_child() {
    let env = setup();
    let brain = Brain::new(|req| {
        if is_child(req) {
            // Never finishes on its own: keeps calling a tool.
            if results(req).len() < 10 {
                call(&[(
                    &format!("k{}", results(req).len()),
                    "t_x",
                    json!({"n": results(req).len()}),
                )])
            } else {
                say("closing summary")
            }
        } else if results(req).is_empty() {
            call(&[(
                "c1",
                "spawn_subagent",
                json!({"task": "loop", "model_role": "classifier", "max_steps": 2}),
            )])
        } else {
            say("fin")
        }
    });
    let x = Arc::new(tool("t_x", Effect::Read, "x")) as Arc<dyn Tool>;
    let lp = make(
        &env.storage,
        env.bot,
        &brain,
        &[x],
        // The role model exists for the child; the router's K3 would call it too.
        profile(
            "classifier = \"fake/small\"
[router]
k3 = false
",
        ),
        cfg(Policy::full()),
    );
    let parent = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(parent).await.unwrap(), RunState::Succeeded);
    let reqs = brain.requests();
    let child_reqs: Vec<_> = reqs.iter().filter(|r| is_child(r)).collect();
    // 2 model calls + the tool-less closing call after the step limit.
    assert_eq!(child_reqs.len(), 3);
    assert!(child_reqs.iter().all(|r| r.model.0 == "fake/small"));
    assert!(child_reqs[2].tools.is_empty());
    let kid = &children(&env, parent)[0];
    assert!(lp.stopped_by_guard(kid.id).unwrap());
    // The parent's own model is unchanged.
    assert!(
        reqs.iter()
            .filter(|r| !is_child(r))
            .all(|r| r.model.0 == "fake/m")
    );
}

#[tokio::test]
async fn a_childs_token_budget_is_a_quarter_of_the_parents_remainder() {
    let env = setup();
    let brain = Brain::new(|req| {
        if is_child(req) {
            say("ok")
        } else if results(req).is_empty() {
            calls_owned(vec![spawn("c1", "t")])
        } else {
            say("fin")
        }
    });
    let mut config = cfg(Policy::full());
    config.guards.max_tokens_per_turn = 4_000;
    let lp = make(&env.storage, env.bot, &brain, &[], profile(""), config);
    let parent = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(parent).await.unwrap(), RunState::Succeeded);
    let kid = &children(&env, parent)[0];
    // The parent had used no tokens when it spawned.
    assert_eq!(kid.spec.as_ref().unwrap()["token_budget"], 4_000 / 4);
    assert_eq!(kid.spec.as_ref().unwrap()["max_steps"], 15);
}

#[tokio::test]
async fn cancelling_the_parent_cancels_its_children() {
    let env = setup();
    let brain = Brain::new(|req| {
        if is_child(req) {
            None // hangs
        } else if results(req).is_empty() {
            calls_owned(vec![spawn("c1", "a"), spawn("c2", "b")])
        } else {
            say("fin")
        }
    });
    let lp = make(
        &env.storage,
        env.bot,
        &brain,
        &[],
        profile(""),
        cfg(Policy::full()),
    );
    let mut rx = lp.subscribe();
    let parent = lp.run_turn(env.chat, "go").unwrap();
    for _ in 0..500 {
        let kids = children(&env, parent);
        if kids.len() == 2 && brain.requests().iter().filter(|r| is_child(r)).count() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let kids: Vec<RunId> = children(&env, parent).iter().map(|k| k.id).collect();
    assert_eq!(kids.len(), 2);

    lp.cancel(parent).unwrap();
    assert_eq!(lp.wait(parent).await.unwrap(), RunState::Cancelled);
    for k in &kids {
        assert_eq!(lp.wait(*k).await.unwrap(), RunState::Cancelled, "child {k}");
    }
    let items = drain(&mut rx);
    for k in &kids {
        assert!(items.iter().any(|i| matches!(
            &i.envelope.event,
            Event::SubagentFinished(f) if f.run_id == *k && f.state == "cancelled"
        )));
    }
}

#[tokio::test]
async fn cancelling_a_single_child_hands_the_parent_an_error_and_it_carries_on() {
    let env = setup();
    let brain = Brain::new(|req| {
        if is_child(req) {
            None
        } else if results(req).is_empty() {
            calls_owned(vec![spawn("c1", "a")])
        } else {
            say(&format!("parent: {}", results(req)[0]))
        }
    });
    let lp = make(
        &env.storage,
        env.bot,
        &brain,
        &[],
        profile(""),
        cfg(Policy::full()),
    );
    let parent = lp.run_turn(env.chat, "go").unwrap();
    let kid = loop {
        if let Some(k) = children(&env, parent).first()
            && brain.requests().iter().any(is_child)
        {
            break k.id;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    lp.cancel(kid).unwrap();
    assert_eq!(lp.wait(parent).await.unwrap(), RunState::Succeeded);
    let reqs = brain.requests();
    let last = reqs.iter().rfind(|r| !is_child(r)).unwrap();
    assert!(
        results(last)[0].contains("cancelled"),
        "{:?}",
        results(last)
    );
}

fn rt2() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn crash_with_a_child_in_flight_then_resume_adopts_the_child() {
    let env = setup();
    let path = env.dir.path().to_path_buf();
    let bot = env.bot;
    let chat = env.chat;

    // Process 1: the child's model call hangs; everything is dropped ("crash").
    let rt1 = rt2();
    let parent = rt1.block_on(async {
        let brain = Brain::new(|req| {
            if is_child(req) {
                None
            } else {
                calls_owned(vec![spawn("c1", "research")])
            }
        });
        let lp = make(
            &env.storage,
            bot,
            &brain,
            &[],
            profile(""),
            cfg(Policy::full()),
        );
        let parent = lp.run_turn(chat, "go").unwrap();
        for _ in 0..500 {
            if brain.requests().iter().any(is_child) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(brain.requests().iter().any(is_child), "child started");
        parent
    });
    drop(rt1);
    let _data_dir = env.crash();

    // Process 2: recover, resume the parent; its tool step waits for the child.
    let storage = Arc::new(Storage::open(&path).unwrap());
    rt2().block_on(async {
        let brain = Brain::new(|req| {
            if is_child(req) {
                say("recovered answer")
            } else if results(req).is_empty() {
                panic!("the parent must not re-plan: its step is replayed, not re-asked");
            } else {
                say(&format!("parent: {}", results(req)[0]))
            }
        });
        let lp = make(&storage, bot, &brain, &[], profile(""), cfg(Policy::full()));
        let kids = storage.list_child_runs(parent).unwrap();
        assert_eq!(kids.len(), 1);
        let kid = kids[0].id;
        let mut recovered = lp.recover().unwrap();
        recovered.sort_by_key(|r| r.0);
        let mut want = vec![parent, kid];
        want.sort_by_key(|r| r.0);
        assert_eq!(recovered, want);
        assert_eq!(lp.runtime().state(parent).unwrap(), RunState::Paused);
        assert_eq!(lp.runtime().state(kid).unwrap(), RunState::Paused);

        lp.resume(parent).unwrap();
        let st = lp.wait(parent).await.unwrap();
        assert_eq!(st, RunState::Succeeded);
        // The very same child finished; no second child was spawned.
        let kids = storage.list_child_runs(parent).unwrap();
        assert_eq!(kids.len(), 1);
        assert_eq!(kids[0].id, kid);
        assert_eq!(lp.runtime().state(kid).unwrap(), RunState::Succeeded);
        let msgs = storage.list_messages(chat).unwrap();
        assert_eq!(msgs.last().unwrap().content, "parent: recovered answer");
    });
}

#[tokio::test]
async fn graceful_shutdown_pauses_parent_and_child_and_the_restart_adopts_the_child() {
    let env = setup();
    let hang = Brain::new(|req| {
        if is_child(req) {
            None
        } else {
            calls_owned(vec![spawn("c1", "research")])
        }
    });
    let lp = make(
        &env.storage,
        env.bot,
        &hang,
        &[],
        profile(""),
        cfg(Policy::full()),
    );
    let parent = lp.run_turn(env.chat, "go").unwrap();
    for _ in 0..500 {
        if hang.requests().iter().any(is_child) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let kid = children(&env, parent)[0].id;
    let paused = lp.shutdown(Duration::from_millis(200)).await;
    assert!(
        paused.contains(&parent) && paused.contains(&kid),
        "{paused:?}"
    );
    assert_eq!(lp.runtime().state(parent).unwrap(), RunState::Paused);
    assert_eq!(lp.runtime().state(kid).unwrap(), RunState::Paused);
    // The parent's step was interrupted, not answered with an error.
    let last = env.storage.list_steps(parent).unwrap().pop().unwrap();
    assert_eq!(
        (last.name.as_str(), last.status.as_str()),
        ("tools", "interrupted")
    );

    // "Next start": a new loop over the same data.
    let brain = Brain::new(|req| {
        if is_child(req) {
            say("after restart")
        } else {
            say(&format!("parent: {}", results(req)[0]))
        }
    });
    let lp2 = make(
        &env.storage,
        env.bot,
        &brain,
        &[],
        profile(""),
        cfg(Policy::full()),
    );
    lp2.recover().unwrap();
    lp2.resume(parent).unwrap();
    assert_eq!(lp2.wait(parent).await.unwrap(), RunState::Succeeded);
    assert_eq!(children(&env, parent).len(), 1);
    let msgs = env.storage.list_messages(env.chat).unwrap();
    assert_eq!(msgs.last().unwrap().content, "parent: after restart");
}

// ---------- skills ----------

fn skill_loop(env: &Env, brain: &Arc<Brain>) -> AgentLoop {
    let skills_dir = env.dir.path().join("skills").join("my-site");
    std::fs::create_dir_all(&skills_dir).unwrap();
    std::fs::write(
        skills_dir.join("SKILL.md"),
        "---\nname: my-site\ndescription: How to use my-site\nwhen:\n  domains: [example.com]\n---\nStep one: click the blue button.\n",
    )
    .unwrap();
    let index = skills::SkillIndex::load(skills::SkillPaths::from_data_dir(env.dir.path(), None));
    make(
        &env.storage,
        env.bot,
        brain,
        &[],
        profile(""),
        cfg(Policy::full()),
    )
    .with_skills(index)
}

fn skill_brain() -> Arc<Brain> {
    Brain::new(|req| {
        if results(req).is_empty() {
            call(&[("s1", "skill_read", json!({"name": "my-site"}))])
        } else {
            say("read it")
        }
    })
}

#[tokio::test]
async fn skills_index_is_in_the_system_prompt_and_skill_read_loads_the_body() {
    let env = setup();
    let brain = skill_brain();
    let lp = skill_loop(&env, &brain);
    let run = lp.run_turn(env.chat, "please use my-site").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    let reqs = brain.requests();
    let sys = system_of(&reqs[0]);
    assert!(sys.contains("Skills (load one with skill_read"), "{sys}");
    assert!(sys.contains("- my-site: How to use my-site"), "{sys}");
    assert!(!sys.contains("[suggested] my-site"), "no page known: {sys}");
    assert!(tool_names(&reqs[0]).contains(&"skill_read".to_owned()));
    assert!(results(&reqs[1])[0].contains("click the blue button"));
}

#[tokio::test]
async fn the_foreground_page_makes_matching_skills_suggested() {
    let env = setup();
    let brain = skill_brain();
    let lp = skill_loop(&env, &brain);
    lp.set_skill_hints(Arc::new(|| SkillHints {
        domain: Some("example.com".into()),
        app: None,
    }));
    let run = lp.run_turn(env.chat, "hello").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert!(system_of(&brain.requests()[0]).contains("[suggested] my-site"));
}

#[tokio::test]
async fn a_skill_learned_by_this_bot_is_in_its_prompt_and_other_bots_skills_are_not() {
    let env = setup();
    let brain = skill_brain();
    let lp = skill_loop(&env, &brain);
    let learned = |bot: protocol::BotId, name: &str| {
        let dir = env
            .dir
            .path()
            .join("bots")
            .join(bot.to_string())
            .join("skills")
            .join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!(
                "---
name: {name}
description: learned {name}
---
body
"
            ),
        )
        .unwrap();
    };
    learned(env.bot, "learned-by-a");
    learned(protocol::BotId::new(), "learned-by-b");
    let run = lp.run_turn(env.chat, "hello").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    let reqs = brain.requests();
    let sys = system_of(&reqs[0]);
    assert!(
        sys.contains("- learned-by-a: learned learned-by-a"),
        "{sys}"
    );
    assert!(!sys.contains("learned-by-b"), "{sys}");
}

// ---------- catalog prices ----------

fn priced(input: f64, output: f64) -> ModelInfo {
    ModelInfo {
        id: "m".into(),
        input_per_mtok: Some(input),
        output_per_mtok: Some(output),
        ..ModelInfo::default()
    }
}

fn logged_cost(env: &Env) -> Option<f64> {
    env.storage
        .list_audit(env.user)
        .unwrap()
        .iter()
        .rev()
        .find(|e| e.action == "usage")
        .and_then(|e| e.detail["cost_usd"].as_f64())
}

#[tokio::test]
async fn cost_comes_from_the_catalog_unless_the_config_overrides_it() {
    // 10 prompt tokens (2 cached, no cached price: charged as input) + 5 completion.
    let env = setup();
    let brain = Brain::new(|_| say("hi"));
    let providers = Providers::single(Arc::clone(&brain) as Arc<dyn ChatProvider>)
        .with_catalog_prices("fake", &[priced(2.0, 10.0)]);
    let lp = AgentLoop::new(
        Arc::clone(&env.storage),
        ToolRegistry::new(),
        providers,
        cfg(Policy::full()),
    );
    lp.add_profile(env.bot, profile(""));
    assert_eq!(lp.price_for("fake/m").unwrap().input_per_mtok, 2.0);
    let run = lp.run_turn(env.chat, "hi").unwrap();
    lp.wait(run).await.unwrap();
    let want = (10.0 * 2.0 + 5.0 * 10.0) / 1_000_000.0;
    assert!((logged_cost(&env).unwrap() - want).abs() < 1e-12);

    // A `LoopConfig::prices` entry wins over the catalog.
    let env2 = setup();
    let mut config = cfg(Policy::full());
    config.prices.insert(
        "fake/m".into(),
        ModelPrice {
            input_per_mtok: 4.0,
            output_per_mtok: 20.0,
            cached_per_mtok: None,
        },
    );
    let providers = Providers::single(Arc::clone(&brain) as Arc<dyn ChatProvider>)
        .with_catalog_prices("fake", &[priced(2.0, 10.0)]);
    let lp2 = AgentLoop::new(
        Arc::clone(&env2.storage),
        ToolRegistry::new(),
        providers,
        config,
    );
    lp2.add_profile(env2.bot, profile(""));
    let run = lp2.run_turn(env2.chat, "hi").unwrap();
    lp2.wait(run).await.unwrap();
    assert!((logged_cost(&env2).unwrap() - 2.0 * want).abs() < 1e-12);
}

#[tokio::test]
async fn a_providers_reload_replaces_the_catalog_prices() {
    let env = setup();
    let brain = Brain::new(|_| say("hi"));
    let with = |p: Option<ModelInfo>| {
        let base = Providers::single(Arc::clone(&brain) as Arc<dyn ChatProvider>);
        match p {
            Some(m) => base.with_catalog_prices("fake", &[m]),
            None => base,
        }
    };
    let lp = AgentLoop::new(
        Arc::clone(&env.storage),
        ToolRegistry::new(),
        with(None),
        cfg(Policy::full()),
    );
    assert!(lp.price_for("fake/m").is_none());
    lp.set_providers(with(Some(priced(1.0, 3.0))));
    assert_eq!(lp.price_for("fake/m").unwrap().output_per_mtok, 3.0);
    lp.set_providers(with(Some(priced(5.0, 7.0))));
    assert_eq!(lp.price_for("fake/m").unwrap().output_per_mtok, 7.0);
    // Models without both prices stay unpriced.
    let half = ModelInfo {
        id: "m".into(),
        input_per_mtok: Some(1.0),
        ..ModelInfo::default()
    };
    lp.set_providers(with(Some(half)));
    assert!(lp.price_for("fake/m").is_none());
}

// ---------- live check (opt-in) ----------

/// Counts requests and refuses to go past a hard cap.
struct Counting {
    inner: Arc<dyn ChatProvider>,
    n: std::sync::atomic::AtomicUsize,
    cap: usize,
}

#[async_trait]
impl ChatProvider for Counting {
    fn id(&self) -> &'static str {
        "gemini"
    }
    async fn stream(
        &self,
        req: ChatRequest,
    ) -> Result<BoxStream<'static, Result<ChatChunk, ProviderError>>, ProviderError> {
        if self.n.fetch_add(1, Ordering::SeqCst) >= self.cap {
            return Err(ProviderError::InvalidRequest(
                "live request cap reached".into(),
            ));
        }
        self.inner.stream(req).await
    }
}

#[tokio::test]
async fn a_child_read_the_parent_surface_cannot_resolve_still_gates_egress() {
    let env = setup();
    let read = Arc::new(with_class(
        "fs_read",
        Effect::Read,
        PermissionCategory::Fs,
        ContentClass::untrusted_private(),
        "Permission denied: secret notes",
    ));
    let search = Arc::new(with_class(
        "web_search",
        Effect::Read,
        PermissionCategory::Browser,
        ContentClass::untrusted_egress(),
        "sent",
    ));
    let tools = vec![
        Arc::clone(&read) as Arc<dyn Tool>,
        Arc::clone(&search) as Arc<dyn Tool>,
    ];
    let brain = Brain::new(|req| {
        if is_child(req) {
            if results(req).is_empty() {
                call(&[("k1", "fs_read", json!({"path": "secret.txt"}))])
            } else {
                say("done")
            }
        } else if results(req).is_empty() {
            call(&[(
                "p1",
                "spawn_subagent",
                json!({"task": "read the file", "tools": ["fs_read"]}),
            )])
        } else if results(req).len() == 1 {
            call(&[("p2", "web_search", json!({"query": "q"}))])
        } else {
            say("done")
        }
    });
    let lp = make(
        &env.storage,
        env.bot,
        &brain,
        &tools,
        profile(""),
        cfg(Policy::full()),
    );
    let mut rx = lp.subscribe();
    let parent = lp.run_turn(env.chat, "go").unwrap();
    let event = until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await;
    let Event::ApprovalNeeded(a) = event else {
        unreachable!()
    };
    assert_eq!(a.tool_name, "web_search");
    assert_eq!(read.count.load(Ordering::SeqCst), 1);
    assert_eq!(search.count.load(Ordering::SeqCst), 0);
    let kids = children(&env, parent);
    assert_eq!(kids.len(), 1);
    let fact = env.storage.list_steps(kids[0].id).unwrap();
    let tools_step = fact.iter().find(|s| s.name == "tools").unwrap();
    let recorded = &tools_step.output.as_ref().unwrap()["results"][0];
    assert!(
        recorded["text"]
            .as_str()
            .unwrap()
            .starts_with("Permission denied"),
        "{recorded}"
    );
    assert_eq!(recorded["fact"]["ran"], true, "{recorded}");
    assert_eq!(recorded["fact"]["reads_private"], true, "{recorded}");
    lp.cancel(parent).unwrap();
}

/// A real model spawns two subagents in one turn.  Run by hand:
/// `GEMINI_API_KEY=... cargo test -p agent-core live_ -- --ignored --nocapture`
/// (the key is read from the environment by the provider, never printed).
#[tokio::test]
#[ignore = "live: needs GEMINI_API_KEY and spends a few requests"]
async fn live_gemini_parent_spawns_two_subagents() {
    let entry: provider_catalog::ProviderEntry =
        toml::from_str(include_str!("../../../providers/catalog/gemini.toml")).unwrap();
    let inner: Arc<dyn ChatProvider> = Arc::from(
        provider_catalog::ProviderSettings::new("gemini", 0)
            .build_provider(
                &entry,
                &secret_store::MemoryStore::new(),
                provider_catalog::OpenAiCompatConfig::default(),
            )
            .expect("GEMINI_API_KEY in the environment"),
    );
    let counting = Arc::new(Counting {
        inner,
        n: std::sync::atomic::AtomicUsize::new(0),
        cap: 18,
    });
    let env = setup();
    let registry = ToolRegistry::new();
    let mut config = cfg(Policy::full());
    config.model_retry.max_attempts = 1;
    config.guards.max_steps = 6;
    let lp = AgentLoop::new(
        Arc::clone(&env.storage),
        registry,
        Providers::single(Arc::clone(&counting) as Arc<dyn ChatProvider>)
            .with_catalog_prices("gemini", &entry.models),
        config,
    )
    .with_subagents();
    lp.add_profile(
        env.bot,
        BotProfile::from_toml_str(
            "[bot]\nid = \"live\"\nname = \"Live\"\n[models]\nexecutor = \"gemini/gemini-2.5-flash-lite\"\n",
        )
        .unwrap(),
    );
    let run = lp
        .run_turn(
            env.chat,
            "Call the spawn_subagent tool exactly twice in the same turn (two parallel calls, \
             tools: []): subagent one task 'What is 17 times 23? Reply with the number only.', \
             subagent two task 'What is the capital of France? Reply with one word.'. \
             Then answer with both results in one short sentence.",
        )
        .unwrap();
    let state = lp.wait(run).await.unwrap();
    let kids = children(&env, run);
    eprintln!(
        "live: state={state:?} children={} requests={}",
        kids.len(),
        counting.n.load(Ordering::SeqCst)
    );
    eprintln!(
        "live: run error={:?}",
        env.storage.get_run(run).unwrap().and_then(|r| r.error)
    );
    for k in &kids {
        eprintln!(
            "live: child state={} spec_task={}",
            k.state,
            k.spec.as_ref().unwrap()["task"]
        );
    }
    eprintln!(
        "live: answer={}",
        env.storage
            .list_messages(env.chat)
            .unwrap()
            .last()
            .map_or("", |m| m.content.as_str())
    );
    assert_eq!(state, RunState::Succeeded);
    assert_eq!(kids.len(), 2, "two subagents were spawned");
    assert!(kids.iter().all(|k| k.state == "succeeded"));
}

#[tokio::test]
async fn finished_children_are_charged_against_the_parents_budget() {
    let env = setup();
    let brain = Brain::new(|req| {
        if is_child(req) {
            Some(vec![
                ChatChunk::TextDelta("ok".to_owned()),
                ChatChunk::Usage(Usage {
                    prompt_tokens: 2_000,
                    completion_tokens: 1_000,
                    cached_tokens: None,
                    cache_write_tokens: None,
                }),
                ChatChunk::Done {
                    finish_reason: FinishReason::Stop,
                },
            ])
        } else {
            match results(req).len() {
                0 => calls_owned(vec![spawn("c1", "first")]),
                1 => calls_owned(vec![spawn("c2", "second")]),
                _ => say("fin"),
            }
        }
    });
    let mut config = cfg(Policy::full());
    config.guards.max_tokens_per_turn = 4_000;
    let lp = make(&env.storage, env.bot, &brain, &[], profile(""), config);
    let parent = lp.run_turn(env.chat, "go").unwrap();
    assert_eq!(lp.wait(parent).await.unwrap(), RunState::Succeeded);
    let kids = children(&env, parent);
    assert_eq!(kids.len(), 2);
    assert_eq!(kids[0].spec.as_ref().unwrap()["token_budget"], 1_000);
    // The first child burned 3000 of the parent's 4000.
    assert_eq!(kids[1].spec.as_ref().unwrap()["token_budget"], 250);
}

#[tokio::test]
async fn a_child_inherits_a_private_read_and_asks_before_search() {
    let env = setup();
    let read = Arc::new(with_class(
        "fs_read",
        Effect::Read,
        PermissionCategory::Fs,
        ContentClass::untrusted_private(),
        "secret",
    ));
    let search = Arc::new(with_class(
        "web_search",
        Effect::Read,
        PermissionCategory::Browser,
        ContentClass::untrusted_egress(),
        "hits",
    ));
    let brain = Brain::new(|req| {
        if is_child(req) {
            if results(req).is_empty() {
                call(&[("w", "web_search", json!({"query": "leak"}))])
            } else {
                say("child done")
            }
        } else if results(req).is_empty() {
            call(&[("r", "fs_read", json!({"path": "secret.txt"}))])
        } else if results(req).len() == 1 {
            call(&[("s", "spawn_subagent", json!({"task": "search"}))])
        } else {
            say("parent done")
        }
    });
    let lp = make(
        &env.storage,
        env.bot,
        &brain,
        &[
            Arc::clone(&read) as Arc<dyn Tool>,
            Arc::clone(&search) as Arc<dyn Tool>,
        ],
        profile(""),
        cfg(Policy::full()),
    );
    let mut rx = lp.subscribe();
    let parent = lp.run_turn(env.chat, "go").unwrap();
    let event = until(
        &mut rx,
        |e| matches!(e, Event::ApprovalNeeded(a) if a.tool_name == "web_search"),
    )
    .await;
    let Event::ApprovalNeeded(a) = event else {
        unreachable!()
    };
    let reason = a.details.unwrap()["reason"].as_str().unwrap().to_owned();
    assert!(reason.contains("file secret.txt"), "{reason}");
    assert_eq!(search.count.load(Ordering::SeqCst), 0);
    lp.cancel(parent).unwrap();
}
