//! `/v1/secret-requests/{call_id}`: the owner's app answers a `secret_request`
//! form (see `docs/design/secret-request.md`).
//!
//! * Owner only: classified in `owner.rs` (`owner_key_required` without the
//!   owner key), refused to every paired peer (`PEER_DENIED_FILL_PREFIXES`) and
//!   refused again here for a non-local actor.
//! * The value goes from this request body straight into the vault
//!   ([`secret_store::FillSecrets`], whose value hook registers it with the
//!   redactor at once). It is never put in an event, the journal, the audit
//!   log, an error message or the answer; the run only learns `stored` or
//!   `cancelled`.
//! * A request can be answered once: the first caller claims it, a second submit
//!   or a cancel finds nothing (`secret_request_gone`). A name that exists is
//!   only replaced when the bot asked to replace it.
//! * No route lists or reads values; a bot has no API token, so it cannot reach
//!   this route at all.

use axum::Json;
use axum::extract::{Extension, Path, State};
use protocol::ToolCallId;
use protocol::event::SecretFields;
use secret_store::FillSecrets;
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::AppState;
use crate::error::{ApiError, ErrorBody};
use crate::fill_routes::{audit, service};
use crate::live_view::Actor;

/// Body of `POST /v1/secret-requests/{call_id}`. Give `value` for a `secret`
/// request, or `username` and `password` for a `login` request.
#[derive(Deserialize, ToSchema)]
pub struct SubmitSecretRequest {
    /// The secret (write-only: no route returns it).
    pub value: Option<String>,
    /// Username of a `login` request.
    pub username: Option<String>,
    /// Password of a `login` request (write-only).
    pub password: Option<String>,
    /// Exact origins the entry may be typed on, like `https://example.com`.
    #[serde(default)]
    pub origins: Vec<String>,
    /// Verified desktop program identities the entry may be typed into.
    #[serde(default)]
    pub programs: Vec<String>,
}

impl std::fmt::Debug for SubmitSecretRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SubmitSecretRequest(<redacted>)")
    }
}

/// How a request ended. Never carries a value.
#[derive(Debug, Serialize, ToSchema)]
pub struct SecretRequestAnswer {
    /// The request that was answered.
    pub call_id: Uuid,
    /// The entry name.
    pub name: String,
    /// `stored` or `cancelled`.
    pub outcome: String,
    /// The waiting run was told. `false` when the run stopped or the request
    /// expired in the same instant (a stored value stays stored).
    pub run_notified: bool,
}

/// Code `secret_request_gone`: the request is not waiting any more (answered,
/// cancelled, expired or the run ended).
/// Parse the body without echoing any of it: serde's messages quote the
/// offending scalar, which here could be the value itself.
fn parse_body(body: &[u8]) -> Result<SubmitSecretRequest, ApiError> {
    serde_json::from_slice(body).map_err(|e| {
        ApiError::bad_request(match e.classify() {
            serde_json::error::Category::Data => {
                "the body does not match the request: use `value`, or `username` and `password`, plus optional `origins` and `programs`"
            }
            _ => "the body is not valid JSON",
        })
    })
}

fn gone() -> ApiError {
    ApiError::new(
        axum::http::StatusCode::NOT_FOUND,
        "secret_request_gone",
        "this request is not waiting any more",
    )
}

fn call_id(raw: &str) -> Result<ToolCallId, ApiError> {
    Uuid::parse_str(raw)
        .map(ToolCallId)
        .map_err(|_| ApiError::bad_request("call_id must be a UUID"))
}

/// The store entries of one request: `(entry name, value)`.
fn entries(
    name: &str,
    fields: SecretFields,
    input: &SubmitSecretRequest,
) -> Result<Vec<(String, String)>, ApiError> {
    let given = |v: &Option<String>| v.clone().filter(|s| !s.is_empty());
    match fields {
        SecretFields::Secret => {
            if input.username.is_some() || input.password.is_some() {
                return Err(ApiError::bad_request(
                    "this request asks for one `value`, not a username and password",
                ));
            }
            let v = given(&input.value)
                .ok_or_else(|| ApiError::bad_request("`value` must not be empty"))?;
            Ok(vec![(name.to_owned(), v)])
        }
        SecretFields::Login => {
            if input.value.is_some() {
                return Err(ApiError::bad_request(
                    "this request asks for `username` and `password`, not one `value`",
                ));
            }
            let (Some(u), Some(p)) = (given(&input.username), given(&input.password)) else {
                return Err(ApiError::bad_request(
                    "`username` and `password` must not be empty",
                ));
            };
            Ok(vec![
                (format!("{name}/username"), u),
                (format!("{name}/password"), p),
            ])
        }
    }
}

