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

use std::net::{Ipv4Addr, Ipv6Addr};

use serde_json::Value;
use url::{Host, Url};

/// Longest URL argument judged. A longer one asks.
const MAX_URL_BYTES: usize = 4096;

/// Name suffixes that are never the public internet.
const PRIVATE_SUFFIXES: &[&str] = &[
    "localhost",
    "local",
    "internal",
    "lan",
    "home.arpa",
    "localdomain",
    "intranet",
    "corp",
    "home",
    "private",
    "arpa",
];

fn v4_is_public(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(a == 0
        || a == 10
        || a == 127
        || a >= 224
        || (a == 100 && (64..=127).contains(&b))
        || (a == 169 && b == 254)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 168)
        || (a == 192 && b == 0 && c == 0)
        || (a == 192 && b == 0 && c == 2)
        || (a == 198 && (b == 18 || b == 19))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113))
}

fn v6_is_public(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return v4_is_public(v4);
    }
    let s = ip.segments();
    // Only global unicast (2000::/3), minus tunnels and documentation ranges
    // that can embed or stand for a private address.
    s[0] & 0xe000 == 0x2000 && s[0] != 0x2002 && !(s[0] == 0x2001 && (s[1] == 0 || s[1] == 0x0db8))
}

fn domain_is_public(domain: &str) -> bool {
    let name = domain.strip_suffix('.').unwrap_or(domain);
    if name.is_empty() || name.len() > 253 || !name.contains('.') {
        return false;
    }
    let labels: Vec<&str> = name.split('.').collect();
    if labels.iter().any(|l| {
        l.is_empty()
            || l.len() > 63
            || !l
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    }) {
        return false;
    }
    // A name ending in digits only is not a name (`1.2.3.4` forms the URL
    // parser already turned into an address).
    if labels
        .last()
        .is_some_and(|l| l.bytes().all(|b| b.is_ascii_digit()))
    {
        return false;
    }
    !PRIVATE_SUFFIXES
        .iter()
        .any(|s| name == *s || name.ends_with(&format!(".{s}")))
}

/// `raw` is an `http` or `https` URL with no credentials and a public DNS name
/// or public IP literal.
#[must_use]
pub(crate) fn is_public_web_url(raw: &str) -> bool {
    if raw.len() > MAX_URL_BYTES {
        return false;
    }
    let Ok(url) = Url::parse(raw.trim()) else {
        return false;
    };
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return false;
    }
    match url.host() {
        Some(Host::Domain(d)) => domain_is_public(d),
        Some(Host::Ipv4(ip)) => v4_is_public(ip),
        Some(Host::Ipv6(ip)) => v6_is_public(ip),
        None => false,
    }
}

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
    use super::{is_public_web_url, opens_non_public};
    use serde_json::json;

    #[test]
    fn only_a_public_http_address_is_public() {
        for url in [
            "https://example.com/",
            "http://Example.COM:8080/a?b=c#d",
            "https://sub.example.co.uk./x",
            "https://xn--bcher-kva.example/",
            "https://bücher.example/",
            "http://8.8.8.8/",
            "https://[2606:4700:4700::1111]/",
            "  https://example.com/  ",
        ] {
            assert!(is_public_web_url(url), "{url} should be public");
        }
        for url in [
            "file:///etc/passwd",
            "FILE:///C:/Users/me/secret.txt",
            "about:blank",
            "chrome://settings",
            "data:text/html,<b>x</b>",
            "javascript:alert(1)",
            "view-source:https://example.com",
            "blob:https://example.com/uuid",
            "ftp://example.com/x",
            "ws://example.com/",
            "https://user:pw@example.com/",
            "https://user@example.com/",
            "http://localhost:3000",
            "http://LOCALHOST./",
            "http://app.localhost/",
            "http://printer.local/",
            "http://db.internal/",
            "http://router.lan/",
            "http://x.home.arpa/",
            "http://intranet/",
            "http://singlelabel",
            "http://127.0.0.1:8080",
            "http://127.1/",
            "http://2130706433",
            "http://0x7f.1",
            "http://0177.0.0.1",
            "http://0.0.0.0/",
            "http://10.0.0.5/",
            "http://172.16.0.1/",
            "http://172.31.255.255/",
            "http://192.168.1.1/",
            "http://169.254.169.254/latest/meta-data",
            "http://100.64.0.1/",
            "http://224.0.0.1/",
            "http://255.255.255.255/",
            "http://[::1]/",
            "http://[::]/",
            "http://[fe80::1]/",
            "http://[fc00::1]/",
            "http://[fd12:3456::1]/",
            "http://[ff02::1]/",
            "http://[::ffff:127.0.0.1]/",
            "http://[::ffff:192.168.0.1]/",
            "http://[64:ff9b::7f00:1]/",
            "https://",
            "not a url",
            "example.com",
            "localhost:3000",
            "",
        ] {
            assert!(!is_public_web_url(url), "{url} must not be public");
        }
        assert!(!is_public_web_url(&format!(
            "https://example.com/{}",
            "a".repeat(5000)
        )));
    }

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
