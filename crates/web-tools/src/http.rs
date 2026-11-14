//! Guarded HTTP client: timeouts, size caps, manual redirect handling with a
//! policy re-check on every hop, content-type allow-list, and a DNS resolver
//! that only returns addresses the [`UrlPolicy`] accepts.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue, LOCATION};
use url::Url;

use crate::error::WebError;
use crate::policy::{PolicyViolation, UrlPolicy};

/// Limits applied to every request.
#[derive(Debug, Clone)]
pub struct HttpConfig {
    /// Total request timeout (connect + headers + body).
    pub timeout: Duration,
    /// Connect timeout.
    pub connect_timeout: Duration,
    /// Default cap on the response body, in bytes.
    pub max_bytes: usize,
    /// Maximum redirects followed.
    pub max_redirects: usize,
    /// `User-Agent` header.
    pub user_agent: String,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(20),
            connect_timeout: Duration::from_secs(8),
            max_bytes: 5 * 1024 * 1024,
            max_redirects: 5,
            user_agent: format!("Praxi-Bot/{} (web-tools)", env!("CARGO_PKG_VERSION")),
        }
    }
}

/// Content types accepted for HTML pages.
pub const HTML_TYPES: &[&str] = &["text/html", "application/xhtml+xml", "text/plain"];
/// Content types accepted for feeds and sitemaps.
pub const XML_TYPES: &[&str] = &[
    "text/xml",
    "application/xml",
    "application/rss+xml",
    "application/atom+xml",
    "application/rdf+xml",
    "text/plain",
    "text/html",
];
/// Content types accepted for JSON APIs.
pub const JSON_TYPES: &[&str] = &["application/json", "text/json", "application/ld+json"];

/// Per-request options.
#[derive(Debug, Clone, Default)]
pub struct GetOptions {
    /// Extra request headers.
    pub headers: Vec<(String, String)>,
    /// Allowed response media types (`type/subtype`, also matches `+suffix`
    /// forms such as `application/foo+json` for `application/json`).
    /// Empty: no check. A missing `Content-Type` header is always accepted.
    pub accept_types: Vec<String>,
    /// Override of [`HttpConfig::max_bytes`].
    pub max_bytes: Option<usize>,
    /// Optional per-request timeout override (capped by `HttpConfig::timeout` if shorter, or overrides it).
    pub timeout: Option<Duration>,
}

impl GetOptions {
    /// Options accepting only the given media types.
    #[must_use]
    pub fn accepting(types: &[&str]) -> Self {
        Self {
            accept_types: types.iter().map(|s| (*s).to_string()).collect(),
            ..Self::default()
        }
    }
}

/// A fetched response.
#[derive(Debug, Clone)]
pub struct Fetched {
    /// Final URL after redirects.
    pub url: Url,
    /// HTTP status of the final response.
    pub status: u16,
    /// Media type without parameters, lowercase.
    pub media_type: Option<String>,
    /// Declared charset, lowercase.
    pub charset: Option<String>,
    /// Body bytes, at most the byte cap.
    pub body: Vec<u8>,
    /// `true` when the body was cut at the cap.
    pub truncated: bool,
    /// Number of redirects followed.
    pub redirects: usize,
}

impl Fetched {
    /// Decode the body to text using the declared charset (UTF-8 default,
    /// ISO-8859-1 / windows-1252 supported, anything else lossy UTF-8).
    #[must_use]
    pub fn text(&self) -> String {
        decode_text(&self.body, self.charset.as_deref())
    }
}

/// Decode bytes with a (possibly absent) charset label.
#[must_use]
pub fn decode_text(bytes: &[u8], charset: Option<&str>) -> String {
    match charset {
        Some("iso-8859-1" | "latin1" | "windows-1252" | "us-ascii") => {
            bytes.iter().map(|&b| char::from(b)).collect()
        }
        _ => String::from_utf8_lossy(bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes))
            .into_owned(),
    }
}

struct GuardedResolver {
    policy: Arc<dyn UrlPolicy>,
}

impl Resolve for GuardedResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let policy = Arc::clone(&self.policy);
        Box::pin(async move {
            let host = name.as_str().to_string();
            let all: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?
                .collect();
            let mut allowed = Vec::new();
            let mut first_err = None;
            for addr in all {
                match policy.check_ip(&host, addr.ip()) {
                    Ok(()) => allowed.push(addr),
                    Err(e) => first_err = first_err.or(Some(e)),
                }
            }
            if allowed.is_empty() {
                let e = first_err.unwrap_or_else(|| PolicyViolation::new("host did not resolve"));
                return Err(Box::new(e) as Box<dyn std::error::Error + Send + Sync>);
            }
            Ok(Box::new(allowed.into_iter()) as Addrs)
        })
    }
}

