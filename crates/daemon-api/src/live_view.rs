//! Authenticated memory-only screen sessions. The `ControlHub` remains the only latch.
pub(crate) mod input;
pub(crate) mod stream;
#[cfg(test)]
mod tests;

use crate::{
    ApiError, AppState,
    control::{OwnedTakeover, ReleaseJournal},
};
use axum::{
    Extension, Json,
    body::Bytes,
    extract::{Path, State},
    http::StatusCode,
};
use live_view::{
    Codec, CreateSession, EffectiveProfile, Heartbeat, HumanAction, Profile, Reason,
    ReleaseRequest, SessionInfo, SessionUpdate, TakeoverAccepted, TakeoverRequest, TargetInfo,
    TargetKind, TargetRegistry, ViewError, ViewState, ViewTarget,
};
use parking_lot::Mutex;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
    time::Duration,
};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub const SESSION_TTL: Duration = Duration::from_secs(30);
/// Identity is installed only after loopback or trusted transport authentication.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Actor {
    pub(crate) key: String,
    pub(crate) recovery: bool,
    /// The authenticated local installation (loopback bearer), never a peer.
    pub(crate) local: bool,
}
impl Actor {
    pub(crate) fn local(s: &AppState) -> Self {
        Self {
            key: format!("local-ui:{}", s.user_id()),
            recovery: true,
            local: true,
        }
    }
    pub(crate) fn peer(p: &crate::PeerIdentity) -> Self {
        Self {
            key: format!("peer:{}", p.key),
            recovery: p.role == crate::PeerRole::Desktop,
            local: false,
        }
    }
}
#[derive(Clone, Copy)]
pub struct ScreenPrivileges {
    pub view: bool,
    pub input: bool,
}
impl Default for ScreenPrivileges {
    fn default() -> Self {
        Self {
            view: true,
            input: true,
        }
    }
}
struct Session {
    actor: String,
    target: Uuid,
    generation: u64,
    profile: EffectiveProfile,
    expires: Instant,
    stream: Option<CancellationToken>,
    geometry: Option<(u64, u64, Instant)>, // revision, frame seq, last live frame
    rendered: Option<u64>,
    healthy_since: Option<Instant>,
    next_seq: u64,
    duplicates: VecDeque<input::Duplicate>,
    counts: input::Counts,
}
/// Policy changes apply to existing streams and every input event.
#[derive(Default)]
pub struct LiveView {
    pub registry: Arc<TargetRegistry>,
    sessions: Mutex<BTreeMap<Uuid, Session>>,
    privileges: Mutex<BTreeMap<String, ScreenPrivileges>>,
    executor: tokio::sync::Mutex<()>,
    rates: Mutex<BTreeMap<String, input::Rate>>,
    pending_input: Arc<std::sync::atomic::AtomicUsize>,
    /// Last written denial audit per `actor|event`, and how many were folded
    /// into it since (see [`audit`]).
    denials: Mutex<BTreeMap<String, DenialWindow>>,
    /// Input batches queued for the executor, per session (see [`INPUT_WAITERS`]).
    input_waiting: Mutex<BTreeMap<Uuid, usize>>,
    /// Woken by a local Stop: queued input batches give up at once, so the
    /// Stop's key reset and audit never wait behind them.
    stop: tokio::sync::Notify,
    /// A key reset is running under the executor; a retry must not stack up
    /// behind it (see `override_takeover`).
    reset_in_flight: std::sync::atomic::AtomicBool,
    /// Test seam: runs in `PUT /v1/control/state` between its takeover check and
    /// the override.
    #[cfg(test)]
    pub(crate) put_state_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    /// Test seam: runs after an input batch is validated under the executor,
    /// right before its first event is applied.
    #[cfg(test)]
    input_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    /// Test seam: runs right after a release suspends input, to land a local
    /// override in the middle of a release.
    #[cfg(test)]
    release_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}
/// Input batches that may queue for the executor: per session and in total.
/// Beyond that the answer is `input_busy` at once.
const INPUT_WAITERS_PER_SESSION: usize = 4;
const INPUT_WAITERS_TOTAL: usize = 16;
/// Bound on each backend call a key reset makes while the executor is held.
const BACKEND_CALL_TIMEOUT: Duration = Duration::from_secs(2);

/// A queued input batch; counted until it holds the executor or gives up.
pub(crate) struct InputWaiter {
    live: Arc<LiveView>,
    session: Uuid,
}
impl Drop for InputWaiter {
    fn drop(&mut self) {
        let mut waiting = self.live.input_waiting.lock();
        if let Some(n) = waiting.get_mut(&self.session) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                waiting.remove(&self.session);
            }
        }
    }
}

impl LiveView {
    /// Reserve a place in the input queue, or `input_busy` immediately.
    pub(crate) fn enter_input_queue(
        self: &Arc<Self>,
        session: Uuid,
    ) -> Result<InputWaiter, ApiError> {
        let mut waiting = self.input_waiting.lock();
        let total: usize = waiting.values().sum();
        let mine = waiting.get(&session).copied().unwrap_or(0);
        if mine >= INPUT_WAITERS_PER_SESSION || total >= INPUT_WAITERS_TOTAL {
            return Err(ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "input_busy",
                "input queue full",
            ));
        }
        *waiting.entry(session).or_default() += 1;
        Ok(InputWaiter {
            live: Arc::clone(self),
            session,
        })
    }

    pub(crate) fn capture_targets(&self) -> std::collections::BTreeSet<Uuid> {
        self.sessions
            .lock()
            .values()
            .filter(|s| s.stream.is_some())
            .map(|s| s.target)
            .collect()
    }

    pub fn with_registry(registry: Arc<TargetRegistry>) -> Self {
        Self {
            registry,
            ..Self::default()
        }
    }

    /// Supply canonical `local-ui:<user UUID>` or `peer:<paired public key>` identities.
    /// Policy callers must also interrupt active takeovers immediately via `AppState`.
    fn set_privileges(&self, actor: String, privileges: ScreenPrivileges) {
        self.privileges.lock().insert(actor, privileges);
    }
    pub(crate) fn allowed(&self, actor: &Actor, input: bool) -> bool {
        // The local user may watch and drive by default. A paired device has
        // neither until the local user grants it, per device.
        let p = self
            .privileges
            .lock()
            .get(&actor.key)
            .copied()
            .unwrap_or(if actor.local {
                ScreenPrivileges::default()
            } else {
                ScreenPrivileges {
                    view: false,
                    input: false,
                }
            });
        p.view && (!input || p.input)
    }
    fn session(&self, actor: &Actor, id: Uuid) -> Result<(Uuid, u64, EffectiveProfile), ApiError> {
        if !self.allowed(actor, false) {
            return Err(ApiError::forbidden("screen:view denied"));
        }
        let sessions = self.sessions.lock();
        let s = sessions
            .get(&id)
            .filter(|s| s.actor == actor.key)
            .ok_or_else(|| ApiError::not_found("view session"))?;
        if s.expires <= Instant::now() {
            return Err(ApiError::new(
                StatusCode::GONE,
                "session_expired",
                "session expired",
            ));
        }
        Ok((s.target, s.generation, s.profile.clone()))
    }
    fn target(&self, id: Uuid, generation: u64) -> Result<Arc<dyn ViewTarget>, ApiError> {
        let target = self
            .registry
            .get(id)
            .ok_or_else(|| ApiError::not_found("view target"))?;
        let info = target.info();
        if info.generation != generation {
            return Err(ApiError::conflict("target generation changed"));
        }
        if !info.capabilities.view || info.unavailable_reason.is_some() {
            return Err(ApiError::unavailable("target unavailable"));
        }
        Ok(target)
    }
    async fn source_profile(&self, target: Uuid) -> Result<(), ApiError> {
        let profiles: Vec<_> = self
            .sessions
            .lock()
            .values()
            .filter(|s| s.target == target && s.stream.is_some() && s.expires > Instant::now())
            .map(|s| s.profile.clone())
            .collect();
        let profile = profiles.iter().cloned().reduce(|mut combined, p| {
            if p.profile == Profile::Live {
                combined.profile = Profile::Live;
            }
            combined.max_width = combined.max_width.min(p.max_width);
            combined.max_height = combined.max_height.min(p.max_height);
            combined.quality = combined.quality.min(p.quality);
            combined.max_fps = combined.max_fps.max(p.max_fps);
            combined.bytes_per_second = combined.bytes_per_second.min(p.bytes_per_second);
            combined.burst_bytes = combined.burst_bytes.min(p.burst_bytes);
            combined
        });
        if let Some(t) = self.registry.get(target) {
            if let Some(p) = profile {
                bounded(t.frame_source().set_profile(p)).await?;
            } else {
                bounded(t.frame_source().stop()).await?;
            }
        }
        Ok(())
    }
}
fn backend(_: ViewError) -> ApiError {
    ApiError::unavailable("view backend unavailable")
}
fn parse<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, ApiError> {
    serde_json::from_slice(bytes).map_err(|_| ApiError::bad_request("invalid live-view request"))
}
/// When a denial was last written, and the denials folded in since, by reason.
type DenialWindow = (Option<Instant>, BTreeMap<String, u64>);

