//! The bot's own pointer, shared by everything that draws it.
//!
//! Computer-use apps show a pointer of their own, distinct from the owner's, so the owner can
//! see where the bot is about to click. Three places draw it: a transparent window over the
//! real desktop, an overlay inside the managed browser's page, and the live viewer. They
//! agree on this module only:
//!
//! * the owner's switch ([`enabled`], default on), read by every drawing site;
//! * the last pointer position the bot reached, per kind of screen ([`publish_move`],
//!   [`publish_click`], [`clear`], [`latest`]), which the live view stamps on its frames
//!   ([`BotPointer`] in `FrameMetadata`);
//! * the mapping from a screen point to a point on a frame ([`normalize`]).
//!
//! Nothing here is secret and nothing here moves the real cursor: it is a position and a
//! click counter. Memory only.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};

use serde::{Deserialize, Serialize};

use crate::{SourceRect, TargetKind};

static ENABLED: AtomicBool = AtomicBool::new(true);

/// Whether the bot cursor is drawn at all (the owner's setting). On by default.
#[must_use]
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Set the owner's switch. Turning it off also forgets every published position, so the live
/// view stops drawing a cursor on the very next frame.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
    if !on {
        for kind in [TargetKind::Browser, TargetKind::Desktop] {
            clear(kind);
        }
    }
}

/// Where the bot's pointer is on a frame, as the viewer draws it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BotPointer {
    /// Fraction of the frame width from the left edge, `0.0..=1.0`.
    pub x: f64,
    /// Fraction of the frame height from the top edge, `0.0..=1.0`.
    pub y: f64,
    /// Counts the bot's clicks at this pointer; a viewer plays a ripple when it grows.
    pub click_seq: u64,
}

/// A position in the units of the screen it was made on (desktop: physical screen pixels;
/// browser: CSS pixels of the viewport) with the click counter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Published {
    /// Pixels from the left.
    pub x: f64,
    /// Pixels from the top.
    pub y: f64,
    /// Clicks so far at this pointer.
    pub click_seq: u64,
}

static LATEST: Mutex<[Option<Published>; 2]> = Mutex::new([None, None]);

fn slot(kind: TargetKind) -> usize {
    match kind {
        TargetKind::Browser => 0,
        TargetKind::Desktop => 1,
    }
}

fn with<R>(f: impl FnOnce(&mut [Option<Published>; 2]) -> R) -> R {
    f(&mut LATEST.lock().unwrap_or_else(PoisonError::into_inner))
}

/// The bot's pointer moved to `(x, y)`. Ignored while the owner's switch is off or the point is
/// not finite. Negative values are fine: a monitor left of or above the primary has them.
pub fn publish_move(kind: TargetKind, x: f64, y: f64) {
    if !enabled() || !x.is_finite() || !y.is_finite() {
        return;
    }
    with(|s| {
        let seq = s[slot(kind)].map_or(0, |p| p.click_seq);
        s[slot(kind)] = Some(Published {
            x,
            y,
            click_seq: seq,
        });
    });
}

/// The bot clicked at `(x, y)`: a move plus one more click for the ripple.
pub fn publish_click(kind: TargetKind, x: f64, y: f64) {
    if !enabled() || !x.is_finite() || !y.is_finite() {
        return;
    }
    with(|s| {
        let seq = s[slot(kind)].map_or(0, |p| p.click_seq);
        s[slot(kind)] = Some(Published {
            x,
            y,
            click_seq: seq.wrapping_add(1),
        });
    });
}

/// The bot stopped, or the owner took control: nothing to draw any more.
pub fn clear(kind: TargetKind) {
    with(|s| s[slot(kind)] = None);
}

/// The last position published for `kind`, if the bot still has a pointer there.
#[must_use]
pub fn latest(kind: TargetKind) -> Option<Published> {
    with(|s| s[slot(kind)])
}

