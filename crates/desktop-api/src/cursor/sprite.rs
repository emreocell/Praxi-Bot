//! The bot cursor's pixels: an arrow with a white outline and a click ripple, drawn into a
//! premultiplied BGRA buffer. Pure and platform-free, so the Windows (layered window) and X11
//! (shape mask) surfaces share it and it is tested on every platform.
//!
//! The canvas is a square. The cursor tip (the click point) is at its centre, so the window
//! never needs to move to show a ripple around the point.

/// Supersampling per axis when the arrow is rasterised.
const SS: u32 = 4;
/// Canvas side at 96 dpi, in pixels.
pub const BASE_CANVAS: u32 = 72;
/// Frames in one click ripple.
pub const RIPPLE_FRAMES: usize = 8;
/// Pause between ripple frames, in milliseconds.
pub const RIPPLE_FRAME_MS: u64 = 45;

/// The arrow's colour (distinct from any system cursor): orange, as B, G, R.
const FILL: [u8; 3] = [0x00, 0x6A, 0xFF];
/// The arrow's outline: white.
const OUTLINE: [u8; 3] = [0xFF, 0xFF, 0xFF];

/// Arrow polygon in 96-dpi pixels relative to the tip.
const ARROW: [(f64, f64); 7] = [
    (0.0, 0.0),
    (0.0, 17.0),
    (4.5, 13.0),
    (7.5, 20.0),
    (10.5, 18.5),
    (7.5, 11.5),
    (12.5, 11.5),
];

/// Premultiplied BGRA pixels, row-major, top row first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bitmap {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width * height * 4` bytes: B, G, R, A with colour already multiplied by alpha.
    pub bgra: Vec<u8>,
}

impl Bitmap {
    fn blank(side: u32) -> Self {
        Self {
            width: side,
            height: side,
            bgra: vec![0; (side as usize) * (side as usize) * 4],
        }
    }

    /// The alpha of one pixel (0 outside the bitmap).
    #[must_use]
    pub fn alpha(&self, x: u32, y: u32) -> u8 {
        if x >= self.width || y >= self.height {
            return 0;
        }
        self.bgra[((y * self.width + x) as usize) * 4 + 3]
    }

    /// Source-over: draw `src` (premultiplied) onto pixel `(x, y)`.
    fn over(&mut self, x: u32, y: u32, src: [u8; 4]) {
        let i = ((y * self.width + x) as usize) * 4;
        let inv = 255 - u32::from(src[3]);
        for c in 0..4 {
            let d = u32::from(self.bgra[i + c]);
            self.bgra[i + c] = (u32::from(src[c]) + (d * inv + 127) / 255).min(255) as u8;
        }
    }
}

/// Canvas side for a DPI scale.
#[must_use]
pub fn canvas_size(scale: f64) -> u32 {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let side = (f64::from(BASE_CANVAS) * dpi_scale_from(scale)).round() as u32;
    // Even, so the tip pixel is exactly centred.
    side.max(BASE_CANVAS) & !1
}

fn dpi_scale_from(scale: f64) -> f64 {
    if scale.is_finite() {
        scale.clamp(1.0, 4.0)
    } else {
        1.0
    }
}

fn inside_polygon(p: (f64, f64), poly: &[(f64, f64)]) -> bool {
    let mut inside = false;
    let mut j = poly.len() - 1;
    for i in 0..poly.len() {
        let (a, b) = (poly[i], poly[j]);
        if (a.1 > p.1) != (b.1 > p.1) && p.0 < (b.0 - a.0) * (p.1 - a.1) / (b.1 - a.1) + a.0 {
            inside = !inside;
        }
        j = i;
    }
    inside
}

fn distance_to_segment(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let t = if len2 == 0.0 {
        0.0
    } else {
        (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0)
    };
    (p.0 - (a.0 + t * dx)).hypot(p.1 - (a.1 + t * dy))
}

