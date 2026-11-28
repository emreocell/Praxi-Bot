//! `/v1/elicitations/{call_id}`: the owner answers an MCP server's form. The
//! answer is checked against the form and reaches the server only.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::http::{Method, StatusCode};
use parking_lot::Mutex;
use protocol::event::{ElicitationField, ElicitationFieldType};
use serde_json::{Value, json};
use tool_registry::{
    ElicitationAnswer, ElicitationRequest, Elicitor, Tool, ToolContext, ToolError, ToolOutput,
    ToolSpec,
};

use super::{T, auth, call, call_token_only, make_chat, send, send_msg, setup, wait_state};

const CANARY: &str = "canary-elicit-route-5512";

/// Stands in for an MCP tool whose server asks for a count and a plan.
struct Form {
    spec: ToolSpec,
    elicitor: Arc<dyn Elicitor>,
    seen: Arc<Mutex<Vec<ElicitationAnswer>>>,
}

fn field(name: &str, kind: ElicitationFieldType) -> ElicitationField {
    ElicitationField {
        name: name.into(),
        title: None,
        description: None,
        kind,
        required: true,
        options: Vec::new(),
        minimum: None,
        maximum: None,
        min_length: None,
        max_length: None,
        format: None,
        default: None,
        blocked: false,
    }
}

#[async_trait]
impl Tool for Form {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }
    async fn call(&self, ctx: &ToolContext, _: Value) -> Result<ToolOutput, ToolError> {
        let mut seats = field("seats", ElicitationFieldType::Integer);
        seats.minimum = Some(1.0);
        seats.maximum = Some(9.0);
        let answer = self
            .elicitor
            .elicit(
                &ctx.run_id,
                ElicitationRequest {
                    server: "booking".into(),
                    message: "How many seats?".into(),
                    fields: vec![seats, field("code", ElicitationFieldType::String)],
                },
            )
            .await;
        self.seen.lock().push(answer);
        Ok(ToolOutput::text("form done"))
    }
}

fn rig() -> (T, Arc<Mutex<Vec<ElicitationAnswer>>>) {
    let t = setup();
    let seen = Arc::new(Mutex::new(Vec::new()));
    t.agent.tool_registry().register(Arc::new(Form {
        spec: ToolSpec {
            name: "booking_form".into(),
            description: "asks for input".into(),
            input_schema: json!({"type": "object"}),
            effect: tool_registry::Effect::Read,
            category: tool_registry::PermissionCategory::Meta,
            platforms: vec![tool_registry::Platform::All],
            content: tool_registry::ContentClass::none(),
        },
        elicitor: t.agent.elicitor(),
        seen: Arc::clone(&seen),
    }));
    (t, seen)
}

