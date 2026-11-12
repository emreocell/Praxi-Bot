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

/// Host-local provider management endpoints forbidden to ALL paired peers.
pub const PEER_DENIED_POST_PATHS: &[&str] = &["/v1/providers/cli/enable", "/v1/providers/reload"];

/// Same API and permissions as loopback, with transport authentication.
pub fn peer_router(state: AppState) -> Router {
    crate::api_routes()
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::live_view::audit_ingress,
        ))
        .layer(axum::middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state)
}

/// Optional paired device display name for audit attribution.
#[derive(Clone, Debug)]
pub struct PeerName(pub String);

fn normalize_deny_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut prev_slash = false;
    for ch in path.chars() {
        if ch == '/' {
            if !prev_slash {
                out.push('/');
                prev_slash = true;
            }
        } else {
            out.push(ch);
            prev_slash = false;
        }
    }
    if out.len() > 1 && out.ends_with('/') {
        out.pop();
    }
    out
}

async fn guard(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let Some(peer) = req.extensions().get::<PeerIdentity>().cloned() else {
        return ApiError::forbidden("paired peer identity required").into_response();
    };
    // Decode and collapse redundant slashes before checking: encoded or
    // non-canonical path characters must not bypass scopes.
    let path = match percent_encoding::percent_decode_str(req.uri().path()).decode_utf8() {
        Ok(path) => path.into_owned(),
        Err(_) => return ApiError::forbidden("invalid path").into_response(),
    };
    let check_path = normalize_deny_path(&path);
    if req.method() == Method::POST && PEER_DENIED_POST_PATHS.contains(&check_path.as_str()) {
        return ApiError::forbidden("host-local provider management is not allowed for peers")
            .into_response();
    }
    let denied = peer.role != PeerRole::Desktop
        && (peer.role != PeerRole::Phone
            || (req.method() == Method::POST
                && PHONE_DENIED_POST_PATHS.contains(&check_path.as_str()))
            || (matches!(*req.method(), Method::POST | Method::PUT)
                && check_path.starts_with("/v1/secrets")));
    if denied {
        return ApiError::forbidden("peer role does not allow this operation").into_response();
    }
    // `POST /v1/control/{request_id}` answers an overlay question: local only.
    if req.method() == Method::POST
        && check_path
            .strip_prefix("/v1/control/")
            .is_some_and(|rest| uuid::Uuid::parse_str(rest).is_ok())
    {
        return ApiError::forbidden("only the local user answers overlay questions")
            .into_response();
    }
    if crate::live_view::reject_query_credentials(&req) {
        return ApiError::bad_request("URL credentials are forbidden").into_response();
    }
    req.extensions_mut()
        .insert(crate::live_view::Actor::peer(&peer));
    let (req, control_transition) = if req.method() == Method::PUT && path == "/v1/control/state" {
        let (parts, body) = req.into_parts();
        let Ok(bytes) = axum::body::to_bytes(body, 4096).await else {
            return ApiError::bad_request("control state body exceeds 4096 bytes").into_response();
        };
        let input: crate::dto::ControlStateInput = match serde_json::from_slice(&bytes) {
            Ok(input) => input,
            Err(e) => return ApiError::bad_request(e.to_string()).into_response(),
        };
        // A device without the input grant is refused by the handler (and that
        // denial is coalesced there): it must not write a decision row per try.
        let granted = state
            .live_view()
            .allowed(&crate::live_view::Actor::peer(&peer), true);
        let changed = granted && (input.release || state.hub().reported_mode() != Some(input.mode));
        (
            Request::from_parts(parts, axum::body::Body::from(bytes)),
            changed,
        )
    } else {
        (req, false)
    };
    // Record the initiator in the existing SQLite audit log before dispatch.
    // Fail closed if attribution cannot be persisted. The normal approval audit
    // and permission engine remain in place and record the resulting decision.
    let mut audit_actor = None;
    let refused_control = matches!(
        path.as_str(),
        "/v1/control/release"
            | "/v1/control/takeover"
            | "/v1/control/override"
            | "/v1/control/recover"
    ) && !state
        .live_view()
        .allowed(&crate::live_view::Actor::peer(&peer), true);
    if (req.method() == Method::POST
        && !refused_control
        && (path.starts_with("/v1/approvals/")
            || path.starts_with("/v1/questions/")
            || path.starts_with("/v1/control/")))
        || control_transition
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
