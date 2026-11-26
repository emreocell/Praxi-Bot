//! A mission session is an ordinary run with a code-built brief as its first message.
//! These tests pin what the loop does differently for it: it offers the `mission_*`
//! tools (and only there), starts untrusted, parks a CAPTCHA item instead of waiting for
//! the owner, and allows the owner exactly one batch of questions.

use std::sync::Arc;

use async_trait::async_trait;
use missions::supervisor::{Clock, SystemClock};
use missions::{Budgets, ItemStatus, MissionStore, NewItem, NewMission};
use permission_engine::Policy;
use protocol::PermissionCategory;
use serde_json::{Value, json};
use task_runtime::RunState;
use tool_registry::{
    ContentClass, ContentPart, Effect, Platform, Tool, ToolContext, ToolError, ToolOutput, ToolSpec,
};

use crate::tests::{Fake, calls, cfg, make_loop, profile, setup, text, without_ledger};

const FULL: &str = "[permissions]\npreset = \"full\"\n";

/// A `browser_handover` that always reports a CAPTCHA on a sign-up page.
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
                    builtin_tools::HandoverKind::Captcha,
                    "https://signup.example",
                ),
            }],
            spill: None,
            exposure: None,
        })
    }
}

fn captcha_tool() -> Arc<CaptchaTool> {
    Arc::new(CaptchaTool(ToolSpec {
        name: "browser_handover".into(),
        description: "hand a CAPTCHA to the owner".into(),
        input_schema: json!({"type": "object"}),
        effect: Effect::Read,
        // As the real tool: a Meta tool, so the loop reads its card.
        category: PermissionCategory::Meta,
        platforms: vec![Platform::All],
        content: ContentClass::none(),
    }))
}

struct World {
    env: crate::tests::Env,
    store: Arc<MissionStore>,
    mission: String,
}

fn world(items: &[&str]) -> World {
    let env = setup();
    let store = Arc::new(MissionStore::open_in_memory().unwrap());
    let m = store
        .create(
            &NewMission {
                user_id: env.user.to_string(),
                bot_id: env.bot.to_string(),
                chat_id: env.chat.to_string(),
                title: "Backlinks".into(),
                goal_text: "Get backlinks for my site".into(),
                target_done: None,
                budgets: Budgets::default(),
                items: items
                    .iter()
                    .map(|k| NewItem {
                        key: (*k).into(),
                        kind: "site".into(),
                        title: format!("Sign up on {k}"),
                        payload: None,
                    })
                    .collect(),
            },
            1,
        )
        .unwrap();
    World {
        env,
        store,
        mission: m.id,
    }
}

fn lp(w: &World, fake: &Arc<Fake>) -> crate::AgentLoop {
    let captcha = captcha_tool();
    let lp = make_loop(
        &w.env.storage,
        w.env.bot,
        fake,
        &[],
        profile(FULL),
        cfg(Policy::full()),
    );
    lp.sh.tools.register(captcha);
    for tool in builtin_tools::meta::tools() {
        lp.sh.tools.register(tool);
    }
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    assert_eq!(
        lp.enable_missions(&w.store, &clock),
        missions::tools::MISSION_TOOL_NAMES.len()
    );
    lp
}

fn tool_names(req: &provider_catalog::ChatRequest) -> Vec<String> {
    req.tools
        .iter()
        .filter_map(|t| t["function"]["name"].as_str().map(str::to_owned))
        .collect()
}

#[tokio::test]
async fn a_session_gets_the_mission_tools_the_brief_and_the_mission_rules() {
    let w = world(&["a.example"]);
    let fake = Fake::new(vec![text("nothing to do")]);
    let lp = lp(&w, &fake);
    let run = lp
        .run_mission_session(w.env.chat, &w.mission, "MISSION BRIEF · session 1")
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    let req = fake.request(0);
    let names = tool_names(&req);
    for t in missions::tools::MISSION_TOOL_NAMES {
        assert!(names.iter().any(|n| n == t), "{t} missing from {names:?}");
    }
    let system = req.messages[0].content.as_str().unwrap();
    assert!(
        system.contains("session of a long-running mission"),
        "{system}"
    );
    assert!(
        system.contains("Finish the task"),
        "autonomy rules apply too"
    );
    let first_user = req
        .messages
        .iter()
        .find(|m| m.role == provider_catalog::MessageRole::User)
        .unwrap();
    assert_eq!(
        first_user.content.as_str(),
        Some("MISSION BRIEF · session 1")
    );
    // The run kind carries the mission, and it counts as the owner's own run (the Full level applies).
    assert!(crate::approval::owner_started_run(
        &w.env.storage,
        run,
        None
    ));
    let run = w.env.storage.get_run(run).unwrap().unwrap();
    assert_eq!(
        missions::mission_of_kind(&run.kind),
        Some(w.mission.as_str())
    );
}

