//! Desktop control state machine (`BotDriving | HumanDriving | Paused | Idle`),
//! overlay coordinate conversion helpers, and internal channel for daemon/agent integration.
//!
//! # Coordinate Convention
//!
//! Matching `desktop-api` (`CoordinateSpace::Absolute` and `desktop_api::snapshot::Rect`),
//! all coordinates passed into [`ControlHandle::begin_action`], `overlay.show_target`,
//! and `overlay.click` are in **global physical screen pixels** across the virtual desktop.
//!
//! Each per-monitor overlay window converts global physical screen pixels into its own
//! monitor-local logical CSS pixels using [`physical_rect_to_monitor_logical`] and
//! [`physical_point_to_monitor_logical`]:
//!
//! ```text
//! local_logical_x = (global_physical_x - monitor_physical_x) / monitor_scale_factor
//! local_logical_y = (global_physical_y - monitor_physical_y) / monitor_scale_factor
//! logical_width   = physical_width / monitor_scale_factor
//! logical_height  = physical_height / monitor_scale_factor
//! ```

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};

/// Default pre-action highlight lead time (~300 ms before the action is dispatched).
pub const DEFAULT_PRE_ACTION_HIGHLIGHT_MS: u64 = 300;

/// Default global shortcut to stop all active bot runs.
pub const DEFAULT_STOP_HOTKEY: &str = "Ctrl+Alt+Esc";

/// Canonical event names (logical dot-separated names from the specification).
pub const EVENT_OVERLAY_SHOW_TARGET: &str = "overlay.show_target";
pub const EVENT_OVERLAY_CLICK: &str = "overlay.click";
pub const EVENT_OVERLAY_CLEAR: &str = "overlay.clear";
pub const EVENT_CONTROL_STATE_CHANGED: &str = "control.state_changed";

/// Tauri wire-compatible event names (Tauri v2 `EventName` allows alphanumeric, `-`, `/`, `:`, `_`).
pub const WIRE_EVENT_OVERLAY_SHOW_TARGET: &str = "overlay:show_target";
pub const WIRE_EVENT_OVERLAY_CLICK: &str = "overlay:click";
pub const WIRE_EVENT_OVERLAY_CLEAR: &str = "overlay:clear";
pub const WIRE_EVENT_OVERLAY_ENVELOPE: &str = "overlay:event";
pub const WIRE_EVENT_CONTROL_STATE: &str = "control:state_changed";

/// Single source of truth for who is currently driving the desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ControlMode {
    /// A bot run is actively controlling the desktop.
    BotDriving,
    /// The human took the wheel; bot desktop actions are refused until given back.
    HumanDriving,
    /// The bot run is paused; bot desktop actions are refused until resumed.
    Paused,
    /// No bot run is active.
    #[default]
    Idle,
}

/// Metadata identifying the active bot in the floating mini widget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BotRunMeta {
    pub bot_id: String,
    pub bot_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bot_shape: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bot_avatar: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bot_color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bot_initials: Option<String>,
}

/// A bounding box in global physical screen pixels (`desktop-api` convention).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct OverlayRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl OverlayRect {
    /// Returns the center point `(x, y)` of this rectangle in the same coordinate space.
    #[must_use]
    pub fn center(self) -> (f64, f64) {
        (self.x + self.width / 2.0, self.y + self.height / 2.0)
    }
}

/// Geometry of a physical display monitor for DPI and multi-monitor conversion.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorGeometry {
    pub monitor_index: usize,
    pub physical_x: f64,
    pub physical_y: f64,
    pub physical_width: f64,
    pub physical_height: f64,
    pub scale_factor: f64,
}

impl MonitorGeometry {
    /// Effective scale factor (falls back to `1.0` if non-positive or non-finite).
    #[must_use]
    pub fn effective_scale(self) -> f64 {
        if self.scale_factor.is_finite() && self.scale_factor > 0.0 {
            self.scale_factor
        } else {
            1.0
        }
    }
}

/// Converts a rectangle in global physical screen pixels to monitor-local logical CSS pixels.
#[must_use]
pub fn physical_rect_to_monitor_logical(
    rect: OverlayRect,
    monitor: MonitorGeometry,
) -> OverlayRect {
    let scale = monitor.effective_scale();
    OverlayRect {
        x: (rect.x - monitor.physical_x) / scale,
        y: (rect.y - monitor.physical_y) / scale,
        width: rect.width / scale,
        height: rect.height / scale,
    }
}

/// Converts a point in global physical screen pixels to monitor-local logical CSS pixels.
#[must_use]
pub fn physical_point_to_monitor_logical(x: f64, y: f64, monitor: MonitorGeometry) -> (f64, f64) {
    let scale = monitor.effective_scale();
    (
        (x - monitor.physical_x) / scale,
        (y - monitor.physical_y) / scale,
    )
}

/// Returns `true` if `rect` (in global physical screen pixels) intersects `monitor`.
#[must_use]
pub fn rect_intersects_monitor(rect: OverlayRect, monitor: MonitorGeometry) -> bool {
    let rect_right = rect.x + rect.width.max(1.0);
    let rect_bottom = rect.y + rect.height.max(1.0);
    let mon_right = monitor.physical_x + monitor.physical_width;
    let mon_bottom = monitor.physical_y + monitor.physical_height;

    rect.x < mon_right
        && rect_right > monitor.physical_x
        && rect.y < mon_bottom
        && rect_bottom > monitor.physical_y
}

/// Payload for `overlay.show_target {rect, label, action}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OverlayShowTargetPayload {
    pub rect: OverlayRect,
    pub label: String,
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// Payload for `overlay.click {x, y}` in global physical screen pixels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OverlayClickPayload {
    pub x: f64,
    pub y: f64,
    pub timestamp_ms: u64,
}

/// Envelope for overlay events carried over Tauri IPC.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", content = "payload")]
pub enum OverlayWireEvent {
    #[serde(rename = "overlay.show_target")]
    ShowTarget(OverlayShowTargetPayload),
    #[serde(rename = "overlay.click")]
    Click(OverlayClickPayload),
    #[serde(rename = "overlay.clear")]
    Clear,
}

/// Serializable snapshot of the control state machine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlSnapshot {
    pub mode: ControlMode,
    pub run_generation: u64,
    pub bot: Option<BotRunMeta>,
    pub current_step: Option<String>,
    pub stop_hotkey: String,
    pub active_target: Option<OverlayShowTargetPayload>,
    pub last_click: Option<OverlayClickPayload>,
    pub refused_actions_count: u64,
    /// Monotonic revision of the control state. Raised under the state lock by every mutating
    /// method, so a consumer can apply a snapshot only when its revision is higher than the last
    /// one it applied (events may be delivered out of order).
    #[serde(default)]
    pub state_rev: u64,
    /// Raised only by a user stop (see [`StopCause::User`]); never by a normal run finish,
    /// `set_stop_hotkey`, `set_current_step`, `clear` or a refused `begin_action`.
    #[serde(default)]
    pub stop_seq: u64,
    /// Token handed in with the user stop that produced the current `stop_seq`
    /// (`None` when that stop carried no token, e.g. hotkey, tray or widget).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_stop_token: Option<String>,
    /// Highest `stop_seq` a consumer has acknowledged through `ack_stop`.
    #[serde(default)]
    pub stop_acked_seq: u64,
}

/// Why the state machine went to [`ControlMode::Idle`] through [`ControlStateMachine::stop_with`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopCause {
    /// The user decided to stop (hotkey, tray, widget, banner, ...). Raises `stop_seq`.
    User { token: Option<String> },
    /// A run ended normally. Goes to Idle and raises `run_generation` but NOT `stop_seq`.
    Finished,
}

/// Errors returned by invalid control state transitions or hotkey configuration.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ControlError {
    #[error("cannot transition from {from:?} via {action}")]
    InvalidTransition {
        from: ControlMode,
        action: &'static str,
    },
    #[error("invalid global hotkey specification: {0}")]
    InvalidHotkey(String),
}

/// Pure state machine holding `BotDriving | HumanDriving | Paused | Idle` as the single source of truth.
#[derive(Debug, Clone)]
pub struct ControlStateMachine {
    mode: ControlMode,
    run_generation: u64,
    bot: Option<BotRunMeta>,
    current_step: Option<String>,
    stop_hotkey: String,
    /// Parsed form of `stop_hotkey`, cached so the hotkey listener never re-parses per tick.
    parsed_hotkey: ParsedHotkey,
    /// Raised whenever the configured hotkey is (re)set; lets the listener re-seed its edge detector.
    hotkey_gen: u64,
    active_target: Option<OverlayShowTargetPayload>,
    last_click: Option<OverlayClickPayload>,
    refused_actions_count: u64,
    state_rev: u64,
    stop_seq: u64,
    last_stop_token: Option<String>,
    stop_acked_seq: u64,
}

