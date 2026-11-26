//! Tool registration, discovery, and BM25-based tool search.
//!
//! Maintains the runtime registry of built-in tools, MCP servers, and skills,
//! and exposes a find-tool interface that narrows the visible set for each agent turn.

pub mod browser_mcp;
pub mod error;
pub mod registry;
pub mod spec;
pub mod tool;
pub mod url_guard;

pub use error::ToolError;
pub use registry::ToolRegistry;
// PermissionCategory and Platform are defined in `protocol` and re-exported here
// for convenience.
pub use spec::{ContentClass, Effect, ObservationScope, PermissionCategory, Platform, ToolSpec};
pub use tool::{
    ContentPart, ControlIdentity, ControlKind, ControlTarget, OutputExposure, PaymentApproval,
    SpillRef, StandingGrant, Tool, ToolContext, ToolOutput, ToolWrapper,
};
pub use url_guard::{UrlGuard, url_blocked, with_url_guard};
