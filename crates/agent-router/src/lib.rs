//! Layered decision/routing engine that maps user intents to tools, skills, and agents.
//!
//! Implements the K0–K4 decision cascade: rule-based, intent-cache, local
//! embedding+BM25, fast classifier, and full LLM planning.
//!
//! Currently implemented: BM25 tool discovery ([`discovery::ToolIndex`]) and the
//! model-facing tool surface ([`surface::ToolSurface`]) that switches between
//! direct tools and `find_tool` / `get_tool_details` / `use_tool` meta-tools.

mod bm25;
pub mod discovery;
pub mod surface;

pub use discovery::{ToolHit, ToolIndex};
pub use surface::{
    DEFAULT_DISCOVERY_THRESHOLD, FIND_TOOL, GET_TOOL_DETAILS, SurfaceMode, ToolSurface,
    ToolVisibility, USE_TOOL,
};
