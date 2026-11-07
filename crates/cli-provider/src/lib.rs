//! CLI provider detection and execution.
//!
//! Reports installed, signed-in coding CLIs (`claude`, `codex`, `cursor`, `opencode`,
//! `gemini`, `chatgpt-plan`).

pub mod detect;
pub mod runtime;

pub use detect::{AuthState, DetectedCli, Detector, PathEnv};
