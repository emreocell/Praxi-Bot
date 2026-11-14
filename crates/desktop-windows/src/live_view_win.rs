//! Windows live desktop view & input injection (`BitBlt` + `SendInput`).
//!
//! Uses dedicated `"live-view-cap"` and `"live-view-in"` worker threads coordinated
//! by [`desktop_api::live_view::DesktopViewTarget`] instead of blocking on the
//! `"uia-sta"` COM automation thread.
//!
//! # Integrity, Security, and Limitations
//!
//! * Shares the caller's interactive Windows desktop session (`"Default"`), never
//!   spawns a second desktop, never writes temporary image files, and never logs
//!   raw pixel buffers, keystrokes, or typed text.
//! * Verifies active input desktop accessibility (`OpenInputDesktop` +
//!   `GetUserObjectInformationW(UOI_NAME)`) and fails closed with
//!   [`Reason::Unavailable`] / [`ViewError::Unavailable`] when locked, under
//!   `Winlogon` / UAC secure desktop, screensaver, or in a non-interactive
//!   session (never fabricates black frames).
//! * Respects Windows UIPI (User Interface Privilege Isolation): this backend
//!   never attempts to elevate privileges or bypass UIPI. Note that `SendInput`
//!   returning `0` or fewer events than submitted can reflect either UIPI
//!   filtering or an input-desktop block and cannot by itself distinguish UIPI
//!   from a desktop transition.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use desktop_api::live_view::{
    AvailabilityProbeFn, CaptureDriver, CaptureDriverFactory, CaptureFailure, CaptureScope,
    CapturedRawInfo, DesktopViewTarget, DesktopViewTargetConfig, GeometrySnapshot, InputDriver,
    InputDriverFactory, LiveViewOptions, MAX_RAW_PIXELS, MonitorDescriptor, PixelFormat, PixelRect,
    map_normalized_point, point_to_virtual_absolute, select_capture_rect,
    wheel_pixels_to_win32_delta,
};
use live_view::{
    Button, InputEvent, KeyPhase, Modifier, PointerPhase, Reason, SourceRect, ViewError,
};

use crate::pure;

/// FNV-1a 64-bit hash of Windows virtual-desktop geometry, monitor list, and DPI
/// values. Deliberately excludes timestamps so `geometry_revision` stays stable
/// while display topology and scaling remain unchanged.
#[must_use]
pub fn hash_win_layout(virtual_rect: PixelRect, monitors: &[MonitorDescriptor]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |v: u64| {
        hash ^= v;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    };
    mix(i64::from(virtual_rect.x).cast_unsigned());
    mix(i64::from(virtual_rect.y).cast_unsigned());
    mix(u64::from(virtual_rect.width));
    mix(u64::from(virtual_rect.height));
    mix(u64::try_from(monitors.len()).unwrap_or(u64::MAX));
    for mon in monitors {
        mix(i64::from(mon.rect.x).cast_unsigned());
        mix(i64::from(mon.rect.y).cast_unsigned());
        mix(u64::from(mon.rect.width));
        mix(u64::from(mon.rect.height));
        mix(u64::from(mon.dpi));
        mix(u64::from(mon.is_primary));
        for b in mon.name.as_bytes() {
            mix(u64::from(*b));
        }
    }
    hash
}

// ── Mockable Win32 Capture & Input Abstractions ───────────────────────────────

/// Display layout snapshot returned by [`WinCaptureApi::query_layout`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WinDesktopLayout {
    pub monitors: Vec<MonitorDescriptor>,
    pub virtual_rect: PixelRect,
    pub desktop_name: Option<String>,
    pub input_desktop_accessible: bool,
    pub display_token: u64,
}

/// Abstract Win32 GDI + desktop query interface (mockable in cross-platform unit tests).
pub trait WinCaptureApi: Send + 'static {
    /// Query current monitors, virtual screen rectangle, DPI, and interactive desktop status.
    ///
    /// # Errors
    ///
    /// Returns [`CaptureFailure::Unavailable`] when locked/Winlogon or unreachable.
    fn query_layout(&mut self) -> Result<WinDesktopLayout, CaptureFailure>;

    /// Capture `rect` as packed top-down `BGRA` pixels (`width * height * 4` bytes).
    ///
    /// # Errors
    ///
    /// Returns [`CaptureFailure`] if GDI capture fails, desktop locks, or limits are exceeded.
    fn bitblt_bgra(&mut self, rect: PixelRect, out_bgra: &mut Vec<u8>)
    -> Result<(), CaptureFailure>;
}

/// Mouse button variants supported by live-view input injection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WinMouseButton {
    Left,
    Middle,
    Right,
}

impl From<&Button> for WinMouseButton {
    fn from(value: &Button) -> Self {
        match value {
            Button::Left => Self::Left,
            Button::Middle => Self::Middle,
            Button::Right => Self::Right,
        }
    }
}

/// Platform-neutral Win32 `SendInput` action primitive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WinInputAction {
    MouseMoveAbs {
        abs_x: i32,
        abs_y: i32,
    },
    MouseButton {
        button: WinMouseButton,
        down: bool,
    },
    WheelVertical {
        abs_x: i32,
        abs_y: i32,
        delta: i32,
    },
    WheelHorizontal {
        abs_x: i32,
        abs_y: i32,
        delta: i32,
    },
    KeyVk {
        vk: u16,
        scan: u16,
        extended: bool,
        down: bool,
    },
    UnicodeKey {
        unit: u16,
        down: bool,
    },
}

/// Abstract Win32 `SendInput` interface (mockable in unit tests on any OS).
pub trait WinInputApi: Send + 'static {
    /// Verify interactive `"Default"` desktop access and return live `SM_*VIRTUALSCREEN` bounds.
    ///
    /// # Errors
    ///
    /// Returns [`ViewError::Unavailable`] if the desktop is locked, on Winlogon/UAC, or unreadable.
    fn check_input_desktop_ready(&mut self) -> Result<PixelRect, ViewError>;

    /// Translate a Win32 virtual-key code into a scan code via `MapVirtualKeyW`.
    fn map_vk_to_scancode(&self, vk: u16) -> u16;

    /// Inject `actions` into the OS input stream and return the count of accepted events.
    ///
    /// # Errors
    ///
    /// Returns [`ViewError::Unavailable`] if the OS desktop handle is unusable.
    fn send_inputs(&mut self, actions: &[WinInputAction]) -> Result<usize, ViewError>;
}

// ── DOM Code -> Win32 Virtual-Key & Extended Flag Mapping ────────────────────

const VK_BACK: u16 = 0x08;
const VK_TAB: u16 = 0x09;
const VK_RETURN: u16 = 0x0D;
const VK_CAPITAL: u16 = 0x14;
const VK_ESCAPE: u16 = 0x1B;
const VK_SPACE: u16 = 0x20;
const VK_PRIOR: u16 = 0x21;
const VK_NEXT: u16 = 0x22;
const VK_END: u16 = 0x23;
const VK_HOME: u16 = 0x24;
const VK_LEFT: u16 = 0x25;
const VK_UP: u16 = 0x26;
const VK_RIGHT: u16 = 0x27;
const VK_DOWN: u16 = 0x28;
const VK_DELETE: u16 = 0x2E;
const VK_LWIN: u16 = 0x5B;
const VK_RWIN: u16 = 0x5C;
const VK_LSHIFT: u16 = 0xA0;
const VK_RSHIFT: u16 = 0xA1;
const VK_LCONTROL: u16 = 0xA2;
const VK_RCONTROL: u16 = 0xA3;
const VK_LMENU: u16 = 0xA4;
const VK_RMENU: u16 = 0xA5;
const VK_OEM_1: u16 = 0xBA;
const VK_OEM_PLUS: u16 = 0xBB;
const VK_OEM_COMMA: u16 = 0xBC;
const VK_OEM_MINUS: u16 = 0xBD;
const VK_OEM_PERIOD: u16 = 0xBE;
const VK_OEM_2: u16 = 0xBF;
const VK_OEM_3: u16 = 0xC0;
const VK_OEM_4: u16 = 0xDB;
const VK_OEM_5: u16 = 0xDC;
const VK_OEM_6: u16 = 0xDD;
const VK_OEM_7: u16 = 0xDE;

/// Whether `vk` requires `KEYEVENTF_EXTENDEDKEY`, extending [`pure::is_extended`]
/// with right-side Ctrl (`VK_RCONTROL`) and Alt (`VK_RMENU`).
#[must_use]
pub fn is_win_extended_vk(vk: u16) -> bool {
    pure::is_extended(vk) || matches!(vk, VK_RCONTROL | VK_RMENU)
}

