//! Page metadata: `<title>`, description, canonical URL, language, feeds,
//! `OpenGraph`, Twitter cards and JSON-LD structured data.

use std::collections::BTreeMap;

use scraper::{Html, Selector};
use serde_json::Value;
use url::Url;

/// Metadata of one HTML page.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct PageMetadata {
    /// `<title>` text.
    pub title: Option<String>,
    /// `<meta name="description">`.
    pub description: Option<String>,
    /// `<link rel="canonical">`, absolute.
    pub canonical: Option<String>,
    /// `<html lang>`.
    pub language: Option<String>,
    /// `<meta name="robots">`.
    pub robots: Option<String>,
    /// Author, from `<meta name="author">`.
    pub author: Option<String>,
    /// Favicon / icon links, absolute.
    pub icons: Vec<String>,
    /// Advertised RSS/Atom feeds as `(title, absolute url)`.
    pub feeds: Vec<FeedLink>,
    /// `og:*` properties without the prefix (`title`, `image`, ...).
    pub open_graph: BTreeMap<String, String>,
    /// `twitter:*` properties without the prefix.
    pub twitter: BTreeMap<String, String>,
    /// Parsed JSON-LD blocks (arrays and `@graph` are flattened to one entry per node).
    pub json_ld: Vec<Value>,
}

/// An advertised feed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct FeedLink {
    /// Link title attribute, if any.
    pub title: Option<String>,
    /// Absolute feed URL.
    pub url: String,
}

fn sel(s: &str) -> Selector {
    Selector::parse(s).expect("static selector is valid")
}

