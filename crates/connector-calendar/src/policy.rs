//! Where a calendar may be dialled.
//!
//! The address of a calendar is typed by a person (or sent to the settings API),
//! and a server can send more addresses back during discovery. The
//! [`web_tools::UrlPolicy`] shared with the web tools decides what is reachable:
//! link-local and cloud-metadata space, the unspecified and multicast ranges and
//! the daemon's own API port are never dialled, whatever name leads there.
//! Loopback and LAN addresses stay open on purpose: a calendar on the owner's
//! own machine or home network (Radicale, Nextcloud) is the common self-hosted
//! setup.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use url::{Host, Url};
use web_tools::UrlPolicy;
use web_tools::policy::{DefaultUrlPolicy, canonical_ip, is_loopback};

/// The policy for calendar servers. `protected` are the sockets of the daemon's own API
/// (the daemon's `daemon_protected_sockets`, the same list the web tools refuse), which a
/// calendar address must never point at. A port of `0` protects every port of that address.
#[must_use]
pub fn calendar_policy(protected: &[SocketAddr]) -> Arc<dyn UrlPolicy> {
    Arc::new(DefaultUrlPolicy {
        block_private: false,
        block_loopback: false,
        blocked_sockets: protected.to_vec(),
        ..DefaultUrlPolicy::default()
    })
}

/// Whether `url` names this machine: `localhost` (or a `*.localhost` name) or a
/// loopback address. Decided on the parsed host, never on its text, so
/// `127.attacker.com` is a name like any other.
pub(crate) fn names_loopback(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain(d)) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            d == "localhost" || d.ends_with(".localhost")
        }
        Some(Host::Ipv4(a)) => is_loopback(IpAddr::V4(a)),
        Some(Host::Ipv6(a)) => is_loopback(IpAddr::V6(a)),
        None => false,
    }
}

/// Resolves names for one calendar and drops every address the policy refuses, at
/// the moment of connecting (so a name that later points somewhere else is caught).
/// A plain-`http` calendar is only ever reached on loopback.
pub(crate) struct GuardedResolver {
    pub policy: Arc<dyn UrlPolicy>,
    pub port: u16,
    pub loopback_only: bool,
}

/// The resolved addresses of `host` the policy lets a connection use. `loopback_only` (a
/// plain-`http` calendar) keeps loopback addresses only.
fn allowed_addrs(
    policy: &dyn UrlPolicy,
    host: &str,
    loopback_only: bool,
    all: impl IntoIterator<Item = SocketAddr>,
) -> Vec<SocketAddr> {
    all.into_iter()
        .filter(|a| !loopback_only || canonical_ip(a.ip()).is_loopback())
        .filter(|a| policy.check_socket(host, *a).is_ok())
        .collect()
}

