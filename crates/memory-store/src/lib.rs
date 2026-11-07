//! Persistent agent memory (ADR-0017): storage, hybrid retrieval and an
//! extraction pipeline.
//!
//! * [`MemoryStore`] owns `<data_dir>/memory.db` (separate from `storage`'s
//!   `bot.db`, with its own migration set: the sqlite-vec extension must be
//!   registered on the connection and `storage` stays free of `unsafe`).
//! * [`MemoryStore::recall`] fuses BM25 (FTS5) and vector search with RRF,
//!   applies a recency/importance boost and cuts to a token budget.
//! * [`render_recall_block`] builds the block that is prepended to the *user
//!   message* (never the system prompt, to keep the prompt cache valid).
//! * [`Pipeline`] runs extraction + consolidation through the [`MemoryModel`]
//!   trait and enforces the safety rules in code.

mod error;
mod manage;
mod pipeline;
mod recall;
mod render;
mod secrets;
mod sessions;
mod store;
mod types;
mod vec;

pub mod embed;
pub mod prompts;

#[cfg(feature = "fastembed")]
pub use embed::FastEmbedder;
pub use embed::{Embedder, HashEmbedder};
pub use error::{MemoryError, Result};
pub use pipeline::{
    Candidate, Decision, ExtractionContext, MemoryModel, Outcome, Pipeline, RejectReason,
    looks_like_question_or_hypothetical,
};
pub use recall::{RecallConfig, estimate_tokens, rrf_fuse};
pub use render::{prepend_to_user_message, render_recall_block};
pub use secrets::{contains_secret, redact};
pub use sessions::SessionHit;
pub use store::MemoryStore;
pub use types::{JournalEntry, JournalOp, Kind, ListFilter, Memory, MemoryPatch, NewMemory, Trust};
