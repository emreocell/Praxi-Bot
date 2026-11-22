//! The "choosing tools" block of the system prompt.
//!
//! Models reach for a browser to look things up, run a second, invisible
//! browser next to the visible one, and tell the user "it is open" without
//! having checked. This block says, once and in a fixed order, which tool
//! family fits which job, and ties every claim to the result of the call that
//! did the work.
//!
//! Like the other blocks it is a pure function of the permitted tool names
//! (fixed text only, never tool descriptions), so it is byte-identical across
//! the model calls of a run and does not break the prompt cache.

use std::fmt::Write as _;

/// Which tool families a run can use.
#[allow(clippy::struct_excessive_bools)] // one flag per tool family
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Families {
    pub web_search: bool,
    pub web_fetch: bool,
    /// The assistant's own managed browser (`browser_*`).
    pub browser: bool,
    /// `browser_act` (several browser steps in one call) is offered.
    pub browser_act: bool,
    /// Browser tools of a connected app (an MCP server such as Playwright).
    pub app_browser: bool,
    pub shell: bool,
    /// Any tool at all.
    pub any: bool,
}

impl Families {
    pub(crate) fn of<'a>(names: impl IntoIterator<Item = &'a str>) -> Self {
        let mut f = Self::default();
        for name in names {
            f.any = true;
            if let Some(rest) = name.strip_prefix("mcp:") {
                let tool = rest.split_once('/').map_or(rest, |(_, t)| t);
                if tool.contains("browser") || rest.contains("playwright") {
                    f.app_browser = true;
                }
                continue;
            }
            match name {
                "web_search" => f.web_search = true,
                "web_extract" => f.web_fetch = true,
                "shell_run" => f.shell = true,
                "browser_act" => {
                    f.browser = true;
                    f.browser_act = true;
                }
                _ if name.starts_with("browser_") => f.browser = true,
                _ => {}
            }
        }
        f
    }
}

