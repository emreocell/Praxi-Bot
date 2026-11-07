//! Meta tools: `ask_user` (question card) and `todo_write` (plan list).
//!
//! Both return a [`ContentPart::Json`] part whose `kind` field tells the agent
//! loop how to treat it: [`QUESTION_CARD_KIND`] suspends the run until the user
//! answers; [`TODO_LIST_KIND`] updates the plan shown in the UI. No change to
//! `tool-registry` types is needed.

use std::fmt::Write as _;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tool_registry::{
    ContentPart, Effect, PermissionCategory, Tool, ToolContext, ToolError, ToolOutput, ToolSpec,
};

use crate::util::{parse, spec};

/// `kind` of the JSON part produced by `ask_user`.
pub const QUESTION_CARD_KIND: &str = "question_card";
/// `kind` of the JSON part produced by `todo_write`.
pub const TODO_LIST_KIND: &str = "todo_list";
/// Names of the meta tools (they have no external effect; the loop may skip
/// approval for them, see the crate docs).
pub const META_TOOL_NAMES: [&str; 2] = ["ask_user", "todo_write"];

/// Status of a todo item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    /// Not started.
    Pending,
    /// Being worked on.
    InProgress,
    /// Done.
    Completed,
}

/// One plan item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoItem {
    /// What to do.
    pub content: String,
    /// Progress.
    pub status: TodoStatus,
}

#[derive(Deserialize)]
struct AskIn {
    question: String,
    #[serde(default)]
    options: Vec<String>,
    #[serde(default)]
    allow_free_text: Option<bool>,
}

#[derive(Deserialize)]
struct TodoIn {
    items: Vec<TodoItem>,
}

#[derive(Clone, Copy)]
enum Op {
    Ask,
    Todo,
}

/// A meta tool.
pub struct MetaTool {
    spec: ToolSpec,
    op: Op,
}

/// Build `ask_user` and `todo_write`.
#[must_use]
pub fn tools() -> Vec<Arc<dyn Tool>> {
    // No permission category fits exactly; `Script` is the closest "agent-internal" one.
    let cat = PermissionCategory::Script;
    vec![
        Arc::new(MetaTool {
            spec: spec(
                "ask_user",
                "Ask the user a question and wait for the answer. Give 2-5 short options when possible. Example: {\"question\":\"Which file?\",\"options\":[\"a.txt\",\"b.txt\"]}",
                json!({"type":"object","properties":{
                    "question":{"type":"string","description":"One clear question."},
                    "options":{"type":"array","items":{"type":"string"},"maxItems":8,"description":"Suggested answers."},
                    "allow_free_text":{"type":"boolean","description":"Let the user type another answer (default true)."}
                },"required":["question"]}),
                Effect::Read,
                cat,
            ),
            op: Op::Ask,
        }),
        Arc::new(MetaTool {
            spec: spec(
                "todo_write",
                "Replace your plan list. Keep at most one item in_progress. Example: {\"items\":[{\"content\":\"Open site\",\"status\":\"in_progress\"}]}",
                json!({"type":"object","properties":{
                    "items":{"type":"array","items":{"type":"object","properties":{
                        "content":{"type":"string","description":"Step description."},
                        "status":{"type":"string","enum":["pending","in_progress","completed"]}
                    },"required":["content","status"]}}
                },"required":["items"]}),
                Effect::Write,
                cat,
            ),
            op: Op::Todo,
        }),
    ]
}

fn json_output(summary: String, data: Value) -> ToolOutput {
    ToolOutput {
        content: vec![
            ContentPart::Json { data },
            ContentPart::Text { text: summary },
        ],
        spill: None,
    }
}

#[async_trait]
impl Tool for MetaTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, _ctx: &ToolContext, input: Value) -> Result<ToolOutput, ToolError> {
        match self.op {
            Op::Ask => {
                let i: AskIn = parse(input)?;
                if i.question.trim().is_empty() {
                    return Err(ToolError::InvalidInput("question is empty".into()));
                }
                let options: Vec<String> = i
                    .options
                    .into_iter()
                    .filter(|o| !o.trim().is_empty())
                    .collect();
                let data = json!({
                    "kind": QUESTION_CARD_KIND,
                    "question": i.question,
                    "options": options,
                    "allow_free_text": i.allow_free_text.unwrap_or(true),
                });
                Ok(json_output(
                    "Question sent to the user. Wait for the answer.".into(),
                    data,
                ))
            }
            Op::Todo => {
                let i: TodoIn = parse(input)?;
                if i.items.iter().any(|t| t.content.trim().is_empty()) {
                    return Err(ToolError::InvalidInput("empty todo content".into()));
                }
                let active = i
                    .items
                    .iter()
                    .filter(|t| t.status == TodoStatus::InProgress)
                    .count();
                if active > 1 {
                    return Err(ToolError::InvalidInput(
                        "only one item may be in_progress".into(),
                    ));
                }
                let mut summary = String::from("Plan updated:\n");
                for t in &i.items {
                    let mark = match t.status {
                        TodoStatus::Pending => "[ ]",
                        TodoStatus::InProgress => "[~]",
                        TodoStatus::Completed => "[x]",
                    };
                    let _ = writeln!(summary, "{mark} {}", t.content);
                }
                let data = json!({"kind": TODO_LIST_KIND, "items": i.items});
                Ok(json_output(summary, data))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn by_name(name: &str) -> Arc<dyn Tool> {
        tools().into_iter().find(|t| t.spec().name == name).unwrap()
    }

    #[tokio::test]
    async fn ask_user_returns_question_card() {
        let out = by_name("ask_user")
            .call(
                &ToolContext::default(),
                json!({"question": "Which?", "options": ["a", " ", "b"]}),
            )
            .await
            .unwrap();
        let ContentPart::Json { data } = &out.content[0] else {
            panic!()
        };
        assert_eq!(data["kind"], QUESTION_CARD_KIND);
        assert_eq!(data["options"], json!(["a", "b"]));
        assert_eq!(data["allow_free_text"], true);
    }

    #[tokio::test]
    async fn todo_write_validates_and_renders() {
        let t = by_name("todo_write");
        let out = t
            .call(
                &ToolContext::default(),
                json!({"items": [
                    {"content": "one", "status": "completed"},
                    {"content": "two", "status": "in_progress"}
                ]}),
            )
            .await
            .unwrap();
        let ContentPart::Json { data } = &out.content[0] else {
            panic!()
        };
        assert_eq!(data["kind"], TODO_LIST_KIND);
        assert_eq!(data["items"].as_array().unwrap().len(), 2);

        let err = t
            .call(
                &ToolContext::default(),
                json!({"items": [
                    {"content": "a", "status": "in_progress"},
                    {"content": "b", "status": "in_progress"}
                ]}),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput(_)));
    }
}
