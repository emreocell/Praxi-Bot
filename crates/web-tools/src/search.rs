//! `web_search` with pluggable backends and normalised results.
//!
//! Backends: a self-hosted `SearXNG` instance (JSON API), the Brave Search API
//! (API key resolved from a secret reference at call time, never stored in the
//! config) and the key-less `DuckDuckGo` HTML endpoint as fallback.
//! [`MultiSearch`] tries them in order and returns the first one that yields
//! results.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use scraper::{Html, Selector};
use secret_store::{SecretRef, SecretStore};
use serde::Deserialize;
use url::Url;

use crate::error::WebError;
use crate::http::{GetOptions, HTML_TYPES, HttpConfig, HttpFetcher, JSON_TYPES};
use crate::policy::TrustedHostPolicy;

/// Hard upper bound for results per query.
pub const MAX_RESULTS: usize = 25;
const MAX_QUERY_CHARS: usize = 500;

/// One normalised search hit.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SearchResult {
    /// 1-based position in the merged list.
    pub rank: usize,
    /// Result title (plain text).
    pub title: String,
    /// Absolute http(s) URL.
    pub url: String,
    /// Snippet (plain text, may be empty).
    pub snippet: String,
    /// Backend that produced the hit (`searxng`, `brave`, `duckduckgo`).
    pub source: String,
}

/// A search request.
#[derive(Debug, Clone)]
pub struct SearchQuery {
    /// Query text.
    pub query: String,
    /// Maximum results (clamped to `1..=MAX_RESULTS`).
    pub limit: usize,
    /// Preferred result language (BCP 47 such as `en` or `tr`), if any.
    pub language: Option<String>,
}

impl SearchQuery {
    /// Query with the default limit of 10.
    #[must_use]
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            limit: 10,
            language: None,
        }
    }

    fn validated(&self) -> Result<(String, usize), WebError> {
        let q = self.query.split_whitespace().collect::<Vec<_>>().join(" ");
        if q.is_empty() {
            return Err(WebError::Search("query is empty".into()));
        }
        if q.chars().count() > MAX_QUERY_CHARS {
            return Err(WebError::Search(format!(
                "query longer than {MAX_QUERY_CHARS} characters"
            )));
        }
        Ok((q, self.limit.clamp(1, MAX_RESULTS)))
    }
}

/// A search provider.
#[async_trait]
pub trait SearchBackend: Send + Sync {
    /// Short stable name used in results and `backend` selection.
    fn name(&self) -> &'static str;

    /// Run the query.
    ///
    /// # Errors
    /// Transport, policy, quota or parse failures.
    async fn search(&self, query: &SearchQuery) -> Result<Vec<SearchResult>, WebError>;
}

