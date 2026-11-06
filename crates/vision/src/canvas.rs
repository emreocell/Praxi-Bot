//! Pure-Rust bitmap font and drawing primitives for image overlays.

use image::{Rgb, RgbImage};

use crate::BoundingBox;

/// Width of one unscaled glyph in pixels.
pub const GLYPH_WIDTH: u32 = 5;
/// Height of one unscaled glyph in pixels.
pub const GLYPH_HEIGHT: u32 = 7;
/// Horizontal spacing between glyphs in unscaled pixels.
pub const GLYPH_SPACING: u32 = 1;

/// Returns the 5x7 bitmap rows (each byte's low 5 bits represent pixels from left to right)
/// for an ASCII character.
#[allow(clippy::too_many_lines)]
const fn glyph_rows(ch: char) -> [u8; 7] {
    match ch.to_ascii_uppercase() {
        '0' => [
            0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
        ],
        '1' => [
            0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
        '2' => [
            0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111,
        ],
        '3' => [
            0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110,
        ],
        '4' => [
            0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
        ],
        '5' => [
            0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110,
        ],
        '6' => [
            0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
        ],
        '7' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
        ],
        '8' => [
            0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
        ],
        '9' => [
            0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100,
        ],
        'A' => [
            0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
        'B' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110,
        ],
        'C' => [
            0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110,
        ],
        'D' => [
            0b11100, 0b10010, 0b10001, 0b10001, 0b10001, 0b10010, 0b11100,
        ],
        'E' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111,
        ],
        'F' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
        'G' => [
            0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01110,
        ],
        'H' => [
            0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
        'I' => [
            0b01110, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
        'J' => [
            0b00111, 0b00010, 0b00010, 0b00010, 0b00010, 0b10010, 0b01100,
        ],
        'K' => [
            0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001,
        ],
        'L' => [
            0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111,
        ],
        'M' => [
            0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001,
        ],
        'N' => [
            0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001,
        ],
        'O' => [
            0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        'P' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
        'Q' => [
            0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101,
        ],
        'R' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001,
        ],
        'S' => [
            0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110,
        ],
        'T' => [
            0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        'U' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        'V' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100,
        ],
        'W' => [
            0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b10101, 0b01010,
        ],
        'X' => [
            0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001,
        ],
        'Y' => [
            0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        'Z' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111,
        ],
        '-' => [
            0b00000, 0b00000, 0b00000, 0b11111, 0b00000, 0b00000, 0b00000,
        ],
        '_' => [
            0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b11111,
        ],
        ':' => [
            0b00000, 0b00100, 0b00100, 0b00000, 0b00100, 0b00100, 0b00000,
        ],
        '.' => [
            0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00100, 0b00100,
        ],
        ',' => [
            0b00000, 0b00000, 0b00000, 0b00000, 0b00100, 0b00100, 0b01000,
        ],
        '/' => [
            0b00001, 0b00010, 0b00010, 0b00100, 0b01000, 0b01000, 0b10000,
        ],
        ' ' => [0b00000; 7],
        _ => [
            0b11111, 0b10001, 0b00010, 0b00100, 0b00100, 0b00000, 0b00100,
        ],
    }
}

/// Computes the pixel width and height of `text` rendered at integer `scale` (>= 1).
#[must_use]
pub fn measure_text(text: &str, scale: u32) -> (u32, u32) {
    let scale = scale.max(1);
    let count = u32::try_from(text.chars().count()).unwrap_or(0);
    if count == 0 {
        return (0, GLYPH_HEIGHT * scale);
    }
    let width = count * (GLYPH_WIDTH * scale) + (count - 1) * (GLYPH_SPACING * scale);
    let height = GLYPH_HEIGHT * scale;
    (width, height)
}

/// Computes the badge size (including padding on all four sides) for `text`.
#[must_use]
pub fn measure_badge(text: &str, scale: u32, padding: u32) -> (u32, u32) {
    let (tw, th) = measure_text(text, scale);
    (tw + padding * 2, th + padding * 2)
}

/// Returns white or black depending on which has higher contrast against `bg`.
#[must_use]
pub const fn contrasting_text_color(bg: Rgb<u8>) -> Rgb<u8> {
    // Perceived luminance (ITU-R BT.601 integer approximation: 299*R + 587*G + 114*B)
    let lum = (bg.0[0] as u32) * 299 + (bg.0[1] as u32) * 587 + (bg.0[2] as u32) * 114;
    if lum > 140_000 {
        Rgb([0, 0, 0])
    } else {
        Rgb([255, 255, 255])
    }
}

/// Blends `fg` over `bg` with `alpha` (`0..=255`).
#[must_use]
#[allow(clippy::cast_possible_truncation)]
pub const fn blend_rgb(bg: Rgb<u8>, fg: Rgb<u8>, alpha: u8) -> Rgb<u8> {
    let a = alpha as u32;
    let inv_a = 255 - a;
    let r = ((bg.0[0] as u32 * inv_a + fg.0[0] as u32 * a) / 255) as u8;
    let g = ((bg.0[1] as u32 * inv_a + fg.0[1] as u32 * a) / 255) as u8;
    let b = ((bg.0[2] as u32 * inv_a + fg.0[2] as u32 * a) / 255) as u8;
    Rgb([r, g, b])
}

/// Fills a solid rectangle clipped to the image bounds.
pub fn draw_filled_rect(img: &mut RgbImage, bbox: BoundingBox, color: Rgb<u8>) {
    let (img_w, img_h) = img.dimensions();
    let x_end = bbox.right().min(img_w);
    let y_end = bbox.bottom().min(img_h);
    for y in bbox.y.min(img_h)..y_end {
        for x in bbox.x.min(img_w)..x_end {
            img.put_pixel(x, y, color);
        }
    }
}

/// Draws a rectangle outline of `thickness` pixels inside/around `bbox`, clipped to the image.
pub fn draw_rect_outline(img: &mut RgbImage, bbox: BoundingBox, color: Rgb<u8>, thickness: u32) {
    let (img_w, img_h) = img.dimensions();
    if bbox.width == 0 || bbox.height == 0 || img_w == 0 || img_h == 0 {
        return;
    }
    let t = thickness.max(1);
    let left = bbox.x.min(img_w - 1);
    let top = bbox.y.min(img_h - 1);
    let right = bbox.right().min(img_w);
    let bottom = bbox.bottom().min(img_h);
    if right <= left || bottom <= top {
        return;
    }

    // Top and bottom horizontal bars
    draw_filled_rect(
        img,
        BoundingBox::new(left, top, right - left, t.min(bottom - top)),
        color,
    );
    draw_filled_rect(
        img,
        BoundingBox::new(
            left,
            bottom.saturating_sub(t).max(top),
            right - left,
            t.min(bottom - top),
        ),
        color,
    );
    // Left and right vertical bars
    draw_filled_rect(
        img,
        BoundingBox::new(left, top, t.min(right - left), bottom - top),
        color,
    );
    draw_filled_rect(
        img,
        BoundingBox::new(
            right.saturating_sub(t).max(left),
            top,
            t.min(right - left),
            bottom - top,
        ),
        color,
    );
}

/// Draws a vertical alpha-blended line at `x` from `y0` to `y1`.
pub fn draw_line_v(img: &mut RgbImage, x: u32, y0: u32, y1: u32, color: Rgb<u8>, alpha: u8) {
    let (img_w, img_h) = img.dimensions();
    if x >= img_w {
        return;
    }
    for y in y0.min(img_h)..y1.min(img_h) {
        let bg = *img.get_pixel(x, y);
        img.put_pixel(x, y, blend_rgb(bg, color, alpha));
    }
}

/// Draws a horizontal alpha-blended line at `y` from `x0` to `x1`.
pub fn draw_line_h(img: &mut RgbImage, x0: u32, x1: u32, y: u32, color: Rgb<u8>, alpha: u8) {
    let (img_w, img_h) = img.dimensions();
    if y >= img_h {
        return;
    }
    for x in x0.min(img_w)..x1.min(img_w) {
        let bg = *img.get_pixel(x, y);
        img.put_pixel(x, y, blend_rgb(bg, color, alpha));
    }
}

/// Draws `text` starting at `(x, y)` using the 5x7 bitmap font scaled by `scale`.
pub fn draw_text(img: &mut RgbImage, x: u32, y: u32, text: &str, color: Rgb<u8>, scale: u32) {
    let scale = scale.max(1);
    let (img_w, img_h) = img.dimensions();
    let step = (GLYPH_WIDTH + GLYPH_SPACING) * scale;

    for (idx, ch) in text.chars().enumerate() {
        let idx_u32 = u32::try_from(idx).unwrap_or(0);
        let gx = x.saturating_add(idx_u32.saturating_mul(step));
        let rows = glyph_rows(ch);
        for (row_idx, &row_bits) in rows.iter().enumerate() {
            let row_u32 = u32::try_from(row_idx).unwrap_or(0);
            for col_u32 in 0..GLYPH_WIDTH {
                let bit = (row_bits >> (GLYPH_WIDTH - 1 - col_u32)) & 1;
                if bit == 1 {
                    let origin_x = gx.saturating_add(col_u32 * scale);
                    let origin_y = y.saturating_add(row_u32 * scale);
                    for dy in 0..scale {
                        for dx in 0..scale {
                            let px = origin_x + dx;
                            let py = origin_y + dy;
                            if px < img_w && py < img_h {
                                img.put_pixel(px, py, color);
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Draws a filled label badge with `text` inside and returns the clamped badge bounding box.
pub fn draw_badge(
    img: &mut RgbImage,
    x: u32,
    y: u32,
    text: &str,
    bg_color: Rgb<u8>,
    scale: u32,
    padding: u32,
) -> BoundingBox {
    let (img_w, img_h) = img.dimensions();
    let (bw, bh) = measure_badge(text, scale, padding);
    let clamped_x = x.min(img_w.saturating_sub(bw));
    let clamped_y = y.min(img_h.saturating_sub(bh));
    let badge_rect = BoundingBox::new(clamped_x, clamped_y, bw.min(img_w), bh.min(img_h));

    draw_filled_rect(img, badge_rect, bg_color);
    let fg_color = contrasting_text_color(bg_color);
    draw_text(
        img,
        clamped_x + padding,
        clamped_y + padding,
        text,
        fg_color,
        scale,
    );
    badge_rect
}
