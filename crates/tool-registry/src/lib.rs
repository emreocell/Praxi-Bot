//! Tool registration, discovery, and BM25-based tool search.
//!
//! Maintains the runtime registry of built-in tools, MCP servers, and skills,
//! and exposes a find-tool interface that narrows the visible set for each agent turn.

pub mod error;
pub mod registry;
pub mod spec;
pub mod tool;

pub use error::ToolError;
pub use registry::ToolRegistry;
// PermissionCategory and Platform are defined in `protocol` and re-exported here
// for convenience.
pub use spec::{ContentClass, Effect, PermissionCategory, Platform, ToolSpec};
pub use tool::{ContentPart, SpillRef, Tool, ToolContext, ToolOutput, ToolWrapper};
