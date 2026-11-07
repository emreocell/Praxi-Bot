//! `skill_list`, `skill_read` and `skill_write` tools.

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
pub const SKILL_TOOL_NAMES: [&str; 3] = ["skill_list", "skill_read", "skill_write"];

#[derive(Clone, Copy)]
enum Op {
    List,
    Read,
    Write,
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

/// Build the three skill tools over a shared [`SkillIndex`].
#[must_use]
pub fn tools(index: &SkillIndex) -> Vec<Arc<dyn Tool>> {
    let defs = [
        (
            Op::List,
            mk_spec(
                "skill_list",
                "List available skills (name, description, version, source, extra files). Example: {}",
                json!({"type":"object","properties":{}}),
                Effect::Read,
                PermissionCategory::Meta,
            ),
        ),
        (
            Op::Read,
            mk_spec(
                "skill_read",
                "Load a skill's instructions, or one of its extra files. Example: {\"name\":\"custom-dropdowns\"}",
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
                "Save a learned skill to your own skills folder. Content is a full SKILL.md: frontmatter (name, description, optional version and when: domains/apps/keywords) plus markdown body. Example: {\"name\":\"my-site\",\"content\":\"---\nname: my-site\ndescription: How to use my-site\n---\nSteps...\"}",
                json!({"type":"object","properties":{
                    "name":{"type":"string","description":"Skill name: a-z, 0-9, '-', '_', '.'."},
                    "content":{"type":"string","description":"Complete SKILL.md text."}
                },"required":["name","content"]}),
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

#[async_trait]
impl Tool for SkillTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, _ctx: &ToolContext, input: Value) -> Result<ToolOutput, ToolError> {
        match self.op {
            Op::List => {
                let skills = self.index.list();
                let mut text = String::new();
                let mut data = Vec::new();
                for s in &skills {
                    let files = self.index.extra_files(&s.name);
                    let _ = writeln!(
                        text,
                        "- {} (v{}, {}): {}",
                        s.name,
                        s.version,
                        s.source.label(),
                        s.description
                    );
                    data.push(json!({
                        "name": s.name, "description": s.description, "version": s.version,
                        "source": s.source.label(), "files": files,
                    }));
                }
                for i in self.index.issues() {
                    let _ = writeln!(text, "! skipped {}: {}", i.path, i.message);
                }
                if text.is_empty() {
                    text.push_str("No skills available.");
                }
                Ok(ToolOutput {
                    content: vec![
                        ContentPart::Json {
                            data: Value::Array(data),
                        },
                        ContentPart::Text { text },
                    ],
                    spill: None,
                })
            }
            Op::Read => {
                let name = str_arg(&input, "name")?;
                let file = input.get("file").and_then(Value::as_str);
                self.index
                    .read(name, file)
                    .map(ToolOutput::text)
                    .map_err(ToolError::Execution)
            }
            Op::Write => {
                let name = str_arg(&input, "name")?;
                let content = str_arg(&input, "content")?;
                self.index
                    .write(name, content)
                    .map(|p| ToolOutput::text(format!("Saved skill `{name}` to {}", p.display())))
                    .map_err(ToolError::Execution)
            }
        }
    }
}
