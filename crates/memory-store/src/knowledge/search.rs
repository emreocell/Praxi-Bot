//! Lexical retrieval and complete-response budget accounting.

use std::collections::HashSet;

use rusqlite::params;
use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

use super::store::{integer_at, sql_int, uuid_at};
use super::{
    KnowledgeStore, RetrievedContentTrust, SearchHit, SearchMode, SearchRequest, SearchResponse,
};
use crate::{MemoryError, Result, Trust, estimate_tokens};

const CANDIDATES: usize = 256;

impl SearchResponse {
    /// Reapply complete-response limits after a trusted host transformation,
    /// such as live-secret redaction. Previous truncation remains visible.
    ///
    /// # Errors
    /// Invalid limits or a budget too small for the response envelope.
    pub fn rebudget(self, request: &SearchRequest) -> Result<Self> {
        let previously_truncated = self.truncated;
        let mut response = pack_search_results(self.hits, request, self.mode)?;
        response.truncated |= previously_truncated;
        if !fits(&mut response, request)? {
            return Err(MemoryError::Invalid(
                "knowledge output budget is too small".into(),
            ));
        }
        Ok(response)
    }
}

impl KnowledgeStore {
    /// Offline BM25 retrieval, bounded after serializing text and citations.
    ///
    /// # Errors
    /// Invalid/excessive query, SQLite error, or corrupt stored metadata.
    pub fn search(&self, request: &SearchRequest) -> Result<SearchResponse> {
        let hits = self.lexical_candidates(request)?;
        pack_search_results(hits, request, SearchMode::Lexical)
    }

