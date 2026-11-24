//! The one embedding configuration used by the router, memory recall and the
//! knowledge index. It holds NO secrets: a provider points at its API key by
//! secret-store name (`key_ref`).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::catalog::EmbeddingCatalog;
use crate::limiter::Limits;

/// Largest vector size the hub accepts.
pub const MAX_DIMENSIONS: usize = 8192;
/// Vector size of the offline hash embedder.
pub const HASH_DIMENSIONS: usize = 384;
/// Id of the always-available offline fallback.
pub const HASH_ID: &str = "offline-hash";

/// Who asks for embeddings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
    /// The decision router (K2).
    Router,
    /// Memory recall.
    Memory,
    /// The knowledge index.
    Knowledge,
}

impl Purpose {
    /// All purposes.
    pub const ALL: [Self; 3] = [Self::Router, Self::Memory, Self::Knowledge];
}

/// Kind of provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderType {
    /// Google Gemini API.
    Gemini,
    /// `OpenAI`.
    Openai,
    /// Any OpenAI-compatible endpoint (Ollama, LM Studio, `OpenRouter`, Mistral, Voyage, Together ...).
    Compatible,
    /// The local model of a `local-embeddings` build.
    Local,
}

impl ProviderType {
    /// Name used for catalog lookups.
    #[must_use]
    pub fn catalog_name(self) -> &'static str {
        match self {
            Self::Gemini => "gemini",
            Self::Openai => "openai",
            Self::Compatible => "compatible",
            Self::Local => "local",
        }
    }

    /// Whether the text leaves this computer.
    #[must_use]
    pub fn is_remote(self) -> bool {
        self != Self::Local
    }
}

/// One configured provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    /// Short unique id (`a-z`, `0-9`, `-`).
    pub id: String,
    /// Kind.
    pub kind: ProviderType,
    /// Endpoint for `compatible` (required); optional override for the others.
    #[serde(default)]
    pub base_url: Option<String>,
    /// Model id.
    pub model: String,
    /// Vector size.
    pub dimensions: usize,
    /// Send the `dimensions` parameter (turn off for endpoints that reject it).
    #[serde(default = "yes")]
    pub request_dimensions: bool,
    /// Secret-store name of the API key (never the key itself).
    #[serde(default)]
    pub key_ref: Option<String>,
    /// The owner explicitly agreed that note, memory and document text is sent
    /// to this provider. Without it the provider is never called.
    #[serde(default)]
    pub allow_remote: bool,
    /// Replace the catalog limits (free key limits).
    #[serde(default)]
    pub limits: Option<Limits>,
    /// Owner-only: also allow a custom endpoint on the private network.
    /// Link-local and cloud-metadata addresses are never allowed.
    #[serde(default)]
    pub allow_local_network: bool,
}

fn yes() -> bool {
    true
}

/// The whole configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingConfig {
    /// Format version.
    #[serde(default = "one")]
    pub version: u32,
    /// Configured providers (the offline hash embedder needs no entry).
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
    /// Per purpose: provider ids in priority order. The offline hash embedder
    /// is always tried last, so an empty or exhausted chain still works.
    #[serde(default)]
    pub chains: BTreeMap<Purpose, Vec<String>>,
    /// Bots that must never use a remote provider.
    #[serde(default)]
    pub remote_denied_bots: BTreeSet<String>,
}

fn one() -> u32 {
    1
}

impl Default for EmbeddingConfig {
    /// No keys, no providers: the offline hash embedder, nothing leaves the PC.
    fn default() -> Self {
        Self {
            version: 1,
            providers: Vec::new(),
            chains: BTreeMap::new(),
            remote_denied_bots: BTreeSet::new(),
        }
    }
}

/// A configuration that cannot be used, in plain words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError(pub String);

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

fn bad<T>(msg: impl Into<String>) -> Result<T, ConfigError> {
    Err(ConfigError(msg.into()))
}

