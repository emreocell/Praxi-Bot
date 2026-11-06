//! Two-stage grid overlay rendering (`N x M` coarse grid + cell zoom with finer grid)
//! and `0..=999` normalized coordinate conversion helpers.

use std::collections::BTreeMap;

use image::imageops::{self, FilterType};
use image::{Rgb, RgbImage};

use crate::canvas::{draw_badge, draw_line_h, draw_line_v};
use crate::{BoundingBox, VisionError};

/// Maximum valid normalized coordinate value (`0..=999`).
pub const NORMALIZED_MAX: u32 = 999;
/// Scale divisor for normalized coordinates (`1000`).
pub const NORMALIZED_SCALE: u32 = 1000;

/// Naming style for grid cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GridLabelStyle {
    /// Row letters (`A, B, C, ...`) followed by 1-based column numbers (`1, 2, ...`), e.g. `A1`, `B3`.
    #[default]
    AlphaNumeric,
    /// 1-based row-major cell numbers (`1, 2, ..., cols * rows`).
    Numeric,
}

/// Configuration for drawing a grid overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridConfig {
    /// Number of columns (`N >= 1`).
    pub cols: u32,
    /// Number of rows (`M >= 1`).
    pub rows: u32,
    /// Cell ID style (`A1...` or `1...`).
    pub label_style: GridLabelStyle,
    /// Alpha blend strength for grid lines (`0..=255`, default `160`).
    pub line_alpha: u8,
    /// Font scale for cell labels (default `1`).
    pub font_scale: u32,
}

impl Default for GridConfig {
    fn default() -> Self {
        Self {
            cols: 8,
            rows: 6,
            label_style: GridLabelStyle::AlphaNumeric,
            line_alpha: 160,
            font_scale: 1,
        }
    }
}

impl GridConfig {
    /// Creates a new [`GridConfig`] with `cols` columns and `rows` rows using [`GridLabelStyle::AlphaNumeric`].
    #[must_use]
    pub const fn new(cols: u32, rows: u32) -> Self {
        Self {
            cols,
            rows,
            label_style: GridLabelStyle::AlphaNumeric,
            line_alpha: 160,
            font_scale: 1,
        }
    }

    /// Sets the [`GridLabelStyle`].
    #[must_use]
    pub const fn with_style(mut self, label_style: GridLabelStyle) -> Self {
        self.label_style = label_style;
        self
    }
}

/// Result of drawing a grid overlay on an image.
#[derive(Debug, Clone)]
pub struct GridOverlay {
    /// Copy of the input image with grid lines and cell ID badges drawn.
    pub image: RgbImage,
    /// Map from cell ID (`"A1"`, `"B2"`, or `"1"`, `"2"`, ...) to its pixel [`BoundingBox`].
    pub cells: BTreeMap<String, BoundingBox>,
    /// Number of columns (`N`).
    pub cols: u32,
    /// Number of rows (`M`).
    pub rows: u32,
    /// Label style used.
    pub label_style: GridLabelStyle,
}

/// Result of zooming into a single coarse grid cell and drawing a finer second-stage grid.
#[derive(Debug, Clone)]
pub struct ZoomedCell {
    /// Cropped, upscaled image with the finer grid drawn on top.
    pub image: RgbImage,
    /// Canonical ID of the coarse cell that was zoomed.
    pub cell_id: String,
    /// Bounding box of the zoomed cell in the original (unzoomed) image.
    pub crop_bbox: BoundingBox,
    /// Upscale factor applied to the cropped cell (`>= 1`).
    pub upscale_factor: u32,
    /// The fine grid overlay rendered onto `image`.
    pub fine_grid: GridOverlay,
}

impl ZoomedCell {
    /// Maps a pixel coordinate `(zoomed_x, zoomed_y)` inside the zoomed image back to
    /// pixel coordinates in the original uncropped screenshot.
    #[must_use]
    pub fn zoomed_point_to_original(&self, zoomed_x: u32, zoomed_y: u32) -> (u32, u32) {
        let factor = self.upscale_factor.max(1);
        let local_x = (zoomed_x / factor).min(self.crop_bbox.width.saturating_sub(1));
        let local_y = (zoomed_y / factor).min(self.crop_bbox.height.saturating_sub(1));
        (self.crop_bbox.x + local_x, self.crop_bbox.y + local_y)
    }

