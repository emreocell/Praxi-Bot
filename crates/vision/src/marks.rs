//! Set-of-Marks (`SoM`) annotation: draws numbered bounding boxes with high-contrast,
//! overlap-avoiding badges on a screenshot copy and returns the `label -> BoundingBox` map.

use std::collections::BTreeMap;

use image::{Rgb, RgbImage};

use crate::BoundingBox;
use crate::canvas::{draw_badge, draw_rect_outline, measure_badge};
use crate::ocr::TextBox;

/// High-contrast color palette used for Set-of-Marks outlines and badges.
pub const MARK_PALETTE: [Rgb<u8>; 8] = [
    Rgb([215, 25, 45]),  // Crimson
    Rgb([20, 95, 215]),  // Royal Blue
    Rgb([15, 145, 55]),  // Forest Green
    Rgb([210, 95, 0]),   // Amber Orange
    Rgb([140, 30, 185]), // Purple
    Rgb([0, 135, 145]),  // Teal
    Rgb([190, 20, 115]), // Magenta
    Rgb([40, 55, 160]),  // Indigo
];

/// Configuration options for Set-of-Marks rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarkOptions {
    /// First numeric label to assign (defaults to `1`).
    pub start_label: u32,
    /// Outline thickness in pixels (defaults to `2`).
    pub outline_thickness: u32,
    /// Integer scale factor for the bitmap font (defaults to `2`).
    pub font_scale: u32,
    /// Inner padding in pixels around the label text (defaults to `2`).
    pub badge_padding: u32,
}

impl Default for MarkOptions {
    fn default() -> Self {
        Self {
            start_label: 1,
            outline_thickness: 2,
            font_scale: 2,
            badge_padding: 2,
        }
    }
}

/// Output of Set-of-Marks annotation.
#[derive(Debug, Clone)]
pub struct AnnotatedMarks {
    /// Annotated copy of the input screenshot.
    pub image: RgbImage,
    /// Map from numeric mark label (`1, 2, ...`) to the clamped target [`BoundingBox`].
    pub marks: BTreeMap<u32, BoundingBox>,
    /// Map from numeric mark label (`1, 2, ...`) to the placed label badge [`BoundingBox`].
    pub badge_boxes: BTreeMap<u32, BoundingBox>,
}

/// Candidate badge placements around/inside `target_box`, ordered from most preferred
/// (outside top-left / top-right / bottom / sides) to fallback (inside corners).
fn candidate_badge_positions(
    target_box: BoundingBox,
    badge_w: u32,
    badge_h: u32,
    img_w: u32,
    img_h: u32,
) -> Vec<BoundingBox> {
    let gap = 2_u32;
    let max_x = img_w.saturating_sub(badge_w);
    let max_y = img_h.saturating_sub(badge_h);
    let mut candidates = Vec::with_capacity(10);

    // 1. Above top-left
    if target_box.y >= badge_h + gap {
        candidates.push(BoundingBox::new(
            target_box.x.min(max_x),
            target_box.y - badge_h - gap,
            badge_w,
            badge_h,
        ));
    }
    // 2. Above top-right
    if target_box.y >= badge_h + gap {
        candidates.push(BoundingBox::new(
            target_box.right().saturating_sub(badge_w).min(max_x),
            target_box.y - badge_h - gap,
            badge_w,
            badge_h,
        ));
    }
    // 3. Below bottom-left
    if target_box.bottom() + badge_h + gap <= img_h {
        candidates.push(BoundingBox::new(
            target_box.x.min(max_x),
            (target_box.bottom() + gap).min(max_y),
            badge_w,
            badge_h,
        ));
    }
    // 4. Below bottom-right
    if target_box.bottom() + badge_h + gap <= img_h {
        candidates.push(BoundingBox::new(
            target_box.right().saturating_sub(badge_w).min(max_x),
            (target_box.bottom() + gap).min(max_y),
            badge_w,
            badge_h,
        ));
    }
    // 5. Left of box
    if target_box.x >= badge_w + gap {
        candidates.push(BoundingBox::new(
            target_box.x - badge_w - gap,
            target_box.y.min(max_y),
            badge_w,
            badge_h,
        ));
    }
    // 6. Right of box
    if target_box.right() + badge_w + gap <= img_w {
        candidates.push(BoundingBox::new(
            (target_box.right() + gap).min(max_x),
            target_box.y.min(max_y),
            badge_w,
            badge_h,
        ));
    }
    // 7. Inside top-left
    candidates.push(BoundingBox::new(
        target_box.x.min(max_x),
        target_box.y.min(max_y),
        badge_w,
        badge_h,
    ));
    // 8. Inside top-right
    candidates.push(BoundingBox::new(
        target_box.right().saturating_sub(badge_w).min(max_x),
        target_box.y.min(max_y),
        badge_w,
        badge_h,
    ));
    // 9. Inside bottom-left
    candidates.push(BoundingBox::new(
        target_box.x.min(max_x),
        target_box.bottom().saturating_sub(badge_h).min(max_y),
        badge_w,
        badge_h,
    ));
    // 10. Inside bottom-right
    candidates.push(BoundingBox::new(
        target_box.right().saturating_sub(badge_w).min(max_x),
        target_box.bottom().saturating_sub(badge_h).min(max_y),
        badge_w,
        badge_h,
    ));

    candidates
}