/// Map a validated DOM `KeyboardEvent.code` + `key` pair into a Win32 `VIRTUAL_KEY`.
///
/// Explicitly rejects composition/unidentified keys (`Dead`, `Unidentified`,
/// `Process`, `Compose`) and unknown DOM `code` strings rather than silently
/// dropping input.
///
/// # Errors
///
/// Returns [`ViewError::Unsupported`] for unmappable or composition-only keys.
pub fn dom_code_to_vk(code: &str, key: &str) -> Result<u16, ViewError> {
    if code.is_empty()
        || matches!(key, "Dead" | "Unidentified" | "Process" | "Compose")
        || key.is_empty()
    {
        return Err(ViewError::Unsupported);
    }
    if let Some(rest) = code.strip_prefix("Key")
        && rest.len() == 1
        && let Some(ch) = rest.chars().next()
        && ch.is_ascii_uppercase()
    {
        return Ok(0x41 + u16::from(ch as u8 - b'A'));
    }
    if let Some(rest) = code.strip_prefix("Digit")
        && rest.len() == 1
        && let Some(ch) = rest.chars().next()
        && ch.is_ascii_digit()
    {
        return Ok(0x30 + u16::from(ch as u8 - b'0'));
    }
    if let Some(rest) = code.strip_prefix('F')
        && let Ok(n) = rest.parse::<u16>()
        && (1..=12).contains(&n)
    {
        return Ok(0x70 + (n - 1));
    }

    let vk = match code {
        "Enter" => VK_RETURN,
        "Tab" => VK_TAB,
        "Escape" => VK_ESCAPE,
        "Backspace" => VK_BACK,
        "Delete" => VK_DELETE,
        "Space" => VK_SPACE,
        "ArrowLeft" => VK_LEFT,
        "ArrowRight" => VK_RIGHT,
        "ArrowUp" => VK_UP,
        "ArrowDown" => VK_DOWN,
        "Home" => VK_HOME,
        "End" => VK_END,
        "PageUp" => VK_PRIOR,
        "PageDown" => VK_NEXT,
        "ShiftLeft" => VK_LSHIFT,
        "ShiftRight" => VK_RSHIFT,
        "ControlLeft" => VK_LCONTROL,
        "ControlRight" => VK_RCONTROL,
        "AltLeft" => VK_LMENU,
        "AltRight" => VK_RMENU,
        "MetaLeft" => VK_LWIN,
        "MetaRight" => VK_RWIN,
        "CapsLock" => VK_CAPITAL,
        "Minus" => VK_OEM_MINUS,
        "Equal" => VK_OEM_PLUS,
        "BracketLeft" => VK_OEM_4,
        "BracketRight" => VK_OEM_6,
        "Backslash" => VK_OEM_5,
        "Semicolon" => VK_OEM_1,
        "Quote" => VK_OEM_7,
        "Backquote" => VK_OEM_3,
        "Comma" => VK_OEM_COMMA,
        "Period" => VK_OEM_PERIOD,
        "Slash" => VK_OEM_2,
        _ => return Err(ViewError::Unsupported),
    };
    Ok(vk)
}

const fn modifier_to_vk(m: &Modifier) -> u16 {
    match m {
        Modifier::Shift => VK_LSHIFT,
        Modifier::Ctrl => VK_LCONTROL,
        Modifier::Alt => VK_LMENU,
        Modifier::Meta => VK_LWIN,
    }
}

const fn modifier_matches_vk(m: &Modifier, vk: u16) -> bool {
    match m {
        Modifier::Shift => vk == VK_LSHIFT || vk == VK_RSHIFT,
        Modifier::Ctrl => vk == VK_LCONTROL || vk == VK_RCONTROL,
        Modifier::Alt => vk == VK_LMENU || vk == VK_RMENU,
        Modifier::Meta => vk == VK_LWIN || vk == VK_RWIN,
    }
}

fn vk_click_chunk(vk: u16, scan: u16) -> Vec<WinInputAction> {
    vec![
        WinInputAction::KeyVk {
            vk,
            scan,
            extended: false,
            down: true,
        },
        WinInputAction::KeyVk {
            vk,
            scan,
            extended: false,
            down: false,
        },
    ]
}

/// Convert validated text into per-character atomic [`WinInputAction`] chunks.
///
/// Coalesces CRLF into a single `VK_RETURN`, maps LF/CR -> `VK_RETURN`, Tab ->
/// `VK_TAB`, BMP chars -> `[UnicodeDown, UnicodeUp]`, and non-BMP chars ->
/// `[HiDown, LoDown, HiUp, LoUp]` surrogate pairs.
fn encode_text_chunks<F>(text: &str, scan_fn: F) -> Result<Vec<Vec<WinInputAction>>, ViewError>
where
    F: Fn(u16) -> u16,
{
    if text.is_empty() || text.len() > 4096 {
        return Err(ViewError::InvalidRecord);
    }
    for ch in text.chars() {
        if (ch < ' ' && !matches!(ch, '\n' | '\r' | '\t')) || ch == '\u{007f}' {
            return Err(ViewError::Unsupported);
        }
    }

    let mut chunks = Vec::new();
    let mut chars = text.chars().peekable();
    let mut utf16 = [0u16; 2];

    while let Some(ch) = chars.next() {
        match ch {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    let _ = chars.next();
                }
                chunks.push(vk_click_chunk(VK_RETURN, scan_fn(VK_RETURN)));
            }
            '\n' => chunks.push(vk_click_chunk(VK_RETURN, scan_fn(VK_RETURN))),
            '\t' => chunks.push(vk_click_chunk(VK_TAB, scan_fn(VK_TAB))),
            _ => {
                let encoded = ch.encode_utf16(&mut utf16);
                if encoded.len() == 1 {
                    let unit = encoded[0];
                    chunks.push(vec![
                        WinInputAction::UnicodeKey { unit, down: true },
                        WinInputAction::UnicodeKey { unit, down: false },
                    ]);
                } else {
                    let (hi, lo) = (encoded[0], encoded[1]);
                    chunks.push(vec![
                        WinInputAction::UnicodeKey {
                            unit: hi,
                            down: true,
                        },
                        WinInputAction::UnicodeKey {
                            unit: lo,
                            down: true,
                        },
                        WinInputAction::UnicodeKey {
                            unit: hi,
                            down: false,
                        },
                        WinInputAction::UnicodeKey {
                            unit: lo,
                            down: false,
                        },
                    ]);
                }
            }
        }
    }
    Ok(chunks)
}

// ── WinCaptureDriver ──────────────────────────────────────────────────────────

/// Windows desktop capture driver backed by a [`WinCaptureApi`] implementation.
pub struct WinCaptureDriver<C: WinCaptureApi> {
    api: C,
    scope: CaptureScope,
}

impl<C: WinCaptureApi> WinCaptureDriver<C> {
    /// Construct a Windows capture driver bound to `scope`.
    #[must_use]
    pub const fn new(api: C, scope: CaptureScope) -> Self {
        Self { api, scope }
    }
}

impl<C: WinCaptureApi> CaptureDriver for WinCaptureDriver<C> {
    fn capture(&mut self, raw_buf: &mut Vec<u8>) -> Result<CapturedRawInfo, CaptureFailure> {
        let layout = self.api.query_layout()?;
        if !layout.input_desktop_accessible
            || layout.desktop_name.as_deref() != Some("Default")
        {
            return Err(CaptureFailure::Unavailable(Reason::Unavailable));
        }
        let geometry = select_capture_rect(
            &self.scope,
            Some(layout.virtual_rect),
            &layout.monitors,
            layout.display_token,
        )
        .map_err(|r| match r {
            Reason::FrameBudgetExceeded => CaptureFailure::LimitExceeded,
            Reason::Unsupported => CaptureFailure::Unsupported(r),
            other => CaptureFailure::Unavailable(other),
        })?;

        // Enforce raw-pixel cap strictly BEFORE allocating any GDI bitmap or raw buffer.
        geometry
            .rect
            .validate_for_capture()
            .map_err(|_| CaptureFailure::LimitExceeded)?;
        let pixel_count = usize::try_from(geometry.rect.width)
            .ok()
            .and_then(|w| usize::try_from(geometry.rect.height).ok().and_then(|h| w.checked_mul(h)))
            .filter(|&px| u64::try_from(px).is_ok_and(|p| p <= MAX_RAW_PIXELS))
            .ok_or(CaptureFailure::LimitExceeded)?;
        let stride_bytes = usize::try_from(geometry.rect.width)
            .ok()
            .and_then(|w| w.checked_mul(4))
            .ok_or(CaptureFailure::LimitExceeded)?;
        let expected_len = pixel_count
            .checked_mul(4)
            .ok_or(CaptureFailure::LimitExceeded)?;

        self.api.bitblt_bgra(geometry.rect, raw_buf)?;
        if raw_buf.len() < expected_len {
            return Err(CaptureFailure::Unavailable(Reason::Unavailable));
        }

        Ok(CapturedRawInfo {
            geometry,
            format: PixelFormat::Bgra32,
            stride_bytes,
        })
    }
}

// ── WinInputDriver ────────────────────────────────────────────────────────────

/// Windows live-view input driver with session-scoped held state and barrier-safe reset.
pub struct WinInputDriver<A: WinInputApi> {
    api: A,
    pressed_buttons: HashSet<WinMouseButton>,
    pressed_vks: Vec<u16>,
    transient_mod_vks: Vec<u16>,
}

impl<A: WinInputApi> WinInputDriver<A> {
    /// Wrap a [`WinInputApi`] implementation in a clean session tracker.
    #[must_use]
    pub fn new(api: A) -> Self {
        Self {
            api,
            pressed_buttons: HashSet::new(),
            pressed_vks: Vec::new(),
            transient_mod_vks: Vec::new(),
        }
    }

    fn key_vk_action(&self, vk: u16, down: bool) -> WinInputAction {
        WinInputAction::KeyVk {
            vk,
            scan: self.api.map_vk_to_scancode(vk),
            extended: is_win_extended_vk(vk),
            down,
        }
    }

