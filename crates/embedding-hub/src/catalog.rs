//! Dated model catalog (data, not code): dimension, input size and free-tier
//! limits per embedding model. Limits are advisory defaults for the client-side
//! limiter; the owner can override them per provider.

use serde::{Deserialize, Serialize};

use crate::limiter::Limits;

const RAW: &str = include_str!("embedding_catalog.json");

/// One embedding model known to the catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogModel {
    /// `gemini`, `openai` or `local`.
    pub provider: String,
    /// Model id as the provider spells it.
    pub model: String,
    /// Plain-language name.
    pub label: String,
    /// Recommended vector size.
    pub dimensions: usize,
    /// Largest vector size the model can produce.
    pub max_dimensions: usize,
    /// Longest input in tokens.
    pub max_input_tokens: usize,
    /// Task wire format: `gemini001`, `gemini2`, `plain` or `e5`.
    pub format: String,
    /// Free-tier limits, if the provider has a free tier.
    pub free_tier: Option<Limits>,
    /// Where the numbers come from.
    pub source: String,
    /// `YYYY-MM-DD` of the last manual check.
    pub last_reviewed: String,
}

/// A native decision model (for the optional decision helper).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionModel {
    /// Provider id.
    pub provider: String,
    /// Model id.
    pub model: String,
    /// Plain-language name.
    pub label: String,
    /// Free-tier limits.
    pub free_tier: Option<Limits>,
    /// Where the numbers come from.
    pub source: String,
    /// `YYYY-MM-DD` of the last manual check.
    pub last_reviewed: String,
}

/// The whole catalog file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingCatalog {
    /// `YYYY-MM-DD` the file as a whole was reviewed.
    pub last_reviewed: String,
    /// Caveat shown next to the numbers.
    pub note: String,
    /// Embedding models.
    pub embedding_models: Vec<CatalogModel>,
    /// Decision models.
    pub decision_models: Vec<DecisionModel>,
}

impl EmbeddingCatalog {
    /// The catalog compiled into this build.
    ///
    /// # Panics
    /// Never in a released build: the data file is checked by a unit test.
    #[must_use]
    pub fn builtin() -> Self {
        serde_json::from_str(RAW).expect("embedding_catalog.json is valid")
    }

    /// Catalog entry for `provider` + `model`.
    #[must_use]
    pub fn find(&self, provider: &str, model: &str) -> Option<&CatalogModel> {
        self.embedding_models
            .iter()
            .find(|m| m.provider == provider && m.model == model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_catalog_parses_and_every_entry_is_dated() {
        let c = EmbeddingCatalog::builtin();
        assert!(c.find("gemini", "gemini-embedding-001").is_some());
        assert!(c.find("openai", "text-embedding-3-small").is_some());
        for m in &c.embedding_models {
            assert_eq!(m.last_reviewed.len(), 10, "{}", m.model);
            assert!(m.dimensions <= m.max_dimensions, "{}", m.model);
        }
        let d = &c.decision_models[0];
        assert_eq!(d.model, "gemini-decision-1");
        assert_eq!(d.free_tier.unwrap().rpd, Some(10_000));
    }
}
