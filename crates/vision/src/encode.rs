//! Image encoding (JPEG/PNG with quality and byte-size budgets) and max-dimension
//! downscaling with reversible coordinate mapping.

use std::io::Cursor;

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::imageops::{self, FilterType};
use image::{ExtendedColorType, ImageEncoder, RgbImage};

use crate::{BoundingBox, VisionError};

/// Reversible coordinate mapping between an original screenshot and a downscaled copy.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScaleMapping {
    /// Width of the original image in pixels.
    pub original_width: u32,
    /// Height of the original image in pixels.
    pub original_height: u32,
    /// Width of the downscaled image in pixels.
    pub scaled_width: u32,
    /// Height of the downscaled image in pixels.
    pub scaled_height: u32,
    /// Uniform scale factor (`scaled / original`, in `(0.0, 1.0]`).
    pub scale: f64,
}

impl ScaleMapping {
    /// Maps a point `(scaled_x, scaled_y)` from the downscaled image back to original
    /// image pixel coordinates, clamped to the original image dimensions.
    #[must_use]
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    pub fn map_to_original(&self, scaled_x: u32, scaled_y: u32) -> (u32, u32) {
        if self.original_width == 0
            || self.original_height == 0
            || self.scaled_width == 0
            || self.scaled_height == 0
        {
            return (0, 0);
        }
        if self.scaled_width == self.original_width && self.scaled_height == self.original_height {
            return (
                scaled_x.min(self.original_width - 1),
                scaled_y.min(self.original_height - 1),
            );
        }
        let ox = ((f64::from(scaled_x) * f64::from(self.original_width))
            / f64::from(self.scaled_width))
        .round() as u32;
        let oy = ((f64::from(scaled_y) * f64::from(self.original_height))
            / f64::from(self.scaled_height))
        .round() as u32;
        (
            ox.min(self.original_width - 1),
            oy.min(self.original_height - 1),
        )
    }

    /// Maps a point `(orig_x, orig_y)` from the original image into the downscaled
    /// image's coordinate space, clamped to the downscaled dimensions.
    #[must_use]
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    pub fn map_to_scaled(&self, orig_x: u32, orig_y: u32) -> (u32, u32) {
        if self.original_width == 0
            || self.original_height == 0
            || self.scaled_width == 0
            || self.scaled_height == 0
        {
            return (0, 0);
        }
        if self.scaled_width == self.original_width && self.scaled_height == self.original_height {
            return (
                orig_x.min(self.scaled_width - 1),
                orig_y.min(self.scaled_height - 1),
            );
        }
        let sx = ((f64::from(orig_x) * f64::from(self.scaled_width))
            / f64::from(self.original_width))
        .round() as u32;
        let sy = ((f64::from(orig_y) * f64::from(self.scaled_height))
            / f64::from(self.original_height))
        .round() as u32;
        (
            sx.min(self.scaled_width - 1),
            sy.min(self.scaled_height - 1),
        )
    }

    /// Maps a [`BoundingBox`] from the downscaled image back to the original image space.
    #[must_use]
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    pub fn map_bbox_to_original(&self, bbox: BoundingBox) -> BoundingBox {
        if self.scaled_width == 0 || self.scaled_height == 0 {
            return BoundingBox::new(0, 0, 0, 0);
        }
        if self.scaled_width == self.original_width && self.scaled_height == self.original_height {
            return bbox;
        }
        let ratio_x = f64::from(self.original_width) / f64::from(self.scaled_width);
        let ratio_y = f64::from(self.original_height) / f64::from(self.scaled_height);
        let x = (f64::from(bbox.x) * ratio_x).round() as u32;
        let y = (f64::from(bbox.y) * ratio_y).round() as u32;
        let w = (f64::from(bbox.width) * ratio_x).round() as u32;
        let h = (f64::from(bbox.height) * ratio_y).round() as u32;
        let clamped_x = x.min(self.original_width);
        let clamped_y = y.min(self.original_height);
        BoundingBox::new(
            clamped_x,
            clamped_y,
            w.min(self.original_width.saturating_sub(clamped_x)),
            h.min(self.original_height.saturating_sub(clamped_y)),
        )
    }

