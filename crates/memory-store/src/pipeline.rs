//! Extraction pipeline: model-proposed candidates -> code-enforced rules ->
//! consolidation -> store.

use async_trait::async_trait;
use protocol::{BotId, ChatId, UserId};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::Result;
use crate::secrets::contains_secret;
use crate::store::MemoryStore;
use crate::types::{Kind, Memory, MemoryPatch, NewMemory, Trust};

/// A memory proposed by the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    /// Category.
    pub kind: Kind,
    /// Statement to remember.
    pub content: String,
    /// Verbatim supporting span of the excerpt (mandatory).
    pub quote: Option<String>,
    /// Model's own flag: source is a question/hypothetical/wish.
    #[serde(default)]
    pub hypothetical: bool,
    /// 0..=1.
    #[serde(default = "default_importance")]
    pub importance: f32,
    /// Model's flag: derived from web/file/tool content rather than the user.
    #[serde(default)]
    pub untrusted: bool,
}

fn default_importance() -> f32 {
    0.5
}

/// Consolidation verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Store as a new memory.
    Add,
    /// Rewrite the given existing memory to the candidate.
    Update(Uuid),
    /// The user retracted the given existing memory.
    Delete(Uuid),
    /// Nothing to do.
    Skip,
}

/// LLM abstraction (prompts live in [`crate::prompts`]).
#[async_trait]
pub trait MemoryModel: Send + Sync {
    /// Propose candidates from a chat excerpt.
    ///
    /// # Errors
    /// Model/transport failure.
    async fn extract(&self, chat_excerpt: &str) -> Result<Vec<Candidate>>;

    /// Decide what to do with `candidate` given similar existing memories.
    ///
    /// # Errors
    /// Model/transport failure.
    async fn consolidate(&self, candidate: &Candidate, similar: &[Memory]) -> Result<Decision>;
}

/// Where an excerpt came from.
#[derive(Debug, Clone)]
pub struct ExtractionContext {
    /// Owner.
    pub user_id: UserId,
    /// Bot in the conversation.
    pub bot_id: BotId,
    /// Chat the excerpt belongs to.
    pub chat_id: ChatId,
    /// Message the excerpt ends at, if known.
    pub message_id: Option<Uuid>,
    /// The text handed to the model (and the only text quotes may come from).
    pub excerpt: String,
    /// The excerpt is (or embeds) web/file/tool content: everything stored
    /// from it is `trust = untrusted`.
    pub untrusted: bool,
}

/// Why a candidate was dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    /// Empty content.
    Empty,
    /// No supporting quote.
    NoQuote,
    /// Quote is not a span of the excerpt.
    QuoteNotInSource,
    /// Question / hypothetical, not a fact.
    Question,
    /// Looks like a credential.
    Secret,
    /// Model returned an unusable decision (unknown id).
    InvalidDecision,
}

/// Result for one candidate.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// New memory stored.
    Added(Memory),
    /// Existing memory rewritten (journaled).
    Updated(Memory),
    /// Existing memory soft-deleted (journaled).
    Deleted(Uuid),
    /// Nothing stored.
    Skipped,
    /// Dropped by a code-enforced rule.
    Rejected(RejectReason),
}

/// Extract -> validate -> consolidate -> persist.
pub struct Pipeline<'a> {
    store: &'a MemoryStore,
    model: &'a dyn MemoryModel,
    similar_k: usize,
}

impl<'a> Pipeline<'a> {
    /// New pipeline (5 similar memories are shown to the consolidator).
    #[must_use]
    pub fn new(store: &'a MemoryStore, model: &'a dyn MemoryModel) -> Self {
        Self {
            store,
            model,
            similar_k: 5,
        }
    }

    /// Run the pipeline on one excerpt; one [`Outcome`] per candidate.
    ///
    /// # Errors
    /// Model or store failures.
    pub async fn ingest(&self, ctx: &ExtractionContext) -> Result<Vec<Outcome>> {
        let candidates = self.model.extract(&ctx.excerpt).await?;
        let mut out = Vec::with_capacity(candidates.len());
        for c in candidates {
            out.push(self.handle(ctx, &c).await?);
        }
        Ok(out)
    }

