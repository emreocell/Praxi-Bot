//! Deterministic long-run harness for missions (ADR `docs/design/missions-adr.md`, section 16).
//! It lives here, next to the production `AgentSessionRunner`, rather than in `crates/evals`.
//!
//! Real: the agent loop, the permission engine, the taint gate, the ledger (SQLite), the
//! supervisor and the production session runner. Fake: the model (a *reactive* provider that
//! reads the brief in each request instead of replaying a fixed queue, so scenarios stay
//! stable when the order of sessions changes) and the browser. No network, no sleeping, no
//! credentials.
//!
//! Scenarios (ADR numbering where one exists):
//! * "continues without asking": the model keeps asking "shall I continue?" and the task still
//!   gets done, across several sessions (the 40-step wall is gone);
//! * A3 "parks a blocked item and continues": a CAPTCHA blocks one site, the rest go on, the
//!   owner answers once and the mission finishes;
//! * A9 "stops at budget": the session budget, the model-call budget and the deadline end the
//!   mission as `blocked` with the exact numbers;
//! * A5 "restart": the daemon restarts in the middle, nothing is done twice;
//! * A12 "stalled": a model that does nothing ends in `blocked(stalled)` after bounded cost.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};
use std::time::Duration;

use crate::mission_host::AgentSessionRunner;
use agent_core::{AgentLoop, BotProfile, Guards, LoopConfig, Providers};
use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use missions::supervisor::{Clock, SessionRunner, Supervisor, SupervisorConfig};
use missions::{Budgets, ItemStatus, MissionState, MissionStore, NewMission};
use parking_lot::Mutex;
use permission_engine::Policy;
use protocol::{BotId, ChatId, PermissionCategory, UserId};
use provider_catalog::{
    ChatChunk, ChatProvider, ChatRequest, FinishReason, MessageRole, ProviderError, ToolCall, Usage,
};
use serde_json::{Value, json};
use storage::{Bot, Chat, Storage, User, now_ms};
use task_runtime::RetryPolicy;
use tokio_util::sync::CancellationToken;
use tool_registry::{
    ContentClass, ContentPart, Effect, Platform, Tool, ToolContext, ToolError, ToolOutput,
    ToolRegistry, ToolSpec,
};

// ---------- the reactive model ----------

/// How the fake model behaves.
#[derive(Default, Clone)]
struct Behaviour {
    /// Sites the model plans when it finds an empty ledger.
    plan: Vec<String>,
    /// Sites that show a CAPTCHA.
    captcha: HashSet<String>,
    /// After each item the model ends its turn with "shall I continue?".
    ask_after_each_item: bool,
    /// The model plans nothing and does nothing (a stalled mission).
    idle: bool,
}

struct ReactiveModel {
    behaviour: Behaviour,
    requests: Mutex<Vec<ChatRequest>>,
    calls: AtomicU32,
}

impl ReactiveModel {
    fn new(behaviour: Behaviour) -> Arc<Self> {
        Arc::new(Self {
            behaviour,
            requests: Mutex::new(Vec::new()),
            calls: AtomicU32::new(0),
        })
    }

    fn texts(&self) -> Vec<String> {
        self.requests
            .lock()
            .iter()
            .flat_map(|r| r.messages.iter())
            .filter_map(|m| m.content.as_str().map(str::to_owned))
            .collect()
    }
}

fn tool_call(id: usize, name: &str, args: &Value) -> ChatChunk {
    ChatChunk::ToolCall(ToolCall {
        id: format!("call-{id}"),
        name: name.to_owned(),
        arguments: args.to_string(),
        provider_extra: None,
    })
}

fn open_keys(brief: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut in_open = false;
    for line in brief.lines() {
        if line.starts_with("NEXT ITEMS") || line.starts_with("ANSWERED BY THE OWNER") {
            in_open = true;
            continue;
        }
        if line.trim().is_empty() {
            in_open = false;
        }
        if in_open
            && let Some(rest) = line.strip_prefix("- \"")
            && let Some(end) = rest.find('"')
        {
            keys.push(rest[..end].to_owned());
        }
    }
    keys
}

/// What the model has already done in this session, read back from the conversation.
#[derive(Default)]
struct Session {
    planned: bool,
    /// Keys of the items this session planned itself (its brief predates them).
    plan_keys: Vec<String>,
    completed: bool,
    doing: Option<String>,
    handled: HashSet<String>,
    handover_for: HashSet<String>,
}

