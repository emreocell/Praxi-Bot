//! Readability-style main-content extraction and HTML to Markdown conversion.
//!
//! The pipeline: parse with `html5ever` (via `scraper`), pick the main content
//! root (`<article>` / `<main>` when substantial, otherwise a paragraph-density
//! score discounted by link density), drop boilerplate (nav, footer, forms,
//! scripts, hidden and ad/cookie/sidebar blocks), then render Markdown with
//! headings, lists, code, quotes, GFM tables and resolved links.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

use ego_tree::NodeId;
use scraper::{ElementRef, Html, Node, Selector, node::Element};
use url::Url;

/// Options for [`html_to_markdown`].
#[derive(Debug, Clone)]
pub struct ExtractOptions {
    /// Render `[text](url)` links (otherwise only the link text).
    pub include_links: bool,
    /// Render `![alt](src)` for images that have alt text.
    pub include_images: bool,
    /// Skip main-content detection and convert the whole `<body>`.
    pub full_page: bool,
    /// Maximum number of links returned in [`Extracted::links`].
    pub max_links: usize,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        Self {
            include_links: true,
            include_images: true,
            full_page: false,
            max_links: 200,
        }
    }
}

/// A hyperlink found in the extracted content.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Link {
    /// Visible link text (may be empty for image links).
    pub text: String,
    /// Absolute URL (resolved against the page URL when one was given).
    pub url: String,
}

/// Result of an extraction.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Extracted {
    /// Page title.
    pub title: Option<String>,
    /// Author byline when declared in meta tags.
    pub byline: Option<String>,
    /// Main content as Markdown.
    pub markdown: String,
    /// Links inside the main content, deduplicated, in document order.
    pub links: Vec<Link>,
    /// Extraction picked a content root other than `<body>`.
    pub main_content_found: bool,
}

/// Elements removed together with their subtree.
const SKIP_TAGS: &[&str] = &[
    "script", "style", "noscript", "template", "iframe", "object", "embed", "svg", "canvas",
    "form", "button", "select", "input", "textarea", "nav", "aside", "footer", "dialog", "audio",
    "video", "map", "head", "link", "meta",
];

/// Class/id fragments marking boilerplate blocks.
const BOILERPLATE_HINTS: &[&str] = &[
    "sidebar",
    "advert",
    "ad-",
    "-ad",
    "adsbygoogle",
    "cookie",
    "consent",
    "popup",
    "modal",
    "newsletter",
    "subscribe",
    "share",
    "social",
    "breadcrumb",
    "related",
    "comment",
    "promo",
    "banner",
    "toolbar",
    "menu",
    "skip-link",
    "sr-only",
    "visually-hidden",
];

