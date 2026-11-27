//! Knowledge catalog, indexing and retrieval contracts.

use protocol::{BotId, UserId};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::Trust;

/// Immutable owner of one physical knowledge index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    /// The authenticated owner, resolved by the host.
    pub user_id: UserId,
    /// The bot that may read this index.
    pub bot_id: BotId,
}

/// Origin of source text. This is provenance, never instruction authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// Selected individual file.
    File,
    /// Selected directory.
    Folder,
    /// Previously saved web response; indexing does not fetch it.
    SavedWeb,
    /// Authorized conversation snapshot.
    Conversation,
    /// Authorized email snapshot.
    Email,
    /// Bot note.
    Note,
    /// Human-editable shared Markdown files.
    SharedBrain,
}

/// An explicit grant for one immutable embedding profile.
///
/// The profile fingerprint binds provider, endpoint, model/revision, dimensions,
/// task formatting and normalization. A grant is not a query-egress grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteEmbeddingConsent {
    /// SHA-256 profile fingerprint (64 lower-case hexadecimal characters).
    pub profile_fingerprint: String,
    /// Time the host recorded the user's grant.
    pub granted_at_ms: i64,
}

/// Source creation input. New sources never have remote embedding consent.
#[derive(Debug, Clone)]
pub struct NewSource {
    /// Source adapter kind.
    pub kind: SourceKind,
    /// Human-readable label.
    pub label: String,
    /// Host-resolved path or opaque source locator.
    pub locator: String,
    /// Provenance only; all retrieval results remain untrusted data.
    pub trust: Trust,
}

impl NewSource {
    /// Create an untrusted-data source with no network authorization.
    #[must_use]
    pub fn new(kind: SourceKind, label: impl Into<String>, locator: impl Into<String>) -> Self {
        Self {
            kind,
            label: label.into(),
            locator: locator.into(),
            trust: Trust::Untrusted,
        }
    }
}

/// Persisted source metadata; document content cannot change these fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Source {
    /// Store-generated source identifier.
    pub id: Uuid,
    /// Adapter kind, immutable after creation.
    pub kind: SourceKind,
    /// Display label.
    pub label: String,
    /// Host-resolved locator, immutable after creation.
    pub locator: String,
    /// Provenance, immutable after creation.
    pub trust: Trust,
    /// Disabled sources cannot be indexed or retrieved.
    pub enabled: bool,
    /// Incremented by every source or document mutation.
    pub revision: u64,
    /// Explicit, profile-bound consent; absent by default.
    pub remote_embedding: Option<RemoteEmbeddingConsent>,
    /// Creation timestamp in milliseconds.
    pub created_ms: i64,
    /// Last mutation timestamp in milliseconds.
    pub updated_ms: i64,
}

/// A revision-checked source mutation. `None` keeps the current field.
///
/// This is a Rust API, deliberately not a JSON patch: `Some(None)` revokes
/// consent, and must remain distinct from an omitted field in any HTTP DTO.
#[derive(Debug, Clone, Default)]
pub struct SourceUpdate {
    /// New display label.
    pub label: Option<String>,
    /// Enable or disable indexing/retrieval.
    pub enabled: Option<bool>,
    /// Keep, revoke, or replace remote embedding consent.
    pub remote_embedding: Option<Option<RemoteEmbeddingConsent>>,
}

/// Format of text already extracted by an authorized source adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentFormat {
    /// Markdown headings, paragraphs, fences and tables.
    Markdown,
    /// Plain paragraphs.
    PlainText,
    /// Source code, chunked on whole lines when possible.
    Code,
}

/// Bounded, decoded document; parsers and network access live outside the store.
#[derive(Debug, Clone)]
pub struct DocumentInput {
    /// Source-relative path or stable opaque document locator.
    pub locator: String,
    /// Human-readable title.
    pub title: String,
    /// Format of the supplied text.
    pub format: DocumentFormat,
    /// Extracted UTF-8 text. Lines in citations refer to this exact text.
    pub content: String,
    /// Source modification time, if known; the content hash is authoritative.
    pub mtime_ms: Option<i64>,
}

/// Persisted document metadata, without loading document bodies.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocumentInfo {
    /// Store-generated document identifier.
    pub id: Uuid,
    /// Owning source.
    pub source_id: Uuid,
    /// Stable document locator.
    pub locator: String,
    /// Display title.
    pub title: String,
    /// SHA-256 of text, format and chunker version.
    pub content_hash: String,
    /// Source modification time.
    pub mtime_ms: Option<i64>,
    /// Monotonic document revision.
    pub revision: u64,
    /// Number of published chunks.
    pub chunks: usize,
}

/// Result of an atomic, single-document indexing operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexOutcome {
    /// True when the content and metadata were unchanged.
    pub unchanged: bool,
    /// Indexed document identifier.
    pub document_id: Uuid,
    /// Current source revision; use for the next mutation in a batch.
    pub source_revision: u64,
    /// Number of published chunks.
    pub chunks: usize,
}