/// Denied attempts of one actor are audited at most once a minute per event;
/// the next row says how many were folded into the silence before it.
const DENIAL_AUDIT_WINDOW: Duration = Duration::from_secs(60);

tokio::task_local! {
    /// Set by a handler's own denial audit, so that `audit_ingress` does not
    /// count the same rejected request a second time.
    static HANDLER_AUDITED: Arc<std::sync::atomic::AtomicBool>;
}

#[allow(clippy::needless_pass_by_value)] // Fixed, ephemeral metadata values are built at each call site.
fn audit(
    s: &AppState,
    actor: &Actor,
    event: &str,
    mut meta: serde_json::Value,
) -> Result<(), ApiError> {
    if matches!(event, "view.denied" | "input.batch_rejected") {
        let _ =
            HANDLER_AUDITED.try_with(|flag| flag.store(true, std::sync::atomic::Ordering::SeqCst));
        let reason = meta
            .get("reason")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unspecified")
            .to_owned();
        let mut denials = s.live_view().denials.lock();
        if denials.len() > 256 {
            denials.clear();
        }
        let now = Instant::now();
        let entry = denials
            .entry(format!("{}|{event}", actor.key))
            .or_insert_with(|| (None, BTreeMap::new()));
        if entry
            .0
            .is_some_and(|at| now.duration_since(at) < DENIAL_AUDIT_WINDOW)
        {
            // Folded into the silence: keep a bounded count per reason.
            let key = if entry.1.contains_key(&reason) || entry.1.len() < 16 {
                reason
            } else {
                "other".to_owned()
            };
            *entry.1.entry(key).or_default() += 1;
            return Ok(());
        }
        let folded = std::mem::take(&mut entry.1);
        entry.0 = Some(now);
        if let Some(object) = meta.as_object_mut() {
            object.insert(
                "suppressed_since_last".into(),
                folded.values().sum::<u64>().into(),
            );
            object.insert("suppressed_by_reason".into(), serde_json::json!(folded));
        }
    }
    s.storage()
        .append_audit(s.user_id(), None, &actor.key, event, &meta)
        .map(|_| ())
        .map_err(|_| ApiError::unavailable("live-view audit unavailable"))
}
fn accepted(t: &OwnedTakeover) -> TakeoverAccepted {
    TakeoverAccepted {
        takeover_id: t.info.id,
        state: t.info.state.clone(),
        control_epoch: t.info.control_epoch,
    }
}

