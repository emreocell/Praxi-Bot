//! MCP elicitation: a tool (standing in for an MCP tool call through the bridge)
//! asks the run's owner through the loop's `Elicitor`. The answer reaches the
//! tool and nothing else.

use std::fmt::Write as _;

use tool_registry::elicitation::FORM_MAX_WAIT;
use tool_registry::{ElicitationAnswer, ElicitationRequest, Elicitor};

use super::*;
use crate::account_register_tests::LogCapture;
use crate::elicitation::{ElicitationInfo, MAX_OPEN, MAX_PER_RUN};
use crate::{Delivery, ElicitationRefusal, MessageMode};
use protocol::event::{ElicitationField, ElicitationFieldType};

const CANARY: &str = "canary-elicit-Tr0ub4dor-9921";

/// What the server would have received.
pub(crate) type Seen = Arc<Mutex<Vec<ElicitationAnswer>>>;

/// How the stand-in server behaves.
#[derive(Clone, Copy, Default)]
struct Behavior {
    /// Forms it asks for, one after the other, in one tool call.
    asks: usize,
    /// The form's field looks like a credential (blocked).
    blocked: bool,
    /// The result repeats the value the server got, like a server that echoes.
    echo: bool,
}

struct FormTool {
    spec: ToolSpec,
    elicitor: Arc<dyn Elicitor>,
    seen: Seen,
    behavior: Behavior,
}

fn city(blocked: bool) -> ElicitationField {
    ElicitationField {
        name: "city".into(),
        title: Some("City".into()),
        description: None,
        kind: ElicitationFieldType::String,
        required: !blocked,
        options: Vec::new(),
        minimum: None,
        maximum: None,
        min_length: None,
        max_length: Some(64),
        format: None,
        default: None,
        blocked,
    }
}

#[async_trait]
impl Tool for FormTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }
    async fn call(&self, ctx: &ToolContext, _: Value) -> Result<ToolOutput, ToolError> {
        let mut results: Vec<String> = Vec::new();
        for _ in 0..self.behavior.asks.max(1) {
            let answer = self
                .elicitor
                .elicit(
                    &ctx.run_id,
                    ElicitationRequest {
                        server: "acme".into(),
                        message: "Which city?".into(),
                        fields: vec![city(self.behavior.blocked)],
                    },
                )
                .await;
            results.push(match &answer {
                ElicitationAnswer::Accept(_) => "accepted".to_owned(),
                ElicitationAnswer::Decline => "declined".to_owned(),
                ElicitationAnswer::Cancel => "cancelled".to_owned(),
                ElicitationAnswer::Parked(text) => text.clone(),
            });
            self.seen.lock().push(answer);
        }
        let mut text = results.join(",");
        if self.behavior.echo {
            let _ = write!(text, " you typed {CANARY}");
        }
        Ok(ToolOutput::text(text))
    }
}

fn register(lp: &AgentLoop, behavior: Behavior) -> Seen {
    let seen = Seen::default();
    lp.sh.tools.register(Arc::new(FormTool {
        spec: ToolSpec {
            name: "acme_login".into(),
            description: "test MCP tool that asks for input".into(),
            input_schema: json!({"type": "object"}),
            effect: Effect::Read,
            category: PermissionCategory::Meta,
            platforms: vec![Platform::All],
            content: ContentClass::none(),
        },
        elicitor: lp.elicitor(),
        seen: Arc::clone(&seen),
        behavior,
    }));
    seen
}

/// A loop with the stand-in server's tool (for the mission test too).
pub(crate) fn register_form(lp: &AgentLoop) -> Seen {
    register(lp, Behavior::default())
}

pub(crate) fn form_call(id: &str) -> Reply {
    calls(&[(id, "acme_login", json!({}))])
}

fn rig(env: &Env, fake: &Arc<Fake>, config: LoopConfig, behavior: Behavior) -> (AgentLoop, Seen) {
    let lp = make_loop(&env.storage, env.bot, fake, &[], profile(""), config);
    let seen = register(&lp, behavior);
    (lp, seen)
}

/// A turn the owner started at the desktop (the message source the secret requests use too).
fn owner_run(lp: &AgentLoop, env: &Env, message: &str) -> protocol::RunId {
    match lp
        .send_user_message_from(env.chat, message, MessageMode::Queue, "owner")
        .unwrap()
    {
        Delivery::NewTurn(run) => run,
        other => panic!("{other:?}"),
    }
}

fn script() -> Arc<Fake> {
    Fake::new(vec![form_call("c1"), text("ok")])
}

fn requested(e: &Event) -> bool {
    matches!(e, Event::ElicitationRequested(_))
}

fn content(city: &str) -> serde_json::Map<String, Value> {
    let mut m = serde_json::Map::new();
    m.insert("city".into(), json!(city));
    m
}