impl Default for ControlStateMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl ControlStateMachine {
    /// Creates a new state machine in [`ControlMode::Idle`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            mode: ControlMode::Idle,
            run_generation: 0,
            bot: None,
            current_step: None,
            stop_hotkey: DEFAULT_STOP_HOTKEY.to_string(),
            parsed_hotkey: ParsedHotkey {
                ctrl: true,
                alt: true,
                shift: false,
                meta: false,
                key: HotkeyKey::Escape,
            },
            hotkey_gen: 0,
            active_target: None,
            last_click: None,
            refused_actions_count: 0,
            state_rev: 0,
            stop_seq: 0,
            last_stop_token: None,
            stop_acked_seq: 0,
        }
    }

    /// Raises the state revision. Called by every mutating method before it snapshots.
    fn bump_rev(&mut self) {
        self.state_rev = self.state_rev.wrapping_add(1);
    }

    /// Current control mode (`BotDriving | HumanDriving | Paused | Idle`).
    #[must_use]
    pub fn mode(&self) -> ControlMode {
        self.mode
    }

    /// Current run generation counter.
    #[must_use]
    pub fn run_generation(&self) -> u64 {
        self.run_generation
    }

    /// Cheap (`Copy`, no allocation) view of the configured hotkey for the listener:
    /// `(generation, parsed)`. The generation changes whenever the hotkey is set.
    #[must_use]
    pub fn hotkey_view(&self) -> (u64, ParsedHotkey) {
        (self.hotkey_gen, self.parsed_hotkey)
    }

    /// Current state revision.
    #[must_use]
    pub fn state_rev(&self) -> u64 {
        self.state_rev
    }

    /// Current user-stop sequence.
    #[must_use]
    pub fn stop_seq(&self) -> u64 {
        self.stop_seq
    }

    /// Returns `true` when the human has taken the wheel (`HumanDriving`).
    #[must_use]
    pub fn is_human_driving(&self) -> bool {
        self.mode == ControlMode::HumanDriving
    }

    /// Returns `true` when the bot is actively driving (`BotDriving`).
    #[must_use]
    pub fn is_bot_driving(&self) -> bool {
        self.mode == ControlMode::BotDriving
    }

    /// Returns `true` when the run is paused (`Paused`).
    #[must_use]
    pub fn is_paused(&self) -> bool {
        self.mode == ControlMode::Paused
    }

    /// Returns `true` when no run is active (`Idle`).
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.mode == ControlMode::Idle
    }

    /// Returns a full snapshot of the current control state.
    #[must_use]
    pub fn snapshot(&self) -> ControlSnapshot {
        ControlSnapshot {
            mode: self.mode,
            run_generation: self.run_generation,
            bot: self.bot.clone(),
            current_step: self.current_step.clone(),
            stop_hotkey: self.stop_hotkey.clone(),
            active_target: self.active_target.clone(),
            last_click: self.last_click,
            refused_actions_count: self.refused_actions_count,
            state_rev: self.state_rev,
            stop_seq: self.stop_seq,
            last_stop_token: self.last_stop_token.clone(),
            stop_acked_seq: self.stop_acked_seq,
        }
    }

    /// Starts a bot run and transitions into [`ControlMode::BotDriving`].
    pub fn start_run(
        &mut self,
        bot: BotRunMeta,
        initial_step: impl Into<String>,
    ) -> ControlSnapshot {
        self.bump_rev();
        self.run_generation = self.run_generation.wrapping_add(1);
        self.mode = ControlMode::BotDriving;
        self.bot = Some(bot);
        self.current_step = Some(initial_step.into());
        self.active_target = None;
        self.last_click = None;
        self.refused_actions_count = 0;
        self.snapshot()
    }

    /// Pauses an active bot run (`BotDriving -> Paused`).
    ///
    /// # Errors
    ///
    /// Returns [`ControlError::InvalidTransition`] if the state machine is in `Idle` or `HumanDriving`.
    pub fn pause(&mut self) -> Result<ControlSnapshot, ControlError> {
        match self.mode {
            ControlMode::BotDriving | ControlMode::Paused => {
                self.bump_rev();
                self.mode = ControlMode::Paused;
                self.active_target = None;
                Ok(self.snapshot())
            }
            other => Err(ControlError::InvalidTransition {
                from: other,
                action: "pause",
            }),
        }
    }

    /// Resumes a paused bot run (`Paused -> BotDriving`).
    ///
    /// # Errors
    ///
    /// Returns [`ControlError::InvalidTransition`] if the state machine is not `Paused` or `BotDriving`.
    pub fn resume(&mut self) -> Result<ControlSnapshot, ControlError> {
        match self.mode {
            ControlMode::Paused | ControlMode::BotDriving => {
                self.bump_rev();
                self.mode = ControlMode::BotDriving;
                Ok(self.snapshot())
            }
            other => Err(ControlError::InvalidTransition {
                from: other,
                action: "resume",
            }),
        }
    }

    /// Transfers control to the human (`BotDriving | Paused -> HumanDriving`).
    ///
    /// While in [`ControlMode::HumanDriving`], the overlay displays `"You are in control"`
    /// and all bot desktop actions via [`Self::begin_action`] are refused.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError::InvalidTransition`] if no bot run is active (`Idle`).
    pub fn take_the_wheel(&mut self) -> Result<ControlSnapshot, ControlError> {
        match self.mode {
            ControlMode::BotDriving | ControlMode::Paused | ControlMode::HumanDriving => {
                self.bump_rev();
                self.mode = ControlMode::HumanDriving;
                self.active_target = None;
                self.last_click = None;
                Ok(self.snapshot())
            }
            ControlMode::Idle => Err(ControlError::InvalidTransition {
                from: ControlMode::Idle,
                action: "take_the_wheel",
            }),
        }
    }

    /// Hands control back from the human to the bot (`HumanDriving -> BotDriving`).
    ///
    /// # Errors
    ///
    /// Returns [`ControlError::InvalidTransition`] if the state is `Idle` or `Paused`.
    pub fn give_back(&mut self) -> Result<ControlSnapshot, ControlError> {
        match self.mode {
            ControlMode::HumanDriving | ControlMode::BotDriving => {
                self.bump_rev();
                self.mode = ControlMode::BotDriving;
                Ok(self.snapshot())
            }
            other => Err(ControlError::InvalidTransition {
                from: other,
                action: "give_back",
            }),
        }
    }

    /// User stop without a token: stops all bot activity immediately from any state,
    /// transitions to [`ControlMode::Idle`] and raises `stop_seq`.
    ///
    /// Every caller that does not explicitly mean "the run ended" (hotkey, tray, menu,
    /// widget, channel) goes through here, so an unlabeled stop is always a user stop.
    pub fn stop_all(&mut self) -> ControlSnapshot {
        self.stop_with(StopCause::User { token: None })
    }

    /// Goes to [`ControlMode::Idle`] from any state. Always raises `run_generation`
    /// (existing contract); raises `stop_seq` only for [`StopCause::User`].
    pub fn stop_with(&mut self, cause: StopCause) -> ControlSnapshot {
        self.bump_rev();
        self.run_generation = self.run_generation.wrapping_add(1);
        if let StopCause::User { token } = cause {
            self.stop_seq = self.stop_seq.wrapping_add(1);
            self.last_stop_token = token;
        }
        self.mode = ControlMode::Idle;
        self.bot = None;
        self.current_step = None;
        self.active_target = None;
        self.last_click = None;
        self.refused_actions_count = 0;
        self.snapshot()
    }

    /// Normal end of a run. A no-op (current snapshot, `false`) when
    /// `run_generation != expected_generation`, so a finished old run cannot idle a newer
    /// native run. Otherwise goes Idle without raising `stop_seq` and returns `true`.
    pub fn finish_run(&mut self, expected_generation: u64) -> (ControlSnapshot, bool) {
        if self.run_generation != expected_generation {
            return (self.snapshot(), false);
        }
        (self.stop_with(StopCause::Finished), true)
    }

    /// Acknowledges a stop sequence (monotonic `max`, clamped to the current `stop_seq` so a
    /// future sequence cannot be acknowledged). Silent: does not raise `state_rev`.
    pub fn ack_stop(&mut self, seq: u64) -> ControlSnapshot {
        self.stop_acked_seq = self.stop_acked_seq.max(seq.min(self.stop_seq));
        self.snapshot()
    }

    /// Updates the current human-readable step text shown in the floating mini widget.
    pub fn set_current_step(&mut self, step: impl Into<String>) -> ControlSnapshot {
        self.bump_rev();
        if self.mode != ControlMode::Idle {
            self.current_step = Some(step.into());
        }
        self.snapshot()
    }

    /// Validates and updates the global stop hotkey (default `Ctrl+Alt+Esc`).
    ///
    /// # Errors
    ///
    /// Returns [`ControlError::InvalidHotkey`] if the hotkey string cannot be parsed.
    pub fn set_stop_hotkey(
        &mut self,
        hotkey: impl Into<String>,
    ) -> Result<ControlSnapshot, ControlError> {
        let raw = hotkey.into();
        let parsed = parse_hotkey(&raw)?;
        self.bump_rev();
        self.hotkey_gen = self.hotkey_gen.wrapping_add(1);
        self.parsed_hotkey = parsed;
        self.stop_hotkey = parsed.canonical_string();
        Ok(self.snapshot())
    }

    /// Synchronous gate called when the bot is about to act on `rect` with `label`.
    ///
    /// Returns `true` (`proceed`) only if the state is [`ControlMode::BotDriving`].
    /// Refuses the action (`false`) when in [`ControlMode::HumanDriving`], [`ControlMode::Paused`],
    /// or [`ControlMode::Idle`].
    pub fn begin_action(&mut self, rect: OverlayRect, label: impl Into<String>) -> bool {
        self.begin_action_with_detail(rect, label, "click", None)
    }

    /// Synchronous gate with explicit action kind and optional typed-text preview.
    pub fn begin_action_with_detail(
        &mut self,
        rect: OverlayRect,
        label: impl Into<String>,
        action: impl Into<String>,
        text: Option<String>,
    ) -> bool {
        let label = label.into();
        let action = action.into();
        self.bump_rev();
        if self.mode != ControlMode::BotDriving {
            if self.mode == ControlMode::HumanDriving {
                self.refused_actions_count = self.refused_actions_count.saturating_add(1);
            }
            return false;
        }
        let payload = build_target_payload(rect, label.clone(), action, text);
        self.current_step = Some(label);
        self.active_target = Some(payload);
        true
    }

    /// Like [`Self::begin_action_with_detail`] for an action without an on-screen
    /// target (a key press, an app launch): only the step text changes.
    pub fn begin_action_untargeted(&mut self, label: impl Into<String>) -> bool {
        self.bump_rev();
        if self.mode != ControlMode::BotDriving {
            if self.mode == ControlMode::HumanDriving {
                self.refused_actions_count = self.refused_actions_count.saturating_add(1);
            }
            return false;
        }
        self.current_step = Some(label.into());
        self.active_target = None;
        true
    }

    /// Confirms after the ~300 ms pre-action highlight delay that the bot is still allowed to act
    /// and that the run generation has not changed.
    pub fn confirm_action_after_highlight(&mut self, expected_generation: u64) -> bool {
        // Only a refusal changes/emits state; a confirmed action leaves the state (and the
        // revision) untouched, so `state_rev` has no gaps between emitted snapshots.
        if self.run_generation != expected_generation {
            self.bump_rev();
            return false;
        }
        if self.mode != ControlMode::BotDriving {
            self.bump_rev();
            if self.mode == ControlMode::HumanDriving {
                self.refused_actions_count = self.refused_actions_count.saturating_add(1);
            }
            return false;
        }
        true
    }

    /// Records a cursor click ripple in global physical screen pixels.
    pub fn record_click(
        &mut self,
        x: f64,
        y: f64,
        timestamp_ms: u64,
    ) -> Option<OverlayClickPayload> {
        if self.mode != ControlMode::BotDriving {
            return None;
        }
        let click = OverlayClickPayload { x, y, timestamp_ms };
        self.bump_rev();
        self.last_click = Some(click);
        Some(click)
    }

    /// Clears any active target highlight and click ripple.
    pub fn clear_overlay(&mut self) -> ControlSnapshot {
        self.bump_rev();
        self.active_target = None;
        self.last_click = None;
        self.snapshot()
    }
}

