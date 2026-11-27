//! Tabs, reload and forward: `browser_tabs` (list, switch, `new`, `close`), `browser_reload`,
//! `browser_forward`.
//!
//! A new tab goes through the SAME address rules as `browser_open` (public http(s) only, then the
//! owner's site rules and approvals, judged by where it goes), and the page it lands on is checked
//! like any other landing. Closing is deliberately narrow: the first tab (the one the live view
//! follows) and the last tab stay, and in the owner's own browser only a tab the assistant opened
//! itself can be closed.

use serde::Deserialize;
use serde_json::json;
use tool_registry::{Effect, PermissionCategory, ToolError, ToolSpec};

use crate::util::spec;

#[derive(Debug, Deserialize)]
pub(super) struct TabsIn {
    pub(super) switch_to: Option<usize>,
    new: Option<bool>,
    url: Option<String>,
    close: Option<usize>,
}

/// What one `browser_tabs` call asks for.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum TabsAct {
    List,
    Switch(usize),
    New(Option<String>),
    Close(usize),
}

impl TabsIn {
    /// The request, or why it makes no sense (said plainly, so the model can fix it).
    pub(super) fn act(self) -> Result<TabsAct, ToolError> {
        let bad = |m: &str| Err(ToolError::InvalidInput(m.into()));
        let new = self.new.unwrap_or(false);
        let asked = usize::from(self.switch_to.is_some())
            + usize::from(new)
            + usize::from(self.close.is_some());
        if asked > 1 {
            return bad("give only one of switch_to, new, close");
        }
        if self.url.is_some() && !new {
            return bad("url goes with new:true (it is the address the new tab opens)");
        }
        if self.switch_to == Some(0) || self.close == Some(0) {
            return bad("tab numbers start at 1 (see the list)");
        }
        if let Some(n) = self.switch_to {
            return Ok(TabsAct::Switch(n));
        }
        if let Some(n) = self.close {
            return Ok(TabsAct::Close(n));
        }
        if new {
            return Ok(TabsAct::New(
                self.url
                    .map(|u| u.trim().to_owned())
                    .filter(|u| !u.is_empty()),
            ));
        }
        Ok(TabsAct::List)
    }
}

/// A bare list changes nothing; any other form is an action.
pub(super) fn is_listing(input: &serde_json::Value) -> bool {
    use serde_json::Value;
    input.get("switch_to").is_none_or(Value::is_null)
        && !input.get("new").and_then(Value::as_bool).unwrap_or(false)
        && input.get("close").is_none_or(Value::is_null)
}

pub(super) fn tabs_spec(category: PermissionCategory) -> ToolSpec {
    spec(
        "browser_tabs",
        "List open tabs (current is marked *), switch with switch_to, open one with new:true (optional url: the public web addresses browser_open takes) or close one with close (its number from the list). The first and the last tab stay open; in the user's own browser only tabs you opened can be closed. Examples: {\"switch_to\":2}, {\"new\":true,\"url\":\"https://example.com\"}, {\"close\":2}",
        json!({"type":"object","properties":{
            "switch_to":{"type":"integer","minimum":1,"description":"Tab number from the list."},
            "new":{"type":"boolean","description":"Open a new tab and make it the current one."},
            "url":{"type":"string","description":"With new: the address to open."},
            "close":{"type":"integer","minimum":1,"description":"Tab number from the list to close."}
        }}),
        Effect::Write,
        category,
    )
}

pub(super) fn reload_spec(category: PermissionCategory) -> ToolSpec {
    spec(
        "browser_reload",
        "Load the current page again (what you typed is lost): for a page that failed to load or needs fresh content. Example: {}",
        json!({"type":"object","properties":{
            "dialog":{"type":"string","enum":["accept","dismiss"],"description":"Dialog handling; default dismiss."}
        }}),
        Effect::Write,
        category,
    )
}

pub(super) fn forward_spec(category: PermissionCategory) -> ToolSpec {
    spec(
        "browser_forward",
        "Go forward one page (after browser_back). Example: {}",
        json!({"type":"object","properties":{
            "dialog":{"type":"string","enum":["accept","dismiss"],"description":"Dialog handling; default dismiss."}
        }}),
        Effect::Write,
        category,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn act(v: serde_json::Value) -> Result<TabsAct, ToolError> {
        serde_json::from_value::<TabsIn>(v).unwrap().act()
    }

    #[test]
    fn each_form_of_the_tabs_call_is_recognised() {
        assert_eq!(act(json!({})).unwrap(), TabsAct::List);
        assert_eq!(act(json!({"switch_to": 2})).unwrap(), TabsAct::Switch(2));
        assert_eq!(act(json!({"close": 3})).unwrap(), TabsAct::Close(3));
        assert_eq!(act(json!({"new": true})).unwrap(), TabsAct::New(None));
        assert_eq!(
            act(json!({"new": true, "url": " https://example.com "})).unwrap(),
            TabsAct::New(Some("https://example.com".into()))
        );
        // A blank address is a blank tab.
        assert_eq!(
            act(json!({"new": true, "url": "  "})).unwrap(),
            TabsAct::New(None)
        );
        // `new:false` is a list, not a new tab.
        assert_eq!(act(json!({"new": false})).unwrap(), TabsAct::List);
    }

    #[test]
    fn contradictory_or_meaningless_requests_are_refused_with_a_reason() {
        for v in [
            json!({"switch_to": 1, "close": 2}),
            json!({"new": true, "switch_to": 1}),
            json!({"new": true, "close": 1}),
            json!({"url": "https://example.com"}),
            json!({"switch_to": 1, "url": "https://example.com"}),
            json!({"close": 0}),
            json!({"switch_to": 0}),
        ] {
            assert!(
                matches!(act(v.clone()), Err(ToolError::InvalidInput(_))),
                "{v}"
            );
        }
    }
}
