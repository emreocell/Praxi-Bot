//! Persistent agent memory: message history, embeddings, and cross-session context.
//!
//! Stores per-thread conversation state and long-horizon memory in SQLite,
//! with vector-search support for relevant context retrieval.
