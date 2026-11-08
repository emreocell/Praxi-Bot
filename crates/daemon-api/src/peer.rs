//! Authenticated ingress for paired devices. The transport supplies identity;
//! HTTP headers can never grant a role.
use crate::{ApiError, AppState};
use axum::{
    Router,
    extract::{Request, State},
    http::Method,
    middleware::Next,
    response::{IntoResponse, Response},
};

/// Role granted by the pairing store, never by the remote HTTP client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerRole {
    Desktop,
    Phone,
    Node,
    Host,
}

/// Authenticated transport identity.
#[derive(Clone, Debug)]
pub struct PeerIdentity {
    pub key: String,
    pub role: PeerRole,
}

/// Future host transfer endpoints forbidden to phones.
pub const PHONE_DENIED_POST_PATHS: &[&str] = &["/v1/host/import", "/v1/host/handover"];

/// Same API and permissions as loopback, with transport authentication.
pub fn peer_router(state: AppState) -> Router {
    crate::api_routes()
        .layer(axum::middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state)
}

/// Optional paired device display name for audit attribution.
#[derive(Clone, Debug)]
pub struct PeerName(pub String);

async fn guard(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let Some(peer) = req.extensions().get::<PeerIdentity>().cloned() else {
        return ApiError::forbidden("paired peer identity required").into_response();
    };
    // Decode before checking: encoded path characters must not bypass scopes.
    let path = match percent_encoding::percent_decode_str(req.uri().path()).decode_utf8() {
        Ok(path) => path.into_owned(),
        Err(_) => return ApiError::forbidden("invalid path").into_response(),
    };
    let denied = peer.role != PeerRole::Desktop
        && (peer.role != PeerRole::Phone
            || (req.method() == Method::POST && PHONE_DENIED_POST_PATHS.contains(&path.as_str()))
            || (matches!(*req.method(), Method::POST | Method::PUT)
                && path.starts_with("/v1/secrets")));
    if denied {
        return ApiError::forbidden("peer role does not allow this operation").into_response();
    }
    // Record the initiator in the existing SQLite audit log before dispatch.
    // Fail closed if attribution cannot be persisted. The normal approval audit
    // and permission engine remain in place and record the resulting decision.
    let mut audit_actor = None;
    if (req.method() == Method::POST
        && (path.starts_with("/v1/approvals/")
            || path.starts_with("/v1/questions/")
            || path.starts_with("/v1/control/")))
        || (req.method() == Method::PUT && path == "/v1/control/state")
    {
        let role = if peer.role == PeerRole::Phone {
            "phone"
        } else {
            "desktop"
        };
        let name = req
            .extensions()
            .get::<PeerName>()
            .map_or(peer.key.as_str(), |n| n.0.as_str());
        let actor = format!("{role}:{name}");
        if let Err(error) = state.storage().append_audit(
            state.user_id(),
            None,
            &actor,
            "peer.decision_requested",
            &serde_json::json!({"peer_key":peer.key,"path":path}),
        ) {
            return ApiError::internal(error).into_response();
        }
        audit_actor = Some(actor);
    }
    let response = next.run(req).await;
    if let Some(actor) = audit_actor
        && let Err(error) = state.storage().append_audit(
            state.user_id(), None, &actor, "peer.decision_completed",
            &serde_json::json!({"peer_key":peer.key,"path":path,"status":response.status().as_u16()}),
        ) { return ApiError::internal(error).into_response(); }
    response
}
