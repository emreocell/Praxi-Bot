//! `/v1/handovers/{call_id}/...`: the owner's app answers a `browser_handover` card
//! (a CAPTCHA the owner solved, or a one-time code the owner typed).
//!
//! * Owner only: classified in `owner.rs` (`owner_key_required` without the owner
//!   key), refused to every paired peer and refused again here for a non-local actor.
//! * A one-time code goes from this request body straight into the page
//!   ([`builtin_tools::HandoverHub::type_code`]). It is never stored, never put in
//!   an event, the journal, the audit log, an error message or the answer; the run
//!   only learns `done` or `cancelled`. `Debug` of the body prints `<redacted>`.
//! * An answer that fails (a wrong code format, the page moved, the check is still
//!   there) keeps the request waiting so the owner can try again.
//! * A request is answered once: the first caller claims it.

use axum::Json;
use axum::extract::{Extension, Path, State};
use builtin_tools::{HandoverError, SolvedBy, handover_hub};
use protocol::ToolCallId;
use protocol::event::HandoverKind;
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::AppState;
use crate::error::{ApiError, ErrorBody};
use crate::fill_routes::audit;
use crate::live_view::Actor;
use agent_core::HandoverInfo;

/// Body of `POST /v1/handovers/{call_id}/code`.
#[derive(Deserialize, ToSchema)]
pub struct SubmitHandoverCode {
    /// The one-time code, 4 to 12 letters or digits (write-only: no route returns it).
    pub code: String,
}

impl std::fmt::Debug for SubmitHandoverCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SubmitHandoverCode(<redacted>)")
    }
}

/// How a handover ended. Never carries a code.
#[derive(Debug, Serialize, ToSchema)]
pub struct HandoverAnswer {
    /// The request that was answered.
    pub call_id: Uuid,
    /// `done` or `cancelled`.
    pub outcome: String,
    /// The waiting run was told. `false` when the run stopped or the request
    /// expired in the same instant.
    pub run_notified: bool,
}

fn gone() -> ApiError {
    ApiError::new(
        axum::http::StatusCode::NOT_FOUND,
        "handover_gone",
        "this request is not waiting any more",
    )
}

fn not_ready(e: &HandoverError) -> ApiError {
    ApiError::new(
        axum::http::StatusCode::CONFLICT,
        "handover_not_ready",
        e.to_string(),
    )
}

/// End an answered request. A run waiting on it learns the outcome; a mission's parked CAPTCHA
/// (no run waits) gives its page and item back on `done`, or closes the page and fails the
/// item, and a released mission runs again.
async fn finish(s: &AppState, id: ToolCallId, info: &HandoverInfo, done: bool) -> bool {
    if info.mission.is_none() {
        return s.agent().finish_handover(id, done, s.user_id()).is_ok();
    }
    let Ok(mission) = s.agent().finish_parked_handover(id, done).await else {
        return false;
    };
    if !done {
        return true;
    }
    s.mission_supervisor().is_ok_and(|sup| {
        // `finish_live_handover` already let a blocked or waiting mission run again.
        sup.wake();
        sup.store()
            .get(&mission)
            .ok()
            .flatten()
            .is_some_and(|m| m.state == missions::MissionState::Running)
    })
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
            "a handover is answered by the local owner at this computer",
        ))
    }
}

/// Parse the body without echoing any of it: serde's messages quote the scalar,
/// which here could be the code itself.
fn parse_body(body: &[u8]) -> Result<SubmitHandoverCode, ApiError> {
    serde_json::from_slice(body)
        .map_err(|_| ApiError::bad_request("the body must be {\"code\": \"...\"}"))
}

/// Answer a one-time-code card: the daemon types the code into the page and the
/// run learns `done`. The code is not stored.
#[utoipa::path(post, path = "/v1/handovers/{call_id}/code", tag = "handovers",
    security(("bearer" = [])),
    params(("call_id" = Uuid, Path, description = "Tool call id from the handover_requested event")),
    request_body = SubmitHandoverCode,
    responses((status = 200, body = HandoverAnswer),
              (status = 400, body = ErrorBody, description = "not a code; the request keeps waiting"),
              (status = 403, body = ErrorBody, description = "not the local owner"),
              (status = 404, body = ErrorBody, description = "code handover_gone: not waiting any more"),
              (status = 409, body = ErrorBody, description = "code handover_not_ready: the page cannot take the code now; the request keeps waiting")))]
