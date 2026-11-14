//! Bounded CDP screencast and viewport-mapped human input adapter for `live-view`.
//!
//! Implements [`live_view::ViewTarget`], [`live_view::FrameSource`], and
//! [`live_view::InputSink`] on top of [`Page`] and [`CdpClient`] using ONLY
//! `Page.startScreencast` / `Page.screencastFrame` / `Page.screencastFrameAck` /
//! `Page.stopScreencast` (JPEG only) and `Input.dispatchMouseEvent` /
//! `Input.dispatchKeyEvent` / `Input.insertText`.
//!
//! # Security and scope limitation
//!
//! Browser CDP input injection only targets the page CSS viewport and never
//! exposes arbitrary CDP forwarding or JS evaluation (`Runtime.evaluate`).
//! Native browser chrome (address bar, extension popups) and OS dialogs
//! (file pickers, print dialogs) cannot be controlled via CDP input and require
//! a desktop target ([`live_view::TargetKind::Desktop`]).

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use live_view::{
    Button, Capabilities, Cause, Codec, EffectiveProfile, Frame, FrameMetadata, FrameSource,
    FrameSubscription, InputEvent, InputSink, KeyPhase, Modifier, PointerPhase, Profile, Reason,
    SourceRect, TargetInfo, TargetKind, Units, ViewError, ViewTarget,
};
use serde_json::{Value, json};
use tokio::sync::{Mutex as AsyncMutex, Notify, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use uuid::Uuid;

use crate::client::{
    CdpClient, DecodedScreencastFrame, ScreencastFault, ScreencastMetadata, ScreencastRegistration,
    ViewStopHook,
};
use crate::error::Error;
use crate::page::Page;

/// Human-readable boundary note explaining what CDP browser input cannot reach.
pub const BROWSER_INPUT_LIMITATION: &str = "Browser CDP input injection only reaches the page CSS viewport; browser chrome and OS-level dialogs require a desktop target.";

const WATCH_EVERY_NTH_FRAME: u32 = 10;
const LIVE_EVERY_NTH_FRAME: u32 = 2;
const REFRESH_EVERY_NTH_FRAME: u32 = 1;
const WHEEL_DELTA_CLAMP: f64 = 1200.0;
const MAX_SOURCE_RECT_DIM: f64 = 4000.0;
const MAX_CONSECUTIVE_FRAME_FAULTS: u32 = 5;

const MOD_ALT: u32 = 1;
const MOD_CTRL: u32 = 2;
const MOD_META: u32 = 4;
const MOD_SHIFT: u32 = 8;

/// Deterministic manual clock usable in [`BrowserViewConfig`] for timing tests without
/// mutating global Tokio time (so real loopback TCP sockets stay responsive).
#[derive(Clone, Debug)]
pub struct ManualClock {
    tx: watch::Sender<Duration>,
}

impl Default for ManualClock {
    fn default() -> Self {
        Self::new()
    }
}

impl ManualClock {
    /// Construct a manual clock starting at virtual offset `0`.
    #[must_use]
    pub fn new() -> Self {
        let (tx, _) = watch::channel(Duration::ZERO);
        Self { tx }
    }

    /// Return the current virtual elapsed offset.
    #[must_use]
    pub fn elapsed(&self) -> Duration {
        *self.tx.borrow()
    }

    /// Advance the virtual clock by `delta`, waking all pending `sleep_until` futures whose
    /// deadline has been reached or passed.
    pub fn advance(&self, delta: Duration) {
        self.tx.send_modify(|cur| {
            *cur = cur.saturating_add(delta);
        });
    }
}

/// Clock source used for screencast scheduling, idle grace windows, heartbeats, and frame timestamps.
#[derive(Clone, Debug, Default)]
pub enum ViewClock {
    /// Real monotonic clock (`tokio::time::Instant`).
    #[default]
    System,
    /// Deterministic virtual clock controlled via [`ManualClock::advance`].
    Manual(ManualClock),
}

#[derive(Clone, Copy, Debug)]
enum ClockInstant {
    System(Instant),
    Manual(Duration),
}

impl ClockInstant {
    fn saturating_add(self, delta: Duration) -> Self {
        match self {
            Self::System(t) => Self::System(t + delta),
            Self::Manual(d) => Self::Manual(d.saturating_add(delta)),
        }
    }

    fn elapsed_since(self, now: Self) -> Duration {
        match (now, self) {
            (Self::System(n), Self::System(t)) => n.saturating_duration_since(t),
            (Self::Manual(n), Self::Manual(t)) => n.saturating_sub(t),
            _ => Duration::ZERO,
        }
    }

    fn is_earlier_than(self, other: Self) -> bool {
        match (self, other) {
            (Self::System(a), Self::System(b)) => a < b,
            (Self::Manual(a), Self::Manual(b)) => a < b,
            _ => false,
        }
    }
}

impl ViewClock {
    fn now(&self) -> ClockInstant {
        match self {
            Self::System => ClockInstant::System(Instant::now()),
            Self::Manual(m) => ClockInstant::Manual(m.elapsed()),
        }
    }

    async fn sleep_until(&self, deadline: ClockInstant) {
        match (self, deadline) {
            (Self::System, ClockInstant::System(t)) => {
                tokio::time::sleep_until(t).await;
            }
            (Self::Manual(m), ClockInstant::Manual(target)) => {
                let mut rx = m.tx.subscribe();
                let _ = rx.wait_for(|&now| now >= target).await;
            }
            _ => tokio::time::sleep(Duration::ZERO).await,
        }
    }
}

/// Optional host callback invoked whenever interactive takeover starts or stops.
pub type TakeoverPinHook = Arc<dyn Fn(bool) + Send + Sync>;

/// Optional listener called when a view detaches cleanly or fails.
type StateNotify = Arc<Notify>;

/// Configuration for [`BrowserViewTarget`].
#[derive(Clone)]
pub struct BrowserViewConfig {
    /// Stable identifier of this live-view target.
    pub target_id: Uuid,
    /// Host identifier reported in [`TargetInfo`].
    pub host_id: String,
    /// Owning bot identifier, if known.
    pub bot_id: Option<Uuid>,
    /// Owning run identifier, if known.
    pub run_id: Option<Uuid>,
    /// Initial generation reported before attaching (or when starting attached).
    pub initial_generation: u64,
    /// Bound wait for a fresh compositor paint after `request_refresh` (default 750 ms).
    pub new_paint_timeout: Duration,
    /// Grace delay before temporarily falling back to `everyNthFrame = 1` on cold/static pages.
    pub watch_idle_grace: Duration,
    /// Interval between short capture windows in `Watch` profile (default 3 s).
    pub watch_window_interval: Duration,
    /// Maximum silence between frames in `Live` profile before emitting a `repeated: true` frame.
    pub live_heartbeat_interval: Duration,
    /// Timeout when sending `Page.stopScreencast` during teardown.
    pub stop_timeout: Duration,
    /// Optional hook wired to driver takeover pinning (e.g. `CdpDriver::set_takeover_pin`).
    pub takeover_pin: Option<TakeoverPinHook>,
    /// Clock used for screencast scheduling windows and timestamps (defaults to real time).
    pub clock: ViewClock,
}

impl std::fmt::Debug for BrowserViewConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserViewConfig")
            .field("target_id", &self.target_id)
            .field("host_id", &self.host_id)
            .field("bot_id", &self.bot_id)
            .field("run_id", &self.run_id)
            .field("initial_generation", &self.initial_generation)
            .field("new_paint_timeout", &self.new_paint_timeout)
            .field("watch_idle_grace", &self.watch_idle_grace)
            .field("watch_window_interval", &self.watch_window_interval)
            .field("live_heartbeat_interval", &self.live_heartbeat_interval)
            .field("stop_timeout", &self.stop_timeout)
            .field("has_takeover_pin", &self.takeover_pin.is_some())
            .field("clock", &self.clock)
            .finish()
    }
}

impl Default for BrowserViewConfig {
    fn default() -> Self {
        Self {
            target_id: Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)),
            host_id: "local".into(),
            bot_id: None,
            run_id: None,
            initial_generation: 0,
            new_paint_timeout: Duration::from_millis(750),
            watch_idle_grace: Duration::from_millis(200),
            watch_window_interval: Duration::from_secs(3),
            live_heartbeat_interval: Duration::from_millis(1000),
            stop_timeout: Duration::from_millis(500),
            takeover_pin: None,
            clock: ViewClock::System,
        }
    }
}

/// Latest viewport coordinate geometry derived from CDP metadata or layout metrics.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewportGeometry {
    /// Top offset in DIPs.
    pub offset_top: f64,
    /// Active pinch-zoom / page scale factor.
    pub page_scale_factor: f64,
    /// Device viewport width in DIPs.
    pub device_width: f64,
    /// Device viewport height in DIPs.
    pub device_height: f64,
    /// Horizontal page scroll offset in CSS pixels.
    pub scroll_offset_x: f64,
    /// Vertical page scroll offset in CSS pixels.
    pub scroll_offset_y: f64,
    /// Visible CSS viewport width (`device_width / page_scale_factor`).
    pub css_width: f64,
    /// Visible CSS viewport height (`(device_height - offset_top).max(1.0) / page_scale_factor`).
    pub css_height: f64,
    /// Monotonic revision incremented on viewport resize, scale, or `offsetTop` changes.
    pub revision: u64,
}

impl ViewportGeometry {
    /// Build geometry from a screencast frame's metadata.
    pub fn from_metadata(meta: ScreencastMetadata, revision: u64) -> Self {
        let scale = meta.page_scale_factor.max(f64::MIN_POSITIVE);
        let avail_h = (meta.device_height - meta.offset_top).max(1.0);
        Self {
            offset_top: meta.offset_top,
            page_scale_factor: scale,
            device_width: meta.device_width,
            device_height: meta.device_height,
            scroll_offset_x: meta.scroll_offset_x,
            scroll_offset_y: meta.scroll_offset_y,
            css_width: (meta.device_width / scale).max(1.0),
            css_height: (avail_h / scale).max(1.0),
            revision,
        }
    }

    /// Map normalized `[0.0, 1.0]` frame coordinates to CDP viewport-relative CSS coordinates.
    ///
    /// Page scroll offset is intentionally NOT added because `Input.dispatchMouseEvent`
    /// already interprets coordinates relative to the visual viewport.
    pub fn normalized_to_css(self, norm_x: f64, norm_y: f64) -> Result<(f64, f64), ViewError> {
        if !norm_x.is_finite()
            || !norm_y.is_finite()
            || !(0.0..=1.0).contains(&norm_x)
            || !(0.0..=1.0).contains(&norm_y)
        {
            return Err(ViewError::InvalidRecord);
        }
        let x = (norm_x * self.css_width).clamp(0.0, self.css_width);
        let y = (norm_y * self.css_height).clamp(0.0, self.css_height);
        Ok((x, y))
    }

