//! Egress guard for index downloads and index-listed git URLs.
//!
//! Every hop is re-validated (https, named host, no userinfo) and the host is
//! resolved first; all resolved addresses must be public. The request is then
//! pinned to the checked address (`curl --resolve`), so a DNS answer that
//! changes between check and fetch cannot redirect the request.

use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};
use std::process::{Command, Stdio};

use super::HubError;
use super::source::{MAX_INDEX_BYTES, drain_capped, read_capped};

/// Redirect hops followed by hand.
pub const MAX_HOPS: usize = 3;

/// Looks up the addresses of a host name.
pub trait Resolver: Send + Sync {
    /// All addresses of `host`.
    ///
    /// # Errors
    /// Lookup failure.
    fn resolve(&self, host: &str, port: u16) -> Result<Vec<IpAddr>, HubError>;
}

/// Uses the system resolver.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemResolver;

impl Resolver for SystemResolver {
    fn resolve(&self, host: &str, port: u16) -> Result<Vec<IpAddr>, HubError> {
        (host, port)
            .to_socket_addrs()
            .map(|it| it.map(|a| a.ip()).collect())
            .map_err(|e| HubError::Source(format!("cannot resolve `{host}`: {e}")))
    }
}

fn blocked_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_multicast()
        || ip.is_broadcast()
        || (o[0] == 100 && (64..128).contains(&o[1]))
        || o[0] == 0
}

fn blocked_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return blocked_v4(v4);
    }
    let s = ip.segments();
    ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || (s[0] & 0xfe00) == 0xfc00
        || (s[0] & 0xffc0) == 0xfe80
}

/// Loopback, private, CGNAT, link-local, unique-local, unspecified or multicast.
#[must_use]
pub fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => blocked_v4(v),
        IpAddr::V6(v) => blocked_v6(v),
    }
}

/// A checked target: the host to put in the request and the address to use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// Host name from the URL.
    pub host: String,
    /// Port (443 by default).
    pub port: u16,
    /// A public address of the host.
    pub ip: IpAddr,
}

/// `https://` + named host (no userinfo, IP literal or `localhost`).
fn split_https_host(url: &str) -> Result<(String, u16), HubError> {
    let bad = || HubError::Source(format!("address not allowed: `{url}`"));
    let rest = url
        .get(..8)
        .filter(|p| p.eq_ignore_ascii_case("https://"))
        .map(|_| &url[8..])
        .ok_or_else(bad)?;
    let auth = rest.split(['/', '?', '#']).next().unwrap_or("");
    if auth.is_empty()
        || auth.contains(['@', '[', ']', '\\'])
        || auth.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(bad());
    }
    let (host, port) = match auth.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().map_err(|_| bad())?),
        None => (auth, 443),
    };
    let numeric = host.chars().all(|c| c.is_ascii_digit() || c == '.');
    if host.is_empty()
        || !host.contains('.')
        || numeric
        || host.eq_ignore_ascii_case("localhost")
        || port == 0
    {
        return Err(bad());
    }
    Ok((host.to_ascii_lowercase(), port))
}

/// Validate `url` and resolve it; every resolved address must be public.
///
/// # Errors
/// [`HubError::Source`] when the URL is not an https named host, the lookup
/// fails, or any address is non-public.
pub fn check_public_https(url: &str, resolver: &dyn Resolver) -> Result<Target, HubError> {
    let all = resolve_public(url, resolver)?;
    Ok(Target {
        host: all.host,
        port: all.port,
        ip: all.ips[0],
    })
}

/// A host whose every resolved address passed the public-address check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitTarget {
    /// Host name from the URL.
    pub host: String,
    /// Port (443 by default).
    pub port: u16,
    /// All validated public addresses (never empty).
    pub ips: Vec<IpAddr>,
}

fn resolve_public(url: &str, resolver: &dyn Resolver) -> Result<GitTarget, HubError> {
    let (host, port) = split_https_host(url)?;
    let ips = resolver.resolve(&host, port)?;
    if ips.is_empty() || ips.iter().any(|ip| is_blocked_ip(*ip)) {
        return Err(HubError::Source(format!(
            "`{host}` does not resolve to a public address"
        )));
    }
    Ok(GitTarget { host, port, ips })
}

/// Index `git` URL for the in-app flow: https only (no ssh), public host.
///
/// # Errors
/// [`HubError::Source`] describing the problem.
pub fn check_index_git_url(url: &str, resolver: &dyn Resolver) -> Result<GitTarget, HubError> {
    super::source::validate_git_url(url)?;
    resolve_public(url, resolver)
}

/// One HTTP response without redirects followed.
#[derive(Debug)]
pub struct Response {
    /// Status code.
    pub status: u16,
    /// `Location` header, if any.
    pub location: Option<String>,
    /// Body (capped).
    pub body: Vec<u8>,
}

