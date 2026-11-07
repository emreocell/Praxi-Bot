//! Memory hook (ADR-0017): after a successful turn the exchange is handed to
//! the memory pipeline in a background task.  Recall (before the turn) lives
//! in `exec.rs`.

use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use memory_store::{
    Candidate, Decision, ExtractionContext, Memory, MemoryError, MemoryModel, Pipeline, prompts,
};
use protocol::{BotId, ChatId, UserId};
use provider_catalog::{ChatChunk, ChatRequest, Message, MessageRole, ModelRef};
use serde_json::Value;
use storage::Run;
use uuid::Uuid;

use crate::shared::Shared;

/// [`MemoryModel`] over the bot's classifier model (falling back to its
/// executor), using the prompt templates shipped with `memory-store`.
struct LoopMemoryModel {
    sh: Arc<Shared>,
    model: ModelRef,
}

impl LoopMemoryModel {
    async fn complete(&self, prompt: String) -> Result<String, MemoryError> {
        let req = ChatRequest::new(
            self.model.clone(),
            vec![Message::text(MessageRole::User, prompt)],
        );
        let provider = self.sh.providers.read().pick(&self.model);
        let mut stream = provider
            .stream(req)
            .await
            .map_err(|e| MemoryError::Model(e.to_string()))?;
        let mut out = String::new();
        while let Some(chunk) = stream.next().await {
            match chunk.map_err(|e| MemoryError::Model(e.to_string()))? {
                ChatChunk::TextDelta(t) => out.push_str(&t),
                ChatChunk::Done { .. } => break,
                _ => {}
            }
        }
        Ok(out)
    }
}

/// The span of `text` from the first `open` to the last `close`.
fn json_span(text: &str, open: char, close: char) -> Option<&str> {
    let a = text.find(open)?;
    let b = text.rfind(close)?;
    (b > a).then(|| &text[a..=b])
}

#[async_trait]
impl MemoryModel for LoopMemoryModel {
    async fn extract(&self, chat_excerpt: &str) -> memory_store::Result<Vec<Candidate>> {
        let reply = self.complete(prompts::render_extract(chat_excerpt)).await?;
        let Some(span) = json_span(&reply, '[', ']') else {
            return Ok(Vec::new());
        };
        Ok(serde_json::from_str(span)?)
    }

    async fn consolidate(
        &self,
        candidate: &Candidate,
        similar: &[Memory],
    ) -> memory_store::Result<Decision> {
        let similar_text = similar
            .iter()
            .map(|m| format!("- id {}: [{}] {}", m.id, m.kind.as_str(), m.content))
            .collect::<Vec<_>>()
            .join("\n");
        let reply = self
            .complete(prompts::render_consolidate(
                &serde_json::to_string(candidate)?,
                &similar_text,
            ))
            .await?;
        let Some(v) =
            json_span(&reply, '{', '}').and_then(|s| serde_json::from_str::<Value>(s).ok())
        else {
            return Ok(Decision::Skip);
        };
        let id = v["id"].as_str().and_then(|s| s.parse::<Uuid>().ok());
        Ok(match (v["action"].as_str(), id) {
            (Some("add"), _) => Decision::Add,
            (Some("update"), Some(id)) => Decision::Update(id),
            (Some("delete"), Some(id)) => Decision::Delete(id),
            _ => Decision::Skip,
        })
    }
}

/// Everything the extraction of one finished turn needs.
pub(crate) struct Exchange {
    pub user_id: UserId,
    pub bot_id: BotId,
    pub chat_id: ChatId,
    pub excerpt: String,
}

impl Exchange {
    /// `None` when there is nothing worth extracting from.
    pub fn of(sh: &Shared, run: &Run, final_text: &str) -> Option<Self> {
        let chat_id = run.chat_id?;
        let users: Vec<String> = sh
            .storage
            .list_messages(chat_id)
            .ok()?
            .into_iter()
            .filter(|m| m.run_id == Some(run.id) && m.role == "user")
            .map(|m| m.content)
            .collect();
        if users.is_empty() {
            return None;
        }
        Some(Self {
            user_id: run.user_id,
            bot_id: run.bot_id,
            chat_id,
            excerpt: format!("User: {}\nAssistant: {final_text}", users.join("\nUser: ")),
        })
    }
}

/// Run extraction + consolidation for one exchange (errors are dropped:
/// memory must never affect a turn).
pub(crate) async fn extract(sh: Arc<Shared>, ex: Exchange) {
    let Some(cfg) = sh.cfg.memory.as_ref().filter(|m| m.enabled) else {
        return;
    };
    let store = Arc::clone(&cfg.store);
    let Ok(profile) = sh.profile_for(ex.bot_id) else {
        return;
    };
    let model = LoopMemoryModel {
        model: ModelRef(
            profile
                .models
                .classifier
                .clone()
                .unwrap_or_else(|| profile.models.executor.clone()),
        ),
        sh: Arc::clone(&sh),
    };
    let ctx = ExtractionContext {
        user_id: ex.user_id,
        bot_id: ex.bot_id,
        chat_id: ex.chat_id,
        message_id: None,
        excerpt: ex.excerpt,
        untrusted: false,
    };
    let _ = Pipeline::new(&store, &model).ingest(&ctx).await;
}