    fn source_rect(self) -> SourceRect {
        SourceRect {
            x: 0.0,
            y: (self.offset_top / self.page_scale_factor).max(0.0),
            width: self.css_width.round().clamp(1.0, MAX_SOURCE_RECT_DIM) as u32,
            height: self.css_height.round().clamp(1.0, MAX_SOURCE_RECT_DIM) as u32,
            units: Units::CssPx,
        }
    }

    fn key(self) -> (u64, u64, u64, u64) {
        (
            canonical_f64_bits(self.device_width),
            canonical_f64_bits(self.device_height),
            canonical_f64_bits(self.page_scale_factor),
            canonical_f64_bits(self.offset_top),
        )
    }
}

fn canonical_f64_bits(v: f64) -> u64 {
    if v == 0.0 {
        0.0_f64.to_bits()
    } else {
        v.to_bits()
    }
}

/// Non-sensitive runtime status and counters exposed for observability and testing.
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::struct_excessive_bools)]
pub struct BrowserViewStatus {
    /// Current target generation.
    pub generation: u64,
    /// Current geometry revision (`0` before any geometry is observed).
    pub geometry_revision: u64,
    /// Active profile while subscribed (`None` when stopped/unsubscribed).
    pub active_profile: Option<Profile>,
    /// True while a refresh request is waiting on paint or timed out before a newer paint arrived.
    pub refresh_pending: bool,
    /// True when a new-paint timeout elapsed without a compositor frame or repeated fault occurred.
    pub stale: bool,
    /// True while takeover is marked active via `InputSink::set_takeover_active`.
    pub takeover_active: bool,
    /// Count of screencast `Frame` publications sent onto the watch channel.
    pub frames_emitted: u64,
    /// Count of valid decoded frames superseded or gated before emission.
    pub frames_dropped: u64,
    /// Count of `Page.screencastFrameAck` calls completed.
    pub acks_sent: u64,
    /// Count of `Page.screencastFrameAck` calls that failed.
    pub acks_failed: u64,
    /// Count of `Page.startScreencast` commands sent.
    pub starts_sent: u64,
    /// Count of `Page.stopScreencast` commands sent.
    pub stops_sent: u64,
    /// True while a background screencast worker task is active.
    pub worker_running: bool,
    /// Non-secret failure reason if the view failed cleanly.
    pub last_error: Option<String>,
    /// Latest known viewport geometry.
    pub geometry: Option<ViewportGeometry>,
}

#[derive(Clone)]
struct SessionBinding {
    client: Arc<CdpClient>,
    session_id: String,
    target_closed: Arc<AtomicBool>,
    target_close_notify: StateNotify,
    target_close_rx: watch::Receiver<bool>,
    client_close_rx: watch::Receiver<bool>,
}

impl SessionBinding {
    fn is_alive(&self) -> bool {
        !self.client.is_closed() && !self.target_closed.load(Ordering::SeqCst)
    }

    async fn wait_closed(&mut self) {
        if !self.is_alive() {
            return;
        }
        let target_notify = self.target_close_notify.clone();
        tokio::select! {
            biased;
            _ = self.target_close_rx.wait_for(|&closed| closed) => {}
            _ = self.client_close_rx.wait_for(|&closed| closed) => {}
            () = target_notify.notified() => {}
        }
    }
}

fn prune_finished_worker_locked(state: &mut InnerState) -> Option<WorkerHandle> {
    if state
        .worker
        .as_ref()
        .is_some_and(|w| w.join.as_ref().is_some_and(JoinHandle::is_finished))
    {
        return state.worker.take();
    }
    None
}

fn reconcile_dead_binding_locked(
    state: &mut InnerState,
    detail: &str,
) -> (bool, Option<StateNotify>) {
    prune_finished_worker_locked(state);
    if let LifecycleState::Attached(binding) = &state.lifecycle
        && !binding.is_alive()
    {
        state.generation = state.generation.saturating_add(1);
        state.active_profile = None;
        state.refresh_pending = false;
        state.stale = false;
        state.last_error = Some(detail.to_string());
        state.lifecycle = LifecycleState::Unavailable {
            reason: Reason::Unavailable,
        };
        let notify = state.worker.as_ref().map(|w| {
            w.stop_requested.store(true, Ordering::SeqCst);
            w.wakeup.clone()
        });
        return (true, notify);
    }
    (false, None)
}

enum LifecycleState {
    Idle,
    Attached(SessionBinding),
    Unavailable { reason: Reason },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReattachPolicy {
    IdleSlot,
    SingleTab,
}

struct WorkerHandle {
    binding_session: String,
    stop_requested: Arc<AtomicBool>,
    wakeup: StateNotify,
    shared: Arc<WorkerShared>,
    join: Option<JoinHandle<()>>,
}

struct PendingProfileAck {
    profile: EffectiveProfile,
    reply: oneshot::Sender<Result<(), ViewError>>,
}

#[derive(Default)]
struct WorkerShared {
    pending_profile: StdMutex<Option<PendingProfileAck>>,
    pending_refresh: StdMutex<Option<u64>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PressedKey {
    code: String,
    key: String,
    location: u8,
}

impl PressedKey {
    fn is_modifier(&self) -> bool {
        matches!(
            self.key.as_str(),
            "Shift" | "Control" | "Alt" | "Meta" | "AltGraph"
        ) || matches!(
            self.code.as_str(),
            "ShiftLeft"
                | "ShiftRight"
                | "ControlLeft"
                | "ControlRight"
                | "AltLeft"
                | "AltRight"
                | "MetaLeft"
                | "MetaRight"
        )
    }
}

#[derive(Default)]
struct HeldInputs {
    buttons: u8,
    last_css: Option<(f64, f64)>,
    keys: Vec<PressedKey>,
}

struct InnerState {
    lifecycle: LifecycleState,
    reattach_policy: ReattachPolicy,
    generation: u64,
    geometry_revision: u64,
    geometry: Option<ViewportGeometry>,
    active_profile: Option<Profile>,
    refresh_pending: bool,
    stale: bool,
    takeover_active: bool,
    frames_emitted: u64,
    frames_dropped: u64,
    acks_sent: u64,
    acks_failed: u64,
    starts_sent: u64,
    stops_sent: u64,
    last_error: Option<String>,
    worker: Option<WorkerHandle>,
}

struct AdapterCore {
    config: BrowserViewConfig,
    anchor_wall: SystemTime,
    anchor_instant: ClockInstant,
    state: StdMutex<InnerState>,
    ctrl_lock: AsyncMutex<()>,
    input_io_lock: AsyncMutex<()>,
    held: StdMutex<HeldInputs>,
    watch_tx: watch::Sender<Option<Arc<Frame>>>,
    active_consumers: Arc<AtomicUsize>,
    action_seq_counter: AtomicU64,
    stop_hook_anchor: StdMutex<Option<ViewStopHook>>,
}

impl AdapterCore {
    fn now_epoch_ms(&self) -> u64 {
        let base_ms = self
            .anchor_wall
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        let now = self.config.clock.now();
        let elapsed_ms =
            u64::try_from(self.anchor_instant.elapsed_since(now).as_millis()).unwrap_or(0);
        base_ms.saturating_add(elapsed_ms)
    }

    fn set_takeover_pin(&self, active: bool) {
        let changed = {
            let Ok(mut state) = self.state.lock() else {
                return;
            };
            let prev = state.takeover_active;
            state.takeover_active = active;
            prev != active
        };
        if changed && let Some(hook) = &self.config.takeover_pin {
            hook(active);
        }
    }

    fn update_geometry(&self, meta: ScreencastMetadata) -> ViewportGeometry {
        let Ok(mut state) = self.state.lock() else {
            return ViewportGeometry::from_metadata(meta, 1);
        };
        let next_key = ViewportGeometry::from_metadata(meta, 0).key();
        let changed = match state.geometry {
            None => true,
            Some(prev) => prev.key() != next_key,
        };
        if changed || state.geometry_revision == 0 {
            state.geometry_revision = state.geometry_revision.saturating_add(1);
        }
        let geom = ViewportGeometry::from_metadata(meta, state.geometry_revision);
        state.geometry = Some(geom);
        geom
    }

    fn reconcile_dead_binding(&self, detail: &str) {
        let (transitioned, notify, finished) = {
            let Ok(mut state) = self.state.lock() else {
                return;
            };
            let finished = prune_finished_worker_locked(&mut state);
            let (transitioned, notify) = reconcile_dead_binding_locked(&mut state, detail);
            (transitioned, notify, finished)
        };
        drop(finished);
        if transitioned {
            self.watch_tx.send_replace(None);
            if let Some(notify) = notify {
                notify.notify_one();
            }
            self.set_takeover_pin(false);
        }
    }

    fn mark_unavailable(&self, reason: Reason, detail: String, bump_generation: bool) {
        self.mark_unavailable_inner(reason, detail, bump_generation, None);
    }

    fn mark_unavailable_inner(
        &self,
        reason: Reason,
        detail: String,
        bump_generation: bool,
        exiting_worker: Option<&Arc<WorkerShared>>,
    ) {
        self.watch_tx.send_replace(None);
        let (notify, cleared_worker, finished_worker) = {
            let Ok(mut state) = self.state.lock() else {
                return;
            };
            let finished_worker = prune_finished_worker_locked(&mut state);
            if bump_generation && !matches!(state.lifecycle, LifecycleState::Unavailable { .. }) {
                state.generation = state.generation.saturating_add(1);
            }
            state.active_profile = None;
            state.refresh_pending = false;
            state.stale = false;
            state.last_error = Some(detail);
            state.lifecycle = LifecycleState::Unavailable { reason };
            let cleared_worker = match exiting_worker {
                Some(shared)
                    if state
                        .worker
                        .as_ref()
                        .is_some_and(|w| Arc::ptr_eq(&w.shared, shared)) =>
                {
                    state.worker.take()
                }
                _ => None,
            };
            let notify = state.worker.as_ref().map(|w| {
                w.stop_requested.store(true, Ordering::SeqCst);
                w.wakeup.clone()
            });
            (notify, cleared_worker, finished_worker)
        };
        drop(finished_worker);
        drop(cleared_worker);
        if let Some(notify) = notify {
            notify.notify_one();
        }
        self.set_takeover_pin(false);
    }

    fn stop_hook(self: &Arc<Self>) -> ViewStopHook {
        let weak: Weak<Self> = Arc::downgrade(self);
        Arc::new(move || -> Pin<Box<dyn Future<Output = ()> + Send>> {
            let core = weak.upgrade();
            Box::pin(async move {
                if let Some(core) = core {
                    let _ = core.stop_all(false).await;
                }
            })
        })
    }

    fn binding_if_alive(&self) -> Option<SessionBinding> {
        let (binding, transitioned, notify) = {
            let mut state = self.state.lock().ok()?;
            let (transitioned, notify) = reconcile_dead_binding_locked(&mut state, "target closed");
            let binding = match &state.lifecycle {
                LifecycleState::Attached(binding) if binding.is_alive() => Some(binding.clone()),
                _ => None,
            };
            (binding, transitioned, notify)
        };
        if transitioned {
            self.watch_tx.send_replace(None);
            if let Some(notify) = notify {
                notify.notify_one();
            }
            self.set_takeover_pin(false);
        }
        binding
    }