/// The arrow alone, tip at the canvas centre.
#[must_use]
pub fn render_arrow(scale: f64) -> Bitmap {
    let scale = dpi_scale_from(scale);
    let side = canvas_size(scale);
    let mut bmp = Bitmap::blank(side);
    let centre = f64::from(side / 2);
    let poly: Vec<(f64, f64)> = ARROW
        .iter()
        .map(|(x, y)| (centre + x * scale, centre + y * scale))
        .collect();
    let outline_w = 1.6 * scale;
    for py in 0..side {
        for px in 0..side {
            let (mut fill, mut shape) = (0u32, 0u32);
            for sy in 0..SS {
                for sx in 0..SS {
                    let p = (
                        f64::from(px) + (f64::from(sx) + 0.5) / f64::from(SS),
                        f64::from(py) + (f64::from(sy) + 0.5) / f64::from(SS),
                    );
                    let inside = inside_polygon(p, &poly);
                    let near = inside
                        || (0..poly.len()).any(|i| {
                            distance_to_segment(p, poly[i], poly[(i + 1) % poly.len()]) <= outline_w
                        });
                    if inside {
                        fill += 1;
                    }
                    if near {
                        shape += 1;
                    }
                }
            }
            if shape == 0 {
                continue;
            }
            let total = SS * SS;
            // Outline colour where the shape is covered but the fill is not, fill colour on the
            // fill; coverage of the whole shape is the alpha.
            let a = (shape * 255 + total / 2) / total;
            let fill_share = f64::from(fill) / f64::from(shape);
            let mut px_bgra = [0u8; 4];
            for k in 0..3 {
                let col =
                    f64::from(FILL[k]) * fill_share + f64::from(OUTLINE[k]) * (1.0 - fill_share);
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                {
                    px_bgra[k] = (col * f64::from(a) / 255.0).round() as u8;
                }
            }
            px_bgra[3] = u8::try_from(a).unwrap_or(u8::MAX);
            bmp.over(px, py, px_bgra);
        }
    }
    bmp
}

/// Ring radius and thickness (96-dpi pixels) of ripple frame `frame`.
#[must_use]
pub fn ripple_ring(frame: usize) -> (f64, f64) {
    let last = (RIPPLE_FRAMES - 1).max(1);
    #[allow(clippy::cast_precision_loss)]
    let t = frame.min(last) as f64 / last as f64;
    (6.0 + 26.0 * t, 4.0 - 3.0 * t)
}

/// `arrow` with ripple frame `frame` drawn under it, centred on the tip. `None`: the arrow only.
#[must_use]
pub fn compose(arrow: &Bitmap, scale: f64, frame: Option<usize>) -> Bitmap {
    let Some(frame) = frame else {
        return arrow.clone();
    };
    let scale = dpi_scale_from(scale);
    let side = arrow.width;
    let centre = f64::from(side / 2);
    let (ring, thick) = ripple_ring(frame);
    let (r, half) = (ring * scale, thick * scale / 2.0);
    let mut out = Bitmap::blank(side);
    let last = (RIPPLE_FRAMES - 1).max(1);
    #[allow(clippy::cast_precision_loss)]
    let fade = 1.0 - 0.6 * (frame.min(last) as f64 / last as f64);
    for py in 0..side {
        for px in 0..side {
            let d = (f64::from(px) + 0.5 - centre).hypot(f64::from(py) + 0.5 - centre);
            let cover = (half + 0.5 - (d - r).abs()).clamp(0.0, 1.0);
            if cover <= 0.0 {
                continue;
            }
            let a = cover * fade;
            let mut v = [0u8; 4];
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            {
                for k in 0..3 {
                    v[k] = (f64::from(FILL[k]) * a).round() as u8;
                }
                v[3] = (255.0 * a).round() as u8;
            }
            out.over(px, py, v);
        }
    }
    for py in 0..side {
        for px in 0..side {
            let i = ((py * side + px) as usize) * 4;
            let s = [
                arrow.bgra[i],
                arrow.bgra[i + 1],
                arrow.bgra[i + 2],
                arrow.bgra[i + 3],
            ];
            if s[3] != 0 {
                out.over(px, py, s);
            }
        }
    }
    out
}

/// A run of opaque-enough pixels: `(x, y, width, height)`.
pub type Rect = (i16, i16, u16, u16);

/// The pixels whose alpha is at least `threshold` as a short list of rectangles (row runs,
/// identical runs on consecutive rows merged): the shape of a window on a server with no
/// compositor (X11 `Shape` bounding region).
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // canvas sides are a few hundred pixels
pub fn mask_rects(bmp: &Bitmap, threshold: u8) -> Vec<Rect> {
    let mut out: Vec<Rect> = Vec::new();
    let mut open: Vec<(i16, u16, i16)> = Vec::new(); // (x, w, y0) of runs continuing from the row above
    for y in 0..bmp.height {
        let mut runs: Vec<(i16, u16)> = Vec::new();
        let mut x = 0;
        while x < bmp.width {
            if bmp.alpha(x, y) >= threshold {
                let start = x;
                while x < bmp.width && bmp.alpha(x, y) >= threshold {
                    x += 1;
                }
                runs.push((start as i16, (x - start) as u16));
            } else {
                x += 1;
            }
        }
        let mut next_open = Vec::new();
        for (rx, rw) in &runs {
            if let Some(pos) = open.iter().position(|(ox, ow, _)| ox == rx && ow == rw) {
                next_open.push(open.remove(pos));
            } else {
                next_open.push((*rx, *rw, y as i16));
            }
        }
        for (ox, ow, oy0) in open.drain(..) {
            out.push((ox, oy0, ow, (y as i16 - oy0) as u16));
        }
        open = next_open;
    }
    for (ox, ow, oy0) in open {
        out.push((ox, oy0, ow, (bmp.height as i16 - oy0) as u16));
    }
    out
}

