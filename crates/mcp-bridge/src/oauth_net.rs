//! The only HTTP client OAuth traffic of the bridge uses (review finding 4).
//!
//! rmcp's default OAuth client filters only literal addresses and what its
//! own resolver returns; a configured proxy resolves the target itself, and
//! discovery and dynamic registration run before any check of ours.  This
//! client is installed with `AuthorizationManager::new_with_oauth_http_client`
//! for every OAuth operation (protected-resource and authorization-server
//! discovery, dynamic client registration, code exchange, refresh) and:
//!
//! * never uses a proxy;
//! * requires `https`, or `http` to a loopback address only when the server
//!   opted in (`allow_loopback_http`, local development and tests);
//! * resolves every host itself and refuses it when **any** answer is
//!   loopback (unless opted in), private, CGNAT, link-local / cloud metadata,
//!   unspecified, multicast, broadcast or otherwise reserved;
//! * connects only to the addresses it vetted (no second lookup);
//! * follows redirects itself, vetting every hop the same way and dropping
//!   `Authorization` and cookies when the origin changes.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use rmcp::transport::auth::{
    OAuthHttpClient, OAuthHttpClientError, OAuthHttpClientFuture, OAuthHttpRedirectPolicy,
    OAuthHttpRequest,
};

use crate::oauth::EndpointResolver;

/// Largest response body read from an OAuth endpoint.
const MAX_BODY: usize = 1024 * 1024;
/// Most redirects followed for one operation.
const MAX_REDIRECTS: usize = 5;
/// Timeout of one operation when rmcp suggests none.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Why a request was not sent.  Names the host only, never a query, path or
/// credential.
#[derive(Debug, thiserror::Error)]
pub enum OAuthNetError {
    /// Scheme other than `https` (or `http` to an opted-in loopback host).
    #[error("OAuth request to `{0}` refused: only https is allowed")]
    Scheme(String),
    /// The host resolves to (or is) an address OAuth traffic may not reach.
    #[error(
        "OAuth request to `{0}` refused: it resolves to a private, loopback or reserved address"
    )]
    Address(String),
    /// The host could not be resolved.
    #[error("OAuth request to `{0}` refused: the host cannot be resolved")]
    Unresolved(String),
    /// Too many redirects.
    #[error("OAuth request exceeded {MAX_REDIRECTS} redirects")]
    Redirects,
    /// The response body is larger than allowed.
    #[error("OAuth response body exceeds {MAX_BODY} bytes")]
    TooLarge,
    /// The request could not be built or sent (no URL or body in the text).
    #[error("OAuth request failed: {0}")]
    Transport(String),
}

/// IPv4 inside an IPv6 address: mapped (`::ffff:a.b.c.d`), compatible
/// (`::a.b.c.d`) or NAT64 well-known prefix (`64:ff9b::/96`).
fn embedded_v4(v6: Ipv6Addr) -> Option<Ipv4Addr> {
    if let Some(v4) = v6.to_ipv4_mapped() {
        return Some(v4);
    }
    let s = v6.segments();
    let tail = Ipv4Addr::new(
        (s[6] >> 8) as u8,
        (s[6] & 0xff) as u8,
        (s[7] >> 8) as u8,
        (s[7] & 0xff) as u8,
    );
    if s[..6] == [0, 0, 0, 0, 0, 0] && !v6.is_unspecified() && !v6.is_loopback() {
        return Some(tail);
    }
    if s[0] == 0x0064 && s[1] == 0xff9b && s[2..6] == [0, 0, 0, 0] {
        return Some(tail);
    }
    None
}

/// Whether OAuth traffic may reach `ip`.  Loopback only with the opt-in.
#[must_use]
pub fn address_allowed(ip: IpAddr, allow_loopback: bool) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            if v4.is_loopback() {
                return allow_loopback;
            }
            !(v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_documentation()
                || o[0] == 0
                || (o[0] == 100 && (64..=127).contains(&o[1])) // CGNAT
                || (o[0] == 192 && o[1] == 0 && o[2] == 0) // IETF protocol assignments
                || (o[0] == 198 && (o[1] == 18 || o[1] == 19)) // benchmarking
                || o[0] >= 240) // reserved
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = embedded_v4(v6) {
                return address_allowed(IpAddr::V4(v4), allow_loopback);
            }
            if v6.is_loopback() {
                return allow_loopback;
            }
            let s0 = v6.segments()[0];
            !(v6.is_unspecified()
                || v6.is_multicast()
                || (s0 & 0xffc0) == 0xfe80 // link-local
                || (s0 & 0xffc0) == 0xfec0 // site-local
                || (s0 & 0xfe00) == 0xfc00 // unique local (incl. fd00:ec2::254)
                || (s0 == 0x2001 && v6.segments()[1] == 0x0db8)) // documentation
        }
    }
}