/// HTTP GET client that enforces a [`UrlPolicy`] and the configured limits.
#[derive(Clone)]
pub struct HttpFetcher {
    client: reqwest::Client,
    policy: Arc<dyn UrlPolicy>,
    cfg: HttpConfig,
}

impl std::fmt::Debug for HttpFetcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpFetcher")
            .field("cfg", &self.cfg)
            .finish_non_exhaustive()
    }
}

impl HttpFetcher {
    /// Build a fetcher. System proxies are ignored on purpose: a proxy would
    /// resolve names itself and bypass the address checks.
    ///
    /// # Errors
    /// [`WebError::Config`] when the TLS client cannot be built.
    pub fn new(policy: Arc<dyn UrlPolicy>, cfg: HttpConfig) -> Result<Self, WebError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(cfg.timeout)
            .connect_timeout(cfg.connect_timeout)
            .user_agent(cfg.user_agent.clone())
            .no_proxy()
            .dns_resolver(Arc::new(GuardedResolver {
                policy: Arc::clone(&policy),
            }))
            .build()
            .map_err(|e| WebError::Config(format!("http client: {e}")))?;
        Ok(Self {
            client,
            policy,
            cfg,
        })
    }

    /// The policy in force.
    #[must_use]
    pub fn policy(&self) -> &Arc<dyn UrlPolicy> {
        &self.policy
    }

    /// The limits in force.
    #[must_use]
    pub fn config(&self) -> &HttpConfig {
        &self.cfg
    }

    /// Parse and policy-check a URL string without fetching it.
    ///
    /// # Errors
    /// [`WebError::InvalidUrl`] or [`WebError::Policy`].
    pub fn check(&self, url: &str) -> Result<Url, WebError> {
        let url =
            Url::parse(url.trim()).map_err(|e| WebError::InvalidUrl(format!("{url}: {e}")))?;
        self.policy.check_url(&url)?;
        Ok(url)
    }

    /// GET `url`, following up to `max_redirects` redirects with the policy
    /// re-checked on every hop.
    ///
    /// # Errors
    /// Policy violations, transport errors, HTTP status >= 400, a refused
    /// content type, or too many redirects.
    pub async fn get(&self, url: &str, opts: &GetOptions) -> Result<Fetched, WebError> {
        let mut current = self.check(url)?;
        let headers = build_headers(&opts.headers)?;
        let cap = opts.max_bytes.unwrap_or(self.cfg.max_bytes);
        let mut hops = 0usize;
        loop {
            // Never forward caller headers (API keys) to a different origin.
            let send_headers = if hops == 0 || same_origin(&current, url) {
                headers.clone()
            } else {
                HeaderMap::new()
            };
            let mut req = self.client.get(current.clone()).headers(send_headers);
            if let Some(t) = opts.timeout {
                req = req.timeout(t);
            }
            let resp = req.send().await.map_err(|e| map_send_error(&e))?;
            let status = resp.status();
            if status.is_redirection() {
                let Some(loc) = resp.headers().get(LOCATION).and_then(|v| v.to_str().ok()) else {
                    return Err(WebError::Http(format!("{status} without Location header")));
                };
                if hops >= self.cfg.max_redirects {
                    return Err(WebError::TooManyRedirects(self.cfg.max_redirects));
                }
                let next = current
                    .join(loc)
                    .map_err(|e| WebError::InvalidUrl(format!("redirect `{loc}`: {e}")))?;
                self.policy.check_url(&next)?;
                current = next;
                hops += 1;
                continue;
            }
            return read_response(resp, current, hops, cap, &opts.accept_types).await;
        }
    }

    /// POST a JSON payload to `url` without following redirects (so API keys
    /// and request bodies are never bounced to another host).
    ///
    /// # Errors
    /// Policy violations, transport errors, HTTP status >= 300, or a refused
    /// content type.
    pub async fn post_json(
        &self,
        url: &str,
        body: &[u8],
        opts: &GetOptions,
    ) -> Result<Fetched, WebError> {
        let current = self.check(url)?;
        let mut headers = build_headers(&opts.headers)?;
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let cap = opts.max_bytes.unwrap_or(self.cfg.max_bytes);
        let mut req = self
            .client
            .post(current.clone())
            .headers(headers)
            .body(body.to_vec());
        if let Some(t) = opts.timeout {
            req = req.timeout(t);
        }
        let resp = req.send().await.map_err(|e| map_send_error(&e))?;
        read_response(resp, current, 0, cap, &opts.accept_types).await
    }
}

