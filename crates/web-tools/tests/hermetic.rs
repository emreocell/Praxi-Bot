#![allow(clippy::format_push_string)]
//! Hermetic end-to-end tests against a local wiremock server. No live network.

use std::net::IpAddr;
use std::sync::Arc;

use secret_store::{MemoryStore, SecretRef, SecretStore};
use serde_json::{Value, json};
use tool_registry::{ContentPart, ToolContext, ToolRegistry};
use url::Url;
use web_tools::{
    DefaultUrlPolicy, GetOptions, HttpConfig, HttpFetcher, PolicyViolation, SearchConfig,
    TOOL_NAMES, UrlPolicy, WebError, WebToolsConfig,
};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn loopback_ok() -> DefaultUrlPolicy {
    DefaultUrlPolicy {
        block_loopback: false,
        ..DefaultUrlPolicy::default()
    }
}

fn cfg(policy: DefaultUrlPolicy) -> WebToolsConfig {
    WebToolsConfig {
        policy: Arc::new(policy),
        search: SearchConfig {
            duckduckgo: false,
            ..SearchConfig::default()
        },
        ..WebToolsConfig::default()
    }
}

async fn call(reg: &ToolRegistry, name: &str, input: Value) -> Result<Vec<ContentPart>, String> {
    call_ctx(reg, name, input, &ToolContext::default())
        .await
        .map(|o| o.content)
}

async fn call_ctx(
    reg: &ToolRegistry,
    name: &str,
    input: Value,
    ctx: &ToolContext,
) -> Result<tool_registry::ToolOutput, String> {
    let tool = reg.get(name).expect("tool registered");
    tool.call(ctx, input).await.map_err(|e| e.to_string())
}

fn text(parts: &[ContentPart]) -> String {
    parts
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn json_part(parts: &[ContentPart]) -> Value {
    parts
        .iter()
        .find_map(|p| match p {
            ContentPart::Json { data } => Some(data.clone()),
            _ => None,
        })
        .expect("json part")
}

fn html(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(body.as_bytes().to_vec(), "text/html; charset=utf-8")
}

#[test]
fn registers_five_tools_with_builtin_conventions() {
    let reg = ToolRegistry::new();
    let added = web_tools::register(&reg, WebToolsConfig::default()).unwrap();
    assert_eq!(added, TOOL_NAMES);
    for name in TOOL_NAMES {
        let spec = reg.get(name).unwrap().spec().clone();
        assert!(spec.is_valid());
        assert_eq!(spec.category, tool_registry::PermissionCategory::Browser);
        assert_eq!(spec.effect, tool_registry::Effect::Read);
    }
    // Registering twice adds nothing.
    assert_eq!(
        web_tools::register(&reg, WebToolsConfig::default()).unwrap(),
        Vec::<String>::new()
    );
}

#[tokio::test]
async fn web_extract_returns_markdown_with_untrusted_label() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/post"))
        .respond_with(html(
            r#"<html><head><title>Post</title></head><body><nav><a href="/x">nav</a></nav>
            <article><h1>Post</h1><p>Body text that is long enough to be treated as the real content of this page,
            with a <a href="/other">link</a> and more words to pass the two hundred character threshold easily,
            because the semantic container heuristic needs real text to prefer an article element.</p></article></body></html>"#,
        ))
        .mount(&server)
        .await;
    let reg = ToolRegistry::new();
    web_tools::register(&reg, cfg(loopback_ok())).unwrap();
    let out = call(
        &reg,
        "web_extract",
        json!({"url": format!("{}/post", server.uri()), "list_links": true}),
    )
    .await
    .unwrap();
    let t = text(&out);
    assert!(t.starts_with("[untrusted external content"), "{t}");
    assert!(t.contains("Title: Post"));
    assert!(t.contains(&format!("[link]({}/other)", server.uri())));
    assert!(t.contains("## Links"));
    assert!(!t.contains("nav"));
}

