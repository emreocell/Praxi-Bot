//! CLI provider detection and execution.
//!
//! Reports installed, signed-in coding CLIs (`claude`, `codex`, `cursor`, `opencode`,
//! `gemini`, `agy`).

pub mod adapter;
pub mod detect;
pub mod install;
pub mod levels;
pub mod login;
pub mod models;
pub mod node_runtime;
pub mod opencode;
mod peer;
pub mod registry;
pub mod runtime;
pub mod search;

pub use detect::{
    AuthState, CLI_ENGINES, CliEngineSpec, DetectedCli, Detector, PathEnv, enable_refusal_message,
    engine_spec,
};
pub use levels::{ApprovalLevel, Driver, EngineMode, LevelPlan, Support, level_plan};