    /// Maps a [`BoundingBox`] from the original image into the downscaled image space.
    #[must_use]
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    pub fn map_bbox_to_scaled(&self, bbox: BoundingBox) -> BoundingBox {
        if self.original_width == 0 || self.original_height == 0 {
            return BoundingBox::new(0, 0, 0, 0);
        }
        if self.scaled_width == self.original_width && self.scaled_height == self.original_height {
            return bbox;
        }
        let ratio_x = f64::from(self.scaled_width) / f64::from(self.original_width);
        let ratio_y = f64::from(self.scaled_height) / f64::from(self.original_height);
        let x = (f64::from(bbox.x) * ratio_x).round() as u32;
        let y = (f64::from(bbox.y) * ratio_y).round() as u32;
        let w = (f64::from(bbox.width) * ratio_x).round() as u32;
        let h = (f64::from(bbox.height) * ratio_y).round() as u32;
        let clamped_x = x.min(self.scaled_width);
        let clamped_y = y.min(self.scaled_height);
        BoundingBox::new(
            clamped_x,
            clamped_y,
            w.min(self.scaled_width.saturating_sub(clamped_x)),
            h.min(self.scaled_height.saturating_sub(clamped_y)),
        )
    }
}

/// Result of downscaling an image to fit within a maximum dimension.
#[derive(Debug, Clone)]
pub struct DownscaledImage {
    /// The (possibly downscaled) RGB image.
    pub image: RgbImage,
    /// Coordinate mapping between `image` and the original screenshot.
    pub mapping: ScaleMapping,
}

/// Downscales `img` proportionally so that `max(width, height) <= max_dimension`,
/// keeping the exact scale factor and dimension mapping so coordinates map back.
///
/// If both dimensions are already `<= max_dimension` (or `max_dimension == 0`),
/// the image is returned unchanged with `scale == 1.0`.
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
pub fn downscale_max_dimension(img: &RgbImage, max_dimension: u32) -> DownscaledImage {
    let (orig_w, orig_h) = img.dimensions();
    let max_orig = orig_w.max(orig_h);

    if max_dimension == 0 || max_orig == 0 || max_orig <= max_dimension {
        return DownscaledImage {
            image: img.clone(),
            mapping: ScaleMapping {
                original_width: orig_w,
                original_height: orig_h,
                scaled_width: orig_w,
                scaled_height: orig_h,
                scale: 1.0,
            },
        };
    }

    let scale = f64::from(max_dimension) / f64::from(max_orig);
    let scaled_w = ((f64::from(orig_w) * scale).round() as u32)
        .max(1)
        .min(max_dimension);
    let scaled_h = ((f64::from(orig_h) * scale).round() as u32)
        .max(1)
        .min(max_dimension);

    let resized = imageops::resize(img, scaled_w, scaled_h, FilterType::Lanczos3);

    DownscaledImage {
        image: resized,
        mapping: ScaleMapping {
            original_width: orig_w,
            original_height: orig_h,
            scaled_width: scaled_w,
            scaled_height: scaled_h,
            scale,
        },
    }
}

/// Encoded image payload with metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedImage {
    /// Encoded image bytes.
    pub bytes: Vec<u8>,
    /// MIME type (`"image/jpeg"` or `"image/png"`).
    pub mime_type: &'static str,
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Effective JPEG quality used (`1..=100`), or `None` for lossless PNG.
    pub quality: Option<u8>,
}

/// Encodes `img` as JPEG at the specified `quality` (`1..=100`).
///
/// # Errors
///
/// Returns [`VisionError::Image`] if encoding fails.
pub fn encode_jpeg(img: &RgbImage, quality: u8) -> Result<EncodedImage, VisionError> {
    let q = quality.clamp(1, 100);
    let (width, height) = img.dimensions();
    let mut buf = Cursor::new(Vec::new());
    let encoder = JpegEncoder::new_with_quality(&mut buf, q);
    encoder.write_image(img.as_raw(), width, height, ExtendedColorType::Rgb8)?;
    Ok(EncodedImage {
        bytes: buf.into_inner(),
        mime_type: "image/jpeg",
        width,
        height,
        quality: Some(q),
    })
}

/// Encodes `img` as lossless PNG.
///
/// # Errors
///
/// Returns [`VisionError::Image`] if encoding fails.
pub fn encode_png(img: &RgbImage) -> Result<EncodedImage, VisionError> {
    let (width, height) = img.dimensions();
    let mut buf = Cursor::new(Vec::new());
    let encoder = PngEncoder::new(&mut buf);
    encoder.write_image(img.as_raw(), width, height, ExtendedColorType::Rgb8)?;
    Ok(EncodedImage {
        bytes: buf.into_inner(),
        mime_type: "image/png",
        width,
        height,
        quality: None,
    })
}