#[tokio::test]
async fn default_policy_blocks_loopback_and_bad_schemes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(html("<p>hi</p>"))
        .mount(&server)
        .await;
    let reg = ToolRegistry::new();
    web_tools::register(&reg, cfg(DefaultUrlPolicy::default())).unwrap();
    let err = call(&reg, "web_extract", json!({"url": server.uri()}))
        .await
        .unwrap_err();
    assert!(err.contains("blocked by URL policy"), "{err}");
    let err = call(&reg, "web_extract", json!({"url": "file:///etc/passwd"}))
        .await
        .unwrap_err();
    assert!(err.contains("scheme"), "{err}");
    let err = call(
        &reg,
        "web_metadata",
        json!({"url": "http://169.254.169.254/latest/meta-data"}),
    )
    .await
    .unwrap_err();
    assert!(err.contains("blocked"), "{err}");
    let err = call(&reg, "web_extract", json!({"url": "not a url"}))
        .await
        .unwrap_err();
    assert!(err.contains("invalid"), "{err}");
}

#[tokio::test]
async fn redirect_to_metadata_ip_is_blocked() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/r"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", "http://169.254.169.254/latest/meta-data/"),
        )
        .mount(&server)
        .await;
    let f = HttpFetcher::new(Arc::new(loopback_ok()), HttpConfig::default()).unwrap();
    let err = f
        .get(&format!("{}/r", server.uri()), &GetOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(err, WebError::Policy(_)), "{err:?}");
}

#[tokio::test]
async fn redirect_to_protected_daemon_port_is_blocked() {
    let server = MockServer::start().await;
    let daemon = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(html("daemon secrets"))
        .mount(&daemon)
        .await;
    Mock::given(method("GET"))
        .and(path("/r"))
        .respond_with(
            ResponseTemplate::new(301).insert_header("location", format!("{}/api", daemon.uri())),
        )
        .mount(&server)
        .await;
    let policy = DefaultUrlPolicy {
        block_loopback: false,
        blocked_sockets: vec![*daemon.address()],
        ..DefaultUrlPolicy::default()
    };
    let f = HttpFetcher::new(Arc::new(policy), HttpConfig::default()).unwrap();
    let err = f
        .get(&format!("{}/r", server.uri()), &GetOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(err, WebError::Policy(_)), "{err:?}");
    assert!(daemon.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn redirects_are_followed_and_limited() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/a"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/b"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/b"))
        .respond_with(html("<p>done</p>"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/loop"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/loop"))
        .mount(&server)
        .await;
    let f = HttpFetcher::new(Arc::new(loopback_ok()), HttpConfig::default()).unwrap();
    let ok = f
        .get(&format!("{}/a", server.uri()), &GetOptions::default())
        .await
        .unwrap();
    assert_eq!(ok.redirects, 1);
    assert_eq!(ok.url.path(), "/b");
    let err = f
        .get(&format!("{}/loop", server.uri()), &GetOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(err, WebError::TooManyRedirects(5)), "{err:?}");
}

/// Passes `check_url` for hostnames but applies the default address checks to
/// DNS results: models a hostname that resolves to a loopback address.
struct DnsOnly(DefaultUrlPolicy);
impl UrlPolicy for DnsOnly {
    fn check_url(&self, _url: &Url) -> Result<(), PolicyViolation> {
        Ok(())
    }
    fn check_ip(&self, host: &str, ip: IpAddr) -> Result<(), PolicyViolation> {
        self.0.check_ip(host, ip)
    }
}

#[tokio::test]
async fn hostname_resolving_to_loopback_is_blocked_after_dns() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(html("<p>internal</p>"))
        .mount(&server)
        .await;
    let port = server.address().port();
    let f = HttpFetcher::new(
        Arc::new(DnsOnly(DefaultUrlPolicy::default())),
        HttpConfig::default(),
    )
    .unwrap();
    let err = f
        .get(&format!("http://localhost:{port}/"), &GetOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(err, WebError::Policy(_)), "{err:?}");
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn oversize_body_is_cut_and_spilled() {
    let server = MockServer::start().await;
    let para =
        "<p>This is a reasonably long paragraph of filler text, with commas, to be extracted.</p>";
    let big = format!(
        "<html><body><article>{}</article></body></html>",
        para.repeat(400)
    );
    Mock::given(method("GET"))
        .respond_with(html(&big))
        .mount(&server)
        .await;
    let spill = tempfile::tempdir().unwrap();
    let mut c = cfg(loopback_ok());
    c.http.max_bytes = 20_000;
    c.spill_dir = Some(spill.path().to_path_buf());
    let reg = ToolRegistry::new();
    web_tools::register(&reg, c).unwrap();
    let ctx = ToolContext {
        run_id: "t".into(),
        max_inline_bytes: 2_000,
        ..Default::default()
    };
    let out = call_ctx(&reg, "web_extract", json!({"url": server.uri()}), &ctx)
        .await
        .unwrap();
    let t = text(&out.content);
    assert!(
        t.contains("truncated"),
        "{}",
        &t[t.len().saturating_sub(200)..]
    );
    assert!(t.len() < 2_500);
    let spill_ref = out.spill.expect("spilled");
    let full = std::fs::read_to_string(&spill_ref.path).unwrap();
    assert!(full.contains("Note: content truncated by size limits."));
    assert!(
        full.len() > 2_000 && full.len() < 22_000,
        "capped near max_bytes: {}",
        full.len()
    );
}

#[tokio::test]
async fn non_html_content_types_are_refused_clearly() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/img"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(vec![0u8; 64], "image/png"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/txt"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw("plain words".as_bytes().to_vec(), "text/plain"),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/gone"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let reg = ToolRegistry::new();
    web_tools::register(&reg, cfg(loopback_ok())).unwrap();
    let err = call(
        &reg,
        "web_extract",
        json!({"url": format!("{}/img", server.uri())}),
    )
    .await
    .unwrap_err();
    assert!(
        err.contains("unsupported content type `image/png`"),
        "{err}"
    );
    let t = text(
        &call(
            &reg,
            "web_extract",
            json!({"url": format!("{}/txt", server.uri())}),
        )
        .await
        .unwrap(),
    );
    assert!(t.ends_with("plain words"));
    let err = call(
        &reg,
        "web_extract",
        json!({"url": format!("{}/gone", server.uri())}),
    )
    .await
    .unwrap_err();
    assert!(err.contains("HTTP 404"), "{err}");
}