/// A screen point as a fraction of the frame that shows `rect` (the source rectangle of a
/// frame, in the same units as the point). `None` when the point is outside the frame, not
/// finite, or the rectangle is empty: a cursor off the picture is not drawn at the edge.
#[must_use]
pub fn normalize(x: f64, y: f64, rect: &SourceRect) -> Option<(f64, f64)> {
    if !x.is_finite() || !y.is_finite() || rect.width == 0 || rect.height == 0 {
        return None;
    }
    let nx = (x - rect.x) / f64::from(rect.width);
    let ny = (y - rect.y) / f64::from(rect.height);
    ((0.0..=1.0).contains(&nx) && (0.0..=1.0).contains(&ny)).then_some((nx, ny))
}

/// What a frame of `kind` showing `rect` should say about the bot's pointer: the published
/// position mapped onto the frame, or `None` (switch off, no pointer, or off the picture).
#[must_use]
pub fn for_frame(kind: TargetKind, rect: &SourceRect) -> Option<BotPointer> {
    if !enabled() {
        return None;
    }
    let p = latest(kind)?;
    let (x, y) = normalize(p.x, p.y, rect)?;
    Some(BotPointer {
        x,
        y,
        click_seq: p.click_seq,
    })
}

#[cfg(test)]
pub(crate) static TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Units;

    fn rect(x: f64, y: f64, w: u32, h: u32) -> SourceRect {
        SourceRect {
            x,
            y,
            width: w,
            height: h,
            units: Units::PhysicalPx,
        }
    }

    #[test]
    fn a_point_maps_to_a_fraction_of_the_frame_and_off_picture_points_do_not() {
        let r = rect(1920.0, 0.0, 1920, 1080);
        assert_eq!(normalize(2880.0, 540.0, &r), Some((0.5, 0.5)));
        assert_eq!(normalize(1920.0, 0.0, &r), Some((0.0, 0.0)));
        assert_eq!(normalize(3840.0, 1080.0, &r), Some((1.0, 1.0)));
        assert_eq!(normalize(100.0, 100.0, &r), None, "left of the frame");
        assert_eq!(normalize(2000.0, 1081.0, &r), None, "below the frame");
        assert_eq!(normalize(f64::NAN, 1.0, &r), None);
        assert_eq!(normalize(1.0, 1.0, &rect(0.0, 0.0, 0, 10)), None);
    }

    #[test]
    fn moves_and_clicks_are_published_and_cleared_per_kind() {
        let _g = TEST_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        set_enabled(true);
        clear(TargetKind::Desktop);
        clear(TargetKind::Browser);
        publish_move(TargetKind::Desktop, 10.0, 20.0);
        publish_click(TargetKind::Desktop, 11.0, 21.0);
        publish_move(TargetKind::Desktop, 30.0, 40.0);
        let p = latest(TargetKind::Desktop).unwrap();
        assert_eq!((p.x, p.y, p.click_seq), (30.0, 40.0, 1));
        assert!(latest(TargetKind::Browser).is_none());
        publish_move(TargetKind::Desktop, f64::NAN, 1.0);
        publish_move(TargetKind::Desktop, f64::INFINITY, 1.0);
        assert_eq!(latest(TargetKind::Desktop).unwrap().x, 30.0);
        clear(TargetKind::Desktop);
        assert!(latest(TargetKind::Desktop).is_none());
    }

    #[test]
    fn the_owner_switch_hides_the_cursor_and_forgets_it() {
        let _g = TEST_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        set_enabled(true);
        publish_move(TargetKind::Desktop, 5.0, 5.0);
        let r = rect(0.0, 0.0, 100, 100);
        assert!(for_frame(TargetKind::Desktop, &r).is_some());
        set_enabled(false);
        assert!(for_frame(TargetKind::Desktop, &r).is_none());
        publish_move(TargetKind::Desktop, 6.0, 6.0);
        assert!(latest(TargetKind::Desktop).is_none(), "ignored while off");
        set_enabled(true);
        assert!(latest(TargetKind::Desktop).is_none(), "not resurrected");
    }
}
