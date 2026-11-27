//! Owner decisions for MCP forms. Request bodies must never be logged or echoed.

use agent_core::ElicitationAnswerError;
use axum::{
    body::Bytes,
    extract::{Extension, Path, State},
    http::StatusCode,
};
use protocol::ToolCallId;
use serde::Deserialize;
use serde_json::{Map, Value};
use utoipa::ToSchema;

use crate::{
    AppState,
    error::{ApiError, ErrorBody},
    live_view::Actor,
};

#[derive(Deserialize, ToSchema)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ElicitationDecision {
    Accept { content: Map<String, Value> },
    Decline,
}

fn require_owner(actor: &Actor) -> Result<(), ApiError> {
    if actor.is_owner() && actor.decisions {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "owner_key_required",
            "only the owner can answer MCP forms",
        ))
    }
}

fn decision_result(result: Result<(), ElicitationAnswerError>) -> Result<StatusCode, ApiError> {
    match result {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(ElicitationAnswerError::Gone) => Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "the MCP form has closed",
        )),
        Err(ElicitationAnswerError::Invalid) => Err(ApiError::bad_request(
            "the answer does not match the form or contains a suspected credential",
        )),
    }
}

#[utoipa::path(
    post, path = "/v1/elicitations/{call_id}", tag = "runs",
    security(("bearer" = [])),
    params(("call_id" = uuid::Uuid, Path, description = "The MCP form ID")),
    request_body = ElicitationDecision,
    responses(
        (status = 204, description = "Decision delivered"),
        (status = 400, body = ErrorBody),
        (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody),
    ),
)]
pub(crate) async fn decide_elicitation(
    State(state): State<AppState>,
    Extension(actor): Extension<Actor>,
    Path(call_id): Path<uuid::Uuid>,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    require_owner(&actor)?;
    let decision = serde_json::from_slice::<ElicitationDecision>(&body)
        .map_err(|_| ApiError::bad_request("invalid MCP form decision"))?;
    let call_id = ToolCallId(call_id);
    decision_result(match decision {
        ElicitationDecision::Accept { content } => {
            state.agent().submit_elicitation(call_id, content)
        }
        ElicitationDecision::Decline => state.agent().close_elicitation(call_id, true),
    })
}

#[utoipa::path(
    post, path = "/v1/elicitations/{call_id}/cancel", tag = "runs",
    security(("bearer" = [])),
    params(("call_id" = uuid::Uuid, Path, description = "The MCP form ID")),
    responses(
        (status = 204, description = "Form cancelled"),
        (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody),
    ),
)]
pub(crate) async fn cancel_elicitation(
    State(state): State<AppState>,
    Extension(actor): Extension<Actor>,
    Path(call_id): Path<uuid::Uuid>,
) -> Result<StatusCode, ApiError> {
    require_owner(&actor)?;
    decision_result(state.agent().close_elicitation(ToolCallId(call_id), false))
}