fn plain(s: &str) -> String {
    let text = if s.contains('<') || s.contains('&') {
        Html::parse_fragment(s)
            .root_element()
            .text()
            .collect::<String>()
    } else {
        s.to_string()
    };
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn make(source: &str, title: &str, url: &str, snippet: &str) -> Option<SearchResult> {
    let url = Url::parse(url.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    Some(SearchResult {
        rank: 0,
        title: plain(title),
        url: url.to_string(),
        snippet: plain(snippet),
        source: source.to_string(),
    })
}

/// Drop duplicate URLs (ignoring a trailing slash and fragment), number the rest.
fn finalize(mut hits: Vec<SearchResult>, limit: usize) -> Vec<SearchResult> {
    let mut seen = HashSet::new();
    hits.retain(|h| {
        let mut key = h.url.split('#').next().unwrap_or(&h.url).to_string();
        if key.ends_with('/') {
            key.pop();
        }
        seen.insert(key)
    });
    hits.truncate(limit);
    for (i, h) in hits.iter_mut().enumerate() {
        h.rank = i + 1;
    }
    hits
}

// ---------------------------------------------------------------- SearXNG

/// Self-hosted `SearXNG` (needs `search.formats: [json]` enabled).
pub struct SearxngBackend {
    base: Url,
    fetcher: HttpFetcher,
}

impl SearxngBackend {
    /// `base` is the instance root, for example `http://localhost:8080`.
    /// The fetcher's policy must allow the instance host.
    #[must_use]
    pub fn new(base: Url, fetcher: HttpFetcher) -> Self {
        Self { base, fetcher }
    }
}

#[derive(Deserialize)]
struct SearxResponse {
    #[serde(default)]
    results: Vec<SearxHit>,
}
#[derive(Deserialize)]
struct SearxHit {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    content: String,
}

#[async_trait]
impl SearchBackend for SearxngBackend {
    fn name(&self) -> &'static str {
        "searxng"
    }

    async fn search(&self, query: &SearchQuery) -> Result<Vec<SearchResult>, WebError> {
        let (q, limit) = query.validated()?;
        let mut url = self
            .base
            .join("search")
            .map_err(|e| WebError::Config(e.to_string()))?;
        {
            let mut p = url.query_pairs_mut();
            p.append_pair("q", &q).append_pair("format", "json");
            if let Some(l) = &query.language {
                p.append_pair("language", l);
            }
        }
        let page = self
            .fetcher
            .get(url.as_str(), &GetOptions::accepting(JSON_TYPES))
            .await?;
        if page.truncated {
            return Err(WebError::Search("searxng response too large".into()));
        }
        let parsed: SearxResponse = serde_json::from_slice(&page.body)
            .map_err(|e| WebError::Search(format!("searxng: bad JSON ({e})")))?;
        let hits = parsed
            .results
            .iter()
            .filter_map(|h| make("searxng", &h.title, &h.url, &h.content))
            .collect();
        Ok(finalize(hits, limit))
    }
}

// ------------------------------------------------------------------ Brave

/// Brave Search API. The subscription token is resolved from the secret store
/// on every call, so rotating the secret needs no restart.
pub struct BraveBackend {
    endpoint: Url,
    fetcher: HttpFetcher,
    secrets: Arc<dyn SecretStore>,
    key: SecretRef,
}

/// Default Brave web-search endpoint.
pub const BRAVE_ENDPOINT: &str = "https://api.search.brave.com/res/v1/web/search";

impl BraveBackend {
    /// Build a backend. `endpoint` defaults to [`BRAVE_ENDPOINT`].
    ///
    /// # Errors
    /// [`WebError::Config`] when the endpoint is not a URL.
    pub fn new(
        endpoint: Option<&str>,
        fetcher: HttpFetcher,
        secrets: Arc<dyn SecretStore>,
        key: SecretRef,
    ) -> Result<Self, WebError> {
        let endpoint = Url::parse(endpoint.unwrap_or(BRAVE_ENDPOINT))
            .map_err(|e| WebError::Config(format!("brave endpoint: {e}")))?;
        Ok(Self {
            endpoint,
            fetcher,
            secrets,
            key,
        })
    }
}

#[derive(Deserialize)]
struct BraveResponse {
    web: Option<BraveWeb>,
}
#[derive(Deserialize)]
struct BraveWeb {
    #[serde(default)]
    results: Vec<BraveHit>,
}
#[derive(Deserialize)]
struct BraveHit {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    description: String,
}

#[async_trait]
impl SearchBackend for BraveBackend {
    fn name(&self) -> &'static str {
        "brave"
    }

    async fn search(&self, query: &SearchQuery) -> Result<Vec<SearchResult>, WebError> {
        let (q, limit) = query.validated()?;
        let token = secret_store::resolve(self.secrets.as_ref(), &self.key).map_err(|_| {
            WebError::Config(format!(
                "secret `{}` for brave is not available",
                self.key.name()
            ))
        })?;
        let mut url = self.endpoint.clone();
        {
            let mut p = url.query_pairs_mut();
            p.append_pair("q", &q)
                .append_pair("count", &limit.to_string());
            if let Some(l) = &query.language {
                p.append_pair("search_lang", l);
            }
        }
        let mut opts = GetOptions::accepting(JSON_TYPES);
        opts.headers
            .push(("X-Subscription-Token".into(), token.expose().to_string()));
        opts.headers
            .push(("Accept".into(), "application/json".into()));
        let page = self
            .fetcher
            .get(url.as_str(), &opts)
            .await
            .map_err(|e| match e {
                WebError::Status(401 | 403) => {
                    WebError::Search("brave rejected the API key".into())
                }
                WebError::Status(429) => WebError::Search("brave rate limit reached".into()),
                other => other,
            })?;
        if page.truncated {
            return Err(WebError::Search("brave response too large".into()));
        }
        let parsed: BraveResponse = serde_json::from_slice(&page.body)
            .map_err(|e| WebError::Search(format!("brave: bad JSON ({e})")))?;
        let hits = parsed
            .web
            .map(|w| w.results)
            .unwrap_or_default()
            .iter()
            .filter_map(|h| make("brave", &h.title, &h.url, &h.description))
            .collect();
        Ok(finalize(hits, limit))
    }
}

// ------------------------------------------------------------- DuckDuckGo

/// Key-less `DuckDuckGo` HTML endpoint. Best effort: the markup is not an API
/// and may change or start serving a challenge page.
pub struct DuckDuckGoBackend {
    endpoint: Url,
    fetcher: HttpFetcher,
}

/// Default `DuckDuckGo` HTML endpoint.
pub const DUCKDUCKGO_ENDPOINT: &str = "https://html.duckduckgo.com/html/";