#[utoipa::path(get,path="/v1/view/targets",tag="view",security(("bearer"=[])),responses((status=200,body=Vec<TargetInfo>)))]
pub(crate) async fn targets(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
) -> Result<Json<Vec<TargetInfo>>, ApiError> {
    if !s.live_view().allowed(&actor, false) {
        audit(
            &s,
            &actor,
            "view.denied",
            serde_json::json!({"reason":"privilege"}),
        )?;
        return Err(ApiError::forbidden("screen:view denied"));
    }
    Ok(Json(s.live_view().registry.targets()))
}
#[utoipa::path(post,path="/v1/view/sessions",tag="view",security(("bearer"=[])),request_body=CreateSession,responses((status=201,body=SessionInfo)))]
pub(crate) async fn create_session(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    body: Bytes,
) -> Result<(StatusCode, Json<SessionInfo>), ApiError> {
    let i: CreateSession = parse(&body)?;
    let live = s.live_view();
    if !live.allowed(&actor, false) {
        audit(
            &s,
            &actor,
            "view.denied",
            serde_json::json!({"reason":"privilege"}),
        )?;
        return Err(ApiError::forbidden("screen:view denied"));
    }
    let target = live
        .registry
        .get(i.target_id)
        .ok_or_else(|| ApiError::not_found("view target"))?;
    let info = target.info();
    if !info.capabilities.view || info.unavailable_reason.is_some() {
        audit(
            &s,
            &actor,
            "view.denied",
            serde_json::json!({"target_id":i.target_id,"reason":"unavailable"}),
        )?;
        return Err(ApiError::unavailable("target unavailable"));
    }
    if i.profile != Profile::Watch
        || !i.codecs.contains(&Codec::Jpeg)
        || !info.capabilities.codecs.contains(&Codec::Jpeg)
        || i.max_width == 0
        || i.max_height == 0
    {
        return Err(ApiError::bad_request(
            "watch/jpeg and nonzero dimensions required",
        ));
    }
    let mut p = EffectiveProfile::watch();
    p.max_width = p.max_width.min(i.max_width);
    p.max_height = p.max_height.min(i.max_height);
    let id = Uuid::now_v7();
    {
        let mut sessions = live.sessions.lock();
        // Expired entries are removed by the lifecycle reaper, never revived here.
        if sessions.len() >= 4
            || sessions.values().filter(|s| s.actor == actor.key).count() >= 2
            || (!sessions.values().any(|s| s.target == i.target_id)
                && sessions
                    .values()
                    .map(|s| s.target)
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    >= 2)
        {
            return Err(ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "view_quota",
                "view session quota exceeded",
            ));
        }
        audit(
            &s,
            &actor,
            "view.started",
            serde_json::json!({"session_id":id,"target_id":i.target_id}),
        )?;
        sessions.insert(
            id,
            Session {
                actor: actor.key.clone(),
                target: i.target_id,
                generation: info.generation,
                profile: p.clone(),
                expires: Instant::now() + SESSION_TTL,
                stream: None,
                geometry: None,
                rendered: None,
                healthy_since: None,
                next_seq: 1,
                duplicates: VecDeque::new(),
                counts: input::Counts::default(),
            },
        );
    }
    // Per-session task owns only a Weak<AppState>; it cannot keep the daemon alive.
    stream::spawn_reaper(&s, id, actor.clone());
    Ok((
        StatusCode::CREATED,
        Json(SessionInfo {
            session_id: id,
            generation: info.generation,
            effective_profile: p,
            input_capabilities: info.capabilities.input && live.allowed(&actor, true),
            stream_url: format!("/v1/view/sessions/{id}/frames"),
            expires_in_ms: 30_000,
        }),
    ))
}
#[utoipa::path(patch,path="/v1/view/sessions/{id}",tag="view",security(("bearer"=[])),params(("id"=Uuid,Path)),request_body=SessionUpdate,responses((status=200,body=EffectiveProfile)))]
pub(crate) async fn update_session(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
    body: Bytes,
) -> Result<Json<EffectiveProfile>, ApiError> {
    let i: SessionUpdate = parse(&body)?;
    let live = s.live_view();
    let _executor = live.executor.lock().await;
    let (target, generation, old) = live.session(&actor, id)?;
    let t = live.target(target, generation)?;
    let is_live = i.profile.as_ref().unwrap_or(&old.profile) == &Profile::Live;
    if is_live && !input::owner(&s, &actor, id, None) {
        return Err(ApiError::forbidden("live profile requires active takeover"));
    }
    let mut p = if is_live {
        EffectiveProfile::live()
    } else {
        EffectiveProfile::watch()
    };
    p.max_width = i.max_width.unwrap_or(old.max_width).clamp(1, p.max_width);
    p.max_height = i
        .max_height
        .unwrap_or(old.max_height)
        .clamp(1, p.max_height);
    p.quality = i.quality.unwrap_or(p.quality).clamp(20, p.quality);
    p.bytes_per_second = i
        .bytes_per_second
        .unwrap_or(p.bytes_per_second)
        .clamp(16 * 1024, p.bytes_per_second);
    p.burst_bytes = p.burst_bytes.min(p.bytes_per_second.saturating_mul(2));
    let unhealthy =
        i.decode_ms.is_some_and(|v| v > 100) || i.buffered_frames.is_some_and(|v| v > 1);
    if unhealthy {
        p.quality = p.quality.min(35);
        p.max_width = p.max_width.min(640);
        p.max_height = p.max_height.min(400);
        p.max_fps = p.max_fps.min(4);
    }
    {
        let mut sessions = live.sessions.lock();
        let session = sessions
            .get_mut(&id)
            .ok_or_else(|| ApiError::not_found("view session"))?;
        if unhealthy {
            session.healthy_since = None;
        } else if old.quality < p.quality || old.max_fps < p.max_fps {
            let since = session.healthy_since.get_or_insert_with(Instant::now);
            if since.elapsed() < Duration::from_secs(10) {
                p.quality = p.quality.min(old.quality);
                p.max_width = p.max_width.min(old.max_width);
                p.max_height = p.max_height.min(old.max_height);
                p.max_fps = p.max_fps.min(old.max_fps);
            } else {
                p.quality = p.quality.min(old.quality.saturating_add(5));
                p.max_fps = p.max_fps.min(old.max_fps.saturating_add(1));
                session.healthy_since = Some(Instant::now());
            }
        }
        session.profile = p.clone();
        session.rendered = i.last_rendered_frame_seq;
    }
    live.source_profile(target).await?;
    bounded(t.frame_source().request_refresh(0)).await?;
    Ok(Json(p))
}
#[utoipa::path(put,path="/v1/view/sessions/{id}/heartbeat",tag="view",security(("bearer"=[])),params(("id"=Uuid,Path)),request_body=Heartbeat,responses((status=204)))]
pub(crate) async fn heartbeat(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    let i: Heartbeat = parse(&body)?;
    let live = s.live_view();
    let _executor = live.executor.lock().await;
    live.session(&actor, id)?;
    retry_release_audits(&s);
    retry_owed_reset(&s).await;
    match (i.takeover_id, i.control_epoch) {
        (Some(t), Some(e)) if live.allowed(&actor, true) => s
            .hub()
            .renew_input(&actor.key, id, t, e)
            .map_err(ApiError::conflict)?,
        (None, None) => {}
        _ => return Err(ApiError::conflict("invalid takeover heartbeat")),
    }
    if let Some(session) = live.sessions.lock().get_mut(&id) {
        session.expires = Instant::now() + SESSION_TTL;
    }
    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(delete,path="/v1/view/sessions/{id}",tag="view",security(("bearer"=[])),params(("id"=Uuid,Path)),responses((status=204)))]
pub(crate) async fn delete_session(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    s.live_view().session(&actor, id)?;
    end_session(&s, &actor, id, Reason::Disconnected).await?;
    Ok(StatusCode::NO_CONTENT)
}
/// Reset pressed inputs of the takeover's target and drop live capture.
/// `false` when the key reset itself failed.
async fn reset_target(s: &AppState, t: &OwnedTakeover) -> bool {
    struct InFlight<'a>(&'a std::sync::atomic::AtomicBool);
    impl Drop for InFlight<'_> {
        fn drop(&mut self) {
            self.0.store(false, std::sync::atomic::Ordering::SeqCst);
        }
    }
    let live = s.live_view();
    live.reset_in_flight
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let _in_flight = InFlight(&live.reset_in_flight);
    // The session no longer claims a live profile: the combined profile (and
    // with it the stream rate) follows the sessions, whatever the backend does.
    if let Some(session) = live.sessions.lock().get_mut(&t.info.session_id) {
        session.profile = EffectiveProfile::watch();
    }
    // ONE budget for the whole reset: a hung backend holds the executor for
    // BACKEND_CALL_TIMEOUT in total, not once per call.
    let deadline = Instant::now() + BACKEND_CALL_TIMEOUT;
    let mut reset_ok = true;
    if let Some(target) = live.registry.get(t.info.target_id) {
        if let Some(sink) = target.input_sink() {
            reset_ok &= matches!(
                tokio::time::timeout_at(deadline, sink.release_pressed_inputs()).await,
                Ok(Ok(()))
            );
            // The reset is only complete when the backend also dropped its
            // takeover pin and live profile, like a release requires.
            reset_ok &= matches!(
                tokio::time::timeout_at(deadline, sink.set_takeover_active(false)).await,
                Ok(Ok(()))
            );
        }
        reset_ok &= matches!(
            tokio::time::timeout_at(
                deadline,
                target.frame_source().set_profile(EffectiveProfile::watch()),
            )
            .await,
            Ok(Ok(()))
        );
    }
    reset_ok
}

