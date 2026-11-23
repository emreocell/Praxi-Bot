//! Shared OS-independent live-view runtime, pixel adaptation, geometry math,
//! and worker coordination for desktop backends.
//!
//! Platform crates (`desktop-linux`, `desktop-windows`) implement the thin OS
//! capture and input traits defined here so subscription lifecycle, bounding
//! checks, scaling, RAM-only JPEG encoding, geometry revision tracking, and
//! cancel-safe input ordering are shared across OS backends.

use std::sync::{
    Arc, Weak,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use jpeg_encoder::{ColorType as JpegColorType, Encoder as JpegEncoder, SamplingFactor};
use live_view::{
    Capabilities as LiveCapabilities, Cause, Codec, EffectiveProfile, Frame, FrameMetadata,
    FrameSource, FrameSubscription, InputEvent, InputSink, MAX_HEADER, MAX_PAYLOAD, Profile,
    Reason, SourceRect, TargetInfo, TargetKind, Units, ViewError, ViewTarget,
};
use parking_lot::{Condvar, Mutex};
use tokio::sync::{oneshot, watch};
use uuid::Uuid;

/// Maximum raw pixel count allowed by the live-view contract (16 Mpixels).
pub const MAX_RAW_PIXELS: u64 = 16_000_000;
/// Hard ceiling on output JPEG width in pixels.
pub const MAX_OUTPUT_WIDTH: u32 = 1920;
/// Hard ceiling on output JPEG height in pixels.
pub const MAX_OUTPUT_HEIGHT: u32 = 1080;
/// Hard ceiling on live-takeover frame rate in FPS (ADR-0029 §5).
pub const LIVE_MAX_FPS_CAP: u8 = 15;
/// Watch-mode periodic idle capture interval.
pub const WATCH_IDLE_INTERVAL: Duration = Duration::from_secs(3);
/// Maximum delay between `Live` keepalive publications to stay below daemon 2 s staleness.
pub const LIVE_HEARTBEAT_INTERVAL: Duration = Duration::from_millis(800);
/// Default retry backoff when the OS display is temporarily locked or unavailable.
const UNAVAILABLE_RETRY_INTERVAL: Duration = Duration::from_millis(500);
/// Default periodic interval for background availability probing while capture is idle.
const AVAILABILITY_PROBE_INTERVAL: Duration = Duration::from_secs(5);
/// Hard bound on how long an individual availability probe may run before timing out.
const AVAILABILITY_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// Minimum downscaled side length allowed when trimming frame size under tight burst budgets.
const MIN_BUDGET_DIM: u32 = 64;

// ── Public configuration & geometry types ─────────────────────────────────────

/// Selection of which desktop display region a live-view target captures.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum CaptureScope {
    /// Capture the full virtual desktop when it fits within [`MAX_RAW_PIXELS`],
    /// otherwise select the primary monitor automatically.
    #[default]
    Auto,
    /// Capture the primary monitor only.
    PrimaryMonitor,
    /// Capture the full virtual desktop across all monitors (fails if > 16 Mpixels).
    VirtualDesktop,
    /// Capture a specific monitor matched by OS name/output selector or numeric index.
    Monitor { selector: String },
    /// Capture an explicit rectangle in global physical pixels.
    Region {
        x: i32,
        y: i32,
        width: u32,
        height: u32,
    },
}

/// Configuration passed to [`crate::DesktopBackend::live_view_target`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveViewOptions {
    /// Stable host identifier placed into [`TargetInfo::host_id`].
    pub host_id: String,
    /// Which screen / monitor / region to capture.
    pub scope: CaptureScope,
    /// Optional owning bot ID for target metadata.
    pub bot_id: Option<Uuid>,
    /// Optional owning run ID for target metadata.
    pub run_id: Option<Uuid>,
}

impl LiveViewOptions {
    /// Create live-view options for `host_id` with [`CaptureScope::Auto`].
    #[must_use]
    pub fn new(host_id: impl Into<String>) -> Self {
        Self {
            host_id: host_id.into(),
            scope: CaptureScope::Auto,
            bot_id: None,
            run_id: None,
        }
    }

    /// Override the capture scope.
    #[must_use]
    pub fn with_scope(mut self, scope: CaptureScope) -> Self {
        self.scope = scope;
        self
    }

    /// Associate a bot ID with the target metadata.
    #[must_use]
    pub const fn with_bot_id(mut self, bot_id: Option<Uuid>) -> Self {
        self.bot_id = bot_id;
        self
    }

    /// Associate a run ID with the target metadata.
    #[must_use]
    pub const fn with_run_id(mut self, run_id: Option<Uuid>) -> Self {
        self.run_id = run_id;
        self
    }
}

/// A discrete screen rectangle in physical pixel coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl PixelRect {
    /// Compute total pixel count safely, returning `None` on overflow.
    #[must_use]
    pub fn pixel_count(&self) -> Option<u64> {
        u64::from(self.width).checked_mul(u64::from(self.height))
    }

    /// Validate that the rectangle is non-empty, stays within `i32` bounds,
    /// and does not exceed [`MAX_RAW_PIXELS`].
    ///
    /// # Errors
    ///
    /// Returns [`ViewError::LimitExceeded`] if dimensions are zero, exceed
    /// 16 Mpixels, or overflow physical coordinate arithmetic.
    pub fn validate_for_capture(&self) -> Result<(), ViewError> {
        let Some(px) = self.pixel_count() else {
            return Err(ViewError::LimitExceeded);
        };
        if self.width == 0 || self.height == 0 || px > MAX_RAW_PIXELS {
            return Err(ViewError::LimitExceeded);
        }
        let max_x = i64::from(self.x).checked_add(i64::from(self.width));
        let max_y = i64::from(self.y).checked_add(i64::from(self.height));
        if max_x.is_none() || max_y.is_none() {
            return Err(ViewError::LimitExceeded);
        }
        Ok(())
    }

    /// Convert to protocol [`SourceRect`] with [`Units::PhysicalPx`].
    #[must_use]
    pub fn to_source_rect(&self) -> SourceRect {
        SourceRect {
            x: f64::from(self.x),
            y: f64::from(self.y),
            width: self.width,
            height: self.height,
            units: Units::PhysicalPx,
        }
    }

    /// Intersect this rectangle with `other`, returning `None` if disjoint.
    #[must_use]
    pub fn intersect(&self, other: &Self) -> Option<Self> {
        let x0 = i64::from(self.x).max(i64::from(other.x));
        let y0 = i64::from(self.y).max(i64::from(other.y));
        let x1 = (i64::from(self.x) + i64::from(self.width))
            .min(i64::from(other.x) + i64::from(other.width));
        let y1 = (i64::from(self.y) + i64::from(self.height))
            .min(i64::from(other.y) + i64::from(other.height));
        let w = x1 - x0;
        let h = y1 - y0;
        if w <= 0 || h <= 0 {
            return None;
        }
        Some(Self {
            x: i32::try_from(x0).ok()?,
            y: i32::try_from(y0).ok()?,
            width: u32::try_from(w).ok()?,
            height: u32::try_from(h).ok()?,
        })
    }
}

/// Monitor descriptor provided by OS backends for [`select_capture_rect`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorDescriptor {
    pub name: String,
    pub is_primary: bool,
    pub rect: PixelRect,
    pub dpi: u32,
}

/// Snapshot of active capture geometry + DPI + topology token used to detect
/// `geometry_revision` changes cleanly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GeometrySnapshot {
    pub rect: PixelRect,
    pub virtual_rect: PixelRect,
    pub dpi: u32,
    pub display_token: u64,
}

/// Select the capture rectangle and initial [`GeometrySnapshot`] for `scope`
/// without allocating pixel buffers.
///
/// # Errors
///
/// Returns [`Reason::Unavailable`] if no valid display matches or bounds are
/// empty, or [`Reason::FrameBudgetExceeded`] when the explicitly requested area
/// exceeds [`MAX_RAW_PIXELS`].
pub fn select_capture_rect(
    scope: &CaptureScope,
    virtual_desktop: Option<PixelRect>,
    monitors: &[MonitorDescriptor],
    display_token: u64,
) -> Result<GeometrySnapshot, Reason> {
    let computed_virtual = virtual_desktop.or_else(|| bounding_box_of(monitors));
    let (selected, dpi) = match scope {
        CaptureScope::Auto => select_auto(computed_virtual, monitors)?,
        CaptureScope::PrimaryMonitor => select_primary(monitors)?,
        CaptureScope::VirtualDesktop => {
            let v = computed_virtual.ok_or(Reason::Unavailable)?;
            v.validate_for_capture()
                .map_err(|_| Reason::FrameBudgetExceeded)?;
            (v, monitors.first().map_or(96, |m| m.dpi.max(1)))
        }
        CaptureScope::Monitor { selector } => select_by_selector(selector, monitors)?,
        CaptureScope::Region {
            x,
            y,
            width,
            height,
        } => {
            let req = PixelRect {
                x: *x,
                y: *y,
                width: *width,
                height: *height,
            };
            let clamped = if let Some(v) = computed_virtual {
                req.intersect(&v).ok_or(Reason::Unavailable)?
            } else {
                req
            };
            clamped
                .validate_for_capture()
                .map_err(|_| Reason::FrameBudgetExceeded)?;
            let dpi = monitors
                .iter()
                .find(|m| clamped.intersect(&m.rect).is_some())
                .map_or(96, |m| m.dpi.max(1));
            (clamped, dpi)
        }
    };

    Ok(GeometrySnapshot {
        rect: selected,
        virtual_rect: computed_virtual.unwrap_or(selected),
        dpi,
        display_token,
    })
}

fn bounding_box_of(monitors: &[MonitorDescriptor]) -> Option<PixelRect> {
    let first = monitors.first()?;
    let mut min_x = i64::from(first.rect.x);
    let mut min_y = i64::from(first.rect.y);
    let mut max_x = min_x + i64::from(first.rect.width);
    let mut max_y = min_y + i64::from(first.rect.height);
    for mon in &monitors[1..] {
        min_x = min_x.min(i64::from(mon.rect.x));
        min_y = min_y.min(i64::from(mon.rect.y));
        max_x = max_x.max(i64::from(mon.rect.x) + i64::from(mon.rect.width));
        max_y = max_y.max(i64::from(mon.rect.y) + i64::from(mon.rect.height));
    }
    Some(PixelRect {
        x: i32::try_from(min_x).ok()?,
        y: i32::try_from(min_y).ok()?,
        width: u32::try_from(max_x - min_x).ok()?,
        height: u32::try_from(max_y - min_y).ok()?,
    })
}

fn select_auto(
    virtual_desktop: Option<PixelRect>,
    monitors: &[MonitorDescriptor],
) -> Result<(PixelRect, u32), Reason> {
    if let Some(v) = virtual_desktop
        && v.validate_for_capture().is_ok()
    {
        let dpi = monitors
            .iter()
            .find(|m| m.is_primary)
            .or_else(|| monitors.first())
            .map_or(96, |m| m.dpi.max(1));
        return Ok((v, dpi));
    }
    select_primary(monitors)
}

fn select_primary(monitors: &[MonitorDescriptor]) -> Result<(PixelRect, u32), Reason> {
    let m = monitors
        .iter()
        .find(|m| m.is_primary)
        .or_else(|| monitors.first())
        .ok_or(Reason::Unavailable)?;
    m.rect
        .validate_for_capture()
        .map_err(|_| Reason::FrameBudgetExceeded)?;
    Ok((m.rect, m.dpi.max(1)))
}

fn select_by_selector(
    selector: &str,
    monitors: &[MonitorDescriptor],
) -> Result<(PixelRect, u32), Reason> {
    let trimmed = selector.trim();
    if trimmed.is_empty() {
        return Err(Reason::Unavailable);
    }
    let found = monitors
        .iter()
        .find(|m| m.name.eq_ignore_ascii_case(trimmed))
        .or_else(|| {
            trimmed
                .parse::<usize>()
                .ok()
                .and_then(|idx| monitors.get(idx))
        })
        .ok_or(Reason::Unavailable)?;
    found
        .rect
        .validate_for_capture()
        .map_err(|_| Reason::FrameBudgetExceeded)?;
    Ok((found.rect, found.dpi.max(1)))
}

// ── Coordinate mapping (pure, negative-origin & DPI safe) ──────────────────────

/// Map normalized `[0.0, 1.0]` viewer coordinates into global physical pixels
/// over `source_rect` (supporting negative monitor origins and single-pixel guards).
#[must_use]
pub fn map_normalized_point(norm_x: f64, norm_y: f64, source_rect: &SourceRect) -> (f64, f64) {
    let cx = norm_x.clamp(0.0, 1.0);
    let cy = norm_y.clamp(0.0, 1.0);
    let span_w = f64::from(source_rect.width.saturating_sub(1));
    let span_h = f64::from(source_rect.height.saturating_sub(1));
    (source_rect.x + cx * span_w, source_rect.y + cy * span_h)
}

/// Convert global physical pixel `(phys_x, phys_y)` into `0..=65535` normalized
/// virtual-desktop coordinates covering `virtual_rect` (including negative origins).
///
/// Uses pixel-center projection `floor(((offset + 0.5) * 65536) / width)` for
/// interior pixels so Win32 `SendInput`'s `(abs * width) / 65536` integer
/// truncation lands on the exact physical pixel even on virtual desktops wider
/// than 4800 px, while preserving `0` and `65_535` at the outer edges.
#[must_use]
pub fn point_to_virtual_absolute(phys_x: f64, phys_y: f64, virtual_rect: PixelRect) -> (i32, i32) {
    let project_axis = |pos: f64, origin: i32, span: u32| -> i32 {
        let w = f64::from(span.max(2));
        let max_off = w - 1.0;
        let off = (pos - f64::from(origin)).round().clamp(0.0, max_off);
        if off <= 0.0 {
            return 0;
        }
        if off >= max_off {
            return 65_535;
        }
        let centered = ((off + 0.5) * 65_536.0 / w).floor().clamp(0.0, 65_535.0);
        // Inputs are finite and clamped into `0.0..=65_535.0` before truncating.
        #[allow(clippy::cast_possible_truncation)]
        {
            centered as i32
        }
    };
    (
        project_axis(phys_x, virtual_rect.x, virtual_rect.width),
        project_axis(phys_y, virtual_rect.y, virtual_rect.height),
    )
}

/// Validate that `source_rect` is a non-empty `PhysicalPx` rectangle contained
/// within `virtual_rect` before injecting positional pointer or wheel events.
///
/// # Errors
///
/// Returns [`ViewError::Unavailable`] when `source_rect` has non-finite origin,
/// zero dimensions, non-`PhysicalPx` units, or falls outside `virtual_rect`.
pub fn validate_positional_geometry(
    source_rect: &SourceRect,
    virtual_rect: PixelRect,
) -> Result<(), ViewError> {
    if source_rect.units != Units::PhysicalPx
        || !source_rect.x.is_finite()
        || !source_rect.y.is_finite()
        || source_rect.width == 0
        || source_rect.height == 0
        || virtual_rect.width == 0
        || virtual_rect.height == 0
        || source_rect.x < f64::from(i32::MIN)
        || source_rect.x > f64::from(i32::MAX)
        || source_rect.y < f64::from(i32::MIN)
        || source_rect.y > f64::from(i32::MAX)
    {
        return Err(ViewError::Unavailable);
    }
    #[allow(clippy::cast_possible_truncation)]
    let (sx0, sy0) = (source_rect.x.round() as i64, source_rect.y.round() as i64);
    let sx1 = sx0 + i64::from(source_rect.width);
    let sy1 = sy0 + i64::from(source_rect.height);
    let vx0 = i64::from(virtual_rect.x);
    let vy0 = i64::from(virtual_rect.y);
    let vx1 = vx0 + i64::from(virtual_rect.width);
    let vy1 = vy0 + i64::from(virtual_rect.height);
    if sx0 < vx0 || sy0 < vy0 || sx1 > vx1 || sy1 > vy1 {
        return Err(ViewError::Unavailable);
    }
    Ok(())
}

/// Convert browser CSS-pixel wheel deltas (`[-1200, 1200]`) to OS-agnostic scroll
/// tick units (1 tick ≈ 100 CSS px / 120 Win32 `WHEEL_DELTA` units).
#[must_use]
pub fn wheel_pixels_to_win32_delta(delta: f64, invert: bool) -> i32 {
    if !delta.is_finite() || delta == 0.0 {
        return 0;
    }
    let signed = if invert { -delta } else { delta };
    let scaled = (signed * 1.2).round().clamp(-1_440.0, 1_440.0);
    // Clamped to `-1440.0..=1440.0` before cast.
    #[allow(clippy::cast_possible_truncation)]
    let out = scaled as i32;
    if out == 0 {
        if signed > 0.0 { 1 } else { -1 }
    } else {
        out
    }
}

/// Convert browser CSS-pixel wheel delta into bounded X11 button-click steps (`0..=15`).
#[must_use]
pub fn wheel_pixels_to_x11_steps(delta: f64) -> u32 {
    if !delta.is_finite() || delta == 0.0 {
        return 0;
    }
    let steps = (delta.abs() / 40.0).ceil().clamp(1.0, 15.0);
    // Clamped to `1.0..=15.0` before cast.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    {
        steps as u32
    }
}

// ── Raw pixel validation, extraction, scaling & JPEG budget encoding ───────────

/// Supported raw OS pixel buffer formats prior to scaling/JPEG encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    /// 32-bit `BGRX`/`BGRA` (4 bytes/pixel). Alpha is forced opaque (`255`).
    Bgra32,
    /// Packed 24-bit `BGR` (3 bytes/pixel).
    Bgr24,
    /// 16-bit `RGB565` little-endian (2 bytes/pixel).
    Rgb565Le,
    /// 16-bit `RGB565` big-endian (2 bytes/pixel).
    Rgb565Be,
}

impl PixelFormat {
    const fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Bgra32 => 4,
            Self::Bgr24 => 3,
            Self::Rgb565Le | Self::Rgb565Be => 2,
        }
    }
}

/// Validate raw pixel dimensions before any allocation, rejecting zero dims,
/// overflow, or images exceeding [`MAX_RAW_PIXELS`] (16 Mpixels).
///
/// # Errors
///
/// Returns [`ViewError::LimitExceeded`] if invalid or oversized.
pub fn validate_raw_dimensions(width: u32, height: u32) -> Result<usize, ViewError> {
    if width == 0 || height == 0 {
        return Err(ViewError::LimitExceeded);
    }
    let Some(total) = u64::from(width).checked_mul(u64::from(height)) else {
        return Err(ViewError::LimitExceeded);
    };
    if total > MAX_RAW_PIXELS {
        return Err(ViewError::LimitExceeded);
    }
    usize::try_from(total).map_err(|_| ViewError::LimitExceeded)
}

/// Extract strided raw pixels into tightly packed top-down `BGRA` (`A = 255`),
/// reusing `dst`'s backing buffer.
///
/// # Errors
///
/// Returns [`ViewError::LimitExceeded`] when dimensions exceed [`MAX_RAW_PIXELS`]
/// or overflow `usize`, and [`ViewError::InvalidRecord`] when `stride_bytes` or
/// `src.len()` is smaller than required for `(width, height, format)`.
pub fn extract_strided_bgra_into(
    src: &[u8],
    width: u32,
    height: u32,
    stride_bytes: usize,
    format: PixelFormat,
    dst: &mut Vec<u8>,
) -> Result<(), ViewError> {
    let pixels = validate_raw_dimensions(width, height)?;
    let bpp = format.bytes_per_pixel();
    let row_bytes = (width as usize)
        .checked_mul(bpp)
        .ok_or(ViewError::LimitExceeded)?;
    if stride_bytes < row_bytes {
        return Err(ViewError::InvalidRecord);
    }
    let last_row_offset = (height as usize - 1)
        .checked_mul(stride_bytes)
        .ok_or(ViewError::LimitExceeded)?;
    let required = last_row_offset
        .checked_add(row_bytes)
        .ok_or(ViewError::LimitExceeded)?;
    if src.len() < required {
        return Err(ViewError::InvalidRecord);
    }

    let out_bytes = pixels.checked_mul(4).ok_or(ViewError::LimitExceeded)?;
    dst.clear();
    dst.resize(out_bytes, 0);

    match format {
        PixelFormat::Bgra32 => {
            for (y, dst_row) in dst.chunks_exact_mut(width as usize * 4).enumerate() {
                let s0 = y * stride_bytes;
                dst_row.copy_from_slice(&src[s0..s0 + row_bytes]);
                #[allow(clippy::chunks_exact_to_as_chunks)]
                for px in dst_row.chunks_exact_mut(4) {
                    px[3] = 255;
                }
            }
        }
        PixelFormat::Bgr24 => {
            for (y, dst_row) in dst.chunks_exact_mut(width as usize * 4).enumerate() {
                let s0 = y * stride_bytes;
                let src_row = &src[s0..s0 + row_bytes];
                #[allow(clippy::chunks_exact_to_as_chunks)]
                for (sp, dp) in src_row.chunks_exact(3).zip(dst_row.chunks_exact_mut(4)) {
                    dp[0] = sp[0];
                    dp[1] = sp[1];
                    dp[2] = sp[2];
                    dp[3] = 255;
                }
            }
        }
        PixelFormat::Rgb565Le | PixelFormat::Rgb565Be => {
            let big = format == PixelFormat::Rgb565Be;
            for (y, dst_row) in dst.chunks_exact_mut(width as usize * 4).enumerate() {
                let s0 = y * stride_bytes;
                let src_row = &src[s0..s0 + row_bytes];
                #[allow(clippy::chunks_exact_to_as_chunks)]
                for (sp, dp) in src_row.chunks_exact(2).zip(dst_row.chunks_exact_mut(4)) {
                    let v = if big {
                        u16::from_be_bytes([sp[0], sp[1]])
                    } else {
                        u16::from_le_bytes([sp[0], sp[1]])
                    };
                    let r5 = ((v >> 11) & 0x1F) as u8;
                    let g6 = ((v >> 5) & 0x3F) as u8;
                    let b5 = (v & 0x1F) as u8;
                    dp[0] = (b5 << 3) | (b5 >> 2);
                    dp[1] = (g6 << 2) | (g6 >> 4);
                    dp[2] = (r5 << 3) | (r5 >> 2);
                    dp[3] = 255;
                }
            }
        }
    }
    Ok(())
}

