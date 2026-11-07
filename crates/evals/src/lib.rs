//! Evaluation harness (task 019).
//!
//! Every change to the agent loop, tools or prompts can be measured here.
//!
//! * **Tier 1** ([`scenario`], [`tier1`]): hermetic YAML scenarios.  A scripted
//!   fake provider replays a list of model responses against fake tools; the
//!   run is checked for the tool-call sequence, permission decisions, final
//!   text, run state and step count.  Runs in CI (`cargo test`).
//! * **Tier 2** ([`task`], [`tier2`]): live YAML tasks.  A real provider drives
//!   the real browser / desktop, with a setup step and deterministic checks or
//!   an LLM judge.  Opt-in via `BOT_EVAL_LIVE=1`; refused when `CI` is set.
//! * **Metrics** ([`metrics`]): one JSONL record per run; [`report`] renders a
//!   markdown before/after comparison of two result files.
//!
//! Everything goes through the public [`agent_core::AgentLoop`] API.

pub mod cli;
pub mod fake;
pub mod harness;
pub mod live;
pub mod metrics;
pub mod report;
pub mod scenario;
pub mod task;
pub mod tier1;
pub mod tier2;

/// Error type shared by the harness.
pub type Error = Box<dyn std::error::Error + Send + Sync>;
/// Result alias.
pub type Result<T> = std::result::Result<T, Error>;
