//! OCR, set-of-marks annotation, and two-step grid localisation for visual grounding.
//!
//! Used as a fallback when the AX/DOM tree is absent or incomplete; labels
//! on-screen elements with numbered marks and zooms into grid cells on demand.

pub mod canvas;
pub mod encode;
pub mod grid;
pub mod marks;
pub mod ocr;

pub use encode::{
    DownscaledImage, EncodedImage, ScaleMapping, downscale_max_dimension, encode_jpeg,
    encode_jpeg_with_budget, encode_png,
};
pub use grid::{
    GridConfig, GridLabelStyle, GridOverlay, NORMALIZED_MAX, NORMALIZED_SCALE, ZoomedCell,
    cell_at_point, cell_bounds, cell_bounds_by_id, cell_center, draw_grid, format_cell_id,
    normalized_bbox_to_pixel, normalized_to_pixel, parse_cell_id, pixel_bbox_to_normalized,
    pixel_to_normalized, zoom, zoom_cell,
};
pub use image::RgbImage;
pub use marks::{
    AnnotatedMarks, MARK_PALETTE, MarkOptions, annotate_marks, annotate_marks_with_options,
    annotate_text_boxes,
};
#[cfg(all(windows, feature = "windows-ocr"))]
pub use ocr::WindowsOcrEngine;
pub use ocr::{
    DETECTION_MODEL_FILENAME, DETECTION_MODEL_URL, OcrEngine, OcrGranularity, OcrModelPaths,
    OcrsEngine, RECOGNITION_MODEL_FILENAME, RECOGNITION_MODEL_URL, TextBox, download_models_to,
    ensure_default_models,
};

/// Errors returned by vision operations.
#[derive(Debug, thiserror::Error)]
pub enum VisionError {
    /// I/O error reading or writing model or image files.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Image decoding or encoding failure.
    #[error("image error: {0}")]
    Image(#[from] image::ImageError),

    /// OCR engine initialization or inference failure.
    #[error("OCR error: {0}")]
    Ocr(String),

    /// Invalid or out-of-range grid cell identifier.
    #[error("invalid grid cell identifier: {0}")]
    InvalidCell(String),
}

/// Axis-aligned bounding box in pixel (or `0..=999` normalized) coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct BoundingBox {
    /// Left X coordinate.
    pub x: u32,
    /// Top Y coordinate.
    pub y: u32,
    /// Box width.
    pub width: u32,
    /// Box height.
    pub height: u32,
}

/// Type alias for [`BoundingBox`].
pub type BBox = BoundingBox;
/// Type alias for [`BoundingBox`].
pub type Rect = BoundingBox;

impl BoundingBox {
    /// Creates a new [`BoundingBox`] from `(x, y, width, height)`.
    #[must_use]
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Creates a [`BoundingBox`] from `(left, top, right, bottom)`.
    #[must_use]
    pub const fn from_ltrb(left: u32, top: u32, right: u32, bottom: u32) -> Self {
        Self {
            x: left,
            y: top,
            width: right.saturating_sub(left),
            height: bottom.saturating_sub(top),
        }
    }

    /// Exclusive right edge (`x + width`).
    #[must_use]
    pub const fn right(&self) -> u32 {
        self.x.saturating_add(self.width)
    }

    /// Exclusive bottom edge (`y + height`).
    #[must_use]
    pub const fn bottom(&self) -> u32 {
        self.y.saturating_add(self.height)
    }

    /// Center point `(cx, cy)` of the bounding box.
    #[must_use]
    pub const fn center(&self) -> (u32, u32) {
        (
            self.x.saturating_add(self.width / 2),
            self.y.saturating_add(self.height / 2),
        )
    }

    /// Area of the bounding box in square units.
    #[must_use]
    pub const fn area(&self) -> u64 {
        (self.width as u64) * (self.height as u64)
    }

    /// Returns `true` if `self` and `other` have a positive-area intersection.
    #[must_use]
    pub const fn intersects(&self, other: &Self) -> bool {
        self.intersection_area(other) > 0
    }

    /// Computes the overlapping area between `self` and `other`.
    #[must_use]
    pub const fn intersection_area(&self, other: &Self) -> u64 {
        let x0 = if self.x > other.x { self.x } else { other.x };
        let y0 = if self.y > other.y { self.y } else { other.y };
        let r_self = self.right();
        let r_other = other.right();
        let x1 = if r_self < r_other { r_self } else { r_other };
        let b_self = self.bottom();
        let b_other = other.bottom();
        let y1 = if b_self < b_other { b_self } else { b_other };

        if x1 <= x0 || y1 <= y0 {
            0
        } else {
            ((x1 - x0) as u64) * ((y1 - y0) as u64)
        }
    }

    /// Clamps `self` to `[0..max_width, 0..max_height]`, returning `None` if the clamped box is empty.
    #[must_use]
    pub const fn clamp_to(&self, max_width: u32, max_height: u32) -> Option<Self> {
        let left = if self.x > max_width {
            max_width
        } else {
            self.x
        };
        let top = if self.y > max_height {
            max_height
        } else {
            self.y
        };
        let r = self.right();
        let right = if r > max_width { max_width } else { r };
        let b = self.bottom();
        let bottom = if b > max_height { max_height } else { b };

        if right <= left || bottom <= top {
            None
        } else {
            Some(Self::from_ltrb(left, top, right, bottom))
        }
    }
}
