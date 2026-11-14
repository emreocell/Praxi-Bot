//! Shared helpers: spec construction, input parsing, output capping.

use std::path::Path;

use serde::de::DeserializeOwned;
use serde_json::Value;
use tool_registry::{
    ContentClass, ContentPart, Effect, PermissionCategory, Platform, SpillRef, ToolContext,
    ToolError, ToolOutput, ToolSpec,
};

/// Classification of a built-in tool. `None` means the name is not a built-in
/// (a test double); [`spec`] then leaves it undeclared so registration fails
/// until the author classifies it.
///
/// Arms that share a class are one pattern. Comments name the family.
#[must_use]
pub fn classify(name: &str) -> Option<ContentClass> {
    Some(match name {
        // Questions, todos, schedule lifecycle, a local write, or a listing
        // of the bot's own bookkeeping. A write does not bring content in:
        // the model already had the bytes, and the result is not a private read.
        "ask_user" | "todo_write" | "schedule_pause" | "schedule_delete" | "fs_write"
        | "fs_edit" | "fs_move" | "fs_delete" | "checkpoint_list" | "checkpoint_restore"
        | "schedule_list" => ContentClass::none(),
        // Observation, including a click, scroll, or drag. Those pick an
        // on-screen target and cannot carry bytes off the machine. Typing,
        // keys, fill, launch, and `browser_click`'s `prompt_text` can; the
        // gate treats a click that includes `prompt_text` as egress.
        "fs_read" | "fs_list" | "fs_search" | "browser_snapshot" | "browser_screenshot"
        | "browser_wait" | "desktop_list_apps" | "desktop_get_state" | "desktop_click"
        | "desktop_scroll" | "desktop_drag" | "browser_scroll" => ContentClass::untrusted_private(),
        // A shell, a script, browser navigation and input, or a desktop
        // mutation that types, replaces a value, presses a key, or launches
        // a program can read and can send data off the machine. Tab changes
        // stay here: a page can turn them into network requests. The desktop
        // control gate is about who owns the computer, not about egress.
        "shell_run" | "script_run" | "browser_open" | "browser_back" | "browser_click"
        | "browser_fill" | "browser_select" | "browser_press" | "browser_eval" | "browser_tabs"
        | "desktop_launch" | "desktop_type" | "desktop_set_value" | "desktop_key" => {
            ContentClass::all()
        }
        "schedule_create" | "schedule_update" | "schedule_resume" => ContentClass::egress(),
        _ => return None,
    })
}

/// Build a [`ToolSpec`] that is available on every platform.
///
/// The content class comes from [`classify`]. A name that is not a built-in
/// stays [`ContentClass::unspecified`] so the registry will not accept it.
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
        content: classify(name).unwrap_or_else(ContentClass::unspecified),
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
    let total = text.len();
    if written {
        // The footer is built first so the whole inline text, footer included,
        // stays within `max_inline_bytes`. The cut length in the footer has at
        // most as many digits as `total`, so `total`'s width is a safe bound.
        let footer = |cut: usize| {
            format!(
                "
…[output truncated at {cut} of {total} bytes; full output in {}]",
                path.display()
            )
        };
        let room = ctx.max_inline_bytes.saturating_sub(footer(total).len());
        let cut = floor_boundary(&text, room);
        let mut inline = text[..cut].to_string();
        inline.push_str(&footer(cut));
        ToolOutput {
            content: vec![ContentPart::Text { text: inline }],
            spill: Some(SpillRef {
                path: path.display().to_string(),
                mime_type: "text/plain".into(),
                size_bytes: total as u64,
            }),
            exposure: None,
        }
    } else {
        const MARK: &str = "
…[output truncated]";
        let cut = floor_boundary(&text, ctx.max_inline_bytes.saturating_sub(MARK.len()));
        let mut inline = text[..cut].to_string();
        inline.push_str(MARK);
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
            max_inline_bytes: 1001,
            bot_id: None,
        };
        let text = "é".repeat(2000);
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

    fn inline_len(out: &ToolOutput) -> usize {
        let ContentPart::Text { text } = &out.content[0] else {
            panic!()
        };
        text.len()
    }

    #[test]
    fn inline_including_footer_fits_max_inline_bytes() {
        let dir = tempfile::tempdir().unwrap();
        for text in [
            "a".repeat(30_000),
            "é".repeat(15_000),
            "日本語".repeat(5_000),
        ] {
            for max in [400_usize, 1000, 16 * 1024] {
                let ctx = ToolContext {
                    run_id: String::new(),
                    max_inline_bytes: max,
                    bot_id: None,
                };
                let out = finish_text(&ctx, dir.path(), text.clone());
                assert!(out.spill.is_some());
                let n = inline_len(&out);
                assert!(n <= max, "{n} > {max}");
                assert!(n > max - 200, "{n} wastes room under {max}");
            }
        }
    }

    #[test]
    fn unwritable_spill_dir_still_respects_limit() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("file");
        std::fs::write(&blocker, "x").unwrap();
        let ctx = ToolContext {
            run_id: String::new(),
            max_inline_bytes: 100,
            bot_id: None,
        };
        let out = finish_text(&ctx, &blocker.join("sub"), "é".repeat(500));
        assert!(out.spill.is_none());
        assert!(inline_len(&out) <= 100);
    }
}
