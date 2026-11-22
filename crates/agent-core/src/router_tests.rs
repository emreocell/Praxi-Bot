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
    TextWithUsage(&'static str),
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
            Some(Rep::TextWithUsage(t)) => Ok(futures::stream::iter(vec![
                Ok(ChatChunk::TextDelta(t.to_owned())),
                Ok(ChatChunk::Usage(provider_catalog::Usage {
                    prompt_tokens: 7,
                    completion_tokens: 3,
                    cached_tokens: Some(1),
                    cache_write_tokens: None,
                })),
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
    (lp, store.for_bot(&env.bot.to_string()).unwrap())
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
    let events = drain(&mut rx);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Event::ToolCallRequested(_))),
        "no tool call"
    );
    let d = decided(&events);
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].level, "K0");
    assert_eq!(d[0].route["kind"], "command");
    assert_eq!(d[0].route["name"], "stop");
    // The route cancels during planning. No new step may be started after
    // that request; its decision remains durable in the event journal.
    assert_eq!(env.storage.list_steps(run).unwrap().len(), 0);
    let journal: Vec<_> = storage::read_event_log(env.dir.path(), run)
        .unwrap()
        .into_iter()
        .map(|envelope| envelope.event)
        .collect();
    let logged = decided(&journal);
    assert_eq!(logged.len(), 1);
    assert_eq!(logged[0].level, "K0");
    assert_eq!(logged[0].route["kind"], "command");
    assert_eq!(logged[0].route["name"], "stop");
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
async fn saved_workflow_is_tainted_before_its_first_egress_step() {
    let env = setup();
    let script = Script::new(vec![]);
    let search = Arc::new(crate::tests::with_class(
        "web_search",
        Effect::Read,
        protocol::PermissionCategory::Browser,
        tool_registry::ContentClass::untrusted_egress(),
        "sent",
    ));
    let (lp, store) = with_store(&env, make(&env, &script, &[as_tool(&search)], ""));
    store
        .save(&wf_tool_step("leak", "web_search", "send saved data"))
        .unwrap();
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "/workflow leak").unwrap();
    let event = crate::tests::until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await;
    let Event::ApprovalNeeded(card) = event else {
        unreachable!()
    };
    assert_eq!(card.tool_name, "web_search");
    assert_eq!(search.count.load(Ordering::SeqCst), 0);
    lp.approve(card.call_id, crate::ApprovalScope::Once, env.user)
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(search.count.load(Ordering::SeqCst), 1);
}

/// The replay note in `exec_workflow` taints the run before the first step,
/// so a routed shell step asks even when the egress waiver is on. Without the
/// note the step runs in a clean run.
#[tokio::test]
async fn a_routed_shell_step_asks_under_the_waiver() {
    let env = setup();
    let script = Script::new(vec![]);
    let shell = Arc::new(crate::tests::with_class(
        "shell_run",
        Effect::Destructive,
        protocol::PermissionCategory::Shell,
        builtin_tools::classify("shell_run").unwrap(),
        "ran",
    ));
    let (lp, store) = with_store(
        &env,
        make(
            &env,
            &script,
            &[as_tool(&shell)],
            "[permissions]
allow_egress_after_reads = true
",
        ),
    );
    store
        .save(&wf_tool_step("run_it", "shell_run", "run a command"))
        .unwrap();
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "/workflow run_it").unwrap();
    let event = crate::tests::until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await;
    let Event::ApprovalNeeded(card) = event else {
        unreachable!()
    };
    assert_eq!(card.tool_name, "shell_run");
    assert_eq!(shell.count.load(Ordering::SeqCst), 0);
    lp.cancel(run).unwrap();
}

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
        .find(|m| m.role == MessageRole::User)
        .unwrap()
        .content
        .as_str()
        .unwrap()
        .to_owned();
    // SEC-10 keeps the user text as the original request. The trusted handover
    // instruction is a system message; the partial state is an untrusted
    // user-role data block.
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
    // The partial state is a labelled User-role data block, not an Assistant
    // turn, and it is the last message of the request (no prefill).
    let last = req.messages.last().unwrap();
    assert_eq!(last.role, MessageRole::User, "{req:?}");
    assert!(
        req.messages
            .iter()
            .all(|m| m.role != MessageRole::Assistant)
    );
    let partial_text = last.content.as_str().unwrap();
    assert!(partial_text.starts_with("[router data: UNTRUSTED"));
    assert!(
        partial_text.contains("Pick the right folder"),
        "{partial_text}"
    );
    assert!(
        partial_text.contains("router.workflow_replay"),
        "{partial_text}"
    );
    assert!(last.tool_calls.is_empty());
    assert!(last.tool_call_id.is_none());
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
    // The text must overlap the skill (here: "github"): with an unrelated text
    // the classifier only sees the corpus when it is tiny, and the bundled
    // skill set grows over time.
    let (_, state) = turn(&lp, env.chat, "frobnicate the gizmo on github").await;
    assert_eq!(state, RunState::Succeeded);
    let d = decided(&drain(&mut rx));
    assert_eq!(d[0].route["kind"], "skill_hint", "{d:?}");
    let sys = script.requests_for("fake/m")[0].messages[0]
        .content
        .as_str()
        .unwrap()
        .to_owned();
    // The index in the system prompt is name-ordered and never carries the
    // per-turn suggestion; that goes into the latest user message as a hint.
    assert!(sys.contains("- github-basics:"), "{sys}");
    assert!(!sys.contains("- [suggested]"), "{sys}");
    let req = &script.requests_for("fake/m")[0];
    let user = req
        .messages
        .iter()
        .rev()
        .find(|m| m.role == provider_catalog::MessageRole::User)
        .unwrap();
    let user = user.content.as_str().unwrap();
    assert!(user.starts_with("frobnicate the gizmo on github"), "{user}");
    assert!(user.contains("[loop hint, not written by the user: Suggested skills"));
    assert!(user.contains("github-basics"));
}