#[tokio::test]
async fn feed_sitemap_and_metadata_tools() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/feed.xml"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"<rss version="2.0"><channel><title>T</title><link>/</link>
            <item><title>A</title><link>/a</link></item><item><title>B</title><link>/b</link></item></channel></rss>"#
                .as_bytes()
                .to_vec(),
            "application/rss+xml",
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/sitemap.xml"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9"><url><loc>https://e.com/1</loc></url>
            <url><loc>https://e.com/2</loc></url><url><loc>https://e.com/3</loc></url></urlset>"#
                .as_bytes()
                .to_vec(),
            "application/xml",
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/page"))
        .respond_with(html(
            r#"<html lang="tr"><head><title>Sayfa</title><meta property="og:title" content="OG">
            <script type="application/ld+json">{"@type":"Article","headline":"H"}</script></head><body></body></html>"#,
        ))
        .mount(&server)
        .await;
    let reg = ToolRegistry::new();
    web_tools::register(&reg, cfg(loopback_ok())).unwrap();
    let base = server.uri();

    let f = json_part(
        &call(
            &reg,
            "web_feed",
            json!({"url": format!("{base}/feed.xml"), "limit": 1}),
        )
        .await
        .unwrap(),
    );
    assert_eq!(f["entries"].as_array().unwrap().len(), 1);
    assert_eq!(f["total_entries"], 2);
    assert_eq!(f["entries"][0]["link"], format!("{base}/a"));
    assert_eq!(f["untrusted_external_content"], true);

    let s = json_part(
        &call(
            &reg,
            "web_sitemap",
            json!({"url": format!("{base}/sitemap.xml"), "limit": 2}),
        )
        .await
        .unwrap(),
    );
    assert_eq!(s["kind"], "url_set");
    assert_eq!(s["entries"].as_array().unwrap().len(), 2);
    assert_eq!(s["total_entries"], 3);

    let m = json_part(
        &call(&reg, "web_metadata", json!({"url": format!("{base}/page")}))
            .await
            .unwrap(),
    );
    assert_eq!(m["title"], "Sayfa");
    assert_eq!(m["language"], "tr");
    assert_eq!(m["open_graph"]["title"], "OG");
    assert_eq!(m["json_ld"][0]["headline"], "H");

    let err = call(&reg, "web_feed", json!({"url": format!("{base}/page")}))
        .await
        .unwrap_err();
    assert!(err.contains("parse error"), "{err}");
}

