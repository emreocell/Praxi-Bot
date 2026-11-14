//! Linux X11 live-view adapter: persistent `XGetImage` capture, `RandR` monitor
//! geometry tracking, and cancel-safe `XTest` input injection.
//!
//! Wayland sessions, missing `DISPLAY`, and servers lacking `XTest` are
//! cleanly reported with explicit [`live_view::Reason`] codes instead of guessing
//! or invoking screenshots/AT-SPI/OCR tools per frame.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use desktop_api::DesktopError;
use desktop_api::live_view::{
    AvailabilityProbeFn, CaptureDriver, CaptureDriverFactory, CaptureFailure, CaptureScope,
    CapturedRawInfo, DesktopViewTarget, DesktopViewTargetConfig, InputDriver,
    InputDriverFactory, LiveViewOptions, MonitorDescriptor, PixelFormat, PixelRect,
    map_normalized_point, select_capture_rect, wheel_pixels_to_x11_steps,
};
use live_view::{
    Button, InputEvent, KeyPhase, Modifier, PointerPhase, Reason, SourceRect, ViewError,
};

use crate::pure::{char_to_keysym, detect_display_session};

// ── X11 capability & environment validation ───────────────────────────────────

/// Validate X11 display session environment variables for live view.
///
/// # Errors
///
/// Returns [`Reason::Unsupported`] on Wayland sessions and
/// [`Reason::Unavailable`] when no X11 `DISPLAY` is configured.
pub fn validate_x11_env(
    xdg_session_type: Option<&str>,
    wayland_display: Option<&str>,
    x11_display: Option<&str>,
) -> Result<String, Reason> {
    match detect_display_session(xdg_session_type, wayland_display, x11_display) {
        Ok(dpy) => Ok(dpy),
        Err(DesktopError::Unsupported(_)) => Err(Reason::Unsupported),
        Err(_) => Err(Reason::Unavailable),
    }
}

// ── Mockable X11 capture & input abstractions ─────────────────────────────────

/// Pixmap metadata queried from the X server setup before image allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct X11PixmapFormat {
    pub depth: u8,
    pub bits_per_pixel: u8,
    pub scanline_pad: u8,
    pub big_endian: bool,
}

/// Complete display topology returned by an X11 capture connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct X11DisplayLayout {
    pub root_rect: PixelRect,
    pub monitors: Vec<MonitorDescriptor>,
    pub format: X11PixmapFormat,
    pub display_token: u64,
}

/// Abstract X11 display connection used for screen capture and geometry queries.
pub trait X11CaptureConnection: Send + 'static {
    /// Query current root dimensions, `RandR` monitor geometry, and pixmap format.
    ///
    /// # Errors
    ///
    /// Returns [`CaptureFailure`] if the X server connection fails or returns unsupported metadata.
    fn query_layout(&mut self) -> Result<X11DisplayLayout, CaptureFailure>;

    /// Capture `rect` via `XGetImage` (`ZPixmap`) into `out_raw`, returning pixel format and stride.
    ///
    /// # Errors
    ///
    /// Returns [`CaptureFailure`] if capture fails or dimensions/format violate bounds.
    fn get_image(
        &mut self,
        rect: PixelRect,
        out_raw: &mut Vec<u8>,
    ) -> Result<(PixelFormat, usize), CaptureFailure>;
}

/// Snapshot of X11 keyboard mapping metadata for a display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct X11KeymapSnapshot {
    pub min_keycode: u8,
    pub max_keycode: u8,
    pub keysyms_per_keycode: u8,
    pub keysyms: Vec<u32>,
}

/// Abstract X11 connection used for `XTest` input injection and keymap updates.
pub trait X11InputConnection: Send + 'static {
    /// Check if `XTest` is present on this X11 display.
    ///
    /// # Errors
    ///
    /// Returns [`ViewError::Unavailable`] if the X11 connection cannot be queried.
    fn has_xtest(&mut self) -> Result<bool, ViewError>;

    /// Fetch the server's keyboard mapping table.
    ///
    /// # Errors
    ///
    /// Returns [`ViewError::Unavailable`] if querying X11 fails.
    fn keymap(&mut self) -> Result<X11KeymapSnapshot, ViewError>;

    /// Temporarily bind a single scratch keycode to `keysym` in both shift columns.
    ///
    /// # Errors
    ///
    /// Returns [`ViewError::Unavailable`] or [`ViewError::Unsupported`] on failure.
    fn bind_scratch_keycode(&mut self, keycode: u8, keysym: u32) -> Result<(), ViewError>;

    /// Unbind a previously allocated scratch keycode back to `NoSymbol`.
    ///
    /// # Errors
    ///
    /// Returns [`ViewError::Unavailable`] if the X11 connection fails.
    fn unbind_scratch_keycode(&mut self, keycode: u8) -> Result<(), ViewError>;

    /// Inject absolute pointer motion in root coordinates.
    ///
    /// # Errors
    ///
    /// Returns [`ViewError::Unavailable`] if `XTest` injection fails.
    fn motion(&mut self, x: i16, y: i16) -> Result<(), ViewError>;

    /// Inject an X11 mouse button press (`press = true`) or release (`false`).
    ///
    /// # Errors
    ///
    /// Returns [`ViewError::Unavailable`] if `XTest` injection fails.
    fn button(&mut self, button: u8, press: bool) -> Result<(), ViewError>;

    /// Inject an X11 key press (`press = true`) or release (`false`).
    ///
    /// # Errors
    ///
    /// Returns [`ViewError::Unavailable`] if `XTest` injection fails.
    fn key(&mut self, keycode: u8, press: bool) -> Result<(), ViewError>;
}

/// Map X11 depth/bpp/endianness into a supported [`PixelFormat`].
///
/// # Errors
///
/// Returns [`CaptureFailure::Unsupported`] for unsupported X11 visuals/depths.
pub const fn x11_pixel_format(fmt: X11PixmapFormat) -> Result<PixelFormat, CaptureFailure> {
    match (fmt.depth, fmt.bits_per_pixel) {
        (24 | 32, 32) => Ok(PixelFormat::Bgra32),
        (24 | 32, 24) => Ok(PixelFormat::Bgr24),
        (16, 16) => {
            if fmt.big_endian {
                Ok(PixelFormat::Rgb565Be)
            } else {
                Ok(PixelFormat::Rgb565Le)
            }
        }
        _ => Err(CaptureFailure::Unsupported(Reason::Unsupported)),
    }
}

/// Derive validated X11 `ZPixmap` row stride from raw reply length without panics.
///
/// # Errors
///
/// Returns [`CaptureFailure::LimitExceeded`] if dimensions or buffer length mismatch.
pub fn compute_x11_stride(
    raw_len: usize,
    width: u32,
    height: u32,
    fmt: PixelFormat,
) -> Result<usize, CaptureFailure> {
    let bpp = match fmt {
        PixelFormat::Bgra32 => 4usize,
        PixelFormat::Bgr24 => 3,
        PixelFormat::Rgb565Le | PixelFormat::Rgb565Be => 2,
    };
    let min_row = (width as usize)
        .checked_mul(bpp)
        .ok_or(CaptureFailure::LimitExceeded)?;
    if height == 0 {
        return Err(CaptureFailure::LimitExceeded);
    }
    let row_stride = raw_len / (height as usize);
    if row_stride < min_row {
        return Err(CaptureFailure::LimitExceeded);
    }
    let expected = row_stride
        .checked_mul(height as usize)
        .ok_or(CaptureFailure::LimitExceeded)?;
    if raw_len < expected {
        return Err(CaptureFailure::LimitExceeded);
    }
    Ok(row_stride)
}