/// A backend call made while the executor is held, bounded by
/// [`BACKEND_CALL_TIMEOUT`]. A timeout drops the future (backends must be
/// cancel-safe, see `live_view::InputSink`).
async fn bounded<T>(
    call: impl std::future::Future<Output = Result<T, ViewError>>,
) -> Result<T, ApiError> {
    match tokio::time::timeout(BACKEND_CALL_TIMEOUT, call).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(e)) => Err(backend(e)),
        Err(_) => Err(ApiError::unavailable(
            "the display backend did not answer in time; the release is paused and can be retried with the same request_id",
        )),
    }
}

/// The audit row records the incoming reason and the one the hub actually
/// holds (a local override is sticky, so they can differ).
fn audit_interrupted(
    s: &AppState,
    actor: &Actor,
    t: &OwnedTakeover,
    reason: &Reason,
) -> Result<(), ApiError> {
    let effective = s
        .hub()
        .takeover()
        .filter(|now| now.info.id == t.info.id)
        .and_then(|now| now.info.reason);
    audit(
        s,
        actor,
        "takeover.interrupted",
        serde_json::json!({"target_id":t.info.target_id,"takeover_id":t.info.id,"control_epoch":t.info.control_epoch,"reason":reason,"effective_reason":effective}),
    )
}

async fn interrupt(
    s: &AppState,
    actor: &Actor,
    t: &OwnedTakeover,
    reason: Reason,
) -> Result<(), ApiError> {
    // Mark input invalid before any fallible reset/audit operation.
    s.hub().interrupt(t.info.id, reason.clone());
    let reset_ok = reset_target(s, t).await;
    if reset_ok {
        // Also clears a reset that an earlier interruption still owed.
        if s.hub().override_pending().0 {
            s.hub().resolve_override_reset();
        }
    } else {
        s.hub().interrupt(t.info.id, Reason::ResetFailed);
        // Keys may still be pressed: owed until a retry confirms it.
        s.hub().mark_reset_pending();
    }
    let audited = audit_interrupted(s, actor, t, &reason);
    if audited.is_ok() && reason == Reason::LocalOverride {
        s.hub().resolve_override_audit();
    }
    audited
}
async fn end_session(
    s: &AppState,
    actor: &Actor,
    id: Uuid,
    reason: Reason,
) -> Result<(), ApiError> {
    let live = s.live_view();
    let _executor = live.executor.lock().await;
    let removed = live.sessions.lock().remove(&id);
    if let Some(session) = removed {
        if let Some(cancel) = session.stream {
            cancel.cancel();
        }
        if let Some(t) = s.hub().takeover().filter(|t| t.info.session_id == id) {
            interrupt(s, actor, &t, reason.clone()).await?;
        }
        live.source_profile(session.target).await?;
        audit(
            s,
            actor,
            "view.ended",
            serde_json::json!({"session_id":id,"target_id":session.target,"reason":reason}),
        )?;
        if !live.sessions.lock().values().any(|s| s.actor == actor.key) {
            live.rates.lock().remove(&actor.key);
        }
    }
    Ok(())
}
#[utoipa::path(post,path="/v1/control/takeover",tag="control",security(("bearer"=[])),request_body=TakeoverRequest,responses((status=202,body=TakeoverAccepted)))]
#[allow(clippy::too_many_lines)] // Validate, reset, persist and activate one owned takeover.
pub(crate) async fn takeover(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    body: Bytes,
) -> Result<(StatusCode, Json<TakeoverAccepted>), ApiError> {
    let i: TakeoverRequest = parse(&body)?;
    let live = s.live_view();
    let _executor = live.executor.lock().await;
    let (target, generation, _) = live.session(&actor, i.session_id)?;
    let target_impl = live.target(target, generation)?;
    if live.pending_input.load(std::sync::atomic::Ordering::SeqCst) != 0 {
        return Err(ApiError::conflict("previous input still draining"));
    }
    // The local user's override outranks every paired device: neither the
    // overridden peer (reacquire) nor another one (recover) may re-take the
    // screen, and a fresh grant alone never suffices: only the local user
    // clears it (`recover` or release), after which a new takeover also needs
    // the grant.
    if !actor.local
        && s.hub()
            .takeover()
            .is_some_and(|t| t.info.reason == Some(Reason::LocalOverride))
    {
        audit(
            &s,
            &actor,
            "view.denied",
            serde_json::json!({"target_id":target,"reason":"local_override"}),
        )?;
        return Err(ApiError::forbidden(
            "the local user stopped the takeover; only the local user can clear it",
        ));
    }
    if !live.allowed(&actor, true)
        || !target_impl.info().capabilities.input
        || target_impl.input_sink().is_none()
    {
        audit(
            &s,
            &actor,
            "view.denied",
            serde_json::json!({"target_id":target,"reason":"input_privilege"}),
        )?;
        return Err(ApiError::forbidden("screen:input denied"));
    }
    // Idempotent lost-response retry does not reset an acquiring/active owner.
    if let Some(old) = s
        .hub()
        .takeover()
        .filter(|old| old.request_id == i.request_id)
    {
        if old.actor != actor.key || old.info.session_id != i.session_id {
            return Err(ApiError::conflict("request conflict"));
        }
        return Ok((StatusCode::ACCEPTED, Json(accepted(&old))));
    }
    // Recover/reset the old sink before transferring ownership to a new epoch.
    if let Some(old) = s.hub().takeover()
        && (old.info.state == ViewState::Paused
            || (old.info.state == ViewState::Active && old.info.input_expires_in_ms == 0))
    {
        let allowed = (old.actor == actor.key && i.reacquire_takeover_id == Some(old.info.id))
            || (actor.recovery && i.recover && i.reason.is_some());
        if !allowed {
            return Err(ApiError::conflict(
                "explicit reacquisition/recovery required",
            ));
        }
        if live.registry.get(old.info.target_id).is_none() {
            return Err(ApiError::unavailable("old target reset cannot be verified"));
        }
    }
    if i.recover || i.reacquire_takeover_id.is_some() {
        for mut journal in s.hub().pending_release_notes() {
            flush_note(&s, &mut journal)?;
        }
    }
    // The hub decides first: a takeover it rejects leaves no audit row and
    // touches no backend. Everything below that fails rolls the new takeover
    // back (it is interrupted, so the bot stays paused, never half-taken).
    let previous = s.hub().takeover().filter(|old| {
        old.info.state == ViewState::Paused
            || (old.info.state == ViewState::Active && old.info.input_expires_in_ms == 0)
    });
    // A pending `takeover.interrupted` row of the takeover being replaced is
    // written now: `begin_takeover` drops the flag with it.
    if let Some(old) = &previous
        && s.hub().override_pending().1
        && let Some(reason) = old.info.reason.clone()
    {
        audit_interrupted(&s, &actor, old, &reason)?;
        s.hub().resolve_override_audit();
    }
    let owned = s
        .hub()
        .begin_takeover(
            &actor.key,
            &i,
            target,
            generation,
            actor.recovery,
            actor.local,
        )
        .map_err(ApiError::conflict)?;
    let setup: Result<(), (Reason, ApiError)> = async {
        // Reset the old owner's sink before the new one may drive.
        if let Some(old) = &previous
            && let Some(sink) = live
                .registry
                .get(old.info.target_id)
                .and_then(|t| t.input_sink())
        {
            bounded(sink.release_pressed_inputs())
                .await
                .map_err(|e| (Reason::ResetFailed, e))?;
            bounded(sink.set_takeover_active(false))
                .await
                .map_err(|e| (Reason::ResetFailed, e))?;
        }
        audit(
            &s,
            &actor,
            if i.recover {
                "takeover.recovered"
            } else {
                "takeover.requested"
            },
            serde_json::json!({"session_id":i.session_id,"target_id":target,"request_id":i.request_id,"recovery_reason":i.reason}),
        )
        .map_err(|e| (Reason::AuditFailed, e))
    }
    .await;
    if let Err((reason, error)) = setup {
        let _ = interrupt(&s, &actor, &owned, reason).await;
        return Err(error);
    }
    let result = accepted(&owned);
    if owned.info.state == ViewState::Acquiring {
        let state = s.clone();
        let actor = actor.clone();
        tokio::spawn(async move {
            if state
                .hub()
                .drain(owned.info.id, owned.info.control_epoch)
                .await
                .is_err()
            {
                return;
            }
            let live = state.live_view();
            let _executor = live.executor.lock().await;
            // The wait for the executor can be long: act only if this exact
            // takeover (id, epoch) is still the one being acquired. A local
            // Stop, a recovery or a newer takeover must never be re-pinned.
            if !state.hub().takeover().is_some_and(|now| {
                now.info.id == owned.info.id
                    && now.info.control_epoch == owned.info.control_epoch
                    && now.info.state == ViewState::Acquiring
            }) {
                return;
            }
            if live.session(&actor, owned.info.session_id).is_err() || !live.allowed(&actor, true) {
                let _ = interrupt(&state, &actor, &owned, Reason::Expired).await;
                return;
            }
            let activation = async {
                let target = live.target(owned.info.target_id,owned.generation)?;
                let sink = target.input_sink().ok_or_else(|| ApiError::unavailable("input unavailable"))?;
                bounded(sink.release_pressed_inputs()).await?;
                bounded(sink.set_takeover_active(true)).await?;
                // Re-verified after the backend call: no `takeover.active` row
                // for a takeover a Stop ended meanwhile (the caller resets the pin).
                if !state.hub().takeover().is_some_and(|now| now.info.id == owned.info.id && now.info.control_epoch == owned.info.control_epoch && now.info.state == ViewState::Acquiring) {
                    return Err(ApiError::conflict("the takeover ended while it was being activated"));
                }
                audit(&state,&actor,"takeover.active",serde_json::json!({"takeover_id":owned.info.id,"control_epoch":owned.info.control_epoch,"target_id":owned.info.target_id}))?;
                state.hub().activate(owned.info.id,owned.info.control_epoch).map_err(ApiError::conflict)?;
                if let Some(session) = live.sessions.lock().get_mut(&owned.info.session_id) { session.profile = EffectiveProfile::live(); session.next_seq = 1; session.duplicates.clear(); }
                live.source_profile(owned.info.target_id).await?;
                bounded(target.frame_source().request_refresh(0)).await
            }.await;
            if activation.is_err() {
                let _ = interrupt(&state, &actor, &owned, Reason::Unavailable).await;
            }
        });
    }
    Ok((StatusCode::ACCEPTED, Json(result)))
}