    /// Maps the center of a fine-grid cell (`fine_cell_id`, e.g. `"B2"` or `"5"`) back to
    /// pixel coordinates in the original uncropped screenshot.
    #[must_use]
    pub fn fine_cell_to_original_point(&self, fine_cell_id: &str) -> Option<(u32, u32)> {
        let (col, row) = parse_cell_id(fine_cell_id, self.fine_grid.cols, self.fine_grid.rows)?;
        let (zw, zh) = self.image.dimensions();
        let (cx, cy) = cell_center(zw, zh, self.fine_grid.cols, self.fine_grid.rows, col, row)?;
        Some(self.zoomed_point_to_original(cx, cy))
    }

    /// Maps `0..=999` normalized coordinates within the zoomed image back to pixel
    /// coordinates in the original uncropped screenshot.
    #[must_use]
    pub fn normalized_to_original(&self, norm_x: u32, norm_y: u32) -> (u32, u32) {
        let (zw, zh) = self.image.dimensions();
        let (zx, zy) = normalized_to_pixel(norm_x, norm_y, zw, zh);
        self.zoomed_point_to_original(zx, zy)
    }
}

/// Converts a 0-based row index into an uppercase spreadsheet-style row prefix (`0 -> "A"`, `25 -> "Z"`, `26 -> "AA"`).
#[must_use]
pub fn row_index_to_letters(mut row: u32) -> String {
    let mut chars = Vec::new();
    loop {
        let rem = (row % 26) as u8;
        chars.push(char::from(b'A' + rem));
        if row < 26 {
            break;
        }
        row = row / 26 - 1;
    }
    chars.into_iter().rev().collect()
}

/// Parses an uppercase/lowercase letter prefix (`"A" -> 0`, `"Z" -> 25`, `"AA" -> 26`).
#[must_use]
pub fn letters_to_row_index(letters: &str) -> Option<u32> {
    if letters.is_empty() {
        return None;
    }
    let mut acc: u32 = 0;
    for ch in letters.chars() {
        let upper = ch.to_ascii_uppercase();
        if !upper.is_ascii_uppercase() {
            return None;
        }
        let digit = u32::from(upper as u8 - b'A') + 1;
        acc = acc.checked_mul(26)?.checked_add(digit)?;
    }
    Some(acc - 1)
}

/// Formats the cell identifier for 0-based `(col, row)` in a grid with `cols` columns.
#[must_use]
pub fn format_cell_id(col: u32, row: u32, cols: u32, style: GridLabelStyle) -> String {
    match style {
        GridLabelStyle::AlphaNumeric => {
            format!("{}{}", row_index_to_letters(row), col + 1)
        }
        GridLabelStyle::Numeric => {
            let index = row
                .saturating_mul(cols)
                .saturating_add(col)
                .saturating_add(1);
            index.to_string()
        }
    }
}

/// Parses a cell identifier (`"A1"`, `"b3"`, or 1-based numeric `"1"`, `"12"`) into
/// 0-based `(col, row)` indices, validated against `(cols, rows)`.
#[must_use]
pub fn parse_cell_id(cell_id: &str, cols: u32, rows: u32) -> Option<(u32, u32)> {
    if cols == 0 || rows == 0 {
        return None;
    }
    let trimmed = cell_id.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Pure numeric cell ID (1-based row-major index)
    if trimmed.chars().all(|c| c.is_ascii_digit()) {
        let idx_1based: u32 = trimmed.parse().ok()?;
        if idx_1based == 0 {
            return None;
        }
        let idx0 = idx_1based - 1;
        let row = idx0 / cols;
        let col = idx0 % cols;
        if col < cols && row < rows {
            return Some((col, row));
        }
        return None;
    }

    // AlphaNumeric cell ID: leading ASCII letters (row) + trailing ASCII digits (1-based col)
    let split_pos = trimmed.find(|c: char| c.is_ascii_digit())?;
    let (letters, digits) = trimmed.split_at(split_pos);
    if letters.is_empty() || digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let row = letters_to_row_index(letters)?;
    let col_1based: u32 = digits.parse().ok()?;
    if col_1based == 0 {
        return None;
    }
    let col = col_1based - 1;
    if col < cols && row < rows {
        Some((col, row))
    } else {
        None
    }
}

