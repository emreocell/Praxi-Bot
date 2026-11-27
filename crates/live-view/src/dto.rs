//! Version 1 DTOs. Input deliberately does not implement Debug.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const MEDIA_TYPE: &str = "application/vnd.praxi.live-view.v1";
pub const MAX_HEADER: usize = 8 * 1024;
pub const MAX_PAYLOAD: usize = 2 * 1024 * 1024;
pub const MAX_BATCH_BYTES: usize = 16 * 1024;
pub const MAX_EVENTS: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    Browser,
    Desktop,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Capabilities {
    pub view: bool,
    pub input: bool,
    pub codecs: Vec<Codec>,
    pub reason: Option<Reason>,
}

/// Why a kind of screen is not offered, as a small FIXED set. Peers only ever
/// see these codes (the UI words them in its own language); raw backend
/// diagnostics (paths, user names, display names, environment values) stay in
/// the local daemon log and never reach this type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum UnavailableCode {
    /// Desktop control is switched off in the daemon's settings.
    DesktopOff,
    /// This platform has no screen capture backend.
    UnsupportedPlatform,
    /// A Wayland session: only X11 can be captured.
    Wayland,
    /// No X11 display is configured or reachable.
    NoDisplay,
    /// The desktop could not be opened (locked session, secure desktop,
    /// missing permission).
    ScreenUnavailable,
    /// The desktop backend failed to start.
    StartFailed,
    /// The screen is being probed right now.
    Starting,
    /// The probe did not answer in time; it is retried.
    TimedOut,
}