/// Encodes `img` as JPEG starting at `initial_quality` and stepping down towards
/// `min_quality` until the encoded size is `<= max_bytes` (or `min_quality` is reached).
///
/// # Errors
///
/// Returns [`VisionError::Image`] if JPEG encoding fails.
pub fn encode_jpeg_with_budget(
    img: &RgbImage,
    initial_quality: u8,
    min_quality: u8,
    max_bytes: usize,
) -> Result<EncodedImage, VisionError> {
    let high = initial_quality.clamp(1, 100);
    let low = min_quality.clamp(1, high);

    let first = encode_jpeg(img, high)?;
    if first.bytes.len() <= max_bytes || high == low {
        return Ok(first);
    }

    let mut current_q = high;
    let mut last_encoded = first;

    while current_q > low {
        current_q = current_q.saturating_sub(10).max(low);
        last_encoded = encode_jpeg(img, current_q)?;
        if last_encoded.bytes.len() <= max_bytes {
            return Ok(last_encoded);
        }
    }

    Ok(last_encoded)
}

#[cfg(test)]
mod tests {
    use image::Rgb;

    use super::*;

    #[test]
    fn downscale_preserves_small_image() {
        let img = RgbImage::from_pixel(640, 480, Rgb([10, 20, 30]));
        let result = downscale_max_dimension(&img, 1280);
        assert_eq!(result.image.dimensions(), (640, 480));
        assert!((result.mapping.scale - 1.0).abs() < f64::EPSILON);
        assert_eq!(result.mapping.map_to_original(320, 240), (320, 240));
    }

    #[test]
    fn downscale_maps_coordinates_and_bboxes_back() {
        let img = RgbImage::from_pixel(2000, 1000, Rgb([100, 150, 200]));
        let downscaled = downscale_max_dimension(&img, 1000);
        assert_eq!(downscaled.image.dimensions(), (1000, 500));
        assert!((downscaled.mapping.scale - 0.5).abs() < 1e-6);

        // Point round-trip
        let (sx, sy) = downscaled.mapping.map_to_scaled(1400, 600);
        assert_eq!((sx, sy), (700, 300));
        let (ox, oy) = downscaled.mapping.map_to_original(sx, sy);
        assert_eq!((ox, oy), (1400, 600));

        // BBox round-trip
        let orig_bbox = BoundingBox::new(200, 100, 400, 200);
        let scaled_bbox = downscaled.mapping.map_bbox_to_scaled(orig_bbox);
        assert_eq!(scaled_bbox, BoundingBox::new(100, 50, 200, 100));
        let mapped_back = downscaled.mapping.map_bbox_to_original(scaled_bbox);
        assert_eq!(mapped_back, orig_bbox);
    }

    #[test]
    fn jpeg_and_png_encoding_with_quality_budget() {
        let mut img = RgbImage::new(256, 256);
        for (x, y, pixel) in img.enumerate_pixels_mut() {
            let r = u8::try_from((x * 7 + y * 13) % 256).unwrap_or(0);
            let g = u8::try_from((x * 17 + y * 3) % 256).unwrap_or(0);
            let b = u8::try_from((x ^ y) % 256).unwrap_or(0);
            *pixel = Rgb([r, g, b]);
        }

        let png = encode_png(&img).expect("PNG encode should succeed");
        assert_eq!(png.mime_type, "image/png");
        assert_eq!(png.quality, None);
        assert_ne!(png.bytes.len(), 0);

        let high_q = encode_jpeg(&img, 95).expect("JPEG 95 should succeed");
        let low_q = encode_jpeg(&img, 20).expect("JPEG 20 should succeed");
        assert!(
            low_q.bytes.len() < high_q.bytes.len(),
            "lower quality JPEG should produce fewer bytes ({} vs {})",
            low_q.bytes.len(),
            high_q.bytes.len()
        );

        let budget = usize::midpoint(high_q.bytes.len(), low_q.bytes.len());
        let budgeted =
            encode_jpeg_with_budget(&img, 95, 20, budget).expect("budgeted JPEG should succeed");
        assert!(
            budgeted.bytes.len() <= budget,
            "budgeted size {} should be <= budget {}",
            budgeted.bytes.len(),
            budget
        );
        assert!(budgeted.quality.unwrap() < 95);
    }
}