/// Compute aspect-preserving output dimensions capped to both `EffectiveProfile`
/// and the protocol [`MAX_OUTPUT_WIDTH`] x [`MAX_OUTPUT_HEIGHT`] (`1920x1080`).
///
/// # Errors
///
/// Returns [`ViewError::LimitExceeded`] or [`ViewError::InvalidRecord`] on zero inputs.
pub fn compute_target_dimensions(
    src_w: u32,
    src_h: u32,
    max_w: u32,
    max_h: u32,
) -> Result<(u16, u16), ViewError> {
    if src_w == 0 || src_h == 0 || max_w == 0 || max_h == 0 {
        return Err(ViewError::InvalidRecord);
    }
    // A portrait screen uses the box turned on its side (1080x1920), so its long edge is
    // not cut to the short side of the landscape box.
    let (cap_w, cap_h) = if src_h > src_w {
        (
            u64::from(max_h.min(MAX_OUTPUT_HEIGHT)),
            u64::from(max_w.min(MAX_OUTPUT_WIDTH)),
        )
    } else {
        (
            u64::from(max_w.min(MAX_OUTPUT_WIDTH)),
            u64::from(max_h.min(MAX_OUTPUT_HEIGHT)),
        )
    };
    let sw = u64::from(src_w);
    let sh = u64::from(src_h);
    let (out_w, out_h) = if sw <= cap_w && sh <= cap_h {
        (sw, sh)
    } else if cap_w * sh <= cap_h * sw {
        // Width-limited: preserve aspect ratio cleanly.
        let h = ((cap_w * sh) / sw).max(1).min(cap_h);
        (cap_w, h)
    } else {
        // Height-limited: preserve aspect ratio cleanly.
        let w = ((cap_h * sw) / sh).max(1).min(cap_w);
        (w, cap_h)
    };
    let w16 = u16::try_from(out_w).map_err(|_| ViewError::LimitExceeded)?;
    let h16 = u16::try_from(out_h).map_err(|_| ViewError::LimitExceeded)?;
    Ok((w16, h16))
}

/// Fast deterministic content-free FNV-1a 64-bit hash of tightly packed BGRA pixels.
#[must_use]
pub fn bgra_fingerprint(bgra: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = OFFSET;
    // Process in 8-byte words when aligned/available for speed while covering all bytes.
    let (chunks, tail) = bgra.as_chunks::<8>();
    for chunk in chunks {
        let word = u64::from_le_bytes(*chunk);
        h ^= word;
        h = h.wrapping_mul(PRIME);
    }
    for &b in tail {
        h ^= u64::from(b);
        h = h.wrapping_mul(PRIME);
    }
    h
}

/// Resampling kernel used when a frame has to be reduced to fit the output cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizeFilter {
    /// Cheaper 4-lobe cubic, used while the screen is moving.
    CatmullRom,
    /// Sharper 6-lobe sinc window that keeps small text legible; used for settled frames.
    Lanczos3,
}

impl ResizeFilter {
    /// Kernel half-width in source-pixel units at scale 1.
    const fn radius(self) -> f64 {
        match self {
            Self::CatmullRom => 2.0,
            Self::Lanczos3 => 3.0,
        }
    }

    fn weight(self, x: f64) -> f64 {
        let x = x.abs();
        match self {
            Self::CatmullRom => {
                if x < 1.0 {
                    1.5 * x * x * x - 2.5 * x * x + 1.0
                } else if x < 2.0 {
                    -0.5 * x * x * x + 2.5 * x * x - 4.0 * x + 2.0
                } else {
                    0.0
                }
            }
            Self::Lanczos3 => {
                if x < 1e-9 {
                    1.0
                } else if x < 3.0 {
                    let px = std::f64::consts::PI * x;
                    3.0 * px.sin() * (px / 3.0).sin() / (px * px)
                } else {
                    0.0
                }
            }
        }
    }
}

/// Fixed-point (14-bit) filter taps for one axis: `taps` source indices and weights per output.
struct AxisTaps {
    taps: usize,
    index: Vec<usize>,
    weight: Vec<i32>,
}

const TAP_SHIFT: u32 = 14;

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_possible_wrap
)]
fn axis_taps(src: usize, dst: usize, filter: ResizeFilter) -> AxisTaps {
    let scale = src as f64 / dst as f64;
    let filter_scale = scale.max(1.0);
    let radius = filter.radius() * filter_scale;
    let taps = (radius * 2.0).ceil() as usize + 1;
    let one = 1_i32 << TAP_SHIFT;
    let mut index = Vec::with_capacity(dst * taps);
    let mut weight = Vec::with_capacity(dst * taps);
    for d in 0..dst {
        let center = (d as f64 + 0.5) * scale;
        let left = (center - radius).floor() as isize;
        let raw: Vec<f64> = (0..taps)
            .map(|t| {
                let x = (left + t as isize) as f64 + 0.5;
                filter.weight((x - center) / filter_scale)
            })
            .collect();
        let sum: f64 = raw.iter().sum();
        let mut fixed: Vec<i32> = raw
            .iter()
            .map(|w| (w / sum * f64::from(one)).round() as i32)
            .collect();
        // Make the taps sum to exactly 1.0 so flat areas stay exactly flat.
        let drift = one - fixed.iter().sum::<i32>();
        if let Some(peak) = (0..taps).max_by_key(|&t| fixed[t]) {
            fixed[peak] += drift;
        }
        for (t, w) in fixed.into_iter().enumerate() {
            let x = left + t as isize;
            index.push(x.clamp(0, src as isize - 1) as usize);
            weight.push(w);
        }
    }
    AxisTaps {
        taps,
        index,
        weight,
    }
}

#[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
fn clamp_fixed(acc: i32) -> u8 {
    let v = (acc + (1 << (TAP_SHIFT - 1))) >> TAP_SHIFT;
    v.clamp(0, 255) as u8
}

/// Resize tightly packed top-down `BGRA` pixels into `dst` with a separable
/// two-pass filter (area-aware: the kernel widens with the reduction factor so
/// every source pixel contributes, which keeps thin text strokes intact).
///
/// # Panics
///
/// Panics only if caller supplies `src` shorter than `src_w * src_h * 4` or zero dimensions.
pub fn resize_bgra_into(
    src: &[u8],
    src_w: u32,
    src_h: u32,
    dst_w: u16,
    dst_h: u16,
    filter: ResizeFilter,
    dst: &mut Vec<u8>,
) {
    let dw = usize::from(dst_w);
    let dh = usize::from(dst_h);
    let sw = src_w as usize;
    let sh = src_h as usize;
    let needed_src = sw * sh * 4;
    assert!(src.len() >= needed_src && sw > 0 && sh > 0 && dw > 0 && dh > 0);

    dst.clear();
    if sw == dw && sh == dh {
        dst.extend_from_slice(&src[..needed_src]);
        return;
    }

    // Horizontal pass: `sh` rows of `dw` pixels.
    let hx = axis_taps(sw, dw, filter);
    let mut mid = vec![0u8; sh * dw * 4];
    for y in 0..sh {
        let row = &src[y * sw * 4..(y + 1) * sw * 4];
        let out = &mut mid[y * dw * 4..(y + 1) * dw * 4];
        for dx in 0..dw {
            let idx = &hx.index[dx * hx.taps..(dx + 1) * hx.taps];
            let wts = &hx.weight[dx * hx.taps..(dx + 1) * hx.taps];
            let mut acc = [0i32; 3];
            for (&sx, &w) in idx.iter().zip(wts) {
                let p = &row[sx * 4..sx * 4 + 3];
                acc[0] += i32::from(p[0]) * w;
                acc[1] += i32::from(p[1]) * w;
                acc[2] += i32::from(p[2]) * w;
            }
            let o = &mut out[dx * 4..dx * 4 + 4];
            o[0] = clamp_fixed(acc[0]);
            o[1] = clamp_fixed(acc[1]);
            o[2] = clamp_fixed(acc[2]);
            o[3] = 255;
        }
    }

    // Vertical pass: `dh` rows of `dw` pixels.
    let vy = axis_taps(sh, dh, filter);
    dst.resize(dw * dh * 4, 0);
    let stride = dw * 4;
    for dy in 0..dh {
        let idx = &vy.index[dy * vy.taps..(dy + 1) * vy.taps];
        let wts = &vy.weight[dy * vy.taps..(dy + 1) * vy.taps];
        let out = &mut dst[dy * stride..(dy + 1) * stride];
        for x in 0..dw {
            let mut acc = [0i32; 3];
            for (&sy, &w) in idx.iter().zip(wts) {
                let p = &mid[sy * stride + x * 4..sy * stride + x * 4 + 3];
                acc[0] += i32::from(p[0]) * w;
                acc[1] += i32::from(p[1]) * w;
                acc[2] += i32::from(p[2]) * w;
            }
            let o = &mut out[x * 4..x * 4 + 4];
            o[0] = clamp_fixed(acc[0]);
            o[1] = clamp_fixed(acc[1]);
            o[2] = clamp_fixed(acc[2]);
            o[3] = 255;
        }
    }
}

/// The encoder never goes below this quality to fit a byte budget (it drops chroma
/// resolution first and, as a last resort for pathological content, the size); a slow link
/// lowers the frame rate instead.
pub const QUALITY_FLOOR: u8 = 55;
/// JPEG quality of the static refinement frame when it is not lossless WebP.
pub const REFINE_JPEG_QUALITY: u8 = 92;
/// Below this requested quality the client asked for a cheap stream (slow viewer): keep 4:2:0
/// and send no refinement frame.
const COMPACT_QUALITY_BELOW: u8 = 50;
/// How long the screen must stay unchanged before one refinement frame is sent.
pub const SETTLE_AFTER: Duration = Duration::from_millis(300);
/// Wait before retrying a refinement frame that the byte budget could not afford yet.
const REFINE_RETRY: Duration = Duration::from_secs(1);

/// JPEG parameters chosen for one frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncodeSettings {
    /// JPEG quality, 10..=95.
    pub quality: u8,
    /// Full-resolution chroma (4:4:4) instead of 4:2:0; keeps coloured text edges sharp.
    pub chroma_444: bool,
    /// Resampling kernel when the frame has to be reduced.
    pub filter: ResizeFilter,
}

/// Encoder settings for a frame that is being sent while the screen changes (pure): the
/// requested quality with full-resolution chroma. A client that asked for a compact stream
/// (quality below 50) gets exactly that, in 4:2:0.
#[must_use]
pub fn select_encode_settings(profile: &EffectiveProfile) -> EncodeSettings {
    let base = profile.quality.clamp(10, 95);
    EncodeSettings {
        quality: base,
        chroma_444: base >= COMPACT_QUALITY_BELOW,
        filter: if profile.profile == Profile::Watch {
            ResizeFilter::Lanczos3
        } else {
            ResizeFilter::CatmullRom
        },
    }
}

/// Whether a still screen gets one refinement frame (lossless WebP, or JPEG q92 4:4:4 when the
/// lossless image is too big for the burst) (pure).
#[must_use]
pub fn refine_enabled(profile: &EffectiveProfile) -> bool {
    profile.quality >= COMPACT_QUALITY_BELOW
}

fn encode_jpeg_once(
    pixels: &[u8],
    w: u16,
    h: u16,
    quality: u8,
    chroma_444: bool,
    out: &mut Vec<u8>,
) -> Result<(), ViewError> {
    out.clear();
    let mut enc = JpegEncoder::new(&mut *out, quality);
    enc.set_sampling_factor(if chroma_444 {
        SamplingFactor::F_1_1
    } else {
        SamplingFactor::F_2_2
    });
    enc.set_optimized_huffman_tables(chroma_444);
    enc.encode(pixels, w, h, JpegColorType::Bgra)
        .map_err(|_| ViewError::InvalidRecord)
}

/// Resize (if needed) and JPEG-encode tightly packed `BGRA` pixels completely in RAM,
/// stepping chroma, quality and dimensions down if necessary so the payload + [`MAX_HEADER`]
/// never exceeds `profile.burst_bytes` or [`MAX_PAYLOAD`].
///
/// # Errors
///
/// Returns [`ViewError::LimitExceeded`] if even minimal scale/quality exceeds the
/// profile's burst allowance, or [`ViewError::InvalidRecord`] on zero inputs.
pub fn encode_jpeg_within_budget(
    bgra: &[u8],
    src_w: u32,
    src_h: u32,
    profile: &EffectiveProfile,
    settings: EncodeSettings,
    scratch_scaled: &mut Vec<u8>,
    out_jpeg: &mut Vec<u8>,
) -> Result<(u32, u32), ViewError> {
    validate_raw_dimensions(src_w, src_h)?;
    let needed = (src_w as usize) * (src_h as usize) * 4;
    if bgra.len() < needed {
        return Err(ViewError::InvalidRecord);
    }
    let burst_cap = usize::try_from(profile.burst_bytes).unwrap_or(usize::MAX);
    let max_payload = burst_cap.saturating_sub(MAX_HEADER).min(MAX_PAYLOAD);
    if max_payload == 0 {
        return Err(ViewError::LimitExceeded);
    }

    let (mut cur_w, mut cur_h) =
        compute_target_dimensions(src_w, src_h, profile.max_width, profile.max_height)?;
    let mut quality = settings.quality.clamp(10, 95);
    let floor = QUALITY_FLOOR.min(quality);
    let mut chroma_444 = settings.chroma_444;
    let mut scaled_for: Option<(u16, u16)> = None;

    loop {
        let pixel_slice: &[u8] = if scaled_for == Some((cur_w, cur_h)) {
            // The resampled image does not depend on quality: reuse it across retries.
            scratch_scaled.as_slice()
        } else if src_w == u32::from(cur_w) && src_h == u32::from(cur_h) {
            &bgra[..needed]
        } else {
            resize_bgra_into(
                bgra,
                src_w,
                src_h,
                cur_w,
                cur_h,
                settings.filter,
                scratch_scaled,
            );
            scaled_for = Some((cur_w, cur_h));
            scratch_scaled.as_slice()
        };
        encode_jpeg_once(pixel_slice, cur_w, cur_h, quality, chroma_444, out_jpeg)?;

        if !out_jpeg.is_empty() && out_jpeg.len() <= max_payload {
            return Ok((u32::from(cur_w), u32::from(cur_h)));
        }

        // Over budget: drop chroma resolution first (cheapest loss), then quality down to the
        // floor, and only then (pathological content, tiny burst) the size.
        if chroma_444 {
            chroma_444 = false;
            continue;
        }
        if quality > floor {
            quality = (quality / 5 * 4).max(floor);
            continue;
        }

        let next_w = u32::from(cur_w) * 3 / 4;
        let next_h = u32::from(cur_h) * 3 / 4;
        if next_w < MIN_BUDGET_DIM && next_h < MIN_BUDGET_DIM {
            return Err(ViewError::LimitExceeded);
        }
        let Ok(nw) = u16::try_from(next_w.max(1)) else {
            return Err(ViewError::LimitExceeded);
        };
        let Ok(nh) = u16::try_from(next_h.max(1)) else {
            return Err(ViewError::LimitExceeded);
        };
        if nw == cur_w && nh == cur_h {
            return Err(ViewError::LimitExceeded);
        }
        // Shrink from the picture already at hand (cheap), not from the full-size source.
        let mut next = Vec::new();
        {
            let (src_px, sw, sh): (&[u8], u32, u32) = if scaled_for == Some((cur_w, cur_h)) {
                (
                    scratch_scaled.as_slice(),
                    u32::from(cur_w),
                    u32::from(cur_h),
                )
            } else {
                (&bgra[..needed], src_w, src_h)
            };
            resize_bgra_into(src_px, sw, sh, nw, nh, settings.filter, &mut next);
        }
        *scratch_scaled = next;
        scaled_for = Some((nw, nh));
        cur_w = nw;
        cur_h = nh;
        quality = floor;
    }
}

/// Encode the settled screen losslessly (WebP, in RAM) at the output size; when that is
/// bigger than the burst allows, fall back to JPEG q92 4:4:4.
///
/// # Errors
///
/// Same as [`encode_jpeg_within_budget`].
pub fn encode_refined(
    bgra: &[u8],
    src_w: u32,
    src_h: u32,
    profile: &EffectiveProfile,
    scratch_scaled: &mut Vec<u8>,
    scratch_rgb: &mut Vec<u8>,
    out: &mut Vec<u8>,
) -> Result<(Codec, u32, u32), ViewError> {
    validate_raw_dimensions(src_w, src_h)?;
    let needed = (src_w as usize) * (src_h as usize) * 4;
    if bgra.len() < needed {
        return Err(ViewError::InvalidRecord);
    }
    let burst_cap = usize::try_from(profile.burst_bytes).unwrap_or(usize::MAX);
    let max_payload = burst_cap.saturating_sub(MAX_HEADER).min(MAX_PAYLOAD);
    let (w, h) = compute_target_dimensions(src_w, src_h, profile.max_width, profile.max_height)?;
    let pixels: &[u8] = if src_w == u32::from(w) && src_h == u32::from(h) {
        &bgra[..needed]
    } else {
        resize_bgra_into(
            bgra,
            src_w,
            src_h,
            w,
            h,
            ResizeFilter::Lanczos3,
            scratch_scaled,
        );
        scratch_scaled.as_slice()
    };
    scratch_rgb.clear();
    scratch_rgb.reserve(usize::from(w) * usize::from(h) * 3);
    for px in pixels.as_chunks::<4>().0 {
        scratch_rgb.extend_from_slice(&[px[2], px[1], px[0]]);
    }
    out.clear();
    let lossless = image_webp::WebPEncoder::new(&mut *out).encode(
        scratch_rgb,
        u32::from(w),
        u32::from(h),
        image_webp::ColorType::Rgb8,
    );
    if lossless.is_ok() && !out.is_empty() && out.len() <= max_payload {
        return Ok((Codec::Webp, u32::from(w), u32::from(h)));
    }
    let settings = EncodeSettings {
        quality: REFINE_JPEG_QUALITY,
        chroma_444: true,
        filter: ResizeFilter::Lanczos3,
    };
    let (jw, jh) =
        encode_jpeg_within_budget(bgra, src_w, src_h, profile, settings, scratch_scaled, out)?;
    Ok((Codec::Jpeg, jw, jh))
}

// ── Content-free stats ────────────────────────────────────────────────────────

/// Content-free observability counters (never records pixel bytes or input strings).
#[derive(Default)]
pub struct LiveViewStats {
    pub frames_captured: AtomicU64,
    pub frames_encoded: AtomicU64,
    pub frames_repeated: AtomicU64,
    pub frames_skipped_unchanged: AtomicU64,
    pub frames_skipped_budget: AtomicU64,
    pub capture_errors: AtomicU64,
    pub generation_bumps: AtomicU64,
    pub geometry_changes: AtomicU64,
    pub input_events: AtomicU64,
    pub input_cancelled: AtomicU64,
    pub input_partial_failures: AtomicU64,
    pub input_resets: AtomicU64,
    last_reason: Mutex<Option<Reason>>,
}

/// Point-in-time copy of [`LiveViewStats`].
#[derive(Debug, Clone, PartialEq)]
pub struct LiveViewStatsSnapshot {
    pub frames_captured: u64,
    pub frames_encoded: u64,
    pub frames_repeated: u64,
    pub frames_skipped_unchanged: u64,
    pub frames_skipped_budget: u64,
    pub capture_errors: u64,
    pub generation_bumps: u64,
    pub geometry_changes: u64,
    pub input_events: u64,
    pub input_cancelled: u64,
    pub input_partial_failures: u64,
    pub input_resets: u64,
    pub last_unavailable_reason: Option<Reason>,
}

impl LiveViewStats {
    fn set_reason(&self, reason: Option<Reason>) {
        *self.last_reason.lock() = reason;
    }

    /// Take a consistent snapshot of all counters and the last reason code.
    #[must_use]
    pub fn snapshot(&self) -> LiveViewStatsSnapshot {
        LiveViewStatsSnapshot {
            frames_captured: self.frames_captured.load(Ordering::Relaxed),
            frames_encoded: self.frames_encoded.load(Ordering::Relaxed),
            frames_repeated: self.frames_repeated.load(Ordering::Relaxed),
            frames_skipped_unchanged: self.frames_skipped_unchanged.load(Ordering::Relaxed),
            frames_skipped_budget: self.frames_skipped_budget.load(Ordering::Relaxed),
            capture_errors: self.capture_errors.load(Ordering::Relaxed),
            generation_bumps: self.generation_bumps.load(Ordering::Relaxed),
            geometry_changes: self.geometry_changes.load(Ordering::Relaxed),
            input_events: self.input_events.load(Ordering::Relaxed),
            input_cancelled: self.input_cancelled.load(Ordering::Relaxed),
            input_partial_failures: self.input_partial_failures.load(Ordering::Relaxed),
            input_resets: self.input_resets.load(Ordering::Relaxed),
            last_unavailable_reason: self.last_reason.lock().clone(),
        }
    }
}

// ── OS Driver Traits ──────────────────────────────────────────────────────────

/// Metadata returned with each captured raw pixel buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapturedRawInfo {
    pub geometry: GeometrySnapshot,
    pub format: PixelFormat,
    pub stride_bytes: usize,
}

/// Typed capture error returned by a [`CaptureDriver`].
#[derive(Debug, Clone, PartialEq)]
pub enum CaptureFailure {
    /// Display or OS session temporarily unavailable (locked, UAC, Winlogon, unreachable).
    Unavailable(Reason),
    /// Session/extension unsupported (e.g. Wayland).
    Unsupported(Reason),
    /// Requested dimensions exceeded limits before allocating.
    LimitExceeded,
    /// Persistent OS display connection broke and must be reopened (bumps generation on recovery).
    ConnectionLost(Reason),
}

impl CaptureFailure {
    const fn to_view_error(&self) -> ViewError {
        match self {
            Self::Unavailable(_) | Self::ConnectionLost(_) => ViewError::Unavailable,
            Self::Unsupported(_) => ViewError::Unsupported,
            Self::LimitExceeded => ViewError::LimitExceeded,
        }
    }

    fn reason(&self) -> Reason {
        match self {
            Self::Unavailable(r) | Self::Unsupported(r) | Self::ConnectionLost(r) => r.clone(),
            Self::LimitExceeded => Reason::FrameBudgetExceeded,
        }
    }
}

/// Platform capture implementation invoked exclusively on a dedicated worker thread.
pub trait CaptureDriver: Send + 'static {
    /// Capture current frame into `raw_buf`. Implementations must check raw
    /// pixel count <= [`MAX_RAW_PIXELS`] BEFORE resizing `raw_buf`.
    ///
    /// # Errors
    ///
    /// Returns [`CaptureFailure`] describing why capture failed.
    fn capture(&mut self, raw_buf: &mut Vec<u8>) -> Result<CapturedRawInfo, CaptureFailure>;
}