impl EmbeddingConfig {
    /// Provider by id.
    #[must_use]
    pub fn provider(&self, id: &str) -> Option<&ProviderConfig> {
        self.providers.iter().find(|p| p.id == id)
    }

    /// Check ids, endpoints, dimensions and chains.
    ///
    /// # Errors
    /// A plain-language description of the first problem.
    pub fn validate(&self, catalog: &EmbeddingCatalog) -> Result<(), ConfigError> {
        let mut seen = BTreeSet::new();
        if self.providers.len() > 16 {
            return bad("too many embedding providers");
        }
        for p in &self.providers {
            let ok_id = !p.id.is_empty()
                && p.id.len() <= 40
                && p.id
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                && p.id != HASH_ID;
            if !ok_id {
                return bad("a provider id may only use a-z, 0-9 and '-'");
            }
            if !seen.insert(p.id.as_str()) {
                return bad(format!("provider id {:?} is used twice", p.id));
            }
            if p.model.trim().is_empty()
                || p.model.len() > 200
                || p.model.chars().any(char::is_control)
            {
                return bad("the embedding model name is empty or invalid");
            }
            if !(1..=MAX_DIMENSIONS).contains(&p.dimensions) {
                return bad("the vector size must be between 1 and 8192");
            }
            if p.kind == ProviderType::Compatible && p.base_url.as_deref().is_none_or(str::is_empty)
            {
                return bad("an OpenAI-compatible provider needs an endpoint address");
            }
            if let Some(url) = &p.base_url
                && provider_catalog::embedding::validate_endpoint(url, p.allow_local_network)
                    .is_err()
            {
                return bad(
                    "the endpoint address must be https (http only for localhost), without a password or query, and not a private, link-local or metadata address",
                );
            }
            if let Some(name) = &p.key_ref
                && (name.is_empty() || name.len() > 128 || name.chars().any(char::is_control))
            {
                return bad("the key reference is invalid");
            }
            if let Some(m) = catalog.find(p.kind.catalog_name(), &p.model)
                && p.dimensions > m.max_dimensions
            {
                return bad(format!(
                    "{} can produce at most {} numbers per text",
                    m.label, m.max_dimensions
                ));
            }
        }
        for (purpose, chain) in &self.chains {
            for id in chain {
                if self.provider(id).is_none() {
                    return bad(format!("{purpose:?} chain names unknown provider {id:?}"));
                }
            }
        }
        Ok(())
    }

    /// Read `path`; a missing file is the default (no keys, offline hash).
    ///
    /// # Errors
    /// Unreadable or invalid file.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|e| ConfigError(format!("embeddings.json is not valid: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => bad(format!("cannot read embeddings.json: {e}")),
        }
    }

    /// Write `path` atomically (temp file + rename).
    ///
    /// # Errors
    /// I/O failure.
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        let tmp = path.with_extension("json.tmp");
        let json = serde_json::to_vec_pretty(self).map_err(|e| ConfigError(e.to_string()))?;
        std::fs::write(&tmp, json)
            .and_then(|()| std::fs::rename(&tmp, path))
            .map_err(|e| ConfigError(format!("cannot write embeddings.json: {e}")))
    }
}

/// Identity of a vector space: vectors of different spaces must never be
/// compared. Vector stores are keyed by this.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VectorSpace {
    /// Provider id (`offline-hash` for the fallback).
    pub provider: String,
    /// Model id.
    pub model: String,
    /// Vector size.
    pub dimensions: usize,
}

impl VectorSpace {
    /// Stable text key (`provider/model@dimensions`).
    #[must_use]
    pub fn key(&self) -> String {
        format!("{}/{}@{}", self.provider, self.model, self.dimensions)
    }

    /// The offline hash space.
    #[must_use]
    pub fn hash() -> Self {
        Self {
            provider: HASH_ID.to_owned(),
            model: "feature-hash-v1".to_owned(),
            dimensions: HASH_DIMENSIONS,
        }
    }
}
