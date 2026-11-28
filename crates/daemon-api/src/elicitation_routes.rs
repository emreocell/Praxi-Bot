//! `/v1/elicitations/{call_id}`: the owner's app answers an MCP server's form
//! (elicitation, see `agent-core`'s `elicitation.rs`).
//!
//! * Owner only: classified in `owner.rs`, refused to every paired peer
//!   (`PEER_DENIED_FILL_PREFIXES`) and refused again here for a non-local actor.
//! * The answer is checked against the form the server sent (types, required
//!   fields, choices, bounds) before it is handed to the waiting server. It is
//!   never put in an event, the journal, the audit log, an error message or the
//!   response; `Debug` of the body prints `<redacted>`.
//! * A form is answered once; one that does not fit keeps waiting.

use axum::Json;
use axum::extract::{Extension, Path, State};
use protocol::ToolCallId;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tool_registry::ElicitationAnswer;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::AppState;
use crate::error::{ApiError, ErrorBody};
use crate::fill_routes::audit;
use crate::live_view::Actor;

/// What the owner does with the form.
#[derive(Debug, Clone, Copy, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ElicitationAction {
    /// Send `content` to the server.
    Accept,
    /// Refuse to give the information.
    Decline,
}

/// Body of `POST /v1/elicitations/{call_id}`.
#[derive(Deserialize, ToSchema)]
pub struct AnswerElicitation {
    pub action: ElicitationAction,
    /// Field name to value (`accept` only). Write-only: no route returns it.
    #[serde(default)]
    #[schema(value_type = Object)]
    pub content: Map<String, Value>,
}

impl std::fmt::Debug for AnswerElicitation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AnswerElicitation(<redacted>)")
    }
}

/// How a form ended. Never carries an answer.
#[derive(Debug, Serialize, ToSchema)]
pub struct ElicitationOutcome {
    /// The form that was answered.
    pub call_id: Uuid,
    /// `accepted`, `declined` or `cancelled`.
    pub outcome: String,
}

fn gone() -> ApiError {
    ApiError::new(
        axum::http::StatusCode::NOT_FOUND,
        "elicitation_gone",
        "this form is not waiting any more",
    )
}

fn call_id(raw: &str) -> Result<ToolCallId, ApiError> {
    Uuid::parse_str(raw)
        .map(ToolCallId)
        .map_err(|_| ApiError::bad_request("call_id must be a UUID"))
}

fn local(actor: &Actor) -> Result<(), ApiError> {
    if actor.is_owner() {
        Ok(())
    } else {
        Err(ApiError::forbidden(
            "a server's form is answered by the local owner at this computer",
        ))
    }
}

/// Parse the body without echoing any of it: serde's messages quote the scalar.
fn parse_body(body: &[u8]) -> Result<AnswerElicitation, ApiError> {
    serde_json::from_slice(body).map_err(|_| {
        ApiError::bad_request(
            "the body must be {\"action\": \"accept\" | \"decline\", \"content\": {...}}",
        )
    })
}

fn finish(
    s: &AppState,
    actor: &Actor,
    id: ToolCallId,
    answer: ElicitationAnswer,
) -> Result<Json<ElicitationOutcome>, ApiError> {
    let outcome = match answer {
        ElicitationAnswer::Accept(_) => "accepted",
        ElicitationAnswer::Decline => "declined",
        ElicitationAnswer::Cancel | ElicitationAnswer::Parked(_) => "cancelled",
    };
    s.agent()
        .finish_elicitation(id, answer)
        .map_err(|e| match e {
            agent_core::ElicitationRefusal::Gone => gone(),
            agent_core::ElicitationRefusal::Invalid(why) => ApiError::bad_request(why),
        })?;
    // The audit row names the outcome only: never a value.
    audit(s, actor, "elicitation.answer", &json!({"outcome": outcome}))?;
    Ok(Json(ElicitationOutcome {
        call_id: id.0,
        outcome: outcome.into(),
    }))
}

/// Answer an MCP server's form: `accept` with the values, or `decline`.
#[utoipa::path(post, path = "/v1/elicitations/{call_id}", tag = "elicitations",
    security(("bearer" = [])),
    params(("call_id" = Uuid, Path, description = "Id from the elicitation_requested event")),
    request_body = AnswerElicitation,
    responses((status = 200, body = ElicitationOutcome),
              (status = 400, body = ErrorBody, description = "the answer does not fit the form; it keeps waiting"),
              (status = 403, body = ErrorBody, description = "not the local owner"),
              (status = 404, body = ErrorBody, description = "code elicitation_gone: not waiting any more")))]
pub(crate) async fn answer_elicitation(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    Path(call): Path<String>,
    body: axum::body::Bytes,
) -> Result<Json<ElicitationOutcome>, ApiError> {
    local(&actor)?;
    let id = call_id(&call)?;
    let input = parse_body(&body)?;
    let answer = match input.action {
        ElicitationAction::Accept => ElicitationAnswer::Accept(input.content),
        ElicitationAction::Decline => ElicitationAnswer::Decline,
    };
    finish(&s, &actor, id, answer)
}

/// Dismiss an MCP server's form: the server learns `cancel`.
#[utoipa::path(post, path = "/v1/elicitations/{call_id}/cancel", tag = "elicitations",
    security(("bearer" = [])),
    params(("call_id" = Uuid, Path, description = "Id from the elicitation_requested event")),
    responses((status = 200, body = ElicitationOutcome),
              (status = 403, body = ErrorBody, description = "not the local owner"),
              (status = 404, body = ErrorBody, description = "code elicitation_gone: not waiting any more")))]
pub(crate) async fn cancel_elicitation(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    Path(call): Path<String>,
) -> Result<Json<ElicitationOutcome>, ApiError> {
    local(&actor)?;
    let id = call_id(&call)?;
    finish(&s, &actor, id, ElicitationAnswer::Cancel)
}
