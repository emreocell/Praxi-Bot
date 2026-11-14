//! URL policy: the SSRF guard injected into every outbound request.
//!
//! The [`UrlPolicy`] trait is deliberately small so the shared URL guard of
//! task 048 (`builtin-tools::net`) can implement it and be plugged in. The
//! [`DefaultUrlPolicy`] shipped here is conservative: only `http`/`https`,
//! no loopback, private, link-local, CGNAT, multicast, unspecified or cloud
//! metadata addresses, with optional host allow/deny lists.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use url::{Host, Url};

/// Why a URL or resolved address was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("blocked by URL policy: {0}")]
pub struct PolicyViolation(pub String);

impl PolicyViolation {
    /// Build a violation from a message.
    pub fn new(msg: impl Into<String>) -> Self {
        Self(msg.into())
    }
}

/// Decides which URLs and resolved addresses the web tools may contact.
///
/// The fetcher calls [`UrlPolicy::check_url`] for the initial URL and for every
/// redirect hop, and [`UrlPolicy::check_ip`] for every address a hostname
/// resolves to (DNS-rebinding safe: the connection is only made to a checked
/// address). `check_url` must itself reject IP-literal hosts that are not
/// allowed, because literals never go through DNS.
pub trait UrlPolicy: Send + Sync {
    /// Check scheme, host and port of a URL before any connection is made.
    ///
    /// # Errors
    /// A [`PolicyViolation`] describing why the URL is refused.
    fn check_url(&self, url: &Url) -> Result<(), PolicyViolation>;

    /// Check one resolved address of `host` (the host of the request URL).
    ///
    /// # Errors
    /// A [`PolicyViolation`] describing why the address is refused.
    fn check_ip(&self, host: &str, ip: IpAddr) -> Result<(), PolicyViolation>;

    /// Like [`UrlPolicy::check_url`] for a host the operator explicitly
    /// trusts (for example a local `SearXNG`): the private/loopback block is
    /// waived, everything else (allow/deny lists, protected sockets,
    /// metadata and link-local space) still applies. The default is the
    /// strict check, so a guard that does not support exemptions never
    /// weakens.
    ///
    /// # Errors
    /// A [`PolicyViolation`] describing why the URL is refused.
    fn check_url_trusted(&self, url: &Url) -> Result<(), PolicyViolation> {
        self.check_url(url)
    }

    /// Trusted-host variant of [`UrlPolicy::check_ip`]; see
    /// [`UrlPolicy::check_url_trusted`].
    ///
    /// # Errors
    /// A [`PolicyViolation`] describing why the address is refused.
    fn check_ip_trusted(&self, host: &str, ip: IpAddr) -> Result<(), PolicyViolation> {
        self.check_ip(host, ip)
    }

    /// Check one resolved socket address (host + IP + port).
    ///
    /// # Errors
    /// A [`PolicyViolation`] describing why the socket address is refused.
    fn check_socket(&self, host: &str, sock: SocketAddr) -> Result<(), PolicyViolation> {
        self.check_ip(host, sock.ip())
    }

    /// Trusted-host variant of [`UrlPolicy::check_socket`]; see
    /// [`UrlPolicy::check_url_trusted`].
    ///
    /// # Errors
    /// A [`PolicyViolation`] describing why the socket address is refused.
    fn check_socket_trusted(&self, host: &str, sock: SocketAddr) -> Result<(), PolicyViolation> {
        self.check_ip_trusted(host, sock.ip())
    }
}

/// Wraps an injected policy and additionally trusts exactly one `host:port`
/// (the configured `SearXNG` instance). Every other URL goes through the
/// inner policy unchanged; the trusted host on any other port is refused.
pub struct TrustedHostPolicy {
    inner: std::sync::Arc<dyn UrlPolicy>,
    host: String,
    port: u16,
}

impl TrustedHostPolicy {
    /// Trust `host` on `port` on top of `inner`.
    #[must_use]
    pub fn new(inner: std::sync::Arc<dyn UrlPolicy>, host: &str, port: u16) -> Self {
        Self {
            inner,
            host: host.trim_end_matches('.').to_ascii_lowercase(),
            port,
        }
    }

    fn same_host(&self, host: &str) -> bool {
        host.trim_end_matches('.').eq_ignore_ascii_case(&self.host)
    }
}