// ---------- budget, switches, embedder ----------

#[tokio::test(start_paused = true)]
async fn a_slow_ladder_never_blocks_the_turn_beyond_its_budget() {
    let env = setup();
    let t = Arc::new(tool("t_a", Effect::Read, "a"));
    let script = Script::new(vec![
        ("fake/c", vec![Rep::Hang]),
        ("fake/m", vec![Rep::Text("answered")]),
    ]);
    let mut config = cfg(Policy::full());
    let budget = Duration::from_millis(60);
    config.router_budget = budget;
    config.ladder.k3_timeout = Duration::from_secs(30);
    let lp = loop_with(
        &env,
        Providers::single(Arc::clone(&script) as Arc<dyn ChatProvider>),
        &[as_tool(&t)],
        CLASSIFIER,
        config,
    );
    let mut rx = lp.subscribe();
    let started = tokio::time::Instant::now();
    let (_, state) = turn(&lp, env.chat, "frobnicate the gizmo").await;
    assert_eq!(state, RunState::Succeeded);
    // Wall time includes filesystem/journal work and runner contention. The
    // virtual clock advances only while waiting for the hanging classifier;
    // without the outer router budget it reaches K3's 30-second deadline.
    let elapsed = started.elapsed();
    assert!(elapsed >= budget && elapsed <= budget + Duration::from_millis(1));
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
    let audit = env.storage.list_audit(env.user).unwrap();
    let router_calls = audit
        .iter()
        .filter(|r| r.action == "usage" && r.detail["purpose"] == "router")
        .collect::<Vec<_>>();
    assert_eq!(router_calls.len(), 1);
    assert_eq!(router_calls[0].detail["calls"], 1);
    assert!(router_calls[0].detail["tokens"].is_null());
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
    store
        .for_bot(&env.bot.to_string())
        .unwrap()
        .save(&wf)
        .unwrap();

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

#[tokio::test]
async fn similar_requests_cannot_route_recorded_recipients_urls_paths_or_commands() {
    for (description, changed) in [
        (
            "send report to bob@example.test",
            "send report to carol@example.test",
        ),
        (
            "open https://trusted.test/invoice",
            "open https://different.test/invoice",
        ),
        (
            "write report to /private/bob.txt",
            "write report to /private/carol.txt",
        ),
        (
            "run curl https://trusted.test",
            "run curl https://different.test",
        ),
    ] {
        let env = setup();
        let log = Arc::new(tool("t_log", Effect::Write, "sent"));
        let script = Script::new(vec![
            (
                "fake/c",
                vec![Rep::Text(
                    r#"{"route":"workflow","name":"recorded","confidence":1.0,"params":{"recorded_secret_1":"different"}}"#,
                )],
            ),
            ("fake/m", vec![Rep::Text("ask the user")]),
        ]);
        let (lp, store) = with_store(&env, make(&env, &script, &[as_tool(&log)], CLASSIFIER));
        let mut wf = wf_tool_step("recorded", "t_log", description);
        wf.params.push(workflow_replay::Param {
            name: "recorded_secret_1".into(),
            ty: workflow_replay::ParamType::String,
            default: None,
            description: "fresh human input".into(),
        });
        wf.steps = vec![Step::Tool(ToolStep {
            tool: "t_log".into(),
            input: json!({"value":"{recorded_secret_1}"}),
        })];
        store.save(&wf).unwrap();
        let mut events = lp.subscribe();
        let (_, state) = turn(&lp, env.chat, changed).await;
        assert_eq!(state, RunState::Succeeded);
        assert_eq!(log.count.load(Ordering::SeqCst), 0, "{changed}");
        assert!(
            decided(&drain(&mut events))
                .iter()
                .all(|d| d.route["kind"] != "workflow")
        );
        let output = lp
            .tool_registry()
            .get("workflow_run")
            .unwrap()
            .call(
                &tool_registry::ToolContext {
                    bot_id: Some(env.bot.to_string()),
                    ..Default::default()
                },
                json!({"name":"recorded","params":{"recorded_secret_1":"different"}}),
            )
            .await;
        assert!(output.is_err(), "model tool must require human entry");
    }
}

struct SelectedSkillAssignment {
    bot: String,
    allowed: &'static str,
}

impl skills::AssignmentSource for SelectedSkillAssignment {
    fn assignment(&self, bot_id: &str) -> Result<Option<skills::Assignment>, String> {
        if bot_id == self.bot {
            Ok(Some(skills::Assignment {
                mode: skills::AssignmentMode::Selected,
                names: std::iter::once(self.allowed.to_owned()).collect(),
            }))
        } else {
            Ok(None)
        }
    }
    fn generation(&self) -> u64 {
        1
    }
}

fn write_router_skill(dir: &std::path::Path, name: &str, desc: &str, body: &str) {
    let root = dir.join(name);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {desc}\n---\n{body}\n"),
    )
    .unwrap();
}

