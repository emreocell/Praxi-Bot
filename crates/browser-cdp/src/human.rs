//! Natural-looking pointer movement, scrolling and pauses.
//!
//! This exists so that a person watching the live view can follow what the assistant does
//! (the pointer glides, the page scrolls instead of jumping) and so that pages that only show
//! a menu while the pointer is over it have time to react. It is NOT a way to avoid bot
//! detection and makes no such claim: the browser still announces itself as automated.
//!
//! Everything here is pure (no browser, no clock): the actions in [`crate::actions`] take the
//! plans and pauses computed here and carry them out.

use std::str::FromStr;
use std::time::Duration;

/// The configured setting (`human_like` in the browser settings).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HumanLike {
    /// Instant actions, no pauses.
    Off,
    /// Natural movement while somebody can watch (a visible window or an open live view),
    /// instant actions otherwise. The default.
    #[default]
    Auto,
    /// Slower, more deliberate movement always (for demos and recordings).
    High,
}

impl HumanLike {
    /// Settings name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Auto => "auto",
            Self::High => "high",
        }
    }

    /// How an action should feel right now. `watched`: a person can see the page.
    #[must_use]
    pub fn feel(self, watched: bool) -> Feel {
        match self {
            Self::Auto if watched => Feel::Natural,
            Self::Off | Self::Auto => Feel::Instant,
            Self::High => Feel::Deliberate,
        }
    }
}

impl FromStr for HumanLike {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" | "false" | "no" | "0" => Ok(Self::Off),
            "auto" | "" => Ok(Self::Auto),
            "high" | "on" | "true" | "yes" | "1" => Ok(Self::High),
            other => Err(format!(
                "human_like must be off, auto or high (got {other:?})"
            )),
        }
    }
}

/// How one action is carried out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feel {
    /// One event, no pauses.
    Instant,
    /// A short eased path and small pauses.
    Natural,
    /// A longer path and longer pauses.
    Deliberate,
}

impl Feel {
    /// A scroll `speed` the model asked for, if it is one of the known names.
    #[must_use]
    pub fn from_speed(speed: &str) -> Option<Self> {
        match speed.trim().to_ascii_lowercase().as_str() {
            "instant" | "fast" => Some(Self::Instant),
            "smooth" | "normal" => Some(Self::Natural),
            "slow" => Some(Self::Deliberate),
            _ => None,
        }
    }

    fn is_instant(self) -> bool {
        self == Self::Instant
    }
}

/// A tiny xorshift generator: the delays only need to be uneven, never unpredictable,
/// and a fixed seed makes the plans testable.
#[derive(Debug, Clone)]
pub struct Jitter(u64);

impl Jitter {
    /// A generator with a fixed seed.
    #[must_use]
    pub fn seeded(seed: u64) -> Self {
        Self(seed.max(1))
    }

    /// A generator seeded from the clock.
    #[must_use]
    pub fn from_clock() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0x9E37_79B9, |d| d.as_nanos() as u64);
        Self::seeded(nanos ^ 0xD1B5_4A32_D192_ED03)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// Uniform in `[0, 1)`.
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform in `[lo, hi]` milliseconds.
    fn millis(&mut self, lo: u64, hi: u64) -> Duration {
        let span = hi.saturating_sub(lo);
        Duration::from_millis(lo + (self.unit() * (span as f64 + 1.0)) as u64)
    }
}

/// Ease in and out (smoothstep).
fn ease(t: f64) -> f64 {
    t * t * (3.0 - 2.0 * t)
}