impl UrlPolicy for TrustedHostPolicy {
    fn check_url(&self, url: &Url) -> Result<(), PolicyViolation> {
        let on_host = url.host_str().is_some_and(|h| self.same_host(h));
        if !on_host {
            return self.inner.check_url(url);
        }
        if url.port_or_known_default() != Some(self.port) {
            return Err(PolicyViolation::new(format!(
                "host `{}` is only trusted on port {}",
                self.host, self.port
            )));
        }
        self.inner.check_url_trusted(url)
    }

    fn check_ip(&self, host: &str, ip: IpAddr) -> Result<(), PolicyViolation> {
        if self.same_host(host) {
            // Every URL for this host already passed `check_url` on the
            // trusted port, so pass that port so `blocked_sockets` still applies.
            self.inner
                .check_socket_trusted(host, SocketAddr::new(ip, self.port))
        } else {
            self.inner.check_ip(host, ip)
        }
    }

    fn check_socket(&self, host: &str, sock: SocketAddr) -> Result<(), PolicyViolation> {
        if self.same_host(host) {
            if sock.port() != self.port {
                return Err(PolicyViolation::new(format!(
                    "host `{}` is only trusted on port {}",
                    self.host, self.port
                )));
            }
            self.inner.check_socket_trusted(host, sock)
        } else {
            self.inner.check_socket(host, sock)
        }
    }

    fn check_socket_trusted(&self, host: &str, sock: SocketAddr) -> Result<(), PolicyViolation> {
        if self.same_host(host) && sock.port() != self.port {
            return Err(PolicyViolation::new(format!(
                "host `{}` is only trusted on port {}",
                self.host, self.port
            )));
        }
        self.inner.check_socket_trusted(host, sock)
    }
}

/// The built-in conservative policy.
#[derive(Debug, Clone)]
pub struct DefaultUrlPolicy {
    /// If non-empty, only these hosts (and their subdomains) are allowed.
    pub allow_hosts: Vec<String>,
    /// These hosts (and their subdomains) are always refused.
    pub deny_hosts: Vec<String>,
    /// Hosts exempt from the private/loopback address block (for example a
    /// user-configured local `SearXNG` instance). Metadata and link-local
    /// addresses stay blocked even for these.
    pub private_ok_hosts: Vec<String>,
    /// Block private LAN ranges (RFC 1918, ULA, CGNAT). Default `true`.
    pub block_private: bool,
    /// Block loopback addresses. Default `true`; tests set it to `false`.
    pub block_loopback: bool,
    /// Extra socket addresses that are always refused, for example the
    /// daemon's own API listener. Matched exactly against IP-literal URLs;
    /// an entry with port `0` blocks that IP on every port, including after
    /// DNS resolution.
    pub blocked_sockets: Vec<SocketAddr>,
}

impl Default for DefaultUrlPolicy {
    fn default() -> Self {
        Self {
            allow_hosts: Vec::new(),
            deny_hosts: Vec::new(),
            private_ok_hosts: Vec::new(),
            block_private: true,
            block_loopback: true,
            blocked_sockets: Vec::new(),
        }
    }
}

fn host_matches(host: &str, pattern: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let pattern = pattern
        .trim()
        .trim_start_matches("*.")
        .trim_end_matches('.')
        .to_ascii_lowercase();
    host == pattern || host.ends_with(&format!(".{pattern}"))
}

/// Fold IPv4-mapped and NAT64 IPv6 addresses to the embedded IPv4 address.
#[must_use]
pub fn canonical_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return IpAddr::V4(v4);
            }
            let s = v6.segments();
            let o = v6.octets();
            // NAT64 64:ff9b::/96 and IPv4-compatible ::a.b.c.d (deprecated;
            // `::` and `::1` are left alone).
            if (s[0] == 0x64 && s[1] == 0xff9b && s[2..6] == [0, 0, 0, 0])
                || (s[..6] == [0; 6] && (s[6] != 0 || s[7] > 1))
            {
                return IpAddr::V4(Ipv4Addr::new(o[12], o[13], o[14], o[15]));
            }
            // 6to4 2002:AABB:CCDD::/48 embeds the IPv4 address in bits 16..48.
            if s[0] == 0x2002 {
                return IpAddr::V4(Ipv4Addr::new(o[2], o[3], o[4], o[5]));
            }
            IpAddr::V6(v6)
        }
        v4 @ IpAddr::V4(_) => v4,
    }
}

