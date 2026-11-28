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
use secret_store::{FillSecrets, SecretValue};
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;
use zeroize::Zeroize;

use crate::AppState;
use crate::error::{ApiError, ErrorBody};
use crate::fill_routes::service;
use crate::live_view::Actor;

/// Body of `POST /v1/secret-requests/{call_id}`. Give `value` for a `secret`
/// request, or `username` and `password` for a `login` request.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SubmitSecretRequest {
    /// The secret (write-only).
    #[serde(default, deserialize_with = "secret_value")]
    #[schema(value_type = Option<String>, write_only)]
    pub value: Option<SecretValue>,
    /// Username of a `login` request (write-only).
    #[serde(default, deserialize_with = "secret_value")]
    #[schema(value_type = Option<String>, write_only)]
    pub username: Option<SecretValue>,
    /// Password of a `login` request (write-only).
    #[serde(default, deserialize_with = "secret_value")]
    #[schema(value_type = Option<String>, write_only)]
    pub password: Option<SecretValue>,
}

fn secret_value<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<SecretValue>, D::Error> {
    Option::<String>::deserialize(d).map(|value| value.map(SecretValue::new))
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
    /// The waiting run was told, or (a mission's request) the parked item was released.
    /// `false` when the run stopped or the request expired in the same instant, the item
    /// was already released by hand, or the request was cancelled (a stored value stays
    /// stored).
    pub run_notified: bool,
}

