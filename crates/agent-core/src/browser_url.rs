//! Which addresses an owner-connected browser may open without a card.
//!
//! A navigation to a public web page is the browser's own page traffic and
//! stays prompt-free. An address on this computer or its network (a `file:`
//! path, `localhost`, a LAN host or IP) is private machine data that never sets
//! the private exposure bit by itself, so opening one always asks and, once
//! approved, counts as a private read for the rest of the run.
//!
//! The decision looks at the URL argument only and fails closed: anything the
//! parser does not accept, or that is not clearly a public `http(s)` address,
//! is not public. A public name that resolves to a private address (DNS
//! rebinding) cannot be seen without resolving it; that is a documented
//! residual.

use serde_json::Value;

use builtin_tools::browser::is_public_web_url;

/// An action of an owner-connected browser that opens a URL other than a
/// public web address. `name` is the registered tool (`mcp:<server>/<tool>`);
/// `browser_navigate` must carry a URL, other tools only when they name one.
#[must_use]
pub(crate) fn opens_non_public(name: &str, input: &Value) -> bool {
    let tool = name.rsplit('/').next().unwrap_or(name);
    let Some(obj) = input.as_object() else {
        return tool == "browser_navigate";
    };
    let mut seen = false;
    for (key, value) in obj {
        if !["url", "uri", "href", "address"].contains(&key.to_ascii_lowercase().as_str()) {
            continue;
        }
        seen = true;
        if !value.as_str().is_some_and(is_public_web_url) {
            return true;
        }
    }
    !seen && tool == "browser_navigate"
}

#[cfg(test)]
mod tests {
    use super::opens_non_public;
    use serde_json::json;

    #[test]
    fn a_navigation_is_judged_by_its_url_argument_alone() {
        let nav = "mcp:pw/browser_navigate";
        assert!(!opens_non_public(
            nav,
            &json!({"url": "https://example.com"})
        ));
        assert!(opens_non_public(nav, &json!({"url": "file:///etc/passwd"})));
        assert!(opens_non_public(
            nav,
            &json!({"url": "http://127.0.0.1:8080"})
        ));
        assert!(opens_non_public(nav, &json!({"url": 5})));
        assert!(opens_non_public(nav, &json!({})));
        assert!(opens_non_public(nav, &json!("x")));
        // Other tools only when they name an address.
        assert!(!opens_non_public(
            "mcp:pw/browser_click",
            &json!({"ref": "e1"})
        ));
        assert!(!opens_non_public(
            "mcp:pw/browser_tabs",
            &json!({"action": "list"})
        ));
        assert!(opens_non_public(
            "mcp:pw/browser_tabs",
            &json!({"action": "new", "url": "file:///x"})
        ));
        assert!(opens_non_public(
            "mcp:pw/browser_tab_new",
            &json!({"URL": "http://localhost/"})
        ));
        assert!(!opens_non_public(
            "mcp:pw/browser_tab_new",
            &json!({"url": "https://example.com"})
        ));
    }
}