pub(crate) async fn submit_handover_code(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    Path(call): Path<String>,
    body: axum::body::Bytes,
) -> Result<Json<HandoverAnswer>, ApiError> {
    local(&actor)?;
    let id = call_id(&call)?;
    let input = parse_body(&body)?;
    let info = s.agent().claim_handover(id).ok_or_else(gone)?;
    if info.kind != HandoverKind::Code {
        s.agent().restore_handover(id, info);
        return Err(ApiError::bad_request(
            "this request asks for a CAPTCHA, not a code",
        ));
    }
    let typed = handover_hub()
        .type_code(&info.hub_key(id), &input.code)
        .await;
    drop(input);
    if let Err(e) = typed {
        s.agent().restore_handover(id, info);
        return Err(match e {
            HandoverError::BadCode => ApiError::bad_request(e.to_string()),
            _ => not_ready(&e),
        });
    }
    // The audit row names the kind only: never the code.
    audit(&s, &actor, "handover.code_typed", &json!({"kind": "code"}))?;
    let run_notified = finish(&s, id, &info, true).await;
    Ok(Json(HandoverAnswer {
        call_id: id.0,
        outcome: "done".into(),
        run_notified,
    }))
}

/// The owner says the CAPTCHA is solved: the daemon checks that the page moved on
/// before the run resumes.
#[utoipa::path(post, path = "/v1/handovers/{call_id}/done", tag = "handovers",
    security(("bearer" = [])),
    params(("call_id" = Uuid, Path, description = "Tool call id from the handover_requested event")),
    responses((status = 200, body = HandoverAnswer),
              (status = 403, body = ErrorBody, description = "not the local owner"),
              (status = 404, body = ErrorBody, description = "code handover_gone: not waiting any more"),
              (status = 409, body = ErrorBody, description = "code handover_not_ready: the page still shows the check; the request keeps waiting")))]
pub(crate) async fn finish_handover_done(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    Path(call): Path<String>,
) -> Result<Json<HandoverAnswer>, ApiError> {
    local(&actor)?;
    let id = call_id(&call)?;
    let info = s.agent().claim_handover(id).ok_or_else(gone)?;
    if info.kind != HandoverKind::Captcha {
        s.agent().restore_handover(id, info);
        return Err(ApiError::bad_request(
            "this request asks for a code, not a CAPTCHA",
        ));
    }
    let solved = match handover_hub().verify_solved(&info.hub_key(id)).await {
        Ok(by) => by,
        Err(e) => {
            s.agent().restore_handover(id, info);
            return Err(not_ready(&e));
        }
    };
    // The audit row says what was verified: the page itself, or the owner's second Done.
    audit(
        &s,
        &actor,
        "handover.captcha_done",
        &json!({"kind": "captcha", "verified_by": match solved {
            SolvedBy::Page => "page",
            SolvedBy::OwnerConfirmed => "owner_confirmed",
        }}),
    )?;
    let run_notified = finish(&s, id, &info, true).await;
    Ok(Json(HandoverAnswer {
        call_id: id.0,
        outcome: "done".into(),
        run_notified,
    }))
}

/// The owner gives up: nothing is typed, the run learns `cancelled`.
#[utoipa::path(post, path = "/v1/handovers/{call_id}/cancel", tag = "handovers",
    security(("bearer" = [])),
    params(("call_id" = Uuid, Path, description = "Tool call id from the handover_requested event")),
    responses((status = 200, body = HandoverAnswer),
              (status = 403, body = ErrorBody, description = "not the local owner"),
              (status = 404, body = ErrorBody, description = "code handover_gone: not waiting any more")))]
pub(crate) async fn cancel_handover(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    Path(call): Path<String>,
) -> Result<Json<HandoverAnswer>, ApiError> {
    local(&actor)?;
    let id = call_id(&call)?;
    let info = s.agent().claim_handover(id).ok_or_else(gone)?;
    let run_notified = finish(&s, id, &info, false).await;
    Ok(Json(HandoverAnswer {
        call_id: id.0,
        outcome: "cancelled".into(),
        run_notified,
    }))
}
