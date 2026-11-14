//! `web_search` with pluggable backends, resilient keyless fallback, per-backend
//! cooldowns, and normalised results.
//!
//! Configured backends may include a self-hosted `SearXNG` instance (JSON API),
//! Brave Search API, Tavily Search API, and a keyless fallback chain
//! (`DuckDuckGo` HTML -> `DuckDuckGo` Lite -> `Bing` HTML). [`MultiSearch`]
//! tries enabled backends in configured priority order and skips backends
//! currently cooling down after a bot challenge or transient failure.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use parking_lot::Mutex;
use scraper::{Html, Selector};
use secret_store::{SecretRef, SecretStore};
use serde::{Deserialize, Serialize};
use tokio::time::Instant;
use url::Url;

use crate::error::WebError;
use crate::http::{GetOptions, HTML_TYPES, HttpConfig, HttpFetcher, JSON_TYPES};
use crate::policy::TrustedHostPolicy;

/// Hard upper bound for results per query.
pub const MAX_RESULTS: usize = 25;
const MAX_QUERY_CHARS: usize = 500;

/// Per-backend timeout cap for search requests so a stalled backend falls
/// back quickly rather than consuming the whole chain budget.
pub const DEFAULT_SEARCH_TIMEOUT: Duration = Duration::from_secs(8);
/// Default cooldown applied after a backend returns a bot challenge or rate limit.
pub const DEFAULT_CHALLENGE_COOLDOWN: Duration = Duration::from_secs(60);
/// Default cooldown applied after a transient upstream / timeout error.
pub const DEFAULT_ERROR_COOLDOWN: Duration = Duration::from_secs(15);

/// All known backend identifiers in default priority order.
pub const KNOWN_BACKENDS: &[&str] = &[
    "searxng",
    "brave",
    "tavily",
    "duckduckgo",
    "duckduckgo_lite",
    "bing",
];

const BROWSER_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) \
    AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

