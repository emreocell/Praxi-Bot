//! K2: BM25 + embedding hybrid ranking over workflows, skills and tools.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::BackendError;
use super::normalize::terms;
use crate::bm25::Bm25Index;

/// What a routing candidate is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateKind {
    /// A saved workflow (replayable without an LLM).
    Workflow,
    /// A skill.
    Skill,
    /// A tool.
    Tool,
}

/// A workflow parameter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateParam {
    /// Parameter name.
    pub name: String,
    /// Must be supplied for a replay.
    pub required: bool,
}

/// One routable item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    /// Kind.
    pub kind: CandidateKind,
    /// Unique name within its kind.
    pub name: String,
    /// Short description (indexed and shown to the classifier).
    pub description: String,
    /// Fingerprint that changes whenever the item changes (workflows: the
    /// cache is invalidated when it differs from the stored one).
    pub version: String,
    /// Declared parameters (workflows).
    #[serde(default)]
    pub params: Vec<CandidateParam>,
}

impl Candidate {
    /// A candidate without parameters and with version `"1"`.
    #[must_use]
    pub fn new(kind: CandidateKind, name: &str, description: &str) -> Self {
        Self {
            kind,
            name: name.to_owned(),
            description: description.to_owned(),
            version: "1".to_owned(),
            params: Vec::new(),
        }
    }

    /// Set the version fingerprint.
    #[must_use]
    pub fn with_version(mut self, version: &str) -> Self {
        version.clone_into(&mut self.version);
        self
    }

    /// Declare a parameter.
    #[must_use]
    pub fn with_param(mut self, name: &str, required: bool) -> Self {
        self.params.push(CandidateParam {
            name: name.to_owned(),
            required,
        });
        self
    }

    /// Names of required parameters.
    pub fn required_params(&self) -> impl Iterator<Item = &str> {
        self.params
            .iter()
            .filter(|p| p.required)
            .map(|p| p.name.as_str())
    }

    /// Text sent to the embedder.
    #[must_use]
    pub fn embed_text(&self) -> String {
        format!("{}: {}", self.name.replace('_', " "), self.description)
    }
}

/// Whether text is a stored document or a search query (Gemini task types).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbedTask {
    /// A candidate document.
    Document,
    /// A user query.
    Query,
}

/// Pluggable text embedder.
#[async_trait]
pub trait Embedder: Send + Sync {
    /// `false` for [`NullEmbedder`]: the ladder then skips embedding entirely.
    fn is_enabled(&self) -> bool {
        true
    }

    /// One vector per input text, in order.
    async fn embed(&self, texts: &[String], task: EmbedTask)
    -> Result<Vec<Vec<f32>>, BackendError>;
}

/// Embedder that is always off; K2 is then BM25 only.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullEmbedder;

#[async_trait]
impl Embedder for NullEmbedder {
    fn is_enabled(&self) -> bool {
        false
    }

    async fn embed(&self, _: &[String], _: EmbedTask) -> Result<Vec<Vec<f32>>, BackendError> {
        Err(BackendError::new("embeddings disabled"))
    }
}

/// Tunables of the hybrid score.
#[derive(Debug, Clone, Copy)]
pub struct Scoring {
    /// BM25 saturation: `lex = s / (s + bm25_k)`.
    pub bm25_k: f64,
    /// Cosine at/below which the semantic score is 0.
    pub cos_floor: f64,
    /// Cosine at/above which the semantic score is 1.
    pub cos_ceil: f64,
}

impl Default for Scoring {
    fn default() -> Self {
        Self {
            bm25_k: 4.0,
            cos_floor: 0.5,
            cos_ceil: 0.85,
        }
    }
}

/// A ranked candidate.
#[derive(Debug, Clone, PartialEq)]
pub struct Scored {
    /// Index into the corpus.
    pub index: usize,
    /// Lexical score in `0..=1`.
    pub lexical: f64,
    /// Semantic score in `0..=1` (0 without embeddings).
    pub semantic: f64,
    /// Hybrid score in `0..=1`.
    pub score: f64,
}

