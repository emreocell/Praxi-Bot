//! Versioned event envelope for the client SSE / WebSocket stream.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::event::Event;
use crate::ids::RunId;

/// A versioned, sequenced wrapper around a single [`Event`].
///
/// Clients use `seq` to detect gaps and `v` to handle future schema changes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Envelope {
    /// Schema version.  Currently always `1`.
    pub v: u16,
    /// Monotonically increasing sequence number within a run.
    pub seq: u64,
    /// Unix epoch milliseconds when the event was emitted (from [`std::time::SystemTime`]).
    pub ts_ms: u64,
    /// The event payload.
    pub event: Event,
    /// Set on every event emitted by a subagent (child) run: the run that
    /// spawned it.  Absent on top-level runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<RunId>,
}

impl Envelope {
    /// Wrap an event with the given sequence number, using the current wall-clock time.
    ///
    /// `ts_ms` is set from [`std::time::SystemTime::now`] relative to
    /// [`std::time::UNIX_EPOCH`].  On platforms where the system clock
    /// pre-dates the Unix epoch the field falls back to `0`.
    #[must_use]
    pub fn new(seq: u64, event: Event) -> Self {
        let ts_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        Self {
            v: 1,
            seq,
            ts_ms,
            event,
            parent_run_id: None,
        }
    }

    /// Mark the envelope as coming from a child run of `parent`.
    #[must_use]
    pub fn with_parent(mut self, parent: Option<RunId>) -> Self {
        self.parent_run_id = parent;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        event::{Event, MessageDelta},
        ids::RunId,
    };

    #[test]
    fn envelope_round_trips() {
        let ev = Event::MessageDelta(MessageDelta {
            run_id: RunId::new(),
            text: "hi".into(),
        });
        let env = Envelope::new(42, ev);
        let json = serde_json::to_string(&env).unwrap();
        let back: Envelope = serde_json::from_str(&json).unwrap();
        assert_eq!(env.seq, back.seq);
        assert_eq!(env.v, back.v);
        assert_eq!(env.ts_ms, back.ts_ms);
    }

    #[test]
    fn envelope_v_is_1() {
        let ev = Event::MessageDelta(MessageDelta {
            run_id: RunId::new(),
            text: "x".into(),
        });
        let env = Envelope::new(0, ev);
        assert_eq!(env.v, 1);
    }

    #[test]
    fn envelope_ts_ms_is_nonzero() {
        let ev = Event::MessageDelta(MessageDelta {
            run_id: RunId::new(),
            text: "y".into(),
        });
        let env = Envelope::new(1, ev);
        // As long as we're past the Unix epoch (which we are), ts_ms should be nonzero.
        assert!(env.ts_ms > 0);
    }
}