    fn apply_transient_modifiers(&mut self, modifiers: &[Modifier]) -> Result<Vec<u16>, ViewError> {
        let mut to_press = Vec::new();
        for m in modifiers {
            let already_held = self.pressed_vks.iter().any(|&vk| modifier_matches_vk(m, vk))
                || self
                    .transient_mod_vks
                    .iter()
                    .any(|&vk| modifier_matches_vk(m, vk));
            if !already_held {
                let vk = modifier_to_vk(m);
                if !to_press.contains(&vk) {
                    to_press.push(vk);
                }
            }
        }
        if to_press.is_empty() {
            return Ok(Vec::new());
        }
        let actions: Vec<WinInputAction> =
            to_press.iter().map(|&vk| self.key_vk_action(vk, true)).collect();
        let sent = self.api.send_inputs(&actions)?;
        let mut added = Vec::with_capacity(sent);
        for &vk in to_press.iter().take(sent) {
            self.transient_mod_vks.push(vk);
            added.push(vk);
        }
        if sent == 0 {
            return Err(ViewError::Unavailable);
        }
        if sent < actions.len() {
            return Err(ViewError::PartialInjection);
        }
        Ok(added)
    }

    fn release_specific_transient(&mut self, vks: &[u16]) -> Result<(), ViewError> {
        if vks.is_empty() {
            return Ok(());
        }
        let actions: Vec<WinInputAction> = vks
            .iter()
            .copied()
            .rev()
            .map(|vk| self.key_vk_action(vk, false))
            .collect();
        let sent = self.api.send_inputs(&actions)?;
        for &vk in vks.iter().rev().take(sent) {
            if let Some(pos) = self.transient_mod_vks.iter().rposition(|&x| x == vk) {
                let _ = self.transient_mod_vks.remove(pos);
            }
        }
        if sent < actions.len() {
            return Err(ViewError::PartialInjection);
        }
        Ok(())
    }