/// The window's colour pixels as opaque BGRX (alpha dropped, premultiplication undone), for a
/// server that shapes the window with [`mask_rects`] instead of blending it.
#[must_use]
pub fn opaque_bgrx(bmp: &Bitmap) -> Vec<u8> {
    let mut out = bmp.bgra.clone();
    for px in out.as_chunks_mut::<4>().0 {
        let a = u32::from(px[3]);
        if a > 0 && a < 255 {
            for c in &mut px[..3] {
                *c = u8::try_from(((u32::from(*c) * 255 + a / 2) / a).min(255)).unwrap_or(u8::MAX);
            }
        }
        px[3] = 0;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_canvas_is_even_and_grows_with_dpi() {
        assert_eq!(canvas_size(1.0), 72);
        assert_eq!(canvas_size(1.5), 108);
        assert_eq!(canvas_size(f64::NAN), 72);
        assert_eq!(canvas_size(100.0), 288);
    }

    #[test]
    fn the_arrow_tip_is_at_the_centre_and_the_arrow_points_down_and_right() {
        let a = render_arrow(1.0);
        assert_eq!(a.bgra.len(), 72 * 72 * 4);
        // The tip pixel and a pixel inside the arrow body are drawn.
        assert!(a.alpha(36, 36) > 0, "tip");
        assert!(a.alpha(38, 44) > 0, "body");
        // Up and to the left of the tip, and far away, is empty.
        assert_eq!(a.alpha(30, 30), 0);
        assert_eq!(a.alpha(0, 0), 0);
        assert_eq!(a.alpha(71, 0), 0);
    }

    #[test]
    fn the_arrow_has_an_orange_fill_and_a_white_outline() {
        let a = render_arrow(1.0);
        // Deep inside the body: fill colour (B,G,R = 0,6A,FF), fully opaque.
        let i = ((46 * 72 + 37) as usize) * 4;
        assert_eq!(a.bgra[i + 3], 255);
        assert_eq!(&a.bgra[i..i + 3], &FILL);
        // Somewhere there is a (nearly) white opaque outline pixel.
        let white = a
            .bgra
            .as_chunks::<4>()
            .0
            .iter()
            .any(|p| p[3] == 255 && p[0] > 240 && p[1] > 240 && p[2] > 240);
        assert!(white);
    }

    #[test]
    fn premultiplied_colour_never_exceeds_alpha() {
        let a = render_arrow(1.5);
        assert!(
            a.bgra
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| p[..3].iter().all(|c| *c <= p[3]))
        );
        let c = compose(&a, 1.5, Some(3));
        assert!(
            c.bgra
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| p[..3].iter().all(|c| *c <= p[3]))
        );
    }

    #[test]
    fn a_ripple_frame_adds_a_ring_around_the_tip_and_grows() {
        let a = render_arrow(1.0);
        let early = compose(&a, 1.0, Some(0));
        let late = compose(&a, 1.0, Some(RIPPLE_FRAMES - 1));
        // Frame 0: ring radius ~6 to the upper left of the tip; frame last: radius ~32.
        assert!(early.alpha(36 - 6, 36 - 1) > 0);
        assert_eq!(a.alpha(36 - 6, 36 - 1), 0, "not part of the arrow");
        assert!(late.alpha(36 - 32, 36 - 1) > 0);
        assert_eq!(early.alpha(36 - 32, 36 - 1), 0);
        // No ripple: just the arrow.
        assert_eq!(compose(&a, 1.0, None), a);
    }

    #[test]
    fn mask_rectangles_cover_exactly_the_opaque_pixels() {
        let a = render_arrow(1.0);
        let rects = mask_rects(&a, 128);
        assert_ne!(rects.len(), 0);
        let mut covered = vec![false; 72 * 72];
        for rect in &rects {
            let left = usize::from(rect.0.unsigned_abs());
            let top = usize::from(rect.1.unsigned_abs());
            for yy in top..top + usize::from(rect.3) {
                for xx in left..left + usize::from(rect.2) {
                    let i = yy * 72 + xx;
                    assert!(!covered[i], "rectangles overlap");
                    covered[i] = true;
                }
            }
        }
        for y in 0..72 {
            for x in 0..72 {
                assert_eq!(
                    covered[(y * 72 + x) as usize],
                    a.alpha(x, y) >= 128,
                    "({x},{y})"
                );
            }
        }
        // Runs merge: far fewer rectangles than rows times runs.
        assert!(rects.len() < 72 * 3);
    }

    #[test]
    fn opaque_pixels_undo_premultiplication_and_drop_alpha() {
        let b = Bitmap {
            width: 1,
            height: 1,
            bgra: vec![50, 100, 150, 200],
        };
        let o = opaque_bgrx(&b);
        assert_eq!(o, vec![64, 128, 191, 0]);
    }
}