impl DuckDuckGoBackend {
    /// `endpoint` defaults to [`DUCKDUCKGO_ENDPOINT`].
    ///
    /// # Errors
    /// [`WebError::Config`] when the endpoint is not a URL.
    pub fn new(endpoint: Option<&str>, fetcher: HttpFetcher) -> Result<Self, WebError> {
        let endpoint = Url::parse(endpoint.unwrap_or(DUCKDUCKGO_ENDPOINT))
            .map_err(|e| WebError::Config(format!("duckduckgo endpoint: {e}")))?;
        Ok(Self { endpoint, fetcher })
    }
}

/// Unwrap `//duckduckgo.com/l/?uddg=<encoded>` redirect links.
fn ddg_target(href: &str, base: &Url) -> Option<String> {
    let abs = base.join(href).ok()?;
    if abs.path().starts_with("/l/") {
        return abs
            .query_pairs()
            .find(|(k, _)| k == "uddg")
            .map(|(_, v)| v.into_owned());
    }
    Some(abs.to_string())
}

/// Parse the `DuckDuckGo` HTML result page.
fn parse_ddg(html: &str, base: &Url) -> Vec<SearchResult> {
    let doc = Html::parse_document(html);
    let result = Selector::parse("div.result, div.web-result").expect("selector");
    let title = Selector::parse("a.result__a").expect("selector");
    let snippet = Selector::parse(".result__snippet").expect("selector");
    doc.select(&result)
        .filter(|r| !r.value().classes().any(|c| c == "result--ad"))
        .filter_map(|r| {
            let a = r.select(&title).next()?;
            let url = ddg_target(a.value().attr("href")?, base)?;
            let text: String = a.text().collect();
            let snip: String = r
                .select(&snippet)
                .next()
                .map(|s| s.text().collect())
                .unwrap_or_default();
            make("duckduckgo", &text, &url, &snip)
        })
        .collect()
}

#[async_trait]
impl SearchBackend for DuckDuckGoBackend {
    fn name(&self) -> &'static str {
        "duckduckgo"
    }

    async fn search(&self, query: &SearchQuery) -> Result<Vec<SearchResult>, WebError> {
        let (q, limit) = query.validated()?;
        let mut url = self.endpoint.clone();
        url.query_pairs_mut().append_pair("q", &q);
        let page = self
            .fetcher
            .get(url.as_str(), &GetOptions::accepting(HTML_TYPES))
            .await?;
        let hits = parse_ddg(&page.text(), &page.url);
        if hits.is_empty() {
            return Err(WebError::Search(
                "duckduckgo returned no parsable results (possibly a bot challenge)".into(),
            ));
        }
        Ok(finalize(hits, limit))
    }
}

// ------------------------------------------------------------ fallback chain

/// Outcome of a [`MultiSearch`].
#[derive(Debug, Clone, serde::Serialize)]
pub struct SearchOutcome {
    /// Backend that answered.
    pub backend: String,
    /// Normalised results.
    pub results: Vec<SearchResult>,
    /// `backend: error` lines for backends tried before the one that answered.
    pub fallback_errors: Vec<String>,
}

/// Ordered fallback chain over several backends.
#[derive(Default, Clone)]
pub struct MultiSearch {
    backends: Vec<Arc<dyn SearchBackend>>,
}

impl MultiSearch {
    /// Chain in the given priority order.
    #[must_use]
    pub fn new(backends: Vec<Arc<dyn SearchBackend>>) -> Self {
        Self { backends }
    }

    /// Names of the configured backends in priority order.
    #[must_use]
    pub fn backend_names(&self) -> Vec<&'static str> {
        self.backends.iter().map(|b| b.name()).collect()
    }

    /// Search, optionally restricted to the backend called `only`.
    ///
    /// # Errors
    /// [`WebError::Search`] when no backend is configured, `only` is unknown, or
    /// every backend failed or returned nothing (all errors are listed).
    pub async fn search(
        &self,
        query: &SearchQuery,
        only: Option<&str>,
    ) -> Result<SearchOutcome, WebError> {
        let candidates: Vec<_> = self
            .backends
            .iter()
            .filter(|b| only.is_none_or(|o| o.eq_ignore_ascii_case(b.name())))
            .collect();
        if candidates.is_empty() {
            return Err(WebError::Search(match only {
                Some(o) => format!(
                    "no search backend named `{o}` (configured: {})",
                    self.backend_names().join(", ")
                ),
                None => "no search backend configured".into(),
            }));
        }
        let mut errors = Vec::new();
        for b in candidates {
            match b.search(query).await {
                Ok(results) if !results.is_empty() => {
                    return Ok(SearchOutcome {
                        backend: b.name().to_string(),
                        results,
                        fallback_errors: errors,
                    });
                }
                Ok(_) => errors.push(format!("{}: no results", b.name())),
                Err(e) => errors.push(format!("{}: {e}", b.name())),
            }
        }
        Err(WebError::Search(errors.join("; ")))
    }
}

// ------------------------------------------------------------------ config