/// A kind of screen this host cannot offer, and why. Lets the UI say
/// "No screen: <reason>" instead of an empty panel.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TargetUnavailable {
    pub kind: TargetKind,
    pub host_id: String,
    pub reason: Reason,
    /// Fixed reason code; never free text.
    pub code: UnavailableCode,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TargetInfo {
    pub target_id: Uuid,
    pub kind: TargetKind,
    pub host_id: String,
    pub bot_id: Option<Uuid>,
    pub run_id: Option<Uuid>,
    pub generation: u64,
    pub capabilities: Capabilities,
    pub unavailable_reason: Option<Reason>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum Codec {
    Jpeg,
    Webp,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    Watch,
    Live,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    BrowserIdle,
    /// The viewer showed no sign of life (no input, window hidden or unfocused)
    /// for the idle limit: the host stopped producing images to save resources.
    ViewerIdle,
    /// The session reached its hard time limit.
    TimeLimit,
    RemoteNodeLiveView,
    Unsupported,
    Unavailable,
    Expired,
    Revoked,
    Disconnected,
    Stale,
    GeometryChanged,
    RefreshPending,
    FrameBudgetExceeded,
    DrainTimeout,
    AuditFailed,
    ResetFailed,
    PartialInjection,
    RateLimited,
    Restart,
    PersistenceFailed,
    /// The authenticated local user overrode another actor's takeover.
    LocalOverride,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum ViewState {
    Watch,
    Acquiring,
    Active,
    Paused,
    Unavailable,
    Closed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum Cause {
    Idle,
    Action,
    Refresh,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum Units {
    PhysicalPx,
    CssPx,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum PointerPhase {
    Move,
    Down,
    Up,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum KeyPhase {
    Down,
    Up,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum Button {
    Left,
    Middle,
    Right,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum Modifier {
    Alt,
    Ctrl,
    Meta,
    Shift,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum HumanAction {
    LoginCompleted,
    ApplicationOpened,
    Other,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Completed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct EffectiveProfile {
    pub profile: Profile,
    pub max_width: u32,
    pub max_height: u32,
    pub quality: u8,
    pub max_fps: u8,
    pub bytes_per_second: u32,
    pub burst_bytes: u32,
    pub codec: Codec,
}

/// Largest long edge of a frame on the wire: 1920 px, in either orientation (landscape
/// 1920x1080 or portrait 1080x1920).
#[must_use]
pub const fn wire_size_ok(width: u32, height: u32) -> bool {
    (width <= 1920 && height <= 1080) || (width <= 1080 && height <= 1920)
}

/// Whether a frame fits a profile's size box; a portrait frame may use the box turned on its side.
#[must_use]
pub const fn fits_box(width: u32, height: u32, max_width: u32, max_height: u32) -> bool {
    (width <= max_width && height <= max_height)
        || (height > width && width <= max_height && height <= max_width)
}

impl EffectiveProfile {
    #[must_use]
    pub fn watch() -> Self {
        Self {
            profile: Profile::Watch,
            max_width: 1920,
            max_height: 1080,
            quality: 80,
            max_fps: 5,
            bytes_per_second: 1024 * 1024,
            burst_bytes: 1024 * 1024,
            codec: Codec::Jpeg,
        }
    }
    #[must_use]
    pub fn live() -> Self {
        Self {
            profile: Profile::Live,
            max_width: 1920,
            max_height: 1080,
            quality: 75,
            max_fps: 10,
            bytes_per_second: 1536 * 1024,
            burst_bytes: 2 * 1024 * 1024,
            codec: Codec::Jpeg,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct CreateSession {
    pub target_id: Uuid,
    pub profile: Profile,
    pub max_width: u32,
    pub max_height: u32,
    pub codecs: Vec<Codec>,
    pub client_id: Uuid,
    /// The bot whose panel asks. When set, the target must be this bot's own
    /// browser or a shared desktop; another bot's browser is refused (403), so
    /// a bot-scoped view can never resolve to someone else's screen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bot_id: Option<Uuid>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SessionInfo {
    pub session_id: Uuid,
    pub generation: u64,
    pub effective_profile: EffectiveProfile,
    pub input_capabilities: bool,
    pub stream_url: String,
    pub expires_in_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct SessionUpdate {
    pub profile: Option<Profile>,
    pub max_width: Option<u32>,
    pub max_height: Option<u32>,
    pub quality: Option<u8>,
    pub bytes_per_second: Option<u32>,
    pub last_rendered_frame_seq: Option<u64>,
    pub decode_ms: Option<u32>,
    pub buffered_frames: Option<u32>,
}

/// `POST /v1/view/sessions/{id}/ack`: the newest frame this viewer has put on screen. Unlike
/// `SessionUpdate` it changes nothing else and never forces a refresh, so a viewer can send
/// it after every frame; the host paces its frames by it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct FrameAck {
    pub last_rendered_frame_seq: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct Heartbeat {
    pub takeover_id: Option<Uuid>,
    pub control_epoch: Option<u64>,
    /// `false`: the viewer is hidden or unfocused right now. Leave it out (or
    /// `true`) while someone is looking; a session whose viewer reports
    /// inactive for the idle limit is ended by the host.
    #[serde(default)]
    pub active: Option<bool>,
}

/// What a viewer needs to know before it asks for the live screen.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum ScreenStreamState {
    /// No live session: the host produces no images.
    Idle,
    /// A session exists, its first frame is not out yet.
    Starting,
    /// Frames are flowing to this viewer.
    Live,
    /// The last live session was ended by the idle or time limit.
    PausedIdle,
    /// There is no screen to show (see `reason`).
    Unavailable,
}

/// `GET /v1/view/status`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ScreenStatus {
    pub state: ScreenStreamState,
    /// Fixed code when `unavailable` (the same codes as `GET /v1/view/availability`).
    pub reason: Option<UnavailableCode>,
    /// Seconds without a sign of life after which the host stops the stream.
    pub idle_timeout_secs: u64,
    /// Longest a single live session may run, in seconds.
    pub hard_cap_secs: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct TakeoverRequest {
    pub session_id: Uuid,
    pub request_id: Uuid,
    pub reacquire_takeover_id: Option<Uuid>,
    #[serde(default)]
    pub recover: bool,
    pub reason: Option<RecoveryReason>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum RecoveryReason {
    OwnerLost,
    DeviceRevoked,
    DaemonRestart,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TakeoverInfo {
    pub id: Uuid,
    pub session_id: Uuid,
    pub target_id: Uuid,
    pub owner: String,
    pub state: ViewState,
    pub control_epoch: u64,
    pub input_expires_in_ms: u64,
    pub reason: Option<Reason>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TakeoverAccepted {
    pub takeover_id: Uuid,
    pub state: ViewState,
    pub control_epoch: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct ReleaseRequest {
    pub takeover_id: Uuid,
    pub control_epoch: u64,
    pub request_id: Uuid,
    pub outcome: Outcome,
    pub human_action: Option<HumanAction>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SourceRect {
    pub x: f64,
    pub y: f64,
    pub width: u32,
    pub height: u32,
    pub units: Units,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct FrameMetadata {
    pub target_id: Uuid,
    pub generation: u64,
    pub frame_seq: u64,
    pub captured_at_ms: u64,
    pub cause: Cause,
    pub action_seq: Option<u64>,
    pub codec: Codec,
    pub width: u32,
    pub height: u32,
    pub source_rect: SourceRect,
    pub geometry_revision: u64,
    pub repeated: bool,
    /// Where the bot's own pointer is on this frame (a fraction of its width and height), when
    /// the bot has one here and the owner has not switched it off. Left out otherwise, so
    /// older viewers and golden records are unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bot_pointer: Option<crate::bot_cursor::BotPointer>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RecordHeader {
    Frame {
        v: u8,
        session_id: Uuid,
        sent_at_ms: u64,
        #[serde(flatten)]
        frame: FrameMetadata,
    },
    Status {
        v: u8,
        session_id: Uuid,
        state: ViewState,
        reason: Option<Reason>,
        effective_profile: EffectiveProfile,
        control_epoch: u64,
        input_allowed: bool,
    },
    Heartbeat {
        v: u8,
        server_time_ms: u64,
    },
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum InputEvent {
    Pointer {
        phase: PointerPhase,
        x: f64,
        y: f64,
        button: Option<Button>,
        buttons: u8,
        modifiers: Vec<Modifier>,
    },
    Wheel {
        x: f64,
        y: f64,
        delta_x: f64,
        delta_y: f64,
        modifiers: Vec<Modifier>,
    },
    Key {
        phase: KeyPhase,
        code: String,
        key: String,
        location: u8,
        repeat: bool,
        modifiers: Vec<Modifier>,
    },
    Text {
        text: String,
    },
    Reset,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct InputBatch {
    pub v: u8,
    pub takeover_id: Uuid,
    pub control_epoch: u64,
    pub generation: u64,
    pub geometry_revision: u64,
    pub first_seq: u64,
    pub events: Vec<InputEvent>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct InputResult {
    pub accepted_through: u64,
    pub applied_through: u64,
    pub state: ViewState,
}

impl InputEvent {
    #[must_use]
    pub fn positional(&self) -> bool {
        matches!(self, Self::Pointer { .. } | Self::Wheel { .. })
    }
    #[must_use]
    pub fn text_bytes(&self) -> usize {
        if let Self::Text { text } = self {
            text.len()
        } else {
            0
        }
    }
    #[must_use]
    pub fn valid(&self) -> bool {
        let point = |x: f64, y: f64| {
            x.is_finite() && y.is_finite() && (0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y)
        };
        match self {
            Self::Pointer {
                phase,
                x,
                y,
                button,
                buttons,
                modifiers,
            } => {
                point(*x, *y)
                    && *buttons <= 7
                    && modifiers.len() <= 4
                    && (matches!(phase, PointerPhase::Move) || button.is_some())
            }
            Self::Wheel {
                x,
                y,
                delta_x,
                delta_y,
                modifiers,
            } => {
                point(*x, *y)
                    && delta_x.is_finite()
                    && delta_y.is_finite()
                    && delta_x.abs() <= 1200.0
                    && delta_y.abs() <= 1200.0
                    && modifiers.len() <= 4
            }
            Self::Key {
                code,
                key,
                location,
                modifiers,
                ..
            } => allowed_code(code) && key.len() <= 64 && *location <= 3 && modifiers.len() <= 4,
            Self::Text { text } => !text.is_empty() && text.len() <= 4096,
            Self::Reset => true,
        }
    }
}
fn allowed_code(code: &str) -> bool {
    (code.len() == 4 && code.starts_with("Key") && code.as_bytes()[3].is_ascii_uppercase())
        || (code.len() == 6 && code.starts_with("Digit") && code.as_bytes()[5].is_ascii_digit())
        || matches!(
            code,
            "Enter"
                | "Tab"
                | "Escape"
                | "Backspace"
                | "Delete"
                | "Space"
                | "ArrowLeft"
                | "ArrowRight"
                | "ArrowUp"
                | "ArrowDown"
                | "Home"
                | "End"
                | "PageUp"
                | "PageDown"
                | "ShiftLeft"
                | "ShiftRight"
                | "ControlLeft"
                | "ControlRight"
                | "AltLeft"
                | "AltRight"
                | "MetaLeft"
                | "MetaRight"
                | "CapsLock"
                | "Minus"
                | "Equal"
                | "BracketLeft"
                | "BracketRight"
                | "Backslash"
                | "Semicolon"
                | "Quote"
                | "Backquote"
                | "Comma"
                | "Period"
                | "Slash"
        )
        || code
            .strip_prefix('F')
            .and_then(|n| n.parse::<u8>().ok())
            .is_some_and(|n| (1..=12).contains(&n))
}