/// Cloud metadata and link-local space: never reachable, whatever the config.
#[must_use]
pub fn is_always_blocked(ip: IpAddr) -> bool {
    match canonical_ip(ip) {
        IpAddr::V4(v4) => {
            v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.octets() == [100, 100, 100, 200]
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            v6.is_unspecified()
                || v6.is_multicast()
                || (s[0] & 0xffc0) == 0xfe80
                || v6 == Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x254)
        }
    }
}

/// Loopback addresses.
#[must_use]
pub fn is_loopback(ip: IpAddr) -> bool {
    canonical_ip(ip).is_loopback()
}

/// Private LAN, ULA, CGNAT and benchmark / reserved ranges.
#[must_use]
pub fn is_private(ip: IpAddr) -> bool {
    match canonical_ip(ip) {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_private()
                || (o[0] == 100 && (o[1] & 0xc0) == 64)
                || (o[0] == 198 && (o[1] & 0xfe) == 18)
                || o[0] == 0
                || (o[0] == 192 && o[1] == 0 && o[2] == 0)
                || o[0] >= 240
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            (s[0] & 0xfe00) == 0xfc00 || (s[0] & 0xffc0) == 0xfec0
        }
    }
}

impl DefaultUrlPolicy {
    fn host_exempt(&self, host: &str) -> bool {
        self.private_ok_hosts.iter().any(|p| host_matches(host, p))
    }

    fn check_addr(&self, host: &str, ip: IpAddr, trusted: bool) -> Result<(), PolicyViolation> {
        if is_always_blocked(ip) {
            return Err(PolicyViolation::new(format!(
                "address {ip} is link-local, metadata or otherwise reserved"
            )));
        }
        if !trusted && !self.host_exempt(host) {
            if self.block_loopback && is_loopback(ip) {
                return Err(PolicyViolation::new(format!("loopback address {ip}")));
            }
            if self.block_private && is_private(ip) {
                return Err(PolicyViolation::new(format!("private address {ip}")));
            }
        }
        Ok(())
    }

    fn check_blocked_socket(&self, sock: SocketAddr) -> Result<(), PolicyViolation> {
        let ip = canonical_ip(sock.ip());
        if self
            .blocked_sockets
            .iter()
            .any(|s| canonical_ip(s.ip()) == ip && (s.port() == sock.port() || s.port() == 0))
        {
            return Err(PolicyViolation::new(format!(
                "{sock} is a protected local service"
            )));
        }
        Ok(())
    }
}

fn is_loopback_name(host: &str) -> bool {
    host == "localhost" || host.ends_with(".localhost") || host == "ip6-localhost"
}

impl DefaultUrlPolicy {
    fn check_url_inner(&self, url: &Url, trusted: bool) -> Result<(), PolicyViolation> {
        if !matches!(url.scheme(), "http" | "https") {
            return Err(PolicyViolation::new(format!(
                "scheme `{}` not allowed (http/https only)",
                url.scheme()
            )));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(PolicyViolation::new("credentials in URL are not allowed"));
        }
        let host = url
            .host()
            .ok_or_else(|| PolicyViolation::new("URL has no host"))?;
        let port = url.port_or_known_default().unwrap_or(0);
        let host_str = match &host {
            Host::Domain(d) => (*d).to_string(),
            Host::Ipv4(a) => a.to_string(),
            Host::Ipv6(a) => a.to_string(),
        };
        if self.deny_hosts.iter().any(|p| host_matches(&host_str, p)) {
            return Err(PolicyViolation::new(format!("host `{host_str}` is denied")));
        }
        if !self.allow_hosts.is_empty()
            && !self.allow_hosts.iter().any(|p| host_matches(&host_str, p))
        {
            return Err(PolicyViolation::new(format!(
                "host `{host_str}` is not on the allow list"
            )));
        }
        if let Host::Domain(d) = &host {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            if is_loopback_name(&d) {
                if self.block_loopback && !trusted && !self.host_exempt(&d) {
                    return Err(PolicyViolation::new("localhost is blocked"));
                }
                // Even when loopback is waived (trusted SearXNG or block_loopback = false),
                // loopback names must never reach a protected daemon socket.
                self.check_blocked_socket(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port))?;
                self.check_blocked_socket(SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), port))?;
            }
        }
        // `url` normalises decimal/hex/octal IPv4 forms to Host::Ipv4.
        let literal = match host {
            Host::Ipv4(a) => Some(IpAddr::V4(a)),
            Host::Ipv6(a) => Some(IpAddr::V6(a)),
            Host::Domain(_) => None,
        };
        if let Some(ip) = literal {
            self.check_addr(&host_str, ip, trusted)?;
            self.check_blocked_socket(SocketAddr::new(ip, port))?;
        }
        Ok(())
    }

    fn check_ip_inner(&self, host: &str, ip: IpAddr, trusted: bool) -> Result<(), PolicyViolation> {
        self.check_addr(host, ip, trusted)?;
        // The port is unknown for DNS results: only port-0 entries apply.
        if self
            .blocked_sockets
            .iter()
            .any(|s| s.port() == 0 && canonical_ip(s.ip()) == canonical_ip(ip))
        {
            return Err(PolicyViolation::new(format!("address {ip} is blocked")));
        }
        Ok(())
    }
}