// ── Generic X11 CaptureDriver ─────────────────────────────────────────────────

/// Persistent X11 capture driver generic over [`X11CaptureConnection`].
pub struct X11CaptureDriver<C: X11CaptureConnection> {
    conn: C,
    scope: CaptureScope,
}

impl<C: X11CaptureConnection> X11CaptureDriver<C> {
    /// Wrap a connected X11 capture connection and requested scope.
    #[must_use]
    pub const fn new(conn: C, scope: CaptureScope) -> Self {
        Self { conn, scope }
    }
}

impl<C: X11CaptureConnection> CaptureDriver for X11CaptureDriver<C> {
    fn capture(&mut self, raw_buf: &mut Vec<u8>) -> Result<CapturedRawInfo, CaptureFailure> {
        let layout = self.conn.query_layout()?;
        let _ = x11_pixel_format(layout.format)?;
        let geom = select_capture_rect(
            &self.scope,
            Some(layout.root_rect),
            &layout.monitors,
            layout.display_token,
        )
        .map_err(|r| {
            if r == Reason::FrameBudgetExceeded {
                CaptureFailure::LimitExceeded
            } else {
                CaptureFailure::Unavailable(r)
            }
        })?;
        // Always enforce 16 Mpixel raw limit BEFORE requesting image bytes from X11.
        geom.rect
            .validate_for_capture()
            .map_err(|_| CaptureFailure::LimitExceeded)?;
        let (pixel_format, stride_bytes) = self.conn.get_image(geom.rect, raw_buf)?;
        Ok(CapturedRawInfo {
            geometry: geom,
            format: pixel_format,
            stride_bytes,
        })
    }
}

// ── Generic X11 InputDriver ───────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
struct PressedKeyRecord {
    keycode: u8,
    modifier: Option<Modifier>,
    scratch: bool,
}

/// Persistent X11 `XTest` input driver tracking session-owned pressed keys,
/// mouse buttons, and temporary scratch keycodes.
pub struct X11InputDriver<C: X11InputConnection> {
    conn: C,
    pressed_buttons: BTreeSet<u8>,
    pressed_keys: BTreeMap<String, PressedKeyRecord>,
    transient_mods: Vec<(Modifier, u8)>,
    scratch_bindings: BTreeMap<u8, u32>,
}

impl<C: X11InputConnection> X11InputDriver<C> {
    /// Verify `XTest` support on `conn` and construct a session-tracked input driver.
    ///
    /// # Errors
    ///
    /// Returns [`ViewError::Unsupported`] if the `XTest` extension is missing.
    pub fn new(mut conn: C) -> Result<Self, ViewError> {
        if !conn.has_xtest()? {
            return Err(ViewError::Unsupported);
        }
        Ok(Self {
            conn,
            pressed_buttons: BTreeSet::new(),
            pressed_keys: BTreeMap::new(),
            transient_mods: Vec::new(),
            scratch_bindings: BTreeMap::new(),
        })
    }

    fn is_shift_held(&self) -> bool {
        self.pressed_keys
            .values()
            .any(|r| r.modifier.as_ref() == Some(&Modifier::Shift))
            || self
                .transient_mods
                .iter()
                .any(|(m, _)| *m == Modifier::Shift)
    }

    fn has_chord_modifier(&self, event_mods: &[Modifier]) -> bool {
        let has_non_shift = |m: &Modifier| !matches!(m, Modifier::Shift);
        event_mods.iter().any(has_non_shift)
            || self
                .pressed_keys
                .values()
                .filter_map(|r| r.modifier.as_ref())
                .any(has_non_shift)
            || self.transient_mods.iter().map(|(m, _)| m).any(has_non_shift)
    }

    fn resolve_keycode(&mut self, keysym: u32) -> Result<(u8, bool), ViewError> {
        if keysym == 0 {
            return Err(ViewError::Unsupported);
        }
        let km = self.conn.keymap()?;
        let per = usize::from(km.keysyms_per_keycode.max(1));
        let col = usize::from(self.is_shift_held());
        let mut unused_kc: Option<u8> = None;

        for (idx, chunk) in km.keysyms.chunks(per).enumerate() {
            let Ok(offset) = u8::try_from(idx) else {
                break;
            };
            let kc = km.min_keycode.saturating_add(offset);
            if kc > km.max_keycode {
                break;
            }
            if !self.scratch_bindings.contains_key(&kc) {
                let effective = chunk
                    .get(col)
                    .copied()
                    .filter(|&sym| sym != 0)
                    .or_else(|| (chunk.len() == 1).then(|| chunk[0]).filter(|&s| s != 0));
                if effective == Some(keysym) {
                    return Ok((kc, false));
                }
            }
            let pressed_now = self.pressed_keys.values().any(|r| r.keycode == kc)
                || self.transient_mods.iter().any(|&(_, m_kc)| m_kc == kc);
            if unused_kc.is_none()
                && kc != 8
                && !pressed_now
                && !self.scratch_bindings.contains_key(&kc)
                && chunk.iter().all(|&s| s == 0)
            {
                unused_kc = Some(kc);
            }
        }

        // Do not clobber already bound keycodes when the server keymap is full.
        let scratch = unused_kc.ok_or(ViewError::Unsupported)?;
        self.conn.bind_scratch_keycode(scratch, keysym)?;
        self.scratch_bindings.insert(scratch, keysym);
        Ok((scratch, true))
    }

    fn unbind_if_unused(&mut self, kc: u8) {
        if !self.scratch_bindings.contains_key(&kc) {
            return;
        }
        let still_held = self.pressed_keys.values().any(|r| r.keycode == kc)
            || self.transient_mods.iter().any(|&(_, mkc)| mkc == kc);
        if !still_held {
            let _ = self.conn.unbind_scratch_keycode(kc);
            self.scratch_bindings.remove(&kc);
        }
    }

    fn apply_transient_modifiers(&mut self, mods: &[Modifier]) -> Result<(), ViewError> {
        for m in mods {
            let already = self
                .pressed_keys
                .values()
                .any(|r| r.modifier.as_ref() == Some(m))
                || self.transient_mods.iter().any(|(tm, _)| tm == m);
            if already {
                continue;
            }
            let sym = modifier_to_x11_keysym(m);
            let (kc, _scratch) = self.resolve_keycode(sym)?;
            if self.conn.key(kc, true).is_err() {
                let _ = self.release_transient_modifiers();
                return Err(ViewError::PartialInjection);
            }
            self.transient_mods.push((m.clone(), kc));
        }
        Ok(())
    }