#[tokio::test]
async fn searxng_backend_normalises_results() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .and(query_param("q", "rust async"))
        .and(query_param("format", "json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results": [
            {"title": "Tokio", "url": "https://tokio.rs/", "content": "An <b>async</b> runtime"},
            {"title": "Dup", "url": "https://tokio.rs", "content": "dup"},
            {"title": "Bad", "url": "javascript:x", "content": ""},
            {"title": "Book", "url": "https://rust-lang.github.io/async-book/", "content": "The book"}
        ]})))
        .mount(&server)
        .await;
    // Default policy: loopback is blocked, but the configured SearXNG host is trusted.
    let mut c = cfg(DefaultUrlPolicy::default());
    c.search.searxng_url = Some(server.uri());
    let reg = ToolRegistry::new();
    web_tools::register(&reg, c).unwrap();
    let out = call(
        &reg,
        "web_search",
        json!({"query": "rust   async", "limit": 5}),
    )
    .await
    .unwrap();
    let t = text(&out);
    assert!(t.starts_with("[untrusted external content"));
    assert!(t.contains("1. Tokio"));
    assert!(t.contains("An async runtime"));
    let j = json_part(&out);
    assert_eq!(j["backend"], "searxng");
    let r = j["results"].as_array().unwrap();
    assert_eq!(r.len(), 2, "duplicate and javascript: URL dropped");
    assert_eq!(r[1]["rank"], 2);
}

#[tokio::test]
async fn brave_backend_uses_secret_ref_and_header() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/res/v1/web/search"))
        .and(header("X-Subscription-Token", "tok-123"))
        .and(query_param("q", "hello"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"web": {"results": [
            {"title": "Hello", "url": "https://example.org/h", "description": "<strong>Hi</strong> there"}
        ]}})))
        .mount(&server)
        .await;
    let store = Arc::new(MemoryStore::new());
    let mut c = cfg(loopback_ok());
    c.secrets = Some(store.clone());
    c.search.brave_key = Some(SecretRef::new("brave_api_key").unwrap());
    c.search.brave_endpoint = Some(format!("{}/res/v1/web/search", server.uri()));
    let reg = ToolRegistry::new();
    web_tools::register(&reg, c).unwrap();

    // Secret missing: clear error that does not leak anything.
    let err = call(&reg, "web_search", json!({"query": "hello"}))
        .await
        .unwrap_err();
    assert!(err.contains("brave_api_key"), "{err}");

    store.set("brave_api_key", "tok-123").unwrap();
    let out = call(
        &reg,
        "web_search",
        json!({"query": "hello", "backend": "brave"}),
    )
    .await
    .unwrap();
    assert!(text(&out).contains("Hi there"));
    assert!(!text(&out).contains("tok-123"));

    // Wrong key: the API's 401 maps to a readable error without echoing the key.
    store.set("brave_api_key", "wrong").unwrap();
    let err = call(&reg, "web_search", json!({"query": "hello"}))
        .await
        .unwrap_err();
    assert!(!err.contains("wrong"), "{err}");
}

#[tokio::test]
async fn brave_key_requires_secret_store() {
    let mut c = cfg(loopback_ok());
    c.search.brave_key = Some(SecretRef::new("k").unwrap());
    assert!(matches!(web_tools::tools(c), Err(WebError::Config(_))));
}

#[tokio::test]
async fn duckduckgo_is_the_fallback_when_others_fail() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/html/"))
        .and(query_param("q", "fallback"))
        .respond_with(html(
            r#"<div class="result"><a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.net%2F">Net</a>
            <a class="result__snippet">snip</a></div>"#,
        ))
        .mount(&server)
        .await;
    let mut c = cfg(loopback_ok());
    c.search.searxng_url = Some(server.uri());
    c.search.duckduckgo = true;
    c.search.duckduckgo_endpoint = Some(format!("{}/html/", server.uri()));
    let reg = ToolRegistry::new();
    web_tools::register(&reg, c).unwrap();
    let out = call(&reg, "web_search", json!({"query": "fallback"}))
        .await
        .unwrap();
    let t = text(&out);
    assert!(t.contains("via duckduckgo"), "{t}");
    assert!(
        t.contains("(fallback: searxng: server returned HTTP 500)"),
        "{t}"
    );
    assert!(t.contains("https://example.net/"));

    let err = call(
        &reg,
        "web_search",
        json!({"query": "x", "backend": "brave"}),
    )
    .await
    .unwrap_err();
    assert!(err.contains("no search backend named"), "{err}");
}