/// Builds an [`OverlayShowTargetPayload`], extracting typed-text previews when `action` uses `"type: ..."`.
#[must_use]
pub fn build_target_payload(
    rect: OverlayRect,
    label: String,
    action: String,
    text: Option<String>,
) -> OverlayShowTargetPayload {
    if text.is_some() {
        return OverlayShowTargetPayload {
            rect,
            label,
            action,
            text,
        };
    }
    if let Some(rest) = action.strip_prefix("type:") {
        let trimmed = rest.trim();
        return OverlayShowTargetPayload {
            rect,
            label,
            action: "type".to_string(),
            text: if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            },
        };
    }
    OverlayShowTargetPayload {
        rect,
        label,
        action,
        text: None,
    }
}

/// Sink trait used by [`ControlHandle`] to broadcast overlay and control events to Tauri windows.
pub trait OverlayEventSink: Send + Sync {
    fn on_show_target(&self, payload: &OverlayShowTargetPayload);
    fn on_click(&self, payload: &OverlayClickPayload);
    fn on_clear(&self);
    fn on_state_changed(&self, snapshot: &ControlSnapshot);
}

/// No-op sink used in headless unit tests.
#[derive(Debug, Default)]
pub struct NoopEventSink;

impl OverlayEventSink for NoopEventSink {
    fn on_show_target(&self, _payload: &OverlayShowTargetPayload) {}
    fn on_click(&self, _payload: &OverlayClickPayload) {}
    fn on_clear(&self) {}
    fn on_state_changed(&self, _snapshot: &ControlSnapshot) {}
}

/// Messages accepted by the internal control channel for agent/daemon integration.
#[derive(Debug)]
pub enum ControlChannelMessage {
    /// Request permission to perform an action on `rect` with `label`.
    /// Shows the target highlight on the overlay ~300 ms before replying `proceed: bool`.
    BeginAction {
        rect: OverlayRect,
        label: String,
        action: String,
        text: Option<String>,
        respond_to: oneshot::Sender<bool>,
    },
    /// Query whether the human is currently driving (`HumanDriving`).
    IsHumanDriving { respond_to: oneshot::Sender<bool> },
    /// Emit `overlay.click {x, y}` at the given physical screen coordinates.
    Click { x: f64, y: f64 },
    /// Emit `overlay.clear`.
    Clear,
    /// Stop all active bot runs immediately.
    StopAll,
}

/// Thread-safe handle to the control state machine and its internal channel.
#[derive(Clone)]
pub struct ControlHandle {
    state: Arc<Mutex<ControlStateMachine>>,
    sink: Arc<RwLock<Arc<dyn OverlayEventSink>>>,
    tx: mpsc::UnboundedSender<ControlChannelMessage>,
    pre_action_delay: Duration,
}

impl ControlHandle {
    /// Creates a new [`ControlHandle`] with the specified pre-action highlight delay
    /// and spawns its internal channel worker on the Tokio runtime (if available).
    #[must_use]
    pub fn new(pre_action_delay: Duration) -> Self {
        let state = Arc::new(Mutex::new(ControlStateMachine::new()));
        let sink: Arc<RwLock<Arc<dyn OverlayEventSink>>> =
            Arc::new(RwLock::new(Arc::new(NoopEventSink)));
        let (tx, rx) = mpsc::unbounded_channel();

        let handle = Self {
            state,
            sink,
            tx,
            pre_action_delay,
        };

        handle.spawn_channel_worker(rx);
        handle
    }

