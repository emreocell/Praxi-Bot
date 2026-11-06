//! Layered decision/routing engine that maps user intents to tools, skills, and agents.
//!
//! Implements the K0–K4 decision cascade: rule-based, intent-cache, local
//! embedding+BM25, fast classifier, and full LLM planning.