async fn open_form(t: &T) -> (String, String) {
    let (_bot, chat) = make_chat(t).await;
    // A form is only for a run the owner started in the app (`local_ui` plus the owner key).
    let (key, value) = auth();
    let started = send(
        &t.app,
        Method::POST,
        &format!("/v1/chats/{chat}/messages"),
        Some(json!({"text": "call:booking_form {}", "mode": "queue"})),
        &[(key, &value), ("x-praxi-message-source", "local_ui")],
    )
    .await;
    assert_eq!(started.status, StatusCode::ACCEPTED);
    let run = started.json()["run_id"].as_str().unwrap().to_owned();
    let id: uuid::Uuid = run.parse().unwrap();
    for _ in 0..200 {
        let log = storage::read_event_log(t.state.storage().data_dir(), protocol::RunId(id))
            .unwrap_or_default();
        for env in log.iter().rev() {
            if let protocol::Event::ElicitationRequested(q) = &env.event {
                return (run, q.call_id.to_string());
            }
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("no form was opened");
}

#[tokio::test]
async fn the_owner_answers_and_only_a_fitting_answer_reaches_the_server() {
    let (t, seen) = rig();
    let (run, call_id) = open_form(&t).await;
    wait_state(&t, &run, "waiting_approval").await;
    let path = format!("/v1/elicitations/{call_id}");
    let good = json!({"action": "accept", "content": {"seats": 2, "code": CANARY}});

    // A token holder is not the owner.
    let denied = call_token_only(&t, Method::POST, &path, Some(good.clone())).await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);

    // Answers that do not fit are refused without quoting them; the form waits.
    for bad in [
        json!({"action": "accept", "content": {"seats": 2}}),
        json!({"action": "accept", "content": {"seats": 10, "code": CANARY}}),
        json!({"action": "accept", "content": {"seats": "2", "code": CANARY}}),
        json!({"action": "accept", "content": {"seats": 2, "code": 5}}),
        json!({"action": "accept", "content": {"seats": 2, "code": "x", CANARY: 1}}),
        json!({"action": CANARY}),
    ] {
        let r = call(&t, Method::POST, &path, Some(bad.clone())).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}");
        assert!(!String::from_utf8_lossy(&r.body).contains(CANARY));
    }
    assert!(seen.lock().is_empty());

    let ok = call(&t, Method::POST, &path, Some(good.clone())).await;
    assert_eq!(ok.status, StatusCode::OK);
    assert_eq!(ok.json()["outcome"], "accepted");
    assert!(!String::from_utf8_lossy(&ok.body).contains(CANARY));
    wait_state(&t, &run, "succeeded").await;
    let mut expected = serde_json::Map::new();
    expected.insert("seats".into(), json!(2));
    expected.insert("code".into(), json!(CANARY));
    assert_eq!(*seen.lock(), [ElicitationAnswer::Accept(expected)]);

    // Single use, and nothing stored about the run carries the value.
    let again = call(&t, Method::POST, &path, Some(good)).await;
    assert_eq!(again.status, StatusCode::NOT_FOUND);
    assert_eq!(again.json()["error"]["code"], "elicitation_gone");
    let id: uuid::Uuid = run.parse().unwrap();
    let stored = format!(
        "{:?}{:?}{:?}",
        storage::read_event_log(t.state.storage().data_dir(), protocol::RunId(id)).unwrap(),
        t.state.storage().list_audit(t.state.user_id()).unwrap(),
        t.state.storage().list_steps(protocol::RunId(id)).unwrap()
    );
    assert!(!stored.contains(CANARY));
    assert!(stored.contains("elicitation.answer"));
}

#[tokio::test]
async fn decline_and_cancel_reach_the_server() {
    for (path_end, body, want) in [
        (
            "",
            Some(json!({"action": "decline"})),
            ElicitationAnswer::Decline,
        ),
        ("/cancel", None, ElicitationAnswer::Cancel),
    ] {
        let (t, seen) = rig();
        let (run, call_id) = open_form(&t).await;
        let r = call(
            &t,
            Method::POST,
            &format!("/v1/elicitations/{call_id}{path_end}"),
            body,
        )
        .await;
        assert_eq!(r.status, StatusCode::OK);
        wait_state(&t, &run, "succeeded").await;
        assert_eq!(*seen.lock(), [want]);
    }
}

/// Guard: a paired device never reaches the routes, and they are owner-only.
#[test]
fn elicitation_routes_are_owner_only_and_closed_to_peers() {
    for p in ["/v1/elicitations/x", "/v1/elicitations/x/cancel"] {
        assert!(crate::fill_routes::is_fill_path(p), "{p}");
    }
    assert!(crate::owner::is_owner_route("POST", "/v1/elicitations/abc"));
    assert!(crate::owner::is_owner_route(
        "POST",
        "/v1/elicitations/abc/cancel"
    ));
}

/// A message that is not the owner's own app (a bearer-only client, a channel,
/// a paired device) starts a run that is declined at once: no form, nothing to answer.
#[tokio::test]
async fn a_message_that_is_not_from_the_owners_app_gets_no_form() {
    let (t, seen) = rig();
    let (_bot, chat) = make_chat(&t).await;
    let run = send_msg(&t, &chat, "call:booking_form {}", "queue").await["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    wait_state(&t, &run, "succeeded").await;
    assert_eq!(*seen.lock(), [ElicitationAnswer::Decline]);
    let id: uuid::Uuid = run.parse().unwrap();
    let log = storage::read_event_log(t.state.storage().data_dir(), protocol::RunId(id)).unwrap();
    assert!(
        log.iter()
            .all(|e| !matches!(e.event, protocol::Event::ElicitationRequested(_)))
    );
    let audit = format!(
        "{:?}",
        t.state.storage().list_audit(t.state.user_id()).unwrap()
    );
    assert!(audit.contains("elicitation.refused"));
}