    async fn stop_all(&self, return_to_idle: bool) -> Result<(), ViewError> {
        let _ctrl = self.ctrl_lock.lock().await;
        self.stop_worker_locked().await;
        if let Err(err) = self.release_inputs_inner().await
            && !return_to_idle
        {
            return Err(err);
        }
        self.set_takeover_pin(false);
        self.watch_tx.send_replace(None);
        if return_to_idle && let Ok(mut state) = self.state.lock() {
            state.generation = state.generation.saturating_add(1);
            state.geometry = None;
            state.geometry_revision = 0;
            state.active_profile = None;
            state.refresh_pending = false;
            state.stale = false;
            state.last_error = None;
            state.lifecycle = LifecycleState::Idle;
        }
        Ok(())
    }

    async fn stop_worker_locked(&self) {
        let worker = {
            let Ok(mut state) = self.state.lock() else {
                return;
            };
            state.active_profile = None;
            state.refresh_pending = false;
            state.worker.take()
        };
        self.watch_tx.send_replace(None);
        let Some(mut worker) = worker else {
            return;
        };
        worker.stop_requested.store(true, Ordering::SeqCst);
        worker.wakeup.notify_one();
        if let Some(mut join) = worker.join.take() {
            let wait_limit = self
                .config
                .stop_timeout
                .saturating_add(Duration::from_secs(1));
            if tokio::time::timeout(wait_limit, &mut join).await.is_err() {
                join.abort();
                let _ = join.await;
            }
        }
    }

    async fn release_inputs_inner(&self) -> Result<(), ViewError> {
        let _io = self.input_io_lock.lock().await;
        let Some(binding) = self.binding_if_alive() else {
            let mut held = self.held.lock().map_err(|_| ViewError::Unavailable)?;
            held.buttons = 0;
            held.keys.clear();
            return Ok(());
        };
        self.release_inputs_with_binding(&binding).await
    }

    async fn release_inputs_with_binding(&self, binding: &SessionBinding) -> Result<(), ViewError> {
        let snapshot = {
            let held = self.held.lock().map_err(|_| ViewError::Unavailable)?;
            (held.buttons, held.last_css, held.keys.clone())
        };
        let (mut buttons, last_css, keys) = snapshot;
        if buttons == 0 && keys.is_empty() {
            return Ok(());
        }

        let mut had_failure = false;
        let (rx, ry) = last_css.unwrap_or((0.0, 0.0));
        for (bit, name) in [(1_u8, "left"), (2_u8, "right"), (4_u8, "middle")] {
            if buttons & bit != 0 {
                let remaining = buttons & !bit;
                let res = send_cdp(
                    &binding.client,
                    &binding.session_id,
                    "Input.dispatchMouseEvent",
                    json!({
                        "type": "mouseReleased",
                        "x": rx,
                        "y": ry,
                        "button": name,
                        "buttons": remaining,
                        "clickCount": 1,
                        "modifiers": 0,
                    }),
                )
                .await;
                match res {
                    Ok(_) => {
                        buttons = remaining;
                        let mut held = self.held.lock().map_err(|_| ViewError::Unavailable)?;
                        held.buttons &= !bit;
                    }
                    Err(err) => {
                        if is_session_gone(&err, &binding.client) {
                            self.mark_unavailable(
                                Reason::Unavailable,
                                format!("input release target lost: {err}"),
                                true,
                            );
                            let mut held = self.held.lock().map_err(|_| ViewError::Unavailable)?;
                            held.buttons = 0;
                            held.keys.clear();
                            return Err(ViewError::Unavailable);
                        }
                        had_failure = true;
                    }
                }
            }
        }

        // Release non-modifier keys first, then modifier keys in reverse press order.
        let mut non_mods = Vec::new();
        let mut mods = Vec::new();
        for key in keys.into_iter().rev() {
            if key.is_modifier() {
                mods.push(key);
            } else {
                non_mods.push(key);
            }
        }
        let mut mod_mask = current_modifier_mask(&mods);
        for key in non_mods.into_iter().chain(mods) {
            let next_mod_mask = mod_mask & !key_modifier_bit(&key.key, &key.code);
            let res = dispatch_key_cdp(
                &binding.client,
                &binding.session_id,
                KeyPhase::Up,
                &key.code,
                &key.key,
                key.location,
                false,
                next_mod_mask,
            )
            .await;
            match res {
                Ok(()) => {
                    mod_mask = next_mod_mask;
                    let mut held = self.held.lock().map_err(|_| ViewError::Unavailable)?;
                    held.keys.retain(|k| k != &key);
                }
                Err(err) => {
                    if is_session_gone(&err, &binding.client) {
                        self.mark_unavailable(
                            Reason::Unavailable,
                            format!("input release target lost: {err}"),
                            true,
                        );
                        let mut held = self.held.lock().map_err(|_| ViewError::Unavailable)?;
                        held.buttons = 0;
                        held.keys.clear();
                        return Err(ViewError::Unavailable);
                    }
                    had_failure = true;
                }
            }
        }

        if had_failure {
            Err(ViewError::PartialInjection)
        } else {
            Ok(())
        }
    }
}

/// Live-view target adapter backed by Chromium CDP screencast and input events.
///
/// Can represent either a single [`Page`] (`BrowserViewTarget::from_page` or [`Page::live_view`])
/// or a browser driver slot that begins in [`Reason::BrowserIdle`] (`BrowserViewTarget::new_idle`)
/// without launching Chromium until an explicit [`BrowserViewTarget::attach_page`] call.
pub struct BrowserViewTarget {
    core: Arc<AdapterCore>,
    frame_source: Arc<BrowserFrameSource>,
    input_sink: Arc<BrowserInputSink>,
}

/// `FrameSource` implementation returned by [`BrowserViewTarget::frame_source`].
pub struct BrowserFrameSource {
    core: Arc<AdapterCore>,
}

/// `InputSink` implementation returned by [`BrowserViewTarget::input_sink`].
pub struct BrowserInputSink {
    core: Arc<AdapterCore>,
}

impl BrowserViewTarget {
    /// Construct a browser live-view target starting in [`Reason::BrowserIdle`].
    ///
    /// Watching or requesting frames while idle returns [`ViewError::Unavailable`] and never
    /// spawns a Chromium process.
    #[must_use]
    pub fn new_idle(config: BrowserViewConfig) -> Arc<Self> {
        let initial_gen = config.initial_generation;
        let anchor_instant = config.clock.now();
        let (watch_tx, _) = watch::channel(None);
        let core = Arc::new(AdapterCore {
            config,
            anchor_wall: SystemTime::now(),
            anchor_instant,
            state: StdMutex::new(InnerState {
                lifecycle: LifecycleState::Idle,
                reattach_policy: ReattachPolicy::IdleSlot,
                generation: initial_gen,
                geometry_revision: 0,
                geometry: None,
                active_profile: None,
                refresh_pending: false,
                stale: false,
                takeover_active: false,
                frames_emitted: 0,
                frames_dropped: 0,
                acks_sent: 0,
                acks_failed: 0,
                starts_sent: 0,
                stops_sent: 0,
                last_error: None,
                worker: None,
            }),
            ctrl_lock: AsyncMutex::new(()),
            input_io_lock: AsyncMutex::new(()),
            held: StdMutex::new(HeldInputs::default()),
            watch_tx,
            active_consumers: Arc::new(AtomicUsize::new(0)),
            action_seq_counter: AtomicU64::new(1),
            stop_hook_anchor: StdMutex::new(None),
        });
        let hook = core.stop_hook();
        if let Ok(mut anchor) = core.stop_hook_anchor.lock() {
            *anchor = Some(hook);
        }
        Arc::new(Self {
            frame_source: Arc::new(BrowserFrameSource { core: core.clone() }),
            input_sink: Arc::new(BrowserInputSink { core: core.clone() }),
            core,
        })
    }

    /// Construct a view target bound exclusively to `page` (closing/detaching ends the view permanently).
    pub fn from_page(page: &Page, mut config: BrowserViewConfig) -> Arc<Self> {
        if config.initial_generation == 0 {
            config.initial_generation = 1;
        }
        let target = Self::new_idle(config);
        {
            if let Ok(mut state) = target.core.state.lock() {
                state.reattach_policy = ReattachPolicy::SingleTab;
            }
        }
        let _ = target.bind_page_inner(page, false);
        target
    }

    /// Attach a running [`Page`] to an idle [`BrowserViewTarget`] slot, bumping `generation`.
    ///
    /// Fails with [`ViewError::Unavailable`] if this target is pinned to a closed/replaced tab or
    /// if `page` has already closed. Never silently rebinds a closed tab to another tab.
    pub fn attach_page(self: &Arc<Self>, page: &Page) -> Result<(), ViewError> {
        self.bind_page_inner(page, true)
    }