fn audit_text(env: &Env) -> String {
    format!("{:?}", env.storage.list_audit(env.user).unwrap())
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one ordered scenario: run, then scan every sink
async fn the_owner_answers_and_only_the_server_gets_the_value() {
    let log = Arc::new(Mutex::new(String::new()));
    let _guard = tracing::subscriber::set_default(LogCapture(Arc::clone(&log)));
    let env = setup();
    let fake = script();
    let (lp, seen) = rig(&env, &fake, cfg(Policy::full()), Behavior::default());
    let mut rx = lp.subscribe();
    let run = owner_run(&lp, &env, "find a city");
    let Event::ElicitationRequested(card) = until(&mut rx, requested).await else {
        unreachable!()
    };
    assert_eq!(card.server, "acme");
    assert_eq!(card.message, "Which city?");
    assert_eq!(card.fields[0].name, "city");
    assert_eq!(lp.runtime().state(run).unwrap(), RunState::WaitingApproval);

    // An answer that does not fit keeps the form waiting.
    let too_long = "x".repeat(65);
    assert!(matches!(
        lp.finish_elicitation(card.call_id, ElicitationAnswer::Accept(content(&too_long))),
        Err(ElicitationRefusal::Invalid(_))
    ));
    lp.finish_elicitation(card.call_id, ElicitationAnswer::Accept(content(CANARY)))
        .unwrap();
    assert_eq!(
        lp.finish_elicitation(card.call_id, ElicitationAnswer::Decline),
        Err(ElicitationRefusal::Gone),
        "single use"
    );
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);

    assert_eq!(
        *seen.lock(),
        [ElicitationAnswer::Accept(content(CANARY))],
        "the server got the value"
    );
    let msgs = tool_messages(&fake.request(1));
    assert_eq!(msgs, ["accepted"]);

    // No sink the run wrote to contains the value.
    let mut all = serde_json::to_string(&msgs).unwrap();
    while let Ok(i) = rx.try_recv() {
        all.push_str(&serde_json::to_string(&i.envelope.event).unwrap());
    }
    all.push_str(&serde_json::to_string(&read_event_log(env.dir.path(), run).unwrap()).unwrap());
    all.push_str(&audit_text(&env));
    let _ = write!(all, "{:?}", env.storage.list_steps(run).unwrap());
    let _ = write!(all, "{:?}", seen.lock());
    all.push_str(&log.lock());
    assert!(!all.contains(CANARY), "the typed value leaked");
    assert!(all.contains("elicitation.requested") && all.contains("elicitation.finished"));
}

#[tokio::test]
async fn decline_and_cancel_reach_the_server_as_such() {
    for answer in [ElicitationAnswer::Decline, ElicitationAnswer::Cancel] {
        let env = setup();
        let fake = script();
        let (lp, seen) = rig(&env, &fake, cfg(Policy::full()), Behavior::default());
        let mut rx = lp.subscribe();
        let run = owner_run(&lp, &env, "go");
        let Event::ElicitationRequested(card) = until(&mut rx, requested).await else {
            unreachable!()
        };
        lp.finish_elicitation(card.call_id, answer.clone()).unwrap();
        assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
        assert_eq!(*seen.lock(), [answer]);
        assert!(lp.elicitation(card.call_id).is_none());
    }
}

#[tokio::test]
async fn a_caller_cannot_hand_the_server_a_parked_answer() {
    let env = setup();
    let fake = script();
    let (lp, seen) = rig(&env, &fake, cfg(Policy::full()), Behavior::default());
    let mut rx = lp.subscribe();
    let run = owner_run(&lp, &env, "go");
    let Event::ElicitationRequested(card) = until(&mut rx, requested).await else {
        unreachable!()
    };
    lp.finish_elicitation(card.call_id, ElicitationAnswer::Parked("x".into()))
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(*seen.lock(), [ElicitationAnswer::Cancel]);
}

#[tokio::test]
async fn a_form_nobody_answers_expires_and_the_server_gets_cancel() {
    let env = setup();
    let fake = script();
    let mut config = cfg(Policy::full());
    config.approval_timeout = Duration::from_millis(50);
    let (lp, seen) = rig(&env, &fake, config, Behavior::default());
    let mut rx = lp.subscribe();
    let run = owner_run(&lp, &env, "go");
    let Event::ElicitationRequested(card) = until(&mut rx, requested).await else {
        unreachable!()
    };
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(*seen.lock(), [ElicitationAnswer::Cancel]);
    assert_eq!(
        lp.finish_elicitation(card.call_id, ElicitationAnswer::Accept(content(CANARY))),
        Err(ElicitationRefusal::Gone)
    );
}