    fn release_transient_modifiers(&mut self) -> Result<(), ViewError> {
        let pending: Vec<u16> = self.transient_mod_vks.clone();
        self.release_specific_transient(&pending)
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_pointer(
        &mut self,
        phase: &PointerPhase,
        norm_x: f64,
        norm_y: f64,
        button: Option<&Button>,
        buttons: u8,
        modifiers: &[Modifier],
        source_rect: &SourceRect,
        virtual_rect: PixelRect,
    ) -> Result<(), ViewError> {
        let (phys_x, phys_y) = map_normalized_point(norm_x, norm_y, source_rect);
        let (abs_x, abs_y) = point_to_virtual_absolute(phys_x, phys_y, virtual_rect);

        if matches!(phase, PointerPhase::Down) && !modifiers.is_empty() {
            let _ = self.apply_transient_modifiers(modifiers)?;
        }

        match phase {
            PointerPhase::Move => {
                let sent = self
                    .api
                    .send_inputs(&[WinInputAction::MouseMoveAbs { abs_x, abs_y }])?;
                if sent == 0 {
                    let _ = self.release_transient_modifiers();
                    return Err(ViewError::Unavailable);
                }
                Ok(())
            }
            PointerPhase::Down => {
                let win_btn = button.map(WinMouseButton::from).ok_or(ViewError::InvalidRecord)?;
                let mut actions = vec![WinInputAction::MouseMoveAbs { abs_x, abs_y }];
                let need_press = !self.pressed_buttons.contains(&win_btn);
                if need_press {
                    actions.push(WinInputAction::MouseButton {
                        button: win_btn,
                        down: true,
                    });
                }
                let sent = self.api.send_inputs(&actions)?;
                if sent == 0 {
                    let _ = self.release_transient_modifiers();
                    return Err(ViewError::Unavailable);
                }
                if need_press && sent >= 2 {
                    let _ = self.pressed_buttons.insert(win_btn);
                }
                if sent < actions.len() {
                    let _ = self.release_transient_modifiers();
                    return Err(ViewError::PartialInjection);
                }
                Ok(())
            }
            PointerPhase::Up => {
                let win_btn = button.map(WinMouseButton::from).ok_or(ViewError::InvalidRecord)?;
                let mut actions = vec![WinInputAction::MouseMoveAbs { abs_x, abs_y }];
                let was_pressed = self.pressed_buttons.contains(&win_btn);
                if was_pressed {
                    actions.push(WinInputAction::MouseButton {
                        button: win_btn,
                        down: false,
                    });
                }
                let sent = self.api.send_inputs(&actions)?;
                if was_pressed && sent >= 2 {
                    let _ = self.pressed_buttons.remove(&win_btn);
                }
                if buttons == 0 && self.pressed_buttons.is_empty() {
                    let _ = self.release_transient_modifiers();
                }
                if sent == 0 {
                    return Err(ViewError::Unavailable);
                }
                if sent < actions.len() {
                    return Err(ViewError::PartialInjection);
                }
                Ok(())
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_wheel(
        &mut self,
        norm_x: f64,
        norm_y: f64,
        delta_x: f64,
        delta_y: f64,
        modifiers: &[Modifier],
        source_rect: &SourceRect,
        virtual_rect: PixelRect,
    ) -> Result<(), ViewError> {
        let (phys_x, phys_y) = map_normalized_point(norm_x, norm_y, source_rect);
        let (abs_x, abs_y) = point_to_virtual_absolute(phys_x, phys_y, virtual_rect);
        // Win32 WHEEL: +delta scrolls up (away from user); DOM +deltaY scrolls down -> invert=true.
        // Win32 HWHEEL: +delta scrolls right; DOM +deltaX scrolls right -> invert=false.
        let v_delta = wheel_pixels_to_win32_delta(delta_y, true);
        let h_delta = wheel_pixels_to_win32_delta(delta_x, false);

        let added_mods = if modifiers.is_empty() {
            Vec::new()
        } else {
            self.apply_transient_modifiers(modifiers)?
        };

        let mut actions = vec![WinInputAction::MouseMoveAbs { abs_x, abs_y }];
        if v_delta != 0 {
            actions.push(WinInputAction::WheelVertical {
                abs_x,
                abs_y,
                delta: v_delta,
            });
        }
        if h_delta != 0 {
            actions.push(WinInputAction::WheelHorizontal {
                abs_x,
                abs_y,
                delta: h_delta,
            });
        }

        let sent = match self.api.send_inputs(&actions) {
            Ok(n) => n,
            Err(e) => {
                let _ = self.release_specific_transient(&added_mods);
                return Err(e);
            }
        };
        let rel_res = self.release_specific_transient(&added_mods);
        if sent == 0 {
            return Err(ViewError::Unavailable);
        }
        if sent < actions.len() {
            return Err(ViewError::PartialInjection);
        }
        rel_res
    }

    fn handle_key(
        &mut self,
        phase: &KeyPhase,
        code: &str,
        key: &str,
        _repeat: bool,
        modifiers: &[Modifier],
    ) -> Result<(), ViewError> {
        let vk = dom_code_to_vk(code, key)?;
        match phase {
            KeyPhase::Down => {
                if !modifiers.is_empty() {
                    let mod_only: Vec<Modifier> = modifiers
                        .iter()
                        .filter(|m| !modifier_matches_vk(m, vk))
                        .cloned()
                        .collect();
                    let _ = self.apply_transient_modifiers(&mod_only)?;
                }
                let action = self.key_vk_action(vk, true);
                let sent = self.api.send_inputs(&[action])?;
                if sent == 0 {
                    return Err(ViewError::Unavailable);
                }
                if !self.pressed_vks.contains(&vk) {
                    self.pressed_vks.push(vk);
                }
                Ok(())
            }
            KeyPhase::Up => {
                let mut res = Ok(());
                if let Some(pos) = self.pressed_vks.iter().rposition(|&x| x == vk) {
                    let action = self.key_vk_action(vk, false);
                    let sent = self.api.send_inputs(&[action])?;
                    if sent == 0 {
                        return Err(ViewError::Unavailable);
                    }
                    let _ = self.pressed_vks.remove(pos);
                }
                if self.pressed_vks.is_empty() {
                    res = self.release_transient_modifiers();
                }
                res
            }
        }
    }

    fn handle_text(&mut self, text: &str, cancelled: &AtomicBool) -> Result<(), ViewError> {
        let chunks = encode_text_chunks(text, |vk| self.api.map_vk_to_scancode(vk))?;
        let mut sent_any = false;
        for chunk in chunks {
            if cancelled.load(Ordering::Acquire) {
                return Err(if sent_any {
                    ViewError::PartialInjection
                } else {
                    ViewError::Unavailable
                });
            }
            let Ok(sent) = self.api.send_inputs(&chunk) else {
                return Err(if sent_any {
                    ViewError::PartialInjection
                } else {
                    ViewError::Unavailable
                });
            };
            if sent == 0 {
                return Err(if sent_any {
                    ViewError::PartialInjection
                } else {
                    ViewError::Unavailable
                });
            }
            if sent < chunk.len() {
                return Err(ViewError::PartialInjection);
            }
            sent_any = true;
        }
        Ok(())
    }
}

impl<A: WinInputApi> InputDriver for WinInputDriver<A> {
    fn inject(
        &mut self,
        event: &InputEvent,
        source_rect: SourceRect,
        _fallback_virtual: PixelRect,
        cancelled: &AtomicBool,
    ) -> Result<(), ViewError> {
        if cancelled.load(Ordering::Acquire) {
            return Err(ViewError::Unavailable);
        }
        if matches!(event, InputEvent::Reset) {
            return self.release_all();
        }
        // Always refresh live virtual desktop & check locked/Winlogon status before injecting.
        let live_virtual = self.api.check_input_desktop_ready()?;

        match event {
            InputEvent::Pointer {
                phase,
                x,
                y,
                button,
                buttons,
                modifiers,
            } => self.handle_pointer(
                phase,
                *x,
                *y,
                button.as_ref(),
                *buttons,
                modifiers,
                &source_rect,
                live_virtual,
            ),
            InputEvent::Wheel {
                x,
                y,
                delta_x,
                delta_y,
                modifiers,
            } => self.handle_wheel(
                *x,
                *y,
                *delta_x,
                *delta_y,
                modifiers,
                &source_rect,
                live_virtual,
            ),
            InputEvent::Key {
                phase,
                code,
                key,
                repeat,
                modifiers,
                ..
            } => self.handle_key(phase, code, key, *repeat, modifiers),
            InputEvent::Text { text } => self.handle_text(text, cancelled),
            InputEvent::Reset => self.release_all(),
        }
    }

    fn release_all(&mut self) -> Result<(), ViewError> {
        let mut actions = Vec::new();
        let btns: Vec<WinMouseButton> = self.pressed_buttons.iter().copied().collect();
        for b in &btns {
            actions.push(WinInputAction::MouseButton {
                button: *b,
                down: false,
            });
        }
        let vks: Vec<u16> = self.pressed_vks.iter().copied().rev().collect();
        for &vk in &vks {
            actions.push(self.key_vk_action(vk, false));
        }
        let tmods: Vec<u16> = self.transient_mod_vks.iter().copied().rev().collect();
        for &vk in &tmods {
            if !vks.contains(&vk) {
                actions.push(self.key_vk_action(vk, false));
            }
        }
        if actions.is_empty() {
            return Ok(());
        }
        let sent = self.api.send_inputs(&actions)?;
        // Clear internal tracking even on partial release? Keep remaining unreleased items so a
        // subsequent retry can still attempt release, removing only those confirmed sent.
        let mut idx = 0usize;
        for b in btns {
            if idx < sent {
                let _ = self.pressed_buttons.remove(&b);
            }
            idx += 1;
        }
        for vk in vks {
            if idx < sent
                && let Some(pos) = self.pressed_vks.iter().rposition(|&x| x == vk)
            {
                let _ = self.pressed_vks.remove(pos);
            }
            idx += 1;
        }
        for vk in tmods {
            if self.pressed_vks.contains(&vk) {
                continue;
            }
            if idx < sent
                && let Some(pos) = self.transient_mod_vks.iter().rposition(|&x| x == vk)
            {
                let _ = self.transient_mod_vks.remove(pos);
            }
            idx += 1;
        }
        if sent < actions.len() {
            Err(ViewError::Unavailable)
        } else {
            Ok(())
        }
    }
}

fn capture_failure_reason(err: CaptureFailure) -> Reason {
    match err {
        CaptureFailure::LimitExceeded => Reason::FrameBudgetExceeded,
        CaptureFailure::Unsupported(r)
        | CaptureFailure::Unavailable(r)
        | CaptureFailure::ConnectionLost(r) => r,
    }
}

/// Build a [`DesktopViewTarget`] from any Windows capture/input driver factories.
///
/// # Errors
///
/// Returns [`Reason`] if the initial display query or scope selection fails.
pub fn build_win_view_target<C, I>(
    options: LiveViewOptions,
    mut initial_capture: C,
    capture_factory: Arc<dyn Fn() -> Result<C, CaptureFailure> + Send + Sync>,
    probe: AvailabilityProbeFn,
    input_factory: Option<Arc<dyn Fn() -> Result<I, ViewError> + Send + Sync>>,
    input_reason: Option<Reason>,
) -> Result<Arc<DesktopViewTarget>, Reason>
where
    C: WinCaptureApi,
    I: WinInputApi,
{
    let layout = initial_capture
        .query_layout()
        .map_err(capture_failure_reason)?;
    if !layout.input_desktop_accessible || layout.desktop_name.as_deref() != Some("Default") {
        return Err(Reason::Unavailable);
    }
    let initial_geometry: GeometrySnapshot = select_capture_rect(
        &options.scope,
        Some(layout.virtual_rect),
        &layout.monitors,
        layout.display_token,
    )?;

    let cap_fn: CaptureDriverFactory = Arc::new(move |scope| {
        let api = capture_factory()?;
        Ok(Box::new(WinCaptureDriver::new(api, scope.clone())))
    });
    let in_fn: Option<InputDriverFactory> = input_factory.map(|f| {
        let f = Arc::clone(&f);
        Arc::new(move || {
            let api = f()?;
            Ok(Box::new(WinInputDriver::new(api)) as Box<dyn InputDriver>)
        }) as InputDriverFactory
    });

    Ok(Arc::new(DesktopViewTarget::new(DesktopViewTargetConfig {
        options,
        initial_geometry,
        capture_factory: cap_fn,
        probe,
        input_factory: in_fn,
        input_reason,
    })))
}

// ── Concrete Windows Win32 Implementation ─────────────────────────────────────

#[cfg(windows)]
#[allow(unsafe_code)]
mod windows_os {
    use std::ffi::c_void;
    use std::mem::{size_of, size_of_val};
    use std::sync::Arc;

    use desktop_api::live_view::{
        AvailabilityProbeFn, CaptureFailure, DesktopViewTarget, LiveViewOptions,
        MonitorDescriptor, PixelRect, select_capture_rect,
    };
    use live_view::{Reason, ViewError};
    use windows::{
        Win32::{
            Foundation::{HANDLE, HWND, LPARAM, RECT},
        Graphics::Gdi::{
            BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleBitmap,
            CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject, EnumDisplayMonitors,
            GetDC, GetDIBits, GetMonitorInfoW, HBITMAP, HDC, HGDIOBJ, HMONITOR, MONITORINFO,
            MONITORINFOEXW, ReleaseDC, SRCCOPY, SelectObject,
        },
        System::StationsAndDesktops::{
            CloseDesktop, DESKTOP_CONTROL_FLAGS, DESKTOP_SWITCHDESKTOP, GetUserObjectInformationW,
            HDESK, OpenInputDesktop, UOI_NAME,
        },
        UI::{
            HiDpi::{GetDpiForMonitor, GetDpiForSystem, MDT_EFFECTIVE_DPI},
            Input::KeyboardAndMouse::{
                INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT,
                KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MAPVK_VK_TO_VSC,
                MOUSE_EVENT_FLAGS, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN,
                MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE,
                MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK,
                MOUSEEVENTF_WHEEL, MOUSEINPUT, MapVirtualKeyW, SendInput, VIRTUAL_KEY,
            },
                WindowsAndMessaging::{
                    GetSystemMetrics, MONITORINFOF_PRIMARY, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
                    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
                },
            },
        },
        core::BOOL,
    };

    use super::{
        WinCaptureApi, WinDesktopLayout, WinInputAction, WinInputApi, WinMouseButton,
        build_win_view_target, hash_win_layout,
    };
    use crate::winlist;

    struct OwnedDesktop(HDESK);
    impl Drop for OwnedDesktop {
        fn drop(&mut self) {
            if !self.0.0.is_null() {
                // SAFETY: `self.0` was obtained from `OpenInputDesktop` and has not been closed yet.
                let _ = unsafe { CloseDesktop(self.0) };
            }
        }
    }

    struct OwnedScreenDc(HDC);
    impl Drop for OwnedScreenDc {
        fn drop(&mut self) {
            if !self.0.0.is_null() {
                // SAFETY: `self.0` came from `GetDC(None)` paired with `ReleaseDC(None, hdc)`.
                let _ = unsafe { ReleaseDC(Some(HWND::default()), self.0) };
            }
        }
    }

    struct OwnedMemDc(HDC);
    impl Drop for OwnedMemDc {
        fn drop(&mut self) {
            if !self.0.0.is_null() {
                // SAFETY: `self.0` came from `CreateCompatibleDC` and is unused once dropped.
                let _ = unsafe { DeleteDC(self.0) };
            }
        }
    }

    struct OwnedBitmap(HBITMAP);
    impl Drop for OwnedBitmap {
        fn drop(&mut self) {
            if !self.0.0.is_null() {
                // SAFETY: `self.0` came from `CreateCompatibleBitmap` and was deselected beforehand.
                let _ = unsafe { DeleteObject(HGDIOBJ(self.0.0)) };
            }
        }
    }

    struct OwnedSelect {
        hdc: HDC,
        prev: HGDIOBJ,
    }
    impl Drop for OwnedSelect {
        fn drop(&mut self) {
            if !self.hdc.0.is_null() && !self.prev.0.is_null() {
                // SAFETY: restores prior object into `self.hdc` before deleting our bitmap.
                let _ = unsafe { SelectObject(self.hdc, self.prev) };
            }
        }
    }

    fn query_input_desktop() -> (bool, Option<String>) {
        // SAFETY: queries desktop handle access without inheriting handles.
        let Ok(raw) = (unsafe {
            OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_SWITCHDESKTOP)
        }) else {
            return (false, None);
        };
        let guard = OwnedDesktop(raw);
        let mut buf = [0u16; 128];
        let mut needed: u32 = 0;
        let byte_len = u32::try_from(size_of_val(&buf)).unwrap_or(256);
        // SAFETY: `guard.0` is an open `HDESK` and `buf` holds up to `byte_len` bytes.
        let ok = unsafe {
            GetUserObjectInformationW(
                HANDLE(guard.0.0),
                UOI_NAME,
                Some(buf.as_mut_ptr().cast()),
                byte_len,
                Some(&raw mut needed),
            )
        };
        if ok.is_err() {
            return (false, None);
        }
        let words = (usize::try_from(needed).unwrap_or(0) / 2).min(buf.len());
        let name = String::from_utf16_lossy(&buf[..words])
            .trim_end_matches('\0')
            .to_string();
        let accessible = name.eq_ignore_ascii_case("Default");
        (accessible, Some(name))
    }

    fn live_virtual_desktop_rect() -> Option<PixelRect> {
        // SAFETY: plain `GetSystemMetrics` calls with constant metric indices.
        let (origin_x, origin_y, width_i, height_i) = unsafe {
            (
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN),
                GetSystemMetrics(SM_CYVIRTUALSCREEN),
            )
        };
        let width = u32::try_from(width_i).ok().filter(|&v| v > 0)?;
        let height = u32::try_from(height_i).ok().filter(|&v| v > 0)?;
        Some(PixelRect {
            x: origin_x,
            y: origin_y,
            width,
            height,
        })
    }

    unsafe extern "system" fn enum_monitor_proc(
        hmon: HMONITOR,
        _hdc: HDC,
        _rc: *mut RECT,
        data: LPARAM,
    ) -> BOOL {
        let monitors_ptr = data.0 as *mut Vec<MonitorDescriptor>;
        if monitors_ptr.is_null() {
            return BOOL(0);
        }
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = u32::try_from(size_of::<MONITORINFOEXW>()).unwrap_or(0);
        // SAFETY: `info` is stack-allocated with `cbSize = size_of::<MONITORINFOEXW>()`.
        let ok = unsafe { GetMonitorInfoW(hmon, (&raw mut info).cast::<MONITORINFO>()) };
        if !ok.as_bool() {
            return BOOL(1);
        }
        let r = info.monitorInfo.rcMonitor;
        let width = u32::try_from(r.right - r.left).unwrap_or(0);
        let height = u32::try_from(r.bottom - r.top).unwrap_or(0);
        if width == 0 || height == 0 {
            return BOOL(1);
        }

        let end = info
            .szDevice
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(info.szDevice.len());
        let mut name = String::from_utf16_lossy(&info.szDevice[..end]);
        let mut dpi_x = 0u32;
        let mut dpi_y = 0u32;
        // SAFETY: `hmon` is the live monitor handle passed by `EnumDisplayMonitors`.
        let dpi = if unsafe {
            GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &raw mut dpi_x, &raw mut dpi_y)
        }
        .is_ok()
            && dpi_x > 0
        {
            dpi_x
        } else {
            // SAFETY: constant Win32 query for system DPI fallback.
            unsafe { GetDpiForSystem() }.max(96)
        };
        let is_primary = (info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY) != 0;

        // SAFETY: `data.0` borrows a live `Vec<MonitorDescriptor>` on `enumerate_monitors`'s stack.
        let vec_ref = unsafe { &mut *monitors_ptr };
        if name.is_empty() {
            name = format!("DISPLAY{}", vec_ref.len() + 1);
        }
        vec_ref.push(MonitorDescriptor {
            name,
            is_primary,
            rect: PixelRect {
                x: r.left,
                y: r.top,
                width,
                height,
            },
            dpi,
        });
        BOOL(1)
    }

    fn enumerate_monitors() -> Vec<MonitorDescriptor> {
        let mut list: Vec<MonitorDescriptor> = Vec::new();
        let ptr = (&raw mut list) as isize;
        // SAFETY: `list` outlives the synchronous `EnumDisplayMonitors` enumeration.
        let _ = unsafe { EnumDisplayMonitors(None, None, Some(enum_monitor_proc), LPARAM(ptr)) };
        list
    }

    struct RealWinCapture;

    impl RealWinCapture {
        fn open() -> Self {
            winlist::ensure_dpi_aware();
            Self
        }
    }

    impl WinCaptureApi for RealWinCapture {
        fn query_layout(&mut self) -> Result<WinDesktopLayout, CaptureFailure> {
            winlist::ensure_dpi_aware();
            let (accessible, desktop_name) = query_input_desktop();
            if !accessible {
                return Err(CaptureFailure::Unavailable(Reason::Unavailable));
            }
            let virtual_rect = live_virtual_desktop_rect()
                .ok_or(CaptureFailure::Unavailable(Reason::Unavailable))?;
            let mut monitors = enumerate_monitors();
            if monitors.is_empty() {
                // SAFETY: constant Win32 query for system DPI fallback.
                let sys_dpi = unsafe { GetDpiForSystem() }.max(96);
                monitors.push(MonitorDescriptor {
                    name: "DISPLAY1".into(),
                    is_primary: true,
                    rect: virtual_rect,
                    dpi: sys_dpi,
                });
            }
            let display_token = hash_win_layout(virtual_rect, &monitors);
            Ok(WinDesktopLayout {
                monitors,
                virtual_rect,
                desktop_name,
                input_desktop_accessible: accessible,
                display_token,
            })
        }

        fn bitblt_bgra(
            &mut self,
            rect: PixelRect,
            out_bgra: &mut Vec<u8>,
        ) -> Result<(), CaptureFailure> {
            rect.validate_for_capture()
                .map_err(|_| CaptureFailure::LimitExceeded)?;
            let (accessible, _) = query_input_desktop();
            if !accessible {
                return Err(CaptureFailure::Unavailable(Reason::Unavailable));
            }

            let width_i =
                i32::try_from(rect.width).map_err(|_| CaptureFailure::LimitExceeded)?;
            let height_i =
                i32::try_from(rect.height).map_err(|_| CaptureFailure::LimitExceeded)?;

            // SAFETY: acquires desktop window DC; `OwnedScreenDc` releases it on every exit.
            let screen = OwnedScreenDc(unsafe { GetDC(Some(HWND::default())) });
            if screen.0.0.is_null() {
                return Err(CaptureFailure::Unavailable(Reason::Unavailable));
            }
            // SAFETY: compatible memory DC released by `OwnedMemDc::drop`.
            let mem = OwnedMemDc(unsafe { CreateCompatibleDC(Some(screen.0)) });
            if mem.0.0.is_null() {
                return Err(CaptureFailure::Unavailable(Reason::Unavailable));
            }
            // SAFETY: bitmap owned by `OwnedBitmap`, created after `validate_for_capture()`.
            let bmp = OwnedBitmap(unsafe { CreateCompatibleBitmap(screen.0, width_i, height_i) });
            if bmp.0.0.is_null() {
                return Err(CaptureFailure::Unavailable(Reason::Unavailable));
            }
            // SAFETY: selects `bmp` into `mem`; `select_guard` restores prior object on exit.
            let prev = unsafe { SelectObject(mem.0, HGDIOBJ(bmp.0.0)) };
            if prev.0.is_null() {
                return Err(CaptureFailure::Unavailable(Reason::Unavailable));
            }
            let select_guard = OwnedSelect { hdc: mem.0, prev };

            // SAFETY: `mem` holds `width_i x height_i` bitmap and `screen` covers `(rect.x, rect.y)`.
            unsafe {
                BitBlt(
                    mem.0,
                    0,
                    0,
                    width_i,
                    height_i,
                    Some(screen.0),
                    rect.x,
                    rect.y,
                    SRCCOPY | CAPTUREBLT,
                )
            }
            .map_err(|_| CaptureFailure::Unavailable(Reason::Unavailable))?;

            let byte_len = usize::try_from(rect.width)
                .ok()
                .and_then(|w| {
                    usize::try_from(rect.height)
                        .ok()
                        .and_then(|h| w.checked_mul(h).and_then(|px| px.checked_mul(4)))
                })
                .ok_or(CaptureFailure::LimitExceeded)?;
            out_bgra.resize(byte_len, 0);

            let mut info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: u32::try_from(size_of::<BITMAPINFOHEADER>()).unwrap_or(40),
                    biWidth: width_i,
                    biHeight: -height_i,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };

            // Microsoft `GetDIBits` docs require `bmp` to be deselected from `mem` first.
            drop(select_guard);
            // SAFETY: `out_bgra` has `byte_len == width * height * 4` writable bytes for 32-bpp top-down DIB.
            let lines = unsafe {
                GetDIBits(
                    mem.0,
                    bmp.0,
                    0,
                    rect.height,
                    Some(out_bgra.as_mut_ptr().cast::<c_void>()),
                    &raw mut info,
                    DIB_RGB_COLORS,
                )
            };
            if lines != height_i {
                return Err(CaptureFailure::Unavailable(Reason::Unavailable));
            }
            Ok(())
        }
    }

