//! Rebuilding the model conversation from persisted state, plus small text helpers.

use std::fmt::Write as _;

use provider_catalog::{Message, MessageRole, ToolCall};
use serde_json::{Value, json};
use storage::{Run, RunStep, Storage};
use task_runtime::StepStatus;
use tool_registry::ToolSpec;

use crate::profile::BotProfile;
use crate::router::workflow_note;
use crate::shared::{
    CallRec, ModelOut, ResultRec, STEP_MODEL, STEP_TOOLS, STEP_USER, STEP_WORKFLOW,
};

/// Providers (OpenAI-style) reject `.` in function names; expose `a.b` as `a__b`.
pub(crate) fn expose(name: &str) -> String {
    name.replace('.', "__")
}

pub(crate) fn status_of(step: &RunStep) -> Option<StepStatus> {
    StepStatus::parse(&step.status)
}

const GUIDANCE: &str = "You are an agent that can call tools. Call a tool when you need information or \
need to act; when the task is done, answer the user in plain text. If a tool result says it was \
denied, interrupted or truncated, adapt instead of repeating the same call.";

const SUBAGENT_GUIDANCE: &str = "\n\nYou are a subagent: another agent delegated one task to you. \
Do exactly that task with your tools, then reply with a concise final answer; the parent receives \
your final text as-is and nothing else.";

pub(crate) fn system_prompt(profile: &BotProfile, subagent: bool, skills: Option<&str>) -> String {
    let mut s = String::new();
    if !profile.persona.is_empty() {
        s.push_str(&profile.persona);
        s.push_str("\n\n");
    }
    if let Some(l) = &profile.language {
        let _ = write!(s, "Reply in the user's language (default: {l}).\n\n");
    }
    s.push_str(GUIDANCE);
    if subagent {
        s.push_str(SUBAGENT_GUIDANCE);
    }
    if let Some(block) = skills.filter(|b| !b.is_empty()) {
        s.push_str("\n\n");
        s.push_str(block);
    }
    s
}

pub(crate) fn tool_defs(specs: &[ToolSpec]) -> Vec<Value> {
    specs
        .iter()
        .map(|s| {
            json!({
                "type": "function",
                "function": {
                    "name": expose(&s.name),
                    "description": s.description,
                    "parameters": s.input_schema,
                }
            })
        })
        .collect()
}

/// Note appended to the closing, tool-less model call after a guard stopped the turn.
pub(crate) const GUARD_NOTE: &str = "The turn was stopped by a safety limit ({reason}). Tools are no longer available. Summarize the progress made so far and what is left to do, in plain text.";

const NOT_EXECUTED: &str = "[not executed: the turn was stopped by a safety limit]";

/// Full provider conversation for the next model call of `run`.
/// `final_note` (guard stop) is appended as a closing system message;
/// `skills` is the skills index block of the system prompt.  A subagent run
/// starts from a fresh history: no chat messages.
pub(crate) fn build_messages(
    storage: &Storage,
    run: &Run,
    steps: &[RunStep],
    profile: &BotProfile,
    final_note: Option<&str>,
    skills: Option<&str>,
) -> Result<Vec<Message>, storage::StorageError> {
    let subagent = run.parent_run_id.is_some();
    let mut out = vec![Message::text(
        MessageRole::System,
        system_prompt(profile, subagent, skills),
    )];
    if let Some(chat) = run.chat_id.filter(|_| !subagent) {
        for m in storage.list_messages(chat)? {
            if m.run_id == Some(run.id) {
                continue;
            }
            match m.role.as_str() {
                "user" => out.push(Message::text(MessageRole::User, m.content)),
                "assistant" => out.push(Message::text(MessageRole::Assistant, m.content)),
                _ => {}
            }
        }
    }
    // Tool calls of the latest model step that have no result yet; a stopped
    // turn leaves some, and providers reject unanswered tool calls.
    let mut outstanding: Vec<String> = Vec::new();
    let flush = |out: &mut Vec<Message>, outstanding: &mut Vec<String>| {
        for id in outstanding.drain(..) {
            out.push(Message::tool_result(id, NOT_EXECUTED));
        }
    };
    for step in steps {
        match step.name.as_str() {
            STEP_USER => {
                flush(&mut out, &mut outstanding);
                let text = step.input["text"].as_str().unwrap_or_default();
                out.push(Message::text(MessageRole::User, text));
            }
            STEP_WORKFLOW => {
                // The router replayed a saved workflow that stopped early:
                // tell the model where it stopped, in the user's message.
                if let Some(note) = workflow_note(step)
                    && let Some(m) = out.iter_mut().rev().find(|m| m.role == MessageRole::User)
                    && let Value::String(s) = &mut m.content
                {
                    s.push_str(&note);
                }
            }
            STEP_MODEL => {
                if status_of(step) != Some(StepStatus::Succeeded) {
                    continue;
                }
                let Some(mo) = step_output::<ModelOut>(step) else {
                    continue;
                };
                flush(&mut out, &mut outstanding);
                outstanding = mo.tool_calls.iter().map(|c| c.id.clone()).collect();
                let content = if mo.text.is_empty() {
                    Value::Null
                } else {
                    Value::String(mo.text)
                };
                out.push(Message {
                    role: MessageRole::Assistant,
                    content,
                    tool_calls: mo
                        .tool_calls
                        .into_iter()
                        .map(|c| ToolCall {
                            name: expose(&c.name),
                            ..c
                        })
                        .collect(),
                    tool_call_id: None,
                });
            }
            STEP_TOOLS => {
                let calls: Vec<CallRec> =
                    serde_json::from_value(step.input["calls"].clone()).unwrap_or_default();
                let results: Vec<ResultRec> = step
                    .output
                    .as_ref()
                    .and_then(|o| serde_json::from_value(o["results"].clone()).ok())
                    .unwrap_or_default();
                for call in calls {
                    let text = results.iter().find(|r| r.id == call.id).map_or_else(
                        || {
                            "[interrupted: the process stopped while this call was in flight; \
                             it may or may not have taken effect, check before retrying]"
                                .to_owned()
                        },
                        |r| r.text.clone(),
                    );
                    outstanding.retain(|i| *i != call.id);
                    out.push(Message::tool_result(call.id, text));
                }
            }
            _ => {}
        }
    }
    flush(&mut out, &mut outstanding);
    if let Some(note) = final_note {
        out.push(Message::text(MessageRole::System, note));
    }
    Ok(out)
}

pub(crate) fn step_output<T: serde::de::DeserializeOwned>(step: &RunStep) -> Option<T> {
    step.output
        .as_ref()
        .and_then(|o| serde_json::from_value(o.clone()).ok())
}

/// Longest prefix of `s` that is at most `max` bytes and ends on a char boundary.
pub(crate) fn truncate_utf8(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_on_char_boundary() {
        assert_eq!(truncate_utf8("héllo", 2), "h");
        assert_eq!(truncate_utf8("héllo", 3), "hé");
        assert_eq!(truncate_utf8("abc", 10), "abc");
    }

    #[test]
    fn exposes_dotted_names() {
        assert_eq!(expose("fs.read_file"), "fs__read_file");
    }
}
