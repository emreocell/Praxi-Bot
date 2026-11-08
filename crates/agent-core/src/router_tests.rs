//! The K0-K3 ladder wired into the loop: every route kind end to end, with a
//! scripted provider (executor and classifier are different models) and fake
//! tools.  Nothing here touches the network.

#![allow(clippy::needless_pass_by_value, clippy::assert_is_empty)]

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use agent_router::ladder::{BackendError, EmbedTask, Embedder};
use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use parking_lot::Mutex;
use permission_engine::Policy;
use protocol::event::RouteDecided;
use protocol::{ChatId, Event};
use provider_catalog::{
    ChatChunk, ChatProvider, ChatRequest, FinishReason, MessageRole, ProviderError, ToolCall,
};
use serde_json::{Value, json};
use skills::{SkillIndex, SkillPaths};
use task_runtime::RunState;
use tokio::sync::broadcast;
use tool_registry::{Effect, Tool, ToolRegistry};
use workflow_replay::{AgentStep, Handles, Step, ToolStep, Workflow, WorkflowStore};

use crate::tests::{Env, TestTool, cfg, profile, setup, tool};
use crate::{AgentLoop, Delivery, LoopConfig, MessageMode, Providers, StreamItem};

// ---------- a provider scripted per model ----------

enum Rep {
    Text(&'static str),
    Calls(Vec<(&'static str, &'static str)>),
    Hang,
}

struct Script {
    replies: Mutex<HashMap<String, VecDeque<Rep>>>,
    requests: Mutex<Vec<ChatRequest>>,
}

impl Script {
    fn new(replies: Vec<(&str, Vec<Rep>)>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(
                replies
                    .into_iter()
                    .map(|(m, r)| (m.to_owned(), r.into()))
                    .collect(),
            ),
            requests: Mutex::new(Vec::new()),
        })
    }

    fn requests_for(&self, model: &str) -> Vec<ChatRequest> {
        self.requests
            .lock()
            .iter()
            .filter(|r| r.model.0 == model)
            .cloned()
            .collect()
    }

    fn n(&self, model: &str) -> usize {
        self.requests_for(model).len()
    }
}

#[async_trait]
impl ChatProvider for Script {
    fn id(&self) -> &'static str {
        "fake"
    }
    async fn stream(
        &self,
        req: ChatRequest,
    ) -> Result<BoxStream<'static, Result<ChatChunk, ProviderError>>, ProviderError> {
        let model = req.model.0.clone();
        self.requests.lock().push(req);
        let rep = self
            .replies
            .lock()
            .get_mut(&model)
            .and_then(VecDeque::pop_front);
        match rep {
            Some(Rep::Text(t)) => Ok(futures::stream::iter(vec![
                Ok(ChatChunk::TextDelta(t.to_owned())),
                Ok(ChatChunk::Done {
                    finish_reason: FinishReason::Stop,
                }),
            ])
            .boxed()),
            Some(Rep::Calls(calls)) => {
                let mut v: Vec<Result<ChatChunk, ProviderError>> = calls
                    .into_iter()
                    .map(|(id, name)| {
                        Ok(ChatChunk::ToolCall(ToolCall {
                            id: id.to_owned(),
                            name: name.replace('.', "__"),
                            arguments: "{}".to_owned(),
                            provider_extra: None,
                        }))
                    })
                    .collect();
                v.push(Ok(ChatChunk::Done {
                    finish_reason: FinishReason::ToolCalls,
                }));
                Ok(futures::stream::iter(v).boxed())
            }
            Some(Rep::Hang) => Ok(futures::stream::pending().boxed()),
            None => Err(ProviderError::InvalidRequest(format!(
                "no scripted reply for {model}"
            ))),
        }
    }
}

struct CountingEmbedder {
    documents: AtomicUsize,
}

#[async_trait]
impl Embedder for CountingEmbedder {
    async fn embed(
        &self,
        texts: &[String],
        task: EmbedTask,
    ) -> Result<Vec<Vec<f32>>, BackendError> {
        if task == EmbedTask::Document {
            self.documents.fetch_add(1, Ordering::SeqCst);
        }
        Ok(texts.iter().map(|_| vec![1.0, 0.0, 0.0]).collect())
    }
}

// ---------- harness ----------

fn loop_with(
    env: &Env,
    providers: Providers,
    tools: &[Arc<dyn Tool>],
    prof: &str,
    config: LoopConfig,
) -> AgentLoop {
    let registry = ToolRegistry::new();
    for t in tools {
        registry.register(Arc::clone(t));
    }
    let lp = AgentLoop::new(Arc::clone(&env.storage), registry, providers, config);
    lp.add_profile(env.bot, profile(prof));
    lp
}