    struct RealWinInput;

    impl RealWinInput {
        fn open() -> Self {
            winlist::ensure_dpi_aware();
            Self
        }
    }

    fn mouse_input(dx: i32, dy: i32, mouse_data: u32, flags: MOUSE_EVENT_FLAGS) -> INPUT {
        INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx,
                    dy,
                    mouseData: mouse_data,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    fn keybd_input(vk: u16, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(vk),
                    wScan: scan,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    fn to_win32_input(action: WinInputAction) -> INPUT {
        match action {
            WinInputAction::MouseMoveAbs { abs_x, abs_y } => mouse_input(
                abs_x,
                abs_y,
                0,
                MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
            ),
            WinInputAction::MouseButton { button, down } => {
                let flags: MOUSE_EVENT_FLAGS = match (button, down) {
                    (WinMouseButton::Left, true) => MOUSEEVENTF_LEFTDOWN,
                    (WinMouseButton::Left, false) => MOUSEEVENTF_LEFTUP,
                    (WinMouseButton::Middle, true) => MOUSEEVENTF_MIDDLEDOWN,
                    (WinMouseButton::Middle, false) => MOUSEEVENTF_MIDDLEUP,
                    (WinMouseButton::Right, true) => MOUSEEVENTF_RIGHTDOWN,
                    (WinMouseButton::Right, false) => MOUSEEVENTF_RIGHTUP,
                };
                mouse_input(0, 0, 0, flags)
            }
            WinInputAction::WheelVertical {
                abs_x,
                abs_y,
                delta,
            } => mouse_input(abs_x, abs_y, delta.cast_unsigned(), MOUSEEVENTF_WHEEL),
            WinInputAction::WheelHorizontal {
                abs_x,
                abs_y,
                delta,
            } => mouse_input(abs_x, abs_y, delta.cast_unsigned(), MOUSEEVENTF_HWHEEL),
            WinInputAction::KeyVk {
                vk,
                scan,
                extended,
                down,
            } => {
                let mut flags = KEYBD_EVENT_FLAGS(0);
                if extended {
                    flags |= KEYEVENTF_EXTENDEDKEY;
                }
                if !down {
                    flags |= KEYEVENTF_KEYUP;
                }
                keybd_input(vk, scan, flags)
            }
            WinInputAction::UnicodeKey { unit, down } => keybd_input(
                0,
                unit,
                if down {
                    KEYEVENTF_UNICODE
                } else {
                    KEYEVENTF_UNICODE | KEYEVENTF_KEYUP
                },
            ),
        }
    }

    impl WinInputApi for RealWinInput {
        fn check_input_desktop_ready(&mut self) -> Result<PixelRect, ViewError> {
            winlist::ensure_dpi_aware();
            let (accessible, _) = query_input_desktop();
            if !accessible {
                return Err(ViewError::Unavailable);
            }
            live_virtual_desktop_rect().ok_or(ViewError::Unavailable)
        }

        fn map_vk_to_scancode(&self, vk: u16) -> u16 {
            // SAFETY: pure translation using constant map type; no pointers.
            let code = unsafe { MapVirtualKeyW(u32::from(vk), MAPVK_VK_TO_VSC) };
            u16::try_from(code & 0xFFFF).unwrap_or(0)
        }

        fn send_inputs(&mut self, actions: &[WinInputAction]) -> Result<usize, ViewError> {
            if actions.is_empty() {
                return Ok(0);
            }
            let inputs: Vec<INPUT> = actions.iter().copied().map(to_win32_input).collect();
            let cb_size = i32::try_from(size_of::<INPUT>()).map_err(|_| ViewError::InvalidRecord)?;
            // SAFETY: `inputs` is a contiguous slice of `INPUT` structs with matching `cbSize`.
            let sent = unsafe { SendInput(&inputs, cb_size) };
            Ok(usize::try_from(sent).unwrap_or(0))
        }
    }

    /// Open a Windows live desktop view target sharing the interactive desktop session.
    ///
    /// # Errors
    ///
    /// Returns [`Reason::Unavailable`] if the interactive `"Default"` desktop is locked,
    /// inaccessible (`Winlogon` / UAC), or has empty monitor geometry, or
    /// [`Reason::FrameBudgetExceeded`] when explicitly requested bounds exceed limits.
    pub fn open_windows_live_view_target(
        options: &LiveViewOptions,
    ) -> Result<Arc<DesktopViewTarget>, Reason> {
        type RealCaptureFactory =
            Arc<dyn Fn() -> Result<RealWinCapture, CaptureFailure> + Send + Sync>;
        type RealInputFactory = Arc<dyn Fn() -> Result<RealWinInput, ViewError> + Send + Sync>;

        winlist::ensure_dpi_aware();
        let mut init_cap = RealWinCapture::open();
        let _ = init_cap
            .query_layout()
            .map_err(super::capture_failure_reason)?;

        let capture_factory: RealCaptureFactory = Arc::new(|| Ok(RealWinCapture::open()));
        let probe: AvailabilityProbeFn = Arc::new(|scope| {
            let mut c = RealWinCapture::open();
            let layout = c.query_layout().map_err(super::capture_failure_reason)?;
            let _ = select_capture_rect(
                scope,
                Some(layout.virtual_rect),
                &layout.monitors,
                layout.display_token,
            )?;
            Ok(())
        });
        let input_factory: RealInputFactory = Arc::new(|| Ok(RealWinInput::open()));

        build_win_view_target(
            options.clone(),
            init_cap,
            capture_factory,
            probe,
            Some(input_factory),
            None,
        )
    }
}

#[cfg(windows)]
pub use windows_os::open_windows_live_view_target;

// ── Hermetic Unit Tests (Safe on All OSes, Zero Real OS Interaction) ──────────

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex as StdMutex};

