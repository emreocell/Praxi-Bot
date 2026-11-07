//! Built-in plan / todo tool (07 #76): the model keeps a todo list that is
//! surfaced to clients as a [`TodoUpdated`] event.

use std::sync::Weak;

use async_trait::async_trait;
use protocol::event::{TodoItem, TodoStatus, TodoUpdated};
use protocol::{Event, PermissionCategory, RunId};
use serde::Deserialize;
use serde_json::{Value, json};
use tool_registry::{Effect, Platform, Tool, ToolContext, ToolError, ToolOutput, ToolSpec};
use uuid::Uuid;

use crate::shared::Shared;

/// Name of the built-in tool.
pub(crate) const TOOL_NAME: &str = "plan.todo_write";

#[derive(Deserialize)]
struct Input {
    todos: Vec<TodoItem>,
}

pub(crate) struct TodoTool {
    spec: ToolSpec,
    sh: Weak<Shared>,
}

impl TodoTool {
    pub fn new(sh: Weak<Shared>) -> Self {
        Self {
            spec: ToolSpec {
                name: TOOL_NAME.to_owned(),
                description: "Maintain your todo list for the current task. Send the complete \
                              list every time (it replaces the previous one); mark items \
                              pending, in_progress or done."
                    .to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "todos": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "text": { "type": "string" },
                                    "status": {
                                        "type": "string",
                                        "enum": ["pending", "in_progress", "done"]
                                    }
                                },
                                "required": ["text", "status"]
                            }
                        }
                    },
                    "required": ["todos"]
                }),
                // Sequential (never batched with reads); exempt from permission
                // checks by name because it only emits an event.
                effect: Effect::Write,
                category: PermissionCategory::Script,
                platforms: vec![Platform::All],
            },
            sh,
        }
    }
}

#[async_trait]
impl Tool for TodoTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolContext, input: Value) -> Result<ToolOutput, ToolError> {
        let parsed: Input =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let done = parsed
            .todos
            .iter()
            .filter(|t| t.status == TodoStatus::Done)
            .count();
        let total = parsed.todos.len();
        if let Some(sh) = self.sh.upgrade()
            && let Ok(id) = ctx.run_id.parse::<Uuid>()
        {
            let run_id = RunId(id);
            let rc = sh.runs.lock().get(&run_id).cloned();
            if let Some(rc) = rc {
                rc.state.lock().todos.clone_from(&parsed.todos);
                sh.emit(
                    &rc,
                    Event::TodoUpdated(TodoUpdated {
                        run_id,
                        items: parsed.todos,
                    }),
                );
            }
        }
        Ok(ToolOutput::text(format!(
            "Todo list updated: {done}/{total} done."
        )))
    }
}
