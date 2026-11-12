use super::{Actor, ApiError, AppState, audit, interrupt, parse};
use axum::{
    Extension, Json,
    body::Bytes,
    extract::{Path, State},
    http::StatusCode,
};
use live_view::{
    InputBatch, InputEvent, InputResult, MAX_BATCH_BYTES, MAX_EVENTS, Reason, ViewState,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio::time::Instant;
use uuid::Uuid;

/// How long a batch waits for the input executor before 429 `input_busy`.
const INPUT_QUEUE_WAIT: Duration = Duration::from_secs(1);

pub(super) struct Duplicate {
    first: u64,
    last: u64,
    epoch: u64,
    digest: [u8; 32],
    result: InputResult,
}
#[derive(Clone, Default, Serialize)]
pub(super) struct Counts {
    pub pointer: u64,
    pub wheel: u64,
    pub key: u64,
    pub text: u64,
    pub reset: u64,
}
impl Counts {
    fn add(&mut self, event: &InputEvent) {
        match event {
            InputEvent::Pointer { .. } => self.pointer += 1,
            InputEvent::Wheel { .. } => self.wheel += 1,
            InputEvent::Key { .. } => self.key += 1,
            InputEvent::Text { .. } => self.text += 1,
            InputEvent::Reset => self.reset += 1,
        }
    }
}
pub(super) struct Rate {
    events: f64,
    text: f64,
    at: Instant,
}
impl Rate {
    fn new() -> Self {
        Self {
            events: 64.0,
            text: 8192.0,
            at: Instant::now(),
        }
    }
    fn take(&mut self, events: usize, text: usize) -> bool {
        self.take_at(Instant::now(), events, text)
    }
    /// The token bucket with an explicit clock: refill is `elapsed * rate`
    /// capped at the burst, so the result depends only on the instants given.
    fn take_at(&mut self, now: Instant, events: usize, text: usize) -> bool {
        let dt = now.saturating_duration_since(self.at).as_secs_f64();
        self.at = now;
        self.events = (self.events + dt * 120.0).min(64.0);
        self.text = (self.text + dt * 8192.0).min(8192.0);
        let events = f64::from(u32::try_from(events).unwrap_or(u32::MAX));
        let text = f64::from(u32::try_from(text).unwrap_or(u32::MAX));
        if self.events < events || self.text < text {
            return false;
        }
        self.events -= events;
        self.text -= text;
        true
    }
}
pub(super) fn owner(
    s: &AppState,
    actor: &Actor,
    session: Uuid,
    batch: Option<&InputBatch>,
) -> bool {
    let live = s.live_view();
    if !live.allowed(actor, true) || live.session(actor, session).is_err() {
        return false;
    }
    let Some(t) = s.hub().takeover() else {
        return false;
    };
    if t.actor != actor.key
        || t.info.session_id != session
        || t.info.state != ViewState::Active
        || t.info.input_expires_in_ms == 0
        || s.hub().latched() != Some(crate::dto::ControlMode::HumanDriving)
    {
        return false;
    }
    let Ok(target) = live.target(t.info.target_id, t.generation) else {
        return false;
    };
    if !target.info().capabilities.input {
        return false;
    }
    let sessions = live.sessions.lock();
    let Some(session) = sessions.get(&session) else {
        return false;
    };
    // A live backend publication, rather than an old image cached indefinitely.
    let Some((revision, _, at)) = session.geometry else {
        return false;
    };
    if at.elapsed() > Duration::from_secs(2) {
        return false;
    }
    batch.is_none_or(|b| {
        b.takeover_id == t.info.id
            && b.control_epoch == t.info.control_epoch
            && b.generation == t.generation
            && (!b.events.iter().any(InputEvent::positional) || b.geometry_revision == revision)
    })
}
struct BatchLifetime {
    hub: std::sync::Arc<crate::ControlHub>,
    takeover: Uuid,
    armed: bool,
}
impl Drop for BatchLifetime {
    fn drop(&mut self) {
        if self.armed {
            self.hub.interrupt(self.takeover, Reason::Disconnected);
        }
    }
}
struct PendingInput(std::sync::Arc<std::sync::atomic::AtomicUsize>);
impl Drop for PendingInput {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[utoipa::path(post,path="/v1/view/sessions/{id}/input",tag="view",security(("bearer"=[])),params(("id"=Uuid,Path)),request_body=InputBatch,responses((status=200,body=InputResult),(status=409),(status=429)))]
#[allow(clippy::too_many_lines)] // Preserve one application-time ownership executor transaction.
pub(crate) async fn input(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
    body: Bytes,
) -> Result<Json<InputResult>, ApiError> {
    let live = s.live_view();
    // The executor is held briefly by the takeover activation task, the session
    // reaper and the heartbeat/profile routes. A client that waited for `active`
    // and sends its next batch at once must queue behind them (FIFO) rather than
    // get a spurious 429; only a genuinely long-held executor is "busy".
    let _executor = tokio::time::timeout(INPUT_QUEUE_WAIT, live.executor.lock())
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "input_busy",
                "input executor busy",
            )
        })?;
    let (target, generation, _) = live.session(&actor, id)?;
    let batch: InputBatch = match parse(&body) {
        Ok(b) => b,
        Err(e) => {
            audit(
                &s,
                &actor,
                "input.batch_rejected",
                serde_json::json!({"target_id":target,"reason":"invalid_schema"}),
            )?;
            return Err(e);
        }
    };
    if batch.v != 1
        || body.len() > MAX_BATCH_BYTES
        || batch.events.is_empty()
        || batch.events.len() > MAX_EVENTS
        || !batch.events.iter().all(InputEvent::valid)
    {
        audit(
            &s,
            &actor,
            "input.batch_rejected",
            serde_json::json!({"target_id":target,"reason":"invalid_batch"}),
        )?;
        return Err(ApiError::bad_request("invalid input batch"));
    }
    let last = batch
        .first_seq
        .checked_add(u64::try_from(batch.events.len()).unwrap_or(u64::MAX) - 1)
        .filter(|last| *last < u64::MAX)
        .ok_or_else(|| ApiError::conflict("sequence exhausted"))?;
    let t = s
        .hub()
        .takeover()
        .filter(|t| {
            t.actor == actor.key && t.info.session_id == id && t.info.id == batch.takeover_id
        })
        .ok_or_else(|| ApiError::forbidden("takeover owner and epoch required"))?;
    let canonical =
        serde_json::to_vec(&batch).map_err(|_| ApiError::bad_request("invalid batch"))?;
    let digest: [u8; 32] = Sha256::digest(&canonical).into();
    {
        let sessions = live.sessions.lock();
        let session = sessions
            .get(&id)
            .ok_or_else(|| ApiError::not_found("view session"))?;
        if batch.first_seq != session.next_seq {
            if let Some(old) = session.duplicates.iter().find(|d| {
                d.first == batch.first_seq
                    && d.last == last
                    && d.epoch == batch.control_epoch
                    && d.digest == digest
            }) {
                return Ok(Json(old.result.clone()));
            }
            drop(sessions);
            audit(
                &s,
                &actor,
                "input.batch_rejected",
                serde_json::json!({"target_id":target,"takeover_id":t.info.id,"first_seq":batch.first_seq,"last_seq":last,"reason":"resync_required"}),
            )?;
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "resync_required",
                "sequence gap, mismatch, or retry outside window",
            ));
        }
    }
    if t.info.control_epoch != batch.control_epoch {
        return Err(ApiError::forbidden("takeover epoch required"));
    }
    if batch.generation != generation || !owner(&s, &actor, id, Some(&batch)) {
        audit(
            &s,
            &actor,
            "input.batch_rejected",
            serde_json::json!({"target_id":target,"takeover_id":t.info.id,"reason":"ownership_or_geometry"}),
        )?;
        return Err(ApiError::conflict(
            "input ownership, generation, frame freshness, or geometry invalid",
        ));
    }
    if live.pending_input.load(std::sync::atomic::Ordering::SeqCst) != 0 {
        return Err(ApiError::conflict("previous input still draining"));
    }
    let target_impl = live.target(target, generation)?;
    let sink = target_impl
        .input_sink()
        .ok_or_else(|| ApiError::unavailable("input unavailable"))?;
    let text_bytes: usize = batch.events.iter().map(InputEvent::text_bytes).sum();
    let rate_ok = live
        .rates
        .lock()
        .entry(actor.key.clone())
        .or_insert_with(Rate::new)
        .take(batch.events.len(), text_bytes);
    if !rate_ok {
        interrupt(&s, &actor, &t, Reason::RateLimited).await?;
        audit(
            &s,
            &actor,
            "input.batch_rejected",
            serde_json::json!({"target_id":target,"reason":"rate_limited"}),
        )?;
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "input_rate_limited",
            "input suspended; explicit reacquisition required",
        ));
    }
    let mut counts = Counts::default();
    for event in &batch.events {
        counts.add(event);
    }
    let meta = serde_json::json!({"target_id":target,"takeover_id":t.info.id,"control_epoch":batch.control_epoch,"first_seq":batch.first_seq,"last_seq":last,"counts":counts,"bytes":body.len()});
    if let Err(e) = audit(&s, &actor, "input.batch_authorized", meta.clone()) {
        let _ = interrupt(&s, &actor, &t, Reason::AuditFailed).await;
        return Err(e);
    }
    let mut lifetime = BatchLifetime {
        hub: std::sync::Arc::clone(s.hub()),
        takeover: t.info.id,
        armed: true,
    };
    let started = Instant::now();
    let mut applied = batch.first_seq.saturating_sub(1);
    let mut state = ViewState::Active;
    for (offset, event) in batch.events.iter().enumerate() {
        // Authorization is checked at application time, under the release executor.
        if !owner(&s, &actor, id, Some(&batch)) {
            state = ViewState::Paused;
            break;
        }
        let sink = std::sync::Arc::clone(&sink);
        let event_to_apply = event.clone();
        let weak = std::sync::Arc::downgrade(&s.0);
        let task_actor = actor.clone();
        let takeover = t.info.id;
        let epoch = t.info.control_epoch;
        let geometry = event.positional().then_some(batch.geometry_revision);
        live.pending_input
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let pending = PendingInput(std::sync::Arc::clone(&live.pending_input));
        let work = tokio::spawn(async move {
            let _pending = pending;
            let Some(inner) = weak.upgrade() else {
                return Err(live_view::ViewError::Unavailable);
            };
            let state = AppState(inner);
            if !state
                .hub()
                .takeover()
                .is_some_and(|t| t.info.id == takeover && t.info.control_epoch == epoch)
                || !owner(&state, &task_actor, id, None)
            {
                return Err(live_view::ViewError::Unavailable);
            }
            if geometry.is_some_and(|revision| {
                state
                    .live_view()
                    .sessions
                    .lock()
                    .get(&id)
                    .and_then(|s| s.geometry)
                    .is_none_or(|(current, _, _)| current != revision)
            }) {
                return Err(live_view::ViewError::Unavailable);
            }
            let result = if matches!(event_to_apply, InputEvent::Reset) {
                sink.release_pressed_inputs().await
            } else {
                sink.inject(&event_to_apply).await
            };
            if let Some(inner) = weak.upgrade() {
                let state = AppState(inner);
                let invalid = !state.hub().takeover().is_some_and(|t| {
                    t.info.id == takeover
                        && t.info.control_epoch == epoch
                        && t.info.state == ViewState::Active
                }) || !owner(&state, &task_actor, id, None);
                if invalid && sink.release_pressed_inputs().await.is_err() {
                    state.hub().interrupt(takeover, Reason::ResetFailed);
                }
            }
            result
        });
        let result = tokio::time::timeout(Duration::from_secs(2), work).await;
        if !matches!(result, Ok(Ok(Ok(())))) {
            state = ViewState::Paused;
            break;
        }
        applied = batch.first_seq + u64::try_from(offset).unwrap_or(0);
        if let Some(session) = live.sessions.lock().get_mut(&id) {
            session.counts.add(event);
        }
        if !owner(&s, &actor, id, Some(&batch)) {
            state = ViewState::Paused;
            break;
        }
    }
    if state == ViewState::Paused {
        let _ = interrupt(&s, &actor, &t, Reason::PartialInjection).await;
    }
    let mut result = InputResult {
        accepted_through: last,
        applied_through: applied,
        state,
    };
    let mut outcome_meta = meta;
    outcome_meta["applied_through"] = applied.into();
    outcome_meta["duration_ms"] = u64::try_from(started.elapsed().as_millis())
        .unwrap_or(u64::MAX)
        .into();
    let audit_failed = audit(
        &s,
        &actor,
        if result.state == ViewState::Active {
            "input.batch_applied"
        } else {
            "input.batch_rejected"
        },
        outcome_meta,
    )
    .is_err();
    if audit_failed {
        result.state = ViewState::Paused;
        let _ = interrupt(&s, &actor, &t, Reason::AuditFailed).await;
    }
    {
        let mut sessions = live.sessions.lock();
        if let Some(session) = sessions.get_mut(&id) {
            session.next_seq = last + 1;
            if session.duplicates.len() == 64 {
                session.duplicates.pop_front();
            }
            session.duplicates.push_back(Duplicate {
                first: batch.first_seq,
                last,
                epoch: batch.control_epoch,
                digest,
                result: result.clone(),
            });
        }
    }
    lifetime.armed = false;
    if audit_failed {
        return Err(ApiError::unavailable(
            "live-view audit unavailable; input suspended",
        ));
    }
    Ok(Json(result))
}

