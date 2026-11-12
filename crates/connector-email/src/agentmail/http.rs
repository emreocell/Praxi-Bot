//! The only HTTP path to `AgentMail`.
//!
//! Properties, each covered by a test:
//! - the base URL is pinned to one host over `https` (an endpoint that is not
//!   loopback `http` is refused at construction, and every request URL is
//!   re-checked against the pin);
//! - redirects are never followed (any `3xx` is an error);
//! - the API key goes only into the `Authorization` header (marked sensitive),
//!   never into a URL, a log line, an error or `Debug` output;
//! - DNS answers are checked against the SSRF policy (no private, loopback,
//!   link-local or metadata addresses);
//! - connect and total timeouts, and byte caps on every body;
//! - provider failures become the coarse [`ProviderError`] classes; the
//!   provider's own error text is never copied.
//!
//! Attachment downloads go to a signed CDN URL the API returns, so that one
//! request cannot be pinned to the API host: it is `https` only, carries no
//! credentials, follows no redirects and is size-capped.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue, RETRY_AFTER};
use reqwest::{Method, Url};
use serde_json::Value;
use web_tools::policy::{DefaultUrlPolicy, PolicyViolation, UrlPolicy};

use super::ApiKey;
use crate::error::{EmailError, ProviderError};

/// Host of the production API.
pub const PRODUCTION_HOST: &str = "api.agentmail.to";

/// Cap on a JSON response body.
pub(crate) const MAX_JSON_BYTES: usize = 4 * 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(30);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(60);
/// Longest `Retry-After` honoured.
const MAX_RETRY_AFTER_SECS: u64 = 3600;

/// Where the client may connect: one pinned origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentMailEndpoint {
    base: Url,
    loopback_http: bool,
}

impl AgentMailEndpoint {
    /// The production API, `https://api.agentmail.to`.
    ///
    /// # Panics
    /// Never (the URL is a constant).
    #[must_use]
    pub fn production() -> Self {
        Self {
            base: Url::parse(&format!("https://{PRODUCTION_HOST}")).expect("constant URL"),
            loopback_http: false,
        }
    }

    /// A local mock server for tests: plain `http`, and only to `localhost`,
    /// `127.0.0.1` or `[::1]`. Anything else is refused, so this cannot be used
    /// to point the client at a remote host.
    ///
    /// # Errors
    /// [`EmailError::Config`] when `url` is not an `http` loopback origin.
    pub fn loopback_mock_for_tests(url: &str) -> Result<Self, EmailError> {
        let base = Url::parse(url).map_err(|_| EmailError::Config("invalid mock URL".into()))?;
        let loopback = match base.host() {
            Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            None => false,
        };
        if base.scheme() != "http" || !loopback {
            return Err(EmailError::Config(
                "a test endpoint must be http on a loopback host".into(),
            ));
        }
        Ok(Self {
            base,
            loopback_http: true,
        })
    }

    /// The pinned host.
    #[must_use]
    pub fn host(&self) -> &str {
        self.base.host_str().unwrap_or_default()
    }

    fn pinned(&self, url: &Url) -> bool {
        url.scheme() == self.base.scheme()
            && url.host_str() == self.base.host_str()
            && url.port_or_known_default() == self.base.port_or_known_default()
    }
}

struct GuardedResolver {
    policy: Arc<dyn UrlPolicy>,
}

impl Resolve for GuardedResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let policy = Arc::clone(&self.policy);
        Box::pin(async move {
            let host = name.as_str().to_owned();
            let all: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?
                .collect();
            let allowed: Vec<SocketAddr> = all
                .into_iter()
                .filter(|a| policy.check_ip(&host, a.ip()).is_ok())
                .collect();
            if allowed.is_empty() {
                return Err(Box::new(PolicyViolation::new("host resolves to a blocked address"))
                    as Box<dyn std::error::Error + Send + Sync>);
            }
            Ok(Box::new(allowed.into_iter()) as Addrs)
        })
    }
}

struct Inner {
    api: reqwest::Client,
    download: reqwest::Client,
    endpoint: AgentMailEndpoint,
    policy: Arc<dyn UrlPolicy>,
}

