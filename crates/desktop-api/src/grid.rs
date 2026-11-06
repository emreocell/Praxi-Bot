//! Coarse-grid helper for OCR / grid-based targeting.
//!
//! Implements the two-level zoom strategy from ADR-0003:
//! 1. Coarse grid over a region.
//! 2. `zoom(cell)` → finer grid over that cell.
//!
//! All coordinates use the `Normalized999` space (0–999 × 0–999 over the
//! target rectangle), making them independent of physical pixel density.

use crate::snapshot::Rect;

/// A coarse grid over a rectangular region.
///
/// Cell IDs are row-major: `cell_id = row * cols + col`.
#[derive(Debug, Clone)]
pub struct Grid {
    /// The region covered by this grid.
    pub region: Rect,
    /// Number of columns.
    pub cols: u32,
    /// Number of rows.
    pub rows: u32,
}

impl Grid {
    /// Create a new N×M grid over `region`.
    ///
    /// # Panics
    ///
    /// Panics if `cols` or `rows` is zero.
    #[must_use]
    pub fn new(region: Rect, cols: u32, rows: u32) -> Self {
        assert!(cols > 0, "cols must be > 0");
        assert!(rows > 0, "rows must be > 0");
        Self { region, cols, rows }
    }

    /// Return the bounding [`Rect`] of cell `id` in physical pixel coordinates.
    ///
    /// Returns `None` if `id` is out of range.
    #[must_use]
    pub fn cell_rect(&self, id: u32) -> Option<Rect> {
        if id >= self.cols * self.rows {
            return None;
        }
        let col = id % self.cols;
        let row = id / self.cols;
        let cell_w = self.region.width / f64::from(self.cols);
        let cell_h = self.region.height / f64::from(self.rows);
        Some(Rect {
            x: self.region.x + f64::from(col) * cell_w,
            y: self.region.y + f64::from(row) * cell_h,
            width: cell_w,
            height: cell_h,
        })
    }

    /// Create a finer grid over a single cell (zoom step 2).
    #[must_use]
    pub fn zoom(&self, cell_id: u32, cols: u32, rows: u32) -> Option<Grid> {
        let rect = self.cell_rect(cell_id)?;
        Some(Grid::new(rect, cols, rows))
    }

    /// Total number of cells.
    #[must_use]
    pub fn cell_count(&self) -> u32 {
        self.cols * self.rows
    }

    /// Convert a physical-pixel point within [`Self::region`] to a normalised
    /// 0–999 coordinate relative to `region`.
    ///
    /// Returns `None` if `(px, py)` is outside the region.
    #[must_use]
    pub fn to_normalized999(region: &Rect, px: f64, py: f64) -> Option<(u32, u32)> {
        if px < region.x
            || py < region.y
            || px > region.x + region.width
            || py > region.y + region.height
        {
            return None;
        }
        // Safety: values are bounded to 0.0..=999.0 by the range check above;
        // round() produces a whole number, and min(999) keeps it in u32 range.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let nx = ((px - region.x) / region.width * 999.0).round() as u32;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let ny = ((py - region.y) / region.height * 999.0).round() as u32;
        Some((nx.min(999), ny.min(999)))
    }

    /// Convert a normalised 0–999 coordinate back to physical pixels within `region`.
    #[must_use]
    pub fn from_normalized999(region: &Rect, nx: u32, ny: u32) -> (f64, f64) {
        let px = region.x + f64::from(nx.min(999)) / 999.0 * region.width;
        let py = region.y + f64::from(ny.min(999)) / 999.0 * region.height;
        (px, py)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect {
            x,
            y,
            width: w,
            height: h,
        }
    }

    #[test]
    fn cell_count_matches_dimensions() {
        let g = Grid::new(rect(0.0, 0.0, 1000.0, 800.0), 5, 4);
        assert_eq!(g.cell_count(), 20);
    }

    #[test]
    fn cell_rect_top_left_cell() {
        let g = Grid::new(rect(0.0, 0.0, 100.0, 80.0), 4, 4);
        let r = g.cell_rect(0).unwrap();
        assert!((r.x - 0.0).abs() < 1e-9);
        assert!((r.y - 0.0).abs() < 1e-9);
        assert!((r.width - 25.0).abs() < 1e-9);
        assert!((r.height - 20.0).abs() < 1e-9);
    }

    #[test]
    fn cell_rect_bottom_right_cell() {
        let g = Grid::new(rect(0.0, 0.0, 100.0, 80.0), 4, 4);
        let r = g.cell_rect(15).unwrap(); // row 3, col 3
        assert!((r.x - 75.0).abs() < 1e-9);
        assert!((r.y - 60.0).abs() < 1e-9);
    }

    #[test]
    fn cell_rect_out_of_range_returns_none() {
        let g = Grid::new(rect(0.0, 0.0, 100.0, 80.0), 4, 4);
        assert!(g.cell_rect(16).is_none());
    }

    #[test]
    fn zoom_produces_finer_grid_over_cell() {
        let g = Grid::new(rect(0.0, 0.0, 100.0, 80.0), 4, 4);
        let zoomed = g.zoom(0, 3, 3).unwrap();
        assert_eq!(zoomed.cell_count(), 9);
        // Should cover the same area as cell 0.
        let cell_rect = g.cell_rect(0).unwrap();
        assert!((zoomed.region.x - cell_rect.x).abs() < 1e-9);
        assert!((zoomed.region.width - cell_rect.width).abs() < 1e-9);
    }

    #[test]
    fn normalized999_round_trip() {
        let r = rect(100.0, 200.0, 800.0, 600.0);
        let px = 500.0;
        let py = 500.0;
        let (nx, ny) = Grid::to_normalized999(&r, px, py).unwrap();
        let (px2, py2) = Grid::from_normalized999(&r, nx, ny);
        assert!((px - px2).abs() < 1.0, "round-trip x error too large");
        assert!((py - py2).abs() < 1.0, "round-trip y error too large");
    }

    #[test]
    fn normalized999_top_left_is_zero() {
        let r = rect(0.0, 0.0, 1000.0, 1000.0);
        assert_eq!(Grid::to_normalized999(&r, 0.0, 0.0), Some((0, 0)));
    }

    #[test]
    fn normalized999_bottom_right_is_999() {
        let r = rect(0.0, 0.0, 1000.0, 1000.0);
        assert_eq!(Grid::to_normalized999(&r, 1000.0, 1000.0), Some((999, 999)));
    }

    #[test]
    fn normalized999_outside_region_returns_none() {
        let r = rect(0.0, 0.0, 100.0, 100.0);
        assert!(Grid::to_normalized999(&r, -1.0, 50.0).is_none());
    }
}