impl UrlPolicy for DefaultUrlPolicy {
    fn check_url(&self, url: &Url) -> Result<(), PolicyViolation> {
        self.check_url_inner(url, false)
    }

    fn check_ip(&self, host: &str, ip: IpAddr) -> Result<(), PolicyViolation> {
        self.check_ip_inner(host, ip, false)
    }

    fn check_url_trusted(&self, url: &Url) -> Result<(), PolicyViolation> {
        self.check_url_inner(url, true)
    }

    fn check_ip_trusted(&self, host: &str, ip: IpAddr) -> Result<(), PolicyViolation> {
        self.check_ip_inner(host, ip, true)
    }

    fn check_socket(&self, host: &str, sock: SocketAddr) -> Result<(), PolicyViolation> {
        self.check_addr(host, sock.ip(), false)?;
        self.check_blocked_socket(sock)
    }

    fn check_socket_trusted(&self, host: &str, sock: SocketAddr) -> Result<(), PolicyViolation> {
        self.check_addr(host, sock.ip(), true)?;
        self.check_blocked_socket(sock)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(p: &DefaultUrlPolicy, u: &str) -> Result<(), PolicyViolation> {
        p.check_url(&Url::parse(u).unwrap())
    }

    #[test]
    fn schemes_restricted() {
        let p = DefaultUrlPolicy::default();
        assert!(check(&p, "https://example.com/").is_ok());
        assert!(check(&p, "http://example.com/").is_ok());
        assert!(check(&p, "file:///etc/passwd").is_err());
        assert!(check(&p, "ftp://example.com/").is_err());
        assert!(check(&p, "https://user:pw@example.com/").is_err());
    }

    #[test]
    fn private_and_loopback_literals_blocked() {
        let p = DefaultUrlPolicy::default();
        for u in [
            "http://127.0.0.1/",
            "http://localhost/",
            "http://foo.localhost/",
            "http://[::1]/",
            "http://10.0.0.5/",
            "http://192.168.1.1/",
            "http://172.16.0.1/",
            "http://100.64.0.1/",
            "http://169.254.169.254/latest/meta-data",
            "http://[fe80::1]/",
            "http://[fd00::1]/",
            "http://[::ffff:127.0.0.1]/",
            "http://[::ffff:169.254.169.254]/",
            "http://0.0.0.0/",
            "http://2130706433/",
            "http://0x7f000001/",
            "http://0177.0.0.1/",
            "http://2852039166/",
        ] {
            assert!(check(&p, u).is_err(), "{u} must be blocked");
        }
        for u in [
            "http://[::127.0.0.1]/",
            "http://[::a00:1]/",
            "http://[2002:7f00:1::]/",
            "http://[2002:a9fe:a9fe::1]/",
            "http://[2002:c0a8:101::1]/",
            "http://192.0.0.8/",
        ] {
            assert!(check(&p, u).is_err(), "{u} must be blocked");
        }
        assert!(check(&p, "http://[2002:5db8:d822::1]/").is_ok());
        assert!(check(&p, "http://93.184.216.34/").is_ok());
        assert!(check(&p, "http://[2606:2800:220:1::1]/").is_ok());
    }

    #[test]
    fn metadata_blocked_even_when_private_allowed() {
        let p = DefaultUrlPolicy {
            block_private: false,
            block_loopback: false,
            ..DefaultUrlPolicy::default()
        };
        assert!(check(&p, "http://127.0.0.1/").is_ok());
        assert!(check(&p, "http://10.1.2.3/").is_ok());
        assert!(check(&p, "http://169.254.169.254/").is_err());
        assert!(check(&p, "http://[fe80::1]/").is_err());
    }

    #[test]
    fn host_lists() {
        let p = DefaultUrlPolicy {
            allow_hosts: vec!["example.com".into()],
            deny_hosts: vec!["evil.example.com".into()],
            ..DefaultUrlPolicy::default()
        };
        assert!(check(&p, "https://example.com/").is_ok());
        assert!(check(&p, "https://docs.example.com/").is_ok());
        assert!(check(&p, "https://evil.example.com/").is_err());
        assert!(check(&p, "https://notexample.com/").is_err());
        assert!(check(&p, "https://other.org/").is_err());
    }

    #[test]
    fn protected_daemon_socket() {
        let p = DefaultUrlPolicy {
            block_loopback: false,
            blocked_sockets: vec!["127.0.0.1:7777".parse().unwrap()],
            ..DefaultUrlPolicy::default()
        };
        assert!(check(&p, "http://127.0.0.1:7777/api").is_err());
        assert!(check(&p, "http://127.0.0.1:8080/").is_ok());
        assert!(check(&p, "http://[::ffff:127.0.0.1]:7777/").is_err());
    }

    #[test]
    fn dns_results_checked_and_exempt_hosts() {
        let p = DefaultUrlPolicy {
            private_ok_hosts: vec!["searx.lan".into()],
            ..DefaultUrlPolicy::default()
        };
        let lan: IpAddr = "192.168.1.20".parse().unwrap();
        assert!(p.check_ip("evil.test", lan).is_err());
        assert!(p.check_ip("searx.lan", lan).is_ok());
        let meta: IpAddr = "169.254.169.254".parse().unwrap();
        assert!(p.check_ip("searx.lan", meta).is_err());
    }

    #[test]
    fn trusted_host_wraps_inner_policy() {
        use std::sync::Arc;
        let inner: Arc<dyn UrlPolicy> = Arc::new(DefaultUrlPolicy {
            blocked_sockets: vec!["10.0.0.5:7777".parse().unwrap()],
            deny_hosts: vec!["bad.lan".into()],
            ..DefaultUrlPolicy::default()
        });
        let p = TrustedHostPolicy::new(Arc::clone(&inner), "searx.lan", 8080);
        let u = |s: &str| Url::parse(s).unwrap();
        assert!(p.check_url(&u("http://searx.lan:8080/")).is_ok());
        assert!(p.check_url(&u("http://searx.lan:7777/")).is_err());
        assert!(p.check_url(&u("http://127.0.0.1/")).is_err());
        assert!(p.check_url(&u("http://bad.lan:8080/")).is_err());
        let lan: IpAddr = "10.0.0.5".parse().unwrap();
        assert!(p.check_ip("searx.lan", lan).is_ok());
        assert!(p.check_ip("other.lan", lan).is_err());
        let meta: IpAddr = "169.254.169.254".parse().unwrap();
        assert!(p.check_ip("searx.lan", meta).is_err());
        // Protected sockets still apply on a trusted literal host.
        let lit = TrustedHostPolicy::new(Arc::clone(&inner), "10.0.0.5", 7777);
        assert!(lit.check_url(&u("http://10.0.0.5:7777/")).is_err());

        // Protected daemon loopback socket is refused for `localhost` and DNS results on that port.
        let inner_loop: Arc<dyn UrlPolicy> = Arc::new(DefaultUrlPolicy {
            blocked_sockets: vec!["127.0.0.1:47000".parse().unwrap()],
            ..DefaultUrlPolicy::default()
        });
        let trusted_daemon = TrustedHostPolicy::new(Arc::clone(&inner_loop), "localhost", 47000);
        assert!(
            trusted_daemon
                .check_url(&u("http://localhost:47000/v1/web"))
                .is_err()
        );
        assert!(
            trusted_daemon
                .check_ip("localhost", "127.0.0.1".parse().unwrap())
                .is_err()
        );
        let trusted_searx = TrustedHostPolicy::new(inner_loop, "localhost", 8080);
        assert!(
            trusted_searx
                .check_url(&u("http://localhost:8080/"))
                .is_ok()
        );
        assert!(
            trusted_searx
                .check_ip("localhost", "127.0.0.1".parse().unwrap())
                .is_ok()
        );
    }
}
