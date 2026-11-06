//! Run and step state machines.

use std::fmt;

/// Lifecycle state of a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RunState {
    /// Created, not started.
    Queued,
    /// Actively executing steps.
    Running,
    /// Blocked on a user approval.
    WaitingApproval,
    /// Stopped (e.g. after a crash) until an explicit resume.
    Paused,
    /// Finished successfully (terminal).
    Succeeded,
    /// Finished with an error (terminal).
    Failed,
    /// Cancelled (terminal).
    Cancelled,
}

impl RunState {
    /// All states, in declaration order.
    pub const ALL: [Self; 7] = [
        Self::Queued,
        Self::Running,
        Self::WaitingApproval,
        Self::Paused,
        Self::Succeeded,
        Self::Failed,
        Self::Cancelled,
    ];

    /// Stable string stored in the database.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::WaitingApproval => "waiting_approval",
            Self::Paused => "paused",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Parse the stored string.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }

    /// Whether no further transitions are possible.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

impl fmt::Display for RunState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Pure legality check for a run state transition.
#[must_use]
pub fn can_transition(from: RunState, to: RunState) -> bool {
    use RunState::{Cancelled, Failed, Paused, Queued, Running, Succeeded, WaitingApproval};
    matches!(
        (from, to),
        (Queued, Running | Cancelled)
            | (
                Running,
                WaitingApproval | Paused | Succeeded | Failed | Cancelled
            )
            | (WaitingApproval, Running | Paused | Failed | Cancelled)
            | (Paused, Running | Failed | Cancelled)
    )
}

/// Status of a persisted step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
    /// Intent persisted; result not yet known.
    Intent,
    /// Completed with a result.
    Succeeded,
    /// Failed (last attempt).
    Failed,
    /// Intent without result found after a crash or cancellation.
    Interrupted,
}

impl StepStatus {
    /// Stable string stored in the database.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Intent => "intent",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
        }
    }

    /// Parse the stored string.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        [
            Self::Intent,
            Self::Succeeded,
            Self::Failed,
            Self::Interrupted,
        ]
        .into_iter()
        .find(|v| v.as_str() == s)
    }
}

#[cfg(test)]
mod tests {
    use super::RunState::{Cancelled, Failed, Paused, Queued, Running, Succeeded, WaitingApproval};
    use super::*;

    #[test]
    fn transition_table() {
        // Rows = from, columns = to, in RunState::ALL order:
        // Queued Running Waiting Paused Succeeded Failed Cancelled
        let table: [(RunState, [bool; 7]); 7] = [
            (Queued, [false, true, false, false, false, false, true]),
            (Running, [false, false, true, true, true, true, true]),
            (
                WaitingApproval,
                [false, true, false, true, false, true, true],
            ),
            (Paused, [false, true, false, false, false, true, true]),
            (Succeeded, [false; 7]),
            (Failed, [false; 7]),
            (Cancelled, [false; 7]),
        ];
        for (from, row) in table {
            for (to, expected) in RunState::ALL.into_iter().zip(row) {
                assert_eq!(can_transition(from, to), expected, "{from} -> {to}");
            }
        }
    }

    #[test]
    fn terminal_states_have_no_exits() {
        for s in RunState::ALL.into_iter().filter(|s| s.is_terminal()) {
            assert!(RunState::ALL.into_iter().all(|t| !can_transition(s, t)));
        }
    }

    #[test]
    fn string_round_trip() {
        for s in RunState::ALL {
            assert_eq!(RunState::parse(s.as_str()), Some(s));
        }
        assert_eq!(RunState::parse("nope"), None);
        for s in [
            StepStatus::Intent,
            StepStatus::Succeeded,
            StepStatus::Failed,
            StepStatus::Interrupted,
        ] {
            assert_eq!(StepStatus::parse(s.as_str()), Some(s));
        }
    }
}