    fn bind_page_inner(&self, page: &Page, bump_gen: bool) -> Result<(), ViewError> {
        if !page.is_alive() {
            return Err(ViewError::Unavailable);
        }
        let (binding, stale_workers) = {
            let mut state = self.core.state.lock().map_err(|_| ViewError::Unavailable)?;
            let finished = prune_finished_worker_locked(&mut state);
            let _ = reconcile_dead_binding_locked(&mut state, "target closed");
            if state.reattach_policy == ReattachPolicy::SingleTab
                && matches!(state.lifecycle, LifecycleState::Unavailable { .. })
            {
                return Err(ViewError::Unavailable);
            }
            let stale_worker = if let Some(worker) = state.worker.as_ref() {
                if !worker.stop_requested.load(Ordering::SeqCst) {
                    return Err(ViewError::Unavailable);
                }
                state.worker.take()
            } else {
                None
            };
            if bump_gen {
                state.generation = state.generation.saturating_add(1);
            } else if state.generation == 0 {
                state.generation = 1;
            }
            let (target_closed, target_close_notify, target_close_rx) =
                page.live_view_close_signal();
            let client = page.client().clone();
            let client_close_rx = client.subscribe_closed();
            let binding = SessionBinding {
                client,
                session_id: page.session_id().to_string(),
                target_closed,
                target_close_notify,
                target_close_rx,
                client_close_rx,
            };
            state.lifecycle = LifecycleState::Attached(binding.clone());
            state.geometry = None;
            state.geometry_revision = 0;
            state.refresh_pending = false;
            state.stale = false;
            state.last_error = None;
            (binding, (finished, stale_worker))
        };
        drop(stale_workers);

        if let Ok(anchor) = self.core.stop_hook_anchor.lock()
            && let Some(hook) = anchor.as_ref()
        {
            binding.client.register_view_stop_hook(hook);
        }
        let weak_core = Arc::downgrade(&self.core);
        let hook: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            if let Some(core) = weak_core.upgrade() {
                let seq = core.action_seq_counter.fetch_add(1, Ordering::Relaxed);
                let _ = enqueue_refresh(&core, seq);
            }
        });
        page.register_live_view_action_hook(&hook);
        let weak_core_close = Arc::downgrade(&self.core);
        let close_hook: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            if let Some(core) = weak_core_close.upgrade() {
                core.reconcile_dead_binding("target closed");
            }
        });
        page.register_live_view_close_hook(&close_hook);
        Ok(())
    }

    /// Deliberately retire an idle-capable browser view slot back to [`Reason::BrowserIdle`],
    /// stopping active capture first and bumping `generation`.
    pub async fn detach_to_idle(&self) -> Result<(), ViewError> {
        {
            let state = self.core.state.lock().map_err(|_| ViewError::Unavailable)?;
            if state.reattach_policy == ReattachPolicy::SingleTab {
                return Err(ViewError::Unsupported);
            }
        }
        self.core.stop_all(true).await
    }

    /// Trigger an action-driven refresh with an explicit `action_seq` without blocking on paint.
    pub async fn request_refresh_action(&self, action_seq: u64) -> Result<(), ViewError> {
        self.frame_source.request_refresh(action_seq).await
    }

    /// Read a snapshot of diagnostic status counters (never contains pixel bytes or typed text).
    #[must_use]
    pub fn status(&self) -> BrowserViewStatus {
        let (status, transitioned, notify, finished) = {
            let Ok(mut state) = self.core.state.lock() else {
                return BrowserViewStatus {
                    generation: 0,
                    geometry_revision: 0,
                    active_profile: None,
                    refresh_pending: false,
                    stale: false,
                    takeover_active: false,
                    frames_emitted: 0,
                    frames_dropped: 0,
                    acks_sent: 0,
                    acks_failed: 0,
                    starts_sent: 0,
                    stops_sent: 0,
                    worker_running: false,
                    last_error: Some("view lock poisoned".into()),
                    geometry: None,
                };
            };
            let finished = prune_finished_worker_locked(&mut state);
            let (transitioned, notify) =
                reconcile_dead_binding_locked(&mut state, "target closed");
            let status = BrowserViewStatus {
                generation: state.generation,
                geometry_revision: state.geometry_revision,
                active_profile: state.active_profile.clone(),
                refresh_pending: state.refresh_pending,
                stale: state.stale,
                takeover_active: state.takeover_active,
                frames_emitted: state.frames_emitted,
                frames_dropped: state.frames_dropped,
                acks_sent: state.acks_sent,
                acks_failed: state.acks_failed,
                starts_sent: state.starts_sent,
                stops_sent: state.stops_sent,
                worker_running: state.worker.is_some(),
                last_error: state.last_error.clone(),
                geometry: state.geometry,
            };
            (status, transitioned, notify, finished)
        };
        drop(finished);
        if transitioned {
            self.core.watch_tx.send_replace(None);
            if let Some(notify) = notify {
                notify.notify_one();
            }
            self.core.set_takeover_pin(false);
        }
        status
    }

    /// Human-readable input limitation note (viewport-only; no browser chrome or OS dialogs).
    #[must_use]
    pub const fn input_limitation_note() -> &'static str {
        BROWSER_INPUT_LIMITATION
    }
}

#[async_trait]
impl ViewTarget for BrowserViewTarget {
    fn info(&self) -> TargetInfo {
        let (info, transitioned, notify, finished) = {
            let Ok(mut state) = self.core.state.lock() else {
                return TargetInfo {
                    target_id: self.core.config.target_id,
                    kind: TargetKind::Browser,
                    host_id: self.core.config.host_id.clone(),
                    bot_id: self.core.config.bot_id,
                    run_id: self.core.config.run_id,
                    generation: 0,
                    capabilities: Capabilities {
                        view: false,
                        input: false,
                        codecs: vec![Codec::Jpeg],
                        reason: Some(Reason::Unavailable),
                    },
                    unavailable_reason: Some(Reason::Unavailable),
                };
            };
            let finished = prune_finished_worker_locked(&mut state);
            let (transitioned, notify) =
                reconcile_dead_binding_locked(&mut state, "target closed");

            let (view, input, reason) = match &state.lifecycle {
                LifecycleState::Idle => (false, false, Some(Reason::BrowserIdle)),
                LifecycleState::Attached(binding) if binding.is_alive() => {
                    let reason = if state.stale {
                        Some(Reason::Stale)
                    } else if state.refresh_pending {
                        Some(Reason::RefreshPending)
                    } else {
                        None
                    };
                    (true, true, reason)
                }
                LifecycleState::Attached(_) | LifecycleState::Unavailable { .. } => {
                    let reason = match &state.lifecycle {
                        LifecycleState::Unavailable { reason } => reason.clone(),
                        _ => Reason::Unavailable,
                    };
                    (false, false, Some(reason))
                }
            };

            let info = TargetInfo {
                target_id: self.core.config.target_id,
                kind: TargetKind::Browser,
                host_id: self.core.config.host_id.clone(),
                bot_id: self.core.config.bot_id,
                run_id: self.core.config.run_id,
                generation: state.generation,
                capabilities: Capabilities {
                    view,
                    input,
                    codecs: vec![Codec::Jpeg],
                    reason: reason.clone(),
                },
                unavailable_reason: (!view).then_some(reason.unwrap_or(Reason::Unavailable)),
            };
            (info, transitioned, notify, finished)
        };
        drop(finished);
        if transitioned {
            self.core.watch_tx.send_replace(None);
            if let Some(notify) = notify {
                notify.notify_one();
            }
            self.core.set_takeover_pin(false);
        }
        info
    }

    fn frame_source(&self) -> Arc<dyn FrameSource> {
        self.frame_source.clone()
    }

    fn input_sink(&self) -> Option<Arc<dyn InputSink>> {
        let attached = self.core.binding_if_alive().is_some();
        attached.then(|| self.input_sink.clone() as Arc<dyn InputSink>)
    }
}

fn validate_profile(profile: &EffectiveProfile) -> Result<(), ViewError> {
    if profile.codec != Codec::Jpeg {
        return Err(ViewError::Unsupported);
    }
    if profile.max_width == 0 || profile.max_height == 0 || profile.quality == 0 {
        return Err(ViewError::InvalidRecord);
    }
    Ok(())
}

struct SubscribeCancelGuard {
    core: Arc<AdapterCore>,
    binding: Option<SessionBinding>,
    armed: bool,
}

impl Drop for SubscribeCancelGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.core.active_consumers.fetch_sub(1, Ordering::SeqCst);
        self.core.watch_tx.send_replace(None);
        if let Some(binding) = self.binding.take()
            && !binding.client.is_closed()
            && let Ok(handle) = tokio::runtime::Handle::try_current()
        {
            let timeout = self.core.config.stop_timeout;
            handle.spawn(async move {
                let _ = binding
                    .client
                    .send_with_timeout(
                        Some(&binding.session_id),
                        "Page.stopScreencast",
                        json!({}),
                        timeout,
                    )
                    .await;
            });
        }
    }
}

fn enqueue_refresh(core: &Arc<AdapterCore>, action_seq: u64) -> Result<(), ViewError> {
    let (wakeup, finished) = {
        let mut state = core.state.lock().map_err(|_| ViewError::Unavailable)?;
        let finished = prune_finished_worker_locked(&mut state);
        match &state.lifecycle {
            LifecycleState::Attached(b) if b.is_alive() => {}
            LifecycleState::Attached(_) => {
                drop(state);
                drop(finished);
                core.reconcile_dead_binding("target closed");
                return Err(ViewError::Unavailable);
            }
            _ => return Err(ViewError::Unavailable),
        }
        let wakeup = {
            let Some(worker) = &state.worker else {
                drop(state);
                drop(finished);
                return Ok(());
            };
            if let Ok(mut pending) = worker.shared.pending_refresh.lock() {
                *pending = Some(action_seq);
            }
            worker.wakeup.clone()
        };
        state.refresh_pending = true;
        (wakeup, finished)
    };
    drop(finished);
    wakeup.notify_one();
    Ok(())
}

#[async_trait]
impl FrameSource for BrowserFrameSource {
    async fn subscribe(&self, profile: EffectiveProfile) -> Result<FrameSubscription, ViewError> {
        validate_profile(&profile)?;
        let _ctrl = self.core.ctrl_lock.lock().await;

        let Some(binding) = self.core.binding_if_alive() else {
            return Err(ViewError::Unavailable);
        };

        // Check whether an active worker for this session is already running cleanly.
        let (existing_worker, finished) = {
            let mut state = self.core.state.lock().map_err(|_| ViewError::Unavailable)?;
            let finished = prune_finished_worker_locked(&mut state);
            let existing = state.worker.as_ref().and_then(|w| {
                (!w.stop_requested.load(Ordering::SeqCst)
                    && w.binding_session == binding.session_id)
                    .then(|| (w.shared.clone(), w.wakeup.clone()))
            });
            (existing, finished)
        };
        drop(finished);

        if let Some((shared, wakeup)) = existing_worker {
            let (tx, rx) = oneshot::channel();
            if let Ok(mut pending) = shared.pending_profile.lock() {
                *pending = Some(PendingProfileAck { profile, reply: tx });
            }
            wakeup.notify_one();
            let timeout = self
                .core
                .config
                .stop_timeout
                .saturating_add(Duration::from_secs(2));
            match tokio::time::timeout(timeout, rx).await {
                Ok(Ok(Ok(()))) => {}
                Ok(Ok(Err(e))) => return Err(e),
                _ => return Err(ViewError::Unavailable),
            }
            return Ok(make_subscription(&self.core));
        }

        // Clean up any stale worker from an earlier session before starting a fresh one.
        self.core.stop_worker_locked().await;
        self.core.watch_tx.send_replace(None);

        // Subscribe sink FIRST before sending `Page.startScreencast` so frame #1 is never lost.
        let reg = binding
            .client
            .register_screencast_sink(Some(&binding.session_id));

        self.core.active_consumers.fetch_add(1, Ordering::SeqCst);
        let mut cancel_guard = SubscribeCancelGuard {
            core: self.core.clone(),
            binding: Some(binding.clone()),
            armed: true,
        };

        let initial_nth = profile_every_nth(&profile.profile);
        if let Err(err) =
            start_screencast_cdp(&binding.client, &binding.session_id, &profile, initial_nth).await
        {
            cancel_guard.binding = None;
            drop(cancel_guard);
            if is_session_gone(&err, &binding.client) {
                self.core.mark_unavailable(
                    Reason::Unavailable,
                    format!("screencast start failed: {err}"),
                    true,
                );
            }
            return Err(ViewError::Unavailable);
        }

        {
            let mut state = self.core.state.lock().map_err(|_| ViewError::Unavailable)?;
            let _ = prune_finished_worker_locked(&mut state);
            if !matches!(&state.lifecycle, LifecycleState::Attached(b) if b.is_alive() && b.session_id == binding.session_id)
            {
                drop(state);
                cancel_guard.binding = None;
                return Err(ViewError::Unavailable);
            }
            state.starts_sent = state.starts_sent.saturating_add(1);
            state.active_profile = Some(profile.profile.clone());
            state.last_error = None;

            let stop_requested = Arc::new(AtomicBool::new(false));
            let wakeup = Arc::new(Notify::new());
            let shared = Arc::new(WorkerShared::default());
            let started_at = self.core.config.clock.now();

            let join = tokio::spawn(run_screencast_worker(
                self.core.clone(),
                binding.clone(),
                reg,
                profile,
                initial_nth,
                started_at,
                stop_requested.clone(),
                wakeup.clone(),
                shared.clone(),
            ));

            state.worker = Some(WorkerHandle {
                binding_session: binding.session_id,
                stop_requested,
                wakeup,
                shared,
                join: Some(join),
            });
        }

        // Disarm cancel guard and transfer consumer refcount to the returned `FrameSubscription`.
        cancel_guard.armed = false;
        let rx = self.core.watch_tx.subscribe();
        let core_on_drop = self.core.clone();
        Ok(FrameSubscription::new(rx, move || {
            on_consumer_drop(&core_on_drop);
        }))
    }

