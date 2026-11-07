//! Web research primitives for the agent: readable HTML to Markdown, web
//! search with pluggable backends, RSS/Atom and sitemap parsing, page
//! metadata (`OpenGraph`, JSON-LD), all behind an injectable SSRF-safe
//! [`UrlPolicy`].
//!
//! Wire into the daemon with [`register`] (or collect [`tools`]). The pure
//! parsers ([`html_to_markdown`], [`parse_feed`], [`parse_sitemap`],
//! [`extract_metadata`]) are usable without any network access.

// Flat declarative renderers; stylistic pedantic lints that add noise here.
#![allow(
    clippy::module_name_repetitions,
    clippy::too_many_lines,
    clippy::collapsible_if,
    clippy::format_push_string,
    clippy::match_same_arms,
    clippy::single_match_else,
    clippy::needless_pass_by_value
)]

pub mod error;
pub mod extract;
pub mod http;
pub mod metadata;
pub mod policy;
pub mod search;
pub mod tools;
pub mod xmlfeed;

pub use error::WebError;
pub use extract::{ExtractOptions, Extracted, Link, html_to_markdown};
pub use http::{Fetched, GetOptions, HttpConfig, HttpFetcher};
pub use metadata::{FeedLink, PageMetadata, extract_metadata};
pub use policy::{DefaultUrlPolicy, PolicyViolation, TrustedHostPolicy, UrlPolicy};
pub use search::{
    BraveBackend, DuckDuckGoBackend, MultiSearch, SearchBackend, SearchConfig, SearchOutcome,
    SearchQuery, SearchResult, SearxngBackend,
};
pub use tools::{TOOL_NAMES, WebToolsConfig, register, tools};
pub use xmlfeed::{
    Feed, FeedEntry, FeedKind, Sitemap, SitemapEntry, SitemapKind, parse_feed, parse_sitemap,
};