    fn spawn_channel_worker(&self, mut rx: mpsc::UnboundedReceiver<ControlChannelMessage>) {
        let worker_handle = self.clone();
        let spawn_task = async move {
            while let Some(msg) = rx.recv().await {
                match msg {
                    ControlChannelMessage::BeginAction {
                        rect,
                        label,
                        action,
                        text,
                        respond_to,
                    } => {
                        let h = worker_handle.clone();
                        tokio::spawn(async move {
                            let proceed = h.execute_begin_action(rect, label, action, text).await;
                            let _ = respond_to.send(proceed);
                        });
                    }
                    ControlChannelMessage::IsHumanDriving { respond_to } => {
                        let _ = respond_to.send(worker_handle.is_human_driving());
                    }
                    ControlChannelMessage::Click { x, y } => {
                        worker_handle.click(x, y);
                    }
                    ControlChannelMessage::Clear => {
                        worker_handle.clear();
                    }
                    ControlChannelMessage::StopAll => {
                        worker_handle.stop_all();
                    }
                }
            }
        };

        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(spawn_task);
        } else {
            tauri::async_runtime::spawn(spawn_task);
        }
    }

    /// Attaches or replaces the [`OverlayEventSink`] used to notify Tauri windows.
    pub fn set_sink(&self, new_sink: Arc<dyn OverlayEventSink>) {
        *self.sink.write() = new_sink;
    }

    /// Returns a clone of the internal `mpsc::UnboundedSender<ControlChannelMessage>`
    /// so the agent/daemon integration can communicate over the channel.
    #[must_use]
    pub fn channel_sender(&self) -> mpsc::UnboundedSender<ControlChannelMessage> {
        self.tx.clone()
    }

    /// Returns `true` if the human currently holds the wheel (`ControlMode::HumanDriving`).
    #[must_use]
    pub fn is_human_driving(&self) -> bool {
        self.state.lock().is_human_driving()
    }

    /// Queries `is_human_driving()` over the internal channel.
    pub async fn is_human_driving_via_channel(&self) -> bool {
        let (tx, rx) = oneshot::channel();
        if self
            .tx
            .send(ControlChannelMessage::IsHumanDriving { respond_to: tx })
            .is_err()
        {
            return self.is_human_driving();
        }
        rx.await.unwrap_or_else(|_| self.is_human_driving())
    }

    /// Calls `begin_action(rect, label) -> proceed` via the internal channel.
    ///
    /// When in `BotDriving` mode, emits `overlay.show_target`, waits ~300 ms so the user can see
    /// the highlight box + label before the action executes, and then re-verifies that the human
    /// has not paused, stopped, or taken the wheel during the lead-in window.
    pub async fn begin_action(&self, rect: OverlayRect, label: impl Into<String>) -> bool {
        self.begin_action_with_detail(rect, label, "click", None)
            .await
    }

    /// Calls `begin_action` with an explicit action kind (`"click"`, `"type"`, etc.) and optional typed text.
    pub async fn begin_action_with_detail(
        &self,
        rect: OverlayRect,
        label: impl Into<String>,
        action: impl Into<String>,
        text: Option<String>,
    ) -> bool {
        let label = label.into();
        let action = action.into();
        let (tx, rx) = oneshot::channel();
        if self
            .tx
            .send(ControlChannelMessage::BeginAction {
                rect,
                label: label.clone(),
                action: action.clone(),
                text: text.clone(),
                respond_to: tx,
            })
            .is_err()
        {
            return self.execute_begin_action(rect, label, action, text).await;
        }
        rx.await.unwrap_or(false)
    }

    async fn execute_begin_action(
        &self,
        rect: OverlayRect,
        label: String,
        action: String,
        text: Option<String>,
    ) -> bool {
        let (initial_ok, generation, snapshot, target_payload) = {
            let mut guard = self.state.lock();
            let current_gen = guard.run_generation();
            let ok = guard.begin_action_with_detail(rect, label, action, text);
            let snap = guard.snapshot();
            let target = snap.active_target.clone();
            (ok, current_gen, snap, target)
        };

        let sink = self.sink.read().clone();
        sink.on_state_changed(&snapshot);

        if !initial_ok {
            return false;
        }

        if let Some(ref payload) = target_payload {
            sink.on_show_target(payload);
        }

        if !self.pre_action_delay.is_zero() {
            tokio::time::sleep(self.pre_action_delay).await;
        }

        let (confirmed, post_snapshot) = {
            let mut guard = self.state.lock();
            let ok = guard.confirm_action_after_highlight(generation);
            (ok, guard.snapshot())
        };

        if !confirmed {
            sink.on_state_changed(&post_snapshot);
        }

        confirmed
    }

    /// `begin_action` for an action with no target to highlight: shows the step
    /// in the widget, keeps the ~300 ms lead-in and re-checks the mode after it.
    pub async fn begin_action_untargeted(&self, label: impl Into<String>) -> bool {
        let (ok, generation, snapshot) = {
            let mut guard = self.state.lock();
            let generation = guard.run_generation();
            let ok = guard.begin_action_untargeted(label);
            (ok, generation, guard.snapshot())
        };
        let sink = self.sink.read().clone();
        sink.on_state_changed(&snapshot);
        if !ok {
            return false;
        }
        if !self.pre_action_delay.is_zero() {
            tokio::time::sleep(self.pre_action_delay).await;
        }
        let (confirmed, post) = {
            let mut guard = self.state.lock();
            let ok = guard.confirm_action_after_highlight(generation);
            (ok, guard.snapshot())
        };
        if !confirmed {
            sink.on_state_changed(&post);
        }
        confirmed
    }

    /// Returns a snapshot of the current control state.
    #[must_use]
    pub fn snapshot(&self) -> ControlSnapshot {
        self.state.lock().snapshot()
    }

    /// Starts a bot run (`Idle -> BotDriving`) and notifies listeners.
    pub fn start_run(&self, bot: BotRunMeta, initial_step: impl Into<String>) -> ControlSnapshot {
        let snapshot = self.state.lock().start_run(bot, initial_step);
        let sink = self.sink.read().clone();
        sink.on_clear();
        sink.on_state_changed(&snapshot);
        snapshot
    }

    /// Pauses the active bot run (`BotDriving -> Paused`).
    ///
    /// # Errors
    ///
    /// Returns [`ControlError`] if the state cannot transition to `Paused`.
    pub fn pause(&self) -> Result<ControlSnapshot, ControlError> {
        let snapshot = self.state.lock().pause()?;
        let sink = self.sink.read().clone();
        sink.on_clear();
        sink.on_state_changed(&snapshot);
        Ok(snapshot)
    }

    /// Resumes a paused bot run (`Paused -> BotDriving`).
    ///
    /// # Errors
    ///
    /// Returns [`ControlError`] if the state cannot transition to `BotDriving`.
    pub fn resume(&self) -> Result<ControlSnapshot, ControlError> {
        let snapshot = self.state.lock().resume()?;
        let sink = self.sink.read().clone();
        sink.on_state_changed(&snapshot);
        Ok(snapshot)
    }

    /// Transfers control to the human (`BotDriving | Paused -> HumanDriving`).
    ///
    /// # Errors
    ///
    /// Returns [`ControlError`] if no run is active.
    pub fn take_the_wheel(&self) -> Result<ControlSnapshot, ControlError> {
        let snapshot = self.state.lock().take_the_wheel()?;
        let sink = self.sink.read().clone();
        sink.on_clear();
        sink.on_state_changed(&snapshot);
        Ok(snapshot)
    }

    /// Returns control from the human to the bot (`HumanDriving -> BotDriving`).
    ///
    /// # Errors
    ///
    /// Returns [`ControlError`] if the human is not currently driving.
    pub fn give_back(&self) -> Result<ControlSnapshot, ControlError> {
        let snapshot = self.state.lock().give_back()?;
        let sink = self.sink.read().clone();
        sink.on_state_changed(&snapshot);
        Ok(snapshot)
    }

    /// Stops all active bot runs (`* -> Idle`) and clears the overlay.
    #[allow(clippy::must_use_candidate)]
    pub fn stop_all(&self) -> ControlSnapshot {
        self.stop_with(StopCause::User { token: None })
    }

    /// User stop carrying the caller's token (echoed back as `last_stop_token`).
    #[allow(clippy::must_use_candidate)]
    pub fn stop_user(&self, token: Option<String>) -> ControlSnapshot {
        self.stop_with(StopCause::User { token })
    }

    /// Normal end of a run: `* -> Idle` without raising `stop_seq`, and only when
    /// `run_generation == expected_generation` (otherwise nothing changes and nothing is emitted).
    #[allow(clippy::must_use_candidate)]
    pub fn finish_run(&self, expected_generation: u64) -> ControlSnapshot {
        let (snapshot, changed) = self.state.lock().finish_run(expected_generation);
        if changed {
            self.emit_stopped(&snapshot);
        }
        snapshot
    }

    /// Acknowledges `seq` (see [`ControlStateMachine::ack_stop`]). Emits no event.
    #[allow(clippy::must_use_candidate)]
    pub fn ack_stop(&self, seq: u64) -> ControlSnapshot {
        self.state.lock().ack_stop(seq)
    }

    fn stop_with(&self, cause: StopCause) -> ControlSnapshot {
        let snapshot = self.state.lock().stop_with(cause);
        self.emit_stopped(&snapshot);
        snapshot
    }

    fn emit_stopped(&self, snapshot: &ControlSnapshot) {
        let sink = self.sink.read().clone();
        sink.on_clear();
        sink.on_state_changed(snapshot);
    }

    /// Updates the current step text and broadcasts the updated snapshot.
    pub fn set_current_step(&self, step: impl Into<String>) -> ControlSnapshot {
        let snapshot = self.state.lock().set_current_step(step);
        let sink = self.sink.read().clone();
        sink.on_state_changed(&snapshot);
        snapshot
    }

    /// Configures the global stop hotkey (default `Ctrl+Alt+Esc`).
    ///
    /// # Errors
    ///
    /// Returns [`ControlError::InvalidHotkey`] if `hotkey` cannot be parsed.
    pub fn set_stop_hotkey(
        &self,
        hotkey: impl Into<String>,
    ) -> Result<ControlSnapshot, ControlError> {
        let snapshot = self.state.lock().set_stop_hotkey(hotkey)?;
        let sink = self.sink.read().clone();
        sink.on_state_changed(&snapshot);
        Ok(snapshot)
    }

    /// Directly emits `overlay.show_target {rect, label, action}`.
    pub fn show_target(
        &self,
        rect: OverlayRect,
        label: impl Into<String>,
        action: impl Into<String>,
        text: Option<String>,
    ) {
        let payload = build_target_payload(rect, label.into(), action.into(), text);
        let snapshot = {
            let mut guard = self.state.lock();
            guard.bump_rev();
            guard.active_target = Some(payload.clone());
            guard.snapshot()
        };
        let sink = self.sink.read().clone();
        sink.on_show_target(&payload);
        sink.on_state_changed(&snapshot);
    }

    /// Directly emits `overlay.click {x, y}` in global physical screen pixels.
    pub fn click(&self, x: f64, y: f64) {
        let now_ms = current_time_ms();
        let click_payload = {
            let mut guard = self.state.lock();
            if guard.mode() == ControlMode::HumanDriving || guard.mode() == ControlMode::Paused {
                None
            } else {
                let payload = OverlayClickPayload {
                    x,
                    y,
                    timestamp_ms: now_ms,
                };
                guard.bump_rev();
                guard.last_click = Some(payload);
                Some(payload)
            }
        };
        if let Some(payload) = click_payload {
            let sink = self.sink.read().clone();
            sink.on_click(&payload);
        }
    }

    /// Directly emits `overlay.clear`.
    pub fn clear(&self) {
        let snapshot = self.state.lock().clear_overlay();
        let sink = self.sink.read().clone();
        sink.on_clear();
        sink.on_state_changed(&snapshot);
    }
}

fn current_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

// ── Global Hotkey Parsing & OS Polling ────────────────────────────────────────

/// Supported primary keys for the configurable global Stop All hotkey.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyKey {
    Escape,
    Space,
    Backspace,
    Letter(char),
    Digit(char),
    Function(u8),
}

impl HotkeyKey {
    #[must_use]
    pub fn canonical_label(self) -> String {
        match self {
            Self::Escape => "Esc".to_string(),
            Self::Space => "Space".to_string(),
            Self::Backspace => "Backspace".to_string(),
            Self::Letter(c) => c.to_ascii_uppercase().to_string(),
            Self::Digit(d) => d.to_string(),
            Self::Function(n) => format!("F{n}"),
        }
    }
}

