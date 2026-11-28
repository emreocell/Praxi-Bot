//! Pure helpers for owner-defined "something changed" watchers: new feed items and the text a
//! CSS selector picks on a page. No network here: the caller fetches with the SSRF-guarded
//! [`crate::http::HttpFetcher`], so every byte this module sees is untrusted third-party text.

use scraper::{Html, Selector};
use sha2::{Digest, Sha256};
use url::Url;

use crate::error::WebError;
use crate::xmlfeed::parse_feed;

/// Most feed items looked at in one poll (the newest by document order).
pub const MAX_FEED_ITEMS: usize = 100;
/// Longest watched page text kept (characters); a longer selection is cut, not refused.
pub const MAX_PAGE_TEXT_CHARS: usize = 4_000;
/// Longest title / summary kept per feed item (characters).
const ITEM_FIELD_CHARS: usize = 400;

/// One feed item reduced to what a bot may be told about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchedItem {
    /// Stable identity: the feed's `id`/`guid`, else the link, else a hash of the title.
    pub key: String,
    /// Title as plain text.
    pub title: String,
    /// Permalink, when the item has one.
    pub link: String,
    /// Publication or update time as the feed wrote it.
    pub published: String,
    /// Start of the summary as plain text.
    pub summary: String,
}

fn cut(text: &str, cap: usize) -> String {
    let stripped: String = text
        .chars()
        .filter(|c| !c.is_control() && !is_invisible(*c))
        .collect();
    stripped
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(cap)
        .collect()
}

/// Zero-width and bidirectional controls that can hide or reorder text.
fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}' | '\u{feff}'
    )
}

