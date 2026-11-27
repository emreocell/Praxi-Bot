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
        // of the bot's own bookkeeping (secret names and origins carry no value; removing
        // bots this bot made brings no content in; `browser_handover` only checks that a
        // CAPTCHA or code prompt is really there and returns fixed text). A write does not bring content in:
        // the model already had the bytes, and the result is not a private read.
        // `secrets_list` / `account_list` show names, sites and logins the owner
        // chose or the bot made: no value and no text a page wrote, so they neither
        // taint nor leave the machine (tainting would also stop an owner's standing
        // grant from covering the very fill the list exists for). `bot_delete` and
        // `account_discard` are bookkeeping (no content in); `account_discard` always asks.
        "ask_user" | "todo_write" | "schedule_pause" | "schedule_delete" | "fs_write"
        | "fs_move" | "fs_delete" | "checkpoint_list" | "checkpoint_restore" | "schedule_list"
        | "secrets_list" | "secret_request" | "bot_delete" | "account_list" | "account_discard"
        | "browser_handover" => ContentClass::none(),
        // Pure helpers: only the clock and the model's own short text go in.
        // Writing or deleting a note only changes the bot's own notes file on this machine:
        // not egress. What a tainted run stores is marked and read back fenced (see `notes`),
        // so a poisoned note is data to the next run, not an instruction.
        "time_now" | "calculate" | "wait" | "notes_write" => {
            ContentClass::declared(false, false, false)
        }
        // Editing reads old content and reports match counts, including no-op edits.
        // Observation, including a click or a scroll. Those pick an on-screen
        // target and cannot carry bytes off the machine. A drag can drop a
        // file or a selection onto an upload. Typing, keys, fill, launch, and
        // `browser_click`'s `prompt_text` can too; the gate treats a click
        // that includes `prompt_text` as egress.
        "browser_snapshot" | "browser_screenshot" | "browser_wait" | "desktop_get_state"
        | "desktop_list_apps" | "desktop_wait" | "desktop_focus" | "desktop_click"
        | "desktop_scroll" | "browser_scroll" | "browser_hover" => {
            ContentClass::untrusted_private()
        }
        // The user's own files and the list of their own windows: private, and not untrusted
        // by themselves (nothing in them was written by an outsider). A file that ARRIVED from
        // outside (Downloads, attachments, mail) is marked untrusted per call by the loop
        // (`taint::arrived_from_outside`). Full screen content (`desktop_get_state`) stays
        // untrusted: the code cannot tell a browser or mail window from another program.
        // The bot's own notes (`notes_read`) are private unless proven otherwise: the tool
        // reports the marks of the notes it returns per call (see `notes`), so a note written
        // in a clean run taints nothing and an untrusted or private one does.
        "fs_read" | "fs_list" | "fs_search" | "fs_edit" | "notes_read" => {
            ContentClass::private_read()
        }
        // A shell, a script, browser navigation and input, a drag, or a
        // desktop mutation that types, replaces a value, presses a key, or
        // launches a program can read and can send data off the machine. Tab
        // changes stay here: a page can turn them into network requests. The
        // desktop control gate is about who owns the computer, not about egress.
        // A shell, a script, and a desktop launch start a program chosen by
        // their arguments. In a run that read untrusted content they ask even
        // when the egress waiver is on; the command is the content.
        // The desktop input tools drive the keyboard and pointer: Win+R, a
        // terminal or a save dialog turns them into a launcher, so they count.
        "shell_run" | "shell_interactive" | "script_run" | "desktop_launch" | "desktop_type" | "desktop_set_value"
        | "desktop_key" | "desktop_drag" => ContentClass::all().executes(),
        "browser_open"
        | "browser_back"
        | "browser_click"
        | "browser_fill"
        | "browser_select"
        | "browser_press"
        | "browser_eval"
        | "browser_tabs"
        | "browser_act"
        | "browser_select_text"
        | "browser_dismiss_consent"
        // Pointer actions click, drag and type by coordinates like `browser_click` / `browser_press`;
        // attaching to the owner's browser opens every tab and session it holds.
        | "browser_pointer"
        | "browser_attach"
        // Reload and forward navigate like `browser_back`; an upload hands a file of this
        // computer to a page, which is both a private read and a send off the machine.
        | "browser_reload"
        | "browser_forward"
        | "browser_upload"
        // `email_wait_for` reads the bot's own inbox: untrusted (a stranger's mail), private
        // (a mailbox) and, like `email_read`, a call to a remote mailbox. No text the model
        // wrote is sent (filters run locally), but the class stays the same as `email_read`
        // so a mail- or webhook-started run keeps being asked.
        | "email_wait_for" => ContentClass::all(),
        // A created or edited bot acts later on a persona written now, like a
        // schedule; the call always asks with the complete resulting profile.
        // `account_register` creates a standing credential and a site identity (login,
        // mailbox) chosen by the model, so a run that read untrusted content asks like a
        // created bot; it also always asks on its own and never takes a standing grant.
        // The self-check makes fixed, read-only sample calls (a mailbox check), each only
        // after the loop's gate allowed it unattended, and returns constant sentences,
        // never a tool's own text: it can reach the network and brings nothing in.
        "schedule_create" | "schedule_update" | "schedule_resume" | "bot_create" | "bot_update"
        | "account_register" | "tool_self_check" => ContentClass::egress(),
        // A bot's name and persona are text any bot with `manage_bots` can write, so a
        // listing brings possibly untrusted text in. The download list shows file names a web
        // page chose; it sends nothing and reads no private data.
        "bot_list" | "browser_downloads" => ContentClass::untrusted(),
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
            content_tainted: false,
            exposure_untrusted: false,
            exposure_private: false,
            expected_origin: None,
            unattended_mail: false,
            payment: tool_registry::PaymentApproval::Unchecked,
            control_guard: None,
            lane: None,
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
                    content_tainted: false,
                    exposure_untrusted: false,
                    exposure_private: false,
                    expected_origin: None,
                    unattended_mail: false,
                    payment: tool_registry::PaymentApproval::Unchecked,
                    control_guard: None,
                    lane: None,
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
            content_tainted: false,
            exposure_untrusted: false,
            exposure_private: false,
            expected_origin: None,
            unattended_mail: false,
            payment: tool_registry::PaymentApproval::Unchecked,
            control_guard: None,
            lane: None,
        };
        let out = finish_text(&ctx, &blocker.join("sub"), "é".repeat(500));
        assert!(out.spill.is_none());
        assert!(inline_len(&out) <= 100);
    }

    #[test]
    fn window_titles_are_untrusted_text() {
        // `desktop_list_apps` returns window titles (a page's document.title among them).
        let class = classify("desktop_list_apps").unwrap();
        assert!(class.ingests_untrusted && class.reads_private);
        assert!(
            !classify("fs_read").unwrap().ingests_untrusted,
            "own files stay private"
        );
    }
}