    use desktop_api::live_view::{
        AvailabilityProbeFn, CaptureDriver, CaptureFailure, CaptureScope, DesktopViewTarget,
        InputDriver, LiveViewOptions, MonitorDescriptor, PixelRect, select_capture_rect,
    };
    use live_view::{
        Button, EffectiveProfile, InputEvent, KeyPhase, Modifier, PointerPhase, Reason, SourceRect,
        Units, ViewError, ViewTarget,
    };

    use super::{
        WinCaptureApi, WinCaptureDriver, WinDesktopLayout, WinInputAction, WinInputApi,
        WinInputDriver, WinMouseButton, build_win_view_target, dom_code_to_vk, hash_win_layout,
        is_win_extended_vk,
    };
    use std::sync::atomic::AtomicBool;

    struct MockWinCaptureState {
        layout: WinDesktopLayout,
        fill_rgb: [u8; 3],
        bitblt_calls: usize,
        fail_bitblt: bool,
    }

    #[derive(Clone)]
    struct MockWinCapture {
        state: Arc<StdMutex<MockWinCaptureState>>,
    }

    impl WinCaptureApi for MockWinCapture {
        fn query_layout(&mut self) -> Result<WinDesktopLayout, CaptureFailure> {
            let guard = self.state.lock().expect("lock");
            if !guard.layout.input_desktop_accessible
                || guard.layout.desktop_name.as_deref() != Some("Default")
            {
                return Err(CaptureFailure::Unavailable(Reason::Unavailable));
            }
            Ok(guard.layout.clone())
        }