/// Selects the candidate badge rectangle with minimum overlap penalty against
/// already-placed badges and other target boxes.
fn choose_badge_rect(
    target_idx: usize,
    target_box: BoundingBox,
    all_targets: &[BoundingBox],
    placed_badges: &[BoundingBox],
    badge_size: (u32, u32),
    img_size: (u32, u32),
) -> BoundingBox {
    let (img_w, img_h) = img_size;
    let bw = badge_size.0.min(img_w);
    let bh = badge_size.1.min(img_h);
    let candidates = candidate_badge_positions(target_box, bw, bh, img_w, img_h);

    let mut best_rect = BoundingBox::new(
        target_box.x.min(img_w.saturating_sub(bw)),
        target_box.y.min(img_h.saturating_sub(bh)),
        bw,
        bh,
    );
    let mut best_penalty = u64::MAX;

    for candidate in candidates {
        // Heavily penalize overlapping existing badges so labels stay readable.
        let badge_overlap: u64 = placed_badges
            .iter()
            .map(|b| candidate.intersection_area(b))
            .sum();

        // Moderately penalize covering other target boxes.
        let other_target_overlap: u64 = all_targets
            .iter()
            .enumerate()
            .filter(|&(idx, _)| idx != target_idx)
            .map(|(_, b)| candidate.intersection_area(b))
            .sum();

        // Lightly penalize covering own box so exterior placement wins when free.
        let self_overlap = candidate.intersection_area(&target_box);

        let penalty = badge_overlap
            .saturating_mul(100)
            .saturating_add(other_target_overlap.saturating_mul(10))
            .saturating_add(self_overlap);

        if penalty == 0 {
            return candidate;
        }
        if penalty < best_penalty {
            best_penalty = penalty;
            best_rect = candidate;
        }
    }

    best_rect
}

/// Draws numbered Set-of-Marks boxes and contrasting labels on a copy of `img`
/// using default [`MarkOptions`].
#[must_use]
pub fn annotate_marks(img: &RgbImage, boxes: &[BoundingBox]) -> AnnotatedMarks {
    annotate_marks_with_options(img, boxes, &MarkOptions::default())
}