/// Which backends to enable.
#[derive(Debug, Clone)]
pub struct SearchConfig {
    /// `SearXNG` instance root. Trusted: its host is exempt from the private
    /// address block (but never from link-local / metadata blocking).
    pub searxng_url: Option<String>,
    /// Secret holding the Brave Search API key.
    pub brave_key: Option<SecretRef>,
    /// Override of the Brave endpoint (tests, proxies).
    pub brave_endpoint: Option<String>,
    /// Enable the `DuckDuckGo` HTML fallback (default `true`).
    pub duckduckgo: bool,
    /// Override of the `DuckDuckGo` endpoint (tests).
    pub duckduckgo_endpoint: Option<String>,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            searxng_url: None,
            brave_key: None,
            brave_endpoint: None,
            duckduckgo: true,
            duckduckgo_endpoint: None,
        }
    }
}

impl SearchConfig {
    /// Build the backend chain: `SearXNG`, Brave, `DuckDuckGo`.
    ///
    /// `public` is the policy-guarded fetcher used for Brave and `DuckDuckGo`.
    /// `SearXNG` gets its own fetcher whose policy wraps the injected one and
    /// additionally trusts only the configured `host:port`.
    ///
    /// # Errors
    /// [`WebError::Config`] for bad URLs, or a Brave key without a secret store.
    pub fn build(
        &self,
        public: &HttpFetcher,
        http: &HttpConfig,
        secrets: Option<Arc<dyn SecretStore>>,
    ) -> Result<MultiSearch, WebError> {
        let mut chain: Vec<Arc<dyn SearchBackend>> = Vec::new();
        if let Some(raw) = &self.searxng_url {
            let mut base =
                Url::parse(raw).map_err(|e| WebError::Config(format!("searxng url: {e}")))?;
            if !base.path().ends_with('/') {
                let p = format!("{}/", base.path());
                base.set_path(&p);
            }
            let host = base
                .host_str()
                .ok_or_else(|| WebError::Config("searxng url has no host".into()))?
                .to_string();
            let port = base
                .port_or_known_default()
                .ok_or_else(|| WebError::Config("searxng url has no port".into()))?;
            let policy = TrustedHostPolicy::new(Arc::clone(public.policy()), &host, port);
            let fetcher = HttpFetcher::new(Arc::new(policy), http.clone())?;
            fetcher.check(base.as_str())?;
            chain.push(Arc::new(SearxngBackend::new(base, fetcher)));
        }
        if let Some(key) = &self.brave_key {
            let store = secrets.clone().ok_or_else(|| {
                WebError::Config("brave_key set but no secret store provided".into())
            })?;
            chain.push(Arc::new(BraveBackend::new(
                self.brave_endpoint.as_deref(),
                public.clone(),
                store,
                key.clone(),
            )?));
        }
        if self.duckduckgo {
            chain.push(Arc::new(DuckDuckGoBackend::new(
                self.duckduckgo_endpoint.as_deref(),
                public.clone(),
            )?));
        }
        Ok(MultiSearch::new(chain))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ddg_parsing_unwraps_redirects_and_skips_ads() {
        let html = r#"<html><body>
        <div class="result results_links web-result"><h2><a class="result__a"
          href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fa%3Fx%3D1&amp;rut=abc">Example <b>A</b></a></h2>
          <a class="result__snippet">Snippet <b>one</b></a></div>
        <div class="result result--ad"><a class="result__a" href="https://ads.example/">Ad</a></div>
        <div class="result"><a class="result__a" href="https://direct.example/b">Direct</a></div>
        <div class="result"><a class="result__a" href="javascript:alert(1)">Bad</a></div>
        </body></html>"#;
        let base = Url::parse("https://html.duckduckgo.com/html/?q=x").unwrap();
        let r = parse_ddg(html, &base);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].url, "https://example.com/a?x=1");
        assert_eq!(r[0].title, "Example A");
        assert_eq!(r[0].snippet, "Snippet one");
        assert_eq!(r[1].url, "https://direct.example/b");
    }

    #[test]
    fn finalize_dedupes_and_ranks() {
        let mk = |u: &str| make("t", "T", u, "").unwrap();
        let out = finalize(
            vec![
                mk("https://a.com/x"),
                mk("https://a.com/x/"),
                mk("https://a.com/x#f"),
                mk("https://b.com/"),
            ],
            10,
        );
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].rank, 2);
        assert!(make("t", "T", "ftp://a.com/", "").is_none());
    }

    #[test]
    fn query_validation() {
        assert!(SearchQuery::new("   ").validated().is_err());
        assert!(SearchQuery::new("x".repeat(600)).validated().is_err());
        let q = SearchQuery {
            limit: 999,
            ..SearchQuery::new("a   b")
        };
        assert_eq!(q.validated().unwrap(), ("a b".to_string(), MAX_RESULTS));
    }
}
