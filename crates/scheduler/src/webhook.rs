//! Webhook trigger signature verification (`HMAC-SHA256`) and execution
//! (`handle_webhook`).
//!
//! # Daemon HTTP Route Contract (for later daemon integration)
//!
//! The HTTP server in `daemon-api` should expose an unauthenticated (HMAC-gated)
//! endpoint:
//!
//! - **Route:** `POST /v1/webhooks/{id}` (where `{id}` is the [`protocol::TaskId`] UUID)
//! - **Headers:** `X-Signature-256: sha256=<hex>` (or `X-Hub-Signature-256: sha256=<hex>`)
//! - **Body:** Raw UTF-8 payload string (JSON or plain text), exposed to the
//!   schedule's payload as `{input}`.
//! - **Handler behaviour:**
//!   1. Extract `id: TaskId`, the raw request body as `&str`, and the signature
//!      header value.
//!   2. Call [`Scheduler::handle_webhook`](crate::Scheduler::handle_webhook)
//!      (or [`handle_webhook`]).
//!   3. Return `202 Accepted` on `Ok(())`, `401 Unauthorized` on
//!      [`SchedulerError::InvalidSignature`], `404 NotFound` on
//!      [`SchedulerError::NotFound`], and `409 Conflict` on
//!      [`SchedulerError::Disabled`].

use hmac::{Hmac, Mac};
use protocol::TaskId;
use sha2::Sha256;

use crate::engine::ScheduleExecutor;
use crate::error::{Result, SchedulerError};
use crate::model::{Schedule, Trigger};
use crate::store::ScheduleStore;

type HmacSha256 = Hmac<Sha256>;

/// Compute the lowercase hex `HMAC-SHA256` digest of `body` under `secret`.
///
/// # Panics
/// Never in practice: HMAC accepts keys of any length.
#[must_use]
pub fn sign_webhook_hex(secret: &str, body: &[u8]) -> String {
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts keys of any length");
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}

/// Compute the `sha256=<hex>` header value for `body` under `secret`.
#[must_use]
pub fn sign_webhook_body(secret: &str, body: &[u8]) -> String {
    format!("sha256={}", sign_webhook_hex(secret, body))
}

/// Verify an `HMAC-SHA256` signature over `body` in constant time.
///
/// Accepts both `"sha256=<hex>"` (case-insensitive prefix) and bare `"<hex>"`.
///
/// # Errors
/// Returns [`SchedulerError::InvalidSignature`] if `signature` is malformed or
/// does not match `HMAC-SHA256(secret, body)`.
pub fn verify_webhook_signature(secret: &str, body: &[u8], signature: &str) -> Result<()> {
    let trimmed = signature.trim();
    let hex_part = trimmed
        .strip_prefix("sha256=")
        .or_else(|| trimmed.strip_prefix("SHA256="))
        .unwrap_or(trimmed);

    let sig_bytes = hex::decode(hex_part).map_err(|_| SchedulerError::InvalidSignature)?;
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .map_err(|_| SchedulerError::InvalidSignature)?;
    mac.update(body);
    mac.verify_slice(&sig_bytes)
        .map_err(|_| SchedulerError::InvalidSignature)
}

/// Verify a webhook's `HMAC-SHA256` signature and fire the target schedule's
/// payload with `body` substituted for `{input}`.
///
/// This free function does not know about the engine's in-flight tracking; use
/// [`Scheduler::handle_webhook`](crate::Scheduler::handle_webhook) in the daemon
/// so the per-schedule concurrency limit applies.
///
/// # Errors
/// - [`SchedulerError::NotFound`] if the schedule or its webhook trigger does not exist.
/// - [`SchedulerError::InvalidSignature`] if the HMAC-SHA256 signature fails verification.
/// - [`SchedulerError::Disabled`] if the schedule is currently disabled.
/// - Any storage or execution error returned by `store` or `executor`.
pub async fn handle_webhook(
    store: &ScheduleStore,
    executor: &dyn ScheduleExecutor,
    id: TaskId,
    body: &str,
    signature: &str,
) -> Result<()> {
    let fired = prepare_webhook(store, id, body, signature, storage::now_ms())?;
    executor.execute(&fired).await
}

/// Look up and authenticate a webhook call, record the firing, and return the
/// schedule with its payload rendered.
pub(crate) fn prepare_webhook(
    store: &ScheduleStore,
    id: TaskId,
    body: &str,
    signature: &str,
    now_ms: i64,
) -> Result<Schedule> {
    let schedule = store
        .get(id)?
        .ok_or_else(|| SchedulerError::NotFound(format!("schedule {id}")))?;
    let trigger = store
        .get_trigger(id)?
        .ok_or_else(|| SchedulerError::NotFound(format!("webhook trigger for schedule {id}")))?;

    let Trigger::Webhook { secret } = trigger;
    verify_webhook_signature(&secret, body.as_bytes(), signature)?;

    if !schedule.enabled {
        return Err(SchedulerError::Disabled(id.to_string()));
    }

    let mut fired = schedule.clone();
    fired.payload = schedule.payload.with_webhook_input(body);
    fired.last_run_at = Some(now_ms);
    // Only touch last_run_at: rewriting next_run_at here could race with the
    // engine and resurrect an already-fired timer occurrence.
    store.touch_last_run(id, now_ms)?;
    Ok(fired)
}