/// Releasing or changing a latch the caller does not own: the local user, or a
/// peer that currently holds an explicit per-device `input` grant. A latch left
/// by a local override is local-only whatever the grants.
pub(crate) fn require_latch_authority(s: &AppState, actor: &Actor) -> Result<(), ApiError> {
    if actor.local {
        return Ok(());
    }
    let overridden = s
        .hub()
        .takeover()
        .is_some_and(|t| t.info.reason == Some(Reason::LocalOverride));
    if overridden || !s.live_view().allowed(actor, true) {
        audit(
            s,
            actor,
            "view.denied",
            serde_json::json!({"reason":"latch_authority"}),
        )?;
        return Err(ApiError::forbidden(
            "this needs the local user or an input grant for this device",
        ));
    }
    Ok(())
}

pub(crate) fn require_local(s: &AppState, actor: &Actor, what: &str) -> Result<(), ApiError> {
    if actor.local {
        return Ok(());
    }
    audit(
        s,
        actor,
        "view.denied",
        serde_json::json!({"reason":"local_only","operation":what}),
    )?;
    Err(ApiError::forbidden("only the local user may do this"))
}

/// The local user stops any takeover, whoever owns it: input is invalidated
/// first (without waiting for the input executor), pressed inputs are reset,
/// the bot stays paused. A pause without takeover is untouched here.
async fn override_takeover(
    s: &AppState,
    actor: &Actor,
    reason: Reason,
) -> Result<Option<OwnedTakeover>, ApiError> {
    // Idempotent: an app heartbeat every few seconds must not re-run the
    // latch write, the event, the audit row and the key reset.
    let live = s.live_view();
    let stopped = |t: &OwnedTakeover| {
        t.info.state == ViewState::Paused && t.info.reason == Some(reason.clone())
    };
    if s.hub().takeover().is_some_and(|t| stopped(&t)) {
        // Already stopped. Only work that has not succeeded yet (a key reset,
        // an audit row, the latch-file write) is retried, until it does.
        let (reset_pending, audit_pending) = s.hub().override_pending();
        if !reset_pending && !audit_pending && !s.hub().persist_dirty() {
            return Ok(None);
        }
        // A retry is already running: do not stack another behind it.
        if reset_pending
            && live
                .reset_in_flight
                .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(ApiError::unavailable(
                "a key reset is already in progress; it will be retried",
            ));
        }
        live.stop.notify_waiters();
        let Ok(_executor) = tokio::time::timeout(BACKEND_CALL_TIMEOUT, live.executor.lock()).await
        else {
            return Err(ApiError::unavailable(
                "the executor is busy; the pending key reset will be retried",
            ));
        };
        // Read again under the lock: a queued heartbeat that ran first may
        // already have finished the work (no double audit row), and the
        // takeover may have been recovered or replaced meanwhile.
        let (reset_pending, audit_pending) = s.hub().override_pending();
        let Some(t) = s.hub().takeover().filter(|t| stopped(t)) else {
            return Ok(None);
        };
        let _ = s.hub().retry_persist();
        if reset_pending && reset_target(s, &t).await {
            s.hub().resolve_override_reset();
        }
        if audit_pending {
            audit_interrupted(s, actor, &t, &reason)?;
            s.hub().resolve_override_audit();
        }
        return reset_confirmed(s, reason == Reason::LocalOverride).map(|()| None);
    }
    // 1. Revoke the overridden device's input grant FIRST, so that nothing
    // below (a failing audit write, a failing key reset) can leave it in
    // place. Set directly, not through `set_screen_privileges`.
    if reason == Reason::LocalOverride
        && let Some(owner) = s.hub().takeover().map(|t| t.actor)
        && owner.starts_with("peer:")
    {
        let keep_view = live.privileges.lock().get(&owner).is_none_or(|p| p.view);
        live.set_privileges(
            owner,
            ScreenPrivileges {
                view: keep_view,
                input: false,
            },
        );
    }
    // 2. Stop the takeover (the reason is sticky inside the hub).
    let Some(old) = s.hub().force_interrupt(reason.clone()) else {
        return Ok(None);
    };
    // Queued input batches give up at once: the Stop must not wait behind them.
    live.stop.notify_waiters();
    // 3. Reset pressed inputs and audit. Failures are reported, but the stop
    // and the revoke above already happened. The wait for the executor is
    // bounded too: a wedged executor can never hang the Stop's HTTP call; the
    // reset stays pending (already persisted with the stop) and is retried.
    let Ok(_executor) = tokio::time::timeout(BACKEND_CALL_TIMEOUT, live.executor.lock()).await
    else {
        return Err(ApiError::unavailable(
            "stopped, but the executor is busy; the pending key reset will be retried",
        ));
    };
    // Under the executor again: only act on the takeover this Stop stopped,
    // and only if its work is still owed. Another task may have recovered it,
    // started a new takeover, or already done the reset and the audit.
    let Some(t) = s.hub().takeover().filter(|t| t.info.id == old.info.id) else {
        return Ok(Some(old));
    };
    let local = reason == Reason::LocalOverride;
    let (reset_pending, audit_pending) = s.hub().override_pending();
    if local && !reset_pending && !audit_pending {
        return reset_confirmed(s, local).map(|()| Some(old));
    }
    interrupt(s, actor, &t, reason).await?;
    reset_confirmed(s, local)?;
    Ok(Some(old))
}

