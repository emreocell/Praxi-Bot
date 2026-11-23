//! Provider-agnostic text embeddings for the router (K2), memory recall and
//! the knowledge index.
//!
//! * [`config::EmbeddingConfig`]: one configuration, no secrets in it.
//! * [`catalog::EmbeddingCatalog`]: models, dimensions and free-tier limits as dated data.
//! * [`limiter::RateLimiter`]: keeps a free key under its RPM / TPM / RPD limits.
//! * [`hub::EmbeddingHub`]: fallback chain per purpose with the privacy gate.
//!
//! Default (no providers configured): the offline hash embedder, nothing is
//! sent anywhere.

pub mod catalog;
pub mod config;
pub mod hub;
pub mod limiter;

#[cfg(test)]
mod tests;

pub use config::{EmbeddingConfig, ProviderConfig, ProviderType, Purpose, VectorSpace};
pub use hub::{
    Attempt, EmbedItem, Embedded, EmbeddingHub, LocalEmbedder, Outcome, ProviderStatus, Task,
    Unserved,
};
