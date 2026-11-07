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
//! - **Optional replay protection:** a caller (or the daemon route, when it
//!   forwards the headers) may supply [`WebhookMeta`]:
//!   - `delivery_id` (e.g. `X-GitHub-Delivery` / `X-Delivery-Id`): its presence
//!     turns on duplicate detection. The id itself is not signed, so requests are
//!     deduplicated on the verified signature (schedule + signature), never on
//!     the id: re-sending a captured request with a new id is still rejected.
//!     An accepted request is remembered until its timestamp leaves the window
//!     (`2 * REPLAY_WINDOW_MS` at most); requests rejected as `Disabled` or
//!     `Busy` are not remembered, so the sender may retry them.
//!   - `timestamp_ms` (e.g. `X-Webhook-Timestamp`, Unix ms): the signature must
//!     then be `HMAC-SHA256(secret, "{timestamp}.{body}")` and the timestamp must
//!     be within +/- [`REPLAY_WINDOW_MS`] of now.
//!
//!   Without either (or after the window), a captured request can be replayed (GitHub-style signatures
//!   cover the body only). The endpoint is only reachable over loopback or a
//!   tunnel, which bounds the risk; senders that can should send a timestamp.
//!   Replays and stale timestamps are reported as [`SchedulerError::InvalidSignature`]
//!   (`401`).
//! - **Handler behaviour:**
//!   1. Extract `id: TaskId`, the raw request body as `&str`, and the signature
//!      header value.
//!   2. Call [`Scheduler::handle_webhook`](crate::Scheduler::handle_webhook)
//!      (or [`handle_webhook`]).
//!   3. Return `202 Accepted` on `Ok(())`, `401 Unauthorized` on
//!      [`SchedulerError::InvalidSignature`], `404 NotFound` on
//!      [`SchedulerError::NotFound`], and `409 Conflict` on
//!      [`SchedulerError::Disabled`].

use std::collections::HashMap;

use hmac::{Hmac, Mac};
use parking_lot::Mutex;
use protocol::TaskId;
use sha2::Sha256;

use crate::engine::ScheduleExecutor;
use crate::error::{Result, SchedulerError};
use crate::model::{Schedule, Trigger};
use crate::store::ScheduleStore;

type HmacSha256 = Hmac<Sha256>;

/// Replay window: accepted clock skew for timestamps and retention of an
/// accepted request after its timestamp (5 minutes).
pub const REPLAY_WINDOW_MS: i64 = 5 * 60 * 1000;

const MAX_REMEMBERED_DELIVERIES: usize = 10_000;

/// Optional per-request replay-protection metadata (see module docs).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WebhookMeta {
    /// Unique delivery id supplied by the sender, if any.
    pub delivery_id: Option<String>,
    /// Sender timestamp (Unix milliseconds); when set, it is covered by the
    /// signature as `"{timestamp}.{body}"`.
    pub timestamp_ms: Option<i64>,
}

/// Identity of an authenticated request: schedule and normalised signature.
/// The signature covers the body (and the timestamp when one is claimed), so
/// it cannot be changed by an attacker; the delivery id is *not* signed and
/// therefore is not part of the identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ReplayKey(TaskId, String);

/// A verified request waiting to be committed to the [`ReplayGuard`] once it is
/// actually accepted (schedule enabled, run slot acquired).
#[derive(Debug, Clone)]
pub(crate) struct PendingReplay {
    key: ReplayKey,
    expires_ms: i64,
}

/// Remembers recently accepted requests per schedule until they can no longer
/// be replayed.
#[derive(Debug, Default)]
pub struct ReplayGuard {
    /// key -> time (ms) until which the request is remembered.
    seen: Mutex<HashMap<ReplayKey, i64>>,
}

impl ReplayGuard {
    fn is_replay(&self, key: &ReplayKey, now_ms: i64) -> bool {
        self.seen.lock().get(key).is_some_and(|exp| *exp > now_ms)
    }

    /// Remember an accepted request; returns `false` if it was already
    /// remembered (lost a race with an identical request) or the cache is full.
    pub(crate) fn commit(&self, pending: PendingReplay, now_ms: i64) -> bool {
        let mut seen = self.seen.lock();
        if seen.len() >= MAX_REMEMBERED_DELIVERIES {
            seen.retain(|_, exp| *exp > now_ms);
            if seen.len() >= MAX_REMEMBERED_DELIVERIES {
                return false; // fail closed under flood
            }
        }
        if seen.get(&pending.key).is_some_and(|exp| *exp > now_ms) {
            return false;
        }
        seen.insert(pending.key, pending.expires_ms);
        true
    }
}

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
    let (fired, _) = prepare_webhook(
        store,
        id,
        body,
        signature,
        storage::now_ms(),
        &WebhookMeta::default(),
        None,
    )?;
    executor.execute(&fired).await
}

/// Look up and authenticate a webhook call, record the firing, and return the
/// schedule with its payload rendered, plus the replay record the caller must
/// [`ReplayGuard::commit`] once the request is accepted (so a `Disabled`/`Busy`
/// answer does not use the request up).
pub(crate) fn prepare_webhook(
    store: &ScheduleStore,
    id: TaskId,
    body: &str,
    signature: &str,
    now_ms: i64,
    meta: &WebhookMeta,
    guard: Option<&ReplayGuard>,
) -> Result<(Schedule, Option<PendingReplay>)> {
    let schedule = store
        .get(id)?
        .ok_or_else(|| SchedulerError::NotFound(format!("schedule {id}")))?;
    let trigger = store
        .get_trigger(id)?
        .ok_or_else(|| SchedulerError::NotFound(format!("webhook trigger for schedule {id}")))?;

    let Trigger::Webhook { secret } = trigger;
    if let Some(ts) = meta.timestamp_ms {
        if (now_ms.saturating_sub(ts)).abs() > REPLAY_WINDOW_MS {
            return Err(SchedulerError::InvalidSignature);
        }
        let signed = format!("{ts}.{body}");
        verify_webhook_signature(&secret, signed.as_bytes(), signature)?;
    } else {
        verify_webhook_signature(&secret, body.as_bytes(), signature)?;
    }
    // Dedupe on the (signed) signature, not the unsigned delivery id. Without a
    // timestamp or delivery id the sender opted out of replay protection.
    let pending = match guard {
        Some(g) if meta.timestamp_ms.is_some() || meta.delivery_id.is_some() => {
            let trimmed = signature.trim();
            let hex_part = trimmed
                .strip_prefix("sha256=")
                .or_else(|| trimmed.strip_prefix("SHA256="))
                .unwrap_or(trimmed);
            let key = ReplayKey(id, hex_part.to_ascii_lowercase());
            if g.is_replay(&key, now_ms) {
                return Err(SchedulerError::InvalidSignature);
            }
            // Timestamps may be up to a window in the future: remember until
            // the timestamp itself leaves the window.
            let base = meta.timestamp_ms.map_or(now_ms, |ts| ts.max(now_ms));
            Some(PendingReplay {
                key,
                expires_ms: base.saturating_add(REPLAY_WINDOW_MS),
            })
        }
        _ => None,
    };

    if !schedule.enabled {
        return Err(SchedulerError::Disabled(id.to_string()));
    }

    let mut fired = schedule.clone();
    fired.payload = schedule.payload.with_webhook_input(body);
    fired.last_run_at = Some(now_ms);
    // Only touch last_run_at: rewriting next_run_at here could race with the
    // engine and resurrect an already-fired timer occurrence.
    store.touch_last_run(id, now_ms)?;
    Ok((fired, pending))
}