/// HTTP client bound to one [`AgentMailEndpoint`]. Cheap to clone.
#[derive(Clone)]
pub struct AgentMailClient {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for AgentMailClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentMailClient")
            .field("host", &self.inner.endpoint.host())
            .finish_non_exhaustive()
    }
}

/// A successful (`2xx`) response.
pub(crate) struct Reply {
    pub body: Value,
}

fn build(
    policy: &Arc<dyn UrlPolicy>,
    timeout: Duration,
    https_only: bool,
) -> Result<reqwest::Client, EmailError> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .connect_timeout(CONNECT_TIMEOUT)
        .user_agent(format!("Praxi-Bot/{} (email)", env!("CARGO_PKG_VERSION")))
        .https_only(https_only)
        .no_proxy()
        .dns_resolver(Arc::new(GuardedResolver {
            policy: Arc::clone(policy),
        }))
        .build()
        .map_err(|_| EmailError::Config("cannot build the HTTP client".into()))
}

impl AgentMailClient {
    /// Build a client for `endpoint`. System proxies are ignored on purpose (a
    /// proxy resolves names itself and would bypass the address checks).
    ///
    /// # Errors
    /// [`EmailError::Config`] when the TLS client cannot be built.
    pub fn new(endpoint: AgentMailEndpoint) -> Result<Self, EmailError> {
        let policy: Arc<dyn UrlPolicy> = Arc::new(DefaultUrlPolicy {
            block_loopback: !endpoint.loopback_http,
            ..DefaultUrlPolicy::default()
        });
        let https_only = !endpoint.loopback_http;
        Ok(Self {
            inner: Arc::new(Inner {
                api: build(&policy, TOTAL_TIMEOUT, https_only)?,
                download: build(&policy, DOWNLOAD_TIMEOUT, https_only)?,
                endpoint,
                policy,
            }),
        })
    }

    /// A client for the production API.
    ///
    /// # Errors
    /// See [`AgentMailClient::new`].
    pub fn production() -> Result<Self, EmailError> {
        Self::new(AgentMailEndpoint::production())
    }

    /// The pinned host.
    #[must_use]
    pub fn host(&self) -> &str {
        self.inner.endpoint.host()
    }

    fn url(&self, segments: &[&str], query: &[(&str, String)]) -> Result<Url, EmailError> {
        let mut url = self.inner.endpoint.base.clone();
        {
            let mut path = url
                .path_segments_mut()
                .map_err(|()| EmailError::Config("invalid endpoint".into()))?;
            path.pop_if_empty();
            for s in segments {
                path.push(s);
            }
        }
        if !query.is_empty() {
            url.query_pairs_mut()
                .extend_pairs(query.iter().map(|(k, v)| (*k, v.as_str())));
        }
        if !self.inner.endpoint.pinned(&url) {
            return Err(EmailError::Config("request is not for the pinned host".into()));
        }
        Ok(url)
    }

