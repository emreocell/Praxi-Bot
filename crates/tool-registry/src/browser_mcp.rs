//! What a tool of a browser-automation MCP server does, judged from the
//! tool name the server registered (never from the model's arguments).
//!
//! Only a server the OWNER connected as a browser (an explicit `browser`
//! kind in the MCP configuration, or the built-in Playwright preset) is
//! judged here. Every other MCP server keeps the generic MCP class.
//!
//! The list of in-page tools is an allow list: a tool name that is not on it
//! is never treated as an in-page action, so a new or unknown tool keeps the
//! generic (asking) class.

use crate::{ContentClass, Effect};

/// Kind of one tool of an owner-connected browser server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserToolKind {
    /// Acts inside the page of the browser the owner connected: navigate,
    /// snapshot, click, type, evaluate in the page, wait, tabs, screenshot.
    InPage,
    /// Runs code outside the page sandbox (`browser_run_code`,
    /// `browser_run_code_unsafe`), or installs or launches a program. Always
    /// asks, whatever the level, the grants or the exposure.
    Code,
    /// Moves a file between this computer and a page (upload, PDF or other
    /// save). Always asks.
    File,
    /// Anything else on the server: the generic MCP class applies.
    Other,
}

/// Tools of the Playwright MCP server (and servers that copy its names) that
/// act only inside the page the browser shows.
const IN_PAGE: &[&str] = &[
    "browser_navigate",
    "browser_navigate_back",
    "browser_navigate_forward",
    "browser_snapshot",
    "browser_click",
    "browser_hover",
    "browser_drag",
    "browser_type",
    "browser_press_key",
    "browser_fill_form",
    "browser_select_option",
    "browser_evaluate",
    "browser_wait_for",
    "browser_tabs",
    "browser_tab_list",
    "browser_tab_new",
    "browser_tab_select",
    "browser_tab_close",
    "browser_take_screenshot",
    "browser_screen_capture",
    "browser_handle_dialog",
    "browser_console_messages",
    "browser_network_requests",
    "browser_resize",
    "browser_close",
    "browser_mouse_move_xy",
    "browser_mouse_click_xy",
    "browser_mouse_drag_xy",
    "browser_screen_move_mouse",
    "browser_screen_click",
    "browser_screen_drag",
    "browser_screen_type",
    "browser_generate_locator",
    "browser_verify_element_visible",
    "browser_verify_text_visible",
    "browser_verify_list_visible",
    "browser_verify_value",
];

/// Content class, effect and the always-ask flag of one tool of an
/// owner-connected browser server. `None` for [`BrowserToolKind::Other`]: the
/// generic MCP class applies.
///
/// * In-page: untrusted (a page is web content, not the user's private data),
///   egress unless the server is local-only, marked as acting inside the
///   owner's browser; `Write` (not destructive), `Read` with a trusted
///   read-only hint. Never always-ask.
/// * Code outside the page: every flag, a program start, `Destructive`,
///   always asks.
/// * File move: every flag, `Destructive`, always asks.
#[must_use]
pub fn rules(
    kind: BrowserToolKind,
    local_only: bool,
    trusted_read_only: bool,
) -> Option<(ContentClass, Effect, bool)> {
    match kind {
        BrowserToolKind::InPage => Some((
            ContentClass::declared(true, false, !local_only).in_owner_browser(),
            if trusted_read_only {
                Effect::Read
            } else {
                Effect::Write
            },
            false,
        )),
        BrowserToolKind::Code => Some((ContentClass::all().executes(), Effect::Destructive, true)),
        BrowserToolKind::File => Some((ContentClass::all(), Effect::Destructive, true)),
        BrowserToolKind::Other => None,
    }
}

/// Classify one tool of an owner-connected browser server by the name the
/// server registered.
#[must_use]
pub fn classify(remote_tool: &str) -> BrowserToolKind {
    if IN_PAGE.contains(&remote_tool) {
        return BrowserToolKind::InPage;
    }
    let lower = remote_tool.to_ascii_lowercase();
    if [
        "code", "script", "exec", "shell", "install", "eval", "launch",
    ]
    .iter()
    .any(|w| lower.contains(w))
    {
        return BrowserToolKind::Code;
    }
    if ["upload", "download", "file", "pdf", "save"]
        .iter()
        .any(|w| lower.contains(w))
    {
        return BrowserToolKind::File;
    }
    BrowserToolKind::Other
}

#[cfg(test)]
mod tests {
    use super::{BrowserToolKind as K, classify};

    #[test]
    fn page_actions_are_in_page_and_code_and_files_are_not() {
        for t in [
            "browser_navigate",
            "browser_snapshot",
            "browser_click",
            "browser_evaluate",
            "browser_type",
            "browser_wait_for",
            "browser_tabs",
        ] {
            assert_eq!(classify(t), K::InPage, "{t}");
        }
        for t in [
            "browser_run_code",
            "browser_run_code_unsafe",
            "browser_install",
            "browser_evaluate_in_node",
        ] {
            assert_eq!(classify(t), K::Code, "{t}");
        }
        for t in [
            "browser_file_upload",
            "browser_pdf_save",
            "browser_download",
        ] {
            assert_eq!(classify(t), K::File, "{t}");
        }
        // Unknown names, and a near miss of an in-page name, keep the generic class.
        for t in [
            "browser_start_tracing",
            "Browser_Click",
            "browser_click2",
            "",
        ] {
            assert_eq!(classify(t), K::Other, "{t}");
        }
    }
}
