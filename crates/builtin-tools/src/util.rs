//! Shared helpers: spec construction, input parsing, output capping.

use std::fmt::Write as _;
use std::path::Path;

use serde::de::DeserializeOwned;
use serde_json::Value;
use tool_registry::{
    ContentPart, Effect, PermissionCategory, Platform, SpillRef, ToolContext, ToolError,
    ToolOutput, ToolSpec,
};

/// Build a [`ToolSpec`] that is available on every platform.
pub(crate) fn spec(
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

/// Deserialize a tool input, mapping failures to [`ToolError::InvalidInput`].
pub(crate) fn parse<T: DeserializeOwned>(input: Value) -> Result<T, ToolError> {
    serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))
}

pub(crate) fn exec_err(msg: impl Into<String>) -> ToolError {
    ToolError::Execution(msg.into())
}

/// Largest index `<= max` that is a char boundary of `s`.
fn floor_boundary(s: &str, max: usize) -> usize {
    let mut i = max.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Return `text` as a tool output. Text larger than `ctx.max_inline_bytes` is
/// written in full to `<spill_dir>/<uuid>.txt`; the inline part is truncated and
/// the file is referenced in [`ToolOutput::spill`].
pub(crate) fn finish_text(ctx: &ToolContext, spill_dir: &Path, text: String) -> ToolOutput {
    if text.len() <= ctx.max_inline_bytes {
        return ToolOutput::text(text);
    }
    let path = spill_dir.join(format!("{}.txt", uuid::Uuid::now_v7()));
    let written = std::fs::create_dir_all(spill_dir)
        .and_then(|()| std::fs::write(&path, text.as_bytes()))
        .is_ok();
    let cut = floor_boundary(&text, ctx.max_inline_bytes);
    let mut inline = text[..cut].to_string();
    if written {
        let _ = write!(
            inline,
            "
…[output truncated at {cut} of {} bytes; full output in {}]",
            text.len(),
            path.display()
        );
        ToolOutput {
            content: vec![ContentPart::Text { text: inline }],
            spill: Some(SpillRef {
                path: path.display().to_string(),
                mime_type: "text/plain".into(),
                size_bytes: text.len() as u64,
            }),
        }
    } else {
        inline.push_str("\n…[output truncated]");
        ToolOutput::text(inline)
    }
}

/// Write `bytes` to `<dir>/<uuid>.<ext>` and return the path.
pub(crate) fn save_file(dir: &Path, ext: &str, bytes: &[u8]) -> Result<String, ToolError> {
    std::fs::create_dir_all(dir).map_err(|e| exec_err(format!("create {}: {e}", dir.display())))?;
    let path = dir.join(format!("{}.{ext}", uuid::Uuid::now_v7()));
    std::fs::write(&path, bytes).map_err(|e| exec_err(format!("write {}: {e}", path.display())))?;
    Ok(path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_text_is_inline() {
        let ctx = ToolContext::default();
        let out = finish_text(&ctx, Path::new("unused"), "hi".into());
        assert!(out.spill.is_none());
    }

    #[test]
    fn large_text_spills_and_truncates_on_char_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext {
            run_id: String::new(),
            max_inline_bytes: 10,
        };
        let text = "é".repeat(50);
        let out = finish_text(&ctx, dir.path(), text.clone());
        let spill = out.spill.expect("spilled");
        assert_eq!(std::fs::read_to_string(&spill.path).unwrap(), text);
        assert_eq!(spill.size_bytes, text.len() as u64);
        let ContentPart::Text { text: inline } = &out.content[0] else {
            panic!()
        };
        assert!(inline.starts_with("ééééé"));
        assert!(inline.contains("truncated"));
    }
}