    async fn request_refresh(&self, action_seq: u64) -> Result<(), ViewError> {
        enqueue_refresh(&self.core, action_seq)
    }

    async fn set_profile(&self, profile: EffectiveProfile) -> Result<(), ViewError> {
        validate_profile(&profile)?;
        let Some(_binding) = self.core.binding_if_alive() else {
            return Err(ViewError::Unavailable);
        };
        let (worker, finished) = {
            let mut state = self.core.state.lock().map_err(|_| ViewError::Unavailable)?;
            let finished = prune_finished_worker_locked(&mut state);
            let worker = state
                .worker
                .as_ref()
                .filter(|w| !w.stop_requested.load(Ordering::SeqCst))
                .map(|w| (w.shared.clone(), w.wakeup.clone()));
            (worker, finished)
        };
        drop(finished);
        let Some((shared, wakeup)) = worker else {
            return Ok(());
        };
        let (tx, rx) = oneshot::channel();
        if let Ok(mut pending) = shared.pending_profile.lock() {
            *pending = Some(PendingProfileAck { profile, reply: tx });
        }
        wakeup.notify_one();
        let timeout = self
            .core
            .config
            .stop_timeout
            .saturating_add(Duration::from_secs(2));
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(res)) => res,
            _ => Err(ViewError::Unavailable),
        }
    }

    async fn stop(&self) -> Result<(), ViewError> {
        let _ctrl = self.core.ctrl_lock.lock().await;
        self.core.stop_worker_locked().await;
        self.core.set_takeover_pin(false);
        Ok(())
    }
}

fn make_subscription(core: &Arc<AdapterCore>) -> FrameSubscription {
    core.active_consumers.fetch_add(1, Ordering::SeqCst);
    let rx = core.watch_tx.subscribe();
    let weak_or_arc = core.clone();
    FrameSubscription::new(rx, move || {
        on_consumer_drop(&weak_or_arc);
    })
}

fn on_consumer_drop(core: &Arc<AdapterCore>) {
    let prev = core.active_consumers.fetch_sub(1, Ordering::SeqCst);
    if prev == 1 {
        // Clear retained frame immediately when the last consumer drops.
        core.watch_tx.send_replace(None);
        let notify = core.state.lock().ok().and_then(|state| {
            state.worker.as_ref().map(|w| {
                w.stop_requested.store(true, Ordering::SeqCst);
                w.wakeup.clone()
            })
        });
        if let Some(notify) = notify {
            notify.notify_one();
        }
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let core = core.clone();
            handle.spawn(async move {
                let _ctrl = core.ctrl_lock.lock().await;
                if core.active_consumers.load(Ordering::SeqCst) == 0 {
                    core.stop_worker_locked().await;
                    let _ = core.release_inputs_inner().await;
                    core.set_takeover_pin(false);
                    core.watch_tx.send_replace(None);
                }
            });
        }
    }
}

const fn profile_every_nth(profile: &Profile) -> u32 {
    match profile {
        Profile::Watch => WATCH_EVERY_NTH_FRAME,
        Profile::Live => LIVE_EVERY_NTH_FRAME,
    }
}

fn min_emit_interval(profile: &EffectiveProfile) -> Duration {
    let fps = if profile.max_fps == 0 {
        1
    } else {
        profile.max_fps
    };
    Duration::from_nanos(1_000_000_000_u64 / u64::from(fps))
}

async fn start_screencast_cdp(
    client: &CdpClient,
    session_id: &str,
    profile: &EffectiveProfile,
    every_nth_frame: u32,
) -> Result<(), Error> {
    client
        .send_on(
            Some(session_id),
            "Page.startScreencast",
            json!({
                "format": "jpeg",
                "quality": profile.quality,
                "maxWidth": profile.max_width,
                "maxHeight": profile.max_height,
                "everyNthFrame": every_nth_frame,
            }),
        )
        .await
        .map(|_| ())
}

async fn stop_screencast_cdp(
    client: &CdpClient,
    session_id: &str,
    timeout: Duration,
) -> Result<(), Error> {
    if client.is_closed() {
        return Ok(());
    }
    client
        .send_with_timeout(Some(session_id), "Page.stopScreencast", json!({}), timeout)
        .await
        .map(|_| ())
}

async fn ack_screencast_cdp(
    client: &CdpClient,
    session_id: &str,
    ack_session_id: i64,
    timeout: Duration,
) -> Result<(), Error> {
    client
        .send_with_timeout(
            Some(session_id),
            "Page.screencastFrameAck",
            json!({ "sessionId": ack_session_id }),
            timeout,
        )
        .await
        .map(|_| ())
}

async fn send_cdp(
    client: &CdpClient,
    session_id: &str,
    method: &str,
    params: Value,
) -> Result<Value, Error> {
    client.send_on(Some(session_id), method, params).await
}

fn is_session_gone(err: &Error, client: &CdpClient) -> bool {
    if client.is_closed() || err.is_fatal() {
        return true;
    }
    match err {
        Error::Protocol { code, message } => {
            *code == -32001
                || message.contains("Session with given id not found")
                || message.contains("Target closed")
                || message.contains("Inspected target navigated or closed")
        }
        _ => false,
    }
}

struct PendingRefreshState {
    action_seq: u64,
    deadline: ClockInstant,
}

#[derive(Clone, Copy)]
enum TimerReason {
    RefreshDeadline,
    WatchIdleGrace,
    WatchNextWindow,
    EmitGate,
    LiveHeartbeat,
}

struct WorkerState {
    core: Arc<AdapterCore>,
    binding: SessionBinding,
    profile: EffectiveProfile,
    current_nth: u32,
    screencast_running: bool,
    window_got_fresh: bool,
    window_idle_boosted: bool,
    window_opened_at: ClockInstant,
    next_watch_tick: Option<ClockInstant>,
    next_frame_seq: u64,
    cached_frame: Option<Arc<Frame>>,
    last_emit_at: Option<ClockInstant>,
    last_heartbeat_at: Option<ClockInstant>,
    pending_emit: Option<DecodedScreencastFrame>,
    pending_refresh: Option<PendingRefreshState>,
    consecutive_faults: u32,
}

impl WorkerState {
    fn earliest_timer(&self) -> Option<(ClockInstant, TimerReason)> {
        let mut best: Option<(ClockInstant, TimerReason)> = None;
        let mut consider = |when: ClockInstant, reason: TimerReason| {
            if best.is_none_or(|(b, _)| when.is_earlier_than(b)) {
                best = Some((when, reason));
            }
        };

        if let Some(req) = &self.pending_refresh {
            consider(req.deadline, TimerReason::RefreshDeadline);
        }

        if self.pending_emit.is_some() {
            let ready_at = match self.last_emit_at {
                Some(t) => t.saturating_add(min_emit_interval(&self.profile)),
                None => self.core.config.clock.now(),
            };
            consider(ready_at, TimerReason::EmitGate);
        }

        match self.profile.profile {
            Profile::Watch => {
                if self.screencast_running
                    && !self.window_got_fresh
                    && self.pending_refresh.is_none()
                {
                    consider(
                        self.window_opened_at
                            .saturating_add(self.core.config.watch_idle_grace),
                        TimerReason::WatchIdleGrace,
                    );
                } else if !self.screencast_running
                    && self.pending_refresh.is_none()
                    && let Some(tick) = self.next_watch_tick
                {
                    consider(tick, TimerReason::WatchNextWindow);
                }
            }
            Profile::Live => {
                if self.screencast_running
                    && !self.window_idle_boosted
                    && self.cached_frame.is_none()
                    && self.pending_refresh.is_none()
                {
                    consider(
                        self.window_opened_at
                            .saturating_add(self.core.config.watch_idle_grace),
                        TimerReason::WatchIdleGrace,
                    );
                } else if self.cached_frame.is_some() && self.pending_refresh.is_none() {
                    let base = self
                        .last_heartbeat_at
                        .or(self.last_emit_at)
                        .unwrap_or(self.window_opened_at);
                    consider(
                        base.saturating_add(self.core.config.live_heartbeat_interval),
                        TimerReason::LiveHeartbeat,
                    );
                }
            }
        }

        best
    }

    async fn ensure_screencast(&mut self, every_nth: u32) -> Result<(), String> {
        if self.screencast_running && self.current_nth == every_nth {
            return Ok(());
        }
        if self.screencast_running {
            self.stop_screencast().await?;
        }
        start_screencast_cdp(
            &self.binding.client,
            &self.binding.session_id,
            &self.profile,
            every_nth,
        )
        .await
        .map_err(|e| format!("Page.startScreencast failed: {e}"))?;
        if let Ok(mut state) = self.core.state.lock() {
            state.starts_sent = state.starts_sent.saturating_add(1);
        }
        self.screencast_running = true;
        self.current_nth = every_nth;
        Ok(())
    }

    async fn stop_screencast(&mut self) -> Result<(), String> {
        if !self.screencast_running {
            return Ok(());
        }
        self.screencast_running = false;
        let res = stop_screencast_cdp(
            &self.binding.client,
            &self.binding.session_id,
            self.core.config.stop_timeout,
        )
        .await;
        if let Ok(mut state) = self.core.state.lock() {
            state.stops_sent = state.stops_sent.saturating_add(1);
        }
        res.map_err(|e| format!("Page.stopScreencast failed: {e}"))
    }