/// Computes the exact pixel [`BoundingBox`] of cell `(col, row)` in an image of size
/// `(image_width, image_height)` divided into `cols x rows`.
///
/// Adjacent cells tile `[0..image_width, 0..image_height]` without gaps or overlaps.
#[must_use]
#[allow(clippy::cast_possible_truncation)]
pub const fn cell_bounds(
    image_width: u32,
    image_height: u32,
    cols: u32,
    rows: u32,
    col: u32,
    row: u32,
) -> Option<BoundingBox> {
    if cols == 0 || rows == 0 || col >= cols || row >= rows || image_width == 0 || image_height == 0
    {
        return None;
    }
    let x0 = ((image_width as u64) * (col as u64) / (cols as u64)) as u32;
    let x1 = ((image_width as u64) * ((col + 1) as u64) / (cols as u64)) as u32;
    let y0 = ((image_height as u64) * (row as u64) / (rows as u64)) as u32;
    let y1 = ((image_height as u64) * ((row + 1) as u64) / (rows as u64)) as u32;

    Some(BoundingBox::new(
        x0,
        y0,
        x1.saturating_sub(x0),
        y1.saturating_sub(y0),
    ))
}

/// Computes the pixel [`BoundingBox`] of the cell identified by `cell_id` (e.g. `"A1"` or `"1"`).
#[must_use]
pub fn cell_bounds_by_id(
    image_width: u32,
    image_height: u32,
    cols: u32,
    rows: u32,
    cell_id: &str,
) -> Option<BoundingBox> {
    let (col, row) = parse_cell_id(cell_id, cols, rows)?;
    cell_bounds(image_width, image_height, cols, rows, col, row)
}

/// Computes the center pixel `(x, y)` of cell `(col, row)`.
#[must_use]
pub const fn cell_center(
    image_width: u32,
    image_height: u32,
    cols: u32,
    rows: u32,
    col: u32,
    row: u32,
) -> Option<(u32, u32)> {
    match cell_bounds(image_width, image_height, cols, rows, col, row) {
        Some(b) => Some(b.center()),
        None => None,
    }
}

/// Returns the 0-based `(col, row)` grid cell containing pixel `(x, y)`.
#[must_use]
#[allow(clippy::cast_possible_truncation)]
pub const fn cell_at_point(
    image_width: u32,
    image_height: u32,
    cols: u32,
    rows: u32,
    x: u32,
    y: u32,
) -> Option<(u32, u32)> {
    if image_width == 0 || image_height == 0 || cols == 0 || rows == 0 {
        return None;
    }
    let clamped_x = if x >= image_width { image_width - 1 } else { x };
    let clamped_y = if y >= image_height {
        image_height - 1
    } else {
        y
    };

    // Since x0(col) = floor(W * col / cols), the cell containing clamped_x is
    // floor(((clamped_x + 1) * cols - 1) / W).
    let col = (((clamped_x as u64 + 1) * (cols as u64) - 1) / (image_width as u64)) as u32;
    let row = (((clamped_y as u64 + 1) * (rows as u64) - 1) / (image_height as u64)) as u32;
    Some((
        if col >= cols { cols - 1 } else { col },
        if row >= rows { rows - 1 } else { row },
    ))
}

/// Converts normalized coordinates `(norm_x, norm_y)` in `0..=999` to pixel coordinates
/// in `(0..width, 0..height)`.
///
/// Follows the `0..=999` CUA convention: values are clamped to `0..=999`, divided by `1000`,
/// scaled to `(width, height)`, and clamped inside the image dimensions.
#[must_use]
#[allow(clippy::cast_possible_truncation)]
pub const fn normalized_to_pixel(norm_x: u32, norm_y: u32, width: u32, height: u32) -> (u32, u32) {
    if width == 0 || height == 0 {
        return (0, 0);
    }
    let nx = if norm_x > NORMALIZED_MAX {
        NORMALIZED_MAX
    } else {
        norm_x
    };
    let ny = if norm_y > NORMALIZED_MAX {
        NORMALIZED_MAX
    } else {
        norm_y
    };

    let px = ((nx as u64) * (width as u64) / (NORMALIZED_SCALE as u64)) as u32;
    let py = ((ny as u64) * (height as u64) / (NORMALIZED_SCALE as u64)) as u32;

    let out_x = if px >= width { width - 1 } else { px };
    let out_y = if py >= height { height - 1 } else { py };
    (out_x, out_y)
}

