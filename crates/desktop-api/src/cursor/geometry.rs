//! Pure coordinate mapping for the bot-cursor overlay window.
//!
//! Screen points are physical pixels in virtual-screen coordinates: on a multi-monitor desktop
//! the origin is the top-left of the primary monitor and a monitor to its left or above has
//! negative coordinates. The overlay window is a small square centred on the point, so its
//! top-left corner can be negative too.

/// The bounding box of every monitor, in virtual-screen physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtualScreen {
    /// Left edge (negative when a monitor sits left of the primary one).
    pub x: i32,
    /// Top edge.
    pub y: i32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl VirtualScreen {
    /// Whether a screen point is inside the virtual screen.
    #[must_use]
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x.is_finite()
            && y.is_finite()
            && x >= f64::from(self.x)
            && y >= f64::from(self.y)
            && x < f64::from(self.x) + f64::from(self.width)
            && y < f64::from(self.y) + f64::from(self.height)
    }
}

/// Round a finite screen coordinate to a pixel, saturating at the `i32` range.
#[must_use]
#[allow(clippy::cast_possible_truncation)]
pub fn to_px(v: f64) -> i32 {
    if v.is_nan() {
        0
    } else {
        v.round().clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
    }
}

/// Top-left corner of a `canvas`-pixel square window whose centre sits on the point `(x, y)`.
#[must_use]
pub fn window_origin(x: f64, y: f64, canvas: u32) -> (i32, i32) {
    let half = i32::try_from(canvas / 2).unwrap_or(0);
    (to_px(x).saturating_sub(half), to_px(y).saturating_sub(half))
}

/// A screen point in the local pixels of a window whose top-left corner is `origin`.
#[must_use]
pub fn to_local(x: f64, y: f64, origin: (i32, i32)) -> (f64, f64) {
    (x - f64::from(origin.0), y - f64::from(origin.1))
}

/// Scale factor for a monitor's DPI (96 dpi is 1.0), kept within a sane range so a bogus value
/// can neither hide the cursor nor make it fill the screen.
#[must_use]
pub fn dpi_scale(dpi: u32) -> f64 {
    if dpi == 0 {
        return 1.0;
    }
    (f64::from(dpi) / 96.0).clamp(1.0, 4.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_is_centred_on_the_point_even_at_negative_coordinates() {
        assert_eq!(window_origin(100.0, 50.0, 72), (64, 14));
        assert_eq!(window_origin(-1920.0 + 10.0, -5.4, 72), (-1946, -41));
        assert_eq!(window_origin(f64::NAN, 0.0, 72), (-36, -36));
        assert_eq!(window_origin(1e30, 0.0, 72).0, i32::MAX - 36);
    }

    #[test]
    fn a_point_maps_into_window_local_pixels_and_back() {
        let origin = window_origin(-300.0, 400.0, 72);
        let (lx, ly) = to_local(-300.0, 400.0, origin);
        assert!((lx - 36.0).abs() < 1e-9 && (ly - 36.0).abs() < 1e-9);
    }

    #[test]
    fn the_virtual_screen_may_start_left_of_and_above_the_primary_monitor() {
        let v = VirtualScreen {
            x: -1920,
            y: -200,
            width: 3840,
            height: 1280,
        };
        assert!(v.contains(-1920.0, -200.0));
        assert!(v.contains(1919.0, 1079.0));
        assert!(!v.contains(1920.0, 0.0));
        assert!(!v.contains(-1921.0, 0.0));
        assert!(!v.contains(f64::NAN, 0.0));
    }

    #[test]
    fn dpi_maps_to_a_bounded_scale() {
        assert!((dpi_scale(96) - 1.0).abs() < 1e-9);
        assert!((dpi_scale(144) - 1.5).abs() < 1e-9);
        assert!((dpi_scale(0) - 1.0).abs() < 1e-9);
        assert!((dpi_scale(10_000) - 4.0).abs() < 1e-9);
        assert!((dpi_scale(10) - 1.0).abs() < 1e-9);
    }
}