/// Code `secret_request_gone`: the request is not waiting any more (answered,
/// cancelled, expired or the run ended).
/// Parse the body without echoing any of it: serde's messages quote the
/// offending scalar, which here could be the value itself.
fn parse_body(body: axum::body::Bytes) -> Result<SubmitSecretRequest, ApiError> {
    // A unique buffer is wiped in place. A shared one cannot be (another owner may still
    // read it), so parse a private copy instead and wipe that: the copy never outlives this
    // call. Either way the parsed values are `SecretValue`s, which wipe themselves on drop.
    let parsed = match body.try_into_mut() {
        Ok(mut unique) => {
            let parsed = serde_json::from_slice(&unique);
            unique.as_mut().zeroize();
            parsed
        }
        Err(shared) => {
            let mut copy = shared.to_vec();
            drop(shared);
            let parsed = serde_json::from_slice(&copy);
            copy.zeroize();
            parsed
        }
    };
    parsed.map_err(|e| {
        ApiError::bad_request(match e.classify() {
            serde_json::error::Category::Data => {
                "the body does not match the request: use `value`, or `username` and `password`"
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
    input: SubmitSecretRequest,
) -> Result<Vec<(String, SecretValue)>, ApiError> {
    let given = |v: Option<SecretValue>| v.filter(|s| !s.expose().is_empty());
    match fields {
        SecretFields::Secret => {
            if input.username.is_some() || input.password.is_some() {
                return Err(ApiError::bad_request(
                    "this request asks for one `value`, not a username and password",
                ));
            }
            let v = given(input.value)
                .ok_or_else(|| ApiError::bad_request("`value` must not be empty"))?;
            Ok(vec![(name.to_owned(), v)])
        }
        SecretFields::Login => {
            if input.value.is_some() {
                return Err(ApiError::bad_request(
                    "this request asks for `username` and `password`, not one `value`",
                ));
            }
            let (Some(u), Some(p)) = (given(input.username), given(input.password)) else {
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
              (status = 400, body = ErrorBody, description = "wrong fields or empty value; the request keeps waiting"),
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
    let input = parse_body(body)?;
    // Finish or restore the claim even if the submitting connection closes.
    let answer = tokio::spawn(async move { submit(s, actor, svc, id, input).await })
        .await
        .map_err(ApiError::internal)??;
    Ok(Json(answer))
}

async fn submit(
    s: AppState,
    actor: Actor,
    svc: std::sync::Arc<FillSecrets>,
    id: ToolCallId,
    input: SubmitSecretRequest,
) -> Result<SecretRequestAnswer, ApiError> {
    let info = s.agent().claim_secret_request(id).ok_or_else(gone)?;
    if let Err(error) = store(&s, &actor, svc, id, &info, input).await {
        put_back(&s, id, info);
        return Err(error);
    }
    // Record the vault outcome independently of whether the run is still listening.
    // The submission attempt was already audited before the write.
    let _ = s.storage().append_audit(
        s.user_id(),
        Some(info.run_id),
        &actor.key,
        "secret_request.stored",
        &json!({
            "call_id": id,
            "name": info.name,
            "mission_item": info.mission.as_ref().map(|t| &t.item),
        }),
    );
    let run_notified = if let Some(target) = &info.mission {
        release_item(&s, target, &info.name)
    } else {
        s.agent()
            .finish_secret_request(id, true, s.user_id())
            .is_ok()
    };
    Ok(SecretRequestAnswer {
        call_id: id.0,
        name: info.name,
        outcome: "stored".into(),
        run_notified,
    })
}

/// A mission asked: no run waits, so the answer releases the parked item instead. The item
/// gets the entry name and how to use it, never a value. `false` when the item was released
/// or skipped by hand in the meantime, or missions are not running (the value stays stored).
fn release_item(s: &AppState, target: &agent_core::MissionTarget, name: &str) -> bool {
    let Ok(supervisor) = s.mission_supervisor() else {
        return false;
    };
    let answer = format!(
        "The owner stored the credential `{name}` through the secure form. Use it by name (secrets_list, then the `secret` parameter of browser_fill); you cannot read it."
    );
    supervisor
        .answer_blocked(&target.mission_id, &[(target.item.clone(), answer)])
        .is_ok_and(|released| released > 0)
}

async fn store(
    s: &AppState,
    actor: &Actor,
    svc: std::sync::Arc<FillSecrets>,
    id: ToolCallId,
    info: &agent_core::SecretRequestInfo,
    input: SubmitSecretRequest,
) -> Result<(), ApiError> {
    let items = entries(&info.name, info.fields, input)?;
    s.storage()
        .append_audit(
            s.user_id(),
            Some(info.run_id),
            &actor.key,
            "secret_request.submit",
            &json!({
                "call_id": id,
                "name": info.name,
                "fields": info.fields,
                "replace": info.replace,
                "origin": info.origin,
                "bot_id": info.bot_id,
            }),
        )
        .map_err(ApiError::internal)?;

    // Only daemon-owned request metadata reaches the locked vault operation.
    let info = info.clone();
    let done = tokio::task::spawn_blocking(move || {
        svc.store_request(
            &items,
            &info.origin,
            &info.bot_id,
            &info.purpose,
            info.replace,
        )
    })
    .await
    .map_err(ApiError::internal)?;
    match done {
        Ok(_) => Ok(()),
        Err(secret_store::FillError::Exists(_)) => Err(exists()),
        Err(error) => Err(crate::fill_routes::map(&error)),
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
    let run_notified = if let Some(target) = &info.mission {
        // Nothing waits: the item stays parked, and the owner decides what happens to it.
        let _ = s.storage().append_audit(
            s.user_id(),
            Some(info.run_id),
            "user",
            "secret_request.cancelled",
            &json!({"call_id": id, "name": info.name, "mission_item": target.item}),
        );
        false
    } else {
        s.agent()
            .finish_secret_request(id, false, s.user_id())
            .is_ok()
    };
    Ok(Json(SecretRequestAnswer {
        call_id: id.0,
        name: info.name,
        outcome: "cancelled".into(),
        run_notified,
    }))
}

/// Forward local requests as navigation-only notices. The channels never receive the
/// request description, the form id, or anything submitted to the vault.
pub(crate) fn start_channel_notifications(state: &AppState) {
    let mut events = state.agent().subscribe();
    let state = state.clone();
    let shutdown = state.shutdown_token();
    tokio::spawn(async move {
        loop {
            let item = tokio::select! {
                () = shutdown.cancelled() => break,
                result = events.recv() => match result {
                    Ok(item) => item,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                },
            };
            let form = match item.envelope.event {
                protocol::Event::SecretRequested(request) => state
                    .agent()
                    .secret_request(request.call_id)
                    .is_some()
                    .then_some(false),
                protocol::Event::ElicitationRequested(request) => state
                    .agent()
                    .elicitation(request.call_id)
                    .is_some()
                    .then_some(true),
                _ => continue,
            };
            let Some(elicitation) = form else { continue };
            if !state.leader().components_allowed() {
                continue;
            }
            let Some(chat) = state
                .run_of_user(item.run_id)
                .ok()
                .and_then(|run| run.chat_id)
            else {
                continue;
            };
            let channels = state.channels();
            let send = futures::future::join_all(channels.iter().map(|channel| async move {
                if elicitation {
                    channel.notify_elicitation(chat).await;
                } else {
                    channel.notify_secret_request(chat).await;
                }
            }));
            tokio::select! {
                () = shutdown.cancelled() => break,
                _ = tokio::time::timeout(std::time::Duration::from_secs(20), send) => {}
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use axum::body::Bytes;

    use super::parse_body;

    #[test]
    fn parse_body_reads_a_unique_and_a_shared_buffer_and_leaves_the_shared_one_intact() {
        let raw = br#"{"value":"canary-parse-7741"}"#;
        let unique = parse_body(Bytes::copy_from_slice(raw)).unwrap();
        assert_eq!(unique.value.as_ref().unwrap().expose(), "canary-parse-7741");
        // A second owner keeps the buffer shared: the copy is wiped, not the original.
        let shared = Bytes::copy_from_slice(raw);
        let other = shared.clone();
        let parsed = parse_body(shared).unwrap();
        assert_eq!(parsed.value.as_ref().unwrap().expose(), "canary-parse-7741");
        assert_eq!(&other[..], raw);
    }

    #[test]
    fn parse_body_errors_never_quote_the_body() {
        for raw in [
            &br#"{"value":"canary-parse-7741""#[..],
            br#"{"value":"canary-parse-7741","extra":1}"#,
            br#"{"value":7}"#,
        ] {
            let shared = Bytes::copy_from_slice(raw);
            let _other_owner = shared.clone();
            for body in [Bytes::copy_from_slice(raw), shared] {
                let error = parse_body(body).unwrap_err();
                assert!(!format!("{error:?}").contains("canary-parse"));
            }
        }
    }
}