#[tokio::test]
async fn router_candidates_excludes_unassigned_skills_for_selected_bot() {
    let env = setup();
    write_router_skill(
        &env.dir.path().join("skills"),
        "assigned-skill",
        "Assigned skill description",
        "ASSIGNED_BODY",
    );
    write_router_skill(
        &env.dir.path().join("skills"),
        "unassigned-skill",
        "Secret hidden description",
        "UNASSIGNED_BODY",
    );
    write_router_skill(
        &env.dir
            .path()
            .join("bots")
            .join(env.bot.to_string())
            .join("skills"),
        "bot-learned",
        "Bot own learned skill",
        "OWN_BODY",
    );

    let index = SkillIndex::load_roots(skills::SkillRoots::from_data_dir(env.dir.path()))
        .with_assignments(Arc::new(SelectedSkillAssignment {
            bot: env.bot.to_string(),
            allowed: "assigned-skill",
        }));

    let script = Script::new(vec![
        (
            "fake/c",
            vec![Rep::Text(
                r#"{"route":"skill","name":"assigned-skill","confidence":0.8}"#,
            )],
        ),
        ("fake/m", vec![Rep::Text("ok")]),
    ]);
    let lp = make(&env, &script, &[], CLASSIFIER).with_skills(index);

    let cands = lp.sh.router_candidates(&profile(CLASSIFIER), env.bot);
    let skill_names: Vec<&str> = cands
        .iter()
        .filter(|c| c.kind == agent_router::CandidateKind::Skill)
        .map(|c| c.name.as_str())
        .collect();
    assert!(skill_names.contains(&"assigned-skill"), "{skill_names:?}");
    assert!(skill_names.contains(&"bot-learned"), "{skill_names:?}");
    assert!(
        !skill_names.contains(&"unassigned-skill"),
        "{skill_names:?}"
    );
    assert!(
        !skill_names.contains(&"custom-dropdowns"),
        "{skill_names:?}"
    );

    let (_, state) = turn(&lp, env.chat, "frobnicate the gizmo").await;
    assert_eq!(state, RunState::Succeeded);
    let classifier_req = &script.requests_for("fake/c")[0];
    let classifier_payload = serde_json::to_string(&classifier_req.messages).unwrap();
    assert!(
        classifier_payload.contains("assigned-skill"),
        "{classifier_payload}"
    );
    assert!(
        classifier_payload.contains("bot-learned"),
        "{classifier_payload}"
    );
    assert!(
        !classifier_payload.contains("unassigned-skill"),
        "{classifier_payload}"
    );
    assert!(
        !classifier_payload.contains("Secret hidden description"),
        "{classifier_payload}"
    );
    assert!(
        !classifier_payload.contains("skill custom-dropdowns"),
        "{classifier_payload}"
    );
}