fn sel(s: &str) -> Selector {
    Selector::parse(s).expect("static selector is valid")
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Deepest element nesting rendered structurally. Deeper subtrees are
/// flattened to their plain text so hostile pages cannot overflow the stack.
const MAX_RENDER_DEPTH: usize = 100;

/// Subtree text and link-text lengths for every node, computed once in a
/// single linear pass (the per-element recomputation was quadratic on deeply
/// nested pages).
struct Stats {
    text: HashMap<NodeId, usize>,
    link: HashMap<NodeId, usize>,
}

impl Stats {
    fn new(doc: &Html) -> Self {
        let mut text: HashMap<NodeId, usize> = HashMap::new();
        let mut link: HashMap<NodeId, usize> = HashMap::new();
        // Pre-order: parents precede children, so a reverse walk sees every
        // child before its parent.
        let order: Vec<_> = doc.tree.root().descendants().collect();
        for n in order.iter().rev() {
            let own = match n.value() {
                Node::Text(t) => t.trim().len(),
                _ => 0,
            };
            let t = text.get(&n.id()).copied().unwrap_or(0) + own;
            let is_anchor = matches!(n.value(), Node::Element(e) if e.name() == "a");
            let l = if is_anchor {
                t
            } else {
                link.get(&n.id()).copied().unwrap_or(0)
            };
            if let Some(parent) = n.parent() {
                *text.entry(parent.id()).or_insert(0) += t;
                *link.entry(parent.id()).or_insert(0) += l;
            }
            text.insert(n.id(), t);
            link.insert(n.id(), l);
        }
        Self { text, link }
    }

    fn text_len(&self, el: ElementRef<'_>) -> usize {
        self.text.get(&el.id()).copied().unwrap_or(0)
    }

    fn link_density(&self, el: ElementRef<'_>) -> f64 {
        let total = self.text_len(el);
        if total == 0 {
            return 1.0;
        }
        let links = self.link.get(&el.id()).copied().unwrap_or(0);
        #[allow(clippy::cast_precision_loss)]
        {
            (links as f64 / total as f64).min(1.0)
        }
    }
}

fn is_hidden(e: &Element) -> bool {
    e.attr("hidden").is_some()
        || e.attr("aria-hidden") == Some("true")
        || e.attr("style").is_some_and(|s| {
            let s: String = s
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect::<String>()
                .to_ascii_lowercase();
            s.contains("display:none") || s.contains("visibility:hidden")
        })
}

fn boilerplate_hint(e: &Element) -> bool {
    let class = e.attr("class").unwrap_or("");
    let id = e.attr("id").unwrap_or("");
    let role = e.attr("role").unwrap_or("");
    if matches!(
        role,
        "navigation" | "banner" | "contentinfo" | "complementary" | "dialog" | "search"
    ) {
        return true;
    }
    let hay = format!("{class} {id}").to_ascii_lowercase();
    BOILERPLATE_HINTS.iter().any(|h| hay.contains(h))
}

/// Document title: `<title>`, then `og:title`, then the first `<h1>`.
fn find_title(doc: &Html) -> Option<String> {
    let from_tag = doc
        .select(&sel("title"))
        .next()
        .map(|t| collapse_ws(&t.text().collect::<String>()))
        .filter(|t| !t.is_empty());
    from_tag
        .or_else(|| meta_content(doc, "property", "og:title"))
        .or_else(|| {
            doc.select(&sel("h1"))
                .next()
                .map(|t| collapse_ws(&t.text().collect::<String>()))
                .filter(|t| !t.is_empty())
        })
}

fn meta_content(doc: &Html, attr: &str, key: &str) -> Option<String> {
    let s = sel("meta");
    doc.select(&s)
        .find(|m| {
            m.value()
                .attr(attr)
                .is_some_and(|v| v.eq_ignore_ascii_case(key))
        })
        .and_then(|m| m.value().attr("content"))
        .map(collapse_ws)
        .filter(|v| !v.is_empty())
}

/// Pick the element holding the main content.
fn find_root<'d>(doc: &'d Html, stats: &Stats) -> Option<ElementRef<'d>> {
    // 1. Semantic containers, when they carry real text.
    for s in ["article", "main", "[role=main]"] {
        let best = doc
            .select(&sel(s))
            .filter(|e| !is_hidden(e.value()))
            .max_by_key(|e| stats.text_len(*e));
        if let Some(b) = best {
            if stats.text_len(b) >= 200 {
                return Some(b);
            }
        }
    }
    // 2. Paragraph-density scoring.
    let mut scores: HashMap<_, f64> = HashMap::new();
    for p in doc.select(&sel("p, pre, td, blockquote")) {
        let len = stats.text_len(p);
        if len < 25 {
            continue;
        }
        let text: String = p.text().collect();
        #[allow(clippy::cast_precision_loss)]
        let points = 1.0 + text.matches(',').count() as f64 + (len as f64 / 100.0).min(3.0);
        let mut ancestors = p.ancestors().filter_map(ElementRef::wrap);
        if let Some(parent) = ancestors.next() {
            *scores.entry(parent.id()).or_insert(0.0) += points;
            if let Some(grand) = ancestors.next() {
                *scores.entry(grand.id()).or_insert(0.0) += points / 2.0;
            }
        }
    }
    let mut best: Option<(f64, ElementRef<'_>)> = None;
    for (id, score) in scores {
        let Some(el) = doc.tree.get(id).and_then(ElementRef::wrap) else {
            continue;
        };
        if matches!(el.value().name(), "html" | "head") {
            continue;
        }
        let adjusted = score * (1.0 - stats.link_density(el));
        if best.as_ref().is_none_or(|(b, _)| adjusted > *b) {
            best = Some((adjusted, el));
        }
    }
    best.filter(|(s, _)| *s > 5.0).map(|(_, e)| e)
}