fn session_of(req: &ChatRequest) -> Session {
    let mut s = Session::default();
    for m in &req.messages {
        if m.role == MessageRole::Assistant {
            for c in &m.tool_calls {
                let args: Value = serde_json::from_str(&c.arguments).unwrap_or_default();
                match c.name.as_str() {
                    "mission_plan" => {
                        s.planned = true;
                        s.plan_keys = args["items"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|i| i["key"].as_str().map(str::to_owned))
                            .collect();
                    }
                    "mission_complete" => s.completed = true,
                    "browser_handover" => {
                        if let Some(k) = s.doing.clone() {
                            s.handover_for.insert(k.clone());
                            s.handled.insert(k);
                        }
                    }
                    "mission_item" => {
                        let key = args["key"].as_str().unwrap_or_default().to_owned();
                        match args["status"].as_str() {
                            Some("doing") => s.doing = Some(key),
                            Some("done" | "failed" | "blocked" | "skipped") => {
                                s.handled.insert(key);
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    s
}

impl ReactiveModel {
    fn decide(&self, req: &ChatRequest) -> Vec<ChatChunk> {
        let text = |t: &str| vec![ChatChunk::TextDelta(t.to_owned())];
        // The closing, tool-less call after a safety limit.
        if req.tools.is_empty() {
            return text("Stopped by a safety limit; the progress is in the ledger.");
        }
        let Some(brief) = req
            .messages
            .iter()
            .rev()
            .filter(|m| m.role == MessageRole::User)
            .filter_map(|m| m.content.as_str())
            .find(|t| t.starts_with("MISSION BRIEF"))
        else {
            return text("Done: nothing to do.");
        };
        if self.behaviour.idle {
            return text("I looked at the mission and have nothing to report.");
        }
        let s = session_of(req);
        if brief.contains("THE LEDGER IS EMPTY") && !s.planned {
            let items: Vec<Value> = self
                .behaviour
                .plan
                .iter()
                .map(|k| json!({"key": k, "title": format!("Sign up on {k}"), "kind": "site"}))
                .collect();
            return vec![tool_call(1, "mission_plan", &json!({"items": items}))];
        }
        // The brief predates a plan made in this very session: use the plan itself then.
        let mut candidates = open_keys(brief);
        if candidates.is_empty() {
            candidates.clone_from(&s.plan_keys);
        }
        let next = candidates.into_iter().find(|k| !s.handled.contains(k));
        let Some(key) = next else {
            if brief.contains("NO OPEN ITEM IS LEFT") && !s.completed {
                return vec![tool_call(
                    1,
                    "mission_complete",
                    &json!({"summary": "all items are done"}),
                )];
            }
            return text("Done: everything I could reach is recorded in the ledger.");
        };
        if s.doing.as_deref() != Some(&key) {
            return vec![tool_call(
                1,
                "mission_item",
                &json!({"key": key, "status": "doing"}),
            )];
        }
        if self.behaviour.captcha.contains(&key) && !s.handover_for.contains(&key) {
            return vec![tool_call(
                1,
                "browser_handover",
                &json!({"kind": "captcha"}),
            )];
        }
        vec![tool_call(
            1,
            "mission_item",
            &json!({"key": key, "status": "done", "result": "account created"}),
        )]
    }
}

#[async_trait]
impl ChatProvider for ReactiveModel {
    fn id(&self) -> &str {
        "fake"
    }

    async fn stream(
        &self,
        req: ChatRequest,
    ) -> Result<BoxStream<'static, Result<ChatChunk, ProviderError>>, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().push(req.clone());
        let mut chunks = self.decide(&req);
        // A model that asks "shall I continue?" instead of going on, right after it
        // finished an item: the text replaces the next tool call.
        // The loop appends its own action-ledger note after tool results; look past it.
        let last = req.messages.iter().rev().find(|m| {
            !(m.role == MessageRole::User
                && m.content
                    .as_str()
                    .is_some_and(|t| t.starts_with("[action ledger")))
        });
        if self.behaviour.ask_after_each_item
            && let Some(last) = last
            && last.role == MessageRole::Tool
            && last
                .content
                .as_str()
                .is_some_and(|t| t.contains("is now done"))
            && !req.tools.is_empty()
        {
            chunks = vec![ChatChunk::TextDelta(
                "Registered that site. Shall I continue with the next one?".to_owned(),
            )];
        }
        let finish = if chunks.iter().any(|c| matches!(c, ChatChunk::ToolCall(_))) {
            FinishReason::ToolCalls
        } else {
            FinishReason::Stop
        };
        chunks.push(ChatChunk::Usage(Usage {
            prompt_tokens: 100,
            completion_tokens: 20,
            cached_tokens: None,
            cache_write_tokens: None,
        }));
        chunks.push(ChatChunk::Done {
            finish_reason: finish,
        });
        Ok(futures::stream::iter(chunks.into_iter().map(Ok)).boxed())
    }
}

// ---------- the fake browser ----------

/// `browser_handover`: always reports a CAPTCHA on a sign-up page (a Meta tool, like the real one).
struct CaptchaTool(ToolSpec);

#[async_trait]
impl Tool for CaptchaTool {
    fn spec(&self) -> &ToolSpec {
        &self.0
    }
    async fn call(&self, _: &ToolContext, _: Value) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput {
            content: vec![ContentPart::Json {
                data: builtin_tools::handover::browser_handover_card(
                    builtin_tools::handover::HandoverKind::Captcha,
                    "https://signup.example",
                ),
            }],
            spill: None,
            exposure: None,
        })
    }
}

fn captcha_tool() -> Arc<dyn Tool> {
    Arc::new(CaptchaTool(ToolSpec {
        name: "browser_handover".into(),
        description: "hand a CAPTCHA to the owner".into(),
        input_schema: json!({"type": "object"}),
        effect: Effect::Read,
        category: PermissionCategory::Meta,
        platforms: vec![Platform::All],
        content: ContentClass::none(),
    }))
}

// ---------- the world ----------

struct ManualClock(AtomicI64);

impl Clock for ManualClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

struct World {
    store: Arc<MissionStore>,
    storage: Arc<Storage>,
    supervisor: Arc<Supervisor>,
    model: Arc<ReactiveModel>,
    clock: Arc<ManualClock>,
    user: UserId,
    bot: BotId,
    chat: ChatId,
}

const FULL_BOT: &str = "[bot]\nid = \"t\"\nname = \"Ada\"\npersona = \"You are Ada.\"\n[models]\nexecutor = \"fake/m\"\n[permissions]\npreset = \"full\"\n";

/// Build (or rebuild, for a restart) everything over the data directory `dir`.
/// `seed` creates the user, bot and chat the first time.
fn world(
    dir: &Path,
    behaviour: &Behaviour,
    max_steps: u32,
    seed: Option<(UserId, BotId, ChatId)>,
) -> World {
    let storage = Arc::new(Storage::open(dir).unwrap());
    let (user, bot, chat) = seed.unwrap_or_else(|| {
        let (user, bot, chat) = (UserId::new(), BotId::new(), ChatId::new());
        storage
            .insert_user(&User {
                id: user,
                name: "owner".into(),
                created_at_ms: now_ms(),
            })
            .unwrap();
        storage
            .insert_bot(&Bot {
                id: bot,
                user_id: user,
                name: "Ada".into(),
                // The scripted model answers every request: no page reader call.
                config: json!({"browser": {"reader": "off"}}),
                created_at_ms: now_ms(),
            })
            .unwrap();
        storage
            .insert_chat(&Chat {
                id: chat,
                user_id: user,
                bot_id: bot,
                title: "Mission".into(),
                created_at_ms: now_ms(),
            })
            .unwrap();
        storage
            .set_bot_spend_limits(user, bot, "{}", &json!({}))
            .unwrap();
        (user, bot, chat)
    });
    let model = ReactiveModel::new(behaviour.clone());
    let registry = ToolRegistry::new();
    registry.register(captcha_tool());
    for tool in builtin_tools::meta::tools() {
        registry.register(tool);
    }
    let config = LoopConfig {
        legacy_tool_surface: true,
        global_policy: Policy::full(),
        model_retry: RetryPolicy {
            max_attempts: 1,
            initial_backoff_ms: 1,
            multiplier: 1,
            max_backoff_ms: 1,
        },
        guards: Guards {
            max_steps,
            ..Guards::default()
        },
        ..LoopConfig::default()
    };
    let agent = AgentLoop::new(
        Arc::clone(&storage),
        registry,
        Providers::single(Arc::clone(&model) as Arc<dyn ChatProvider>),
        config,
    );
    agent.add_profile(bot, BotProfile::from_toml_str(FULL_BOT).unwrap());
    let store = Arc::new(MissionStore::open(dir.join(missions::DB_FILE)).unwrap());
    let clock = Arc::new(ManualClock(AtomicI64::new(now_ms())));
    assert_eq!(
        agent.enable_missions(&store, &(Arc::clone(&clock) as Arc<dyn Clock>)),
        missions::tools::MISSION_TOOL_NAMES.len()
    );
    let runner: Arc<dyn SessionRunner> =
        Arc::new(AgentSessionRunner::new(agent, Arc::clone(&storage)));
    let supervisor = Arc::new(Supervisor::new(
        Arc::clone(&store),
        runner,
        Arc::clone(&clock) as Arc<dyn Clock>,
        SupervisorConfig {
            session_gap: Duration::ZERO,
            poll: Duration::from_millis(10),
        },
        CancellationToken::new(),
    ));
    World {
        store,
        storage,
        supervisor,
        model,
        clock,
        user,
        bot,
        chat,
    }
}

fn sites(n: usize) -> Vec<String> {
    (1..=n).map(|i| format!("site-{i}.example")).collect()
}

fn start(w: &World, goal: &str, budgets: Budgets) -> String {
    w.store
        .create(
            &NewMission {
                user_id: w.user.to_string(),
                bot_id: w.bot.to_string(),
                chat_id: w.chat.to_string(),
                title: "Backlinks".into(),
                goal_text: goal.into(),
                target_done: None,
                budgets,
                items: Vec::new(),
            },
            w.clock.now_ms(),
        )
        .unwrap()
        .id
}

const GOAL: &str = "Get backlinks for my site: find sites that give backlinks, register on each with your own email, move on to the next site.";

// ---------- scenarios ----------

#[tokio::test(flavor = "multi_thread")]
async fn continues_without_asking_across_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let behaviour = Behaviour {
        plan: sites(6),
        ask_after_each_item: true,
        ..Behaviour::default()
    };
    // Six model calls per session: far fewer than the items, so the mission needs several
    // sessions where the old loop stopped at its step limit.
    let w = world(dir.path(), &behaviour, 6, None);
    let id = start(&w, GOAL, Budgets::default());
    let end = w.supervisor.drive(&id).await.unwrap();
    assert_eq!(end, MissionState::Done, "{:?}", w.store.get(&id).unwrap());
    let m = w.store.get(&id).unwrap().unwrap();
    let p = w.store.progress(&id).unwrap();
    assert_eq!((p.total, p.done), (6, 6));
    assert!(
        m.sessions > 1,
        "session chaining past the step wall: {} sessions",
        m.sessions
    );
    assert_eq!(m.digest.as_ref().unwrap().outcome, "done");
    // The model asked "shall I continue?" and the runtime answered, never a human.
    let texts = w.model.texts();
    assert!(texts.iter().any(|t| t.contains("Shall I continue")));
    assert!(
        texts
            .iter()
            .any(|t| t.contains("Automatic reply from the Praxi runtime, not typed by the owner")),
        "the runtime answers the question"
    );
    // The chat holds the mission briefs and the final digest, and nothing the owner typed after the start.
    let owner_msgs = w
        .storage
        .list_messages(w.chat)
        .unwrap()
        .into_iter()
        .filter(|m| m.role == "user" && !m.content.starts_with("MISSION BRIEF"))
        .count();
    assert_eq!(owner_msgs, 0);
    let last = w.storage.list_messages(w.chat).unwrap().pop().unwrap();
    assert!(
        last.content.contains("Mission finished"),
        "{}",
        last.content
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_captcha_parks_one_item_the_rest_continue_and_the_owner_answers_once() {
    let dir = tempfile::tempdir().unwrap();
    let behaviour = Behaviour {
        plan: sites(5),
        captcha: ["site-3.example".to_owned()].into(),
        ..Behaviour::default()
    };
    let w = world(dir.path(), &behaviour, 40, None);
    let id = start(&w, GOAL, Budgets::default());
    let end = w.supervisor.drive(&id).await.unwrap();
    assert_eq!(end, MissionState::Blocked);
    let p = w.store.progress(&id).unwrap();
    assert_eq!(
        (p.done, p.blocked),
        (4, 1),
        "four sites registered, one parked"
    );
    let blocked = w.store.items(&id, Some(ItemStatus::Blocked)).unwrap();
    assert_eq!(blocked[0].key, "site-3.example");
    assert!(
        blocked[0]
            .blocked_reason
            .as_deref()
            .unwrap()
            .contains("CAPTCHA")
    );
    let m = w.store.get(&id).unwrap().unwrap();
    assert_eq!(
        m.digest.as_ref().unwrap().blocked.len(),
        1,
        "one entry for the owner"
    );
    // No handover card was shown: nobody waited.
    let audit = w.storage.list_audit(w.user).unwrap();
    assert!(!audit.iter().any(|a| a.action == "handover.requested"));
    assert!(audit.iter().any(|a| a.action == "handover.parked"));

    // The owner answers once ("I solved it"): the item goes back and the mission finishes.
    let released = w
        .supervisor
        .answer_blocked(
            &id,
            &[(
                "site-3.example".into(),
                "I solved the CAPTCHA, try again".into(),
            )],
        )
        .unwrap();
    assert_eq!(released, 1);
    let behaviour_after = Behaviour {
        captcha: HashSet::new(),
        ..behaviour
    };
    let w2 = world(
        dir.path(),
        &behaviour_after,
        40,
        Some((w.user, w.bot, w.chat)),
    );
    assert_eq!(w2.supervisor.drive(&id).await.unwrap(), MissionState::Done);
    assert_eq!(w2.store.progress(&id).unwrap().done, 5);
}

#[tokio::test(flavor = "multi_thread")]
async fn stops_at_the_session_budget_with_the_numbers_and_the_digest() {
    let dir = tempfile::tempdir().unwrap();
    let behaviour = Behaviour {
        plan: sites(40),
        ..Behaviour::default()
    };
    // Four calls per session: plan, then doing/done pairs.
    let w = world(dir.path(), &behaviour, 4, None);
    let budgets = Budgets {
        max_sessions: 3,
        ..Budgets::default()
    };
    let id = start(&w, GOAL, budgets);
    assert_eq!(
        w.supervisor.drive(&id).await.unwrap(),
        MissionState::Blocked
    );
    let m = w.store.get(&id).unwrap().unwrap();
    assert_eq!(m.sessions, 3);
    assert_eq!(
        m.state_reason.as_deref(),
        Some("session budget used up: 3 of 3 sessions")
    );
    let d = m.digest.unwrap();
    assert_eq!(d.outcome, "blocked");
    assert!(
        d.progress.done > 0,
        "work was done before the budget ended it"
    );
    // The model cannot raise the budget: the tools have no such argument and the store only
    // accepts the owner's call.
    let before = w.store.get(&id).unwrap().unwrap().budgets;
    assert_eq!(before.max_sessions, 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn stops_at_the_model_call_budget_and_at_the_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let behaviour = Behaviour {
        plan: sites(40),
        ..Behaviour::default()
    };
    let w = world(dir.path(), &behaviour, 4, None);
    let id = start(
        &w,
        GOAL,
        Budgets {
            max_model_steps: 9,
            ..Budgets::default()
        },
    );
    assert_eq!(
        w.supervisor.drive(&id).await.unwrap(),
        MissionState::Blocked
    );
    let reason = w.store.get(&id).unwrap().unwrap().state_reason.unwrap();
    assert!(reason.starts_with("model-call budget used up"), "{reason}");

    let dir = tempfile::tempdir().unwrap();
    let w = world(dir.path(), &behaviour, 4, None);
    let id = start(
        &w,
        GOAL,
        Budgets {
            max_minutes: 30,
            ..Budgets::default()
        },
    );
    assert_eq!(
        w.supervisor.step(&id).await.unwrap(),
        missions::supervisor::Step::Continue
    );
    w.clock.0.fetch_add(31 * 60_000, Ordering::SeqCst);
    assert_eq!(
        w.supervisor.drive(&id).await.unwrap(),
        MissionState::Blocked
    );
    let reason = w.store.get(&id).unwrap().unwrap().state_reason.unwrap();
    assert!(
        reason.contains("deadline reached after 30 minutes"),
        "{reason}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restart_in_the_middle_loses_nothing_and_repeats_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let behaviour = Behaviour {
        plan: sites(8),
        ..Behaviour::default()
    };
    let w = world(dir.path(), &behaviour, 5, None);
    let id = start(&w, GOAL, Budgets::default());
    // Session 1 plans and does a little, then the daemon "dies".
    w.supervisor.step(&id).await.unwrap();
    let done_before = w.store.progress(&id).unwrap().done;
    assert!(done_before > 0 && done_before < 8);
    let seed = (w.user, w.bot, w.chat);
    // A session that was in flight when the process died: its row never ended.
    let lost = w.store.begin_session(&id, w.clock.now_ms()).unwrap();
    w.store.set_session_run(lost, "run-lost").unwrap();
    w.store.set_lease(&id, Some("run-lost")).unwrap();
    drop(w);

    let w2 = world(dir.path(), &behaviour, 5, Some(seed));
    assert_eq!(
        w2.store.recover_interrupted(now_ms()).unwrap(),
        vec!["run-lost".to_owned()]
    );
    assert_eq!(w2.supervisor.drive(&id).await.unwrap(), MissionState::Done);
    let p = w2.store.progress(&id).unwrap();
    assert_eq!(
        (p.total, p.done),
        (8, 8),
        "no site registered twice, none missed"
    );
    let done_events = w2
        .store
        .events(&id, 0, 1000)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "item" && e.data["to"] == "done")
        .count();
    assert_eq!(done_events, 8, "each item reached `done` exactly once");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_model_that_does_nothing_ends_blocked_after_bounded_cost() {
    let dir = tempfile::tempdir().unwrap();
    let behaviour = Behaviour {
        idle: true,
        ..Behaviour::default()
    };
    let w = world(dir.path(), &behaviour, 40, None);
    let id = start(&w, GOAL, Budgets::default());
    assert_eq!(
        w.supervisor.drive(&id).await.unwrap(),
        MissionState::Blocked
    );
    let m = w.store.get(&id).unwrap().unwrap();
    assert_eq!(m.sessions, m.budgets.max_stalled_sessions);
    assert!(m.state_reason.unwrap().contains("no progress"));
    assert!(
        w.model.calls.load(Ordering::SeqCst) <= 4 * 3,
        "a handful of calls, not a runaway"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bot_below_the_full_level_does_not_run_unattended() {
    let dir = tempfile::tempdir().unwrap();
    let behaviour = Behaviour {
        plan: sites(2),
        ..Behaviour::default()
    };
    let w = world(dir.path(), &behaviour, 40, None);
    // The owner moves the bot to a lower level after the mission started.
    let agent_profile = FULL_BOT.replace("preset = \"full\"", "preset = \"approve-destructive\"");
    // A fresh loop with the lower-level profile over the same ledger.
    let storage = Arc::clone(&w.storage);
    let model = ReactiveModel::new(behaviour);
    let agent = AgentLoop::new(
        Arc::clone(&storage),
        ToolRegistry::new(),
        Providers::single(model as Arc<dyn ChatProvider>),
        LoopConfig {
            legacy_tool_surface: true,
            ..LoopConfig::default()
        },
    );
    agent.add_profile(w.bot, BotProfile::from_toml_str(&agent_profile).unwrap());
    let runner: Arc<dyn SessionRunner> = Arc::new(AgentSessionRunner::new(agent, storage));
    let sup = Supervisor::new(
        Arc::clone(&w.store),
        runner,
        Arc::clone(&w.clock) as Arc<dyn Clock>,
        SupervisorConfig {
            session_gap: Duration::ZERO,
            poll: Duration::from_millis(10),
        },
        CancellationToken::new(),
    );
    let id = start(&w, GOAL, Budgets::default());
    assert_eq!(sup.drive(&id).await.unwrap(), MissionState::Blocked);
    let reason = w.store.get(&id).unwrap().unwrap().state_reason.unwrap();
    assert!(reason.contains("Full level"), "{reason}");
    assert_eq!(
        w.store.get(&id).unwrap().unwrap().sessions,
        0,
        "no session was started"
    );
}