#[tokio::test]
async fn an_ordinary_chat_of_the_same_bot_does_not_get_the_mission_tools() {
    let w = world(&["a.example"]);
    let fake = Fake::new(vec![text("hello")]);
    let lp = lp(&w, &fake);
    let run = lp.run_turn(w.env.chat, "hi").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    let names = tool_names(&fake.request(0));
    assert!(
        !names.iter().any(|n| n.starts_with("mission_")),
        "{names:?}"
    );
    assert!(
        !fake.request(0).messages[0]
            .content
            .as_str()
            .unwrap()
            .contains("long-running mission")
    );
}

#[tokio::test]
async fn a_captcha_parks_only_that_item_and_the_session_carries_on() {
    let w = world(&["a.example", "b.example"]);
    // The model starts on a.example, hits a CAPTCHA, then finishes b.example.
    let fake = Fake::new(vec![
        calls(&[(
            "c0",
            "mission_item",
            json!({"key": "a.example", "status": "doing"}),
        )]),
        calls(&[("c1", "browser_handover", json!({"kind": "captcha"}))]),
        calls(&[(
            "c2",
            "mission_item",
            json!({"key": "b.example", "status": "done", "result": "account created"}),
        )]),
        text("Done: one site registered, one needs the owner."),
    ]);
    let lp = lp(&w, &fake);
    let t0 = std::time::Instant::now();
    let run = lp
        .run_mission_session(w.env.chat, &w.mission, "brief")
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert!(
        t0.elapsed() < std::time::Duration::from_secs(5),
        "nobody waited for the owner"
    );
    let a = w
        .store
        .items(&w.mission, Some(ItemStatus::Blocked))
        .unwrap();
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].key, "a.example");
    assert!(a[0].blocked_reason.as_deref().unwrap().contains("CAPTCHA"));
    assert_eq!(w.store.progress(&w.mission).unwrap().done, 1);
    // The model was told in plain words to move on.
    let tools = crate::tests::tool_messages(&without_ledger(fake.request(2)));
    assert!(
        tools
            .iter()
            .any(|t| t.contains("parked as blocked") && t.contains("Take the next open item")),
        "{tools:?}"
    );
    // No handover card was ever shown.
    let audit = w.env.storage.list_audit(w.env.user).unwrap();
    assert!(audit.iter().any(|a| a.action == "handover.parked"));
    assert!(!audit.iter().any(|a| a.action == "handover.requested"));
}

#[tokio::test]
async fn without_a_single_doing_item_the_model_is_told_to_park_it_itself() {
    let w = world(&["a.example", "b.example"]);
    let fake = Fake::new(vec![
        calls(&[("c1", "browser_handover", json!({"kind": "captcha"}))]),
        text("ok"),
    ]);
    let lp = lp(&w, &fake);
    let run = lp
        .run_mission_session(w.env.chat, &w.mission, "brief")
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(
        w.store
            .items(&w.mission, Some(ItemStatus::Blocked))
            .unwrap()
            .len(),
        0
    );
    let tools = crate::tests::tool_messages(&without_ledger(fake.request(1)));
    assert!(
        tools
            .iter()
            .any(|t| t.contains("Mark the item you are working on as blocked")),
        "{tools:?}"
    );
}

#[tokio::test]
async fn the_owner_is_asked_once_and_later_questions_become_blocked_items() {
    let w = world(&["a.example"]);
    // The one batch is already used (the first session asked).
    assert!(w.store.take_question_slot(&w.mission).unwrap());
    let fake = Fake::new(vec![
        calls(&[(
            "c1",
            "ask_user",
            json!({"question": "Which e-mail should I use?"}),
        )]),
        text("ok, deciding myself"),
    ]);
    let lp = lp(&w, &fake);
    let run = lp
        .run_mission_session(w.env.chat, &w.mission, "brief")
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    let tools = crate::tests::tool_messages(&without_ledger(fake.request(1)));
    assert!(
        tools
            .iter()
            .any(|t| t.contains("already used the one batch of questions")),
        "{tools:?}"
    );
}

#[tokio::test]
async fn mission_tools_refuse_to_work_for_a_run_that_is_not_a_session() {
    let w = world(&["a.example"]);
    let fake = Fake::new(vec![
        calls(&[(
            "c1",
            "mission_item",
            json!({"key": "a.example", "status": "done"}),
        )]),
        text("ok"),
    ]);
    let lp = lp(&w, &fake);
    // An ordinary turn that tries the tool anyway (it is hidden, so the call is unknown).
    let run = lp.run_turn(w.env.chat, "mark it done").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(w.store.progress(&w.mission).unwrap().done, 0);
}