/// SHA-256 of `text` in hex, used as a fingerprint (the text itself is not the identity).
#[must_use]
pub fn fingerprint(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    digest.iter().fold(String::with_capacity(64), |mut out, b| {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// The items of an RSS/RDF/Atom document, newest first as the feed lists them.
///
/// # Errors
/// [`WebError::Parse`] when the text is not a feed.
pub fn feed_items(xml: &str, base: Option<&Url>) -> Result<Vec<WatchedItem>, WebError> {
    let feed = parse_feed(xml, base)?;
    Ok(feed
        .entries
        .into_iter()
        .take(MAX_FEED_ITEMS)
        .filter_map(|e| {
            let title = cut(e.title.as_deref().unwrap_or(""), ITEM_FIELD_CHARS);
            let link = cut(e.link.as_deref().unwrap_or(""), 2048);
            let key = [e.id.as_deref(), Some(link.as_str())]
                .into_iter()
                .flatten()
                .map(str::trim)
                .find(|k| !k.is_empty())
                .map(|k| k.chars().take(512).collect::<String>())
                .or_else(|| {
                    (!title.is_empty()).then(|| format!("title:{}", fingerprint(&title)))
                })?;
            Some(WatchedItem {
                key,
                title,
                link,
                published: cut(
                    e.published
                        .as_deref()
                        .or(e.updated.as_deref())
                        .unwrap_or(""),
                    64,
                ),
                summary: cut(
                    e.summary.as_deref().or(e.content.as_deref()).unwrap_or(""),
                    ITEM_FIELD_CHARS,
                ),
            })
        })
        .collect())
}

/// Check that `selector` is a CSS selector this module can evaluate.
///
/// # Errors
/// [`WebError::Parse`] for a selector that does not parse.
pub fn check_selector(selector: &str) -> Result<(), WebError> {
    Selector::parse(selector)
        .map(|_| ())
        .map_err(|_| WebError::Parse("not a valid CSS selector".into()))
}

/// The visible text of every element `selector` matches, whitespace collapsed, in document
/// order. Script and style text is never included.
///
/// # Errors
/// [`WebError::Parse`] for an invalid selector, or when nothing matches (a redesigned page
/// or an error page must read as "cannot check", not as "the text became empty").
pub fn select_text(html: &str, selector: &str) -> Result<String, WebError> {
    let sel = Selector::parse(selector)
        .map_err(|_| WebError::Parse("not a valid CSS selector".into()))?;
    let doc = Html::parse_document(html);
    let mut parts = Vec::new();
    let mut remaining = MAX_PAGE_TEXT_CHARS;
    let mut visited = 0usize;
    for element in doc.select(&sel).take(10_000) {
        let mut text = String::new();
        let mut stack = vec![*element];
        while let Some(node) = stack.pop() {
            visited += 1;
            if visited > 100_000 || remaining == 0 {
                break;
            }
            if node
                .value()
                .as_element()
                .is_some_and(|e| matches!(e.name(), "script" | "style" | "noscript"))
            {
                continue;
            }
            if let Some(t) = node.value().as_text() {
                let fragment: String = t.chars().take(remaining).collect();
                remaining -= fragment.chars().count();
                text.push_str(&fragment);
                text.push(' ');
            }
            stack.extend(node.children().rev());
        }
        let text = cut(&text, MAX_PAGE_TEXT_CHARS);
        if !text.is_empty() {
            parts.push(text);
        }
        if remaining == 0 || visited > 100_000 {
            break;
        }
    }
    if parts.is_empty() {
        return Err(WebError::Parse("the selector matched no text".into()));
    }
    Ok(cut(&parts.join(" | "), MAX_PAGE_TEXT_CHARS))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RSS: &str = r#"<?xml version="1.0"?><rss version="2.0"><channel><title>T</title>
      <item><title>Second  post</title><link>https://e.test/2</link><guid>g2</guid><pubDate>Mon, 02 Nov 2026 10:00:00 GMT</pubDate><description>Hello &lt;b&gt;world&lt;/b&gt;</description></item>
      <item><title>First</title><link>https://e.test/1</link></item>
      <item><description>no title and no link</description></item></channel></rss>"#;

    #[test]
    fn dtd_entity_expansion_is_rejected() {
        let xml = r#"<!DOCTYPE rss [<!ENTITY a "ha"><!ENTITY b "&a;&a;&a;&a;&a;&a;&a;&a;&a;&a;"><!ENTITY c "&b;&b;&b;&b;&b;&b;&b;&b;&b;&b;"><!ENTITY d "&c;&c;&c;&c;&c;&c;&c;&c;&c;&c;">]><rss><channel><item><title>&d;</title></item></channel></rss>"#;
        assert!(feed_items(xml, None).is_err());
    }

    #[test]
    fn broad_selector_on_large_page_is_bounded() {
        let html = format!(
            "<div>{}</div>",
            "<div>content<script>hidden</script></div>".repeat(50_000)
        );
        let text = select_text(&html, "*").unwrap();
        assert!(text.chars().count() <= MAX_PAGE_TEXT_CHARS);
        assert!(!text.contains("hidden"));
    }

    #[test]
    fn feed_items_use_guid_then_link_and_skip_items_with_no_identity() {
        let items = feed_items(RSS, None).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].key, "g2");
        assert_eq!(items[0].title, "Second post");
        assert_eq!(items[0].summary, "Hello world");
        assert_eq!(items[1].key, "https://e.test/1");
    }

    #[test]
    fn feed_text_loses_control_and_bidi_characters() {
        let xml = "<rss version=\"2.0\"><channel><item><title>a\u{202e}b\u{200b}c\u{85}d</title><link>https://e.test/x</link></item></channel></rss>";
        assert_eq!(feed_items(xml, None).unwrap()[0].title, "abcd");
    }

    #[test]
    fn a_page_that_is_not_a_feed_is_an_error_not_an_empty_list() {
        assert!(feed_items("<html><body>hi</body></html>", None).is_err());
        assert!(feed_items("not xml at all", None).is_err());
    }

    #[test]
    fn selector_text_collapses_space_and_skips_script() {
        let html = r#"<html><body><div id="price"> $ 12 <script>var x=1</script><b>.50</b></div><p class=x>a</p><p class=x>b</p></body></html>"#;
        assert_eq!(select_text(html, "#price").unwrap(), "$ 12 .50");
        assert_eq!(select_text(html, "p.x").unwrap(), "a | b");
    }

    #[test]
    fn no_match_or_bad_selector_is_an_error() {
        let html = "<html><body><p>x</p></body></html>";
        assert!(select_text(html, "#missing").is_err());
        assert!(select_text(html, "p[").is_err());
        assert!(check_selector("div > a.link").is_ok());
        assert!(check_selector("<<").is_err());
    }

    #[test]
    fn long_selections_are_cut_and_fingerprints_differ_per_text() {
        let html = format!("<p id=a>{}</p>", "words ".repeat(2000));
        assert_eq!(
            select_text(&html, "#a").unwrap().chars().count(),
            MAX_PAGE_TEXT_CHARS
        );
        assert_ne!(fingerprint("a"), fingerprint("b"));
        assert_eq!(fingerprint("a").len(), 64);
    }
}