    fn release_transient_modifiers(&mut self) -> Result<(), ViewError> {
        let mut had_err = false;
        while let Some((_m, kc)) = self.transient_mods.pop() {
            if self.conn.key(kc, false).is_err() {
                had_err = true;
            }
            self.unbind_if_unused(kc);
        }
        if had_err {
            Err(ViewError::PartialInjection)
        } else {
            Ok(())
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_pointer(
        &mut self,
        phase: &PointerPhase,
        x: f64,
        y: f64,
        button: Option<&Button>,
        buttons: u8,
        modifiers: &[Modifier],
        source_rect: &SourceRect,
    ) -> Result<(), ViewError> {
        let (px, py) = map_normalized_point(x, y, source_rect);
        // Physical coordinates are finite; clamp cleanly to X11 `i16` range.
        #[allow(clippy::cast_possible_truncation)]
        let rx = px.round().clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16;
        #[allow(clippy::cast_possible_truncation)]
        let ry = py.round().clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16;

        if matches!(phase, PointerPhase::Down) && !modifiers.is_empty() {
            self.apply_transient_modifiers(modifiers)?;
        }

        if self.conn.motion(rx, ry).is_err() {
            let _ = self.release_transient_modifiers();
            return Err(ViewError::Unavailable);
        }

        match phase {
            PointerPhase::Move => Ok(()),
            PointerPhase::Down => {
                let btn = button.map(x11_button_code).ok_or(ViewError::InvalidRecord)?;
                if self.pressed_buttons.insert(btn) && self.conn.button(btn, true).is_err() {
                    self.pressed_buttons.remove(&btn);
                    return Err(ViewError::PartialInjection);
                }
                Ok(())
            }
            PointerPhase::Up => {
                let btn = button.map(x11_button_code).ok_or(ViewError::InvalidRecord)?;
                let res = if self.pressed_buttons.remove(&btn) {
                    self.conn
                        .button(btn, false)
                        .map_err(|_| ViewError::PartialInjection)
                } else {
                    // Preserve physical mouse state if we did not press this button.
                    Ok(())
                };
                if buttons == 0 && self.pressed_buttons.is_empty() {
                    let _ = self.release_transient_modifiers();
                }
                res
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_wheel(
        &mut self,
        x: f64,
        y: f64,
        delta_x: f64,
        delta_y: f64,
        modifiers: &[Modifier],
        source_rect: &SourceRect,
        cancelled: &AtomicBool,
    ) -> Result<(), ViewError> {
        let (px, py) = map_normalized_point(x, y, source_rect);
        #[allow(clippy::cast_possible_truncation)]
        let rx = px.round().clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16;
        #[allow(clippy::cast_possible_truncation)]
        let ry = py.round().clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16;

        if !modifiers.is_empty() {
            self.apply_transient_modifiers(modifiers)?;
        }
        if self.conn.motion(rx, ry).is_err() {
            let _ = self.release_transient_modifiers();
            return Err(ViewError::Unavailable);
        }

        let mut sent_any = false;
        let v_steps = wheel_pixels_to_x11_steps(delta_y);
        let v_btn = if delta_y > 0.0 { 5 } else { 4 };
        for _ in 0..v_steps {
            if cancelled.load(Ordering::Acquire) {
                let _ = self.release_transient_modifiers();
                return Err(if sent_any {
                    ViewError::PartialInjection
                } else {
                    ViewError::Unavailable
                });
            }
            if self.conn.button(v_btn, true).is_err() || self.conn.button(v_btn, false).is_err() {
                let _ = self.release_transient_modifiers();
                return Err(ViewError::PartialInjection);
            }
            sent_any = true;
        }

        let h_steps = wheel_pixels_to_x11_steps(delta_x);
        let h_btn = if delta_x > 0.0 { 7 } else { 6 };
        for _ in 0..h_steps {
            if cancelled.load(Ordering::Acquire) {
                let _ = self.release_transient_modifiers();
                return Err(if sent_any {
                    ViewError::PartialInjection
                } else {
                    ViewError::Unavailable
                });
            }
            if self.conn.button(h_btn, true).is_err() || self.conn.button(h_btn, false).is_err() {
                let _ = self.release_transient_modifiers();
                return Err(ViewError::PartialInjection);
            }
            sent_any = true;
        }

        self.release_transient_modifiers()
    }

    fn handle_key(
        &mut self,
        phase: &KeyPhase,
        code: &str,
        key: &str,
        repeat: bool,
        modifiers: &[Modifier],
    ) -> Result<(), ViewError> {
        match phase {
            KeyPhase::Down => {
                if let Some(existing) = self.pressed_keys.get(code) {
                    let kc = existing.keycode;
                    if repeat {
                        return self
                            .conn
                            .key(kc, true)
                            .map_err(|_| ViewError::PartialInjection);
                    }
                    return Ok(());
                }
                let chord = self.has_chord_modifier(modifiers);
                let (sym, mod_tag) = dom_key_to_x11_keysym(code, key, chord)?;
                let (kc, scratch) = self.resolve_keycode(sym)?;
                if self.conn.key(kc, true).is_err() {
                    self.unbind_if_unused(kc);
                    return Err(ViewError::Unavailable);
                }
                self.pressed_keys.insert(
                    code.to_string(),
                    PressedKeyRecord {
                        keycode: kc,
                        modifier: mod_tag,
                        scratch,
                    },
                );
                Ok(())
            }
            KeyPhase::Up => {
                let Some(rec) = self.pressed_keys.remove(code) else {
                    // Only release keys originally pressed by our session.
                    return Ok(());
                };
                let rel = self.conn.key(rec.keycode, false);
                if rec.scratch {
                    self.unbind_if_unused(rec.keycode);
                }
                rel.map_err(|_| ViewError::PartialInjection)
            }
        }
    }

    fn handle_text(&mut self, text: &str, cancelled: &AtomicBool) -> Result<(), ViewError> {
        if text.is_empty() || text.len() > 4096 {
            return Err(ViewError::InvalidRecord);
        }
        // Validate all characters upfront before sending any X11 events.
        let mut syms = Vec::with_capacity(text.chars().count());
        for ch in text.chars() {
            if ch < ' ' && !matches!(ch, '\n' | '\r' | '\t') || ch == '\u{007f}' {
                return Err(ViewError::Unsupported);
            }
            syms.push(char_to_keysym(ch));
        }

        let mut sent_any = false;
        for sym in syms {
            if cancelled.load(Ordering::Acquire) {
                return Err(if sent_any {
                    ViewError::PartialInjection
                } else {
                    ViewError::Unavailable
                });
            }
            let (kc, scratch) = match self.resolve_keycode(sym) {
                Ok(v) => v,
                Err(e) => {
                    return Err(if sent_any {
                        ViewError::PartialInjection
                    } else {
                        e
                    });
                }
            };
            let down_ok = self.conn.key(kc, true).is_ok();
            let up_ok = if down_ok {
                self.conn.key(kc, false).is_ok()
            } else {
                false
            };
            if scratch {
                self.unbind_if_unused(kc);
            }
            if !down_ok || !up_ok {
                return Err(if sent_any || down_ok {
                    ViewError::PartialInjection
                } else {
                    ViewError::Unavailable
                });
            }
            sent_any = true;
        }
        Ok(())
    }
}

impl<C: X11InputConnection> InputDriver for X11InputDriver<C> {
    fn inject(
        &mut self,
        event: &InputEvent,
        source_rect: SourceRect,
        _virtual_rect: PixelRect,
        cancelled: &AtomicBool,
    ) -> Result<(), ViewError> {
        if cancelled.load(Ordering::Acquire) {
            return Err(ViewError::Unavailable);
        }
        match event {
            InputEvent::Pointer {
                phase,
                x,
                y,
                button,
                buttons,
                modifiers,
            } => self.handle_pointer(phase, *x, *y, button.as_ref(), *buttons, modifiers, &source_rect),
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
                cancelled,
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
        let mut failed = false;

        let keys: Vec<(String, PressedKeyRecord)> = self
            .pressed_keys
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        self.pressed_keys.clear();
        for (_code, rec) in keys.into_iter().rev() {
            if self.conn.key(rec.keycode, false).is_err() {
                failed = true;
            }
        }

        if self.release_transient_modifiers().is_err() {
            failed = true;
        }

        let buttons: Vec<u8> = self.pressed_buttons.iter().copied().collect();
        self.pressed_buttons.clear();
        for btn in buttons {
            if self.conn.button(btn, false).is_err() {
                failed = true;
            }
        }

        let scratches: Vec<u8> = self.scratch_bindings.keys().copied().collect();
        self.scratch_bindings.clear();
        for kc in scratches {
            if self.conn.unbind_scratch_keycode(kc).is_err() {
                failed = true;
            }
        }

        if failed {
            Err(ViewError::Unavailable)
        } else {
            Ok(())
        }
    }
}

// ── DOM KeyboardEvent -> X11 Keysym mapping ───────────────────────────────────

const fn x11_button_code(b: &Button) -> u8 {
    match b {
        Button::Left => 1,
        Button::Middle => 2,
        Button::Right => 3,
    }
}

const fn modifier_to_x11_keysym(m: &Modifier) -> u32 {
    match m {
        Modifier::Shift => 0xFFE1,   // XK_Shift_L
        Modifier::Ctrl => 0xFFE3,    // XK_Control_L
        Modifier::Alt => 0xFFE9,     // XK_Alt_L
        Modifier::Meta => 0xFFEB,    // XK_Super_L
    }
}

/// Resolve DOM `(code, key)` plus active chord modifier status to `(keysym, modifier_tag)`.
///
/// Explicitly rejects `Dead`/`Unidentified`/`Process`/`Compose` or unmappable
/// inputs with [`ViewError::Unsupported`] rather than typing the wrong character.
///
/// # Errors
///
/// Returns [`ViewError::Unsupported`] if the key name or layout mapping is unsupported.
pub fn dom_key_to_x11_keysym(
    code: &str,
    key: &str,
    chord_modifier_active: bool,
) -> Result<(u32, Option<Modifier>), ViewError> {
    if matches!(key, "Dead" | "Unidentified" | "Process" | "Compose") {
        return Err(ViewError::Unsupported);
    }

    let fixed = match code {
        "ShiftLeft" => Some((0xFFE1, Some(Modifier::Shift))),
        "ShiftRight" => Some((0xFFE2, Some(Modifier::Shift))),
        "ControlLeft" => Some((0xFFE3, Some(Modifier::Ctrl))),
        "ControlRight" => Some((0xFFE4, Some(Modifier::Ctrl))),
        "AltLeft" => Some((0xFFE9, Some(Modifier::Alt))),
        "AltRight" => Some((0xFFEA, Some(Modifier::Alt))),
        "MetaLeft" => Some((0xFFEB, Some(Modifier::Meta))),
        "MetaRight" => Some((0xFFEC, Some(Modifier::Meta))),
        "CapsLock" => Some((0xFFE5, None)),
        "Enter" => Some((0xFF0D, None)),
        "Tab" => Some((0xFF09, None)),
        "Escape" => Some((0xFF1B, None)),
        "Backspace" => Some((0xFF08, None)),
        "Delete" => Some((0xFFFF, None)),
        "Space" => Some((0x0020, None)),
        "ArrowLeft" => Some((0xFF51, None)),
        "ArrowUp" => Some((0xFF52, None)),
        "ArrowRight" => Some((0xFF53, None)),
        "ArrowDown" => Some((0xFF54, None)),
        "Home" => Some((0xFF50, None)),
        "End" => Some((0xFF57, None)),
        "PageUp" => Some((0xFF55, None)),
        "PageDown" => Some((0xFF56, None)),
        _ => None,
    };
    if let Some(pair) = fixed {
        return Ok(pair);
    }

    if let Some(n) = code.strip_prefix('F').and_then(|s| s.parse::<u32>().ok())
        && (1..=12).contains(&n)
    {
        return Ok((0xFFBE + n - 1, None));
    }

    if chord_modifier_active {
        let mut chars = key.chars();
        if let (Some(ch), None) = (chars.next(), chars.next()) {
            if ch.is_ascii_graphic() {
                return Ok((char_to_keysym(ch.to_ascii_lowercase()), None));
            }
            if !ch.is_ascii_control() {
                return Err(ViewError::Unsupported);
            }
        }
        return canonical_us_code_keysym(code).map(|s| (s, None));
    }

    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        (Some(ch), None) if ch >= ' ' && ch != '\u{007f}' => Ok((char_to_keysym(ch), None)),
        _ => Err(ViewError::Unsupported),
    }
}

fn canonical_us_code_keysym(code: &str) -> Result<u32, ViewError> {
    if let Some(rest) = code.strip_prefix("Key")
        && rest.len() == 1
        && rest.as_bytes()[0].is_ascii_uppercase()
    {
        return Ok(u32::from(rest.as_bytes()[0].to_ascii_lowercase()));
    }
    if let Some(rest) = code.strip_prefix("Digit")
        && rest.len() == 1
        && rest.as_bytes()[0].is_ascii_digit()
    {
        return Ok(u32::from(rest.as_bytes()[0]));
    }
    match code {
        "Minus" => Ok(u32::from(b'-')),
        "Equal" => Ok(u32::from(b'=')),
        "BracketLeft" => Ok(u32::from(b'[')),
        "BracketRight" => Ok(u32::from(b']')),
        "Backslash" => Ok(u32::from(b'\\')),
        "Semicolon" => Ok(u32::from(b';')),
        "Quote" => Ok(u32::from(b'\'')),
        "Backquote" => Ok(u32::from(b'`')),
        "Comma" => Ok(u32::from(b',')),
        "Period" => Ok(u32::from(b'.')),
        "Slash" => Ok(u32::from(b'/')),
        _ => Err(ViewError::Unsupported),
    }
}

// ── Constructor helper (used by real X11 and hermetic mocks) ──────────────────

/// Build a [`DesktopViewTarget`] from X11 capture/input driver factories.
///
/// # Errors
///
/// Fails with [`Reason::Unavailable`] or [`Reason::FrameBudgetExceeded`] if the
/// initial geometry query / scope selection fails.
pub fn build_x11_view_target<C, I>(
    options: &LiveViewOptions,
    mut initial_probe_conn: C,
    capture_factory: Arc<dyn Fn() -> Result<C, CaptureFailure> + Send + Sync>,
    probe: AvailabilityProbeFn,
    input_factory: Option<Arc<dyn Fn() -> Result<I, ViewError> + Send + Sync>>,
    input_reason: Option<Reason>,
) -> Result<Arc<DesktopViewTarget>, Reason>
where
    C: X11CaptureConnection,
    I: X11InputConnection,
{
    let layout = initial_probe_conn
        .query_layout()
        .map_err(|_| Reason::Unavailable)?;
    let initial_geometry = select_capture_rect(
        &options.scope,
        Some(layout.root_rect),
        &layout.monitors,
        layout.display_token,
    )?;

    let wrapped_capture: CaptureDriverFactory = Arc::new(move |scope| {
        let conn = capture_factory()?;
        Ok(Box::new(X11CaptureDriver::new(conn, scope.clone())))
    });

    let wrapped_input: Option<InputDriverFactory> = input_factory.map(|f| {
        Arc::new(move || -> Result<Box<dyn InputDriver>, ViewError> {
            let conn = f()?;
            Ok(Box::new(X11InputDriver::new(conn)?))
        }) as InputDriverFactory
    });

    Ok(Arc::new(DesktopViewTarget::new(DesktopViewTargetConfig {
        options: options.clone(),
        initial_geometry,
        capture_factory: wrapped_capture,
        probe,
        input_factory: wrapped_input,
        input_reason,
    })))
}

// ── Concrete Linux x11rb implementation ───────────────────────────────────────

#[cfg(target_os = "linux")]
mod linux_x11 {
    use std::sync::Arc;

    use super::{
        AvailabilityProbeFn, CaptureFailure, DesktopViewTarget, LiveViewOptions, MonitorDescriptor,
        PixelFormat, PixelRect, Reason, ViewError, X11CaptureConnection, X11DisplayLayout,
        X11InputConnection, X11KeymapSnapshot, X11PixmapFormat, build_x11_view_target,
        compute_x11_stride, select_capture_rect, validate_x11_env, x11_pixel_format,
    };
    use x11rb::{
        CURRENT_TIME, NONE,
        connection::Connection as _,
        protocol::{
            randr::ConnectionExt as _,
            xproto::{
                BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, ConnectionExt as _, ImageFormat,
                ImageOrder, KEY_PRESS_EVENT, KEY_RELEASE_EVENT, MOTION_NOTIFY_EVENT,
            },
            xtest::ConnectionExt as _,
        },
        rust_connection::RustConnection,
        wrapper::ConnectionExt as _,
    };

    struct RealX11Capture {
        dpy: String,
        conn: RustConnection,
        screen_num: usize,
    }

    impl RealX11Capture {
        fn open(dpy: String) -> Result<Self, CaptureFailure> {
            let (conn, screen_num) = x11rb::connect(Some(&dpy))
                .map_err(|_| CaptureFailure::ConnectionLost(Reason::Disconnected))?;
            Ok(Self {
                dpy,
                conn,
                screen_num,
            })
        }
    }

    fn estimate_dpi(width_px: u32, width_mm: u32) -> u32 {
        if width_mm == 0 {
            return 96;
        }
        ((u64::from(width_px) * 254) / (u64::from(width_mm) * 10))
            .clamp(72, 480)
            .try_into()
            .unwrap_or(96)
    }

    fn hash_layout(root: PixelRect, monitors: &[MonitorDescriptor], timestamp: u32) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut mix = |v: u64| {
            h ^= v;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        };
        mix(u64::from(root.width));
        mix(u64::from(root.height));
        mix(u64::from(timestamp));
        for m in monitors {
            mix(i64::from(m.rect.x).cast_unsigned());
            mix(i64::from(m.rect.y).cast_unsigned());
            mix(u64::from(m.rect.width));
            mix(u64::from(m.rect.height));
            mix(u64::from(m.dpi));
            mix(u64::from(m.is_primary));
        }
        h
    }

    impl X11CaptureConnection for RealX11Capture {
        fn query_layout(&mut self) -> Result<X11DisplayLayout, CaptureFailure> {
            let setup = self.conn.setup();
            let screen = setup
                .roots
                .get(self.screen_num)
                .ok_or(CaptureFailure::ConnectionLost(Reason::Disconnected))?;
            let root = screen.root;
            let root_depth = screen.root_depth;
            let root_w = u32::from(screen.width_in_pixels);
            let root_h = u32::from(screen.height_in_pixels);
            let geom_reply = self
                .conn
                .get_geometry(root)
                .map_err(|_| CaptureFailure::ConnectionLost(Reason::Disconnected))?
                .reply()
                .map_err(|_| CaptureFailure::ConnectionLost(Reason::Disconnected))?;
            let live_w = u32::from(geom_reply.width).max(1);
            let live_h = u32::from(geom_reply.height).max(1);
            let root_rect = PixelRect {
                x: 0,
                y: 0,
                width: if live_w > 0 { live_w } else { root_w },
                height: if live_h > 0 { live_h } else { root_h },
            };

            let bpp = setup
                .pixmap_formats
                .iter()
                .find(|pf| pf.depth == root_depth)
                .map_or(32, |pf| pf.bits_per_pixel);
            let scanline_pad = setup
                .pixmap_formats
                .iter()
                .find(|pf| pf.depth == root_depth)
                .map_or(32, |pf| pf.scanline_pad);
            let format = X11PixmapFormat {
                depth: root_depth,
                bits_per_pixel: bpp,
                scanline_pad,
                big_endian: setup.image_byte_order == ImageOrder::MSB_FIRST,
            };

            let mut monitors = Vec::new();
            let mut timestamp = 0u32;
            if let Ok(Ok(ver)) = self
                .conn
                .randr_query_version(1, 5)
                .map(x11rb::cookie::Cookie::reply)
                && (ver.major_version > 1 || (ver.major_version == 1 && ver.minor_version >= 5))
                && let Ok(Ok(rep)) = self
                    .conn
                    .randr_get_monitors(root, true)
                    .map(x11rb::cookie::Cookie::reply)
            {
                timestamp = rep.timestamp;
                for m in rep.monitors {
                    if m.width == 0 || m.height == 0 {
                        continue;
                    }
                    let name = self
                        .conn
                        .get_atom_name(m.name)
                        .ok()
                        .and_then(|c| c.reply().ok())
                        .map(|r| String::from_utf8_lossy(&r.name).to_string())
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| format!("monitor-{}", monitors.len()));
                    monitors.push(MonitorDescriptor {
                        name,
                        is_primary: m.primary,
                        rect: PixelRect {
                            x: i32::from(m.x),
                            y: i32::from(m.y),
                            width: u32::from(m.width),
                            height: u32::from(m.height),
                        },
                        dpi: estimate_dpi(u32::from(m.width), m.width_in_millimeters),
                    });
                }
            }
            if monitors.is_empty() {
                monitors.push(MonitorDescriptor {
                    name: format!("x11:{}", self.dpy),
                    is_primary: true,
                    rect: root_rect,
                    dpi: estimate_dpi(root_rect.width, u32::from(screen.width_in_millimeters)),
                });
            }

            let display_token = hash_layout(root_rect, &monitors, timestamp);
            Ok(X11DisplayLayout {
                root_rect,
                monitors,
                format,
                display_token,
            })
        }

        fn get_image(
            &mut self,
            rect: PixelRect,
            out_raw: &mut Vec<u8>,
        ) -> Result<(PixelFormat, usize), CaptureFailure> {
            rect.validate_for_capture()
                .map_err(|_| CaptureFailure::LimitExceeded)?;
            let left = i16::try_from(rect.x).map_err(|_| CaptureFailure::LimitExceeded)?;
            let top = i16::try_from(rect.y).map_err(|_| CaptureFailure::LimitExceeded)?;
            let width = u16::try_from(rect.width).map_err(|_| CaptureFailure::LimitExceeded)?;
            let height = u16::try_from(rect.height).map_err(|_| CaptureFailure::LimitExceeded)?;

            let layout = self.query_layout()?;
            let px_fmt = x11_pixel_format(layout.format)?;
            let root = self.conn.setup().roots[self.screen_num].root;
            let reply = self
                .conn
                .get_image(ImageFormat::Z_PIXMAP, root, left, top, width, height, !0)
                .map_err(|_| CaptureFailure::ConnectionLost(Reason::Disconnected))?
                .reply()
                .map_err(|err| match err {
                    x11rb::errors::ReplyError::X11Error(_) => {
                        CaptureFailure::Unavailable(Reason::GeometryChanged)
                    }
                    x11rb::errors::ReplyError::ConnectionError(_) => {
                        CaptureFailure::ConnectionLost(Reason::Disconnected)
                    }
                })?;

            let stride = compute_x11_stride(reply.data.len(), rect.width, rect.height, px_fmt)?;
            out_raw.clear();
            out_raw.extend_from_slice(&reply.data);
            Ok((px_fmt, stride))
        }
    }

    struct RealX11Input {
        conn: RustConnection,
        root: u32,
    }

    impl RealX11Input {
        fn open(dpy: &str) -> Result<Self, ViewError> {
            let (conn, screen_num) =
                x11rb::connect(Some(dpy)).map_err(|_| ViewError::Unavailable)?;
            let root = conn.setup().roots[screen_num].root;
            Ok(Self { conn, root })
        }
    }

    impl X11InputConnection for RealX11Input {
        fn has_xtest(&mut self) -> Result<bool, ViewError> {
            match self.conn.xtest_get_version(2, 1) {
                Ok(cookie) => Ok(cookie.reply().is_ok()),
                Err(_) => Ok(false),
            }
        }

        fn keymap(&mut self) -> Result<X11KeymapSnapshot, ViewError> {
            let setup = self.conn.setup();
            let min_kc = setup.min_keycode;
            let max_kc = setup.max_keycode;
            let count = max_kc.saturating_sub(min_kc).saturating_add(1);
            let rep = self
                .conn
                .get_keyboard_mapping(min_kc, count)
                .map_err(|_| ViewError::Unavailable)?
                .reply()
                .map_err(|_| ViewError::Unavailable)?;
            Ok(X11KeymapSnapshot {
                min_keycode: min_kc,
                max_keycode: max_kc,
                keysyms_per_keycode: rep.keysyms_per_keycode,
                keysyms: rep.keysyms,
            })
        }

        fn bind_scratch_keycode(&mut self, keycode: u8, keysym: u32) -> Result<(), ViewError> {
            self.conn
                .change_keyboard_mapping(1, keycode, 2, &[keysym, keysym])
                .map_err(|_| ViewError::Unavailable)?;
            self.conn.sync().map_err(|_| ViewError::Unavailable)
        }

        fn unbind_scratch_keycode(&mut self, keycode: u8) -> Result<(), ViewError> {
            self.conn
                .change_keyboard_mapping(1, keycode, 2, &[0, 0])
                .map_err(|_| ViewError::Unavailable)?;
            self.conn.sync().map_err(|_| ViewError::Unavailable)
        }

        fn motion(&mut self, x: i16, y: i16) -> Result<(), ViewError> {
            self.conn
                .xtest_fake_input(MOTION_NOTIFY_EVENT, 0, CURRENT_TIME, NONE, x, y, 0)
                .map_err(|_| ViewError::Unavailable)?;
            self.conn.sync().map_err(|_| ViewError::Unavailable)
        }

        fn button(&mut self, button: u8, press: bool) -> Result<(), ViewError> {
            let ev = if press {
                BUTTON_PRESS_EVENT
            } else {
                BUTTON_RELEASE_EVENT
            };
            self.conn
                .xtest_fake_input(ev, button, CURRENT_TIME, self.root, 0, 0, 0)
                .map_err(|_| ViewError::Unavailable)?;
            self.conn.sync().map_err(|_| ViewError::Unavailable)
        }

        fn key(&mut self, keycode: u8, press: bool) -> Result<(), ViewError> {
            let ev = if press {
                KEY_PRESS_EVENT
            } else {
                KEY_RELEASE_EVENT
            };
            self.conn
                .xtest_fake_input(ev, keycode, CURRENT_TIME, self.root, 0, 0, 0)
                .map_err(|_| ViewError::Unavailable)?;
            self.conn.sync().map_err(|_| ViewError::Unavailable)
        }
    }

    /// Create an X11 live-view target on Linux, checking session type, `DISPLAY`,
    /// `XGetImage`, `RandR`, and `XTest` availability.
    ///
    /// # Errors
    ///
    /// Returns [`Reason::Unsupported`] on Wayland or [`Reason::Unavailable`] when
    /// no X11 server is reachable.
    pub fn open_x11_live_view_target(
        options: &LiveViewOptions,
    ) -> Result<Arc<DesktopViewTarget>, Reason> {
        type RealCaptureFactory = Arc<dyn Fn() -> Result<RealX11Capture, CaptureFailure> + Send + Sync>;
        type RealInputFactory = Arc<dyn Fn() -> Result<RealX11Input, ViewError> + Send + Sync>;

        let xdg = std::env::var("XDG_SESSION_TYPE").ok();
        let wayland = std::env::var("WAYLAND_DISPLAY").ok();
        let x11 = std::env::var("DISPLAY").ok();
        let dpy = validate_x11_env(xdg.as_deref(), wayland.as_deref(), x11.as_deref())?;

        let init_cap = RealX11Capture::open(dpy.clone()).map_err(|_| Reason::Unavailable)?;
        let xtest_ok = RealX11Input::open(&dpy)
            .ok()
            .and_then(|mut c| c.has_xtest().ok())
            .unwrap_or(false);

        let dpy_cap = dpy.clone();
        let dpy_probe = dpy.clone();
        let dpy_in = dpy;

        let capture_factory: RealCaptureFactory =
            Arc::new(move || RealX11Capture::open(dpy_cap.clone()));
        let probe: AvailabilityProbeFn = Arc::new(move |scope| {
            let xdg = std::env::var("XDG_SESSION_TYPE").ok();
            let wayland = std::env::var("WAYLAND_DISPLAY").ok();
            let x11 = std::env::var("DISPLAY").ok();
            let _ = validate_x11_env(xdg.as_deref(), wayland.as_deref(), x11.as_deref())?;
            let mut c = RealX11Capture::open(dpy_probe.clone()).map_err(|_| Reason::Unavailable)?;
            let layout = c.query_layout().map_err(|_| Reason::Unavailable)?;
            let _ = select_capture_rect(
                scope,
                Some(layout.root_rect),
                &layout.monitors,
                layout.display_token,
            )?;
            Ok(())
        });

        let (input_factory, input_reason): (Option<RealInputFactory>, Option<Reason>) = if xtest_ok
        {
            (
                Some(Arc::new(move || RealX11Input::open(&dpy_in))),
                None,
            )
        } else {
            (None, Some(Reason::Unsupported))
        };

        build_x11_view_target(
            options,
            init_cap,
            capture_factory,
            probe,
            input_factory,
            input_reason,
        )
    }
}

#[cfg(target_os = "linux")]
pub use linux_x11::open_x11_live_view_target;

// ── Hermetic cross-platform tests ─────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use live_view::{Codec, EffectiveProfile, ViewTarget};
    use parking_lot::Mutex;
    use std::sync::atomic::AtomicU64;

    #[derive(Clone)]
    struct MockX11CapState {
        layout: X11DisplayLayout,
        raw: Vec<u8>,
        stride: usize,
        format: PixelFormat,
        get_image_calls: Arc<AtomicU64>,
        drop_conn_once: Arc<AtomicBool>,
    }

    struct MockX11Capture {
        state: Arc<Mutex<MockX11CapState>>,
    }

    impl X11CaptureConnection for MockX11Capture {
        fn query_layout(&mut self) -> Result<X11DisplayLayout, CaptureFailure> {
            Ok(self.state.lock().layout.clone())
        }

        fn get_image(
            &mut self,
            rect: PixelRect,
            out_raw: &mut Vec<u8>,
        ) -> Result<(PixelFormat, usize), CaptureFailure> {
            let st = self.state.lock();
            if st.drop_conn_once.swap(false, Ordering::SeqCst) {
                return Err(CaptureFailure::ConnectionLost(Reason::Disconnected));
            }
            st.get_image_calls.fetch_add(1, Ordering::SeqCst);
            assert!(rect.width * rect.height <= 16_000_000);
            out_raw.clear();
            out_raw.extend_from_slice(&st.raw);
            Ok((st.format, st.stride))
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum X11Call {
        Motion(i16, i16),
        Button(u8, bool),
        Key(u8, bool),
        Bind(u8, u32),
        Unbind(u8),
    }

    struct MockX11InputState {
        has_xtest: bool,
        keymap: X11KeymapSnapshot,
        calls: Vec<X11Call>,
        fail_after_n: Option<usize>,
    }

    struct MockX11Input {
        state: Arc<Mutex<MockX11InputState>>,
    }

    impl MockX11Input {
        fn record(&self, call: X11Call) -> Result<(), ViewError> {
            let mut st = self.state.lock();
            if let Some(rem) = st.fail_after_n.as_mut() {
                if *rem == 0 {
                    return Err(ViewError::Unavailable);
                }
                *rem -= 1;
            }
            st.calls.push(call);
            Ok(())
        }
    }

    impl X11InputConnection for MockX11Input {
        fn has_xtest(&mut self) -> Result<bool, ViewError> {
            Ok(self.state.lock().has_xtest)
        }

        fn keymap(&mut self) -> Result<X11KeymapSnapshot, ViewError> {
            Ok(self.state.lock().keymap.clone())
        }

        fn bind_scratch_keycode(&mut self, keycode: u8, keysym: u32) -> Result<(), ViewError> {
            self.record(X11Call::Bind(keycode, keysym))?;
            let mut st = self.state.lock();
            let offset = usize::from(keycode - st.keymap.min_keycode);
            let per = usize::from(st.keymap.keysyms_per_keycode);
            st.keymap.keysyms[offset * per] = keysym;
            st.keymap.keysyms[offset * per + 1] = keysym;
            Ok(())
        }

        fn unbind_scratch_keycode(&mut self, keycode: u8) -> Result<(), ViewError> {
            self.record(X11Call::Unbind(keycode))?;
            let mut st = self.state.lock();
            let offset = usize::from(keycode - st.keymap.min_keycode);
            let per = usize::from(st.keymap.keysyms_per_keycode);
            st.keymap.keysyms[offset * per] = 0;
            st.keymap.keysyms[offset * per + 1] = 0;
            Ok(())
        }

        fn motion(&mut self, x: i16, y: i16) -> Result<(), ViewError> {
            self.record(X11Call::Motion(x, y))
        }

        fn button(&mut self, button: u8, press: bool) -> Result<(), ViewError> {
            self.record(X11Call::Button(button, press))
        }

        fn key(&mut self, keycode: u8, press: bool) -> Result<(), ViewError> {
            self.record(X11Call::Key(keycode, press))
        }
    }

    fn base_keymap(full: bool) -> X11KeymapSnapshot {
        // Keycodes 9..=13 (2 keysyms per keycode):
        // kc 9: a / A, kc 10: Shift_L, kc 11: Control_L, kc 12: free (unless full), kc 13: Return
        let mut keysyms = vec![
            u32::from(b'a'),
            u32::from(b'A'),
            0xFFE1,
            0xFFE1,
            0xFFE3,
            0xFFE3,
            0,
            0,
            0xFF0D,
            0xFF0D,
        ];
        if full {
            keysyms[6] = u32::from(b'z');
            keysyms[7] = u32::from(b'Z');
        }
        X11KeymapSnapshot {
            min_keycode: 9,
            max_keycode: 13,
            keysyms_per_keycode: 2,
            keysyms,
        }
    }

    #[test]
    fn env_detection_refuses_wayland_as_unsupported_and_missing_display_as_unavailable() {
        assert_eq!(
            validate_x11_env(Some("wayland"), Some("wayland-0"), Some(":0")),
            Err(Reason::Unsupported)
        );
        assert_eq!(
            validate_x11_env(Some("x11"), None, None),
            Err(Reason::Unavailable)
        );
        assert_eq!(
            validate_x11_env(Some("x11"), None, Some(":99")),
            Ok(":99".into())
        );
    }

    #[test]
    fn oversized_x11_virtual_desktop_fails_before_calling_get_image() {
        let state = Arc::new(Mutex::new(MockX11CapState {
            layout: X11DisplayLayout {
                root_rect: PixelRect {
                    x: 0,
                    y: 0,
                    width: 8000,
                    height: 3000,
                },
                monitors: vec![MonitorDescriptor {
                    name: "DP-1".into(),
                    is_primary: true,
                    rect: PixelRect {
                        x: 0,
                        y: 0,
                        width: 8000,
                        height: 3000,
                    },
                    dpi: 96,
                }],
                format: X11PixmapFormat {
                    depth: 24,
                    bits_per_pixel: 32,
                    scanline_pad: 32,
                    big_endian: false,
                },
                display_token: 1,
            },
            raw: Vec::new(),
            stride: 0,
            format: PixelFormat::Bgra32,
            get_image_calls: Arc::new(AtomicU64::new(0)),
            drop_conn_once: Arc::new(AtomicBool::new(false)),
        }));
        let mut drv = X11CaptureDriver::new(
            MockX11Capture {
                state: Arc::clone(&state),
            },
            CaptureScope::VirtualDesktop,
        );
        let mut buf = Vec::new();
        assert_eq!(drv.capture(&mut buf).err(), Some(CaptureFailure::LimitExceeded));
        assert_eq!(state.lock().get_image_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn x11_capture_and_randr_layout_changes_publish_jpeg_with_zero_temp_files() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let stride = 80 * 4 + 16; // Deliberate odd scanline padding!
        let mut raw = vec![0xCD; stride * 60];
        for y in 0..60 {
            for x in 0..80 {
                let off = y * stride + x * 4;
                raw[off] = u8::try_from(x & 0xFF).unwrap_or(0);
                raw[off + 1] = u8::try_from(y & 0xFF).unwrap_or(0);
                raw[off + 2] = 0x55;
                raw[off + 3] = 0x00;
            }
        }
        let cap_state = Arc::new(Mutex::new(MockX11CapState {
            layout: X11DisplayLayout {
                root_rect: PixelRect {
                    x: 0,
                    y: 0,
                    width: 80,
                    height: 60,
                },
                monitors: vec![MonitorDescriptor {
                    name: "HDMI-1".into(),
                    is_primary: true,
                    rect: PixelRect {
                        x: 0,
                        y: 0,
                        width: 80,
                        height: 60,
                    },
                    dpi: 96,
                }],
                format: X11PixmapFormat {
                    depth: 24,
                    bits_per_pixel: 32,
                    scanline_pad: 32,
                    big_endian: false,
                },
                display_token: 10,
            },
            raw,
            stride,
            format: PixelFormat::Bgra32,
            get_image_calls: Arc::new(AtomicU64::new(0)),
            drop_conn_once: Arc::new(AtomicBool::new(false)),
        }));

        let target = build_x11_view_target::<_, MockX11Input>(
            &LiveViewOptions::new("linux-x11-host"),
            MockX11Capture {
                state: Arc::clone(&cap_state),
            },
            Arc::new({
                let s = Arc::clone(&cap_state);
                move || Ok(MockX11Capture { state: Arc::clone(&s) })
            }),
            Arc::new(|_| Ok(())),
            None,
            Some(Reason::Unsupported),
        )
        .unwrap();

        let info = target.info();
        assert!(info.capabilities.view);
        assert!(!info.capabilities.input);
        assert_eq!(info.capabilities.codecs, vec![Codec::Jpeg]);
        assert!(target.input_sink().is_none());

        let src = target.frame_source();
        let mut sub = src.subscribe(EffectiveProfile::watch()).await.unwrap();
        let f1 = sub.frames.borrow_and_update().clone().unwrap();
        assert_eq!(f1.metadata.geometry_revision, 1);
        assert_eq!(&f1.payload[..2], &[0xFF, 0xD8]);
        assert_eq!(f1.metadata.source_rect.units, live_view::Units::PhysicalPx);

        // Simulate RandR resolution switch from 80x60 -> 100x80
        {
            let mut st = cap_state.lock();
            st.layout.root_rect.width = 100;
            st.layout.root_rect.height = 80;
            st.layout.monitors[0].rect.width = 100;
            st.layout.monitors[0].rect.height = 80;
            st.layout.display_token = 11;
            st.stride = 100 * 4;
            st.raw = vec![0x77; 100 * 80 * 4];
        }
        src.request_refresh(42).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        let f2 = sub.frames.borrow_and_update().clone().unwrap();
        assert_eq!(f2.metadata.geometry_revision, 2);
        assert_eq!(f2.metadata.source_rect.width, 100);
        assert_eq!(f2.metadata.action_seq, Some(42));

        src.stop().await.unwrap();
        drop(sub);
        // No temporary image files ever touched disk.
        assert_eq!(std::fs::read_dir(tmp_dir.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn xtest_driver_tracks_pressed_inputs_rejects_unmappable_keys_and_handles_unicode() {
        let in_state = Arc::new(Mutex::new(MockX11InputState {
            has_xtest: true,
            keymap: base_keymap(false),
            calls: Vec::new(),
            fail_after_n: None,
        }));
        let mut drv = X11InputDriver::new(MockX11Input {
            state: Arc::clone(&in_state),
        })
        .unwrap();
        let sr = SourceRect {
            x: 100.0,
            y: 50.0,
            width: 1000,
            height: 500,
            units: live_view::Units::PhysicalPx,
        };
        let vr = PixelRect {
            x: 100,
            y: 50,
            width: 1000,
            height: 500,
        };
        let cancel = AtomicBool::new(false);

        // Pointer down at normalized (0.5, 0.5) -> maps cleanly to physical pixels (600, 300)
        drv.inject(
            &InputEvent::Pointer {
                phase: PointerPhase::Down,
                x: 0.5,
                y: 0.5,
                button: Some(Button::Middle),
                buttons: 2,
                modifiers: vec![],
            },
            sr.clone(),
            vr,
            &cancel,
        )
        .unwrap();
        // Hold ShiftLeft + KeyA ('A' hits col 1 of permanent kc 9, NO scratch binding!)
        drv.inject(
            &InputEvent::Key {
                phase: KeyPhase::Down,
                code: "ShiftLeft".into(),
                key: "Shift".into(),
                location: 1,
                repeat: false,
                modifiers: vec![Modifier::Shift],
            },
            sr.clone(),
            vr,
            &cancel,
        )
        .unwrap();
        drv.inject(
            &InputEvent::Key {
                phase: KeyPhase::Down,
                code: "KeyA".into(),
                key: "A".into(),
                location: 0,
                repeat: false,
                modifiers: vec![Modifier::Shift],
            },
            sr.clone(),
            vr,
            &cancel,
        )
        .unwrap();

        // Turkish Unicode text binds scratch kc 12 cleanly and unbinds right after each char.
        drv.inject(
            &InputEvent::Text { text: "ğİ".into() },
            sr.clone(),
            vr,
            &cancel,
        )
        .unwrap();
        // Dead / Unidentified key explicitly rejected without guessing.
        assert_eq!(
            drv.inject(
                &InputEvent::Key {
                    phase: KeyPhase::Down,
                    code: "KeyE".into(),
                    key: "Dead".into(),
                    location: 0,
                    repeat: false,
                    modifiers: vec![],
                },
                sr.clone(),
                vr,
                &cancel,
            ),
            Err(ViewError::Unsupported)
        );

        // release_all releases ONLY held button 2, kc 9 (KeyA), and kc 10 (ShiftLeft)
        in_state.lock().calls.clear();
        drv.release_all().unwrap();
        let calls = in_state.lock().calls.clone();
        assert!(calls.contains(&X11Call::Key(9, false)));
        assert!(calls.contains(&X11Call::Key(10, false)));
        assert!(calls.contains(&X11Call::Button(2, false)));
        assert_eq!(calls.len(), 3);

        // Repeating release_all is an idempotent no-op.
        in_state.lock().calls.clear();
        drv.release_all().unwrap();
        assert_eq!(in_state.lock().calls.len(), 0);
    }

    #[test]
    fn xtest_full_keymap_rejects_unmapped_unicode() {
        let mut drv_full = X11InputDriver::new(MockX11Input {
            state: Arc::new(Mutex::new(MockX11InputState {
                has_xtest: true,
                keymap: base_keymap(true),
                calls: Vec::new(),
                fail_after_n: None,
            })),
        })
        .unwrap();
        let sr = SourceRect {
            x: 0.0,
            y: 0.0,
            width: 100,
            height: 100,
            units: live_view::Units::PhysicalPx,
        };
        let vr = PixelRect {
            x: 0,
            y: 0,
            width: 100,
            height: 100,
        };
        assert_eq!(
            drv_full.inject(
                &InputEvent::Text { text: "ş".into() },
                sr,
                vr,
                &AtomicBool::new(false)
            ),
            Err(ViewError::Unsupported)
        );
    }

    #[test]
    fn xtest_mid_text_failure_returns_partial_injection() {
        let in_state = Arc::new(Mutex::new(MockX11InputState {
            has_xtest: true,
            keymap: base_keymap(false),
            calls: Vec::new(),
            fail_after_n: Some(2), // first char down+up succeeds, second char down fails
        }));
        let mut drv = X11InputDriver::new(MockX11Input {
            state: Arc::clone(&in_state),
        })
        .unwrap();
        let sr = SourceRect {
            x: 0.0,
            y: 0.0,
            width: 100,
            height: 100,
            units: live_view::Units::PhysicalPx,
        };
        let vr = PixelRect {
            x: 0,
            y: 0,
            width: 100,
            height: 100,
        };
        assert_eq!(
            drv.inject(
                &InputEvent::Text {
                    text: "aa".into()
                },
                sr,
                vr,
                &AtomicBool::new(false)
            ),
            Err(ViewError::PartialInjection)
        );
    }
}