/// The points the pointer visits from `from` to `to`, the last one being exactly `to`.
/// A few pixels of sideways drift make the path a gentle curve instead of a ruler line.
#[must_use]
pub fn pointer_path(
    from: (f64, f64),
    to: (f64, f64),
    feel: Feel,
    rng: &mut Jitter,
) -> Vec<(f64, f64)> {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let dist = dx.hypot(dy);
    if feel.is_instant() || dist < 3.0 {
        return vec![to];
    }
    let (per, lo, hi) = match feel {
        Feel::Deliberate => (28.0, 8, 30),
        _ => (45.0, 4, 16),
    };
    let steps = ((dist / per).ceil() as usize).clamp(lo, hi);
    // Unit normal of the line, and how far the middle of the path bows away from it.
    let (nx, ny) = (-dy / dist, dx / dist);
    let bow = ((rng.unit() * 2.0 - 1.0) * dist * 0.05).clamp(-14.0, 14.0);
    let mut path = Vec::with_capacity(steps);
    for i in 1..steps {
        let t = i as f64 / steps as f64;
        let e = ease(t);
        let side = (std::f64::consts::PI * t).sin() * bow;
        path.push((from.0 + dx * e + nx * side, from.1 + dy * e + ny * side));
    }
    path.push(to);
    path
}

/// Pause between two pointer moves of one path.
#[must_use]
pub fn step_pause(feel: Feel, rng: &mut Jitter) -> Duration {
    match feel {
        Feel::Instant => Duration::ZERO,
        Feel::Natural => rng.millis(6, 14),
        Feel::Deliberate => rng.millis(10, 24),
    }
}

/// The beat between arriving over a target and pressing on it.
#[must_use]
pub fn settle_pause(feel: Feel, rng: &mut Jitter) -> Duration {
    match feel {
        Feel::Instant => Duration::ZERO,
        Feel::Natural => rng.millis(40, 110),
        Feel::Deliberate => rng.millis(120, 300),
    }
}

/// How long a press is held before it is released.
#[must_use]
pub fn hold_pause(feel: Feel, rng: &mut Jitter) -> Duration {
    match feel {
        Feel::Instant => Duration::ZERO,
        Feel::Natural => rng.millis(30, 80),
        Feel::Deliberate => rng.millis(60, 140),
    }
}

/// How long the pointer rests over an element a hover is meant to reveal something on
/// (menus open on a transition, often with a short delay).
#[must_use]
pub fn hover_dwell(feel: Feel, rng: &mut Jitter) -> Duration {
    match feel {
        Feel::Instant => rng.millis(120, 180),
        Feel::Natural => rng.millis(280, 420),
        Feel::Deliberate => rng.millis(550, 800),
    }
}

/// A short beat before a fill or a key press, like a person getting ready to type.
#[must_use]
pub fn prepare_pause(feel: Feel, rng: &mut Jitter) -> Duration {
    match feel {
        Feel::Instant => Duration::ZERO,
        Feel::Natural => rng.millis(50, 150),
        Feel::Deliberate => rng.millis(150, 400),
    }
}

/// One wheel step of a scroll and the pause after it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollStep {
    /// Pixels (positive: down / right).
    pub delta: f64,
    /// Pause after the step.
    pub pause: Duration,
}

/// Most wheel steps one scroll takes, however long the way is (the steps grow instead).
const MAX_SCROLL_STEPS: usize = 40;