/// Platform input implementation invoked exclusively on a dedicated input worker thread.
pub trait InputDriver: Send + 'static {
    /// Inject a single validated [`InputEvent`].
    ///
    /// Implementations should observe `cancelled.load(Ordering::Acquire)` before
    /// executing multi-step injections, maintain held-key/button tracking, and
    /// return [`ViewError::PartialInjection`] when an injection is partially applied.
    ///
    /// # Errors
    ///
    /// Returns a fixed [`ViewError`] when unsupported, unavailable, invalid, or partially applied.
    fn inject(
        &mut self,
        event: &InputEvent,
        source_rect: SourceRect,
        virtual_rect: PixelRect,
        cancelled: &AtomicBool,
    ) -> Result<(), ViewError>;

    /// Release only buttons/keys/temporary bindings acquired by this driver session.
    /// Must be idempotent and leave local user physical key state intact where feasible.
    ///
    /// # Errors
    ///
    /// Returns [`ViewError::Unavailable`] if the OS connection is unusable during reset.
    fn release_all(&mut self) -> Result<(), ViewError>;

    /// Returns `true` if the underlying OS input connection has been permanently lost
    /// (for example, an X11 socket disconnect or server restart) and the driver should be
    /// recreated via the [`InputDriverFactory`].
    fn is_connection_lost(&self) -> bool {
        false
    }
}

/// Factory creating a fresh [`CaptureDriver`] bound to a capture scope.
pub type CaptureDriverFactory =
    Arc<dyn Fn(&CaptureScope) -> Result<Box<dyn CaptureDriver>, CaptureFailure> + Send + Sync>;

/// Optional lightweight availability probe run when [`DesktopViewTarget::refresh_availability`] is queried.
pub type AvailabilityProbeFn = Arc<dyn Fn(&CaptureScope) -> Result<(), Reason> + Send + Sync>;

/// Factory creating a fresh [`InputDriver`].
pub type InputDriverFactory =
    Arc<dyn Fn() -> Result<Box<dyn InputDriver>, ViewError> + Send + Sync>;

// ── Shared DesktopViewTarget runtime ──────────────────────────────────────────

// Each flag tracks an orthogonal worker lifecycle condition cleanly under one lock.
#[allow(clippy::struct_excessive_bools)]
struct CaptureControl {
    subscribers: usize,
    paused: bool,
    stop_requested: bool,
    shutdown: bool,
    running: bool,
    profile: EffectiveProfile,
    profile_dirty: bool,
    pending_refresh: Option<u64>,
    exit_waiters: Vec<oneshot::Sender<()>>,
    join: Option<std::thread::JoinHandle<()>>,
}

struct TargetStatus {
    generation: u64,
    geometry_revision: u64,
    view_available: bool,
    input_available: bool,
    input_reason: Option<Reason>,
    unavailable_reason: Option<Reason>,
    latest_geometry: GeometrySnapshot,
}

enum InputJob {
    Inject {
        epoch: u64,
        generation: u64,
        geometry_revision: u64,
        cancelled: Arc<AtomicBool>,
        event: InputEvent,
        source_rect: SourceRect,
        virtual_rect: PixelRect,
        reply: oneshot::Sender<Result<(), ViewError>>,
    },
    ReleaseAll {
        reply: oneshot::Sender<Result<(), ViewError>>,
    },
    Shutdown,
}

struct InputWorker {
    tx: std::sync::mpsc::Sender<InputJob>,
    join: Option<std::thread::JoinHandle<()>>,
}

struct ProbeControl {
    shutdown: bool,
    trigger_now: bool,
    in_flight: bool,
    in_flight_started_at: Option<Instant>,
    interval: Duration,
    timeout: Duration,
    last_probe_at: Option<Instant>,
    join: Option<std::thread::JoinHandle<()>>,
    call_join: Option<std::thread::JoinHandle<()>>,
}

struct ProbeSync {
    ctrl: Mutex<ProbeControl>,
    cvar: Condvar,
}

struct TargetCore {
    target_id: Uuid,
    options: LiveViewOptions,
    capture_factory: CaptureDriverFactory,
    probe: AvailabilityProbeFn,
    input_factory: Option<InputDriverFactory>,
    stats: Arc<LiveViewStats>,
    status: Mutex<TargetStatus>,
    last_published_geometry_revision: AtomicU64,
    require_explicit_revision: AtomicBool,
    ctrl: Mutex<CaptureControl>,
    cvar: Condvar,
    frames_tx: watch::Sender<Option<Arc<Frame>>>,
    takeover_active: AtomicBool,
    input_epoch: AtomicU64,
    active_input_cancels: Mutex<Vec<Weak<AtomicBool>>>,
    input_worker: Mutex<Option<InputWorker>>,
    unclean_input_reset: AtomicBool,
    probe_sync: Arc<ProbeSync>,
    #[cfg(test)]
    pre_finish_hook: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl TargetCore {
    fn register_input_cancel(&self, cancelled: &Arc<AtomicBool>) {
        let mut guard = self.active_input_cancels.lock();
        guard.retain(|w| w.strong_count() > 0);
        guard.push(Arc::downgrade(cancelled));
    }

    fn cancel_in_flight_input(&self) {
        let mut guard = self.active_input_cancels.lock();
        for weak in guard.drain(..) {
            if let Some(flag) = weak.upgrade() {
                flag.store(true, Ordering::Release);
            }
        }
    }

    fn request_availability_probe(&self) {
        let mut p = self.probe_sync.ctrl.lock();
        if !p.shutdown {
            p.trigger_now = true;
            self.probe_sync.cvar.notify_all();
        }
    }

    fn notify_input_unavailable(&self) {
        {
            let mut ctrl = self.ctrl.lock();
            if ctrl.running {
                ctrl.pending_refresh.get_or_insert(0);
                self.cvar.notify_all();
                return;
            }
        }
        self.request_availability_probe();
    }

    fn apply_probe_result(&self, probe_res: Result<(), Reason>) {
        let running = self.ctrl.lock().running;
        if !running {
            let mut st = self.status.lock();
            match probe_res {
                Ok(()) => {
                    st.view_available = true;
                    st.unavailable_reason = None;
                    self.stats.set_reason(None);
                }
                Err(reason) => {
                    st.view_available = false;
                    st.unavailable_reason = Some(reason.clone());
                    self.stats.set_reason(Some(reason));
                }
            }
        }
        let mut p = self.probe_sync.ctrl.lock();
        p.in_flight = false;
        p.in_flight_started_at = None;
        p.last_probe_at = Some(Instant::now());
        self.probe_sync.cvar.notify_all();
    }

    fn shutdown_workers(&self) {
        self.takeover_active.store(false, Ordering::SeqCst);
        self.input_epoch.fetch_add(1, Ordering::SeqCst);
        self.cancel_in_flight_input();
        let probe_join = {
            let mut p = self.probe_sync.ctrl.lock();
            p.shutdown = true;
            self.probe_sync.cvar.notify_all();
            p.join.take()
        };
        if let Some(j) = probe_join
            && j.thread().id() != std::thread::current().id()
        {
            let _ = j.join();
        }
        let call_join = {
            let mut p = self.probe_sync.ctrl.lock();
            let wait_bound = p
                .timeout
                .saturating_mul(4)
                .clamp(Duration::from_millis(150), Duration::from_secs(3));
            let deadline = Instant::now() + wait_bound;
            while p.in_flight {
                if p.call_join
                    .as_ref()
                    .is_some_and(std::thread::JoinHandle::is_finished)
                {
                    p.in_flight = false;
                    p.in_flight_started_at = None;
                    break;
                }
                let rem = deadline.saturating_duration_since(Instant::now());
                if rem.is_zero() {
                    break;
                }
                let _ = self.probe_sync.cvar.wait_for(&mut p, rem);
            }
            if !p.in_flight
                || p.call_join
                    .as_ref()
                    .is_some_and(std::thread::JoinHandle::is_finished)
            {
                p.call_join.take()
            } else {
                None
            }
        };
        if let Some(j) = call_join
            && j.thread().id() != std::thread::current().id()
        {
            let _ = j.join();
        }
        let cap_join = {
            let mut ctrl = self.ctrl.lock();
            ctrl.shutdown = true;
            ctrl.stop_requested = true;
            self.cvar.notify_all();
            ctrl.join.take()
        };
        if let Some(j) = cap_join
            && j.thread().id() != std::thread::current().id()
        {
            let _ = j.join();
        }
        let in_worker = self.input_worker.lock().take();
        if let Some(w) = in_worker {
            let InputWorker { tx, join } = w;
            let _ = tx.send(InputJob::Shutdown);
            drop(tx);
            if let Some(j) = join
                && j.thread().id() != std::thread::current().id()
            {
                let _ = j.join();
            }
        }
    }
}

struct ProbeCallFinishGuard {
    weak: Weak<TargetCore>,
    probe_sync: Arc<ProbeSync>,
    finished: bool,
}

fn finish_probe_call(weak: &Weak<TargetCore>, probe_sync: &ProbeSync, res: Result<(), Reason>) {
    if let Some(c) = weak.upgrade() {
        c.apply_probe_result(res);
    } else {
        let mut p = probe_sync.ctrl.lock();
        p.in_flight = false;
        p.in_flight_started_at = None;
        p.last_probe_at = Some(Instant::now());
        probe_sync.cvar.notify_all();
    }
}

impl Drop for ProbeCallFinishGuard {
    fn drop(&mut self) {
        if !self.finished {
            finish_probe_call(&self.weak, &self.probe_sync, Err(Reason::Unavailable));
        }
    }
}

fn mark_probe_timed_out_unavailable(core_weak: &Weak<TargetCore>) {
    if let Some(core) = core_weak.upgrade()
        && !core.ctrl.lock().running
    {
        let mut st = core.status.lock();
        st.view_available = false;
        st.unavailable_reason = Some(Reason::Unavailable);
        core.stats.set_reason(Some(Reason::Unavailable));
    }
}

fn next_probe_call_args(
    core_weak: &Weak<TargetCore>,
    probe_sync: &ProbeSync,
) -> Option<(AvailabilityProbeFn, CaptureScope)> {
    let mut p = probe_sync.ctrl.lock();
    loop {
        if p.shutdown || core_weak.strong_count() == 0 {
            return None;
        }
        if p.in_flight {
            if p.call_join
                .as_ref()
                .is_some_and(std::thread::JoinHandle::is_finished)
            {
                let finished = p.call_join.take();
                p.in_flight = false;
                p.in_flight_started_at = None;
                p.last_probe_at.get_or_insert_with(Instant::now);
                drop(p);
                if let Some(j) = finished {
                    let _ = j.join();
                }
                p = probe_sync.ctrl.lock();
                continue;
            }
            let timed_out = p
                .in_flight_started_at
                .is_some_and(|t| t.elapsed() >= p.timeout);
            if timed_out {
                let wait_for = p.interval.min(Duration::from_millis(200));
                drop(p);
                mark_probe_timed_out_unavailable(core_weak);
                p = probe_sync.ctrl.lock();
                if p.shutdown || core_weak.strong_count() == 0 {
                    return None;
                }
                if p.in_flight
                    && !p
                        .call_join
                        .as_ref()
                        .is_some_and(std::thread::JoinHandle::is_finished)
                {
                    let _ = probe_sync.cvar.wait_for(&mut p, wait_for);
                }
            } else {
                let rem = p
                    .in_flight_started_at
                    .map_or(p.timeout, |t| p.timeout.saturating_sub(t.elapsed()));
                let _ = probe_sync.cvar.wait_for(&mut p, rem);
            }
            continue;
        }

        if let Some(prev) = p.call_join.take() {
            drop(p);
            let _ = prev.join();
            p = probe_sync.ctrl.lock();
            continue;
        }

        let due = p.trigger_now || p.last_probe_at.is_none_or(|t| t.elapsed() >= p.interval);
        if due {
            drop(p);
            let core = core_weak.upgrade()?;
            let running = core.ctrl.lock().running;
            let probe_fn = Arc::clone(&core.probe);
            let scope = core.options.scope.clone();
            drop(core);

            p = probe_sync.ctrl.lock();
            if p.shutdown || core_weak.strong_count() == 0 {
                return None;
            }
            if p.in_flight {
                continue;
            }
            if running {
                p.trigger_now = false;
                p.last_probe_at = Some(Instant::now());
                continue;
            }
            p.trigger_now = false;
            p.in_flight = true;
            p.in_flight_started_at = Some(Instant::now());
            return Some((probe_fn, scope));
        }

        let wait_for = p.last_probe_at.map_or(p.interval, |t| {
            (t + p.interval).saturating_duration_since(Instant::now())
        });
        let _ = probe_sync.cvar.wait_for(&mut p, wait_for);
    }
}

fn spawn_probe_call(
    core_weak: &Weak<TargetCore>,
    probe_sync: &ProbeSync,
    probe: AvailabilityProbeFn,
    scope: CaptureScope,
) {
    let Some(core) = core_weak.upgrade() else {
        finish_probe_call(core_weak, probe_sync, Err(Reason::Unavailable));
        return;
    };
    let ps = Arc::clone(&core.probe_sync);
    drop(core);

    let weak = Weak::clone(core_weak);
    let spawned = std::thread::Builder::new()
        .name("live-view-probe-call".into())
        .spawn(move || {
            let mut guard = ProbeCallFinishGuard {
                weak: Weak::clone(&weak),
                probe_sync: Arc::clone(&ps),
                finished: false,
            };
            let res = (probe)(&scope);
            guard.finished = true;
            finish_probe_call(&weak, &ps, res);
        });
    match spawned {
        Ok(handle) => {
            probe_sync.ctrl.lock().call_join = Some(handle);
        }
        Err(_) => {
            finish_probe_call(core_weak, probe_sync, Err(Reason::Unavailable));
        }
    }
}

fn run_availability_probe_loop(core_weak: &Weak<TargetCore>, probe_sync: &ProbeSync) {
    // Hold no strong `Arc<TargetCore>` across condvar waits so `TargetCore` ownership
    // stays strictly with external callers (`DesktopViewTarget`, `FrameSource`, `DesktopInputSink`).
    while let Some((probe, scope)) = next_probe_call_args(core_weak, probe_sync) {
        spawn_probe_call(core_weak, probe_sync, probe, scope);
    }
}

/// A target id that survives a daemon restart: the same host showing the same
/// screen (and owned by the same bot) gets the same id every time, so a
/// takeover that was persisted before a restart still points at a target that
/// exists afterwards. FNV-1a (128 bit) over the identity, stamped as a UUID.
/// Not a secret and not used for access control.
#[must_use]
pub fn stable_target_id(options: &LiveViewOptions) -> Uuid {
    const OFFSET: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;
    let identity = format!(
        "praxi-live-view-target\u{1f}{}\u{1f}{:?}\u{1f}{:?}",
        options.host_id, options.scope, options.bot_id
    );
    let mut hash = OFFSET;
    for byte in identity.bytes() {
        hash ^= u128::from(byte);
        hash = hash.wrapping_mul(PRIME);
    }
    // Version 8 (custom) and the RFC 4122 variant bits.
    let hash = (hash & !(0xf_u128 << 76) & !(0x3_u128 << 62)) | (0x8_u128 << 76) | (0x2_u128 << 62);
    Uuid::from_u128(hash)
}

/// Shared [`ViewTarget`] + [`FrameSource`] + [`InputSink`] coordinator used by OS backends.
pub struct DesktopViewTarget {
    core: Arc<TargetCore>,
}

/// Parameters constructing a [`DesktopViewTarget`].
pub struct DesktopViewTargetConfig {
    pub options: LiveViewOptions,
    pub initial_geometry: GeometrySnapshot,
    pub capture_factory: CaptureDriverFactory,
    pub probe: AvailabilityProbeFn,
    pub input_factory: Option<InputDriverFactory>,
    pub input_reason: Option<Reason>,
}

impl DesktopViewTarget {
    /// Construct a new live-view target coordinator around OS driver factories.
    #[must_use]
    pub fn new(config: DesktopViewTargetConfig) -> Self {
        let (frames_tx, _rx) = watch::channel(None);
        let input_available = config.input_factory.is_some();
        let core = Arc::new(TargetCore {
            target_id: stable_target_id(&config.options),
            options: config.options,
            capture_factory: config.capture_factory,
            probe: config.probe,
            input_factory: config.input_factory,
            stats: Arc::new(LiveViewStats::default()),
            status: Mutex::new(TargetStatus {
                generation: 1,
                geometry_revision: 1,
                view_available: true,
                input_available,
                input_reason: config.input_reason,
                unavailable_reason: None,
                latest_geometry: config.initial_geometry,
            }),
            last_published_geometry_revision: AtomicU64::new(1),
            require_explicit_revision: AtomicBool::new(true),
            ctrl: Mutex::new(CaptureControl {
                subscribers: 0,
                paused: false,
                stop_requested: false,
                shutdown: false,
                running: false,
                profile: EffectiveProfile::watch(),
                profile_dirty: false,
                pending_refresh: None,
                exit_waiters: Vec::new(),
                join: None,
            }),
            cvar: Condvar::new(),
            frames_tx,
            takeover_active: AtomicBool::new(false),
            input_epoch: AtomicU64::new(0),
            active_input_cancels: Mutex::new(Vec::new()),
            input_worker: Mutex::new(None),
            unclean_input_reset: AtomicBool::new(false),
            probe_sync: Arc::new(ProbeSync {
                ctrl: Mutex::new(ProbeControl {
                    shutdown: false,
                    trigger_now: false,
                    in_flight: false,
                    in_flight_started_at: None,
                    interval: AVAILABILITY_PROBE_INTERVAL,
                    timeout: AVAILABILITY_PROBE_TIMEOUT,
                    last_probe_at: Some(Instant::now()),
                    join: None,
                    call_join: None,
                }),
                cvar: Condvar::new(),
            }),
            #[cfg(test)]
            pre_finish_hook: Mutex::new(None),
        });
        let probe_weak = Arc::downgrade(&core);
        let probe_sync = Arc::clone(&core.probe_sync);
        if let Ok(handle) = std::thread::Builder::new()
            .name("live-view-probe".into())
            .spawn(move || run_availability_probe_loop(&probe_weak, &probe_sync))
        {
            core.probe_sync.ctrl.lock().join = Some(handle);
        }
        Self { core }
    }

    /// Snapshot current content-free observability counters.
    #[must_use]
    pub fn stats(&self) -> LiveViewStatsSnapshot {
        self.core.stats.snapshot()
    }

    /// Return the current `(generation, geometry_revision)` pair tracked by this target.
    #[must_use]
    pub fn current_revision(&self) -> (u64, u64) {
        let st = self.core.status.lock();
        (st.generation, st.geometry_revision)
    }

    /// Return the latest `geometry_revision` tracked by this target.
    #[must_use]
    pub fn geometry_revision(&self) -> u64 {
        self.core.status.lock().geometry_revision
    }

    /// Return the typed desktop [`DesktopInputSink`] handle directly so callers that
    /// carry `(generation, geometry_revision)` (daemon / UI) can invoke
    /// [`DesktopInputSink::inject_at_revision`] without passing through revision-less
    /// [`InputSink::inject`].
    #[must_use]
    pub fn desktop_input_sink(&self) -> Option<Arc<DesktopInputSink>> {
        self.core.input_factory.as_ref().map(|_| {
            Arc::new(DesktopInputSink {
                core: Arc::clone(&self.core),
            })
        })
    }

    /// Require callers to supply an explicit `(generation, geometry_revision)` via
    /// [`Self::inject_at_revision`] / [`DesktopInputSink::inject_at_revision`] for
    /// positional input events, making the legacy revision-less [`InputSink::inject`]
    /// entry point fail closed for positional clicks/wheels.
    pub fn set_require_explicit_revision(&self, required: bool) {
        self.core
            .require_explicit_revision
            .store(required, Ordering::Release);
    }

    /// Override the background availability probe interval (and wake the probe thread).
    pub fn set_availability_probe_interval(&self, interval: Duration) {
        let mut p = self.core.probe_sync.ctrl.lock();
        p.interval = interval.max(Duration::from_millis(1));
        self.core.probe_sync.cvar.notify_all();
    }

    /// Override the background availability probe timeout (and wake the probe thread).
    pub fn set_availability_probe_timeout(&self, timeout: Duration) {
        let mut p = self.core.probe_sync.ctrl.lock();
        p.timeout = timeout.max(Duration::from_millis(1));
        self.core.probe_sync.cvar.notify_all();
    }

    /// Trigger an immediate non-blocking background availability probe when idle.
    pub fn trigger_availability_probe(&self) {
        self.core.request_availability_probe();
    }

    /// Validate a client-supplied `(expected_generation, expected_geometry_revision)` against
    /// the target's active geometry and forward the input event if they match.
    ///
    /// Note: Because [`InputEvent`] in `crates/live-view` does not carry `geometry_revision`
    /// and `crates/live-view` cannot be modified from the desktop-backend card, the daemon
    /// (`c170`/`c173`) must pass the client's `(generation, geometry_revision)` through this
    /// helper (or [`DesktopInputSink::inject_at_revision`]). Both at enqueue time and on the
    /// input worker thread, positional events whose `expected_geometry_revision` differs from
    /// the backend's current `geometry_revision` are rejected even after a new frame has been
    /// published.
    ///
    /// # Errors
    ///
    /// Returns [`ViewError::Unsupported`] when the target has no input sink, or
    /// [`ViewError::Unavailable`] when `expected_generation` / `expected_geometry_revision`
    /// do not match the current target state.
    pub async fn inject_at_revision(
        &self,
        event: &InputEvent,
        expected_generation: u64,
        expected_geometry_revision: u64,
    ) -> Result<(), ViewError> {
        let Some(sink) = self.desktop_input_sink() else {
            return Err(ViewError::Unsupported);
        };
        InputSink::inject_at_revision(
            sink.as_ref(),
            event,
            expected_generation,
            expected_geometry_revision,
        )
        .await
    }

    /// Run the synchronous OS availability probe when the capture worker is idle.
    ///
    /// [`ViewTarget::info`] intentionally reads cached status without blocking the caller's
    /// thread on X11/Win32 syscalls; callers on blocking threads may invoke this method to
    /// force an offline probe.
    pub fn refresh_availability(&self) {
        let running = self.core.ctrl.lock().running;
        if !running {
            let probe_res = (self.core.probe)(&self.core.options.scope);
            let mut st = self.core.status.lock();
            match probe_res {
                Ok(()) => {
                    st.view_available = true;
                    st.unavailable_reason = None;
                    self.core.stats.set_reason(None);
                }
                Err(reason) => {
                    st.view_available = false;
                    st.unavailable_reason = Some(reason.clone());
                    self.core.stats.set_reason(Some(reason));
                }
            }
        }
    }

    /// Explicitly shut down capture and input worker threads for this target.
    pub fn shutdown(&self) {
        self.core.shutdown_workers();
    }