fn clean(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn resolve(base: Option<&Url>, href: &str) -> Option<String> {
    let href = href.trim();
    if href.is_empty() {
        return None;
    }
    let url = match base {
        Some(b) => b.join(href).ok()?,
        None => Url::parse(href).ok()?,
    };
    matches!(url.scheme(), "http" | "https").then(|| url.to_string())
}

fn flatten_json_ld(v: Value, out: &mut Vec<Value>) {
    match v {
        Value::Array(items) => items.into_iter().for_each(|i| flatten_json_ld(i, out)),
        Value::Object(mut map) => {
            if let Some(Value::Array(graph)) = map.remove("@graph") {
                // Keep the shared @context on each node.
                let ctx = map.get("@context").cloned();
                for mut node in graph {
                    if let (Some(c), Value::Object(n)) = (&ctx, &mut node) {
                        n.entry("@context").or_insert_with(|| c.clone());
                    }
                    flatten_json_ld(node, out);
                }
                if map.keys().any(|k| k != "@context") {
                    out.push(Value::Object(map));
                }
            } else {
                out.push(Value::Object(map));
            }
        }
        _ => {}
    }
}

/// Extract metadata from `html`. `base` resolves relative URLs.
#[must_use]
pub fn extract_metadata(html: &str, base: Option<&Url>) -> PageMetadata {
    let doc = Html::parse_document(html);
    let mut m = PageMetadata {
        title: doc
            .select(&sel("title"))
            .next()
            .map(|t| clean(&t.text().collect::<String>()))
            .filter(|t| !t.is_empty()),
        language: doc
            .select(&sel("html"))
            .next()
            .and_then(|h| h.value().attr("lang"))
            .map(str::to_string)
            .filter(|l| !l.is_empty()),
        ..PageMetadata::default()
    };

    for meta in doc.select(&sel("meta")) {
        let e = meta.value();
        let Some(content) = e.attr("content").map(clean).filter(|c| !c.is_empty()) else {
            continue;
        };
        let key = e
            .attr("property")
            .or_else(|| e.attr("name"))
            .map(str::to_ascii_lowercase);
        let Some(key) = key else { continue };
        if let Some(k) = key.strip_prefix("og:") {
            m.open_graph.entry(k.to_string()).or_insert(content);
        } else if let Some(k) = key.strip_prefix("twitter:") {
            m.twitter.entry(k.to_string()).or_insert(content);
        } else {
            match key.as_str() {
                "description" => m.description = m.description.take().or(Some(content)),
                "robots" => m.robots = m.robots.take().or(Some(content)),
                "author" => m.author = m.author.take().or(Some(content)),
                _ => {}
            }
        }
    }
    if m.description.is_none() {
        m.description = m.open_graph.get("description").cloned();
    }

    for link in doc.select(&sel("link[href]")) {
        let e = link.value();
        let rel = e.attr("rel").unwrap_or("").to_ascii_lowercase();
        let Some(href) = e.attr("href").and_then(|h| resolve(base, h)) else {
            continue;
        };
        let rels: Vec<&str> = rel.split_whitespace().collect();
        if rels.contains(&"canonical") {
            m.canonical.get_or_insert(href);
        } else if rels.contains(&"icon") || rels.contains(&"apple-touch-icon") {
            m.icons.push(href);
        } else if rels.contains(&"alternate") {
            let ty = e.attr("type").unwrap_or("").to_ascii_lowercase();
            if ty == "application/rss+xml" || ty == "application/atom+xml" {
                m.feeds.push(FeedLink {
                    title: e.attr("title").map(clean).filter(|t| !t.is_empty()),
                    url: href,
                });
            }
        }
    }

    for script in doc.select(&sel("script[type]")) {
        let ty = script
            .value()
            .attr("type")
            .unwrap_or("")
            .to_ascii_lowercase();
        if ty != "application/ld+json" {
            continue;
        }
        let raw: String = script.text().collect();
        // Real pages contain stray HTML comment markers and trailing commas
        // are not tolerated; bad blocks are skipped, not fatal.
        let raw = raw
            .trim()
            .trim_start_matches("<!--")
            .trim_end_matches("-->");
        if let Ok(v) = serde_json::from_str::<Value>(raw.trim()) {
            flatten_json_ld(v, &mut m.json_ld);
        }
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r#"<html lang="en"><head>
    <title> Hello   World </title>
    <meta name="description" content="A test page">
    <meta name="robots" content="index,follow">
    <meta property="og:title" content="OG Title">
    <meta property="og:image" content="/img/cover.png">
    <meta name="twitter:card" content="summary">
    <link rel="canonical" href="/canonical/path">
    <link rel="icon" href="/favicon.ico">
    <link rel="alternate" type="application/rss+xml" title="Posts" href="/feed.xml">
    <script type="application/ld+json">{"@context":"https://schema.org","@type":"Article","headline":"H"}</script>
    <script type="application/ld+json">{"@context":"https://schema.org","@graph":[{"@type":"Person","name":"P"},{"@type":"Organization","name":"O"}]}</script>
    <script type="application/ld+json">{ broken json </script>
    </head><body></body></html>"#;

    #[test]
    fn extracts_everything() {
        let base = Url::parse("https://example.com/a/b").unwrap();
        let m = extract_metadata(PAGE, Some(&base));
        assert_eq!(m.title.as_deref(), Some("Hello World"));
        assert_eq!(m.description.as_deref(), Some("A test page"));
        assert_eq!(m.language.as_deref(), Some("en"));
        assert_eq!(m.robots.as_deref(), Some("index,follow"));
        assert_eq!(
            m.canonical.as_deref(),
            Some("https://example.com/canonical/path")
        );
        assert_eq!(m.icons, vec!["https://example.com/favicon.ico"]);
        assert_eq!(m.feeds[0].url, "https://example.com/feed.xml");
        assert_eq!(m.feeds[0].title.as_deref(), Some("Posts"));
        assert_eq!(m.open_graph["title"], "OG Title");
        assert_eq!(m.open_graph["image"], "/img/cover.png");
        assert_eq!(m.twitter["card"], "summary");
        assert_eq!(m.json_ld.len(), 3);
        assert_eq!(m.json_ld[0]["@type"], "Article");
        assert_eq!(m.json_ld[1]["@type"], "Person");
        assert_eq!(m.json_ld[1]["@context"], "https://schema.org");
    }

    #[test]
    fn empty_page_is_fine() {
        let m = extract_metadata("", None);
        assert!(m.title.is_none() && m.json_ld.is_empty());
    }
}