/// The Stop took effect, but if the key reset is not confirmed (failed or timed
/// out) say so: it stays pending and is retried by the next `paused` heartbeat.
fn reset_confirmed(s: &AppState, local_override: bool) -> Result<(), ApiError> {
    if local_override && s.hub().override_pending().0 {
        return Err(ApiError::unavailable(
            "stopped, but the pressed-key reset did not complete; it will be retried",
        ));
    }
    if local_override && s.hub().persist_dirty() {
        // Never a clean answer for a stop the disk does not know about: it is
        // in force in memory and the write is retried by the next heartbeat.
        return Err(ApiError::unavailable(
            "stopped, but the latch file could not be written; it will be retried",
        ));
    }
    Ok(())
}

/// The app's `PUT /v1/control/state {mode: paused}` while a takeover is owned.
pub(crate) async fn override_local_pause(s: &AppState, actor: &Actor) -> Result<(), ApiError> {
    override_takeover(s, actor, Reason::LocalOverride).await?;
    Ok(())
}

/// `POST /v1/control/override`: the local user's stop. Works against a peer's
/// takeover; the bot stays paused until `release` or `recover`.
#[utoipa::path(post,path="/v1/control/override",tag="control",security(("bearer"=[])),responses((status=200,body=crate::dto::ControlStateDto),(status=403,body=crate::error::ErrorBody)))]
pub(crate) async fn local_override(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
) -> Result<Json<crate::dto::ControlStateDto>, ApiError> {
    require_local(&s, &actor, "override")?;
    override_takeover(&s, &actor, Reason::LocalOverride).await?;
    Ok(Json(crate::routes::control_state_dto(&s, Some(&actor))))
}

/// `POST /v1/control/recover`: the local user clears a takeover latch (also one
/// restored after a restart) without a registered target or view session. The
/// bot resumes with a fresh observation.
#[utoipa::path(post,path="/v1/control/recover",tag="control",security(("bearer"=[])),responses((status=200,body=crate::dto::ControlStateDto),(status=403,body=crate::error::ErrorBody),(status=409,body=crate::error::ErrorBody)))]
pub(crate) async fn local_recover(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
) -> Result<Json<crate::dto::ControlStateDto>, ApiError> {
    require_local(&s, &actor, "recover")?;
    // The stop is in force either way; an unconfirmed key reset must not block
    // clearing the latch (the target is reset again best-effort below).
    let _ = override_takeover(&s, &actor, Reason::LocalOverride).await;
    let live = s.live_view();
    let Ok(_executor) = tokio::time::timeout(BACKEND_CALL_TIMEOUT, live.executor.lock()).await
    else {
        return Err(ApiError::unavailable("the executor is busy; try again"));
    };
    let Some(owned) = s.hub().takeover() else {
        // No owner, but a latch that demands recovery (an unreadable latch file).
        if s.hub().unowned_recovery_required() {
            audit(
                &s,
                &actor,
                "takeover.recovered",
                serde_json::json!({"by":"local","reason":"unreadable_latch"}),
            )?;
            s.hub()
                .clear_unowned_recovery()
                .map_err(ApiError::unavailable)?;
            return Ok(Json(crate::routes::control_state_dto(&s, Some(&actor))));
        }
        return Err(ApiError::conflict("no takeover is recorded"));
    };
    // Everything that can fail happens BEFORE the latch is cleared: the human
    // must not resume a bot while a note is owed or keys may still be pressed.
    for mut journal in s.hub().pending_release_notes() {
        flush_note(&s, &mut journal)?;
    }
    // A target that is gone has nothing to reset; one that is present must
    // confirm the key reset, takeover pin and watch profile.
    if live.registry.get(owned.info.target_id).is_some() && !reset_target(&s, &owned).await {
        return Err(ApiError::unavailable(
            "the display target did not confirm the key reset; the latch is kept, try again",
        ));
    }
    audit(
        &s,
        &actor,
        "takeover.recovered",
        serde_json::json!({"by":"local","reason":"local_recovery"}),
    )?;
    s.hub()
        .clear_owned_latch()
        .map_err(ApiError::unavailable)?
        .ok_or_else(|| ApiError::conflict("no takeover is recorded"))?;
    Ok(Json(crate::routes::control_state_dto(&s, Some(&actor))))
}

