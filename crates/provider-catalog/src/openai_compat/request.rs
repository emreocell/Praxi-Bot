//! Mapping from [`ChatRequest`] to an `OpenAI` `/chat/completions` JSON body.

use std::collections::HashMap;

use serde_json::{Map, Value, json};

use crate::{
    entry::ProviderEntry,
    error::ProviderError,
    provider::{ChatRequest, Message, MessageRole, ToolChoice},
    util::{invalid_arg_calls, sanitize_args, with_args_note},
};

fn role_str(role: &MessageRole) -> &'static str {
    match role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
    }
}

/// Plain text of a message content (string, or the text parts of a parts array).
fn content_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn has_image(content: &Value) -> bool {
    content.as_array().is_some_and(|parts| {
        parts
            .iter()
            .any(|p| p.get("type").and_then(Value::as_str) == Some("image_url"))
    })
}

fn map_message(
    m: &Message,
    role: &str,
    vision: bool,
    bad_args: &HashMap<&str, &str>,
) -> Result<Value, ProviderError> {
    if !vision && has_image(&m.content) {
        return Err(ProviderError::InvalidRequest(
            "image content sent to a model without vision capability".into(),
        ));
    }
    let mut obj = Map::new();
    obj.insert("role".into(), role.into());
    if m.role == MessageRole::Assistant && !m.tool_calls.is_empty() {
        let content = if m.content.is_null() || content_text(&m.content).is_empty() {
            Value::Null
        } else {
            m.content.clone()
        };
        obj.insert("content".into(), content);
        let calls: Vec<Value> = m
            .tool_calls
            .iter()
            .map(|c| {
                let mut call = json!({
                    "id": c.id,
                    "type": "function",
                    "function": {"name": c.name, "arguments": sanitize_args(&c.arguments)},
                });
                // Google's thought signature must be echoed back verbatim.
                if let Some(extra) = &c.provider_extra {
                    call["extra_content"] = extra.clone();
                }
                call
            })
            .collect();
        obj.insert("tool_calls".into(), Value::Array(calls));
    } else if m.role == MessageRole::Tool {
        let mut text = content_text(&m.content);
        if let Some(original) = m.tool_call_id.as_deref().and_then(|id| bad_args.get(id)) {
            text = with_args_note(&text, original);
        }
        obj.insert("content".into(), Value::String(text));
        obj.insert(
            "tool_call_id".into(),
            m.tool_call_id.clone().unwrap_or_default().into(),
        );
    } else {
        obj.insert("content".into(), m.content.clone());
    }
    Ok(Value::Object(obj))
}

fn map_messages(entry: &ProviderEntry, req: &ChatRequest) -> Result<Vec<Value>, ProviderError> {
    let flags = &entry.compat_flags;
    let vision = entry.capabilities.vision;
    let bad_args = invalid_arg_calls(&req.messages);

    if flags.strip_system_role {
        let system: Vec<String> = req
            .messages
            .iter()
            .filter(|m| m.role == MessageRole::System)
            .map(|m| content_text(&m.content))
            .collect();
        let system = system.join("\n\n");
        let mut out = Vec::new();
        let mut merged = system.is_empty();
        for m in req
            .messages
            .iter()
            .filter(|m| m.role != MessageRole::System)
        {
            let mut v = map_message(m, role_str(&m.role), vision, &bad_args)?;
            if !merged && m.role == MessageRole::User {
                prepend_text(&mut v, &system);
                merged = true;
            }
            out.push(v);
        }
        if !merged {
            out.insert(0, json!({"role": "user", "content": system}));
        }
        return Ok(out);
    }

    req.messages
        .iter()
        .map(|m| {
            let role = if m.role == MessageRole::System && flags.system_as_user {
                "user"
            } else {
                role_str(&m.role)
            };
            map_message(m, role, vision, &bad_args)
        })
        .collect()
}

fn prepend_text(msg: &mut Value, prefix: &str) {
    match msg.get_mut("content") {
        Some(Value::String(s)) => *s = format!("{prefix}\n\n{s}"),
        Some(Value::Array(parts)) => {
            parts.insert(0, json!({"type": "text", "text": prefix}));
        }
        _ => msg["content"] = Value::String(prefix.to_owned()),
    }
}

fn map_tools(entry: &ProviderEntry, tools: &[Value]) -> Vec<Value> {
    if !entry.compat_flags.tools_require_strict {
        return tools.to_vec();
    }
    tools
        .iter()
        .cloned()
        .map(|mut t| {
            if let Some(f) = t.get_mut("function").and_then(Value::as_object_mut) {
                f.insert("strict".into(), Value::Bool(true));
                if let Some(p) = f.get_mut("parameters").and_then(Value::as_object_mut) {
                    p.entry("additionalProperties")
                        .or_insert(Value::Bool(false));
                }
            }
            t
        })
        .collect()
}

/// Build the JSON request body for `model`.
///
/// # Errors
///
/// [`ProviderError::InvalidRequest`] when the request needs a capability the
/// model entry does not declare (images without `vision`, tools without
/// `tools`).
pub(super) fn build_body(
    entry: &ProviderEntry,
    model: &str,
    req: &ChatRequest,
    stream: bool,
) -> Result<Value, ProviderError> {
    let flags = &entry.compat_flags;
    if !req.tools.is_empty() && !entry.capabilities.tools {
        return Err(ProviderError::InvalidRequest(
            "tools sent to a model without tool-calling capability".into(),
        ));
    }

    let mut body = Map::new();
    body.insert("model".into(), model.into());
    body.insert("messages".into(), Value::Array(map_messages(entry, req)?));
    body.insert("stream".into(), Value::Bool(stream));
    if stream && !flags.no_stream_usage {
        body.insert("stream_options".into(), json!({"include_usage": true}));
    }
    if let Some(t) = req.temperature {
        body.insert("temperature".into(), json!(t));
    }
    if let Some(n) = req.max_tokens {
        let field = flags.max_tokens_field.as_deref().unwrap_or("max_tokens");
        body.insert(field.into(), n.into());
    }
    if !req.tools.is_empty() {
        body.insert("tools".into(), Value::Array(map_tools(entry, &req.tools)));
        if flags.no_parallel_tool_calls {
            body.insert("parallel_tool_calls".into(), Value::Bool(false));
        }
        match &req.tool_choice {
            None => {}
            Some(ToolChoice::Auto) => {
                body.insert("tool_choice".into(), "auto".into());
            }
            Some(ToolChoice::None) => {
                body.insert("tool_choice".into(), "none".into());
            }
            Some(ToolChoice::Required) => {
                body.insert("tool_choice".into(), "required".into());
            }
            Some(ToolChoice::Function(name)) => {
                body.insert(
                    "tool_choice".into(),
                    json!({"type": "function", "function": {"name": name}}),
                );
            }
        }
    }
    if req.json_mode && entry.capabilities.json_mode {
        body.insert("response_format".into(), json!({"type": "json_object"}));
    }
    Ok(Value::Object(body))
}
