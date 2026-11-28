//! Window management: what can be done to a top-level window besides focusing it.

use serde::{Deserialize, Serialize};

/// Largest window edge a move or resize may ask for, in pixels.
pub const MAX_WINDOW_EDGE: i32 = 16_384;

/// An operation on one top-level window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum WindowOp {
    Minimize,
    Maximize,
    /// Undo a minimise or maximise.
    Restore,
    /// Move the window's top-left corner to a screen point.
    Move {
        x: i32,
        y: i32,
    },
    /// Resize the window (outer size, in pixels).
    Resize {
        width: u32,
        height: u32,
    },
    /// Ask the program to close the window, like its close button. The
    /// program may still keep it open (an unsaved-changes dialog).
    Close,
}

impl WindowOp {
    /// A short name for cards, logs and results.
    #[must_use]
    pub fn label(self) -> String {
        match self {
            Self::Minimize => "minimise".into(),
            Self::Maximize => "maximise".into(),
            Self::Restore => "restore".into(),
            Self::Move { x, y } => format!("move to ({x}, {y})"),
            Self::Resize { width, height } => format!("resize to {width}x{height}"),
            Self::Close => "close".into(),
        }
    }

    /// Whether the numbers are inside what a sane desktop can show.
    ///
    /// # Errors
    /// Why the operation was refused.
    pub fn validate(self) -> Result<(), String> {
        match self {
            Self::Move { x, y } if x.abs() > MAX_WINDOW_EDGE || y.abs() > MAX_WINDOW_EDGE => Err(
                format!("a window position must be within ±{MAX_WINDOW_EDGE} pixels"),
            ),
            Self::Resize { width, height }
                if !(100..=MAX_WINDOW_EDGE.unsigned_abs()).contains(&width)
                    || !(50..=MAX_WINDOW_EDGE.unsigned_abs()).contains(&height) =>
            {
                Err(format!(
                    "a window size must be 100..={MAX_WINDOW_EDGE} wide and 50..={MAX_WINDOW_EDGE} high"
                ))
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_positions_are_bounded() {
        assert!(
            WindowOp::Resize {
                width: 800,
                height: 600
            }
            .validate()
            .is_ok()
        );
        assert!(
            WindowOp::Resize {
                width: 10,
                height: 600
            }
            .validate()
            .is_err()
        );
        assert!(
            WindowOp::Resize {
                width: 800,
                height: 99_999
            }
            .validate()
            .is_err()
        );
        assert!(WindowOp::Move { x: -1200, y: 40 }.validate().is_ok());
        assert!(WindowOp::Move { x: 1_000_000, y: 0 }.validate().is_err());
        assert!(WindowOp::Close.validate().is_ok());
    }

    #[test]
    fn ops_round_trip_through_json() {
        for op in [
            WindowOp::Minimize,
            WindowOp::Move { x: 1, y: 2 },
            WindowOp::Resize {
                width: 300,
                height: 200,
            },
            WindowOp::Close,
        ] {
            let v = serde_json::to_value(op).unwrap();
            assert_eq!(serde_json::from_value::<WindowOp>(v).unwrap(), op);
        }
    }
}