fn make(env: &Env, script: &Arc<Script>, tools: &[Arc<dyn Tool>], prof: &str) -> AgentLoop {
    loop_with(
        env,
        Providers::single(Arc::clone(script) as Arc<dyn ChatProvider>),
        tools,
        prof,
        cfg(Policy::full()),
    )
}

fn as_tool(t: &Arc<TestTool>) -> Arc<dyn Tool> {
    Arc::clone(t) as Arc<dyn Tool>
}

fn wf_tool_step(name: &str, tool: &str, description: &str) -> Workflow {
    let mut wf = Workflow::new(name);
    description.clone_into(&mut wf.description);
    wf.steps = vec![Step::Tool(ToolStep {
        tool: tool.to_owned(),
        input: json!({}),
    })];
    wf
}

fn with_store(env: &Env, lp: AgentLoop) -> (AgentLoop, WorkflowStore) {
    let store = WorkflowStore::new(env.dir.path());
    let lp = lp.with_workflows(
        store.clone(),
        Handles {
            desktop: None,
            browser: None,
            tools: ToolRegistry::new(),
        },
    );
    (lp, store)
}

fn drain(rx: &mut broadcast::Receiver<StreamItem>) -> Vec<Event> {
    let mut v = Vec::new();
    while let Ok(i) = rx.try_recv() {
        v.push(i.envelope.event);
    }
    v
}

fn decided(evs: &[Event]) -> Vec<RouteDecided> {
    evs.iter()
        .filter_map(|e| match e {
            Event::RouteDecided(d) => Some(d.clone()),
            _ => None,
        })
        .collect()
}

async fn turn(lp: &AgentLoop, chat: ChatId, text: &str) -> (protocol::RunId, RunState) {
    let run = lp.run_turn(chat, text).unwrap();
    let state = lp.wait(run).await.unwrap();
    (run, state)
}

fn assistant_texts(env: &Env) -> Vec<String> {
    env.storage
        .list_messages(env.chat)
        .unwrap()
        .into_iter()
        .filter(|m| m.role == "assistant")
        .map(|m| m.content)
        .collect()
}

fn tool_names(req: &ChatRequest) -> Vec<String> {
    let mut v: Vec<String> = req
        .tools
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap().to_owned())
        .collect();
    v.sort();
    v
}

const CLASSIFIER: &str = "classifier = \"fake/c\"\n";

// ---------- K0: commands ----------

#[tokio::test]
async fn stop_command_cancels_the_run_that_carries_it() {
    let env = setup();
    let script = Script::new(vec![("fake/m", vec![Rep::Text("never")])]);
    let lp = make(&env, &script, &[], "");
    let mut rx = lp.subscribe();
    let (run, state) = turn(&lp, env.chat, "/stop").await;
    assert_eq!(state, RunState::Cancelled);
    assert_eq!(script.n("fake/m"), 0, "no model call");
    let d = decided(&drain(&mut rx));
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].level, "K0");
    assert_eq!(d[0].route["kind"], "command");
    assert_eq!(d[0].route["name"], "stop");
    // The decision is the audit record of the first step.
    let first = &env.storage.list_steps(run).unwrap()[0];
    assert_eq!(first.input["route"]["level"], "K0");
    assert_eq!(first.input["route"]["route"]["name"], "stop");
}

#[tokio::test]
async fn stop_message_cancels_the_running_turn() {
    let env = setup();
    let script = Script::new(vec![("fake/m", vec![Rep::Hang])]);
    let lp = make(&env, &script, &[], "");
    let run = lp.run_turn(env.chat, "do something long").unwrap();
    while script.n("fake/m") == 0 {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let delivery = lp
        .send_user_message(env.chat, " /stop ", MessageMode::Steer)
        .unwrap();
    assert_eq!(delivery, Delivery::Stopped(run));
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Cancelled);
    // A new turn can start right away.
    assert!(lp.run_turn(env.chat, "/stop").is_ok());
}

#[tokio::test]
async fn new_command_starts_a_chat_without_a_model() {
    let env = setup();
    let script = Script::new(vec![]);
    let lp = make(&env, &script, &[], "");
    let (_, state) = turn(&lp, env.chat, "/new").await;
    assert_eq!(state, RunState::Succeeded);
    assert_eq!(script.n("fake/m"), 0);
    assert_eq!(env.storage.list_chats(env.user).unwrap().len(), 2);
    let said = assistant_texts(&env);
    assert_eq!(said.len(), 1);
    assert!(said[0].starts_with("Started a new chat: "), "{said:?}");
}