    /// Request an immediate Watch/Live refresh without blocking (used when bot
    /// actions mutate the desktop on the same backend session).
    pub fn notify_desktop_action(&self) {
        let mut ctrl = self.core.ctrl.lock();
        if ctrl.subscribers > 0 && !ctrl.paused {
            ctrl.pending_refresh.get_or_insert(0);
            self.core.cvar.notify_all();
        }
    }
}

impl Drop for DesktopViewTarget {
    fn drop(&mut self) {
        self.core.shutdown_workers();
    }
}

impl Drop for TargetCore {
    fn drop(&mut self) {
        self.shutdown_workers();
    }
}

#[async_trait]
impl ViewTarget for DesktopViewTarget {
    fn info(&self) -> TargetInfo {
        let info = {
            let st = self.core.status.lock();
            let available = st.view_available && st.unavailable_reason.is_none();
            TargetInfo {
                target_id: self.core.target_id,
                kind: TargetKind::Desktop,
                host_id: self.core.options.host_id.clone(),
                bot_id: self.core.options.bot_id,
                run_id: self.core.options.run_id,
                generation: st.generation,
                capabilities: LiveCapabilities {
                    view: available,
                    input: st.input_available && available,
                    codecs: vec![Codec::Jpeg, Codec::Webp],
                    reason: st
                        .unavailable_reason
                        .clone()
                        .or_else(|| st.input_reason.clone()),
                },
                unavailable_reason: st.unavailable_reason.clone(),
            }
        };
        // Non-blocking nudge for the background probe when idle and due.
        if !self.core.ctrl.lock().running {
            let mut p = self.core.probe_sync.ctrl.lock();
            let gap = p.interval.min(Duration::from_secs(1));
            if !p.shutdown
                && !p.in_flight
                && !p.trigger_now
                && p.last_probe_at.is_none_or(|t| t.elapsed() >= gap)
            {
                p.trigger_now = true;
                self.core.probe_sync.cvar.notify_one();
            }
        }
        info
    }

    fn frame_source(&self) -> Arc<dyn FrameSource> {
        Arc::new(DesktopFrameSource {
            core: Arc::clone(&self.core),
        })
    }

    fn input_sink(&self) -> Option<Arc<dyn InputSink>> {
        self.desktop_input_sink()
            .map(|sink| sink as Arc<dyn InputSink>)
    }
}

struct DesktopFrameSource {
    core: Arc<TargetCore>,
}

struct SubTicket {
    core: Arc<TargetCore>,
    armed: bool,
}

impl SubTicket {
    fn disarm(mut self) -> impl FnOnce() + Send + Sync + 'static {
        self.armed = false;
        let core = Arc::clone(&self.core);
        move || decrement_subscriber(&core)
    }
}

impl Drop for SubTicket {
    fn drop(&mut self) {
        if self.armed {
            decrement_subscriber(&self.core);
        }
    }
}

fn decrement_subscriber(core: &Arc<TargetCore>) {
    let mut ctrl = core.ctrl.lock();
    ctrl.subscribers = ctrl.subscribers.saturating_sub(1);
    if ctrl.subscribers == 0 {
        ctrl.stop_requested = true;
        let _ = core.frames_tx.send(None);
        core.cvar.notify_all();
    }
}

fn validate_profile(profile: &EffectiveProfile) -> Result<(), ViewError> {
    if profile.codec != Codec::Jpeg {
        return Err(ViewError::Unsupported);
    }
    if profile.max_width == 0 || profile.max_height == 0 {
        return Err(ViewError::InvalidRecord);
    }
    let burst = usize::try_from(profile.burst_bytes).unwrap_or(0);
    if burst <= MAX_HEADER {
        return Err(ViewError::LimitExceeded);
    }
    Ok(())
}

fn merge_profiles(existing: &EffectiveProfile, incoming: &EffectiveProfile) -> EffectiveProfile {
    EffectiveProfile {
        profile: if existing.profile == Profile::Live || incoming.profile == Profile::Live {
            Profile::Live
        } else {
            Profile::Watch
        },
        max_width: existing.max_width.min(incoming.max_width),
        max_height: existing.max_height.min(incoming.max_height),
        quality: existing.quality.min(incoming.quality),
        max_fps: existing.max_fps.max(incoming.max_fps).min(LIVE_MAX_FPS_CAP),
        bytes_per_second: existing.bytes_per_second.min(incoming.bytes_per_second),
        burst_bytes: existing.burst_bytes.min(incoming.burst_bytes),
        codec: Codec::Jpeg,
    }
}

fn spawn_capture_worker_after_stop(
    core: &Arc<TargetCore>,
    prev_join: Option<std::thread::JoinHandle<()>>,
    init_tx: Option<oneshot::Sender<Result<(), ViewError>>>,
) -> Result<(), ViewError> {
    if let Some(j) = prev_join
        && j.thread().id() != std::thread::current().id()
    {
        let _ = j.join();
    }
    let worker_core = Arc::clone(core);
    let spawned = std::thread::Builder::new()
        .name("live-view-cap".into())
        .spawn(move || run_capture_worker(&worker_core, init_tx));
    if let Ok(handle) = spawned {
        core.ctrl.lock().join = Some(handle);
        Ok(())
    } else {
        core.ctrl.lock().running = false;
        Err(ViewError::Unavailable)
    }
}

#[async_trait]
impl FrameSource for DesktopFrameSource {
    async fn subscribe(&self, profile: EffectiveProfile) -> Result<FrameSubscription, ViewError> {
        validate_profile(&profile)?;
        let rx = self.core.frames_tx.subscribe();
        let (init_rx, ticket) = {
            let old_join = {
                let mut ctrl = self.core.ctrl.lock();
                if ctrl.shutdown {
                    return Err(ViewError::Unavailable);
                }
                if ctrl.subscribers == 0 || ctrl.paused {
                    ctrl.profile = profile;
                } else {
                    ctrl.profile = merge_profiles(&ctrl.profile, &profile);
                }
                ctrl.profile.max_fps = ctrl.profile.max_fps.min(LIVE_MAX_FPS_CAP);
                ctrl.subscribers += 1;
                ctrl.paused = false;
                ctrl.stop_requested = false;
                ctrl.profile_dirty = true;
                ctrl.pending_refresh.get_or_insert(0);

                if ctrl.running {
                    self.core.cvar.notify_all();
                    None
                } else {
                    let prev = ctrl.join.take();
                    ctrl.running = true;
                    Some(prev)
                }
            };

            let ticket = SubTicket {
                core: Arc::clone(&self.core),
                armed: true,
            };

            let init_rx = if let Some(prev) = old_join {
                let (itx, irx) = oneshot::channel();
                spawn_capture_worker_after_stop(&self.core, prev, Some(itx))?;
                Some(irx)
            } else {
                None
            };
            (init_rx, ticket)
        };

        if let Some(irx) = init_rx {
            match irx.await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => return Err(e),
                Err(_) => return Err(ViewError::Unavailable),
            }
        }

        let on_drop = ticket.disarm();
        Ok(FrameSubscription::new(rx, on_drop))
    }

    async fn request_refresh(&self, action_seq: u64) -> Result<(), ViewError> {
        let prev_join = {
            let mut ctrl = self.core.ctrl.lock();
            if ctrl.subscribers == 0 || ctrl.paused || ctrl.shutdown {
                return Ok(());
            }
            ctrl.pending_refresh = Some(
                ctrl.pending_refresh
                    .map_or(action_seq, |prev| prev.max(action_seq)),
            );
            if ctrl.running {
                self.core.cvar.notify_all();
                None
            } else {
                ctrl.stop_requested = false;
                let prev = ctrl.join.take();
                ctrl.running = true;
                Some(prev)
            }
        };
        if let Some(prev) = prev_join {
            spawn_capture_worker_after_stop(&self.core, prev, None)?;
        }
        Ok(())
    }

    async fn set_profile(&self, profile: EffectiveProfile) -> Result<(), ViewError> {
        validate_profile(&profile)?;
        let prev_join = {
            let mut ctrl = self.core.ctrl.lock();
            if ctrl.shutdown {
                return Err(ViewError::Unavailable);
            }
            let mut clamped = profile;
            clamped.max_fps = clamped.max_fps.min(LIVE_MAX_FPS_CAP);
            ctrl.profile = clamped;
            ctrl.profile_dirty = true;
            if ctrl.subscribers > 0 {
                ctrl.paused = false;
                ctrl.stop_requested = false;
                ctrl.pending_refresh.get_or_insert(0);
                if ctrl.running {
                    self.core.cvar.notify_all();
                    None
                } else {
                    let prev = ctrl.join.take();
                    ctrl.running = true;
                    Some(prev)
                }
            } else {
                None
            }
        };
        if let Some(prev) = prev_join {
            spawn_capture_worker_after_stop(&self.core, prev, None)?;
        }
        Ok(())
    }

    async fn stop(&self) -> Result<(), ViewError> {
        let waiter_rx = {
            let mut ctrl = self.core.ctrl.lock();
            ctrl.paused = true;
            ctrl.stop_requested = true;
            let _ = self.core.frames_tx.send(None);
            self.core.cvar.notify_all();
            if ctrl.running {
                let (wtx, wrx) = oneshot::channel();
                ctrl.exit_waiters.push(wtx);
                Some(wrx)
            } else {
                None
            }
        };
        if let Some(rx) = waiter_rx {
            let _ = rx.await;
        }
        Ok(())
    }
}

// ── Capture worker loop ───────────────────────────────────────────────────────

struct CaptureSessionCache {
    last_hash: Option<u64>,
    cached_payload: Option<Arc<[u8]>>,
    cached_dims: (u32, u32),
    cached_captured_at_ms: u64,
    /// The cached payload is the refinement frame of the current still screen.
    cached_refined: bool,
    /// Codec of the cached payload.
    cached_codec: Codec,
    /// Since when the screen has been unchanged.
    static_since: Option<Instant>,
    /// No refinement attempt before this instant (budget could not afford it yet).
    refine_retry_at: Option<Instant>,
    last_geom: Option<GeometrySnapshot>,
    frame_seq: u64,
    last_capture: Instant,
    last_publish: Instant,
    token_bytes: f64,
    token_at: Instant,
}

impl CaptureSessionCache {
    fn new(burst_bytes: u32) -> Self {
        let now = Instant::now();
        Self {
            last_hash: None,
            cached_payload: None,
            cached_dims: (0, 0),
            cached_captured_at_ms: 0,
            cached_refined: false,
            cached_codec: Codec::Jpeg,
            static_since: None,
            refine_retry_at: None,
            last_geom: None,
            frame_seq: 0,
            last_capture: now.checked_sub(Duration::from_secs(60)).unwrap_or(now),
            last_publish: now.checked_sub(Duration::from_secs(60)).unwrap_or(now),
            token_bytes: f64::from(burst_bytes),
            token_at: now,
        }
    }

    fn refill_tokens(&mut self, profile: &EffectiveProfile) {
        let now = Instant::now();
        let dt = now.duration_since(self.token_at).as_secs_f64();
        self.token_at = now;
        let max_burst = f64::from(profile.burst_bytes);
        self.token_bytes =
            (self.token_bytes + dt * f64::from(profile.bytes_per_second)).min(max_burst);
    }

    fn invalidate(&mut self) {
        self.last_hash = None;
        self.cached_payload = None;
        self.cached_dims = (0, 0);
        self.cached_captured_at_ms = 0;
        self.cached_refined = false;
        self.cached_codec = Codec::Jpeg;
        self.static_since = None;
        self.refine_retry_at = None;
    }
}

/// Attempt to mark the capture worker as stopped.
///
/// Returns `true` if the worker should exit, or `false` if a new subscriber/resume arrived
/// before `ctrl.lock()` was acquired and the worker must continue running.
fn try_finish_capture_worker(core: &Arc<TargetCore>, allow_restart: bool) -> bool {
    #[cfg(test)]
    {
        let hook = core.pre_finish_hook.lock().take();
        if let Some(f) = hook {
            f();
        }
    }
    let (waiters, should_exit) = {
        let mut ctrl = core.ctrl.lock();
        let waiters = std::mem::take(&mut ctrl.exit_waiters);
        if allow_restart
            && !ctrl.shutdown
            && !ctrl.stop_requested
            && !ctrl.paused
            && ctrl.subscribers > 0
        {
            (waiters, false)
        } else {
            ctrl.running = false;
            (waiters, true)
        }
    };
    if should_exit {
        let _ = core.frames_tx.send(None);
    }
    for w in waiters {
        let _ = w.send(());
    }
    should_exit
}

fn periodic_interval(profile: &EffectiveProfile) -> Duration {
    // Capture is change-driven: an unchanged screen costs one hash, not an encode.
    let fps = profile.max_fps.clamp(1, LIVE_MAX_FPS_CAP);
    Duration::from_secs_f64(1.0 / f64::from(fps))
}

fn send_init(
    init_tx: &mut Option<oneshot::Sender<Result<(), ViewError>>>,
    val: Result<(), ViewError>,
) {
    if let Some(tx) = init_tx.take() {
        let _ = tx.send(val);
    }
}

fn update_status_ok(core: &Arc<TargetCore>, geom: GeometrySnapshot) -> (u64, u64) {
    let mut st = core.status.lock();
    let changed = st.latest_geometry != geom;
    if changed {
        st.geometry_revision = st.geometry_revision.saturating_add(1);
        st.latest_geometry = geom;
        core.stats.geometry_changes.fetch_add(1, Ordering::Relaxed);
    }
    st.view_available = true;
    st.unavailable_reason = None;
    core.stats.set_reason(None);
    (st.generation, st.geometry_revision)
}

fn handle_capture_failure(
    core: &Arc<TargetCore>,
    err: &CaptureFailure,
    driver: &mut Option<Box<dyn CaptureDriver>>,
    saw_error: &mut bool,
    cache: &mut CaptureSessionCache,
) {
    *saw_error = true;
    core.stats.capture_errors.fetch_add(1, Ordering::Relaxed);
    let reason = err.reason();
    core.stats.set_reason(Some(reason.clone()));
    {
        let mut st = core.status.lock();
        st.view_available = false;
        st.unavailable_reason = Some(reason);
    }
    cache.invalidate();
    let _ = core.frames_tx.send(None);
    if matches!(err, CaptureFailure::ConnectionLost(_)) {
        *driver = None;
    }
}

struct WorkerBuffers {
    raw: Vec<u8>,
    bgra: Vec<u8>,
    scaled: Vec<u8>,
    rgb: Vec<u8>,
    jpeg: Vec<u8>,
}

fn wait_for_capture_wake(
    core: &Arc<TargetCore>,
    cache: &CaptureSessionCache,
) -> Option<(EffectiveProfile, Option<u64>, bool)> {
    let mut ctrl = core.ctrl.lock();
    loop {
        if ctrl.shutdown || ctrl.stop_requested || ctrl.paused || ctrl.subscribers == 0 {
            return None;
        }
        let base_interval = periodic_interval(&ctrl.profile);
        let interval = if cache.cached_payload.is_none() {
            base_interval.min(UNAVAILABLE_RETRY_INTERVAL)
        } else {
            base_interval
        };
        let due_periodic = cache.last_capture.elapsed() >= interval;
        if ctrl.pending_refresh.is_some() || ctrl.profile_dirty || due_periodic {
            let refresh = ctrl.pending_refresh.take();
            let pdirty = ctrl.profile_dirty;
            ctrl.profile_dirty = false;
            return Some((ctrl.profile.clone(), refresh, pdirty));
        }
        let wait_for = (cache.last_capture + interval).saturating_duration_since(Instant::now());
        let _ = core.cvar.wait_for(&mut ctrl, wait_for);
    }
}

fn record_recovery_if_needed(core: &Arc<TargetCore>, saw_error: &mut bool) {
    if *saw_error {
        let mut st = core.status.lock();
        st.generation = st.generation.saturating_add(1);
        core.stats.generation_bumps.fetch_add(1, Ordering::Relaxed);
        *saw_error = false;
    }
}

fn ensure_active_driver(
    core: &Arc<TargetCore>,
    driver: &mut Option<Box<dyn CaptureDriver>>,
    saw_error: &mut bool,
    cache: &mut CaptureSessionCache,
) -> Result<(), ViewError> {
    if driver.is_some() {
        return Ok(());
    }
    match (core.capture_factory)(&core.options.scope) {
        Ok(d) => {
            *driver = Some(d);
            record_recovery_if_needed(core, saw_error);
            Ok(())
        }
        Err(err) => {
            let ve = err.to_view_error();
            handle_capture_failure(core, &err, driver, saw_error, cache);
            Err(ve)
        }
    }
}

fn publish_reused_frame(
    core: &Arc<TargetCore>,
    cache: &mut CaptureSessionCache,
    profile: &EffectiveProfile,
    geom: &GeometrySnapshot,
    refresh_seq: Option<u64>,
    generation: u64,
    geometry_revision: u64,
) -> bool {
    let forced = refresh_seq.is_some();
    let keepalive_due = match profile.profile {
        Profile::Watch => cache.last_publish.elapsed() >= WATCH_IDLE_INTERVAL,
        Profile::Live => cache.last_publish.elapsed() >= LIVE_HEARTBEAT_INTERVAL,
    };
    if !(forced || keepalive_due) {
        core.stats
            .frames_skipped_unchanged
            .fetch_add(1, Ordering::Relaxed);
        return false;
    }
    let Some(payload) = cache.cached_payload.as_ref().map(Arc::clone) else {
        return false;
    };
    let cause = match refresh_seq {
        Some(seq) if seq > 0 => Cause::Action,
        Some(_) => Cause::Refresh,
        None => Cause::Idle,
    };
    let (frame_seq, captured_at_ms) = if forced {
        cache.frame_seq = cache.frame_seq.saturating_add(1);
        let now_ms = unix_now_ms();
        cache.cached_captured_at_ms = now_ms;
        (cache.frame_seq, now_ms)
    } else {
        (cache.frame_seq, cache.cached_captured_at_ms)
    };
    let frame = Arc::new(Frame {
        metadata: FrameMetadata {
            target_id: core.target_id,
            generation,
            frame_seq,
            captured_at_ms,
            cause,
            action_seq: refresh_seq.filter(|&s| s > 0),
            codec: cache.cached_codec.clone(),
            width: cache.cached_dims.0,
            height: cache.cached_dims.1,
            source_rect: geom.rect.to_source_rect(),
            geometry_revision,
            repeated: !forced,
        },
        payload,
    });
    if frame.validate().is_ok() {
        core.last_published_geometry_revision
            .store(geometry_revision, Ordering::Release);
        let _ = core.frames_tx.send(Some(frame));
        cache.last_publish = Instant::now();
        core.stats.frames_repeated.fetch_add(1, Ordering::Relaxed);
        return true;
    }
    false
}