#[tokio::test]
async fn the_wait_is_capped_even_when_the_approval_timeout_is_zero_or_huge() {
    for timeout in [Duration::ZERO, Duration::from_secs(24 * 3600)] {
        let env = setup();
        let fake = script();
        let mut config = cfg(Policy::full());
        config.approval_timeout = timeout;
        let (lp, _) = rig(&env, &fake, config, Behavior::default());
        let mut rx = lp.subscribe();
        let _run = owner_run(&lp, &env, "go");
        let Event::ElicitationRequested(card) = until(&mut rx, requested).await else {
            unreachable!()
        };
        assert_eq!(
            card.expires_in_secs,
            FORM_MAX_WAIT.as_secs(),
            "{timeout:?}: the card counts down to the hard cap"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn with_no_approval_timeout_a_form_nobody_answers_still_ends_with_cancel() {
    let env = setup();
    let fake = script();
    let mut config = cfg(Policy::full());
    config.approval_timeout = Duration::ZERO;
    let (lp, seen) = rig(&env, &fake, config, Behavior::default());
    let run = owner_run(&lp, &env, "go");
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(*seen.lock(), [ElicitationAnswer::Cancel]);
    assert!(audit_text(&env).contains("expired"));
}

#[tokio::test]
async fn a_run_the_owner_did_not_start_at_the_desktop_is_declined_and_audited_with_no_form() {
    type Start = Box<dyn Fn(&AgentLoop, &Env) -> protocol::RunId>;
    let mut cases: Vec<(String, Start)> = Vec::new();
    for initiator in [InitiatorKind::Schedule, InitiatorKind::Channel] {
        cases.push((
            format!("{initiator:?}"),
            Box::new(move |lp, env| lp.run_turn_as(env.chat, "go", initiator).unwrap()),
        ));
    }
    // A human turn whose message did not come from the owner's own desktop.
    for source in ["telegram", "telegram_owner", "peer_admin", "api"] {
        cases.push((
            source.to_owned(),
            Box::new(move |lp, env| {
                match lp
                    .send_user_message_from(env.chat, "go", MessageMode::Queue, source)
                    .unwrap()
                {
                    Delivery::NewTurn(run) => run,
                    other => panic!("{other:?}"),
                }
            }),
        ));
    }
    for (name, start) in cases {
        let env = setup();
        let fake = script();
        let (lp, seen) = rig(&env, &fake, cfg(Policy::full()), Behavior::default());
        let mut rx = lp.subscribe();
        let run = start(&lp, &env);
        let ended = tokio::time::timeout(Duration::from_secs(30), lp.wait(run))
            .await
            .unwrap_or_else(|_| panic!("{name}: the run did not end"));
        assert_eq!(ended.unwrap(), RunState::Succeeded, "{name}");
        assert_eq!(*seen.lock(), [ElicitationAnswer::Decline], "{name}");
        while let Ok(i) = rx.try_recv() {
            assert!(!requested(&i.envelope.event), "{name}: a form was shown");
        }
        let audit = audit_text(&env);
        assert!(
            audit.contains("elicitation.refused") && audit.contains("desktop"),
            "{name}: {audit}"
        );
    }
}

#[tokio::test]
async fn a_run_may_ask_for_three_forms_and_the_fourth_is_declined_and_audited() {
    assert_eq!(MAX_PER_RUN, 3);
    let env = setup();
    let fake = script();
    let behavior = Behavior {
        asks: 4,
        ..Behavior::default()
    };
    let (lp, seen) = rig(&env, &fake, cfg(Policy::full()), behavior);
    let mut rx = lp.subscribe();
    let run = owner_run(&lp, &env, "go");
    for _ in 0..3 {
        let Event::ElicitationRequested(card) = until(&mut rx, requested).await else {
            unreachable!()
        };
        lp.finish_elicitation(card.call_id, ElicitationAnswer::Decline)
            .unwrap();
    }
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(*seen.lock(), vec![ElicitationAnswer::Decline; 4]);
    while let Ok(i) = rx.try_recv() {
        assert!(!requested(&i.envelope.event), "a fourth form was shown");
    }
    assert!(audit_text(&env).contains("too often"));
}

#[tokio::test]
async fn at_most_sixteen_forms_are_open_at_once() {
    let env = setup();
    let fake = script();
    let (lp, seen) = rig(&env, &fake, cfg(Policy::full()), Behavior::default());
    for _ in 0..MAX_OPEN {
        lp.sh.elicitations.lock().insert(
            protocol::ToolCallId::new(),
            ElicitationInfo { fields: Vec::new() },
        );
    }
    let mut rx = lp.subscribe();
    let run = owner_run(&lp, &env, "go");
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(*seen.lock(), [ElicitationAnswer::Decline]);
    while let Ok(i) = rx.try_recv() {
        assert!(
            !requested(&i.envelope.event),
            "a seventeenth form was shown"
        );
    }
    assert!(audit_text(&env).contains("too many forms"));
    assert_eq!(lp.sh.elicitations.lock().len(), MAX_OPEN);
}

#[tokio::test]
async fn a_blocked_field_is_shown_as_blocked_and_never_takes_a_value() {
    let env = setup();
    let fake = script();
    let behavior = Behavior {
        blocked: true,
        ..Behavior::default()
    };
    let (lp, seen) = rig(&env, &fake, cfg(Policy::full()), behavior);
    let mut rx = lp.subscribe();
    let run = owner_run(&lp, &env, "go");
    let Event::ElicitationRequested(card) = until(&mut rx, requested).await else {
        unreachable!()
    };
    assert!(card.fields[0].blocked);
    assert!(matches!(
        lp.finish_elicitation(card.call_id, ElicitationAnswer::Accept(content("Paris"))),
        Err(ElicitationRefusal::Invalid(_))
    ));
    // Nothing typed: the form may be sent empty, and the server gets no key.
    lp.finish_elicitation(
        card.call_id,
        ElicitationAnswer::Accept(serde_json::Map::new()),
    )
    .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(
        *seen.lock(),
        [ElicitationAnswer::Accept(serde_json::Map::new())]
    );
}

#[tokio::test]
async fn a_value_that_is_itself_a_credential_is_refused() {
    let env = setup();
    let fake = script();
    let (lp, seen) = rig(&env, &fake, cfg(Policy::full()), Behavior::default());
    let mut rx = lp.subscribe();
    let run = owner_run(&lp, &env, "go");
    let Event::ElicitationRequested(card) = until(&mut rx, requested).await else {
        unreachable!()
    };
    let key = "sk-proj-AbCdEfGhIjKlMnOpQrStUvWxYz0123456789";
    let Err(ElicitationRefusal::Invalid(why)) =
        lp.finish_elicitation(card.call_id, ElicitationAnswer::Accept(content(key)))
    else {
        panic!("a key shape was taken");
    };
    assert!(!why.contains(key));
    lp.finish_elicitation(card.call_id, ElicitationAnswer::Cancel)
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(*seen.lock(), [ElicitationAnswer::Cancel]);
}

#[tokio::test]
async fn a_value_the_server_echoes_back_is_masked_for_the_model_in_that_run_only() {
    let env = setup();
    let fake = Fake::new(vec![
        form_call("c1"),
        text("ok"),
        form_call("c2"),
        text("ok again"),
    ]);
    let behavior = Behavior {
        echo: true,
        ..Behavior::default()
    };
    let (lp, seen) = rig(&env, &fake, cfg(Policy::full()), behavior);
    let mut rx = lp.subscribe();
    let run = owner_run(&lp, &env, "go");
    let Event::ElicitationRequested(card) = until(&mut rx, requested).await else {
        unreachable!()
    };
    lp.finish_elicitation(card.call_id, ElicitationAnswer::Accept(content(CANARY)))
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(*seen.lock(), [ElicitationAnswer::Accept(content(CANARY))]);
    let echoed = tool_messages(&fake.request(1)).join("\n");
    assert!(echoed.starts_with("accepted you typed "), "{echoed}");
    assert!(!echoed.contains(CANARY), "the model read the typed value");
    assert!(echoed.contains(secret_store::REDACTED), "{echoed}");
    let mut sinks = serde_json::to_string(&read_event_log(env.dir.path(), run).unwrap()).unwrap();
    let _ = write!(sinks, "{:?}", env.storage.list_steps(run).unwrap());
    assert!(!sinks.contains(CANARY));

    // The mask belongs to that run: the next run has typed nothing yet, and the
    // app-wide redactor never learned the value.
    let run2 = owner_run(&lp, &env, "again");
    let Event::ElicitationRequested(card2) = until(&mut rx, requested).await else {
        unreachable!()
    };
    lp.finish_elicitation(card2.call_id, ElicitationAnswer::Decline)
        .unwrap();
    assert_eq!(lp.wait(run2).await.unwrap(), RunState::Succeeded);
    let again = tool_messages(&fake.request(3)).join("\n");
    assert!(again.contains(CANARY), "{again}");
}

#[tokio::test]
async fn a_tool_call_without_a_live_run_is_declined() {
    let env = setup();
    let fake = script();
    let (lp, _) = rig(&env, &fake, cfg(Policy::full()), Behavior::default());
    let req = ElicitationRequest {
        server: "acme".into(),
        message: "x".into(),
        fields: Vec::new(),
    };
    let elicitor = lp.elicitor();
    assert_eq!(
        elicitor.elicit("not-a-run", req.clone()).await,
        ElicitationAnswer::Decline
    );
    assert_eq!(
        elicitor
            .elicit(&protocol::RunId::new().to_string(), req)
            .await,
        ElicitationAnswer::Decline
    );
}
