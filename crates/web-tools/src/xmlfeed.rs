//! RSS 2.0 / RSS 1.0 (RDF) / Atom 1.0 feed parsing and sitemap parsing.
//!
//! Parsing is lenient: unknown HTML entities are tolerated, and XML that was
//! cut off by the download size cap is salvaged up to its last complete entry.

use roxmltree::{Document, Node, ParsingOptions};
use scraper::Html;
use url::Url;

use crate::error::WebError;

/// Feed flavour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FeedKind {
    /// RSS 0.9x / 2.0.
    Rss,
    /// RSS 1.0 (RDF).
    Rdf,
    /// Atom 1.0.
    Atom,
}

/// A parsed feed.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Feed {
    /// Flavour of the source document.
    pub kind: FeedKind,
    /// Feed title.
    pub title: Option<String>,
    /// Site link.
    pub link: Option<String>,
    /// Feed description / subtitle (plain text).
    pub description: Option<String>,
    /// Language code.
    pub language: Option<String>,
    /// Last update (as written in the feed; RFC 822 or RFC 3339).
    pub updated: Option<String>,
    /// Entries in document order.
    pub entries: Vec<FeedEntry>,
}

/// One feed entry.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct FeedEntry {
    /// Entry title.
    pub title: Option<String>,
    /// Permalink.
    pub link: Option<String>,
    /// Stable id (`guid` / Atom `id`).
    pub id: Option<String>,
    /// Publication date as written.
    pub published: Option<String>,
    /// Last update as written (Atom).
    pub updated: Option<String>,
    /// Summary as plain text.
    pub summary: Option<String>,
    /// Full content as plain text, when the feed carries it.
    pub content: Option<String>,
    /// Author name.
    pub author: Option<String>,
    /// Categories / tags.
    pub categories: Vec<String>,
}

/// Sitemap flavour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SitemapKind {
    /// `<urlset>`: entries are pages.
    UrlSet,
    /// `<sitemapindex>`: entries are further sitemaps.
    Index,
    /// Plain text list of URLs, one per line.
    Text,
}

/// One sitemap entry.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SitemapEntry {
    /// Location URL.
    pub loc: String,
    /// Last modification, as written.
    pub lastmod: Option<String>,
    /// Change frequency hint.
    pub changefreq: Option<String>,
    /// Priority 0.0 to 1.0.
    pub priority: Option<f32>,
}

/// A parsed sitemap or sitemap index.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Sitemap {
    /// Flavour.
    pub kind: SitemapKind,
    /// Entries in document order.
    pub entries: Vec<SitemapEntry>,
}

