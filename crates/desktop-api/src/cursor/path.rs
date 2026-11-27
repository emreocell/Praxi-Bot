//! The path the bot cursor takes from one point to the next. Pure: no clock, no OS.

/// Pixels covered per step on a long move; short moves take fewer steps.
const STEP_PX: f64 = 28.0;

/// Smoothstep: slow start, slow end.
fn ease(t: f64) -> f64 {
    t * t * (3.0 - 2.0 * t)
}

/// The points visited going from `from` to `to`, eased, the last one exactly `to`. Never empty.
/// At most `max_steps` points (at least one), and a move of under a pixel is a single point.
#[must_use]
pub fn glide_path(from: (f64, f64), to: (f64, f64), max_steps: usize) -> Vec<(f64, f64)> {
    let dist = (to.0 - from.0).hypot(to.1 - from.1);
    if !dist.is_finite() || dist < 1.0 || max_steps <= 1 {
        return vec![to];
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let wanted = (dist / STEP_PX).ceil() as usize;
    let steps = wanted.clamp(2, max_steps.max(2));
    let mut out = Vec::with_capacity(steps);
    for i in 1..steps {
        #[allow(clippy::cast_precision_loss)]
        let t = ease(i as f64 / steps as f64);
        out.push((from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t));
    }
    out.push(to);
    out
}

/// Where the cursor first appears when it has no previous point: a little up and to the left
/// of the target (never off the left or top edge), so it glides in instead of popping up.
#[must_use]
pub fn entry_point(to: (f64, f64)) -> (f64, f64) {
    ((to.0 - 120.0).max(0.0), (to.1 - 70.0).max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_ends_exactly_on_the_target_and_is_bounded() {
        let p = glide_path((0.0, 0.0), (900.0, 300.0), 16);
        assert_eq!(p.last().copied(), Some((900.0, 300.0)));
        assert!(p.len() <= 16 && p.len() >= 2);
        let p = glide_path((0.0, 0.0), (90_000.0, 0.0), 12);
        assert_eq!(p.len(), 12);
    }

    #[test]
    fn a_path_is_eased_and_monotonic_along_a_straight_line() {
        let p = glide_path((0.0, 0.0), (600.0, 0.0), 20);
        assert!(p.windows(2).all(|w| w[1].0 >= w[0].0));
        // Slow start: the first step is shorter than a middle step.
        let first = p[0].0;
        let mid = p[p.len() / 2].0 - p[p.len() / 2 - 1].0;
        assert!(first < mid, "{first} < {mid}");
    }

    #[test]
    fn tiny_unmoved_or_invalid_moves_are_one_point() {
        assert_eq!(glide_path((5.0, 5.0), (5.4, 5.0), 10), vec![(5.4, 5.0)]);
        assert_eq!(glide_path((0.0, 0.0), (500.0, 0.0), 1), vec![(500.0, 0.0)]);
        assert_eq!(
            glide_path((f64::NAN, 0.0), (7.0, 7.0), 10),
            vec![(7.0, 7.0)]
        );
    }

    #[test]
    fn the_entry_point_stays_on_screen() {
        assert_eq!(entry_point((500.0, 400.0)), (380.0, 330.0));
        assert_eq!(entry_point((10.0, 10.0)), (0.0, 0.0));
    }
}