    /// Send one API request. `2xx` returns the parsed JSON (`Null` for an
    /// empty body); everything else is a coarse error.
    pub(crate) async fn call(
        &self,
        key: &ApiKey,
        method: Method,
        segments: &[&str],
        query: &[(&str, String)],
        body: Option<&Value>,
        extra: &[(&'static str, &str)],
    ) -> Result<Reply, EmailError> {
        let url = self.url(segments, query)?;
        let mut headers = HeaderMap::new();
        let mut auth = HeaderValue::from_str(&format!("Bearer {}", key.expose()))
            .map_err(|_| EmailError::Input("the API key has an invalid format".into()))?;
        auth.set_sensitive(true);
        headers.insert(AUTHORIZATION, auth);
        for (k, v) in extra {
            let value = HeaderValue::from_str(v)
                .map_err(|_| EmailError::Input("invalid request header".into()))?;
            headers.insert(HeaderName::from_static(k), value);
        }
        let mut req = self.inner.api.request(method, url).headers(headers);
        if let Some(b) = body {
            req = req.json(b);
        }
        let resp = req.send().await.map_err(|e| transport(&e))?;
        let status = resp.status();
        let retry_after = resp
            .headers()
            .get(RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(|s| s.min(MAX_RETRY_AFTER_SECS));
        if let Some(len) = resp.content_length()
            && usize::try_from(len).map_or(true, |l| l > MAX_JSON_BYTES)
        {
            return Err(ProviderError::BadResponse.into());
        }
        let bytes = read_capped(resp, MAX_JSON_BYTES).await?;
        let parsed: Option<Value> = if bytes.is_empty() {
            Some(Value::Null)
        } else {
            serde_json::from_slice(&bytes).ok()
        };
        if status.is_success() {
            return match parsed {
                Some(body) => Ok(Reply { body }),
                None => Err(ProviderError::BadResponse.into()),
            };
        }
        Err(map_status(
            status.as_u16(),
            retry_after,
            parsed.as_ref(),
        ))
    }

    /// Download an attachment from the signed URL the API returned. No
    /// credentials are sent; redirects are refused; more than `max_bytes` is an
    /// error (decided from `Content-Length` before reading, and again while
    /// streaming).
    pub(crate) async fn download(&self, url: &str, max_bytes: usize) -> Result<Vec<u8>, EmailError> {
        let url = Url::parse(url).map_err(|_| EmailError::from(ProviderError::BadResponse))?;
        let local = self.inner.endpoint.loopback_http;
        let scheme_ok = url.scheme() == "https" || (local && self.inner.endpoint.pinned(&url));
        if !scheme_ok || !url.username().is_empty() || url.password().is_some() {
            return Err(ProviderError::BadResponse.into());
        }
        self.inner
            .policy
            .check_url(&url)
            .map_err(|_| EmailError::from(ProviderError::BadResponse))?;
        let resp = self
            .inner
            .download
            .get(url)
            .send()
            .await
            .map_err(|e| transport(&e))?;
        let status = resp.status();
        if status.is_redirection() {
            return Err(ProviderError::Redirect.into());
        }
        if !status.is_success() {
            return Err(map_status(status.as_u16(), None, None));
        }
        if let Some(len) = resp.content_length()
            && usize::try_from(len).map_or(true, |l| l > max_bytes)
        {
            return Err(ProviderError::BadResponse.into());
        }
        read_capped(resp, max_bytes).await
    }
}

async fn read_capped(resp: reqwest::Response, cap: usize) -> Result<Vec<u8>, EmailError> {
    let mut out = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| transport(&e))?;
        if out.len() + chunk.len() > cap {
            return Err(ProviderError::BadResponse.into());
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

/// Coarse class of a transport failure. Never contains the URL or the library's
/// text.
fn transport(e: &reqwest::Error) -> EmailError {
    let mut src: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(s) = src {
        if s.downcast_ref::<PolicyViolation>().is_some() {
            return EmailError::Config("the provider host resolved to a blocked address".into());
        }
        src = s.source();
    }
    ProviderError::Transient.into()
}

/// The provider's machine code, if it is well formed.
fn error_code(body: Option<&Value>) -> Option<String> {
    let code = body?.get("code")?.as_str()?;
    let ok = !code.is_empty()
        && code.len() <= 64
        && code
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    ok.then(|| code.to_owned())
}

/// Map a failing HTTP status to a coarse error.
pub(crate) fn map_status(status: u16, retry_after: Option<u64>, body: Option<&Value>) -> EmailError {
    let code = error_code(body);
    let limit = code.as_deref() == Some("limit_exceeded");
    match status {
        300..=399 => ProviderError::Redirect.into(),
        401 => ProviderError::Auth.into(),
        403 | 409 if limit => ProviderError::Quota.into(),
        403 if code.as_deref() == Some("message_rejected") => {
            ProviderError::Rejected { code }.into()
        }
        403 => ProviderError::Forbidden.into(),
        404 => EmailError::NotFound("not found at the mailbox provider".into()),
        429 => ProviderError::RateLimited {
            retry_after_secs: retry_after,
        }
        .into(),
        500..=599 => ProviderError::Transient.into(),
        _ => ProviderError::Rejected { code }.into(),
    }
}

/// Whether `ip` may never be contacted (the guard the resolver applies).
#[cfg(test)]
pub(crate) fn is_blocked_address(ip: std::net::IpAddr) -> bool {
    DefaultUrlPolicy::default().check_ip("x", ip).is_err()
}