/// Converts pixel coordinates `(px_x, px_y)` in `(0..width, 0..height)` to normalized
/// coordinates in `0..=999`.
///
/// Paired with [`normalized_to_pixel`] using ceiling division so that:
/// - For any dimension `>= 1000`, `pixel_to_normalized(normalized_to_pixel(n)) == n` for all `n in 0..=999`.
/// - For any dimension `<= 1000`, `normalized_to_pixel(pixel_to_normalized(p)) == p` for all `p in 0..dim`.
#[must_use]
#[allow(clippy::cast_possible_truncation)]
pub const fn pixel_to_normalized(px_x: u32, px_y: u32, width: u32, height: u32) -> (u32, u32) {
    if width == 0 || height == 0 {
        return (0, 0);
    }
    let cx = if px_x >= width { width - 1 } else { px_x };
    let cy = if px_y >= height { height - 1 } else { px_y };

    let nx = ((cx as u64) * (NORMALIZED_SCALE as u64)).div_ceil(width as u64) as u32;
    let ny = ((cy as u64) * (NORMALIZED_SCALE as u64)).div_ceil(height as u64) as u32;

    (
        if nx > NORMALIZED_MAX {
            NORMALIZED_MAX
        } else {
            nx
        },
        if ny > NORMALIZED_MAX {
            NORMALIZED_MAX
        } else {
            ny
        },
    )
}

/// Converts a [`BoundingBox`] in `0..=999` normalized coordinates to pixel coordinates.
#[must_use]
pub const fn normalized_bbox_to_pixel(
    norm_bbox: BoundingBox,
    width: u32,
    height: u32,
) -> BoundingBox {
    if width == 0 || height == 0 {
        return BoundingBox::new(0, 0, 0, 0);
    }
    let (x0, y0) = normalized_to_pixel(norm_bbox.x, norm_bbox.y, width, height);
    let (x1, y1) = normalized_to_pixel(norm_bbox.right(), norm_bbox.bottom(), width, height);
    BoundingBox::from_ltrb(x0, y0, x1, y1)
}

/// Converts a pixel [`BoundingBox`] into `0..=999` normalized coordinates.
#[must_use]
pub const fn pixel_bbox_to_normalized(
    px_bbox: BoundingBox,
    width: u32,
    height: u32,
) -> BoundingBox {
    if width == 0 || height == 0 {
        return BoundingBox::new(0, 0, 0, 0);
    }
    let (nx0, ny0) = pixel_to_normalized(px_bbox.x, px_bbox.y, width, height);
    let (nx1, ny1) = pixel_to_normalized(px_bbox.right(), px_bbox.bottom(), width, height);
    BoundingBox::from_ltrb(nx0, ny0, nx1, ny1)
}

/// Draws an `N x M` grid overlay with cell ID badges onto a copy of `img`.
#[must_use]
pub fn draw_grid(img: &RgbImage, config: &GridConfig) -> GridOverlay {
    let cols = config.cols.max(1);
    let rows = config.rows.max(1);
    let mut annotated = img.clone();
    let (img_w, img_h) = annotated.dimensions();
    let mut cells = BTreeMap::new();

    if img_w == 0 || img_h == 0 {
        return GridOverlay {
            image: annotated,
            cells,
            cols,
            rows,
            label_style: config.label_style,
        };
    }

    let light_line = Rgb([230, 230, 230]);
    let dark_line = Rgb([35, 35, 35]);

    // Draw vertical grid lines (dual light+dark 1px lines for visibility on any background)
    for col in 1..cols {
        if let Some(bounds) = cell_bounds(img_w, img_h, cols, rows, col, 0) {
            draw_line_v(
                &mut annotated,
                bounds.x,
                0,
                img_h,
                light_line,
                config.line_alpha,
            );
            if bounds.x + 1 < img_w {
                draw_line_v(
                    &mut annotated,
                    bounds.x + 1,
                    0,
                    img_h,
                    dark_line,
                    config.line_alpha / 2,
                );
            }
        }
    }

    // Draw horizontal grid lines
    for row in 1..rows {
        if let Some(bounds) = cell_bounds(img_w, img_h, cols, rows, 0, row) {
            draw_line_h(
                &mut annotated,
                0,
                img_w,
                bounds.y,
                light_line,
                config.line_alpha,
            );
            if bounds.y + 1 < img_h {
                draw_line_h(
                    &mut annotated,
                    0,
                    img_w,
                    bounds.y + 1,
                    dark_line,
                    config.line_alpha / 2,
                );
            }
        }
    }

    // Draw cell ID badges and populate map
    let badge_bg = Rgb([25, 35, 55]);
    for row in 0..rows {
        for col in 0..cols {
            if let Some(bounds) = cell_bounds(img_w, img_h, cols, rows, col, row) {
                let id = format_cell_id(col, row, cols, config.label_style);
                draw_badge(
                    &mut annotated,
                    bounds.x + 2,
                    bounds.y + 2,
                    &id,
                    badge_bg,
                    config.font_scale.max(1),
                    2,
                );
                cells.insert(id, bounds);
            }
        }
    }

    GridOverlay {
        image: annotated,
        cells,
        cols,
        rows,
        label_style: config.label_style,
    }
}