fn build_headers(pairs: &[(String, String)]) -> Result<HeaderMap, WebError> {
    let mut headers = HeaderMap::new();
    for (k, v) in pairs {
        let name = HeaderName::from_bytes(k.as_bytes())
            .map_err(|e| WebError::Config(format!("header name `{k}`: {e}")))?;
        let mut value = HeaderValue::from_str(v)
            .map_err(|_| WebError::Config(format!("header `{k}` has an invalid value")))?;
        value.set_sensitive(true);
        headers.insert(name, value);
    }
    Ok(headers)
}

async fn read_response(
    resp: reqwest::Response,
    url: Url,
    redirects: usize,
    cap: usize,
    accept_types: &[String],
) -> Result<Fetched, WebError> {
    let status = resp.status();
    if status.as_u16() >= 400 || status.is_redirection() {
        return Err(WebError::Status(status.as_u16()));
    }
    let (media_type, charset) = parse_content_type(
        resp.headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
    );
    if let Some(mt) = &media_type {
        if !accept_types.is_empty() && !type_allowed(mt, accept_types) {
            return Err(WebError::ContentType(mt.clone()));
        }
    }
    let mut body = Vec::new();
    let mut truncated = false;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| map_send_error(&e))?;
        let room = cap - body.len();
        if chunk.len() > room {
            body.extend_from_slice(&chunk[..room]);
            truncated = true;
            break;
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Fetched {
        url,
        status: status.as_u16(),
        media_type,
        charset,
        body,
        truncated,
        redirects,
    })
}

fn same_origin(a: &Url, original: &str) -> bool {
    Url::parse(original).is_ok_and(|o| {
        o.scheme() == a.scheme()
            && o.host_str() == a.host_str()
            && o.port_or_known_default() == a.port_or_known_default()
    })
}

fn map_send_error(e: &reqwest::Error) -> WebError {
    // Walk the source chain: a policy violation raised by the resolver is
    // wrapped by hyper / reqwest.
    let mut src: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(s) = src {
        if let Some(v) = s.downcast_ref::<PolicyViolation>() {
            return WebError::Policy(v.clone());
        }
        src = s.source();
    }
    if e.is_timeout() {
        WebError::Timeout
    } else {
        // `without_url` keeps query strings (possibly secrets) out of errors.
        let msg = e.to_string();
        WebError::Http(msg.split(" for url").next().unwrap_or(&msg).to_string())
    }
}

/// Split a `Content-Type` header into (media type, charset), lowercased.
#[must_use]
pub fn parse_content_type(value: Option<&str>) -> (Option<String>, Option<String>) {
    let Some(value) = value else {
        return (None, None);
    };
    let mut parts = value.split(';');
    let media = parts
        .next()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty());
    let charset = parts.find_map(|p| {
        let (k, v) = p.split_once('=')?;
        k.trim()
            .eq_ignore_ascii_case("charset")
            .then(|| v.trim().trim_matches('"').to_ascii_lowercase())
    });
    (media, charset)
}

fn type_allowed(media: &str, allowed: &[String]) -> bool {
    allowed.iter().any(|a| {
        let a = a.to_ascii_lowercase();
        if media == a {
            return true;
        }
        // `application/foo+json` is JSON, `+xml` is XML.
        a.rsplit_once('/')
            .is_some_and(|(_, sub)| media.ends_with(&format!("+{sub}")))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_type_parsing() {
        let (m, c) = parse_content_type(Some("Text/HTML; Charset=\"UTF-8\""));
        assert_eq!(m.as_deref(), Some("text/html"));
        assert_eq!(c.as_deref(), Some("utf-8"));
        assert_eq!(parse_content_type(None), (None, None));
    }

    #[test]
    fn suffix_types_match() {
        let allowed = vec![
            "application/json".to_string(),
            "application/xml".to_string(),
        ];
        assert!(type_allowed("application/vnd.api+json", &allowed));
        assert!(type_allowed("application/json", &allowed));
        assert!(!type_allowed("image/png", &allowed));
    }

    #[test]
    fn latin1_decoding() {
        assert_eq!(
            decode_text(&[0x63, 0x61, 0x66, 0xE9], Some("iso-8859-1")),
            "caf\u{e9}"
        );
        assert_eq!(decode_text("é".as_bytes(), None), "é");
    }
}
