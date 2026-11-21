//! API error type: a status code plus a stable JSON body
//! `{"error": {"code": "...", "message": "..."}}`.

use agent_core::LoopError;
use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use task_runtime::RuntimeError;
use utoipa::ToSchema;

/// An error returned by a route.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

/// JSON body of an error response.
#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorBody {
    /// The error.
    pub error: ErrorDetail,
}

/// Machine-readable code plus human-readable message.
#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorDetail {
    /// Stable `snake_case` code, e.g. `not_found`.
    pub code: String,
    /// Human-readable message.
    pub message: String,
}

impl ApiError {
    pub(crate) fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }
    pub(crate) fn message(&self) -> &str {
        &self.message
    }
    pub(crate) fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", message)
    }
    pub(crate) fn not_found(what: impl std::fmt::Display) -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("{what} not found"),
        )
    }
    pub(crate) fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "conflict", message)
    }
    pub(crate) fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", message)
    }
    pub(crate) fn unauthorized() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "missing or invalid bearer token",
        )
    }
    pub(crate) fn unavailable(message: impl Into<String>) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, "unavailable", message)
    }
    /// Background startup (peer check, component start) has not finished yet.
    pub(crate) fn starting() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "starting",
            "the daemon is still starting; retry in a few seconds",
        )
    }
    pub(crate) fn internal(message: impl std::fmt::Display) -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            message.to_string(),
        )
    }

    pub(crate) fn code_str(&self) -> String {
        self.code.to_owned()
    }

    /// HTTP status of the error.
    #[must_use]
    pub fn status(&self) -> StatusCode {
        self.status
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ApiError {}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Json(serde_json::json!({
            "error": { "code": self.code, "message": self.message }
        }));
        let mut resp = (self.status, body).into_response();
        if self.status == StatusCode::UNAUTHORIZED {
            resp.headers_mut().insert(
                axum::http::header::WWW_AUTHENTICATE,
                axum::http::HeaderValue::from_static("Bearer"),
            );
        }
        resp
    }
}

impl From<storage::StorageError> for ApiError {
    fn from(e: storage::StorageError) -> Self {
        match e {
            storage::StorageError::NotFound(w) => Self::not_found(w),
            storage::StorageError::Conflict(w) => Self::conflict(w),
            other => Self::internal(other),
        }
    }
}

impl From<scheduler::SchedulerError> for ApiError {
    fn from(e: scheduler::SchedulerError) -> Self {
        use scheduler::SchedulerError as E;
        match e {
            E::InvalidSpec(m) => Self::new(StatusCode::UNPROCESSABLE_ENTITY, "bad_schedule", m),
            E::NotFound(w) => Self::not_found(w),
            E::Disabled(m) => Self::conflict(format!("schedule {m} is disabled")),
            E::Busy(m) => Self::conflict(format!("schedule {m} is already running")),
            E::Conflict(m) => Self::conflict(m),
            E::RateLimited(m) => {
                Self::new(StatusCode::TOO_MANY_REQUESTS, "schedule_retry_limited", m)
            }
            E::Frozen(m) => Self::new(
                StatusCode::CONFLICT,
                "schedule_moved",
                format!("schedule {m} moved to a server; use reclaim to bring it back"),
            ),
            E::InvalidSignature => Self::new(
                StatusCode::UNAUTHORIZED,
                "invalid_signature",
                "missing or invalid X-Signature-256",
            ),
            other => Self::internal(other),
        }
    }
}

impl From<LoopError> for ApiError {
    fn from(e: LoopError) -> Self {
        match e {
            LoopError::SetupRequired => {
                Self::new(StatusCode::CONFLICT, "setup_required", e.to_string())
            }
            LoopError::NotFound(w) => Self::not_found(w),
            LoopError::Profile(m) => Self::new(StatusCode::UNPROCESSABLE_ENTITY, "bad_profile", m),
            LoopError::Busy(c) => Self::conflict(format!("chat {c} already has an active turn")),
            LoopError::NotLastMessage => Self::new(
                StatusCode::CONFLICT,
                "message_not_last",
                "that message is no longer the last message of the chat",
            ),
            LoopError::NotResendable(why) => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "message_not_editable",
                why,
            ),
            LoopError::StillFinishing(run) => {
                Self::conflict(format!("run {run} is still finishing; retry resume"))
            }
            LoopError::Router(m) => Self::conflict(m),
            LoopError::ShuttingDown => Self::unavailable("the daemon is shutting down"),
            e @ LoopError::OnceOnly(_) => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "approval_once_only",
                e.to_string(),
            ),
            LoopError::NoPendingApproval(c) => {
                Self::not_found(format!("pending approval for call {c}"))
            }
            LoopError::Runtime(RuntimeError::IllegalTransition { from, to }) => {
                Self::conflict(format!("illegal run transition {from} -> {to}"))
            }
            other => Self::internal(other),
        }
    }
}
