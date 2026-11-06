//! Embedded script interpreter sandbox for code-as-action execution.
//!
//! Allows the agent to write short scripts (Rhai, JS, Python) that are executed
//! in an isolated sandbox, reducing round-trips by batching multiple tool calls.