#[tokio::test]
async fn workflow_replay_enforces_selected_bot_skill_assignment() {
    let env = setup();
    write_router_skill(
        &env.dir.path().join("skills"),
        "assigned-skill",
        "Assigned skill description",
        "ASSIGNED_SECRET_BODY",
    );
    write_router_skill(
        &env.dir.path().join("skills"),
        "unassigned-skill",
        "Hidden skill description",
        "UNASSIGNED_SECRET_BODY",
    );

    let index = SkillIndex::load_roots(skills::SkillRoots::from_data_dir(env.dir.path()))
        .with_assignments(Arc::new(SelectedSkillAssignment {
            bot: env.bot.to_string(),
            allowed: "assigned-skill",
        }));

    let store = WorkflowStore::new(env.dir.path());
    // Workflows are bot-scoped: save into this bot's folder.
    let bot_store = store.for_bot(&env.bot.to_string()).unwrap();
    let mut denied_wf = Workflow::new("read_unassigned");
    denied_wf.description = "read an unassigned skill".into();
    denied_wf.steps = vec![Step::Tool(ToolStep {
        tool: "skill_read".into(),
        input: json!({ "name": "unassigned-skill" }),
    })];
    bot_store.save(&denied_wf).unwrap();

    let mut allowed_wf = Workflow::new("read_assigned");
    allowed_wf.description = "read an assigned skill".into();
    allowed_wf.steps = vec![Step::Tool(ToolStep {
        tool: "skill_read".into(),
        input: json!({ "name": "assigned-skill" }),
    })];
    bot_store.save(&allowed_wf).unwrap();

    let script = Script::new(vec![("fake/m", vec![Rep::Text("fallback")])]);
    let lp = make(&env, &script, &[], "")
        .with_skills(index)
        .with_workflows(
            store,
            Handles {
                desktop: None,
                browser: None,
                tools: ToolRegistry::new(),
            },
        );

    let (denied_run, denied_state) = turn(&lp, env.chat, "/workflow read_unassigned").await;
    assert_eq!(denied_state, RunState::Succeeded);
    let denied_steps = env.storage.list_steps(denied_run).unwrap();
    let wf_step = denied_steps
        .iter()
        .find(|s| s.name == "workflow")
        .expect("workflow step");
    let wf_out = wf_step.output.as_ref().unwrap();
    assert_eq!(wf_out["completed"], false, "{wf_out}");
    let summary = wf_out["summary"].as_str().unwrap_or("");
    assert!(!summary.contains("UNASSIGNED_SECRET_BODY"), "{wf_out}");

    let (allowed_run, allowed_state) = turn(&lp, env.chat, "/workflow read_assigned").await;
    assert_eq!(allowed_state, RunState::Succeeded);
    let allowed_steps = env.storage.list_steps(allowed_run).unwrap();
    let ok_step = allowed_steps
        .iter()
        .find(|s| s.name == "workflow")
        .expect("workflow step");
    let ok_out = ok_step.output.as_ref().unwrap();
    assert_eq!(ok_out["completed"], true, "{ok_out}");
    assert_eq!(ok_out["steps_replayed"], 1, "{ok_out}");
}

