//! Who or what started an agent run.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ids::{ChannelRef, RunId, TaskId, UserId};

/// The kind of initiator, without the associated identifier payload.
///
/// Used in permission rules and policy resolution where only the category
/// of initiator matters, not the specific identity.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum InitiatorKind {
    /// A human user initiated the run interactively.
    Human,
    /// A scheduled or recurring task initiated the run.
    Routine,
    /// An external messaging channel triggered the run.
    Channel,
    /// A parent agent run spawned this as a sub-agent.
    Subagent,
}

/// The entity that initiated an agent run, including the specific identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum Initiator {
    /// A human user initiated the run interactively.
    Human(UserId),
    /// A scheduled or recurring task initiated the run.
    Routine(TaskId),
    /// An external messaging channel (e.g. Telegram, e-mail) triggered the run.
    Channel(ChannelRef),
    /// A parent agent run spawned this as a sub-agent.
    Subagent(RunId),
}

impl Initiator {
    /// Return the [`InitiatorKind`] without the identifier payload.
    #[must_use]
    pub fn kind(&self) -> InitiatorKind {
        match self {
            Self::Human(_) => InitiatorKind::Human,
            Self::Routine(_) => InitiatorKind::Routine,
            Self::Channel(_) => InitiatorKind::Channel,
            Self::Subagent(_) => InitiatorKind::Subagent,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::UserId;

    #[test]
    fn initiator_serialises_with_kind_tag() {
        let uid = UserId::new();
        let ini = Initiator::Human(uid);
        let json = serde_json::to_value(&ini).unwrap();
        assert_eq!(json["kind"], "human");
    }

    #[test]
    fn initiator_round_trips() {
        let cases: Vec<Initiator> = vec![
            Initiator::Human(UserId::new()),
            Initiator::Routine(TaskId::new()),
            Initiator::Channel(ChannelRef::new("telegram", "123")),
            Initiator::Subagent(RunId::new()),
        ];
        for ini in cases {
            let json = serde_json::to_string(&ini).unwrap();
            let back: Initiator = serde_json::from_str(&json).unwrap();
            assert_eq!(ini, back);
        }
    }
}