/// Draws numbered Set-of-Marks boxes and contrasting labels on a copy of `img`
/// using custom [`MarkOptions`].
#[must_use]
pub fn annotate_marks_with_options(
    img: &RgbImage,
    boxes: &[BoundingBox],
    options: &MarkOptions,
) -> AnnotatedMarks {
    let mut annotated = img.clone();
    let (img_w, img_h) = annotated.dimensions();
    let mut marks = BTreeMap::new();
    let mut badge_boxes = BTreeMap::new();

    if img_w == 0 || img_h == 0 {
        return AnnotatedMarks {
            image: annotated,
            marks,
            badge_boxes,
        };
    }

    let clamped_targets: Vec<BoundingBox> = boxes
        .iter()
        .filter_map(|b| b.clamp_to(img_w, img_h))
        .collect();

    let mut placed_badges: Vec<BoundingBox> = Vec::with_capacity(clamped_targets.len());

    // First pass: draw all bounding box outlines and compute non-overlapping badge rects.
    let mut planned_badges: Vec<(u32, BoundingBox, Rgb<u8>, String)> =
        Vec::with_capacity(clamped_targets.len());

    for (idx, &target_box) in clamped_targets.iter().enumerate() {
        let idx_u32 = u32::try_from(idx).unwrap_or(0);
        let label = options.start_label.saturating_add(idx_u32);
        let color = MARK_PALETTE[idx % MARK_PALETTE.len()];

        draw_rect_outline(&mut annotated, target_box, color, options.outline_thickness);

        let label_text = label.to_string();
        let (bw, bh) = measure_badge(&label_text, options.font_scale, options.badge_padding);
        let badge_rect = choose_badge_rect(
            idx,
            target_box,
            &clamped_targets,
            &placed_badges,
            (bw, bh),
            (img_w, img_h),
        );

        placed_badges.push(badge_rect);
        marks.insert(label, target_box);
        planned_badges.push((label, badge_rect, color, label_text));
    }

    // Second pass: draw all badges on top of box outlines so labels are never overwritten by outlines.
    for (label, badge_rect, color, label_text) in planned_badges {
        let actual = draw_badge(
            &mut annotated,
            badge_rect.x,
            badge_rect.y,
            &label_text,
            color,
            options.font_scale,
            options.badge_padding,
        );
        badge_boxes.insert(label, actual);
    }

    AnnotatedMarks {
        image: annotated,
        marks,
        badge_boxes,
    }
}

/// Convenience helper that extracts bounding boxes from OCR [`TextBox`]es and annotates `img`.
#[must_use]
pub fn annotate_text_boxes(img: &RgbImage, text_boxes: &[TextBox]) -> AnnotatedMarks {
    let boxes: Vec<BoundingBox> = text_boxes.iter().map(|tb| tb.bbox).collect();
    annotate_marks(img, &boxes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mark_placement_returns_expected_map_and_modifies_image() {
        let img = RgbImage::from_pixel(400, 300, Rgb([250, 250, 250]));
        let boxes = [
            BoundingBox::new(40, 50, 80, 30),
            BoundingBox::new(200, 100, 90, 40),
        ];

        let result = annotate_marks(&img, &boxes);
        assert_eq!(result.marks.len(), 2);
        assert_eq!(result.marks.get(&1), Some(&boxes[0]));
        assert_eq!(result.marks.get(&2), Some(&boxes[1]));
        assert_ne!(result.image.as_raw(), img.as_raw());
    }

    #[test]
    fn mark_placement_avoids_badge_overlap_for_identical_or_adjacent_boxes() {
        let img = RgbImage::from_pixel(400, 300, Rgb([240, 240, 240]));
        // Two boxes at the exact same position and a third right next to them
        let boxes = [
            BoundingBox::new(100, 100, 80, 30),
            BoundingBox::new(100, 100, 80, 30),
            BoundingBox::new(105, 100, 80, 30),
        ];

        let result = annotate_marks(&img, &boxes);
        assert_eq!(result.badge_boxes.len(), 3);

        let b1 = result.badge_boxes[&1];
        let b2 = result.badge_boxes[&2];
        let b3 = result.badge_boxes[&3];

        assert!(
            !b1.intersects(&b2),
            "badge 1 ({b1:?}) and badge 2 ({b2:?}) should not overlap"
        );
        assert!(
            !b1.intersects(&b3),
            "badge 1 ({b1:?}) and badge 3 ({b3:?}) should not overlap"
        );
        assert!(
            !b2.intersects(&b3),
            "badge 2 ({b2:?}) and badge 3 ({b3:?}) should not overlap"
        );
    }

    #[test]
    fn mark_placement_clamps_boxes_at_image_edges() {
        let img = RgbImage::from_pixel(200, 150, Rgb([20, 20, 20]));
        // Box right at top-left corner (y=0) where above-box placement is out of bounds
        let boxes = [BoundingBox::new(0, 0, 60, 25)];
        let result = annotate_marks(&img, &boxes);
        assert_eq!(result.marks.get(&1), Some(&boxes[0]));
        let badge = result.badge_boxes[&1];
        assert!(badge.right() <= 200);
        assert!(badge.bottom() <= 150);
    }
}