/// One normalised search hit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SearchResult {
    /// 1-based position in the merged list.
    pub rank: usize,
    /// Result title (plain text).
    pub title: String,
    /// Absolute http(s) URL.
    pub url: String,
    /// Snippet (plain text, may be empty).
    pub snippet: String,
    /// Backend that produced the hit (e.g. `searxng`, `brave`, `tavily`,
    /// `duckduckgo`, `duckduckgo_lite`, `bing`).
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
    let title = plain(title);
    if title.is_empty() {
        return None;
    }
    Some(SearchResult {
        rank: 0,
        title,
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

fn search_timeout(fetcher: &HttpFetcher) -> Duration {
    fetcher.config().timeout.min(DEFAULT_SEARCH_TIMEOUT)
}

fn browser_search_options(language: Option<&str>, timeout: Duration) -> GetOptions {
    let mut opts = GetOptions::accepting(HTML_TYPES);
    opts.timeout = Some(timeout);
    let accept_lang = language
        .map(str::trim)
        .filter(|l| !l.is_empty() && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
        .map_or_else(|| "en-US,en;q=0.9".to_string(), |l| format!("{l},en;q=0.8"));
    opts.headers
        .push(("User-Agent".into(), BROWSER_USER_AGENT.into()));
    opts.headers.push((
        "Accept".into(),
        "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8".into(),
    ));
    opts.headers.push(("Accept-Language".into(), accept_lang));
    opts.headers
        .push(("Upgrade-Insecure-Requests".into(), "1".into()));
    opts
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
        let mut opts = GetOptions::accepting(JSON_TYPES);
        opts.timeout = Some(search_timeout(&self.fetcher));
        let page = self.fetcher.get(url.as_str(), &opts).await?;
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
        opts.timeout = Some(search_timeout(&self.fetcher));
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

// ----------------------------------------------------------------- Tavily

/// Tavily Search API. The API key is resolved from the secret store on every
/// call, so rotating the secret needs no restart.
pub struct TavilyBackend {
    endpoint: Url,
    fetcher: HttpFetcher,
    secrets: Arc<dyn SecretStore>,
    key: SecretRef,
}

/// Default Tavily web-search endpoint.
pub const TAVILY_ENDPOINT: &str = "https://api.tavily.com/search";

impl TavilyBackend {
    /// Build a backend. `endpoint` defaults to [`TAVILY_ENDPOINT`].
    ///
    /// # Errors
    /// [`WebError::Config`] when the endpoint is not a URL.
    pub fn new(
        endpoint: Option<&str>,
        fetcher: HttpFetcher,
        secrets: Arc<dyn SecretStore>,
        key: SecretRef,
    ) -> Result<Self, WebError> {
        let endpoint = Url::parse(endpoint.unwrap_or(TAVILY_ENDPOINT))
            .map_err(|e| WebError::Config(format!("tavily endpoint: {e}")))?;
        Ok(Self {
            endpoint,
            fetcher,
            secrets,
            key,
        })
    }
}

#[derive(Serialize)]
struct TavilyRequest<'a> {
    query: &'a str,
    max_results: usize,
}

#[derive(Deserialize)]
struct TavilyResponse {
    #[serde(default)]
    results: Vec<TavilyHit>,
}

#[derive(Deserialize)]
struct TavilyHit {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    content: String,
}

#[async_trait]
impl SearchBackend for TavilyBackend {
    fn name(&self) -> &'static str {
        "tavily"
    }

    async fn search(&self, query: &SearchQuery) -> Result<Vec<SearchResult>, WebError> {
        let (q, limit) = query.validated()?;
        let token = secret_store::resolve(self.secrets.as_ref(), &self.key).map_err(|_| {
            WebError::Config(format!(
                "secret `{}` for tavily is not available",
                self.key.name()
            ))
        })?;
        let payload = serde_json::to_vec(&TavilyRequest {
            query: &q,
            max_results: limit,
        })
        .map_err(|e| WebError::Search(format!("tavily: {e}")))?;
        let mut opts = GetOptions::accepting(JSON_TYPES);
        opts.timeout = Some(search_timeout(&self.fetcher));
        opts.headers
            .push(("Authorization".into(), format!("Bearer {}", token.expose())));
        opts.headers
            .push(("Accept".into(), "application/json".into()));
        let page = self
            .fetcher
            .post_json(self.endpoint.as_str(), &payload, &opts)
            .await
            .map_err(|e| match e {
                WebError::Status(401 | 403) => {
                    WebError::Search("tavily rejected the API key".into())
                }
                WebError::Status(429) => WebError::Search("tavily rate limit reached".into()),
                other => other,
            })?;
        if page.truncated {
            return Err(WebError::Search("tavily response too large".into()));
        }
        let parsed: TavilyResponse = serde_json::from_slice(&page.body)
            .map_err(|e| WebError::Search(format!("tavily: bad JSON ({e})")))?;
        let hits = parsed
            .results
            .iter()
            .filter_map(|h| make("tavily", &h.title, &h.url, &h.content))
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

fn is_ddg_challenge(status: u16, html: &str) -> bool {
    if matches!(status, 202 | 403 | 429) {
        return true;
    }
    let lower = html.to_ascii_lowercase();
    lower.contains("anomaly-modal")
        || lower.contains("challenge-form")
        || lower.contains("anomaly.js")
        || lower.contains("unfortunately, bots use duckduckgo too")
}

fn is_ddg_no_results(html: &str) -> bool {
    html.contains("class=\"no-results\"")
        || html.contains("class='no-results'")
        || html.contains("No results.")
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
        let opts = browser_search_options(query.language.as_deref(), search_timeout(&self.fetcher));
        let page = self.fetcher.get(url.as_str(), &opts).await?;
        let text = page.text();
        let hits = parse_ddg(&text, &page.url);
        if hits.is_empty() {
            if is_ddg_challenge(page.status, &text) {
                return Err(WebError::Search(format!(
                    "duckduckgo returned a bot challenge (HTTP {})",
                    page.status
                )));
            }
            if is_ddg_no_results(&text) {
                return Ok(Vec::new());
            }
            return Err(WebError::Search(
                "duckduckgo returned no parsable results (possibly a bot challenge)".into(),
            ));
        }
        Ok(finalize(hits, limit))
    }
}

// -------------------------------------------------------- DuckDuckGo Lite

/// Key-less `DuckDuckGo` Lite HTML endpoint (`lite.duckduckgo.com`).
pub struct DuckDuckGoLiteBackend {
    endpoint: Url,
    fetcher: HttpFetcher,
}

/// Default `DuckDuckGo` Lite endpoint.
pub const DUCKDUCKGO_LITE_ENDPOINT: &str = "https://lite.duckduckgo.com/lite/";

impl DuckDuckGoLiteBackend {
    /// `endpoint` defaults to [`DUCKDUCKGO_LITE_ENDPOINT`].
    ///
    /// # Errors
    /// [`WebError::Config`] when the endpoint is not a URL.
    pub fn new(endpoint: Option<&str>, fetcher: HttpFetcher) -> Result<Self, WebError> {
        let endpoint = Url::parse(endpoint.unwrap_or(DUCKDUCKGO_LITE_ENDPOINT))
            .map_err(|e| WebError::Config(format!("duckduckgo_lite endpoint: {e}")))?;
        Ok(Self { endpoint, fetcher })
    }
}

/// Parse `DuckDuckGo` Lite table rows (`a.result-link` paired with the
/// following `td.result-snippet` row).
fn parse_ddg_lite(html: &str, base: &Url) -> Vec<SearchResult> {
    let doc = Html::parse_document(html);
    let tr_sel = Selector::parse("tr").expect("selector");
    let link_sel = Selector::parse("a.result-link").expect("selector");
    let snippet_sel = Selector::parse(".result-snippet").expect("selector");
    let ad_badge_sel = Selector::parse(".badge--ad").expect("selector");

    let mut out = Vec::new();
    let mut pending: Option<(String, String)> = None;

    for tr in doc.select(&tr_sel) {
        let is_sponsored = tr.value().classes().any(|c| c == "result-sponsored")
            || tr.select(&ad_badge_sel).next().is_some();
        if is_sponsored {
            if let Some((t, u)) = pending.take() {
                if let Some(hit) = make("duckduckgo_lite", &t, &u, "") {
                    out.push(hit);
                }
            }
            continue;
        }
        if let Some(a) = tr.select(&link_sel).next() {
            if let Some((t, u)) = pending.take() {
                if let Some(hit) = make("duckduckgo_lite", &t, &u, "") {
                    out.push(hit);
                }
            }
            if let Some(href) = a.value().attr("href")
                && let Some(url) = ddg_target(href, base)
            {
                let title: String = a.text().collect();
                pending = Some((title, url));
            }
        } else if let Some(snip_el) = tr.select(&snippet_sel).next()
            && let Some((t, u)) = pending.take()
        {
            let snip: String = snip_el.text().collect();
            if let Some(hit) = make("duckduckgo_lite", &t, &u, &snip) {
                out.push(hit);
            }
        }
    }
    if let Some((t, u)) = pending.take() {
        if let Some(hit) = make("duckduckgo_lite", &t, &u, "") {
            out.push(hit);
        }
    }
    out
}

#[async_trait]
impl SearchBackend for DuckDuckGoLiteBackend {
    fn name(&self) -> &'static str {
        "duckduckgo_lite"
    }

    async fn search(&self, query: &SearchQuery) -> Result<Vec<SearchResult>, WebError> {
        let (q, limit) = query.validated()?;
        let mut url = self.endpoint.clone();
        url.query_pairs_mut().append_pair("q", &q);
        let opts = browser_search_options(query.language.as_deref(), search_timeout(&self.fetcher));
        let page = self.fetcher.get(url.as_str(), &opts).await?;
        let text = page.text();
        let hits = parse_ddg_lite(&text, &page.url);
        if hits.is_empty() {
            if is_ddg_challenge(page.status, &text) {
                return Err(WebError::Search(format!(
                    "duckduckgo_lite returned a bot challenge (HTTP {})",
                    page.status
                )));
            }
            if is_ddg_no_results(&text) {
                return Ok(Vec::new());
            }
            return Err(WebError::Search(
                "duckduckgo_lite returned no parsable results (possibly a bot challenge)".into(),
            ));
        }
        Ok(finalize(hits, limit))
    }
}

// ------------------------------------------------------------------- Bing

/// Key-less Bing HTML search endpoint.
pub struct BingBackend {
    endpoint: Url,
    fetcher: HttpFetcher,
}

/// Default Bing HTML search endpoint.
pub const BING_ENDPOINT: &str = "https://www.bing.com/search";

impl BingBackend {
    /// `endpoint` defaults to [`BING_ENDPOINT`].
    ///
    /// # Errors
    /// [`WebError::Config`] when the endpoint is not a URL.
    pub fn new(endpoint: Option<&str>, fetcher: HttpFetcher) -> Result<Self, WebError> {
        let endpoint = Url::parse(endpoint.unwrap_or(BING_ENDPOINT))
            .map_err(|e| WebError::Config(format!("bing endpoint: {e}")))?;
        Ok(Self { endpoint, fetcher })
    }
}

/// Unwrap Bing click-tracking links (`/ck/a?...&u=a1<base64url>`).
fn bing_target(href: &str, base: &Url) -> Option<String> {
    let abs = base.join(href).ok()?;
    let is_bing_ck = abs
        .host_str()
        .is_some_and(|h| h == "bing.com" || h.ends_with(".bing.com"))
        && abs.path().starts_with("/ck/a");
    if is_bing_ck {
        if let Some((_, raw)) = abs.query_pairs().find(|(k, _)| k == "u")
            && let Some(b64) = raw.strip_prefix("a1")
        {
            if let Ok(bytes) = URL_SAFE_NO_PAD.decode(b64.trim_end_matches('='))
                && let Ok(decoded) = String::from_utf8(bytes)
                && !decoded.trim().is_empty()
            {
                return Some(decoded);
            }
        }
        return None;
    }
    Some(abs.to_string())
}

fn is_bing_challenge(status: u16, html: &str) -> bool {
    if matches!(status, 403 | 429) {
        return true;
    }
    let lower = html.to_ascii_lowercase();
    lower.contains("g-recaptcha")
        || lower.contains("hcaptcha")
        || lower.contains("cf-turnstile")
        || lower.contains("challenge-platform")
        || lower.contains("verify you are human")
        || lower.contains("one more step")
}

/// Parse Bing `<li class="b_algo">` organic search results.
fn parse_bing(html: &str, base: &Url) -> Vec<SearchResult> {
    let doc = Html::parse_document(html);
    let algo = Selector::parse("li.b_algo").expect("selector");
    let link = Selector::parse("h2 a").expect("selector");
    let snippet =
        Selector::parse(".b_caption p, .b_algoSlug, p.b_lineclamp2, p.b_lineclamp3, .b_snippet")
            .expect("selector");
    doc.select(&algo)
        .filter_map(|li| {
            let a = li.select(&link).next()?;
            let url = bing_target(a.value().attr("href")?, base)?;
            let title: String = a.text().collect();
            let snip: String = li
                .select(&snippet)
                .next()
                .map(|s| s.text().collect())
                .unwrap_or_default();
            make("bing", &title, &url, &snip)
        })
        .collect()
}

#[async_trait]
impl SearchBackend for BingBackend {
    fn name(&self) -> &'static str {
        "bing"
    }

    async fn search(&self, query: &SearchQuery) -> Result<Vec<SearchResult>, WebError> {
        let (q, limit) = query.validated()?;
        let mut url = self.endpoint.clone();
        {
            let mut p = url.query_pairs_mut();
            p.append_pair("q", &q)
                .append_pair("count", &limit.to_string());
            if let Some(l) = &query.language {
                p.append_pair("setlang", l);
            }
        }
        let opts = browser_search_options(query.language.as_deref(), search_timeout(&self.fetcher));
        let page = self.fetcher.get(url.as_str(), &opts).await?;
        let text = page.text();
        let hits = parse_bing(&text, &page.url);
        if hits.is_empty() {
            if is_bing_challenge(page.status, &text) {
                return Err(WebError::Search(format!(
                    "bing returned a bot challenge (HTTP {})",
                    page.status
                )));
            }
            if text.contains("b_no") || text.contains("There are no results for") {
                return Ok(Vec::new());
            }
            return Err(WebError::Search(
                "bing returned no parsable results (possibly a bot challenge)".into(),
            ));
        }
        Ok(finalize(hits, limit))
    }
}

// ------------------------------------------------------------ fallback chain

/// Outcome of a [`MultiSearch`].
#[derive(Debug, Clone, Serialize)]
pub struct SearchOutcome {
    /// Backend that answered.
    pub backend: String,
    /// Normalised results.
    pub results: Vec<SearchResult>,
    /// `backend: error` lines for backends tried before the one that answered.
    pub fallback_errors: Vec<String>,
}

#[derive(Clone, Debug)]
struct CooldownEntry {
    until: Instant,
    reason: &'static str,
}

/// Ordered fallback chain over several backends with short per-backend cooldowns.
#[derive(Default, Clone)]
pub struct MultiSearch {
    backends: Vec<Arc<dyn SearchBackend>>,
    cooldown: Duration,
    cooldowns: Arc<Mutex<HashMap<String, CooldownEntry>>>,
}

impl MultiSearch {
    /// Chain in the given priority order, using [`DEFAULT_CHALLENGE_COOLDOWN`].
    #[must_use]
    pub fn new(backends: Vec<Arc<dyn SearchBackend>>) -> Self {
        Self::with_cooldown(backends, DEFAULT_CHALLENGE_COOLDOWN)
    }

    /// Chain with a custom challenge cooldown duration (`Duration::ZERO` disables cooldowns).
    #[must_use]
    pub fn with_cooldown(backends: Vec<Arc<dyn SearchBackend>>, cooldown: Duration) -> Self {
        Self {
            backends,
            cooldown,
            cooldowns: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Clear all active backend cooldowns.
    pub fn clear_cooldowns(&self) {
        self.cooldowns.lock().clear();
    }

    /// Names of the configured backends in priority order.
    #[must_use]
    pub fn backend_names(&self) -> Vec<&'static str> {
        self.backends.iter().map(|b| b.name()).collect()
    }

    fn classify_cooldown(&self, err: &WebError) -> Option<(Duration, &'static str)> {
        if self.cooldown.is_zero() {
            return None;
        }
        match err {
            WebError::Status(202 | 401 | 403 | 429) => {
                Some((self.cooldown, "bot challenge or rate limit"))
            }
            WebError::Search(m) => {
                let lower = m.to_ascii_lowercase();
                if lower.contains("bot challenge")
                    || lower.contains("captcha")
                    || lower.contains("rate limit")
                    || lower.contains("rejected the api key")
                    || lower.contains("no parsable results")
                {
                    Some((self.cooldown, "bot challenge or rate limit"))
                } else {
                    Some((self.cooldown.min(DEFAULT_ERROR_COOLDOWN), "search error"))
                }
            }
            WebError::Timeout | WebError::Http(_) | WebError::Status(500..=599) => {
                Some((self.cooldown.min(DEFAULT_ERROR_COOLDOWN), "upstream error"))
            }
            WebError::Config(_)
            | WebError::Policy(_)
            | WebError::InvalidUrl(_)
            | WebError::Status(_)
            | WebError::ContentType(_)
            | WebError::TooManyRedirects(_)
            | WebError::Parse(_) => None,
        }
    }

    /// Search, optionally restricted to the backend called `only`.
    ///
    /// # Errors
    /// [`WebError::Search`] when no backend is configured, `only` is unknown, or
    /// every backend failed or returned nothing (all errors are listed with a
    /// concise instruction not to retry the same query or invent backends).
    pub async fn search(
        &self,
        query: &SearchQuery,
        only: Option<&str>,
    ) -> Result<SearchOutcome, WebError> {
        let configured = self.backend_names();
        let candidates: Vec<_> = self
            .backends
            .iter()
            .filter(|b| only.is_none_or(|o| o.eq_ignore_ascii_case(b.name())))
            .collect();
        if candidates.is_empty() {
            return Err(WebError::Search(match only {
                Some(o) if configured.is_empty() => {
                    format!("no search backend named `{o}` (no search backends are configured)")
                }
                Some(o) => format!(
                    "no search backend named `{o}` (configured: {}); omit `backend` to use the automatic fallback chain",
                    configured.join(", ")
                ),
                None => "no search backend configured; enable a backend or configure Brave/Tavily/SearXNG in Web Search settings".into(),
            }));
        }
        let mut errors = Vec::new();
        let now = Instant::now();
        for b in candidates {
            let name = b.name();
            if only.is_none() {
                let active_cd = {
                    let mut guard = self.cooldowns.lock();
                    match guard.get(name) {
                        Some(entry) if entry.until > now => Some((
                            entry.until.saturating_duration_since(now).as_secs().max(1),
                            entry.reason,
                        )),
                        Some(_) => {
                            guard.remove(name);
                            None
                        }
                        None => None,
                    }
                };
                if let Some((secs, reason)) = active_cd {
                    errors.push(format!(
                        "{name}: skipped (cooling down for {secs}s after {reason})"
                    ));
                    continue;
                }
            }
            match b.search(query).await {
                Ok(results) if !results.is_empty() => {
                    self.cooldowns.lock().remove(name);
                    return Ok(SearchOutcome {
                        backend: name.to_string(),
                        results,
                        fallback_errors: errors,
                    });
                }
                Ok(_) => errors.push(format!("{name}: no results")),
                Err(e) => {
                    if let Some((dur, reason)) = self.classify_cooldown(&e) {
                        self.cooldowns.lock().insert(
                            name.to_string(),
                            CooldownEntry {
                                until: Instant::now() + dur,
                                reason,
                            },
                        );
                    }
                    errors.push(format!("{name}: {e}"));
                }
            }
        }
        Err(WebError::Search(format!(
            "all configured search backends ({}) failed ({}); do not retry the exact same query or invent another backend name—rephrase keywords, open a known URL with web_extract, or configure Brave/Tavily/SearXNG in Web Search settings",
            configured.join(", "),
            errors.join("; ")
        )))
    }
}

// ------------------------------------------------------------------ config

/// Which backends to enable and their priority order.
#[derive(Debug, Clone)]
pub struct SearchConfig {
    /// `SearXNG` instance root. Trusted: its host is exempt from the private
    /// address block (but never from link-local / metadata blocking).
    pub searxng_url: Option<String>,
    /// Secret holding the Brave Search API key.
    pub brave_key: Option<SecretRef>,
    /// Override of the Brave endpoint (tests, proxies).
    pub brave_endpoint: Option<String>,
    /// Secret holding the Tavily Search API key.
    pub tavily_key: Option<SecretRef>,
    /// Override of the Tavily endpoint (tests, proxies).
    pub tavily_endpoint: Option<String>,
    /// Enable the `DuckDuckGo` HTML fallback (default `true`).
    pub duckduckgo: bool,
    /// Override of the `DuckDuckGo` endpoint (tests).
    pub duckduckgo_endpoint: Option<String>,
    /// Enable the `DuckDuckGo` Lite fallback (default `true`).
    pub duckduckgo_lite: bool,
    /// Override of the `DuckDuckGo` Lite endpoint (tests).
    pub duckduckgo_lite_endpoint: Option<String>,
    /// Enable the `Bing` HTML fallback (default `true`).
    pub bing: bool,
    /// Override of the `Bing` endpoint (tests).
    pub bing_endpoint: Option<String>,
    /// Optional explicit priority order of backend names; empty uses the
    /// default order (`searxng`, `brave`, `tavily`, `duckduckgo`,
    /// `duckduckgo_lite`, `bing`). Any enabled backend omitted from `order`
    /// is appended in default order.
    pub order: Vec<String>,
    /// Cooldown duration per backend after a bot challenge / rate limit.
    pub cooldown: Duration,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            searxng_url: None,
            brave_key: None,
            brave_endpoint: None,
            tavily_key: None,
            tavily_endpoint: None,
            duckduckgo: true,
            duckduckgo_endpoint: None,
            duckduckgo_lite: true,
            duckduckgo_lite_endpoint: None,
            bing: true,
            bing_endpoint: None,
            order: Vec::new(),
            cooldown: DEFAULT_CHALLENGE_COOLDOWN,
        }
    }
}

