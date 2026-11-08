//! Memory hook (ADR-0017): after a successful turn the exchange is handed to
//! the memory pipeline in a background task.  Recall (before the turn) lives
//! in `exec.rs`.

use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use memory_store::{
    Candidate, Decision, ExtractionContext, Memory, MemoryError, MemoryModel, Pipeline, prompts,
};
use protocol::{BotId, ChatId, InitiatorKind, PermissionCategory, UserId};
use provider_catalog::{ChatChunk, ChatRequest, Message, MessageRole, ModelRef};
use serde_json::Value;
use storage::Run;
use uuid::Uuid;

use crate::shared::{STEP_MODEL, STEP_TOOLS, STEP_WORKFLOW, Shared, initiator_of};

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
        let provider = self
            .sh
            .providers
            .read()
            .pick(&self.model)
            .map_err(|e| MemoryError::Model(e.to_string()))?;
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
    /// The turn was not a plain human conversation (channel/schedule/subagent
    /// initiator) or it used a tool that can bring in outside content (web,
    /// browser, files, MCP, workflows, subagents): everything extracted from
    /// it is stored as `trust = untrusted`.
    pub untrusted: bool,
}

/// Whether the steps of a run show contact with content the user did not
/// write.  Only `Meta` tools (which have no external effect) keep a run
/// clean; `use_tool` hides the real tool, and unknown names cannot be vouched
/// for, so both count as outside content.
fn steps_touch_outside_content(steps: &[storage::RunStep], is_meta: impl Fn(&str) -> bool) -> bool {
    steps.iter().any(|s| match s.name.as_str() {
        // CLI tool execution is inside the session's model step. Treat its
        // exchange conservatively as outside content, even without tool rows.
        STEP_MODEL => s
            .output
            .as_ref()
            .is_some_and(|o| o["model"].as_str().is_some_and(|m| m.starts_with("cli:"))),
        STEP_TOOLS => s.input["calls"].as_array().is_some_and(|calls| {
            calls.iter().any(|c| {
                let name = c["name"].as_str().unwrap_or_default();
                name == agent_router::USE_TOOL || !is_meta(name)
            })
        }),
        STEP_WORKFLOW => true,
        _ => false,
    })
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
        let untrusted = initiator_of(&run.kind) != InitiatorKind::Human
            || sh.storage.list_steps(run.id).map_or(true, |steps| {
                steps_touch_outside_content(&steps, |name| {
                    sh.tools
                        .get(name)
                        .is_some_and(|t| t.spec().category == PermissionCategory::Meta)
                })
            })
            || sh
                .storage
                .list_child_runs(run.id)
                .map_or(true, |k| !k.is_empty());
        Some(Self {
            user_id: run.user_id,
            bot_id: run.bot_id,
            chat_id,
            excerpt: sh.redact(format!(
                "User: {}\nAssistant: {final_text}",
                users.join("\nUser: ")
            )),
            untrusted,
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
        untrusted: ex.untrusted,
    };
    let _ = Pipeline::new(&store, &model).ingest(&ctx).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn step(name: &str, input: Value) -> storage::RunStep {
        storage::RunStep {
            id: Uuid::nil(),
            run_id: protocol::RunId::new(),
            user_id: UserId::new(),
            idx: 0,
            name: name.to_owned(),
            input,
            idempotent: true,
            retry: None,
            status: "succeeded".to_owned(),
            attempt: 1,
            output: None,
            error: None,
            started_at_ms: 0,
            finished_at_ms: None,
        }
    }

    #[test]
    fn only_meta_tools_keep_an_exchange_trusted() {
        let is_meta = |n: &str| n == "ask_user";
        let calls = |names: &[&str]| {
            step(
                STEP_TOOLS,
                json!({ "calls": names.iter().map(|n| json!({ "name": n })).collect::<Vec<_>>() }),
            )
        };
        assert!(!steps_touch_outside_content(&[], is_meta));
        assert!(!steps_touch_outside_content(
            &[calls(&["ask_user"])],
            is_meta
        ));
        assert!(steps_touch_outside_content(
            &[calls(&["web_fetch"])],
            is_meta
        ));
        assert!(steps_touch_outside_content(
            &[calls(&["ask_user", "mcp.x"])],
            is_meta
        ));
        assert!(steps_touch_outside_content(&[calls(&["use_tool"])], |_| {
            true
        }));
        assert!(steps_touch_outside_content(
            &[step(STEP_WORKFLOW, json!({}))],
            is_meta
        ));
        let mut cli = step(STEP_MODEL, json!({}));
        cli.output = Some(json!({"model":"cli:claude"}));
        assert!(steps_touch_outside_content(&[cli], is_meta));
    }
}