    async fn ack_all(&mut self, mut ids: Vec<i64>) -> Result<(), String> {
        let ack_timeout = self.core.config.stop_timeout.max(Duration::from_secs(2));
        for ack_id in ids.drain(..) {
            match ack_screencast_cdp(
                &self.binding.client,
                &self.binding.session_id,
                ack_id,
                ack_timeout,
            )
            .await
            {
                Ok(()) => {
                    if let Ok(mut state) = self.core.state.lock() {
                        state.acks_sent = state.acks_sent.saturating_add(1);
                    }
                }
                Err(err) => {
                    if let Ok(mut state) = self.core.state.lock() {
                        state.acks_failed = state.acks_failed.saturating_add(1);
                    }
                    if is_session_gone(&err, &self.binding.client) {
                        return Err(format!("Page.screencastFrameAck session lost: {err}"));
                    }
                }
            }
        }
        Ok(())
    }

    fn publish_fresh(
        &mut self,
        decoded: DecodedScreencastFrame,
        cause: Cause,
        action_seq: Option<u64>,
    ) {
        let geom = self.core.update_geometry(decoded.metadata);
        let generation = self.core.state.lock().map_or(1, |s| s.generation.max(1));
        self.next_frame_seq = self.next_frame_seq.saturating_add(1);
        let frame = Arc::new(Frame {
            metadata: FrameMetadata {
                target_id: self.core.config.target_id,
                generation,
                frame_seq: self.next_frame_seq,
                captured_at_ms: self.core.now_epoch_ms(),
                cause,
                action_seq,
                codec: Codec::Jpeg,
                width: decoded.width,
                height: decoded.height,
                source_rect: geom.source_rect(),
                geometry_revision: geom.revision,
                repeated: false,
            },
            payload: decoded.jpeg,
        });
        if frame.validate().is_err() {
            if let Ok(mut state) = self.core.state.lock() {
                state.frames_dropped = state.frames_dropped.saturating_add(1);
            }
            return;
        }
        self.cached_frame = Some(frame.clone());
        let now = self.core.config.clock.now();
        self.last_emit_at = Some(now);
        self.last_heartbeat_at = Some(now);
        self.core.watch_tx.send_replace(Some(frame));
        if let Ok(mut state) = self.core.state.lock() {
            state.frames_emitted = state.frames_emitted.saturating_add(1);
            state.refresh_pending = false;
            state.stale = false;
            state.last_error = None;
        }
    }

    fn publish_repeated_idle(&mut self, mark_stale_timeout: bool) {
        let Some(prev) = &self.cached_frame else {
            if mark_stale_timeout && let Ok(mut state) = self.core.state.lock() {
                state.refresh_pending = true;
                state.stale = true;
            }
            return;
        };
        let mut meta = prev.metadata.clone();
        meta.repeated = true;
        meta.cause = Cause::Idle;
        meta.action_seq = None;
        let frame = Arc::new(Frame {
            metadata: meta,
            payload: prev.payload.clone(),
        });
        self.cached_frame = Some(frame.clone());
        let now = self.core.config.clock.now();
        self.last_emit_at = Some(now);
        self.last_heartbeat_at = Some(now);
        self.core.watch_tx.send_replace(Some(frame));
        if let Ok(mut state) = self.core.state.lock() {
            state.frames_emitted = state.frames_emitted.saturating_add(1);
            if mark_stale_timeout {
                state.refresh_pending = true;
                state.stale = true;
            }
        }
    }
}

struct WorkerExitGuard {
    core: Arc<AdapterCore>,
    shared: Arc<WorkerShared>,
}

