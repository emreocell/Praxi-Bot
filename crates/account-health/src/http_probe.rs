//! Cheap GET of a ledger `evidence_url`: looks for sign-in redirects.
//!
//! The URL is bot-written data, so the request goes through the shared guarded fetcher
//! (`web_tools::HttpFetcher`: no proxies, only checked addresses, manual redirects). On top of
//! that [`SameSitePolicy`] allows `https` only, no user info, and only the ledger site (or a
//! sub-domain of it) for the first request and for every redirect hop.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use url::Url;
use web_tools::{
    DefaultUrlPolicy, GetOptions, HttpConfig, HttpFetcher, PolicyViolation, UrlPolicy, WebError,
};

const TIMEOUT: Duration = Duration::from_secs(12);
const MAX_BODY: usize = 16 * 1024;
const MAX_REDIRECTS: usize = 5;
/// Whole path segments that mean "you are being asked to sign in".
const LOGIN_SEGMENTS: &[&str] = &[
    "login", "logon", "signin", "sign-in", "sign_in", "log-in", "log_in", "session", "sessions",
    "auth", "sso",
];

/// What the evidence page says about the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Evidence {
    /// Reachable and not a sign-in page.
    SignedIn,
    /// 401/403, or a redirect to a sign-in page.
    SignedOut,
    /// Not probed or not answered (blocked URL, other site, network error, 5xx).
    Unknown,
}

/// The ledger site only, over `https`, never with user info, on top of the default SSRF policy.
struct SameSitePolicy {
    site: String,
    inner: DefaultUrlPolicy,
}

impl UrlPolicy for SameSitePolicy {
    fn check_url(&self, url: &Url) -> Result<(), PolicyViolation> {
        if url.scheme() != "https" {
            return Err(PolicyViolation::new("only https is probed"));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(PolicyViolation::new("a URL with user info is not probed"));
        }
        let host = url.host_str().unwrap_or("").to_ascii_lowercase();
        if !on_site(&host, &self.site) {
            return Err(PolicyViolation::new("the page is not on the ledger site"));
        }
        self.inner.check_url(url)
    }

    fn check_ip(&self, host: &str, ip: IpAddr) -> Result<(), PolicyViolation> {
        self.inner.check_ip(host, ip)
    }
}

/// `host` is `site` or a sub-domain of it.
fn on_site(host: &str, site: &str) -> bool {
    let host = host.trim_end_matches('.');
    host == site || host.strip_suffix(site).is_some_and(|p| p.ends_with('.'))
}

fn login_path(url: &Url) -> bool {
    url.path_segments().is_some_and(|mut segs| {
        segs.any(|s| LOGIN_SEGMENTS.contains(&s.to_ascii_lowercase().as_str()))
    })
}

/// Probe `evidence_url` of `site` (a validated lower-case host name).
pub async fn evidence_status(site: &str, evidence_url: &str) -> Evidence {
    let site = site.trim_start_matches("www.").to_ascii_lowercase();
    let policy = SameSitePolicy {
        site,
        inner: DefaultUrlPolicy::default(),
    };
    let cfg = HttpConfig {
        timeout: TIMEOUT,
        max_bytes: MAX_BODY,
        max_redirects: MAX_REDIRECTS,
        user_agent: "praxi-account-health".into(),
        ..HttpConfig::default()
    };
    probe(Arc::new(policy), cfg, evidence_url).await
}

async fn probe(policy: Arc<dyn UrlPolicy>, cfg: HttpConfig, evidence_url: &str) -> Evidence {
    let Ok(fetcher) = HttpFetcher::new(policy, cfg) else {
        return Evidence::Unknown;
    };
    let opts = GetOptions {
        max_bytes: Some(MAX_BODY),
        ..GetOptions::default()
    };
    match fetcher.get(evidence_url, &opts).await {
        Ok(page) => {
            // A sign-in page only counts when the request was sent there; an evidence URL that
            // is itself a `/login` path says nothing.
            if page.redirects > 0 && login_path(&page.url) {
                Evidence::SignedOut
            } else {
                Evidence::SignedIn
            }
        }
        Err(WebError::Status(401 | 403)) => Evidence::SignedOut,
        Err(_) => Evidence::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// The production policy with the loopback block lifted and https waived, so a local mock
    /// server can stand in for the site. The site rule and the redirect re-check stay real.
    struct LocalPolicy {
        site: String,
    }

    impl UrlPolicy for LocalPolicy {
        fn check_url(&self, url: &Url) -> Result<(), PolicyViolation> {
            let host = url.host_str().unwrap_or("");
            if !on_site(host, &self.site) {
                return Err(PolicyViolation::new("the page is not on the ledger site"));
            }
            Ok(())
        }
        fn check_ip(&self, _host: &str, _ip: IpAddr) -> Result<(), PolicyViolation> {
            Ok(())
        }
    }

    async fn local(url: &str) -> Evidence {
        let policy = Arc::new(LocalPolicy {
            site: "127.0.0.1".into(),
        });
        probe(policy, HttpConfig::default(), url).await
    }

    #[tokio::test]
    async fn login_redirect_counts_as_signed_out() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/me"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "/login"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/login"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        assert_eq!(
            local(&format!("{}/me", server.uri())).await,
            Evidence::SignedOut
        );
    }

    #[tokio::test]
    async fn author_path_is_not_a_login() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/author/jane"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        assert_eq!(
            local(&format!("{}/author/jane", server.uri())).await,
            Evidence::SignedIn
        );
    }

    #[tokio::test]
    async fn unauthorized_counts_as_signed_out() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        assert_eq!(
            local(&format!("{}/me", server.uri())).await,
            Evidence::SignedOut
        );
    }

    #[tokio::test]
    async fn redirect_to_another_host_is_not_followed() {
        let other = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&other)
            .await;
        let server = MockServer::start().await;
        let target = other.uri().replace("127.0.0.1", "localhost");
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", target.as_str()))
            .mount(&server)
            .await;
        assert_eq!(
            local(&format!("{}/me", server.uri())).await,
            Evidence::Unknown
        );
    }

    #[test]
    fn site_policy_refuses_http_userinfo_and_other_hosts() {
        let p = SameSitePolicy {
            site: "example.com".into(),
            inner: DefaultUrlPolicy::default(),
        };
        let ok = |u: &str| p.check_url(&Url::parse(u).unwrap()).is_ok();
        assert!(ok("https://example.com/me"));
        assert!(ok("https://app.example.com/me"));
        assert!(!ok("http://example.com/me"));
        assert!(!ok("https://user:pw@example.com/me"));
        assert!(!ok("https://evilexample.com/me"));
        assert!(!ok("https://example.com.evil.net/me"));
        assert!(!ok("https://127.0.0.1/me"));
    }

    #[tokio::test]
    async fn private_ip_and_plain_http_are_unknown() {
        assert_eq!(
            evidence_status("127.0.0.1", "https://127.0.0.1:9/me").await,
            Evidence::Unknown
        );
        assert_eq!(
            evidence_status("example.com", "http://example.com/me").await,
            Evidence::Unknown
        );
    }
}