/// Replace entities XML does not define (`&nbsp;`, `&copy;`, ...) by a space.
fn fix_entities(xml: &str) -> String {
    let mut out = String::with_capacity(xml.len());
    let mut rest = xml;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let end = tail.find(';').filter(|&e| e <= 10);
        match end {
            Some(e) => {
                let name = &tail[1..e];
                let known = matches!(name, "amp" | "lt" | "gt" | "quot" | "apos")
                    || (name.starts_with('#') && name.len() > 1);
                if known {
                    out.push_str(&tail[..=e]);
                } else {
                    out.push(' ');
                }
                rest = &tail[e + 1..];
            }
            None => {
                out.push_str("&amp;");
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Cut `xml` after the last complete entry and close the open containers.
fn salvage(xml: &str) -> Option<String> {
    const ENTRY_ENDS: &[&str] = &["</item>", "</entry>", "</url>", "</sitemap>"];
    let cut = ENTRY_ENDS
        .iter()
        .filter_map(|e| xml.rfind(e).map(|i| i + e.len()))
        .max()?;
    let mut s = xml[..cut].to_string();
    for (marker, closers) in [
        ("<urlset", "</urlset>"),
        ("<sitemapindex", "</sitemapindex>"),
        ("<feed", "</feed>"),
        ("<rdf:RDF", "</rdf:RDF>"),
        ("<rss", "</channel></rss>"),
    ] {
        if xml.contains(marker) {
            s.push_str(closers);
            return Some(s);
        }
    }
    None
}

fn parse_once<R>(
    text: &str,
    f: &impl Fn(&Document<'_>) -> Result<R, WebError>,
) -> Result<R, WebError> {
    let opts = ParsingOptions {
        allow_dtd: true,
        nodes_limit: 2_000_000,
        ..ParsingOptions::default()
    };
    let doc = Document::parse_with_options(text, opts)
        .map_err(|e| WebError::Parse(format!("invalid XML: {e}")))?;
    f(&doc)
}

fn parse_doc<R>(
    xml: &str,
    f: impl Fn(&Document<'_>) -> Result<R, WebError>,
) -> Result<R, WebError> {
    let xml = xml.trim_start_matches('\u{feff}').trim_start();
    let first = match parse_once(xml, &f) {
        Ok(r) => return Ok(r),
        Err(e) => e,
    };
    let fixed = fix_entities(xml);
    if let Ok(r) = parse_once(&fixed, &f) {
        return Ok(r);
    }
    match salvage(&fixed) {
        Some(s) => parse_once(&s, &f),
        None => Err(first),
    }
}

fn child<'a, 'i>(n: Node<'a, 'i>, local: &str) -> Option<Node<'a, 'i>> {
    n.children()
        .find(|c| c.is_element() && c.tag_name().name() == local)
}

fn text_of(n: Node<'_, '_>) -> Option<String> {
    let t: String = n
        .descendants()
        .filter(Node::is_text)
        .filter_map(|t| t.text())
        .collect();
    let t = t.trim();
    (!t.is_empty()).then(|| t.to_string())
}

fn child_text(n: Node<'_, '_>, local: &str) -> Option<String> {
    child(n, local).and_then(text_of)
}

/// Strip markup from a feed text field and collapse whitespace.
fn plain(s: &str) -> String {
    let text = if s.contains('<') {
        Html::parse_fragment(s)
            .root_element()
            .text()
            .collect::<String>()
    } else {
        s.to_string()
    };
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn abs(base: Option<&Url>, link: &str) -> String {
    let link = link.trim();
    base.and_then(|b| b.join(link).ok())
        .map_or_else(|| link.to_string(), |u| u.to_string())
}

fn atom_link(n: Node<'_, '_>, base: Option<&Url>) -> Option<String> {
    let links: Vec<_> = n
        .children()
        .filter(|c| c.is_element() && c.tag_name().name() == "link")
        .collect();
    let pick = links
        .iter()
        .find(|l| matches!(l.attribute("rel"), None | Some("alternate")))
        .or_else(|| links.first())?;
    pick.attribute("href").map(|h| abs(base, h))
}

fn atom_entry(e: Node<'_, '_>, base: Option<&Url>) -> FeedEntry {
    FeedEntry {
        title: child_text(e, "title").map(|t| plain(&t)),
        link: atom_link(e, base),
        id: child_text(e, "id"),
        published: child_text(e, "published"),
        updated: child_text(e, "updated"),
        summary: child_text(e, "summary").map(|t| plain(&t)),
        content: child_text(e, "content").map(|t| plain(&t)),
        author: child(e, "author").and_then(|a| child_text(a, "name")),
        categories: e
            .children()
            .filter(|c| c.is_element() && c.tag_name().name() == "category")
            .filter_map(|c| c.attribute("term").map(str::to_string))
            .collect(),
    }
}

fn rss_item(i: Node<'_, '_>, base: Option<&Url>) -> FeedEntry {
    let guid = child_text(i, "guid");
    let link = child_text(i, "link").map(|l| abs(base, &l)).or_else(|| {
        // permalink guids double as links
        guid.as_deref()
            .filter(|g| g.starts_with("http"))
            .map(str::to_string)
    });
    FeedEntry {
        title: child_text(i, "title").map(|t| plain(&t)),
        link,
        id: guid,
        published: child_text(i, "pubDate").or_else(|| child_text(i, "date")),
        updated: None,
        summary: child_text(i, "description").map(|t| plain(&t)),
        content: child_text(i, "encoded").map(|t| plain(&t)),
        author: child_text(i, "creator").or_else(|| child_text(i, "author")),
        categories: i
            .children()
            .filter(|c| c.is_element() && c.tag_name().name() == "category")
            .filter_map(text_of)
            .collect(),
    }
}

/// Parse an RSS, RDF or Atom document. `base` resolves relative links.
///
/// # Errors
/// [`WebError::Parse`] when the text is not XML or not a recognised feed.
pub fn parse_feed(xml: &str, base: Option<&Url>) -> Result<Feed, WebError> {
    parse_doc(xml, |doc| {
        let root = doc.root_element();
        match root.tag_name().name() {
            "feed" => Ok(Feed {
                kind: FeedKind::Atom,
                title: child_text(root, "title").map(|t| plain(&t)),
                link: atom_link(root, base),
                description: child_text(root, "subtitle").map(|t| plain(&t)),
                language: root
                    .attribute(("http://www.w3.org/XML/1998/namespace", "lang"))
                    .map(str::to_string),
                updated: child_text(root, "updated"),
                entries: root
                    .children()
                    .filter(|c| c.is_element() && c.tag_name().name() == "entry")
                    .map(|e| atom_entry(e, base))
                    .collect(),
            }),
            name @ ("rss" | "RDF") => {
                let channel = child(root, "channel")
                    .ok_or_else(|| WebError::Parse("feed has no <channel>".into()))?;
                // RSS 2.0 nests items in the channel, RSS 1.0 beside it.
                let container = if name == "rss" { channel } else { root };
                Ok(Feed {
                    kind: if name == "rss" {
                        FeedKind::Rss
                    } else {
                        FeedKind::Rdf
                    },
                    title: child_text(channel, "title").map(|t| plain(&t)),
                    link: child_text(channel, "link").map(|l| abs(base, &l)),
                    description: child_text(channel, "description").map(|t| plain(&t)),
                    language: child_text(channel, "language"),
                    updated: child_text(channel, "lastBuildDate")
                        .or_else(|| child_text(channel, "pubDate"))
                        .or_else(|| child_text(channel, "date")),
                    entries: container
                        .children()
                        .filter(|c| c.is_element() && c.tag_name().name() == "item")
                        .map(|i| rss_item(i, base))
                        .collect(),
                })
            }
            other => Err(WebError::Parse(format!(
                "not a feed (root element <{other}>)"
            ))),
        }
    })
}

/// Parse a sitemap, sitemap index, or plain-text URL list.
///
/// # Errors
/// [`WebError::Parse`] when the XML root is neither `urlset` nor `sitemapindex`.
pub fn parse_sitemap(text: &str) -> Result<Sitemap, WebError> {
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    if !trimmed.starts_with('<') {
        let entries = trimmed
            .lines()
            .map(str::trim)
            .filter(|l| l.starts_with("http://") || l.starts_with("https://"))
            .map(|l| SitemapEntry {
                loc: l.to_string(),
                lastmod: None,
                changefreq: None,
                priority: None,
            })
            .collect::<Vec<_>>();
        if entries.is_empty() {
            return Err(WebError::Parse("no sitemap URLs found".into()));
        }
        return Ok(Sitemap {
            kind: SitemapKind::Text,
            entries,
        });
    }
    parse_doc(trimmed, |doc| {
        let root = doc.root_element();
        let (kind, tag) = match root.tag_name().name() {
            "urlset" => (SitemapKind::UrlSet, "url"),
            "sitemapindex" => (SitemapKind::Index, "sitemap"),
            other => {
                return Err(WebError::Parse(format!(
                    "not a sitemap (root element <{other}>)"
                )));
            }
        };
        let entries = root
            .children()
            .filter(|c| c.is_element() && c.tag_name().name() == tag)
            .filter_map(|e| {
                Some(SitemapEntry {
                    loc: child_text(e, "loc")?,
                    lastmod: child_text(e, "lastmod"),
                    changefreq: child_text(e, "changefreq"),
                    priority: child_text(e, "priority").and_then(|p| p.parse().ok()),
                })
            })
            .collect();
        Ok(Sitemap { kind, entries })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const RSS: &str = r#"<?xml version="1.0"?><rss version="2.0" xmlns:dc="http://purl.org/dc/elements/1.1/"
      xmlns:content="http://purl.org/rss/1.0/modules/content/"><channel>
      <title>Example Feed</title><link>https://example.com/</link><description>About &amp; stuff</description>
      <language>en</language><lastBuildDate>Mon, 02 Jan 2023 10:00:00 GMT</lastBuildDate>
      <item><title>First &amp; best</title><link>/p/1</link><guid isPermaLink="false">id-1</guid>
        <pubDate>Mon, 02 Jan 2023 09:00:00 GMT</pubDate><dc:creator>Ann</dc:creator>
        <description><![CDATA[<p>Hello <b>world</b>&nbsp;!</p>]]></description>
        <content:encoded><![CDATA[<div>Full <i>text</i></div>]]></content:encoded>
        <category>rust</category><category>web</category></item>
      <item><title>Second</title><link>https://example.com/p/2</link></item>
    </channel></rss>"#;

    const ATOM: &str = r#"<?xml version="1.0" encoding="utf-8"?><feed xmlns="http://www.w3.org/2005/Atom" xml:lang="en">
      <title>Atom Feed</title><subtitle>Sub</subtitle><updated>2023-01-02T10:00:00Z</updated>
      <link rel="self" href="https://example.com/feed.atom"/><link href="https://example.com/"/>
      <entry><title>One</title><id>urn:1</id><link rel="alternate" href="/one"/>
        <published>2023-01-01T00:00:00Z</published><updated>2023-01-02T00:00:00Z</updated>
        <summary>Short</summary><content type="html">&lt;p&gt;Body&lt;/p&gt;</content>
        <author><name>Bob</name></author><category term="news"/></entry></feed>"#;

    #[test]
    fn rss_parsed() {
        let base = Url::parse("https://example.com/feed").unwrap();
        let f = parse_feed(RSS, Some(&base)).unwrap();
        assert_eq!(f.kind, FeedKind::Rss);
        assert_eq!(f.title.as_deref(), Some("Example Feed"));
        assert_eq!(f.description.as_deref(), Some("About & stuff"));
        assert_eq!(f.entries.len(), 2);
        let e = &f.entries[0];
        assert_eq!(e.title.as_deref(), Some("First & best"));
        assert_eq!(e.link.as_deref(), Some("https://example.com/p/1"));
        assert_eq!(e.id.as_deref(), Some("id-1"));
        assert_eq!(e.author.as_deref(), Some("Ann"));
        assert_eq!(e.summary.as_deref(), Some("Hello world !"));
        assert_eq!(e.content.as_deref(), Some("Full text"));
        assert_eq!(e.categories, vec!["rust", "web"]);
    }

    #[test]
    fn atom_parsed() {
        let f = parse_feed(ATOM, None).unwrap();
        assert_eq!(f.kind, FeedKind::Atom);
        assert_eq!(f.link.as_deref(), Some("https://example.com/"));
        assert_eq!(f.language.as_deref(), Some("en"));
        let e = &f.entries[0];
        assert_eq!(e.link.as_deref(), Some("/one"));
        assert_eq!(e.author.as_deref(), Some("Bob"));
        assert_eq!(e.content.as_deref(), Some("Body"));
        assert_eq!(e.categories, vec!["news"]);
    }

    #[test]
    fn rdf_and_html_entities_and_truncation() {
        let rdf = r#"<?xml version="1.0"?><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
          xmlns="http://purl.org/rss/1.0/"><channel><title>R</title><link>https://r.example/</link></channel>
          <item><title>Caf&eacute;</title><link>https://r.example/1</link></item></rdf:RDF>"#;
        let f = parse_feed(rdf, None).unwrap();
        assert_eq!(f.kind, FeedKind::Rdf);
        assert_eq!(f.entries.len(), 1);

        let cut = &RSS[..RSS.find("<item><title>Second").unwrap() + 20];
        let f = parse_feed(cut, None).unwrap();
        assert_eq!(f.entries.len(), 1, "salvaged up to the last full item");
    }

    #[test]
    fn non_feed_rejected() {
        assert!(parse_feed("<html><body>hi</body></html>", None).is_err());
        assert!(parse_feed("not xml at all", None).is_err());
    }

    #[test]
    fn sitemap_urlset_index_and_text() {
        let xml = r#"<?xml version="1.0"?><urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
          <url><loc>https://e.com/a</loc><lastmod>2023-01-01</lastmod><changefreq>daily</changefreq><priority>0.8</priority></url>
          <url><loc> https://e.com/b </loc></url><url><lastmod>x</lastmod></url></urlset>"#;
        let s = parse_sitemap(xml).unwrap();
        assert_eq!(s.kind, SitemapKind::UrlSet);
        assert_eq!(s.entries.len(), 2);
        assert_eq!(s.entries[0].priority, Some(0.8));
        assert_eq!(s.entries[1].loc, "https://e.com/b");

        let idx = r#"<sitemapindex xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
          <sitemap><loc>https://e.com/s1.xml</loc><lastmod>2023-01-01</lastmod></sitemap></sitemapindex>"#;
        let s = parse_sitemap(idx).unwrap();
        assert_eq!(s.kind, SitemapKind::Index);
        assert_eq!(s.entries[0].loc, "https://e.com/s1.xml");

        let txt = parse_sitemap("https://e.com/1\nhttps://e.com/2\n#c\n").unwrap();
        assert_eq!(txt.kind, SitemapKind::Text);
        assert_eq!(txt.entries.len(), 2);

        let cut = &xml[..xml.find("<url><loc> https").unwrap() + 15];
        assert_eq!(parse_sitemap(cut).unwrap().entries.len(), 1);
        assert!(parse_sitemap("<html/>").is_err());
    }
}