// ---------- workflows ----------

#[tokio::test]
async fn workflow_command_replays_through_the_gate_without_a_model() {
    let env = setup();
    let log = Arc::new(tool("t_log", Effect::Write, "logged"));
    let script = Script::new(vec![]);
    let (lp, store) = with_store(&env, make(&env, &script, &[as_tool(&log)], ""));
    store
        .save(&wf_tool_step("log_it", "t_log", "write a log line"))
        .unwrap();
    let mut rx = lp.subscribe();
    let (_, state) = turn(&lp, env.chat, "/workflow log_it").await;
    assert_eq!(state, RunState::Succeeded);
    assert_eq!(log.count.load(Ordering::SeqCst), 1);
    assert_eq!(script.requests.lock().len(), 0, "zero model calls");
    let evs = drain(&mut rx);
    let d = decided(&evs);
    assert_eq!(
        (d[0].level.as_str(), d[0].route["kind"].as_str()),
        ("K0", Some("workflow"))
    );
    assert!(
        evs.iter()
            .any(|e| matches!(e, Event::WorkflowStepStarted(s) if s.workflow == "log_it" && s.run_id.is_some()))
    );
    assert!(
        evs.iter()
            .any(|e| matches!(e, Event::WorkflowStepCompleted(_)))
    );
    let said = assistant_texts(&env);
    assert!(said[0].contains("completed"), "{said:?}");
}

#[tokio::test]
async fn scheduled_workflow_run_replays_through_the_gate_without_a_model() {
    let env = setup();
    let log = Arc::new(tool("t_log", Effect::Write, "logged"));
    let script = Script::new(vec![]);
    let (lp, store) = with_store(&env, make(&env, &script, &[as_tool(&log)], ""));
    store
        .save(&wf_tool_step("log_it", "t_log", "write a log line"))
        .unwrap();
    let mut rx = lp.subscribe();
    let sid = protocol::TaskId::new();
    let run = lp
        .run_scheduled(
            env.chat,
            sid,
            crate::ScheduleWork::Workflow {
                name: "log_it".into(),
                params: std::collections::BTreeMap::new(),
            },
        )
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(log.count.load(Ordering::SeqCst), 1);
    assert_eq!(script.requests.lock().len(), 0, "zero model calls");
    let evs = drain(&mut rx);
    let started = evs.iter().find_map(|e| match e {
        Event::RunStarted(r) => Some(r.clone()),
        _ => None,
    });
    assert_eq!(started.unwrap().schedule_id, Some(sid));
    assert!(
        evs.iter()
            .any(|e| matches!(e, Event::WorkflowStepStarted(s) if s.workflow == "log_it"))
    );
}

#[tokio::test]
async fn workflow_replay_asks_the_loops_approval_flow() {
    let env = setup();
    let log = Arc::new(tool("t_log", Effect::Write, "logged"));
    let script = Script::new(vec![]);
    let lp = loop_with(
        &env,
        Providers::single(Arc::clone(&script) as Arc<dyn ChatProvider>),
        &[as_tool(&log)],
        "",
        cfg(Policy::approve_destructive()),
    );
    let (lp, store) = with_store(&env, lp);
    store
        .save(&wf_tool_step("log_it", "t_log", "write a log line"))
        .unwrap();
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "/workflow log_it").unwrap();
    // `t_log` is a write in the fs category: the policy asks, the replay waits.
    let call = loop {
        let item = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .unwrap()
            .unwrap();
        if let Event::ApprovalNeeded(a) = item.envelope.event {
            break a.call_id;
        }
    };
    assert_eq!(
        log.count.load(Ordering::SeqCst),
        0,
        "not run before approval"
    );
    lp.deny(call, env.user, Some("no".into())).unwrap();
    // A denied step is a handover: K4 continues (the script has no reply, so it fails).
    let _ = lp.wait(run).await.unwrap();
    assert_eq!(log.count.load(Ordering::SeqCst), 0);
    assert_eq!(script.n("fake/m"), 1, "the denied replay handed over to K4");
}