/// A structure-aware chunk before persistence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextChunk {
    /// Heading ancestry, joined by ` / `.
    pub section: String,
    /// First line in the supplied text, one-based.
    pub line_start: usize,
    /// Last line in the supplied text, inclusive.
    pub line_end: usize,
    /// Text of this chunk.
    pub content: String,
}

/// Bounded resource limits for one knowledge store.
#[derive(Debug, Clone, Copy)]
pub struct KnowledgeLimits {
    /// Maximum UTF-8 bytes accepted for a document.
    pub document_bytes: usize,
    /// Maximum Unicode scalars in a chunk, including repeated table headers.
    pub chunk_chars: usize,
    /// Maximum chunks produced by one document.
    pub chunks_per_document: usize,
    /// Maximum registered sources.
    pub sources: usize,
    /// Maximum indexed documents.
    pub documents: usize,
    /// Maximum chunks stored in this index.
    pub chunks: usize,
}

impl Default for KnowledgeLimits {
    fn default() -> Self {
        Self {
            document_bytes: 4 * 1024 * 1024,
            chunk_chars: 1600,
            chunks_per_document: 4096,
            sources: 256,
            documents: 50_000,
            chunks: 100_000,
        }
    }
}

/// Parameters for bounded retrieval; the bot and owner are never model inputs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SearchRequest {
    /// Plain query text, not an FTS expression.
    pub query: String,
    /// Optional source allowlist within the already-bound bot index.
    pub source_ids: Vec<Uuid>,
    /// Maximum returned chunks (1..=20).
    pub top_k: usize,
    /// Final serialized response budget, estimated as Unicode scalars / 4.
    pub budget_tokens: usize,
    /// Independent hard UTF-8 output byte budget.
    pub max_bytes: usize,
    /// Search only selected files and folders (not notes, conversations, saved
    /// pages or mail). Set by the host for `documents_search`; never read from
    /// a tool call.
    #[serde(skip)]
    pub files_only: bool,
}

impl SearchHit {
    /// Fill `path`, `page` and `citation` from the catalog fields of the source
    /// this chunk belongs to. Other source kinds keep them empty.
    pub fn attach_citation(&mut self, source_kind: &str, source_locator: &str) {
        if let Some(citation) = super::citation::cite(
            self.source_id,
            source_kind,
            source_locator,
            &self.locator,
            &self.section,
            (self.line_start, self.line_end),
        ) {
            self.path = Some(citation.path);
            self.page = citation.page;
            self.citation = Some(citation.markdown);
        }
    }
}

impl Default for SearchRequest {
    fn default() -> Self {
        Self {
            query: String::new(),
            source_ids: Vec::new(),
            top_k: 5,
            budget_tokens: 600,
            max_bytes: 8192,
            files_only: false,
        }
    }
}

impl SearchRequest {
    /// Search with the default 600 estimated-token, 8 KiB output budgets.
    #[must_use]
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            ..Self::default()
        }
    }
}

/// A cited result. Provenance never grants instruction authority.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchHit {
    /// Chunk identifier, stable until the document changes.
    pub id: Uuid,
    /// Source identifier.
    pub source_id: Uuid,
    /// Document identifier.
    pub document_id: Uuid,
    /// Source label.
    pub source_label: String,
    /// Document locator.
    pub locator: String,
    /// Heading ancestry.
    pub section: String,
    /// First cited line, one-based.
    pub line_start: usize,
    /// Last cited line in the extracted text.
    pub line_end: usize,
    /// Full path of the cited file, for file and folder sources.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// One-based PDF page of the passage, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
    /// Markdown link to the file at that place, to copy into an answer as given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub citation: Option<String>,
    /// Catalog provenance; the result itself is always untrusted data.
    pub source_trust: Trust,
    /// Bounded excerpt.
    pub content: String,
    /// Ranking score, higher is better.
    pub score: f64,
}

/// Which retrievers contributed to the result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    /// SQLite FTS5 BM25; no embedder or network needed.
    Lexical,
    /// Lexical and exact cosine candidates fused with reciprocal ranks.
    Hybrid,
    /// Optional semantic retrieval was unavailable; lexical results remain valid.
    LexicalDegraded,
}

/// Effective trust of every retrieved body, independently of source authorship.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievedContentTrust {
    /// Quoted source data; never an instruction from the user or system.
    UntrustedData,
}

/// Complete budgeted response, including citation overhead.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchResponse {
    /// Fixed effective trust label, included in the serialized output budget.
    pub content_trust: RetrievedContentTrust,
    /// All entries are quoted, untrusted data even for user-authored sources.
    pub hits: Vec<SearchHit>,
    /// Retrieval method actually used.
    pub mode: SearchMode,
    /// Candidates or excerpts were omitted to honor a limit.
    pub truncated: bool,
    /// Estimate for the complete serialized response; not provider billing usage.
    pub estimated_tokens: usize,
}
