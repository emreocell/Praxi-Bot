//! Wrapping remote MCP tools as [`tool_registry::Tool`]s.

use std::sync::Arc;

use async_trait::async_trait;
use rmcp::model::{CallToolResult, Tool as McpTool};
use serde_json::{Value, json};
use tool_registry::{
    ContentPart, Effect, PermissionCategory, Platform, Tool, ToolContext, ToolError, ToolOutput,
    ToolSpec,
};

use crate::manager::Server;

/// Registry name of a remote tool: `mcp:<server>/<tool>`.
#[must_use]
pub fn qualified_name(server: &str, tool: &str) -> String {
    format!("mcp:{server}/{tool}")
}

/// Effect of a remote tool.  Per ADR-0004 the default is `Destructive`; only
/// an explicit MCP `readOnlyHint = true` lowers it to `Read`.  (Annotations
/// are hints from a possibly untrusted server — the permission engine still
/// decides, `Read` merely avoids needless prompts for honest servers.)
pub(crate) fn effect_of(tool: &McpTool) -> Effect {
    match tool.annotations.as_ref().and_then(|a| a.read_only_hint) {
        Some(true) => Effect::Read,
        _ => Effect::Destructive,
    }
}

/// Make sure the schema is a JSON-Schema object with `type` and `properties`.
fn normalise_schema(schema: &serde_json::Map<String, Value>) -> Value {
    let mut schema = schema.clone();
    schema
        .entry("type")
        .or_insert_with(|| Value::String("object".into()));
    schema.entry("properties").or_insert_with(|| json!({}));
    Value::Object(schema)
}

pub(crate) fn spec_of(server: &str, tool: &McpTool) -> ToolSpec {
    let description = tool
        .description
        .as_deref()
        .filter(|d| !d.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| tool.title.clone())
        .unwrap_or_else(|| format!("MCP tool `{}` of server `{server}`", tool.name));
    ToolSpec {
        name: qualified_name(server, &tool.name),
        description,
        input_schema: normalise_schema(&tool.input_schema),
        effect: effect_of(tool),
        category: PermissionCategory::Accounts,
        platforms: vec![Platform::All],
    }
}

/// A remote tool living in the registry.
pub(crate) struct McpToolAdapter {
    spec: ToolSpec,
    remote_name: String,
    server: Arc<Server>,
}

impl McpToolAdapter {
    pub(crate) fn new(server: Arc<Server>, tool: &McpTool) -> Self {
        Self {
            spec: spec_of(server.id(), tool),
            remote_name: tool.name.to_string(),
            server,
        }
    }
}

#[async_trait]
impl Tool for McpToolAdapter {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, _ctx: &ToolContext, input: Value) -> Result<ToolOutput, ToolError> {
        let arguments = match input {
            Value::Object(map) => Some(map),
            Value::Null => None,
            other => {
                return Err(ToolError::InvalidInput(format!(
                    "MCP tool input must be a JSON object, got {other}"
                )));
            }
        };
        let result = self
            .server
            .call(&self.remote_name, arguments)
            .await
            .map_err(|e| ToolError::Execution(e.to_string()))?;
        into_output(result)
    }
}

/// Convert a remote result; `isError` results become [`ToolError::Execution`].
pub(crate) fn into_output(result: CallToolResult) -> Result<ToolOutput, ToolError> {
    let mut parts = Vec::new();
    for block in &result.content {
        // Go through JSON so new/unknown block kinds degrade gracefully.
        let v = serde_json::to_value(block).unwrap_or(Value::Null);
        let kind = v.get("type").and_then(Value::as_str).unwrap_or_default();
        let str_field = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
        parts.push(match (kind, str_field("text"), str_field("data")) {
            ("text", Some(text), _) => ContentPart::Text { text },
            ("image" | "audio", _, Some(base64)) => ContentPart::Binary {
                mime_type: str_field("mimeType")
                    .unwrap_or_else(|| "application/octet-stream".into()),
                base64,
            },
            _ => ContentPart::Json { data: v },
        });
    }
    if result.is_error == Some(true) {
        let text: Vec<String> = parts
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text { text } => Some(text.clone()),
                _ => None,
            })
            .collect();
        let msg = if text.is_empty() {
            "the MCP tool reported an error".to_owned()
        } else {
            text.join("\n")
        };
        return Err(ToolError::Execution(msg));
    }
    if parts.is_empty()
        && let Some(data) = result.structured_content
    {
        parts.push(ContentPart::Json { data });
    }
    Ok(ToolOutput {
        content: parts,
        spill: None,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rmcp::model::{ContentBlock, ToolAnnotations};

    use super::*;

    fn tool(read_only: Option<bool>) -> McpTool {
        let schema = serde_json::Map::new();
        let mut t = McpTool::new("do_it", "Does it", Arc::new(schema));
        if let Some(r) = read_only {
            t.annotations = Some(ToolAnnotations::new().read_only(r));
        }
        t
    }

    #[test]
    fn effect_defaults_to_destructive_unless_read_only_hint() {
        assert_eq!(effect_of(&tool(None)), Effect::Destructive);
        assert_eq!(effect_of(&tool(Some(false))), Effect::Destructive);
        assert_eq!(effect_of(&tool(Some(true))), Effect::Read);
    }

    #[test]
    fn spec_has_qualified_name_and_normalised_schema() {
        let spec = spec_of("srv", &tool(Some(true)));
        assert_eq!(spec.name, "mcp:srv/do_it");
        assert_eq!(spec.input_schema["type"], "object");
        assert!(spec.input_schema["properties"].is_object());
        assert!(spec.is_valid());
    }

    #[test]
    fn missing_description_falls_back() {
        let mut t = tool(None);
        t.description = None;
        assert!(spec_of("srv", &t).is_valid());
    }

    #[test]
    fn results_map_to_content_parts_and_errors() {
        let ok = CallToolResult::success(vec![
            ContentBlock::text("hi"),
            ContentBlock::image("QQ==", "image/png"),
        ]);
        let out = into_output(ok).unwrap();
        assert!(matches!(&out.content[0], ContentPart::Text { text } if text == "hi"));
        assert!(
            matches!(&out.content[1], ContentPart::Binary { mime_type, base64 } if mime_type == "image/png" && base64 == "QQ==")
        );

        let err = CallToolResult::error(vec![ContentBlock::text("boom")]);
        assert!(matches!(into_output(err), Err(ToolError::Execution(m)) if m == "boom"));
    }
}