#[tokio::test]
async fn caller_headers_are_not_forwarded_across_origins() {
    let origin = MockServer::start().await;
    let other = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(html("ok"))
        .mount(&other)
        .await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("location", format!("{}/x", other.uri())),
        )
        .mount(&origin)
        .await;
    let f = HttpFetcher::new(Arc::new(loopback_ok()), HttpConfig::default()).unwrap();
    let mut o = GetOptions::default();
    o.headers
        .push(("X-Subscription-Token".into(), "secret".into()));
    f.get(&origin.uri(), &o).await.unwrap();
    let seen = other.received_requests().await.unwrap();
    assert_eq!(seen.len(), 1);
    assert!(seen[0].headers.get("x-subscription-token").is_none());
    let first = origin.received_requests().await.unwrap();
    assert!(first[0].headers.get("x-subscription-token").is_some());
}

#[tokio::test]
async fn large_sitemap_and_feed_json_respect_inline_budget_and_spill() {
    let server = MockServer::start().await;
    let mut urls = String::new();
    for i in 0..3000 {
        urls.push_str(&format!(
            "<url><loc>https://example.com/page/{i}</loc></url>"
        ));
    }
    let sitemap = format!(
        r#"<?xml version="1.0"?><urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">{urls}</urlset>"#
    );
    let blob = "lorem ipsum dolor sit amet ".repeat(200);
    let mut items = String::new();
    for i in 0..100 {
        items.push_str(&format!(
            "<item><title>t{i}</title><link>https://example.com/{i}</link><description>{blob}</description></item>"
        ));
    }
    let feed = format!(
        r#"<?xml version="1.0"?><rss version="2.0"><channel><title>f</title>{items}</channel></rss>"#
    );
    Mock::given(method("GET"))
        .and(path("/sitemap.xml"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sitemap, "application/xml"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/feed.xml"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(feed, "application/rss+xml"))
        .mount(&server)
        .await;
    let spill = tempfile::tempdir().unwrap();
    let mut c = cfg(loopback_ok());
    c.spill_dir = Some(spill.path().to_path_buf());
    let reg = ToolRegistry::new();
    web_tools::register(&reg, c).unwrap();
    let ctx = ToolContext {
        run_id: "t".into(),
        max_inline_bytes: 4_000,
        ..Default::default()
    };
    for (tool, file, limit) in [
        ("web_sitemap", "sitemap.xml", 3000),
        ("web_feed", "feed.xml", 100),
    ] {
        let out = call_ctx(
            &reg,
            tool,
            json!({"url": format!("{}/{file}", server.uri()), "limit": limit}),
            &ctx,
        )
        .await
        .unwrap();
        let v = json_part(&out.content);
        assert!(v.to_string().len() <= 4_000, "{tool} inline too large");
        assert_eq!(v["truncated"], true, "{tool}");
        assert!(v["inline_entries"].as_u64().unwrap() < limit);
        let sp = out.spill.expect("spilled");
        assert_eq!(sp.mime_type, "application/json");
        let full: Value =
            serde_json::from_str(&std::fs::read_to_string(&sp.path).unwrap()).unwrap();
        assert_eq!(
            full["entries"].as_array().unwrap().len(),
            usize::try_from(limit).unwrap()
        );
    }
}

#[tokio::test]
async fn overlong_urls_and_queries_are_refused_before_any_request() {
    let reg = ToolRegistry::new();
    web_tools::register(&reg, cfg(DefaultUrlPolicy::default())).unwrap();
    let long_url = format!("https://example.org/?q={}", "a".repeat(2100));
    for name in ["web_extract", "web_metadata", "web_feed", "web_sitemap"] {
        let err = call(&reg, name, json!({"url": long_url}))
            .await
            .unwrap_err();
        assert!(err.contains("too long"), "{name}: {err}");
    }
    let err = call(&reg, "web_search", json!({"query": "q".repeat(401)}))
        .await
        .unwrap_err();
    assert!(err.contains("too long"), "{err}");
    // At the limit the call gets past the length check (and fails later, offline).
    let err = call(&reg, "web_search", json!({"query": "q".repeat(400)}))
        .await
        .unwrap_err();
    assert!(!err.contains("too long"), "{err}");
}