#[allow(clippy::too_many_arguments)]
fn encode_and_publish_frame(
    core: &Arc<TargetCore>,
    cache: &mut CaptureSessionCache,
    bufs: &mut WorkerBuffers,
    profile: &EffectiveProfile,
    geom: &GeometrySnapshot,
    hash: u64,
    refine: bool,
    refresh_seq: Option<u64>,
    generation: u64,
    geometry_revision: u64,
) -> Result<(), ViewError> {
    let forced = refresh_seq.is_some();
    let cause = match refresh_seq {
        Some(seq) if seq > 0 => Cause::Action,
        Some(_) => Cause::Refresh,
        None => Cause::Idle,
    };
    cache.refill_tokens(profile);
    let encoded = if refine {
        encode_refined(
            &bufs.bgra,
            geom.rect.width,
            geom.rect.height,
            profile,
            &mut bufs.scaled,
            &mut bufs.rgb,
            &mut bufs.jpeg,
        )
    } else {
        encode_jpeg_within_budget(
            &bufs.bgra,
            geom.rect.width,
            geom.rect.height,
            profile,
            select_encode_settings(profile),
            &mut bufs.scaled,
            &mut bufs.jpeg,
        )
        .map(|(w, h)| (Codec::Jpeg, w, h))
    };
    let Ok((codec, out_w, out_h)) = encoded else {
        core.stats
            .frames_skipped_budget
            .fetch_add(1, Ordering::Relaxed);
        core.stats.set_reason(Some(Reason::FrameBudgetExceeded));
        {
            let mut st = core.status.lock();
            st.unavailable_reason = Some(Reason::FrameBudgetExceeded);
        }
        return Err(ViewError::LimitExceeded);
    };

    let cost =
        f64::from(u32::try_from(bufs.jpeg.len().saturating_add(MAX_HEADER)).unwrap_or(u32::MAX));
    if !forced && cache.cached_payload.is_some() && cache.token_bytes < cost {
        // Record the fingerprint of the skipped screen so subsequent periodic ticks on
        // the same static screen do not re-encode the identical BGRA buffer.
        cache.last_hash = Some(hash);
        if refine {
            cache.refine_retry_at = Some(Instant::now() + REFINE_RETRY);
        }
        core.stats
            .frames_skipped_budget
            .fetch_add(1, Ordering::Relaxed);
        return Ok(());
    }
    cache.token_bytes = (cache.token_bytes - cost).max(0.0);
    let payload: Arc<[u8]> = Arc::from(bufs.jpeg.as_slice());
    cache.frame_seq = cache.frame_seq.saturating_add(1);
    let captured_at_ms = unix_now_ms();
    let frame = Arc::new(Frame {
        metadata: FrameMetadata {
            target_id: core.target_id,
            generation,
            frame_seq: cache.frame_seq,
            captured_at_ms,
            cause,
            action_seq: refresh_seq.filter(|&s| s > 0),
            codec: codec.clone(),
            width: out_w,
            height: out_h,
            source_rect: geom.rect.to_source_rect(),
            geometry_revision,
            repeated: false,
        },
        payload: Arc::clone(&payload),
    });
    if frame.validate().is_ok() {
        core.last_published_geometry_revision
            .store(geometry_revision, Ordering::Release);
        let _ = core.frames_tx.send(Some(frame));
        cache.last_hash = Some(hash);
        cache.cached_payload = Some(payload);
        cache.cached_dims = (out_w, out_h);
        cache.cached_captured_at_ms = captured_at_ms;
        cache.cached_refined = refine;
        cache.cached_codec = codec;
        cache.last_publish = Instant::now();
        core.stats.frames_encoded.fetch_add(1, Ordering::Relaxed);
        Ok(())
    } else {
        core.stats
            .frames_skipped_budget
            .fetch_add(1, Ordering::Relaxed);
        core.stats.set_reason(Some(Reason::FrameBudgetExceeded));
        Err(ViewError::LimitExceeded)
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_capture_step(
    core: &Arc<TargetCore>,
    driver: &mut Option<Box<dyn CaptureDriver>>,
    saw_error: &mut bool,
    cache: &mut CaptureSessionCache,
    bufs: &mut WorkerBuffers,
    profile: &EffectiveProfile,
    refresh_seq: Option<u64>,
    profile_dirty: bool,
) -> Option<Result<(), ViewError>> {
    cache.last_capture = Instant::now();
    let capture_res = {
        let drv = driver.as_mut()?;
        drv.capture(&mut bufs.raw)
    };
    let raw_info = match capture_res {
        Ok(info) => {
            record_recovery_if_needed(core, saw_error);
            info
        }
        Err(err) => {
            let ve = err.to_view_error();
            handle_capture_failure(core, &err, driver, saw_error, cache);
            sleep_interruptible(core, UNAVAILABLE_RETRY_INTERVAL);
            return Some(Err(ve));
        }
    };

    if extract_strided_bgra_into(
        &bufs.raw,
        raw_info.geometry.rect.width,
        raw_info.geometry.rect.height,
        raw_info.stride_bytes,
        raw_info.format,
        &mut bufs.bgra,
    )
    .is_err()
    {
        let err = CaptureFailure::LimitExceeded;
        handle_capture_failure(core, &err, driver, saw_error, cache);
        sleep_interruptible(core, UNAVAILABLE_RETRY_INTERVAL);
        return Some(Err(ViewError::LimitExceeded));
    }

    core.stats.frames_captured.fetch_add(1, Ordering::Relaxed);
    let (generation, geometry_revision) = update_status_ok(core, raw_info.geometry);
    let geom_changed = cache.last_geom != Some(raw_info.geometry);
    cache.last_geom = Some(raw_info.geometry);
    if geom_changed {
        cache.invalidate();
    }

    let hash = bgra_fingerprint(&bufs.bgra);
    if !geom_changed && cache.last_hash == Some(hash) {
        cache.static_since.get_or_insert_with(Instant::now);
    } else {
        cache.static_since = None;
    }
    // A screen that stayed still for `SETTLE_AFTER` gets ONE refinement frame (lossless WebP at
    // the output size, JPEG q92 4:4:4 if that is too big) on top of the frames sent while it
    // moved.
    let refine_due = !profile_dirty
        && refine_enabled(profile)
        && !cache.cached_refined
        && cache.cached_payload.is_some()
        && cache
            .static_since
            .is_some_and(|t| t.elapsed() >= SETTLE_AFTER)
        && cache.refine_retry_at.is_none_or(|t| Instant::now() >= t);
    let can_reuse = !profile_dirty
        && !refine_due
        && !geom_changed
        && cache.last_hash == Some(hash)
        && cache.cached_payload.is_some()
        && cache.cached_dims.0 <= profile.max_width.min(MAX_OUTPUT_WIDTH)
        && cache.cached_dims.1 <= profile.max_height.min(MAX_OUTPUT_HEIGHT);

    if can_reuse {
        return publish_reused_frame(
            core,
            cache,
            profile,
            &raw_info.geometry,
            refresh_seq,
            generation,
            geometry_revision,
        )
        .then_some(Ok(()));
    }

    let res = encode_and_publish_frame(
        core,
        cache,
        bufs,
        profile,
        &raw_info.geometry,
        hash,
        refine_due,
        refresh_seq,
        generation,
        geometry_revision,
    );
    if res.is_err() {
        sleep_interruptible(core, UNAVAILABLE_RETRY_INTERVAL);
    }
    Some(res)
}

fn run_capture_worker(
    core: &Arc<TargetCore>,
    mut init_tx: Option<oneshot::Sender<Result<(), ViewError>>>,
) {
    let mut driver: Option<Box<dyn CaptureDriver>> =
        match (core.capture_factory)(&core.options.scope) {
            Ok(d) => Some(d),
            Err(err) => {
                let view_err = err.to_view_error();
                let reason = err.reason();
                core.stats.capture_errors.fetch_add(1, Ordering::Relaxed);
                core.stats.set_reason(Some(reason.clone()));
                {
                    let mut st = core.status.lock();
                    st.view_available = false;
                    st.unavailable_reason = Some(reason);
                }
                send_init(&mut init_tx, Err(view_err));
                let _ = try_finish_capture_worker(core, false);
                return;
            }
        };

    let initial_burst = core.ctrl.lock().profile.burst_bytes;
    let mut cache = CaptureSessionCache::new(initial_burst);
    let mut bufs = WorkerBuffers {
        raw: Vec::new(),
        bgra: Vec::new(),
        scaled: Vec::new(),
        rgb: Vec::new(),
        jpeg: Vec::new(),
    };
    let mut saw_error = false;

    loop {
        while let Some((profile, refresh_seq, profile_dirty)) = wait_for_capture_wake(core, &cache)
        {
            if let Err(ve) = ensure_active_driver(core, &mut driver, &mut saw_error, &mut cache) {
                send_init(&mut init_tx, Err(ve));
                sleep_interruptible(core, UNAVAILABLE_RETRY_INTERVAL);
                continue;
            }
            if let Some(res) = execute_capture_step(
                core,
                &mut driver,
                &mut saw_error,
                &mut cache,
                &mut bufs,
                &profile,
                refresh_seq,
                profile_dirty,
            ) {
                send_init(&mut init_tx, res);
            }
        }

        send_init(&mut init_tx, Ok(()));
        if try_finish_capture_worker(core, true) {
            break;
        }
    }
}

fn sleep_interruptible(core: &Arc<TargetCore>, duration: Duration) {
    let mut ctrl = core.ctrl.lock();
    if ctrl.shutdown || ctrl.stop_requested || ctrl.paused || ctrl.subscribers == 0 {
        return;
    }
    let _ = core.cvar.wait_for(&mut ctrl, duration);
}

// ── Cancel-safe InputSink coordinator ─────────────────────────────────────────

/// Cancel-safe desktop [`InputSink`] coordinator implementing revision-checked
/// [`InputSink::inject_at_revision`] alongside legacy [`InputSink::inject`]
/// (which rejects positional events by default).
pub struct DesktopInputSink {
    core: Arc<TargetCore>,
}

impl DesktopInputSink {
    async fn inject_inner(
        &self,
        event: &InputEvent,
        expected: Option<(u64, u64)>,
    ) -> Result<(), ViewError> {
        if !event.valid() {
            return Err(ViewError::InvalidRecord);
        }
        if matches!(event, InputEvent::Reset) {
            return self.release_pressed_inputs().await;
        }
        if !self.core.takeover_active.load(Ordering::Acquire) {
            return Err(ViewError::Unavailable);
        }
        let (generation, geometry_revision, source_rect, virtual_rect) = {
            let st = self.core.status.lock();
            if !st.view_available || !st.input_available || st.unavailable_reason.is_some() {
                return Err(ViewError::Unavailable);
            }
            if expected.is_none()
                && event.positional()
                && self.core.require_explicit_revision.load(Ordering::Acquire)
            {
                self.core
                    .stats
                    .input_cancelled
                    .fetch_add(1, Ordering::Relaxed);
                return Err(ViewError::Unavailable);
            }
            let published_rev = self
                .core
                .last_published_geometry_revision
                .load(Ordering::Acquire);
            let (expected_gen, expected_rev) = if let Some((g, r)) = expected {
                if st.generation != g
                    || (event.positional()
                        && (st.geometry_revision != r || st.geometry_revision != published_rev))
                {
                    self.core
                        .stats
                        .input_cancelled
                        .fetch_add(1, Ordering::Relaxed);
                    return Err(ViewError::Unavailable);
                }
                (g, r)
            } else {
                if event.positional() && st.geometry_revision != published_rev {
                    self.core
                        .stats
                        .input_cancelled
                        .fetch_add(1, Ordering::Relaxed);
                    return Err(ViewError::Unavailable);
                }
                (st.generation, st.geometry_revision)
            };
            (
                expected_gen,
                expected_rev,
                st.latest_geometry.rect.to_source_rect(),
                st.latest_geometry.virtual_rect,
            )
        };
        if event.positional() {
            validate_positional_geometry(&source_rect, virtual_rect)?;
        }
        let tx = ensure_input_worker(&self.core)?;
        let cancelled = Arc::new(AtomicBool::new(false));
        self.core.register_input_cancel(&cancelled);
        let _guard = CancelOnDrop(Arc::clone(&cancelled));
        let epoch = self.core.input_epoch.load(Ordering::Acquire);
        let (reply_tx, reply_rx) = oneshot::channel();
        if tx
            .send(InputJob::Inject {
                epoch,
                generation,
                geometry_revision,
                cancelled,
                event: event.clone(),
                source_rect,
                virtual_rect,
                reply: reply_tx,
            })
            .is_err()
        {
            self.core.unclean_input_reset.store(true, Ordering::Release);
            *self.core.input_worker.lock() = None;
            return Err(ViewError::Unavailable);
        }
        if let Ok(res) = reply_rx.await {
            res
        } else {
            self.core.unclean_input_reset.store(true, Ordering::Release);
            *self.core.input_worker.lock() = None;
            Err(ViewError::Unavailable)
        }
    }
}

struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

#[allow(clippy::too_many_lines)]
fn run_input_worker_loop(
    initial_drv: Box<dyn InputDriver>,
    factory: &InputDriverFactory,
    rx: &std::sync::mpsc::Receiver<InputJob>,
    core_weak: &Weak<TargetCore>,
) {
    let mut drv: Option<Box<dyn InputDriver>> = Some(initial_drv);
    let mut unclean_driver_loss = false;
    while let Ok(job) = rx.recv() {
        match job {
            InputJob::Inject {
                epoch: job_epoch,
                generation: job_generation,
                geometry_revision: job_geometry_revision,
                cancelled,
                event,
                source_rect,
                virtual_rect,
                reply,
            } => {
                let Some(core) = core_weak.upgrade() else {
                    let _ = reply.send(Err(ViewError::Unavailable));
                    break;
                };
                let stats = &core.stats;
                if cancelled.load(Ordering::Acquire)
                    || !core.takeover_active.load(Ordering::Acquire)
                    || job_epoch != core.input_epoch.load(Ordering::Acquire)
                {
                    stats.input_cancelled.fetch_add(1, Ordering::Relaxed);
                    let _ = reply.send(Err(ViewError::Unavailable));
                    continue;
                }
                {
                    let st = core.status.lock();
                    let published_rev = core
                        .last_published_geometry_revision
                        .load(Ordering::Acquire);
                    let stale_geom = event.positional()
                        && (st.geometry_revision != job_geometry_revision
                            || st.geometry_revision != published_rev);
                    if !st.view_available
                        || !st.input_available
                        || st.unavailable_reason.is_some()
                        || st.generation != job_generation
                        || stale_geom
                    {
                        stats.input_cancelled.fetch_add(1, Ordering::Relaxed);
                        let _ = reply.send(Err(ViewError::Unavailable));
                        continue;
                    }
                }
                if event.positional()
                    && validate_positional_geometry(&source_rect, virtual_rect).is_err()
                {
                    stats.input_cancelled.fetch_add(1, Ordering::Relaxed);
                    let _ = reply.send(Err(ViewError::Unavailable));
                    continue;
                }
                if drv.is_none() {
                    match factory() {
                        Ok(new_drv) => drv = Some(new_drv),
                        Err(e) => {
                            if matches!(e, ViewError::Unavailable) {
                                core.notify_input_unavailable();
                            }
                            let _ = reply.send(Err(e));
                            continue;
                        }
                    }
                }
                let Some(active_drv) = drv.as_mut() else {
                    let _ = reply.send(Err(ViewError::Unavailable));
                    continue;
                };
                let call_res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    active_drv.inject(&event, source_rect, virtual_rect, &cancelled)
                }));
                let res = if let Ok(r) = call_res {
                    r
                } else {
                    // Best-effort release on the panicked driver; if release itself fails or
                    // panics (e.g. a poisoned mutex), keep the reset owed (`unclean_driver_loss`).
                    let rel = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        active_drv.release_all()
                    }));
                    drv = None;
                    if matches!(rel, Ok(Ok(()))) {
                        unclean_driver_loss = false;
                    } else {
                        unclean_driver_loss = true;
                        core.unclean_input_reset.store(true, Ordering::Release);
                    }
                    Err(ViewError::Unavailable)
                };
                match &res {
                    Ok(()) => {
                        stats.input_events.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(ViewError::PartialInjection) => {
                        stats.input_partial_failures.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(ViewError::Unavailable) => {
                        core.notify_input_unavailable();
                    }
                    Err(_) => {}
                }
                let _ = reply.send(res);
            }
            InputJob::ReleaseAll { reply } => {
                let core_opt = core_weak.upgrade();
                if let Some(core) = core_opt.as_ref() {
                    core.stats.input_resets.fetch_add(1, Ordering::Relaxed);
                }
                let res = if let Some(active_drv) = drv.as_mut() {
                    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        active_drv.release_all()
                    })) {
                        Ok(Ok(())) => {
                            unclean_driver_loss = false;
                            if let Some(core) = core_opt.as_ref() {
                                core.unclean_input_reset.store(false, Ordering::Release);
                            }
                            if active_drv.is_connection_lost() {
                                drv = None;
                            }
                            Ok(())
                        }
                        Ok(Err(e)) => {
                            // Keep `drv` alive with its remaining held-input debt for retries.
                            if matches!(e, ViewError::Unavailable)
                                && let Some(core) = core_opt.as_ref()
                            {
                                core.notify_input_unavailable();
                            }
                            Err(e)
                        }
                        Err(_) => {
                            // A panicking or poisoned `release_all` must NEVER report `Ok(())`:
                            // drop the poisoned driver and keep the reset owed.
                            drv = None;
                            unclean_driver_loss = true;
                            if let Some(core) = core_opt.as_ref() {
                                core.unclean_input_reset.store(true, Ordering::Release);
                                core.notify_input_unavailable();
                            }
                            Err(ViewError::Unavailable)
                        }
                    }
                } else if unclean_driver_loss {
                    if let Some(core) = core_opt.as_ref() {
                        core.notify_input_unavailable();
                    }
                    Err(ViewError::Unavailable)
                } else {
                    if let Some(core) = core_opt.as_ref() {
                        core.unclean_input_reset.store(false, Ordering::Release);
                    }
                    Ok(())
                };
                let _ = reply.send(res);
            }
            InputJob::Shutdown => {
                if let Some(active_drv) = drv.as_mut() {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let _ = active_drv.release_all();
                    }));
                }
                return;
            }
        }
    }
    if let Some(active_drv) = drv.as_mut() {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = active_drv.release_all();
        }));
    }
}

