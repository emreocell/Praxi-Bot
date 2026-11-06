//! Workflow recording and deterministic replay: captures agent action streams for reuse.
//!
//! Records resolved action sequences as named workflows so they can be
//! replayed without an LLM call, reducing latency and cost for repeated tasks.