/// `PUT /v1/view/privileges/{peer_key}`: the local user grants or revokes
/// `screen:view` / `screen:input` for one paired device. A paired device has
/// neither until granted; revoking interrupts its sessions and takeover.
#[utoipa::path(put,path="/v1/view/privileges/{peer_key}",tag="view",security(("bearer"=[])),params(("peer_key"=String,Path)),request_body=crate::dto::ScreenPrivilegesInput,responses((status=204),(status=403,body=crate::error::ErrorBody)))]
pub(crate) async fn set_privileges(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    Path(peer_key): Path<String>,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    require_local(&s, &actor, "privileges")?;
    let i: crate::dto::ScreenPrivilegesInput = parse(&body)?;
    if peer_key.is_empty()
        || peer_key.len() > 128
        || !peer_key.chars().all(|c| c.is_ascii_alphanumeric())
        || (i.input && !i.view)
    {
        return Err(ApiError::bad_request("invalid device key or privileges"));
    }
    audit(
        &s,
        &actor,
        "view.privileges_set",
        serde_json::json!({"peer":peer_key,"view":i.view,"input":i.input}),
    )?;
    s.set_screen_privileges(
        format!("peer:{peer_key}"),
        ScreenPrivileges {
            view: i.view,
            input: i.input,
        },
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Release is serialized with every input event, including key reset and durable outbox.
#[allow(clippy::too_many_lines)] // One serialized durable release transaction.
pub(crate) async fn release(
    s: &AppState,
    actor: &Actor,
    i: ReleaseRequest,
) -> Result<(), ApiError> {
    let live = s.live_view();
    let Ok(_executor) = tokio::time::timeout(BACKEND_CALL_TIMEOUT, live.executor.lock()).await
    else {
        return Err(ApiError::unavailable(
            "the executor is busy; retry the release",
        ));
    };
    if !live.allowed(actor, true) {
        return Err(ApiError::forbidden("screen:input denied"));
    }
    ensure_not_overridden(s)?;
    if let Some(old) = s.hub().release_journal(i.request_id) {
        if old.actor != actor.key
            || old.takeover_id != i.takeover_id
            || old.epoch != i.control_epoch
            || old.human_action != i.human_action
        {
            return Err(ApiError::conflict("release request conflict"));
        }
        if old.cancelled {
            return Err(override_stands());
        }
        if old.completed {
            // Completed, but its audit row may still be owed (the first
            // attempt failed after the latch was cleared): write it now.
            write_released_audit(s, &old)?;
            return Ok(());
        }
    }
    let t = s
        .hub()
        .takeover()
        .filter(|t| {
            t.actor == actor.key
                && t.info.id == i.takeover_id
                && t.info.control_epoch == i.control_epoch
        })
        .ok_or_else(|| ApiError::forbidden("takeover owner required"))?;
    live.session(actor, t.info.session_id)?;
    let target = live.target(t.info.target_id, t.generation)?;
    let sink = target
        .input_sink()
        .ok_or_else(|| ApiError::unavailable("input unavailable"))?;
    if live.pending_input.load(std::sync::atomic::Ordering::SeqCst) != 0 {
        return Err(ApiError::conflict("previous input still draining"));
    }
    // Input expires before awaiting OS reset. Reset failure retains pause.
    if t.info.state != ViewState::Active
        && t.info.reason.is_some()
        && s.hub().release_journal(i.request_id).is_none()
    {
        return Err(ApiError::conflict(
            "explicit reacquisition required before release",
        ));
    }
    s.hub().suspend_release(t.info.id);
    #[cfg(test)]
    {
        let hook = live.release_hook.lock().clone();
        if let Some(hook) = hook {
            hook();
        }
    }
    // After a timed-out or failed backend call the takeover is left paused with
    // input dead (`suspend_release` above); nothing else was applied. The
    // journal and the chat note are idempotent by request id, so the same
    // request can simply be retried.
    bounded(sink.release_pressed_inputs()).await?;
    // A local Stop may have landed while the reset was awaited: stop before any
    // side effect that cannot be undone (the chat note, the resume).
    ensure_not_overridden(s)?;
    let (counts, kind) = {
        let sessions = live.sessions.lock();
        let session = sessions
            .get(&t.info.session_id)
            .ok_or_else(|| ApiError::not_found("view session"))?;
        (session.counts.clone(), target.info().kind)
    };
    let declared = match i.human_action {
        Some(HumanAction::LoginCompleted) => agent_core::control_note::Declared::LoginCompleted,
        Some(HumanAction::ApplicationOpened) => {
            agent_core::control_note::Declared::ApplicationOpened
        }
        _ => agent_core::control_note::Declared::Intervened,
    };
    let summary = agent_core::control_note::build(
        declared,
        agent_core::control_note::Counts {
            pointer: counts.pointer,
            wheel: counts.wheel,
            key: counts.key,
            text: counts.text,
        },
        kind == TargetKind::Browser,
    );
    let mut journal = s
        .hub()
        .prepare_release(ReleaseJournal {
            takeover_id: i.takeover_id,
            epoch: i.control_epoch,
            request_id: i.request_id,
            actor: actor.key.clone(),
            human_action: i.human_action,
            summary,
            runs: target
                .info()
                .run_id
                .map(protocol::RunId)
                .into_iter()
                .collect(),
            messages: Vec::new(),
            completed: false,
            noted: false,
            cancelled: false,
            audit_pending: false,
            target: Some(t.info.target_id),
            counts: serde_json::to_value(&counts).ok(),
        })
        .map_err(ApiError::unavailable)?;
    ensure_not_overridden(s)?;
    flush_note(s, &mut journal)?;
    bounded(sink.set_takeover_active(false)).await?;
    bounded(target.frame_source().set_profile(EffectiveProfile::watch())).await?;
    s.hub()
        .finish_release(&actor.key, i.request_id)
        .map_err(|e| {
            if e == "local override stands" {
                override_stands()
            } else {
                ApiError::unavailable(e)
            }
        })?;
    if let Some(session) = live.sessions.lock().get_mut(&t.info.session_id) {
        session.profile = EffectiveProfile::watch();
    }
    // Recorded once the release really happened (never for one that failed or
    // lost to a local Stop). `finish_release` marked the audit as owed in the
    // write that completed the journal: if this write fails, the marker stays
    // and a retry (or a heartbeat) writes the row.
    audit(
        s,
        actor,
        "takeover.released",
        serde_json::json!({"target_id":t.info.target_id,"takeover_id":t.info.id,"control_epoch":t.info.control_epoch,"request_id":i.request_id,"counts":counts}),
    )?;
    s.hub().resolve_release_audit(i.request_id);
    Ok(())
}

/// Write the owed `takeover.released` row of a completed release (idempotent:
/// the marker is cleared only after the row exists).
fn write_released_audit(s: &AppState, journal: &ReleaseJournal) -> Result<(), ApiError> {
    if !journal.audit_pending {
        return Ok(());
    }
    let actor = Actor {
        key: journal.actor.clone(),
        recovery: false,
        local: false,
    };
    audit(
        s,
        &actor,
        "takeover.released",
        serde_json::json!({"target_id":journal.target,"takeover_id":journal.takeover_id,"control_epoch":journal.epoch,"request_id":journal.request_id,"counts":journal.counts,"retried":true}),
    )?;
    s.hub().resolve_release_audit(journal.request_id);
    Ok(())
}

/// A key reset that failed after an interruption (a partial injection, a
/// disconnect) is owed until a retry confirms it; session heartbeats retry it.
async fn retry_owed_reset(s: &AppState) {
    let live = s.live_view();
    if !s.hub().override_pending().0
        || live
            .reset_in_flight
            .load(std::sync::atomic::Ordering::SeqCst)
    {
        return;
    }
    // Only an interrupted (paused) takeover can owe a reset: never a live one.
    // And only once its target is registered: a target that is not there yet
    // (just after a restart) has not been reset by this daemon.
    if let Some(t) = s.hub().takeover()
        && t.info.state == ViewState::Paused
        && live.registry.get(t.info.target_id).is_some()
        && reset_target(s, &t).await
    {
        s.hub().resolve_override_reset();
    }
}

/// How often the daemon itself retries work an interruption still owes.
pub(crate) const OWED_WORK_INTERVAL: Duration = Duration::from_secs(5);

/// Everything an interruption, a release or a failed latch write still owes
/// (key reset, `takeover.interrupted` / `takeover.released` audit rows, the
/// latch file) is retried by the daemon on its own, without a session: a
/// disconnected peer or a closed app must not leave it undone for good.
/// Cheap when nothing is owed; skipped when the executor is busy (the next
/// tick retries).
pub(crate) async fn retry_owed_work(s: &AppState) {
    let hub = s.hub();
    let (reset, audit) = hub.override_pending();
    if !reset && !audit && !hub.persist_dirty() && hub.release_audits_pending().is_empty() {
        return;
    }
    let live = s.live_view();
    let Ok(_executor) = tokio::time::timeout(BACKEND_CALL_TIMEOUT, live.executor.lock()).await
    else {
        return;
    };
    let _ = hub.retry_persist();
    retry_release_audits(s);
    retry_owed_reset(s).await;
    if hub.override_pending().1
        && let Some(t) = hub.takeover()
        && t.info.state == ViewState::Paused
        && let Some(reason) = t.info.reason.clone()
        && audit_interrupted(s, &Actor::local(s), &t, &reason).is_ok()
    {
        hub.resolve_override_audit();
    }
}

/// Best-effort retry of every owed release audit row (session heartbeats).
fn retry_release_audits(s: &AppState) {
    for journal in s.hub().release_audits_pending() {
        if write_released_audit(s, &journal).is_err() {
            break;
        }
    }
}

fn override_stands() -> ApiError {
    ApiError::forbidden(
        "the local user stopped this takeover; a release cannot clear it, only the local user can",
    )
}

/// A standing local override is never released by anyone but the local user.
fn ensure_not_overridden(s: &AppState) -> Result<(), ApiError> {
    if s.hub()
        .takeover()
        .is_some_and(|t| t.info.reason == Some(Reason::LocalOverride))
    {
        return Err(override_stands());
    }
    Ok(())
}

fn flush_note(s: &AppState, journal: &mut ReleaseJournal) -> Result<(), ApiError> {
    // The latch file is not trusted: only the fixed note format may become a
    // chat message the model reads as a system message.
    if !agent_core::control_note::is_well_formed(&journal.summary) {
        return Err(ApiError::unavailable("release note is not well formed"));
    }
    if journal.messages.is_empty() {
        let mut chats = std::collections::BTreeSet::new();
        for run in &journal.runs {
            if let Some(run) = s.storage().get_run(*run)?
                && let Some(chat) = run.chat_id
            {
                chats.insert(chat);
            }
        }
        journal.messages = chats.into_iter().map(|c| (Uuid::now_v7(), c)).collect();
        s.hub()
            .save_release_messages(journal.request_id, journal.messages.clone())
            .map_err(ApiError::unavailable)?;
    }
    for (id, chat) in &journal.messages {
        if s.storage().get_message(*id)?.is_none() {
            s.storage().insert_message(&storage::Message {
                id: *id,
                user_id: s.user_id(),
                chat_id: *chat,
                run_id: None,
                role: agent_core::control_note::ROLE.into(),
                content: journal.summary.clone(),
                created_at_ms: storage::now_ms(),
            })?;
        }
    }
    s.hub()
        .mark_release_noted(journal.request_id)
        .map_err(ApiError::unavailable)?;
    Ok(())
}

pub(crate) fn reject_query_credentials(req: &axum::extract::Request) -> bool {
    let path = req.uri().path();
    (path.starts_with("/v1/view/")
        || matches!(path, "/v1/control/takeover" | "/v1/control/release"))
        && req.uri().query().is_some_and(|q| {
            q.split('&').any(|kv| {
                let key = kv.split('=').next().unwrap_or_default();
                let decoded = percent_encoding::percent_decode_str(key).decode_utf8_lossy();
                matches!(decoded.as_ref(), "token" | "access_token" | "authorization")
            })
        })
}

impl AppState {
    /// Apply screen policy as an executor barrier. On return no revoked input can apply.
    /// # Errors
    /// Returns an error if revocation audit cannot be persisted; input stays paused.
    pub async fn set_screen_privileges(
        &self,
        actor_key: String,
        privileges: ScreenPrivileges,
    ) -> Result<(), ApiError> {
        let live = self.live_view();
        let executor = live.executor.lock().await;
        live.set_privileges(actor_key.clone(), privileges);
        let actor = Actor {
            key: actor_key.clone(),
            recovery: false,
            local: false,
        };
        if (!privileges.view || !privileges.input)
            && let Some(t) = self.hub().takeover().filter(|t| t.actor == actor_key)
        {
            interrupt(self, &actor, &t, Reason::Revoked).await?;
        }
        if !privileges.view {
            let ids: Vec<Uuid> = live
                .sessions
                .lock()
                .iter()
                .filter(|(_, s)| s.actor == actor_key)
                .map(|(id, _)| *id)
                .collect();
            drop(executor);
            for id in ids {
                end_session(self, &actor, id, Reason::Revoked).await?;
            }
        }
        Ok(())
    }

    /// Pairing reconciliation: a `peer:<key>` actor that is no longer paired
    /// loses its screen privileges, live sessions and takeover immediately.
    /// Call it periodically with the currently paired keys, or right after a
    /// revoke. Local identities are never touched.
    /// # Errors
    /// Returns an error if a revocation audit cannot be persisted.
    pub async fn reconcile_paired_peers(
        &self,
        paired: &std::collections::BTreeSet<String>,
    ) -> Result<(), ApiError> {
        let live = self.live_view();
        let mut stale: std::collections::BTreeSet<String> = {
            let sessions = live.sessions.lock();
            sessions
                .values()
                .map(|s| s.actor.clone())
                .chain(self.hub().takeover().map(|t| t.actor))
                .filter(|k| k.starts_with("peer:") && !paired.contains(k))
                .collect()
        };
        stale.extend(
            live.privileges
                .lock()
                .iter()
                .filter(|(k, p)| {
                    k.starts_with("peer:") && !paired.contains(*k) && (p.view || p.input)
                })
                .map(|(k, _)| k.clone()),
        );
        for key in stale {
            self.set_screen_privileges(
                key,
                ScreenPrivileges {
                    view: false,
                    input: false,
                },
            )
            .await?;
        }
        Ok(())
    }
}

fn sensitive_path(path: &str) -> bool {
    path.starts_with("/v1/view/") || matches!(path, "/v1/control/takeover" | "/v1/control/release")
}
/// A rejection audit contains only the authenticated actor and fixed operation/status.
pub(crate) async fn audit_ingress(
    State(s): State<AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let sensitive = sensitive_path(request.uri().path());
    let actor = request.extensions().get::<Actor>().cloned();
    let input = request.uri().path().ends_with("/input");
    let handler_audited = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let response = HANDLER_AUDITED
        .scope(Arc::clone(&handler_audited), next.run(request))
        .await;
    if sensitive
        && !handler_audited.load(std::sync::atomic::Ordering::SeqCst)
        && response.status().is_client_error()
        && let Some(actor) = actor
        && audit(
            &s,
            &actor,
            if input {
                "input.batch_rejected"
            } else {
                "view.denied"
            },
            serde_json::json!({"status":response.status().as_u16(),"reason":"request_rejected"}),
        )
        .is_err()
    {
        let live = s.live_view();
        let _executor = live.executor.lock().await;
        if let Some(t) = s.hub().takeover().filter(|t| t.actor == actor.key) {
            let _ = interrupt(&s, &actor, &t, Reason::AuditFailed).await;
        }
        return ApiError::unavailable("live-view audit unavailable").into_response();
    }
    response
}