#[tokio::test]
async fn workflow_handover_continues_in_k4_with_the_partial_state() {
    let env = setup();
    let script = Script::new(vec![("fake/m", vec![Rep::Text("handled")])]);
    let (lp, store) = with_store(&env, make(&env, &script, &[], ""));
    let mut wf = Workflow::new("needs_agent");
    wf.description = "needs a decision".into();
    wf.steps = vec![Step::Agent(AgentStep {
        instruction: "Pick the right folder".into(),
    })];
    store.save(&wf).unwrap();
    let (run, state) = turn(&lp, env.chat, "/workflow needs_agent").await;
    assert_eq!(state, RunState::Succeeded);
    assert_eq!(assistant_texts(&env), ["handled"]);
    let req = script.requests_for("fake/m").remove(0);
    let user = req
        .messages
        .iter()
        .rfind(|m| m.role == MessageRole::User)
        .unwrap()
        .content
        .as_str()
        .unwrap()
        .to_owned();
    // SEC-10 keeps the user text as the original request. The trusted handover
    // instruction is a system message; the partial state is an untrusted
    // assistant data block.
    assert_eq!(user, "/workflow needs_agent");
    let handover = req
        .messages
        .iter()
        .find(|m| {
            m.role == MessageRole::System
                && m.content
                    .as_str()
                    .is_some_and(|s| s.contains("did not complete"))
        })
        .unwrap_or_else(|| panic!("missing handover instruction: {req:?}"));
    assert!(
        handover
            .content
            .as_str()
            .unwrap()
            .contains("untrusted tool data"),
        "{handover:?}"
    );
    let partial = req
        .messages
        .iter()
        .rfind(|m| m.role == MessageRole::Assistant)
        .unwrap_or_else(|| panic!("missing partial state: {req:?}"));
    let partial_text = partial.content.as_str().unwrap();
    assert!(
        partial_text.contains("Pick the right folder"),
        "{partial_text}"
    );
    assert!(
        partial_text.contains("router.workflow_replay"),
        "{partial_text}"
    );
    assert!(partial.tool_calls.is_empty());
    assert!(partial.tool_call_id.is_none());
    // Steps: user (with the decision), the workflow attempt, the model call.
    let names: Vec<String> = env
        .storage
        .list_steps(run)
        .unwrap()
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(names, ["user_message", "workflow", "model"]);
}

#[tokio::test]
async fn an_unknown_workflow_command_falls_back_to_k4() {
    let env = setup();
    let script = Script::new(vec![("fake/m", vec![Rep::Text("ok")])]);
    let (lp, _store) = with_store(&env, make(&env, &script, &[], ""));
    let (_, state) = turn(&lp, env.chat, "/workflow nope").await;
    assert_eq!(state, RunState::Succeeded);
    let req = script.requests_for("fake/m").remove(0);
    let user = req
        .messages
        .last()
        .unwrap()
        .content
        .as_str()
        .unwrap()
        .to_owned();
    assert!(user.contains("could not be replayed"), "{user}");
}

// ---------- K3, K1, remember, invalidate ----------

const WF_ROUTE: &str = r#"{"route":"workflow","name":"log_it","params":{},"confidence":0.95}"#;
const AGENT_ROUTE: &str = r#"{"route":"agent","name":null,"confidence":0.2}"#;

#[tokio::test]
async fn classifier_routes_to_a_workflow_and_remember_makes_it_a_cache_hit() {
    let env = setup();
    let log = Arc::new(tool("t_log", Effect::Write, "logged"));
    let script = Script::new(vec![
        ("fake/c", vec![Rep::Text(WF_ROUTE), Rep::Text(AGENT_ROUTE)]),
        ("fake/m", vec![Rep::Text("done by the model")]),
    ]);
    let (lp, store) = with_store(&env, make(&env, &script, &[as_tool(&log)], CLASSIFIER));
    store
        .save(&wf_tool_step("log_it", "t_log", "write a log line"))
        .unwrap();
    let mut rx = lp.subscribe();
    let text = "please tidy up my desk";

    let (run1, s1) = turn(&lp, env.chat, text).await;
    assert_eq!(s1, RunState::Succeeded);
    let d = decided(&drain(&mut rx));
    assert_eq!(d[0].level, "K3");
    assert_eq!(d[0].route["kind"], "workflow");
    assert!(d[0].confidence >= 0.9);
    assert_eq!(log.count.load(Ordering::SeqCst), 1);
    assert_eq!(script.n("fake/m"), 0);

    // Remembering: the same text now routes at K1, no classifier call.
    assert!(lp.can_remember(run1), "eligible, and asking caches nothing");
    let route = lp.remember_route(run1).unwrap();
    assert_eq!(route["kind"], "workflow");
    assert_eq!(route["name"], "log_it");
    let (_, s2) = turn(&lp, env.chat, text).await;
    assert_eq!(s2, RunState::Succeeded);
    let d = decided(&drain(&mut rx));
    assert_eq!(d[0].level, "K1");
    assert_eq!(script.n("fake/c"), 1);
    assert_eq!(log.count.load(Ordering::SeqCst), 2);

    // Editing the workflow changes its version: the cached route is stale, the
    // ladder asks the classifier again (here: "agent").
    store
        .save(&wf_tool_step(
            "log_it",
            "t_log",
            "write a different log line",
        ))
        .unwrap();
    lp.workflow_changed("log_it");
    let (_, s3) = turn(&lp, env.chat, text).await;
    assert_eq!(s3, RunState::Succeeded);
    let d = decided(&drain(&mut rx));
    assert_eq!(d[0].level, "K3");
    assert_eq!(d[0].route["kind"], "agent");
    assert_eq!(script.n("fake/c"), 2);
    assert_eq!(script.n("fake/m"), 1);
}