        fn bitblt_bgra(
            &mut self,
            rect: PixelRect,
            out_bgra: &mut Vec<u8>,
        ) -> Result<(), CaptureFailure> {
            let mut guard = self.state.lock().expect("lock");
            guard.bitblt_calls += 1;
            if guard.fail_bitblt {
                return Err(CaptureFailure::Unavailable(Reason::Unavailable));
            }
            let len = usize::try_from(rect.width).unwrap()
                * usize::try_from(rect.height).unwrap()
                * 4;
            out_bgra.clear();
            out_bgra.resize(len, 255);
            let [r, g, b] = guard.fill_rgb;
            for (idx, px) in out_bgra.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let ramp = u8::try_from(idx % 64).unwrap_or(0);
                px[0] = b.wrapping_add(ramp);
                px[1] = g;
                px[2] = r;
                px[3] = 255;
            }
            Ok(())
        }
    }

    struct MockWinInputState {
        virtual_rect: PixelRect,
        desktop_ready: bool,
        sent: Vec<WinInputAction>,
        next_send_limit: Option<usize>,
        force_send_error: bool,
    }

    #[derive(Clone)]
    struct MockWinInput {
        state: Arc<StdMutex<MockWinInputState>>,
    }

    impl WinInputApi for MockWinInput {
        fn check_input_desktop_ready(&mut self) -> Result<PixelRect, ViewError> {
            let guard = self.state.lock().expect("lock");
            if !guard.desktop_ready {
                return Err(ViewError::Unavailable);
            }
            Ok(guard.virtual_rect)
        }

        fn map_vk_to_scancode(&self, vk: u16) -> u16 {
            match vk {
                0x0D => 0x1C,
                0x09 => 0x0F,
                0x27 => 0x4D,
                0x2E => 0x53,
                0x41 => 0x1E,
                0x43 => 0x2E,
                0x5B => 0x5B,
                0xA2 | 0xA3 => 0x1D,
                _ => 0x20,
            }
        }

        fn send_inputs(&mut self, actions: &[WinInputAction]) -> Result<usize, ViewError> {
            let mut guard = self.state.lock().expect("lock");
            if guard.force_send_error {
                return Err(ViewError::Unavailable);
            }
            let limit = guard.next_send_limit.take().unwrap_or(actions.len());
            let accepted = limit.min(actions.len());
            guard.sent.extend_from_slice(&actions[..accepted]);
            Ok(accepted)
        }
    }

    fn mixed_dpi_layout() -> WinDesktopLayout {
        let monitors = vec![
            MonitorDescriptor {
                name: r"\\.\DISPLAY2".into(),
                is_primary: false,
                rect: PixelRect {
                    x: -2560,
                    y: 0,
                    width: 2560,
                    height: 1440,
                },
                dpi: 144,
            },
            MonitorDescriptor {
                name: r"\\.\DISPLAY1".into(),
                is_primary: true,
                rect: PixelRect {
                    x: 0,
                    y: 0,
                    width: 1920,
                    height: 1080,
                },
                dpi: 96,
            },
        ];
        let virtual_rect = PixelRect {
            x: -2560,
            y: 0,
            width: 4480,
            height: 1440,
        };
        let display_token = hash_win_layout(virtual_rect, &monitors);
        WinDesktopLayout {
            monitors,
            virtual_rect,
            desktop_name: Some("Default".into()),
            input_desktop_accessible: true,
            display_token,
        }
    }

    fn build_mock_target(
        cap_state: &Arc<StdMutex<MockWinCaptureState>>,
        in_state: &Arc<StdMutex<MockWinInputState>>,
        options: LiveViewOptions,
    ) -> Result<Arc<DesktopViewTarget>, Reason> {
        let init_cap = MockWinCapture {
            state: Arc::clone(cap_state),
        };
        let cap_factory = {
            let cap_state = Arc::clone(cap_state);
            Arc::new(move || {
                Ok(MockWinCapture {
                    state: Arc::clone(&cap_state),
                })
            })
        };
        let probe: AvailabilityProbeFn = {
            let cap_state = Arc::clone(cap_state);
            Arc::new(move |scope| {
                let mut c = MockWinCapture {
                    state: Arc::clone(&cap_state),
                };
                let layout = c.query_layout().map_err(|_| Reason::Unavailable)?;
                let _ = select_capture_rect(
                    scope,
                    Some(layout.virtual_rect),
                    &layout.monitors,
                    layout.display_token,
                )?;
                Ok(())
            })
        };
        let in_factory = {
            let in_state = Arc::clone(in_state);
            Arc::new(move || {
                Ok(MockWinInput {
                    state: Arc::clone(&in_state),
                })
            })
        };
        build_win_view_target(
            options,
            init_cap,
            cap_factory,
            probe,
            Some(in_factory),
            None,
        )
    }

    #[test]
    fn mixed_dpi_and_negative_virtual_desktop_geometry_and_budget_cap() {
        let layout = mixed_dpi_layout();
        // Auto captures full 4480x1440 (6.45M px <= 16M) virtual desktop including negative X.
        let auto = select_capture_rect(
            &CaptureScope::Auto,
            Some(layout.virtual_rect),
            &layout.monitors,
            layout.display_token,
        )
        .expect("auto");
        assert_eq!(auto.rect.x, -2560);
        assert_eq!(auto.rect.width, 4480);

        // Selecting negative-origin 144-DPI monitor picks its exact origin and DPI.
        let sec = select_capture_rect(
            &CaptureScope::Monitor {
                selector: r"\\.\DISPLAY2".into(),
            },
            Some(layout.virtual_rect),
            &layout.monitors,
            layout.display_token,
        )
        .expect("secondary");
        assert_eq!(sec.rect.x, -2560);
        assert_eq!(sec.rect.width, 2560);
        assert_eq!(sec.dpi, 144);

        // Oversized triple-4K virtual desktop falls back to primary under Auto and rejects VirtualDesktop.
        let huge_monitors = vec![
            MonitorDescriptor {
                name: r"\\.\DISPLAY1".into(),
                is_primary: true,
                rect: PixelRect {
                    x: 0,
                    y: 0,
                    width: 3840,
                    height: 2160,
                },
                dpi: 192,
            },
            MonitorDescriptor {
                name: r"\\.\DISPLAY2".into(),
                is_primary: false,
                rect: PixelRect {
                    x: 3840,
                    y: 0,
                    width: 3840,
                    height: 2160,
                },
                dpi: 192,
            },
            MonitorDescriptor {
                name: r"\\.\DISPLAY3".into(),
                is_primary: false,
                rect: PixelRect {
                    x: 7680,
                    y: 0,
                    width: 3840,
                    height: 2160,
                },
                dpi: 192,
            },
        ];
        let huge_virtual = PixelRect {
            x: 0,
            y: 0,
            width: 11_520,
            height: 2160,
        };
        let tok = hash_win_layout(huge_virtual, &huge_monitors);
        let fallback = select_capture_rect(&CaptureScope::Auto, Some(huge_virtual), &huge_monitors, tok)
            .expect("fallback primary");
        assert_eq!(fallback.rect.width, 3840);
        assert_eq!(
            select_capture_rect(
                &CaptureScope::VirtualDesktop,
                Some(huge_virtual),
                &huge_monitors,
                tok
            ),
            Err(Reason::FrameBudgetExceeded)
        );
    }

    #[test]
    fn oversized_gdi_region_fails_before_calling_bitblt() {
        let cap_state = Arc::new(StdMutex::new(MockWinCaptureState {
            layout: WinDesktopLayout {
                monitors: vec![MonitorDescriptor {
                    name: r"\\.\DISPLAY1".into(),
                    is_primary: true,
                    rect: PixelRect {
                        x: 0,
                        y: 0,
                        width: 5000,
                        height: 4000,
                    },
                    dpi: 144,
                }],
                virtual_rect: PixelRect {
                    x: 0,
                    y: 0,
                    width: 5000,
                    height: 4000,
                },
                desktop_name: Some("Default".into()),
                input_desktop_accessible: true,
                display_token: 99,
            },
            fill_rgb: [10, 20, 30],
            bitblt_calls: 0,
            fail_bitblt: false,
        }));
        let mut driver = WinCaptureDriver::new(
            MockWinCapture {
                state: Arc::clone(&cap_state),
            },
            CaptureScope::VirtualDesktop,
        );
        let mut raw = Vec::new();
        let err = driver.capture(&mut raw).expect_err("must reject 20M px");
        assert_eq!(err, CaptureFailure::LimitExceeded);
        assert_eq!(cap_state.lock().expect("lock").bitblt_calls, 0);
        assert!(raw.capacity() == 0 && raw == [] as [u8; 0]);
    }

    #[tokio::test]
    async fn winlogon_lock_refuses_capture_with_zero_black_frames_and_topology_bumps_revision() {
        let tmp_dir = tempfile::tempdir().expect("tempdir");
        let before_entries = std::fs::read_dir(tmp_dir.path()).expect("read_dir").count();

        let cap_state = Arc::new(StdMutex::new(MockWinCaptureState {
            layout: mixed_dpi_layout(),
            fill_rgb: [40, 120, 220],
            bitblt_calls: 0,
            fail_bitblt: false,
        }));
        let in_state = Arc::new(StdMutex::new(MockWinInputState {
            virtual_rect: mixed_dpi_layout().virtual_rect,
            desktop_ready: true,
            sent: Vec::new(),
            next_send_limit: None,
            force_send_error: false,
        }));

        let target = build_mock_target(
            &cap_state,
            &in_state,
            LiveViewOptions::new("test-win-host"),
        )
        .expect("target");

        let source = target.frame_source();
        let mut sub = source
            .subscribe(EffectiveProfile::live())
            .await
            .expect("subscribe");

        let first = loop {
            sub.frames.changed().await.expect("frame1");
            if let Some(f) = sub.frames.borrow().clone() {
                break f;
            }
        };
        first.validate().expect("valid jpeg");
        assert_eq!(first.metadata.geometry_revision, 1);
        assert_eq!(first.metadata.source_rect.x, -2560.0);

        // Simulate display topology + DPI change on virtual desktop.
        {
            let mut g = cap_state.lock().expect("lock");
            g.layout.monitors[0].dpi = 192;
            g.layout.monitors[1].rect.width = 2560;
            g.layout.monitors[1].rect.height = 1440;
            g.layout.virtual_rect.width = 5120;
            g.layout.display_token = hash_win_layout(g.layout.virtual_rect, &g.layout.monitors);
            g.fill_rgb = [180, 60, 30];
        }

        let second = loop {
            sub.frames.changed().await.expect("frame2");
            if let Some(f) = sub.frames.borrow().clone()
                && f.metadata.geometry_revision >= 2
            {
                break f;
            }
        };
        assert!(second.metadata.geometry_revision >= 2);
        assert_eq!(second.metadata.source_rect.width, 5120);

        // Transition desktop to Winlogon / locked: must NOT publish black frame, must go Unavailable.
        {
            let mut g = cap_state.lock().expect("lock");
            g.layout.input_desktop_accessible = false;
            g.layout.desktop_name = Some("Winlogon".into());
        }
        // Poll target info after stopping subscriber.
        let _ = source.stop().await;
        drop(sub);

        let info_locked = target.info();
        assert!(!info_locked.capabilities.view);
        assert_eq!(info_locked.unavailable_reason, Some(Reason::Unavailable));
        assert_eq!(
            std::fs::read_dir(tmp_dir.path()).expect("read_dir").count(),
            before_entries
        );
    }

    fn mock_win_input(
        virtual_rect: PixelRect,
    ) -> (Arc<StdMutex<MockWinInputState>>, WinInputDriver<MockWinInput>) {
        let in_state = Arc::new(StdMutex::new(MockWinInputState {
            virtual_rect,
            desktop_ready: true,
            sent: Vec::new(),
            next_send_limit: None,
            force_send_error: false,
        }));
        let driver = WinInputDriver::new(MockWinInput {
            state: Arc::clone(&in_state),
        });
        (in_state, driver)
    }

    #[test]
    fn win_input_pointer_and_wheel_coords() {
        let virtual_rect = PixelRect {
            x: -2560,
            y: 0,
            width: 4480,
            height: 1440,
        };
        let (in_state, mut driver) = mock_win_input(virtual_rect);
        let cancel = AtomicBool::new(false);
        let mon2_source = SourceRect {
            x: -2560.0,
            y: 0.0,
            width: 2560,
            height: 1440,
            units: Units::PhysicalPx,
        };

        driver
            .inject(
                &InputEvent::Pointer {
                    phase: PointerPhase::Down,
                    x: 0.5,
                    y: 0.5,
                    button: Some(Button::Left),
                    buttons: 1,
                    modifiers: vec![Modifier::Ctrl],
                },
                mon2_source.clone(),
                virtual_rect,
                &cancel,
            )
            .expect("pointer down");
        driver
            .inject(
                &InputEvent::Wheel {
                    x: 0.5,
                    y: 0.5,
                    delta_x: 100.0,
                    delta_y: 100.0,
                    modifiers: vec![],
                },
                mon2_source,
                virtual_rect,
                &cancel,
            )
            .expect("wheel");

        let g = in_state.lock().expect("lock");
        assert!(g.sent.iter().any(|a| matches!(
            a,
            WinInputAction::MouseMoveAbs { abs_x, abs_y }
                if (18_000..=19_500).contains(abs_x) && (32_000..=33_500).contains(abs_y)
        )));
        assert!(
            g.sent
                .iter()
                .any(|a| matches!(a, WinInputAction::WheelVertical { delta, .. } if *delta == -120))
        );
        assert!(
            g.sent
                .iter()
                .any(|a| matches!(a, WinInputAction::WheelHorizontal { delta, .. } if *delta == 120))
        );
    }

    #[test]
    fn win_input_extended_keys_and_session_release() {
        let virtual_rect = PixelRect {
            x: -2560,
            y: 0,
            width: 4480,
            height: 1440,
        };
        let (in_state, mut driver) = mock_win_input(virtual_rect);
        let cancel = AtomicBool::new(false);
        let src = SourceRect {
            x: -2560.0,
            y: 0.0,
            width: 2560,
            height: 1440,
            units: Units::PhysicalPx,
        };

        assert!(is_win_extended_vk(0x27));
        assert!(is_win_extended_vk(0xA3));
        assert!(is_win_extended_vk(0xA5));
        assert!(!is_win_extended_vk(0x41));

        driver
            .inject(
                &InputEvent::Pointer {
                    phase: PointerPhase::Down,
                    x: 0.5,
                    y: 0.5,
                    button: Some(Button::Left),
                    buttons: 1,
                    modifiers: vec![Modifier::Ctrl],
                },
                src.clone(),
                virtual_rect,
                &cancel,
            )
            .expect("pointer down");
        driver
            .inject(
                &InputEvent::Key {
                    phase: KeyPhase::Down,
                    code: "ArrowRight".into(),
                    key: "ArrowRight".into(),
                    location: 0,
                    repeat: false,
                    modifiers: vec![],
                },
                src.clone(),
                virtual_rect,
                &cancel,
            )
            .expect("arrow right down");
        assert_eq!(dom_code_to_vk("KeyA", "Dead"), Err(ViewError::Unsupported));
        assert_eq!(
            driver.inject(
                &InputEvent::Key {
                    phase: KeyPhase::Down,
                    code: "KeyA".into(),
                    key: "Unidentified".into(),
                    location: 0,
                    repeat: false,
                    modifiers: vec![],
                },
                src,
                virtual_rect,
                &cancel,
            ),
            Err(ViewError::Unsupported)
        );
        assert!(in_state.lock().expect("lock").sent.iter().any(|a| matches!(
            a,
            WinInputAction::KeyVk { vk: 0x27, scan: 0x4D, extended: true, down: true }
        )));

        let before_rel = in_state.lock().expect("lock").sent.len();
        driver.release_all().expect("release all");
        let after = &in_state.lock().expect("lock").sent[before_rel..];
        assert!(after.contains(&WinInputAction::MouseButton { button: WinMouseButton::Left, down: false }));
        assert!(after.iter().any(|a| matches!(a, WinInputAction::KeyVk { vk: 0x27, down: false, .. })));
        assert!(after.iter().any(|a| matches!(a, WinInputAction::KeyVk { vk: 0xA2, down: false, .. })));
        assert_eq!(after.len(), 3);

        let len_after_rel = in_state.lock().expect("lock").sent.len();
        driver.release_all().expect("idempotent");
        assert_eq!(in_state.lock().expect("lock").sent.len(), len_after_rel);
    }

    #[test]
    fn win_input_turkish_unicode_surrogate_pairs_and_invalid_control_chars() {
        let virtual_rect = PixelRect {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let src = SourceRect {
            x: 0.0,
            y: 0.0,
            width: 1920,
            height: 1080,
            units: Units::PhysicalPx,
        };
        let in_state = Arc::new(StdMutex::new(MockWinInputState {
            virtual_rect,
            desktop_ready: true,
            sent: Vec::new(),
            next_send_limit: None,
            force_send_error: false,
        }));
        let mut driver = WinInputDriver::new(MockWinInput {
            state: Arc::clone(&in_state),
        });
        let cancel = AtomicBool::new(false);

        driver
            .inject(
                &InputEvent::Text {
                    text: "ğİŞç\r\n\t😀".into(),
                },
                src.clone(),
                virtual_rect,
                &cancel,
            )
            .expect("unicode text");

        let count_before_invalid = in_state.lock().expect("lock").sent.len();
        assert_eq!(
            driver.inject(
                &InputEvent::Text {
                    text: "ab\x07".into(),
                },
                src,
                virtual_rect,
                &cancel,
            ),
            Err(ViewError::Unsupported)
        );
        assert_eq!(in_state.lock().expect("lock").sent.len(), count_before_invalid);

        let g = in_state.lock().expect("lock");
        assert!(g.sent.iter().any(|a| matches!(
            a,
            WinInputAction::UnicodeKey { unit, down: true } if *unit == ('ğ' as u16)
        )));
        assert!(g.sent.iter().any(|a| matches!(
            a,
            WinInputAction::UnicodeKey { unit: 0xD83D, down: true }
        )));
        assert!(g.sent.iter().any(|a| matches!(
            a,
            WinInputAction::UnicodeKey { unit: 0xDE00, down: true }
        )));
        let enter_downs = g
            .sent
            .iter()
            .filter(|a| matches!(a, WinInputAction::KeyVk { vk: 0x0D, down: true, .. }))
            .count();
        assert_eq!(enter_downs, 1);
    }

    #[test]
    fn win_input_partial_injection_locked_desktop_and_cancel_mid_text() {
        let virtual_rect = PixelRect {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let src = SourceRect {
            x: 0.0,
            y: 0.0,
            width: 1920,
            height: 1080,
            units: Units::PhysicalPx,
        };
        let in_state = Arc::new(StdMutex::new(MockWinInputState {
            virtual_rect,
            desktop_ready: true,
            sent: Vec::new(),
            next_send_limit: Some(1),
            force_send_error: false,
        }));
        let mut driver = WinInputDriver::new(MockWinInput {
            state: Arc::clone(&in_state),
        });
        let cancel = AtomicBool::new(false);

        // Partial SendInput (move succeeded=1, click blocked=0) -> PartialInjection without marking Down held.
        let err = driver
            .inject(
                &InputEvent::Pointer {
                    phase: PointerPhase::Down,
                    x: 0.25,
                    y: 0.25,
                    button: Some(Button::Right),
                    buttons: 2,
                    modifiers: vec![],
                },
                src.clone(),
                virtual_rect,
                &cancel,
            )
            .expect_err("partial");
        assert_eq!(err, ViewError::PartialInjection);

        // Zero SendInput -> Unavailable.
        in_state.lock().expect("lock").next_send_limit = Some(0);
        assert_eq!(
            driver.inject(
                &InputEvent::Key {
                    phase: KeyPhase::Down,
                    code: "KeyB".into(),
                    key: "b".into(),
                    location: 0,
                    repeat: false,
                    modifiers: vec![],
                },
                src.clone(),
                virtual_rect,
                &cancel,
            ),
            Err(ViewError::Unavailable)
        );

        // Locked Winlogon desktop -> Unavailable without calling SendInput.
        let before = in_state.lock().expect("lock").sent.len();
        in_state.lock().expect("lock").desktop_ready = false;
        assert_eq!(
            driver.inject(
                &InputEvent::Text { text: "hi".into() },
                src.clone(),
                virtual_rect,
                &cancel,
            ),
            Err(ViewError::Unavailable)
        );
        assert_eq!(in_state.lock().expect("lock").sent.len(), before);

        // Cancel set before send -> Unavailable.
        in_state.lock().expect("lock").desktop_ready = true;
        cancel.store(true, std::sync::atomic::Ordering::Release);
        assert_eq!(
            driver.inject(
                &InputEvent::Text {
                    text: "hello".into()
                },
                src,
                virtual_rect,
                &cancel,
            ),
            Err(ViewError::Unavailable)
        );
    }

    /// Explicit interactive Windows desktop smoke test (moves real mouse / sends real GDI `BitBlt`).
    #[cfg(windows)]
    #[ignore = "requires an active interactive Windows desktop session; touches live cursor/screen"]
    #[tokio::test]
    async fn live_windows_session_smoke() {
        let target = super::open_windows_live_view_target(&LiveViewOptions::new("local-win"))
            .expect("open windows live view");
        let info = target.info();
        assert!(info.capabilities.view);
        assert!(info.capabilities.input);
        let source = target.frame_source();
        let mut sub = source
            .subscribe(EffectiveProfile::watch())
            .await
            .expect("subscribe");
        sub.frames.changed().await.expect("frame");
        let frame = sub.frames.borrow().clone().expect("latest");
        frame.validate().expect("valid frame");
        let _ = source.stop().await;
    }
}
