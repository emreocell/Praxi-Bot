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
}
impl LiveView {
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
    fn allowed(&self, actor: &Actor, input: bool) -> bool {
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
                t.frame_source().set_profile(p).await.map_err(backend)?;
            } else {
                t.frame_source().stop().await.map_err(backend)?;
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
#[allow(clippy::needless_pass_by_value)] // Fixed, ephemeral metadata values are built at each call site.
fn audit(
    s: &AppState,
    actor: &Actor,
    event: &str,
    meta: serde_json::Value,
) -> Result<(), ApiError> {
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
    t.frame_source().request_refresh(0).await.map_err(backend)?;
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
async fn interrupt(
    s: &AppState,
    actor: &Actor,
    t: &OwnedTakeover,
    reason: Reason,
) -> Result<(), ApiError> {
    // Mark input invalid before any fallible reset/audit operation.
    s.hub().interrupt(t.info.id, reason.clone());
    if let Some(target) = s.live_view().registry.get(t.info.target_id) {
        if let Some(sink) = target.input_sink() {
            if sink.release_pressed_inputs().await.is_err() {
                s.hub().interrupt(t.info.id, Reason::ResetFailed);
            }
            let _ = sink.set_takeover_active(false).await;
        }
        let _ = target
            .frame_source()
            .set_profile(EffectiveProfile::watch())
            .await;
    }
    audit(
        s,
        actor,
        "takeover.interrupted",
        serde_json::json!({"target_id":t.info.target_id,"takeover_id":t.info.id,"control_epoch":t.info.control_epoch,"reason":reason}),
    )
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
        if let Some(t) = live.registry.get(old.info.target_id) {
            if let Some(sink) = t.input_sink() {
                sink.release_pressed_inputs().await.map_err(backend)?;
                sink.set_takeover_active(false).await.map_err(backend)?;
            }
        } else {
            return Err(ApiError::unavailable("old target reset cannot be verified"));
        }
    }
    if i.recover || i.reacquire_takeover_id.is_some() {
        for mut journal in s.hub().pending_release_notes() {
            flush_note(&s, &mut journal)?;
        }
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
    )?;
    let owned = s
        .hub()
        .begin_takeover(&actor.key, &i, target, generation, actor.recovery)
        .map_err(ApiError::conflict)?;
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
            if live.session(&actor, owned.info.session_id).is_err() || !live.allowed(&actor, true) {
                let _ = interrupt(&state, &actor, &owned, Reason::Expired).await;
                return;
            }
            let activation = async {
                let target = live.target(owned.info.target_id,owned.generation)?;
                let sink = target.input_sink().ok_or_else(|| ApiError::unavailable("input unavailable"))?;
                sink.release_pressed_inputs().await.map_err(backend)?;
                sink.set_takeover_active(true).await.map_err(backend)?;
                audit(&state,&actor,"takeover.active",serde_json::json!({"takeover_id":owned.info.id,"control_epoch":owned.info.control_epoch,"target_id":owned.info.target_id}))?;
                state.hub().activate(owned.info.id,owned.info.control_epoch).map_err(ApiError::conflict)?;
                if let Some(session) = live.sessions.lock().get_mut(&owned.info.session_id) { session.profile = EffectiveProfile::live(); session.next_seq = 1; session.duplicates.clear(); }
                live.source_profile(owned.info.target_id).await?;
                target.frame_source().request_refresh(0).await.map_err(backend)
            }.await;
            if activation.is_err() {
                let _ = interrupt(&state, &actor, &owned, Reason::Unavailable).await;
            }
        });
    }
    Ok((StatusCode::ACCEPTED, Json(result)))
}

fn require_local(s: &AppState, actor: &Actor, what: &str) -> Result<(), ApiError> {
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
    let Some(old) = s.hub().force_interrupt(reason.clone()) else {
        return Ok(None);
    };
    let live = s.live_view();
    let _executor = live.executor.lock().await;
    let t = s.hub().takeover().unwrap_or(old.clone());
    interrupt(s, actor, &t, reason).await?;
    Ok(Some(old))
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
    override_takeover(&s, &actor, Reason::LocalOverride).await?;
    let live = s.live_view();
    let _executor = live.executor.lock().await;
    audit(
        &s,
        &actor,
        "takeover.recovered",
        serde_json::json!({"by":"local","reason":"local_recovery"}),
    )?;
    let old = s
        .hub()
        .clear_owned_latch()
        .map_err(ApiError::unavailable)?
        .ok_or_else(|| ApiError::conflict("no takeover is recorded"))?;
    for mut journal in s.hub().pending_release_notes() {
        flush_note(&s, &mut journal)?;
    }
    // Best effort: the old target may be gone.
    if let Some(sink) = live
        .registry
        .get(old.info.target_id)
        .and_then(|t| t.input_sink())
    {
        let _ = sink.release_pressed_inputs().await;
        let _ = sink.set_takeover_active(false).await;
    }
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
    let _executor = live.executor.lock().await;
    if !live.allowed(actor, true) {
        return Err(ApiError::forbidden("screen:input denied"));
    }
    if let Some(old) = s.hub().release_journal(i.request_id) {
        if old.actor != actor.key
            || old.takeover_id != i.takeover_id
            || old.epoch != i.control_epoch
            || old.human_action != i.human_action
        {
            return Err(ApiError::conflict("release request conflict"));
        }
        if old.completed {
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
    sink.release_pressed_inputs().await.map_err(backend)?;
    let (counts, kind) = {
        let sessions = live.sessions.lock();
        let session = sessions
            .get(&t.info.session_id)
            .ok_or_else(|| ApiError::not_found("view session"))?;
        (session.counts.clone(), target.info().kind)
    };
    let declared = match i.human_action {
        Some(HumanAction::LoginCompleted) => "user reports login completed",
        Some(HumanAction::ApplicationOpened) => "user reports application opened",
        _ => "user intervened",
    };
    let summary = format!(
        "Human intervention completed; {declared}. {} pointer events, {} wheel events, {} key events, {} text entries; target: {}. Discard cached references and obtain a fresh observation.",
        counts.pointer,
        counts.wheel,
        counts.key,
        counts.text,
        if kind == TargetKind::Browser {
            "browser"
        } else {
            "desktop"
        }
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
        })
        .map_err(ApiError::unavailable)?;
    flush_note(s, &mut journal)?;
    audit(
        s,
        actor,
        "takeover.released",
        serde_json::json!({"target_id":t.info.target_id,"takeover_id":t.info.id,"control_epoch":t.info.control_epoch,"request_id":i.request_id,"counts":counts}),
    )?;
    sink.set_takeover_active(false).await.map_err(backend)?;
    target
        .frame_source()
        .set_profile(EffectiveProfile::watch())
        .await
        .map_err(backend)?;
    s.hub()
        .finish_release(&actor.key, i.request_id)
        .map_err(ApiError::unavailable)?;
    if let Some(session) = live.sessions.lock().get_mut(&t.info.session_id) {
        session.profile = EffectiveProfile::watch();
    }
    Ok(())
}

fn flush_note(s: &AppState, journal: &mut ReleaseJournal) -> Result<(), ApiError> {
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
                role: "system".into(),
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
        let _executor = live.executor.lock().await;
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
            drop(_executor);
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
                .filter(|(k, p)| k.starts_with("peer:") && !paired.contains(*k) && (p.view || p.input))
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
    let response = next.run(request).await;
    if sensitive
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