    async fn handle(&self, ctx: &ExtractionContext, c: &Candidate) -> Result<Outcome> {
        if let Some(reason) = validate(ctx, c) {
            return Ok(Outcome::Rejected(reason));
        }
        let untrusted = ctx.untrusted || c.untrusted;
        let content = c.content.trim();
        let similar = self
            .store
            .find_similar(ctx.user_id, ctx.bot_id, content, self.similar_k)?;
        let decision = if similar.is_empty() {
            Decision::Add
        } else {
            self.model.consolidate(c, &similar).await?
        };
        match decision {
            Decision::Skip => Ok(Outcome::Skipped),
            Decision::Add => {
                let mut new =
                    NewMemory::new(ctx.user_id, scope(ctx.bot_id, c.kind), c.kind, content);
                new.source_quote.clone_from(&c.quote);
                new.source_chat_id = Some(ctx.chat_id);
                new.source_message_id = ctx.message_id;
                new.trust = trust(untrusted);
                new.importance = c.importance;
                Ok(Outcome::Added(self.store.add(&new, "extracted")?))
            }
            Decision::Update(id) | Decision::Delete(id) => {
                let Some(target) = similar.iter().find(|m| m.id == id) else {
                    return Ok(Outcome::Rejected(RejectReason::InvalidDecision));
                };
                // Untrusted content may never overwrite or retract what the user said.
                if untrusted && target.trust == Trust::User {
                    return Ok(Outcome::Skipped);
                }
                if matches!(decision, Decision::Delete(_)) {
                    if untrusted {
                        return Ok(Outcome::Skipped);
                    }
                    self.store
                        .delete(ctx.user_id, id, "retracted in conversation")?;
                    return Ok(Outcome::Deleted(id));
                }
                let patch = MemoryPatch {
                    content: Some(content.to_owned()),
                    source_quote: c.quote.clone(),
                    source_chat_id: Some(ctx.chat_id),
                    source_message_id: ctx.message_id,
                    trust: Some(trust(untrusted)),
                    importance: Some(c.importance.max(target.importance)),
                    valid_until_ms: None,
                };
                let m = self
                    .store
                    .update(ctx.user_id, id, &patch, "consolidated update")?;
                Ok(Outcome::Updated(m))
            }
        }
    }
}

fn trust(untrusted: bool) -> Trust {
    if untrusted {
        Trust::Untrusted
    } else {
        Trust::User
    }
}

/// Profile/episodic facts are shared across the user's bots; the rest is
/// scoped to the bot that learned it.
fn scope(bot: BotId, kind: Kind) -> Option<BotId> {
    match kind {
        Kind::Profile | Kind::Episodic => None,
        Kind::Bot | Kind::Procedural | Kind::Lesson => Some(bot),
    }
}

fn norm(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn validate(ctx: &ExtractionContext, c: &Candidate) -> Option<RejectReason> {
    if c.content.trim().is_empty() {
        return Some(RejectReason::Empty);
    }
    if contains_secret(&c.content) || c.quote.as_deref().is_some_and(contains_secret) {
        return Some(RejectReason::Secret);
    }
    let quote = c.quote.as_deref().map(str::trim).filter(|q| !q.is_empty());
    let Some(quote) = quote else {
        return Some(RejectReason::NoQuote);
    };
    if !norm(&ctx.excerpt).contains(&norm(quote)) {
        return Some(RejectReason::QuoteNotInSource);
    }
    if c.hypothetical
        || looks_like_question_or_hypothetical(quote)
        || looks_like_question_or_hypothetical(&c.content)
    {
        return Some(RejectReason::Question);
    }
    None
}

const HYPOTHETICAL_STARTS: &[&str] = &[
    "what if",
    "suppose",
    "imagine",
    "hypothetically",
    "i wonder",
    "let's say",
    "lets say",
    "say i",
    "if i were",
    "if i was",
    "what would",
    "would it be",
];

fn sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in text.chars() {
        cur.push(ch);
        if matches!(ch, '.' | '!' | '?' | '\n') {
            out.push(std::mem::take(&mut cur));
        }
    }
    out.push(cur);
    out.into_iter()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Simple detector: true if every sentence of `text` is a question or a
/// hypothetical ("what if ...", "suppose ...").  A statement mixed with a
/// question still counts as a statement.
#[must_use]
pub fn looks_like_question_or_hypothetical(text: &str) -> bool {
    let s = sentences(text);
    !s.is_empty()
        && s.iter().all(|sent| {
            let l = sent.to_lowercase();
            sent.ends_with('?') || HYPOTHETICAL_STARTS.iter().any(|p| l.starts_with(p))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detector() {
        assert!(looks_like_question_or_hypothetical("Do I live in Berlin?"));
        assert!(looks_like_question_or_hypothetical(
            "What if I moved to Paris."
        ));
        assert!(!looks_like_question_or_hypothetical("I live in Berlin."));
        assert!(!looks_like_question_or_hypothetical(
            "I live in Berlin. Is it sunny?"
        ));
        assert!(!looks_like_question_or_hypothetical(""));
    }
}