/// Parsed representation of a global hotkey combination (e.g. `Ctrl+Alt+Esc`).
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsedHotkey {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub meta: bool,
    pub key: HotkeyKey,
}

impl ParsedHotkey {
    /// Formats the hotkey in canonical `Ctrl+Alt+Shift+Key` form.
    #[must_use]
    pub fn canonical_string(self) -> String {
        let mut parts = Vec::with_capacity(5);
        if self.ctrl {
            parts.push("Ctrl".to_string());
        }
        if self.alt {
            parts.push("Alt".to_string());
        }
        if self.shift {
            parts.push("Shift".to_string());
        }
        if self.meta {
            parts.push("Super".to_string());
        }
        parts.push(self.key.canonical_label());
        parts.join("+")
    }
}

/// Parses a hotkey specification string such as `"Ctrl+Alt+Esc"` or `"Ctrl+Shift+S"`.
///
/// Requires at least one modifier (`Ctrl`, `Alt`, `Shift`, `Super`/`Win`/`Cmd`) and one trigger key.
///
/// # Errors
///
/// Returns [`ControlError::InvalidHotkey`] if the string is empty, lacks a modifier,
/// or specifies an unrecognized trigger key.
pub fn parse_hotkey(spec: &str) -> Result<ParsedHotkey, ControlError> {
    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
    let mut meta = false;
    let mut primary_key: Option<HotkeyKey> = None;

    for raw_token in spec.split('+').map(str::trim).filter(|t| !t.is_empty()) {
        let lower = raw_token.to_ascii_lowercase();
        match lower.as_str() {
            "ctrl" | "control" => ctrl = true,
            "alt" | "option" => alt = true,
            "shift" => shift = true,
            "super" | "meta" | "win" | "cmd" | "command" => meta = true,
            "esc" | "escape" => primary_key = Some(HotkeyKey::Escape),
            "space" => primary_key = Some(HotkeyKey::Space),
            "backspace" => primary_key = Some(HotkeyKey::Backspace),
            token if token.len() == 1 => {
                let ch = token.chars().next().unwrap_or(' ');
                if ch.is_ascii_alphabetic() {
                    primary_key = Some(HotkeyKey::Letter(ch.to_ascii_uppercase()));
                } else if ch.is_ascii_digit() {
                    primary_key = Some(HotkeyKey::Digit(ch));
                } else {
                    return Err(ControlError::InvalidHotkey(spec.to_string()));
                }
            }
            token if token.starts_with('f') => {
                let num: u8 = token[1..]
                    .parse()
                    .map_err(|_| ControlError::InvalidHotkey(spec.to_string()))?;
                if (1..=12).contains(&num) {
                    primary_key = Some(HotkeyKey::Function(num));
                } else {
                    return Err(ControlError::InvalidHotkey(spec.to_string()));
                }
            }
            _ => return Err(ControlError::InvalidHotkey(spec.to_string())),
        }
    }

    if !(ctrl || alt || shift || meta) {
        return Err(ControlError::InvalidHotkey(spec.to_string()));
    }

    let key = primary_key.ok_or_else(|| ControlError::InvalidHotkey(spec.to_string()))?;

    Ok(ParsedHotkey {
        ctrl,
        alt,
        shift,
        meta,
        key,
    })
}

/// Interval of the passive global hotkey poll (unchanged from the Idle-skipping version).
pub const HOTKEY_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Pure edge detector of the global stop hotkey: fires once per physical press (when the combo
/// goes from released to pressed). Holding the keys (OS auto-repeat) never fires again.
/// Takes no control mode on purpose: Idle is not special.
#[must_use]
pub fn hotkey_edge(was_pressed: bool, pressed_now: bool) -> (bool, bool) {
    (pressed_now && !was_pressed, pressed_now)
}

/// Edge-detector state of the listener, re-seeded whenever the configured hotkey changes so a
/// combination that is already physically held when it becomes the hotkey (e.g. the user just
/// recorded it in Settings) never synthesises a press edge.
#[derive(Debug, Default)]
pub struct HotkeyWatcher {
    seen_gen: Option<u64>,
    was_pressed: bool,
}

impl HotkeyWatcher {
    /// One listener tick. `view` is `(generation, parsed)` from [`ControlHandle::hotkey_view`]
    /// (`None` = no usable hotkey: the detector is reset). `is_pressed` reads the keyboard and is
    /// only called with the current parsed hotkey; no parsing happens here.
    pub fn tick(
        &mut self,
        control: &ControlHandle,
        view: Option<(u64, ParsedHotkey)>,
        is_pressed: impl FnOnce(ParsedHotkey) -> bool,
    ) {
        let Some((generation, parsed)) = view else {
            self.was_pressed = false;
            self.seen_gen = None;
            return;
        };
        let pressed_now = is_pressed(parsed);
        if self.seen_gen != Some(generation) {
            // Hotkey (re)configured: adopt the current key state as the baseline, no press.
            self.seen_gen = Some(generation);
            self.was_pressed = pressed_now;
            return;
        }
        self.was_pressed = poll_hotkey_once(control, self.was_pressed, pressed_now);
    }
}

/// One poll step of the hotkey listener: applies [`hotkey_edge`] and, on a press edge,
/// performs a user stop (`stop_seq` rises) from whatever mode the handle is in.
/// Returns the new `was_pressed`. Testable without Win32.
#[must_use]
pub fn poll_hotkey_once(control: &ControlHandle, was_pressed: bool, pressed_now: bool) -> bool {
    let (fire, now) = hotkey_edge(was_pressed, pressed_now);
    if fire {
        let _ = control.stop_all();
    }
    now
}

/// Spawns a background global hotkey listener on Windows so pressing the configured
/// Stop All hotkey (default `Ctrl+Alt+Esc`) cancels the active run even when another
/// application holds focus.
#[cfg(windows)]
#[allow(unsafe_code)]
pub fn spawn_global_hotkey_listener(control: ControlHandle) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, VK_BACK, VK_CONTROL, VK_ESCAPE, VK_F1, VK_LWIN, VK_MENU, VK_RWIN,
        VK_SHIFT, VK_SPACE,
    };

    fn is_vk_down(vk: i32) -> bool {
        // SAFETY: `GetAsyncKeyState` is a thread-safe Win32 query reading the current key state.
        let state = unsafe { GetAsyncKeyState(vk) };
        (state.cast_unsigned() & 0x8000) != 0
    }

    fn is_hotkey_pressed(hk: ParsedHotkey) -> bool {
        if hk.ctrl && !is_vk_down(i32::from(VK_CONTROL.0)) {
            return false;
        }
        if hk.alt && !is_vk_down(i32::from(VK_MENU.0)) {
            return false;
        }
        if hk.shift && !is_vk_down(i32::from(VK_SHIFT.0)) {
            return false;
        }
        if hk.meta && !(is_vk_down(i32::from(VK_LWIN.0)) || is_vk_down(i32::from(VK_RWIN.0))) {
            return false;
        }

        let key_vk: i32 = match hk.key {
            HotkeyKey::Escape => i32::from(VK_ESCAPE.0),
            HotkeyKey::Space => i32::from(VK_SPACE.0),
            HotkeyKey::Backspace => i32::from(VK_BACK.0),
            HotkeyKey::Letter(c) => i32::from(c.to_ascii_uppercase() as u8),
            HotkeyKey::Digit(d) => i32::from(d as u8),
            HotkeyKey::Function(n) => i32::from(VK_F1.0) + i32::from(n.saturating_sub(1)),
        };

        is_vk_down(key_vk)
    }

    std::thread::Builder::new()
        .name("bot-global-stop-hotkey".to_string())
        .spawn(move || {
            let mut watcher = HotkeyWatcher::default();
            loop {
                std::thread::sleep(HOTKEY_POLL_INTERVAL);
                // Polled in EVERY mode including Idle: a restored browser/remote run has no
                // native run, yet the global shortcut must still produce a Stop.
                let view = Some(control.state.lock().hotkey_view());
                watcher.tick(&control, view, is_hotkey_pressed);
            }
        })
        .ok();
}

#[cfg(not(windows))]
pub fn spawn_global_hotkey_listener(_control: ControlHandle) {}