impl Resolve for GuardedResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let (policy, port, loopback_only) =
            (Arc::clone(&self.policy), self.port, self.loopback_only);
        Box::pin(async move {
            let host = name.as_str().to_owned();
            let all: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), port))
                .await
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?
                .collect();
            let allowed = allowed_addrs(policy.as_ref(), &host, loopback_only, all);
            if allowed.is_empty() {
                return Err(Box::new(std::io::Error::other(
                    "the address leads somewhere a calendar may not be",
                ))
                    as Box<dyn std::error::Error + Send + Sync>);
            }
            Ok(Box::new(allowed.into_iter()) as Addrs)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    fn sock(ip: &str, port: u16) -> SocketAddr {
        SocketAddr::new(ip.parse().unwrap(), port)
    }

    #[test]
    fn loopback_is_decided_on_the_parsed_host_not_on_its_text() {
        for yes in [
            "http://localhost:5232/",
            "http://LOCALHOST./",
            "http://dav.localhost/",
            "http://127.0.0.1:5232/",
            "http://127.9.9.9/",
            "http://[::1]:5232/",
            "http://[::ffff:127.0.0.1]/",
            // `url` folds the other spellings of 127.0.0.1 into the address.
            "http://2130706433/",
            "http://0x7f.1/",
        ] {
            assert!(names_loopback(&url(yes)), "{yes}");
        }
        for no in [
            "http://127.attacker.com/",
            "http://127.0.0.1.nip.io/",
            "http://localhost.attacker.com/",
            "http://notlocalhost/",
            "http://10.0.0.5/",
            "http://[::ffff:10.0.0.5]/",
            "http://169.254.169.254/",
        ] {
            assert!(!names_loopback(&url(no)), "{no}");
        }
    }

    #[test]
    fn metadata_link_local_and_unspecified_space_is_never_a_calendar() {
        let policy = calendar_policy(&[]);
        for bad in [
            "https://169.254.169.254/latest/meta-data/",
            "https://169.254.1.1/",
            "https://[::ffff:169.254.169.254]/",
            "https://[fe80::1]/",
            "https://[fd00:ec2::254]/",
            "https://100.100.100.200/",
            "https://0.0.0.0/",
            "https://[::]/",
            "https://224.0.0.1/",
            // Decimal and hex spellings are normalised by the URL parser first.
            "https://2852039166/",
            "https://0xa9fea9fe/",
        ] {
            assert!(policy.check_url(&url(bad)).is_err(), "{bad}");
        }
        // The owner's own machine and network, and any public name, stay reachable.
        for ok in [
            "https://caldav.example.com/dav/",
            "https://192.168.1.20:8443/dav/",
            "https://10.0.0.7/remote.php/dav/",
            "http://127.0.0.1:5232/",
            "http://localhost:5232/",
        ] {
            assert!(policy.check_url(&url(ok)).is_ok(), "{ok}");
        }
    }

    #[test]
    fn the_daemons_own_port_is_never_dialled_by_any_name() {
        let protected = [SocketAddr::from((Ipv4Addr::LOCALHOST, 47_821))];
        let policy = calendar_policy(&protected);
        for bad in [
            "http://127.0.0.1:47821/v1/chats",
            "http://localhost:47821/",
            "http://dav.localhost:47821/",
            "http://[::ffff:127.0.0.1]:47821/",
            "http://2130706433:47821/",
        ] {
            assert!(policy.check_url(&url(bad)).is_err(), "{bad}");
        }
        // Another port on the same address (a Radicale next door) is fine.
        assert!(policy.check_url(&url("http://127.0.0.1:5232/")).is_ok());
        // A port of 0 protects the whole address.
        let all = calendar_policy(&[SocketAddr::from((Ipv4Addr::LOCALHOST, 0))]);
        assert!(all.check_url(&url("http://127.0.0.1:5232/")).is_err());
    }

    #[test]
    fn a_name_that_resolves_into_forbidden_space_loses_those_addresses() {
        let protected = [SocketAddr::from((Ipv4Addr::LOCALHOST, 47_821))];
        let policy = calendar_policy(&protected);
        let resolved = [
            sock("169.254.169.254", 443),
            sock("::ffff:169.254.169.254", 443),
            sock("fe80::1", 443),
            sock("93.184.216.34", 443),
        ];
        // An attacker's name that points at metadata and at a real server keeps only the server.
        assert_eq!(
            allowed_addrs(policy.as_ref(), "evil.example", false, resolved),
            [sock("93.184.216.34", 443)]
        );
        // A name that resolves only to the cloud metadata service leaves nothing to connect to.
        assert!(
            allowed_addrs(
                policy.as_ref(),
                "evil.example",
                false,
                [sock("169.254.169.254", 443)]
            )
            .is_empty()
        );
        // A name that resolves to the daemon's own port.
        assert!(
            allowed_addrs(
                policy.as_ref(),
                "evil.example",
                false,
                [
                    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 47_821),
                    SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 5232),
                ]
            )
            .iter()
            .all(|a| a.port() != 47_821)
        );
        // Plain http is only ever for this machine: a LAN or public answer is dropped.
        assert_eq!(
            allowed_addrs(
                policy.as_ref(),
                "dav.localhost",
                true,
                [
                    sock("10.0.0.5", 5232),
                    sock("127.0.0.1", 5232),
                    sock("::1", 5232)
                ]
            ),
            [sock("127.0.0.1", 5232), sock("::1", 5232)]
        );
    }
}