impl Drop for WorkerExitGuard {
    fn drop(&mut self) {
        let cleared_worker = if let Ok(mut state) = self.core.state.lock()
            && state
                .worker
                .as_ref()
                .is_some_and(|w| Arc::ptr_eq(&w.shared, &self.shared))
        {
            state.active_profile = None;
            state.refresh_pending = false;
            state.worker.take()
        } else {
            None
        };
        drop(cleared_worker);
        if let Ok(mut pending) = self.shared.pending_profile.lock()
            && let Some(req) = pending.take()
        {
            let _ = req.reply.send(Err(ViewError::Unavailable));
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_screencast_worker(
    core: Arc<AdapterCore>,
    binding: SessionBinding,
    reg: ScreencastRegistration,
    initial_profile: EffectiveProfile,
    initial_nth: u32,
    started_at: ClockInstant,
    stop_requested: Arc<AtomicBool>,
    wakeup: StateNotify,
    shared: Arc<WorkerShared>,
) {
    let _exit_guard = WorkerExitGuard {
        core: core.clone(),
        shared: shared.clone(),
    };
    let mut ws = WorkerState {
        core,
        binding,
        profile: initial_profile,
        current_nth: initial_nth,
        screencast_running: true,
        window_got_fresh: false,
        window_idle_boosted: false,
        window_opened_at: started_at,
        next_watch_tick: None,
        next_frame_seq: 0,
        cached_frame: None,
        last_emit_at: None,
        last_heartbeat_at: Some(started_at),
        pending_emit: None,
        pending_refresh: None,
        consecutive_faults: 0,
    };

    let failure_reason: Option<String> = loop {
        if stop_requested.load(Ordering::SeqCst) || !ws.binding.is_alive() {
            if !ws.binding.is_alive() && !stop_requested.load(Ordering::SeqCst) {
                break Some("CDP page session closed or crashed".into());
            }
            break None;
        }

        // 1. Drain any incoming frames/dropped ACKs/faults first without holding locks across awaits.
        let take = reg.slot().take();
        if let Some(fault) = take.fault {
            if let Err(reason) = handle_slot_fault(&mut ws, &fault, &take.dropped_acks).await {
                break Some(reason);
            }
            continue;
        }
        if !take.dropped_acks.is_empty() || take.frame.is_some() {
            let mut acks = take.dropped_acks;
            let dropped_superseded = u64::try_from(acks.len()).unwrap_or(0);
            if dropped_superseded > 0
                && let Ok(mut state) = ws.core.state.lock()
            {
                state.frames_dropped = state.frames_dropped.saturating_add(dropped_superseded);
            }
            if let Some(frame) = take.frame {
                acks.push(frame.ack_session_id);
                if let Err(err) = ws.ack_all(acks).await {
                    break Some(err);
                }
                ws.consecutive_faults = 0;
                let was_fresh_window = ws.window_got_fresh;
                ws.window_got_fresh = true;
                ws.window_idle_boosted = false;

                if let Some(req) = ws.pending_refresh.take() {
                    let (cause, seq) = if req.action_seq > 0 {
                        (Cause::Action, Some(req.action_seq))
                    } else {
                        (Cause::Refresh, None)
                    };
                    ws.pending_emit = None;
                    ws.publish_fresh(frame, cause, seq);
                    if let Err(err) = finish_post_frame_capture(&mut ws).await {
                        break Some(err);
                    }
                } else if ws.profile.profile == Profile::Watch && was_fresh_window {
                    if let Ok(mut state) = ws.core.state.lock() {
                        state.frames_dropped = state.frames_dropped.saturating_add(1);
                    }
                } else {
                    let gate_now = ws.core.config.clock.now();
                    let gate_open = ws.last_emit_at.is_none_or(|t| {
                        t.elapsed_since(gate_now) >= min_emit_interval(&ws.profile)
                    });
                    if gate_open && ws.pending_emit.is_none() {
                        ws.publish_fresh(frame, Cause::Idle, None);
                    } else if ws.pending_emit.replace(frame).is_some()
                        && let Ok(mut state) = ws.core.state.lock()
                    {
                        state.frames_dropped = state.frames_dropped.saturating_add(1);
                    }
                    if let Err(err) = finish_post_frame_capture(&mut ws).await {
                        break Some(err);
                    }
                }
                continue;
            }
            if let Err(err) = ws.ack_all(acks).await {
                break Some(err);
            }
        }

        // 2. Apply pending profile change or refresh request cleanly.
        let pending_prof = shared
            .pending_profile
            .lock()
            .ok()
            .and_then(|mut g| g.take());
        if let Some(req) = pending_prof {
            let res = apply_profile_change(&mut ws, req.profile).await;
            let fatal_msg = res.as_ref().err().cloned();
            let _ = req.reply.send(if res.is_ok() {
                Ok(())
            } else {
                Err(ViewError::Unavailable)
            });
            if let Some(msg) = fatal_msg {
                break Some(msg);
            }
            continue;
        }

        let pending_req = shared
            .pending_refresh
            .lock()
            .ok()
            .and_then(|mut g| g.take());
        if let Some(action_seq) = pending_req {
            ws.next_watch_tick = None;
            ws.window_got_fresh = false;
            ws.pending_refresh = Some(PendingRefreshState {
                action_seq,
                deadline: ws
                    .core
                    .config
                    .clock
                    .now()
                    .saturating_add(ws.core.config.new_paint_timeout),
            });
            if let Err(err) = ws.ensure_screencast(REFRESH_EVERY_NTH_FRAME).await {
                break Some(err);
            }
            continue;
        }

        // Re-check closure immediately before parking so concurrent teardown signals are never lost.
        if stop_requested.load(Ordering::SeqCst) || !ws.binding.is_alive() {
            if !ws.binding.is_alive() && !stop_requested.load(Ordering::SeqCst) {
                break Some("CDP page session closed or crashed".into());
            }
            break None;
        }

        // 3. Wait for next event: slot frame, wakeup, target closed, or earliest timer.
        let timer = ws.earliest_timer();
        let clock = ws.core.config.clock.clone();
        match timer {
            Some((deadline, reason)) => {
                tokio::select! {
                    biased;
                    () = stop_wait(&stop_requested, &wakeup) => {},
                    () = ws.binding.wait_closed() => {},
                    () = reg.slot().notified() => {},
                    () = clock.sleep_until(deadline) => {
                        if let Err(err) = handle_timer(&mut ws, reason).await {
                            break Some(err);
                        }
                    }
                }
            }
            None => {
                tokio::select! {
                    biased;
                    () = stop_wait(&stop_requested, &wakeup) => {},
                    () = ws.binding.wait_closed() => {},
                    () = reg.slot().notified() => {},
                }
            }
        }
    };

    // Teardown: stop CDP capture cleanly while keeping WS bounded.
    let _ = ws.stop_screencast().await;
    drop(reg);

    // Reply cleanly to any pending profile waiter so no caller hangs.
    if let Ok(mut pending) = shared.pending_profile.lock()
        && let Some(req) = pending.take()
    {
        let _ = req.reply.send(Err(ViewError::Unavailable));
    }

    if let Some(reason) = failure_reason {
        ws.core
            .mark_unavailable_inner(Reason::Unavailable, reason, true, Some(&shared));
    } else {
        ws.core.watch_tx.send_replace(None);
        let cleared_worker = if let Ok(mut state) = ws.core.state.lock() {
            state.active_profile = None;
            state.refresh_pending = false;
            if state
                .worker
                .as_ref()
                .is_some_and(|w| Arc::ptr_eq(&w.shared, &shared))
            {
                state.worker.take()
            } else {
                None
            }
        } else {
            None
        };
        drop(cleared_worker);
    }
}

async fn stop_wait(stop_requested: &AtomicBool, wakeup: &Notify) {
    if stop_requested.load(Ordering::SeqCst) {
        return;
    }
    wakeup.notified().await;
}

async fn finish_post_frame_capture(ws: &mut WorkerState) -> Result<(), String> {
    match ws.profile.profile {
        Profile::Watch => {
            ws.stop_screencast().await?;
            ws.next_watch_tick = Some(
                ws.core
                    .config
                    .clock
                    .now()
                    .saturating_add(ws.core.config.watch_window_interval),
            );
        }
        Profile::Live => {
            if ws.current_nth != LIVE_EVERY_NTH_FRAME {
                ws.ensure_screencast(LIVE_EVERY_NTH_FRAME).await?;
            }
        }
    }
    Ok(())
}

async fn apply_profile_change(
    ws: &mut WorkerState,
    new_profile: EffectiveProfile,
) -> Result<(), String> {
    let profile_kind = new_profile.profile.clone();
    ws.profile = new_profile;
    ws.window_got_fresh = false;
    ws.window_idle_boosted = false;
    ws.window_opened_at = ws.core.config.clock.now();
    ws.next_watch_tick = None;
    if let Ok(mut state) = ws.core.state.lock() {
        state.active_profile = Some(profile_kind.clone());
    }
    let nth = if ws.pending_refresh.is_some() {
        REFRESH_EVERY_NTH_FRAME
    } else {
        profile_every_nth(&profile_kind)
    };
    ws.stop_screencast().await?; // Stop active stream before re-issuing updated parameters.
    ws.ensure_screencast(nth).await
}

async fn handle_slot_fault(
    ws: &mut WorkerState,
    fault: &ScreencastFault,
    dropped_acks: &[i64],
) -> Result<(), String> {
    if !dropped_acks.is_empty() {
        ws.ack_all(dropped_acks.to_vec()).await?;
    }
    ws.consecutive_faults = ws.consecutive_faults.saturating_add(1);
    let msg = match fault {
        ScreencastFault::EventTooLarge { size, limit } => {
            format!("screencast event exceeded byte budget ({size} > {limit})")
        }
        ScreencastFault::ImageTooLarge { size, limit } => {
            format!("screencast JPEG exceeded payload budget ({size} > {limit})")
        }
        ScreencastFault::MalformedFrame(detail) => format!("malformed screencast frame: {detail}"),
    };
    if let Ok(mut state) = ws.core.state.lock() {
        state.frames_dropped = state.frames_dropped.saturating_add(1);
        state.last_error = Some(msg.clone());
    }
    if ws.consecutive_faults >= MAX_CONSECUTIVE_FRAME_FAULTS {
        return Err(format!("{msg}; failing view explicitly"));
    }
    // Attempt bounded single reset of screencast; if session needs recreation, fail explicitly.
    let _ = ws.stop_screencast().await;
    let nth = if ws.pending_refresh.is_some() {
        REFRESH_EVERY_NTH_FRAME
    } else {
        profile_every_nth(&ws.profile.profile)
    };
    ws.ensure_screencast(nth)
        .await
        .map_err(|err| format!("{msg}; recovery failed ({err})"))
}

async fn handle_timer(ws: &mut WorkerState, reason: TimerReason) -> Result<(), String> {
    match reason {
        TimerReason::EmitGate => {
            if let Some(frame) = ws.pending_emit.take() {
                ws.publish_fresh(frame, Cause::Idle, None);
            }
        }
        TimerReason::RefreshDeadline => {
            ws.pending_refresh = None;
            ws.publish_repeated_idle(true);
            finish_post_frame_capture(ws).await?;
        }
        TimerReason::WatchIdleGrace => {
            if ws.profile.profile == Profile::Watch && !ws.window_idle_boosted {
                ws.window_idle_boosted = true;
                if ws.cached_frame.is_none() {
                    // Initial cold watch on a static page: temporarily use `everyNthFrame = 1`
                    // so Chrome emits frame #1 cleanly without waiting forever.
                    ws.ensure_screencast(REFRESH_EVERY_NTH_FRAME).await?;
                    ws.window_opened_at = ws.core.config.clock.now();
                } else {
                    // Static already-cached page in Watch: re-emit cached frame without waking encoder.
                    let now = ws.core.config.clock.now();
                    if ws
                        .last_emit_at
                        .is_none_or(|t| t.elapsed_since(now) >= min_emit_interval(&ws.profile))
                    {
                        ws.publish_repeated_idle(false);
                    }
                    ws.stop_screencast().await?;
                    ws.next_watch_tick = Some(
                        ws.core
                            .config
                            .clock
                            .now()
                            .saturating_add(ws.core.config.watch_window_interval),
                    );
                }
            } else if ws.profile.profile == Profile::Watch && ws.window_idle_boosted {
                // Even `everyNthFrame = 1` yielded no frame within grace; avoid spinning Chrome.
                if let Ok(mut state) = ws.core.state.lock() {
                    state.stale = true;
                }
                ws.stop_screencast().await?;
                ws.next_watch_tick = Some(
                    ws.core
                        .config
                        .clock
                        .now()
                        .saturating_add(ws.core.config.watch_window_interval),
                );
            } else if ws.profile.profile == Profile::Live && !ws.window_idle_boosted {
                ws.window_idle_boosted = true;
                if ws.cached_frame.is_none() {
                    ws.ensure_screencast(REFRESH_EVERY_NTH_FRAME).await?;
                }
            }
        }
        TimerReason::WatchNextWindow => {
            ws.next_watch_tick = None;
            ws.window_got_fresh = false;
            ws.window_idle_boosted = false;
            ws.window_opened_at = ws.core.config.clock.now();
            ws.ensure_screencast(WATCH_EVERY_NTH_FRAME).await?;
        }
        TimerReason::LiveHeartbeat => {
            ws.publish_repeated_idle(false);
        }
    }
    Ok(())
}

// -- InputSink ---------------------------------------------------------------------------------

impl BrowserInputSink {
    async fn resolve_geometry(
        &self,
        binding: &SessionBinding,
    ) -> Result<ViewportGeometry, ViewError> {
        if let Ok(state) = self.core.state.lock()
            && let Some(geom) = state.geometry
        {
            return Ok(geom);
        }
        // Cold-start fallback before frame #1 arrives: read-only `Page.getLayoutMetrics`
        // (never arbitrary JS evaluation).
        let res = send_cdp(
            &binding.client,
            &binding.session_id,
            "Page.getLayoutMetrics",
            json!({}),
        )
        .await
        .map_err(|_| ViewError::Unavailable)?;

        let vv = res
            .get("cssVisualViewport")
            .or_else(|| res.get("visualViewport"))
            .ok_or(ViewError::Unavailable)?;
        let scale = vv
            .get("scale")
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or(1.0);
        let client_w = vv
            .get("clientWidth")
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite() && *v > 0.0)
            .ok_or(ViewError::Unavailable)?;
        let client_h = vv
            .get("clientHeight")
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite() && *v > 0.0)
            .ok_or(ViewError::Unavailable)?;

        let meta = ScreencastMetadata {
            offset_top: 0.0,
            page_scale_factor: scale,
            device_width: client_w * scale,
            device_height: client_h * scale,
            scroll_offset_x: vv.get("pageX").and_then(Value::as_f64).unwrap_or(0.0),
            scroll_offset_y: vv.get("pageY").and_then(Value::as_f64).unwrap_or(0.0),
            timestamp: None,
        };
        Ok(self.core.update_geometry(meta))
    }

    async fn handle_inject_error(&self, binding: &SessionBinding, err: Error) -> ViewError {
        if is_session_gone(&err, &binding.client) {
            self.core.mark_unavailable(
                Reason::Unavailable,
                format!("input target lost: {err}"),
                true,
            );
            if let Ok(mut h) = self.core.held.lock() {
                h.buttons = 0;
                h.keys.clear();
            }
            ViewError::Unavailable
        } else {
            // Reset held state cleanly on transient input error.
            match self.core.release_inputs_with_binding(binding).await {
                Err(ViewError::Unavailable) => ViewError::Unavailable,
                _ => ViewError::PartialInjection,
            }
        }
    }
}

#[async_trait]
impl InputSink for BrowserInputSink {
    async fn inject(&self, event: &InputEvent) -> Result<(), ViewError> {
        if matches!(event, InputEvent::Reset) {
            return self.release_pressed_inputs().await;
        }

        let _io = self.core.input_io_lock.lock().await;
        let Some(binding) = self.core.binding_if_alive() else {
            return Err(ViewError::Unavailable);
        };

        match event {
            InputEvent::Pointer {
                phase,
                x,
                y,
                button,
                buttons,
                modifiers,
            } => {
                let geom = self.resolve_geometry(&binding).await?;
                let (cx, cy) = geom.normalized_to_css(*x, *y)?;
                let mod_mask = modifiers_to_cdp(modifiers);

                let (cdp_type, btn_name, click_count, btn_bit) = match phase {
                    PointerPhase::Move => (
                        "mouseMoved",
                        button.as_ref().map_or("none", button_to_cdp),
                        0_u32,
                        0_u8,
                    ),
                    PointerPhase::Down => {
                        let btn = button.as_ref().ok_or(ViewError::InvalidRecord)?;
                        ("mousePressed", button_to_cdp(btn), 1_u32, button_bit(btn))
                    }
                    PointerPhase::Up => {
                        let btn = button.as_ref().ok_or(ViewError::InvalidRecord)?;
                        ("mouseReleased", button_to_cdp(btn), 1_u32, button_bit(btn))
                    }
                };

                // Record press BEFORE awaiting so cancelling mid-flight is cleanly released later.
                let effective_buttons = {
                    let mut held = self.core.held.lock().map_err(|_| ViewError::Unavailable)?;
                    held.last_css = Some((cx, cy));
                    match phase {
                        PointerPhase::Down => {
                            held.buttons |= btn_bit;
                            held.buttons | *buttons
                        }
                        PointerPhase::Move => *buttons,
                        PointerPhase::Up => *buttons & !btn_bit,
                    }
                };

                let mut params = json!({
                    "type": cdp_type,
                    "x": cx,
                    "y": cy,
                    "button": btn_name,
                    "buttons": effective_buttons,
                    "modifiers": mod_mask,
                });
                if click_count > 0 {
                    params["clickCount"] = json!(click_count);
                }
                let res = send_cdp(
                    &binding.client,
                    &binding.session_id,
                    "Input.dispatchMouseEvent",
                    params,
                )
                .await;

                match res {
                    Ok(_) => {
                        if *phase == PointerPhase::Up {
                            let mut held = self
                                .core
                                .held
                                .lock()
                                .map_err(|_| ViewError::Unavailable)?;
                            held.buttons &= !btn_bit;
                        }
                        Ok(())
                    }
                    Err(err) => Err(self.handle_inject_error(&binding, err).await),
                }
            }
            InputEvent::Wheel {
                x,
                y,
                delta_x,
                delta_y,
                modifiers,
            } => {
                if !delta_x.is_finite() || !delta_y.is_finite() {
                    return Err(ViewError::InvalidRecord);
                }
                let geom = self.resolve_geometry(&binding).await?;
                let (cx, cy) = geom.normalized_to_css(*x, *y)?;
                let dx = delta_x.clamp(-WHEEL_DELTA_CLAMP, WHEEL_DELTA_CLAMP);
                let dy = delta_y.clamp(-WHEEL_DELTA_CLAMP, WHEEL_DELTA_CLAMP);
                let mod_mask = modifiers_to_cdp(modifiers);
                {
                    let mut held = self
                        .core
                        .held
                        .lock()
                        .map_err(|_| ViewError::Unavailable)?;
                    held.last_css = Some((cx, cy));
                }
                let res = send_cdp(
                    &binding.client,
                    &binding.session_id,
                    "Input.dispatchMouseEvent",
                    json!({
                        "type": "mouseWheel",
                        "x": cx,
                        "y": cy,
                        "deltaX": dx,
                        "deltaY": dy,
                        "modifiers": mod_mask,
                    }),
                )
                .await;
                match res {
                    Ok(_) => Ok(()),
                    Err(err) => Err(self.handle_inject_error(&binding, err).await),
                }
            }
            InputEvent::Key {
                phase,
                code,
                key,
                location,
                repeat,
                modifiers,
            } => {
                if code.is_empty() && key.is_empty() {
                    return Err(ViewError::InvalidRecord);
                }
                let pk = PressedKey {
                    code: code.clone(),
                    key: key.clone(),
                    location: *location,
                };
                if *phase == KeyPhase::Down {
                    let mut held = self
                        .core
                        .held
                        .lock()
                        .map_err(|_| ViewError::Unavailable)?;
                    if !held.keys.contains(&pk) {
                        held.keys.push(pk.clone());
                    }
                }

                let mod_mask = modifiers_to_cdp(modifiers);
                let res = dispatch_key_cdp(
                    &binding.client,
                    &binding.session_id,
                    phase.clone(),
                    code,
                    key,
                    *location,
                    *repeat,
                    mod_mask,
                )
                .await;

                match res {
                    Ok(()) => {
                        if *phase == KeyPhase::Up {
                            let mut held = self
                                .core
                                .held
                                .lock()
                                .map_err(|_| ViewError::Unavailable)?;
                            held.keys.retain(|x| x != &pk);
                        }
                        Ok(())
                    }
                    Err(err) => Err(self.handle_inject_error(&binding, err).await),
                }
            }
            InputEvent::Text { text } => {
                if text.is_empty() {
                    return Err(ViewError::InvalidRecord);
                }
                // `Input.insertText` inserts Unicode/IME text once without dispatching extra `keyDown` text.
                let res = send_cdp(
                    &binding.client,
                    &binding.session_id,
                    "Input.insertText",
                    json!({ "text": text }),
                )
                .await;
                match res {
                    Ok(_) => Ok(()),
                    Err(err) => Err(self.handle_inject_error(&binding, err).await),
                }
            }
            InputEvent::Reset => unreachable!("handled above"),
        }
    }

