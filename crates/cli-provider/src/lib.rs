//! CLI provider detection and execution.
//!
//! Reports installed, signed-in coding CLIs (`claude`, `codex`, `cursor`, `opencode`,
//! `gemini`, `chatgpt-plan`).

pub mod detect;
mod peer;
pub mod runtime;

pub use detect::{
    AuthState, CLI_ENGINES, CliEngineSpec, DetectedCli, Detector, PathEnv, enable_refusal_message,
    engine_spec,
};
