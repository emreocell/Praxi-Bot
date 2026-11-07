//! Mapping from [`ChatRequest`] to a Gemini `generateContent` JSON body.

use std::collections::HashMap;

use serde_json::{Map, Value, json};

use super::schema::convert_parameters;
use crate::{
    entry::ProviderEntry,
    error::ProviderError,
    provider::{ChatRequest, Message, MessageRole, ToolChoice},
    util::{
        content_text, has_image, image_url, invalid_arg_calls, parse_args, parse_data_url,
        push_merged, tool_parts, with_args_note,
    },
};

/// Length of the hex part of a synthesised call id (`gm<16 hex>[x<signature>]`).
const ID_HEX: usize = 16;

/// Build a tool-call id. Gemini calls carry no usable id, so one is synthesised;
/// the call's `thoughtSignature` (required by Gemini 3 when the call is sent
/// back) is carried inside the id as URL-safe base64, which survives storage in
/// the conversation history.
pub(super) fn make_call_id(unique: u64, signature: Option<&str>) -> String {
    let mut id = format!("gm{unique:016x}");
    if let Some(sig) = signature.filter(|s| !s.is_empty()) {
        id.push('x');
        id.extend(sig.chars().filter(|c| *c != '=').map(|c| match c {
            '+' => '-',
            '/' => '_',
            c => c,
        }));
    }
    id
}

/// Recover the standard-base64 `thoughtSignature` from a call id, if present.
pub(super) fn signature_of(id: &str) -> Option<String> {
    let rest = id.strip_prefix("gm")?.get(ID_HEX..)?;
    let sig = rest.strip_prefix('x')?;
    let mut s: String = sig
        .chars()
        .map(|c| match c {
            '-' => '+',
            '_' => '/',
            c => c,
        })
        .collect();
    while !s.len().is_multiple_of(4) {
        s.push('=');
    }
    Some(s)
}

fn text_part(text: &str) -> Value {
    json!({"text": text})
}

fn image_part(part: &Value) -> Result<Value, ProviderError> {
    let url = image_url(part)
        .ok_or_else(|| ProviderError::InvalidRequest("image part without a URL".into()))?;
    let (mime, data) = parse_data_url(url).ok_or_else(|| {
        ProviderError::InvalidRequest("Gemini images must be base64 `data:` URLs".into())
    })?;
    Ok(json!({"inlineData": {"mimeType": mime, "data": data}}))
}

fn user_parts(content: &Value) -> Result<Vec<Value>, ProviderError> {
    match content {
        Value::String(s) if !s.is_empty() => Ok(vec![text_part(s)]),
        Value::Array(parts) => {
            let mut out = Vec::new();
            for p in parts {
                match p.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(t) = p.get("text").and_then(Value::as_str)
                            && !t.is_empty()
                        {
                            out.push(text_part(t));
                        }
                    }
                    Some("image_url") => out.push(image_part(p)?),
                    _ => {}
                }
            }
            Ok(out)
        }
        _ => Ok(Vec::new()),
    }
}

fn model_parts(m: &Message) -> Vec<Value> {
    let mut out = Vec::new();
    let text = content_text(&m.content);
    if !text.is_empty() {
        out.push(text_part(&text));
    }
    for c in &m.tool_calls {
        let mut p = json!({"functionCall": {"name": c.name, "args": parse_args(&c.arguments)}});
        if let Some(sig) = signature_of(&c.id) {
            p["thoughtSignature"] = Value::String(sig);
        }
        out.push(p);
    }
    out
}

fn function_response(
    m: &Message,
    names: &HashMap<&str, &str>,
    bad_args: &HashMap<&str, &str>,
) -> Result<Value, ProviderError> {
    let id = m.tool_call_id.as_deref().unwrap_or_default();
    let name = names.get(id).ok_or_else(|| {
        ProviderError::InvalidRequest(format!("tool result for unknown call id {id:?}"))
    })?;
    let mut text = content_text(&m.content);
    if let Some(original) = bad_args.get(id) {
        text = with_args_note(&text, original);
    }
    // `response` must be a JSON object.
    let response = match serde_json::from_str::<Value>(&text) {
        Ok(v @ Value::Object(_)) => v,
        _ => json!({"output": text}),
    };
    Ok(json!({"functionResponse": {"name": name, "response": response}}))
}