    async fn release_pressed_inputs(&self) -> Result<(), ViewError> {
        self.core.release_inputs_inner().await
    }

    async fn set_takeover_active(&self, active: bool) -> Result<(), ViewError> {
        if active && self.core.binding_if_alive().is_none() {
            return Err(ViewError::Unavailable);
        }
        self.core.set_takeover_pin(active);
        if !active {
            self.release_pressed_inputs().await?;
        }
        Ok(())
    }
}

const fn button_to_cdp(button: &Button) -> &'static str {
    match button {
        Button::Left => "left",
        Button::Middle => "middle",
        Button::Right => "right",
    }
}

const fn button_bit(button: &Button) -> u8 {
    match button {
        Button::Left => 1,
        Button::Right => 2,
        Button::Middle => 4,
    }
}

fn modifiers_to_cdp(modifiers: &[Modifier]) -> u32 {
    let mut mask = 0_u32;
    for m in modifiers {
        mask |= match m {
            Modifier::Alt => MOD_ALT,
            Modifier::Ctrl => MOD_CTRL,
            Modifier::Meta => MOD_META,
            Modifier::Shift => MOD_SHIFT,
        };
    }
    mask
}

fn key_modifier_bit(key: &str, code: &str) -> u32 {
    match (key, code) {
        ("Alt" | "AltGraph", _) | (_, "AltLeft" | "AltRight") => MOD_ALT,
        ("Control", _) | (_, "ControlLeft" | "ControlRight") => MOD_CTRL,
        ("Meta", _) | (_, "MetaLeft" | "MetaRight") => MOD_META,
        ("Shift", _) | (_, "ShiftLeft" | "ShiftRight") => MOD_SHIFT,
        _ => 0,
    }
}

fn current_modifier_mask(keys: &[PressedKey]) -> u32 {
    keys.iter()
        .fold(0_u32, |acc, k| acc | key_modifier_bit(&k.key, &k.code))
}

#[allow(clippy::too_many_arguments)]
async fn dispatch_key_cdp(
    client: &CdpClient,
    session_id: &str,
    phase: KeyPhase,
    code: &str,
    key: &str,
    location: u8,
    repeat: bool,
    modifiers: u32,
) -> Result<(), Error> {
    // Use `rawKeyDown` for general keys (so paired `InputEvent::Text` never double-types) and
    // `keyDown` + "\r" specifically for unmodified/Shift `Enter` so form submission and newlines work.
    let is_enter = code == "Enter" || code == "NumpadEnter" || key == "Enter";
    let (cdp_type, text): (&str, Option<&str>) = match phase {
        KeyPhase::Up => ("keyUp", None),
        KeyPhase::Down if is_enter && (modifiers & (MOD_CTRL | MOD_ALT | MOD_META) == 0) => {
            ("keyDown", Some("\r"))
        }
        KeyPhase::Down => ("rawKeyDown", None),
    };
    let vk = windows_vk_for_dom_key(code, key);
    let mut params = json!({
        "type": cdp_type,
        "key": key,
        "code": code,
        "location": location,
        "modifiers": modifiers,
        "windowsVirtualKeyCode": vk,
        "nativeVirtualKeyCode": vk,
        "autoRepeat": repeat,
        "isKeypad": location == 3,
    });
    if let Some(t) = text {
        params["text"] = json!(t);
        params["unmodifiedText"] = json!(t);
    }
    send_cdp(client, session_id, "Input.dispatchKeyEvent", params)
        .await
        .map(|_| ())
}

fn windows_vk_for_dom_key(code: &str, key: &str) -> u32 {
    if let Some(rest) = code.strip_prefix("Key")
        && let Some(b) = rest.as_bytes().first()
        && rest.len() == 1
        && b.is_ascii_uppercase()
    {
        return u32::from(*b);
    }
    if let Some(rest) = code.strip_prefix("Digit")
        && let Some(b) = rest.as_bytes().first()
        && rest.len() == 1
        && b.is_ascii_digit()
    {
        return u32::from(*b);
    }
    if let Some(rest) = code.strip_prefix('F')
        && let Ok(n) = rest.parse::<u32>()
        && (1..=24).contains(&n)
    {
        return 111 + n;
    }
    match code {
        "Backspace" => 8,
        "Tab" => 9,
        "Enter" | "NumpadEnter" => 13,
        "ShiftLeft" | "ShiftRight" => 16,
        "ControlLeft" | "ControlRight" => 17,
        "AltLeft" | "AltRight" => 18,
        "Pause" => 19,
        "CapsLock" => 20,
        "Escape" => 27,
        "Space" => 32,
        "PageUp" => 33,
        "PageDown" => 34,
        "End" => 35,
        "Home" => 36,
        "ArrowLeft" => 37,
        "ArrowUp" => 38,
        "ArrowRight" => 39,
        "ArrowDown" => 40,
        "Insert" => 45,
        "Delete" => 46,
        "MetaLeft" => 91,
        "MetaRight" => 92,
        "Numpad0" => 96,
        "Numpad1" => 97,
        "Numpad2" => 98,
        "Numpad3" => 99,
        "Numpad4" => 100,
        "Numpad5" => 101,
        "Numpad6" => 102,
        "Numpad7" => 103,
        "Numpad8" => 104,
        "Numpad9" => 105,
        "NumpadMultiply" => 106,
        "NumpadAdd" => 107,
        "NumpadSubtract" => 109,
        "NumpadDecimal" => 110,
        "NumpadDivide" => 111,
        "Semicolon" => 186,
        "Equal" => 187,
        "Comma" => 188,
        "Minus" => 189,
        "Period" => 190,
        "Slash" => 191,
        "Backquote" => 192,
        "BracketLeft" => 219,
        "Backslash" => 220,
        "BracketRight" => 221,
        "Quote" => 222,
        _ => match key {
            "Backspace" => 8,
            "Tab" => 9,
            "Enter" => 13,
            "Shift" => 16,
            "Control" => 17,
            "Alt" => 18,
            "Escape" => 27,
            " " => 32,
            "PageUp" => 33,
            "PageDown" => 34,
            "End" => 35,
            "Home" => 36,
            "ArrowLeft" => 37,
            "ArrowUp" => 38,
            "ArrowRight" => 39,
            "ArrowDown" => 40,
            "Delete" => 46,
            _ => {
                let mut chars = key.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) if c.is_ascii() => u32::from(c.to_ascii_uppercase()),
                    _ => 0,
                }
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn release_inputs_returns_unavailable_when_held_lock_poisoned() {
        let target = BrowserViewTarget::new_idle(BrowserViewConfig::default());
        let core = target.core.clone();
        let _ = std::thread::spawn(move || {
            let _guard = core.held.lock().expect("lock held");
            panic!("poison held mutex");
        })
        .join();
        assert!(target.core.held.is_poisoned());
        let idle_res = target.core.release_inputs_inner().await;
        assert!(
            matches!(idle_res, Err(ViewError::Unavailable)),
            "expected Unavailable on poisoned held lock when idle, got {idle_res:?}"
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let server_task = tokio::spawn(async move {
            if let Ok((stream, _)) = listener.accept().await {
                let _ws = tokio_tungstenite::accept_async(stream).await;
                std::future::pending::<()>().await;
            }
        });
        let client = CdpClient::connect(&format!("ws://127.0.0.1:{port}"))
            .await
            .expect("connect");
        let (_tx, target_close_rx) = watch::channel(false);
        let binding = SessionBinding {
            client_close_rx: client.subscribe_closed(),
            client,
            session_id: "sess-poison".into(),
            target_closed: Arc::new(AtomicBool::new(false)),
            target_close_notify: Arc::new(Notify::new()),
            target_close_rx,
        };
        let attached_res = target.core.release_inputs_with_binding(&binding).await;
        assert!(
            matches!(attached_res, Err(ViewError::Unavailable)),
            "expected Unavailable on poisoned held lock while attached, got {attached_res:?}"
        );
        server_task.abort();
    }
}