    pub(super) fn lexical_candidates(&self, request: &SearchRequest) -> Result<Vec<SearchHit>> {
        validate_request(request)?;
        let Some((query, terms, required_matches)) = query_expression(&request.query) else {
            return Ok(Vec::new());
        };
        let sources = serde_json::to_string(&request.source_ids)?;
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT c.id,c.source_id,c.document_id,s.label,d.locator,c.section,c.line_start,c.line_end,s.trust,c.body,bm25(chunks_fts,3.0,2.0,1.0),d.title,s.kind,s.locator
             FROM chunks_fts JOIN chunks c ON c.rowid=chunks_fts.rowid
             JOIN sources s ON s.id=c.source_id JOIN documents d ON d.id=c.document_id
             WHERE chunks_fts MATCH ?1 AND s.enabled=1
             AND (?2='[]' OR c.source_id IN (SELECT value FROM json_each(?2)))
             AND (?4=0 OR s.kind IN ('\"file\"','\"folder\"'))
             ORDER BY bm25(chunks_fts,3.0,2.0,1.0),d.locator,c.ordinal,c.id LIMIT ?3")?;
        let mut hits = stmt
            .query_map(
                params![query, sources, sql_int(CANDIDATES)?, request.files_only],
                |row| {
                    let source_trust = match row.get::<_, String>(8)?.as_str() {
                        "user" => Trust::User,
                        _ => Trust::Untrusted,
                    };
                    let mut hit = SearchHit {
                        id: uuid_at(row, 0)?,
                        source_id: uuid_at(row, 1)?,
                        document_id: uuid_at(row, 2)?,
                        source_label: row.get(3)?,
                        locator: row.get(4)?,
                        section: row.get(5)?,
                        line_start: integer_at(row, 6)?,
                        line_end: integer_at(row, 7)?,
                        path: None,
                        page: None,
                        citation: None,
                        source_trust,
                        content: row.get(9)?,
                        score: -row.get::<_, f64>(10)?,
                    };
                    hit.attach_citation(&row.get::<_, String>(12)?, &row.get::<_, String>(13)?);
                    let title: String = row.get(11)?;
                    let words: HashSet<_> =
                        normalized_terms(&format!("{title}\n{}\n{}", hit.section, hit.content))
                            .into_iter()
                            .collect();
                    let matches = terms.iter().filter(|term| words.contains(*term)).count();
                    Ok((hit, matches))
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        hits.retain(|(_, matches)| *matches >= required_matches);
        // Prefer evidence covering more of the meaningful query. A short
        // glossary repeating one keyword should not outrank an answer covering
        // the whole question merely because its document length is smaller.
        hits.sort_by(|(a, a_matches), (b, b_matches)| {
            b_matches
                .cmp(a_matches)
                .then_with(|| b.score.total_cmp(&a.score))
        });
        Ok(hits.into_iter().map(|(hit, _)| hit).collect())
    }
}

pub(super) fn validate_request(request: &SearchRequest) -> Result<()> {
    if request.query.len() > 4096
        || request.source_ids.len() > 32
        || !(1..=20).contains(&request.top_k)
        || !(64..=2000).contains(&request.budget_tokens)
        || !(256..=32768).contains(&request.max_bytes)
    {
        return Err(MemoryError::Invalid(
            "knowledge search exceeds supported limits".into(),
        ));
    }
    Ok(())
}

fn normalized_terms(text: &str) -> Vec<String> {
    let folded: String = text
        .nfd()
        .filter(|ch| !is_combining_mark(*ch))
        .flat_map(char::to_lowercase)
        .collect();
    folded
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect()
}

fn stopword(word: &str) -> bool {
    matches!(
        word,
        "a" | "an"
            | "and"
            | "are"
            | "as"
            | "at"
            | "be"
            | "been"
            | "before"
            | "but"
            | "by"
            | "can"
            | "could"
            | "did"
            | "do"
            | "does"
            | "doing"
            | "for"
            | "from"
            | "have"
            | "has"
            | "how"
            | "i"
            | "if"
            | "in"
            | "is"
            | "it"
            | "its"
            | "me"
            | "my"
            | "of"
            | "on"
            | "or"
            | "our"
            | "s"
            | "should"
            | "that"
            | "the"
            | "their"
            | "them"
            | "there"
            | "these"
            | "they"
            | "this"
            | "to"
            | "today"
            | "was"
            | "we"
            | "were"
            | "what"
            | "when"
            | "where"
            | "which"
            | "who"
            | "why"
            | "will"
            | "with"
            | "would"
            | "you"
            | "your"
            | "find"
            | "please"
            | "acaba"
            | "bir"
            | "bu"
            | "bugun"
            | "da"
            | "de"
            | "icin"
            | "ile"
            | "mi"
            | "mu"
            | "nasil"
            | "ne"
            | "neden"
            | "nerede"
            | "olan"
            | "ve"
            | "veya"
    )
}

fn query_expression(query: &str) -> Option<(String, Vec<String>, usize)> {
    let mut seen = HashSet::new();
    let terms: Vec<String> = normalized_terms(query)
        .into_iter()
        .filter(|word| !stopword(word))
        .filter(|word| seen.insert(word.clone()))
        .take(32)
        .collect();
    let required_matches = if query.contains('?') {
        2
    } else {
        terms.len().min(2)
    };
    if terms.is_empty() || terms.len() < required_matches {
        return None;
    }
    let expression = terms
        .iter()
        .map(|word| format!("\"{word}\""))
        .collect::<Vec<_>>()
        .join(" OR ");
    Some((expression, terms, required_matches))
}

pub(super) fn pack_search_results(
    hits: Vec<SearchHit>,
    request: &SearchRequest,
    mode: SearchMode,
) -> Result<SearchResponse> {
    validate_request(request)?;
    let mut response = SearchResponse {
        content_trust: RetrievedContentTrust::UntrustedData,
        hits: Vec::new(),
        mode,
        truncated: false,
        estimated_tokens: 0,
    };
    let candidate_count = hits.len();
    for hit in hits {
        if response.hits.len() == request.top_k {
            response.truncated = true;
            break;
        }
        response.hits.push(hit);
        if fits(&mut response, request)? {
            continue;
        }
        response.truncated = true;
        let mut hit = response.hits.pop().expect("pushed hit");
        let original = std::mem::take(&mut hit.content);
        let boundaries: Vec<usize> = original
            .char_indices()
            .map(|(byte, _)| byte)
            .chain(std::iter::once(original.len()))
            .collect();
        let mut low = 0;
        let mut high = boundaries.len().saturating_sub(1);
        // Serialize each candidate: quotes, control characters, citations and
        // the budget's own numeric field are part of the actual output.
        while low < high {
            let middle = low + (high - low).div_ceil(2);
            hit.content = format!("{}\n[excerpt truncated]", &original[..boundaries[middle]]);
            response.hits.push(hit.clone());
            let accepted = fits(&mut response, request)?;
            response.hits.pop();
            if accepted {
                low = middle;
            } else {
                high = middle - 1;
            }
        }
        if low > 0 {
            hit.content = format!("{}\n[excerpt truncated]", &original[..boundaries[low]]);
            response.hits.push(hit);
        }
    }
    response.truncated |= candidate_count > response.hits.len();
    // Changing false -> true shortens the wire representation by one byte.
    if !fits(&mut response, request)? {
        return Err(MemoryError::Invalid(
            "knowledge output budget is too small".into(),
        ));
    }
    Ok(response)
}

fn fits(response: &mut SearchResponse, request: &SearchRequest) -> Result<bool> {
    // Fixed point includes the serialized estimate itself. It converges after
    // at most a few digit-width transitions for the bounded result sizes.
    for _ in 0..4 {
        let json = serde_json::to_string(response)?;
        let estimated = estimate_tokens(&json);
        if estimated == response.estimated_tokens {
            return Ok(estimated <= request.budget_tokens && json.len() <= request.max_bytes);
        }
        response.estimated_tokens = estimated;
    }
    let json = serde_json::to_string(response)?;
    Ok(estimate_tokens(&json) <= request.budget_tokens && json.len() <= request.max_bytes)
}
