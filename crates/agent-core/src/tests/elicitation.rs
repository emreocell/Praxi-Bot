use protocol::elicitation::{ElicitationField, ElicitationFieldKind};
use tool_registry::elicitation::{Decision, Form};

use super::*;

pub(crate) const NAME: &str = "mcp:calendar/elicit";
const CANARY: &str = "elicitation-canary-9413";

struct FormTool {
    spec: ToolSpec,
    answers: Arc<Mutex<Vec<&'static str>>>,
}

#[async_trait]
impl Tool for FormTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolContext, _: Value) -> Result<ToolOutput, ToolError> {
        let field = ElicitationField {
            name: "region".into(),
            label: "Region".into(),
            description: None,
            required: true,
            blocked: false,
            kind: ElicitationFieldKind::String,
            min_length: Some(2),
            max_length: Some(64),
            minimum: None,
            maximum: None,
            format: None,
            options: Vec::new(),
        };
        let decision = ctx
            .elicitation
            .as_ref()
            .unwrap()
            .ask(Form {
                server: "calendar".into(),
                message: "Choose a region".into(),
                fields: vec![
                    field.clone(),
                    ElicitationField {
                        name: "password".into(),
                        blocked: true,
                        required: false,
                        ..field
                    },
                ],
            })
            .await;
        let result = match decision {
            Decision::Accept(values) => {
                assert!(values.get("region").and_then(Value::as_str) == Some(CANARY));
                assert_eq!(values.len(), 1);
                "accepted"
            }
            Decision::Decline => "declined",
            Decision::Cancel => "cancelled",
            Decision::Parked(_) => "parked",
        };
        self.answers.lock().push(result);
        Ok(ToolOutput::text(result))
    }
}

fn rig(env: &Env, config: LoopConfig) -> (AgentLoop, Arc<Fake>, Arc<Mutex<Vec<&'static str>>>) {
    // The test focuses on in-tool owner decisions, after the tool was admitted.
    let fake = Fake::new(vec![calls(&[("form", NAME, json!({}))]), text("Finished")]);
    let lp = make_loop(&env.storage, env.bot, &fake, &[], profile(""), config);
    let answers = register_form(&lp);
    (lp, fake, answers)
}

pub(crate) fn register_form(lp: &AgentLoop) -> Arc<Mutex<Vec<&'static str>>> {
    let answers = Arc::new(Mutex::new(Vec::new()));
    lp.tool_registry().register(Arc::new(FormTool {
        spec: ToolSpec {
            name: NAME.into(),
            description: "Ask for a region".into(),
            input_schema: json!({"type": "object"}),
            platforms: vec![Platform::All],
            effect: Effect::Read,
            category: PermissionCategory::Accounts,
            content: ContentClass::none(),
        },
        answers: Arc::clone(&answers),
    }));
    answers
}

#[tokio::test]
async fn elicitation_values_are_single_use_and_absent_from_model_and_journal() {
    use crate::ElicitationAnswerError::{Gone, Invalid};

    let env = setup();
    let (lp, fake, answers) = rig(&env, cfg(Policy::full()));
    let mut rx = lp.subscribe();
    let run = lp
        .run_turn(env.chat, "Help me set up the calendar")
        .unwrap();
    let Event::ElicitationRequested(card) = until(&mut rx, |event| {
        matches!(event, Event::ElicitationRequested(_))
    })
    .await
    else {
        unreachable!()
    };
    assert_eq!(lp.runtime().state(run).unwrap(), RunState::WaitingApproval);
    assert!(lp.answer_question(card.call_id, CANARY).is_err());

    for content in [
        json!({}),
        json!({"region": 42}),
        json!({"region": "x"}),
        json!({"region": "test", "password": CANARY}),
        json!({"region": "test", "blocked-1": CANARY}),
    ] {
        assert_eq!(
            lp.submit_elicitation(card.call_id, content.as_object().unwrap().clone()),
            Err(Invalid),
        );
    }
    lp.submit_elicitation(
        card.call_id,
        [("region".into(), json!(CANARY))].into_iter().collect(),
    )
    .unwrap();
    assert_eq!(lp.close_elicitation(card.call_id, true), Err(Gone));
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(*answers.lock(), ["accepted"]);
    let request = serde_json::to_string(&fake.request(1).messages).unwrap();
    let journal = std::fs::read_to_string(storage::event_log::event_log_path(
        env.storage.data_dir(),
        run,
    ))
    .unwrap();
    let audit = format!("{:?}", env.storage.list_audit(env.user).unwrap());
    let steps = format!("{:?}", env.storage.list_steps(run).unwrap());
    for surface in [request, journal, audit, steps] {
        assert!(!surface.contains(CANARY));
    }
}

#[tokio::test]
async fn elicitation_expiry_is_cancel_and_messaging_chat_receives_a_separate_link_event() {
    let env = setup();
    let mut config = cfg(Policy::full());
    config.approval_timeout = Duration::from_millis(200);
    let (lp, _, answers) = rig(&env, config);
    let mut rx = lp.subscribe();
    let Delivery::NewTurn(run) = lp
        .send_user_message_from(
            env.chat,
            "Set up the calendar",
            MessageMode::Queue,
            "telegram_owner",
        )
        .unwrap()
    else {
        unreachable!()
    };
    let Event::ElicitationLink(link) =
        until(&mut rx, |event| matches!(event, Event::ElicitationLink(_))).await
    else {
        unreachable!()
    };
    assert_eq!(link.chat_id, env.chat);
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    assert_eq!(*answers.lock(), ["cancelled"]);
}