/// Crops `cell_id` from `img` according to `coarse_config`, upscales it by `upscale_factor` (`>= 1`),
/// and draws `fine_config` on the upscaled cell.
///
/// # Errors
///
/// Returns [`VisionError::InvalidCell`] if `cell_id` is invalid or out of range for `coarse_config`.
pub fn zoom(
    img: &RgbImage,
    coarse_config: &GridConfig,
    cell_id: &str,
    fine_config: &GridConfig,
    upscale_factor: u32,
) -> Result<ZoomedCell, VisionError> {
    let cols = coarse_config.cols.max(1);
    let rows = coarse_config.rows.max(1);
    let (img_w, img_h) = img.dimensions();

    let (col, row) = parse_cell_id(cell_id, cols, rows)
        .ok_or_else(|| VisionError::InvalidCell(cell_id.to_string()))?;
    let crop_bbox = cell_bounds(img_w, img_h, cols, rows, col, row)
        .ok_or_else(|| VisionError::InvalidCell(cell_id.to_string()))?;

    if crop_bbox.width == 0 || crop_bbox.height == 0 {
        return Err(VisionError::InvalidCell(cell_id.to_string()));
    }

    let factor = upscale_factor.max(1);
    let cropped = imageops::crop_imm(
        img,
        crop_bbox.x,
        crop_bbox.y,
        crop_bbox.width,
        crop_bbox.height,
    )
    .to_image();

    let upscaled = if factor == 1 {
        cropped
    } else {
        imageops::resize(
            &cropped,
            crop_bbox.width.saturating_mul(factor),
            crop_bbox.height.saturating_mul(factor),
            FilterType::Lanczos3,
        )
    };

    let canonical_id = format_cell_id(col, row, cols, coarse_config.label_style);
    let fine_grid = draw_grid(&upscaled, fine_config);

    Ok(ZoomedCell {
        image: fine_grid.image.clone(),
        cell_id: canonical_id,
        crop_bbox,
        upscale_factor: factor,
        fine_grid,
    })
}