/// The policy client.  Cheap to clone.
#[derive(Clone)]
pub struct PolicyHttpClient {
    allow_loopback: bool,
    resolver: Arc<dyn EndpointResolver>,
}

impl std::fmt::Debug for PolicyHttpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PolicyHttpClient")
            .field("allow_loopback", &self.allow_loopback)
            .finish_non_exhaustive()
    }
}

impl PolicyHttpClient {
    /// With the operating system resolver.
    #[must_use]
    pub fn new(allow_loopback: bool) -> Self {
        Self::with_resolver(
            allow_loopback,
            Arc::new(crate::oauth::SystemEndpointResolver),
        )
    }

    /// With `resolver` (tests use a static table; no real DNS).
    #[must_use]
    pub fn with_resolver(allow_loopback: bool, resolver: Arc<dyn EndpointResolver>) -> Self {
        Self {
            allow_loopback,
            resolver,
        }
    }

    /// Check `url` and return the addresses a connection may use.
    ///
    /// # Errors
    /// A refused scheme, an unresolvable host or a disallowed address.
    pub async fn vet(&self, url: &reqwest::Url) -> Result<Vec<SocketAddr>, OAuthNetError> {
        let host = url.host_str().unwrap_or_default().to_owned();
        let port = url
            .port_or_known_default()
            .ok_or_else(|| OAuthNetError::Scheme(host.clone()))?;
        let found: Vec<IpAddr> = match url.host() {
            Some(url::Host::Ipv4(ip)) => vec![IpAddr::V4(ip)],
            Some(url::Host::Ipv6(ip)) => vec![IpAddr::V6(ip)],
            Some(url::Host::Domain(name)) => self
                .resolver
                .lookup(name.trim_end_matches('.'), port)
                .await
                .filter(|f| !f.is_empty())
                .ok_or_else(|| OAuthNetError::Unresolved(host.clone()))?,
            None => return Err(OAuthNetError::Unresolved(host)),
        };
        if found
            .iter()
            .any(|ip| !address_allowed(*ip, self.allow_loopback))
        {
            return Err(OAuthNetError::Address(host));
        }
        match url.scheme() {
            "https" => {}
            "http"
                if self.allow_loopback && found.iter().all(|ip| canonical(*ip).is_loopback()) => {}
            _ => return Err(OAuthNetError::Scheme(host)),
        }
        Ok(found
            .into_iter()
            .map(|ip| SocketAddr::new(ip, port))
            .collect())
    }

    async fn send(
        &self,
        request: reqwest::Request,
        timeout: Duration,
    ) -> Result<reqwest::Response, OAuthNetError> {
        let addrs = self.vet(request.url()).await?;
        let mut builder = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(timeout);
        if let Some(url::Host::Domain(name)) = request.url().host() {
            // Connect to exactly the vetted answers; no second lookup.
            builder = builder.resolve_to_addrs(name, &addrs);
        }
        let client = builder
            .build()
            .map_err(|_| OAuthNetError::Transport("cannot build the HTTP client".into()))?;
        client
            .execute(request)
            .await
            .map_err(|e| OAuthNetError::Transport(transport_kind(&e)))
    }