/// Put the request back after a failed attempt so the owner can correct the form.
fn put_back(s: &AppState, id: ToolCallId, info: agent_core::SecretRequestInfo) {
    s.agent().restore_secret_request(id, info);
}

/// Answer a `secret_request` form: the value goes into the vault, the run learns `stored`.
#[utoipa::path(post, path = "/v1/secret-requests/{call_id}", tag = "secret-requests",
    security(("bearer" = [])),
    params(("call_id" = Uuid, Path, description = "Tool call id from the secret_requested event")),
    request_body = SubmitSecretRequest,
    responses((status = 200, body = SecretRequestAnswer),
              (status = 400, body = ErrorBody, description = "wrong fields, empty value or bad allow-list entry; the request keeps waiting"),
              (status = 403, body = ErrorBody, description = "not the local owner"),
              (status = 404, body = ErrorBody, description = "code secret_request_gone: not waiting any more"),
              (status = 409, body = ErrorBody, description = "code secret_request_exists: the name exists and the bot did not ask to replace it")))]
pub(crate) async fn submit_secret_request(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    Path(call): Path<String>,
    body: axum::body::Bytes,
) -> Result<Json<SecretRequestAnswer>, ApiError> {
    let svc = service(&s, &actor)?;
    let id = call_id(&call)?;
    let input = parse_body(&body)?;
    let info = s.agent().claim_secret_request(id).ok_or_else(gone)?;
    let result = store(&s, &actor, &svc, &info, &input).await;
    if let Err(e) = result {
        put_back(&s, id, info);
        return Err(e);
    }
    let run_notified = s
        .agent()
        .finish_secret_request(id, true, s.user_id())
        .is_ok();
    Ok(Json(SecretRequestAnswer {
        call_id: id.0,
        name: info.name,
        outcome: "stored".into(),
        run_notified,
    }))
}

async fn store(
    s: &AppState,
    actor: &Actor,
    svc: &std::sync::Arc<FillSecrets>,
    info: &agent_core::SecretRequestInfo,
    input: &SubmitSecretRequest,
) -> Result<(), ApiError> {
    let items = entries(&info.name, info.fields, input)?;
    let existing = items.iter().any(|(n, _)| svc.exists(n));
    if existing && !info.replace {
        return Err(exists());
    }
    audit(
        s,
        actor,
        "secret_request.submit",
        &json!({
            "name": info.name,
            "fields": info.fields,
            "replaces_existing": existing,
            "origins": input.origins,
            "programs": input.programs,
        }),
    )?;
    // One all-or-nothing operation: validated first, and a failed write puts the
    // previous username AND password back (a half-replaced login never remains).
    let (svc2, origins, programs, replace) = (
        std::sync::Arc::clone(svc),
        input.origins.clone(),
        input.programs.clone(),
        info.replace,
    );
    let done =
        tokio::task::spawn_blocking(move || svc2.upsert_all(&items, &origins, &programs, replace))
            .await
            .map_err(ApiError::internal)?;
    match done {
        Ok(_) => Ok(()),
        // Another writer created the name between the check above and the write.
        Err(secret_store::FillError::Exists(_)) => Err(exists()),
        Err(e) => Err(crate::fill_routes::map(&e)),
    }
}

fn exists() -> ApiError {
    ApiError::new(
        axum::http::StatusCode::CONFLICT,
        "secret_request_exists",
        "an entry with this name exists and the bot did not ask to replace it",
    )
}

/// Cancel a `secret_request` form: nothing is stored, the run learns `cancelled`.
#[utoipa::path(post, path = "/v1/secret-requests/{call_id}/cancel", tag = "secret-requests",
    security(("bearer" = [])),
    params(("call_id" = Uuid, Path, description = "Tool call id from the secret_requested event")),
    responses((status = 200, body = SecretRequestAnswer),
              (status = 403, body = ErrorBody, description = "not the local owner"),
              (status = 404, body = ErrorBody, description = "code secret_request_gone: not waiting any more")))]
pub(crate) async fn cancel_secret_request(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    Path(call): Path<String>,
) -> Result<Json<SecretRequestAnswer>, ApiError> {
    // Same local-owner rule as the submit route, even though nothing is stored.
    service(&s, &actor)?;
    let id = call_id(&call)?;
    let info = s.agent().claim_secret_request(id).ok_or_else(gone)?;
    let run_notified = s
        .agent()
        .finish_secret_request(id, false, s.user_id())
        .is_ok();
    Ok(Json(SecretRequestAnswer {
        call_id: id.0,
        name: info.name,
        outcome: "cancelled".into(),
        run_notified,
    }))
}
