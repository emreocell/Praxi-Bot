//! Shared OS-independent live-view runtime, pixel adaptation, geometry math,
//! and worker coordination for desktop backends.
//!
//! Platform crates (`desktop-linux`, `desktop-windows`) implement the thin OS
//! capture and input traits defined here so subscription lifecycle, bounding
//! checks, scaling, RAM-only JPEG encoding, geometry revision tracking, and
//! cancel-safe input ordering are shared across OS backends.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use jpeg_encoder::{ColorType as JpegColorType, Encoder as JpegEncoder};
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
#[must_use]
pub fn point_to_virtual_absolute(phys_x: f64, phys_y: f64, virtual_rect: PixelRect) -> (i32, i32) {
    let span_x = f64::from(virtual_rect.width.max(2) - 1);
    let span_y = f64::from(virtual_rect.height.max(2) - 1);
    let ax = (phys_x - f64::from(virtual_rect.x)) * 65_535.0 / span_x;
    let ay = (phys_y - f64::from(virtual_rect.y)) * 65_535.0 / span_y;
    // Inputs are finite and clamped into `0.0..=65_535.0` before truncating.
    #[allow(clippy::cast_possible_truncation)]
    (
        ax.round().clamp(0.0, 65_535.0) as i32,
        ay.round().clamp(0.0, 65_535.0) as i32,
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
    let cap_w = u64::from(max_w.min(MAX_OUTPUT_WIDTH));
    let cap_h = u64::from(max_h.min(MAX_OUTPUT_HEIGHT));
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

/// Resize tightly packed top-down `BGRA` pixels into `dst` using fixed-point
/// bilinear interpolation.
///
/// # Panics
///
/// Panics only if caller supplies `src` shorter than `src_w * src_h * 4` or zero dimensions.
pub fn downscale_bgra_bilinear_into(
    src: &[u8],
    src_w: u32,
    src_h: u32,
    dst_w: u16,
    dst_h: u16,
    dst: &mut Vec<u8>,
) {
    let dw = usize::from(dst_w);
    let dh = usize::from(dst_h);
    let sw = src_w as usize;
    let sh = src_h as usize;
    let needed_src = sw * sh * 4;
    assert!(src.len() >= needed_src && sw > 0 && sh > 0 && dw > 0 && dh > 0);

    let out_len = dw * dh * 4;
    dst.clear();
    if sw == dw && sh == dh {
        dst.extend_from_slice(&src[..needed_src]);
        return;
    }
    dst.resize(out_len, 0);

    // Precompute X lookup indices + 8-bit fractional weights outside row loops.
    let x_lut: Vec<(usize, usize, u32)> = (0..dw)
        .map(|dx| {
            let fx = (dx as u64) * ((sw as u64 - 1) << 8) / (dw as u64 - 1).max(1);
            let x0 = usize::try_from(fx >> 8).unwrap_or(0).min(sw - 1);
            let x1 = (x0 + 1).min(sw - 1);
            let w = u32::try_from(fx & 0xFF).unwrap_or(0);
            (x0 * 4, x1 * 4, w)
        })
        .collect();

    let row_stride = sw * 4;
    for dy in 0..dh {
        let fy_q = (dy as u64) * ((sh as u64 - 1) << 8) / (dh as u64 - 1).max(1);
        let y0 = usize::try_from(fy_q >> 8).unwrap_or(0).min(sh - 1);
        let y1 = (y0 + 1).min(sh - 1);
        let wy = u32::try_from(fy_q & 0xFF).unwrap_or(0);
        let comp_y = 256 - wy;

        let r0 = &src[y0 * row_stride..(y0 + 1) * row_stride];
        let r1 = &src[y1 * row_stride..(y1 + 1) * row_stride];
        let dst_row = &mut dst[dy * dw * 4..(dy + 1) * dw * 4];

        for (dx, &(ox0, ox1, wx)) in x_lut.iter().enumerate() {
            let comp_x = 256 - wx;
            let w00 = comp_x * comp_y;
            let w01 = wx * comp_y;
            let w10 = comp_x * wy;
            let w11 = wx * wy;
            let out_px = &mut dst_row[dx * 4..dx * 4 + 4];
            for c in 0..3 {
                let p = u32::from(r0[ox0 + c]) * w00
                    + u32::from(r0[ox1 + c]) * w01
                    + u32::from(r1[ox0 + c]) * w10
                    + u32::from(r1[ox1 + c]) * w11;
                // Maximum sum is 255 * 65536, shifted down to 0..=255.
                #[allow(clippy::cast_possible_truncation)]
                {
                    out_px[c] = (p >> 16) as u8;
                }
            }
            out_px[3] = 255;
        }
    }
}

/// Resize (if needed) and JPEG-encode tightly packed `BGRA` pixels completely in RAM,
/// stepping quality and dimensions down if necessary so the payload + [`MAX_HEADER`]
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
    let mut quality = profile.quality.clamp(10, 95);

    loop {
        let pixel_slice: &[u8] = if src_w == u32::from(cur_w) && src_h == u32::from(cur_h) {
            &bgra[..needed]
        } else {
            downscale_bgra_bilinear_into(bgra, src_w, src_h, cur_w, cur_h, scratch_scaled);
            scratch_scaled.as_slice()
        };
        out_jpeg.clear();
        JpegEncoder::new(&mut *out_jpeg, quality)
            .encode(pixel_slice, cur_w, cur_h, JpegColorType::Bgra)
            .map_err(|_| ViewError::InvalidRecord)?;

        if !out_jpeg.is_empty() && out_jpeg.len() <= max_payload {
            return Ok((u32::from(cur_w), u32::from(cur_h)));
        }

        if quality > 15 {
            quality = (quality * 3 / 4).max(15);
            continue;
        }
        if quality > 10 {
            quality = 10;
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
        cur_w = nw;
        cur_h = nh;
        quality = 35;
    }
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
}

/// Factory creating a fresh [`CaptureDriver`] bound to a capture scope.
pub type CaptureDriverFactory =
    Arc<dyn Fn(&CaptureScope) -> Result<Box<dyn CaptureDriver>, CaptureFailure> + Send + Sync>;

/// Optional lightweight availability probe run when [`ViewTarget::info`] is queried offline.
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

struct TargetCore {
    target_id: Uuid,
    options: LiveViewOptions,
    capture_factory: CaptureDriverFactory,
    probe: AvailabilityProbeFn,
    input_factory: Option<InputDriverFactory>,
    stats: Arc<LiveViewStats>,
    status: Mutex<TargetStatus>,
    ctrl: Mutex<CaptureControl>,
    cvar: Condvar,
    frames_tx: watch::Sender<Option<Arc<Frame>>>,
    takeover_active: AtomicBool,
    input_epoch: AtomicU64,
    input_worker: Mutex<Option<InputWorker>>,
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
            target_id: Uuid::now_v7(),
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
            input_worker: Mutex::new(None),
        });
        Self { core }
    }

    /// Snapshot current content-free observability counters.
    #[must_use]
    pub fn stats(&self) -> LiveViewStatsSnapshot {
        self.core.stats.snapshot()
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

impl Drop for TargetCore {
    fn drop(&mut self) {
        let cap_join = {
            let mut ctrl = self.ctrl.lock();
            ctrl.shutdown = true;
            ctrl.stop_requested = true;
            self.cvar.notify_all();
            ctrl.join.take()
        };
        if let Some(j) = cap_join {
            let _ = j.join();
        }
        let in_worker = self.input_worker.lock().take();
        if let Some(mut w) = in_worker {
            let _ = w.tx.send(InputJob::Shutdown);
            if let Some(j) = w.join.take() {
                let _ = j.join();
            }
        }
    }
}

#[async_trait]
impl ViewTarget for DesktopViewTarget {
    fn info(&self) -> TargetInfo {
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
        let st = self.core.status.lock();
        TargetInfo {
            target_id: self.core.target_id,
            kind: TargetKind::Desktop,
            host_id: self.core.options.host_id.clone(),
            bot_id: self.core.options.bot_id,
            run_id: self.core.options.run_id,
            generation: st.generation,
            capabilities: LiveCapabilities {
                view: st.view_available && st.unavailable_reason.is_none(),
                input: st.input_available,
                codecs: vec![Codec::Jpeg],
                reason: st
                    .unavailable_reason
                    .clone()
                    .or_else(|| st.input_reason.clone()),
            },
            unavailable_reason: st.unavailable_reason.clone(),
        }
    }

    fn frame_source(&self) -> Arc<dyn FrameSource> {
        Arc::new(DesktopFrameSource {
            core: Arc::clone(&self.core),
        })
    }

    fn input_sink(&self) -> Option<Arc<dyn InputSink>> {
        self.core.input_factory.as_ref().map(|_| {
            Arc::new(DesktopInputSink {
                core: Arc::clone(&self.core),
            }) as Arc<dyn InputSink>
        })
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

#[async_trait]
impl FrameSource for DesktopFrameSource {
    async fn subscribe(&self, profile: EffectiveProfile) -> Result<FrameSubscription, ViewError> {
        validate_profile(&profile)?;
        let rx = self.core.frames_tx.subscribe();
        let (init_rx, ticket) = {
            let old_join = {
                let mut ctrl = self.core.ctrl.lock();
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
                if let Some(j) = prev {
                    let _ = j.join();
                }
                let (itx, irx) = oneshot::channel();
                let worker_core = Arc::clone(&self.core);
                let spawned = std::thread::Builder::new()
                    .name("live-view-cap".into())
                    .spawn(move || run_capture_worker(&worker_core, Some(itx)));
                if let Ok(handle) = spawned {
                    self.core.ctrl.lock().join = Some(handle);
                    Some(irx)
                } else {
                    self.core.ctrl.lock().running = false;
                    return Err(ViewError::Unavailable);
                }
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
        let mut ctrl = self.core.ctrl.lock();
        if ctrl.subscribers == 0 || ctrl.paused {
            return Ok(());
        }
        ctrl.pending_refresh = Some(
            ctrl.pending_refresh
                .map_or(action_seq, |prev| prev.max(action_seq)),
        );
        self.core.cvar.notify_all();
        Ok(())
    }

    async fn set_profile(&self, profile: EffectiveProfile) -> Result<(), ViewError> {
        validate_profile(&profile)?;
        let mut ctrl = self.core.ctrl.lock();
        let mut clamped = profile;
        clamped.max_fps = clamped.max_fps.min(LIVE_MAX_FPS_CAP);
        ctrl.profile = clamped;
        ctrl.profile_dirty = true;
        if ctrl.subscribers > 0 {
            ctrl.paused = false;
            ctrl.stop_requested = false;
            ctrl.pending_refresh.get_or_insert(0);
            self.core.cvar.notify_all();
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
    }
}

fn finish_capture_worker(core: &Arc<TargetCore>) {
    let _ = core.frames_tx.send(None);
    let waiters = {
        let mut ctrl = core.ctrl.lock();
        ctrl.running = false;
        std::mem::take(&mut ctrl.exit_waiters)
    };
    for w in waiters {
        let _ = w.send(());
    }
}

fn periodic_interval(profile: &EffectiveProfile) -> Duration {
    if profile.profile == Profile::Watch {
        WATCH_IDLE_INTERVAL
    } else {
        let fps = profile.max_fps.clamp(1, LIVE_MAX_FPS_CAP);
        Duration::from_secs_f64(1.0 / f64::from(fps))
    }
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
        let interval = periodic_interval(&ctrl.profile);
        let due_periodic = cache.last_capture.elapsed() >= interval;
        if ctrl.pending_refresh.is_some()
            || ctrl.profile_dirty
            || cache.cached_payload.is_none()
            || due_periodic
        {
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
            codec: Codec::Jpeg,
            width: cache.cached_dims.0,
            height: cache.cached_dims.1,
            source_rect: geom.rect.to_source_rect(),
            geometry_revision,
            repeated: !forced,
        },
        payload,
    });
    if frame.validate().is_ok() {
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
    let Ok((out_w, out_h)) = encode_jpeg_within_budget(
        &bufs.bgra,
        geom.rect.width,
        geom.rect.height,
        profile,
        &mut bufs.scaled,
        &mut bufs.jpeg,
    ) else {
        core.stats
            .frames_skipped_budget
            .fetch_add(1, Ordering::Relaxed);
        return Err(ViewError::LimitExceeded);
    };

    let cost =
        f64::from(u32::try_from(bufs.jpeg.len().saturating_add(MAX_HEADER)).unwrap_or(u32::MAX));
    if !forced && cache.cached_payload.is_some() && cache.token_bytes < cost {
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
            codec: Codec::Jpeg,
            width: out_w,
            height: out_h,
            source_rect: geom.rect.to_source_rect(),
            geometry_revision,
            repeated: false,
        },
        payload: Arc::clone(&payload),
    });
    if frame.validate().is_ok() {
        let _ = core.frames_tx.send(Some(frame));
        cache.last_hash = Some(hash);
        cache.cached_payload = Some(payload);
        cache.cached_dims = (out_w, out_h);
        cache.cached_captured_at_ms = captured_at_ms;
        cache.last_publish = Instant::now();
        core.stats.frames_encoded.fetch_add(1, Ordering::Relaxed);
    }
    Ok(())
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
    let can_reuse = !profile_dirty
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

    Some(encode_and_publish_frame(
        core,
        cache,
        bufs,
        profile,
        &raw_info.geometry,
        hash,
        refresh_seq,
        generation,
        geometry_revision,
    ))
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
                finish_capture_worker(core);
                return;
            }
        };

    let initial_burst = core.ctrl.lock().profile.burst_bytes;
    let mut cache = CaptureSessionCache::new(initial_burst);
    let mut bufs = WorkerBuffers {
        raw: Vec::new(),
        bgra: Vec::new(),
        scaled: Vec::new(),
        jpeg: Vec::new(),
    };
    let mut saw_error = false;

    while let Some((profile, refresh_seq, profile_dirty)) = wait_for_capture_wake(core, &cache) {
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
    finish_capture_worker(core);
}

fn sleep_interruptible(core: &Arc<TargetCore>, duration: Duration) {
    let mut ctrl = core.ctrl.lock();
    if ctrl.shutdown || ctrl.stop_requested || ctrl.paused || ctrl.subscribers == 0 {
        return;
    }
    let _ = core.cvar.wait_for(&mut ctrl, duration);
}

// ── Cancel-safe InputSink coordinator ─────────────────────────────────────────

struct DesktopInputSink {
    core: Arc<TargetCore>,
}

struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

fn run_input_worker_loop(
    mut drv: Box<dyn InputDriver>,
    rx: &std::sync::mpsc::Receiver<InputJob>,
    core: &Arc<TargetCore>,
) {
    let stats = &core.stats;
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
                    let stale_geom =
                        event.positional() && st.geometry_revision != job_geometry_revision;
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
                let res = drv.inject(&event, source_rect, virtual_rect, &cancelled);
                match &res {
                    Ok(()) => {
                        stats.input_events.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(ViewError::PartialInjection) => {
                        stats.input_partial_failures.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(_) => {}
                }
                let _ = reply.send(res);
            }
            InputJob::ReleaseAll { reply } => {
                stats.input_resets.fetch_add(1, Ordering::Relaxed);
                let res = drv.release_all();
                let _ = reply.send(res);
            }
            InputJob::Shutdown => {
                let _ = drv.release_all();
                break;
            }
        }
    }
}

fn ensure_input_worker(
    core: &Arc<TargetCore>,
) -> Result<std::sync::mpsc::Sender<InputJob>, ViewError> {
    let mut guard = core.input_worker.lock();
    if let Some(worker) = guard.as_ref() {
        return Ok(worker.tx.clone());
    }
    let Some(factory) = core.input_factory.as_ref().map(Arc::clone) else {
        return Err(ViewError::Unsupported);
    };
    let (tx, rx) = std::sync::mpsc::channel::<InputJob>();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), ViewError>>();
    let core_epoch = Arc::clone(core);
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
            run_input_worker_loop(drv, &rx, &core_epoch);
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
            (
                st.generation,
                st.geometry_revision,
                st.latest_geometry.rect.to_source_rect(),
                st.latest_geometry.virtual_rect,
            )
        };
        if event.positional() {
            validate_positional_geometry(&source_rect, virtual_rect)?;
        }
        let tx = ensure_input_worker(&self.core)?;
        let cancelled = Arc::new(AtomicBool::new(false));
        let _guard = CancelOnDrop(Arc::clone(&cancelled));
        let epoch = self.core.input_epoch.load(Ordering::Acquire);
        let (reply_tx, reply_rx) = oneshot::channel();
        tx.send(InputJob::Inject {
            epoch,
            generation,
            geometry_revision,
            cancelled,
            event: event.clone(),
            source_rect,
            virtual_rect,
            reply: reply_tx,
        })
        .map_err(|_| ViewError::Unavailable)?;
        reply_rx.await.map_err(|_| ViewError::Unavailable)?
    }

    async fn release_pressed_inputs(&self) -> Result<(), ViewError> {
        self.core.input_epoch.fetch_add(1, Ordering::SeqCst);
        let tx_opt = {
            let guard = self.core.input_worker.lock();
            guard.as_ref().map(|w| w.tx.clone())
        };
        let Some(tx) = tx_opt else {
            // No input worker was ever spawned; nothing was pressed by this sink.
            return Ok(());
        };
        let (reply_tx, reply_rx) = oneshot::channel();
        tx.send(InputJob::ReleaseAll { reply: reply_tx })
            .map_err(|_| ViewError::Unavailable)?;
        reply_rx.await.map_err(|_| ViewError::Unavailable)?
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
            let mut ctrl = self.core.ctrl.lock();
            if ctrl.profile.profile == Profile::Live {
                ctrl.profile = EffectiveProfile::watch();
                ctrl.profile_dirty = true;
                self.core.cvar.notify_all();
            }
            Ok(())
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
        let (ow, oh) =
            encode_jpeg_within_budget(&bgra, width, height, &profile, &mut scratch, &mut out)
                .unwrap();
        assert!(ow <= 160 && oh <= 120);
        assert_ne!(out.len(), 0);
        assert!(out.len() + MAX_HEADER <= profile.burst_bytes as usize);
        assert_eq!(&out[..2], &[0xFF, 0xD8]); // JPEG SOI

        profile.burst_bytes = u32::try_from(MAX_HEADER).unwrap();
        assert_eq!(
            encode_jpeg_within_budget(&bgra, width, height, &profile, &mut scratch, &mut out),
            Err(ViewError::LimitExceeded)
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
        tokio::time::sleep(Duration::from_millis(650)).await;
        let f_resized = sub1.frames.borrow_and_update().clone().unwrap();
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

    struct SlowMockInput {
        log: Arc<Mutex<Vec<&'static str>>>,
        partial_next: Arc<AtomicBool>,
    }

    impl InputDriver for SlowMockInput {
        fn inject(
            &mut self,
            event: &InputEvent,
            _source_rect: SourceRect,
            _virtual_rect: PixelRect,
            cancelled: &AtomicBool,
        ) -> Result<(), ViewError> {
            match event {
                InputEvent::Text { text } if text == "slow" => {
                    std::thread::sleep(Duration::from_millis(60));
                    if cancelled.load(Ordering::Acquire) {
                        return Err(ViewError::Unavailable);
                    }
                    self.log.lock().push("slow_delivered");
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
            Ok(())
        }
    }

    #[tokio::test]
    async fn input_cancel_and_reset_barrier_blocks_late_events_and_is_idempotent() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let partial_next = Arc::new(AtomicBool::new(false));

        let target = DesktopViewTarget::new(DesktopViewTargetConfig {
            options: LiveViewOptions::new("host"),
            initial_geometry: test_geom(800, 600),
            capture_factory: Arc::new(|_| Err(CaptureFailure::Unavailable(Reason::Unavailable))),
            probe: Arc::new(|_| Ok(())),
            input_factory: Some({
                let log = Arc::clone(&log);
                let partial_next = Arc::clone(&partial_next);
                Arc::new(move || {
                    Ok(Box::new(SlowMockInput {
                        log: Arc::clone(&log),
                        partial_next: Arc::clone(&partial_next),
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

        // Start a slow inject, immediately drop/abort its future, then call reset barrier.
        let sink2 = Arc::clone(&sink);
        let fut = tokio::spawn(async move {
            sink2
                .inject(&InputEvent::Text {
                    text: "slow".into(),
                })
                .await
        });
        tokio::time::sleep(Duration::from_millis(10)).await;
        fut.abort();
        let _ = fut.await;
        sink.release_pressed_inputs().await.unwrap();
        tokio::time::sleep(Duration::from_millis(90)).await;
        assert!(!log.lock().contains(&"slow_delivered"));
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
    async fn takeover_gate_and_stale_geometry_revision_reject_positional_input() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let partial_next = Arc::new(AtomicBool::new(false));

        let target = DesktopViewTarget::new(DesktopViewTargetConfig {
            options: LiveViewOptions::new("host-geom"),
            initial_geometry: test_geom(800, 600),
            capture_factory: Arc::new(|_| Err(CaptureFailure::Unavailable(Reason::Unavailable))),
            probe: Arc::new(|_| Ok(())),
            input_factory: Some({
                let log = Arc::clone(&log);
                let partial_next = Arc::clone(&partial_next);
                Arc::new(move || {
                    Ok(Box::new(SlowMockInput {
                        log: Arc::clone(&log),
                        partial_next: Arc::clone(&partial_next),
                    }))
                })
            }),
            input_reason: None,
        });

        let sink = target.input_sink().unwrap();
        sink.set_takeover_active(true).await.unwrap();

        // Queue a slow event on the input worker, and while it is executing queue a positional
        // pointer event and bump `geometry_revision` before the worker reaches the pointer event.
        let sink_slow = Arc::clone(&sink);
        let slow_fut = tokio::spawn(async move {
            sink_slow
                .inject(&InputEvent::Text {
                    text: "slow".into(),
                })
                .await
        });
        tokio::time::sleep(Duration::from_millis(10)).await;

        let sink_ptr = Arc::clone(&sink);
        let ptr_fut = tokio::spawn(async move {
            sink_ptr
                .inject(&InputEvent::Pointer {
                    phase: ::live_view::PointerPhase::Down,
                    x: 0.5,
                    y: 0.5,
                    button: Some(::live_view::Button::Left),
                    buttons: 1,
                    modifiers: vec![],
                })
                .await
        });
        tokio::time::sleep(Duration::from_millis(5)).await;
        // Simulate monitor topology change mid-flight before the queued pointer job executes.
        let _ = update_status_ok(&target.core, test_geom(1024, 768));

        assert_eq!(slow_fut.await.unwrap(), Ok(()));
        assert_eq!(ptr_fut.await.unwrap(), Err(ViewError::Unavailable));
        assert!(!log.lock().contains(&"other"));
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

        // Wait for Live keepalive heartbeat on static desktop (>800 ms).
        let repeated = loop {
            sub.frames.changed().await.unwrap();
            let f = sub.frames.borrow_and_update().clone().unwrap();
            if f.metadata.repeated {
                break f;
            }
        };
        assert_eq!(repeated.metadata.frame_seq, first.metadata.frame_seq);
        assert_eq!(
            repeated.metadata.captured_at_ms,
            first.metadata.captured_at_ms
        );
        assert_eq!(repeated.metadata.cause, Cause::Idle);
        src.stop().await.unwrap();
    }
}