struct Renderer<'a> {
    base: Option<&'a Url>,
    opts: &'a ExtractOptions,
    links: RefCell<Vec<Link>>,
    seen: RefCell<HashSet<String>>,
    root_len: usize,
    stats: &'a Stats,
    depth: Cell<usize>,
}

impl Renderer<'_> {
    fn resolve(&self, href: &str) -> Option<String> {
        let href = href.trim();
        if href.is_empty() || href.starts_with('#') {
            return None;
        }
        let url = match self.base {
            Some(b) => b.join(href).ok()?,
            None => Url::parse(href).ok()?,
        };
        matches!(url.scheme(), "http" | "https").then(|| url.to_string())
    }

    fn skip(&self, el: ElementRef<'_>) -> bool {
        let e = el.value();
        if SKIP_TAGS.contains(&e.name()) || is_hidden(e) {
            return true;
        }
        // Boilerplate-looking blocks are only dropped when they are small
        // relative to the content root (a wrapper called `has-sidebar` must
        // not eat the whole article).
        boilerplate_hint(e)
            && self.stats.text_len(el) * 2 < self.root_len.max(1) + 200
            && !matches!(e.name(), "html" | "body" | "article" | "main")
    }

    fn children(&self, el: ElementRef<'_>, out: &mut String, pre: bool) {
        for child in el.children() {
            self.node(child, out, pre);
        }
    }

    fn node(&self, n: ego_tree::NodeRef<'_, Node>, out: &mut String, pre: bool) {
        match n.value() {
            Node::Text(t) => {
                if pre {
                    out.push_str(t);
                } else {
                    push_text(out, t);
                }
            }
            Node::Element(_) => {
                if let Some(el) = ElementRef::wrap(n) {
                    self.element(el, out, pre);
                }
            }
            _ => {}
        }
    }

    fn inline_string(&self, el: ElementRef<'_>) -> String {
        let mut s = String::new();
        self.children(el, &mut s, false);
        s.trim().to_string()
    }

    fn element(&self, el: ElementRef<'_>, out: &mut String, pre: bool) {
        if self.skip(el) {
            return;
        }
        let depth = self.depth.get();
        if depth >= MAX_RENDER_DEPTH {
            // Flatten: no further structural recursion past the cap.
            Self::flatten(el, out, pre);
            return;
        }
        self.depth.set(depth + 1);
        self.element_inner(el, out, pre);
        self.depth.set(depth);
    }

    /// Plain text of a subtree without recursion, honouring the skip rules
    /// for script-like content.
    fn flatten(el: ElementRef<'_>, out: &mut String, pre: bool) {
        let mut skipping: Option<NodeId> = None;
        for edge in el.traverse() {
            match edge {
                ego_tree::iter::Edge::Open(n) => {
                    if skipping.is_some() {
                        continue;
                    }
                    match n.value() {
                        Node::Element(e) if SKIP_TAGS.contains(&e.name()) || is_hidden(e) => {
                            skipping = Some(n.id());
                        }
                        Node::Text(t) if pre => out.push_str(t),
                        Node::Text(t) => push_text(out, t),
                        _ => {}
                    }
                }
                ego_tree::iter::Edge::Close(n) => {
                    if skipping == Some(n.id()) {
                        skipping = None;
                    }
                }
            }
        }
    }

    fn element_inner(&self, el: ElementRef<'_>, out: &mut String, pre: bool) {
        let name = el.value().name();
        match name {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let level = usize::from(name.as_bytes()[1] - b'0');
                let text = collapse_ws(&self.inline_string(el));
                if !text.is_empty() {
                    block_break(out);
                    out.push_str(&"#".repeat(level));
                    out.push(' ');
                    out.push_str(&text);
                    block_break(out);
                }
            }
            "p" | "div" | "section" | "article" | "main" | "header" | "figure" | "figcaption"
            | "details" | "summary" | "dl" | "dt" | "dd" | "address" | "body" | "html"
            | "center" | "fieldset" | "legend" | "hgroup" => {
                block_break(out);
                self.children(el, out, pre);
                block_break(out);
            }
            "br" => {
                trim_spaces(out);
                out.push('\n');
            }
            "hr" => {
                block_break(out);
                out.push_str("---");
                block_break(out);
            }
            "ul" | "ol" => self.list(el, out, name == "ol"),
            "blockquote" => {
                let mut inner = String::new();
                self.children(el, &mut inner, false);
                let inner = tidy(&inner);
                if !inner.is_empty() {
                    block_break(out);
                    for line in inner.lines() {
                        if line.is_empty() {
                            out.push_str(">\n");
                        } else {
                            out.push_str("> ");
                            out.push_str(line);
                            out.push('\n');
                        }
                    }
                    block_break(out);
                }
            }
            "pre" => {
                let code = el.select(&sel("code")).next();
                let lang = code
                    .and_then(|c| c.value().attr("class"))
                    .and_then(|c| {
                        c.split_whitespace().find_map(|t| {
                            t.strip_prefix("language-")
                                .or_else(|| t.strip_prefix("lang-"))
                        })
                    })
                    .unwrap_or("");
                let mut raw = String::new();
                self.children(el, &mut raw, true);
                let raw = raw.trim_matches('\n');
                if !raw.trim().is_empty() {
                    block_break(out);
                    let fence = if raw.contains("```") { "~~~~" } else { "```" };
                    out.push_str(fence);
                    out.push_str(lang);
                    out.push('\n');
                    out.push_str(raw);
                    out.push('\n');
                    out.push_str(fence);
                    block_break(out);
                }
            }
            "code" | "kbd" | "samp" | "tt" => {
                let mut raw = String::new();
                self.children(el, &mut raw, pre);
                if pre {
                    out.push_str(&raw);
                } else if !raw.trim().is_empty() {
                    let raw = collapse_ws(&raw);
                    let tick = if raw.contains('`') { "``" } else { "`" };
                    out.push_str(&format!("{tick}{raw}{tick}"));
                }
            }
            "strong" | "b" => self.wrap(el, out, "**"),
            "em" | "i" | "cite" => self.wrap(el, out, "*"),
            "del" | "s" | "strike" => self.wrap(el, out, "~~"),
            "a" => self.anchor(el, out),
            "img" => {
                if self.opts.include_images {
                    let alt = collapse_ws(el.value().attr("alt").unwrap_or(""));
                    let src = el.value().attr("src").unwrap_or("");
                    if !alt.is_empty() && !src.starts_with("data:") {
                        if let Some(u) = self.resolve(src) {
                            out.push_str(&format!("![{alt}]({u})"));
                        }
                    }
                }
            }
            "table" => self.table(el, out),
            "li" => {
                block_break(out);
                self.children(el, out, pre);
                block_break(out);
            }
            _ => self.children(el, out, pre),
        }
    }

    fn wrap(&self, el: ElementRef<'_>, out: &mut String, mark: &str) {
        let inner = self.inline_string(el);
        if inner.is_empty() {
            return;
        }
        if !out.is_empty() && !out.ends_with(char::is_whitespace) && !out.ends_with(['(', '[']) {
            out.push(' ');
        }
        out.push_str(mark);
        out.push_str(&inner);
        out.push_str(mark);
        out.push(' ');
    }

    fn anchor(&self, el: ElementRef<'_>, out: &mut String) {
        let text = collapse_ws(&self.inline_string(el));
        let href = el.value().attr("href").and_then(|h| self.resolve(h));
        match href {
            Some(url) => {
                if self.seen.borrow_mut().insert(url.clone()) {
                    self.links.borrow_mut().push(Link {
                        text: text.clone(),
                        url: url.clone(),
                    });
                }
                if text.is_empty() {
                    return;
                }
                if self.opts.include_links {
                    let text = text.replace('[', "\\[").replace(']', "\\]");
                    out.push_str(&format!("[{text}]({url})"));
                } else {
                    out.push_str(&text);
                }
            }
            None => out.push_str(&text),
        }
    }

    fn list(&self, el: ElementRef<'_>, out: &mut String, ordered: bool) {
        let mut items = Vec::new();
        for child in el.children().filter_map(ElementRef::wrap) {
            if child.value().name() != "li" || self.skip(child) {
                continue;
            }
            let mut inner = String::new();
            self.children(child, &mut inner, false);
            let inner = tidy(&inner);
            if !inner.is_empty() {
                items.push(inner);
            }
        }
        if items.is_empty() {
            return;
        }
        block_break(out);
        let start: usize = el
            .value()
            .attr("start")
            .and_then(|s| s.parse().ok())
            .unwrap_or(1);
        for (i, item) in items.iter().enumerate() {
            let marker = if ordered {
                format!("{}. ", start + i)
            } else {
                "- ".to_string()
            };
            let pad = " ".repeat(marker.len());
            for (j, line) in item.lines().enumerate() {
                if j == 0 {
                    out.push_str(&marker);
                } else if !line.is_empty() {
                    out.push_str(&pad);
                }
                out.push_str(line);
                out.push('\n');
            }
        }
        block_break(out);
    }

    fn table(&self, el: ElementRef<'_>, out: &mut String) {
        let tr = sel("tr");
        let cell_sel = sel("th, td");
        let mut rows: Vec<Vec<String>> = Vec::new();
        let mut header_idx = None;
        for row in el.select(&tr) {
            // Skip rows that belong to a nested table.
            let owner = row
                .ancestors()
                .filter_map(ElementRef::wrap)
                .find(|a| a.value().name() == "table");
            if owner.is_none_or(|o| o.id() != el.id()) {
                continue;
            }
            let mut cells = Vec::new();
            let mut has_th = false;
            for cell in row.select(&cell_sel) {
                if cell.value().name() == "th" {
                    has_th = true;
                }
                let span: usize = cell
                    .value()
                    .attr("colspan")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(1)
                    .clamp(1, 20);
                let text = collapse_ws(&self.inline_string(cell)).replace('|', "\\|");
                cells.push(text);
                for _ in 1..span {
                    cells.push(String::new());
                }
            }
            if cells.is_empty() {
                continue;
            }
            if has_th && header_idx.is_none() {
                header_idx = Some(rows.len());
            }
            rows.push(cells);
        }
        if rows.is_empty() {
            return;
        }
        let width = rows.iter().map(Vec::len).max().unwrap_or(0);
        for r in &mut rows {
            r.resize(width, String::new());
        }
        let header = header_idx.map_or(0, |h| h.min(rows.len() - 1));
        let head = rows.remove(header);
        block_break(out);
        out.push_str(&format!("| {} |\n", head.join(" | ")));
        out.push_str(&format!("|{}|\n", vec![" --- "; width].join("|")));
        for r in rows {
            out.push_str(&format!("| {} |\n", r.join(" | ")));
        }
        block_break(out);
    }
}