// ── Unit Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_bot() -> BotRunMeta {
        BotRunMeta {
            bot_id: "atlas".to_string(),
            bot_name: "Atlas".to_string(),
            bot_shape: Some("cloud".to_string()),
            bot_avatar: None,
            bot_color: Some("#3b82f6".to_string()),
            bot_initials: Some("AT".to_string()),
        }
    }

    fn sample_rect() -> OverlayRect {
        OverlayRect {
            x: 200.0,
            y: 150.0,
            width: 180.0,
            height: 44.0,
        }
    }

    #[test]
    fn initial_state_is_idle_and_refuses_actions() {
        let mut sm = ControlStateMachine::new();
        assert_eq!(sm.mode(), ControlMode::Idle);
        assert!(sm.is_idle());
        assert!(!sm.is_human_driving());
        assert!(!sm.is_bot_driving());
        assert!(!sm.begin_action(sample_rect(), "Click search"));
    }

    #[test]
    fn start_run_transitions_to_bot_driving_and_allows_begin_action() {
        let mut sm = ControlStateMachine::new();
        let snap = sm.start_run(sample_bot(), "Opening browser");
        assert_eq!(snap.mode, ControlMode::BotDriving);
        assert_eq!(snap.run_generation, 1);
        assert!(sm.is_bot_driving());
        assert!(!sm.is_human_driving());

        let proceed = sm.begin_action(sample_rect(), "Click search bar");
        assert!(proceed);
        let active = sm
            .snapshot()
            .active_target
            .expect("target should be recorded");
        assert_eq!(active.label, "Click search bar");
        assert_eq!(active.rect, sample_rect());
    }

    #[test]
    fn pause_and_resume_transitions() {
        let mut sm = ControlStateMachine::new();
        sm.start_run(sample_bot(), "Step 1");

        let paused = sm.pause().expect("pause should succeed from BotDriving");
        assert_eq!(paused.mode, ControlMode::Paused);
        assert!(sm.is_paused());
        assert!(!sm.begin_action(sample_rect(), "Refused while paused"));

        let resumed = sm.resume().expect("resume should succeed from Paused");
        assert_eq!(resumed.mode, ControlMode::BotDriving);
        assert!(sm.begin_action(sample_rect(), "Allowed after resume"));
    }

    #[test]
    fn take_the_wheel_refuses_bot_actions_and_give_back_resumes() {
        let mut sm = ControlStateMachine::new();
        sm.start_run(sample_bot(), "Typing credentials");
        assert!(sm.begin_action(sample_rect(), "Focus password field"));

        let wheel = sm
            .take_the_wheel()
            .expect("take_the_wheel should succeed from BotDriving");
        assert_eq!(wheel.mode, ControlMode::HumanDriving);
        assert!(sm.is_human_driving());
        assert!(
            wheel.active_target.is_none(),
            "active target highlight must be cleared when human takes control"
        );

        // Bot actions must be refused while human drives
        assert!(!sm.begin_action(sample_rect(), "Click Sign In"));
        assert!(!sm.begin_action(sample_rect(), "Type 2FA code"));
        assert_eq!(sm.snapshot().refused_actions_count, 2);

        // Give back resumes BotDriving
        let back = sm
            .give_back()
            .expect("give_back should succeed from HumanDriving");
        assert_eq!(back.mode, ControlMode::BotDriving);
        assert!(!sm.is_human_driving());
        assert!(sm.begin_action(sample_rect(), "Click Continue after 2FA"));
    }

    #[test]
    fn take_the_wheel_from_paused_and_give_back() {
        let mut sm = ControlStateMachine::new();
        sm.start_run(sample_bot(), "Step 1");
        sm.pause().expect("pause ok");

        let wheel = sm
            .take_the_wheel()
            .expect("take_the_wheel should succeed from Paused");
        assert_eq!(wheel.mode, ControlMode::HumanDriving);
        assert!(sm.is_human_driving());

        let back = sm.give_back().expect("give_back ok");
        assert_eq!(back.mode, ControlMode::BotDriving);
    }

    #[test]
    fn invalid_transitions_from_idle_return_error() {
        let mut sm = ControlStateMachine::new();
        assert!(sm.pause().is_err());
        assert!(sm.resume().is_err());
        assert!(sm.take_the_wheel().is_err());
        assert!(sm.give_back().is_err());
    }

    #[test]
    fn stop_all_resets_to_idle_from_any_state() {
        let mut sm = ControlStateMachine::new();
        sm.start_run(sample_bot(), "Running");
        sm.take_the_wheel().expect("wheel");
        let stopped = sm.stop_all();
        assert_eq!(stopped.mode, ControlMode::Idle);
        assert!(stopped.bot.is_none());
        assert!(stopped.current_step.is_none());
        assert!(stopped.active_target.is_none());
        assert_eq!(stopped.run_generation, 2);
    }

    #[test]
    fn confirm_action_after_highlight_rejects_interrupted_actions() {
        let mut sm = ControlStateMachine::new();
        sm.start_run(sample_bot(), "Step 1");
        let generation = sm.run_generation();
        assert!(sm.begin_action(sample_rect(), "Click target"));
        assert!(sm.confirm_action_after_highlight(generation));

        // Human takes the wheel during the 300ms highlight lead-in
        sm.take_the_wheel().expect("take wheel");
        assert!(!sm.confirm_action_after_highlight(generation));
        assert_eq!(sm.snapshot().refused_actions_count, 1);
    }

    /// Drives the machine into each mode, returning it ready for a stop.
    fn machine_in(mode: ControlMode) -> ControlStateMachine {
        let mut sm = ControlStateMachine::new();
        match mode {
            ControlMode::Idle => {}
            ControlMode::BotDriving => {
                sm.start_run(sample_bot(), "Running");
            }
            ControlMode::HumanDriving => {
                sm.start_run(sample_bot(), "Running");
                sm.take_the_wheel().expect("wheel");
            }
            ControlMode::Paused => {
                sm.start_run(sample_bot(), "Running");
                sm.pause().expect("pause");
            }
        }
        sm
    }

    const ALL_MODES: [ControlMode; 4] = [
        ControlMode::BotDriving,
        ControlMode::HumanDriving,
        ControlMode::Paused,
        ControlMode::Idle,
    ];

    #[test]
    fn state_rev_strictly_increases_on_every_emitting_method() {
        let mut sm = ControlStateMachine::new();
        let mut last = sm.state_rev();
        let mut check = |snap: ControlSnapshot, what: &str| {
            assert!(snap.state_rev > last, "{what} did not raise state_rev");
            last = snap.state_rev;
        };
        // refused while Idle still emits a snapshot, so it must raise the revision too
        assert!(!sm.begin_action(sample_rect(), "idle refused"));
        check(sm.snapshot(), "begin_action (refused)");
        check(sm.start_run(sample_bot(), "s"), "start_run");
        assert!(sm.begin_action(sample_rect(), "a"));
        check(sm.snapshot(), "begin_action");
        assert!(sm.begin_action_untargeted("u"));
        check(sm.snapshot(), "begin_action_untargeted");
        let generation = sm.run_generation();
        assert!(!sm.confirm_action_after_highlight(generation + 1)); // refusal emits
        check(sm.snapshot(), "confirm_action_after_highlight (refused)");
        sm.record_click(1.0, 2.0, 3).expect("click");
        check(sm.snapshot(), "record_click");
        check(sm.set_current_step("x"), "set_current_step");
        check(sm.clear_overlay(), "clear_overlay");
        check(
            sm.set_stop_hotkey("Ctrl+Shift+S").expect("hk"),
            "set_stop_hotkey",
        );
        check(sm.pause().expect("pause"), "pause");
        check(sm.resume().expect("resume"), "resume");
        check(sm.take_the_wheel().expect("wheel"), "take_the_wheel");
        check(sm.give_back().expect("back"), "give_back");
        check(sm.stop_all(), "stop_all");
        check(sm.stop_all(), "stop_all (Idle->Idle)");
        check(sm.stop_with(StopCause::Finished), "stop_with(Finished)");
        let generation = sm.run_generation();
        check(sm.finish_run(generation).0, "finish_run");
        // an invalid transition changes nothing and must not raise it
        let before = sm.state_rev();
        assert!(sm.pause().is_err());
        assert_eq!(sm.state_rev(), before);
    }

    #[test]
    fn user_stop_raises_stop_seq_and_generation_from_every_mode() {
        for mode in ALL_MODES {
            let mut sm = machine_in(mode);
            let (seq, generation) = (sm.stop_seq(), sm.run_generation());
            let snap = sm.stop_all();
            assert_eq!(snap.mode, ControlMode::Idle, "{mode:?}");
            assert_eq!(snap.stop_seq, seq + 1, "{mode:?}");
            assert_eq!(snap.run_generation, generation + 1, "{mode:?}");
            // a second press while already idle is another Stop
            assert_eq!(sm.stop_all().stop_seq, seq + 2, "{mode:?} idle->idle");
        }
    }

    #[test]
    fn only_user_stops_raise_stop_seq() {
        let mut sm = ControlStateMachine::new();
        sm.start_run(sample_bot(), "s");
        let generation = sm.run_generation();
        let _ = sm.begin_action(sample_rect(), "a");
        let _ = sm.confirm_action_after_highlight(generation);
        let _ = sm.record_click(1.0, 1.0, 1);
        sm.set_current_step("x");
        sm.clear_overlay();
        sm.set_stop_hotkey("Ctrl+Alt+F9").expect("hk");
        sm.pause().expect("pause");
        sm.resume().expect("resume");
        sm.take_the_wheel().expect("wheel");
        // refused begin_action while the human drives
        assert!(!sm.begin_action(sample_rect(), "refused"));
        sm.give_back().expect("back");
        assert_eq!(sm.stop_seq(), 0, "no non-stop method may raise stop_seq");
        let generation = sm.run_generation();
        sm.finish_run(generation);
        assert_eq!(sm.stop_seq(), 0, "finish_run must not raise stop_seq");
        assert_eq!(sm.stop_all().stop_seq, 1);
    }

    #[test]
    fn finished_stop_raises_generation_not_stop_seq() {
        for mode in ALL_MODES {
            let mut sm = machine_in(mode);
            let (seq, generation) = (sm.stop_seq(), sm.run_generation());
            let snap = sm.stop_with(StopCause::Finished);
            assert_eq!(snap.mode, ControlMode::Idle, "{mode:?}");
            assert_eq!(snap.run_generation, generation + 1, "{mode:?}");
            assert_eq!(snap.stop_seq, seq, "{mode:?}");
            assert!(
                snap.bot.is_none() && snap.current_step.is_none(),
                "{mode:?}"
            );
        }
    }

    #[test]
    fn finish_with_stale_generation_is_noop() {
        let mut sm = ControlStateMachine::new();
        sm.start_run(sample_bot(), "old run");
        let old_generation = sm.run_generation();
        sm.start_run(sample_bot(), "new run");
        let before = sm.snapshot();

        let (snap, changed) = sm.finish_run(old_generation);
        assert!(!changed);
        assert_eq!(snap, before, "stale finish must not change anything");
        assert_eq!(sm.snapshot(), before);
        assert!(sm.is_bot_driving(), "newer native run must keep driving");

        let current = sm.run_generation();
        let (snap, changed) = sm.finish_run(current);
        assert!(changed);
        assert_eq!(snap.mode, ControlMode::Idle);
        assert_eq!(snap.stop_seq, 0);
    }

    #[test]
    fn stop_token_is_reported_with_its_seq() {
        let mut sm = ControlStateMachine::new();
        let snap = sm.stop_with(StopCause::User {
            token: Some("stop-1".to_string()),
        });
        assert_eq!(snap.stop_seq, 1);
        assert_eq!(snap.last_stop_token.as_deref(), Some("stop-1"));
        // a finish keeps the token of the stop that produced the current seq
        sm.start_run(sample_bot(), "s");
        let generation = sm.run_generation();
        let snap = sm.finish_run(generation).0;
        assert_eq!(snap.stop_seq, 1);
        assert_eq!(snap.last_stop_token.as_deref(), Some("stop-1"));
        // an untokened user stop replaces it, so token and seq never disagree
        let snap = sm.stop_all();
        assert_eq!(snap.stop_seq, 2);
        assert_eq!(snap.last_stop_token, None);
        let json = serde_json::to_value(&snap).expect("serialize");
        assert_eq!(json["stopSeq"], 2);
        assert_eq!(json["stateRev"], snap.state_rev);
        assert!(json.get("lastStopToken").is_none());
    }

    #[test]
    fn ack_stop_is_monotonic_idempotent_silent_and_cannot_ack_the_future() {
        let mut sm = ControlStateMachine::new();
        sm.stop_all();
        sm.stop_all();
        sm.stop_all();
        let rev = sm.state_rev();
        assert_eq!(sm.ack_stop(2).stop_acked_seq, 2);
        assert_eq!(sm.ack_stop(2).stop_acked_seq, 2, "idempotent");
        assert_eq!(sm.ack_stop(1).stop_acked_seq, 2, "never goes backwards");
        assert_eq!(sm.ack_stop(99).stop_acked_seq, 3, "clamped to stop_seq");
        assert_eq!(sm.state_rev(), rev, "ack is silent");
        // a stop raised after the ack is again unacknowledged
        let snap = sm.stop_all();
        assert!(snap.stop_seq > snap.stop_acked_seq);
    }

    #[derive(Default)]
    struct RecordingSink {
        snapshots: Mutex<Vec<ControlSnapshot>>,
    }

    impl OverlayEventSink for RecordingSink {
        fn on_show_target(&self, _payload: &OverlayShowTargetPayload) {}
        fn on_click(&self, _payload: &OverlayClickPayload) {}
        fn on_clear(&self) {}
        fn on_state_changed(&self, snapshot: &ControlSnapshot) {
            self.snapshots.lock().push(snapshot.clone());
        }
    }

    #[tokio::test]
    async fn handle_finish_and_ack_emit_only_when_something_changed() {
        let handle = ControlHandle::new(Duration::from_millis(1));
        let sink = Arc::new(RecordingSink::default());
        handle.set_sink(sink.clone());
        handle.start_run(sample_bot(), "run");
        let generation = handle.snapshot().run_generation;
        let emitted = sink.snapshots.lock().len();

        // stale finish: no event, run untouched
        handle.finish_run(generation + 5);
        assert_eq!(sink.snapshots.lock().len(), emitted);
        assert_eq!(handle.snapshot().mode, ControlMode::BotDriving);

        // ack: no event
        handle.ack_stop(0);
        assert_eq!(sink.snapshots.lock().len(), emitted);

        // real finish: one event, no stop_seq
        let snap = handle.finish_run(generation);
        assert_eq!(sink.snapshots.lock().len(), emitted + 1);
        assert_eq!((snap.mode, snap.stop_seq), (ControlMode::Idle, 0));

        // token stop through the handle
        let snap = handle.stop_user(Some("t".to_string()));
        assert_eq!(snap.stop_seq, 1);
        assert_eq!(snap.last_stop_token.as_deref(), Some("t"));
        let revs: Vec<u64> = sink.snapshots.lock().iter().map(|s| s.state_rev).collect();
        assert!(
            revs.windows(2).all(|w| w[0] < w[1]),
            "emitted revs {revs:?}"
        );
    }

    #[test]
    fn hotkey_edge_fires_once_per_press() {
        // no mode input at all: Idle is not special
        assert_eq!(hotkey_edge(false, false), (false, false));
        assert_eq!(hotkey_edge(false, true), (true, true));
        // key held / OS auto-repeat: no further fire
        assert_eq!(hotkey_edge(true, true), (false, true));
        assert_eq!(hotkey_edge(true, false), (false, false));
        let mut was = false;
        let fires = [false, true, true, true, false, true, true, false]
            .into_iter()
            .filter(|&now| {
                let (fire, next) = hotkey_edge(was, now);
                was = next;
                fire
            })
            .count();
        assert_eq!(fires, 2, "two physical presses");
    }

    #[tokio::test]
    async fn hotkey_handler_produces_one_stop_per_press_in_every_mode_including_idle() {
        for mode in ALL_MODES {
            let handle = ControlHandle::new(Duration::from_millis(1));
            match mode {
                ControlMode::Idle => {}
                ControlMode::BotDriving => {
                    handle.start_run(sample_bot(), "r");
                }
                ControlMode::HumanDriving => {
                    handle.start_run(sample_bot(), "r");
                    handle.take_the_wheel().expect("wheel");
                }
                ControlMode::Paused => {
                    handle.start_run(sample_bot(), "r");
                    handle.pause().expect("pause");
                }
            }
            let base = handle.snapshot().stop_seq;
            let mut was = false;
            // press held for several polls = one Stop
            for _ in 0..5 {
                was = poll_hotkey_once(&handle, was, true);
            }
            assert_eq!(
                handle.snapshot().stop_seq,
                base + 1,
                "{mode:?}: one press = one Stop"
            );
            assert_eq!(handle.snapshot().mode, ControlMode::Idle, "{mode:?}");
            // released, then pressed again = a second Stop
            was = poll_hotkey_once(&handle, was, false);
            let _ = poll_hotkey_once(&handle, was, true);
            assert_eq!(
                handle.snapshot().stop_seq,
                base + 2,
                "{mode:?}: second press"
            );
        }
    }

    #[tokio::test]
    async fn changing_the_hotkey_while_held_produces_no_stop_and_one_press_one_stop() {
        let handle = ControlHandle::new(Duration::from_millis(1));
        let mut watcher = HotkeyWatcher::default();
        // key state shared by the fake keyboard: which hotkey combos are physically down
        let down = |hk: ParsedHotkey| hk.key == HotkeyKey::Letter('S');
        let tick = |w: &mut HotkeyWatcher| {
            let view = Some(handle.state.lock().hotkey_view());
            w.tick(&handle, view, down);
        };
        tick(&mut watcher); // seed on the default hotkey (not held)
        assert_eq!(handle.snapshot().stop_seq, 0);

        // user records Ctrl+Shift+S in Settings while the keys are still held
        handle.set_stop_hotkey("Ctrl+Shift+S").expect("hotkey");
        for _ in 0..5 {
            tick(&mut watcher);
        }
        assert_eq!(
            handle.snapshot().stop_seq,
            0,
            "held keys must not synthesise a press"
        );

        // release (fake keyboard: nothing down), press again = exactly one Stop
        let released = |w: &mut HotkeyWatcher| {
            let view = Some(handle.state.lock().hotkey_view());
            w.tick(&handle, view, |_| false);
        };
        released(&mut watcher);
        for _ in 0..5 {
            tick(&mut watcher);
        }
        assert_eq!(handle.snapshot().stop_seq, 1);
    }

    #[tokio::test]
    async fn hotkey_view_is_cached_and_only_changes_when_the_hotkey_is_set() {
        let handle = ControlHandle::new(Duration::from_millis(1));
        let (gen0, parsed0) = handle.state.lock().hotkey_view();
        assert_eq!(parsed0, parse_hotkey(DEFAULT_STOP_HOTKEY).expect("default"));
        // stops, runs and steps never touch it: the listener has nothing to re-parse
        handle.start_run(sample_bot(), "r");
        handle.set_current_step("x");
        handle.stop_all();
        assert_eq!(handle.state.lock().hotkey_view(), (gen0, parsed0));
        // an invalid hotkey is rejected without changing the cached value
        assert!(handle.set_stop_hotkey("nonsense").is_err());
        assert_eq!(handle.state.lock().hotkey_view(), (gen0, parsed0));
        handle.set_stop_hotkey("Alt+F4").expect("hotkey");
        let (gen1, parsed1) = handle.state.lock().hotkey_view();
        assert_ne!(gen1, gen0);
        assert_eq!(parsed1, parse_hotkey("Alt+F4").expect("parse"));
    }

    #[tokio::test]
    async fn unusable_hotkey_resets_the_edge_detector() {
        let handle = ControlHandle::new(Duration::from_millis(1));
        let mut watcher = HotkeyWatcher::default();
        let view = Some(handle.state.lock().hotkey_view());
        watcher.tick(&handle, view, |_| true); // seeded as held
        watcher.tick(&handle, None, |_| true); // hotkey unset/invalid: reset
        let view = Some(handle.state.lock().hotkey_view());
        watcher.tick(&handle, view, |_| true); // re-seeded as held, still no Stop
        assert_eq!(handle.snapshot().stop_seq, 0);
    }

    #[test]
    fn confirmed_action_leaves_state_rev_alone_but_refusals_raise_it() {
        let mut sm = ControlStateMachine::new();
        sm.start_run(sample_bot(), "s");
        let generation = sm.run_generation();
        let rev = sm.state_rev();
        assert!(sm.confirm_action_after_highlight(generation));
        assert_eq!(
            sm.state_rev(),
            rev,
            "confirmed: nothing changed, nothing emitted"
        );
        assert!(!sm.confirm_action_after_highlight(generation + 1));
        assert_eq!(sm.state_rev(), rev + 1, "stale generation emits a refusal");
        sm.take_the_wheel().expect("wheel");
        let rev = sm.state_rev();
        assert!(!sm.confirm_action_after_highlight(generation));
        assert_eq!(sm.state_rev(), rev + 1, "refused while human drives");
    }

    #[tokio::test]
    async fn handle_show_target_click_and_refusals_emit_strictly_higher_revisions() {
        let handle = ControlHandle::new(Duration::from_millis(1));
        let sink = Arc::new(RecordingSink::default());
        handle.set_sink(sink.clone());
        let last_rev = || sink.snapshots.lock().last().map(|s| s.state_rev);

        handle.start_run(sample_bot(), "r");
        let r0 = last_rev().expect("start_run emitted");
        handle.show_target(sample_rect(), "t", "click", None);
        let r1 = last_rev().expect("show_target emitted");
        assert!(r1 > r0, "show_target must raise state_rev");
        let before = handle.snapshot().state_rev;
        handle.click(1.0, 2.0);
        assert!(
            handle.snapshot().state_rev > before,
            "click must raise state_rev"
        );
        // refused begin_action (human driving) emits a higher revision
        handle.take_the_wheel().expect("wheel");
        let r2 = last_rev().expect("take_the_wheel emitted");
        assert!(!handle.begin_action(sample_rect(), "refused").await);
        assert!(last_rev().expect("refusal emitted") > r2);
        // ack after a real Stop: acknowledges, emits nothing, keeps the revision
        let stopped = handle.stop_all();
        let emitted = sink.snapshots.lock().len();
        let acked = handle.ack_stop(stopped.stop_seq);
        assert_eq!(acked.stop_acked_seq, stopped.stop_seq);
        assert_eq!(acked.state_rev, stopped.state_rev);
        assert_eq!(sink.snapshots.lock().len(), emitted);
    }

    #[tokio::test]
    async fn internal_channel_begin_action_and_is_human_driving_work() {
        let handle = ControlHandle::new(Duration::from_millis(5));
        assert!(!handle.is_human_driving());
        assert!(!handle.is_human_driving_via_channel().await);

        // Refused while Idle
        assert!(!handle.begin_action(sample_rect(), "While idle").await);

        // Start run -> allowed
        handle.start_run(sample_bot(), "Active step");
        assert!(handle.begin_action(sample_rect(), "Click button").await);

        // Take the wheel -> is_human_driving is true and begin_action is refused
        handle.take_the_wheel().expect("take wheel");
        assert!(handle.is_human_driving());
        assert!(handle.is_human_driving_via_channel().await);
        assert!(!handle.begin_action(sample_rect(), "Refused action").await);

        // Give back -> allowed again
        handle.give_back().expect("give back");
        assert!(!handle.is_human_driving());
        assert!(handle.begin_action(sample_rect(), "Resumed action").await);

        // Stop all -> back to Idle
        handle.stop_all();
        assert_eq!(handle.snapshot().mode, ControlMode::Idle);
    }

    #[tokio::test]
    async fn untargeted_actions_follow_the_same_rules_without_a_highlight() {
        let handle = ControlHandle::new(Duration::from_millis(5));
        assert!(
            !handle.begin_action_untargeted("Press ctrl+s").await,
            "idle refuses"
        );
        handle.start_run(sample_bot(), "Active step");
        assert!(handle.begin_action_untargeted("Press ctrl+s").await);
        let snap = handle.snapshot();
        assert_eq!(snap.current_step.as_deref(), Some("Press ctrl+s"));
        assert!(snap.active_target.is_none());
        handle.take_the_wheel().expect("take wheel");
        assert!(!handle.begin_action_untargeted("Press ctrl+s").await);
        assert_eq!(handle.snapshot().refused_actions_count, 1);
    }

    #[test]
    fn dpi_and_multi_monitor_coordinate_conversion() {
        // Primary monitor at (0, 0), 3840x2160 physical, 200% DPI (scale_factor = 2.0)
        let primary = MonitorGeometry {
            monitor_index: 0,
            physical_x: 0.0,
            physical_y: 0.0,
            physical_width: 3840.0,
            physical_height: 2160.0,
            scale_factor: 2.0,
        };
        // Secondary monitor to the right at (3840, 0), 1920x1080 physical, 125% DPI (scale_factor = 1.25)
        let secondary = MonitorGeometry {
            monitor_index: 1,
            physical_x: 3840.0,
            physical_y: 0.0,
            physical_width: 1920.0,
            physical_height: 1080.0,
            scale_factor: 1.25,
        };

        let target_on_primary = OverlayRect {
            x: 1920.0,
            y: 1080.0,
            width: 400.0,
            height: 100.0,
        };
        assert!(rect_intersects_monitor(target_on_primary, primary));
        assert!(!rect_intersects_monitor(target_on_primary, secondary));

        let logical_primary = physical_rect_to_monitor_logical(target_on_primary, primary);
        assert_eq!(
            logical_primary,
            OverlayRect {
                x: 960.0,
                y: 540.0,
                width: 200.0,
                height: 50.0,
            }
        );

        let target_on_secondary = OverlayRect {
            x: 4090.0,
            y: 250.0,
            width: 250.0,
            height: 50.0,
        };
        assert!(!rect_intersects_monitor(target_on_secondary, primary));
        assert!(rect_intersects_monitor(target_on_secondary, secondary));

        let logical_secondary = physical_rect_to_monitor_logical(target_on_secondary, secondary);
        assert_eq!(
            logical_secondary,
            OverlayRect {
                x: 200.0,
                y: 200.0,
                width: 200.0,
                height: 40.0,
            }
        );

        let click_logical = physical_point_to_monitor_logical(4090.0, 250.0, secondary);
        assert_eq!(click_logical, (200.0, 200.0));
    }

    #[test]
    fn hotkey_parsing_normalizes_and_validates() {
        let parsed = parse_hotkey("ctrl + alt + esc").expect("valid default hotkey");
        assert_eq!(parsed.canonical_string(), "Ctrl+Alt+Esc");

        let f_key = parse_hotkey("Alt+Shift+F12").expect("valid function hotkey");
        assert_eq!(f_key.canonical_string(), "Alt+Shift+F12");

        // Reject missing modifier or missing key
        assert!(parse_hotkey("Escape").is_err());
        assert!(parse_hotkey("Ctrl+Alt").is_err());
        assert!(parse_hotkey("").is_err());
    }

    #[test]
    fn bot_run_meta_preserves_explicit_bot_shape_across_serde_and_snapshot() {
        // `bot-cube-0` hashes to `cube` in `fallbackBotShape`, so storing `heart`
        // proves the explicit shape survives IPC instead of falling back to the hash.
        let incoming = serde_json::json!({
            "botId": "bot-cube-0",
            "botName": "HeartBot",
            "botShape": "heart",
            "botColor": "#4c8dff"
        });
        let meta: BotRunMeta =
            serde_json::from_value(incoming).expect("BotRunMeta should deserialize botShape");
        assert_eq!(meta.bot_shape.as_deref(), Some("heart"));

        let mut sm = ControlStateMachine::new();
        let snap = sm.start_run(meta, "Working");
        let out = serde_json::to_value(&snap).expect("ControlSnapshot should serialize");
        assert_eq!(out["bot"]["botShape"], "heart");
        assert_eq!(out["bot"]["botId"], "bot-cube-0");

        let legacy: BotRunMeta = serde_json::from_value(serde_json::json!({
            "botId": "legacy-id",
            "botName": "Legacy"
        }))
        .expect("legacy BotRunMeta without botShape should deserialize");
        assert!(legacy.bot_shape.is_none());
        let legacy_json = serde_json::to_value(&legacy).expect("legacy serialize");
        assert!(legacy_json.get("botShape").is_none());
    }
}