impl SearchConfig {
    /// Build the ordered backend chain.
    ///
    /// `public` is the policy-guarded fetcher used for public backends.
    /// `SearXNG` gets its own fetcher whose policy wraps the injected one and
    /// additionally trusts only the configured `host:port`.
    ///
    /// # Errors
    /// [`WebError::Config`] for bad URLs, or an API key without a secret store.
    pub fn build(
        &self,
        public: &HttpFetcher,
        http: &HttpConfig,
        secrets: Option<Arc<dyn SecretStore>>,
    ) -> Result<MultiSearch, WebError> {
        let mut available: HashMap<&'static str, Arc<dyn SearchBackend>> = HashMap::new();
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
            available.insert("searxng", Arc::new(SearxngBackend::new(base, fetcher)));
        }
        if let Some(key) = &self.brave_key {
            let store = secrets.clone().ok_or_else(|| {
                WebError::Config("brave_key set but no secret store provided".into())
            })?;
            available.insert(
                "brave",
                Arc::new(BraveBackend::new(
                    self.brave_endpoint.as_deref(),
                    public.clone(),
                    store,
                    key.clone(),
                )?),
            );
        }
        if let Some(key) = &self.tavily_key {
            let store = secrets.clone().ok_or_else(|| {
                WebError::Config("tavily_key set but no secret store provided".into())
            })?;
            available.insert(
                "tavily",
                Arc::new(TavilyBackend::new(
                    self.tavily_endpoint.as_deref(),
                    public.clone(),
                    store,
                    key.clone(),
                )?),
            );
        }
        if self.duckduckgo {
            available.insert(
                "duckduckgo",
                Arc::new(DuckDuckGoBackend::new(
                    self.duckduckgo_endpoint.as_deref(),
                    public.clone(),
                )?),
            );
        }
        if self.duckduckgo_lite {
            available.insert(
                "duckduckgo_lite",
                Arc::new(DuckDuckGoLiteBackend::new(
                    self.duckduckgo_lite_endpoint.as_deref(),
                    public.clone(),
                )?),
            );
        }
        if self.bing {
            available.insert(
                "bing",
                Arc::new(BingBackend::new(
                    self.bing_endpoint.as_deref(),
                    public.clone(),
                )?),
            );
        }

