//! Per-caller `skill_list`, `skill_read`, `skill_write` and `skill_publish` tools.

use std::fmt::Write as _;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};
use tool_registry::{
    ContentPart, Effect, PermissionCategory, Platform, Tool, ToolContext, ToolError, ToolOutput,
    ToolSpec,
};

use crate::index::SkillIndex;

/// Names of the skill tools.
pub const SKILL_TOOL_NAMES: [&str; 4] =
    ["skill_list", "skill_read", "skill_write", "skill_publish"];

#[derive(Clone, Copy)]
enum Op {
    List,
    Read,
    Write,
    Publish,
}

struct SkillTool {
    spec: ToolSpec,
    op: Op,
    index: SkillIndex,
}

fn mk_spec(
    name: &str,
    description: &str,
    input_schema: Value,
    effect: Effect,
    category: PermissionCategory,
) -> ToolSpec {
    ToolSpec {
        name: name.to_string(),
        description: description.to_string(),
        input_schema,
        effect,
        category,
        platforms: vec![Platform::All],
    }
}

fn str_arg<'a>(input: &'a Value, key: &str) -> Result<&'a str, ToolError> {
    input
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::InvalidInput(format!("`{key}` (string) is required")))
}

/// Build scoped tools over a registry [`SkillIndex`]. Every call resolves its
/// own bot view; `None` never means all bots.
#[must_use]
pub fn tools(index: &SkillIndex) -> Vec<Arc<dyn Tool>> {
    let defs = [
        (
            Op::List,
            mk_spec(
                "skill_list",
                "List skills visible to this bot (name, description, version, scope, source, extra files). Example: {}",
                json!({"type":"object","properties":{}}),
                Effect::Read,
                PermissionCategory::Meta,
            ),
        ),
        (
            Op::Read,
            mk_spec(
                "skill_read",
                "Load a visible skill and its version. Pass expected_version back to skill_write to update a per-bot skill. Example: {\"name\":\"custom-dropdowns\"}",
                json!({"type":"object","properties":{
                    "name":{"type":"string","description":"Skill name from the skills index."},
                    "file":{"type":"string","description":"Extra file inside the skill directory (relative path), as referenced by the skill."}
                },"required":["name"]}),
                Effect::Read,
                PermissionCategory::Meta,
            ),
        ),
        (
            Op::Write,
            mk_spec(
                "skill_write",
                "Save a learned skill only to this bot's folder. Missing/null expected_version creates only; to update, pass the per-bot version returned by skill_read. A save bumps that version. Content is a full SKILL.md (frontmatter and markdown body). Example: {\"name\":\"my-site\",\"content\":\"---\nname: my-site\ndescription: How to use my-site\n---\nSteps...\"}",
                json!({"type":"object","properties":{
                    "name":{"type":"string","description":"Skill name: a-z, 0-9, '-', '_', '.'."},
                    "content":{"type":"string","description":"Complete SKILL.md text."},
                    "expected_version":{"type":["string","null"],"description":"Current per-bot version returned by skill_read; omit/null only when creating a new bot skill."}
                },"required":["name","content"]}),
                Effect::Write,
                PermissionCategory::Fs,
            ),
        ),
        (
            Op::Publish,
            mk_spec(
                "skill_publish",
                "Publish this bot's skill to the shared scope, where other bots can read it. Requires write approval. A different shared skill is kept unless replace is explicitly true. A name that matches a built-in skill is refused unless replace is true, which replaces the built-in skill for all bots. Example: {\"name\":\"my-site\"}",
                json!({"type":"object","properties":{
                    "name":{"type":"string","description":"Name of a skill in this bot's own scope."},
                    "replace":{"type":"boolean","default":false,"description":"Explicitly replace a different shared skill, or a built-in skill (for all bots)."}
                },"required":["name"]}),
                Effect::Write,
                PermissionCategory::Fs,
            ),
        ),
    ];
    defs.into_iter()
        .map(|(op, spec)| {
            Arc::new(SkillTool {
                spec,
                op,
                index: index.clone(),
            }) as Arc<dyn Tool>
        })
        .collect()
}

fn list_output(index: &SkillIndex) -> ToolOutput {
    let skills = index.list();
    let mut text = String::new();
    let mut data = Vec::new();
    for s in &skills {
        let files = index.extra_files(&s.name);
        let _ = writeln!(
            text,
            "- {} (v{}, {}): {}",
            s.name,
            s.version,
            s.source.scope(),
            s.description
        );
        data.push(json!({
            "name": s.name, "description": s.description, "version": s.version,
            "source": s.source.label(), "scope": s.source.scope(), "files": files,
        }));
    }
    for i in index.issues() {
        let _ = writeln!(text, "! skipped {}: {}", i.path, i.message);
    }
    if text.is_empty() {
        text.push_str("No skills available.");
    }
    ToolOutput {
        content: vec![
            ContentPart::Json {
                data: Value::Array(data),
            },
            ContentPart::Text { text },
        ],
        spill: None,
    }
}

#[async_trait]
impl Tool for SkillTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolContext, input: Value) -> Result<ToolOutput, ToolError> {
        let index = self
            .index
            .for_bot(ctx.bot_id.as_deref())
            .map_err(ToolError::InvalidInput)?;
        match self.op {
            Op::List => Ok(list_output(&index)),
            Op::Read => {
                let name = str_arg(&input, "name")?;
                let file = input.get("file").and_then(Value::as_str);
                let text = index.read(name, file).map_err(ToolError::Execution)?;
                let skill = index
                    .get(name)
                    .ok_or_else(|| ToolError::Execution("skill disappeared".into()))?;
                Ok(ToolOutput {
                    content: vec![
                        ContentPart::Json {
                            data: json!({
                                "name": skill.name,
                                "version": skill.version,
                                "scope": skill.source.scope(),
                                "expected_version": (skill.source == crate::Source::Bot).then_some(&skill.version),
                            }),
                        },
                        ContentPart::Text { text },
                    ],
                    spill: None,
                })
            }
            Op::Write => {
                let name = str_arg(&input, "name")?;
                let content = str_arg(&input, "content")?;
                let expected = match input.get("expected_version") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(value)) => Some(value.as_str()),
                    Some(_) => {
                        return Err(ToolError::InvalidInput(
                            "expected_version must be a string or null".into(),
                        ));
                    }
                };
                let written = index
                    .write_versioned(name, content, expected)
                    .map_err(ToolError::Execution)?;
                self.index.reload();
                Ok(ToolOutput::text(format!(
                    "Saved skill `{name}` v{} to {}",
                    written.version,
                    written.path.display()
                )))
            }
            Op::Publish => {
                let name = str_arg(&input, "name")?;
                let replace = match input.get("replace") {
                    None => false,
                    Some(Value::Bool(value)) => *value,
                    Some(_) => {
                        return Err(ToolError::InvalidInput("replace must be a boolean".into()));
                    }
                };
                let path = index.publish(name, replace).map_err(ToolError::Execution)?;
                self.index.reload();
                Ok(ToolOutput::text(format!(
                    "Published skill `{name}` to {}",
                    path.display()
                )))
            }
        }
    }
}