/// Split a scroll of `total` pixels into wheel steps that start slowly, speed up and slow down
/// again, with a longer "looking at it" pause every few steps. The deltas add up to `total`.
#[must_use]
pub fn scroll_plan(total: f64, feel: Feel, rng: &mut Jitter) -> Vec<ScrollStep> {
    if total == 0.0 {
        return Vec::new();
    }
    if feel.is_instant() {
        return vec![ScrollStep {
            delta: total,
            pause: Duration::ZERO,
        }];
    }
    let (chunk, read_every) = match feel {
        Feel::Deliberate => (70.0, 3),
        _ => (110.0, 5),
    };
    let n = ((total.abs() / chunk).ceil() as usize).clamp(1, MAX_SCROLL_STEPS);
    let weights: Vec<f64> = (0..n)
        .map(|i| (std::f64::consts::PI * (i as f64 + 0.5) / n as f64).sin() + 0.2)
        .collect();
    let sum: f64 = weights.iter().sum();
    let mut steps = Vec::with_capacity(n);
    let mut given = 0.0;
    for (i, w) in weights.iter().enumerate() {
        let delta = if i + 1 == n {
            total - given
        } else {
            (total * w / sum).round()
        };
        given += delta;
        let pause = if i + 1 == n {
            Duration::ZERO
        } else if (i + 1) % read_every == 0 {
            match feel {
                Feel::Deliberate => rng.millis(250, 500),
                _ => rng.millis(120, 260),
            }
        } else {
            match feel {
                Feel::Deliberate => rng.millis(20, 40),
                _ => rng.millis(14, 28),
            }
        };
        steps.push(ScrollStep { delta, pause });
    }
    steps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setting_parses_and_defaults_to_auto() {
        assert_eq!(HumanLike::default(), HumanLike::Auto);
        assert_eq!("off".parse(), Ok(HumanLike::Off));
        assert_eq!(" HIGH ".parse(), Ok(HumanLike::High));
        assert!("sometimes".parse::<HumanLike>().is_err());
    }

    #[test]
    fn auto_is_natural_only_when_somebody_can_watch() {
        assert_eq!(HumanLike::Auto.feel(false), Feel::Instant);
        assert_eq!(HumanLike::Auto.feel(true), Feel::Natural);
        assert_eq!(HumanLike::Off.feel(true), Feel::Instant);
        assert_eq!(HumanLike::High.feel(false), Feel::Deliberate);
    }

    #[test]
    fn pointer_path_is_eased_ends_exactly_on_target_and_is_instant_when_asked() {
        let mut rng = Jitter::seeded(7);
        let path = pointer_path((0.0, 0.0), (400.0, 200.0), Feel::Natural, &mut rng);
        assert!(path.len() >= 4, "{path:?}");
        assert_eq!(*path.last().unwrap(), (400.0, 200.0));
        // Slow start: the first hop is shorter than the middle one.
        let hop = |a: (f64, f64), b: (f64, f64)| (b.0 - a.0).hypot(b.1 - a.1);
        let first = hop((0.0, 0.0), path[0]);
        let mid = hop(path[path.len() / 2 - 1], path[path.len() / 2]);
        assert!(first < mid, "first {first} mid {mid}");
        assert_eq!(
            pointer_path((0.0, 0.0), (400.0, 200.0), Feel::Instant, &mut rng),
            vec![(400.0, 200.0)]
        );
    }

    #[test]
    fn scroll_plan_adds_up_and_stays_bounded() {
        let mut rng = Jitter::seeded(11);
        for total in [50.0, 900.0, -2400.0, 250_000.0] {
            for feel in [Feel::Natural, Feel::Deliberate] {
                let plan = scroll_plan(total, feel, &mut rng);
                let sum: f64 = plan.iter().map(|s| s.delta).sum();
                assert!((sum - total).abs() < 0.5, "{total}: {sum}");
                assert!(plan.len() <= MAX_SCROLL_STEPS);
                assert!(plan.iter().all(|s| s.delta.signum() == total.signum()));
                let time: Duration = plan.iter().map(|s| s.pause).sum();
                assert!(time < Duration::from_secs(6), "{total} {feel:?}: {time:?}");
            }
        }
        let one = scroll_plan(900.0, Feel::Instant, &mut rng);
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].delta, 900.0);
        assert_eq!(scroll_plan(0.0, Feel::Natural, &mut rng).len(), 0);
    }

    #[test]
    fn pauses_are_zero_when_instant_and_uneven_otherwise() {
        let mut rng = Jitter::seeded(3);
        assert_eq!(settle_pause(Feel::Instant, &mut rng), Duration::ZERO);
        let a: Vec<_> = (0..8)
            .map(|_| settle_pause(Feel::Natural, &mut rng))
            .collect();
        assert!(
            a.iter()
                .all(|d| *d >= Duration::from_millis(40) && *d <= Duration::from_millis(110))
        );
        assert!(a.windows(2).any(|w| w[0] != w[1]), "{a:?}");
        // A hover always rests long enough for a menu to open, even when instant.
        assert!(hover_dwell(Feel::Instant, &mut rng) >= Duration::from_millis(120));
    }

    #[test]
    fn speed_names() {
        assert_eq!(Feel::from_speed("slow"), Some(Feel::Deliberate));
        assert_eq!(Feel::from_speed("Smooth"), Some(Feel::Natural));
        assert_eq!(Feel::from_speed("instant"), Some(Feel::Instant));
        assert_eq!(Feel::from_speed("warp"), None);
    }
}