        let mut chain: Vec<Arc<dyn SearchBackend>> = Vec::new();
        for name in &self.order {
            if let Some(b) = available.remove(name.as_str()) {
                chain.push(b);
            }
        }
        for name in KNOWN_BACKENDS {
            if let Some(b) = available.remove(name) {
                chain.push(b);
            }
        }
        Ok(MultiSearch::with_cooldown(chain, self.cooldown))
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
    fn ddg_challenge_page_is_detected() {
        let challenge_html = r#"<!DOCTYPE html><html><head><title>DuckDuckGo</title></head><body>
        <form id="challenge-form" action="//duckduckgo.com/anomaly.js?sv=html"></form>
        <div class="anomaly-modal__modal" data-testid="anomaly-modal">
          <div class="anomaly-modal__title">Unfortunately, bots use DuckDuckGo too.</div>
        </div></body></html>"#;
        let base = Url::parse("https://html.duckduckgo.com/html/?q=rust").unwrap();
        assert_eq!(parse_ddg(challenge_html, &base), Vec::new());
        assert!(is_ddg_challenge(202, challenge_html));
        assert!(is_ddg_challenge(200, challenge_html));
        assert!(!is_ddg_challenge(
            200,
            "<html><body><div class=\"no-results\">No results.</div></body></html>"
        ));
    }

