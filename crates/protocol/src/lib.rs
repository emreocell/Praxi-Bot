//! Shared protocol types for the node-agent wire format and client API.
//!
//! Defines serde-annotated types shared by the core daemon, node-agents, and
//! all client interfaces.  Every public type derives `JsonSchema` so callers
//! can generate and snapshot JSON Schema documents.

pub mod envelope;
pub mod event;
pub mod ids;
pub mod initiator;
pub mod permission;
mod schema_snapshot;

pub use envelope::Envelope;
pub use event::Event;
pub use ids::{BotId, ChannelRef, ChatId, DeviceId, NodeId, RunId, TaskId, ToolCallId, UserId};
pub use initiator::{Initiator, InitiatorKind};
pub use permission::{PermissionCategory, Platform};