/// Sends one request to a checked target.
pub trait Transport: Send + Sync {
    /// GET `url`, connecting to `target.ip`, without following redirects.
    ///
    /// # Errors
    /// Transport failure or oversized body.
    fn request(&self, url: &str, target: &Target) -> Result<Response, HubError>;
}

/// `curl` with a pinned address and no redirects.
#[derive(Debug, Default, Clone, Copy)]
pub struct CurlTransport;

pub(super) fn pinned_curl_args(url: &str, target: &Target, header_file: &str) -> Vec<String> {
    let ip = match target.ip {
        IpAddr::V6(v6) => format!("[{v6}]"),
        IpAddr::V4(v4) => v4.to_string(),
    };
    [
        "--silent",
        "--show-error",
        "--proto",
        "=https",
        "--max-redirs",
        "0",
        "--max-time",
        "30",
        "--dump-header",
        header_file,
        "--resolve",
        &format!("{}:{}:{ip}", target.host, target.port),
        "--output",
        "-",
        "--",
        url,
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect()
}

impl Transport for CurlTransport {
    fn request(&self, url: &str, target: &Target) -> Result<Response, HubError> {
        let dir = tempfile::tempdir().map_err(|e| HubError::Source(format!("temp dir: {e}")))?;
        let hdr = dir.path().join("h");
        let mut child = Command::new("curl")
            .args(pinned_curl_args(url, target, &hdr.to_string_lossy()))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| HubError::Source(format!("cannot run curl: {e}")))?;
        let stderr = child.stderr.take();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| HubError::Source("curl stdout missing".into()))?;
        let err_thread = std::thread::spawn(move || {
            stderr.map_or_else(Vec::new, |pipe| drain_capped(pipe, 64 * 1024))
        });
        let body = read_capped(stdout, MAX_INDEX_BYTES);
        if body.is_err() {
            let _ = child.kill();
        }
        let status = child
            .wait()
            .map_err(|e| HubError::Source(format!("curl wait: {e}")))?;
        let stderr_buf = err_thread.join().unwrap_or_default();
        let body = body?;
        if !status.success() {
            return Err(HubError::Source(format!(
                "curl failed: {}",
                String::from_utf8_lossy(&stderr_buf).trim()
            )));
        }
        let mut raw = String::new();
        std::fs::File::open(&hdr)
            .and_then(|f| f.take(64 * 1024).read_to_string(&mut raw))
            .map_err(|e| HubError::Source(format!("read headers: {e}")))?;
        let (code, location) = parse_last_header_block(&raw)
            .ok_or_else(|| HubError::Source("no HTTP status".into()))?;
        Ok(Response {
            status: code,
            location,
            body,
        })
    }
}

/// Status and `Location` of the last response block in a header dump.
fn parse_last_header_block(raw: &str) -> Option<(u16, Option<String>)> {
    let block = raw
        .rsplit("\r\n\r\n")
        .map(str::trim)
        .find(|b| !b.is_empty())?;
    let mut lines = block.lines();
    let code = lines
        .next()?
        .split_whitespace()
        .nth(1)?
        .parse::<u16>()
        .ok()?;
    let location = lines.find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.eq_ignore_ascii_case("location")
            .then(|| v.trim().to_owned())
    });
    Some((code, location))
}

fn join_location(base: &str, loc: &str) -> String {
    if loc.contains("://") {
        return loc.to_owned(); // scheme is checked on the next hop
    }
    let origin_end = base[8..]
        .find(['/', '?', '#'])
        .map_or(base.len(), |i| i + 8);
    if let Some(rest) = loc.strip_prefix("//") {
        return format!("https://{rest}");
    }
    if loc.starts_with('/') {
        return format!("{}{loc}", &base[..origin_end]);
    }
    let dir_end = base
        .rfind('/')
        .filter(|i| *i >= origin_end)
        .map_or(origin_end, |i| i + 1);
    let sep = if base[..dir_end].ends_with('/') {
        ""
    } else {
        "/"
    };
    format!("{}{sep}{loc}", &base[..dir_end])
}