    #[test]
    fn ddg_lite_parsing_pairs_rows_and_skips_sponsored() {
        let html = r#"<html><body><table>
        <tr class="result-sponsored"><td><span class="badge--ad">Ad</span></td>
          <td><a class="result-link" href="https://ads.example/">Sponsored</a></td></tr>
        <tr class="result-sponsored"><td></td><td class="result-snippet">Buy now</td></tr>
        <tr><td>1.</td><td><a rel="nofollow" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Frust-lang.org%2F&amp;rut=abc" class='result-link'>Rust <b>Language</b></a></td></tr>
        <tr><td></td><td class='result-snippet'>Fast and <b>reliable</b>.</td></tr>
        <tr><td></td><td><span class='link-text'>rust-lang.org</span></td></tr>
        <tr><td>2.</td><td><a href="https://docs.rs/" class="result-link">Docs.rs</a></td></tr>
        </table></body></html>"#;
        let base = Url::parse("https://lite.duckduckgo.com/lite/?q=rust").unwrap();
        let r = parse_ddg_lite(html, &base);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].title, "Rust Language");
        assert_eq!(r[0].url, "https://rust-lang.org/");
        assert_eq!(r[0].snippet, "Fast and reliable.");
        assert_eq!(r[0].source, "duckduckgo_lite");
        assert_eq!(r[1].title, "Docs.rs");
        assert_eq!(r[1].url, "https://docs.rs/");
        assert_eq!(r[1].snippet, "");
    }

    #[test]
    fn bing_parsing_unwraps_tracking_urls_and_skips_challenges() {
        let encoded = URL_SAFE_NO_PAD.encode("https://www.rust-lang.org/");
        let html = format!(
            r#"<html><body><ol id="b_results">
            <li class="b_algo"><h2><a href="https://www.bing.com/ck/a?!&amp;&amp;p=abc&amp;u=a1{encoded}&amp;ntb=1"><strong>Rust</strong> Programming</a></h2>
            <div class="b_caption"><p class="b_lineclamp2">A systems programming language.</p></div></li>
            <li class="b_algo"><h2><a href="https://doc.rust-lang.org/book/">The Rust Book</a></h2>
            <div class="b_caption"><p>Learn Rust.</p></div></li>
            </ol></body></html>"#
        );
        let base = Url::parse("https://www.bing.com/search?q=rust").unwrap();
        let r = parse_bing(&html, &base);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].title, "Rust Programming");
        assert_eq!(r[0].url, "https://www.rust-lang.org/");
        assert_eq!(r[0].snippet, "A systems programming language.");
        assert_eq!(r[0].source, "bing");
        assert_eq!(r[1].url, "https://doc.rust-lang.org/book/");
        assert!(is_bing_challenge(429, "<html></html>"));
        assert!(is_bing_challenge(
            200,
            "<html><div class=\"cf-turnstile\"></div></html>"
        ));
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