    /// `POST` a form to `url` under the same policy (vetted addresses, no
    /// proxy, no redirects) and return the status. The answer's body is
    /// never read: a revocation endpoint has nothing to tell.
    pub(crate) async fn post_form(
        &self,
        url: reqwest::Url,
        form: &[(&str, &str)],
        timeout: Duration,
    ) -> Result<reqwest::StatusCode, OAuthNetError> {
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(form)
            .finish();
        let mut request = reqwest::Request::new(reqwest::Method::POST, url);
        request.headers_mut().insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("application/x-www-form-urlencoded"),
        );
        *request.body_mut() = Some(body.into());
        Ok(self.send(request, timeout).await?.status())
    }

    async fn run(&self, request: OAuthHttpRequest) -> Result<oauth2::HttpResponse, OAuthNetError> {
        let OAuthHttpRequest {
            request,
            redirect_policy,
            timeout,
            ..
        } = request;
        let timeout = timeout.unwrap_or(DEFAULT_TIMEOUT);
        let mut request = reqwest::Request::try_from(request)
            .map_err(|_| OAuthNetError::Transport("malformed request".into()))?;
        let follow = matches!(redirect_policy, OAuthHttpRedirectPolicy::Follow);
        for hop in 0..=MAX_REDIRECTS {
            let retry = request.try_clone();
            let response = self.send(request, timeout).await?;
            let location = response
                .headers()
                .get(http::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|l| response.url().join(l).ok());
            match (follow, response.status().is_redirection(), location, retry) {
                (true, true, Some(next), Some(previous)) => {
                    if hop == MAX_REDIRECTS {
                        return Err(OAuthNetError::Redirects);
                    }
                    request = redirected(previous, response.status(), next);
                }
                _ => return into_http(response).await,
            }
        }
        Err(OAuthNetError::Redirects)
    }
}

fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => embedded_v4(v6).map_or(ip, IpAddr::V4),
        v4 @ IpAddr::V4(_) => v4,
    }
}

/// A transport failure described without the URL (it may carry a code).
fn transport_kind(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "timed out".into()
    } else if e.is_connect() {
        "cannot connect".into()
    } else {
        "transport error".into()
    }
}

/// The next hop of a redirect: 303 (and 301/302 for non-GET) become GET
/// without a body; credentials and cookies never cross to another origin.
fn redirected(
    mut previous: reqwest::Request,
    status: reqwest::StatusCode,
    next: reqwest::Url,
) -> reqwest::Request {
    let same_origin = previous.url().origin() == next.origin();
    let to_get = status == reqwest::StatusCode::SEE_OTHER
        || ((status == reqwest::StatusCode::MOVED_PERMANENTLY
            || status == reqwest::StatusCode::FOUND)
            && previous.method() != reqwest::Method::GET
            && previous.method() != reqwest::Method::HEAD);
    if to_get {
        *previous.method_mut() = reqwest::Method::GET;
        *previous.body_mut() = None;
        previous.headers_mut().remove(http::header::CONTENT_TYPE);
        previous.headers_mut().remove(http::header::CONTENT_LENGTH);
    }
    if !same_origin {
        previous.headers_mut().remove(http::header::AUTHORIZATION);
        previous.headers_mut().remove(http::header::COOKIE);
    }
    *previous.url_mut() = next;
    previous
}