/// GET with manual redirects; every hop is checked and pinned.
///
/// # Errors
/// Refused hop, too many hops, HTTP error status, or transport failure.
pub fn guarded_get(
    url: &str,
    resolver: &dyn Resolver,
    transport: &dyn Transport,
) -> Result<Vec<u8>, HubError> {
    let mut current = url.to_owned();
    for _ in 0..=MAX_HOPS {
        let target = check_public_https(&current, resolver)?;
        let resp = transport.request(&current, &target)?;
        match resp.status {
            301 | 302 | 303 | 307 | 308 => {
                let loc = resp
                    .location
                    .ok_or_else(|| HubError::Source("redirect without Location".into()))?;
                current = join_location(&current, &loc);
            }
            200..=299 => return Ok(resp.body),
            s => return Err(HubError::Source(format!("index request failed: HTTP {s}"))),
        }
    }
    Err(HubError::Source("too many redirects".into()))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;

    struct FakeDns(HashMap<&'static str, Vec<IpAddr>>);
    impl Resolver for FakeDns {
        fn resolve(&self, host: &str, _port: u16) -> Result<Vec<IpAddr>, HubError> {
            self.0
                .get(host)
                .cloned()
                .ok_or_else(|| HubError::Source("nxdomain".into()))
        }
    }

    struct FakeNet {
        pages: HashMap<&'static str, (u16, Option<&'static str>)>,
        seen: Mutex<Vec<(String, IpAddr)>>,
    }
    impl Transport for FakeNet {
        fn request(&self, url: &str, t: &Target) -> Result<Response, HubError> {
            self.seen.lock().unwrap().push((url.to_owned(), t.ip));
            let (status, location) = self.pages[url];
            Ok(Response {
                status,
                location: location.map(str::to_owned),
                body: b"{}".to_vec(),
            })
        }
    }

    fn dns() -> FakeDns {
        let ip = |s: &str| vec![s.parse::<IpAddr>().unwrap()];
        FakeDns(HashMap::from([
            ("good.example.com", ip("93.184.216.34")),
            ("evil.example.com", ip("10.0.0.5")),
            ("meta.example.com", ip("169.254.169.254")),
            (
                "mixed.example.com",
                vec![
                    "93.184.216.34".parse().unwrap(),
                    "127.0.0.1".parse().unwrap(),
                ],
            ),
        ]))
    }

    #[test]
    fn public_host_is_fetched_pinned_to_checked_address() {
        let net = FakeNet {
            pages: HashMap::from([("https://good.example.com/i.json", (200, None))]),
            seen: Mutex::default(),
        };
        let body = guarded_get("https://good.example.com/i.json", &dns(), &net).unwrap();
        assert_eq!(body, b"{}");
        assert_eq!(net.seen.lock().unwrap()[0].1.to_string(), "93.184.216.34");
    }

    #[test]
    fn redirect_to_loopback_or_metadata_is_refused() {
        for loc in [
            "https://127.0.0.1/x",
            "https://169.254.169.254/latest",
            "https://meta.example.com/x",
        ] {
            let net = FakeNet {
                pages: HashMap::from([("https://good.example.com/i.json", (302, Some(loc)))]),
                seen: Mutex::default(),
            };
            assert!(
                guarded_get("https://good.example.com/i.json", &dns(), &net).is_err(),
                "{loc}"
            );
            assert_eq!(net.seen.lock().unwrap().len(), 1, "second hop requested");
        }
    }

    #[test]
    fn dns_name_resolving_to_private_address_is_refused() {
        assert!(check_public_https("https://evil.example.com/a", &dns()).is_err());
        assert!(check_public_https("https://mixed.example.com/a", &dns()).is_err());
    }

    #[test]
    fn index_git_url_must_be_public_https() {
        let ok = check_index_git_url("https://good.example.com/r.git", &dns()).unwrap();
        assert_eq!(ok.host, "good.example.com");
        assert_eq!(ok.port, 443);
        assert_eq!(ok.ips.len(), 1);
        assert!(check_index_git_url("https://10.0.0.1/r.git", &dns()).is_err());
        assert!(check_index_git_url("https://evil.example.com/r.git", &dns()).is_err());
        assert!(check_index_git_url("ssh://git@good.example.com/r.git", &dns()).is_err());
    }

    #[test]
    fn redirect_chain_is_limited() {
        let net = FakeNet {
            pages: HashMap::from([("https://good.example.com/a", (302, Some("/a")))]),
            seen: Mutex::default(),
        };
        assert!(guarded_get("https://good.example.com/a", &dns(), &net).is_err());
        assert_eq!(net.seen.lock().unwrap().len(), MAX_HOPS + 1);
    }

    #[test]
    fn address_classes_and_header_parse() {
        for s in [
            "10.1.1.1",
            "172.16.0.1",
            "192.168.1.1",
            "100.64.0.1",
            "127.0.0.1",
            "0.0.0.0",
            "224.0.0.1",
            "::1",
            "fe80::1",
            "fd00::1",
            "::ffff:10.0.0.1",
        ] {
            assert!(is_blocked_ip(s.parse().unwrap()), "{s}");
        }
        assert!(!is_blocked_ip("93.184.216.34".parse().unwrap()));
        let (c, l) = parse_last_header_block("HTTP/2 302\r\nLocation: /x\r\n\r\n").unwrap();
        assert_eq!((c, l.as_deref()), (302, Some("/x")));
    }
}