fn push_text(out: &mut String, text: &str) {
    let mut prev_space = out.is_empty() || out.ends_with(char::is_whitespace);
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
}

fn trim_spaces(out: &mut String) {
    while out.ends_with(' ') {
        out.pop();
    }
}

fn block_break(out: &mut String) {
    if out.is_empty() {
        return;
    }
    trim_spaces(out);
    while !out.ends_with("\n\n") {
        out.push('\n');
    }
}

/// Trim trailing spaces per line, collapse 3+ newlines, trim the whole.
fn tidy(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut blank = 0usize;
    let mut in_fence = false;
    for line in s.lines() {
        let line = if in_fence { line } else { line.trim_end() };
        if line.starts_with("```") || line.starts_with("~~~~") {
            in_fence = !in_fence;
        }
        if line.is_empty() && !in_fence {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim().to_string()
}

/// Extract the main content of `html` as Markdown.
///
/// `base` resolves relative links and images. The function never fails: broken
/// HTML is repaired by the parser and an empty page yields empty Markdown.
#[must_use]
pub fn html_to_markdown(html: &str, base: Option<&Url>, opts: &ExtractOptions) -> Extracted {
    let doc = Html::parse_document(html);
    let title = find_title(&doc);
    let byline = meta_content(&doc, "name", "author")
        .or_else(|| meta_content(&doc, "property", "article:author"));
    let body = doc
        .select(&sel("body"))
        .next()
        .unwrap_or_else(|| doc.root_element());
    let stats = Stats::new(&doc);
    let found = if opts.full_page {
        None
    } else {
        find_root(&doc, &stats)
    };
    let root = found.unwrap_or(body);
    let renderer = Renderer {
        base,
        opts,
        links: RefCell::new(Vec::new()),
        seen: RefCell::new(HashSet::new()),
        root_len: stats.text_len(root),
        stats: &stats,
        depth: Cell::new(0),
    };
    let mut out = String::new();
    // Render the root's children so a boilerplate class on the root itself
    // cannot suppress it.
    renderer.children(root, &mut out, false);
    let mut markdown = tidy(&out);
    // Add the title as a heading when the content has no top-level heading.
    if let Some(t) = &title {
        if !markdown.lines().any(|l| l.starts_with("# ")) && !markdown.is_empty() {
            markdown = format!(
                "# {t}

{markdown}"
            );
        }
    }
    let mut links = renderer.links.into_inner();
    links.truncate(opts.max_links);
    Extracted {
        title,
        byline,
        markdown,
        links,
        main_content_found: found.is_some(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARTICLE: &str = r#"<!doctype html><html><head><title>The Great Post | Blog</title>
    <meta name="author" content="Ada Lovelace"><style>p{color:red}</style></head><body>
    <nav><a href="/home">Home</a><a href="/about">About</a></nav>
    <div class="sidebar"><a href="/ad">Buy stuff</a></div>
    <article>
      <h1>The Great Post</h1>
      <p>This is the first paragraph of the article, with <strong>bold</strong> text, <em>emphasis</em>,
      and a <a href="/docs/guide">relative link</a> to follow. It is long enough to count as content.</p>
      <h2>Details</h2>
      <ul><li>First item</li><li>Second <code>item</code><ul><li>Nested</li></ul></li></ul>
      <ol start="3"><li>Three</li><li>Four</li></ol>
      <blockquote><p>Quoted wisdom goes here.</p></blockquote>
      <pre><code class="language-rust">fn main() {
    println!("hi");
}</code></pre>
      <table><thead><tr><th>Name</th><th>Age</th></tr></thead>
      <tbody><tr><td>Ann</td><td>30</td></tr><tr><td>Bob | B</td><td>41</td></tr></tbody></table>
      <p>Another paragraph that makes the article comfortably longer than two hundred characters so that the
      semantic container heuristics prefer it over the fallback scoring approach used for pages without article tags.</p>
      <script>alert(1)</script>
    </article>
    <footer>Copyright</footer></body></html>"#;

    fn conv(html: &str) -> Extracted {
        let base = Url::parse("https://example.com/blog/post").unwrap();
        html_to_markdown(html, Some(&base), &ExtractOptions::default())
    }

    #[test]
    fn extracts_article_and_drops_boilerplate() {
        let e = conv(ARTICLE);
        assert_eq!(e.title.as_deref(), Some("The Great Post | Blog"));
        assert_eq!(e.byline.as_deref(), Some("Ada Lovelace"));
        assert!(e.main_content_found);
        let md = &e.markdown;
        assert!(md.contains("# The Great Post"), "{md}");
        assert!(md.contains("**bold**"));
        assert!(md.contains("*emphasis*"));
        assert!(md.contains("[relative link](https://example.com/docs/guide)"));
        assert!(md.contains("## Details"));
        assert!(md.contains("- First item"));
        assert!(md.contains("- Second `item`"));
        assert!(md.contains("  - Nested"), "{md}");
        assert!(md.contains("3. Three"));
        assert!(md.contains("4. Four"));
        assert!(md.contains("> Quoted wisdom goes here."));
        assert!(
            md.contains("```rust\nfn main() {\n    println!(\"hi\");\n}\n```"),
            "{md}"
        );
        assert!(!md.contains("Buy stuff"));
        assert!(!md.contains("Copyright"));
        assert!(!md.contains("alert(1)"));
        assert!(!md.contains("Home"));
        assert_eq!(e.links.len(), 1);
        assert_eq!(e.links[0].url, "https://example.com/docs/guide");
    }

    /// Run on a small stack to prove the renderer never recurses per level.
    fn on_small_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        std::thread::Builder::new()
            .stack_size(1024 * 1024)
            .spawn(f)
            .unwrap()
            .join()
            .expect("no stack overflow")
    }

    #[test]
    fn deep_nesting_does_not_overflow() {
        let depth = 10_000;
        let html = format!(
            "<html><body>{}deep <a href=\"/x\">link</a> text{}</body></html>",
            "<div>".repeat(depth),
            "</div>".repeat(depth)
        );
        let e = on_small_stack(move || conv(&html));
        assert!(e.markdown.contains("deep"), "content survives flattening");
        assert!(e.markdown.contains("text"));
    }

    #[test]
    fn deep_nesting_of_inline_and_lists() {
        let depth = 2_000;
        for (open, close) in [
            ("<span>", "</span>"),
            ("<ul><li>", "</li></ul>"),
            ("<blockquote>", "</blockquote>"),
        ] {
            let html = format!(
                "<body>{}payload{}</body>",
                open.repeat(depth),
                close.repeat(depth)
            );
            let e = on_small_stack(move || conv(&html));
            assert!(e.markdown.contains("payload"), "{open}");
        }
    }

    #[test]
    fn many_nested_boilerplate_wrappers_are_fast() {
        let depth = 4_000;
        let html = format!(
            "<body><article><p>{}</p>{}x{}</article></body>",
            "word ".repeat(80),
            "<div class=\"menu share\">".repeat(depth),
            "</div>".repeat(depth)
        );
        let t = std::time::Instant::now();
        let _ = on_small_stack(move || conv(&html));
        assert!(t.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn tables_become_gfm() {
        let md = conv(ARTICLE).markdown;
        assert!(
            md.contains("| Name | Age |\n| --- | --- |\n| Ann | 30 |\n| Bob \\| B | 41 |"),
            "{md}"
        );
    }

    #[test]
    fn fallback_scoring_without_article_tag() {
        let filler = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt.";
        let html = format!(
            r#"<html><body><div id="top"><a href="/a">menu one</a> <a href="/b">menu two</a></div>
            <div class="post"><p>{filler}</p><p>{filler}</p><p>{filler}</p></div>
            <div class="links"><a href="/x">x</a><a href="/y">y</a><a href="/z">z</a></div></body></html>"#
        );
        let e = conv(&html);
        assert!(e.main_content_found);
        assert!(e.markdown.contains("Lorem ipsum"));
        assert!(!e.markdown.contains("menu one"));
    }

    #[test]
    fn hidden_and_broken_html() {
        let e = conv(
            "<p>visible text that is here<p style='display:none'>secret</p><div hidden>nope</div>",
        );
        assert!(e.markdown.contains("visible text"));
        assert!(!e.markdown.contains("secret"));
        assert!(!e.markdown.contains("nope"));
        let empty = conv("");
        assert_eq!(empty.markdown, "");
    }

    #[test]
    fn links_without_markdown_and_unsafe_schemes() {
        let html = r##"<body><p>See <a href="javascript:alert(1)">bad</a> and <a href="mailto:a@b.c">mail</a>
        and <a href="https://other.org/x?y=1#frag">good</a> and <a href="#top">top</a>.</p></body>"##;
        let base = Url::parse("https://example.com/").unwrap();
        let opts = ExtractOptions {
            include_links: false,
            ..ExtractOptions::default()
        };
        let e = html_to_markdown(html, Some(&base), &opts);
        assert!(
            e.markdown.contains("See bad and mail and good and top."),
            "{}",
            e.markdown
        );
        assert_eq!(e.links.len(), 1);
        assert_eq!(e.links[0].url, "https://other.org/x?y=1#frag");
    }
}