fn convert_tools(tools: &[Value]) -> Vec<Value> {
    let decls: Vec<Value> = tools
        .iter()
        .filter_map(tool_parts)
        .map(|(name, desc, params)| {
            let mut d = Map::new();
            d.insert("name".into(), name.into());
            d.insert("description".into(), desc.unwrap_or_default().into());
            if let Some(p) = params.and_then(convert_parameters) {
                d.insert("parameters".into(), p);
            }
            Value::Object(d)
        })
        .collect();
    if decls.is_empty() {
        Vec::new()
    } else {
        vec![json!({"functionDeclarations": decls})]
    }
}

fn tool_config(choice: &ToolChoice) -> Value {
    let cfg = match choice {
        ToolChoice::Auto => json!({"mode": "AUTO"}),
        ToolChoice::None => json!({"mode": "NONE"}),
        ToolChoice::Required => json!({"mode": "ANY"}),
        ToolChoice::Function(n) => json!({"mode": "ANY", "allowedFunctionNames": [n]}),
    };
    json!({"functionCallingConfig": cfg})
}

/// Build the JSON request body.
///
/// # Errors
/// [`ProviderError::InvalidRequest`] for images without `vision`, tools without
/// `tools`, non-`data:` image URLs, or a tool result with an unknown call id.
pub(super) fn build_body(entry: &ProviderEntry, req: &ChatRequest) -> Result<Value, ProviderError> {
    if !req.tools.is_empty() && !entry.capabilities.tools {
        return Err(ProviderError::InvalidRequest(
            "tools sent to a model without tool-calling capability".into(),
        ));
    }
    let names: HashMap<&str, &str> = req
        .messages
        .iter()
        .flat_map(|m| &m.tool_calls)
        .map(|c| (c.id.as_str(), c.name.as_str()))
        .collect();

    let bad_args = invalid_arg_calls(&req.messages);
    let mut system = Vec::new();
    let mut contents: Vec<(&'static str, Vec<Value>)> = Vec::new();
    for m in &req.messages {
        if !entry.capabilities.vision && has_image(&m.content) {
            return Err(ProviderError::InvalidRequest(
                "image content sent to a model without vision capability".into(),
            ));
        }
        match m.role {
            MessageRole::System => {
                let t = content_text(&m.content);
                if !t.is_empty() {
                    system.push(text_part(&t));
                }
            }
            MessageRole::User => push_merged(&mut contents, "user", user_parts(&m.content)?),
            MessageRole::Assistant => push_merged(&mut contents, "model", model_parts(m)),
            // All responses to one turn's calls go into one `user` content.
            MessageRole::Tool => {
                push_merged(
                    &mut contents,
                    "user",
                    vec![function_response(m, &names, &bad_args)?],
                );
            }
        }
    }

    let mut body = Map::new();
    if !system.is_empty() {
        body.insert("systemInstruction".into(), json!({"parts": system}));
    }
    body.insert(
        "contents".into(),
        Value::Array(
            contents
                .into_iter()
                .map(|(role, parts)| json!({"role": role, "parts": parts}))
                .collect(),
        ),
    );
    let mut gen_cfg = Map::new();
    if let Some(t) = req.temperature {
        gen_cfg.insert("temperature".into(), json!(t));
    }
    if let Some(n) = req.max_tokens {
        gen_cfg.insert("maxOutputTokens".into(), n.into());
    }
    if req.json_mode && entry.capabilities.json_mode {
        gen_cfg.insert("responseMimeType".into(), "application/json".into());
    }
    if !gen_cfg.is_empty() {
        body.insert("generationConfig".into(), Value::Object(gen_cfg));
    }
    let tools = convert_tools(&req.tools);
    if !tools.is_empty() {
        body.insert("tools".into(), Value::Array(tools));
        if let Some(c) = &req.tool_choice {
            body.insert("toolConfig".into(), tool_config(c));
        }
    }
    Ok(Value::Object(body))
}
