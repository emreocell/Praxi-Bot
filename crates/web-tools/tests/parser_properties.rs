//! Property-based tests for untrusted web text parsers (`html_to_markdown`,
//! `extract_metadata`, `parse_feed`, `parse_sitemap`, `parse_content_type`).

use std::fmt::Write as _;

use proptest::collection::vec;
use proptest::prelude::*;
use proptest::sample::select;
use proptest::test_runner::RngSeed;
use url::Url;
use web_tools::{
    ExtractOptions, SitemapKind, extract_metadata, html_to_markdown, http::parse_content_type,
    parse_feed, parse_sitemap,
};

fn config() -> ProptestConfig {
    let mut config = ProptestConfig::default();
    if config.rng_seed == RngSeed::Random {
        config.rng_seed = RngSeed::Fixed(461);
    }
    config
}

fn untrusted_markup() -> impl Strategy<Value = String> {
    const TOKENS: &[&str] = &[
        "<",
        ">",
        "</",
        "/>",
        "<!--",
        "-->",
        "<![CDATA[",
        "]]>",
        "\u{feff}",
        "\0",
        "&amp;",
        "&#x0;",
        "<script>",
        "</script>",
        "<title>",
        "</title>",
        "<meta name=\"description\" content=\"",
        "<link rel=\"canonical\" href=\"",
        "<script type=\"application/ld+json\">",
        "<rss><channel><item><title>",
        "<feed><entry><title>",
        "<urlset><url><loc>",
        "https://example.com/a",
        "\n",
        "İıŞşĞğÜüÖöÇç🦀",
    ];
    let piece = prop_oneof![
        3 => select(TOKENS).prop_map(str::to_owned),
        2 => ".{0,32}",
    ];
    vec(piece, 0..=32).prop_map(|parts| parts.concat())
}

proptest! {
    #![proptest_config(config())]

    /// All web HTML, metadata, XML feed, sitemap, and Content-Type parsers tolerate arbitrary UTF-8 without panicking.
    #[test]
    fn untrusted_web_parsers_never_panic_on_arbitrary_utf8(input in untrusted_markup()) {
        let base = Url::parse("https://example.com/dir/page.html").unwrap();
        let _ = html_to_markdown(&input, Some(&base), &ExtractOptions::default());
        let _ = html_to_markdown(&input, None, &ExtractOptions { full_page: true, ..ExtractOptions::default() });
        let _ = extract_metadata(&input, Some(&base));
        let _ = parse_feed(&input, Some(&base));
        let _ = parse_sitemap(&input);
        let _ = parse_content_type(Some(&input));
    }

    /// HTML extraction never returns more links than `max_links` and strips `<script>` and `<style>` bodies.
    #[test]
    fn html_to_markdown_respects_max_links_and_strips_scripts(
        links_count in 0..=16_usize,
        max_links in 0..=8_usize,
        body_word in "[a-zA-Z0-9]{3,12}",
    ) {
        let base = Url::parse("https://example.com/").unwrap();
        let mut html = String::from(
            "<html><head><title>T</title><script>SECRET_SCRIPT_PAYLOAD_461</script>\
             <style>SECRET_STYLE_PAYLOAD_461</style></head><body><article><p>",
        );
        html.push_str(&body_word);
        for i in 0..links_count {
            let _ = write!(html, " <a href=\"/p/{i}\">link{i}</a>");
        }
        html.push_str("</p></article></body></html>");
        let opts = ExtractOptions {
            max_links,
            full_page: true,
            ..ExtractOptions::default()
        };
        let extracted = html_to_markdown(&html, Some(&base), &opts);
        prop_assert!(extracted.links.len() <= max_links);
        prop_assert!(!extracted.markdown.contains("SECRET_SCRIPT_PAYLOAD_461"));
        prop_assert!(!extracted.markdown.contains("SECRET_STYLE_PAYLOAD_461"));
        prop_assert!(extracted.markdown.contains(&body_word));
    }

    /// `parse_content_type` lowercases and trims media type and charset across arbitrary ASCII case, quotes, and whitespace.
    #[test]
    fn parse_content_type_normalizes_media_type_and_charset(
        media_type in "[A-Za-z]{2,12}/[A-Za-z0-9.+-]{2,16}",
        charset in "[A-Za-z0-9_-]{2,12}",
        quoted in any::<bool>(),
        space in select(&["", " ", "\t"][..]),
    ) {
        let cs = if quoted {
            format!("\"{charset}\"")
        } else {
            charset.clone()
        };
        let header = format!("{space}{media_type}{space};{space}ChArSeT={cs}{space}");
        let (actual_media, actual_charset) = parse_content_type(Some(&header));
        prop_assert_eq!(actual_media, Some(media_type.to_ascii_lowercase()));
        prop_assert_eq!(actual_charset, Some(charset.to_ascii_lowercase()));
    }

    /// Plain-text sitemaps retain only `http://` and `https://` lines in order.
    #[test]
    fn parse_sitemap_extracts_plain_text_http_urls(
        paths in vec("[a-z0-9/_-]{1,24}", 1..=12),
        noise in "[a-zA-Z0-9 _.-]{0,24}",
    ) {
        let expected: Vec<String> = paths
            .iter()
            .enumerate()
            .map(|(i, p)| {
                if i % 2 == 0 {
                    format!("https://example.com/{p}")
                } else {
                    format!("http://example.com/{p}")
                }
            })
            .collect();
        let mut text = String::from("\u{feff}");
        for url in &expected {
            let _ = write!(text, "  {noise}\n  {url}  \nftp://example.com/ignored\n");
        }
        let sitemap = parse_sitemap(&text).unwrap();
        prop_assert_eq!(sitemap.kind, SitemapKind::Text);
        let locs: Vec<String> = sitemap.entries.into_iter().map(|e| e.loc).collect();
        prop_assert_eq!(locs, expected);
    }
}