/// Immutable snapshot of the routable items plus their indexes.
#[derive(Debug, Clone, Default)]
pub struct Corpus {
    pub(crate) candidates: Vec<Candidate>,
    bm25: Bm25Index,
    pub(crate) embeddings: Option<Vec<Vec<f32>>>,
}

const NAME_BOOST: usize = 3;

impl Corpus {
    /// Index `candidates` (no embeddings yet).
    #[must_use]
    pub fn new(candidates: Vec<Candidate>) -> Self {
        let docs = candidates.iter().map(|c| {
            let name_terms = terms(&c.name);
            let mut t = Vec::new();
            for _ in 0..NAME_BOOST {
                t.extend(name_terms.iter().cloned());
            }
            t.extend(terms(&c.description));
            t
        });
        let bm25 = Bm25Index::new(docs);
        Self {
            candidates,
            bm25,
            embeddings: None,
        }
    }

    /// Same candidates with embeddings attached.
    #[must_use]
    pub fn with_embeddings(&self, embeddings: Vec<Vec<f32>>) -> Self {
        Self {
            candidates: self.candidates.clone(),
            bm25: self.bm25.clone(),
            embeddings: Some(embeddings),
        }
    }

    /// Number of candidates.
    #[must_use]
    pub fn len(&self) -> usize {
        self.candidates.len()
    }

    /// `true` if there are no candidates.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.candidates.is_empty()
    }

    /// Candidate by index.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&Candidate> {
        self.candidates.get(index)
    }

    /// Find a candidate by kind and name.
    #[must_use]
    pub fn find(&self, kind: CandidateKind, name: &str) -> Option<&Candidate> {
        self.candidates
            .iter()
            .find(|c| c.kind == kind && c.name == name)
    }

    /// Rank every candidate with a non-zero score, best first.
    #[must_use]
    pub fn rank(&self, query: &str, query_emb: Option<&[f32]>, scoring: Scoring) -> Vec<Scored> {
        let lex: std::collections::HashMap<usize, f64> = self
            .bm25
            .search(&query_terms(query), self.len())
            .into_iter()
            .collect();
        let mut out = Vec::new();
        for index in 0..self.len() {
            let lexical = lex.get(&index).map_or(0.0, |s| s / (s + scoring.bm25_k));
            let semantic = match (
                query_emb,
                self.embeddings.as_ref().and_then(|e| e.get(index)),
            ) {
                (Some(q), Some(d)) => ((cosine(q, d) - scoring.cos_floor)
                    / (scoring.cos_ceil - scoring.cos_floor))
                    .clamp(0.0, 1.0),
                _ => 0.0,
            };
            // noisy-or: agreement of both signals raises confidence.
            let score = 1.0 - (1.0 - lexical) * (1.0 - semantic);
            if score > 0.0 {
                out.push(Scored {
                    index,
                    lexical,
                    semantic,
                    score,
                });
            }
        }
        out.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.index.cmp(&b.index)));
        out
    }
}

/// Cosine similarity (0 for mismatched or zero vectors).
#[must_use]
pub fn cosine(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0.0_f64, 0.0_f64, 0.0_f64);
    for (x, y) in a.iter().zip(b) {
        let (x, y) = (f64::from(*x), f64::from(*y));
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

/// Folded function words (EN + TR) that carry no routing signal.
const STOPWORDS: &[&str] = &[
    "a", "an", "the", "to", "of", "my", "me", "and", "for", "on", "in", "is", "it", "this", "that",
    "i", "you", "please", "bir", "ve", "icin", "de", "da", "bu", "su", "mi", "ben", "bana",
];

/// Query terms without stopwords (unless that would leave nothing).
fn query_terms(query: &str) -> Vec<String> {
    let all = terms(query);
    let kept: Vec<String> = all
        .iter()
        .filter(|t| !STOPWORDS.contains(&t.as_str()))
        .cloned()
        .collect();
    if kept.is_empty() { all } else { kept }
}