/// The block, or `None` when the run has no tool at all.
pub(crate) fn block(f: Families) -> Option<String> {
    if !f.any {
        return None;
    }
    let mut out = String::from("Choosing tools:");
    if f.web_search || f.web_fetch {
        out.push_str(
            "\n- To look something up, read an article or check a fact or a price, use web_search and web_extract \
(fast, plain text, no browser window). Do this BEFORE opening a browser.",
        );
    }
    if f.browser {
        out.push_str(
            "\n- Use the browser_* tools only when a real page is needed: logging in, clicking, forms, pages that \
need scripts, or when the user wants to watch. Every browser_* result ends with a [view: ...] line saying whether the \
user can see that page right now; never tell the user a page is open or visible unless that line says so, and if it \
says no live view is attached, tell them to open the live view of this chat.",
        );
        out.push_str(
            "\n- Work quietly: every browser_* action already returns what changed, so do not call browser_snapshot or \
browser_eval after each click just to look again, and do not write a paragraph between steps. Make the next call \
straight away; say something only when you are done, blocked or need the user.",
        );
        out.push_str(
            "
- Page workflow: the result of browser_open already holds the page with element refs (e1, e2...); act on those refs. After a page change refs go stale, so look again before the next click. Dismiss a cookie banner or consent dialog first (click its Accept or Reject button), then do the task. A link that opens a new tab or a popup shows up in browser_tabs; switch to it there. If a result says the page was still loading, read what is shown or use browser_wait; do not reopen the same address in a loop. Take a screenshot only when the question is about how the page looks. Downloads are blocked by design: say so instead of retrying.",
        );
        if f.browser_act {
            out.push_str(
                "
- For a form or a flow of several clicks, take ONE browser_snapshot and do the steps with ONE browser_act call (click, fill, select, press, scroll, wait, up to 12) instead of one tool call per step: it is much faster. The batch checks the page before every step and stops at the first change; its result names the step it stopped at and the steps not run, then continue from the page state it shows. browser_open and secret fills stay single calls.",
            );
        }
    }
    if f.app_browser && f.browser {
        out.push_str(
            "\n- A connected app also offers browser tools. Prefer browser_*: they drive this assistant's own browser, \
which the user can watch. A connected app's browser is a different browser the user cannot see here; if you use it, \
say so.",
        );
    }
    if f.app_browser && !f.browser {
        out.push_str(
            "
- A connected app offers browser tools (Playwright): call browser_snapshot before browser_click or browser_type and pass the ref it shows; use browser_wait_for for text that loads late; dismiss cookie banners first; use browser_tabs for popups and new tabs. Its pages are in a separate browser the user may not see: say so.",
        );
    }
    if (f.web_fetch || f.browser) && f.shell {
        out.push_str(
            "\n- Do not read web pages with shell_run (curl, wget) or browser_eval when web_extract or browser_* can do it.",
        );
    }
    let _ = write!(
        out,
        "\n- Before you tell the user a job is done, check the result of the call that did it. A failed, timed-out, \
denied or empty result means it is NOT done: say that plainly. If a call timed out, it is unknown whether it \
finished; check, retry once differently, or tell the user."
    );
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_goes_to_web_tools_before_the_browser() {
        let f = Families::of(["web_search", "web_extract", "browser_open", "shell_run"]);
        let b = block(f).unwrap();
        let web = b.find("web_search and web_extract").unwrap();
        let browser = b.find("browser_* tools only when").unwrap();
        assert!(web < browser, "{b}");
        assert!(b.contains("BEFORE opening a browser"), "{b}");
        assert!(b.contains("[view: ...]"), "{b}");
        assert!(b.contains("do not write a paragraph between steps"), "{b}");
        assert!(b.contains("Do not read web pages with shell_run"), "{b}");
    }

    #[test]
    fn a_connected_apps_browser_is_told_apart_from_the_own_one() {
        let f = Families::of(["browser_open", "mcp:playwright/browser_snapshot"]);
        assert!(f.app_browser && f.browser);
        let b = block(f).unwrap();
        assert!(b.contains("different browser the user cannot see"), "{b}");
        // Nothing about a second browser when there is only one.
        let only = block(Families::of(["browser_open"])).unwrap();
        assert!(!only.contains("connected app"), "{only}");
    }

    #[test]
    fn batches_are_recommended_only_when_browser_act_is_offered() {
        let with = block(Families::of(["browser_open", "browser_act"])).unwrap();
        assert!(with.contains("ONE browser_act call"), "{with}");
        assert!(with.contains("stops at the first change"), "{with}");
        let without = block(Families::of(["browser_open", "browser_click"])).unwrap();
        assert!(!without.contains("browser_act"), "{without}");
    }

    #[test]
    fn page_workflow_is_taught_for_both_browser_surfaces() {
        let own = block(Families::of(["browser_open"])).unwrap();
        for needle in [
            "element refs",
            "cookie banner",
            "browser_tabs",
            "still loading",
            "Downloads are blocked",
        ] {
            assert!(own.contains(needle), "{needle}: {own}");
        }
        let app = block(Families::of(["mcp:playwright/browser_navigate"])).unwrap();
        assert!(
            app.contains("browser_snapshot before browser_click"),
            "{app}"
        );
        assert!(app.contains("browser_wait_for"), "{app}");
        // Nothing browser-related when no browser tool exists.
        let none = block(Families::of(["fs_read"])).unwrap();
        assert!(!none.contains("cookie banner"), "{none}");
    }

    #[test]
    fn no_tools_no_block_and_unlisted_families_are_not_mentioned() {
        assert!(block(Families::of([])).is_none());
        let b = block(Families::of(["fs_read"])).unwrap();
        assert!(!b.contains("browser_*") && !b.contains("web_search"), "{b}");
        assert!(b.contains("NOT done"), "{b}");
    }
}