fn ensure_input_worker(
    core: &Arc<TargetCore>,
) -> Result<std::sync::mpsc::Sender<InputJob>, ViewError> {
    let mut guard = core.input_worker.lock();
    if let Some(worker) = guard.as_ref() {
        let alive = worker.join.as_ref().is_some_and(|j| !j.is_finished());
        if alive {
            return Ok(worker.tx.clone());
        }
        if let Some(mut dead) = guard.take()
            && let Some(j) = dead.join.take()
            && j.thread().id() != std::thread::current().id()
        {
            let _ = j.join();
        }
    }
    let Some(factory) = core.input_factory.as_ref().map(Arc::clone) else {
        return Err(ViewError::Unsupported);
    };
    let (tx, rx) = std::sync::mpsc::channel::<InputJob>();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), ViewError>>();
    let core_weak = Arc::downgrade(core);
    let join = std::thread::Builder::new()
        .name("live-view-in".into())
        .spawn(move || {
            let drv = match factory() {
                Ok(d) => {
                    let _ = ready_tx.send(Ok(()));
                    d
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            run_input_worker_loop(drv, &factory, &rx, &core_weak);
        })
        .map_err(|_| ViewError::Unavailable)?;

    match ready_rx.recv() {
        Ok(Ok(())) => {
            *guard = Some(InputWorker {
                tx: tx.clone(),
                join: Some(join),
            });
            Ok(tx)
        }
        Ok(Err(e)) => {
            let _ = join.join();
            Err(e)
        }
        Err(_) => {
            let _ = join.join();
            Err(ViewError::Unavailable)
        }
    }
}

#[async_trait]
impl InputSink for DesktopInputSink {
    async fn inject(&self, event: &InputEvent) -> Result<(), ViewError> {
        self.inject_inner(event, None).await
    }

    async fn inject_at_revision(
        &self,
        event: &InputEvent,
        generation: u64,
        geometry_revision: u64,
    ) -> Result<(), ViewError> {
        self.inject_inner(event, Some((generation, geometry_revision)))
            .await
    }

    async fn release_pressed_inputs(&self) -> Result<(), ViewError> {
        self.core.input_epoch.fetch_add(1, Ordering::SeqCst);
        self.core.cancel_in_flight_input();
        let tx_opt = {
            let guard = self.core.input_worker.lock();
            guard.as_ref().map(|w| w.tx.clone())
        };
        let Some(tx) = tx_opt else {
            if self.core.unclean_input_reset.load(Ordering::Acquire) {
                return Err(ViewError::Unavailable);
            }
            // No input worker was ever spawned; nothing was pressed by this sink.
            return Ok(());
        };
        let (reply_tx, reply_rx) = oneshot::channel();
        if tx.send(InputJob::ReleaseAll { reply: reply_tx }).is_err() {
            self.core.unclean_input_reset.store(true, Ordering::Release);
            *self.core.input_worker.lock() = None;
            return Err(ViewError::Unavailable);
        }
        if let Ok(res) = reply_rx.await {
            res
        } else {
            self.core.unclean_input_reset.store(true, Ordering::Release);
            *self.core.input_worker.lock() = None;
            Err(ViewError::Unavailable)
        }
    }

    async fn set_takeover_active(&self, active: bool) -> Result<(), ViewError> {
        if active {
            {
                let st = self.core.status.lock();
                if !st.view_available || !st.input_available || st.unavailable_reason.is_some() {
                    return Err(ViewError::Unavailable);
                }
            }
            let _ = ensure_input_worker(&self.core)?;
            self.core.takeover_active.store(true, Ordering::SeqCst);
            Ok(())
        } else {
            self.core.takeover_active.store(false, Ordering::SeqCst);
            self.core.input_epoch.fetch_add(1, Ordering::SeqCst);
            self.core.cancel_in_flight_input();
            {
                let mut ctrl = self.core.ctrl.lock();
                if ctrl.profile.profile == Profile::Live {
                    ctrl.profile = EffectiveProfile::watch();
                    ctrl.profile_dirty = true;
                    self.core.cvar.notify_all();
                }
            }
            self.release_pressed_inputs().await
        }
    }
}

fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    #[test]
    fn target_id_is_stable_for_the_same_screen_and_differs_between_screens() {
        let a = stable_target_id(&LiveViewOptions::new("host-a"));
        assert_eq!(a, stable_target_id(&LiveViewOptions::new("host-a")));
        assert_ne!(a, stable_target_id(&LiveViewOptions::new("host-b")));
        let mut other = LiveViewOptions::new("host-a");
        other.scope = CaptureScope::PrimaryMonitor;
        assert_ne!(a, stable_target_id(&other));
        assert_eq!(a.get_version_num(), 8);
    }
    use super::*;
    use crate::{
        Action, ActionResult, Capabilities, DesktopBackend, DesktopError, Image, Quality, Region,
        Snapshot, WindowInfo,
        snapshot::{AppSpec, Pid},
    };

    #[test]
    fn default_desktop_backend_hook_returns_unsupported_without_breaking_fakes() {
        struct MinimalFakeBackend;
        #[async_trait]
        impl DesktopBackend for MinimalFakeBackend {
            async fn snapshot(&self, _scope: Option<u64>) -> Result<Snapshot, DesktopError> {
                Err(DesktopError::Unsupported("fake".into()))
            }
            async fn act(&self, _action: Action) -> Result<ActionResult, DesktopError> {
                Err(DesktopError::Unsupported("fake".into()))
            }
            async fn screenshot(&self, _r: Region, _q: Quality) -> Result<Image, DesktopError> {
                Err(DesktopError::Unsupported("fake".into()))
            }
            async fn windows(&self) -> Result<Vec<WindowInfo>, DesktopError> {
                Ok(vec![])
            }
            async fn launch(&self, _app: AppSpec) -> Result<Pid, DesktopError> {
                Err(DesktopError::Unsupported("fake".into()))
            }
            fn capabilities(&self) -> Capabilities {
                Capabilities {
                    platform: "fake".into(),
                    accessibility_tree: false,
                    screenshot: false,
                    app_launch: false,
                    has_display: false,
                }
            }
        }

        let fake = MinimalFakeBackend;
        assert_eq!(
            fake.live_view_target(&LiveViewOptions::new("host-1")).err(),
            Some(Reason::Unsupported)
        );
    }

    #[test]
    fn raw_dimension_validation_rejects_zero_overflow_and_pixel_bombs() {
        assert_eq!(
            validate_raw_dimensions(0, 100),
            Err(ViewError::LimitExceeded)
        );
        assert_eq!(
            validate_raw_dimensions(100, 0),
            Err(ViewError::LimitExceeded)
        );
        assert_eq!(
            validate_raw_dimensions(4001, 4000),
            Err(ViewError::LimitExceeded)
        );
        assert_eq!(
            validate_raw_dimensions(u32::MAX, 2),
            Err(ViewError::LimitExceeded)
        );
        assert_eq!(validate_raw_dimensions(4000, 4000), Ok(16_000_000));
    }

    #[test]
    fn strided_bgra_and_rgb565_extract_accurately_and_force_alpha_255() {
        // 2x2 BGRX with 12-byte stride (4 extra padding bytes per row)
        let mut src = vec![0xAA; 2 * 12];
        src[0..8].copy_from_slice(&[10, 20, 30, 0, 40, 50, 60, 1]);
        src[12..20].copy_from_slice(&[70, 80, 90, 2, 100, 110, 120, 3]);
        let mut dst = Vec::new();
        extract_strided_bgra_into(&src, 2, 2, 12, PixelFormat::Bgra32, &mut dst).unwrap();
        assert_eq!(
            dst,
            vec![
                10, 20, 30, 255, 40, 50, 60, 255, 70, 80, 90, 255, 100, 110, 120, 255
            ]
        );

        // Short stride or truncated buffer must fail before out-of-bounds read
        assert_eq!(
            extract_strided_bgra_into(&src, 2, 2, 7, PixelFormat::Bgra32, &mut dst),
            Err(ViewError::InvalidRecord)
        );
        assert_eq!(
            extract_strided_bgra_into(&src[..15], 2, 2, 12, PixelFormat::Bgra32, &mut dst),
            Err(ViewError::InvalidRecord)
        );

        // 1x1 RGB565 pure red (0xF800 LE -> [0x00, 0xF8])
        extract_strided_bgra_into(&[0x00, 0xF8], 1, 1, 2, PixelFormat::Rgb565Le, &mut dst).unwrap();
        assert_eq!(dst, vec![0, 0, 255, 255]);
    }

    #[test]
    fn jpeg_encoder_fits_tight_burst_budget_in_ram_and_handles_impossible_budget() {
        let width = 320u32;
        let height = 240u32;
        let mut bgra = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                bgra.extend_from_slice(&[
                    (x & 0xFF) as u8,
                    (y & 0xFF) as u8,
                    ((x ^ y) & 0xFF) as u8,
                    255,
                ]);
            }
        }
        let mut profile = EffectiveProfile::live();
        profile.max_width = 160;
        profile.max_height = 120;
        profile.burst_bytes = u32::try_from(MAX_HEADER + 8_192).unwrap();

        let mut scratch = Vec::new();
        let mut out = Vec::new();
        let (ow, oh) = encode_jpeg_within_budget(
            &bgra,
            width,
            height,
            &profile,
            select_encode_settings(&profile),
            &mut scratch,
            &mut out,
        )
        .unwrap();
        assert!(ow <= 160 && oh <= 120);
        assert_ne!(out.len(), 0);
        assert!(out.len() + MAX_HEADER <= profile.burst_bytes as usize);
        assert_eq!(&out[..2], &[0xFF, 0xD8]); // JPEG SOI

        profile.burst_bytes = u32::try_from(MAX_HEADER).unwrap();
        assert_eq!(
            encode_jpeg_within_budget(
                &bgra,
                width,
                height,
                &profile,
                select_encode_settings(&profile),
                &mut scratch,
                &mut out
            ),
            Err(ViewError::LimitExceeded)
        );
    }

    #[test]
    fn encode_settings_keep_full_colour_and_compact_streams_stay_compact() {
        let watch = select_encode_settings(&EffectiveProfile::watch());
        assert_eq!(watch.quality, EffectiveProfile::watch().quality);
        assert!(watch.chroma_444);
        assert_eq!(watch.filter, ResizeFilter::Lanczos3);
        assert!(refine_enabled(&EffectiveProfile::watch()));

        let live = EffectiveProfile::live();
        let moving = select_encode_settings(&live);
        assert_eq!(moving.quality, live.quality);
        assert!(
            moving.chroma_444,
            "coloured text must not smear while moving"
        );
        assert_eq!(moving.filter, ResizeFilter::CatmullRom);

        // A slow viewer asked for a compact stream: 4:2:0 and no refinement frame.
        let mut slow = EffectiveProfile::live();
        slow.quality = 35;
        let slow_settings = select_encode_settings(&slow);
        assert_eq!(slow_settings.quality, 35);
        assert!(!slow_settings.chroma_444);
        assert!(!refine_enabled(&slow));

        // The output cap is the native size up to 1920x1080, never an upscale.
        assert_eq!(
            compute_target_dimensions(1280, 800, 1920, 1080).unwrap(),
            (1280, 800)
        );
        assert_eq!(
            compute_target_dimensions(3840, 2160, 1920, 1080).unwrap(),
            (1920, 1080)
        );
        // A portrait screen keeps its long edge: the box is turned on its side.
        assert_eq!(
            compute_target_dimensions(1080, 1920, 1920, 1080).unwrap(),
            (1080, 1920)
        );
        assert_eq!(
            compute_target_dimensions(2160, 3840, 1920, 1080).unwrap(),
            (1080, 1920)
        );
        assert_eq!(
            compute_target_dimensions(1440, 2560, 1920, 1080).unwrap(),
            (1080, 1920)
        );
        assert!(EffectiveProfile::watch().max_width >= 1920);
        assert!(EffectiveProfile::live().max_width >= 1920);
    }

    #[test]
    fn resize_keeps_flat_colour_exact_and_dimensions_right() {
        let (sw, sh) = (97u32, 61u32);
        let src: Vec<u8> = std::iter::repeat_n([10u8, 130, 250, 255], (sw * sh) as usize)
            .flatten()
            .collect();
        for filter in [ResizeFilter::CatmullRom, ResizeFilter::Lanczos3] {
            let mut dst = Vec::new();
            resize_bgra_into(&src, sw, sh, 40, 25, filter, &mut dst);
            assert_eq!(dst.len(), 40 * 25 * 4);
            assert!(dst.chunks(4).all(|p| p == [10, 130, 250, 255]));
        }
    }

    /// Deterministic text-like page: stems and bars in 7x14 cells, softened with a [1 2 1]
    /// kernel like anti-aliased text, dark and orange on white, with word gaps.
    #[allow(clippy::cast_possible_truncation)]
    fn synthetic_text_page(w: u32, h: u32) -> Vec<u8> {
        let (wu, hu) = (w as usize, h as usize);
        let mut mask = vec![0u8; wu * hu];
        let (cell_w, cell_h) = (7u32, 14u32);
        for row in 0..h / cell_h {
            for col in 0..(w / cell_w).saturating_sub(2) {
                let glyph = row
                    .wrapping_mul(2_654_435_761)
                    .wrapping_add(col.wrapping_mul(40_503))
                    .rotate_left(row % 13);
                if glyph % 4 == 0 {
                    continue; // a space
                }
                for gy in 0..9u32 {
                    for gx in 0..5u32 {
                        let stem = (gx == 0 && glyph & 1 != 0)
                            || (gx == 4 && glyph & 2 != 0)
                            || (gx == 2 && glyph & 4 != 0 && gy > 1);
                        let bar = (gy == 0 && glyph & 8 != 0)
                            || (gy == 4 && glyph & 16 != 0)
                            || (gy == 8 && glyph & 32 != 0);
                        if stem || bar {
                            let x = (col * cell_w + 1 + gx) as usize;
                            let y = (row * cell_h + 2 + gy) as usize;
                            mask[y * wu + x] = 255;
                        }
                    }
                }
            }
        }
        let mut px = vec![255u8; wu * hu * 4];
        for y in 0..hu {
            for x in 1..wu - 1 {
                let c = (u32::from(mask[y * wu + x - 1])
                    + 2 * u32::from(mask[y * wu + x])
                    + u32::from(mask[y * wu + x + 1]))
                    / 4;
                if c == 0 {
                    continue;
                }
                let ink: [u32; 3] = if (y as u32 / cell_h) % 5 == 2 {
                    [200, 90, 20]
                } else {
                    [30, 30, 30]
                };
                let o = (y * wu + x) * 4;
                for k in 0..3 {
                    px[o + k] = (255 - c * (255 - ink[k]) / 255) as u8;
                }
            }
        }
        px
    }

    fn psnr_db(a: &[u8], b: &[u8]) -> f64 {
        assert_eq!(a.len(), b.len());
        let mut se = 0f64;
        let mut n = 0f64;
        for (pa, pb) in a.as_chunks::<4>().0.iter().zip(b.as_chunks::<4>().0) {
            for c in 0..3 {
                let d = f64::from(pa[c]) - f64::from(pb[c]);
                se += d * d;
                n += 1.0;
            }
        }
        10.0 * (255.0f64 * 255.0 / (se / n).max(1e-9)).log10()
    }

    /// Decode a JPEG to BGRA at the encoded size.
    fn decode_to_bgra(jpeg: &[u8]) -> (Vec<u8>, u32, u32) {
        let img = image::load_from_memory_with_format(jpeg, image::ImageFormat::Jpeg)
            .unwrap()
            .to_rgb8();
        let (w, h) = img.dimensions();
        let mut bgra = Vec::with_capacity((w * h * 4) as usize);
        for p in img.pixels() {
            bgra.extend_from_slice(&[p[2], p[1], p[0], 255]);
        }
        (bgra, w, h)
    }

    #[test]
    fn budget_fit_never_drops_below_the_quality_floor_before_shrinking() {
        let (w, h) = (1280u32, 720u32);
        let page = synthetic_text_page(w, h);
        let mut profile = EffectiveProfile::watch();
        profile.burst_bytes = u32::try_from(MAX_HEADER + 150 * 1024).unwrap();
        let (mut scratch, mut out) = (Vec::new(), Vec::new());
        let (ow, oh) = encode_jpeg_within_budget(
            &page,
            w,
            h,
            &profile,
            select_encode_settings(&profile),
            &mut scratch,
            &mut out,
        )
        .unwrap();
        assert!(out.len() + MAX_HEADER <= profile.burst_bytes as usize);
        // Whatever it took, the picture was never encoded below the floor: decoding it and
        // comparing with the (possibly reduced) source stays well above a q10 result.
        let (dec, dw, dh) = decode_to_bgra(&out);
        assert_eq!((dw, dh), (ow, oh));
        let reference = if (ow, oh) == (w, h) {
            page.clone()
        } else {
            let mut r = Vec::new();
            resize_bgra_into(
                &page,
                w,
                h,
                u16::try_from(ow).unwrap(),
                u16::try_from(oh).unwrap(),
                ResizeFilter::Lanczos3,
                &mut r,
            );
            r
        };
        assert!(psnr_db(&reference, &dec) > 24.0);
    }

    #[test]
    fn watch_profile_is_full_hd_with_sharp_motion_and_a_quick_still() {
        let w = EffectiveProfile::watch();
        assert_eq!((w.max_width, w.max_height), (1920, 1080));
        assert!(w.quality >= 80, "text must stay readable while it moves");
        const { assert!(SETTLE_AFTER.as_millis() <= 300) };
        const { assert!(REFINE_JPEG_QUALITY >= 85) };
        assert!(refine_enabled(&w));
        // A native 1280x800 desktop is never scaled (up or down).
        assert_eq!(
            compute_target_dimensions(1280, 800, w.max_width, w.max_height).unwrap(),
            (1280, 800)
        );
    }

    #[test]
    fn still_screen_refinement_is_lossless_webp_or_a_q92_jpeg() {
        let (w, h) = (1280u32, 720u32);
        let page = synthetic_text_page(w, h);
        let (mut scratch, mut rgb, mut out) = (Vec::new(), Vec::new(), Vec::new());
        let profile = EffectiveProfile::watch();
        let (codec, rw, rh) =
            encode_refined(&page, w, h, &profile, &mut scratch, &mut rgb, &mut out).unwrap();
        assert_eq!((rw, rh), (w, h));
        assert_eq!(codec, Codec::Webp);
        let webp_bytes = out.len();
        let mut dec = image_webp::WebPDecoder::new(std::io::Cursor::new(out.clone())).unwrap();
        let mut rgb_back = vec![0u8; dec.output_buffer_size().unwrap()];
        dec.read_image(&mut rgb_back).unwrap();
        let exact = page
            .as_chunks::<4>()
            .0
            .iter()
            .zip(rgb_back.as_chunks::<3>().0)
            .all(|(p, r)| p[2] == r[0] && p[1] == r[1] && p[0] == r[2]);
        assert!(exact, "lossless refinement must reproduce every pixel");
        println!("refinement 1280x720 text page: lossless WebP = {webp_bytes} B");

        // A burst too small for the lossless image falls back to a JPEG.
        let mut tight = EffectiveProfile::watch();
        tight.burst_bytes = u32::try_from(MAX_HEADER + 64 * 1024).unwrap();
        let (codec, _, _) =
            encode_refined(&page, w, h, &tight, &mut scratch, &mut rgb, &mut out).unwrap();
        assert_eq!(codec, Codec::Jpeg);
        assert!(out.len() + MAX_HEADER <= tight.burst_bytes as usize);
    }

    #[test]
    fn new_defaults_reproduce_small_text_far_better_than_the_old_960_q45_stream() {
        let (w, h) = (1920u32, 1080u32);
        let page = synthetic_text_page(w, h);
        let mut scratch = Vec::new();
        let mut out = Vec::new();

        // New default: native 1920x1080, watch profile, 4:4:4, quality 80.
        let watch = EffectiveProfile::watch();
        let (nw, nh) = encode_jpeg_within_budget(
            &page,
            w,
            h,
            &watch,
            select_encode_settings(&watch),
            &mut scratch,
            &mut out,
        )
        .unwrap();
        assert_eq!((nw, nh), (1920, 1080), "native resolution is kept");
        let new_bytes = out.len();
        let (dec, dw, dh) = decode_to_bgra(&out);
        assert_eq!((dw, dh), (1920, 1080));
        let new_psnr = psnr_db(&page, &dec);

        // Old default: capped to 960x600, quality 45, 4:2:0, shown stretched back up.
        let mut old = EffectiveProfile::watch();
        old.max_width = 960;
        old.max_height = 600;
        old.quality = 45;
        let old_settings = EncodeSettings {
            quality: 45,
            chroma_444: false,
            filter: ResizeFilter::CatmullRom,
        };
        let (ow, oh) =
            encode_jpeg_within_budget(&page, w, h, &old, old_settings, &mut scratch, &mut out)
                .unwrap();
        assert_eq!((ow, oh), (960, 540));
        let old_bytes = out.len();
        let (small, sw, sh) = decode_to_bgra(&out);
        let mut upscaled = Vec::new();
        resize_bgra_into(
            &small,
            sw,
            sh,
            1920,
            1080,
            ResizeFilter::CatmullRom,
            &mut upscaled,
        );
        let old_psnr = psnr_db(&page, &upscaled);

        println!(
            "text page 1920x1080: old 960x540 q45 4:2:0 = {old_bytes} B, {old_psnr:.2} dB; \
             new 1920x1080 q70 4:4:4 = {new_bytes} B, {new_psnr:.2} dB"
        );
        assert!(
            new_psnr > old_psnr + 6.0,
            "new {new_psnr:.2} dB vs old {old_psnr:.2} dB"
        );
        assert!(new_psnr > 30.0, "new stream too lossy: {new_psnr:.2} dB");
        assert!(
            new_bytes + MAX_HEADER <= watch.burst_bytes as usize,
            "a full-resolution frame must fit the watch burst: {new_bytes} B"
        );
    }

    #[test]
    fn normalized_coordinates_map_across_negative_and_mixed_dpi_virtual_desktops() {
        // Secondary left monitor at x = -1920..0 (1920x1080) beside 4K primary at 0..3840 (DPI 192)
        let monitors = vec![
            MonitorDescriptor {
                name: "DP-1".into(),
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
                name: "DP-2".into(),
                is_primary: false,
                rect: PixelRect {
                    x: -1920,
                    y: 0,
                    width: 1920,
                    height: 1080,
                },
                dpi: 96,
            },
        ];
        // Combined virtual bounding box is 5760 x 2160 = 12.44 Mpixels <= 16 Mpixels
        let auto = select_capture_rect(&CaptureScope::Auto, None, &monitors, 1).unwrap();
        assert_eq!(auto.rect.x, -1920);
        assert_eq!(auto.rect.width, 5760);

        let left_only = select_capture_rect(
            &CaptureScope::Monitor {
                selector: "DP-2".into(),
            },
            None,
            &monitors,
            1,
        )
        .unwrap();
        let sr = left_only.rect.to_source_rect();
        assert_eq!(map_normalized_point(0.0, 0.0, &sr), (-1920.0, 0.0));
        assert_eq!(map_normalized_point(1.0, 1.0, &sr), (-1.0, 1079.0));

        // 0..=65535 virtual desktop projection across (-1920..3840)
        assert_eq!(
            point_to_virtual_absolute(-1920.0, 0.0, auto.virtual_rect),
            (0, 0)
        );
        assert_eq!(
            point_to_virtual_absolute(3839.0, 2159.0, auto.virtual_rect),
            (65_535, 65_535)
        );
        // Every integer pixel across a 5760px wide virtual desktop round-trips to the exact pixel
        // under Win32 SendInput's integer division `((abs * width) / 65536) + left`.
        for px in -1920..3839 {
            let (abs_x, _) = point_to_virtual_absolute(f64::from(px), 0.0, auto.virtual_rect);
            let reconstructed =
                i32::try_from((i64::from(abs_x) * 5760) / 65_536).unwrap() + auto.virtual_rect.x;
            assert_eq!(reconstructed, px, "pixel {px} drifted to {reconstructed}");
        }
    }

    #[test]
    fn oversized_virtual_desktop_falls_back_to_primary_on_auto_and_rejects_explicit_virtual() {
        let monitors = vec![
            MonitorDescriptor {
                name: "M1".into(),
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
                name: "M2".into(),
                is_primary: false,
                rect: PixelRect {
                    x: 3840,
                    y: 0,
                    width: 3840,
                    height: 2160,
                },
                dpi: 192,
            },
        ];
        // Combined 7680x2160 = 16.588 Mpixels > 16 Mpixels
        assert_eq!(
            select_capture_rect(&CaptureScope::VirtualDesktop, None, &monitors, 1),
            Err(Reason::FrameBudgetExceeded)
        );
        let auto = select_capture_rect(&CaptureScope::Auto, None, &monitors, 1).unwrap();
        assert_eq!(auto.rect.width, 3840);
        assert_eq!(auto.rect.height, 2160);
    }

    struct MockCapture {
        seq: Arc<AtomicU64>,
        geom: Arc<Mutex<GeometrySnapshot>>,
        fail_next: Arc<AtomicBool>,
        reconnect_mode: Arc<AtomicBool>,
        captures_done: Arc<AtomicU64>,
    }

    impl CaptureDriver for MockCapture {
        fn capture(&mut self, raw_buf: &mut Vec<u8>) -> Result<CapturedRawInfo, CaptureFailure> {
            if self.reconnect_mode.swap(false, Ordering::SeqCst) {
                return Err(CaptureFailure::ConnectionLost(Reason::Disconnected));
            }
            if self.fail_next.swap(false, Ordering::SeqCst) {
                return Err(CaptureFailure::Unavailable(Reason::Unavailable));
            }
            let geom = *self.geom.lock();
            let pixels = (geom.rect.width as usize) * (geom.rect.height as usize) * 4;
            raw_buf.resize(pixels, 0);
            let tick = (self.seq.fetch_add(1, Ordering::Relaxed) & 0xFF) as u8;
            for (i, chunk) in raw_buf.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                chunk[0] = tick.wrapping_add(u8::try_from(i & 0xFF).unwrap_or(0));
                chunk[1] = tick.wrapping_mul(3);
                chunk[2] = 0x40;
                chunk[3] = 0;
            }
            self.captures_done.fetch_add(1, Ordering::Relaxed);
            Ok(CapturedRawInfo {
                geometry: geom,
                format: PixelFormat::Bgra32,
                stride_bytes: geom.rect.width as usize * 4,
            })
        }
    }

    fn test_geom(width: u32, height: u32) -> GeometrySnapshot {
        let rect = PixelRect {
            x: 0,
            y: 0,
            width,
            height,
        };
        GeometrySnapshot {
            rect,
            virtual_rect: rect,
            dpi: 96,
            display_token: 1,
        }
    }

    #[tokio::test]
    async fn single_producer_stops_on_last_drop_and_tracks_geometry_and_generation() {
        let captures_done = Arc::new(AtomicU64::new(0));
        let seq = Arc::new(AtomicU64::new(1));
        let geom = Arc::new(Mutex::new(test_geom(160, 120)));
        let fail_next = Arc::new(AtomicBool::new(false));
        let reconnect_mode = Arc::new(AtomicBool::new(false));
        let driver_creations = Arc::new(AtomicU64::new(0));

        let target = DesktopViewTarget::new(DesktopViewTargetConfig {
            options: LiveViewOptions::new("local-host"),
            initial_geometry: *geom.lock(),
            capture_factory: {
                let captures_done = Arc::clone(&captures_done);
                let seq = Arc::clone(&seq);
                let geom = Arc::clone(&geom);
                let fail_next = Arc::clone(&fail_next);
                let reconnect_mode = Arc::clone(&reconnect_mode);
                let driver_creations = Arc::clone(&driver_creations);
                Arc::new(move |_| {
                    driver_creations.fetch_add(1, Ordering::SeqCst);
                    Ok(Box::new(MockCapture {
                        seq: Arc::clone(&seq),
                        geom: Arc::clone(&geom),
                        fail_next: Arc::clone(&fail_next),
                        reconnect_mode: Arc::clone(&reconnect_mode),
                        captures_done: Arc::clone(&captures_done),
                    }))
                })
            },
            probe: Arc::new(|_| Ok(())),
            input_factory: None,
            input_reason: Some(Reason::Unsupported),
        });

        let src = target.frame_source();
        let mut sub1 = src.subscribe(EffectiveProfile::watch()).await.unwrap();
        let sub2 = src.subscribe(EffectiveProfile::watch()).await.unwrap();
        // Second subscriber reuses the exact same capture driver/producer.
        assert_eq!(driver_creations.load(Ordering::SeqCst), 1);

        let f1 = sub1.frames.borrow_and_update().clone().unwrap();
        assert_eq!(f1.metadata.geometry_revision, 1);
        assert_eq!(f1.metadata.generation, 1);
        assert_eq!(f1.metadata.cause, Cause::Refresh);

        // Slow receiver check: push multiple refreshes rapidly without reading sub2,
        // confirm producer never blocks and latest weak payload drops after last unsubscribe.
        for i in 1..=5 {
            src.request_refresh(i).await.unwrap();
        }
        tokio::time::sleep(Duration::from_millis(80)).await;
        let weak_first = Arc::downgrade(&f1.payload);
        drop(f1);
        assert_eq!(
            sub1.frames
                .borrow_and_update()
                .as_ref()
                .unwrap()
                .metadata
                .action_seq,
            Some(5)
        );

        // Simulate display geometry resize + monitor reconnect
        *geom.lock() = test_geom(320, 240);
        reconnect_mode.store(true, Ordering::SeqCst);
        src.request_refresh(6).await.unwrap();
        let f_resized = loop {
            sub1.frames.changed().await.unwrap();
            if let Some(f) = sub1.frames.borrow_and_update().clone()
                && f.metadata.generation >= 2
            {
                break f;
            }
        };
        assert!(f_resized.metadata.geometry_revision >= 2);
        assert!(f_resized.metadata.generation >= 2);
        let weak_latest = Arc::downgrade(&f_resized.payload);
        drop(f_resized);

        // Drop one subscriber: producer stays alive; drop last: producer & pixels drop cleanly.
        drop(sub2);
        drop(sub1);
        src.stop().await.unwrap();
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert!(weak_first.upgrade().is_none());
        assert!(weak_latest.upgrade().is_none());

        let count_at_stop = captures_done.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(captures_done.load(Ordering::SeqCst), count_at_stop);
    }

    struct GateSync {
        entered: Mutex<bool>,
        unblock: Mutex<bool>,
        cvar: Condvar,
    }

    impl GateSync {
        fn new() -> Self {
            Self {
                entered: Mutex::new(false),
                unblock: Mutex::new(false),
                cvar: Condvar::new(),
            }
        }

        fn signal_entered(&self) {
            *self.entered.lock() = true;
            self.cvar.notify_all();
        }

        fn wait_entered(&self) {
            let mut g = self.entered.lock();
            while !*g {
                self.cvar.wait(&mut g);
            }
        }

        fn release_gate(&self) {
            *self.unblock.lock() = true;
            self.cvar.notify_all();
        }

        fn wait_unblocked_or_cancelled(&self, cancelled: &AtomicBool) -> bool {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut g = self.unblock.lock();
            while !*g && !cancelled.load(Ordering::Acquire) {
                if self.cvar.wait_until(&mut g, deadline).timed_out() {
                    break;
                }
            }
            cancelled.load(Ordering::Acquire)
        }
    }

    struct DeterministicMockInput {
        log: Arc<Mutex<Vec<&'static str>>>,
        partial_next: Arc<AtomicBool>,
        panic_next: Arc<AtomicBool>,
        panic_release_next: Arc<AtomicBool>,
        conn_lost: Arc<AtomicBool>,
        gate: Arc<GateSync>,
        dropped_count: Arc<AtomicU64>,
    }

    impl Drop for DeterministicMockInput {
        fn drop(&mut self) {
            self.dropped_count.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl InputDriver for DeterministicMockInput {
        fn inject(
            &mut self,
            event: &InputEvent,
            _source_rect: SourceRect,
            _virtual_rect: PixelRect,
            cancelled: &AtomicBool,
        ) -> Result<(), ViewError> {
            assert!(
                !self.panic_next.swap(false, Ordering::SeqCst),
                "simulated driver panic"
            );
            match event {
                InputEvent::Text { text } if text == "gated" => {
                    self.log.lock().push("gated_step1");
                    self.gate.signal_entered();
                    if self.gate.wait_unblocked_or_cancelled(cancelled) {
                        self.log.lock().push("gated_cancelled");
                        return Err(ViewError::PartialInjection);
                    }
                    self.log.lock().push("gated_delivered");
                    Ok(())
                }
                InputEvent::Key { .. } => {
                    if self.partial_next.swap(false, Ordering::SeqCst) {
                        return Err(ViewError::PartialInjection);
                    }
                    self.log.lock().push("key");
                    Ok(())
                }
                _ => {
                    self.log.lock().push("other");
                    Ok(())
                }
            }
        }

        fn release_all(&mut self) -> Result<(), ViewError> {
            self.log.lock().push("release_all");
            assert!(
                !self.panic_release_next.swap(false, Ordering::SeqCst),
                "simulated release_all panic"
            );
            if self.conn_lost.load(Ordering::SeqCst) {
                return Err(ViewError::Unavailable);
            }
            Ok(())
        }

        fn is_connection_lost(&self) -> bool {
            self.conn_lost.load(Ordering::SeqCst)
        }
    }

    #[tokio::test]
    async fn input_cancel_and_reset_barrier_blocks_late_events_and_is_idempotent() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let partial_next = Arc::new(AtomicBool::new(false));
        let panic_next = Arc::new(AtomicBool::new(false));
        let conn_lost = Arc::new(AtomicBool::new(false));
        let gate = Arc::new(GateSync::new());
        let dropped_count = Arc::new(AtomicU64::new(0));

        let target = DesktopViewTarget::new(DesktopViewTargetConfig {
            options: LiveViewOptions::new("host"),
            initial_geometry: test_geom(800, 600),
            capture_factory: Arc::new(|_| Err(CaptureFailure::Unavailable(Reason::Unavailable))),
            probe: Arc::new(|_| Ok(())),
            input_factory: Some({
                let log = Arc::clone(&log);
                let partial_next = Arc::clone(&partial_next);
                let panic_next = Arc::clone(&panic_next);
                let conn_lost = Arc::clone(&conn_lost);
                let gate = Arc::clone(&gate);
                let dropped_count = Arc::clone(&dropped_count);
                Arc::new(move || {
                    Ok(Box::new(DeterministicMockInput {
                        log: Arc::clone(&log),
                        partial_next: Arc::clone(&partial_next),
                        panic_next: Arc::clone(&panic_next),
                        panic_release_next: Arc::new(AtomicBool::new(false)),
                        conn_lost: Arc::clone(&conn_lost),
                        gate: Arc::clone(&gate),
                        dropped_count: Arc::clone(&dropped_count),
                    }))
                })
            }),
            input_reason: None,
        });

        let sink = target.input_sink().unwrap();
        // Idempotent reset when nothing is pressed works cleanly.
        sink.release_pressed_inputs().await.unwrap();
        sink.release_pressed_inputs().await.unwrap();

        // Inject before takeover activation must be rejected.
        assert_eq!(
            sink.inject(&InputEvent::Key {
                phase: ::live_view::KeyPhase::Down,
                code: "KeyA".into(),
                key: "a".into(),
                location: 0,
                repeat: false,
                modifiers: vec![],
            })
            .await,
            Err(ViewError::Unavailable)
        );
        sink.set_takeover_active(true).await.unwrap();

        // Start a gated inject, deterministically wait until worker enters step 1, then abort
        // its future and call release_pressed_inputs.
        let sink2 = Arc::clone(&sink);
        let fut = tokio::spawn(async move {
            sink2
                .inject(&InputEvent::Text {
                    text: "gated".into(),
                })
                .await
        });
        let gate_wait = Arc::clone(&gate);
        tokio::task::spawn_blocking(move || gate_wait.wait_entered())
            .await
            .unwrap();
        fut.abort();
        let _ = fut.await;
        // `CancelOnDrop` and `release_pressed_inputs` flip `cancelled` and wake the worker.
        gate.cvar.notify_all();
        sink.release_pressed_inputs().await.unwrap();
        assert!(!log.lock().contains(&"gated_delivered"));
        assert!(log.lock().contains(&"gated_cancelled"));
        assert!(log.lock().contains(&"release_all"));

        // PartialInjection error surfaces verbatim.
        partial_next.store(true, Ordering::SeqCst);
        let res = sink
            .inject(&InputEvent::Key {
                phase: ::live_view::KeyPhase::Down,
                code: "KeyA".into(),
                key: "a".into(),
                location: 0,
                repeat: false,
                modifiers: vec![],
            })
            .await;
        assert_eq!(res, Err(ViewError::PartialInjection));

        // Dropping takeover active blocks subsequent input immediately.
        sink.set_takeover_active(false).await.unwrap();
        assert_eq!(
            sink.inject(&InputEvent::Key {
                phase: ::live_view::KeyPhase::Up,
                code: "KeyA".into(),
                key: "a".into(),
                location: 0,
                repeat: false,
                modifiers: vec![],
            })
            .await,
            Err(ViewError::Unavailable)
        );

        // Codec WebP is rejected cleanly.
        let mut p = EffectiveProfile::watch();
        p.codec = Codec::Webp;
        assert_eq!(
            target.frame_source().subscribe(p).await.err(),
            Some(ViewError::Unsupported)
        );
    }

    #[tokio::test]
    async fn set_takeover_active_false_cancels_in_flight_text_and_releases_before_returning() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let gate = Arc::new(GateSync::new());

        let target = DesktopViewTarget::new(DesktopViewTargetConfig {
            options: LiveViewOptions::new("host-takeover-end"),
            initial_geometry: test_geom(800, 600),
            capture_factory: Arc::new(|_| Err(CaptureFailure::Unavailable(Reason::Unavailable))),
            probe: Arc::new(|_| Ok(())),
            input_factory: Some({
                let log = Arc::clone(&log);
                let gate = Arc::clone(&gate);
                Arc::new(move || {
                    Ok(Box::new(DeterministicMockInput {
                        log: Arc::clone(&log),
                        partial_next: Arc::new(AtomicBool::new(false)),
                        panic_next: Arc::new(AtomicBool::new(false)),
                        panic_release_next: Arc::new(AtomicBool::new(false)),
                        conn_lost: Arc::new(AtomicBool::new(false)),
                        gate: Arc::clone(&gate),
                        dropped_count: Arc::new(AtomicU64::new(0)),
                    }))
                })
            }),
            input_reason: None,
        });

        let sink = target.input_sink().unwrap();
        sink.set_takeover_active(true).await.unwrap();

        let sink_inject = Arc::clone(&sink);
        let inject_fut = tokio::spawn(async move {
            sink_inject
                .inject(&InputEvent::Text {
                    text: "gated".into(),
                })
                .await
        });

        // Wait deterministically until the worker is mid-batch inside `inject`.
        let gate_wait = Arc::clone(&gate);
        tokio::task::spawn_blocking(move || gate_wait.wait_entered())
            .await
            .unwrap();

        // Spawn a background notifier that wakes the condvar once `set_takeover_active(false)`
        // flips `cancelled = true` via `cancel_in_flight_input()`.
        let gate_notify = Arc::clone(&gate);
        let active_cancels_ref = Arc::clone(&target.core);
        let wake_handle = std::thread::spawn(move || {
            while active_cancels_ref.takeover_active.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            gate_notify.cvar.notify_all();
        });

        // `set_takeover_active(false)` must cancel the in-flight batch AND await `ReleaseAll`
        // before returning to the caller.
        sink.set_takeover_active(false).await.unwrap();
        let _ = wake_handle.join();

        let snapshot = log.lock().clone();
        assert_eq!(
            snapshot,
            vec!["gated_step1", "gated_cancelled", "release_all"]
        );
        assert_eq!(inject_fut.await.unwrap(), Err(ViewError::PartialInjection));
    }

    #[tokio::test]
    async fn input_worker_has_no_arc_cycle_joins_on_drop_and_recovers_from_driver_panic() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let panic_next = Arc::new(AtomicBool::new(false));
        let conn_lost = Arc::new(AtomicBool::new(false));
        let dropped_count = Arc::new(AtomicU64::new(0));
        let creations = Arc::new(AtomicU64::new(0));

        let target = DesktopViewTarget::new(DesktopViewTargetConfig {
            options: LiveViewOptions::new("host-lifecycle"),
            initial_geometry: test_geom(800, 600),
            capture_factory: Arc::new(|_| Err(CaptureFailure::Unavailable(Reason::Unavailable))),
            probe: Arc::new(|_| Ok(())),
            input_factory: Some({
                let log = Arc::clone(&log);
                let panic_next = Arc::clone(&panic_next);
                let conn_lost = Arc::clone(&conn_lost);
                let dropped_count = Arc::clone(&dropped_count);
                let creations = Arc::clone(&creations);
                Arc::new(move || {
                    creations.fetch_add(1, Ordering::SeqCst);
                    Ok(Box::new(DeterministicMockInput {
                        log: Arc::clone(&log),
                        partial_next: Arc::new(AtomicBool::new(false)),
                        panic_next: Arc::clone(&panic_next),
                        panic_release_next: Arc::new(AtomicBool::new(false)),
                        conn_lost: Arc::clone(&conn_lost),
                        gate: Arc::new(GateSync::new()),
                        dropped_count: Arc::clone(&dropped_count),
                    }))
                })
            }),
            input_reason: None,
        });

        let sink = target.input_sink().unwrap();
        sink.set_takeover_active(true).await.unwrap();
        assert_eq!(creations.load(Ordering::SeqCst), 1);

        // Even with the input worker thread spawned and running, only `target` and `sink`
        // hold strong references to `TargetCore` (the worker holds a `Weak<TargetCore>`).
        assert_eq!(Arc::strong_count(&target.core), 2);

        // Simulate a panic inside `drv.inject`: worker must catch the panic, run best-effort
        // `release_all`, drop the poisoned driver, and rebuild a fresh driver on the next call.
        panic_next.store(true, Ordering::SeqCst);
        let err_res = sink
            .inject(&InputEvent::Key {
                phase: ::live_view::KeyPhase::Down,
                code: "KeyA".into(),
                key: "a".into(),
                location: 0,
                repeat: false,
                modifiers: vec![],
            })
            .await;
        assert_eq!(err_res, Err(ViewError::Unavailable));
        assert!(log.lock().contains(&"release_all"));
        assert_eq!(dropped_count.load(Ordering::SeqCst), 1);

        // Subsequent inject succeeds by recreating the driver via `input_factory`.
        sink.inject(&InputEvent::Key {
            phase: ::live_view::KeyPhase::Down,
            code: "KeyA".into(),
            key: "a".into(),
            location: 0,
            repeat: false,
            modifiers: vec![],
        })
        .await
        .unwrap();
        assert_eq!(creations.load(Ordering::SeqCst), 2);

        // Simulate transient connection loss during `release_pressed_inputs`: must preserve the
        // driver/reset debt and return `Err(ViewError::Unavailable)` instead of silently claiming
        // `Ok(())`, then succeed once the server connection recovers.
        conn_lost.store(true, Ordering::SeqCst);
        let reset_err = sink.release_pressed_inputs().await;
        assert_eq!(reset_err, Err(ViewError::Unavailable));
        conn_lost.store(false, Ordering::SeqCst);
        let reset_ok = sink.release_pressed_inputs().await;
        assert_eq!(reset_ok, Ok(()));

        // Dropping `sink` and `target` must synchronously shut down and join the worker thread,
        // dropping the active driver before `drop(target)` returns.
        let drops_before = dropped_count.load(Ordering::SeqCst);
        drop(sink);
        drop(target);
        assert!(dropped_count.load(Ordering::SeqCst) > drops_before);
    }

    #[tokio::test]
    async fn takeover_gate_and_stale_geometry_revision_reject_positional_input() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let gate = Arc::new(GateSync::new());

        let target = DesktopViewTarget::new(DesktopViewTargetConfig {
            options: LiveViewOptions::new("host-geom"),
            initial_geometry: test_geom(800, 600),
            capture_factory: Arc::new(|_| Err(CaptureFailure::Unavailable(Reason::Unavailable))),
            probe: Arc::new(|_| Ok(())),
            input_factory: Some({
                let log = Arc::clone(&log);
                let gate = Arc::clone(&gate);
                Arc::new(move || {
                    Ok(Box::new(DeterministicMockInput {
                        log: Arc::clone(&log),
                        partial_next: Arc::new(AtomicBool::new(false)),
                        panic_next: Arc::new(AtomicBool::new(false)),
                        panic_release_next: Arc::new(AtomicBool::new(false)),
                        conn_lost: Arc::new(AtomicBool::new(false)),
                        gate: Arc::clone(&gate),
                        dropped_count: Arc::new(AtomicU64::new(0)),
                    }))
                })
            }),
            input_reason: None,
        });

        let sink = target.input_sink().unwrap();
        sink.set_takeover_active(true).await.unwrap();

        let ptr_event = InputEvent::Pointer {
            phase: ::live_view::PointerPhase::Down,
            x: 0.5,
            y: 0.5,
            button: Some(::live_view::Button::Left),
            buttons: 1,
            modifiers: vec![],
        };

        // 1. Daemon-side explicit revision check (`inject_at_revision`) rejects stale
        // `expected_geometry_revision` or `expected_generation` deterministically.
        assert_eq!(target.current_revision(), (1, 1));
        assert_eq!(
            target.inject_at_revision(&ptr_event, 1, 99).await,
            Err(ViewError::Unavailable)
        );
        assert_eq!(
            target.inject_at_revision(&ptr_event, 99, 1).await,
            Err(ViewError::Unavailable)
        );
        assert_eq!(target.inject_at_revision(&ptr_event, 1, 1).await, Ok(()));
        log.lock().clear();

        // 2. Queue a gated event on the input worker, wait on `gate.wait_entered()`, then
        // enqueue a positional pointer event and bump `geometry_revision` before releasing
        // the gate (zero sleeps).
        let sink_gated = Arc::clone(&sink);
        let gated_fut = tokio::spawn(async move {
            sink_gated
                .inject(&InputEvent::Text {
                    text: "gated".into(),
                })
                .await
        });
        let gate_wait = Arc::clone(&gate);
        tokio::task::spawn_blocking(move || gate_wait.wait_entered())
            .await
            .unwrap();

        let (queued_tx, queued_rx) = oneshot::channel();
        let sink_ptr = Arc::clone(&sink);
        let ptr_clone = ptr_event.clone();
        let ptr_fut = tokio::spawn(async move {
            let _ = queued_tx.send(());
            sink_ptr.inject_at_revision(&ptr_clone, 1, 1).await
        });
        queued_rx.await.unwrap();
        tokio::task::yield_now().await;

        // Simulate monitor topology change before the worker finishes the gated job.
        let (_, new_rev) = update_status_ok(&target.core, test_geom(1024, 768));
        assert_eq!(new_rev, 2);
        gate.release_gate();

        assert_eq!(gated_fut.await.unwrap(), Ok(()));
        assert_eq!(ptr_fut.await.unwrap(), Err(ViewError::Unavailable));
        assert!(!log.lock().contains(&"other"));

        // 3. Even a freshly submitted positional event is rejected until a frame with the new
        // `geometry_revision` (2) has actually been published to subscribers, and legacy
        // revision-less `inject` fails closed for positional events by default.
        assert_eq!(
            sink.inject_at_revision(&ptr_event, 1, new_rev).await,
            Err(ViewError::Unavailable)
        );
        target
            .core
            .last_published_geometry_revision
            .store(new_rev, Ordering::Release);
        assert_eq!(sink.inject(&ptr_event).await, Err(ViewError::Unavailable));
        assert_eq!(
            sink.inject_at_revision(&ptr_event, 1, new_rev).await,
            Ok(())
        );
    }

    #[tokio::test]
    async fn capture_worker_race_on_last_unsubscribe_and_set_profile_after_stop_keep_producer_alive()
     {
        let seq = Arc::new(AtomicU64::new(1));
        let geom = Arc::new(Mutex::new(test_geom(64, 64)));
        let captures_done = Arc::new(AtomicU64::new(0));

        let target = DesktopViewTarget::new(DesktopViewTargetConfig {
            options: LiveViewOptions::new("race-host"),
            initial_geometry: *geom.lock(),
            capture_factory: {
                let seq = Arc::clone(&seq);
                let geom = Arc::clone(&geom);
                let captures_done = Arc::clone(&captures_done);
                Arc::new(move |_| {
                    Ok(Box::new(MockCapture {
                        seq: Arc::clone(&seq),
                        geom: Arc::clone(&geom),
                        fail_next: Arc::new(AtomicBool::new(false)),
                        reconnect_mode: Arc::new(AtomicBool::new(false)),
                        captures_done: Arc::clone(&captures_done),
                    }))
                })
            },
            probe: Arc::new(|_| Ok(())),
            input_factory: None,
            input_reason: Some(Reason::Unsupported),
        });

        let src = target.frame_source();
        let sub1 = src.subscribe(EffectiveProfile::watch()).await.unwrap();

        // Install a deterministic hook that runs inside `try_finish_capture_worker` AFTER
        // `wait_for_capture_wake` returned `None` (on `drop(sub1)`) and BEFORE `ctrl.running`
        // is cleared, simulating a new subscriber arriving in the exact race window.
        let hook_core = Arc::clone(&target.core);
        let (race_sub_tx, race_sub_rx) = oneshot::channel();
        *target.core.pre_finish_hook.lock() = Some(Box::new(move || {
            let rx = hook_core.frames_tx.subscribe();
            {
                let mut ctrl = hook_core.ctrl.lock();
                assert!(
                    ctrl.running,
                    "worker was still marked running in race window"
                );
                ctrl.subscribers += 1;
                ctrl.paused = false;
                ctrl.stop_requested = false;
                ctrl.profile_dirty = true;
                ctrl.pending_refresh = Some(77);
                hook_core.cvar.notify_all();
            }
            let ticket = SubTicket {
                core: Arc::clone(&hook_core),
                armed: true,
            };
            let _ = race_sub_tx.send(FrameSubscription::new(rx, ticket.disarm()));
        }));

        // Dropping `sub1` triggers worker exit -> `pre_finish_hook` adds the new subscriber ->
        // `try_finish_capture_worker` sees `subscribers > 0` and keeps the worker alive!
        drop(sub1);
        let mut sub2 = race_sub_rx.await.unwrap();
        let f_race = loop {
            if let Some(f) = sub2.frames.borrow_and_update().clone()
                && f.metadata.action_seq == Some(77)
            {
                break f;
            }
            sub2.frames.changed().await.unwrap();
        };
        assert_eq!(f_race.metadata.action_seq, Some(77));
        assert!(target.core.ctrl.lock().running);

        // Now call `stop()` while `sub2` is still held (`subscribers == 1, running == false`).
        src.stop().await.unwrap();
        assert!(!target.core.ctrl.lock().running);

        // Calling `set_profile()` after `stop()` while `subscribers > 0` must respawn the
        // capture worker so `sub2` receives fresh frames again.
        src.set_profile(EffectiveProfile::live()).await.unwrap();
        src.request_refresh(88).await.unwrap();
        let f_resumed = loop {
            if let Some(f) = sub2.frames.borrow_and_update().clone()
                && f.metadata.action_seq == Some(88)
            {
                break f;
            }
            sub2.frames.changed().await.unwrap();
        };
        assert_eq!(f_resumed.metadata.action_seq, Some(88));
        src.stop().await.unwrap();
    }

    struct NoisyCapture {
        geom: GeometrySnapshot,
        seed: Arc<AtomicU64>,
        calls: Arc<AtomicU64>,
        called_notify: Arc<Condvar>,
        called_lock: Arc<Mutex<u64>>,
    }

    impl CaptureDriver for NoisyCapture {
        fn capture(&mut self, raw_buf: &mut Vec<u8>) -> Result<CapturedRawInfo, CaptureFailure> {
            let len = (self.geom.rect.width as usize) * (self.geom.rect.height as usize) * 4;
            raw_buf.resize(len, 0);
            let mut s = self.seed.load(Ordering::Acquire);
            for b in raw_buf.iter_mut() {
                s = s.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                *b = (s >> 56) as u8;
            }
            let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            *self.called_lock.lock() = n;
            self.called_notify.notify_all();
            Ok(CapturedRawInfo {
                geometry: self.geom,
                format: PixelFormat::Bgra32,
                stride_bytes: self.geom.rect.width as usize * 4,
            })
        }
    }

    #[tokio::test]
    async fn capture_worker_backs_off_on_encode_failure_and_caches_hash_on_budget_skip() {
        let geom = test_geom(640, 480);
        let seed = Arc::new(AtomicU64::new(0x1234_5678));
        let calls = Arc::new(AtomicU64::new(0));
        let called_notify = Arc::new(Condvar::new());
        let called_lock = Arc::new(Mutex::new(0u64));

        let target = DesktopViewTarget::new(DesktopViewTargetConfig {
            options: LiveViewOptions::new("budget-host"),
            initial_geometry: geom,
            capture_factory: {
                let seed = Arc::clone(&seed);
                let calls = Arc::clone(&calls);
                let called_notify = Arc::clone(&called_notify);
                let called_lock = Arc::clone(&called_lock);
                Arc::new(move |_| {
                    Ok(Box::new(NoisyCapture {
                        geom,
                        seed: Arc::clone(&seed),
                        calls: Arc::clone(&calls),
                        called_notify: Arc::clone(&called_notify),
                        called_lock: Arc::clone(&called_lock),
                    }))
                })
            },
            probe: Arc::new(|_| Ok(())),
            input_factory: None,
            input_reason: Some(Reason::Unsupported),
        });

        let src = target.frame_source();
        // 1. Impossible JPEG budget (> MAX_HEADER so `validate_profile` passes, but too small
        // for any valid JPEG): `subscribe` must return `Err(ViewError::LimitExceeded)`, record
        // `Reason::FrameBudgetExceeded`, and NOT spin in a tight CPU loop.
        let mut tight = EffectiveProfile::live();
        tight.burst_bytes = u32::try_from(MAX_HEADER + 32).unwrap();
        let started = std::time::Instant::now();
        let err = src.subscribe(tight).await.err();
        assert_eq!(err, Some(ViewError::LimitExceeded));
        // Never a tight loop: at most one capture per retry interval that really elapsed (plus
        // the first one and one in flight), however slow or loaded the machine is.
        let allowed = 2 + started.elapsed().as_millis() / UNAVAILABLE_RETRY_INTERVAL.as_millis();
        assert!(u128::from(calls.load(Ordering::SeqCst)) <= allowed);
        assert_eq!(
            target.stats().last_unavailable_reason,
            Some(Reason::FrameBudgetExceeded)
        );

        // 2. Unit-test `execute_capture_step` token-budget skip directly: when a periodic tick
        // sees a new screen hash (`0xCAFE`) but `token_bytes < cost`, it skips publishing for
        // budget AND updates `cache.last_hash = Some(hash)` so the next periodic tick on the
        // same screen hits `can_reuse` (`frames_skipped_unchanged`) instead of re-encoding JPEG.
        let mut profile = EffectiveProfile::live();
        profile.bytes_per_second = 0;
        let mut cache = CaptureSessionCache::new(profile.burst_bytes);
        let mut bufs = WorkerBuffers {
            raw: Vec::new(),
            bgra: Vec::new(),
            scaled: Vec::new(),
            rgb: Vec::new(),
            jpeg: Vec::new(),
        };
        let mut drv: Option<Box<dyn CaptureDriver>> = Some(Box::new(NoisyCapture {
            geom: test_geom(64, 64),
            seed: Arc::clone(&seed),
            calls: Arc::clone(&calls),
            called_notify: Arc::clone(&called_notify),
            called_lock: Arc::clone(&called_lock),
        }));
        let mut saw_error = false;

        // First forced capture encodes and caches frame 1.
        let _ = execute_capture_step(
            &target.core,
            &mut drv,
            &mut saw_error,
            &mut cache,
            &mut bufs,
            &profile,
            Some(0),
            false,
        );
        let encoded_after_first = target.stats().frames_encoded;
        assert_eq!(encoded_after_first, 1);

        // Drain token bucket and change screen content (`seed = 0x9999`).
        cache.token_bytes = 0.0;
        seed.store(0x9999, Ordering::Release);
        let _ = execute_capture_step(
            &target.core,
            &mut drv,
            &mut saw_error,
            &mut cache,
            &mut bufs,
            &profile,
            None,
            false,
        );
        assert_eq!(target.stats().frames_encoded, 1);
        assert!(cache.last_hash.is_some());

        // Next periodic tick with the same screen (`seed = 0x9999`) must hit `can_reuse` and
        // increment `frames_skipped_unchanged` without re-encoding JPEG.
        let skipped_unchanged_before = target.stats().frames_skipped_unchanged;
        let skipped_budget_before = target.stats().frames_skipped_budget;
        let _ = execute_capture_step(
            &target.core,
            &mut drv,
            &mut saw_error,
            &mut cache,
            &mut bufs,
            &profile,
            None,
            false,
        );
        assert_eq!(
            target.stats().frames_skipped_unchanged,
            skipped_unchanged_before + 1
        );
        assert_eq!(target.stats().frames_skipped_budget, skipped_budget_before);
    }

    #[tokio::test]
    async fn static_idle_repeated_frame_preserves_seq_and_captured_at() {
        struct StaticCapture {
            geom: GeometrySnapshot,
        }
        impl CaptureDriver for StaticCapture {
            fn capture(
                &mut self,
                raw_buf: &mut Vec<u8>,
            ) -> Result<CapturedRawInfo, CaptureFailure> {
                let len = (self.geom.rect.width as usize) * (self.geom.rect.height as usize) * 4;
                raw_buf.clear();
                raw_buf.resize(len, 0x7F);
                Ok(CapturedRawInfo {
                    geometry: self.geom,
                    format: PixelFormat::Bgra32,
                    stride_bytes: self.geom.rect.width as usize * 4,
                })
            }
        }

        let geom = test_geom(64, 64);
        let target = DesktopViewTarget::new(DesktopViewTargetConfig {
            options: LiveViewOptions::new("static-host"),
            initial_geometry: geom,
            capture_factory: Arc::new(move |_| Ok(Box::new(StaticCapture { geom }))),
            probe: Arc::new(|_| Ok(())),
            input_factory: None,
            input_reason: Some(Reason::Unsupported),
        });

        let src = target.frame_source();
        let mut sub = src.subscribe(EffectiveProfile::live()).await.unwrap();
        let first = sub.frames.borrow_and_update().clone().unwrap();
        assert_eq!(first.metadata.frame_seq, 1);
        assert!(!first.metadata.repeated);

        // Wait for Live keepalive heartbeat on static desktop (>800 ms). A static live screen
        // first gets one sharper refinement frame (a new, non-repeated frame); the keepalive
        // then repeats that latest frame.
        let mut latest_real = first;
        let repeated = loop {
            sub.frames.changed().await.unwrap();
            let f = sub.frames.borrow_and_update().clone().unwrap();
            if f.metadata.repeated {
                break f;
            }
            latest_real = f;
        };
        assert!(latest_real.metadata.frame_seq <= 2);
        assert_eq!(repeated.metadata.frame_seq, latest_real.metadata.frame_seq);
        assert_eq!(
            repeated.metadata.captured_at_ms,
            latest_real.metadata.captured_at_ms
        );
        assert_eq!(repeated.metadata.cause, Cause::Idle);
        src.stop().await.unwrap();
    }

    #[tokio::test]
    async fn release_all_panic_keeps_reset_owed_and_returns_unavailable() {
        // 1. Direct panic inside `release_all` must return `Err(ViewError::Unavailable)` and keep
        // reset owed across subsequent `release_pressed_inputs` calls.
        let log = Arc::new(Mutex::new(Vec::new()));
        let panic_next = Arc::new(AtomicBool::new(false));
        let panic_release_next = Arc::new(AtomicBool::new(false));
        let target = DesktopViewTarget::new(DesktopViewTargetConfig {
            options: LiveViewOptions::new("host-release-panic"),
            initial_geometry: test_geom(800, 600),
            capture_factory: Arc::new(|_| Err(CaptureFailure::Unavailable(Reason::Unavailable))),
            probe: Arc::new(|_| Ok(())),
            input_factory: Some({
                let log = Arc::clone(&log);
                let panic_next = Arc::clone(&panic_next);
                let panic_release_next = Arc::clone(&panic_release_next);
                Arc::new(move || {
                    Ok(Box::new(DeterministicMockInput {
                        log: Arc::clone(&log),
                        partial_next: Arc::new(AtomicBool::new(false)),
                        panic_next: Arc::clone(&panic_next),
                        panic_release_next: Arc::clone(&panic_release_next),
                        conn_lost: Arc::new(AtomicBool::new(false)),
                        gate: Arc::new(GateSync::new()),
                        dropped_count: Arc::new(AtomicU64::new(0)),
                    }))
                })
            }),
            input_reason: None,
        });

        let sink = target.input_sink().unwrap();
        sink.set_takeover_active(true).await.unwrap();
        sink.inject(&InputEvent::Key {
            phase: ::live_view::KeyPhase::Down,
            code: "KeyA".into(),
            key: "a".into(),
            location: 0,
            repeat: false,
            modifiers: vec![],
        })
        .await
        .unwrap();

        panic_release_next.store(true, Ordering::SeqCst);
        assert_eq!(
            sink.release_pressed_inputs().await,
            Err(ViewError::Unavailable)
        );
        // Subsequent release attempt still surfaces the unverified reset debt cleanly.
        assert_eq!(
            sink.release_pressed_inputs().await,
            Err(ViewError::Unavailable)
        );

        // 2. Panic inside `inject` followed by a panic in the follow-up `release_all` must also
        // mark the reset debt unclean so `release_pressed_inputs()` returns `Unavailable`.
        let log2 = Arc::new(Mutex::new(Vec::new()));
        let panic_next2 = Arc::new(AtomicBool::new(false));
        let panic_release_next2 = Arc::new(AtomicBool::new(false));
        let target2 = DesktopViewTarget::new(DesktopViewTargetConfig {
            options: LiveViewOptions::new("host-both-panic"),
            initial_geometry: test_geom(800, 600),
            capture_factory: Arc::new(|_| Err(CaptureFailure::Unavailable(Reason::Unavailable))),
            probe: Arc::new(|_| Ok(())),
            input_factory: Some({
                let log2 = Arc::clone(&log2);
                let panic_next2 = Arc::clone(&panic_next2);
                let panic_release_next2 = Arc::clone(&panic_release_next2);
                Arc::new(move || {
                    Ok(Box::new(DeterministicMockInput {
                        log: Arc::clone(&log2),
                        partial_next: Arc::new(AtomicBool::new(false)),
                        panic_next: Arc::clone(&panic_next2),
                        panic_release_next: Arc::clone(&panic_release_next2),
                        conn_lost: Arc::new(AtomicBool::new(false)),
                        gate: Arc::new(GateSync::new()),
                        dropped_count: Arc::new(AtomicU64::new(0)),
                    }))
                })
            }),
            input_reason: None,
        });

        let sink2 = target2.input_sink().unwrap();
        sink2.set_takeover_active(true).await.unwrap();
        panic_next2.store(true, Ordering::SeqCst);
        panic_release_next2.store(true, Ordering::SeqCst);
        assert_eq!(
            sink2
                .inject(&InputEvent::Key {
                    phase: ::live_view::KeyPhase::Down,
                    code: "KeyB".into(),
                    key: "b".into(),
                    location: 0,
                    repeat: false,
                    modifiers: vec![],
                })
                .await,
            Err(ViewError::Unavailable)
        );
        assert_eq!(
            sink2.release_pressed_inputs().await,
            Err(ViewError::Unavailable)
        );
    }

    #[tokio::test]
    async fn inject_at_revision_rejects_stale_revision_after_frame_published_and_enforces_explicit_mode()
     {
        let log = Arc::new(Mutex::new(Vec::new()));
        let gate = Arc::new(GateSync::new());

        let target = DesktopViewTarget::new(DesktopViewTargetConfig {
            options: LiveViewOptions::new("host-explicit-rev"),
            initial_geometry: test_geom(800, 600),
            capture_factory: Arc::new(|_| Err(CaptureFailure::Unavailable(Reason::Unavailable))),
            probe: Arc::new(|_| Ok(())),
            input_factory: Some({
                let log = Arc::clone(&log);
                let gate = Arc::clone(&gate);
                Arc::new(move || {
                    Ok(Box::new(DeterministicMockInput {
                        log: Arc::clone(&log),
                        partial_next: Arc::new(AtomicBool::new(false)),
                        panic_next: Arc::new(AtomicBool::new(false)),
                        panic_release_next: Arc::new(AtomicBool::new(false)),
                        conn_lost: Arc::new(AtomicBool::new(false)),
                        gate: Arc::clone(&gate),
                        dropped_count: Arc::new(AtomicU64::new(0)),
                    }))
                })
            }),
            input_reason: None,
        });

        let desktop_sink = target.desktop_input_sink().unwrap();
        let dyn_sink = target.input_sink().unwrap();
        dyn_sink.set_takeover_active(true).await.unwrap();

        let ptr_event = InputEvent::Pointer {
            phase: ::live_view::PointerPhase::Down,
            x: 0.5,
            y: 0.5,
            button: Some(::live_view::Button::Left),
            buttons: 1,
            modifiers: vec![],
        };

        // Advance geometry revision to 2 AND publish frame at revision 2.
        let (generation, new_rev) = update_status_ok(&target.core, test_geom(1280, 720));
        assert_eq!((generation, new_rev), (1, 2));
        target
            .core
            .last_published_geometry_revision
            .store(new_rev, Ordering::Release);

        // A click computed for revision 1 must still be rejected even after revision 2 is published.
        assert_eq!(
            desktop_sink.inject_at_revision(&ptr_event, 1, 1).await,
            Err(ViewError::Unavailable)
        );
        assert_eq!(
            target.inject_at_revision(&ptr_event, 1, 1).await,
            Err(ViewError::Unavailable)
        );
        assert!(!log.lock().contains(&"other"));
        assert_eq!(
            desktop_sink
                .inject_at_revision(&ptr_event, generation, new_rev)
                .await,
            Ok(())
        );
        log.lock().clear();

        // Worker-queue race: queue `inject_at_revision(..., rev 2)` behind a gated job, bump +
        // publish rev 3 while queued, and verify the worker rejects the stale rev 2 event.
        let sink_gated = Arc::clone(&dyn_sink);
        let gated_fut = tokio::spawn(async move {
            sink_gated
                .inject(&InputEvent::Text {
                    text: "gated".into(),
                })
                .await
        });
        let gate_wait = Arc::clone(&gate);
        tokio::task::spawn_blocking(move || gate_wait.wait_entered())
            .await
            .unwrap();
        let (queued_tx, queued_rx) = oneshot::channel();
        let desktop_sink_queued = Arc::clone(&desktop_sink);
        let ptr_clone = ptr_event.clone();
        let ptr_fut = tokio::spawn(async move {
            let _ = queued_tx.send(());
            desktop_sink_queued
                .inject_at_revision(&ptr_clone, generation, new_rev)
                .await
        });
        queued_rx.await.unwrap();
        tokio::task::yield_now().await;

        let (_, rev3) = update_status_ok(&target.core, test_geom(1920, 1080));
        target
            .core
            .last_published_geometry_revision
            .store(rev3, Ordering::Release);
        gate.release_gate();

        assert_eq!(gated_fut.await.unwrap(), Ok(()));
        assert_eq!(ptr_fut.await.unwrap(), Err(ViewError::Unavailable));
        assert!(!log.lock().contains(&"other"));

        // Explicit revision mode is enabled by default for desktop targets: legacy `InputSink::inject`
        // fails closed for positional events, while `InputSink::inject_at_revision` succeeds.
        assert_eq!(
            dyn_sink.inject(&ptr_event).await,
            Err(ViewError::Unavailable)
        );
        assert_eq!(
            dyn_sink
                .inject_at_revision(&ptr_event, generation, rev3)
                .await,
            Ok(())
        );
    }

    #[tokio::test]
    async fn background_availability_probe_updates_info_without_blocking_on_display_loss_and_inject_error()
     {
        struct FailingKeyInput(Arc<AtomicBool>);
        impl InputDriver for FailingKeyInput {
            fn inject(
                &mut self,
                _: &InputEvent,
                _: SourceRect,
                _: PixelRect,
                _: &AtomicBool,
            ) -> Result<(), ViewError> {
                if self.0.swap(false, Ordering::SeqCst) {
                    return Err(ViewError::Unavailable);
                }
                Ok(())
            }
            fn release_all(&mut self) -> Result<(), ViewError> {
                Ok(())
            }
        }

        let probe_gate = Arc::new(GateSync::new());
        let fail_probe = Arc::new(AtomicBool::new(false));
        let block_next_probe = Arc::new(AtomicBool::new(false));
        let inject_fail_next = Arc::new(AtomicBool::new(false));
        let probe_calls = Arc::new(AtomicU64::new(0));

        let target = DesktopViewTarget::new(DesktopViewTargetConfig {
            options: LiveViewOptions::new("host-probe"),
            initial_geometry: test_geom(800, 600),
            capture_factory: Arc::new(|_| Err(CaptureFailure::Unavailable(Reason::Unavailable))),
            probe: {
                let (pg, fp, bp, pc) = (
                    Arc::clone(&probe_gate),
                    Arc::clone(&fail_probe),
                    Arc::clone(&block_next_probe),
                    Arc::clone(&probe_calls),
                );
                Arc::new(move |_| {
                    pc.fetch_add(1, Ordering::SeqCst);
                    if bp.swap(false, Ordering::SeqCst) {
                        pg.signal_entered();
                        let _ = pg.wait_unblocked_or_cancelled(&AtomicBool::new(false));
                    }
                    if fp.load(Ordering::SeqCst) {
                        Err(Reason::Disconnected)
                    } else {
                        Ok(())
                    }
                })
            },
            input_factory: Some({
                let fail_next = Arc::clone(&inject_fail_next);
                Arc::new(move || Ok(Box::new(FailingKeyInput(Arc::clone(&fail_next)))))
            }),
            input_reason: None,
        });

        assert!(target.info().capabilities.view);

        // Block probe on a barrier returning `Reason::Disconnected`; `info()` must not block.
        fail_probe.store(true, Ordering::SeqCst);
        block_next_probe.store(true, Ordering::SeqCst);
        target.set_availability_probe_interval(Duration::from_millis(1));
        target.trigger_availability_probe();
        let probe_wait = Arc::clone(&probe_gate);
        tokio::task::spawn_blocking(move || probe_wait.wait_entered())
            .await
            .unwrap();
        let started = Instant::now();
        assert!(target.info().capabilities.view);
        assert!(started.elapsed() < Duration::from_millis(200));

        probe_gate.release_gate();
        while target.info().capabilities.view {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(target.info().unavailable_reason, Some(Reason::Disconnected));

        // Recover, raise interval to 1h, and confirm an inject `Unavailable` error while idle
        // immediately triggers a probe that picks up `Reason::Disconnected`.
        fail_probe.store(false, Ordering::SeqCst);
        target.set_availability_probe_interval(Duration::from_secs(3600));
        target.trigger_availability_probe();
        while !target.info().capabilities.view {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let sink = target.input_sink().unwrap();
        sink.set_takeover_active(true).await.unwrap();
        let calls_before = probe_calls.load(Ordering::SeqCst);
        fail_probe.store(true, Ordering::SeqCst);
        inject_fail_next.store(true, Ordering::SeqCst);
        assert_eq!(
            sink.inject(&InputEvent::Key {
                phase: ::live_view::KeyPhase::Down,
                code: "KeyA".into(),
                key: "a".into(),
                location: 0,
                repeat: false,
                modifiers: vec![],
            })
            .await,
            Err(ViewError::Unavailable)
        );
        while probe_calls.load(Ordering::SeqCst) == calls_before || target.info().capabilities.view
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(target.info().unavailable_reason, Some(Reason::Disconnected));
    }

    #[tokio::test]
    async fn wedged_probe_times_out_and_later_probe_recovers() {
        let probe_gate = Arc::new(GateSync::new());
        let block_next_probe = Arc::new(AtomicBool::new(true));
        let fail_while_blocked = Arc::new(AtomicBool::new(true));
        let probe_calls = Arc::new(AtomicU64::new(0));

        let target = DesktopViewTarget::new(DesktopViewTargetConfig {
            options: LiveViewOptions::new("host-probe-timeout"),
            initial_geometry: test_geom(800, 600),
            capture_factory: Arc::new(|_| Err(CaptureFailure::Unavailable(Reason::Unavailable))),
            probe: {
                let (pg, bp, fp, pc) = (
                    Arc::clone(&probe_gate),
                    Arc::clone(&block_next_probe),
                    Arc::clone(&fail_while_blocked),
                    Arc::clone(&probe_calls),
                );
                Arc::new(move |_| {
                    pc.fetch_add(1, Ordering::SeqCst);
                    if bp.swap(false, Ordering::SeqCst) {
                        pg.signal_entered();
                        let _ = pg.wait_unblocked_or_cancelled(&AtomicBool::new(false));
                        return Err(Reason::Unavailable);
                    }
                    if fp.load(Ordering::SeqCst) {
                        Err(Reason::Unavailable)
                    } else {
                        Ok(())
                    }
                })
            },
            input_factory: None,
            input_reason: Some(Reason::Unsupported),
        });

        target.set_availability_probe_timeout(Duration::from_millis(40));
        target.set_availability_probe_interval(Duration::from_millis(15));
        target.trigger_availability_probe();

        let probe_wait = Arc::clone(&probe_gate);
        tokio::task::spawn_blocking(move || probe_wait.wait_entered())
            .await
            .unwrap();

        // Wait past the probe timeout while the first call is still wedged:
        // `info()` must report `Unavailable`, and a second probe must NOT pile up behind it.
        while target.info().capabilities.view {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(target.info().unavailable_reason, Some(Reason::Unavailable));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(probe_calls.load(Ordering::SeqCst), 1);

        // Now allow the wedged probe call to return and the display to recover.
        fail_while_blocked.store(false, Ordering::SeqCst);
        probe_gate.release_gate();

        while !target.info().capabilities.view {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(probe_calls.load(Ordering::SeqCst) >= 2);
        assert_eq!(target.info().unavailable_reason, None);
    }

    #[tokio::test]
    async fn dropping_target_with_wedged_probe_returns_within_bound() {
        // 1. A bounded probe (simulating bounded socket connect/read) is joined on Drop
        // before Drop returns, leaving no background thread or connection behind.
        let bounded_finished = Arc::new(AtomicBool::new(false));
        let bounded_entered = Arc::new(GateSync::new());
        let target1 = DesktopViewTarget::new(DesktopViewTargetConfig {
            options: LiveViewOptions::new("host-drop-bounded"),
            initial_geometry: test_geom(800, 600),
            capture_factory: Arc::new(|_| Err(CaptureFailure::Unavailable(Reason::Unavailable))),
            probe: {
                let (entered, finished) =
                    (Arc::clone(&bounded_entered), Arc::clone(&bounded_finished));
                Arc::new(move |_| {
                    entered.signal_entered();
                    std::thread::sleep(Duration::from_millis(70));
                    finished.store(true, Ordering::SeqCst);
                    Err(Reason::Unavailable)
                })
            },
            input_factory: None,
            input_reason: Some(Reason::Unsupported),
        });
        target1.set_availability_probe_timeout(Duration::from_millis(40));
        target1.trigger_availability_probe();
        let wait1 = Arc::clone(&bounded_entered);
        tokio::task::spawn_blocking(move || wait1.wait_entered())
            .await
            .unwrap();
        let drop_start1 = Instant::now();
        drop(target1);
        assert!(bounded_finished.load(Ordering::SeqCst));
        assert!(drop_start1.elapsed() < Duration::from_millis(400));

        // 2. Even an indefinitely wedged probe call must not hang Drop beyond the bound.
        let stuck_gate = Arc::new(GateSync::new());
        let target2 = DesktopViewTarget::new(DesktopViewTargetConfig {
            options: LiveViewOptions::new("host-drop-stuck"),
            initial_geometry: test_geom(800, 600),
            capture_factory: Arc::new(|_| Err(CaptureFailure::Unavailable(Reason::Unavailable))),
            probe: {
                let gate = Arc::clone(&stuck_gate);
                Arc::new(move |_| {
                    gate.signal_entered();
                    let _ = gate.wait_unblocked_or_cancelled(&AtomicBool::new(false));
                    Err(Reason::Unavailable)
                })
            },
            input_factory: None,
            input_reason: Some(Reason::Unsupported),
        });
        target2.set_availability_probe_timeout(Duration::from_millis(40));
        target2.trigger_availability_probe();
        let wait2 = Arc::clone(&stuck_gate);
        tokio::task::spawn_blocking(move || wait2.wait_entered())
            .await
            .unwrap();
        let drop_start2 = Instant::now();
        drop(target2);
        let elapsed2 = drop_start2.elapsed();
        stuck_gate.release_gate();
        assert!(
            elapsed2 < Duration::from_millis(500),
            "drop took {elapsed2:?}"
        );
    }
}