#[tokio::test]
async fn an_edited_workflow_makes_the_cached_route_stale_even_without_a_notification() {
    let env = setup();
    let log = Arc::new(tool("t_log", Effect::Write, "logged"));
    let script = Script::new(vec![
        ("fake/c", vec![Rep::Text(WF_ROUTE), Rep::Text(AGENT_ROUTE)]),
        ("fake/m", vec![Rep::Text("model")]),
    ]);
    let (lp, store) = with_store(&env, make(&env, &script, &[as_tool(&log)], CLASSIFIER));
    store
        .save(&wf_tool_step("log_it", "t_log", "write a log line"))
        .unwrap();
    let text = "please tidy up my desk";
    let (run1, _) = turn(&lp, env.chat, text).await;
    lp.remember_route(run1).unwrap();
    // Deleted behind the loop's back: K1 notices through the candidate version.
    assert!(store.delete("log_it").unwrap());
    let mut rx = lp.subscribe();
    let (_, state) = turn(&lp, env.chat, text).await;
    assert_eq!(state, RunState::Succeeded);
    let d = decided(&drain(&mut rx));
    assert_ne!(d[0].level, "K1", "{d:?}");
    assert_eq!(log.count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn remember_refuses_runs_that_took_no_route_or_did_not_succeed() {
    let env = setup();
    let script = Script::new(vec![
        ("fake/m", vec![Rep::Text("plain")]),
        ("fake/c", vec![Rep::Text(AGENT_ROUTE)]),
    ]);
    let t = Arc::new(tool("t_a", Effect::Read, "a"));
    let lp = make(&env, &script, &[as_tool(&t)], CLASSIFIER);
    let (run, _) = turn(&lp, env.chat, "hello there friend").await;
    assert!(!lp.can_remember(run));
    let err = lp.remember_route(run).unwrap_err();
    assert!(matches!(err, crate::LoopError::Router(_)), "{err}");
    // A cancelled run is refused as well; an unknown one is not found.
    let (stopped, state) = turn(&lp, env.chat, "/stop").await;
    assert_eq!(state, RunState::Cancelled);
    assert!(!lp.can_remember(stopped));
    assert!(!lp.can_remember(protocol::RunId::new()));
    assert!(matches!(
        lp.remember_route(stopped),
        Err(crate::LoopError::Router(_))
    ));
    assert!(matches!(
        lp.remember_route(protocol::RunId::new()),
        Err(crate::LoopError::NotFound(_))
    ));
}

#[tokio::test]
async fn a_workflow_the_model_ran_can_be_remembered() {
    let env = setup();
    let log = Arc::new(tool("t_log", Effect::Write, "logged"));
    let script = Script::new(vec![
        ("fake/c", vec![Rep::Text(AGENT_ROUTE)]),
        (
            "fake/m",
            vec![
                Rep::Calls(vec![("c1", "workflow_run")]),
                Rep::Text("ran it"),
            ],
        ),
    ]);
    let (lp, store) = with_store(&env, make(&env, &script, &[as_tool(&log)], CLASSIFIER));
    store
        .save(&wf_tool_step("log_it", "t_log", "write a log line"))
        .unwrap();
    // The scripted call has no arguments, so it fails; remember refuses.
    let (run, state) = turn(&lp, env.chat, "tidy my desk please").await;
    assert_eq!(state, RunState::Succeeded);
    assert!(matches!(
        lp.remember_route(run),
        Err(crate::LoopError::Router(_))
    ));
}

// ---------- hints ----------

#[tokio::test]
async fn tool_hint_narrows_the_surface_and_keeps_discovery() {
    let env = setup();
    let (a, b, c) = (
        Arc::new(tool("t_a", Effect::Read, "a")),
        Arc::new(tool("t_b", Effect::Read, "b")),
        Arc::new(tool("t_c", Effect::Read, "c")),
    );
    let script = Script::new(vec![
        (
            "fake/c",
            vec![Rep::Text(
                r#"{"route":"tool","name":"t_b","confidence":0.8}"#,
            )],
        ),
        (
            "fake/m",
            vec![Rep::Calls(vec![("k1", "t_b")]), Rep::Text("done")],
        ),
    ]);
    let lp = make(
        &env,
        &script,
        &[as_tool(&a), as_tool(&b), as_tool(&c)],
        CLASSIFIER,
    );
    let mut rx = lp.subscribe();
    let (_, state) = turn(&lp, env.chat, "frobnicate the gizmo").await;
    assert_eq!(state, RunState::Succeeded);
    let d = decided(&drain(&mut rx));
    assert_eq!(d[0].route["kind"], "tool_hint");
    assert_eq!(d[0].route["tools"], json!(["t_b"]));
    let reqs = script.requests_for("fake/m");
    assert_eq!(
        tool_names(&reqs[0]),
        ["find_tool", "get_tool_details", "t_b", "use_tool"]
    );
    assert_eq!(b.count.load(Ordering::SeqCst), 1, "the hinted tool ran");
    assert_eq!(a.count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn skill_hint_lists_the_skill_first() {
    let env = setup();
    let t = Arc::new(tool("t_a", Effect::Read, "a"));
    let script = Script::new(vec![
        (
            "fake/c",
            vec![Rep::Text(
                r#"{"route":"skill","name":"github-basics","confidence":0.8}"#,
            )],
        ),
        ("fake/m", vec![Rep::Text("ok")]),
    ]);
    let lp =
        make(&env, &script, &[as_tool(&t)], CLASSIFIER).with_skills(SkillIndex::load(SkillPaths {
            user: None,
            bot: None,
        }));
    let mut rx = lp.subscribe();
    let (_, state) = turn(&lp, env.chat, "frobnicate the gizmo").await;
    assert_eq!(state, RunState::Succeeded);
    let d = decided(&drain(&mut rx));
    assert_eq!(d[0].route["kind"], "skill_hint", "{d:?}");
    let sys = script.requests_for("fake/m")[0].messages[0]
        .content
        .as_str()
        .unwrap()
        .to_owned();
    let first_skill_line = sys.lines().find(|l| l.starts_with("- ")).unwrap();
    assert!(
        first_skill_line.starts_with("- [suggested] github-basics"),
        "{first_skill_line}"
    );
}

// ---------- budget, switches, embedder ----------

#[tokio::test]
async fn a_slow_ladder_never_blocks_the_turn_beyond_its_budget() {
    let env = setup();
    let t = Arc::new(tool("t_a", Effect::Read, "a"));
    let script = Script::new(vec![
        ("fake/c", vec![Rep::Hang]),
        ("fake/m", vec![Rep::Text("answered")]),
    ]);
    let mut config = cfg(Policy::full());
    config.router_budget = Duration::from_millis(60);
    config.ladder.k3_timeout = Duration::from_secs(30);
    let lp = loop_with(
        &env,
        Providers::single(Arc::clone(&script) as Arc<dyn ChatProvider>),
        &[as_tool(&t)],
        CLASSIFIER,
        config,
    );
    let mut rx = lp.subscribe();
    let started = std::time::Instant::now();
    let (_, state) = turn(&lp, env.chat, "frobnicate the gizmo").await;
    assert_eq!(state, RunState::Succeeded);
    assert!(started.elapsed() < Duration::from_secs(5));
    let d = decided(&drain(&mut rx));
    assert_eq!(d[0].level, "K4");
    assert_eq!(d[0].route["kind"], "agent");
    assert!(d[0].reason.contains("budget"), "{}", d[0].reason);
    assert_eq!(assistant_texts(&env), ["answered"]);
}

#[tokio::test]
async fn a_classifier_timeout_is_fail_open_too() {
    let env = setup();
    let t = Arc::new(tool("t_a", Effect::Read, "a"));
    let script = Script::new(vec![
        ("fake/c", vec![Rep::Hang]),
        ("fake/m", vec![Rep::Text("answered")]),
    ]);
    let mut config = cfg(Policy::full());
    config.ladder.k3_timeout = Duration::from_millis(40);
    let lp = loop_with(
        &env,
        Providers::single(Arc::clone(&script) as Arc<dyn ChatProvider>),
        &[as_tool(&t)],
        CLASSIFIER,
        config,
    );
    let mut rx = lp.subscribe();
    let (_, state) = turn(&lp, env.chat, "frobnicate the gizmo").await;
    assert_eq!(state, RunState::Succeeded);
    let d = decided(&drain(&mut rx));
    assert_eq!(d[0].level, "K4");
    assert!(d[0].reason.contains("timeout"), "{}", d[0].reason);
}

#[tokio::test]
async fn a_disabled_router_changes_nothing() {
    let env = setup();
    let script = Script::new(vec![("fake/m", vec![Rep::Text("ok"), Rep::Text("ok2")])]);
    let t = Arc::new(tool("t_a", Effect::Read, "a"));
    let lp = make(
        &env,
        &script,
        &[as_tool(&t)],
        "classifier = \"fake/c\"\n[router]\nenabled = false\n",
    );
    let mut rx = lp.subscribe();
    // `/stop` is just text for the model now.
    let (_, state) = turn(&lp, env.chat, "/stop").await;
    assert_eq!(state, RunState::Succeeded);
    assert_eq!(script.n("fake/m"), 1);
    assert_eq!(script.n("fake/c"), 0);
    assert!(decided(&drain(&mut rx)).is_empty());
    // And a `/stop` message mid-turn is steered, not intercepted.
    assert!(matches!(
        lp.send_user_message(env.chat, "/stop", MessageMode::Steer),
        Ok(Delivery::NewTurn(_))
    ));
}

#[tokio::test]
async fn without_a_classifier_model_k3_is_off_and_k3_can_be_switched_off() {
    let env = setup();
    let t = Arc::new(tool("t_a", Effect::Read, "a"));
    for (extra, expect_calls) in [
        ("", 0),
        ("classifier = \"fake/c\"\n[router]\nk3 = false\n", 0),
    ] {
        let script = Script::new(vec![("fake/m", vec![Rep::Text("ok")])]);
        let lp = make(&env, &script, &[as_tool(&t)], extra);
        let (_, state) = turn(&lp, env.chat, "frobnicate the gizmo").await;
        assert_eq!(state, RunState::Succeeded);
        assert_eq!(script.n("fake/c"), expect_calls);
    }
    assert_eq!(profile(CLASSIFIER).k3_model(), Some("fake/c"));
    assert_eq!(
        profile("classifier = \"fake/c\"\n[router]\nk3 = false\n").k3_model(),
        None
    );
    assert_eq!(profile("").k3_model(), None);
}

#[tokio::test]
async fn the_embedder_comes_from_the_providers_and_a_reload_rebuilds_the_ladder() {
    let env = setup();
    let t = Arc::new(tool("t_a", Effect::Read, "a"));
    let script = Script::new(vec![("fake/m", vec![Rep::Text("1"), Rep::Text("2")])]);
    let embedder = Arc::new(CountingEmbedder {
        documents: AtomicUsize::new(0),
    });
    let providers = Providers::single(Arc::clone(&script) as Arc<dyn ChatProvider>)
        .with_embedder(Arc::clone(&embedder) as Arc<dyn Embedder>);
    let lp = loop_with(
        &env,
        providers.clone(),
        &[as_tool(&t)],
        "",
        cfg(Policy::full()),
    );
    turn(&lp, env.chat, "frobnicate the gizmo").await;
    lp.flush_background().await;
    assert_eq!(embedder.documents.load(Ordering::SeqCst), 1);
    // Same candidates, same ladder: nothing is embedded again.
    turn(&lp, env.chat, "frobnicate the gizmo").await;
    lp.flush_background().await;
    assert_eq!(embedder.documents.load(Ordering::SeqCst), 1);
    // A providers reload drops the ladders; the next turn builds a new one.
    let script2 = Script::new(vec![("fake/m", vec![Rep::Text("3")])]);
    lp.set_providers(
        Providers::single(script2 as Arc<dyn ChatProvider>)
            .with_embedder(Arc::clone(&embedder) as Arc<dyn Embedder>),
    );
    turn(&lp, env.chat, "frobnicate the gizmo").await;
    lp.flush_background().await;
    assert_eq!(embedder.documents.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn subagents_are_not_routed() {
    // A child run never gets a `route_decided` event and its first step has no decision.
    let env = setup();
    let t = Arc::new(tool("t_a", Effect::Read, "a"));
    let script = Script::new(vec![("fake/m", vec![Rep::Text("parent says hi")])]);
    let lp = make(&env, &script, &[as_tool(&t)], "").with_subagents();
    let mut rx = lp.subscribe();
    let (run, _) = turn(&lp, env.chat, "hello").await;
    let evs = drain(&mut rx);
    assert_eq!(decided(&evs).len(), 1);
    let first = &env.storage.list_steps(run).unwrap()[0];
    assert!(first.input.get("route").is_some_and(Value::is_object));
}

#[tokio::test]
async fn can_remember_does_not_build_a_ladder_or_touch_the_disk() {
    let env = setup();
    let log = Arc::new(tool("t_log", Effect::Write, "logged"));
    let script = Script::new(vec![
        ("fake/c", vec![Rep::Text(WF_ROUTE)]),
        ("fake/m", vec![Rep::Text("done")]),
    ]);
    let (lp, store) = with_store(&env, make(&env, &script, &[as_tool(&log)], CLASSIFIER));
    store
        .save(&wf_tool_step("log_it", "t_log", "write a log line"))
        .unwrap();
    let (run, state) = turn(&lp, env.chat, "please tidy up my desk").await;
    assert_eq!(state, RunState::Succeeded);
    // Forget the ladder the turn built, and its cache directory.
    lp.sh.router.ladders.lock().clear();
    let _ = std::fs::remove_dir_all(env.storage.data_dir().join("router"));
    assert!(lp.can_remember(run));
    assert!(lp.sh.router.ladders.lock().is_empty(), "no ladder created");
    assert!(
        !env.storage.data_dir().join("router").exists(),
        "no sqlite cache created"
    );
}

// ---------- control gate on the replay path ----------

struct CountingBrowser {
    opened: AtomicUsize,
}

#[async_trait]
impl builtin_tools::BrowserDriver for CountingBrowser {
    async fn open(&self, _url: &str, _profile: Option<&str>) -> Result<String, String> {
        self.opened.fetch_add(1, Ordering::SeqCst);
        Ok("# page".into())
    }
    async fn snapshot(&self, _i: bool) -> Result<String, String> {
        Ok(String::new())
    }
    async fn click(&self, _r: &str) -> Result<String, String> {
        Err("unused".into())
    }
    async fn fill(&self, _r: &str, _t: &str) -> Result<String, String> {
        Err("unused".into())
    }
    async fn select(&self, _r: &str, _v: &str) -> Result<String, String> {
        Err("unused".into())
    }
    async fn press(&self, _k: &str) -> Result<String, String> {
        Err("unused".into())
    }
    async fn scroll(&self, _r: Option<&str>, _x: f64, _y: f64) -> Result<String, String> {
        Err("unused".into())
    }
    async fn wait(&self, _c: builtin_tools::Wait, _t: Option<Duration>) -> Result<String, String> {
        Err("unused".into())
    }
    async fn back(&self) -> Result<String, String> {
        Err("unused".into())
    }
    async fn tabs(&self, _s: Option<usize>) -> Result<String, String> {
        Err("unused".into())
    }
    async fn screenshot(
        &self,
        _f: bool,
        _e: Option<&str>,
    ) -> Result<(Vec<u8>, &'static str), String> {
        Err("unused".into())
    }
    async fn eval(&self, _js: &str) -> Result<Value, String> {
        Err("unused".into())
    }
}

struct TakeoverGate {
    seen: Mutex<Vec<crate::ControlRequest>>,
}

#[async_trait]
impl crate::ControlGate for TakeoverGate {
    async fn before_action(
        &self,
        req: crate::ControlRequest,
        _publish: &(dyn Fn() + Send + Sync),
    ) -> Result<(), String> {
        self.seen.lock().push(req);
        Err("the user has taken control".into())
    }
}

#[tokio::test]
async fn replay_started_through_the_agent_is_refused_during_human_takeover() {
    let env = setup();
    let script = Script::new(vec![("fake/m", vec![Rep::Text("handled")])]);
    let lp = make(&env, &script, &[], "");
    let browser = Arc::new(CountingBrowser {
        opened: AtomicUsize::new(0),
    });
    let store = WorkflowStore::new(env.dir.path());
    let lp = lp.with_workflows(
        store.clone(),
        Handles {
            desktop: None,
            browser: Some(Arc::clone(&browser) as Arc<dyn builtin_tools::BrowserDriver>),
            tools: ToolRegistry::new(),
        },
    );
    let gate = Arc::new(TakeoverGate {
        seen: Mutex::new(Vec::new()),
    });
    lp.set_control_gate(gate.clone());
    let mut wf = Workflow::new("open_it");
    wf.description = "open a page".into();
    wf.steps = vec![Step::Navigate(workflow_replay::Navigate {
        url: "https://example.test/".into(),
        profile: None,
    })];
    store.save(&wf).unwrap();

    let (run, _) = turn(&lp, env.chat, "/workflow open_it").await;
    assert_eq!(
        browser.opened.load(Ordering::SeqCst),
        0,
        "backend untouched"
    );
    let seen = gate.seen.lock().clone();
    assert_eq!(seen.len(), 1, "the replay asked the control gate");
    assert_eq!(
        (seen[0].run_id, seen[0].action.as_str()),
        (run, "browser_open")
    );
}