async fn into_http(mut response: reqwest::Response) -> Result<oauth2::HttpResponse, OAuthNetError> {
    let mut builder = http::Response::builder()
        .status(response.status())
        .version(response.version());
    for (name, value) in response.headers() {
        builder = builder.header(name, value);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| OAuthNetError::Transport(transport_kind(&e)))?
    {
        if chunk.len() > MAX_BODY - body.len() {
            return Err(OAuthNetError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    builder
        .body(body)
        .map_err(|_| OAuthNetError::Transport("malformed response".into()))
}

impl OAuthHttpClient for PolicyHttpClient {
    fn execute(&self, request: OAuthHttpRequest) -> OAuthHttpClientFuture<'_> {
        Box::pin(async move {
            self.run(request)
                .await
                .map_err(|e| Box::new(e) as OAuthHttpClientError)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    /// Static DNS: no network.
    pub(crate) struct Table(pub BTreeMap<&'static str, Vec<IpAddr>>);

    #[async_trait::async_trait]
    impl EndpointResolver for Table {
        async fn lookup(&self, host: &str, _port: u16) -> Option<Vec<IpAddr>> {
            self.0.get(host).cloned()
        }
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn address_classes() {
        for refused in [
            "127.0.0.1",
            "10.0.0.5",
            "172.16.0.1",
            "192.168.1.1",
            "100.64.0.1",
            "169.254.169.254",
            "0.0.0.0",
            "255.255.255.255",
            "224.0.0.1",
            "192.0.0.1",
            "198.18.0.1",
            "240.0.0.1",
            "::1",
            "::",
            "fe80::1",
            "fd00:ec2::254",
            "fc00::1",
            "ff02::1",
            "2001:db8::1",
            "::ffff:169.254.169.254",
            "::ffff:10.0.0.1",
            "64:ff9b::a9fe:a9fe",
            "::a9fe:a9fe",
        ] {
            assert!(!address_allowed(ip(refused), false), "{refused}");
        }
        for allowed in ["93.184.216.34", "2606:4700::1111", "::ffff:93.184.216.34"] {
            assert!(address_allowed(ip(allowed), false), "{allowed}");
        }
        assert!(address_allowed(ip("127.0.0.1"), true));
        assert!(address_allowed(ip("::1"), true));
        assert!(!address_allowed(ip("10.0.0.1"), true));
    }

    #[tokio::test]
    async fn every_answer_and_the_scheme_are_vetted() {
        let table = Table(BTreeMap::from([
            ("public.example", vec![ip("93.184.216.34")]),
            ("imds.attacker.test", vec![ip("169.254.169.254")]),
            ("internal.attacker.test", vec![ip("10.0.0.5")]),
            (
                "mixed.attacker.test",
                vec![ip("93.184.216.34"), ip("10.0.0.5")],
            ),
            (
                "mixed2.attacker.test",
                vec![ip("10.0.0.5"), ip("93.184.216.34")],
            ),
            ("mapped.attacker.test", vec![ip("::ffff:169.254.169.254")]),
            ("cgnat.attacker.test", vec![ip("100.100.100.200")]),
            ("loop.attacker.test", vec![ip("127.0.0.1")]),
            ("empty.attacker.test", vec![]),
        ]));
        let client = PolicyHttpClient::with_resolver(false, Arc::new(table));
        let vet = |u: &str| {
            let url = reqwest::Url::parse(u).unwrap();
            let client = client.clone();
            async move { client.vet(&url).await }
        };
        assert!(vet("https://public.example/.well-known/x").await.is_ok());
        for refused in [
            "https://imds.attacker.test/",
            "https://internal.attacker.test/",
            "https://mixed.attacker.test/",
            "https://mixed2.attacker.test/",
            "https://mapped.attacker.test/",
            "https://cgnat.attacker.test/",
            "https://loop.attacker.test/",
            "https://empty.attacker.test/",
            "https://missing.attacker.test/",
            "https://169.254.169.254/latest/meta-data",
            "https://[fd00:ec2::254]/",
            "https://127.0.0.1/",
            "http://public.example/",
            "ftp://public.example/",
        ] {
            assert!(vet(refused).await.is_err(), "{refused}");
        }
        // The loopback opt-in admits loopback, http only there, nothing private.
        let dev = PolicyHttpClient::with_resolver(
            true,
            Arc::new(Table(BTreeMap::from([
                ("loop.dev.test", vec![ip("127.0.0.1")]),
                ("internal.dev.test", vec![ip("10.0.0.5")]),
            ]))),
        );
        let ok = reqwest::Url::parse("http://127.0.0.1:8080/token").unwrap();
        assert!(dev.vet(&ok).await.is_ok());
        let ok = reqwest::Url::parse("http://loop.dev.test:8080/token").unwrap();
        assert!(dev.vet(&ok).await.is_ok());
        let bad = reqwest::Url::parse("https://internal.dev.test/token").unwrap();
        assert!(dev.vet(&bad).await.is_err());
    }

    #[test]
    fn redirects_drop_credentials_across_origins_and_switch_to_get_on_303() {
        let mut first = reqwest::Request::new(
            reqwest::Method::POST,
            reqwest::Url::parse("https://a.example/register").unwrap(),
        );
        first
            .headers_mut()
            .insert(http::header::AUTHORIZATION, "Bearer x".parse().unwrap());
        *first.body_mut() = Some("{}".into());
        let same = redirected(
            first.try_clone().unwrap(),
            reqwest::StatusCode::TEMPORARY_REDIRECT,
            reqwest::Url::parse("https://a.example/register2").unwrap(),
        );
        assert_eq!(same.method(), reqwest::Method::POST);
        assert!(same.headers().contains_key(http::header::AUTHORIZATION));
        let other = redirected(
            first,
            reqwest::StatusCode::SEE_OTHER,
            reqwest::Url::parse("https://b.example/").unwrap(),
        );
        assert_eq!(other.method(), reqwest::Method::GET);
        assert!(other.body().is_none());
        assert!(!other.headers().contains_key(http::header::AUTHORIZATION));
    }
}
