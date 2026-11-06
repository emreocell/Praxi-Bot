//! Strongly-typed `UUIDv7` identifier newtypes.
//!
//! Each newtype wraps [`uuid::Uuid`] and is opaque at the type level, preventing
//! accidental mixing of e.g. a [`UserId`] where a [`RunId`] is expected.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! uuid_newtype {
    ($($(#[$attr:meta])* $name:ident),* $(,)?) => {
        $(
            $(#[$attr])*
            #[derive(
                Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash,
                Serialize, Deserialize, JsonSchema,
            )]
            #[serde(transparent)]
            pub struct $name(pub Uuid);

            impl $name {
                /// Generate a new random identifier using the `UUIDv7` algorithm.
                #[must_use]
                pub fn new() -> Self {
                    Self(Uuid::now_v7())
                }

                /// Return the inner [`Uuid`].
                #[must_use]
                pub fn into_inner(self) -> Uuid {
                    self.0
                }
            }

            impl Default for $name {
                fn default() -> Self {
                    Self::new()
                }
            }

            impl std::fmt::Display for $name {
                fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    self.0.fmt(f)
                }
            }
        )*
    };
}

uuid_newtype!(
    /// Identifies a human user account.
    UserId,
    /// Identifies a persistent bot profile.
    BotId,
    /// Identifies a chat session / thread.
    ChatId,
    /// Identifies a single agent run (task execution).
    RunId,
    /// Identifies a node-agent instance (a controlled computer).
    NodeId,
    /// Identifies a physical or virtual device.
    DeviceId,
    /// Identifies a scheduled or recurring task.
    TaskId,
    /// Identifies a single tool-call invocation within a run.
    ToolCallId,
);

/// A reference to an external messaging channel (e.g. a Telegram chat or e-mail address).
///
/// The string is `"<kind>:<address>"`, e.g. `"telegram:12345678"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct ChannelRef(pub String);

impl ChannelRef {
    /// Construct a channel reference from its kind and address.
    pub fn new(kind: impl Into<String>, address: impl Into<String>) -> Self {
        let kind = kind.into();
        let address = address.into();
        Self(format!("{kind}:{address}"))
    }
}

impl std::fmt::Display for ChannelRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_id_round_trips_through_json() {
        let id = UserId::new();
        let json = serde_json::to_string(&id).unwrap();
        let back: UserId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn distinct_newtypes_do_not_unify() {
        // Compile-time check: UserId and BotId cannot be assigned to each other.
        // This test just exercises construction to confirm the macro output is sane.
        let _user = UserId::new();
        let _bot = BotId::new();
        let _run = RunId::new();
    }

    #[test]
    fn channel_ref_format() {
        let r = ChannelRef::new("telegram", "12345678");
        assert_eq!(r.to_string(), "telegram:12345678");
    }
}
