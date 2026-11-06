//! A small in-house BM25 (Okapi, Lucene-style IDF) inverted index.
//!
//! # Why not `tantivy`?
//!
//! The corpus here is *tool descriptions*: tens to a few thousand tiny
//! documents that change whenever an MCP server connects, disconnects or is
//! quarantined.  `tantivy` brings a segment/commit model, an on-disk (or mmap)
//! store and a large dependency tree for a problem that fits in ~100 lines of
//! in-memory code and rebuilds in well under a millisecond for realistic tool
//! counts.  Klavis Strata (our reference) does the same with a plain BM25
//! library.  If the corpus ever grows to millions of documents the
//! [`Bm25Index`] boundary is small enough to swap out.

use std::collections::HashMap;

/// Term-frequency saturation.
const K1: f64 = 1.2;
/// Length normalisation.
const B: f64 = 0.75;

/// Split `text` into lowercase alphanumeric terms.
///
/// Any non-alphanumeric character is a separator (so `mcp:github/create_issue`
/// yields `mcp`, `github`, `create`, `issue`), and `camelCase` boundaries are
/// split as well (`createIssue` → `create`, `issue`).
#[must_use]
pub fn tokenize(text: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut current = String::new();
    let mut prev_lower_or_digit = false;
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            if ch.is_uppercase() && prev_lower_or_digit && !current.is_empty() {
                terms.push(std::mem::take(&mut current));
            }
            current.extend(ch.to_lowercase());
            prev_lower_or_digit = ch.is_lowercase() || ch.is_numeric();
        } else {
            if !current.is_empty() {
                terms.push(std::mem::take(&mut current));
            }
            prev_lower_or_digit = false;
        }
    }
    if !current.is_empty() {
        terms.push(current);
    }
    terms
}

/// In-memory BM25 index over pre-tokenised documents.
///
/// Documents are identified by their position in the iterator passed to
/// [`Bm25Index::new`].
#[derive(Debug, Clone, Default)]
pub struct Bm25Index {
    /// term -> (doc id, term frequency in that doc)
    postings: HashMap<String, Vec<(usize, u32)>>,
    doc_lens: Vec<u32>,
    avg_len: f64,
}

impl Bm25Index {
    /// Build an index; document `i` is the `i`-th item of `docs`.
    pub fn new(docs: impl IntoIterator<Item = Vec<String>>) -> Self {
        let mut postings: HashMap<String, Vec<(usize, u32)>> = HashMap::new();
        let mut doc_lens = Vec::new();
        for (doc_id, terms) in docs.into_iter().enumerate() {
            let mut tf: HashMap<String, u32> = HashMap::new();
            for term in &terms {
                *tf.entry(term.clone()).or_insert(0) += 1;
            }
            doc_lens.push(u32::try_from(terms.len()).unwrap_or(u32::MAX));
            for (term, freq) in tf {
                postings.entry(term).or_default().push((doc_id, freq));
            }
        }
        let total: f64 = doc_lens.iter().map(|&l| f64::from(l)).sum();
        #[allow(clippy::cast_precision_loss)] // document counts are tiny
        let avg_len = if doc_lens.is_empty() {
            0.0
        } else {
            total / doc_lens.len() as f64
        };
        Self {
            postings,
            doc_lens,
            avg_len,
        }
    }

    /// Number of indexed documents.
    #[must_use]
    pub fn len(&self) -> usize {
        self.doc_lens.len()
    }

    /// `true` if no documents are indexed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.doc_lens.is_empty()
    }

    /// Score documents against `query_terms` and return the best `k`
    /// `(doc id, score)` pairs, best first.  Documents that match no query term
    /// are never returned.  Ties are broken by ascending doc id, so results are
    /// deterministic.
    #[must_use]
    pub fn search(&self, query_terms: &[String], k: usize) -> Vec<(usize, f64)> {
        if k == 0 || self.is_empty() {
            return Vec::new();
        }
        #[allow(clippy::cast_precision_loss)]
        let n_docs = self.len() as f64;
        let mut scores: HashMap<usize, f64> = HashMap::new();
        let mut seen: Vec<&str> = Vec::new();
        for term in query_terms {
            if seen.contains(&term.as_str()) {
                continue;
            }
            seen.push(term);
            let Some(posting) = self.postings.get(term) else {
                continue;
            };
            #[allow(clippy::cast_precision_loss)]
            let df = posting.len() as f64;
            let idf = (1.0 + (n_docs - df + 0.5) / (df + 0.5)).ln();
            for &(doc_id, freq) in posting {
                let tf = f64::from(freq);
                let len = f64::from(self.doc_lens[doc_id]);
                let norm = 1.0 - B + B * len / self.avg_len.max(f64::EPSILON);
                let score = idf * tf * (K1 + 1.0) / (tf + K1 * norm);
                *scores.entry(doc_id).or_insert(0.0) += score;
            }
        }
        let mut ranked: Vec<(usize, f64)> = scores.into_iter().collect();
        ranked.sort_by(|(ia, sa), (ib, sb)| sb.total_cmp(sa).then(ia.cmp(ib)));
        ranked.truncate(k);
        ranked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_splits_separators_and_camel_case() {
        assert_eq!(
            tokenize("mcp:github/create_issue"),
            ["mcp", "github", "create", "issue"]
        );
        assert_eq!(tokenize("createIssueV2"), ["create", "issue", "v2"]);
        assert!(tokenize("  ,;. ").is_empty());
    }

    #[test]
    fn rarer_terms_and_shorter_docs_rank_higher() {
        let docs = vec![
            tokenize("read file contents from disk"),
            tokenize("send email message"),
            tokenize("read email"),
        ];
        let index = Bm25Index::new(docs);
        // "email" matches docs 1 and 2; the shorter doc 2 wins.
        let hits = index.search(&tokenize("email"), 10);
        assert_eq!(hits.iter().map(|h| h.0).collect::<Vec<_>>(), [2, 1]);
        // "disk" is unique to doc 0.
        assert_eq!(index.search(&tokenize("disk"), 1)[0].0, 0);
        assert!(index.search(&tokenize("nothing"), 5).is_empty());
        assert!(index.search(&tokenize("email"), 0).is_empty());
    }

    #[test]
    fn empty_index_returns_nothing() {
        let index = Bm25Index::new(Vec::new());
        assert!(index.is_empty());
        assert!(index.search(&tokenize("x"), 3).is_empty());
    }
}