#[tokio::test]
async fn ledger_classifier_keeps_reported_usage_separate_from_executor() {
    let env = setup();
    let t = Arc::new(tool("t_a", Effect::Read, "a"));
    let script = Script::new(vec![
        ("fake/c", vec![Rep::TextWithUsage(AGENT_ROUTE)]),
        ("fake/m", vec![Rep::Text("answered")]),
    ]);
    let lp = make(&env, &script, &[as_tool(&t)], CLASSIFIER);
    let (run, state) = turn(&lp, env.chat, "frobnicate the gizmo").await;
    assert_eq!(state, RunState::Succeeded);
    assert_eq!(script.n("fake/c"), 1);
    let rows = env.storage.list_audit(env.user).unwrap();
    let usage = rows
        .iter()
        .filter(|r| r.action == "usage" && r.detail["purpose"] == "router")
        .collect::<Vec<_>>();
    assert_eq!(usage.len(), 1);
    assert_eq!(usage[0].detail["prompt_tokens"], 7);
    assert_eq!(usage[0].detail["cached_tokens"], 1);
    let summary = rows
        .iter()
        .find(|r| r.action == "run.usage_summary" && r.run_id == Some(run))
        .unwrap();
    assert_eq!(summary.detail["router"]["calls"], 1);
    assert_eq!(summary.detail["executor"]["calls"], 1);
}

struct SmallBatchEmbedder(AtomicUsize);

#[async_trait]
impl Embedder for SmallBatchEmbedder {
    fn max_batch_size(&self) -> usize {
        1
    }
    async fn embed(&self, texts: &[String], _: EmbedTask) -> Result<Vec<Vec<f32>>, BackendError> {
        assert_eq!(texts.len(), 1);
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(vec![vec![1.0, 0.0, 0.0]])
    }
}

#[tokio::test]
async fn workflows_from_one_bot_are_not_candidates_for_another_bot() {
    let env = setup();
    let script = Script::new(vec![]);
    let (lp, store) = with_store(&env, make(&env, &script, &[], ""));
    store
        .save(&wf_tool_step("private", "hidden", "private automation"))
        .unwrap();
    let profile = profile("");
    let other = protocol::BotId::new();
    assert!(
        lp.sh
            .router_candidates(&profile, other)
            .iter()
            .all(|c| c.name != "private")
    );
    assert!(
        lp.sh
            .router_candidates(&profile, env.bot)
            .iter()
            .any(|c| c.name == "private")
    );
}

#[tokio::test]
async fn ledger_embeddings_count_transport_batches_and_query_calls() {
    let env = setup();
    let tools = (0..3)
        .map(|i| Arc::new(tool(&format!("t_{i}"), Effect::Read, "value")))
        .collect::<Vec<_>>();
    let script = Script::new(vec![("fake/m", vec![Rep::Text("one"), Rep::Text("two")])]);
    let embedder = Arc::new(SmallBatchEmbedder(AtomicUsize::new(0)));
    let lp = loop_with(
        &env,
        Providers::single(script as Arc<dyn ChatProvider>)
            .with_embedder(Arc::clone(&embedder) as Arc<dyn Embedder>),
        &tools.iter().map(as_tool).collect::<Vec<_>>(),
        "",
        cfg(Policy::full()),
    );
    let (first, state) = turn(&lp, env.chat, "frobnicate the gizmo").await;
    assert_eq!(state, RunState::Succeeded);
    lp.flush_background().await;
    assert_eq!(embedder.0.load(Ordering::SeqCst), 3);
    let (second, state) = turn(&lp, env.chat, "different uncached request").await;
    assert_eq!(state, RunState::Succeeded);
    lp.flush_background().await;
    assert_eq!(embedder.0.load(Ordering::SeqCst), 4);
    let audit = env.storage.list_audit(env.user).unwrap();
    let rows = audit
        .iter()
        .filter(|r| r.action == "usage" && r.detail["purpose"] == "embedding")
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 4);
    assert!(
        rows.iter()
            .all(|r| r.detail["tokens"].is_null() && r.detail["latency_ms"].is_u64())
    );
    for (run, count) in [(first, 3), (second, 1)] {
        let summary = audit
            .iter()
            .find(|r| r.action == "run.usage_summary" && r.run_id == Some(run))
            .unwrap();
        assert_eq!(summary.detail["embedding"]["calls"], count);
    }
}
