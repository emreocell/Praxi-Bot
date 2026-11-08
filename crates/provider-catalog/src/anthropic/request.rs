//! Mapping from [`ChatRequest`] to an Anthropic Messages API JSON body.

use serde_json::{Map, Value, json};

use crate::{
    entry::ProviderEntry,
    error::ProviderError,
    provider::{ChatRequest, Message, MessageRole, ToolChoice},
    util::{
        content_text, has_image, image_url, parse_args, parse_data_url, push_merged, tool_parts,
    },
};

/// `max_tokens` is mandatory in this API; used when the request has none.
pub(super) const DEFAULT_MAX_TOKENS: u32 = 8192;

fn cache_marker() -> Value {
    json!({"type": "ephemeral"})
}

fn set_cache_control(block: &mut Value) {
    if let Some(o) = block.as_object_mut() {
        o.insert("cache_control".into(), cache_marker());
    }
}

fn text_block(text: &str) -> Value {
    json!({"type": "text", "text": text})
}

fn image_block(part: &Value) -> Result<Value, ProviderError> {
    let url = image_url(part)
        .ok_or_else(|| ProviderError::InvalidRequest("image part without a URL".into()))?;
    if let Some((media_type, data)) = parse_data_url(url) {
        Ok(json!({
            "type": "image",
            "source": {"type": "base64", "media_type": media_type, "data": data},
        }))
    } else {
        Ok(json!({"type": "image", "source": {"type": "url", "url": url}}))
    }
}

fn user_blocks(content: &Value) -> Result<Vec<Value>, ProviderError> {
    match content {
        Value::String(s) if !s.is_empty() => Ok(vec![text_block(s)]),
        Value::Array(parts) => {
            let mut out = Vec::new();
            for p in parts {
                match p.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(t) = p.get("text").and_then(Value::as_str)
                            && !t.is_empty()
                        {
                            out.push(text_block(t));
                        }
                    }
                    Some("image_url") => out.push(image_block(p)?),
                    // Native Anthropic blocks are passed through.
                    Some("image" | "document") => out.push(p.clone()),
                    _ => {}
                }
            }
            Ok(out)
        }
        _ => Ok(Vec::new()),
    }
}

fn assistant_blocks(m: &Message) -> Vec<Value> {
    let mut out = Vec::new();
    let text = content_text(&m.content);
    if !text.is_empty() {
        out.push(text_block(&text));
    }
    for c in &m.tool_calls {
        out.push(json!({
            "type": "tool_use",
            "id": c.id,
            "name": c.name,
            "input": parse_args(&c.arguments),
        }));
    }
    out
}

fn tool_result_block(m: &Message) -> Value {
    let mut b = json!({
        "type": "tool_result",
        "tool_use_id": m.tool_call_id.clone().unwrap_or_default(),
    });
    let text = content_text(&m.content);
    if !text.is_empty() {
        b["content"] = Value::String(text);
    }
    b
}

type Turns = Vec<(&'static str, Vec<Value>)>;

/// Returns `(system blocks, messages)`.
fn convert_messages(req: &ChatRequest, vision: bool) -> Result<(Vec<Value>, Turns), ProviderError> {
    let mut system = Vec::new();
    let mut out: Turns = Vec::new();
    for m in &req.messages {
        if !vision && has_image(&m.content) {
            return Err(ProviderError::Unsupported {
                capability: "vision".into(),
            });
        }
        match m.role {
            MessageRole::System => {
                let t = content_text(&m.content);
                if !t.is_empty() {
                    system.push(text_block(&t));
                }
            }
            MessageRole::User => push_merged(&mut out, "user", user_blocks(&m.content)?),
            MessageRole::Assistant => push_merged(&mut out, "assistant", assistant_blocks(m)),
            // All results of one assistant turn must share one user message.
            MessageRole::Tool => push_merged(&mut out, "user", vec![tool_result_block(m)]),
        }
    }
    // `tool_result` blocks must precede any other block in a user message.
    for (role, blocks) in &mut out {
        if *role == "user" {
            blocks.sort_by_key(|b| b.get("type").and_then(Value::as_str) != Some("tool_result"));
        }
    }
    Ok((system, out))
}

fn convert_tools(tools: &[Value]) -> Vec<Value> {
    tools
        .iter()
        .filter_map(tool_parts)
        .map(|(name, desc, params)| {
            let mut t = Map::new();
            t.insert("name".into(), name.into());
            if let Some(d) = desc {
                t.insert("description".into(), d.into());
            }
            let schema = match params {
                Some(p) if p.is_object() => p.clone(),
                _ => json!({"type": "object", "properties": {}}),
            };
            t.insert("input_schema".into(), schema);
            Value::Object(t)
        })
        .collect()
}

fn tool_choice(entry: &ProviderEntry, choice: Option<&ToolChoice>) -> Option<Value> {
    let mut v = match choice {
        None => None,
        Some(ToolChoice::Auto) => Some(json!({"type": "auto"})),
        Some(ToolChoice::None) => return Some(json!({"type": "none"})),
        Some(ToolChoice::Required) => Some(json!({"type": "any"})),
        Some(ToolChoice::Function(n)) => Some(json!({"type": "tool", "name": n})),
    };
    if entry.compat_flags.no_parallel_tool_calls {
        let o = v.get_or_insert_with(|| json!({"type": "auto"}));
        o["disable_parallel_tool_use"] = Value::Bool(true);
    }
    v
}

/// Build the JSON request body for `model`.
///
/// # Errors
/// [`ProviderError::Unsupported`] when the request needs a capability `model`
/// does not have (images without `vision`, tools without `tools`).
pub(super) fn build_body(
    entry: &ProviderEntry,
    model: &str,
    req: &ChatRequest,
    caching: bool,
) -> Result<Value, ProviderError> {
    let caps = entry.caps_for(model);
    if !req.tools.is_empty() && !caps.tools {
        return Err(ProviderError::Unsupported {
            capability: "tools".into(),
        });
    }
    let (mut system, messages) = convert_messages(req, caps.vision)?;
    let mut tools = convert_tools(&req.tools);

    let mut messages: Vec<Value> = messages
        .into_iter()
        .map(|(role, blocks)| json!({"role": role, "content": blocks}))
        .collect();

    if caching {
        if let Some(b) = system.last_mut() {
            set_cache_control(b);
        }
        if let Some(t) = tools.last_mut() {
            set_cache_control(t);
        }
        if let Some(b) = messages
            .last_mut()
            .and_then(|m| m.get_mut("content"))
            .and_then(Value::as_array_mut)
            .and_then(|c| c.last_mut())
        {
            set_cache_control(b);
        }
    }

    let mut body = Map::new();
    body.insert("model".into(), model.into());
    body.insert(
        "max_tokens".into(),
        req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS).into(),
    );
    body.insert("stream".into(), Value::Bool(true));
    if !system.is_empty() {
        body.insert("system".into(), Value::Array(system));
    }
    body.insert("messages".into(), Value::Array(messages));
    if let Some(t) = req.temperature {
        body.insert("temperature".into(), json!(t));
    }
    if !tools.is_empty() {
        body.insert("tools".into(), Value::Array(tools));
        if let Some(tc) = tool_choice(entry, req.tool_choice.as_ref()) {
            body.insert("tool_choice".into(), tc);
        }
    }
    Ok(Value::Object(body))
}