/// Convenience 2-stage zoom helper: crops `cell_id` from a `cols x rows` coarse grid,
/// upscales by `3x`, and renders a `4 x 4` fine alphanumeric grid.
///
/// # Errors
///
/// Returns [`VisionError::InvalidCell`] if `cell_id` is invalid.
pub fn zoom_cell(
    img: &RgbImage,
    cols: u32,
    rows: u32,
    cell_id: &str,
) -> Result<ZoomedCell, VisionError> {
    let coarse = GridConfig::new(cols, rows);
    let fine = GridConfig::new(4, 4);
    zoom(img, &coarse, cell_id, &fine, 3)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_cells_tile_image_without_gaps_or_overlaps() {
        let width = 1920;
        let height = 1080;
        let cols = 7;
        let rows = 5;

        let mut total_area = 0_u64;
        for r in 0..rows {
            let mut row_width = 0_u32;
            for c in 0..cols {
                let b = cell_bounds(width, height, cols, rows, c, r).unwrap();
                if c > 0 {
                    let prev = cell_bounds(width, height, cols, rows, c - 1, r).unwrap();
                    assert_eq!(b.x, prev.right(), "horizontal adjacency at col {c}");
                }
                if r > 0 {
                    let above = cell_bounds(width, height, cols, rows, c, r - 1).unwrap();
                    assert_eq!(b.y, above.bottom(), "vertical adjacency at row {r}");
                }
                row_width += b.width;
                total_area += b.area();

                // Center of cell must map back to the same (col, row)
                let (cx, cy) = b.center();
                assert_eq!(
                    cell_at_point(width, height, cols, rows, cx, cy),
                    Some((c, r))
                );
            }
            assert_eq!(row_width, width);
        }
        assert_eq!(total_area, u64::from(width) * u64::from(height));
    }

    #[test]
    fn cell_id_formatting_and_parsing_round_trip() {
        let cols = 8;
        let rows = 6;
        for r in 0..rows {
            for c in 0..cols {
                let alpha = format_cell_id(c, r, cols, GridLabelStyle::AlphaNumeric);
                let num = format_cell_id(c, r, cols, GridLabelStyle::Numeric);
                assert_eq!(parse_cell_id(&alpha, cols, rows), Some((c, r)));
                assert_eq!(
                    parse_cell_id(&alpha.to_lowercase(), cols, rows),
                    Some((c, r))
                );
                assert_eq!(parse_cell_id(&num, cols, rows), Some((c, r)));
            }
        }
        assert_eq!(format_cell_id(0, 0, 8, GridLabelStyle::AlphaNumeric), "A1");
        assert_eq!(format_cell_id(2, 1, 8, GridLabelStyle::AlphaNumeric), "B3");
        assert_eq!(format_cell_id(2, 1, 8, GridLabelStyle::Numeric), "11");
        assert_eq!(parse_cell_id("Z99", 8, 6), None);
        assert_eq!(parse_cell_id("0", 8, 6), None);
    }

    #[test]
    fn normalized_coordinates_round_trip_and_clamp() {
        // When dimensions >= 1000, every normalized coordinate 0..=999 round-trips exactly!
        let width = 1920;
        let height = 1080;
        for norm in 0..=NORMALIZED_MAX {
            let (px, py) = normalized_to_pixel(norm, norm, width, height);
            assert!(px < width && py < height);
            let (rx, ry) = pixel_to_normalized(px, py, width, height);
            assert_eq!(rx, norm, "x round-trip failed for norm={norm}");
            assert_eq!(ry, norm, "y round-trip failed for norm={norm}");
        }

        // When dimensions <= 1000, every pixel coordinate 0..dim round-trips through normalized!
        let small_w = 640;
        let small_h = 480;
        for x in 0..small_w {
            let (nx, _) = pixel_to_normalized(x, 0, small_w, small_h);
            let (px, _) = normalized_to_pixel(nx, 0, small_w, small_h);
            assert_eq!(px, x, "small dimension pixel round-trip failed for x={x}");
        }

        // Out-of-range inputs clamp cleanly to bounds
        assert_eq!(normalized_to_pixel(5000, 5000, 1000, 800), (999, 799));
        assert_eq!(pixel_to_normalized(5000, 5000, 1000, 800), (999, 999));
    }

    #[test]
    fn two_stage_zoom_crops_upscales_and_maps_back() {
        let img = RgbImage::from_pixel(800, 600, Rgb([50, 60, 70]));
        let zoomed = zoom_cell(&img, 4, 3, "B3").expect("B3 should be a valid cell in 4x3 grid");

        // Cell B3 is col=2, row=1 -> x in 400..600 (w=200), y in 200..400 (h=200)
        assert_eq!(zoomed.crop_bbox, BoundingBox::new(400, 200, 200, 200));
        assert_eq!(zoomed.upscale_factor, 3);
        assert_eq!(zoomed.image.dimensions(), (600, 600));
        assert_eq!(zoomed.fine_grid.cells.len(), 16);

        // Top-left fine cell A1 in the 4x4 fine grid covers (0..150, 0..150) in zoomed space
        // -> center is (75, 75) in zoomed space -> (25, 25) offset in crop -> (425, 225) in original!
        let orig_pt = zoomed
            .fine_cell_to_original_point("A1")
            .expect("fine cell A1 should exist");
        assert_eq!(orig_pt, (425, 225));

        // Normalized center (500, 500) in zoomed image maps to center of B3 (500, 300) in original
        let norm_pt = zoomed.normalized_to_original(500, 500);
        assert_eq!(norm_pt, (500, 300));
    }
}
