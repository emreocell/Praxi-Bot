//! Permission-related types shared across crates.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Broad permission category for a tool or action.
///
/// Each category is independently controllable in a policy.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum PermissionCategory {
    /// File-system reads and writes.
    Fs,
    /// Shell and command execution.
    Shell,
    /// Desktop automation (windows, clicks, keyboard).
    Desktop,
    /// Browser navigation, form submission, downloads.
    Browser,
    /// Connector accounts (read, send, share).
    Accounts,
    /// Messaging (e-mail, chat).
    Messaging,
    /// Financial operations (payments, purchases, subscriptions).
    Money,
    /// System settings and software installation.
    System,
    /// Remote computer access via node-agent.
    Remote,
    /// Script or code execution.
    Script,
}

/// Target operating system platforms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Windows,
    MacOs,
    Linux,
    /// Available on all platforms (default when the field is omitted).
    All,
}
