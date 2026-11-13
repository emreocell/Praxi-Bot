use super::{Actor, ApiError, AppState, audit, end_session, input, interrupt};
use axum::{
    Extension,
    body::Bytes,
    extract::{Path, State},
    http::StatusCode,
};
use axum::{
    body::Body,
    http::{HeaderMap, HeaderValue, header},
    response::Response,
};
use live_view::wire::Record;
use live_view::{EffectiveProfile, MEDIA_TYPE, Reason, RecordHeader, ViewState};
use std::{sync::Arc, time::Duration};
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub(super) fn spawn_reaper(s: &AppState, id: Uuid, actor: Actor) {
    let weak = Arc::downgrade(&s.0);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let Some(inner) = weak.upgrade() else {
                return;
            };
            let s = AppState(inner);
            if s.shutdown_token().is_cancelled() {
                let _ = end_session(&s, &actor, id, Reason::Disconnected).await;
                return;
            }
            let live = s.live_view();
            if !live.sessions.lock().contains_key(&id) {
                return;
            }
            if live.session(&actor, id).is_err() {
                let _ = end_session(&s, &actor, id, Reason::Expired).await;
                return;
            }
            let _executor = live.executor.lock().await;
            if let Some(t) = s.hub().takeover().filter(|t| {
                t.info.session_id == id
                    && matches!(t.info.state, ViewState::Active | ViewState::Acquiring)
            }) {
                if t.info.input_expires_in_ms == 0 || !live.allowed(&actor, true) {
                    let _ = interrupt(&s, &actor, &t, Reason::Expired).await;
                } else if t.info.state == ViewState::Active
                    && live
                        .sessions
                        .lock()
                        .get(&id)
                        .and_then(|s| s.geometry)
                        .is_some_and(|(_, _, at)| at.elapsed() > Duration::from_secs(2))
                {
                    let _ = interrupt(&s, &actor, &t, Reason::Stale).await;
                }
            }
        }
    });
}
#[utoipa::path(get,path="/v1/view/sessions/{id}/frames",tag="view",security(("bearer"=[])),params(("id"=Uuid,Path)),responses((status=200,content_type="application/vnd.praxi.live-view.v1",body=String),(status=409)))]
#[allow(clippy::too_many_lines)] // The producer owns cleanup and bounded body capacity in one task.
pub(crate) async fn frames(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if headers.get(header::ACCEPT).and_then(|h| h.to_str().ok()) != Some(MEDIA_TYPE) {
        return Err(ApiError::new(
            StatusCode::NOT_ACCEPTABLE,
            "view_media_type",
            "live-view v1 Accept required",
        ));
    }
    let live = Arc::clone(s.live_view());
    let executor = live.executor.lock().await;
    let (target, generation, profile) = live.session(&actor, id)?;
    let target_impl = live.target(target, generation)?;
    let cancel = CancellationToken::new();
    {
        let mut sessions = live.sessions.lock();
        let session = sessions
            .get_mut(&id)
            .ok_or_else(|| ApiError::not_found("view session"))?;
        if session.stream.is_some() {
            return Err(ApiError::conflict("one frame stream per session"));
        }
        session.stream = Some(cancel.clone());
    }
    let source = target_impl.frame_source();
    // Bounded like every backend call made under the executor.
    let subscription = match super::bounded(source.subscribe(profile)).await {
        Ok(sub) => sub,
        Err(e) => {
            if let Some(session) = live.sessions.lock().get_mut(&id) {
                session.stream = None;
            }
            return Err(e);
        }
    };
    if let Err(error) = live.source_profile(target).await {
        if let Some(session) = live.sessions.lock().get_mut(&id) {
            session.stream = None;
        }
        return Err(error);
    }
    let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(1);
    let state = s.clone();
    drop(executor);
    tokio::spawn(async move {
        let mut sub = subscription;
        let mut last_status = Vec::new();
        let mut last_frame: Option<(u64, u64)> = None;
        let mut publication = true;
        let mut heartbeat_at = Instant::now();
        // Heartbeat records flow every 5 s; the audit proves the trail is
        // writable at most once a minute (a failure still ends the stream).
        let mut checkpoint_at = Instant::now();
        let mut token_at = Instant::now();
        let mut tokens = f64::from(EffectiveProfile::watch().burst_bytes);
        let mut last_sent = Instant::now()
            .checked_sub(Duration::from_secs(5))
            .unwrap_or_else(Instant::now);
        let shutdown = state.shutdown_token();
        loop {
            if cancel.is_cancelled() || shutdown.is_cancelled() || live.session(&actor, id).is_err()
            {
                break;
            }
            // Reserve body capacity before cloning/encoding pixels: one in flight + latest watch slot.
            let permit = tokio::select! {
                () = cancel.cancelled() => break,
                () = shutdown.cancelled() => break,
                p = tokio::time::timeout(Duration::from_secs(2),tx.reserve()) => match p { Ok(Ok(p)) => p,_ => break },
            };
            if live.session(&actor, id).is_err() {
                break;
            }
            let Ok((_, _, profile)) = live.session(&actor, id) else {
                break;
            };
            let takeover = state.hub().takeover().filter(|t| t.info.session_id == id);
            let status = Record {
                header: RecordHeader::Status {
                    v: 1,
                    session_id: id,
                    state: takeover
                        .as_ref()
                        .map_or(ViewState::Watch, |t| t.info.state.clone()),
                    reason: takeover.as_ref().and_then(|t| t.info.reason.clone()),
                    effective_profile: profile.clone(),
                    control_epoch: takeover.as_ref().map_or(0, |t| t.info.control_epoch),
                    input_allowed: input::owner(&state, &actor, id, None),
                },
                payload: Vec::new(),
            }
            .encode();
            if let Ok(status) = status
                && status != last_status
            {
                last_status.clone_from(&status);
                permit.send(Ok(Bytes::from(status)));
                continue;
            }
            // Clone only after validating authorization; the buffer lives only in this producer/body.
            publication |= sub.frames.has_changed().unwrap_or(false);
            let frame = sub.frames.borrow_and_update().clone();
            if let Some(frame) = frame {
                let meta = &frame.metadata;
                let now = Instant::now();
                tokens = (tokens
                    + now.duration_since(token_at).as_secs_f64()
                        * f64::from(profile.bytes_per_second))
                .min(f64::from(profile.burst_bytes));
                token_at = now;
                let frame_key = (meta.frame_seq, meta.captured_at_ms);
                if frame.validate().is_err()
                    || meta.target_id != target
                    || meta.generation != generation
                    || live.target(target, generation).is_err()
                {
                    break;
                }
                let frame_cost = frame.payload.len().saturating_add(live_view::MAX_HEADER);
                if frame_cost > usize::try_from(profile.burst_bytes).unwrap_or(0)
                    || meta.width > profile.max_width
                    || meta.height > profile.max_height
                {
                    let header = RecordHeader::Status {
                        v: 1,
                        session_id: id,
                        state: ViewState::Unavailable,
                        reason: Some(Reason::FrameBudgetExceeded),
                        effective_profile: profile.clone(),
                        control_epoch: takeover.as_ref().map_or(0, |t| t.info.control_epoch),
                        input_allowed: false,
                    };
                    if let Ok(bytes) = (Record {
                        header,
                        payload: Vec::new(),
                    })
                    .encode()
                    {
                        permit.send(Ok(Bytes::from(bytes)));
                    } else {
                        break;
                    }
                    let mut degraded = profile;
                    degraded.quality = 30;
                    degraded.max_width = degraded.max_width.min(640);
                    degraded.max_height = degraded.max_height.min(400);
                    degraded.max_fps = degraded.max_fps.min(4);
                    if let Some(session) = live.sessions.lock().get_mut(&id) {
                        session.profile = degraded.clone();
                        session.geometry = None;
                        session.healthy_since = None;
                    }
                    let _ = source.set_profile(degraded).await;
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
                // Source publications (including repeated frames) are health evidence only once.
                if publication {
                    let mut sessions = live.sessions.lock();
                    if let Some(session) = sessions.get_mut(&id) {
                        if session
                            .geometry
                            .is_some_and(|(r, _, _)| r != meta.geometry_revision)
                        {
                            session.rendered = None;
                        }
                        let capture_recent = u64::try_from(storage::now_ms())
                            .unwrap_or(0)
                            .saturating_sub(meta.captured_at_ms)
                            <= 2000;
                        if capture_recent || meta.repeated {
                            session.geometry = Some((meta.geometry_revision, meta.frame_seq, now));
                        }
                    }
                }
                let repeated_publication = publication && meta.repeated;
                publication = false;
                let period = if profile.profile == live_view::Profile::Watch {
                    if matches!(
                        meta.cause,
                        live_view::Cause::Action | live_view::Cause::Refresh
                    ) {
                        Duration::ZERO
                    } else {
                        Duration::from_secs(3)
                    }
                } else {
                    Duration::from_secs_f64(1.0 / f64::from(profile.max_fps.max(1)))
                };
                if (last_frame != Some(frame_key) || repeated_publication)
                    && last_sent.elapsed() >= period
                    && tokens >= f64::from(u32::try_from(frame_cost).unwrap_or(u32::MAX))
                {
                    let record = Record {
                        header: RecordHeader::Frame {
                            v: 1,
                            session_id: id,
                            sent_at_ms: u64::try_from(storage::now_ms()).unwrap_or(0),
                            frame: meta.clone(),
                        },
                        payload: frame.payload.to_vec(),
                    };
                    if let Ok(bytes) = record.encode() {
                        tokens -= f64::from(u32::try_from(bytes.len()).unwrap_or(u32::MAX));
                        permit.send(Ok(Bytes::from(bytes)));
                        last_frame = Some(frame_key);
                        last_sent = now;
                        continue;
                    }
                    break;
                }
            }
            if heartbeat_at.elapsed() >= Duration::from_secs(5) {
                if checkpoint_at.elapsed() >= Duration::from_secs(60) {
                    if audit(
                        &state,
                        &actor,
                        "view.checkpoint",
                        serde_json::json!({"session_id":id,"target_id":target}),
                    )
                    .is_err()
                    {
                        break;
                    }
                    checkpoint_at = Instant::now();
                }
                if let Ok(bytes) = (Record {
                    header: RecordHeader::Heartbeat {
                        v: 1,
                        server_time_ms: u64::try_from(storage::now_ms()).unwrap_or(0),
                    },
                    payload: Vec::new(),
                })
                .encode()
                {
                    permit.send(Ok(Bytes::from(bytes)));
                    heartbeat_at = Instant::now();
                    continue;
                }
            }
            drop(permit);
            tokio::select! {
                () = cancel.cancelled() => break,
                () = shutdown.cancelled() => break,
                () = tx.closed() => break,
                () = tokio::time::sleep(Duration::from_millis(100)) => {},
                result = sub.frames.changed() => { if result.is_err() { break; } publication = true; },
            }
        }
        drop(sub);
        drop(tx);
        let _executor = live.executor.lock().await;
        if let Some(session) = live.sessions.lock().get_mut(&id) {
            session.stream = None;
            session.geometry = None;
        }
        if let Some(t) = state.hub().takeover().filter(|t| t.info.session_id == id) {
            let _ = interrupt(&state, &actor, &t, Reason::Disconnected).await;
        }
        // Backends also have a subscription drop callback; stop clears their last frame.
        if !live
            .sessions
            .lock()
            .values()
            .any(|s| s.target == target && s.stream.is_some())
        {
            let _ = tokio::time::timeout(super::BACKEND_CALL_TIMEOUT, source.stop()).await;
        }
        let _ = audit(
            &state,
            &actor,
            "view.stream_ended",
            serde_json::json!({"session_id":id,"target_id":target}),
        );
    });
    let body_stream = futures::stream::unfold(rx, |mut rx| async {
        rx.recv().await.map(|item| (item, rx))
    });
    let mut response = Response::new(Body::from_stream(body_stream));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(MEDIA_TYPE));
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    Ok(response)
}