#[cfg(test)]
mod rate_tests {
    use super::Rate;
    use std::time::Duration;
    use tokio::time::Instant;

    #[test]
    fn burst_then_refill_is_exact_around_the_straddling_boundary() {
        let t0 = Instant::now();
        let mut rate = Rate::new();
        rate.at = t0;
        assert!(rate.take_at(t0, 32, 0));
        assert!(rate.take_at(t0, 32, 0));
        // The burst of 64 is spent: 32 more events at the same instant fail.
        assert!(!rate.take_at(t0, 32, 0));
        // 120 events/s: 32 tokens need 266.67 ms. A batch that straddles the
        // boundary is refused just before it and accepted just after.
        assert!(!rate.take_at(t0 + Duration::from_millis(266), 32, 0));
        assert!(rate.take_at(t0 + Duration::from_millis(268), 32, 0));
    }

    #[test]
    fn refill_never_exceeds_the_burst_and_text_has_its_own_budget() {
        let t0 = Instant::now();
        let mut rate = Rate::new();
        rate.at = t0;
        // A long idle period still allows only the burst of 64.
        let later = t0 + Duration::from_secs(600);
        assert!(rate.take_at(later, 64, 0));
        assert!(!rate.take_at(later, 1, 0));
        // 8192 text bytes/s, independent of the event budget.
        assert!(rate.take_at(later + Duration::from_secs(10), 1, 8192));
        assert!(!rate.take_at(later + Duration::from_secs(10), 1, 1));
        assert!(rate.take_at(
            later + Duration::from_secs(10) + Duration::from_millis(125),
            1,
            1024
        ));
    }

    #[test]
    fn a_clock_that_goes_backwards_never_refills() {
        let t0 = Instant::now();
        let mut rate = Rate::new();
        rate.at = t0 + Duration::from_secs(5);
        assert!(rate.take_at(t0, 64, 0));
        assert!(!rate.take_at(t0, 1, 0));
    }
}
